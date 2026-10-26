//! Backups (format spec 12.6 and 12.7): the backup marker, the choice between a full and an
//! incremental backup, the increment itself (new snapshots, new WAL segments, the tail of the
//! backup's last segment, under an `INTENT`), and the operations on a backup directory
//! (restore, prune, rolling back an interrupted increment). Every checksum of what an
//! increment copies is read in the store before anything is written and again in the copy
//! (`backup_check`): damage stops it with a report, and a started increment is rolled back.

use crate::persist::StoreFailure;
use std::io::{Read, Write};

use super::super::error::{DamageSide, PersistError};
use super::super::journal::DurablePosition;
use super::super::wal::FRAME_BYTES;
use super::super::wal::WAL_HEADER_BYTES;
use super::backup_check::{
    Findings, SegmentCheck, check_graph_files, check_marks, check_segment, check_snapshot,
};
use super::files::{
    self, BackupMarker, DirLock, DirPath, GRAPH_COPY, GRAPH_FILE, GraphFile, Intent, MARKS_FILE,
    SNAPSHOTS_DIR, TMP_PREFIX, WAL_DIR, list_snapshots, list_wal, read_graph_file, safe_relative,
    snapshot_name, sync_dir, wal_name, write_graph_file,
};
use super::maintenance::{BackupReport, copy_exact};
use super::snapshot_dir::{MANIFEST_FILE, SCHEMA_FILE, read_manifest, segment_path};
use super::wal_files::{scan_marks, write_marks};

/// The `INTENT` operation of an incremental backup (in the backup directory).
pub(super) const INCREMENT_OP: &str = "backup_increment";

/// The suffix of a WAL segment the increment is replacing (the old file, kept until the
/// increment is complete).
const REPLACED_SUFFIX: &str = ".replaced";

/// How [`Store::backup_with`](super::Store::backup_with) copies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum BackupMode {
    /// Incremental into a backup directory of the same store (only what the backup does not
    /// hold yet is copied), full into an empty or missing directory. The default.
    #[default]
    Auto,
    /// Always a full backup: the directory must be empty or missing.
    Full,
}

/// A refusal of an increment.
pub(super) fn refuse(reason: impl Into<String>) -> PersistError {
    PersistError::BackupRefused {
        reason: reason.into(),
    }
}

/// The identity check of an increment (format spec 12.7): the backup must be of THIS store,
/// not of a fork, repair or copy of it (which shares its lineage chain up to the copy).
pub(super) fn check_same_store(
    backup: &GraphFile,
    source: &GraphFile,
    target: &DirPath,
) -> Result<(), PersistError> {
    if backup.store_id == source.store_id {
        return Ok(());
    }
    Err(refuse(format!(
        "the backup in {} is of a different store (a fork, repair or copy of the backed-up          store; store id {}, this store's is {}): back it up into a new directory (a full          backup, --full)",
        target.display(),
        crate::value::uuid::format_canonical(backup.store_id),
        crate::value::uuid::format_canonical(source.store_id),
    )))
}

fn io(context: &'static str) -> impl Fn(std::io::Error) -> PersistError {
    move |err| PersistError::io(context, err)
}

fn now() -> i64 {
    crate::graph::undo::now_micros()
}

/// A backup into `target`: full when it is empty or missing, incremental when it holds a
/// backup of this store (and `mode` allows it); anything else is refused.
pub(super) fn backup_into(
    dir: &DirPath,
    durable: Option<DurablePosition>,
    target: &DirPath,
    mode: BackupMode,
) -> Result<BackupReport, PersistError> {
    // The source and an existing backup are both version-checked before anything is read or
    // written (format spec 15).
    files::refuse_other_version(dir)?;
    if !files::has_entries(target) {
        return super::maintenance::backup_to_dir(dir, durable, target);
    }
    if mode == BackupMode::Full {
        return Err(refuse(format!(
            "a full backup needs an empty or new directory, and {} is not empty",
            target.display()
        )));
    }
    if !target.join(GRAPH_FILE).exists() && !target.join(GRAPH_COPY).exists() {
        return Err(refuse(format!(
            "{} is not empty and holds no backup (no GRAPH file; an interrupted full backup? \
             remove it and back up again)",
            target.display()
        )));
    }
    files::refuse_other_version(target)?;
    let mut report = increment(dir, durable, target)?;
    // The models follow the store's (format spec 4.5): each one added or removed whole, after
    // the increment (they are no part of its INTENT; an interrupted sync is completed by the
    // next increment).
    super::models::sync_models(dir, target)?;
    report.models = super::models::list(target)?;
    Ok(report)
}

