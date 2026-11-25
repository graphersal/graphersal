//! **Internal crate, EXPERIMENTAL, no stability guarantees.** The minimal hosting core of
//! Graphersal's servers for ONE graph, without an HTTP library:
//!
//! - the hosted graph runtime: one [`graphersal_session::Session`] on its session thread with
//!   its job queue ([`spawn_session`], [`Context::call`]; its failures are [`ServeError`]s, an
//!   HTTP host answers them through `From<ServeError> for HttpError`), the job workers of the
//!   scheduled jobs ([`start_job_ticker`], stopped through its [`JobTicker`] or when the last
//!   context is gone), the run registry with the client-scoped cancel ([`Runs`]);
//! - the playground HTTP API as an HTTP-library-free boundary: the host converts each request
//!   of its own server into a [`Request`], calls [`Server::handle`] (or [`handle`] with one
//!   context), and writes the [`Response`] back;
//! - the lifecycle of the hosted graph: the health probes (`/healthz`, `/readyz`) and the `503`
//!   answers while it loads or stops ([`Server`]);
//! - the `Host`/`Origin` checks of a listening server ([`HostCheck`]; where it listens is the
//!   host's decision);
//! - the protocol marker every `GET /api/info` answers ([`PROTOCOL`], `"graphersal/1"`), by
//!   which the playground recognizes any Graphersal host; `server` only names the host
//!   ([`ContextBuilder::server_name`], default [`DEFAULT_SERVER_NAME`]) and `dev` says whether it
//!   is a development server ([`ContextBuilder::dev`], the playground's DEV badge);
//! - the playground UI: a UI directory over the copy embedded at build time ([`Ui`], feature
//!   `ui-assets`), and the UI profile its pages show ([`ContextBuilder::ui_profile`], `GET
//!   /api/info` `uiProfile`; a host whose profile brings its own files serves them itself
//!   before [`Server::handle`], or puts them into the [`Ui`] directory, an overlay);
//! - the glue that implements `graphersal-mcp`'s `Host`/`Executor` over the runtime and serves
//!   `POST /mcp` (feature `mcp`).
//!
//! Two EXPERIMENTAL helpers start a hosted graph: [`launch`] loads a [`ServedSource`] (a
//! sample, a graph file, a Store, a layer store, a graph the host loaded) into a new session
//! thread and answers a [`Launched`] whose [`Launched::context_builder`] is pre-filled for the
//! [`Context`]; [`open_store`], [`layer_base_of_store`] and [`open_layer_store`] open the
//! databases of a host serving several (each [`Context`] is still one graph), answering what
//! they found as data ([`OpenNote`]) and the embedding models per database.
//!
//! A host serving several graphs mounts each [`Context`] under a URL prefix (`/db/<name>/`,
//! EXPERIMENTAL): it strips the prefix when it builds the [`Request`] and picks the context in
//! its [`GraphResolver`]; [`ContextBuilder::mount_path`] only tells the clients where they are
//! (`GET /api/info` `mountPath`, the MCP endpoint `<mount>/mcp`), [`HostCheck::with_hint`]
//! puts the host's own advice into the `403` answers, and [`Request::copy_body_to`] streams an
//! upload into a sink of the host.
//!
//! A host may also turn a served graph read-only and back while it is served, without
//! reopening it (EXPERIMENTAL): [`ContextBuilder::runtime_read_only`],
//! [`Context::set_read_only`], [`Context::is_read_only`]; every modifying request (HTTP API, MCP
//! tools, scheduled jobs) is then refused with one message ([`READ_ONLY_HEADLINE`]).
//!
//! A host may also mark ONE request read-only (EXPERIMENTAL; [`Request::mark_read_only`], from
//! its router or its [`RequestFilter`], e.g. for a user with read access only): that request
//! runs its queries under the reading authorizer, every route that writes answers `409`
//! ([`READ_ONLY_HEADLINE`], [`READ_ONLY_REQUEST_HELP`]), `/mcp` serves it under the context's
//! MCP profile made read-only, and `GET /api/info` answers `readOnly: true` for it. Who may do
//! what stays the host's decision: the core never authenticates anyone.
//!
//! A host may confine every file path a client names (backup jobs, catalog loads, the
//! directories of Store forks and backups) to one directory (EXPERIMENTAL;
//! [`ContextBuilder::file_root`]); the jobs of a store it loads it checks with
//! [`jobs_outside_file_root`]. Without a root a client may name any path the process can write.
//!
//! # Extension points
//!
//! What a host adds goes through these points; each has the core default a host gets without
//! it. The core never grows several databases, authentication, quotas or bounded queues of its
//! own: those belong to the hosts. The run administration is the one administrative surface it
//! has (issue #32), off unless the host turns it on per context, guarded by an optional token.
//!
//! | Point | Core default | Where |
//! |---|---|---|
//! | Graph resolver: which hosted graph serves a request / an MCP connection | one graph | [`GraphResolver`], [`Server::ready_with`] ([`OneGraph`], [`Server::ready`]) |
//! | Run observer + cancel handle: every run's start and end, with a handle that cancels it | none | [`RunObserver`], [`Runs::with_observer`] |
//! | Request filter: sees each request before routing, refuses it, attaches an identity or marks it read-only ([`Request::mark_read_only`]) | none beyond the loopback/`Host`/`Origin` checks, which always run first | [`RequestFilter`], [`ContextBuilder::filter`] ([`HostCheck`]) |
//! | Extra routes: the `/api/` endpoints the core does not know | `404` | [`Hooks::route`], [`ContextBuilder::hooks`] |
//! | Activity / lifecycle hooks: ready, stopping, last activity, background work | none | [`LifecycleHooks`], [`Server::with_hooks`], [`Server::idle_for`] (requests), [`Context::background_runs`] (scheduled-job runs on the job workers) |
//! | Executor policy: how jobs reach the session | one session thread, unbounded queue | [`Runner`], [`JobQueue`] |
//! | Allow-list mode of the HTTP API: only `GET /api/info`, `GET /api/changes` and `POST /api/query` for the selected saved queries, under the host's access policy; everything else under `/api/` and `/mcp` is `403` | every endpoint | [`ContextBuilder::api_allow_list`], [`Context::is_allow_list`] |
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
//! [`ContextBuilder::ui_profile`],
//! [`Request::json_body`], [`Response::json`]): this crate is a JSON boundary like
//! `graphersal-session`, an exception to the engine's rule of no third-party types in the API.

