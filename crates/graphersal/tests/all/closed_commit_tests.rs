//! Catalog commits on a CLOSED store (format spec 12.12; book page "Embedding Models in a
//! Store"): `Store::import_model_dir`, `set_default_model_dir` and `remove_model_dir` append ONE
//! commit to the WAL without loading the graph; the next open replays it like any commit.
#![cfg(all(feature = "persist", not(target_arch = "wasm32")))]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use graphersal::catalog::{Definition, DefinitionFlags, DefinitionKind};
use graphersal::persist::{
    ClosedCommitOptions, DirEntry, DirLockGuard, EntryKind, ImportModelOptions, LockKind, MemDir,
    ModelSource, PersistError, Store, StoreDir, StoreFailure, StoreFile,
};
use graphersal::prelude::*;
use graphersal::system_journal::JournalQuery;

use crate::model_fixture::*;

/// Every file of the store at `path` with its bytes (sorted by name).
fn files_of(path: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    fn walk(dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        let mut entries: Vec<_> = fs::read_dir(dir).unwrap().flatten().collect();
        entries.sort_by_key(|e| e.path());
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.file_name().is_some_and(|n| n != "LOCK") {
                out.push((path.clone(), fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(path, &mut out);
    out
}

/// The WAL segment files of the store at `path`, in order.
fn wal_segments(path: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fs::read_dir(path.join("wal"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    out.sort();
    out
}

/// A closed store at `path` with some data in a snapshot and some in the WAL after it.
fn closed_store_with_data(path: &Path) -> u64 {
    let store = Store::create(path, options()).unwrap();
    for name in ["ann", "bob", "cid"] {
        add_person(&store, name);
    }
    store.checkpoint(None).unwrap();
    add_person(&store, "dan");
    let seq = store.graph().read().last_commit_seq();
    store.close().unwrap();
    seq
}

fn import_closed(path: &Path, seed: u8, repo: &str) -> graphersal::persist::ImportModelReport {
    Store::import_model_dir(
        path,
        ModelSource::Files(tiny_model(seed, repo)),
        ImportModelOptions::new().with_principal("test"),
    )
    .unwrap()
}

fn cause_of<T>(result: Result<T, PersistError>) -> StoreFailure {
    match result {
        Err(PersistError::Store { cause, .. }) => cause,
        Err(other) => panic!("another error: {other}"),
        Ok(_) => panic!("not refused"),
    }
}

#[test]
fn an_import_into_a_closed_store_is_one_commit_without_loading() {
    let temp = Temp::new("closed-import");
    let path = temp.join("s");
    let last = closed_store_with_data(&path);
    // Damage every element segment of the snapshot: a load would find it, the closed commit
    // reads no element (only the manifest, the schema and the WAL's catalog changes).
    let snapshot = fs::read_dir(path.join("snapshots"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .max()
        .unwrap();
    let segments: Vec<PathBuf> = fs::read_dir(&snapshot)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "seg"))
        .collect();
    assert!(!segments.is_empty(), "{snapshot:?}");
    let saved: Vec<Vec<u8>> = segments.iter().map(|p| fs::read(p).unwrap()).collect();
    for segment in &segments {
        flip_byte(segment);
    }
    let report = import_closed(&path, 1, "acme/tiny");
    assert_eq!(report.commit_seq, Some(last + 1));
    assert!(!report.already_present);
    assert_eq!(report.model.repo, "acme/tiny");
    for (segment, bytes) in segments.iter().zip(saved) {
        fs::write(segment, bytes).unwrap();
    }
    let store = Store::open(&path, options()).unwrap();
    assert!(store.open_report().clean_close);
    assert_eq!(store.graph().read().last_commit_seq(), last + 1);
    assert_eq!(store.graph().read().vertex_count(), 4);
    let models = store.models().unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].identity, report.model.identity);
    // The commit is an ordinary one: the CDC stream shows it with its after image and principal.
    let changes: Vec<_> = store
        .changes_since(last)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].commit_seq(), last + 1);
    assert_eq!(changes[0].metadata().principal(), Some("test"));
    store.close().unwrap();
}

#[test]
fn the_commit_replays_at_open() {
    let temp = Temp::new("closed-replay");
    let path = temp.join("s");
    let last = closed_store_with_data(&path);
    let a = import_closed(&path, 1, "acme/a");
    let b = import_closed(&path, 2, "acme/b");
    let default =
        Store::set_default_model_dir(&path, Some("acme/b"), ClosedCommitOptions::new()).unwrap();
    assert_eq!(default.commit_seq, Some(last + 3));
    assert!(default.committed_at.is_some());
    assert!(default.model.as_ref().is_some_and(|m| m.default));
    let moved =
        Store::set_default_model_dir(&path, Some("acme/a"), ClosedCommitOptions::new()).unwrap();
    assert_eq!(moved.commit_seq, Some(last + 4));
    let removed = Store::remove_model_dir(&path, "acme/b", ClosedCommitOptions::new()).unwrap();
    assert_eq!(removed.commit_seq, Some(last + 5));
    assert_eq!(removed.model.unwrap().identity, b.model.identity);
    // The files say the same as the open store.
    let in_files = Store::models_dir(&path).unwrap();
    let store = Store::open(&path, options()).unwrap();
    let models = store.models().unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].identity, a.model.identity);
    assert!(models[0].default);
    assert_eq!(in_files.len(), 1);
    assert_eq!(in_files[0].identity, a.model.identity);
    assert!(in_files[0].default);
    // Before images: the default move recorded both definitions' before and after.
    let changes: Vec<_> = store
        .changes_since(last + 3)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let defaults = &changes[0];
    assert_eq!(defaults.mutations().len(), 2);
    for mutation in defaults.mutations() {
        let graphersal::changes::Mutation::SetDefinition { before, after, .. } = mutation else {
            panic!("{mutation:?}");
        };
        assert!(before.is_some() && after.is_some());
    }
    // The open store continues after the closed commits.
    add_person(&store, "eve");
    assert_eq!(store.graph().read().last_commit_seq(), last + 6);
    store.close().unwrap();
    // The system journal holds the closed commits.
    let journal = Store::journal_dir(&path, &JournalQuery::new()).unwrap();
    let closed: Vec<_> = journal
        .records
        .iter()
        .filter(|r| r.kind == "catalog.closed_commit")
        .collect();
    assert_eq!(closed.len(), 5);
    assert!(
        journal.records.iter().any(|r| r.kind == "model.removed"
            && r.field("closed") == Some(&ElementProperty::from(true)))
    );
}

