//! Runs the vendored openCypher TCK 2024.3 (`tests/opencypher/features/`) against Graphersal's
//! GQL (the Rust API, no Rhai).
//!
//! A separate test target (an exception to the one-binary rule, like `tinkerpop_features.rs`):
//! own process for parallel lanes, the panic hook and per-case timeouts. The harness lives under
//! `tests/opencypher/harness/`; its unit tests run as part of this target. The target parses
//! the whole tree, runs and classifies every in-scope case (`harness::execute`,
//! `harness::classify` with `harness::compare`; the scope is `harness::scope`), writes the
//! report (`harness::report`) and gates the run against the ratchet baseline
//! (`harness::baseline`) and the generated blocks of the book page (`harness::page`).
#![cfg(feature = "gql")]

#[path = "opencypher/harness/mod.rs"]
mod harness;

// The proof storage of `GRAPHERSAL_TEST_STORAGE=forwarding` (`harness::storage`), included by
// path like `tinkerpop_features.rs` does: a dev-dependency on `graphersal-storage-tests` would
// link a second copy of `graphersal` into this binary.
#[allow(dead_code)]
#[path = "../../graphersal-storage-tests/src/forwarding.rs"]
mod forwarding;

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use harness::gherkin::{self, Step};

/// Root of the vendored `.feature` files.
fn features_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/opencypher/features")
}

/// Root of the vendored named graphs.
fn graphs_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/opencypher/graphs")
}

/// Counts found by plain line scanning, independently of the parser.
#[derive(Debug, Default, PartialEq, Eq)]
struct LineCounts {
    files: usize,
    scenarios: usize,
    outlines: usize,
    /// Plain scenarios + example rows of the outlines.
    expanded: usize,
}

/// Scans every `.feature` file below `dir` and adds its line counts to `per_dir`, keyed by the
/// first two path components below the features root (e.g. `clauses/match`).
fn line_counts(root: &Path, dir: &Path, per_dir: &mut BTreeMap<String, LineCounts>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.unwrap_or_else(|e| panic!("read entry: {e}")).path();
        if path.is_dir() {
            line_counts(root, &path, per_dir);
            continue;
        }
        if path.extension().is_none_or(|ext| ext != "feature") {
            continue;
        }
        let rel = path.strip_prefix(root).unwrap_or(&path);
        let key = rel
            .components()
            .take(2)
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let c = per_dir.entry(key).or_default();
        c.files += 1;
        // Example rows: the `|` lines after an `Examples:` line, minus its header row.
        let mut in_examples = false;
        let mut header_seen = false;
        for line in text.lines() {
            let t = line.trim();
            if t.starts_with("Scenario:") {
                c.scenarios += 1;
                c.expanded += 1;
            } else if t.starts_with("Scenario Outline:") {
                c.outlines += 1;
            }
            if t.starts_with("Examples:") {
                in_examples = true;
                header_seen = false;
            } else if in_examples && t.starts_with('|') {
                if header_seen {
                    c.expanded += 1;
                } else {
                    header_seen = true;
                }
            } else if in_examples && !t.is_empty() && !t.starts_with('#') {
                in_examples = false;
            }
        }
    }
}

/// Expanded case counts per sub-directory of the pinned tree (recorded at the pin, tag 2024.3).
const EXPANDED_PER_DIR: [(&str, usize); 37] = [
    ("clauses/call", 52),
    ("clauses/create", 78),
    ("clauses/delete", 41),
    ("clauses/match", 381),
    ("clauses/match-where", 34),
    ("clauses/merge", 75),
    ("clauses/remove", 33),
    ("clauses/return", 63),
    ("clauses/return-orderby", 35),
    ("clauses/return-skip-limit", 31),
    ("clauses/set", 53),
    ("clauses/union", 12),
    ("clauses/unwind", 14),
    ("clauses/with", 29),
    ("clauses/with-orderBy", 292),
    ("clauses/with-skip-limit", 9),
    ("clauses/with-where", 19),
    ("expressions/aggregation", 35),
    ("expressions/boolean", 150),
    ("expressions/comparison", 72),
    ("expressions/conditional", 13),
    ("expressions/existentialSubqueries", 10),
    ("expressions/graph", 61),
    ("expressions/list", 185),
    ("expressions/literals", 131),
    ("expressions/map", 44),
    ("expressions/mathematical", 6),
    ("expressions/null", 44),
    ("expressions/path", 7),
    ("expressions/pattern", 50),
    ("expressions/precedence", 121),
    ("expressions/quantifier", 604),
    ("expressions/string", 32),
    ("expressions/temporal", 1004),
    ("expressions/typeConversion", 47),
    ("useCases/countingSubgraphMatches", 11),
    ("useCases/triadicSelection", 19),
];

