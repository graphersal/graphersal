//! Store maintenance (format spec sections 12 and 13): prune with donor retention, the scrub
//! (`verify`), file-level backups and the change stream.

use crate::graph::PersistentStorage;
use crate::persist::StoreFailure;
use std::collections::{BTreeSet, VecDeque};
use std::io::{Read, Write};

use super::super::error::DamageSide;
use super::super::error::PersistError;
use super::super::journal::DurablePosition;
use super::super::manifest::{FIXED_BYTES, ManifestFixed};
use super::super::snapshot::{ReadOptions, SnapshotInfo, read_snapshot};
use super::super::wal::{JournalReader, JournalRecord, WAL_HEADER_BYTES};
#[cfg(feature = "persist-zip")]
use super::backup_check::check_segment_stream;
use super::backup_check::{
    Findings, SegmentCheck, check_continuity, check_graph_files, check_marks, check_segment,
    check_snapshot,
};
use super::files::{
    BackupPin, DirPath, GRAPH_COPY, GRAPH_FILE, GraphFile, Intent, MARKS_FILE, SNAPSHOTS_DIR,
    WAL_DIR, list_snapshots, list_wal, read_bounded, read_graph_file, sync_dir, wal_after,
};
use super::snapshot_dir::{DirSnapshotReader, read_manifest, segment_path, verify_snapshot_dir};
use super::wal_files::{decode_marks, open_segment_reader, read_marks};
use crate::graph::change::ChangeSet;

/// What [`Store::prune`](super::Store::prune) removed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct PruneReport {
    /// The oldest snapshot kept: every target from its position on stays reachable.
    pub kept_from: u64,
    /// The snapshots removed (their commit sequence numbers).
    pub snapshots_removed: Vec<u64>,
    /// The WAL segments removed (their first commit sequence numbers).
    pub segments_removed: Vec<u64>,
    /// Attic entries whose base snapshot was kept for them (remove an entry with
    /// [`Store::attic_remove`](super::Store::attic_remove) to let prune go further).
    pub kept_for_attic: Vec<String>,
    /// Segment files of removed snapshots that kept snapshots (incremental ones, format spec
    /// 6.2) still reference: they stay, relative to the store directory, in a snapshot
    /// directory without a manifest (format spec 4.1) until no kept snapshot uses them.
    pub files_retained: Vec<String>,
    /// The marks (names) a target could reach before and cannot any more: they lie before the
    /// oldest kept snapshot (format spec 10). [`Store::marks`](super::Store::marks) leaves them
    /// out from now on; their names stay taken.
    pub marks_unreachable: Vec<String>,
}

/// One problem a scrub found.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct VerifyProblem {
    /// The file, relative to the store directory.
    pub path: String,
    /// The byte offset in it, when known.
    pub offset: Option<u64>,
    /// What is wrong.
    pub reason: String,
}

/// The result of a scrub ([`Store::verify`](super::Store::verify)).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct VerifyReport {
    /// Snapshots checked (fully decoded).
    pub snapshots_checked: usize,
    /// WAL segments checked.
    pub segments_checked: usize,
    /// WAL records checked.
    pub records_checked: u64,
    /// The first and last commit found in the WAL (`0`: none).
    pub first_commit: u64,
    /// See [`Self::first_commit`].
    pub last_commit: u64,
    /// Damage and inconsistencies: empty when the store is intact.
    pub problems: Vec<VerifyProblem>,
    /// Observations that are not damage (an incomplete last record of a running writer, a
    /// `marks` file that will be rebuilt, a segment without a donor).
    pub notes: Vec<String>,
    /// A WARNING, not damage (format spec 12.3, 13.1): the segment files (relative to the
    /// store) the newest snapshot shares with the previous one, so their id ranges have no
    /// independent donor. Checkpoints never write such a snapshot; the next checkpoint writes
    /// copies. Each one is also in [`Self::notes`].
    pub without_donor: Vec<String>,
    /// Embedding models checked (every file against its manifest's SHA-256, format spec 4.5).
    /// A damaged model is a problem with a path under `models/`; it does not make an open
    /// store read-only (it is no graph data).
    pub models_checked: usize,
}

impl VerifyReport {
    /// Whether no problem was found.
    pub fn is_ok(&self) -> bool {
        self.problems.is_empty()
    }
}

/// What a backup copied.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct BackupReport {
    /// The newest snapshot the backup holds (its commit sequence number).
    pub snapshot_commit_seq: u64,
    /// The position the backup holds now: the WAL copied up to the durable end (the last
    /// valid record for a backup by another process), or the newest snapshot when later.
    pub commit_seq: u64,
    /// Whether this continued an existing backup (an increment) rather than a full copy.
    pub incremental: bool,
    /// The position the backup held before an increment (`0` for a full backup).
    pub previous_commit_seq: u64,
    /// The snapshots copied (their commit sequence numbers).
    pub snapshots_copied: Vec<u64>,
    /// The WAL segments copied whole (their first commit sequence numbers).
    pub segments_copied: Vec<u64>,
    /// The bytes appended to the backup's last WAL segment (an increment).
    pub tail_bytes: u64,
    /// The store was rolled back in place since the last backup (at or after the backup's
    /// position): the backup now follows the store's new lineage.
    pub lineage_changed: bool,
    /// When the backup was taken (the time in its marker), microseconds since the Unix epoch.
    pub taken_at: i64,
    /// Every file written (or appended to), relative to the backup root, with the bytes.
    pub files: Vec<(String, u64)>,
    /// The bytes written in total.
    pub bytes: u64,
    /// Damage of redundant metadata in the store that did not stop the backup (one `GRAPH`
    /// copy, one copy of a manifest's fixed part, the derived `marks` file; format spec 13.2):
    /// the backup is intact, plan a repair of the store.
    pub warnings: Vec<String>,
    /// Whether the backup was written from the graph in memory
    /// ([`Store::backup_from_memory`](super::Store::backup_from_memory)).
    pub from_memory: bool,
    /// The embedding models the backup holds (format spec 4.5) with their licences: who hands
    /// the backup on redistributes them.
    pub models: Vec<super::StoredModel>,
}

fn relative(dir: &DirPath, path: &DirPath) -> String {
    path.relative_to(dir)
}

/// What a prune would remove (no data is read: names, the attic bases given, and the `marks`
/// file when a snapshot goes).
pub(super) struct PrunePlan {
    pub(super) report: PruneReport,
    /// The snapshot directories and WAL segments to remove, relative to the store.
    pub(super) doomed: Vec<String>,
    snapshots: Vec<(u64, DirPath)>,
}

