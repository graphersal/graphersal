//! The lowering: a planned GQL query ([`LogicalPlan`] of a [`BoundQuery`]) to ordinary
//! traversal steps, plus the Rust entry points [`GraphTraversalSource::gql`] and
//! [`GraphTraversalSource::gql_with`].
//!
//! The steps are built with the ordinary builders (`v()`, `has_label()`, `has()`, `as()`,
//! `optional()`, `skip()`, `limit()`) and the internal `gql:*` builders, so the plan goes through
//! the optimizer, the path analysis, the profile, the limits and the permission check like a
//! hand-written traversal (design D5, architecture e). Every fallible part (parsing, binding,
//! compiling expressions, checking parameter values) runs first; when one fails, the source gets
//! the `gql:error` marker instead, which raises the error before the traversal runs (D13).
//!
//! # Forms per construct (Phase 0 "Lowering forms fixed")
//!
//! | Construct | Steps |
//! |---|---|
//! | node scan `(p:A {k: v})` | `v()`, then `has_label(A)` (the scan labels first, so `SourceFilterPushdown` folds them into `v(labels: ..)`), the other label factors, the pushed conjuncts as `has(k, P..)`, the residual ones as `gql:filter`, then `as("p")` only when a label is needed |
//! | label factor `A`, `A\|B` | `has_label("A")`, `has_label("A", "B")`; `:A&B` is one `has_label` per factor |
//! | label factor with `!`, `%`, or `&` under `\|` | `gql:filter("A\|!B")` (`IS LABELED` on the current element) |
//! | pushed conjunct (`plan::classify`) | `has(k, P.eq(v))` (never the bare value), `P.lt` / `P.lte` / `P.gt` / `P.gte`, `P.within([..])` (`NULL`s dropped), `TextP.starting_with` / `ending_with` / `containing`, `IS NOT NULL` as `has(k, P.neq(()))` |
//! | `$param` operand | pushed only when its value fits the form (a non-null string, number or boolean; for `IN` a list of them, `NULL`s dropped; a string for the text forms), else `gql:filter` |
//! | other conjuncts | `gql:filter(expr)`, after the element it reads is bound |
//! | `-[e:T]->`, `<-[e:T]-` with nothing on the edge | `out("T")` / `in("T")` (also `-->` / `<--`, no labels), so `BarrierPushdown` and `lazy_barrier` apply |
//! | `-[e:T {k: v} WHERE ..]->`, an edge read later, joined or trail-checked | `out_e("T")` (`in_e` for `<-`), then on the edge: the other label factors (`has_label`), the pushed conjuncts (`has(k, P..)`), the join / trail checks, `gql:filter` for label factors with `!` / `%` and residual conjuncts, `as("e")` only when a label is needed; then `in_v()` (`out_v()`) |
//! | edge label `T`, `A\|B` | the labels of the adjacency step (the first `&` factor that is a label disjunction); other factors as for nodes, on the edge traverser |
//! | either direction `-[e]-`, `--`, `-[:T]-` | `union(__.out_e(..)<edge steps>.in_v(), __.in_e(..).gql:not_self_loop()<edge steps>.out_v())`, `as("e")` in both branches; without edge steps `union(__.out(..), __.in_e(..).gql:not_self_loop().out_v())`. Never `both_e().other_v()` (it forces full paths) nor plain `both()` (a self-loop would match twice; GQL binds it once) |
//! | target node `(b:L {k: v})` | after the hop: `has_label(L)`, the pushed `has()`, `gql:filter`, then `as("b")` only when a label is needed |
//! | join: a node bound earlier (a cycle, a variable repeated across patterns) | `where(P.eq("x"))` right after the hop reaching it (Phase 0 Q3) |
//! | an edge variable repeated in a later `MATCH` | `where(P.eq("e"))` on the edge traverser, no second `as("e")` |
//! | branch: the next hop starts at an earlier binding | `select("x")`, then the hop |
//! | trail (per `MATCH` clause) | `where(P.neq("e1"))` on the edge traverser for every earlier edge of the clause whose label set may overlap (anonymous edges get `#<id>` labels); skipped for disjoint label sets (Phase 0 Q8) |
//! | merge point | `lazy_barrier()` right after a hop that yields vertices when the next hop continues from them (before the target's filters and `as()`); merges only traversers with equal path labels, so it is invisible (see below) |
//! | several patterns / components | another `v()` per component (a Cartesian product, not optimized yet) |
//! | `RETURN` / `RETURN *` | `gql:project(columns)` (written order, `NULL` columns kept) |
//! | `DISTINCT` | `gql:distinct()` after `gql:project` (GQL equality, decision C3: `1` and `1.0` are one value, `NULL`s are equal) |
//! | `ORDER BY` | `gql:order(keys)` after `gql:project` (and `gql:distinct()`): a returned column is read from the row, any other variable through its path label; an alias is read only where the key names it (a substituted `LET` / `WITH` variable of the same name stays the variable) |
//! | `OFFSET n` / `LIMIT m` | `skip(n)` / `limit(m)`; a `$param` value must be a non-negative integer (checked before the run) |
//! | `LET x = expr`, a `WITH` that only projects | no step: the binder substitutes the definition, so the query lowers like the same query written without it (pushdown included) |
//! | `FILTER cond`, the `WHERE` of a projecting `WITH` | conjuncts like `WHERE`: `has()` / `gql:filter` where their bindings are bound |
//! | materializing `WITH` (aggregate, `DISTINCT`, `ORDER BY`, `OFFSET`, `LIMIT`; decision C2) | its result pipeline as for `RETURN` (`gql:project`, `gql:distinct()`, `gql:order`, `skip`, `limit`), then its `WHERE` as `gql:filter` on the row map (after `LIMIT`), then `as("#w<k>")` only when a later step reads the row after leaving it |
//! | a later `MATCH` continuing from a `WITH` column | `gql:bind("a")` (moves to the element the column holds; `NULL` or not an element: no row); a join, edge join or trail check against a column is `gql:filter` comparing the current element with the column |
//! | `OPTIONAL MATCH` | `optional(__.<part>)`, the part's own `WHERE` inside; a row it does not match continues unchanged and its variables read `NULL` (absent labels); after it every read goes through a label (no `Binding::Current`) |
//! | a leading `OPTIONAL MATCH` | `inject(0).optional(__.v()...)`: one unit row (decision C13) |
//! | a query that starts with `FOR`, `LET`, `WITH` or `RETURN` (design M1, E4) | `inject(0)` first: one unit row that holds no binding (`RETURN 1 AS x` is `inject(0).gql:project("x")`); a leading `LET` / projecting `WITH` followed by `MATCH` needs none (substituted, the `MATCH` starts the query) |
//! | `FOR x IN list` (Cypher `UNWIND list AS x`; design mechanism 2) | `gql:for("x IN list")`: one derived traverser per item, the item itself the current object (`Binding::Current` on its row, `as("x")` only when read after a later step left it, read as that label); `NULL` list no row, a non-list one row (E1) |
//! | `FOR x IN list WITH ORDINALITY i` / `WITH OFFSET i` | `gql:for(".. WITH ORDINALITY i")`: the record `{x, i}`, read like a materializing `WITH` row (`Binding::Column`, `Binding::RowColumn` through `as("#f<k>")` only when read after leaving it) |
//! | a later `MATCH` continuing from a `FOR` item (design E3) | `gql:bind("x")` (an item that is not an element of the pattern's kind, `NULL` included: no row; never a bare `out()` on the item); a join or trail check against it is `gql:filter` as for a `WITH` column |
//! | quantified edge `-[e:T]->{m,n}` (also `{m,}`, `{,n}`, `{n}`, `*`, `+`, Cypher `[e*m..n]`; plan mechanism 5) | `repeat(B)` with the bounds as modulators: `{m,n}` `repeat(B).emit(__.loops().is(P.gte(m))).times(n)` (`emit()` for `m = 1`), `{n}` `repeat(B).times(n)`, `{0,n}` `emit().repeat(B).times(n)` (the zero-length match first), no upper bound: no `times()` (trail ends the loop; `repeat.max_loops` and `traversal.max_traversers` are errors, never a cut result, design D14); an upper bound beyond `u32::MAX` is no bound; `{0}` emits no step (the far node is the near one). The body `B` is the edge hop above (`out_e(T)`, the pushed `has()`, the inline `WHERE` as `gql:filter`, either direction as the `union`) with `gql:trail([e, ..])` before `as("e")`: the edge differs from every earlier iteration and every earlier edge of the clause |
//! | trail of a single edge against a group variable | `gql:trail(["e"])` on the edge traverser (every iteration); single edges among themselves keep `where(P.neq(..))` |
//! | path variable `p = (a)-[e]->(b)` (plan mechanism 6, decision C11) | no step of its own: every node and edge of the pattern of a read path variable gets `as()` (a `WITH` column node after its `gql:bind` or join), a quantified edge also `as("#gv<id>")` on the vertex each iteration reaches (after `in_v()` / `out_v()` in the `repeat()` body); the slot reads `Binding::Path`, which assembles the path value in text order from those labels (a group reversed when walked from its far end), whichever node the planner starts at. Never `path()` (it needs full paths on every step). In an `OPTIONAL MATCH` part the path is anchored like a group variable, so a part not matched reads `NULL` |
//! | group variable `e` read outside its element | `Binding::Group`: every occurrence of `as("e")` as a list in text order (reversed when walked from the far end), `[]` for a zero-length match; inside an `OPTIONAL MATCH` part anchored on the part's witness node, or on `as("#o<k>")` at the end of a part without a new node, so a part not matched reads `NULL` |
//!
//! # Path labels (design D11)
//!
//! A binding gets `as()` only when something reads it while it is not the current element: a
//! filter over several bindings, a `RETURN` column after a later scan moved on, an `ORDER BY`
//! key (which runs on the row map), or a reason of the plan (join, trail, branch). An
//! expression reading the current element reads it directly (`Binding::Current`), with no path
//! label at all, so `MATCH (p:person) RETURN p.name` records no path and keeps bulk merging.
//!
//! # Merge points (Phase 0 outcome 1)
//!
//! `LazyBarrierRule` never places a merge point across an `as()` or a filter, so the lowering
//! places its own `lazy_barrier()` between two hops (the global rule stays unchanged, so Gremlin
//! plans do not move). Merging is sound there: the merge key keeps traversers with different path
//! labels apart, the step passes everything through under full paths, a sack merge operator or
//! `bulk.one`, and it merges only where the next hop's fan-out pays for it. It pays off where the
//! middle node is not read and no edge before it is labelled; a trail check against an earlier
//! edge of the same `MATCH` (edges with overlapping label sets) labels that edge and keeps the
//! traversers apart.
//!
//! A materializing `WITH` column is read from its row: `Binding::Column` while the row is the
//! current object, `Binding::RowColumn` through the row label after a later step left it.
//!
//! # Not yet
//!
//! The phase 2 anchor ([`Op::Anchor`], a `gql()` continuing a traversal) fails with
//! [`GqlErrorKind::NotYet`]; stage 1a never plans one. The constructs still out of scope are
//! refused by the parser (`NEXT`, `UNION`, subqueries, quantified path patterns, shortest
//! paths, ...) and the binder. A `MATCH` after `FOR` that compares a node with the item
//! (`FOR id IN $ids MATCH (n) WHERE element_id(n) = id`) is still a mid-traversal scan plus a
//! filter per row; the correlated lookup `gql:lookup` (design M3) comes next.
//!
//! # Aggregation
//!
//! An aggregating `RETURN` / `WITH` ([`ResultOp::Aggregate`]) is `gql:aggregate` in place of
//! `gql:project`: its columns are compiled by `aggregate::compile_aggregating` (a column without
//! an aggregate call is a grouping key), and `DISTINCT`, `ORDER BY`, `OFFSET` and `LIMIT` follow
//! it as after `gql:project`. Its output rows are generated traversers (fresh paths), so an
//! aggregating `WITH` row read later is labelled `#w<k>` like any materializing `WITH` row.

