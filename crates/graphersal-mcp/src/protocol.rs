//! The protocol: JSON-RPC over MCP Streamable HTTP (initialize-based revisions, plain JSON
//! answers), `initialize`, `ping`, `tools/*`, `resources/*`, `prompts/list`, and the server
//! instructions.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::{Value, json};

use crate::catalog::is_built_in;
use crate::config::{Instructions, McpConfig, SavedQueryTools};
use crate::host::{ClientRef, Ctx, Host, McpError, clear_busy, take_busy};
use crate::resources::{RESOURCES, read_resource};
use crate::sessions::{SessionKey, Sessions};
use crate::stats::{McpStats, Refusal};
use crate::tools::{CallRefused, ToolCaller, call_tool, tool_listings, tool_result};
use crate::transport::{McpRequest, McpResponse};

/// The protocol revisions this endpoint speaks, newest first (all initialize-based).
pub const PROTOCOL_VERSIONS: [&str; 3] = ["2025-11-25", "2025-06-18", "2025-03-26"];

/// Largest tool answer text in characters by default ([`Mcp::with_max_chars`] sets another; an
/// agent's context is small; the display limits already bound the results). A longer answer
/// keeps whole results only, so its JSON stays valid.
pub const MAX_TEXT_CHARS: usize = 60_000;

/// The `serverInfo` of `initialize`.
const SERVER_NAME: &str = "graphersal-dev";
const SERVER_TITLE: &str = "Graphersal dev server";

/// JSON-RPC error codes.
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;

/// The client name of the endpoint's session jobs (the host's change feed records it).
const MCP_CLIENT: &str = "mcp";

/// The MCP endpoint: its options and its MCP sessions. One per server; [`Mcp::handle`] answers
/// every request to it.
pub struct Mcp {
    read_only: bool,
    token: Option<String>,
    /// The operator's configuration (tools, descriptions, instructions, saved-query tools,
    /// resources, answer limit), with the shorthand flags folded in.
    config: Arc<McpConfig>,
    /// The MCP sessions: a bounded table (the default) or stateless signed ids
    /// ([`Mcp::with_session_key`]).
    sessions: Sessions,
    /// The usage statistics (memory only, since the endpoint was built).
    stats: Arc<McpStats>,
    /// The `clientInfo` of the last `initialize` (the playground's top bar names it).
    client: Mutex<Option<Value>>,
}

impl Mcp {
    /// An endpoint that offers only reading tools (`read_only`), requires `Authorization:
    /// Bearer <token>` when `token` is set, and lists every saved query as a tool of its own
    /// with `query_tools`.
    pub fn new(read_only: bool, token: Option<String>, query_tools: bool) -> Self {
        let mut config = McpConfig::default();
        if query_tools {
            config.saved_query_tools = Some(SavedQueryTools::all());
        }
        let sessions = Sessions::table();
        Self {
            read_only,
            token,
            config: Arc::new(config),
            stats: Arc::new(McpStats::new(sessions.clients())),
            sessions,
            client: Mutex::new(None),
        }
    }

    /// Makes the MCP session ids stateless: `initialize` answers an id signed with `key`
    /// (HMAC-SHA256 over a random nonce and a digest of the client's name), and a request's id
    /// is checked against the key instead of a table. So there is no limit on the number of
    /// clients and none evicts another, and every endpoint holding the same key (several
    /// replicas behind a load balancer) accepts the ids of the others. `DELETE` of a valid id
    /// answers `200` and the id stays valid (nothing is stored to forget). The client names
    /// are kept for the statistics, at most [`crate::MAX_CLIENT_NAMES`].
    pub fn with_session_key(mut self, key: SessionKey) -> Self {
        self.sessions = Sessions::signed(&key);
        self.stats = Arc::new(McpStats::new(self.sessions.clients()));
        self
    }

    /// The usage statistics of the endpoint since it was built: in process memory only, never
    /// written anywhere, lost on a restart ([`McpStats::to_json`] for `GET /api/mcp/stats`; the
    /// host adds its own refusals with [`McpStats::record_refusal`]).
    pub fn stats(&self) -> &McpStats {
        &self.stats
    }

    /// Whether the session ids are stateless ([`Mcp::with_session_key`]).
    pub fn stateless_sessions(&self) -> bool {
        self.sessions.is_signed()
    }