/// Plans a prune up to `up_to` (format spec 12.4): the newest snapshot at or before it, the last
/// two snapshots and every attic base (`attic_bases`: entry id, base snapshot) stay.
pub(super) fn plan_prune(
    dir: &DirPath,
    up_to: u64,
    attic_bases: Vec<(String, u64)>,
) -> Result<PrunePlan, PersistError> {
    let snapshots = list_snapshots(dir)?;
    let mut report = PruneReport::default();
    let Some(base) = snapshots.iter().rev().find(|(seq, _)| *seq <= up_to) else {
        report.kept_from = snapshots.first().map_or(0, |(seq, _)| *seq);
        return Ok(PrunePlan {
            report,
            doomed: Vec::new(),
            snapshots,
        });
    };
    // Donor retention: the last two snapshots and the WAL between them always stay.
    let second_newest = snapshots
        .len()
        .checked_sub(2)
        .map_or(base.0, |i| snapshots[i].0);
    let mut kept_from = base.0.min(second_newest);
    // The attic entries' bases stay (with the WAL after them): an entry is replayed onto them.
    for (id, needed) in attic_bases {
        if needed < base.0.min(second_newest) {
            report.kept_for_attic.push(id);
        }
        kept_from = kept_from.min(needed);
    }
    report.kept_from = kept_from;
    let segments = list_wal(dir)?;
    let keep_segments = wal_after(&segments, kept_from);
    let first_kept = keep_segments.first().map(|(first, _)| *first);
    let mut doomed = Vec::new();
    for (seq, path) in snapshots.iter().filter(|(seq, _)| *seq < kept_from) {
        report.snapshots_removed.push(*seq);
        doomed.push(relative(dir, path));
    }
    for (first, path) in &segments {
        if first_kept.is_some_and(|kept| *first < kept) {
            report.segments_removed.push(*first);
            doomed.push(relative(dir, path));
        }
    }
    // The marks between the oldest snapshot and the oldest kept one lose their history. Only
    // the `marks` file is read, and only when a snapshot goes (the advice stays free of reads
    // otherwise); a missing file is rebuilt at the next open, without them.
    if let Some((oldest, _)) = snapshots.first()
        && !report.snapshots_removed.is_empty()
        && let Some(marks) = read_marks(dir)?
    {
        report.marks_unreachable = marks
            .iter()
            .filter(|mark| (*oldest..kept_from).contains(&mark.commit_seq()))
            .map(|mark| mark.name().to_string())
            .collect();
    }
    Ok(PrunePlan {
        report,
        doomed,
        snapshots,
    })
}

/// The bytes a prune up to `up_to` would remove: exactly the names [`prune`] removes (the doomed
/// snapshot directories and WAL segments, the holders older than the kept position, without the
/// files kept snapshots or attic entries still reference), sized by their lengths. The only
/// data read, and only when there is something to remove, are the manifests of the kept and the
/// attic snapshots (their file references) and the `marks` file when a snapshot goes; the attic bases come from the entries' names, see
/// [`super::rollback::attic_bases_by_name`]. When a manifest does not read, nothing is spared
/// (an upper bound: [`prune`] itself refuses then).
pub(super) fn prune_estimate(
    dir: &DirPath,
    up_to: u64,
) -> Result<(PruneReport, u64), PersistError> {
    let bases = super::rollback::attic_bases_by_name(dir)?;
    let PrunePlan {
        mut report,
        doomed,
        snapshots,
    } = plan_prune(dir, up_to, bases)?;
    // Nothing to remove: no manifest is read (the advice stays free of reads then).
    let old_holders = super::files::list_holders(dir)?
        .iter()
        .any(|(seq, _)| *seq < report.kept_from);
    if doomed.is_empty() && !old_holders {
        return Ok((report, 0));
    }
    let users = snapshots
        .iter()
        .filter(|(seq, _)| *seq >= report.kept_from)
        .map(|(_, path)| path.clone())
        .chain(super::rollback::attic_snapshot_dirs(dir).unwrap_or_default());
    let retained = referenced_files(dir, users).unwrap_or_default();
    let (removed, files_retained) = spare_retained(dir, doomed, &retained, report.kept_from)?;
    report.files_retained = files_retained;
    let bytes = removed
        .iter()
        .map(|path| super::rollback::tree_bytes(&dir.join(path)))
        .sum();
    Ok((report, bytes))
}

/// The files of other snapshot directories that `snapshots` reference (incremental snapshots,
/// format spec 6.2), relative to the store directory `dir`.
pub(super) fn referenced_files(
    dir: &DirPath,
    snapshots: impl IntoIterator<Item = DirPath>,
) -> Result<BTreeSet<String>, PersistError> {
    let mut out = BTreeSet::new();
    for snapshot in snapshots {
        let (_, manifest) = read_manifest(&snapshot)?;
        for entry in manifest.segments.iter().filter(|s| s.path.contains('/')) {
            if let Some(path) = segment_path(&snapshot, &entry.path) {
                out.insert(relative(dir, &path));
            }
        }
    }
    Ok(out)
}

/// Removes the files of holders (format spec 4.1) that no snapshot of the store or of an attic
/// entry references any more, and a holder left empty (format spec 12.4, 12.5): after a
/// rollback that deleted the history using them, or the removal of the attic entry whose
/// snapshots used them. A file nobody references is never referenced again (a checkpoint
/// references only files of a listed snapshot), so this needs no `INTENT`. When a manifest does
/// not read, nothing is removed. Returns the names removed.
pub(super) fn remove_unused_holders(dir: &DirPath) -> Result<Vec<String>, PersistError> {
    let holders = super::files::list_holders(dir)?;
    if holders.is_empty() {
        return Ok(Vec::new());
    }
    let users = list_snapshots(dir)?
        .into_iter()
        .map(|(_, path)| path)
        .chain(super::rollback::attic_snapshot_dirs(dir)?);
    let Ok(retained) = referenced_files(dir, users) else {
        return Ok(Vec::new());
    };
    let mut removed = Vec::new();
    for (_, holder) in holders {
        let entries = holder
            .list()
            .map_err(|err| PersistError::io("listing a snapshot directory", err))?;
        let mut left = 0;
        for (file, _) in entries {
            let path = holder.join(&file);
            let name = relative(dir, &path);
            if retained.contains(&name) {
                left += 1;
                continue;
            }
            super::files::remove_path(&path)
                .map_err(|err| PersistError::io("removing an unused file", err))?;
            removed.push(name);
        }
        if left == 0 {
            super::files::remove_path(&holder)
                .map_err(|err| PersistError::io("removing an unused file", err))?;
            removed.push(relative(dir, &holder));
        }
    }
    if !removed.is_empty() {
        sync_dir(&dir.join(SNAPSHOTS_DIR))?;
    }
    Ok(removed)
}

/// The element segment files the newest of `snapshots` shares with the previous one (format
/// spec 12.3, the donor rule), relative to the store directory, with the newest snapshot's
/// commit. Empty with fewer than two snapshots.
pub(super) fn files_without_donor(
    dir: &DirPath,
    snapshots: &[(u64, DirPath)],
) -> Result<Vec<(String, u64)>, PersistError> {
    let [.., (_, previous), (seq, newest)] = snapshots else {
        return Ok(Vec::new());
    };
    let files = |snapshot: &DirPath| -> Result<Vec<String>, PersistError> {
        let (_, manifest) = read_manifest(snapshot)?;
        Ok(manifest
            .segments
            .iter()
            .filter(|s| super::super::manifest::is_element_kind(s.kind))
            .filter_map(|s| segment_path(snapshot, &s.path))
            .map(|path| relative(dir, &path))
            .collect())
    };
    let used: BTreeSet<String> = files(previous)?.into_iter().collect();
    Ok(files(newest)?
        .into_iter()
        .filter(|file| used.contains(file))
        .map(|file| (file, *seq))
        .collect())
}

