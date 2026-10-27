//! `nearest(key, q, k[, metric])`: exact top-k vector search over the incoming elements (a
//! Graphersal extension, book page "Vectors").

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use crate::graph::GraphStorage;
use crate::traversal::optimizer::path_analysis::{PathAnalysis, PathRequirement};
use crate::traversal::step::map_steps::step_similarity::{VectorKey, VectorQuery};
use crate::value::vector::vector_error;
use crate::value::{Vector, VectorError, VectorMetric};
use crate::*;

/// One kept candidate: its rank (smaller is better), its position in the input (ties go to
/// the earlier one) and the traverser.
struct Candidate<S: GraphStorage> {
    rank: f64,
    position: usize,
    traverser: Traverser<S>,
}

impl<S: GraphStorage> Candidate<S> {
    fn order(&self, other: &Self) -> Ordering {
        self.rank
            .total_cmp(&other.rank)
            .then(self.position.cmp(&other.position))
    }
}

impl<S: GraphStorage> PartialEq for Candidate<S> {
    fn eq(&self, other: &Self) -> bool {
        self.order(other) == Ordering::Equal
    }
}

impl<S: GraphStorage> Eq for Candidate<S> {}

impl<S: GraphStorage> PartialOrd for Candidate<S> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<S: GraphStorage> Ord for Candidate<S> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.order(other)
    }
}

/// The rank of a score: smaller is better, `-0.0` folded into `0.0` so the total order of
/// floats ties equal scores.
fn rank_of(score: f64, metric: VectorMetric) -> f64 {
    let rank = if metric.higher_is_better() {
        -score
    } else {
        score
    };
    rank + 0.0
}

/// `nearest(..)`: keeps the `k` incoming elements whose vector property `key` is closest to the
/// query vector, best first (module docs); see [`GraphTraversalSource::nearest`].
struct StepNearest<'g, S: GraphStorage + 'g> {
    key: VectorKey,
    query: VectorQuery<'g, S>,
    k: usize,
    /// `None`: the schema's `metric` annotation of the property, else cosine.
    metric: Option<VectorMetric>,
}

impl<'g, S: GraphStorage + 'g> Clone for StepNearest<'g, S> {
    fn clone(&self) -> Self {
        Self {
            key: self.key.clone(),
            query: self.query.clone(),
            k: self.k,
            metric: self.metric,
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepNearest<'g, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "nearest({}, {}, {}",
            self.key.literal(),
            self.query,
            self.k
        )?;
        if let Some(metric) = self.metric {
            write!(
                f,
                ", #{{metric: {}}}",
                crate::value::literal::rhai_string(metric.name())
            )?;
        }
        f.write_str(&self.query.embedder_suffix())?;
        f.write_str(")")
    }
}

fn nearest_step<'g, S: GraphStorage + 'g>(
    key: PropKey,
    query: VectorQuery<'g, S>,
    k: usize,
    metric: Option<VectorMetric>,
) -> GraphStep<'g, S> {
    GraphStep::FlatBarrier(
        "nearest",
        Box::new(StepNearest {
            key: VectorKey::new(key),
            query,
            k,
            metric,
        }),
    )
}

/// The metric resolved from the schema for the elements seen so far, cached by label set.
struct MetricCache {
    by_labels: Vec<(Vec<CompactString>, VectorMetric)>,
    chosen: Option<VectorMetric>,
}