/// A tiny deterministic generator (xorshift).
struct Rng(u64);

impl Rng {
    fn next(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }
}

#[test]
fn the_view_matches_recovery() {
    let temp = Temp::new("closed-view");
    let path = temp.join("s");
    closed_store_with_data(&path);
    let repos = ["acme/a", "acme/b", "acme/c"];
    let mut rng = Rng(0x5eed_1234_abcd_0001);
    let mut expected_seq = Store::inspect(&path).unwrap().commit_seq;
    for step in 0..40 {
        let repo = repos[rng.next(3) as usize];
        let seed = (rng.next(2) + 1) as u8;
        let closed = rng.next(2) == 0;
        let action = rng.next(5);
        let result: Result<Option<u64>, PersistError> = if closed {
            match action {
                0 | 1 => Store::import_model_dir(
                    &path,
                    ModelSource::Files(tiny_model(seed, repo)),
                    ImportModelOptions::new()
                        .with_replace(rng.next(2) == 0)
                        .with_default(rng.next(3) == 0),
                )
                .map(|r| r.commit_seq),
                2 => Store::set_default_model_dir(
                    &path,
                    (rng.next(4) != 0).then_some(repo),
                    ClosedCommitOptions::new(),
                )
                .map(|r| r.commit_seq),
                3 => Store::remove_model_dir(&path, repo, ClosedCommitOptions::new())
                    .map(|r| r.commit_seq),
                _ => Store::checkpoint_dir(&path, options(), None).map(|_| None),
            }
        } else {
            let store = Store::open(&path, options()).unwrap();
            let result = match action {
                0 | 1 => store
                    .import_model(
                        ModelSource::Files(tiny_model(seed, repo)),
                        ImportModelOptions::new()
                            .with_replace(rng.next(2) == 0)
                            .with_default(rng.next(3) == 0),
                    )
                    .map(|r| r.commit_seq),
                2 => store
                    .set_default_model((rng.next(4) != 0).then_some(repo))
                    .map(|r| r.commit_seq),
                3 => store.remove_model(repo).map(|r| r.commit_seq),
                _ => {
                    add_person(&store, "x");
                    Ok(Some(store.graph().read().last_commit_seq()))
                }
            };
            store.close().unwrap();
            result
        };
        match result {
            Ok(Some(seq)) => {
                expected_seq += 1;
                assert_eq!(seq, expected_seq, "step {step}");
            }
            Ok(None) => {}
            Err(PersistError::Store { cause, .. })
                if matches!(
                    cause,
                    StoreFailure::UnknownModel | StoreFailure::ModelRevisionExists
                ) => {}
            Err(err) => panic!("step {step}: {err}"),
        }
        // The closed view (models_dir reads the same files) equals the recovered catalog.
        let in_files = Store::models_dir(&path).unwrap();
        let store = Store::open(&path, options()).unwrap();
        let mut opened = store.models().unwrap();
        assert_eq!(
            store.graph().read().last_commit_seq(),
            expected_seq,
            "step {step}"
        );
        store.close().unwrap();
        for model in &mut opened {
            model.state = in_files
                .iter()
                .find(|m| m.identity == model.identity)
                .map_or(model.state.clone(), |m| m.state.clone());
        }
        assert_eq!(in_files, opened, "step {step}");
        assert!(opened.iter().filter(|m| m.default).count() <= 1);
    }
}

