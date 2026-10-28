# Storage Conformance Tests

> `GraphStorage` is **experimental in 0.4.x**: its methods and associated types may change in any
> 0.4.x release (a layered storage with snapshot reads is planned). The trait's own rustdoc
> is the implementer's guide: handles, units, capabilities, change capture and statistics.

The crate `graphersal-storage-tests` (on crates.io, **EXPERIMENTAL** like the trait: it may break in any minor
release before 1.0, so pin the exact version) runs one contract against any `GraphStorage` implementation. Add it
as a dev-dependency (`graphersal-storage-tests = "=0.4.0"`) and, in a test of the implementing crate:

```rust,ignore
graphersal_storage_tests::conformance!(my_storage, base = MyStorage::new() => base);
```

The suite uses only `graphersal`'s public API.

Each contract item is a separate test (ids, label sets, edge endpoints, property kinds, JSONPath writes and
removals, cascading drops, counts and `*_by_label` equal to scans after every mutation, schema application,
read-only wrappers, stale handles that never alias another element) plus a smoke layer that loads the
modern graph through the trait and compares eight traversals with `TraversalGraph`.

Schema support is optional: the schema items check enforcement only when `set_schema` does not return
`GraphError::Unsupported` (the trait default); otherwise they pass without checking anything. Schema enforcement is
not reusable outside `graphersal` yet, so a storage in another crate keeps that default and still passes
`conformance!`. `conformance_core!` leaves the schema items out entirely, for a storage that accepts schemas but
deliberately does not enforce them.

Some items depend on what the storage claims in `capabilities()` (see [Transactions](../users_guide/transactions.md#storages)):
`capabilities_are_consistent` always runs (`change_capture` requires `atomic`); the rollback, savepoint and
`unit_changes` items run for an `atomic` storage; the commit-hook and `apply_change_set` items run for a storage
with `change_capture` (and check that a storage without it refuses hook registration). Where the trait documentation is silent the reference implementation decides;
those points are listed in the crate README.

The suite is run against `TraversalGraph`, `&mut TraversalGraph`, `Box<TraversalGraph>`, `Arc<TraversalGraph>`,
(read-only items) `&TraversalGraph` and the blanket `&G` view of that wrapper, the crate's `Forwarding` test storage
(own handle types, every method forwarded to a `TraversalGraph`) and a wrapper in the suite's own tests that keeps the
trait's schema defaults. It found two contract violations in `TraversalGraph` (a panic in `add_edge` for a dropped endpoint, stale edge-id
entries after `drop_vertex`); both are fixed and the suite runs with no ignored test.

## Running the TinkerPop suite against another storage

The harness (`crates/graphersal/tests/tinkerpop/harness/`) holds every dataset graph as
`Arc<dyn AnyGraph>`, so the suite is storage-independent above the storage. Inside this repository
`GRAPHERSAL_TEST_STORAGE=forwarding` runs it on the `Forwarding` storage of this crate (see
[TinkerPop Compliance](tinkerpop_compliance.md)). A storage in another crate cannot run it yet: the
harness lives inside the library's integration tests and would have to move into a library-like
crate that does not depend on test-only paths. Until then such a storage follows the
[`Forwarding` pattern](custom_storage.md#the-forwarding-pattern): the same scripts on a
`TraversalGraph` and on the storage, results compared.

How a host plugs its storage into the DSL, the session and the Store: [Custom Storages](custom_storage.md).
