//! "Freeze the structure you have": `set_schema(infer_schema(), mode)` in Rust
//! (`GraphSchema::with_mode`) and in the DSL (`g.set_schema(g.infer_schema(), SchemaMode.closed)`,
//! the `SchemaMode` token, the mode strings).
//!
//! The guarantee: the inferred schema applies in `open` and in `closed` mode to the data it was
//! inferred from (modern, large, nested objects/arrays/unions, multi-label vertices, generated
//! graphs). The one exception is `closed` with unlabeled vertices or edges, which nothing
//! declares: the call fails, changes nothing, and the report and help name them.
#![cfg(all(feature = "script", feature = "serde"))]

use std::sync::Arc;

use crate::script_host::eval_value;
use graphersal::prelude::*;
use graphersal::schema::SchemaViolationReport;
use graphersal::schema::{UNLABELED, ViolationKind};

/// Infers the schema of `graph` and stores it with `mode`.
fn freeze(graph: &mut TraversalGraph, mode: SchemaMode) -> TraverserResult<()> {
    let frozen = graph.traversal().infer_schema()?.with_mode(mode);
    graph.traversal_mut().set_schema(frozen)
}

/// Freezes a fresh graph from `build` in each mode; both succeed.
fn assert_freezes(build: impl Fn() -> TraversalGraph) {
    for mode in [SchemaMode::Open, SchemaMode::Closed] {
        let mut graph = build();
        freeze(&mut graph, mode).unwrap_or_else(|err| panic!("{mode}: {err}"));
        let stored = graph.traversal().get_schema().unwrap().unwrap();
        assert_eq!(stored.mode, mode);
    }
}

fn report_of(err: &TraverserError) -> &SchemaViolationReport {
    match err {
        TraverserError::Graph(GraphError::Schema(schema)) => schema
            .violation_report()
            .unwrap_or_else(|| panic!("not an aggregated report: {err}")),
        other => panic!("expected a schema error, got {other}"),
    }
}

fn obj(entries: Vec<(&str, ElementProperty)>) -> ElementProperty {
    let mut map = PropertyMap::default();
    for (k, v) in entries {
        map.insert(k, v);
    }
    ElementProperty::Object(Box::new(map))
}

fn props(entries: Vec<(&str, ElementProperty)>) -> PropertyMap {
    let mut map = PropertyMap::default();
    for (k, v) in entries {
        map.insert(k, v);
    }
    map
}

fn labels(names: &[&str]) -> Vec<compact_str::CompactString> {
    names.iter().map(|n| (*n).into()).collect()
}

#[test]
fn modern_freezes_open_and_closed() {
    assert_freezes(TraversalGraph::tinkerpop_modern);
    // Closed admits nothing else afterwards.
    let mut graph = TraversalGraph::tinkerpop_modern();
    freeze(&mut graph, SchemaMode::Closed).unwrap();
    assert!(graph.traversal().add_v("robot").to_list().is_err());
    assert!(
        graph
            .traversal()
            .v(None)
            .has_label("person")
            .property("shoe_size", 42)
            .to_list()
            .is_err()
    );
}

fn nested() -> TraversalGraph {
    let mut graph = TraversalGraph::new();
    let uuid = ElementProperty::Uuid(0x123e4567_e89b_12d3_a456_426614174000);
    let a = graph
        .add_vertex(
            None,
            labels(&["person", "admin"]),
            props(vec![
                ("name", "a".into()),
                (
                    "tags",
                    ElementProperty::Array(vec![
                        "x".into(),
                        1.into(),
                        2.5.into(),
                        ElementProperty::Null,
                        obj(vec![("k", 1.into())]),
                    ]),
                ),
                (
                    "addr",
                    obj(vec![
                        ("zip", "1".into()),
                        ("geo", obj(vec![("lat", 1.5.into())])),
                    ]),
                ),
            ]),
        )
        .unwrap();
    let b = graph
        .add_vertex(
            None,
            labels(&["person"]),
            props(vec![
                ("name", 3.into()),
                ("addr", ElementProperty::Null),
                ("u", uuid.clone()),
            ]),
        )
        .unwrap();
    graph
        .add_vertex(
            None,
            labels(&["person"]),
            props(vec![
                ("u", "plain".into()),
                ("tags", ElementProperty::Array(vec![])),
                (
                    "addr",
                    obj(vec![
                        ("zip", 5.into()),
                        (
                            "more",
                            ElementProperty::Array(vec![
                                obj(vec![("a", 1.into())]),
                                obj(vec![("b", 2.into())]),
                                ElementProperty::Array(vec![1.into()]),
                            ]),
                        ),
                    ]),
                ),
            ]),
        )
        .unwrap();
    let c = graph
        .add_vertex(
            None,
            labels(&["admin"]),
            props(vec![
                ("big", i64::MAX.into()),
                ("f", 1.0e308.into()),
                ("empty", obj(vec![])),
            ]),
        )
        .unwrap();
    graph
        .add_edge(
            None,
            Some("knows".into()),
            a,
            b,
            props(vec![("w", 1.into()), ("m", obj(vec![("x", uuid)]))]),
        )
        .unwrap();
    graph
        .add_edge(
            None,
            Some("knows".into()),
            c,
            a,
            props(vec![("w", 0.5.into())]),
        )
        .unwrap();
    graph
        .add_edge(None, Some("rel".into()), b, b, PropertyMap::default())
        .unwrap();
    graph
}

