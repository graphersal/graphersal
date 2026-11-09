//! **Internal crate, EXPERIMENTAL, no stability guarantees.** The minimal hosting core of
//! Graphersal's servers for ONE graph, without an HTTP library:
//!
//! - the hosted graph runtime: one [`graphersal_session::Session`] on its session thread with
//!   its job queue ([`spawn_session`], [`Context::call`]), the job workers of the scheduled jobs
//!   ([`start_job_ticker`]), the run registry with the client-scoped cancel ([`Runs`]);
//! - the playground HTTP API as an HTTP-library-free boundary: the host converts each request
//!   of its own server into a [`Request`], calls [`handle`], and writes the [`Response`] back;
//! - the `Host`/`Origin` checks of a listening server ([`HostCheck`]; where it listens is the
//!   host's decision);
//! - the playground UI: a UI directory over the copy embedded at build time ([`Ui`], feature
//!   `ui-assets`);
//! - the glue that implements `graphersal-mcp`'s `Host`/`Executor` over the runtime and serves
//!   `POST /mcp` (feature `mcp`).
//!
//! Today's host is the dev server of `graphersal-cli` (`graphersal --server`), which keeps the
//! `tiny_http` accept loop, the argument parsing, the health probes, the reader pool of the
//! read-only serving mode (plugged in as a [`JobQueue`]) and the AI Chat (plugged in through
//! [`Hooks`]). It is published only because `graphersal-cli` depends on it.

#![warn(missing_docs)]

mod api;
mod context;
mod host_check;
mod http;
mod job_workers;
mod mcp;
mod runs;
mod ui;

pub use api::{SERVER_ID, handle};
pub use context::{
    Busy, BusyReason, Context, ContextBuilder, Hooks, Job, JobQueue, Runner, SESSION_STACK_BYTES,
    ServeError, spawn_session, start_job_ticker,
};
pub use host_check::{Exposure, HostCheck, HostRules};
pub use http::{HttpError, MAX_BODY_BYTES, Method, Request, Response};
#[cfg(feature = "mcp")]
pub use mcp::current_client;
pub use runs::{Registered, Runs};
pub use ui::Ui;
