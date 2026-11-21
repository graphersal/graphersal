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
//! catalog, statistics) and the commit position must be equal. These classes of difference are
//! accepted, each counted and listed by the test that meets it (`--nocapture` prints them):
//!
//! - order only (plan decision P4 and the order classes of S1.5/S1.6): a script that drops
//!   elements or labels (or relabels an edge) and differs only in the order the layer lists
//!   elements afterwards; checked by re-rendering both graphs with every list sorted;
//! - refusals of plan decision P5: compression rules (and a compression estimate over changed
//!   elements) are refused on a layer with `GraphError::Unsupported` (vector rules are
//!   accepted since S4.5b); the script must fail on the layer with that refusal, and only
//!   scripts that define or estimate one of them may;
//! - origin queries in the `Delta` setup (S2.3): a script asking for layer origins
//!   (`layer(`, `has_layer(`, `layer_diff(`) may give another result there, since every element
//!   of that setup is an added one; the change set, the state afterwards and the commit position
//!   must still be equal;
//! - a later task of the series, named (none today: the memory figures of S1.11 compare
//!   equal).
//!
//! Anything else is a bug.
//!
//! **The index fixture** (S4.2, `*_on_an_indexed_layer`): the same lists, plus curated index
//! scripts (`INDEX_SCRIPTS`: equality, range, `each`, label-less, `to_lower`, counts, around
//! writes), run over the modern graph with seven property indexes in the base, in three setups
//! compared with a `TraversalGraph` holding the same data and indexes: `Base` (the indexed
//! modern graph is the base), `Delta` (an empty base holding only the index definitions, the
//! data in the delta: every lookup merges the delta part alone) and `Touched` (the indexed
//! modern graph plus 94 filler vertices as the base, with a few percent of it changed in the
//! layer before the script: an image, an added person, a dropped filler, an imaged edge; the
//! reference is a `TraversalGraph` built from that layer's data). After every script the
//! overlays must equal a rebuild (`check_layer_indexes`). One more accepted class there:
//!
//! - index stats: a script listing the indexes (`indexes(`) whose result differs only in the
//!   `bytes` (the layer counts only its own parts) and `distinct_values` (an upper bound on a
//!   touched layer) figures;
//! - origin queries in the `Touched` setup, as in `Delta`: the elements changed before the
//!   script are `added` / `modified` there, `base` in the reference.
#![cfg(all(feature = "script", feature = "io"))]

use std::sync::Arc;

use graphersal::TraversalGraph;
use graphersal::catalog::{Definition, PropertyIndexDefinition, PropertyIndexTransform};
use graphersal::storage::{ElementKind, Graph, GraphStorage, LayeredGraph};

use crate::foreign_storage_tests::{self as reference_scripts, Mode, Outcome, reference};
use crate::test_storage::{copy_into_delta, layer_over};

/// Where the dataset lives in the layered graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Setup {
    /// The modern graph is the frozen base.
    Base,
    /// The modern graph is in the delta of a layer over an empty base.
    Delta,
    /// The index fixture only: a base with filler vertices, a few percent of it changed in the
    /// layer.
    Touched,
}

const SETUPS: [Setup; 2] = [Setup::Base, Setup::Delta];
const INDEX_SETUPS: [Setup; 3] = [Setup::Base, Setup::Delta, Setup::Touched];

/// What a differential compares: the reference per setup, the layered graph per setup, and
/// whether the index overlays are checked after every script.
#[derive(Clone, Copy)]
struct Fixture {
    setups: &'static [Setup],
    reference: fn(Setup) -> Arc<Graph<TraversalGraph>>,
    layered: fn(Setup) -> Arc<Graph<LayeredGraph>>,
    check_indexes: bool,
}

/// The modern graph, as the lists were written for.
const MODERN: Fixture = Fixture {
    setups: &SETUPS,
    reference: |_| reference(),
    layered,
    check_indexes: false,
};

/// The modern graph with seven property indexes (module docs).
const INDEXED: Fixture = Fixture {
    setups: &INDEX_SETUPS,
    reference: indexed_reference,
    layered: indexed_layered,
    check_indexes: true,
};

