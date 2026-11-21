//! Layer stores (layered-graph plan S3.3, format spec 7.5): `Store::create_layer` /
//! `create_layer_with` / `open_layer` keep one project layer over a frozen base durable and
//! recoverable on every backend (a directory, one file, memory). A reopen, a recovery after a
//! simulated crash (a crash image after every commit, every cut of the last record) and a
//! checkpoint give the committed layer exactly (the `state` oracle of `layered_persist_tests`:
//! data in property order, `layer_diff`, origins, statistics, index overlays, schema, catalog,
//! position, id sequences). The store records only the base's identity: another base is
//! refused naming both, a plain open of a layer store and a layer open of a plain store are
//! clear refusals (never damage), a layer manifest (file format version 5) is accepted only
//! in a layer store, and the operations not supported for layer stores yet refuse with
//! `StoreFailure::LayerStore`. The base may be a Store opened read-only (its files stay
//! unchanged) or a `.gsnap`; its embedding models are served by the layer store's graph.
//!
//! S3.4a (rule 22: the base is a creation parameter): backups (full, increment, closed, ZIP,
//! from memory) and their restores, the read-only open of a backup, the exported snapshot,
//! verification (with and without the base), the damage report, read-only views and the closed
//! checkpoint carry the base and give the layer back over it only; backups warn that they are
//! useless without the base; the materialized export is a plain format-4 snapshot of the whole
//! visible graph.
#![cfg(feature = "persist")]

use std::collections::BTreeMap;
use std::sync::Arc;

use graphersal::persist::{
    MemDir, PersistError, RecoveryTarget, RollbackMode, Store, StoreDir, StoreFailure,
    StoreOptions, WriteOptions, write_snapshot,
};
use graphersal::prelude::*;
use graphersal::storage::{BaseRef, LayerBase, LayeredGraph, PersistentStorage};

use crate::layered_persist_tests::{assert_same, base, check, history, state, unit};

type LayerStore = Store<LayeredGraph>;

fn options() -> StoreOptions {
    StoreOptions::new()
        .with_checkpoint_after_bytes(0)
        .with_wal_segment_bytes(4096)
}

/// The base of the history in `layered_persist_tests`, with its identity.
fn layer_base() -> LayerBase {
    LayerBase::new(base()).with_name("test base")
}

/// The first `units` units of the history run on a fresh in-memory layer over `base`: the
/// oracle of what a layer store must hold after committing them.
fn oracle(base: &LayerBase, units: usize) -> LayeredGraph {
    let mut layer = LayeredGraph::over_base(base).unwrap();
    for writes in history().into_iter().take(units) {
        unit(&mut layer, |l| writes(l));
    }
    layer
}

/// Runs history units `from..to` on the layer store's graph, one commit each.
fn commit_units(store: &LayerStore, from: usize, to: usize) {
    let history = history();
    for writes in &history[from..to] {
        let mut graph = store.graph().write();
        unit(&mut graph, |l| writes(l));
    }
}

/// A unique directory under the system temp dir, removed on drop.
struct Temp(std::path::PathBuf);

