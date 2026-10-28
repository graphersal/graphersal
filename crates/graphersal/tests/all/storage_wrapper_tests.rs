//! The `GraphStorage` wrappers (`Box<G>`, `&mut G`, `Arc<TraversalGraph>`, `&TraversalGraph`)
//! behave exactly like the graph they wrap: one full read/write lifecycle runs against each
//! writable wrapper and must give the same observable results, a read-only view and a shared
//! `Arc` reject every write with a reason and leave the graph unchanged, and a change set
//! captured on one graph replays through every writable wrapper.

#![cfg(feature = "serde")]

use std::sync::{Arc, Mutex};

use compact_str::CompactString;
use graphersal::prelude::*;
use graphersal::{
    changes::{ChangeSet, CommitHook, HookError, RollbackReason},
    storage::{EdgeHandle, GraphNodeType, VertexHandle},
};

fn s(text: &str) -> CompactString {
    CompactString::new(text)
}

fn props(entries: &[(&str, ElementProperty)]) -> PropertyMap {
    let mut map = PropertyMap::default();
    for (key, value) in entries {
        map.insert(s(key), value.clone());
    }
    map
}

fn obj(entries: &[(&str, ElementProperty)]) -> ElementProperty {
    ElementProperty::Object(Box::new(props(entries)))
}

fn jpath(text: &str) -> JsonPath {
    text.parse().expect("a valid path")
}

const SCHEMA: &str = r#"{
    "mode": "open",
    "vertices": { "person": { "schema": { "type": "object",
        "properties": { "name": { "type": ["string", "null"] } } } } },
    "edges": { "knows": { "schema": { "type": "object",
        "properties": { "w": { "type": ["integer", "null"] } } } } }
}"#;