#[test]
fn all_feature_files_parse() {
    let root = features_root();
    let features = gherkin::load_all(&root).unwrap_or_else(|e| panic!("parse error: {e}"));

    // Independent line counts.
    let mut lines: BTreeMap<String, LineCounts> = BTreeMap::new();
    line_counts(&root, &root, &mut lines);
    let total = lines.values().fold(LineCounts::default(), |mut a, c| {
        a.files += c.files;
        a.scenarios += c.scenarios;
        a.outlines += c.outlines;
        a.expanded += c.expanded;
        a
    });
    assert_eq!(
        total,
        LineCounts {
            files: 220,
            scenarios: 1339,
            outlines: 276,
            expanded: 3897,
        },
        "the line counts of the pinned tree changed"
    );

    // Parser counts, overall and per directory.
    let mut parsed: BTreeMap<String, LineCounts> = BTreeMap::new();
    for f in &features {
        let key = f.path.split('/').take(2).collect::<Vec<_>>().join("/");
        let c = parsed.entry(key).or_default();
        c.files += 1;
        for s in &f.scenarios {
            if s.outline {
                c.outlines += 1;
            } else {
                c.scenarios += 1;
            }
            c.expanded += s.cases.len();
        }
    }
    assert_eq!(parsed, lines, "parser counts != line counts");
    assert_eq!(features.len(), 220);
    let scenarios: usize = features.iter().map(|f| f.scenarios.len()).sum();
    assert_eq!(scenarios, 1615);
    let cases: Vec<&gherkin::Case> = features
        .iter()
        .flat_map(|f| f.scenarios.iter().flat_map(|s| s.cases.iter()))
        .collect();
    assert_eq!(cases.len(), 3897);
    let expected: BTreeMap<String, usize> = EXPANDED_PER_DIR
        .iter()
        .map(|(d, n)| (d.to_string(), *n))
        .collect();
    let got: BTreeMap<String, usize> = parsed
        .iter()
        .map(|(d, c)| (d.clone(), c.expanded))
        .collect();
    assert_eq!(got, expected, "expanded cases per directory");

    // Ids are unique across the tree.
    let mut ids = HashSet::new();
    for c in &cases {
        assert!(ids.insert(c.id.as_str()), "duplicate case id {}", c.id);
    }

    // Exactly one Background (`clauses/match/Match5.feature`) and one `@ignore` scenario.
    let backgrounds: Vec<&str> = features
        .iter()
        .filter(|f| !f.background.is_empty())
        .map(|f| f.path.as_str())
        .collect();
    assert_eq!(backgrounds, vec!["clauses/match/Match5.feature"]);
    let ignored: Vec<&str> = cases
        .iter()
        .filter(|c| c.is_ignored())
        .map(|c| c.id.as_str())
        .collect();
    assert_eq!(
        ignored,
        vec!["expressions/graph/Graph5.feature::[2] Single-labels expression on relationships"]
    );

    // Every named graph exists.
    for c in &cases {
        for s in &c.steps {
            if let Step::NamedGraph(name) = &s.step {
                let file = graphs_root().join(name).join(format!("{name}.cypher"));
                assert!(
                    file.is_file(),
                    "{}: missing named graph {}",
                    c.id,
                    file.display()
                );
            }
        }
    }

    eprintln!(
        "opencypher tck: {} files, {scenarios} scenarios, {} cases",
        features.len(),
        cases.len()
    );
}

