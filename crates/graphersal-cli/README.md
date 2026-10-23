# graphersal-cli

The command line tool of [Graphersal](https://graphersal.dev), an in-memory graph database and
traversal engine queried with Gremlin. It installs the binary `graphersal`: a one-shot query runner
for shell scripts, an interactive REPL, a local server with the playground UI and an MCP endpoint
for AI agents, and `graphersal store` for graphs kept on disk.

**[Book](https://graphersal.dev)** · **[Playground](https://play.graphersal.dev)** ·
**[Repository](https://github.com/graphersal/graphersal)**

```bash
cargo install graphersal-cli      # or: cargo binstall graphersal-cli (prebuilt)
```

Prebuilt binaries for Linux, macOS and Windows are on the
[releases page](https://github.com/graphersal/graphersal/releases); checksums, WSL and the note
on the musl-linked Linux binaries are on
[Installation](https://graphersal.dev/getting_started/installation.html).

## One-shot queries

```bash
graphersal -e 'g.v().has("name", "marko").out("knows").values("name")'   # the TinkerPop "modern" sample
graphersal --graph people.json -e 'g.v().has_label("person").count()'     # GraphSON (.json/.jsonl/.graphson)
graphersal --graph people.graphml --format mermaid -e 'g.infer_schema()'  # GraphML (.xml/.graphml)
echo 'g.v().values("age").max().next()' | graphersal --graph modern       # the query from stdin
```

stdout carries only the result, stderr only diagnostics. Exit codes: `0` ok, `1` the query failed,
`2` invalid invocation, `141` stdout closed early (`| head -1`), so a script or a CI job can check
a graph:

```bash
graphersal --graph export.json -e 'g.v().has_label("server").has_not("owner").fail("a server without an owner")'
```

`--graph` takes `modern` (default), `empty`, `large` (about 110,000 elements), `tree`, a GraphSON or
GraphML file, a `.gsnap` snapshot or a Store. `--schema schema.json [--schema-mode open|closed|none]`
sets a schema on the new graph before the file is imported against it. Displayed results are cut
at 100 rows (`--max-rows`, `--no-limit`); `--timeout`, `--memory-limit` and the script limits
bound every query. `graphersal --help` lists every option.

## REPL

```bash
graphersal --graph people.json
```

`graphersal>` completes steps, tokens and commands with Tab and documents each of them with
`/help <name>` (`/help has_label`, `/help P.eq`); `/help` lists the commands.

## Dev server, MCP and AI Chat (local only, unstable)

```bash
graphersal --graph people.json --server --mcp          # open http://127.0.0.1:8080/
claude mcp add --transport http graphersal http://127.0.0.1:8080/mcp
```

`--server` serves the playground UI (embedded in the binary) on the loopback interface and runs
its queries in this process: tables, the graph drawing, schema, profile, Stop, and "Save to file"
back to the `--graph` file. `--args-file FILE` (or `@FILE`) reads flags from a JSON file. `--mcp` adds a Model Context Protocol endpoint on the same graph, so an
AI agent works next to you in the browser (`--mcp-read-only`, `--mcp-token <secret>`, `--mcp-instructions FILE`); the page
reloads when the agent changed the graph. `--ai config.json` enables the playground's AI Chat tab
with your own LLM provider (experimental). There is no authentication and the HTTP API changes
without notice. See [Dev Server, MCP and AI Chat](https://graphersal.dev/getting_started/server_mcp_ai.html).

## The Store

```bash
graphersal store create data/ --from modern                # one directory (or data.gstore: one file) = one graph
graphersal --graph data/ -e 'g.add_v("person").iterate()'  # every commit is durable before it returns
graphersal --graph data/ --server                          # the dev server with a Store menu
graphersal store help                                      # info, marks, checkpoint, verify, fork, rollback,
                                                           # backup, restore, export, compact, repair, ...
```

Reference: [The Store from the Command Line](https://graphersal.dev/persistence/store_cli.html);
every command on one page: the
[Command Cheat Sheet](https://graphersal.dev/users_guide/command_cheat_sheet.html).

Graphersal is compatible with, and inspired by, Apache TinkerPop Gremlin. It is an independent
project: it is not part of Apache TinkerPop and is not endorsed by the Apache Software Foundation.

Licensed under the Apache License, Version 2.0 (see `LICENSE-APACHE` and `NOTICE`).