/// Splits the snapshot directories among `doomed` (and the holders older than `kept_from`,
/// format spec 4.1) by `retained`: a directory none of whose files is retained goes whole; of
/// one that holds a retained file only the other files go, its manifest first (it stops being a
/// snapshot at once). Returns the names to remove (snapshot parts first, then the rest of
/// `doomed`) and the files kept.
fn spare_retained(
    dir: &DirPath,
    doomed: Vec<String>,
    retained: &BTreeSet<String>,
    kept_from: u64,
) -> Result<(Vec<String>, Vec<String>), PersistError> {
    let snapshots_prefix = format!("{SNAPSHOTS_DIR}/");
    let (mut directories, rest): (Vec<String>, Vec<String>) = doomed
        .into_iter()
        .partition(|name| name.starts_with(&snapshots_prefix));
    for (seq, path) in super::files::list_holders(dir)? {
        if seq < kept_from {
            directories.push(relative(dir, &path));
        }
    }
    let mut remove = Vec::new();
    let mut kept = Vec::new();
    for name in directories {
        let prefix = format!("{name}/");
        let holds = retained
            .range(prefix.clone()..)
            .next()
            .is_some_and(|file| file.starts_with(&prefix));
        if !holds {
            remove.push(name);
            continue;
        }
        let mut files: Vec<String> = dir
            .join(&name)
            .list()
            .map_err(|err| PersistError::io("listing a snapshot directory", err))?
            .into_iter()
            .map(|(file, _)| file)
            .collect();
        files.sort_by_key(|file| (file != super::snapshot_dir::MANIFEST_FILE, file.clone()));
        for file in files {
            let full = format!("{prefix}{file}");
            if retained.contains(&full) {
                kept.push(full);
            } else {
                remove.push(full);
            }
        }
    }
    remove.extend(rest);
    Ok((remove, kept))
}

pub(super) fn prune(dir: &DirPath, up_to: u64) -> Result<PruneReport, PersistError> {
    let _pin = BackupPin::exclusive(dir)?;
    let PrunePlan {
        mut report,
        doomed,
        snapshots,
    } = plan_prune(dir, up_to, super::rollback::attic_bases(dir))?;
    // The kept snapshots are verified before anything older goes.
    for (seq, path) in snapshots.iter().filter(|(seq, _)| *seq >= report.kept_from) {
        verify_snapshot_dir(path).map_err(|err| {
            PersistError::store(
                "prune the store", StoreFailure::KeptSnapshotDamaged,
                format!("the snapshot at commit {seq} that would be kept fails verification ({err}); nothing was removed"),
            )
        })?;
    }
    // Files the kept snapshots and the attic entries' snapshots reference stay.
    let users = snapshots
        .iter()
        .filter(|(seq, _)| *seq >= report.kept_from)
        .map(|(_, path)| path.clone())
        .chain(super::rollback::attic_snapshot_dirs(dir)?);
    let retained = referenced_files(dir, users)?;
    let (doomed, files_retained) = spare_retained(dir, doomed, &retained, report.kept_from)?;
    report.files_retained = files_retained;
    if doomed.is_empty() {
        return Ok(report);
    }
    let intent = Intent {
        op: "prune".into(),
        files: doomed.clone(),
        started_at: crate::graph::undo::now_micros(),
        params: Vec::new(),
    };
    intent.write(dir)?;
    for path in &doomed {
        super::files::remove_path(&dir.join(path))
            .map_err(|err| PersistError::io("removing a pruned file", err))?;
    }
    sync_dir(&dir.join(SNAPSHOTS_DIR))?;
    sync_dir(&dir.join(WAL_DIR))?;
    Intent::clear(dir)?;
    Ok(report)
}

/// The last commit (and its time) in the files: the last complete commit of the WAL, else the
/// latest snapshot's position.
pub(super) fn last_commit_in_files(
    dir: &DirPath,
    snapshots: &[SnapshotInfo],
) -> Result<(u64, i64), PersistError> {
    let mut last = snapshots
        .last()
        .map_or((0, 0), |s| (s.commit_seq, s.last_commit_at));
    let segments = list_wal(dir)?;
    // The last segment holding a commit (a fresh segment may hold none yet).
    for (_, path) in segments.iter().rev() {
        let source = open_segment_reader(path, None)?;
        let Ok(mut reader) =
            JournalReader::open(source, 0, crate::value::depth::DEFAULT_MAX_VALUE_DEPTH)
        else {
            continue;
        };
        let mut found: Option<(u64, i64)> = None;
        // A commit of a newer version (format spec 16) counts; its time is not read.
        let mut skipped = None;
        while let Ok(Some((_, _, record))) =
            reader.next_known_record(&mut |seq| skipped = Some(seq))
        {
            if let Some(seq) = skipped.take() {
                found = Some((seq, found.map_or(last.1, |f| f.1)));
            }
            if let JournalRecord::Commit(changes) = record {
                found = Some((changes.commit_seq(), changes.committed_at()));
            }
        }
        if let Some(seq) = skipped {
            found = Some((seq, found.map_or(last.1, |f| f.1)));
        }
        if let Some(found) = found {
            if found.0 > last.0 {
                last = found;
            }
            break;
        }
    }
    Ok(last)
}

/// How many definitions the catalog holds by the files alone (no lock, no load): the latest
/// snapshot's definitions table (names only, no payload decoded), then the catalog changes of
/// every later commit in the WAL. A WAL segment that cannot be read is skipped, like
/// [`last_commit_in_files`] does.
/// The definitions per kind and the model identities the schema names (format spec 4.5), read
/// from the files: the latest snapshot's definitions table and `schema.json` with the WAL's
/// catalog and schema changes after it applied (one pass over the WAL). A schema that does
/// not parse names no model.
pub(super) fn definitions_in_files(
    dir: &DirPath,
) -> Result<(std::collections::BTreeMap<String, u64>, Vec<String>), PersistError> {
    let mut names = std::collections::BTreeSet::new();
    let mut base = 0;
    let mut schema: Option<crate::graph::schema::GraphSchema> = None;
    if let Some((commit_seq, path)) = list_snapshots(dir)?.last() {
        schema = read_bounded(&path.join(super::snapshot_dir::SCHEMA_FILE), 256 << 20)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .and_then(|text| crate::graph::schema::GraphSchema::from_json(&text).ok());
        let (_, manifest) = super::snapshot_dir::read_manifest(path)?;
        names.extend(
            manifest
                .definitions
                .iter()
                .map(|record| (record.kind, record.name.clone())),
        );
        base = *commit_seq;
    }
    for (_, path) in list_wal(dir)? {
        let source = open_segment_reader(&path, None)?;
        let Ok(mut reader) =
            JournalReader::open(source, 0, crate::value::depth::DEFAULT_MAX_VALUE_DEPTH)
        else {
            continue;
        };
        while let Ok(Some((_, _, record))) = reader.next_known_record(&mut |_| {}) {
            let JournalRecord::Commit(changes) = record else {
                continue;
            };
            if changes.commit_seq() <= base {
                continue;
            }
            for mutation in changes.mutations() {
                match mutation {
                    crate::graph::Mutation::SetDefinition {
                        kind, name, after, ..
                    } => {
                        let key = (kind.code(), name.clone());
                        if after.is_some() {
                            names.insert(key);
                        } else {
                            names.remove(&key);
                        }
                    }
                    crate::graph::Mutation::SetSchema { after, .. } => {
                        schema = Some((**after).clone());
                    }
                    _ => {}
                }
            }
        }
    }
    let models = schema
        .as_ref()
        .map(|schema| {
            crate::value::vector::embed::provenance::schema_models(schema)
                .into_iter()
                .map(|(model, _)| model)
                .collect()
        })
        .unwrap_or_default();
    Ok((
        super::count_by_kind(
            names
                .iter()
                .map(|(kind, _)| crate::graph::DefinitionKind::from_code(*kind)),
        ),
        models,
    ))
}

