//! The planner: turns a [`BoundQuery`] into a [`LogicalPlan`].
//!
//! It reads the graph only through [`PlannerFacts`] (vertex label counts and which vertex
//! property indexes exist), which [`StorageFacts::from_storage`] builds from any
//! [`GraphStorage`] through `statistics()` and `property_indexes()`, never `TraversalGraph`
//! internals; tests pass their own facts. The plan is deterministic for a given query and facts:
//! every choice is a minimum over a total order ending in the text order of the bindings or
//! pattern edges.
//!
//! # Segments (stage 1b, plan mechanism 2)
//!
//! The planner plans the [`BoundQuery::segments`] in order, each with the rules below over the
//! pattern graph of its own clauses; the first segment's plan is [`LogicalPlan::ops`] (for a
//! stage 1a query, the only segment: the plan is exactly the stage 1a plan), every later one a
//! [`Stage`]. The bindings bound by earlier segments are bound already when a segment starts:
//!
//! - a component of a later segment holding such a node starts there ([`Op::Resume`]): at the
//!   current element when it is one (an `OPTIONAL MATCH` continuing from where the segment
//!   before it ended), else at the earliest such node by binding id; components with such a
//!   start come first (current first), then the others as mid-traversal scans ranked as below;
//! - an expansion reaching such a node is a join, an edge variable bound before it an edge join;
//! - the segment's conjuncts that read only bindings bound before it are an [`Op::Filter`] at
//!   the start of the segment (the first segment keeps the stage 1a rule: after the first scan);
//! - a materializing `WITH` is a [`Stage::Materialize`]: its result pipeline (`Aggregate` when
//!   a column aggregates, else `Project`; then `DISTINCT`, `ORDER BY`, `OFFSET`, `LIMIT`) and
//!   its `WHERE` conjuncts; its columns are bound after it.
//!
//! The current element after a segment is the one its last operation reached; after an
//! `OPTIONAL MATCH` or a materializing `WITH` there is none.
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
//! From the start, the pattern edges with a bound end are walked greedily, ranked by: a plain
//! edge before a quantified one (a loop multiplies the rows most); then an edge whose both ends
//! are bound first (it closes a cycle, an [`Op::Join`] check, the earliest
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
//! - A quantified edge walked from its written target reverses its direction like a plain
//!   one ([`Expand::reversed`]); its trail list holds the earlier edges as for a plain edge.
//!
//! # Labelling
//!
//! Besides the stage 1a reasons (read, join, trail, branch) the planner records: a later
//! segment's start away from the current element (`resume`), every group variable (`group`),
//! every node and edge of a read path variable's pattern (`path`), and, from a pass over the
//! finished plan in execution order, every read of a pattern binding after an `OPTIONAL MATCH`
//! part that ended after the binding was bound (`optional`; the witness of a part whose group
//! variable is read after it too) and every read of a materializing `WITH` column after a later
//! stage left its row ([`Materialize::labelled`]).

use super::ast::{EdgeDirection, Quantifier};
use super::bind::{BindingId, BindingKind, BoundQuery, BoundWith, Count, Segment};
use super::plan::{
    EdgeLabels, ElementFilters, Expand, Labelling, LogicalPlan, Materialize, Op, Pushdown,
    ResultOp, Resume, Scan, Stage, and_factors, classify_all, element_filters, scan_labels,
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
    quantifier: Option<Quantifier>,
}

/// How a component starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Start {
    /// A scan of every vertex.
    Scan,
    /// The phase 2 anchor.
    Anchor,
    /// A node an earlier segment bound ([`Op::Resume`]).
    Resume {
        /// It is the current element.
        current: bool,
    },
}

/// Plans `bound` with `facts`; `anchor` is the phase 2 anchor binding (`None` in stage 1a).
pub(crate) fn plan(
    bound: &BoundQuery,
    facts: &dyn PlannerFacts,
    anchor: Option<BindingId>,
) -> LogicalPlan {
    Planner::new(bound, facts).run(anchor)
}

