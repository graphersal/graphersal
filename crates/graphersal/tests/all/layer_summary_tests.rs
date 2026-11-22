//! `GraphStorage::layer_summary` / `AnyGraph::layer_summary` (S35.1): a plain storage answers
//! `None`; a `LayeredGraph` answers its base's name and identity, the change counts from its
//! delta (equal to the lengths of an unlimited `layer_diff`'s lists, through a rollback and a
//! change back to the base state) and its own bytes beside the shared base's. Also
//! `LayerBase::into_graph`, the way back from a base to a writable graph.

use std::sync::Arc;

use graphersal::prelude::*;
use graphersal::storage::{
    AnyGraph, BaseRef, ElementKind, GraphStorage, IntoAnyGraph, LayerBase, LayerCounts,
    LayerDiffRequest, LayerSummary, LayeredGraph, Transactional,
};

fn summary(layer: &LayeredGraph) -> LayerSummary {
    layer
        .layer_summary()
        .unwrap()
        .expect("a layer has a summary")
}

/// The summary's counts as the diff's list lengths per kind: `(added, modified, removed)`.
fn diff_counts(layer: &LayeredGraph) -> (LayerCounts, LayerCounts, LayerCounts) {
    let diff = layer.layer_diff(&LayerDiffRequest::new()).unwrap();
    let count = |kinds: Vec<ElementKind>| {
        let vertices = kinds.iter().filter(|k| **k == ElementKind::Vertex).count() as u64;
        LayerCounts::new(vertices, kinds.len() as u64 - vertices)
    };
    (
        count(diff.added.iter().map(|e| e.kind).collect()),
        count(diff.modified.iter().map(|e| e.kind).collect()),
        count(diff.removed.iter().map(|e| e.kind).collect()),
    )
}

fn assert_counts_match_the_diff(layer: &LayeredGraph) {
    let summary = summary(layer);
    assert_eq!(
        (summary.added, summary.modified, summary.removed),
        diff_counts(layer)
    );
}

#[test]
fn a_plain_storage_has_no_layer_summary() {
    let graph = TraversalGraph::tinkerpop_modern();
    assert_eq!(graph.layer_summary().unwrap(), None);
    let any = Arc::new(Graph::new(TraversalGraph::tinkerpop_modern())).into_any_graph();
    assert_eq!(any.layer_summary().unwrap(), None);
}

#[test]
fn a_layer_summarises_its_base_counts_and_memory() {
    let base = Arc::new(TraversalGraph::tinkerpop_modern());
    let mut layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
    let untouched = summary(&layer);
    assert_eq!(untouched.base, "base");
    assert_eq!(untouched.base_ref, Some(BaseRef::of(&base)));
    assert_eq!(untouched.added, LayerCounts::default());
    assert_eq!(untouched.modified, LayerCounts::default());
    assert_eq!(untouched.removed, LayerCounts::default());
    let usage = layer.memory_usage().unwrap().unwrap();
    assert_eq!(untouched.own_bytes, usage.total_bytes());
    assert_eq!(untouched.shared_bytes, usage.shared_bytes);
    assert!(untouched.shared_bytes > untouched.own_bytes);

    GraphTraversalSource::new(&mut layer)
        .add_v("person")
        .property("name", "ada")
        .iterate()
        .unwrap();
    GraphTraversalSource::new(&mut layer)
        .v("1")
        .property("age", 30)
        .iterate()
        .unwrap();
    GraphTraversalSource::new(&mut layer)
        .v("6")
        .drop()
        .iterate()
        .unwrap();
    GraphTraversalSource::new(&mut layer)
        .e("1")
        .property("weight", 0.9)
        .iterate()
        .unwrap();
    GraphTraversalSource::new(&mut layer)
        .add_e("knows")
        .from("2")
        .to("4")
        .iterate()
        .unwrap();
    let changed = summary(&layer);
    assert_eq!(changed.added, LayerCounts::new(1, 1));
    assert_eq!(changed.modified, LayerCounts::new(1, 1));
    assert_eq!(changed.removed, LayerCounts::new(1, 1));
    assert_counts_match_the_diff(&layer);
    assert_eq!(changed.shared_bytes, untouched.shared_bytes);
    assert_eq!(
        changed.own_bytes,
        layer.memory_usage().unwrap().unwrap().total_bytes()
    );
    assert!(changed.own_bytes > untouched.own_bytes);

    // Back to the base value: no longer modified.
    GraphTraversalSource::new(&mut layer)
        .v("1")
        .property("age", 29)
        .iterate()
        .unwrap();
    assert_eq!(summary(&layer).modified, LayerCounts::new(0, 1));
    assert_counts_match_the_diff(&layer);
}

#[test]
fn a_rolled_back_unit_leaves_no_count() {
    let base = LayerBase::from_graph(Graph::new(TraversalGraph::tinkerpop_modern()))
        .unwrap()
        .with_name("modern");
    let graph = base.new_layer().unwrap();
    let failed: Result<(), GraphError> = graph.write().transaction(|l| {
        let vadas = l.get_vertex_by_id("2").unwrap().unwrap();
        l.drop_vertex(vadas)?;
        l.insert_vertex(Some("t".into()), vec![], PropertyMap::default())?;
        Err(GraphError::unsupported("the closure gives up"))
    });
    assert!(failed.is_err());
    let any: Arc<dyn AnyGraph> = Arc::new(graph);
    let summary = any.layer_summary().unwrap().unwrap();
    assert_eq!(summary.base, "modern");
    assert_eq!(summary.base_ref.as_ref(), base.base_ref());
    assert_eq!(summary.added, LayerCounts::default());
    assert_eq!(summary.removed, LayerCounts::default());
}

#[test]
fn the_summary_value_and_json_name_every_figure() {
    let layer = LayeredGraph::over(Arc::new(TraversalGraph::tinkerpop_modern())).unwrap();
    let summary = summary(&layer);
    let json: serde_json::Value = serde_json::from_str(&summary.to_json()).unwrap();
    let base_ref = summary.base_ref.unwrap();
    assert_eq!(json["base"], "base");
    assert_eq!(json["base_ref"]["commit_seq"], base_ref.commit_seq());
    assert_eq!(json["base_ref"]["committed_at"], base_ref.committed_at());
    assert_eq!(json["base_ref"]["graph_id"].as_str().unwrap().len(), 36);
    assert_eq!(
        json["added"],
        serde_json::json!({"vertices": 0, "edges": 0})
    );
    assert_eq!(json["shared_bytes"], summary.shared_bytes);
    let without = LayerSummary::new("x", None).with_bytes(1, 2);
    let json: serde_json::Value = serde_json::from_str(&without.to_json()).unwrap();
    assert!(json["base_ref"].is_null());
    assert_eq!(
        (json["own_bytes"].clone(), json["shared_bytes"].clone()),
        (1.into(), 2.into())
    );
}

#[test]
fn a_base_turns_back_into_a_graph_once_nothing_shares_it() {
    let base = LayerBase::from_graph(Graph::new(TraversalGraph::tinkerpop_modern())).unwrap();
    let layer = base.new_layer().unwrap();
    let clone = base.clone();
    let base = base.into_graph().unwrap_err();
    drop(clone);
    let base = base.into_graph().unwrap_err();
    drop(layer);
    let graph = base.into_graph().unwrap();
    assert_eq!(graph.read().vertex_count(), 6);
    GraphTraversalSource::new(&mut *graph.write())
        .add_v("person")
        .iterate()
        .unwrap();
    assert_eq!(graph.read().vertex_count(), 7);
}