pub(super) fn verify(
    dir: &DirPath,
    durable: Option<DurablePosition>,
) -> Result<VerifyReport, PersistError> {
    // A newer (or older) format is refused, never listed as damage (format spec 15).
    super::files::refuse_other_version(dir)?;
    let mut report = VerifyReport::default();
    fn problem(report: &mut VerifyReport, path: &str, offset: Option<u64>, reason: String) {
        report.problems.push(VerifyProblem {
            path: path.to_string(),
            offset,
            reason,
        });
    }

    // The backend's own records (a single-file container).
    let backend = crate::persist::dir::backend_check(&**dir.store_dir());
    for reason in backend.damage {
        problem(&mut report, ".", None, format!("container: {reason}"));
    }
    report.notes.extend(backend.warnings);
    // GRAPH and its copy: both must be valid and equal.
    let graph_file = read_graph_file(dir)?;
    if let Some(repaired) = &graph_file.repaired {
        problem(&mut report, GRAPH_FILE, None, repaired.clone());
    }
    let chain = graph_file.file;
    // The set of WAL segments against GRAPH (format spec 9.8).
    {
        let mut segments = list_wal(dir)?;
        super::drop_refused_segments(&mut segments, chain.wal_cut);
        for (name, reason) in super::wal_files::wal_set_problems(&segments, &chain) {
            problem(&mut report, &name, None, reason);
        }
    }
    if let Ok(Some(intent)) = Intent::read(dir) {
        report.notes.push(format!(
            "an interrupted operation ({}) is pending: it is completed (or, an incremental \
             backup, rolled back) at the next open, backup, restore or prune",
            intent.op
        ));
    }
    for name in [GRAPH_FILE, GRAPH_COPY] {
        if let Err(err) = read_bounded(&dir.join(name), 64 << 20) {
            problem(&mut report, name, None, err.to_string());
        }
    }

    // Snapshots: every byte, fully decoded (ids in order, ranges, counts).
    let snapshots = list_snapshots(dir)?;
    let read = ReadOptions::new().with_max_value_depth(4096);
    for (seq, path) in &snapshots {
        let rel = relative(dir, path);
        report.snapshots_checked += 1;
        // Both copies of the manifest's fixed part must be intact (the load uses one).
        if let Ok(bytes) = read_bounded(&path.join(super::snapshot_dir::MANIFEST_FILE), 256 << 20)
            && bytes.len() >= 2 * FIXED_BYTES
        {
            let head = ManifestFixed::decode(&bytes[..FIXED_BYTES]);
            let tail = ManifestFixed::decode(&bytes[bytes.len() - FIXED_BYTES..]);
            match (head, tail) {
                (Ok(head), Ok(tail)) if head == tail => {}
                (Ok(_), Ok(_)) => problem(
                    &mut report,
                    &format!("{rel}/manifest"),
                    None,
                    "the two copies of the fixed part differ".into(),
                ),
                (Err(err), _) => problem(
                    &mut report,
                    &format!("{rel}/manifest"),
                    Some(err.offset),
                    format!("head copy: {}", err.reason),
                ),
                (_, Err(err)) => problem(
                    &mut report,
                    &format!("{rel}/manifest"),
                    Some((bytes.len() - FIXED_BYTES) as u64 + err.offset),
                    format!("tail copy: {}", err.reason),
                ),
            }
        }
        let loaded = DirSnapshotReader::open(path).and_then(|reader| read_snapshot(reader, &read));
        match loaded {
            // Content of a newer version behind valid checksums (format spec 16): not damage.
            Err(err @ PersistError::UnsupportedContent { .. }) => {
                report.notes.push(format!("{rel}: {err}"));
            }
            Ok(graph) => {
                if graph.last_commit_seq() != *seq {
                    problem(
                        &mut report,
                        &rel,
                        None,
                        format!("its manifest says commit {}", graph.last_commit_seq()),
                    );
                }
                if !chain.on_chain(graph.graph_id()) {
                    problem(
                        &mut report,
                        &rel,
                        None,
                        "a lineage not on the store's chain".into(),
                    );
                }
            }
            Err(err) => problem(&mut report, &rel, None, err.to_string()),
        }
    }

    // The embedding models (format spec 4.5): every file against its manifest's SHA-256.
    for (path, reason) in super::models::stray_entries(dir) {
        problem(&mut report, &path, None, reason);
    }
    for id in super::models::model_ids(dir)? {
        report.models_checked += 1;
        let (model, problems) = super::models::check(dir, &id);
        for (path, reason) in problems {
            problem(&mut report, &path, None, reason);
        }
        if let Some(note) = model.and_then(|m| m.problem) {
            report
                .notes
                .push(format!("{}/{id}: {note}", super::models::MODELS_DIR));
        }
    }

    // The donor rule (format spec 12.3, 13.1): the newest snapshot shares no segment file with
    // the previous one, which is the donor of every id range.
    if let Ok(shared) = files_without_donor(dir, &snapshots) {
        for (file, seq) in shared {
            report.notes.push(format!(
                "warning: {file}: the newest snapshot (commit {seq}) shares this segment file \
                 with the previous snapshot, so its id range has no independent donor \
                 (redundancy, not data loss; the next checkpoint writes a copy)"
            ));
            report.without_donor.push(file);
        }
    }

    // Snapshot directories without a manifest hold files newer snapshots use (format spec 4.1):
    // one no snapshot uses lost its manifest, or a prune left it.
    let users = snapshots
        .iter()
        .map(|(_, path)| path.clone())
        .chain(super::rollback::attic_snapshot_dirs(dir).unwrap_or_default());
    if let Ok(retained) = referenced_files(dir, users) {
        for (_, path) in super::files::list_holders(dir)? {
            let rel = relative(dir, &path);
            let prefix = format!("{rel}/");
            let used = retained
                .range(prefix.clone()..)
                .next()
                .is_some_and(|file| file.starts_with(&prefix));
            let empty = path.list().map(|e| e.is_empty()).unwrap_or(false);
            if !used && !empty {
                problem(
                    &mut report,
                    &rel,
                    None,
                    "a snapshot directory without a manifest whose files no snapshot uses (its \
                     manifest is lost, or an interrupted prune left it)"
                        .into(),
                );
            }
        }
    }

    // WAL: every record, continuity of commits across segments, lineage. A recorded cut of a
    // refused record (format spec 9.7) ends the valid WAL; the next open removes the rest.
    let mut segments = list_wal(dir)?;
    if let Some((segment, offset)) = chain.wal_cut {
        report.notes.push(format!(
            "a refused WAL record from segment {segment}, byte {offset} on is ignored and cut at \
             the next open"
        ));
        super::drop_refused_segments(&mut segments, chain.wal_cut);
    }
    let mut previous: Option<u64> = None;
    let mut wal_marks = Vec::new();
    for (index, (first, path)) in segments.iter().enumerate() {
        let rel = relative(dir, path);
        report.segments_checked += 1;
        let last_segment = index + 1 == segments.len();
        let limit = durable
            .filter(|d| d.segment == *first)
            .map(|d| d.offset)
            .or(super::cut_limit(chain.wal_cut, *first));
        let source = open_segment_reader(path, limit)?;
        let mut reader = match JournalReader::open(source, index, 4096) {
            Ok(reader) => reader,
            Err(err) => {
                problem(&mut report, &rel, Some(0), err.to_string());
                continue;
            }
        };
        if reader.first_commit_seq() != *first {
            problem(
                &mut report,
                &rel,
                Some(28),
                format!("its header says first commit {}", reader.first_commit_seq()),
            );
        }
        if !chain.on_chain(reader.graph_id()) {
            problem(
                &mut report,
                &rel,
                Some(12),
                "a lineage not on the store's chain".into(),
            );
        }
        loop {
            let (offset, seq) = match reader.next_record() {
                Ok(None) => break,
                Ok(Some((offset, _, record))) => {
                    report.records_checked += 1;
                    match record {
                        JournalRecord::Commit(changes) => (offset, changes.commit_seq()),
                        JournalRecord::Mark(mark) => {
                            wal_marks.push(mark);
                            continue;
                        }
                        JournalRecord::Checkpoint { .. } => continue,
                    }
                }
                // Content of a newer version behind valid checksums (format spec 16): not
                // damage; the records after it are checked on.
                Err(err @ PersistError::UnsupportedContent { .. }) => {
                    report.records_checked += 1;
                    report.notes.push(format!("{rel}: {err}"));
                    match reader.unsupported_seq() {
                        Some(record) => record,
                        None => continue,
                    }
                }
                Err(err) => {
                    problem(&mut report, &rel, None, err.to_string());
                    break;
                }
            };
            if seq < *first {
                problem(
                    &mut report,
                    &rel,
                    Some(offset),
                    format!("commit {seq} before the segment's first commit {first}"),
                );
            }
            if let Some(prev) = previous
                && seq != prev + 1
            {
                problem(
                    &mut report,
                    &rel,
                    Some(offset),
                    format!("commit {seq} follows commit {prev} (a gap)"),
                );
            }
            if report.first_commit == 0 {
                report.first_commit = seq;
            }
            report.last_commit = seq;
            previous = Some(seq);
        }
        if let Some(torn) = reader.torn_tail() {
            if last_segment {
                report.notes.push(format!(
                    "{rel}: an incomplete last record at byte {} ({} bytes): an interrupted or \
                     running write, cut at the next open",
                    torn.offset, torn.bytes
                ));
            } else {
                problem(
                    &mut report,
                    &rel,
                    Some(torn.offset),
                    "an incomplete record in a segment that is not the last one".into(),
                );
            }
        }
    }

    // The WAL must continue the oldest snapshot (every target after it is reachable).
    if let Some((oldest, path)) = snapshots.first()
        && report.last_commit > *oldest
        && report.first_commit > oldest + 1
    {
        let reason = format!(
            "the WAL starts at commit {}, after this snapshot's next commit {}",
            report.first_commit,
            oldest + 1
        );
        problem(&mut report, &relative(dir, path), None, reason);
    }

    // A clean close recorded its last commit: the files must still reach it (format spec 9.8).
    let newest = snapshots.last().map_or(0, |(seq, _)| *seq);
    if chain.clean
        && let Some(closed_at) = chain.closed_at
        && report.last_commit.max(newest) < closed_at
    {
        let reason = format!(
            "the store was closed cleanly at commit {closed_at}, but its files end at commit {}",
            report.last_commit.max(newest)
        );
        problem(&mut report, "wal", None, reason);
    }

    // The attic (format spec 11.2): every entry's history rebuilds (its base snapshot or its
    // own newest one, and the WAL after it), and every snapshot of an entry verifies (a
    // restore brings it back into the store).
    match super::rollback::attic_list(dir) {
        Ok(entries) => {
            for entry in entries {
                let at = format!("attic/{}", entry.id);
                if let Err(err) = super::rollback::attic_state(dir, &entry.id, &read) {
                    problem(
                        &mut report,
                        &at,
                        None,
                        format!("its history cannot be rebuilt: {err}"),
                    );
                }
                let path = dir.join(&at);
                for (seq, snapshot) in list_snapshots(&path).unwrap_or_default() {
                    if let Err(err) = verify_snapshot_dir(&snapshot) {
                        problem(
                            &mut report,
                            &format!("{at}/snapshots/{seq:020}"),
                            None,
                            err.to_string(),
                        );
                    }
                }
            }
        }
        Err(err) => problem(&mut report, "attic", None, err.to_string()),
    }

    // The marks file: derived, so damage is a note (it is rebuilt), a mark of the WAL that is
    // missing in it too.
    match read_bounded(&dir.join(MARKS_FILE), 256 << 20) {
        Ok(bytes) => match decode_marks(&bytes) {
            Some(marks) => {
                for mark in &wal_marks {
                    if !marks.iter().any(|m| m.name() == mark.name()) {
                        report.notes.push(format!(
                            "{MARKS_FILE}: the mark {:?} of the WAL is missing; added at the \
                             next open",
                            mark.name()
                        ));
                    }
                }
            }
            None => report.notes.push(format!(
                "{MARKS_FILE}: damaged; rebuilt from the WAL at the next open"
            )),
        },
        Err(_) if !wal_marks.is_empty() => report.notes.push(format!(
            "{MARKS_FILE}: missing; rebuilt from the WAL at the next open"
        )),
        Err(_) => {}
    }
    Ok(report)
}

