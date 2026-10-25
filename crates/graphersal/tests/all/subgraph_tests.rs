//! `subgraph("sg")` + `cap("sg")` is a graph value, and `to_graph()` its short form: content (edges
//! plus both endpoint vertices, ids, vertex label sets and nested properties), snapshot semantics,
//! schema copy, an explicit `withSideEffect("sg", graph)` target, bulk invisibility, the Rhai
//! notation and the error cases.

use graphersal::prelude::*;
use std::sync::Arc;

fn s(text: &str) -> compact_str::CompactString {
    compact_str::CompactString::new(text)
}

fn obj(entries: Vec<(&str, ElementProperty)>) -> ElementProperty {
    let mut map = PropertyMap::default();
    for (key, value) in entries {
        map.insert(s(key), value);
    }
    ElementProperty::Object(Box::new(map))
}

fn props(entries: Vec<(&str, ElementProperty)>) -> PropertyMap {
    match obj(entries) {
        ElementProperty::Object(map) => *map,
        _ => unreachable!(),
    }
}

fn text(value: &str) -> ElementProperty {
    ElementProperty::String(s(value))
}

fn int(value: i64) -> ElementProperty {
    ElementProperty::from(value)
}

/// a(Person,Admin) -e1-> b(Person), a -e2-> c(unlabeled), b -e3-> c, c -e4-> d(City).
/// Vertex `a` and edge `e1` carry nested properties. The vertices are added in the order d, c, b, a
/// so that the source handles differ from the order a copy creates them in.
fn source() -> TraversalGraph {
    let mut tg = TraversalGraph::new();
    let d = tg
        .add_vertex(
            Some(s("d")),
            vec![s("City")],
            props(vec![("name", text("Berlin"))]),
        )
        .unwrap();
    let c = tg
        .add_vertex(Some(s("c")), vec![], PropertyMap::default())
        .unwrap();
    let b = tg
        .add_vertex(
            Some(s("b")),
            vec![s("Person")],
            props(vec![("name", text("bob"))]),
        )
        .unwrap();
    let profile = obj(vec![
        ("city", text("Berlin")),
        ("tags", ElementProperty::Array(vec![text("x"), text("y")])),
        ("geo", obj(vec![("lat", ElementProperty::from(1.5f64))])),
    ]);
    let a = tg
        .add_vertex(
            Some(s("a")),
            vec![s("Person"), s("Admin")],
            props(vec![("name", text("alice")), ("profile", profile)]),
        )
        .unwrap();
    let meta = obj(vec![(
        "w",
        ElementProperty::Array(vec![int(1), int(2), obj(vec![("k", text("v"))])]),
    )]);
    tg.add_edge(
        Some(s("e1")),
        Some(s("knows")),
        a,
        b,
        props(vec![("since", int(2020)), ("meta", meta)]),
    )
    .unwrap();
    tg.add_edge(
        Some(s("e2")),
        Some(s("knows")),
        a,
        c,
        PropertyMap::default(),
    )
    .unwrap();
    tg.add_edge(
        Some(s("e3")),
        Some(s("likes")),
        b,
        c,
        PropertyMap::default(),
    )
    .unwrap();
    tg.add_edge(
        Some(s("e4")),
        Some(s("lives")),
        c,
        d,
        PropertyMap::default(),
    )
    .unwrap();
    tg
}

fn ids_of(graph: &TraversalGraph) -> (Vec<String>, Vec<String>) {
    let mut vertices: Vec<String> = graph
        .vertices()
        .map(|v| graph.vertex_id(v).unwrap().to_string())
        .collect();
    let mut edges: Vec<String> = graph
        .edges()
        .map(|e| graph.edge_id(e).unwrap().to_string())
        .collect();
    vertices.sort();
    edges.sort();
    (vertices, edges)
}

fn shared(graph: TraversalGraph) -> Arc<Graph> {
    Arc::new(Graph::new(graph))
}

mod rust_api {
    use super::*;