#[test]
fn a_torn_tail_is_cut_before_appending() {
    let temp = Temp::new("closed-torn");
    let path = temp.join("s");
    let last = closed_store_with_data(&path);
    // Half of a record after the last one: an interrupted write.
    let segment = wal_segments(&path).pop().unwrap();
    let bytes = fs::read(&segment).unwrap();
    let header = 64;
    let mut torn = bytes.clone();
    torn.extend_from_slice(&bytes[header..header + 12]);
    fs::write(&segment, &torn).unwrap();
    let report = import_closed(&path, 1, "acme/tiny");
    assert_eq!(report.commit_seq, Some(last + 1));
    let store = Store::open(&path, options()).unwrap();
    assert!(store.open_report().torn_tail.is_none());
    assert_eq!(store.graph().read().last_commit_seq(), last + 1);
    assert_eq!(store.models().unwrap().len(), 1);
    store.close().unwrap();
}

#[test]
fn a_damaged_tail_refuses_and_writes_nothing() {
    let temp = Temp::new("closed-damaged");
    let path = temp.join("s");
    closed_store_with_data(&path);
    let segment = wal_segments(&path).pop().unwrap();
    let mut bytes = fs::read(&segment).unwrap();
    let at = bytes.len() - 3;
    bytes[at] ^= 0x40;
    fs::write(&segment, &bytes).unwrap();
    let before = files_of(&path);
    let err = Store::import_model_dir(
        &path,
        ModelSource::Files(tiny_model(1, "acme/tiny")),
        ImportModelOptions::new(),
    )
    .unwrap_err();
    assert!(matches!(err, PersistError::Corrupt { .. }), "{err}");
    assert_eq!(files_of(&path), before);
}

#[test]
fn nothing_to_do_writes_nothing() {
    let temp = Temp::new("closed-nothing");
    let path = temp.join("s");
    closed_store_with_data(&path);
    import_closed(&path, 1, "acme/tiny");
    Store::set_default_model_dir(&path, Some("acme/tiny"), ClosedCommitOptions::new()).unwrap();
    let before = files_of(&path);
    let again =
        Store::set_default_model_dir(&path, Some("acme/tiny"), ClosedCommitOptions::new()).unwrap();
    assert_eq!(again.commit_seq, None);
    let present = import_closed(&path, 1, "acme/tiny");
    assert!(present.already_present);
    assert_eq!(present.commit_seq, None);
    assert_eq!(files_of(&path), before);
}