impl Temp {
    fn new(name: &str) -> Self {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "graphersal-layer-store-{}-{}-{name}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn join(&self, part: &str) -> std::path::PathBuf {
        self.0.join(part)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The three backends, each a fresh empty location.
fn backends(temp: &Temp) -> Vec<(&'static str, Arc<dyn StoreDir>)> {
    let mut out: Vec<(&'static str, Arc<dyn StoreDir>)> =
        vec![("memory", Arc::new(MemDir::named("memory:layer")))];
    #[cfg(not(target_arch = "wasm32"))]
    {
        use graphersal::persist::{FsDir, SingleFileDir};
        out.push(("directory", Arc::new(FsDir::new(temp.join("dir")))));
        out.push((
            "single file",
            Arc::new(SingleFileDir::new(temp.join("layer.gstore"))),
        ));
    }
    let _ = temp;
    out
}

/// Every file of a `MemDir` with its bytes.
fn mem_files(dir: &MemDir) -> BTreeMap<String, Vec<u8>> {
    dir.file_names()
        .into_iter()
        .map(|name| {
            let bytes = dir.file(&name).unwrap();
            (name, bytes)
        })
        .collect()
}

#[track_caller]
fn assert_layer_store_refused<T: std::fmt::Debug>(result: Result<T, PersistError>, what: &str) {
    match result {
        Err(err @ PersistError::Store { .. }) => {
            let PersistError::Store { cause, .. } = &err else {
                unreachable!()
            };
            assert_eq!(*cause, StoreFailure::LayerStore, "{what}: {err}");
            assert!(
                err.to_string().contains("layer store"),
                "{what}: the message says why: {err}"
            );
        }
        other => panic!("{what}: expected a LayerStore refusal, got {other:?}"),
    }
}

/// Create, commit, close, reopen over the same base on every backend: the layer as
/// committed; then more commits and a second reopen.
#[test]
fn a_layer_store_reopens_as_committed_on_every_backend() {
    let temp = Temp::new("backends");
    let base = layer_base();
    let all = history().len();
    let half = all / 2;
    for (name, dir) in backends(&temp) {
        let store = LayerStore::create_layer(&dir, &base, options()).unwrap();
        commit_units(&store, 0, half);
        let live = state(&store.graph().read());
        assert_same(&store.graph().read(), &oracle(&base, half), name);
        store.close().unwrap();

        let store = LayerStore::open_layer(&dir, &base, options())
            .unwrap_or_else(|err| panic!("{name}: {err}"));
        assert_eq!(state(&store.graph().read()), live, "{name}: reopened");
        check(&store.graph().read());
        assert!(store.open_report().commits_replayed > 0, "{name}");
        assert_eq!(store.layer_base().unwrap(), *base.base_ref().unwrap());
        commit_units(&store, half, all);
        drop(store);

        let store = LayerStore::open_layer(&dir, &base, options()).unwrap();
        assert_same(&store.graph().read(), &oracle(&base, all), name);
        check(&store.graph().read());
        store.close().unwrap();
    }
}

/// A layer built in memory first becomes the store's first snapshot (`create_layer_with`);
/// a layer over another base is refused before anything is written.
#[test]
fn create_layer_with_writes_the_given_layer_over_its_own_base_only() {
    let base = layer_base();
    let layer = oracle(&base, history().len());
    let expected = state(&layer);
    let dir = MemDir::new();
    let store = LayerStore::create_layer_with(&dir, &base, layer, options()).unwrap();
    assert_eq!(state(&store.graph().read()), expected);
    drop(store);
    let store = LayerStore::open_layer(&dir, &base, options()).unwrap();
    assert_eq!(state(&store.graph().read()), expected);
    assert_eq!(store.open_report().commits_replayed, 0);
    drop(store);

    let other = layer_base();
    let empty = MemDir::new();
    let err = LayerStore::create_layer_with(&empty, &base, oracle(&other, 1), options())
        .expect_err("another base");
    assert!(
        matches!(err, PersistError::LayerBaseMismatch { .. }),
        "{err}"
    );
    assert!(empty.file_names().is_empty(), "nothing written");

    // `create_in` with a layer would reopen over `LayeredGraph::default()`: refused.
    let refused = MemDir::new();
    assert_layer_store_refused(
        Store::<LayeredGraph>::create_in(&refused, oracle(&base, 1), options()),
        "create_in",
    );
    assert!(refused.file_names().is_empty(), "nothing written");
}

/// Kill after every commit (a crash image of the files, durability per commit): each image
/// recovers exactly the layer committed so far; a recovered image keeps committing.
#[test]
fn a_crash_after_every_commit_recovers_the_committed_layer() {
    let base = layer_base();
    let dir = MemDir::new();
    let store = LayerStore::create_layer(&dir, &base, options()).unwrap();
    let mut images = vec![(0, dir.deep_copy())];
    let all = history().len();
    for done in 1..=all {
        commit_units(&store, done - 1, done);
        if done == all / 2 {
            store.checkpoint(None).unwrap();
            images.push((done, dir.deep_copy()));
        }
        images.push((done, dir.deep_copy()));
    }
    drop(store);
    for (done, image) in images {
        let store = LayerStore::open_layer(&image, &base, options())
            .unwrap_or_else(|err| panic!("after {done} commits: {err}"));
        assert!(!store.open_report().clean_close, "a crash image");
        assert_same(
            &store.graph().read(),
            &oracle(&base, done),
            &format!("crash after {done} commits"),
        );
        check(&store.graph().read());
        if done < all {
            commit_units(&store, done, done + 1);
            drop(store);
            let store = LayerStore::open_layer(&image, &base, options()).unwrap();
            assert_same(
                &store.graph().read(),
                &oracle(&base, done + 1),
                &format!("continued after {done} commits"),
            );
        }
    }
}

/// Every cut of the last WAL record (an interrupted write): the torn tail is cut and the layer
/// is the one before that commit.
#[test]
fn every_cut_of_the_last_record_recovers_the_layer_before_it() {
    let base = layer_base();
    let dir = MemDir::new();
    let store =
        LayerStore::create_layer(&dir, &base, options().with_wal_segment_bytes(1 << 20)).unwrap();
    let all = history().len();
    commit_units(&store, 0, all - 1);
    let wal = dir
        .file_names()
        .into_iter()
        .rfind(|n| n.starts_with("wal/"))
        .unwrap();
    let complete = dir.file(&wal).unwrap().len();
    commit_units(&store, all - 1, all);
    let crashed = dir.deep_copy();
    drop(store);
    let full = crashed.file(&wal).unwrap().len();
    assert!(full > complete);
    let before = state(&oracle(&base, all - 1));
    // Every byte near both ends of the record, and a stride through its middle.
    let cuts: Vec<usize> = (complete..full)
        .filter(|cut| *cut < complete + 80 || *cut + 80 > full || (cut - complete) % 37 == 0)
        .collect();
    for cut in cuts {
        let attempt = crashed.deep_copy();
        let mut bytes = attempt.file(&wal).unwrap();
        bytes.truncate(cut);
        attempt.put_file(&wal, bytes);
        let store = LayerStore::open_layer(&attempt, &base, options())
            .unwrap_or_else(|err| panic!("cut at {cut}: {err}"));
        assert_eq!(state(&store.graph().read()), before, "cut at {cut}");
        assert_eq!(
            store.open_report().torn_tail.is_some(),
            cut > complete,
            "cut at {cut}"
        );
        assert_eq!(attempt.file(&wal).unwrap().len(), complete, "cut at {cut}");
    }
}

/// A checkpoint keeps the layer a layer: the snapshot is a layer snapshot over the base (store
/// info names it), the store reopens equal without a replay, and the snapshot stays the size
/// of the delta, far below the base's.
#[test]
fn a_checkpoint_keeps_the_layer_a_layer_and_delta_sized() {
    let base = layer_base();
    let dir = MemDir::new();
    let store = LayerStore::create_layer(&dir, &base, options()).unwrap();
    commit_units(&store, 0, history().len());
    let live = state(&store.graph().read());
    store.checkpoint(Some("after-history")).unwrap();
    let info = store.info().unwrap();
    assert_eq!(info.layer_base, Some(*base.base_ref().unwrap()));
    assert_eq!(info.snapshots.len(), 2);
    assert!(info.models.is_empty());
    drop(store);
    let store = LayerStore::open_layer(&dir, &base, options()).unwrap();
    assert_eq!(store.open_report().commits_replayed, 0);
    assert_eq!(state(&store.graph().read()), live);
    check(&store.graph().read());
    store.close().unwrap();

    // Delta-sized: a large base, a small layer.
    let mut big = TraversalGraph::new();
    for i in 0..10_000i64 {
        GraphTraversalSource::new(&mut big)
            .add_v("item")
            .property("n", i)
            .property(
                "text",
                format!("item {i} of the base, code {}", i * 7919 % 10_007),
            )
            .iterate()
            .unwrap();
    }
    let big_base = LayerBase::new(Arc::new(big));
    let plain = MemDir::new();
    let mut whole = Vec::new();
    write_snapshot(&**big_base.graph(), &mut whole, &WriteOptions::new()).unwrap();
    Store::create_from_packed(&plain, whole.as_slice(), options()).unwrap();
    let base_bytes = Store::inspect(&plain).unwrap().snapshot_bytes;
    let layer_dir = MemDir::new();
    let store = LayerStore::create_layer(&layer_dir, &big_base, options()).unwrap();
    {
        let mut graph = store.graph().write();
        GraphTraversalSource::new(&mut *graph)
            .add_v("item")
            .property("n", -1i64)
            .iterate()
            .unwrap();
        GraphTraversalSource::new(&mut *graph)
            .v(None::<()>)
            .has("n", 7i64)
            .property("n", 70i64)
            .iterate()
            .unwrap();
        GraphTraversalSource::new(&mut *graph)
            .v(None::<()>)
            .has("n", 9i64)
            .drop()
            .iterate()
            .unwrap();
    }
    let newest = store.checkpoint(None).unwrap().commit_seq;
    let prefix = format!("snapshots/{newest:020}/");
    let layer_bytes: usize = layer_dir
        .file_names()
        .iter()
        .filter(|n| n.starts_with(&prefix))
        .map(|n| layer_dir.file(n).unwrap().len())
        .sum();
    assert!(layer_bytes > 0, "{prefix}: {:?}", layer_dir.file_names());
    assert!(
        (layer_bytes as u64) * 10 < base_bytes,
        "layer snapshot {layer_bytes} bytes vs base {base_bytes} bytes"
    );
    drop(store);
    let store = LayerStore::open_layer(&layer_dir, &big_base, options()).unwrap();
    assert_eq!(store.graph().read().vertex_count().unwrap(), 10_000);
}

/// An automatic checkpoint (past the WAL threshold) encodes the layer too.
#[test]
fn an_automatic_checkpoint_keeps_the_layer_a_layer() {
    let base = layer_base();
    let dir = MemDir::new();
    let opts = options().with_checkpoint_after_bytes(1);
    let store = LayerStore::create_layer(&dir, &base, opts.clone()).unwrap();
    commit_units(&store, 0, history().len());
    let live = state(&store.graph().read());
    // On a platform with threads the checkpoint thread may have run already: either way the
    // newest snapshot is at the last commit after this call.
    store.run_due_checkpoint().unwrap();
    store.checkpoint(None).unwrap();
    assert!(store.last_auto_checkpoint_error().unwrap().is_none());
    let at = store.graph().read().last_commit_seq().unwrap();
    assert_eq!(
        store.info().unwrap().snapshots.last().unwrap().commit_seq,
        at
    );
    drop(store);
    let store = LayerStore::open_layer(&dir, &base, options()).unwrap();
    assert_eq!(state(&store.graph().read()), live);
}

/// Another base is refused before the lock and before any file changes, naming both bases
/// and saying how to fix it.
#[test]
fn another_base_is_refused_naming_both() {
    let base = layer_base();
    let dir = MemDir::new();
    let store = LayerStore::create_layer(&dir, &base, options()).unwrap();
    commit_units(&store, 0, 2);
    store.close().unwrap();
    let before = mem_files(&dir);
    let other = layer_base();
    let err = LayerStore::open_layer(&dir, &other, options()).expect_err("another base");
    let PersistError::LayerBaseMismatch {
        expected, given, ..
    } = &err
    else {
        panic!("expected LayerBaseMismatch, got {err:?}");
    };
    assert_eq!(expected, base.base_ref().unwrap());
    assert_eq!(given, other.base_ref().unwrap());
    let text = err.to_string();
    assert!(
        text.contains(&base.base_ref().unwrap().to_string()),
        "{text}"
    );
    assert!(
        text.contains(&other.base_ref().unwrap().to_string()),
        "{text}"
    );
    assert!(text.contains("rebuild"), "{text}");
    assert_eq!(mem_files(&dir), before, "nothing changed");
    // The right base still opens it.
    LayerStore::open_layer(&dir, &base, options()).unwrap();
}

/// A plain open of a layer store says it is a layer store and needs its base (not damage, no
/// maintenance mode); a layer open of a plain store says it is not a layer store.
#[test]
fn the_kinds_of_store_and_open_must_agree() {
    let base = layer_base();
    let dir = MemDir::new();
    LayerStore::create_layer(&dir, &base, options())
        .unwrap()
        .close()
        .unwrap();
    let before = mem_files(&dir);
    let err = Store::open(&dir, options()).expect_err("a layer store");
    assert!(
        matches!(
            &err,
            PersistError::Store {
                cause: StoreFailure::LayerStore,
                ..
            }
        ),
        "{err:?}"
    );
    let text = err.to_string();
    assert!(text.contains("Store::open_layer"), "{text}");
    assert!(
        text.contains(&base.base_ref().unwrap().to_string()),
        "{text}"
    );
    let help = err.help().unwrap();
    assert!(help.contains("open_layer"), "{help}");
    assert_eq!(mem_files(&dir), before, "nothing changed");
    // `open_in` with a layer factory is refused the same way (it would load over an empty
    // base).
    assert_layer_store_refused(
        Store::<LayeredGraph>::open_in(&dir, options(), LayeredGraph::default),
        "open_in",
    );

    let plain = MemDir::new();
    Store::create(&plain, options()).unwrap().close().unwrap();
    let err = LayerStore::open_layer(&plain, &base, options()).expect_err("a plain store");
    assert!(
        matches!(&err, PersistError::SnapshotKindMismatch { reason, .. } if reason.contains("not a layer store")),
        "{err:?}"
    );
}

/// A version-5 manifest (a layer snapshot's) is accepted only in a layer store: copied into a
/// plain store it makes the plain open refuse; the listings read it either way.
#[test]
fn a_layer_manifest_is_accepted_only_in_a_layer_store() {
    let base = layer_base();
    let layer = MemDir::new();
    let store = LayerStore::create_layer(&layer, &base, options()).unwrap();
    commit_units(&store, 0, 1);
    store.checkpoint(None).unwrap();
    let at = store.graph().read().last_commit_seq().unwrap();
    store.close().unwrap();
    let snapshot = format!("snapshots/{at:020}/");
    let copied: Vec<String> = layer
        .file_names()
        .into_iter()
        .filter(|n| n.starts_with(&snapshot))
        .collect();
    assert!(copied.iter().any(|n| n.ends_with("manifest")), "{copied:?}");

    let plain = MemDir::new();
    Store::create(&plain, options()).unwrap().close().unwrap();
    for name in &copied {
        plain.put_file(name, layer.file(name).unwrap());
    }
    assert_layer_store_refused(
        Store::open(&plain, options()),
        "plain store with a layer manifest",
    );
    // Opened as a layer, it holds whole-graph snapshots too: refused.
    assert!(matches!(
        LayerStore::open_layer(&plain, &base, options()),
        Err(PersistError::SnapshotKindMismatch { .. })
    ));
    // The layer store itself opens, and `inspect` reads both (the plain store's newest
    // snapshot is the copied layer snapshot now).
    LayerStore::open_layer(&layer, &base, options()).unwrap();
    assert_eq!(
        Store::inspect(&layer).unwrap().layer_base,
        Some(*base.base_ref().unwrap())
    );
    assert!(Store::inspect(&plain).is_ok());
    let fresh = MemDir::new();
    Store::create(&fresh, options()).unwrap().close().unwrap();
    assert_eq!(Store::inspect(&fresh).unwrap().layer_base, None);
}

/// What is not supported for layer stores yet refuses with `StoreFailure::LayerStore` (fork,
/// rollback, attic, repair, convert: S3.4b); what works on files only works.
#[test]
fn unsupported_operations_refuse_a_layer_store() {
    let base = layer_base();
    let dir = MemDir::new();
    let store = LayerStore::create_layer(&dir, &base, options()).unwrap();
    commit_units(&store, 0, 3);
    store.mark("m").unwrap();
    commit_units(&store, 3, 4);
    store.checkpoint(None).unwrap();

    // The open store.
    assert_layer_store_refused(store.fork(RecoveryTarget::Latest, MemDir::new()), "fork");
    assert_layer_store_refused(
        store.rollback_to(RecoveryTarget::Mark("m".into()), RollbackMode::Move),
        "rollback",
    );
    assert_layer_store_refused(store.repair_to(MemDir::new()), "repair");
    // File-level operations work.
    store.prune(0).unwrap();
    assert!(store.changes_since(0).is_ok());
    assert_eq!(store.marks().unwrap().len(), 1);
    assert!(store.compaction_advice().is_ok());
    store.close().unwrap();

    // The closed store.
    assert_layer_store_refused(
        Store::fork_dir(&dir, RecoveryTarget::Latest, MemDir::new()),
        "fork_dir",
    );
    // A plain closed checkpoint, view or backup open would load over an empty base: refused
    // (the layer forms take the base).
    assert_layer_store_refused(
        Store::checkpoint_dir(&dir, options(), None),
        "checkpoint_dir",
    );
    assert_layer_store_refused(
        Store::open_read_only(&dir, RecoveryTarget::Latest),
        "open_read_only",
    );
    assert_layer_store_refused(Store::open_backup(&dir, options()), "open_backup");
    assert_layer_store_refused(Store::convert(&dir, MemDir::new()), "convert");
    // Listings work and say what the store is.
    assert_eq!(Store::snapshots_dir(&dir).unwrap().len(), 2);
    assert_eq!(Store::marks_dir(&dir).unwrap().len(), 1);
    assert!(Store::models_dir(&dir).unwrap().is_empty());
    assert!(Store::compaction_advice_dir(&dir, Default::default()).is_ok());
    assert!(Store::prune_estimate_dir(&dir, 0).is_ok());
    let info = Store::inspect(&dir).unwrap();
    assert_eq!(info.layer_base, Some(*base.base_ref().unwrap()));
}

/// A `.gsnap` of a layer does not become a plain store, and leaves nothing behind.
#[test]
fn a_layer_gsnap_does_not_become_a_plain_store() {
    let base = layer_base();
    let layer = oracle(&base, history().len());
    let mut packed = Vec::new();
    write_snapshot(&layer, &mut packed, &WriteOptions::new()).unwrap();
    let dir = MemDir::new();
    assert_layer_store_refused(
        Store::create_from_packed(&dir, packed.as_slice(), options()),
        "create_from_packed",
    );
    assert!(dir.file_names().is_empty(), "{:?}", dir.file_names());
}

/// Damage refuses the open with the damage (no maintenance mode for layer stores yet).
#[test]
fn a_damaged_layer_store_refuses_the_open_with_the_damage() {
    let base = layer_base();
    let dir = MemDir::new();
    let store = LayerStore::create_layer(&dir, &base, options()).unwrap();
    commit_units(&store, 0, 3);
    store.close().unwrap();
    let wal = dir
        .file_names()
        .into_iter()
        .find(|n| n.starts_with("wal/"))
        .unwrap();
    let mut bytes = dir.file(&wal).unwrap();
    let at = 64 + (bytes.len() - 64) / 2;
    bytes[at] ^= 0x10;
    dir.put_file(&wal, bytes);
    let before = mem_files(&dir);
    let err = LayerStore::open_layer(&dir, &base, options()).expect_err("damage");
    assert!(matches!(err, PersistError::Corrupt { .. }), "{err:?}");
    assert_eq!(mem_files(&dir), before, "nothing written");
}

/// The base is a Store opened read-only (its files stay unchanged), with an embedding model in
/// its catalog: the layer store records only the base's identity, so it reopens over the
/// base opened again, and over a `.gsnap` of the same commit; the layer graph serves the base's
/// models (the store keeps none, and its open does not look for them in its own files).
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_layer_store_over_a_store_base_opened_read_only() {
    use graphersal::persist::{ImportModelOptions, ModelSource, read_snapshot_file};

    let temp = Temp::new("store-base");
    let base_dir = temp.join("base");
    let base_graph = Arc::try_unwrap(base()).unwrap_or_else(|_| unreachable!());
    let base_store = Store::create_with(&base_dir, base_graph, options()).unwrap();
    let identity = base_store
        .import_model(
            ModelSource::Files(crate::model_fixture::tiny_model(1, "acme/tiny")),
            ImportModelOptions::new(),
        )
        .unwrap()
        .model
        .identity;
    // A `.gsnap` of the base's last commit (export writes the newest snapshot).
    base_store.checkpoint(None).unwrap();
    let gsnap = temp.join("base.gsnap");
    base_store
        .export_packed(None, std::fs::File::create(&gsnap).unwrap())
        .unwrap();
    base_store.close().unwrap();
    let files_of = |root: &std::path::Path| -> BTreeMap<String, Vec<u8>> {
        let mut out = BTreeMap::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(at) = stack.pop() {
            for entry in std::fs::read_dir(&at).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    let name = path.strip_prefix(root).unwrap().display().to_string();
                    out.insert(name, std::fs::read(&path).unwrap());
                }
            }
        }
        out
    };
    let base_files = files_of(&base_dir);

    let open_base = || {
        let view = Store::open_read_only(&base_dir, RecoveryTarget::Latest).unwrap();
        LayerBase::from_graph(view.into_graph()).unwrap()
    };
    let base = open_base();
    assert!(base.serves_models());
    let layer_dir = temp.join("layer");
    let store = LayerStore::create_layer(&layer_dir, &base, options()).unwrap();
    commit_units(&store, 0, history().len());
    // The layer's schema needs the base's model: a plain store would refuse to open without
    // its blob in its own files; the layer store's models are the base's.
    {
        let mut graph = store.graph().write();
        let schema = GraphSchema::from_json(&format!(
            r#"{{"mode": "open", "vertices": {{"note": {{"schema": {{"type": "object", "properties": {{"vec": {{"type": "array", "format": "vector", "dimension": 3, "embedder": "{identity}"}}}}}}}}}}}}"#
        ))
        .unwrap();
        unit(&mut graph, |l| l.set_schema(schema));
    }
    let live = state(&store.graph().read());
    store.close().unwrap();

    // The base opened again: another instance, the same identity.
    let again = open_base();
    assert_eq!(again.base_ref(), base.base_ref());
    let store = LayerStore::open_layer(&layer_dir, &again, options()).unwrap();
    assert_eq!(state(&store.graph().read()), live);
    let model = store
        .graph()
        .read()
        .definitions()
        .unwrap()
        .any(|d| d.name() == identity);
    assert!(
        model,
        "the base's model definition is in the layer's catalog"
    );
    let info = store.info().unwrap();
    assert!(info.models.is_empty(), "the store keeps no model");
    assert_eq!(info.layer_base, Some(*base.base_ref().unwrap()));
    store.close().unwrap();

    // Over a `.gsnap` of the base's commit.
    let (graph, _) = read_snapshot_file(&gsnap, &Default::default()).unwrap();
    let from_file = LayerBase::from_graph(graph).unwrap();
    assert_eq!(from_file.base_ref(), base.base_ref());
    let store = LayerStore::open_layer(&layer_dir, &from_file, options()).unwrap();
    assert_eq!(state(&store.graph().read()), live);
    drop(store);

    assert_eq!(
        files_of(&base_dir),
        base_files,
        "the base store's files are unchanged"
    );
}

/// The identity a layer store records is the base's `BaseRef` (lineage, commit, time).
#[test]
fn the_store_records_the_bases_identity() {
    let base = layer_base();
    let identity: BaseRef = *base.base_ref().unwrap();
    let dir = MemDir::new();
    let store = LayerStore::create_layer(&dir, &base, options()).unwrap();
    assert_eq!(store.layer_base().unwrap(), identity);
    assert_eq!(store.info().unwrap().layer_base, Some(identity));
    assert_eq!(
        identity.commit_seq(),
        TraversalGraph::last_commit_seq(base.graph()),
        "the base's commit"
    );
    // The layer has its own lineage, recorded in GRAPH.
    assert_eq!(
        store.info().unwrap().graph_id,
        PersistentStorage::graph_id(&*store.graph().read())
    );
    assert_ne!(store.info().unwrap().graph_id, identity.graph_id());
}

// ---- S3.4a: copy paths, verification, views, materialized export (rule 22: `base_ref`) ----

/// A layer store with the whole history committed, a mark in the middle and a checkpoint (two
/// snapshots, a WAL), closed. Returns the store's state and its commit position at the mark.
fn closed_layer_store(dir: &MemDir, base: &LayerBase) -> (Vec<String>, u64) {
    let all = history().len();
    let store = LayerStore::create_layer(dir, base, options()).unwrap();
    commit_units(&store, 0, all / 2);
    store.checkpoint(None).unwrap();
    store.mark("half").unwrap();
    let at_mark = store.graph().read().last_commit_seq().unwrap();
    commit_units(&store, all / 2, all);
    let live = state(&store.graph().read());
    store.close().unwrap();
    (live, at_mark)
}

/// The base identity a store's files name ([`Store::inspect`]).
fn base_of(dir: impl graphersal::persist::IntoStoreDir) -> Option<BaseRef> {
    Store::inspect(dir).unwrap().layer_base
}

/// The state of the layer store in `dir` reopened over `base`, and the refusals of another base
/// and of a plain open.
#[track_caller]
fn assert_copy_is_the_layer(dir: &MemDir, base: &LayerBase, expected: &[String], what: &str) {
    assert_eq!(base_of(dir), base.base_ref().copied(), "{what}: base_ref");
    let other = layer_base();
    let err = LayerStore::open_layer(dir, &other, options()).expect_err(what);
    assert!(
        matches!(err, PersistError::LayerBaseMismatch { .. }),
        "{what}: another base: {err:?}"
    );
    assert_layer_store_refused(Store::open(dir, options()), what);
    let store =
        LayerStore::open_layer(dir, base, options()).unwrap_or_else(|e| panic!("{what}: {e}"));
    assert_eq!(state(&store.graph().read()), expected, "{what}: the layer");
    check(&store.graph().read());
    store.close().unwrap();
}

/// Every backup of a layer store (full and incremental from the open store, `backup_dir` of the
/// closed one) is a layer store over the same base: it opens read-only over that base only
/// (`open_layer_backup`), a restore makes it the live layer store, which opens over that base
/// only, holding the backed-up layer. Each report names the base and warns that the backup is
/// useless without it.
#[test]
fn backups_of_a_layer_store_carry_its_base_and_warn() {
    let base = layer_base();
    let identity = *base.base_ref().unwrap();
    let dir = MemDir::new();
    let store = LayerStore::create_layer(&dir, &base, options()).unwrap();
    commit_units(&store, 0, 3);
    let backup = MemDir::new();
    let report = store.backup(&backup).unwrap();
    assert!(!report.incremental);
    assert_eq!(report.layer_base, Some(identity));
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("useless without") && w.contains(&identity.to_string())),
        "{:?}",
        report.warnings
    );
    assert!(report.models.is_empty());
    commit_units(&store, 3, history().len());
    let live = state(&store.graph().read());
    let report = store.backup(&backup).unwrap();
    assert!(report.incremental, "an increment continues it");
    assert_eq!(report.layer_base, Some(identity));
    assert!(report.warnings.iter().any(|w| w.contains("layer store")));
    store.close().unwrap();

    // The backup opened read-only over its base.
    let opened = LayerStore::open_layer_backup(&backup, &base, options()).unwrap();
    assert_eq!(state(&opened.graph().read()), live);
    assert!(opened.backup_marker().is_some());
    {
        let mut graph = opened.graph().write();
        let refused = GraphTraversalSource::new(&mut *graph)
            .add_v("person")
            .iterate();
        assert!(refused.is_err(), "a backup takes no commit");
    }
    drop(opened);
    assert!(matches!(
        LayerStore::open_layer_backup(&backup, &layer_base(), options()),
        Err(PersistError::LayerBaseMismatch { .. })
    ));
    assert_layer_store_refused(Store::open_backup(&backup, options()), "open_backup");
    // A plain store's backup is no layer backup.
    let plain = MemDir::new();
    let plain_store = Store::create(&plain, options()).unwrap();
    let plain_backup = MemDir::new();
    let plain_report = plain_store.backup(&plain_backup).unwrap();
    assert_eq!(plain_report.layer_base, None);
    assert!(
        plain_report.warnings.is_empty(),
        "{:?}",
        plain_report.warnings
    );
    drop(plain_store);
    assert!(matches!(
        LayerStore::open_layer_backup(&plain_backup, &base, options()),
        Err(PersistError::SnapshotKindMismatch { .. })
    ));

    // `backup_dir` of the closed store.
    let by_dir = MemDir::new();
    let report = Store::backup_dir(&dir, &by_dir).unwrap();
    assert_eq!(report.layer_base, Some(identity));
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("useless without"))
    );
    assert_eq!(
        Store::backup_marker_dir(&by_dir)
            .unwrap()
            .unwrap()
            .commit_seq,
        report.commit_seq
    );

    // Restored in place, each is the live layer store over the same base.
    for (what, copy) in [("backup", &backup), ("backup_dir", &by_dir)] {
        Store::restore_backup(copy).unwrap();
        assert_copy_is_the_layer(copy, &base, &live, what);
    }
}

