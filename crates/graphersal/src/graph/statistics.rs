//! Graph statistics: counts a storage keeps up to date as the graph changes, so the optimizer and
//! users can read them without scanning the graph.
//!
//! [`TraversalGraph`](crate::TraversalGraph) maintains its [`GraphStatistics`] incrementally in the
//! same raw helpers its undo log replays (`insert_vertex_raw`, `remove_vertex_raw`,
//! `insert_edge_raw`, `remove_edge_raw`, label changes), so every mutation, every rollback and
//! every load or replay keeps them exact. Each update is O(number of labels involved) and
//! allocates only when a label is counted for the first time.
//!
//! Extension plan: degree and property statistics (counts, distinct values), histograms, and
//! persistence stamped with the commit sequence they describe. Everything kept today is
//! recomputable from the data, so nothing is persisted: loading a graph (GraphML, replay) rebuilds
//! the statistics. A serialized form (behind the `serde` feature) comes with the first statistic
//! that cannot be derived cheaply on load.

use std::collections::HashMap;

use compact_str::CompactString;
use fnv::FnvBuildHasher;

use crate::graph::GraphStorage;

/// Per-label counters: a label is present only while its count is above zero.
type LabelCounts = HashMap<CompactString, usize, FnvBuildHasher>;

/// Counts of a graph's elements, kept up to date by the storage as the graph changes; read them
/// through [`GraphStorage::statistics`] (or `g.statistics()` in the script DSL).
///
/// - [`Self::vertex_count`] / [`Self::edge_count`]: the totals.
/// - [`Self::vertex_label_count`]: vertices carrying a label anywhere in their label set (a
///   multi-label vertex counts under every label it carries, like `has_label`).
/// - [`Self::primary_vertex_label_count`]: vertices whose *primary* (first) label it is, what
///   `label()` returns; every labeled vertex counts exactly once (what
///   `group_count().by(T.label)` returns).
/// - [`Self::edge_label_count`]: edges per label.
///
/// Unlabeled elements are counted in the totals only.
///
/// ```rust
/// use graphersal::prelude::*;
///
/// let graph = TraversalGraph::tinkerpop_modern();
/// let stats = graph.statistics().unwrap().expect("TraversalGraph keeps statistics");
/// assert_eq!(stats.vertex_count(), 6);
/// assert_eq!(stats.vertex_label_count("person"), 4);
/// assert_eq!(stats.edge_label_count("knows"), 2);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct GraphStatistics {
    vertex_count: usize,
    edge_count: usize,
    vertex_labels: LabelCounts,
    primary_vertex_labels: LabelCounts,
    edge_labels: LabelCounts,
}

fn increment(counts: &mut LabelCounts, label: &str) {
    match counts.get_mut(label) {
        Some(count) => *count += 1,
        None => {
            counts.insert(CompactString::from(label), 1);
        }
    }
}

fn decrement(counts: &mut LabelCounts, label: &str) {
    if let Some(count) = counts.get_mut(label) {
        *count = count.saturating_sub(1);
        if *count == 0 {
            counts.remove(label);
        }
    }
}

fn sorted(counts: &LabelCounts) -> Vec<(&str, usize)> {
    let mut entries: Vec<(&str, usize)> = counts.iter().map(|(l, c)| (l.as_str(), *c)).collect();
    entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
    entries
}

impl GraphStatistics {
    /// The statistics of an empty graph.
    pub fn new() -> Self {
        Self::default()
    }

    /// Computes the statistics of `storage` with one full scan of its vertices and edges: the
    /// reference an incrementally maintained [`GraphStatistics`] must always equal, and a way for
    /// a storage without its own bookkeeping to produce one.
    pub fn from_storage<S: GraphStorage + ?Sized>(storage: &S) -> Self {
        let mut stats = Self::new();
        for vertex in storage.vertices() {
            stats.vertex_added(storage.vertex_labels(vertex));
        }
        for edge in storage.edges() {
            stats.edge_added(storage.edge_label(edge));
        }
        stats
    }

    /// The number of vertices.
    pub fn vertex_count(&self) -> usize {
        self.vertex_count
    }

    /// The number of edges.
    pub fn edge_count(&self) -> usize {
        self.edge_count
    }

    /// The number of vertices carrying `label` anywhere in their label set.
    pub fn vertex_label_count(&self, label: &str) -> usize {
        self.vertex_labels.get(label).copied().unwrap_or(0)
    }

    /// The number of vertices whose primary (first) label is `label`.
    pub fn primary_vertex_label_count(&self, label: &str) -> usize {
        self.primary_vertex_labels.get(label).copied().unwrap_or(0)
    }

    /// The number of edges labeled `label`.
    pub fn edge_label_count(&self, label: &str) -> usize {
        self.edge_labels.get(label).copied().unwrap_or(0)
    }

    /// Every vertex label with its [`Self::vertex_label_count`], sorted by label.
    pub fn vertex_label_counts(&self) -> Vec<(&str, usize)> {
        sorted(&self.vertex_labels)
    }

    /// Every primary vertex label with its [`Self::primary_vertex_label_count`], sorted by label.
    pub fn primary_vertex_label_counts(&self) -> Vec<(&str, usize)> {
        sorted(&self.primary_vertex_labels)
    }

    /// Every edge label with its [`Self::edge_label_count`], sorted by label.
    pub fn edge_label_counts(&self) -> Vec<(&str, usize)> {
        sorted(&self.edge_labels)
    }

    /// A vertex with `labels` was stored.
    pub(crate) fn vertex_added(&mut self, labels: &[CompactString]) {
        self.vertex_count += 1;
        self.labels_added(labels);
    }

    /// A vertex with `labels` was removed.
    pub(crate) fn vertex_removed(&mut self, labels: &[CompactString]) {
        self.vertex_count = self.vertex_count.saturating_sub(1);
        self.labels_removed(labels);
    }

    /// A vertex's label set changed from `old` to `new`.
    pub(crate) fn vertex_relabeled(&mut self, old: &[CompactString], new: &[CompactString]) {
        self.labels_removed(old);
        self.labels_added(new);
    }

    /// An edge with `label` was stored.
    pub(crate) fn edge_added(&mut self, label: Option<&str>) {
        self.edge_count += 1;
        if let Some(label) = label {
            increment(&mut self.edge_labels, label);
        }
    }

    /// An edge with `label` was removed.
    pub(crate) fn edge_removed(&mut self, label: Option<&str>) {
        self.edge_count = self.edge_count.saturating_sub(1);
        if let Some(label) = label {
            decrement(&mut self.edge_labels, label);
        }
    }

    /// An edge's label changed from `old` to `new` (`None`: unlabeled).
    pub(crate) fn edge_relabeled(&mut self, old: Option<&str>, new: Option<&str>) {
        if let Some(old) = old {
            decrement(&mut self.edge_labels, old);
        }
        if let Some(new) = new {
            increment(&mut self.edge_labels, new);
        }
    }

    fn labels_added(&mut self, labels: &[CompactString]) {
        for label in labels {
            increment(&mut self.vertex_labels, label);
        }
        if let Some(primary) = labels.first() {
            increment(&mut self.primary_vertex_labels, primary);
        }
    }

    fn labels_removed(&mut self, labels: &[CompactString]) {
        for label in labels {
            decrement(&mut self.vertex_labels, label);
        }
        if let Some(primary) = labels.first() {
            decrement(&mut self.primary_vertex_labels, primary);
        }
    }
}
