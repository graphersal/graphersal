# Release 0.8.0

A minor release for hosts. The EXPERIMENTAL hosting crates `graphersal-serve` and
`graphersal-mcp` get what a server needs once it knows who sends each request: MCP sessions
that belong to the identity that opened them and can survive a restart, directory selections
that follow a re-pointed or renamed alias, a read-only mark per request, a file root that keeps
client paths inside one directory, and presets a query may lower but not raise. The playground
sends a visitor to the host's login page when the host asks for a login. GQL counts after any
walk without building a row per match, and the knowledge base Docker image is built by one
Python script.

The public API of `graphersal` is unchanged apart from additions: `cargo semver-checks` against
0.7.1 reports no break of `graphersal`, `graphersal-session`, `graphersal-serve` and
`graphersal-storage-tests`. The breaks are in the EXPERIMENTAL crate `graphersal-mcp` (one API
break and changes of behaviour): read "Upgrading from 0.7.x" first. No format changes; stores,
snapshots, journals and backups of 0.7.x are read and written exactly as before. The
`graphersal --server` dev server answers as before.

## Upgrading from 0.7.x

**Stores and files.** A drop-in replacement: no format change, no upgrade step. 0.7.x reads
everything 0.8.0 writes.

**`graphersal-mcp` (EXPERIMENTAL, breaking).** These changes affect hosts built on the crate
and agents of a directory-mode endpoint. The dev server's MCP endpoint is unchanged: it serves
one graph and attaches no identity.

- **`use_graph` answers a new shape.** It answers `selected` (the name the agent asked for, a
  graph or an alias), `graph` (the graph that name resolves to now), `kind`, `state`,
  `readOnly` and `note`. The `requested` key is gone, and `selected` is now the name, not the
  route. `list_graphs` gains `selectedGraph` beside `selected`.
- **A directory selection follows its name.** A session that selected an alias follows it when
  the alias is re-pointed (before, it stayed on the graph resolved at `use_graph`). A removed
  name fails the session's calls with a message naming `list_graphs` and `use_graph` until a
  graph or alias of that name exists again.
- **A session belongs to its identity.** A request that names an MCP session id with another
  identity than the session's `initialize` (`McpRequest::with_identity`; none is anonymous, an
  identity of its own) gets the unknown-session `404`, and its `DELETE` ends nothing. Signed
  session ids minted for an identity differ from anonymous ones; anonymous ids are unchanged.
  `Mcp::verified_session_route(id)` now checks the id as anonymous; a host that attaches
  identities calls `Mcp::verified_session_route_for(id, identity)`.
- `Mcp` is no longer `UnwindSafe` and `RefUnwindSafe` (it may now hold an
  `Arc<dyn SessionStore>`). Code that moves an `Mcp` into `std::panic::catch_unwind` wraps it in
  `AssertUnwindSafe`.

**`graphersal-session` (internal).** `SessionPresets` gains the field `ceilings` (the struct is
`#[non_exhaustive]`, so this is additive).

## Hosting pieces for identity-aware servers (EXPERIMENTAL)

These additions to `graphersal-serve`, `graphersal-mcp` and the script engine let a host that
authenticates its callers serve them safely. Every one is off unless the host turns it on, so
the dev server and every host that does not use them behave as in 0.7.1. The book page
[Serving Layers](../for_developers/serving_layers.md) describes each one.

