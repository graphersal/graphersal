//! A backup from memory (format spec 12.11): when the store on disk is damaged and open in this
//! process, its graph in memory (verified when it was loaded, changed only by commits since)
//! is written by the streaming encoder straight into the BACKUP directory, under the graph's
//! read lock: one snapshot, `GRAPH` with the backup marker and the store's identity and lineage,
//! and an empty WAL segment after that commit. Nothing is read from or written to the store's
//! own directory (a dying disk gets no more writes). Into a new directory (a full backup) or as
//! an increment of a backup of the same store (the snapshot is newer than everything in it).

use super::super::error::{DamageSide, PersistError};
use super::super::wal::WAL_HEADER_BYTES;
use super::backup::{BackupMode, INCREMENT_OP, recover_interrupted, refuse, roll_back_increment};
use super::backup_check::{Findings, check_segment};
use super::files::{
    self, BackupMarker, DirLock, DirPath, GRAPH_COPY, GRAPH_FILE, GraphFile, Intent, SNAPSHOTS_DIR,
    WAL_DIR, list_snapshots, list_wal, read_graph_file, snapshot_name, wal_name, write_graph_file,
};
use super::maintenance::BackupReport;
use super::snapshot_dir::{NewSnapshotDir, write_graph_snapshot};
use crate::graph::PersistentStorage;
use crate::persist::snapshot::WriteOptions;

/// The name of a snapshot written from memory (shown by `store list`).
pub(super) const FROM_MEMORY_NAME: &str = "from-memory";

/// Writes the backup of `graph` (the store's graph, read-locked by the caller) into `target`.
/// `graph_file` is the store's `GRAPH` as the open store holds it (identity, lineage, the
/// creation parameters); `store` names the store in messages only.
pub(super) fn backup_from_memory<S: PersistentStorage + ?Sized>(
    store: &DirPath,
    graph: &S,
    graph_file: &GraphFile,
    write: &WriteOptions,
    target: &DirPath,
    mode: BackupMode,
) -> Result<BackupReport, PersistError> {
    if !files::has_entries(target) {
        return full(store, graph, graph_file, write, target);
    }
    if mode == BackupMode::Full {
        return Err(refuse(format!(
            "a full backup needs an empty or new directory, and {} is not empty",
            target.display()
        )));
    }
    if !target.join(GRAPH_FILE).exists() && !target.join(GRAPH_COPY).exists() {
        return Err(refuse(format!(
            "{} is not empty and holds no backup (no GRAPH file); back up into a new, empty \
             directory",
            target.display()
        )));
    }
    let mut report = increment(store, graph, graph_file, write, target)?;
    // The models follow the store's, best effort (see `full`).
    if let Err(err) = super::models::sync_models(store, target) {
        report
            .warnings
            .push(format!("the models were not brought up to date: {err}"));
    }
    report.models = super::models::list(target).unwrap_or_default();
    Ok(report)
}

/// The `GRAPH` file of a backup from memory at commit `seq`: the store's identity, lineage and
/// creation parameters, clean, the marker, its own newest segment `seq + 1`.
fn backup_graph_file(graph_file: &GraphFile, seq: u64, taken_at: i64) -> GraphFile {
    let mut file = graph_file.clone();
    file.clean = true;
    file.wal_cut = None;
    file.backup = Some(BackupMarker {
        commit_seq: seq,
        taken_at,
    });
    file.wal_head = Some(seq.saturating_add(1));
    file.closed_at = Some(seq);
    file
}