use std::sync::Arc;

use super::aggregate::{AggregateCall, Aggregating, compile_aggregating};
use super::ast::{
    BinaryOp, CallArgs, EdgeDirection, Expr, ExprKind, Identifier, LabelExpr, Literal, Quantifier,
    SortDirection,
};
use super::bind::{
    BindingId, BindingKind, BoundQuery, Column, ColumnRef, Conjunct, Count, Predicate, Segment,
    SortKey, bind_with_parameters,
};
use super::eval::{Scope, Value, compile};
use super::lexer::Span;
use super::parser::parse;
use super::plan::{
    ElementFilters, Expand, ForStage, LogicalPlan, Materialize, Op, PushOp, PushOperand, Pushdown,
    ResultOp, Stage, and_factors, label_disjuncts,
};
use super::planner::{StorageFacts, plan};
use super::runtime::{Binding, GqlElement, PathPart, RowExpr, eval_error};
use crate::error::{GqlErrorBuilder, GqlErrorKind, TraverserError};
use crate::graph::GraphStorage;
use crate::traversal::step::GraphStep;
use crate::traversal::step::flatbarrier_steps::step_barrier::lazy_barrier_step;
use crate::traversal::step::flatbarrier_steps::step_gql_aggregate::GqlAggregateColumn;
use crate::traversal::step::flatbarrier_steps::step_gql_order::GqlSortKey;
use crate::traversal::step::flatmap_steps::step_gql_for::GqlOrdinal;
use crate::traversal::step::map_steps::step_gql_project::GqlColumn;
use crate::value::{Number, P, PValue};
use crate::{
    AnonymousTraversal, ArgStrings, CompactString, ElementKind, ElementProperty,
    GraphTraversalSource,
};

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Runs a GQL read query (ISO/IEC 39075, feature `gql`): `MATCH` and `OPTIONAL MATCH`
    /// patterns, `WHERE`, the linear pipeline `LET`, `FILTER`, `WITH` (Cypher) and `FOR x IN
    /// list [WITH ORDINALITY i]` (Cypher `UNWIND list AS x`), a query that starts without
    /// `MATCH` (`RETURN 1 + 2 AS x`, `FOR id IN $ids MATCH ..`), `RETURN`
    /// (mandatory; `RETURN *` returns every named variable), `ORDER BY`, `OFFSET` (Cypher `SKIP`)
    /// and `LIMIT`, and the aggregates `count`, `sum`, `avg`, `min`, `max` and `collect_list`
    /// (Cypher `collect`) in `RETURN` and `WITH`, grouped by the items without an aggregate
    /// (NULL skipped except by `count(*)`), and quantified edges (`-[e:knows]->{1,3}`, `*`, `+`,
    /// Cypher `[e:knows*1..3]`) with trail semantics (no edge twice in one match; the edge
    /// variable is the list of the edges), and path variables (`MATCH p = (a)-->(b) RETURN p`,
    /// `path_length(p)`, Cypher `length(p)`; a path is the Gremlin path value). Use
    /// [`Self::gql_with`] for a query with `$parameters`.
    ///
    /// Every result is one map per row: the keys are the column names (the `AS` alias, else the
    /// expression as written) in written order, a missing property is a `null` value (never a
    /// missing key), and a returned node or edge stays the live element, so Gremlin steps may
    /// follow (`select("p").out("knows")`). `WHERE` uses three-valued logic: a row passes only
    /// when the condition is TRUE (a missing property, a stored `null` and a comparison across
    /// types are NULL). Without `ORDER BY` the row order is unspecified. `ORDER BY` sorts `NULL`
    /// last ascending and first descending.
    ///
    /// The query is compiled into ordinary steps (`.profile()` shows them, with internal
    /// `gql:*` steps where Gremlin has no equivalent), so property indexes, the optimizer,
    /// limits and permissions apply as to a Gremlin traversal. A query that does not parse or
    /// bind fails before the traversal runs, with a [`TraverserError::Gql`] carrying the
    /// GQLSTATUS code and a caret under the offending text; building the traversal itself never
    /// fails. `gql()` starts a traversal: called after other steps, the traversal fails the
    /// same way.
    ///
    /// ```
    /// use graphersal::prelude::*;
    ///
    /// let graph = TraversalGraph::tinkerpop_modern();
    /// let rows = graph
    ///     .traversal()
    ///     .gql("MATCH (p:person) WHERE p.age > 30 RETURN p.name AS name ORDER BY name")
    ///     .select("name")
    ///     .to_list()
    ///     .unwrap();
    /// assert_eq!(rows, ["josh".into(), "peter".into()]);
    ///
    /// // An error fails the traversal before it runs, even on an empty graph.
    /// let empty = TraversalGraph::new();
    /// let error = empty.traversal().gql("MATCH (p) RETRUN p").to_list().unwrap_err();
    /// assert!(matches!(error.root_cause(), TraverserError::Gql { .. }));
    /// ```
    pub fn gql(self, query: &str) -> Self {
        self.gql_with(
            query,
            std::iter::empty::<(CompactString, ElementProperty)>(),
        )
    }

    /// Runs a GQL read query with parameter values: `$name` in the query reads the value given
    /// for `name` (a value, never query text). A parameter the query reads but `params` lacks
    /// fails the traversal before it runs; unused parameters are ignored. Otherwise as
    /// [`Self::gql`].
    ///
    /// ```
    /// use graphersal::prelude::*;
    ///
    /// let graph = TraversalGraph::tinkerpop_modern();
    /// let ages = graph
    ///     .traversal()
    ///     .gql_with(
    ///         "MATCH (p:person {name: $name}) RETURN p.age AS age",
    ///         [("name", ElementProperty::from("marko"))],
    ///     )
    ///     .select("age")
    ///     .to_list()
    ///     .unwrap();
    /// assert_eq!(ages, [ElementProperty::from(29)]);
    /// ```
    pub fn gql_with<I, K, V>(self, query: &str, params: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<CompactString>,
        V: Into<ElementProperty>,
    {
        let params: Vec<(CompactString, ElementProperty)> = params
            .into_iter()
            .map(|(name, value)| (name.into(), value.into()))
            .collect();
        if !self.steps.is_empty() {
            return self.gql_error(not_at_start(query));
        }
        let lowered = {
            let facts = StorageFacts::from_storage(&self.storage);
            prepare::<S>(query, &params, &facts)
        };
        match lowered {
            Ok(lowered) => lowered.emit(self),
            Err(error) => self.v(None).gql_error(error),
        }
    }
}

/// The error of `gql()` called after other steps (a start step in this version, design D4).
fn not_at_start(query: &str) -> TraverserError {
    GqlErrorBuilder::new(
        GqlErrorKind::NotYet,
        0..query.len(),
        "gql() starts a traversal in this version; it cannot follow other steps",
    )
    .alternative(start_with_gql(query))
    .into_error(query)
}

/// The advice of a `gql()` that does not start the traversal, built from its `query`: start
/// with it, express what the steps before it selected in the pattern, and continue with Gremlin
/// steps after it.
pub(crate) fn start_with_gql(query: &str) -> String {
    format!(
        "start the traversal with it, as in g.gql({}), and write what the steps before it \
         selected as the MATCH pattern and its WHERE condition; Gremlin steps may follow it, as \
         in g.gql(\"MATCH (p:person) RETURN p\").select(\"p\").out(\"created\")",
        crate::value::literal::rhai_string(query)
    )
}

/// `name` as the key of a Rhai object map literal: bare when it is an identifier, quoted
/// otherwise (a back-quoted GQL parameter name may hold any character).
pub(crate) fn rhai_map_key(name: &str) -> String {
    let identifier = name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if identifier {
        name.to_string()
    } else {
        crate::value::literal::rhai_string(name)
    }
}

/// Where a step runs: the binding the traverser stands on (read as `Binding::Current`) and the
/// materializing `WITH` or `FOR` whose row is the current object (its columns read as
/// `Binding::Column`; the item of a `FOR` without an ordinal, which is the row itself, as
/// `Binding::Current`). Every other binding is read through its path label, a column of a row
/// left behind through the row's label (`Binding::RowColumn`; the item of a `FOR` without an
/// ordinal as `Binding::Label` of its row label).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Site {
    /// The binding the traverser stands on.
    current: Option<BindingId>,
    /// The segment of the materializing `WITH` or `FOR` whose row is the current object.
    row: Option<usize>,
    /// The `OPTIONAL MATCH` segment whose part the step runs in: a group variable of that part
    /// reads its list there without the part's anchor (the part matched so far).
    part: Option<usize>,
}

impl Site {
    /// On the element of binding `id`.
    fn on(id: BindingId) -> Self {
        Site {
            current: Some(id),
            row: None,
            part: None,
        }
    }

    /// On the row of the materializing `WITH` or `FOR` of segment `segment`.
    fn row(segment: usize) -> Self {
        Site {
            current: None,
            row: Some(segment),
            part: None,
        }
    }
}

/// One step of the lowered plan, every fallible part already done.
enum Instr<S: GraphStorage> {
    /// `v()`.
    Scan,
    /// `inject(0)`: the one unit row a query that does not start with `MATCH` starts from
    /// (decision C13, design M1).
    Unit,
    /// `has_label(labels)`.
    HasLabel(Vec<String>),
    /// `has(key, predicate)`.
    Has(String, P<PValue>),
    /// `gql:filter(condition)` at `site`.
    Filter { condition: RowExpr<S>, site: Site },
    /// `gql:filter(condition)` comparing the current element (the extra slot after the
    /// bindings) with a materializing `WITH` column binding: a join, an edge join or a trail
    /// check against an element the row holds, which has no path label of its own.
    RowCheck(RowExpr<S>),
    /// `as(label)` of the binding, emitted only when a label is needed.
    Label(BindingId),
    /// `as("#w<k>")` of the row of the materializing `WITH` of segment `k` (of a `FOR`:
    /// `as("#f<k>")` on its record, `as("x")` on the item itself without an ordinal), emitted
    /// only when a later step reads the row after leaving it (`Binding::RowColumn`).
    RowLabel(usize),
    /// `gql:for(variable IN list)` at `site` (`FOR` / `UNWIND`, design mechanism 2).
    For {
        list: RowExpr<S>,
        site: Site,
        variable: CompactString,
        ordinal: Option<GqlOrdinal>,
    },
    /// `gql:project(columns)` at `site`.
    Project {
        columns: Vec<GqlColumn<S>>,
        site: Site,
    },
    /// `gql:aggregate(keys, aggregates, columns)` at `site`: an aggregating `RETURN` / `WITH`.
    Aggregate {
        columns: Vec<GqlAggregateColumn<S>>,
        calls: Vec<AggregateCall<GqlElement<S>>>,
        site: Site,
    },
    /// `gql:distinct()` (GQL equality, decision C3).
    Distinct,
    /// `gql:order(keys)`, on the row map of the `gql:project` before it (`columns` columns).
    Order {
        keys: Vec<GqlSortKey<S>>,
        columns: usize,
    },
    /// `skip(n)`.
    Skip(usize),
    /// `limit(n)`.
    Limit(usize),
    /// One pattern edge walked from the current vertex ([`Op::Expand`]); see [`Hop`].
    Hop(Hop<S>),
    /// `select(label)`: back to a binding labelled earlier (a branch of the pattern, a later
    /// segment starting at a node bound before it).
    Select(BindingId),
    /// `gql:bind(name)`: moves to the element a materializing `WITH` column binding holds, read
    /// at `site` (a later segment starting at it, a branch back to it).
    Bind {
        id: BindingId,
        site: Site,
        name: CompactString,
        kind: ElementKind,
    },
    /// `where(P.eq(label))`: the current element is the one bound earlier to the binding (a
    /// join: a cycle closing, a node or edge variable repeated).
    Same(BindingId),
    /// `where(P.neq(label))`: the current edge differs from an earlier edge (trail).
    Differs(BindingId),
    /// `gql:trail(labels)`: the current edge differs from every occurrence of these bindings'
    /// labels (trail against a group variable, or the trail check in the body of a quantified
    /// edge, its own label included; plan mechanism 5).
    Trail(Vec<BindingId>),
    /// `as("#o<k>")` at the end of the part of `OPTIONAL MATCH` segment `k` that binds no new
    /// node: the anchor that tells a part not matched (its group variables `NULL`) from a
    /// zero-length match (`[]`); emitted only when a group variable of the part is read.
    PartLabel(usize),
    /// A merge point (`lazy_barrier()`) right after a hop whose next hop walks `direction`
    /// over `labels` (the step the merge point judges its fan-out with).
    Merge {
        direction: EdgeDirection,
        labels: Option<Vec<String>>,
    },
    /// `optional(__.<steps>)`: the part of an `OPTIONAL MATCH` (plan mechanism 4).
    Optional(Vec<Instr<S>>),
}

