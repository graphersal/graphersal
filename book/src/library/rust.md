# Rust Quick Start

Add the crate with the features you need ([Library Features](../users_guide/installation.md) lists
them all):

```toml
[dependencies]
graphersal = { version = "0.4", features = ["script", "io", "display"] }
```

- `script`: the Gremlin DSL as text, the same language as the CLI, the playground and Python;
- `io`: GraphSON and GraphML files;
- `display`: results rendered as tables;
- `persist`: snapshots, the journal and the Store on disk.

The code on this page is the example `crates/graphersal/examples/quick_start.rs`; run it with
`cargo run -p graphersal --example quick_start --features script,io,display`.

## Read

`use graphersal::prelude::*` brings everything a query needs. A graph is shared behind a lock: a
read lock gives a traversal source, the steps are methods, and a terminal (`to_list`, `next`,
`iterate`) runs the traversal.

```rust,ignore
{{#include ../../../crates/graphersal/examples/quick_start.rs:read}}
```

`v(None)` starts at every vertex; `v("1")` or `v(vec!["1", "2"])` at given ids. The Rust names are the
snake_case names of the DSL, with a trailing underscore where Rust needs one (`as_`, `in_`).

## Write

Writes take the write lock and `traversal_mut()`. A traversal is one unit: it commits when it
succeeds and is rolled back completely when any step fails. Anonymous traversals start with `__::`.

```rust,ignore
{{#include ../../../crates/graphersal/examples/quick_start.rs:write}}
```

Several traversals become one unit with `Transactional::transaction`; a dry run shows the changes
a closure would make without keeping them ([Transactions](../users_guide/transactions.md)).

## The DSL from Rust

With the `script` feature a query can also be text, for example one a user typed. `eval_value`
returns the result as a script value, `eval` renders it for display:

```rust,ignore
{{#include ../../../crates/graphersal/examples/quick_start.rs:script}}
```

These two helpers allow everything. For a query you did not write, use
`script::eval_value_with_limits` with an `Authorizer` such as `AccessPolicy::read_only()` and
`ScriptLimits` ([Permissions](../users_guide/permissions.md),
[Resource Limits](../users_guide/resource_limits.md)).

## Files and samples

With the `io` feature:

```rust,ignore
{{#include ../../../crates/graphersal/examples/quick_start.rs:files}}
```

## Where to go from here

- the API reference on docs.rs: `prelude` for queries, the topic modules (`schema`, `changes`,
  `auth`, `catalog`, `exec`, `profile`, `storage`, `persist`) for integration;
- [Transactions](../users_guide/transactions.md): units, dry runs, change capture and commit hooks;
- [Schemas](../users_guide/schema_enforcement.md);
- [Persistence Overview](../persistence/overview.md): a graph on disk;
- [Custom Storages](../for_developers/custom_storage.md): run the engine on your own storage.
