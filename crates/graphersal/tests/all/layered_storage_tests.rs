//! The DSL differential on `LayeredGraph` (`graphersal::storage::LayeredGraph`): every
//! documented DSL example (`graphersal::script::doc_examples`) and every curated Rhai and GQL
//! script of `foreign_storage_tests.rs` runs on `Graph<TraversalGraph>` (the modern graph) and on
//! `Graph<LayeredGraph>` in two setups, in each of the three run modes (`Plain`: a unit per
//! traversal, `Atomic`: the whole script one unit, `DryRun`: the whole script rolled back with
//! its change set):
//!
//! - `Base`: the modern graph is the layer's frozen base, every write of a script goes into the
//!   layer (copy-up images, tombstones, cross edges);
//! - `Delta`: the modern graph is written into the delta of a layer over an empty base
//!   (`test_storage::copy_into_delta`), so every element the script touches is a delta element.
//!
//! The rendered result, the change set of a dry run, the state afterwards (data, schema,
//! catalog, statistics) and the commit position must be equal. Two classes of difference are
//! accepted, each counted and listed by the test that meets it (`--nocapture` prints them):
//!
//! - order only (plan decision P4 and the order classes of S1.5/S1.6): a script that drops
//!   elements or labels (or relabels an edge) and differs only in the order the layer lists
//!   elements afterwards; checked by re-rendering both graphs with every list sorted;
//! - refusals of plan decision P5 (until stage 4): property indexes, compression rules (and a
//!   compression estimate over changed elements) and vector rules are refused on a layer with
//!   `GraphError::Unsupported`; the script must fail on the layer with that refusal, and only
//!   scripts that define or estimate one of them may;
//! - a later task of the series, named (none today: the memory figures of S1.11 compare
//!   equal).
//!
//! Anything else is a bug.
#![cfg(all(feature = "script", feature = "io"))]

use std::sync::Arc;

use graphersal::TraversalGraph;
use graphersal::storage::{Graph, GraphStorage, LayeredGraph};

use crate::foreign_storage_tests::{self as reference_scripts, Mode, Outcome, reference};
use crate::test_storage::{copy_into_delta, layer_over};

/// Where the dataset lives in the layered graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Setup {
    /// The modern graph is the frozen base.
    Base,
    /// The modern graph is in the delta of a layer over an empty base.
    Delta,
}

const SETUPS: [Setup; 2] = [Setup::Base, Setup::Delta];
const MODES: [Mode; 3] = [Mode::Plain, Mode::Atomic, Mode::DryRun];

/// The modern graph as a layered graph in `setup`.
fn layered(setup: Setup) -> Arc<Graph<LayeredGraph>> {
    let modern = TraversalGraph::tinkerpop_modern();
    Arc::new(Graph::new(match setup {
        Setup::Base => layer_over(modern),
        Setup::Delta => copy_into_delta(&modern),
    }))
}

/// Whether `script` drops elements, properties or labels, or relabels an edge: the scripts
/// whose element order may differ on a layer (P4).
fn reorders(script: &str) -> bool {
    [
        "drop(",
        "drop_label(",
        "dropLabel(",
        "set_label(",
        "setLabel(",
        "DELETE",
        "REMOVE",
        "drop_query(",
    ]
    .iter()
    .any(|needle| script.contains(needle))
}

/// Whether `script` defines or estimates what plan decision P5 refuses on a layer until stage 4.
fn defines_p5_kind(script: &str) -> bool {
    [
        "create_index(",
        "define_compression(",
        "define_compressions(",
        "estimate_compression(",
    ]
    .iter()
    .any(|needle| script.contains(needle))
}

/// The error of a P5 refusal on a layer: the storage's (`... a layered graph ... yet`) or the
/// front end's, which refuses an index definition from the capabilities first.
fn is_p5_refusal(outcome: &Outcome) -> bool {
    outcome.result.as_ref().is_err_and(|e| {
        e.contains("a layered graph") || e.contains("does not support property indexes")
    })
}

