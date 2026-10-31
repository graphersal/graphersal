//! Blob recovery (format spec 4.6; book page "Blobs", "Damage"): `Store::restore_blobs_from`
//! copies the missing or damaged blobs a closed store references back from a backup, a ZIP
//! backup, a packed snapshot or another store, never touching a healthy blob; a repair with
//! `RepairOptions::with_drop_damaged_blobs` leaves the definitions of such blobs out.
#![cfg(feature = "persist")]

use graphersal::blob::{BlobHashAlgo, BlobRef};
use graphersal::persist::{
    BlobSource, MemDir, PersistError, RepairOptions, Store, StoreDir, StoreFailure, StoreOptions,
};
use graphersal::prelude::*;
use graphersal::system_journal::JournalQuery;

use crate::blob_store_tests::{define, name_of, new_store, write};

/// A closed store with blob `a` in its snapshot and blob `b` referenced in the WAL after it.
fn source() -> (MemDir, BlobRef, BlobRef) {
    let (dir, store) = new_store();
    let a = write(&store, b"blob in the snapshot").blob().clone();
    define(&store, "a", &[&a]);
    store.checkpoint(None).unwrap();
    let b = write(&store, b"blob in the wal").blob().clone();
    define(&store, "b", &[&b]);
    store.close().unwrap();
    (dir, a, b)
}

/// Damages `blob` in `dir`: removed (`missing`) or a byte flipped.
fn damage(dir: &MemDir, blob: &BlobRef, missing: bool) {
    let name = name_of(blob);
    if missing {
        dir.remove_file(&name).unwrap();
    } else {
        let mut bytes = dir.read(&name, u64::MAX).unwrap();
        bytes[0] ^= 0x20;
        dir.write_atomic(&name, &bytes).unwrap();
    }
}

fn intact(dir: &MemDir, blobs: &[&BlobRef]) {
    let store = Store::open(dir, StoreOptions::new()).unwrap();
    for blob in blobs {
        assert_eq!(store.read_blob(blob).unwrap().len() as u64, blob.length());
    }
    assert!(store.verify().unwrap().is_ok(), "{:?}", store.verify());
    store.close().unwrap();
}

#[test]
fn restore_from_every_source_kind() {
    let (dir, a, b) = source();
    // The sources, made while every blob is intact.
    let backup = MemDir::new();
    Store::backup_dir(&dir, &backup).unwrap();
    let other = MemDir::new();
    Store::fork_dir(&dir, graphersal::persist::RecoveryTarget::Latest, &other).unwrap();
    let mut packed = Vec::new();
    {
        let store = Store::open(&dir, StoreOptions::new()).unwrap();
        store.checkpoint(None).unwrap();
        store.export_packed(None, &mut packed).unwrap();
        store.close().unwrap();
    }
    #[cfg(feature = "persist-zip")]
    let zip = {
        let mut zip = std::io::Cursor::new(Vec::new());
        let store = Store::open(&dir, StoreOptions::new()).unwrap();
        store.backup_zip(&mut zip).unwrap();
        store.close().unwrap();
        zip.into_inner()
    };
    let mut sources: Vec<(&str, Box<dyn Fn() -> BlobSource>)> = vec![
        ("backup", Box::new(|| BlobSource::store(backup.clone()))),
        ("other store", Box::new(|| BlobSource::store(other.clone()))),
        (
            "packed",
            Box::new(|| BlobSource::packed(std::io::Cursor::new(packed.clone()), "x.gsnap")),
        ),
    ];
    #[cfg(feature = "persist-zip")]
    sources.push((
        "zip",
        Box::new(|| BlobSource::zip(std::io::Cursor::new(zip.clone()), "x.zip")),
    ));
    for (what, make) in &sources {
        for missing in [true, false] {
            let target = dir.deep_copy();
            damage(&target, &a, missing);
            damage(&target, &b, !missing);
            let report = Store::restore_blobs_from(&target, make()).unwrap();
            assert_eq!(report.referenced, 2, "{what}");
            assert_eq!(report.wanted.len(), 2, "{what}");
            assert!(report.is_complete(), "{what}: {report:?}");
            assert_eq!(report.restored.len(), 2, "{what}");
            assert_eq!(report.bytes_restored, a.length() + b.length(), "{what}");
            intact(&target, &[&a, &b]);
            let journal = Store::journal_dir(&target, &JournalQuery::new()).unwrap();
            let restored: Vec<_> = journal
                .records
                .iter()
                .filter(|r| {
                    r.kind == "blob.repaired"
                        && r.field("how") == Some(&ElementProperty::from("restored"))
                })
                .collect();
            assert_eq!(restored.len(), 2, "{what}");
        }
    }
}

#[test]
fn an_increment_is_a_source_too() {
    let (dir, a, _) = source();
    let backup = MemDir::new();
    Store::backup_dir(&dir, &backup).unwrap();
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    let c = write(&store, b"blob after the full backup").blob().clone();
    define(&store, "c", &[&c]);
    store.close().unwrap();
    // The increment brings c into the backup.
    Store::backup_dir(&dir, &backup).unwrap();
    damage(&dir, &c, true);
    damage(&dir, &a, false);
    let report = Store::restore_blobs_from(&dir, BlobSource::store(backup)).unwrap();
    assert!(report.is_complete(), "{report:?}");
    assert_eq!(report.restored.len(), 2);
    intact(&dir, &[&a, &c]);
}