    /// Serves the operator's configuration (`--mcp-config`, with the shorthand flags folded in
    /// by [`McpConfig::with_flags`]). It replaces the whole configuration, including what
    /// `query_tools`, [`Mcp::with_operator_notes`] and [`Mcp::with_max_chars`] set before.
    pub fn with_config(mut self, config: McpConfig) -> Self {
        self.config = Arc::new(config);
        self
    }

    /// Adds the operator's notes to the `initialize` instructions (under the heading "Notes
    /// from the operator of this server about this graph:"). The AI Chat prompt does not carry
    /// them: it has its own text.
    pub fn with_operator_notes(mut self, notes: Option<String>) -> Self {
        Arc::make_mut(&mut self.config).instructions = match notes {
            Some(notes) => Instructions::Append(notes),
            None => Instructions::Builtin,
        };
        self
    }

    /// Sets the largest tool answer in characters (`None` keeps [`MAX_TEXT_CHARS`]).
    pub fn with_max_chars(mut self, max_chars: Option<usize>) -> Self {
        Arc::make_mut(&mut self.config).answer_limit_chars = max_chars;
        self
    }

    /// The largest tool answer in characters.
    fn max_chars(&self) -> usize {
        self.config.answer_limit_chars.unwrap_or(MAX_TEXT_CHARS)
    }

    /// The client sessions (stateless ids: the client names kept) and their estimated bytes
    /// (ids and stored values as JSON text), for the host's memory report; `(0, 0)` while a
    /// request holds the list.
    pub fn memory_bytes(&self) -> (usize, u64) {
        let (count, bytes) = self.sessions.memory_bytes();
        (count, bytes + self.stats.bytes())
    }

    /// Whether the endpoint offers only reading tools.
    pub fn read_only(&self) -> bool {
        self.read_only
    }

    /// Whether saved queries are tools of their own (`--mcp-query-tools` or the
    /// configuration's `saved_query_tools`).
    pub fn query_tools(&self) -> bool {
        self.config.has_saved_query_tools()
    }

    /// The configuration served.
    pub fn config(&self) -> &McpConfig {
        &self.config
    }

    /// The instructions `initialize` answers.
    pub fn instructions_text(&self) -> String {
        let mut instructions = match &self.config.instructions {
            Instructions::Replace(text) => return text.clone(),
            Instructions::Builtin | Instructions::Append(_) => INSTRUCTIONS.to_string(),
        };
        if self.read_only {
            instructions.push_str(READ_ONLY_NOTE);
        }
        if let Instructions::Append(notes) = &self.config.instructions {
            instructions.push_str("\n\n");
            instructions.push_str(OPERATOR_HEADING);
            instructions.push_str("\n\n");
            instructions.push_str(notes);
        }
        instructions
    }

    /// The caller of the endpoint's tools.
    fn caller(&self) -> ToolCaller {
        self.caller_for(ClientRef::new(MCP_CLIENT, None))
    }

    /// The caller of the endpoint's tools for `client` (the request's MCP session as its key).
    fn caller_for(&self, client: ClientRef) -> ToolCaller {
        ToolCaller::new(client, self.read_only)
            .with_config(self.config.clone())
            .with_max_chars(self.max_chars())
            .with_stats(self.stats.clone())
    }

    /// The `tools/list` entries an agent gets now (saved-query tools read through `host`).
    pub fn tool_listings(&self, host: &dyn Host) -> Result<Vec<Value>, McpError> {
        tool_listings(host, &self.caller())
    }

    /// What the endpoint's prompt costs an agent's context: the instructions and every tool's
    /// `tools/list` entry in characters (`graphersal mcp-cost`, the budget tests).
    pub fn prompt_cost(&self, host: &dyn Host) -> Result<PromptCost, McpError> {
        let tools = self
            .tool_listings(host)?
            .iter()
            .map(|tool| {
                (
                    tool["name"].as_str().unwrap_or_default().to_string(),
                    tool.to_string().chars().count(),
                )
            })
            .collect();
        Ok(PromptCost {
            instructions_chars: self.instructions_text().chars().count(),
            tools,
        })
    }

