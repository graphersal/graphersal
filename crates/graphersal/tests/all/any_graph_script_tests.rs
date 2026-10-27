//! The script layer over `Arc<dyn AnyGraph>` (`book/src/for_developers/custom_storage.md`): the
//! DSL serves a host graph of another storage than `TraversalGraph`. `Wrapped` is a minimal
//! foreign storage (a newtype forwarding to a `TraversalGraph`, units and hooks included) held
//! as `Graph<Wrapped>`; the scripts run through the `eval_*` entries, which take
//! `impl IntoAnyGraph`. Covered: whole-script units (`AnyGraph::run_unit`: commit, rollback,
//! dry run, a panic in the middle of a script), retargeting of a returned traversal, a returned closure,
//! `withSideEffect(name, g)` into a foreign graph (`Unsupported`), and the script's scratch
//! graphs, which stay `TraversalGraph`s.
#![cfg(feature = "script")]

use std::sync::Arc;

use graphersal::prelude::*;
use graphersal::storage::{
    AnyGraph, ChangeSet, CommitHook, CompactString, EdgeHandle, GraphError, GraphStorage,
    IntoAnyGraph, PropertyMap, RollbackReason, StorageCapabilities, UnitChanges, UnitMark,
    VertexHandle,
};

use crate::script_host::{
    ScriptLimits, eval_value_atomic, eval_value_dry_run, eval_value_with_limits,
    materialize_traversals,
};

/// A foreign storage: every call forwards to the inner `TraversalGraph`, units, change capture
/// and commit hooks included, but its type is not `TraversalGraph`.
#[derive(Default)]
struct Wrapped(TraversalGraph);

impl GraphStorage for Wrapped {
    type VertexId = VertexHandle;
    type EdgeId = EdgeHandle;