#[test]
fn remove_refused_while_the_schema_needs_it() {
    let temp = Temp::new("closed-needed");
    let path = temp.join("s");
    let store = Store::create(&path, options()).unwrap();
    let model = import(&store, 1, "acme/tiny").model;
    set_schema(&store, &model.identity);
    store.close().unwrap();
    let before = files_of(&path);
    let cause = cause_of(Store::remove_model_dir(
        &path,
        "acme/tiny",
        ClosedCommitOptions::new(),
    ));
    assert_eq!(cause, StoreFailure::ModelInUse);
    assert_eq!(
        cause_of(Store::remove_model_dir(
            &path,
            "acme/none",
            ClosedCommitOptions::new()
        )),
        StoreFailure::UnknownModel
    );
    // Another revision while the schema uses this one: a replace needs vector rules.
    let err = Store::import_model_dir(
        &path,
        ModelSource::Files(tiny_model(2, "acme/tiny")),
        ImportModelOptions::new().with_replace(true),
    )
    .unwrap_err();
    assert!(matches!(err, PersistError::Graph(_)), "{err}");
    assert_eq!(
        cause_of(Store::import_model_dir(
            &path,
            ModelSource::Files(tiny_model(2, "acme/tiny")),
            ImportModelOptions::new(),
        )),
        StoreFailure::ModelRevisionExists
    );
    assert_eq!(files_of(&path), before);
}

#[test]
fn a_locked_store_is_refused() {
    let temp = Temp::new("closed-locked");
    let path = temp.join("s");
    closed_store_with_data(&path);
    let store = Store::open(&path, options()).unwrap();
    let err = Store::set_default_model_dir(&path, None, ClosedCommitOptions::new()).unwrap_err();
    assert!(matches!(err, PersistError::Locked { .. }), "{err}");
    let help = graphersal::error::format_diagnostic(&err);
    assert!(help.contains("Store::import_model"), "{help}");
    store.close().unwrap();
}

#[test]
fn a_backup_is_refused() {
    let temp = Temp::new("closed-backup");
    let path = temp.join("s");
    closed_store_with_data(&path);
    let store = Store::open(&path, options()).unwrap();
    store.backup(temp.join("b")).unwrap();
    store.close().unwrap();
    let err = Store::import_model_dir(
        temp.join("b"),
        ModelSource::Files(tiny_model(1, "acme/tiny")),
        ImportModelOptions::new(),
    )
    .unwrap_err();
    assert!(matches!(err, PersistError::IsBackup { .. }), "{err}");
}

#[test]
fn closed_at_moves_forward_and_a_crashed_store_stays_crashed() {
    let temp = Temp::new("closed-at");
    let path = temp.join("s");
    let last = closed_store_with_data(&path);
    import_closed(&path, 1, "acme/tiny");
    // A clean close recorded the new commit: an open reaches it cleanly.
    let store = Store::open(&path, options()).unwrap();
    assert!(store.open_report().clean_close);
    assert_eq!(store.open_report().commit_seq, last + 1);
    store.close().unwrap();
    // A WAL cut back behind the closed commit is the damage 9.8 describes.
    let segment = wal_segments(&path).pop().unwrap();
    let bytes = fs::read(&segment).unwrap();
    let report =
        Store::set_default_model_dir(&path, Some("acme/tiny"), ClosedCommitOptions::new()).unwrap();
    assert_eq!(report.commit_seq, Some(last + 2));
    fs::write(&segment, &bytes).unwrap();
    assert!(matches!(
        Store::open(&path, options()),
        Ok(store) if store.maintenance().is_some()
    ));
}

#[test]
fn a_full_segment_gets_a_new_segment() {
    let temp = Temp::new("closed-rotate");
    let path = temp.join("s");
    let last = closed_store_with_data(&path);
    let before = wal_segments(&path).len();
    Store::import_model_dir(
        &path,
        ModelSource::Files(tiny_model(1, "acme/tiny")),
        ImportModelOptions::new(),
    )
    .unwrap();
    assert_eq!(wal_segments(&path).len(), before);
    let report = Store::set_default_model_dir(
        &path,
        Some("acme/tiny"),
        ClosedCommitOptions::new().with_wal_segment_bytes(1),
    )
    .unwrap();
    assert_eq!(report.commit_seq, Some(last + 2));
    let segments = wal_segments(&path);
    assert_eq!(segments.len(), before + 1);
    assert!(
        segments
            .last()
            .unwrap()
            .to_string_lossy()
            .ends_with(&format!("{:020}.wal", last + 2))
    );
    let store = Store::open(&path, options()).unwrap();
    assert_eq!(store.graph().read().last_commit_seq(), last + 2);
    assert!(store.models().unwrap()[0].default);
    store.close().unwrap();
}

