//! Commit hooks, `capabilities()`, `apply_change_set` / `replay` and the snapshot position
//! (`book/src/users_guide/transactions.md`): veto and rollback, several hooks and their
//! order, `after_rollback` for every hook, empty units, schema changes, the commit sequence,
//! applying and replaying change sets, GraphML import as a unit, `Execution::mutated`.

use std::sync::{Arc, Mutex};

use graphersal::changes::{ChangeSet, Mutation};
use graphersal::prelude::*;
use graphersal::{
    changes::{CommitHook, HookError, RollbackReason},
    storage::StorageCapabilities,
};

// ---------------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------------

/// The events every hook of one test saw, in order: `"<hook>:before:<seq>"`,
/// `"<hook>:after:<seq>"`, `"<hook>:rollback:<vetoed|failed>"`.
type Events = Arc<Mutex<Vec<String>>>;

/// A hook that records its events, keeps every change set it accepted and vetoes while
/// `veto` is set.
struct Probe {
    name: &'static str,
    events: Events,
    accepted: Arc<Mutex<Vec<ChangeSet>>>,
    veto: Arc<Mutex<bool>>,
}

/// The error a vetoing probe returns.
#[derive(Debug)]
struct DiskFull;

impl std::fmt::Display for DiskFull {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("disk full")
    }
}

impl std::error::Error for DiskFull {}

impl CommitHook for Probe {
    fn name(&self) -> &str {
        self.name
    }

    fn before_commit(&mut self, changes: &ChangeSet) -> Result<(), HookError> {
        self.log(format!("before:{}", changes.commit_seq()));
        if *self.veto.lock().unwrap() {
            return Err(Box::new(DiskFull));
        }
        self.accepted.lock().unwrap().push(changes.clone());
        Ok(())
    }

    fn after_commit(&mut self, changes: &ChangeSet) {
        self.log(format!("after:{}", changes.commit_seq()));
    }

    fn after_rollback(&mut self, reason: &RollbackReason) {
        let kind = match reason {
            RollbackReason::Vetoed { hook, .. } => format!("vetoed by {hook}"),
            RollbackReason::Failed { .. } => "failed".to_string(),
            _ => "other".to_string(),
        };
        self.log(format!("rollback:{kind}"));
    }
}

impl Probe {
    fn log(&self, event: String) {
        self.events
            .lock()
            .unwrap()
            .push(format!("{}:{event}", self.name));
    }
}

/// Handles to read a probe's records and switch its veto after registration.
#[derive(Clone)]
struct ProbeHandle {
    accepted: Arc<Mutex<Vec<ChangeSet>>>,
    veto: Arc<Mutex<bool>>,
}

impl ProbeHandle {
    fn accepted(&self) -> Vec<ChangeSet> {
        self.accepted.lock().unwrap().clone()
    }

    fn set_veto(&self, on: bool) {
        *self.veto.lock().unwrap() = on;
    }
}

fn probe(name: &'static str, events: &Events) -> (Probe, ProbeHandle) {
    let handle = ProbeHandle {
        accepted: Arc::default(),
        veto: Arc::default(),
    };
    let probe = Probe {
        name,
        events: events.clone(),
        accepted: handle.accepted.clone(),
        veto: handle.veto.clone(),
    };
    (probe, handle)
}

/// Registers one probe named `name` on `storage`.
fn hook<S: GraphStorage>(storage: &mut S, name: &'static str, events: &Events) -> ProbeHandle {
    let (probe, handle) = probe(name, events);
    storage
        .register_commit_hook(Box::new(probe))
        .expect("the storage supports hooks");
    handle
}

fn events(events: &Events) -> Vec<String> {
    std::mem::take(&mut *events.lock().unwrap())
}

fn names(graph: &TraversalGraph) -> Vec<ElementProperty> {
    let mut out = graph
        .traversal()
        .v(None::<()>)
        .values("name")
        .to_list()
        .unwrap();
    out.sort_by_key(|value| format!("{value:?}"));
    out
}

