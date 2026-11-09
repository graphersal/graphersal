//! The front of a host's listener: the lifecycle of the hosted graph (loading, ready,
//! stopping), the health probes, the graph resolver and the activity of the server.
//!
//! A host creates its [`Server`] BEFORE the graph is loaded and hands every request of its
//! listener to [`Server::handle`] from the start; it tells the server [`Server::ready`] once the
//! graph serves and [`Server::stop`] when it stops. Meanwhile:
//!
//! - `GET`/`HEAD /healthz` (liveness) answers `200 {"status": "ok"}` always, `/readyz`
//!   (readiness) `200 {"status": "ready"}` once ready, `503 {"status": "loading"}` before and
//!   `503 {"status": "stopping"}` after a stop; another method on them is a `405`. The probes
//!   come first: before the request filter (a Kubernetes probe sends `Host: <pod IP>:<port>`),
//!   without a lock, without the session (a long query cannot fail liveness), logged at trace
//!   only, and they are no activity.
//! - Every other request answers `503` while the graph loads ("still loading") and after a stop
//!   ("stopping"), else goes to the context the graph resolver picks ([`crate::handle`]).
//!
//! Extension points here (each with its core default):
//! - **graph resolver** ([`GraphResolver`]; default: one graph, [`Server::ready`]): which
//!   context serves a request (and so an MCP connection, whose requests carry its
//!   `Mcp-Session-Id`). The server holds no registry of graphs: a host with several supplies
//!   its own resolver ([`Server::ready_with`]);
//! - **activity / lifecycle hooks** ([`LifecycleHooks`]; default: none): told when the server
//!   becomes ready, when it starts stopping, and of every request's activity
//!   ([`Server::idle_for`] answers the same without a hook).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::context::{Context, ServeError};
use crate::http::{HttpError, Method, Request, Response};

/// Where a [`Server`] is in its life.
///
/// `#[non_exhaustive]`: further states may be added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Lifecycle {
    /// The graph is loading: only the probes answer.
    Loading,
    /// The graph serves.
    Ready,
    /// The host stops: no new request is served.
    Stopping,
}

/// **Extension point "graph resolver"**: which hosted graph (its [`Context`]) serves a request.
/// Default: one graph ([`OneGraph`], what [`Server::ready`] installs).
///
/// Called for every request but the probes, after the lifecycle checks and before the
/// `Host`/`Origin` checks and the request filter (both the resolved context's). An MCP request names its connection by its
/// `Mcp-Session-Id` header.
pub trait GraphResolver: Send + Sync {
    /// The context that serves `request`, or the answer that refuses it (`{"error": message}`
    /// with the error's status; a `503` also with `Retry-After: 1`).
    fn resolve(&self, request: &Request<'_>) -> Result<Arc<Context>, HttpError>;
}

/// The default graph resolver: every request goes to the one context.
pub struct OneGraph {
    context: Arc<Context>,
}

impl OneGraph {
    /// The resolver that answers every request with `context`.
    pub fn new(context: Arc<Context>) -> Self {
        Self { context }
    }
}

impl GraphResolver for OneGraph {
    fn resolve(&self, _request: &Request<'_>) -> Result<Arc<Context>, HttpError> {
        Ok(self.context.clone())
    }
}

/// **Extension point "activity / lifecycle hooks"**: told of the server's lifecycle and
/// activity. Default: none. Every method has a default that does nothing; each is called on
/// the thread that caused it and must return quickly.
pub trait LifecycleHooks: Send + Sync {
    /// The server became ready ([`Server::ready`]): the graph serves.
    fn ready(&self) {}

    /// The server started stopping ([`Server::stop`], once): from now on every request but
    /// the probes answers `503`. The host drains its queue itself.
    fn stopping(&self) {}

    /// A request is being served: called when it reaches its context and again when it is
    /// answered (a probe, or a `503` while loading or stopping, is no activity).
    fn activity(&self) {}
}

/// The front of a host's listener: lifecycle, probes, graph resolver, activity (module docs).
pub struct Server {
    resolver: OnceLock<Arc<dyn GraphResolver>>,
    stopping: AtomicBool,
    hooks: Option<Arc<dyn LifecycleHooks>>,
    /// When the server was created; the last activity is measured from here.
    base: Instant,
    /// Nanoseconds from `base` to the last activity.
    last_activity: AtomicU64,
}

impl Default for Server {
    fn default() -> Self {
        Self::new()
    }
}

impl Server {
    /// A server that is loading, without lifecycle hooks.
    pub fn new() -> Self {
        Self {
            resolver: OnceLock::new(),
            stopping: AtomicBool::new(false),
            hooks: None,
            base: Instant::now(),
            last_activity: AtomicU64::new(0),
        }
    }