/// Writes the snapshot of `graph` into `target`'s `snapshots/` (the streaming encoder into
/// `tmp-*`, synced, every checksum read back, renamed) and the empty WAL segment after it.
/// Returns the files written with their sizes.
fn write_files<S: PersistentStorage + ?Sized>(
    store: &DirPath,
    graph: &S,
    graph_file: &GraphFile,
    write: &WriteOptions,
    target: &DirPath,
) -> Result<Vec<(String, u64)>, PersistError> {
    let seq = graph.last_commit_seq();
    let next = seq
        .checked_add(1)
        .ok_or(crate::error::GraphError::SequenceOverflow {
            sequence: crate::error::COMMIT_SEQUENCE,
        })?;
    let snapshot = NewSnapshotDir::begin(target, seq)?;
    write_graph_snapshot(snapshot.tmp(), graph, write)?;
    // The snapshot is read back before its rename: a copy that does not read back intact is
    // the backup's damage (the graph in memory is fine).
    let (dir, _) = snapshot.finish().map_err(|err| match err {
        PersistError::Corrupt {
            location,
            offset,
            reason,
        } => {
            let mut findings = Findings::default();
            let root = target.display();
            let file = location
                .strip_prefix(&root)
                .map(|rest| rest.trim_start_matches('/').to_string())
                .unwrap_or(location);
            findings.problem(file, Some(offset), reason);
            findings.into_error(DamageSide::Backup, store, target.display(), true)
        }
        other => other,
    })?;
    let mut written = Vec::new();
    for (name, _) in dir
        .list()
        .map_err(|err| PersistError::io("listing the backup's snapshot", err))?
    {
        let len = dir
            .join(&name)
            .len()
            .map_err(|err| PersistError::io("reading the backup's snapshot", err))?;
        written.push((
            format!("{SNAPSHOTS_DIR}/{}/{name}", snapshot_name(seq)),
            len,
        ));
    }
    let segment = super::wal_files::create_segment(target, graph_file.graph_id, next)
        .map_err(|err| PersistError::io("starting the backup's WAL segment", err))?;
    drop(segment);
    written.push((
        format!("{WAL_DIR}/{}", wal_name(next)),
        WAL_HEADER_BYTES as u64,
    ));
    Ok(written)
}

/// Reads the WAL segment and `GRAPH` a backup from memory wrote back (the snapshot was read
/// back before its rename).
fn check_written(target: &DirPath, file: &GraphFile, seq: u64) -> Findings {
    let mut findings = Findings::default();
    let next = seq.saturating_add(1);
    let path = target.join(WAL_DIR).join(wal_name(next));
    if let Some(check) = check_segment(target, next, &path, None, false, Some(file), &mut findings)
        && (check.end != WAL_HEADER_BYTES as u64 || !check.commits.is_empty())
    {
        findings.problem(
            path.relative_to(target),
            None,
            "the new WAL segment is not empty".into(),
        );
    }
    let bytes = file.encode();
    for name in [GRAPH_FILE, GRAPH_COPY] {
        match files::read_bounded(&target.join(name), 64 << 20) {
            Ok(read) if read == bytes => {}
            Ok(_) => findings.problem(name, None, "differs from what was written".into()),
            Err(err) => findings.problem(name, None, err.to_string()),
        }
    }
    findings
}

fn full<S: PersistentStorage + ?Sized>(
    store: &DirPath,
    graph: &S,
    graph_file: &GraphFile,
    write: &WriteOptions,
    target: &DirPath,
) -> Result<BackupReport, PersistError> {
    let existed = target.exists();
    let result = (|| {
        for sub in [SNAPSHOTS_DIR, WAL_DIR] {
            target
                .join(sub)
                .create_dir_all()
                .map_err(|err| PersistError::io("creating the backup directory", err))?;
        }
        let seq = graph.last_commit_seq();
        let taken_at = crate::graph::undo::now_micros();
        let mut files_written = write_files(store, graph, graph_file, write, target)?;
        // The models are files of the store, copied as they are; a damaged one is skipped
        // with a warning (this backup exists because the store's files are damaged).
        let (models, model_warnings) = super::models::copy_all_lenient(store, target);
        let file = backup_graph_file(graph_file, seq, taken_at);
        write_graph_file(target, &file)?;
        files::sync_dir(target)?;
        let check = check_written(target, &file, seq);
        if !check.is_ok() {
            return Err(check.into_error(DamageSide::Backup, store, target.display(), true));
        }
        let graph_len = file.encode().len() as u64;
        files_written.push((GRAPH_COPY.to_string(), graph_len));
        files_written.push((GRAPH_FILE.to_string(), graph_len));
        Ok(BackupReport {
            snapshot_commit_seq: seq,
            commit_seq: seq,
            snapshots_copied: vec![seq],
            segments_copied: vec![seq.saturating_add(1)],
            taken_at,
            bytes: files_written.iter().map(|(_, len)| len).sum(),
            files: files_written,
            from_memory: true,
            models,
            warnings: model_warnings,
            ..BackupReport::default()
        })
    })();
    if result.is_err() {
        files::undo_new_dir(target, existed);
    }
    result
}

