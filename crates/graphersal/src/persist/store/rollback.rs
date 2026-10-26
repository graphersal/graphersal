//! In-place rollback and the attic (format spec sections 11 and 12.5).
//!
//! `rollback_to(target)` moves everything after the target (later snapshots, the WAL after it;
//! the WAL segment that contains the target is split) into `attic/<time>-<first moved seq>/`,
//! starts a new lineage in `GRAPH` and a new WAL segment with the new lineage id. Every step
//! runs under an `INTENT` and is idempotent, so an interrupted rollback is completed at the
//! next open. The attic keeps the moved history: list it, read its change sets, restore it (only
//! while nothing was committed since), fork it into a new store, or remove it.
//!
//! **Every attic entry stays readable on its own terms** (format spec 11.2): an entry needs
//! its base snapshot in the store plus the store's WAL from that base up to the first commit
//! its own WAL holds. A later rollback to an EARLIER commit would move part of that history
//! into its own new entry; before it does, it copies the WAL the older entry needs into the
//! older entry (its *prefix*, segments named at or before the entry's target), points the
//! older entry's base at the rollback's own base snapshot, and drops the older entry's
//! snapshots when they reference files the rollback moves (they are derived data: the entry's
//! WAL rebuilds them). Entries therefore never depend on each other: removing, restoring or
//! forking one never breaks another, and prune keeps every entry's base.

use super::super::error::PersistError;
use super::super::manifest::is_element_kind;
use super::super::recover::{Recovery, RecoveryTarget};
use super::super::snapshot::{SnapshotInfo, WriteOptions};
use super::super::wal::{JournalReader, JournalRecord, WAL_HEADER_BYTES, WalHeader};
use super::files::{
    self, DirPath, GraphFile, Intent, LineageEntry, SNAPSHOTS_DIR, WAL_DIR, list_snapshots,
    list_wal, read_bounded, read_graph_file, remove_path, safe_relative, snapshot_name, sync_dir,
    wal_after, wal_name, write_atomic, write_graph_file,
};
use super::snapshot_dir::{DirSnapshotReader, read_manifest};
use super::wal_files::{open_segment_reader, read_marks, scan_marks, write_marks};
use crate::graph::PersistentStorage;
use crate::graph::hooks::Mark;
use crate::persist::StoreFailure;
use crate::value::physical;
use crate::value::{ElementProperty, PropertyMap};

/// The attic directory of a store.
pub(crate) const ATTIC_DIR: &str = "attic";
/// The description file of an attic entry.
const ATTIC_FILE: &str = "ATTIC";
/// The prefix of an attic entry being removed (cleaned at the next open).
pub(crate) const REMOVING_PREFIX: &str = "removing-";

/// What happens to the history after the target of an in-place rollback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum RollbackMode {
    /// Moved into the store's attic (the default): an admin mistake stays recoverable.
    #[default]
    Move,
    /// Deleted for good.
    Delete,
}

/// What [`Store::rollback_to`](super::Store::rollback_to) did.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RollbackReport {
    /// The commit the store is at now.
    pub target: u64,
    /// The commit it was at before.
    pub previous_commit_seq: u64,
    /// The new lineage id (the store's `graph_id` from now on).
    pub graph_id: u128,
    /// The snapshots moved (or deleted), by commit sequence number.
    pub snapshots_moved: Vec<u64>,
    /// The WAL segment files moved (or deleted), including the split-off tail.
    pub segments_moved: usize,
    /// The attic entry holding the moved history (`None` with [`RollbackMode::Delete`]).
    pub attic: Option<String>,
}

/// What [`Store::fork`](super::Store::fork) and [`Store::attic_fork`](super::Store::attic_fork)
/// built: the new store's first snapshot, and the marks a fork does NOT carry.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ForkReport {
    /// The new store's first (and only) snapshot: its position, lineage id and parent.
    pub snapshot: SnapshotInfo,
    /// The source's marks within the forked history (at or before the fork's position), in
    /// order. A fork starts a new lineage with its own, empty set of marks (format spec 10):
    /// these names stay targets in the source store only.
    pub marks_not_carried: Vec<Mark>,
    /// The embedding models the new store holds: every model of the source is carried
    /// (format spec 4.5).
    pub models: Vec<super::StoredModel>,
}

impl std::fmt::Display for ForkReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "Forked at commit {} (new graph {}, parent {})",
            self.snapshot.commit_seq,
            crate::value::uuid::format_canonical(self.snapshot.graph_id),
            crate::value::uuid::format_canonical(self.snapshot.parent_graph_id),
        )?;
        write_marks_not_carried(f, &self.marks_not_carried)?;
        write_models_carried(f, &self.models)
    }
}

/// The lines of the models a copy carried (`  model carried: identity (licence mit)`).
pub(super) fn write_models_carried(
    f: &mut std::fmt::Formatter<'_>,
    models: &[super::StoredModel],
) -> std::fmt::Result {
    for model in models {
        writeln!(
            f,
            "  model carried: {} (licence {})",
            if model.identity.is_empty() {
                &model.id
            } else {
                &model.identity
            },
            model.license
        )?;
    }
    Ok(())
}

/// The lines of marks a fork or repair did not carry (`  mark not carried: name @ commit N`).
pub(super) fn write_marks_not_carried(
    f: &mut std::fmt::Formatter<'_>,
    marks: &[Mark],
) -> std::fmt::Result {
    for mark in marks {
        writeln!(
            f,
            "  mark not carried: {} @ commit {}",
            mark.name(),
            mark.commit_seq()
        )?;
    }
    Ok(())
}

/// One entry of a store's attic: the history an in-place rollback moved aside.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AtticEntry {
    /// The entry's name (`<time>-<first moved commit>`), the id of the attic operations.
    pub id: String,
    /// The lineage the moved history belongs to.
    pub graph_id: u128,
    /// The lineage the rollback started.
    pub new_graph_id: u128,
    /// The commit the store was rolled back to.
    pub target: u64,
    /// The last commit of the moved history.
    pub last_commit_seq: u64,
    /// When the rollback ran (microseconds since the Unix epoch, UTC).
    pub time: i64,
    /// Why (the rollback's target, as text).
    pub reason: String,
    /// The store's snapshot the entry's history continues: the newest one at or before the
    /// target when the rollback ran, or, after a later rollback to an earlier commit, that
    /// rollback's base ([`Store::prune`](super::Store::prune) keeps it).
    pub base_snapshot: u64,
    /// The moved snapshots (commit sequence numbers).
    pub snapshots: Vec<u64>,
    /// The entry's WAL segments (their first commit sequence numbers): the moved ones, and
    /// before them, named at or before [`Self::target`], the copies of the store's history that
    /// a later rollback to an earlier commit took from the store (format spec 11.2).
    pub segments: Vec<u64>,
    /// The size of the entry's files on disk, in bytes.
    pub bytes: u64,
}

