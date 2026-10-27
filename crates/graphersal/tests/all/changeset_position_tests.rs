//! The position of a `ChangeSet` (persistence prep for the write-ahead log): the monotonic
//! commit time, the auto-id sequences after the unit and their restoration by
//! `TraversalGraph::replay` and `GraphStorage::apply_change_set`, and the checked counters
//! (`commit_seq` and both auto-id sequences never wrap; overflow is
//! `GraphError::SequenceOverflow`).

use std::sync::{Arc, Mutex};

use graphersal::changes::ChangeSet;
use graphersal::changes::{CommitHook, HookError, Mutation, RollbackReason};
use graphersal::prelude::*;

// ---------------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------------

/// A journal: every change set it accepted, and the rollbacks it was told about.
#[derive(Clone, Default)]
struct Journal {
    accepted: Arc<Mutex<Vec<ChangeSet>>>,
    rollbacks: Arc<Mutex<Vec<String>>>,
}

impl CommitHook for Journal {
    fn before_commit(&mut self, changes: &ChangeSet) -> Result<(), HookError> {
        self.accepted.lock().unwrap().push(changes.clone());
        Ok(())
    }

    fn after_rollback(&mut self, reason: &RollbackReason) {
        self.rollbacks
            .lock()
            .unwrap()
            .push(reason.message().to_string());
    }
}

impl Journal {
    fn attach(graph: &mut TraversalGraph) -> Journal {
        let journal = Journal::default();
        graph
            .register_commit_hook(Box::new(journal.clone()))
            .unwrap();
        journal
    }

    fn change_sets(&self) -> Vec<ChangeSet> {
        self.accepted.lock().unwrap().clone()
    }

    fn rollbacks(&self) -> Vec<String> {
        self.rollbacks.lock().unwrap().clone()
    }
}

fn now_micros() -> i64 {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    i64::try_from(elapsed.as_micros()).unwrap()
}

fn vertex_ids(graph: &TraversalGraph) -> Vec<String> {
    let mut ids: Vec<String> = graph
        .traversal()
        .v(None::<()>)
        .id()
        .to_list()
        .unwrap()
        .into_iter()
        .map(|id| match id {
            ElementProperty::String(id) => id.to_string(),
            other => panic!("unexpected id {other:?}"),
        })
        .collect();
    ids.sort();
    ids
}

fn add_vertex(graph: &mut TraversalGraph, label: &str) -> String {
    let ids = graph.traversal_mut().add_v(label).id().to_list().unwrap();
    match &ids[..] {
        [ElementProperty::String(id)] => id.to_string(),
        other => panic!("unexpected ids {other:?}"),
    }
}