fn increment<S: PersistentStorage + ?Sized>(
    store: &DirPath,
    graph: &S,
    graph_file: &GraphFile,
    write: &WriteOptions,
    target: &DirPath,
) -> Result<BackupReport, PersistError> {
    // An existing backup of a newer (or older) format is never written to (format spec 15).
    files::refuse_other_version(target)?;
    let _lock = DirLock::exclusive(target).map_err(|err| match err {
        PersistError::Locked { path } => refuse(format!(
            "the backup directory {path} is in use (another backup, a restore or a prune of it \
             runs); try again when it has finished"
        )),
        other => other,
    })?;
    recover_interrupted(target)?;
    let backup = read_graph_file(target)?.file;
    super::guard_version(target, backup.version)?;
    let Some(previous_marker) = backup.backup else {
        return Err(refuse(format!(
            "{} holds a store that is not a backup (it is never written to): back up into a \
             backup directory of this store or into an empty one",
            target.display()
        )));
    };
    // The backup must be of this store (its chain the tail of the store's).
    let position = graph_file
        .lineage
        .iter()
        .position(|entry| entry.graph_id == backup.graph_id)
        .filter(|&k| graph_file.lineage[k..] == backup.lineage[..]);
    let Some(k) = position else {
        return Err(refuse(format!(
            "the backup in {} is of another graph (graph id {}), not of this store: back up \
             into a new, empty directory",
            target.display(),
            crate::value::uuid::format_canonical(backup.graph_id),
        )));
    };
    super::backup::check_same_store(&backup, graph_file, target)?;
    // The backup's lineage is the store's history only up to the EARLIEST branch point
    // after it (a rollback of a later lineage to before its own start branches from this one).
    let valid_to = graph_file.lineage[..k]
        .iter()
        .map(|entry| entry.branched_at)
        .min()
        .unwrap_or(u64::MAX);
    let seq = graph.last_commit_seq();
    let b_newest = list_snapshots(target)?.last().map(|(s, _)| *s).unwrap_or(0);
    let b_commit = backup.backup.map_or(0, |m| m.commit_seq).max(b_newest);
    if b_commit > valid_to.min(seq) {
        return Err(refuse(format!(
            "the backup in {} holds commit {b_commit}, after the graph in memory (commit {seq} \
             of its lineage): back up into a new, empty directory",
            target.display()
        )));
    }
    let lineage_changed = backup.lineage != graph_file.lineage;
    let taken_at = crate::graph::undo::now_micros().max(previous_marker.taken_at.saturating_add(1));
    let mut report = BackupReport {
        incremental: true,
        previous_commit_seq: b_commit,
        snapshot_commit_seq: b_newest,
        commit_seq: b_commit,
        lineage_changed,
        taken_at,
        from_memory: true,
        ..BackupReport::default()
    };
    if b_commit == seq && !lineage_changed {
        // The backup holds the graph's position already.
        let mut file = backup.clone();
        file.backup = Some(BackupMarker {
            commit_seq: b_commit,
            taken_at,
        });
        write_graph_file(target, &file)?;
        return Ok(report);
    }
    let next = seq.saturating_add(1);
    if list_wal(target)?.iter().any(|(first, _)| *first >= next) {
        return Err(refuse(format!(
            "the backup in {} has a WAL segment at or after commit {next}: back up into a new, \
             empty directory",
            target.display()
        )));
    }
    let intent = Intent {
        op: INCREMENT_OP.into(),
        files: vec![
            format!("{SNAPSHOTS_DIR}/{}", snapshot_name(seq)),
            format!("{WAL_DIR}/{}", wal_name(next)),
        ],
        started_at: taken_at,
        params: vec![
            (
                "graph_before".into(),
                super::rollback::hex_bytes(&backup.encode()),
            ),
            ("taken_at".into(), taken_at.to_string()),
        ],
    };
    intent.write(target)?;
    let result = (|| {
        if lineage_changed {
            // The new lineage chain first, so every new file is on it.
            let mut chained = backup.clone();
            chained.graph_id = graph_file.graph_id;
            chained.lineage = graph_file.lineage.clone();
            write_graph_file(target, &chained)?;
        }
        let written = write_files(store, graph, graph_file, write, target)?;
        let file = backup_graph_file(graph_file, seq, taken_at);
        write_graph_file(target, &file)?;
        let check = check_written(target, &file, seq);
        if !check.is_ok() {
            return Err(check.into_error(DamageSide::Backup, store, target.display(), true));
        }
        Intent::clear(target)?;
        Ok(written)
    })();
    match result {
        Ok(written) => {
            report.snapshots_copied = vec![seq];
            report.snapshot_commit_seq = seq;
            report.segments_copied = vec![next];
            report.commit_seq = seq;
            report.bytes = written.iter().map(|(_, len)| len).sum();
            report.files = written;
            Ok(report)
        }
        Err(err) => {
            roll_back_increment(target, &intent, &backup);
            Err(err)
        }
    }
}