/// Microseconds since the epoch as `YYYYMMDDTHHMMSSZ`.
fn compact_time(micros: i64) -> String {
    let (year, month, day, hour, minute, second) = super::super::recover::civil(micros);
    format!("{year:04}{month:02}{day:02}T{hour:02}{minute:02}{second:02}Z")
}

/// The size of `path` in bytes: a file's length, or the total size of the files below a
/// directory (an attic entry, a snapshot directory, the store); `0` when it does not exist.
pub(super) fn tree_bytes(path: &DirPath) -> u64 {
    if path.kind() == Some(files::EntryKind::File) {
        // A file (a WAL segment, a retained snapshot file) is never listed as a directory.
        return path.len().unwrap_or(0);
    }
    let Ok(entries) = path.list() else {
        return 0;
    };
    entries
        .into_iter()
        .map(|(name, kind)| match kind {
            files::EntryKind::Dir => tree_bytes(&path.join(&name)),
            _ => path.join(&name).len().unwrap_or(0),
        })
        .sum()
}

fn hex_id(id: u128) -> String {
    format!("{id:032x}")
}

fn parse_id(text: &str) -> Option<u128> {
    (text.len() == 32)
        .then(|| u128::from_str_radix(text, 16).ok())
        .flatten()
}

pub(super) fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub(super) fn parse_hex_bytes(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

/// A flat JSON object of string values (the `ATTIC` file).
fn flat_json(pairs: &[(&str, String)]) -> String {
    let mut object = PropertyMap::default();
    for (key, value) in pairs {
        object.insert(*key, ElementProperty::from(value.as_str()));
    }
    physical::to_json_text(&ElementProperty::Object(Box::new(object))).unwrap_or_default()
}

fn read_flat_json(text: &str) -> Result<Vec<(String, String)>, String> {
    let value = physical::lift_untyped(physical::read_json(text, 8).map_err(|e| e.to_string())?);
    let ElementProperty::Object(object) = value else {
        return Err("not a JSON object".into());
    };
    Ok(object
        .iter()
        .filter_map(|(k, v)| match v {
            ElementProperty::String(s) => Some((k.to_string(), s.to_string())),
            _ => None,
        })
        .collect())
}

fn get<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// A required parameter of an intent.
fn param<'a>(intent: &'a Intent, key: &str) -> Result<&'a str, PersistError> {
    intent.param(key).ok_or_else(|| {
        PersistError::corrupt(
            files::INTENT_FILE,
            0,
            format!("the {} intent has no \"{key}\"", intent.op),
        )
    })
}

fn param_u64(intent: &Intent, key: &str) -> Result<u64, PersistError> {
    param(intent, key)?.parse().map_err(|_| {
        PersistError::corrupt(files::INTENT_FILE, 0, format!("\"{key}\" is not a number"))
    })
}

fn param_id(intent: &Intent, key: &str) -> Result<u128, PersistError> {
    parse_id(param(intent, key)?).ok_or_else(|| {
        PersistError::corrupt(
            files::INTENT_FILE,
            0,
            format!("\"{key}\" is not a lineage id"),
        )
    })
}

fn param_path(dir: &DirPath, intent: &Intent, key: &str) -> Result<DirPath, PersistError> {
    let relative = param(intent, key)?;
    safe_relative(dir, relative).ok_or_else(|| {
        PersistError::corrupt(files::INTENT_FILE, 0, format!("unsafe path {relative:?}"))
    })
}

fn io(context: &'static str) -> impl Fn(std::io::Error) -> PersistError {
    move |err| PersistError::io(context, err)
}

/// Where the history after `target` starts in the WAL.
struct Cut {
    /// The segment that holds the first commit after the target.
    segment: (u64, DirPath),
    /// The offset of that commit's record.
    offset: u64,
}

/// Finds the record of the first commit after `target` (format spec 11.1: the split point).
fn find_cut(dir: &DirPath, target: u64) -> Result<Option<Cut>, PersistError> {
    let segments = list_wal(dir)?;
    let start = wal_after(&segments, target)
        .first()
        .map(|(first, _)| *first);
    for (first, path) in &segments {
        if start.is_some_and(|start| *first < start) {
            continue;
        }
        let mut reader = JournalReader::open(
            open_segment_reader(path, None)?,
            0,
            crate::value::depth::DEFAULT_MAX_VALUE_DEPTH,
        )?;
        while let Some((offset, _, record)) = reader.next_record()? {
            if let JournalRecord::Commit(changes) = record
                && changes.commit_seq() > target
            {
                return Ok(Some(Cut {
                    segment: (*first, path.clone()),
                    offset,
                }));
            }
        }
    }
    Ok(None)
}

/// The bytes of the WAL segment that the tail of `split` after `offset` becomes (format spec
/// 11.1): a header named `first_moved` with the lineage of the segment it is cut from (an
/// ancestor's when the target lies before the current lineage's branch point), created at
/// `time`, then the records from the cut on. The attic entry of the rollback and every older
/// entry that needs the tail get exactly these bytes.
fn split_tail(
    split: &DirPath,
    offset: u64,
    first_moved: u64,
    time: i64,
    old_id: u128,
) -> Result<Vec<u8>, PersistError> {
    let lineage = split
        .read_range(0, WAL_HEADER_BYTES)
        .ok()
        .and_then(|head| WalHeader::decode(&head).ok())
        .map_or(old_id, |header| header.graph_id);
    let mut bytes = WalHeader {
        graph_id: lineage,
        first_commit_seq: first_moved,
        created_at: time,
    }
    .encode();
    let tail = files::read_bounded(split, u64::MAX).map_err(io("reading the split WAL segment"))?;
    let start = (offset as usize).min(tail.len());
    bytes.extend_from_slice(&tail[start..]);
    Ok(bytes)
}

/// The lines of a multi-line intent parameter (none when it is missing).
fn param_lines<'a>(intent: &'a Intent, key: &str) -> impl Iterator<Item = &'a str> {
    intent
        .param(key)
        .unwrap_or_default()
        .split('\n')
        .filter(|line| !line.is_empty())
}

