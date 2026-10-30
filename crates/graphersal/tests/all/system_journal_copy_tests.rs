//! The system journal on every copy path (book page "System Journal", format spec 4.7, 12.6 to
//! 12.8, 12.11): full, incremental and ZIP backups and restores carry it, a conversion carries
//! it, a fork, an attic fork and a repair do not (their first record names the source), and no
//! path copies the instance file.
#![cfg(feature = "persist")]

use graphersal::persist::{MemDir, RecoveryTarget, RollbackMode, Store};
use graphersal::prelude::*;
use graphersal::system_journal::JournalRecord;

use crate::system_journal_store_tests::{add_person, all, kinds, options};

fn has_instance(dir: &MemDir) -> bool {
    dir.file_names().iter().any(|name| name == "INSTANCE")
}

fn journal_files(dir: &MemDir) -> Vec<String> {
    dir.file_names()
        .into_iter()
        .filter(|name| name.starts_with("journal/"))
        .collect()
}

/// A store with a few records; it stays open.
fn source() -> (MemDir, Store) {
    let dir = MemDir::new();
    let store = Store::create(&dir, options()).unwrap();
    add_person(&store, "a");
    store.checkpoint(None).unwrap();
    add_person(&store, "b");
    // The batched records go to the files.
    store.journal().unwrap().flush().unwrap();
    (dir, store)
}

fn origins(records: &[JournalRecord]) -> Vec<u128> {
    let mut ids: Vec<u128> = records.iter().map(|r| r.origin.id).collect();
    ids.dedup();
    ids
}

#[test]
fn a_full_backup_carries_the_journal_and_not_the_instance() {
    let (dir, store) = source();
    let backup = MemDir::new();
    let report = store.backup(&backup).unwrap();
    assert!(!report.journal_files.is_empty());
    assert!(
        report
            .files
            .iter()
            .all(|(name, _)| !name.starts_with("journal/")),
        "the journal is reported apart from the graph's files"
    );
    assert!(!has_instance(&backup));
    assert_eq!(journal_files(&backup), journal_files(&dir));
    // Everything the store had written before the backup.
    let source_records = all(&dir);
    let copied = all(&backup);
    assert_eq!(copied, source_records);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    // The backup itself is recorded in the store (after the copy).
    store.close().unwrap();
    assert!(kinds(&all(&dir)).contains(&"store.backup_done".to_string()));
}

#[test]
fn an_increment_appends_the_new_records() {
    let (dir, store) = source();
    let backup = MemDir::new();
    store.backup(&backup).unwrap();
    let first = all(&backup);
    add_person(&store, "c");
    store.checkpoint(None).unwrap();
    store.journal().unwrap().flush().unwrap();
    let report = store.backup(&backup).unwrap();
    assert!(!report.journal_files.is_empty());
    let second = all(&backup);
    assert!(second.len() > first.len());
    assert_eq!(&second[..first.len()], &first[..]);
    // The backup's journal is a prefix of the store's (the store has the backup record).
    let source_records = all(&dir);
    assert_eq!(&source_records[..second.len()], &second[..]);
    // Nothing new on disk (the backup record is still batched): nothing written.
    let again = store.backup(&backup).unwrap();
    assert!(again.journal_files.is_empty(), "{:?}", again.journal_files);
    store.close().unwrap();
}

#[test]
fn a_restored_backup_continues_in_a_new_segment_of_a_new_instance() {
    let (_dir, store) = source();
    let backup = MemDir::new();
    store.backup(&backup).unwrap();
    store.close().unwrap();
    let before = all(&backup);
    Store::restore_backup(&backup).unwrap();
    let segments_before = journal_files(&backup).len();
    let restored = Store::open(&backup, options()).unwrap();
    restored.close().unwrap();
    assert_eq!(journal_files(&backup).len(), segments_before + 1);
    let after = all(&backup);
    assert_eq!(&after[..before.len()], &before[..]);
    assert_eq!(
        origins(&after).len(),
        2,
        "the restoring instance is another one"
    );
    assert!(after.windows(2).all(|w| w[0].seq < w[1].seq));
}

