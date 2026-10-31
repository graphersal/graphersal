//! The blob usage of `Store::info` and `Store::quick_info` (format spec 4.6, 12.4): `info`
//! counts the blob files no retained history references (the snapshots' blob tables, the
//! ledger, and the attic entries' own WAL, read only for a blob nothing else references);
//! `quick_info` lists them by names and lengths and counts none.
#![cfg(feature = "persist")]

use graphersal::persist::{MemDir, RecoveryTarget, RollbackMode, Store, StoreOptions};

use crate::blob_store_tests::{define, touch, write};

#[test]
fn info_counts_a_blob_only_the_attic_references_as_referenced_and_an_orphan_as_not() {
    let store = Store::create(MemDir::new(), StoreOptions::new()).unwrap();
    let kept = write(&store, b"kept by the live catalog");
    define(&store, "kept", &[kept.blob()]);
    drop(kept);
    touch(&store, "a");
    store.mark("m").unwrap();
    let aside = write(
        &store,
        b"referenced only by the history a rollback moves aside",
    );
    define(&store, "aside", &[aside.blob()]);
    drop(aside);
    touch(&store, "b");
    let report = store
        .rollback_to(RecoveryTarget::Mark("m".into()), RollbackMode::Move)
        .unwrap();
    let entry = report.attic.expect("an attic entry");

    // Both blobs are referenced: one by the live history, one by the attic entry's WAL.
    let info = store.info().unwrap();
    assert_eq!((info.blobs.count, info.blobs.unreferenced), (2, 0));
    let quick = store.quick_info().unwrap();
    assert_eq!((quick.blobs.count, quick.blobs.unreferenced), (2, 0));
    assert_eq!(quick.blobs.bytes, info.blobs.bytes);

    // An orphan (no commit references it) is counted by `info` only.
    drop(write(&store, b"an orphan"));
    let info = store.info().unwrap();
    assert_eq!((info.blobs.count, info.blobs.unreferenced), (3, 1));
    let quick = store.quick_info().unwrap();
    assert_eq!((quick.blobs.count, quick.blobs.unreferenced), (3, 0));

    // Removing the attic entry collects what only it and nothing referenced.
    store.attic_remove(&entry).unwrap();
    let info = store.info().unwrap();
    assert_eq!((info.blobs.count, info.blobs.unreferenced), (1, 0));

    // Everything but the blob listing is the same in both answers.
    let (mut info, mut quick) = (store.info().unwrap(), store.quick_info().unwrap());
    info.blobs = Default::default();
    quick.blobs = Default::default();
    assert_eq!(info, quick);
}
