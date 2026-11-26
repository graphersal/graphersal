//! The reset of a layer (`LayeredGraph::reset_layer`, `Graph<LayeredGraph>::reset_layer`;
//! round 4 task A4): every change a layer made over its base discarded in one unit, the base
//! untouched. After a reset the layer shows exactly what a new layer over the same base shows
//! (the `state` oracle of `layered_persist_tests`: data in property order, `layer_diff`,
//! origins, statistics, index overlays, schema, catalog), only its position and id sequences
//! are its own; the reset is an ordinary unit (rolled back by a failing unit, a dry run and a
//! vetoing hook; the hooks get its change set, which replays into a fresh layer over the base
//! and applies to a materialized copy of the base); a layer store journals it and a reopen
//! recovers the reset layer. A create with the id and the exact state of a removed base
//! element restores that element.
#![cfg(feature = "persist")]

use std::sync::{Arc, Mutex};

use graphersal::catalog::{Definition, PropertyIndexDefinition};
use graphersal::changes::{ChangeSet, CommitHook, HookError, RollbackReason};
use graphersal::persist::{MemDir, Store, StoreOptions};
use graphersal::prelude::*;
use graphersal::storage::{
    ElementKind, GraphStorage, LayerBase, LayerDiffRequest, LayerOrigin, LayeredGraph,
    PersistentStorage,
};

use crate::layered_persist_tests::{assert_same, base, check, history, state, unit};

fn props(pairs: &[(&str, ElementProperty)]) -> PropertyMap {
    let mut map = PropertyMap::default();
    for (key, value) in pairs {
        map.insert(*key, value.clone());
    }
    map
}

/// The rows of [`state`] that describe what the layer shows (everything but its commit
/// position and id sequences, which a reset keeps).
fn shown(layer: &LayeredGraph) -> Vec<String> {
    state(layer)
        .into_iter()
        .filter(|row| !row.starts_with("position "))
        .collect()
}

/// Asserts `layer` shows exactly what a new layer over its base shows, with an empty diff and
/// consistent index overlays.
#[track_caller]
fn assert_like_new(layer: &LayeredGraph, what: &str) {
    let fresh = LayeredGraph::over(Arc::clone(layer.base())).unwrap();
    assert_eq!(shown(layer), shown(&fresh), "{what}");
    assert!(
        layer
            .layer_diff(&LayerDiffRequest::new())
            .unwrap()
            .is_empty(),
        "{what}: the diff is empty"
    );
    check(layer);
}

/// The layer after the first `units` units of the history.
fn layer_after(units: usize) -> LayeredGraph {
    let mut layer = LayeredGraph::over(base()).unwrap();
    for writes in history().into_iter().take(units) {
        unit(&mut layer, |l| writes(l));
    }
    layer
}

/// A hook keeping every change set it accepted; `veto` refuses every commit instead.
#[derive(Clone, Default)]
struct Keeper {
    kept: Arc<Mutex<Vec<ChangeSet>>>,
    veto: Arc<Mutex<bool>>,
}

impl CommitHook for Keeper {
    fn name(&self) -> &str {
        "keeper"
    }

    fn before_commit(&mut self, changes: &ChangeSet) -> Result<(), HookError> {
        if *self.veto.lock().unwrap() {
            return Err(HookError::from("vetoed by the test"));
        }
        self.kept.lock().unwrap().push(changes.clone());
        Ok(())
    }

    fn after_commit(&mut self, _: &ChangeSet) {}

    fn after_rollback(&mut self, _: &RollbackReason) {}
}

impl Keeper {
    fn on<S: GraphStorage>(storage: &mut S) -> Self {
        let keeper = Keeper::default();
        storage
            .register_commit_hook(Box::new(keeper.clone()))
            .unwrap();
        keeper
    }

    fn kept(&self) -> Vec<ChangeSet> {
        self.kept.lock().unwrap().clone()
    }
}

