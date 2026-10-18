# graphersal

An in-memory graph database and traversal engine in Rust, queried with Gremlin, the query language
of Apache TinkerPop, with vector search for GraphRAG: store embeddings as `vector` values (`f32`,
or quantized to `f16`, `i8`, `bit`), find the nearest ones with `nearest()` and follow the graph
from them in the same traversal ([Embeddings and Vector Search](https://graphersal.dev/getting_started/embeddings.html)).

**[Playground](https://play.graphersal.dev)** · **[Book](https://graphersal.dev)** ·
**[Step reference](https://graphersal.dev/traversal/step_reference.html)** ·
**[Repository](https://github.com/graphersal/graphersal)**

![The web playground: a recursive traversal over a directory tree, its results and the graph](https://graphersal.dev/images/getting_started/playground-tree.png)

```toml
[dependencies]
graphersal = { version = "0.2", features = ["script", "io", "display"] }
```

```rust
use std::sync::Arc;

use graphersal::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The TinkerPop "modern" sample graph: 4 people, 2 pieces of software, 6 edges.
    let graph = GraphSource::tinkerpop_modern();

    // The Rust API: a read lock gives a traversal source, a terminal runs the traversal.
    let friends = graph
        .read()
        .traversal()
        .v(None)
        .has("name", "marko")
        .out("knows")
        .values("name")
        .to_list()?;
    println!("{friends:?}"); // ["josh", "vadas"]

    // The same query as Gremlin text (feature `script`), in snake_case or camelCase.
    let names = graphersal::script::eval_value(
        Arc::new(graph),
        r#"g.V().has("name", "marko").out("knows").values("name").toList()"#,
    )?;
    println!("{names}"); // ["josh", "vadas"]
    Ok(())
}
```

## What is in it

- A property graph in memory: vertices with a set of labels, edges, properties of logical types
  (`int64`, `float64`, `string`, `uuid`, the date and time types `datetime`, `localdatetime`,
  `date`, `time` and `duration` with IANA time zones, vectors of `f32`/`f16`/`i8`/`bit`, arrays,
  objects), declared property indexes for `has()` filters and graph statistics.
- Exact vector search (`similarity()`, `nearest()`, `vector_mean()`) and quantization of
  embeddings computed outside the engine.
- The Gremlin steps as a Rust API and as text, the Gremlin DSL (on [Rhai](https://rhai.rs)), with
  a rule-based optimizer, bulk traversers and `profile()` / `execute()` showing the optimized plan
  with timings, counts and memory per step.
- Every traversal is atomic: it commits completely or is rolled back. Explicit transactions, dry
  runs and change capture with commit hooks for everything around it.
- Schemas in a subset of JSON Schema 2020-12, enforced on write in `open` or `closed` mode,
  inferred from data, patched, validated and diffed; `default` and `defaultFunction` (`uuidv4`,
  `uuidv7`, `now`, `today`) fill properties when an element is created.
- Running queries you did not write: resource limits, timeouts, cancellation, a memory budget and
  permissions (an `Authorizer`, for example the ready-made `AccessPolicy::read_only()`).
- Saved queries: named, parameterized queries stored in the graph.
- Persistence: lossless snapshots, a write-ahead journal, and the Store (one directory or one file
  = one graph) with point-in-time views, fork, rollback, verified backups and repair; the format is
  a [public specification](https://graphersal.dev/persistence/format.html).
- Your own storage: the whole engine is generic over the `GraphStorage` trait (EXPERIMENTAL in
  0.2.x), with the conformance suite `graphersal-storage-tests`.

## Features

All off by default and strictly additive:

| Feature | What it adds |
|---|---|
| `script` | the Gremlin DSL as text, with safe default limits |
| `io` | GraphSON 3.0 and GraphML import and export |
| `display` | results rendered as tables |
| `serde` | serialization of change sets and schemas, the JSON visualizer |
| `gql` | GQL (ISO/IEC 39075) read queries: `MATCH ... WHERE ... RETURN ...` as a start step, Cypher spellings as aliases |
| `persist` | snapshots, the journal and the Store; `persist-zstd` and `persist-zip` add zstd chunks and ZIP backups |
| `alloc-tracking` | exact per-step memory figures in `profile()` (the binary installs the allocator) |

## Learn more

- [Rust Quick Start](https://graphersal.dev/library/rust.html) and the
  [API documentation](https://docs.rs/graphersal).
- The command line tool, REPL, dev server and MCP endpoint: the
  [`graphersal-cli`](https://crates.io/crates/graphersal-cli) crate (binary `graphersal`).
- Apache TinkerPop's own test suite runs on every test run: more than 97 % of the scenarios in scope
  pass, and no implemented step gives a wrong result
  ([compliance](https://graphersal.dev/for_developers/tinkerpop_compliance.html),
  [deviations](https://graphersal.dev/for_developers/tinkerpop_deviations.html)).

Graphersal is compatible with, and inspired by, Apache TinkerPop Gremlin. It is an independent
project: it is not part of Apache TinkerPop and is not endorsed by the Apache Software Foundation.

Licensed under the Apache License, Version 2.0 (see `LICENSE-APACHE` and `NOTICE`).