impl MetricCache {
    fn resolve<S: GraphStorage>(
        &mut self,
        storage: &S,
        key: &VectorKey,
        value: &TraverserValue<S>,
    ) -> TraverserResult<VectorMetric> {
        let labels: &[CompactString] = match value {
            TraverserValue::Vertex(node) => storage.vertex_labels(*node),
            _ => &[],
        };
        let metric = match value {
            TraverserValue::Vertex(_) => {
                match self
                    .by_labels
                    .iter()
                    .find(|(seen, _)| seen.as_slice() == labels)
                {
                    Some((_, metric)) => *metric,
                    None => {
                        let metric = key
                            .declared_metric(storage, value)?
                            .unwrap_or(VectorMetric::Cosine);
                        if self.by_labels.len() < 64 {
                            self.by_labels.push((labels.to_vec(), metric));
                        }
                        metric
                    }
                }
            }
            _ => key
                .declared_metric(storage, value)?
                .unwrap_or(VectorMetric::Cosine),
        };
        match self.chosen {
            None => {
                self.chosen = Some(metric);
                Ok(metric)
            }
            Some(first) if first == metric => Ok(metric),
            Some(first) => Err(vector_error(
                "nearest",
                VectorError::MetricConflict {
                    first,
                    second: metric,
                },
            )),
        }
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatBarrier<'g, S> for StepNearest<'g, S> {
    fn property_access(
        &self,
        visit: &mut dyn FnMut(Option<&str>, crate::graph::workload::AccessKind),
    ) {
        if let Some(key) = self.key.top_level() {
            visit(Some(key), crate::graph::workload::AccessKind::Similarity);
        }
    }
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Vec<Traverser<S>>,
    ) -> TraverserResult<Option<Vec<Traverser<S>>>> {
        let k = self.k as u64;
        if k == 0 {
            return Ok(Some(Vec::new()));
        }
        let mut heap: BinaryHeap<Candidate<S>> = BinaryHeap::new();
        // The units (bulk) the heap holds: a merged traverser stands for `bulk` equal ones.
        let mut units: u64 = 0;
        let mut query: Option<Vector> = None;
        // The query's squared norm, computed once for the cosine of every candidate.
        let mut query_norm: Option<f64> = None;
        let mut cache = MetricCache {
            by_labels: Vec::new(),
            chosen: None,
        };
        for (position, traverser) in source.into_iter().enumerate() {
            context.tick()?;
            let Some(Some(stored)) =
                self.key
                    .peek(context, &traverser.value, |stored| match stored {
                        ElementProperty::Vector(vector) => Some(vector.clone()),
                        _ => None,
                    })?
            else {
                // No such property, or a value that is not a vector: skipped.
                continue;
            };
            let this_query =
                self.query
                    .evaluate("nearest", context, &traverser, Some(&self.key))?;
            let query = match &query {
                None => {
                    query_norm = this_query.raw_squared_norm();
                    query.insert(this_query)
                }
                Some(first) => {
                    if !first.same_data(&this_query) && *first != this_query {
                        return Err(vector_error("nearest", VectorError::QueryNotConstant));
                    }
                    first
                }
            };
            let metric = match self.metric {
                Some(metric) => metric,
                None => cache.resolve(context.storage, &self.key, &traverser.value)?,
            };
            context.tick_n(stored.dimension() / 1024)?;
            let score = stored
                .similarity_prepared(query, metric, query_norm)
                .map_err(|error| vector_error("nearest", error))?;
            let rank = rank_of(score, metric);
            if units >= k
                && let Some(worst) = heap.peek()
                && rank >= worst.rank
            {
                // Not better than the worst kept unit (an equal rank came earlier and wins).
                continue;
            }
            units = units.saturating_add(traverser.bulk);
            heap.push(Candidate {
                rank,
                position,
                traverser,
            });
            // Drop the worst entries while the rest still hold k units.
            while let Some(worst) = heap.peek() {
                if units - worst.traverser.bulk >= k {
                    units -= worst.traverser.bulk;
                    heap.pop();
                } else {
                    break;
                }
            }
        }
        let mut kept: Vec<Traverser<S>> = heap
            .into_sorted_vec()
            .into_iter()
            .map(|candidate| candidate.traverser)
            .collect();
        // The worst kept traverser may straddle the bound: cut its bulk to the k units.
        if units > k
            && let Some(last) = kept.last_mut()
        {
            last.bulk -= units - k;
        }
        Ok(Some(kept))
    }

    fn text_query_mut(
        &mut self,
    ) -> Option<(
        Option<&PropKey>,
        &mut crate::traversal::step::map_steps::step_similarity::TextQuery,
    )> {
        match &mut self.query {
            VectorQuery::Text(text) => Some((Some(self.key.prop_key()), text)),
            _ => None,
        }
    }

    fn text_query(&self) -> Option<&crate::traversal::step::map_steps::step_similarity::TextQuery> {
        match &self.query {
            VectorQuery::Text(text) => Some(text),
            _ => None,
        }
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &PathAnalysis,
        _downstream: &PathRequirement,
    ) -> PathRequirement {
        self.query.analyze_paths(analysis)
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut AnonymousTraversal<'g, S>> {
        self.query.traversal_mut().into_iter().collect()
    }

    fn clone_box(&self) -> Box<dyn GraphStepFlatBarrier<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `nearest(key, q, k[, metric])`: an exact top-k vector search. Keeps the `k` incoming
    /// elements whose vector property `key` (a name or a [`JsonPath`](crate::value::JsonPath))
    /// is closest to the query vector `q`, best first: the highest similarity for cosine and dot,
    /// the smallest distance for l2 and hamming. A Graphersal extension (book page "Vectors").
    ///
    /// - Exact: every incoming element is scored (a bounded heap of `k`, O(n log k)); filter
    ///   first (`has_label`, `has`) to search fewer.
    /// - Equal scores keep the input order (the earlier element first), so the result is
    ///   deterministic.
    /// - An element without the property, or whose value is not a vector, is skipped.
    /// - `q` is a [`Vector`] or an anonymous traversal (`__::select("q")`) run on every scored
    ///   element; every run must give the same vector (one query per search), else the step
    ///   fails.
    /// - `metric` `None` takes the `metric` annotation the stored schema declares for the
    ///   property (a top-level name), else cosine; elements whose labels declare different
    ///   metrics fail.
    /// - Bulk counts: a merged traverser of bulk n is n results (cut at `k`).
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::empty();
    /// let mut lock = graph.write();
    /// for (name, x) in [("a", 1.0), ("b", 0.0), ("c", -1.0)] {
    ///     let emb = Vector::from_f32(vec![x, 1.0]).unwrap();
    ///     lock.traversal_mut().add_v("doc").property("name", name).property("emb", emb)
    ///         .iterate().unwrap();
    /// }
    /// let q = Vector::from_f32(vec![1.0, 0.0]).unwrap();
    /// let names = lock.traversal().v(None).nearest("emb", q, 2, None).values("name")
    ///     .to_list().unwrap();
    /// assert_eq!(names, vec!["a".into(), "b".into()]);
    /// ```
    pub fn nearest(
        self,
        key: impl Into<PropKey>,
        q: impl Into<VectorQuery<'g, S>>,
        k: usize,
        metric: Option<VectorMetric>,
    ) -> Self {
        self.push_step(nearest_step(key.into(), q.into(), k, metric))
    }
}

impl __ {
    /// Anonymous `nearest(key, q, k[, metric])`; see [`GraphTraversalSource::nearest`].
    pub fn nearest<'g, S: GraphStorage + 'g>(
        key: impl Into<PropKey>,
        q: impl Into<VectorQuery<'g, S>>,
        k: usize,
        metric: Option<VectorMetric>,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().nearest(key, q, k, metric)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `nearest(key, q, k[, metric])`; see [`GraphTraversalSource::nearest`].
    pub fn nearest(
        self,
        key: impl Into<PropKey>,
        q: impl Into<VectorQuery<'g, S>>,
        k: usize,
        metric: Option<VectorMetric>,
    ) -> Self {
        self.push_step(nearest_step(key.into(), q.into(), k, metric))
    }
}
