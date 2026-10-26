//! [`PersistError`]: every failure of the persistence formats.

use crate::error::{DiagnosticError, GraphError};

/// A failure while writing or reading a snapshot or a journal, or while recovering.
///
/// Every decoder is hostile-input safe: damaged or malicious bytes come back as
/// [`PersistError::Corrupt`] (with the section and byte offset), never as a panic.
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum PersistError {
    /// Reading from or writing to the stream failed.
    #[error("I/O error while {context}: {source}")]
    #[non_exhaustive]
    Io {
        /// What was being done.
        context: &'static str,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },

    /// The bytes are not a valid file of the format: a checksum mismatch, a truncated section,
    /// an out-of-range length, an unknown tag, ids out of order, ...
    #[error("Corrupt {location} at byte {offset}: {reason}")]
    #[non_exhaustive]
    Corrupt {
        /// Which part: `"packed snapshot"`, `"manifest"`, `"vertex segment 0"`,
        /// `"journal 1"`, ...
        location: String,
        /// The byte offset within that part (or within the stream for a journal).
        offset: u64,
        /// What is wrong.
        reason: String,
    },

    /// The store or file was written by a newer format version than this build reads (format
    /// spec 15): never damage, nothing is changed.
    #[error(
        "Unsupported format version {found} of the {location}: written by a newer Graphersal \
         (this build reads up to version {supported})"
    )]
    #[non_exhaustive]
    UnsupportedVersion {
        /// Which file or section.
        location: String,
        /// The version in the file.
        found: u32,
        /// The version this build reads and writes.
        supported: u32,
    },

    /// A part of a file whose checksum is valid holds content this version cannot interpret: a
    /// schema keyword, a definition payload or a WAL record body written by a newer Graphersal
    /// (format spec 16). Never damage: nothing is changed, the store is never opened in
    /// maintenance mode or repaired by this version (a repair would drop what it cannot read).
    #[error(
        "Unsupported content in the {location} at byte {offset}: {reason} (its checksum is \
         valid: written by a newer Graphersal, not damage)"
    )]
    #[non_exhaustive]
    UnsupportedContent {
        /// Which file or section.
        location: String,
        /// The byte offset within it.
        offset: u64,
        /// What this version cannot interpret.
        reason: String,
    },

    /// The snapshot does not fit into the memory budget: its header's estimate of the loaded
    /// graph is larger than the limit given to the load.
    #[error(
        "The snapshot needs about {needed} bytes in memory, more than the memory limit of {limit} bytes"
    )]
    #[non_exhaustive]
    MemoryBudget {
        /// The estimate from the snapshot header.
        needed: u64,
        /// The configured limit.
        limit: u64,
    },

    /// A journal belongs to another graph lineage than the snapshot (or the first journal).
    #[error("Journal {journal} belongs to graph {found}, not to graph {expected}")]
    #[non_exhaustive]
    LineageMismatch {
        /// The index of the journal in the recovery's list.
        journal: usize,
        /// The lineage id the recovery expects (canonical UUID text).
        expected: String,
        /// The lineage id in the journal header.
        found: String,
    },

    /// The journals do not continue the snapshot without a gap: a commit is missing.
    #[error("Journal {journal} at byte {offset}: expected commit {expected}, found commit {found}")]
    #[non_exhaustive]
    Gap {
        /// The index of the journal in the recovery's list.
        journal: usize,
        /// The byte offset of the record in that journal.
        offset: u64,
        /// The commit sequence number that should come next.
        expected: u64,
        /// The commit sequence number found instead.
        found: u64,
    },

    /// The recovery target is not in the snapshot and journals given.
    #[error("Recovery target {target} not reached: {reason}")]
    #[non_exhaustive]
    TargetNotReached {
        /// The target as text (`commit 12`, `time 1760000000000000`, `mark "x"`).
        target: String,
        /// Why.
        reason: String,
    },

    /// A value or record cannot be written in the format (a vertex as a property value, a
    /// record larger than 4 GiB, a value nested too deeply).
    #[error("Cannot encode {what}: {reason}")]
    #[non_exhaustive]
    Encode {
        /// What was being encoded.
        what: String,
        /// Why it cannot be.
        reason: String,
    },

    /// One commit is too large for the journal: its record body (every mutation of the unit
    /// with its before and after values, before compression) exceeds the per-commit limit of
    /// 1 GiB (format spec 9.3). The journal refuses (vetoes) the commit, so the unit is rolled
    /// back and nothing is written; the journal stays usable. Split the work into several
    /// smaller units.
    #[error(
        "Commit {commit_seq} is too large for the journal: its uncompressed record body is \
         {bytes} bytes, more than the limit of {limit} bytes per commit"
    )]
    #[non_exhaustive]
    CommitTooLarge {
        /// The sequence number the refused commit would have had.
        commit_seq: u64,
        /// The size of its uncompressed record body.
        bytes: usize,
        /// The limit per commit (1 GiB).
        limit: usize,
    },

    /// The journal stopped accepting writes after a failed write or sync: the outcome of the
    /// failed commit on disk is unknown, so nothing may follow it.
    #[error("The journal is poisoned by an earlier failure: {reason}")]
    #[non_exhaustive]
    Poisoned {
        /// The earlier failure.
        reason: String,
    },

    /// The Store directory is locked by another writer (another process, or another `Store`
    /// of this process): one directory has one writer.
    #[error("The store {path} is in use: another writer holds its lock")]
    #[non_exhaustive]
    Locked {
        /// The store directory.
        path: String,
    },

    /// The directory is not a Store (no valid `GRAPH` file).
    #[error("{path} is not a Graphersal store: {reason}")]
    #[non_exhaustive]
    NotAStore {
        /// The directory.
        path: String,
        /// Why.
        reason: String,
    },

    /// A new store (a [`Store::create`](super::Store::create), the target of a
    /// [`Store::convert`](super::Store::convert)) needs a new or empty directory, and this one
    /// holds something (perhaps a store already).
    #[error("Cannot create a store in {path}: the directory is not empty")]
    #[non_exhaustive]
    DirectoryNotEmpty {
        /// The directory.
        path: String,
    },

    /// The Store was written by an older store format version (its `GRAPH` file is valid but
    /// of an older version): this build does not open it as it is. `help()` names the
    /// converter for a released version ([`Store::upgrade`](super::Store::upgrade), CLI
    /// `graphersal store upgrade`, which needs no older Graphersal), and for a pre-release
    /// version the migration through a packed snapshot written by the Graphersal that made it.
    #[error(
        "The store {path} has the older format version {found}; this build reads and writes \
         only version {supported}"
    )]
    #[non_exhaustive]
    OlderFormat {
        /// The store directory (or file).
        path: String,
        /// The version in its `GRAPH` file.
        found: u32,
        /// The version this build reads and writes.
        supported: u32,
    },

    /// A Store operation cannot be done in the store's current state (a snapshot name already
    /// taken, a backup running during a prune, the store closed, ...). `cause` says which, for
    /// a caller that reacts to it; `help()` gives the advice per cause.
    #[error("Cannot {operation}: {reason}")]
    #[non_exhaustive]
    Store {
        /// The operation.
        operation: &'static str,
        /// Why not, as one of a closed set of causes.
        cause: StoreFailure,
        /// Why not, in words (names the commit, the snapshot, the entry, ...).
        reason: String,
    },

    /// The store opened in maintenance mode: damage was found when it was opened, so it is
    /// read-only (format spec 13). Commits, marks, checkpoints, prune and rollback are refused.
    #[error(
        "The store {path} is in maintenance mode: {damage} damaged item(s) were found when it \
         was opened, so it is read-only"
    )]
    #[non_exhaustive]
    Maintenance {
        /// The store directory.
        path: String,
        /// How many damaged items the scan found.
        damage: usize,
    },

    /// The store holds something this version does not know and must not write around: a
    /// critical definition of an unknown kind (a property index of a newer version, ...) or a
    /// snapshot file of a critical unknown segment kind (format spec 16 rule 3). It opens
    /// read-only: the data is readable, commits, marks, checkpoints, prune and rollback are
    /// refused.
    #[error("The store {path} is read-only: {reason}")]
    #[non_exhaustive]
    ReadOnly {
        /// The store directory.
        path: String,
        /// What makes it read-only.
        reason: String,
    },

    /// The directory is a backup (its `GRAPH` file carries the backup marker, format spec 12.6):
    /// it opens read-only, so nothing writes to it by accident. Restoring it
    /// ([`Store::restore_backup`](super::Store::restore_backup)) makes it the live store.
    #[error("{path} is a backup (up to commit {commit_seq}): it opens read-only")]
    #[non_exhaustive]
    IsBackup {
        /// The backup directory.
        path: String,
        /// The last commit it holds.
        commit_seq: u64,
    },

    /// A backup was refused (format spec 12.7): the target directory holds a backup of another
    /// graph, of a different store (a fork, repair or copy of the backed-up store: another
    /// store id), one the store was rolled back behind, a store that is not a backup, or the
    /// store no longer has the WAL an increment needs. The target is left as it was.
    #[error("Cannot back up the store: {reason}")]
    #[non_exhaustive]
    BackupRefused {
        /// Why.
        reason: String,
    },

    /// A backup stopped on damage (format spec 12.6): a checksum of what it copies failed in
    /// the store (the source) or in the copy it wrote (the backup). Nothing of the failed
    /// backup is kept: a new backup directory is removed, an increment is rolled back. The
    /// details (which side, the problems as [`Store::verify`](super::Store::verify) reports
    /// them) are in [`BackupDamage`].
    #[error("{0}")]
    BackupDamaged(Box<BackupDamage>),

    /// The open store found damage on disk (a backup, a checkpoint or a verification found
    /// it) and its damage policy is `maintenance` (the default, format spec 5.2 bit 5): it is
    /// read-only from then on. Commits, marks, checkpoints, prune and rollback are refused;
    /// reading and a backup from memory work.
    #[error(
        "The store {path} is read-only: {found_by} found damage on disk ({problems} problem(s)) \
         and its damage policy is \"maintenance\""
    )]
    #[non_exhaustive]
    DamageFound {
        /// The store directory.
        path: String,
        /// What found it first: `"backup"`, `"checkpoint"` or `"verify"`.
        found_by: String,
        /// How many problems it found.
        problems: usize,
    },

    /// A graph operation failed while loading or replaying (a journal record that does not
    /// apply, a sequence overflow, registering the journal).
    #[error(transparent)]
    Graph(#[from] GraphError),
}

