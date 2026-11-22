//! Directory mode: ONE endpoint over many graphs ([`GraphDirectory`], [`Mcp::handle_directory`]).
//! The host lists its graphs, resolves a name (an alias now) to the route of one and answers the
//! [`Host`] and [`Profile`] of a route; inside an MCP session the agent lists the graphs
//! (`list_graphs`), selects one (`use_graph`, kept in the session's table entry) and may switch,
//! or names another graph for one call (the `graph` argument of every data tool).

use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::{Map, Value, json};

use crate::answer::fit;
use crate::catalog::is_built_in;
use crate::host::{ClientRef, Ctx, Host, McpError};
use crate::profile::Profile;
use crate::protocol::{
    INTERNAL_ERROR, INVALID_PARAMS, MCP_CLIENT, METHOD_NOT_FOUND, Mcp, RpcResult, frame,
    initialize_result, instructions_of, method_not_allowed, negotiate, parse_message, rpc_error,
};
use crate::resources::{RESOURCES, gql_reference_text, read_resource};
use crate::tools::{CallRefused, ToolOutcome, call_tool, tool_result};
use crate::transport::{McpRequest, McpResponse};

/// The tool that lists the graphs of a directory endpoint.
const LIST_GRAPHS: &str = "list_graphs";
/// The tool that selects the graph of the MCP session.
const USE_GRAPH: &str = "use_graph";
/// The argument of every data tool (and of `resources/read`) that names the graph of one call.
const GRAPH_ARG: &str = "graph";

/// The graphs a directory endpoint serves ([`Mcp::handle_directory`]), implemented by the host
/// over its registry. EXPERIMENTAL like the crate.
///
/// Every method answers from the host's own state and may be called for every request: keep
/// them cheap (no session job). A refusal is an [`McpError`] whose message the agent reads as
/// the tool error, so it says what to do (`database 'x' is not loaded; ask an operator to load
/// it`).
pub trait GraphDirectory: Send + Sync {
    /// The graphs (and aliases) an agent may name, with their states (`list_graphs`).
    fn list(&self) -> Result<Vec<GraphEntry>, McpError>;

    /// The route of the graph `name` names (a graph or an alias): the real graph's name,
    /// resolved NOW. The endpoint stores this route as the session's selection (`use_graph`),
    /// so an alias that moves later does not move an open session. `Err` for an unknown name.
    fn resolve(&self, name: &str) -> Result<String, McpError>;

    /// The host serving the graph of `route` (a route [`GraphDirectory::resolve`] answered)
    /// and the profile its calls run under (its own read-only flag, access policy, answer
    /// limit). `Err` refuses the call, e.g. a graph that is not loaded (`database 'x' is not
    /// loaded; ask an operator to load it`) or no longer exists.
    fn host(&self, route: &str) -> Result<(Arc<dyn Host>, Profile), McpError>;
}

/// What kind of graph a [`GraphEntry`] is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum GraphKind {
    /// A graph of its own (the default).
    #[default]
    Plain,
    /// A layer over a frozen base graph.
    Layer,
    /// A frozen base graph that layers are built over.
    Base,
}

impl GraphKind {
    /// The kind's name in `list_graphs`: `plain`, `layer`, `base`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Plain => "plain",
            Self::Layer => "layer",
            Self::Base => "base",
        }
    }
}

/// The state of a graph of a [`GraphEntry`]: only a loaded graph serves calls (the host's
/// [`GraphDirectory::host`] decides).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum GraphState {
    /// Known but not in memory.
    Unloaded,
    /// Being loaded.
    Loading,
    /// In memory, serving.
    Loaded,
    /// Cannot load until an embedding model it needs is added.
    NeedsModel,
    /// Failed to load or to open; the text says why.
    Failed(String),
}

impl GraphState {
    /// The state's name in `list_graphs`: `unloaded`, `loading`, `loaded`, `needs-model`,
    /// `failed`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unloaded => "unloaded",
            Self::Loading => "loading",
            Self::Loaded => "loaded",
            Self::NeedsModel => "needs-model",
            Self::Failed(_) => "failed",
        }
    }

    /// Why a graph failed, if it did.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Failed(reason) => Some(reason),
            Self::Unloaded | Self::Loading | Self::Loaded | Self::NeedsModel => None,
        }
    }
}

