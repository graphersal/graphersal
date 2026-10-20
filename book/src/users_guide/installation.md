# Library Features

This page is about the Rust crate; the command-line tool and the Python package are on
[Installation](../getting_started/installation.md).

## Library
Graphersal is primarily provided as a library and can be easily incorporated into your Rust project by adding it as a dependency. 

To include Graphersal as a dependency in your Rust project's ```Cargo.toml``` file, refer to the following example:

```toml
[dependencies]
graphersal = "0.2"
```

### Supported Features

Graphersal comes with the several key features.

| Feature | Description |
| --------| ----------- |
| io | Reads and writes graph files: [GraphSON](graphson.md) 3.0 (TinkerPop's format; values keep their types, dates and other types Graphersal cannot hold are skipped with a report) and GraphML. GraphML values are written as their plain text (a UUID as its canonical string, arrays and objects as JSON text); logical types (`uuid`, `array`, `object`) are restored on import only from the schema stored in the target graph, so set the schema first. GraphML never carries the schema; it travels as its own JSON. |
| serde | Converts between plain JSON and Graphersal values (a UUID is written as its canonical string), and reads/writes schema files. |
| script | Graphersal also supports a Domain Specific Language (DSL) for scripting based on the Rhai library. The `math()` step does not need it: its equations run on Graphersal's own expression engine in every build (see [Math Expressions](math.md)). |
| gql | [GQL queries](../gql/index.md) (experimental): `GraphTraversalSource::gql` / `gql_with` run ISO GQL queries (`MATCH ... WHERE ... RETURN ...`, `INSERT`, `SET`, `REMOVE`, Cypher spellings as aliases) as ordinary traversals; with `script` also `g.gql(..)` in the DSL. No dependencies. |
| persist | Graphersal's own lossless binary format: packed snapshots (`.gsnap`), a write-ahead journal with recovery, and the Store (one directory or one `.gstore` file = one graph, durable commits, point in time, fork, rollback, backups, repair). See [Persistence](../persistence/overview.md). |
| persist-zstd | zstd-compressed snapshot chunks on top of `persist` (pure Rust). |
| persist-zip | ZIP backup archives of a Store on top of `persist`. |
| model2vec | The embedder `vector::Model2Vec`: a [Model2Vec](../getting_started/embeddings.md#real-models-model2vec) static embedding model loaded from a local directory (never the network), through `model2vec-rs` and `tokenizers` (pure Rust). Native targets only; the `graphersal` CLI has it on by default. |

## CLI Application and Python

The command-line tool `graphersal` (prebuilt binaries, `cargo binstall`, `cargo install
graphersal-cli`) and the Python package are on [Installation](../getting_started/installation.md).
Every command of the tool is on the [Command Cheat Sheet](command_cheat_sheet.md).

## WEB Playground

Graphersal also runs in the browser: the web playground is a static page with the engine compiled to
WebAssembly, so there is nothing to install on a server. Build it from the repository with
`playground/build.sh` and serve the `playground/` directory, as described in the
[WEB Playground](web_playground.md) chapter.