/// One lifecycle through `GraphStorage` only: every observable result is asserted, so a wrapper
/// that forwards a call to the wrong method (or swallows it) fails here.
fn lifecycle<S>(storage: &mut S)
where
    S: GraphStorage<VertexId = VertexHandle, EdgeId = EdgeHandle>,
{
    // vertices, labels
    let a = storage
        .add_vertex(
            Some(s("a")),
            vec![s("person")],
            props(&[("name", "A".into())]),
        )
        .unwrap();
    let b = storage
        .add_vertex(Some(s("b")), vec![], PropertyMap::default())
        .unwrap();
    assert_eq!(storage.vertex_count().unwrap(), 2);
    assert!(storage.contains_vertex(a).unwrap() && storage.contains_vertex(b).unwrap());
    assert_eq!(storage.get_vertex_by_id("a").unwrap(), Some(a));
    assert_eq!(storage.vertex_id(b).unwrap(), Some("b"));
    assert_eq!(storage.vertex_label(a).unwrap(), Some("person"));
    assert_eq!(storage.vertex_label(b).unwrap(), None);
    assert_eq!(storage.vertices().unwrap().count(), 2);
    assert_eq!(
        storage
            .vertices_by_label("person")
            .unwrap()
            .collect::<Vec<_>>(),
        vec![a]
    );
    assert_eq!(
        storage
            .statistics()
            .unwrap()
            .expect("statistics")
            .vertex_label_count("person"),
        1
    );

    assert!(storage.add_vertex_label(a, s("employee")).unwrap());
    assert!(
        !storage.add_vertex_label(a, s("employee")).unwrap(),
        "already there"
    );
    assert_eq!(
        storage.vertex_labels(a).unwrap(),
        &[s("person"), s("employee")]
    );
    assert!(storage.remove_vertex_label(a, "employee").unwrap());
    assert!(
        !storage.remove_vertex_label(a, "employee").unwrap(),
        "already gone"
    );
    assert_eq!(storage.vertex_labels(a).unwrap(), &[s("person")]);

    // edges
    let e = storage
        .add_edge(
            Some(s("e")),
            Some(s("knows")),
            a,
            b,
            props(&[("w", 1.into())]),
        )
        .unwrap();
    assert_eq!(storage.edge_count().unwrap(), 1);
    assert!(storage.contains_edge(e).unwrap());
    assert_eq!(storage.get_edge_by_id("e").unwrap(), Some(e));
    assert_eq!(storage.edge_id(e).unwrap(), Some("e"));
    assert_eq!(storage.edge_label(e).unwrap(), Some("knows"));
    assert_eq!(storage.edge_endpoints(e).unwrap(), Some((a, b)));
    assert_eq!(storage.out_edges(a).unwrap().collect::<Vec<_>>(), vec![e]);
    assert_eq!(storage.in_edges(b).unwrap().collect::<Vec<_>>(), vec![e]);
    assert_eq!(storage.in_edges(a).unwrap().count(), 0);
    assert_eq!(storage.edges().unwrap().count(), 1);
    assert_eq!(
        storage.edges_by_label("knows").unwrap().collect::<Vec<_>>(),
        vec![e]
    );
    assert_eq!(
        storage
            .statistics()
            .unwrap()
            .expect("statistics")
            .edge_label_count("knows"),
        1
    );

    // properties, plain and by path
    storage
        .set_vertex_property(a, s("meta"), obj(&[("x", 1.into())]))
        .unwrap();
    storage
        .set_vertex_property_path(a, &jpath("meta.y"), 2.into())
        .unwrap();
    storage
        .remove_vertex_property_path(a, &jpath("meta.x"))
        .unwrap();
    assert_eq!(
        storage.vertex_properties(a).unwrap().unwrap().get("meta"),
        Some(&obj(&[("y", 2.into())]))
    );
    storage.set_edge_property(e, s("info"), obj(&[])).unwrap();
    storage
        .set_edge_property_path(e, &jpath("info.k"), "v".into())
        .unwrap();
    assert_eq!(
        storage.edge_properties(e).unwrap().unwrap().get("info"),
        Some(&obj(&[("k", "v".into())]))
    );
    storage
        .remove_edge_property_path(e, &jpath("info.k"))
        .unwrap();
    assert_eq!(
        storage.edge_properties(e).unwrap().unwrap().get("info"),
        Some(&obj(&[]))
    );
    // a one-segment path removes the whole property
    storage
        .remove_edge_property_path(e, &jpath("info"))
        .unwrap();
    assert!(
        storage
            .edge_properties(e)
            .unwrap()
            .unwrap()
            .get("info")
            .is_none()
    );
    storage.drop_vertex_property("name", a).unwrap();
    assert!(
        storage
            .vertex_properties(a)
            .unwrap()
            .unwrap()
            .get("name")
            .is_none()
    );
    storage.drop_edge_property("w", e).unwrap();
    assert!(
        storage
            .edge_properties(e)
            .unwrap()
            .unwrap()
            .get("w")
            .is_none()
    );

    // schema and diagnostics
    assert!(storage.get_schema().unwrap().is_none());
    storage
        .set_schema(GraphSchema::from_json(SCHEMA).unwrap())
        .unwrap();
    assert_eq!(
        storage.get_schema().unwrap().unwrap().mode,
        SchemaMode::Open
    );
    assert_eq!(
        storage
            .declared_property_schema(GraphNodeType::Vertex, &[s("person")], "name")
            .unwrap()
            .map(|node| node.type_name()),
        Some("union<string|null>".to_string())
    );
    assert_eq!(
        storage
            .declared_property_schema(GraphNodeType::Edge, &[s("knows")], "w")
            .unwrap()
            .map(|node| node.type_name()),
        Some("union<int64|null>".to_string())
    );
    assert_eq!(
        storage
            .declared_property_schema(GraphNodeType::Vertex, &[s("person")], "nope")
            .unwrap(),
        None
    );
    assert!(storage.memory_usage().unwrap().is_some());
    assert!(storage.capabilities().unwrap().writable);

    // removal
    storage.drop_edge(e).unwrap();
    assert!(!storage.contains_edge(e).unwrap());
    assert_eq!(storage.edge_count().unwrap(), 0);
    storage.drop_vertex(b).unwrap();
    assert!(!storage.contains_vertex(b).unwrap());
    assert_eq!(storage.vertex_count().unwrap(), 1);
}

#[test]
fn the_lifecycle_is_the_same_through_every_writable_wrapper() {
    let mut owned = TraversalGraph::new();
    lifecycle(&mut owned);

    let mut boxed: Box<TraversalGraph> = Box::new(TraversalGraph::new());
    lifecycle(&mut boxed);

    let mut inner = TraversalGraph::new();
    let mut by_ref: &mut TraversalGraph = &mut inner;
    lifecycle(&mut by_ref);

    let mut boxed_inner: Box<TraversalGraph> = Box::new(TraversalGraph::new());
    let mut by_ref_box: &mut Box<TraversalGraph> = &mut boxed_inner;
    lifecycle(&mut by_ref_box);

    let mut sole: Arc<TraversalGraph> = Arc::new(TraversalGraph::new());
    lifecycle(&mut sole);
}

