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
//! pattern graph of its own clauses; the first segment's plan is [`PipelinePlan::ops`] (for a
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
//! Stage 2d: every pipeline of the query ([`BoundLinear::pipeline`]: the whole query, each
//! `UNION` operand, each block after `NEXT`, each pipeline of a `CALL` body or a whole-query
//! subquery body) is planned on its own by these rules into a [`PipelinePlan`]; "the first
//! segment" is the pipeline's. A `CALL` is a [`Stage::Call`]: its body is planned right there
//! (a recursion, at most `bind::MAX_BODY_DEPTH` levels), with every binding before the `CALL`
//! bound and the current element there as the start of its first block's operands, which plan
//! their first segment like a subquery body (see "Subqueries"; its first component may also be
//! a correlated lookup when the equality reads a binding, since the body runs per row); its
//! returned columns are bound after it, and there is no current element after it. A
//! [`Segment::Rows`] is a [`Stage::Rows`] binding the columns of the block before.
//!
//! Stage 3a: a write clause ([`Segment::Write`]) is a [`Stage::Write`]: the elements it creates
//! are bound after it, the last one the current element; a `CALL` whose body ends in `FINISH`
//! or a write (a unit subquery, [`BoundCall::unit`]) keeps the outer row and its current
//! element; a pipeline ending in `FINISH` or a write has no result pipeline. The position pass
//! reads each inserted element's values where its insert step runs and hosts the subqueries
//! they hold there.
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
//! edge before a quantified one or a parenthesized path pattern (a loop multiplies the rows
//! most); then an edge whose both ends
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
//! # Parenthesized path patterns (stage 2e, design item 9)
//!
//! A parenthesized path pattern `(a) ((x)-[e]->(y) WHERE ..){m,n} (b)` is one pattern edge
//! between its outer nodes (the node before it, [`BoundPath::node_before`], and the node after
//! it) and one [`Op::Group`] when walked ([`GroupExpand`]), ranked like a quantified edge. The
//! merge rule (P0 of stage 2e) needs no join of its own: the walk starts on the outer node it
//! comes from, which is the first node of the first iteration, every iteration starts on the
//! node the one before ended on, and the last one ends on the outer node at the other end (an
//! [`Op::Join`] when that one is bound already, as for an edge). Walked from the node after it
//! the body is reversed hop by hop ([`GroupExpand::reversed`]: its last node first, each hop's
//! direction flipped). Per iteration: the first node's own conjuncts and the body conditions on
//! no element at its start, each hop's edge and node conjuncts, the body conditions over
//! several elements ([`BoundGroup::conjuncts`]) right after the hop binding the last of them
//! in walk order, reading the others through their labels ([`Labelling::body`]). Trail: each
//! body edge lists the earlier walked edges of the clause and the other body edges whose labels
//! may overlap (all of them when the body repeats, the earlier ones for one iteration); every
//! edge walked after it lists the body edges. Body elements belong to the component of the
//! outer nodes but never start one, and no other pattern edge reaches one before its body is
//! walked (a singleton written again later is continued from or joined to afterwards; with
//! `?` the outer conditions on it are cross conjuncts after the body, ISO conditional
//! singletons). Labels: a group edge always (its trail), a group node when its list is read, a
//! read path variable every element of its pattern, the body included; the witness of an
//! `OPTIONAL MATCH` part is never a body node (zero iterations bind none).
//!
//! [`BoundPath::node_before`]: super::bind::BoundPath::node_before
//! [`BoundGroup::conjuncts`]: super::bind::BoundGroup::conjuncts
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
//! is a correlated [`Op::Lookup`] when the id or an index serves an equality of it reading a
//! binding (stage 2d.T5c), else a child-leading [`Op::Scan`] (its pushdown and index apply); a
//! later one may be an [`Op::Lookup`]. Trail holds per body clause (design E8). A body is planned once per start
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
//!
//! Stage 2d: the pass walks every pipeline of the plan tree (each operand of a block from the
//! block's start, a `CALL` body from the position of its `CALL`, a whole-query subquery body
//! from its host's position) and also records, by the same rule as for a `WITH` row, whether
//! the row of a `CALL` ([`CallStage::labelled`], `#c<k>`; always when a column of an
//! `OPTIONAL CALL` is read) or of a [`Stage::Rows`] ([`RowsStage::labelled`], `#n<k>`) is read
//! after a later stage left it. In a whole-query subquery body an import read (by a filter, a
//! column, a sort key, a `FOR` list) while it is not the current element gets the `subquery`
//! reason.

use super::ast::QueryEnd;
use super::ast::{EdgeDirection, Quantifier};
use super::bind::{
    BindingId, BindingKind, BoundCall, BoundComposite, BoundLinear, BoundQuery, BoundWith,
    BoundWrite, Count, InsertProperty, Predicate, Segment,
};
use std::collections::BTreeSet;