impl Default for Fixture {
    fn default() -> Self {
        MODERN
    }
}
const MODES: [Mode; 3] = [Mode::Plain, Mode::Atomic, Mode::DryRun];

/// The modern graph as a layered graph in `setup`.
fn layered(setup: Setup) -> Arc<Graph<LayeredGraph>> {
    let modern = TraversalGraph::tinkerpop_modern();
    Arc::new(Graph::new(match setup {
        Setup::Base => layer_over(modern),
        Setup::Delta => copy_into_delta(&modern),
        Setup::Touched => panic!("the modern fixture has no Touched setup"),
    }))
}

/// The index definitions of the index fixture: per label and label-less, vertices and edges,
/// `to_lower` and `each`.
fn index_definitions() -> Vec<Definition> {
    [
        (
            "age",
            PropertyIndexDefinition::new(ElementKind::Vertex, "person", "age"),
        ),
        (
            "lang",
            PropertyIndexDefinition::new(ElementKind::Vertex, "software", "lang"),
        ),
        (
            "name",
            PropertyIndexDefinition::label_less(ElementKind::Vertex, "name"),
        ),
        (
            "name_lower",
            PropertyIndexDefinition::new(ElementKind::Vertex, "person", "name")
                .with_transform(Some(PropertyIndexTransform::ToLower)),
        ),
        (
            "tags",
            PropertyIndexDefinition::new(ElementKind::Vertex, "person", "tags")
                .with_transform(Some(PropertyIndexTransform::Each)),
        ),
        (
            "knows_weight",
            PropertyIndexDefinition::new(ElementKind::Edge, "knows", "weight"),
        ),
        (
            "weight",
            PropertyIndexDefinition::label_less(ElementKind::Edge, "weight"),
        ),
    ]
    .into_iter()
    .map(|(name, definition)| Definition::property_index(name, definition))
    .collect()
}

/// `graph` with the index fixture's definitions.
fn with_indexes(mut graph: TraversalGraph) -> TraversalGraph {
    for definition in index_definitions() {
        graph.set_definition(definition).unwrap();
    }
    graph
}

/// The base of the `Touched` setup: the indexed modern graph plus 94 filler vertices (100 in
/// all), some of them people with an age.
fn touched_base() -> TraversalGraph {
    let mut graph = TraversalGraph::tinkerpop_modern();
    for i in 0..94_i64 {
        let label = if i % 2 == 0 { "filler" } else { "person" };
        let mut properties = graphersal::prelude::PropertyMap::default();
        properties.insert("name", format!("Filler{i}").as_str().into());
        properties.insert("age", (40 + i % 7).into());
        graph
            .insert_vertex(None, vec![label.into()], properties)
            .unwrap();
    }
    with_indexes(graph)
}

/// The `Touched` layer: a few percent of its base changed before any script (an image of a
/// base person, an added person, a dropped filler, an imaged base edge).
fn touched_layer() -> LayeredGraph {
    let mut layer = layer_over(touched_base());
    let marko = layer.get_vertex_by_id("1").unwrap().unwrap();
    layer
        .set_vertex_property(marko, "age".into(), 30.into())
        .unwrap();
    let mut properties = graphersal::prelude::PropertyMap::default();
    properties.insert("name", "Ada".into());
    properties.insert("age", 30.into());
    layer
        .insert_vertex(None, vec!["person".into()], properties)
        .unwrap();
    let filler = layer.vertices_by_label("filler").unwrap().next().unwrap();
    layer.drop_vertex(filler).unwrap();
    let knows = layer.get_edge_by_id("0").unwrap().unwrap();
    layer
        .set_edge_property(knows, "weight".into(), 0.75.into())
        .unwrap();
    layer.check_layer_indexes().unwrap();
    layer
}

/// The reference of the index fixture: the indexed modern graph, or for `Touched` a
/// `TraversalGraph` built from the touched layer's data.
fn indexed_reference(setup: Setup) -> Arc<Graph<TraversalGraph>> {
    Arc::new(Graph::new(match setup {
        Setup::Touched => crate::layered_index_tests::materialize(&touched_layer()),
        _ => with_indexes(TraversalGraph::tinkerpop_modern()),
    }))
}

