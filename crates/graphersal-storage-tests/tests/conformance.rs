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
// A layer over a base with no live element but with vacant slots (a vertex with a self-loop,
// a labelled vertex and an edge between them, all dropped before freezing) and advanced id
// sequences: the items' elements live in the delta beside dead base slots, so the base's
// slot space, generations and sequences are in play (see `vacant_base`). The same base as a
// plain `TraversalGraph` is the control: the items hold on such a storage at all.
conformance!(traversal_graph_vacant, base = vacant_base() => base);
conformance!(
    layered_vacant_base,
    base = vacant_base() => graphersal::storage::LayeredGraph::over(Arc::new(base)).unwrap()
);

/// A `TraversalGraph` whose elements were all dropped: no live element, vacant vertex and edge
/// slots with bumped generations, raised auto-id sequences (auto and numeric explicit ids).
fn vacant_base() -> TraversalGraph {
    let mut graph = TraversalGraph::new();
    let a = contract::vertex(
        &mut graph,
        None,
        &["n"],
        contract::props(&[("k", 1.into())]),
    );
    let b = contract::vertex(&mut graph, Some("41"), &["m", "n"], contract::props(&[]));
    contract::edge(&mut graph, None, Some("l"), a, b, contract::props(&[]));
    contract::edge(&mut graph, Some("17"), None, a, a, contract::props(&[]));
    graph.drop_vertex(a).unwrap();
    graph.drop_vertex(b).unwrap();
    assert_eq!(GraphStorage::vertex_count(&graph).unwrap(), 0);
    assert_eq!(GraphStorage::edge_count(&graph).unwrap(), 0);
    graph
}

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

/// The `atomic` items of the contract on a layer over modern whose base elements the layer
/// already changed (a base vertex written, a base edge dropped, an added edge at a base
/// vertex): a rollback restores the base elements' layer state too, and the layer's
/// bookkeeping stays consistent (task S1.8).
#[test]
fn atomic_items_on_a_layer_with_changed_base_elements() {
    use graphersal::storage::LayeredGraph;
    use graphersal_storage_tests::{extension, smoke};

    let layer = || {
        let mut base = TraversalGraph::new();
        smoke::load_modern(&mut base);
        let mut layer = LayeredGraph::over(Arc::new(base)).unwrap();
        let marko = layer.get_vertex_by_id("1").unwrap().unwrap();
        let vadas = layer.get_vertex_by_id("2").unwrap().unwrap();
        layer
            .set_vertex_property(marko, "age".into(), 30.into())
            .unwrap();
        let e = layer
            .out_edges(vadas)
            .unwrap()
            .chain(layer.in_edges(vadas).unwrap())
            .next()
            .unwrap();
        layer.drop_edge(e).unwrap();
        layer
            .insert_edge(None, None, vadas, marko, contract::props(&[]))
            .unwrap();
        contract::assert_consistent(&layer);
        layer
    };
    extension::rollback_restores_everything(&mut layer());
    extension::savepoint_rollback_keeps_the_outer_unit(&mut layer());
    extension::unit_changes_never_claims_an_undone_change(&mut layer());
    extension::edge_label_change_rolls_back_and_is_captured(&mut layer());
    extension::transactional_panic_rolls_back(&mut layer());
    extension::rollback_restores_definitions(&mut layer());
    extension::a_definition_list_is_stored_like_single_calls(&mut layer());
}

/// The `change_capture` items of the contract on a layer over modern whose base elements the
/// layer already changed (as in `atomic_items_on_a_layer_with_changed_base_elements`): commit
/// hooks, captured change sets, dry runs, metadata, the commit clock (task S1.9).
/// `applied_change_set_reproduces_the_unit`, `applied_change_set_raises_the_id_sequences` and
/// `change_set_builder_round_trip` need an empty storage (they count from zero or reuse
/// modern's ids): they run in `layered_empty_base`.
#[test]
fn change_capture_items_on_a_layer_with_changed_base_elements() {
    use graphersal::storage::LayeredGraph;
    use graphersal_storage_tests::{extension, smoke};

    let layer = || {
        let mut base = TraversalGraph::new();
        smoke::load_modern(&mut base);
        let mut layer = LayeredGraph::over(Arc::new(base)).unwrap();
        let marko = layer.get_vertex_by_id("1").unwrap().unwrap();
        let vadas = layer.get_vertex_by_id("2").unwrap().unwrap();
        layer
            .set_vertex_property(marko, "age".into(), 30.into())
            .unwrap();
        let e = layer
            .out_edges(vadas)
            .unwrap()
            .chain(layer.in_edges(vadas).unwrap())
            .next()
            .unwrap();
        layer.drop_edge(e).unwrap();
        layer
            .insert_edge(None, None, vadas, marko, contract::props(&[]))
            .unwrap();
        contract::assert_consistent(&layer);
        layer
    };
    assert!(layer().capabilities().unwrap().change_capture);
    extension::capabilities_are_consistent(&mut layer());
    extension::commit_hooks_follow_the_capability(&mut layer());
    extension::definition_changes_are_captured(&mut layer());
    extension::edge_label_change_rolls_back_and_is_captured(&mut layer());
    extension::discard_unit_restores_state_keeps_sequences(&mut layer());
    extension::transaction_with_metadata_reaches_hooks(&mut layer());
    extension::commit_clock_is_shared_with_hooks(&mut layer());
}

