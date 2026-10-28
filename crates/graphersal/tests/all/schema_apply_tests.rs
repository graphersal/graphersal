//! Task 24 (decisions C and D): applying a schema to existing data fails with an aggregated,
//! structured report; edge topology accepts ANY label of an endpoint and is re-checked when
//! labels change. Fixture: Person(name!, age!, bio?), Company(name!), Robot(name!),
//! `works_at(since!)` and `member_of()` (Person -> Company).
#![cfg(all(feature = "script", feature = "serde"))]

use std::sync::Arc;

use crate::script_host::eval_value;
use graphersal::prelude::*;
use graphersal::schema::{
    EdgeSchema, PropertySchema, SchemaViolationReport, VertexSchema, ViolationGroup, ViolationKind,
};
use graphersal::{
    schema::{MAX_GROUPS, PRINTED_GROUPS, SAMPLE_ID_CHARS, SAMPLE_IDS},
    storage::VertexHandle,
};

/// A key: `nullable` is the old "optional" (absent or null allowed), otherwise required.
fn field(ty: PropertySchema, nullable: bool) -> (PropertySchema, bool) {
    if nullable {
        (ty.nullable(), false)
    } else {
        (ty, true)
    }
}

fn root(fields: Vec<(&str, (PropertySchema, bool))>) -> PropertySchema {
    crate::schema_support::object(
        fields
            .into_iter()
            .map(|(k, (node, required))| (k, node, required))
            .collect(),
    )
}

fn vertex(_label: &str, fields: Vec<(&str, (PropertySchema, bool))>) -> VertexSchema {
    VertexSchema::new(root(fields))
}

fn edge(_label: &str, fields: Vec<(&str, (PropertySchema, bool))>) -> EdgeSchema {
    EdgeSchema::new(root(fields)).with_connection("Person", "Company")
}

fn schema(mode: SchemaMode) -> GraphSchema {
    let mut s = GraphSchema::new().with_mode(mode);
    s.vertices.insert(
        "Person".into(),
        vertex(
            "Person",
            vec![
                ("name", field(PropertySchema::string(), false)),
                ("age", field(PropertySchema::integer(), false)),
                ("bio", field(PropertySchema::string(), true)),
            ],
        ),
    );
    s.vertices.insert(
        "Company".into(),
        vertex(
            "Company",
            vec![("name", field(PropertySchema::string(), false))],
        ),
    );
    s.vertices.insert(
        "Robot".into(),
        vertex(
            "Robot",
            vec![("name", field(PropertySchema::string(), false))],
        ),
    );
    s.edges.insert(
        "works_at".into(),
        edge(
            "works_at",
            vec![("since", field(PropertySchema::integer(), false))],
        ),
    );
    s.edges
        .insert("member_of".into(), edge("member_of", vec![]));
    s
}

fn props(entries: &[(&str, ElementProperty)]) -> PropertyMap {
    let mut out = PropertyMap::default();
    for (k, v) in entries {
        out.insert(*k, v.clone());
    }
    out
}

fn s(v: &str) -> ElementProperty {
    ElementProperty::String(v.into())
}

fn n(v: i64) -> ElementProperty {
    ElementProperty::from(v)
}

fn report_of(err: &SchemaError) -> &SchemaViolationReport {
    err.violation_report()
        .unwrap_or_else(|| panic!("not an aggregated report: {err}"))
}

fn apply_err(graph: &mut TraversalGraph, schema: GraphSchema) -> SchemaError {
    match graph.set_schema(schema) {
        Err(GraphError::Schema(err)) => err,
        other => panic!("expected a schema error, got {other:?}"),
    }
}

fn group<'a>(
    report: &'a SchemaViolationReport,
    kind: ViolationKind,
    label: &str,
    field: Option<&str>,
) -> &'a ViolationGroup {
    report
        .groups
        .iter()
        .find(|g| g.kind == kind && g.label == label && g.field.as_deref() == field)
        .unwrap_or_else(|| panic!("no group {kind:?} {label} {field:?} in {report}"))
}

/// 12 Persons without `age` (one of them with a 100-char id, one with a quote/newline id), 3
/// Companies without `name`, 5 Aliens, a Person with a string `age`, a Robot with an undeclared
/// property.
fn violating_graph() -> TraversalGraph {
    let mut g = TraversalGraph::new();
    g.add_vertex(
        Some("a".repeat(100).into()),
        vec!["Person".into()],
        props(&[("name", s("long"))]),
    )
    .unwrap();
    g.add_vertex(
        Some("q\"\n\u{1b}x".into()),
        vec!["Person".into()],
        props(&[("name", s("odd"))]),
    )
    .unwrap();
    for i in 0..10 {
        g.add_vertex(
            Some(format!("p{i}").into()),
            vec!["Person".into()],
            props(&[("name", s("x"))]),
        )
        .unwrap();
    }
    for i in 0..3 {
        g.add_vertex(
            Some(format!("c{i}").into()),
            vec!["Company".into()],
            PropertyMap::default(),
        )
        .unwrap();
    }
    for i in 0..5 {
        g.add_vertex(
            Some(format!("x{i}").into()),
            vec!["Alien".into()],
            props(&[("planet", s("Mars"))]),
        )
        .unwrap();
    }
    g.add_vertex(
        Some("t".into()),
        vec!["Person".into()],
        props(&[("name", s("T")), ("age", s("old"))]),
    )
    .unwrap();
    g.add_vertex(
        Some("r".into()),
        vec!["Robot".into()],
        props(&[("name", s("R")), ("model", s("M"))]),
    )
    .unwrap();
    g
}

