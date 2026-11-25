//! `POST /mcp`: the Model Context Protocol endpoint on the same session as the playground
//! (feature `mcp`), so an AI agent (Claude Code: `claude mcp add --transport http graphersal
//! http://127.0.0.1:<port>/mcp`) works with the SAME graph and session as the browser.
//!
//! The endpoint itself (protocol, tools, resources, answers) is the crate `graphersal_mcp`;
//! this module is its host: it adapts a [`crate::Request`] to `McpRequest`/`McpResponse` after
//! the `Host`/`Origin` checks of the HTTP API, and implements `Executor` with the session-thread
//! queue ([`Context::call`]: the server's limits apply, the UI's Stop cancels a running MCP
//! query too, the change feed records MCP changes with the client `mcp`) and `Host` with the
//! server's state (the read-only view the user opened, the process statistics, `graph_info`'s
//! source/store/damage fields, the playground's examples).
//!
//! Without the feature the endpoint answers `404` like a server started without `--mcp`.

use serde_json::Value;

use crate::context::Context;
use crate::http::HttpError;

#[cfg(feature = "mcp")]
pub use enabled::current_client;
#[cfg(feature = "mcp")]
pub(crate) use enabled::*;

#[cfg(not(feature = "mcp"))]
pub(crate) use disabled::*;

/// The answer of the endpoint on a server without it.
fn off() -> HttpError {
    HttpError(
        404,
        "the MCP endpoint is off: start the server with --mcp".into(),
    )
}

/// `{readOnly, endpoint, client: {name, version} | null}` for `GET /api/info` and
/// `/api/changes`, or null without `--mcp` (`endpoint`: `/mcp`, or `<mount>/mcp` under the
/// host's [`crate::ContextBuilder::mount_path`]).
pub(crate) fn status(context: &Context) -> Value {
    // In allow-list mode `/mcp` is refused: no endpoint to show.
    if context.allow_list.is_some() {
        return Value::Null;
    }
    match &context.mcp {
        None => Value::Null,
        Some(_) => endpoint_status(context),
    }
}

#[cfg(feature = "mcp")]
mod enabled {
    use graphersal::exec::CancelToken;
    use graphersal_mcp::{ClientRef, Executor, Host, Job, McpError, McpRequest, Refusal};
    use serde_json::{Value, json};

    pub(crate) use graphersal_mcp::Mcp;

    use super::off;
    use crate::api::current_view;
    use crate::context::{
        BusyReason, CLIENT, Client, Context, ServeError, as_client, request_read_only,
    };
    use crate::http::{HttpError, Request, Response};
    use crate::runs::{RunDetails, RunKind};

    /// The endpoint's status with `readOnly` of the context's profile (or of the runtime
    /// read-only toggle, while it is on, or of a request the host marked read-only).
    pub(crate) fn endpoint_status(context: &Context) -> Value {
        let (Some(mcp), Some(profile)) = (context.mcp(), context.mcp_profile()) else {
            return Value::Null;
        };
        let mut status = mcp.status();
        status["readOnly"] =
            json!(profile.read_only() || context.runtime_read_only_on() || request_read_only());
        // Under a host's URL prefix the endpoint is `<mount>/mcp` (ContextBuilder::mount_path).
        if let Some(mount) = &context.mount_path {
            status["endpoint"] = json!(format!("{mount}/mcp"));
        }
        status
    }

    /// Whether an agent's writes are refused here: the context's MCP profile is read-only, or
    /// the runtime read-only toggle is on (its commit hook refuses them; the profile and so the
    /// tool list stay as they are), or the request this thread answers is marked read-only
    /// ([`Request::mark_read_only`]: it is served under the profile made read-only). Asked only
    /// of a context with an endpoint.
    fn profile_read_only(context: &Context) -> bool {
        context.mcp_profile().is_some_and(|p| p.read_only())
            || context.runtime_read_only_on()
            || request_read_only()
    }