    /// Whether requests need `Authorization: Bearer <token>`.
    pub fn has_token(&self) -> bool {
        self.token.is_some()
    }

    /// `{endpoint, readOnly, client: {name, version} | null}`: what a status page shows of the
    /// endpoint (`client` = the last `initialize`'s client).
    pub fn status(&self) -> Value {
        json!({
            "endpoint": "/mcp",
            "readOnly": self.read_only,
            "client": self.client.lock().ok().and_then(|c| c.clone()),
        })
    }

    /// Answers one HTTP request to the endpoint (after the host's own checks, e.g. `Host` and
    /// `Origin`): `POST` carries one JSON-RPC message, `DELETE` ends an MCP session, every other
    /// method is `405`. Session jobs run through `host` for the client `mcp`.
    pub fn handle(&self, host: &dyn Host, mut request: McpRequest<'_>) -> McpResponse {
        if let Some(token) = &self.token {
            let given = request
                .header_value("Authorization")
                .and_then(|v| v.trim().strip_prefix("Bearer "))
                .map(str::trim);
            if !given.is_some_and(|given| same_secret(given, token)) {
                self.stats.record_refusal(Refusal::Unauthorized);
                let answer = rpc_error(
                    401,
                    Value::Null,
                    INVALID_REQUEST,
                    "unauthorized: send the header \"Authorization: Bearer <token>\" with the \
                     server's --mcp-token",
                );
                return answer.with_header("WWW-Authenticate", "Bearer");
            }
        }
        match request.method() {
            "POST" => self.post(host, &mut request),
            "DELETE" => {
                let Some(id) = request.header_value("Mcp-Session-Id") else {
                    return rpc_error(
                        400,
                        Value::Null,
                        INVALID_REQUEST,
                        "DELETE /mcp needs the Mcp-Session-Id header",
                    );
                };
                let removed = self.sessions.forget(id);
                empty(if removed { 200 } else { 404 })
            }
            // No server-initiated stream (GET) on this endpoint.
            _ => rpc_error(
                405,
                Value::Null,
                INVALID_REQUEST,
                "the MCP endpoint takes POST (JSON-RPC); it offers no SSE stream",
            )
            .with_header("Allow", "POST, DELETE"),
        }
    }

    /// One `POST /mcp`: a JSON-RPC request, notification or response; a `4xx` answer before
    /// the message is dispatched counts as a refusal (unknown session or invalid request).
    fn post(&self, host: &dyn Host, request: &mut McpRequest<'_>) -> McpResponse {
        let answer = self.post_message(host, request);
        match answer.status() {
            404 => self.stats.record_refusal(Refusal::UnknownSession),
            400..=499 => self.stats.record_refusal(Refusal::InvalidRequest),
            _ => {}
        }
        answer
    }

