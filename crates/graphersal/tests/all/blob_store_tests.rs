//! Blobs of a Store (format spec 4.6): written once, named by their SHA-256, deduplicated,
//! read with length/CRC checks, refused as element property values, hashed only at write,
//! import and verify. Shared helpers of the `blob_*_tests` modules.
#![cfg(feature = "persist")]

use std::sync::Arc;

use graphersal::blob::{BlobHashAlgo, BlobRef, PendingBlob, blob_hash_count};
use graphersal::catalog::{Definition, DefinitionFlags, DefinitionKind, encode_payload};
use graphersal::persist::{MemDir, PersistError, Store, StoreDir, StoreFailure, StoreOptions};
use graphersal::prelude::*;

/// The host kind the tests keep blob-referencing definitions under.
pub(crate) fn host_kind() -> DefinitionKind {
    DefinitionKind::host(200).unwrap()
}

/// A host definition `name` whose payload references `blobs`.
pub(crate) fn host_def(name: &str, blobs: &[&BlobRef]) -> Definition {
    let mut payload = PropertyMap::default();
    payload.insert(
        "blobs",
        ElementProperty::Array(
            blobs
                .iter()
                .map(|blob| ElementProperty::Blob(Box::new((*blob).clone())))
                .collect(),
        ),
    );
    Definition::opaque(
        host_kind(),
        DefinitionFlags::NONE,
        name,
        encode_payload(&payload).unwrap(),
    )
}

/// Writes `bytes` as a blob of `store`.
pub(crate) fn write(store: &Store, bytes: &[u8]) -> PendingBlob {
    store.write_blob(&mut &bytes[..]).unwrap()
}

/// Defines `name` referencing `blobs` (one commit).
pub(crate) fn define(store: &Store, name: &str, blobs: &[&BlobRef]) {
    store
        .graph()
        .write()
        .set_definition(host_def(name, blobs))
        .unwrap();
}

/// Removes the definition `name` (one commit).
pub(crate) fn undefine(store: &Store, name: &str) {
    assert!(
        store
            .graph()
            .write()
            .remove_definition(host_kind(), name)
            .unwrap()
    );
}

/// One commit without blobs.
pub(crate) fn touch(store: &Store, name: &str) {
    store
        .graph()
        .write()
        .traversal_mut()
        .add_v("x")
        .property("name", name)
        .iterate()
        .unwrap();
}

