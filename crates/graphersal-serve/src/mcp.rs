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
/// `/api/changes`, or null without `--mcp`.
pub(crate) fn status(context: &Context) -> Value {
    match &context.mcp {
        None => Value::Null,
        Some(mcp) => endpoint_status(mcp),
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
    use crate::context::{BusyReason, CLIENT, Client, Context, as_client};
    use crate::http::{HttpError, Request, Response};
    use crate::runs::{RunDetails, RunKind};

    pub(crate) fn endpoint_status(mcp: &Mcp) -> Value {
        mcp.status()
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
        answer["readOnly"] = json!(mcp.read_only());
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
        let mut mcp_request = McpRequest::new(&method, request.body());
        for (name, value) in &headers {
            mcp_request = mcp_request.header(name, value);
        }
        let (status, headers, body) = mcp.handle(context, mcp_request).into_parts();
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
        Client::with_detail(client.name(), client.detail()).with_key(client.key())
    }

    /// The key the server cancels a caller's runs by (`mcp:<session id>`, `ai:<conversation>`).
    fn run_key(client: &ClientRef) -> String {
        client_of(client).pool_key()
    }

    impl Executor for Context {
        /// Queues `job` on the session thread for `client` and waits for it ([`Context::call`]).
        fn execute(&self, client: &ClientRef, job: Job) -> Result<(), McpError> {
            let caller = client_of(client);
            as_client(caller, || self.call(move |session| job(session))).map_err(
                |HttpError(status, message)| match status {
                    // The host's job queue is busy: the endpoint answers 503.
                    503 => McpError::Busy(message),
                    _ => McpError::Unavailable(message),
                },
            )
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
            if let Some(mcp) = &self.mcp {
                info["mcpReadOnly"] = json!(mcp.read_only());
            }
            Ok(())
        }

        fn examples_json(&self) -> Option<Vec<u8>> {
            self.ui.read("js/examples.json").map(|(bytes, _)| bytes)
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
            // Called on the thread of the request (or the agent's turn): its identity, if any.
            let identity = CLIENT.with(|c| c.borrow().identity.clone());
            self.runs.start_with(
                &run_key(client),
                client.request(),
                cancel.clone(),
                RunDetails {
                    kind: Some(RunKind::Tool),
                    text: (!text.is_empty()).then(|| text.to_string()),
                    identity,
                },
            )
        }

        fn run_ended(&self, run: u64) {
            self.runs.end(run);
        }

        fn cancel_request(&self, client: &ClientRef, request: &str) -> bool {
            self.runs.cancel_alias(&run_key(client), request)
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

    pub(crate) fn endpoint_status(mcp: &Mcp) -> Value {
        match *mcp {}
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