    fn post_message(&self, host: &dyn Host, request: &mut McpRequest<'_>) -> McpResponse {
        if let Some(accept) = request.header_value("Accept") {
            let accept = accept.to_ascii_lowercase();
            if !accept.contains("application/json") && !accept.contains("*/*") {
                return rpc_error(
                    406,
                    Value::Null,
                    INVALID_REQUEST,
                    "this endpoint answers application/json: send Accept: application/json, \
                     text/event-stream",
                );
            }
        }
        if let Some(version) = request.header_value("MCP-Protocol-Version")
            && !PROTOCOL_VERSIONS.contains(&version.trim())
        {
            return rpc_error(
                400,
                Value::Null,
                INVALID_REQUEST,
                &format!(
                    "unsupported MCP-Protocol-Version {version:?}: this server speaks {} \
                     (initialize first)",
                    PROTOCOL_VERSIONS.join(", ")
                ),
            );
        }
        let session_id = request.header_value("Mcp-Session-Id").map(str::to_string);
        if let Some(id) = &session_id
            && !self.sessions.knows(id)
        {
            return rpc_error(
                404,
                Value::Null,
                INVALID_REQUEST,
                "unknown or expired MCP session (the server restarted?): initialize again",
            );
        }
        let body = match request.text_body() {
            Ok(body) => body,
            Err((status, message)) => {
                return rpc_error(status, Value::Null, INVALID_REQUEST, &message);
            }
        };
        let message: Value = match serde_json::from_str(&body) {
            Ok(message) => message,
            Err(err) => {
                return rpc_error(
                    400,
                    Value::Null,
                    PARSE_ERROR,
                    &format!("malformed JSON: {err}"),
                );
            }
        };
        let Some(object) = message.as_object() else {
            return rpc_error(
                400,
                Value::Null,
                INVALID_REQUEST,
                "expected one JSON-RPC message object (batches are not supported)",
            );
        };
        let id = object.get("id").cloned();
        let Some(method) = object.get("method").and_then(Value::as_str) else {
            // A response to a server request (this server sends none): accepted.
            return if object.contains_key("result") || object.contains_key("error") {
                empty(202)
            } else {
                rpc_error(
                    400,
                    id.unwrap_or(Value::Null),
                    INVALID_REQUEST,
                    "a JSON-RPC message needs \"method\"",
                )
            };
        };
        let client_digest = session_id
            .as_deref()
            .and_then(|id| self.sessions.client_of(id));
        self.stats.record_request(
            id.is_some().then_some(method),
            session_id.as_deref(),
            client_digest,
            method == "tools/call" && id.is_some(),
        );
        let Some(id) = id else {
            // A notification (`notifications/initialized`, `notifications/cancelled`, ...).
            return empty(202);
        };
        let params = object.get("params").cloned().unwrap_or(Value::Null);
        let client = match &session_id {
            Some(id) => ClientRef::new(MCP_CLIENT, None).with_key(id),
            None => ClientRef::new(MCP_CLIENT, None),
        };
        clear_busy();
        let dispatched = self.dispatch(host, &client, method, &params);
        // The host could not take a job now (queue full, waited too long, too many jobs of this
        // client, stopping): the whole request is `503`, so the client retries it.
        if let Some(message) = take_busy() {
            self.stats.record_busy();
            return rpc_error(503, id, INTERNAL_ERROR, &message).with_header("Retry-After", "1");
        }
        match dispatched {
            Ok((result, new_session)) => {
                let answer =
                    rpc_answer(200, &json!({"jsonrpc": "2.0", "id": id, "result": result}));
                match new_session {
                    Some(session) => {
                        self.stats.record_initialize(&session);
                        answer.with_header("Mcp-Session-Id", &session)
                    }
                    None => answer,
                }
            }
            Err((code, message)) => rpc_error(200, id, code, &message),
        }
    }

