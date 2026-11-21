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
//!   `target/tinkerpop-report-forwarding.md`;
//! - `layered`: every dataset graph is a `Graph<LayeredGraph>` over a frozen base that holds the
//!   dataset (the TinkerPop graph initializer and the openCypher setup queries write into that
//!   base too, before the layer is put over it); the scenario's own writes go into the layer;
//! - `layered-delta`: every dataset graph is a `Graph<LayeredGraph>` over an EMPTY base, with the
//!   dataset written into the layer's delta ([`copy_into_delta`]; initializers and setup queries
//!   write into the delta as well).
//!
//! Every lane other than the default one is gated against the same baseline and page exactly
//! like `forwarding` and writes its report to `<suite>-report-<name>.md`.
//!
//! Any other value fails the run with a message. The harness holds every graph as
//! `Arc<dyn AnyGraph>` in every mode, so the code path above the storage is the same.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use graphersal::TraversalGraph;
use graphersal::storage::{AnyGraph, Graph, GraphStorage, IntoAnyGraph, LayeredGraph};

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
    /// `Graph<LayeredGraph>` over a base holding the dataset (`GRAPHERSAL_TEST_STORAGE=layered`).
    Layered,
    /// `Graph<LayeredGraph>` over an empty base, the dataset in the delta
    /// (`GRAPHERSAL_TEST_STORAGE=layered-delta`).
    LayeredDelta,
}