/// How far WAL segment `first` of the store may be copied, checked (format spec 12.6): up to
/// its durable end (an open store; a record boundary), its last complete record when it is the
/// last segment of a store another process writes, never past a recorded cut of a refused
/// record. `None`: the segment is after the durable end. `Some(None)`: damage (in `findings`).
#[allow(clippy::too_many_arguments)]
fn check_source_segment(
    dir: &DirPath,
    first: u64,
    path: &DirPath,
    durable: Option<DurablePosition>,
    cut: Option<(u64, u64)>,
    last: bool,
    chain: &GraphFile,
    findings: &mut Findings,
) -> Option<Option<SegmentCheck>> {
    let cut_at = super::cut_limit(cut, first);
    let (limit, torn_ok) = match durable {
        Some(d) if first > d.segment => return None,
        Some(d) if first == d.segment => {
            (Some(cut_at.map_or(d.offset, |c| c.min(d.offset))), false)
        }
        Some(_) => (cut_at, false),
        None => (cut_at, last),
    };
    Some(check_segment(
        dir,
        first,
        path,
        limit,
        torn_ok,
        Some(chain),
        findings,
    ))
}

/// Copies bytes `from..to` of `source` to `out`.
fn copy_range(
    source: &DirPath,
    from: u64,
    to: u64,
    out: &mut dyn Write,
) -> Result<(), PersistError> {
    let mut file = source
        .open_read()
        .map_err(io("reading a file to back up"))?;
    let skipped = std::io::copy(&mut (&mut file).take(from), &mut std::io::sink())
        .map_err(io("reading a file to back up"))?;
    let copied = std::io::copy(&mut file.take(to - from), out)
        .map_err(io("copying a file to the backup"))?;
    if skipped != from || copied != to - from {
        return Err(refuse(format!(
            "{} shrank while it was copied",
            source.display()
        )));
    }
    Ok(())
}

/// A whole WAL segment the increment copies, with what the check of the store's segment
/// found (the copy must read back the same).
struct SegmentCopy {
    first: u64,
    source: DirPath,
    check: SegmentCheck,
}

/// The bytes appended to the backup's last segment, with the store's checked segment.
struct TailCopy {
    first: u64,
    source: DirPath,
    from: u64,
    check: SegmentCheck,
}

/// A damage error of the backup directory the increment continues (or of its copy).
fn backup_damage(dir: &DirPath, target: &DirPath, findings: Findings) -> PersistError {
    findings.into_error(DamageSide::Backup, dir, target.display(), false)
}