/// The index fixture as a layered graph in `setup`.
fn indexed_layered(setup: Setup) -> Arc<Graph<LayeredGraph>> {
    Arc::new(Graph::new(match setup {
        Setup::Base => layer_over(with_indexes(TraversalGraph::tinkerpop_modern())),
        Setup::Delta => indexed_delta(),
        Setup::Touched => touched_layer(),
    }))
}

/// A layer over an empty base holding only the index definitions, the modern graph's data in
/// its delta (vertices, then edges, in the source's scan order, as `copy_into_delta`).
fn indexed_delta() -> LayeredGraph {
    let source = TraversalGraph::tinkerpop_modern();
    let mut layer = layer_over(with_indexes(TraversalGraph::new()));
    let mut vertices = std::collections::HashMap::new();
    for node in source.vertices().unwrap() {
        let id = source.vertex_id(node).unwrap().map(Into::into);
        let labels = source.vertex_labels(node).unwrap().to_vec();
        let properties = source.vertex_properties(node).unwrap().cloned();
        let copy = layer
            .insert_vertex(id, labels, properties.unwrap_or_default())
            .unwrap();
        vertices.insert(node, copy);
    }
    for edge in source.edges().unwrap() {
        let id = source.edge_id(edge).unwrap().map(Into::into);
        let label = source.edge_label(edge).unwrap().map(Into::into);
        let (from, to) = source.edge_endpoints(edge).unwrap().unwrap();
        let properties = source.edge_properties(edge).unwrap().cloned();
        layer
            .insert_edge(
                id,
                label,
                vertices[&from],
                vertices[&to],
                properties.unwrap_or_default(),
            )
            .unwrap();
    }
    layer
}

/// Whether `script` lists the indexes with their stats (`g.indexes()`).
fn lists_indexes(script: &str) -> bool {
    script.contains("indexes(")
}

