//! Workload statistics (book page "Workload Statistics"): what the queries of an instance did,
//! in time buckets (minutes, hours, days) per series, kept in memory and persisted best effort
//! by a Store.
//!
//! For hosts and operators: a Store keeps one [`WorkloadStats`] (`Store::workload_stats`), at
//! the [`WorkloadLevel`] set when it was opened.

pub use crate::graph::workload::{
    AccessKind, Channel, Counters, JobRunRecord, MAX_QUERY_SERIES, PropertyUse, Sample, SeriesKey,
    SeriesKind, SeriesTotals, WorkloadLevel, WorkloadReport, WorkloadStats, scope,
};
