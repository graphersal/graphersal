# Dev Server and MCP

> **DEV mode, unstable, local only, temporary. Not for production.** The dev server is a way to try Graphersal "as a
> small server" and watch how it behaves. Its HTTP API is internal to the playground and changes
> without notice; it has no authentication and serves only the loopback (`127.0.0.1`, and `[::1]`
> when the system has IPv6). Do not expose it.

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
- The playground UI is **embedded in the binary** (its static files: `index.html`, `css/`, `js/`,
  `assets/`, `reset/`), so `graphersal` serves it on its own, from any working directory.
  `--ui-dir <DIR>` names a directory whose files win over the embedded copy (for working on the
  UI); a file missing there comes from the embedded copy. Without `--ui-dir`, `./playground` is
  used that way when it exists (a repository checkout), else the embedded UI alone. An explicit
  `--ui-dir` that does not exist is a usage error. The stderr banner says where the UI comes from:
  `UI: embedded` or `UI: <dir> (embedded fallback)`. A path leaving the UI root (`..`) is a 404.
- After the graph is in memory, stderr shows **how long loading took and what was loaded**, from
  the same data as the playground's Profile -> Statistics tab: the load time (sample build, file
  import with `--schema`, `.gsnap` load, or Store open = snapshot load + WAL replay, with the
  number of WAL commits replayed), the vertex and edge totals, the ten largest vertex and edge
  labels (`+N more` for the rest), the graph's memory (structure, indexes, properties, and the
  [property indexes](../users_guide/property_indexes.md) when there are any, as
  `..., property indexes 2.0 KiB)`) with the process heap, and the compressed values when there
  are any:

  ```text
  Loaded in 34.3 ms: 6 vertices, 6 edges
  Vertex labels (2): person 4, software 2
  Edge labels (2): created 4, knows 2
  Memory: graph 5.5 KiB (structure 2.2 KiB, indexes 1.1 KiB, properties 2.2 KiB); process heap 1.9 MiB
  Limits: none (start with --timeout, --memory-limit, --safe-limits, --max-* to bound queries)
  ```

  On a Store the first line reads `Loaded in 53.6 ms (snapshot + 2 WAL commits replayed): ...`.
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
  "embedder": ["potion=minishlab/potion-base-8M"],
  "timeout": 30000
}
```

```bash
graphersal --args-file args.json                 # everything from the file
graphersal @args.json --port 9000                # the command line wins: port 9000
graphersal --args-file args.json --embedder b=./m   # a repeatable flag adds to the file's
```

- A switch (`server`, `mcp`, `no-limit`, ...) is `true`; `false` is the same as leaving it out.
  A number is written as a number (`"port": 8080`), a string as a string. `eval` and `embedder`
  repeat: a string or an array of strings. Every flag of `graphersal --help` is a key except
  `help`, `version` and `args-file`.
- **Relative paths** (`graph`, `schema`, `ui-dir`, `ai`, `mcp-instructions`, and the path of an
  `embedder`) resolve against the directory of the args file, not the working directory. The
  sample names (`modern`, `empty`, `large`, `tree`) and model repositories stay as written.
- **The command line wins.** The file's arguments come first, as if typed before the others: a flag
  given on the command line replaces the file's, a repeatable flag (`--eval`, `--embedder`) adds to
  it. The usual validation runs on the merged arguments, so a file with `"server": true` plus
  `-e` on the command line fails as `--server -e` does.
- A file may not name another args file, and `--args-file` may appear once. An unreadable file,
  invalid JSON, a value of the wrong type or an unknown key is a usage error (exit code 2) that
  names the key; values are never echoed. Unlike the [`--ai` file](#the-configuration-file),
  where an unknown key only warns, an unknown key here is an error (a near-typo adds
  `did you mean "server"?`): a flag that is silently dropped could switch off `mcp-read-only`
  or `mcp-token`. Only a key starting with `_` is a silent comment.

The file holds no secrets by design (give `--mcp-token` on the command line or keep the file
private); the repository has an example, `crates/graphersal-cli/examples/args-dev-server.json`.

## Limits

The limit options become **session presets**: every query sent to the server runs under them, as
every script of the one-shot runner does (the [resource limits](../users_guide/resource_limits.md)
page explains each limit).

| Option | What every query gets |
|---|---|
| `--timeout <MS>` | `evaluationTimeout` on every traversal, and the budget of a query as a whole (a `loop {}` between traversals stops too, with "Script exceeded evaluationTimeout of N ms") |
| `--memory-limit <BYTES\|auto>`, `--memory-headroom <BYTES>` | `memory.limit` / `memory.headroom`: a query that would take the server's live heap over the budget fails with "Resource limit 'memory.limit'" and rolls back, instead of the process being killed |
| `--max-traversers <N>` | `traversal.max_traversers` |
| `--max-string-size <N>` | the script's string limit and `traversal.max_string_bytes` |
| `--max-value-depth <N>` | the script's value depth and `traversal.max_value_depth` |
| `--max-operations`, `--max-array-size`, `--max-map-size` | the Rhai limits of the server's script engine |
| `--safe-limits` | the library's SAFE script limits and the engine's traversal defaults |

A query overrides a traversal option with `g.with(..)` (for example
`g.with("evaluationTimeout", 0)`), as in the CLI. The values in force are listed on stderr at
start ("Limits of every query: ...", or "Limits: none (...)" without limit options) and in
`GET /api/info` (`presets`). Without limit options the
server runs like the playground: no script limits, the engine's traversal defaults, no time
budget; Stop is the way to end a runaway query.

```bash
graphersal --graph large --server --timeout 5000 --memory-limit auto
```

## Embedding models

`--embedder NAME=PATH|REPO` (repeatable) and `--default-embedder NAME` work with `--server` as
with every other mode: the Model2Vec models are loaded once at start (a model that does not
load stops the start with exit code 2), and every query of the server, the MCP `query` tool and
the AI Chat's tools call the same registry (`embed()`, the `embed` step, `g.embedders()`).
`GET /api/info` lists it as `embedders`. Running a model is local; only `graphersal embed-model
download` connects to Hugging Face ([Embeddings](../getting_started/embeddings.md#real-models-model2vec)).

```bash
graphersal embed-model download minishlab/potion-base-8M
graphersal --graph data/ --server --mcp --embedder potion=minishlab/potion-base-8M --default-embedder potion
```

## What the UI does in this mode

- A red **DEV SERVER · unstable** badge in the top bar. There is no start page and no graph choice:
  the server serves the one graph it was started with. To use another graph, restart the server.
- Queries, results, profiles, the graph drawing, the schema views and the schema editor work as
  in the WebAssembly playground: both run the same session code (the internal crate
  `graphersal-session`), so the answers are the same structures.
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
- **Stop** cancels the running query on the server: it stops at its next check (where
  `evaluationTimeout` is checked, plus the script loop), fails with "cancelled", and everything it
  changed is rolled back. The graph stays as it was before the query; nothing restarts. Stop
  cancels whatever query runs, also one started from another tab.
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

## Safety

The server is a local development tool: it binds the loopback only, `127.0.0.1` and, when the
system has IPv6, `[::1]` on the same port (stderr then says `Also listening on
http://[::1]:<port>/ (IPv6 loopback)`; if that bind fails the server stays on IPv4, silently;
the URL on stdout is always `http://127.0.0.1:<port>/`). There is no option to bind elsewhere. It
has no authentication, and refuses requests whose `Host` (`127.0.0.1`, `localhost` or `[::1]` with
the server's port) is not the server itself or
whose `Origin` is another site; every write needs `Content-Type: application/json`, which a page of
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
merge bound). A whole script is ONE commit here, and one commit is at most 1 GiB of
uncompressed WAL record: a script that drops and re-creates a large graph can be refused as a
whole; run the drop on its own and load in several runs
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
    deletes it for good. The server cancels the running query, rolls the store back in place and
    serves the result at once; the Graph, Schema and Statistics panels reload and every tab's
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
# MCP endpoint: http://127.0.0.1:9000/mcp              (stderr, with the claude mcp add command)
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

