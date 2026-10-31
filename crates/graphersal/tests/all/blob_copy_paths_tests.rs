//! Every copy of a store carries the blobs its history references (format spec 4.6, 12.5-12.11,
//! 13.4), checked by length and CRC-32C, and `BlobCopy::Exclude` leaves them out of the
//! shareable copies (ZIP, `.gsnap`). Rule 22 style: every copy path is tried.
#![cfg(feature = "persist")]

use graphersal::blob::{BlobCopy, BlobRef};
use graphersal::persist::{
    BackupMode, MemDir, RecoveryTarget, RollbackMode, Store, StoreDir, StoreOptions,
};

use crate::blob_store_tests::{blob_files, define, name_of, new_store, touch, write};

/// A store with blob `a` referenced at a snapshot and blob `b` referenced in the WAL after it.
fn source() -> (MemDir, Store, BlobRef, BlobRef) {
    let (dir, store) = new_store();
    let a = write(&store, b"blob in the snapshot").blob().clone();
    define(&store, "a", &[&a]);
    store.checkpoint(None).unwrap();
    let b = write(&store, b"blob in the wal").blob().clone();
    define(&store, "b", &[&b]);
    (dir, store, a, b)
}

/// The copy in `dir` holds `blobs`, readable through a store opened on it.
fn holds(dir: &MemDir, blobs: &[&BlobRef], backup: bool) {
    let mut expected: Vec<String> = blobs.iter().map(|b| name_of(b)).collect();
    expected.sort();
    assert_eq!(blob_files(dir), expected);
    let store = if backup {
        Store::open_backup(dir, StoreOptions::new()).unwrap()
    } else {
        Store::open(dir, StoreOptions::new()).unwrap()
    };
    for blob in blobs {
        assert_eq!(store.read_blob(blob).unwrap().len() as u64, blob.length());
    }
    assert!(store.verify().unwrap().is_ok(), "{:?}", store.verify());
}

#[test]
fn every_copy_path_carries_the_blobs() {
    let (dir, store, a, b) = source();
    let latest = store.graph().read().last_commit_seq();

    // Fork at the latest position: the state references both.
    let fork = MemDir::new();
    let report = store.fork(RecoveryTarget::Latest, &fork).unwrap();
    assert_eq!(report.blobs.len(), 2);
    assert!(report.blobs_not_carried.is_empty());
    holds(&fork, &[&a, &b], false);

    // Full backup, then an increment that adds a blob.
    let backup = MemDir::new();
    let full = store.backup(&backup).unwrap();
    assert_eq!(full.blobs.len(), 2);
    holds(&backup, &[&a, &b], true);
    let c = write(&store, b"blob of the increment").blob().clone();
    define(&store, "c", &[&c]);
    let increment = store.backup_with(&backup, BackupMode::Auto).unwrap();
    assert_eq!(increment.blobs, vec![name_of(&c)]);
    holds(&backup, &[&a, &b, &c], true);

    // Restore of a backup directory: in place.
    let restored = MemDir::new();
    store.backup(&restored).unwrap();
    Store::restore_backup(&restored).unwrap();
    holds(&restored, &[&a, &b, &c], false);

    // ZIP and its restore.
    #[cfg(feature = "persist-zip")]
    {
        let mut zip = std::io::Cursor::new(Vec::new());
        let report = store.backup_zip(&mut zip).unwrap();
        assert_eq!(report.blobs.len(), 3);
        let unpacked = MemDir::new();
        Store::restore_zip(zip.into_inner().as_slice(), &unpacked).unwrap();
        holds(&unpacked, &[&a, &b, &c], false);
    }

    // `.gsnap` export of the latest snapshot and a store from it.
    store.checkpoint(None).unwrap();
    let mut packed = Vec::new();
    store.export_packed(None, &mut packed).unwrap();
    let from_packed = MemDir::new();
    Store::create_from_packed(&from_packed, packed.as_slice(), StoreOptions::new())
        .unwrap()
        .close()
        .unwrap();
    holds(&from_packed, &[&a, &b, &c], false);
    // The packed file loads without a store (the blob rows are skipped).
    let loaded = graphersal::persist::read_snapshot(
        packed.as_slice(),
        &graphersal::persist::ReadOptions::new(),
    )
    .unwrap();
    assert_eq!(
        loaded.last_commit_seq(),
        store.graph().read().last_commit_seq()
    );

    // Convert (name by name).
    let converted = MemDir::new();
    store.close().unwrap();
    Store::convert(&dir, &converted).unwrap();
    holds(&converted, &[&a, &b, &c], false);

    // Attic fork: the moved history's state.
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    store
        .rollback_to(RecoveryTarget::CommitSeq(latest), RollbackMode::Move)
        .unwrap();
    let entry = store.attic_list().unwrap()[0].id.clone();
    let attic_fork = MemDir::new();
    let report = store.attic_fork(&entry, &attic_fork).unwrap();
    assert!(report.blobs_not_carried.is_empty());
    holds(&attic_fork, &[&a, &b, &c], false);
}

