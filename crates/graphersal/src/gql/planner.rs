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
//!   its `WHERE` conjuncts; its columns are bound after it;
//! - a `FOR` / `UNWIND` is a [`Stage::For`]; its item and ordinal are bound after it (columns of
//!   its row, like a materializing `WITH`'s: a later `MATCH` written with the item as a node
//!   continues from it through `gql:bind`, design E3).
//!
//! A query that does not start with `MATCH` starts from one unit row (design M1): the first
//! segment's operations are then empty (or only the conjuncts of a leading `LET` / `FILTER`
//! pipeline), and the lowering emits `inject(0)` first.
//!
//! The current element after a segment is the one its last operation reached; after an
//! `OPTIONAL MATCH`, a materializing `WITH` or a `FOR` there is none.
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
//! others follow as mid-traversal scans (a Cartesian product), or as correlated lookups.
//!
//! # Correlated lookup (stage 2a, design M3, E5)
//!
//! A component that is not the root scan (any component of a later segment, every component of
//! the first segment after its first) and does not start at a bound node starts with an
//! [`Op::Lookup`] instead of a mid-traversal scan when one of its nodes has an equality conjunct
//! whose other side reads only bindings bound already (and parameters) and that the graph
//! serves: `element_id(n) = e` (by id), or `n.k = e` / `{k: e}` with a property index on `k`
//! for every scan label of `n` ([`PlannerFacts::has_vertex_index`]). Without such a conjunct the
//! component keeps its scan: a lookup no id or index serves is no cheaper than scan + filter.
//! Among several, an id lookup wins over an index lookup, then the node's start rank, then the
//! conjunct's text order. The lookup checks the equality itself (GQL `=`), so the conjunct is
//! not placed again; the node's other conjuncts and label factors follow as for a scan.
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
//! # Subqueries (stage 2b, design M2, E7, E11)
//!
//! The bodies of `EXISTS`, pattern predicates and pattern comprehensions
//! ([`BoundQuery::subqueries`]) are planned after the outer plan, with their host's position
//! known: a pass over the finished plan in execution
//! order finds every host (a conjunct where it is placed: on an element, in an [`Op::Filter`],
//! in the `WHERE` of a materializing `WITH` on its row; a `RETURN` / `WITH` column; an
//! `ORDER BY` key, on the new row map; a `FOR` list) and the position it runs at (the current
//! element, the row, the last `OPTIONAL MATCH` part before it). Each body is then planned like a
//! later segment of its own clauses ([`super::plan::SubqueryPlan`]): its imports are bound
//! before it, so its conjuncts over imports only (or none) run first, a component holding an
//! import starts there ([`Op::Resume`]: from the host's current element when the body imports
//! it, else by moving to the earliest import, `select` or `gql:bind`), an expansion reaching an
//! import is a join (an imported edge an edge join), a body's first component holding no import
//! is a child-leading [`Op::Scan`] (its pushdown and index apply), a later one may be an
//! [`Op::Lookup`]. Trail holds per body clause (design E8). A body is planned once per start
//! (the host's current element when it is a node the body imports, else none); a subquery
//! nested in a body is hosted in that body's plan the same way (a worklist, no recursion). The
//! `|` part of a pattern comprehension reads at the end of its body (its reads and the hosts of
//! the subqueries it holds are recorded there).
//!
//! The fast path (design E11): a conjunct that is a top-level `EXISTS` / `NOT EXISTS`
//! ([`SubqueryUse::Exists`]) runs last at its site: in [`ElementFilters::exists`] after the
//! pushed and residual conjuncts of its element, at the end of its [`Op::Filter`]; a conjunct
//! holding a subquery elsewhere ([`SubqueryUse::Hook`]) runs after the plain conjuncts there.
//!
//! # Labelling
//!
//! Besides the stage 1a reasons (read, join, trail, branch) the planner records: a later
//! segment's start away from the current element (`resume`), every group variable (`group`),
//! every node and edge of a read path variable's pattern (`path`), and, from a pass over the
//! finished plan in execution order, every read of a pattern binding after an `OPTIONAL MATCH`
//! part that ended after the binding was bound (`optional`; the witness of a part whose group
//! variable is read after it too) and every read of a materializing `WITH` or `FOR` column after
//! a later stage left its row ([`Materialize::labelled`], [`ForStage::labelled`]). The same pass
//! over every planned body at each of its host positions records those reasons for the reads
//! inside it, and the `subquery` reason for an import a body condition reads while it is not
//! the current element there.

