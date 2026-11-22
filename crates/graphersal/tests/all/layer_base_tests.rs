//! `LayerBase` constructors and base identity (layered-graph plan S5.1):
//! `LayerBase::from_store_read_only` and `LayerBase::from_snapshot_file` load a base with the
//! embedding models of its database and name it after its location; the identity (`BaseRef`)
//! is equal across two read-only opens of the same commit and a `.gsnap` export of that
//! commit, differs after a commit to the base store, and pins a past commit; a layer store
//! made over one load opens over the others; a host pools bases by `BaseRef`.
#![cfg(all(feature = "persist", not(target_arch = "wasm32")))]

use std::collections::HashMap;
use std::sync::Arc;

use graphersal::persist::{
    ImportModelOptions, MemDir, ModelSource, PersistError, ReadOptions, RecoveryTarget, Store,
    StoreOptions, WriteOptions, write_snapshot,
};
use graphersal::prelude::*;
use graphersal::storage::{BaseRef, LayerBase, LayeredGraph};

use crate::model_fixture::{Temp, tiny_model};

/// A base store in `dir` holding the modern graph and the tiny model `acme/tiny`,
/// checkpointed (its newest snapshot is its last commit), closed. Returns the model identity.
fn base_store(dir: &std::path::Path) -> String {
    let store =
        Store::create_with(dir, TraversalGraph::tinkerpop_modern(), StoreOptions::new()).unwrap();
    let identity = store
        .import_model(
            ModelSource::Files(tiny_model(1, "acme/tiny")),
            ImportModelOptions::new(),
        )
        .unwrap()
        .model
        .identity;
    store.checkpoint(None).unwrap();
    store.close().unwrap();
    identity
}

/// Exports the newest snapshot of the closed store in `dir` as a `.gsnap` at `path`.
fn export(dir: &std::path::Path, path: &std::path::Path) {
    Store::export_packed_dir(dir, None, std::fs::File::create(path).unwrap()).unwrap();
}

/// One more commit to the closed store in `dir` (a vertex `added`).
fn commit_to(dir: &std::path::Path) {
    let store = Store::open(dir, StoreOptions::new()).unwrap();
    GraphTraversalSource::new(&mut *store.graph().write())
        .add_v("added")
        .iterate()
        .unwrap();
    store.close().unwrap();
}

fn latest(dir: &std::path::Path) -> LayerBase {
    LayerBase::from_store_read_only(dir, RecoveryTarget::Latest).unwrap()
}

#[test]
fn a_store_opened_read_only_is_a_named_base_with_its_models() {
    let temp = Temp::new("base-store");
    let dir = temp.join("base");
    let model = base_store(&dir);
    let base = latest(&dir);
    assert_eq!(base.name(), dir.display().to_string());
    assert!(base.serves_models(), "the store's models are carried");
    let layer = base.new_layer().unwrap();
    assert_eq!(layer.read().vertex_count().unwrap(), 6);
    assert_eq!(layer.read().base_name(), dir.display().to_string());
    let has_model = layer
        .read()
        .definitions()
        .unwrap()
        .any(|d| d.name() == model);
    assert!(has_model, "the base's model definition is visible");
}

#[test]
fn a_gsnap_is_a_named_base_with_its_models_and_load_report() {
    let temp = Temp::new("base-gsnap");
    let dir = temp.join("base");
    base_store(&dir);
    let gsnap = temp.join("base.gsnap");
    export(&dir, &gsnap);
    let (base, report) = LayerBase::from_snapshot_file(&gsnap, &ReadOptions::new()).unwrap();
    assert_eq!(base.name(), gsnap.display().to_string());
    assert!(base.serves_models(), "the file's models are carried");
    assert_eq!(report.models.len(), 1, "the load report lists the model");
    assert_eq!(base.new_layer().unwrap().read().vertex_count().unwrap(), 6);
}