fn increment(
    dir: &DirPath,
    durable: Option<DurablePosition>,
    target: &DirPath,
) -> Result<BackupReport, PersistError> {
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
    let _pin = files::BackupPin::shared(dir)?;
    // Every checksum of what the increment copies is read in the store first: damage stops it
    // before anything is written.
    let mut findings = Findings::default();
    // Read BEFORE the WAL ends are fixed: every mark in it is in the WAL copied.
    let source_marks = check_marks(dir, &mut findings);
    let Some(source) = check_graph_files(dir, &mut findings)? else {
        return Err(findings.into_error(DamageSide::Store, dir, target.display(), false));
    };

    // The backup must be of this store: its lineage chain the tail of the store's (the same
    // lineage, or an ancestor the store branched from by an in-place rollback).
    let position = source
        .lineage
        .iter()
        .position(|entry| entry.graph_id == backup.graph_id)
        .filter(|&k| source.lineage[k..] == backup.lineage[..]);
    let Some(k) = position else {
        return Err(refuse(format!(
            "the backup in {} is of another graph (graph id {}), not of this store (graph id \
             {}, {} lineage entr{}): back up into a new, empty directory",
            target.display(),
            crate::value::uuid::format_canonical(backup.graph_id),
            crate::value::uuid::format_canonical(source.graph_id),
            source.lineage.len(),
            if source.lineage.len() == 1 {
                "y"
            } else {
                "ies"
            }
        )));
    };
    check_same_store(&backup, &source, target)?;
    // The backup's lineage is the store's history only up to the EARLIEST branch point
    // after it (a rollback of a later lineage to before its own start branches from this one).
    let valid_to = source.lineage[..k]
        .iter()
        .map(|entry| entry.branched_at)
        .min()
        .unwrap_or(u64::MAX);

    // The backup's own WAL, checked: its position and its last segment's end.
    let b_snapshots = list_snapshots(target)?;
    let b_segments = list_wal(target)?;
    let mut b_findings = Findings::default();
    let mut b_wal_commit = 0;
    let mut b_last: Option<(u64, DirPath, SegmentCheck)> = None;
    for (index, (first, b_path)) in b_segments.iter().enumerate() {
        let last = index + 1 == b_segments.len();
        // Only the last segment is read whole; the commit position needs the newest commit.
        if !last {
            continue;
        }
        if let Some(check) = check_segment(
            target,
            *first,
            b_path,
            None,
            false,
            Some(&backup),
            &mut b_findings,
        ) {
            // Read to its end without a torn tail: it ends with a complete record.
            b_last = Some((*first, b_path.clone(), check));
        }
    }
    if !b_findings.is_ok() {
        return Err(backup_damage(dir, target, b_findings));
    }
    // The newest commit the backup's WAL holds (its last segment may hold none yet).
    for (first, b_path) in b_segments.iter().rev() {
        let mut scratch = Findings::default();
        let commits = check_segment(target, *first, b_path, None, true, None, &mut scratch)
            .map(|check| check.commits);
        if let Some(seq) = commits.as_ref().and_then(|c| c.last()).map(|(_, seq)| *seq) {
            b_wal_commit = seq;
            break;
        }
    }
    let b_newest = b_snapshots.last().map(|(seq, _)| *seq);
    let b_commit = b_wal_commit.max(b_newest.unwrap_or(0));
    if b_commit > valid_to {
        return Err(refuse(format!(
            "the store was rolled back in place to commit {valid_to}, before the backup's \
             commit {b_commit}: the backup holds history the store no longer has (it is in the \
             store's attic). Back up into a NEW directory (a full backup, --full); keep this \
             backup as the archive of the abandoned history"
        )));
    }

    // The WAL: the tail of the backup's last segment, then every later segment.
    let mut s_segments = list_wal(dir)?;
    super::drop_refused_segments(&mut s_segments, source.wal_cut);
    let mut set_file = source.clone();
    set_file.wal_cut = source.wal_cut;
    for (name, reason) in super::wal_files::wal_set_problems(&s_segments, &set_file) {
        findings.problem(name, None, reason);
    }
    let s_last = s_segments.last().map(|(first, _)| *first);
    let mut tail = None;
    let mut replace = None;
    let mut copies = Vec::new();
    let gap = |first: u64| {
        refuse(format!(
            "the store no longer has the WAL after the backup's commit {b_commit} (it was \
             pruned: the store's WAL continues at segment {first}; or the backup's last \
             increment was a backup from memory); a full backup is needed: back up into a new, \
             empty directory (--full)"
        ))
    };
    // The first segment name copied whole.
    let copy_from = match &b_last {
        Some((first, b_path, scan)) => {
            let b_len = scan.end;
            match s_segments.iter().find(|(f, _)| f == first) {
                Some((_, s_path)) => {
                    let checked = check_source_segment(
                        dir,
                        *first,
                        s_path,
                        durable,
                        source.wal_cut,
                        s_last == Some(*first),
                        &source,
                        &mut findings,
                    );
                    let Some(checked) = checked else {
                        return Err(not_a_prefix(target, *first));
                    };
                    let Some(check) = checked else {
                        return Err(findings.into_error(
                            DamageSide::Store,
                            dir,
                            target.display(),
                            false,
                        ));
                    };
                    let header = |path: &DirPath| {
                        path.read_range(0, WAL_HEADER_BYTES)
                            .map_err(io("reading a WAL segment header"))
                    };
                    if header(b_path)? == header(s_path)? {
                        if check.end < b_len {
                            return Err(not_a_prefix(target, *first));
                        }
                        if let Some(at) = scan.last_record {
                            let frame = |path: &DirPath| {
                                path.read_range(at, FRAME_BYTES)
                                    .map_err(io("reading a WAL record frame"))
                            };
                            if frame(b_path)? != frame(s_path)? {
                                return Err(not_a_prefix(target, *first));
                            }
                        }
                        if check.end > b_len {
                            tail = Some(TailCopy {
                                first: *first,
                                source: s_path.clone(),
                                from: b_len,
                                check,
                            });
                        }
                    } else if scan.commits.is_empty() && valid_to != u64::MAX {
                        // A rollback to the backup's position started its lineage's segment
                        // under this name, with the records (marks) the old one held: the
                        // store's segment replaces the backup's.
                        replace = Some(SegmentCopy {
                            first: *first,
                            source: s_path.clone(),
                            check,
                        });
                    } else {
                        return Err(not_a_prefix(target, *first));
                    }
                }
                // The store pruned the backup's last segment: the next one must hold the
                // backup's next commit.
                None => match s_segments.iter().find(|(f, _)| f > first) {
                    Some((f, _)) if *f <= b_commit.saturating_add(1) => {}
                    Some((f, _)) => return Err(gap(*f)),
                    None => return Err(not_a_prefix(target, *first)),
                },
            }
            first.saturating_add(1)
        }
        // A backup without WAL: everything from the segment holding the next commit on.
        None => match super::files::wal_after(&s_segments, b_commit).first() {
            Some((f, _)) if *f <= b_commit.saturating_add(1) => *f,
            Some((f, _)) => return Err(gap(*f)),
            None => u64::MAX,
        },
    };
    for (first, path) in &s_segments {
        if *first < copy_from {
            continue;
        }
        match check_source_segment(
            dir,
            *first,
            path,
            durable,
            source.wal_cut,
            s_last == Some(*first),
            &source,
            &mut findings,
        ) {
            None => break,
            Some(Some(check)) => copies.push(SegmentCopy {
                first: *first,
                source: path.clone(),
                check,
            }),
            Some(None) => {}
        }
    }

    // Snapshots the backup does not hold yet (newer than its newest one), checked, and the
    // files of older snapshots they reference that the backup lacks.
    let mut snapshot_copies = Vec::new();
    let mut retained: Vec<(String, DirPath, u64)> = Vec::new();
    for (seq, path) in list_snapshots(dir)? {
        if b_newest.is_some_and(|newest| seq <= newest) {
            continue;
        }
        let healed = check_snapshot(dir, &path, true, &mut findings);
        if !findings.is_ok() {
            continue;
        }
        let (_, manifest) = read_manifest(&path)?;
        let mut files = vec![MANIFEST_FILE.to_string(), SCHEMA_FILE.to_string()];
        for entry in &manifest.segments {
            if entry.path.contains('/') {
                // A segment of an older snapshot (an incremental snapshot): one the backup
                // does not hold yet (its snapshot was written and pruned since the last
                // backup, a holder directory of the store, format spec 4.1) is copied with
                // the increment, before the snapshots that use it.
                let own = target.join(SNAPSHOTS_DIR).join(snapshot_name(seq));
                let held = segment_path(&own, &entry.path)
                    .is_some_and(|p| p.len().is_ok_and(|len| len == entry.byte_len));
                let relative = format!("{SNAPSHOTS_DIR}/{}", entry.path);
                if !held && !retained.iter().any(|(r, _, _)| *r == relative) {
                    let from = segment_path(&path, &entry.path)
                        .filter(|p| p.len().is_ok_and(|len| len == entry.byte_len))
                        .ok_or_else(|| {
                            refuse(format!(
                                "the snapshot at commit {seq} uses the file {} of an older \
                                 snapshot that neither the backup nor the store holds",
                                entry.path
                            ))
                        })?;
                    retained.push((relative, from, entry.byte_len));
                }
            } else {
                files.push(entry.path.clone());
            }
        }
        snapshot_copies.push(SnapshotCopy {
            seq,
            source: path,
            files,
            healed,
        });
    }

    // The copied WAL continues the backup's without a gap or an overlap.
    let copied = copied_commits(tail.as_ref(), replace.as_ref(), &copies);
    check_wal_continues(b_wal_commit, b_commit, &copied, &mut findings);
    if !findings.is_ok() {
        return Err(findings.into_error(DamageSide::Store, dir, target.display(), false));
    }

    let lineage_changed = backup.lineage != source.lineage;
    let taken_at = now().max(previous_marker.taken_at.saturating_add(1));
    let mut report = BackupReport {
        incremental: true,
        previous_commit_seq: b_commit,
        snapshot_commit_seq: b_newest.unwrap_or(0),
        commit_seq: b_commit,
        lineage_changed,
        taken_at,
        warnings: std::mem::take(&mut findings.warnings),
        ..BackupReport::default()
    };
    if tail.is_none()
        && replace.is_none()
        && copies.is_empty()
        && snapshot_copies.is_empty()
        && retained.is_empty()
        && !lineage_changed
    {
        // Nothing new: the backup is current as of now.
        let mut file = backup.clone();
        file.backup = Some(BackupMarker {
            commit_seq: b_commit,
            taken_at,
        });
        file.closed_at = Some(b_commit);
        write_graph_file(target, &file)?;
        return Ok(report);
    }

    let mut intent = Intent {
        op: INCREMENT_OP.into(),
        files: Vec::new(),
        started_at: taken_at,
        params: vec![
            (
                "graph_before".into(),
                super::rollback::hex_bytes(&backup.encode()),
            ),
            ("taken_at".into(), taken_at.to_string()),
        ],
    };
    for (relative, _, _) in &retained {
        intent.files.push(relative.clone());
    }
    for copy in &snapshot_copies {
        intent
            .files
            .push(format!("{SNAPSHOTS_DIR}/{}", snapshot_name(copy.seq)));
    }
    for copy in &copies {
        intent
            .files
            .push(format!("{WAL_DIR}/{}", wal_name(copy.first)));
    }
    if let Some(tail) = &tail {
        intent
            .params
            .push(("tail".into(), format!("{WAL_DIR}/{}", wal_name(tail.first))));
        intent
            .params
            .push(("tail_len".into(), tail.from.to_string()));
    }
    if let Some(replace) = &replace {
        intent.params.push((
            "replaced".into(),
            format!("{WAL_DIR}/{}", wal_name(replace.first)),
        ));
    }
    intent.write(target)?;
    let plan = Plan {
        snapshots: &snapshot_copies,
        retained: &retained,
        tail: tail.as_ref(),
        replace: replace.as_ref(),
        copies: &copies,
        source_marks: source_marks.as_deref(),
    };
    let result = apply_increment(target, &backup, &source, &mut report, &plan);
    let result = match result {
        // The copy did not read back intact: the store's files again, to tell whose damage it
        // is (a store file that changed under the copy is the store's).
        Err(Copied::Damage(copy)) => Err(match recheck_sources(dir, &plan, &source) {
            Some(again) => again.into_error(DamageSide::Store, dir, target.display(), false),
            None => backup_damage(dir, target, copy),
        }),
        Err(Copied::Failed(err)) => Err(err),
        Ok(()) => Ok(()),
    };
    match result {
        Ok(()) => Ok(report),
        Err(err) => {
            roll_back_increment(target, &intent, &backup);
            Err(err)
        }
    }
}

