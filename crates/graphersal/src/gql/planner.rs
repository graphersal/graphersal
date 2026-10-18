//! The planner: turns a [`BoundQuery`] into a [`LogicalPlan`] (stage 1a).
//!
//! It reads the graph only through [`PlannerFacts`] (vertex label counts and which vertex
//! property indexes exist), which [`StorageFacts::from_storage`] builds from any
//! [`GraphStorage`] through `statistics()` and `property_indexes()`, never `TraversalGraph`
//! internals; tests pass their own facts. The plan is deterministic for a given query and facts:
//! every choice is a minimum over a total order ending in the text order of the bindings or
//! pattern edges.
//!
//! # Start node of a component
//!
//! Every node binding of the component is a candidate, ranked by (lower wins):
//!
//! 1. its level: `0` an equality conjunct (`=` or `IN`, pushable) on a key that a property
//!    index serves for every one of its scan labels ([`Scan::scan_labels`]); `1` scan labels and
//!    a pushable equality conjunct; `2` scan labels; `3` anything else;
//! 2. its estimated size from the label counts (unknown: last);
//! 3. its binding id (the pattern's first node wins a tie).
//!
//! The phase 2 anchor, when given, starts its component regardless.
//!
//! # Walk order
//!
//! From the start, the pattern edges with a bound end are walked greedily, ranked by: an edge
//! whose both ends are bound first (it closes a cycle, an [`Op::Join`] check, the earliest
//! filter there is); then the far node's filters (own conjuncts before label expressions before
//! none); then continuing from the current element before branching back to an earlier node
//! (`select(x)`, which needs a label); then the text order of the pattern edges.
//!
//! # Components
//!
//! The components of the pattern graph (across all `MATCH` clauses) are emitted best start first
//! (the same rank), so the cheapest, possibly index-served component is the root scan; the
//! others follow as mid-traversal scans (a Cartesian product).
//!
//! # Checks and filters
//!
//! - An expansion reaching a node bound earlier is followed by an [`Op::Join`]; an edge binding
//!   bound earlier (the same edge variable in an earlier `MATCH`) sets [`Expand::edge_join`].
//! - Trail (Phase 0 Q8): each walked edge lists the earlier walked edges of the same `MATCH`
//!   clause whose label sets may overlap ([`EdgeLabels::may_overlap`]).
//! - A conjunct over zero or several bindings is an [`Op::Filter`] right after the operation
//!   that binds the last binding it reads (one with no binding right after the first scan).

use super::ast::EdgeDirection;
use super::bind::{BindingId, BindingKind, BoundQuery};
use super::plan::{
    EdgeLabels, ElementFilters, Expand, Labelling, LogicalPlan, Op, Pushdown, ResultOp, Scan,
    and_factors, classify_all, element_filters, scan_labels,
};
use crate::graph::ElementKind;
use crate::graph::GraphStatistics;
use crate::graph::storage::GraphStorage;

/// What the planner knows about the graph.
pub(crate) trait PlannerFacts {
    /// The number of vertices carrying `label`; `None` when unknown.
    fn vertex_label_count(&self, label: &str) -> Option<u64>;
    /// The number of vertices; `None` when unknown.
    fn vertex_count(&self) -> Option<u64>;
    /// Whether a property index on `(vertex, label, key)` (no transform) serves lookups.
    fn has_vertex_index(&self, label: &str, key: &str) -> bool;
}

/// No facts at all: every count unknown, no index.
#[derive(Debug, Clone, Copy, Default)]
#[cfg_attr(not(test), allow(dead_code, reason = "used by the tests"))]
pub(crate) struct NoFacts;

impl PlannerFacts for NoFacts {
    fn vertex_label_count(&self, _label: &str) -> Option<u64> {
        None
    }
    fn vertex_count(&self) -> Option<u64> {
        None
    }
    fn has_vertex_index(&self, _label: &str, _key: &str) -> bool {
        false
    }
}

/// The facts of a [`GraphStorage`]: its `statistics()` (when it keeps them) and the
/// untransformed vertex indexes of `property_indexes()`.
#[derive(Debug, Clone)]
pub(crate) struct StorageFacts<'s> {
    statistics: Option<&'s GraphStatistics>,
    /// `(label, key)` of every untransformed vertex property index, sorted.
    indexes: Vec<(String, String)>,
}

