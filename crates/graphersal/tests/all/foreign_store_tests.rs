//! The Store over a storage other than `TraversalGraph` (`book/src/for_developers/custom_storage.md`):
//! a `Store<Forwarding>` (the proof storage of `graphersal-storage-tests`,
//! included once by path in `main.rs`: its own handle types, every method forwarded) on every
//! backend (a directory, memory, one file): create, commits, reopen with WAL replay,
//! checkpoints (manual and automatic), rollback and attic restore (a user hook survives), full
//! and incremental backups and `open_backup_in`, ZIP, marks, the read-only open of a store with
//! a critical definition of an unknown kind, damage (maintenance mode), and the files: a store
//! written through `Store<Forwarding>` opens as `Store<TraversalGraph>` with the same state and
//! vice versa. The capability precondition is a crate test (`persist/store/graph.rs`), where a
//! storage claiming fewer capabilities already exists.
#![cfg(all(feature = "persist", not(target_arch = "wasm32")))]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use graphersal::catalog::{Definition, DefinitionFlags, DefinitionKind};
use graphersal::error::GraphError;
use graphersal::persist::{
    self, FsDir, MemDir, PersistError, RecoveryTarget, RollbackMode, SingleFileDir, SnapshotInfo,
    Store, StoreDir, StoreOptions, WriteOptions,
};
use graphersal::prelude::*;
use graphersal::storage::{
    ChangeSet, CommitHook, GraphStorage, HookError, PersistentStorage, Transactional,
};

use crate::forwarding::Forwarding;

/// A fresh directory under the system temp dir, removed on drop.
struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "graphersal-foreign-{}-{}-{name}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug, Clone, Copy)]
enum Backend {
    Dir,
    Memory,
    File,
}

const BACKENDS: [Backend; 3] = [Backend::Dir, Backend::Memory, Backend::File];

impl Backend {
    fn make(self, temp: &Temp, name: &str) -> Arc<dyn StoreDir> {
        match self {
            Backend::Dir => Arc::new(FsDir::new(temp.0.join(name))),
            Backend::Memory => Arc::new(MemDir::named(name)),
            Backend::File => Arc::new(SingleFileDir::new(temp.0.join(format!("{name}.gstore")))),
        }
    }
}

fn options() -> StoreOptions {
    StoreOptions::new()
        .with_wal_segment_bytes(600)
        .with_checkpoint_after_bytes(0)
}

fn open(dir: &Arc<dyn StoreDir>) -> Store<Forwarding> {
    Store::open_in(dir, options(), Forwarding::default).unwrap()
}

/// Commits a `person` vertex named `name` (one unit, so one WAL record).
fn add_person<S: PersistentStorage + Default>(store: &Store<S>, name: &str) {
    store
        .graph()
        .write()
        .transaction(|g| {
            let mut properties = PropertyMap::new();
            properties.insert("name", ElementProperty::String(name.into()));
            g.add_vertex(None, vec!["person".into()], properties)?;
            Ok::<_, GraphError>(())
        })
        .unwrap();
}

/// The `name`s of the vertices, sorted.
fn names<S: GraphStorage>(graph: &S) -> Vec<String> {
    let mut names: Vec<String> = graph
        .vertices()
        .unwrap()
        .filter_map(
            |v| match graph.vertex_properties(v).unwrap()?.get("name")? {
                ElementProperty::String(s) => Some(s.to_string()),
                _ => None,
            },
        )
        .collect();
    names.sort();
    names
}

/// The whole state of a graph, storage-independent: its packed snapshot's data sections (the
/// schema, the catalog and every element, byte for byte) and the manifest's listing without
/// the write time and the memory estimate (which follows index capacities, not data): the
/// position, lineage id and counts.
fn state<S: PersistentStorage>(graph: &S) -> (Vec<u8>, SnapshotInfo) {
    let mut bytes = Vec::new();
    persist::write_snapshot(graph, &mut bytes, &WriteOptions::new()).unwrap();
    let len = u64::from_le_bytes(bytes[17..25].try_into().unwrap()) as usize;
    let mut info = persist::read_snapshot_info(bytes.as_slice()).unwrap();
    info.created_at = 0;
    info.memory_bytes = 0;
    (bytes[25 + len..bytes.len() - 4].to_vec(), info)
}