/// One file of a backup: where it goes (relative), where it comes from, how many bytes.
struct BackupFile {
    relative: String,
    source: DirPath,
    len: u64,
    /// The bytes to write instead of the source's (a manifest with a damaged copy of its
    /// fixed part healed from the intact one).
    bytes: Option<Vec<u8>>,
}

/// What a full backup copies: `GRAPH` (marked clean and as a backup), the `marks` file read
/// BEFORE the durable end (every mark in it is in the WAL copied), the latest snapshot's files,
/// the WAL after it up to the durable end; and what the checks of the source found, which the
/// copy must read back the same.
pub(super) struct BackupPlan {
    graph: Vec<u8>,
    marks: Vec<u8>,
    files: Vec<BackupFile>,
    snapshot_commit_seq: u64,
    commit_seq: u64,
    /// Every WAL segment copied with what its check found (the copy must equal it).
    segments: Vec<(u64, SegmentCheck)>,
    taken_at: i64,
    warnings: Vec<String>,
    /// The models copied (format spec 4.5) with their manifests' bytes; their files are in
    /// `files`.
    models: Vec<(super::StoredModel, Vec<u8>)>,
}

/// Plans a full backup of the store in `dir` and checks every checksum of what it would copy
/// (format spec 12.6): `GRAPH`, `marks`, the latest snapshot (and the files it references),
/// every WAL record of the replay set up to the durable end (header and body CRC, commit
/// continuity, the segment set of 9.8). Damage is returned as the findings' problems.
fn plan_backup(
    dir: &DirPath,
    durable: Option<DurablePosition>,
    with_models: bool,
    findings: &mut Findings,
) -> Result<Option<BackupPlan>, PersistError> {
    let marks = check_marks(dir, findings).unwrap_or_default();
    let Some(mut graph_file) = check_graph_files(dir, findings)? else {
        return Ok(None);
    };
    graph_file.clean = true;
    let cut = graph_file.wal_cut.take();
    let snapshots = list_snapshots(dir)?;
    let Some((snapshot_seq, snapshot)) = snapshots.last().cloned() else {
        return Err(PersistError::store(
            "back up the store",
            StoreFailure::NoSnapshot,
            "it has no snapshot",
        ));
    };
    let healed = check_snapshot(dir, &snapshot, true, findings);
    if !findings.is_ok() {
        return Ok(None);
    }
    let (_, manifest) = read_manifest(&snapshot)?;
    let mut files = Vec::new();
    let snapshot_rel = format!(
        "{SNAPSHOTS_DIR}/{}",
        super::files::snapshot_name(snapshot_seq)
    );
    fn add(
        files: &mut Vec<BackupFile>,
        relative: String,
        source: DirPath,
    ) -> Result<(), PersistError> {
        let len = source
            .len()
            .map_err(|err| PersistError::io("reading a file to back up", err))?;
        files.push(BackupFile {
            relative,
            source,
            len,
            bytes: None,
        });
        Ok(())
    }
    add(
        &mut files,
        format!("{snapshot_rel}/{}", super::snapshot_dir::MANIFEST_FILE),
        snapshot.join(super::snapshot_dir::MANIFEST_FILE),
    )?;
    if let (Some(healed), Some(manifest)) = (healed, files.last_mut()) {
        manifest.len = healed.len() as u64;
        manifest.bytes = Some(healed);
    }
    add(
        &mut files,
        format!("{snapshot_rel}/{}", super::snapshot_dir::SCHEMA_FILE),
        snapshot.join(super::snapshot_dir::SCHEMA_FILE),
    )?;
    for entry in &manifest.segments {
        let source = segment_path(&snapshot, &entry.path).ok_or_else(|| {
            PersistError::corrupt(
                "manifest",
                0,
                format!("unsafe segment path {:?}", entry.path),
            )
        })?;
        let relative = if entry.path.contains('/') {
            format!("{SNAPSHOTS_DIR}/{}", entry.path)
        } else {
            format!("{snapshot_rel}/{}", entry.path)
        };
        add(&mut files, relative, source)?;
    }
    // The models (format spec 4.5): every file checked against its manifest, then copied as
    // it is, the manifest as read.
    let mut models = Vec::new();
    if with_models {
        for id in super::models::model_ids(dir)? {
            let (model, problems) = super::models::check(dir, &id);
            for (path, reason) in problems {
                findings.problem(path, None, reason);
            }
            let Some(model) = model else { continue };
            let Ok((manifest, _)) = super::models::read_model_manifest(dir, &id) else {
                continue;
            };
            let base = super::models::models_dir(dir).join(&id);
            let rel = format!("{}/{id}", super::models::MODELS_DIR);
            for file in &model.files {
                files.push(BackupFile {
                    relative: format!("{rel}/{}", file.name),
                    source: base.join(&file.name),
                    len: file.bytes,
                    bytes: None,
                });
            }
            files.push(BackupFile {
                relative: format!("{rel}/{}", super::models::MODEL_MANIFEST),
                source: base.join(super::models::MODEL_MANIFEST),
                len: manifest.len() as u64,
                bytes: Some(manifest.clone()),
            });
            models.push((model, manifest));
        }
    }
    let mut commit_seq = snapshot_seq;
    let mut segments = list_wal(dir)?;
    super::drop_refused_segments(&mut segments, cut);
    // The segment set against GRAPH (format spec 9.8): a missing, emptied or replaced segment
    // would make the backup silently shorter.
    let mut set_file = graph_file.clone();
    set_file.wal_cut = cut;
    for (name, reason) in super::wal_files::wal_set_problems(&segments, &set_file) {
        findings.problem(name, None, reason);
    }
    let replay = wal_after(&segments, snapshot_seq);
    let mut checked: Vec<(u64, SegmentCheck)> = Vec::new();
    for (index, (first, path)) in replay.iter().enumerate() {
        let last = index + 1 == replay.len();
        let cut_at = super::cut_limit(cut, *first);
        let (limit, torn_ok) = match durable {
            // An open store: up to its durable end, which is a record boundary.
            Some(position) if *first > position.segment => break,
            Some(position) if *first == position.segment => (
                Some(cut_at.map_or(position.offset, |c| c.min(position.offset))),
                false,
            ),
            Some(_) => (cut_at, false),
            // Another process: up to the last complete record of the last segment.
            None => (cut_at, last),
        };
        // A last segment shorter than its header that GRAPH does not record as started: a
        // crash while it was being started (format spec 9.8); it holds nothing.
        if last
            && durable.is_none()
            && graph_file.wal_head.is_none_or(|head| *first > head)
            && path.len().is_ok_and(|len| len < WAL_HEADER_BYTES as u64)
        {
            break;
        }
        if let Some(check) = check_segment(
            dir,
            *first,
            path,
            limit,
            torn_ok,
            Some(&graph_file),
            findings,
        ) {
            if let Some((_, seq)) = check.commits.last() {
                commit_seq = commit_seq.max(*seq);
            }
            files.push(BackupFile {
                relative: format!("{WAL_DIR}/{}", super::files::wal_name(*first)),
                source: path.clone(),
                len: check.end,
                bytes: None,
            });
            checked.push((*first, check));
        }
    }
    let refs: Vec<(u64, &SegmentCheck)> = checked.iter().map(|(f, c)| (*f, c)).collect();
    check_continuity(&refs, Some(snapshot_seq), findings);
    if let Some(position) = durable
        && findings.is_ok()
        && commit_seq < position.commit_seq
    {
        findings.problem(
            WAL_DIR,
            None,
            format!(
                "the WAL ends at commit {commit_seq}, before the durable commit {}: records \
                 are lost",
                position.commit_seq
            ),
        );
    }
    if !findings.is_ok() {
        return Ok(None);
    }
    let taken_at = crate::graph::undo::now_micros();
    graph_file.backup = Some(super::files::BackupMarker {
        commit_seq,
        taken_at,
    });
    // The backup's own newest segment and position (format spec 9.8).
    graph_file.wal_head = checked.last().map(|(first, _)| *first);
    graph_file.closed_at = Some(commit_seq);
    Ok(Some(BackupPlan {
        graph: graph_file.encode(),
        marks,
        files,
        snapshot_commit_seq: snapshot_seq,
        commit_seq,
        segments: checked,
        taken_at,
        warnings: std::mem::take(&mut findings.warnings),
        models,
    }))
}