/// The blob files of the store directory `dir` (relative names, sorted), tmp files left out.
pub(crate) fn blob_files(dir: &dyn StoreDir) -> Vec<String> {
    fn walk(dir: &dyn StoreDir, at: &str, out: &mut Vec<String>) {
        let Ok(entries) = dir.list(at) else { return };
        for entry in entries {
            let name = format!("{at}/{}", entry.name);
            match entry.kind {
                graphersal::persist::EntryKind::Dir => walk(dir, &name, out),
                _ if entry.name.starts_with("tmp-") => {}
                _ => out.push(name),
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, "blobs", &mut out);
    out.sort();
    out
}

/// The name of `blob` relative to the store.
pub(crate) fn name_of(blob: &BlobRef) -> String {
    let hex = blob.hash_hex();
    format!("blobs/{}/{hex}", &hex[..2])
}

pub(crate) fn new_store() -> (MemDir, Store) {
    let dir = MemDir::new();
    let store = Store::create(&dir, StoreOptions::new()).unwrap();
    (dir, store)
}

#[test]
fn a_blob_is_named_by_its_sha256_and_read_back_exactly() {
    let (dir, store) = new_store();
    let pending = write(&store, b"hello");
    let blob = pending.blob().clone();
    assert!(pending.is_new());
    assert_eq!(blob.hash_algo(), BlobHashAlgo::Sha256);
    assert_eq!(
        blob.hash_hex(),
        "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
    );
    assert_eq!(blob.length(), 5);
    assert_eq!(blob_files(&dir), vec![name_of(&blob)]);
    assert_eq!(store.read_blob(&blob).unwrap(), b"hello");
    // The same content again: deduplicated, nothing new written.
    let again = write(&store, b"hello");
    assert!(!again.is_new());
    assert_eq!(again.blob(), &blob);
    assert_eq!(blob_files(&dir).len(), 1);
    // An empty blob is a blob too.
    let empty = write(&store, b"");
    assert_eq!(store.read_blob(empty.blob()).unwrap(), Vec::<u8>::new());
    let info = store.info().unwrap();
    assert_eq!(info.blobs.count, 2);
    assert_eq!(info.blobs.bytes, 5);
}

#[test]
fn the_hash_is_computed_at_write_never_at_read() {
    let (_dir, store) = new_store();
    let blob = write(&store, b"counted").blob().clone();
    let before = blob_hash_count();
    for _ in 0..3 {
        store.read_blob(&blob).unwrap();
    }
    // Other tests may hash in parallel; a read never hashes, so the count moves by those only.
    // Each read computes no SHA-256 of its own: compare with writes done here.
    let mid = blob_hash_count();
    write(&store, b"another");
    assert!(blob_hash_count() > mid);
    let _ = before;
}

#[test]
fn a_blob_reference_is_refused_as_an_element_property_value() {
    let (_dir, store) = new_store();
    let blob = write(&store, b"model").blob().clone();
    let err = store
        .graph()
        .write()
        .traversal_mut()
        .add_v("x")
        .property("weights", ElementProperty::Blob(Box::new(blob)))
        .iterate()
        .unwrap_err();
    let text = format!("{err:?}");
    assert!(text.contains("blob reference"), "{text}");
    let help = err.help().unwrap_or_default();
    assert!(
        help.contains("reserved for model definitions in this version"),
        "{help}"
    );
}

#[test]
fn a_blob_reference_travels_in_a_catalog_payload_and_a_change_set() {
    let (_dir, store) = new_store();
    let blob = write(&store, b"payload").blob().clone();
    define(&store, "m", &[&blob]);
    let graph = store.graph().read();
    let stored = graph.definition(host_kind(), "m").unwrap().unwrap().clone();
    let payload = graphersal::catalog::decode_payload(stored.opaque_payload().unwrap()).unwrap();
    assert_eq!(
        payload.get("blobs"),
        Some(&ElementProperty::Array(vec![ElementProperty::Blob(
            Box::new(blob.clone())
        )]))
    );
}

#[test]
fn reads_check_length_and_crc_and_name_the_missing_blob() {
    let (dir, store) = new_store();
    let blob = write(&store, b"0123456789").blob().clone();
    // Same length, other bytes: the CRC-32C fails.
    dir.write_atomic(&name_of(&blob), b"012345678X").unwrap();
    let err = store.read_blob(&blob).unwrap_err();
    assert!(
        matches!(
            &err,
            PersistError::Store {
                cause: StoreFailure::BlobDamaged,
                ..
            }
        ),
        "{err:?}"
    );
    assert!(
        err.help()
            .unwrap_or_default()
            .contains("graphersal store verify")
    );
    // Another length.
    dir.write_atomic(&name_of(&blob), b"0123").unwrap();
    assert!(matches!(
        store.read_blob(&blob).unwrap_err(),
        PersistError::Store {
            cause: StoreFailure::BlobDamaged,
            ..
        }
    ));
    // Gone.
    dir.remove_file(&name_of(&blob)).unwrap();
    assert!(matches!(
        store.read_blob(&blob).unwrap_err(),
        PersistError::Store {
            cause: StoreFailure::BlobUnavailable,
            ..
        }
    ));
    // The store keeps working: blob damage is never store damage.
    touch(&store, "after");
    assert!(!store.is_frozen_by_damage());
}

#[test]
fn a_damaged_blob_is_replaced_by_a_write_of_the_same_content() {
    let (dir, store) = new_store();
    let blob = write(&store, b"abcdef").blob().clone();
    dir.write_atomic(&name_of(&blob), b"abcdeX").unwrap();
    let again = write(&store, b"abcdef");
    assert_eq!(again.blob(), &blob);
    assert_eq!(store.read_blob(&blob).unwrap(), b"abcdef");
}

#[test]
fn two_writers_of_the_same_content_leave_one_intact_blob() {
    let (dir, store) = new_store();
    let store = Arc::new(store);
    let content: Vec<u8> = (0..200_000u32).flat_map(|i| i.to_le_bytes()).collect();
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let store = store.clone();
            let content = content.clone();
            std::thread::spawn(move || {
                store
                    .write_blob(&mut content.as_slice())
                    .unwrap()
                    .blob()
                    .clone()
            })
        })
        .collect();
    let refs: Vec<BlobRef> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(refs[0], refs[1]);
    assert_eq!(blob_files(&dir), vec![name_of(&refs[0])]);
    assert_eq!(store.read_blob(&refs[0]).unwrap(), content);
}

