//! `graphersal --server --mcp`: a Model Context Protocol endpoint (`POST /mcp`) on the dev
//! server, so an AI agent (Claude Code: `claude mcp add --transport http graphersal
//! http://127.0.0.1:<port>/mcp`) works with the SAME graph and session as the browser. **DEV
//! mode, unstable, local only.**
//!
//! Transport: MCP Streamable HTTP of the initialize-based protocol revisions (`2025-03-26` to
//! `2025-11-25`, [`PROTOCOL_VERSIONS`]), with plain JSON answers (no SSE stream): every
//! JSON-RPC request is one `POST /mcp` answered with one `application/json` object; a
//! notification or a response is answered `202 Accepted` without a body; `GET /mcp` (a stream
//! the server would push on) is `405`; `DELETE /mcp` ends the MCP session. `initialize` assigns
//! an `Mcp-Session-Id` (random, 128 bits); a request naming an unknown one is `404` (the client
//! then initializes again, e.g. after a restart of the server); a request without one is served
//! too. A request whose `MCP-Protocol-Version` header names a revision this server does not
//! speak (a stateless, newer client) is `400` with a plain JSON-RPC error, which tells a dual-era
//! client to fall back to `initialize`. The `Host`/`Origin` checks of the HTTP API apply (a
//! foreign `Origin` is `403`), the sockets bind the loopback only, and `--mcp-token <secret>`
//! requires `Authorization: Bearer <secret>` (`401` otherwise).
//!
//! Hand-written JSON-RPC over `tiny_http` rather than the `rmcp` SDK: `rmcp` needs the tokio
//! runtime and an async HTTP stack (axum/hyper), dozens of crates for a dev endpoint whose whole
//! protocol surface is seven methods; the server is synchronous by design (one session thread).
//!
//! Every tool is a thin wrapper of one session operation and queues on the session thread like
//! the browser's calls: the server's limits (`--timeout`, `--memory-limit`, ...) apply, the UI's
//! Stop cancels a running MCP query too, and the change feed records MCP changes (client `mcp`),
//! so the browser reloads. `--mcp-read-only` lists only the reading tools and runs `query` under
//! [`read_only_authorizer`] (creating, updating, deleting data, the schema or a catalog
//! definition is denied).
//!
//! Saved queries (`book/src/users_guide/saved_queries.md`): `list_saved_queries` and
//! `run_saved_query` are always offered, with the resource `graphersal://saved-queries`; the
//! instructions and the `query` tool tell the agent to look for a saved query before writing a
//! new one, and to run one marked `writes: true` (it changes data, its `effects` say what) only
//! when the user asked for that change. Read-only mode lists only the queries that do not write
//! and refuses a call of one that does. `--mcp-query-tools` also lists every saved query as a
//! tool of its own (name, description, `inputSchema` from the parameter schemas, `readOnlyHint` /
//! `destructiveHint` from its effects); `tools/list` reflects the catalog at the time of the
//! call. The endpoint has no SSE
//! stream, so it cannot send `notifications/tools/list_changed` (`listChanged: false`): a
//! client re-lists the tools to see a changed catalog.
//!
//! The endpoint works with the in-memory GRAPH only: it has no persistence tools (no
//! checkpoint, mark, view, rollback, attic, backup or compaction; fewer tools keep an agent's
//! context small). Those stay the user's, in the browser's Store menu and `/api/store*`. When the
//! user opens a read-only view of a past state there, the view is server-wide: `graph_info`
//! carries a `note` saying so, and every tool error carries it too (a write is refused).
//!
//! Compression rules (`book/src/users_guide/compressed_properties.md`): `list_compressions` and
//! `estimate_compression` (both reading, also in read-only mode: an estimate changes nothing),
//! `define_compression`, `drop_compression` and `recompress` (refused in read-only mode) wrap
//! the session's `compression` operations; a define or drop is a commit recorded in the change
//! feed with the client `mcp`.
//!
//! Property indexes (`book/src/users_guide/property_indexes.md`): `list_indexes` (also in
//! read-only mode), `create_index` and `drop_index` (refused in read-only mode, where the
//! `query` tool's authorizer denies `g.create_index`/`g.drop_index` too) wrap the session's
//! `indexes` operations; a create or drop is a commit recorded in the change feed.

use std::sync::atomic::Ordering;
use std::sync::{Mutex, OnceLock};

use graphersal::exec::CancelToken;
use graphersal_session::{RunOptions, Session, playground_authorizer, read_only_authorizer};
use serde_json::{Value, json};
use tiny_http::{Method, Request, Response};

use super::store::current_view;
use super::{
    Answer, Client, Context, HttpError, SERVER_ID, as_client, header, text_body, with_header,
};

/// The protocol revisions this endpoint speaks, newest first (all initialize-based).
pub(super) const PROTOCOL_VERSIONS: [&str; 3] = ["2025-11-25", "2025-06-18", "2025-03-26"];

/// Largest tool answer text in characters; longer text is cut with a note (an agent's context
/// is small; the display limits already bound the results).
const MAX_TEXT_CHARS: usize = 60_000;

/// Default display limits of the `query` tool (the UI's defaults).
const DEFAULT_MAX_ROWS: usize = 100;
const DEFAULT_MAX_ITEMS: usize = 100;

/// Most MCP sessions remembered; the oldest is forgotten first (its client initializes again).
const MAX_SESSIONS: usize = 64;

/// JSON-RPC error codes.
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// The MCP endpoint of a server started with `--mcp`.
pub(super) struct Mcp {
    read_only: bool,
    /// `--mcp-query-tools`: every saved query is also a tool of its own.
    query_tools: bool,
    token: Option<String>,
    /// Session id -> `clientInfo` of its `initialize`, in the order they were created.
    sessions: Mutex<Vec<(String, Value)>>,
    /// The `clientInfo` of the last `initialize` (the top bar's badge names it).
    client: Mutex<Option<Value>>,
}

impl Mcp {
    pub(super) fn new(read_only: bool, token: Option<String>, query_tools: bool) -> Self {
        Self {
            read_only,
            query_tools,
            token,
            sessions: Mutex::new(Vec::new()),
            client: Mutex::new(None),
        }
    }

    /// The stderr lines that say where the endpoint is and how to connect Claude Code.
    pub(super) fn banner(&self, port: u16) -> String {
        let url = format!("http://127.0.0.1:{port}/mcp");
        let header = match &self.token {
            Some(_) => " --header \"Authorization: Bearer <your --mcp-token>\"",
            None => "",
        };
        format!(
            "MCP endpoint{}{}: {url}\n  connect Claude Code: claude mcp add --transport http \
             graphersal {url}{header}",
            if self.read_only { " (read-only)" } else { "" },
            if self.query_tools {
                " (saved queries as tools)"
            } else {
                ""
            }
        )
    }
}

/// `{readOnly, endpoint, client: {name, version} | null}` for `GET /api/info` and
/// `/api/changes`, or null without `--mcp`.
pub(super) fn status(context: &Context) -> Value {
    match &context.mcp {
        None => Value::Null,
        Some(mcp) => json!({
            "endpoint": "/mcp",
            "readOnly": mcp.read_only,
            "client": mcp.client.lock().ok().and_then(|c| c.clone()),
        }),
    }
}

/// A JSON-RPC error answer with an HTTP status (`id` null when the request had none).
fn rpc_error(status: u16, id: Value, code: i64, message: &str) -> Answer {
    rpc_answer(
        status,
        &json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
    )
}

fn rpc_answer(status: u16, body: &Value) -> Answer {
    let response = Response::from_data(body.to_string().into_bytes()).with_status_code(status);
    let response = with_header(response, "Content-Type", "application/json");
    with_header(response, "Cache-Control", "no-store")
}

fn empty(status: u16) -> Answer {
    with_header(
        Response::from_data(Vec::new()).with_status_code(status),
        "Cache-Control",
        "no-store",
    )
}

/// Routes `/mcp` (after the Host/Origin checks of the HTTP API).
pub(super) fn route(
    context: &Context,
    request: &mut Request,
    method: &Method,
) -> Result<Answer, HttpError> {
    let Some(mcp) = &context.mcp else {
        return Err(HttpError(
            404,
            "the MCP endpoint is off: start the server with --mcp".into(),
        ));
    };
    if let Some(token) = &mcp.token {
        let given = header(request, "Authorization")
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
            return Ok(with_header(answer, "WWW-Authenticate", "Bearer"));
        }
    }
    match method {
        Method::Post => Ok(post(context, mcp, request)),
        Method::Delete => {
            let Some(id) = header(request, "Mcp-Session-Id") else {
                return Ok(rpc_error(
                    400,
                    Value::Null,
                    INVALID_REQUEST,
                    "DELETE /mcp needs the Mcp-Session-Id header",
                ));
            };
            let removed = mcp.sessions.lock().is_ok_and(|mut sessions| {
                let before = sessions.len();
                sessions.retain(|(known, _)| known != id);
                sessions.len() < before
            });
            Ok(empty(if removed { 200 } else { 404 }))
        }
        // No server-initiated stream (GET) on this endpoint.
        _ => Ok(with_header(
            rpc_error(
                405,
                Value::Null,
                INVALID_REQUEST,
                "the MCP endpoint takes POST (JSON-RPC); it offers no SSE stream",
            ),
            "Allow",
            "POST, DELETE",
        )),
    }
}

