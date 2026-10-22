# Vendored openCypher TCK

This directory vendors the Technology Compatibility Kit (TCK) of the openCypher project so that
Graphersal's GQL query language can be measured against the openCypher TCK offline, reproducibly,
against a pinned upstream version. The TCK scenarios exercise the Cypher compatibility forms that
Graphersal's GQL accepts (`CREATE`, `MATCH ... RETURN`, `WITH`, `UNWIND`, ...); Graphersal's GQL is
not a Cypher implementation and is not openCypher (see "License" below for the wording rule).

## Pinned version

- **Tag:** `2024.3` (GitHub release "2024.3")
- **Commit:** `677cbafabb8c3c5eed458fd3b1ec0daec8d67d23`
- **Tag date:** 2026-03-20
- **Source:** https://github.com/opencypher/openCypher, directories `tck/features/` and
  `tck/graphs/`

At vendoring time (2026-10-20) the default branch `main` pointed at the same commit: no newer TCK
material existed outside the tag.

## Contents

- `features/` — the full TCK feature tree, copied verbatim from `tck/features/` (220 `.feature`
  files in `clauses/`, `expressions/` and `useCases/`; 1615 scenarios, 3897 test cases after
  `Scenario Outline` expansion). Each file carries its own Apache-2.0 header and the openCypher
  Attribution Notice. Nine files (`clauses/return-orderby/`, `clauses/return-skip-limit/`) use CRLF
  line endings upstream; the repository `.gitattributes` disables line-ending normalization for
  this tree so every file stays byte-identical to upstream.
- `graphs/` — the TCK named graphs, copied verbatim from `tck/graphs/`: `binary-tree-1/` and
  `binary-tree-2/` (each a `.cypher` creation script and a `.json` metadata file) and
  `named-graphs.adoc` (the upstream description of the metadata format). The scenarios
  `Given the binary-tree-1 graph` / `Given the binary-tree-2 graph` (`useCases/triadicSelection`)
  load them.
- `LICENSE` — the upstream Apache-2.0 license, copied verbatim.
- `NOTICE` — attribution for the vendored files, scoped to this directory: a short header naming
  the tag, commit and copied paths, followed by the upstream `NOTICE` verbatim (the repository
  root `NOTICE` repeats it for the project as a whole).
- `harness/`, `baseline.toml` — the runner's harness modules and the ratchet baseline (ours, not
  vendored); see "Runner, baseline and report" below.

## License

Apache-2.0, same as this project. This directory contains files copied from the openCypher project
(https://github.com/opencypher/openCypher), Copyright (c) "Neo4j" Neo4j Sweden AB
[https://neo4j.com], licensed under the Apache License, Version 2.0. See `LICENSE` and `NOTICE` in
this directory.

The upstream `NOTICE` carries an **Attribution Notice**: a work not approved by the openCypher
Implementers Group should not be described as "Cypher" (Cypher® is a registered trademark of
Neo4j Inc.) or as "openCypher". Wording rule for everything Graphersal writes (code comments,
README, book pages, reports, release notes): say that Graphersal's GQL is *measured against the
openCypher TCK* and accepts *Cypher compatibility forms*; never call it "a Cypher implementation",
"Cypher" or "openCypher". The upstream terms for extensions are "implementation extensions to
Cypher" and "proposed changes to Cypher that are not yet approved by the openCypher community".

## Upgrade procedure

1. Pick the new release tag (the latest on https://github.com/opencypher/openCypher/releases;
   check whether newer TCK material lives on the default branch only, and prefer a tag) and
   shallow-clone it:
   `git clone --depth 1 --branch <tag> https://github.com/opencypher/openCypher.git /tmp/opencypher-<tag>`
   and check `git -C /tmp/opencypher-<tag> rev-parse HEAD` against the release's commit. Copy data
   files only; never run anything from the clone (its build files are not needed).
2. Re-copy `tck/features/` to `features/` and `tck/graphs/` to `graphs/`, replacing this
   directory's contents (`LICENSE` too, and the upstream part of `NOTICE`, in case they changed).
   `diff -r` the copies against the clone: they must be identical (line endings included).
3. Record the new tag, commit SHA and tag date in this file, in the header of `NOTICE` and in the
   openCypher section of the repository root `NOTICE`.
4. Run the TCK target, regenerate the baseline and the compliance page blocks with
   `OPENCYPHER_UPDATE_BASELINE=1` (see below), and review the diff like code: scenarios that newly
   fail are regressions to investigate before merging the upgrade; scenarios that newly pass leave
   the baseline in the same change. Update the scenario counts in this file.

## Runner, baseline and report

The runner is the separate integration test
target `crates/graphersal/tests/opencypher_tck.rs` (own process: parallel lanes, a panic hook,
per-scenario timeouts and a report, the same reasons as `tinkerpop_features.rs`), gated on the
`gql` feature only: it calls the Rust GQL API directly, not the Rhai DSL. Run it with

```bash
cargo test --package graphersal --features script,io,display,serde,persist,gql --test opencypher_tck
```

It is also part of the plain `cargo test --package graphersal --features script,io,display,serde,persist,gql`.

- **Execution.** Every test case (a scenario, or one `Examples` row of a `Scenario Outline`) runs on
  a fresh graph: the `having executed` setup queries and the named graphs run through Graphersal's
  own GQL `CREATE` / `INSERT` (no second loader), then the `When executing query` query runs with
  its parameters, and the result, the side effects (a state diff) and expected errors (type and
  phase only, never the detail text) are compared.
- **Classes.** `pass`; **Unsupported**: `missing` (a refused or "not yet" construct), `setup` (a
  setup query that does not run), `excluded` (out of scope: an explicit reviewed table of scenario
  ids, never tags or content); **Wrong behaviour of supported constructs**: `wrong_result`,
  `wrong_columns`, `wrong_side_effects`, `wrong_error`, `unexpected_error`, `timeout`, `panic`.
- **Ratchet baseline.** `baseline.toml` lists every non-passing test case with its class and reason.
  The test fails when a case outside the baseline fails (a regression) and when a baseline entry
  passes now (a stale entry: remove it in the same change). Regenerate it, together with the
  generated blocks of the compliance page `book/src/gql/tck_compliance.md`, and review the diff
  like code:

  ```bash
  OPENCYPHER_UPDATE_BASELINE=1 cargo test --package graphersal --features script,io,display,serde,persist,gql --test opencypher_tck
  ```
- **Scope.** `harness/scope.rs` lists the excluded case ids by reason (the
  upstream `@ignore`, deliberate deviations decided by the user); the compliance page shows the
  table with the reasons.
- **Report.** Every run writes `target/opencypher-report.md` (headline Unsupported vs Wrong
  behaviour, per-directory table, exclusions, the full wrong-behaviour list, ranked missing
  constructs, setup failures by reason).