/// Exact property-index lookups on a TOUCHED layer (S4.2): the base holds the look-alike data
/// and the four indexes of `extension::exact_lookup_fixture`; the layer, which serves them from
/// its overlays and claims `exact_property_index_lookups` as the base does, then changes base
/// vertices (copy-up values into and out of the slices, labels gained and lost, a drop), adds
/// look-alikes of its own, and rolls a unit back: after each stage every lookup returns exactly
/// what a scan of the layer accepts.
#[test]
fn layered_touched_layer_keeps_exact_property_index_lookups() {
    use graphersal::ElementProperty;
    use graphersal::storage::{LayeredGraph, RollbackReason};
    use graphersal_storage_tests::extension::{assert_exact_lookups, exact_lookup_fixture};

    let mut base = TraversalGraph::new();
    let handles = exact_lookup_fixture(&mut base);
    assert_exact_lookups(&base, "the base");
    let ids: Vec<String> = handles
        .iter()
        .map(|v| base.vertex_id(*v).unwrap().unwrap().to_string())
        .collect();
    let mut layer = LayeredGraph::over(Arc::new(base)).unwrap();
    assert!(layer.capabilities().unwrap().exact_property_index_lookups);
    assert_exact_lookups(&layer, "untouched");
    let at = |layer: &LayeredGraph, n: usize| layer.get_vertex_by_id(&ids[n]).unwrap().unwrap();
    let list = |items: &[ElementProperty]| ElementProperty::Array(items.to_vec());

    let x0 = at(&layer, 0);
    layer
        .set_vertex_property(x0, "ex".into(), list(&["x".into(), "z".into()]))
        .unwrap();
    let x6 = at(&layer, 6);
    layer
        .set_vertex_property(x6, "ex".into(), "x".into())
        .unwrap();
    let x4 = at(&layer, 4);
    layer
        .set_vertex_property(x4, "ex".into(), "x".into())
        .unwrap();
    let x3 = at(&layer, 3);
    layer.add_vertex_label(x3, "p".into()).unwrap();
    let x1 = at(&layer, 1);
    layer.remove_vertex_label(x1, "p").unwrap();
    let x2 = at(&layer, 2);
    layer.drop_vertex(x2).unwrap();
    for (labels, value) in [
        (vec!["p"], ElementProperty::from("x")),
        (vec!["p"], ElementProperty::from("X")),
        (vec![], ElementProperty::from(true)),
        (vec!["p", "q"], list(&["x".into(), "x".into(), "1".into()])),
        (
            vec!["p"],
            ElementProperty::Uuid(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef),
        ),
        (vec!["q"], 1.into()),
    ] {
        contract::vertex(&mut layer, None, &labels, contract::props(&[("ex", value)]));
    }
    contract::assert_consistent(&layer);
    assert_exact_lookups(&layer, "touched");

    let mark = layer.begin_unit().unwrap();
    let x11 = at(&layer, 11);
    layer
        .set_vertex_property(x11, "ex".into(), "1".into())
        .unwrap();
    layer
        .set_vertex_property(x0, "ex".into(), "true".into())
        .unwrap();
    let x10 = at(&layer, 10);
    layer.drop_vertex(x10).unwrap();
    assert_exact_lookups(&layer, "inside a unit");
    layer
        .rollback_unit(mark, &RollbackReason::failed(&"contract"))
        .unwrap();
    assert_exact_lookups(&layer, "after the rollback");
}