/// Compares a secret without stopping at the first difference.
fn same_secret(given: &str, expected: &str) -> bool {
    given.len() == expected.len()
        && given
            .bytes()
            .zip(expected.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

/// One `POST /mcp`: a JSON-RPC request, notification or response.
fn post(context: &Context, mcp: &Mcp, request: &mut Request) -> Answer {
    if let Some(accept) = header(request, "Accept") {
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
    if let Some(version) = header(request, "MCP-Protocol-Version")
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
    let session_id = header(request, "Mcp-Session-Id").map(str::to_string);
    if let Some(id) = &session_id {
        let known = mcp
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
    let body = match text_body(request) {
        Ok(body) => body,
        Err(HttpError(status, message)) => {
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
    match dispatch(context, mcp, method, &params) {
        Ok((result, new_session)) => {
            let answer = rpc_answer(200, &json!({"jsonrpc": "2.0", "id": id, "result": result}));
            match new_session {
                Some(session) => with_header(answer, "Mcp-Session-Id", &session),
                None => answer,
            }
        }
        Err((code, message)) => rpc_error(200, id, code, &message),
    }
}

type RpcResult = Result<(Value, Option<String>), (i64, String)>;

fn dispatch(context: &Context, mcp: &Mcp, method: &str, params: &Value) -> RpcResult {
    let plain = |value: Value| Ok((value, None));
    match method {
        "initialize" => initialize(mcp, params),
        "ping" => plain(json!({})),
        "tools/list" => {
            let listed = tool_listings(context, mcp.read_only, mcp.query_tools)
                .map_err(|err| (-32603, err))?;
            plain(json!({ "tools": listed }))
        }
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            let caller = ToolCaller {
                read_only: mcp.read_only,
                query_tools: mcp.query_tools,
                client: Client::with_detail("mcp", None),
                cancel: None,
            };
            match call_tool(context, &caller, name, &args) {
                Ok(outcome) => plain(tool_result(outcome.text, outcome.is_error)),
                Err(CallRefused::Internal(message)) => Err((-32603, message)),
                Err(CallRefused::Params(message)) => {
                    let message = if mcp.read_only && message.contains("(read-only") {
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
            let text = read_resource(context, uri, mcp.read_only).map_err(|err| (-32603, err))?;
            plain(json!({ "contents": [{
                "uri": uri, "mimeType": resource.mime, "text": text,
            }] }))
        }
        "prompts/list" => plain(json!({ "prompts": [] })),
        _ => Err((METHOD_NOT_FOUND, format!("method not found: {method}"))),
    }
}

/// `initialize`: negotiates the protocol version (the client's when this server speaks it, else
/// the newest), assigns a session id and remembers the client's name for the top bar's badge.
fn initialize(mcp: &Mcp, params: &Value) -> RpcResult {
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
    if let Ok(mut sessions) = mcp.sessions.lock() {
        if sessions.len() >= MAX_SESSIONS {
            sessions.remove(0);
        }
        sessions.push((session.clone(), client.clone()));
    }
    if let Ok(mut last) = mcp.client.lock() {
        *last = Some(client);
    }
    let mut instructions = INSTRUCTIONS.to_string();
    if mcp.read_only {
        instructions.push_str(READ_ONLY_NOTE);
    }
    Ok((
        json!({
            "protocolVersion": version,
            "capabilities": {
                "tools": { "listChanged": false },
                "resources": { "listChanged": false },
            },
            "serverInfo": {
                "name": SERVER_ID,
                "title": "Graphersal dev server",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "instructions": instructions,
        }),
        Some(session),
    ))
}

const INSTRUCTIONS: &str = "Graphersal dev server: an in-memory property graph (Gremlin/TinkerPop \
model: vertices and edges with labels and properties) that a user works with LIVE in a browser \
playground at the same time; the browser reloads when you change the graph. Use `query` to run \
traversals (Gremlin in Rhai syntax, see its description), `gql` for GQL read queries (MATCH \
... WHERE ... RETURN, Cypher-like patterns, rows as JSON objects; see its description), `get_schema`/`statistics`/`graph_info` \
to learn the data first, and the schema tools to read or evolve the schema. Before writing a \
new query, check `list_saved_queries`: saved queries are named, parameterized queries the user \
stored in the graph; run one with `run_saved_query` {name, params} instead of writing it again. \
Saved queries marked `writes: true` change data (their `effects` say what); run them only when \
the user asked for that change. Large text that queries rarely read (CVs, descriptions) can be kept \
compressed in memory with a compression rule: `list_compressions`, `estimate_compression` \
(measures the candidate rules on the current data first, changes nothing), \
`define_compression`, `drop_compression`, `recompress` (reads stay plain). A query that filters a label on a \
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
failing query changes nothing. Errors carry a `Help:` line that names the fix. Persistence \
(checkpoints, history, views of past states) is the user's, in the browser: when the user opens \
a read-only view of a past state, `graph_info` says so and writes are refused until they go \
back to the live graph.";

const READ_ONLY_NOTE: &str = " This server is READ-ONLY for MCP: only reading tools are \
offered and a query that would write is refused.";

/// The MCP server instructions (what `initialize` answers), for the AI Chat's system prompt.
#[cfg(feature = "ai")]
pub(super) fn instructions() -> &'static str {
    INSTRUCTIONS
}

/// The DSL reference (the resource `graphersal://dsl-reference`), built once from the engine.
#[cfg(feature = "ai")]
pub(super) fn dsl_reference_text(context: &Context) -> Result<String, String> {
    read_resource(context, "graphersal://dsl-reference", false)
}

/// What a tool does, for its annotations and for `--mcp-read-only`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Read,
    Write,
    Destructive,
}

struct Tool {
    name: &'static str,
    title: &'static str,
    kind: Kind,
    description: String,
    input: Value,
}

impl Tool {
    fn listing(&self, read_only: bool) -> Value {
        // In read-only mode `query` only reads.
        let kind = if read_only { Kind::Read } else { self.kind };
        json!({
            "name": self.name,
            "title": self.title,
            "description": self.description,
            "inputSchema": self.input,
            "annotations": {
                "title": self.title,
                "readOnlyHint": kind == Kind::Read,
                "destructiveHint": kind == Kind::Destructive,
                "idempotentHint": kind == Kind::Read,
                "openWorldHint": false,
            },
        })
    }
}

const QUERY_DESCRIPTION: &str = "Run a Graphersal script against the shared graph and return \
its results as JSON (bounded: at most max_rows top-level results, default 100, and 100 items of \
every nested list; a note says when something was cut).

Before writing a new query, check list_saved_queries: a saved query the user stored for this \
task is run with run_saved_query (or inside a script as g.query(\"name\", #{param: value})).

The language is Gremlin (Apache TinkerPop) in Rhai syntax: `g` is the traversal source, steps \
chain with `.`, strings use double quotes, lists are `[..]`, maps `#{k: v}`. Both spellings of \
every step work: snake_case (has_label, value_map, out_e, in_) and Gremlin camelCase (hasLabel, \
valueMap, outE, in). Anonymous child traversals need an explicit `__.` (`__.out(\"knows\")`, never \
a bare `out()`), tokens are explicit too: `Order.desc`, `T.id`, `T.label`, `P.gt(30)`, \
`P.within([\"a\", \"b\"])`, `Column.keys`, `Scope.local`. A traversal without a terminal is \
executed and all its results returned; terminals such as `.to_list()`, `.next()`, `.count()` \
work as well. A script may have several statements (`let x = ...;`); the last value is the \
result.

Examples:
  g.v().has_label(\"person\").values(\"name\")
  g.V().has(\"person\", \"name\", \"marko\").out(\"knows\").valueMap(true)
  g.v().group_count().by(T.label)
  g.v().has(\"age\", P.gt(30)).order().by(\"age\", Order.desc).limit(5).element_map()
  g.v().has_label(\"person\").project(\"name\", \"friends\").by(\"name\").by(__.out(\"knows\").count())
  g.add_v(\"person\").property(\"name\", \"ada\").property(\"age\", 36)
  g.v().has(\"name\", \"ada\").drop()
  let f = Fake(42); for i in 0..10 { let n = f.full_name(); g.add_v(\"person\").property(\"name\", n).property(\"email\", f.email(n)).property(\"age\", f.int(18, 90)).to_list(); }

The whole script is ONE transaction: it commits only when it succeeds; a failing script changes \
nothing. The answer says whether it changed the graph and the new commit number. An error comes \
back as the engine's diagnostic with a `Help:` line naming the fix: read it before retrying. \
`profile: true` adds the per-step metrics of the optimized plan. The resource \
graphersal://dsl-reference lists every step; graphersal://examples has more examples. Test \
data: `Fake(seed)` (same seed, same values; `f.fork(\"name\")` for independent streams).";

/// The schema format in one line, for the tool descriptions.
const SCHEMA_SHAPE: &str = "{\"mode\": \"open\", \"vertices\": {\"person\": {\"schema\": \
{\"type\": \"object\", \"properties\": {\"name\": {\"type\": \"string\"}, \"age\": {\"type\": \
\"integer\"}}, \"required\": [\"name\"]}}}, \"edges\": {\"knows\": {\"connections\": [{\"from\": \
\"person\", \"to\": \"person\"}], \"schema\": {\"type\": \"object\", \"properties\": {\"weight\": \
{\"type\": \"number\"}}}}}} (each label's properties are a JSON Schema object under \"schema\"; \
mode none/open/closed)";

/// An example JSON Merge Patch of a stored schema, for the tool descriptions.
const PATCH_EXAMPLE: &str = "{\"vertices\": {\"person\": {\"schema\": {\"properties\": \
{\"email\": {\"type\": \"string\"}}}}}} adds an optional property; null deletes a key, e.g. \
{\"vertices\": {\"person\": {\"schema\": {\"properties\": {\"email\": null}}}}}";

fn schema_arg(what: &str) -> Value {
    json!({
        "type": "object",
        "description": format!("{what}, in Graphersal's schema format: {SCHEMA_SHAPE}. infer_schema shows the full shape for the current data. A JSON string is accepted too."),
    })
}

fn mode_arg() -> Value {
    json!({
        "type": "string",
        "enum": ["none", "open", "closed"],
        "description": "Replaces the schema's own mode: none (stored, not enforced), open \
                        (declared fields enforced), closed (also rejects undeclared labels and \
                        properties).",
    })
}

fn no_args() -> Value {
    json!({"type": "object", "properties": {}})
}

const GQL_DESCRIPTION: &str = "Run a GQL (ISO graph query language, Cypher-like) read query \
against the shared graph and return its rows as JSON: one JSON object per row, its keys the \
RETURN columns in the order written (bounded like query's: at most max_rows rows, default 100; \
a note says when something was cut). It runs the script g.gql(query, params) and never changes \
the graph. No Rhai escaping is needed: pass the GQL text as it is.

Supported (stages 1a, 1b, 1c, 2a, 2b, 2c and 2d): MATCH with comma-separated patterns of node patterns \
(v:Label {key: value}) and edge patterns -[e:LABEL]->, <-[e]-, -[e]- (either direction); label \
expressions with | & ! and %; WHERE (comparisons, AND/OR/NOT/XOR, IS [NOT] NULL, IN, STARTS \
WITH, ENDS WITH, CONTAINS); OPTIONAL MATCH (NULL columns when nothing matches); WITH (Cypher \
pipeline: project, aggregate, DISTINCT, ORDER BY, SKIP, LIMIT, WHERE, then more MATCH clauses); \
LET x = expr and FILTER expr; FOR x IN list [WITH ORDINALITY i | WITH OFFSET i] (Cypher \
UNWIND list AS x): one row per item, e.g. FOR id IN $ids MATCH (n) WHERE element_id(n) = id \
(a NULL list gives no row, a non-list value one row; an equality on element_id or on an \
indexed property after FOR is a direct lookup, not a scan); a query may start with MATCH, \
OPTIONAL MATCH, FOR, UNWIND, LET, WITH, FILTER or RETURN (RETURN 1 + 2 AS x runs on one empty \
row); EXISTS subqueries EXISTS { MATCH <pattern> [WHERE <condition>] } and EXISTS { <pattern> } \
and Cypher pattern predicates (a pattern where a condition stands, naming only variables bound \
before it), TRUE or FALSE (never NULL), anywhere an expression stands, each row kept once (a \
semi-join, unlike a MATCH of the same pattern), e.g. who has no project: MATCH (p:person) WHERE \
NOT EXISTS { (p)-[:created]->() } RETURN p.name AS name ORDER BY name, or MATCH (p:person) \
WHERE NOT (p)-[:created]->() ...; map projections p {.name, .age, .*, f, total: expr} \
(Cypher; NULL for a NULL p, while the GQL record {name: p.name} gives {name: NULL}), list \
comprehensions [x IN list WHERE x > 1 | x * 2] (WHERE and | each optional) and pattern \
comprehensions [(p)-[:knows]->(f) WHERE f.age > 30 | f.name] (per row the list of the \
pattern's matches, [] when none, in no defined order), anywhere an expression stands and \
nested: use them for JSON-shaped answers, one row per entity with its related data inside, e.g. \
MATCH (p:person) RETURN p {.name, friends: [(p)-[:knows]->(f) | f.name]} AS person ORDER BY \
person.name; list predicates all(x IN list WHERE cond), any, none, single (NULL when a NULL \
item could decide) and reduce(acc = 0, x IN list | acc + x); COUNT { <pattern> } (the number \
of matches per row, never NULL) and COLLECT { MATCH ... RETURN x } (the list of its one RETURN \
column per row); the body of EXISTS { }, COUNT { } and COLLECT { } may be a whole query with \
WITH, ORDER BY, LIMIT and RETURN; composite queries q1 UNION q2 (duplicate rows removed; UNION \
ALL keeps them; every operand returns the same column names, in any order; ORDER BY and LIMIT \
belong to their operand) and q1 NEXT q2 (the RETURN columns of q1 are the rows of q2, e.g. \
... UNION ... NEXT RETURN name ORDER BY name LIMIT 3); CALL (p) { MATCH ... RETURN ... } runs \
its body once per incoming row and joins the row with each of the body's rows (a row with none \
is dropped; OPTIONAL CALL keeps it with NULL columns; Cypher CALL { WITH p ... } imports p \
too); the body's aggregates, ORDER BY and LIMIT apply per row, so CALL gives top k per group, \
e.g. each person's heaviest software: MATCH (p:person) CALL (p) { MATCH (p)-[e:created]->(s) \
RETURN s.name AS top ORDER BY e.weight DESC LIMIT 1 } RETURN p.name AS who, top ORDER BY who; \
aggregates count(x), count(*), sum, avg, min, max, collect_list \
(Cypher alias collect), each with DISTINCT, grouped implicitly by the other columns (an \
optional GROUP BY after the RETURN items must list exactly those columns); quantified edges -[:knows]->{1,3}, {n}, {m,}, * (0 or more), + (1 or more) and \
Cypher -[e*1..3]-> (a group variable e is the list of edges); path variables p = (a)-->(b) \
returned as a path value, path_length(p) (Cypher alias length), nodes(p) and \
relationships(p) (lists of the path's vertices and edges); CASE, coalesce, nullif; scalar \
functions: conversions CAST(x AS INTEGER|FLOAT|STRING|BOOLEAN) (strict: bad text fails the query \
with GQLSTATUS 22018) vs Cypher toInteger, toFloat, toBoolean, toString (bad text gives NULL) \
and their OrNull forms (NULL for any other kind too); strings upper/toUpper, lower/toLower, \
substring (0-based), left, right, split, replace, reverse, trim, btrim, ltrim, rtrim, \
char_length, size, || concatenation; numbers abs, ceil, floor, round(x[, precision[, mode]]), \
sign, sqrt, exp, ln, log, log10, power, mod; lists head, last, tail, range, size, isEmpty; \
elements keys, properties, labels, type, element_id, startNode(e) and endNode(e) (the edge's \
stored direction), a IS [NOT] SOURCE OF e / DESTINATION OF e; RETURN (mandatory; expressions \
with AS aliases, DISTINCT, RETURN *); ORDER BY (ASC/DESC; NULLs last ascending), OFFSET (or \
SKIP) and LIMIT; $name parameters read from params (values, never spliced into the text). \
String literals use single quotes. A missing property is NULL. Edges in one match are distinct \
(trail semantics), also inside quantified edges. Vertices and edges in a row come back as {id, \
label, properties}. Rows come in no defined order unless the query has ORDER BY: add one when \
the order matters. Not yet supported (refused with a GQLSTATUS error and a Help line): \
quantified path patterns ((a)-->(b)){1,3}, EXCEPT and INTERSECT (filter the first query's \
rows with a pattern predicate instead, e.g. WHERE NOT (p)-[:created]->()), SHORTEST, CALL { } IN TRANSACTIONS and vendor \
procedures (CALL db.labels()), non-deterministic functions (rand, randomUUID), trigonometry, and writes (INSERT, SET, DELETE, CREATE, MERGE): use the query tool \
with Gremlin for those, e.g. g.gql(\"MATCH (p:person) RETURN p\").select(\"p\").out(\"created\").

Examples: MATCH (p:person)-[:knows]->(f:person) WHERE f.age > $min RETURN p.name AS who, \
f.name AS friend ORDER BY friend LIMIT 10 with params {\"min\": 30}; MATCH (p:person) \
OPTIONAL MATCH (p)-[:created]->(s) RETURN p.name AS who, count(s) AS projects ORDER BY \
projects DESC, who; more in the group \
\"GQL\" of the resource graphersal://examples (scripts g.gql(\"...\")).";

fn all_tools() -> Vec<Tool> {
    let tool = |name, title, kind, description: &str, input| Tool {
        name,
        title,
        kind,
        description: description.to_string(),
        input,
    };
    vec![
        tool(
            "query",
            "Run a query",
            Kind::Destructive,
            QUERY_DESCRIPTION,
            json!({
                "type": "object",
                "properties": {
                    "script": {"type": "string", "description": "The Graphersal script (Gremlin in Rhai syntax)."},
                    "profile": {"type": "boolean", "description": "Also return the per-step metrics of the optimized plan (default false)."},
                    "max_rows": {"type": "integer", "minimum": 1, "description": "Most top-level results returned (default 100)."},
                },
                "required": ["script"],
            }),
        ),
        tool(
            "gql",
            "Run a GQL query",
            Kind::Read,
            GQL_DESCRIPTION,
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "The GQL read query (MATCH ... RETURN ...), as plain text."},
                    "params": {"type": "object", "description": "Parameter name -> value; $name in the query reads the entry name."},
                    "profile": {"type": "boolean", "description": "Also return the per-step metrics of the Gremlin plan the query runs as (default false)."},
                    "max_rows": {"type": "integer", "minimum": 1, "description": "Most rows returned (default 100)."},
                },
                "required": ["query"],
            }),
        ),
        tool(
            "list_saved_queries",
            "Saved queries",
            Kind::Read,
            "The saved queries stored in the graph: named, parameterized queries the user wrote \
             for recurring questions and tasks. Each has a name, a folder, a description, its \
             parameters (JSON Schema, default, required, description), a status (error: it \
             cannot run) and writes (true: it changes data, effects lists what; run it only when \
             the user asked for that change). Check them before writing a new query, and run \
             one with run_saved_query.",
            json!({
                "type": "object",
                "properties": {
                    "folder": {"type": "string", "description": "Only the queries in this folder and below it (e.g. \"reports\" lists reports and reports/sales)."},
                    "search": {"type": "string", "description": "Only the queries whose name or description contains this text (case-insensitive)."},
                },
            }),
        ),
        tool(
            "run_saved_query",
            "Run a saved query",
            Kind::Destructive,
            "Runs a saved query by name with named parameters, exactly like the script \
             g.query(name, #{..}): the parameters are checked against their declared schemas \
             (missing required, wrong type, violated constraint: an error with a Help line), \
             defaults fill the omitted ones, and the answer is bounded like query's. A query \
             listed with writes: true changes data (only what its effects name, as one \
             transaction): run it only when the user asked for that change.",
            json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "The saved query's name (list_saved_queries)."},
                    "params": {"type": "object", "description": "Parameter name -> value; omit a parameter to use its default."},
                    "max_rows": {"type": "integer", "minimum": 1, "description": "Most top-level results returned (default 100)."},
                },
                "required": ["name"],
            }),
        ),
        tool(
            "list_compressions",
            "Compression rules",
            Kind::Read,
            "The compression rules of the graph: strings of one vertex or edge label at one \
             property path that are kept LZ4-compressed in memory (codec lz4) or stored once \
             per distinct value (codec dedup); reads are always plain. Each has name, element, \
             label, path, codec, minBytes, dictionary, served (false: the rule is kept but \
             not applied, values stay plain; reason says why, such as a codec of a newer \
             version) and its current stats (compressedValues, plainBytes, storedBytes, \
             dictionaryBytes, distinctValues: the distinct texts of a dedup rule).",
            no_args(),
        ),
        tool(
            "estimate_compression",
            "Estimate a compression rule",
            Kind::Read,
            "Measures what compression rules for one vertex or edge label and property path \
             would hold over the current values, WITHOUT changing the graph: lz4 with and \
             without a dictionary at minBytes 64, 128, 256, 512 and 1024 (or only the codec, \
             minBytes and dictionary given; codec [\"lz4\", \"dedup\"] also measures dedup at \
             25, 64, 128 and 256), plus the rule that covers the path today. Answers \
             valuesFound, stringValues, a size histogram, per candidate compressedValues, \
             plainBytes, storedBytes, dictionaryBytes, distinctValues (dedup), savedBytes, ratio \
             and the read (decode) speed, the best candidate's index and warnings (unknown \
             label, no value at the path, values that are not strings, high_cardinality: \
             dedup saves nothing). Run it before define_compression to choose the codec, \
             minBytes and dictionary, and to catch a typo in the label or path.",
            json!({
                "type": "object",
                "properties": {
                    "label": {"type": "string", "description": "The vertex or edge label."},
                    "path": {"type": "string", "description": "The property path: a name or a jpath of names (details.career.cv)."},
                    "element": {"type": "string", "enum": ["vertex", "edge"], "description": "Default vertex."},
                    "codec": {
                        "description": "Measure only this codec, or these codecs (default: lz4 only). dedup stores each distinct value once: long repeated values.",
                        "oneOf": [
                            {"type": "string", "enum": ["lz4", "dedup"]},
                            {"type": "array", "items": {"type": "string", "enum": ["lz4", "dedup"]}},
                        ],
                    },
                    "minBytes": {"type": "integer", "minimum": 1, "description": "Measure only this threshold (default: several)."},
                    "dictionary": {"type": "boolean", "description": "Measure only with (true) or without (false) a dictionary (default: both)."},
                },
                "required": ["label", "path"],
            }),
        ),
        tool(
            "define_compression",
            "Define a compression rule",
            Kind::Write,
            "Stores (or replaces) a compression rule: strings of `element`s labelled `label` at \
             `path` (a jpath of names such as \"bio\" or \"details.career.cv\"; no index) are \
             kept compressed in memory when they are at least minBytes long and compress to at \
             most 90 %, or (codec dedup) stored once per distinct value when they are at least \
             minBytes and more than 24 bytes long. The label's existing values are re-encoded \
             at once. lz4 is worth it for large text that queries rarely read (CVs, \
             descriptions, document bodies; a read or a has() decompresses it); dedup for long \
             values repeated across many elements (paths, URLs, addresses, category texts; \
             reads cost nothing extra). One rule per element, label and path.",
            json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "The rule's name (an identifier, e.g. customer_cv)."},
                    "label": {"type": "string", "description": "The vertex or edge label."},
                    "path": {"type": "string", "description": "The property path: a name or a jpath of names (details.career.cv)."},
                    "element": {"type": "string", "enum": ["vertex", "edge"], "description": "Default vertex."},
                    "codec": {"type": "string", "enum": ["lz4", "dedup"], "description": "lz4 (default) compresses each value; dedup stores each distinct value once."},
                    "minBytes": {"type": "integer", "minimum": 1, "description": "Shorter strings stay plain (default 256 for lz4, 25 for dedup)."},
                    "dictionary": {"type": "boolean", "description": "lz4 only: build a dictionary from the rule's values (default true; a dedup rule has none)."},
                    "replaces": {"type": "string", "description": "Rename: the rule this one replaces (dropped in the same commit)."},
                },
                "required": ["name", "label", "path"],
            }),
        ),
        tool(
            "drop_compression",
            "Drop a compression rule",
            Kind::Destructive,
            "Removes a compression rule; its values are decompressed (no data changes).",
            json!({
                "type": "object",
                "properties": {"name": {"type": "string", "description": "The rule's name (list_compressions)."}},
                "required": ["name"],
            }),
        ),
        tool(
            "recompress",
            "Recompress a rule",
            Kind::Write,
            "Rebuilds a compression rule's dictionary from the current values and re-encodes \
             them (after much data was added); changes no data.",
            json!({
                "type": "object",
                "properties": {"name": {"type": "string", "description": "The rule's name (list_compressions)."}},
                "required": ["name"],
            }),
        ),
        tool(
            "list_indexes",
            "Property indexes",
            Kind::Read,
            "The property indexes of the graph: the vertices or edges of one label ordered by \
             one top-level property, so that v()/e() with has_label(label) and has(key, ..) \
             (equality, P.within, ranges) read only the matching elements. Each has name, \
             element, label, key, type (memory-ordered), transform (null, or to_lower: the \
             lowered text of string values), served (false: the definition is kept but no \
             query reads it; reason says why, such as a storage without indexes or a type or \
             transform this version does not know) and its current stats (entries, \
             distinctValues, skipped, bytes; a to_lower index is read only while skipped, the \
             count of non-string values at the key, is 0).",
            no_args(),
        ),
        tool(
            "create_index",
            "Create a property index",
            Kind::Write,
            "Declares a property index on the `element`s labelled `label`, ordered by their \
             top-level property `key`, and builds it from the data. Queries that start with \
             g.v().has_label(label) (g.e() for edges) and filter has(key, value), \
             has(key, P.within(..)) or a range (P.gt/gte/lt/lte/between/inside/outside) then \
             read only the matching elements; results are the same as without the index, only \
             their order may differ. It costs memory (see bytes in list_indexes) and a little \
             time on every write of the key. One index per element, label, key, type and \
             transform; `name` defaults to <label>_<key> (<label>_<key>_edge for edges, then \
             _to_lower with that transform), `type` to memory-ordered (an ordered index in \
             memory holding scalar values: null, booleans, numbers, strings, uuids, dates and \
             times). `transform` \"to_lower\" indexes the lowered text of string values for a \
             case-insensitive search written has(key, __.to_lower().is(\"alice\")) (also \
             P.within and ranges; the constants compared as given). A key the schema declares \
             as a vector, an array or an object is refused (search vectors with \
             nearest()/similarity()), with to_lower any non-string key.",
            json!({
                "type": "object",
                "properties": {
                    "label": {"type": "string", "description": "The vertex or edge label."},
                    "key": {"type": "string", "description": "The top-level property name (not a jpath)."},
                    "element": {"type": "string", "enum": ["vertex", "edge"], "description": "Default vertex."},
                    "name": {"type": "string", "description": "The index's name (an identifier); default <label>_<key>."},
                    "type": {
                        "type": "string",
                        "enum": graphersal::catalog::PropertyIndexType::KNOWN_NAMES,
                        "description": "The index type; default memory-ordered (an ordered index kept in memory, rebuilt from the data on open; serves =, P.within and ranges).",
                    },
                    "transform": {
                        "type": "string",
                        "enum": graphersal::catalog::PropertyIndexTransform::KNOWN_NAMES,
                        "description": "Optional: to_lower indexes the lowered text of string values, for has(key, __.to_lower().is(..)) (case-insensitive search). Default: none (the values themselves).",
                    },
                },
                "required": ["label", "key"],
            }),
        ),
        tool(
            "drop_index",
            "Drop a property index",
            Kind::Destructive,
            "Removes a property index (no data changes; queries scan again).",
            json!({
                "type": "object",
                "properties": {"name": {"type": "string", "description": "The index's name (list_indexes)."}},
                "required": ["name"],
            }),
        ),
        tool(
            "get_schema",
            "Stored schema",
            Kind::Read,
            "The schema stored in the graph (Graphersal's schema format, JSON), or null when \
             the graph stores none (then infer_schema shows what the data has).",
            no_args(),
        ),
        tool(
            "infer_schema",
            "Inferred schema",
            Kind::Read,
            "The schema the data satisfies now (labels, properties, types, edge topology; \
             mode none). Stores nothing; a good start for set_schema.",
            no_args(),
        ),
        tool(
            "validate_schema",
            "Check a schema",
            Kind::Read,
            "Checks a proposed schema against the data without applying it: violations by \
             rule with sample element ids, the diff from the stored schema, and whether it is \
             backward compatible.",
            json!({"type": "object", "properties": {"schema": schema_arg("The proposed schema"), "mode": mode_arg()}, "required": ["schema"]}),
        ),
        tool(
            "validate_schema_patch",
            "Check a schema patch",
            Kind::Read,
            &format!(
                "validate_schema of the stored schema with a JSON Merge Patch (RFC 7396) \
                 applied: {PATCH_EXAMPLE}. Applies nothing."
            ),
            json!({"type": "object", "properties": {"patch": {"type": "object", "description": "A JSON Merge Patch of the stored schema (a JSON string is accepted too)."}}, "required": ["patch"]}),
        ),
        tool(
            "diff_schema",
            "Diff a schema",
            Kind::Read,
            "The changes from the stored schema to a proposed one, each marked backward \
             compatible or not.",
            json!({"type": "object", "properties": {"schema": schema_arg("The proposed schema")}, "required": ["schema"]}),
        ),
        tool(
            "set_schema",
            "Replace the schema",
            Kind::Destructive,
            "Stores a schema (replacing the stored one). In mode open/closed the existing data \
             is validated first: on violations nothing changes and the error lists them. Run \
             validate_schema first.",
            json!({"type": "object", "properties": {"schema": schema_arg("The new schema"), "mode": mode_arg()}, "required": ["schema"]}),
        ),
        tool(
            "patch_schema",
            "Patch the schema",
            Kind::Write,
            &format!(
                "Applies a JSON Merge Patch (RFC 7396) to the stored schema and stores the \
                 result (validated against the data like set_schema); returns the new schema. \
                 Example: {PATCH_EXAMPLE}. Without a stored schema, set_schema first (e.g. \
                 the infer_schema result with mode open)."
            ),
            json!({"type": "object", "properties": {"patch": {"type": "object", "description": "A JSON Merge Patch of the stored schema (a JSON string is accepted too)."}}, "required": ["patch"]}),
        ),
        tool(
            "statistics",
            "Statistics",
            Kind::Read,
            "Counts of the graph (vertices and edges in total and per label), its memory \
             footprint, and the limits every query runs under.",
            no_args(),
        ),
        tool(
            "graph_info",
            "Graph info",
            Kind::Read,
            "What the server serves: the graph's name, vertex and edge counts, schema mode, \
             source file or Store directory, the read-only view of a past state if the user \
             opened one in the browser (view, and a note: writes are refused while it is \
             open), damage found on the store's disk while it is open (damageFound: writes are \
             refused when it says frozen), and whether MCP is read-only.",
            no_args(),
        ),
    ]
}

/// The tools offered: all of them, only the reading ones (and `query`, `run_saved_query`) when
/// `read_only` (`--mcp-read-only`, an `--ai` file's `read_only`).
fn tools(read_only: bool) -> Vec<Tool> {
    all_tools()
        .into_iter()
        .filter(|t| {
            !read_only || t.kind == Kind::Read || t.name == "query" || t.name == "run_saved_query"
        })
        .collect()
}

/// Who calls a tool through [`call_tool`]: the MCP endpoint or an AI Chat conversation.
pub(super) struct ToolCaller {
    /// Only reading tools; `query` runs under the read-only authorizer.
    pub(super) read_only: bool,
    /// Every saved query is a tool of its own (`--mcp-query-tools`).
    pub(super) query_tools: bool,
    /// The change-feed client of the calls: `mcp`, or `ai` with the conversation id.
    pub(super) client: Client,
    /// The caller's own cancellation token of the queries it runs (an AI Chat turn's Stop),
    /// instead of the session's (the UI's Stop, `POST /api/cancel`).
    pub(super) cancel: Option<CancelToken>,
}

/// The answer of a tool: its text (data for the model, cut to [`MAX_TEXT_CHARS`]) and whether
/// it is a tool error.
pub(super) struct ToolOutcome {
    pub(super) text: String,
    pub(super) is_error: bool,
}

/// A tool call refused before it ran: an unknown tool or arguments that are not an object
/// (MCP: the JSON-RPC error "invalid params"), or a failure of the session (`Internal`).
pub(super) enum CallRefused {
    Params(String),
    Internal(String),
}

impl CallRefused {
    /// The message.
    #[cfg(feature = "ai")]
    pub(super) fn message(&self) -> &str {
        match self {
            Self::Params(message) | Self::Internal(message) => message,
        }
    }
}

/// The tool catalog of a caller, as MCP `tools/list` entries (`name`, `title`, `description`,
/// `inputSchema`, `annotations`): every tool, only the reading ones (and `query`) when
/// `read_only`, plus every saved query as a tool of its own with `query_tools`. The MCP endpoint
/// and the AI Chat agent list the same catalog.
pub(super) fn tool_listings(
    context: &Context,
    read_only: bool,
    query_tools: bool,
) -> Result<Vec<Value>, String> {
    let mut listed: Vec<Value> = tools(read_only)
        .into_iter()
        .map(|tool| tool.listing(read_only))
        .collect();
    if query_tools {
        listed.extend(query_tool_listings(context, read_only)?);
    }
    Ok(listed)
}

/// Runs the tool `name` with `args` for `caller` (the MCP endpoint's `tools/call` and the AI
/// Chat agent): the session calls queue on the session thread like the browser's, and the
/// change feed records their commits with `caller.client`. A failing tool is a
/// [`ToolOutcome`] with `is_error` (its text the diagnostic, with the read-only view note).
pub(super) fn call_tool(
    context: &Context,
    caller: &ToolCaller,
    name: &str,
    args: &Value,
) -> Result<ToolOutcome, CallRefused> {
    as_client(caller.client.clone(), || {
        call_tool_as_client(context, caller, name, args)
    })
}

fn call_tool_as_client(
    context: &Context,
    caller: &ToolCaller,
    name: &str,
    args: &Value,
) -> Result<ToolOutcome, CallRefused> {
    let outcome = |result: Result<String, String>| match result {
        Ok(text) => ToolOutcome {
            text: cut(text),
            is_error: false,
        },
        Err(text) => ToolOutcome {
            text: cut(with_view_note(context, text)),
            is_error: true,
        },
    };
    let Some(tool) = tools(caller.read_only).into_iter().find(|t| t.name == name) else {
        if caller.query_tools && !all_tools().iter().any(|t| t.name == name) {
            let wanted = name.to_string();
            let known = context
                .call(move |s| !s.get_query(&wanted).is_null())
                .map_err(|err| CallRefused::Internal(err.1))?;
            if known {
                if !args.is_object() {
                    return Err(CallRefused::Params("arguments must be an object".into()));
                }
                let call = json!({"name": name, "params": args});
                return Ok(outcome(run_saved_query(context, caller, &call)));
            }
        }
        let why = if all_tools().iter().any(|t| t.name == name) {
            " (read-only: only reading tools)"
        } else {
            ""
        };
        return Err(CallRefused::Params(format!("unknown tool {name:?}{why}")));
    };
    if !args.is_object() {
        return Err(CallRefused::Params("arguments must be an object".into()));
    }
    Ok(outcome(run_tool(context, caller, tool.name, args)))
}

/// The note an agent gets while the user has a read-only view of a past state open in the
/// browser (Store menu): what it serves and why writes fail. MCP cannot close the view (the
/// endpoint has no persistence tools); `None` on the live graph.
fn view_note(view: &Value) -> Option<String> {
    if view.is_null() {
        return None;
    }
    let commit = view["commitSeq"].as_u64().unwrap_or(0);
    // The target when it says more than the commit (a mark, a time).
    let target = view["target"].as_str().unwrap_or("");
    let target = if target.is_empty() || target.starts_with("commit ") {
        String::new()
    } else {
        format!(" ({target})")
    };
    Some(format!(
        "the served state is a read-only view at commit {commit}{target}, opened by the user in \
         the browser's Store menu: queries read that state, every write is refused until the \
         user goes back to the live graph"
    ))
}

/// A tool error's text, with the [`view_note`] when a read-only view is open.
fn with_view_note(context: &Context, text: String) -> String {
    match view_note(&current_view(context)) {
        Some(note) => format!("{text}\nnote: {note}."),
        None => text,
    }
}

fn tool_result(text: String, is_error: bool) -> Value {
    json!({
        "content": [{"type": "text", "text": text}],
        "isError": is_error,
    })
}

/// Cuts `text` to [`MAX_TEXT_CHARS`] with a note.
fn cut(text: String) -> String {
    if text.chars().count() <= MAX_TEXT_CHARS {
        return text;
    }
    let kept: String = text.chars().take(MAX_TEXT_CHARS).collect();
    format!(
        "{kept}\n… (cut at {MAX_TEXT_CHARS} characters; narrow the query, e.g. with limit(), \
         values(..) or a smaller max_rows)"
    )
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

/// A schema or patch argument as JSON text: an object, or a string holding JSON. `mode` set on
/// a schema object replaces its own.
fn json_arg(args: &Value, key: &str, mode: Option<&str>) -> Result<String, String> {
    let mut value = match args.get(key) {
        None | Some(Value::Null) => return Err(format!("missing argument {key:?}")),
        Some(Value::String(text)) => serde_json::from_str::<Value>(text)
            .map_err(|err| format!("argument {key:?} is not JSON: {err}"))?,
        Some(other) => other.clone(),
    };
    if let (Some(mode), Some(map)) = (mode, value.as_object_mut()) {
        map.insert("mode".into(), json!(mode));
    }
    Ok(value.to_string())
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn http_failed(err: HttpError) -> String {
    err.1
}

/// Runs one tool; `Err` is a tool error (its text is the diagnostic).
fn run_tool(
    context: &Context,
    caller: &ToolCaller,
    name: &str,
    args: &Value,
) -> Result<String, String> {
    match name {
        "query" => query(context, caller, args),
        "gql" => gql(context, caller, args),
        "get_schema" => {
            let text = context.call(|s| s.get_schema_text()).map_err(http_failed)?;
            Ok(if text == "null" {
                "null: the graph stores no schema (mode none). infer_schema shows what the data \
                 has; set_schema stores one."
                    .to_string()
            } else {
                text
            })
        }
        "infer_schema" => context.call(|s| s.infer_schema_text()).map_err(http_failed),
        "validate_schema" => {
            let schema = json_arg(args, "schema", str_arg(args, "mode"))?;
            fallible(context, move |s| s.validate_schema(&schema))
        }
        "validate_schema_patch" => {
            let patch = json_arg(args, "patch", None)?;
            fallible(context, move |s| s.validate_schema_patch(&patch))
        }
        "diff_schema" => {
            let schema = json_arg(args, "schema", None)?;
            fallible(context, move |s| s.diff_schema(&schema))
        }
        "set_schema" => {
            let schema = json_arg(args, "schema", None)?;
            let mode = str_arg(args, "mode").map(str::to_string);
            let info = context
                .call(move |s| s.set_schema(&schema, mode.as_deref()))
                .map_err(http_failed)??;
            Ok(format!("Schema stored.\n{}", pretty(&info)))
        }
        "patch_schema" => {
            let patch = json_arg(args, "patch", None)?;
            let text = context
                .call(move |s| s.patch_schema_text(&patch))
                .map_err(http_failed)??;
            Ok(format!(
                "Schema patched; the stored schema is now:\n{}",
                text
            ))
        }
        "statistics" => Ok(pretty(
            &context.call(|s| s.statistics()).map_err(http_failed)?,
        )),
        "list_saved_queries" => list_saved_queries(context, caller.read_only, args),
        "run_saved_query" => run_saved_query(context, caller, args),
        "list_compressions" => {
            let list = context
                .call(|s| s.list_compressions())
                .map_err(http_failed)?;
            Ok(if list.as_array().is_some_and(Vec::is_empty) {
                "The graph has no compression rules (define_compression adds one).".to_string()
            } else {
                pretty(&list)
            })
        }
        "estimate_compression" => {
            let mut spec = args.clone();
            // `min_bytes` (the DSL's spelling) is accepted too.
            if let Some(map) = spec.as_object_mut()
                && let Some(value) = map.remove("min_bytes")
            {
                map.entry("minBytes").or_insert(value);
            }
            let answer = context
                .call(move |s| s.estimate_compression(&spec))
                .map_err(http_failed)?;
            if answer["ok"] == json!(true) {
                Ok(format!(
                    "Estimate (the graph was not changed; best is the index of the candidate \
                     that saves the most, savedBytes = plainBytes - storedBytes - \
                     dictionaryBytes):\n{}",
                    pretty(&answer["estimate"])
                ))
            } else {
                Err(format!(
                    "The estimate was refused:\n{}",
                    pretty(&answer["errors"])
                ))
            }
        }
        "define_compression" => {
            let mut spec = args.clone();
            // `min_bytes` (the DSL's spelling) is accepted too.
            if let Some(map) = spec.as_object_mut()
                && let Some(value) = map.remove("min_bytes")
            {
                map.entry("minBytes").or_insert(value);
            }
            let answer = context
                .call(move |s| s.define_compression(&spec))
                .map_err(http_failed)?;
            if answer["ok"] == json!(true) {
                Ok(format!("Rule stored.\n{}", pretty(&answer["rule"])))
            } else {
                Err(format!(
                    "The rule was refused:\n{}",
                    pretty(&answer["errors"])
                ))
            }
        }
        "drop_compression" => {
            let name = str_arg(args, "name")
                .ok_or("missing argument \"name\" (list_compressions)")?
                .to_string();
            let dropped = context
                .call(move |s| s.drop_compression(&name))
                .map_err(http_failed)??;
            Ok(if dropped {
                "Rule dropped; its values are plain again.".to_string()
            } else {
                "There is no rule of that name (list_compressions).".to_string()
            })
        }
        "recompress" => {
            let name = str_arg(args, "name")
                .ok_or("missing argument \"name\" (list_compressions)")?
                .to_string();
            fallible(context, move |s| s.recompress(&name))
        }
        "list_indexes" => {
            let list = context.call(|s| s.list_indexes()).map_err(http_failed)?;
            Ok(if list.as_array().is_some_and(Vec::is_empty) {
                "The graph has no property indexes (create_index adds one).".to_string()
            } else {
                pretty(&list)
            })
        }
        "create_index" => {
            let spec = args.clone();
            let answer = context
                .call(move |s| s.create_index(&spec))
                .map_err(http_failed)?;
            if answer["ok"] == json!(true) {
                Ok(format!("Index created.\n{}", pretty(&answer["index"])))
            } else {
                Err(format!(
                    "The index was refused:\n{}",
                    pretty(&answer["errors"])
                ))
            }
        }
        "drop_index" => {
            let name = str_arg(args, "name")
                .ok_or("missing argument \"name\" (list_indexes)")?
                .to_string();
            let dropped = context
                .call(move |s| s.drop_index(&name))
                .map_err(http_failed)??;
            Ok(if dropped {
                "Index dropped.".to_string()
            } else {
                "There is no index of that name (list_indexes).".to_string()
            })
        }
        "graph_info" => {
            let mut info = context.call(|s| s.info()).map_err(http_failed)?;
            info["source"] = json!(context.source.as_ref().map(|p| p.display().to_string()));
            info["store"] = json!(context.store.as_ref().map(|s| s.location()));
            let view = current_view(context);
            if let Some(note) = view_note(&view) {
                info["note"] = json!(note);
            }
            info["view"] = view;
            // Damage found on disk while the store is open (writes refused when frozen).
            info["damageFound"] = context.store.as_ref().map_or(Value::Null, |s| {
                graphersal_session::store::found_damage_value(&**s)
            });
            info["readOnly"] = json!(caller.read_only);
            if let Some(mcp) = &context.mcp {
                info["mcpReadOnly"] = json!(mcp.read_only);
            }
            Ok(pretty(&info))
        }
        other => Err(format!("unknown tool {other:?}")),
    }
}

fn fallible(
    context: &Context,
    job: impl FnOnce(&mut Session) -> Result<Value, String> + Send + 'static,
) -> Result<String, String> {
    Ok(pretty(&context.call(job).map_err(http_failed)??))
}

/// The saved queries as the agent sees them: the session's entries (`folder` prefix, `search`
/// in name or description), compact.
/// The saved queries as compact entries (no `null` fields, `effects` as their texts, no empty
/// `denied_writes`), `folder` and `search` as `list_saved_queries` takes them; `read_only`
/// leaves out every query that writes.
fn saved_queries(
    context: &Context,
    folder: Option<String>,
    search: Option<String>,
    read_only: bool,
) -> Result<Vec<Value>, String> {
    let list = context
        .call(move |s| s.list_queries(folder.as_deref()))
        .map_err(http_failed)?;
    let search = search.map(|s| s.to_lowercase());
    Ok(list
        .as_array()
        .into_iter()
        .flatten()
        .filter(|entry| !(read_only && entry["writes"].as_bool().unwrap_or(false)))
        .filter(|entry| {
            search.as_deref().is_none_or(|needle| {
                ["name", "description"].iter().any(|key| {
                    entry[*key]
                        .as_str()
                        .is_some_and(|text| text.to_lowercase().contains(needle))
                })
            })
        })
        .map(|entry| {
            let mut compact = serde_json::Map::new();
            for (key, value) in entry.as_object().into_iter().flatten() {
                let value = match key.as_str() {
                    // The effect texts are enough for an agent (`Create Data(vertex "person")`).
                    "effects" | "denied_writes" => Value::Array(
                        value
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|effect| effect.get("text").cloned())
                            .collect(),
                    ),
                    _ => value.clone(),
                };
                let empty = value.as_array().is_some_and(Vec::is_empty);
                if !value.is_null() && !(empty && key != "params") {
                    compact.insert(key.clone(), value);
                }
            }
            Value::Object(compact)
        })
        .collect())
}

/// The `list_saved_queries` tool.
fn list_saved_queries(context: &Context, read_only: bool, args: &Value) -> Result<String, String> {
    let folder = str_arg(args, "folder").map(str::to_string);
    let search = str_arg(args, "search").map(str::to_string);
    let filtered = folder.is_some() || search.is_some();
    let list = saved_queries(context, folder, search, read_only)?;
    if list.is_empty() {
        return Ok(if filtered {
            "No saved query matches (call list_saved_queries without arguments to see all).".into()
        } else {
            "The graph has no saved queries: write the query with the query tool.".into()
        });
    }
    Ok(format!(
        "{} saved quer{} (run one with run_saved_query {{name, params}}; status \"error\" means \
         it cannot run, see \"error\"; writes: true means it changes data, see \"effects\": run \
         it only when the user asked for that change):\n{}",
        list.len(),
        if list.len() == 1 { "y" } else { "ies" },
        pretty(&Value::Array(list))
    ))
}

/// The `run_saved_query` tool (and a saved query's own tool with `--mcp-query-tools`): the
/// call text `g.query(name, #{..})` run like the `query` tool.
fn run_saved_query(context: &Context, caller: &ToolCaller, args: &Value) -> Result<String, String> {
    let name = str_arg(args, "name").ok_or("missing argument \"name\" (list_saved_queries)")?;
    let params = match args.get("params") {
        None | Some(Value::Null) => Value::Null,
        Some(Value::String(text)) => serde_json::from_str(text)
            .map_err(|err| format!("argument \"params\" is not a JSON object: {err}"))?,
        Some(other) => other.clone(),
    };
    let script = graphersal_session::queries::query_call_text(name, &params)?;
    if caller.read_only {
        let wanted = name.to_string();
        let entry = context
            .call(move |s| s.get_query(&wanted))
            .map_err(http_failed)?;
        if entry["writes"].as_bool().unwrap_or(false) {
            return Err(format!(
                "the saved query \"{name}\" changes data, and this server is read-only for MCP"
            ));
        }
    }
    let max_rows = args
        .get("max_rows")
        .and_then(Value::as_u64)
        .map_or(DEFAULT_MAX_ROWS, |n| n.max(1) as usize);
    let (ok, text) = run_script(context, caller, script.clone(), false, max_rows)?;
    run_result((ok, format!("call: {script}\n{text}")))
}

/// With `--mcp-query-tools`: every saved query as a tool listing (`inputSchema` from its
/// parameters); a query named like a built-in tool is left out (`run_saved_query` reaches it).
fn query_tool_listings(context: &Context, read_only: bool) -> Result<Vec<Value>, String> {
    let list = saved_queries(context, None, None, read_only)?;
    Ok(list
        .iter()
        .filter_map(|entry| {
            let name = entry["name"].as_str()?;
            if all_tools().iter().any(|t| t.name == name) {
                return None;
            }
            let mut properties = serde_json::Map::new();
            let mut required = Vec::new();
            for param in entry["params"].as_array().into_iter().flatten() {
                let Some(param_name) = param["name"].as_str() else {
                    continue;
                };
                let mut schema = param["schema"].clone();
                if let (Some(default), Some(map)) = (param.get("default"), schema.as_object_mut()) {
                    map.insert("default".into(), default.clone());
                }
                properties.insert(param_name.to_string(), schema);
                if param["required"].as_bool().unwrap_or(false) {
                    required.push(json!(param_name));
                }
            }
            let mut description = entry["description"]
                .as_str()
                .map_or_else(|| format!("The saved query {name}."), str::to_string);
            if !description.trim_end().ends_with(['.', '!', '?']) {
                description.push('.');
            }
            if let Some(folder) = entry["folder"].as_str() {
                description.push_str(&format!(" Folder: {folder}."));
            }
            let writes = entry["writes"].as_bool().unwrap_or(false);
            let effects: Vec<&str> = entry["effects"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            if writes {
                description.push_str(&format!(
                    " Saved query that CHANGES DATA ({}); run it only when the user asked for \
                     that change. The same as run_saved_query.",
                    effects.join(", ")
                ));
            } else {
                description.push_str(" Saved query (read-only); the same as run_saved_query.");
            }
            if let Some(error) = entry["error"].as_str() {
                description.push_str(&format!(" It cannot run: {error}"));
            }
            let destructive = effects
                .iter()
                .any(|effect| effect.starts_with("Update") || effect.starts_with("Delete"));
            Some(json!({
                "name": name,
                "title": name,
                "description": description,
                "inputSchema": {
                    "type": "object",
                    "properties": properties,
                    "required": required,
                    "additionalProperties": false,
                },
                "annotations": {
                    "title": name,
                    "readOnlyHint": !writes,
                    "destructiveHint": writes && destructive,
                    "idempotentHint": !writes,
                    "openWorldHint": false,
                },
            }))
        })
        .collect())
}

/// The `gql` tool: the call text `g.gql(query, #{params})` (the query one Rhai string literal,
/// the parameters Rhai literals) run like the `query` tool; read-only by construction, and under
/// the read-only authorizer for a read-only caller like every script.
fn gql(context: &Context, caller: &ToolCaller, args: &Value) -> Result<String, String> {
    let text = str_arg(args, "query").ok_or("missing argument \"query\" (the GQL query to run)")?;
    let params = match args.get("params") {
        None | Some(Value::Null) => Value::Null,
        Some(Value::String(text)) => serde_json::from_str(text)
            .map_err(|err| format!("argument \"params\" is not a JSON object: {err}"))?,
        Some(other) => other.clone(),
    };
    let script = graphersal_session::queries::gql_call_text(text, &params)?;
    let profile = args
        .get("profile")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let max_rows = args
        .get("max_rows")
        .and_then(Value::as_u64)
        .map_or(DEFAULT_MAX_ROWS, |n| n.max(1) as usize);
    run_result(run_script(context, caller, script, profile, max_rows)?)
}

/// The `query` tool: one run of the session (like the browser's Run), the answer as text.
fn query(context: &Context, caller: &ToolCaller, args: &Value) -> Result<String, String> {
    let script = str_arg(args, "script")
        .ok_or("missing argument \"script\" (the Graphersal script to run)")?
        .to_string();
    let profile = args
        .get("profile")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let max_rows = args
        .get("max_rows")
        .and_then(Value::as_u64)
        .map_or(DEFAULT_MAX_ROWS, |n| n.max(1) as usize);
    run_result(run_script(context, caller, script, profile, max_rows)?)
}

/// Runs `script` on the session (under the read-only authorizer for a read-only caller; with
/// the caller's own cancellation token when it has one): whether it succeeded and the answer's
/// text.
fn run_script(
    context: &Context,
    caller: &ToolCaller,
    script: String,
    profile: bool,
    max_rows: usize,
) -> Result<(bool, String), String> {
    let options = RunOptions {
        profile,
        max_rows: Some(max_rows),
        max_items: Some(DEFAULT_MAX_ITEMS),
        ..RunOptions::default()
    };
    let authorizer = if caller.read_only {
        read_only_authorizer()
    } else {
        playground_authorizer()
    };
    let answer = match caller.cancel.clone() {
        // The caller's own token: the session-wide Stop (`POST /api/cancel`) does not reach it.
        Some(cancel) => context.call(move |s| {
            let text = s.execute_with_cancel(&script, &options, authorizer, &cancel);
            (text, s.graph().last_commit_seq())
        }),
        None => {
            context.running.fetch_add(1, Ordering::SeqCst);
            let answer = context.call(move |s| {
                let text = s.execute_with_authorizer(&script, &options, authorizer);
                (text, s.graph().last_commit_seq())
            });
            context.running.fetch_sub(1, Ordering::SeqCst);
            answer
        }
    };
    let (text, commit_seq) = answer.map_err(http_failed)?;
    let response: Value =
        serde_json::from_str(&text).map_err(|err| format!("malformed engine answer: {err}"))?;
    Ok(query_text(&response, commit_seq, profile))
}

/// `Ok` for a successful run, `Err` (a tool error) for a failed one.
fn run_result((ok, text): (bool, String)) -> Result<String, String> {
    if ok { Ok(text) } else { Err(text) }
}

/// The text of a `query` answer: a status line, the results (JSON within the display limits) or
/// the error's diagnostic, and the profile table when asked.
fn query_text(response: &Value, commit_seq: u64, profile: bool) -> (bool, String) {
    let ok = response["ok"].as_bool().unwrap_or(false);
    let execution = &response["execution"];
    let elapsed = response["elapsed_ms"].as_f64().unwrap_or(0.0);
    let mut out = Vec::new();
    if ok {
        let changes = &response["changes"];
        let changed = match (
            changes["data"].as_bool().unwrap_or(false),
            changes["schema"].as_bool().unwrap_or(false),
        ) {
            (true, true) => "changed the data and the schema",
            (true, false) => "changed the data",
            (false, true) => "changed the schema",
            (false, false) => "",
        };
        let display = &response["display"];
        let total = display["total_rows"].as_u64();
        let mut status = match total {
            Some(n) => format!("ok: {n} result{}", if n == 1 { "" } else { "s" }),
            None => "ok".to_string(),
        };
        status.push_str(&format!(" ({elapsed:.1} ms)"));
        if response["mutated"].as_bool().unwrap_or(false) {
            let what = if changed.is_empty() {
                "changed the graph"
            } else {
                changed
            };
            status.push_str(&format!("; {what}, committed as commit {commit_seq}"));
        }
        out.push(status);
        let shown = if response["preferred_view"] == "raw" || response["terminal"] == "profile" {
            display["raw"].as_str().unwrap_or_default().to_string()
        } else {
            display["json"].as_str().unwrap_or_default().to_string()
        };
        if !shown.is_empty() {
            out.push(shown);
        }
        if let Some(notice) = display["notice"].as_str() {
            out.push(format!(
                "note: {notice} (pass max_rows, or narrow the query)"
            ));
        }
    } else {
        out.push(format!(
            "error ({elapsed:.1} ms): the script failed and was rolled back; the graph is \
             unchanged."
        ));
        let error = &execution["error"];
        let text = error["diagnostic"]
            .as_str()
            .or_else(|| error["message"].as_str())
            .map(str::to_string)
            .unwrap_or_else(|| error.to_string());
        out.push(text);
    }
    if profile && execution["profile"].is_object() {
        out.push(profile_text(&execution["profile"]));
    }
    (ok, out.join("\n"))
}

/// The per-step metrics as a compact text table (the layout of the CLI's "Traversal Metrics").
fn profile_text(profile: &Value) -> String {
    fn walk(list: &Value, depth: usize, out: &mut Vec<String>) {
        for m in list.as_array().into_iter().flatten() {
            let mut name = format!(
                "{}{}",
                "  ".repeat(depth),
                m["name"].as_str().unwrap_or("?")
            );
            if let Some(path) = m["path_recording"].as_str() {
                name.push_str(&format!(" [path: {path}]"));
            }
            out.push(format!(
                "{:<8} {name}  in {} out {} {:.3} ms {:.1}%",
                m["id"].as_str().unwrap_or(""),
                m["count_in"],
                m["counts"]["traverser_count"],
                m["duration_ns"].as_f64().unwrap_or(0.0) / 1e6,
                m["percent_duration"].as_f64().unwrap_or(0.0),
            ));
            walk(&m["nested"], depth + 1, out);
        }
    }
    let mut out = vec!["Traversal metrics (step id, step, traversers in/out, time):".to_string()];
    walk(&profile["metrics"], 0, &mut out);
    out.push(format!(
        "total {:.3} ms",
        profile["duration_ns"].as_f64().unwrap_or(0.0) / 1e6
    ));
    if let Some(rules) = profile["optimizer_rules_applied"].as_array()
        && !rules.is_empty()
    {
        let names: Vec<&str> = rules.iter().filter_map(Value::as_str).collect();
        out.push(format!("Optimizer rules applied: {}", names.join(", ")));
    }
    out.join("\n")
}

struct Resource {
    uri: &'static str,
    name: &'static str,
    title: &'static str,
    description: &'static str,
    mime: &'static str,
}

const RESOURCES: [Resource; 5] = [
    Resource {
        uri: "graphersal://schema",
        name: "schema",
        title: "Stored schema",
        description: "The schema stored in the graph (graphersal-schema-v2 JSON), or null.",
        mime: "application/json",
    },
    Resource {
        uri: "graphersal://statistics",
        name: "statistics",
        title: "Statistics",
        description: "Counts per label, memory footprint and query limits of the graph.",
        mime: "application/json",
    },
    Resource {
        uri: "graphersal://dsl-reference",
        name: "dsl-reference",
        title: "DSL reference",
        description: "Every step and function of the query language with its signatures, a \
                      one-line description and a runnable example with its result, generated \
                      from the engine (about 55 KB).",
        mime: "text/markdown",
    },
    Resource {
        uri: "graphersal://examples",
        name: "examples",
        title: "Examples",
        description: "The playground's runnable example queries (about 8 KB).",
        mime: "text/markdown",
    },
    Resource {
        uri: "graphersal://saved-queries",
        name: "saved-queries",
        title: "Saved queries",
        description: "The saved queries stored in the graph (name, folder, description, \
                      parameters, status), as list_saved_queries returns them: check them \
                      before writing a new query.",
        mime: "application/json",
    },
];

/// The text of the resource `uri`; `read_only` leaves the saved queries that write out of
/// `graphersal://saved-queries`.
fn read_resource(context: &Context, uri: &str, read_only: bool) -> Result<String, String> {
    match uri {
        "graphersal://schema" => context.call(|s| s.get_schema_text()).map_err(http_failed),
        "graphersal://saved-queries" => Ok(pretty(&Value::Array(saved_queries(
            context, None, None, read_only,
        )?))),
        "graphersal://statistics" => Ok(pretty(
            &context.call(|s| s.statistics()).map_err(http_failed)?,
        )),
        "graphersal://dsl-reference" => {
            static REFERENCE: OnceLock<String> = OnceLock::new();
            if let Some(text) = REFERENCE.get() {
                return Ok(text.clone());
            }
            let completions = context
                .call(|s| graphersal_session::views::completions(s.engine()))
                .map_err(http_failed)?;
            Ok(REFERENCE
                .get_or_init(|| dsl_reference(&completions))
                .clone())
        }
        "graphersal://examples" => {
            let (bytes, _) = context
                .ui
                .read("js/examples.json")
                .ok_or_else(|| "the UI has no js/examples.json".to_string())?;
            let examples: Value = serde_json::from_slice(&bytes)
                .map_err(|err| format!("js/examples.json is not JSON: {err}"))?;
            Ok(examples_text(&examples))
        }
        other => Err(format!("resource not found: {other}")),
    }
}

/// The DSL reference: syntax rules, then one line per function of every receiver.
fn dsl_reference(completions: &Value) -> String {
    let mut out = String::from(
        "# Graphersal DSL reference\n\n\
         Gremlin (Apache TinkerPop) in Rhai syntax. `g` is the traversal source; `__.` starts an \
         anonymous child traversal, always explicit (`__.out(\"knows\")`). Names have a \
         snake_case and a camelCase spelling (`has_label`, `hasLabel`): the steps list their \
         aliases in brackets, other sections only irregular ones. Tokens: `T.id`, \
         `T.label`, `P.eq/neq/lt/lte/gt/gte/inside/outside/between/within/without(..)`, \
         `TextP.containing/startingWith/endingWith(..)`, `Order.asc/desc/shuffle`, \
         `Column.keys/values`, `Scope.local/global`, `Pop.first/last/all/mixed`, \
         `Cardinality.single/list/set`, `Direction.OUT/IN/BOTH`, `GType.LONG/DOUBLE/STRING/..`, \
         `Operator.sum/..`, `DT.second/minute/hour/day/week/month/year/..`. Literals: strings in \
         double quotes, lists `[1, 2]`, maps `#{name: \"x\"}`, `jpath(\"a.b[0]\")` for a nested \
         property key, `datetime(\"2023-08-02T09:30Z\")` and the other date constructors. A \
         traversal without a terminal returns all its results; terminals: `to_list()`, \
         `next()`, `iterate()` (side effects only, returns nothing), `to_graph()`, `profile()`, \
         `execute()` and the visualizers (`to_table()`, `to_json()`, ...). Below: overloads by \
         argument types (`(any…)`: up to 10), an example on the modern graph (a final \
         `to_list()` left out), `→` its result.\n",
    );
    // receiver -> (lines, entries written as another one: (`P.x()`, `gt`) or ("", `a` = `b`))
    #[allow(clippy::type_complexity)]
    let mut sections: Vec<(String, Vec<String>, Vec<(String, String)>)> = Vec::new();
    let mut first_with_doc: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for entry in completions.as_array().into_iter().flatten() {
        let receiver = entry["receiver"].as_str().unwrap_or("other").to_string();
        let name = entry["name"].as_str().unwrap_or("");
        let signatures: Vec<&str> = entry["signatures"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let aliases: Vec<&str> = entry["aliases"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let doc = compact_doc(&doc_line(entry["doc"].as_str().unwrap_or("")));
        let section = match sections.iter().position(|(r, ..)| *r == receiver) {
            Some(at) => at,
            None => {
                sections.push((receiver.clone(), Vec::new(), Vec::new()));
                sections.len() - 1
            }
        };
        // A text shared by several entries (`gt(30)` is `P.gt(30)`, `P.containing` is
        // `TextP.containing`) is written once; the later entries are listed as the first.
        if let Some(first) = first_with_doc.get(&doc) {
            let irregular = aliases.iter().any(|alias| *alias != camel_case(name));
            let names: Vec<&str> = std::iter::once(name).chain(aliases).collect();
            // `gt` = `P.gt()`: grouped under `P.x()`; another name is written out.
            let same_name = first
                .rsplit_once('.')
                .filter(|(_, called)| *called == format!("{name}()") && !irregular);
            sections[section].2.push(match same_name {
                Some((class, _)) => (format!("`{class}.x()`"), name.to_string()),
                None => (
                    String::new(),
                    format!("`{}` = `{first}`", names.join("`/`")),
                ),
            });
            continue;
        }
        let mut line = format!("- {name}{}", compact_signatures(&signatures));
        // Outside the steps a regular camelCase twin (`domain_name` -> `domainName`) goes
        // without saying (the header says so).
        let listed: Vec<&str> = aliases
            .iter()
            .copied()
            .filter(|alias| receiver == "traversal" || *alias != camel_case(name))
            .collect();
        if !listed.is_empty() {
            line.push_str(&format!(" [{}]", listed.join(", ")));
        }
        match first_with_doc.get(&doc) {
            Some(_) => {}
            None if !doc.is_empty() => {
                line.push_str(&format!(": {doc}"));
                let label = match receiver.as_str() {
                    "traversal" | "global" => format!("{name}()"),
                    other => format!("{other}.{name}()"),
                };
                first_with_doc.insert(doc, label);
            }
            None => {}
        }
        sections[section].1.push(line);
    }
    sections.sort_by_key(|(receiver, ..)| receiver != "traversal");
    for (receiver, lines, same) in sections {
        let title = match receiver.as_str() {
            "traversal" => "Steps and source methods (g.x(), __.x(), traversal.x())".to_string(),
            "global" => "Free functions (jpath(..), bare predicates such as gt(30))".to_string(),
            "GraphSource" => "Private graphs (GraphSource::x())".to_string(),
            "Predicate" => "Predicate values (p.x())".to_string(),
            "Uuid" => "UUID values (u.x())".to_string(),
            "Fake" => "Test data generator (let f = Fake(seed); f.x())".to_string(),
            "Map" => "Maps with non-string keys (m.x())".to_string(),
            "map" => "Schema maps (schema.x())".to_string(),
            "Execution" => "execute() results (r.x())".to_string(),
            "TraversalMetrics" => "profile() results (m.x())".to_string(),
            "temporal" => "Date and time values (d.x())".to_string(),
            other => format!("{other}.x()"),
        };
        out.push_str(&format!("\n## {title}\n\n"));
        if !lines.is_empty() {
            out.push_str(&format!("{}\n", lines.join("\n")));
        }
        let mut groups: Vec<(&str, Vec<&str>)> = Vec::new();
        for (class, text) in &same {
            match groups.iter_mut().find(|(c, _)| c == class) {
                Some((_, texts)) => texts.push(text),
                None => groups.push((class, vec![text])),
            }
        }
        for (class, texts) in groups {
            match class {
                "" => out.push_str(&format!("\nThe same functions: {}.\n", texts.join(", "))),
                class => out.push_str(&format!(
                    "\nThe same as {class} under the same name: {}.\n",
                    texts.join(", ")
                )),
            }
        }
    }
    out
}

/// The overloads of one function in the reference's compact form: argument types only (an
/// argument name is kept where the engine has a real one), a type list once, `|` between the
/// overloads, and a variadic function's overloads of any arguments (`(any)`, `(any, any)`,
/// `(array)`, `(arg1, …, arg10)`) as `(any…)`.
fn compact_signatures(signatures: &[&str]) -> String {
    let placeholder = |name: &str| {
        name == "_"
            || name
                .strip_prefix("arg")
                .is_some_and(|n| n.parse::<u32>().is_ok())
    };
    let mut seen: Vec<Vec<&str>> = Vec::new();
    let mut out: Vec<String> = Vec::new();
    let mut variadic = false;
    for signature in signatures {
        let inner = signature.strip_prefix('(').unwrap_or(signature);
        let inner = inner.strip_suffix(')').unwrap_or(inner);
        let params: Vec<(&str, &str)> = match inner.strip_prefix("arg1, …, arg") {
            Some("10") => {
                variadic = true;
                continue;
            }
            Some(max) => vec![("_", "any"); max.parse().unwrap_or(1)],
            None => inner
                .split(", ")
                .filter(|p| !p.is_empty())
                .map(|p| p.split_once(": ").unwrap_or((p, "any")))
                .collect(),
        };
        let types: Vec<&str> = params.iter().map(|(_, ty)| *ty).collect();
        if seen.contains(&types) {
            continue;
        }
        seen.push(types);
        let text: Vec<String> = params
            .iter()
            .map(|(name, ty)| {
                if placeholder(name) {
                    (*ty).to_string()
                } else {
                    format!("{name}: {ty}")
                }
            })
            .collect();
        out.push(format!("({})", text.join(", ")));
    }
    if variadic {
        // `(any…)` stands for the overloads of one to ten arguments of any type.
        out.retain(|s| {
            s == "()"
                || !s[1..s.len() - 1]
                    .split(", ")
                    .all(|ty| ty == "any" || ty == "array")
        });
        out.push("(any…)".to_string());
    }
    out.join("|")
}

/// The regular camelCase spelling of a snake_case name (`domain_name` -> `domainName`).
fn camel_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut upper = false;
    for c in name.chars() {
        if c == '_' {
            upper = true;
        } else if upper {
            out.push(c.to_ascii_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// A [`doc_line`] made shorter for the reference: `→` between an example and its result (a
/// final `to_list()` left out: a traversal without a terminal returns all its results), and
/// without the pointers to book pages, which a client of the reference cannot open.
fn compact_doc(line: &str) -> String {
    const POINTER: &str = "see the book page";
    const SUMMARY_CHARS: usize = 150;
    let mut text = line
        .replace(".to_list()` returns `", "` → `")
        .replace("` returns `", "` → `");
    let (summary, example) = match text.find(" Example: `") {
        Some(at) => text.split_at(at),
        None => (text.as_str(), ""),
    };
    if summary.chars().count() > SUMMARY_CHARS {
        let cut: String = summary.chars().take(SUMMARY_CHARS - 1).collect();
        text = format!("{}…{example}", cut.trim_end_matches('…').trim_end());
    }
    while let Some(at) = text.find(POINTER) {
        let before = &text[..at];
        // `(see the book page X)`: the whole parenthesis; `..., see the book page X`: up to the
        // parenthesis or the sentence it ends.
        let own_parenthesis = before.ends_with('(');
        let start = if own_parenthesis {
            before.trim_end_matches('(').trim_end().len()
        } else {
            let kept = before.trim_end();
            let kept = kept
                .strip_suffix(',')
                .or_else(|| kept.strip_suffix(';'))
                .unwrap_or(kept);
            kept.len()
        };
        let tail = at + POINTER.len();
        let mut depth = 0usize;
        let mut end = text.len();
        for (i, c) in text[tail..].char_indices() {
            let here = tail + i;
            match c {
                '(' => depth += 1,
                ')' if depth > 0 => depth -= 1,
                ')' => {
                    end = if own_parenthesis { here + 1 } else { here };
                    break;
                }
                '.' if depth == 0 && text[here + 1..].starts_with(' ') => {
                    end = here;
                    break;
                }
                // A summary cut short (`doc_line`) ends the pointer too.
                '…' => {
                    end = here;
                    break;
                }
                _ => {}
            }
        }
        text.replace_range(start..end, "");
    }
    text
}

/// A function's doc text (the engine's doc table: one sentence, an empty line, `Example: ...`)
/// on one line, each part at most 240 characters.
fn doc_line(doc: &str) -> String {
    let flat = |text: &str| {
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if text.chars().count() > 240 {
            format!("{}…", text.chars().take(239).collect::<String>())
        } else {
            text
        }
    };
    let (summary, example) = doc.split_once("\n\n").unwrap_or((doc, ""));
    let mut line = flat(summary);
    let example = flat(example);
    if !example.is_empty() {
        line.push(' ');
        line.push_str(&example);
    }
    line
}

/// The playground's examples as Markdown, grouped like the start page.
fn examples_text(examples: &Value) -> String {
    let mut out = String::from(
        "# Graphersal examples\n\nRunnable with the `query` tool. Each group names the sample \
         graph it was written for; the server serves one graph, so adapt labels and keys to it \
         (get_schema / statistics).\n",
    );
    let mut group = String::new();
    for example in examples.as_array().into_iter().flatten() {
        let this = example["group"].as_str().unwrap_or("Examples");
        if this != group {
            group = this.to_string();
            let graph = example["graph"].as_str().unwrap_or("");
            out.push_str(&format!("\n## {group} (graph: {graph})\n"));
        }
        out.push_str(&format!(
            "\n{}:\n```\n{}\n```\n",
            example["title"].as_str().unwrap_or(""),
            example["script"].as_str().unwrap_or("")
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gql_is_a_reading_tool_offered_read_only() {
        for read_only in [false, true] {
            let tool = tools(read_only)
                .into_iter()
                .find(|t| t.name == "gql")
                .expect("gql listed");
            let listing = tool.listing(read_only);
            assert_eq!(listing["annotations"]["readOnlyHint"], true, "{listing}");
            assert_eq!(
                listing["annotations"]["destructiveHint"], false,
                "{listing}"
            );
            assert_eq!(listing["inputSchema"]["required"], json!(["query"]));
            for key in ["query", "params", "profile", "max_rows"] {
                assert!(
                    listing["inputSchema"]["properties"][key].is_object(),
                    "{key}"
                );
            }
        }
        for named in [
            "OPTIONAL MATCH",
            "RETURN *",
            "graphersal://examples",
            "\"GQL\"",
        ] {
            assert!(GQL_DESCRIPTION.contains(named), "{named}");
        }
        assert!(INSTRUCTIONS.contains("`gql`"));
    }

    /// The gql tool's description lists the stage 1b, 1c, 2a, 2b, 2c and 2d subset as supported and
    /// still names what is refused; it stays a read-only tool.
    #[test]
    fn gql_description_lists_stage_1b_and_what_is_still_refused() {
        let (supported, refused) = GQL_DESCRIPTION
            .split_once("Not yet supported")
            .expect("a not-yet list");
        for named in [
            "OPTIONAL MATCH",
            "WITH",
            "LET",
            "FILTER",
            "collect_list",
            "count(*)",
            "DISTINCT",
            "->{1,3}",
            "-[e*1..3]->",
            "p = (a)-->(b)",
            "path_length(p)",
            "ORDER BY",
            "GROUP BY",
            "nodes(p)",
            "relationships(p)",
            "CAST(",
            "toInteger",
            "OrNull",
            "substring",
            "round(",
            "keys",
            "properties",
            "startNode(e)",
            "FOR x IN list",
            "UNWIND",
            "WITH ORDINALITY",
            "$ids",
            "may start with",
            "EXISTS { MATCH",
            "EXISTS { <pattern> }",
            "pattern predicates",
            "never NULL",
            "NOT EXISTS { (p)-[:created]->() }",
            "map projections p {.name",
            "list comprehensions [x IN list WHERE",
            "pattern comprehensions [(p)-[:knows]->(f)",
            "JSON-shaped answers",
            "RETURN p {.name, friends: [(p)-[:knows]->(f) | f.name]}",
            "all(x IN list WHERE cond)",
            "reduce(acc = 0, x IN list | acc + x)",
            "COUNT { <pattern> }",
            "COLLECT { MATCH ... RETURN x }",
            "q1 UNION q2",
            "UNION ALL",
            "q1 NEXT q2",
            "CALL (p) { MATCH ... RETURN ... }",
            "OPTIONAL CALL",
            "top k per group",
            "CALL (p) { MATCH (p)-[e:created]->(s) RETURN s.name AS top ORDER BY e.weight DESC LIMIT 1 }",
        ] {
            assert!(supported.contains(named), "{named}");
        }
        for named in [
            "((a)-->(b)){1,3}",
            "EXCEPT and INTERSECT",
            "IN TRANSACTIONS",
            "randomUUID",
            "INSERT",
            "use the query tool",
        ] {
            assert!(refused.contains(named), "{named}");
        }
        for gone in [
            "without MATCH",
            "UNWIND",
            "EXISTS",
            "comprehension",
            "projection",
            "COUNT { }",
            "COLLECT",
            "UNION",
            "NEXT",
            "reduce()",
        ] {
            assert!(!refused.contains(gone), "{gone}");
        }
        assert!(GQL_DESCRIPTION.contains("never changes"));
    }

    #[test]
    fn secrets_targets_and_sentences() {
        assert!(same_secret("abc", "abc"));
        assert!(!same_secret("abd", "abc"));
        assert!(!same_secret("ab", "abc"));
        assert_eq!(view_note(&Value::Null), None);
        let note = view_note(&json!({"commitSeq": 3, "target": "commit 3"})).unwrap();
        assert!(note.starts_with("the served state is a read-only view at commit 3, opened"));
        let note = view_note(&json!({"commitSeq": 3, "target": "mark \"m1\""})).unwrap();
        assert!(note.starts_with("the served state is a read-only view at commit 3 (mark \"m1\")"));
        assert_eq!(
            doc_line("Filters by label.\n\nExample: `g.v().has_label(\"x\")`  returns `[]`"),
            "Filters by label. Example: `g.v().has_label(\"x\")` returns `[]`"
        );
        assert_eq!(doc_line("Only a sentence."), "Only a sentence.");
        assert_eq!(
            json_arg(
                &json!({"schema": "{\"vertices\": {}}"}),
                "schema",
                Some("open")
            )
            .unwrap(),
            r#"{"mode":"open","vertices":{}}"#
        );
        assert!(json_arg(&json!({}), "schema", None).is_err());
        assert!(cut("x".repeat(MAX_TEXT_CHARS + 5)).contains("cut at"));
    }

    #[test]
    fn the_reference_is_compact() {
        assert_eq!(
            compact_doc(
                "Pushes (see the book page Bulk and Barriers). Converts (zero, see the book \
                 page Type Conversion and length()). Ends, see the book page X. Example: `a` \
                 returns `b`"
            ),
            "Pushes. Converts (zero). Ends. Example: `a` → `b`"
        );
        assert_eq!(
            compact_doc("X. Example: `g.v().to_list()` returns `[]`"),
            "X. Example: `g.v()` → `[]`"
        );
        assert_eq!(
            compact_doc("Sets (see the book page Prop… Example: `x` returns `y`"),
            "Sets… Example: `x` → `y`"
        );
        assert_eq!(
            compact_doc(&format!("{}. Example: `x` returns `y`", "a".repeat(200))),
            format!("{}… Example: `x` → `y`", "a".repeat(149))
        );
        assert_eq!(camel_case("domain_name"), "domainName");
        assert_eq!(
            compact_signatures(&["(arg1: map)", "(arg1, …, arg3)"]),
            "(map)|(any, any, any)"
        );
        assert_eq!(
            compact_signatures(&[
                "()",
                "(arg1: any)",
                "(val: any)",
                "(arg1, arg2)",
                "(arg1, …, arg10)"
            ]),
            "()|(any…)"
        );
        assert_eq!(
            compact_signatures(&["(pattern: string)"]),
            "(pattern: string)"
        );
    }

    #[test]
    fn read_only_lists_only_reading_tools() {
        let names = |read_only: bool| -> Vec<&'static str> {
            all_tools()
                .into_iter()
                .filter(|t| !read_only || t.kind == Kind::Read || t.name == "query")
                .map(|t| t.name)
                .collect()
        };
        assert!(names(false).contains(&"set_schema"));
        assert!(!names(true).contains(&"set_schema"));
        assert!(!names(true).contains(&"patch_schema"));
        assert!(names(true).contains(&"query"));
        assert!(names(true).contains(&"list_compressions"));
        assert!(!names(true).contains(&"define_compression"));
        assert!(!names(true).contains(&"drop_compression"));
        assert!(!names(true).contains(&"recompress"));
        assert!(names(false).contains(&"recompress"));
        let query = all_tools().into_iter().find(|t| t.name == "query").unwrap();
        assert_eq!(query.listing(true)["annotations"]["readOnlyHint"], true);
        assert_eq!(query.listing(false)["annotations"]["readOnlyHint"], false);
    }
}