/// The overflow of `sequence`, wherever the error came from.
fn assert_overflow(error: &TraverserError, sequence: &str) {
    match error.root_cause() {
        TraverserError::Graph(GraphError::SequenceOverflow { sequence: got, .. }) => {
            assert_eq!(*got, sequence);
        }
        other => panic!("expected SequenceOverflow({sequence}), got {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------------
// Commit time
// ---------------------------------------------------------------------------------------------

#[test]
fn every_commit_gets_a_monotonic_time_also_without_hooks() {
    let mut graph = TraversalGraph::new();
    assert_eq!(graph.last_committed_at(), 0, "no commit yet");
    let before = now_micros();
    add_vertex(&mut graph, "a");
    let first = graph.last_committed_at();
    assert!(first >= before && first <= now_micros(), "{first} is now");

    let journal = Journal::attach(&mut graph);
    add_vertex(&mut graph, "b");
    add_vertex(&mut graph, "c");
    let times: Vec<i64> = journal
        .change_sets()
        .iter()
        .map(ChangeSet::committed_at)
        .collect();
    assert_eq!(times.len(), 2);
    assert!(first <= times[0] && times[0] <= times[1]);
    assert_eq!(graph.last_committed_at(), times[1]);
}

#[test]
fn a_unit_that_commits_nothing_takes_no_time() {
    let mut graph = TraversalGraph::new();
    add_vertex(&mut graph, "a");
    let time = graph.last_committed_at();
    graph.traversal_mut().v(None::<()>).count().next().unwrap();
    let failed = graph
        .traversal_mut()
        .add_v("n")
        .constant("x")
        .as_number(GType::LONG)
        .to_list();
    assert!(failed.is_err());
    assert_eq!(graph.last_committed_at(), time);
}

#[test]
fn a_dry_run_change_set_has_no_commit_time_but_the_sequences() {
    let mut graph = TraversalGraph::new();
    add_vertex(&mut graph, "a");
    let time = graph.last_committed_at();
    let (_, changes) = graph
        .dry_run(|g| g.traversal_mut().add_v("b").to_list())
        .unwrap();
    assert_eq!(changes.commit_seq(), 0);
    assert_eq!(changes.committed_at(), 0, "never committed");
    assert_eq!(changes.node_sequence(), 2, "the dry run handed out id 2");
    assert_eq!(graph.last_committed_at(), time);
    // The id stays used: the next vertex gets 3.
    assert_eq!(add_vertex(&mut graph, "c"), "3");
}

#[cfg(feature = "serde")]
#[test]
fn a_clock_behind_the_last_commit_repeats_its_time() {
    // A journal written by a clock far in the future (or a clock that stepped back since):
    // replaying it moves the graph's clock forward, and the next commit is not earlier.
    let future = now_micros() + 3_600_000_000;
    let text = format!(
        r#"{{"commit_seq":1,"committed_at":{future},"mutations":[{{"op":"add_vertex","id":"x","labels":[],"properties":{{}}}}]}}"#
    );
    let changes: ChangeSet = serde_json::from_str(&text).unwrap();
    let mut graph = TraversalGraph::new().replay(&changes).unwrap();
    assert_eq!(graph.last_committed_at(), future);
    let journal = Journal::attach(&mut graph);
    add_vertex(&mut graph, "a");
    assert_eq!(journal.change_sets()[0].committed_at(), future);
    assert_eq!(journal.change_sets()[0].commit_seq(), 2);
}

// ---------------------------------------------------------------------------------------------
// Auto-id sequences
// ---------------------------------------------------------------------------------------------

#[test]
fn every_change_set_carries_the_sequences_after_the_unit() {
    let mut graph = TraversalGraph::new();
    let journal = Journal::attach(&mut graph);
    let mark = graph.begin_unit().unwrap();
    let a = graph
        .add_vertex(None, vec![], PropertyMap::default())
        .unwrap();
    let b = graph
        .add_vertex(None, vec![], PropertyMap::default())
        .unwrap();
    graph
        .add_edge(None, Some("knows".into()), a, b, PropertyMap::default())
        .unwrap();
    graph.commit_unit(mark).unwrap();
    // An explicit numeric id raises the sequence; a property write moves nothing.
    let mark = graph.begin_unit().unwrap();
    graph
        .add_vertex(Some("40".into()), vec![], PropertyMap::default())
        .unwrap();
    graph.commit_unit(mark).unwrap();
    let mark = graph.begin_unit().unwrap();
    graph
        .set_vertex_property(a, "k".into(), ElementProperty::from(1i64))
        .unwrap();
    graph.commit_unit(mark).unwrap();
    let sequences: Vec<(u64, u64)> = journal
        .change_sets()
        .iter()
        .map(|set| (set.node_sequence(), set.edge_sequence()))
        .collect();
    assert_eq!(sequences, [(2, 1), (40, 1), (40, 1)]);
}

#[test]
fn replay_never_reuses_an_id_of_a_dropped_or_rolled_back_element() {
    let mut live = TraversalGraph::new();
    let journal = Journal::attach(&mut live);
    // 1: committed, auto id.
    assert_eq!(add_vertex(&mut live, "kept"), "1");
    assert_eq!(add_vertex(&mut live, "doomed"), "2");
    // 3: handed out by a unit that is rolled back (no change set at all).
    let failed = live
        .traversal_mut()
        .add_v("rolled_back")
        .constant("x")
        .as_number(GType::LONG)
        .to_list();
    assert!(failed.is_err());
    // An edge whose id is handed out and then dropped.
    let mark = live.begin_unit().unwrap();
    let (one, two) = (
        live.get_vertex_by_id("1").unwrap(),
        live.get_vertex_by_id("2").unwrap(),
    );
    live.add_edge(None, Some("e".into()), one, two, PropertyMap::default())
        .unwrap();
    live.commit_unit(mark).unwrap();
    // 2 and its edge: committed and dropped.
    live.traversal_mut().v("2").drop().to_list().unwrap();
    assert_eq!(vertex_ids(&live), ["1"]);

    let mut recovered = TraversalGraph::new();
    for changes in journal.change_sets() {
        recovered = recovered.replay(&changes).unwrap();
    }
    assert_eq!(vertex_ids(&recovered), ["1"]);
    assert_eq!(recovered.last_commit_seq(), live.last_commit_seq());
    assert_eq!(recovered.last_committed_at(), live.last_committed_at());
    // Neither 2 (dropped) nor 3 (rolled back) comes back; the same holds for the edge id.
    assert_eq!(add_vertex(&mut recovered, "new"), "4");
    assert_eq!(add_vertex(&mut live, "new"), "4");
    let mark = recovered.begin_unit().unwrap();
    let (one, four) = (
        recovered.get_vertex_by_id("1").unwrap(),
        recovered.get_vertex_by_id("4").unwrap(),
    );
    let edge = recovered
        .add_edge(None, Some("e".into()), one, four, PropertyMap::default())
        .unwrap();
    recovered.commit_unit(mark).unwrap();
    assert_eq!(recovered.edge_id(edge).unwrap(), "2");
}

/// The vertex and edge ids a change set mentions in its add mutations.
fn added_ids(changes: &ChangeSet) -> Vec<String> {
    changes
        .mutations()
        .iter()
        .filter_map(|m| match m {
            Mutation::AddVertex { id, .. } | Mutation::AddEdge { id, .. } => Some(id.to_string()),
            _ => None,
        })
        .collect()
}

#[test]
fn apply_never_reuses_an_id_whose_add_and_drop_were_compacted_away() {
    let mut source = TraversalGraph::new();
    let journal = Journal::attach(&mut source);
    let mark = source.begin_unit().unwrap();
    let one = source
        .add_vertex(None, vec![], PropertyMap::default())
        .unwrap();
    let two = source
        .add_vertex(None, vec![], PropertyMap::default())
        .unwrap();
    let three = source
        .add_vertex(None, vec![], PropertyMap::default())
        .unwrap();
    source
        .add_edge(None, Some("e".into()), one, two, PropertyMap::default())
        .unwrap();
    source
        .add_edge(None, Some("e".into()), two, three, PropertyMap::default())
        .unwrap();
    source.drop_vertex(two).unwrap();
    source.drop_vertex(three).unwrap();
    source.commit_unit(mark).unwrap();
    let changes = journal.change_sets().remove(0).compact();
    assert_eq!(
        added_ids(&changes),
        ["1"],
        "2, 3 and both edges compacted away"
    );
    assert_eq!((changes.node_sequence(), changes.edge_sequence()), (3, 2));

    let mut target = TraversalGraph::new();
    let target_journal = Journal::attach(&mut target);
    target.apply_change_set(&changes).unwrap();
    // The applied unit's own change set carries the raised sequences, so a journal of the
    // target keeps them across a recovery.
    let applied = target_journal.change_sets().remove(0);
    assert_eq!((applied.node_sequence(), applied.edge_sequence()), (3, 2));
    assert_eq!(add_vertex(&mut target, "new"), "4");
    let mark = target.begin_unit().unwrap();
    let (one, four) = (
        target.get_vertex_by_id("1").unwrap(),
        target.get_vertex_by_id("4").unwrap(),
    );
    let edge = target
        .add_edge(None, Some("e".into()), one, four, PropertyMap::default())
        .unwrap();
    target.commit_unit(mark).unwrap();
    assert_eq!(target.edge_id(edge).unwrap(), "3");
}

#[test]
fn apply_never_reuses_an_id_of_a_unit_the_source_rolled_back() {
    let mut source = TraversalGraph::new();
    let journal = Journal::attach(&mut source);
    // Ids 1 and 2 are handed out by a unit that is rolled back: no change set at all.
    let failed = source
        .traversal_mut()
        .add_v("a")
        .add_v("b")
        .constant("x")
        .as_number(GType::LONG)
        .to_list();
    assert!(failed.is_err());
    let mark = source.begin_unit().unwrap();
    source
        .add_vertex(Some("x".into()), vec![], PropertyMap::default())
        .unwrap();
    source.commit_unit(mark).unwrap();
    let changes = journal.change_sets().remove(0);
    assert_eq!(added_ids(&changes), ["x"]);
    assert_eq!(changes.node_sequence(), 2);

    let mut target = TraversalGraph::new();
    target.apply_change_set(&changes).unwrap();
    assert_eq!(add_vertex(&mut target, "new"), "3");
}

/// A change set whose every mutation was compacted away: a vertex and an edge added and
/// dropped again in one unit of the source (sequences 2 and 1).
pub(crate) fn compacted_away_change_set() -> ChangeSet {
    let mut source = TraversalGraph::new();
    let journal = Journal::attach(&mut source);
    let mark = source.begin_unit().unwrap();
    let one = source
        .add_vertex(None, vec![], PropertyMap::default())
        .unwrap();
    let two = source
        .add_vertex(None, vec![], PropertyMap::default())
        .unwrap();
    source
        .add_edge(None, Some("e".into()), one, two, PropertyMap::default())
        .unwrap();
    source.drop_vertex(one).unwrap();
    source.drop_vertex(two).unwrap();
    source.commit_unit(mark).unwrap();
    let changes = journal.change_sets().remove(0).compact();
    assert!(changes.is_empty(), "every mutation compacted away");
    assert_eq!((changes.node_sequence(), changes.edge_sequence()), (2, 1));
    changes
}

#[test]
fn applying_a_compacted_away_change_set_is_a_sequence_only_commit() {
    let changes = compacted_away_change_set();
    let mut target = TraversalGraph::new();
    let journal = Journal::attach(&mut target);
    target.apply_change_set(&changes).unwrap();
    let committed = journal.change_sets();
    assert_eq!(committed.len(), 1, "one sequence-only commit");
    assert!(committed[0].is_empty(), "it carries no mutations");
    assert_eq!(committed[0].commit_seq(), 1);
    assert_eq!(target.last_commit_seq(), 1);
    assert!(committed[0].committed_at() > 0);
    assert_eq!(
        (committed[0].node_sequence(), committed[0].edge_sequence()),
        (2, 1)
    );
    assert!(
        target
            .statistics()
            .unwrap()
            .is_some_and(|s| s.vertex_count() == 0)
    );
    // Replaying it into a fresh graph (a recovery) keeps the raise.
    let replayed = TraversalGraph::new().replay(&committed[0]).unwrap();
    assert_eq!((replayed.node_sequence(), replayed.edge_sequence()), (2, 1));
    assert_eq!(replayed.last_commit_seq(), 1);

    // Applying it again raises nothing: no commit at all.
    target.apply_change_set(&changes).unwrap();
    assert_eq!(journal.change_sets().len(), 1);
    assert_eq!(target.last_commit_seq(), 1);
    assert_eq!(add_vertex(&mut target, "new"), "3");
}

#[test]
fn a_sequence_only_commit_is_counted_also_without_hooks() {
    let changes = compacted_away_change_set();
    let mut target = TraversalGraph::new();
    target.apply_change_set(&changes).unwrap();
    assert_eq!(target.last_commit_seq(), 1);
    assert!(target.last_committed_at() > 0);
}

#[test]
fn a_vetoed_sequence_only_commit_rolls_back_and_keeps_the_raise_in_memory() {
    struct Veto;
    impl CommitHook for Veto {
        fn name(&self) -> &str {
            "veto"
        }
        fn before_commit(&mut self, _: &ChangeSet) -> Result<(), HookError> {
            Err("no".into())
        }
    }
    let changes = compacted_away_change_set();
    let mut target = TraversalGraph::new();
    let journal = Journal::attach(&mut target);
    target.register_commit_hook(Box::new(Veto)).unwrap();
    let error = target.apply_change_set(&changes).unwrap_err();
    assert!(
        matches!(error, GraphError::CommitRejected { ref hook, .. } if hook == "veto"),
        "{error:?}"
    );
    assert_eq!(target.last_commit_seq(), 0, "nothing committed");
    assert_eq!(
        journal.change_sets().len(),
        1,
        "the journal saw the attempt"
    );
    assert_eq!(journal.rollbacks(), ["no"]);
    // Sequences are never rolled back: no id of the source is handed out here.
    assert_eq!((target.node_sequence(), target.edge_sequence()), (2, 1));
}

#[test]
fn a_rolled_back_raise_commits_nothing_and_stays_in_memory() {
    let mut target = TraversalGraph::new();
    let journal = Journal::attach(&mut target);
    let mark = target.begin_unit().unwrap();
    target.raise_id_sequences(5, 7).unwrap();
    target
        .rollback_unit(mark, &RollbackReason::failed(&"stop"))
        .unwrap();
    assert!(journal.change_sets().is_empty());
    assert_eq!(journal.rollbacks(), ["stop"]);
    assert_eq!(target.last_commit_seq(), 0);
    assert_eq!((target.node_sequence(), target.edge_sequence()), (5, 7));
    // A later unit that changes nothing commits nothing (the raise belonged to the old unit).
    let mark = target.begin_unit().unwrap();
    target.commit_unit(mark).unwrap();
    assert!(journal.change_sets().is_empty());
}

#[test]
fn apply_never_lowers_a_sequence() {
    let mut source = TraversalGraph::new();
    let journal = Journal::attach(&mut source);
    add_vertex(&mut source, "a");
    let changes = journal.change_sets().remove(0);
    let mut target = TraversalGraph::new();
    let mark = target.begin_unit().unwrap();
    target
        .add_vertex(Some("90".into()), vec![], PropertyMap::default())
        .unwrap();
    target.commit_unit(mark).unwrap();
    target.apply_change_set(&changes).unwrap();
    assert_eq!(add_vertex(&mut target, "b"), "91");
}

#[test]
fn replay_never_lowers_a_sequence() {
    let mut graph = TraversalGraph::new();
    let journal = Journal::attach(&mut graph);
    add_vertex(&mut graph, "a");
    let changes = journal.change_sets().remove(0);
    assert_eq!(changes.node_sequence(), 1);
    let mut target = TraversalGraph::new();
    let mark = target.begin_unit().unwrap();
    target
        .add_vertex(Some("90".into()), vec![], PropertyMap::default())
        .unwrap();
    target.commit_unit(mark).unwrap();
    let mut target = target.replay(&changes).unwrap();
    assert_eq!(add_vertex(&mut target, "b"), "91");
}

// ---------------------------------------------------------------------------------------------
// Checked counters
// ---------------------------------------------------------------------------------------------

#[test]
fn the_commit_sequence_never_wraps() {
    let mut graph = TraversalGraph::new().with_last_commit_seq(u64::MAX);
    let journal = Journal::attach(&mut graph);
    let error = graph.traversal_mut().add_v("a").to_list().unwrap_err();
    assert_overflow(&error, "commit sequence number");
    assert!(error.to_string().contains("exhausted"), "{error}");
    // The unit was rolled back: nothing is left, no hook saw a commit, every hook was told.
    assert_eq!(graph.vertex_count(), 0);
    assert!(journal.change_sets().is_empty());
    assert_eq!(journal.rollbacks().len(), 1);
    assert_eq!(graph.last_commit_seq(), u64::MAX);
    assert_eq!(graph.last_committed_at(), 0);
    // Reads still work.
    assert_eq!(
        graph.traversal().v(None::<()>).count().next().unwrap(),
        Some(ElementProperty::from(0i64))
    );
}

#[test]
fn the_commit_sequence_reaches_its_maximum_once() {
    let mut graph = TraversalGraph::new().with_last_commit_seq(u64::MAX - 1);
    add_vertex(&mut graph, "a");
    assert_eq!(graph.last_commit_seq(), u64::MAX);
    assert!(graph.traversal_mut().add_v("b").to_list().is_err());
    assert_eq!(graph.vertex_count(), 1);
}

#[test]
fn the_vertex_id_sequence_never_wraps() {
    let mut graph = TraversalGraph::new();
    let mark = graph.begin_unit().unwrap();
    graph
        .add_vertex(
            Some(u64::MAX.to_string().into()),
            vec![],
            PropertyMap::default(),
        )
        .unwrap();
    graph.commit_unit(mark).unwrap();
    let error = graph.traversal_mut().add_v("a").to_list().unwrap_err();
    assert_overflow(&error, "vertex auto-id sequence");
    assert_eq!(graph.vertex_count(), 1);
    // Explicit ids still work.
    let mark = graph.begin_unit().unwrap();
    graph
        .add_vertex(Some("p-1".into()), vec![], PropertyMap::default())
        .unwrap();
    graph.commit_unit(mark).unwrap();
    assert_eq!(graph.vertex_count(), 2);
}

#[test]
fn the_edge_id_sequence_never_wraps() {
    let mut graph = TraversalGraph::new();
    let mark = graph.begin_unit().unwrap();
    let a = graph
        .add_vertex(None, vec![], PropertyMap::default())
        .unwrap();
    graph
        .add_edge(
            Some(u64::MAX.to_string().into()),
            Some("e".into()),
            a,
            a,
            PropertyMap::default(),
        )
        .unwrap();
    graph.commit_unit(mark).unwrap();
    let mark = graph.begin_unit().unwrap();
    let error = graph
        .add_edge(None, Some("e".into()), a, a, PropertyMap::default())
        .unwrap_err();
    graph
        .rollback_unit(mark, &RollbackReason::failed(&error))
        .unwrap();
    match error {
        GraphError::SequenceOverflow { sequence, .. } => {
            assert_eq!(sequence, "edge auto-id sequence")
        }
        other => panic!("expected SequenceOverflow, got {other:?}"),
    }
    assert_eq!(graph.edge_count(), 1);
}

#[cfg(feature = "serde")]
#[test]
fn a_replayed_sequence_at_its_maximum_fails_the_next_auto_id_cleanly() {
    let text = format!(
        r#"{{"commit_seq":1,"node_sequence":{max},"edge_sequence":{max},"mutations":[{{"op":"add_vertex","id":"x","labels":[],"properties":{{}}}}]}}"#,
        max = u64::MAX
    );
    let changes: ChangeSet = serde_json::from_str(&text).unwrap();
    let mut graph = TraversalGraph::new().replay(&changes).unwrap();
    let error = graph.traversal_mut().add_v("a").to_list().unwrap_err();
    assert_overflow(&error, "vertex auto-id sequence");
    assert_eq!(graph.vertex_count(), 1);
}

// ---------------------------------------------------------------------------------------------
// Serialization
// ---------------------------------------------------------------------------------------------

#[cfg(feature = "serde")]
#[test]
fn the_position_round_trips_through_json_and_defaults_to_zero() {
    let mut graph = TraversalGraph::new();
    let journal = Journal::attach(&mut graph);
    add_vertex(&mut graph, "a");
    let changes = journal.change_sets().remove(0);
    let json = serde_json::to_value(&changes).unwrap();
    assert_eq!(json["commit_seq"], 1);
    assert_eq!(json["committed_at"], changes.committed_at());
    assert_eq!(json["node_sequence"], 1);
    assert_eq!(json["edge_sequence"], 0);
    let back: ChangeSet = serde_json::from_value(json).unwrap();
    assert_eq!(back, changes);
    assert_eq!(back.compact().committed_at(), changes.committed_at());

    let bare: ChangeSet = serde_json::from_str(r#"{"commit_seq":3,"mutations":[]}"#).unwrap();
    assert_eq!(
        (
            bare.committed_at(),
            bare.node_sequence(),
            bare.edge_sequence()
        ),
        (0, 0, 0)
    );
}