Resources (read with `resources/read`): `graphersal://schema` (the stored schema),
`graphersal://statistics`, `graphersal://dsl-reference` (every step and function of the language
with its signatures, both spellings, a one-line description and a runnable example with its result,
generated from the engine's function docs, about 55 KB),
`graphersal://examples` (the playground's examples, about 8 KB) and `graphersal://saved-queries`
(the list of `list_saved_queries`).

**Saved queries first.** The server's instructions and the `query` tool's description tell the
agent to look for a saved query (`list_saved_queries`) before it writes a new query, and to run
it with `run_saved_query`: the queries a user stored for recurring questions are reused instead of
rewritten. A query listed with `writes: true` changes data (its `effects` say what), and the
instructions tell the agent to run one only when the user asked for that change. Both tools are
offered with `--mcp-read-only` too, which lists only the queries that do not write and refuses a
call of one that does. With `--mcp-query-tools`, `tools/list` also lists every saved query as a
tool named like the query, with its description (and folder; a query that writes says so and
names its effects), an `inputSchema` built from its parameter schemas (defaults included,
required parameters in `required`, no other properties) and annotations from its effects
(`readOnlyHint` false for a query that writes, `destructiveHint` when it updates or deletes);
calling it is `run_saved_query` with those arguments. `tools/list` reflects the catalog at the time of the
call; the endpoint has no event stream, so it announces `tools.listChanged: false` and a client
lists the tools again to see a changed catalog. A saved query named like a built-in tool
(`statistics`, ...) is not listed as a tool of its own; `run_saved_query` reaches it.

The agent's queries share everything with the browser: one query runs at a time, **Stop** in the
browser also cancels a running agent query (it rolls back), a read-only view opened by either side
is what both see, and the [limits](#limits) apply.

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
[Safety](#safety) section apply (another site's `Origin` is `403`), and the server still binds
the loopback only: `--mcp-token` protects against other local users and programs, not against the
network.

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
- per question, the steps that led to the answer in ONE collapsed group above it (e.g. "Worked:
  2 tool calls (list_saved_queries, query)", "Working: running query" while the turn runs,
  "Stopped after 1 tool call: list_saved_queries" when it failed); opened, it lists the agent's
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
| `max_tool_calls_per_turn` | `20` | The most tool calls one turn (one question) may make; at the limit the turn ends with a message that says so, and "go on" continues it |
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
instructions are unchanged (and the `--mcp-instructions` notes are not added). The prompt prefix stays fixed for the conversation, so the provider's
prompt cache still works.

### What data leaves the machine

Everything the agent sees goes to the provider: your messages, the system prompt (the MCP
instructions, the agent's rules, your [custom system prompt](#custom-system-prompt) text if any, the [DSL reference](#what-the-agent-gets) and a short summary of
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
  whole history with every call.

## The HTTP API (internal)

For reference while it exists; it follows the playground's engine boundary
(`playground/js/engine/engine.js`) and may change in any release. Answers are JSON; errors are
`{"error": "..."}` with a 4xx status (422 for a failing schema operation).

| Endpoint | Engine method |
|---|---|
| `GET /api/info` | `info()` (`server: "graphersal-dev"`, `version`, `saveable`, `source`, `presets`: the limits in force, `mcp`: `{endpoint, readOnly, client}` or null, `ai`: `{provider, model, readOnly, sendsDataTo, maxToolCallsPerTurn}` or null (never the key; `sendsDataTo` is the provider's host, null for one on this machine), `damageFound`: `{text, frozen, foundBy, policy, problems}` or null, `embedders`: `[{name, model, dimension, default}]`, the built-in `hash-ngram/1` and every `--embedder`) |
| `GET /api/changes` | `changes()`: the change feed `{version, commitSeq, recent: [{version, by, commitSeq, detail?}], view, mcp, damageFound}`; `by` is the page's `X-Graphersal-Client` header (every request of the page sends it), `mcp`, or `ai` (an AI Chat conversation, its id in `detail`) |
| `POST /mcp` | the MCP endpoint (with `--mcp`, [above](#mcp-an-ai-agent-on-the-same-graph)) |
| `POST /api/ai/conversations` | with `--ai` (EXPERIMENTAL): a new AI Chat conversation `{id}` (random, 128 bits) owned by the page's `X-Graphersal-Client` (required); every other client gets `403` for it, an unknown id is `404`, and without `--ai` every `/api/ai/*` is `404` |
| `POST /api/ai/conversations/<id>/messages` `{text}` | starts a turn on the server (`{turn}`; `409` while one runs): the provider is called, the tools it asks for run on the session like MCP tool calls (commits in the change feed as `ai`), until it answers without a tool call or reaches `max_tool_calls_per_turn` |
| `GET /api/ai/conversations/<id>/events?after=N` | `{events, running, last}`: the events after `N`, each with `seq`: `user {text}`, `assistant {text}`, `tool_call {id, name, args}`, `tool_result {id, name, ok, summary, text}`, `error {message, help}`, `done {stopped, toolCalls, elapsedMs, toolMs}` (the turn's wall time and the part of it spent in tool calls, in milliseconds; the page shows them under the answer, e.g. "12.4 s · tools 3.1 s") |
| `POST /api/ai/conversations/<id>/stop` | `{stopped}`: ends this conversation's turn only: its pending provider call is abandoned, the query it runs is cancelled and rolled back; other conversations are not affected (and `POST /api/cancel` does not stop the agent's query) |
| `DELETE /api/ai/conversations/<id>` | drops the conversation (history, pending call, running query) |
| `GET /api/graph` | `graphInfo()` |
| `POST /api/execute` `{script, options}` | `execute(script, options)` |
| `POST /api/cancel` | `stop()`: `{cancelled}` |
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
| `POST /api/layout/step` | `layoutStep(job, budgetMs)`: the next slice (`{job, budgetMs?}`, default 100 ms, at most 500); Stop (`POST /api/cancel`) ends a running slice as `cancelled` |
| `POST /api/layout/cancel` | `layoutCancel(job)`: ends the job, keeping the positions reached so far |
| `GET /api/completions`, `GET /api/token-values`, `GET /api/memory` | `completions()`, `tokenValues()`, `memoryUsage()` |
| `GET /api/statistics` | `statistics()`: `{statistics, memory, process, limits: {presets, defaults}}`. `process` is the memory the process holds beside the graph (`GraphStatistics` stays data counts): `memory` is the whole process (`liveHeapBytes`: the bytes the Rust allocator holds, from the CLI's tracking allocator; `rssBytes` and `peakRssBytes`: the resident set size, from `/proc/self/status` `VmRSS`/`VmHWM` on Linux and from `ps -o rss=` on macOS, where the peak is the highest value any report of this server has seen; no RSS on Windows or other platforms; each member is left out when unknown; `source` is `proc`, `ps`, `allocator` or `none`; the WebAssembly playground adds `wasmMemoryBytes`, the linear memory size, `source` `wasm`), `models` lists every loaded embedding model that reports its size (`name`, `model`, `dimension`, `bytes`, `default`; an estimate of the weights and the tokenizer), and `aiChat {conversations, historyBytes}` (an estimate of the stored chat history) is added when the server runs with `--ai`. The MCP `statistics` tool returns the same |
| `GET /api/workload[?history=minutes\|hours\|days]` | `workloadReport(history)`: the [workload statistics](workload_statistics.md) of the served graph (`{level, queries, properties, locks, jobs}`; level `off` outside a Store); with `history` every row's buckets of that tier (chart data; an unknown tier is a 422). Only the queries a page or an agent runs count (under `playground` / `mcp`); the page's drawing and panels are not counted |
| `GET /api/jobs`, `POST /api/jobs/define\|drop\|enable\|schedule\|run`, `GET /api/jobs/history?name=..&tier=..` | `jobs()`, `defineJob(spec)`, `dropJob(name)`, `setJobEnabled(name, on)`, `setJobSchedule(name, cron)`, `runJobNow(name)`, `jobHistory(name, tier)`: the [scheduled jobs](scheduled_jobs.md) (list, define with field errors, switch, a system job's schedule, run now, the runs per minute/hour/day); a backup job may name any directory here, never in the browser |
| `GET /api/export?format=graphson\|graphml` | `saveGraph(format)` (download) |
| `GET /api/export?format=gsnap` | `saveGraph('snapshot')`: the current state as a packed snapshot (binary download) |
| `POST /api/save` | Save to file: `{path, format, bytes, warnings}`, plus `schemaPath` when the schema was written to the `--schema` file, or `warning` when a stored schema could not be saved (GraphML/GraphSON without `--schema`) |
| `GET /api/store` | `storeInfo()`: `{store: null}` without a Store, else position, snapshots, marks, WAL, the creation parameters (`storeId`, `damagePolicy`, `createdText`, `chunkBytes`), `damageFound` (`[{foundBy, atText, problems}]`), `frozenByDamage`, `damageText`, `backupFromMemory` |
| `POST /api/store/checkpoint` `{name}`, `POST /api/store/mark` `{name}` | `checkpoint(name)`, `mark(name)` (422 with the diagnostic on failure; 409 without a Store) |
| `GET /api/store/snapshot?commit=N` | a stored snapshot as `.gsnap` (no `commit`: the latest) |
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
| `POST /api/compression/drop` `{name}`, `POST /api/compression/recompress` `{name}` | `dropCompression(name)`: `{dropped}`; `recompress(name)`: the rule's entry (422 when there is none) |
| `POST /api/compression/estimate` `{label, path, element?, codec?, minBytes?, dictionary?}` | `estimateCompression(spec)`: `{ok: true, estimate}` or `{ok: false, errors: [{field, message, help?}]}` (always 200); [changes nothing](../users_guide/compressed_properties.md#estimating-the-gain) |
| `GET /api/indexes` | `listIndexes()`: the [property indexes](../users_guide/property_indexes.md) `[{name, element, label, key, type, transform, served, reason, stats: {entries, distinctValues, skipped, bytes} \| null}]` (`transform` is `null` or `"to_lower"`; `stats` is `null`, `served` false and `reason` a text when the storage keeps the definition but serves no index, or the index type or transform is unknown to this version; `reason` is `null` otherwise) |
| `POST /api/indexes/create` `{label, key, element?, name?, type?, transform?}` | `createIndex(spec)`: `{ok: true, index}` or `{ok: false, errors: [{field, message, help?}]}` (always 200; `field` is `name`, `element`, `label`, `key`, `type`, `transform` or `general`; an unknown `type` is an error of `type`, an unknown `transform` of `transform`, a key the schema declares as values the index cannot hold one of `key`); `name` defaults to `<label>_<key>` (`<label>_<key>_edge` for an edge index, then `_to_lower` with that transform), `type` to `memory-ordered`, `transform` to none (`"to_lower"`: case-insensitive search with `has(key, __.to_lower().is(..))`); an existing name is refused (drop it first) |
| `POST /api/indexes/drop` `{name}` | `dropIndex(name)`: `{dropped}` |

A `target` is `{"commit": N}`, `{"mark": "name"}` or `{"time": T}` with `T` an RFC 3339 date-time
(as the CLI's `--at-time`) or microseconds. `GET /api/store` lists every attic entry with
`bytes`, `restorable` and `restoreBlocked` (the reason a restore is refused now). A change of the
catalog (a saved query or a compression rule defined, replaced or dropped, a property index
created or dropped) is a commit like any other: the change feed records it, and on a Store it is journaled. A `recompress` changes no data
and commits nothing. Any other path is a static file of the UI (`--ui-dir` first, then the embedded copy).
