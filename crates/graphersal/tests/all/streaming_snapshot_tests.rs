//! The streaming snapshot encoder through the public API (feature `persist`; format spec 8 and
//! 12.3): a checkpoint, a store's first snapshot and a fork interrupted after every write leave
//! a store that opens with its data (the partial `tmp-*` directory is removed); a packed
//! snapshot imported into a store of every backend and exported again is the same stream; a
//! damaged one is refused with nothing created; a failing writer is an error, never a panic.
//! Byte identity with the encoder this one replaced and its memory bound are crate tests
//! (`persist::encoder`, `persist::store::tests`).
#![cfg(feature = "persist")]

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use graphersal::persist::{
    self, Codec, MemDir, RecoveryTarget, Store, StoreDir, StoreOptions, WriteOptions,
};

use crate::backup_tests::Crashing;
use crate::persist_tests::{dump, varied_graph};

fn options() -> StoreOptions {
    // Small chunks and segments: a snapshot has many files and many writes.
    StoreOptions::new()
        .with_checkpoint_after_bytes(0)
        .with_chunk_bytes(256)
        .with_snapshot_segment_bytes(2048)
}

fn add_people(store: &Store, prefix: &str, n: usize) {
    for i in 0..n {
        store
            .graph()
            .write()
            .traversal_mut()
            .add_v("person")
            .property("name", format!("{prefix}{i} with a longer name"))
            .property("n", i as i64)
            .to_list()
            .unwrap();
    }
}

fn crashing(inner: &MemDir) -> Crashing {
    Crashing {
        inner: inner.clone(),
        budget: Arc::new(AtomicI64::new(i64::MAX)),
    }
}

/// The names under `snapshots/` that are temporary directories.
fn leftovers(dir: &MemDir) -> Vec<String> {
    dir.list("snapshots")
        .map(|entries| {
            entries
                .into_iter()
                .map(|e| e.name)
                .filter(|n| n.starts_with("tmp-"))
                .collect()
        })
        .unwrap_or_default()
}

/// A checkpoint interrupted after every write in turn: the store opens with every commit, the
/// partial snapshot is gone, and the next checkpoint succeeds.
#[test]
fn a_checkpoint_interrupted_after_every_write_leaves_a_store_that_opens() {
    for (steps, budget) in (0..).enumerate() {
        let image = MemDir::new();
        let dir = crashing(&image);
        let shared: Arc<dyn StoreDir> = Arc::new(dir.clone());
        let store = Store::create(&shared, options()).unwrap();
        add_people(&store, "p", 40);
        let expected = dump(&store.graph().read());
        let before = store.snapshots().unwrap().len();
        dir.budget.store(budget, Ordering::SeqCst);
        let result = store.checkpoint(None);
        if !dir.crashed() {
            result.unwrap();
            assert!(steps > 5, "a checkpoint of many files takes many writes");
            break;
        }
        assert!(result.is_err(), "crash after {budget} writes");
        drop(store);

        let reopened = Store::open(&image, options()).unwrap();
        assert_eq!(dump(&reopened.graph().read()), expected, "budget {budget}");
        assert!(leftovers(&image).is_empty(), "budget {budget}");
        let snapshots = reopened.snapshots().unwrap().len();
        assert!(snapshots == before || snapshots == before + 1);
        assert!(reopened.verify().unwrap().is_ok());
        reopened.checkpoint(None).unwrap();
        reopened.close().unwrap();
    }
}

/// A store's first snapshot (create, fork) interrupted after every write: the directory is
/// either no store at all or a complete one, and a new attempt in a fresh directory succeeds.
#[test]
fn a_first_snapshot_interrupted_after_every_write_leaves_no_half_store() {
    let graph = varied_graph();
    let expected = dump(&graph);
    for budget in 0.. {
        let image = MemDir::new();
        let dir = crashing(&image);
        dir.budget.store(budget, Ordering::SeqCst);
        let shared: Arc<dyn StoreDir> = Arc::new(dir.clone());
        let result = Store::create_with(&shared, varied_graph(), options());
        // A crash may also come after the creation, while the new store opens.
        let crashed = dir.crashed();
        dir.budget.store(i64::MAX, Ordering::SeqCst);
        match result {
            Ok(store) => {
                assert_eq!(dump(&store.graph().read()), expected, "budget {budget}");
                drop(store);
                if !crashed {
                    break;
                }
            }
            Err(_) => assert!(crashed, "an error without a crash after {budget} writes"),
        }
        if Store::is_store(&image).unwrap()
            && let Ok(store) = Store::open(&image, options())
        {
            assert_eq!(dump(&store.graph().read()), expected, "budget {budget}");
            assert!(leftovers(&image).is_empty());
        }
    }

    let source = Store::create_with(MemDir::new(), graph, options()).unwrap();
    add_people(&source, "f", 10);
    let forked_state = dump(&source.graph().read());
    for budget in 0.. {
        let image = MemDir::new();
        let dir = crashing(&image);
        dir.budget.store(budget, Ordering::SeqCst);
        let shared: Arc<dyn StoreDir> = Arc::new(dir.clone());
        let result = source.fork(RecoveryTarget::Latest, &shared);
        let crashed = dir.crashed();
        dir.budget.store(i64::MAX, Ordering::SeqCst);
        if !crashed {
            result.unwrap();
            let fork = Store::open(&image, options()).unwrap();
            assert_eq!(dump(&fork.graph().read()), forked_state);
            break;
        }
        assert!(result.is_err(), "crash after {budget} writes");
        if Store::is_store(&image).unwrap()
            && let Ok(store) = Store::open(&image, options())
        {
            assert_eq!(dump(&store.graph().read()), forked_state);
        }
    }
}

