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

/// What is not supported for layer stores yet refuses with `StoreFailure::LayerStore` (copies,
/// verification, rollback, closed-store checkpoints, views); what works on files only works.
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
    assert_layer_store_refused(store.backup(MemDir::new()), "backup");
    assert_layer_store_refused(
        store.backup_from_memory(MemDir::new(), graphersal::persist::BackupMode::Full),
        "backup from memory",
    );
    assert_layer_store_refused(store.verify(), "verify");
    assert_layer_store_refused(store.export_packed(None, Vec::new()), "export");
    assert_layer_store_refused(
        store.rollback_to(RecoveryTarget::Mark("m".into()), RollbackMode::Move),
        "rollback",
    );
    assert_layer_store_refused(store.read_only_at(RecoveryTarget::Latest), "view");
    assert_layer_store_refused(store.repair_to(MemDir::new()), "repair");
    // File-level operations work.
    store.prune(0).unwrap();
    assert!(store.changes_since(0).is_ok());
    assert_eq!(store.marks().unwrap().len(), 1);
    assert!(store.compaction_advice().is_ok());
    store.close().unwrap();

    // The closed store.
    assert_layer_store_refused(Store::verify_dir(&dir), "verify_dir");
    assert_layer_store_refused(
        Store::fork_dir(&dir, RecoveryTarget::Latest, MemDir::new()),
        "fork_dir",
    );
    assert_layer_store_refused(Store::backup_dir(&dir, MemDir::new()), "backup_dir");
    assert_layer_store_refused(
        Store::export_packed_dir(&dir, None, Vec::new()),
        "export_packed_dir",
    );
    assert_layer_store_refused(
        Store::checkpoint_dir(&dir, options(), None),
        "checkpoint_dir",
    );
    assert_layer_store_refused(
        Store::open_read_only(&dir, RecoveryTarget::Latest),
        "open_read_only",
    );
    assert_layer_store_refused(Store::convert(&dir, MemDir::new()), "convert");
    assert_layer_store_refused(Store::open_backup(&dir, options()), "open_backup");
    assert_layer_store_refused(Store::damage_report_dir(&dir), "damage_report_dir");
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
