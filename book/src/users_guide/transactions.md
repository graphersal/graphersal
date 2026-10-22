# Transactions

Every traversal is **atomic**: it runs as one unit of work (implicit auto-commit). When it succeeds,
all its changes stay. When it fails, every change it made is rolled back before the error is
returned, so a failing traversal leaves nothing behind.

```groovy
g.V().property("checked", true).values("name").asNumber(GType.LONG).toList()
// fails in asNumber("marko") -> no vertex has "checked" afterwards

g.inject(1).sideEffect(__.V("1").drop()).constant("x").asNumber(GType.LONG).next()
// fails -> vertex 1 and its three edges are still there
```

Any error counts: a failing step, a schema violation, a resource limit
(`traversal.max_traversers`, see [Resource Limits](resource_limits.md)), an expired
`evaluationTimeout` (see [Query Limits](query_limits.md)), and an error while the terminal
materializes the results (for example returning a vertex the same traversal dropped, see
[Dropping Elements](dropping_elements.md)).

## What one unit is

| Entry point | Unit |
|---|---|
| Rust API and Rhai: `to_list()`, `next()`, `iterate()`, `to_table()` and the other visualizers, `profile()`, `execute()`, `to_graph()` | one traversal, including the materialization of its results |
| Rhai (CLI, Python, `eval_*` entries): every executed traversal of a script | one traversal; statements before a failing one stay applied |
| Rhai through `eval_value_atomic` (host option), the playground, Python `execute(.., atomic=True)` | the whole script ([below](#a-whole-script-as-one-unit-rhai)) |
| Rhai through `eval_value_dry_run` (host option), Python `Graph.dry_run` | the whole script, always rolled back ([below](#dry-run-of-a-script-rhai)) |
| Rust API: `Transactional::transaction(\|g\| ..)` (every storage) | the closure; every traversal inside is a savepoint ([below](#explicit-transactions-rust)) |
| Rust API: `Transactional::dry_run(\|g\| ..)` (storages with change capture) | the closure, always rolled back ([below](#dry-run)) |
| `GraphStorage` methods called directly (`add_vertex`, `insert_vertex`, `set_vertex_property`, ...) | one call: it either succeeds or changes nothing; outside a unit it is not recorded, so no hook sees it (use it for loading, see [below](#loading-versus-importing)) |
| `set_schema()` / `patch_schema()` outside a unit | one unit of its own |
| a catalog change (`set_definition()`, `remove_definition()`, `define_query()`, `move_query()`, `describe_query()`) outside a unit | one unit of its own |
| `import_graphml()` / `import_graphson()` into an existing graph | the whole import |
| `apply_change_set()` | the whole change set |

In a script, the unit is the traversal, not the whole script:

```rhai,graph=modern
g.V("1").property("checked", true).toList();                               // applied
g.V("2").property("checked", true).values("name").asNumber(GType.LONG).toList(); // fails, rolled back
// vertex 1 has "checked", vertex 2 has not
```

A host can make the whole script one unit instead (see below); the script itself cannot.

A [GQL](../gql/writes.md) query is one traversal, so one unit too: when its last row fails, the
elements its earlier rows created are rolled back with it.

```rhai,graph=modern,script
let run = g.gql("FOR x IN [1, 0] INSERT (:n {v: 1 / x})").execute();   // fails on the second row
g.v().has_label("n").count().next()   // 0: the node of the first row is gone too
```

`execute()` never throws: on failure its `error` and the partial `profile` are kept, its `results`
are empty, and none of the traversal's changes are applied.

## Explicit transactions (Rust)

`graph.transaction(|g| -> Result<T, E> { .. }) -> Result<T, E>` runs a closure as one unit. There
is no transaction object to finish or forget, and nothing is rolled back in a `Drop`: the closure's
result decides.

The API is the extension trait `Transactional` (`transaction`, `transaction_with`, `dry_run`; in
`graphersal::prelude` and `graphersal::storage`). It has a blanket implementation for every
`GraphStorage`: `TraversalGraph`, its `&mut`/`Box`/solely owned `Arc` wrappers and a storage of your
own, built only on the trait's unit methods (`begin_unit`, `set_unit_metadata`, `commit_unit`,
`rollback_unit`, `discard_unit`). What `Err` undoes is the storage's business: an atomic storage
(`capabilities().atomic`, like `TraversalGraph`) rolls everything back, a non-atomic one keeps
what was changed before the error.

```rust
use graphersal::prelude::*;

let mut graph = TraversalGraph::tinkerpop_modern();
let result: TraverserResult<()> = graph.transaction(|g| {
    g.traversal_mut().add_v("person").property("name", "ann").to_list()?;
    // A failing traversal is a savepoint: only its own changes are rolled back, and the
    // closure gets the error and decides (here: go on).
    let failed = g.traversal_mut().v("1").property("age", 30).constant("x")
        .as_number(GType::LONG).to_list();
    assert!(failed.is_err());
    g.traversal_mut().v("2").property("age", 28).to_list()?;
    Ok(())
});
assert!(result.is_ok());
```

- **Savepoints.** Every traversal inside the closure is a nested unit. When it fails, only its own
  changes are rolled back and the error is returned to the closure, which gives up with `?` or goes
  on (TinkerPop / SQL behaviour).
- **`Err` from the closure** rolls back everything the transaction changed. The commit hooks get
  `after_rollback` (`RollbackReason::Failed`) when it had changed something.
- **`Ok` commits once:** one commit sequence number and one `ChangeSet` for the whole transaction.
- **A veto.** When a hook's `before_commit` refuses the change set, everything is rolled back and
  the call returns `E::from(GraphError::CommitRejected { hook, source })`; `source` is the hook's
  own error. This is why the closure's error type needs `E: From<GraphError>`. The usual types all
  qualify: `TraverserResult` / `BoxedTraverserError` (what `?` on a terminal gives),
  `TraverserError`, `GraphError`, `Box<dyn Error>`, or an application error with a `From` impl.
- **Nesting.** A `transaction` inside another one is a savepoint of the enclosing transaction: its
  `Err` rolls back only its own changes, its `Ok` keeps them in the enclosing unit, which alone
  commits and can still roll them back.
- **Commit metadata.** `transaction_with(CommitMetadata::new().with_principal("migrator"), |g| ..)`
  gives the commit's `ChangeSet` a principal and free attributes. A nested `transaction_with` that
  succeeds adds its metadata to the enclosing one's (a principal only when none is set yet,
  attributes only for keys not set yet); a nested one that fails adds nothing. Plain traversals
  commit with empty metadata.
- **No isolation needed.** `&mut` excludes every other reader and writer for the duration; a
  shared `Graph` is used through its write lock (`graph.write().transaction(..)`).
- A rolled-back transaction leaves gaps in the automatic ids (they are never reused).

`Execution::mutated()` (Rhai: `r.mutated`) of a traversal inside a transaction says whether it
changed something that is now part of the enclosing unit; whether that becomes final is decided by
the transaction. Outside a transaction it says whether the run committed a change.

## Dry run

`graph.dry_run(|g| ..)` runs the closure like a transaction and then **always** rolls it back. It returns the closure's result together with the `ChangeSet` of what the closure changed
(and what was undone), exactly as a commit hook would have received it; `compact()` gives the net
effect. A preview of a migration:

```rust
use graphersal::prelude::*;

let mut graph = TraversalGraph::tinkerpop_modern();
let (_, changes) = graph
    .dry_run(|g| g.traversal_mut().v(None).has_label("software").drop().to_list())
    .unwrap();
assert_eq!(changes.len(), 6); // 2 vertices and their 4 edges
assert_eq!(graph.vertex_count(), 6); // untouched
```

- The graph is left exactly as it was, schema included. Only the automatic id sequences keep
  their progress.
- No commit hook is called: nothing is committed, attempted or vetoed. The change set is built
  whether or not hooks are registered; its `commit_seq()` and `committed_at()` are `0` (never
  committed), its metadata is empty, and `last_commit_seq()` / `last_committed_at()` do not move.
  Its `node_sequence()` / `edge_sequence()` are the id sequences after the dry run.
- A failing traversal inside the dry run is a savepoint as in a transaction and is not in the
  change set. `Err` from the closure comes back unchanged (no change set).
- Inside a transaction a dry run is a savepoint that is always rolled back; its change set holds
  only its own changes.
- A dry run needs a storage with change capture (`capabilities().change_capture`, like
  `TraversalGraph`): it ends with `GraphStorage::discard_unit` ("roll back without hooks, tell me
  what it changed"). On any other storage the unit is closed with an ordinary rollback and the call
  returns `E::from(GraphError::Unsupported { .. })`; that is why `dry_run` also needs
  `E: From<GraphError>`.

## A whole script as one unit (Rhai)

The default unit of a script is one traversal. A host that wants all-or-nothing for the whole
script (a migration script, an import, a playground run) calls
`graphersal::script::eval_value_atomic(graph, script, params, &limits, authorizer)` instead of
`eval_value_with_limits`. The script cannot choose this itself (there is no `g.tx()`).

```rhai,graph=modern
g.addV("person").property("name", "ann").next();          // part of the unit
g.V("1").property("age", 99).next();                      // part of the unit
g.V().values("name").asNumber(GType.LONG).toList();       // fails: the whole script is rolled back
```

- Every traversal of the script is a savepoint. A failure the script handles (`execute()` never
  throws; `try`/`catch`) rolls back only that statement, and the script goes on.
- An uncaught error (or a resource limit) rolls back everything the script changed and is
  returned.
- A script that ends normally commits once (one `ChangeSet`); a commit hook's veto rolls it back
  and is returned as the script's error.
- The graph's write lock is held for the whole script: no other reader or writer of the same
  graph runs between its statements.
- A traversal the script returns without a terminal is not part of the unit: it runs when the
  host renders it, after the commit, as a unit of its own.
- The script must not return a closure or function pointer (also inside an array or a map): a
  closure that captured `g` would point at the empty private graph once the script ends. It fails
  with `ScriptError::ReturnedClosure` and everything is rolled back; return data or a traversal.
- The authorizer is asked per traversal as everywhere else ([Permissions](permissions.md)).

Front ends: the **playground** runs every script as one unit (it also counts the implicit
execution of a trailing traversal, and commits only when the run reports no error, so a run that
shows an error leaves nothing behind). The **CLI** keeps the default (each traversal is a unit),
so a scripted `-e` session behaves like TinkerPop's Gremlin console. The **Python binding** keeps
the default too and offers the whole-script unit per call: `graph.execute(script, atomic=True)`
(a returned closure raises `ReturnedClosureError`; a traversal returned without a terminal is run
like `to_list()` after the commit).

## Dry run of a script (Rhai)

`graphersal::script::eval_value_dry_run(graph, script, params, &limits, authorizer)` is the
script-side counterpart of `Transactional::dry_run`: it runs the script like `eval_value_atomic`
and then **always** rolls everything back, returning `(value, ChangeSet)`.

```rust
use std::sync::Arc;
use graphersal::prelude::*;
use graphersal::auth::AllowAll;
use graphersal::script::{ScriptLimits, eval_value_dry_run};

let graph = Arc::new(GraphSource::tinkerpop_modern());
let (count, changes) = eval_value_dry_run(
    graph.clone(),
    r#"g.V().hasLabel("software").drop().toList(); g.V().count().next()"#,
    Default::default(),
    &ScriptLimits::default(),
    Arc::new(AllowAll),
)
.unwrap();
assert_eq!(count.as_int().unwrap(), 4);
assert_eq!(changes.len(), 6); // 2 vertices and their 4 edges
assert_eq!(graph.read().vertex_count(), 6); // untouched
```

- The rules of [Dry run](#dry-run) apply: the graph is left as it was, no commit hook is called,
  `commit_seq()` is `0`, a failing traversal the script handles is a savepoint and is not in the
  change set, and a failing script returns its error and no change set.
- As in a whole-script unit, the write lock is held for the whole script, and a returned closure
  fails with `ScriptError::ReturnedClosure`.
- A traversal the script returns without a terminal (also inside an array or a map) runs
  **inside** the dry run, as if it ended in `toList()`, so its result reflects the previewed
  changes (`graphersal::script::materialize_traversals`).

The Python binding exposes it as `graph.dry_run(script, params=None, *, policy=None)`, which
returns a `DryRunResult` with `.result` and `.changes` (the change set's serde form as plain
Python data).

## What a rollback restores

Everything the traversal changed: vertices and edges (a dropped vertex comes back with all its
edges), labels, properties (at their old position in the property order), ids and the lookups by id
(`g.V(id)`, `g.E(id)`), the label index and the label counts, the stored schema and the catalog of definitions (saved
queries).

Not restored:

- **Iteration order.** A restored element can come back in another place: `g.V()` / `g.E()` order,
  the order of a neighbour's `out()`/`in()`, the order within a label. Iteration order is
  unspecified, as in TinkerGraph.
- **Element handles** (Rust API). A handle taken inside the failed traversal, or a handle to an
  element the traversal dropped, stays *stale*: a restored element is a new element with the same
  id. Look elements up again by id.

## Cost

The reference storage `TraversalGraph` keeps an internal undo log while a unit is open. A read
costs nothing. A mutation costs one log entry holding the old value (a property write, a label
change, a schema change) or the removed element (a drop); on success the log is cleared.

A rollback does **not** give back automatically assigned ids: an id handed out to an element that
was rolled back is never handed out again, exactly like a database sequence. Auto ids can therefore
have gaps; they are unique, not dense.

The counters never wrap around. The commit sequence number and both automatic id sequences are
`u64`; an increment past `u64::MAX` fails with `GraphError::SequenceOverflow` instead (a reused
commit number or id would corrupt a journal or a replica). For the commit sequence number the
failing unit is rolled back like any failed unit (the hooks get `after_rollback`); for an id
sequence the step that needed the id fails, and with it the query. In practice only a renumbered
graph (`with_last_commit_seq(u64::MAX)`) or an explicit numeric id at the end of the range
(`property(T.id, "18446744073709551615")`, which raises the vertex id sequence to it) gets there;
explicit ids keep working.

## Change capture: `ChangeSet`

What a committed unit changed is described as a `ChangeSet` (Rust API,
`graphersal::changes`): the basis for persistence, replication, change data capture and
audit on top of the library. It is handed to the **commit hooks** (next section).

A `ChangeSet` carries:

- `commit_seq()`: the commit sequence number, monotonic per graph. Only a unit that changed
  something advances it (`last_commit_seq()`); a read-only, rolled-back or vetoed unit does not.
- `committed_at()`: the commit time, `i64` microseconds since the Unix epoch (UTC), taken from the
  system clock at the outermost commit (`web_time`, so also on wasm32). It is **monotonic per
  graph**: `max(now, time of the previous commit)`, so a clock that steps back repeats the previous
  time instead of going back. Every commit of a unit that changed something takes a time, also
  without hooks (`last_committed_at()`); point-in-time recovery compares against it.
- `node_sequence()` / `edge_sequence()`: the automatic id sequences of vertices and edges **after**
  the unit (the last auto id handed out, or the highest numeric explicit id seen). They are on every
  change set because an id of a rolled-back unit or of a dropped element leaves no other trace in a
  journal: `replay` and `apply_change_set` raise the graph's sequences to them, so no automatic id
  is ever handed out twice, also not after recovery or on a replica.
- `metadata()`: an optional principal and free attributes, passed through, never interpreted.
- `mutations()`: the changes **in execution order**, each with its before and after values:

| `Mutation` | Fields |
|---|---|
| `AddVertex` | `id`, `labels`, `properties` |
| `DropVertex` | `id`, `before` (labels and properties) |
| `AddEdge` | `id`, `label`, `out_id`, `in_id`, `properties` |
| `DropEdge` | `id`, `before` (label, endpoints and properties) |
| `SetProperty` | `element` (kind and id), `key`, `before` (absent: `None`), `after` |
| `RemoveProperty` | `element`, `key`, `before` |
| `AddLabel` / `RemoveLabel` | `id`, `label` (a vertex label) |
| `SetEdgeLabel` | `id`, `before` (unlabeled: `None`), `after` |
| `SetSchema` | `before`, `after` |
| `SetDefinition` | `kind`, `name`, `before` (absent: `None`), `after` (removed: `None`): one mutation for every kind of catalog definition (define, replace, move, describe, remove) |

Rules a consumer can rely on:

- **Cascades are explicit.** Dropping a vertex lists one `DropEdge` per removed edge *before* the
  `DropVertex`; replaying the list in order needs no knowledge of the cascade.
- **jpath writes** (`property(jpath("a.b[1]"), v)`) appear as `SetProperty` of the top-level key
  with the whole new value.
- **Schema changes** are in the same stream, in order with the data; so are **catalog changes**
  (`SetDefinition`).
- A write of an equal value is still a `SetProperty` (`before == after`).
- `compact()` returns the net effect: one entry per element, key and label (an element added and
  dropped again disappears, an added element absorbs its later changes, a property set back to its
  old value disappears). Schema changes stay barriers, and the result is ordered so that it
  replays. The changes of one catalog definition merge into one (dropped when it ends where it
  started), listed at the end.

### Building a change set

The graph builds the change sets of its own commits. Outside it (a storage of your own that
captures changes, a replication feed, a test) a change set is built with
`ChangeSet::builder(commit_seq, committed_at)`, optionally `.with_sequences(node, edge)` and
`.with_metadata(..)`, then `push(..)` per mutation and `build()`. Every mutation kind has a
constructor: `Mutation::add_vertex`, `drop_vertex` (with `VertexState::new`), `add_edge`,
`drop_edge` (with `EdgeState::new`), `set_property`, `remove_property`, `add_label`,
`remove_label`, `set_edge_label`, `set_schema` and `set_definition`. `build()` makes the cheap
checks and fails with `GraphError::InvalidChangeSet`: a `commit_seq` of `0` (a never-committed
change set, like a dry run's) has no commit time, a commit time is not negative, and a definition
change has a before or an after image of its own kind and name. A built change set applies,
replays and serializes exactly like a captured one.

```rust
use graphersal::changes::{ChangeSet, ElementRef, Mutation};
use graphersal::prelude::*;

let mut builder = ChangeSet::builder(1, 1_700_000_000_000_000);
builder
    .push(Mutation::add_vertex("ann", ["person"], Default::default()))
    .push(Mutation::set_property(ElementRef::vertex("ann"), "age", None, 29i64.into()));
let changes = builder.build().unwrap();
let replica = TraversalGraph::new().replay(&changes).unwrap();
assert_eq!(replica.last_commit_seq(), 1);
```

### Serialization

With the `serde` feature a `ChangeSet` serializes as JSON (`{"commit_seq": 1, "committed_at":
1791369725123456, "node_sequence": 7, "edge_sequence": 12, "mutations": [{"op": "set_property",
...}]}`; a missing `committed_at` or sequence reads as `0`, which `replay` ignores), and a serialized change set replays into an **identical** graph
without any schema. JSON-native values stay plain JSON; a value of a logical type JSON cannot hold
carries an explicit per-value type tag, at any depth (in element images, property writes and
commit attributes): `uuid` and the date and time types, in TinkerPop GraphSON style with the
canonical text:

```json
{"@type": "g:UUID", "@value": "6f1d3c2a-0000-4000-8000-000000000001"}
{"@type": "gx:OffsetDateTime", "@value": "2024-05-01T09:30:00+02:00"}
```

| Type | Tag |
|---|---|
| `datetime`, `date`, `localdatetime`, `time` | `gx:OffsetDateTime`, `gx:LocalDate`, `gx:LocalDateTime`, `gx:LocalTime` |
| `duration` of microseconds only (no months, no days) | `gx:Duration` (`PT26H`) |
| `duration` of months and days only | `gx:Period` (`P1Y2M3D`) |
| any other `duration` | `graphersal:Interval` (`P1MT2H`) |
| `vector` (book page [Vectors](vectors.md)) | `graphersal:Vector` with an object: `{"element": "f32", "data": [0.5, -1.0]}` (`f16` the same), `{"element": "i8", "scale": 0.007874016, "codes": [64, -127]}`, `{"element": "bit", "dimension": 3, "bits": "a0"}` (the packed bytes in hex); exact for every element kind |

A user map that happens to have an `"@type"` key of its own is escaped, so it can never be misread
as a tag: `{"@type": "graphersal:Object", "@value": {"@type": "mine", "x": 1}}` (the payload's keys
are literal). Reading is strict: an object with an `"@type"` key must be exactly one of these
forms (only `@type` and `@value`, a canonical lowercase uuid, the text of a value of the tag's
type, with a `gx:Duration` holding no months or days and a `gx:Period` no time part, or an object
payload); anything else is an error, never a guess. The tags exist for the `ChangeSet` only; GraphML and the other exports
are unchanged.

## Commit hooks

A host registers a `CommitHook` on the graph (`GraphStorage::register_commit_hook`); while at least
one is registered, every committed unit builds its `ChangeSet` (without hooks nothing is captured
and nothing is paid):

| Event | When | Can veto? |
|---|---|---|
| `before_commit(&ChangeSet) -> Result<(), HookError>` | the unit validated, before it is final | yes: `Err` rolls the unit back |
| `after_commit(&ChangeSet)` | after the unit is final | no |
| `after_rollback(&RollbackReason)` | after a unit that changed something was rolled back (vetoed or failed) | no |

```rust,ignore
use graphersal::prelude::*;
use graphersal::changes::ChangeSet;
use graphersal::changes::{CommitHook, HookError, RollbackReason};

/// A write-ahead log: one JSON line per committed unit (needs the `serde` feature).
struct Wal<W: std::io::Write + Send + Sync> {
    out: W, // a `std::fs::File` in a server (call `sync_data()` after the write)
}

impl<W: std::io::Write + Send + Sync> CommitHook for Wal<W> {
    fn name(&self) -> &str {
        "wal"
    }

    fn before_commit(&mut self, changes: &ChangeSet) -> Result<(), HookError> {
        // Persist BEFORE the commit is final: an Err rolls the unit back and the caller
        // gets GraphError::CommitRejected { hook: "wal", source }.
        serde_json::to_writer(&mut self.out, changes)?;
        self.out.write_all(b"\n")?;
        self.out.flush()?;
        Ok(())
    }

    fn after_rollback(&mut self, reason: &RollbackReason) {
        let _ = reason; // metrics, audit of failed attempts
    }
}

let mut graph = TraversalGraph::tinkerpop_modern();
graph.register_commit_hook(Box::new(Wal { out: Vec::new() })).unwrap();
graph.traversal_mut().add_v("person").property("name", "ann").to_list().unwrap();
assert_eq!(graph.last_commit_seq(), 1); // one committed unit, one WAL line
```

- Hooks run in registration order. The first `before_commit` that returns `Err` stops the commit:
  the later `before_commit` hooks are not called, the unit is rolled back, **every** hook gets
  `after_rollback` (`RollbackReason::Vetoed`), and the caller gets
  `GraphError::CommitRejected { hook, source }` (inside a `TraverserError` for a traversal) whose
  `source` is the hook's own error. The error's help says to fix the cause and run the query
  again; a veto for which that is wrong (a state that refuses every write) returns a
  `changes::HookRefusal::new(message, help)`, whose help replaces it (a Store's refusals do the
  same with their own help: a backup, maintenance mode, damage, a read-only view).
- This makes `before_commit` a write-ahead log: when persisting fails the in-memory change is
  undone, so memory and journal never diverge.
- Only the outermost unit emits events; a unit that changed nothing emits none and takes no
  sequence number, unless it raised an automatic id sequence (a *sequence-only commit*, see
  [Applying and replaying change sets](#applying-and-replaying-change-sets)). Hooks run synchronously, strictly in commit sequence order, and get the change
  set only, never the graph.
- Hooks cannot be removed (register them before publishing the graph).
- A hook that **panics** in `before_commit` is treated like a veto that does not come back as an
  error: the unit is rolled back, every hook gets `after_rollback` (`RollbackReason::Failed`; a
  second panic there is dropped), the commit sequence number does not move, and the panic then
  continues to the caller (see [Panics](#panics)). A Store's journal runs after every user hook,
  so it never writes a unit whose user hook panicked. A panic inside the journal itself cuts its
  record off again, poisons it and rolls the unit back as well
  ([Failures while committing](../persistence/journal_and_recovery.md#failures-while-committing)). A panic in `after_commit` comes after the
  unit is final (and, with a journal, durable): the unit stays committed and the panic continues.

`execute()` reports whether a run changed the graph: `Execution::mutated()` (Rhai: `r.mutated`;
inside a transaction or a whole-script unit see [above](#explicit-transactions-rust)).

## Applying and replaying change sets

- `apply_change_set(&changes)` (any `GraphStorage`) applies a change set to a **live** graph as one
  unit: ids preserved (an explicit numeric id raises the automatic id sequence), schema enforced,
  all-or-nothing, captured and seen by the hooks like any other commit. A replica or a migration
  uses it. Inside the unit it raises the graph's automatic id sequences to the change set's own
  (`GraphStorage::raise_id_sequences`, never lowered), like `replay` does: an automatic id the
  source handed out to an element whose add and drop were compacted away, or in a unit it rolled
  back, is never handed out again in the target. The raise is not undone by a rollback (ids are
  never reused, gaps are fine). When the change set changes nothing in the target (every mutation
  was compacted away) but raises a sequence, the unit is still committed as a **sequence-only
  commit**: it takes a commit sequence number, the hooks get a change set without mutations whose
  `node_sequence`/`edge_sequence` carry the raised values (a veto rolls it back; the raise stays in
  memory), and a journal or a Store writes it, so the raise survives a reopen, a checkpoint, a
  backup or a fork. `Execution::mutated()` stays `false` for it (no data changed). A change set that
  raises nothing (the target's sequences are already as high) commits nothing.
- `TraversalGraph::replay(self, &changes) -> Result<TraversalGraph, _>` rebuilds a graph nobody
  sees yet (recovery from a snapshot plus the write-ahead log). It runs **outside** units: no undo
  log, no capture, no hook fires (a server must not re-write the journal it is reading). The
  graph's commit sequence number, commit time and both automatic id sequences are raised to the
  change set's own (never lowered), so the next live commit continues the journal's numbering and
  clock, and an automatic id of an element that was dropped or rolled back before the journal ends
  is never handed out again. On error the half-built graph is simply dropped.

## Loading versus importing

Two ways to bring data in, with different rules:

| | Loading (a graph nobody sees yet) | Importing (a live graph) |
|---|---|---|
| Examples | start-up from disk: `GraphSource::from_graphml`, `from_graphml_reader`; recovery: `TraversalGraph::replay`; building a graph with direct `GraphStorage` calls before it is shared | `import_graphml` into an existing graph, `apply_change_set`, any mutating traversal |
| Unit | none: no undo log, no `ChangeSet`, no hooks | one unit: all-or-nothing |
| On error | drop the half-built graph | the import is rolled back |
| Commit hooks | never called (replaying a journal must not re-write it) | called like for any commit, may veto |

Isolation for loading comes from "build, then publish": nobody else can see the graph yet. A
server therefore registers its hooks *after* loading and *before* it publishes the graph. Only an
import into a live graph pays for change capture (a `ChangeSet` about the size of the imported
data, built only while hooks are registered).

## Snapshot with position

`last_commit_seq()` read together with a full export under the same read lock is a consistent
snapshot with its position:

```rust
use graphersal::prelude::*;
use graphersal::changes::ChangeSet;

# fn wal_after(_position: u64) -> Vec<ChangeSet> { Vec::new() }
# fn main() -> Result<(), Box<dyn std::error::Error>> {
let graph: Graph = GraphSource::tinkerpop_modern(); // the live, shared graph
let (graphml, position) = {
    let g = graph.read();
    let mut buf = Vec::new();
    g.export_graphml_writer(&mut buf)?;
    (buf, g.last_commit_seq())
};
// keep the snapshot, truncate the write-ahead log up to `position`

// recovery:
let loaded = GraphSource::from_graphml_reader(std::io::Cursor::new(graphml))?;
let mut recovered = std::mem::take(&mut *loaded.write()).with_last_commit_seq(position);
for changes in wal_after(position) {
    recovered = recovered.replay(&changes)?;
}
// register the commit hooks, then publish `recovered`
# Ok(())
# }
```

GraphML carries no schema. A graph that stores one keeps its schema JSON next to the snapshot
(`serde_json::to_string(&schema)` under the same read lock); on recovery, set that schema on the
empty target before importing the GraphML, so the declared types (`uuid`, `array`, `object`) come
back (see [UUID Values](uuid_values.md#graphml-import-and-export)).

## Storages

The unit boundaries are three methods of `GraphStorage`: `begin_unit()`, `commit_unit(mark)` and
`rollback_unit(mark, reason)`. Units nest; an inner unit is a savepoint. Their default
implementation does nothing: a custom storage that does not implement them is **not atomic**, and
a failing traversal keeps the changes it made before the error. `TraversalGraph` (and `&mut`,
`Box` and a solely owned `Arc` of it) implements them with the undo log. An atomic storage also
answers `unit_changes(mark)` exactly (`UnitChanges { data, schema, definitions }`; it drives
`Execution::mutated()` and `Execution::changes()` inside an outer unit)
and counts its commits in `last_commit_seq()`. Outside a unit a mutation records nothing: no hook
is called and `last_commit_seq()` does not move.

Three more methods carry the explicit units of `Transactional`, each with a default:
`set_unit_metadata(metadata)` hands `transaction_with`'s metadata to the unit before its commit
(default: ignored); `discard_unit(mark)` undoes a unit without hooks and returns its `ChangeSet`
(the end of `dry_run`; it needs `change_capture`, the default rolls back and returns
`GraphError::Unsupported`); `mark(name)` has the storage's journal record a named mark (Rhai
`g.mark`; default `GraphError::MarkUnavailable`). The rustdoc of `GraphStorage` is the guide for
writing a storage; [Storage Conformance](../for_developers/storage_conformance.md) is its test
suite.

A storage that captures changes builds its change sets with the constructors above (there is no
reusable undo-log component) and embeds a `CommitHooks` (`graphersal::storage`): it registers the
user hooks, holds the journal slot (`JournalSlot`, handed out by the library's journal and Store)
and runs the events in the required order: `before_commit` of the user hooks in registration
order, then the journal's. The storage assigns the commit sequence number first and does nothing
fallible after an `Ok` (the journal has written its record); on a veto it rolls back and calls
`after_rollback(&RollbackReason::from_error(&err))`. `GraphStorage::mark` forwards to
`CommitHooks::mark`. With feature `persist`, `PersistentStorage` (EXPERIMENTAL) adds what a
snapshot and a Store need: the lineage id, the commit position (`raise_position`, never lowered),
a load sink (`load_vertex`, `load_edge`, `load_schema`, `load_definition`, `finish_load`; outside
units, no hooks, no schema scan), `clear` (keeps the hooks and the journal), `attach_journal` and
`replay(&changes, verify)` (a default over the write methods; `TraversalGraph` overrides it).

What a storage guarantees is `GraphStorage::capabilities()`, a `StorageCapabilities` value (facts,
not permissions): `atomic`, `change_capture`, `isolation`, `concurrent_writers`, `durability`,
`writable`, `immutable`. The default claims nothing. `TraversalGraph` reports `atomic`,
`change_capture` and `writable`; a read-only `&TraversalGraph` and a shared `Arc` claim nothing.

**`change_capture` requires `atomic`.** A storage that cannot roll back cannot honour a
`before_commit` veto, so it reports `change_capture: false` and refuses hook registration
(`GraphError::Unsupported`); there is no half-atomic notification. The conformance suite
(`graphersal-storage-tests`) runs its rollback and hook items only for a storage that claims them.

## Panics

The engine maps every failure to an error, so a panic inside it is a bug. Still, a host that
catches a panic (a server thread, a test harness) must not keep serving a half-applied unit: every
unit rolls itself back when a panic unwinds through it, innermost first, and the panic then
continues to the caller unchanged. This covers a traversal run and its terminal, `execute()`, an
explicit `transaction(|g| ..)` (also a panic in the caller's own closure; the commit hooks get
`after_rollback`), a `dry_run(|g| ..)`, a whole-script unit (`eval_value_atomic`,
`eval_value_dry_run`), `apply_change_set`, a GraphML or GraphSON import into a live graph and a
schema change. The commit itself is covered too: a panic in a commit hook's `before_commit` rolls
the unit back before it continues (see [Commit hooks](#commit-hooks)); a panic in `after_commit`
leaves the unit committed.
No unit is left open afterwards, so the graph keeps working. The success path pays nothing for it
(no allocation, no clock).

With `panic = "abort"` (the default on `wasm32-unknown-unknown`) there is nothing to catch: the
process ends, and the graph with it.

## Not covered yet

- Transactions in the script DSL (`g.tx()`) and a dry-run preview in the playground.
- Commit hooks registered from Python (Python gets change sets only as data, through `dry_run`).
- Removing a commit hook.
- Isolation of concurrent readers during a write: readers wait for the writer. A layered storage
  with snapshot reads is planned.

## Deviations from TinkerPop

- TinkerGraph without transactions keeps the changes a traversal made before it failed.
  Graphersal rolls them back: the graph is never left half-changed by one failing traversal.
- TinkerPop opens an explicit transaction with `g.tx()` and ends it with `commit()` /
  `rollback()`. Graphersal has no `tx()`; the Rust API uses the closure
  `transaction(|g| ..)`, and a host can make a whole script one unit.
- TinkerPop's `EventStrategy` reports each mutation to a `MutationListener` after it happened.
  Graphersal's commit hooks get one `ChangeSet` per committed unit, before it is final (with a
  veto) and after it; there are no per-mutation events.

See [TinkerPop Deviations](../for_developers/tinkerpop_deviations.md).