/// ZIP backups of a layer store (open and closed) restore into a layer store over the same
/// base; their reports warn.
#[cfg(feature = "persist-zip")]
#[test]
fn zip_backups_of_a_layer_store_restore_over_its_base() {
    let base = layer_base();
    let identity = *base.base_ref().unwrap();
    let dir = MemDir::new();
    let store = LayerStore::create_layer(&dir, &base, options()).unwrap();
    commit_units(&store, 0, history().len());
    let live = state(&store.graph().read());
    let mut zip = std::io::Cursor::new(Vec::new());
    let report = store.backup_zip(&mut zip).unwrap();
    assert_eq!(report.layer_base, Some(identity));
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("useless without"))
    );
    store.close().unwrap();
    let mut closed = std::io::Cursor::new(Vec::new());
    let report = Store::backup_zip_dir(&dir, &mut closed).unwrap();
    assert_eq!(report.layer_base, Some(identity));
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("useless without"))
    );
    for (what, archive) in [("zip", zip), ("zip_dir", closed)] {
        let restored = MemDir::new();
        Store::restore_zip(archive.get_ref().as_slice(), &restored).unwrap();
        assert!(
            Store::backup_marker_dir(&restored).unwrap().is_none(),
            "{what}: live"
        );
        assert_copy_is_the_layer(&restored, &base, &live, what);
    }
}

