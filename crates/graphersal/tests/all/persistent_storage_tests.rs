//! The storage-side persistence foundations: the public `CommitHooks` helper (user hooks, then
//! the journal slot), the public `ChangeSet` builder and `Mutation` constructors, and
//! `PersistentStorage` on `TraversalGraph` (position, load sink, `clear`, journal slot).
#![cfg(feature = "persist")]

use std::io::Write;
use std::sync::{Arc, Mutex};

use graphersal::catalog::{Definition, DefinitionKind, QueryDefinition};
use graphersal::changes::{
    ChangeSet, CommitHook, CommitMetadata, EdgeState, ElementRef, HookError, Mutation,
    RollbackReason, VertexState,
};
use graphersal::error::{DiagnosticError, GraphError};
use graphersal::persist::{Durability, Journal};
use graphersal::prelude::*;
use graphersal::schema::{GraphSchema, PropertySchema, SchemaMode};
use graphersal::storage::{CommitHooks, PersistentStorage};

type Log = Arc<Mutex<Vec<String>>>;

/// A user hook that logs its events and vetoes when told to.
struct Logging {
    name: &'static str,
    log: Log,
    veto: bool,
}

impl CommitHook for Logging {
    fn name(&self) -> &str {
        self.name
    }
    fn before_commit(&mut self, _: &ChangeSet) -> Result<(), HookError> {
        self.log
            .lock()
            .unwrap()
            .push(format!("{} before", self.name));
        if self.veto {
            return Err(format!("{} says no", self.name).into());
        }
        Ok(())
    }
    fn after_commit(&mut self, _: &ChangeSet) {
        self.log
            .lock()
            .unwrap()
            .push(format!("{} after", self.name));
    }
    fn after_rollback(&mut self, reason: &RollbackReason) {
        let kind = match reason {
            RollbackReason::Vetoed { .. } => "vetoed",
            _ => "failed",
        };
        self.log
            .lock()
            .unwrap()
            .push(format!("{} rollback {kind}", self.name));
    }
}

fn logging(name: &'static str, log: &Log, veto: bool) -> Box<Logging> {
    Box::new(Logging {
        name,
        log: log.clone(),
        veto,
    })
}

/// A journal writer that logs every write into the shared log.
struct LoggingWriter(Log);

impl Write for LoggingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().push("journal write".to_string());
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn add_one(graph: &mut TraversalGraph) -> Result<(), Box<TraverserError>> {
    graph.traversal_mut().add_v("person").to_list().map(drop)
}

#[test]
fn the_journal_runs_after_every_user_hook_whatever_the_registration_order() {
    let log: Log = Arc::default();
    let mut graph = TraversalGraph::new();
    graph
        .register_commit_hook(logging("first", &log, false))
        .unwrap();
    Journal::new(LoggingWriter(log.clone()), Durability::Os)
        .attach(&mut graph)
        .unwrap();
    // Registered after the journal, it still runs before it.
    graph
        .register_commit_hook(logging("second", &log, false))
        .unwrap();
    log.lock().unwrap().clear();
    add_one(&mut graph).unwrap();
    let events = log.lock().unwrap().clone();
    let journal_at = events.iter().position(|e| e == "journal write").unwrap();
    assert_eq!(&events[..2], ["first before", "second before"]);
    assert_eq!(journal_at, 2, "{events:?}");
    assert!(events.ends_with(&["first after".to_string(), "second after".to_string()]));
}

#[test]
fn a_veto_stops_before_the_journal() {
    let log: Log = Arc::default();
    let mut graph = TraversalGraph::new();
    Journal::new(LoggingWriter(log.clone()), Durability::Os)
        .attach(&mut graph)
        .unwrap();
    graph
        .register_commit_hook(logging("veto", &log, true))
        .unwrap();
    graph
        .register_commit_hook(logging("later", &log, false))
        .unwrap();
    log.lock().unwrap().clear();
    let err = add_one(&mut graph).unwrap_err();
    assert!(err.to_string().contains("veto says no"), "{err}");
    let events = log.lock().unwrap().clone();
    assert_eq!(
        events,
        [
            "veto before",
            "veto rollback vetoed",
            "later rollback vetoed"
        ],
        "nothing reached the journal"
    );
    assert_eq!(graph.vertex_count(), 0);
    assert_eq!(graph.last_commit_seq(), 0);
}