/// The last commit before an attic entry's own WAL: the entry needs the store's history up to
/// it (its target, or an earlier commit once a later rollback copied the rest into it).
fn prefix_end(entry: &AtticEntry) -> u64 {
    entry
        .segments
        .first()
        .map_or(entry.target, |first| first.saturating_sub(1))
        .min(entry.target)
}

/// What a rollback to `target` must give the existing attic entries before it moves files
/// (format spec 11.1, `rebase`, `copies`, `drop`): `moving` are the store-relative names the
/// rollback moves (or deletes), `split` the segment it cuts (its tail is the history after the
/// target). Returns the `rebase`, `copies` and `drop` lines.
fn plan_entries(
    dir: &DirPath,
    target: u64,
    moving: &[String],
    split: Option<&str>,
) -> Result<[Vec<String>; 3], PersistError> {
    let (mut rebase, mut copies, mut drop) = (Vec::new(), Vec::new(), Vec::new());
    let first_moved = target + 1;
    let wal = list_wal(dir)?;
    for entry in attic_list(dir)? {
        let entry_dir = format!("{ATTIC_DIR}/{}", entry.id);
        let needed = prefix_end(&entry);
        if needed > target {
            // Its history after `target` up to `needed` moves with this rollback: a copy.
            if let Some(split) = split {
                copies.push(format!(
                    "{split} {entry_dir}/{WAL_DIR}/{}",
                    wal_name(first_moved)
                ));
            }
            for (first, path) in &wal {
                let name = path.relative_to(dir);
                if *first <= needed && moving.contains(&name) {
                    copies.push(format!("{name} {entry_dir}/{WAL_DIR}/{}", wal_name(*first)));
                }
            }
            rebase.push(entry.id.clone());
        }
        // Its snapshots reference files of the store's snapshots that this rollback moves:
        // they go (the entry's WAL rebuilds every state they hold).
        let path = dir.join(&entry_dir);
        let mut stale = false;
        for (_, snapshot) in entry_snapshots(&path)? {
            let Ok((_, manifest)) = read_manifest(&snapshot) else {
                continue;
            };
            for segment in manifest.segments.iter().filter(|s| is_element_kind(s.kind)) {
                let Some((holder, _)) = segment.path.split_once('/') else {
                    continue;
                };
                let near = snapshot
                    .parent()
                    .and_then(|parent| safe_relative(&parent, &segment.path));
                let moves = files::parse_seq(holder).is_some_and(|seq| seq > target);
                if moves && near.is_some_and(|near| !near.exists()) {
                    stale = true;
                }
            }
        }
        if stale {
            for (_, snapshot) in entry_snapshot_dirs(&path)? {
                drop.push(snapshot.relative_to(dir));
            }
        }
    }
    Ok([rebase, copies, drop])
}

/// Plans an in-place rollback of the store in `dir` to commit `target` (the store's lock is
/// held and the WAL is synced): the `INTENT` to write.
pub(super) fn plan_rollback(
    dir: &DirPath,
    target: u64,
    last_commit: u64,
    mode: RollbackMode,
    reason: &str,
) -> Result<Intent, PersistError> {
    let graph_file = read_graph_file(dir)?.file;
    let snapshots = list_snapshots(dir)?;
    let Some((base, _)) = snapshots.iter().rev().find(|(seq, _)| *seq <= target) else {
        return Err(PersistError::TargetNotReached {
            target: format!("commit {target}"),
            reason: "no snapshot at or before it (pruned)".into(),
        });
    };
    let Some(cut) = find_cut(dir, target)? else {
        return Err(PersistError::store(
            "roll the store back",
            StoreFailure::NothingAfterTarget,
            format!("the WAL holds no commit after commit {target}"),
        ));
    };
    let time = crate::graph::undo::now_micros();
    let first_moved = target + 1;
    // The name has a resolution of one second: a second rollback to the same target within it
    // takes the next free second (format spec 11.2), never another entry's directory.
    let mut named = time;
    let attic = loop {
        let attic = format!(
            "{ATTIC_DIR}/{}-{}",
            compact_time(named),
            snapshot_name(first_moved)
        );
        let taken = dir.join(&attic).exists()
            || dir
                .join(ATTIC_DIR)
                .join(format!(
                    "{REMOVING_PREFIX}{}-{}",
                    compact_time(named),
                    snapshot_name(first_moved)
                ))
                .exists();
        if !taken {
            break attic;
        }
        named = named.saturating_add(1_000_000);
    };
    let relative = |path: &DirPath| path.relative_to(dir);
    let mut files = Vec::new();
    // Snapshot directories after the target, holders of referenced files too (format spec
    // 4.1): they belong to the moved history.
    for (seq, path) in files::list_snapshot_dirs(dir)? {
        if seq > target {
            files.push(relative(&path));
        }
    }
    let (cut_first, cut_path) = &cut.segment;
    let mut params = vec![
        ("target".to_string(), target.to_string()),
        ("last_commit".to_string(), last_commit.to_string()),
        ("base_snapshot".to_string(), base.to_string()),
        (
            "mode".to_string(),
            match mode {
                RollbackMode::Move => "move",
                RollbackMode::Delete => "delete",
            }
            .to_string(),
        ),
        ("old_graph_id".to_string(), hex_id(graph_file.graph_id)),
        (
            "new_graph_id".to_string(),
            hex_id(crate::value::uuid::new_v7()),
        ),
        ("time".to_string(), time.to_string()),
        ("attic".to_string(), attic),
        ("reason".to_string(), reason.to_string()),
    ];
    let whole = cut.offset <= WAL_HEADER_BYTES as u64;
    for (first, path) in list_wal(dir)? {
        if first > *cut_first || (first == *cut_first && (whole || first == first_moved)) {
            files.push(relative(&path));
        }
    }
    if !whole {
        if *cut_first == first_moved {
            // The split segment carries the target's name: it moves whole, and the records it
            // holds before the cut (marks pointing at the target) open the new segment.
            let kept = cut_path
                .read_range(
                    WAL_HEADER_BYTES as u64,
                    (cut.offset - WAL_HEADER_BYTES as u64) as usize,
                )
                .map_err(io("reading a WAL segment"))?;
            params.push(("kept".to_string(), hex_bytes(&kept)));
        } else {
            params.push(("split".to_string(), relative(cut_path)));
            params.push(("split_offset".to_string(), cut.offset.to_string()));
        }
    }
    let split = params
        .iter()
        .find(|(key, _)| key == "split")
        .map(|(_, value)| value.clone());
    let [rebase, copies, drop] = plan_entries(dir, target, &files, split.as_deref())?;
    for (key, lines) in [("rebase", rebase), ("copies", copies), ("drop", drop)] {
        if !lines.is_empty() {
            params.push((key.to_string(), lines.join("\n")));
        }
    }
    Ok(Intent {
        op: "rollback".into(),
        files,
        started_at: time,
        params,
    })
}