/// `(id, labels, properties)` per vertex and `(id, label, out, in, properties)` per edge, sorted
/// by id, plus the stored schema: what an applied or replayed change set must reproduce.
type Content = (
    Vec<(String, Vec<String>, Vec<(String, ElementProperty)>)>,
    Vec<(
        String,
        Option<String>,
        String,
        String,
        Vec<(String, ElementProperty)>,
    )>,
    Option<GraphSchema>,
);

fn sorted(element: Option<&PropertyMap>) -> Vec<(String, ElementProperty)> {
    let mut out: Vec<_> = element
        .into_iter()
        .flatten()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn content<S: GraphStorage>(graph: &S) -> Content {
    let mut vertices: Vec<_> = graph
        .vertices()
        .unwrap()
        .map(|h| {
            (
                graph.vertex_id(h).unwrap().unwrap().to_string(),
                graph
                    .vertex_labels(h)
                    .unwrap()
                    .iter()
                    .map(|l| l.to_string())
                    .collect(),
                sorted(graph.vertex_properties(h).unwrap()),
            )
        })
        .collect();
    vertices.sort_by(|a, b| a.0.cmp(&b.0));
    let mut edges: Vec<_> = graph
        .edges()
        .unwrap()
        .map(|h| {
            let (from, to) = graph.edge_endpoints(h).unwrap().unwrap();
            (
                graph.edge_id(h).unwrap().unwrap().to_string(),
                graph.edge_label(h).unwrap().map(str::to_string),
                graph.vertex_id(from).unwrap().unwrap().to_string(),
                graph.vertex_id(to).unwrap().unwrap().to_string(),
                sorted(graph.edge_properties(h).unwrap()),
            )
        })
        .collect();
    edges.sort_by(|a, b| a.0.cmp(&b.0));
    (vertices, edges, graph.get_schema().unwrap())
}

fn person_schema(mode: SchemaMode) -> GraphSchema {
    GraphSchema::from_json(
        r#"{"vertices": {"person": {"schema": {"type": "object",
            "properties": {"name": {"type": "string"}}, "required": ["name"]}}}}"#,
    )
    .unwrap()
    .with_mode(mode)
}

// ---------------------------------------------------------------------------------------------
// Capabilities
// ---------------------------------------------------------------------------------------------

#[test]
fn traversal_graph_reports_atomic_change_capture_and_writable() {
    let graph = TraversalGraph::new();
    let caps = graph.capabilities().unwrap();
    assert!(caps.atomic && caps.change_capture && caps.writable);
    assert!(!caps.immutable && !caps.durability && !caps.concurrent_writers);
    assert!(caps.is_consistent());
    let boxed: Box<TraversalGraph> = Box::new(TraversalGraph::new());
    assert_eq!(boxed.capabilities().unwrap(), caps);
    let mut owned = TraversalGraph::new();
    assert_eq!(GraphStorage::capabilities(&&mut owned).unwrap(), caps);
    assert_eq!(
        GraphStorage::capabilities(&&graph).unwrap(),
        StorageCapabilities::none(),
        "a read-only view claims nothing"
    );
}

#[test]
fn change_capture_without_atomic_is_inconsistent() {
    let bad = StorageCapabilities::none().with_change_capture(true);
    assert!(!bad.is_consistent());
    assert!(bad.with_atomic(true).is_consistent());
    assert!(
        !StorageCapabilities::none()
            .with_writable(true)
            .with_immutable(true)
            .is_consistent()
    );
}

#[test]
fn hooks_on_a_read_only_view_or_a_shared_arc_are_rejected() {
    let graph = TraversalGraph::new();
    let events = Events::default();
    let (p, _) = probe("p", &events);
    let mut view = &graph;
    assert!(matches!(
        view.register_commit_hook(Box::new(p)),
        Err(GraphError::Unsupported { .. })
    ));
    let mut shared = Arc::new(TraversalGraph::new());
    let _second = shared.clone();
    let (p, _) = probe("p", &events);
    assert!(shared.register_commit_hook(Box::new(p)).is_err());
}