    fn vertex_count(&self) -> usize {
        GraphStorage::vertex_count(&self.0)
    }
    fn edge_count(&self) -> usize {
        GraphStorage::edge_count(&self.0)
    }
    fn vertices(&self) -> impl Iterator<Item = VertexHandle> + '_ {
        GraphStorage::vertices(&self.0)
    }
    fn edges(&self) -> impl Iterator<Item = EdgeHandle> + '_ {
        GraphStorage::edges(&self.0)
    }
    fn get_vertex_by_id(&self, id: &str) -> Option<VertexHandle> {
        GraphStorage::get_vertex_by_id(&self.0, id)
    }
    fn get_edge_by_id(&self, id: &str) -> Option<EdgeHandle> {
        GraphStorage::get_edge_by_id(&self.0, id)
    }
    fn out_edges(&self, node: VertexHandle) -> impl Iterator<Item = EdgeHandle> + '_ {
        GraphStorage::out_edges(&self.0, node)
    }
    fn in_edges(&self, node: VertexHandle) -> impl Iterator<Item = EdgeHandle> + '_ {
        GraphStorage::in_edges(&self.0, node)
    }
    fn vertex_id(&self, node: VertexHandle) -> Option<&str> {
        GraphStorage::vertex_id(&self.0, node)
    }
    fn edge_id(&self, edge: EdgeHandle) -> Option<&str> {
        GraphStorage::edge_id(&self.0, edge)
    }
    fn vertex_label(&self, node: VertexHandle) -> Option<&str> {
        GraphStorage::vertex_label(&self.0, node)
    }
    fn edge_label(&self, edge: EdgeHandle) -> Option<&str> {
        GraphStorage::edge_label(&self.0, edge)
    }
    fn vertex_labels(&self, node: VertexHandle) -> &[CompactString] {
        GraphStorage::vertex_labels(&self.0, node)
    }
    fn vertex_properties(&self, node: VertexHandle) -> Option<&PropertyMap> {
        GraphStorage::vertex_properties(&self.0, node)
    }
    fn edge_properties(&self, edge: EdgeHandle) -> Option<&PropertyMap> {
        GraphStorage::edge_properties(&self.0, edge)
    }
    fn edge_endpoints(&self, edge: EdgeHandle) -> Option<(VertexHandle, VertexHandle)> {
        GraphStorage::edge_endpoints(&self.0, edge)
    }
    fn insert_vertex(
        &mut self,
        id: Option<CompactString>,
        labels: Vec<CompactString>,
        properties: PropertyMap,
    ) -> Result<VertexHandle, GraphError> {
        // A storage bug stand-in: a panic in the middle of a script.
        assert!(!labels.iter().any(|label| label == "panic"), "storage bug");
        GraphStorage::insert_vertex(&mut self.0, id, labels, properties)
    }
    fn add_vertex_label(
        &mut self,
        node: VertexHandle,
        label: CompactString,
    ) -> Result<bool, GraphError> {
        GraphStorage::add_vertex_label(&mut self.0, node, label)
    }
    fn remove_vertex_label(&mut self, node: VertexHandle, label: &str) -> Result<bool, GraphError> {
        GraphStorage::remove_vertex_label(&mut self.0, node, label)
    }
    fn insert_edge(
        &mut self,
        id: Option<CompactString>,
        label: Option<CompactString>,
        from: VertexHandle,
        to: VertexHandle,
        properties: PropertyMap,
    ) -> Result<EdgeHandle, GraphError> {
        GraphStorage::insert_edge(&mut self.0, id, label, from, to, properties)
    }
    fn set_vertex_property(
        &mut self,
        node: VertexHandle,
        key: CompactString,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        GraphStorage::set_vertex_property(&mut self.0, node, key, value)
    }
    fn set_edge_property(
        &mut self,
        edge: EdgeHandle,
        key: CompactString,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        GraphStorage::set_edge_property(&mut self.0, edge, key, value)
    }
    fn drop_vertex(&mut self, node: VertexHandle) -> Result<(), GraphError> {
        GraphStorage::drop_vertex(&mut self.0, node)
    }
    fn drop_vertex_property(&mut self, prop: &str, node: VertexHandle) -> Result<(), GraphError> {
        GraphStorage::drop_vertex_property(&mut self.0, prop, node)
    }
    fn drop_edge(&mut self, edge: EdgeHandle) -> Result<(), GraphError> {
        GraphStorage::drop_edge(&mut self.0, edge)
    }
    fn drop_edge_property(&mut self, prop: &str, edge: EdgeHandle) -> Result<(), GraphError> {
        GraphStorage::drop_edge_property(&mut self.0, prop, edge)
    }
    fn begin_unit(&mut self) -> Result<UnitMark, GraphError> {
        GraphStorage::begin_unit(&mut self.0)
    }
    fn commit_unit(&mut self, mark: UnitMark) -> Result<(), GraphError> {
        GraphStorage::commit_unit(&mut self.0, mark)
    }
    fn rollback_unit(&mut self, mark: UnitMark, reason: &RollbackReason) -> Result<(), GraphError> {
        GraphStorage::rollback_unit(&mut self.0, mark, reason)
    }
    fn unit_changes(&self, mark: UnitMark) -> Result<UnitChanges, GraphError> {
        GraphStorage::unit_changes(&self.0, mark)
    }
    fn discard_unit(&mut self, mark: UnitMark) -> Result<ChangeSet, GraphError> {
        GraphStorage::discard_unit(&mut self.0, mark)
    }
    fn capabilities(&self) -> Result<StorageCapabilities, GraphError> {
        GraphStorage::capabilities(&self.0)
    }
    fn register_commit_hook(&mut self, hook: Box<dyn CommitHook>) -> Result<(), GraphError> {
        GraphStorage::register_commit_hook(&mut self.0, hook)
    }
    fn last_commit_seq(&self) -> Result<u64, GraphError> {
        GraphStorage::last_commit_seq(&self.0)
    }
}

/// The modern graph in a `Graph<Wrapped>`.
fn foreign_modern() -> Arc<Graph<Wrapped>> {
    Arc::new(Graph::new(Wrapped(TraversalGraph::tinkerpop_modern())))
}

fn int(value: Dynamic) -> i64 {
    value.as_int().expect("an integer")
}

use graphersal::script::rhai::Dynamic;

#[test]
fn a_foreign_graph_is_an_any_graph_of_its_own_type() {
    let graph = foreign_modern();
    let any: Arc<dyn AnyGraph> = graph.clone().into_any_graph();
    assert!(any.storage_type_name().ends_with("Wrapped"));
    assert!(any.as_any().downcast_ref::<Graph<Wrapped>>().is_some());
    assert!(any.as_any().downcast_ref::<Graph>().is_none());
}