/// Carries out (or completes, after a crash) a rollback intent; every step is idempotent.
/// `stop_after` interrupts it after that many steps (crash simulations in the crate tests).
pub(super) fn apply_rollback(
    dir: &DirPath,
    intent: &Intent,
    stop_after: Option<usize>,
) -> Result<(), PersistError> {
    let target = param_u64(intent, "target")?;
    let first_moved = target + 1;
    let old_id = param_id(intent, "old_graph_id")?;
    let new_id = param_id(intent, "new_graph_id")?;
    let time: i64 = param(intent, "time")?.parse().unwrap_or(0);
    let delete = param(intent, "mode")? == "delete";
    let attic = param_path(dir, intent, "attic")?;
    let mut step = 0usize;
    let done = |step: &mut usize| {
        *step += 1;
        stop_after.is_some_and(|stop| *step >= stop)
    };

    // 1. The attic directory.
    if !delete {
        for sub in [SNAPSHOTS_DIR, WAL_DIR] {
            attic
                .join(sub)
                .create_dir_all()
                .map_err(io("creating the attic"))?;
        }
        if let Some(parent) = attic.parent() {
            sync_dir(&parent)?;
        }
    }
    if done(&mut step) {
        return Ok(());
    }

    // 1b. Older attic entries get the history they need that moves now (format spec 11.1):
    // whole segments copied, the split segment's tail as the attic gets it. Every copy is
    // written whole under its final name, so a repeated step skips it; all of them exist
    // before step 2 changes a source.
    let mut entry_dirs = Vec::new();
    for line in param_lines(intent, "copies") {
        let unsafe_line =
            || PersistError::corrupt(files::INTENT_FILE, 0, format!("unsafe copy {line:?}"));
        let (source, target_name) = line.split_once(' ').ok_or_else(unsafe_line)?;
        let to = safe_relative(dir, target_name).ok_or_else(unsafe_line)?;
        let from = safe_relative(dir, source).ok_or_else(unsafe_line)?;
        let (Some(parent), true) = (to.parent(), target_name.starts_with(ATTIC_DIR)) else {
            return Err(unsafe_line());
        };
        if !entry_dirs
            .iter()
            .any(|p: &DirPath| p.name() == parent.name())
        {
            entry_dirs.push(parent.clone());
        }
        if to.exists() {
            continue;
        }
        let bytes = if intent.param("split") == Some(source) {
            split_tail(
                &from,
                param_u64(intent, "split_offset")?,
                first_moved,
                time,
                old_id,
            )?
        } else {
            read_bounded(&from, u64::MAX).map_err(io("copying a WAL segment to the attic"))?
        };
        parent
            .create_dir_all()
            .map_err(io("copying a WAL segment to the attic"))?;
        write_atomic(&parent, to.file_name(), &bytes)?;
    }
    for parent in &entry_dirs {
        sync_dir(parent)?;
    }
    if done(&mut step) {
        return Ok(());
    }

    // 1c. Snapshots of older entries that reference files moving now go (derived data).
    for relative in param_lines(intent, "drop") {
        let path = safe_relative(dir, relative)
            .filter(|_| relative.starts_with(ATTIC_DIR))
            .ok_or_else(|| {
                PersistError::corrupt(files::INTENT_FILE, 0, format!("unsafe path {relative:?}"))
            })?;
        remove_path(&path).map_err(io("dropping an attic entry's snapshot"))?;
        if let Some(parent) = path.parent() {
            sync_dir(&parent)?;
        }
    }
    if done(&mut step) {
        return Ok(());
    }

    // 1d. Those entries continue this rollback's base snapshot from now on.
    for id in param_lines(intent, "rebase") {
        rebase_entry(dir, id, param(intent, "base_snapshot")?)?;
    }
    if done(&mut step) {
        return Ok(());
    }

    // 2. The split: the tail after the cut becomes an attic segment, then the cut.
    if intent.param("split").is_some() {
        let split = param_path(dir, intent, "split")?;
        let offset = param_u64(intent, "split_offset")?;
        let len = split.len().map_err(io("reading the split WAL segment"))?;
        if len > offset {
            if !delete {
                let bytes = split_tail(&split, offset, first_moved, time, old_id)?;
                write_atomic(&attic.join(WAL_DIR), &wal_name(first_moved), &bytes)?;
            }
            split
                .truncate(offset)
                .map_err(io("splitting a WAL segment"))?;
        }
    }
    if done(&mut step) {
        return Ok(());
    }

    // 3. Whole files: later snapshots and WAL segments.
    for relative in &intent.files {
        let source = safe_relative(dir, relative).ok_or_else(|| {
            PersistError::corrupt(files::INTENT_FILE, 0, format!("unsafe path {relative:?}"))
        })?;
        if !source.exists() {
            continue;
        }
        if delete {
            remove_path(&source).map_err(io("deleting the rolled-back history"))?;
        } else {
            source
                .rename_to(&attic.join(relative))
                .map_err(io("moving the rolled-back history to the attic"))?;
        }
        if done(&mut step) {
            return Ok(());
        }
    }
    for sub in [SNAPSHOTS_DIR, WAL_DIR] {
        sync_dir(&dir.join(sub))?;
        if !delete {
            sync_dir(&attic.join(sub))?;
        }
    }

    // 4. The description of the attic entry.
    if !delete {
        let moved_last = param(intent, "last_commit")?.to_string();
        let ok = write_atomic(
            &attic,
            ATTIC_FILE,
            flat_json(&[
                ("graph_id", hex_id(old_id)),
                ("new_graph_id", hex_id(new_id)),
                ("target", target.to_string()),
                ("last_commit", moved_last),
                ("time", time.to_string()),
                ("reason", param(intent, "reason")?.to_string()),
                ("base_snapshot", param(intent, "base_snapshot")?.to_string()),
                (
                    "kept_bytes",
                    intent
                        .param("kept")
                        .map_or(0, |kept| kept.len() / 2)
                        .to_string(),
                ),
            ])
            .as_bytes(),
        );
        ok?;
    }
    if done(&mut step) {
        return Ok(());
    }

    // 5. The marks after the target leave with their history.
    let marks: Vec<Mark> = read_marks(dir)?
        .unwrap_or_else(|| scan_marks(&list_wal(dir).unwrap_or_default(), 4096))
        .into_iter()
        .filter(|mark| mark.commit_seq() <= target)
        .collect();
    write_marks(dir, &marks)?;
    if done(&mut step) {
        return Ok(());
    }

    // 6. The new lineage in GRAPH, with its WAL segment (`target + 1`, started in step 7, also
    // when this rollback is completed at the next open) as the newest one (format spec 9.8).
    let mut graph_file = read_graph_file(dir)?.file;
    let head = Some(target + 1);
    if graph_file.graph_id != new_id
        || graph_file.wal_head != head
        || graph_file.closed_at.is_some()
    {
        if graph_file.graph_id != new_id {
            graph_file.lineage.insert(
                0,
                LineageEntry {
                    graph_id: new_id,
                    branched_at: target,
                    time,
                },
            );
            graph_file.graph_id = new_id;
        }
        graph_file.wal_head = head;
        // The history changed: a clean close records its position again.
        graph_file.closed_at = None;
        write_graph_file(dir, &graph_file)?;
    }
    if done(&mut step) {
        return Ok(());
    }

    // 7. The new lineage's first WAL segment (with the records kept before the cut).
    // Written whole (a crash leaves it complete or missing); one found shorter than it (a
    // crash of an older version, or an interrupted backend write) is written again: nothing
    // is appended to it before the INTENT is gone.
    let path = dir.join(WAL_DIR).join(wal_name(first_moved));
    let mut bytes = WalHeader {
        graph_id: new_id,
        first_commit_seq: first_moved,
        created_at: time,
    }
    .encode();
    if let Some(kept) = intent.param("kept") {
        bytes.extend_from_slice(
            &parse_hex_bytes(kept).ok_or_else(|| {
                PersistError::corrupt(files::INTENT_FILE, 0, "\"kept\" is not hex")
            })?,
        );
    }
    if path.len().map_or(true, |len| len < bytes.len() as u64) {
        write_atomic(&dir.join(WAL_DIR), &wal_name(first_moved), &bytes)?;
    }
    sync_dir(&dir.join(WAL_DIR))?;
    // 8. Holders whose files no snapshot uses any more (the history that referenced them was
    // deleted, or an older entry's snapshots were dropped).
    super::maintenance::remove_unused_holders(dir)?;
    Intent::clear(dir)
}

