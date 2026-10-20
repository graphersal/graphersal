//! Harness for running the vendored openCypher TCK against Graphersal's GQL.
//!
//! Each submodule owns one concern; the baseline and the report join as the harness is built. The generic parts are copied from the TinkerPop harness
//! and adapted, never shared with it; the one exception is the storage selection, included
//! unchanged by path (P0: `tinkerpop/harness/storage.rs` is generic).

// Model fields and helpers are consumed by the baseline and the report, which are added later; silence dead-code warnings until they are wired in.
#![allow(dead_code)]

pub mod classify;
pub mod compare;
pub mod execute;
pub mod gherkin;
pub mod state;
#[path = "../../tinkerpop/harness/storage.rs"]
pub mod storage;
pub mod values;
