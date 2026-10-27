//! Reproduction tests for the gaps found by the schema audit.
//!
//! Every test encodes the decided behaviour (Open AND Closed enforce required keys; removing a
//! required property is rejected in both; applying a schema to violating data fails; any label
//! of an endpoint can satisfy a topology connection; `required` and nullability are separate, as
//! in JSON Schema).
//! Gap 4 (partial application of `property(map)`, `property_json`, `mergeV` `onMatch` and `addE`
//! over several endpoints) is resolved by the implicit auto-commit
//! (`book/src/users_guide/transactions.md`): a failing traversal is rolled back, so nothing of it is applied.
#![cfg(all(feature = "script", feature = "serde"))]

use std::sync::Arc;

use crate::script_host::eval_value;
use compact_str::CompactString;
use graphersal::prelude::*;

// ---------------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Mode {
    None,
    Open,
    Closed,
}

const ENFORCING: [Mode; 2] = [Mode::Open, Mode::Closed];

/// `Person(name: string, age: integer, bio?: string|null)` (name and age required),
/// `Company(name)`, `Robot(name)`, `works_at(since: integer): Person -> Company`,
/// `member_of(): Person -> Company`.
fn schema(mode: SchemaMode) -> GraphSchema {
    crate::schema_support::schema_in(
        r#"{
          "vertices": {
            "Person": {"schema": {"type": "object", "properties": {
                "name": {"type": "string"}, "age": {"type": "integer"},
                "bio": {"type": ["string", "null"]}}, "required": ["name", "age"]}},
            "Company": {"schema": {"type": "object",
                "properties": {"name": {"type": "string"}}, "required": ["name"]}},
            "Robot": {"schema": {"type": "object",
                "properties": {"name": {"type": "string"}}, "required": ["name"]}}
          },
          "edges": {
            "works_at": {"connections": [{"from": "Person", "to": "Company"}],
                "schema": {"type": "object", "properties": {"since": {"type": "integer"}},
                           "required": ["since"]}},
            "member_of": {"connections": [{"from": "Person", "to": "Company"}],
                "schema": {"type": "object"}}
          }
        }"#,
        mode,
    )
}