#[test]
fn scripts_read_and_write_a_foreign_graph() {
    let graph = foreign_modern();
    let count = eval_value_with_limits(
        graph.clone(),
        r#"g.addV("person").property("name", "ann").next(); g.V().hasLabel("person").count().next()"#,
        Default::default(),
        &ScriptLimits::default(),
    )
    .unwrap();
    assert_eq!(int(count), 5);
    assert_eq!(graph.read().vertex_count(), 7);
    // `Arc<dyn AnyGraph>` is accepted as well.
    let any: Arc<dyn AnyGraph> = graph.clone();
    let names = eval_value_with_limits(
        any,
        r#"g.V().has("name", "ann").values("name").toList()"#,
        Default::default(),
        &ScriptLimits::default(),
    )
    .unwrap();
    assert_eq!(names.into_array().unwrap().len(), 1);
}

#[test]
fn an_atomic_script_on_a_foreign_graph_commits_or_rolls_back_as_a_whole() {
    let graph = foreign_modern();
    let committed = eval_value_atomic(
        graph.clone(),
        r#"g.addV("person").property("name", "ann").next(); g.V().count().next()"#,
        Default::default(),
        &ScriptLimits::default(),
    )
    .unwrap();
    assert_eq!(int(committed), 7);
    assert_eq!(graph.read().vertex_count(), 7);
    let seq = graph.read().last_commit_seq().unwrap();

    let failed = eval_value_atomic(
        graph.clone(),
        r#"g.addV("person").property("name", "bob").next();
           g.V().values("name").asNumber(GType.LONG).toList();"#,
        Default::default(),
        &ScriptLimits::default(),
    );
    assert!(failed.is_err());
    assert_eq!(graph.read().vertex_count(), 7);
    assert_eq!(graph.read().last_commit_seq().unwrap(), seq);
}

#[test]
fn a_dry_run_on_a_foreign_graph_reports_and_undoes_its_changes() {
    let graph = foreign_modern();
    let (count, changes) = eval_value_dry_run(
        graph.clone(),
        r#"g.addV("person").property("name", "ann").next(); g.V().count().next()"#,
        Default::default(),
        &ScriptLimits::default(),
    )
    .unwrap();
    assert_eq!(int(count), 7);
    assert_eq!(changes.len(), 1);
    assert_eq!(graph.read().vertex_count(), 6);
    assert_eq!(graph.read().last_commit_seq().unwrap(), 0);
}

#[test]
fn a_panic_in_an_atomic_script_rolls_back_and_moves_the_storage_back() {
    let graph = foreign_modern();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        eval_value_atomic(
            graph.clone(),
            r#"g.addV("person").property("name", "ann").next(); g.addV("panic").next(); 1"#,
            Default::default(),
            &ScriptLimits::default(),
        )
    }));
    assert!(
        outcome.is_err(),
        "the storage's panic continues to the caller"
    );
    // The storage is back in the shared lock (not the empty one left by the move), and the
    // script's change is gone.
    assert_eq!(graph.read().vertex_count(), 6);
    assert_eq!(graph.read().edge_count(), 6);
    // The unit was closed: the next whole-script unit is the outermost one and commits.
    let count = eval_value_atomic(
        graph.clone(),
        r#"g.addV("person").next(); g.V().count().next()"#,
        Default::default(),
        &ScriptLimits::default(),
    )
    .unwrap();
    assert_eq!(int(count), 7);
    assert_eq!(graph.read().last_commit_seq().unwrap(), 1);
}

