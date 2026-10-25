//! # `graphersal` - Graphersal In-Memory Graph Traversal Engine
//!
//! An in-memory graph traversal engine inspired by Apache TinkerPop Gremlin.
//!
//! ## Quick Start
//! ```rust
//! use graphersal::prelude::*;
//!
//! let graph = TraversalGraph::new();
//! let g = graph.traversal();
//! let count = g.v(None::<()>).count().next().unwrap();
//! assert!(count.is_some());
//! ```

// Rule 11: every public item has a `///` doc comment (denied in CI through `-D warnings`).
#![warn(missing_docs)]

use core::fmt;

#[cfg(feature = "alloc-tracking")]
pub mod alloc_tracking;
pub mod auth;
pub mod catalog;
pub mod changes;
/// Error types and their diagnostic (`help()`) text.
pub mod error;
pub mod exec;
// The GQL query language: internal; its entry points are `GraphTraversalSource::gql` and
// `gql_with`.
#[cfg(feature = "gql")]
pub(crate) mod gql;
// Graph storage, schema and indexes: internal; the public items are re-exported by the topic
// modules (`storage`, `schema`, `changes`, `auth`, `catalog`, `exec`).
pub(crate) mod graph;
#[cfg(feature = "io")]
pub mod io;
pub mod jobs;
#[cfg(feature = "persist")]
pub mod persist;
pub mod prelude;
pub mod profile;
pub mod schema;
/// The Rhai script DSL.
#[cfg(feature = "script")]
pub mod script;
pub mod storage;
pub mod temporal;
// The traversal pipeline (source, steps, optimizer, execution) and the property values:
// internal; the public items are re-exported by `prelude`, `profile` and `exec`.
pub(crate) mod traversal;
pub(crate) mod value;
pub mod vector;
pub mod workload;

// The domain types at the crate root (rule 8). No glob re-exports: query writers use
// `prelude`, integrators the topic modules.
/// The small-string type of labels, property keys and string values, re-exported from
/// `compact_str` (the only third-party type of the public API); see [`prelude::CompactString`].
pub use compact_str::CompactString;
pub use error::TraverserError;
pub use graph::TraversalGraph;
pub use traversal::{GraphSource, GraphTraversalSource};
pub use value::{ElementProperty, TraverserValue};

// Crate-internal shortcuts: the code inside `graphersal` writes `use crate::*;` to reach the items
// of the internal modules. These globs are `pub(crate)` and never part of the public API.
#[allow(unused_imports)]
pub(crate) use error::*;
#[allow(unused_imports)]
pub(crate) use graph::*;
#[allow(unused_imports)]
pub(crate) use prelude::*;
#[allow(unused_imports)]
pub(crate) use traversal::*;
#[allow(unused_imports)]
pub(crate) use value::*;

/// Comma-separated Rhai string literals (`"a", "b"`), the arguments of a rendered step.
pub(crate) fn display_vec_string(arg: &[String]) -> String {
    crate::value::literal::rhai_strings(arg)
}

pub(crate) fn display_option_vec_string(arg: &Option<Vec<String>>) -> String {
    match arg {
        Some(arg) => display_vec_string(arg),
        None => String::new(),
    }
}
