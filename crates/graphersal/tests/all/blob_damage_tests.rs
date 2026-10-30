//! A damaged or missing blob is never store damage (format spec 4.6, 13): the store stays
//! writable, the damage policy is untouched, `verify` lists the blob and the system journal
//! gets a `blob.damaged` error; foreign blobs (a ZIP archive) are checked by their hash.
#![cfg(feature = "persist")]

use graphersal::persist::{Store, StoreDir, StoreOptions};
use graphersal::system_journal::JournalQuery;

use crate::blob_store_tests::{define, name_of, new_store, touch, write};

#[test]
fn a_damaged_blob_is_a_verify_problem_and_a_journal_error_never_store_damage() {
    let (dir, store) = new_store();
    let blob = write(&store, b"some model weights").blob().clone();
    define(&store, "m", &[&blob]);
    let orphan = write(&store, b"unreferenced").blob().clone();
    drop(orphan);
    // Same length, other bytes.
    dir.write_atomic(&name_of(&blob), b"some model weightX")
        .unwrap();
    // A stray entry under blobs/.
    dir.write_atomic("blobs/README", b"not a blob").unwrap();
    let report = store.verify().unwrap();
    assert_eq!(report.blobs_checked, 1);
    let paths: Vec<&str> = report.problems.iter().map(|p| p.path.as_str()).collect();
    assert!(paths.contains(&name_of(&blob).as_str()), "{paths:?}");
    assert!(paths.contains(&"blobs/README"), "{paths:?}");
    assert!(
        report
            .notes
            .iter()
            .any(|n| n.contains("no retained history references")),
        "{:?}",
        report.notes
    );
    // Never store damage: nothing frozen, nothing found, the store keeps committing.
    assert!(!store.is_frozen_by_damage());
    assert!(store.damage_found().unwrap().is_empty());
    touch(&store, "still writable");
    let records = store
        .journal()
        .unwrap()
        .query(&JournalQuery::new())
        .unwrap()
        .records;
    let damaged: Vec<_> = records
        .iter()
        .filter(|r| r.kind == "blob.damaged")
        .collect();
    assert_eq!(damaged.len(), 1, "{records:?}");
    let text = format!("{:?}", damaged[0]);
    assert!(text.contains(&blob.hash_hex()), "{text}");
    assert!(text.contains("crc"), "{text}");
    store.close().unwrap();
    // The open is not refused either.
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    assert!(store.maintenance().is_none());
}

#[test]
fn a_missing_blob_is_listed_by_verify() {
    let (dir, store) = new_store();
    let blob = write(&store, b"gone").blob().clone();
    define(&store, "m", &[&blob]);
    dir.remove_file(&name_of(&blob)).unwrap();
    let report = store.verify().unwrap();
    assert_eq!(report.problems.len(), 1);
    assert_eq!(report.problems[0].path, name_of(&blob));
    assert!(report.problems[0].reason.contains("missing"));
    touch(&store, "still writable");
}

#[cfg(feature = "persist-zip")]
#[test]
fn a_zip_blob_whose_content_is_not_its_hash_is_refused() {
    use graphersal::persist::MemDir;
    let (dir, store) = new_store();
    let blob = write(&store, b"genuine content").blob().clone();
    define(&store, "m", &[&blob]);
    // The store's own copy is replaced by other bytes of the same length before the backup:
    // the backup leaves it out (a warning), so build the hostile archive by hand instead: a
    // backup of an intact store, then the blob entry's bytes swapped in the archive.
    let mut zip = std::io::Cursor::new(Vec::new());
    store.backup_zip(&mut zip).unwrap();
    let mut bytes = zip.into_inner();
    let at = bytes
        .windows(b"genuine content".len())
        .position(|w| w == b"genuine content")
        .unwrap();
    bytes[at] ^= 1;
    let target = MemDir::new();
    assert!(Store::restore_zip(bytes.as_slice(), &target).is_err());
    let _ = dir.list("blobs");
}
