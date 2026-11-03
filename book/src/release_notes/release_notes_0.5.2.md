# Release 0.5.2

A patch release of the 0.5 line. Three fixes come from a benchmark against another in-memory
graph database on graphs of up to 5 million vertices and 10 million edges: whole-pattern counts
in GQL, membership tests against side effects, and the spare memory a graph built by write
queries keeps. The release also adds the GQL operator `=~` and gives every note of the command
line tool one readable layout. It changes no format; stores, snapshots and journals of 0.5.0 and
0.5.1 are read and written exactly as before. The new API items are additive.

## Added

- **`shrink_to_fit()` releases the spare capacity of a graph built by write queries**
  ([#22](https://github.com/graphersal/graphersal/issues/22)). A graph grows its vertex and edge
  slots by doubling; after a bulk load by queries up to half of them can stay unused (on 5M
  vertices and 10M edges built with GQL `CREATE`: 1.24 GB of 5.0 GB). Every load of a file
  already ended compact; now a graph in memory can be made compact too: `g.shrink_to_fit()` in
  the DSL, `graph.shrink_to_fit()` in Python, `Graph::shrink_to_fit()` /
  `AnyGraph::shrink_to_fit()` in Rust (`ShrinkReport` in the `storage` module). It reports the
  memory before and after, writes no commit and keeps every handle valid; call it once after a
  bulk load, not after every batch. It asks for `Update` on the data
  ([Permissions](../users_guide/permissions.md)), so a read-only policy refuses it
  ([Memory Inspection and Profiling](../users_guide/memory_and_profiling.md#releasing-spare-capacity-shrink_to_fit)).
- **GQL: the regular-expression match `s =~ pattern`**, Cypher's operator as a compatibility
  form: it matches the whole string, in the dialect of the Rust `regex` crate; a NULL or
  non-string operand gives NULL, an invalid literal pattern is refused before the query runs,
  and `n.key =~ 'literal'` is answered with `has(key, TextP.regex(..))`
  ([GQL aliases](../gql/aliases.md)).

## Changed

- **GQL counts over a whole pattern are answered from the statistics**
  ([#20](https://github.com/graphersal/graphersal/issues/20)). `MATCH (n) RETURN count(n)`,
  `MATCH (n:Label) ...`, `MATCH ()-[r]->() RETURN count(r)`, `MATCH ()-[r:TYPE]->() ...` and
  their `count(*)` forms now get the plans of `g.v().count()` / `g.e().count()`: on 10M edges
  the edge count takes 0.03 ms instead of 1.3 s, and it no longer fails the default 1 GiB
  per-query memory limit. Every other `count(x)` of a variable that can never be NULL no longer
  records the variable for each row, so counts over larger patterns need less memory. Results do
  not change ([GQL plans](../gql/plans.md#counts)). An edge count now reads edges only, as
  `g.e().count()` does: a policy that denies reading vertices but allows edges allows it.
- **`within` / `without` against a side effect or a long list are hash lookups**
  ([#21](https://github.com/graphersal/graphersal/issues/21)). `where(P.without("x"))` against an
  `aggregate("x")` or `store("x")` of 16 or more members, and `has` / `is` / `has_value` with a
  literal `within` / `without` list of 16 or more values, no longer compare against every member
  for every traverser. The usual breadth-first search with a visited set,
  `aggregate("x").repeat(__.both().where(P.without("x")).aggregate("x"))`, was quadratic: on
  100 000 vertices and 1M edges it now takes 0.7 s, where 0.5.1 did not finish in 10 minutes.
  The comparison itself is unchanged (a hash only proposes candidates), so results do not change;
  one edge case: when reading a member fails, its error now surfaces only if no other member
  matches.
- **Every note of the command line tool has one layout** (#19): the notes of an upgrade, a
  `.gsnap` load, a model import, the store refusals, the `store`, `embed-model` and start
  outputs, the error helps that name commands and the journal's recommendations are a short
  headline, one indented `key: value` per line and each shell command on a line of its own under
  a `# comment`. `graphersal store models`, `store attic list` and `embed-model list` show one
  section per entry.

## Fixed

- `graphersal store info` counts the blobs in use and the unused ones apart; before, the row read
  as if a model's blob were about to be removed by the next prune (#19).

## Upgrading from 0.5.1

A drop-in replacement: no format change, no upgrade step, only additive API. Stores, snapshots,
journals and backups of 0.5.0, 0.5.1 and 0.5.2 are interchangeable.
