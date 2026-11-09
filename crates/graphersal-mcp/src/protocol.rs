//! The protocol: JSON-RPC over MCP Streamable HTTP (initialize-based revisions, plain JSON
//! answers), `initialize`, `ping`, `tools/*`, `resources/*`, `prompts/list`, and the server
//! instructions.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, Weak};
use std::time::Instant;

use serde_json::{Value, json};

use crate::catalog::is_built_in;
use crate::config::{Instructions, McpConfig, SavedQueryTools};
use crate::connection::Stream;
use crate::host::{ClientRef, Ctx, Host, McpError, clear_busy, take_busy};
use crate::profile::Profile;
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
pub(crate) const PARSE_ERROR: i64 = -32700;
pub(crate) const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
pub(crate) const INTERNAL_ERROR: i64 = -32603;

/// The client name of the endpoint's session jobs (the host's change feed records it).
pub(crate) const MCP_CLIENT: &str = "mcp";

/// The MCP endpoint: its options and its MCP sessions. One per server; [`Mcp::handle`] answers
/// every request to it.
pub struct Mcp {
    token: Option<String>,
    /// The default profile: the operator's configuration (tools, descriptions, instructions,
    /// saved-query tools, resources, answer limit, with the shorthand flags folded in), the
    /// read-only flag and the access policy. A connection or a request may get another from
    /// the host ([`Connection::with_profile`](crate::Connection::with_profile),
    /// [`Mcp::handle_with_profile`]).
    profile: Profile,
    /// The MCP sessions: a bounded table (the default) or stateless signed ids
    /// ([`Mcp::with_session_key`]).
    sessions: Sessions,
    /// The usage statistics (memory only, since the endpoint was built).
    stats: Arc<McpStats>,
    /// The `clientInfo` of the last `initialize` (the playground's top bar names it).
    client: Mutex<Option<Value>>,
    /// The open stream connections ([`crate::Connection`]), which hear the server's pushes.
    /// Weak: a dropped connection leaves by itself.
    streams: Mutex<Vec<Weak<Stream>>>,
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
            token,
            profile: Profile::new(read_only).with_config(config),
            stats: Arc::new(McpStats::new(sessions.clients())),
            sessions,
            client: Mutex::new(None),
            streams: Mutex::new(Vec::new()),
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
        self.profile.config = Arc::new(config);
        self
    }

    /// Serves `profile` by default: to every HTTP request ([`Mcp::handle`]) and every
    /// connection built afterwards ([`crate::Connection::new`]) unless the host gives it
    /// another. It replaces the read-only flag of [`Mcp::new`] and the whole configuration,
    /// including what `query_tools`, [`Mcp::with_config`], [`Mcp::with_operator_notes`] and
    /// [`Mcp::with_max_chars`] set before.
    pub fn with_profile(mut self, profile: Profile) -> Self {
        self.profile = profile;
        self
    }

    /// The default profile ([`Mcp::with_profile`]).
    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    /// Adds the operator's notes to the `initialize` instructions (under the heading "Notes
    /// from the operator of this server about this graph:"). The AI Chat prompt does not carry
    /// them: it has its own text.
    pub fn with_operator_notes(mut self, notes: Option<String>) -> Self {
        Arc::make_mut(&mut self.profile.config).instructions = match notes {
            Some(notes) => Instructions::Append(notes),
            None => Instructions::Builtin,
        };
        self
    }

    /// Sets the largest tool answer in characters (`None` keeps [`MAX_TEXT_CHARS`]).
    pub fn with_max_chars(mut self, max_chars: Option<usize>) -> Self {
        self.profile = self.profile.with_max_chars(max_chars);
        self
    }

    /// The client sessions (stateless ids: the client names kept) and their estimated bytes
    /// (ids and stored values as JSON text), for the host's memory report; `(0, 0)` while a
    /// request holds the list.
    pub fn memory_bytes(&self) -> (usize, u64) {
        let (count, bytes) = self.sessions.memory_bytes();
        (count, bytes + self.stats.bytes())
    }

    /// Whether the default profile offers only reading tools.
    pub fn read_only(&self) -> bool {
        self.profile.read_only
    }

    /// Whether saved queries are tools of their own in the default profile
    /// (`--mcp-query-tools` or the configuration's `saved_query_tools`).
    pub fn query_tools(&self) -> bool {
        self.profile.config.has_saved_query_tools()
    }

    /// The configuration of the default profile.
    pub fn config(&self) -> &McpConfig {
        &self.profile.config
    }

    /// The instructions `initialize` answers under the default profile.
    pub fn instructions_text(&self) -> String {
        instructions_of(&self.profile)
    }

    /// The caller of the endpoint's tools.
    fn caller(&self) -> ToolCaller {
        self.caller_for(ClientRef::new(MCP_CLIENT, None), &self.profile)
    }

    /// The caller of the endpoint's tools for `client` (the request's MCP session as its key)
    /// under `profile`.
    fn caller_for(&self, client: ClientRef, profile: &Profile) -> ToolCaller {
        ToolCaller::new(client, profile.read_only)
            .with_profile(profile)
            .with_stats(self.stats.clone())
    }

    /// The `tools/list` entries an agent gets now under the default profile (saved-query tools
    /// read through `host`).
    pub fn tool_listings(&self, host: &dyn Host) -> Result<Vec<Value>, McpError> {
        tool_listings(host, &self.caller())
    }

    /// What the endpoint's prompt costs an agent's context under the default profile: the
    /// instructions and every tool's `tools/list` entry in characters (`graphersal mcp-cost`,
    /// the budget tests).
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
    /// endpoint (`readOnly` of the default profile, `client` = the last `initialize`'s client).
    pub fn status(&self) -> Value {
        json!({
            "endpoint": "/mcp",
            "readOnly": self.profile.read_only,
            "client": self.client.lock().ok().and_then(|c| c.clone()),
        })
    }

    /// Answers one HTTP request to the endpoint (after the host's own checks, e.g. `Host` and
    /// `Origin`): `POST` carries one JSON-RPC message, `DELETE` ends an MCP session, every other
    /// method is `405`. Session jobs run through `host` for the client `mcp`, under the default
    /// profile.
    pub fn handle(&self, host: &dyn Host, request: McpRequest<'_>) -> McpResponse {
        self.handle_with_profile(host, &self.profile, request)
    }

    /// [`Mcp::handle`] under `profile`, which the host chose for this request (the graph its
    /// resolver picked, the identity its request filter attached; never anything the client
    /// says inside MCP). The MCP sessions, statistics and token stay the endpoint's.
    pub fn handle_with_profile(
        &self,
        host: &dyn Host,
        profile: &Profile,
        mut request: McpRequest<'_>,
    ) -> McpResponse {
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
            "POST" => self.post(host, profile, &mut request),
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
    fn post(
        &self,
        host: &dyn Host,
        profile: &Profile,
        request: &mut McpRequest<'_>,
    ) -> McpResponse {
        let answer = self.post_message(host, profile, request);
        match answer.status() {
            404 => self.stats.record_refusal(Refusal::UnknownSession),
            400..=499 => self.stats.record_refusal(Refusal::InvalidRequest),
            _ => {}
        }
        answer
    }

    fn post_message(
        &self,
        host: &dyn Host,
        profile: &Profile,
        request: &mut McpRequest<'_>,
    ) -> McpResponse {
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
        let identity = request.identity().map(str::to_string);
        let peer = Peer {
            host,
            profile,
            key: session_id.as_deref(),
            client_digest: session_id
                .as_deref()
                .and_then(|id| self.sessions.client_of(id)),
            identity: identity.as_deref(),
            stream: None,
        };
        let outcome = match parse_message(&body) {
            Ok(message) => self.message(&peer, &message),
            Err(outcome) => outcome,
        };
        match outcome {
            Outcome::Accepted => empty(202),
            Outcome::Answer { body, session } => {
                let answer = rpc_answer(200, &body);
                match session {
                    Some(session) => answer.with_header("Mcp-Session-Id", &session),
                    None => answer,
                }
            }
            // The host could not take a job now (queue full, waited too long, too many jobs of
            // this client, stopping): the whole request is `503`, so the client retries it.
            Outcome::Busy { id, message } => {
                rpc_error(503, id, INTERNAL_ERROR, &message).with_header("Retry-After", "1")
            }
            Outcome::Invalid { id, code, message } => rpc_error(400, id, code, &message),
        }
    }

    /// One parsed JSON-RPC message of `peer`, whatever carried it (an HTTP `POST` or a line of
    /// a stream connection): a request is dispatched, a notification or a response accepted.
    pub(crate) fn message(&self, peer: &Peer<'_>, message: &Value) -> Outcome {
        let Some(object) = message.as_object() else {
            return Outcome::Invalid {
                id: Value::Null,
                code: INVALID_REQUEST,
                message: "expected one JSON-RPC message object (batches are not supported)".into(),
            };
        };
        let id = object.get("id").cloned();
        let Some(method) = object.get("method").and_then(Value::as_str) else {
            // A response to a server request (this server sends none): accepted.
            return if object.contains_key("result") || object.contains_key("error") {
                Outcome::Accepted
            } else {
                Outcome::Invalid {
                    id: id.unwrap_or(Value::Null),
                    code: INVALID_REQUEST,
                    message: "a JSON-RPC message needs \"method\"".into(),
                }
            };
        };
        self.stats.record_request(
            id.is_some().then_some(method),
            peer.key,
            peer.client_digest,
            method == "tools/call" && id.is_some(),
        );
        let Some(id) = id else {
            // A notification (`notifications/initialized`, `notifications/cancelled`, ...).
            if method == "notifications/cancelled" {
                cancel_request(peer.host, peer.key, object.get("params"));
            }
            return Outcome::Accepted;
        };
        let params = object.get("params").cloned().unwrap_or(Value::Null);
        let client = peer.client().with_request(&id.to_string());
        clear_busy();
        let dispatched = self.dispatch(peer, &client, method, &params);
        if let Some(message) = take_busy() {
            self.stats.record_busy();
            return Outcome::Busy { id, message };
        }
        match dispatched {
            Ok((result, session)) => Outcome::Answer {
                body: json!({"jsonrpc": "2.0", "id": id, "result": result}),
                session,
            },
            Err((code, message)) => Outcome::Answer {
                body: error_body(id, code, &message),
                session: None,
            },
        }
    }

    fn dispatch(
        &self,
        peer: &Peer<'_>,
        client: &ClientRef,
        method: &str,
        params: &Value,
    ) -> RpcResult {
        let host = peer.host;
        let profile = peer.profile;
        let plain = |value: Value| Ok((value, None));
        let caller = self.caller_for(client.clone(), profile);
        match method {
            "initialize" => self.initialize(peer, params),
            "ping" => plain(json!({})),
            "tools/list" => {
                let listed = tool_listings(host, &caller)
                    .map_err(|err: McpError| (-32603, err.to_string()))?;
                let listed = json!({ "tools": listed });
                // What the client saw: a later change of it is pushed (stream only).
                if let Some(stream) = peer.stream {
                    stream.listed(fingerprint(&listed["tools"]));
                }
                plain(listed)
            }
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                let started = Instant::now();
                // A stream client that asked for progress (`_meta.progressToken`) hears that the
                // call still runs; over HTTP nothing is pushed.
                let token = params
                    .get("_meta")
                    .and_then(|meta| meta.get("progressToken"))
                    .filter(|token| token.is_string() || token.is_number());
                let called = match (peer.stream, token) {
                    (Some(stream), Some(token)) => {
                        stream.with_progress(token, || call_tool(host, &caller, name, &args))
                    }
                    _ => call_tool(host, &caller, name, &args),
                };
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
                // The call may have changed the saved queries the stream clients listed.
                // Best effort: a failed listing leaves the clients' lists as they were.
                let _ = self.check_listings(peer.stream);
                match called {
                    Ok(outcome) => plain(tool_result(outcome.text, outcome.is_error)),
                    Err(CallRefused::Internal(message)) => Err((INTERNAL_ERROR, message)),
                    Err(CallRefused::Params(message)) => {
                        let message = if profile.read_only && message.contains("(read-only") {
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
                .filter(|r| profile.config.lists_resource(r.name)).map(|r| json!({
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
                    read_resource(&context, uri, profile.read_only).map_err(|err| (-32603, err))?;
                plain(json!({ "contents": [{
                    "uri": uri, "mimeType": resource.mime, "text": text,
                }] }))
            }
            "prompts/list" => plain(json!({ "prompts": [] })),
            _ => Err((METHOD_NOT_FOUND, format!("method not found: {method}"))),
        }
    }

    /// `initialize`: negotiates the protocol version (the client's when this server speaks it,
    /// else the newest), remembers the client's name for the [`Mcp::status`] and, over HTTP,
    /// assigns a session id. A stream connection keeps the negotiated state itself and is told
    /// that the tool list may change (`tools.listChanged`) when it can: saved queries are tools
    /// of their own, and a stream carries the notification.
    fn initialize(&self, peer: &Peer<'_>, params: &Value) -> RpcResult {
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
        let session = match peer.stream {
            None => {
                let session = self.sessions.create(&client);
                self.stats.record_initialize(&session);
                Some(session)
            }
            Some(stream) => {
                let digest = self.sessions.remember_client(&client);
                self.stats.record_initialize(stream.key());
                let capabilities = params.get("capabilities").cloned().unwrap_or(json!({}));
                stream.initialized(version, client.clone(), capabilities, digest);
                None
            }
        };
        if let Ok(mut last) = self.client.lock() {
            *last = Some(client);
        }
        let instructions = instructions_of(peer.profile);
        let list_changed = peer.stream.is_some() && lists_change(peer.profile);
        Ok((
            json!({
                "protocolVersion": version,
                "capabilities": {
                    "tools": { "listChanged": list_changed },
                    "resources": { "listChanged": false },
                },
                "serverInfo": {
                    "name": peer.profile.config.server_name.as_deref().unwrap_or(SERVER_NAME),
                    "title": peer.profile.config.server_title.as_deref().unwrap_or(SERVER_TITLE),
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "instructions": instructions,
            }),
            session,
        ))
    }

    /// Registers a stream connection for the server's pushes.
    pub(crate) fn add_stream(&self, stream: Weak<Stream>) {
        if let Ok(mut streams) = self.streams.lock() {
            streams.retain(|known| known.strong_count() > 0);
            streams.push(stream);
        }
    }

    /// Tells the stream connections that listed the tools when their list changed (after a
    /// tool call of any client, or [`Mcp::check_tools_changed`]): `notifications/tools/list_changed`
    /// to each whose listing is no longer what it saw. `origin` (the stream that made the call)
    /// gets the notification after its answer instead. Nothing
    /// to do without stream connections or saved-query tools; over HTTP nothing is pushed.
    pub(crate) fn check_listings(&self, origin: Option<&Stream>) -> Result<(), McpError> {
        let streams: Vec<Arc<Stream>> = match self.streams.lock() {
            Ok(streams) => streams.iter().filter_map(Weak::upgrade).collect(),
            Err(_) => return Err(McpError::Failed("the stream registry is poisoned".into())),
        };
        let mut failure = Ok(());
        // One listing per host and profile (connections over the same host with equal
        // profiles see the same catalog).
        let mut seen: Vec<(*const (), Arc<Profile>, Option<u64>)> = Vec::new();
        for stream in streams
            .iter()
            .filter(|stream| lists_change(&stream.profile()) && stream.has_listed())
        {
            let host = stream.host();
            let address = Arc::as_ptr(host) as *const ();
            let profile = stream.profile();
            let now = match seen
                .iter()
                .find(|(known, by, _)| *known == address && **by == *profile)
            {
                Some((_, _, now)) => *now,
                None => {
                    let client = ClientRef::new(MCP_CLIENT, None).with_key(stream.key());
                    let caller = self.caller_for(client, &profile);
                    let now = match tool_listings(host.as_ref(), &caller) {
                        Ok(listed) => Some(fingerprint(&Value::Array(listed))),
                        Err(err) => {
                            if failure.is_ok() {
                                failure = Err(err);
                            }
                            None
                        }
                    };
                    seen.push((address, profile, now));
                    now
                }
            };
            if let Some(now) = now
                && stream.list_changed(now)
            {
                if origin.is_some_and(|origin| std::ptr::eq(origin, stream.as_ref())) {
                    stream.mark_list_changed();
                } else {
                    stream.push(list_changed_notice());
                }
            }
        }
        failure
    }

    /// Tells the stream connections that listed the tools when the list changed: a host calls
    /// this after it changed the saved queries itself (its own UI, an import), since the
    /// endpoint checks only after the tool calls it serves. A job per host runs when saved
    /// queries are tools of their own and a stream client has listed the tools; otherwise it
    /// does nothing. HTTP clients are never notified (no stream). `Err` when a host could not
    /// list the tools (the other connections are checked all the same).
    pub fn check_tools_changed(&self) -> Result<(), McpError> {
        self.check_listings(None)
    }
}

/// Who sent a message: the host its jobs run on, the profile the host chose for it and how the
/// endpoint tells its clients apart.
pub(crate) struct Peer<'a> {
    pub(crate) host: &'a dyn Host,
    /// What the sender may do.
    pub(crate) profile: &'a Profile,
    /// The MCP session id (HTTP) or the connection's key (stream), if any.
    pub(crate) key: Option<&'a str>,
    /// The client digest of the session, for the statistics.
    pub(crate) client_digest: Option<u64>,
    /// The identity the host attached to the request or connection, if any.
    pub(crate) identity: Option<&'a str>,
    /// The stream connection the message came on (none over HTTP).
    pub(crate) stream: Option<&'a Stream>,
}

impl Peer<'_> {
    /// The client of the sender's jobs: `mcp` with its key and identity.
    pub(crate) fn client(&self) -> ClientRef {
        let client = ClientRef::new(MCP_CLIENT, None);
        let client = match self.key {
            Some(key) => client.with_key(key),
            None => client,
        };
        match self.identity {
            Some(identity) => client.with_identity(identity),
            None => client,
        }
    }
}

/// Whether the tool list of `profile` can change while a client is connected: saved queries
/// are tools of their own (the rest of the list is fixed by the configuration).
fn lists_change(profile: &Profile) -> bool {
    profile.config.has_saved_query_tools()
}

/// The instructions `initialize` answers under `profile`.
fn instructions_of(profile: &Profile) -> String {
    let mut instructions = match &profile.config.instructions {
        Instructions::Replace(text) => return text.clone(),
        Instructions::Builtin | Instructions::Append(_) => INSTRUCTIONS.to_string(),
    };
    if profile.read_only {
        instructions.push_str(READ_ONLY_NOTE);
    }
    if let Instructions::Append(notes) = &profile.config.instructions {
        instructions.push_str("\n\n");
        instructions.push_str(OPERATOR_HEADING);
        instructions.push_str("\n\n");
        instructions.push_str(notes);
    }
    instructions
}

/// What a message comes to, before a transport frames it.
pub(crate) enum Outcome {
    /// A notification or a response: nothing to answer (HTTP `202`).
    Accepted,
    /// The JSON-RPC answer (a result or an error) and, for an HTTP `initialize`, the new
    /// session id.
    Answer {
        body: Value,
        session: Option<String>,
    },
    /// The host could not take a job (HTTP `503` + `Retry-After`).
    Busy { id: Value, message: String },
    /// Not a JSON-RPC message this endpoint takes (HTTP `400`).
    Invalid {
        id: Value,
        code: i64,
        message: String,
    },
}

impl Outcome {
    /// The JSON-RPC message of the outcome, if it has one (a stream writes it as one line).
    pub(crate) fn into_message(self) -> Option<Value> {
        match self {
            Self::Accepted => None,
            Self::Answer { body, .. } => Some(body),
            Self::Busy { id, message } => Some(error_body(id, INTERNAL_ERROR, &message)),
            Self::Invalid { id, code, message } => Some(error_body(id, code, &message)),
        }
    }
}

/// Parses one message's text: `Err` is the parse error's outcome.
pub(crate) fn parse_message(text: &str) -> Result<Value, Outcome> {
    serde_json::from_str(text).map_err(|err| Outcome::Invalid {
        id: Value::Null,
        code: PARSE_ERROR,
        message: format!("malformed JSON: {err}"),
    })
}

/// The JSON-RPC error message.
pub(crate) fn error_body(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// `notifications/tools/list_changed`.
pub(crate) fn list_changed_notice() -> Value {
    json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"})
}

/// A digest of a tool listing (what a client saw), to tell a changed one.
fn fingerprint(listed: &Value) -> u64 {
    let mut hasher = DefaultHasher::new();
    listed.to_string().hash(&mut hasher);
    hasher.finish()
}

/// A JSON-RPC error answer with an HTTP status (`id` null when the request had none).
fn rpc_error(status: u16, id: Value, code: i64, message: &str) -> McpResponse {
    rpc_answer(status, &error_body(id, code, message))
}

fn rpc_answer(status: u16, body: &Value) -> McpResponse {
    McpResponse::new(status, body.to_string().into_bytes())
        .with_header("Content-Type", "application/json")
        .with_header("Cache-Control", "no-store")
}

/// `notifications/cancelled` `{requestId, reason?}`: cancels the runs of that request of the
/// same MCP session. JSON-RPC ids are chosen by each client, so without a session id there is
/// no telling whose request it names: ignored (cancellation is best effort in MCP, as is a
/// notice for a request that has already ended).
fn cancel_request(host: &dyn Host, session_id: Option<&str>, params: Option<&Value>) {
    let (Some(session_id), Some(request)) = (
        session_id,
        params.and_then(|params| params.get("requestId")),
    ) else {
        return;
    };
    let client = ClientRef::new(MCP_CLIENT, None).with_key(session_id);
    host.cancel_request(&client, &request.to_string());
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