/// Every write through `storage` fails with `InvalidElementState` carrying `reason`, and the
/// graph behind it is unchanged.
fn rejects_every_write<S>(storage: &mut S, reason: &str)
where
    S: GraphStorage<VertexId = VertexHandle, EdgeId = EdgeHandle>,
{
    let a = storage.get_vertex_by_id("1").unwrap().expect("marko");
    let b = storage.get_vertex_by_id("2").unwrap().expect("vadas");
    let e = storage.get_edge_by_id("0").unwrap().expect("an edge");
    let check = |result: Result<(), GraphError>, what: &str| match result {
        Err(GraphError::InvalidElementState { reason: got, .. }) => {
            assert!(got.contains(reason), "{what}: {got}")
        }
        other => panic!("{what}: expected InvalidElementState, got {other:?}"),
    };
    check(
        storage
            .add_vertex(None, vec![], PropertyMap::default())
            .map(drop),
        "add_vertex",
    );
    check(
        storage.add_vertex_label(a, s("x")).map(drop),
        "add_vertex_label",
    );
    check(
        storage.remove_vertex_label(a, "person").map(drop),
        "remove_vertex_label",
    );
    check(
        storage
            .add_edge(None, Some(s("l")), a, b, PropertyMap::default())
            .map(drop),
        "add_edge",
    );
    check(
        storage.set_vertex_property(a, s("k"), 1.into()),
        "set_vertex_property",
    );
    check(
        storage.set_edge_property(e, s("k"), 1.into()),
        "set_edge_property",
    );
    check(
        storage.drop_vertex_property("name", a),
        "drop_vertex_property",
    );
    check(
        storage.drop_edge_property("weight", e),
        "drop_edge_property",
    );
    check(storage.drop_edge(e), "drop_edge");
    check(storage.drop_vertex(a), "drop_vertex");
    check(storage.set_schema(GraphSchema::default()), "set_schema");

    assert_eq!(storage.vertex_count().unwrap(), 6);
    assert_eq!(storage.edge_count().unwrap(), 6);
    assert_eq!(storage.vertex_labels(a).unwrap(), &[s("person")]);
    assert!(
        storage
            .vertex_properties(a)
            .unwrap()
            .unwrap()
            .contains_key("name")
    );
    assert!(
        storage
            .edge_properties(e)
            .unwrap()
            .unwrap()
            .contains_key("weight")
    );
}

#[test]
fn a_read_only_reference_rejects_every_write() {
    let graph = TraversalGraph::tinkerpop_modern();
    let mut view: &TraversalGraph = &graph;
    rejects_every_write(&mut view, "immutable graph reference");
    // reads are forwarded
    let marko = view.get_vertex_by_id("1").unwrap().unwrap();
    assert!(view.contains_vertex(marko).unwrap());
    assert_eq!(
        view.statistics()
            .unwrap()
            .expect("statistics")
            .vertex_label_count("person"),
        4
    );
    assert_eq!(
        view.statistics()
            .unwrap()
            .expect("statistics")
            .edge_label_count("created"),
        4
    );
    assert!(view.get_schema().unwrap().is_none());
    assert_eq!(
        view.declared_property_schema(GraphNodeType::Vertex, &[s("person")], "name")
            .unwrap(),
        None
    );
    assert!(view.memory_usage().unwrap().is_some());
}

#[test]
fn a_shared_arc_rejects_every_write_and_a_sole_one_accepts_it() {
    let mut shared = Arc::new(TraversalGraph::tinkerpop_modern());
    let other_owner = shared.clone();
    rejects_every_write(&mut shared, "shared Arc<TraversalGraph>");
    let a = shared.get_vertex_by_id("1").unwrap().unwrap();
    let path_write = shared.set_vertex_property_path(a, &jpath("meta.x"), 1.into());
    assert!(matches!(
        path_write,
        Err(GraphError::InvalidElementState { .. })
    ));
    assert!(
        shared
            .remove_vertex_property_path(a, &jpath("meta.x"))
            .is_err()
    );
    let e = shared.get_edge_by_id("0").unwrap().unwrap();
    assert!(
        shared
            .set_edge_property_path(e, &jpath("meta.x"), 1.into())
            .is_err()
    );
    assert!(
        shared
            .remove_edge_property_path(e, &jpath("meta.x"))
            .is_err()
    );
    assert!(
        !shared.capabilities().unwrap().writable,
        "a shared Arc claims nothing"
    );

    // once the second owner is gone, the same Arc writes again
    drop(other_owner);
    assert!(shared.capabilities().unwrap().writable);
    shared.set_vertex_property(a, s("k"), 1.into()).unwrap();
    assert_eq!(
        shared.vertex_properties(a).unwrap().unwrap().get("k"),
        Some(&1.into())
    );
}