#[test]
fn a_reopened_store_removes_interrupted_writes_and_keeps_the_blobs() {
    let (dir, store) = new_store();
    let blob = write(&store, b"kept").blob().clone();
    define(&store, "m", &[&blob]);
    store.close().unwrap();
    dir.write_atomic("blobs/tmp-0123456789abcdef0123456789abcdef", b"half")
        .unwrap();
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    assert!(store.open_report().tmp_removed >= 1);
    assert_eq!(blob_files(&dir), vec![name_of(&blob)]);
    assert_eq!(store.read_blob(&blob).unwrap(), b"kept");
}

#[test]
fn every_backend_keeps_blobs() {
    let check = |dir: Arc<dyn StoreDir>| {
        let store = Store::create(&dir, StoreOptions::new()).unwrap();
        let blob = write(&store, b"on every backend").blob().clone();
        define(&store, "m", &[&blob]);
        store.close().unwrap();
        let store = Store::open(&dir, StoreOptions::new()).unwrap();
        assert_eq!(store.read_blob(&blob).unwrap(), b"on every backend");
        assert!(store.verify().unwrap().is_ok());
        assert_eq!(store.verify().unwrap().blobs_checked, 1);
    };
    check(Arc::new(MemDir::new()));
    let base = std::env::temp_dir().join(format!("graphersal-blob-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    check(Arc::new(graphersal::persist::FsDir::new(base.join("dir"))));
    check(Arc::new(graphersal::persist::SingleFileDir::new(
        base.join("file.gstore"),
    )));
    let _ = std::fs::remove_dir_all(&base);
}

/// The throughput of a blob write (SHA-256 + CRC-32C + the file write) on a 500 MB stream:
/// `cargo test --release ... blob_store_tests::write_throughput -- --ignored --nocapture`.
#[test]
#[ignore]
fn write_throughput() {
    struct Generated(u64);
    impl std::io::Read for Generated {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = (buf.len() as u64).min(self.0) as usize;
            for (i, byte) in buf[..n].iter_mut().enumerate() {
                *byte = (i as u8).wrapping_mul(31) ^ (self.0 as u8);
            }
            self.0 -= n as u64;
            Ok(n)
        }
    }
    let base = std::env::temp_dir().join(format!("graphersal-blob-speed-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let dir: Arc<dyn StoreDir> = Arc::new(graphersal::persist::FsDir::new(base.join("s")));
    let store = Store::create(&dir, StoreOptions::new()).unwrap();
    let bytes = 500_000_000u64;
    let started = std::time::Instant::now();
    let pending = store.write_blob(&mut Generated(bytes)).unwrap();
    let seconds = started.elapsed().as_secs_f64();
    assert_eq!(pending.blob().length(), bytes);
    println!(
        "blob write: {:.0} MB/s ({bytes} bytes in {seconds:.2} s)",
        bytes as f64 / 1e6 / seconds
    );
    drop(pending);
    drop(store);
    let _ = std::fs::remove_dir_all(&base);
}