    fn dispatch(
        &self,
        host: &dyn Host,
        client: &ClientRef,
        method: &str,
        params: &Value,
    ) -> RpcResult {
        let plain = |value: Value| Ok((value, None));
        let caller = self.caller_for(client.clone());
        match method {
            "initialize" => self.initialize(params),
            "ping" => plain(json!({})),
            "tools/list" => {
                let listed = tool_listings(host, &caller)
                    .map_err(|err: McpError| (-32603, err.to_string()))?;
                plain(json!({ "tools": listed }))
            }
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                let started = Instant::now();
                let called = call_tool(host, &caller, name, &args);
                let elapsed_us = started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
                // A built-in tool by its name; a saved query's own tool ran when the call was
                // not refused; any other name is an unknown tool.
                let built_in = is_built_in(name);
                let (tool, saved_query) = match (&called, built_in) {
                    (_, true) if name == "run_saved_query" => (
                        Some(name),
                        args.get("name").and_then(Value::as_str).map(str::trim),
                    ),
                    (_, true) => (Some(name), None),
                    (Ok(_), false) => (Some(name), Some(name)),
                    (Err(_), false) => (None, None),
                };
                let (is_error, chars) = match &called {
                    Ok(outcome) => (outcome.is_error, outcome.text.chars().count() as u64),
                    Err(_) => (true, 0),
                };
                self.stats
                    .record_tool(tool, saved_query, elapsed_us, is_error, chars);
                match called {
                    Ok(outcome) => plain(tool_result(outcome.text, outcome.is_error)),
                    Err(CallRefused::Internal(message)) => Err((INTERNAL_ERROR, message)),
                    Err(CallRefused::Params(message)) => {
                        let message = if self.read_only && message.contains("(read-only") {
                            message.replace(
                                "(read-only: only reading tools)",
                                "(the server runs with --mcp-read-only: only reading tools)",
                            )
                        } else {
                            message
                        };
                        Err((INVALID_PARAMS, message))
                    }
                }
            }
            "resources/list" => plain(json!({ "resources": RESOURCES.iter()
                .filter(|r| self.config.lists_resource(r.name)).map(|r| json!({
                "uri": r.uri, "name": r.name, "title": r.title,
                "description": r.description, "mimeType": r.mime,
            })).collect::<Vec<_>>() })),
            "resources/templates/list" => plain(json!({ "resourceTemplates": [] })),
            "resources/read" => {
                let uri = params.get("uri").and_then(Value::as_str).unwrap_or("");
                let Some(resource) = RESOURCES.iter().find(|r| r.uri == uri) else {
                    return Err((-32002, format!("resource not found: {uri}")));
                };
                let context = Ctx { host, client };
                let text =
                    read_resource(&context, uri, self.read_only).map_err(|err| (-32603, err))?;
                plain(json!({ "contents": [{
                    "uri": uri, "mimeType": resource.mime, "text": text,
                }] }))
            }
            "prompts/list" => plain(json!({ "prompts": [] })),
            _ => Err((METHOD_NOT_FOUND, format!("method not found: {method}"))),
        }
    }

    /// `initialize`: negotiates the protocol version (the client's when this server speaks it,
    /// else the newest), assigns a session id and remembers the client's name for the
    /// [`Mcp::status`].
    fn initialize(&self, params: &Value) -> RpcResult {
        let requested = params
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or("");
        let version = PROTOCOL_VERSIONS
            .iter()
            .find(|v| **v == requested)
            .copied()
            .unwrap_or(PROTOCOL_VERSIONS[0]);
        let client = params.get("clientInfo").cloned().unwrap_or(Value::Null);
        let client = json!({
            "name": client.get("name").and_then(Value::as_str).unwrap_or("unknown client"),
            "version": client.get("version").and_then(Value::as_str).unwrap_or(""),
        });
        let session = self.sessions.create(&client);
        if let Ok(mut last) = self.client.lock() {
            *last = Some(client);
        }
        let instructions = self.instructions_text();
        Ok((
            json!({
                "protocolVersion": version,
                "capabilities": {
                    "tools": { "listChanged": false },
                    "resources": { "listChanged": false },
                },
                "serverInfo": {
                    "name": self.config.server_name.as_deref().unwrap_or(SERVER_NAME),
                    "title": SERVER_TITLE,
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "instructions": instructions,
            }),
            Some(session),
        ))
    }
}

/// A JSON-RPC error answer with an HTTP status (`id` null when the request had none).
fn rpc_error(status: u16, id: Value, code: i64, message: &str) -> McpResponse {
    rpc_answer(
        status,
        &json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
    )
}

fn rpc_answer(status: u16, body: &Value) -> McpResponse {
    McpResponse::new(status, body.to_string().into_bytes())
        .with_header("Content-Type", "application/json")
        .with_header("Cache-Control", "no-store")
}

fn empty(status: u16) -> McpResponse {
    McpResponse::new(status, Vec::new()).with_header("Cache-Control", "no-store")
}

