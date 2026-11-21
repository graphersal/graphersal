//! Runs the vendored Apache TinkerPop 3.8.2 Gherkin feature suite (`tests/tinkerpop/features/`)
//! against Graphersal's Rhai DSL.
//!
//! The harness lives under `tests/tinkerpop/harness/`; its unit tests run as part of this target.
#![cfg(all(feature = "script", feature = "io", feature = "serde"))]

#[path = "tinkerpop/harness/mod.rs"]
mod harness;

// The proof storage of `GRAPHERSAL_TEST_STORAGE=forwarding` (`harness::storage`), included by
// path like `tests/all/foreign_storage_tests.rs` does: a dev-dependency on
// `graphersal-storage-tests` would link a second copy of `graphersal` into this binary.
#[allow(dead_code)]
#[path = "../../graphersal-storage-tests/src/forwarding.rs"]
mod forwarding;

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use harness::classify::{self, Class, Outcome};
use harness::storage::TestStorage;
use harness::translate::{self, Translation};
use harness::{baseline, gherkin, inventory, notation, page, report, scope};

/// Root of the vendored `.feature` files.
fn features_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/tinkerpop/features")
}

/// Counts `.feature` files and non-commented `Scenario:` lines independently of the parser.
fn count_files_and_scenario_lines(dir: &Path, files: &mut usize, scenarios: &mut usize) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.unwrap_or_else(|e| panic!("read entry: {e}")).path();
        if path.is_dir() {
            count_files_and_scenario_lines(&path, files, scenarios);
        } else if path.extension().is_some_and(|ext| ext == "feature") {
            *files += 1;
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            *scenarios += text
                .lines()
                .filter(|l| l.trim_start().starts_with("Scenario:"))
                .count();
        }
    }
}

#[test]
fn all_feature_files_parse() {
    let root = features_root();
    let features = gherkin::load_all(&root).unwrap_or_else(|e| panic!("parse error: {e}"));

    let (mut files, mut scenario_lines) = (0, 0);
    count_files_and_scenario_lines(&root, &mut files, &mut scenario_lines);
    assert_eq!(files, 159, "unexpected number of vendored feature files");
    assert_eq!(features.len(), files);

    let scenarios: Vec<&gherkin::Scenario> =
        features.iter().flat_map(|f| f.scenarios.iter()).collect();
    assert_eq!(
        scenarios.len(),
        scenario_lines,
        "parsed scenarios != `Scenario:` lines"
    );

    let mut ids = HashSet::new();
    for s in &scenarios {
        assert!(ids.insert(s.id.as_str()), "duplicate scenario id {}", s.id);
    }

    eprintln!(
        "tinkerpop features: {} files, {} scenarios",
        features.len(),
        scenarios.len()
    );
}

#[test]
fn all_notation_parses() {
    let features =
        gherkin::load_all(&features_root()).unwrap_or_else(|e| panic!("parse error: {e}"));

    let mut parsed = 0usize;
    let mut failures: Vec<(String, String, String)> = Vec::new();
    for scenario in features.iter().flat_map(|f| f.scenarios.iter()) {
        let mut check =
            |text: &str, result: Result<notation::Expected, notation::NotationError>| match result {
                Ok(_) => parsed += 1,
                Err(e) => failures.push((scenario.id.clone(), text.to_string(), e.to_string())),
            };
        for assertion in &scenario.assertions {
            let cells: Vec<&String> = match assertion {
                gherkin::Assertion::Ordered(cells)
                | gherkin::Assertion::Unordered(cells)
                | gherkin::Assertion::Of(cells) => cells.iter().collect(),
                gherkin::Assertion::Subgraph { edges, vertices } => {
                    edges.iter().chain(vertices.iter()).collect()
                }
                _ => Vec::new(),
            };
            for cell in cells {
                check(cell, notation::parse_cell(cell));
            }
        }
        for (_, value) in scenario.params.iter().chain(scenario.side_effects.iter()) {
            check(value, notation::parse(value));
        }
    }

    if !failures.is_empty() {
        let listing: Vec<String> = failures
            .iter()
            .take(20)
            .map(|(id, text, err)| format!("  {id}: `{text}`: {err}"))
            .collect();
        panic!(
            "{} notation values failed to parse (first 20):\n{}",
            failures.len(),
            listing.join("\n")
        );
    }
    eprintln!("tinkerpop notation: {parsed} values parsed");
}