/// Rolls a failed increment back (format spec 12.7): the backup's old `GRAPH` first, so the
/// rollback never takes a final `GRAPH` that did not read back intact for a completed
/// increment, then every step of [`undo_increment`]. A failure is logged: the interrupted
/// increment is rolled back at the next backup, restore or prune of the backup.
pub(super) fn roll_back_increment(target: &DirPath, intent: &Intent, before: &GraphFile) {
    let result = write_graph_file(target, before).and_then(|()| undo_increment(target, intent));
    if let Err(undo) = result {
        tracing::error!(
            "rolling back the failed incremental backup in {} failed too ({undo}); it is \
             rolled back at the next backup, restore or prune of it",
            target.display()
        );
    }
}

/// The commits an increment copies, in order: the tail's (after the backup's end), the
/// replacing segment's, every new segment's.
fn copied_commits(
    tail: Option<&TailCopy>,
    replace: Option<&SegmentCopy>,
    copies: &[SegmentCopy],
) -> Vec<(u64, u64, u64)> {
    let mut out = Vec::new();
    if let Some(tail) = tail {
        out.extend(
            tail.check
                .commits
                .iter()
                .filter(|(start, _)| *start >= tail.from)
                .map(|(start, seq)| (tail.first, *start, *seq)),
        );
    }
    for copy in replace.into_iter().chain(copies) {
        out.extend(
            copy.check
                .commits
                .iter()
                .map(|(start, seq)| (copy.first, *start, *seq)),
        );
    }
    out
}