/// A backup from memory of a layer store whose files are damaged writes the layer in memory as
/// a layer snapshot: restored, it is the layer over the same base.
#[test]
fn a_backup_from_memory_of_a_layer_store_is_a_layer_store() {
    let base = layer_base();
    let identity = *base.base_ref().unwrap();
    let dir = MemDir::new();
    let store = LayerStore::create_layer(&dir, &base, options()).unwrap();
    commit_units(&store, 0, history().len());
    store.checkpoint(None).unwrap();
    let live = state(&store.graph().read());
    // Damage the first snapshot's manifest: `verify` finds it (the store turns read-only).
    let manifest = dir
        .file_names()
        .into_iter()
        .find(|n| n.starts_with("snapshots/") && n.ends_with("/manifest"))
        .unwrap();
    let mut bytes = dir.file(&manifest).unwrap();
    let at = bytes.len() / 2;
    bytes[at] ^= 0x20;
    dir.put_file(&manifest, bytes);
    let verified = store.verify().unwrap();
    assert!(!verified.is_ok(), "the damage is found");
    assert!(!store.damage_found().unwrap().is_empty());
    let saved = MemDir::new();
    let report = store
        .backup_from_memory(&saved, graphersal::persist::BackupMode::Auto)
        .unwrap();
    assert!(report.from_memory);
    assert_eq!(report.layer_base, Some(identity));
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("useless without"))
    );
    drop(store);
    Store::restore_backup(&saved).unwrap();
    assert_copy_is_the_layer(&saved, &base, &live, "backup from memory");
}