// ---------------------------------------------------------------------------------------------
// Events, order, veto
// ---------------------------------------------------------------------------------------------

#[test]
fn a_committed_traversal_runs_before_then_after_in_registration_order() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let log = Events::default();
    let a = hook(&mut graph, "a", &log);
    let b = hook(&mut graph, "b", &log);
    graph
        .traversal_mut()
        .add_v("person")
        .property("name", "ann")
        .to_list()
        .unwrap();
    assert_eq!(
        events(&log),
        ["a:before:1", "b:before:1", "a:after:1", "b:after:1"]
    );
    assert_eq!(a.accepted(), b.accepted());
    assert_eq!(a.accepted().len(), 1);
    assert!(matches!(
        a.accepted()[0].mutations(),
        [Mutation::AddVertex { .. }]
    ));
    assert_eq!(graph.last_commit_seq(), 1);
}

#[test]
fn a_veto_rolls_the_traversal_back_and_every_hook_hears_it() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let before = content(&graph);
    let log = Events::default();
    let _a = hook(&mut graph, "a", &log);
    let b = hook(&mut graph, "b", &log);
    let c = hook(&mut graph, "c", &log);
    b.set_veto(true);
    let err = graph
        .traversal_mut()
        .v("1")
        .property("age", 30)
        .out("knows")
        .drop()
        .to_list()
        .unwrap_err();
    match &*err {
        TraverserError::Graph(GraphError::CommitRejected { hook, source, .. }) => {
            assert_eq!(hook, "b");
            assert!(
                source.downcast_ref::<DiskFull>().is_some(),
                "the hook's own error"
            );
        }
        other => panic!("expected CommitRejected, got {other:?}"),
    }
    assert!(err.to_string().contains("disk full"), "{err}");
    // c's before_commit never ran; everybody got after_rollback.
    assert_eq!(
        events(&log),
        [
            "a:before:1",
            "b:before:1",
            "a:rollback:vetoed by b",
            "b:rollback:vetoed by b",
            "c:rollback:vetoed by b",
        ]
    );
    assert_eq!(content(&graph), before, "nothing of the unit is left");
    assert_eq!(graph.last_commit_seq(), 0, "a vetoed unit takes no number");
    assert!(c.accepted().is_empty());
    // The next accepted unit takes the number the vetoed one did not.
    b.set_veto(false);
    graph
        .traversal_mut()
        .v("1")
        .property("age", 31)
        .to_list()
        .unwrap();
    assert_eq!(graph.last_commit_seq(), 1);
    assert_eq!(c.accepted()[0].commit_seq(), 1);
}

#[test]
fn a_failing_traversal_sends_after_rollback_failed_to_every_hook() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let log = Events::default();
    hook(&mut graph, "a", &log);
    hook(&mut graph, "b", &log);
    let failed = graph
        .traversal_mut()
        .add_v("n")
        .constant("x")
        .as_number(GType::LONG)
        .to_list();
    assert!(failed.is_err());
    assert_eq!(events(&log), ["a:rollback:failed", "b:rollback:failed"]);
    assert_eq!(graph.last_commit_seq(), 0);
}

#[test]
fn units_that_change_nothing_emit_nothing() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let log = Events::default();
    hook(&mut graph, "a", &log);
    graph.traversal_mut().v(None::<()>).count().next().unwrap();
    graph.traversal_mut().v("nope").drop().to_list().unwrap();
    graph
        .traversal_mut()
        .v("1")
        .properties("missing")
        .drop()
        .to_list()
        .unwrap();
    // A read-only traversal that fails rolls back nothing and says nothing.
    let _ = graph
        .traversal_mut()
        .v("1")
        .values("name")
        .as_number(GType::LONG)
        .to_list();
    assert!(events(&log).is_empty());
    assert_eq!(graph.last_commit_seq(), 0);
}

