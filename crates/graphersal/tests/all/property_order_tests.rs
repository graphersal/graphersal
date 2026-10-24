//! The insertion order of element properties, pinned before the storage representation of
//! `PropertyMap` changes (shared key layouts).
//!
//! The order of an element's properties is visible through `valueMap()`, `elementMap()`,
//! `properties()` and `values()`: a new key goes to the end, an overwrite keeps the position, a
//! removed and re-added key goes to the end. Every way a property map is rebuilt must keep it:
//! the undo log (rollback of a drop, an overwrite, a vertex or edge drop; a failed traversal; a
//! dry run; a nested savepoint), change-set replay, snapshots, journal recovery, the Store and
//! the exchange formats. Elements that share a key set never influence each other. Compressed
//! properties keep their position, and property indexes answer the same after a rollback.

use std::sync::{Arc, Mutex};

use graphersal::catalog::{CompressionDefinition, Definition, PropertyIndexDefinition};
use graphersal::changes::{ChangeSet, CommitHook, HookError, RollbackReason};
use graphersal::prelude::PropertyMap;
use graphersal::prelude::*;
use graphersal::storage::{EdgeHandle, ElementKind, VertexHandle};

// ---------------------------------------------------------------------------------------------
// Observation helpers
// ---------------------------------------------------------------------------------------------

fn props(entries: &[(&str, ElementProperty)]) -> PropertyMap {
    let mut out = PropertyMap::new();
    for (key, value) in entries {
        out.insert(*key, value.clone());
    }
    out
}

/// `[(k, i)]` for the given keys, the value being the position in the list.
fn numbered(keys: &[&str]) -> PropertyMap {
    keys.iter()
        .enumerate()
        .map(|(i, k)| (*k, ElementProperty::from(i as i64)))
        .collect()
}

fn v(graph: &TraversalGraph, id: &str) -> VertexHandle {
    graph.get_vertex_by_id(id).expect("vertex")
}

fn e(graph: &TraversalGraph, id: &str) -> EdgeHandle {
    graph.get_edge_by_id(id).expect("edge")
}

fn keys_of(map: &PropertyMap) -> Vec<String> {
    map.keys().map(|k| k.to_string()).collect()
}