#[test]
fn aggregated_error_groups_every_kind_with_counts_and_samples() {
    let mut graph = violating_graph();
    let err = apply_err(&mut graph, schema(SchemaMode::Closed));
    let report = report_of(&err);
    assert_eq!(report.mode, SchemaMode::Closed);
    assert_eq!(report.elements_checked, 12 + 3 + 5 + 1 + 1);

    let age = group(
        report,
        ViolationKind::MissingRequired,
        "Person",
        Some("age"),
    );
    assert_eq!(age.count, 12);
    assert_eq!(age.element, "vertex");
    assert_eq!(age.sample_ids.len(), SAMPLE_IDS);
    assert_eq!(age.omitted_ids, 7);
    assert_eq!(
        group(
            report,
            ViolationKind::MissingRequired,
            "Company",
            Some("name")
        )
        .count,
        3
    );
    assert_eq!(
        group(report, ViolationKind::UnknownLabel, "Alien", None).count,
        5
    );
    let ty = group(report, ViolationKind::TypeMismatch, "Person", Some("age"));
    assert_eq!(ty.count, 1);
    assert_eq!(ty.detail.as_deref(), Some("declared int64, found string"));
    assert_eq!(
        group(
            report,
            ViolationKind::UndeclaredProperty,
            "Robot",
            Some("model")
        )
        .count,
        1
    );
    // the Person with a string age also lacks nothing else; 12 + 3 + 5 + 1 + 1 offending elements
    assert_eq!(report.violating_elements, 12 + 3 + 5 + 1 + 1);
    // largest group first
    assert_eq!(report.groups[0].count, 12);

    let text = err.to_string();
    assert!(
        text.contains("Person.age is declared mandatory but is missing on 12 vertices"),
        "{text}"
    );
    assert!(
        text.contains("Company.name is declared mandatory but is missing on 3 vertices"),
        "{text}"
    );
    assert!(
        text.contains("vertex label Alien is not declared (Closed): 5 vertices"),
        "{text}"
    );
    assert!(
        text.contains("Person.age has the wrong type on 1 vertex (declared int64, found string)"),
        "{text}"
    );
    assert!(text.contains("and 7 more"), "{text}");
    assert!(text.starts_with("Cannot apply the Closed schema"), "{text}");
}

#[test]
fn sample_ids_are_truncated_and_escaped_for_display() {
    let mut graph = violating_graph();
    let err = apply_err(&mut graph, schema(SchemaMode::Closed));
    let report = report_of(&err);
    // every sampled id is at most 40 chars + the ellipsis, nowhere near 100
    for g in &report.groups {
        for id in &g.sample_ids {
            assert!(id.chars().count() <= SAMPLE_ID_CHARS + 1, "{id}");
        }
    }
    let age = group(
        report,
        ViolationKind::MissingRequired,
        "Person",
        Some("age"),
    );
    let long = age
        .sample_ids
        .iter()
        .find(|id| id.starts_with("aaaa"))
        .expect("long id sampled");
    assert_eq!(long.chars().count(), SAMPLE_ID_CHARS + 1);
    assert!(long.ends_with('\u{2026}'));
    let text = err.to_string();
    assert!(
        !text.contains(&"a".repeat(41)),
        "the full 100-char id must not be printed"
    );
    // the odd id is escaped in the text: no raw control characters besides the layout newlines
    assert!(text.contains("q\\\"\\n\\u{1b}x"), "{text}");
    assert!(!text.contains('\u{1b}'), "{text}");
}

#[test]
fn printed_groups_are_capped() {
    let mut g = TraversalGraph::new();
    for i in 0..25 {
        g.add_vertex(
            Some(format!("v{i}").into()),
            vec![format!("L{i}").into()],
            PropertyMap::default(),
        )
        .unwrap();
    }
    let err = apply_err(&mut g, schema(SchemaMode::Closed));
    let report = report_of(&err);
    assert_eq!(report.groups.len(), 25, "the structure keeps every group");
    let text = err.to_string();
    assert_eq!(
        text.matches("is not declared (Closed)").count(),
        PRINTED_GROUPS
    );
    assert!(text.contains("and 5 more kinds of violations"), "{text}");
    assert!(text.contains("25 kinds of violations"), "{text}");
}

#[test]
fn group_storage_is_bounded() {
    let mut g = TraversalGraph::new();
    for i in 0..(MAX_GROUPS + 50) {
        g.add_vertex(
            Some(format!("v{i}").into()),
            vec!["Robot".into()],
            props(&[("name", s("R")), (&format!("undeclared{i}"), n(1))]),
        )
        .unwrap();
    }
    let err = apply_err(&mut g, schema(SchemaMode::Closed));
    let report = report_of(&err);
    assert_eq!(report.groups.len(), MAX_GROUPS);
    assert_eq!(report.omitted_violations, 50);
    assert_eq!(report.total_violations, (MAX_GROUPS + 50) as u64);
    assert!(
        err.to_string()
            .contains("50 more violations of further kinds"),
        "{err}"
    );
}