/// One graph (or alias) of a [`GraphDirectory`], as `list_graphs` shows it.
///
/// ```
/// use graphersal_mcp::{GraphEntry, GraphKind, GraphState};
///
/// let entry = GraphEntry::new("acme", GraphState::Loaded)
///     .with_kind(GraphKind::Layer)
///     .with_description("the ACME project over the 2026.3 product graph");
/// let alias = GraphEntry::new("prod", GraphState::Loaded).with_alias_of("acme");
/// assert_eq!(alias.alias_of(), Some("acme"));
/// assert_eq!(entry.kind(), GraphKind::Layer);
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct GraphEntry {
    name: String,
    alias_of: Option<String>,
    kind: GraphKind,
    state: GraphState,
    read_only: bool,
    description: Option<String>,
}

impl GraphEntry {
    /// A plain, writable graph `name` in `state`, without alias or description.
    pub fn new(name: &str, state: GraphState) -> Self {
        Self {
            name: name.to_string(),
            alias_of: None,
            kind: GraphKind::Plain,
            state,
            read_only: false,
            description: None,
        }
    }

    /// The entry is an alias of the graph `target` (its state is the target's).
    pub fn with_alias_of(mut self, target: &str) -> Self {
        self.alias_of = Some(target.to_string());
        self
    }

    /// The kind of graph.
    pub fn with_kind(mut self, kind: GraphKind) -> Self {
        self.kind = kind;
        self
    }

    /// Whether the graph refuses writes.
    pub fn with_read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    /// A short description for the agent.
    pub fn with_description(mut self, description: &str) -> Self {
        self.description = Some(description.to_string());
        self
    }

    /// The name an agent passes to `use_graph` or `graph`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The graph this alias names, if it is one.
    pub fn alias_of(&self) -> Option<&str> {
        self.alias_of.as_deref()
    }

    /// The kind of graph.
    pub fn kind(&self) -> GraphKind {
        self.kind
    }

    /// The state.
    pub fn state(&self) -> &GraphState {
        &self.state
    }

    /// Whether the graph refuses writes.
    pub fn read_only(&self) -> bool {
        self.read_only
    }

    /// The description, if any.
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// The entry as `list_graphs` answers it (keys in a fixed order).
    fn to_text(&self) -> String {
        let mut pairs = vec![("name", json!(self.name))];
        if let Some(target) = &self.alias_of {
            pairs.push(("aliasOf", json!(target)));
        }
        pairs.push(("kind", json!(self.kind.as_str())));
        pairs.push(("state", json!(self.state.as_str())));
        if let Some(reason) = self.state.reason() {
            pairs.push(("reason", json!(reason)));
        }
        pairs.push(("readOnly", json!(self.read_only)));
        if let Some(description) = &self.description {
            pairs.push(("description", json!(description)));
        }
        object(&pairs)
    }
}

/// A JSON object with its keys in the given order (serde_json's map sorts them).
fn object(pairs: &[(&str, Value)]) -> String {
    let fields: Vec<String> = pairs
        .iter()
        .map(|(key, value)| format!("{}:{value}", json!(key)))
        .collect();
    format!("{{{}}}", fields.join(","))
}

/// A `tools/call` running in directory mode: its MCP session, its JSON-RPC id and the graph
/// host it runs on (a `notifications/cancelled` goes there).
pub(crate) struct InFlight {
    session: String,
    request: String,
    host: Arc<dyn Host>,
}

/// Removes an [`InFlight`] entry when the call ends.
struct Tracked<'a> {
    calls: &'a Mutex<Vec<InFlight>>,
    key: Option<(String, String)>,
}

impl Drop for Tracked<'_> {
    fn drop(&mut self) {
        let Some((session, request)) = &self.key else {
            return;
        };
        let mut calls = match self.calls.lock() {
            Ok(calls) => calls,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(at) = calls
            .iter()
            .position(|c| &c.session == session && &c.request == request)
        {
            calls.remove(at);
        }
    }
}

/// The graph a call runs on: its route, host and profile.
type Target = (String, Arc<dyn Host>, Profile);