#[test]
fn nested_union_and_multi_label_data_freezes() {
    assert_freezes(nested);
}

/// A small deterministic generator (an LCG): random label sets, random values up to depth 3.
struct Gen(u64);

impl Gen {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn value(&mut self, depth: u32) -> ElementProperty {
        let kinds = if depth >= 3 { 6 } else { 8 };
        match self.below(kinds) {
            0 => ElementProperty::Null,
            1 => (self.below(2) == 0).into(),
            2 => (self.below(100) as i64 - 50).into(),
            3 => (self.below(1000) as f64 / 7.0).into(),
            4 => format!("s{}", self.below(10)).into(),
            5 => ElementProperty::Uuid(u128::from(self.next())),
            6 => {
                let n = self.below(4);
                ElementProperty::Array((0..n).map(|_| self.value(depth + 1)).collect())
            }
            _ => {
                let n = self.below(4);
                let mut map = PropertyMap::default();
                for _ in 0..n {
                    let key = format!("k{}", self.below(4));
                    map.insert(key, self.value(depth + 1));
                }
                ElementProperty::Object(Box::new(map))
            }
        }
    }

    fn properties(&mut self) -> PropertyMap {
        let mut map = PropertyMap::default();
        for _ in 0..self.below(5) {
            let key = format!("p{}", self.below(6));
            map.insert(key, self.value(0));
        }
        map
    }
}

fn generated(seed: u64) -> TraversalGraph {
    const LABELS: [&str; 4] = ["A", "B", "C", "D"];
    let mut rng = Gen(seed);
    let mut graph = TraversalGraph::new();
    let mut vertices = Vec::new();
    for _ in 0..30 {
        let mut set = Vec::new();
        // At least one label: unlabeled elements are the documented exception.
        for _ in 0..=rng.below(3) {
            set.push(LABELS[rng.below(4) as usize].into());
        }
        let props = rng.properties();
        vertices.push(graph.add_vertex(None, set, props).unwrap());
    }
    for _ in 0..40 {
        let from = vertices[rng.below(30) as usize];
        let to = vertices[rng.below(30) as usize];
        let label = ["e", "f"][rng.below(2) as usize];
        let props = rng.properties();
        graph
            .add_edge(None, Some(label.into()), from, to, props)
            .unwrap();
    }
    graph
}

#[test]
fn generated_graphs_freeze() {
    for seed in 0..40u64 {
        for mode in [SchemaMode::Open, SchemaMode::Closed] {
            let mut graph = generated(seed);
            freeze(&mut graph, mode).unwrap_or_else(|err| panic!("seed {seed}, {mode}: {err}"));
        }
    }
}

/// A graph with an unlabeled vertex, an unlabeled edge and an edge into the unlabeled vertex.
fn with_unlabeled() -> TraversalGraph {
    let mut graph = TraversalGraph::new();
    let a = graph
        .add_vertex(None, labels(&["a"]), props(vec![("x", 1.into())]))
        .unwrap();
    let b = graph
        .add_vertex(None, labels(&["b"]), PropertyMap::default())
        .unwrap();
    let bare = graph
        .add_vertex(Some("bare".into()), vec![], props(vec![("y", 2.into())]))
        .unwrap();
    graph
        .add_edge(None, Some("e".into()), a, b, PropertyMap::default())
        .unwrap();
    graph
        .add_edge(Some("nolabel".into()), None, a, b, PropertyMap::default())
        .unwrap();
    graph
        .add_edge(
            Some("into-bare".into()),
            Some("e".into()),
            a,
            bare,
            PropertyMap::default(),
        )
        .unwrap();
    graph
}

#[test]
fn unlabeled_elements_freeze_open_but_closed_names_them() {
    let mut graph = with_unlabeled();
    freeze(&mut graph, SchemaMode::Open).unwrap();

    let mut graph = with_unlabeled();
    let err = freeze(&mut graph, SchemaMode::Closed).unwrap_err();
    // Nothing changed.
    assert!(graph.traversal().get_schema().unwrap().is_none());
    let report = report_of(&err);
    let unlabeled = |element: &str| {
        report
            .groups
            .iter()
            .find(|g| {
                g.kind == ViolationKind::UnknownLabel
                    && g.label == UNLABELED
                    && g.element == element
            })
            .unwrap_or_else(|| panic!("no unlabeled {element} group: {report}"))
    };
    assert_eq!(unlabeled("vertex").sample_ids, vec!["bare".to_string()]);
    assert_eq!(unlabeled("edge").sample_ids, vec!["nolabel".to_string()]);
    let text = report.to_string();
    assert!(
        text.contains(
            "1 vertex without a label (Closed admits only declared labels) [ids: \"bare\"]"
        ),
        "{text}"
    );
    assert!(text.contains("1 edge without a label"), "{text}");
    // The edge into the unlabeled vertex: its endpoint renders as `()`.
    assert!(text.contains("(:a)-[:e]->()"), "{text}");
    let diagnostic = graphersal::error::format_diagnostic(&*err);
    assert!(
        diagnostic.contains("Elements without a label"),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains("g.set_schema(g.infer_schema(), SchemaMode.open)"),
        "{diagnostic}"
    );
}