#[test]
fn a_failed_application_leaves_graph_and_schema_unchanged() {
    let mut g = violating_graph();
    let (p0, c0) = (
        g.get_vertex_by_id("p0").unwrap().unwrap(),
        g.get_vertex_by_id("c0").unwrap().unwrap(),
    );
    g.add_edge(None, Some("knows".into()), p0, c0, PropertyMap::default())
        .unwrap();
    let (v, e) = (g.vertex_count(), g.edge_count());
    let before = g.get_schema().unwrap();
    assert_eq!(before, None);
    for mode in [SchemaMode::Open, SchemaMode::Closed] {
        assert!(g.set_schema(schema(mode)).is_err(), "{mode:?}");
        assert_eq!((g.vertex_count(), g.edge_count()), (v, e));
        assert_eq!(
            g.get_schema().unwrap(),
            before,
            "{mode:?}: no schema was stored"
        );
        // the data is untouched: no coercion, no removal
        assert_eq!(
            g.vertex_properties(p0).unwrap().and_then(|p| p.get("name")),
            Some(&s("x"))
        );
    }
    assert!(
        g.set_schema(GraphSchema::from_json(&schema(SchemaMode::Closed).to_json()).unwrap())
            .is_err()
    );
    assert_eq!(g.get_schema().unwrap(), before);

    // the id sequences are not consumed by failed applications
    let mut a = TraversalGraph::new();
    let mut b = TraversalGraph::new();
    for graph in [&mut a, &mut b] {
        graph
            .add_vertex(None, vec!["Alien".into()], PropertyMap::default())
            .unwrap();
    }
    assert!(a.set_schema(schema(SchemaMode::Closed)).is_err());
    let ia = a
        .add_vertex(None, vec!["Alien".into()], PropertyMap::default())
        .unwrap();
    let ib = b
        .add_vertex(None, vec!["Alien".into()], PropertyMap::default())
        .unwrap();
    assert_eq!(a.vertex_id(ia).unwrap(), b.vertex_id(ib).unwrap());
}

#[test]
fn a_failed_application_keeps_an_existing_enforcing_schema() {
    let mut g = TraversalGraph::new();
    g.add_vertex(
        Some("p".into()),
        vec!["Person".into()],
        props(&[("name", s("A")), ("age", n(1))]),
    )
    .unwrap();
    g.set_schema(schema(SchemaMode::Open)).unwrap();
    // a stricter schema the data violates: Closed with Person missing
    let mut strict = schema(SchemaMode::Closed);
    strict.vertices.shift_remove("Person");
    assert!(g.set_schema(strict).is_err());
    assert_eq!(g.get_schema().unwrap().unwrap().mode, SchemaMode::Open);
    assert!(
        g.add_vertex(
            Some("q".into()),
            vec!["Person".into()],
            PropertyMap::default()
        )
        .is_err(),
        "still enforced"
    );
}

#[test]
fn mode_none_applies_without_validation_and_removes_an_enforcing_schema() {
    let mut g = violating_graph();
    g.set_schema(schema(SchemaMode::None)).unwrap();
    assert!(
        g.traversal()
            .validate_schema(&schema(SchemaMode::None))
            .unwrap()
            .is_valid()
    );
    let mut g2 = TraversalGraph::new();
    g2.set_schema(schema(SchemaMode::Closed)).unwrap();
    // The empty schema is no schema.
    g2.set_schema(GraphSchema::new()).unwrap();
    assert_eq!(g2.get_schema().unwrap(), None);
}

#[test]
fn inferred_schemas_apply_cleanly_in_open_and_closed() {
    for mut g in [TraversalGraph::tinkerpop_modern(), violating_graph()] {
        for mode in [SchemaMode::Open, SchemaMode::Closed] {
            let inferred = g.traversal().infer_schema().unwrap().with_mode(mode);
            g.set_schema(inferred)
                .unwrap_or_else(|e| panic!("{mode:?}: {e}"));
        }
    }
}