/// The attic entries of the store in `dir`, oldest first.
pub(super) fn attic_list(dir: &DirPath) -> Result<Vec<AtticEntry>, PersistError> {
    let root = dir.join(ATTIC_DIR);
    let mut out = Vec::new();
    let Ok(entries) = root.list() else {
        return Ok(out);
    };
    for (name, kind) in entries {
        if name.starts_with(REMOVING_PREFIX) || kind != files::EntryKind::Dir {
            continue;
        }
        out.push(read_entry(dir, &name)?);
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

/// The snapshot directories of every attic entry (their references into the store's
/// `snapshots/` keep files a prune would otherwise remove, format spec 12.4).
pub(super) fn attic_snapshot_dirs(dir: &DirPath) -> Result<Vec<DirPath>, PersistError> {
    let mut out = Vec::new();
    for entry in attic_list(dir)? {
        let path = entry_path(dir, &entry.id)?;
        out.extend(entry_snapshots(&path)?.into_iter().map(|(_, path)| path));
    }
    Ok(out)
}

/// Reads one attic entry.
fn read_entry(dir: &DirPath, id: &str) -> Result<AtticEntry, PersistError> {
    let path = entry_path(dir, id)?;
    let location = format!("{ATTIC_DIR}/{id}/{ATTIC_FILE}");
    let bytes =
        read_bounded(&path.join(ATTIC_FILE), 1 << 20).map_err(io("reading an attic entry"))?;
    let text =
        String::from_utf8(bytes).map_err(|_| PersistError::corrupt(&location, 0, "not UTF-8"))?;
    let pairs =
        read_flat_json(&text).map_err(|reason| PersistError::corrupt(&location, 0, reason))?;
    let number = |key: &str| -> Result<u64, PersistError> {
        get(&pairs, key)
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| PersistError::corrupt(&location, 0, format!("no number \"{key}\"")))
    };
    let id_of = |key: &str| -> Result<u128, PersistError> {
        get(&pairs, key)
            .and_then(parse_id)
            .ok_or_else(|| PersistError::corrupt(&location, 0, format!("no lineage id \"{key}\"")))
    };
    let snapshots = entry_snapshots(&path)?
        .into_iter()
        .map(|(s, _)| s)
        .collect();
    let segments = entry_wal(&path)?.into_iter().map(|(s, _)| s).collect();
    Ok(AtticEntry {
        id: id.to_string(),
        graph_id: id_of("graph_id")?,
        new_graph_id: id_of("new_graph_id")?,
        target: number("target")?,
        last_commit_seq: number("last_commit")?,
        time: get(&pairs, "time")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        reason: get(&pairs, "reason").unwrap_or_default().to_string(),
        base_snapshot: number("base_snapshot")?,
        snapshots,
        segments,
        bytes: tree_bytes(&path),
    })
}

/// Points attic entry `id` at the base snapshot `base` (format spec 11.1, `rebase`): its
/// `ATTIC` file is rewritten with every other field as it was. Idempotent; a missing entry
/// (removed meanwhile) is fine.
fn rebase_entry(dir: &DirPath, id: &str, base: &str) -> Result<(), PersistError> {
    let Ok(path) = entry_path(dir, id) else {
        return Ok(());
    };
    let location = format!("{ATTIC_DIR}/{id}/{ATTIC_FILE}");
    let bytes =
        read_bounded(&path.join(ATTIC_FILE), 1 << 20).map_err(io("reading an attic entry"))?;
    let text =
        String::from_utf8(bytes).map_err(|_| PersistError::corrupt(&location, 0, "not UTF-8"))?;
    let mut pairs =
        read_flat_json(&text).map_err(|reason| PersistError::corrupt(&location, 0, reason))?;
    if get(&pairs, "base_snapshot") == Some(base) {
        return Ok(());
    }
    pairs.retain(|(key, _)| key != "base_snapshot");
    pairs.push(("base_snapshot".to_string(), base.to_string()));
    let pairs: Vec<(&str, String)> = pairs
        .iter()
        .map(|(key, value)| (key.as_str(), value.clone()))
        .collect();
    write_atomic(&path, ATTIC_FILE, flat_json(&pairs).as_bytes())
}

/// The snapshots of the attic entry in `path` (none when its `snapshots/` is gone: a backend
/// without empty directories drops it with its last snapshot).
fn entry_snapshots(path: &DirPath) -> Result<Vec<(u64, DirPath)>, PersistError> {
    if !path.join(SNAPSHOTS_DIR).exists() {
        return Ok(Vec::new());
    }
    list_snapshots(path)
}

/// The snapshot directories of the attic entry in `path`, holders too (see [`entry_snapshots`]).
fn entry_snapshot_dirs(path: &DirPath) -> Result<Vec<(u64, DirPath)>, PersistError> {
    if !path.join(SNAPSHOTS_DIR).exists() {
        return Ok(Vec::new());
    }
    files::list_snapshot_dirs(path)
}

/// The WAL segments of the attic entry in `path` (see [`entry_snapshots`]).
fn entry_wal(path: &DirPath) -> Result<Vec<(u64, DirPath)>, PersistError> {
    if !path.join(WAL_DIR).exists() {
        return Ok(Vec::new());
    }
    list_wal(path)
}

/// The directory of attic entry `id` (a plain name: no path parts).
fn entry_path(dir: &DirPath, id: &str) -> Result<DirPath, PersistError> {
    if id.contains('/') || id.starts_with(REMOVING_PREFIX) {
        return Err(PersistError::store(
            "use the attic entry",
            StoreFailure::UnknownAtticEntry,
            format!("{id:?} is not an attic entry name"),
        ));
    }
    let path = safe_relative(&dir.join(ATTIC_DIR), id).ok_or_else(|| {
        PersistError::store(
            "use the attic entry",
            StoreFailure::UnknownAtticEntry,
            format!("{id:?} is not an attic entry name"),
        )
    })?;
    if !path.is_dir() {
        return Err(PersistError::store(
            "use the attic entry",
            StoreFailure::UnknownAtticEntry,
            format!(
                "the store has no attic entry {id:?} (graphersal store attic <dir> lists them)"
            ),
        ));
    }
    Ok(path)
}

/// The WAL segments of an attic entry.
pub(super) fn attic_segments(
    dir: &DirPath,
    id: &str,
) -> Result<(AtticEntry, Vec<(u64, DirPath)>), PersistError> {
    let entry = read_entry(dir, id)?;
    let segments = entry_wal(&entry_path(dir, id)?)?;
    Ok((entry, segments))
}

/// Removes attic entry `id` (renamed first, so an interruption leaves no half entry).
pub(super) fn attic_remove(dir: &DirPath, id: &str) -> Result<(), PersistError> {
    let path = entry_path(dir, id)?;
    let doomed = dir.join(ATTIC_DIR).join(format!("{REMOVING_PREFIX}{id}"));
    path.rename_to(&doomed)
        .map_err(io("removing an attic entry"))?;
    sync_dir(&dir.join(ATTIC_DIR))?;
    remove_path(&doomed).map_err(io("removing an attic entry"))?;
    // Holders kept only for the entry's snapshots go with it (format spec 12.5).
    super::maintenance::remove_unused_holders(dir)?;
    Ok(())
}

/// Removes attic entries whose removal was interrupted; returns how many.
pub(super) fn remove_interrupted(dir: &DirPath) -> usize {
    let mut removed = 0;
    let root = dir.join(ATTIC_DIR);
    if let Ok(entries) = root.list() {
        for (name, _) in entries {
            if name.starts_with(REMOVING_PREFIX) && remove_path(&root.join(&name)).is_ok() {
                removed += 1;
            }
        }
    }
    removed
}

/// Plans restoring attic entry `id` into the store: allowed only while nothing was committed
/// (and no mark set) since its rollback.
pub(super) fn plan_restore(
    dir: &DirPath,
    id: &str,
    commit_seq: u64,
) -> Result<Intent, PersistError> {
    let entry = read_entry(dir, id)?;
    let graph_file = read_graph_file(dir)?.file;
    let refuse = |cause: StoreFailure, reason: String| {
        Err(PersistError::store(
            "restore the attic entry",
            cause,
            reason,
        ))
    };
    if graph_file.graph_id != entry.new_graph_id {
        // Entries restore newest first: a later rollback from this entry's lineage is in the
        // attic as the entry to restore before this one.
        let newer = attic_list(dir)
            .ok()
            .and_then(|entries| {
                entries
                    .into_iter()
                    .find(|other| other.id != id && other.graph_id == entry.new_graph_id)
            })
            .map(|other| {
                format!(
                    "restore the newer entry {} first (graphersal store attic <dir> restore {}), \
                     or ",
                    other.id, other.id
                )
            })
            .unwrap_or_default();
        return refuse(
            StoreFailure::AtticLineageChanged,
            format!(
                "the store's lineage changed since that rollback (another rollback or restore; \
             attic entries restore newest first); {newer}fork the entry instead \
             (graphersal store attic <dir> fork {id} <new_dir>)"
            ),
        );
    }
    if commit_seq != entry.target {
        return refuse(
            StoreFailure::ChangedSinceRollback,
            format!(
                "the store has new commits since the rollback (it is at commit {commit_seq}, the \
             rollback went to {}); fork the entry instead",
                entry.target
            ),
        );
    }
    let kept = read_entry_kept(dir, id)?;
    let new_segment = dir.join(WAL_DIR).join(wal_name(entry.target + 1));
    if let Ok(len) = new_segment.len()
        && len > WAL_HEADER_BYTES as u64 + kept
    {
        return refuse(
            StoreFailure::ChangedSinceRollback,
            "marks were set since the rollback; fork the entry instead".to_string(),
        );
    }
    let entry_dir = format!("{ATTIC_DIR}/{id}");
    let mut files = Vec::new();
    // Snapshot directories, holders of referenced files too.
    for (_, path) in entry_snapshot_dirs(&dir.join(&entry_dir))? {
        files.push(format!("{SNAPSHOTS_DIR}/{}", path.file_name()));
    }
    // The moved WAL; the copies of the store's history up to the target (format spec 11.2)
    // stay behind: the store holds that history again (the later rollback that took it was
    // undone first, or the store's lineage would differ).
    for (first, _) in entry_wal(&dir.join(&entry_dir))? {
        if first > entry.target {
            files.push(format!("{WAL_DIR}/{}", wal_name(first)));
        }
    }
    // (The new lineage's segment, named after the target, goes first: step 1.)
    let new_lineage = format!("{WAL_DIR}/{}", wal_name(entry.target + 1));
    if let Some(taken) = files
        .iter()
        .find(|name| **name != new_lineage && dir.join(name.as_str()).exists())
    {
        return refuse(
            StoreFailure::ChangedSinceRollback,
            format!("the store already holds {taken}; fork the entry instead"),
        );
    }
    Ok(Intent {
        op: "attic_restore".into(),
        files,
        started_at: crate::graph::undo::now_micros(),
        params: vec![
            ("attic".to_string(), entry_dir),
            ("target".to_string(), entry.target.to_string()),
            ("old_graph_id".to_string(), hex_id(entry.graph_id)),
            ("new_graph_id".to_string(), hex_id(entry.new_graph_id)),
        ],
    })
}

fn read_entry_kept(dir: &DirPath, id: &str) -> Result<u64, PersistError> {
    let path = entry_path(dir, id)?;
    let bytes =
        read_bounded(&path.join(ATTIC_FILE), 1 << 20).map_err(io("reading an attic entry"))?;
    let pairs = read_flat_json(&String::from_utf8_lossy(&bytes)).unwrap_or_default();
    Ok(get(&pairs, "kept_bytes")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0))
}

