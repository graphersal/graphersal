//! When system journal records appear (book page "System Journal", "When records appear"):
//! at once to readers in the process (written or not, no gap, no duplicate), on disk within
//! about a second for a store on a file system (its journal flusher, independent of the system
//! job `stats_flush`), and through `Store::flush_journal` for a store without that thread.
#![cfg(all(feature = "persist", not(target_arch = "wasm32")))]

use std::time::{Duration, Instant};

use graphersal::persist::{MemDir, Store, StoreDir, StoreOptions, store_dir_at};
use graphersal::system_journal::{JournalQuery, JournalRecord};

use crate::model_fixture::Temp;

fn options() -> StoreOptions {
    StoreOptions::new().with_checkpoint_after_bytes(0)
}

fn on_disk(dir: impl graphersal::persist::IntoStoreDir) -> Vec<JournalRecord> {
    Store::journal_dir(dir, &JournalQuery::new().limit(10_000))
        .unwrap()
        .records
}

fn kinds(records: &[JournalRecord]) -> Vec<&str> {
    records.iter().map(|record| record.kind.as_str()).collect()
}

fn in_process(store: &Store) -> Vec<JournalRecord> {
    store
        .journal()
        .unwrap()
        .query(&JournalQuery::new().limit(10_000))
        .unwrap()
        .records
}

/// Polls `found` until it holds or `limit` passes (a condition, never a fixed sleep).
fn within(limit: Duration, mut found: impl FnMut() -> bool) -> bool {
    let started = Instant::now();
    loop {
        if found() {
            return true;
        }
        if started.elapsed() > limit {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn assert_seq_unique_and_ordered(records: &[JournalRecord]) {
    assert!(
        records.windows(2).all(|w| w[0].seq < w[1].seq),
        "{:?}",
        records.iter().map(|r| r.seq).collect::<Vec<_>>()
    );
}

/// A store on a file system (a directory and a single file): a checkpoint's `info` record is
/// read at once in the process and appears on disk for another reader (no lock, as `graphersal
/// store journal` reads a store a server holds open) without `stats_flush` or a flush.
#[test]
fn a_batched_record_reaches_the_disk_by_itself_within_about_a_second() {
    let temp = Temp::new("journal-flush");
    let directory = temp.join("dir-store");
    let single = temp.join("single.gstore");
    let dirs: Vec<std::sync::Arc<dyn StoreDir>> =
        vec![store_dir_at(&directory), store_dir_at(&single)];
    for dir in dirs {
        let store = Store::create(&dir, options()).unwrap();
        let reader = dir.clone();
        // What the open wrote reaches the disk first (so the checkpoint below is batched).
        assert!(
            within(Duration::from_secs(10), || kinds(&on_disk(&reader))
                .contains(&"store.opened")),
            "{}: the open's records reach the disk",
            dir.location()
        );
        store.checkpoint(None).unwrap();
        // In the process: at once, written or not.
        let now = in_process(&store);
        assert!(kinds(&now).contains(&"store.checkpoint_done"), "{now:?}");
        assert_seq_unique_and_ordered(&now);
        // On disk: by itself, well within the bound (10 s for a loaded test machine; the
        // flusher writes 0.5 s after the record).
        assert!(
            within(Duration::from_secs(10), || kinds(&on_disk(&reader))
                .contains(&"store.checkpoint_done")),
            "{}: the checkpoint record reaches the disk without a flush",
            dir.location()
        );
        // After the write: the same records in the process, no duplicate, no gap.
        let after = in_process(&store);
        assert_eq!(
            after.iter().map(|r| r.seq).collect::<Vec<_>>(),
            now.iter().map(|r| r.seq).collect::<Vec<_>>()
        );
        assert_eq!(
            on_disk(&reader).iter().map(|r| r.seq).collect::<Vec<_>>(),
            now.iter().map(|r| r.seq).collect::<Vec<_>>(),
            "the disk holds exactly the records the process shows"
        );
        store.close().unwrap();
    }
}

/// A store in memory has no flusher thread (its records are read from memory; the write
/// counts of fault-injection tests stay deterministic): `Store::flush_journal` writes the
/// batch, which the session's tick calls on wasm32.
#[test]
fn flush_journal_writes_the_batch_of_a_store_without_the_thread() {
    let dir = MemDir::new();
    let store = Store::create(&dir, options()).unwrap();
    store.checkpoint(None).unwrap();
    assert!(kinds(&in_process(&store)).contains(&"store.checkpoint_done"));
    assert!(
        !kinds(&on_disk(&dir)).contains(&"store.checkpoint_done"),
        "batched, not written yet"
    );
    assert!(store.flush_journal().unwrap());
    let written = on_disk(&dir);
    assert!(kinds(&written).contains(&"store.checkpoint_done"));
    assert_seq_unique_and_ordered(&written);
    assert_eq!(
        written.iter().map(|r| r.seq).collect::<Vec<_>>(),
        in_process(&store).iter().map(|r| r.seq).collect::<Vec<_>>()
    );
    store.shutdown().unwrap();
    // A closed store writes nothing more.
    assert!(!store.flush_journal().unwrap());
}

/// The `store.backup_done` text names the backup with the right article.
#[test]
fn the_backup_record_names_a_full_and_an_incremental_backup() {
    let dir = MemDir::new();
    let store = Store::create(&dir, options()).unwrap();
    let backup = MemDir::new();
    store.backup(&backup).unwrap();
    store.backup(&backup).unwrap();
    let messages: Vec<String> = in_process(&store)
        .into_iter()
        .filter(|r| r.kind == "store.backup_done")
        .map(|r| r.message)
        .collect();
    assert_eq!(messages.len(), 2, "{messages:?}");
    assert!(messages[0].starts_with("a full backup to "), "{messages:?}");
    assert!(
        messages[1].starts_with("an incremental backup to "),
        "{messages:?}"
    );
    store.close().unwrap();
}