    /// The server with lifecycle hooks ([`LifecycleHooks`]); default: none.
    pub fn with_hooks(mut self, hooks: Arc<dyn LifecycleHooks>) -> Self {
        self.hooks = Some(hooks);
        self
    }

    /// The graph serves: every request goes to `context` (the default graph resolver,
    /// [`OneGraph`]). `Err` when the server was made ready before.
    pub fn ready(&self, context: Arc<Context>) -> Result<(), ServeError> {
        self.ready_with(Arc::new(OneGraph::new(context)))
    }

    /// The graph serves: `resolver` picks the context of every request. `Err` when the server
    /// was made ready before. A stopped server stays stopping.
    pub fn ready_with(&self, resolver: Arc<dyn GraphResolver>) -> Result<(), ServeError> {
        self.resolver
            .set(resolver)
            .map_err(|_| ServeError::Lifecycle("the server was made ready before".to_string()))?;
        if let Some(hooks) = &self.hooks {
            hooks.ready();
        }
        Ok(())
    }

    /// The host stops: `/readyz` answers `503 stopping` and every other request but the probes
    /// `503 the server is stopping`. Only the first call tells the hooks.
    pub fn stop(&self) {
        if !self.stopping.swap(true, Ordering::SeqCst)
            && let Some(hooks) = &self.hooks
        {
            hooks.stopping();
        }
    }

    /// Where the server is in its life (a stop wins over ready).
    pub fn state(&self) -> Lifecycle {
        if self.stopping.load(Ordering::SeqCst) {
            Lifecycle::Stopping
        } else if self.resolver.get().is_none() {
            Lifecycle::Loading
        } else {
            Lifecycle::Ready
        }
    }

    /// How long ago a request was last served (since the server was created when none was).
    pub fn idle_for(&self) -> Duration {
        let last = Duration::from_nanos(self.last_activity.load(Ordering::SeqCst));
        self.base.elapsed().saturating_sub(last)
    }

    /// Records a request's activity and tells the hooks.
    fn touch(&self) {
        let now = u64::try_from(self.base.elapsed().as_nanos()).unwrap_or(u64::MAX);
        self.last_activity.fetch_max(now, Ordering::SeqCst);
        if let Some(hooks) = &self.hooks {
            hooks.activity();
        }
    }

    /// The answer of a health probe (`GET`/`HEAD /healthz`, `/readyz`, any query string), or
    /// `None` for every other path: what [`Self::handle`] answers first. A host that refuses a
    /// request before converting it (a connection cap) still answers the probes with this.
    pub fn probe(&self, method: &Method, url: &str) -> Option<Response> {
        let path = url.split('?').next().unwrap_or_default();
        if path != "/healthz" && path != "/readyz" {
            return None;
        }
        if !matches!(method, Method::Get | Method::Head) {
            return Some(Response::json(
                405,
                &json!({ "error": format!("method {method} is not allowed on {path}") }),
            ));
        }
        // Probes come every few seconds: never above trace.
        tracing::trace!(target: "graphersal::server", "{method} {path}");
        if path == "/healthz" {
            return Some(Response::json(200, &json!({ "status": "ok" })));
        }
        let (status, state) = match self.state() {
            Lifecycle::Stopping => (503, "stopping"),
            Lifecycle::Loading => (503, "loading"),
            Lifecycle::Ready => (200, "ready"),
        };
        Some(Response::json(status, &json!({ "status": state })))
    }

    /// Answers one request of the host's listener: a probe ([`Self::probe`]); `503` while
    /// loading or stopping; else the context the graph resolver picks answers it
    /// ([`crate::handle`]: the request filter, then the API).
    pub fn handle(&self, request: Request<'_>) -> Response {
        if let Some(answer) = self.probe(request.method(), request.url()) {
            return answer;
        }
        let Some(resolver) = self.resolver.get() else {
            return Response::json(
                503,
                &json!({ "error": "the server is still loading its graph; GET /readyz answers \
                    200 when it serves queries" }),
            );
        };
        if self.stopping.load(Ordering::SeqCst) {
            return Response::json(503, &json!({ "error": "the server is stopping" }));
        }
        self.touch();
        let answer = match resolver.resolve(&request) {
            Ok(context) => crate::api::handle(&context, request),
            Err(refused) => crate::api::error_response(refused),
        };
        self.touch();
        answer
    }
}
