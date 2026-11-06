//! `graphersal-mcp`: the Model Context Protocol endpoint of Graphersal's servers (today
//! `graphersal --server --mcp`, the dev server of `graphersal-cli`, and its AI Chat), so an AI
//! agent works with the same graph and session as the browser.
//!
//! **Internal, EXPERIMENTAL, no stability guarantees.** Published only because `graphersal-cli`
//! depends on it; its API changes with the servers in any release.
//!
//! # Host boundary
//!
//! The crate depends on no HTTP library and on no way of running jobs:
//!
//! - **Transport**: the host's HTTP server turns a request to its MCP path into an
//!   [`McpRequest`] (method, headers, body), calls [`Mcp::handle`] and sends the
//!   [`McpResponse`] (status, headers, body) back.
//! - **Jobs**: every tool is a thin wrapper of one [`Session`](graphersal_session::Session)
//!   operation; the host's [`Executor`] runs it (the dev server queues it on its session thread
//!   like the browser's calls, so the server's limits apply, the UI's Stop cancels a running MCP
//!   query too, and the change feed records MCP changes with the client `mcp`).
//! - **Host state**: [`Host`] answers what the tools read from the server besides the session
//!   (the read-only view the user opened, the process statistics, `graph_info`'s server fields,
//!   the playground's examples).
//!
//! The AI Chat calls the same tools in process: [`tool_listings`], [`call_tool`] (with a
//! [`ToolCaller`] carrying its own cancellation token), [`instructions`] and
//! [`dsl_reference_text`].
//!
//! # Configuration
//!
//! The operator chooses what an agent sees with an [`McpConfig`] ([`Mcp::with_config`], the
//! CLI's `--mcp-config FILE`): the server name, the instructions (built-in, appended to,
//! replaced), full or short ([`Descriptions`]) tool descriptions, the built-in tools offered
//! and their descriptions, which saved queries are tools of their own, the resources listed and
//! the answer limit. There are no built-in profiles; without a configuration the endpoint
//! offers every tool of its mode with its full description. [`Mcp::prompt_cost`] measures what
//! the instructions and the tool list cost an agent's context.
//!
//! # Protocol
//!
//! MCP Streamable HTTP of the initialize-based protocol revisions (`2025-03-26` to
//! `2025-11-25`, [`PROTOCOL_VERSIONS`]), with plain JSON answers (no SSE stream): every
//! JSON-RPC request is one `POST` answered with one `application/json` object; a notification
//! or a response is answered `202 Accepted` without a body; `GET` (a stream the server would
//! push on) is `405`; `DELETE` ends the MCP session. `initialize` assigns an `Mcp-Session-Id`
//! (random, 128 bits); a request naming an unknown one is `404` (the client then initializes
//! again, e.g. after a restart of the server); a request without one is served too. A request
//! whose `MCP-Protocol-Version` header names a revision this server does not speak (a
//! stateless, newer client) is `400` with a plain JSON-RPC error, which tells a dual-era client
//! to fall back to `initialize`. With a token ([`Mcp::new`]) a request needs `Authorization:
//! Bearer <token>` (`401` otherwise). The host's own checks (`Host`/`Origin`, the bind address)
//! come before [`Mcp::handle`].
//!
//! Hand-written JSON-RPC rather than the `rmcp` SDK: `rmcp` needs the tokio runtime and an async
//! HTTP stack (axum/hyper), dozens of crates for an endpoint whose whole protocol surface is
//! seven methods.
//!
//! # Tools
//!
//! A read-only endpoint lists only the reading tools and runs `query` and `gql` under
//! [`read_only_authorizer`](graphersal_session::read_only_authorizer) (creating, updating,
//! deleting data, the schema or a catalog definition is denied).
//!
//! Saved queries (`book/src/users_guide/saved_queries.md`): `list_saved_queries` and
//! `run_saved_query` are always offered, with the resource `graphersal://saved-queries`; the
//! instructions and the `query` tool tell the agent to look for a saved query before writing a
//! new one, and to run one marked `writes: true` (it changes data, its `effects` say what) only
//! when the user asked for that change. Read-only mode lists only the queries that do not write
//! and refuses a call of one that does. With query tools every saved query is also a tool of
//! its own (name, description, `inputSchema` from the parameter schemas, `readOnlyHint` /
//! `destructiveHint` from its effects); `tools/list` reflects the catalog at the time of the
//! call. The endpoint has no SSE stream, so it cannot send `notifications/tools/list_changed`
//! (`listChanged: false`): a client re-lists the tools to see a changed catalog.
//!
//! The endpoint works with the in-memory GRAPH only: it has no persistence tools (no
//! checkpoint, mark, view, rollback, attic, backup or compaction; fewer tools keep an agent's
//! context small). Those stay the user's, in the browser's Store menu. When the user opens a
//! read-only view of a past state there, the view is server-wide: `graph_info` carries a `note`
//! saying so, and every tool error carries it too (a write is refused).
//!
//! `memory_report` (reading, also in read-only mode) answers the session's memory report
//! (`Session::memory_report`), at most 50 property rows and 50 label rows unless `limit`
//! (both lists) or `max_rows`/`max_labels` (one list each) say otherwise; the rows left out are
//! counted with their bytes (`rows_omitted`, `labels_omitted`, ...), the totals cover everything.
//! `workload_statistics` (reading) answers the workload statistics of a store's graph
//! (`Session::workload_report`): the queries, properties, lock times and jobs counted, at most
//! 50 rows per list (most total time first) unless `max_rows` says otherwise. `list_jobs`
//! (reading) answers the scheduled jobs (`Session::jobs_view`); defining, switching or running
//! a job stays the user's. `advise` (reading) answers the storage advisor's recommendations
//! (`Session::advise`), each with the command applying it through the catalog tools, and their
//! `groups`; `apply_recommendations` (writing, refused in read-only mode) applies the ones the
//! user picked by id in one commit (`Session::apply_recommendations`). `journal` (reading)
//! answers one page of the graph's system journal (`Session::journal`, at most 100 records
//! unless `limit` says otherwise).
//!
//! Compression rules (`book/src/users_guide/compressed_properties.md`): `list_compressions` and
//! `estimate_compression` (both reading, also in read-only mode: an estimate changes nothing),
//! `define_compression`, `drop_compression` and `recompress` (refused in read-only mode) wrap
//! the session's `compression` operations; a define or drop is a commit the host's change feed
//! records with the client `mcp`.
//!
//! Property indexes (`book/src/users_guide/property_indexes.md`): `list_indexes` (also in
//! read-only mode), `create_index` and `drop_index` (refused in read-only mode, where the
//! `query` tool's authorizer denies `g.create_index`/`g.drop_index` too) wrap the session's
//! `indexes` operations; a create or drop is a commit.

#![warn(missing_docs)]

mod answer;
mod catalog;
mod config;
mod host;
mod protocol;
mod resources;
mod sessions;
mod tools;
mod transport;

#[cfg(test)]
mod tests;

pub use config::{
    Descriptions, MAX_INSTRUCTIONS_CHARS, MIN_ANSWER_CHARS, McpConfig, McpConfigError,
};
pub use host::{ClientRef, Executor, Host, Job, McpError};
pub use protocol::{
    MAX_TEXT_CHARS, Mcp, PROTOCOL_VERSIONS, PromptCost, dsl_reference_text, instructions,
};
pub use tools::{CallRefused, ToolCaller, ToolOutcome, call_tool, tool_listings};
pub use sessions::{MAX_CLIENT_NAMES, SessionKey};
pub use transport::{McpRequest, McpResponse};
