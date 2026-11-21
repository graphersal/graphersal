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
/// `conformance!` all start from an empty storage and write, so the layer runs them once it can
/// write): the invariant checker, the traversal smoke against the reference, and the refusal of
/// every write.
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
    contract::read_only_rejects_writes(&mut layer, a, e);
    graphersal_storage_tests::extension::capabilities_are_consistent(&mut layer);
    graphersal_storage_tests::extension::commit_hooks_follow_the_capability(&mut layer);
}
