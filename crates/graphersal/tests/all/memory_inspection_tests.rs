//! Integration tests for `graph.memory_usage()` and `.profile()`'s memory-aware figures — the
//! memory-inspection design.

use graphersal::prelude::*;
use graphersal::profile::{MemorySource, Metrics};
use graphersal::storage::MemoryStats;
use graphersal::{GraphSource, profile::ProfileType};

/// T1: basic sanity on an empty graph, and monotonic growth as elements/properties are added.
#[test]
fn t1_memory_usage_basics() {
    let empty = TraversalGraph::new();
    let stats = empty
        .memory_usage()
        .unwrap()
        .expect("TraversalGraph always supports introspection");
    // Index structures may have a minimum capacity even for an empty graph, so this is "small",
    // not necessarily exactly zero.
    assert!(
        stats.total_bytes() < 4096,
        "unexpectedly large empty-graph footprint: {stats:?}"
    );
    assert_eq!(stats.total_mb(), stats.total_bytes() as f64 / 1_048_576.0);

    let mut graph = TraversalGraph::new();
    let before = graph.memory_usage().unwrap().unwrap();

    let mut props = graphersal::prelude::PropertyMap::default();
    props.insert(
        "bio",
        ElementProperty::String("a fairly long biography string to force a heap allocation".into()),
    );
    let v1 = graph
        .add_vertex(None, vec!["person".into()], props)
        .unwrap();
    let after_one_vertex = graph.memory_usage().unwrap().unwrap();
    assert!(after_one_vertex.total_bytes() > before.total_bytes());

    let v2 = graph
        .add_vertex(
            None,
            vec!["person".into()],
            graphersal::prelude::PropertyMap::default(),
        )
        .unwrap();
    graph
        .add_edge(
            None,
            Some("knows".into()),
            v1,
            v2,
            graphersal::prelude::PropertyMap::default(),
        )
        .unwrap();
    let after_edge = graph.memory_usage().unwrap().unwrap();
    assert!(after_edge.total_bytes() > after_one_vertex.total_bytes());
}