/// Plans a full backup; damage of the source stops it with the report.
fn checked_plan(
    dir: &DirPath,
    durable: Option<DurablePosition>,
    backup: &str,
    with_models: bool,
) -> Result<BackupPlan, PersistError> {
    let mut findings = Findings::default();
    match plan_backup(dir, durable, with_models, &mut findings)? {
        Some(plan) if findings.is_ok() => Ok(plan),
        _ => Err(findings.into_error(DamageSide::Store, dir, backup.to_string(), false)),
    }
}

/// Checks a full backup where it landed (`target`: a directory, or an archive read back):
/// both `GRAPH` copies and `marks` byte for byte as written, the snapshot's every checksum,
/// every WAL segment read back with the end and the commits the source's check found.
fn check_copy(target: &DirPath, plan: &BackupPlan) -> Findings {
    let mut findings = Findings::default();
    for name in [GRAPH_FILE, GRAPH_COPY] {
        match read_bounded(&target.join(name), 64 << 20) {
            Ok(bytes) if bytes == plan.graph => {}
            Ok(_) => findings.problem(name, None, "differs from what was written".into()),
            Err(err) => findings.problem(name, None, err.to_string()),
        }
    }
    let marks = target.join(MARKS_FILE);
    if !plan.marks.is_empty() || marks.exists() {
        match read_bounded(&marks, 256 << 20) {
            Ok(bytes) if !plan.marks.is_empty() && bytes != plan.marks => {
                findings.problem(MARKS_FILE, None, "differs from what was written".into())
            }
            Ok(bytes) if decode_marks(&bytes).is_none() => {
                findings.problem(MARKS_FILE, None, "a damaged frame".into())
            }
            Ok(_) => {}
            Err(err) => findings.problem(MARKS_FILE, None, err.to_string()),
        }
    }
    let snapshot = target
        .join(SNAPSHOTS_DIR)
        .join(super::files::snapshot_name(plan.snapshot_commit_seq));
    check_snapshot(target, &snapshot, false, &mut findings);
    for (model, _) in &plan.models {
        for (path, reason) in super::models::check(target, &model.id).1 {
            findings.problem(path, None, reason);
        }
    }
    let graph_file = GraphFile::decode(&plan.graph).ok();
    for (first, source) in &plan.segments {
        let path = target.join(WAL_DIR).join(super::files::wal_name(*first));
        if let Some(copy) = check_segment(
            target,
            *first,
            &path,
            None,
            false,
            graph_file.as_ref(),
            &mut findings,
        ) && (copy.end != source.end || copy.commits != source.commits)
        {
            findings.problem(
                path.relative_to(target),
                None,
                format!(
                    "the copy ends at byte {} with {} commit(s), the store's segment at byte {} \
                     with {}",
                    copy.end,
                    copy.commits.len(),
                    source.end,
                    source.commits.len()
                ),
            );
        }
    }
    findings
}