#[test]
fn a_healthy_blob_is_never_touched_and_missing_sources_are_reported() {
    let (dir, a, b) = source();
    // A source that holds `a` damaged and lacks `b`.
    let poor = dir.deep_copy();
    damage(&poor, &a, false);
    damage(&poor, &b, true);
    let report = Store::restore_blobs_from(&dir, BlobSource::store(poor.clone())).unwrap();
    assert!(report.wanted.is_empty() && report.restored.is_empty());
    assert!(report.is_complete());
    damage(&dir, &a, true);
    damage(&dir, &b, false);
    let before_b = dir.read(&name_of(&b), u64::MAX).unwrap();
    let report = Store::restore_blobs_from(&dir, BlobSource::store(poor)).unwrap();
    assert!(!report.is_complete());
    assert_eq!(report.damaged_in_source, vec![name_of(&a)]);
    assert_eq!(report.not_found, vec![name_of(&b)]);
    assert!(report.restored.is_empty());
    assert!(dir.read(&name_of(&a), u64::MAX).is_err());
    assert_eq!(dir.read(&name_of(&b), u64::MAX).unwrap(), before_b);
}

#[test]
fn a_blob_of_an_unknown_algorithm_is_restored_by_name() {
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
    dir.create_dir_all("blobs/algo-7/c4").unwrap();
    dir.write_atomic(&name, &content).unwrap();
    define(&store, "m", &[&foreign]);
    store.close().unwrap();
    let copy = dir.deep_copy();
    dir.remove_file(&name).unwrap();
    let report = Store::restore_blobs_from(&dir, BlobSource::store(copy)).unwrap();
    assert!(report.is_complete(), "{report:?}");
    assert_eq!(report.restored, vec![name.clone()]);
    assert_eq!(dir.read(&name, u64::MAX).unwrap(), content);
}

#[test]
fn restore_needs_a_closed_live_store() {
    let (dir, _, _) = source();
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    let err = Store::restore_blobs_from(&dir, BlobSource::store(dir.deep_copy())).unwrap_err();
    assert!(matches!(err, PersistError::Locked { .. }), "{err}");
    let backup = MemDir::new();
    store.backup(&backup).unwrap();
    store.close().unwrap();
    let err = Store::restore_blobs_from(&backup, BlobSource::store(dir.clone())).unwrap_err();
    assert!(matches!(err, PersistError::IsBackup { .. }), "{err}");
}

#[test]
fn a_repair_can_drop_the_definitions_of_damaged_blobs() {
    let (dir, a, b) = source();
    damage(&dir, &a, true);
    // Without the flag: carried, its blob reported as not carried.
    let plain = MemDir::new();
    let report = Store::repair_dir(&dir, &plain).unwrap();
    assert_eq!(report.blobs_not_carried, vec![name_of(&a)]);
    assert!(report.blobs_dropped.is_empty() && report.definitions_dropped.is_empty());
    // With it: left out, reported, journaled; the repaired store verifies.
    let repaired = MemDir::new();
    let report = Store::repair_dir_with(
        &dir,
        &repaired,
        RepairOptions::new().with_drop_damaged_blobs(true),
    )
    .unwrap();
    assert_eq!(report.blobs_dropped, vec![name_of(&a)]);
    assert_eq!(report.definitions_dropped.len(), 1);
    assert!(report.definitions_dropped[0].ends_with(":a"), "{report:?}");
    assert!(report.blobs_not_carried.is_empty(), "{report:?}");
    let store = Store::open(&repaired, StoreOptions::new()).unwrap();
    let names: Vec<String> = store
        .graph()
        .read()
        .definitions()
        .unwrap()
        .map(|d| d.name().to_string())
        .collect();
    assert_eq!(names, vec!["b".to_string()]);
    assert_eq!(store.read_blob(&b).unwrap().len() as u64, b.length());
    assert!(store.verify().unwrap().is_ok());
    store.close().unwrap();
    let journal = Store::journal_dir(&repaired, &JournalQuery::new()).unwrap();
    let dropped: Vec<_> = journal
        .records
        .iter()
        .filter(|r| r.kind == "blob.dropped")
        .collect();
    assert_eq!(dropped.len(), 1);
    assert_eq!(
        dropped[0].field("hash"),
        Some(&ElementProperty::from(a.hash_hex()))
    );
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_repair_does_not_drop_a_model_the_schema_needs() {
    use crate::model_fixture::{Temp, import, options, set_schema};
    let temp = Temp::new("drop-needed");
    let path = temp.join("s");
    let store = Store::create(&path, options()).unwrap();
    let model = import(&store, 1, "acme/tiny").model;
    set_schema(&store, &model.identity);
    store.close().unwrap();
    for entry in std::fs::read_dir(path.join("blobs")).unwrap().flatten() {
        if entry.path().is_dir() {
            std::fs::remove_dir_all(entry.path()).unwrap();
        }
    }
    let to = temp.join("r");
    let err = Store::repair_dir_with(
        &path,
        &to,
        RepairOptions::new().with_drop_damaged_blobs(true),
    )
    .unwrap_err();
    let PersistError::Store { cause, reason, .. } = &err else {
        panic!("{err}");
    };
    assert_eq!(*cause, StoreFailure::BlobUnavailable);
    assert!(reason.contains("doc.emb"), "{reason}");
    assert!(reason.contains("restore-blobs"), "{reason}");
    assert!(!to.exists() || std::fs::read_dir(&to).unwrap().next().is_none());
}
