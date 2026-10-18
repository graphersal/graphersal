# Test Coverage

Coverage is measured with [`cargo-llvm-cov`](https://github.com/taiki-e/cargo-llvm-cov) (source-based LLVM coverage on the
stable toolchain; install the tool and `llvm-tools-preview` once). Coverage is an analysis aid, not an acceptance gate: it shows
thinly tested places, it does not prove behaviour.

## Running it

Three runs separate what the hand-written tests cover from what only the TinkerPop suite reaches. Clear the old profile between
runs, otherwise the runs are merged:

```bash
F=script,io,display,serde,persist,gql
IGN='(tests/|benches/)'

# 1. everything: lib unit tests + tests/all + the TinkerPop suite
rm -f target/llvm-cov-target/*.profraw target/llvm-cov-target/*.profdata
cargo llvm-cov --no-report --package graphersal --features $F

# 2. hand-written tests only (no TinkerPop suite)
rm -f target/llvm-cov-target/*.profraw target/llvm-cov-target/*.profdata
cargo llvm-cov --no-report --package graphersal --features $F --lib --test all

# 3. TinkerPop suite only
rm -f target/llvm-cov-target/*.profraw target/llvm-cov-target/*.profdata
cargo llvm-cov --no-report --package graphersal --features $F --test tinkerpop_features
```

Right after a run, turn its profile into a report:

```bash
cargo llvm-cov report --package graphersal --ignore-filename-regex "$IGN"                 # table on stdout
cargo llvm-cov report --package graphersal --ignore-filename-regex "$IGN" --json --summary-only > summary.json
cargo llvm-cov report --package graphersal --ignore-filename-regex "$IGN" --html          # target/llvm-cov/html/index.html
```

`--ignore-filename-regex '(tests/|benches/)'` removes the tests, the TinkerPop harness and the benches from the percentages, so only
`crates/graphersal/src/` is measured. Never commit `target/`. The single command from `CLAUDE.md`
(`cargo llvm-cov --package graphersal --features script,io,display,serde,persist,gql`) is run 1 with a text report.

## Reading the report

- **Lines %** is the number to rank by. **Functions** count monomorphized instances (a generic step instantiated for three storages
  counts three times). **Regions** are finer than lines (branches, `?` early returns, match arms).
- A file with a `#[cfg(test)]` module counts those test lines as covered code, so `error.rs` looks a little better than it is.
- **Doc tests are not measured**: `--doctests` needs a nightly toolchain.
- Compare the runs per file: **covered by run 1 but not run 2** means only the TinkerPop suite protects that code (fragile; add a
  hand-written test). **Missed by run 1** means nothing tests it.
- In the HTML report, red lines are uncovered; open a file and look for whole `process`/error branches that are red.
  Fused or batch steps often have an unexercised per-traverser fallback; run the query with the optimizer disabled
  (`g.with("optimizer.disabled", [...])`) to reach it.

## Last measured numbers

Measured with the three-run recipe above, with the features `script,io,display,serde` (before `persist` joined the
recipe, so the persistence code is not in these figures):

| Run | Lines | Functions | Regions |
|---|---|---|---|
| Everything | 89.50 % | 89.14 % | 89.63 % |
| Hand-written only | 89.22 % | 89.03 % | 89.37 % |
| TinkerPop only | 32.82 % | 29.26 % | 29.87 % |

The TinkerPop suite adds only 0.3 points on top of the hand-written tests, so little code depends on it alone. The
weakest hand-written areas at that point were `traversal/traverser.rs` (72 %, the string readers of edge-property
handles), the `from()`/`to()` modulators (72 %, `Debug`/`Clone` only), `has(key, P)` (75 %), `step_by.rs` (75 %),
`traversal/step/mod.rs` (75 %, trait default methods every step overrides) and `value/mod.rs` (76 %, the per-shape
`fetch_*` arms).

These figures are a snapshot, not a gate; re-run the recipe for a current report.
