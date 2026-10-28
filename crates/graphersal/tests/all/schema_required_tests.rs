//! `Open` enforces required keys exactly like `Closed`; removing a required property is rejected
//! in both; an optional key may be absent, and null where its type admits null;
//! `add_vertex_label` / `remove_vertex_label` go through the schema.
//! Matrix: Open/Closed x vertices/edges x (add without required, set null, remove required,
//! remove optional, remove absent).
#![cfg(all(feature = "script", feature = "serde"))]

use std::sync::Arc;

use crate::script_host::eval_value;
use compact_str::CompactString;
use graphersal::prelude::*;
use graphersal::schema::{EdgeSchema, PropertySchema, VertexSchema};

const MODES: [SchemaMode; 2] = [SchemaMode::Open, SchemaMode::Closed];

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

fn vs(_label: &str, fields: Vec<(&str, (PropertySchema, bool))>) -> VertexSchema {
    VertexSchema::new(root(fields))
}

/// Person(name!, age!, bio?), Employee(badge!), Company(name!), `works_at(since!, note?)`.
fn schema(mode: SchemaMode) -> GraphSchema {
    let mut s = GraphSchema::new().with_mode(mode);
    s.vertices.insert(
        "Person".into(),
        vs(
            "Person",
            vec![
                ("name", field(PropertySchema::string(), false)),
                ("age", field(PropertySchema::integer(), false)),
                ("bio", field(PropertySchema::string(), true)),
            ],
        ),
    );
    s.vertices.insert(
        "Employee".into(),
        vs(
            "Employee",
            vec![("badge", field(PropertySchema::integer(), false))],
        ),
    );
    s.vertices.insert(
        "Company".into(),
        vs(
            "Company",
            vec![("name", field(PropertySchema::string(), false))],
        ),
    );
    s.edges.insert(
        "works_at".into(),
        EdgeSchema::new(root(vec![
            ("since", field(PropertySchema::integer(), false)),
            ("note", field(PropertySchema::string(), true)),
        ])),
    );
    s
}

fn el(entries: &[(&str, ElementProperty)]) -> PropertyMap {
    let mut out = PropertyMap::default();
    for (k, v) in entries {
        out.insert(*k, v.clone());
    }
    out
}

fn st(v: &str) -> ElementProperty {
    ElementProperty::String(v.into())
}

fn int(v: i64) -> ElementProperty {
    ElementProperty::from(v)
}

/// Person p1 (name, age, bio), Company c1, edge e1 works_at (since, note).
fn populated(mode: SchemaMode) -> Arc<Graph> {
    let mut graph = TraversalGraph::new();
    graph.set_schema(schema(mode)).unwrap();
    let p = graph
        .add_vertex(
            Some("p1".into()),
            vec!["Person".into()],
            el(&[("name", st("Ann")), ("age", int(30)), ("bio", st("hi"))]),
        )
        .unwrap();
    let c = graph
        .add_vertex(
            Some("c1".into()),
            vec!["Company".into()],
            el(&[("name", st("Acme"))]),
        )
        .unwrap();
    graph
        .add_edge(
            Some("e1".into()),
            Some("works_at".into()),
            p,
            c,
            el(&[("since", int(2020)), ("note", st("n"))]),
        )
        .unwrap();
    Arc::new(Graph::new(graph))
}