impl<'s> StorageFacts<'s> {
    /// Reads the facts of `storage` (cheap: no walk over the data).
    pub(crate) fn from_storage<S: GraphStorage + ?Sized>(storage: &'s S) -> Self {
        let mut indexes: Vec<(String, String)> = storage
            .property_indexes()
            .into_iter()
            .filter(|info| info.element == ElementKind::Vertex && info.transform.is_none())
            .map(|info| (info.label, info.key))
            .collect();
        indexes.sort_unstable();
        indexes.dedup();
        StorageFacts {
            statistics: storage.statistics(),
            indexes,
        }
    }
}

impl PlannerFacts for StorageFacts<'_> {
    fn vertex_label_count(&self, label: &str) -> Option<u64> {
        self.statistics
            .map(|s| u64::try_from(s.vertex_label_count(label)).unwrap_or(u64::MAX))
    }
    fn vertex_count(&self) -> Option<u64> {
        self.statistics
            .map(|s| u64::try_from(s.vertex_count()).unwrap_or(u64::MAX))
    }
    fn has_vertex_index(&self, label: &str, key: &str) -> bool {
        self.indexes
            .binary_search_by(|(l, k)| (l.as_str(), k.as_str()).cmp(&(label, key)))
            .is_ok()
    }
}

/// One occurrence of an edge in a path pattern, as written.
#[derive(Debug, Clone, Copy)]
struct PatternEdge {
    clause: usize,
    source: BindingId,
    edge: BindingId,
    direction: EdgeDirection,
    target: BindingId,
}

/// Plans `bound` with `facts`; `anchor` is the phase 2 anchor binding (`None` in stage 1a).
pub(crate) fn plan(
    bound: &BoundQuery,
    facts: &dyn PlannerFacts,
    anchor: Option<BindingId>,
) -> LogicalPlan {
    Planner::new(bound, facts).run(anchor)
}

struct Planner<'q> {
    bound: &'q BoundQuery,
    facts: &'q dyn PlannerFacts,
    pushdown: Vec<Option<Pushdown>>,
    edges: Vec<PatternEdge>,
    bound_set: Vec<bool>,
    labelling: Vec<Labelling>,
    ops: Vec<Op>,
    /// Cross conjuncts not placed yet.
    pending: Vec<usize>,
    /// The walked edge bindings per clause, in walk order.
    walked: Vec<Vec<BindingId>>,
}

impl<'q> Planner<'q> {
    fn new(bound: &'q BoundQuery, facts: &'q dyn PlannerFacts) -> Self {
        let mut edges = Vec::new();
        for (clause, m) in bound.matches.iter().enumerate() {
            for path in &m.paths {
                let mut source = path.start;
                for step in &path.steps {
                    edges.push(PatternEdge {
                        clause,
                        source,
                        edge: step.edge,
                        direction: step.direction,
                        target: step.node,
                    });
                    source = step.node;
                }
            }
        }
        let n = bound.bindings.len();
        Planner {
            bound,
            facts,
            pushdown: classify_all(bound),
            edges,
            bound_set: vec![false; n],
            labelling: bound
                .bindings
                .iter()
                .map(|b| Labelling {
                    read: b.read,
                    ..Labelling::default()
                })
                .collect(),
            ops: Vec::new(),
            pending: bound.cross.clone(),
            walked: vec![Vec::new(); bound.matches.len()],
        }
    }