#[test]
fn a_traversal_returned_by_an_atomic_script_runs_on_the_shared_graph() {
    let graph = foreign_modern();
    let value = eval_value_atomic(
        graph.clone(),
        r#"g.addV("person").property("name", "ann").next(); g.V().hasLabel("person").values("name")"#,
        Default::default(),
        &ScriptLimits::default(),
    )
    .unwrap();
    // Retargeted from the script's private graph (empty once the script ended) to `graph`.
    let names = materialize_traversals(value, 0)
        .unwrap()
        .into_array()
        .unwrap();
    assert_eq!(names.len(), 5);
    // It sees later changes of the shared graph too.
    let _ = eval_value_with_limits(
        graph.clone(),
        r#"g.addV("person").property("name", "cy").next()"#,
        Default::default(),
        &ScriptLimits::default(),
    )
    .unwrap();
    let value = eval_value_atomic(
        graph.clone(),
        r#"[g.V().hasLabel("person"), 1]"#,
        Default::default(),
        &ScriptLimits::default(),
    )
    .unwrap();
    let items = materialize_traversals(value, 0)
        .unwrap()
        .into_array()
        .unwrap();
    assert_eq!(items[0].clone().into_array().unwrap().len(), 6);
}

#[test]
fn a_closure_returned_by_an_atomic_script_still_fails() {
    let graph = foreign_modern();
    let err = eval_value_atomic(
        graph.clone(),
        r#"g.addV("person").next(); |x| x + 1"#,
        Default::default(),
        &ScriptLimits::default(),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("cannot return a closure"), "{err}");
    assert_eq!(graph.read().vertex_count(), 6);
}

#[test]
fn a_panic_in_a_dry_run_discards_and_moves_the_storage_back() {
    let graph = foreign_modern();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        eval_value_dry_run(
            graph.clone(),
            r#"g.addV("person").next(); g.addV("panic").next(); 1"#,
            Default::default(),
            &ScriptLimits::default(),
        )
    }));
    assert!(outcome.is_err());
    assert_eq!(graph.read().vertex_count(), 6);
    let (count, changes) = eval_value_dry_run(
        graph.clone(),
        r#"g.addV("person").next(); g.V().count().next()"#,
        Default::default(),
        &ScriptLimits::default(),
    )
    .unwrap();
    assert_eq!(int(count), 7);
    assert_eq!(changes.len(), 1);
    assert_eq!(graph.read().vertex_count(), 6);
}

#[test]
fn with_side_effect_into_a_foreign_graph_is_unsupported() {
    let graph = foreign_modern();
    let err = eval_value_with_limits(
        graph.clone(),
        r#"g.withSideEffect("sg", g).E().hasLabel("knows").subgraph("sg").toList()"#,
        Default::default(),
        &ScriptLimits::default(),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("does not support"), "{err}");
    assert!(err.contains("subgraph() target"), "{err}");
    assert!(err.contains("Wrapped"), "{err}");
    assert!(
        err.contains("Help: subgraph() writes only into a TraversalGraph"),
        "{err}"
    );
    assert_eq!(graph.read().vertex_count(), 6);
}

#[test]
fn scratch_graphs_of_a_script_on_a_foreign_graph_stay_traversal_graphs() {
    let graph = foreign_modern();
    let limits = ScriptLimits::default();
    let run = |script: &str| {
        eval_value_with_limits(graph.clone(), script, Default::default(), &limits)
            .unwrap_or_else(|err| panic!("`{script}` failed: {err}"))
    };
    // `to_graph()` and `subgraph().cap()` build TraversalGraphs from the foreign graph's edges.
    assert_eq!(
        int(run(
            r#"g.E().hasLabel("knows").to_graph().V().count().next()"#
        )),
        3
    );
    assert_eq!(
        int(run(
            r#"let sg = g.E().hasLabel("created").subgraph("sg").cap("sg").next();
               sg.E().count().next()"#
        )),
        4
    );
    // A private target of withSideEffect receives the subgraph.
    assert_eq!(
        int(run(r#"let sg = GraphSource::empty();
               g.withSideEffect("sg", sg).E().hasLabel("knows").subgraph("sg").iterate();
               sg.V().count().next()"#)),
        3
    );
    // `__`, `_g` and `GraphSource::*` work beside the foreign `g`.
    assert_eq!(
        int(run(r#"g.V().where(__.out("created")).count().next()"#)),
        3
    );
    assert_eq!(int(run(r#"_g.addV("x").next(); _g.V().count().next()"#)), 1);
    assert_eq!(
        int(run(r#"GraphSource::tinkerpop_modern().V().count().next()"#)),
        6
    );
    assert_eq!(graph.read().vertex_count(), 6);
}