impl Mcp {
    /// Answers one HTTP request to a DIRECTORY endpoint: one endpoint over the graphs of
    /// `directory` (the host's registry). Like [`Mcp::handle`] (token, `POST`/`DELETE`/`405`,
    /// the header checks, `404` for an unknown session), but:
    ///
    /// - `initialize`, `tools/list` and `resources/list` are the directory's, under the
    ///   endpoint's default profile ([`Mcp::with_profile`]): the tools are `list_graphs`,
    ///   `use_graph` and the data tools of the default profile, each with an optional `graph`
    ///   argument; saved queries are not tools of their own (run them with `run_saved_query`
    ///   and `graph`); the instructions explain the flow ([`Mcp::directory_instructions_text`]).
    /// - `tools/call` and `resources/read` run on the graph the call names with `graph`
    ///   (resolved for this call, the argument removed before the tool runs), else on the one
    ///   the session selected with `use_graph`, else they fail naming `list_graphs` and
    ///   `use_graph`. They run on the host and under the profile
    ///   [`GraphDirectory::host`] answers for that graph (its read-only flag and policy).
    /// - `use_graph {name}` resolves `name` now ([`GraphDirectory::resolve`]) and stores the
    ///   route in the session's table entry, only when [`GraphDirectory::host`] accepts it (a
    ///   graph that is not loaded is refused with the host's text, the selection unchanged).
    ///   An alias that moves later does not move the session. A new session (a reconnect, or
    ///   one the table forgot: [`Mcp::with_max_sessions`]) starts without a selection.
    ///
    /// Directory mode needs the session table: an endpoint with signed session ids
    /// ([`Mcp::with_session_key`]) answers every request `500` with a JSON-RPC error saying
    /// so. Only HTTP: a stream [`Connection`](crate::Connection) serves one host.
    pub fn handle_directory(
        &self,
        directory: &dyn GraphDirectory,
        mut request: McpRequest<'_>,
    ) -> McpResponse {
        if let Some(refused) = self.check_token(&request) {
            return refused;
        }
        if self.sessions.is_signed() {
            return rpc_error(
                500,
                Value::Null,
                INTERNAL_ERROR,
                "this MCP endpoint is misconfigured: serving several graphs (directory mode) \
                 keeps each session's selected graph in the session table, so it cannot use \
                 stateless signed session ids",
            );
        }
        match request.method() {
            "POST" => {
                let answer = self.post_directory(directory, &mut request);
                self.count_refusal(answer)
            }
            "DELETE" => self.delete(&request),
            _ => method_not_allowed(),
        }
    }

    /// The instructions `initialize` answers on a directory endpoint: how to pick a graph
    /// (`list_graphs`, `use_graph`, `graph`), then the default profile's text about the tools.
    pub fn directory_instructions_text(&self) -> String {
        format!(
            "{DIRECTORY_INSTRUCTIONS}\n\n{}",
            instructions_of(&self.profile, false)
        )
    }

    /// The `tools/list` entries of a directory endpoint ([`Mcp::handle_directory`]):
    /// `list_graphs`, `use_graph`, then the default profile's built-in tools with the optional
    /// `graph` argument (no saved query as a tool of its own).
    pub fn directory_tool_listings(&self) -> Vec<Value> {
        let caller = self.caller_for(ClientRef::new(MCP_CLIENT, None), &self.profile);
        let read_only = self.profile.read_only;
        let mut listed = vec![list_graphs_listing(), use_graph_listing()];
        listed.extend(caller.offered().into_iter().map(|tool| {
            let mut listing = tool.listing(read_only);
            if let Some(properties) = listing["inputSchema"]
                .as_object_mut()
                .map(|schema| schema.entry("properties").or_insert_with(|| json!({})))
                .and_then(Value::as_object_mut)
            {
                properties.insert(GRAPH_ARG.into(), graph_property());
            }
            listing
        }));
        listed
    }

    fn post_directory(
        &self,
        directory: &dyn GraphDirectory,
        request: &mut McpRequest<'_>,
    ) -> McpResponse {
        let posted = match self.read_post(request) {
            Ok(posted) => posted,
            Err(answer) => return answer,
        };
        let session = posted.session_id.as_deref();
        let client = ClientRef::new(MCP_CLIENT, None);
        let client = match session {
            Some(key) => client.with_key(key),
            None => client,
        };
        let client = match posted.identity.as_deref() {
            Some(identity) => client.with_identity(identity),
            None => client,
        };
        let outcome = match parse_message(&posted.body) {
            Ok(message) => self.message_with(
                session,
                session.and_then(|id| self.sessions.client_of(id)),
                client,
                &message,
                |params| self.cancel_directory_call(session, params),
                |client, method, params| {
                    self.dispatch_directory(directory, session, client, method, params)
                },
            ),
            Err(outcome) => outcome,
        };
        frame(outcome)
    }