/// The copied commits continue the backup's WAL (its last commit `prev_wal_commit`, its
/// position `position`) without a gap or an overlap.
fn check_wal_continues(
    prev_wal_commit: u64,
    position: u64,
    commits: &[(u64, u64, u64)],
    findings: &mut Findings,
) {
    let mut previous = prev_wal_commit;
    for (i, (first, start, seq)) in commits.iter().enumerate() {
        let fits = if i == 0 && previous == 0 {
            *seq <= position.saturating_add(1)
        } else {
            *seq == previous.saturating_add(1)
        };
        if !fits {
            findings.problem(
                format!("{WAL_DIR}/{}", wal_name(*first)),
                Some(*start),
                format!(
                    "commit {seq} after commit {previous}: the store's WAL does not continue \
                     the backup's (a gap or an overlap)"
                ),
            );
            return;
        }
        previous = *seq;
    }
}

/// The store's files an increment copied, checked again: `Some` with the problems when they
/// fail now.
fn recheck_sources(dir: &DirPath, plan: &Plan<'_>, chain: &GraphFile) -> Option<Findings> {
    let mut again = Findings::default();
    for copy in plan.snapshots {
        check_snapshot(dir, &copy.source, true, &mut again);
    }
    let segments = plan
        .tail
        .map(|t| (t.first, &t.source, t.check.end))
        .into_iter()
        .chain(plan.replace.map(|r| (r.first, &r.source, r.check.end)))
        .chain(
            plan.copies
                .iter()
                .map(|c| (c.first, &c.source, c.check.end)),
        );
    for (first, path, end) in segments {
        check_segment(dir, first, path, Some(end), false, Some(chain), &mut again);
    }
    (!again.is_ok()).then_some(again)
}

fn not_a_prefix(target: &DirPath, first: u64) -> PersistError {
    refuse(format!(
        "the backup's WAL segment {} is not a prefix of the store's segment of that name (the \
         backup in {} holds a history the store does not have, its last increment was a \
         backup from memory, or an attic restore split that segment anew); back up into a new, empty directory (--full)",
        wal_name(first),
        target.display()
    ))
}

/// A snapshot the increment copies: its commit, its directory in the store, its file names, and
/// its manifest with a damaged fixed copy healed (when one was).
struct SnapshotCopy {
    seq: u64,
    source: DirPath,
    files: Vec<String>,
    healed: Option<Vec<u8>>,
}

/// What [`apply_increment`] copies.
struct Plan<'a> {
    snapshots: &'a [SnapshotCopy],
    /// Files of older snapshots the new ones reference: `(name in the backup, source, length)`.
    retained: &'a [(String, DirPath, u64)],
    tail: Option<&'a TailCopy>,
    replace: Option<&'a SegmentCopy>,
    copies: &'a [SegmentCopy],
    source_marks: Option<&'a [u8]>,
}

/// Why [`apply_increment`] stopped.
enum Copied {
    /// A copy did not read back as the store's checked file: the problems (in the backup).
    Damage(Findings),
    /// Any other failure (a write, a sync, a rename).
    Failed(PersistError),
}

impl From<PersistError> for Copied {
    fn from(err: PersistError) -> Self {
        Copied::Failed(err)
    }
}