/// Which copy a [`BackupDamage`] was found in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DamageSide {
    /// The store being backed up: repair the store (never the backup).
    Store,
    /// The backup directory or archive: the copy written did not read back intact, or the
    /// backup an increment continues is damaged. The store is not affected.
    Backup,
}

/// The details of [`PersistError::BackupDamaged`]: where the damage is and what it is.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct BackupDamage {
    /// Where the damage is.
    pub side: DamageSide,
    /// The store being backed up (its location).
    pub store: String,
    /// The backup directory or archive.
    pub backup: String,
    /// The problems, as [`Store::verify`](super::Store::verify) reports them (paths relative to
    /// the store, or to the backup for [`DamageSide::Backup`]).
    pub problems: Vec<super::VerifyProblem>,
    /// The store is open in this process with an intact graph in memory: a backup from memory
    /// ([`Store::backup_from_memory`](super::Store::backup_from_memory)) can save it.
    pub from_memory: bool,
}

impl std::fmt::Display for BackupDamage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let first = self.problems.first().map_or_else(String::new, |p| {
            format!(
                ": {}{}: {}",
                p.path,
                p.offset
                    .map(|o| format!(" at byte {o}"))
                    .unwrap_or_default(),
                p.reason
            )
        });
        let more = match self.problems.len() {
            0 | 1 => String::new(),
            n => format!(" (and {} more problem(s))", n - 1),
        };
        match self.side {
            DamageSide::Store => write!(
                f,
                "The backup stopped: the store {} is damaged{first}{more}; nothing of the \
                 backup into {} was kept",
                self.store, self.backup
            ),
            DamageSide::Backup => write!(
                f,
                "The backup stopped: the backup {} does not verify{first}{more}; nothing of \
                 this backup was kept, the store is not affected",
                self.backup
            ),
        }
    }
}