use super::ast::{EdgeDirection, Quantifier};
use super::bind::{BindingId, BindingKind, BoundQuery, BoundWith, Count, Predicate, Segment};
use std::collections::BTreeSet;

use super::ast::Expr;
use super::plan::{
    EdgeLabels, ElementFilters, Expand, ForStage, Labelling, LogicalPlan, Lookup, LookupBy,
    Materialize, Op, Pushdown, ResultOp, Resume, Scan, Stage, SubqueryPlan, SubqueryUse,
    and_factors, classify_all, element_filters, lookup_form, scan_labels, subqueries_in,
    subquery_start,
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
    /// Emitted already (an [`Op::Lookup`]).
    Started,
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
    /// Per binding: the nodes and edges of a path variable's pattern (empty for the others);
    /// a path variable is bound once all of them are.
    path_elements: Vec<Vec<BindingId>>,
    /// How each conjunct uses subqueries, by conjunct index.
    uses: Vec<SubqueryUse>,
    /// The subquery bodies planned so far.
    subplans: Vec<SubqueryPlan>,
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
        let mut path_elements = vec![Vec::new(); n];
        for path in bound.matches.iter().flat_map(|m| &m.paths) {
            if let Some(slot) = path.variable.and_then(|p| path_elements.get_mut(p.0)) {
                *slot = std::iter::once(path.start)
                    .chain(path.steps.iter().flat_map(|step| [step.edge, step.node]))
                    .collect();
            }
        }
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
            path_elements,
            uses: bound.conjuncts.iter().map(SubqueryUse::of).collect(),
            subplans: Vec::new(),
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
                    let ops = self.segment(index, clauses, current, anchor, false);
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
                    let ops =
                        self.segment(index, std::slice::from_ref(clause), current, None, false);
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
                Segment::For(bound_for) => {
                    // The item and the ordinal are bound after it (columns of its row).
                    self.bind(bound_for.variable);
                    if let Some((_, ordinal)) = bound_for.ordinal {
                        self.bind(ordinal);
                    }
                    stages.push(Stage::For(ForStage {
                        segment: index,
                        labelled: false,
                    }));
                    current = None;
                }
            }
        }
        self.mark_static();
        self.mark_positions(&first, &mut stages);
        let mut subqueries = std::mem::take(&mut self.subplans);
        subqueries.sort_by_key(|p| (p.subquery, p.current));
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
            subqueries,
        }
    }

    /// Plans segment `index` of `clauses` (module docs): its components best start first, its
    /// conjuncts, the leftovers at the end. `current` is the current element when it starts.
    /// `body`: the segment is a subquery body (its conjuncts over bound bindings only run at
    /// its start, its first component when no import starts it is a child-leading scan).
    fn segment(
        &mut self,
        index: usize,
        clauses: &[usize],
        current: Option<BindingId>,
        anchor: Option<BindingId>,
        body: bool,
    ) -> Vec<Op> {
        let bound = self.bound;
        self.pending = bound
            .cross
            .iter()
            .copied()
            .filter(|&c| bound.conjuncts.get(c).is_some_and(|cj| cj.segment == index))
            .collect();
        let first = index == 0 && !body;
        if !first {
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
        for (k, ((_, _, _, start), how, nodes)) in starts.into_iter().enumerate() {
            // Every component but the root scan may start with a correlated lookup (module
            // docs); a body's first component is a child-leading scan.
            let root = (first || body) && k == 0;
            let lookup = (how == Start::Scan && !root)
                .then(|| self.lookup(&nodes))
                .flatten();
            match lookup {
                Some(lookup) => {
                    let start = lookup.scan.binding;
                    self.pending.retain(|&c| c != lookup.conjunct);
                    self.ops.push(Op::Lookup(lookup));
                    self.walk_component(start, &nodes, &edges, Start::Started);
                }
                None => self.walk_component(start, &nodes, &edges, how),
            }
        }
        // Conjuncts left over (none in a well-formed query: every binding is bound).
        if !self.pending.is_empty() {
            let mut rest = std::mem::take(&mut self.pending);
            rest.sort_by_key(|&c| (self.use_of(c), c));
            self.ops.push(Op::Filter(rest));
        }
        std::mem::take(&mut self.ops)
    }

    /// The correlated lookup of a component of `nodes` (module docs), `None` when no id or
    /// index serves an equality of its nodes against the bindings bound so far.
    fn lookup(&self, nodes: &[BindingId]) -> Option<Lookup> {
        let bound = self.bound;
        type Rank = (u8, (u8, u64, BindingId), usize);
        let mut best: Option<(Rank, BindingId, LookupBy, &super::ast::Expr)> = None;
        for &node in nodes {
            let Some(binding) = bound.binding(node) else {
                continue;
            };
            let labels = scan_labels(&binding.labels);
            for &c in binding.predicates.iter().chain(&self.pending) {
                let Some(conjunct) = bound.conjuncts.get(c) else {
                    continue;
                };
                let ready = conjunct.bindings.contains(&node)
                    && conjunct
                        .bindings
                        .iter()
                        .all(|&b| b == node || self.is_bound(b));
                if !ready {
                    continue;
                }
                let Some((by, value)) = lookup_form(conjunct, node, binding.key.as_deref()) else {
                    continue;
                };
                let served = match &by {
                    LookupBy::Id => true,
                    LookupBy::Index(key) => labels.as_ref().is_some_and(|labels| {
                        !labels.is_empty()
                            && labels
                                .iter()
                                .all(|label| self.facts.has_vertex_index(label, key))
                    }),
                };
                if !served {
                    continue;
                }
                let rank = (u8::from(by != LookupBy::Id), self.start_rank(node), c);
                if best.as_ref().is_none_or(|(r, ..)| rank < *r) {
                    best = Some((rank, node, by, value));
                }
            }
        }
        let ((_, _, conjunct), node, by, value) = best?;
        let scan_labels = bound.binding(node).and_then(|b| scan_labels(&b.labels));
        let mut filters = self.filters_of(node, scan_labels.as_deref());
        filters.pushed.retain(|&c| c != conjunct);
        filters.residual.retain(|&c| c != conjunct);
        Some(Lookup {
            scan: Scan {
                binding: node,
                scan_labels,
                filters,
            },
            conjunct,
            by,
            value: value.clone(),
        })
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
        element_filters(&binding.predicates, &self.pushdown, &self.uses, |key| {
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
        match self.path_elements.get(id.0) {
            Some(elements) if !elements.is_empty() => elements
                .iter()
                .all(|&e| self.bound_set.get(e.0) == Some(&true)),
            _ => self.bound_set.get(id.0).copied().unwrap_or(false),
        }
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
            // Design E11: conjuncts holding a subquery after the others, the fast-path
            // `EXISTS` ones last.
            ready.sort_by_key(|&c| (self.use_of(c), c));
            self.ops.push(Op::Filter(ready));
        }
    }

    /// How conjunct `c` uses subqueries.
    fn use_of(&self, c: usize) -> SubqueryUse {
        self.uses.get(c).copied().unwrap_or(SubqueryUse::None)
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
            Start::Started => {}
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
    /// pass over the plan in execution order, which also finds the host of every subquery and
    /// plans the bodies (module docs, "Subqueries").
    fn mark_positions(&mut self, first: &[Op], stages: &mut [Stage]) {
        let bound = self.bound;
        let mut at = Position::default();
        let mut reads = Vec::new();
        let mut hosts = Vec::new();
        let mut current = None;
        self.pipeline_reads(first, &mut current, &mut at, &mut reads, &mut hosts);
        for stage in stages.iter() {
            match stage {
                Stage::Match { ops, .. } => {
                    self.pipeline_reads(ops, &mut current, &mut at, &mut reads, &mut hosts);
                }
                Stage::Optional { segment, ops, .. } => {
                    self.pipeline_reads(ops, &mut current, &mut at, &mut reads, &mut hosts);
                    at.optional = Some(*segment);
                    at.row = None;
                    current = None;
                }
                Stage::Materialize(m) => {
                    if let Some(Segment::With(with)) = bound.segments.get(m.segment) {
                        let here = at.host(current);
                        for column in &with.columns {
                            reads.extend_from_slice(&column.bindings);
                            self.expr_hosts(&column.expr, here, &mut hosts);
                        }
                        // `ORDER BY` runs on the new row map, read away from any row.
                        let sorted = at.host(None).away();
                        for key in &with.order_by {
                            reads.extend_from_slice(&key.bindings);
                            self.expr_hosts(&key.expr, sorted, &mut hosts);
                        }
                    }
                    self.read_all(&mut reads, &mut at);
                    at.row = Some(m.segment);
                    current = None;
                    for &c in &m.filters {
                        if let Some(cj) = bound.conjuncts.get(c) {
                            reads.extend_from_slice(&cj.bindings);
                        }
                        self.conjunct_hosts(c, at.host(None), &mut hosts);
                    }
                    self.read_all(&mut reads, &mut at);
                }
                Stage::For(f) => {
                    // The list is read on the row before the `FOR`.
                    if let Some(Segment::For(bound_for)) = bound.segments.get(f.segment) {
                        reads.extend_from_slice(&bound_for.bindings);
                        self.expr_hosts(&bound_for.list, at.host(current), &mut hosts);
                    }
                    self.read_all(&mut reads, &mut at);
                    at.row = Some(f.segment);
                    current = None;
                }
            }
        }
        let here = at.host(current);
        for column in &bound.columns {
            reads.extend_from_slice(&column.bindings);
            self.expr_hosts(&column.expr, here, &mut hosts);
        }
        let sorted = at.host(None).away();
        for key in &bound.order_by {
            reads.extend_from_slice(&key.bindings);
            self.expr_hosts(&key.expr, sorted, &mut hosts);
        }
        self.read_all(&mut reads, &mut at);
        self.plan_subqueries(hosts, &mut at);
        for stage in stages.iter_mut() {
            match stage {
                Stage::Materialize(m) => m.labelled = at.rows.contains(&m.segment),
                Stage::For(f) => f.labelled = at.rows.contains(&f.segment),
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

    /// Records the reads of the match pipeline `ops` (starting with `current` as the current
    /// element) and the hosts of the subqueries its conjuncts hold.
    fn pipeline_reads(
        &mut self,
        ops: &[Op],
        current: &mut Option<BindingId>,
        at: &mut Position,
        reads: &mut Vec<BindingId>,
        hosts: &mut Vec<(usize, Host)>,
    ) {
        for op in ops {
            self.op_hosts(op, *current, at, hosts);
            self.op_reads(op, at, reads);
            *current = current_after(std::slice::from_ref(op), *current);
        }
    }

    /// The subqueries held by the conjuncts `op` places, each with the position its conjunct
    /// runs at (`current`: the current element before `op`).
    fn op_hosts(
        &self,
        op: &Op,
        current: Option<BindingId>,
        at: &Position,
        hosts: &mut Vec<(usize, Host)>,
    ) {
        let mut on = |filters: &ElementFilters, id: BindingId| {
            let here = at.host(Some(id));
            let ids = filters
                .pushed
                .iter()
                .chain(&filters.residual)
                .chain(&filters.exists);
            for &c in ids {
                self.conjunct_hosts(c, here, hosts);
            }
        };
        match op {
            Op::Filter(ids) => {
                for &c in ids {
                    self.conjunct_hosts(c, at.host(current), hosts);
                }
            }
            Op::Scan(s) | Op::Anchor(s) => on(&s.filters, s.binding),
            // The lookup's own value holds no subquery (`plan::lookup_form`).
            Op::Lookup(l) => on(&l.scan.filters, l.scan.binding),
            Op::Expand(e) => {
                on(&e.edge_filters, e.edge);
                on(&e.to_filters, e.to);
            }
            Op::Resume(_) | Op::Join { .. } => {}
        }
    }

    /// The subqueries conjunct `c` holds, hosted at `host`.
    fn conjunct_hosts(&self, c: usize, host: Host, hosts: &mut Vec<(usize, Host)>) {
        if self.use_of(c) == SubqueryUse::None {
            return;
        }
        match self.bound.conjuncts.get(c).map(|cj| &cj.predicate) {
            Some(Predicate::Expr(expr)) => self.expr_hosts(expr, host, hosts),
            Some(Predicate::Property { value, .. }) => self.expr_hosts(value, host, hosts),
            None => {}
        }
    }

    /// The subqueries `expr` holds directly, hosted at `host`.
    fn expr_hosts(&self, expr: &Expr, host: Host, hosts: &mut Vec<(usize, Host)>) {
        let bound = self.bound;
        subqueries_in(expr, |span| {
            if let Some(k) = bound.subquery_at(span) {
                hosts.push((k, host));
            }
        });
    }

    /// Plans the body of every hosted subquery (module docs, "Subqueries"), once per subquery
    /// and start, and records the reads of each body at each host position (nested hosts
    /// included: a worklist, no recursion).
    fn plan_subqueries(&mut self, hosts: Vec<(usize, Host)>, at: &mut Position) {
        let mut queue: std::collections::VecDeque<(usize, Host)> = hosts.into();
        let mut seen: BTreeSet<(usize, Host)> = BTreeSet::new();
        while let Some((k, mut host)) = queue.pop_front() {
            host.current = subquery_start(self.bound, k, host.current);
            if !seen.insert((k, host)) {
                continue;
            }
            let planned = self
                .subplans
                .iter()
                .position(|p| p.subquery == k && p.current == host.current);
            let index = match planned {
                Some(index) => index,
                None => {
                    self.plan_subquery(k, host.current);
                    self.subplans.len().saturating_sub(1)
                }
            };
            let ops = self
                .subplans
                .get(index)
                .map(|p| p.ops.clone())
                .unwrap_or_default();
            let mut nested = Vec::new();
            self.body_reads(k, &ops, host, at, &mut nested);
            queue.extend(nested);
        }
    }

    /// Plans subquery `k`'s body for a host on `current` (an import or `None`): its imports
    /// are bound before it, its locals not yet (they may have been bound by a plan of the
    /// same body for another start).
    fn plan_subquery(&mut self, k: usize, current: Option<BindingId>) {
        let bound = self.bound;
        let Some(sub) = bound.subqueries.get(k) else {
            return;
        };
        for b in &bound.bindings {
            if b.segment == sub.segment
                && let Some(slot) = self.bound_set.get_mut(b.id.0)
            {
                *slot = false;
            }
        }
        for &clause in &sub.clauses {
            if let Some(walked) = self.walked.get_mut(clause) {
                walked.clear();
            }
        }
        for &import in &sub.imports {
            self.bind(import);
        }
        let ops = self.segment(sub.segment, &sub.clauses, current, None, true);
        self.subplans.push(SubqueryPlan {
            subquery: k,
            current,
            ops,
        });
    }

    /// Records the reads of subquery `k`'s body `ops` run at `host`: the position reasons as
    /// for the outer plan, and [`Labelling::subquery`] for an import a condition reads while it
    /// is not the current element; collects the hosts of the subqueries nested in the body.
    fn body_reads(
        &mut self,
        k: usize,
        ops: &[Op],
        host: Host,
        at: &mut Position,
        nested: &mut Vec<(usize, Host)>,
    ) {
        let bound = self.bound;
        let Some(segment) = bound.subqueries.get(k).map(|s| s.segment) else {
            return;
        };
        at.optional = host.optional;
        at.row = host.row;
        let mut current = host.current;
        let mut reads = Vec::new();
        let mut away = Vec::new();
        for op in ops {
            self.op_hosts(op, current, at, nested);
            match op {
                Op::Filter(ids) => {
                    for &c in ids {
                        if let Some(cj) = bound.conjuncts.get(c) {
                            away.extend_from_slice(&cj.bindings);
                        }
                    }
                }
                Op::Lookup(l) => {
                    if let Some(cj) = bound.conjuncts.get(l.conjunct) {
                        away.extend(cj.bindings.iter().filter(|&&b| b != l.scan.binding));
                    }
                }
                // Resume, branch and join targets have reasons of their own.
                Op::Scan(_) | Op::Anchor(_) | Op::Resume(_) | Op::Expand(_) | Op::Join { .. } => {}
            }
            for id in away.drain(..) {
                let import = bound.binding(id).is_some_and(|b| b.segment != segment);
                if import && current != Some(id) {
                    self.mark(id, |l| l.subquery = true);
                }
            }
            self.op_reads(op, at, &mut reads);
            current = current_after(std::slice::from_ref(op), current);
        }
        // The `|` part of a pattern comprehension runs at the end of the body (`gql:value`).
        if let Some(sub) = bound.subqueries.get(k)
            && let Some(map) = &sub.map
        {
            self.expr_hosts(map, at.host(current), nested);
            for &id in &sub.map_bindings {
                let import = bound.binding(id).is_some_and(|b| b.segment != segment);
                if import && current != Some(id) {
                    self.mark(id, |l| l.subquery = true);
                }
            }
            reads.extend_from_slice(&sub.map_bindings);
            self.read_all(&mut reads, at);
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
            // The other side of the equality is read on the row before the lookup moves.
            Op::Lookup(l) => {
                if let Some(cj) = bound.conjuncts.get(l.conjunct) {
                    reads.extend(cj.bindings.iter().filter(|&&b| b != l.scan.binding));
                }
            }
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
    /// The segment of the materializing `WITH` or `FOR` whose row is the current object.
    row: Option<usize>,
    /// The materializing `WITH` and `FOR` segments whose rows are read after they were left.
    rows: Vec<usize>,
    /// The `OPTIONAL MATCH` segments whose group variables are read after them.
    witnesses: Vec<usize>,
}

impl Position {
    /// The position of a subquery host here, on `current`.
    fn host(&self, current: Option<BindingId>) -> Host {
        Host {
            current,
            row: self.row,
            optional: self.optional,
        }
    }
}

/// Where a subquery's host runs: the body starts there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Host {
    /// The current element (a body importing it starts there without moving).
    current: Option<BindingId>,
    /// The materializing `WITH` or `FOR` segment whose row is the current object.
    row: Option<usize>,
    /// The segment of the last `OPTIONAL MATCH` part that ended before it.
    optional: Option<usize>,
}

impl Host {
    /// The same position away from any row (an `ORDER BY` key, read on the new row map).
    fn away(self) -> Host {
        Host { row: None, ..self }
    }
}

/// The current element after `ops` when it was `current` before (as the lowering tracks it).
fn current_after(ops: &[Op], current: Option<BindingId>) -> Option<BindingId> {
    ops.iter().fold(current, |current, op| match op {
        Op::Scan(s) | Op::Anchor(s) => Some(s.binding),
        Op::Lookup(l) => Some(l.scan.binding),
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