#[test]
fn commit_seq_is_monotonic_and_matches_the_change_sets() {
    let mut graph = TraversalGraph::new();
    let log = Events::default();
    let a = hook(&mut graph, "a", &log);
    for i in 0..5 {
        graph
            .traversal_mut()
            .add_v("n")
            .property("i", i)
            .to_list()
            .unwrap();
        graph.traversal_mut().v(None::<()>).count().next().unwrap();
    }
    let seqs: Vec<u64> = a.accepted().iter().map(ChangeSet::commit_seq).collect();
    assert_eq!(seqs, [1, 2, 3, 4, 5]);
    assert_eq!(graph.last_commit_seq(), 5);
}

#[test]
fn a_nested_savepoint_emits_nothing_of_its_own() {
    let mut graph = TraversalGraph::new();
    let log = Events::default();
    let a = hook(&mut graph, "a", &log);
    let outer = graph.begin_unit().unwrap();
    graph
        .add_vertex(Some("a".into()), vec![], PropertyMap::default())
        .unwrap();
    let inner = graph.begin_unit().unwrap();
    graph
        .add_vertex(Some("b".into()), vec![], PropertyMap::default())
        .unwrap();
    graph
        .rollback_unit(inner, &RollbackReason::failed(&"inner"))
        .unwrap();
    let inner = graph.begin_unit().unwrap();
    graph
        .add_vertex(Some("c".into()), vec![], PropertyMap::default())
        .unwrap();
    graph.commit_unit(inner).unwrap();
    assert!(events(&log).is_empty(), "nothing before the outer commit");
    graph.commit_unit(outer).unwrap();
    assert_eq!(events(&log), ["a:before:1", "a:after:1"]);
    assert_eq!(a.accepted()[0].len(), 2);
}

// ---------------------------------------------------------------------------------------------
// Schema changes
// ---------------------------------------------------------------------------------------------

#[test]
fn a_schema_change_outside_a_unit_is_a_captured_unit() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let log = Events::default();
    let a = hook(&mut graph, "a", &log);
    graph.set_schema(person_schema(SchemaMode::Open)).unwrap();
    assert_eq!(graph.last_commit_seq(), 1);
    assert!(matches!(
        a.accepted()[0].mutations(),
        [Mutation::SetSchema { .. }]
    ));
    // The same through the lock of a shared `Graph`.
    let shared: Graph = Graph::new(TraversalGraph::new());
    let log2 = Events::default();
    let b = hook(&mut *shared.write(), "b", &log2);
    shared
        .write()
        .set_schema(person_schema(SchemaMode::Closed))
        .unwrap();
    assert_eq!(b.accepted().len(), 1);
    assert_eq!(shared.read().last_commit_seq(), 1);
}

#[test]
fn a_vetoed_schema_change_is_rolled_back() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let log = Events::default();
    let a = hook(&mut graph, "a", &log);
    a.set_veto(true);
    let err = graph
        .set_schema(person_schema(SchemaMode::Open))
        .unwrap_err();
    assert!(matches!(err, GraphError::CommitRejected { .. }));
    assert_eq!(
        graph.get_schema().unwrap(),
        None,
        "the old (absent) schema is back"
    );
    // Also when it is part of a traversal-level unit: `GraphTraversalSource::set_schema`.
    let err = graph
        .traversal_mut()
        .set_schema(person_schema(SchemaMode::Open))
        .unwrap_err();
    assert!(matches!(
        &*err,
        TraverserError::Graph(GraphError::CommitRejected { .. })
    ));
    assert_eq!(graph.get_schema().unwrap(), None);
    assert_eq!(graph.last_commit_seq(), 0);
    assert_eq!(
        events(&log),
        [
            "a:before:1",
            "a:rollback:vetoed by a",
            "a:before:1",
            "a:rollback:vetoed by a"
        ]
    );
}