/// Why a Store operation was refused: the `cause` of [`PersistError::Store`].
///
/// A caller (a server, a front end) can `match` on it; [`StoreFailure::name`] is its stable
/// snake_case name (the Python exception's `cause` attribute).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum StoreFailure {
    /// The store was closed ([`Store::close`](super::Store::close) or
    /// [`Store::shutdown`](super::Store::shutdown)): it accepts no more operations.
    Closed,
    /// An internal lock of the open store is poisoned: a thread panicked while it held it.
    LockPoisoned,
    /// The name is already used: by another snapshot, or (a mark) by a mark whose history a
    /// prune removed (mark names stay unique along the store's whole history).
    NameTaken,
    /// A snapshot name is longer than 255 bytes.
    NameTooLong,
    /// A snapshot at the current commit already exists under another name: a checkpoint with
    /// a new name needs a commit after it.
    SnapshotExists,
    /// The store has no snapshot (at the commit asked for).
    NoSnapshot,
    /// A prune was asked while a backup copies the store's files.
    BackupRunning,
    /// A backup was asked while a prune deletes the store's files.
    PruneRunning,
    /// A rollback target at or after the current commit (or no commit after it in the WAL):
    /// there is nothing to roll back.
    NothingAfterTarget,
    /// No attic entry has that id (or the id is not an attic entry name).
    UnknownAtticEntry,
    /// The store's lineage changed since the attic entry's rollback (another rollback or
    /// restore came after it), so the entry cannot be restored or read as part of the chain.
    AtticLineageChanged,
    /// Commits or marks were made since the attic entry's rollback, so it cannot be restored.
    ChangedSinceRollback,
    /// The base snapshot an attic entry continues is missing from the store.
    AtticBaseMissing,
    /// The directory is a store, not a backup (a backup-only operation such as
    /// [`Store::prune_backup`](super::Store::prune_backup)).
    NotABackup,
    /// A snapshot a prune would keep fails verification: nothing was removed.
    KeptSnapshotDamaged,
    /// A copy just written (a converted or a repaired store) does not verify.
    CopyDoesNotVerify,
    /// A file of the store changed or vanished while the operation read it (another process
    /// or a person changed the store's files).
    ChangedConcurrently,
    /// Syncing the journal, or writing a checkpoint record into it, failed.
    JournalSyncFailed,
    /// The graph's storage does not claim a capability a Store needs to journal it
    /// ([`StorageCapabilities::atomic`](crate::graph::StorageCapabilities) and `change_capture`):
    /// without them a journal cannot honour a veto.
    MissingCapability,
    /// The files of an in-place rollback or attic restore were changed, but loading the new
    /// state into the open graph failed (an I/O error, damage, out of memory, the storage
    /// refusing the data). The graph in memory was emptied (never a partial state), the store
    /// is stopped ([`Store::stopped_reason`](super::Store::stopped_reason)) and writes nothing
    /// more; the files on disk are intact and the change completes (or is undone) at the next
    /// open.
    ReloadFailed,
    /// The store stopped after an in-place rollback or attic restore failed part-way
    /// ([`Store::stopped_reason`](super::Store::stopped_reason)): it writes nothing more until
    /// it is closed and opened again, which completes or undoes the change.
    Stopped,
    /// A store to upgrade ([`Store::upgrade`](super::Store::upgrade)) already has the current
    /// store format version: a copying upgrade has nothing to convert.
    AlreadyCurrent,
    /// A store to upgrade ([`Store::upgrade`](super::Store::upgrade),
    /// [`Store::upgrade_in_place`](super::Store::upgrade_in_place)) does not verify: nothing
    /// was written; repair it into a new current store instead.
    DamagedSource,
    /// The store format version of a store to upgrade in place
    /// ([`Store::upgrade_in_place`](super::Store::upgrade_in_place)) is upgraded only into a NEW
    /// copy (the upgrade table, format spec 15.1): nothing was written.
    UpgradeNeedsCopy,
    /// The store holds no embedding model of that identity or id (format spec 4.5).
    UnknownModel,
    /// An embedding model to add is not usable: a file name or role is missing or not
    /// allowed, a file is too large, the repository or licence is not a valid name.
    InvalidModel,
}

