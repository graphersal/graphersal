//! Which storage the suite runs on (`book/src/for_developers/custom_storage.md`).
//!
//! `GRAPHERSAL_TEST_STORAGE` selects it once per process:
//! - unset, empty or `traversal-graph`: every dataset graph is a `Graph<TraversalGraph>` (the
//!   default run, which owns the baseline, the report and the compliance page);
//! - `forwarding`: every dataset graph is a `Graph<Forwarding>` (the proof storage of
//!   `graphersal-storage-tests`, included by `#[path]` in `tinkerpop_features.rs`), which
//!   forwards every `GraphStorage` method to a `TraversalGraph` but has its own handle types.
//!   The run is gated against the SAME baseline and compliance page and must classify every
//!   scenario like the default run; it never rewrites either of them and writes its report to
//!   `target/tinkerpop-report-forwarding.md`.
//!
//! Any other value fails the run with a message. The harness holds every graph as
//! `Arc<dyn AnyGraph>` in both modes, so the code path above the storage is the same.

use std::sync::{Arc, OnceLock};

use graphersal::TraversalGraph;
use graphersal::storage::{AnyGraph, Graph, IntoAnyGraph};

use crate::forwarding::Forwarding;

/// The environment variable that selects the storage.
pub const STORAGE_ENV: &str = "GRAPHERSAL_TEST_STORAGE";

/// The storage every dataset graph of this process is built on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestStorage {
    /// `Graph<TraversalGraph>` (the default).
    TraversalGraph,
    /// `Graph<Forwarding>` (`GRAPHERSAL_TEST_STORAGE=forwarding`).
    Forwarding,
}

impl TestStorage {
    /// The value of [`STORAGE_ENV`] that selects this storage.
    pub fn name(self) -> &'static str {
        match self {
            Self::TraversalGraph => "traversal-graph",
            Self::Forwarding => "forwarding",
        }
    }

    /// Parses a value of [`STORAGE_ENV`] (`None`: unset).
    pub fn parse(value: Option<&str>) -> Result<Self, String> {
        match value.map(str::trim) {
            None | Some("") | Some("traversal-graph") => Ok(Self::TraversalGraph),
            Some("forwarding") => Ok(Self::Forwarding),
            Some(other) => Err(format!(
                "{STORAGE_ENV}={other:?} is not a known test storage; use `forwarding`, \
                 `traversal-graph` or leave it unset"
            )),
        }
    }
}

/// The storage selected by [`STORAGE_ENV`], or the message for an unknown value (read once).
pub fn selected() -> Result<TestStorage, String> {
    static SELECTED: OnceLock<Result<TestStorage, String>> = OnceLock::new();
    SELECTED
        .get_or_init(|| {
            let value = std::env::var_os(STORAGE_ENV).map(|v| v.to_string_lossy().into_owned());
            TestStorage::parse(value.as_deref())
        })
        .clone()
}

/// The selected storage; panics with the message for an unknown value (test code: the run must
/// stop, never fall back to the default silently).
pub fn current() -> TestStorage {
    selected().unwrap_or_else(|message| panic!("{message}"))
}

/// Puts `graph` behind the selected storage, as the shared handle the harness holds.
pub fn wrap(graph: TraversalGraph) -> Arc<dyn AnyGraph> {
    match current() {
        TestStorage::TraversalGraph => Arc::new(Graph::new(graph)).into_any_graph(),
        TestStorage::Forwarding => Arc::new(Graph::new(Forwarding::new(graph))).into_any_graph(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_parse_and_unknown_ones_fail_with_a_message() {
        assert_eq!(TestStorage::parse(None), Ok(TestStorage::TraversalGraph));
        assert_eq!(
            TestStorage::parse(Some("")),
            Ok(TestStorage::TraversalGraph)
        );
        assert_eq!(
            TestStorage::parse(Some("traversal-graph")),
            Ok(TestStorage::TraversalGraph)
        );
        assert_eq!(
            TestStorage::parse(Some("forwarding")),
            Ok(TestStorage::Forwarding)
        );
        let err = TestStorage::parse(Some("sqlite")).unwrap_err();
        assert!(err.contains("GRAPHERSAL_TEST_STORAGE=\"sqlite\""), "{err}");
    }

    #[test]
    fn wrapped_graphs_are_of_the_selected_storage() {
        let graph = wrap(TraversalGraph::tinkerpop_modern());
        let is_forwarding = graph.as_any().downcast_ref::<Graph<Forwarding>>().is_some();
        assert_eq!(is_forwarding, current() == TestStorage::Forwarding);
        assert_eq!(
            graph.statistics().unwrap().map(|s| s.vertex_count()),
            Some(6)
        );
    }
}