fn entries_of(map: &PropertyMap) -> Vec<(String, ElementProperty)> {
    map.iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

fn single_map(results: Vec<ElementProperty>) -> PropertyMap {
    assert_eq!(results.len(), 1, "expected one map, got {results:?}");
    match results.into_iter().next() {
        Some(ElementProperty::Object(map)) => *map,
        other => panic!("expected a map, got {other:?}"),
    }
}

fn strings(results: Vec<ElementProperty>) -> Vec<String> {
    results
        .into_iter()
        .map(|value| match value {
            ElementProperty::String(s) => s.to_string(),
            other => panic!("expected a string, got {other:?}"),
        })
        .collect()
}

/// The keys of an `elementMap()` without its `id`/`label` (and `IN`/`OUT`) entries, which come
/// first.
fn element_map_property_keys(map: &PropertyMap) -> Vec<String> {
    keys_of(map)
        .into_iter()
        .filter(|k| !matches!(k.as_str(), "id" | "label" | "IN" | "OUT"))
        .collect()
}

/// Everything a reader sees of a vertex's property order: the stored map, `valueMap()`,
/// `elementMap()`, `properties().key()` and `values()`. Asserts that the views agree and returns
/// the stored entries.
fn vertex_view(graph: &TraversalGraph, id: &str) -> Vec<(String, ElementProperty)> {
    let stored = graph
        .vertex_properties(v(graph, id))
        .expect("vertex properties");
    let stored_keys = keys_of(stored);
    let value_map = single_map(graph.traversal().v(id).value_map(None).to_list().unwrap());
    assert_eq!(
        keys_of(&value_map),
        stored_keys,
        "valueMap() of vertex {id}"
    );
    let element_map = single_map(graph.traversal().v(id).element_map(None).to_list().unwrap());
    assert_eq!(
        element_map_property_keys(&element_map),
        stored_keys,
        "elementMap() of vertex {id}"
    );
    let property_keys = strings(
        graph
            .traversal()
            .v(id)
            .properties(None)
            .key()
            .to_list()
            .unwrap(),
    );
    assert_eq!(
        property_keys, stored_keys,
        "properties().key() of vertex {id}"
    );
    let values = graph.traversal().v(id).values(None).to_list().unwrap();
    let stored_values: Vec<ElementProperty> = stored.values().cloned().collect();
    assert_eq!(values, stored_values, "values() of vertex {id}");
    entries_of(stored)
}

/// The edge counterpart of [`vertex_view`]; an edge without a property map shows no entries.
fn edge_view(graph: &TraversalGraph, id: &str) -> Vec<(String, ElementProperty)> {
    let stored = graph
        .edge_properties(e(graph, id))
        .map(entries_of)
        .unwrap_or_default();
    let stored_keys: Vec<String> = stored.iter().map(|(k, _)| k.clone()).collect();
    let value_map = single_map(graph.traversal().e(id).value_map(None).to_list().unwrap());
    assert_eq!(keys_of(&value_map), stored_keys, "valueMap() of edge {id}");
    let element_map = single_map(graph.traversal().e(id).element_map(None).to_list().unwrap());
    assert_eq!(
        element_map_property_keys(&element_map),
        stored_keys,
        "elementMap() of edge {id}"
    );
    let property_keys = strings(
        graph
            .traversal()
            .e(id)
            .properties(None)
            .key()
            .to_list()
            .unwrap(),
    );
    assert_eq!(
        property_keys, stored_keys,
        "properties().key() of edge {id}"
    );
    let values = graph.traversal().e(id).values(None).to_list().unwrap();
    let stored_values: Vec<ElementProperty> = stored.iter().map(|(_, v)| v.clone()).collect();
    assert_eq!(values, stored_values, "values() of edge {id}");
    stored
}

fn vkeys(graph: &TraversalGraph, id: &str) -> Vec<String> {
    vertex_view(graph, id).into_iter().map(|(k, _)| k).collect()
}

fn ekeys(graph: &TraversalGraph, id: &str) -> Vec<String> {
    edge_view(graph, id).into_iter().map(|(k, _)| k).collect()
}

fn owned(keys: &[&str]) -> Vec<String> {
    keys.iter().map(|k| k.to_string()).collect()
}

/// The property order of every element, by id (vertices and edges separately, sorted by id).
type OrderDump = (
    Vec<(String, Vec<(String, ElementProperty)>)>,
    Vec<(String, Vec<(String, ElementProperty)>)>,
);

fn dump(graph: &TraversalGraph) -> OrderDump {
    let mut vertices: Vec<_> = GraphStorage::vertices(graph)
        .map(|h| {
            let id = graph.vertex_id(h).unwrap().to_string();
            let view = vertex_view(graph, &id);
            (id, view)
        })
        .collect();
    vertices.sort_by(|a, b| a.0.cmp(&b.0));
    let mut edges: Vec<_> = GraphStorage::edges(graph)
        .map(|h| {
            let id = graph.edge_id(h).unwrap().to_string();
            let view = edge_view(graph, &id);
            (id, view)
        })
        .collect();
    edges.sort_by(|a, b| a.0.cmp(&b.0));
    (vertices, edges)
}

const FIVE: [&str; 5] = ["e", "a", "d", "b", "c"];

/// Vertices `x` (keys `e a d b c`, label `p`) and `y` (keys `c b a`, label `p`), an edge
/// `xy` (keys `e a d b c`, label `r`) and an edge `bare` without properties.
fn fixture() -> TraversalGraph {
    let mut graph = TraversalGraph::new();
    let x = graph
        .add_vertex(Some("x".into()), vec!["p".into()], numbered(&FIVE))
        .unwrap();
    let y = graph
        .add_vertex(
            Some("y".into()),
            vec!["p".into()],
            numbered(&["c", "b", "a"]),
        )
        .unwrap();
    graph
        .add_edge(Some("xy".into()), Some("r".into()), x, y, numbered(&FIVE))
        .unwrap();
    graph
        .add_edge(
            Some("bare".into()),
            Some("r".into()),
            y,
            x,
            PropertyMap::new(),
        )
        .unwrap();
    graph
}

/// Runs `change` inside a unit, asserts it changed something, rolls it back and asserts the
/// property order of every element is exactly as before.
fn assert_order_restored(
    graph: &mut TraversalGraph,
    change: impl FnOnce(&mut TraversalGraph) -> Result<(), GraphError>,
) {
    let before = dump(graph);
    let mark = graph.begin_unit();
    change(graph).expect("the change itself succeeds");
    assert_ne!(dump(graph), before, "the change must be visible");
    graph.rollback_unit(mark, &RollbackReason::failed(&"test"));
    assert_eq!(dump(graph), before);
}

// ---------------------------------------------------------------------------------------------
// Plain writes
// ---------------------------------------------------------------------------------------------

#[test]
fn creation_order_is_kept_and_writes_follow_the_documented_rules() {
    let mut graph = fixture();
    assert_eq!(vkeys(&graph, "x"), owned(&FIVE));
    assert_eq!(vkeys(&graph, "y"), owned(&["c", "b", "a"]));
    assert_eq!(ekeys(&graph, "xy"), owned(&FIVE));
    assert!(ekeys(&graph, "bare").is_empty());

    // Overwrite: same position.
    let x = v(&graph, "x");
    graph.set_vertex_property(x, "d".into(), 99.into()).unwrap();
    assert_eq!(vkeys(&graph, "x"), owned(&FIVE));
    assert_eq!(vertex_view(&graph, "x")[2].1, ElementProperty::from(99));
    // New key: the end.
    graph
        .set_vertex_property(x, "n".into(), true.into())
        .unwrap();
    assert_eq!(vkeys(&graph, "x"), owned(&["e", "a", "d", "b", "c", "n"]));
    // Drop in the middle: the others keep their order.
    graph.drop_vertex_property("d", x).unwrap();
    assert_eq!(vkeys(&graph, "x"), owned(&["e", "a", "b", "c", "n"]));
    // Re-added: the end.
    graph.set_vertex_property(x, "d".into(), 1.into()).unwrap();
    assert_eq!(vkeys(&graph, "x"), owned(&["e", "a", "b", "c", "n", "d"]));
    // Dropping an absent key changes nothing.
    graph.drop_vertex_property("absent", x).unwrap();
    assert_eq!(vkeys(&graph, "x"), owned(&["e", "a", "b", "c", "n", "d"]));

    let xy = e(&graph, "xy");
    graph.set_edge_property(xy, "e".into(), 5.into()).unwrap();
    graph.drop_edge_property("a", xy).unwrap();
    graph.set_edge_property(xy, "a".into(), 0.into()).unwrap();
    assert_eq!(ekeys(&graph, "xy"), owned(&["e", "d", "b", "c", "a"]));
    // The first property of an edge created without any.
    let bare = e(&graph, "bare");
    graph.set_edge_property(bare, "z".into(), 1.into()).unwrap();
    graph.set_edge_property(bare, "a".into(), 2.into()).unwrap();
    assert_eq!(ekeys(&graph, "bare"), owned(&["z", "a"]));
    // Removing every property of an edge leaves an empty map.
    graph.drop_edge_property("z", bare).unwrap();
    graph.drop_edge_property("a", bare).unwrap();
    assert!(ekeys(&graph, "bare").is_empty());
}

#[test]
fn traversal_writes_keep_positions_and_append_new_keys() {
    let mut graph = fixture();
    graph
        .traversal_mut()
        .v("x")
        .property("a", "overwritten")
        .property("new", 1)
        .iterate()
        .unwrap();
    assert_eq!(vkeys(&graph, "x"), owned(&["e", "a", "d", "b", "c", "new"]));
    graph
        .traversal_mut()
        .v("x")
        .properties("e")
        .drop()
        .iterate()
        .unwrap();
    graph
        .traversal_mut()
        .v("x")
        .property("e", "back")
        .iterate()
        .unwrap();
    assert_eq!(vkeys(&graph, "x"), owned(&["a", "d", "b", "c", "new", "e"]));
    graph
        .traversal_mut()
        .e("xy")
        .property("c", 0)
        .property("w", 0.5)
        .iterate()
        .unwrap();
    assert_eq!(ekeys(&graph, "xy"), owned(&["e", "a", "d", "b", "c", "w"]));
    graph
        .traversal_mut()
        .add_v("q")
        .property("m", 1)
        .property("b", 2)
        .property("k", 3)
        .iterate()
        .unwrap();
    let id = graph
        .traversal()
        .v(None::<()>)
        .has_label("q")
        .id()
        .to_list()
        .unwrap();
    let id = strings(id).remove(0);
    assert_eq!(vkeys(&graph, &id), owned(&["m", "b", "k"]));
}

// ---------------------------------------------------------------------------------------------
// The undo log
// ---------------------------------------------------------------------------------------------

#[test]
fn rollback_of_a_property_drop_restores_its_position_at_every_index() {
    for key in FIVE {
        let mut graph = fixture();
        assert_order_restored(&mut graph, |g| {
            let x = v(g, "x");
            g.drop_vertex_property(key, x)
        });
        assert_order_restored(&mut graph, |g| {
            let xy = e(g, "xy");
            g.drop_edge_property(key, xy)
        });
    }
}

#[test]
fn rollback_of_several_drops_in_one_unit_restores_the_order() {
    for drops in [
        &["b", "d"][..],
        &["d", "b"],
        &["e", "c"],
        &["c", "e"],
        &["a", "e", "c", "d", "b"],
        &["c", "b", "d", "a", "e"],
    ] {
        let mut graph = fixture();
        assert_order_restored(&mut graph, |g| {
            let (x, xy) = (v(g, "x"), e(g, "xy"));
            for key in drops {
                g.drop_vertex_property(key, x)?;
                g.drop_edge_property(key, xy)?;
            }
            Ok(())
        });
    }
}

#[test]
fn rollback_of_an_overwrite_restores_value_and_position() {
    let mut graph = fixture();
    assert_order_restored(&mut graph, |g| {
        let (x, xy) = (v(g, "x"), e(g, "xy"));
        g.set_vertex_property(x, "d".into(), "new".into())?;
        g.set_edge_property(xy, "e".into(), "new".into())?;
        Ok(())
    });
    assert_eq!(vertex_view(&graph, "x")[2].1, ElementProperty::from(2));
}

#[test]
fn rollback_of_mixed_property_changes_restores_the_order() {
    let mut graph = fixture();
    assert_order_restored(&mut graph, |g| {
        let (x, xy, bare) = (v(g, "x"), e(g, "xy"), e(g, "bare"));
        // Drop, re-add at the end, overwrite, add, drop again.
        g.drop_vertex_property("a", x)?;
        g.set_vertex_property(x, "a".into(), "again".into())?;
        g.set_vertex_property(x, "d".into(), 9.into())?;
        g.set_vertex_property(x, "n".into(), 1.into())?;
        g.drop_vertex_property("e", x)?;
        g.drop_vertex_property("n", x)?;
        g.set_vertex_property(x, "e".into(), 0.into())?;
        g.drop_edge_property("c", xy)?;
        g.set_edge_property(xy, "c".into(), 1.into())?;
        g.set_edge_property(xy, "b".into(), 2.into())?;
        g.set_edge_property(bare, "first".into(), 1.into())?;
        g.set_edge_property(bare, "second".into(), 1.into())?;
        g.drop_edge_property("first", bare)?;
        Ok(())
    });
}

#[test]
fn rollback_of_a_vertex_drop_restores_its_properties_and_its_edges_in_order() {
    let mut graph = fixture();
    // Make the orders less regular first (committed).
    let x = v(&graph, "x");
    graph.drop_vertex_property("a", x).unwrap();
    graph.set_vertex_property(x, "a".into(), 1.into()).unwrap();
    let xy = e(&graph, "xy");
    graph.drop_edge_property("d", xy).unwrap();
    graph.set_edge_property(xy, "d".into(), 1.into()).unwrap();
    assert_order_restored(&mut graph, |g| {
        let x = v(g, "x");
        g.drop_vertex(x)
    });
    assert_eq!(vkeys(&graph, "x"), owned(&["e", "d", "b", "c", "a"]));
    assert_eq!(ekeys(&graph, "xy"), owned(&["e", "a", "b", "c", "d"]));
    // A property change before the drop in the same unit is undone too.
    assert_order_restored(&mut graph, |g| {
        let x = v(g, "x");
        g.drop_vertex_property("e", x)?;
        g.set_vertex_property(x, "zz".into(), 1.into())?;
        let xy = e(g, "xy");
        g.drop_edge_property("b", xy)?;
        g.drop_vertex(x)
    });
}

#[test]
fn rollback_of_an_edge_drop_restores_its_properties_in_order() {
    let mut graph = fixture();
    assert_order_restored(&mut graph, |g| {
        let xy = e(g, "xy");
        g.drop_edge_property("e", xy)?;
        g.set_edge_property(xy, "e".into(), 1.into())?;
        g.drop_edge(xy)?;
        let bare = e(g, "bare");
        g.drop_edge(bare)
    });
    assert!(ekeys(&graph, "bare").is_empty());
}

#[test]
fn a_failed_traversal_leaves_the_order_unchanged() {
    let mut graph = fixture();
    let before = dump(&graph);
    let result = graph
        .traversal_mut()
        .inject(vec![ElementProperty::from(1)])
        .side_effect_t(__::v("x").properties("a").drop())
        .side_effect_t(__::v("x").property("a", "re-added").property("d", 0))
        .side_effect_t(__::e("xy").properties("e").drop())
        .side_effect_t(__::e("xy").property("e", 1).property("new", 1))
        .side_effect_t(__::v("y").drop())
        .constant("x")
        .as_number(GType::LONG)
        .to_list();
    assert!(result.is_err());
    assert_eq!(dump(&graph), before);
}

#[test]
fn a_dry_run_sees_its_own_order_and_leaves_the_graph_unchanged() {
    let mut graph = fixture();
    let before = dump(&graph);
    let (inside, changes) = graph
        .dry_run(|g| {
            let x = v(g, "x");
            g.drop_vertex_property("e", x)?;
            g.set_vertex_property(x, "e".into(), 1.into())?;
            g.set_vertex_property(x, "a".into(), 1.into())?;
            let xy = e(g, "xy");
            g.drop_edge_property("b", xy)?;
            g.set_edge_property(xy, "b".into(), 1.into())?;
            Ok::<_, GraphError>((vkeys(g, "x"), ekeys(g, "xy")))
        })
        .unwrap();
    assert_eq!(inside.0, owned(&["a", "d", "b", "c", "e"]));
    assert_eq!(inside.1, owned(&["e", "a", "d", "c", "b"]));
    assert!(!changes.is_empty());
    assert_eq!(dump(&graph), before);

    // Applying the previewed change set gives the order the dry run saw.
    graph.apply_change_set(&changes).unwrap();
    assert_eq!(vkeys(&graph, "x"), inside.0);
    assert_eq!(ekeys(&graph, "xy"), inside.1);
}

#[test]
fn a_rolled_back_savepoint_restores_the_order_of_the_enclosing_unit() {
    let mut graph = fixture();
    let original = dump(&graph);
    let outer = graph.begin_unit();
    let x = v(&graph, "x");
    graph.drop_vertex_property("d", x).unwrap();
    graph.set_vertex_property(x, "d".into(), 1.into()).unwrap();
    let after_outer = dump(&graph);
    assert_eq!(vkeys(&graph, "x"), owned(&["e", "a", "b", "c", "d"]));

    let inner = graph.begin_unit();
    graph.drop_vertex_property("e", x).unwrap();
    graph.drop_vertex_property("d", x).unwrap();
    graph.set_vertex_property(x, "e".into(), 1.into()).unwrap();
    graph.set_vertex_property(x, "n".into(), 1.into()).unwrap();
    let xy = e(&graph, "xy");
    graph.drop_edge_property("a", xy).unwrap();
    graph.set_edge_property(xy, "a".into(), 1.into()).unwrap();
    assert_eq!(vkeys(&graph, "x"), owned(&["a", "b", "c", "e", "n"]));
    graph.rollback_unit(inner, &RollbackReason::failed(&"inner"));
    assert_eq!(dump(&graph), after_outer);

    graph.rollback_unit(outer, &RollbackReason::failed(&"outer"));
    assert_eq!(dump(&graph), original);
}

#[test]
fn a_nested_transaction_error_restores_the_order_of_the_outer_transaction() {
    let mut graph = fixture();
    let outcome: Result<(), GraphError> = graph.transaction(|g| {
        let x = v(g, "x");
        g.drop_vertex_property("a", x)?;
        g.set_vertex_property(x, "a".into(), 1.into())?;
        let inner: Result<(), GraphError> = g.transaction(|g| {
            let x = v(g, "x");
            g.drop_vertex_property("b", x)?;
            g.set_vertex_property(x, "b".into(), 1.into())?;
            Err(GraphError::vertex_not_found("stop"))
        });
        assert!(inner.is_err());
        assert_eq!(vkeys(g, "x"), owned(&["e", "d", "b", "c", "a"]));
        Ok(())
    });
    outcome.unwrap();
    assert_eq!(vkeys(&graph, "x"), owned(&["e", "d", "b", "c", "a"]));
}

// ---------------------------------------------------------------------------------------------
// Change-set replay
// ---------------------------------------------------------------------------------------------

/// A commit hook that records every accepted change set.
#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<ChangeSet>>>);

impl CommitHook for Recorder {
    fn before_commit(&mut self, changes: &ChangeSet) -> Result<(), HookError> {
        self.0.lock().unwrap().push(changes.clone());
        Ok(())
    }
}

/// Committed units that create elements and reorder their properties.
fn reorder_in_units(graph: &mut TraversalGraph) {
    graph
        .transaction(|g| -> Result<(), GraphError> {
            let x = g.add_vertex(Some("x".into()), vec!["p".into()], numbered(&FIVE))?;
            let y = g.add_vertex(Some("y".into()), vec!["p".into()], numbered(&FIVE))?;
            g.add_edge(Some("xy".into()), Some("r".into()), x, y, numbered(&FIVE))?;
            g.add_edge(
                Some("bare".into()),
                Some("r".into()),
                y,
                x,
                PropertyMap::new(),
            )?;
            Ok(())
        })
        .unwrap();
    graph
        .transaction(|g| -> Result<(), GraphError> {
            let x = v(g, "x");
            g.drop_vertex_property("a", x)?;
            g.set_vertex_property(x, "a".into(), "end".into())?;
            g.set_vertex_property(x, "d".into(), "kept".into())?;
            g.set_vertex_property(x, "n".into(), 1.into())?;
            let xy = e(g, "xy");
            g.drop_edge_property("e", xy)?;
            g.set_edge_property(xy, "e".into(), 1.into())?;
            let bare = e(g, "bare");
            g.set_edge_property(bare, "z".into(), 1.into())?;
            g.set_edge_property(bare, "a".into(), 1.into())?;
            Ok(())
        })
        .unwrap();
    graph
        .transaction(|g| -> Result<(), GraphError> {
            let y = v(g, "y");
            g.drop_vertex_property("c", y)?;
            g.drop_vertex_property("e", y)?;
            g.set_vertex_property(y, "c".into(), 1.into())?;
            Ok(())
        })
        .unwrap();
    let mark = graph.begin_unit();
    let x = v(graph, "x");
    graph.drop_vertex_property("b", x).unwrap();
    graph.rollback_unit(mark, &RollbackReason::failed(&"not recorded"));
}

#[test]
fn change_set_replay_reproduces_the_order() {
    let mut source = TraversalGraph::new();
    let recorder = Recorder::default();
    source
        .register_commit_hook(Box::new(recorder.clone()))
        .unwrap();
    reorder_in_units(&mut source);
    assert_eq!(vkeys(&source, "x"), owned(&["e", "d", "b", "c", "a", "n"]));
    assert_eq!(vkeys(&source, "y"), owned(&["a", "d", "b", "c"]));
    assert_eq!(ekeys(&source, "xy"), owned(&["a", "d", "b", "c", "e"]));
    assert_eq!(ekeys(&source, "bare"), owned(&["z", "a"]));

    let sets = recorder.0.lock().unwrap().clone();
    assert_eq!(sets.len(), 3);
    let mut replica = TraversalGraph::new();
    for set in &sets {
        replica.apply_change_set(set).unwrap();
    }
    assert_eq!(dump(&replica), dump(&source));
}

#[cfg(feature = "serde")]
#[test]
fn a_serialized_change_set_replays_to_the_same_order() {
    let mut source = TraversalGraph::new();
    let recorder = Recorder::default();
    source
        .register_commit_hook(Box::new(recorder.clone()))
        .unwrap();
    reorder_in_units(&mut source);
    let mut replica = TraversalGraph::new();
    for set in recorder.0.lock().unwrap().iter() {
        let text = serde_json::to_string(set).unwrap();
        let back: ChangeSet = serde_json::from_str(&text).unwrap();
        replica.apply_change_set(&back).unwrap();
    }
    assert_eq!(dump(&replica), dump(&source));
}

// ---------------------------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------------------------

#[cfg(feature = "persist")]
mod persist_order {
    use super::*;
    use graphersal::persist::{
        self, Durability, Journal, RecoveryTarget, SharedBuffer, WriteOptions,
    };

    fn snapshot_bytes(graph: &TraversalGraph) -> Vec<u8> {
        let mut out = Vec::new();
        persist::write_snapshot(graph, &mut out, &WriteOptions::new()).expect("write");
        out
    }

    #[test]
    fn a_snapshot_round_trip_keeps_the_order() {
        let mut graph = TraversalGraph::new();
        reorder_in_units(&mut graph);
        wide_vertex(&mut graph, "wide", 100);
        let loaded = TraversalGraph::read_snapshot(snapshot_bytes(&graph).as_slice()).unwrap();
        assert_eq!(dump(&loaded), dump(&graph));
    }

    #[test]
    fn journal_recovery_keeps_the_order() {
        let mut graph = fixture();
        let snapshot = snapshot_bytes(&graph);
        let wal = SharedBuffer::new();
        Journal::new(wal.clone(), Durability::EveryCommit)
            .attach(&mut graph)
            .unwrap();
        reorder_in_units(graph_with_fresh_ids(&mut graph));
        let recovered = persist::recover(
            snapshot.as_slice(),
            [wal.contents().as_slice()],
            RecoveryTarget::Latest,
        )
        .unwrap();
        assert_eq!(dump(&recovered.graph), dump(&graph));
    }

    /// Drops the fixture's elements so that [`reorder_in_units`] can create its own (one unit).
    fn graph_with_fresh_ids(graph: &mut TraversalGraph) -> &mut TraversalGraph {
        graph
            .transaction(|g| -> Result<(), GraphError> {
                let x = v(g, "x");
                let y = v(g, "y");
                g.drop_vertex(x)?;
                g.drop_vertex(y)
            })
            .unwrap();
        graph
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_store_keeps_the_order_through_wal_replay_and_checkpoint() {
        use graphersal::persist::{MemDir, Store, StoreOptions};

        let dir = MemDir::new();
        let options = || StoreOptions::new().with_checkpoint_on_close(false);
        let store = Store::create_with(&dir, fixture(), options()).unwrap();
        {
            let mut guard = store.graph().write();
            graph_with_fresh_ids(&mut guard);
            reorder_in_units(&mut guard);
        }
        let expected = dump(&store.graph().read());
        store.close().unwrap();

        // Reopen: the WAL is replayed onto the creation snapshot.
        let reopened = Store::open(&dir, options()).unwrap();
        assert_eq!(dump(&reopened.graph().read()), expected);
        // Checkpoint, reopen: loaded from the new snapshot.
        reopened.checkpoint(None).unwrap();
        reopened.close().unwrap();
        let again = Store::open(&dir, options()).unwrap();
        assert_eq!(dump(&again.graph().read()), expected);
    }
}

// ---------------------------------------------------------------------------------------------
// Exchange formats
// ---------------------------------------------------------------------------------------------

#[cfg(feature = "io")]
mod exchange_order {
    use super::*;
    use graphersal::io::{GraphExport, GraphImport};

    fn graphson_round_trip(graph: &TraversalGraph) -> TraversalGraph {
        let mut text = Vec::new();
        graph.export_graphson_writer(&mut text).unwrap();
        let mut back = TraversalGraph::new();
        back.import_graphson_reader(std::io::Cursor::new(text))
            .unwrap();
        back
    }

    #[test]
    fn a_graphson_round_trip_keeps_the_order() {
        let mut graph = TraversalGraph::new();
        reorder_in_units(&mut graph);
        wide_vertex(&mut graph, "wide", 25);
        let back = graphson_round_trip(&graph);
        assert_eq!(dump(&back), dump(&graph));
        // Exporting the re-import gives the same text.
        let (mut first, mut second) = (Vec::new(), Vec::new());
        graph.export_graphson_writer(&mut first).unwrap();
        back.export_graphson_writer(&mut second).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn graphson_import_keeps_the_order_of_the_document() {
        let text = br#"{"id":{"@type":"g:Int32","@value":1},"label":"p","properties":{"zeta":[{"id":{"@type":"g:Int64","@value":0},"value":"z"}],"alpha":[{"id":{"@type":"g:Int64","@value":1},"value":"a"}],"mid":[{"id":{"@type":"g:Int64","@value":2},"value":"m"}]}}"#;
        let mut graph = TraversalGraph::new();
        graph
            .import_graphson_reader(std::io::Cursor::new(&text[..]))
            .unwrap();
        assert_eq!(vkeys(&graph, "1"), owned(&["zeta", "alpha", "mid"]));
    }
}

// ---------------------------------------------------------------------------------------------
// Wide elements
// ---------------------------------------------------------------------------------------------

/// A permutation of `0..n` (deterministic, not sorted).
fn scrambled(n: usize) -> Vec<usize> {
    (0..n).map(|i| (i * 37 + 11) % n).collect()
}

/// Adds vertex `id` with `n` keys `k<i>` in a scrambled order, value `i`.
fn wide_vertex(graph: &mut TraversalGraph, id: &str, n: usize) -> Vec<String> {
    let keys: Vec<String> = scrambled(n).into_iter().map(|i| format!("k{i}")).collect();
    let map: PropertyMap = keys
        .iter()
        .map(|k| {
            (
                k.as_str(),
                ElementProperty::from(k[1..].parse::<i64>().unwrap()),
            )
        })
        .collect();
    graph
        .add_vertex(Some(id.into()), vec!["wide".into()], map)
        .unwrap();
    keys
}

#[test]
fn wide_elements_keep_order_through_get_insert_remove_and_rollback() {
    for n in [20, 25, 100] {
        let mut graph = TraversalGraph::new();
        let mut keys = wide_vertex(&mut graph, "w", n);
        let w = v(&graph, "w");
        assert_eq!(vkeys(&graph, "w"), keys);
        // Every key is found, also by values(key) and has(key, value).
        for key in &keys {
            let value = ElementProperty::from(key[1..].parse::<i64>().unwrap());
            assert_eq!(
                graph.vertex_properties(w).unwrap().get(key),
                Some(&value),
                "{key}"
            );
            assert_eq!(
                graph
                    .traversal()
                    .v("w")
                    .values(key.as_str())
                    .to_list()
                    .unwrap(),
                vec![value.clone()]
            );
            assert_eq!(
                graph
                    .traversal()
                    .v(None::<()>)
                    .has(key.as_str(), value)
                    .count()
                    .next()
                    .unwrap(),
                Some(1.into())
            );
        }
        // Overwrite every third key: positions stay.
        for key in keys.iter().step_by(3) {
            graph
                .set_vertex_property(w, key.as_str().into(), "over".into())
                .unwrap();
        }
        assert_eq!(vkeys(&graph, "w"), keys);
        // Rollback of drops of every second key, then of an overwrite of all.
        assert_order_restored(&mut graph, |g| {
            let w = v(g, "w");
            for key in keys.iter().step_by(2) {
                g.drop_vertex_property(key, w)?;
            }
            Ok(())
        });
        assert_order_restored(&mut graph, |g| {
            let w = v(g, "w");
            for key in keys.iter().rev() {
                g.set_vertex_property(w, key.as_str().into(), ElementProperty::Null)?;
            }
            g.set_vertex_property(w, "extra".into(), 1.into())
        });
        // Remove a few, re-add one: it goes to the end.
        for key in [keys[0].clone(), keys[n / 2].clone(), keys[n - 1].clone()] {
            graph.drop_vertex_property(&key, w).unwrap();
            keys.retain(|k| *k != key);
        }
        let readded = format!("k{}", scrambled(n)[n / 2]);
        graph
            .set_vertex_property(w, readded.as_str().into(), 7.into())
            .unwrap();
        keys.push(readded.clone());
        assert_eq!(vkeys(&graph, "w"), keys);
        assert_eq!(
            graph
                .traversal()
                .v("w")
                .values(readded.as_str())
                .to_list()
                .unwrap(),
            vec![ElementProperty::from(7)]
        );
        assert_eq!(graph.vertex_properties(w).unwrap().len(), n - 2);
    }
}

#[test]
fn a_wide_edge_keeps_its_order_in_element_map() {
    let mut graph = TraversalGraph::new();
    let a = graph
        .add_vertex(Some("a".into()), vec![], PropertyMap::new())
        .unwrap();
    let keys: Vec<String> = scrambled(25).into_iter().map(|i| format!("p{i}")).collect();
    let map: PropertyMap = keys
        .iter()
        .map(|k| (k.as_str(), ElementProperty::from(k.as_str())))
        .collect();
    graph
        .add_edge(Some("w".into()), Some("r".into()), a, a, map)
        .unwrap();
    assert_eq!(ekeys(&graph, "w"), keys);
    let element_map = single_map(
        graph
            .traversal()
            .e("w")
            .element_map(None)
            .to_list()
            .unwrap(),
    );
    let shown: Vec<_> = element_map
        .iter()
        .filter(|(k, _)| k.starts_with('p'))
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
    let expected: Vec<_> = keys
        .iter()
        .map(|k| (k.clone(), ElementProperty::from(k.as_str())))
        .collect();
    assert_eq!(shown, expected);
    // A value map of a key subset follows the element's order, not the argument order.
    let subset = single_map(
        graph
            .traversal()
            .e("w")
            .value_map(vec![keys[20].as_str(), keys[3].as_str()])
            .to_list()
            .unwrap(),
    );
    assert_eq!(keys_of(&subset), vec![keys[3].clone(), keys[20].clone()]);
}

// ---------------------------------------------------------------------------------------------
// Shared and divergent key sets
// ---------------------------------------------------------------------------------------------

#[test]
fn elements_with_the_same_keys_are_independent() {
    let mut graph = TraversalGraph::new();
    for i in 0..50 {
        graph
            .add_vertex(
                Some(format!("s{i}").into()),
                vec!["p".into()],
                numbered(&["a", "b", "c"]),
            )
            .unwrap();
    }
    // Same keys, other orders.
    graph
        .add_vertex(
            Some("r1".into()),
            vec!["p".into()],
            numbered(&["c", "b", "a"]),
        )
        .unwrap();
    graph
        .add_vertex(
            Some("r2".into()),
            vec!["p".into()],
            numbered(&["b", "a", "c"]),
        )
        .unwrap();
    assert_eq!(vkeys(&graph, "r1"), owned(&["c", "b", "a"]));
    assert_eq!(vkeys(&graph, "r2"), owned(&["b", "a", "c"]));

    // Adding a key to one element changes no other.
    let s0 = v(&graph, "s0");
    graph.set_vertex_property(s0, "d".into(), 1.into()).unwrap();
    assert_eq!(vkeys(&graph, "s0"), owned(&["a", "b", "c", "d"]));
    assert_eq!(vkeys(&graph, "s1"), owned(&["a", "b", "c"]));
    // Removing a key from one changes no other.
    let s1 = v(&graph, "s1");
    graph.drop_vertex_property("b", s1).unwrap();
    assert_eq!(vkeys(&graph, "s1"), owned(&["a", "c"]));
    assert_eq!(vkeys(&graph, "s2"), owned(&["a", "b", "c"]));
    // Re-adding it puts it at the end of that element only.
    graph.set_vertex_property(s1, "b".into(), 1.into()).unwrap();
    assert_eq!(vkeys(&graph, "s1"), owned(&["a", "c", "b"]));
    assert_eq!(vkeys(&graph, "s2"), owned(&["a", "b", "c"]));
    // Two elements reaching the same key set by different paths keep their own order.
    let s2 = v(&graph, "s2");
    graph.drop_vertex_property("a", s2).unwrap();
    graph.set_vertex_property(s2, "a".into(), 1.into()).unwrap();
    assert_eq!(vkeys(&graph, "s2"), owned(&["b", "c", "a"]));
    assert_eq!(vkeys(&graph, "r1"), owned(&["c", "b", "a"]));
    // Overwriting a value changes only that element.
    let s3 = v(&graph, "s3");
    graph
        .set_vertex_property(s3, "a".into(), "mine".into())
        .unwrap();
    assert_eq!(vertex_view(&graph, "s4")[0].1, ElementProperty::from(0));

    // A traversal changing half of them leaves the other half untouched.
    graph
        .traversal_mut()
        .v(None::<()>)
        .has("a", 0)
        .has_id((5..30).map(|i| format!("s{i}")).collect::<Vec<_>>())
        .properties("a")
        .drop()
        .iterate()
        .unwrap();
    for i in 5..50 {
        let expected = if i < 30 {
            owned(&["b", "c"])
        } else {
            owned(&["a", "b", "c"])
        };
        assert_eq!(vkeys(&graph, &format!("s{i}")), expected, "s{i}");
    }
    // A rollback on one element restores only it.
    assert_order_restored(&mut graph, |g| {
        let s40 = v(g, "s40");
        g.drop_vertex_property("a", s40)?;
        g.set_vertex_property(s40, "a".into(), 1.into())
    });
    assert_eq!(vkeys(&graph, "s40"), owned(&["a", "b", "c"]));
}

#[test]
fn edges_with_the_same_keys_are_independent() {
    let mut graph = TraversalGraph::new();
    let a = graph
        .add_vertex(Some("a".into()), vec![], PropertyMap::new())
        .unwrap();
    for i in 0..10 {
        graph
            .add_edge(
                Some(format!("e{i}").into()),
                Some("r".into()),
                a,
                a,
                numbered(&["w", "s"]),
            )
            .unwrap();
    }
    let e0 = e(&graph, "e0");
    graph.drop_edge_property("w", e0).unwrap();
    graph.set_edge_property(e0, "w".into(), 1.into()).unwrap();
    graph.set_edge_property(e0, "x".into(), 1.into()).unwrap();
    assert_eq!(ekeys(&graph, "e0"), owned(&["s", "w", "x"]));
    for i in 1..10 {
        assert_eq!(ekeys(&graph, &format!("e{i}")), owned(&["w", "s"]));
    }
}

// ---------------------------------------------------------------------------------------------
// Compressed properties and property indexes
// ---------------------------------------------------------------------------------------------

fn long_text(seed: usize) -> String {
    let mut out = format!("text #{seed}: ");
    while out.len() < 600 {
        out.push_str("A long biography that a compression rule keeps compressed in memory. ");
    }
    out
}

#[test]
fn a_compressed_property_keeps_its_position() {
    let mut graph = TraversalGraph::new();
    graph
        .add_vertex(
            Some("c".into()),
            vec!["customer".into()],
            props(&[
                ("name", "ann".into()),
                ("bio", long_text(1).as_str().into()),
                ("age", 30.into()),
            ]),
        )
        .unwrap();
    graph
        .traversal_mut()
        .define_compression(
            "bio",
            CompressionDefinition::new(ElementKind::Vertex, "customer", "bio")
                .unwrap()
                .with_min_bytes(64),
        )
        .unwrap();
    assert_eq!(graph.compression_stats("bio").unwrap().compressed_values, 1);
    assert_eq!(vkeys(&graph, "c"), owned(&["name", "bio", "age"]));
    // Reads are plain, in position.
    assert_eq!(
        vertex_view(&graph, "c")[1].1,
        ElementProperty::from(long_text(1).as_str())
    );

    // An overwrite keeps the position and stays compressed.
    let c = v(&graph, "c");
    graph
        .set_vertex_property(c, "bio".into(), long_text(2).as_str().into())
        .unwrap();
    assert_eq!(vkeys(&graph, "c"), owned(&["name", "bio", "age"]));
    assert_eq!(graph.compression_stats("bio").unwrap().compressed_values, 1);

    // Rollback of a drop and of an overwrite restores position and compression.
    let before = dump(&graph);
    let mark = graph.begin_unit();
    graph.drop_vertex_property("bio", c).unwrap();
    assert_eq!(graph.compression_stats("bio").unwrap().compressed_values, 0);
    graph
        .set_vertex_property(c, "bio".into(), long_text(3).as_str().into())
        .unwrap();
    assert_eq!(vkeys(&graph, "c"), owned(&["name", "age", "bio"]));
    graph.rollback_unit(mark, &RollbackReason::failed(&"test"));
    assert_eq!(dump(&graph), before);
    assert_eq!(graph.compression_stats("bio").unwrap().compressed_values, 1);

    // Dropping the rule leaves the plain value in place.
    graph
        .remove_definition(graphersal::catalog::DefinitionKind::COMPRESSION, "bio")
        .unwrap();
    assert_eq!(dump(&graph), before);
}

#[cfg(feature = "persist")]
#[test]
fn a_compressed_property_keeps_its_position_through_a_snapshot() {
    let mut graph = TraversalGraph::new();
    graph
        .add_vertex(
            Some("c".into()),
            vec!["customer".into()],
            props(&[
                ("bio", long_text(1).as_str().into()),
                ("name", "ann".into()),
            ]),
        )
        .unwrap();
    graph
        .traversal_mut()
        .define_compression(
            "bio",
            CompressionDefinition::new(ElementKind::Vertex, "customer", "bio")
                .unwrap()
                .with_min_bytes(64),
        )
        .unwrap();
    let mut bytes = Vec::new();
    graphersal::persist::write_snapshot(&graph, &mut bytes, &Default::default()).unwrap();
    let loaded = TraversalGraph::read_snapshot(bytes.as_slice()).unwrap();
    assert_eq!(dump(&loaded), dump(&graph));
    assert_eq!(
        loaded.compression_stats("bio").unwrap().compressed_values,
        1
    );
}

fn ids_with(graph: &TraversalGraph, key: &str, value: impl Into<ElementProperty>) -> Vec<String> {
    let mut ids: Vec<String> = graph
        .traversal()
        .v(None::<()>)
        .has_label("p")
        .has(key, value.into())
        .id()
        .to_list()
        .unwrap()
        .into_iter()
        .map(|id| strings(vec![id]).remove(0))
        .collect();
    ids.sort();
    ids
}

#[test]
fn property_index_results_are_unchanged_after_overwrite_and_rollback() {
    let mut graph = fixture();
    graph
        .set_definition(Definition::property_index(
            "p_d",
            PropertyIndexDefinition::new(ElementKind::Vertex, "p", "d"),
        ))
        .unwrap();
    graph
        .set_definition(Definition::property_index(
            "r_e",
            PropertyIndexDefinition::new(ElementKind::Edge, "r", "e"),
        ))
        .unwrap();
    graph.check_property_indexes().unwrap();
    assert_eq!(ids_with(&graph, "d", 2), owned(&["x"]));
    let before = dump(&graph);

    let mark = graph.begin_unit();
    let (x, xy) = (v(&graph, "x"), e(&graph, "xy"));
    graph.set_vertex_property(x, "d".into(), 7.into()).unwrap();
    graph.set_edge_property(xy, "e".into(), 7.into()).unwrap();
    graph.check_property_indexes().unwrap();
    assert_eq!(ids_with(&graph, "d", 7), owned(&["x"]));
    assert!(ids_with(&graph, "d", 2).is_empty());
    graph.drop_vertex_property("d", x).unwrap();
    graph.set_vertex_property(x, "d".into(), 8.into()).unwrap();
    assert_eq!(vkeys(&graph, "x"), owned(&["e", "a", "b", "c", "d"]));
    graph.rollback_unit(mark, &RollbackReason::failed(&"test"));

    graph.check_property_indexes().unwrap();
    assert_eq!(dump(&graph), before);
    assert_eq!(ids_with(&graph, "d", 2), owned(&["x"]));
    assert!(ids_with(&graph, "d", 7).is_empty());
    assert!(ids_with(&graph, "d", 8).is_empty());

    // A committed overwrite moves the entry and keeps the position.
    graph.set_vertex_property(x, "d".into(), 3.into()).unwrap();
    graph.check_property_indexes().unwrap();
    assert_eq!(ids_with(&graph, "d", 3), owned(&["x"]));
    assert_eq!(vkeys(&graph, "x"), owned(&FIVE));
}

// ---------------------------------------------------------------------------------------------
// Rhai DSL
// ---------------------------------------------------------------------------------------------

#[cfg(feature = "script")]
mod dsl_order {
    use super::*;
    use crate::script_host::eval_value;
    use graphersal::prelude::Graph;

    fn keys(graph: &Arc<Graph>, script: &str) -> Vec<String> {
        eval_value(graph.clone(), script)
            .unwrap_or_else(|e| panic!("{script}: {e}"))
            .into_array()
            .unwrap()
            .into_iter()
            .map(|k| k.into_string().unwrap())
            .collect()
    }

    #[test]
    fn a_failing_script_statement_leaves_the_order_unchanged() {
        let graph = Arc::new(Graph::new(fixture()));
        let read = r#"g.V("x").properties().key().toList()"#;
        assert_eq!(keys(&graph, read), owned(&FIVE));
        for script in [
            r#"g.V("x").properties("a").drop().iterate(); g.V("x").property("a", 1).property("zz", 2).values("a").asNumber(GType.LONG).asString().asNumber(GType.LONG).toList(); g.V("x").properties("e").drop().inject("no").asNumber(GType.LONG).toList()"#,
            r#"g.inject(1).sideEffect(__.V("x").properties("e").drop()).sideEffect(__.V("x").property("e", 0)).constant("x").asNumber(GType.LONG).toList()"#,
            r#"g.inject(1).sideEffect(__.V("x").drop()).constant("x").asNumber(GType.LONG).toList()"#,
        ] {
            let _ = eval_value(graph.clone(), script);
        }
        // The first statement of the first script committed: `a` moved to the end, `zz` added.
        assert_eq!(keys(&graph, read), owned(&["e", "d", "b", "c", "a", "zz"]));
        assert_eq!(
            keys(&graph, r#"g.E("xy").properties().key().toList()"#),
            owned(&FIVE)
        );
    }

    #[test]
    fn script_writes_keep_positions_and_append_new_keys() {
        let graph = Arc::new(Graph::new(fixture()));
        let _ = eval_value(
            graph.clone(),
            r#"g.V("x").property("d", "over").property("new", 1).iterate(); g.V("x").properties("e").drop().iterate(); g.V("x").property("e", 2).iterate()"#,
        )
        .unwrap();
        assert_eq!(
            keys(&graph, r#"g.V("x").properties().key().toList()"#),
            owned(&["a", "d", "b", "c", "new", "e"])
        );
        assert_eq!(
            vkeys(&graph.read(), "x"),
            owned(&["a", "d", "b", "c", "new", "e"])
        );
    }
}
