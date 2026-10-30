//! The system journal (book page "System Journal"): a durable, append-only stream of declared
//! operational events (store opens and closes, checkpoints, backups, prunes, damage, recovery,
//! rollbacks, permission denials, clock steps) that a person or a host's logging reads later.
//!
//! For hosts and operators. Every graph has one
//! ([`Graph::system_journal`](crate::prelude::Graph::system_journal)): a Store keeps it in its
//! `journal/` directory (`Store::journal`, `Store::journal_dir` for a closed store, the CLI's
//! `graphersal store journal`), a graph without a store in memory (the newest
//! [`MEMORY_CAPACITY`] records). Records are written by the engine only, each of a declared
//! kind; read them with [`SystemJournal::query`] (filters and a cursor), follow them with
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

pub use crate::graph::system_journal::{
    DEFAULT_QUERY_LIMIT, InstanceId, JournalError, JournalPage, JournalQuery, JournalRecord,
    JournalSink, JournalSubscription, Level, MAX_QUERY_LIMIT, MEMORY_CAPACITY, SinkId,
    SystemJournal,
};
