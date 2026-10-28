//! `GraphStorage::set_edge_label`: changing the single label of an edge (e.g. labeling unlabeled
//! edges before freezing a schema as `closed`). Index and statistics, rollback, change capture
//! and `compact()`, schema checks under the new label, compression rules of the new label, and a
//! journal round trip.

use std::cell::RefCell;

use graphersal::changes::{ChangeSet, Mutation};
use graphersal::prelude::*;
use graphersal::schema::{GraphSchema, SchemaMode};
use graphersal::storage::GraphStatistics;
use graphersal::{
    changes::{CommitHook, HookError, RollbackReason},
    storage::EdgeHandle,
};

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

thread_local! {
    static RECORDED: RefCell<Vec<ChangeSet>> = const { RefCell::new(Vec::new()) };
}

/// Records every change set it accepts (per test thread).
struct Recorder;

impl CommitHook for Recorder {
    fn before_commit(&mut self, changes: &ChangeSet) -> Result<(), HookError> {
        RECORDED.with(|recorded| recorded.borrow_mut().push(changes.clone()));
        Ok(())
    }
}

fn recorded() -> Vec<ChangeSet> {
    RECORDED.with(|recorded| std::mem::take(&mut *recorded.borrow_mut()))
}

fn props(entries: &[(&str, ElementProperty)]) -> PropertyMap {
    let mut out = PropertyMap::default();
    for (key, value) in entries {
        out.insert(*key, value.clone());
    }
    out
}

fn e(graph: &TraversalGraph, id: &str) -> EdgeHandle {
    graph.get_edge_by_id(id).unwrap().expect("edge")
}

fn label_of(graph: &TraversalGraph, id: &str) -> Option<String> {
    graph.edge_label(e(graph, id)).unwrap().map(str::to_string)
}

fn edge_label_count(graph: &TraversalGraph, label: &str) -> usize {
    graph
        .statistics()
        .unwrap()
        .expect("statistics")
        .edge_label_count(label)
}

/// `a(person) -created-> b(software)` with `w: 1`, `a -?-> c(person)` unlabeled.
fn small() -> TraversalGraph {
    let mut graph = TraversalGraph::new();
    let a = graph
        .add_vertex(
            Some("a".into()),
            vec!["person".into()],
            PropertyMap::default(),
        )
        .unwrap();
    let b = graph
        .add_vertex(
            Some("b".into()),
            vec!["software".into()],
            PropertyMap::default(),
        )
        .unwrap();
    let c = graph
        .add_vertex(
            Some("c".into()),
            vec!["person".into()],
            PropertyMap::default(),
        )
        .unwrap();
    graph
        .add_edge(
            Some("ab".into()),
            Some("created".into()),
            a,
            b,
            props(&[("w", 1.into())]),
        )
        .unwrap();
    graph
        .add_edge(Some("ac".into()), None, a, c, PropertyMap::default())
        .unwrap();
    graph
}

// ---------------------------------------------------------------------------------------------
// Basics, rollback
// ---------------------------------------------------------------------------------------------

#[test]
fn relabel_updates_label_index_and_statistics() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    assert_eq!(edge_label_count(&graph, "knows"), 2);
    assert!(
        graph
            .set_edge_label(e(&graph, "0"), "likes".into())
            .unwrap()
    );
    assert!(
        !graph
            .set_edge_label(e(&graph, "0"), "likes".into())
            .unwrap()
    );
    assert_eq!(label_of(&graph, "0").as_deref(), Some("likes"));
    assert_eq!(edge_label_count(&graph, "knows"), 1);
    assert_eq!(edge_label_count(&graph, "likes"), 1);
    assert_eq!(
        graph.statistics().unwrap().cloned(),
        Some(GraphStatistics::from_storage(&graph).unwrap())
    );
    // The traversal sees the new label through the label index.
    let ids = graph
        .traversal()
        .e(None)
        .has_label("likes")
        .id()
        .to_list()
        .unwrap();
    assert_eq!(ids, vec![ElementProperty::from("0")]);
    let knows = graph
        .traversal()
        .v("1")
        .out("knows")
        .count()
        .next()
        .unwrap();
    assert_eq!(knows, Some(ElementProperty::from(1i64)));
}

#[test]
fn relabel_of_a_removed_edge_is_an_error() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let edge = e(&graph, "0");
    graph.drop_edge(edge).unwrap();
    assert!(matches!(
        graph.set_edge_label(edge, "x".into()),
        Err(GraphError::ElementRemoved { .. })
    ));
}