/// The steps of an increment after its `INTENT` was written. The backup is a valid store after
/// each one: a snapshot appears whole (renamed after its check), a WAL segment appears whole
/// (renamed after its check), the tail is complete records only, and the lineage chain is
/// written first when it changed. Every copy is read back and checked against the store's
/// checked file before it counts.
fn apply_increment(
    target: &DirPath,
    backup: &GraphFile,
    source: &GraphFile,
    report: &mut BackupReport,
    plan: &Plan<'_>,
) -> Result<(), Copied> {
    // A rollback of the store: the new lineage chain first, so every new file is on it.
    if report.lineage_changed {
        let mut chained = backup.clone();
        chained.graph_id = source.graph_id;
        chained.lineage = source.lineage.clone();
        write_graph_file(target, &chained)?;
    }
    let snapshots_dir = target.join(SNAPSHOTS_DIR);
    snapshots_dir
        .create_dir_all()
        .map_err(io("creating the backup directory"))?;
    // Referenced files first (written under their names; the INTENT lists them, so an
    // interrupted increment removes them again; the snapshots that use them check them).
    for (relative, from, len) in plan.retained {
        let to = target.join(relative);
        if let Some(parent) = to.parent() {
            parent
                .create_dir_all()
                .map_err(io("creating a snapshot in the backup"))?;
        }
        let mut out = to.create(true).map_err(io("writing the backup"))?;
        copy_exact(from, *len, &mut out)?;
        out.sync().map_err(io("writing the backup"))?;
        drop(out);
        if let Some(parent) = to.parent() {
            sync_dir(&parent)?;
        }
        report.files.push((relative.clone(), *len));
        report.bytes += len;
    }
    for SnapshotCopy {
        seq,
        source: source_dir,
        files: names,
        healed,
    } in plan.snapshots
    {
        let tmp = snapshots_dir.join(format!("{TMP_PREFIX}{:032x}", crate::value::uuid::new_v7()));
        tmp.create_dir_all()
            .map_err(io("creating a snapshot in the backup"))?;
        for name in names {
            let from = source_dir.join(name);
            let mut out = tmp
                .join(name)
                .create(true)
                .map_err(io("writing the backup"))?;
            let len = match healed.as_ref().filter(|_| name == MANIFEST_FILE) {
                Some(bytes) => {
                    out.write_all(bytes).map_err(io("writing the backup"))?;
                    bytes.len() as u64
                }
                None => {
                    let len = from.len().map_err(io("reading a file to back up"))?;
                    copy_exact(&from, len, &mut out)?;
                    len
                }
            };
            out.sync().map_err(io("writing the backup"))?;
            report.files.push((
                format!("{SNAPSHOTS_DIR}/{}/{name}", snapshot_name(*seq)),
                len,
            ));
            report.bytes += len;
        }
        sync_dir(&tmp)?;
        let mut copy = Findings::default();
        check_snapshot(target, &tmp, false, &mut copy);
        if !copy.is_ok() {
            return Err(Copied::Damage(copy));
        }
        tmp.rename_to(&snapshots_dir.join(snapshot_name(*seq)))
            .map_err(io("renaming a snapshot in the backup"))?;
        sync_dir(&snapshots_dir)?;
        report.snapshots_copied.push(*seq);
        report.snapshot_commit_seq = report.snapshot_commit_seq.max(*seq);
        report.commit_seq = report.commit_seq.max(*seq);
    }

    let wal_dir = target.join(WAL_DIR);
    wal_dir
        .create_dir_all()
        .map_err(io("creating the backup directory"))?;
    if let Some(tail) = plan.tail {
        let path = wal_dir.join(wal_name(tail.first));
        let mut out = path
            .open_write(tail.from)
            .map_err(io("appending to the backup's WAL"))?;
        copy_range(&tail.source, tail.from, tail.check.end, &mut out)?;
        out.sync().map_err(io("appending to the backup's WAL"))?;
        drop(out);
        read_back(target, tail.first, &path, &tail.check, source)?;
        report.tail_bytes = tail.check.end - tail.from;
        report.bytes += report.tail_bytes;
        report.files.push((
            format!("{WAL_DIR}/{}", wal_name(tail.first)),
            report.tail_bytes,
        ));
    }
    if let Some(replace) = plan.replace {
        let name = wal_name(replace.first);
        let fresh = copy_segment(target, &wal_dir, replace, source)?;
        let current = wal_dir.join(&name);
        current
            .rename_to(&wal_dir.join(format!("{name}{REPLACED_SUFFIX}")))
            .map_err(io("replacing a WAL segment of the backup"))?;
        fresh
            .rename_to(&current)
            .map_err(io("replacing a WAL segment of the backup"))?;
        sync_dir(&wal_dir)?;
        report.segments_copied.push(replace.first);
        report.bytes += replace.check.end;
        report
            .files
            .push((format!("{WAL_DIR}/{name}"), replace.check.end));
    }
    for copy in plan.copies {
        let fresh = copy_segment(target, &wal_dir, copy, source)?;
        fresh
            .rename_to(&wal_dir.join(wal_name(copy.first)))
            .map_err(io("renaming a WAL segment in the backup"))?;
        sync_dir(&wal_dir)?;
        report.segments_copied.push(copy.first);
        report.bytes += copy.check.end;
        report.files.push((
            format!("{WAL_DIR}/{}", wal_name(copy.first)),
            copy.check.end,
        ));
    }
    let commits = copied_commits(plan.tail, plan.replace, plan.copies);
    if let Some((_, _, last)) = commits.last() {
        report.commit_seq = report.commit_seq.max(*last);
    }

    // The marks (all of them are in the WAL copied), then the GRAPH file with the new marker.
    let marks = match plan.source_marks {
        Some(bytes) => bytes.to_vec(),
        None => super::wal_files::marks_bytes(&scan_marks(
            &list_wal(target)?,
            crate::value::depth::DEFAULT_MAX_VALUE_DEPTH,
        )),
    };
    files::write_atomic(target, MARKS_FILE, &marks)?;
    let mut file = source.clone();
    file.clean = true;
    file.wal_cut = None;
    file.backup = Some(BackupMarker {
        commit_seq: report.commit_seq,
        taken_at: report.taken_at,
    });
    // The backup's own newest segment and position (format spec 9.8), not the source's.
    file.wal_head = list_wal(target)?.last().map(|(first, _)| *first);
    file.closed_at = Some(report.commit_seq);
    write_graph_file(target, &file)?;
    // GRAPH and marks read back: as written.
    let written = file.encode();
    let mut copy = Findings::default();
    for (name, written) in [
        (GRAPH_FILE, &written),
        (GRAPH_COPY, &written),
        (MARKS_FILE, &marks),
    ] {
        match files::read_bounded(&target.join(name), 256 << 20) {
            Ok(bytes) if bytes == *written => {}
            Ok(_) => copy.problem(name, None, "differs from what was written".into()),
            Err(err) => copy.problem(name, None, err.to_string()),
        }
    }
    if !copy.is_ok() {
        return Err(Copied::Damage(copy));
    }
    if let Some(replace) = plan.replace {
        let _ = wal_dir
            .join(format!("{}{REPLACED_SUFFIX}", wal_name(replace.first)))
            .remove();
    }
    Intent::clear(target)?;
    Ok(())
}