/// Every element of `storage` as sorted text rows (id, labels, properties in order, endpoints):
/// the data a materialized copy is compared by.
fn data<S: GraphStorage>(storage: &S) -> Vec<String> {
    let mut rows = Vec::new();
    for node in storage.vertices().unwrap() {
        rows.push(format!(
            "v {} {:?} {:?}",
            storage.vertex_id(node).unwrap().unwrap(),
            storage.vertex_labels(node).unwrap(),
            storage
                .vertex_properties(node)
                .unwrap()
                .cloned()
                .unwrap_or_default()
        ));
    }
    for edge in storage.edges().unwrap() {
        let (from, to) = storage.edge_endpoints(edge).unwrap().unwrap();
        rows.push(format!(
            "e {} {:?} {} -> {} {:?}",
            storage.edge_id(edge).unwrap().unwrap(),
            storage.edge_label(edge).unwrap(),
            storage.vertex_id(from).unwrap().unwrap(),
            storage.vertex_id(to).unwrap().unwrap(),
            storage
                .edge_properties(edge)
                .unwrap()
                .cloned()
                .unwrap_or_default()
        ));
    }
    rows.sort();
    rows
}

/// After every prefix of the history (every delta kind: added and cross edges, vertex and edge
/// images with label changes, tombstones with cascade, a return to clean, a layer-defined
/// index, a removed and a new definition, a schema change, a reused base id) a reset gives a
/// layer like a new one, reports what it discarded and commits once.
#[test]
fn a_reset_gives_the_base_back_after_every_prefix_of_the_history() {
    for units in 1..=history().len() {
        let mut layer = layer_after(units);
        let summary = layer.layer_summary().unwrap().unwrap();
        let seq = layer.last_commit_seq().unwrap();
        let sequences = layer.id_sequences();
        let reset = layer.reset_layer().unwrap();
        let what = format!("after {units} units");
        assert_eq!(
            (reset.added, reset.modified, reset.removed),
            (summary.added, summary.modified, summary.removed),
            "{what}"
        );
        assert!(!reset.is_empty(), "{what}");
        assert_like_new(&layer, &what);
        assert_eq!(
            layer.last_commit_seq().unwrap(),
            seq + 1,
            "{what}: one commit"
        );
        assert_eq!(layer.id_sequences(), sequences, "{what}: sequences kept");
        let after = layer.layer_summary().unwrap().unwrap();
        assert_eq!(
            (after.added, after.modified, after.removed),
            Default::default(),
            "{what}"
        );
    }
    // The schema and catalog changes are reported.
    let mut layer = layer_after(history().len());
    let reset = layer.reset_layer().unwrap();
    assert!(reset.schema);
    assert_eq!(reset.definitions, 3, "index name, q1 back, q3 gone");
}

/// A layer without changes: nothing to discard, no commit.
#[test]
fn a_reset_of_an_unchanged_layer_commits_nothing() {
    let mut layer = LayeredGraph::over(base()).unwrap();
    let keeper = Keeper::on(&mut layer);
    assert!(layer.reset_layer().unwrap().is_empty());
    assert_eq!(layer.last_commit_seq().unwrap(), 0);
    assert!(keeper.kept().is_empty());
    assert_like_new(&layer, "unchanged");
}

/// New writes after a reset work as on a new layer; auto ids continue the sequence (never an
/// id handed out before); a handle of an element the reset removed stays stale.
#[test]
fn writes_after_a_reset_work_and_get_new_ids() {
    let mut layer = layer_after(history().len());
    let added = layer.get_vertex_by_id("x1").unwrap().unwrap();
    let (vertex_sequence, edge_sequence) = layer.id_sequences();
    layer.reset_layer().unwrap();
    assert!(!layer.contains_vertex(added).unwrap());
    // The whole history again, on the reset layer and on a new layer whose sequences are
    // raised to the same point: the same layer.
    let mut fresh = LayeredGraph::over(base()).unwrap();
    fresh
        .raise_id_sequences(vertex_sequence, edge_sequence)
        .unwrap();
    for writes in history() {
        unit(&mut layer, |l| writes(l));
        unit(&mut fresh, |l| writes(l));
    }
    assert_eq!(shown(&layer), shown(&fresh));
    check(&layer);
    assert!(!layer.contains_vertex(added).unwrap(), "no alias");
    let auto = layer
        .insert_vertex(None, vec!["person".into()], PropertyMap::default())
        .unwrap();
    let id: u64 = layer.vertex_id(auto).unwrap().unwrap().parse().unwrap();
    assert!(id > vertex_sequence, "{id} after {vertex_sequence}");
}

