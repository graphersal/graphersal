//! Blob collection (format spec 12.4, 12.5) and the blob ledger (10.1): a blob stays exactly
//! as long as some retained history references it; prune, attic removal and rollback `delete`
//! remove the others (orphans included); the derived ledger is rebuilt when lost or damaged;
//! the manifest's blob table (6.2 item 5) carries the references across checkpoints.
#![cfg(feature = "persist")]

use graphersal::persist::{RecoveryTarget, RollbackMode, Store, StoreDir, StoreOptions};

use crate::blob_store_tests::{blob_files, define, name_of, new_store, touch, undefine, write};

fn position(store: &Store) -> u64 {
    store.graph().read().last_commit_seq()
}

#[test]
fn prune_keeps_every_referenced_blob_and_removes_orphans() {
    let (dir, store) = new_store();
    let kept = write(&store, b"kept").blob().clone();
    define(&store, "m", &[&kept]);
    // Written, never referenced, the handle dropped: an orphan.
    let orphan = write(&store, b"orphan").blob().clone();
    store.checkpoint(None).unwrap();
    touch(&store, "a");
    store.checkpoint(None).unwrap();
    let report = store.prune(position(&store)).unwrap();
    assert_eq!(report.blobs_removed, 1);
    assert_eq!(report.blob_bytes_removed, 6);
    assert_eq!(blob_files(&dir), vec![name_of(&kept)]);
    assert!(store.read_blob(&orphan).is_err());
    assert_eq!(store.read_blob(&kept).unwrap(), b"kept");
    let kinds = crate::system_journal_store_tests::kinds(
        &store
            .journal()
            .unwrap()
            .query(&graphersal::system_journal::JournalQuery::new())
            .unwrap()
            .records,
    );
    assert!(kinds.iter().any(|k| k == "blob.collected"), "{kinds:?}");
}

#[test]
fn a_pending_blob_survives_a_prune_until_its_handle_goes() {
    let (dir, store) = new_store();
    let pending = write(&store, b"pending");
    store.checkpoint(None).unwrap();
    touch(&store, "a");
    store.checkpoint(None).unwrap();
    assert_eq!(store.prune(position(&store)).unwrap().blobs_removed, 0);
    assert_eq!(blob_files(&dir).len(), 1);
    drop(pending);
    assert_eq!(store.prune(position(&store)).unwrap().blobs_removed, 1);
    assert!(blob_files(&dir).is_empty());
}

#[test]
fn a_blob_goes_when_no_retained_history_references_it_any_more() {
    let (dir, store) = new_store();
    let a = write(&store, b"aaaa").blob().clone();
    let shared = write(&store, b"shared").blob().clone();
    define(&store, "m1", &[&a, &shared]);
    define(&store, "m2", &[&shared]);
    store.checkpoint(None).unwrap();
    undefine(&store, "m1");
    // History still references `a` (the snapshot and the ledger): kept.
    assert_eq!(store.prune(position(&store)).unwrap().blobs_removed, 0);
    store.checkpoint(None).unwrap();
    touch(&store, "b");
    store.checkpoint(None).unwrap();
    // Pruned up to the latest: the kept snapshots and the WAL after them no longer reference
    // `a`; `shared` is still referenced by m2.
    let report = store.prune(position(&store)).unwrap();
    assert_eq!(report.blobs_removed, 1, "{report:?}");
    assert_eq!(blob_files(&dir), vec![name_of(&shared)]);
    store.close().unwrap();
    // Reopened: the same.
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    assert_eq!(store.read_blob(&shared).unwrap(), b"shared");
}

#[test]
fn the_blob_table_carries_references_across_a_merged_checkpoint_without_the_ledger() {
    let (dir, store) = new_store();
    let blob = write(&store, b"in the table").blob().clone();
    define(&store, "m", &[&blob]);
    store.checkpoint(None).unwrap();
    touch(&store, "x");
    store.checkpoint(None).unwrap();
    // The ledger is derived: without it the snapshot tables still keep the blob.
    dir.remove_file("ledger").unwrap();
    store.close().unwrap();
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    assert_eq!(store.prune(position(&store)).unwrap().blobs_removed, 0);
    assert_eq!(store.read_blob(&blob).unwrap(), b"in the table");
    assert!(store.verify().unwrap().is_ok());
}