use super::ast::Expr;
use super::bind::BoundGroupStep;
use super::plan::{
    BodyHop, CallStage, CompositePlan, EdgeLabels, ElementFilters, Expand, ForStage, GroupExpand,
    GroupRef, Labelling, LogicalPlan, Lookup, LookupBy, Materialize, Op, PipelinePlan, Pushdown,
    ResultOp, Resume, RowsStage, Scan, Stage, SubqueryPlan, SubqueryUse, UnionPlan, WriteStage,
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

/// One occurrence of an edge in a path pattern, as written, or a parenthesized path pattern
/// (stage 2e) between the outer nodes it merges with.
#[derive(Debug, Clone, Copy)]
struct PatternEdge {
    clause: usize,
    /// The node written before it (for a parenthesized path pattern the node before it,
    /// [`BoundPath::node_before`](super::bind::BoundPath::node_before)).
    source: BindingId,
    /// The edge binding (for a parenthesized path pattern its body's first edge).
    edge: BindingId,
    direction: EdgeDirection,
    /// The node written after it.
    target: BindingId,
    quantifier: Option<Quantifier>,
    /// The parenthesized path pattern it stands for; `None` for an edge.
    group: Option<GroupRef>,
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
    /// Per binding: an element of a parenthesized path pattern's body (stage 2e), bound only
    /// by walking its body: never a start, never reached by another pattern edge first.
    in_body: Vec<bool>,
}

impl<'q> Planner<'q> {
    fn new(bound: &'q BoundQuery, facts: &'q dyn PlannerFacts) -> Self {
        let n = bound.bindings.len();
        let mut edges = Vec::new();
        let mut in_body = vec![false; n];
        for (clause, m) in bound.matches.iter().enumerate() {
            for (p, path) in m.paths.iter().enumerate() {
                let mut source = path.start;
                let plain = |source: &mut BindingId, step: &super::bind::BoundStep| {
                    let edge = PatternEdge {
                        clause,
                        source: *source,
                        edge: step.edge,
                        direction: step.direction,
                        target: step.node,
                        quantifier: step.quantifier,
                        group: None,
                    };
                    *source = step.node;
                    edge
                };
                for step in &path.steps {
                    edges.push(plain(&mut source, step));
                }
                for (index, g) in path.groups.iter().enumerate() {
                    let body = &g.group;
                    for id in std::iter::once(body.start)
                        .chain(body.steps.iter().flat_map(|s| [s.edge, s.node]))
                    {
                        if let Some(slot) = in_body.get_mut(id.0) {
                            *slot = true;
                        }
                    }
                    edges.push(PatternEdge {
                        clause,
                        source,
                        edge: body.steps.first().map_or(body.start, |s| s.edge),
                        direction: EdgeDirection::Right,
                        target: g.node,
                        quantifier: body.quantifier,
                        group: Some(GroupRef {
                            clause,
                            path: p,
                            index,
                        }),
                    });
                    source = g.node;
                    for step in &g.steps {
                        edges.push(plain(&mut source, step));
                    }
                }
            }
        }
        let mut path_elements = vec![Vec::new(); n];
        for path in bound.matches.iter().flat_map(|m| &m.paths) {
            if let Some(slot) = path.variable.and_then(|p| path_elements.get_mut(p.0)) {
                *slot = path.elements().collect();
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
            in_body,
        }
    }

    /// Whether `id` is an element of a parenthesized path pattern's body.
    fn in_body(&self, id: BindingId) -> bool {
        self.in_body.get(id.0).copied().unwrap_or(false)
    }

    /// The parenthesized path pattern `r` points at.
    fn group_step(&self, r: GroupRef) -> Option<&'q BoundGroupStep> {
        let bound = self.bound;
        bound
            .matches
            .get(r.clause)?
            .paths
            .get(r.path)?
            .groups
            .get(r.index)
    }

    fn run(mut self, anchor: Option<BindingId>) -> LogicalPlan {
        let anchor = anchor.filter(|a| self.is_node(*a));
        let bound = self.bound;
        let mut body = self.composite(&bound.body, Entry::Top { anchor });
        self.mark_static();
        self.mark_positions(&mut body);
        let mut subqueries = std::mem::take(&mut self.subplans);
        subqueries.sort_by_key(|p| (p.subquery, p.current));
        LogicalPlan {
            body,
            labelling: self.labelling,
            pushdown: self.pushdown,
            anchor,
            subqueries,
        }
    }

    /// Plans a composite query (module docs, "Composite queries"): every pipeline of every
    /// block in order. A `CALL` body inside recurses here (at most `bind::MAX_BODY_DEPTH`
    /// levels).
    fn composite(&mut self, composite: &BoundComposite, entry: Entry) -> CompositePlan {
        let mut blocks = Vec::with_capacity(composite.next.len() + 1);
        for (k, block) in std::iter::once(&composite.first)
            .chain(&composite.next)
            .enumerate()
        {
            let mut operands = Vec::with_capacity(block.unions.len() + 1);
            for (i, linear) in block.operands().enumerate() {
                // The first block's operands start at the entry (the phase 2 anchor only for
                // the first one); a block after `NEXT` starts each from the rows before.
                let start = match entry {
                    Entry::Top { anchor } if k == 0 => PipelineStart {
                        kind: SegmentKind::First,
                        current: None,
                        anchor: anchor.filter(|_| i == 0),
                    },
                    Entry::Body { current } if k == 0 => PipelineStart {
                        kind: SegmentKind::Body,
                        current,
                        anchor: None,
                    },
                    Entry::Top { .. } | Entry::Body { .. } => PipelineStart {
                        kind: SegmentKind::Later,
                        current: None,
                        anchor: None,
                    },
                };
                operands.push(self.pipeline(linear, start));
            }
            let mut operands = operands.into_iter();
            blocks.push(UnionPlan {
                first: operands.next().unwrap_or_default(),
                unions: operands.collect(),
                distinct: block.distinct(),
            });
        }
        let mut blocks = blocks.into_iter();
        CompositePlan {
            first: blocks.next().unwrap_or_default(),
            next: blocks.collect(),
        }
    }

    /// Plans the pipeline of `linear` (module docs, "Segments"): its first segment's match
    /// pipeline when it is a `MATCH`, a stage per later segment, its result pipeline.
    fn pipeline(&mut self, linear: &BoundLinear, start: PipelineStart) -> PipelinePlan {
        let bound = self.bound;
        let mut first = Vec::new();
        let mut stages = Vec::new();
        let mut current = start.current;
        for (position, &index) in linear.pipeline.iter().enumerate() {
            let Some(segment) = bound.segments.get(index) else {
                continue;
            };
            match segment {
                Segment::Match(clauses) => {
                    let (kind, anchor) = if position == 0 {
                        (start.kind, start.anchor)
                    } else {
                        (SegmentKind::Later, None)
                    };
                    let ops = self.segment(index, clauses, current, anchor, kind);
                    current = current_after(&ops, current);
                    if position == 0 {
                        first = ops;
                    } else {
                        stages.push(Stage::Match {
                            segment: index,
                            ops,
                        });
                    }
                }
                Segment::Optional(clause) => {
                    let ops = self.segment(
                        index,
                        std::slice::from_ref(clause),
                        current,
                        None,
                        SegmentKind::Later,
                    );
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
                Segment::Call(call) => {
                    stages.push(self.call(index, call, current));
                    // Its rows are the body's records: no current element (design item 8);
                    // after a unit subquery the outer row itself (stage 3a, design W3).
                    if !call.unit() {
                        current = None;
                    }
                }
                Segment::Rows(rows) => {
                    // The columns of the records before (column bindings of its row).
                    for &output in &rows.outputs {
                        self.bind(output);
                    }
                    stages.push(Stage::Rows(RowsStage {
                        segment: index,
                        labelled: false,
                    }));
                    current = None;
                }
                Segment::Write(write) => {
                    // The elements it creates are bound after it; the last one is the current
                    // element (the lowering's insert chain ends on it).
                    let created = write_created(write);
                    for &id in &created {
                        self.bind(id);
                    }
                    if let Some(&last) = created.last() {
                        current = Some(last);
                    }
                    stages.push(Stage::Write(WriteStage { segment: index }));
                }
            }
        }
        // A pipeline ending in `FINISH` or a write returns no row (stage 3a, design W2).
        let result = if linear.end == QueryEnd::Return {
            result_ops(
                linear.columns.iter().any(|c| c.aggregate),
                linear.distinct,
                !linear.order_by.is_empty(),
                &linear.offset,
                &linear.limit,
            )
        } else {
            Vec::new()
        };
        PipelinePlan {
            ops: first,
            stages,
            result,
        }
    }

    /// The stage of a `CALL` (module docs, "Composite queries"): its body planned with every
    /// binding before it bound and the current element `current` as its start; its returned
    /// columns are bound after it. A thin frame: it recurses through [`Self::composite`].
    #[inline(never)]
    fn call(&mut self, index: usize, call: &BoundCall, current: Option<BindingId>) -> Stage {
        let plan = self.composite(&call.body, Entry::Body { current });
        for &output in &call.outputs {
            self.bind(output);
        }
        Stage::Call(CallStage {
            segment: index,
            plan: Box::new(plan),
            optional: call.optional,
            imports: call.imports.clone(),
            labelled: false,
        })
    }

    /// Plans segment `index` of `clauses` (module docs): its components best start first, its
    /// conjuncts, the leftovers at the end. `current` is the current element when it starts.
    /// `kind`: the first segment of a top-level pipeline (its first component is the root scan,
    /// its conjuncts over no binding run after it, as in stage 1a), a body ([`SegmentKind`]: its
    /// conjuncts over bound bindings only run at its start, its first component when no import
    /// starts it is a correlated lookup or a child-leading scan), or a later
    /// one.
    fn segment(
        &mut self,
        index: usize,
        clauses: &[usize],
        current: Option<BindingId>,
        anchor: Option<BindingId>,
        kind: SegmentKind,
    ) -> Vec<Op> {
        let bound = self.bound;
        let body = kind == SegmentKind::Body;
        self.pending = bound
            .cross
            .iter()
            .copied()
            .filter(|&c| bound.conjuncts.get(c).is_some_and(|cj| cj.segment == index))
            .collect();
        let first = kind == SegmentKind::First;
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
                // Every element (the node filter below keeps the nodes), parenthesized path
                // pattern bodies included: their nodes belong to the component (a singleton
                // may be written again later), but never start it.
                for id in path.elements() {
                    if let Some(slot) = in_pattern.get_mut(id.0) {
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
                            .filter(|&&id| !self.in_body(id))
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
            // docs).
            let root = (first || body) && k == 0;
            // The first component of a body (a `CALL` body, a subquery body) runs per incoming
            // row: a lookup serves it when its value reads that row (module docs), else it is
            // a child-leading scan.
            let correlated = root && body;
            let lookup = (how == Start::Scan && (!root || correlated))
                .then(|| self.lookup(&nodes, correlated))
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
    /// `correlated`: only an equality whose other side reads a binding (a constant one is
    /// served by the scan's pushdown and index).
    fn lookup(&self, nodes: &[BindingId], correlated: bool) -> Option<Lookup> {
        let bound = self.bound;
        type Rank = (u8, (u8, u64, BindingId), usize);
        let mut best: Option<(Rank, BindingId, LookupBy, &super::ast::Expr)> = None;
        for &node in nodes.iter().filter(|&&id| !self.in_body(id)) {
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
                        .all(|&b| b == node || self.is_bound(b))
                    && (!correlated || conjunct.bindings.iter().any(|&b| b != node));
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

    /// The first node an `OPTIONAL MATCH` segment binds (see `Stage::Optional`); never a body
    /// node of a parenthesized path pattern, which zero iterations leave unbound (stage 2e).
    fn witness(&self, segment: usize) -> Option<BindingId> {
        self.bound
            .bindings
            .iter()
            .find(|b| {
                b.kind == BindingKind::Node
                    && b.segment == segment
                    && b.column.is_none()
                    && !self.in_body(b.id)
            })
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
            // A parenthesized path pattern's body belongs to the component of its outer nodes.
            if let Some(g) = e.group.and_then(|r| self.group_step(r)) {
                let body = &g.group;
                for id in std::iter::once(body.start)
                    .chain(body.steps.iter().flat_map(|s| [s.edge, s.node]))
                {
                    union(&mut parent, e.source.0, id.0);
                }
            }
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
                .filter(|&n| !self.is_bound(n) && !self.in_body(n))
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
                // A body element is bound only by walking its body (stage 2e).
                if !closing && self.in_body(far) {
                    return None;
                }
                let far_rank = if closing { 0 } else { self.filter_rank(far) };
                Some((
                    (
                        u8::from(e.quantifier.is_some() || e.group.is_some()),
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
        if let Some(r) = e.group {
            return self.expand_group(e, r, current);
        }
        let (from, to) = self.orient(&e, current);
        let direction = if from == e.source {
            e.direction
        } else {
            flip(e.direction)
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

    /// Emits the walk of the parenthesized path pattern `r` (module docs, "Parenthesized path
    /// patterns"); returns the new current element. Not recursive: a body holds no group.
    #[inline(never)]
    fn expand_group(&mut self, e: PatternEdge, r: GroupRef, current: BindingId) -> BindingId {
        let (from, to) = self.orient(&e, current);
        let Some(step) = self.group_step(r) else {
            return current;
        };
        let group = &step.group;
        let reversed = from != e.source;
        let from_current = from == current;
        if !from_current {
            self.mark(from, |l| l.branch = true);
        }
        // The body in walk order: nodes `walk[0..=k]`, hop `i` from `walk[i]` to `walk[i + 1]`
        // over `edges[i]` (its direction as seen from `walk[i]`).
        let mut walk: Vec<BindingId> = std::iter::once(group.start)
            .chain(group.steps.iter().map(|s| s.node))
            .collect();
        let mut edges: Vec<(BindingId, EdgeDirection)> =
            group.steps.iter().map(|s| (s.edge, s.direction)).collect();
        if reversed {
            walk.reverse();
            edges.reverse();
            for (_, direction) in &mut edges {
                *direction = flip(*direction);
            }
        }
        // Body variables are group variables when the body repeats (lists over the
        // iterations): every body edge then differs from every other body edge of every
        // iteration; singletons (no quantifier, `?`) only from the earlier ones.
        let repeats = group.quantifier.is_some() && !group.conditional;
        let first = walk.first().copied().unwrap_or(group.start);
        // Each body condition after the hop binding the last body element it reads (walk
        // order); one reading none at the start of each iteration. Body ids are consecutive
        // in text order (`start`, then each step's edge and node), so the hop binding one is
        // computed, not searched.
        let k = edges.len();
        let hop_of = |b: BindingId| -> usize {
            let offset = b.0.saturating_sub(group.start.0);
            // Text order: node `j` at offset `2j`, the edge into node `i` at `2i - 1`; both
            // bound by text hop `ceil(offset / 2)`.
            let text = offset.div_ceil(2).min(k);
            if !reversed {
                text
            } else if offset % 2 == 1 {
                // The edge of text hop `i` is walked by hop `k - i + 1`.
                (k + 1).saturating_sub(text)
            } else {
                k.saturating_sub(text)
            }
        };
        let mut at_hop: Vec<Vec<usize>> = vec![Vec::new(); k + 1];
        for &c in &group.conjuncts {
            let h = self
                .bound
                .conjuncts
                .get(c)
                .and_then(|cj| cj.bindings.iter().map(|&b| hop_of(b)).max())
                .unwrap_or(0);
            if let Some(slot) = at_hop.get_mut(h) {
                slot.push(c);
            }
        }
        let mut at_hop = at_hop.into_iter();
        let first_filters = self.filters_of(first, None);
        let first_conjuncts = at_hop.next().unwrap_or_default();
        self.bind(first);
        // The earlier walked edges of the clause (before the body) the body edges compare with.
        let outer: Vec<BindingId> = self.walked.get(e.clause).cloned().unwrap_or_default();
        let labels_of = |planner: &Self, id: BindingId| {
            planner
                .bound
                .binding(id)
                .map_or(EdgeLabels::Any, |b| EdgeLabels::of(&b.labels))
        };
        let mut hops = Vec::with_capacity(edges.len());
        for (i, &(edge, direction)) in edges.iter().enumerate() {
            let hop_from = walk.get(i).copied().unwrap_or(first);
            let hop_to = walk.get(i + 1).copied().unwrap_or(hop_from);
            let mine = labels_of(self, edge);
            let others = edges
                .iter()
                .enumerate()
                .filter(|&(j, _)| j != i && (repeats || j < i))
                .map(|(_, &(other, _))| other);
            let trail: Vec<BindingId> = outer
                .iter()
                .copied()
                .chain(others)
                .filter(|&other| other != edge && labels_of(self, other).may_overlap(&mine))
                .collect();
            for &other in &trail {
                self.mark(other, |l| l.trail = true);
            }
            let filters = at_hop.next().unwrap_or_default();
            // A condition reads every element but the one just reached through its label.
            for &c in &filters {
                let reads = self
                    .bound
                    .conjuncts
                    .get(c)
                    .map_or(&[][..], |cj| &cj.bindings[..]);
                for &b in reads {
                    if b != hop_to {
                        self.mark(b, |l| l.body = true);
                    }
                }
            }
            hops.push(BodyHop {
                from: hop_from,
                edge,
                direction,
                trail,
                edge_filters: self.filters_of(edge, None),
                to: hop_to,
                to_filters: self.filters_of(hop_to, None),
                filters,
            });
            self.bind(edge);
            self.bind(hop_to);
        }
        if let Some(w) = self.walked.get_mut(e.clause) {
            w.extend(edges.iter().map(|(edge, _)| *edge));
        }
        let to_bound = self.is_bound(to);
        let to_filters = if to_bound {
            ElementFilters::default()
        } else {
            self.filters_of(to, None)
        };
        self.ops.push(Op::Group(GroupExpand {
            group: r,
            from,
            from_current,
            reversed,
            quantifier: group.quantifier,
            conditional: group.conditional,
            first,
            first_filters,
            first_conjuncts,
            hops,
            to,
            to_bound,
            to_filters,
        }));
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
            // A group edge always (its trail checks every iteration); a group node of a
            // parenthesized path pattern only when its list is read (stage 2e).
            if b.group && (b.kind == BindingKind::Edge || b.read) {
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
            // A column binding too: the path reads it through a label set after `gql:bind`;
            // every body element of a parenthesized path pattern (stage 2e).
            for id in path.elements() {
                if let Some(l) = self.labelling.get_mut(id.0) {
                    l.path = true;
                }
            }
        }
    }

    /// The labelling reasons that depend on where a read runs (module docs, "Labelling"): a
    /// pass over the plan in execution order, which also finds the host of every subquery and
    /// plans the bodies (module docs, "Subqueries").
    fn mark_positions(&mut self, body: &mut CompositePlan) {
        let bound = self.bound;
        let mut at = Position::default();
        let mut hosts = Vec::new();
        self.composite_positions(body, &bound.body, None, &mut at, &mut hosts, None);
        self.plan_subqueries(hosts, &mut at);
        self.finish_composite(body, &at);
        let mut subplans = std::mem::take(&mut self.subplans);
        for query in subplans.iter_mut().filter_map(|p| p.query.as_deref_mut()) {
            self.finish_composite(query, &at);
        }
        self.subplans = subplans;
    }

    /// The position pass over a composite plan (module docs, "Labelling"): its first block's
    /// operands start at the position `at` holds and on `current` (the top level: none; a body:
    /// its host's row), a block after `NEXT` from its rows. `body`: the whole-query subquery
    /// whose body this is (its imports read away from the current element get
    /// [`Labelling::subquery`]).
    fn composite_positions(
        &mut self,
        plan: &CompositePlan,
        composite: &BoundComposite,
        current: Option<BindingId>,
        at: &mut Position,
        hosts: &mut Vec<(usize, Host)>,
        body: Option<usize>,
    ) {
        let entry = (at.optional, at.row);
        let blocks = std::iter::once(&composite.first).chain(&composite.next);
        for (k, (block_plan, block)) in plan.blocks().zip(blocks).enumerate() {
            for (pipeline, linear) in block_plan.operands().zip(block.operands()) {
                let start = if k == 0 {
                    (at.optional, at.row) = entry;
                    current
                } else {
                    (at.optional, at.row) = (None, None);
                    None
                };
                self.pipeline_positions(pipeline, linear, start, at, hosts, body);
            }
        }
        (at.optional, at.row) = entry;
    }

    /// The position pass over one pipeline starting on `current` (see
    /// [`Self::composite_positions`]).
    fn pipeline_positions(
        &mut self,
        pipeline: &PipelinePlan,
        linear: &BoundLinear,
        mut current: Option<BindingId>,
        at: &mut Position,
        hosts: &mut Vec<(usize, Host)>,
        body: Option<usize>,
    ) {
        let bound = self.bound;
        let mut reads = Vec::new();
        self.pipeline_reads(&pipeline.ops, &mut current, at, &mut reads, hosts, body);
        for stage in &pipeline.stages {
            match stage {
                Stage::Match { ops, .. } => {
                    self.pipeline_reads(ops, &mut current, at, &mut reads, hosts, body);
                }
                Stage::Optional { segment, ops, .. } => {
                    self.pipeline_reads(ops, &mut current, at, &mut reads, hosts, body);
                    at.optional = Some(*segment);
                    at.row = None;
                    current = None;
                }
                Stage::Materialize(m) => {
                    if let Some(Segment::With(with)) = bound.segments.get(m.segment) {
                        let here = at.host(current);
                        for column in &with.columns {
                            self.body_reads_away(body, &column.bindings, current);
                            reads.extend_from_slice(&column.bindings);
                            self.expr_hosts(&column.expr, here, hosts);
                        }
                        // `ORDER BY` runs on the new row map, read away from any row.
                        let sorted = at.host(None).away();
                        for key in &with.order_by {
                            self.body_reads_away(body, &key.bindings, None);
                            reads.extend_from_slice(&key.bindings);
                            self.expr_hosts(&key.expr, sorted, hosts);
                        }
                    }
                    self.read_all(&mut reads, at);
                    at.row = Some(m.segment);
                    current = None;
                    for &c in &m.filters {
                        if let Some(cj) = bound.conjuncts.get(c) {
                            self.body_reads_away(body, &cj.bindings, None);
                            reads.extend_from_slice(&cj.bindings);
                        }
                        self.conjunct_hosts(c, at.host(None), hosts);
                    }
                    self.read_all(&mut reads, at);
                }
                Stage::For(f) => {
                    // The list is read on the row before the `FOR`.
                    if let Some(Segment::For(bound_for)) = bound.segments.get(f.segment) {
                        self.body_reads_away(body, &bound_for.bindings, current);
                        reads.extend_from_slice(&bound_for.bindings);
                        self.expr_hosts(&bound_for.list, at.host(current), hosts);
                    }
                    self.read_all(&mut reads, at);
                    at.row = Some(f.segment);
                    current = None;
                }
                Stage::Call(c) => {
                    // The body runs on the row here; its records are the new rows (of an
                    // `OPTIONAL CALL` a row may stay the incoming one: no row to read from).
                    // After a unit subquery (stage 3a) the row is the incoming one.
                    self.call_positions(c, current, at, hosts, body);
                    let unit = matches!(
                        bound.segments.get(c.segment),
                        Some(Segment::Call(call)) if call.unit()
                    );
                    if !unit {
                        at.row = (!c.optional).then_some(c.segment);
                        current = None;
                    }
                }
                Stage::Rows(r) => {
                    at.row = Some(r.segment);
                    current = None;
                }
                Stage::Write(w) => {
                    if let Some(Segment::Write(write)) = bound.segments.get(w.segment) {
                        self.write_positions(write, &mut current, at, &mut reads, hosts, body);
                    }
                }
            }
        }
        let here = at.host(current);
        for column in &linear.columns {
            self.body_reads_away(body, &column.bindings, current);
            reads.extend_from_slice(&column.bindings);
            self.expr_hosts(&column.expr, here, hosts);
        }
        let sorted = at.host(None).away();
        for key in &linear.order_by {
            self.body_reads_away(body, &key.bindings, None);
            reads.extend_from_slice(&key.bindings);
            self.expr_hosts(&key.expr, sorted, hosts);
        }
        self.read_all(&mut reads, at);
    }

    /// The position pass over a write clause (stage 3a), run on `current`: each element's
    /// property values (and the ends of an edge) are read where its insert step runs, on the
    /// row before the clause for the first element, then on the element created just before
    /// (the lowering's insert chain); the subqueries the values hold are hosted there. Leaves
    /// `current` on the last element created.
    fn write_positions(
        &mut self,
        write: &BoundWrite,
        current: &mut Option<BindingId>,
        at: &mut Position,
        reads: &mut Vec<BindingId>,
        hosts: &mut Vec<(usize, Host)>,
        body: Option<usize>,
    ) {
        match write {
            BoundWrite::Insert(insert) => {
                for node in &insert.nodes {
                    self.insert_reads(&node.properties, &[], *current, at, reads, hosts, body);
                    *current = Some(node.binding);
                }
                for edge in &insert.edges {
                    let ends = [edge.source, edge.target];
                    self.insert_reads(&edge.properties, &ends, *current, at, reads, hosts, body);
                    *current = Some(edge.binding);
                }
            }
            // Planned from stage 3c.T3 on (the lowering refuses them until then).
            BoundWrite::Set(_) | BoundWrite::Remove(_) => {}
        }
    }

    /// The reads of one insert step at `current` (see [`Self::write_positions`]): its
    /// property values and `ends`; the step leaves the row.
    #[allow(clippy::too_many_arguments)]
    fn insert_reads(
        &mut self,
        properties: &[InsertProperty],
        ends: &[BindingId],
        current: Option<BindingId>,
        at: &mut Position,
        reads: &mut Vec<BindingId>,
        hosts: &mut Vec<(usize, Host)>,
        body: Option<usize>,
    ) {
        let here = at.host(current);
        for property in properties {
            self.body_reads_away(body, &property.bindings, current);
            reads.extend_from_slice(&property.bindings);
            self.expr_hosts(&property.value, here, hosts);
        }
        self.body_reads_away(body, ends, current);
        reads.extend_from_slice(ends);
        self.read_all(reads, at);
        at.row = None;
    }

    /// The position pass over the body of the `CALL` stage `call`, run on `current`. A thin
    /// frame: it recurses through [`Self::composite_positions`].
    #[inline(never)]
    fn call_positions(
        &mut self,
        call: &CallStage,
        current: Option<BindingId>,
        at: &mut Position,
        hosts: &mut Vec<(usize, Host)>,
        body: Option<usize>,
    ) {
        if let Some(Segment::Call(bound_call)) = self.bound.segments.get(call.segment) {
            self.composite_positions(&call.plan, &bound_call.body, current, at, hosts, body);
        }
    }

    /// In the whole-query body of subquery `body`: records [`Labelling::subquery`] for every
    /// import among `ids` that is read while it is not the current element `current`.
    fn body_reads_away(
        &mut self,
        body: Option<usize>,
        ids: &[BindingId],
        current: Option<BindingId>,
    ) {
        let Some(k) = body else {
            return;
        };
        for &id in ids {
            if current != Some(id) && self.is_import(k, id) {
                self.mark(id, |l| l.subquery = true);
            }
        }
    }

    /// Whether `id` is declared outside the body of subquery `k` (an import when the body
    /// reads it).
    fn is_import(&self, k: usize, id: BindingId) -> bool {
        self.bound
            .binding(id)
            .is_some_and(|b| b.subquery != Some(k))
    }

    /// Sets the row labels the position pass found needed (`at.rows`) on every stage of
    /// `plan`, nested `CALL` bodies included, and labels the witness of every `OPTIONAL MATCH`
    /// whose group variable is read after it.
    fn finish_composite(&mut self, plan: &mut CompositePlan, at: &Position) {
        for pipeline in plan.pipelines_mut() {
            for stage in &mut pipeline.stages {
                match stage {
                    Stage::Materialize(m) => m.labelled = at.rows.contains(&m.segment),
                    Stage::For(f) => f.labelled = at.rows.contains(&f.segment),
                    Stage::Rows(r) => r.labelled = at.rows.contains(&r.segment),
                    Stage::Call(c) => {
                        c.labelled = at.rows.contains(&c.segment);
                        self.finish_call(c, at);
                    }
                    Stage::Optional {
                        segment,
                        witness: Some(w),
                        ..
                    } if at.witnesses.contains(segment) => {
                        let w = *w;
                        self.mark(w, |l| l.optional = true);
                    }
                    Stage::Optional { .. } | Stage::Match { .. } | Stage::Write(_) => {}
                }
            }
        }
    }

    /// [`Self::finish_composite`] of a `CALL` body. A thin frame for the recursion.
    #[inline(never)]
    fn finish_call(&mut self, call: &mut CallStage, at: &Position) {
        self.finish_composite(&mut call.plan, at);
    }

    /// Records the reads of the match pipeline `ops` (starting with `current` as the current
    /// element) and the hosts of the subqueries its conjuncts hold.
    /// In the whole-query body of subquery `body`, an import a filter reads while it is not
    /// the current element gets [`Labelling::subquery`] (as in a body of `MATCH` clauses).
    fn pipeline_reads(
        &mut self,
        ops: &[Op],
        current: &mut Option<BindingId>,
        at: &mut Position,
        reads: &mut Vec<BindingId>,
        hosts: &mut Vec<(usize, Host)>,
        body: Option<usize>,
    ) {
        let bound = self.bound;
        for op in ops {
            self.op_hosts(op, *current, at, hosts);
            if body.is_some() {
                match op {
                    Op::Filter(ids) => {
                        for &c in ids {
                            if let Some(cj) = bound.conjuncts.get(c) {
                                self.body_reads_away(body, &cj.bindings, *current);
                            }
                        }
                    }
                    Op::Lookup(l) => {
                        if let Some(cj) = bound.conjuncts.get(l.conjunct) {
                            let other: Vec<BindingId> = cj
                                .bindings
                                .iter()
                                .copied()
                                .filter(|&b| b != l.scan.binding)
                                .collect();
                            self.body_reads_away(body, &other, *current);
                        }
                    }
                    Op::Scan(_)
                    | Op::Anchor(_)
                    | Op::Resume(_)
                    | Op::Expand(_)
                    | Op::Group(_)
                    | Op::Join { .. } => {}
                }
            }
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
            // A body holds no subquery (the binder refuses one); its outer end may.
            Op::Group(g) => on(&g.to_filters, g.to),
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
            let (ops, query) = self
                .subplans
                .get(index)
                .map(|p| (p.ops.clone(), p.query.clone()))
                .unwrap_or_default();
            let mut nested = Vec::new();
            match query {
                Some(query) => self.query_body_reads(k, &query, host, at, &mut nested),
                None => self.body_reads(k, &ops, host, at, &mut nested),
            }
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
        if let Some(query) = &sub.query {
            self.plan_query_body(k, query, current);
            return;
        }
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
        let ops = self.segment(sub.segment, &sub.clauses, current, None, SegmentKind::Body);
        self.subplans.push(SubqueryPlan {
            subquery: k,
            current,
            ops,
            query: None,
        });
    }

    /// Plans the whole-query body `query` of subquery `k` for a host on `current` (stage 2d):
    /// like a `CALL` body, with its imports bound before it and its locals (every binding of
    /// its pipelines, nested `CALL` bodies included) not yet.
    fn plan_query_body(&mut self, k: usize, query: &BoundComposite, current: Option<BindingId>) {
        let bound = self.bound;
        let mut segments = Vec::new();
        body_segments(bound, query, &mut segments);
        segments.sort_unstable();
        let inside = |segment: usize| segments.binary_search(&segment).is_ok();
        for b in &bound.bindings {
            if inside(b.segment)
                && let Some(slot) = self.bound_set.get_mut(b.id.0)
            {
                *slot = false;
            }
        }
        for (clause, m) in bound.matches.iter().enumerate() {
            if inside(m.segment)
                && let Some(walked) = self.walked.get_mut(clause)
            {
                walked.clear();
            }
        }
        if let Some(sub) = bound.subqueries.get(k) {
            for &import in &sub.imports {
                self.bind(import);
            }
        }
        let plan = self.composite(query, Entry::Body { current });
        self.subplans.push(SubqueryPlan {
            subquery: k,
            current,
            ops: Vec::new(),
            query: Some(Box::new(plan)),
        });
    }

    /// The position pass over the whole-query body `plan` of subquery `k` run at `host`
    /// (stage 2d, as [`Self::body_reads`] for a body of `MATCH` clauses); collects the hosts of
    /// the subqueries nested in it.
    fn query_body_reads(
        &mut self,
        k: usize,
        plan: &CompositePlan,
        host: Host,
        at: &mut Position,
        nested: &mut Vec<(usize, Host)>,
    ) {
        let bound = self.bound;
        let Some(query) = bound.subqueries.get(k).and_then(|s| s.query.as_deref()) else {
            return;
        };
        at.optional = host.optional;
        at.row = host.row;
        self.composite_positions(plan, query, host.current, at, nested, Some(k));
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
                Op::Scan(_)
                | Op::Anchor(_)
                | Op::Resume(_)
                | Op::Expand(_)
                | Op::Group(_)
                | Op::Join { .. } => {}
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
            Op::Group(g) => {
                if !g.from_current {
                    reads.push(g.from);
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

/// Collects the segments of every pipeline of `composite` into `out`, those of nested `CALL`
/// bodies included (at most `bind::MAX_BODY_DEPTH` levels).
fn body_segments(bound: &BoundQuery, composite: &BoundComposite, out: &mut Vec<usize>) {
    for linear in composite.linears() {
        for &index in &linear.pipeline {
            out.push(index);
            if let Some(Segment::Call(call)) = bound.segments.get(index) {
                body_segments(bound, &call.body, out);
            }
        }
    }
}

/// Where a composite's first block starts (see [`Planner::composite`]).
#[derive(Debug, Clone, Copy)]
enum Entry {
    /// The top level: the start (with the phase 2 anchor, `None` in stage 1a).
    Top {
        /// The anchor binding.
        anchor: Option<BindingId>,
    },
    /// A `CALL` body or a whole-query subquery body: the incoming row, on `current`.
    Body {
        /// The current element there.
        current: Option<BindingId>,
    },
}

/// How a pipeline starts (see [`Planner::pipeline`]).
#[derive(Debug, Clone, Copy)]
struct PipelineStart {
    /// The kind of its first segment.
    kind: SegmentKind,
    /// The current element it starts on.
    current: Option<BindingId>,
    /// The phase 2 anchor of its first segment.
    anchor: Option<BindingId>,
}

/// The kind of a segment for [`Planner::segment`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SegmentKind {
    /// The first segment of a top-level pipeline: its first component is the root scan, its
    /// conjuncts over no binding run after it (stage 1a).
    First,
    /// The segment of a subquery body of `MATCH` clauses (stage 2b), or the first segment of a
    /// pipeline of the first block of a `CALL` body or a whole-query subquery body (stage 2d):
    /// its conjuncts over bound bindings only run first; its first component not starting at
    /// an import starts with a correlated lookup when an equality reading a binding is served
    /// by the id or an index (the body runs per incoming row, stage 2d.T5c for the bodies of
    /// `MATCH` clauses too), else it is a child-leading scan.
    Body,
    /// Any other segment.
    Later,
}

/// The direction of an edge seen from its other end.
fn flip(direction: EdgeDirection) -> EdgeDirection {
    match direction {
        EdgeDirection::Right => EdgeDirection::Left,
        EdgeDirection::Left => EdgeDirection::Right,
        EdgeDirection::Any => EdgeDirection::Any,
    }
}

/// The bindings a write clause creates, in the order its steps create them (stage 3a: an
/// `INSERT`'s new nodes, then its new edges).
fn write_created(write: &BoundWrite) -> Vec<BindingId> {
    match write {
        BoundWrite::Insert(insert) => insert
            .nodes
            .iter()
            .map(|n| n.binding)
            .chain(insert.edges.iter().map(|e| e.binding))
            .collect(),
        // `SET` / `REMOVE` declare nothing.
        BoundWrite::Set(_) | BoundWrite::Remove(_) => Vec::new(),
    }
}

/// The current element after `ops` when it was `current` before (as the lowering tracks it).
fn current_after(ops: &[Op], current: Option<BindingId>) -> Option<BindingId> {
    ops.iter().fold(current, |current, op| match op {
        Op::Scan(s) | Op::Anchor(s) => Some(s.binding),
        Op::Lookup(l) => Some(l.scan.binding),
        Op::Resume(r) => Some(r.binding),
        Op::Expand(e) => Some(e.to),
        Op::Group(g) => Some(g.to),
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