fn empty_graph(mode: Mode) -> TraversalGraph {
    let mut graph = TraversalGraph::new();
    match mode {
        Mode::None => {}
        Mode::Open => graph.set_schema(schema(SchemaMode::Open)).unwrap(),
        Mode::Closed => graph.set_schema(schema(SchemaMode::Closed)).unwrap(),
    }
    graph
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

fn person_props() -> PropertyMap {
    props(&[("name", s("Ann")), ("age", n(30))])
}

fn wrap(graph: TraversalGraph) -> Arc<Graph> {
    Arc::new(Graph::new(graph))
}

/// A graph with Person "p1" (name, age), Company "c1" and edge "e1" `works_at` (since).
fn populated(mode: Mode) -> Arc<Graph> {
    let mut graph = empty_graph(mode);
    let p = graph
        .add_vertex(Some("p1".into()), vec!["Person".into()], person_props())
        .unwrap();
    let c = graph
        .add_vertex(
            Some("c1".into()),
            vec!["Company".into()],
            props(&[("name", s("Acme"))]),
        )
        .unwrap();
    graph
        .add_edge(
            Some("e1".into()),
            Some("works_at".into()),
            p,
            c,
            props(&[("since", n(2020))]),
        )
        .unwrap();
    wrap(graph)
}

fn run(graph: &Arc<Graph>, script: &str) -> Result<(), String> {
    eval_value(graph.clone(), script)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn count(graph: &Arc<Graph>, script: &str) -> i64 {
    eval_value(graph.clone(), script)
        .unwrap_or_else(|e| panic!("`{script}` failed: {e}"))
        .as_int()
        .unwrap_or_else(|_| panic!("`{script}` is not an int"))
}

fn vertices(graph: &Arc<Graph>) -> i64 {
    count(graph, "g.V().count().next()")
}

fn edges(graph: &Arc<Graph>) -> i64 {
    count(graph, "g.E().count().next()")
}

fn has_value(graph: &Arc<Graph>, id: &str, key: &str) -> bool {
    count(
        graph,
        &format!(r#"g.V("{id}").values("{key}").count().next()"#),
    ) > 0
}

// ---------------------------------------------------------------------------------------------
// Gap 1: add_v / add_e create an empty element, properties come after
// ---------------------------------------------------------------------------------------------

#[test]
fn gap1_add_v_with_required_properties_succeeds_in_open_and_closed() {
    for mode in ENFORCING {
        let graph = wrap(empty_graph(mode));
        let r = run(
            &graph,
            r#"g.addV("Person").property("name", "Ann").property("age", 30).next()"#,
        );
        assert!(r.is_ok(), "{mode:?}: {r:?}");
        assert_eq!(vertices(&graph), 1, "{mode:?}");
    }
}

#[test]
fn gap1_add_v_in_none_mode_is_unconstrained() {
    let graph = wrap(empty_graph(Mode::None));
    run(&graph, r#"g.addV("Person").property("name", "Ann").next()"#).unwrap();
    assert_eq!(vertices(&graph), 1);
}

/// Confirmed real: today the empty `add_v("Person")` is rejected in Closed, so the query can never
/// succeed without properties. The failure itself must not mutate the graph: guard.
#[test]
fn gap1_closed_empty_creation_error_does_not_mutate_the_graph() {
    let graph = wrap(empty_graph(Mode::Closed));
    let r = run(&graph, r#"g.addV("Person").next()"#);
    assert!(r.is_err());
    assert_eq!(vertices(&graph), 0);
}

#[test]
fn gap1_failing_property_leaves_no_vertex_behind() {
    for mode in ENFORCING {
        let graph = wrap(empty_graph(mode));
        let r = run(
            &graph,
            r#"g.addV("Person").property("name", "Ann").property("age", "old").next()"#,
        );
        assert!(r.is_err(), "{mode:?}");
        assert_eq!(
            vertices(&graph),
            0,
            "{mode:?}: an invalid creation must leave no element"
        );
    }
}

#[test]
fn gap1_add_e_with_required_properties_succeeds_in_open_and_closed() {
    for mode in ENFORCING {
        let graph = populated(mode);
        let r = run(
            &graph,
            r#"g.addE("works_at").property("since", 2021).from(__.V("p1")).to(__.V("c1")).next()"#,
        );
        assert!(r.is_ok(), "{mode:?}: {r:?}");
        assert_eq!(edges(&graph), 2, "{mode:?}");
    }
}

#[test]
fn gap1_failing_edge_property_leaves_no_edge_behind() {
    for mode in ENFORCING {
        let graph = populated(mode);
        let r = run(
            &graph,
            r#"g.addE("works_at").property("since", "x").from(__.V("p1")).to(__.V("c1")).next()"#,
        );
        assert!(r.is_err(), "{mode:?}");
        assert_eq!(
            edges(&graph),
            1,
            "{mode:?}: an invalid edge must not remain"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Gap 2: removing a required property
// ---------------------------------------------------------------------------------------------

#[test]
fn gap2_remove_property_of_required_vertex_property_is_rejected() {
    for mode in ENFORCING {
        let graph = populated(mode);
        assert!(
            run(&graph, r#"g.V("p1").remove_property("age").toList()"#).is_err(),
            "{mode:?}"
        );
        assert!(
            has_value(&graph, "p1", "age"),
            "{mode:?}: the property must survive the rejected removal"
        );
    }
}

#[test]
fn gap2_one_segment_jpath_removal_of_required_property_is_rejected() {
    for mode in ENFORCING {
        let graph = populated(mode);
        assert!(
            run(
                &graph,
                r#"g.V("p1").remove_property(jpath("age")).toList()"#
            )
            .is_err(),
            "{mode:?}"
        );
        assert!(has_value(&graph, "p1", "age"), "{mode:?}");
    }
}

#[test]
fn gap2_drop_of_required_property_is_rejected() {
    for mode in ENFORCING {
        let graph = populated(mode);
        assert!(
            run(&graph, r#"g.V("p1").properties("age").drop().toList()"#).is_err(),
            "{mode:?}"
        );
        assert!(has_value(&graph, "p1", "age"), "{mode:?}");
    }
}

#[test]
fn gap2_remove_property_of_required_edge_property_is_rejected() {
    for mode in ENFORCING {
        let graph = populated(mode);
        assert!(
            run(&graph, r#"g.E("e1").remove_property("since").toList()"#).is_err(),
            "{mode:?}"
        );
        assert_eq!(
            count(&graph, r#"g.E("e1").values("since").count().next()"#),
            1,
            "{mode:?}"
        );
    }
}

#[test]
fn gap2_removing_an_optional_property_and_removal_in_none_mode_stay_allowed() {
    for mode in [Mode::None, Mode::Open, Mode::Closed] {
        let graph = populated(mode);
        run(&graph, r#"g.V("p1").property("bio", "hi").toList()"#).unwrap();
        run(&graph, r#"g.V("p1").remove_property("bio").toList()"#).unwrap();
        assert!(!has_value(&graph, "p1", "bio"), "{mode:?}");
    }
    let graph = populated(Mode::None);
    run(&graph, r#"g.V("p1").remove_property("age").toList()"#).unwrap();
    assert!(!has_value(&graph, "p1", "age"));
}

/// Guard: the equivalent write (setting the required property to null) is already rejected.
#[test]
fn gap2_setting_a_required_property_to_null_is_rejected() {
    for mode in ENFORCING {
        let graph = populated(mode);
        assert!(
            run(&graph, r#"g.V("p1").property("age", ()).toList()"#).is_err(),
            "{mode:?}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Gap 3: add_vertex_label / remove_vertex_label on the GraphStorage API
// ---------------------------------------------------------------------------------------------

#[test]
fn gap3_add_unknown_label_is_rejected_in_closed() {
    let mut graph = empty_graph(Mode::Closed);
    let v = graph
        .add_vertex(
            Some("c".into()),
            vec!["Company".into()],
            props(&[("name", s("A"))]),
        )
        .unwrap();
    assert!(graph.add_vertex_label(v, "Alien".into()).is_err());
    assert_eq!(graph.vertex_labels(v).len(), 1);
}

#[test]
fn gap3_add_unknown_label_is_allowed_in_open_and_none() {
    for mode in [Mode::None, Mode::Open] {
        let mut graph = empty_graph(mode);
        let v = graph
            .add_vertex(
                Some("c".into()),
                vec!["Company".into()],
                props(&[("name", s("A"))]),
            )
            .unwrap();
        assert!(
            graph.add_vertex_label(v, "Alien".into()).unwrap(),
            "{mode:?}"
        );
    }
}

#[test]
fn gap3_removing_the_last_label_is_rejected_in_closed() {
    let mut graph = empty_graph(Mode::Closed);
    let v = graph
        .add_vertex(
            Some("c".into()),
            vec!["Company".into()],
            props(&[("name", s("A"))]),
        )
        .unwrap();
    assert!(graph.remove_vertex_label(v, "Company").is_err());
    assert_eq!(graph.vertex_labels(v).len(), 1);
}

#[test]
fn gap3_added_label_re_checks_existing_required_properties() {
    for mode in ENFORCING {
        let mut graph = empty_graph(mode);
        let v = graph
            .add_vertex(
                Some("c".into()),
                vec!["Company".into()],
                props(&[("name", s("A"))]),
            )
            .unwrap();
        // Person additionally requires `age`, which the vertex does not have.
        assert!(
            graph.add_vertex_label(v, "Person".into()).is_err(),
            "{mode:?}"
        );
        assert_eq!(graph.vertex_labels(v).len(), 1, "{mode:?}");
    }
}

#[test]
fn gap3_removed_label_re_checks_incident_edge_topology_in_closed() {
    let mut graph = empty_graph(Mode::Closed);
    let a = graph
        .add_vertex(
            Some("a".into()),
            vec!["Person".into(), "Robot".into()],
            person_props(),
        )
        .unwrap();
    let c = graph
        .add_vertex(
            Some("c".into()),
            vec!["Company".into()],
            props(&[("name", s("A"))]),
        )
        .unwrap();
    graph
        .add_edge(
            Some("e".into()),
            Some("works_at".into()),
            a,
            c,
            props(&[("since", n(1))]),
        )
        .unwrap();
    // Without Person the works_at edge has no declared connection from this vertex.
    assert!(graph.remove_vertex_label(a, "Person").is_err());
}

// ---------------------------------------------------------------------------------------------
// Gap 4 (resolved by the implicit auto-commit): a failing traversal applies nothing
// ---------------------------------------------------------------------------------------------

/// The entries are applied one by one; when entry 2 (`bio`) fails, the rollback also undoes
/// entry 1 (`age`).
#[test]
fn gap4_property_map_is_atomic() {
    for mode in ENFORCING {
        let graph = populated(mode);
        let r = run(
            &graph,
            r#"g.V("p1").property(#{"age": 31, "bio": 5}).toList()"#,
        );
        assert!(r.is_err(), "{mode:?}");
        assert_eq!(
            count(&graph, r#"g.V("p1").values("age").next()"#),
            30,
            "{mode:?}: nothing of the failed traversal is applied"
        );
    }
}

#[test]
fn gap4_property_json_is_atomic() {
    for mode in ENFORCING {
        let graph = populated(mode);
        let r = graph
            .write()
            .traversal_mut()
            .v("p1")
            .property_json(serde_json::json!({"age": 31, "bio": 5}))
            .to_list();
        assert!(r.is_err(), "{mode:?}");
        assert_eq!(
            count(&graph, r#"g.V("p1").values("age").next()"#),
            30,
            "{mode:?}: nothing of the failed traversal is applied"
        );
    }
}

#[test]
fn gap4_merge_v_on_match_is_atomic() {
    for mode in ENFORCING {
        let graph = populated(mode);
        let r = run(
            &graph,
            r#"g.mergeV(#{"T.label": "Person", "name": "Ann"}).option(Merge.onMatch, #{"age": 31, "bio": 5}).toList()"#,
        );
        assert!(r.is_err(), "{mode:?}");
        assert_eq!(
            count(&graph, r#"g.V("p1").values("age").next()"#),
            30,
            "{mode:?}: nothing of the failed traversal is applied"
        );
    }
}

/// `addE` over several `from` vertices creates the valid edges until the first invalid endpoint;
/// the rollback removes them again.
#[test]
fn gap4_add_e_over_several_endpoints_is_atomic() {
    let mut graph = empty_graph(Mode::Closed);
    graph
        .add_vertex(Some("p1".into()), vec!["Person".into()], person_props())
        .unwrap();
    graph
        .add_vertex(
            Some("r1".into()),
            vec!["Robot".into()],
            props(&[("name", s("R"))]),
        )
        .unwrap();
    graph
        .add_vertex(
            Some("c1".into()),
            vec!["Company".into()],
            props(&[("name", s("A"))]),
        )
        .unwrap();
    let graph = wrap(graph);
    let r = run(
        &graph,
        r#"g.V("p1", "r1").addE("member_of").to(__.V("c1")).toList()"#,
    );
    assert!(r.is_err(), "the Robot endpoint violates the topology");
    assert_eq!(
        edges(&graph),
        0,
        "the edge of the first endpoint is rolled back"
    );
}

// ---------------------------------------------------------------------------------------------
// Gap 5: applying a schema to violating data
// ---------------------------------------------------------------------------------------------

fn violating_graph() -> TraversalGraph {
    let mut graph = TraversalGraph::new();
    // Person without the required `age`, plus an undeclared label.
    graph
        .add_vertex(
            Some("p".into()),
            vec!["Person".into()],
            props(&[("name", s("Ann"))]),
        )
        .unwrap();
    graph
        .add_vertex(
            Some("x".into()),
            vec!["Alien".into()],
            props(&[("planet", s("Mars"))]),
        )
        .unwrap();
    graph
}

#[test]
fn gap5_applying_a_schema_to_violating_data_fails_in_open_and_closed() {
    for mode in [SchemaMode::Open, SchemaMode::Closed] {
        let mut graph = violating_graph();
        let schema = GraphSchema::from_json(&schema(mode).to_json()).unwrap();
        assert!(graph.set_schema(schema).is_err(), "{mode:?}");
    }
}

#[test]
fn gap5_failed_schema_application_keeps_the_previous_schema() {
    let mut graph = violating_graph();
    let _ = graph.set_schema(schema(SchemaMode::Closed));
    assert!(graph.get_schema().unwrap().is_none());
}

/// Guard: the dry run reports the violations, and conforming data is accepted.
#[test]
fn gap5_validate_schema_reports_violations_and_accepts_conforming_data() {
    let graph = violating_graph();
    let validation = graph
        .traversal()
        .validate_schema(&schema(SchemaMode::Closed))
        .unwrap();
    assert!(
        validation.violations.groups.len() >= 2,
        "{}",
        validation.violations
    );
    assert!(!validation.is_valid());
    let mut ok = empty_graph(Mode::None);
    ok.add_vertex(Some("p".into()), vec!["Person".into()], person_props())
        .unwrap();
    assert!(
        ok.traversal()
            .validate_schema(&schema(SchemaMode::Closed))
            .unwrap()
            .is_valid()
    );
    assert!(ok.set_schema(schema(SchemaMode::Closed)).is_ok());
}

// ---------------------------------------------------------------------------------------------
// Gap 6: edge topology with multi-label endpoints
// ---------------------------------------------------------------------------------------------

fn topology_edge(labels: Vec<CompactString>) -> Result<(), GraphError> {
    let mut graph = empty_graph(Mode::Closed);
    let a = graph.add_vertex(Some("a".into()), labels, person_props())?;
    let c = graph.add_vertex(
        Some("c".into()),
        vec!["Company".into()],
        props(&[("name", s("A"))]),
    )?;
    graph
        .add_edge(
            Some("e".into()),
            Some("works_at".into()),
            a,
            c,
            props(&[("since", n(1))]),
        )
        .map(|_| ())
}

#[test]
fn gap6_person_as_first_label_satisfies_the_connection() {
    assert!(topology_edge(vec!["Person".into(), "Robot".into()]).is_ok());
}

#[test]
fn gap6_person_as_second_label_satisfies_the_connection() {
    // Robot declares no `age`, but the union of declared fields is checked per label; name+age are valid for Person.
    assert!(topology_edge(vec!["Robot".into(), "Person".into()]).is_ok());
}

#[test]
fn gap6_endpoint_without_a_matching_label_is_still_rejected() {
    assert!(topology_edge(vec!["Robot".into()]).is_err());
}

#[test]
fn gap6_topology_is_not_checked_in_open_mode() {
    let mut graph = empty_graph(Mode::Open);
    let a = graph
        .add_vertex(
            Some("a".into()),
            vec!["Robot".into()],
            props(&[("name", s("R"))]),
        )
        .unwrap();
    let c = graph
        .add_vertex(
            Some("c".into()),
            vec!["Company".into()],
            props(&[("name", s("A"))]),
        )
        .unwrap();
    assert!(
        graph
            .add_edge(
                Some("e".into()),
                Some("works_at".into()),
                a,
                c,
                props(&[("since", n(1))])
            )
            .is_ok()
    );
}

// ---------------------------------------------------------------------------------------------
// Gap 7: null vs absent, Open requiring declared fields
// ---------------------------------------------------------------------------------------------

#[test]
fn gap7_add_without_a_non_nullable_field_is_rejected_in_open() {
    let mut graph = empty_graph(Mode::Open);
    let r = graph.add_vertex(
        Some("p".into()),
        vec!["Person".into()],
        props(&[("name", s("Ann"))]),
    );
    assert!(
        matches!(
            r,
            Err(GraphError::Schema(
                SchemaError::MissingRequiredProperty { .. }
            ))
        ),
        "{r:?}"
    );
}

#[test]
fn gap7_add_without_a_non_nullable_field_is_rejected_in_closed() {
    let mut graph = empty_graph(Mode::Closed);
    let r = graph.add_vertex(
        Some("p".into()),
        vec!["Person".into()],
        props(&[("name", s("Ann"))]),
    );
    assert!(
        matches!(
            r,
            Err(GraphError::Schema(
                SchemaError::MissingRequiredProperty { .. }
            ))
        ),
        "{r:?}"
    );
}

#[test]
fn gap7_none_mode_accepts_any_omission() {
    let mut graph = empty_graph(Mode::None);
    assert!(
        graph
            .add_vertex(
                Some("p".into()),
                vec!["Person".into()],
                props(&[("name", s("Ann"))])
            )
            .is_ok()
    );
}

/// Guard (audit claim "Open rejects Null on a non-nullable field on set" confirmed).
#[test]
fn gap7_setting_a_non_nullable_field_to_null_is_rejected_in_open_and_closed() {
    for mode in ENFORCING {
        let mut graph = empty_graph(mode);
        let v = graph
            .add_vertex(Some("p".into()), vec!["Person".into()], person_props())
            .unwrap();
        let r = graph.set_vertex_property(v, "age".into(), ElementProperty::Null);
        assert!(r.is_err(), "{mode:?}");
        let r = graph.set_vertex_property(v, "name".into(), ElementProperty::Null);
        assert!(r.is_err(), "{mode:?}");
    }
}

/// An optional key whose type admits null may be absent or null, in both modes.
#[test]
fn gap7_nullable_field_may_be_absent_or_null() {
    for mode in ENFORCING {
        let mut graph = empty_graph(mode);
        let v = graph
            .add_vertex(Some("p".into()), vec!["Person".into()], person_props())
            .unwrap();
        assert!(
            graph
                .set_vertex_property(v, "bio".into(), ElementProperty::Null)
                .is_ok(),
            "{mode:?}"
        );
        let mut with_null = person_props();
        with_null.insert("bio", ElementProperty::Null);
        assert!(
            graph
                .add_vertex(Some("q".into()), vec!["Person".into()], with_null)
                .is_ok(),
            "{mode:?}"
        );
    }
}

#[test]
fn gap7_none_mode_accepts_null_everywhere() {
    let mut graph = empty_graph(Mode::None);
    let v = graph
        .add_vertex(Some("p".into()), vec!["Person".into()], person_props())
        .unwrap();
    assert!(
        graph
            .set_vertex_property(v, "age".into(), ElementProperty::Null)
            .is_ok()
    );
}