/// Reads a WAL segment of the backup back: every record's CRCs, complete records only, the
/// end and the commits of the store's checked segment (`chain`: the store's lineage chain).
fn read_back(
    target: &DirPath,
    first: u64,
    path: &DirPath,
    expected: &SegmentCheck,
    chain: &GraphFile,
) -> Result<(), Copied> {
    let mut copy = Findings::default();
    if let Some(check) = check_segment(target, first, path, None, false, Some(chain), &mut copy)
        && (check.end != expected.end || check.commits != expected.commits)
    {
        copy.problem(
            path.relative_to(target),
            None,
            format!(
                "the copy ends at byte {} with {} commit(s), the store's segment at byte {} \
                 with {}",
                check.end,
                check.commits.len(),
                expected.end,
                expected.commits.len()
            ),
        );
    }
    if copy.is_ok() {
        Ok(())
    } else {
        Err(Copied::Damage(copy))
    }
}

/// Copies a whole WAL segment into `wal/<name>.tmp` of the backup, synced and read back
/// against the store's checked segment; returns the temporary file.
fn copy_segment(
    target: &DirPath,
    wal_dir: &DirPath,
    copy: &SegmentCopy,
    source: &GraphFile,
) -> Result<DirPath, Copied> {
    let tmp = wal_dir.join(format!("{}.tmp", wal_name(copy.first)));
    let mut out = tmp.create(false).map_err(io("writing the backup"))?;
    copy_range(&copy.source, 0, copy.check.end, &mut out)?;
    out.sync().map_err(io("writing the backup"))?;
    drop(out);
    read_back(target, copy.first, &tmp, &copy.check, source)?;
    Ok(tmp)
}

/// Completes or rolls back an interrupted operation in a backup directory (format spec 12.7):
/// an increment whose `GRAPH` file was written is complete (only the clean-up remains),
/// otherwise it is rolled back; an interrupted prune is completed. Returns the operation.
pub(super) fn recover_interrupted(target: &DirPath) -> Result<Option<String>, PersistError> {
    let Some(intent) = Intent::read(target)? else {
        return Ok(None);
    };
    super::complete_intent(target, &intent)?;
    Ok(Some(intent.op))
}

fn intent_path(target: &DirPath, relative: &str) -> Result<DirPath, PersistError> {
    safe_relative(target, relative).ok_or_else(|| {
        PersistError::corrupt(files::INTENT_FILE, 0, format!("unsafe path {relative:?}"))
    })
}