#[test]
fn commit_hooks_embedded_by_a_storage_keep_the_order_and_report_the_veto() {
    let log: Log = Arc::default();
    let mut hooks = CommitHooks::new();
    assert!(!hooks.is_active());
    assert!(!hooks.has_journal());
    hooks.register(logging("a", &log, false));
    hooks.register(logging("b", &log, true));
    hooks.register(logging("c", &log, false));
    assert!(hooks.is_active());
    let changes = ChangeSet::builder(1, 10).build().unwrap();
    let err = hooks.before_commit(&changes).unwrap_err();
    assert!(matches!(&err, GraphError::CommitRejected { hook, .. } if hook == "b"));
    hooks.after_rollback(&RollbackReason::from_error(&err));
    assert_eq!(
        *log.lock().unwrap(),
        [
            "a before",
            "b before",
            "a rollback vetoed",
            "b rollback vetoed",
            "c rollback vetoed"
        ]
    );
    // Without a journal no mark can be set; a bad name is refused first.
    assert!(matches!(
        hooks.mark("m", 1, 10),
        Err(GraphError::MarkUnavailable { .. })
    ));
    assert!(matches!(
        hooks.mark("", 1, 10),
        Err(GraphError::MarkUnavailable { .. })
    ));
    assert!(matches!(
        RollbackReason::from_error(&GraphError::unsupported("x")),
        RollbackReason::Failed { .. }
    ));
}

fn query(name: &str) -> Definition {
    Definition::query(
        name,
        QueryDefinition::new(format!("fn {name}() {{ g.v() }}")),
    )
}

fn schema() -> GraphSchema {
    GraphSchema::new()
        .with_mode(SchemaMode::Open)
        .with_vertex("person", PropertySchema::object())
}

/// What a live graph captures for one unit, and the same change set built by hand.
fn captured_and_built() -> (ChangeSet, ChangeSet) {
    let captured: Arc<Mutex<Vec<ChangeSet>>> = Arc::default();
    struct Collect(Arc<Mutex<Vec<ChangeSet>>>);
    impl CommitHook for Collect {
        fn before_commit(&mut self, changes: &ChangeSet) -> Result<(), HookError> {
            self.0.lock().unwrap().push(changes.clone());
            Ok(())
        }
    }
    let mut live = TraversalGraph::new();
    live.register_commit_hook(Box::new(Collect(captured.clone())))
        .unwrap();
    live.transaction_with(CommitMetadata::new().with_principal("ann"), |g| {
        g.set_schema(schema())?;
        let mut props = PropertyMap::new();
        props.insert("name", ElementProperty::from("ann"));
        let a = g.add_vertex(Some("a".into()), vec!["person".into()], props)?;
        let b = g.add_vertex(Some("b".into()), vec![], PropertyMap::new())?;
        g.add_edge(
            Some("e".into()),
            Some("knows".into()),
            a,
            b,
            PropertyMap::new(),
        )?;
        g.set_vertex_property(a, "age".into(), 29i64.into())?;
        g.add_vertex_label(b, "robot".into())?;
        let gone = g.add_vertex(Some("gone".into()), vec![], PropertyMap::new())?;
        g.drop_vertex(gone)?;
        g.set_definition(query("q"))?;
        Ok::<_, GraphError>(())
    })
    .unwrap();
    let captured = captured.lock().unwrap()[0].clone();

    let mut name = PropertyMap::new();
    name.insert("name", ElementProperty::from("ann"));
    let mut builder = ChangeSet::builder(captured.commit_seq(), captured.committed_at())
        .with_sequences(captured.node_sequence(), captured.edge_sequence())
        .with_metadata(CommitMetadata::new().with_principal("ann"));
    builder
        .push(Mutation::set_schema(GraphSchema::new(), schema()))
        .push(Mutation::add_vertex("a", ["person"], name))
        .push(Mutation::add_vertex(
            "b",
            Vec::<String>::new(),
            PropertyMap::new(),
        ))
        .push(Mutation::add_edge(
            "e",
            Some("knows"),
            "a",
            "b",
            PropertyMap::new(),
        ))
        .push(Mutation::set_property(
            ElementRef::vertex("a"),
            "age",
            None,
            29i64.into(),
        ))
        .push(Mutation::add_label("b", "robot"))
        .push(Mutation::add_vertex(
            "gone",
            Vec::<String>::new(),
            PropertyMap::new(),
        ))
        .push(Mutation::drop_vertex(
            "gone",
            VertexState::new(Vec::<String>::new(), PropertyMap::new()),
        ))
        .push(Mutation::set_definition(
            DefinitionKind::QUERY,
            "q",
            None,
            Some(query("q")),
        ));
    (captured, builder.build().unwrap())
}