#[test]
fn schema_and_data_of_one_traversal_roll_back_together_on_a_veto() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let before = content(&graph);
    let log = Events::default();
    let a = hook(&mut graph, "a", &log);
    a.set_veto(true);
    let mark = graph.begin_unit().unwrap();
    graph.set_schema(person_schema(SchemaMode::Open)).unwrap();
    let marko = graph.get_vertex_by_id("1").unwrap().unwrap();
    graph
        .set_vertex_property(marko, "name".into(), "MARKO".into())
        .unwrap();
    assert!(graph.commit_unit(mark).is_err());
    assert_eq!(content(&graph), before);
}

// ---------------------------------------------------------------------------------------------
// apply_change_set, replay, snapshot position
// ---------------------------------------------------------------------------------------------

/// A journal of mixed units recorded on a live modern graph: the graph at the end and the
/// accepted change sets.
fn journal() -> (TraversalGraph, Vec<ChangeSet>) {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let log = Events::default();
    let a = hook(&mut graph, "journal", &log);
    graph.set_schema(person_schema(SchemaMode::Open)).unwrap();
    graph
        .traversal_mut()
        .add_v("person")
        .property("name", "ann")
        .property("id", ElementProperty::Uuid(7))
        .to_list()
        .unwrap();
    graph
        .traversal_mut()
        .v("1")
        .property("age", 30)
        .out("knows")
        .drop()
        .to_list()
        .unwrap();
    let marko = graph.get_vertex_by_id("1").unwrap().unwrap();
    let ann = graph
        .traversal()
        .v(None::<()>)
        .has("name", "ann")
        .id()
        .next()
        .unwrap();
    let ann = match ann {
        Some(ElementProperty::String(id)) => graph.get_vertex_by_id(&id).unwrap().unwrap(),
        other => panic!("unexpected id {other:?}"),
    };
    let mark = graph.begin_unit().unwrap();
    graph
        .add_edge(
            Some("e9".into()),
            Some("knows".into()),
            marko,
            ann,
            PropertyMap::default(),
        )
        .unwrap();
    graph.add_vertex_label(ann, "admin".into()).unwrap();
    graph.drop_vertex_property("age", marko).unwrap();
    graph.commit_unit(mark).unwrap();
    (graph, a.accepted())
}