    fn dispatch_directory(
        &self,
        directory: &dyn GraphDirectory,
        session: Option<&str>,
        client: &ClientRef,
        method: &str,
        params: &Value,
    ) -> RpcResult {
        let plain = |value: Value| Ok((value, None));
        match method {
            "initialize" => {
                let (version, info) = negotiate(params);
                let id = self.sessions.create_routed(&info, None);
                self.stats.record_initialize(&id);
                self.remember_last_client(info);
                let result = initialize_result(
                    version,
                    false,
                    &self.profile,
                    self.directory_instructions_text(),
                );
                Ok((result, Some(id)))
            }
            "ping" => plain(json!({})),
            "tools/list" => plain(json!({ "tools": self.directory_tool_listings() })),
            "tools/call" => self.directory_call(directory, session, client, params),
            "resources/list" => plain(json!({ "resources": RESOURCES.iter()
                .filter(|r| self.profile.config.lists_resource(r.name)).map(|r| json!({
                "uri": r.uri, "name": r.name, "title": r.title,
                "description": r.description, "mimeType": r.mime,
            })).collect::<Vec<_>>() })),
            "resources/templates/list" => plain(json!({ "resourceTemplates": [] })),
            "resources/read" => {
                let uri = params.get("uri").and_then(Value::as_str).unwrap_or("");
                let Some(resource) = RESOURCES.iter().find(|r| r.uri == uri) else {
                    return Err((-32002, format!("resource not found: {uri}")));
                };
                // The GQL reference reads no graph.
                let text = if uri == "graphersal://gql-reference" {
                    gql_reference_text()
                } else {
                    let (_, host, profile) = self
                        .target(directory, session, params.get(GRAPH_ARG))
                        .map_err(|text| (INVALID_PARAMS, text))?;
                    let context = Ctx {
                        host: host.as_ref(),
                        client,
                    };
                    read_resource(&context, uri, profile.read_only)
                        .map_err(|err| (INTERNAL_ERROR, err))?
                };
                plain(json!({ "contents": [{
                    "uri": uri, "mimeType": resource.mime, "text": text,
                }] }))
            }
            "prompts/list" => plain(json!({ "prompts": [] })),
            _ => Err((METHOD_NOT_FOUND, format!("method not found: {method}"))),
        }
    }

    /// `tools/call` on a directory endpoint.
    fn directory_call(
        &self,
        directory: &dyn GraphDirectory,
        session: Option<&str>,
        client: &ClientRef,
        params: &Value,
    ) -> RpcResult {
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        let args = params.get("arguments").cloned().unwrap_or(json!({}));
        let started = Instant::now();
        let max_chars = self.profile.max_chars();
        let fitted = |result: Result<String, String>| match result {
            Ok(text) => ToolOutcome {
                text: fit(text, max_chars),
                is_error: false,
            },
            Err(text) => ToolOutcome {
                text: fit(text, max_chars),
                is_error: true,
            },
        };
        let called = match (name, args.as_object()) {
            (_, None) => Err(CallRefused::Params("arguments must be an object".into())),
            (LIST_GRAPHS, Some(_)) => Ok(fitted(self.list_graphs(directory, session))),
            (USE_GRAPH, Some(args)) => Ok(fitted(self.use_graph(directory, session, args))),
            (_, Some(_)) if !is_built_in(name) => Err(CallRefused::Params(format!(
                "unknown tool {name:?} (this endpoint serves several graphs: saved queries are \
                 not tools of their own here, run one with run_saved_query {{name, params, \
                 graph}})"
            ))),
            (_, Some(args)) => {
                let mut args = args.clone();
                let graph = args.remove(GRAPH_ARG);
                match self.target(directory, session, graph.as_ref()) {
                    Err(text) => Ok(fitted(Err(text))),
                    Ok((route, host, profile)) => {
                        let caller = self.caller_for(client.clone(), &profile);
                        let _tracked = self.track(session, client.request(), &host);
                        call_tool(host.as_ref(), &caller, name, &Value::Object(args)).map_err(
                            |refused| match refused {
                                CallRefused::Params(message)
                                    if message.contains("(read-only: only reading tools)") =>
                                {
                                    CallRefused::Params(message.replace(
                                        "(read-only: only reading tools)",
                                        &format!(
                                            "(graph {route:?} is read-only for MCP: only \
                                             reading tools)"
                                        ),
                                    ))
                                }
                                other => other,
                            },
                        )
                    }
                }
            }
        };
        let elapsed_us = started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
        let tool = (is_built_in(name) || name == LIST_GRAPHS || name == USE_GRAPH).then_some(name);
        let saved_query = (name == "run_saved_query")
            .then(|| args.get("name").and_then(Value::as_str).map(str::trim))
            .flatten();
        let (is_error, chars) = match &called {
            Ok(outcome) => (outcome.is_error, outcome.text.chars().count() as u64),
            Err(_) => (true, 0),
        };
        self.stats
            .record_tool(tool, saved_query, elapsed_us, is_error, chars);
        // A call may have changed the saved queries a stream client of that graph listed.
        let _ = self.check_tools_changed();
        match called {
            Ok(outcome) => Ok((tool_result(outcome.text, outcome.is_error), None)),
            Err(CallRefused::Internal(message)) => Err((INTERNAL_ERROR, message)),
            Err(CallRefused::Params(message)) => Err((INVALID_PARAMS, message)),
        }
    }