/// One walked pattern edge. The form is chosen at emission, once the labels are known: the
/// vertex step (`out(labels)`, `in(labels)`) when nothing runs on the edge traverser, else the
/// edge step, `steps` on the edge, then the vertex at the other end (`out_e(labels)..in_v()`,
/// `in_e(labels)..out_v()`). Either direction is a `union` of both, the incoming branch without
/// self-loops, so a self-loop matches once.
struct Hop<S: GraphStorage> {
    /// The direction as seen from the current vertex.
    direction: EdgeDirection,
    /// The labels of the adjacency step (`None`: any edge).
    labels: Option<Vec<String>>,
    /// The steps on the edge traverser: label factors, pushed `has()`, join and trail checks,
    /// `gql:filter`s, the edge's `as()` placeholder.
    steps: Vec<Instr<S>>,
    /// The quantifier of a quantified edge: the hop is the body of a `repeat()` (see
    /// [`Lowered::quantified`]).
    quantifier: Option<Quantifier>,
    /// The group variable of a quantified edge: the vertex each iteration reaches gets its
    /// inner label (`as("#gv<id>")`) when a path variable reads the group.
    inner: Option<BindingId>,
}

/// The lowered query, ready to be emitted onto a source.
struct Lowered<S: GraphStorage> {
    query: Arc<str>,
    instrs: Vec<Instr<S>>,
    /// The path label of each binding.
    labels: Vec<CompactString>,
    /// Whether each binding gets `as()`.
    labelled: Vec<bool>,
    /// The materializing `WITH` column each binding stands for (`None`: a pattern binding).
    columns: Vec<Option<ColumnRef>>,
    /// The row label of each segment (`#w<k>`; used only for a materializing `WITH`).
    row_labels: Vec<CompactString>,
    /// Whether the row of each segment gets its row label.
    rows_labelled: Vec<bool>,
    /// Per segment: a `FOR` without an ordinal, whose row is the item itself (its item reads
    /// `Binding::Current` on the row, `Binding::Label` of the row label after it).
    items: Vec<bool>,
    /// Per binding: the `OPTIONAL MATCH` part and the anchor of a group variable (`None`: not a
    /// group variable).
    groups: Vec<Option<GroupRead>>,
    /// The part label of each `OPTIONAL MATCH` segment that needs one (`#o<k>`, see
    /// [`Instr::PartLabel`]); empty for the others.
    part_labels: Vec<CompactString>,
    /// Per binding: the inner label of a group variable a path variable reads (`#gv<id>`, set
    /// on the vertex each iteration reaches); empty for the others.
    inner_labels: Vec<CompactString>,
    /// Per binding: how a path variable is assembled (`None`: not a path variable).
    paths: Vec<Option<PathRead>>,
}

/// How a path variable is read (`Binding::Path`, plan mechanism 6).
#[derive(Debug, Clone)]
struct PathRead {
    /// The segment that binds it.
    segment: usize,
    /// The parts in text order: the labels of the pattern's nodes and edges, a quantified
    /// edge as its edges' label and its inner vertices' label.
    parts: Vec<PathPart>,
    /// The label the path holds whenever its `OPTIONAL MATCH` part matched (as for a group
    /// variable); `None` outside an `OPTIONAL MATCH`.
    anchor: Option<CompactString>,
}

/// How a group variable is read outside its element pattern (`Binding::Group`).
#[derive(Debug, Clone)]
struct GroupRead {
    /// The segment that binds it.
    segment: usize,
    /// The label the path holds whenever its `OPTIONAL MATCH` part matched (the part's witness
    /// node or its part label); `None` outside an `OPTIONAL MATCH`.
    anchor: Option<CompactString>,
    /// Walked from the node written after the edge ([`Expand::reversed`]): the list is the
    /// reverse of path order.
    reversed: bool,
}

impl<S: GraphStorage> Lowered<S> {
    /// Where variable `slot` reads from at `site` (see [`Site`]).
    fn binding(&self, slot: usize, site: Site) -> Binding {
        if let Some(Some(path)) = self.paths.get(slot) {
            // Inside its own part the part matched so far: no anchor to check.
            let anchor = path
                .anchor
                .as_ref()
                .filter(|_| site.part != Some(path.segment));
            let parts = anchor
                .map(|label| PathPart::Anchor(label.clone()))
                .into_iter()
                .chain(path.parts.iter().cloned())
                .collect();
            return Binding::Path(parts);
        }
        if site.current == Some(BindingId(slot)) {
            return Binding::Current;
        }
        if let Some(Some(group)) = self.groups.get(slot) {
            return Binding::Group {
                label: self.labels.get(slot).cloned().unwrap_or_default(),
                // Inside its own part the part matched so far: no anchor to check.
                anchor: if site.part == Some(group.segment) {
                    None
                } else {
                    group.anchor.clone()
                },
                reversed: group.reversed,
            };
        }
        match self.columns.get(slot).copied().flatten() {
            Some(column) if self.items.get(column.segment).copied().unwrap_or(false) => {
                if site.row == Some(column.segment) {
                    Binding::Current
                } else {
                    Binding::Label(self.row_label(column.segment))
                }
            }
            Some(column) if site.row == Some(column.segment) => Binding::Column(column.column),
            Some(column) => Binding::RowColumn {
                label: self.row_label(column.segment),
                column: column.column,
            },
            None => Binding::Label(self.labels.get(slot).cloned().unwrap_or_default()),
        }
    }

    /// Where each variable slot reads from at `site`.
    fn bindings(&self, site: Site) -> Arc<[Binding]> {
        (0..self.labels.len())
            .map(|slot| self.binding(slot, site))
            .collect()
    }

    /// The slots of `ORDER BY` on a new row map of `columns` columns: every binding read away
    /// from its row, then the row's columns.
    fn order_bindings(&self, columns: usize) -> Arc<[Binding]> {
        (0..self.labels.len())
            .map(|slot| self.binding(slot, Site::default()))
            .chain((0..columns).map(Binding::Column))
            .collect()
    }

    /// The slots of a [`Instr::RowCheck`]: every binding read away from its row, then the
    /// current element.
    fn check_bindings(&self) -> Arc<[Binding]> {
        (0..self.labels.len())
            .map(|slot| self.binding(slot, Site::default()))
            .chain(std::iter::once(Binding::Current))
            .collect()
    }

    /// The row label of segment `segment`.
    fn row_label(&self, segment: usize) -> CompactString {
        self.row_labels.get(segment).cloned().unwrap_or_default()
    }

