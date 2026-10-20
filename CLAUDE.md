# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

**Graphersal** is an in-memory graph traversal engine in Rust modelled on Apache TinkerPop Gremlin. Agent guidance is this file plus the nested `CLAUDE.md` files below, each beside the code it describes (Claude Code loads one when it reads a file under its directory; `AGENTS.md` only points here). When guidance disagrees with the code, the code wins. Tell the user so the stale section gets fixed. Keep this file short: detail of a subsystem goes into its nested file, not here.

| Nested guidance | Covers |
|---|---|
| `crates/graphersal/src/graph/CLAUDE.md` | storage, handles, custom storages, units/undo log, change capture and hooks, statistics, memory, catalog, compressed properties, property indexes, permissions |
| `crates/graphersal/src/graph/schema/CLAUDE.md` | schema format, defaults, inference, enforcement, apply, diff |
| `crates/graphersal/src/traversal/CLAUDE.md` | step categories, optimizer rules, path analysis, bulk, sack, Pop, profiling, execute, rendering/spelling, limits, cancellation, memory budget |
| `crates/graphersal/src/value/CLAUDE.md` | values, maps, temporal, vectors, embedders, Model2Vec, jpath keys |
| `crates/graphersal/src/script/CLAUDE.md` | Rhai DSL layout, lowering, registration and docs, tokens, saved queries, script limits and permissions |
| `crates/graphersal/src/persist/CLAUDE.md` | snapshots, WAL, recovery, versions/upgrades, Store, checkpoints, backups, rollback/attic, damage, single-file store |
| `crates/graphersal/src/io/CLAUDE.md` | GraphSON and GraphML |
| `crates/graphersal/src/gql/CLAUDE.md` | GQL query language (feature `gql`): stage layout, parser rules, lowering into the pipeline |
| `crates/graphersal/tests/tinkerpop/CLAUDE.md` | TinkerPop conformance suite |
| `crates/graphersal/tests/opencypher/CLAUDE.md` | openCypher TCK (GQL), vendored data, attribution wording rule |
| `crates/graphersal-cli/CLAUDE.md` | REPL, output rules, dev server, MCP, AI Chat, `embed-model` |
| `crates/graphersal-session/CLAUDE.md` | session modules, any-storage session |
| `playground/CLAUDE.md` | playground UI structure |
| `bindings/py-graphersal/CLAUDE.md` | Python binding, GIL rule |
| `book/CLAUDE.md` | book structure, runnable examples, generated step reference |

Nested files under `crates/graphersal/src/` are kept out of the published crate by `"!/src/**/CLAUDE.md"` in its `include`.

## Workspace

- `crates/graphersal`: the core library. Almost all work happens here.
- `crates/graphersal-cli`: the REPL, a one-shot scriptable query runner, `graphersal store ...`, and `--server`: the DEV server serving the embedded playground with this process as the engine, optionally with MCP (`--mcp`) and the AI Chat (`--ai`).
- `crates/graphersal-session`: the playground session logic (JSON in, JSON out), shared by `graphersal-wasm` and the dev server; serves any storage. Publishable but internal.
- `crates/graphersal-wasm`: the engine as WebAssembly (wasm-bindgen, `publish = false`), a thin wrapper of `graphersal-session`; its JS API is internal to the playground (an npm package is planned).
- `playground/`: the static browser playground (Vue, no bundler) on `graphersal-wasm` in a Web Worker, or on the dev server.
- `bindings/py-graphersal`: the PyO3/maturin Python binding (package `graphersal`).
- `crates/graphersal-storage-tests`: the `GraphStorage` conformance suite (`conformance!`/`conformance_core!`; capability-conditional items in `extension`; schema items skip when `set_schema` is `Unsupported`; public facade paths only, no `compact_str` dependency). Published, EXPERIMENTAL like the trait; `tests/no_schema/` is a wrapper keeping the schema defaults. `src/forwarding.rs` is `Forwarding` (forwards every method to a `TraversalGraph`, own handle types, runs the full `conformance!`); `graphersal`'s `tests/all/foreign_storage_tests.rs` includes it by `#[path]` (a dev-dependency would link a second `graphersal`) and runs the DSL differential `Graph<TraversalGraph>` vs `Graph<Forwarding>` over every `register/docs.rs` example (`graphersal::script::doc_examples`) plus curated scripts: zero differences allowed.
- `crates/graphersal/benches/graph.rs`: the Divan bench. The root manifest is a virtual workspace.
- `book/`: mdBook docs.
- `ai/`: session working files (plans, reports), git-ignored, never published, never linked from published files. Long-term plans and unimplemented designs live in `ai/roadmap/`; planned work is tracked in GitHub issues; an implemented design is removed and documented in the book, which code comments cite instead. Pre-release history: the frozen, read-only repository `/Users/aleks/Development/open_cloud_inventory` and the private archive `github.com/graphersal/graphersal-history`.