#[test]
fn open_and_closed_differ_only_in_undeclared_labels_properties_and_topology() {
    // Only the Open-visible problem: the required `age` is missing.
    let mut g = TraversalGraph::new();
    g.add_vertex(
        Some("p".into()),
        vec!["Person".into()],
        props(&[("name", s("A"))]),
    )
    .unwrap();
    let open = apply_err(&mut g, schema(SchemaMode::Open));
    assert_eq!(report_of(&open).groups.len(), 1);
    assert_eq!(
        group(
            report_of(&open),
            ViolationKind::MissingRequired,
            "Person",
            Some("age")
        )
        .count,
        1
    );

    // Closed-only problems: undeclared label, undeclared property, topology.
    let build = || {
        let mut g = TraversalGraph::new();
        g.add_vertex(
            Some("a".into()),
            vec!["Alien".into()],
            props(&[("planet", s("Mars"))]),
        )
        .unwrap();
        let r = g
            .add_vertex(
                Some("r".into()),
                vec!["Robot".into()],
                props(&[("name", s("R")), ("model", s("M"))]),
            )
            .unwrap();
        let c = g
            .add_vertex(
                Some("c".into()),
                vec!["Company".into()],
                props(&[("name", s("C"))]),
            )
            .unwrap();
        g.add_edge(
            Some("e".into()),
            Some("member_of".into()),
            r,
            c,
            PropertyMap::default(),
        )
        .unwrap();
        g.add_edge(
            Some("u".into()),
            Some("unknown_edge".into()),
            r,
            c,
            PropertyMap::default(),
        )
        .unwrap();
        g.add_edge(Some("w".into()), None, r, c, PropertyMap::default())
            .unwrap();
        g
    };
    let mut g = build();
    build()
        .set_schema(schema(SchemaMode::Open))
        .expect("Open allows all of this");
    let closed = apply_err(&mut g, schema(SchemaMode::Closed));
    let report = report_of(&closed);
    assert_eq!(
        group(report, ViolationKind::UnknownLabel, "Alien", None).count,
        1
    );
    assert_eq!(
        group(
            report,
            ViolationKind::UndeclaredProperty,
            "Robot",
            Some("model")
        )
        .count,
        1
    );
    let topo = group(
        report,
        ViolationKind::InvalidTopology,
        "member_of",
        Some("(:Robot)-[:member_of]->(:Company)"),
    );
    assert_eq!(topo.element, "edge");
    assert_eq!(
        group(report, ViolationKind::UnknownLabel, "unknown_edge", None).element,
        "edge"
    );
    assert_eq!(
        group(report, ViolationKind::UnknownLabel, "<unlabeled>", None).count,
        1
    );
    let text = closed.to_string();
    assert!(text.contains("edge member_of (:Robot)-[:member_of]->(:Company) is not a declared connection (Closed): 1 edge"), "{text}");
}

// ---------------------------------------------------------------------------------------------
// Nested paths
// ---------------------------------------------------------------------------------------------

fn nested_schema(mode: SchemaMode) -> GraphSchema {
    let address = root(vec![
        ("zip", field(PropertySchema::string(), false)),
        ("floor", field(PropertySchema::integer(), true)),
    ]);
    let mut s = GraphSchema::new().with_mode(mode);
    s.vertices.insert(
        "Person".into(),
        vertex("Person", vec![("address", field(address, false))]),
    );
    s
}

fn object(entries: &[(&str, ElementProperty)]) -> ElementProperty {
    ElementProperty::Object(Box::new(props(entries)))
}

#[test]
fn nested_path_violations_are_reported_with_the_canonical_path() {
    let mut g = TraversalGraph::new();
    g.add_vertex(
        Some("ok".into()),
        vec!["Person".into()],
        props(&[("address", object(&[("zip", s("1"))]))]),
    )
    .unwrap();
    g.add_vertex(
        Some("nozip".into()),
        vec!["Person".into()],
        props(&[("address", object(&[("floor", n(2))]))]),
    )
    .unwrap();
    g.add_vertex(
        Some("badfloor".into()),
        vec!["Person".into()],
        props(&[("address", object(&[("zip", s("1")), ("floor", s("two"))]))]),
    )
    .unwrap();
    g.add_vertex(
        Some("extra".into()),
        vec!["Person".into()],
        props(&[("address", object(&[("zip", s("1")), ("note", s("x"))]))]),
    )
    .unwrap();
    // Open: the missing required nested key and the nested type are violations, an extra key is not
    let open = apply_err(&mut g, nested_schema(SchemaMode::Open));
    let report = report_of(&open);
    assert_eq!(
        group(
            report,
            ViolationKind::MissingRequired,
            "Person",
            Some("$.address.zip")
        )
        .count,
        1
    );
    assert_eq!(
        group(
            report,
            ViolationKind::TypeMismatch,
            "Person",
            Some("$.address.floor")
        )
        .count,
        1
    );
    assert!(
        report
            .groups
            .iter()
            .all(|g| g.kind != ViolationKind::UndeclaredProperty),
        "{open}"
    );
    assert!(
        open.to_string()
            .contains("Person $.address.zip is declared mandatory but is missing on 1 vertex"),
        "{open}"
    );
    // Closed: the extra nested key too
    let closed = apply_err(&mut g, nested_schema(SchemaMode::Closed));
    assert_eq!(
        group(
            report_of(&closed),
            ViolationKind::UndeclaredProperty,
            "Person",
            Some("$.address.note")
        )
        .count,
        1
    );
}

// ---------------------------------------------------------------------------------------------
// Decision D: topology with label sets
// ---------------------------------------------------------------------------------------------

fn closed_graph() -> TraversalGraph {
    let mut g = TraversalGraph::new();
    g.set_schema(schema(SchemaMode::Closed)).unwrap();
    g
}

fn person(g: &mut TraversalGraph, id: &str, labels: &[&str]) -> VertexHandle {
    g.add_vertex(
        Some(id.into()),
        labels.iter().map(|l| (*l).into()).collect(),
        props(&[("name", s("N")), ("age", n(1))]),
    )
    .unwrap()
}

