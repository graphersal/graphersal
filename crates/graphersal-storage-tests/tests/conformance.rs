//! Runs the contract against every `GraphStorage` implementation of `graphersal` that can be
//! constructed from outside the crate, and against `no_schema::NoSchema`, a wrapper that keeps
//! the trait's schema defaults (the shape of a foreign storage without schema support), and
//! against `Forwarding`, the forwarding storage with its own handle types.

mod no_schema;

use std::sync::Arc;

use graphersal::TraversalGraph;
use graphersal::storage::GraphStorage;
use graphersal_storage_tests::forwarding::Forwarding;
use graphersal_storage_tests::{conformance, contract};

conformance!(traversal_graph, base = TraversalGraph::new() => base);
conformance!(traversal_graph_by_mut_ref, base = TraversalGraph::new() => &mut base);
conformance!(boxed_traversal_graph, base = Box::new(TraversalGraph::new()) => base);
conformance!(arc_traversal_graph, base = Arc::new(TraversalGraph::new()) => base);
conformance!(forwarding, base = Forwarding::default() => base);
conformance!(forwarding_by_mut_ref, base = Forwarding::default() => &mut base);
// A layer over an empty base: every element the items create lives in the layer's delta.
conformance!(layered_empty_base, base = graphersal::storage::LayeredGraph::default() => base);

/// Populates `storage` and checks that the blanket read-only view `&S` reads it and refuses
/// every write, claims no capability and refuses commit hooks.
fn shared_reference_is_read_only_for<S: GraphStorage>(mut storage: S) {
    let a = contract::vertex(&mut storage, Some("a"), &["n"], contract::props(&[]));
    let b = contract::vertex(&mut storage, Some("b"), &["n"], contract::props(&[]));
    let e = contract::edge(
        &mut storage,
        Some("e"),
        Some("l"),
        a,
        b,
        contract::props(&[]),
    );
    let mut read_only = &storage;
    contract::read_only_rejects_writes(&mut read_only, a, e);
    // Reads still work through the same wrapper.
    assert_eq!(GraphStorage::vertex_count(&read_only).unwrap(), 2);
    assert_eq!(
        GraphStorage::get_vertex_by_id(&read_only, "b").unwrap(),
        Some(b)
    );
    assert_eq!(
        GraphStorage::edge_endpoints(&read_only, e).unwrap(),
        Some((a, b))
    );
    // A read-only view claims no capability and refuses commit hooks.
    graphersal_storage_tests::extension::capabilities_are_consistent(&mut read_only);
    graphersal_storage_tests::extension::commit_hooks_follow_the_capability(&mut read_only);
}

#[test]
fn shared_reference_is_read_only() {
    shared_reference_is_read_only_for(TraversalGraph::new());
}

/// The blanket `&G` view is not tied to `TraversalGraph`: a storage outside the crate gets it.
#[test]
fn shared_reference_of_a_foreign_storage_is_read_only() {
    shared_reference_is_read_only_for(no_schema::NoSchema(TraversalGraph::new()));
    shared_reference_is_read_only_for(Forwarding::default());
}

/// The read-only items on an untouched `LayeredGraph` over the modern graph (the write items of
/// `conformance!` all start from an empty storage, see `layered_empty_base` above): the
/// invariant checker, the traversal smoke against the reference, and a schema stored in the
/// layer (its own copy since task S1.7) leaving the data as it was.
#[test]
fn untouched_layer_passes_the_read_only_items() {
    use graphersal::storage::LayeredGraph;
    use graphersal_storage_tests::smoke;

    let mut base = TraversalGraph::new();
    smoke::load_modern(&mut base);
    let mut reference = TraversalGraph::new();
    smoke::load_modern(&mut reference);
    let mut layer = LayeredGraph::over(Arc::new(base)).unwrap();
    contract::assert_consistent(&layer);
    assert_eq!(
        smoke::run_queries(&mut layer),
        smoke::run_queries(&mut reference)
    );
    let a = layer.get_vertex_by_id("1").unwrap().unwrap();
    let e = layer.out_edges(a).unwrap().next().unwrap();
    layer
        .set_schema(graphersal::schema::GraphSchema::new())
        .unwrap();
    assert!(layer.contains_edge(e).unwrap());
    contract::assert_consistent(&layer);
    assert_eq!(layer.vertex_count().unwrap(), 6);
    graphersal_storage_tests::extension::capabilities_are_consistent(&mut layer);
    graphersal_storage_tests::extension::commit_hooks_follow_the_capability(&mut layer);
}