#[test]
fn all_traversals_translate() {
    let features =
        gherkin::load_all(&features_root()).unwrap_or_else(|e| panic!("parse error: {e}"));
    // Syntax check only. The DSL engine keeps Rhai's default expression-depth limit, which rejects
    // the long `addV(..).addE(..)...` initializer chains of some datasets ("Expression exceeds
    // maximum complexity"); that is an engine limit, not a translation error, so it is lifted
    // here. Execution (step 4) runs on the unmodified engine and reports it as an engine failure.
    let mut engine = graphersal::script::engine_with_limits(
        &graphersal::script::ScriptLimits::default(),
        std::sync::Arc::new(graphersal::auth::AllowAll),
    );
    engine.set_max_expr_depths(0, 0);

    let (mut total, mut translated) = (0usize, 0usize);
    let mut unsupported: BTreeMap<String, usize> = BTreeMap::new();
    let mut compile_errors: Vec<(String, String, String)> = Vec::new();
    for scenario in features.iter().flat_map(|f| f.scenarios.iter()) {
        let params: Vec<String> = scenario.params.iter().map(|(n, _)| n.clone()).collect();
        let side_effects: Vec<String> = scenario
            .side_effects
            .iter()
            .map(|(n, _)| n.clone())
            .collect();

        let mut texts: Vec<(&str, &[String])> = Vec::new();
        if let Some(t) = &scenario.traversal {
            texts.push((t, &side_effects));
        }
        if let Some(init) = &scenario.initializer {
            texts.push((init, &[]));
        }
        for assertion in &scenario.assertions {
            if let gherkin::Assertion::GraphCount { traversal, .. } = assertion {
                texts.push((traversal, &[]));
            }
        }

        for (text, side_effects) in texts {
            total += 1;
            match translate::translate(text, &params, side_effects) {
                Translation::Ok { script, .. } => {
                    translated += 1;
                    if let Err(e) = engine.compile(&script) {
                        compile_errors.push((scenario.id.clone(), script, e.to_string()));
                    }
                }
                Translation::Unsupported { reason } => {
                    *unsupported.entry(reason).or_default() += 1;
                }
            }
        }
    }

    let unsupported_total: usize = unsupported.values().sum();
    eprintln!(
        "tinkerpop translation: {total} texts, {translated} translated, {unsupported_total} unsupported, {} compile errors",
        compile_errors.len()
    );
    let mut by_count: Vec<(&String, &usize)> = unsupported.iter().collect();
    by_count.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    for (reason, count) in by_count {
        eprintln!("  unsupported {count:>4}  {reason}");
    }
    for (id, script, err) in &compile_errors {
        eprintln!("  compile error in {id}: {err}\n    {script}");
    }
    assert!(
        compile_errors.is_empty(),
        "{} translated scripts do not compile",
        compile_errors.len()
    );
}