/// T1 (continued): `memory_usage()` is reachable identically through the `GraphStorage` trait, not
/// just the concrete `TraversalGraph` type — the regression test for the trait-placement fix
/// itself (D1's "corrected from an earlier draft" note). A storage backend that does not override
/// the trait default (modeled here by a thin wrapper that implements every other method but
/// leaves `memory_usage` at its default) reports `None`, exactly as documented.
#[test]
fn t1_memory_usage_via_generic_storage_trait() {
    fn reachable_through_trait<S: GraphStorage>(storage: &S) -> Option<MemoryStats> {
        storage.memory_usage().unwrap()
    }

    let graph = TraversalGraph::new();
    assert!(reachable_through_trait(&graph).is_some());

    struct NoMemoryStorage<'a>(&'a TraversalGraph);
    impl<'a> GraphStorage for NoMemoryStorage<'a> {
        type VertexId = <TraversalGraph as GraphStorage>::VertexId;
        type EdgeId = <TraversalGraph as GraphStorage>::EdgeId;

        fn vertex_count(&self) -> Result<usize, GraphError> {
            GraphStorage::vertex_count(&self.0)
        }
        fn edge_count(&self) -> Result<usize, GraphError> {
            GraphStorage::edge_count(&self.0)
        }
        fn vertices(&self) -> Result<impl Iterator<Item = Self::VertexId> + '_, GraphError> {
            GraphStorage::vertices(self.0)
        }
        fn edges(&self) -> Result<impl Iterator<Item = Self::EdgeId> + '_, GraphError> {
            GraphStorage::edges(self.0)
        }
        fn get_vertex_by_id(&self, id: &str) -> Result<Option<Self::VertexId>, GraphError> {
            self.0.get_vertex_by_id(id)
        }
        fn get_edge_by_id(&self, id: &str) -> Result<Option<Self::EdgeId>, GraphError> {
            self.0.get_edge_by_id(id)
        }
        fn out_edges(
            &self,
            node: Self::VertexId,
        ) -> Result<impl Iterator<Item = Self::EdgeId> + '_, GraphError> {
            GraphStorage::out_edges(self.0, node)
        }
        fn in_edges(
            &self,
            node: Self::VertexId,
        ) -> Result<impl Iterator<Item = Self::EdgeId> + '_, GraphError> {
            GraphStorage::in_edges(self.0, node)
        }
        fn vertex_id(&self, node: Self::VertexId) -> Result<Option<&str>, GraphError> {
            self.0.vertex_id(node)
        }
        fn edge_id(&self, edge: Self::EdgeId) -> Result<Option<&str>, GraphError> {
            self.0.edge_id(edge)
        }
        fn vertex_label(&self, node: Self::VertexId) -> Result<Option<&str>, GraphError> {
            self.0.vertex_label(node)
        }
        fn edge_label(&self, edge: Self::EdgeId) -> Result<Option<&str>, GraphError> {
            self.0.edge_label(edge)
        }
        fn vertex_labels(
            &self,
            node: Self::VertexId,
        ) -> Result<&[compact_str::CompactString], GraphError> {
            self.0.vertex_labels(node)
        }
        fn vertex_properties(
            &self,
            node: Self::VertexId,
        ) -> Result<Option<&graphersal::prelude::PropertyMap>, GraphError> {
            self.0.vertex_properties(node)
        }
        fn edge_properties(
            &self,
            edge: Self::EdgeId,
        ) -> Result<Option<&graphersal::prelude::PropertyMap>, GraphError> {
            self.0.edge_properties(edge)
        }
        fn edge_endpoints(
            &self,
            edge: Self::EdgeId,
        ) -> Result<Option<(Self::VertexId, Self::VertexId)>, GraphError> {
            self.0.edge_endpoints(edge)
        }
        fn insert_vertex(
            &mut self,
            _id: Option<compact_str::CompactString>,
            _labels: Vec<compact_str::CompactString>,
            _properties: graphersal::prelude::PropertyMap,
        ) -> Result<Self::VertexId, graphersal::error::GraphError> {
            unimplemented!("read-only test wrapper")
        }
        fn add_vertex_label(
            &mut self,
            _node: Self::VertexId,
            _label: compact_str::CompactString,
        ) -> Result<bool, graphersal::error::GraphError> {
            unimplemented!("read-only test wrapper")
        }
        fn remove_vertex_label(
            &mut self,
            _node: Self::VertexId,
            _label: &str,
        ) -> Result<bool, graphersal::error::GraphError> {
            unimplemented!("read-only test wrapper")
        }
        fn insert_edge(
            &mut self,
            _id: Option<compact_str::CompactString>,
            _label: Option<compact_str::CompactString>,
            _from: Self::VertexId,
            _to: Self::VertexId,
            _properties: graphersal::prelude::PropertyMap,
        ) -> Result<Self::EdgeId, graphersal::error::GraphError> {
            unimplemented!("read-only test wrapper")
        }
        fn set_vertex_property(
            &mut self,
            _node: Self::VertexId,
            _key: compact_str::CompactString,
            _value: ElementProperty,
        ) -> Result<(), graphersal::error::GraphError> {
            unimplemented!("read-only test wrapper")
        }
        fn set_edge_property(
            &mut self,
            _edge: Self::EdgeId,
            _key: compact_str::CompactString,
            _value: ElementProperty,
        ) -> Result<(), graphersal::error::GraphError> {
            unimplemented!("read-only test wrapper")
        }
        fn drop_vertex(
            &mut self,
            _node: Self::VertexId,
        ) -> Result<(), graphersal::error::GraphError> {
            unimplemented!("read-only test wrapper")
        }
        fn drop_vertex_property(
            &mut self,
            _prop: &str,
            _node: Self::VertexId,
        ) -> Result<(), graphersal::error::GraphError> {
            unimplemented!("read-only test wrapper")
        }
        fn drop_edge(&mut self, _edge: Self::EdgeId) -> Result<(), graphersal::error::GraphError> {
            unimplemented!("read-only test wrapper")
        }
        fn drop_edge_property(
            &mut self,
            _prop: &str,
            _edge: Self::EdgeId,
        ) -> Result<(), graphersal::error::GraphError> {
            unimplemented!("read-only test wrapper")
        }
        // `memory_usage` is deliberately NOT overridden here, to exercise the trait's default.
    }

    let wrapper = NoMemoryStorage(&graph);
    assert_eq!(reachable_through_trait(&wrapper), None);
}