#![warn(missing_docs)]

mod api;
mod context;
mod file_root;
mod host_check;
mod http;
mod job_workers;
mod launch;
mod mcp;
mod open;
mod read_only;
mod runs;
mod server;
mod ui;

#[cfg(all(test, feature = "mcp"))]
mod directory_tests;
#[cfg(test)]
mod extension_tests;
#[cfg(test)]
mod file_root_tests;
#[cfg(test)]
mod launch_tests;
#[cfg(test)]
mod mount_tests;
#[cfg(test)]
mod read_mark_tests;
#[cfg(test)]
mod read_only_tests;

pub use api::{DEFAULT_SERVER_NAME, PROTOCOL, handle};
pub use context::{
    Busy, BusyReason, Context, ContextBuilder, Hooks, Job, JobQueue, JobTicker, RequestFilter,
    Runner, SCHEDULED_JOBS_REFUSAL, SESSION_STACK_BYTES, ServeError, spawn_session,
    start_job_ticker,
};
pub use file_root::{JobOutsideFileRoot, jobs_outside_file_root};
pub use host_check::{Exposure, HostCheck, HostRules};
pub use http::{HttpError, MAX_BODY_BYTES, Method, Request, Response};
pub use launch::{Launched, ServedSource, launch};
#[cfg(feature = "mcp")]
pub use mcp::current_client;
pub use open::{
    NoteLevel, OpenError, OpenErrorKind, OpenNote, OpenStore, OpenedBase, OpenedLayerStore,
    OpenedStore, layer_base_of_store, open_layer_store, open_store,
};
pub use read_only::{READ_ONLY_HEADLINE, READ_ONLY_HELP, READ_ONLY_REQUEST_HELP};
pub use runs::{
    RUN_TEXT_HEAD_CHARS, Registered, RunEntry, RunHandle, RunInfo, RunKind, RunObserver, RunState,
    Runs,
};
pub use server::{GraphResolver, Lifecycle, LifecycleHooks, OneGraph, Server};
pub use ui::Ui;