/// Rolls back an interrupted increment (idempotent steps), or only cleans up when its last
/// step (the `GRAPH` file) was done; clears the intent.
pub(super) fn undo_increment(target: &DirPath, intent: &Intent) -> Result<(), PersistError> {
    let bad = |what: &str| PersistError::corrupt(files::INTENT_FILE, 0, what.to_string());
    let taken_at: i64 = intent
        .param("taken_at")
        .and_then(|t| t.parse().ok())
        .ok_or_else(|| bad("no \"taken_at\""))?;
    let complete = read_graph_file(target)
        .ok()
        .and_then(|read| read.file.backup)
        .is_some_and(|marker| marker.taken_at == taken_at);
    let replaced = intent
        .param("replaced")
        .map(|rel| intent_path(target, rel))
        .transpose()?;
    if complete {
        if let Some(segment) = &replaced {
            let old = DirPath::join(
                &segment.parent().unwrap_or_else(|| target.clone()),
                format!("{}{REPLACED_SUFFIX}", segment.file_name()),
            );
            old.remove()
                .map_err(io("removing a replaced WAL segment"))?;
        }
        return Intent::clear(target);
    }
    for relative in &intent.files {
        intent_path(target, relative)?
            .remove()
            .map_err(io("rolling back an incremental backup"))?;
    }
    // Temporary copies of snapshots and segments.
    for (sub, is_tmp) in [
        (
            SNAPSHOTS_DIR,
            (|name: &str| name.starts_with(TMP_PREFIX)) as fn(&str) -> bool,
        ),
        (WAL_DIR, |name: &str| name.ends_with(".tmp")),
    ] {
        let dir = target.join(sub);
        if let Ok(entries) = dir.list() {
            for (name, _) in entries {
                if is_tmp(&name) {
                    dir.join(&name)
                        .remove()
                        .map_err(io("rolling back an incremental backup"))?;
                }
            }
        }
    }
    if let Some(segment) = &replaced {
        let old = DirPath::join(
            &segment.parent().unwrap_or_else(|| target.clone()),
            format!("{}{REPLACED_SUFFIX}", segment.file_name()),
        );
        if old.exists() {
            old.rename_to(segment)
                .map_err(io("rolling back an incremental backup"))?;
        }
    }
    if let Some(tail) = intent.param("tail") {
        let path = intent_path(target, tail)?;
        let len: u64 = intent
            .param("tail_len")
            .and_then(|l| l.parse().ok())
            .ok_or_else(|| bad("no \"tail_len\""))?;
        if path
            .len()
            .map_err(io("rolling back an incremental backup"))?
            > len
        {
            path.truncate(len)
                .map_err(io("rolling back an incremental backup"))?;
        }
    }
    sync_dir(&target.join(SNAPSHOTS_DIR))?;
    sync_dir(&target.join(WAL_DIR))?;
    let before = intent
        .param("graph_before")
        .and_then(super::rollback::parse_hex_bytes)
        .ok_or_else(|| bad("no \"graph_before\""))?;
    let before = GraphFile::decode(&before).map_err(|reason| bad(&reason))?;
    write_graph_file(target, &before)?;
    // The marks of the WAL left (no marks: no file, as before the increment wrote one).
    let marks = scan_marks(
        &list_wal(target)?,
        crate::value::depth::DEFAULT_MAX_VALUE_DEPTH,
    );
    if marks.is_empty() {
        target
            .join(MARKS_FILE)
            .remove()
            .map_err(io("rolling back an incremental backup"))?;
    } else {
        write_marks(target, &marks)?;
    }
    Intent::clear(target)
}

/// Clears the backup marker of the backup in `dir` IN PLACE: it becomes the live store with
/// the same lineage (an interrupted increment is rolled back first).
pub(super) fn restore(dir: &DirPath) -> Result<BackupMarker, PersistError> {
    files::refuse_other_version(dir)?;
    let _lock = DirLock::exclusive(dir)?;
    let mut file = read_graph_file(dir)?.file;
    if file.backup.is_none() {
        return Err(not_a_backup(dir, "restore the backup"));
    }
    recover_interrupted(dir)?;
    file = read_graph_file(dir)?.file;
    super::guard_version(dir, file.version)?;
    let Some(marker) = file.backup.take() else {
        return Err(not_a_backup(dir, "restore the backup"));
    };
    file.clean = true;
    write_graph_file(dir, &file)?;
    Ok(marker)
}

/// Prunes the backup in `dir` (its own retention, the store's rules).
pub(super) fn prune(
    dir: &DirPath,
    up_to: u64,
) -> Result<super::maintenance::PruneReport, PersistError> {
    files::refuse_other_version(dir)?;
    let _lock = DirLock::exclusive(dir)?;
    if read_graph_file(dir)?.file.backup.is_none() {
        return Err(not_a_backup(dir, "prune the backup"));
    }
    recover_interrupted(dir)?;
    super::maintenance::prune(dir, up_to)
}

fn not_a_backup(dir: &DirPath, operation: &'static str) -> PersistError {
    PersistError::store(
        operation,
        StoreFailure::NotABackup,
        format!(
            "{} is not a backup (its GRAPH file has no backup marker); open it as a store \
             (Store::open; CLI: graphersal --graph {})",
            dir.display(),
            dir.display()
        ),
    )
}

/// The marker of the backup in `dir` (`None`: a store, not a backup).
pub(super) fn marker_of(dir: &DirPath) -> Result<Option<BackupMarker>, PersistError> {
    files::refuse_other_version(dir)?;
    Ok(read_graph_file(dir)?.file.backup)
}