/// Acceptance: on the `large` fixture, `memory_usage()` returns `Some` with `total_bytes() > 0`.
#[test]
fn t1_large_fixture_reports_positive_total() {
    let graph = GraphSource::large_110k();
    let stats = graph
        .read()
        .memory_usage()
        .unwrap()
        .expect("TraversalGraph always supports introspection");
    assert!(stats.total_bytes() > 0);
    assert!(stats.structure_bytes > 0);
    assert!(stats.index_bytes > 0);
    assert!(stats.property_bytes > 0);
}

/// T2: category attribution. A large string property increases `property_bytes` specifically;
/// many same-labeled vertices increase `index_bytes` (label bucket growth).
#[test]
fn t2_category_attribution() {
    let mut graph = TraversalGraph::new();
    let before = graph.memory_usage().unwrap().unwrap();

    let mut props = graphersal::prelude::PropertyMap::default();
    let long_string: String = "x".repeat(4096);
    props.insert("blob", ElementProperty::String(long_string.into()));
    graph.add_vertex(None, vec![], props).unwrap();
    let after = graph.memory_usage().unwrap().unwrap();

    assert!(after.property_bytes > before.property_bytes + 3000);
    // Structure/index should not jump by anywhere near as much as the 4KB string did.
    assert!(after.structure_bytes - before.structure_bytes < 1000);
    assert!(after.index_bytes - before.index_bytes < 1000);

    let before_labels = graph.memory_usage().unwrap().unwrap();
    for _ in 0..200 {
        graph
            .add_vertex(
                None,
                vec!["dup".into()],
                graphersal::prelude::PropertyMap::default(),
            )
            .unwrap();
    }
    let after_labels = graph.memory_usage().unwrap().unwrap();
    assert!(after_labels.index_bytes > before_labels.index_bytes);
}

/// T3: the hard requirement's functional half. Profiling must never change query semantics — the
/// same query run with and without `.profile()` produces identical data results.
#[test]
fn t3_profiling_does_not_change_results() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();

    let g1 = lock.traversal();
    let without_profile = g1
        .v(None::<()>)
        .has_label("person")
        .values("name")
        .to_list()
        .unwrap();

    let g2 = lock.traversal();
    let profiled = g2
        .v(None::<()>)
        .has_label("person")
        .values("name")
        .execute_with(ExecOptions::profile(ProfileType::Memory));

    assert!(profiled.profile().is_some());
    assert_eq!(without_profile, profiled.into_result().unwrap());
}

