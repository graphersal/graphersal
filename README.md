# Graphersal

**An in-memory graph database and traversal engine in Rust, queried with GQL, the ISO standard
graph query language, and with Gremlin, with vector search for GraphRAG.**

[![crates.io](https://img.shields.io/crates/v/graphersal.svg)](https://crates.io/crates/graphersal)
[![docs.rs](https://img.shields.io/docsrs/graphersal)](https://docs.rs/graphersal)
[![PyPI](https://img.shields.io/pypi/v/graphersal.svg)](https://pypi.org/project/graphersal/)
[![CI](https://github.com/graphersal/graphersal/actions/workflows/ci.yml/badge.svg)](https://github.com/graphersal/graphersal/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE-APACHE)

**[Playground](https://play.graphersal.dev)** · **[Book](https://graphersal.dev)** ·
**[Step reference](https://graphersal.dev/traversal/step_reference.html)** ·
**[API docs](https://docs.rs/graphersal)** ·
[Mirror on GitHub Pages](https://graphersal.github.io)

## Why Graphersal

AI moves faster than the tools around it. We built Graphersal so you don't have to wait for them: a
lightweight, embeddable graph engine you can turn into whatever your AI solution needs — a library
inside your application, a tool for your agents, or a full server with your own features on top.

It works out of the box: a browser UI, an MCP endpoint for AI agents, an AI chat where you simply
ask about your graph in plain language and it writes and runs the queries for you, durable storage
and transactions, and the safety to run queries you did not write — permissions, read-only access,
resource limits and timeouts. You start with a working system, not with a list of components to
wire together.

## Vector search and GraphRAG

Store your embeddings next to the graph and search them in the same query that walks it. A
`vector` is a value type of its own (`f32`, or quantized to `f16`, `i8` or `bit` to save memory),
declared in the schema, searched exactly with `nearest()` and `similarity()` (cosine, dot product,
L2, Hamming). **GraphRAG** in one traversal: find the passages nearest to the question, then follow
the edges to what the graph knows about them, and hand that context to your language model.

```groovy
// "What should I cook for my cat?" ~ the question's embedding
g.v().has_label("doc").nearest("emb", question, 2)
  .project("title", "author", "topic")
    .by("title")
    .by(__.in("wrote").values("name"))
    .by(__.out("about").values("name"))
```

[Run it in the playground](https://play.graphersal.dev/#/play?graph=empty&q=%2F%2F%20Documents%20with%20toy%20embeddings%20%5Banimals%2C%20food%2C%20technology%5D%2C%20their%20authors%20and%20topics.%0Afor%20d%20in%20%5B%0A%20%20%5B%22Cats%20and%20dogs%22%2C%20%20%20%20%20%20%22Ada%22%2C%20%20%22pets%22%2C%20%20%20%20%20%20%20%20%5B0.9%2C%200.1%2C%200.0%5D%5D%2C%0A%20%20%5B%22Feeding%20your%20cat%22%2C%20%20%20%22Ada%22%2C%20%20%22pets%22%2C%20%20%20%20%20%20%20%20%5B0.6%2C%200.7%2C%200.0%5D%5D%2C%0A%20%20%5B%22Pasta%20at%20home%22%2C%20%20%20%20%20%20%22Ben%22%2C%20%20%22cooking%22%2C%20%20%20%20%20%5B0.0%2C%200.9%2C%200.1%5D%5D%2C%0A%20%20%5B%22Robot%20chefs%22%2C%20%20%20%20%20%20%20%20%22Cleo%22%2C%20%22cooking%22%2C%20%20%20%20%20%5B0.1%2C%200.6%2C%200.8%5D%5D%2C%0A%20%20%5B%22Rust%20for%20beginners%22%2C%20%22Cleo%22%2C%20%22programming%22%2C%20%5B0.0%2C%200.0%2C%201.0%5D%5D%2C%0A%5D%20%7B%0A%20%20g.merge_v(%23%7B%22T.label%22%3A%20%22person%22%2C%20name%3A%20d%5B1%5D%7D).as(%22a%22)%0A%20%20%20.merge_v(%23%7B%22T.label%22%3A%20%22topic%22%2C%20name%3A%20d%5B2%5D%7D).as(%22t%22)%0A%20%20%20.add_v(%22doc%22).property(%22title%22%2C%20d%5B0%5D).property(%22emb%22%2C%20vector(d%5B3%5D)).as(%22d%22)%0A%20%20%20.add_e(%22about%22).to(%22t%22)%0A%20%20%20.add_e(%22wrote%22).from(%22a%22).to(%22d%22)%0A%20%20%20.iterate()%3B%0A%7D%0A%0A%2F%2F%20%22What%20should%20I%20cook%20for%20my%20cat%3F%22%20~%20%5B0.5%2C%200.8%2C%200.0%5D%3A%20the%20two%20best%20hits%20with%20their%20context.%0A%2F%2F%20%5B%23%7B%22author%22%3A%20%22Ada%22%2C%20%22title%22%3A%20%22Feeding%20your%20cat%22%2C%20%22topic%22%3A%20%22pets%22%7D%2C%0A%2F%2F%20%20%23%7B%22author%22%3A%20%22Ben%22%2C%20%22title%22%3A%20%22Pasta%20at%20home%22%2C%20%22topic%22%3A%20%22cooking%22%7D%5D%0Ag.v().has_label(%22doc%22).nearest(%22emb%22%2C%20%5B0.5%2C%200.8%2C%200.0%5D%2C%202)%0A%20.project(%22title%22%2C%20%22author%22%2C%20%22topic%22)%0A%20%20%20.by(%22title%22)%0A%20%20%20.by(__.in(%22wrote%22).values(%22name%22))%0A%20%20%20.by(__.out(%22about%22).values(%22name%22))%0A%20.to_list()&run=1)
with toy embeddings, no model needed. Graphersal can also make the embeddings itself: the
`embed()` step and `embed_property()` turn text into vectors with the built-in `hash-ngram/1`, a
local [Model2Vec](https://github.com/MinishLab/model2vec) model
(`graphersal embed-model download minishlab/potion-base-8M`, then `--embedder`) or any model you
register from Rust or Python; the schema records which model made each property, so a text query
such as `nearest("emb", "what should I cook for my cat?", 2)` uses the right one. Or bring vectors
from the provider of your choice: Graphersal stores, filters and searches them, in the browser, in
your Rust application, from Python and through MCP. The guide:
[Embeddings and Vector Search](https://graphersal.dev/getting_started/embeddings.html).

## A first look

[![The web playground: a recursive traversal over a directory tree, its results and the graph](book/src/images/getting_started/playground-tree.png)](https://play.graphersal.dev/#/play?graph=tree&q=%2F%2F%20every%20path%20below%20the%20src%20directory%0Ag.v().has(%22name%22%2C%20%22src%22)%0A%20%20.repeat(__.out()).emit()%0A%20%20.path().by(%22name%22)&run=1)

You ask Graphersal questions in **plain language**, in **GQL** or in **Gremlin**. In the AI chat you
simply write what you want to know, and the model of your choice turns it into queries, runs them
and answers.

**GQL** is the ISO standard graph query language (ISO/IEC 39075): you draw the pattern you are
looking for, and Graphersal returns one row per match.

```sql
-- who created software together with marko?
MATCH (me:person {name: 'marko'})-[:created]->(s:software)<-[:created]-(other:person)
WHERE other <> me
RETURN other.name AS coworker, s.name AS software
ORDER BY coworker
```

[Run it in the playground](https://play.graphersal.dev/#/play?graph=modern&lang=gql&q=%2F%2F%20who%20created%20software%20together%20with%20marko%3F%0AMATCH%20(me%3Aperson%20%7Bname%3A%20%27marko%27%7D)-%5B%3Acreated%5D-%3E(s%3Asoftware)%3C-%5B%3Acreated%5D-(other%3Aperson)%0AWHERE%20other%20%3C%3E%20me%0ARETURN%20other.name%20AS%20coworker%2C%20s.name%20AS%20software%0AORDER%20BY%20coworker&run=1).
GQL reads and writes (`INSERT`, `SET`, `REMOVE`, `DELETE`), and familiar Cypher spellings such as
`CREATE`, `MERGE`, `UNWIND` or `toUpper()` are accepted as aliases where they mean the same, so a
query you already know usually runs as it is. GQL support is experimental and grows from release to
release: [GQL Queries](https://graphersal.dev/gql/index.html),
[the supported subset](https://graphersal.dev/gql/subset.html).

**Gremlin** is the query language of [Apache TinkerPop&trade;](https://tinkerpop.apache.org): start
somewhere in a graph, walk along its edges, filter, group and count.

```groovy
// who created software together with marko?
g.v().has("name", "marko").as("me")
  .out("created").in("created")
  .where(P.neq("me"))
  .path().by("name")
```

[Run it in the playground](https://play.graphersal.dev/#/play?graph=modern&q=%2F%2F%20who%20created%20software%20together%20with%20marko%3F%0Ag.v().has(%22name%22%2C%20%22marko%22).as(%22me%22)%0A%20%20.out(%22created%22).in(%22created%22)%0A%20%20.where(P.neq(%22me%22))%0A%20%20.path().by(%22name%22)&run=1):
the real engine, compiled to WebAssembly, runs in your browser. Nothing to install, and your data
never leaves your browser. Both languages run on the same graph and the same engine: a GQL query is
compiled into an ordinary traversal plan, with the same optimizer, indexes, `profile()`, limits and
permissions as Gremlin.

## What you get

- **GQL, the ISO standard.** Pattern queries with `MATCH`, `OPTIONAL MATCH`, aggregation,
  subqueries, quantified paths and `UNION`, and writes with `INSERT`, `SET`, `REMOVE`, `DELETE` and
  `MERGE`; Cypher spellings are accepted as aliases. Run a query with `g.gql("MATCH ...")`, in the
  playground's GQL tab, through MCP, or save it as a named query (experimental).
- **Gremlin, two ways.** Build traversals in Rust or write them as text in the Gremlin DSL, in
  snake_case or in Gremlin's own camelCase (`has_label` or `hasLabel`). The same text runs in the
  browser, on the command line, from Python and through an AI agent.
- **A playground in the browser.** Write a query and see a table, the graph drawing, the schema and
  the optimized plan.
- **A command line tool.** One-shot queries for shell scripts, an interactive REPL with completion
  and help, and a local server that brings the playground UI to a graph on your disk.
- **AI agents on your graph.** The server speaks the Model Context Protocol (MCP), so Claude Code or
  another MCP client can query the graph you are looking at, in GQL or in Gremlin. In the playground's AI Chat tab you ask
  about your graph in plain language, and the model of your choice writes and runs the queries
  (experimental).
- **Values with real types.** Besides numbers, strings, UUIDs and nested JSON: five date and time
  types modelled on PostgreSQL (time zones from the IANA database, calendar-correct arithmetic,
  TinkerPop's date steps), and vectors for embeddings (`f32`, `f16`, `i8`, `bit`) with exact
  similarity search (`nearest()`, cosine, dot, L2, Hamming) and quantization. A string that looks
  like a date stays a string: types come from a schema or a typed value, never from guessing.
- **Safe by construction.** Every traversal is atomic: it commits completely or leaves nothing
  behind. Schemas (a subset of JSON Schema) are enforced on write and fill create-time defaults
  (`default`, `uuidv7`, `now`). Limits, timeouts and permissions make it safe to run queries you
  did not write.
- **Durable when you need it.** A graph lives in memory, and the Store keeps it on disk: every
  commit goes to a write-ahead log, and you can go back to any point in time, fork, roll back, back
  up and repair. The file format is a public specification.
- **Fast and observable.** A rule-based optimizer rewrites each query and answers `has()` filters
  from declared property indexes (`g.create_index(..)`); `profile()` shows the optimized plan with
  timings, traverser counts and memory per step.
- **Open for integration.** Rust and Python APIs, saved (named, parameterized) queries stored in the
  graph, change capture with commit hooks, GraphSON 3.0 and GraphML files, and a storage trait to
  run the whole engine on a storage of your own.

## Install

- **Prebuilt binaries** for Linux, macOS and Windows: the archives on the
  [releases page](https://github.com/graphersal/graphersal/releases), or `cargo binstall graphersal-cli`.
- **From source**: `cargo install graphersal-cli` (Rust 1.88 or newer); the binary is `graphersal`.
- **Python**: `pip install graphersal`. **Rust**: `cargo add graphersal --features script,io,display`.

The Linux binaries are statically linked with musl and run on any distribution, but are slower on
allocation-heavy queries than a source build. Checksums, WSL and the macOS/Windows prompts:
[Installation](https://graphersal.dev/getting_started/installation.html).

## Get started

### In the browser

Open **[play.graphersal.dev](https://play.graphersal.dev)**, pick a sample graph and press Run. The
[playground tour](https://graphersal.dev/getting_started/playground_tour.html) shows every panel.

### On the command line

```bash
graphersal -e 'g.v().has("name", "marko").out("knows").values("name")'
graphersal -e 'g.gql("MATCH (p:person)-[:knows]->(f) RETURN f.name AS friend")'   # GQL
graphersal --graph people.json -e 'g.v().has_label("person").count()'    # GraphSON or GraphML
graphersal                                                               # interactive REPL
graphersal --graph people.json --server --mcp      # the playground UI + MCP on http://127.0.0.1:8080
```

The result goes to stdout, a diagnostic to stderr, and a failing query exits with `1`, so a shell
script or a CI job can check a graph. See [The Command Line](https://graphersal.dev/getting_started/cli.html)
and [Dev Server, MCP and AI Chat](https://graphersal.dev/getting_started/server_mcp_ai.html).

### In Docker

The repository's `Dockerfile` builds a small image of the command line; by default it serves a
single-file Store read-only with a read-only MCP endpoint (no authentication: keep it on a
private network). See [Running in Docker](https://graphersal.dev/persistence/docker.html).

```bash
docker build -t graphersal .
docker run --rm -p 127.0.0.1:8080:8080 -v "$PWD/my.gstore:/data/graph.gstore:ro" graphersal
```

### From Rust

```toml
[dependencies]
graphersal = { version = "0.4", features = ["script", "io", "display", "gql"] }
```

```rust
use graphersal::prelude::*;

let graph = GraphSource::tinkerpop_modern();
let friends = graph
    .read()
    .traversal()
    .v(None)
    .has("name", "marko")
    .out("knows")
    .values("name")
    .to_list()?; // ["josh", "vadas"]

// the same query as Gremlin text (feature `script`), and as GQL (features `script` and `gql`)
let graph = std::sync::Arc::new(graph);
let names = graphersal::script::eval_value(
    graph.clone(),
    r#"g.V().has("name", "marko").out("knows").values("name").toList()"#,
)?;
let rows = graphersal::script::eval_value(
    graph,
    r#"g.gql("MATCH (:person {name: 'marko'})-[:knows]->(f) RETURN f.name AS friend").toList()"#,
)?;
```

Every feature is off by default: `script` (the Gremlin DSL), `gql` (GQL queries), `io` (GraphSON
and GraphML), `display` (tables), `serde`, `persist` (snapshots, journal and the Store). See the
[Rust Quick Start](https://graphersal.dev/library/rust.html).

### From Python

```bash
pip install graphersal
```

```python
import graphersal

graph = graphersal.Graph.tinkerpop_modern()
graph.query('g.V().has("name", "marko").out("knows").values("name")')
# ['josh', 'vadas']
graph.query('''g.gql("MATCH (:person {name: 'marko'})-[:knows]->(f) RETURN f.name AS friend")''')
# [{'friend': 'josh'}, {'friend': 'vadas'}]
```

See the [Python Quick Start](https://graphersal.dev/library/python.html).

## How mature is it

Graphersal 0.4 is an early release line, built with the care of a database:

- **TinkerPop conformance.** Apache TinkerPop's own Gherkin test suite (3.8.2) runs on every test
  run. More than 97 % of the scenarios in scope pass, and no implemented step gives a wrong result.
  The rest are steps that are not implemented yet or differ on purpose; each difference is
  documented ([compliance](https://graphersal.dev/for_developers/tinkerpop_compliance.html),
  [deviations](https://graphersal.dev/for_developers/tinkerpop_deviations.html)).
- **GQL conformance.** Which parts of ISO/IEC 39075 are supported is listed feature by feature
  ([GQL conformance](https://graphersal.dev/gql/conformance.html)); how far the Cypher compatibility
  forms agree with Neo4j's behaviour is measured against the openCypher TCK on every test run
  ([TCK compliance](https://graphersal.dev/gql/tck_compliance.html)), and no supported construct
  gives a wrong result there. GQL is experimental: its subset grows, and an answer may still change.
- **Tested hard.** About 90 % line coverage; randomized differential tests and fuzzing of queries,
  file decoders, checkpoints and backups run in every `cargo test`, with long budgets before each
  release. A query that goes wrong ends in an error with a `help()` text, never in a crash.
- **A public storage format.** Snapshots, the write-ahead log and the Store are described byte by
  byte in the [format specification](https://graphersal.dev/persistence/format.html); every
  decoder treats its input as hostile.
- **Clear stability labels.** Within 0.4.x the public API of `graphersal` (outside the items
  marked experimental), the DSL and the schema format change only additively. GQL, the storage
  traits (`GraphStorage`, `PersistentStorage`, `StoreDir`) and the AI Chat are experimental, and the
  dev server's HTTP API is internal.

Graphersal is **not a complete copy of TinkerPop.** Read
[Graphersal and TinkerPop](https://graphersal.dev/concepts/graphersal_and_tinkerpop.html) for what
is the same, what is different and why.

## Repository

| Path | What |
|---|---|
| [`crates/graphersal`](crates/graphersal) | the library: storage, traversal engine, optimizer, Gremlin DSL, GQL, schema, persistence |
| [`crates/graphersal-cli`](crates/graphersal-cli) | the `graphersal` binary: one-shot runner, REPL, dev server with MCP, `graphersal store` |
| [`bindings/py-graphersal`](bindings/py-graphersal) | the Python package `graphersal` |
| [`playground`](playground) | the browser playground (runs `crates/graphersal-wasm`) |
| [`crates/graphersal-storage-tests`](crates/graphersal-storage-tests) | the conformance suite for your own `GraphStorage` |
| [`book`](book) | the documentation at [graphersal.dev](https://graphersal.dev) |

Building from source needs Rust 1.88 or newer:

```bash
cargo build --release -p graphersal-cli      # target/release/graphersal
```

## Contributing

Bug reports, questions and pull requests are welcome. Contributions are accepted under the
[Contributor License Agreement](CLA.md), signed once with a comment on your first pull request; see
[CONTRIBUTING.md](CONTRIBUTING.md).
Please report security issues privately as described in [SECURITY.md](SECURITY.md).

## License

Licensed under the [Apache License, Version 2.0](LICENSE-APACHE); see also [NOTICE](NOTICE). Use of
the project name is described in [TRADEMARKS.md](TRADEMARKS.md).

Graphersal is compatible with, and inspired by, Apache TinkerPop Gremlin. It is an independent
project: it is not part of Apache TinkerPop and is not endorsed by the Apache Software Foundation.
Apache TinkerPop, TinkerPop and Gremlin are trademarks of the Apache Software Foundation.

Graphersal's GQL is measured against the openCypher TCK and accepts Cypher compatibility forms. It
is not Cypher and not openCypher, and it is not affiliated with, endorsed by or sponsored by Neo4j
or the openCypher Implementers Group. Cypher&reg; is a registered trademark of Neo4j Inc.; the names
Cypher and openCypher are used here only to describe compatibility testing.