`graphersal` feature flags. All are off by default and must stay strictly additive:
- `io`: GraphSON 3.0 (primary exchange format, dependency-free) and GraphML (`quick-xml`); neither carries the schema (`src/io/CLAUDE.md`).
- `script`: the Rhai DSL.
- `display`: tables via `tabled`. Never pull `tabled` into `script` or the engine.
- `serde`: `ChangeSet` serialization, `Serialize`/`Deserialize` of `GraphSchema` (the schema format itself, `from_json`/`to_json`, needs no feature) and the `json` visualizer. It is not a graph file format.
- `persist`: the native lossless binary format, journal, recovery and the Store (`lz4_flex`, `crc32c`); `persist-zstd` adds zstd chunks, `persist-zip` ZIP backups. All build for wasm32 (`src/persist/CLAUDE.md`).
- `alloc-tracking`: exact per-step memory figures for `.profile_with(ProfileType::Memory)` via `alloc_tracking::TrackingAllocator`, which the *binary* installs as its `#[global_allocator]`. Off for `graphersal` itself, but `graphersal-cli`, `graphersal-wasm` (wasm32 only) and `py-graphersal` build with it ON and install it: a small constant per-allocation cost (thread-local counters plus one relaxed global atomic, `alloc_tracking::global_live_bytes`, which the memory budget reads). Without it the `Mem` column is `MemorySource::Estimated`. A bare `.profile()` never collects memory data.
- `model2vec`: the local-directory Model2Vec embedder, native only, never for wasm32 (`src/value/CLAUDE.md`).
- `gql`: the ISO GQL read query language (hand-written parser, lowered into the ordinary pipeline), no dependencies, independent of `script`, builds for wasm32 (`src/gql/CLAUDE.md`).

The acceptance feature set is `script,io,display,serde,persist,gql` (the surface snapshot is taken with it; without `persist` the snapshot functions are filtered out of the comparison).

The workspace is on Rust edition 2024 (MSRV 1.88). Doctests are merged into one binary per crate: a doctest must not rely on process-global state, env vars or `#[global_allocator]`; mark one that must stay separate with `standalone_crate`.

## Commands