    #[test]
    fn vertices_labels_and_nested_properties_are_copied_at_full_depth() {
        let src = source();
        let sub = src
            .traversal()
            .e("e1")
            .subgraph("sg")
            .cap("sg")
            .to_graph()
            .unwrap();
        let sub = sub.read();
        assert_eq!(
            ids_of(&sub),
            (
                vec!["a".to_string(), "b".to_string()],
                vec!["e1".to_string()]
            )
        );

        let a = sub.get_vertex_by_id("a").unwrap();
        let src_a = src.get_vertex_by_id("a").unwrap();
        assert_eq!(
            sub.vertex_labels(a),
            &[s("Person"), s("Admin")],
            "the whole label set, in order"
        );
        assert_eq!(
            sub.vertex_properties(a),
            src.vertex_properties(src_a),
            "nested object/array props"
        );
        let profile = sub.vertex_properties(a).unwrap().get("profile").unwrap();
        assert!(matches!(profile, ElementProperty::Object(_)));

        let b = sub.get_vertex_by_id("b").unwrap();
        assert_eq!(sub.vertex_labels(b), &[s("Person")]);

        let e1 = sub.get_edge_by_id("e1").unwrap();
        let src_e1 = src.get_edge_by_id("e1").unwrap();
        assert_eq!(sub.edge_label(e1), Some("knows"));
        assert_eq!(sub.edge_properties(e1), src.edge_properties(src_e1));
        let (from, to) = sub.edge_endpoints(e1).unwrap();
        assert_eq!(
            (sub.vertex_id(from), sub.vertex_id(to)),
            (Some("a"), Some("b")),
            "direction kept"
        );
    }

    #[test]
    fn an_unlabeled_vertex_stays_unlabeled() {
        let src = source();
        let sub = src.traversal().e("e2").to_graph().unwrap();
        let sub = sub.read();
        let c = sub.get_vertex_by_id("c").unwrap();
        assert!(sub.vertex_labels(c).is_empty());
    }

    #[test]
    fn ids_are_preserved_even_when_the_handles_differ() {
        let src = source();
        // e4 = c -> d: in the copy c is handle 0, in the source it is handle 1.
        let sub = src
            .traversal()
            .e("e4")
            .subgraph("sg")
            .cap("sg")
            .to_graph()
            .unwrap();
        let sub = sub.read();
        assert_eq!(
            ids_of(&sub),
            (
                vec!["c".to_string(), "d".to_string()],
                vec!["e4".to_string()]
            )
        );
        assert_eq!(
            sub.vertex_properties(sub.get_vertex_by_id("d").unwrap())
                .unwrap()
                .get("name"),
            Some(&text("Berlin"))
        );
        // Every id is unique: the whole graph copies without a unique-constraint error.
        let all = src.traversal().e(None).to_graph().unwrap();
        assert_eq!(ids_of(&all.read()), ids_of(&src));
    }

    #[test]
    fn numeric_ids_round_trip_and_new_ids_do_not_collide() {
        let src = GraphSource::tinkerpop_modern();
        let src = src.read();
        let sub = src.traversal().v("1").out_e(None).to_graph().unwrap();
        let mut sub = sub.write();
        let (vertex_ids, edge_ids) = ids_of(&sub);
        let source_edge_ids: Vec<String> = {
            let mut ids: Vec<String> = src
                .out_edges(src.get_vertex_by_id("1").unwrap())
                .map(|e| src.edge_id(e).unwrap().to_string())
                .collect();
            ids.sort();
            ids
        };
        assert_eq!(edge_ids, source_edge_ids);
        assert_eq!(vertex_ids, vec!["1", "2", "3", "4"]);
        // A later insert gets a fresh id, never one of the copied ones.
        let fresh = sub
            .add_vertex(None, vec![], PropertyMap::default())
            .unwrap();
        let fresh_id = sub.vertex_id(fresh).unwrap().to_string();
        assert!(
            !vertex_ids.contains(&fresh_id),
            "fresh id {fresh_id} collides"
        );
        let edge_fresh = sub
            .add_edge(None, Some(s("x")), fresh, fresh, PropertyMap::default())
            .unwrap();
        assert!(!edge_ids.contains(&sub.edge_id(edge_fresh).unwrap().to_string()));
    }