    fn run(mut self, anchor: Option<BindingId>) -> LogicalPlan {
        let anchor = anchor.filter(|a| self.is_node(*a));
        // Each component with its start: the anchor's component first, then best start first
        // (level, estimated size, binding id).
        let mut starts: Vec<((bool, u8, u64, BindingId), Vec<BindingId>)> = self
            .components()
            .into_iter()
            .map(|nodes| {
                let key = match anchor.filter(|a| nodes.contains(a)) {
                    Some(a) => (false, 0, 0, a),
                    None => {
                        let (level, size, id) = nodes
                            .iter()
                            .map(|&id| self.start_rank(id))
                            .min()
                            .unwrap_or((u8::MAX, u64::MAX, BindingId(usize::MAX)));
                        (true, level, size, id)
                    }
                };
                (key, nodes)
            })
            .collect();
        starts.sort_unstable_by_key(|(key, _)| *key);
        for ((not_anchor, _, _, start), nodes) in starts {
            self.walk_component(start, &nodes, !not_anchor);
        }
        // Conjuncts left over (none in a well-formed query: every binding is bound).
        if !self.pending.is_empty() {
            let rest = std::mem::take(&mut self.pending);
            self.ops.push(Op::Filter(rest));
        }
        let result = self.result();
        LogicalPlan {
            ops: self.ops,
            result,
            labelling: self.labelling,
            pushdown: self.pushdown,
            anchor,
        }
    }

    fn is_node(&self, id: BindingId) -> bool {
        self.bound
            .binding(id)
            .is_some_and(|b| b.kind == BindingKind::Node)
    }

    /// The node bindings of each connected component, each sorted, components in order of
    /// their smallest node binding.
    fn components(&self) -> Vec<Vec<BindingId>> {
        let n = self.bound.bindings.len();
        let mut parent: Vec<usize> = (0..n).collect();
        fn find(parent: &mut [usize], mut x: usize) -> usize {
            while let Some(&p) = parent.get(x) {
                if p == x {
                    break;
                }
                let grand = parent.get(p).copied().unwrap_or(p);
                if let Some(slot) = parent.get_mut(x) {
                    *slot = grand;
                }
                x = p;
            }
            x
        }
        let union = |parent: &mut Vec<usize>, a: usize, b: usize| {
            let (ra, rb) = (find(parent, a), find(parent, b));
            if ra != rb {
                let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
                if let Some(slot) = parent.get_mut(hi) {
                    *slot = lo;
                }
            }
        };
        for e in &self.edges {
            union(&mut parent, e.source.0, e.edge.0);
            union(&mut parent, e.edge.0, e.target.0);
        }
        let mut components: Vec<(usize, Vec<BindingId>)> = Vec::new();
        for b in &self.bound.bindings {
            if b.kind != BindingKind::Node {
                continue;
            }
            let root = find(&mut parent, b.id.0);
            match components.iter_mut().find(|(r, _)| *r == root) {
                Some((_, nodes)) => nodes.push(b.id),
                None => components.push((root, vec![b.id])),
            }
        }
        components.into_iter().map(|(_, nodes)| nodes).collect()
    }

    /// The start rank of node `id` (module docs): level, estimated size, id.
    fn start_rank(&self, id: BindingId) -> (u8, u64, BindingId) {
        let Some(binding) = self.bound.binding(id) else {
            return (u8::MAX, u64::MAX, id);
        };
        let labels = scan_labels(&binding.labels);
        let equality_keys: Vec<&str> = binding
            .predicates
            .iter()
            .filter_map(|&c| self.pushdown.get(c).and_then(Option::as_ref))
            .filter(|p| p.op.is_equality())
            .map(|p| p.key.as_str())
            .collect();
        let level = match &labels {
            Some(labels) => {
                let indexed = equality_keys.iter().any(|key| {
                    labels
                        .iter()
                        .all(|label| self.facts.has_vertex_index(label, key))
                });
                if indexed {
                    0
                } else if !equality_keys.is_empty() {
                    1
                } else {
                    2
                }
            }
            None => 3,
        };
        (level, self.estimate(id).unwrap_or(u64::MAX), id)
    }

    /// An upper bound of the vertices node `id` can match, from its label expressions.
    fn estimate(&self, id: BindingId) -> Option<u64> {
        let binding = self.bound.binding(id)?;
        let factors = and_factors(&binding.labels);
        let known = factors
            .into_iter()
            .filter_map(|f| self.estimate_expr(f))
            .min();
        known.or_else(|| self.facts.vertex_count())
    }