    fn emit<'g>(mut self, source: GraphTraversalSource<'g, S>) -> GraphTraversalSource<'g, S>
    where
        S: 'g,
    {
        let instrs = std::mem::take(&mut self.instrs);
        self.emit_into(source, instrs)
    }

    /// Emits `instrs` onto `g` (the source, or the child of an `optional()`).
    fn emit_into<'g, T: Sink<'g, S>>(&self, mut g: T, instrs: Vec<Instr<S>>) -> T
    where
        S: 'g,
    {
        for instr in instrs {
            g = match instr {
                Instr::Scan => g.v(ArgStrings::None),
                Instr::Unit => g.inject(vec![ElementProperty::from(0_i64)]),
                Instr::HasLabel(labels) => g.has_label(labels),
                Instr::Has(key, predicate) => g.has_p(key, predicate),
                Instr::Filter { condition, site } => {
                    g.gql_filter(Arc::clone(&self.query), condition, self.bindings(site))
                }
                Instr::RowCheck(condition) => {
                    g.gql_filter(Arc::clone(&self.query), condition, self.check_bindings())
                }
                Instr::Label(id) => {
                    if self.labelled.get(id.0).copied().unwrap_or(false)
                        && let Some(label) = self.labels.get(id.0)
                    {
                        g.as_(label.clone())
                    } else {
                        g
                    }
                }
                Instr::RowLabel(segment) => {
                    if self.rows_labelled.get(segment).copied().unwrap_or(false) {
                        g.as_(self.row_label(segment))
                    } else {
                        g
                    }
                }
                Instr::Project { columns, site } => {
                    g.gql_project(Arc::clone(&self.query), columns, self.bindings(site))
                }
                Instr::For {
                    list,
                    site,
                    variable,
                    ordinal,
                } => g.gql_for(
                    Arc::clone(&self.query),
                    variable,
                    list,
                    self.bindings(site),
                    ordinal,
                ),
                Instr::Aggregate {
                    columns,
                    calls,
                    site,
                } => g.gql_aggregate(Arc::clone(&self.query), columns, calls, self.bindings(site)),
                Instr::Distinct => g.gql_distinct(),
                Instr::Order { keys, columns } => {
                    g.gql_order(Arc::clone(&self.query), keys, self.order_bindings(columns))
                }
                Instr::Skip(n) => g.skip(n),
                Instr::Limit(n) => g.limit(n),
                Instr::Hop(hop) => match hop.quantifier {
                    Some(quantifier) => self.quantified(g, hop, quantifier),
                    None => self.hop(g, hop),
                },
                Instr::Select(id) => g.select(self.label(id)),
                Instr::Bind {
                    id,
                    site,
                    name,
                    kind,
                } => g.gql_bind(name, self.binding(id.0, site), kind),
                Instr::Same(id) => g.where_p(P::Eq(PValue::String(self.label(id)))),
                Instr::Differs(id) => g.where_p(P::Neq(PValue::String(self.label(id)))),
                Instr::Trail(ids) => g.gql_trail(ids.iter().map(|&id| self.label(id)).collect()),
                Instr::PartLabel(segment) => match self.part_labels.get(segment) {
                    Some(label) if !label.is_empty() => g.as_(label.clone()),
                    _ => g,
                },
                Instr::Merge { direction, labels } => {
                    let walk = AnonymousTraversal::<'g, S>::new();
                    let mut walk = match direction {
                        EdgeDirection::Right => walk.out(adjacency(&labels)),
                        EdgeDirection::Left => walk.in_(adjacency(&labels)),
                        EdgeDirection::Any => walk.both(adjacency(&labels)),
                    };
                    match walk.steps.pop() {
                        Some(GraphStep::FlatMap(_, next)) => g.push_step(lazy_barrier_step(next)),
                        _ => g,
                    }
                }
                Instr::Optional(steps) => {
                    let part = self.emit_into(AnonymousTraversal::new(), steps);
                    g.optional(part)
                }
            };
        }
        g
    }

    /// The path label of binding `id`.
    fn label(&self, id: BindingId) -> CompactString {
        self.labels.get(id.0).cloned().unwrap_or_default()
    }

    /// Whether binding `id` gets `as()`.
    fn is_labelled(&self, id: BindingId) -> bool {
        self.labelled.get(id.0).copied().unwrap_or(false)
    }

    /// Emits a hop (see [`Hop`]).
    fn hop<'g, T: Sink<'g, S>>(&self, g: T, hop: Hop<S>) -> T
    where
        S: 'g,
    {
        let labels = || adjacency(&hop.labels);
        let on_edge = hop
            .steps
            .iter()
            .any(|instr| !matches!(instr, Instr::Label(id) if !self.is_labelled(*id)));
        if !on_edge {
            return match hop.direction {
                EdgeDirection::Right => g.out(labels()),
                EdgeDirection::Left => g.in_(labels()),
                EdgeDirection::Any => g.union(vec![
                    AnonymousTraversal::new().out(labels()),
                    AnonymousTraversal::new()
                        .in_e(labels())
                        .gql_not_self_loop()
                        .out_v(),
                ]),
            };
        }
        let mut edge = AnonymousTraversal::<'g, S>::new();
        for instr in hop.steps {
            edge = self.edge_step(edge, instr);
        }
        let append = |mut t: AnonymousTraversal<'g, S>, steps: &[GraphStep<'g, S>]| {
            for step in steps {
                t = t.push_step(step.clone());
            }
            t
        };
        match hop.direction {
            EdgeDirection::Right => {
                let mut g = g.out_e(labels());
                for step in edge.steps {
                    g = g.push_step(step);
                }
                g.in_v()
            }
            EdgeDirection::Left => {
                let mut g = g.in_e(labels());
                for step in edge.steps {
                    g = g.push_step(step);
                }
                g.out_v()
            }
            EdgeDirection::Any => {
                let outgoing =
                    append(AnonymousTraversal::new().out_e(labels()), &edge.steps).in_v();
                let incoming = append(
                    AnonymousTraversal::new().in_e(labels()).gql_not_self_loop(),
                    &edge.steps,
                )
                .out_v();
                g.union(vec![outgoing, incoming])
            }
        }
    }

    /// Emits a quantified edge (plan mechanism 5): the single hop is the body of a `repeat()`
    /// whose modulators give the bounds. `{m,n}`: `repeat(B).emit(__.loops().is(P.gte(m)))
    /// .times(n)` (`emit()` for `m = 1`); `{n}`: `repeat(B).times(n)`; `{0,n}`:
    /// `emit().repeat(B).times(n)` (the zero-length match first); no upper bound: no `times()`,
    /// the loop ends when trail leaves no unused edge, or fails with `repeat.max_loops` (D14:
    /// an error, never a cut result). An upper bound beyond `u32::MAX` is no bound (the loop
    /// counter is a `u32`); `{0}` emits no step at all (see [`Lowering::ops`]).
    fn quantified<'g, T: Sink<'g, S>>(&self, g: T, hop: Hop<S>, quantifier: Quantifier) -> T
    where
        S: 'g,
    {
        let inner = hop
            .inner
            .and_then(|id| self.inner_labels.get(id.0))
            .filter(|label| !label.is_empty())
            .cloned();
        let body = self.hop(AnonymousTraversal::<'g, S>::new(), hop);
        let body = match inner {
            Some(label) => body.as_(label),
            None => body,
        };
        let max = quantifier.max.and_then(|n| u32::try_from(n).ok());
        let at_least = || {
            let min = i64::try_from(quantifier.min).unwrap_or(i64::MAX);
            AnonymousTraversal::<'g, S>::new()
                .loops()
                .is(P::Gte(PValue::Number(Number::from_i64(min))))
        };
        let walk = AnonymousTraversal::<'g, S>::new();
        let walk = match (quantifier.min, max) {
            (0, Some(0)) => return g,
            (0, Some(n)) => walk.emit().repeat(body).times(n),
            (0, None) => walk.emit().repeat(body),
            (m, Some(n)) if u64::from(n) == m => walk.repeat(body).times(n),
            (1, Some(n)) => walk.repeat(body).emit().times(n),
            (1, None) => walk.repeat(body).emit(),
            (_, Some(n)) => walk.repeat(body).emit_with(at_least()).times(n),
            (_, None) => walk.repeat(body).emit_with(at_least()),
        };
        let mut g = g;
        for step in walk.steps {
            g = g.push_step(step);
        }
        g
    }

    /// Emits one step on the edge traverser of a hop.
    fn edge_step<'g>(
        &self,
        t: AnonymousTraversal<'g, S>,
        instr: Instr<S>,
    ) -> AnonymousTraversal<'g, S>
    where
        S: 'g,
    {
        match instr {
            Instr::HasLabel(labels) => t.has_label(labels),
            Instr::Has(key, predicate) => t.has_p(key, predicate),
            Instr::Filter { condition, site } => {
                t.gql_filter(Arc::clone(&self.query), condition, self.bindings(site))
            }
            Instr::RowCheck(condition) => {
                t.gql_filter(Arc::clone(&self.query), condition, self.check_bindings())
            }
            Instr::Label(id) if self.is_labelled(id) => t.as_(self.label(id)),
            Instr::Same(id) => t.where_p(P::Eq(PValue::String(self.label(id)))),
            Instr::Differs(id) => t.where_p(P::Neq(PValue::String(self.label(id)))),
            Instr::Trail(ids) => t.gql_trail(ids.iter().map(|&id| self.label(id)).collect()),
            Instr::Label(_)
            | Instr::PartLabel(_)
            | Instr::Scan
            | Instr::Unit
            | Instr::RowLabel(_)
            | Instr::Project { .. }
            | Instr::For { .. }
            | Instr::Aggregate { .. }
            | Instr::Distinct
            | Instr::Order { .. }
            | Instr::Skip(_)
            | Instr::Limit(_)
            | Instr::Hop(_)
            | Instr::Select(_)
            | Instr::Bind { .. }
            | Instr::Merge { .. }
            | Instr::Optional(_) => t,
        }
    }
}

/// The traversal the lowering emits onto: the source (`g`), or a child traversal (`__`: the
/// part inside `optional()`). The same builders on both, so one emission serves both.
trait Sink<'g, S: GraphStorage + 'g>: Sized {
    fn v(self, ids: ArgStrings) -> Self;
    fn inject(self, values: Vec<ElementProperty>) -> Self;
    fn has_label(self, labels: Vec<String>) -> Self;
    fn has_p(self, key: String, predicate: P<PValue>) -> Self;
    fn gql_filter(self, query: Arc<str>, condition: RowExpr<S>, bindings: Arc<[Binding]>) -> Self;
    fn as_(self, label: CompactString) -> Self;
    fn gql_project(
        self,
        query: Arc<str>,
        columns: Vec<GqlColumn<S>>,
        bindings: Arc<[Binding]>,
    ) -> Self;
    fn gql_aggregate(
        self,
        query: Arc<str>,
        columns: Vec<GqlAggregateColumn<S>>,
        calls: Vec<AggregateCall<GqlElement<S>>>,
        bindings: Arc<[Binding]>,
    ) -> Self;
    fn gql_for(
        self,
        query: Arc<str>,
        variable: CompactString,
        list: RowExpr<S>,
        bindings: Arc<[Binding]>,
        ordinal: Option<GqlOrdinal>,
    ) -> Self;
    fn gql_distinct(self) -> Self;
    fn gql_order(self, query: Arc<str>, keys: Vec<GqlSortKey<S>>, bindings: Arc<[Binding]>)
    -> Self;
    fn skip(self, n: usize) -> Self;
    fn limit(self, n: usize) -> Self;
    fn select(self, label: CompactString) -> Self;
    fn gql_bind(self, name: CompactString, binding: Binding, kind: ElementKind) -> Self;
    fn where_p(self, predicate: P<PValue>) -> Self;
    fn gql_trail(self, labels: Vec<CompactString>) -> Self;
    fn out(self, labels: ArgStrings) -> Self;
    fn in_(self, labels: ArgStrings) -> Self;
    fn out_e(self, labels: ArgStrings) -> Self;
    fn in_e(self, labels: ArgStrings) -> Self;
    fn in_v(self) -> Self;
    fn out_v(self) -> Self;
    fn union(self, branches: Vec<AnonymousTraversal<'g, S>>) -> Self;
    fn optional(self, part: AnonymousTraversal<'g, S>) -> Self;
    fn push_step(self, step: GraphStep<'g, S>) -> Self;
}

/// [`Sink`] for a traversal type with the builders of the same names (inherent methods take
/// precedence over the trait's, so each body calls the builder).
macro_rules! sink {
    ($traversal:ident) => {
        impl<'g, S: GraphStorage + 'g> Sink<'g, S> for $traversal<'g, S> {
            fn v(self, ids: ArgStrings) -> Self {
                self.v(ids)
            }
            fn inject(self, values: Vec<ElementProperty>) -> Self {
                self.inject(values)
            }
            fn has_label(self, labels: Vec<String>) -> Self {
                self.has_label(labels)
            }
            fn has_p(self, key: String, predicate: P<PValue>) -> Self {
                self.has_p(key, predicate)
            }
            fn gql_filter(
                self,
                query: Arc<str>,
                condition: RowExpr<S>,
                bindings: Arc<[Binding]>,
            ) -> Self {
                self.gql_filter(query, condition, bindings)
            }
            fn as_(self, label: CompactString) -> Self {
                self.as_(label)
            }
            fn gql_project(
                self,
                query: Arc<str>,
                columns: Vec<GqlColumn<S>>,
                bindings: Arc<[Binding]>,
            ) -> Self {
                self.gql_project(query, columns, bindings)
            }
            fn gql_aggregate(
                self,
                query: Arc<str>,
                columns: Vec<GqlAggregateColumn<S>>,
                calls: Vec<AggregateCall<GqlElement<S>>>,
                bindings: Arc<[Binding]>,
            ) -> Self {
                self.gql_aggregate(query, columns, calls, bindings)
            }
            fn gql_for(
                self,
                query: Arc<str>,
                variable: CompactString,
                list: RowExpr<S>,
                bindings: Arc<[Binding]>,
                ordinal: Option<GqlOrdinal>,
            ) -> Self {
                self.gql_for(query, variable, list, bindings, ordinal)
            }
            fn gql_distinct(self) -> Self {
                self.gql_distinct()
            }
            fn gql_order(
                self,
                query: Arc<str>,
                keys: Vec<GqlSortKey<S>>,
                bindings: Arc<[Binding]>,
            ) -> Self {
                self.gql_order(query, keys, bindings)
            }
            fn skip(self, n: usize) -> Self {
                self.skip(n)
            }
            fn limit(self, n: usize) -> Self {
                self.limit(n)
            }
            fn select(self, label: CompactString) -> Self {
                self.select(label)
            }
            fn gql_bind(self, name: CompactString, binding: Binding, kind: ElementKind) -> Self {
                self.gql_bind(name, binding, kind)
            }
            fn where_p(self, predicate: P<PValue>) -> Self {
                self.where_p(predicate)
            }
            fn gql_trail(self, labels: Vec<CompactString>) -> Self {
                self.gql_trail(labels)
            }
            fn out(self, labels: ArgStrings) -> Self {
                self.out(labels)
            }
            fn in_(self, labels: ArgStrings) -> Self {
                self.in_(labels)
            }
            fn out_e(self, labels: ArgStrings) -> Self {
                self.out_e(labels)
            }
            fn in_e(self, labels: ArgStrings) -> Self {
                self.in_e(labels)
            }
            fn in_v(self) -> Self {
                self.in_v()
            }
            fn out_v(self) -> Self {
                self.out_v()
            }
            fn union(self, branches: Vec<AnonymousTraversal<'g, S>>) -> Self {
                self.union(branches)
            }
            fn optional(self, part: AnonymousTraversal<'g, S>) -> Self {
                self.optional(part)
            }
            fn push_step(self, step: GraphStep<'g, S>) -> Self {
                self.push_step(step)
            }
        }
    };
}

sink!(GraphTraversalSource);
sink!(AnonymousTraversal);

/// The labels argument of an adjacency step.
fn adjacency(labels: &Option<Vec<String>>) -> ArgStrings {
    match labels {
        Some(labels) => ArgStrings::List(labels.clone()),
        None => ArgStrings::None,
    }
}

/// Parses, binds, plans and lowers `query`; every error is the [`TraverserError::Gql`] the
/// marker raises.
fn prepare<S: GraphStorage>(
    query: &str,
    params: &[(CompactString, ElementProperty)],
    facts: &StorageFacts<'_>,
) -> Result<Lowered<S>, TraverserError> {
    let parsed = parse(query)?;
    let bound = bind_with_parameters(&parsed, params.iter().map(|(name, _)| name.as_str()))?;
    let plan = plan(&bound, facts, None);
    Lowering::new(query, &bound, &plan, params).run()
}

/// The names a compiled expression resolves (slot `i` < bindings: binding `i`; then the
/// `RETURN` columns for `ORDER BY`).
struct LowerScope<'a> {
    /// The key of each binding ([`Binding::key`](super::bind::Binding), the name stored
    /// expressions use; `None`: anonymous).
    names: &'a [Option<String>],
    /// The slot the reserved empty name stands for: the element a synthesized expression
    /// (an inline property of an anonymous node, a label factor) is about, or an `ORDER BY`
    /// key that is a whole column. No query can write the empty name.
    this: Option<usize>,
    /// The aliased `RETURN` columns `ORDER BY` may name, with their slots (an alias shadows a
    /// variable of the same name).
    aliases: &'a [(String, usize)],
    params: &'a [(CompactString, ElementProperty)],
}

impl Scope for LowerScope<'_> {
    fn variable(&self, name: &str) -> Option<usize> {
        if name.is_empty() {
            return self.this;
        }
        if let Some((_, slot)) = self.aliases.iter().find(|(alias, _)| alias == name) {
            return Some(*slot);
        }
        self.names.iter().position(|n| n.as_deref() == Some(name))
    }

    fn parameter(&self, name: &str) -> Option<ElementProperty> {
        parameter(self.params, name).cloned()
    }
}

/// The value given for `$name` (the last one when given twice).
fn parameter<'p>(
    params: &'p [(CompactString, ElementProperty)],
    name: &str,
) -> Option<&'p ElementProperty> {
    params
        .iter()
        .rev()
        .find(|(n, _)| n.as_str() == name)
        .map(|(_, value)| value)
}