    /// The graph of a call: the one `graph` names (resolved now), else the session's
    /// selection; `Err` is the agent's error text.
    fn target(
        &self,
        directory: &dyn GraphDirectory,
        session: Option<&str>,
        graph: Option<&Value>,
    ) -> Result<Target, String> {
        let route = match graph {
            Some(Value::String(name)) if !name.trim().is_empty() => directory
                .resolve(name.trim())
                .map_err(|err| format!("{err} (list_graphs lists the graphs)"))?,
            None | Some(Value::Null) => session
                .and_then(|id| self.sessions.selected(id))
                .ok_or_else(|| NO_SELECTION.to_string())?,
            Some(_) => {
                return Err(format!(
                    "argument \"{GRAPH_ARG}\" must be the name of a graph (list_graphs lists them)"
                ));
            }
        };
        let (host, profile) = directory.host(&route).map_err(|err| err.to_string())?;
        Ok((route, host, profile))
    }

    /// `list_graphs`: the directory's entries and the session's selection.
    fn list_graphs(
        &self,
        directory: &dyn GraphDirectory,
        session: Option<&str>,
    ) -> Result<String, String> {
        let entries = directory.list().map_err(|err| err.to_string())?;
        let graphs: Vec<String> = entries.iter().map(GraphEntry::to_text).collect();
        let selected = session.and_then(|id| self.sessions.selected(id));
        Ok(format!(
            "{{\"graphs\":[{}],\"selected\":{}}}",
            graphs.join(","),
            json!(selected)
        ))
    }

    /// `use_graph {name}`: resolves the name now and stores the route as the session's
    /// selection when the directory serves it.
    fn use_graph(
        &self,
        directory: &dyn GraphDirectory,
        session: Option<&str>,
        args: &Map<String, Value>,
    ) -> Result<String, String> {
        let Some(name) = args
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
        else {
            return Err(
                "use_graph needs \"name\": the name of a graph (list_graphs lists them)".into(),
            );
        };
        let Some(session) = session else {
            return Err(
                "use_graph keeps the selection in the MCP session, and this request \
                        has none (no Mcp-Session-Id): initialize first, or pass \"graph\" with \
                        every call"
                    .into(),
            );
        };
        let route = directory
            .resolve(name)
            .map_err(|err| format!("{err} (list_graphs lists the graphs)"))?;
        let (_, profile) = directory.host(&route).map_err(|err| err.to_string())?;
        if !self.sessions.select(session, &route) {
            return Err(
                "this MCP session is no longer known (the server forgot it): \
                        initialize again, then call use_graph"
                    .into(),
            );
        }
        let entry = directory
            .list()
            .ok()
            .and_then(|entries| entries.into_iter().find(|entry| entry.name == route));
        let mut pairs = vec![("selected", json!(route))];
        if name != route {
            pairs.push(("requested", json!(name)));
        }
        if let Some(entry) = &entry {
            pairs.push(("kind", json!(entry.kind.as_str())));
            pairs.push(("state", json!(entry.state.as_str())));
        }
        let read_only = profile.read_only || entry.as_ref().is_some_and(|e| e.read_only);
        pairs.push(("readOnly", json!(read_only)));
        pairs.push((
            "note",
            json!(
                "calls without \"graph\" now run on this graph for this MCP session (an alias \
                 moved later does not move it); after a reconnect call use_graph again"
            ),
        ));
        Ok(object(&pairs))
    }

