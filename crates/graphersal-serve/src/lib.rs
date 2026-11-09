//! **Internal crate, EXPERIMENTAL, no stability guarantees.** The minimal hosting core of
//! Graphersal's servers for ONE graph, without an HTTP library:
//!
//! - the hosted graph runtime: one [`graphersal_session::Session`] on its session thread with
//!   its job queue ([`spawn_session`], [`Context::call`]), the job workers of the scheduled jobs
//!   ([`start_job_ticker`]), the run registry with the client-scoped cancel ([`Runs`]);
//! - the playground HTTP API as an HTTP-library-free boundary: the host converts each request
//!   of its own server into a [`Request`], calls [`Server::handle`] (or [`handle`] with one
//!   context), and writes the [`Response`] back;
//! - the lifecycle of the hosted graph: the health probes (`/healthz`, `/readyz`) and the `503`
//!   answers while it loads or stops ([`Server`]);
//! - the `Host`/`Origin` checks of a listening server ([`HostCheck`]; where it listens is the
//!   host's decision);
//! - the playground UI: a UI directory over the copy embedded at build time ([`Ui`], feature
//!   `ui-assets`);
//! - the glue that implements `graphersal-mcp`'s `Host`/`Executor` over the runtime and serves
//!   `POST /mcp` (feature `mcp`).
//!
//! # Extension points
//!
//! What a host adds goes through these points; each has the core default a host gets without
//! it. The core never grows several databases, run administration, authentication, quotas or
//! bounded queues of its own: those belong to the hosts.
//!
//! | Point | Core default | Where |
//! |---|---|---|
//! | Graph resolver: which hosted graph serves a request / an MCP connection | one graph | [`GraphResolver`], [`Server::ready_with`] ([`OneGraph`], [`Server::ready`]) |
//! | Run observer + cancel handle: every run's start and end, with a handle that cancels it | none | [`RunObserver`], [`Runs::with_observer`] |
//! | Request filter: sees each request before routing, refuses it or attaches an identity | none beyond the loopback/`Host`/`Origin` checks, which always run first | [`RequestFilter`], [`ContextBuilder::filter`] ([`HostCheck`]) |
//! | Extra routes: the `/api/` endpoints the core does not know | `404` | [`Hooks::route`], [`ContextBuilder::hooks`] |
//! | Activity / lifecycle hooks: ready, stopping, last activity | none | [`LifecycleHooks`], [`Server::with_hooks`], [`Server::idle_for`] |
//! | Executor policy: how jobs reach the session | one session thread, unbounded queue | [`Runner`], [`JobQueue`] |
//!
//! With the feature `mcp`, one `graphersal_mcp::Mcp` endpoint can serve several graphs: each
//! context gets the same endpoint ([`ContextBuilder::shared_mcp`]; its statistics, MCP sessions
//! and token stay the process's) and, if the host wants, a profile of its own
//! ([`ContextBuilder::mcp_profile`]: configuration, read-only flag, answer limit, access
//! policy). The graph resolver picks the context of an `/mcp` request, and so the session its
//! jobs run on and its profile; a host that serves MCP over a stream picks the context from its
//! own handshake and calls [`Context::mcp_connection`]. The identity a request filter attached
//! travels with the MCP request to its session jobs and to the run observer's [`RunInfo`].
//!
//! Today's host is the dev server of `graphersal-cli` (`graphersal --server`), which keeps the
//! `tiny_http` accept loop, the argument parsing, the signals, the reader pool of the read-only
//! serving mode (plugged in as a [`JobQueue`]) and the AI Chat (its endpoints plugged in through
//! [`Hooks::route`]). It is published because `graphersal-cli` depends on it, and for hosts of
//! Graphersal's own servers; its API changes in any release (a patch release included): a crate
//! that depends on it pins the exact version (`graphersal-serve = "=0.x.y"`) during 0.x.
//!
//! `serde_json::Value` appears in the public API on purpose ([`JobQueue::status`],
//! [`Hooks::add_info`], [`Hooks::add_process_statistics`], [`ContextBuilder::presets`],
//! [`Request::json_body`], [`Response::json`]): this crate is a JSON boundary like
//! `graphersal-session`, an exception to the engine's rule of no third-party types in the API.

#![warn(missing_docs)]

mod api;
mod context;
mod host_check;
mod http;
mod job_workers;
mod mcp;
mod runs;
mod server;
mod ui;

#[cfg(test)]
mod extension_tests;

pub use api::{SERVER_ID, handle};
pub use context::{
    Busy, BusyReason, Context, ContextBuilder, Hooks, Job, JobQueue, RequestFilter, Runner,
    SCHEDULED_JOBS_REFUSAL, SESSION_STACK_BYTES, ServeError, spawn_session, start_job_ticker,
};
pub use host_check::{Exposure, HostCheck, HostRules};
pub use http::{HttpError, MAX_BODY_BYTES, Method, Request, Response};
#[cfg(feature = "mcp")]
pub use mcp::current_client;
pub use runs::{RUN_TEXT_HEAD_CHARS, Registered, RunHandle, RunInfo, RunKind, RunObserver, Runs};
pub use server::{GraphResolver, Lifecycle, LifecycleHooks, OneGraph, Server};
pub use ui::Ui;
