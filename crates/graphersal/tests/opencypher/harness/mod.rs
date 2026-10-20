//! Harness for running the vendored openCypher TCK against Graphersal's GQL.
//!
//! Each submodule owns one concern; the value notation, the runner, the baseline and the report
//! join as the harness is built. The generic parts are copied from the TinkerPop harness and
//! adapted, never shared with it.

// Model fields and helpers are consumed by the runner and the comparison, which are added later;
// silence dead-code warnings until they are wired in.
#![allow(dead_code)]

pub mod gherkin;