#[test]
fn apply_change_set_into_a_second_graph_yields_an_equal_graph() {
    let (source, sets) = journal();
    assert_eq!(sets.len(), 4);
    let mut replica = TraversalGraph::tinkerpop_modern();
    let log = Events::default();
    let r = hook(&mut replica, "replica", &log);
    for set in &sets {
        replica.apply_change_set(set).unwrap();
    }
    assert_eq!(content(&replica), content(&source));
    // Each apply is one live, captured unit with the replica's own numbering.
    assert_eq!(replica.last_commit_seq(), 4);
    assert_eq!(
        r.accepted()
            .iter()
            .map(|s| s.mutations().to_vec())
            .collect::<Vec<_>>(),
        sets.iter()
            .map(|s| s.mutations().to_vec())
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_failing_or_vetoed_apply_leaves_nothing_behind() {
    let (_, sets) = journal();
    // The drop of vertex 2 cannot be applied to an empty graph: VertexNotFound, rolled back.
    let mut empty = TraversalGraph::new();
    let log = Events::default();
    hook(&mut empty, "a", &log);
    let err = empty.apply_change_set(&sets[2]).unwrap_err();
    assert!(
        matches!(
            err,
            GraphError::EdgeNotFound { .. } | GraphError::VertexNotFound { .. }
        ),
        "{err:?}"
    );
    assert_eq!(content(&empty), content(&TraversalGraph::new()));
    // It failed before it changed anything: an empty unit, so no event.
    assert!(events(&log).is_empty());
    // A veto rolls the whole applied set back.
    let mut replica = TraversalGraph::tinkerpop_modern();
    let before = content(&replica);
    let v = hook(&mut replica, "v", &log);
    v.set_veto(true);
    assert!(matches!(
        replica.apply_change_set(&sets[0]),
        Err(GraphError::CommitRejected { .. })
    ));
    assert_eq!(content(&replica), before);
}

#[test]
fn replay_on_an_unpublished_graph_fires_no_hook_and_continues_the_numbering() {
    let (source, sets) = journal();
    let log = Events::default();
    let (p, handle) = probe("wal", &log);
    // Even with a hook already registered, a replay must not re-write the journal it reads.
    let mut recovered = TraversalGraph::tinkerpop_modern();
    recovered.register_commit_hook(Box::new(p)).unwrap();
    for set in &sets {
        recovered = recovered.replay(set).unwrap();
    }
    assert!(events(&log).is_empty(), "no hook fired");
    assert!(handle.accepted().is_empty());
    assert_eq!(content(&recovered), content(&source));
    assert_eq!(recovered.last_commit_seq(), source.last_commit_seq());
    // The next live commit continues the journal's numbering.
    recovered.traversal_mut().add_v("x").to_list().unwrap();
    assert_eq!(
        handle.accepted()[0].commit_seq(),
        source.last_commit_seq() + 1
    );
}

#[test]
fn replay_of_a_broken_journal_is_an_error() {
    let (_, sets) = journal();
    assert!(TraversalGraph::new().replay(&sets[2]).is_err());
}

#[cfg(feature = "serde")]
#[test]
fn a_serialized_journal_replays_into_an_identical_graph_without_a_schema_declaration() {
    let mut graph = TraversalGraph::new();
    let log = Events::default();
    let a = hook(&mut graph, "a", &log);
    graph
        .traversal_mut()
        .add_v("doc")
        .property("id", ElementProperty::Uuid(0xabcdef))
        .to_list()
        .unwrap();
    let wire: Vec<String> = a
        .accepted()
        .iter()
        .map(|set| serde_json::to_string(set).unwrap())
        .collect();
    let mut recovered = TraversalGraph::new();
    for text in &wire {
        let set: ChangeSet = serde_json::from_str(text).unwrap();
        recovered = recovered.replay(&set).unwrap();
    }
    assert_eq!(content(&recovered), content(&graph));
}

#[test]
fn a_snapshot_with_position_restores_the_numbering() {
    let (source, _) = journal();
    let position = source.last_commit_seq();
    let restored = TraversalGraph::new().with_last_commit_seq(position);
    assert_eq!(restored.last_commit_seq(), position);
    assert_eq!(GraphStorage::last_commit_seq(&restored).unwrap(), position);
}

#[cfg(feature = "io")]
#[test]
fn a_graphml_snapshot_plus_position_and_the_wal_tail_recover_the_graph() {
    // Snapshot under the read lock: the export and the position belong together.
    let (mut live, _) = journal();
    let shared: Graph = Graph::new(TraversalGraph::tinkerpop_modern());
    std::mem::swap(&mut *shared.write(), &mut live);
    let (text, position) = {
        let guard = shared.read();
        let mut buf = Vec::new();
        guard.export_graphml_writer(&mut buf).unwrap();
        (buf, guard.last_commit_seq())
    };
    // More commits after the snapshot: the WAL tail.
    let log = Events::default();
    let tail = hook(&mut *shared.write(), "tail", &log);
    shared
        .write()
        .traversal_mut()
        .add_v("person")
        .property("name", "bob")
        .to_list()
        .unwrap();
    // Recovery: load the snapshot (outside units), continue from its position, replay the tail.
    let loaded = GraphSource::from_graphml_reader(std::io::Cursor::new(text)).unwrap();
    let mut recovered = std::mem::take(&mut *loaded.write()).with_last_commit_seq(position);
    for set in tail.accepted() {
        assert!(set.commit_seq() > position);
        recovered = recovered.replay(&set).unwrap();
    }
    let mut want = names(&shared.read());
    let mut got = names(&recovered);
    want.sort_by_key(|v| format!("{v:?}"));
    got.sort_by_key(|v| format!("{v:?}"));
    assert_eq!(got, want);
    assert_eq!(recovered.last_commit_seq(), shared.read().last_commit_seq());
}

// ---------------------------------------------------------------------------------------------
// GraphML import into a live graph
// ---------------------------------------------------------------------------------------------

#[cfg(feature = "io")]
const TWO_NODES: &str = r#"<graphml>
  <key id="labelV" for="node" attr.name="labelV" attr.type="string"/>
  <key id="name" for="node" attr.name="name" attr.type="string"/>
  <graph>
    <node id="a"><data key="labelV">person</data><data key="name">ann</data></node>
    <node id="b"><data key="labelV">person</data><data key="name">bob</data></node>
    <edge id="ab" source="a" target="b"/>
  </graph>
</graphml>"#;

#[cfg(feature = "io")]
#[test]
fn a_live_graphml_import_is_one_captured_unit() {
    let mut graph = GraphSource::empty();
    let log = Events::default();
    let a = hook(&mut *graph.write(), "a", &log);
    graph
        .import_graphml_reader(std::io::Cursor::new(TWO_NODES.as_bytes()))
        .unwrap();
    assert_eq!(events(&log), ["a:before:1", "a:after:1"]);
    let ops: Vec<_> = a.accepted()[0]
        .mutations()
        .iter()
        .map(|m| match m {
            Mutation::AddVertex { id, .. } => format!("+v{id}"),
            Mutation::AddEdge { id, .. } => format!("+e{id}"),
            _ => "other".to_string(),
        })
        .collect();
    assert_eq!(ops, ["+va", "+vb", "+eab"]);
}

#[cfg(feature = "io")]
#[test]
fn a_vetoed_graphml_import_leaves_nothing_behind() {
    let mut graph = GraphSource::tinkerpop_modern();
    let before = content(&*graph.read());
    let log = Events::default();
    let a = hook(&mut *graph.write(), "a", &log);
    a.set_veto(true);
    let err = graph
        .import_graphml_reader(std::io::Cursor::new(TWO_NODES.as_bytes()))
        .unwrap_err();
    assert!(
        matches!(err, IOError::Graph(GraphError::CommitRejected { .. })),
        "{err:?}"
    );
    assert_eq!(content(&*graph.read()), before);
}

#[cfg(feature = "io")]
#[test]
fn loading_an_unpublished_graph_records_nothing() {
    let graph =
        GraphSource::from_graphml_reader(std::io::Cursor::new(TWO_NODES.as_bytes())).unwrap();
    assert_eq!(graph.read().vertex_count(), 2);
    assert_eq!(graph.read().last_commit_seq(), 0, "a load is not a commit");
}

// ---------------------------------------------------------------------------------------------
// Execution::mutated
// ---------------------------------------------------------------------------------------------

#[test]
fn execution_reports_whether_it_committed_a_change() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    assert!(graph.traversal_mut().add_v("x").execute().mutated());
    assert!(
        !graph
            .traversal_mut()
            .v(None::<()>)
            .count()
            .execute()
            .mutated()
    );
    assert!(!graph.traversal_mut().v("nope").drop().execute().mutated());
    let failed = graph
        .traversal_mut()
        .add_v("n")
        .constant("x")
        .as_number(GType::LONG)
        .execute();
    assert!(!failed.is_ok() && !failed.mutated());
    let log = Events::default();
    let a = hook(&mut graph, "a", &log);
    a.set_veto(true);
    let vetoed = graph.traversal_mut().add_v("y").execute();
    assert!(!vetoed.mutated());
    assert!(matches!(
        vetoed.error(),
        Some(TraverserError::Graph(GraphError::CommitRejected { .. }))
    ));
    assert!(vetoed.results().is_empty());
}

// ---------------------------------------------------------------------------------------------
// Rhai
// ---------------------------------------------------------------------------------------------

#[cfg(feature = "script")]
#[test]
fn rhai_statements_are_units_a_veto_is_a_script_error_and_execute_reports_mutated() {
    let graph = Arc::new(GraphSource::tinkerpop_modern());
    let log = Events::default();
    let a = hook(&mut *graph.write(), "a", &log);
    let mutated = crate::script_host::eval_value(
        graph.clone(),
        r#"let r = g.addV("a").execute(); let s = g.V().count().execute(); [r.mutated, s.mutated]"#,
    )
    .unwrap()
    .into_array()
    .unwrap();
    assert_eq!(mutated[0].as_bool(), Ok(true));
    assert_eq!(mutated[1].as_bool(), Ok(false));
    assert_eq!(a.accepted().len(), 1);
    a.set_veto(true);
    let err =
        crate::script_host::eval_value(graph.clone(), r#"g.add_v("b").to_list()"#).unwrap_err();
    assert!(err.to_string().contains("disk full"), "{err}");
    assert_eq!(
        graph
            .read()
            .traversal()
            .v(None::<()>)
            .has_label("b")
            .count()
            .next()
            .unwrap(),
        Some(ElementProperty::from(0i64))
    );
}

// ---------------------------------------------------------------------------------------------
// Defaults of the hook trait, rollback reasons, capability builders
// ---------------------------------------------------------------------------------------------

/// A hook that overrides only `after_rollback`: the default `before_commit` accepts every
/// commit, and the default `name` is the type name.
struct OnlyRollbacks(Arc<Mutex<Vec<String>>>);

impl CommitHook for OnlyRollbacks {
    fn after_rollback(&mut self, reason: &RollbackReason) {
        self.0.lock().unwrap().push(reason.message().to_string());
    }
}

#[test]
fn default_hook_methods_accept_and_rollback_reasons_carry_the_error_text() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let seen = Arc::new(Mutex::new(Vec::new()));
    graph
        .register_commit_hook(Box::new(OnlyRollbacks(seen.clone())))
        .unwrap();
    graph
        .traversal_mut()
        .add_v("person")
        .to_list()
        .expect("the default before_commit accepts");
    assert_eq!(graph.vertex_count(), 7);
    assert!(seen.lock().unwrap().is_empty());

    // a failing traversal is rolled back and reported with the error text
    let err = graph
        .traversal_mut()
        .add_v("person")
        .constant("x")
        .as_number(GType::LONG)
        .to_list()
        .expect_err("not a number");
    assert_eq!(graph.vertex_count(), 7);
    let messages = seen.lock().unwrap().clone();
    assert_eq!(messages.len(), 1);
    assert!(!messages[0].is_empty());
    assert_eq!(
        messages[0],
        err.to_string(),
        "the rollback carries the error text"
    );

    let reason = RollbackReason::failed(&"boom");
    assert_eq!(reason.message(), "boom");
    assert!(matches!(reason, RollbackReason::Failed { .. }));
}

#[test]
fn capability_builders_set_each_flag() {
    use graphersal::storage::IsolationLevel;
    let caps = StorageCapabilities::none()
        .with_writable(true)
        .with_atomic(true)
        .with_isolation(IsolationLevel::Snapshot)
        .with_concurrent_writers(true)
        .with_durability(true);
    assert_eq!(caps.isolation, IsolationLevel::Snapshot);
    assert!(caps.concurrent_writers && caps.durability && caps.writable && caps.atomic);
    assert!(caps.is_consistent());
    assert_eq!(StorageCapabilities::none().isolation, IsolationLevel::None);
}