/// The hooks get the reset's change set; the journal of the whole life (history + reset)
/// replays into a fresh layer over the base giving the same layer, and applies to a
/// materialized copy of the base giving the base's data.
#[test]
fn the_reset_change_set_replays_and_applies() {
    let mut layer = LayeredGraph::over(base()).unwrap();
    let keeper = Keeper::on(&mut layer);
    for writes in history() {
        unit(&mut layer, |l| writes(l));
    }
    layer.reset_layer().unwrap();
    let kept = keeper.kept();
    assert_eq!(kept.len(), history().len() + 1);
    let last = kept.last().unwrap();
    assert_eq!(last.commit_seq(), layer.last_commit_seq().unwrap());
    assert!(!last.mutations().is_empty());

    let mut replayed = LayeredGraph::over(Arc::clone(layer.base())).unwrap();
    for changes in &kept {
        replayed
            .replay(changes, true)
            .unwrap_or_else(|err| panic!("commit {}: {err}", changes.commit_seq()));
    }
    assert_same(&replayed, &layer, "replayed");
    assert_like_new(&replayed, "replayed");

    let mut copy = Arc::try_unwrap(base()).ok().expect("a new base");
    for changes in &kept {
        copy.apply_change_set(changes)
            .unwrap_or_else(|err| panic!("commit {}: {err}", changes.commit_seq()));
    }
    assert_eq!(data(&copy), data(&*base()));
}

/// A reset inside a unit that fails, inside a dry run and under a vetoing hook leaves the layer
/// as it was.
#[test]
fn a_reset_that_does_not_commit_changes_nothing() {
    let mut layer = layer_after(history().len());
    let before = state(&layer);

    let mark = layer.begin_unit().unwrap();
    layer.reset_layer().unwrap();
    assert!(
        layer
            .layer_diff(&LayerDiffRequest::new())
            .unwrap()
            .is_empty()
    );
    layer
        .rollback_unit(mark, &RollbackReason::failed(&"the unit failed"))
        .unwrap();
    assert_eq!(state(&layer), before, "rolled back");
    check(&layer);

    let (reset, changes) = layer
        .dry_run(|l| l.reset_layer())
        .expect("a dry run of a reset");
    assert!(!reset.is_empty());
    assert!(!changes.mutations().is_empty());
    assert_eq!(state(&layer), before, "dry run");
    check(&layer);

    let keeper = Keeper::on(&mut layer);
    *keeper.veto.lock().unwrap() = true;
    let err = layer.reset_layer().expect_err("vetoed");
    assert!(matches!(err, GraphError::CommitRejected { .. }), "{err}");
    assert_eq!(state(&layer), before, "vetoed");
    check(&layer);

    // Inside a transaction the reset is part of the unit: later writes commit with it.
    *keeper.veto.lock().unwrap() = false;
    layer
        .transaction(|l| {
            l.reset_layer()?;
            l.insert_vertex(
                Some("z".into()),
                vec!["person".into()],
                PropertyMap::default(),
            )?;
            Ok::<_, GraphError>(())
        })
        .unwrap();
    let diff = layer.layer_diff(&LayerDiffRequest::new()).unwrap();
    assert_eq!(diff.len(), 1);
    assert_eq!(keeper.kept().len(), 1, "one commit");
}

/// Two base vertices swap the values of a unique index in the layer (each step valid); the
/// reset puts both back, and its change set replays and applies (no intermediate state fails a
/// uniqueness check).
#[test]
fn a_swap_of_unique_values_is_reset_and_replays() {
    let unique_base = || {
        let mut base = TraversalGraph::new();
        base.set_definition(Definition::property_index(
            "email",
            PropertyIndexDefinition::new(ElementKind::Vertex, "person", "email").with_unique(true),
        ))
        .unwrap();
        for (id, email) in [("a", "a@x"), ("b", "b@x")] {
            base.insert_vertex(
                Some(id.into()),
                vec!["person".into()],
                props(&[("email", email.into())]),
            )
            .unwrap();
        }
        base
    };
    let base = Arc::new(unique_base());
    let mut layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
    let keeper = Keeper::on(&mut layer);
    unit(&mut layer, |l| {
        let (a, b) = (
            l.get_vertex_by_id("a")?.unwrap(),
            l.get_vertex_by_id("b")?.unwrap(),
        );
        l.set_vertex_property(a, "email".into(), "tmp@x".into())?;
        l.set_vertex_property(b, "email".into(), "a@x".into())?;
        l.set_vertex_property(a, "email".into(), "b@x".into())
    });
    layer.reset_layer().unwrap();
    assert_like_new(&layer, "swap reset");

    let mut replayed = LayeredGraph::over(Arc::clone(&base)).unwrap();
    let mut copy = unique_base();
    for changes in keeper.kept() {
        replayed.replay(&changes, true).unwrap();
        copy.apply_change_set(&changes).unwrap();
    }
    assert_same(&replayed, &layer, "replayed");
    assert_eq!(data(&copy), data(&*base));
    // The unique index is enforced again.
    let b = layer.get_vertex_by_id("b").unwrap().unwrap();
    assert!(matches!(
        layer.set_vertex_property(b, "email".into(), "a@x".into()),
        Err(GraphError::UniqueViolation { .. })
    ));
}

