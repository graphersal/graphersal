//! Runs the vendored openCypher TCK 2024.3 (`tests/opencypher/features/`) against Graphersal's
//! GQL (the Rust API, no Rhai).
//!
//! A separate test target (an exception to the one-binary rule, like `tinkerpop_features.rs`):
//! own process for parallel lanes, the panic hook and per-case timeouts. The harness lives under
//! `tests/opencypher/harness/`; its unit tests run as part of this target. For now the target
//! parses the whole tree; the runner, comparison and baseline come with the later harness steps.
#![cfg(feature = "gql")]

#[path = "opencypher/harness/mod.rs"]
mod harness;

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
