//! The lowering: a planned GQL query ([`LogicalPlan`] of a [`BoundQuery`]) to ordinary
//! traversal steps, plus the Rust entry points [`GraphTraversalSource::gql`] and
//! [`GraphTraversalSource::gql_with`].
//!
//! The steps are built with the ordinary builders (`v()`, `has_label()`, `has()`, `as()`,
//! `dedup()`, `skip()`, `limit()`) and the internal `gql:*` builders, so the plan goes through
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
//! | `DISTINCT` | `dedup()` after `gql:project` |
//! | `ORDER BY` | `gql:order(keys)` after `gql:project` (and `dedup()`): a returned column is read from the row, any other variable through its path label |
//! | `OFFSET n` / `LIMIT m` | `skip(n)` / `limit(m)`; a `$param` value must be a non-negative integer (checked before the run) |
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
//! # Not yet
//!
//! The phase 2 anchor ([`Op::Anchor`], a `gql()` continuing a traversal) fails with
//! [`GqlErrorKind::NotYet`]; stage 1a never plans one.

use std::sync::Arc;

use super::ast::{
    BinaryOp, EdgeDirection, Expr, ExprKind, Identifier, LabelExpr, Literal, SortDirection,
};
use super::bind::{
    BindingId, BoundQuery, Conjunct, Count, Predicate, bind_with_parameters, refuse_unplanned,
};
use super::eval::{Scope, Value, compile};
use super::lexer::Span;
use super::parser::parse;
use super::plan::{
    ElementFilters, Expand, LogicalPlan, Op, PushOp, PushOperand, Pushdown, ResultOp, and_factors,
    label_disjuncts,
};
use super::planner::{StorageFacts, plan};
use super::runtime::{Binding, RowExpr, eval_error};
use crate::error::{GqlErrorBuilder, GqlErrorKind, TraverserError};
use crate::graph::GraphStorage;
use crate::traversal::step::GraphStep;
use crate::traversal::step::flatbarrier_steps::step_barrier::lazy_barrier_step;
use crate::traversal::step::flatbarrier_steps::step_gql_order::GqlSortKey;
use crate::traversal::step::map_steps::step_gql_project::GqlColumn;
use crate::value::{Number, P, PValue};
use crate::{AnonymousTraversal, ArgStrings, CompactString, ElementProperty, GraphTraversalSource};

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Runs a GQL read query (ISO/IEC 39075, feature `gql`): `MATCH` patterns, `WHERE`,
    /// `RETURN` (mandatory; `RETURN *` returns every named variable), `ORDER BY`, `OFFSET`
    /// (Cypher `SKIP`) and `LIMIT`. Use [`Self::gql_with`] for a query with `$parameters`.
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

/// One step of the lowered plan, every fallible part already done.
enum Instr<S: GraphStorage> {
    /// `v()`.
    Scan,
    /// `has_label(labels)`.
    HasLabel(Vec<String>),
    /// `has(key, predicate)`.
    Has(String, P<PValue>),
    /// `gql:filter(condition)`; `current` is the binding the traverser stands on.
    Filter {
        condition: RowExpr<S>,
        current: Option<BindingId>,
    },
    /// `as(label)` of the binding, emitted only when a label is needed.
    Label(BindingId),
    /// `gql:project(columns)`.
    Project {
        columns: Vec<GqlColumn<S>>,
        current: Option<BindingId>,
    },
    /// `dedup()`.
    Dedup,
    /// `gql:order(keys)`, on the row map of `gql:project`.
    Order(Vec<GqlSortKey<S>>),
    /// `skip(n)`.
    Skip(usize),
    /// `limit(n)`.
    Limit(usize),
    /// One pattern edge walked from the current vertex ([`Op::Expand`]); see [`Hop`].
    Hop(Hop<S>),
    /// `select(label)`: back to a binding labelled earlier (a branch of the pattern).
    Select(BindingId),
    /// `where(P.eq(label))`: the current element is the one bound earlier to the binding (a
    /// join: a cycle closing, a node or edge variable repeated).
    Same(BindingId),
    /// `where(P.neq(label))`: the current edge differs from an earlier edge (trail).
    Differs(BindingId),
    /// A merge point (`lazy_barrier()`) right after a hop whose next hop walks `direction`
    /// over `labels` (the step the merge point judges its fan-out with).
    Merge {
        direction: EdgeDirection,
        labels: Option<Vec<String>>,
    },
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
}