impl StoreFailure {
    /// The stable snake_case name of the cause (`"name_taken"`, `"backup_running"`, ...).
    pub fn name(self) -> &'static str {
        match self {
            StoreFailure::Closed => "closed",
            StoreFailure::LockPoisoned => "lock_poisoned",
            StoreFailure::NameTaken => "name_taken",
            StoreFailure::NameTooLong => "name_too_long",
            StoreFailure::SnapshotExists => "snapshot_exists",
            StoreFailure::NoSnapshot => "no_snapshot",
            StoreFailure::BackupRunning => "backup_running",
            StoreFailure::PruneRunning => "prune_running",
            StoreFailure::NothingAfterTarget => "nothing_after_target",
            StoreFailure::UnknownAtticEntry => "unknown_attic_entry",
            StoreFailure::AtticLineageChanged => "attic_lineage_changed",
            StoreFailure::ChangedSinceRollback => "changed_since_rollback",
            StoreFailure::AtticBaseMissing => "attic_base_missing",
            StoreFailure::NotABackup => "not_a_backup",
            StoreFailure::KeptSnapshotDamaged => "kept_snapshot_damaged",
            StoreFailure::CopyDoesNotVerify => "copy_does_not_verify",
            StoreFailure::ChangedConcurrently => "changed_concurrently",
            StoreFailure::JournalSyncFailed => "journal_sync_failed",
            StoreFailure::MissingCapability => "missing_capability",
            StoreFailure::ReloadFailed => "reload_failed",
            StoreFailure::Stopped => "stopped",
            StoreFailure::AlreadyCurrent => "already_current",
            StoreFailure::DamagedSource => "damaged_source",
            StoreFailure::UpgradeNeedsCopy => "upgrade_needs_copy",
            StoreFailure::UnknownModel => "unknown_model",
            StoreFailure::InvalidModel => "invalid_model",
        }
    }

    /// The advice for this cause: the `help()` of [`PersistError::Store`].
    fn advice(self) -> &'static str {
        match self {
            StoreFailure::Closed => {
                "The store was closed (Store::close or Store::shutdown) and accepts no more \
                 operations. Open it again (Store::open; CLI: graphersal --graph <dir>) and \
                 repeat the operation."
            }
            StoreFailure::LockPoisoned => {
                "A thread panicked while it held a lock of this open store, so its state in \
                 memory cannot be trusted. End the process (or drop the store) and open the \
                 store again: recovery continues from the last durable commit."
            }
            StoreFailure::NameTaken => {
                "Choose another name. Snapshot names are unique in the store (Store::snapshots; \
                 CLI: graphersal store list <dir>); mark names are unique along the store's \
                 whole history, also those of marks a prune made unreachable (Store::marks; \
                 CLI: graphersal store marks <dir>)."
            }
            StoreFailure::NameTooLong => "Use a snapshot name of at most 255 bytes (UTF-8).",
            StoreFailure::SnapshotExists => {
                "A commit has at most one snapshot, and the one at the current commit has \
                 another name. Commit something first, or checkpoint without a name (or with \
                 that snapshot's name), which returns the existing snapshot (CLI: graphersal \
                 store list <dir> shows it)."
            }
            StoreFailure::NoSnapshot => {
                "Name a commit that has a snapshot (Store::snapshots; CLI: graphersal store \
                 list <dir>), or write one first (Store::checkpoint; CLI: graphersal store \
                 checkpoint <dir>)."
            }
            StoreFailure::BackupRunning => {
                "A backup is copying the store's files, and a prune would delete some of them. \
                 Prune again when the backup has finished (Store::prune; CLI: graphersal store \
                 prune <dir> --up-to <commit>)."
            }
            StoreFailure::PruneRunning => {
                "A prune is deleting files of the store. Back up again when it has finished \
                 (Store::backup; CLI: graphersal store backup <dir> <backup_dir>)."
            }
            StoreFailure::NothingAfterTarget => {
                "A rollback goes back in time: choose a target before the current commit \
                 (Store::info, Store::marks; CLI: graphersal store info <dir>, graphersal store \
                 marks <dir>)."
            }
            StoreFailure::UnknownAtticEntry => {
                "Name an existing attic entry by its id (Store::attic_list; CLI: graphersal \
                 store attic <dir> lists them)."
            }
            StoreFailure::AtticLineageChanged => {
                "Another rollback or restore came after this entry's rollback, and attic \
                 entries restore newest first: restore the newer entry first, or fork this one \
                 into a new store (Store::attic_fork; CLI: graphersal store attic <dir> fork \
                 <id> <new_dir>)."
            }
            StoreFailure::ChangedSinceRollback => {
                "A restore would drop what was committed or marked since the rollback. Fork the \
                 entry into a new store instead (Store::attic_fork; CLI: graphersal store attic \
                 <dir> fork <id> <new_dir>)."
            }
            StoreFailure::AtticBaseMissing => {
                "The snapshot the attic entry continues is gone from the store (removed by hand, \
                 or damaged). Check the store (Store::verify; CLI: graphersal store verify \
                 <dir>); when the entry is needed, take it from a backup of the store, else \
                 remove it (Store::attic_remove; CLI: graphersal store attic <dir> remove <id>)."
            }
            StoreFailure::NotABackup => {
                "The directory is a live store, not a backup: open it as a store (Store::open; \
                 CLI: graphersal --graph <dir>) and prune it with Store::prune (CLI: graphersal \
                 store prune <dir> --up-to <commit>)."
            }
            StoreFailure::KeptSnapshotDamaged => {
                "A snapshot the prune would keep is damaged, so nothing was removed (the older \
                 snapshots are its donors). Check the store (Store::verify; CLI: graphersal \
                 store verify <dir>) and build a repaired copy in a new directory \
                 (Store::repair_dir; CLI: graphersal store repair <dir> --to <new_dir>)."
            }
            StoreFailure::CopyDoesNotVerify => {
                "The copy was written but did not read back intact: check the target disk \
                 (free space, I/O errors), remove the copy and repeat the operation into a new \
                 directory (graphersal store verify <new_dir> lists what failed)."
            }
            StoreFailure::ChangedConcurrently => {
                "A file of the store changed or vanished while the operation read it: never \
                 change a store's files by hand or from another program. Repeat the operation; \
                 when it fails again, check the store (Store::verify; CLI: graphersal store \
                 verify <dir>)."
            }
            StoreFailure::JournalSyncFailed => {
                "Writing or syncing the journal failed (a full disk, an I/O error), so the \
                 journal refuses further commits. Fix the cause, then reopen the store: \
                 recovery continues from the last durable commit."
            }
            StoreFailure::MissingCapability => {
                "A Store journals every commit in a commit hook that runs before the commit is \
                 final, so the storage must roll a unit back on a veto and build a change set \
                 per commit: it must claim StorageCapabilities::atomic and change_capture. Keep \
                 the graph in a storage that does (TraversalGraph does), or implement both."
            }
            StoreFailure::ReloadFailed => {
                "The graph in memory was emptied after the failed reload, so queries now read \
                 an empty graph, never a partial one; the store is stopped and writes nothing \
                 more. The files on disk are intact. Close the store and open it again (restart \
                 the server; CLI: run the command again): the open completes the rollback or \
                 restore (or undoes it) and loads the full graph. If the open fails too, check \
                 the disk and the store (graphersal store verify <dir>)."
            }
            StoreFailure::Stopped => {
                "An in-place rollback or attic restore failed part-way, so the store stopped: \
                 it writes nothing more (no commit, mark, checkpoint, prune or rollback). The \
                 files on disk are intact. Close the store and open it again (restart the \
                 server): the open completes the change (or undoes it) and the store is \
                 writable again."
            }
            StoreFailure::AlreadyCurrent => {
                "The store already has the current format: open it as it is (Store::open; CLI: \
                 graphersal --graph <dir>). For a copy of it use Store::convert (CLI: graphersal \
                 store convert <dir> <new>) or a backup (graphersal store backup <dir> \
                 <backup_dir>)."
            }
            StoreFailure::DamagedSource => {
                "Nothing was written: the upgrade only converts an intact store (the message \
                 lists the first problems the scrub found). Repair it instead: \
                 Store::repair_dir (CLI: graphersal store repair <dir> --to <new_dir>) reads the \
                 older store and writes a NEW store of the current format with what is intact (it reports what was \
                 repaired, lost and diverged; the damaged store is never changed). Or restore a \
                 backup and upgrade that (graphersal store upgrade <backup_dir> <new_dir>)."
            }
            StoreFailure::UpgradeNeedsCopy => {
                "Nothing was written: an upgrade from this store format version changes more \
                 than the version word, so it is never done in place. Upgrade it into a NEW \
                 store (the older one stays as it is): Store::upgrade (CLI: graphersal store \
                 upgrade <dir> <new_dir>; Python graphersal.Store.upgrade(source, target)). The \
                 supported upgrades are listed in the format versions table of the book \
                 (persistence/store_versions.md)."
            }
            StoreFailure::UnknownModel => {
                "List the models the store holds and their identities (Store::models; CLI: \
                 graphersal store models <dir>) and name one of them, by its identity \
                 (model2vec/<repo>@<12 hex>) or its id (model2vec-<12 hex>). A model enters a \
                 store only by an explicit add (CLI: graphersal embed-model download <repo> \
                 --into <dir>, or embed-model import <model_dir> --into <dir>)."
            }
            StoreFailure::InvalidModel => {
                "Nothing was added. A Model2Vec model is one config, one tokenizer and one \
                 weights file (config.json, tokenizer.json, model.safetensors), plus its card \
                 (README.md) and licence files; file names are 1 to 40 ASCII letters, digits, \
                 '.', '_' and '-'. Check the model directory (it loads with `graphersal \
                 --embedder name=<model_dir>`) and add it again (CLI: graphersal embed-model \
                 import <model_dir> --into <dir>)."
            }
        }
    }
}