/// T4: the zero-allocation indicator. A fully lazy chain reports zero-allocation for every step;
/// `group()` reports `bytes_materialized > 0`. Requires `.profile_with(ProfileType::Memory)`: a
/// bare `.profile()` collects no memory data at all (no `memory` section on any step).
#[test]
fn t4_zero_allocation_indicator() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let g = lock.traversal();

    let stats = g
        .v(None::<()>)
        .has_label("person")
        .limit(1)
        .count()
        .profile_with(ProfileType::Memory)
        .unwrap();
    fn assert_all_zero_alloc(nodes: &[Metrics]) {
        for node in nodes {
            assert_eq!(
                node.memory().map(|m| m.bytes_materialized()),
                Some(0),
                "expected zero allocation for {:?}",
                node.name(),
            );
            assert_all_zero_alloc(node.nested());
        }
    }
    assert_all_zero_alloc(stats.metrics());

    let g2 = lock.traversal();
    let group_stats = g2
        .v(None::<()>)
        .group()
        .by(By::Label)
        .profile_with(ProfileType::Memory)
        .unwrap();
    #[allow(unused_variables)]
    let group_node = group_stats
        .metrics()
        .iter()
        .find(|n| n.name().contains("group"))
        .expect("a group() node is present");
    // Under the default (estimated) path this is always `> 0`. Under `alloc-tracking` without an
    // installed `TrackingAllocator` (this test binary does not install one — see `t8`'s doc
    // comment), a `Tracked` delta legitimately reads `0`, indistinguishable from "allocated
    // nothing" by design (`MemorySource::Tracked`'s doc comment) — so this specific assertion is
    // only meaningful on the estimation path.
    #[cfg(not(feature = "alloc-tracking"))]
    {
        assert!(group_node.memory().unwrap().bytes_materialized() > 0);
    }
}

/// T5: nested steps report their own `bytes_materialized` in their own `memory` section.
#[test]
fn t5_nested_steps_sum_correctly() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let g = lock.traversal();

    let stats = g
        .v(None::<()>)
        .repeat(
            graphersal::prelude::__::out(None::<()>)
                .group()
                .by(By::Label),
        )
        .times(1)
        .profile_with(ProfileType::Memory)
        .unwrap();

    #[cfg_attr(feature = "alloc-tracking", allow(dead_code))]
    fn sum(nodes: &[Metrics]) -> u64 {
        nodes
            .iter()
            .map(|n| n.memory().map_or(0, |m| m.bytes_materialized()) + sum(n.nested()))
            .sum()
    }
    // The nested `group()` of the repeat body has its own memory section, so the tree carries a
    // figure below the top level, and the table's total row sums the whole tree.
    let repeat = &stats.metrics()[1];
    assert!(repeat.nested().iter().all(|n| n.memory().is_some()));
    #[cfg(not(feature = "alloc-tracking"))]
    assert!(sum(repeat.nested()) > 0);
    assert!(stats.to_string().contains("TOTAL: memory materialized:"));
}

/// T6: `#[non_exhaustive]` blocks external struct-literal construction, by construction (a
/// compile-time property; there is no runtime check to make here). Documented per D5/D6's
/// grep-for-usage requirement: nothing outside `crates/graphersal/src` constructs `MemoryStats`,
/// `MemorySource`, or any of the six profiling structs via struct-literal syntax — every
/// integration test that previously did (`test_serde.rs`, `nested_profiling_tests.rs`,
/// `repeat_tests.rs`) was migrated to `ValueType::default()` + field assignment, which remains legal
/// for a `#[non_exhaustive]` type with public fields and a `Default` impl. The disabled block below
/// is intentionally never compiled — its only purpose is to show what no longer compiles.
#[cfg(any())]
fn this_would_not_compile() {
    let _ = MemoryStats {
        structure_bytes: 0,
        index_bytes: 0,
        property_bytes: 0,
    };
}

/// T7: broad regression coverage — a representative slice of steps still profile cleanly with the
/// new memory fields present, and `Display` renders without panicking.
#[test]
fn t7_profile_display_regression() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let g = lock.traversal();
    let stats = g
        .v(None::<()>)
        .out(None::<()>)
        .values("name")
        .profile_with(ProfileType::Memory)
        .unwrap();
    let rendered = stats.to_string();
    assert!(rendered.contains("Mem"));
    assert!(rendered.contains("TOTAL: memory materialized:"));
}