    #[test]
    fn edges_sharing_vertices_do_not_duplicate_them() {
        let src = source();
        let sub = src
            .traversal()
            .v("a")
            .out_e(None)
            .subgraph("sg")
            .cap("sg")
            .to_graph()
            .unwrap();
        let sub = sub.read();
        assert_eq!(sub.edge_count(), 2);
        assert_eq!(sub.vertex_count(), 3, "a is shared by e1 and e2");
        let all = src.traversal().e(None).to_graph().unwrap();
        assert_eq!(all.read().vertex_count(), 4);
        assert_eq!(all.read().edge_count(), 4);
    }

    #[test]
    fn vertices_without_a_selected_edge_are_not_included() {
        let src = source();
        let sub = src.traversal().e("e1").to_graph().unwrap();
        assert_eq!(sub.read().vertex_count(), 2);
        assert!(sub.read().get_vertex_by_id("c").is_none());
    }

    #[test]
    fn an_empty_subgraph_is_an_empty_graph() {
        let src = source();
        let sub = src
            .traversal()
            .e(None)
            .has_label("nope")
            .subgraph("sg")
            .cap("sg")
            .to_graph()
            .unwrap();
        assert_eq!((sub.read().vertex_count(), sub.read().edge_count()), (0, 0));
        let direct = src
            .traversal()
            .e(None)
            .has_label("nope")
            .to_graph()
            .unwrap();
        assert_eq!(
            (direct.read().vertex_count(), direct.read().edge_count()),
            (0, 0)
        );
    }

    #[test]
    fn cap_works_without_a_to_graph_and_lists_the_content() {
        let src = source();
        let listed = src
            .traversal()
            .e("e1")
            .subgraph("sg")
            .cap("sg")
            .next()
            .unwrap()
            .unwrap();
        let ElementProperty::Object(map) = listed else {
            panic!("object expected")
        };
        assert!(matches!(map.get("vertices"), Some(ElementProperty::Array(v)) if v.len() == 2));
        assert!(matches!(map.get("edges"), Some(ElementProperty::Array(e)) if e.len() == 1));
        // select("sg") reads the same graph.
        let selected = src
            .traversal()
            .e("e1")
            .subgraph("sg")
            .select("sg")
            .next()
            .unwrap()
            .unwrap();
        assert!(matches!(selected, ElementProperty::Object(_)));
    }

    #[test]
    fn the_stream_continues_unchanged_after_subgraph() {
        let src = source();
        let count = src
            .traversal()
            .e(None)
            .subgraph("sg")
            .count()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(count.as_i64(), Some(4));
    }

    #[test]
    fn the_copy_is_a_snapshot_and_the_source_stays_untouched() {
        let src = source();
        let sub = src.traversal().e("e1").to_graph().unwrap();
        {
            let mut sub = sub.write();
            sub.traversal_mut()
                .v("a")
                .property("name", "zed")
                .to_list()
                .unwrap();
            sub.traversal_mut().v("b").drop().to_list().unwrap();
        }
        assert_eq!(sub.read().vertex_count(), 1);
        let a = src.get_vertex_by_id("a").unwrap();
        assert_eq!(
            src.vertex_properties(a).unwrap().get("name"),
            Some(&text("alice"))
        );
        assert_eq!((src.vertex_count(), src.edge_count()), (4, 4));
        // And the other way round: a later change of the source does not reach the copy.
        let sub2 = src.traversal().e("e1").to_graph().unwrap();
        let mut src = src;
        src.traversal_mut()
            .v("a")
            .property("name", "late")
            .to_list()
            .unwrap();
        let sub2 = sub2.read();
        let a2 = sub2.get_vertex_by_id("a").unwrap();
        assert_eq!(
            sub2.vertex_properties(a2).unwrap().get("name"),
            Some(&text("alice"))
        );
    }

    #[test]
    fn bulk_merging_does_not_change_the_subgraph() {
        let src = source();
        let counts = |merge: bool| {
            let sub = src
                .traversal()
                .with("bulk.merge", merge)
                .v(None)
                .both(None)
                .barrier()
                .both_e(None)
                .subgraph("sg")
                .cap("sg")
                .to_graph()
                .unwrap();
            let sub = sub.read();
            (ids_of(&sub), sub.vertex_count(), sub.edge_count())
        };
        let merged = counts(true);
        assert_eq!(merged, counts(false));
        assert_eq!(merged.1, 4);
        assert_eq!(merged.2, 4);
    }