/// Compares a secret without stopping at the first difference.
pub(crate) fn same_secret(given: &str, expected: &str) -> bool {
    given.len() == expected.len()
        && given
            .bytes()
            .zip(expected.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

type RpcResult = Result<(Value, Option<String>), (i64, String)>;

/// What an endpoint's prompt costs ([`Mcp::prompt_cost`]): characters of the instructions and
/// of every tool's `tools/list` entry (its compact JSON). Tokens are estimated as characters / 4.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct PromptCost {
    /// Characters of the `initialize` instructions.
    pub instructions_chars: usize,
    /// Every listed tool's name and the characters of its JSON entry, in listing order.
    pub tools: Vec<(String, usize)>,
}

impl PromptCost {
    /// Characters of every tool entry together.
    pub fn tools_chars(&self) -> usize {
        self.tools.iter().map(|(_, chars)| chars).sum()
    }

    /// Instructions and tools together.
    pub fn total_chars(&self) -> usize {
        self.instructions_chars + self.tools_chars()
    }

    /// The rough token estimate of `chars` characters (characters / 4, rounded up).
    pub fn tokens(chars: usize) -> usize {
        chars.div_ceil(4)
    }
}

pub(crate) const INSTRUCTIONS: &str = "Graphersal dev server: an in-memory property graph (Gremlin/TinkerPop \
model: vertices and edges with labels and properties) that a user works with LIVE in a browser \
playground at the same time; the browser reloads when you change the graph. Prefer GQL, the ISO graph \
query language (it also accepts the Cypher spellings, such as CREATE and UNWIND), for reading and \
writing data: the `gql` tool takes the query text as it is (MATCH ... WHERE ... RETURN, rows as \
JSON objects, INSERT / CREATE, SET, REMOVE, DELETE and MERGE writes; experimental, see its \
description). Use `query` for what GQL does not cover: Gremlin (Apache TinkerPop) in Rhai syntax, \
and scripts with several statements (see its description). Use `get_schema`/`statistics`/`graph_info` \
to learn the data first, and the schema tools to read or evolve the schema. Before writing a \
new query, check `list_saved_queries`: saved queries are named, parameterized queries the user \
stored in the graph, each written in GQL (`language: \"gql\"`) or in Gremlin (Rhai); run one \
with `run_saved_query` {name, params} instead of writing it again \
(a GQL query calls one written in GQL as `CALL folder.name(args) YIELD col`). \
Saved queries marked `writes: true` change data (their `effects` say what); run them only when \
the user asked for that change. Large text that queries rarely read (CVs, descriptions) can be kept \
compressed in memory with a compression rule: `list_compressions`, `estimate_compression` \
(measures the candidate rules on the current data first, changes nothing), \
`define_compression`, `drop_compression`, `recompress` (reads stay plain); `memory_report` \
shows where the memory goes per label and property, `workload_statistics` which queries ran \
and how the properties were used. A query that filters a label on a \
property (`g.v().has_label(\"person\").has(\"age\", P.gt(30))`) and is slow on a large \
graph can be served by a property index: `list_indexes`, `create_index` {label, key, \
element?, type?, transform?}, `drop_index`; results never change, `.profile()` names the index \
it reads. For a case-insensitive search create it with transform \"to_lower\" and query \
`has(\"name\", __.to_lower().is(\"alice\"))` (the constant compared as given, so write it in \
lower case). \
Resources: graphersal://dsl-reference (every step and function), \
graphersal://examples (runnable examples), graphersal://saved-queries, graphersal://schema, \
graphersal://statistics. To create test data, use \
`Fake(seed)` inside a query (reproducible English names, e-mails, addresses, companies and \
random numbers; section \"Test data generator\" of graphersal://dsl-reference, group \
\"Generated test data (Fake)\" of graphersal://examples). Every query is one transaction: a \
failing query changes nothing. Errors carry a `Help:` line that names the fix. Answers are \
compact JSON. `query`, `gql` and `run_saved_query` page their results: at most `max_rows` \
results (default 100) after the first `offset` ones (default 0), `max_items` items of every \
nested list (default 100), and only whole results within the server's answer limit \
(`graph_info`: `answerLimitChars`); a trailing note says what was shown and names the next \
page's `offset`. Bound large answers in the query itself (LIMIT, `values(..)`) or page them. \
Persistence \
(checkpoints, history, views of past states) is the user's, in the browser: when the user opens \
a read-only view of a past state, `graph_info` says so and writes are refused until they go \
back to the live graph.";

/// The heading above the `--mcp-instructions` text.
const OPERATOR_HEADING: &str = "Notes from the operator of this server about this graph:";

const READ_ONLY_NOTE: &str = " This server is READ-ONLY for MCP: only reading tools are \
offered and a query that would write is refused.";

/// The MCP server instructions (what `initialize` answers without the read-only note and the
/// operator's notes), for an agent's system prompt (the AI Chat).
pub fn instructions() -> &'static str {
    INSTRUCTIONS
}

/// The DSL reference (the resource `graphersal://dsl-reference`), built once from the engine;
/// the first call runs a job for `client` through `host`.
pub fn dsl_reference_text(host: &dyn Host, client: &ClientRef) -> Result<String, McpError> {
    read_resource(&Ctx { host, client }, "graphersal://dsl-reference", false)
        .map_err(McpError::Failed)
}