/// The order of a component's start: not started at a bound node (the anchor or a node bound
/// before the segment), level (for a bound start: 0 current, 1 other), estimated size, binding
/// id.
type StartKey = (bool, u8, u64, BindingId);

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
                        quantifier: step.quantifier,
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
                    read: b.read && b.column.is_none(),
                    ..Labelling::default()
                })
                .collect(),
            ops: Vec::new(),
            pending: Vec::new(),
            walked: vec![Vec::new(); bound.matches.len()],
        }
    }

    fn run(mut self, anchor: Option<BindingId>) -> LogicalPlan {
        let anchor = anchor.filter(|a| self.is_node(*a));
        let bound = self.bound;
        let mut first = Vec::new();
        let mut stages = Vec::new();
        let mut current: Option<BindingId> = None;
        for (index, segment) in bound.segments.iter().enumerate() {
            match segment {
                Segment::Match(clauses) => {
                    let anchor = anchor.filter(|_| index == 0);
                    let ops = self.segment(index, clauses, current, anchor);
                    current = current_after(&ops, current);
                    if index == 0 {
                        first = ops;
                    } else {
                        stages.push(Stage::Match {
                            segment: index,
                            ops,
                        });
                    }
                }
                Segment::Optional(clause) => {
                    let ops = self.segment(index, std::slice::from_ref(clause), current, None);
                    stages.push(Stage::Optional {
                        segment: index,
                        ops,
                        witness: self.witness(index),
                    });
                    current = None;
                }
                Segment::With(with) => {
                    stages.push(Stage::Materialize(self.materialize(index, with)));
                    current = None;
                }
            }
        }
        self.mark_static();
        self.mark_positions(&first, &mut stages);
        let result = result_ops(
            bound.aggregated(),
            bound.distinct,
            !bound.order_by.is_empty(),
            &bound.offset,
            &bound.limit,
        );
        LogicalPlan {
            ops: first,
            stages,
            result,
            labelling: self.labelling,
            pushdown: self.pushdown,
            anchor,
        }
    }

    /// Plans segment `index` of `clauses` (module docs): its components best start first, its
    /// conjuncts, the leftovers at the end. `current` is the current element when it starts.
    fn segment(
        &mut self,
        index: usize,
        clauses: &[usize],
        current: Option<BindingId>,
        anchor: Option<BindingId>,
    ) -> Vec<Op> {
        let bound = self.bound;
        self.pending = bound
            .cross
            .iter()
            .copied()
            .filter(|&c| bound.conjuncts.get(c).is_some_and(|cj| cj.segment == index))
            .collect();
        if index > 0 {
            self.place_filters();
        }
        let n = bound.bindings.len();
        // The pattern edges and nodes of the segment's clauses (membership by index, linear).
        let mut in_clauses = vec![false; bound.matches.len()];
        for &clause in clauses {
            if let Some(slot) = in_clauses.get_mut(clause) {
                *slot = true;
            }
        }
        let edges: Vec<usize> = (0..self.edges.len())
            .filter(|&i| {
                self.edges
                    .get(i)
                    .is_some_and(|e| in_clauses.get(e.clause).copied().unwrap_or(false))
            })
            .collect();
        let mut in_pattern = vec![false; n];
        for &clause in clauses {
            for path in bound.matches.get(clause).map_or(&[][..], |m| &m.paths) {
                for node in std::iter::once(path.start).chain(path.steps.iter().map(|s| s.node)) {
                    if let Some(slot) = in_pattern.get_mut(node.0) {
                        *slot = true;
                    }
                }
            }
        }
        let nodes: Vec<BindingId> = bound
            .bindings
            .iter()
            .filter(|b| b.kind == BindingKind::Node && in_pattern.get(b.id.0) == Some(&true))
            .map(|b| b.id)
            .collect();
        // Each component with its start: the anchor's component or one holding a node bound
        // before the segment first (the current element first), then best start first (level,
        // estimated size, binding id).
        let mut starts: Vec<(StartKey, Start, Vec<BindingId>)> = self
            .components(&edges, &nodes)
            .into_iter()
            .map(|nodes| {
                let resume = current
                    .filter(|c| nodes.contains(c) && self.is_bound(*c))
                    .or_else(|| nodes.iter().copied().find(|&id| self.is_bound(id)));
                match (anchor.filter(|a| nodes.contains(a)), resume) {
                    (Some(a), _) => ((false, 0, 0, a), Start::Anchor, nodes),
                    (None, Some(r)) => {
                        let is_current = current == Some(r);
                        (
                            (false, u8::from(!is_current), 0, r),
                            Start::Resume {
                                current: is_current,
                            },
                            nodes,
                        )
                    }
                    (None, None) => {
                        let (level, size, id) = nodes
                            .iter()
                            .map(|&id| self.start_rank(id))
                            .min()
                            .unwrap_or((u8::MAX, u64::MAX, BindingId(usize::MAX)));
                        ((true, level, size, id), Start::Scan, nodes)
                    }
                }
            })
            .collect();
        starts.sort_unstable_by_key(|(key, _, _)| *key);
        for ((_, _, _, start), how, nodes) in starts {
            self.walk_component(start, &nodes, &edges, how);
        }
        // Conjuncts left over (none in a well-formed query: every binding is bound).
        if !self.pending.is_empty() {
            let rest = std::mem::take(&mut self.pending);
            self.ops.push(Op::Filter(rest));
        }
        std::mem::take(&mut self.ops)
    }

    /// The first node an `OPTIONAL MATCH` segment binds (see `Stage::Optional`).
    fn witness(&self, segment: usize) -> Option<BindingId> {
        self.bound
            .bindings
            .iter()
            .find(|b| b.kind == BindingKind::Node && b.segment == segment && b.column.is_none())
            .map(|b| b.id)
    }

    /// The stage of a materializing `WITH`; its columns are bound after it.
    fn materialize(&mut self, index: usize, with: &BoundWith) -> Materialize {
        let filters = self
            .bound
            .cross
            .iter()
            .copied()
            .filter(|&c| {
                self.bound
                    .conjuncts
                    .get(c)
                    .is_some_and(|cj| cj.segment == index)
            })
            .collect();
        for &output in &with.outputs {
            self.bind(output);
        }
        Materialize {
            segment: index,
            result: result_ops(
                with.columns.iter().any(|c| c.aggregate),
                with.distinct,
                !with.order_by.is_empty(),
                &with.offset,
                &with.limit,
            ),
            filters,
            labelled: false,
        }
    }

    fn is_node(&self, id: BindingId) -> bool {
        self.bound
            .binding(id)
            .is_some_and(|b| b.kind == BindingKind::Node)
    }

    /// The `nodes` (sorted) of each connected component over the pattern edges `edges`, each
    /// sorted, components in order of their smallest node binding.
    fn components(&self, edges: &[usize], nodes: &[BindingId]) -> Vec<Vec<BindingId>> {
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
        for e in edges.iter().filter_map(|&i| self.edges.get(i)) {
            union(&mut parent, e.source.0, e.edge.0);
            union(&mut parent, e.edge.0, e.target.0);
        }
        let mut components: Vec<(usize, Vec<BindingId>)> = Vec::new();
        for &id in nodes {
            let root = find(&mut parent, id.0);
            match components.iter_mut().find(|(r, _)| *r == root) {
                Some((_, nodes)) => nodes.push(id),
                None => components.push((root, vec![id])),
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

    /// Records a labelling reason of `id`; none for a materializing `WITH` column binding,
    /// which is read from its row ([`Materialize::labelled`]).
    fn mark(&mut self, id: BindingId, f: impl FnOnce(&mut Labelling)) {
        if self.bound.binding(id).is_some_and(|b| b.column.is_some()) {
            return;
        }
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

    /// Walks the component of `nodes` from `start` over the segment's pattern edges `edges`.
    fn walk_component(
        &mut self,
        start: BindingId,
        nodes: &[BindingId],
        edges: &[usize],
        how: Start,
    ) {
        match how {
            Start::Resume { current } => {
                if !current {
                    self.mark(start, |l| l.resume = true);
                }
                self.ops.push(Op::Resume(Resume {
                    binding: start,
                    from_current: current,
                }));
            }
            Start::Scan | Start::Anchor => {
                let anchored = how == Start::Anchor;
                let labels = self
                    .bound
                    .binding(start)
                    .and_then(|b| scan_labels(&b.labels));
                let scan = Scan {
                    binding: start,
                    filters: self
                        .filters_of(start, if anchored { None } else { labels.as_deref() }),
                    scan_labels: labels,
                };
                self.ops.push(if anchored {
                    Op::Anchor(scan)
                } else {
                    Op::Scan(scan)
                });
            }
        }
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
        let mut todo: Vec<usize> = edges
            .iter()
            .copied()
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
                    (
                        u8::from(e.quantifier.is_some()),
                        u8::from(!closing),
                        far_rank,
                        u8::from(origin != current),
                        i,
                    ),
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
            reversed: from != e.source,
            quantifier: e.quantifier,
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

    /// The static labelling reasons: every group variable, every node and edge of a read path
    /// variable's pattern.
    fn mark_static(&mut self) {
        let bound = self.bound;
        for b in &bound.bindings {
            if b.group {
                self.mark(b.id, |l| l.group = true);
            }
        }
        for path in bound.matches.iter().flat_map(|m| &m.paths) {
            let read = path
                .variable
                .and_then(|p| bound.binding(p))
                .is_some_and(|p| p.read);
            if !read {
                continue;
            }
            // A column binding too: the path reads it through a label set after `gql:bind`.
            let ids = std::iter::once(path.start)
                .chain(path.steps.iter().flat_map(|step| [step.edge, step.node]));
            for id in ids {
                if let Some(l) = self.labelling.get_mut(id.0) {
                    l.path = true;
                }
            }
        }
    }

    /// The labelling reasons that depend on where a read runs (module docs, "Labelling"): a
    /// pass over the plan in execution order.
    fn mark_positions(&mut self, first: &[Op], stages: &mut [Stage]) {
        let bound = self.bound;
        let mut at = Position::default();
        let mut reads = Vec::new();
        for op in first {
            self.op_reads(op, &mut at, &mut reads);
        }
        for stage in stages.iter() {
            match stage {
                Stage::Match { ops, .. } => {
                    for op in ops {
                        self.op_reads(op, &mut at, &mut reads);
                    }
                }
                Stage::Optional { segment, ops, .. } => {
                    for op in ops {
                        self.op_reads(op, &mut at, &mut reads);
                    }
                    at.optional = Some(*segment);
                    at.row = None;
                }
                Stage::Materialize(m) => {
                    if let Some(Segment::With(with)) = bound.segments.get(m.segment) {
                        for column in &with.columns {
                            reads.extend_from_slice(&column.bindings);
                        }
                        for key in &with.order_by {
                            reads.extend_from_slice(&key.bindings);
                        }
                    }
                    self.read_all(&mut reads, &mut at);
                    at.row = Some(m.segment);
                    for &c in &m.filters {
                        if let Some(cj) = bound.conjuncts.get(c) {
                            reads.extend_from_slice(&cj.bindings);
                        }
                    }
                    self.read_all(&mut reads, &mut at);
                }
            }
        }
        for column in &bound.columns {
            reads.extend_from_slice(&column.bindings);
        }
        for key in &bound.order_by {
            reads.extend_from_slice(&key.bindings);
        }
        self.read_all(&mut reads, &mut at);
        for stage in stages.iter_mut() {
            match stage {
                Stage::Materialize(m) => m.labelled = at.rows.contains(&m.segment),
                Stage::Optional {
                    segment,
                    witness: Some(w),
                    ..
                } if at.witnesses.contains(segment) => {
                    let w = *w;
                    self.mark(w, |l| l.optional = true);
                }
                Stage::Optional { .. } | Stage::Match { .. } => {}
            }
        }
    }

    /// Records the reads of `op` at `at`; an operation other than a filter leaves the row.
    fn op_reads(&mut self, op: &Op, at: &mut Position, reads: &mut Vec<BindingId>) {
        let bound = self.bound;
        match op {
            Op::Filter(ids) => {
                for &c in ids {
                    if let Some(cj) = bound.conjuncts.get(c) {
                        reads.extend_from_slice(&cj.bindings);
                    }
                }
                self.read_all(reads, at);
                return;
            }
            Op::Resume(r) => reads.push(r.binding),
            Op::Join { binding } => reads.push(*binding),
            Op::Expand(e) => {
                if !e.from_current {
                    reads.push(e.from);
                }
                if e.edge_join {
                    reads.push(e.edge);
                }
            }
            Op::Scan(_) | Op::Anchor(_) => {}
        }
        self.read_all(reads, at);
        at.row = None;
    }

    /// Records the reads `reads` at `at` and clears them.
    fn read_all(&mut self, reads: &mut Vec<BindingId>, at: &mut Position) {
        let bound = self.bound;
        for id in reads.drain(..) {
            let Some(b) = bound.binding(id) else {
                continue;
            };
            if let Some(column) = b.column {
                if at.row != Some(column.segment) && !at.rows.contains(&column.segment) {
                    at.rows.push(column.segment);
                }
                continue;
            }
            if !matches!(b.kind, BindingKind::Node | BindingKind::Edge) {
                continue;
            }
            if at.optional.is_some_and(|k| b.segment <= k) {
                self.mark(id, |l| l.optional = true);
                let in_optional =
                    matches!(bound.segments.get(b.segment), Some(Segment::Optional(_)));
                if b.group && in_optional && !at.witnesses.contains(&b.segment) {
                    at.witnesses.push(b.segment);
                }
            }
        }
    }
}

/// Where a read runs, for [`Planner::mark_positions`].
#[derive(Debug, Default)]
struct Position {
    /// The segment of the last `OPTIONAL MATCH` part that ended before it.
    optional: Option<usize>,
    /// The segment of the materializing `WITH` whose row is the current object.
    row: Option<usize>,
    /// The materializing `WITH` segments whose rows are read after they were left.
    rows: Vec<usize>,
    /// The `OPTIONAL MATCH` segments whose group variables are read after them.
    witnesses: Vec<usize>,
}

/// The current element after `ops` when it was `current` before (as the lowering tracks it).
fn current_after(ops: &[Op], current: Option<BindingId>) -> Option<BindingId> {
    ops.iter().fold(current, |current, op| match op {
        Op::Scan(s) | Op::Anchor(s) => Some(s.binding),
        Op::Resume(r) => Some(r.binding),
        Op::Expand(e) => Some(e.to),
        Op::Join { binding } => Some(*binding),
        Op::Filter(_) => current,
    })
}

/// A result pipeline: `Aggregate` when a column aggregates, else `Project`; then `DISTINCT`,
/// `ORDER BY`, `OFFSET`, `LIMIT` as far as given.
fn result_ops(
    aggregated: bool,
    distinct: bool,
    ordered: bool,
    offset: &Option<Count>,
    limit: &Option<Count>,
) -> Vec<ResultOp> {
    let mut result = vec![if aggregated {
        ResultOp::Aggregate
    } else {
        ResultOp::Project
    }];
    if distinct {
        result.push(ResultOp::Distinct);
    }
    if ordered {
        result.push(ResultOp::Order);
    }
    if let Some(offset) = offset {
        result.push(ResultOp::Offset(offset.clone()));
    }
    if let Some(limit) = limit {
        result.push(ResultOp::Limit(limit.clone()));
    }
    result
}

#[cfg(test)]
mod tests;