    /// `GET /api/mcp/stats`: the endpoint's usage statistics (memory only, since the start), or
    /// 404 without `--mcp`.
    pub(crate) fn stats(context: &Context) -> Result<Response, HttpError> {
        let Some(mcp) = &context.mcp else {
            return Err(off());
        };
        let mut answer = mcp
            .stats()
            .to_json()
            .map_err(|err| HttpError(500, err.to_string()))?;
        answer["readOnly"] = json!(profile_read_only(context));
        answer["statelessSessions"] = json!(mcp.stateless_sessions());
        Ok(Response::json(200, &answer))
    }

    /// Counts a refusal of the host's job queue for an MCP request.
    pub(crate) fn record_refusal(mcp: &Mcp, refused: BusyReason) {
        mcp.stats().record_refusal(match refused {
            BusyReason::QueueFull => Refusal::QueueFull,
            BusyReason::QueueTimeout => Refusal::QueueTimeout,
            BusyReason::ClientLimit => Refusal::ClientLimit,
            BusyReason::Stopping => Refusal::Stopping,
        });
    }

    /// Routes `/mcp` (after the Host/Origin checks of the HTTP API).
    pub(crate) fn route(context: &Context, request: &mut Request) -> Result<Response, HttpError> {
        let Some(mcp) = &context.mcp else {
            return Err(off());
        };
        let headers = request.headers().to_vec();
        let method = request.method().as_str().to_string();
        let identity = request.identity().map(str::to_string);
        let marked_read_only = request.is_read_only();
        let mut mcp_request = McpRequest::new(&method, request.body());
        for (name, value) in &headers {
            mcp_request = mcp_request.header(name, value);
        }
        if let Some(identity) = &identity {
            mcp_request = mcp_request.with_identity(identity);
        }
        let profile = context.mcp_profile().unwrap_or_else(|| mcp.profile());
        // A request the host marked read-only: only reading tools, a writing query refused.
        let read_only;
        let profile = if marked_read_only && !profile.read_only() {
            read_only = profile.clone().with_read_only(true);
            &read_only
        } else {
            profile
        };
        let (status, headers, body) = mcp
            .handle_with_profile(context, profile, mcp_request)
            .into_parts();
        let mut answer = Response::new(status, body);
        for (name, value) in &headers {
            answer = answer.with_header(name, value);
        }
        Ok(answer)
    }

    /// The client of the request (or AI Chat turn) this thread answers, for a job the MCP code
    /// runs on its behalf.
    pub fn current_client() -> ClientRef {
        CLIENT.with(|c| {
            let client = c.borrow();
            ClientRef::new(&client.name, client.detail.as_deref())
        })
    }

    /// The server's client of an endpoint caller.
    fn client_of(client: &ClientRef) -> Client {
        let mut caller = Client::with_detail(client.name(), client.detail()).with_key(client.key());
        caller.identity = client.identity().map(str::to_string);
        caller
    }

    /// The key the server cancels a caller's runs by (`mcp:<session id>`, `ai:<conversation>`).
    fn run_key(client: &ClientRef) -> String {
        client_of(client).pool_key()
    }

    impl Executor for Context {
        /// Queues `job` on the session thread for `client` and waits for it ([`Context::call`]).
        fn execute(&self, client: &ClientRef, job: Job) -> Result<(), McpError> {
            let caller = client_of(client);
            as_client(caller, || self.call(move |session| job(session))).map_err(|err| match err {
                // The host's job queue is busy: the endpoint answers 503.
                ServeError::Busy(busy) => McpError::Busy(busy.message().to_string()),
                ServeError::Setup(_)
                | ServeError::Thread(_)
                | ServeError::Ui(_)
                | ServeError::Lifecycle(_)
                | ServeError::Session(_)
                | ServeError::NoMcp(_)
                | ServeError::Unsupported(_)
                | ServeError::Open(_)
                | ServeError::Failed(_) => McpError::Unavailable(err.to_string()),
            })
        }
    }

    impl Host for Context {
        fn view(&self) -> Value {
            current_view(self)
        }

        fn add_process_statistics(&self, statistics: Value) -> Value {
            self.with_process_memory(statistics)
        }