#[test]
fn the_commit_time_is_never_earlier_than_the_store_s_times() {
    let temp = Temp::new("closed-time");
    let path = temp.join("s");
    closed_store_with_data(&path);
    let store = Store::open(&path, options()).unwrap();
    store.mark("m").unwrap();
    let last_time =
        graphersal::storage::PersistentStorage::last_committed_at(&*store.graph().read());
    store.close().unwrap();
    let journal_time = Store::journal_dir(&path, &JournalQuery::new())
        .unwrap()
        .records
        .iter()
        .map(|r| r.time)
        .max()
        .unwrap();
    let report = import_closed(&path, 1, "acme/tiny");
    let store = Store::open(&path, options()).unwrap();
    let at = graphersal::storage::PersistentStorage::last_committed_at(&*store.graph().read());
    assert_eq!(
        store.graph().read().last_commit_seq(),
        report.commit_seq.unwrap()
    );
    assert!(
        at >= last_time && at >= journal_time,
        "{at} {last_time} {journal_time}"
    );
    store.close().unwrap();
}

#[test]
fn a_critical_opaque_definition_refuses() {
    let temp = Temp::new("closed-critical");
    let path = temp.join("s");
    // A graph without a journal holds it (as a newer version would have written it).
    let mut graph = TraversalGraph::new();
    graph
        .set_definition(Definition::opaque(
            DefinitionKind::from_code(200),
            DefinitionFlags::NONE.with_critical(true),
            "future",
            Vec::new(),
        ))
        .unwrap();
    let store = Store::create_with(&path, graph, options()).unwrap();
    store.close().unwrap();
    let before = files_of(&path);
    let err = Store::import_model_dir(
        &path,
        ModelSource::Files(tiny_model(1, "acme/tiny")),
        ImportModelOptions::new(),
    )
    .unwrap_err();
    assert!(matches!(err, PersistError::ReadOnly { .. }), "{err}");
    assert_eq!(files_of(&path), before);
}

// ---------------------------------------------------------------------------------------------
// Crashes after every write step
// ---------------------------------------------------------------------------------------------

/// A [`MemDir`] that "crashes" after a number of writes: from then on every write fails.
#[derive(Debug, Clone)]
struct Crashing {
    inner: MemDir,
    budget: Arc<AtomicI64>,
}

impl Crashing {
    fn tick(&self) -> io::Result<()> {
        if self.budget.fetch_sub(1, Ordering::SeqCst) <= 0 {
            return Err(io::Error::other("simulated crash"));
        }
        Ok(())
    }
}

struct CrashingFile {
    inner: Box<dyn StoreFile>,
    dir: Crashing,
}

impl io::Write for CrashingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.dir.tick()?;
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl StoreFile for CrashingFile {
    fn sync(&mut self) -> io::Result<()> {
        self.dir.tick()?;
        self.inner.sync()
    }

    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.dir.tick()?;
        self.inner.set_len(len)
    }
}

impl StoreDir for Crashing {
    fn location(&self) -> String {
        "memory:crashing".into()
    }
    fn entry_kind(&self, name: &str) -> io::Result<Option<EntryKind>> {
        self.inner.entry_kind(name)
    }
    fn list(&self, dir: &str) -> io::Result<Vec<DirEntry>> {
        self.inner.list(dir)
    }
    fn file_len(&self, name: &str) -> io::Result<u64> {
        self.inner.file_len(name)
    }
    fn read(&self, name: &str, max: u64) -> io::Result<Vec<u8>> {
        self.inner.read(name, max)
    }
    fn read_range(&self, name: &str, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        self.inner.read_range(name, offset, len)
    }
    fn open_read(&self, name: &str) -> io::Result<Box<dyn io::Read + Send>> {
        self.inner.open_read(name)
    }
    fn create_dir_all(&self, name: &str) -> io::Result<()> {
        self.tick()?;
        self.inner.create_dir_all(name)
    }
    fn create(&self, name: &str, exclusive: bool) -> io::Result<Box<dyn StoreFile>> {
        self.tick()?;
        Ok(Box::new(CrashingFile {
            inner: self.inner.create(name, exclusive)?,
            dir: self.clone(),
        }))
    }
    fn open_write(&self, name: &str, offset: u64) -> io::Result<Box<dyn StoreFile>> {
        self.tick()?;
        Ok(Box::new(CrashingFile {
            inner: self.inner.open_write(name, offset)?,
            dir: self.clone(),
        }))
    }
    fn append(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        self.tick()?;
        self.inner.append(name, bytes)
    }
    fn write_atomic(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        self.tick()?;
        self.inner.write_atomic(name, bytes)
    }
    fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        self.tick()?;
        self.inner.rename(from, to)
    }
    fn truncate(&self, name: &str, len: u64) -> io::Result<()> {
        self.tick()?;
        self.inner.truncate(name, len)
    }
    fn remove_file(&self, name: &str) -> io::Result<()> {
        self.tick()?;
        self.inner.remove_file(name)
    }
    fn remove_tree(&self, name: &str) -> io::Result<()> {
        self.tick()?;
        self.inner.remove_tree(name)
    }
    fn sync_dir(&self, name: &str) -> io::Result<()> {
        self.tick()?;
        self.inner.sync_dir(name)
    }
    fn lock(&self, kind: LockKind) -> io::Result<Option<DirLockGuard>> {
        self.inner.lock(kind)
    }
}