#[test]
fn a_fork_carries_only_its_target_states_blobs() {
    let (_dir, store, a, _b) = source();
    let snapshot = store.snapshots().unwrap()[1].commit_seq;
    let fork = MemDir::new();
    store
        .fork(RecoveryTarget::CommitSeq(snapshot), &fork)
        .unwrap();
    holds(&fork, &[&a], false);
}

#[test]
fn the_exclude_option_leaves_the_blobs_out_of_shareable_copies() {
    let (_dir, store, a, b) = source();
    store.checkpoint(None).unwrap();
    let mut packed = Vec::new();
    store
        .export_packed_with(None, &mut packed, BlobCopy::Exclude)
        .unwrap();
    let from_packed = MemDir::new();
    let created =
        Store::create_from_packed(&from_packed, packed.as_slice(), StoreOptions::new()).unwrap();
    assert!(blob_files(&from_packed).is_empty());
    // The catalog still references them: missing, never store damage.
    assert!(created.read_blob(&a).is_err());
    assert!(created.read_blob(&b).is_err());
    let verify = created.verify().unwrap();
    assert!(verify.problems.iter().all(|p| p.path.starts_with("blobs/")));
    assert_eq!(verify.problems.len(), 2);
    #[cfg(feature = "persist-zip")]
    {
        let mut zip = std::io::Cursor::new(Vec::new());
        store.backup_zip_with(&mut zip, BlobCopy::Exclude).unwrap();
        let unpacked = MemDir::new();
        Store::restore_zip(zip.into_inner().as_slice(), &unpacked).unwrap();
        assert!(blob_files(&unpacked).is_empty());
    }
}

#[test]
fn a_backup_from_memory_and_a_repair_carry_the_blobs() {
    let (dir, store, a, b) = source();
    touch(&store, "before");
    store.checkpoint(None).unwrap();
    touch(&store, "after");
    // Damage a segment file of the newest snapshot: verify finds it while the store is open.
    let snapshot = store.snapshots().unwrap().last().unwrap().commit_seq;
    let snapshot_dir = format!("snapshots/{snapshot:020}");
    let seg = dir
        .list(&snapshot_dir)
        .unwrap()
        .into_iter()
        .map(|entry| format!("{snapshot_dir}/{}", entry.name))
        .find(|name| name.ends_with(".seg"))
        .unwrap();
    let mut bytes = dir.read(&seg, u64::MAX).unwrap();
    let at = bytes.len() / 2;
    bytes[at] ^= 0x55;
    dir.write_atomic(&seg, &bytes).unwrap();
    assert!(!store.verify().unwrap().is_ok());
    let from_memory = MemDir::new();
    let report = store
        .backup_from_memory(&from_memory, BackupMode::Auto)
        .unwrap();
    assert!(report.blobs_not_carried.is_empty(), "{report:?}");
    holds(&from_memory, &[&a, &b], true);
    store.close().unwrap();
    let repaired = MemDir::new();
    let report = Store::repair_dir(&dir, &repaired).unwrap();
    assert!(report.blobs_not_carried.is_empty());
    holds(&repaired, &[&a, &b], false);
}

#[test]
fn a_hostile_packed_blob_is_refused() {
    let (_dir, store, _a, b) = source();
    store.checkpoint(None).unwrap();
    let mut packed = Vec::new();
    store.export_packed(None, &mut packed).unwrap();
    // The blob's bytes changed (its row's CRC no longer matches): damage of the archive.
    let content = b"blob in the wal";
    let at = packed
        .windows(content.len())
        .position(|w| w == content)
        .unwrap();
    packed[at] ^= 1;
    let target = MemDir::new();
    assert!(Store::create_from_packed(&target, packed.as_slice(), StoreOptions::new()).is_err());
    assert!(blob_files(&target).is_empty());
    let _ = b;
    let _: &dyn StoreDir = &target;
}