/// The labels for the step that reaches an element (`None`: no label restriction there) and the
/// steps that run on the element.
type ElementSteps<S> = (Option<Vec<String>>, Vec<Instr<S>>);

struct Lowering<'q> {
    query: &'q str,
    shared: Arc<str>,
    bound: &'q BoundQuery,
    plan: &'q LogicalPlan,
    params: &'q [(CompactString, ElementProperty)],
    names: Vec<Option<String>>,
}

impl<'q> Lowering<'q> {
    fn new(
        query: &'q str,
        bound: &'q BoundQuery,
        plan: &'q LogicalPlan,
        params: &'q [(CompactString, ElementProperty)],
    ) -> Self {
        Lowering {
            query,
            shared: Arc::from(query),
            bound,
            plan,
            params,
            names: bound.bindings.iter().map(|b| b.key.clone()).collect(),
        }
    }

    fn scope<'s>(&'s self, this: Option<usize>, aliases: &'s [(String, usize)]) -> LowerScope<'s> {
        LowerScope {
            names: &self.names,
            this,
            aliases,
            params: self.params,
        }
    }

    fn compile<S: GraphStorage>(
        &self,
        expr: &Expr,
        this: Option<usize>,
        aliases: &[(String, usize)],
    ) -> Result<RowExpr<S>, TraverserError> {
        compile(expr, &self.scope(this, aliases)).map_err(|error| eval_error(self.query, error))
    }

    fn run<S: GraphStorage>(self) -> Result<Lowered<S>, TraverserError> {
        let mut instrs = Vec::new();
        // A query that does not start with `MATCH` (`OPTIONAL MATCH`, `FOR`, `WITH`, `RETURN`,
        // or the conjuncts of a leading `LET` / `FILTER` pipeline) starts from one unit row.
        if !matches!(self.plan.ops.first(), Some(Op::Scan(_) | Op::Anchor(_))) {
            instrs.push(Instr::Unit);
        }
        let mut site = self.ops(&self.plan.ops, Site::default(), &mut instrs)?;
        for stage in &self.plan.stages {
            site = match stage {
                Stage::Match { ops, .. } => self.ops(ops, site, &mut instrs)?,
                Stage::Optional { ops, segment, .. } => {
                    let mut part = Vec::new();
                    self.ops(ops, site, &mut part)?;
                    part.push(Instr::PartLabel(*segment));
                    in_part(&mut part, *segment);
                    instrs.push(Instr::Optional(part));
                    // The current object differs per row (matched or not): every later read
                    // goes through a label.
                    Site::default()
                }
                Stage::Materialize(m) => self.materialize(m, site, &mut instrs)?,
                Stage::For(f) => self.for_stage(f, site, &mut instrs)?,
            };
        }
        let bound = self.bound;
        self.pipeline(
            &self.plan.result,
            &bound.columns,
            &bound.order_by,
            site,
            &mut instrs,
        )?;
        let labels = self.labels();
        let row_labels = self.row_labels(&labels);
        let columns: Vec<Option<ColumnRef>> = bound.bindings.iter().map(|b| b.column).collect();
        let (mut labelled, rows_labelled) = self.labelled(&instrs, &columns);
        let items = self
            .bound
            .segments
            .iter()
            .map(|segment| matches!(segment, Segment::For(f) if f.ordinal.is_none()))
            .collect();
        let (groups, part_labels, anchors) = self.groups(&labels, &mut labelled);
        let inner_labels = self.inner_labels(&labels);
        let paths = self.paths(&labels, &inner_labels, &groups, &anchors);
        Ok(Lowered {
            query: self.shared,
            instrs,
            labels,
            labelled,
            columns,
            row_labels,
            rows_labelled,
            items,
            groups,
            part_labels,
            inner_labels,
            paths,
        })
    }

    /// Lowers the match pipeline `ops` of one segment, starting at `site`; returns the site
    /// after it.
    fn ops<S: GraphStorage>(
        &self,
        ops: &[Op],
        mut site: Site,
        instrs: &mut Vec<Instr<S>>,
    ) -> Result<Site, TraverserError> {
        for (at, op) in ops.iter().enumerate() {
            match op {
                Op::Scan(scan) => {
                    instrs.push(Instr::Scan);
                    let (labels, steps) = self.element(scan.binding, &scan.filters, Vec::new())?;
                    if let Some(labels) = labels {
                        instrs.push(Instr::HasLabel(labels));
                    }
                    instrs.extend(steps);
                    instrs.push(Instr::Label(scan.binding));
                    site = Site::on(scan.binding);
                }
                Op::Filter(ids) => {
                    for &id in ids {
                        self.conjunct(id, site, instrs)?;
                    }
                }
                Op::Expand(expand) => {
                    if !expand.from_current {
                        self.move_to(expand.from, site, instrs)?;
                    }
                    let zero_only = expand.quantifier.is_some_and(|q| q.max == Some(0));
                    // `{0}` matches only the zero-length path: no step, the far node is the
                    // node the walk starts from (its filters and label run on it).
                    if !zero_only {
                        instrs.push(Instr::Hop(self.hop(expand)?));
                    }
                    // After a quantified edge every traverser carries its own edges' labels,
                    // so a merge point there would never merge.
                    if let Some(next) = next_hop(ops, at)
                        && expand.quantifier.is_none()
                    {
                        instrs.push(Instr::Merge {
                            direction: next.direction,
                            labels: self.edge_labels(next),
                        });
                    }
                    if !expand.to_bound {
                        let (labels, steps) =
                            self.element(expand.to, &expand.to_filters, Vec::new())?;
                        if let Some(labels) = labels {
                            instrs.push(Instr::HasLabel(labels));
                        }
                        instrs.extend(steps);
                        instrs.push(Instr::Label(expand.to));
                    }
                    site = Site::on(expand.to);
                }
                Op::Join { binding } => {
                    instrs.push(self.same(*binding)?);
                    if self.is_column(*binding) {
                        // A column binding has no label of its own; a path variable reads it
                        // through one set here (emitted only then).
                        instrs.push(Instr::Label(*binding));
                    }
                    site = Site::on(*binding);
                }
                Op::Resume(resume) => {
                    if !resume.from_current && site.current != Some(resume.binding) {
                        self.move_to(resume.binding, site, instrs)?;
                    }
                    site = Site::on(resume.binding);
                }
                Op::Anchor(scan) => return Err(self.anchor_not_yet(scan.binding)),
            }
        }
        Ok(site)
    }

    /// Moves from `site` to node `id` bound earlier: `select(label)` for a pattern binding,
    /// `gql:bind` for a materializing `WITH` column binding (read from its row).
    fn move_to<S: GraphStorage>(
        &self,
        id: BindingId,
        site: Site,
        instrs: &mut Vec<Instr<S>>,
    ) -> Result<(), TraverserError> {
        let Some(binding) = self.bound.binding(id) else {
            return Err(self.unlowered());
        };
        if binding.column.is_none() {
            instrs.push(Instr::Select(id));
            return Ok(());
        }
        let kind = match binding.kind {
            BindingKind::Node => ElementKind::Vertex,
            BindingKind::Edge => ElementKind::Edge,
            BindingKind::Path | BindingKind::Value => return Err(self.unlowered()),
        };
        let name = binding
            .name
            .as_ref()
            .map(|n| n.name.as_str())
            .or(binding.key.as_deref())
            .unwrap_or_default();
        instrs.push(Instr::Bind {
            id,
            site,
            name: CompactString::from(name),
            kind,
        });
        // A path variable reads the element through a label set here (emitted only then).
        instrs.push(Instr::Label(id));
        Ok(())
    }

    /// The check that the current element is the one bound earlier to `id` (a join).
    fn same<S: GraphStorage>(&self, id: BindingId) -> Result<Instr<S>, TraverserError> {
        if self.is_column(id) {
            self.row_check(id, BinaryOp::Eq)
        } else {
            Ok(Instr::Same(id))
        }
    }

    /// The check that the current edge differs from the edge bound earlier to `id` (trail).
    fn differs<S: GraphStorage>(&self, id: BindingId) -> Result<Instr<S>, TraverserError> {
        if self.is_column(id) {
            self.row_check(id, BinaryOp::Ne)
        } else {
            Ok(Instr::Differs(id))
        }
    }

    /// Whether `id` is a materializing `WITH` column binding (no path label of its own).
    fn is_column(&self, id: BindingId) -> bool {
        self.bound.binding(id).is_some_and(|b| b.column.is_some())
    }

    /// `current <op> var` over the extra slot of [`Instr::RowCheck`], for column binding `id`.
    fn row_check<S: GraphStorage>(
        &self,
        id: BindingId,
        op: BinaryOp,
    ) -> Result<Instr<S>, TraverserError> {
        let Some(binding) = self.bound.binding(id) else {
            return Err(self.unlowered());
        };
        let (Some(key), Some(&span)) = (binding.key.clone(), binding.occurrences.last()) else {
            return Err(self.unlowered());
        };
        let variable = |name: String| Expr {
            kind: ExprKind::Variable(Identifier { name, span }),
            span,
        };
        let expr = Expr {
            kind: ExprKind::Binary {
                op,
                left: Box::new(variable(String::new())),
                right: Box::new(variable(key)),
            },
            span,
        };
        let extra = self.bound.bindings.len();
        Ok(Instr::RowCheck(self.compile(&expr, Some(extra), &[])?))
    }

