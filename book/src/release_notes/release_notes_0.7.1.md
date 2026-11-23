# Release 0.7.1

A patch release of the 0.7 line. Running a large query again and again no longer grows the
process on macOS ([#36](https://github.com/graphersal/graphersal/issues/36)), and large
expansions run 5 to 35 % faster. GQL calls the allocator less per row. The dev server gets a run
administration that lists and stops every client's queries
([#32](https://github.com/graphersal/graphersal/issues/32)), and a store conversion can leave
out the blobs it cannot find.

It changes no format; stores, snapshots and journals of 0.7.0 are read and written exactly as
before. The API changes of `graphersal` and of the EXPERIMENTAL crate `graphersal-serve` are
additive (`cargo semver-checks` reports no break of `graphersal` against 0.7.0).

## Upgrading from 0.7.0

A drop-in replacement: no format change, no upgrade step. The run administration stays off
unless the dev server is started with `--admin-runs`.

## Fixed

- **Repeated executions keep memory flat on macOS**
  ([#36](https://github.com/graphersal/graphersal/issues/36)). A query whose intermediate
  results grew beyond about 8 MB kept that memory after every execution. A two-hop over 400k
  edges added about 288 MB per run: 2.4 GB of resident memory after 8 runs. It now stays at
  about 440 MB however often it runs. The engine did not leak: the macOS allocator never reuses
  a large block that `realloc` grew in place once it is freed. The engine's step buffers
  (expansions, `repeat()`, filter and write outputs, the path arena and the undo log) now grow
  by moving to a new block sized from a projection of the inputs so far, so the peak of a single
  large expansion is also slightly lower. Buffers below 128 KiB grow as before. Other platforms
  are unaffected.

  The fix matters most where one process runs many queries: the REPL, the dev server, MCP agents
  and host applications. Maximum resident memory after 1 and after 8 runs:

  | Query | 0.7.0 | 0.7.1 |
  |---|---|---|
  | two-hop over 400k edges | 386 -> 2399 MB | 441 -> 438 MB |
  | GQL variable-length match `*1..3` | 683 -> 1676 MB | 712 -> 712 MB |
  | GQL `SET` over 250k rows | 172 -> 495 MB | 162 -> 164 MB |
  | GQL two-hop with `ORDER BY ... LIMIT 3` | 194 -> 314 MB | 169 -> 169 MB |

## Performance

- **Large expansions are faster.** A large step buffer no longer faults in fresh pages on every
  execution, so the benchmark cases that expand many traversers on the `large` graph run 5 to
  35 % faster than in 0.7.0: `repeat(out()).times(3).count()` 23 %, `out().out().count()`
  21 %, `out().out().simple_path()` 16 %, `as()`/`select()` over two hops 12 %, `both_v()` 9 %,
  `tree()` 5 %. A benchmark process of such a case peaks at about 100 MB instead of 0.5 to
  1.6 GB. The two-hop benchmarks, which exceeded 5 GB in 0.7.0, run at 420 MB. Across all 145
  benchmark cases the geometric mean is 2.5 % faster.
- **GQL allocates less per row.** `ORDER BY` no longer allocates per row for its sort keys
  (`ORDER BY ... LIMIT 10` over 10k rows: about 20 000 -> 10 000 allocator calls). Function
  calls (`toString`, `substring`, `left`, `right`, `toUpper`, `toLower`, `replace`, ...) no
  longer allocate for their arguments, and short string results stay inline (`substring` and
  `toString` over 10k rows: 70 000 and 80 000 -> 40 000 calls). A GQL query's steps now
  allocate per row as the equivalent Gremlin `project()` query does.

## Added

- **Run administration for the dev server** (EXPERIMENTAL, off by default;
  [#32](https://github.com/graphersal/graphersal/issues/32)).
  `graphersal --server --admin-runs [--admin-runs-token <SECRET>]` lets the operator see every
  query that waits or runs, from every client (browser pages, MCP agents, the AI Chat), and stop
  any of them. `GET /api/admin/runs` lists each run's id, client, kind, state (queued or
  running), start time, duration and the first 200 characters of its text.
  `POST /api/admin/runs/cancel` `{"id": n}` cancels one, whatever its client, and rolls back
  its changes. With a token, every admin request must send `X-Graphersal-Admin-Token`. Without
  the flag both endpoints answer 404. While it is on, `GET /api/info` answers
  `adminRuns: {tokenRequired}` and the playground shows a **Runs** button with a live list and
  a Kill button per run. The MCP endpoint offers no tool for it. Both flags are also keys of the
  arguments file, and the start banner names the setting
  ([Run administration](../persistence/dev_server.md#run-administration)).
- **`graphersal-serve` (EXPERIMENTAL, additive).** A host turns the run administration on per
  graph with `ContextBuilder::admin_runs(token)` (`Context::admin_runs_enabled`). A graph
  served under a mount path lists and stops only its own runs. A host's own view across its
  graphs uses `Runs::list` and `Runs::cancel_run`, with the new `RunEntry` and `RunState` and
  the new `RunKind::name` and `RunInfo::mount`.
- **A store conversion can leave out the blobs it cannot find.**
  `ConvertOptions::with_skip_missing_blobs()` makes `Store::convert_with` complete when a blob
  the store references (an embedding model's) is missing from the source store and from the fill
  source: the copy is made without it, never with a placeholder, and the new field
  `ConvertReport::blobs_missing` names each one. The copy is a store whose blob was lost, which
  is not damage: its model is listed as missing, an open is refused while the schema needs the
  model, and `Store::restore_blobs_from` fills the blob later. Everything else is still verified;
  the default stays strict. See [Blobs](../persistence/store_blobs.md#damage).