        fn add_graph_info(&self, info: &mut Value) -> Result<(), McpError> {
            info["source"] = json!(self.source.as_ref().map(|p| p.display().to_string()));
            info["store"] = json!(self.store.as_ref().map(|s| s.location()));
            // Damage found on disk while the store is open (writes refused when frozen).
            info["damageFound"] = match self.store.as_ref() {
                None => Value::Null,
                Some(store) => graphersal_session::store::found_damage_value(&**store)
                    .map_err(|err| McpError::Failed(err.to_string()))?,
            };
            if self.mcp.is_some() {
                info["mcpReadOnly"] = json!(profile_read_only(self));
            }
            Ok(())
        }

        fn examples_json(&self) -> Option<Vec<u8>> {
            self.ui.read("js/examples.json").map(|(bytes, _)| bytes)
        }

        /// What the host said when it built the context (`ContextBuilder::layered`) or the
        /// last session job saw (every job refreshes it when the session serves another
        /// graph, the first job always); runs no job (`initialize` must not wait for a busy
        /// session): unknown counts as a plain graph.
        fn serves_layer(&self) -> bool {
            use crate::context::Layered;
            use std::sync::atomic::Ordering;
            self.layered.load(Ordering::Relaxed) == Layered::LAYER
        }

        /// The host's route tag ([`crate::ContextBuilder::mcp_session_route`]); runs no job.
        fn session_route(&self) -> Option<String> {
            self.mcp_session_route.clone()
        }

        fn run_started(&self, client: &ClientRef, cancel: &CancelToken) -> u64 {
            self.run_started_with_text(client, cancel, "")
        }

        fn run_started_with_text(
            &self,
            client: &ClientRef,
            cancel: &CancelToken,
            text: &str,
        ) -> u64 {
            // The identity the endpoint carries (a request's or a connection's), else the one
            // of the thread of the request (or the agent's turn), if any.
            let identity = client
                .identity()
                .map(str::to_string)
                .or_else(|| CLIENT.with(|c| c.borrow().identity.clone()));
            // The Host trait answers plain values: a registry failure is logged and the run is
            // not registered (id 0, which no run has: the registry counts from 1).
            self.runs
                .start_with(
                    &run_key(client),
                    client.request(),
                    cancel.clone(),
                    RunDetails {
                        kind: Some(RunKind::Tool),
                        text: (!text.is_empty()).then(|| text.to_string()),
                        identity,
                        mount: self.mount_path.clone(),
                    },
                )
                .unwrap_or_else(|err| {
                    tracing::warn!(target: "graphersal::server", %err, "cannot register an MCP run");
                    0
                })
        }

        fn run_ended(&self, run: u64) {
            if let Err(err) = self.runs.end(run) {
                tracing::warn!(target: "graphersal::server", %err, run, "cannot end an MCP run");
            }
        }

        fn cancel_request(&self, client: &ClientRef, request: &str) -> bool {
            self.runs
                .cancel_alias(&run_key(client), request)
                .unwrap_or_else(|err| {
                    tracing::warn!(target: "graphersal::server", %err, "cannot cancel an MCP run");
                    false
                })
        }
    }
}

#[cfg(not(feature = "mcp"))]
mod disabled {
    use serde_json::Value;

    use super::off;
    use crate::context::{BusyReason, Context};
    use crate::http::{HttpError, Request, Response};

    /// No endpoint: the type has no value.
    pub(crate) enum Mcp {}

    pub(crate) fn endpoint_status(context: &Context) -> Value {
        match context.mcp.as_deref() {
            Some(mcp) => match *mcp {},
            None => Value::Null,
        }
    }

    pub(crate) fn stats(_context: &Context) -> Result<Response, HttpError> {
        Err(off())
    }

    pub(crate) fn record_refusal(mcp: &Mcp, _refused: BusyReason) {
        match *mcp {}
    }

    pub(crate) fn route(_context: &Context, _request: &mut Request) -> Result<Response, HttpError> {
        Err(off())
    }
}
