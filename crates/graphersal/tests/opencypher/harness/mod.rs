//! Harness for running the vendored openCypher TCK against Graphersal's GQL.
//!
//! Each submodule owns one concern. The generic parts are copied from the TinkerPop harness and
//! adapted, never shared with it; the one exception is the storage selection, included unchanged
//! by path (P0: `tinkerpop/harness/storage.rs` is generic).

// Some model fields and helpers are only read by the harness's own unit tests.
#![allow(dead_code)]

pub mod baseline;
pub mod classify;
pub mod compare;
pub mod execute;
pub mod gherkin;
pub mod page;
pub mod procedure;
pub mod report;
pub mod scope;
pub mod state;
#[path = "../../tinkerpop/harness/storage.rs"]
pub mod storage;
pub mod values;
