# Dev Server and MCP

> **DEV mode, unstable, local only, temporary. Not for production.** The dev server is a way to try Graphersal "as a
> small server" and watch how it behaves. Its HTTP API is internal to the playground and changes
> without notice; it has no authentication and by default serves only the loopback (`127.0.0.1`,
> and `[::1]` when the system has IPv6; inside a container `0.0.0.0`, see
> [Listening address and Docker](#listening-address-and-docker)). Do not expose it to a network
> you do not trust.

`graphersal --server` serves the [Web Playground](../users_guide/web_playground.md) UI over
HTTP, with the CLI process itself as the engine instead of the WebAssembly worker. It is the same single-page app:
the page asks the serving origin `GET /api/info` at start, and when a Graphersal dev server
answers, every engine call goes to it over HTTP. Served from GitHub Pages or any static file
server, the page stays the WebAssembly playground.

## Start it

```bash
cargo run -p graphersal-cli -- --graph modern --server            # or: graphersal --graph ... --server
# Graphersal dev server: http://127.0.0.1:8080/
graphersal --graph ./mygraph.json --server --port 9000            # a GraphSON or GraphML file
graphersal --graph large --server --port 0                        # 0 picks a free port
graphersal --graph data/ --server                                 # a Store: every commit is durable
graphersal --graph new/ --create-store --server                   # create the store when missing
graphersal --graph g.graphml --schema schema.json --server        # a graph file loaded against its schema
```

- `--graph` takes the same sources as the CLI: `modern` (default), `empty`, `large`, `tree`, a
  GraphSON (`.json`, `.jsonl`, `.graphson`) or GraphML (`.xml`, `.graphml`) file, a packed
  snapshot (`.gsnap`), or a **Store** (a directory or a `.gstore` file; see
  [On a Store](#on-a-store)).
- `--port` (default `8080`; `0` picks a free one). The address is the only line on stdout; the
  "dev mode, unstable, local only" banner goes to stderr.
- `--host <ADDR>` (default `127.0.0.1`, inside a container `0.0.0.0`): the address to listen on,
  see [Listening address and Docker](#listening-address-and-docker).
- `--read-only`: every write is refused (queries, schema, catalog, "Save to file"; MCP offers
  only its reading tools) and a Store is loaded without its lock and without writing anything,
  so a read-only file or file system works ([Running in Docker](docker.md#read-only)). A Store
  the process may not write is served that way anyway, with a note.
- `GET /healthz` (liveness) and `GET /readyz` (readiness: 503 while the graph loads or the
  server stops) answer without a lock and under any `Host`, for container health checks;
  `graphersal health [--ready]` probes them ([Health checks](docker.md#health-checks)). The
  server listens before the graph loads and answers every other request with 503 until then.
- Ctrl+C or SIGTERM stops the server: the running query is cancelled, a Store is closed
  cleanly, the exit code is 0 ([Stopping](docker.md#stopping)).
- The playground UI is **embedded in the binary** (its static files: `index.html`, `css/`, `js/`,
  `assets/`, `reset/`), so `graphersal` serves it on its own, from any working directory.
  `--ui-dir <DIR>` names a directory whose files win over the embedded copy (for working on the
  UI); a file missing there comes from the embedded copy. Without `--ui-dir`, `./playground` is
  used that way when it exists (a repository checkout), else the embedded UI alone. An explicit
  `--ui-dir` that does not exist is a usage error. The stderr banner says where the UI comes from:
  `UI: embedded` or `UI: <dir> (embedded fallback)` (in its Server section). A path leaving the UI root (`..`) is a 404.
- After the graph is in memory, stderr shows **what was loaded and how the server runs**, in
  sections under the warning box, one `key: value` row per fact (long values wrap under their
  column; the `claude mcp add` command never wraps, so it copies in one piece):
  - **Store** (a Store only): its path, the commit and the number of snapshots, its state
    (durable, a read-only backup, or maintenance mode), and the load time (snapshot load + WAL
    replay, with the number of WAL commits replayed);
  - **Graph**: for a sample or a file its source and load time; the vertex and edge totals, the
    ten largest vertex and edge labels (`+N more` for the rest), the graph's memory (structure,
    indexes, properties, and the [property indexes](../users_guide/property_indexes.md) when
    there are any), the memory of its [workload statistics](workload_statistics.md)
    (`0 B` until queries ran, unless a Store brought its saved ones), the loaded embedding models
    (when there are any) and the process heap, and the compressed and deduplicated values when there
    are any (the same data as the playground's Profile -> Statistics tab);
  - **Embedding models** (when there are any): every model of the database besides the built-in
    `hash-ngram/1` (imported into the Store, or carried by a `.gsnap` file), by its repository
    (the name queries call it by) with its licence and whether it is the default, a note when the
    database holds models but none is the default, and every model the schema names that the
    database lacks ([Embedding Models in a Store](store_models.md)); a note keeps the layout of
    the CLI's notes (its headline in the row, one detail per line below it, each command on a
    line of its own under its `# comment`, never wrapped);
  - **Server**: the limits of every query, where the UI comes from, the MCP endpoint and the
    command that connects Claude Code, the AI Chat, and the addresses it listens on.

  ```text
  ── Store ─────────────────────────────────────────────────────────────────────
    path:      data
    commit:    7021, 1 snapshot
    state:     durable: every commit is in the write-ahead log before the query
               answers; Ctrl+C stops the server and closes the store cleanly
    loaded:    2.02 s (snapshot, no WAL commits to replay)

  ── Graph ─────────────────────────────────────────────────────────────────────
    size:      193,947 vertices, 923,752 edges
    vertices:  45 labels: Method 91,098, Field 39,155, Type 16,306, DocChunk
               13,018, File 12,814, Bean 3,293, LogMessage 3,129, ConfigKey 2,989,
               Package 2,602, ExternalArtifact 1,278, +35 more
    edges:     78 labels: CALLS 251,991, USES_TYPE 142,362, DECLARES 122,073, ...
    memory:    graph 703.2 MiB (structure 144.7 MiB, indexes 64.0 MiB, properties
               486.8 MiB, property indexes 7.6 MiB)
               workload statistics 182.4 KiB
               model 506.4 MiB (minishlab/potion-multilingual-128M 506.4 MiB)
               process heap 1.6 GiB
               process 47 % of 3.4 GiB (cgroup)

  ── Embedding models ──────────────────────────────────────────────────────────
    model:     minishlab/potion-multilingual-128M (the default), licence MIT
               stored in data; loaded at its first use
               model2vec/minishlab/potion-multilingual-128M@<12 hex>

  ── Server ────────────────────────────────────────────────────────────────────
    limits:    none (start with --timeout, --safe-limits, --max-* to bound
               queries; the process memory ceiling applies, --memory-limit)
    UI:        embedded
    MCP:       http://127.0.0.1:9010/mcp
    connect:   claude mcp add --transport http graphersal http://127.0.0.1:9010/mcp
    listening: http://127.0.0.1:9010/ and http://[::1]:9010/ (IPv6 loopback)
  ```
- The **limit options** of the CLI apply to every query of the server (see [Limits](#limits)).
  The display options (`--format`, `--max-rows`, `--max-items`, `--no-limit`, `--spelling`) are
  refused with a usage error, because the UI sets the display per query; `-e`/`--in` cannot be
  combined with `--server` either.
- **Stop** the server with Ctrl+C (SIGINT) or SIGTERM: a running query is cancelled (it rolls
  back) and an open store is closed cleanly; the exit code is 0. A second Ctrl+C exits at once.

The server needs no WebAssembly build: `playground/pkg/` is not used in this mode.

### Arguments from a file

`--args-file FILE` (or `@FILE`) reads arguments from a JSON file, so a server with many flags
starts with one. The keys are the long flag names without dashes:

```json
{
  "_comment": "keys starting with _ are comments",
  "graph": "data/",
  "server": true,
  "port": 8080,
  "mcp": true,
  "mcp-read-only": true,
  "mcp-instructions": "operator-notes.md",
  "timeout": 30000
}
```

```bash
graphersal --args-file args.json                 # everything from the file
graphersal @args.json --port 9000                # the command line wins: port 9000
graphersal --args-file args.json -e 'g.v().count()'   # a repeatable flag adds to the file's
```

- A switch (`server`, `mcp`, `no-limit`, ...) is `true`; `false` is the same as leaving it out.
  A number is written as a number (`"port": 8080`), a string as a string. `eval` repeats: a string
  or an array of strings. Every flag of `graphersal --help` is a key except
  `help`, `version` and `args-file`.
- **Relative paths** (`graph`, `schema`, `ui-dir`, `ai`, `mcp-instructions`, `mcp-config`) resolve against the
  directory of the args file, not the working directory. The sample names (`modern`, `empty`,
  `large`, `tree`) stay as written.
- **The command line wins.** The file's arguments come first, as if typed before the others: a flag
  given on the command line replaces the file's, a repeatable flag (`--eval`) adds to it. The
  usual validation runs on the merged arguments, so a file with `"server": true` plus
  `-e` on the command line fails as `--server -e` does.
- A file may not name another args file, and `--args-file` may appear once. An unreadable file,
  invalid JSON, a value of the wrong type or an unknown key is a usage error (exit code 2) that
  names the key; values are never echoed. Unlike the [`--ai` file](#the-configuration-file),
  where an unknown key only warns, an unknown key here is an error (a near-typo adds
  `did you mean "server"?`): a flag that is silently dropped could switch off `mcp-read-only`
  or `mcp-token`. Only a key starting with `_` is a silent comment. The keys `embedder` and `default-embedder`
  of earlier releases are refused with the import command (0.5.0 removed the flags).

The file holds no secrets by design (give `--mcp-token` on the command line or keep the file
private); the repository has an example, `crates/graphersal-cli/examples/args-dev-server.json`.

## Limits

The limit options become **session presets**: every query sent to the server runs under them, as
every script of the one-shot runner does (the [resource limits](../users_guide/resource_limits.md)
page explains each limit).

| Option | What every query gets |
|---|---|
| `--timeout <MS>` | `evaluationTimeout` on every traversal, and the budget of a query as a whole (a `loop {}` between traversals stops too, with "Script exceeded evaluationTimeout of N ms") |
| `--memory-limit <BYTES\|auto\|none>`, `--memory-headroom <BYTES>` | the process memory ceiling (`memory.limit`, on by default: the cgroup limit minus the headroom, else 80 % of RAM): a query, import or write that would take the server's live heap over it fails with "Resource limit 'memory.limit'" and rolls back, instead of the process being killed; a `--graph` store or file whose graph does not fit refuses to load (exit 1, the error names the minimum). One setting for the whole server process, not a session preset |
| `--max-traversers <N>` | `traversal.max_traversers` |
| `--max-materialized-bytes <N>` | `traversal.max_materialized_bytes`, the per-query memory limit (`0` is unlimited) |
| `--max-string-size <N>` | the script's string limit and `traversal.max_string_bytes` |
| `--max-value-depth <N>` | the script's value depth and `traversal.max_value_depth` |
| `--max-operations`, `--max-array-size`, `--max-map-size` | the Rhai limits of the server's script engine |
| `--safe-limits` | the library's SAFE script limits and the engine's traversal defaults |

A query overrides a traversal option with `g.with(..)` (for example
`g.with("evaluationTimeout", 0)`), as in the CLI. The values in force are listed on stderr at
start (the `limits` row of the Server section: "of every query: ...", or "none (...)" without
limit options) and in
`GET /api/info` (`presets`). Without limit options the
server runs like the playground: no script limits, the engine's traversal defaults, no time
budget; Stop is the way to end a runaway query.

```bash
graphersal --graph large --server --timeout 5000 --memory-limit 4000000000
```

## Read-only serving mode

`--server --read-only` is the **read-only serving mode**: the data cannot change, so the server
runs the queries of many browser pages and MCP clients **in parallel** instead of one after the
other. It is meant for a knowledge base served to many agents and readers (a Docker image, a
shared machine).

**The server has no authentication.** Whoever reaches its port can read the whole graph and run
any reading query. What protects it is that nothing can be written (every write is refused, the
database file is never touched) and the limits below; keep the port on a network you trust or
behind a reverse proxy that authenticates.

```bash
graphersal --graph kb.gstore --read-only --server --host 0.0.0.0 --port 8080 \
  --mcp --mcp-read-only --workers 8
```

How it works:

- **Workers**: `--workers N` threads (default: the machine's available parallelism) each own a
  session over the SAME graph in memory (one copy of the data, one copy of each embedding
  model). Every query of a page, of an MCP client and of the AI Chat goes to them through one
  queue.
- **Bounded queue**: at most `--queue N` jobs wait for a worker (default 4 x workers). A request
  that finds the queue full, or that waited longer than `--queue-timeout S` seconds (default 10)
  for a worker, is answered **`503` with `Retry-After: 1`**, so the client retries it a moment later (the
  playground shows the error; run the query again).
- **Fairness**: one client (an MCP session, a browser page) has at most
  `--max-inflight-per-client N` queries running or waiting (default 4). A further one waits for
  a slot of its own client, up to the queue timeout, then gets `503`: one looping agent cannot
  take every worker.
- **Connections**: at most `--max-connections N` requests are handled at once (default 512, a
  thread each); beyond it the server answers `503` at once. The health probes `/healthz` and
  `/readyz` are always answered.
- **Stop**: the Stop button of a page cancels only that page's query, never another page's or an
  agent's, also while the query still waits in the queue.
- **Graceful stop**: on SIGTERM (`docker stop`, Kubernetes) `/readyz` turns `503`, no new query
  is taken, the running ones get `--drain-timeout S` seconds (default 20) to finish, then the
  waiting ones are refused, every query is cancelled (also an AI Chat turn's) and the process
  exits with code 0. A query that has not stopped 2 s later is named in a warning on stderr.
- **Safe defaults**: unlike the read-write server (which runs like the playground, without
  limits), every query runs under the SAFE script limits (`--safe-limits`), a 30 s timeout
  (`--timeout 30000`) and a 500 MiB per-query memory limit
  (`--max-materialized-bytes 524288000`). Each flag still overrides its own default
  (`--timeout 0` turns the timeout off, `--max-operations 0` lifts the operation limit).
- **MCP sessions** are stateless: the session id `initialize` answers is signed (HMAC-SHA256)
  instead of being kept in a table, so any number of agents stay connected and none evicts
  another. The key is random per process (after a restart the agents initialize again), or the
  value of the environment variable `GRAPHERSAL_MCP_SESSION_KEY` (at least 32 characters, never
  printed): several replicas behind a load balancer with the same key accept each other's
  session ids.

What is not available in this mode: the Store panel, views of past states, rollbacks and
history (a Store is opened read-only, as a graph); "Save to file"; scheduled jobs (defining,
switching or running one answers `409`); and every write (queries, schema, saved queries,
indexes, compression rules), which the graph refuses as with `--read-only` alone. The change
feed stays empty: nothing changes. Force layouts of the Graph panel work; their jobs are shared
by all pages.

`GET /api/info` adds `serving`: the pool's settings, the queries running and waiting, and the
requests refused so far (`refused.queueFull`, `expired`, `clientBusy`, `stopping`). The start
banner's `serving` row shows the settings. How agents use the MCP endpoint (tools, saved
queries, clients, refusals of MCP requests) is in [MCP usage statistics](#mcp-usage-statistics).

The new flags need `--server --read-only` (`--max-connections` only `--server`: on a
read-write server it caps the requests too, without a default); like every flag they can be
keys of an [arguments file](#arguments-from-a-file) (`"workers": 8`).

## Embedding models

The server serves the embedding models of its database: the models imported into the `--graph`
Store, or carried by a `--graph` `.gsnap` file; every query of the server, the MCP `query` tool
and the AI Chat's tools use them (`embed()`, the `embed` step, text queries, vector rules,
`g.embedders()`). `GET /api/info` lists them as `embedders`. The models the schema needs are
loaded when the store opens, the others at their first use. Running a model is local; only
`graphersal embed-model download` connects to Hugging Face
([Embeddings](../getting_started/embeddings.md#real-models-model2vec)).

```bash
graphersal embed-model download minishlab/potion-base-8M
graphersal embed-model import minishlab/potion-base-8M --into data/ --default   # the store closed
graphersal --graph data/ --server --mcp
```

The import into a closed store appends one commit without loading the graph; while the server
has the store open the import is refused (`Locked`): stop the server first. Changed in 0.5.0:
`--embedder` and `--default-embedder` are removed ([Embedding Models in a
Store](store_models.md)).

## What the UI does in this mode

- A red **DEV SERVER · unstable** badge in the top bar. There is no start page and no graph choice:
  the server serves the one graph it was started with. To use another graph, restart the server.
- Queries, results, profiles, the graph drawing, the schema views and the schema editor work as
  in the WebAssembly playground: both run the same session code (the internal crate
  `graphersal-session`), so the answers are the same structures. The **Examples** menu is not
  shown: its queries are written for the sample graphs, and the server serves its own.
- The **Statistics** tab of the Profile panel shows the limits the server applies (its presets,
  from the options above) beside the engine's defaults for the rest, next to the graph's counts and
  memory footprint.
- Several browser tabs share the one graph. Queries run one after the other. A change made in
  another tab (or by an AI agent, see [MCP](#mcp-an-ai-agent-on-the-same-graph) and
  [AI Chat](#ai-chat)) reloads the
  page by itself within about 2 seconds and shows "changed: commit N". While the graph changes
  fast (a bulk load: changes from elsewhere in three polls within 10 seconds), the page reloads at
  most every 30 seconds instead and a banner says so, with **Refresh now** and **Pause auto
  refresh** (no automatic reloads until **Resume**); 10 seconds after the last change it reloads
  once more and goes back to normal.
- **Stop** cancels the query this tab started on the server: it stops at its next check (where
  `evaluationTimeout` is checked, plus the script loop), fails with "cancelled", and everything it
  changed is rolled back; a query that still waits for the server (behind another tab's query)
  runs nothing. The graph stays as it was before the query; nothing restarts. Stop never cancels
  another tab's query, an agent's query over MCP or an AI Chat turn's query (the chat has its own
  Stop).
- **Save ▾** downloads the graph data (GraphSON or GraphML), the schema (JSON) or a snapshot, one
  file per entry, as in the playground. **Save to file** appears when the server was started with a graph file: it writes
  the graph back to that file in its format (GraphSON, GraphML or a packed snapshot), atomically (a
  temporary file next to it, then a rename). The stored schema is not part of a GraphSON or GraphML
  file: with `--schema FILE` Save to file writes the schema back to that file too (atomically, both
  temporary files are written before either is renamed; a cleared schema is written as `{}`).
  Without `--schema`, saving a graph that has a schema shows a warning (*the schema is not part of a
  GraphML/GraphSON file: download it with Save schema, or start the server with --schema FILE*);
  no file is ever written beside the graph implicitly. A snapshot (`.gsnap`) is lossless: schema and
  saved queries included.
- Without a Store directory nothing is saved automatically, and nothing survives a restart of the
  server. On a Store directory every commit is persisted (below).

## Listening address and Docker

Without `--host` the server binds the loopback, `127.0.0.1` and, when the system has IPv6, `[::1]`
on the same port (the `listening` row on stderr then names both addresses; if that bind fails the
server stays on IPv4, silently). Only programs on the same machine reach it.

Inside a container the loopback is useless: a published port (`docker run -p 8080:8080`) arrives
on the container's network interface. So when the server finds `/.dockerenv` (Docker),
`/run/.containerenv` (Podman) or the variable `KUBERNETES_SERVICE_HOST` (a Kubernetes pod), its
default is `0.0.0.0`, every interface of the container, and `docker run -p` works without flags.
The repository's `Dockerfile`, health checks, read-only serving, SIGTERM and Kubernetes probes:
[Running in Docker](docker.md).

```bash
# an image that has the graphersal binary; the Store lives in a volume
docker run --rm -p 8080:8080 -v "$PWD/data:/data" my-graphersal-image \
  graphersal --graph /data --create-store --server --port 8080
# the same with the address spelled out (it also works where the container is not detected)
docker run --rm -p 8080:8080 my-graphersal-image \
  graphersal --graph modern --server --port 8080 --host 0.0.0.0
# then open http://localhost:8080/ on the host
```

`docker run -p 127.0.0.1:8080:8080 ...` publishes the port on the host's loopback only, which
keeps the server private to the host machine.

`--host <ADDR>` names the address explicitly, on any machine: an IP address (`0.0.0.0` every
IPv4 interface, `::` every IPv6 interface, `192.168.1.5` one interface, `127.0.0.1` the loopback,
which also overrides the container default) or a host name that resolves on the machine
(`localhost` is the loopback default). An address that is not valid or does not resolve is an
invocation error (exit code `2`). For a wildcard address the URL on stdout is
`http://127.0.0.1:<port>/` (`http://[::1]:<port>/` for `::`), the one a browser on the same
machine opens, and the `listening` row adds `(all interfaces: 0.0.0.0:<port>)`; otherwise it is
the address or name given.

> **Warning.** On any address other than a loopback one the server is **reachable from the
> network and has no authentication**: whoever reaches the port can read and change the graph,
> write Store checkpoints and backups, and, with `--ai`, spend the AI provider key. The banner
> says so, and a one-line warning on stderr names the address. Publish the port only to networks
> you trust, and use `--mcp-token` for the MCP endpoint.

## Safety

The server is a local development tool: it binds the loopback unless `--host` or a container
says otherwise ([above](#listening-address-and-docker)). It has no authentication, and checks
the `Host` and `Origin` headers of every request against DNS rebinding and cross-site requests:

- On the loopback, `Host` must name the server itself (`127.0.0.1`, `localhost` or `[::1]`, or the
  `--host` name, with the server's port), and an `Origin` must be one of them.
- On any other address the port of `Host` is not checked (a container's port mapping changes it)
  and its name must be an IP address, `localhost` (or a `*.localhost` name), a name without a dot
  (a machine name, a Docker Compose service name such as `http://graphersal:8080`), or the
  `--host` name. Any other fully qualified name is refused (`403`): a DNS rebinding attack sends
  the attacker's domain. To open the server by a domain name, start it with `--host <that name>`.
  An `Origin` must be the request's own origin (`http://` followed by its `Host`), so a page of
  another site is refused even when it names an IP address.
- `--allowed-host <NAME>` (repeatable; also the variable `GRAPHERSAL_ALLOWED_HOSTS`, names
  separated by commas) admits further names on either address: a `Host` naming one of them is
  served on any port, and an `Origin` of `http://` or `https://` one of them is accepted whatever
  the `Host`. They are the public names of the server behind a reverse proxy that ends TLS
  (`https://kb.example.com`); the `listening` banner lists them under `allowed hosts`. A value
  with a scheme, port or path is an invocation error (exit code `2`). See
  [Behind a reverse proxy](docker.md#behind-a-reverse-proxy).

Beyond the headers, every write needs `Content-Type: application/json`, which a page of
another site cannot send without a CORS preflight that the server never answers. Scripts cannot
read or write files (as in the playground: file access is denied), so a file is written only by
the explicit Save to file. Request bodies are limited to 16 MiB; a malformed request gets a 4xx
answer with a JSON error. A request that declares a body larger than 64 MiB (`Content-Length`) is
ignored without an answer: the HTTP library would otherwise reserve memory for the whole declared
size.

## On a Store

```bash
graphersal store create data/ --from modern      # once (or: --graph data/ --create-store --server)
graphersal --graph data/ --server                # then open http://127.0.0.1:8080/
graphersal --graph new/ --create-store --on-damage continue --server   # a new store, its damage policy
```

The server opens the [Store](store.md) read-write (its
lock is held while it runs) and serves its graph: **every commit is written to the store's
write-ahead log before the query answers**, so writes survive a restart of the server, also a
crash or `kill -9` (the next start replays the WAL; stderr then notes that the store was not
closed cleanly, which is harmless). Stopping the server with Ctrl+C is such a stop: everything
committed is already on disk.

The store's warnings go to stderr as `WARN graphersal::persist::store: ...` lines: a failed
automatic checkpoint, damage found while it runs, a read-only open (also when the store is used
with `-e`, `--in` or the REPL); an open that rewrote a damaged `GRAPH` copy notes it on stderr
before the start banner. `GRAPHERSAL_LOG` sets the
level (`off`, `error`, `warn` = the default, `info`, `debug`, `trace`; `info` also notes a
checkpoint that encodes the loaded graph because the WAL since the last snapshot exceeds the
merge bound). A whole script is ONE commit here: a large one is written to the WAL in parts
of 64 MiB, and the process memory ceiling bounds it (the unit's changes are held while it
commits); a script that drops and re-creates a huge graph can still be refused as a whole: run
the drop on its own and load in several runs
([The size of one commit](../users_guide/resource_limits.md#the-size-of-one-commit-journal)).

The UI shows a **Store** menu in the top bar (its **↻** button in the top right corner reloads
it): the position (commit number and last commit time), the WAL size, **Checkpoint** (write a
snapshot now, optionally named) and **Mark** (name the current position), and the history:

- **Marks**: "a name for a position in the history; any commit can be restored, not only
  snapshots". **Snapshots**: "a full copy of the graph at a commit (fast start)", each with a
  `.gsnap` download. The menu lists the newest five of each; **Calendar…** (**See all…** when
  there are more) opens the history calendar below. The table in
  [Snapshots and marks](store_marks.md#snapshots-and-marks) compares them.
- Every snapshot and mark has three actions, and **Go to commit...** / **Go to time...** (a
  date and time in your browser's time zone) offer the same for any other point:
  - **Open read-only here** (View): the server serves the state at that point instead of the
    live graph. A banner says "Read-only view of commit N (mark X)" and offers **Back to the live
    graph**. Queries, the graph drawing, the schema and the statistics show that state; every
    write is refused ("the served state is a read-only view at commit N ...: writes are
    refused"; its help points to **Back to the live graph** and to a fork), and so are
    Checkpoint and Mark. The live graph is not changed; Download current state saves the view.
  - **Fork to a new directory...**: asks for a directory (empty: `<store>-fork-<target>` next to
    the store; a relative path is next to the store; it must not exist) and makes a new store
    there. The answer says where it is and how to open it (`graphersal --graph <new> --server
    --port 0`, another server). This store is not changed; to undo, delete the new directory.
  - **Roll back here...**: a confirmation explains the effect: everything after commit N moves
    to the **attic** (restorable as long as nothing new is committed or marked), the live graph
    switches to commit N. The option "delete instead of keeping in the attic" (off by default)
    deletes it for good. The server waits for a running query (it never cancels another
    client's query: use that tab's Stop), rolls the store back in place and serves the result; the Graph, Schema and Statistics panels reload and every tab's
    result is marked as possibly stale.
- **Attic**: one entry per rollback (when, commit range, size) with **Show changes** (per
  commit: time, author, counts by kind, examples), **Restore** (undo the rollback; disabled with
  the reason once something was committed or marked since, then use Fork), **Fork...** (the
  rolled-back history as a new store) and **Remove...** (delete it for good, asks first).
- **Disk space**: the [compaction advice](store_prune.md#is-it-worth-it-the-compaction-advice),
  refreshed with the menu (cheap, nothing is read): "Compact recommended" or "No compaction
  needed" with the reason (which threshold decided), the file's size, live and free-inside bytes
  for a single-file store, what a prune would free first, and the free disk space a compaction
  needs. **Compact** (always enabled, except in a read-only view, in maintenance mode or on a
  store frozen by damage; when the advice does not recommend it, it asks first, showing what it
  would free and the reason; when marks would become unreachable it ALWAYS asks first,
  recommended or not, naming them and saying they cannot be opened afterwards) prunes up to the
  current commit and, for a single file, rewrites it without its free space; queries wait while
  it runs. On a directory store Compact is the prune.

- **Backup**: a directory (empty: `<store>-backup` next to the store, which takes increments
  from then on; a relative path is next to the store) and **full**; **Back up** copies the store
  while queries and commits go on, reading every checksum in the store and again in the copy.
  The answer says how much was copied and how to restore it.
- The store's **creation parameters**: when it was created and its **damage policy** ("on
  damage: maintenance (turns read-only; fixed at creation)").
- **Damage found on disk** while the server runs (a Back up, a Checkpoint, Verify, the automatic
  checkpoint found it): a red block with what found it, every problem, and what the policy did
  (with `maintenance` the store is READ-ONLY now: Checkpoint and Mark are gone, writes are
  refused); **Back up from memory** writes the intact graph in memory into a new directory
  without touching the damaged store, then repair the store with `graphersal store repair`. A
  banner at the top says the same in every tab (through the change feed), and MCP's
  `graph_info` carries it (`damageFound`), so an agent learns why writes fail.

The server runs on a **single-file store** the same way (`graphersal --graph graph.gstore
--server`, `--create-store` creates it); a fork of it becomes a `.gstore` file next to it
(`graph-fork-<target>.gstore`).

Opening a view, going back to live, a rollback and an attic restore change the graph the session
serves, so they cancel a running query first (it rolls back). Several browser tabs share the one
server: a view opened in one tab is the view of all (the other tabs follow through the
[change feed](#live-refresh-in-the-browser) within two seconds: banner, graph, Schema and
Statistics; an MCP agent's `graph_info` says so too and its writes are refused). The CLI's `graphersal store` commands that only read (`list`, `fork`, `verify`, ...)
work while the server runs; `rollback` and `attic restore` from the CLI report "in use" then,
use the menu.

On a **backup** directory (`graphersal --graph backup/ --server`) the server serves the backup
read-only: a banner says "Backup: read-only" with its position and the command that makes it the
live store (`graphersal store restore backup/`, after stopping the server); the Store menu shows
the backup's snapshots and marks with View and Fork only (no Checkpoint, Mark or Roll back), and
every write is refused. See [Backup and Restore](store_backups.md).

### The history calendar

A store keeps hundreds or thousands of snapshots and marks; the Store menu shows only the newest
five of each. **Calendar…** opens a dialog with a month calendar of all of them: each day shows
two badges, the number of marks (M) and of snapshots (S) on that day in your browser's time zone
(a snapshot counts on the day of the commit it holds), their colour deeper the more there are.
‹ › and the month and year selectors (each month with its counts) move through the history;
**Today**, **Newest** and **Oldest** jump. A click on a day (or Enter) lists all its marks and
snapshots with time and commit and the same actions as the menu (View, Fork…, Roll back…,
`.gsnap`). The filter keeps marks and/or snapshots and matches a name or `#commit`. Keyboard: the
arrows move between days (across months), PageUp/PageDown change the month, Home/End go to the
start or end of the week, Esc closes. The WebAssembly playground's store in memory has the same
calendar.

## MCP: an AI agent on the same graph

With `--mcp` the server also offers a [Model Context Protocol](https://modelcontextprotocol.io)
endpoint at `http://127.0.0.1:<port>/mcp`. An AI agent such as Claude Code then works with the
**same graph and session** as the browser: you watch and query in the playground while the agent
queries, changes the graph or evolves the schema through MCP tools, and the page reloads by
itself when the agent changed something. MCP works with the **graph only**: persistence
(checkpoints, marks, views of past states, rollback, the attic, backups, compaction) stays in the
browser's [Store menu](#on-a-store), so the agent's tool list stays short.

### Start the server with MCP

```bash
graphersal --graph modern --server --port 9000 --mcp
# Graphersal dev server: http://127.0.0.1:9000/        (stdout)
#   MCP:       http://127.0.0.1:9000/mcp               (stderr, Server section; the next row,
#   connect:   claude mcp add --transport http ...      is the command that connects Claude Code)
```

Any `--graph` source works, a Store directory too (the agent gets the same graph tools; the
commits it makes are journaled like the browser's):

```bash
graphersal --graph data/ --server --port 9000 --mcp
```

Read-only for the agent (only the reading tools are offered, and a query that would create,
change or delete data, the schema or a catalog definition (a saved query, a compression rule, a
property index) is refused; the browser can still write):

```bash
graphersal --graph modern --server --port 9000 --mcp --mcp-read-only
```

With a token (every MCP request must send `Authorization: Bearer <token>`, otherwise `401`):

```bash
graphersal --graph modern --server --port 9000 --mcp --mcp-token my-secret-token
```

With every [saved query](../users_guide/saved_queries.md) as a tool of its own (besides
`list_saved_queries` and `run_saved_query`, which are always there), for a small, curated catalog:

```bash
graphersal --graph data/ --server --port 9000 --mcp --mcp-query-tools
```

With a note of the operator that every connecting agent reads in the `instructions` of
`initialize` (what the graph holds, which queries to prefer, house rules):

```bash
graphersal --graph data/ --server --mcp --mcp-instructions operator-notes.md
```

The file is UTF-8 text of at most 20000 characters; it is appended under the line "Notes from the
operator of this server about this graph:" after the built-in instructions (and the read-only
note). It is trusted text of the operator. A missing, non-UTF-8 or empty file, or the option
without `--mcp`, is a usage error. The [AI Chat](#ai-chat) does not use it (its system prompt
has the built-in MCP instructions, but not these notes): give the chat its own text with
`system_prompt` or `system_prompt_file`, so nothing is added twice.

The largest tool answer is 60000 characters by default; `--mcp-max-chars N` (at least 1000)
sets another limit (see [Answer size and paging](#answer-size-and-paging)):

```bash
graphersal --graph data/ --server --mcp --mcp-max-chars 30000
```

The options combine (`--mcp --mcp-read-only --mcp-token ... --mcp-query-tools`), and the
[limit options](#limits) apply to the agent's queries as to the browser's.

### The MCP configuration file: `--mcp-config`

Every tool, its description and the instructions take room in the agent's context before its
first call: about 6.7 K tokens for the read-write endpoint, 4.6 K read-only, and more with
every saved query as a tool. Each built-in tool's description is a sentence or two with an
example where it helps; the `gql` description names element ids, anchoring variable-length
patterns and (on a server that allows writes) the write clauses, and points to the resource
`graphersal://gql-reference` for the whole supported subset. `--mcp-config FILE` lets the operator choose what the agent sees:

```bash
graphersal --graph kb.gstore --read-only --server --mcp --mcp-config mcp-kb-readonly.json
```

```json
{
  "_comment": "a key starting with _ is a comment",
  "server_name": "project-kb",
  "instructions": { "mode": "replace", "text": "A read-only knowledge graph of this project. ..." },
  "tools": {
    "include": ["gql", "get_schema", "run_saved_query"],
    "overrides": { "gql": { "description": "Read-only GQL over the project graph. ..." } }
  },
  "saved_query_tools": { "folders": ["kb"], "paging_params": false },
  "resources": ["schema", "saved-queries", "gql-reference"],
  "answer_limit_chars": 30000
}
```

| Key | Meaning | Default |
|---|---|---|
| `server_name` | The `serverInfo.name` of `initialize` (1 to 100 characters) | `graphersal-dev` |
| `instructions` | `{"mode": "builtin" \| "append" \| "replace", "text": ".."}` or `{"mode": .., "file": "notes.md"}` (UTF-8, 1 to 20000 characters): `builtin` the built-in text (+ the read-only note), `append` the built-in text with the operator's text under "Notes from the operator of this server about this graph:" (what `--mcp-instructions` does), `replace` the operator's text only | `builtin` |
| `descriptions` | Kept so that older files still load: `short`, the only style, changes nothing. `full` (the long descriptions of 0.5.2 and before) was removed: an A/B test of Claude Code with Sonnet on read and write tasks measured the same accuracy, fewer failed calls (above all fewer calls of a tool by its bare name) and about 20 to 27 K fewer input tokens per task with the short texts; a file that still says `full` is refused at start with a message (remove the key) | `short` |
| `tools.include` | The built-in tools offered (in their usual order); a tool not offered is not callable either | every tool of the mode but `usage_statistics` (offered only when named here) |
| `tools.exclude` | Built-in tools left out | none |
| `tools.overrides` | `{"<tool>": {"description": ".."}}`: the operator's description instead of the built-in one (the input schema never changes) | none |
| `saved_query_tools` | Saved queries as tools of their own: `{}` = every saved query (what `--mcp-query-tools` does), `folders` (each with its subfolders), `names`, `exclude` (names); `paging_params: true` lists `offset`/`max_rows`/`max_items` in their schemas (by default they are left out: the defaults apply and a call may still pass them). The description is the query's own text (a query that writes adds what it changes). `run_saved_query` still reaches every saved query | no saved-query tools |
| `resources` | The resources listed by `resources/list`, by name: `schema`, `statistics`, `dsl-reference`, `examples`, `saved-queries`, `gql-reference`. Every resource stays readable | all six |
| `answer_limit_chars` | The largest tool answer in characters (at least 1000), like `--mcp-max-chars` | 60000 |

The file is checked at start (exit 2 with the reason): JSON, an unknown key is an error with a
did-you-mean (a key starting with `_` is a comment at every level), an unknown tool or resource
name is an error, a relative instructions `file` resolves against the configuration file, and
**read-only always wins**: a writing tool named in `tools.include` or `tools.overrides` of a
read-only server (`--mcp-read-only`, `--read-only`, or a Store that could only be opened
read-only) is an error rather than a silently missing tool. The flags `--mcp-instructions`,
`--mcp-query-tools` and `--mcp-max-chars` stay as shorthands; the same setting given by a flag
and in the file is an error (there is no precedence to remember). In an [args file](#arguments-from-a-file)
the key is `mcp-config`. Nothing in the file is a built-in profile: without it the endpoint offers
every tool of its mode with its built-in description. Two examples ship in `crates/graphersal-cli/examples/`:
`mcp-kb-readonly.json` (a read-only knowledge graph: `gql`, `get_schema`, `run_saved_query`, the
saved queries of the folder `kb` as tools, short instructions) and
`mcp-short.json` (the data tools, the storage administration tools left out, the operator's notes
appended).

The [AI Chat](#ai-chat) does not read `--mcp-config`: it keeps the full tool set of its mode, the
built-in descriptions and instructions; it cannot read resources, so its prompt carries the DSL
and GQL references instead.

#### What a configuration costs: `graphersal mcp-cost`

`graphersal mcp-cost` prints what the prompt of an endpoint costs, without starting a server: the
characters of the instructions and of every `tools/list` entry, and a rough token estimate
(characters / 4; a real tokenizer counts differently). It takes `--graph` (default `modern`; the
saved queries of a store become tools with `--mcp-query-tools` or `saved_query_tools`),
`--mcp-config`, `--read-only` (measure the read-only endpoint), `--mcp-query-tools` and
`--mcp-instructions`. The graph is always opened read-only (nothing is written, a Store's lock is
not taken), so a store a running server holds can be measured.

```bash
graphersal mcp-cost --graph kb.gstore --read-only --mcp-config mcp-kb-readonly.json
```

```text
MCP prompt cost of kb.gstore (read-only, with --mcp-config)
  instructions: 427 chars, ~107 tokens
  tools: 17: 12774 chars, ~3194 tokens
  total: 13201 chars, ~3301 tokens
  estimate: tokens = characters / 4, a rough estimate (a real tokenizer counts differently)

Tools (characters of each tools/list entry, largest first)
  gql:                1546 chars  ~387 tokens
  references_to:      1275 chars  ~319 tokens
  run_saved_query:    1181 chars  ~296 tokens
  ...
```

The same knowledge graph without the file (`--read-only --mcp-query-tools`) costs about 7 K
tokens.

### Schema changes that fill vectors: `confirm_lock`

`set_schema` and `patch_schema` run the schema preview first when the schema names a
[vector rule](../users_guide/vector_rules.md). A fill whose estimate is 1 s or more (or, without
an estimate, of 10,000 elements or more) locks the whole database while it runs, so the tool
answers a tool error (`isError: true`) with the preview's warning text plus "Repeat with
confirm_lock: true" and applies NOTHING;
the agent repeats the call with `"confirm_lock": true` once the user agreed. A smaller fill is
applied, with the warning text before the answer.

### Connect Claude Code

```bash
claude mcp add --transport http graphersal http://127.0.0.1:9000/mcp
```

With a token:

```bash
claude mcp add --transport http graphersal http://127.0.0.1:9000/mcp \
  --header "Authorization: Bearer my-secret-token"
```

Check the connection, and remove the entry when you are done:

```bash
claude mcp list                  # graphersal: http://127.0.0.1:9000/mcp (HTTP) - ✔ Connected
claude mcp remove graphersal
```

`claude mcp add` stores the server for the current project directory (`-s user` makes it
available everywhere). **A newly added MCP server is picked up by a NEW Claude Code session**:
start `claude` again (or start it after adding). Use the same port as the running server; when
you restart the server on another port, remove and add the entry again. The server must be
running when Claude Code starts, or `claude mcp list` shows it as failed (start the server, then
a new session).

### Try it

1. `graphersal --graph modern --server --port 9000 --mcp`, and open
   `http://127.0.0.1:9000/` in the browser: the top bar shows an **MCP** badge.
2. `claude mcp add --transport http graphersal http://127.0.0.1:9000/mcp`, then start `claude`.
3. Ask: *"Use the graphersal tools: who does marko know?"*. The agent calls `query` with a
   traversal such as `g.v().has("name", "marko").out("knows").values("name")`. The badge's
   tooltip now names the agent (`claude-code`).
4. Ask: *"Add a person ada, 36 years old, who knows marko."* The browser reloads its graph by
   itself and shows "changed: commit N"; results you ran before are marked "possibly stale".
5. Ask: *"Infer the schema, store it in mode open and add an optional string property email to
   person."* (`infer_schema`, `set_schema`, `patch_schema`); the Schema panel follows.

### What the agent gets

The server introduces itself as `graphersal-dev` with instructions that explain the query
language. Tools (each one queues on the server's session like a browser call; read-only tools
are marked `readOnlyHint`, `query`, `set_schema`, `drop_compression` and `drop_index`
`destructiveHint`).
The list is the same on every `--graph` source; there are no persistence tools:

| Tool | What it does |
|---|---|
| `query` `{script, profile?, offset?, max_rows?, max_items?}` | Runs a script (Gremlin in Rhai syntax) as one transaction: the results as compact JSON, [paged](#answer-size-and-paging) (100 results after the first `offset`, 100 items of every nested list by default, with a note when something was cut and the next page's offset), whether it changed the graph and the new commit number, the per-step profile with `profile: true`; a failing script is a tool error carrying the diagnostic and its `Help:` line, and changes nothing |
| `gql` `{query, params?, profile?, offset?, max_rows?, max_items?}` | Runs a GQL query, the text as it is (`$name` values from `params`): one JSON object per row, [paged](#answer-size-and-paging) like `query`; a write (`INSERT`, `SET`, `REMOVE`, `MERGE`, `DELETE`) is one transaction and the answer names the net changes; refused in read-only mode. Its description is short; the resource `graphersal://gql-reference` lists the whole supported subset |
| `get_schema`, `infer_schema` | The stored schema (or null), the schema the data satisfies |
| `validate_schema` `{schema, mode?}`, `validate_schema_patch` `{patch}`, `diff_schema` `{schema}` | Check a proposed schema or a JSON Merge Patch against the data, without applying |
| `set_schema` `{schema, mode?}`, `patch_schema` `{patch}` | Store a schema, patch the stored one (validated against the data) |
| `list_compressions` | The [compression rules](../users_guide/compressed_properties.md) with their stats (also in read-only mode) |
| `estimate_compression` `{label, path, element?, codec?, minBytes?, dictionary?}` | [Estimates](../users_guide/compressed_properties.md#estimating-the-gain) what rules for that label and path would save on the current data, without changing the graph (also in read-only mode) |
| `define_compression` `{name, label, path, element?, minBytes?, dictionary?, replaces?}`, `drop_compression` `{name}`, `recompress` `{name}` | Define, drop or re-run a compression rule (refused in read-only mode); a refused rule answers its field errors |
| `list_indexes` | The [property indexes](../users_guide/property_indexes.md) with their `type` and stats (`entries`, `distinctValues`, `bytes`; `served: false` with a `reason` when the storage serves none or the index type is unknown to this version) (also in read-only mode) |
| `create_index` `{label, key, element?, name?, type?}`, `drop_index` `{name}` | Create (built from the data in the same commit) or drop a property index; `type` is an enum of the known [index types](../users_guide/property_indexes.md#index-types) (`memory-ordered`, the default); refused in read-only mode, where `query` also refuses `g.create_index`/`g.drop_index`; a refused index answers its field errors (an unknown type, a key the schema declares as a vector, a list or a map) |
| `list_saved_queries` `{folder?, search?}` | The [saved queries](../users_guide/saved_queries.md) stored in the graph: name, folder, description, parameters (JSON Schema, default, required, description), status (`error` with the reason when it cannot run), `writes` and `effects` (`true` and what it changes, for a query that writes); `folder` keeps a folder and those below it, `search` a text in the name or description (case-insensitive). With `--mcp-read-only` the queries that write are left out |
| `run_saved_query` `{name, params?, offset?, max_rows?, max_items?}` | Runs `g.query(name, #{..})` like `query` (paged answer, the call text on its first line): the arguments are checked against the parameter schemas, omitted ones take their default, a wrong one is a tool error with its `Help:` line. A query that writes changes only its effects, as one transaction; with `--mcp-read-only` it is refused |
| `workload_statistics` | The [workload statistics](workload_statistics.md) of a Store's graph: queries by normalized plan, property uses, lock times and jobs per channel (`max_rows` per list, default 50; `history`: `minutes`, `hours` or `days` adds every row's buckets; also in read-only mode) |
| `list_jobs` | The [scheduled jobs](scheduled_jobs.md) of a Store's graph: system jobs and jobs of users with schedule, switch, next time, last run, run counts, a user job's action and effects (reading; also in read-only mode; defining and changing jobs stays the user's) |
| `advise` `{everything?}` | The [storage advisor](optimize.md)'s recommendations, each with its command, and their `groups` (`dedup: 74 keys, total -400.1 MiB` with the ids); changes nothing (also in read-only mode) |
| `apply_recommendations` `{ids, everything?}` | Applies the recommendations the user picked (ids from `advise`) in ONE commit, all new compression rules in one pass over the data; all or nothing (an id no longer advised or a refused change applies nothing); refused in read-only mode |
| `usage_statistics` `{max_rows?}` | Opt-in (only when `tools.include` of the [configuration](#the-mcp-configuration-file---mcp-config) names it): the endpoint's [usage statistics](#mcp-usage-statistics) since the server started (memory only) |
| `statistics`, `graph_info` | Counts per label, memory and limits; what the server serves (name, counts, schema mode, source file or Store directory, `view` and a `note` while a read-only view is open, `damageFound`, `mcpReadOnly`, `answerLimitChars`) |

### Answer size and paging

Tool answers are compact JSON (no indentation), on one line after the status lines (`call: ...`
of a saved query, `ok: 443 results (2.1 ms)`), followed by notes. `query`, `gql`,
`run_saved_query` and the tools of `--mcp-query-tools` take three paging arguments:

- `offset` (default 0): the first `offset` top-level results are skipped. The query itself is
  not changed and computes every result again; only the answer shows a later page.
- `max_rows` (default 100): the most top-level results of one page.
- `max_items` (default 100): the most items of every nested list or map; a cut list ends with the
  item `"… N more"`, a cut map with the key `"…"`, and a note says so.

On top of that the whole answer stays within the server's answer limit (`--mcp-max-chars`, default
60000 characters; `graph_info` shows it as `answerLimitChars`). The limit never cuts inside a
result, so the JSON stays valid:

- A list of results keeps its leading **whole** results that fit, and the note names what is
  shown and where to go on: `note: shown results 101-160 of 443, cut at a whole result to fit the
  answer limit of 60000 characters (...); more results follow: next page offset 160`. A page cut
  by `max_rows` alone says `... (max_rows sets the page size); more results follow: next page
  offset 200`; the last page says `no more results`, an offset past the end says so too.
- A result that alone is longer than the limit is left out with a note that names the offset that
  skips it.
- A single value (such as a map from `.next()`) that is too long keeps its leading whole
  entries with a note; `offset` pages only a list of results (GQL rows are one).
- Every other tool keeps the leading whole items of its JSON (a note says how many of how many),
  or the leading whole lines of a text answer.

Clients limit tool output as well: Claude Code caps one MCP tool answer at about 25000 tokens
by default (the environment variable `MAX_MCP_OUTPUT_TOKENS` changes it). Raising `--mcp-max-chars`
alone may not help; bound large answers in the query (LIMIT, `values(..)`) or page them.

**A read-only view opened in the browser** is server-wide: the agent's queries read that past
state too. `graph_info` then carries a `note` ("the served state is a read-only view at commit N
..."), and every tool error carries it as a last line, so a refused write says why. The agent
cannot close the view; the user does (**Back to the live graph**).

Resources (read with `resources/read`): `graphersal://gql-reference` (the GQL subset the
`gql` tool supports, its write semantics and examples, about 13 KB; the `gql` description points
to it), `graphersal://schema` (the stored schema),
`graphersal://statistics`, `graphersal://dsl-reference` (every step and function of the language
with its signatures, both spellings, a one-line description and a runnable example with its result,
generated from the engine's function docs, about 55 KB),
`graphersal://examples` (the playground's examples, about 8 KB) and `graphersal://saved-queries`
(the list of `list_saved_queries`).

**Saved queries first.** The server's instructions and the `list_saved_queries` description tell the
agent to look for a saved query (`list_saved_queries`) before it writes a new query, and to run
it with `run_saved_query`: the queries a user stored for recurring questions are reused instead of
rewritten. A query listed with `writes: true` changes data (its `effects` say what), and the
instructions tell the agent to run one only when the user asked for that change. Both tools are
offered with `--mcp-read-only` too, which lists only the queries that do not write and refuses a
call of one that does. With `--mcp-query-tools`, `tools/list` also lists every saved query as a
tool named like the query, with its own description (a query that writes says so and
names its effects), an `inputSchema` built from its parameter schemas (defaults included,
required parameters in `required`, no other properties; the paging parameters only with
`saved_query_tools.paging_params: true`) and annotations from its effects
(`readOnlyHint` false for a query that writes, `destructiveHint` when it updates or deletes);
calling it is `run_saved_query` with those arguments. `tools/list` reflects the catalog at the time of the
call; the endpoint has no event stream, so it announces `tools.listChanged: false` and a client
lists the tools again to see a changed catalog. A saved query named like a built-in tool
(`statistics`, ...) is not listed as a tool of its own; `run_saved_query` reaches it.

The agent's queries share everything with the browser: one query runs at a time, a read-only
view opened by either side is what both see, and the [limits](#limits) apply. **Stop** in the
browser cancels only that page's own query, never the agent's. The agent cancels its own request
with the MCP notification `notifications/cancelled` (`{"requestId": <id of the tools/call>}`, sent
with its `Mcp-Session-Id`): the query stops and rolls back, also while it still waits for the
session, and the request answers a "cancelled" tool error. A notification for another session's
request, for a request that has ended, or sent without a session id is ignored.

### MCP usage statistics

The endpoint counts how it is used since the server process started, in both run modes (the
read-write session thread and the [read-only serving mode](#read-only-serving-mode)'s workers).
The counters are kept **in the server's memory only**: nothing is written to the database or to
any file (the process may run with a read-only root filesystem and no volume), and a restart
starts from zero. The [workload statistics](workload_statistics.md) of a Store are a different
thing (per query plan, kept in the Store; off in read-only mode).

`GET /api/mcp/stats` (404 without `--mcp`) answers:

| Key | What it counts |
|---|---|
| `startedAtMs`, `uptimeS` | When the counting started (milliseconds since the Unix epoch) and for how long |
| `requests` | `total` JSON-RPC messages of a known (or no) session, `withoutSession`, `busy503` (answered `503` because the pool was busy) |
| `methods` | Requests per JSON-RPC method (`initialize`, `tools/call`, ...; `(notification)`, `(other)`) |
| `sessions` | `initialized`, and `activeLast5Min`: the distinct session ids seen in about the last five minutes (an estimate, `activeIsEstimate: true`; within a few percent) |
| `refusals` | `queueFull`, `queueTimeout`, `clientLimit` (the [pool's limits](#read-only-serving-mode) for MCP requests), `tooManyConnections` (`--max-connections`, requests to `/mcp`), `stopping`, `unknownSession` (`404`), `unauthorized` (`--mcp-token`), `invalidRequest` (a wrong `Accept`, `Content-Type`, protocol version, size or JSON) |
| `tools`, `savedQueries` | One row per tool and per saved query (run by `run_saved_query` or by its own tool): `calls`, `errors` (tool errors and refused calls), `totalMs`, `meanMs`, `maxMs`, `histogram` (calls below each bound of `latencyBucketsMs`, 1 ms to 10 s, the last entry the slower ones) and `answerChars`; most calls first. At most 1024 names each, further ones count as `(other)`; an unknown tool counts as `(unknown)` |
| `clients` | One row per client name and version (the `clientInfo` of `initialize`): `initialized`, `requests`, `toolCalls`; the 4096 most recently initialized names are kept (`clientsKept`, `clientsMax`) |
| `readOnly`, `statelessSessions` | The endpoint's mode |

The playground shows the same in a dialog: click the **MCP** badge in the top bar (Refresh
reads it again).

The agent can read them too with the tool `usage_statistics` `{max_rows?}` (reading; at most
50 rows per list unless `max_rows` says otherwise, `<list>_omitted` counts the rest). It is
**opt-in**: it is never in the default tool list (nor the AI Chat's), and an endpoint offers it
only when its [configuration](#the-mcp-configuration-file---mcp-config) names it in
`tools.include`:

```json
{ "tools": { "include": ["gql", "get_schema", "run_saved_query", "usage_statistics"] } }
```

### Live refresh in the browser

Every page polls `GET /api/changes` every 2 seconds while it is visible (it pauses in a hidden
tab and checks at once when it becomes visible again). When the graph changed because of
something this page did not do (an agent over MCP, or another browser tab on the same server),
the page reloads the graph info, the Graph, Schema and Statistics panels and the Store menu,
marks every tab's result as possibly stale and shows "changed: commit N". Its own queries do not
trigger the notice.

### Protocol details

The endpoint implements MCP's Streamable HTTP transport of the protocol revisions `2025-11-25`,
`2025-06-18` and `2025-03-26` (the initialize-based ones; `initialize` negotiates one of them)
with JSON answers and no server-sent event stream: `POST /mcp` with one JSON-RPC message
(`initialize`, `ping`, `tools/list`, `tools/call`, `resources/list`, `resources/read`; a
notification is answered `202 Accepted`), `GET /mcp` is `405`, `DELETE /mcp` ends the session.
`initialize` returns an `Mcp-Session-Id`; a request that names an unknown session (after a
restart of the server) is `404`, so the client initializes again. A request whose
`MCP-Protocol-Version` header names another revision is `400`, which tells a client that also
speaks newer, stateless revisions to fall back to `initialize`. The `Host`/`Origin` checks of the
[Safety](#safety) section apply (another site's `Origin` is `403`). On the loopback
`--mcp-token` protects against other local users and programs; on a network address
([`--host`, or a container](#listening-address-and-docker)) it is the only thing that keeps other
machines from the MCP tools. The printed MCP URL uses the host of the stdout URL (`127.0.0.1` for a
wildcard address); a client on another machine uses the server's address instead.

## AI Chat

> **EXPERIMENTAL**, like the dev server. The configuration file, the endpoints and the tab change
> without notice.

With `--ai FILE` the playground's Query panel can open an **AI Chat** tab: you ask questions in
plain language and an LLM agent answers them by working on the served graph. The agent uses the
same tools as an [MCP](#mcp-an-ai-agent-on-the-same-graph) client (`query`, the schema tools,
saved queries, compression rules, `statistics`, `graph_info`), but the agent loop runs **inside
the dev server process**: the server calls the LLM provider, runs the tool calls the model asks
for on the session (they queue like every browser and MCP call), sends the results back and
repeats until the model answers. The browser only shows the conversation; the API key never
reaches the page. `--ai` does not need `--mcp` (both may be on).

```bash
export ANTHROPIC_API_KEY=...                           # the key stays in the server process
graphersal --graph modern --server --ai ai.json
# Graphersal dev server: http://127.0.0.1:8080/                              (stdout)
# AI Chat (EXPERIMENTAL): model ... over the Anthropic API; data the agent queries is sent to api.anthropic.com   (stderr)
```

Open the page, click **+** in the Query panel's tab strip and choose **AI Chat** (the tab has its
own colour). Ask, for example, *"Who does marko know?"* or *"Which software was created by more
than one person?"*. The transcript shows:

- your messages and the agent's answers (Markdown: tables, lists, code);
- per question, the steps that led to the answer in ONE collapsed group above it, with the time
  the turn took (e.g. "Worked for 34 s: 2 tool calls (list_saved_queries, query)", "Working for
  12 s: running query" while the turn runs, counting up every second, "Stopped after 5 s and 1
  tool call: list_saved_queries" when it failed; a turn without tool calls shows "Working for
  3 s…" at the bottom instead); opened, it lists the agent's
  interim texts and every tool call in order, each collapsible with its arguments, a result
  summary and the result text; a `query` call has **Open in a query tab** and **Run**;
- a query the agent proposes in its answer (a fenced block) with **Copy** and **Run** (Run opens
  a new query tab and runs it there);
- errors with their help text (a refused key, an unknown model, the provider's rate limit, ...).

A [saved query that writes](../users_guide/saved_queries.md#queries-that-read-and-queries-that-write)
(`writes: true`) runs only after you confirmed it: the agent's first `run_saved_query` of it in a
turn is not run but answered "ask the user first", so the agent asks you in the chat what it is
about to change; when it calls the query again in the turn of your next message, it runs (once).

**Stop** ends the agent's turn: the pending provider call is abandoned (the client is synchronous,
so the answer is dropped when it arrives) and the query the agent runs is cancelled and rolled
back. It stops only this chat; another chat tab, the browser's own query and an MCP client keep
running (and the browser's query **Stop** does not stop the agent's query). `/clear` (the whole
message) starts over: it stops a running turn and deletes the conversation on the server; `/help`
lists the commands. Each chat tab is its own conversation, and a conversation belongs to the page
that created it: after a reload the transcript stays as history and the next message starts a new
conversation. Conversations live in the server's memory only (at most 64; a restart ends them).

**Export ▾** in the chat's header downloads the transcript of this chat tab as a file named after
the tab and the time (`graphersal-chat-2-20261027-090507.md`): **Markdown (.md)** (a title, the
export time, model, server and graph, then per turn your question, the final answer as the model
wrote it, errors with their help and one line with the time the turn took and its number of tool
calls, e.g. `_Answered in 34 s · 5 tool calls_`; the steps, i.e. the interim texts and the tool
calls with their arguments and results, are left out, the time when the server did not report it) or **JSON (.json)** (`{"format":
"graphersal-ai-chat", "version": 1, "exportedAt", "meta": {chat, provider, model, readOnly,
server, graph}, "preamble": [...], "turns": [{question, state, steps, answer, tail, done,
timing}]}`, where `steps` are `{type: "text", text}` and `{type: "tool", id, name, args, ok,
summary, result, resultTruncated}`, `tail` holds `error {message, help}`, `notice {text}`, `text
{text}` and `stopped`, and `done` the turn's figures `elapsedMs`, `toolMs`, `llmCalls`,
`inputTokens`, `outputTokens`, `cachedTokens`, `toolCalls`, `limitReached`, `stopped`). It holds
what the page holds: a transcript restored after a reload keeps the newest 300 items and each tool
result text shortened to 2000 characters (`resultTruncated` in JSON).

When the agent changes the graph, the page reloads it like after an MCP change: the change feed
names the client `ai` (with the conversation id), and the other tabs' results are marked
"possibly stale".

In the **WebAssembly playground** (no server), an AI Chat tab can be opened but is read-only: it
says "AI Chat is available when you run Graphersal locally: graphersal --server --ai ai.json".

### The configuration file

`--ai FILE` names a JSON object. It is read once, at the start of the server. A value of the wrong type
or a missing environment variable stops the start with a message that names the key. Unknown keys
are lenient: a key starting with `_` (for example `"_comment"`) is ignored silently; any other
unknown key is ignored with a startup warning that lists the keys; but an unknown key that is
within two edits of a known one (`"readonly"`, `"max_token"`) stops the start with a `did you
mean "read_only"?` hint, so a typo cannot silently drop a safety setting.

| Key | Default | Meaning |
|---|---|---|
| `provider` | required | `"anthropic"` (the Messages API with tool use; the static system prompt is cached by the provider) or `"openai"` (Chat Completions with tools: OpenAI, and every compatible server through `base_url`: Google Gemini, Azure OpenAI, OpenRouter, Groq, Ollama, LM Studio, vLLM, ...) |
| `model` | required | The model id, passed to the provider as is. There is no default model: the file names it. Choose a model that supports tool calls |
| `base_url` | the provider's (`https://api.anthropic.com`, `https://api.openai.com/v1`) | An `http://` or `https://` URL; the adapter appends `/v1/messages` (anthropic) or `/chat/completions` (openai), so an OpenAI-compatible URL ends in `/v1` (Ollama: `http://localhost:11434/v1`) |
| `api_key_env` | none | The name of the environment variable that holds the API key (`"ANTHROPIC_API_KEY"`, `"OPENAI_API_KEY"`, ...). The variable must be set and not empty when the server starts |
| `api_key` | none | The key inline. Accepted with a startup warning (the file may end up shared or committed); prefer `api_key_env`. Not together with `api_key_env` |
| `read_only` | `false` | `true`: the agent may only read (see below) |
| `max_tool_calls_per_turn` | `20` | The most tool calls one turn (one question) may make. The prompt states this budget, and the last results before the limit tell the model how many calls are left. At the limit the calls beyond it are not run, and the model is asked once more, without tools (`tool_choice` "none"), to answer from what it has; the answer is marked "tool-call limit reached". A provider that ignores `tool_choice` and keeps asking for tools ends the turn with a message that says so. "go on" continues it either way |
| `max_tokens` | `4096` | The answer length bound of one provider call; an answer cut there ends with an error that says so |
| `system_prompt` | none | Your own text for the agent's system prompt (at most 20 000 characters), see [Custom system prompt](#custom-system-prompt). Not together with `system_prompt_file` |
| `system_prompt_file` | none | The same text from a UTF-8 file; a relative path is relative to the directory of the config file. An unreadable or non-UTF-8 file, or too long a text, stops the start |

A key is required, except for an `openai` provider with a custom `base_url` (a local server such as
Ollama needs none). The key is never logged, never written to a file by the server and never sent
to the page (`GET /api/info` shows provider, model, read-only and the host data is sent to, never
the key). A key sent over plain `http://` to a host that is not this machine gets a startup
warning.

Examples (the repository holds them as `crates/graphersal-cli/examples/ai-*.json`; the model ids
are examples, use a current model of your provider):

Anthropic:

```json
{
  "provider": "anthropic",
  "model": "claude-sonnet-4-5",
  "base_url": null,
  "api_key_env": "ANTHROPIC_API_KEY",
  "read_only": false,
  "max_tool_calls_per_turn": 20,
  "max_tokens": 4096
}
```

OpenAI (and, with another `base_url` and key variable, OpenRouter, Groq, Azure, ...):

```json
{
  "provider": "openai",
  "model": "gpt-4.1-mini",
  "api_key_env": "OPENAI_API_KEY"
}
```

Google Gemini, through Google's OpenAI-compatible endpoint (a key from Google AI Studio in
`GEMINI_API_KEY`; the adapter appends `/chat/completions` to the `base_url`):

```json
{
  "provider": "openai",
  "model": "gemini-3.8-flash",
  "base_url": "https://generativelanguage.googleapis.com/v1beta/openai",
  "api_key_env": "GEMINI_API_KEY"
}
```

Thinking models (Gemini 3) work too: the thought signatures Gemini attaches to its tool calls
(`extra_content`) are sent back unchanged with the next request, as Google requires.

Ollama on this machine (`ollama pull qwen2.5:7b` first; no key, no data leaves the machine):

```json
{
  "provider": "openai",
  "model": "qwen2.5:7b",
  "base_url": "http://localhost:11434/v1"
}
```

### Custom system prompt

`system_prompt` (inline) or `system_prompt_file` tells the agent what it cannot read from the
graph: units, the meaning of labels, house rules, the language to answer in.

```json
{
  "provider": "anthropic",
  "model": "claude-sonnet-4-5",
  "api_key_env": "ANTHROPIC_API_KEY",
  "_comment": "ages are years; the notes live next to this file",
  "system_prompt_file": "graph-notes.md"
}
```

The text is placed in the system prompt after the agent's rules and before the DSL reference,
under the line "Notes from the operator of this server about this graph:". It is trusted text of
the operator, but it comes after the rules and does not lift them: tool results stay data, never
instructions. It applies to the AI Chat only; the [MCP](#mcp-an-ai-agent-on-the-same-graph)
instructions are unchanged (and the `--mcp-instructions` notes are not added; the chat does not
read `--mcp-config` either: it keeps the full tool set and the built-in instructions, and its prompt carries the GQL reference after the DSL reference). The prompt prefix stays fixed for the conversation, so the provider's
prompt cache still works.

### What data leaves the machine

Everything the agent sees goes to the provider: your messages, the system prompt (the MCP
instructions, the agent's rules, your [custom system prompt](#custom-system-prompt) text if any, the [DSL and GQL references](#what-the-agent-gets) and a short summary of
the graph taken at the first question: counts, labels, schema mode, number of saved queries), and **every tool result**:
query results, schemas, statistics, saved queries. The tab's header says so: "Data you query is
sent to api.anthropic.com (Anthropic API)"; with `provider: "openai"` it names the
OpenAI-compatible API, whoever runs it (Gemini's host, for example). When `base_url` points at
this machine (`localhost`, `127.*`, `[::1]`), nothing leaves it and the header says "The model
runs on this machine". Use a local provider for
data that must not leave it.

Graph data is untrusted input: a property value can contain text that looks like an instruction.
The agent's system prompt says that tool results are data, never instructions, and only your own
messages instruct it; that lowers the risk, it does not remove it. Use `read_only` when the agent
should not change anything.

### Read-only

With `"read_only": true` the agent gets only the reading tools, and its `query` runs under the
same read-only rule as [`--mcp-read-only`](#start-the-server-with-mcp): a query that would create,
change or delete data, the schema or a catalog definition is refused. The tab shows a
**read-only** badge. The browser can still write.

### Limits

- `max_tool_calls_per_turn` bounds the tool calls of one question; `max_tokens` the length of one
  answer.
- The server's [limit options](#limits) (`--timeout`, `--memory-limit`, `--max-traversers`, ...)
  apply to every query the agent runs, as to the browser's and MCP's; a query result is cut at
  the display limits (100 rows and 100 nested items) before it goes to the model, with a note.
- A message is at most 100 000 characters. One turn runs at a time per conversation; several
  conversations run at the same time.
- The provider's own limits (rate, context length, cost) are yours: a long conversation sends the
  whole history with every call. The line under an answer shows the tokens the turn's provider
  calls read (every request token, the cached ones included, with the cached part named) and
  wrote, summed over the turn, beside its time.
- The server keeps the conversations in memory only, each finished turn compressed (LZ4); a
  restart forgets them.

## The HTTP API (EXPERIMENTAL)

The API follows the playground's engine boundary (`playground/js/engine/engine.js`) and is
**EXPERIMENTAL**: it may change in any release, minor or patch. Answers are JSON; errors are
`{"error": "..."}` with a 4xx status (422 for a failing schema operation).

An **OpenAPI 3.1 description** of every endpoint below is part of each release:
[`crates/graphersal-cli/openapi.json`](https://github.com/graphersal/graphersal/blob/main/crates/graphersal-cli/openapi.json)
in the repository, and a running server answers it at `GET /openapi.json` (also
`GET /api/openapi.json`; `GET /api/info` names it as `openapi`), with the server's own version in
`info.version`. Generate a client from it, or open it in any OpenAPI viewer; pin the Graphersal
version you read it from. The schemas in it are deliberately loose (objects with the documented
keys); the table below and the linked pages describe every answer.

| Endpoint | Engine method |
|---|---|
| `GET /api/info` | `info()` (`server: "graphersal-dev"`, `version`, `saveable`, `source`, `presets`: the limits in force, `mcp`: `{endpoint, readOnly, client}` or null, `ai`: `{provider, model, readOnly, sendsDataTo, maxToolCallsPerTurn}` or null (never the key; `sendsDataTo` is the provider's host, null for one on this machine), `damageFound`: `{text, frozen, foundBy, policy, problems}` or null, `embedders`: `[{name, model, dimension, default}]`, the built-in `hash-ngram/1` and every model of the database) |
| `GET /api/changes` | `changes()`: the change feed `{version, commitSeq, recent: [{version, by, commitSeq, detail?}], view, mcp, damageFound}`; `by` is the page's `X-Graphersal-Client` header (every request of the page sends it), `mcp`, or `ai` (an AI Chat conversation, its id in `detail`) |
| `POST /mcp` | the MCP endpoint (with `--mcp`, [above](#mcp-an-ai-agent-on-the-same-graph)) |
| `POST /api/ai/conversations` | with `--ai` (EXPERIMENTAL): a new AI Chat conversation `{id}` (random, 128 bits) owned by the page's `X-Graphersal-Client` (required); every other client gets `403` for it, an unknown id is `404`, and without `--ai` every `/api/ai/*` is `404` |
| `POST /api/ai/conversations/<id>/messages` `{text}` | starts a turn on the server (`{turn}`; `409` while one runs): the provider is called, the tools it asks for run on the session like MCP tool calls (commits in the change feed as `ai`), until it answers without a tool call or reaches `max_tool_calls_per_turn` |
| `GET /api/ai/conversations/<id>/events?after=N` | `{events, running, last}`: the events after `N`, each with `seq`: `user {text}`, `assistant {text}`, `tool_call {id, name, args}`, `tool_result {id, name, ok, summary, text}`, `error {message, help}`, `done {stopped, toolCalls, elapsedMs, toolMs, llmCalls, inputTokens, outputTokens, cachedTokens, limitReached}` (the turn's wall time and the part of it spent in tool calls, in milliseconds; its answered provider calls and their tokens summed: `inputTokens` counts every request token, the cached ones included, `cachedTokens` the part read from the prompt cache; whether the turn reached `max_tool_calls_per_turn`; the page shows them under the answer, e.g. "12.4 s · tools 3.1 s · tokens 15.2k in (12k cached) / 830 out") |
| `POST /api/ai/conversations/<id>/stop` | `{stopped}`: ends this conversation's turn only: its pending provider call is abandoned, the query it runs is cancelled and rolled back; other conversations are not affected (and `POST /api/cancel` does not stop the agent's query) |
| `DELETE /api/ai/conversations/<id>` | drops the conversation (history, pending call, running query) |
| `GET /api/graph` | `graphInfo()` |
| `POST /api/execute` `{script, options}` | `execute(script, options)` |
| `POST /api/cancel` | `stop(runId)`: cancels the page's run `runId` (body `{"runId": ".."}`, the id given to `POST /api/execute`), or without a body every run of the page (queries, layout slices); never another client's: `{cancelled}` |
| `GET /api/schema` | `schema()` (the schema views) |
| `GET /api/schema/stored`, `GET /api/schema/infer` | `getSchema()`, `inferSchema()` |
| `POST /api/schema/view` (body: a schema) | `schemaViewOf(schema)`: the schema views of that schema (the editor's draft) with the graph's label counts, `source: "given"`; nothing stored |
| `POST /api/schema/set`, `/patch`, `/validate`, `/validate-patch`, `/diff` | the schema methods (body: the schema or patch; `schema/set?mode=open\|closed\|none` replaces the schema's mode) |
| `GET /api/graph-view?maxNodes=&maxEdges=` | `graphView()`: the drawing as columns (ids, label indices, captions, edge endpoints as indices), no properties |
| `POST /api/graph-view` `{maxNodes?, maxEdges?, degrees?, filter?}` | `graphView(limits, filter)`: the same columns with the Graph panel's filter applied BEFORE the limits (`filter`: `{vertices?: {label: [{key, op, value?}]}, edges?: {..}, hideIsolated?}`; `op` `eq`, `neq`, `lt`, `lte`, `gt`, `gte`, `between` (low inclusive, high exclusive), `contains`, `starts_with`, `ends_with`, `regex`, `exists`, `missing`; values of keys the schema declares are coerced like a write) plus `matchedNodes`/`matchedEdges` (everything that passes, counted exactly); `degrees: true` adds `vertexInDegree`/`vertexOutDegree` (edges in the whole graph). An invalid filter is a 422 naming the label, key and operator. `POST /api/neighbourhood` and `/api/sample` take `degrees` too, the neighbourhood also `filter` (a display filter: the neighbourhood is computed as without it, then what it hides is dropped) |
| `POST /api/graph-filter-query` (body: a filter) | `graphFilterQuery(filter)`: `{query}`, the Rhai query selecting the same vertices and edges (`#{vertices: g.v()..., edges: g.e()...}`) |
| `POST /api/neighbourhood` | `neighbourhood(request)`: the vertices within `hops` (the depth, 1-5) along `direction` (`both`, `out`, `in`) of the centre `ids`, plus the vertices `keep` (a drawing being expanded), and the edges between them, bounded by `maxNodes`/`maxEdges`: the drawing's columns plus `seeds`, `missing`, `hiddenEdges` and, when the limits cut it, `leftOut: {vertices, edges, complete}`; 422 for an unknown id or a depth above 5 |
| `POST /api/sample` | `labelSample(request)`: the first vertices of some primary labels (`{vertices: {labels, exclude?}}`) or the first edges of one label pair (`{edge: {label, from?, to?}}`), with `sample: {kind, shown, total, complete}` |
| `GET /api/meta-graph` | `metaGraph()`: vertex labels with their vertex counts (the 40 largest, the rest as `other`) and label pairs with their edge counts (the 200 largest) |
| `GET /api/element?kind=vertex\|edge&id=` | `element(kind, id)`: one element with its properties (the inspector), `null` when it no longer exists |
| `GET /api/element/edit?kind=vertex\|edge&id=` | `elementForEdit(kind, id)`: the element for the property editor, its values in the typed form (`{type, value}`: `int64` as decimal text, `uuid`, `object` as `[key, value]` pairs, ...): `{kind, id, label, labels, source?, target?, properties: [[key, typed]], readOnly}` (`readOnly`: why the graph cannot be written now, e.g. a read-only view or a store in maintenance mode, else `null`) |
| `POST /api/element/update` `{kind, id, set: [[key, typed]], remove: [key], validateOnly?}` | `updateElement(request)`: writes the changed and removed top-level properties as ONE commit, validated by the engine (schema, required properties); any failing key refuses the whole update: `{ok: true, changed, element}` or `{ok: false, errors: [{field, message, help?}]}` (`field` the key, or `general`; always 200). `validateOnly` runs it as a dry run (no commit) |
| `POST /api/element/labels` `{kind: "vertex"}` or `{kind: "edge", from, to}` | `labelChoices(request)`: the labels the Graph panel's edit mode offers for a new element: `{kind, mode, free, labels: [{label, declared, count, connects}], readOnly}`. Vertices: the schema's declared labels, then the graph's other labels by count. An edge between the vertices `from` and `to`: the declared edge labels whose `connections` allow the pair (any label of an endpoint matches, `*` matches all, no connections allow every pair), then the graph's other edge labels, then (outside mode `closed`, where topology is not enforced) the declared labels that connect other labels (`connects: false`). In mode `closed` only declared, connecting labels and `free: false` (no label typed by hand). A missing endpoint is a 422 |
| `POST /api/element/create` `{kind: "vertex", labels, id?, properties: [[key, typed]], validateOnly?}` or `{kind: "edge", label, from, to, id?, properties, validateOnly?}` | `createElement(request)`: creates the element as ONE commit, validated by the engine (labels, connections, required properties, types); every property is tried, so all bad values come back at once: `{ok: true, created, element, drawn: {kind, id, label, caption, source?, target?}}` or `{ok: false, errors: [{field, message, help?}]}` (`field` a property key, `label`, `id`, `from`/`to` or `general`; always 200). `validateOnly` runs it as a dry run (no commit, no automatic id used up) |
| `POST /api/element/delete` `{kind, id, validateOnly?}` | `deleteElement(request)`: deletes a vertex with its edges, or an edge, as ONE commit: `{ok: true, deleted, edges}` (`edges`: the edges removed with the vertex; with `validateOnly` they are counted and nothing is deleted) or `{ok: false, errors}`. Creating and deleting are refused, like `element/update`, while the graph is read-only (a past state's view, a backup, maintenance mode) |
| `POST /api/layout` | `layoutStart(input, options)`: starts the engine's force layout of a drawing (`{vertexCount, edgeSource, edgeTarget, seed?, iterations?, budgetMs?}`) and runs its first slice; answers the job's status `{job, state, iterations, totalIterations, progress, elapsedMs, vertexCount, cached}`, with `positions` (`x, y` per vertex) once `state` is `done` or `cancelled`. A drawing laid out recently comes from the cache at once |
| `POST /api/layout/step` | `layoutStep(job, budgetMs)`: the next slice (`{job, budgetMs?}`, default 100 ms, at most 500); Stop (`POST /api/cancel` without a `runId`) ends a running or waiting slice as `cancelled` |
| `POST /api/layout/cancel` | `layoutCancel(job)`: ends the job, keeping the positions reached so far |
| `GET /api/completions`, `GET /api/token-values`, `GET /api/memory` | `completions()`, `tokenValues()`, `memoryUsage()` |
| `GET /api/statistics` | `statistics()`: `{statistics, memory, process, limits: {presets, defaults}}`. `process` is the memory the process holds beside the graph (`GraphStatistics` stays data counts): `memory` is the whole process (`liveHeapBytes`: the bytes the Rust allocator holds, from the CLI's tracking allocator; `rssBytes` and `peakRssBytes`: the resident set size, from `/proc/self/status` `VmRSS`/`VmHWM` on Linux and from `ps -o rss=` on macOS, where the peak is the highest value any report of this server has seen; no RSS on Windows or other platforms; each member is left out when unknown; `source` is `proc`, `ps`, `allocator` or `none`; the WebAssembly playground adds `wasmMemoryBytes`, the linear memory size, `source` `wasm`; beside them the process memory bar: `usedBytes` (the tracked live heap, else the sum of the known segments with `usedTracked` false), `limitBytes` and `usedPercent` (the process ceiling and the share used; absent with `--memory-limit none`), `limitSource` (`configured`, `cgroup`, `physical`, `wasm`, `none`, `unknown`), `segments` (`[{name, bytes}]`: `structure`, `properties`, `compressed`, `indexes`, `models`, then `other` when tracked), `compression {plainBytes, storedBytes}` when values are compressed or deduplicated, `estimateAboveHeap: true` when the graph's estimate exceeds the live heap, or `limitError` when the ceiling cannot be read; see [Seeing how full the process is](../users_guide/resource_limits.md#seeing-how-full-the-process-is)), `models` lists every loaded embedding model that reports its size (`name`, `model`, `dimension`, `bytes`, `default`; an estimate of the weights and the tokenizer), and `aiChat {conversations, historyBytes, historyUncompressedBytes}` (the bytes the stored chat history takes, compressed per finished turn, and the bytes it would take uncompressed) is added when the server runs with `--ai`. The MCP `statistics` tool returns the same |
| `GET /api/mcp/stats` | The MCP endpoint's [usage statistics](#mcp-usage-statistics) (memory only, since the start; 404 without `--mcp`) |
| `GET /api/workload[?history=minutes\|hours\|days]` | `workloadReport(history)`: the [workload statistics](workload_statistics.md) of the served graph (`{level, queries, properties, locks, jobs}`; level `off` outside a Store); with `history` every row's buckets of that tier (chart data; an unknown tier is a 422). Only the queries a page or an agent runs count (under `playground` / `mcp`); the page's drawing and panels are not counted |
| `GET /api/journal?after=N&level=..&since=..&until=..&source=..&kind=..&limit=..` | `journal(spec)`: one page of the served graph's [system journal](system_journal.md) (`{records, next_after, next_before?, truncated, more, notes}`; page with `after` = the previous `next_after`; `newest_first=true` answers the newest records first, page backwards with `before` = the previous `next_before`; a bad parameter is a 422). Reading it is `Read` on the resource `Journal`. The MCP tool `journal` (reading) answers the same, at most 100 records unless `limit` says otherwise |
| `GET /api/jobs`, `POST /api/jobs/define\|drop\|enable\|schedule\|run`, `GET /api/jobs/history?name=..&tier=..` | `jobs()`, `defineJob(spec)`, `dropJob(name)`, `setJobEnabled(name, on)`, `setJobSchedule(name, cron)`, `runJobNow(name)`, `jobHistory(name, tier)`: the [scheduled jobs](scheduled_jobs.md) (list, define with field errors, switch, a system job's schedule, run now, the runs per minute/hour/day); a backup job may name any directory here, never in the browser |
| `GET /api/export?format=graphson\|graphml` | `saveGraph(format)` (download) |
| `GET /api/export?format=gsnap[&models=without]` | `saveGraph('snapshot', {withModels})`: the current state as a packed snapshot (binary download), with the embedding models of the store unless `models=without` |
| `POST /api/save` | Save to file: `{path, format, bytes, warnings}`, plus `schemaPath` when the schema was written to the `--schema` file, or `warning` when a stored schema could not be saved (GraphML/GraphSON without `--schema`) |
| `GET /api/store` | `storeInfo()`: `{store: null}` without a Store, else position, snapshots, marks, WAL, the creation parameters (`storeId`, `damagePolicy`, `createdText`, `chunkBytes`), `damageFound` (`[{foundBy, atText, problems}]`), `frozenByDamage`, `damageText`, `backupFromMemory`, `models` (the [embedding models](store_models.md) with their licences and `usedBySchema`) and `missingModels` |
| `GET /api/store/summary`, `GET /api/store/details` | `storeSummary()`, `storeDetails()`: the Store menu's two calls. The summary is `GET /api/store` with `compaction`, `compactionError`, `storeBytes(Text)` and `graphBytes(Text)` `null` and `detailsPending: true`: it reads only names, lengths and the store's small metadata files, never the WAL, an attic entry's WAL, a model blob or the graph's elements, so the menu opens at once however long the history is. The details `{details: {commitSeq, compaction, compactionError, storeBytes, storeBytesText, graphBytes, graphBytesText}}` follow (the graph's size is walked once per commit; the advice's prune estimate reads an attic entry's WAL only for a blob nothing else references, once per open store) |
| `POST /api/store/checkpoint` `{name}`, `POST /api/store/mark` `{name}` | `checkpoint(name)`, `mark(name)` (422 with the diagnostic on failure; 409 without a Store) |
| `GET /api/store/snapshot?commit=N[&models=without]` | a stored snapshot as `.gsnap` (no `commit`: the latest), with the store's [embedding models](store_models.md) unless `models=without` |
| `POST /api/store/view` `{target}` | `openView(target)`: serve the state at the target read-only: `{view: {commitSeq, label, timeText, target}, graph}`; `GET /api/info` and `GET /api/store` report `view` |
| `POST /api/store/live` | `closeView()`: back to the live graph |
| `POST /api/store/fork` `{target, dir}` | `fork(target, dir)`: `{fork: {dir, commitSeq, graphId, open, undo}}` (409: the directory exists) |
| `POST /api/store/rollback` `{target, delete}` | `rollback(target, delete)`: `{rollback: {target, previousCommitSeq, attic, undo, ...}, graph, store}` |
| `GET /api/store/attic/changes?id=&offset=&limit=` | `atticChanges(id, offset, limit)`: one page of commits (`limit` default 100, at most 500) and the totals over all of them: `{commitCount, mutationCount, totals, offset, limit, commits: [{commitSeq, timeText, principal, counts, samples}], hasMore}` |
| `POST /api/store/verify` | `verifyStore()`: `{verify: {ok, problems, notes, snapshotsChecked, segmentsChecked, recordsChecked}}` |
| `POST /api/store/backup` `{dir, full, from_memory}` | `backupStore(body)`: a backup into `dir` (default `<store>-backup` next to the store; a relative path is next to it; an existing backup of the store takes an increment): `{backup: {dir, incremental, fromMemory, commitSeq, bytesText, warnings, restore}, store}`; 422 with the report on damage (nothing kept; the store's damage policy applies) and for a refused backup; `from_memory` only after damage was found |
| `GET /api/store/compaction` | `compactionAdvice()`: `{compaction: {recommended, reason, decidedBy, compactable, totalBytes, liveBytes, garbageBytes, pruneBytes, pruneFirst, reclaimableBytes, spaceNeeded, freeSpace, policy, ...}}` (also in `GET /api/store` as `compaction`, with `kind` and `space`) |
| `POST /api/store/compact` | `compactStore({dropMarks})`, optional body `{"dropMarks": true}` (an empty body is `{}`): `{compact: {keptFrom, snapshotsRemoved, segmentsRemoved, marksUnreachable, compacted, bytesBefore, bytesAfter, bytesFreed}, compaction}`; 409 in a read-only view; 409 `{"error", "marksUnreachable": [...]}` with nothing changed when the prune would make marks unreachable and `dropMarks` is not `true` (the playground sends it after the user confirmed) |
| `POST /api/store/attic/restore`, `/fork` (`{id, dir}`), `/remove` (`{id}`) | `atticRestore(id)`, `atticFork(id, dir)`, `atticRemove(id)` |
| `GET /api/queries?folder=` | `listQueries(folder)`: the saved queries `[{name, folder, description, params, status, error?, writes, effects, denied_writes}]` (an effect `{action, element, label, text}`) |
| `GET /api/queries/get?name=` | `getQuery(name)`: one with its `body`, `dialect`, `meta`; `null` when there is none |
| `POST /api/queries/define` `{name, folder?, description?, params?: [{name, schema, default?}], body, replaces?}` | `defineQuery(spec)`: `{ok: true, query}` or `{ok: false, errors: [{field, message, help?}]}` (always 200); `replaces` renames in the same commit |
| `POST /api/queries/drop` `{name}`, `POST /api/queries/move` `{name, folder}` | `dropQuery(name)`: `{dropped}`; `moveQuery(name, folder)`: the entry |
| `POST /api/queries/call-text` `{name, args}` | `queryCallText(name, args)`: `{text}`, the call `g.query("name", #{..})` with the arguments as Rhai literals |
| `POST /api/queries/preview` `{name, args}` | `previewQuery(name, args)`: the call run as a dry run and rolled back, `{ok: true, changes: {vertices_added, vertices_dropped, edges_added, edges_dropped, properties_set, properties_removed, labels_changed, other}}` or `{ok: false, error}`; the run form shows it before it runs a query that writes |
| `GET /api/catalog/export`, `POST /api/catalog/load?replace=true` | `saveCatalog()`: the catalog file (`{"definitions": [..]}`: saved queries, compression rules, every kind); `loadCatalog(file)`: stores a catalog file in one commit (`replace=true`: drops what the file does not have; a compression rule compresses its values), `{loaded, graph}` |
| `GET /api/compression` | `listCompressions()`: the [compression rules](../users_guide/compressed_properties.md) `[{name, element, label, path, codec, minBytes, dictionary, stats: {compressedValues, plainBytes, storedBytes, dictionaryBytes}}]` |
| `POST /api/compression/define` `{name, label, path, element?, codec?, minBytes?, dictionary?, replaces?}` | `defineCompression(spec)`: `{ok: true, rule}` or `{ok: false, errors: [{field, message, help?}]}` (always 200); `replaces` renames in the same commit |
| `POST /api/compression/drop-many` `{names: [..]}` | `dropCompressions`: drops the rules in one commit, answers `{dropped}` (how many; a name not stored is skipped) |
| `POST /api/compression/drop` `{name}`, `POST /api/compression/recompress` `{name}` | `dropCompression(name)`: `{dropped}`; `recompress(name)`: the rule's entry (422 when there is none) |
| `POST /api/compression/estimate` `{label, path, element?, codec?, minBytes?, dictionary?}` | `estimateCompression(spec)`: `{ok: true, estimate}` or `{ok: false, errors: [{field, message, help?}]}` (always 200); [changes nothing](../users_guide/compressed_properties.md#estimating-the-gain) |
| `GET /api/indexes` | `listIndexes()`: the [property indexes](../users_guide/property_indexes.md) `[{name, element, label, key, type, transform, served, reason, stats: {entries, distinctValues, skipped, bytes} \| null}]` (`transform` is `null` or `"to_lower"`; `stats` is `null`, `served` false and `reason` a text when the storage keeps the definition but serves no index, or the index type or transform is unknown to this version; `reason` is `null` otherwise) |
| `POST /api/indexes/create` `{label, key, element?, name?, type?, transform?}` | `createIndex(spec)`: `{ok: true, index}` or `{ok: false, errors: [{field, message, help?}]}` (always 200; `field` is `name`, `element`, `label`, `key`, `type`, `transform` or `general`; an unknown `type` is an error of `type`, an unknown `transform` of `transform`, a key the schema declares as values the index cannot hold one of `key`); `name` defaults to `<label>_<key>` (`<label>_<key>_edge` for an edge index, then `_to_lower` with that transform), `type` to `memory-ordered`, `transform` to none (`"to_lower"`: case-insensitive search with `has(key, __.to_lower().is(..))`); an existing name is refused (drop it first) |
| `POST /api/indexes/drop` `{name}` | `dropIndex(name)`: `{dropped}` |
| `POST /api/memory/report` (a spec `{sample_every?, max_elements?, limit?, max_rows?, max_labels?}`, `{}`: a full walk, at most 500 rows per list) | `memoryReport(spec)`: the [memory report](../users_guide/memory_and_profiling.md), `{ok, report}` or `{ok: false, error}` |
| `POST /api/advise` (thresholds, `{}`: the defaults), `POST /api/advise/apply` `{id, spec}` or `{ids, spec}` (several in one commit), `POST /api/advise/dismiss` `{id, days?}`, `POST /api/advise/restore` `{id}` | `advise(spec)`, `applyRecommendation(id, spec)`, `applyRecommendations(ids, spec)`, `dismissRecommendation(id, days)`, `restoreRecommendation(id)`: the [storage advisor](optimize.md) |
| `GET /openapi.json`, `GET /api/openapi.json` | the OpenAPI description of this API, with this server's version |

A `target` is `{"commit": N}`, `{"mark": "name"}` or `{"time": T}` with `T` an RFC 3339 date-time
(as the CLI's `--at-time`) or microseconds. `GET /api/store` lists every attic entry with
`bytes`, `restorable` and `restoreBlocked` (the reason a restore is refused now). A change of the
catalog (a saved query or a compression rule defined, replaced or dropped, a property index
created or dropped) is a commit like any other: the change feed records it, and on a Store it is journaled. A `recompress` changes no data
and commits nothing. Any other path is a static file of the UI (`--ui-dir` first, then the embedded copy).
