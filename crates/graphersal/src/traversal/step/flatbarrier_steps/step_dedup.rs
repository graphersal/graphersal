use crate::graph::GraphStorage;
use crate::traversal::optimizer::path_analysis::{PathAnalysis, PathRequirement};
use crate::*;
use fnv::FnvBuildHasher;
use indexmap::IndexMap;

/// The `reason` texts of `TraverserError::InvalidModulator` for `dedup()`; its `help()` keys on
/// them.
pub(crate) mod reason {
    /// A `by()` kind that projects nothing a traverser can be deduplicated by.
    pub(crate) const KIND: &str = "an order or Count is not a projection dedup() can key by";
    /// A second `by()`: `dedup()` keys by one projection.
    pub(crate) const TOO_MANY: &str = "dedup() takes a single by()";
}

/// `dedup()`: keeps the first traverser of each distinct key, in stream order.
///
/// Without `by()` the key is the traverser's own object; with `by()` it is that object's
/// projection (TinkerPop `DedupGlobalStep`). A traverser whose projection yields nothing (such as
/// `by("age")` on a vertex without `age`) is filtered out, TinkerPop 3.6+'s "productive `by()`"
/// rule.
struct StepDedup<'g, S: GraphStorage + 'g> {
    /// Every `by()` received, in order. Only the first is used; any further one is rejected (via
    /// `by_check`) but kept, so the plan in an error or `.profile()` shows what was written.
    by: Vec<By<'g, S>>,
    by_check: ByModulatorCheck,
    /// `dedup("a", "b")`: the path labels whose values (each projected by the `by()`) form the
    /// key, instead of the traverser itself. Empty for plain `dedup()`.
    pub(crate) labels: Vec<CompactString>,
}

impl<'g, S: GraphStorage + 'g> Default for StepDedup<'g, S> {
    fn default() -> Self {
        Self {
            by: Vec::new(),
            by_check: ByModulatorCheck::default(),
            labels: Vec::new(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> Clone for StepDedup<'g, S> {
    fn clone(&self) -> Self {
        Self {
            by: self.by.clone(),
            by_check: self.by_check.clone(),
            labels: self.labels.clone(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepDedup<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.labels.is_empty() {
            write!(f, "dedup()")?;
        } else {
            write!(
                f,
                "dedup({})",
                crate::value::literal::rhai_strings(&self.labels)
            )?;
        }
        for by in &self.by {
            f.write_str(&crate::traversal::render_by(by))?;
        }
        Ok(())
    }
}

/// Dedup key for [`StepDedup`]. `Vertex`/`Edge` traversers carry an identity (their storage id)
/// already, so they key by that cheap `Copy` handle directly — no property access, no
/// allocation. A property *element* (what `properties()` yields, [`PropertyHandleKind::Element`])
/// keys like TinkerPop's property equality: a vertex property by its identity (the owning vertex
/// and the key; a graph holds one value per key, so that pair is TinkerPop's vertex-property id),
/// so three `josh` names on three vertices stay three; an edge property by its key and value
/// (TinkerPop's `Property.equals`), so the same weight on two edges is one.
/// Every other [`TraverserValue`] variant (e.g. after `.values(...)`, whose lazy handles are plain
/// values) keys by its fully materialized value, since deduplicating those is legitimately
/// value-based.
///
/// `PartialEq`/`Eq`/`Hash` are implemented by hand rather than derived: a derive would add a
/// spurious `S: PartialEq + Eq + Hash` bound on the storage type itself, when only
/// `S::VertexId`/`S::EdgeId` (already required to be `Eq + Hash + Copy` by [`GraphStorage`]) need
/// to satisfy it.
pub(crate) enum DedupKey<S: GraphStorage> {
    Vertex(S::VertexId),
    Edge(S::EdgeId),
    Value(ElementProperty),
    /// A vertex property element: owning vertex and key.
    VertexProperty(S::VertexId, CompactString),
    /// An edge property element: key and value (TinkerPop `Property.equals`).
    EdgeProperty(CompactString, ElementProperty),
    /// `dedup("a", "b")`: one key per path label, in label order.
    Composite(Vec<DedupKey<S>>),
}

impl<S: GraphStorage> PartialEq for DedupKey<S> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (DedupKey::Vertex(a), DedupKey::Vertex(b)) => a == b,
            (DedupKey::Edge(a), DedupKey::Edge(b)) => a == b,
            (DedupKey::Value(a), DedupKey::Value(b)) => a == b,
            (DedupKey::VertexProperty(a, ka), DedupKey::VertexProperty(b, kb)) => {
                a == b && ka == kb
            }
            (DedupKey::EdgeProperty(ka, a), DedupKey::EdgeProperty(kb, b)) => ka == kb && a == b,
            (DedupKey::Composite(a), DedupKey::Composite(b)) => a == b,
            _ => false,
        }
    }
}

impl<S: GraphStorage> Eq for DedupKey<S> {}

impl<S: GraphStorage> Clone for DedupKey<S> {
    fn clone(&self) -> Self {
        match self {
            DedupKey::Vertex(id) => DedupKey::Vertex(*id),
            DedupKey::Edge(id) => DedupKey::Edge(*id),
            DedupKey::Value(value) => DedupKey::Value(value.clone()),
            DedupKey::VertexProperty(id, key) => DedupKey::VertexProperty(*id, key.clone()),
            DedupKey::EdgeProperty(key, value) => {
                DedupKey::EdgeProperty(key.clone(), value.clone())
            }
            DedupKey::Composite(keys) => DedupKey::Composite(keys.clone()),
        }
    }
}

impl<S: GraphStorage> std::hash::Hash for DedupKey<S> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            DedupKey::Vertex(id) => {
                0u8.hash(state);
                id.hash(state);
            }
            DedupKey::Edge(id) => {
                1u8.hash(state);
                id.hash(state);
            }
            DedupKey::Value(value) => {
                2u8.hash(state);
                value.hash(state);
            }
            DedupKey::VertexProperty(id, key) => {
                3u8.hash(state);
                id.hash(state);
                key.hash(state);
            }
            DedupKey::EdgeProperty(key, value) => {
                4u8.hash(state);
                key.hash(state);
                value.hash(state);
            }
            DedupKey::Composite(keys) => {
                5u8.hash(state);
                keys.hash(state);
            }
        }
    }
}

