//! The protocol: JSON-RPC over MCP Streamable HTTP (initialize-based revisions, plain JSON
//! answers), `initialize`, `ping`, `tools/*`, `resources/*`, `prompts/list`, and the server
//! instructions.

use std::sync::Mutex;

use serde_json::{Value, json};

use crate::host::{ClientRef, Ctx, Host, McpError};
use crate::resources::{RESOURCES, read_resource};
use crate::tools::{CallRefused, ToolCaller, call_tool, tool_listings, tool_result};
use crate::transport::{McpRequest, McpResponse};

/// The protocol revisions this endpoint speaks, newest first (all initialize-based).
pub const PROTOCOL_VERSIONS: [&str; 3] = ["2025-11-25", "2025-06-18", "2025-03-26"];

/// Largest tool answer text in characters by default ([`Mcp::with_max_chars`] sets another; an
/// agent's context is small; the display limits already bound the results). A longer answer
/// keeps whole results only, so its JSON stays valid.
pub const MAX_TEXT_CHARS: usize = 60_000;

/// Most MCP sessions remembered; the oldest is forgotten first (its client initializes again).
const MAX_SESSIONS: usize = 64;

/// The `serverInfo` of `initialize`.
const SERVER_NAME: &str = "graphersal-dev";
const SERVER_TITLE: &str = "Graphersal dev server";

/// JSON-RPC error codes.
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// The client name of the endpoint's session jobs (the host's change feed records it).
const MCP_CLIENT: &str = "mcp";

/// The MCP endpoint: its options and its MCP sessions. One per server; [`Mcp::handle`] answers
/// every request to it.
pub struct Mcp {
    read_only: bool,
    /// Every saved query is also a tool of its own.
    query_tools: bool,
    token: Option<String>,
    /// The operator's text, appended to the instructions.
    operator_notes: Option<String>,
    /// The largest tool answer in characters.
    max_chars: usize,
    /// Session id -> `clientInfo` of its `initialize`, in the order they were created.
    sessions: Mutex<Vec<(String, Value)>>,
    /// The `clientInfo` of the last `initialize` (the playground's top bar names it).
    client: Mutex<Option<Value>>,
}

impl Mcp {
    /// An endpoint that offers only reading tools (`read_only`), requires `Authorization:
    /// Bearer <token>` when `token` is set, and lists every saved query as a tool of its own
    /// with `query_tools`.
    pub fn new(read_only: bool, token: Option<String>, query_tools: bool) -> Self {
        Self {
            read_only,
            query_tools,
            token,
            operator_notes: None,
            max_chars: MAX_TEXT_CHARS,
            sessions: Mutex::new(Vec::new()),
            client: Mutex::new(None),
        }
    }

    /// Adds the operator's notes to the `initialize` instructions (under the heading "Notes
    /// from the operator of this server about this graph:"). The AI Chat prompt does not carry
    /// them: it has its own text.
    pub fn with_operator_notes(mut self, notes: Option<String>) -> Self {
        self.operator_notes = notes;
        self
    }

    /// Sets the largest tool answer in characters (`None` keeps [`MAX_TEXT_CHARS`]).
    pub fn with_max_chars(mut self, max_chars: Option<usize>) -> Self {
        self.max_chars = max_chars.unwrap_or(MAX_TEXT_CHARS);
        self
    }

    /// The client sessions and their estimated bytes (ids and stored values as JSON text), for
    /// the host's memory report; `(0, 0)` while a request holds the list.
    pub fn memory_bytes(&self) -> (usize, u64) {
        let Ok(sessions) = self.sessions.try_lock() else {
            return (0, 0);
        };
        let bytes = sessions
            .iter()
            .map(|(id, value)| {
                (std::mem::size_of::<(String, Value)>() + id.capacity() + value.to_string().len())
                    as u64
            })
            .sum();
        (sessions.len(), bytes)
    }

    /// Whether the endpoint offers only reading tools.
    pub fn read_only(&self) -> bool {
        self.read_only
    }

    /// Whether every saved query is a tool of its own.
    pub fn query_tools(&self) -> bool {
        self.query_tools
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
                let removed = self.sessions.lock().is_ok_and(|mut sessions| {
                    let before = sessions.len();
                    sessions.retain(|(known, _)| known != id);
                    sessions.len() < before
                });
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

    /// One `POST /mcp`: a JSON-RPC request, notification or response.
    fn post(&self, host: &dyn Host, request: &mut McpRequest<'_>) -> McpResponse {
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
        if let Some(id) = &session_id {
            let known = self
                .sessions
                .lock()
                .is_ok_and(|sessions| sessions.iter().any(|(known, _)| known == id));
            if !known {
                return rpc_error(
                    404,
                    Value::Null,
                    INVALID_REQUEST,
                    "unknown or expired MCP session (the server restarted?): initialize again",
                );
            }
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
        let Some(id) = id else {
            // A notification (`notifications/initialized`, `notifications/cancelled`, ...).
            return empty(202);
        };
        let params = object.get("params").cloned().unwrap_or(Value::Null);
        match self.dispatch(host, method, &params) {
            Ok((result, new_session)) => {
                let answer =
                    rpc_answer(200, &json!({"jsonrpc": "2.0", "id": id, "result": result}));
                match new_session {
                    Some(session) => answer.with_header("Mcp-Session-Id", &session),
                    None => answer,
                }
            }
            Err((code, message)) => rpc_error(200, id, code, &message),
        }
    }

    fn dispatch(&self, host: &dyn Host, method: &str, params: &Value) -> RpcResult {
        let plain = |value: Value| Ok((value, None));
        let client = ClientRef::new(MCP_CLIENT, None);
        let caller = ToolCaller::new(client.clone(), self.read_only)
            .with_query_tools(self.query_tools)
            .with_max_chars(self.max_chars);
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
                match call_tool(host, &caller, name, &args) {
                    Ok(outcome) => plain(tool_result(outcome.text, outcome.is_error)),
                    Err(CallRefused::Internal(message)) => Err((-32603, message)),
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
            "resources/list" => plain(json!({ "resources": RESOURCES.iter().map(|r| json!({
                "uri": r.uri, "name": r.name, "title": r.title,
                "description": r.description, "mimeType": r.mime,
            })).collect::<Vec<_>>() })),
            "resources/templates/list" => plain(json!({ "resourceTemplates": [] })),
            "resources/read" => {
                let uri = params.get("uri").and_then(Value::as_str).unwrap_or("");
                let Some(resource) = RESOURCES.iter().find(|r| r.uri == uri) else {
                    return Err((-32002, format!("resource not found: {uri}")));
                };
                let context = Ctx {
                    host,
                    client: &client,
                };
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
        let session = format!("{:032x}", rand::random::<u128>());
        if let Ok(mut sessions) = self.sessions.lock() {
            if sessions.len() >= MAX_SESSIONS {
                sessions.remove(0);
            }
            sessions.push((session.clone(), client.clone()));
        }
        if let Ok(mut last) = self.client.lock() {
            *last = Some(client);
        }
        let mut instructions = INSTRUCTIONS.to_string();
        if self.read_only {
            instructions.push_str(READ_ONLY_NOTE);
        }
        if let Some(notes) = &self.operator_notes {
            instructions.push_str("\n\n");
            instructions.push_str(OPERATOR_HEADING);
            instructions.push_str("\n\n");
            instructions.push_str(notes);
        }
        Ok((
            json!({
                "protocolVersion": version,
                "capabilities": {
                    "tools": { "listChanged": false },
                    "resources": { "listChanged": false },
                },
                "serverInfo": {
                    "name": SERVER_NAME,
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