/// Number of scenario lanes run in parallel (each lane owns one worker thread).
/// The deliberate-incompatibility table is consistent: ids exist, are unique, are not also
/// strategy-excluded, and every doc page exists.
#[test]
fn deliberate_table_is_consistent() {
    let features =
        gherkin::load_all(&features_root()).unwrap_or_else(|e| panic!("parse error: {e}"));
    let known: HashSet<&str> = features
        .iter()
        .flat_map(|f| f.scenarios.iter())
        .map(|s| s.id.as_str())
        .collect();
    let by_id: BTreeMap<&str, &gherkin::Scenario> = features
        .iter()
        .flat_map(|f| f.scenarios.iter())
        .map(|s| (s.id.as_str(), s))
        .collect();
    let book = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../book/src");
    let mut problems: Vec<String> = Vec::new();
    let mut codes: HashSet<&str> = HashSet::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for inc in scope::INCOMPATIBILITIES {
        if !codes.insert(inc.code) {
            problems.push(format!("duplicate code {}", inc.code));
        }
        if inc.ids.is_empty() || inc.why.is_empty() || inc.title.is_empty() {
            problems.push(format!("{}: empty ids, title or why", inc.code));
        }
        let page = inc.doc.split('#').next().unwrap_or(inc.doc);
        if !book.join(page).is_file() {
            problems.push(format!("{}: doc page {page} does not exist", inc.code));
        }
        for id in inc.ids {
            if !known.contains(id) {
                problems.push(format!("{}: no such scenario {id}", inc.code));
            }
            if !seen.insert(id) {
                problems.push(format!("{}: id listed twice {id}", inc.code));
            }
            if by_id.get(id).is_some_and(|s| scope::exclusion(s).is_some()) {
                problems.push(format!(
                    "{}: {id} is excluded by a strategy already",
                    inc.code
                ));
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// A deliberately incompatible scenario that starts to pass is a stale exclusion: the deviation is
/// gone and the scenario belongs back in the scope.
#[test]
fn deliberate_scenarios_do_not_pass_silently() {
    let features =
        gherkin::load_all(&features_root()).unwrap_or_else(|e| panic!("parse error: {e}"));
    let listed: Vec<&gherkin::Scenario> = features
        .iter()
        .flat_map(|f| f.scenarios.iter())
        .filter(|s| scope::deliberate(&s.id).is_some() && scope::exclusion(s).is_none())
        .collect();
    assert!(!listed.is_empty());
    let passing: Vec<String> = std::thread::scope(|s| {
        s.spawn(|| {
            listed
                .iter()
                .filter(|sc| {
                    let run = harness::execute::run(sc);
                    classify::classify(sc, &run).class == Class::Pass
                })
                .map(|sc| sc.id.clone())
                .collect()
        })
        .join()
        .unwrap_or_else(|_| panic!("a listed scenario panicked the runner"))
    });
    assert!(
        passing.is_empty(),
        "these scenarios pass now: the deviation is gone, so move each one back into scope (remove \
         it from scope::INCOMPATIBILITIES) and update the docs:\n{}",
        passing.join("\n")
    );
}

fn lanes() -> usize {
    std::env::var("TINKERPOP_LANES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(4, |n| n.get()))
        .max(1)
}

/// Path of the committed ratchet baseline.
fn baseline_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/tinkerpop/baseline.toml")
}

/// Path of the Markdown report: `$CARGO_TARGET_DIR/tinkerpop-report.md` (relative values resolve
/// against the workspace root), else `<workspace root>/target/tinkerpop-report.md`; on the
/// forwarding storage (`GRAPHERSAL_TEST_STORAGE=forwarding`) `tinkerpop-report-forwarding.md`, and
/// `tinkerpop-report-<name>.md` on every other lane (`layered`, `layered-delta`).
fn report_path() -> PathBuf {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let workspace = workspace.canonicalize().unwrap_or(workspace);
    let target = match std::env::var_os("CARGO_TARGET_DIR").filter(|v| !v.is_empty()) {
        Some(dir) => workspace.join(dir),
        None => workspace.join("target"),
    };
    target.join(harness::storage::current().report_file("tinkerpop"))
}

/// Whether `TINKERPOP_UPDATE_BASELINE` asks for a rewrite (set, non-empty, not `0`).
fn update_requested() -> bool {
    std::env::var(baseline::UPDATE_ENV).is_ok_and(|v| !v.is_empty() && v != "0")
}

/// Writes to the process's stderr directly, bypassing the test harness's output capture, so the
/// summary shows on every run (not only on failure or with `--nocapture`).
fn stderr_uncaptured(text: &str) {
    use std::io::Write as _;
    let mut err = std::io::stderr().lock();
    let _ = err.write_all(text.as_bytes());
    let _ = err.flush();
}

/// Runs every scenario, writes the report and gates the result against the ratchet baseline
/// (`tests/tinkerpop/baseline.toml`): fails on regressions and on stale baseline entries.
///
/// It also compares the generated blocks of the book page `tinkerpop_compliance.md` with this run
/// and fails when they drifted. `TINKERPOP_UPDATE_BASELINE=1` rewrites the baseline and those
/// blocks from this run instead of gating;
/// `TINKERPOP_DETAILS=1` prints one line per non-passing scenario.
///
/// `GRAPHERSAL_TEST_STORAGE=forwarding` runs every scenario on `Graph<Forwarding>`
/// (`harness::storage`): the same gate against the same baseline and page, plus no scenario may
/// change class against the baseline; it never rewrites the baseline or the page (an update
/// request is refused) and writes `tinkerpop-report-forwarding.md`.
#[test]
fn tinkerpop_conformance() {
    use harness::execute::{self, ScenarioRun};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let storage = harness::storage::current();
    assert!(
        !(storage != TestStorage::TraversalGraph && update_requested()),
        "{}=1 is refused with {}={}: the default run (on TraversalGraph) owns the \
         baseline and the compliance page; unset {} to regenerate them",
        baseline::UPDATE_ENV,
        harness::storage::STORAGE_ENV,
        storage.name(),
        harness::storage::STORAGE_ENV
    );
    if storage != TestStorage::TraversalGraph {
        stderr_uncaptured(&format!(
            "tinkerpop: running on the {} storage ({}={})\n",
            storage.name(),
            harness::storage::STORAGE_ENV,
            storage.name()
        ));
    }

    let features =
        gherkin::load_all(&features_root()).unwrap_or_else(|e| panic!("parse error: {e}"));
    let mut scenarios: Vec<&gherkin::Scenario> =
        features.iter().flat_map(|f| f.scenarios.iter()).collect();
    scenarios.sort_by(|a, b| a.id.cmp(&b.id));

    let started = std::time::Instant::now();
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<ScenarioRun>>> = Mutex::new(vec![None; scenarios.len()]);
    std::thread::scope(|s| {
        for _ in 0..lanes() {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(scenario) = scenarios.get(i) else {
                        break;
                    };
                    // Out of the compatibility scope: not executed (classified below).
                    if scope::scope_exclusion(scenario).is_some() {
                        continue;
                    }
                    let run = execute::run(scenario);
                    results.lock().unwrap_or_else(|p| p.into_inner())[i] = Some(run);
                }
            });
        }
    });
    let elapsed = started.elapsed();
    let results = results.into_inner().unwrap_or_else(|p| p.into_inner());

    let mut outcomes: Vec<(&gherkin::Scenario, Outcome)> = Vec::with_capacity(scenarios.len());
    let mut harness_errors: Vec<(String, String)> = Vec::new();
    for (scenario, run) in scenarios.iter().zip(results.iter()) {
        if let Some(exclusion) = scope::scope_exclusion(scenario) {
            outcomes.push((scenario, classify::excluded(scenario, &exclusion)));
            continue;
        }
        let Some(run) = run else {
            panic!("scenario {} did not report", scenario.id);
        };
        if let Some(e) = &run.harness_error {
            harness_errors.push((scenario.id.clone(), e.clone()));
        }
        outcomes.push((scenario, classify::classify(scenario, run)));
    }
    eprintln!(
        "tinkerpop execution: {} scenarios in {:.1} s ({} lanes)",
        scenarios.len(),
        elapsed.as_secs_f64(),
        lanes()
    );
    if std::env::var_os("TINKERPOP_DETAILS").is_some() {
        for (scenario, outcome) in &outcomes {
            if !matches!(outcome.class, Class::Pass | Class::Excluded) || outcome.order_relaxed {
                eprintln!(
                    "detail {} {}{}: {}",
                    outcome.class.name(),
                    scenario.id,
                    if outcome.order_relaxed {
                        " (order-relaxed)"
                    } else {
                        ""
                    },
                    outcome.detail
                );
            }
        }
    }
    for (id, message) in &harness_errors {
        eprintln!("harness error in {id}: {message}");
    }

    let by_id = || outcomes.iter().map(|(s, o)| (s.id.as_str(), o));

    // Gate against the baseline as committed (before any rewrite).
    let path = baseline_path();
    let recorded = match std::fs::read_to_string(&path) {
        Ok(text) => Some(
            baseline::parse(&text)
                .unwrap_or_else(|e| panic!("{}: invalid baseline: {e}", path.display())),
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => panic!("read {}: {e}", path.display()),
    };
    let gate = recorded.as_ref().map(|b| baseline::gate(b, by_id()));

    let update = update_requested();
    let note = if update {
        let fresh = baseline::from_outcomes(by_id());
        let text = baseline::render(&fresh);
        std::fs::write(&path, &text).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
        let old = recorded.as_ref().map_or(0, |b| b.len());
        let added = recorded.as_ref().map_or(fresh.len(), |b| {
            fresh.keys().filter(|k| !b.contains_key(*k)).count()
        });
        let removed = recorded
            .as_ref()
            .map_or(0, |b| b.keys().filter(|k| !fresh.contains_key(*k)).count());
        stderr_uncaptured(&format!(
            "{}=1: rewrote {} with {} entries (was {old}; +{added} -{removed}); review the diff\n",
            baseline::UPDATE_ENV,
            path.display(),
            fresh.len()
        ));
        format!(
            "regenerated by this run ({} entries; the comparison below is against the previous file).",
            fresh.len()
        )
    } else {
        match &recorded {
            Some(b) => format!("`tests/tinkerpop/baseline.toml`, {} entries.", b.len()),
            None => "missing (`tests/tinkerpop/baseline.toml` does not exist).".to_string(),
        }
    };

    let rows: Vec<report::Row<'_>> = outcomes
        .iter()
        .map(|(s, o)| report::Row {
            id: s.id.as_str(),
            outcome: o,
        })
        .collect();
    // API inventory (informational): only an unreadable or malformed inventory file fails the test.
    let inventory_check = inventory::check(
        &inventory::load().unwrap_or_else(|e| panic!("API inventory: {e}")),
        &inventory::registered(),
    );
    let inventory_section = inventory::markdown(&inventory_check, &report::missing_counts(&rows));
    let report_file = report_path();
    if let Some(dir) = report_file.parent() {
        std::fs::create_dir_all(dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
    }
    std::fs::write(
        &report_file,
        report::markdown(&rows, gate.as_ref(), &note, &inventory_section),
    )
    .unwrap_or_else(|e| panic!("write {}: {e}", report_file.display()));
    stderr_uncaptured(&report::summary(&rows, &report_file.display().to_string()));
    stderr_uncaptured(&inventory::summary_line(&inventory_check));

    if let Some(g) = &gate
        && !g.class_changed.is_empty()
    {
        stderr_uncaptured(&format!(
            "note: {} baseline scenario(s) still fail under another class than recorded (listed in the report; not a failure)\n",
            g.class_changed.len()
        ));
    }

    // The generated blocks of the book page: rewritten on update, compared (and failing) otherwise.
    let blocks = page::render_blocks(&rows, &inventory::summary_line(&inventory_check));
    let page_file = page::page_path();
    let page_text = std::fs::read_to_string(&page_file)
        .unwrap_or_else(|e| panic!("read {}: {e}", page_file.display()));
    let page_problem = if update {
        let new = page::rewrite(&page_text, &blocks)
            .unwrap_or_else(|e| panic!("{}: {e}", page_file.display()));
        if new != page_text {
            std::fs::write(&page_file, &new)
                .unwrap_or_else(|e| panic!("write {}: {e}", page_file.display()));
            stderr_uncaptured(&format!(
                "{}=1: rewrote the generated blocks of {}; review the diff\n",
                baseline::UPDATE_ENV,
                page_file.display()
            ));
        }
        None
    } else {
        match page::check(&page_text, &blocks) {
            Ok(None) => None,
            Ok(Some(diff)) => Some(format!(
                "the generated blocks of {} are out of date:\n{diff}run `{}=1 cargo test --package graphersal --features script,io,display,serde --test tinkerpop_features` and commit the page",
                page_file.display(),
                baseline::UPDATE_ENV
            )),
            Err(e) => Some(format!("{}: {e}", page_file.display())),
        }
    };
    if update {
        return;
    }
    let Some(gate) = gate else {
        panic!(
            "no baseline at {}; generate it with {}=1 cargo test --package graphersal --features script,io,display,serde --test tinkerpop_features",
            path.display(),
            baseline::UPDATE_ENV
        );
    };
    let mut problems: Vec<String> = Vec::new();
    if gate.failed() {
        problems.push(format!(
            "TinkerPop feature suite does not match the baseline:\n{}",
            gate.failure_message()
        ));
    }
    problems.extend(page_problem);
    // On a foreign storage the classification must be the default run's exactly: a scenario
    // that still fails but under another class than recorded is a difference there.
    if storage != TestStorage::TraversalGraph && !gate.class_changed.is_empty() {
        problems.push(format!(
            "{} baseline scenario(s) fail under another class on the {} storage than recorded \
             (see the report): {:?}",
            gate.class_changed.len(),
            storage.name(),
            gate.class_changed
        ));
    }
    assert!(problems.is_empty(), "{}", problems.join("\n\n"));
}