impl<S: GraphStorage> DedupKey<S> {
    /// The key of one stream value: the identity of a `Vertex`/`Edge`/property element, the
    /// materialized value of anything else.
    #[inline]
    pub(crate) fn of(
        context: &TraverserContext<'_, S>,
        value: &TraverserValue<S>,
    ) -> TraverserResult<Self> {
        Ok(match value {
            TraverserValue::Vertex(id) => DedupKey::Vertex(*id),
            TraverserValue::Edge(id) => DedupKey::Edge(*id),
            TraverserValue::VertexProperty(key, id, PropertyHandleKind::Element) => {
                DedupKey::VertexProperty(*id, key.clone())
            }
            TraverserValue::EdgeProperty(key, _, PropertyHandleKind::Element) => {
                DedupKey::EdgeProperty(key.clone(), value.into_node_property(context.storage)?)
            }
            TraverserValue::PathProperty(prop) => {
                DedupKey::EdgeProperty(prop.0.clone(), prop.1.clone())
            }
            _ => DedupKey::Value(value.into_node_property(context.storage)?),
        })
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Removes duplicate traversers from the stream, keeping the first traverser of each distinct
    /// key, in stream order.
    ///
    /// `Vertex`/`Edge` traversers are deduplicated by identity (their storage id) — the common
    /// case after `out()`/`in()`/`both()`/`V()` — without materializing their properties. Every
    /// other traverser (e.g. after `.values(...)`) is deduplicated by its fully materialized
    /// value, since that case is legitimately dedup-by-value. A property element from
    /// `properties()` follows TinkerPop's property equality: a vertex property by identity (its
    /// vertex and key), so equal values on different vertices stay distinct; an edge property by
    /// key and value, so the same weight on two edges is one. `properties().value().dedup()` is
    /// the by-value form.
    ///
    /// Follow it with one `by()` to deduplicate by a projection instead of the object itself: a
    /// property key (`by("name")`), `By::Id`/`By::Label` (`T.id`/`T.label`), a traversal
    /// (`by(__.out().count())`, whose first result is the key) or a JSON path. A traverser whose
    /// projection yields nothing, such as `by("age")` on a vertex without `age`, is filtered out
    /// (TinkerPop's "productive `by()`" rule). A label list, an `Order`, `By::Count` or a second
    /// `by()` fail with `TraverserError::InvalidModulator` when the traversal executes.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let lock = graph.read();
    /// let one_per_label = lock.traversal().v(None).dedup().by(By::Label).count().to_list().unwrap();
    /// assert_eq!(one_per_label[0].as_i64(), Some(2));
    /// ```
    ///
    /// This is a barrier step: it buffers the whole incoming stream before emitting.
    ///
    /// Inside a `repeat()` body, like in TinkerPop, one `dedup()` keeps a single seen-set for the
    /// whole loop: an element met again in a later iteration, or reached by another traverser,
    /// is dropped. A `dedup()` in a per-traverser child of the body (`where`, `local`,
    /// `by(traversal)`, `until`/`emit` conditions) starts fresh for every traverser, and every
    /// `dedup()` step has its own set. Several `dedup()` steps are never merged. Details in the
    /// user's guide page "Recursive Traversals".
    pub fn dedup(self) -> Self {
        self.push_step(GraphStep::FlatBarrier(
            "dedup",
            Box::new(StepDedup::default()),
        ))
    }
}

impl __ {
    /// See [`GraphTraversalSource::dedup`].
    pub fn dedup<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().dedup()
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// See [`GraphTraversalSource::dedup`].
    pub fn dedup(self) -> Self {
        let step = GraphStep::FlatBarrier("dedup", Box::new(StepDedup::default()));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> StepDedup<'g, S> {
    /// The loop-body form: the seen-set of this step instance lives in the innermost `repeat()`
    /// frame, so an element met again in a later iteration (or by another traverser) is dropped.
    /// Survivors keep input order and stand for themselves only (bulk 1).
    fn process_in_loop(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Vec<Traverser<S>>,
    ) -> TraverserResult<Vec<Traverser<S>>> {
        let id = self as *const Self as usize;
        let mut survivors = Vec::with_capacity(source.len());
        for mut traverser in source {
            let Some(key) = self.key_of(context, &traverser)? else {
                continue;
            };
            let Some(frame) = context.loop_stack.last_mut() else {
                break;
            };
            if frame.dedup_seen.entry(id).or_default().insert(key) {
                traverser.bulk = 1;
                survivors.push(traverser);
            }
        }
        Ok(survivors)
    }

    /// The projection of `traverser` by `by`, or `None` when it yields nothing (the traverser is
    /// then filtered out). A traversal keys by its first result, like TinkerPop.
    fn project(
        context: &mut TraverserContext<'_, S>,
        traverser: &Traverser<S>,
        by: &By<'g, S>,
    ) -> TraverserResult<Option<TraverserValue<S>>> {
        TraverserValue::fetch_traverser_value_by(context, traverser, &traverser.value, by)
    }

    /// The dedup key of `traverser`, or `None` when it is filtered out (an unproductive `by()`,
    /// or a label that is not on its path).
    ///
    /// With labels (`dedup("a", "b")`, TinkerPop `DedupGlobalStep` with `dedupLabels`) the key
    /// is the list of the objects last labelled so, each projected by the `by()`, which then
    /// runs on that object as a fresh traverser.
    fn key_of(
        &self,
        context: &mut TraverserContext<'_, S>,
        traverser: &Traverser<S>,
    ) -> TraverserResult<Option<DedupKey<S>>> {
        if self.labels.is_empty() {
            return Ok(match self.by.first() {
                None => Some(DedupKey::of(context, &traverser.value)?),
                Some(by) => match Self::project(context, traverser, by)? {
                    Some(projected) => Some(DedupKey::of(context, &projected)?),
                    None => None,
                },
            });
        }
        let mut keys = Vec::with_capacity(self.labels.len());
        for label in &self.labels {
            let Some(value) = context.paths.by_label(traverser.path, label) else {
                return Ok(None);
            };
            let value = match self.by.first() {
                None => value,
                Some(by) => {
                    let start = Traverser::start(context, value);
                    match Self::project(context, &start, by)? {
                        Some(projected) => projected,
                        None => return Ok(None),
                    }
                }
            };
            keys.push(DedupKey::of(context, &value)?);
        }
        Ok(Some(DedupKey::Composite(keys)))
    }
}

/// `dedup("a", "b", ...)`: [`GraphTraversalSource::dedup`] keyed by the values of path labels.
pub(crate) fn dedup_labels_step<'g, S: GraphStorage + 'g>(
    labels: Vec<CompactString>,
) -> GraphStep<'g, S> {
    GraphStep::FlatBarrier(
        "dedup",
        Box::new(StepDedup {
            labels,
            ..StepDedup::default()
        }),
    )
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatBarrier<'g, S> for StepDedup<'g, S> {
    fn property_access(
        &self,
        visit: &mut dyn FnMut(Option<&str>, crate::graph::workload::AccessKind),
    ) {
        for key in self.by.iter().filter_map(By::property_key) {
            visit(Some(key), crate::graph::workload::AccessKind::OrderGroup);
        }
    }
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Vec<Traverser<S>>,
    ) -> TraverserResult<Option<Vec<Traverser<S>>>> {
        self.by_check.check("dedup")?;
        if context.loop_persist && !context.loop_stack.is_empty() {
            return self.process_in_loop(context, source).map(Some);
        }
        let mut result: IndexMap<DedupKey<S>, Traverser<S>, FnvBuildHasher> =
            IndexMap::with_capacity_and_hasher(source.len(), FnvBuildHasher::default());
        for r in source {
            let Some(key) = self.key_of(context, &r)? else {
                continue;
            };
            result.entry(key).or_insert(r);
        }

        // Survivors stand for themselves only: TinkerPop's `dedup()` resets the bulk to 1.
        Ok(Some(
            result
                .into_values()
                .map(|mut survivor| {
                    survivor.bulk = 1;
                    survivor
                })
                .collect(),
        ))
    }

