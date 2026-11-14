//! The system journal (book page "System Journal"): a durable, append-only stream of declared
//! operational events (store opens and closes, checkpoints, backups, prunes, damage, recovery,
//! rollbacks, permission denials, clock steps) that a person or a host's logging reads later.
//!
//! For hosts and operators. Every graph has one
//! ([`Graph::system_journal`](crate::prelude::Graph::system_journal)): a Store keeps it in its
//! `journal/` directory (`Store::journal`, `Store::journal_dir` for a closed store, the CLI's
//! `graphersal store journal`), a graph without a store in memory (the newest
//! [`MEMORY_CAPACITY`] records). Every record is of a declared kind: the engine's own (written
//! by the engine only) or a host's, declared once per process with [`declare`] and recorded
//! with [`SystemJournal::record`] (book "System Journal", section "Host events"); read them with [`SystemJournal::query`] (filters and a cursor), follow them with
//! [`SystemJournal::subscribe`], forward them with a [`JournalSink`]. Every record is mirrored to
//! `tracing` under the target `graphersal::journal`. The journal decides nothing: a failed
//! journal write never fails an operation ([`SystemJournal::lost_records`] counts it).
//!
//! ```rust
//! use graphersal::prelude::*;
//! use graphersal::system_journal::{JournalQuery, Level};
//!
//! let graph = Graph::new(TraversalGraph::new());
//! let journal = graph.system_journal().unwrap();
//! let page = journal.query(&JournalQuery::new().min_level(Level::Warn)).unwrap();
//! assert!(page.records.is_empty());
//! ```
//!
//! A host's own event, declared once per process, then recorded:
//!
//! ```rust
//! use graphersal::prelude::*;
//! use graphersal::system_journal::{HostEvent, HostEventKind, JournalQuery, Level, declare};
//!
//! declare(
//!     HostEventKind::new(
//!         "myhost.job_failed",
//!         "myhost.jobs",
//!         Level::Error,
//!         "A background job of the host failed.",
//!     )
//!     .field("job", "the job's name")
//!     .optional_field("cause", "the error"),
//! )
//! .unwrap();
//!
//! let graph = Graph::new(TraversalGraph::new());
//! let journal = graph.system_journal().unwrap();
//! journal
//!     .record(
//!         HostEvent::new("myhost.job_failed", "the job nightly failed: timeout")
//!             .field("job", "nightly")
//!             .field("cause", "timeout"),
//!     )
//!     .unwrap();
//! let page = journal.query(&JournalQuery::new().source_prefix("myhost")).unwrap();
//! assert_eq!(page.records[0].kind, "myhost.job_failed");
//! ```

pub use crate::graph::system_journal::host::{HostEvent, HostEventKind, declare, declared_kinds};
pub use crate::graph::system_journal::{
    DEFAULT_QUERY_LIMIT, InstanceId, JournalError, JournalPage, JournalQuery, JournalRecord,
    JournalSink, JournalSubscription, Level, MAX_QUERY_LIMIT, MEMORY_CAPACITY, SinkId,
    SystemJournal,
};