/// The error of a copy that did not read back intact: when the store's own files no longer
/// pass their checks either, the damage is the store's (a source that changed under the
/// copy), otherwise the backup's.
fn copy_damage(
    dir: &DirPath,
    durable: Option<DurablePosition>,
    backup: String,
    copy: Findings,
    with_models: bool,
) -> PersistError {
    let mut again = Findings::default();
    match plan_backup(dir, durable, with_models, &mut again) {
        Ok(Some(_)) if again.is_ok() => copy.into_error(DamageSide::Backup, dir, backup, false),
        Ok(_) => again.into_error(DamageSide::Store, dir, backup, false),
        Err(err) => err,
    }
}

/// The report of a full backup before its files are written.
fn full_report(plan: &BackupPlan) -> BackupReport {
    BackupReport {
        snapshot_commit_seq: plan.snapshot_commit_seq,
        commit_seq: plan.commit_seq,
        snapshots_copied: vec![plan.snapshot_commit_seq],
        segments_copied: plan.segments.iter().map(|(first, _)| *first).collect(),
        taken_at: plan.taken_at,
        warnings: plan.warnings.clone(),
        models: plan.models.iter().map(|(model, _)| model.clone()).collect(),
        ..BackupReport::default()
    }
}

/// Copies exactly `len` bytes of `source` to `out`.
pub(super) fn copy_exact(
    source: &DirPath,
    len: u64,
    out: &mut impl Write,
) -> Result<(), PersistError> {
    let file = source
        .open_read()
        .map_err(|err| PersistError::io("reading a file to back up", err))?;
    let copied = std::io::copy(&mut file.take(len), out)
        .map_err(|err| PersistError::io("copying a file to the backup", err))?;
    if copied != len {
        return Err(PersistError::store(
            "back up the store",
            StoreFailure::ChangedConcurrently,
            format!("{} shrank while it was copied", source.display()),
        ));
    }
    Ok(())
}

pub(super) fn backup_to_dir(
    dir: &DirPath,
    durable: Option<DurablePosition>,
    target: &DirPath,
) -> Result<BackupReport, PersistError> {
    if super::files::has_entries(target) {
        return Err(PersistError::NotAStore {
            path: target.display().to_string(),
            reason: "the backup directory is not empty".into(),
        });
    }
    let _pin = BackupPin::shared(dir)?;
    let plan = checked_plan(dir, durable, &target.display(), true)?;
    let existed = target.exists();
    let result = (|| {
        let mut report = full_report(&plan);
        target
            .join(SNAPSHOTS_DIR)
            .create_dir_all()
            .and_then(|()| target.join(WAL_DIR).create_dir_all())
            .map_err(|err| PersistError::io("creating the backup directory", err))?;
        for file in &plan.files {
            let path = target.join(&file.relative);
            if let Some(parent) = path.parent() {
                parent
                    .create_dir_all()
                    .map_err(|err| PersistError::io("creating the backup directory", err))?;
            }
            let mut out = path
                .create(false)
                .map_err(|err| PersistError::io("writing the backup", err))?;
            match &file.bytes {
                Some(bytes) => out
                    .write_all(bytes)
                    .map_err(|err| PersistError::io("writing the backup", err))?,
                None => copy_exact(&file.source, file.len, &mut out)?,
            }
            out.sync()
                .map_err(|err| PersistError::io("writing the backup", err))?;
            report.files.push((file.relative.clone(), file.len));
            report.bytes += file.len;
        }
        let marks = if plan.marks.is_empty() {
            // The store's marks file was damaged (or there is none): rebuilt from the WAL
            // copied (the source of truth, format spec 10).
            let marks = super::wal_files::scan_marks(
                &list_wal(target)?,
                crate::value::depth::DEFAULT_MAX_VALUE_DEPTH,
            );
            super::wal_files::marks_bytes(&marks)
        } else {
            plan.marks.clone()
        };
        for (name, bytes) in [
            (MARKS_FILE, &marks),
            (GRAPH_COPY, &plan.graph),
            (GRAPH_FILE, &plan.graph),
        ] {
            if name == MARKS_FILE && bytes.is_empty() {
                continue;
            }
            super::files::write_atomic(target, name, bytes)?;
            report.files.push((name.to_string(), bytes.len() as u64));
            report.bytes += bytes.len() as u64;
        }
        for sub in [
            target.join(format!(
                "{SNAPSHOTS_DIR}/{}",
                super::files::snapshot_name(plan.snapshot_commit_seq)
            )),
            target.join(SNAPSHOTS_DIR),
            target.join(WAL_DIR),
            target.clone(),
        ] {
            sync_dir(&sub)?;
        }
        // Every checksum again, where the copy landed.
        let copy = check_copy(target, &plan);
        if !copy.is_ok() {
            return Err(copy_damage(dir, durable, target.display(), copy, true));
        }
        Ok(report)
    })();
    if result.is_err() {
        super::files::undo_new_dir(target, existed);
    }
    result
}

/// A full backup as one ZIP archive into `archive` from its current position on; the archive
/// is read back and checked afterwards (every entry's CRC-32 and every checksum of the store
/// files in it).
#[cfg(feature = "persist-zip")]
pub(super) fn backup_to_zip<W>(
    dir: &DirPath,
    durable: Option<DurablePosition>,
    archive: &mut W,
    models: super::ModelCopy,
) -> Result<BackupReport, PersistError>
where
    W: Read + Write + std::io::Seek + Send,
{
    super::files::refuse_other_version(dir)?;
    let _pin = BackupPin::shared(dir)?;
    let with_models = models == super::ModelCopy::Include;
    let plan = checked_plan(dir, durable, "the ZIP archive", with_models)?;
    let start = archive
        .stream_position()
        .map_err(|err| PersistError::io("writing the backup archive", err))?;
    let mut report = full_report(&plan);
    {
        let mut zip = super::zip::ZipWriter::new(std::io::BufWriter::new(&mut *archive));
        let mut memory = vec![
            (GRAPH_FILE, plan.graph.clone()),
            (GRAPH_COPY, plan.graph.clone()),
        ];
        if !plan.marks.is_empty() {
            memory.push((MARKS_FILE, plan.marks.clone()));
        }
        for (name, bytes) in &memory {
            zip.add_bytes(name, bytes)?;
            report.files.push((name.to_string(), bytes.len() as u64));
            report.bytes += bytes.len() as u64;
        }
        for file in &plan.files {
            match &file.bytes {
                Some(bytes) => zip.add_bytes(&file.relative, bytes)?,
                None => zip.add_file(&file.relative, &file.source, file.len)?,
            }
            report.files.push((file.relative.clone(), file.len));
            report.bytes += file.len;
        }
        zip.finish()?;
    }
    // Every checksum again, in the archive as written.
    archive
        .seek(std::io::SeekFrom::Start(start))
        .map_err(|err| PersistError::io("reading the backup archive back", err))?;
    let copy = check_archive(&mut *archive, &plan);
    if !copy.is_ok() {
        return Err(copy_damage(
            dir,
            durable,
            "the ZIP archive".into(),
            copy,
            with_models,
        ));
    }
    Ok(report)
}