/// A difference that a later task of the series removes, by name; `None` for any other. None
/// is left: the memory figures (S1.11) compare equal since the layer reports its own.
fn later_task(script: &str, expected: &Outcome, actual: &Outcome) -> Option<&'static str> {
    let _ = (script, expected, actual);
    None
}

/// Characters of a rendered value, sorted: equal for two renderings that list the same items
/// in another order (a necessary condition for an order-only difference; the sorted state
/// check below is the sufficient one for the data).
fn sorted_chars(text: &Result<String, String>) -> Result<Vec<char>, String> {
    text.clone().map(|t| {
        let mut chars: Vec<char> = t.chars().collect();
        chars.sort_unstable();
        chars
    })
}

/// The state of a graph with every list in id order: the comparison of an order-only difference.
const SORTED_STATE: &str = r#"[
    g.V().order().by(T.id).elementMap().toList(),
    g.E().order().by(T.id).elementMap().toList(),
    g.V().order().by(T.id).project("v", "out", "in").by(T.id).by(__.outE().id().order().fold()).by(__.inE().id().order().fold()).toList(),
    g.V().order().by(T.id).project("v", "l").by(T.id).by(__.labels()).toList(),
    g.get_schema(),
    g.statistics()
]"#;

/// One script of one list in one mode and setup that differs.
struct Difference {
    setup: Setup,
    mode: Mode,
    list: &'static str,
    script: String,
    expected: Outcome,
    actual: Outcome,
}

impl Difference {
    fn describe(&self) -> String {
        format!(
            "[{:?} {:?} {}] `{}`\n  TraversalGraph: {:?}\n  LayeredGraph:   {:?}",
            self.setup, self.mode, self.list, self.script, self.expected, self.actual
        )
    }
}

/// The differences of one list, sorted into the accepted classes and the rest.
#[derive(Default)]
struct Differences {
    /// Scripts compared (script x mode x setup).
    compared: usize,
    order_only: Vec<String>,
    p5_refusals: Vec<String>,
    later_tasks: Vec<String>,
    unexplained: Vec<String>,
}

impl Differences {
    /// Runs `script` (of `list`) in every mode on the reference and on both layered setups.
    fn compare(&mut self, list: &'static str, script: &str) {
        for mode in MODES {
            let expected = reference_scripts::run_curated(reference(), mode, script);
            for setup in SETUPS {
                self.compared += 1;
                let layer = layered(setup);
                let actual = reference_scripts::run_curated(Arc::clone(&layer), mode, script);
                if expected == actual {
                    continue;
                }
                let difference = Difference {
                    setup,
                    mode,
                    list,
                    script: script.to_string(),
                    expected: reference_scripts::run_curated(reference(), mode, script),
                    actual,
                };
                self.classify(difference, layer);
            }
        }
    }

    fn classify(&mut self, difference: Difference, layer: Arc<Graph<LayeredGraph>>) {
        let Difference {
            script,
            expected,
            actual,
            ..
        } = &difference;
        if defines_p5_kind(script) && is_p5_refusal(actual) {
            self.p5_refusals.push(difference.describe());
            return;
        }
        if let Some(task) = later_task(script, expected, actual) {
            self.later_tasks
                .push(format!("({task}) {}", difference.describe()));
            return;
        }
        if reorders(script)
            && sorted_chars(&expected.result) == sorted_chars(&actual.result)
            && sorted_chars(&Ok(format!("{:?}", expected.changes)))
                == sorted_chars(&Ok(format!("{:?}", actual.changes)))
            && sorted_chars(&expected.state) == sorted_chars(&actual.state)
            && expected.last_commit_seq == actual.last_commit_seq
        {
            // The same data in another order: the states sorted by id must be equal (the
            // reference re-runs the script on a fresh graph in the same mode).
            let plain = reference();
            let _ = reference_scripts::run_curated(Arc::clone(&plain), difference.mode, script);
            if reference_scripts::render(plain, SORTED_STATE)
                == reference_scripts::render(layer, SORTED_STATE)
            {
                self.order_only.push(difference.describe());
                return;
            }
        }
        self.unexplained.push(difference.describe());
    }