```bash
cargo fmt   --all --check                       # rustfmt.toml: max_width = 100, stable options only
cargo check --workspace --all-targets
cargo test  --package graphersal --features script,io,display,serde,persist,gql
# integration tests are ONE binary (crates/graphersal/tests/all/main.rs, one module per former file):
# a single module (= former test file) / a single test
cargo test  --package graphersal --features script,io,display,serde,persist,gql --test all script_tests::
cargo test  --package graphersal --features script,io,display,serde,persist,gql --test all script_tests::<test_name_substring>
# TinkerPop 3.8.2 Gherkin conformance suite (also part of the plain command above); every run
# writes the full report to target/tinkerpop-report.md
cargo test  --package graphersal --features script,io,display,serde,persist,gql --test tinkerpop_features
TINKERPOP_UPDATE_BASELINE=1 cargo test --package graphersal --features script,io,display,serde,persist,gql --test tinkerpop_features   # rewrite the baseline AND the generated blocks of the compliance page
# openCypher TCK 2024.3 against GQL (separate target, feature gql; runner being built in 3b); every run
# writes target/opencypher-report.md
cargo test  --package graphersal --features script,io,display,serde,persist,gql --test opencypher_tck
OPENCYPHER_UPDATE_BASELINE=1 cargo test --package graphersal --features script,io,display,serde,persist,gql --test opencypher_tck   # rewrite its baseline AND the generated blocks of book/src/gql/tck_compliance.md
cargo test  --package graphersal-cli                          # CLI contract (tests/cli_tests.rs) and the dev server (tests/server_tests.rs, port 0)
cargo test  --package graphersal-storage-tests                # GraphStorage conformance for TraversalGraph, its wrappers and Forwarding
cargo test  --package graphersal --doc --features script,io
RUSTDOCFLAGS="-D warnings" cargo doc --package graphersal --no-deps                                   # also with --features script,io,display,serde,persist,gql
cargo bench -p graphersal --bench graph --features io,script,display,serde
cargo bench -p graphersal --bench graph --features io,script,display,serde --no-run   # compile check only (the bench target requires these features)
cargo llvm-cov --package graphersal --features script,io,display,serde,persist,gql   # three-run recipe: book/src/for_developers/test_coverage.md; last measured numbers: same page
# robustness: the fast fuzz budget runs in every `cargo test` (tests/all/robustness_*); the LONG budget is a release gate
# (overflow checks kept in release; findings -> target/robustness-findings.txt; ROBUSTNESS_REPLAY=<seed> replays one case):
CARGO_PROFILE_RELEASE_OVERFLOW_CHECKS=true CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS=true ROBUSTNESS_CASES=500000 ROBUSTNESS_SEED=23 ROBUSTNESS_THREADS=7 \
  cargo test --release -p graphersal --features script,io,display,serde,persist,gql --test all robustness_fuzz_tests::fuzz_long -- --ignored
ROBUSTNESS_INPUT_CASES=20000 cargo test --release -p graphersal --features script,io,display,serde,persist,gql --test all robustness_input_tests::
# release gates too (fast budgets run in every `cargo test`); a failing case prints MERGE_FUZZ_REPLAY=<seed> / BACKUP_FUZZ_REPLAY=<seed>:
MERGE_FUZZ_CASES=60000 MERGE_FUZZ_SEED=1 MERGE_FUZZ_THREADS=8 cargo test --release -p graphersal --features script,io,display,serde,persist,gql,persist-zip --test all merge_checkpoint_fuzz_tests::
BACKUP_FUZZ_CASES=60000 BACKUP_FUZZ_SEED=1 BACKUP_FUZZ_THREADS=8 cargo test --release -p graphersal --features script,io,display,serde,persist,gql,persist-zip --test all backup_fuzz_tests::
# regenerated files (review the diff like code)
GRAPHERSAL_UPDATE_SURFACE=1 cargo test -p graphersal --features script,io,display,serde,persist,gql --lib surface   # tests/snapshots/script_surface.txt
GRAPHERSAL_UPDATE_BOOK=1 cargo test -p graphersal-session --test book_step_reference                           # generated step reference
```

The workspace includes `bindings/py-graphersal` (PyO3, `abi3-py310`): building it needs a Python >= 3.10 interpreter. If `python3` is older (the macOS system Python is 3.9), set `PYO3_PYTHON=/path/to/python3.12` for workspace-wide commands, or add `--exclude py-graphersal`.

Python binding tests (venv in the git-ignored `bindings/py-graphersal/.venv`; details in `bindings/py-graphersal/README.md`):
```bash
/opt/homebrew/bin/python3.12 -m venv bindings/py-graphersal/.venv && bindings/py-graphersal/.venv/bin/pip install maturin pytest   # one-time
(cd bindings/py-graphersal && VIRTUAL_ENV=$PWD/.venv .venv/bin/maturin develop && .venv/bin/python -m pytest tests/)
PYO3_PYTHON=$PWD/bindings/py-graphersal/.venv/bin/python cargo test -p py-graphersal
```