/// Carries out (or completes) an attic restore intent; idempotent steps.
pub(super) fn apply_restore(
    dir: &DirPath,
    intent: &Intent,
    stop_after: Option<usize>,
) -> Result<(), PersistError> {
    let target = param_u64(intent, "target")?;
    let new_id = param_id(intent, "new_graph_id")?;
    let attic = param_path(dir, intent, "attic")?;
    let mut step = 0usize;
    let done = |step: &mut usize| {
        *step += 1;
        stop_after.is_some_and(|stop| *step >= stop)
    };

    // 1. The new lineage's WAL segment goes (it holds no commit), and so does a snapshot of
    // the new lineage at the target (a checkpoint of a closed store after the rollback writes
    // none into the WAL): the old lineage's history replaces both.
    let new_segment = dir.join(WAL_DIR).join(wal_name(target + 1));
    if new_segment.exists() && segment_id(&new_segment) == Some(new_id) {
        remove_path(&new_segment).map_err(io("removing the new lineage's WAL segment"))?;
        sync_dir(&dir.join(WAL_DIR))?;
    }
    let at_target = dir.join(SNAPSHOTS_DIR).join(snapshot_name(target));
    if at_target.join(super::snapshot_dir::MANIFEST_FILE).exists()
        && super::snapshot_dir::read_listing(&at_target).is_ok_and(|info| info.graph_id == new_id)
    {
        remove_path(&at_target).map_err(io("removing the new lineage's snapshot"))?;
        sync_dir(&dir.join(SNAPSHOTS_DIR))?;
    }
    if done(&mut step) {
        return Ok(());
    }

    // 2. The moved history comes back.
    for relative in &intent.files {
        let source = safe_relative(&attic, relative).ok_or_else(|| {
            PersistError::corrupt(files::INTENT_FILE, 0, format!("unsafe path {relative:?}"))
        })?;
        if source.exists() {
            source
                .rename_to(&dir.join(relative))
                .map_err(io("moving the attic history back"))?;
        }
        if done(&mut step) {
            return Ok(());
        }
    }
    for sub in [SNAPSHOTS_DIR, WAL_DIR] {
        sync_dir(&dir.join(sub))?;
    }

    // 3. The lineage of the rollback goes; the newest segment is the restored one.
    let mut graph_file = read_graph_file(dir)?.file;
    let head = list_wal(dir)?.last().map(|(first, _)| *first);
    if (graph_file.graph_id == new_id && graph_file.lineage.len() > 1)
        || graph_file.wal_head != head
        || graph_file.closed_at.is_some()
    {
        if graph_file.graph_id == new_id && graph_file.lineage.len() > 1 {
            graph_file.lineage.remove(0);
            graph_file.graph_id = graph_file.lineage[0].graph_id;
        }
        graph_file.wal_head = head;
        // The history changed: a clean close records its position again.
        graph_file.closed_at = None;
        write_graph_file(dir, &graph_file)?;
    }
    if done(&mut step) {
        return Ok(());
    }

    // 4. The marks of the restored history, from the WAL.
    write_marks(dir, &scan_marks(&list_wal(dir)?, 4096))?;
    if done(&mut step) {
        return Ok(());
    }

    // 5. The empty attic entry.
    if attic.exists() {
        remove_path(&attic).map_err(io("removing the restored attic entry"))?;
        sync_dir(&dir.join(ATTIC_DIR))?;
    }
    Intent::clear(dir)
}

