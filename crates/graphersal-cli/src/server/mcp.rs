//! `graphersal --server --mcp`: the Model Context Protocol endpoint (`POST /mcp`) on the dev
//! server, so an AI agent (Claude Code: `claude mcp add --transport http graphersal
//! http://127.0.0.1:<port>/mcp`) works with the SAME graph and session as the browser. **DEV
//! mode, unstable, local only.**
//!
//! The endpoint itself (protocol, tools, resources, answers) is the crate `graphersal_mcp`;
//! this module is its host on the dev server: it adapts `tiny_http` to
//! [`McpRequest`]/`McpResponse` after the `Host`/`Origin` checks of the HTTP API (the sockets
//! bind the loopback unless `--host` or a container says otherwise, `super::bind`), and
//! implements [`Executor`] with the session-thread queue ([`Context::call`]: the server's limits
//! apply, the UI's Stop cancels a running MCP query too, the change feed records MCP changes
//! with the client `mcp`) and [`Host`] with the server's state (the read-only view the user
//! opened, the process statistics, `graph_info`'s source/store/damage fields, the playground's
//! examples).

use std::sync::atomic::Ordering;

use graphersal_mcp::{ClientRef, Executor, Host, Job, McpError, McpRequest};
use serde_json::{Value, json};
use tiny_http::{Method, Request, Response};

pub(super) use graphersal_mcp::Mcp;

use super::store::current_view;
use super::{Answer, Client, Context, HttpError, as_client, with_header, with_process_memory};

/// The start banner's MCP rows: where the endpoint is, and the command that connects Claude
/// Code to it.
pub(super) fn banner(mcp: &Mcp, host: &str, port: u16) -> (String, String) {
    let url = format!("http://{host}:{port}/mcp");
    let header = if mcp.has_token() {
        " --header \"Authorization: Bearer <your --mcp-token>\""
    } else {
        ""
    };
    (
        format!(
            "{url}{}{}",
            if mcp.read_only() { " (read-only)" } else { "" },
            if mcp.query_tools() {
                " (saved queries as tools)"
            } else {
                ""
            }
        ),
        format!("claude mcp add --transport http graphersal {url}{header}"),
    )
}

/// `{readOnly, endpoint, client: {name, version} | null}` for `GET /api/info` and
/// `/api/changes`, or null without `--mcp`.
pub(super) fn status(context: &Context) -> Value {
    match &context.mcp {
        None => Value::Null,
        Some(mcp) => mcp.status(),
    }
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
    let headers: Vec<(String, String)> = request
        .headers()
        .iter()
        .map(|h| (h.field.as_str().to_string(), h.value.as_str().to_string()))
        .collect();
    let mut mcp_request = McpRequest::new(method.as_str(), request.as_reader());
    for (name, value) in &headers {
        mcp_request = mcp_request.header(name, value);
    }
    let (status, headers, body) = mcp.handle(context, mcp_request).into_parts();
    let mut answer = Response::from_data(body).with_status_code(status);
    for (name, value) in &headers {
        answer = with_header(answer, name, value);
    }
    Ok(answer)
}

/// The client of the request (or AI Chat turn) this thread answers, for a job the MCP code runs
/// on its behalf.
#[cfg(feature = "ai")]
pub(super) fn current_client() -> ClientRef {
    super::CLIENT.with(|c| {
        let client = c.borrow();
        ClientRef::new(&client.name, client.detail.as_deref())
    })
}

impl Executor for Context {
    /// Queues `job` on the session thread for `client` and waits for it ([`Context::call`]).
    fn execute(&self, client: &ClientRef, job: Job) -> Result<(), McpError> {
        let caller = Client::with_detail(client.name(), client.detail()).with_key(client.key());
        as_client(caller, || self.call(move |session| job(session))).map_err(
            |HttpError(status, message)| match status {
                // The pool of the read-only serving mode is busy: the endpoint answers 503.
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
        with_process_memory(self, statistics)
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

    fn query_started(&self) {
        self.running.fetch_add(1, Ordering::SeqCst);
    }

    fn query_finished(&self) {
        self.running.fetch_sub(1, Ordering::SeqCst);
    }
}