#[test]
fn a_built_change_set_equals_the_captured_one_and_applies_and_replays_like_it() {
    let (captured, built) = captured_and_built();
    assert_eq!(built, captured);

    let mut applied = TraversalGraph::new();
    applied.apply_change_set(&built).unwrap();
    let replayed = TraversalGraph::new().replay(&built).unwrap();
    let mut via_trait = TraversalGraph::new();
    PersistentStorage::replay(&mut via_trait, &built, true).unwrap();
    for graph in [&applied, &replayed, &via_trait] {
        assert_eq!(graph.vertex_count(), 2);
        assert_eq!(graph.edge_count(), 1);
        assert_eq!(graph.get_schema().unwrap(), Some(schema()));
        assert!(
            graph
                .definition(DefinitionKind::QUERY, "q")
                .unwrap()
                .is_some()
        );
        let names = graph
            .traversal()
            .v(None::<()>)
            .has_label("robot")
            .id()
            .to_list()
            .unwrap();
        assert_eq!(names.len(), 1);
    }
    assert_eq!(replayed.last_commit_seq(), built.commit_seq());
    assert_eq!(
        PersistentStorage::last_committed_at(&via_trait),
        built.committed_at()
    );
}

#[cfg(feature = "serde")]
#[test]
fn a_built_change_set_survives_a_serde_round_trip() {
    let (_, built) = captured_and_built();
    let mut builder = ChangeSet::builder(2, 20);
    builder
        .push(Mutation::drop_edge(
            "e",
            EdgeState::new(Some("knows"), "a", "b", PropertyMap::new()),
        ))
        .push(Mutation::set_edge_label("f", None, "likes"))
        .push(Mutation::remove_property(
            ElementRef::edge("f"),
            "w",
            1.5.into(),
        ))
        .push(Mutation::remove_label("b", "robot"))
        .push(Mutation::set_definition(
            DefinitionKind::QUERY,
            "q",
            Some(query("q")),
            None,
        ));
    let other = builder.build().unwrap();
    for changes in [built, other] {
        let text = serde_json::to_string(&changes).unwrap();
        let back: ChangeSet = serde_json::from_str(&text).unwrap();
        assert_eq!(back, changes);
    }
}

#[test]
fn the_builder_refuses_what_no_commit_produces() {
    let refused = |builder: graphersal::changes::ChangeSetBuilder| match builder.build() {
        Err(err @ GraphError::InvalidChangeSet { .. }) => err,
        other => panic!("expected InvalidChangeSet, got {other:?}"),
    };
    let err = refused(ChangeSet::builder(0, 5));
    assert!(err.to_string().contains("never committed"), "{err}");
    assert!(
        err.help()
            .unwrap()
            .contains("Mutation::set_definition(kind, name, before, after)")
    );
    refused(ChangeSet::builder(1, -1));
    let mut builder = ChangeSet::builder(1, 1);
    builder.push(Mutation::set_definition(
        DefinitionKind::QUERY,
        "q",
        None,
        None,
    ));
    let err = refused(builder);
    assert!(err.to_string().contains("mutation 0"), "{err}");
    let mut builder = ChangeSet::builder(1, 1);
    builder.push(Mutation::set_definition(
        DefinitionKind::QUERY,
        "q",
        None,
        Some(query("other")),
    ));
    refused(builder);
    // A never-committed change set (a dry run's) is fine.
    assert!(ChangeSet::builder(0, 0).build().unwrap().is_empty());
}