    fn push_modulator_by(&mut self, args: By<'g, S>, _modulator_name: &str) -> ByModulation {
        self.by_check.observe(&args);
        match &args {
            By::Order(_) | By::Count => self.by_check.reject_last(reason::KIND),
            By::Property(_)
            | By::Traversal(_)
            | By::Label
            | By::Id
            | By::Value
            | By::Identity
            | By::Key
            | By::JsonPath(_)
            | By::Labels(_)
            | By::Column(_) => {}
        }
        if !self.by.is_empty() {
            self.by_check.reject_last(reason::TOO_MANY);
        }
        self.by.push(args);
        ByModulation::Accepted
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut crate::traversal::AnonymousTraversal<'g, S>> {
        self.by.iter_mut().filter_map(By::traversal_mut).collect()
    }

    /// `dedup("a", "b")` reads the objects labelled `a` and `b` from the path.
    fn path_requirement(&self) -> PathRequirement {
        if self.labels.is_empty() {
            PathRequirement::None
        } else {
            PathRequirement::labels(self.labels.iter().cloned())
        }
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &PathAnalysis,
        _downstream: &PathRequirement,
    ) -> PathRequirement {
        // A `by(__...)` child runs on the incoming traverser, path included.
        By::analyze_all_paths(&mut self.by, analysis)
    }

