# Contributing to Graphersal

Thanks for your interest. Graphersal is an in-memory graph database and traversal engine in Rust, queried with
Gremlin. The detailed rules and the architecture notes live in `CLAUDE.md` (written for coding agents, readable by
humans); this file is the short human version.

## Before you start

- Bugs and wrong query results: open an issue with the query, the graph and what you expected. The easiest way is a
  playground link: open the query in the [playground](https://play.graphersal.dev) and press the green **Share link**
  button in the top bar; the link carries the query and the graph, and whoever opens it gets both.
  **Only small graphs fit into a link** (at most 60,000 characters, roughly a thousand elements): reduce the graph
  to the few vertices and edges that reproduce the problem first. That is the most useful bug report anyway. For a
  larger graph, attach its GraphSON file (Save ▸ Data) instead.
- A larger change (a new step, a new feature, a format change): open an issue first, so we can agree on the design
  before you write it. Planned work is tracked in the GitHub issues.
- Security issues: never in public, see [SECURITY.md](SECURITY.md).

## Build and test

Rust 1.88 or newer. The workspace also contains the PyO3 binding, which needs a Python 3.10+ interpreter at build time
(set `PYO3_PYTHON` if your default `python3` is older, or add `--exclude py-graphersal`).

```bash
cargo fmt --all --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test  --package graphersal --features script,io,display,serde,persist,gql   # includes the TinkerPop suite
cargo test  --package graphersal --doc --features script,io
cargo test  --package graphersal-cli
cargo test  --package graphersal-storage-tests
cargo deny check                                                          # cargo install --locked cargo-deny
```

CI does not run on its own: a maintainer starts it for a pull request. Run the checks above locally before you open
one, and the ones below for what you touched:

```bash
cargo test -p graphersal-session -p graphersal-wasm          # playground session logic (also the book's examples)
node --test playground/tests/*.test.mjs                       # playground UI models
mdbook build book                                             # the book
(cd bindings/py-graphersal && VIRTUAL_ENV=$PWD/.venv .venv/bin/maturin develop && .venv/bin/python -m pytest tests/)
# the Python tests need a one-time venv with maturin and pytest: bindings/py-graphersal/README.md
```

Some files are generated and checked by tests; regenerate them instead of editing by hand and review the diff:

| What | Regenerate with |
|---|---|
| the DSL surface snapshot (`tests/snapshots/script_surface.txt`) | `GRAPHERSAL_UPDATE_SURFACE=1 cargo test -p graphersal --features script,io,display,serde,persist,gql --lib surface` |
| the step reference pages of the book | `GRAPHERSAL_UPDATE_BOOK=1 cargo test -p graphersal-session --test book_step_reference` |
| the TinkerPop baseline and compliance page | `TINKERPOP_UPDATE_BASELINE=1 cargo test -p graphersal --features script,io,display,serde,persist,gql --test tinkerpop_features` |

Feature hygiene (after touching `cfg(feature)` code or dependencies): `cargo check --package graphersal` for each of
`--no-default-features`, `--features script`, `--features display`, `--features serde`, `--features io`,
`--features persist`, `--features persist-zstd`, `--features persist-zip`, `--all-features`, and
`--no-default-features --features script,serde,display`. The library must also build for
`wasm32-unknown-unknown` (`rustup target add wasm32-unknown-unknown`, then the same `cargo check` with `--target`).

Try a query: `cargo build -p graphersal-cli && target/debug/graphersal -e 'g.v().has_label("person").values("name").to_list()'`.

## Robustness (fuzz) tests

Arbitrary queries and inputs must never crash the engine: every failure is an error, never a panic, a hang or
unbounded memory. A seeded fuzzer checks this:

- The fast budget (`crates/graphersal/tests/all/robustness_*`) runs with every `cargo test` above; a pull request must
  keep it green.
- If your change touches steps, the Rhai bindings, value conversion, parsing (jpath, GraphSON, GraphML, schemas, change sets) or
  limits, also run the long budget locally before opening the pull request (about 10-15 minutes on a laptop):

```bash
CARGO_PROFILE_RELEASE_OVERFLOW_CHECKS=true CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS=true \
ROBUSTNESS_CASES=500000 ROBUSTNESS_SEED=23 ROBUSTNESS_THREADS=4 \
  cargo test --release -p graphersal --features script,io,display,serde,persist,gql \
  --test all robustness_fuzz_tests::fuzz_long -- --ignored
ROBUSTNESS_INPUT_CASES=20000 cargo test --release -p graphersal \
  --features script,io,display,serde,persist,gql --test all robustness_input_tests::
```

  Findings are written to `target/robustness-findings.txt`; `ROBUSTNESS_REPLAY=<seed>` replays one case. A new
  finding caused by your change must be fixed in the same pull request. A pre-existing one goes into
  `crates/graphersal/tests/all/robustness_bug_tests.rs` as an `#[ignore = "BUG: ..."]` test with the minimized
  repro, mentioned in the pull request.
- A fix for a known robustness bug un-ignores its test there.
- If your change touches persistence (`crates/graphersal/src/persist/`), run the two randomized persistence tests
  with their long budgets as well:

```bash
MERGE_FUZZ_CASES=60000 MERGE_FUZZ_SEED=1 MERGE_FUZZ_THREADS=8 cargo test --release -p graphersal \
  --features script,io,display,serde,persist,gql,persist-zip --test all merge_checkpoint_fuzz_tests::
BACKUP_FUZZ_CASES=60000 BACKUP_FUZZ_SEED=1 BACKUP_FUZZ_THREADS=8 cargo test --release -p graphersal \
  --features script,io,display,serde,persist,gql,persist-zip --test all backup_fuzz_tests::
```

  A failing case prints `MERGE_FUZZ_REPLAY=<seed>` or `BACKUP_FUZZ_REPLAY=<seed>` to replay it.

## The TinkerPop suite

`cargo test --package graphersal --features script,io,display,serde,persist,gql --test tinkerpop_features` runs the vendored
Apache TinkerPop 3.8.2 scenarios through the Rhai DSL against a ratchet baseline
(`crates/graphersal/tests/tinkerpop/baseline.toml`) and writes `target/tinkerpop-report.md`. When a step is added or
fixed, remove the scenarios that now pass from the baseline (or regenerate it with `TINKERPOP_UPDATE_BASELINE=1` and
review the diff) in the same change. See `crates/graphersal/tests/tinkerpop/README.md`.

## Code rules (summary)

- No `unwrap`/`expect`/`panic!` in step or script paths; map failures to a `TraverserError`.
- A change to a Rust step also updates its Rhai binding, in both spellings.
- Steps are lazy: no per-item allocation, materialize only at terminals.
- Public API stays small; public enums and result structs are `#[non_exhaustive]` unless closed by definition.
- Every public item has a `///` comment; new public features get integration tests in `crates/graphersal/tests/all/`
  (one test binary; add a module, never a new top-level test file).
- Library code logs through `tracing`, never `println!`.
- Do not infer types from string content.
- Documentation, comments and commit messages are in English.

## Contributor License Agreement (CLA)

Graphersal is licensed under Apache-2.0. Contributions are accepted under the
[Contributor License Agreement](CLA.md): you keep the copyright in your contribution and grant the maintainer a
licence to use, distribute and sublicense it, including under licence terms other than Apache-2.0, plus a patent
licence for it. This keeps the project able to change the licence of future versions; a version already published
under Apache-2.0 stays available under it.

You sign once. When you open your first pull request, the CLA check posts a comment with a link to the agreement;
reply with the comment

```text
I have read the CLA Document and I hereby sign the CLA
```

and the check turns green for this and every later pull request (comment `recheck` to run it again). If you
contribute on behalf of your employer, make sure you are allowed to accept the agreement for it (section 1 of
[CLA.md](CLA.md)). A pull request is merged only when every author of its commits has signed.

Contributions to a future Graphersal server, which will live in its own repository, may follow a different
process; that repository will say so.
