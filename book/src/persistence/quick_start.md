# Quick Start

## Command line

```bash
cargo build -p graphersal-cli                        # target/debug/graphersal
graphersal store create data/ --from modern          # a store with the modern graph, at commit 0
graphersal --graph data/ -e 'g.add_v("person").property("name", "zoe").to_list()'
graphersal store mark data/ before-import            # name this position
graphersal --graph data/ -e 'g.add_v("person").property("name", "ada").to_list()'
graphersal store info data/                          # commit 2, 1 snapshot, 1 mark
graphersal store fork data/ branch/ --at-mark before-import   # the state at the mark, as a new store
graphersal store backup data/ backup/                # a consistent copy, also while in use
graphersal --graph data/ --server                    # the playground UI on the store: http://127.0.0.1:8080/
```

Every commit to `data/` is durable before the command returns; the REPL (`graphersal --graph
data/`) and the dev server keep it open and commit every query. Ctrl+C closes the store
cleanly; a crash or `kill -9` loses nothing committed either.

Without a store, a packed snapshot saves and loads the whole graph:

```bash
graphersal -e 'g.export_snapshot("m.gsnap")'          # Exported a snapshot to m.gsnap (6 vertices, 6 edges, commit 0)
graphersal --graph m.gsnap -e 'g.v().count().next()'  # 6
```

## Rust

```toml
graphersal = { version = "0.4", features = ["persist"] }
```

```rust,no_run
use graphersal::persist::{RecoveryTarget, Store, StoreOptions};

let store = Store::create("data", StoreOptions::new())?;      // later: Store::open("data", ..)
store.graph().write().traversal_mut().add_v("person").property("name", "ann").to_list()?;
store.mark("after-ann")?;                                      // a point-in-time target
store.checkpoint(Some("nightly"))?;                            // a snapshot of the current state
store.close()?;                                                // a clean close (Drop does it too)

let store = Store::open("data", StoreOptions::new())?;        // recovery: snapshot + WAL replay
assert_eq!(store.graph().read().vertex_count(), 1);
let past = Store::open_read_only("data", RecoveryTarget::Mark("after-ann".into()))?;
assert_eq!(past.graph.vertex_count(), 1);
# Ok::<(), Box<dyn std::error::Error>>(())
```

`store.graph()` is the shared graph (`Graph`, a `TraversalGraph` behind a read-write lock); every commit of it
is written to the store's write-ahead log and synced before it returns.

A packed snapshot over any `Write` / `Read`:

```rust
use graphersal::prelude::*;

let graph = TraversalGraph::tinkerpop_modern();
let mut bytes = Vec::new();
graph.write_snapshot(&mut bytes).unwrap();
let loaded = TraversalGraph::read_snapshot(bytes.as_slice()).unwrap();
assert_eq!(loaded.vertex_count(), 6);
```

## Python

```python
import graphersal

graph = graphersal.Graph.tinkerpop_modern()
graph.save("g.gsnap")                                    # {"commit_seq", "vertex_count", "edge_count"}
graph = graphersal.Graph.load("g.gsnap")

with graphersal.Store.create("data") as store:           # later: graphersal.Store.open("data")
    store.graph.execute('g.addV("person").property("name", "ann").next()')
    store.mark("after-ann")
    store.checkpoint("nightly")
```

## Web playground

Every graph you load in the [playground](../users_guide/web_playground.md) is a Store in
memory: the **Store** menu in the top bar checkpoints, marks, opens past states read-only, forks
and rolls back, exactly as the dev server does on a directory
([The Store in the Playground](playground_store.md)). Everything in memory is lost on a page
reload: **Download current state (.gsnap)** keeps a state, and the start page loads it back.