/// T9: feature-off equivalence. Without `alloc-tracking`, every `Metrics::memory` source is
/// `Estimated`, unconditionally, once `ProfileType::Memory` is requested.
#[cfg(not(feature = "alloc-tracking"))]
#[test]
fn t9_feature_off_is_always_estimated() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let g = lock.traversal();
    let stats = g
        .v(None::<()>)
        .group()
        .by(By::Label)
        .profile_with(ProfileType::Memory)
        .unwrap();

    fn assert_all_estimated(nodes: &[Metrics]) {
        for node in nodes {
            assert_eq!(
                node.memory().map(|m| m.source()),
                Some(MemorySource::Estimated)
            );
            assert_all_estimated(node.nested());
        }
    }
    assert_all_estimated(stats.metrics());
}

/// T8: `alloc-tracking` correctness. Only compiled when the feature is enabled
/// (`cargo test --features alloc-tracking`). A query that materializes a known amount of data
/// reports `bytes_materialized > 0` with `MemorySource::Tracked`; a fully-lazy chain still reports
/// `0` even under `Tracked` (D3/D4's documented, deliberate gating — see `source.rs`'s
/// `step_memory_metrics` doc comment for why this is not a false guarantee).
///
/// This test does not install `TrackingAllocator` as the process's `#[global_allocator]` (that can
/// only happen once, process-wide, and `cargo test` links many independent test binaries) — so
/// `thread_allocated_bytes()` reads `0` throughout, and every delta is `0`. This is exactly
/// [`MemorySource::Tracked`]'s documented "indistinguishable from genuinely allocated nothing"
/// caveat, exercised deliberately: it proves the `Tracked` *label* is applied whenever the feature
/// is compiled in (matching D4's compile-time, not runtime, choice), while `graphersal`'s own
/// integration test (`memory_usage_reports_fields_through_the_visualizer` and
/// `profile_reports_tracked_memory_figures` in `crates/graphersal-cli/tests/cli_tests.rs`) is what proves
/// the end-to-end *non-zero, exact* figure once the allocator actually is installed by a binary.
#[cfg(feature = "alloc-tracking")]
#[test]
fn t8_alloc_tracking_labels_every_node_tracked() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let g = lock.traversal();
    let stats = g
        .v(None::<()>)
        .group()
        .by(By::Label)
        .profile_with(ProfileType::Memory)
        .unwrap();

    fn assert_all_tracked(nodes: &[Metrics]) {
        for node in nodes {
            assert_eq!(
                node.memory().map(|m| m.source()),
                Some(MemorySource::Tracked)
            );
            assert_all_tracked(node.nested());
        }
    }
    assert_all_tracked(stats.metrics());

    // A fully lazy chain still reports 0 even though `memory_source` reads `Tracked` — the
    // gating documented on `source.rs`'s `step_memory_metrics`.
    let g2 = lock.traversal();
    let lazy_stats = g2
        .v(None::<()>)
        .has_label("person")
        .limit(1)
        .count()
        .profile_with(ProfileType::Memory)
        .unwrap();
    fn assert_all_zero(nodes: &[Metrics]) {
        for node in nodes {
            assert_eq!(node.memory().map(|m| m.bytes_materialized()), Some(0));
            assert_all_zero(node.nested());
        }
    }
    assert_all_zero(lazy_stats.metrics());
}

/// T10: richer-memory-diagnostics spec, feature-off equivalence. Without `alloc-tracking`, every
/// `MemoryMetrics::bytes_retained`/`alloc_count` is `None`, unconditionally — the estimation path
/// has no allocator-level data to draw them from (D2's Non-goals).
#[cfg(not(feature = "alloc-tracking"))]
#[test]
fn t10_feature_off_retained_and_alloc_count_are_none() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let g = lock.traversal();
    let stats = g
        .v(None::<()>)
        .group()
        .by(By::Label)
        .profile_with(ProfileType::Memory)
        .unwrap();

    fn assert_all_none(nodes: &[Metrics]) {
        for node in nodes {
            assert_eq!(node.memory().unwrap().bytes_retained(), None);
            assert_eq!(node.memory().unwrap().alloc_count(), None);
            assert_all_none(node.nested());
        }
    }
    assert_all_none(stats.metrics());
}