    #[test]
    fn repeated_visits_collect_an_edge_once() {
        let src = source();
        let sub = src
            .traversal()
            .v(None)
            .repeat(__::both_e(None).subgraph("sg").other_v())
            .times(3)
            .cap("sg")
            .to_graph()
            .unwrap();
        assert_eq!(sub.read().edge_count(), 4);
        assert_eq!(sub.read().vertex_count(), 4);
    }

    #[test]
    fn cap_twice_gives_equal_independent_snapshots() {
        let src = source();
        let both = src
            .traversal()
            .e(None)
            .subgraph("sg")
            .cap(vec!["sg"])
            .next()
            .unwrap();
        assert!(both.is_some());
        let first = src
            .traversal()
            .e("e1")
            .subgraph("sg")
            .cap("sg")
            .to_graph()
            .unwrap();
        let second = src
            .traversal()
            .e("e1")
            .subgraph("sg")
            .cap("sg")
            .to_graph()
            .unwrap();
        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(ids_of(&first.read()), ids_of(&second.read()));
    }

    #[test]
    fn the_source_works_through_any_storage_wrapper() {
        // `g.traversal()` over `&TraversalGraph`, a `Box` and the shared handle all copy alike.
        let src = source();
        let boxed: Box<TraversalGraph> = Box::new(source());
        let from_box = GraphTraversalSource::new(boxed).e(None).to_graph().unwrap();
        assert_eq!(ids_of(&from_box.read()), ids_of(&src));
    }

    // ---- to_graph forms -------------------------------------------------------------------

    #[test]
    fn to_graph_builds_from_edge_streams() {
        let src = source();
        let one = src.traversal().e("e1").to_graph().unwrap();
        assert_eq!((one.read().vertex_count(), one.read().edge_count()), (2, 1));
        let both = src.traversal().v("a").both_e(None).to_graph().unwrap();
        assert_eq!(
            (both.read().vertex_count(), both.read().edge_count()),
            (3, 2)
        );
        let by_label = src
            .traversal()
            .e(None)
            .has_label("knows")
            .to_graph()
            .unwrap();
        assert_eq!(by_label.read().edge_count(), 2);
    }

    #[test]
    fn to_graph_of_a_vertex_stream_is_an_error_with_a_hint() {
        let src = source();
        let err = src
            .traversal()
            .v("a")
            .to_graph()
            .expect_err("vertices are not edges");
        let report = format_diagnostic(&err);
        assert!(report.contains("to_graph() needs edges"), "{report}");
        assert!(report.contains("g.e(0).to_graph()"), "{report}");
        let err = src
            .traversal()
            .e(None)
            .values("since")
            .to_graph()
            .expect_err("values are not edges");
        assert!(format_diagnostic(&err).contains("to_graph() needs edges"));
    }

    #[test]
    fn to_graph_of_a_cap_returns_the_graph_itself_and_rejects_a_mix() {
        let src = source();
        let sub = src
            .traversal()
            .e("e1")
            .subgraph("sg")
            .cap("sg")
            .to_graph()
            .unwrap();
        assert_eq!(sub.read().edge_count(), 1);
        let err = src
            .traversal()
            .e("e1")
            .subgraph("sg")
            .cap(vec!["sg", "sg"])
            .to_graph()
            .expect_err("a map of graphs is not a graph");
        assert!(format_diagnostic(&err).contains("to_graph() needs edges"));
    }

    // ---- schema ---------------------------------------------------------------------------