/// Classifies a step line by its text alone (a deliberately naive second reading of the step
/// vocabulary, independent of the parser).
fn naive_kind(text: &str) -> &'static str {
    let t = text.trim_end_matches(':').trim_end();
    match t {
        "an empty graph" => "empty graph",
        "any graph" => "any graph",
        "having executed" => "having executed",
        "parameters are" => "parameters",
        "executing query" => "query",
        "executing control query" => "control query",
        "the result should be empty" => "result empty",
        "no side effects" => "no side effects",
        "the side effects should be" => "side effects",
        _ if t.starts_with("the result should be") => "result",
        _ if t.starts_with("there exists a procedure") => "procedure",
        _ if t.ends_with(" graph") => "named graph",
        _ if t.contains(" should be raised at ") => "error",
        _ => "unknown",
    }
}

fn step_kind(step: &Step) -> &'static str {
    match step {
        Step::EmptyGraph => "empty graph",
        Step::AnyGraph => "any graph",
        Step::NamedGraph(_) => "named graph",
        Step::HavingExecuted(_) => "having executed",
        Step::Parameters(_) => "parameters",
        Step::Procedure { .. } => "procedure",
        Step::ExecutingQuery(_) => "query",
        Step::ExecutingControlQuery(_) => "control query",
        Step::Result { .. } => "result",
        Step::ResultEmpty => "result empty",
        Step::Error { .. } => "error",
        Step::NoSideEffects => "no side effects",
        Step::SideEffects(_) => "side effects",
    }
}

/// Every step line of the tree is typed: the per-kind counts of the parsed steps (background
/// steps once per feature, scenario steps once per scenario) equal a naive line classification.
#[test]
fn every_step_line_is_typed() {
    let root = features_root();
    let features = gherkin::load_all(&root).unwrap_or_else(|e| panic!("parse error: {e}"));

    let mut naive: BTreeMap<&str, usize> = BTreeMap::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read: {e}")) {
            let path = entry.unwrap_or_else(|e| panic!("entry: {e}")).path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read: {e}"));
            for line in text.lines() {
                let t = line.trim();
                if let Some(rest) = ["Given ", "When ", "Then ", "And ", "But "]
                    .iter()
                    .find_map(|kw| t.strip_prefix(kw))
                {
                    *naive.entry(naive_kind(rest)).or_default() += 1;
                }
            }
        }
    }
    assert_eq!(naive.get("unknown"), None, "unclassified step lines");

    let mut parsed: BTreeMap<&str, usize> = BTreeMap::new();
    for f in &features {
        for s in &f.background {
            *parsed.entry(step_kind(&s.step)).or_default() += 1;
        }
        for s in &f.scenarios {
            // The first case stands for the scenario; skip the prepended background steps.
            let first = &s.cases[0];
            for st in first.steps.iter().filter(|st| st.line > s.line) {
                *parsed.entry(step_kind(&st.step)).or_default() += 1;
            }
        }
    }
    assert_eq!(parsed, naive);

    // The step vocabulary of the pinned tree (step lines, not expanded).
    let expected: BTreeMap<&str, usize> = [
        ("query", 1615),
        ("result", 1258 + 86 + 16 + 1),
        ("no side effects", 1179),
        ("empty graph", 949),
        ("having executed", 728),
        ("any graph", 619),
        ("side effects", 244),
        ("error", 192),
        ("result empty", 89),
        ("parameters", 43),
        ("procedure", 39),
        ("control query", 27),
        ("named graph", 19),
    ]
    .into_iter()
    .collect();
    assert_eq!(parsed, expected);
}

/// Number of case lanes run in parallel (each lane owns one worker thread).
fn lanes() -> usize {
    std::env::var("OPENCYPHER_LANES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(4, |n| n.get()))
        .max(1)
}

/// Whether the environment variable `name` is set (non-empty, not `0`).
fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| !v.is_empty() && v != "0")
}