/// A blob of a newer version's hash algorithm (format spec 3 tag 21, 4.6): never read nor
/// hashed by this version, but carried BY NAME (`blobs/algo-<n>/<2 hex>/<hash>`) by every copy,
/// checked by length and CRC-32C.
#[test]
fn every_copy_path_carries_a_blob_of_an_unknown_algorithm_by_name() {
    use graphersal::blob::BlobHashAlgo;
    let (dir, store) = new_store();
    let content = b"bytes named by a newer hash algorithm".to_vec();
    let foreign = BlobRef::new(
        BlobHashAlgo::Unknown(7),
        &[0xc4, 0x01, 0x02, 0x03, 0x04, 0x05],
        content.len() as u64,
        crate::store_tests::crc32c(&content),
    )
    .unwrap();
    let name = format!("blobs/{}", foreign.relative_path());
    assert_eq!(name, "blobs/algo-7/c4/c40102030405");
    dir.create_dir_all("blobs/algo-7/c4").unwrap();
    dir.write_atomic(&name, &content).unwrap();
    let known = write(&store, b"a known one").blob().clone();
    define(&store, "m", &[&foreign, &known]);
    store.checkpoint(None).unwrap();
    // Never read: unsupported, not damage; verify checks its length and CRC-32C.
    assert!(store.read_blob(&foreign).is_err());
    let report = store.verify().unwrap();
    assert!(report.is_ok(), "{:?}", report.problems);
    assert_eq!(report.blobs_checked, 2);

    let carries = |copy: &MemDir, what: &str| {
        assert_eq!(
            copy.read(&name, u64::MAX)
                .unwrap_or_else(|err| panic!("{what}: {err}")),
            content,
            "{what}"
        );
        assert!(blob_files(copy).contains(&name_of(&known)), "{what}");
    };

    let fork = MemDir::new();
    let report = store.fork(RecoveryTarget::Latest, &fork).unwrap();
    assert!(report.blobs_not_carried.is_empty(), "{report:?}");
    carries(&fork, "fork");

    let backup = MemDir::new();
    let report = store.backup(&backup).unwrap();
    assert!(report.blobs_not_carried.is_empty(), "{report:?}");
    carries(&backup, "backup");
    assert!(
        Store::open_backup(&backup, StoreOptions::new())
            .unwrap()
            .verify()
            .unwrap()
            .is_ok()
    );

    #[cfg(feature = "persist-zip")]
    {
        let mut zip = std::io::Cursor::new(Vec::new());
        let report = store.backup_zip(&mut zip).unwrap();
        assert!(report.blobs.contains(&name), "{report:?}");
        let unpacked = MemDir::new();
        Store::restore_zip(zip.into_inner().as_slice(), &unpacked).unwrap();
        carries(&unpacked, "zip");
    }

    let mut packed = Vec::new();
    store.export_packed(None, &mut packed).unwrap();
    let from_packed = MemDir::new();
    Store::create_from_packed(&from_packed, packed.as_slice(), StoreOptions::new())
        .unwrap()
        .close()
        .unwrap();
    carries(&from_packed, "gsnap");

    touch(&store, "after");
    let latest = store.graph().read().last_commit_seq();
    store
        .rollback_to(RecoveryTarget::CommitSeq(latest - 1), RollbackMode::Move)
        .unwrap();
    let entry = store.attic_list().unwrap()[0].id.clone();
    let attic_fork = MemDir::new();
    let report = store.attic_fork(&entry, &attic_fork).unwrap();
    assert!(report.blobs_not_carried.is_empty(), "{report:?}");
    carries(&attic_fork, "attic fork");
    store.close().unwrap();

    let repaired = MemDir::new();
    let report = Store::repair_dir(&dir, &repaired).unwrap();
    assert!(report.blobs_not_carried.is_empty(), "{report:?}");
    carries(&repaired, "repair");

    let converted = MemDir::new();
    Store::convert(&dir, &converted).unwrap();
    carries(&converted, "convert");
}