Feature hygiene check. Run it after touching `cfg(feature)` code or dependencies:
```bash
for f in "--no-default-features" "--features script" "--features display" "--features serde" "--features io" "--features persist" "--features persist-zstd" "--features persist-zip" "--features gql" "--all-features" "--no-default-features --features script,serde,display"; do
  cargo check --package graphersal $f || break; done
```

wasm32 (the browser playground must keep building). One-time: `rustup target add wasm32-unknown-unknown`, and for `graphersal-wasm` the pinned CLI `cargo install --locked wasm-bindgen-cli --version 0.2.129` (it must equal the `wasm-bindgen` crate version; `.cargo/config.toml` makes it the wasm32 test runner). `.cargo/config.toml` sets the `getrandom_backend="wasm_js"` cfg for that target:
```bash
for f in "--no-default-features" "--no-default-features --features script" \
         "--no-default-features --features script,serde,display" "--no-default-features --features io" \
         "--no-default-features --features persist-zstd" "--no-default-features --features persist-zip" \
         "--no-default-features --features gql" \
         "--no-default-features --features script,io,display,serde,persist,gql"; do
  cargo check --package graphersal --target wasm32-unknown-unknown ${=f} || break; done
```
Use `web_time::Instant`, never `std::time::Instant` (it panics on wasm32-unknown-unknown); do not add `std::thread`/`std::fs` use outside the `io` feature (on wasm32 `std::fs` returns errors; it must never panic); the exceptions are the parallel chunk decode of `persist` (`std::thread::scope`), `persist::FsDir` / `persist::SingleFileDir` (`std::fs`, `fd-lock`) and `vector::Model2Vec` (`std::fs`, feature `model2vec`), all `cfg(not(target_arch = "wasm32"))`; the Store itself never touches `std::fs` (it goes through `StoreDir`).

The playground engine and its tests:
```bash
cargo test  -p graphersal-session -p graphersal-wasm               # session logic, natively
cargo test  -p graphersal-wasm --target wasm32-unknown-unknown    # wasm-bindgen tests under Node.js
node --test playground/tests/*.test.mjs                           # playground query-tab store, /reset page script, save/restore files, schema editor model, graph tiers
playground/build.sh                                               # playground/pkg/ (wasm-release profile + wasm-opt if installed)
python3 playground/serve.py 8000                                  # static server with Cache-Control: no-store (python3 -m http.server lets browsers keep stale modules)
cargo run -p graphersal-cli -- --graph modern --server --port 8080   # the same UI over the dev server (no pkg/ needed)
```

Optional, not installed by default: `cargo semver-checks --package graphersal --baseline-rev main` (`cargo install --locked cargo-semver-checks`; from v0.1.0 on, 0.1.0 being published, a reported break is a release gate, see `book/src/for_developers/release_plan.md`) and `cargo deny check` against `deny.toml` (`cargo install --locked cargo-deny`; licences, RustSec advisories, duplicates, sources; becomes a release/CI gate).

### Observe queries with `graphersal` instead of guessing

Run a traversal to see what it actually returns. Don't reason about what it should return.
```bash
cargo build -p graphersal-cli                                   # rebuild after touching graphersal
target/debug/graphersal -e 'g.v().has_label("person").values("name").to_list()'
target/debug/graphersal -e 'g.v().has_label("person").count().profile()'   # optimized physical plan
echo 'g.v().values("age").max().next();' | target/debug/graphersal --in --graph modern
target/debug/graphersal -e 'let xs = g.v().to_list()' -e 'xs.len()'         # -e repeats and shares one scope
```
- `--graph` takes `modern` (the default), `empty`, `large` (about 110k elements), a Store directory/`.gstore` file, or a GraphSON (`.json`/`.jsonl`/`.graphson`; skipped data is a `note:` on stderr) or GraphML (`.xml`/`.graphml`) file. `--schema FILE [--schema-mode MODE]` sets that schema on the new empty graph first and imports the file against it as one unit (a sample graph gets it applied; a violation exits `1`; refused with a Store or `.gsnap`).
- `--format <fmt>` presets the visualizer.
- Display is bounded, data never: a displayed result shows at most 100 rows and 100 nested items. `--max-rows <N>`, `--max-items <N>`, `--no-limit` (REPL `/set ...`; one call: `#{max_rows: n}` / `#{no_limit: true}`). `to_list()`/`next()`/variables are never cut. See `book/src/users_guide/displaying_results.md`.
- stdout carries only the result; stderr only diagnostics and the truncation notice (exit code stays `0`). Exit codes: `0` ok, `1` script failed, `2` invalid invocation, `141` stdout closed early (`| head`).
- `.profile_with(ProfileType::Memory)` reports `MemorySource::Tracked` bytes in this binary; `g.memory_usage()` reports a graph's own footprint.