/// Writes to the process's stderr directly, bypassing the test harness's output capture, so the
/// summary shows on every run (not only on failure or with `--nocapture`).
fn stderr_uncaptured(text: &str) {
    use std::io::Write as _;
    let mut err = std::io::stderr().lock();
    let _ = err.write_all(text.as_bytes());
    let _ = err.flush();
}

/// Path of the committed ratchet baseline.
fn baseline_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/opencypher/baseline.toml")
}

/// Path of the Markdown report: `$CARGO_TARGET_DIR/opencypher-report.md` (relative values resolve
/// against the workspace root), else `<workspace root>/target/opencypher-report.md`; on the
/// forwarding storage (`GRAPHERSAL_TEST_STORAGE=forwarding`) `opencypher-report-forwarding.md`.
fn report_path() -> PathBuf {
    use harness::storage::TestStorage;
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let workspace = workspace.canonicalize().unwrap_or(workspace);
    let target = match std::env::var_os("CARGO_TARGET_DIR").filter(|v| !v.is_empty()) {
        Some(dir) => workspace.join(dir),
        None => workspace.join("target"),
    };
    target.join(match harness::storage::current() {
        TestStorage::TraversalGraph => "opencypher-report.md",
        TestStorage::Forwarding => "opencypher-report-forwarding.md",
    })
}

/// Every case of the tree, sorted by id.
fn all_cases(features: &[gherkin::Feature]) -> Vec<&gherkin::Case> {
    let mut cases: Vec<&gherkin::Case> = features
        .iter()
        .flat_map(|f| f.scenarios.iter().flat_map(|s| s.cases.iter()))
        .collect();
    cases.sort_by(|a, b| a.id.cmp(&b.id));
    cases
}

/// Runs `cases` in parallel lanes and classifies each.
fn run_and_classify(cases: &[&gherkin::Case]) -> Vec<harness::classify::Outcome> {
    use harness::execute::{self, RawOutcome};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<RawOutcome>>> = Mutex::new(vec![None; cases.len()]);
    std::thread::scope(|s| {
        for _ in 0..lanes() {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(case) = cases.get(i) else {
                        break;
                    };
                    let run = execute::run(case);
                    results.lock().unwrap_or_else(|p| p.into_inner())[i] = Some(run);
                }
            });
        }
    });
    let results = results.into_inner().unwrap_or_else(|p| p.into_inner());
    cases
        .iter()
        .zip(results.iter())
        .map(|(case, run)| {
            let Some(run) = run else {
                panic!("case {} did not report", case.id);
            };
            harness::classify::classify(case, run)
        })
        .collect()
}