#[test]
fn a_lost_or_damaged_ledger_is_rebuilt_from_the_wal() {
    let (dir, store) = new_store();
    let blob = write(&store, b"ledgered").blob().clone();
    define(&store, "m", &[&blob]);
    store.close().unwrap();
    let ledger = dir.read("ledger", u64::MAX).unwrap();
    assert!(!ledger.is_empty());
    // Lost.
    dir.remove_file("ledger").unwrap();
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    assert!(
        store
            .open_report()
            .warnings
            .iter()
            .any(|w| w.contains("blob ledger was missing")),
        "{:?}",
        store.open_report().warnings
    );
    store.close().unwrap();
    assert_eq!(dir.read("ledger", u64::MAX).unwrap(), ledger);
    // Damaged.
    let mut damaged = ledger.clone();
    let last = damaged.len() - 1;
    damaged[last] ^= 0x40;
    dir.write_atomic("ledger", &damaged).unwrap();
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    assert!(
        store
            .open_report()
            .warnings
            .iter()
            .any(|w| w.contains("blob ledger was damaged"))
    );
    assert!(!store.is_frozen_by_damage());
    store.close().unwrap();
    assert_eq!(dir.read("ledger", u64::MAX).unwrap(), ledger);
}

#[test]
fn a_store_without_blobs_writes_no_ledger() {
    let (dir, store) = new_store();
    touch(&store, "a");
    store.close().unwrap();
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    store.close().unwrap();
    assert!(dir.read("ledger", u64::MAX).is_err());
}

#[test]
fn rollback_delete_collects_the_blobs_of_the_deleted_history() {
    let (dir, store) = new_store();
    let before = write(&store, b"before").blob().clone();
    define(&store, "m0", &[&before]);
    let target = position(&store);
    let after = write(&store, b"after").blob().clone();
    define(&store, "m1", &[&after]);
    store
        .rollback_to(RecoveryTarget::CommitSeq(target), RollbackMode::Delete)
        .unwrap();
    assert_eq!(blob_files(&dir), vec![name_of(&before)]);
    assert!(store.read_blob(&after).is_err());
}

#[test]
fn rollback_move_keeps_the_attic_blobs_until_the_entry_goes() {
    let (dir, store) = new_store();
    touch(&store, "a");
    let target = position(&store);
    let moved = write(&store, b"moved").blob().clone();
    define(&store, "m", &[&moved]);
    store
        .rollback_to(RecoveryTarget::CommitSeq(target), RollbackMode::Move)
        .unwrap();
    // The attic entry's history references it: kept by a prune.
    store.checkpoint(None).unwrap();
    store.prune(position(&store)).unwrap();
    assert_eq!(blob_files(&dir), vec![name_of(&moved)]);
    let entry = store.attic_list().unwrap()[0].id.clone();
    store.attic_remove(&entry).unwrap();
    assert!(blob_files(&dir).is_empty());
}

#[test]
fn a_rolled_back_store_still_references_the_blobs_before_the_target() {
    let (dir, store) = new_store();
    let early = write(&store, b"early").blob().clone();
    define(&store, "m", &[&early]);
    let target = position(&store);
    undefine(&store, "m");
    store
        .rollback_to(RecoveryTarget::CommitSeq(target), RollbackMode::Delete)
        .unwrap();
    // The removal was rolled back: the current state references `early` again.
    assert_eq!(blob_files(&dir), vec![name_of(&early)]);
    store.checkpoint(None).unwrap();
    touch(&store, "z");
    store.checkpoint(None).unwrap();
    store.prune(position(&store)).unwrap();
    assert_eq!(store.read_blob(&early).unwrap(), b"early");
    let _ = dir.list("blobs");
}