    /// Lowers a materializing `WITH` (decision C2) at `site`: its result pipeline, its `WHERE`
    /// on the new row, the row label (emitted only when read later). Returns the row's site.
    fn materialize<S: GraphStorage>(
        &self,
        m: &Materialize,
        site: Site,
        instrs: &mut Vec<Instr<S>>,
    ) -> Result<Site, TraverserError> {
        let Some(Segment::With(with)) = self.bound.segments.get(m.segment) else {
            return Err(self.unlowered());
        };
        self.pipeline(&m.result, &with.columns, &with.order_by, site, instrs)?;
        let row = Site::row(m.segment);
        for &c in &m.filters {
            self.conjunct(c, row, instrs)?;
        }
        instrs.push(Instr::RowLabel(m.segment));
        Ok(row)
    }

    /// Lowers a `FOR` / `UNWIND` at `site`: `gql:for` over its list (read at `site`), then the
    /// row label (emitted only when read later). Returns the row's site: the item (or the
    /// record `{x, i}`) is the current object.
    fn for_stage<S: GraphStorage>(
        &self,
        f: &ForStage,
        site: Site,
        instrs: &mut Vec<Instr<S>>,
    ) -> Result<Site, TraverserError> {
        let Some(Segment::For(bound_for)) = self.bound.segments.get(f.segment) else {
            return Err(self.unlowered());
        };
        let name = |id: BindingId| {
            self.bound
                .binding(id)
                .and_then(|b| b.name.as_ref())
                .map(|n| CompactString::from(n.name.as_str()))
                .unwrap_or_default()
        };
        let list = self.compile(&bound_for.list, None, &[])?;
        let ordinal = bound_for.ordinal.map(|(kind, id)| GqlOrdinal {
            name: name(id),
            kind,
        });
        instrs.push(Instr::For {
            list,
            site,
            variable: name(bound_for.variable),
            ordinal,
        });
        instrs.push(Instr::RowLabel(f.segment));
        Ok(Site::row(f.segment))
    }

    /// The error of a plan part the lowering does not emit (the binder refuses those queries
    /// with their own messages first, so this is a guard only).
    fn unlowered(&self) -> TraverserError {
        GqlErrorBuilder::new(
            GqlErrorKind::NotYet,
            0..self.query.len(),
            "this query form is not yet supported in this version",
        )
        .alternative(
            "write the query as MATCH patterns with WHERE and RETURN, or chain Gremlin steps \
             after gql()",
        )
        .into_error(self.query)
    }

    /// The error of the phase 2 anchor (a mid-traversal `gql()`), which stage 1a never plans.
    fn anchor_not_yet(&self, id: BindingId) -> TraverserError {
        let span = self
            .bound
            .binding(id)
            .and_then(|b| b.occurrences.first().copied())
            .map_or(0..0, |s| s.start..s.end);
        GqlErrorBuilder::new(
            GqlErrorKind::NotYet,
            span,
            "gql() cannot continue a traversal yet",
        )
        .alternative(start_with_gql(self.query))
        .into_error(self.query)
    }

    /// The label factors and conjuncts of element `id` where it is bound: the first factor that
    /// is a label disjunction (the scan's `has_label`, folded into `v(labels: ..)` by the
    /// optimizer, or the labels of an edge's adjacency step), then the steps on the element: the
    /// other label disjunctions as `has_label`, the pushed conjuncts as `has(k, P..)`, `checks`
    /// (join and trail checks), the other label factors and the residual conjuncts as
    /// `gql:filter`.
    fn element<S: GraphStorage>(
        &self,
        id: BindingId,
        filters: &ElementFilters,
        checks: Vec<Instr<S>>,
    ) -> Result<ElementSteps<S>, TraverserError> {
        let mut instrs = Vec::new();
        let labels = self
            .bound
            .binding(id)
            .map(|b| b.labels.as_slice())
            .unwrap_or_default();
        let factors = and_factors(labels);
        let first_factor = factors.iter().position(|f| label_disjuncts(f).is_some());
        let first = first_factor
            .and_then(|at| factors.get(at))
            .and_then(|f| label_disjuncts(f));
        let mut label_filters = Vec::new();
        for (at, factor) in factors.iter().enumerate() {
            if Some(at) == first_factor {
                continue;
            }
            match label_disjuncts(factor) {
                Some(labels) => instrs.push(Instr::HasLabel(labels)),
                None => label_filters.push(*factor),
            }
        }
        let mut residual = filters.residual.clone();
        for &c in &filters.pushed {
            match self.pushed(c)? {
                Some((key, predicate)) => instrs.push(Instr::Has(key, predicate)),
                None => residual.push(c),
            }
        }
        residual.sort_unstable();
        instrs.extend(checks);
        for factor in label_filters {
            let condition = self.compile(&is_labeled(factor), Some(id.0), &[])?;
            instrs.push(Instr::Filter {
                condition,
                site: Site::on(id),
            });
        }
        for c in residual {
            self.conjunct(c, Site::on(id), &mut instrs)?;
        }
        Ok((first, instrs))
    }

    /// The hop of an expansion: the edge's labels, filters, join and trail checks (Phase 0 Q3,
    /// Q8) and its `as()` placeholder (none for an edge bound earlier, which is checked against
    /// its label instead).
    fn hop<S: GraphStorage>(&self, expand: &Expand) -> Result<Hop<S>, TraverserError> {
        let id = expand.edge;
        let mut checks = Vec::new();
        if expand.edge_join {
            if expand.quantifier.is_some() {
                // The binder refuses a group variable bound again; a guard only.
                return Err(self.unlowered());
            }
            checks.push(self.same(id)?);
        }
        // Trail (plan mechanism 5): a quantified edge checks its own earlier iterations and
        // every earlier edge of the clause with `gql:trail` (before its own `as()`, so it
        // compares with the earlier occurrences only); a single edge checks an earlier group
        // variable with `gql:trail` and an earlier single edge with `where(P.neq(..))`.
        let mut every = Vec::new();
        if expand.quantifier.is_some() {
            every.push(id);
        }
        for &other in &expand.trail {
            if expand.quantifier.is_some() || self.is_group(other) {
                every.push(other);
            } else {
                checks.push(self.differs(other)?);
            }
        }
        if !every.is_empty() {
            checks.push(Instr::Trail(every));
        }
        let (labels, mut steps) = if expand.edge_join {
            (None, checks)
        } else {
            self.element(id, &expand.edge_filters, checks)?
        };
        // An edge joined to a column binding has no label of its own: a path variable reads it
        // through one set here (emitted only then).
        if !expand.edge_join || self.is_column(id) {
            steps.push(Instr::Label(id));
        }
        Ok(Hop {
            direction: expand.direction,
            labels,
            steps,
            quantifier: expand.quantifier,
            inner: expand.quantifier.map(|_| id),
        })
    }

    /// Whether `id` is a group variable (the edge of a quantified edge).
    fn is_group(&self, id: BindingId) -> bool {
        self.bound.binding(id).is_some_and(|b| b.group)
    }

    /// The adjacency labels of an expansion's edge (the first label disjunction factor), as
    /// [`Self::hop`] emits them.
    fn edge_labels(&self, expand: &Expand) -> Option<Vec<String>> {
        if expand.edge_join {
            return None;
        }
        let labels = self.bound.binding(expand.edge)?.labels.as_slice();
        and_factors(labels).into_iter().find_map(label_disjuncts)
    }

    /// `gql:filter` of conjunct `c` at `site` (skipped when it is constantly TRUE).
    fn conjunct<S: GraphStorage>(
        &self,
        c: usize,
        site: Site,
        instrs: &mut Vec<Instr<S>>,
    ) -> Result<(), TraverserError> {
        let Some(conjunct) = self.bound.conjuncts.get(c) else {
            return Ok(());
        };
        let (expr, this) = self.condition(conjunct);
        let condition: RowExpr<S> = self.compile(&expr, this, &[])?;
        if matches!(
            condition.as_const(),
            Some(Value::Scalar(ElementProperty::Boolean(true)))
        ) {
            return Ok(());
        }
        instrs.push(Instr::Filter { condition, site });
        Ok(())
    }

    /// The condition of a conjunct as an expression; an inline property `{k: v}` becomes
    /// `var.k = v` (over the reserved name when the element is anonymous).
    fn condition(&self, conjunct: &Conjunct) -> (Expr, Option<usize>) {
        match &conjunct.predicate {
            Predicate::Expr(expr) => (expr.clone(), None),
            Predicate::Property {
                binding,
                key,
                value,
            } => {
                let name = self
                    .names
                    .get(binding.0)
                    .cloned()
                    .flatten()
                    .unwrap_or_default();
                let this = name.is_empty().then_some(binding.0);
                let span = conjunct.span;
                let target = Expr {
                    kind: ExprKind::Variable(Identifier { name, span }),
                    span,
                };
                let property = Expr {
                    kind: ExprKind::Property {
                        target: Box::new(target),
                        key: key.clone(),
                    },
                    span,
                };
                let expr = Expr {
                    kind: ExprKind::Binary {
                        op: BinaryOp::Eq,
                        left: Box::new(property),
                        right: Box::new(value.clone()),
                    },
                    span,
                };
                (expr, this)
            }
        }
    }

    /// The `has(key, P)` of pushed conjunct `c`; `None` when a parameter's value does not fit
    /// the form (the conjunct then stays in `gql:filter`).
    fn pushed(&self, c: usize) -> Result<Option<(String, P<PValue>)>, TraverserError> {
        let Some(Pushdown { key, op, operand }) =
            self.plan.pushdown.get(c).and_then(Option::as_ref)
        else {
            return Ok(None);
        };
        let predicate = match (op, operand) {
            (PushOp::NotNull, _) => Some(P::Neq(PValue::Null)),
            (PushOp::In, PushOperand::List(items)) => items
                .iter()
                .map(literal_pvalue)
                .collect::<Option<Vec<_>>>()
                .map(P::WithIn),
            (PushOp::In, PushOperand::Parameter { name, .. }) => parameter(self.params, name)
                .and_then(list_pvalues)
                .map(P::WithIn),
            (op, PushOperand::Value(literal)) => {
                literal_pvalue(literal).and_then(|v| single(*op, v))
            }
            (op, PushOperand::Parameter { name, .. }) => parameter(self.params, name)
                .and_then(scalar_pvalue)
                .and_then(|v| single(*op, v)),
            (_, PushOperand::None | PushOperand::List(_)) => None,
        };
        Ok(predicate.map(|p| (key.clone(), p)))
    }