    fn estimate_expr(&self, expr: &super::ast::LabelExpr) -> Option<u64> {
        use super::ast::LabelExprKind;
        match &expr.kind {
            LabelExprKind::Label(id) => self.facts.vertex_label_count(&id.name),
            LabelExprKind::Or(l, r) => Some(
                self.estimate_expr(l)?
                    .saturating_add(self.estimate_expr(r)?),
            ),
            LabelExprKind::And(l, r) => match (self.estimate_expr(l), self.estimate_expr(r)) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            },
            LabelExprKind::Wildcard | LabelExprKind::Not(_) => self.facts.vertex_count(),
        }
    }

    /// The filters of `id` where it is bound first (vertex scans know the indexes).
    fn filters_of(&self, id: BindingId, scan: Option<&[String]>) -> ElementFilters {
        let Some(binding) = self.bound.binding(id) else {
            return ElementFilters::default();
        };
        element_filters(&binding.predicates, &self.pushdown, |key| {
            scan.is_some_and(|labels| {
                !labels.is_empty()
                    && labels
                        .iter()
                        .all(|label| self.facts.has_vertex_index(label, key))
            })
        })
    }

    fn bind(&mut self, id: BindingId) {
        if let Some(slot) = self.bound_set.get_mut(id.0) {
            *slot = true;
        }
    }

    fn is_bound(&self, id: BindingId) -> bool {
        self.bound_set.get(id.0).copied().unwrap_or(false)
    }

    fn mark(&mut self, id: BindingId, f: impl FnOnce(&mut Labelling)) {
        if let Some(l) = self.labelling.get_mut(id.0) {
            f(l);
        }
    }

    /// Emits the filters whose bindings are all bound now.
    fn place_filters(&mut self) {
        let mut ready = Vec::new();
        let mut rest = Vec::new();
        for c in std::mem::take(&mut self.pending) {
            let all = self
                .bound
                .conjuncts
                .get(c)
                .is_some_and(|cj| cj.bindings.iter().all(|&b| self.is_bound(b)));
            if all {
                ready.push(c);
            } else {
                rest.push(c);
            }
        }
        self.pending = rest;
        if !ready.is_empty() {
            self.ops.push(Op::Filter(ready));
        }
    }

    /// How much the node `id`'s own filters narrow (lower: more).
    fn filter_rank(&self, id: BindingId) -> u8 {
        match self.bound.binding(id) {
            Some(b) if !b.predicates.is_empty() => 0,
            Some(b) if !b.labels.is_empty() => 1,
            _ => 2,
        }
    }

    fn walk_component(&mut self, start: BindingId, nodes: &[BindingId], anchored: bool) {
        let labels = self
            .bound
            .binding(start)
            .and_then(|b| scan_labels(&b.labels));
        let scan = Scan {
            binding: start,
            filters: self.filters_of(start, if anchored { None } else { labels.as_deref() }),
            scan_labels: labels,
        };
        self.ops.push(if anchored {
            Op::Anchor(scan)
        } else {
            Op::Scan(scan)
        });
        self.bind(start);
        self.place_filters();
        let mut current = start;
        // Membership by binding id: `nodes.contains` per edge would be quadratic.
        let mut in_component = vec![false; self.bound.bindings.len()];
        for node in nodes {
            if let Some(slot) = in_component.get_mut(node.0) {
                *slot = true;
            }
        }
        let mut todo: Vec<usize> = (0..self.edges.len())
            .filter(|&i| {
                self.edges
                    .get(i)
                    .is_some_and(|e| in_component.get(e.source.0).copied().unwrap_or(false))
            })
            .collect();
        loop {
            while let Some(pick) = self.pick(&todo, current) {
                todo.retain(|&i| i != pick);
                let Some(edge) = self.edges.get(pick).copied() else {
                    break;
                };
                current = self.expand(edge, current);
            }
            // Pattern edges left over reach the walked part only through an edge bound
            // earlier (the same edge variable in another MATCH): scan their best node again;
            // the expansion then checks the edge (`edge_join`).
            let Some(restart) = todo
                .iter()
                .filter_map(|&i| self.edges.get(i))
                .flat_map(|e| [e.source, e.target])
                .filter(|&n| !self.is_bound(n))
                .map(|n| self.start_rank(n))
                .min()
                .map(|(_, _, id)| id)
            else {
                break;
            };
            let labels = self
                .bound
                .binding(restart)
                .and_then(|b| scan_labels(&b.labels));
            let filters = self.filters_of(restart, labels.as_deref());
            self.ops.push(Op::Scan(Scan {
                binding: restart,
                scan_labels: labels,
                filters,
            }));
            self.bind(restart);
            self.place_filters();
            current = restart;
        }
    }

    /// The next pattern edge to walk among `todo` (module docs), with a bound end.
    fn pick(&self, todo: &[usize], current: BindingId) -> Option<usize> {
        todo.iter()
            .filter_map(|&i| {
                let e = self.edges.get(i)?;
                let (s, t) = (self.is_bound(e.source), self.is_bound(e.target));
                if !s && !t {
                    return None;
                }
                let closing = s && t;
                let (origin, far) = self.orient(e, current);
                let far_rank = if closing { 0 } else { self.filter_rank(far) };
                Some((
                    (u8::from(!closing), far_rank, u8::from(origin != current), i),
                    i,
                ))
            })
            .min_by_key(|(key, _)| *key)
            .map(|(_, i)| i)
    }

    /// The walk origin and the far end of `e`: from the current element when it is an end,
    /// else from a bound end (the written source first).
    fn orient(&self, e: &PatternEdge, current: BindingId) -> (BindingId, BindingId) {
        if e.source == current {
            (e.source, e.target)
        } else if e.target == current {
            (e.target, e.source)
        } else if self.is_bound(e.source) {
            (e.source, e.target)
        } else {
            (e.target, e.source)
        }
    }

    /// Emits the expansion of `e`; returns the new current element.
    fn expand(&mut self, e: PatternEdge, current: BindingId) -> BindingId {
        let (from, to) = self.orient(&e, current);
        let direction = if from == e.source {
            e.direction
        } else {
            match e.direction {
                EdgeDirection::Right => EdgeDirection::Left,
                EdgeDirection::Left => EdgeDirection::Right,
                EdgeDirection::Any => EdgeDirection::Any,
            }
        };
        let from_current = from == current;
        if !from_current {
            self.mark(from, |l| l.branch = true);
        }
        let edge_join = self.is_bound(e.edge);
        if edge_join {
            self.mark(e.edge, |l| l.join = true);
        }
        let my_labels = self
            .bound
            .binding(e.edge)
            .map_or(EdgeLabels::Any, |b| EdgeLabels::of(&b.labels));
        let earlier: Vec<BindingId> = self
            .walked
            .get(e.clause)
            .map(|w| {
                w.iter()
                    .copied()
                    .filter(|&other| other != e.edge)
                    .filter(|&other| {
                        self.bound
                            .binding(other)
                            .is_some_and(|b| EdgeLabels::of(&b.labels).may_overlap(&my_labels))
                    })
                    .collect()
            })
            .unwrap_or_default();
        for &other in &earlier {
            self.mark(other, |l| l.trail = true);
        }
        if let Some(w) = self.walked.get_mut(e.clause) {
            w.push(e.edge);
        }
        let edge_filters = if edge_join {
            ElementFilters::default()
        } else {
            self.filters_of(e.edge, None)
        };
        let to_bound = self.is_bound(to);
        let to_filters = if to_bound {
            ElementFilters::default()
        } else {
            self.filters_of(to, None)
        };
        self.ops.push(Op::Expand(Expand {
            from,
            from_current,
            edge: e.edge,
            direction,
            edge_join,
            trail: earlier,
            edge_filters,
            to,
            to_bound,
            to_filters,
        }));
        self.bind(e.edge);
        if to_bound {
            self.mark(to, |l| l.join = true);
            self.ops.push(Op::Join { binding: to });
        }
        self.bind(to);
        self.place_filters();
        to
    }

    fn result(&self) -> Vec<ResultOp> {
        let mut result = vec![ResultOp::Project];
        if self.bound.distinct {
            result.push(ResultOp::Distinct);
        }
        if !self.bound.order_by.is_empty() {
            result.push(ResultOp::Order);
        }
        if let Some(offset) = &self.bound.offset {
            result.push(ResultOp::Offset(offset.clone()));
        }
        if let Some(limit) = &self.bound.limit {
            result.push(ResultOp::Limit(limit.clone()));
        }
        result
    }
}

#[cfg(test)]
mod tests;