/// Coordinator decision D4: two read-only opens of one commit and a `.gsnap` export of that
/// commit are the same base; a commit to the base store is another one; a past target pins
/// the old one.
#[test]
fn the_identity_is_stable_across_loads_of_one_commit() {
    let temp = Temp::new("identity");
    let dir = temp.join("base");
    base_store(&dir);
    let gsnap = temp.join("base.gsnap");
    export(&dir, &gsnap);

    let first = latest(&dir);
    let second = latest(&dir);
    let (from_file, _) = LayerBase::from_snapshot_file(&gsnap, &ReadOptions::new()).unwrap();
    let identity: BaseRef = *first.base_ref().unwrap();
    assert!(!Arc::ptr_eq(first.graph(), second.graph()), "two loads");
    assert_eq!(second.base_ref(), Some(&identity));
    assert_eq!(from_file.base_ref(), Some(&identity));
    assert_eq!(
        from_file.graph().last_commit_seq().unwrap(),
        identity.commit_seq(),
        "the file records the commit"
    );

    // A commit to the base store (in its WAL, after the snapshot) is another base.
    commit_to(&dir);
    let after = latest(&dir);
    let moved = *after.base_ref().unwrap();
    assert_ne!(moved, identity);
    assert_eq!(moved.graph_id(), identity.graph_id(), "the same lineage");
    assert_eq!(moved.commit_seq(), identity.commit_seq() + 1);
    assert_eq!(
        latest(&dir).base_ref(),
        Some(&moved),
        "stable from the WAL too"
    );
    // The old commit stays reachable: a past target and the old export.
    let pinned =
        LayerBase::from_store_read_only(&dir, RecoveryTarget::CommitSeq(identity.commit_seq()))
            .unwrap();
    assert_eq!(pinned.base_ref(), Some(&identity));
    // An export now copies the newest snapshot, still the old commit (the new one is only in
    // the WAL): checkpoint first to export the latest commit.
    let stale = temp.join("stale.gsnap");
    export(&dir, &stale);
    let (stale, _) = LayerBase::from_snapshot_file(&stale, &ReadOptions::new()).unwrap();
    assert_eq!(stale.base_ref(), Some(&identity));
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    store.checkpoint(None).unwrap();
    store.close().unwrap();
    let fresh = temp.join("fresh.gsnap");
    export(&dir, &fresh);
    let (fresh, _) = LayerBase::from_snapshot_file(&fresh, &ReadOptions::new()).unwrap();
    assert_eq!(fresh.base_ref(), Some(&moved));
}

/// A layer store made over a base opened read-only reopens over the `.gsnap` of the same
/// commit, and is refused over the base after a commit.
#[test]
fn a_layer_store_opens_over_any_load_of_its_base() {
    let temp = Temp::new("layer-over-loads");
    let dir = temp.join("base");
    base_store(&dir);
    let gsnap = temp.join("base.gsnap");
    export(&dir, &gsnap);
    let layer_dir = MemDir::new();
    let store = Store::<LayeredGraph>::create_layer(&layer_dir, &latest(&dir), StoreOptions::new())
        .unwrap();
    GraphTraversalSource::new(&mut *store.graph().write())
        .add_v("project")
        .iterate()
        .unwrap();
    store.close().unwrap();

    let (from_file, _) = LayerBase::from_snapshot_file(&gsnap, &ReadOptions::new()).unwrap();
    let store =
        Store::<LayeredGraph>::open_layer(&layer_dir, &from_file, StoreOptions::new()).unwrap();
    assert_eq!(store.graph().read().vertex_count().unwrap(), 7);
    store.close().unwrap();

    commit_to(&dir);
    let err = Store::<LayeredGraph>::open_layer(&layer_dir, &latest(&dir), StoreOptions::new())
        .err()
        .unwrap();
    assert!(
        matches!(err, PersistError::LayerBaseMismatch { .. }),
        "{err:?}"
    );
}

/// A layer is no base: a layer store and a layer `.gsnap` are refused, nothing panics.
#[test]
fn a_layer_is_refused_as_a_base() {
    let temp = Temp::new("layer-refused");
    let base = LayerBase::new(Arc::new(TraversalGraph::tinkerpop_modern()));
    let layer_dir = temp.join("layer");
    Store::<LayeredGraph>::create_layer(&layer_dir, &base, StoreOptions::new())
        .unwrap()
        .close()
        .unwrap();
    assert!(LayerBase::from_store_read_only(&layer_dir, RecoveryTarget::Latest).is_err());

    let layer = base.new_layer().unwrap();
    let gsnap = temp.join("layer.gsnap");
    write_snapshot(
        &*layer.read(),
        &mut std::fs::File::create(&gsnap).unwrap(),
        &WriteOptions::new(),
    )
    .unwrap();
    let err = LayerBase::from_snapshot_file(&gsnap, &ReadOptions::new())
        .err()
        .unwrap();
    assert!(
        matches!(err, PersistError::SnapshotKindMismatch { .. }),
        "{err:?}"
    );
    assert!(
        LayerBase::from_snapshot_file(temp.join("missing.gsnap"), &ReadOptions::new()).is_err()
    );
}

/// A pool keyed by `BaseRef` keeps one base per commit: loads of the same commit share the
/// pooled base; another commit is another entry.
#[test]
fn a_host_pools_bases_by_identity() {
    let temp = Temp::new("pool");
    let dir = temp.join("base");
    base_store(&dir);
    let gsnap = temp.join("base.gsnap");
    export(&dir, &gsnap);
    let mut pool: HashMap<BaseRef, LayerBase> = HashMap::new();
    let mut pooled = |loaded: LayerBase| -> LayerBase {
        pool.entry(*loaded.base_ref().unwrap())
            .or_insert(loaded)
            .clone()
    };
    let a = pooled(latest(&dir));
    let b = pooled(
        LayerBase::from_snapshot_file(&gsnap, &ReadOptions::new())
            .unwrap()
            .0,
    );
    assert!(Arc::ptr_eq(a.graph(), b.graph()));
    commit_to(&dir);
    let c = pooled(latest(&dir));
    assert!(!Arc::ptr_eq(a.graph(), c.graph()));
    assert_eq!(pool.len(), 2);
}
