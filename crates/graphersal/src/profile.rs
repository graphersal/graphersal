//! Profiling: the metrics that `.profile()`, `.profile_with(..)` and `execute()` return.
//!
//! For query writers tuning a traversal and for hosts that display or ship profiles
//! ([`TraversalMetrics`] is pure data; its `Display` is the text table). Guide: the book page
//! `users_guide/profiling_and_execute.md`.
//!
//! The shape follows TinkerPop's `TraversalMetrics`/`Metrics`, extended with our own data, with
//! snake_case names everywhere (Rust getters, Rhai map keys, JSON keys):
//!
//! | TinkerPop       | Graphersal                 |
//! |-----------------|----------------------------|
//! | `dur`           | `duration` (`duration_ns` in Rhai/JSON) |
//! | `traverserCount`| `counts.traverser_count`   |
//! | `elementCount`  | `counts.element_count`     |
//! | `percentDur`    | `percent_duration`         |
//! | nested metrics  | `nested`                   |
//!
//! The whole run's memory is [`TraversalMetrics::memory`] ([`QueryMemory`]): the peak of the
//! live bytes the run held at once against its per-execution limit
//! `traversal.max_materialized_bytes`, on every profile (`query_memory` key in the map and the JSON);
//! the text table shows it as one line under `TOTAL:` only with `ProfileType::Memory`.
//!
//! # Extensibility rule
//!
//! These types are pure data that callers may store, compare and serialize, so they may only
//! grow in one way: **a new [`ProfileType`] adds a NEW optional section to
//! [`Metrics`]** (like [`Metrics::memory`], present only with `ProfileType::Memory`): an
//! `Option<..>` getter in Rust, a new key in the Rhai map and the JSON object, absent unless that
//! type was requested. The meaning and the type of an existing field never change. The structs
//! are `#[non_exhaustive]` and read through getters; consumers of the map/JSON form must ignore
//! keys they do not know.
//!
//! No rendered text lives inside the data: the text table is produced by the
//! [`Display`](std::fmt::Display) impl of [`TraversalMetrics`] (the session visualizer of the
//! script DSL uses it), the map/JSON forms by [`TraversalMetrics::to_property`] and
//! [`TraversalMetrics::to_json`].

pub use crate::traversal::{
    LoopMetrics, MemoryMetrics, MemorySource, Metrics, ProfileType, QueryMemory, StepCounts,
    TimingMetrics, TraversalMetrics,
};