#[test]
fn topology_accepts_any_label_of_an_endpoint_on_add_edge_and_in_the_apply_scan() {
    for labels in [
        &["Person", "Robot"][..],
        &["Robot", "Person"][..],
        &["Robot", "Person", "Company"][..],
    ] {
        let mut g = closed_graph();
        let a = person(&mut g, "a", labels);
        let c = g
            .add_vertex(
                Some("c".into()),
                vec!["Company".into()],
                props(&[("name", s("C"))]),
            )
            .unwrap();
        g.add_edge(
            Some("e".into()),
            Some("member_of".into()),
            a,
            c,
            PropertyMap::default(),
        )
        .unwrap_or_else(|e| panic!("{labels:?}: {e}"));
        // the same edge passes the scan of a schema applied to existing data
        let mut fresh = {
            let mut f = TraversalGraph::new();
            let a = f
                .add_vertex(
                    Some("a".into()),
                    labels.iter().map(|l| (*l).into()).collect(),
                    props(&[("name", s("N")), ("age", n(1))]),
                )
                .unwrap();
            let c = f
                .add_vertex(
                    Some("c".into()),
                    vec!["Company".into()],
                    props(&[("name", s("C"))]),
                )
                .unwrap();
            f.add_edge(
                Some("e".into()),
                Some("member_of".into()),
                a,
                c,
                PropertyMap::default(),
            )
            .unwrap();
            f
        };
        fresh
            .set_schema(schema(SchemaMode::Closed))
            .unwrap_or_else(|e| panic!("{labels:?}: {e}"));
    }
    // no matching label on either side: rejected, and the text names the label sets
    let mut g = closed_graph();
    let r = g
        .add_vertex(
            Some("r".into()),
            vec!["Robot".into()],
            props(&[("name", s("R"))]),
        )
        .unwrap();
    let c = g
        .add_vertex(
            Some("c".into()),
            vec!["Company".into()],
            props(&[("name", s("C"))]),
        )
        .unwrap();
    let err = g
        .add_edge(
            Some("e".into()),
            Some("member_of".into()),
            r,
            c,
            PropertyMap::default(),
        )
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("(:Robot)-[:member_of]->(:Company)"),
        "{err}"
    );
}

#[test]
fn label_changes_recheck_incident_edge_topology_atomically_in_closed() {
    let mut employee_schema = schema(SchemaMode::Closed);
    employee_schema.vertices.insert(
        "Employee".into(),
        vertex(
            "Employee",
            vec![
                ("name", field(PropertySchema::string(), false)),
                ("age", field(PropertySchema::integer(), true)),
            ],
        ),
    );
    let mut g = TraversalGraph::new();
    g.set_schema(employee_schema).unwrap();
    let a = person(&mut g, "a", &["Person", "Employee"]);
    let c = g
        .add_vertex(
            Some("c".into()),
            vec!["Company".into()],
            props(&[("name", s("C"))]),
        )
        .unwrap();
    let r = g
        .add_vertex(
            Some("r".into()),
            vec!["Robot".into()],
            props(&[("name", s("R"))]),
        )
        .unwrap();
    g.add_edge(
        Some("e".into()),
        Some("member_of".into()),
        a,
        c,
        PropertyMap::default(),
    )
    .unwrap();
    g.add_edge(
        Some("f".into()),
        Some("member_of".into()),
        a,
        c,
        PropertyMap::default(),
    )
    .unwrap();
    // removing Person leaves no matching label on the source of two edges
    let err = g.remove_vertex_label(a, "Person").unwrap_err();
    assert!(err.to_string().contains("not defined in schema"), "{err}");
    assert_eq!(g.vertex_labels(a).unwrap().len(), 2, "unchanged");
    // as target: the Company side
    g.add_edge(
        Some("g".into()),
        Some("member_of".into()),
        a,
        c,
        PropertyMap::default(),
    )
    .unwrap();
    assert!(
        g.remove_vertex_label(c, "Company").is_err(),
        "last label, and the topology"
    );
    // removing the unrelated label is fine
    assert!(g.remove_vertex_label(a, "Employee").unwrap());
    // adding a label never removes a match
    assert!(g.add_vertex_label(r, "Company".into()).unwrap());
    // an edge whose endpoint would lose its only matching label by a label ADD cannot happen, but
    // an unrelated added label is fine; and an added label that makes the TARGET lose nothing is fine
    assert!(g.add_vertex_label(a, "Employee".into()).unwrap());
    // Open mode checks no topology on label changes
    let mut open = TraversalGraph::new();
    open.set_schema(schema(SchemaMode::Open)).unwrap();
    let a = person(&mut open, "a", &["Person", "Robot"]);
    let c = open
        .add_vertex(
            Some("c".into()),
            vec!["Company".into()],
            props(&[("name", s("C"))]),
        )
        .unwrap();
    open.add_edge(
        Some("e".into()),
        Some("member_of".into()),
        a,
        c,
        PropertyMap::default(),
    )
    .unwrap();
    assert!(open.remove_vertex_label(a, "Person").unwrap());
}

// ---------------------------------------------------------------------------------------------
// Entry points: validate_schema, GraphTraversalSource, Rhai
// ---------------------------------------------------------------------------------------------

#[test]
fn validate_schema_is_the_dry_run_with_the_same_report() {
    let mut g = violating_graph();
    let dry = g
        .traversal()
        .validate_schema(&schema(SchemaMode::Closed))
        .unwrap()
        .violations;
    let applied = apply_err(&mut g, schema(SchemaMode::Closed));
    assert_eq!(&dry, report_of(&applied));
    assert_eq!(g.get_schema().unwrap(), None);
    assert!(
        g.traversal()
            .validate_schema(&schema(SchemaMode::None))
            .unwrap()
            .is_valid()
    );
}

