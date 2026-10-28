//! Id lookups (`get_vertex_by_id` / `get_edge_by_id`, `g.v(id)`) end to end. The id index of
//! `TraversalGraph` keeps no copy of the id: an entry is a handle plus a cached hash, confirmed
//! against the id stored in the element (`graph/index.rs`). These tests pin the cases where the
//! handle behind an id changes: drop and re-add, slot reuse, rollback of adds and drops, long
//! (heap-allocated) ids.

use graphersal::changes::RollbackReason;
use graphersal::prelude::*;

fn uuid_id(i: u32) -> String {
    format!("0192f0c4-7d2e-7a1b-9c3d-{i:012x}")
}

fn add(graph: &mut TraversalGraph, id: &str) -> graphersal::storage::VertexHandle {
    graph
        .insert_vertex(
            Some(id.into()),
            vec!["person".into()],
            PropertyMap::default(),
        )
        .expect("free id")
}

#[test]
fn drop_and_re_add_the_same_id_resolves_the_new_element() {
    let mut graph = TraversalGraph::new();
    let first = add(&mut graph, &uuid_id(1));
    let other = add(&mut graph, &uuid_id(2));
    GraphStorage::drop_vertex(&mut graph, first).expect("drop");
    assert_eq!(graph.get_vertex_by_id(&uuid_id(1)).unwrap(), None);
    assert_eq!(graph.get_vertex_by_id(&uuid_id(2)).unwrap(), Some(other));

    let again = add(&mut graph, &uuid_id(1));
    assert_ne!(again, first);
    assert_eq!(graph.get_vertex_by_id(&uuid_id(1)).unwrap(), Some(again));
    assert_eq!(graph.vertex_id(again).unwrap(), Some(uuid_id(1).as_str()));
    // The old handle stays removed.
    assert_eq!(graph.vertex_id(first).unwrap(), None);
}

#[test]
fn a_reused_slot_does_not_answer_for_the_dropped_id() {
    let mut graph = TraversalGraph::new();
    let a = add(&mut graph, "a");
    let b = add(&mut graph, "b");
    let e = graph
        .insert_edge(
            Some("e1".into()),
            Some("knows".into()),
            a,
            b,
            PropertyMap::default(),
        )
        .expect("edge");
    GraphStorage::drop_edge(&mut graph, e).expect("drop edge");
    GraphStorage::drop_vertex(&mut graph, a).expect("drop vertex");
    // The freed slots are taken by new elements with other ids.
    let c = add(&mut graph, "c");
    let f = graph
        .insert_edge(
            Some("e2".into()),
            Some("knows".into()),
            c,
            b,
            PropertyMap::default(),
        )
        .expect("edge");
    assert_eq!(graph.get_vertex_by_id("a").unwrap(), None);
    assert_eq!(graph.get_vertex_by_id("c").unwrap(), Some(c));
    assert_eq!(graph.get_edge_by_id("e1").unwrap(), None);
    assert_eq!(graph.get_edge_by_id("e2").unwrap(), Some(f));
}

#[test]
fn rollback_of_adds_and_drops_restores_the_lookups() {
    let mut graph = TraversalGraph::new();
    let a = add(&mut graph, &uuid_id(10));
    let b = add(&mut graph, &uuid_id(11));
    graph
        .insert_edge(
            Some(uuid_id(12).into()),
            Some("knows".into()),
            a,
            b,
            PropertyMap::default(),
        )
        .expect("edge");

    let mark = graph.begin_unit().unwrap();
    // Drop a vertex (cascading its edge), add new elements, one of them reusing a dropped id.
    GraphStorage::drop_vertex(&mut graph, a).expect("drop");
    let c = add(&mut graph, &uuid_id(13));
    add(&mut graph, &uuid_id(10));
    graph
        .insert_edge(Some(uuid_id(14).into()), None, c, b, PropertyMap::default())
        .expect("edge");
    graph
        .rollback_unit(mark, &RollbackReason::failed(&"test"))
        .unwrap();

    for id in [uuid_id(10), uuid_id(11)] {
        let handle = graph
            .get_vertex_by_id(&id)
            .unwrap()
            .expect("restored vertex");
        assert_eq!(graph.vertex_id(handle).unwrap(), Some(id.as_str()));
    }
    let edge = graph
        .get_edge_by_id(&uuid_id(12))
        .unwrap()
        .expect("restored edge");
    assert_eq!(graph.edge_id(edge).unwrap(), Some(uuid_id(12).as_str()));
    assert_eq!(graph.get_vertex_by_id(&uuid_id(13)).unwrap(), None);
    assert_eq!(graph.get_edge_by_id(&uuid_id(14)).unwrap(), None);
    assert_eq!((graph.vertex_count(), graph.edge_count()), (2, 1));
}

#[test]
fn traversal_by_long_ids_after_churn() {
    let graph = GraphSource::empty();
    {
        let mut guard = graph.write();
        for i in 0..2_000 {
            add(&mut guard, &uuid_id(i));
        }
        for i in (0..2_000).step_by(2) {
            let handle = guard
                .get_vertex_by_id(&uuid_id(i))
                .unwrap()
                .expect("present");
            GraphStorage::drop_vertex(&mut *guard, handle).expect("drop");
        }
    }
    let found = graph
        .read()
        .traversal()
        .v(vec![uuid_id(1), uuid_id(2), uuid_id(1_999)])
        .id()
        .to_list()
        .expect("runs");
    let found: Vec<String> = found
        .iter()
        .filter_map(|value| value.as_str().map(str::to_string))
        .collect();
    assert_eq!(found, vec![uuid_id(1), uuid_id(1_999)]);
}