- **Directory selections follow aliases.** `use_graph` stores the name the agent asked for and
  the endpoint resolves it again on every call, as an HTTP path `/db/<name>/` is. A host that
  renames an alias calls `Mcp::rename_selection(old, new)` (it answers the number of sessions
  moved); each moved session's next tool answer starts with the line
  `note: alias "<old>" was renamed to "<new>"`
  ([directory mode](../for_developers/serving_layers.md#one-endpoint-over-all-graphs-directory-mode)).
- **Sessions belong to their identity.** Every MCP session is bound to the identity of its
  `initialize`, in table mode and with signed ids, with `Mcp::verified_session_route_for` for
  the host's resolver. A host whose graphs differ per caller builds its `GraphDirectory` per
  request and answers a hidden graph like an unknown one
  ([Sessions belong to their identity](../for_developers/serving_layers.md#sessions-belong-to-their-identity)).
- **Sessions survive a restart.** `Mcp::with_session_store(Arc<dyn SessionStore>)` loads the
  stored sessions once and saves the whole table after every change. Each row is a
  `StoredSession`: the SHA-256 of the id (the table never holds the plain id), the identity, the
  selected name, a pending rename note, the protocol revision, the client and the creation time.
  A failed save is a `tracing` warning, never a failed request. `Mcp::forget_sessions_of(identity)`
  ends one identity's sessions at once, for a revoked token or a removed user. Signed ids keep no
  table, so `with_session_store` refuses them
  ([Sessions survive a restart](../for_developers/serving_layers.md#sessions-survive-a-restart)).
- **Read-only requests.** `Request::mark_read_only()` (and `Request::is_read_only()`) marks one
  request as coming from a reader, in the host's router or `RequestFilter`. `/api/execute` and
  `/api/query` then run under the reading authorizer, every other writing route answers `409`
  (`READ_ONLY_REQUEST_HELP` names the reason), reads and downloads stay open, `/mcp` serves the
  request under the context's profile made read-only (the new `Profile::with_read_only`), and
  `/api/info` and `/api/changes` answer `readOnly: true` for it. The context stays writable for
  every other request
  ([Read-only requests](../for_developers/serving_layers.md#read-only-requests)).
- **File root.** `ContextBuilder::file_root(Some(root))` (`Context::file_root`) confines the paths
  a client names to one directory: backup job paths of `POST /api/jobs/define`, the jobs of a
  loaded catalog, and the `dir` of a Store fork, attic fork or backup. A relative path is relative
  to the root; one that leaves it (an absolute path elsewhere, `..`, a symbolic link pointing out)
  is refused. Jobs already stored in a store the host loads are checked by the host with
  `graphersal_serve::jobs_outside_file_root`, which answers a `JobOutsideFileRoot` per offending
  job. The default, no root, keeps the old behaviour
  ([File root](../for_developers/serving_layers.md#file-root)).
- **Presets as ceilings.** `graphersal::script::graph_scope_with_ceilings` and
  `engine_with_ceilings` (the same arguments as `graph_scope_with_options` and
  `engine_with_options`) make the preset limits ceilings: a query's `g.with(..)` may lower
  `evaluationTimeout`, `repeat.max_loops`, `bulk.max_expansion` and the `traversal.max_*` limits
  without asking the authorizer, and a larger value or no limit at all fails before anything runs
  with the new `TraverserError::OptionAboveCeiling` (a raise is refused, never clamped). The rule
  covers every source the script reaches and the saved queries it calls. A `graphersal-session`
  host sets `SessionPresets::with_ceilings(true)`; its presets then answer `ceilings: true`
  ([Presets as ceilings](../users_guide/resource_limits.md#presets-as-ceilings)).

## Playground

- **Login of a host** (EXPERIMENTAL). When a host answers an API request with `401` and names
  its login page in the body (`{"error": "...", "login": "/"}`), the playground goes to that page
  with `?next=` and its own path, when it starts (instead of falling back to the WebAssembly
  engine) and on every later request. Only a path of the page's own origin is followed, and the
  page never redirects to itself. The dev server and the WebAssembly playground never send it
  ([Login of a host](../users_guide/web_playground.md#login-of-a-host)).

## Performance

- **GQL `count(*)` after any walk is the lazy count.** An aggregation that only counts rows
  (`count(*)`, or `count(x)` of a variable that is never NULL, without a grouping key) became
  the fused `count()` only after a node scan or one plain hop; after any other walk (more hops,
  joins, `OPTIONAL MATCH`, `UNWIND`, `WITH`) it built a row per match. It is now the lazy count
  whatever comes before it. On the `large` graph,
  `MATCH (a)-->(b) WHERE b.id <> "x" RETURN count(*)` materializes 21.8 MB like the equivalent
  Gremlin `count()` (before: 33.5 MB), and `MATCH (a)--(b) RETURN count(*)` peaks at 8.5 MiB
  instead of 34.8 MiB ([Counts](../gql/plans.md#counts)).

## Docker

- **One build script for the knowledge base image.** `docker/prebuilt/build.py` (Python 3.9+,
  standard library only; Linux, macOS and Windows) replaces `fetch-binary.sh` and
  `prepare-kb.sh`. It takes the binary from a release archive checked against its `SHA256SUMS`,
  a plain Linux binary, or a GitHub Release or CI run (`--version`, cached for offline builds),
  and builds with Docker or Podman (`--engine`, `--push` for a multi-platform manifest). The
  image ships the prepared single-file Store as is: the build runs nothing on it, so prepare the
  store first (`store fork`, `store verify`, `mcp-cost`); a directory store is refused with a
  hint to fork it into a `.gstore`
  ([A knowledge base image](../persistence/docker.md#a-knowledge-base-image)).