/// A packed snapshot of many segments imported into a store of every backend (copied section
/// by section) and exported again (streamed from the files) is the same stream.
#[test]
fn a_packed_snapshot_survives_import_and_export_on_every_backend() {
    let mut graph = varied_graph();
    for i in 0..300 {
        graph
            .traversal_mut()
            .add_v("item")
            .property("i", i as i64)
            .property("text", "x".repeat(i % 50))
            .to_list()
            .unwrap();
    }
    for codec in [Codec::None, Codec::Lz4] {
        let mut packed = Vec::new();
        let options = WriteOptions::new()
            .with_codec(codec)
            .with_chunk_bytes(300)
            .with_segment_bytes(1500);
        let info = persist::write_snapshot(&graph, &mut packed, &options).unwrap();
        assert!(info.segment_count > 4);
        let mut backends: Vec<(Arc<dyn StoreDir>, Option<std::path::PathBuf>)> =
            vec![(Arc::new(MemDir::new()), None)];
        #[cfg(not(target_arch = "wasm32"))]
        {
            let base = std::env::temp_dir().join(format!(
                "graphersal-streaming-it-{}-{codec:?}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&base);
            std::fs::create_dir_all(&base).unwrap();
            backends.push((
                Arc::new(persist::FsDir::new(base.join("dir"))),
                Some(base.clone()),
            ));
            backends.push((
                Arc::new(persist::SingleFileDir::new(base.join("store.gstore"))),
                Some(base),
            ));
        }
        for (dir, cleanup) in backends {
            let store =
                Store::create_from_packed(&dir, packed.as_slice(), StoreOptions::new()).unwrap();
            assert_eq!(dump(&store.graph().read()), dump(&graph));
            let mut exported = Vec::new();
            store.export_packed(None, &mut exported).unwrap();
            assert!(exported == packed, "{}", dir.location());
            store.close().unwrap();
            if let Some(base) = cleanup {
                let _ = std::fs::remove_dir_all(base);
            }
        }
    }
}

/// A packed snapshot damaged anywhere is refused by the streaming import and leaves no store.
#[test]
fn a_damaged_packed_snapshot_is_refused_and_creates_nothing() {
    let graph = varied_graph();
    let mut packed = Vec::new();
    let options = WriteOptions::new()
        .with_chunk_bytes(64)
        .with_segment_bytes(400);
    persist::write_snapshot(&graph, &mut packed, &options).unwrap();
    let step = (packed.len() / 97).max(1);
    for at in (0..packed.len()).step_by(step) {
        let mut damaged = packed.clone();
        damaged[at] ^= 0x10;
        let dir = MemDir::new();
        let result = Store::create_from_packed(&dir, damaged.as_slice(), StoreOptions::new());
        match result {
            Err(_) => assert!(!Store::is_store(&dir).unwrap(), "byte {at}"),
            // A flip the format cannot see (none expected: every byte is under a CRC).
            Ok(_) => panic!("a flip at byte {at} was accepted"),
        }
    }
    // Cut anywhere: refused too.
    for cut in (0..packed.len()).step_by(step) {
        let dir = MemDir::new();
        assert!(
            Store::create_from_packed(&dir, &packed[..cut], StoreOptions::new()).is_err(),
            "cut at {cut}"
        );
        assert!(!Store::is_store(&dir).unwrap());
    }
}

/// A writer that fails after some bytes (a full disk, a closed pipe) under a packed write that
/// streams: an error naming the write, never a panic, whatever the point.
#[test]
fn a_failing_writer_is_an_error_at_every_point() {
    struct Failing(usize);
    impl io::Write for Failing {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.0 == 0 {
                return Err(io::Error::other("disk full"));
            }
            let n = buf.len().min(self.0);
            self.0 -= n;
            Ok(n)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let graph = varied_graph();
    let options = WriteOptions::new()
        .with_chunk_bytes(64)
        .with_segment_bytes(300);
    let mut full = Vec::new();
    persist::write_snapshot(&graph, &mut full, &options).unwrap();
    for limit in (0..full.len()).step_by(37) {
        let err = persist::write_snapshot(&graph, Failing(limit), &options).unwrap_err();
        assert!(err.to_string().contains("disk full"), "{err}");
    }
    persist::write_snapshot(&graph, Failing(full.len()), &options).unwrap();
}