#[test]
fn large_graph_freezes_open_and_reports_its_unlabeled_edges_closed() {
    let graph = GraphSource::large_110k();
    let mut guard = graph.write();
    freeze(&mut guard, SchemaMode::Open).unwrap();
    // The generated graph's edges carry no label: closed fails with exactly that group.
    let err = freeze(&mut guard, SchemaMode::Closed).unwrap_err();
    let report = report_of(&err);
    assert_eq!(report.groups.len(), 1, "{report}");
    assert_eq!(report.groups[0].label, UNLABELED);
    assert_eq!(report.groups[0].element, "edge");
    assert_eq!(
        guard.traversal().get_schema().unwrap().unwrap().mode,
        SchemaMode::Open
    );
}

// ------------------------------------------------------------------------------------ DSL

fn run(graph: &Arc<Graph>, script: &str) -> Result<String, String> {
    eval_value(graph.clone(), script)
        .map(|v| v.to_string())
        .map_err(|err| err.to_string())
}

#[test]
fn dsl_one_liner_in_every_spelling() {
    for (call, mode) in [
        (
            "g.set_schema(g.infer_schema(), SchemaMode.closed)",
            "closed",
        ),
        (
            "g.set_schema(g.infer_schema(), SchemaMode::closed)",
            "closed",
        ),
        ("g.set_schema(g.infer_schema(), SchemaMode.open)", "open"),
        ("g.set_schema(g.infer_schema(), \"closed\")", "closed"),
        ("g.set_schema(g.infer_schema(), \"open\")", "open"),
        ("g.set_schema(g.infer_schema(), SchemaMode.none)", "none"),
    ] {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        run(&graph, call).unwrap_or_else(|err| panic!("{call}: {err}"));
        let stored = graph.read().get_schema().unwrap();
        match mode {
            // An inferred schema in mode none is still stored (it declares labels).
            "none" => assert_eq!(stored.unwrap().mode, SchemaMode::None),
            "open" => assert_eq!(stored.unwrap().mode, SchemaMode::Open),
            _ => assert_eq!(stored.unwrap().mode, SchemaMode::Closed),
        }
    }
}

#[test]
fn dsl_mode_overrides_the_schemas_own_mode() {
    let graph = Arc::new(GraphSource::tinkerpop_modern());
    run(
        &graph,
        r#"let s = g.infer_schema(); s.mode = "closed"; g.set_schema(s, SchemaMode.open)"#,
    )
    .unwrap();
    assert_eq!(
        graph.read().get_schema().unwrap().unwrap().mode,
        SchemaMode::Open
    );
    // JSON text works too.
    run(
        &graph,
        r#"g.set_schema(`{"mode": "closed", "vertices": {"person": {}}}`, "open")"#,
    )
    .unwrap();
    assert_eq!(
        graph.read().get_schema().unwrap().unwrap().mode,
        SchemaMode::Open
    );
}

#[test]
fn dsl_validate_schema_with_mode_is_the_dry_run() {
    let graph = Arc::new(GraphSource::empty());
    let out = run(
        &graph,
        r#"g.add_v().next(); g.add_v("a").next();
           let r = g.validate_schema(g.infer_schema(), SchemaMode.closed);
           [r.violations.mode, r.violations.total_violations, r.violations.groups[0].label,
            g.validate_schema(g.infer_schema(), "open").violations.total_violations]"#,
    )
    .unwrap();
    assert_eq!(out, r#"["closed", 1, "<unlabeled>", 0]"#);
    assert!(graph.read().get_schema().unwrap().is_none());
}

#[test]
fn dsl_rejects_an_unknown_mode() {
    let graph = Arc::new(GraphSource::tinkerpop_modern());
    let err = run(&graph, r#"g.set_schema(g.infer_schema(), "Closed")"#).unwrap_err();
    assert!(
        err.contains("Invalid schema at /mode: unknown mode 'Closed'"),
        "{err}"
    );
    assert!(err.contains("SchemaMode.closed"), "{err}");
    let err = run(&graph, "g.set_schema(g.infer_schema(), 3)").unwrap_err();
    assert!(err.contains("expecting a schema mode"), "{err}");
    assert!(graph.read().get_schema().unwrap().is_none());
}

#[test]
fn schema_mode_tokens_are_values() {
    let graph = Arc::new(GraphSource::empty());
    let out = run(
        &graph,
        "[SchemaMode.none.to_string(), SchemaMode::open.to_string(), SchemaMode.closed.to_string()]",
    )
    .unwrap();
    assert_eq!(
        out,
        r#"["SchemaMode.none", "SchemaMode.open", "SchemaMode.closed"]"#
    );
}