/// The stored snapshot of a layer store exported as a `.gsnap` (open store and closed) is a
/// layer snapshot: it names the base, loads over it only (`read_layer_snapshot`) as the layer,
/// and a plain read refuses it.
#[test]
fn an_exported_layer_snapshot_loads_over_its_base_only() {
    use graphersal::persist::{
        ReadOptions, read_layer_snapshot, read_snapshot, read_snapshot_info,
    };

    let base = layer_base();
    let identity = *base.base_ref().unwrap();
    let dir = MemDir::new();
    let store = LayerStore::create_layer(&dir, &base, options()).unwrap();
    commit_units(&store, 0, history().len());
    store.checkpoint(None).unwrap();
    let live = state(&store.graph().read());
    let mut open_export = Vec::new();
    let info = store.export_packed(None, &mut open_export).unwrap();
    assert_eq!(info.layer_base, Some(identity));
    store.close().unwrap();
    let mut closed_export = Vec::new();
    let info = Store::export_packed_dir(&dir, None, &mut closed_export).unwrap();
    assert_eq!(info.layer_base, Some(identity));
    for (what, bytes) in [
        ("export_packed", open_export),
        ("export_packed_dir", closed_export),
    ] {
        assert_eq!(
            read_snapshot_info(bytes.as_slice()).unwrap().layer_base,
            Some(identity),
            "{what}"
        );
        assert_eq!(
            u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
            5,
            "{what}: v5"
        );
        let loaded = read_layer_snapshot(bytes.as_slice(), &ReadOptions::new(), &base).unwrap();
        assert_eq!(state(&loaded.read()), live, "{what}");
        assert!(matches!(
            read_layer_snapshot(bytes.as_slice(), &ReadOptions::new(), &layer_base()),
            Err(PersistError::LayerBaseMismatch { .. })
        ));
        assert!(matches!(
            read_snapshot(bytes.as_slice(), &ReadOptions::new()),
            Err(PersistError::SnapshotKindMismatch { .. })
        ));
    }
}