/// A vertex created with the id and the exact state of a base vertex the layer removed is the
/// base vertex again (origin `base`, its base handle); with another state it is a new element
/// (origin `added`). The same for an edge.
#[test]
fn a_create_equal_to_a_removed_base_element_restores_it() {
    let mut layer = LayeredGraph::over(base()).unwrap();
    let s1 = layer.get_vertex_by_id("s1").unwrap().unwrap();
    let c1 = layer.get_edge_by_id("c1").unwrap().unwrap();
    let state_of_s1 = (
        layer.vertex_labels(s1).unwrap().to_vec(),
        layer
            .vertex_properties(s1)
            .unwrap()
            .cloned()
            .unwrap_or_default(),
    );
    layer.drop_vertex(s1).unwrap();
    let again = layer
        .insert_vertex(
            Some("s1".into()),
            state_of_s1.0.clone(),
            state_of_s1.1.clone(),
        )
        .unwrap();
    assert_eq!(again, s1, "the base handle");
    assert_eq!(layer.vertex_layer(again).unwrap(), Some(LayerOrigin::Base));
    let p3 = layer.get_vertex_by_id("p3").unwrap().unwrap();
    let edge = layer
        .insert_edge(
            Some("c1".into()),
            Some("created".into()),
            p3,
            again,
            PropertyMap::default(),
        )
        .unwrap();
    assert_eq!(edge, c1);
    assert_like_new(&layer, "recreated");

    // Another state: a new element.
    layer.drop_vertex(s1).unwrap();
    let other = layer
        .insert_vertex(
            Some("s1".into()),
            state_of_s1.0,
            props(&[("name", "other".into())]),
        )
        .unwrap();
    assert_eq!(layer.vertex_layer(other).unwrap(), Some(LayerOrigin::Added));
    check(&layer);
}

/// `Graph<LayeredGraph>::reset_layer` on a layer store's live graph: one journaled commit; a
/// reopen (WAL replay) and a reopen after a checkpoint give the reset layer; the store keeps
/// committing afterwards.
#[test]
fn a_layer_store_journals_the_reset_and_reopens_reset() {
    let base = LayerBase::new(base()).with_name("test base");
    let options = || {
        StoreOptions::new()
            .with_checkpoint_after_bytes(0)
            .with_wal_segment_bytes(4096)
    };
    let dir = MemDir::new();
    let store = Store::<LayeredGraph>::create_layer(&dir, &base, options()).unwrap();
    for writes in history() {
        let mut graph = store.graph().write();
        unit(&mut graph, |l| writes(l));
    }
    let reset = store.graph().reset_layer().unwrap();
    assert!(!reset.is_empty());
    let live = state(&store.graph().read());
    assert_like_new(&store.graph().read(), "live");
    drop(store);

    let store = Store::<LayeredGraph>::open_layer(&dir, &base, options()).unwrap();
    assert!(store.open_report().commits_replayed > 0);
    assert_eq!(state(&store.graph().read()), live, "reopened");
    assert_like_new(&store.graph().read(), "reopened");
    store.checkpoint(None).unwrap();
    drop(store);

    let store = Store::<LayeredGraph>::open_layer(&dir, &base, options()).unwrap();
    assert_eq!(state(&store.graph().read()), live, "after a checkpoint");
    let first = history().into_iter().next().unwrap();
    {
        let mut graph = store.graph().write();
        unit(&mut graph, |l| first(l));
    }
    let expected = state(&store.graph().read());
    drop(store);
    let store = Store::<LayeredGraph>::open_layer(&dir, &base, options()).unwrap();
    assert_eq!(state(&store.graph().read()), expected, "continued");
    check(&store.graph().read());
}