/// `text` with the figures of the `bytes` and `distinct_values` stats blanked.
fn without_index_sizes(text: &Result<String, String>) -> Result<String, String> {
    text.clone().map(|text| {
        let mut out = String::with_capacity(text.len());
        let mut rest = text.as_str();
        while let Some(at) = ["\"bytes\": ", "\"distinct_values\": "]
            .iter()
            .filter_map(|needle| rest.find(needle).map(|at| (at, needle.len())))
            .min()
        {
            let (at, len) = at;
            out.push_str(&rest[..at + len]);
            rest = rest[at + len..].trim_start_matches(|c: char| c.is_ascii_digit());
            out.push('#');
        }
        out.push_str(rest);
        out
    })
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

/// Whether `script` defines or estimates what plan decision P5 refuses on a layer (property
/// indexes are accepted since S4.4: a refused `create_index` is a difference).
fn defines_p5_kind(script: &str) -> bool {
    [
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

/// Whether `script` asks for layer origins (`layer()`, `has_layer()`, `layer_diff()`), whose
/// answers differ by design in the `Delta` setup, where every element is an added one.
fn asks_origins(script: &str) -> bool {
    script.contains("layer(") || script.contains("layer_diff(")
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

/// [`reference_scripts::run_curated`] with the commit position counted from the graph's
/// position before the script (the index fixture's reference committed its index definitions,
/// a layer starts at 0).
fn run_relative<S>(graph: Arc<Graph<S>>, mode: Mode, script: &str) -> Outcome
where
    S: GraphStorage + Default + Send + Sync + 'static,
{
    let start = graph.read().last_commit_seq().unwrap();
    let mut outcome = reference_scripts::run_curated(graph, mode, script);
    outcome.last_commit_seq -= start;
    outcome
}

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
        let (expected, actual) = (format!("{:?}", self.expected), format!("{:?}", self.actual));
        // Where the two outcomes part, with some context (the outcomes are long).
        let at = expected
            .char_indices()
            .zip(actual.chars())
            .find(|((_, a), b)| a != b)
            .map_or(expected.len().min(actual.len()), |((at, _), _)| at);
        let from = expected[..at]
            .char_indices()
            .rev()
            .nth(80)
            .map_or(0, |(i, _)| i);
        let around = |text: &str| {
            text[from.min(text.len())..]
                .chars()
                .take(240)
                .collect::<String>()
        };
        format!(
            "[{:?} {:?} {}] `{}`\n  TraversalGraph: {expected}\n  LayeredGraph:   {actual}\n  \
             first difference: ...{}\n                vs ...{}",
            self.setup,
            self.mode,
            self.list,
            self.script,
            around(&expected),
            around(&actual)
        )
    }
}

/// The differences of one list, sorted into the accepted classes and the rest.
#[derive(Default)]
struct Differences {
    fixture: Fixture,
    /// Scripts compared (script x mode x setup).
    compared: usize,
    index_stats: Vec<String>,
    order_only: Vec<String>,
    p5_refusals: Vec<String>,
    origin_queries: Vec<String>,
    later_tasks: Vec<String>,
    unexplained: Vec<String>,
}

impl Differences {
    /// Runs `script` (of `list`) in every mode on the reference and on both layered setups.
    fn compare(&mut self, list: &'static str, script: &str) {
        let fixture = self.fixture;
        for mode in MODES {
            let mut expected = None;
            let mut expected_for = None;
            for &setup in fixture.setups {
                self.compared += 1;
                // The reference is the same for every setup but `Touched`.
                let key = setup == Setup::Touched;
                if expected_for != Some(key) {
                    expected = Some(run_relative((fixture.reference)(setup), mode, script));
                    expected_for = Some(key);
                }
                let Some(expected) = expected.as_ref() else {
                    continue;
                };
                let layer = (fixture.layered)(setup);
                let actual = run_relative(Arc::clone(&layer), mode, script);
                if fixture.check_indexes
                    && let Err(problem) = layer.read().check_layer_indexes()
                {
                    self.unexplained.push(format!(
                        "[{setup:?} {mode:?} {list}] `{script}`: overlays: {problem}"
                    ));
                }
                if *expected == actual {
                    continue;
                }
                let difference = Difference {
                    setup,
                    mode,
                    list,
                    script: script.to_string(),
                    expected: run_relative((fixture.reference)(setup), mode, script),
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
        if matches!(difference.setup, Setup::Delta | Setup::Touched)
            && asks_origins(script)
            && expected.changes == actual.changes
            && expected.state == actual.state
            && expected.last_commit_seq == actual.last_commit_seq
        {
            self.origin_queries.push(difference.describe());
            return;
        }
        if lists_indexes(script)
            && without_index_sizes(&expected.result) == without_index_sizes(&actual.result)
            && expected.changes == actual.changes
            && expected.state == actual.state
            && expected.last_commit_seq == actual.last_commit_seq
        {
            self.index_stats.push(difference.describe());
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
            let plain = (self.fixture.reference)(difference.setup);
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
             origin queries (Delta, Touched): {}; index stats: {}; left to later tasks: {}",
            self.compared,
            self.order_only.len(),
            self.p5_refusals.len(),
            self.origin_queries.len(),
            self.index_stats.len(),
            self.later_tasks.len()
        );
        let lines = self
            .order_only
            .iter()
            .chain(&self.p5_refusals)
            .chain(&self.index_stats);
        for line in lines.chain(&self.origin_queries).chain(&self.later_tasks) {
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

// ---------------------------------------------------------------------------------------------
// The index fixture (S4.2)
// ---------------------------------------------------------------------------------------------

/// Curated index scripts: every lookup shape the overlays serve (equality, range, `each`,
/// label-less, `to_lower`, counts from the postings, edges), before and after writes of every
/// kind in the same script.
const INDEX_SCRIPTS: &[&str] = &[
    r#"g.V().has("name", "marko").values("age").toList()"#,
    r#"g.V().has("name", "marko").count().next()"#,
    r#"g.V().has("name", "Ada").id().toList()"#,
    r#"g.V().hasLabel("person").has("age", 30).values("name").toList()"#,
    r#"g.V().hasLabel("person").has("age", P.gte(30)).values("name").toList()"#,
    r#"g.V().hasLabel("person").has("age", P.between(27, 33)).count().next()"#,
    r#"g.V().hasLabel("person").has("age", P.within(29, 30, 32)).id().toList()"#,
    r#"g.V().hasLabel("person").has("name", __.to_lower().is(P.eq("marko"))).id().toList()"#,
    r#"g.V().hasLabel("person").has("name", __.to_lower().is(P.eq("marko"))).count().next()"#,
    r#"g.V().hasLabel("software").has("lang", "java").values("name").toList()"#,
    r#"g.E().has("weight", 0.5).id().toList()"#,
    r#"g.E().hasLabel("knows").has("weight", P.lt(0.8)).id().toList()"#,
    r#"g.E().has("weight", P.gt(0.3)).count().next()"#,
    r#"g.addV("person").property("name", "MARKO").property("age", 30).iterate(); [g.V().has("name", "MARKO").id().toList(), g.V().hasLabel("person").has("name", __.to_lower().is(P.eq("marko"))).id().toList(), g.V().hasLabel("person").has("age", 30).count().next()]"#,
    r#"g.V().has("name", "vadas").property("age", 30).iterate(); [g.V().hasLabel("person").has("age", 30).values("name").toList(), g.V().hasLabel("person").has("age", 27).count().next()]"#,
    r#"g.V().has("name", "josh").drop().iterate(); [g.V().hasLabel("person").has("age", P.gt(0)).values("name").toList(), g.V().has("name", "josh").count().next(), g.E().has("weight", 1.0).count().next()]"#,
    r#"g.V("1").property("tags", ["a", "b", "a"]).iterate(); g.V("2").property("tags", ["b"]).iterate(); g.addV("person").property("tags", ["c", "b"]).iterate(); [g.V().hasLabel("person").has("tags", __.unfold().is(P.eq("b"))).id().toList(), g.V().hasLabel("person").has("tags", __.unfold().is(P.eq("b"))).count().next(), g.V().hasLabel("person").has("tags", __.unfold().is(P.within("a", "c"))).id().toList()]"#,
    r#"g.V("1").drop_label("person").iterate(); g.V("3").add_label("person").iterate(); [g.V().hasLabel("person").has("age", P.gte(0)).values("name").toList(), g.V().hasLabel("person").has("name", __.to_lower().is(P.eq("lop"))).count().next()]"#,
    r#"g.V("1").property("name", "Marko").iterate(); g.V("2").properties("name").drop().iterate(); [g.V().has("name", "marko").count().next(), g.V().has("name", "Marko").id().toList(), g.V().hasLabel("person").has("name", __.to_lower().is(P.eq("marko"))).id().toList()]"#,
    r#"g.E("2").property("weight", 0.5).iterate(); g.E("0").set_label("likes").iterate(); g.V("1").addE("knows").to(__.V("3")).property("weight", 0.5).iterate(); [g.E().hasLabel("knows").has("weight", 0.5).id().toList(), g.E().has("weight", 0.5).count().next(), g.E().hasLabel("knows").has("weight", P.gte(0.0)).count().next()]"#,
    r#"g.V().hasLabel("person").property("age", 50).iterate(); [g.V().hasLabel("person").has("age", 50).count().next(), g.V().hasLabel("person").has("age", P.lt(50)).count().next()]"#,
    r#"g.indexes().map(|i| [i.name, i.entries, i.skipped])"#,
    r#"g.V("1").property("name", "Mx").iterate(); g.indexes()"#,
];

/// Curated index definitions (S4.4): create (per label, label-less, a transform, unique),
/// drop (also of an inherited index), redefine (the other index kind), a unique define refused
/// over repeated values formed by a base and a delta value, accepted once the layer dropped
/// one of them, its enforcement; each with lookups and the index list after it.
const INDEX_DEFINITION_SCRIPTS: &[&str] = &[
    r#"g.create_index(#{label: "person", key: "name"}); [g.indexes().map(|i| [i.name, i.entries, i.skipped]), g.V().hasLabel("person").has("name", "josh").id().toList()]"#,
    r#"g.create_index(#{key: "lang", transform: "to_lower"}); g.addV("software").property("lang", "JAVA").iterate(); [g.V().has("lang", __.to_lower().is(P.eq("java"))).id().toList(), g.V().has("lang", __.to_lower().is(P.eq("java"))).count().next()]"#,
    r#"g.create_index(#{label: "created", key: "weight", element: "edge"}); g.E("1").property("weight", 0.9).iterate(); [g.E().hasLabel("created").has("weight", P.gte(0.4)).id().toList(), g.indexes().map(|i| [i.name, i.entries])]"#,
    r#"let dropped = g.drop_index("age"); [dropped, g.indexes().map(|i| i.name), g.V().hasLabel("person").has("age", 29).values("name").toList()]"#,
    r#"g.drop_index("name"); g.V("1").property("name", "Marko").iterate(); [g.V().has("name", "Marko").id().toList(), g.V().has("name", "marko").count().next(), g.indexes().map(|i| i.name)]"#,
    r#"g.create_index(#{key: "age", name: "age"}); [g.indexes().map(|i| [i.name, i.label, i.entries]), g.V().has("age", 32).id().toList(), g.V().has("age", P.lt(30)).count().next()]"#,
    r#"g.create_index(#{label: "person", key: "name", unique: true}); g.addV("person").property("name", "josh").iterate()"#,
    r#"g.addV("person").property("name", "marko").iterate(); g.create_index(#{label: "person", key: "name", unique: true})"#,
    r#"g.V("1").drop().iterate(); g.addV("person").property("name", "marko").iterate(); g.create_index(#{label: "person", key: "name", unique: true}); [g.indexes().map(|i| [i.name, i.unique, i.entries]), g.V().hasLabel("person").has("name", "marko").count().next()]"#,
    r#"g.create_index(#{label: "person", key: "name", unique: true}); g.V("4").property("name", "peter").iterate()"#,
    r#"g.create_index(#{label: "person", key: "name", unique: true}); g.drop_index("person_name"); g.addV("person").property("name", "josh").iterate(); g.V().hasLabel("person").has("name", "josh").count().next()"#,
    r#"g.create_index(#{label: "person", key: "age"}); g.drop_index("person_age"); g.create_index(#{label: "person", key: "age", name: "person_age"}); g.V().hasLabel("person").property("age", 40).iterate(); g.V().hasLabel("person").has("age", 40).count().next()"#,
];

#[test]
fn curated_index_definitions_give_the_same_result_on_a_layer() {
    for fixture in [
        Fixture {
            check_indexes: true,
            ..MODERN
        },
        INDEXED,
    ] {
        let mut differences = Differences {
            fixture,
            ..Differences::default()
        };
        for script in INDEX_DEFINITION_SCRIPTS {
            differences.compare("INDEX_DEFINITION_SCRIPTS", script);
        }
        assert!(
            differences.p5_refusals.is_empty(),
            "{:?}",
            differences.p5_refusals
        );
        differences.check(
            "index definitions",
            INDEX_DEFINITION_SCRIPTS.len() * 3 * fixture.setups.len(),
        );
    }
}

#[test]
fn every_documented_example_gives_the_same_result_on_an_indexed_layer() {
    let mut differences = Differences {
        fixture: INDEXED,
        ..Differences::default()
    };
    for (_receiver, _name, example, result) in graphersal::script::doc_examples() {
        if result.is_none() {
            continue;
        }
        differences.compare("doc examples", example);
    }
    differences.check("doc examples, indexed", 200 * 9);
}

#[test]
fn curated_index_scripts_give_the_same_result_on_an_indexed_layer() {
    let mut differences = Differences {
        fixture: INDEXED,
        ..Differences::default()
    };
    for script in INDEX_SCRIPTS {
        differences.compare("INDEX_SCRIPTS", script);
    }
    for (_mode, script) in reference_scripts::CURATED {
        differences.compare("CURATED", script);
    }
    differences.check(
        "curated scripts, indexed",
        (INDEX_SCRIPTS.len() + reference_scripts::CURATED.len()) * 9,
    );
}