/// `verify` of an open layer store and `verify_layer_dir` load every snapshot over the base;
/// `verify_dir` checks a layer store without its base (and says so). Damage of the layer file is
/// found by every form; another base is refused.
#[test]
fn a_layer_store_verifies_with_and_without_its_base() {
    let base = layer_base();
    let dir = MemDir::new();
    let store = LayerStore::create_layer(&dir, &base, options()).unwrap();
    commit_units(&store, 0, history().len());
    store.checkpoint(None).unwrap();
    let report = store.verify().unwrap();
    assert!(report.is_ok(), "{:?}", report.problems);
    assert_eq!(report.snapshots_checked, 2);
    assert!(!report.notes.iter().any(|n| n.contains("without its base")));
    store.close().unwrap();

    let report = Store::verify_layer_dir(&dir, &base).unwrap();
    assert!(report.is_ok(), "{:?}", report.problems);
    assert_eq!(report.snapshots_checked, 2);
    let report = Store::verify_dir(&dir).unwrap();
    assert!(report.is_ok(), "{:?}", report.problems);
    assert_eq!(report.snapshots_checked, 2);
    assert!(
        report
            .notes
            .iter()
            .any(|n| n.contains("without its base") && n.contains("verify_layer_dir")),
        "{:?}",
        report.notes
    );
    assert!(matches!(
        Store::verify_layer_dir(&dir, &layer_base()),
        Err(PersistError::LayerBaseMismatch { .. })
    ));
    let plain = MemDir::new();
    Store::create(&plain, options()).unwrap().close().unwrap();
    assert!(matches!(
        Store::verify_layer_dir(&plain, &base),
        Err(PersistError::SnapshotKindMismatch { .. })
    ));
    assert!(Store::verify_dir(&plain).unwrap().is_ok());

    // A flipped byte in a layer file: every form names it.
    let layer_file = dir
        .file_names()
        .into_iter()
        .find(|n| n.ends_with("/layer.bin"))
        .unwrap();
    let mut bytes = dir.file(&layer_file).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0x01;
    dir.put_file(&layer_file, bytes);
    for (what, report) in [
        ("verify_dir", Store::verify_dir(&dir).unwrap()),
        (
            "verify_layer_dir",
            Store::verify_layer_dir(&dir, &base).unwrap(),
        ),
    ] {
        assert!(!report.is_ok(), "{what}");
        let snapshot = layer_file.rsplit_once('/').unwrap().0;
        assert!(
            report
                .problems
                .iter()
                .any(|p| p.path.contains(snapshot.trim_start_matches("snapshots/"))),
            "{what}: {:?}",
            report.problems
        );
    }
}

/// The damage report of a layer store reads its files without the base: intact, no damage and a
/// note naming the base; a damaged WAL record is listed.
#[test]
fn the_damage_report_of_a_layer_store_lists_the_damage_of_its_files() {
    let base = layer_base();
    let dir = MemDir::new();
    closed_layer_store(&dir, &base);
    let report = Store::damage_report_dir(&dir).unwrap();
    assert!(report.is_empty(), "{report}");
    assert!(
        report
            .notes
            .iter()
            .any(|n| n.contains("layer store") && n.contains(&base.base_ref().unwrap().to_string())),
        "{:?}",
        report.notes
    );
    let wal = dir
        .file_names()
        .into_iter()
        .filter(|n| n.starts_with("wal/"))
        .last()
        .unwrap();
    let mut bytes = dir.file(&wal).unwrap();
    let at = 64 + (bytes.len() - 64) / 2;
    bytes[at] ^= 0x10;
    dir.put_file(&wal, bytes);
    let report = Store::damage_report_dir(&dir).unwrap();
    assert!(!report.is_empty(), "{report}");
}

/// Read-only views of a layer store over its base: `open_layer_read_only` of the closed store
/// and `read_only_at` of the open one, at the latest commit and at a mark, equal the oracle of
/// that many units; another base is refused.
#[test]
fn read_only_views_of_a_layer_store_load_over_its_base() {
    let base = layer_base();
    let all = history().len();
    let dir = MemDir::new();
    let (live, at_mark) = closed_layer_store(&dir, &base);
    let half = oracle(&base, all / 2);
    let latest = Store::open_layer_read_only(&dir, &base, RecoveryTarget::Latest).unwrap();
    assert_eq!(state(&latest.graph), live);
    let at = Store::open_layer_read_only(&dir, &base, RecoveryTarget::Mark("half".into())).unwrap();
    assert_same(&at.graph, &half, "at the mark");
    assert_eq!(at.graph.last_commit_seq().unwrap(), at_mark);
    // Shared, it serves the base's models (none here) and reads as the layer.
    let shared = at.into_graph();
    assert_eq!(state(&shared.read()), state(&half));
    assert!(matches!(
        Store::open_layer_read_only(&dir, &layer_base(), RecoveryTarget::Latest),
        Err(PersistError::LayerBaseMismatch { .. })
    ));
    let plain = MemDir::new();
    Store::create(&plain, options()).unwrap().close().unwrap();
    assert!(matches!(
        Store::open_layer_read_only(&plain, &base, RecoveryTarget::Latest),
        Err(PersistError::SnapshotKindMismatch { .. })
    ));

    let store = LayerStore::open_layer(&dir, &base, options()).unwrap();
    let view = store
        .read_only_at(RecoveryTarget::CommitSeq(at_mark))
        .unwrap();
    assert_same(&view.graph, &half, "read_only_at");
    let view = store.read_only_at(RecoveryTarget::Latest).unwrap();
    assert_eq!(state(&view.graph), live);
    assert_eq!(
        state(&store.graph().read()),
        live,
        "the open store is unchanged"
    );
}

/// A closed layer store's checkpoint (`checkpoint_layer_dir`) writes a layer snapshot over the
/// same base, from which the store reopens without replay; another base is refused with nothing
/// written; the plain `checkpoint_dir` refuses a layer store.
#[test]
fn a_closed_layer_store_checkpoints_over_its_base() {
    let base = layer_base();
    let dir = MemDir::new();
    let (live, _) = closed_layer_store(&dir, &base);
    let before = mem_files(&dir);
    assert!(matches!(
        LayerStore::checkpoint_layer_dir(&dir, &layer_base(), options(), None),
        Err(PersistError::LayerBaseMismatch { .. })
    ));
    assert_layer_store_refused(
        Store::checkpoint_dir(&dir, options(), None),
        "checkpoint_dir",
    );
    assert_eq!(mem_files(&dir), before, "nothing written");
    let snapshots = Store::snapshots_dir(&dir).unwrap().len();
    let report = LayerStore::checkpoint_layer_dir(&dir, &base, options(), Some("closed")).unwrap();
    assert_eq!(report.snapshot.name, "closed");
    let listed = Store::snapshots_dir(&dir).unwrap();
    assert_eq!(listed.len(), snapshots + 1);
    assert_eq!(listed.last().unwrap().layer_base, base.base_ref().copied());
    assert_eq!(base_of(&dir), base.base_ref().copied());
    // Again at the same position: the existing snapshot.
    let again = LayerStore::checkpoint_layer_dir(&dir, &base, options(), Some("closed")).unwrap();
    assert_eq!(again.snapshot.commit_seq, report.snapshot.commit_seq);
    assert_eq!(Store::snapshots_dir(&dir).unwrap().len(), snapshots + 1);
    let store = LayerStore::open_layer(&dir, &base, options()).unwrap();
    assert_eq!(store.open_report().commits_replayed, 0);
    assert_eq!(state(&store.graph().read()), live);
}