/// The scope table (`harness::scope`) names existing cases only, covers every procedure and
/// `@ignore` case, links existing book pages, and its type-error deviation lists only cases
/// that expect a compile-time `InvalidArgumentType` error.
#[test]
fn scope_table_is_consistent() {
    use gherkin::{ErrorPhase, ErrorType};
    use harness::scope;

    let features =
        gherkin::load_all(&features_root()).unwrap_or_else(|e| panic!("parse error: {e}"));
    let cases = all_cases(&features);
    let by_id: BTreeMap<&str, &gherkin::Case> = cases.iter().map(|c| (c.id.as_str(), *c)).collect();
    let book = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../book/src");
    let mut problems = Vec::new();
    for e in scope::EXCLUSIONS {
        let page = e.docs.split('#').next().unwrap_or(e.docs);
        if !book.join(page).is_file() {
            problems.push(format!(
                "{}: docs page book/src/{page} does not exist",
                e.code
            ));
        }
        for id in e.ids {
            if !by_id.contains_key(id) {
                problems.push(format!("{}: no case `{id}` in the tree", e.code));
            }
        }
    }
    for case in &cases {
        let procedure = case
            .steps
            .iter()
            .any(|s| matches!(s.step, Step::Procedure { .. }));
        let code = scope::exclusion(&case.id).map(|e| e.code);
        if procedure && code != Some("procedures") {
            problems.push(format!(
                "{}: uses a vendor procedure, not in `procedures`",
                case.id
            ));
        }
        if case.is_ignored() && code != Some("upstream-ignore") {
            problems.push(format!("{}: @ignore, not in `upstream-ignore`", case.id));
        }
    }
    let deviation = scope::by_code("type-error-at-evaluation")
        .unwrap_or_else(|| panic!("the type-error deviation is missing"));
    for id in deviation.ids {
        let Some(case) = by_id.get(id) else { continue };
        let expects = case.steps.iter().any(|s| {
            matches!(
                &s.step,
                Step::Error { kind: ErrorType::SyntaxError | ErrorType::TypeError, phase: ErrorPhase::CompileTime, detail }
                    if detail == "InvalidArgumentType"
            )
        });
        if !expects {
            problems.push(format!(
                "{id}: does not expect a compile-time InvalidArgumentType error"
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// The deliberate deviations of the scope still deviate: run, no case of them passes (then it
/// belongs back in scope), and every type-error case still raises `22G03` at runtime (the
/// documented reason).
#[test]
fn deviation_cases_still_deviate() {
    use harness::classify::Class;
    use harness::scope;

    let features =
        gherkin::load_all(&features_root()).unwrap_or_else(|e| panic!("parse error: {e}"));
    let cases: Vec<&gherkin::Case> = all_cases(&features)
        .into_iter()
        .filter(|c| scope::exclusion(&c.id).is_some_and(|e| e.deviation))
        .collect();
    assert!(!cases.is_empty());
    let outcomes = run_and_classify(&cases);
    let mut problems = Vec::new();
    for (case, outcome) in cases.iter().zip(&outcomes) {
        let code = scope::exclusion(&case.id).map_or("", |e| e.code);
        if outcome.class == Class::Pass {
            problems.push(format!(
                "{} ({code}) passes now: remove it from harness/scope.rs",
                case.id
            ));
        } else if code == "type-error-at-evaluation"
            && !(outcome.class == Class::WrongError
                && outcome.detail.contains("got 22G03 at runtime"))
        {
            problems.push(format!(
                "{} ({code}) no longer raises 22G03 at runtime: {} {}",
                case.id,
                outcome.class.name(),
                outcome.detail
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// Whether `OPENCYPHER_UPDATE_BASELINE` asks for a rewrite (set, non-empty, not `0`).
fn update_requested() -> bool {
    env_flag(harness::baseline::UPDATE_ENV)
}

/// Runs every in-scope case, writes the report and gates the result against the ratchet
/// baseline (`tests/opencypher/baseline.toml`): fails on regressions and on stale entries. It
/// also compares the generated blocks of the book page `book/src/gql/tck_compliance.md` with
/// this run and fails when they drifted. `OPENCYPHER_UPDATE_BASELINE=1` rewrites the baseline and
/// those blocks from this run instead of gating.
///
/// `GRAPHERSAL_TEST_STORAGE=forwarding` runs every case on `Graph<Forwarding>`: the same gate
/// against the same baseline and page, plus no case may change class against the baseline; it
/// never rewrites the baseline or the page and writes `opencypher-report-forwarding.md`.
///
/// Debug: `OPENCYPHER_DETAILS=1` prints one line per in-scope case that does not pass;
/// `OPENCYPHER_LANES` sets the number of lanes.
#[test]
fn opencypher_tck_conformance() {
    use harness::classify::{Class, Outcome};
    use harness::storage::TestStorage;
    use harness::{baseline, page, report, scope};

    let storage = harness::storage::current();
    assert!(
        !(storage == TestStorage::Forwarding && update_requested()),
        "{}=1 is refused with {}=forwarding: the default run (on TraversalGraph) owns the \
         baseline and the compliance page",
        baseline::UPDATE_ENV,
        harness::storage::STORAGE_ENV,
    );
    let features =
        gherkin::load_all(&features_root()).unwrap_or_else(|e| panic!("parse error: {e}"));
    let cases = all_cases(&features);
    assert_eq!(cases.len(), 3897);
    let (excluded, in_scope): (Vec<&gherkin::Case>, Vec<&gherkin::Case>) = cases
        .iter()
        .partition(|c| scope::exclusion(&c.id).is_some());

    let started = std::time::Instant::now();
    let ran = run_and_classify(&in_scope);
    let elapsed = started.elapsed();
    let mut outcomes: Vec<(&str, Outcome)> =
        in_scope
            .iter()
            .map(|c| c.id.as_str())
            .zip(ran)
            .chain(excluded.iter().filter_map(|c| {
                scope::exclusion(&c.id).map(|e| (c.id.as_str(), Outcome::excluded(e)))
            }))
            .collect();
    outcomes.sort_by(|a, b| a.0.cmp(b.0));
    stderr_uncaptured(&format!(
        "opencypher tck ({}): {} cases in {:.1} s ({} lanes)\n",
        storage.name(),
        outcomes.len(),
        elapsed.as_secs_f64(),
        lanes()
    ));
    if env_flag("OPENCYPHER_DETAILS") {
        let mut text = String::new();
        for (id, outcome) in &outcomes {
            if !matches!(outcome.class, Class::Pass | Class::Excluded) {
                text.push_str(&format!(
                    "detail {} {id}: {}\n",
                    outcome.class.name(),
                    outcome.detail
                ));
            }
        }
        stderr_uncaptured(&text);
    }

    let by_id = || outcomes.iter().map(|(id, o)| (*id, o));

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
            "regenerated by this run ({} entries; the comparison is against the previous file).",
            fresh.len()
        )
    } else {
        match &recorded {
            Some(b) => format!("`tests/opencypher/baseline.toml`, {} entries.", b.len()),
            None => "missing (`tests/opencypher/baseline.toml` does not exist).".to_string(),
        }
    };

    let rows: Vec<report::Row<'_>> = outcomes
        .iter()
        .map(|(id, o)| report::Row { id, outcome: o })
        .collect();
    let report_file = report_path();
    if let Some(dir) = report_file.parent() {
        std::fs::create_dir_all(dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
    }
    std::fs::write(&report_file, report::markdown(&rows, gate.as_ref(), &note))
        .unwrap_or_else(|e| panic!("write {}: {e}", report_file.display()));
    stderr_uncaptured(&report::summary(&rows, &report_file.display().to_string()));
    if let Some(g) = &gate
        && !g.class_changed.is_empty()
    {
        stderr_uncaptured(&format!(
            "note: {} baseline case(s) still fail under another class than recorded (listed in the report)\n",
            g.class_changed.len()
        ));
    }

    // The generated blocks of the book page: rewritten on update, compared (and failing) otherwise.
    let blocks = page::render_blocks(&rows);
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
                "the generated blocks of {} are out of date:\n{diff}run `{}=1 cargo test --package graphersal --features script,io,display,serde,persist,gql --test opencypher_tck` and commit the page",
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
            "no baseline at {}; generate it with {}=1 cargo test --package graphersal --features script,io,display,serde,persist,gql --test opencypher_tck",
            path.display(),
            baseline::UPDATE_ENV
        );
    };
    let mut problems: Vec<String> = Vec::new();
    if gate.failed() {
        problems.push(format!(
            "the openCypher TCK run does not match the baseline:\n{}",
            gate.failure_message()
        ));
    }
    problems.extend(page_problem);
    if storage != TestStorage::TraversalGraph && !gate.class_changed.is_empty() {
        problems.push(format!(
            "{} baseline case(s) fail under another class on the {} storage than recorded: {:?}",
            gate.class_changed.len(),
            storage.name(),
            gate.class_changed
        ));
    }
    assert!(problems.is_empty(), "{}", problems.join("\n\n"));
}