/// The lowered query, ready to be emitted onto a source.
struct Lowered<S: GraphStorage> {
    query: Arc<str>,
    instrs: Vec<Instr<S>>,
    /// The path label of each binding.
    labels: Vec<CompactString>,
    /// Whether each binding gets `as()`.
    labelled: Vec<bool>,
    /// The number of `RETURN` columns (the extra slots of `ORDER BY`).
    columns: usize,
}

impl<S: GraphStorage> Lowered<S> {
    /// Where each variable slot reads from at a step whose current element is `current`.
    fn bindings(&self, current: Option<BindingId>) -> Arc<[Binding]> {
        self.labels
            .iter()
            .enumerate()
            .map(|(slot, label)| {
                if current == Some(BindingId(slot)) {
                    Binding::Current
                } else {
                    Binding::Label(label.clone())
                }
            })
            .collect()
    }

    /// The slots of `ORDER BY`: every binding through its label, then the row's columns.
    fn order_bindings(&self) -> Arc<[Binding]> {
        self.labels
            .iter()
            .map(|label| Binding::Label(label.clone()))
            .chain((0..self.columns).map(Binding::Column))
            .collect()
    }

    fn emit<'g>(mut self, source: GraphTraversalSource<'g, S>) -> GraphTraversalSource<'g, S>
    where
        S: 'g,
    {
        let mut g = source;
        let order_bindings = self.order_bindings();
        for instr in std::mem::take(&mut self.instrs) {
            g = match instr {
                Instr::Scan => g.v(None),
                Instr::HasLabel(labels) => g.has_label(labels),
                Instr::Has(key, predicate) => g.has_p(key, predicate),
                Instr::Filter { condition, current } => {
                    g.gql_filter(Arc::clone(&self.query), condition, self.bindings(current))
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
                Instr::Project { columns, current } => {
                    g.gql_project(Arc::clone(&self.query), columns, self.bindings(current))
                }
                Instr::Dedup => g.dedup(),
                Instr::Order(keys) => {
                    g.gql_order(Arc::clone(&self.query), keys, Arc::clone(&order_bindings))
                }
                Instr::Skip(n) => g.skip(n),
                Instr::Limit(n) => g.limit(n),
                Instr::Hop(hop) => self.hop(g, hop),
                Instr::Select(id) => g.select(self.label(id)),
                Instr::Same(id) => g.where_p(P::Eq(PValue::String(self.label(id)))),
                Instr::Differs(id) => g.where_p(P::Neq(PValue::String(self.label(id)))),
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
    fn hop<'g>(&self, g: GraphTraversalSource<'g, S>, hop: Hop<S>) -> GraphTraversalSource<'g, S>
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
            Instr::Filter { condition, current } => {
                t.gql_filter(Arc::clone(&self.query), condition, self.bindings(current))
            }
            Instr::Label(id) if self.is_labelled(id) => t.as_(self.label(id)),
            Instr::Same(id) => t.where_p(P::Eq(PValue::String(self.label(id)))),
            Instr::Differs(id) => t.where_p(P::Neq(PValue::String(self.label(id)))),
            Instr::Label(_)
            | Instr::Scan
            | Instr::Project { .. }
            | Instr::Dedup
            | Instr::Order(_)
            | Instr::Skip(_)
            | Instr::Limit(_)
            | Instr::Hop(_)
            | Instr::Select(_)
            | Instr::Merge { .. } => t,
        }
    }
}

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
    refuse_unplanned(&parsed, &bound)?;
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
        if !self.plan.stages.is_empty() {
            return Err(self.unlowered());
        }
        let mut instrs = Vec::new();
        let mut current: Option<BindingId> = None;
        let ops = &self.plan.ops;
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
                    current = Some(scan.binding);
                }
                Op::Filter(ids) => {
                    for &id in ids {
                        self.conjunct(id, current, &mut instrs)?;
                    }
                }
                Op::Expand(expand) => {
                    if !expand.from_current {
                        instrs.push(Instr::Select(expand.from));
                    }
                    instrs.push(Instr::Hop(self.hop(expand)?));
                    if let Some(next) = next_hop(ops, at) {
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
                    current = Some(expand.to);
                }
                Op::Join { binding } => {
                    instrs.push(Instr::Same(*binding));
                    current = Some(*binding);
                }
                Op::Anchor(scan) => return Err(self.anchor_not_yet(scan.binding)),
                Op::Resume(_) => return Err(self.unlowered()),
            }
        }
        let columns = self.result(current, &mut instrs)?;
        let labelled = self.labelled(&instrs);
        let labels = self.labels();
        Ok(Lowered {
            query: self.shared,
            instrs,
            labels,
            labelled,
            columns,
        })
    }

    /// The error of a plan part the lowering does not emit yet (stage 1b: later segments,
    /// aggregation). `bind::refuse_unplanned` refuses those queries with their own messages
    /// first, so this is a guard only.
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
                current: Some(id),
            });
        }
        for c in residual {
            self.conjunct(c, Some(id), &mut instrs)?;
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
            checks.push(Instr::Same(id));
        }
        checks.extend(expand.trail.iter().map(|&other| Instr::Differs(other)));
        let (labels, mut steps) = if expand.edge_join {
            (None, checks)
        } else {
            self.element(id, &expand.edge_filters, checks)?
        };
        if !expand.edge_join {
            steps.push(Instr::Label(id));
        }
        Ok(Hop {
            direction: expand.direction,
            labels,
            steps,
        })
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

    /// `gql:filter` of conjunct `c` (skipped when it is constantly TRUE).
    fn conjunct<S: GraphStorage>(
        &self,
        c: usize,
        current: Option<BindingId>,
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
        instrs.push(Instr::Filter { condition, current });
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

    /// The result pipeline; returns the number of columns.
    fn result<S: GraphStorage>(
        &self,
        current: Option<BindingId>,
        instrs: &mut Vec<Instr<S>>,
    ) -> Result<usize, TraverserError> {
        let n = self.bound.bindings.len();
        let columns = &self.bound.columns;
        for op in &self.plan.result {
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
                        current,
                    });
                }
                ResultOp::Aggregate => return Err(self.unlowered()),
                ResultOp::Distinct => instrs.push(Instr::Dedup),
                ResultOp::Order => {
                    let aliases: Vec<(String, usize)> = columns
                        .iter()
                        .enumerate()
                        .filter(|(_, c)| c.aliased)
                        .map(|(i, c)| (c.name.clone(), n + i))
                        .collect();
                    let mut keys = Vec::with_capacity(self.bound.order_by.len());
                    for key in &self.bound.order_by {
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
                            None => self.compile(&key.expr, None, &aliases)?,
                        };
                        keys.push(GqlSortKey {
                            expr,
                            descending: key.direction == SortDirection::Desc,
                        });
                    }
                    instrs.push(Instr::Order(keys));
                }
                ResultOp::Offset(count) => instrs.push(Instr::Skip(self.count(count, "OFFSET")?)),
                ResultOp::Limit(count) => instrs.push(Instr::Limit(self.count(count, "LIMIT")?)),
            }
        }
        Ok(columns.len())
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
    /// read away from the current element.
    fn labelled<S: GraphStorage>(&self, instrs: &[Instr<S>]) -> Vec<bool> {
        let n = self.bound.bindings.len();
        let mut labelled: Vec<bool> = self
            .plan
            .labelling
            .iter()
            .map(|l| l.join || l.trail || l.branch)
            .collect();
        labelled.resize(n, false);
        mark_reads(instrs, n, &mut labelled);
        labelled
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

/// Marks every binding an expression of `instrs` reads while it is not the current element.
fn mark_reads<S: GraphStorage>(instrs: &[Instr<S>], n: usize, labelled: &mut [bool]) {
    let mut slots = Vec::new();
    let mark = |labelled: &mut [bool], slots: &mut Vec<usize>, current: Option<BindingId>| {
        for slot in slots.drain(..) {
            if slot < n
                && current != Some(BindingId(slot))
                && let Some(flag) = labelled.get_mut(slot)
            {
                *flag = true;
            }
        }
    };
    for instr in instrs {
        match instr {
            Instr::Filter { condition, current } => {
                condition.variables(&mut slots);
                mark(labelled, &mut slots, *current);
            }
            Instr::Project { columns, current } => {
                for column in columns {
                    column.expr.variables(&mut slots);
                }
                mark(labelled, &mut slots, *current);
            }
            Instr::Order(keys) => {
                for key in keys {
                    key.expr.variables(&mut slots);
                }
                mark(labelled, &mut slots, None);
            }
            Instr::Hop(hop) => mark_reads(&hop.steps, n, labelled),
            Instr::Scan
            | Instr::HasLabel(_)
            | Instr::Has(..)
            | Instr::Label(_)
            | Instr::Dedup
            | Instr::Skip(_)
            | Instr::Limit(_)
            | Instr::Select(_)
            | Instr::Same(_)
            | Instr::Differs(_)
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