#[cfg(feature = "persist-zip")]
#[test]
fn a_zip_backup_carries_the_journal_and_its_restore_brings_it_back() {
    let (dir, store) = source();
    let mut archive = std::io::Cursor::new(Vec::new());
    let report = store.backup_zip(&mut archive).unwrap();
    assert!(!report.journal_files.is_empty());
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let source_records = all(&dir);
    store.close().unwrap();
    let restored = MemDir::new();
    Store::restore_zip(std::io::Cursor::new(archive.into_inner()), &restored).unwrap();
    assert!(!has_instance(&restored));
    assert_eq!(all(&restored), source_records);
    let opened = Store::open(&restored, options()).unwrap();
    opened.close().unwrap();
    assert_eq!(origins(&all(&restored)).len(), 2);
}

#[test]
fn a_backup_from_memory_carries_the_readable_journal() {
    let (dir, store) = source();
    // Damage found while open: a backup from memory is possible.
    let real = dir.file("GRAPH").unwrap();
    let mut damaged = real.clone();
    damaged[20] ^= 1;
    dir.put_file("GRAPH", damaged.clone());
    dir.put_file("GRAPH.copy", damaged);
    let _ = store.verify();
    dir.put_file("GRAPH", real.clone());
    dir.put_file("GRAPH.copy", real);
    assert!(!store.damage_found().unwrap().is_empty());
    let backup = MemDir::new();
    let report = store
        .backup_from_memory(&backup, graphersal::persist::BackupMode::Auto)
        .unwrap();
    assert!(!report.journal_files.is_empty());
    assert!(!all(&backup).is_empty());
    // The frozen store wrote nothing more to its journal (the damage record stays in memory).
    let on_disk = kinds(&all(&dir));
    assert!(
        !on_disk.contains(&"store.damage_found".to_string()),
        "{on_disk:?}"
    );
    let in_memory = store.journal().unwrap();
    let records = in_memory
        .query(&graphersal::system_journal::JournalQuery::new().kind("store.damage_found"))
        .unwrap()
        .records;
    assert_eq!(records.len(), 1);
}

#[test]
fn a_conversion_carries_the_journal_but_not_the_instance() {
    let (dir, store) = source();
    store.close().unwrap();
    let to = MemDir::new();
    Store::convert(&dir, &to).unwrap();
    assert!(!has_instance(&to));
    assert_eq!(all(&to), all(&dir));
}

/// The new store's journal holds exactly its creation record, naming the source.
fn created_from(dir: &MemDir, how: &str, store_id: u128, commit: i64) {
    let records = all(dir);
    assert_eq!(kinds(&records), ["store.created"], "{how}");
    let created = &records[0];
    assert_eq!(created.field("how"), Some(&ElementProperty::from(how)));
    let id = created
        .field("from_store_id")
        .and_then(|v| v.as_str())
        .unwrap();
    assert_eq!(id.replace('-', ""), format!("{store_id:032x}"), "{how}");
    assert_eq!(
        created.field("from_commit"),
        Some(&ElementProperty::from(commit))
    );
}

#[test]
fn a_fork_an_attic_fork_and_a_repair_start_a_journal_of_their_own() {
    let (dir, store) = source();
    let store_id = store.store_id().unwrap();
    let commit = store.graph().read().last_commit_seq();

    let fork = MemDir::new();
    store.fork(RecoveryTarget::Latest, &fork).unwrap();
    created_from(&fork, "fork", store_id, commit as i64);

    add_person(&store, "c");
    let report = store
        .rollback_to(RecoveryTarget::CommitSeq(commit), RollbackMode::Move)
        .unwrap();
    let entry = report.attic.expect("an attic entry");
    let attic_fork = MemDir::new();
    store.attic_fork(&entry, &attic_fork).unwrap();
    created_from(&attic_fork, "attic_fork", store_id, (commit + 1) as i64);
    store.close().unwrap();

    let repaired = MemDir::new();
    let report = Store::repair_dir(&dir, &repaired).unwrap();
    created_from(&repaired, "repair", store_id, report.commit_seq as i64);
    for copy in [&fork, &attic_fork, &repaired] {
        assert!(has_instance(copy), "the creating process's instance");
        let opened = Store::open(copy, options()).unwrap();
        opened.close().unwrap();
        // The source's records are not there.
        assert_eq!(all(copy)[0].kind, "store.created");
    }
}