#[test]
fn the_traversal_source_fails_with_the_report() {
    let mut g = violating_graph();
    let err = g
        .traversal_mut()
        .set_schema(schema(SchemaMode::Closed))
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("Person.age is declared mandatory but is missing on 12 vertices"),
        "{err}"
    );
    let help = err.help().expect("help");
    assert!(
        help.contains("NOT applied")
            && help.contains("mode \"none\"")
            && help.contains("infer_schema")
            && help.contains("validate_schema"),
        "{help}"
    );
    let err = g
        .traversal_mut()
        .set_schema(GraphSchema::from_json(&schema(SchemaMode::Open).to_json()).unwrap())
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("Company.name is declared mandatory"),
        "{err}"
    );
    // ... and through the GraphStorage trait object
    let err = GraphStorage::set_schema(&mut g, schema(SchemaMode::Closed)).unwrap_err();
    assert!(matches!(
        &err,
        GraphError::Schema(SchemaError::ExistingDataViolatesSchema { .. })
    ));
}

#[test]
fn rhai_set_schema_fails_with_the_report_and_changes_nothing() {
    let graph = Arc::new(GraphSource::empty());
    let _ = eval_value(
        graph.clone(),
        r#"g.addV("Person").property("name", "Ann").toList();
           g.addV("Company").toList(); 1"#,
    )
    .unwrap();
    let json = schema(SchemaMode::Closed).to_json();
    let err = eval_value(graph.clone(), &format!("g.set_schema(`{json}`)"))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("Person.age is declared mandatory but is missing on 1 vertex"),
        "{err}"
    );
    assert!(
        err.contains("Company.name is declared mandatory but is missing on 1 vertex"),
        "{err}"
    );
    // the camelCase spelling and a map go through the same check
    let err = eval_value(
        graph.clone(),
        &format!("g.set_schema(parse_json(`{json}`))"),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("Cannot apply the Closed schema"), "{err}");
    // nothing was applied: the creation that Closed would reject still works
    assert!(graph.read().get_schema().unwrap().is_none());
    let _ = eval_value(graph.clone(), r#"g.addV("Alien").toList(); 1"#).unwrap();
    // mode "none" applies regardless
    let none = schema(SchemaMode::None).to_json();
    let _ = eval_value(graph.clone(), &format!("g.set_schema(`{none}`)")).unwrap();
    assert_eq!(
        graph.read().get_schema().unwrap().unwrap().mode,
        SchemaMode::None
    );
}

// ---------------------------------------------------------------------------------------------
// Task I: every nested violation of a value is reported (the write path stays fail-fast)
// ---------------------------------------------------------------------------------------------

/// A Closed schema with one vertex label `p` whose single property `prop` is `node`.
fn one_property_schema(node: &str) -> GraphSchema {
    GraphSchema::from_json(&format!(
        r#"{{"mode": "closed", "vertices": {{"p": {{"schema": {{"type": "object",
            "properties": {{"prop": {node}}}}}}}}}}}"#
    ))
    .unwrap()
}

fn graph_with(id: &str, value: ElementProperty) -> TraversalGraph {
    let mut g = TraversalGraph::new();
    g.add_vertex(Some(id.into()), vec!["p".into()], props(&[("prop", value)]))
        .unwrap();
    g
}

fn report_for(g: &TraversalGraph, schema: &GraphSchema) -> SchemaViolationReport {
    g.traversal().validate_schema(schema).unwrap().violations
}

/// `(kind, field)` of every group, in report order.
fn kinds_and_fields(report: &SchemaViolationReport) -> Vec<(&'static str, String)> {
    report
        .groups
        .iter()
        .map(|g| (g.kind.as_str(), g.field.clone().unwrap_or_default()))
        .collect()
}

const ADDRESS: &str = r#"{"type": "object", "required": ["zip", "street"],
    "properties": {"zip": {"type": "string"}, "street": {"type": "string"}}}"#;

#[test]
fn every_nested_violation_of_a_value_is_reported_not_only_the_first() {
    // The CLI repro of the task: an undeclared key, a wrong type and a missing required key in
    // ONE value.
    let g = graph_with("1", object(&[("city", s("x")), ("zip", n(5))]));
    let schema = one_property_schema(ADDRESS);
    let report = report_for(&g, &schema);
    assert_eq!(report.total_violations, 3, "{report}");
    assert_eq!(report.violating_elements, 1);
    let mut found = kinds_and_fields(&report);
    found.sort();
    assert_eq!(
        found,
        vec![
            ("missing_required", "$.prop.street".to_string()),
            ("type_mismatch", "$.prop.zip".to_string()),
            ("undeclared_property", "$.prop.city".to_string()),
        ]
    );
    assert_eq!(
        group(
            &report,
            ViolationKind::TypeMismatch,
            "p",
            Some("$.prop.zip")
        )
        .detail,
        Some("declared string, found int64".to_string())
    );
    // `set_schema` fails with the same complete report.
    let mut applied = graph_with("1", object(&[("city", s("x")), ("zip", n(5))]));
    assert_eq!(report_of(&apply_err(&mut applied, schema.clone())), &report);
}

