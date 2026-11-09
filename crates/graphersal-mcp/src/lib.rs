//! `graphersal-mcp`: the Model Context Protocol endpoint of Graphersal's servers (today
//! `graphersal --server --mcp`, the dev server of `graphersal-cli`, and its AI Chat), so an AI
//! agent works with the same graph and session as the browser.
//!
//! **Internal, EXPERIMENTAL, no stability guarantees.** Published because `graphersal-cli`
//! depends on it, and for hosts of Graphersal's own servers; its API changes with the servers in
//! any release (a patch release included): a crate that depends on it pins the exact version
//! (`graphersal-mcp = "=0.x.y"`) during 0.x.
//!
//! # Host boundary
//!
//! The crate depends on no HTTP library and on no way of running jobs:
//!
//! - **Transport**: the host's HTTP server turns a request to its MCP path into an
//!   [`McpRequest`] (method, headers, body), calls [`Mcp::handle`] and sends the
//!   [`McpResponse`] (status, headers, body) back; or the host serves a [`Connection`] over a
//!   byte stream ([`serve_lines`], see "Connections" below).
//! - **Jobs**: every tool is a thin wrapper of one [`Session`](graphersal_session::Session)
//!   operation; the host's [`Executor`] runs it (the dev server queues it on its session thread
//!   like the browser's calls, so the server's limits apply, the UI's Stop cancels a running MCP
//!   query too, and the change feed records MCP changes with the client `mcp`).
//! - **Host state**: [`Host`] answers what the tools read from the server besides the session
//!   (the read-only view the user opened, the process statistics, `graph_info`'s server fields,
//!   the playground's examples).
//!
//! The AI Chat calls the same tools in process: [`tool_listings`], [`call_tool`] (with a
//! [`ToolCaller`] carrying its own cancellation token), [`instructions`],
//! [`dsl_reference_text`] and [`gql_reference_text`] (it has no resources, so both references
//! go into its prompt).
//!
//! # Configuration
//!
//! The operator chooses what an agent sees with an [`McpConfig`] ([`Mcp::with_config`], the
//! CLI's `--mcp-config FILE`): the server name, the instructions (built-in, appended to,
//! replaced), the built-in tools offered and their descriptions, which saved queries are tools
//! of their own, the resources listed and the answer limit. There are no built-in profiles;
//! without a configuration the endpoint offers every tool of its mode with its built-in
//! description: a sentence or two and one example where it helps (the GQL support list is the
//! resource `graphersal://gql-reference`). [`Mcp::prompt_cost`] measures what the instructions
//! and the tool list cost an agent's context.
//!
//! # Profiles and hosts per connection
//!
//! What a client may do is a [`Profile`]: an [`McpConfig`], the read-only flag, the answer limit
//! and an optional `graphersal::auth::AccessPolicy` that the engine enforces on every query run
//! (once per execution, before the optimizer: a refused write is a tool error naming
//! `Permission denied`). A policy that allows reading plus exactly the data writes of some saved
//! queries' inferred effects is a `use`-like profile: the agent runs those queries and writes
//! nothing else (a policy matches action and resource kinds, not labels); the session's own
//! writing tools (`set_schema`, the index and compression tools, ...) are offered only where the
//! policy allows their writes. The endpoint has a default profile ([`Mcp::with_profile`], what
//! [`Mcp::new`] and [`Mcp::with_config`] set); the HOST picks another for a connection
//! ([`Connection::with_profile`]) or an HTTP request ([`Mcp::handle_with_profile`]), from what
//! it knows (its handshake, the graph its resolver picked, the identity its request filter
//! attached), never from anything the client says inside MCP. A connection keeps its profile
//! for its life.
//!
//! Each [`Connection`] carries its own [`Host`] (and so its own graph and session), and an
//! HTTP request is answered with the host the server passes; one [`Mcp`] serves them all, its
//! statistics, MCP sessions, token, stream registry and tool catalog code shared by the
//! process. An identity the host established ([`Connection::with_identity`],
//! [`McpRequest::with_identity`]) travels with the session jobs ([`ClientRef::identity`]).
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
//! # Connections
//!
//! The protocol works at the message level: a [`Connection`] is one MCP session (whether and
//! how it initialized: the protocol version, the client's info and capabilities) over the host
//! it was built with; [`Connection::handle`] answers one JSON-RPC message with the lines to send
//! back, and the server's own messages go to the connection's outbound channel. [`serve_lines`]
//! serves a connection over newline-delimited JSON-RPC (MCP's stdio framing) on any
//! `BufRead`/`Write` pair with blocking std I/O: a plain stdio server, or a socket after a host's
//! own handshake. The HTTP path is an adapter over the same message handling: it maps the
//! `Mcp-Session-Id` (table or signed ids) to the session and frames the outcome as HTTP; its
//! answers are the stream's byte for byte.
//!
//! Only a stream carries the server's pushes (HTTP has no stream here, and none is planned):
//! `notifications/tools/list_changed` when saved queries are tools of their own and a tool call
//! of any client (or [`Mcp::check_tools_changed`]) changed the listing a stream client last got,
//! announced in its `initialize` as `tools.listChanged: true` (HTTP keeps `false`); and
//! `notifications/progress` while a `tools/call` runs whose request carried
//! `_meta.progressToken` (a heartbeat: the engine reports no finer progress). Cancellation
//! (`notifications/cancelled`) works on both paths through the host's runs registry
//! ([`Host::cancel_request`], keyed by the session id or the [`Connection::key`]); the end of
//! a stream cancels its running requests.
//!
//! Hand-written JSON-RPC rather than the `rmcp` SDK: `rmcp` needs the tokio runtime and an async
//! HTTP stack (axum/hyper), dozens of crates for an endpoint whose whole protocol surface is
//! seven methods.
//!
//! # Tools
//!
//! A read-only endpoint lists only the reading tools and runs `query` and `gql` under
//! [`read_only_authorizer`](graphersal_session::read_only_authorizer) (creating, updating,
//! deleting data, the schema or a catalog definition is denied); a profile with an access
//! policy runs them under that policy instead (read-only: without its writes).
//!
//! `get_schema` / `infer_schema` answer the schema summary without the stored schema's
//! descriptions by default (issue #34: about a third of a large graph's summary, written to the
//! agent's prompt cache in every conversation); `descriptions: true` and the resource
//! `graphersal://schema-summary` give the full summary.
//!
//! Saved queries (`book/src/users_guide/saved_queries.md`): `list_saved_queries` and
//! `run_saved_query` are always offered, with the resource `graphersal://saved-queries`; the
//! instructions and the `query` tool tell the agent to look for a saved query before writing a
//! new one, and to run one marked `writes: true` (it changes data, its `effects` say what) only
//! when the user asked for that change. Read-only mode lists only the queries that do not write
//! and refuses a call of one that does. With query tools every saved query is also a tool of
//! its own (name, description, `inputSchema` from the parameter schemas, `readOnlyHint` /
//! `destructiveHint` from its effects); `tools/list` reflects the catalog at the time of the
//! call. Over HTTP the endpoint has no SSE stream, so it cannot send
//! `notifications/tools/list_changed` (`listChanged: false`): a client re-lists the tools to
//! see a changed catalog. A stream [`Connection`] gets the notification.
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
//! Usage statistics ([`McpStats`], [`Mcp::stats`]): counters since the process started, in
//! memory only (nothing is written anywhere; a restart starts from zero), cheap under parallel
//! load (atomics; read locks to find a name's entry): requests per JSON-RPC method, tool calls
//! per tool and per saved query (calls, errors, total and largest latency, a latency
//! histogram, answer characters), requests per client name (at most [`MAX_CLIENT_NAMES`]),
//! sessions initialized and active in about the last five minutes (an estimate), refusals by
//! kind ([`Refusal`]; the host reports its own). The host serves [`McpStats::to_json`] (the dev
//! server: `GET /api/mcp/stats`); the tool `usage_statistics` (reading) answers the same, but
//! only when the configuration's `tools.include` names it (never in the default listing).
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
mod connection;
mod host;
mod profile;
mod protocol;
mod resources;
mod sessions;
mod stats;
mod stream;
mod tools;
mod transport;

#[cfg(test)]
mod tests;

pub use config::{MAX_INSTRUCTIONS_CHARS, MIN_ANSWER_CHARS, McpConfig, McpConfigError};
pub use connection::{Connection, DEFAULT_PROGRESS_INTERVAL};
pub use host::{ClientRef, Executor, Host, Job, McpError};
pub use profile::Profile;
pub use protocol::{
    MAX_TEXT_CHARS, Mcp, PROTOCOL_VERSIONS, PromptCost, dsl_reference_text, instructions,
};
pub use resources::gql_reference_text;
pub use sessions::{MAX_CLIENT_NAMES, SessionKey};
pub use stats::{LATENCY_BUCKETS_MS, MAX_COUNTED_NAMES, McpStats, Refusal};
pub use stream::serve_lines;
pub use tools::{CallRefused, ToolCaller, ToolOutcome, call_tool, tool_listings};
pub use transport::{McpRequest, McpResponse};