    /// Fails on an unexplained difference; prints the accepted ones.
    fn check(&self, what: &str, min_compared: usize) {
        assert!(
            self.compared >= min_compared,
            "{what}: only {} runs compared",
            self.compared
        );
        println!(
            "{what}: {} runs compared; order-only differences (P4): {}; P5 refusals: {}; \
             left to later tasks: {}",
            self.compared,
            self.order_only.len(),
            self.p5_refusals.len(),
            self.later_tasks.len()
        );
        let lines = self.order_only.iter();
        for line in lines.chain(&self.p5_refusals).chain(&self.later_tasks) {
            // The script and the class; the outcomes only for order-only differences.
            println!("{}", line.lines().next().unwrap_or_default());
        }
        assert!(
            self.unexplained.is_empty(),
            "{what}: {} differences:\n{}",
            self.unexplained.len(),
            self.unexplained.join("\n")
        );
    }
}

#[test]
fn the_copy_into_a_delta_reads_like_its_source() {
    let expected = reference_scripts::render(reference(), reference_scripts::STATE);
    assert!(expected.is_ok(), "{expected:?}");
    for setup in SETUPS {
        let actual = reference_scripts::render(layered(setup), reference_scripts::STATE);
        assert_eq!(actual, expected, "{setup:?}");
        let layer = layered(setup);
        let base_vertices = layer
            .read()
            .base()
            .statistics()
            .unwrap()
            .map(|s| s.vertex_count());
        let empty_base = base_vertices == Some(0);
        assert_eq!(empty_base, setup == Setup::Delta, "{setup:?}");
    }
}

#[test]
fn every_documented_example_gives_the_same_result_on_a_layer() {
    let mut differences = Differences::default();
    for (_receiver, _name, example, result) in graphersal::script::doc_examples() {
        // `None`: the example needs a file or a host setup (the curated lists cover those).
        if result.is_none() {
            continue;
        }
        differences.compare("doc examples", example);
    }
    differences.check("doc examples", 200 * 6);
}

#[test]
fn curated_scripts_give_the_same_result_on_a_layer() {
    let mut differences = Differences::default();
    for (_mode, script) in reference_scripts::CURATED {
        differences.compare("CURATED", script);
    }
    differences.check("curated Rhai scripts", reference_scripts::CURATED.len() * 6);
}