#[test]
fn a_traversal_over_a_read_only_reference_cannot_mutate() {
    let graph = TraversalGraph::tinkerpop_modern();
    let err = GraphTraversalSource::new(&graph)
        .add_v("x")
        .to_list()
        .expect_err("a read-only view cannot add a vertex");
    assert!(
        format_diagnostic(&*err).contains("immutable graph reference"),
        "{}",
        format_diagnostic(&*err)
    );
    assert_eq!(graph.vertex_count(), 6, "nothing was added");
}

// ---------------------------------------------------------------------------------------------
// apply_change_set / commit hooks through the wrappers
// ---------------------------------------------------------------------------------------------

#[derive(Default, Clone)]
struct Recorder(Arc<Mutex<Vec<ChangeSet>>>);

impl CommitHook for Recorder {
    fn before_commit(&mut self, changes: &ChangeSet) -> Result<(), HookError> {
        self.0.lock().unwrap().push(changes.clone());
        Ok(())
    }
}

fn recorded_change_sets() -> Vec<ChangeSet> {
    let mut source = TraversalGraph::new();
    let recorder = Recorder::default();
    source
        .register_commit_hook(Box::new(recorder.clone()))
        .unwrap();
    source
        .traversal_mut()
        .add_v("person")
        .property("name", "ann")
        .as_("a")
        .add_v("person")
        .property("name", "bob")
        .add_e("knows")
        .from("a")
        .to_list()
        .unwrap();
    let sets = recorder.0.lock().unwrap().clone();
    assert_eq!(sets.len(), 1);
    sets
}

fn assert_replayed<S: GraphStorage>(storage: &S) {
    assert_eq!(storage.vertex_count().unwrap(), 2);
    assert_eq!(storage.edge_count().unwrap(), 1);
    assert_eq!(
        storage
            .statistics()
            .unwrap()
            .expect("statistics")
            .edge_label_count("knows"),
        1
    );
}

#[test]
fn a_change_set_replays_through_every_writable_wrapper() {
    let sets = recorded_change_sets();

    let mut boxed: Box<TraversalGraph> = Box::new(TraversalGraph::new());
    GraphStorage::apply_change_set(&mut boxed, &sets[0]).unwrap();
    assert_replayed(&boxed);

    let mut inner = TraversalGraph::new();
    let mut by_ref: &mut TraversalGraph = &mut inner;
    GraphStorage::apply_change_set(&mut by_ref, &sets[0]).unwrap();
    assert_replayed(&inner);

    let mut sole = Arc::new(TraversalGraph::new());
    GraphStorage::apply_change_set(&mut sole, &sets[0]).unwrap();
    assert_replayed(&sole);
}

#[test]
fn commit_hooks_register_through_box_and_mut_ref_and_see_traversal_commits() {
    let recorder = Recorder::default();
    let mut boxed: Box<TraversalGraph> = Box::new(TraversalGraph::new());
    GraphStorage::register_commit_hook(&mut boxed, Box::new(recorder.clone())).unwrap();
    let mut by_ref: &mut Box<TraversalGraph> = &mut boxed;
    GraphStorage::register_commit_hook(&mut by_ref, Box::new(recorder.clone())).unwrap();
    GraphTraversalSource::new(&mut boxed)
        .add_v("x")
        .to_list()
        .unwrap();
    // two hooks, one commit each
    assert_eq!(recorder.0.lock().unwrap().len(), 2);
    assert!(boxed.last_commit_seq().unwrap() > 0);
    let mark = boxed.begin_unit().unwrap();
    boxed
        .rollback_unit(mark, &RollbackReason::failed(&"test"))
        .unwrap();
    assert_eq!(boxed.vertex_count().unwrap(), 1);
}