/// The lineage id in a WAL segment's header.
fn segment_id(path: &DirPath) -> Option<u128> {
    let source = open_segment_reader(path, Some(WAL_HEADER_BYTES as u64)).ok()?;
    JournalReader::open(source, 0, 1).ok().map(|r| r.graph_id())
}

/// The state at the end of attic entry `id` (the moved history replayed onto the store's
/// state at the rollback target), as a new graph of the entry's lineage.
pub(super) fn attic_state(
    dir: &DirPath,
    id: &str,
    read: &super::super::snapshot::ReadOptions,
) -> Result<(AtticEntry, GraphFile, crate::graph::TraversalGraph), PersistError> {
    let (entry, attic_segments) = attic_segments(dir, id)?;
    let entry_dir = entry_path(dir, id)?;
    let graph_file = read_graph_file(dir)?.file;
    // The chain as it was before the rollback: from the entry's lineage on.
    let position = graph_file
        .lineage
        .iter()
        .position(|e| e.graph_id == entry.graph_id)
        .ok_or_else(|| {
            PersistError::store(
                "read the attic entry",
                StoreFailure::AtticLineageChanged,
                "its lineage is not on the store's chain any more",
            )
        })?;
    let mut old = graph_file.clone();
    old.lineage = graph_file.lineage[position..].to_vec();
    old.graph_id = entry.graph_id;
    // The newest snapshot moved with the entry, else its base snapshot in the store (format
    // spec 11.2; the store's newest snapshot at or before the target may be of the new
    // lineage).
    let own = entry_snapshots(&entry_dir)?.pop();
    let (base, path, in_attic) = match own {
        Some((seq, path)) => (seq, path, true),
        None => {
            let path = dir
                .join(SNAPSHOTS_DIR)
                .join(snapshot_name(entry.base_snapshot));
            if !path.join(super::snapshot_dir::MANIFEST_FILE).exists() {
                return Err(PersistError::store(
                    "read the attic entry",
                    StoreFailure::AtticBaseMissing,
                    format!(
                        "the base snapshot at commit {} it continues is missing",
                        entry.base_snapshot
                    ),
                ));
            }
            (entry.base_snapshot, path, false)
        }
    };
    let mut journals: Vec<DirPath> = Vec::new();
    if !in_attic {
        // The store's WAL up to the entry's own (its prefix, format spec 11.2).
        let prefix_end = prefix_end(&entry);
        let main: Vec<(u64, DirPath)> = list_wal(dir)?
            .into_iter()
            .filter(|(first, _)| *first <= prefix_end)
            .collect();
        journals.extend(wal_after(&main, base).iter().map(|(_, p)| p.clone()));
        journals.extend(attic_segments.iter().map(|(_, p)| p.clone()));
    } else {
        journals.extend(
            wal_after(&attic_segments, base)
                .iter()
                .map(|(_, p)| p.clone()),
        );
    }
    let mut recovery = Recovery::new()
        .snapshot(DirSnapshotReader::open(&path)?)
        .options(read.clone())
        .target(RecoveryTarget::Latest)
        .chain(old.chain());
    for path in &journals {
        recovery = recovery.journal(open_segment_reader(path, None)?);
    }
    let recovered = recovery.run()?;
    Ok((entry, old, recovered.graph))
}