#[cfg(feature = "gql")]
#[test]
fn curated_gql_scripts_give_the_same_result_on_a_layer() {
    use reference_scripts::*;
    let mut differences = Differences::default();
    let plain: [(&'static str, &[&str]); 9] = [
        ("GQL_CURATED", GQL_CURATED),
        ("GQL_CURATED_1B_ERRORS", GQL_CURATED_1B_ERRORS),
        ("GQL_CURATED_1C", GQL_CURATED_1C),
        ("GQL_CURATED_2A", GQL_CURATED_2A),
        ("GQL_CURATED_2B", GQL_CURATED_2B),
        ("GQL_CURATED_2C", GQL_CURATED_2C),
        ("GQL_CURATED_2D", GQL_CURATED_2D),
        ("GQL_CURATED_2E", GQL_CURATED_2E),
        ("GQL_CURATED_3G", GQL_CURATED_3G),
    ];
    let mut count = 0;
    for (list, scripts) in plain {
        for script in scripts {
            count += 1;
            if list == "GQL_CURATED_3G" {
                differences.compare(list, &format!("{script}; {DUMP_3A}"));
            } else {
                differences.compare(list, script);
            }
        }
    }
    let writes: [(&'static str, &[(Mode, &str)]); 4] = [
        ("GQL_CURATED_3A", GQL_CURATED_3A),
        ("GQL_CURATED_3C", GQL_CURATED_3C),
        ("GQL_CURATED_3D", GQL_CURATED_3D),
        ("GQL_CURATED_3E", GQL_CURATED_3E),
    ];
    for (list, scripts) in writes {
        for (_mode, script) in scripts {
            count += 1;
            differences.compare(list, &format!("{script}; {DUMP_3A}"));
        }
    }
    for script in GQL_CURATED_3H {
        count += 1;
        let script = script
            .replace("OLDER", OLDER_QUERY_3H)
            .replace("HIRE", HIRE_QUERY_3H);
        differences.compare("GQL_CURATED_3H", &format!("{script}; {DUMP_3A}"));
    }
    differences.check("curated GQL scripts", count * 6);
}

/// A file of this test under the system temp directory, removed on drop.
struct TempFile(std::path::PathBuf);

impl TempFile {
    fn new(name: &str) -> Self {
        Self(std::env::temp_dir().join(format!(
            "graphersal-layered-storage-{}-{name}",
            std::process::id()
        )))
    }
    fn path(&self) -> String {
        self.0.display().to_string().replace('\\', "/")
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn graphson_export_and_import_give_the_same_result_on_a_layer() {
    let reference_file = TempFile::new("reference.json");
    let export = |path: &str| format!(r#"g.export_graphson("{path}")"#);
    let expected = reference_scripts::render(reference(), &export(&reference_file.path()))
        .map(|text| text.replace(&reference_file.path(), "<file>"));
    assert!(expected.is_ok(), "{expected:?}");
    let import = format!(
        r#"g.V().drop().iterate(); let report = g.import_graphson("{}"); [report, g.V().count().next(), g.E().count().next()]"#,
        reference_file.path()
    );
    for setup in SETUPS {
        let layered_file = TempFile::new(&format!("{setup:?}.json"));
        let actual = reference_scripts::render(layered(setup), &export(&layered_file.path()))
            .map(|text| text.replace(&layered_file.path(), "<file>"));
        assert_eq!(expected, actual, "{setup:?}");
        assert_eq!(
            std::fs::read_to_string(&reference_file.0).unwrap(),
            std::fs::read_to_string(&layered_file.0).unwrap(),
            "{setup:?}"
        );
    }
    // Import into the emptied graph: same report, same state, in every mode (the reference
    // reuses the dropped slots, the layer appends: an order-only difference of P4).
    let expected = reference_scripts::run_curated(reference(), Mode::Plain, &import);
    assert!(expected.result.is_ok(), "{expected:?}");
    let mut differences = Differences::default();
    differences.compare("GraphSON import", &import);
    differences.check("GraphSON import", 6);
}

/// A compression estimate on a layer: the base's while the kind is untouched, refused (P5) once
/// the layer added, removed or changed an element of it, the other kind still answered.
#[test]
fn compression_estimates_answer_only_for_untouched_kinds() {
    use graphersal::catalog::CompressionEstimateRequest;
    use graphersal::error::GraphError;
    use graphersal::storage::ElementKind;

    let base = TraversalGraph::tinkerpop_modern();
    let vertices = CompressionEstimateRequest::new(ElementKind::Vertex, "person", "name").unwrap();
    let edges = CompressionEstimateRequest::new(ElementKind::Edge, "knows", "weight").unwrap();
    let expected = base.estimate_compression(&vertices);
    let mut layer = layer_over(base);
    let actual = GraphStorage::estimate_compression(&layer, &vertices).unwrap();
    assert_eq!(
        (actual.values_found, actual.candidates.len()),
        (expected.values_found, expected.candidates.len())
    );
    layer
        .insert_vertex(None, vec!["person".into()], Default::default())
        .unwrap();
    let err = GraphStorage::estimate_compression(&layer, &vertices).unwrap_err();
    assert!(matches!(err, GraphError::Unsupported { .. }), "{err}");
    assert!(GraphStorage::estimate_compression(&layer, &edges).is_ok());
    let delta = copy_into_delta(&TraversalGraph::tinkerpop_modern());
    assert!(GraphStorage::estimate_compression(&delta, &vertices).is_err());
}
