# Releases and Versioning

## Versioning

Graphersal follows [Semantic Versioning](https://semver.org). Before 1.0 a minor version (0.1 to 0.2) may break
the API; a patch version (0.2.0 to 0.2.1) does not.

Every crate of the workspace shares one version number (`[workspace.package]` in the root `Cargo.toml`), and the
Python package and the web playground are released with the same number:

| Artifact | Where | Notes |
|---|---|---|
| `graphersal` | crates.io | the library: engine, Rhai DSL, I/O, persistence (feature flags) |
| `graphersal-session` | crates.io | the playground session logic, published because the CLI depends on it; internal, no stability guarantees |
| `graphersal-cli` | crates.io | the `graphersal` binary: REPL, one-shot runner, `graphersal store`, the dev server |
| `graphersal` (CLI binaries) | GitHub Release | prebuilt archives of the `graphersal` binary for five platforms, with `SHA256SUMS` and build provenance attestations |
| `graphersal-storage-tests` | crates.io | the `GraphStorage` conformance suite, EXPERIMENTAL like the trait (pin the exact version) |
| `graphersal` (Python) | wheels (abi3, Python 3.10+) | the PyO3 binding from `bindings/py-graphersal` |
| web playground | static files | runs `graphersal-wasm` (not published; its JS API is internal to the playground) |

## Stability in 0.3.x

The same rules hold for every minor line before 1.0 (they held for 0.1.x and 0.2.x as well); a minor
release (0.2 to 0.3) may break them, and its release notes list every break with the upgrade step
([Release Notes 0.3.0](../release_notes/release_notes_0.3.0.md)).

- Stable within 0.3.x (additive changes only): the public API of `graphersal` outside the items marked
  EXPERIMENTAL, the DSL and the schema format.
- EXPERIMENTAL (may change in any 0.3.x release): the `GraphStorage` trait and its associated types,
  `PersistentStorage`, `AnyGraph`, `StoreDir`, and `graphersal-storage-tests`; pin the exact version.
- Internal: `graphersal-session`, the wasm crate's JS API, the dev server's HTTP API and its MCP tools
  (`--server` is a development tool, bound to the loopback interface), the experimental AI Chat.
- The storage format has its own version numbers ([Format Versions and Upgrade](../persistence/store_versions.md)):
  a patch release never changes a format version, and every release reads the formats of every
  release before it (an older store is upgraded with `graphersal store upgrade`, no older build needed).

The API rules that keep a change additive (public enums and result structs `#[non_exhaustive]`, default methods
for every new trait method) are checked with [`cargo-semver-checks`](https://github.com/obi1kenobi/cargo-semver-checks):

```bash
cargo install --locked cargo-semver-checks          # one-time
cargo semver-checks --package graphersal --baseline-rev <previous release tag>
```

From 0.1.0 on a reported break is a release blocker unless the version number says so.

## Breaking changes in 0.4.0 (unreleased)

Collected for the release notes of 0.4.0. One user decision (2026-10-28) drives them: a public
method that touches shared state (the graph's lock, the storage, a Store, files, embedders)
returns a `Result` with a `#[non_exhaustive]` error type, even where it cannot fail today, and
never answers a failure with an empty or default value. The upgrade step for each is "handle the
`Result`" (`?`, or `.unwrap()` where the caller knows better).

`graphersal`:

- `AnyGraph` (EXPERIMENTAL): `capabilities() -> Result<StorageCapabilities, GraphError>`,
  `last_commit_seq() -> Result<u64, GraphError>`,
  `statistics() -> Result<Option<GraphStatistics>, GraphError>`,
  `memory_usage() -> Result<Option<MemoryStats>, GraphError>`,
  `get_schema() -> Result<Option<GraphSchema>, GraphError>`,
  `definitions(kind) -> Result<Vec<Definition>, GraphError>`,
  `definition(kind, name) -> Result<Option<Definition>, GraphError>`,
  `compression_stats(name) -> Result<Option<CompressionStats>, GraphError>`,
  `property_indexes() -> Result<Vec<PropertyIndexInfo>, GraphError>` (were the plain values; a
  read the current thread may not have was answered as "no data", now `GraphError::GraphViewHeld`).
- `Graph::advise` fails when a compression estimate fails for another reason than
  `GraphError::Unsupported` (was: advice without compression recommendations).
- `Store`: `read_only_reason() -> Result<Option<String>, PersistError>`,
  `stopped_reason() -> Result<Option<String>, PersistError>`,
  `damage_policy() -> Result<DamagePolicy, PersistError>`,
  `store_id() -> Result<u128, PersistError>`,
  `damage_found() -> Result<Vec<FoundDamage>, PersistError>`,
  `last_auto_checkpoint_error() -> Result<Option<Arc<PersistError>>, PersistError>` (was
  `Option<String>`), `user_jobs() -> Result<UserJobs, PersistError>` (was `Vec<Job>`;
  `UserJobs { jobs, not_scheduled }` lists the definitions it does not schedule with the reason),
  `stored_embedders(&registered) -> Result<StoredEmbedders, PersistError>`,
  `Store::is_store(dir) -> Result<bool, PersistError>` and
  `SingleFileDir::is_single_file(path) -> Result<bool, PersistError>` (an unreadable directory or
  file is `PersistError::Io`, not `false`). A poisoned internal lock is
  `StoreFailure::LockPoisoned` (was a default answer).
- `Store::info` (and `StoreInfo` everywhere) fails when the attic or the models directory cannot
  be read (was an empty list); a fork or attic fork fails before writing when the marks it does not
  carry cannot be listed.
- `GraphStorage` (EXPERIMENTAL; implementers and callers): the methods beyond single elements
  return `Result<_, GraphError>` (defaults: `Ok` of the old default): `statistics`,
  `infer_schema`, `get_schema`, `declared_property_schema`, `definitions` (`Result<impl
  Iterator>`), `definition`, `compression_stats`, `property_indexes`, the four property index
  lookups (`Result<Option<Box<dyn Iterator>>>`), `shrink_to_fit`, `memory_usage`,
  `memory_structure`, `memory_footprint_bytes`, `execution_policy`, `begin_unit`,
  `begin_unit_at`, `next_unit_started_at`, `rollback_unit`, `unit_changes`, `set_unit_metadata`,
  `capabilities`, `last_commit_seq`. A failed `rollback_unit` still closes the unit; the engine
  reports `GraphError::RollbackFailed`.
- Behaviour (user decision 2026-10-28): a failed rollback POISONS the `Graph` (was: "the graph
  stays usable"). When a host storage's `rollback_unit` (or `discard_unit`, other than the
  `Unsupported` of a storage without change capture) fails in any unit the engine closes on a
  `Graph` (a traversal, a transaction or dry run on a write view, an import, a change-set apply,
  a held unit's close, a panic unwinding through a unit), the graph refuses every later data
  operation with the new `GraphError::Poisoned { cause }` (`read_checked`, `write_checked`,
  `try_write_checked`, `begin_unit`, the closes of a held unit, the `AnyGraph` methods, sessions;
  `try_read`/`try_write` answer `None`) and `Graph::read()`/`write()` panic. No unpoison: drop the
  graph and reopen the storage. `TraversalGraph` and the Store cannot get there (their rollback
  cannot fail). Upgrade step for a host storage: return `Err` from `rollback_unit` only when the
  storage really is in an unknown state. The poison is isolated per graph: the failing storage is
  named by the new `GraphStorage::storage_identity() -> StorageIdentity` (opaque; default
  `StorageIdentity::of(self)`; a wrapper over a storage held elsewhere forwards it, as the shipped
  wrapper impls do; the pinned clock of a held unit is now found the same way, also for
  `Graph<Box<TraversalGraph>>` and wrapper storages), so a failure never
  poisons another graph on the same thread, and a bare storage poisons nothing.
- Behaviour: a storage read that fails while the engine builds the diagnostic of another error
  (the value origin of cast, numeric and incomparable-value errors, the type a string step, a
  list function or `T.value` names) no longer replaces that error with the storage's: the
  original error is reported without the unreadable detail.
- `GraphStorage`, the element reads too: `vertex_count`/`edge_count` -> `Result<usize, _>`,
  `vertices`/`edges`/`out_edges`/`in_edges` -> `Result<impl Iterator, _>` (one `Result` per
  call, never per item), `vertices_by_label`/`edges_by_label` -> `Result<Box<dyn Iterator>, _>`
  (their defaults now collect the matches first, so a failing label read is the call's error),
  `get_vertex_by_id`/`get_edge_by_id`/`vertex_id`/`edge_id`/`vertex_label`/`edge_label`/
  `vertex_properties`/`edge_properties`/`edge_endpoints` -> `Result<Option<_>, _>`,
  `vertex_labels` -> `Result<&[CompactString], _>`, `vertex_has_label`/`contains_vertex`/
  `contains_edge` -> `Result<bool, _>`, `unit_started_at` -> `Result<Option<i128>, _>`. The
  engine fails the reading query with the error, located at its step. `TraversalGraph`'s own
  inherent `vertex_count()`/`edge_count()` stay plain `usize` (an in-memory count that cannot
  fail).
- `CreateContext::for_storage` and `GraphStatistics::from_storage` return
  `Result<_, GraphError>` (they read the storage).
- `PersistentStorage` (EXPERIMENTAL): `set_graph_id`, `raise_position`, `reserve_load`,
  `finish_load`, `clear` return `Result<(), GraphError>`.
- `StoreDir` (EXPERIMENTAL): `space_usage() -> io::Result<Option<SpaceUsage>>`,
  `free_space() -> io::Result<Option<u64>>`, `check() -> io::Result<DirCheck>`. A failing
  `free_space` fails the compaction advice (`PersistError::Io`) instead of reading as unknown; a
  failing `check` opens the Store in maintenance mode with that damage (as `SingleFileDir` did
  for an unreadable file).
- `GraphTraversalSource::statistics() -> TraverserResult<Option<&GraphStatistics>>` and
  `memory_usage() -> TraverserResult<Option<MemoryStats>>` (were the plain values).

- Added (not breaking): `Graph::try_write_checked`, `UserJobs`, `NotScheduledJob`,
  `RepairReport::marks_unreadable`, `StoreFailure::AutoCheckpointFailed`,
  `GraphError::StorageFailed` (`GraphError::storage_failed`), `GraphError::RollbackFailed`
  (`GraphError::rollback_failed`), `GraphError::Poisoned` (`GraphError::poisoned`),
  `Graph::is_poisoned`, `Graph::poison_cause`, `GraphStorage::storage_identity` (with a
  default) and `storage::StorageIdentity`, `graphersal-storage-tests`' `failing::Failing` (with the `discard_unit`,
  `element_identity` and `fail_element_reads_after` faults).

`graphersal-session` (internal, listed for the CLI and the wasm crate):

- `SessionError` replaces `String` as the error of every public method that returned
  `Result<_, String>` (session methods, `SessionGraph`, `GraphFactory`, the generic helpers of
  `views`, `filter`, `selection`, `create`, `edit`, `queries`, `compression`, `layout`, `advisor`,
  `store::parse_time`).
- Now `Result<_, SessionError>`: `Session::info`, `statistics`, `get_schema`, `get_schema_text`,
  `infer_schema`, `infer_schema_text`, `load_shared`, `attach_store`, `list_queries`, `get_query`,
  `query_count`, `save_catalog_text`, `list_compressions`, `get_compression`,
  `compression_count`, `list_indexes`, `get_index`, `index_count`, `jobs_view`, `job_entry`,
  `workload_report`, `tick_jobs`, `read_only_reason`.
- `SessionGraph`: `vertex_count`, `edge_count`, `inferred_schema`, `schema_view`,
  `schema_view_of`, `graph_view`, `add_degrees`, `statistics_view`, `memory_view`, `element`,
  `subgraph`, `element_view`, `drawn`, `incident_edges`, `queries`, `query`, `count_kind`,
  `catalog_text`, `compressions`, `compression`, `indexes`, `index`, `run_changes`, `rollback_run`
  return `Result<_, GraphError>`.
- The generic helpers over a storage's element reads return `Result<_, GraphError>` too:
  `CompactGraph::build`, `CompactGraph::add_degrees`, `display::element_of`
  (`Result<Option<_>, _>`), `display::node_of`, `display::edge_of`, `display::collect_subgraph`
  (`Result<Option<_>, _>`), `views::graph_view`, `create::drawn_in`, `create::incident_edges`
  (`Result<Option<usize>, _>`), `edit::element_view`.
- `SessionStore`: `read_only_reason`, `damage_found`, `damage_policy`,
  `last_auto_checkpoint_error`, `user_jobs` return `Result<_, PersistError>` like `Store`.
- The generic helpers `compressions_in`, `compression_in`, `indexes_in`, `index_in`,
  `queries_in`, `query_in`, `count_kind`, `catalog_text`, `views::schema_view`,
  `schema_view_of`, `statistics_view`, `memory_usage` return `Result<_, GraphError>` (they read
  through the `GraphStorage` methods that now return one).
- `store::found_damage_value -> Result<Value, PersistError>`; `StoreError` is
  `#[non_exhaustive]` (build it with `StoreError::new`, `bad_request`, `conflict`, `refused`,
  `internal`, `with_data`, or `From` a `SessionError`, `PersistError` or `GraphError`).
- The Store menu's JSON gains `compactionError` (beside `compaction: null` when the advice
  cannot be computed).

`graphersal-wasm` (internal JS API): `info`, `getSchema`, `inferSchema`, `schema`, `graphView`,
`memoryUsage`, `statistics`, `tickJobs`, `jobs`, `listQueries`, `getQuery`, `saveCatalog`,
`listCompressions`, `listIndexes` throw the error instead of answering an empty value.

Python: `Store.close()` raises on a poisoned store lock (was "already closed"), `Graph.execute`
raises on a poisoned limits lock (was the default limits).

## Release gates

Before a version is tagged:

- the CI workflow, started by hand with `full` (`gh workflow run CI -f full=true`: Linux and Windows), is green:
  format, clippy, every test suite incl. the TinkerPop suite, the feature and wasm32 matrices, the MSRV, the
  playground tests;
- the checks CI leaves out pass locally: `RUSTDOCFLAGS="-D warnings" cargo doc` with the feature sets of
  `CLAUDE.md`, `mdbook build book`, and the TinkerPop suite on the foreign storage
  (`GRAPHERSAL_TEST_STORAGE=forwarding`);
- `cargo deny check` (licences, RustSec advisories, duplicate crates, sources) passes against `deny.toml`;
- the long fuzz budgets pass: robustness, the merge checkpoint and the verified backups (below; a fast budget of
  each runs in every `cargo test`);
- a benchmark run on a quiet machine shows no unexplained regression against the previous release;
- a dry run of the CLI binaries (`gh workflow run "CLI binaries" --ref <release branch>`, see Publishing) is
  green on all five platforms;
- `cargo publish --dry-run` succeeds for every published crate, `graphersal` first, then the crates that depend on it;
- the version's release notes (`book/src/release_notes/`, for 0.2.0
  [Release Notes 0.2.0](../release_notes/release_notes_0.2.0.md)) describe what changed; they are the project's
  record of changes (there is no separate `CHANGELOG.md`) and the text of the GitHub Release.

The long fuzz budgets (release builds with overflow checks and debug assertions kept on):

```bash
CARGO_PROFILE_RELEASE_OVERFLOW_CHECKS=true CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS=true \
ROBUSTNESS_CASES=500000 ROBUSTNESS_SEED=23 ROBUSTNESS_THREADS=7 \
  cargo test --release -p graphersal --features script,io,display,serde,persist,gql \
  --test all robustness_fuzz_tests::fuzz_long -- --ignored
ROBUSTNESS_INPUT_CASES=20000 cargo test --release -p graphersal \
  --features script,io,display,serde,persist,gql --test all robustness_input_tests::
MERGE_FUZZ_CASES=60000 MERGE_FUZZ_SEED=1 MERGE_FUZZ_THREADS=8 cargo test --release -p graphersal \
  --features script,io,display,serde,persist,gql,persist-zip --test all merge_checkpoint_fuzz_tests::
BACKUP_FUZZ_CASES=60000 BACKUP_FUZZ_SEED=1 BACKUP_FUZZ_THREADS=8 cargo test --release -p graphersal \
  --features script,io,display,serde,persist,gql,persist-zip --test all backup_fuzz_tests::
```

Robustness findings go to `target/robustness-findings.txt` (`ROBUSTNESS_REPLAY=<seed>` replays one case); a failing
merge or backup case prints `MERGE_FUZZ_REPLAY=<seed>` or `BACKUP_FUZZ_REPLAY=<seed>`.

## Benchmarks

`cargo bench -p graphersal --bench graph --features io,script,display,serde` is the regression
benchmark. Every supported step or feature has at least one representative shape in
`crates/graphersal/benches/graph.rs`, run on a graph large enough to surface an O(n²)-class
regression (the 110k-element `large` graph), so a change that reintroduces an allocation- or
complexity-class regression is caught by `cargo bench` rather than by a user profiling a real
dataset. A new step gets its bench shape with the step. Benchmarks run once per release on a quiet
machine and are compared with the previous release, not per change.

## Publishing

1. Tag the release commit: `git tag -a v0.2.0 -m "Graphersal 0.2.0" && git push origin v0.2.0` (the tag equals the workspace `version`).
2. The crates go to crates.io by hand, in dependency order: `cargo publish -p graphersal`, then
   `graphersal-storage-tests`, `graphersal-session` and `graphersal-cli`.
3. The workflow `release.yml`, started on the tag: `gh workflow run Release --ref v0.2.0`. It checks that the
   tag matches the version and that the version's release notes exist, then runs two independent parts:
   - **PyPI.** It builds the five-platform wheels and the sdist through `python.yml` (which also runs the
     binding's tests and installs each wheel it can run), and uploads them with PyPI Trusted Publishing from
     the environment `pypi` (no token is stored).
   - **CLI binaries and the draft GitHub Release.** It builds the `graphersal` binary through `binaries.yml`
     with the `dist` profile (release + fat LTO + stripped symbols; never `panic = "abort"`, units roll back
     on unwind), natively on each platform's runner: Linux x86_64 and aarch64 as static musl builds,
     macOS arm64 and x86_64, Windows x86_64. Each build is smoke-tested on its runner (`--version`, a query,
     the dev server's `/api/info`) and packed as `graphersal-v<version>-<platform>.tar.gz` (Windows `.zip`)
     with `LICENSE-APACHE`, `NOTICE`, `README.md` and `THIRD-PARTY-LICENSES.html` (cargo-about, `about.toml`).
     A final job writes `SHA256SUMS` and creates the GitHub build provenance attestations. The job
     `github-release` then creates a **draft** GitHub Release for the tag, titled "Graphersal <version>",
     with the five archives and `SHA256SUMS` attached; its text is the version's release notes
     (`book/src/release_notes/release_notes_<version>.md`) with the book links made absolute on
     graphersal.dev, the soft line breaks joined and a section about the prebuilt binaries added
     (`.github/scripts/release_notes.py <version>` prints the same text locally).
4. Check the draft on the repository's Releases page: five archives (`linux-x86_64`, `linux-aarch64`,
   `macos-arm64`, `macos-x86_64`, `windows-x86_64`) plus `SHA256SUMS`; download one archive and check it
   (`sha256sum -c SHA256SUMS --ignore-missing`, `gh attestation verify <archive> --repo graphersal/graphersal`,
   unpack it and run `graphersal --version`); read the release text, its links included. Then publish the
   draft ("Publish release", or `gh release edit v0.2.0 --draft=false`). Only a published release is visible
   to users and to `cargo binstall graphersal-cli` (its metadata in `crates/graphersal-cli/Cargo.toml`
   names the same archives).

Before tagging, the binaries can be built as a dry run on any branch: `gh workflow run "CLI binaries" --ref
<branch>` builds, smoke-tests and uploads the archives as workflow artifacts and publishes nothing (no
attestations). `-f compare_musl=true` adds a job that times the same queries on the `large` graph with a glibc
and a musl build of linux-x86_64 and writes the comparison into the run's summary.