    /// A result pipeline (`RETURN`, or a materializing `WITH`) of `columns` at `site`.
    fn pipeline<S: GraphStorage>(
        &self,
        ops: &[ResultOp],
        columns: &[Column],
        order_by: &[SortKey],
        site: Site,
        instrs: &mut Vec<Instr<S>>,
    ) -> Result<(), TraverserError> {
        let n = self.bound.bindings.len();
        for op in ops {
            match op {
                ResultOp::Project => {
                    let mut compiled = Vec::with_capacity(columns.len());
                    for column in columns {
                        compiled.push(GqlColumn {
                            name: CompactString::from(column.name.as_str()),
                            expr: self.compile(&column.expr, None, &[])?,
                        });
                    }
                    instrs.push(Instr::Project {
                        columns: compiled,
                        site,
                    });
                }
                ResultOp::Aggregate => {
                    let Aggregating { outputs, calls } = compile_aggregating(
                        columns.iter().map(|c| &c.expr),
                        &self.scope(None, &[]),
                    )
                    .map_err(|error| eval_error(self.query, error))?;
                    let columns = columns
                        .iter()
                        .zip(outputs)
                        .map(|(column, expr)| GqlAggregateColumn {
                            name: CompactString::from(column.name.as_str()),
                            expr,
                            key: !column.aggregate,
                        })
                        .collect();
                    instrs.push(Instr::Aggregate {
                        columns,
                        calls,
                        site,
                    });
                }
                ResultOp::Distinct => instrs.push(Instr::Distinct),
                ResultOp::Order => {
                    let mut keys = Vec::with_capacity(order_by.len());
                    for key in order_by {
                        let expr = match key.column {
                            Some(i) => {
                                let reference = Expr {
                                    kind: ExprKind::Variable(Identifier {
                                        name: String::new(),
                                        span: key.expr.span,
                                    }),
                                    span: key.expr.span,
                                };
                                self.compile(&reference, Some(n + i), &[])?
                            }
                            None => self.sort_key(key, columns, n)?,
                        };
                        keys.push(GqlSortKey {
                            expr,
                            descending: key.direction == SortDirection::Desc,
                        });
                    }
                    instrs.push(Instr::Order {
                        keys,
                        columns: columns.len(),
                    });
                }
                ResultOp::Offset(count) => instrs.push(Instr::Skip(self.count(count, "OFFSET")?)),
                ResultOp::Limit(count) => instrs.push(Instr::Limit(self.count(count, "LIMIT")?)),
            }
        }
        Ok(())
    }

    /// An `ORDER BY` key that is not a whole column. An identifier the key names as a column
    /// alias reads the column (slot `n + column`); every other variable is a binding, also one
    /// whose key equals an alias (a substituted `LET` / `WITH` name standing for a variable,
    /// T2 note): the binder's alias references are the identifiers of
    /// [`SortKey::aliases`] written inside the key, which are renamed to names no binding has.
    fn sort_key<S: GraphStorage>(
        &self,
        key: &SortKey,
        columns: &[Column],
        n: usize,
    ) -> Result<RowExpr<S>, TraverserError> {
        if key.aliases.is_empty() {
            return self.compile(&key.expr, None, &[]);
        }
        let mut renames: Vec<(&str, String)> = Vec::with_capacity(key.aliases.len());
        let mut aliases: Vec<(String, usize)> = Vec::with_capacity(key.aliases.len());
        for &i in &key.aliases {
            let Some(column) = columns.get(i) else {
                continue;
            };
            let mut reserved = format!("\u{0}{i}");
            while self
                .names
                .iter()
                .any(|n| n.as_deref() == Some(reserved.as_str()))
            {
                reserved.insert(0, '\u{0}');
            }
            renames.push((column.name.as_str(), reserved.clone()));
            aliases.push((reserved, n + i));
        }
        let mut expr = key.expr.clone();
        rename_aliases(&mut expr, key.span, &renames);
        self.compile(&expr, None, &aliases)
    }

    /// The value of `OFFSET` / `LIMIT`: a literal, or a parameter whose value must be a
    /// non-negative integer (GQLSTATUS `22G02` negative, `22G03` not an integer).
    fn count(&self, count: &Count, keyword: &str) -> Result<usize, TraverserError> {
        let (name, span) = match count {
            Count::Literal(n) => return Ok(usize::try_from(*n).unwrap_or(usize::MAX)),
            Count::Parameter { name, span } => (name, span),
        };
        let refused = |code: &'static str, what: &str| {
            GqlErrorBuilder::new(
                GqlErrorKind::Semantic,
                span.start..span.end,
                format!("{keyword} must be a non-negative integer, but ${name} is {what}"),
            )
            .gqlstatus(code)
            .alternative(format!(
                "give ${name} a value such as 10, as in g.gql({}, #{{{}: 10}})",
                crate::value::literal::rhai_string(self.query),
                rhai_map_key(name)
            ))
            .into_error(self.query)
        };
        match parameter(self.params, name).map(|v| v.clone().into_logical()) {
            Some(ElementProperty::Number(number)) => match number.as_i64() {
                Some(value) => match u64::try_from(value) {
                    Ok(value) => Ok(usize::try_from(value).unwrap_or(usize::MAX)),
                    Err(_) => Err(refused("22G02", "negative")),
                },
                None => Err(refused("22G03", "a float")),
            },
            Some(ElementProperty::Null) | None => Err(refused("22G03", "null")),
            Some(_) => Err(refused("22G03", "not a number")),
        }
    }

    /// Which bindings need `as()` (module docs): a reason of the plan other than `read`, or a
    /// read away from the current element; and which materializing `WITH` rows need their row
    /// label: the plan says so, or a step reads a column after the row was left.
    fn labelled<S: GraphStorage>(
        &self,
        instrs: &[Instr<S>],
        columns: &[Option<ColumnRef>],
    ) -> (Vec<bool>, Vec<bool>) {
        let n = self.bound.bindings.len();
        let mut labelled: Vec<bool> = self
            .plan
            .labelling
            .iter()
            .map(|l| l.join || l.trail || l.branch || l.resume || l.optional || l.group || l.path)
            .collect();
        labelled.resize(n, false);
        let mut rows = vec![false; self.bound.segments.len()];
        for stage in &self.plan.stages {
            let (segment, labelled) = match stage {
                Stage::Materialize(m) => (m.segment, m.labelled),
                Stage::For(f) => (f.segment, f.labelled),
                Stage::Match { .. } | Stage::Optional { .. } => continue,
            };
            if labelled && let Some(flag) = rows.get_mut(segment) {
                *flag = true;
            }
        }
        mark_reads(
            instrs,
            &mut Marks {
                columns,
                labelled: &mut labelled,
                rows: &mut rows,
            },
        );
        (labelled, rows)
    }

    /// How each group variable is read ([`GroupRead`]), the part label of each
    /// `OPTIONAL MATCH` segment that needs one, and the anchor of each segment (`None`: none
    /// needed). A group or path variable of an `OPTIONAL MATCH` part that is read anywhere is
    /// anchored: on the part's witness node (labelled for it), or, when the part binds no new
    /// node (`OPTIONAL MATCH (a)-[e]->{0,2}(a)`), on the part label `#o<k>` set at the end of
    /// the part. Without the anchor a part not matched (`NULL`) and a zero-length match (`[]`)
    /// would read alike, and a path over elements bound before the part would read as matched.
    fn groups(
        &self,
        labels: &[CompactString],
        labelled: &mut [bool],
    ) -> (
        Vec<Option<GroupRead>>,
        Vec<CompactString>,
        Vec<Option<CompactString>>,
    ) {
        let mut part_labels = vec![CompactString::default(); self.bound.segments.len()];
        let mut anchors: Vec<Option<CompactString>> = vec![None; self.bound.segments.len()];
        for stage in &self.plan.stages {
            let Stage::Optional {
                segment, witness, ..
            } = stage
            else {
                continue;
            };
            let read = self.bound.bindings.iter().any(|b| {
                (b.group || b.kind == BindingKind::Path)
                    && b.segment == *segment
                    && self.plan.labelling.get(b.id.0).is_some_and(|l| l.read)
            });
            if !read {
                continue;
            }
            let anchor = match witness {
                Some(w) => {
                    if let Some(flag) = labelled.get_mut(w.0) {
                        *flag = true;
                    }
                    labels.get(w.0).cloned()
                }
                None => {
                    let mut label = format!("#o{segment}");
                    while labels.iter().any(|l| l.as_str() == label) {
                        label.insert(0, '#');
                    }
                    let label = CompactString::from(label);
                    if let Some(slot) = part_labels.get_mut(*segment) {
                        *slot = label.clone();
                    }
                    Some(label)
                }
            };
            if let Some(slot) = anchors.get_mut(*segment) {
                *slot = anchor;
            }
        }
        let mut reversed = vec![false; self.bound.bindings.len()];
        let stage_ops = self.plan.stages.iter().filter_map(|stage| match stage {
            Stage::Match { ops, .. } | Stage::Optional { ops, .. } => Some(ops.as_slice()),
            Stage::Materialize(_) | Stage::For(_) => None,
        });
        for op in std::iter::once(self.plan.ops.as_slice())
            .chain(stage_ops)
            .flatten()
        {
            if let Op::Expand(e) = op
                && e.quantifier.is_some()
                && let Some(flag) = reversed.get_mut(e.edge.0)
            {
                *flag = e.reversed;
            }
        }
        let groups = self
            .bound
            .bindings
            .iter()
            .map(|b| {
                b.group.then(|| GroupRead {
                    segment: b.segment,
                    anchor: anchors.get(b.segment).cloned().flatten(),
                    reversed: reversed.get(b.id.0).copied().unwrap_or(false),
                })
            })
            .collect();
        (groups, part_labels, anchors)
    }

    /// The inner label of each group variable a read path variable passes through
    /// (`#gv<id>`, more `#` in front while it collides with a binding's label); empty for the
    /// others. The planner marks every element of a read path's pattern ([`Labelling::path`]).
    ///
    /// [`Labelling::path`]: super::plan::Labelling
    fn inner_labels(&self, labels: &[CompactString]) -> Vec<CompactString> {
        self.bound
            .bindings
            .iter()
            .map(|b| {
                let path = self.plan.labelling.get(b.id.0).is_some_and(|l| l.path);
                if !(b.group && path) {
                    return CompactString::default();
                }
                let mut label = format!("#gv{}", b.id.0);
                while labels.iter().any(|l| l.as_str() == label) {
                    label.insert(0, '#');
                }
                CompactString::from(label)
            })
            .collect()
    }

    /// How each path variable is assembled ([`PathRead`]): its pattern's nodes and edges in
    /// text order, a quantified edge as a group part (reversed when walked from its far end),
    /// anchored like a group variable when it is bound in an `OPTIONAL MATCH` part.
    fn paths(
        &self,
        labels: &[CompactString],
        inner_labels: &[CompactString],
        groups: &[Option<GroupRead>],
        anchors: &[Option<CompactString>],
    ) -> Vec<Option<PathRead>> {
        let mut paths: Vec<Option<PathRead>> = vec![None; self.bound.bindings.len()];
        let label = |id: BindingId| labels.get(id.0).cloned().unwrap_or_default();
        for path in self.bound.matches.iter().flat_map(|m| &m.paths) {
            let Some(variable) = path.variable else {
                continue;
            };
            let Some(binding) = self.bound.binding(variable) else {
                continue;
            };
            let mut parts = Vec::with_capacity(2 * path.steps.len() + 1);
            parts.push(PathPart::Element(label(path.start)));
            for step in &path.steps {
                if step.quantifier.is_some() {
                    parts.push(PathPart::Group {
                        edges: label(step.edge),
                        vertices: inner_labels.get(step.edge.0).cloned().unwrap_or_default(),
                        reversed: groups
                            .get(step.edge.0)
                            .and_then(Option::as_ref)
                            .is_some_and(|g| g.reversed),
                    });
                } else {
                    parts.push(PathPart::Element(label(step.edge)));
                }
                parts.push(PathPart::Element(label(step.node)));
            }
            if let Some(slot) = paths.get_mut(variable.0) {
                *slot = Some(PathRead {
                    segment: binding.segment,
                    parts,
                    anchor: anchors.get(binding.segment).cloned().flatten(),
                });
            }
        }
        paths
    }

    /// The path label of each binding: its key (the variable name, or `#<id>` for a name bound
    /// again after a `WITH` dropped it, decision C14); an anonymous binding `#<id>`
    /// (no plain identifier starts with `#`), with more `#` in front while it collides with a
    /// back-quoted variable name.
    fn labels(&self) -> Vec<CompactString> {
        self.names
            .iter()
            .enumerate()
            .map(|(id, name)| match name {
                Some(name) => CompactString::from(name.as_str()),
                None => {
                    let mut label = format!("#{id}");
                    while self
                        .names
                        .iter()
                        .any(|n| n.as_deref() == Some(label.as_str()))
                    {
                        label.insert(0, '#');
                    }
                    CompactString::from(label)
                }
            })
            .collect()
    }

    /// The row label of each materializing `WITH` segment, `#w<k>`, and of each `FOR` with an
    /// ordinal, `#f<k>` (more `#` in front while it collides with a binding's label); of a `FOR`
    /// without an ordinal its item's own label (the row is the item); empty for the other
    /// segments.
    fn row_labels(&self, labels: &[CompactString]) -> Vec<CompactString> {
        self.bound
            .segments
            .iter()
            .enumerate()
            .map(|(k, segment)| match segment {
                Segment::With(_) => {
                    let mut label = format!("#w{k}");
                    while labels.iter().any(|l| l.as_str() == label) {
                        label.insert(0, '#');
                    }
                    CompactString::from(label)
                }
                Segment::For(f) if f.ordinal.is_none() => {
                    labels.get(f.variable.0).cloned().unwrap_or_default()
                }
                Segment::For(_) => {
                    let mut label = format!("#f{k}");
                    while labels.iter().any(|l| l.as_str() == label) {
                        label.insert(0, '#');
                    }
                    CompactString::from(label)
                }
                Segment::Match(_) | Segment::Optional(_) => CompactString::default(),
            })
            .collect()
    }
}