/// T11: richer-memory-diagnostics spec, `alloc-tracking` correctness of the *labeling*. Every node
/// carries `Some` for `bytes_retained`/`alloc_count` whenever `memory_source` is `Tracked` — same
/// "labeling, not magnitude" scope as `t8`, since this test binary does not install
/// `TrackingAllocator` (see `t8`'s doc comment for why): the real, non-zero, self-consistent
/// figures are proven end to end by `graphersal`'s `profile_reports_net_retained_and_alloc_count`
/// (`crates/graphersal-cli/tests/cli_tests.rs`).
#[cfg(feature = "alloc-tracking")]
#[test]
fn t11_alloc_tracking_labels_every_node_with_retained_and_alloc_count() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let g = lock.traversal();
    let stats = g
        .v(None::<()>)
        .group()
        .by(By::Label)
        .profile_with(ProfileType::Memory)
        .unwrap();

    fn assert_all_some(nodes: &[Metrics]) {
        for node in nodes {
            assert!(
                node.memory().unwrap().bytes_retained().is_some(),
                "{:?} missing bytes_retained under Tracked",
                node.name()
            );
            assert!(
                node.memory().unwrap().alloc_count().is_some(),
                "{:?} missing alloc_count under Tracked",
                node.name()
            );
            assert_all_some(node.nested());
        }
    }
    assert_all_some(stats.metrics());
    // The table sums the same telescoping deltas into a net retained row.
    // Column padding sits between "TOTAL:" and the row label, so match per line.
    let table = stats.to_string();
    assert!(
        table
            .lines()
            .any(|line| line.trim_start().starts_with("TOTAL:") && line.contains("net retained:")),
        "no net retained total row in:\n{table}"
    );
}

/// T12: `ProfileType` bitmask spec, D2/D3. A bare `.profile()` collects no memory data at all:
/// no step carries a `memory` section (not measured is not "zero allocation") — regardless of
/// whether `alloc-tracking` is compiled
/// in, since `ProfileType::Memory` gates collection before any allocator/estimation code runs.
#[test]
fn t12_bare_profile_never_requests_memory() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let g = lock.traversal();
    let stats = g.v(None::<()>).group().by(By::Label).profile().unwrap();

    fn assert_all_not_requested(nodes: &[Metrics]) {
        for node in nodes {
            // Not measured is not "zero": the section is absent.
            assert_eq!(node.memory(), None);
            assert_all_not_requested(node.nested());
        }
    }
    assert_all_not_requested(stats.metrics());
    assert!(!stats.to_json().contains("\"memory\""));
}

/// T13: `ProfileType` bitmask spec, D4. A bare `.profile()`'s rendered table is exactly the
/// plain, pre-memory-work shape: `Step | Call | In | Out | Time | % Dur`, no `Mem` column, and
/// none of the `[zero-alloc:..]`/`[retained:..]` annotation lines this spec drops entirely.
#[test]
fn t13_bare_profile_renders_the_plain_table() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let g = lock.traversal();
    let stats = g
        .v(None::<()>)
        .has_label("person")
        .count()
        .profile()
        .unwrap();
    let rendered = stats.to_string();

    assert!(
        !rendered.contains("Mem"),
        "unexpected Mem column: {rendered}"
    );
    assert!(
        !rendered.contains("Retained"),
        "unexpected Retained column: {rendered}"
    );
    assert!(
        !rendered.contains("Allocs"),
        "unexpected Allocs column: {rendered}"
    );
    assert!(
        !rendered.contains("zero-alloc"),
        "unexpected zero-alloc line: {rendered}"
    );
    assert!(
        !rendered.contains("[retained:"),
        "unexpected retained annotation line: {rendered}"
    );
    assert!(
        rendered.contains("Step"),
        "missing the plain header: {rendered}"
    );
    assert!(
        rendered.contains("% Dur"),
        "missing the plain header: {rendered}"
    );
}