/// A store with history: the modern graph, then `ann`, a mark, `bob`.
fn with_history(dir: &Arc<dyn StoreDir>) -> Store<Forwarding> {
    let store = Store::create_in(
        dir,
        Forwarding::new(TraversalGraph::tinkerpop_modern()),
        options(),
    )
    .unwrap();
    add_person(&store, "ann");
    store.mark("after-ann").unwrap();
    add_person(&store, "bob");
    store
}

#[test]
fn a_forwarding_store_creates_commits_reopens_and_checkpoints_on_every_backend() {
    for backend in BACKENDS {
        let temp = Temp::new("basics");
        let dir = backend.make(&temp, "main");
        let store = with_history(&dir);
        let modern = names(&TraversalGraph::tinkerpop_modern());
        assert_eq!(names(&*store.graph().read()).len(), modern.len() + 2);
        let marks = store.marks().unwrap();
        assert_eq!(marks.len(), 1, "{backend:?}");
        assert_eq!(marks[0].name(), "after-ann");
        let before = state(&*store.graph().read());
        store.close().unwrap();

        // Reopen: the snapshot through the load sink, the WAL replayed.
        let store = open(&dir);
        assert!(store.open_report().commits_replayed >= 2, "{backend:?}");
        assert_eq!(state(&*store.graph().read()), before, "{backend:?}");
        // A past state (the mark) into a new Forwarding.
        let past = store
            .read_only_at(RecoveryTarget::Mark("after-ann".into()))
            .unwrap();
        assert!(names(&past.graph).contains(&"ann".to_string()));
        assert!(!names(&past.graph).contains(&"bob".to_string()));
        let past = Store::open_read_only_in(
            &dir,
            RecoveryTarget::Mark("after-ann".into()),
            Forwarding::default(),
        )
        .unwrap();
        assert!(!names(&past.graph).contains(&"bob".to_string()));

        // A manual checkpoint, then more commits, then a reopen from the new snapshot.
        let snapshot = store.checkpoint(Some("manual")).unwrap();
        assert_eq!(
            snapshot.commit_seq,
            store.graph().read().last_commit_seq().unwrap()
        );
        add_person(&store, "cy");
        let before = state(&*store.graph().read());
        store.close().unwrap();
        let store = open(&dir);
        assert_eq!(store.open_report().snapshot_commit_seq, snapshot.commit_seq);
        assert_eq!(state(&*store.graph().read()), before, "{backend:?}");
        assert!(store.verify().unwrap().is_ok(), "{backend:?}");
        store.close().unwrap();

        // The automatic checkpoint past the threshold (a background thread natively).
        let store = Store::open_in(
            &dir,
            options().with_checkpoint_after_bytes(2_000),
            Forwarding::default,
        )
        .unwrap();
        let snapshots = store.snapshots().unwrap().len();
        for i in 0..40 {
            add_person(&store, &format!("p{i:02}"));
        }
        let deadline = Instant::now() + Duration::from_secs(20);
        while store.snapshots().unwrap().len() <= snapshots {
            assert!(
                Instant::now() < deadline,
                "{backend:?}: no automatic checkpoint"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(store.last_auto_checkpoint_error().unwrap().is_none());
        let before = state(&*store.graph().read());
        store.close().unwrap();
        let store = open(&dir);
        assert_eq!(state(&*store.graph().read()), before, "{backend:?}");
        store.close().unwrap();
    }
}

/// Counts the commits it sees.
struct Counter(Arc<AtomicU64>);

impl CommitHook for Counter {
    fn name(&self) -> &str {
        "counter"
    }

    fn after_commit(&mut self, _changes: &ChangeSet) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    fn before_commit(&mut self, _changes: &ChangeSet) -> Result<(), HookError> {
        Ok(())
    }
}

#[test]
fn rollback_and_attic_restore_reload_the_same_forwarding_instance_and_keep_its_hooks() {
    for backend in BACKENDS {
        let temp = Temp::new("rollback");
        let dir = backend.make(&temp, "main");
        let store = with_history(&dir);
        let seen = Arc::new(AtomicU64::new(0));
        store
            .graph()
            .write()
            .register_commit_hook(Box::new(Counter(seen.clone())))
            .unwrap();
        let graph = store.graph().clone();
        let full = state(&*store.graph().read());

        let report = store
            .rollback_to(RecoveryTarget::Mark("after-ann".into()), RollbackMode::Move)
            .unwrap();
        // The same `Arc<Graph<Forwarding>>`, reloaded in place.
        assert!(Arc::ptr_eq(&graph, store.graph()));
        let current = names(&*store.graph().read());
        assert!(current.contains(&"ann".to_string()), "{backend:?}");
        assert!(!current.contains(&"bob".to_string()), "{backend:?}");
        let entries = store.attic_list().unwrap();
        assert_eq!(entries.len(), 1, "{backend:?}: {report:?}");

        // Restore: bob is back, with the graph id and position of before the rollback.
        store.attic_restore(&entries[0].id).unwrap();
        assert_eq!(state(&*store.graph().read()), full, "{backend:?}");
        assert!(Arc::ptr_eq(&graph, store.graph()));

        // The user hook survived both reloads.
        add_person(&store, "cy");
        assert_eq!(seen.load(Ordering::SeqCst), 1, "{backend:?}");
        let before = state(&*store.graph().read());
        store.close().unwrap();
        let store = open(&dir);
        assert_eq!(state(&*store.graph().read()), before, "{backend:?}");
        assert!(store.verify().unwrap().is_ok(), "{backend:?}");
        store.close().unwrap();
    }
}

#[test]
fn full_and_incremental_backups_of_a_forwarding_store_open_as_forwarding() {
    for backend in BACKENDS {
        let temp = Temp::new("backup");
        let dir = backend.make(&temp, "main");
        let backup = backend.make(&temp, "backup");
        let store = with_history(&dir);
        let full = store.backup(&backup).unwrap();
        assert!(!full.incremental, "{backend:?}");
        add_person(&store, "cy");
        store.checkpoint(None).unwrap();
        add_person(&store, "dee");
        let increment = store.backup(&backup).unwrap();
        assert!(increment.incremental, "{backend:?}");
        let expected = state(&*store.graph().read());

        let opened = Store::open_backup_in(&backup, options(), Forwarding::default).unwrap();
        assert!(opened.backup_marker().is_some());
        assert_eq!(state(&*opened.graph().read()), expected, "{backend:?}");
        // A backup refuses commits.
        let refused = opened.graph().write().transaction(|g| {
            g.add_vertex(None, vec![], PropertyMap::new())?;
            Ok::<_, GraphError>(())
        });
        assert!(refused.is_err(), "{backend:?}");
        assert!(matches!(
            opened.mark("m"),
            Err(PersistError::IsBackup { .. })
        ));
        drop(opened);
        store.close().unwrap();
    }
}

#[cfg(feature = "persist-zip")]
#[test]
fn a_zip_backup_of_a_forwarding_store_restores_and_opens_as_forwarding() {
    for backend in BACKENDS {
        let temp = Temp::new("zip");
        let dir = backend.make(&temp, "main");
        let store = with_history(&dir);
        let mut archive = std::io::Cursor::new(Vec::new());
        store.backup_zip(&mut archive).unwrap();
        let expected = state(&*store.graph().read());
        store.close().unwrap();

        let restored = backend.make(&temp, "restored");
        Store::restore_zip(std::io::Cursor::new(archive.into_inner()), &restored).unwrap();
        let store = open(&restored);
        assert_eq!(state(&*store.graph().read()), expected, "{backend:?}");
        store.close().unwrap();
    }
}

#[test]
fn a_critical_unknown_definition_opens_a_forwarding_store_read_only() {
    for backend in BACKENDS {
        let temp = Temp::new("critical");
        let dir = backend.make(&temp, "main");
        let mut graph = TraversalGraph::tinkerpop_modern();
        graph
            .set_definition(Definition::opaque(
                DefinitionKind::from_code(77),
                DefinitionFlags::NONE.with_critical(true),
                "future",
                vec![1, 2, 3],
            ))
            .unwrap();
        Store::create_in(&dir, Forwarding::new(graph), options())
            .unwrap()
            .close()
            .unwrap();
        let store = open(&dir);
        let reason = store.read_only_reason().unwrap().expect("read-only");
        assert!(reason.contains("critical"), "{backend:?}: {reason}");
        assert_eq!(store.graph().read().definitions().unwrap().count(), 1);
        let refused = store.graph().write().transaction(|g| {
            g.add_vertex(None, vec![], PropertyMap::new())?;
            Ok::<_, GraphError>(())
        });
        assert!(refused.is_err(), "{backend:?}");
        assert!(matches!(
            store.checkpoint(None),
            Err(PersistError::ReadOnly { .. })
        ));
        store.close().unwrap();
    }
}

/// The name of the newest WAL segment.
fn last_wal(dir: &Arc<dyn StoreDir>) -> String {
    let mut names: Vec<String> = dir
        .list("wal")
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    names.sort();
    format!("wal/{}", names.last().expect("a WAL segment"))
}

#[test]
fn damage_opens_a_forwarding_store_in_maintenance_mode_with_the_salvaged_state() {
    for backend in BACKENDS {
        let temp = Temp::new("damage");
        let dir = backend.make(&temp, "main");
        let store = with_history(&dir);
        store.close().unwrap();
        // A flipped bit in the last record (bob's commit).
        let wal = last_wal(&dir);
        let mut bytes = dir.read(&wal, u64::MAX).unwrap();
        let at = bytes.len() - 8;
        bytes[at] ^= 0x10;
        dir.write_atomic(&wal, &bytes).unwrap();

        let store = open(&dir);
        let report = store.maintenance().expect("maintenance mode");
        assert!(!report.damage.is_empty(), "{backend:?}");
        // The salvaged state is served by a Forwarding: what was intact is readable.
        let current = names(&*store.graph().read());
        assert!(
            current.contains(&"ann".to_string()),
            "{backend:?}: {current:?}"
        );
        let refused = store.graph().write().transaction(|g| {
            g.add_vertex(None, vec![], PropertyMap::new())?;
            Ok::<_, GraphError>(())
        });
        assert!(refused.is_err(), "{backend:?}");
        assert!(matches!(
            store.mark("m"),
            Err(PersistError::Maintenance { .. })
        ));
        // The repaired copy is storage-independent: it opens as Forwarding.
        let repaired = backend.make(&temp, "repaired");
        store.repair_to(&repaired).unwrap();
        let fixed = open(&repaired);
        assert!(fixed.maintenance().is_none(), "{backend:?}");
        assert_eq!(names(&*fixed.graph().read()), current, "{backend:?}");
        fixed.close().unwrap();
        store.close().unwrap();
    }
}

#[test]
fn a_store_written_as_forwarding_opens_as_traversal_graph_and_vice_versa() {
    for backend in BACKENDS {
        let temp = Temp::new("cross");
        // Forwarding -> TraversalGraph.
        let dir = backend.make(&temp, "forwarding");
        let store = with_history(&dir);
        let expected = state(&*store.graph().read());
        store.close().unwrap();
        let store = Store::open(&dir, options()).unwrap();
        assert_eq!(state(&*store.graph().read()), expected, "{backend:?}");
        assert_eq!(store.marks().unwrap().len(), 1);
        // A commit through TraversalGraph, read back through Forwarding.
        add_person(&store, "cy");
        let expected = state(&*store.graph().read());
        store.close().unwrap();
        let store = open(&dir);
        assert_eq!(state(&*store.graph().read()), expected, "{backend:?}");
        store.close().unwrap();

        // TraversalGraph -> Forwarding.
        let dir = backend.make(&temp, "traversal");
        let store =
            Store::create_with(&dir, TraversalGraph::tinkerpop_modern(), options()).unwrap();
        add_person(&store, "ann");
        let expected = state(&*store.graph().read());
        store.close().unwrap();
        let store = open(&dir);
        assert_eq!(state(&*store.graph().read()), expected, "{backend:?}");
        store.close().unwrap();
    }
}