impl std::fmt::Display for StoreFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

impl PersistError {
    pub(crate) fn io(context: &'static str, source: std::io::Error) -> Self {
        PersistError::Io { context, source }
    }

    pub(crate) fn corrupt(
        location: impl Into<String>,
        offset: u64,
        reason: impl Into<String>,
    ) -> Self {
        PersistError::Corrupt {
            location: location.into(),
            offset,
            reason: reason.into(),
        }
    }

    pub(crate) fn unsupported_content(
        location: impl Into<String>,
        offset: u64,
        reason: impl Into<String>,
    ) -> Self {
        PersistError::UnsupportedContent {
            location: location.into(),
            offset,
            reason: reason.into(),
        }
    }

    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) fn store(
        operation: &'static str,
        cause: StoreFailure,
        reason: impl Into<String>,
    ) -> Self {
        PersistError::Store {
            operation,
            cause,
            reason: reason.into(),
        }
    }

    pub(crate) fn encode(what: impl Into<String>, reason: impl Into<String>) -> Self {
        PersistError::Encode {
            what: what.into(),
            reason: reason.into(),
        }
    }
}

impl DiagnosticError for PersistError {
    fn help(&self) -> Option<String> {
        Some(match self {
            PersistError::Io { .. } => "Check that the file or stream is readable and writable, \
                 and that the disk is not full; then repeat the operation."
                .to_string(),
            PersistError::Corrupt { .. } => "The bytes were damaged or are not a Graphersal \
                 snapshot or journal. Load an older copy (another snapshot, a backup); a damaged \
                 journal record is never skipped silently, because the commits after it depend \
                 on it. A Store with damage opens read-only in maintenance mode; build a repaired \
                 copy with Store::repair_to (CLI: graphersal store repair <dir> --to <new_dir>)."
                .to_string(),
            PersistError::UnsupportedVersion { .. } => "The store or file was written by a \
                 newer Graphersal; open it with that version (or a later one). It is not \
                 damaged: this build refuses it without reading further or changing anything, \
                 and it is never opened in maintenance mode, repaired or upgraded here (a \
                 repair by this build would lose what it cannot read)."
                .to_string(),
            PersistError::UnsupportedContent { .. } => "The store or file holds content a \
                 newer Graphersal wrote (its checksum is valid, so the bytes are intact); open \
                 it with that version (or a later one). It is not damaged: this build changes \
                 nothing, and it never repairs it (a repair by this build would drop what it \
                 cannot read). Do not run graphersal store repair on it with this version."
                .to_string(),
            PersistError::MemoryBudget { .. } => "Raise the memory limit of the load \
                 (ReadOptions::with_memory_limit, Python memory_limit=...) or load the graph on \
                 a machine with more memory."
                .to_string(),
            PersistError::LineageMismatch { .. } => "Recover each graph from its own snapshot \
                 and journals: a journal is replayed only onto a snapshot of the same graph \
                 lineage (graph_id)."
                .to_string(),
            PersistError::Gap { .. } => "Pass every journal written since the snapshot, in \
                 order; when a journal file is lost, recover to a target before the gap \
                 (RecoveryTarget::CommitSeq)."
                .to_string(),
            PersistError::TargetNotReached { .. } => "Choose a target between the snapshot's \
                 position and the end of the journals (RecoveryReport lists the marks and the \
                 last commit), or pass an older snapshot for an earlier target."
                .to_string(),
            PersistError::Encode { .. } => "Only storable property values (null, booleans, \
                 numbers, strings, uuids, arrays and string-keyed objects) can be persisted."
                .to_string(),
            PersistError::CommitTooLarge { .. } => "The journal writes every commit as one \
                 record of at most 1 GiB before compression, and a unit's record holds all its \
                 changes, dropped elements with their full properties included. The whole unit \
                 was rolled back and nothing was written; the journal keeps working. Split the \
                 work into several smaller units: in the web playground, which runs a whole \
                 script as one unit, drop the old data in a run of its own (for example \
                 g.v().drop().iterate()) and create the new data over several runs (for example \
                 a part of the input per run); in the CLI, Python and Rust each traversal is \
                 already its own unit, so split a single huge traversal (drop or load in \
                 batches), and in Rust keep transaction(..) blocks and whole-script units \
                 (script::eval_value_atomic) below the limit."
                .to_string(),
            PersistError::Poisoned { .. } => "Recover the graph from its snapshot and journal \
                 (persist::recover) and attach a new journal; the poisoned one accepts no more \
                 commits. A Store: reopen it (the refused record was cut off, or its position \
                 recorded, so it is never replayed); check the disk first."
                .to_string(),
            PersistError::Locked { .. } => "Close the other writer first (stop the server or \
                 the program that has the store open). To only read the data while it runs, use \
                 Store::open_read_only (CLI: graphersal store info / list / backup work on a \
                 store in use)."
                .to_string(),
            PersistError::NotAStore { .. } => "Create a store with Store::create (CLI: \
                 graphersal store create <dir>, or --graph <dir> --create-store), or name the \
                 directory of an existing store (it holds a GRAPH file)."
                .to_string(),
            PersistError::DirectoryNotEmpty { path } => format!(
                "Choose an empty or new directory for the new store, or open the existing store \
                 if it is one (Store::open; CLI: graphersal store info {path} shows what it \
                 holds, graphersal --graph {path} opens it)."
            ),
            PersistError::OlderFormat { path, found, .. }
                if super::store::upgradable_from(*found) =>
            {
                format!(
                    "Upgrade it with this build; no older Graphersal is needed. Into a new store \
                     (the older one stays as it is): graphersal store upgrade {path} <new_dir> \
                     (a <new_file>.gstore path for one file); or in place (back it up first: \
                     afterwards the older Graphersal no longer opens it): graphersal store \
                     upgrade {path} --in-place. In Rust Store::upgrade / Store::upgrade_in_place, \
                     in Python graphersal.Store.upgrade. Everything is carried: the graph, schema \
                     and catalog, the history, marks, attic entries and the store's identity; a \
                     backup directory stays a backup. The store is verified first; a damaged one \
                     is refused (graphersal store repair {path} --to <new_dir> writes a repaired \
                     store of the current format)."
                )
            }
            PersistError::OlderFormat { path, .. } => format!(
                "This is a pre-release store format the converter does not read. Migrate it \
                 through a packed snapshot. With the Graphersal that wrote the store \
                 (the older binary; the store closed), export its current state: graphersal \
                 --graph {path} -e 'g.export_snapshot(\"graph.gsnap\")'. Then create a new store \
                 from it with this build: graphersal store create <new_dir> --from graph.gsnap \
                 (a <new_file>.gstore path or --single-file for one file). The new store has the \
                 graph, its schema and its catalog; marks, attic entries and backups are not \
                 carried, so keep the old store until the new one is verified and backed up."
            ),
            PersistError::Store { cause, .. } => cause.advice().to_string(),
            PersistError::Maintenance { path, .. } => format!(
                "Read what is intact (the data stays queryable). The damage report lists the \
                 damaged files, chunks and records and their donors (Store::maintenance; CLI: \
                 graphersal store verify {path}). Build a repaired copy in ANOTHER directory \
                 with Store::repair_to (CLI: graphersal store repair {path} --to <new_dir>): it \
                 reports what was repaired, lost and diverged; the damaged files are never \
                 changed. Then use the repaired store, or restore a backup."
            ),
            PersistError::ReadOnly { path, .. } => format!(
                "The store was written by a newer Graphersal that maintains something this \
                 version does not know; a write from this version would leave it inconsistent. \
                 Read the data here (queries work), and open the store read-write with that \
                 newer version (graphersal store info {path} shows the store's position). A \
                 backup copies it byte for byte; a fork keeps a critical definition and stays \
                 read-only for this version (a graph schema of a newer version cannot be forked \
                 here: this version would drop it). Never run graphersal store repair on it \
                 with this version."
            ),
            PersistError::IsBackup { path, .. } => format!(
                "Read it read-only: Store::open_backup or Store::open_read_only (CLI: graphersal \
                 --graph {path} opens it read-only; store info / list / verify / export work on \
                 it). When the original store is lost and you continue from the backup, make it \
                 the live store with the same graph id: graphersal store restore {path} \
                 (Store::restore_backup). For an independent writable copy: graphersal store \
                 fork {path} <new_dir> (Store::fork_dir)."
            ),
            PersistError::BackupRefused { .. } => "The backup directory is left as it was (a \
                 refused or interrupted increment is rolled back; the backup stays readable). \
                 Back up into a new, empty directory for a full backup (CLI: graphersal store \
                 backup <dir> <new_dir> --full; BackupMode::Full), or name the backup directory \
                 of this store: a fork, repair or copy is a different store and never continues \
                 the original's backups, and after a rollback behind the backup the old backup \
                 stays as the archive of the abandoned history. An increment needs the store's WAL since the backup's position: \
                 back up more often than you prune (store prune --up-to)."
                .to_string(),
            PersistError::BackupDamaged(damage) => match damage.side {
                DamageSide::Store => {
                    let memory = if damage.from_memory {
                        "The store is open in this process and its graph in memory is intact \
                         (it was verified when it was loaded and changed only by commits): \
                         save it first with a backup from memory into a new directory \
                         (Store::backup_from_memory; the dev server's Store menu, Back up from \
                         memory; Python store.backup(path, from_memory=True)), which writes \
                         nothing to the damaged store. "
                    } else {
                        ""
                    };
                    format!(
                        "{memory}Repair the MAIN store, never the backup: graphersal store \
                         repair {store} --to <new_dir> (Store::repair_dir) builds a verified, \
                         repaired copy in a new directory and reports what was repaired, lost \
                         and diverged; the damaged files are never changed. The full damage \
                         report: graphersal store verify {store}. Existing backups stay valid: \
                         keep them until the repaired store is backed up.",
                        store = damage.store
                    )
                }
                DamageSide::Backup => format!(
                    "The store verified; the copy written to {backup} did not read back intact \
                     (or the backup an increment continues is damaged: graphersal store verify \
                     {backup}). Check the backup's disk, then back up again, into a new, empty \
                     directory when the old backup is damaged (CLI: graphersal store backup \
                     <dir> <new_dir> --full; BackupMode::Full).",
                    backup = damage.backup
                ),
            },
            PersistError::DamageFound { path, .. } => format!(
                "The data in memory is intact and stays queryable. Save it first with a backup \
                 from memory into a new directory (Store::backup_from_memory; the dev server's \
                 Store menu, Back up from memory; Python store.backup(path, from_memory=True)): \
                 it writes nothing to the damaged store. Then build a repaired copy of the store \
                 in another directory: graphersal store repair {path} --to <new_dir> \
                 (Store::repair_dir), and continue with it. A store created with --on-damage \
                 continue keeps committing instead (the policy is fixed when a store is \
                 created)."
            ),
            PersistError::Graph(err) => return err.help(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn help_of(cause: StoreFailure) -> String {
        PersistError::store("do it", cause, "because")
            .help()
            .unwrap_or_default()
    }

    /// The causes that need a fault inside the process (the others are triggered through the
    /// public API in `tests/all/store_error_tests.rs`).
    #[test]
    fn faults_inside_the_process_have_their_own_help() {
        assert!(help_of(StoreFailure::LockPoisoned).contains("A thread panicked"));
        assert!(help_of(StoreFailure::JournalSyncFailed).contains("reopen the store"));
        assert!(help_of(StoreFailure::ChangedConcurrently).contains("never change a store's"));
        assert!(help_of(StoreFailure::CopyDoesNotVerify).contains("check the target disk"));
        // The built-in backends wait for a running prune instead of refusing the backup.
        assert!(help_of(StoreFailure::PruneRunning).contains("Back up again"));
        // A failed in-place reload (store/tests.rs drives it with a failing read).
        assert!(help_of(StoreFailure::ReloadFailed).contains("emptied"));
        assert!(help_of(StoreFailure::Stopped).contains("Close the store and open it again"));
    }

    /// A commit over the journal's per-commit limit names the limit and the fix, also wrapped
    /// in the veto the journal's hook turns it into (a 1 GiB commit cannot be built cheaply in
    /// a test, so the error is built by hand).
    #[test]
    fn a_too_large_commit_says_how_to_split_the_work() {
        let limit = crate::persist::codec::MAX_BLOCK_BYTES;
        let err = PersistError::CommitTooLarge {
            commit_seq: 2,
            bytes: limit + 1,
            limit,
        };
        assert_eq!(
            err.to_string(),
            "Commit 2 is too large for the journal: its uncompressed record body is 1073741825 \
             bytes, more than the limit of 1073741824 bytes per commit"
        );
        let help = err.help().unwrap_or_default();
        assert!(help.contains("at most 1 GiB"), "{help}");
        assert!(
            help.contains("drop the old data in a run of its own"),
            "{help}"
        );
        assert!(
            help.contains("each traversal is already its own unit"),
            "{help}"
        );
        let vetoed = crate::error::GraphError::CommitRejected {
            hook: "journal".to_string(),
            source: Box::new(err),
        };
        let help = vetoed.help().unwrap_or_default();
        assert!(
            help.starts_with("The commit hook 'journal' refused the commit"),
            "{help}"
        );
        assert!(
            help.contains("Split the work into several smaller units"),
            "{help}"
        );
    }

    #[test]
    fn every_cause_has_its_own_help_and_name() {
        let causes = [
            StoreFailure::Closed,
            StoreFailure::LockPoisoned,
            StoreFailure::NameTaken,
            StoreFailure::NameTooLong,
            StoreFailure::SnapshotExists,
            StoreFailure::NoSnapshot,
            StoreFailure::BackupRunning,
            StoreFailure::PruneRunning,
            StoreFailure::NothingAfterTarget,
            StoreFailure::UnknownAtticEntry,
            StoreFailure::AtticLineageChanged,
            StoreFailure::ChangedSinceRollback,
            StoreFailure::AtticBaseMissing,
            StoreFailure::NotABackup,
            StoreFailure::KeptSnapshotDamaged,
            StoreFailure::CopyDoesNotVerify,
            StoreFailure::ChangedConcurrently,
            StoreFailure::JournalSyncFailed,
            StoreFailure::MissingCapability,
            StoreFailure::ReloadFailed,
            StoreFailure::Stopped,
        ];
        let helps: std::collections::HashSet<_> = causes.iter().map(|c| help_of(*c)).collect();
        let names: std::collections::HashSet<_> = causes.iter().map(|c| c.name()).collect();
        assert_eq!(helps.len(), causes.len());
        assert_eq!(names.len(), causes.len());
        let err = PersistError::store("prune the store", StoreFailure::BackupRunning, "busy");
        assert_eq!(err.to_string(), "Cannot prune the store: busy");
    }
}