/// T14: `ProfileType` bitmask spec, D4. `.profile_with(ProfileType::Memory)` renders real,
/// aligned `Mem`/`Retained`/`Allocs` columns — spot-checks column alignment across multiple rows
/// of differing name/number lengths, a real risk area for hand-rolled fixed-width formatting.
#[test]
fn t14_memory_profile_renders_aligned_columns() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let g = lock.traversal();
    let stats = g
        .v(None::<()>)
        .out(None::<()>)
        .has_label("software")
        .values("name")
        .profile_with(ProfileType::Memory)
        .unwrap();
    let rendered = stats.to_string();

    assert!(rendered.contains("Mem"));
    assert!(rendered.contains("Retained"));
    assert!(rendered.contains("Allocs"));
    assert!(!rendered.contains("zero-alloc"));
    assert!(!rendered.contains("[retained:"));

    let header_idx = rendered
        .lines()
        .position(|l| l.trim_start().starts_with("Step"))
        .expect("header row present");
    let header_line = rendered.lines().nth(header_idx).unwrap();
    let header_field_count = header_line.split_whitespace().count();
    assert_eq!(
        header_field_count, 10,
        "expected 10 whitespace-delimited header tokens (Step Call In Out Time Mem Retained Allocs % Dur): {header_line:?}"
    );
    // Every data row has 9 fields: the header's "% Dur" is two words, but a row's own `% Dur`
    // value is a single number (e.g. `4.17`).
    let expected_row_fields = header_field_count - 1;

    // Every data row (after the header and its `====` separator, up to the `TOTAL:` block) must
    // split into the same number of whitespace-delimited fields — a regression that drops or
    // misaligns a column changes this count, since none of this query's step names contain
    // internal whitespace.
    let mut checked_rows = 0;
    for line in rendered.lines().skip(header_idx + 2) {
        if line.trim_start().starts_with("TOTAL:") || line.starts_with('=') {
            break;
        }
        if line.trim().is_empty() {
            continue;
        }
        let field_count = line.split_whitespace().count();
        assert_eq!(
            field_count, expected_row_fields,
            "row has {field_count} fields, expected {expected_row_fields}: {line:?}"
        );
        checked_rows += 1;
    }
    assert!(
        checked_rows >= 3,
        "expected at least 3 data rows to spot-check, got {checked_rows}: {rendered}"
    );
}

/// `property_bytes` counts the storage of every property: a stored element keeps one value slot
/// per property (its keys live once per distinct key sequence, in `shape_bytes`, which is part
/// of `property_bytes`). A graph of many small vertices therefore reports at least the value
/// slot per property, and far less than a map per element would cost.
#[test]
fn property_bytes_count_the_hash_index_of_every_map() {
    let mut graph = TraversalGraph::new();
    let vertices = 2000usize;
    let per_vertex = 5usize;
    for i in 0..vertices {
        let mut props = PropertyMap::default();
        for j in 0..per_vertex {
            props.insert(format!("k{j}"), ElementProperty::from((i * j) as i64));
        }
        graph
            .add_vertex(None, vec!["person".into()], props)
            .unwrap();
    }
    let stats = graph.memory_usage().unwrap().unwrap();
    let per_slot = stats.property_bytes as f64 / (vertices * per_vertex) as f64;
    // Never below the value slot alone.
    let entry = size_of::<ElementProperty>() as f64;
    assert!(per_slot >= entry, "{per_slot} per slot");
    assert!(stats.shape_bytes > 0 && stats.shape_bytes < stats.property_bytes);
    #[cfg(target_pointer_width = "64")]
    assert!(
        (32.0..64.0).contains(&per_slot),
        "{per_slot} bytes per property slot"
    );

    // A vertex without properties owns no property heap.
    let mut bare = TraversalGraph::new();
    bare.add_vertex(None, vec!["person".into()], PropertyMap::default())
        .unwrap();
    assert_eq!(bare.memory_usage().unwrap().unwrap().property_bytes, 0);
}