    fn clone_box(&self) -> Box<dyn GraphStepFlatBarrier<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).out(None).in_(None).dedup().count().to_list(),
            rhai: [
                r#"g.V().out().in().dedup().count()"#,
                r#"g.v().out().in().dedup().count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(3)),
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g
            .v(None)
            .out(None)
            .in_(None)
            .dedup()
            .count()
            .profile()
            .unwrap();
        assert!(result.to_string().contains("dedup("));
    }

    /// D1 regression: `dedup()` on a `Vertex` stream with genuine duplicates produced by two
    /// paths converging on the same vertex (`union(both(), both())` from `marko`, each branch
    /// reaching `vadas`/`josh`/`lop`) must key by vertex identity and keep exactly one survivor
    /// per distinct vertex, unaffected by the switch away from full property materialization.
    #[test]
    fn identity_keyed_dedup_on_converging_vertex_paths() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v("1")
                .union(vec![__::both(None), __::both(None)])
                .dedup()
                .values("name")
                .to_list(),
            rhai: [
                r#"g.V("1").union([__.both(), __.both()]).dedup().values("name")"#,
                r#"g.v("1").union([__.both(), __.both()]).dedup().values("name")"#,
            ],
            assert: |res| {
                let mut names: Vec<String> = res
                    .iter()
                    .map(|v| v.as_str().expect("name is a string").to_string())
                    .collect();
                names.sort();
                assert_eq!(names, vec!["josh", "lop", "vadas"]);
            },
        );
    }

    /// D1 regression: the same convergence, but on an `Edge` stream (`bothE()` from two branches
    /// landing on the same edges), must key by edge identity rather than materializing each
    /// edge's properties.
    #[test]
    fn identity_keyed_dedup_on_converging_edge_paths() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v("1")
                .union(vec![__::both_e(None), __::both_e(None)])
                .dedup()
                .count()
                .to_list(),
            rhai: [
                r#"g.V("1").union([__.bothE(), __.bothE()]).dedup().count()"#,
                r#"g.v("1").union([__.bothE(), __.bothE()]).dedup().count()"#,
            ],
            // marko ("1") has 3 incident edges (2 "knows", 1 "created"); each union branch
            // reaches all 3, doubling them to 6 before dedup, 3 distinct survivors after.
            assert: |res| assert_eq!(res[0].as_i64(), Some(3)),
        );
    }

    #[test]
    fn labels() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).label().dedup().count().to_list(),
            rhai: [
                r#"g.V().label().dedup().count()"#,
                r#"g.v().label().dedup().count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
    }
}