/// The models (identity, default) and the position of the store in `dir`, opened.
fn state_of(dir: &MemDir) -> (u64, Vec<(String, bool)>) {
    let store = Store::open(dir, options()).unwrap();
    assert!(store.maintenance().is_none());
    let models = store
        .models()
        .unwrap()
        .into_iter()
        .map(|m| (m.identity, m.default))
        .collect();
    let seq = store.graph().read().last_commit_seq();
    store.close().unwrap();
    (seq, models)
}

#[test]
fn a_crash_after_every_write_step_opens_consistently() {
    let base = MemDir::new();
    let store = Store::create(&base, options()).unwrap();
    add_person(&store, "ann");
    store.checkpoint(None).unwrap();
    add_person(&store, "bob");
    store.close().unwrap();
    let (old_seq, old_models) = state_of(&base.deep_copy());
    let mut completed = false;
    for budget in 0..400 {
        let image = base.deep_copy();
        let dir = Crashing {
            inner: image.clone(),
            budget: Arc::new(AtomicI64::new(budget)),
        };
        let result = Store::import_model_dir(
            Arc::new(dir.clone()) as Arc<dyn StoreDir>,
            ModelSource::Files(tiny_model(1, "acme/tiny")),
            ImportModelOptions::new().with_default(true),
        );
        let (seq, models) = state_of(&image);
        if seq == old_seq {
            assert_eq!(models, old_models, "budget {budget}");
        } else {
            assert_eq!(seq, old_seq + 1, "budget {budget}");
            assert_eq!(models.len(), 1, "budget {budget}");
            assert!(models[0].1, "budget {budget}");
        }
        // The image takes the import afterwards (or has it).
        let again = import_closed_mem(&image);
        assert!(again.already_present || again.commit_seq == Some(old_seq + 1));
        if result.is_ok() {
            assert_eq!(seq, old_seq + 1, "budget {budget}");
            completed = true;
            break;
        }
    }
    assert!(completed);
}

fn import_closed_mem(dir: &MemDir) -> graphersal::persist::ImportModelReport {
    Store::import_model_dir(
        dir,
        ModelSource::Files(tiny_model(1, "acme/tiny")),
        ImportModelOptions::new().with_default(true),
    )
    .unwrap()
}

#[test]
fn a_closed_commit_works_on_every_backend() {
    let temp = Temp::new("closed-backends");
    let file = temp.join("s.gstore");
    let mem = MemDir::new();
    for (name, dir) in [
        ("fs", graphersal::persist::store_dir_at(temp.join("d"))),
        ("file", graphersal::persist::store_dir_at(&file)),
        ("mem", Arc::new(mem.clone()) as Arc<dyn StoreDir>),
    ] {
        let store = Store::create(dir.clone(), options()).unwrap();
        add_person(&store, "ann");
        store.close().unwrap();
        let report = Store::import_model_dir(
            dir.clone(),
            ModelSource::Files(tiny_model(1, "acme/tiny")),
            ImportModelOptions::new(),
        )
        .unwrap();
        assert_eq!(report.commit_seq, Some(2), "{name}");
        let store = Store::open(dir.clone(), options()).unwrap();
        assert_eq!(store.models().unwrap().len(), 1, "{name}");
        assert_eq!(store.graph().read().vertex_count(), 1, "{name}");
        store.close().unwrap();
    }
}