Use it to verify a new step or DSL binding end to end, confirm a pushdown with `.profile()`, copy real `help()` text into tests, and reproduce a user's query. The CLI does not replace tests: behaviour you rely on needs a test in `crates/graphersal/tests/`.

### TinkerPop conformance suite

`--test tinkerpop_features` runs the vendored TinkerPop 3.8.2 scenarios against the ratchet baseline `crates/graphersal/tests/tinkerpop/baseline.toml`; `target/tinkerpop-report.md` is the fix backlog. **Principle (user decision):** an unimplemented step is fine; an implemented step must follow the TinkerPop specification (the vendored feature files win over assumptions) unless a user decision says otherwise; error-message text is not chased. "Wrong behaviour of supported steps" must stay 0. Details: `crates/graphersal/tests/tinkerpop/CLAUDE.md`.

### openCypher TCK

`--test opencypher_tck` measures GQL against the vendored openCypher TCK 2024.3 (`crates/graphersal/tests/opencypher/`), with its own ratchet baseline. Same principle (W43, user decision): an unimplemented construct is fine; a supported one follows the TCK unless a user decision says otherwise; "wrong behaviour of supported constructs" is driven to 0. Licence wording: never call Graphersal's GQL "Cypher" or "openCypher" (the upstream Attribution Notice). Details: `crates/graphersal/tests/opencypher/CLAUDE.md`.

## Architecture (graphersal)

Each paragraph is a map; the nested file named holds the detail.