#[test]
fn the_rhai_repro_reports_all_three_violations() {
    let graph = Arc::new(GraphSource::empty());
    let total = eval_value(
        graph,
        r#"g.add_v("p").property("address", #{city: "x", zip: 5}).next();
           g.validate_schema(#{mode: "closed", vertices: #{p: #{schema: #{type: "object",
               properties: #{address: #{type: "object",
                   properties: #{zip: #{type: "string"}, street: #{type: "string"}},
                   required: ["zip", "street"]}}}}}}).violations.total_violations"#,
    )
    .unwrap();
    assert_eq!(total.as_int().unwrap(), 3);
}

#[test]
fn every_item_of_an_array_of_objects_is_checked() {
    let node = format!(r#"{{"type": "array", "items": {ADDRESS}}}"#);
    let items = ElementProperty::Array(vec![
        object(&[("zip", n(1)), ("street", s("a"))]),
        object(&[("zip", s("1")), ("street", s("a")), ("extra", n(1))]),
        object(&[]),
    ]);
    let report = report_for(&graph_with("v", items), &one_property_schema(&node));
    let mut found = kinds_and_fields(&report);
    found.sort();
    assert_eq!(
        found,
        vec![
            ("missing_required", "$.prop[*].street".to_string()),
            ("missing_required", "$.prop[*].zip".to_string()),
            ("type_mismatch", "$.prop[*].zip".to_string()),
            ("undeclared_property", "$.prop[*].extra".to_string()),
        ]
    );
    assert_eq!(
        group(
            &report,
            ViolationKind::TypeMismatch,
            "p",
            Some("$.prop[*].zip")
        )
        .example_field
        .as_deref(),
        Some("$.prop[0].zip")
    );
    assert_eq!(report.total_violations, 4);
    assert_eq!(report.violating_elements, 1);
}

#[test]
fn nested_objects_inside_arrays_are_checked_at_any_depth() {
    let node = r#"{"type": "array", "items": {"type": "object", "properties": {
        "members": {"type": "array", "items": {"type": "object", "required": ["name"],
            "properties": {"name": {"type": "string", "minLength": 2}}}}}}}"#;
    let teams = ElementProperty::Array(vec![
        object(&[(
            "members",
            ElementProperty::Array(vec![
                object(&[("name", n(1))]),
                object(&[("name", s("x"))]),
                object(&[("name", s("ok"))]),
            ]),
        )]),
        object(&[("members", ElementProperty::Array(vec![object(&[])]))]),
    ]);
    let report = report_for(&graph_with("v", teams), &one_property_schema(node));
    let mut found = kinds_and_fields(&report);
    found.sort();
    assert_eq!(
        found,
        vec![
            (
                "constraint_violation",
                "$.prop[*].members[*].name".to_string()
            ),
            ("missing_required", "$.prop[*].members[*].name".to_string()),
            ("type_mismatch", "$.prop[*].members[*].name".to_string()),
        ]
    );
    let example = |kind| {
        group(&report, kind, "p", Some("$.prop[*].members[*].name"))
            .example_field
            .clone()
    };
    assert_eq!(
        example(ViolationKind::ConstraintViolation).as_deref(),
        Some("$.prop[0].members[1].name")
    );
    assert_eq!(
        example(ViolationKind::MissingRequired).as_deref(),
        Some("$.prop[1].members[0].name")
    );
}

fn key<'a>(value: &'a ElementProperty, name: &str) -> Option<&'a ElementProperty> {
    match value {
        ElementProperty::Object(map) => map.get(name),
        _ => None,
    }
}

/// An address list where item 0 is valid and items 1..4 lack `zip` and have an integer `street`.
fn addresses() -> ElementProperty {
    let mut items = vec![object(&[("zip", s("1")), ("street", s("a"))])];
    for i in 1..4 {
        items.push(object(&[("street", n(i))]));
    }
    ElementProperty::Array(items)
}