    #[cfg(feature = "serde")]
    fn schema_json(mode: &str) -> String {
        format!(
            r#"{{
                "mode": "{mode}",
                "vertices": {{
                    "Person": {{ "schema": {{ "type": "object",
                        "properties": {{ "name": {{ "type": "string" }} }}, "required": ["name"] }} }}
                }},
                "edges": {{
                    "knows": {{ "schema": {{ "type": "object",
                        "properties": {{ "since": {{ "type": "integer" }} }}, "required": ["since"] }} }}
                }}
            }}"#
        )
    }

    #[cfg(feature = "serde")]
    fn schema_source(mode: &str) -> TraversalGraph {
        let mut tg = TraversalGraph::new();
        let a = tg
            .add_vertex(
                Some(s("a")),
                vec![s("Person")],
                props(vec![("name", text("alice"))]),
            )
            .unwrap();
        let b = tg
            .add_vertex(
                Some(s("b")),
                vec![s("Person")],
                props(vec![("name", text("bob"))]),
            )
            .unwrap();
        tg.add_edge(
            Some(s("e1")),
            Some(s("knows")),
            a,
            b,
            props(vec![("since", int(2020))]),
        )
        .unwrap();
        tg.set_schema(GraphSchema::from_json(&schema_json(mode)).unwrap())
            .unwrap();
        tg
    }

    #[cfg(feature = "serde")]
    #[test]
    fn a_closed_schema_is_copied_and_enforced_in_the_subgraph() {
        let src = schema_source("closed");
        let sub = src
            .traversal()
            .e("e1")
            .subgraph("sg")
            .cap("sg")
            .to_graph()
            .unwrap();
        let mut sub = sub.write();
        let schema = sub.get_schema().expect("schema copied");
        assert_eq!(schema.mode, SchemaMode::Closed);
        assert!(schema.vertices.contains_key("Person") && schema.edges.contains_key("knows"));
        // Enforced: an undeclared label is rejected in the copy.
        assert!(
            sub.add_vertex(None, vec![s("Unknown")], PropertyMap::default())
                .is_err()
        );
        // The copy's schema is its own: clearing it leaves the source enforcing.
        sub.set_schema(GraphSchema::new()).unwrap();
        assert!(
            sub.add_vertex(None, vec![s("Unknown")], PropertyMap::default())
                .is_ok()
        );
        let mut src = src;
        assert!(
            src.add_vertex(None, vec![s("Unknown")], PropertyMap::default())
                .is_err()
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn an_open_schema_is_copied_and_coerces_in_the_subgraph() {
        let src = schema_source("open");
        let sub = src.traversal().e("e1").to_graph().unwrap();
        let mut sub = sub.write();
        assert_eq!(
            sub.get_schema().expect("schema copied").mode,
            SchemaMode::Open
        );
        // Open: a declared field is validated, an undeclared one is allowed.
        assert!(
            sub.add_vertex(None, vec![s("Person")], props(vec![("name", int(1))]))
                .is_err()
        );
        assert!(
            sub.add_vertex(None, vec![s("Other")], props(vec![("x", int(1))]))
                .is_ok()
        );
    }

    #[test]
    fn a_graph_without_a_stored_schema_gives_a_subgraph_without_one() {
        let src = source();
        assert!(src.get_schema().is_none());
        let sub = src.traversal().e("e1").to_graph().unwrap();
        let mut sub = sub.write();
        assert!(
            sub.get_schema().is_none(),
            "an inferred schema is never copied as a stored one"
        );
        sub.add_vertex(None, vec![s("Later")], PropertyMap::default())
            .unwrap();
        assert!(
            sub.traversal()
                .infer_schema()
                .unwrap()
                .vertices
                .contains_key("Later"),
            "inference sees the later vertex"
        );
    }

    // ---- explicit target ------------------------------------------------------------------

    #[test]
    fn an_explicit_target_is_written_and_returned_by_cap() {
        let src = source();
        let target = shared(TraversalGraph::new());
        let capped = src
            .traversal()
            .with_side_effect_graph("sg", target.clone())
            .e("e1")
            .subgraph("sg")
            .cap("sg")
            .to_graph()
            .unwrap();
        assert!(
            Arc::ptr_eq(&capped, &target),
            "cap returns the target itself"
        );
        assert_eq!(
            ids_of(&target.read()),
            (
                vec!["a".to_string(), "b".to_string()],
                vec!["e1".to_string()]
            )
        );
    }

    #[test]
    fn an_explicit_target_is_filled_even_without_cap() {
        let src = source();
        let target = shared(TraversalGraph::new());
        src.traversal()
            .with_side_effect_graph("sg", target.clone())
            .e(None)
            .subgraph("sg")
            .to_list()
            .unwrap();
        assert_eq!(target.read().edge_count(), 4);
        assert_eq!(target.read().vertex_count(), 4);
    }

    #[test]
    fn writing_the_same_edges_twice_is_idempotent_and_prefilled_vertices_are_reused() {
        let src = source();
        let mut pre = TraversalGraph::new();
        pre.add_vertex(
            Some(s("a")),
            vec![s("Person")],
            props(vec![("name", text("pre-existing"))]),
        )
        .unwrap();
        let target = shared(pre);
        for _ in 0..2 {
            src.traversal()
                .with_side_effect_graph("sg", target.clone())
                .e("e1")
                .subgraph("sg")
                .to_list()
                .unwrap();
        }
        let target = target.read();
        assert_eq!((target.vertex_count(), target.edge_count()), (2, 1));
        let a = target.get_vertex_by_id("a").unwrap();
        assert_eq!(
            target.vertex_properties(a).unwrap().get("name"),
            Some(&text("pre-existing")),
            "reused as is"
        );
    }

    #[test]
    fn a_stored_schema_of_the_target_is_not_overwritten() {
        let src = source();
        let mut pre = TraversalGraph::new();
        let mut schema = GraphSchema::new();
        schema.mode = SchemaMode::Open;
        pre.set_schema(schema).unwrap();
        let target = shared(pre);
        src.traversal()
            .with_side_effect_graph("sg", target.clone())
            .e("e1")
            .subgraph("sg")
            .to_list()
            .unwrap();
        assert_eq!(
            target.read().get_schema().map(|s| s.mode),
            Some(SchemaMode::Open)
        );
    }

    #[test]
    fn the_traversed_graph_is_not_a_valid_target() {
        let src = Arc::new(shared(source()));
        let guard = src.read();
        let err = guard
            .traversal()
            .with_side_effect_graph("sg", (*src).clone())
            .e(None)
            .subgraph("sg")
            .to_list()
            .expect_err("writing the traversed graph must fail, not deadlock");
        let report = format_diagnostic(&err);
        assert!(report.contains("is locked"), "{report}");
        assert!(report.contains("never the graph you traverse"), "{report}");
    }

    #[test]
    fn a_non_subgraph_bucket_cannot_collect_a_subgraph() {
        let src = source();
        let err = src
            .traversal()
            .with_side_effect("sg", 1)
            .e(None)
            .subgraph("sg")
            .to_list()
            .expect_err("sg already holds a value");
        let report = format_diagnostic(&err);
        assert!(
            report.contains("already holds a value that is not a subgraph"),
            "{report}"
        );
        assert!(
            report.contains("with_side_effect(\"sg\", target)"),
            "{report}"
        );
    }

    // ---- large graph ----------------------------------------------------------------------

    #[test]
    fn the_large_graph_copies_whole_and_in_part() {
        let graph = GraphSource::large_110k();
        let src = graph.read();
        let whole = src.traversal().e(None).to_graph().unwrap();
        assert_eq!(whole.read().edge_count(), src.edge_count());
        assert!(whole.read().vertex_count() <= src.vertex_count());
        let some = src
            .traversal()
            .e(None)
            .limit(1000)
            .subgraph("sg")
            .cap("sg")
            .to_graph()
            .unwrap();
        let some = some.read();
        assert_eq!(some.edge_count(), 1000);
        // Every edge of the copy has both endpoints in the copy.
        for edge in some.edges() {
            let (from, to) = some.edge_endpoints(edge).unwrap();
            assert!(some.vertex_id(from).is_some() && some.vertex_id(to).is_some());
        }
    }
}

#[cfg(feature = "script")]
mod dsl {
    use super::*;
    use crate::script_host::{eval_value, rhai};

    fn run(script: &str) -> Result<rhai::Dynamic, String> {
        eval_value(Arc::new(GraphSource::tinkerpop_modern()), script).map_err(|e| e.to_string())
    }

    fn int_of(script: &str) -> i64 {
        run(script)
            .unwrap_or_else(|e| panic!("{script}: {e}"))
            .as_int()
            .expect("int result")
    }

    #[test]
    fn the_cap_value_is_a_traversal_source() {
        assert_eq!(
            int_of(r#"let sub = g.e(0).subgraph("sg").cap("sg").next(); sub.v().count().next()"#),
            2
        );
        assert_eq!(
            int_of(r#"let sub = g.e(0).subgraph("sg").cap("sg").next(); sub.e().count().next()"#),
            1
        );
        // The camelCase twin and the list form.
        assert_eq!(
            int_of(
                r#"let sub = g.e(0).subgraph("sg").cap("sg").to_list()[0]; sub.v().count().next()"#
            ),
            2
        );
    }

    #[test]
    fn the_example_of_the_task_prints_the_vertices() {
        let json = run(
            r#"let sub = g.e(0).subgraph("sg").cap("sg").next(); sub.v().to_json().to_string()"#,
        )
        .unwrap();
        let json = json.into_string().unwrap();
        assert!(json.contains("marko") && json.contains("vadas"), "{json}");
        assert!(!json.contains("josh"), "{json}");
    }

    #[test]
    fn to_graph_forms() {
        assert_eq!(int_of(r#"g.e(0).to_graph().v().count().next()"#), 2);
        assert_eq!(int_of(r#"g.e(0).to_graph().v().count().next()"#), 2);
        assert_eq!(
            int_of(r#"g.v(1).both_e().to_graph().v().count().next()"#),
            4
        );
        assert_eq!(int_of(r#"g.v(1).out_e().to_graph().e().count().next()"#), 3);
        assert_eq!(
            int_of(r#"g.v().out_e("noexist").to_graph().v().count().next()"#),
            0
        );
        // A cap value passes through to_graph unchanged.
        assert_eq!(
            int_of(r#"g.e(0).subgraph("sg").cap("sg").to_graph().v().count().next()"#),
            2
        );
    }

    #[test]
    fn to_graph_on_a_vertex_stream_is_an_error() {
        for script in [r#"g.v(1).to_graph()"#, r#"g.V().to_graph()"#] {
            let err = run(script).expect_err("vertices are not edges");
            assert!(err.contains("to_graph() needs edges"), "{err}");
            assert!(err.contains("g.e(0).to_graph()"), "{err}");
        }
    }

    #[test]
    fn ids_labels_and_properties_survive() {
        assert_eq!(
            run(r#"let sub = g.v(1).out_e("knows").to_graph(); sub.v().has_label("person").values("name").order().to_list()"#)
                .unwrap()
                .into_array()
                .unwrap()
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>(),
            vec!["josh", "marko", "vadas"]
        );
        assert_eq!(
            int_of(r#"let sub = g.e(0).to_graph(); sub.v("1").values("age").next()"#),
            29
        );
        assert_eq!(
            run(r#"let sub = g.e(0).to_graph(); sub.e(0).values("weight").next()"#)
                .unwrap()
                .as_float()
                .unwrap(),
            0.5
        );
    }

    #[test]
    fn mutating_the_subgraph_leaves_the_source_alone() {
        assert_eq!(
            int_of(
                r#"let sub = g.v(1).both_e().to_graph();
                   sub.v().drop().to_list();
                   sub.add_v("person").to_list();
                   g.v().count().next()"#
            ),
            6
        );
        assert_eq!(
            int_of(
                r#"let sub = g.e(0).to_graph(); sub.add_v("x").to_list(); sub.v().count().next()"#
            ),
            3
        );
        assert_eq!(
            run(r#"let sub = g.e(0).to_graph(); sub.v("1").property("name", "changed").to_list(); g.v("1").values("name").next()"#)
                .unwrap()
                .to_string(),
            "marko"
        );
    }

    #[test]
    fn with_side_effect_graph_target() {
        // A target from an earlier to_graph, extended by a second subgraph.
        assert_eq!(
            int_of(
                r#"let t = g.e(0).to_graph();
                   g.with_side_effect("sg", t).v(1).out_e("created").subgraph("sg").to_list();
                   t.e().count().next()"#
            ),
            2
        );
        assert_eq!(
            int_of(
                r#"let t = GraphSource::empty();
                   let r = g.with_side_effect("sg", t).e().subgraph("sg").cap("sg").next();
                   t.e().count().next() * 100 + r.v().count().next()"#
            ),
            606
        );
    }

    #[test]
    fn the_session_graph_is_not_a_valid_target() {
        let err =
            run(r#"g.withSideEffect("sg", g).e().subgraph("sg").toList()"#).expect_err("must fail");
        assert!(err.contains("is locked"), "{err}");
    }

    #[test]
    fn subgraph_does_not_make_a_traversal_mutating() {
        // Collecting into a new graph never needs the write lock of the source: it runs while a
        // read lock of the source is held elsewhere.
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let _reader = graph.read();
        let result = eval_value(
            graph.clone(),
            r#"g.e(0).subgraph("sg").cap("sg").next().v().count().next()"#,
        );
        assert_eq!(result.unwrap().as_int().unwrap(), 2);
    }

    #[test]
    fn the_listing_of_a_graph_value_shows_vertices_and_edges() {
        let listed = run(r#"g.e(0).subgraph("sg").cap("sg").to_json().to_string()"#)
            .unwrap()
            .into_string()
            .unwrap();
        assert!(
            listed.contains("\"vertices\"") && listed.contains("\"edges\""),
            "{listed}"
        );
    }

    #[test]
    fn a_graph_value_answers_to_gtype_graph() {
        assert_eq!(
            int_of(r#"g.e(0).subgraph("sg").cap("sg").is(P.typeOf(GType.GRAPH)).count().next()"#),
            1
        );
        assert_eq!(
            int_of(r#"g.e(0).subgraph("sg").cap("sg").is(P.typeOf(GType.GRAPH)).count().next()"#),
            1
        );
        assert_eq!(
            int_of(r#"g.v().is(P.typeOf(GType.GRAPH)).count().next()"#),
            0
        );
    }

    #[test]
    fn subgraph_without_a_label_names_the_working_forms() {
        let err = run("g.e(0).subgraph()").expect_err("no label");
        assert!(
            err.contains(r#"subgraph("sg")"#)
                && err.contains(r#"cap("sg")"#)
                && err.contains("to_graph()"),
            "{err}"
        );
    }

    #[test]
    fn subgraph_over_vertices_is_an_error_with_the_edge_forms() {
        for script in [
            r#"g.v(1).subgraph("sg").cap("sg").next()"#,
            r#"g.v(1).subgraph("sg").toList()"#,
            r#"g.v(1).local(__.out().subgraph("sg")).cap("sg").next()"#,
        ] {
            let err = run(script).expect_err(script);
            assert!(
                err.contains("collects edges, but the traversal yields a vertex"),
                "{script}: {err}"
            );
            assert!(err.contains("both_e()"), "{err}");
        }
        // Edge streams (also nested), empty streams and a mixed stream.
        assert_eq!(
            int_of(
                r#"g.v(1).local(__.out_e().subgraph("sg")).cap("sg").next().e().count().next()"#
            ),
            3
        );
        assert_eq!(
            int_of(r#"g.e().has("nope").subgraph("sg").cap("sg").next().v().count().next()"#),
            0
        );
        assert!(run(r#"g.union(__.v(1), __.e(0)).subgraph("sg").cap("sg").next()"#).is_err());
    }

    /// A single list-valued traverser is a list of ONE element for `to_list()` and `to_json()`.
    #[test]
    fn single_list_valued_results_agree_between_terminals() {
        for source in [
            r#"g.v().values("age").fold()"#,
            r#"g.v().values("age").aggregate("a").cap("a")"#,
        ] {
            assert_eq!(int_of(&format!("{source}.to_list().len()")), 1, "{source}");
            assert_eq!(
                int_of(&format!("{source}.to_list()[0].len()")),
                4,
                "{source}"
            );
            let json = run(&format!("{source}.to_json().to_string()"))
                .unwrap()
                .into_string()
                .unwrap();
            let compact: String = json.chars().filter(|c| !c.is_whitespace()).collect();
            assert_eq!(compact, "[[29,27,32,35]]", "{source}");
        }
    }
}
