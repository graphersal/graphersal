//! Persistence over streams (feature `persist`): the binary storage format specified in the
//! book's "Storage Format Specification" (`book/src/persistence/format.md`; "format spec N" in
//! the comments of this module names its section N). User documentation: the book's
//! "Persistence" part (`book/src/persistence/`).
//!
//! - **Packed snapshot**: the whole graph (data, schema, lineage id, commit position, commit
//!   time, auto-id sequences) in one file or stream: [`write_snapshot`] /
//!   [`TraversalGraph::write_snapshot`](crate::graph::TraversalGraph::write_snapshot) and
//!   [`read_snapshot`] / [`TraversalGraph::read_snapshot`](crate::graph::TraversalGraph::read_snapshot).
//!   Both work for any [`PersistentStorage`](crate::storage::PersistentStorage):
//!   [`write_snapshot`] takes `&S`, [`read_snapshot_into`] loads into an empty `S`.
//!   Values are written losslessly (every logical type, NaN payloads, property order,
//!   multi-label vertices, unlabeled elements); loading does not validate against the schema
//!   (the data was valid when it was committed), every byte is protected by CRC-32C.
//! - **Journal**: [`Journal`], the commit hook that writes one write-ahead record per committed
//!   unit to any `Write` before the commit becomes final (a failed write vetoes it). It always
//!   runs after every other hook, so the journal never holds a unit that was vetoed. Marks
//!   ([`GraphStorage::mark`](crate::graph::GraphStorage::mark)) are journal records.
//! - **Recovery**: [`recover`] / [`Recovery`]: a snapshot plus the journals written after it,
//!   replayed up to the end, a commit sequence number, a time or a mark.
//!
//! ```rust
//! use graphersal::prelude::*;
//! use graphersal::persist::{self, Durability, Journal, RecoveryTarget, SharedBuffer};
//!
//! // A graph, its base snapshot and a journal from then on.
//! let mut graph = TraversalGraph::tinkerpop_modern();
//! let mut snapshot = Vec::new();
//! graph.write_snapshot(&mut snapshot).unwrap();
//! let wal = SharedBuffer::new();
//! Journal::new(wal.clone(), Durability::EveryCommit).attach(&mut graph).unwrap();
//!
//! graph.traversal_mut().add_v("person").property("name", "ann").to_list().unwrap();
//! graph.mark("after-ann").unwrap();
//! graph.traversal_mut().v("1").drop().to_list().unwrap();
//!
//! // Recovery to the mark: ann is there, vertex 1 is not dropped yet.
//! let recovered = persist::recover(
//!     snapshot.as_slice(),
//!     [wal.contents().as_slice()],
//!     RecoveryTarget::Mark("after-ann".into()),
//! )
//! .unwrap();
//! assert_eq!(recovered.graph.vertex_count(), 7);
//! ```

mod codec;
mod definitions;
mod dir;
mod encoder;
mod error;
mod journal;
mod manifest;
mod recover;
mod segment;
#[cfg(not(target_arch = "wasm32"))]
mod single_file;
mod snapshot;
mod store;
mod wal;

pub use codec::Codec;
pub(crate) use definitions::{decode_payload, encode_payload};
pub use dir::{
    DirCheck, DirCompaction, DirEntry, DirLockGuard, EntryKind, IntoStoreDir, LockKind, MemDir,
    SpaceUsage, StoreDir, StoreFile,
};
#[cfg(not(target_arch = "wasm32"))]
pub use dir::{FsDir, store_dir_at};
pub use error::{BackupDamage, DamageSide, PersistError, StoreFailure};
pub use journal::{Durability, DurableEnd, DurablePosition, Journal, SharedBuffer};
pub use recover::{Recovered, Recovery, RecoveryReport, RecoveryTarget, recover};
pub use segment::{DEFAULT_CHUNK_BYTES, DEFAULT_SEGMENT_BYTES};
#[cfg(not(target_arch = "wasm32"))]
pub use single_file::{SINGLE_FILE_EXTENSION, SingleFileDir};
pub use snapshot::{
    ReadOptions, SnapshotInfo, WriteOptions, read_snapshot, read_snapshot_info, read_snapshot_into,
    write_snapshot,
};
pub use wal::{JournalReader, JournalRecord, TornTail, read_journal};

pub use store::{
    AddModelReport, AdviceRule, AtticEntry, BackupMarker, BackupMode, BackupReport, BaseModel,
    ChangesSince, CheckpointReport, CompactReport, CompactionAdvice, CompactionPolicy, CutTail,
    DEFAULT_CHECKPOINT_AFTER_BYTES, DEFAULT_CHECKPOINT_MERGE_BYTES, DEFAULT_STORE_SEGMENT_BYTES,
    DEFAULT_WAL_SEGMENT_BYTES, Damage, DamageKind, DamagePolicy, DamageReport, Diverged, Donor,
    ForkReport, FoundDamage, IdRange, LineageEntry, ModelCopy, ModelFile, ModelRole, NewModel,
    OpenReport, PruneReport, RepairReport, RollbackMode, RollbackReport, Store, StoreInfo,
    StoreOptions, StoredModel, UpgradeReport, VerifyProblem, VerifyReport,
};

#[cfg(all(feature = "model2vec", not(target_arch = "wasm32")))]
pub use store::StoredEmbedders;

/// The file extension of packed snapshots written by the bindings (`graph.gsnap`).
pub const SNAPSHOT_EXTENSION: &str = "gsnap";

/// The version of the file formats this build writes: packed snapshots, snapshot manifests and
/// segments, WAL segments (format spec 15). This build also reads version 1 (Graphersal 0.1.x),
/// which differs only by the temporal value tags it does not have, so a `.gsnap` written by
/// 0.1.x loads here; a file of this version is not readable by a build of version 1.
pub const FORMAT_VERSION: u32 = codec::FORMAT_VERSION;

/// The version of the Store format this build writes and reads: the version word of a store's
/// `GRAPH` file (format spec 5.1, 15). A store of an older version is refused with
/// [`PersistError::OlderFormat`]; one of a released older version (2, Graphersal 0.1.x) is
/// converted by [`Store::upgrade`] / [`Store::upgrade_in_place`] without the older build.
pub const STORE_FORMAT_VERSION: u32 = store::STORE_VERSION;