#[test]
fn array_items_collapse_into_one_group_per_rule_with_the_first_example() {
    let node = format!(r#"{{"type": "array", "items": {ADDRESS}}}"#);
    let mut g = TraversalGraph::new();
    for i in 0..5 {
        g.add_vertex(
            Some(format!("v{i}").into()),
            vec!["p".into()],
            props(&[("prop", addresses())]),
        )
        .unwrap();
    }
    let schema = one_property_schema(&node);
    let report = report_for(&g, &schema);
    assert_eq!(report.groups.len(), 2, "{report}");
    for kind in [ViolationKind::MissingRequired, ViolationKind::TypeMismatch] {
        let field = if kind == ViolationKind::MissingRequired {
            "zip"
        } else {
            "street"
        };
        let found = group(&report, kind, "p", Some(&format!("$.prop[*].{field}")));
        // 5 elements x 3 bad items: each element counts once per group
        assert_eq!(found.count, 5, "{report}");
        assert_eq!(found.sample_ids, vec!["v0", "v1", "v2", "v3", "v4"]);
        assert_eq!(
            found.example_field.as_deref(),
            Some(format!("$.prop[1].{field}").as_str())
        );
    }
    assert_eq!(report.total_violations, 10);
    assert_eq!(report.violating_elements, 5);
    let text = report.to_string();
    assert!(
        text.contains(
            "p $.prop[*].zip is declared mandatory but is missing on 5 vertices \
             (first at $.prop[1].zip) [ids: "
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "p $.prop[*].street has the wrong type on 5 vertices (first at $.prop[1].street) \
             (declared string, found int64)"
        ),
        "{text}"
    );
    // The data form carries both.
    let value = report.to_value();
    let groups = key(&value, "groups").and_then(|g| g.as_array()).unwrap();
    let zip = groups
        .iter()
        .find(|g| key(g, "field").and_then(|f| f.as_str()) == Some("$.prop[*].zip"))
        .unwrap();
    assert_eq!(
        key(zip, "example_field").and_then(|f| f.as_str()),
        Some("$.prop[1].zip")
    );
    // A field without an index has no example.
    let flat = report_for(
        &graph_with("x", object(&[("zip", n(1)), ("street", s("a"))])),
        &one_property_schema(ADDRESS),
    );
    assert_eq!(flat.groups[0].field.as_deref(), Some("$.prop.zip"));
    assert_eq!(flat.groups[0].example_field, None);
    let flat_value = flat.to_value();
    let flat_group = &key(&flat_value, "groups")
        .and_then(|g| g.as_array())
        .unwrap()[0];
    assert!(matches!(
        key(flat_group, "example_field"),
        Some(ElementProperty::Null)
    ));
}

#[test]
fn one_array_rule_does_not_fill_the_group_cap() {
    let node = format!(r#"{{"type": "array", "items": {ADDRESS}}}"#);
    let items = ElementProperty::Array(
        (0..MAX_GROUPS + 10)
            .map(|i| object(&[("zip", n(i as i64)), ("street", s("a"))]))
            .collect(),
    );
    let report = report_for(&graph_with("v", items), &one_property_schema(&node));
    assert_eq!(report.groups.len(), 1, "{report}");
    assert_eq!(report.omitted_violations, 0);
    assert_eq!(report.groups[0].count, 1);
    assert_eq!(
        report.groups[0].example_field.as_deref(),
        Some("$.prop[0].zip")
    );
}

#[test]
fn an_any_of_member_reports_all_of_its_violations() {
    // No member accepts the object: every violation of the first object member is listed.
    let node = format!(r#"{{"anyOf": [{{"type": "string"}}, {ADDRESS}]}}"#);
    let value = object(&[("zip", n(5)), ("street", n(6))]);
    let report = report_for(&graph_with("v", value), &one_property_schema(&node));
    let mut found = kinds_and_fields(&report);
    found.sort();
    assert_eq!(
        found,
        vec![
            ("type_mismatch", "$.prop.street".to_string()),
            ("type_mismatch", "$.prop.zip".to_string()),
        ]
    );
}

#[test]
fn one_element_counts_once_per_group() {
    // The node and its `anyOf` member both reject `$.prop.zip`: one violation, not two.
    let node = r#"{"type": "object", "properties": {"zip": {"type": "string"}},
        "anyOf": [{"type": "object", "properties": {"zip": {"type": "string"}}}]}"#;
    let mut g = graph_with("a", object(&[("zip", n(5))]));
    g.add_vertex(
        Some("b".into()),
        vec!["p".into()],
        props(&[("prop", object(&[("zip", n(6))]))]),
    )
    .unwrap();
    let report = report_for(&g, &one_property_schema(node));
    assert_eq!(report.groups.len(), 1, "{report}");
    assert_eq!(report.groups[0].count, 2);
    assert_eq!(report.groups[0].sample_ids, vec!["a", "b"]);
    assert_eq!(report.total_violations, 2);
}

#[test]
fn the_report_order_is_deterministic() {
    let build = || {
        let mut g = TraversalGraph::new();
        for (id, value) in [
            ("1", object(&[("city", s("x")), ("zip", n(5))])),
            ("2", object(&[("zip", n(5)), ("street", n(1))])),
            ("3", object(&[("street", s("s")), ("a", n(1)), ("b", n(2))])),
        ] {
            g.add_vertex(Some(id.into()), vec!["p".into()], props(&[("prop", value)]))
                .unwrap();
        }
        g
    };
    let schema = one_property_schema(ADDRESS);
    let first = report_for(&build(), &schema);
    for _ in 0..5 {
        assert_eq!(report_for(&build(), &schema), first);
    }
    // largest first (`$.prop.zip` is wrong on "1" and "2"), then kind, element, label, field
    assert_eq!(first.groups[0].count, 2);
    assert_eq!(
        kinds_and_fields(&first),
        vec![
            ("type_mismatch", "$.prop.zip".to_string()),
            ("missing_required", "$.prop.street".to_string()),
            ("missing_required", "$.prop.zip".to_string()),
            ("type_mismatch", "$.prop.street".to_string()),
            ("undeclared_property", "$.prop.a".to_string()),
            ("undeclared_property", "$.prop.b".to_string()),
            ("undeclared_property", "$.prop.city".to_string()),
        ]
    );
}

#[test]
fn a_single_write_still_fails_on_the_first_violation() {
    let mut g = TraversalGraph::new();
    g.set_schema(one_property_schema(ADDRESS)).unwrap();
    let err = g
        .add_vertex(
            Some("1".into()),
            vec!["p".into()],
            props(&[("prop", object(&[("city", s("x")), ("zip", n(5))]))]),
        )
        .unwrap_err();
    // One plain violation (the first in walk order: the undeclared key), not a report.
    match err {
        GraphError::Schema(SchemaError::UndeclaredProperty { property_name, .. }) => {
            assert_eq!(property_name, "$.prop.city")
        }
        other => panic!("expected the first violation, got {other:?}"),
    }
}