    /// Remembers a running call's host for `notifications/cancelled` until the guard drops.
    fn track(
        &self,
        session: Option<&str>,
        request: Option<&str>,
        host: &Arc<dyn Host>,
    ) -> Tracked<'_> {
        let key = session
            .zip(request)
            .map(|(s, r)| (s.to_string(), r.to_string()));
        if let Some((session, request)) = &key {
            let mut calls = match self.directory_calls.lock() {
                Ok(calls) => calls,
                Err(poisoned) => poisoned.into_inner(),
            };
            calls.push(InFlight {
                session: session.clone(),
                request: request.clone(),
                host: host.clone(),
            });
        }
        Tracked {
            calls: &self.directory_calls,
            key,
        }
    }

    /// `notifications/cancelled {requestId}` on a directory endpoint: cancels the request on
    /// the graph host it runs on (ignored without a session, like [`Mcp::handle`]).
    fn cancel_directory_call(&self, session: Option<&str>, params: Option<&Value>) {
        let (Some(session), Some(request)) =
            (session, params.and_then(|params| params.get("requestId")))
        else {
            return;
        };
        let request = request.to_string();
        let hosts: Vec<Arc<dyn Host>> = match self.directory_calls.lock() {
            Ok(calls) => calls
                .iter()
                .filter(|c| c.session == session && c.request == request)
                .map(|c| c.host.clone())
                .collect(),
            Err(_) => return,
        };
        let client = ClientRef::new(MCP_CLIENT, None).with_key(session);
        for host in hosts {
            host.cancel_request(&client, &request);
        }
    }
}

/// The input property `graph` of every data tool on a directory endpoint.
fn graph_property() -> Value {
    json!({
        "type": "string",
        "description": "The graph to run this one call on (list_graphs); default: the graph use_graph selected.",
    })
}

fn list_graphs_listing() -> Value {
    json!({
        "name": LIST_GRAPHS,
        "title": "List graphs",
        "description": "Lists the graphs this endpoint serves: name, alias target, kind (plain, layer, base), state (only a loaded graph answers), read-only flag, and the graph this session selected.",
        "inputSchema": {"type": "object", "properties": {}},
        "annotations": {
            "title": "List graphs",
            "readOnlyHint": true,
            "destructiveHint": false,
            "idempotentHint": true,
            "openWorldHint": false,
        },
    })
}

fn use_graph_listing() -> Value {
    json!({
        "name": USE_GRAPH,
        "title": "Use graph",
        "description": "Selects the graph the other tools work on for this MCP session (an alias is resolved now). Call it again to switch; after a reconnect call it again.",
        "inputSchema": {
            "type": "object",
            "properties": {"name": {"type": "string", "description": "A graph or alias from list_graphs."}},
            "required": ["name"],
        },
        "annotations": {
            "title": "Use graph",
            "readOnlyHint": true,
            "destructiveHint": false,
            "idempotentHint": true,
            "openWorldHint": false,
        },
    })
}

/// The error of a call that names no graph in a session without a selection.
const NO_SELECTION: &str = "no graph selected: this endpoint serves several graphs and this MCP \
session has not called use_graph (a new session, e.g. after a reconnect, starts without a \
selection). Call list_graphs, then use_graph {\"name\": \"...\"}, or pass \"graph\" with this \
call.";

/// The start of a directory endpoint's instructions.
const DIRECTORY_INSTRUCTIONS: &str = "This MCP endpoint serves SEVERAL graphs. Call \
`list_graphs` to see them (name, alias target, kind, state, read-only), then `use_graph` {name} \
to select one for this MCP session: every other tool then works on that graph. Pass `graph` \
{name} with a single call to run it on another graph without changing the selection; call \
`use_graph` again to switch. The selection belongs to the MCP session: after a reconnect (a new \
session) call `use_graph` again. A graph that is not loaded is refused until an operator loads \
it. Saved queries are not tools of their own here: run them with `run_saved_query` (with \
`graph` when needed). Each graph may be read-only. What follows describes the tools on each \
graph.";