fn run(graph: &Arc<Graph>, script: &str) -> Result<(), String> {
    eval_value(graph.clone(), script)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn count(graph: &Arc<Graph>, script: &str) -> i64 {
    eval_value(graph.clone(), script).unwrap().as_int().unwrap()
}

fn present(graph: &Arc<Graph>, element: &str, key: &str) -> bool {
    count(
        graph,
        &format!(r#"g.{element}.values("{key}").count().next()"#),
    ) > 0
}

// --- add without required ----------------------------------------------------------------

#[test]
fn add_without_a_required_field_is_rejected_in_open_and_closed() {
    for mode in MODES {
        let mut graph = TraversalGraph::new();
        graph.set_schema(schema(mode)).unwrap();
        let r = graph.add_vertex(
            Some("p".into()),
            vec!["Person".into()],
            el(&[("name", st("Ann"))]),
        );
        assert!(
            matches!(
                r,
                Err(GraphError::Schema(
                    SchemaError::MissingRequiredProperty { .. }
                ))
            ),
            "{mode:?}: {r:?}"
        );
        // null counts as absent
        let r = graph.add_vertex(
            Some("p".into()),
            vec!["Person".into()],
            el(&[("name", st("Ann")), ("age", ElementProperty::Null)]),
        );
        assert!(r.is_err(), "{mode:?}");
        // optional absent / null is fine
        graph
            .add_vertex(
                Some("p".into()),
                vec!["Person".into()],
                el(&[
                    ("name", st("Ann")),
                    ("age", int(1)),
                    ("bio", ElementProperty::Null),
                ]),
            )
            .unwrap();
        // an undeclared label stays free in Open (and is rejected in Closed)
        let alien = graph.add_vertex(
            Some("a".into()),
            vec!["Alien".into()],
            PropertyMap::default(),
        );
        assert_eq!(alien.is_ok(), matches!(mode, SchemaMode::Open), "{mode:?}");
    }
}

#[test]
fn add_edge_without_a_required_field_is_rejected_in_open_and_closed() {
    for mode in MODES {
        let mut graph = TraversalGraph::new();
        graph.set_schema(schema(mode)).unwrap();
        let p = graph
            .add_vertex(
                None,
                vec!["Person".into()],
                el(&[("name", st("A")), ("age", int(1))]),
            )
            .unwrap();
        let c = graph
            .add_vertex(None, vec!["Company".into()], el(&[("name", st("C"))]))
            .unwrap();
        let r = graph.add_edge(None, Some("works_at".into()), p, c, PropertyMap::default());
        assert!(
            matches!(
                r,
                Err(GraphError::Schema(
                    SchemaError::MissingRequiredProperty { .. }
                ))
            ),
            "{mode:?}: {r:?}"
        );
        graph
            .add_edge(
                None,
                Some("works_at".into()),
                p,
                c,
                el(&[("since", int(1))]),
            )
            .unwrap();
    }
}

#[test]
fn creation_through_the_fold_rule_works_and_unfused_creation_fails_cleanly_in_open() {
    for mode in MODES {
        let graph = populated(mode);
        run(
            &graph,
            r#"g.addV("Person").property("name", "B").property("age", 2).next()"#,
        )
        .unwrap();
        // fold rule disabled: the empty creation is rejected in Open too, graph unchanged
        let r = run(
            &graph,
            r#"g.with("optimizer.disabled", ["add_property_fold"]).addV("Person").property("name", "C").property("age", 3).next()"#,
        );
        assert!(r.unwrap_err().contains("Missing required"), "{mode:?}");
        assert_eq!(count(&graph, "g.V().count().next()"), 3, "{mode:?}");
    }
}

#[test]
fn a_required_top_level_field_is_enforced_for_a_label_without_other_fields_in_open() {
    let mut s = GraphSchema::new().with_mode(SchemaMode::Open);
    s.vertices.insert(
        "T".into(),
        vs("T", vec![("k", field(PropertySchema::integer(), false))]),
    );
    let mut graph = TraversalGraph::new();
    graph.set_schema(s).unwrap();
    assert!(
        graph
            .add_vertex(None, vec!["T".into()], PropertyMap::default())
            .is_err()
    );
}

// --- set null ------------------------------------------------------------------------------

#[test]
fn setting_a_required_field_to_null_is_rejected_and_an_optional_one_allowed() {
    for mode in MODES {
        let graph = populated(mode);
        assert!(
            run(&graph, r#"g.V("p1").property("age", ()).toList()"#).is_err(),
            "{mode:?}"
        );
        assert!(
            run(&graph, r#"g.E("e1").property("since", ()).toList()"#).is_err(),
            "{mode:?}"
        );
        run(&graph, r#"g.V("p1").property("bio", ()).toList()"#).unwrap();
        run(&graph, r#"g.E("e1").property("note", ()).toList()"#).unwrap();
        assert!(present(&graph, r#"V("p1")"#, "age"));
    }
}

// --- removal -------------------------------------------------------------------------------

#[test]
fn removing_a_required_property_is_rejected_atomically_in_open_and_closed() {
    for mode in MODES {
        let graph = populated(mode);
        for (script, element, key) in [
            (
                r#"g.V("p1").remove_property("age").toList()"#,
                r#"V("p1")"#,
                "age",
            ),
            (
                r#"g.V("p1").properties("age").drop().toList()"#,
                r#"V("p1")"#,
                "age",
            ),
            (
                r#"g.V("p1").remove_property(jpath("age")).toList()"#,
                r#"V("p1")"#,
                "age",
            ),
            (
                r#"g.E("e1").remove_property("since").toList()"#,
                r#"E("e1")"#,
                "since",
            ),
            (
                r#"g.E("e1").properties("since").drop().toList()"#,
                r#"E("e1")"#,
                "since",
            ),
            (
                r#"g.E("e1").remove_property(jpath("since")).toList()"#,
                r#"E("e1")"#,
                "since",
            ),
        ] {
            let err = run(&graph, script).expect_err(script);
            assert!(
                err.contains("Cannot remove required property"),
                "{mode:?} {script}: {err}"
            );
            assert!(err.contains(key), "{err}");
            assert!(
                present(&graph, element, key),
                "{mode:?} {script}: graph must be unchanged"
            );
        }
    }
}

#[test]
fn removing_optional_or_absent_properties_stays_allowed() {
    for mode in MODES {
        let graph = populated(mode);
        run(&graph, r#"g.V("p1").remove_property("bio").toList()"#).unwrap();
        run(&graph, r#"g.E("e1").remove_property("note").toList()"#).unwrap();
        assert!(!present(&graph, r#"V("p1")"#, "bio"));
        assert!(!present(&graph, r#"E("e1")"#, "note"));
        // absent (optional, undeclared): no-op without error
        run(&graph, r#"g.V("p1").remove_property("bio").toList()"#).unwrap();
        run(&graph, r#"g.V("p1").remove_property("nickname").toList()"#).unwrap();
        run(
            &graph,
            r#"g.V("p1").properties("nickname").drop().toList()"#,
        )
        .unwrap();
    }
}

#[test]
fn removing_an_undeclared_property_is_allowed_in_open() {
    let graph = populated(SchemaMode::Open);
    run(&graph, r#"g.V("p1").property("nickname", "A").toList()"#).unwrap();
    run(&graph, r#"g.V("p1").remove_property("nickname").toList()"#).unwrap();
    assert!(!present(&graph, r#"V("p1")"#, "nickname"));
}

#[test]
fn a_property_required_by_any_label_cannot_be_removed() {
    for mode in MODES {
        let mut graph = TraversalGraph::new();
        graph.set_schema(schema(mode)).unwrap();
        let v = graph
            .add_vertex(
                Some("x".into()),
                vec!["Person".into(), "Employee".into()],
                el(&[("name", st("A")), ("age", int(1)), ("badge", int(7))]),
            )
            .unwrap();
        // `badge` is required by the second label only
        assert!(graph.drop_vertex_property("badge", v).is_err(), "{mode:?}");
        assert!(graph.drop_vertex_property("bio", v).is_ok(), "{mode:?}");
        assert!(graph.drop_vertex_property("age", v).is_err(), "{mode:?}");
    }
}

#[test]
fn removal_in_none_mode_is_unconstrained() {
    let mut graph = TraversalGraph::new();
    let v = graph
        .add_vertex(None, vec!["Person".into()], el(&[("name", st("A"))]))
        .unwrap();
    graph.drop_vertex_property("name", v).unwrap();
}

// --- label methods -----------------------------------------------------------------------

fn labels(graph: &TraversalGraph, v: graphersal::storage::VertexHandle) -> Vec<String> {
    graph
        .vertex_labels(v)
        .unwrap()
        .iter()
        .map(|l| l.to_string())
        .collect()
}

#[test]
fn add_vertex_label_matrix() {
    for mode in MODES {
        let mut graph = TraversalGraph::new();
        graph.set_schema(schema(mode)).unwrap();
        let v = graph
            .add_vertex(
                Some("x".into()),
                vec!["Person".into()],
                el(&[("name", st("A")), ("age", int(1))]),
            )
            .unwrap();
        // already present: Ok(false)
        assert!(
            !graph.add_vertex_label(v, "Person".into()).unwrap(),
            "{mode:?}"
        );
        // a newly declaring label requires its fields: `badge` is missing
        assert!(
            graph.add_vertex_label(v, "Employee".into()).is_err(),
            "{mode:?}"
        );
        assert_eq!(
            labels(&graph, v),
            ["Person"],
            "{mode:?}: unchanged on error"
        );
        // unknown label: Open allows, Closed rejects
        let unknown = graph.add_vertex_label(v, "Alien".into());
        match mode {
            SchemaMode::Open => {
                assert!(unknown.unwrap());
                assert_eq!(labels(&graph, v), ["Person", "Alien"]);
            }
            _ => {
                assert!(matches!(
                    unknown,
                    Err(GraphError::Schema(SchemaError::UnknownVertexLabel { .. }))
                ));
                assert_eq!(labels(&graph, v), ["Person"]);
            }
        }
        // a label whose declared type clashes with an existing property is rejected
        graph
            .set_schema({
                let mut s = schema(mode);
                s.vertices.insert(
                    "Typed".into(),
                    vs(
                        "Typed",
                        vec![("name", field(PropertySchema::integer(), true))],
                    ),
                );
                s
            })
            .unwrap();
        assert!(
            graph.add_vertex_label(v, "Typed".into()).is_err(),
            "{mode:?}"
        );
        // a label with only optional fields is accepted and applied
        graph
            .set_schema({
                let mut s = schema(mode);
                s.vertices.insert(
                    "Tagged".into(),
                    vs(
                        "Tagged",
                        vec![("x", field(PropertySchema::integer(), true))],
                    ),
                );
                s
            })
            .unwrap();
        assert!(
            graph.add_vertex_label(v, "Tagged".into()).unwrap(),
            "{mode:?}"
        );
        assert_eq!(labels(&graph, v).last().map(String::as_str), Some("Tagged"));
    }
}

#[test]
fn remove_vertex_label_matrix() {
    for mode in MODES {
        let mut graph = TraversalGraph::new();
        graph.set_schema(schema(mode)).unwrap();
        let v = graph
            .add_vertex(
                Some("x".into()),
                vec!["Person".into(), "Employee".into()],
                el(&[("name", st("A")), ("age", int(1)), ("badge", int(7))]),
            )
            .unwrap();
        // absent label: Ok(false)
        assert!(
            !graph.remove_vertex_label(v, "Company").unwrap(),
            "{mode:?}"
        );
        // removing Employee: `badge` becomes undeclared (Closed rejects, Open allows)
        let r = graph.remove_vertex_label(v, "Employee");
        match mode {
            SchemaMode::Open => {
                assert!(r.unwrap());
                assert_eq!(labels(&graph, v), ["Person"]);
            }
            _ => {
                assert!(
                    matches!(
                        r,
                        Err(GraphError::Schema(SchemaError::UndeclaredProperty { .. }))
                    ),
                    "{r:?}"
                );
                assert_eq!(labels(&graph, v), ["Person", "Employee"]);
            }
        }
        // removing Person leaves Employee, whose requirement `badge` holds; `name` is
        // undeclared then (Closed rejects).
        let r = graph.remove_vertex_label(v, "Person");
        assert_eq!(
            r.is_ok(),
            matches!(mode, SchemaMode::Open),
            "{mode:?}: {r:?}"
        );
    }
}

#[test]
fn removing_the_last_label_is_rejected_in_closed_only() {
    let mut open = TraversalGraph::new();
    open.set_schema(schema(SchemaMode::Open)).unwrap();
    let v = open
        .add_vertex(None, vec!["Company".into()], el(&[("name", st("A"))]))
        .unwrap();
    assert!(open.remove_vertex_label(v, "Company").unwrap());

    let mut closed = TraversalGraph::new();
    closed.set_schema(schema(SchemaMode::Closed)).unwrap();
    let v = closed
        .add_vertex(None, vec!["Company".into()], el(&[("name", st("A"))]))
        .unwrap();
    assert!(closed.remove_vertex_label(v, "Company").is_err());
    assert_eq!(labels(&closed, v), ["Company"]);
}

#[test]
fn label_methods_in_none_mode_are_unconstrained() {
    let mut graph = TraversalGraph::new();
    let v = graph
        .add_vertex(None, vec!["A".into()], PropertyMap::default())
        .unwrap();
    assert!(graph.add_vertex_label(v, CompactString::from("B")).unwrap());
    assert!(graph.remove_vertex_label(v, "A").unwrap());
    assert!(graph.remove_vertex_label(v, "B").unwrap());
}