/// Every vertex and edge with labels, properties in order and endpoints; the label buckets;
/// the statistics' counts; the property indexes; the schema; the catalog; the lineage id, the
/// position, the commit time and the id sequences, for any persistent storage: what a
/// materialized snapshot must show of the layer.
fn visible<S: PersistentStorage>(graph: &S) -> Vec<String> {
    let mut rows = Vec::new();
    let vid = |node| graph.vertex_id(node).unwrap().unwrap().to_string();
    let eid = |edge| graph.edge_id(edge).unwrap().unwrap().to_string();
    let mut labels = std::collections::BTreeSet::new();
    let mut edge_labels = std::collections::BTreeSet::new();
    for node in graph.vertices().unwrap() {
        let mut out: Vec<String> = graph.out_edges(node).unwrap().map(eid).collect();
        let mut inc: Vec<String> = graph.in_edges(node).unwrap().map(eid).collect();
        out.sort();
        inc.sort();
        let node_labels = graph.vertex_labels(node).unwrap();
        labels.extend(node_labels.iter().map(|l| l.to_string()));
        rows.push(format!(
            "v {} {:?} {:?} out {out:?} in {inc:?}",
            vid(node),
            node_labels,
            graph.vertex_properties(node).unwrap(),
        ));
    }
    for edge in graph.edges().unwrap() {
        let (from, to) = graph.edge_endpoints(edge).unwrap().unwrap();
        let label = graph.edge_label(edge).unwrap();
        if let Some(label) = label {
            edge_labels.insert(label.to_string());
        }
        rows.push(format!(
            "e {} {:?} {} -> {} {:?}",
            eid(edge),
            label,
            vid(from),
            vid(to),
            graph.edge_properties(edge).unwrap(),
        ));
    }
    for label in &labels {
        let mut members: Vec<String> = graph.vertices_by_label(label).unwrap().map(vid).collect();
        members.sort();
        rows.push(format!("label {label} {members:?}"));
    }
    for label in &edge_labels {
        let mut members: Vec<String> = graph.edges_by_label(label).unwrap().map(eid).collect();
        members.sort();
        rows.push(format!("edge label {label} {members:?}"));
    }
    if let Some(statistics) = graph.statistics().unwrap() {
        let sorted = |counts: Vec<String>| {
            let mut counts = counts;
            counts.sort();
            counts
        };
        rows.push(format!(
            "statistics {} {} {:?} {:?}",
            statistics.vertex_count(),
            statistics.edge_count(),
            sorted(
                statistics
                    .vertex_label_counts()
                    .iter()
                    .map(|c| format!("{c:?}"))
                    .collect()
            ),
            sorted(
                statistics
                    .edge_label_counts()
                    .iter()
                    .map(|c| format!("{c:?}"))
                    .collect()
            ),
        ));
    }
    // The definitions and entry counts (a layer's index statistics count its overlays
    // otherwise: distinct values and bytes are no data).
    let mut indexes: Vec<String> = graph
        .property_indexes()
        .unwrap()
        .iter()
        .map(|info| {
            let text = format!("{info:?}");
            let head = text
                .split(", stats:")
                .next()
                .unwrap_or_default()
                .to_string();
            format!("{head} entries {}", info.stats.entries)
        })
        .collect();
    indexes.sort();
    rows.push(format!("indexes {indexes:?}"));
    rows.push(format!(
        "schema {}",
        graph.get_schema().unwrap().unwrap_or_default().to_json()
    ));
    let mut catalog: Vec<String> = graph
        .definitions()
        .unwrap()
        .map(|d| format!("{d:?}"))
        .collect();
    catalog.sort();
    rows.push(format!("catalog {catalog:?}"));
    rows.push(format!(
        "position {} {} {} {:?}",
        graph.graph_id(),
        graph.last_commit_seq().unwrap(),
        graph.last_committed_at(),
        graph.id_sequences()
    ));
    rows.sort();
    rows
}

/// The materialized export of a layer store (and `WriteOptions::with_materialize` of a bare
/// layer) is a standalone whole-graph snapshot of file format version 4: `read_snapshot` loads it
/// without a base, and it shows exactly what the layer shows; it becomes a plain store.
#[test]
fn a_materialized_export_is_the_whole_visible_graph_in_format_4() {
    use graphersal::persist::{ModelCopy, ReadOptions, read_snapshot, read_snapshot_info};

    let base = layer_base();
    let dir = MemDir::new();
    let store = LayerStore::create_layer(&dir, &base, options()).unwrap();
    commit_units(&store, 0, history().len());
    let expected = visible(&*store.graph().read());
    let mut bytes = Vec::new();
    let info = store
        .export_materialized(&mut bytes, ModelCopy::Include)
        .unwrap();
    assert_eq!(info.layer_base, None, "no layer");
    assert_eq!(
        u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
        4,
        "file format version 4: older releases read it"
    );
    let info = read_snapshot_info(bytes.as_slice()).unwrap();
    assert_eq!(info.layer_base, None);
    assert_eq!(
        info.vertex_count,
        store.graph().read().vertex_count().unwrap() as u64
    );
    assert_eq!(
        info.edge_count,
        store.graph().read().edge_count().unwrap() as u64
    );
    let whole = read_snapshot(bytes.as_slice(), &ReadOptions::new()).unwrap();
    assert_eq!(visible(&whole), expected);
    // It is no layer snapshot: over the base it is refused.
    assert!(matches!(
        graphersal::persist::read_layer_snapshot(bytes.as_slice(), &ReadOptions::new(), &base),
        Err(PersistError::SnapshotKindMismatch { .. })
    ));
    // A plain store from it.
    let plain = MemDir::new();
    let from = Store::create_from_packed(&plain, bytes.as_slice(), options()).unwrap();
    assert_eq!(visible(&*from.graph().read()), expected);
    drop(from);
    store.close().unwrap();

    // The write option on a bare layer, for any writer.
    let layer = oracle(&base, history().len());
    let mut bare = Vec::new();
    write_snapshot(
        &layer,
        &mut bare,
        &WriteOptions::new().with_materialize(true),
    )
    .unwrap();
    assert_eq!(u32::from_le_bytes(bare[8..12].try_into().unwrap()), 4);
    assert_eq!(
        visible(&read_snapshot(bare.as_slice(), &ReadOptions::new()).unwrap()),
        visible(&layer)
    );
    // Without it, a layer snapshot (version 5).
    let mut delta = Vec::new();
    write_snapshot(&layer, &mut delta, &WriteOptions::new()).unwrap();
    assert_eq!(u32::from_le_bytes(delta[8..12].try_into().unwrap()), 5);
    assert!(delta.len() < bare.len());
}