/// A layer over modern whose base elements were changed (properties, path writes, labels gained
/// and lost, an edge relabeled, a change set back) passes the storage contract's consistency
/// check, and the smoke queries match a `TraversalGraph` with the same writes.
#[test]
fn layer_with_modified_base_elements_is_consistent() {
    use graphersal::storage::{GraphStorage, LayeredGraph};
    use graphersal_storage_tests::smoke;

    fn writes<S: GraphStorage>(s: &mut S) {
        let a = s.get_vertex_by_id("1").unwrap().unwrap();
        let b = s.get_vertex_by_id("2").unwrap().unwrap();
        let e = s.out_edges(a).unwrap().next().unwrap();
        s.set_vertex_property(a, "k".into(), 1.into()).unwrap();
        s.set_vertex_property_path(b, &"m.n".parse().unwrap(), 2.into())
            .unwrap();
        s.add_vertex_label(a, "x".into()).unwrap();
        s.remove_vertex_label(b, "person").unwrap();
        s.set_edge_label(e, "y".into()).unwrap();
        s.set_edge_property(e, "k".into(), 1.into()).unwrap();
        s.drop_vertex_property("k", a).unwrap();
    }
    let mut base = TraversalGraph::new();
    smoke::load_modern(&mut base);
    let mut reference = TraversalGraph::new();
    smoke::load_modern(&mut reference);
    let mut layer = LayeredGraph::over(Arc::new(base)).unwrap();
    writes(&mut layer);
    writes(&mut reference);
    contract::assert_consistent(&layer);
    assert_eq!(
        smoke::run_queries(&mut layer),
        smoke::run_queries(&mut reference)
    );
}

/// A layer over modern with new edges at base vertices (every endpoint combination, base and
/// added self-loops) passes the storage contract's consistency check: stubs are invisible,
/// endpoints and adjacency agree.
#[test]
fn layer_with_edges_at_base_vertices_is_consistent() {
    use graphersal::prelude::PropertyMap;
    use graphersal::storage::LayeredGraph;
    use graphersal_storage_tests::smoke;

    let mut base = TraversalGraph::new();
    smoke::load_modern(&mut base);
    let mut layer = LayeredGraph::over(Arc::new(base)).unwrap();
    let a = layer.get_vertex_by_id("1").unwrap().unwrap();
    let b = layer.get_vertex_by_id("2").unwrap().unwrap();
    let n = layer
        .insert_vertex(None, vec!["person".into()], PropertyMap::default())
        .unwrap();
    for (from, to) in [(a, b), (a, a), (n, a), (b, n), (n, n)] {
        layer
            .insert_edge(None, Some("x".into()), from, to, PropertyMap::default())
            .unwrap();
    }
    contract::assert_consistent(&layer);
    assert_eq!(layer.vertex_count().unwrap(), 7);
    assert_eq!(layer.edge_count().unwrap(), 11);
}

/// The same layer after drops of base elements (a base edge, a base vertex with base edges,
/// added edges at its stub and a self-loop) stays consistent: no removed element is listed,
/// adjacency and endpoints agree.
#[test]
fn layer_with_dropped_base_elements_is_consistent() {
    use graphersal::prelude::PropertyMap;
    use graphersal::storage::LayeredGraph;
    use graphersal_storage_tests::smoke;

    let mut base = TraversalGraph::new();
    smoke::load_modern(&mut base);
    let mut layer = LayeredGraph::over(Arc::new(base)).unwrap();
    let a = layer.get_vertex_by_id("1").unwrap().unwrap();
    let b = layer.get_vertex_by_id("2").unwrap().unwrap();
    let n = layer
        .insert_vertex(None, vec!["person".into()], PropertyMap::default())
        .unwrap();
    for (from, to) in [(a, b), (a, a), (n, a), (b, n)] {
        layer
            .insert_edge(None, Some("x".into()), from, to, PropertyMap::default())
            .unwrap();
    }
    let edges = layer.edge_count().unwrap();
    let c = layer.get_vertex_by_id("4").unwrap().unwrap();
    let e = layer.out_edges(c).unwrap().next().unwrap();
    layer.drop_edge(e).unwrap();
    contract::assert_consistent(&layer);
    let incident = layer.out_edges(a).unwrap().count() + layer.in_edges(a).unwrap().count() - 1;
    layer.drop_vertex(a).unwrap();
    contract::assert_consistent(&layer);
    assert_eq!(layer.vertex_count().unwrap(), 6);
    assert_eq!(layer.edge_count().unwrap(), edges - 1 - incident);
}