/// `this IS LABELED <factor>` over the reserved name.
fn is_labeled(factor: &LabelExpr) -> Expr {
    let span: Span = factor.span;
    Expr {
        kind: ExprKind::IsLabeled {
            operand: Box::new(Expr {
                kind: ExprKind::Variable(Identifier {
                    name: String::new(),
                    span,
                }),
                span,
            }),
            labels: factor.clone(),
            negated: false,
        },
        span,
    }
}

/// The predicate of a single-operand form.
fn single(op: PushOp, value: PValue) -> Option<P<PValue>> {
    let text = matches!(value, PValue::String(_));
    Some(match op {
        PushOp::Eq => P::Eq(value),
        PushOp::Lt => P::Lt(value),
        PushOp::Le => P::Lte(value),
        PushOp::Gt => P::Gt(value),
        PushOp::Ge => P::Gte(value),
        PushOp::StartsWith if text => P::StartingWith(value),
        PushOp::EndsWith if text => P::EndingWith(value),
        PushOp::Contains if text => P::Containing(value),
        PushOp::StartsWith | PushOp::EndsWith | PushOp::Contains | PushOp::In | PushOp::NotNull => {
            return None;
        }
    })
}

/// A pushable literal: a string, a number (never `NaN`) or a boolean.
fn literal_pvalue(literal: &Literal) -> Option<PValue> {
    match literal {
        Literal::Null => None,
        Literal::Bool(flag) => Some(PValue::Boolean(*flag)),
        Literal::Integer(value) => Some(PValue::Number(Number::from_i64(*value))),
        Literal::Float(value) if value.is_nan() => None,
        Literal::Float(value) => Some(PValue::Number(Number::from_f64(*value))),
        Literal::String(text) => Some(PValue::String(CompactString::from(text.as_str()))),
    }
}

/// A pushable parameter value: a string, a number (never `NaN`) or a boolean (Phase 0 Q5
/// verified exactly these kinds; others stay in `gql:filter`).
fn scalar_pvalue(value: &ElementProperty) -> Option<PValue> {
    match value.clone().into_logical() {
        ElementProperty::String(text) => Some(PValue::String(text)),
        ElementProperty::Number(number) if number.as_f64().is_some_and(f64::is_nan) => None,
        ElementProperty::Number(number) => Some(PValue::Number(number)),
        ElementProperty::Boolean(flag) => Some(PValue::Boolean(flag)),
        _ => None,
    }
}

/// The elements of a list parameter for `IN`, `NULL`s dropped; `None` when it is not a list
/// or holds a value that is not pushable.
fn list_pvalues(value: &ElementProperty) -> Option<Vec<PValue>> {
    let ElementProperty::Array(items) = value.clone().into_logical() else {
        return None;
    };
    items
        .iter()
        .filter(|item| !matches!(item, ElementProperty::Null))
        .map(scalar_pvalue)
        .collect()
}

/// What [`mark_reads`] records.
struct Marks<'a> {
    /// The materializing `WITH` column each binding stands for.
    columns: &'a [Option<ColumnRef>],
    /// Per binding: read away from its element (needs `as()`).
    labelled: &'a mut [bool],
    /// Per segment: its row is read after it was left (needs `as("#w<k>")`).
    rows: &'a mut [bool],
}

impl Marks<'_> {
    /// Records the reads of `slots` at `site` and clears them. Slots past the bindings (the
    /// columns of `ORDER BY`, the current element of a row check) read no label.
    fn read(&mut self, slots: &mut Vec<usize>, site: Site) {
        for slot in slots.drain(..) {
            if site.current == Some(BindingId(slot)) {
                continue;
            }
            match self.columns.get(slot) {
                Some(Some(column)) => {
                    if site.row != Some(column.segment)
                        && let Some(flag) = self.rows.get_mut(column.segment)
                    {
                        *flag = true;
                    }
                }
                Some(None) => {
                    if let Some(flag) = self.labelled.get_mut(slot) {
                        *flag = true;
                    }
                }
                None => {}
            }
        }
    }
}

/// Marks every binding an expression of `instrs` reads while it is not the current element, and
/// every materializing `WITH` row a step reads while it is not the current object.
fn mark_reads<S: GraphStorage>(instrs: &[Instr<S>], marks: &mut Marks<'_>) {
    let mut slots = Vec::new();
    for instr in instrs {
        match instr {
            Instr::Filter { condition, site } => {
                condition.variables(&mut slots);
                marks.read(&mut slots, *site);
            }
            Instr::RowCheck(condition) => {
                condition.variables(&mut slots);
                marks.read(&mut slots, Site::default());
            }
            Instr::Project { columns, site } => {
                for column in columns {
                    column.expr.variables(&mut slots);
                }
                marks.read(&mut slots, *site);
            }
            Instr::For { list, site, .. } => {
                list.variables(&mut slots);
                marks.read(&mut slots, *site);
            }
            Instr::Aggregate {
                columns,
                calls,
                site,
            } => {
                for column in columns {
                    column.expr.variables(&mut slots);
                }
                for argument in calls.iter().filter_map(|call| call.argument.as_ref()) {
                    argument.variables(&mut slots);
                }
                marks.read(&mut slots, *site);
            }
            Instr::Order { keys, .. } => {
                for key in keys {
                    key.expr.variables(&mut slots);
                }
                marks.read(&mut slots, Site::default());
            }
            Instr::Bind { id, site, .. } => {
                slots.push(id.0);
                marks.read(&mut slots, *site);
            }
            Instr::Hop(hop) => mark_reads(&hop.steps, marks),
            Instr::Optional(part) => mark_reads(part, marks),
            Instr::Scan
            | Instr::Unit
            | Instr::HasLabel(_)
            | Instr::Has(..)
            | Instr::Label(_)
            | Instr::RowLabel(_)
            | Instr::Distinct
            | Instr::Skip(_)
            | Instr::Limit(_)
            | Instr::Select(_)
            | Instr::Same(_)
            | Instr::Differs(_)
            | Instr::Trail(_)
            | Instr::PartLabel(_)
            | Instr::Merge { .. } => {}
        }
    }
}

/// Marks every site of `instrs` (the part of `OPTIONAL MATCH` segment `segment`) as inside
/// that part ([`Site::part`]).
fn in_part<S: GraphStorage>(instrs: &mut [Instr<S>], segment: usize) {
    for instr in instrs {
        match instr {
            Instr::Filter { site, .. }
            | Instr::Project { site, .. }
            | Instr::For { site, .. }
            | Instr::Aggregate { site, .. }
            | Instr::Bind { site, .. } => site.part = Some(segment),
            Instr::Hop(hop) => in_part(&mut hop.steps, segment),
            Instr::Optional(part) => in_part(part, segment),
            Instr::Scan
            | Instr::Unit
            | Instr::HasLabel(_)
            | Instr::Has(..)
            | Instr::RowCheck(_)
            | Instr::Label(_)
            | Instr::RowLabel(_)
            | Instr::Distinct
            | Instr::Order { .. }
            | Instr::Skip(_)
            | Instr::Limit(_)
            | Instr::Select(_)
            | Instr::Same(_)
            | Instr::Differs(_)
            | Instr::Trail(_)
            | Instr::PartLabel(_)
            | Instr::Merge { .. } => {}
        }
    }
}

/// The next expansion after `ops[at]` when it continues from the vertex `ops[at]` reached
/// (only join checks and filters between them): the place of a merge point (Phase 0 outcome 1).
fn next_hop(ops: &[Op], at: usize) -> Option<&Expand> {
    for op in ops.get(at + 1..)? {
        match op {
            Op::Join { .. } | Op::Filter(_) => {}
            Op::Expand(next) if next.from_current => return Some(next),
            Op::Expand(_) | Op::Scan(_) | Op::Anchor(_) | Op::Resume(_) => return None,
        }
    }
    None
}

/// Renames the alias references of an `ORDER BY` key: every variable written inside `within`
/// (the key's text) whose name is a renamed alias. A variable outside it comes from a
/// substituted `LET` / `WITH` definition and stays a binding. Recursion is bounded by the
/// expression height (`MAX_HEIGHT`, kept by substitution).
fn rename_aliases(expr: &mut Expr, within: Span, renames: &[(&str, String)]) {
    match &mut expr.kind {
        ExprKind::Variable(var) => {
            let inside = var.span.start >= within.start && var.span.end <= within.end;
            if inside && let Some((_, reserved)) = renames.iter().find(|(n, _)| *n == var.name) {
                var.name = reserved.clone();
            }
        }
        ExprKind::Literal(_) | ExprKind::Parameter(_) => {}
        ExprKind::List(items) => {
            for item in items {
                rename_aliases(item, within, renames);
            }
        }
        ExprKind::Map(map) => {
            for entry in &mut map.entries {
                rename_aliases(&mut entry.value, within, renames);
            }
        }
        ExprKind::Property { target, .. } => rename_aliases(target, within, renames),
        ExprKind::Unary { operand, .. }
        | ExprKind::IsNull { operand, .. }
        | ExprKind::IsLabeled { operand, .. } => rename_aliases(operand, within, renames),
        ExprKind::Binary { left, right, .. }
        | ExprKind::IsEndpoint {
            operand: left,
            edge: right,
            ..
        } => {
            rename_aliases(left, within, renames);
            rename_aliases(right, within, renames);
        }
        ExprKind::Case(case) => {
            if let Some(operand) = &mut case.operand {
                rename_aliases(operand, within, renames);
            }
            for arm in &mut case.whens {
                rename_aliases(&mut arm.when, within, renames);
                rename_aliases(&mut arm.then, within, renames);
            }
            if let Some(else_result) = &mut case.else_result {
                rename_aliases(else_result, within, renames);
            }
        }
        ExprKind::FunctionCall { args, .. } => match args {
            CallArgs::Star(_) => {}
            CallArgs::Exprs(args) => {
                for arg in args {
                    rename_aliases(arg, within, renames);
                }
            }
            CallArgs::Cast { operand, .. } => rename_aliases(operand, within, renames),
            CallArgs::Trim {
                character, source, ..
            } => {
                if let Some(character) = character {
                    rename_aliases(character, within, renames);
                }
                rename_aliases(source, within, renames);
            }
        },
    }
}