/// Rule 22: the base a layer store was built over (`base_ref`) is a creation parameter: every
/// copy path supported for layer stores carries it (backups full, increment, from the closed
/// store, ZIP and from memory, their restores, the read-only open of a backup, the exported
/// snapshot, the closed checkpoint), and the copy paths not supported yet (fork, attic, rollback,
/// repair, convert, upgrade, closed-store commits) refuse a layer store with
/// `StoreFailure::LayerStore` and write nothing.
#[test]
fn the_base_ref_is_a_creation_parameter_carried_by_every_copy_path() {
    use graphersal::persist::{BackupMode, ClosedCommitOptions};

    let base = layer_base();
    let identity = Some(*base.base_ref().unwrap());
    let dir = MemDir::new();
    let store = LayerStore::create_layer(&dir, &base, options()).unwrap();
    assert_eq!(base_of(&dir), identity, "create");
    assert_eq!(store.info().unwrap().layer_base, identity, "info");
    commit_units(&store, 0, 3);
    store.mark("m").unwrap();
    commit_units(&store, 3, history().len());
    store.checkpoint(None).unwrap();
    assert_eq!(base_of(&dir), identity, "checkpoint");

    let full = MemDir::new();
    store.backup(&full).unwrap();
    assert_eq!(base_of(&full), identity, "full backup");
    store.backup(&full).unwrap();
    assert_eq!(base_of(&full), identity, "increment");
    let mut packed = Vec::new();
    assert_eq!(
        store.export_packed(None, &mut packed).unwrap().layer_base,
        identity,
        "export"
    );
    #[cfg(feature = "persist-zip")]
    let zip = {
        let mut zip = std::io::Cursor::new(Vec::new());
        assert_eq!(
            store.backup_zip(&mut zip).unwrap().layer_base,
            identity,
            "zip"
        );
        zip
    };

    // Not supported yet (S3.4b): refused, nothing written.
    assert_layer_store_refused(store.fork(RecoveryTarget::Latest, MemDir::new()), "fork");
    assert_layer_store_refused(
        store.rollback_to(RecoveryTarget::Mark("m".into()), RollbackMode::Move),
        "rollback",
    );
    assert_layer_store_refused(store.attic_restore("none"), "attic restore");
    assert_layer_store_refused(store.attic_can_restore("none"), "attic can restore");
    assert_layer_store_refused(store.attic_fork("none", MemDir::new()), "attic fork");
    assert_layer_store_refused(store.repair_to(MemDir::new()), "repair");
    store.close().unwrap();
    let before = mem_files(&dir);
    for (what, result) in [
        (
            "fork_dir",
            Store::fork_dir(&dir, RecoveryTarget::Latest, MemDir::new()).map(|_| ()),
        ),
        (
            "attic_fork_dir",
            Store::attic_fork_dir(&dir, "none", MemDir::new()).map(|_| ()),
        ),
        (
            "repair_dir",
            Store::repair_dir(&dir, MemDir::new()).map(|_| ()),
        ),
        ("convert", Store::convert(&dir, MemDir::new()).map(|_| ())),
        ("upgrade", Store::upgrade(&dir, MemDir::new()).map(|_| ())),
        (
            "upgrade_in_place",
            Store::upgrade_in_place(&dir).map(|_| ()),
        ),
        (
            "closed-store commit",
            Store::remove_model_dir(&dir, "acme/none", ClosedCommitOptions::new()).map(|_| ()),
        ),
    ] {
        assert_layer_store_refused(result, what);
    }
    assert_eq!(mem_files(&dir), before, "the refusals wrote nothing");

    // The closed store's paths.
    let by_dir = MemDir::new();
    Store::backup_dir_with(&dir, &by_dir, BackupMode::Full).unwrap();
    assert_eq!(base_of(&by_dir), identity, "backup_dir");
    #[cfg(feature = "persist-zip")]
    let closed_zip = {
        let mut closed_zip = std::io::Cursor::new(Vec::new());
        Store::backup_zip_dir(&dir, &mut closed_zip).unwrap();
        closed_zip
    };
    let mut packed_dir = Vec::new();
    assert_eq!(
        Store::export_packed_dir(&dir, None, &mut packed_dir)
            .unwrap()
            .layer_base,
        identity,
        "export_packed_dir"
    );
    {
        let store = LayerStore::open_layer(&dir, &base, options()).unwrap();
        let mut graph = store.graph().write();
        GraphTraversalSource::new(&mut *graph)
            .add_v("person")
            .iterate()
            .unwrap();
    }
    LayerStore::checkpoint_layer_dir(&dir, &base, options(), Some("closed")).unwrap();
    assert_eq!(base_of(&dir), identity, "closed checkpoint");
    let opened = LayerStore::open_layer_backup(&full, &base, options()).unwrap();
    assert_eq!(
        opened.info().unwrap().layer_base,
        identity,
        "open_layer_backup"
    );
    drop(opened);
    for (what, copy) in [
        ("restore of the backup", &full),
        ("restore of backup_dir", &by_dir),
    ] {
        Store::restore_backup(copy).unwrap();
        assert_eq!(base_of(copy), identity, "{what}");
        assert_eq!(
            LayerStore::open_layer(copy, &base, options())
                .unwrap()
                .layer_base()
                .unwrap(),
            identity.unwrap(),
            "{what}"
        );
    }
    #[cfg(feature = "persist-zip")]
    for (what, archive) in [
        ("restore_zip", zip),
        ("restore_zip of backup_zip_dir", closed_zip),
    ] {
        let restored = MemDir::new();
        Store::restore_zip(archive.get_ref().as_slice(), &restored).unwrap();
        assert_eq!(base_of(&restored), identity, "{what}");
    }
    for (what, bytes) in [("export", packed), ("export_packed_dir", packed_dir)] {
        assert_eq!(
            graphersal::persist::read_snapshot_info(bytes.as_slice())
                .unwrap()
                .layer_base,
            identity,
            "{what}"
        );
    }

    // A backup from memory (it needs damage found while open).
    let store = LayerStore::open_layer(&dir, &base, options()).unwrap();
    let manifest = dir
        .file_names()
        .into_iter()
        .find(|n| n.starts_with("snapshots/") && n.ends_with("/manifest"))
        .unwrap();
    let mut bytes = dir.file(&manifest).unwrap();
    let at = bytes.len() / 2;
    bytes[at] ^= 0x20;
    dir.put_file(&manifest, bytes);
    let _ = store.verify().unwrap();
    let saved = MemDir::new();
    store.backup_from_memory(&saved, BackupMode::Full).unwrap();
    assert_eq!(base_of(&saved), identity, "backup from memory");
    Store::restore_backup(&saved).unwrap();
    assert_eq!(base_of(&saved), identity, "restored backup from memory");
}

/// A layer over a Store base with an embedding model in its catalog: the layer's catalog names
/// the base's model (its blob lives in the base), and backups, verification and the export of
/// the layer store still succeed (the blob is the base's, not the layer store's to carry).
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn copies_of_a_layer_store_over_a_base_with_models_succeed() {
    use graphersal::persist::{ImportModelOptions, ModelCopy, ModelSource, ReadOptions};

    let temp = Temp::new("model-base");
    let base_dir = temp.join("base");
    let base_graph = Arc::try_unwrap(base()).unwrap_or_else(|_| unreachable!());
    let base_store = Store::create_with(&base_dir, base_graph, options()).unwrap();
    base_store
        .import_model(
            ModelSource::Files(crate::model_fixture::tiny_model(1, "acme/tiny")),
            ImportModelOptions::new(),
        )
        .unwrap();
    base_store.close().unwrap();
    let view = Store::open_read_only(&base_dir, RecoveryTarget::Latest).unwrap();
    let base = LayerBase::from_graph(view.into_graph()).unwrap();
    let dir = MemDir::new();
    let store = LayerStore::create_layer(&dir, &base, options()).unwrap();
    commit_units(&store, 0, history().len());
    store.checkpoint(None).unwrap();
    let live = state(&store.graph().read());
    let verified = store.verify().unwrap();
    assert!(verified.is_ok(), "{:?}", verified.problems);
    let backup = MemDir::new();
    let report = store.backup(&backup).unwrap();
    assert!(report.models.is_empty(), "the layer store keeps no model");
    #[cfg(feature = "persist-zip")]
    {
        let mut zip = std::io::Cursor::new(Vec::new());
        store.backup_zip(&mut zip).unwrap();
        let unzipped = MemDir::new();
        Store::restore_zip(zip.get_ref().as_slice(), &unzipped).unwrap();
        let restored = LayerStore::open_layer(&unzipped, &base, options()).unwrap();
        assert_eq!(state(&restored.graph().read()), live);
    }
    let mut packed = Vec::new();
    store.export_packed(None, &mut packed).unwrap();
    let mut whole = Vec::new();
    let info = store
        .export_materialized(&mut whole, ModelCopy::Include)
        .unwrap();
    assert!(
        info.blobs_not_carried.is_empty(),
        "{:?}",
        info.blobs_not_carried
    );
    store.close().unwrap();
    let verified = Store::verify_dir(&dir).unwrap();
    assert!(verified.is_ok(), "{:?}", verified.problems);
    let verified = Store::verify_layer_dir(&dir, &base).unwrap();
    assert!(verified.is_ok(), "{:?}", verified.problems);
    Store::restore_backup(&backup).unwrap();
    let restored = LayerStore::open_layer(&backup, &base, options()).unwrap();
    assert_eq!(state(&restored.graph().read()), live);
    drop(restored);
    let loaded =
        graphersal::persist::read_layer_snapshot(packed.as_slice(), &ReadOptions::new(), &base)
            .unwrap();
    assert_eq!(state(&loaded.read()), live);
    // The materialized export carries the model's blob from the base: a standalone store.
    let standalone = MemDir::new();
    let opened = Store::create_from_packed(&standalone, whole.as_slice(), options()).unwrap();
    assert_eq!(opened.models().unwrap().len(), 1);
}