#[test]
fn raise_position_never_lowers() {
    let mut graph = TraversalGraph::new();
    graph.raise_position(10, 1_000).unwrap();
    assert_eq!(graph.last_commit_seq(), 10);
    graph.raise_position(3, 5).unwrap();
    assert_eq!(graph.last_commit_seq(), 10);
    assert_eq!(PersistentStorage::last_committed_at(&graph), 1_000);
    graph.raise_position(11, 2_000).unwrap();
    assert_eq!(graph.last_commit_seq(), 11);
    assert_eq!(PersistentStorage::last_committed_at(&graph), 2_000);
    graph.raise_id_sequences(4, 2).unwrap();
    assert_eq!(graph.id_sequences(), (4, 2));
    // The next commit continues the numbering.
    add_one(&mut graph).unwrap();
    assert_eq!(graph.last_commit_seq(), 12);
    assert_eq!(graph.id_sequences().0, 5);
}

#[test]
fn the_load_sink_stores_as_is_and_refuses_duplicates_and_missing_endpoints() {
    let mut graph = TraversalGraph::new();
    graph.set_graph_id(0xABC).unwrap();
    assert_eq!(graph.graph_id(), 0xABC);
    graph.load_schema(schema()).unwrap();
    graph.load_definition(query("q")).unwrap();
    graph
        .load_vertex("a".into(), vec!["person".into()], PropertyMap::new())
        .unwrap();
    graph
        .load_vertex("b".into(), vec![], PropertyMap::new())
        .unwrap();
    assert!(matches!(
        graph.load_vertex("a".into(), vec![], PropertyMap::new()),
        Err(GraphError::VertexIdUniqueConstraint { .. })
    ));
    graph
        .load_edge(
            "e".into(),
            Some("knows".into()),
            "a",
            "b",
            PropertyMap::new(),
        )
        .unwrap();
    assert!(matches!(
        graph.load_edge("e".into(), None, "a", "b", PropertyMap::new()),
        Err(GraphError::EdgeIdUniqueConstraint { .. })
    ));
    assert!(matches!(
        graph.load_edge("f".into(), None, "a", "zz", PropertyMap::new()),
        Err(GraphError::VertexNotFound { .. })
    ));
    graph.finish_load().unwrap();
    assert_eq!((graph.vertex_count(), graph.edge_count()), (2, 1));
    assert_eq!(graph.get_schema().unwrap(), Some(schema()));
    assert_eq!(graph.last_commit_seq(), 0, "loading commits nothing");
    assert_eq!(graph.statistics().unwrap().unwrap().vertex_count(), 2);
}

#[test]
fn clear_empties_everything_and_keeps_the_hooks_and_the_journal() {
    let log: Log = Arc::default();
    let mut graph = TraversalGraph::tinkerpop_modern();
    graph
        .register_commit_hook(logging("kept", &log, false))
        .unwrap();
    Journal::new(LoggingWriter(log.clone()), Durability::Os)
        .attach(&mut graph)
        .unwrap();
    graph.set_schema(schema()).unwrap();
    graph.set_definition(query("q")).unwrap();
    let lineage = graph.graph_id();
    graph.clear().unwrap();
    assert_eq!((graph.vertex_count(), graph.edge_count()), (0, 0));
    assert_eq!(graph.get_schema().unwrap(), None);
    assert_eq!(graph.definitions().unwrap().count(), 0);
    assert_eq!(graph.last_commit_seq(), 0);
    assert_eq!(graph.id_sequences(), (0, 0));
    assert_ne!(
        graph.graph_id(),
        lineage,
        "a new lineage id is created on use"
    );
    // The journal slot is still taken ...
    assert!(
        Journal::new(LoggingWriter(log.clone()), Durability::Os)
            .attach(&mut graph)
            .is_err()
    );
    // ... and the user hook still sees the commits.
    log.lock().unwrap().clear();
    add_one(&mut graph).unwrap();
    let events = log.lock().unwrap().clone();
    assert_eq!(events.first().map(String::as_str), Some("kept before"));
    assert!(events.contains(&"journal write".to_string()));
    // A mark goes to the kept journal.
    let mark = graph.mark("after-clear").unwrap();
    assert_eq!(mark.commit_seq(), 1);
}