- **Storage** (`graph/CLAUDE.md`): `TraversalGraph` (petgraph `StableGraph` + property maps + indexes) behind `Graph<S = TraversalGraph>` (a lock newtype); the pipeline is generic over the EXPERIMENTAL `GraphStorage` trait (its rustdoc is the implementer's guide); front ends hold the sealed dyn boundary `Arc<dyn AnyGraph>`. Generational handles never alias a removed element. Vertices carry a label set (first = primary), edges one label. `insert_*` is the storing primitive, `add_*` creates like a query (schema defaults). A host's own storage runs under every generic front end (book `for_developers/custom_storage.md`; proof storage `Forwarding`).
- **Units and change capture** (`graph/CLAUDE.md`): every traversal, `set_schema`, catalog change, live import and `apply_change_set` is one unit (undo log, nested = savepoint, a failure leaves nothing); `Transactional::transaction`/`dry_run`. Commit hooks get a `ChangeSet` (built only while a hook is registered) and may veto. `StorageCapabilities`: `change_capture` requires `atomic`.
- **Statistics / memory** (`graph/CLAUDE.md`): `GraphStorage::statistics()` is the one place for data counts, kept exact incrementally; `memory_usage()` is an on-demand footprint.
- **Schema** (`graph/schema/CLAUDE.md`): one format everywhere (envelope + strict JSON Schema 2020-12 subset), modes `none`/`open`/`closed`, create-time defaults, write-time coercion, never read-time; seven source methods.
- **Catalog, compressed properties, property indexes** (`graph/CLAUDE.md`): named definitions beside the schema (saved queries, index and compression rules, opaque host kinds).
- **Pipeline** (`traversal/CLAUDE.md`): `Vec<GraphStep>`, one enum variant per category, one file per step; a rule-based optimizer (10 rules, `KNOWN_RULES`) then path analysis; bulk merging, sacks, Pop; `.profile()` shows the optimized plan; `execute()` never errs.
- **Values** (`value/CLAUDE.md`): `ElementProperty`/`TraverserValue`, temporal types, vectors and embedders; `value/physical.rs` is the only I/O codec.
- **Script DSL** (`script/CLAUDE.md`): Rhai bindings in both spellings from one registration, one lowering for `g` and `__`, documented per function in `register/docs.rs`, surface snapshot; saved queries; script limits.
- **Persistence** (`persist/CLAUDE.md`): streaming snapshots, WAL journal in its own hook slot, recovery, the Store over `StoreDir` (directory, single file, memory), verified backups, merged checkpoints, rollback/attic, damage/maintenance, versions and the `UPGRADES` table.
- **Resource limits** (`traversal/CLAUDE.md`, `script/CLAUDE.md`): `ScriptLimits` (SAFE default), administrative traversal options, cancellation via `CancelToken`, an opt-in memory budget. New limits go into `book/src/users_guide/resource_limits.md`.
- **Permissions** (`graph/CLAUDE.md`, `script/CLAUDE.md`): an `Authorizer` decides `Action` x `Resource` once per execution before the optimizer; everything is allowed by default; `AccessPolicy` is the ready-made policy.
- **Errors**: `error.rs` defines `TraverserError`, its sub-errors and their `help()` texts. `help()` wording tests live in `tests/all/error_diagnostics_tests.rs` when a query can trigger the error; named-field variants are `#[non_exhaustive]`, so a test building an error by hand lives in `error.rs`'s `#[cfg(test)]` module.

## Rules

1. **No panics in step or script paths.** Don't use `unwrap`/`expect`/`panic!`. Map every failure to a `TraverserError` variant. Arbitrary user queries must never crash the engine.
2. **Keep DSL parity.** Any change to a Rust step also updates its Rhai binding (builder method in `script/source/`, registration in `script/register/`), in both spellings, registered from one `register_fn_as` entry, with its doc entry in `script/register/docs.rs` (one sentence + an example checked by the test); update the surface snapshot (`GRAPHERSAL_UPDATE_SURFACE=1`).
3. **Mutating steps.** A mutating step is a `SideEffectMut` (or `drop()`); `GraphStep::for_each_access()` (`traversal/step/mod.rs`, the single step -> `AccessRequest` mapping) gives it its `Create`/`Update`/`Delete` request on `Data` (with the element kind and the label when the plan states them, `static_labels()`), and the authorization check, `GraphStep::mutates()` and `ScriptGraphTraversalSource::is_mutating()` derive from it. A mutating step in another category, or a new reading step that knows its element kind, needs an arm there. Add Rhai-level mutation tests in `tests/all/script_tests.rs` and a denial case in `tests/all/permission_tests.rs`.
4. **Evaluate lazily; don't allocate per item.** Steps must not allocate `HashMap`/`IndexMap`/`String` keys or clone properties for each traverser. Materialize (`into_node_property`) only at terminal steps. Steps propagate `bulk` (`Traverser::split`/`with_parts`), counters add it, and materializing steps expand it through `bulk::expand`. `count`, `limit`, `has`, and `profile` stay lazy.
5. **Keep optimizer and profile in sync.** For every new source or expansion step, check whether it can take pushdown parameters. Keep `.profile()` accurate for fused steps.
6. **Update `help()` for new steps.** When a step is added or gains capabilities, go through every `help()` in `error.rs`. Name the step wherever it can now fix the error, with a runnable example built from the failing query. Keep the existing advice. Assert each changed message in `tests/all/error_diagnostics_tests.rs`, or in the test module of `error.rs` when the error has to be built by hand. Spelling follows TinkerPop: the coercion steps are `as_string()`, `as_bool()`, and `as_number(GType::LONG)`. `GType` is the type-token enum. `T` means only `T.id`/`T.label`, never a type.
7. **Don't infer types from content.** A string stays `string`, even when it looks like a date, UUID, or IP. Logical types come only from an explicit schema declaration or a per-value type tag (e.g. `{"@type": "g:UUID", ...}`). Never add a content-sniffing detector, because an ambiguous parse like `"03/02/2024"` corrupts data without raising an error.
8. **Keep the public API small.** The crate root holds only the modules and the domain types `TraversalGraph`, `GraphTraversalSource`, `GraphSource`, `TraverserValue`, `ElementProperty`, `TraverserError` (+ `CompactString`). NO public glob re-exports, and no third-party type in the API except `CompactString`. Query writers use `prelude` (graph, sources, `GraphStorage` for the graph's methods, values, tokens, `GraphSchema`/`SchemaMode`, `Execution`, errors); integrators use the facade modules (`src/<name>.rs`, `pub use` lists with module docs naming their audience, each public item in exactly one of them besides the prelude): `storage` (`GraphStorage` and EVERY type in its signatures; its doctest implements a storage with only `storage::*` + `prelude::*`), `changes`, `auth`, `catalog`, `schema`, `profile`, `exec`, `error`, `temporal`, `vector`, plus `script`/`io`/`persist`/`alloc_tracking`; a new public type goes into the one that fits. `graph`, `traversal` and `value` are `pub(crate)`: crate code reaches their items through `pub(crate)` globs in `lib.rs` (`use crate::*;`), the public documentation of their topics lives in the facade module docs. Step internals, buffers, AST converters and helper types are `pub(crate)`; a helper type that a public signature needs (`ArgStrings`, `IntoSelectArgs`, `TraverserPath`, `TextRegex`) stays `pub` inside a private module, so it cannot be named outside the crate. A storage outside the crate builds the `#[non_exhaustive]` `GraphError` variants it returns through constructors (`GraphError::unsupported`, `element_removed`, `duplicate_vertex_id`, `invalid_property_value`, `invalid_property_path`, ...), and runs the contract's write checks through the public helpers in `storage` (`graph/write_checks.rs`: `check_property_value`/`check_properties`, `check_label`/`normalize_vertex_labels`, the jpath helpers `path_property_name`/`write_path(_in)`/`remove_path(_in)`; `TraversalGraph` and the trait defaults call the same functions; `JsonPathSegment` and `JsonPath::segments()` are public).
9. **Library code logs through `tracing` macros.** Never `println!`/`eprintln!`/`dbg!`.
10. **Dependencies.** Add a new crate only with justification. Declare it in the root `[workspace.dependencies]` and reference it with `x.workspace = true`.
11. **Tests and docs.** New public features get integration tests in `crates/graphersal/tests/`. Every public item has a `///` doc comment (`#![warn(missing_docs)]` in `graphersal`'s `lib.rs`, denied by `-D warnings`; `graphersal-session` denies it).
12. **English** in every on-disk artifact: code, comments, and docs. Chat may be in any language.
13. **Steps that read `Traverser.path` must override `path_requirement()`**, and steps that hold child traversals must override `analyze_nested_paths()`. Otherwise the path analysis does not record what they read. Verify the result with `.profile()`, which shows `[path: ...]` on each step that records something.
14. **`Debug` is never a data converter.** `{:?}` is only for `Debug` impls, `tracing`, error or diagnostic text, and REPL rendering of non-graph Rhai values. Any text written to files, visualizer output or script values comes from `value/physical.rs` (`lower`, `write_json`, `to_json_text`).
15. **Additive changes must not break downstream code.** Every public enum is `#[non_exhaustive]` unless its variant set is closed by definition (`Column`, `ElementKind`, `GraphNodeType`); then its doc comment says why. Named-field error variants are `#[non_exhaustive]` as well, so a field can be added later. Public result/config structs with `pub` fields are `#[non_exhaustive]` (`MemoryStats`, the profiling structs, `RenderOptions`, `RenderedOutput`, `Truncation`, `StepFrame`); if callers must create one, give it a constructor or builder. A new `GraphStorage` trait method always has a default implementation. The payload of a new value type's variant is our own type (like `ElementProperty::Uuid(u128)`), never a third-party type such as `chrono::NaiveDate`; conversions to third-party types are `From` impls behind a feature. Inside `graphersal`, `match`es on these enums stay exhaustive, with no `_ =>` arm, so a new variant forces every internal site to be updated. Other workspace crates and `crates/graphersal/tests/` are downstream: they use `_` arms and `..` patterns.
16. **Keep the TinkerPop baseline current.** When a step is added or fixed, run the TinkerPop suite (`--test tinkerpop_features`) and remove the scenarios that now pass from `crates/graphersal/tests/tinkerpop/baseline.toml` in the same change (or regenerate it with `TINKERPOP_UPDATE_BASELINE=1` and review the diff like code). The suite fails on stale entries anyway; the point is that progress is recorded with the change that made it, and a scenario that passes once is protected from regressing from then on.
17. **One integration-test binary.** Every `graphersal` integration test lives in `crates/graphersal/tests/all/<name>.rs`, registered as `mod <name>;` in `tests/all/main.rs` (a feature-gated file keeps its `#![cfg(feature = ..)]` at its own top). Never add a new top-level `tests/*.rs` target: each one is another link and another first-run scan (the full run took 7+ minutes with ~60 binaries). The only exceptions are `tinkerpop_features.rs` and `opencypher_tck.rs` (own process: parallel lanes, panic hook, per-scenario timeouts, report). Name files `<topic>_tests.rs`, run a module with `--test all <name>::` (trailing `::`, so a prefix does not match other modules). Keep process-global state (`set_var`, `#[global_allocator]`, a global tracing subscriber, panic hooks, current directory) out of `tests/all/`: a test that needs it becomes a second separate target, listed as an exception with its reason in the `main.rs` header.
18. **Document every deliberate deviation from TinkerPop in three places:** the `///` of the item, the dedicated mdBook page of the feature, and `book/src/for_developers/tinkerpop_deviations.md`. A deviation comes from a user decision, never from convenience.
19. **Keep the compliance page in sync.** Never hand-edit a generated block of `tinkerpop_compliance.md`; regenerate with `TINKERPOP_UPDATE_BASELINE=1` and commit the page together with the baseline when a step moves the number (the page has no hand-written history: the git log is the record) (rule 16 covers the page too).
20. **New traverser constructors say whether the traverser is generated or derived.** Generated (a start step or reducing barrier): `Traverser::start(context, v)`, initial sack. Derived: `split`/`with_parts(.., source.sack)`, keeps the parent's sack and bulk. `size_of::<Traverser>()` stays 80.
21. **No runtime rule struct in the merge hot path.** Never put a runtime rule or option struct into `MergeKey::of` or the merge loop; use const generics as `merge_inner::<S, OPERATOR, ONE>` does (a runtime struct measured +25 % there). When touching `bulk.rs` or `Traverser`, check `cargo bench -p graphersal --bench graph --features io,script,display,serde` (no-sack paths within 5 %).
22. **Creation parameters have one documentation place.** A store parameter that is set when the store is created and never changes (today: the store id, the damage policy, the chunk size, the creation time, the backend and its container version, the format version) goes into the table of `book/src/persistence/creation_parameters.md` (default, how to set it in Rust/CLI/Python/dev server, where it is stored with its format spec section, what copies carry it) and into `graphersal store info`'s "fixed at creation" block and `StoreInfo`; it is never changed by an open (`StoreOptions` setters for it apply to create only), and every copy path (fork, attic fork, repair, convert, backup, restore) carries it, with a test that tries every path. Per-open options are listed separately on the same page.
23. **Generated files are regenerated, never hand-edited:** the surface snapshot (`GRAPHERSAL_UPDATE_SURFACE=1`), the book step reference and upgrade table (`GRAPHERSAL_UPDATE_BOOK=1`), the TinkerPop baseline and compliance blocks (`TINKERPOP_UPDATE_BASELINE=1`), the openCypher TCK baseline `tests/opencypher/baseline.toml` and the generated blocks of `book/src/gql/tck_compliance.md` (`OPENCYPHER_UPDATE_BASELINE=1`).