impl TestStorage {
    /// The value of [`STORAGE_ENV`] that selects this storage.
    pub fn name(self) -> &'static str {
        match self {
            Self::TraversalGraph => "traversal-graph",
            Self::Forwarding => "forwarding",
            Self::Layered => "layered",
            Self::LayeredDelta => "layered-delta",
        }
    }

    /// The suite's report file on this storage: `<suite>-report.md` on the default one, else
    /// `<suite>-report-<name>.md`.
    pub fn report_file(self, suite: &str) -> String {
        match self {
            Self::TraversalGraph => format!("{suite}-report.md"),
            other => format!("{suite}-report-{}.md", other.name()),
        }
    }

    /// Parses a value of [`STORAGE_ENV`] (`None`: unset).
    pub fn parse(value: Option<&str>) -> Result<Self, String> {
        match value.map(str::trim) {
            None | Some("") | Some("traversal-graph") => Ok(Self::TraversalGraph),
            Some("forwarding") => Ok(Self::Forwarding),
            Some("layered") => Ok(Self::Layered),
            Some("layered-delta") => Ok(Self::LayeredDelta),
            Some(other) => Err(format!(
                "{STORAGE_ENV}={other:?} is not a known test storage; use `forwarding`, \
                 `layered`, `layered-delta`, `traversal-graph` or leave it unset"
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
        TestStorage::Layered => Arc::new(Graph::new(layer_over(graph))).into_any_graph(),
        TestStorage::LayeredDelta => Arc::new(Graph::new(copy_into_delta(&graph))).into_any_graph(),
    }
}

/// A new layer over `graph` as its frozen base; panics when the base cannot carry a layer
/// (test code: the lane must stop, never fall back silently).
pub fn layer_over(graph: TraversalGraph) -> LayeredGraph {
    LayeredGraph::over(Arc::new(graph))
        .unwrap_or_else(|e| panic!("the dataset cannot be the base of a layer: {e}"))
}

/// A layer over an empty base that holds `graph`'s schema, catalog and data in its delta:
/// every vertex, then every edge, in the source's scan order, with ids, labels and properties
/// as stored ([`GraphStorage::insert_vertex`] / [`GraphStorage::insert_edge`], so no default is
/// generated). The copy reads like the source (the same scan and adjacency order: a source
/// without removed slots lists its elements in creation order, and the copy creates them in
/// that order). Panics on a refused write (test code).
pub fn copy_into_delta(graph: &TraversalGraph) -> LayeredGraph {
    fn copy(graph: &TraversalGraph) -> Result<LayeredGraph, graphersal::error::GraphError> {
        let mut layer = LayeredGraph::default();
        if let Some(schema) = graph.get_schema()? {
            layer.set_schema(schema)?;
        }
        let definitions: Vec<_> = graph.definitions()?.cloned().collect();
        if !definitions.is_empty() {
            layer.set_definitions(definitions)?;
        }
        let mut vertices = HashMap::new();
        for node in graph.vertices()? {
            let id = graph.vertex_id(node)?.map(Into::into);
            let labels = graph.vertex_labels(node)?.to_vec();
            let properties = graph.vertex_properties(node)?.cloned().unwrap_or_default();
            vertices.insert(node, layer.insert_vertex(id, labels, properties)?);
        }
        for edge in graph.edges()? {
            let id = graph.edge_id(edge)?.map(Into::into);
            let label = graph.edge_label(edge)?.map(Into::into);
            let Some((from, to)) = graph.edge_endpoints(edge)? else {
                continue;
            };
            let properties = graph.edge_properties(edge)?.cloned().unwrap_or_default();
            layer.insert_edge(id, label, vertices[&from], vertices[&to], properties)?;
        }
        Ok(layer)
    }
    copy(graph).unwrap_or_else(|e| panic!("the dataset cannot be copied into a delta: {e}"))
}

/// Runs `f` on the graph that receives the dataset's setup writes (the TinkerPop graph
/// initializer): on `layered`, a plain graph over the layer's base, after which the layer is put
/// back over the changed base (the layer must be untouched and the only holder of its base,
/// which every fresh dataset copy is); on every other storage `graph` itself.
pub fn write_into_base<R>(graph: &Arc<dyn AnyGraph>, f: impl FnOnce(Arc<dyn AnyGraph>) -> R) -> R {
    let layered = match current() {
        TestStorage::Layered => graph.as_any().downcast_ref::<Graph<LayeredGraph>>(),
        _ => None,
    };
    let Some(layered) = layered else {
        return f(graph.clone());
    };
    let mut guard = layered.write();
    let base = Arc::clone(std::mem::take(&mut *guard).base());
    let base = Arc::try_unwrap(base)
        .unwrap_or_else(|_| panic!("write_into_base: the layer's base is shared"));
    let plain = Arc::new(Graph::new(base));
    let result = f(Arc::clone(&plain).into_any_graph());
    let base = Arc::try_unwrap(plain)
        .unwrap_or_else(|_| panic!("write_into_base: the setup graph is still held"))
        .into_inner();
    *guard = layer_over(base);
    result
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
        assert_eq!(
            TestStorage::parse(Some("layered")),
            Ok(TestStorage::Layered)
        );
        assert_eq!(
            TestStorage::parse(Some("layered-delta")),
            Ok(TestStorage::LayeredDelta)
        );
        let err = TestStorage::parse(Some("sqlite")).unwrap_err();
        assert!(err.contains("GRAPHERSAL_TEST_STORAGE=\"sqlite\""), "{err}");
    }

    #[test]
    fn wrapped_graphs_are_of_the_selected_storage() {
        let graph = wrap(TraversalGraph::tinkerpop_modern());
        let is_forwarding = graph.as_any().downcast_ref::<Graph<Forwarding>>().is_some();
        assert_eq!(is_forwarding, current() == TestStorage::Forwarding);
        let is_layered = graph
            .as_any()
            .downcast_ref::<Graph<LayeredGraph>>()
            .is_some();
        assert_eq!(
            is_layered,
            matches!(current(), TestStorage::Layered | TestStorage::LayeredDelta)
        );
        assert_eq!(
            graph.statistics().unwrap().map(|s| s.vertex_count()),
            Some(6)
        );
    }

    #[test]
    fn setup_writes_go_into_the_base_on_the_layered_storage() {
        let graph = wrap(TraversalGraph::default());
        // The setup graph is a plain one on the default and the layered storage.
        let plain = write_into_base(&graph, |setup| {
            let plain = setup.as_any().downcast_ref::<Graph<TraversalGraph>>();
            if let Some(plain) = plain {
                plain
                    .write()
                    .insert_vertex(Some("a".into()), vec!["x".into()], Default::default())
                    .unwrap();
            }
            plain.is_some()
        });
        let storage = current();
        assert_eq!(
            plain,
            matches!(storage, TestStorage::TraversalGraph | TestStorage::Layered)
        );
        if !plain {
            return;
        }
        assert_eq!(
            graph.statistics().unwrap().map(|s| s.vertex_count()),
            Some(1)
        );
        if storage == TestStorage::Layered {
            let layered = graph
                .as_any()
                .downcast_ref::<Graph<LayeredGraph>>()
                .unwrap();
            let layer = layered.read();
            assert_eq!(
                layer.base().statistics().unwrap().map(|s| s.vertex_count()),
                Some(1)
            );
        }
    }

    #[test]
    fn the_copy_into_a_delta_holds_the_dataset_over_an_empty_base() {
        let source = TraversalGraph::tinkerpop_modern();
        let layer = copy_into_delta(&source);
        assert_eq!(
            layer.base().statistics().unwrap().map(|s| s.vertex_count()),
            Some(0)
        );
        assert_eq!(
            layer
                .statistics()
                .unwrap()
                .map(|s| (s.vertex_count(), s.edge_count())),
            source
                .statistics()
                .unwrap()
                .map(|s| (s.vertex_count(), s.edge_count()))
        );
        fn ids<S: GraphStorage>(graph: &S) -> Vec<String> {
            let vertices = graph.vertices().unwrap();
            vertices
                .map(|v| graph.vertex_id(v).unwrap().unwrap_or_default().to_string())
                .collect()
        }
        assert_eq!(ids(&layer), ids(&source));
    }
}