/// Checks a full backup written as a ZIP archive, read back from `reader` in one pass: every
/// entry's CRC-32, both `GRAPH` copies and `marks` byte for byte as written, every snapshot
/// file against its manifest (every chunk), every WAL segment with the end and the commits
/// the source's check found, and that nothing is missing.
#[cfg(feature = "persist-zip")]
fn check_archive(reader: impl Read, plan: &BackupPlan) -> Findings {
    use super::super::manifest::Manifest;
    let mut findings = Findings::default();
    let graph_file = GraphFile::decode(&plan.graph).ok();
    let snapshot_name = super::files::snapshot_name(plan.snapshot_commit_seq);
    let mut manifest: Option<Manifest> = None;
    let mut seen = BTreeSet::new();
    let mut segments = Vec::new();
    let problems = super::zip::check_entries(reader, &mut |name, data| {
        seen.insert(name.to_string());
        let read_all = |data: &mut dyn Read| {
            let mut bytes = Vec::new();
            data.read_to_end(&mut bytes).map(|_| bytes)
        };
        let expect = |bytes: std::io::Result<Vec<u8>>, written: &[u8]| match bytes {
            Ok(bytes) if bytes == written => Vec::new(),
            Ok(_) => vec!["differs from what was written".to_string()],
            Err(err) => vec![err.to_string()],
        };
        if name == GRAPH_FILE || name == GRAPH_COPY {
            return expect(read_all(data), &plan.graph);
        }
        if name == MARKS_FILE {
            return expect(read_all(data), &plan.marks);
        }
        if let Some(rest) = name.strip_prefix(&format!("{}/", super::models::MODELS_DIR)) {
            let Some((id, file)) = rest.split_once('/') else {
                return vec!["not a file of a store backup".into()];
            };
            let Some((model, manifest)) = plan.models.iter().find(|(m, _)| m.id == id) else {
                return vec!["a model the backup did not copy".into()];
            };
            if file == super::models::MODEL_MANIFEST {
                return expect(read_all(data), manifest);
            }
            return super::models::check_stream(model, file, data);
        }
        if let Some(file) = name.strip_prefix(&format!("{WAL_DIR}/")) {
            let first = file
                .strip_suffix(".wal")
                .and_then(super::files::parse_seq)
                .unwrap_or(0);
            let mut own = Findings::default();
            let check =
                check_segment_stream(name, first, data, false, graph_file.as_ref(), &mut own);
            let mut out: Vec<String> = own.problems.into_iter().map(|p| p.reason).collect();
            match (check, plan.segments.iter().find(|(f, _)| *f == first)) {
                (Some(copy), Some((_, source)))
                    if copy.end != source.end || copy.commits != source.commits =>
                {
                    out.push(format!(
                        "the copy ends at byte {} with {} commit(s), the store's segment at \
                         byte {} with {}",
                        copy.end,
                        copy.commits.len(),
                        source.end,
                        source.commits.len()
                    ));
                }
                (Some(_), None) => out.push("a WAL segment the backup did not copy".into()),
                _ => {}
            }
            segments.push(first);
            return out;
        }
        let Some(rest) = name.strip_prefix(&format!("{SNAPSHOTS_DIR}/")) else {
            return vec!["not a file of a store backup".into()];
        };
        let path = match rest.split_once('/') {
            Some((seq, file)) if seq == snapshot_name => file.to_string(),
            _ => rest.to_string(),
        };
        if path == super::snapshot_dir::MANIFEST_FILE {
            return match read_all(data)
                .map_err(|err| err.to_string())
                .and_then(|bytes| Manifest::decode(&bytes).map_err(|err| err.reason))
            {
                Ok(decoded) => {
                    manifest = Some(decoded);
                    Vec::new()
                }
                Err(reason) => vec![reason],
            };
        }
        let Some(manifest) = &manifest else {
            return vec!["a snapshot file before its manifest".into()];
        };
        match super::snapshot_dir::check_snapshot_file(manifest, &path, data, name) {
            Ok(()) => Vec::new(),
            Err(err) => vec![err.to_string()],
        }
    });
    for (path, reason) in problems {
        findings.problem(path, None, reason);
    }
    let mut expected = vec![GRAPH_FILE.to_string(), GRAPH_COPY.to_string()];
    if !plan.marks.is_empty() {
        expected.push(MARKS_FILE.to_string());
    }
    expected.extend(plan.files.iter().map(|file| file.relative.clone()));
    for name in expected {
        if !seen.contains(&name) {
            findings.problem(name, None, "missing in the archive".into());
        }
    }
    findings
}

/// The committed change sets after a position ([`Store::changes_since`](super::Store::changes_since)).
pub struct ChangesSince {
    segments: VecDeque<(usize, DirPath, Option<u64>)>,
    current: Option<JournalReader<Box<dyn Read + Send>>>,
    next: u64,
    max_depth: usize,
    failed: bool,
}

impl std::fmt::Debug for ChangesSince {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChangesSince")
            .field("next", &self.next)
            .finish_non_exhaustive()
    }
}

pub(super) fn changes_since(
    dir: &DirPath,
    commit_seq: u64,
    durable: Option<DurablePosition>,
    max_depth: usize,
) -> Result<ChangesSince, PersistError> {
    let segments = list_wal(dir)?;
    let next = commit_seq.saturating_add(1);
    if segments.first().is_none_or(|(first, _)| *first > next) {
        return Err(PersistError::TargetNotReached {
            target: format!("commit {commit_seq}"),
            reason: "the WAL after it was pruned".into(),
        });
    }
    let chosen = wal_after(&segments, commit_seq);
    let mut queue = VecDeque::new();
    for (index, (first, path)) in chosen.iter().enumerate() {
        let limit = match durable {
            Some(d) if *first > d.segment => break,
            Some(d) if *first == d.segment => Some(d.offset),
            _ => None,
        };
        queue.push_back((index, path.clone(), limit));
    }
    Ok(ChangesSince {
        segments: queue,
        current: None,
        next,
        max_depth,
        failed: false,
    })
}

/// The committed change sets after `commit_seq` in the given WAL segments (an attic entry's).
pub(super) fn changes_over(
    segments: Vec<(u64, DirPath)>,
    commit_seq: u64,
    max_depth: usize,
) -> ChangesSince {
    ChangesSince {
        segments: segments
            .into_iter()
            .enumerate()
            .map(|(index, (_, path))| (index, path, None))
            .collect(),
        current: None,
        next: commit_seq.saturating_add(1),
        max_depth,
        failed: false,
    }
}

impl Iterator for ChangesSince {
    type Item = Result<ChangeSet, PersistError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        loop {
            if self.current.is_none() {
                let (index, path, limit) = self.segments.pop_front()?;
                let opened = open_segment_reader(&path, limit)
                    .and_then(|source| JournalReader::open(source, index, self.max_depth));
                match opened {
                    Ok(reader) => self.current = Some(reader),
                    Err(err) => {
                        self.failed = true;
                        return Some(Err(err));
                    }
                }
            }
            let reader = self.current.as_mut()?;
            match reader.next_record() {
                Ok(Some((offset, _, JournalRecord::Commit(changes)))) => {
                    let seq = changes.commit_seq();
                    if seq < self.next {
                        continue;
                    }
                    if seq != self.next {
                        self.failed = true;
                        return Some(Err(PersistError::Gap {
                            journal: 0,
                            offset,
                            expected: self.next,
                            found: seq,
                        }));
                    }
                    self.next += 1;
                    return Some(Ok(changes));
                }
                Ok(Some(_)) => {}
                Ok(None) => self.current = None,
                Err(err) => {
                    self.failed = true;
                    return Some(Err(err));
                }
            }
        }
    }
}