/// A new store in `new_dir` from the end of attic entry `id`: a new lineage whose parent is
/// the entry's lineage at its last commit.
pub(super) fn attic_fork(
    dir: &DirPath,
    id: &str,
    new_dir: &DirPath,
) -> Result<ForkReport, PersistError> {
    super::files::refuse_other_version(dir)?;
    let options = super::StoreOptions::new();
    let (entry, old, graph) = attic_state(dir, id, &options.read)?;
    // The creation-time parameters of the store carry over (a fork is the same data under a
    // new lineage, not a new decision).
    let options = options
        .with_chunk_bytes(old.chunk_bytes)
        .with_damage_policy(old.damage_policy);
    let branched_at = graph.last_commit_seq();
    let parent_id = graph.graph_id();
    let graph = graph.with_graph_id(crate::value::uuid::new_v7());
    let mut lineage = vec![LineageEntry {
        graph_id: graph.graph_id(),
        branched_at,
        time: crate::graph::undo::now_micros(),
    }];
    lineage.extend(old.lineage.iter().copied());
    let existed = new_dir.exists();
    super::StoreCore::create_lineage(
        new_dir,
        &graph,
        lineage,
        WriteOptions::new().with_parent(parent_id, branched_at),
        &options,
    )?;
    let models = super::models::carry_into_new(dir, new_dir, existed)?;
    let (_, path) = list_snapshots(new_dir)?.pop().ok_or_else(|| {
        PersistError::store(
            "fork the attic entry",
            StoreFailure::ChangedConcurrently,
            "the new store has no snapshot",
        )
    })?;
    Ok(ForkReport {
        snapshot: super::snapshot_dir::read_listing(&path)?,
        marks_not_carried: attic_marks(dir, &entry, branched_at),
        models,
    })
}

/// The marks of attic entry `entry`'s history up to `up_to` (best effort, for a report): the
/// store's marks up to the entry's target, then the marks recorded in the entry's own WAL.
fn attic_marks(dir: &DirPath, entry: &AtticEntry, up_to: u64) -> Vec<Mark> {
    let mut marks = super::marks_up_to(dir, entry.target);
    let own = entry_path(dir, &entry.id)
        .and_then(|path| entry_wal(&path))
        .map(|segments| scan_marks(&segments, crate::value::depth::DEFAULT_MAX_VALUE_DEPTH))
        .unwrap_or_default();
    for mark in own {
        if mark.commit_seq() <= up_to
            && !marks
                .iter()
                .any(|m| m.name() == mark.name() && m.commit_seq() == mark.commit_seq())
        {
            marks.push(mark);
        }
    }
    marks.sort_by_key(Mark::commit_seq);
    marks
}

/// The base snapshots attic entries need (prune keeps them and the WAL after them).
/// The attic entries' base snapshots from NAMES only (no file is read; the cheap prune
/// estimate): an entry `<time>-<first moved commit>` needs the newest snapshot at or before
/// its target (the commit before the first moved one), as its rollback chose it.
pub(super) fn attic_bases_by_name(dir: &DirPath) -> Result<Vec<(String, u64)>, PersistError> {
    let Ok(entries) = dir.join(ATTIC_DIR).list() else {
        return Ok(Vec::new());
    };
    let snapshots: Vec<u64> = list_snapshots(dir)?.into_iter().map(|(s, _)| s).collect();
    let mut out = Vec::new();
    for (name, kind) in entries {
        if name.starts_with(REMOVING_PREFIX) || kind != files::EntryKind::Dir {
            continue;
        }
        let Some(first_moved) = name
            .rsplit_once('-')
            .and_then(|(_, seq)| seq.parse::<u64>().ok())
        else {
            continue;
        };
        // The history the entry continues ends before its own WAL (format spec 11.2).
        let own_first = list_wal(&dir.join(ATTIC_DIR).join(&name))
            .ok()
            .and_then(|wal| wal.first().map(|(first, _)| *first))
            .unwrap_or(first_moved);
        let target = first_moved.min(own_first).saturating_sub(1);
        let base = snapshots
            .iter()
            .rev()
            .find(|s| **s <= target)
            .copied()
            .unwrap_or(0);
        out.push((name, base));
    }
    Ok(out)
}

pub(super) fn attic_bases(dir: &DirPath) -> Vec<(String, u64)> {
    attic_list(dir)
        .unwrap_or_default()
        .into_iter()
        .map(|entry| (entry.id, entry.base_snapshot))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attic_names_and_hex_round_trip() {
        assert_eq!(compact_time(0), "19700101T000000Z");
        assert_eq!(compact_time(1_760_000_000_000_000), "20251009T085320Z");
        assert_eq!(
            parse_hex_bytes(&hex_bytes(&[0, 1, 254])),
            Some(vec![0, 1, 254])
        );
        assert_eq!(parse_id(&hex_id(7)), Some(7));
        let pairs = read_flat_json(&flat_json(&[("a", "1".into())])).unwrap();
        assert_eq!(get(&pairs, "a"), Some("1"));
    }
}