#[test]
fn rollback_restores_the_label_and_the_statistics() {
    let mut graph = small();
    let before = graph.statistics().unwrap().cloned();
    let mark = graph.begin_unit().unwrap();
    assert!(
        graph
            .set_edge_label(e(&graph, "ab"), "made".into())
            .unwrap()
    );
    assert!(
        graph
            .set_edge_label(e(&graph, "ac"), "knows".into())
            .unwrap()
    );
    assert_eq!(edge_label_count(&graph, "made"), 1);
    graph
        .rollback_unit(mark, &RollbackReason::failed(&"test"))
        .unwrap();
    assert_eq!(label_of(&graph, "ab").as_deref(), Some("created"));
    assert_eq!(label_of(&graph, "ac"), None, "unlabeled again");
    assert_eq!(graph.statistics().unwrap().cloned(), before);
    assert_eq!(graph.edges_by_label("made").unwrap().count(), 0);
    assert_eq!(graph.edges_by_label("knows").unwrap().count(), 0);
    assert_eq!(graph.edges_by_label("created").unwrap().count(), 1);
}

// ---------------------------------------------------------------------------------------------
// Change capture
// ---------------------------------------------------------------------------------------------

#[test]
fn relabel_is_captured_with_before_and_after() {
    let mut graph = small();
    graph.register_commit_hook(Box::new(Recorder)).unwrap();
    graph
        .transaction(|g| -> Result<(), GraphError> {
            g.set_edge_label(e(g, "ac"), "knows".into())?;
            g.set_edge_label(e(g, "ab"), "made".into())?;
            // The same label again records nothing.
            g.set_edge_label(e(g, "ab"), "made".into())?;
            Ok(())
        })
        .unwrap();
    let sets = recorded();
    assert_eq!(sets.len(), 1);
    let changes: Vec<(String, Option<String>, String)> = sets[0]
        .mutations()
        .iter()
        .map(|m| match m {
            Mutation::SetEdgeLabel {
                id, before, after, ..
            } => (id.clone(), before.clone(), after.clone()),
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(
        changes,
        vec![
            ("ac".to_string(), None, "knows".to_string()),
            (
                "ab".to_string(),
                Some("created".to_string()),
                "made".to_string()
            ),
        ]
    );

    // Replaying the change set onto the start state reproduces the end state.
    let mut copy = small();
    copy.apply_change_set(&sets[0]).unwrap();
    assert_eq!(label_of(&copy, "ac").as_deref(), Some("knows"));
    assert_eq!(label_of(&copy, "ab").as_deref(), Some("made"));
    assert_eq!(copy.statistics().unwrap(), graph.statistics().unwrap());
}

#[test]
fn compact_merges_and_drops_label_changes() {
    let mut graph = small();
    graph.register_commit_hook(Box::new(Recorder)).unwrap();
    graph
        .transaction(|g| -> Result<(), GraphError> {
            // Changed twice: one change from the first before to the last after.
            g.set_edge_label(e(g, "ac"), "x".into())?;
            g.set_edge_label(e(g, "ac"), "y".into())?;
            // Changed and changed back: nothing.
            g.set_edge_label(e(g, "ab"), "made".into())?;
            g.set_edge_label(e(g, "ab"), "created".into())?;
            // A new edge absorbs its label change.
            let a = g.get_vertex_by_id("a").unwrap().unwrap();
            let b = g.get_vertex_by_id("b").unwrap().unwrap();
            let new = g.add_edge(
                Some("new".into()),
                Some("old".into()),
                a,
                b,
                PropertyMap::default(),
            )?;
            g.set_edge_label(new, "fresh".into())?;
            Ok(())
        })
        .unwrap();
    let compact = recorded().remove(0).compact();
    let summary: Vec<String> = compact
        .mutations()
        .iter()
        .map(|m| match m {
            Mutation::AddEdge { id, label, .. } => format!("add {id} {label:?}"),
            Mutation::SetEdgeLabel {
                id, before, after, ..
            } => format!("label {id} {before:?} -> {after}"),
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            r#"add new Some("fresh")"#.to_string(),
            r#"label ac None -> y"#.to_string(),
        ]
    );
    let mut copy = small();
    copy.apply_change_set(&compact).unwrap();
    assert_eq!(label_of(&copy, "ac").as_deref(), Some("y"));
    assert_eq!(label_of(&copy, "ab").as_deref(), Some("created"));
    assert_eq!(label_of(&copy, "new").as_deref(), Some("fresh"));
}

#[test]
fn a_dropped_relabeled_edge_compacts_to_its_original_image() {
    let mut graph = small();
    graph.register_commit_hook(Box::new(Recorder)).unwrap();
    graph
        .transaction(|g| -> Result<(), GraphError> {
            g.set_edge_label(e(g, "ab"), "made".into())?;
            g.drop_edge(e(g, "ab"))?;
            Ok(())
        })
        .unwrap();
    let compact = recorded().remove(0).compact();
    match compact.mutations() {
        [Mutation::DropEdge { id, before, .. }] => {
            assert_eq!(id, "ab");
            assert_eq!(before.label.as_deref(), Some("created"));
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[cfg(feature = "serde")]
#[test]
fn a_label_change_round_trips_through_serde() {
    let mut graph = small();
    graph.register_commit_hook(Box::new(Recorder)).unwrap();
    graph
        .transaction(|g| -> Result<(), GraphError> {
            g.set_edge_label(e(g, "ac"), "knows".into())?;
            g.set_edge_label(e(g, "ab"), "made".into())?;
            Ok(())
        })
        .unwrap();
    let set = recorded().remove(0);
    let text = serde_json::to_string(&set).unwrap();
    assert!(text.contains(r#"{"op":"set_edge_label","id":"ac","after":"knows"}"#));
    assert!(
        text.contains(r#"{"op":"set_edge_label","id":"ab","before":"created","after":"made"}"#)
    );
    let back: ChangeSet = serde_json::from_str(&text).unwrap();
    assert_eq!(back, set);
}

// ---------------------------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------------------------

const CLOSED: &str = r#"{
  "mode": "closed",
  "vertices": {
    "person": {"schema": {"type": "object"}},
    "software": {"schema": {"type": "object"}}
  },
  "edges": {
    "created": {"connections": [{"from": "person", "to": "software"}],
                "schema": {"type": "object", "properties": {"w": {"type": "integer"}}}},
    "made": {"connections": [{"from": "person", "to": "software"}],
             "schema": {"type": "object", "properties": {"w": {"type": "integer"}}}},
    "knows": {"connections": [{"from": "person", "to": "person"}],
              "schema": {"type": "object", "properties": {"w": {"type": "integer"}}}},
    "rated": {"connections": [{"from": "person", "to": "software"}],
              "schema": {"type": "object",
                         "properties": {"w": {"type": "integer"}, "stars": {"type": "integer"}},
                         "required": ["stars"]}},
    "scored": {"connections": [{"from": "person", "to": "software"}],
               "schema": {"type": "object", "properties": {"w": {"type": "string"}}}},
    "bare": {"schema": {"type": "object"}}
  }
}"#;

/// `small()` without its unlabeled edge, under [`CLOSED`].
fn closed() -> TraversalGraph {
    let mut graph = small();
    graph.drop_edge(e(&graph, "ac")).unwrap();
    graph
        .set_schema(GraphSchema::from_json(CLOSED).unwrap())
        .unwrap();
    graph
}

#[test]
fn closed_schema_rejects_a_label_the_edge_does_not_satisfy() {
    let mut graph = closed();
    let before = graph.statistics().unwrap().cloned();
    let edge = e(&graph, "ab");
    for (label, expected) in [
        ("undeclared", "UnknownEdgeLabel"),
        ("knows", "InvalidEdgeTopology"),
        ("rated", "MissingRequiredProperty"),
        ("scored", "TypeMismatch"),
        ("bare", "UndeclaredProperty"),
    ] {
        let err = graph.set_edge_label(edge, label.into()).unwrap_err();
        let text = format!("{err:?}");
        assert!(text.contains(expected), "{label}: {text}");
        assert_eq!(
            label_of(&graph, "ab").as_deref(),
            Some("created"),
            "{label}"
        );
    }
    assert_eq!(graph.statistics().unwrap().cloned(), before);
    assert!(!graph.set_edge_label(edge, "created".into()).unwrap());
    assert!(graph.set_edge_label(edge, "made".into()).unwrap());
    assert_eq!(edge_label_count(&graph, "made"), 1);
}

#[test]
fn labeling_unlabeled_edges_lets_a_graph_freeze_closed() {
    let mut graph = small();
    let inferred = graph.traversal().infer_schema().unwrap();
    assert!(
        graph
            .set_schema(inferred.clone().with_mode(SchemaMode::Closed))
            .is_err(),
        "an unlabeled edge cannot be frozen closed"
    );
    assert!(
        graph
            .set_edge_label(e(&graph, "ac"), "knows".into())
            .unwrap()
    );
    let inferred = graph.traversal().infer_schema().unwrap();
    graph
        .set_schema(inferred.with_mode(SchemaMode::Closed))
        .unwrap();
    assert!(
        graph
            .get_schema()
            .unwrap()
            .unwrap()
            .edges
            .contains_key("knows")
    );
}

#[test]
fn open_schema_checks_required_keys_and_types_of_the_new_label() {
    let mut graph = small();
    graph
        .set_schema(
            GraphSchema::from_json(
                r#"{"mode": "open", "edges": {"rated": {"schema": {"type": "object",
                    "properties": {"stars": {"type": "number"}}, "required": ["stars"]}}}}"#,
            )
            .unwrap(),
        )
        .unwrap();
    let edge = e(&graph, "ac");
    let err = graph.set_edge_label(edge, "rated".into()).unwrap_err();
    assert!(
        format!("{err:?}").contains("MissingRequiredProperty"),
        "{err:?}"
    );
    // An int64 is not a float64: the stored value is checked, never coerced by a label change.
    graph
        .set_edge_property(edge, "stars".into(), 4.into())
        .unwrap();
    let err = graph.set_edge_label(edge, "rated".into()).unwrap_err();
    assert!(format!("{err:?}").contains("TypeMismatch"), "{err:?}");
    assert_eq!(label_of(&graph, "ac"), None);
    graph
        .set_edge_property(edge, "stars".into(), 4.5.into())
        .unwrap();
    assert!(graph.set_edge_label(edge, "rated".into()).unwrap());
    // Later writes coerce by the new label's declarations.
    graph
        .set_edge_property(edge, "stars".into(), 5.into())
        .unwrap();
    assert_eq!(
        graph
            .edge_properties(edge)
            .unwrap()
            .and_then(|p| p.get("stars")),
        Some(&ElementProperty::from(5.0))
    );
}

// ---------------------------------------------------------------------------------------------
// Compression rules follow the label
// ---------------------------------------------------------------------------------------------

#[test]
fn compression_rules_of_the_new_label_apply() {
    use graphersal::{catalog::CompressionDefinition, storage::ElementKind};
    let mut graph = small();
    let text = "a long note that repeats itself; ".repeat(40);
    graph
        .set_edge_property(e(&graph, "ab"), "notes".into(), text.as_str().into())
        .unwrap();
    graph
        .traversal_mut()
        .define_compression(
            "notes",
            CompressionDefinition::new(ElementKind::Edge, "made", "notes").unwrap(),
        )
        .unwrap();
    let compressed = |g: &TraversalGraph| g.memory_usage().unwrap().unwrap().compressed_values;
    assert_eq!(compressed(&graph), 0);
    let mark = graph.begin_unit().unwrap();
    graph
        .set_edge_label(e(&graph, "ab"), "made".into())
        .unwrap();
    assert_eq!(compressed(&graph), 1);
    graph.check_compression_invariant().unwrap();
    graph
        .rollback_unit(mark, &RollbackReason::failed(&"test"))
        .unwrap();
    assert_eq!(compressed(&graph), 0);
    graph.check_compression_invariant().unwrap();
    graph
        .set_edge_label(e(&graph, "ab"), "made".into())
        .unwrap();
    graph
        .set_edge_label(e(&graph, "ab"), "other".into())
        .unwrap();
    assert_eq!(compressed(&graph), 0);
    graph.check_compression_invariant().unwrap();
    let value = graph.traversal().e("ab").values("notes").next().unwrap();
    assert_eq!(value, Some(ElementProperty::from(text.as_str())));
}

// ---------------------------------------------------------------------------------------------
// Journal round trip (WAL op 11)
// ---------------------------------------------------------------------------------------------

#[cfg(feature = "persist")]
#[test]
fn a_label_change_survives_the_journal() {
    use graphersal::changes::{ChangeSet, Mutation};
    use graphersal::persist::{
        self, Durability, Journal, JournalRecord, RecoveryTarget, SharedBuffer, WriteOptions,
    };
    let mut graph = small();
    let mut snapshot = Vec::new();
    persist::write_snapshot(&graph, &mut snapshot, &WriteOptions::new()).unwrap();
    let wal = SharedBuffer::new();
    Journal::new(wal.clone(), Durability::EveryCommit)
        .attach(&mut graph)
        .unwrap();
    graph
        .transaction(|g| -> Result<(), GraphError> {
            g.set_edge_label(e(g, "ac"), "knows".into())?;
            g.set_edge_label(e(g, "ab"), "made".into())?;
            Ok(())
        })
        .unwrap();
    let journal = wal.contents();
    let records: Vec<ChangeSet> = persist::read_journal(journal.as_slice())
        .unwrap()
        .map(|record| match record.unwrap() {
            JournalRecord::Commit(changes) => changes,
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(records.len(), 1);
    assert!(matches!(
        records[0].mutations(),
        [Mutation::SetEdgeLabel { .. }, Mutation::SetEdgeLabel { .. }]
    ));
    let recovered = persist::recover(
        snapshot.as_slice(),
        [journal.as_slice()],
        RecoveryTarget::Latest,
    )
    .unwrap()
    .graph;
    assert_eq!(label_of(&recovered, "ac").as_deref(), Some("knows"));
    assert_eq!(label_of(&recovered, "ab").as_deref(), Some("made"));
    assert_eq!(recovered.statistics().unwrap(), graph.statistics().unwrap());
    assert_eq!(recovered.last_commit_seq(), graph.last_commit_seq());
}
