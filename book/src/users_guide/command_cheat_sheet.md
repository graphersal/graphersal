# Command Cheat Sheet

Every command you need to build, run and operate Graphersal, ready to copy, each with one line of
explanation. Run them from the root of a checkout of the repository. Every command on this page
was run against the built binary; when the binary says something else, the binary is right and
this page is wrong.

The examples write `graphersal` for the binary. After a debug build it is
`target/debug/graphersal`, after a release build `target/release/graphersal`; put one of them on
your `PATH` or type the path.

## Build

```bash
cargo build -p graphersal-cli                 # debug build: target/debug/graphersal
cargo build -p graphersal-cli --release       # optimized build: target/release/graphersal
target/debug/graphersal --version             # check what you built
playground/build.sh                           # the WebAssembly playground into playground/pkg/
```

`playground/build.sh` needs two one-time installs: `rustup target add wasm32-unknown-unknown`
and `cargo install --locked wasm-bindgen-cli --version 0.2.129` (it must equal the
`wasm-bindgen` crate version). `wasm-opt` (binaryen) is optional and makes the module smaller.

## The web playground (WebAssembly, in the browser)

```bash
python3 playground/serve.py 8000              # open http://localhost:8000/
```

`serve.py` is a static file server that sends `Cache-Control: no-store`. Plain
`python3 -m http.server` sends no cache headers, so after a rebuild the browser keeps running the
old JavaScript modules and the old engine worker. If the page hangs after a reload, open
`http://localhost:8000/reset/` (it clears the playground's saved state). See
[WEB Playground](web_playground.md).

## One-shot queries and the REPL

```bash
graphersal                                                       # the REPL on the modern graph
graphersal --graph empty                                         # the REPL on an empty graph
graphersal -e 'g.v().has_label("person").values("name").to_list()'   # run, print, exit
graphersal -e 'let xs = g.v().to_list()' -e 'xs.len()'           # -e repeats and shares one scope
echo 'g.v().values("age").max().next()' | graphersal --in --graph modern   # the script from stdin
printf '/help has_label\n' | graphersal --repl                   # the REPL on a piped stdin (scripted sessions)
```

Embedding models ([Model2Vec](../getting_started/embeddings.md#real-models-model2vec); the
download is the only network access, a model runs locally):

```bash
graphersal embed-model download minishlab/potion-base-8M          # into $GRAPHERSAL_EMBED_MODELS or ~/.cache/graphersal/embed-models
graphersal embed-model download minishlab/potion-base-8M --revision v1.0 --dir ./models   # a pinned revision, another directory
graphersal embed-model list                                        # repository, size, model identity
graphersal --embedder potion=minishlab/potion-base-8M --default-embedder potion -e 'embed("graph databases")'
graphersal --embedder mine=./models/my-model -e 'g.v().embed("name", #{embedder: "mine"}).to_list()'   # a model directory
```

In the REPL a script runs when its input ends with `;`. Commands start with `/` (an input starting
with `:` gets a pointer to its `/` form); Tab completes steps, token values (`P.`, `Order.`, `T.`,
...), commands and their arguments, in the session spelling. Completion and help read the same
catalogue as the [playground](web_playground.md)'s query editor: the engine's own function docs.

| Command | What it does |
|---|---|
| `/help` | the commands, grouped |
| `/help <name>` | the doc of a step, function, token class or command in either spelling, with its overloads and an example with its result (`/help has_label`, `/help hasLabel`, `/help P.eq`, `/help Order`) |
| `/help steps`, `/help tokens` | every step and terminal; the token classes with their values |
| `/set format <fmt>` | the default visualizer |
| `/set max-rows <n>`, `/set max-items <n>`, `/set no-limit` | the [display limits](displaying_results.md) |
| `/set spelling <snake\|camel>` | the spelling of completion, help, `.profile()` and errors |
| `/set max-operations <n>`, ..., `/set safe-limits` | the [script limits](resource_limits.md) (`/help set` lists them) |
| `/exit` (or Ctrl-D) | close the graph and leave |

`--graph` takes:

| Source | What it is |
|---|---|
| `modern` | TinkerPop's "modern" sample graph (the default) |
| `empty` | an empty graph |
| `large` | a generated graph of about 110k vertices and 110k edges |
| `tree` | a file-tree sample (37 vertices) for `glob_path` |
| `g.json`, `g.jsonl`, `g.graphson` | a GraphSON file ([GraphSON](graphson.md)) |
| `g.xml`, `g.graphml` | a GraphML file |
| `g.gsnap` | a [packed snapshot](../persistence/packed_snapshots.md) |
| `data/`, `g.gstore` | a [Store](../persistence/store.md): opened read-write, every commit is durable |

```bash
graphersal -e 'g.export_snapshot("m.gsnap")'                    # save the graph as a packed snapshot
graphersal --graph m.gsnap -e 'g.v().count().next()'             # and load it
graphersal -e 'g.export_graphson("m.json")'                      # GraphSON (TinkerPop's g.io() format)
```

A graph file carries no schema; `--schema` brings one (the [schema format](schema_enforcement.md)):

```bash
graphersal --graph g.graphml --schema schema.json -e 'g.v().count().next()'   # schema first, then the file imported against it
graphersal --graph g.json --schema schema.json --schema-mode closed           # --schema-mode overrides the file's "mode"
graphersal --graph modern --schema schema.json                                # a sample graph: the schema is applied (validated)
```

The schema is set on the new empty graph first and the file is imported as one unit, so declared
logical types (`uuid`, `array`, `object`) come back and an `open`/`closed` mode is enforced. Data
that violates the schema stops the start with exit code `1` and the violation report; an
unreadable or invalid schema file, an unknown `--schema-mode`, and `--schema` with a Store or a
`.gsnap` (both carry their own schema) are exit code `2`.

Saved queries (stored in the database, called by name; a query whose body writes data writes as one transaction; see [Saved Queries](saved_queries.md)):

```bash
graphersal -e 'g.define_query(#{name: "people", body: "fn people() { g.v().has_label(\"person\") }"})' \
           -e 'g.query("people").values("name").to_list()'    # define, then call and continue
graphersal -e 'g.queries()'                                       # list (also g.queries("folder"), g.get_query(name))
```

Display (a result without a data terminal is rendered; `to_list()`/`next()` are never cut; `iterate()` runs
for the side effects and shows nothing):

```bash
graphersal -e 'g.v().has_label("person").values("name")' --format json   # auto, table, markdown, json, jsonschema, mermaid, plantuml, tree
graphersal -e 'g.v()' --max-rows 2            # at most 2 rows (default 100); --max-items N for nested items
graphersal -e 'g.v().values("name")' --no-limit
graphersal --spelling camel -e 'g.v().has_label("person").count().profile()'   # Gremlin spelling in profiles and errors
```

Limits (scripts run without limits by default; see [Resource Limits](resource_limits.md)):

```bash
graphersal --graph large --timeout 50 -e 'g.v().out().out().out().out().count().next()'  # 50 ms per traversal and script
graphersal --memory-limit auto -e 'g.v().count().next()'         # memory budget: the cgroup limit minus a headroom
graphersal --memory-limit 2000000000 -e 'g.v().count().next()'  # an explicit budget in bytes
graphersal --safe-limits -e 'g.v().count().next()'               # the library's SAFE script limits
graphersal --max-operations 1000 -e 'let n = 0; loop { n += 1; }' # stops with "Resource limit 'script.max_operations'"
graphersal --max-traversers 1000000 --max-value-depth 64 -e 'g.v().count().next()'  # also --max-string-size, --max-array-size, --max-map-size
```

Exit codes: `0` ok, `1` the script failed (diagnostic on stderr), the `--graph` data violates
the `--schema`, or a `--graph <store>` cannot be opened (in use by another writer, ...), `2` invalid invocation (unknown option, a graph or schema file that cannot be
loaded), `141` stdout was closed before every result was written (`graphersal -e '..' | head -1`:
the run stops quietly; a failing script keeps its own code; the same for `graphersal store`).
A closed stdout or stderr is never a crash: the dev server keeps serving when its log reader
goes away. `graphersal --help` lists every option.

## The dev server

DEV mode, unstable, local only (127.0.0.1, no authentication). It serves the playground UI with
this process as the engine. Details: [Dev Server and MCP](../persistence/dev_server.md).

```bash
graphersal --graph modern --server                       # http://127.0.0.1:8080/
graphersal --graph ./mygraph.json --server --port 9000   # any --graph source; "Save to file" writes it back
graphersal --graph large --server --port 0               # 0 picks a free port (the address is printed on stdout)
graphersal --graph data/ --server                        # a Store: every commit is durable, the Store menu appears
graphersal --graph new/ --create-store --server          # create the store when it does not exist
graphersal --graph g.graphml --schema schema.json --server   # a graph file loaded against its schema
graphersal --graph large --server --timeout 5000 --memory-limit auto   # limits apply to every query
graphersal --graph modern --server --ui-dir ~/src/graphersal/playground # its files win over the embedded UI
graphersal --graph modern --server --embedder potion=minishlab/potion-base-8M   # a Model2Vec model for every query
```

Stop it with **Ctrl+C**: a running query is cancelled and an open store is closed cleanly. A
second Ctrl+C exits at once. The display options (`--format`, `--max-rows`, ...) and `-e`/`--in`
are refused with `--server` (exit 2).

## MCP: an AI agent on the same graph

```bash
graphersal --graph modern --server --port 9000 --mcp                    # MCP endpoint http://127.0.0.1:9000/mcp
graphersal --graph modern --server --port 9000 --mcp --mcp-read-only    # the agent may only read
graphersal --graph modern --server --port 9000 --mcp --mcp-token my-secret-token   # the agent must send the token
graphersal --graph data/ --server --port 9000 --mcp --mcp-query-tools  # also every saved query as a tool of its own
graphersal --graph data/ --server --mcp --mcp-instructions notes.md      # operator notes in the MCP instructions
graphersal --args-file args.json                                         # the flags from a JSON file (also @args.json)
```

Connect Claude Code (the server must be running; then start a NEW `claude` session):

```bash
claude mcp add --transport http graphersal http://127.0.0.1:9000/mcp
claude mcp add --transport http graphersal http://127.0.0.1:9000/mcp --header "Authorization: Bearer my-secret-token"
claude mcp list                    # graphersal: http://127.0.0.1:9000/mcp (HTTP) - Connected
claude mcp remove graphersal       # when you are done, or before adding it again on another port
```

## AI Chat in the playground (EXPERIMENTAL)

```bash
export ANTHROPIC_API_KEY=...                                   # the variable the file names; the key stays in the server
graphersal --graph modern --server --ai ai.json                # "+" in the Query panel -> AI Chat
graphersal --graph modern --server --ai crates/graphersal-cli/examples/ai-ollama.json   # a local model: no data leaves the machine
GEMINI_API_KEY=... graphersal --graph modern --server --ai crates/graphersal-cli/examples/ai-google.json   # Google Gemini (OpenAI-compatible endpoint)
```

`ai.json`: `{"provider": "anthropic"|"openai", "model": "<id>", "api_key_env": "<VARIABLE>",
"base_url": null, "read_only": false, "max_tool_calls_per_turn": 20, "max_tokens": 4096}`.
Details: [AI Chat](../persistence/dev_server.md#ai-chat).

## The Store

A Store is one directory (or one `.gstore` file) that keeps one graph durable. Reference:
[The `graphersal store` Command](../persistence/store_cli.md). `TARGET` below is one of
`--at-commit N`, `--at-time 2026-10-08T02:43:00Z` (RFC 3339 with a zone, or microseconds since
the epoch) or `--at-mark NAME`.

```bash
graphersal store create data/ --from modern          # a new store (or: --from empty|large|tree|FILE|FILE.gsnap)
graphersal store create graph.gstore --from modern   # a single-file store (or: --single-file)
graphersal store create data/ --on-damage continue   # keep committing when damage is found while open (fixed at creation)
graphersal --graph data/ -e 'g.add_v("person").property("name", "zoe").to_list()'   # a durable commit
graphersal --graph data/                             # the REPL on the store (--server: the dev server)
graphersal store mark data/ before-import            # name the current position
graphersal --graph data/ -e 'g.add_v("person").property("name", "ada").to_list()'
graphersal store info data/                          # identity, position, snapshots, marks, WAL size
graphersal store list data/                          # the snapshots
graphersal store marks data/                         # the marks
graphersal store checkpoint data/ --name nightly     # write a snapshot now
graphersal store verify data/                        # scrub every checksum (exit 1 on damage)
graphersal store fork data/ branch/ --at-mark before-import          # a past state as a new store
graphersal store fork data/ branch2/ --at-time 2026-10-08T02:43:00Z  # or --at-commit N
graphersal store backup data/ /mnt/backup/data       # full the first time, incremental afterwards
graphersal store backup data/ weekly/ --full         # always full (the directory must be empty)
graphersal store backup data/ data.zip --zip         # a full backup as one ZIP archive
# every backup checks every checksum in the store and in the copy; damage stops it (repair the STORE)
# a damaged store open in the dev server: Store menu > Back up from memory (POST /api/store/backup {"from_memory": true})
graphersal store export data/ data.gsnap             # the latest snapshot as a .gsnap (--commit N: another)
graphersal store rollback data/ --at-mark before-import   # back IN PLACE; the rest goes to the attic
graphersal store attic data/                         # the attic entries (rolled-back history), with their <ID>
graphersal store attic data/ changes <ID>            # the commits of an entry
graphersal store attic data/ fork <ID> rolled-back/  # keep the rolled-back history as a new store
graphersal store attic data/ restore <ID>            # undo the rollback
graphersal store attic data/ remove <ID>             # or: delete an entry for good
graphersal store rollback data/ --at-commit 0 --delete    # back in place, deleting the rest (no undo)
graphersal store restore /mnt/backup/data            # make a backup the live store (same graph id)
graphersal store restore data.zip restored/          # unpack a ZIP backup as the live store
graphersal store prune data/ --up-to 1200            # remove history only needed before commit 1200
graphersal store compact-advice data/                # is compact worth it now? (cheap)
graphersal store compact data/                       # prune to now; a single file gives its free space back
graphersal store compact data/ --drop-marks          # ... also when marks become unreachable (refused without)
graphersal store convert data/ data.gstore           # a closed store into one file (or back)
graphersal store upgrade old/ new/                   # a store of Graphersal 0.1.x into the current format (or: old/ --in-place)
graphersal store repair damaged/ --to repaired/      # a damaged store, repaired into a new directory
graphersal store rollback --help                     # the usage of one command (or: store help rollback)
```

### Migrate a store from an older format

A store written by an older Graphersal is refused with `The store ... has the older format
version N` (nothing is changed); the error's help names the command. A store of a released
version (2: Graphersal 0.1.x) is upgraded by THIS binary, no older one needed, with everything
carried (graph, schema, catalog, history, marks, attic, identity):

```bash
cp -R old_store/ old_store.bak/                       # optional: keep the older store as it is
graphersal store upgrade old_store/ new_store/       # into a new store (old_store/ unchanged), verified
graphersal store upgrade old_store/ --in-place       # or the store itself
graphersal store upgrade old_backup/ new_backup/     # a backup directory stays a backup
graphersal store restore old_backup.zip new_store/   # a ZIP backup is upgraded as it is restored
graphersal store repair damaged_old/ --to new_store/ # a DAMAGED older store: repaired into the current format
```

A store of store format version 1 (a pre-release, before the store id) moves through a packed
snapshot instead: export it with the OLD binary (the store closed), create a new store from the
file with the new one:

```bash
old/graphersal --graph old_store/ -e 'g.export_snapshot("graph.gsnap")'   # the OLD binary: the current state
graphersal store create new_store/ --from graph.gsnap                     # this binary (or new.gstore: one file)
graphersal store verify new_store/                                        # then back it up
```

The new store has the graph, its schema and its catalog (saved queries, compression rules), with
the same element ids and commit position, but it is a new store: the marks, the attic (rolled-back
history) and the backups of the old store are not carried. `old/graphersal store export
old_store/ graph.gsnap` exports the latest stored SNAPSHOT only (run `old/graphersal store
checkpoint old_store/` first to include the WAL). Keep the old store until the new one is
verified and backed up. A `.gsnap` written by 0.1.x loads in this build as it is.

## Python

```python
import graphersal
graph = graphersal.Graph.tinkerpop_modern()
graph.save("g.gsnap")                                   # a packed snapshot
graph = graphersal.Graph.load("g.gsnap")
with graphersal.Store.create("data") as store:          # or Store.open("data"); "g.gstore": one file
    store.graph.execute('g.addV("person").property("name", "ann").next()')   # durable
    store.mark("after-ann"); store.checkpoint("nightly"); store.backup("bk")
```

More: [Python](../persistence/python.md) and `bindings/py-graphersal/README.md`.

## Tests and checks (contributors)

The full list of build, test, lint, coverage, fuzzing and wasm commands, with the acceptance
feature set, is in `CLAUDE.md` (section "Commands") at the root of the repository; the Python
binding's in `bindings/py-graphersal/README.md`, the playground's in `playground/README.md`.
