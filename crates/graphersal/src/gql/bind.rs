//! The binder: resolves the names of a parsed GQL query into a [`BoundQuery`], the input of the
//! planner and the lowering.
//!
//! [`bind`] (or [`bind_with_parameters`] when the supplied parameter names are known) never
//! panics; every semantic error is a [`TraverserError::Gql`] of kind
//! [`GqlErrorKind::Semantic`] with the span of the offending text and, for a misspelt name, a
//! "did you mean" suggestion; a `MATCH` clause past the pattern size limit, or an expression that
//! grows too large when its `LET` / `WITH` variables are substituted, is kind
//! [`GqlErrorKind::Limit`] (see "Limits").
//!
//! # What it produces
//!
//! - **Bindings.** Every node and edge pattern element gets a [`BindingId`] and a
//!   [`BindingKind`]; ids follow the first appearance in the text. A named variable written
//!   again while it is in scope (in the same pattern, another pattern or a later `MATCH`) is the
//!   same binding, a join; anonymous elements get bindings without a name. Each binding collects
//!   the label expressions of its occurrences in its own segment (a conjunction); a label written
//!   on an occurrence in a later segment is a conjunct of that segment instead
//!   ([`ConjunctOrigin::Label`]). A path variable `p = ...` is a binding of kind
//!   [`BindingKind::Path`]; the edge variable of a quantified edge is a **group variable**
//!   ([`Binding::group`]): inside its own element pattern it is the edge of one iteration,
//!   everywhere else the list of the matched edges. Each column of a materializing `WITH` is a
//!   new binding ([`Binding::column`]): of the element's kind when the column is a node or edge
//!   variable, else [`BindingKind::Value`].
//! - **Segments (plan mechanism 2).** [`BoundQuery::segments`] cuts the pipeline where the rows
//!   change shape: the non-optional `MATCH` clauses between two cuts form one
//!   [`Segment::Match`] (planned together, as in stage 1a); each `OPTIONAL MATCH` is a
//!   [`Segment::Optional`]; each materializing `WITH` (one with an aggregate, `DISTINCT`,
//!   `ORDER BY`, `OFFSET` / `SKIP` or `LIMIT`, decision C2) is a [`Segment::With`]. A stage 1a
//!   query is exactly one `Segment::Match` holding every clause.
//! - **Substitutions (decision C1).** `LET x = expr` and a `WITH` that only projects are not
//!   operations: the new name stands for its expression, and every expression that reads it is
//!   stored with the expression substituted (a name standing for a node or edge variable becomes
//!   a reference to that binding's [`Binding::key`]). `FILTER`, the `WHERE` of a projecting
//!   `WITH` and the `WHERE` of a materializing one are conjuncts like `WHERE`
//!   ([`ConjunctOrigin::Filter`]).
//! - **Conjuncts.** Inline property maps (`{k: expr}` becomes `var.k = expr`), inline `WHERE`,
//!   the clause `WHERE` and `FILTER` are split at their top-level `AND`s into [`Conjunct`]s, each
//!   tagged with the bindings it reads and its segment. A conjunct reading exactly one node or
//!   edge binding of its own segment is listed on that binding ([`Binding::predicates`]), except
//!   a `WHERE` / `FILTER` conjunct on a group variable (it reads the list); the others are in
//!   [`BoundQuery::cross`]. Whether a conjunct is pushed down into `has()` is the lowering's
//!   decision (the plan's Phase 0 table), not the binder's.
//! - **Read flags (design D11).** [`Binding::read`] is set when an expression reads the binding
//!   away from its own position: a conjunct in [`BoundQuery::cross`] (over several bindings, or
//!   over one binding of another segment), a `RETURN` / `WITH` column, an `ORDER BY` key. A
//!   conjunct over the binding alone in its segment is evaluated where the binding is the
//!   current element and does not set it. A repeated variable sets [`Binding::joined`]; whether
//!   the join needs a path label depends on the plan (continuing from the variable needs none,
//!   closing a cycle does), so the planner decides.
//! - **Columns.** In written order: the alias when given, else the expression text exactly as
//!   written (Neo4j's rule; the ISO naming rule of an unaliased column is still to be verified,
//!   so this is provisional). `RETURN *` / `WITH *` come first and expand to every named variable
//!   in scope in the order it entered the scope (design D10). A `WITH` item that is not a bare
//!   variable needs `AS` (Neo4j's rule). A column holding an aggregate call is
//!   [`Column::aggregate`]; the other columns are the grouping keys (implicit grouping).
//! - **`ORDER BY`** keys may name a column alias (an alias shadows a variable of the same name,
//!   as in Neo4j) or read any variable in scope; a key whose text equals a column's expression
//!   text is that column. After `DISTINCT` or an aggregation a key may only read returned
//!   columns (Neo4j's rule), and an aggregate in a key must be a whole column.
//! - **`OFFSET` / `LIMIT`** are a non-negative integer literal or a parameter (Neo4j's rule); the
//!   lowering checks a parameter's value.
//! - **Parameters:** every `$name` reference with its span, in text order.
//!
//! # Scope
//!
//! The scope runs along the stages. A `MATCH` clause's inline maps, inline `WHERE` and `WHERE`
//! see the variables in scope before the clause and every variable of the clause (a path
//! variable only in the clause `WHERE`, once its pattern is complete). `LET` adds its names (its
//! definitions read the scope before the `LET`, so one definition cannot read another of the
//! same `LET`; redefining a name in scope is an error). `WITH` replaces the scope by its columns
//! (`WITH *` keeps it); a dropped name written again in a later pattern is a new binding (its
//! key is `#<id>`, decision C14). `RETURN` and its `ORDER BY` see the final scope.
//!
//! # Aggregates
//!
//! Aggregate calls stand in `RETURN` and `WITH` items only; a key of `ORDER BY` may hold one
//! only when the whole key is a column. Refused: an aggregate in `WHERE`, `FILTER`, `LET`, a
//! pattern, `OFFSET` / `LIMIT`, or inside another aggregate; an aggregate item reading, outside
//! its aggregates, anything but constants, parameters, grouping-key variables and grouping-key
//! expressions (by their text).
//!
//! # Errors (GQLSTATUS provisional, like the kind defaults)
//!
//! `42002` (the semantic default, decision C10) for an undefined variable, a variable used as
//! two kinds (node, edge, path, value), an edge variable used twice in one `MATCH` (Neo4j:
//! "Cannot use the same relationship variable ... for multiple relationships"; trail semantics),
//! a repeated group variable, a property read of a group variable outside its element, a path
//! variable read inside its own pattern, `RETURN *` / `WITH *` without a named variable, a
//! duplicate column name, an unaliased `WITH` expression, a redefined or twice-defined `LET`
//! name, a misplaced aggregate, a non-grouped read beside an aggregate, an `ORDER BY` key reading
//! a dropped variable after `DISTINCT` / aggregation, a duplicate key in a map, a parameter that
//! was not supplied. `22G02` for a negative `OFFSET` / `LIMIT` literal, `22G03` for one that is
//! not an integer. Kind `NotYet` (`0A000`) for a condition inside a quantified edge that reads
//! another variable.
//!
//! # Limits (kind `Limit`, `54001`)
//!
//! One `MATCH` clause holds at most [`MAX_MATCH_EDGES`] edge patterns over all its
//! comma-separated paths (a quantified edge counts once). Edge uniqueness checks every edge
//! against every earlier edge of the same clause, so the plan grows with the square of a clause's
//! edges; the limit keeps every query fast to prepare. It is checked for every clause before any
//! other work (the parser itself is linear in a long path). Separate `MATCH` clauses are not
//! limited together: edge uniqueness holds within one clause, and their cost adds up linearly.
//!
//! A substituted expression keeps the parser's bounds (height [`MAX_HEIGHT`], nesting
//! [`MAX_DEPTH`]), and one query substitutes at most [`MAX_SUBSTITUTED_NODES`] expression nodes
//! in all, so chained `LET`s that double an expression each cannot grow it exponentially.
//!
//! # Not planned yet
//!
//! [`refuse_unplanned`] refuses, after binding, the stage 1b constructs the planner and the
//! lowering do not handle yet (kind `NotYet`, `0A000`, with the messages the parser gave in
//! stage 1a); the lowering calls it before planning, and its arms go away as the later stages
//! land.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use super::ast::{
    CallArgs, CaseExpr, CaseWhen, CountClause, EdgeDirection, Expr, ExprKind, Identifier,
    LabelExpr, LetClause, Literal, MapEntry, MapLiteral, MatchClause, NodePattern, OrderBy,
    Quantifier, Query, ReturnItem, SortDirection, Stage, WithClause,
};
use super::lexer::Span;
use super::parser::{FunctionKind, MAX_DEPTH, MAX_HEIGHT, close_to, lookup_function};
use crate::error::{GqlErrorBuilder, GqlErrorKind, TraverserError};

/// The index of a [`Binding`] in [`BoundQuery::bindings`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct BindingId(pub(crate) usize);

/// What a binding holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BindingKind {
    /// A node pattern `(..)`, or a `WITH` column holding a node variable.
    Node,
    /// An edge pattern `-[..]->`, or a `WITH` column holding an edge variable.
    Edge,
    /// A path variable `p = ...`.
    Path,
    /// A column of a materializing `WITH` that is not a node or edge variable.
    Value,
}

/// The column of a materializing `WITH` a binding stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ColumnRef {
    /// The [`Segment::With`] (index into [`BoundQuery::segments`]).
    pub(crate) segment: usize,
    /// The column (index into [`BoundWith::columns`]).
    pub(crate) column: usize,
}

/// One variable of the query (named or anonymous).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Binding {
    /// Its id (its index in [`BoundQuery::bindings`]).
    pub(crate) id: BindingId,
    /// Node, edge, path or value.
    pub(crate) kind: BindingKind,
    /// The variable as written at its first appearance; `None` for an anonymous element.
    pub(crate) name: Option<Identifier>,
    /// The unique name the stored expressions use for it: the variable name, or `#<id>` (with
    /// more `#` in front while that is taken) when an earlier binding of the query had the same
    /// name (a name dropped by `WITH` and bound again, decision C14). `None` when anonymous. In
    /// a stage 1a query it always equals the name.
    pub(crate) key: Option<String>,
    /// The span of every pattern element that binds it, in text order (more than one: a join).
    /// A `WITH` column binding starts with its item; a path binding holds its pattern.
    pub(crate) occurrences: Vec<Span>,
    /// The label expressions of its occurrences in its own segment, all of which must hold.
    pub(crate) labels: Vec<LabelExpr>,
    /// Indexes into [`BoundQuery::conjuncts`] of the conjuncts reading this binding alone in its
    /// segment (module docs).
    pub(crate) predicates: Vec<usize>,
    /// Read by an expression away from its own position (module docs, D11).
    pub(crate) read: bool,
    /// The `MATCH` clause of its first appearance (for a `WITH` column: the last clause before
    /// the `WITH`).
    pub(crate) clause: usize,
    /// The segment of its first appearance (index into [`BoundQuery::segments`]).
    pub(crate) segment: usize,
    /// The edge variable of a quantified edge: a group variable (module docs).
    pub(crate) group: bool,
    /// The materializing `WITH` column it stands for.
    pub(crate) column: Option<ColumnRef>,
}

impl Binding {
    /// Whether the variable is written more than once (a join).
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "read by the tests; the planner decides joins")
    )]
    pub(crate) fn joined(&self) -> bool {
        self.occurrences.len() > 1
    }

    /// Whether it was first bound by an `OPTIONAL MATCH` of `segments` (it may be NULL).
    #[cfg_attr(not(test), allow(dead_code, reason = "read by the tests until T5"))]
    pub(crate) fn optional(&self, segments: &[Segment]) -> bool {
        matches!(segments.get(self.segment), Some(Segment::Optional(_)))
    }
}

/// A bound `MATCH` clause.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BoundMatch {
    /// The comma-separated path patterns in order.
    pub(crate) paths: Vec<BoundPath>,
    /// `OPTIONAL MATCH`.
    pub(crate) optional: bool,
    /// Its segment (index into [`BoundQuery::segments`]).
    pub(crate) segment: usize,
    /// The clause.
    pub(crate) span: Span,
}

/// A bound path pattern.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BoundPath {
    /// The path variable of `p = ...` (a [`BindingKind::Path`] binding).
    pub(crate) variable: Option<BindingId>,
    /// The first node.
    pub(crate) start: BindingId,
    /// The edges, each with the node it leads to.
    pub(crate) steps: Vec<BoundStep>,
    /// The pattern.
    pub(crate) span: Span,
}

/// One edge of a bound path and the node after it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct BoundStep {
    /// The edge binding (a group variable when quantified).
    pub(crate) edge: BindingId,
    /// The direction as written.
    pub(crate) direction: EdgeDirection,
    /// The node binding it leads to.
    pub(crate) node: BindingId,
    /// How often the edge repeats (normalized by the parser, `min <= max`).
    pub(crate) quantifier: Option<Quantifier>,
}

/// A part of the pipeline the planner plans as one unit (module docs, plan mechanism 2).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Segment {
    /// Non-optional `MATCH` clauses planned together (indexes into [`BoundQuery::matches`]).
    /// Empty when the segment only holds conjuncts (a `FILTER`, or the `WHERE` of a projecting
    /// `WITH`, after an `OPTIONAL MATCH` or a materializing `WITH`).
    Match(Vec<usize>),
    /// One `OPTIONAL MATCH` clause (index into [`BoundQuery::matches`]): its own conjuncts run
    /// inside the optional part.
    Optional(usize),
    /// A materializing `WITH`; the conjuncts of this segment are its `WHERE`, over its columns.
    With(Box<BoundWith>),
}

/// A materializing `WITH` (decision C2).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BoundWith {
    /// Its columns in order (expressions over the bindings before it).
    pub(crate) columns: Vec<Column>,
    /// `WITH DISTINCT`.
    pub(crate) distinct: bool,
    /// The `ORDER BY` keys in priority order.
    pub(crate) order_by: Vec<SortKey>,
    /// `OFFSET` / `SKIP`.
    pub(crate) offset: Option<Count>,
    /// `LIMIT`.
    pub(crate) limit: Option<Count>,
    /// The binding of each column (what the names mean after the `WITH`).
    pub(crate) outputs: Vec<BindingId>,
    /// The clause.
    pub(crate) span: Span,
}

/// Where a conjunct was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConjunctOrigin {
    /// An entry of an inline property map `{k: expr}`.
    InlineProperty,
    /// An inline `WHERE` of a node or edge pattern.
    InlineWhere,
    /// The `WHERE` of a `MATCH` clause.
    Where,
    /// `FILTER`, or the `WHERE` of a `WITH`.
    Filter,
    /// The label expression of an occurrence of a binding in a later segment than its first
    /// (`var IS LABELED labels`).
    Label,
}

/// The condition of a [`Conjunct`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Predicate {
    /// `binding.key = value`, from an inline property map (the binding may be anonymous).
    Property {
        /// The element whose property is compared.
        binding: BindingId,
        /// The property key.
        key: Identifier,
        /// The value it must equal.
        value: Expr,
    },
    /// A `WHERE` / `FILTER` conjunct (no top-level `AND`), or an occurrence label test.
    Expr(Expr),
}

/// One top-level `AND` conjunct of the query's filters; a row passes only when every conjunct
/// is `TRUE`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Conjunct {
    /// The condition (`LET` / `WITH` names substituted).
    pub(crate) predicate: Predicate,
    /// The bindings it reads, sorted, without duplicates.
    pub(crate) bindings: Vec<BindingId>,
    /// Where it was written.
    pub(crate) origin: ConjunctOrigin,
    /// The `MATCH` clause it belongs to (for `FILTER` / `WITH`: the last clause before it).
    pub(crate) clause: usize,
    /// The segment it runs in (index into [`BoundQuery::segments`]).
    pub(crate) segment: usize,
    /// Its text (`k: expr` for an inline property).
    pub(crate) span: Span,
}

/// One `RETURN` or `WITH` column.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Column {
    /// The column name: the alias, else the expression text as written.
    pub(crate) name: String,
    /// The expression (`LET` / `WITH` names substituted; a variable reference for a column of
    /// `*`).
    pub(crate) expr: Expr,
    /// The binding when the expression is a bare variable (or a name standing for one).
    pub(crate) variable: Option<BindingId>,
    /// The bindings it reads, sorted, without duplicates.
    pub(crate) bindings: Vec<BindingId>,
    /// Whether the name comes from `AS`.
    pub(crate) aliased: bool,
    /// Whether the expression holds an aggregate call (else the column is a grouping key when
    /// another column aggregates).
    pub(crate) aggregate: bool,
    /// The expression as written (for a column of `*`: the name where it entered the scope).
    pub(crate) written: Span,
    /// The item (the `*` for a column of `*`).
    pub(crate) span: Span,
}

/// One `ORDER BY` key.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SortKey {
    /// The key expression (`LET` / `WITH` names substituted, aliases kept).
    pub(crate) expr: Expr,
    /// The direction.
    pub(crate) direction: SortDirection,
    /// The column the whole key is (an alias, or the same text as a column's expression).
    pub(crate) column: Option<usize>,
    /// The bindings the key reads (through a column too), sorted, without duplicates.
    pub(crate) bindings: Vec<BindingId>,
    /// The aliased columns the key names inside its expression (identifiers that resolve to an
    /// alias rather than a variable), sorted, without duplicates.
    pub(crate) aliases: Vec<usize>,
    /// The item.
    pub(crate) span: Span,
}

/// The value of `OFFSET` / `LIMIT`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Count {
    /// A non-negative integer literal.
    Literal(u64),
    /// `$name`: the lowering checks the supplied value.
    Parameter {
        /// The parameter name.
        name: String,
        /// The `$name` text.
        span: Span,
    },
}

/// One `$name` reference.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ParameterRef {
    /// The name without `$`.
    pub(crate) name: String,
    /// The `$name` text.
    pub(crate) span: Span,
}

/// A query with every name resolved (module docs).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BoundQuery {
    /// Every binding, by [`BindingId`], in order of first appearance.
    pub(crate) bindings: Vec<Binding>,
    /// The `[OPTIONAL] MATCH` clauses.
    pub(crate) matches: Vec<BoundMatch>,
    /// The pipeline before `RETURN`, cut into segments (module docs).
    pub(crate) segments: Vec<Segment>,
    /// Every conjunct of every filter, in text order per clause.
    pub(crate) conjuncts: Vec<Conjunct>,
    /// Indexes into [`Self::conjuncts`] of those not listed on a binding (module docs).
    pub(crate) cross: Vec<usize>,
    /// The `RETURN` columns in order.
    pub(crate) columns: Vec<Column>,
    /// `RETURN DISTINCT`.
    pub(crate) distinct: bool,
    /// The `ORDER BY` keys in priority order.
    pub(crate) order_by: Vec<SortKey>,
    /// `OFFSET` / `SKIP`.
    pub(crate) offset: Option<Count>,
    /// `LIMIT`.
    pub(crate) limit: Option<Count>,
    /// Every parameter reference in text order.
    pub(crate) parameters: Vec<ParameterRef>,
}

impl BoundQuery {
    /// The binding `id`.
    pub(crate) fn binding(&self, id: BindingId) -> Option<&Binding> {
        self.bindings.get(id.0)
    }

    /// The first binding of the variable `name`.
    #[cfg_attr(not(test), allow(dead_code, reason = "used by the tests"))]
    pub(crate) fn lookup(&self, name: &str) -> Option<BindingId> {
        self.bindings
            .iter()
            .find(|b| b.name.as_ref().is_some_and(|n| n.name == name))
            .map(|b| b.id)
    }

    /// Whether `RETURN` aggregates (some column holds an aggregate call).
    #[cfg_attr(not(test), allow(dead_code, reason = "used by the tests"))]
    pub(crate) fn aggregated(&self) -> bool {
        self.columns.iter().any(|c| c.aggregate)
    }
}

/// Binds `query` without checking parameter values (the caller checks them later).
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "used by the tests; queries bind with their parameters"
    )
)]
pub(crate) fn bind(query: &Query) -> Result<BoundQuery, TraverserError> {
    Binder::new(query)
        .run(None)
        .map_err(|e| e.into_error(&query.source))
}

/// Binds `query` and checks that every `$name` it reads is among `supplied` (a missing one is a
/// semantic error suggesting the closest supplied name).
pub(crate) fn bind_with_parameters<'a>(
    query: &Query,
    supplied: impl IntoIterator<Item = &'a str>,
) -> Result<BoundQuery, TraverserError> {
    let supplied: Vec<&str> = supplied.into_iter().collect();
    Binder::new(query)
        .run(Some(&supplied))
        .map_err(|e| e.into_error(&query.source))
}

/// The most edge patterns one `MATCH` clause may hold, over all its comma-separated paths
/// (module docs, "Limits"); a quantified edge counts once.
///
/// Every edge of a clause is checked against every earlier edge of the clause (edge
/// uniqueness, one `where(P.neq(..))` per pair of edges whose labels may overlap), so a clause
/// of `n` edges plans `n(n-1)/2` checks and the lowering, the optimizer and the path analysis
/// are quadratic in `n`: in a debug build 400 edges took 0.3 s to prepare and run, 2000 edges
/// 8 s. 64 edges (2016 checks) take a few milliseconds and lie far beyond any realistic
/// pattern; a longer walk is a Gremlin `repeat()`.
pub(crate) const MAX_MATCH_EDGES: usize = 64;

/// The most expression nodes one query may substitute for `LET` / `WITH` names in all (module
/// docs, "Limits"). Far above any written query (a 100-node definition read a thousand times),
/// low enough that binding stays fast.
pub(crate) const MAX_SUBSTITUTED_NODES: usize = 100_000;

type BResult<T> = Result<T, Box<GqlErrorBuilder>>;

fn semantic(span: Span, message: impl Into<String>) -> GqlErrorBuilder {
    GqlErrorBuilder::new(GqlErrorKind::Semantic, span.start..span.end, message)
}

/// The error of a `MATCH` clause with more than [`MAX_MATCH_EDGES`] edge patterns, at the first
/// edge past the limit.
fn too_many_edges(span: Span) -> Box<GqlErrorBuilder> {
    let b = GqlErrorBuilder::new(
        GqlErrorKind::Limit,
        span.start..span.end,
        format!("the MATCH clause has more than {MAX_MATCH_EDGES} edge patterns"),
    )
    .alternative(
        "split the pattern into several MATCH clauses that share a variable, for example \
         MATCH (a)-->(b)-->(c) MATCH (c)-->(d)-->(e) RETURN e (edge uniqueness then holds \
         within each clause only), or walk a long path with Gremlin repeat(), for example \
         g.v().repeat(__.out()).times(100)",
    );
    Box::new(b)
}

/// The error of an expression that grows past the bounds when its `LET` / `WITH` names are
/// substituted.
fn too_large(span: Span, what: &str) -> Box<GqlErrorBuilder> {
    Box::new(
        GqlErrorBuilder::new(
            GqlErrorKind::Limit,
            span.start..span.end,
            format!("the expression is too large once its LET and WITH variables are substituted: {what}"),
        )
        .alternative(
            "compute the value once in a WITH that aggregates or orders (its columns are not \
             substituted), or write a smaller expression",
        ),
    )
}

/// The prefix of `span` that holds a keyword of `len` ASCII characters.
fn keyword_span(span: Span, len: usize) -> Span {
    Span {
        end: span.start.saturating_add(len).min(span.end),
        ..span
    }
}

fn not_yet(span: Span, what: &str, alternative: &str) -> Box<GqlErrorBuilder> {
    Box::new(
        GqlErrorBuilder::new(
            GqlErrorKind::NotYet,
            span.start..span.end,
            format!("{what} is not yet supported in this version"),
        )
        .alternative(alternative),
    )
}

/// Refuses the stage 1b constructs that bind but that the planner and the lowering do not
/// handle yet: path variables (kind
/// `NotYet`, the messages the parser gave in stage 1a). Called by the lowering
/// between binding and planning; the arms go away as the later stages land.
pub(crate) fn refuse_unplanned(query: &Query, _bound: &BoundQuery) -> Result<(), TraverserError> {
    unplanned(query).map_err(|e| e.into_error(&query.source))
}

fn unplanned(query: &Query) -> BResult<()> {
    for m in &query.matches {
        for path in &m.patterns {
            if let Some(variable) = &path.variable {
                return Err(not_yet(
                    variable.span,
                    "a path variable (p = ...)",
                    "return the elements by their variables, or use Gremlin path(), for example \
                     g.v(\"1\").out().out().path()",
                ));
            }
        }
    }
    Ok(())
}

/// Whether `name` is an aggregate function.
fn is_aggregate(name: &Identifier) -> bool {
    lookup_function(&name.name).is_some_and(|spec| spec.kind == FunctionKind::Aggregate)
}

/// Calls `f` on every direct sub-expression of `expr`, left to right.
fn for_each_child<'e>(expr: &'e Expr, f: &mut impl FnMut(&'e Expr)) {
    match &expr.kind {
        ExprKind::Literal(_) | ExprKind::Parameter(_) | ExprKind::Variable(_) => {}
        ExprKind::List(items) => items.iter().for_each(f),
        ExprKind::Map(map) => map.entries.iter().for_each(|e| f(&e.value)),
        ExprKind::Property { target, .. } => f(target),
        ExprKind::Unary { operand, .. }
        | ExprKind::IsNull { operand, .. }
        | ExprKind::IsLabeled { operand, .. } => f(operand),
        ExprKind::Binary { left, right, .. } => {
            f(left);
            f(right);
        }
        ExprKind::Case(case) => {
            if let Some(operand) = &case.operand {
                f(operand);
            }
            for arm in &case.whens {
                f(&arm.when);
                f(&arm.then);
            }
            if let Some(else_result) = &case.else_result {
                f(else_result);
            }
        }
        ExprKind::FunctionCall { args, .. } => match args {
            CallArgs::Star(_) => {}
            CallArgs::Exprs(args) => args.iter().for_each(f),
        },
    }
}

/// The closest of `candidates` to `word` (a case-only difference is the closest).
fn suggest<'c>(word: &str, candidates: impl IntoIterator<Item = &'c str>) -> Option<&'c str> {
    candidates
        .into_iter()
        .filter_map(|c| {
            if c != word && c.eq_ignore_ascii_case(word) {
                Some((0, c))
            } else {
                close_to(word, c).map(|d| (d, c))
            }
        })
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c)
}

/// What a name in scope stands for.
#[derive(Debug, Clone)]
enum Symbol {
    /// A binding (also a `LET` / `WITH` name for a node, edge, path or value binding).
    Binding(BindingId),
    /// A `LET` / projecting-`WITH` name for an expression.
    Value(Rc<Substitution>),
}

/// The expression a `LET` / projecting-`WITH` name stands for.
#[derive(Debug)]
struct Substitution {
    /// The expression, its own names already substituted.
    expr: Expr,
    /// Its shape (bounds of the substituted expressions).
    shape: Shape,
    /// The bindings it reads, sorted, without duplicates.
    bindings: Vec<BindingId>,
}

/// The size of an expression tree: height, nesting the evaluator recurses on (left spines are
/// loops there, so they do not nest), node count.
#[derive(Debug, Clone, Copy)]
struct Shape {
    height: usize,
    depth: usize,
    nodes: usize,
}

impl Shape {
    const LEAF: Shape = Shape {
        height: 1,
        depth: 1,
        nodes: 1,
    };
}

/// The names in scope, in the order they entered it.
#[derive(Debug, Clone, Default)]
struct Names {
    entries: Vec<(Identifier, Symbol)>,
    index: HashMap<String, usize>,
}

impl Names {
    fn get(&self, name: &str) -> Option<&Symbol> {
        self.index
            .get(name)
            .and_then(|i| self.entries.get(*i))
            .map(|(_, symbol)| symbol)
    }

    /// Adds `name` (the caller checked it is not in scope; a repeat replaces the meaning).
    fn insert(&mut self, name: Identifier, symbol: Symbol) {
        match self.index.get(&name.name).copied() {
            Some(i) => {
                if let Some(entry) = self.entries.get_mut(i) {
                    *entry = (name, symbol);
                }
            }
            None => {
                self.index.insert(name.name.clone(), self.entries.len());
                self.entries.push((name, symbol));
            }
        }
    }

    fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(n, _)| n.name.as_str())
    }
}

/// Where aggregate calls may stand in an expression.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Aggregates {
    /// A `RETURN` / `WITH` item or an `ORDER BY` key.
    Allowed,
    /// Refused; the place for the message ("in WHERE").
    Refused(&'static str),
}

/// The context of one expression.
#[derive(Clone, Copy)]
struct Ctx<'s> {
    /// The aliased columns (`ORDER BY` only): name and column index.
    aliases: &'s [(String, usize)],
    /// Whether aggregates may stand here.
    aggregates: Aggregates,
    /// The element whose inline filter this is (its group variable is one edge there).
    element: Option<BindingId>,
    /// The `MATCH` clause of an inline filter (its path variables are not complete there).
    inline_clause: Option<usize>,
}

impl Ctx<'_> {
    fn refusing(place: &'static str) -> Ctx<'static> {
        Ctx {
            aliases: &[],
            aggregates: Aggregates::Refused(place),
            element: None,
            inline_clause: None,
        }
    }

    fn items() -> Ctx<'static> {
        Ctx {
            aliases: &[],
            aggregates: Aggregates::Allowed,
            element: None,
            inline_clause: None,
        }
    }
}

/// What an expression reads.
#[derive(Default)]
struct Refs {
    bindings: Vec<BindingId>,
    aliases: Vec<usize>,
    /// It reads a name that must be substituted.
    substitute: bool,
    /// The first aggregate call in it.
    aggregate: Option<Span>,
}

impl Refs {
    fn finish(mut self) -> Refs {
        self.bindings.sort_unstable();
        self.bindings.dedup();
        self.aliases.sort_unstable();
        self.aliases.dedup();
        self
    }
}

/// An expression with its names resolved and substituted.
struct Resolved {
    expr: Expr,
    refs: Refs,
    shape: Shape,
}

struct Binder<'q> {
    query: &'q Query,
    bindings: Vec<Binding>,
    /// The names in scope now.
    scope: Names,
    /// Every name and key given to a binding so far (keys stay unique, C14).
    taken: HashSet<String>,
    /// Names a `WITH` dropped from the scope, with that `WITH`'s keyword.
    dropped: HashMap<String, Span>,
    /// The names of the `LET` being bound (its definitions cannot read them).
    pending_let: Vec<String>,
    conjuncts: Vec<Conjunct>,
    parameters: Vec<ParameterRef>,
    segments: Vec<Segment>,
    /// The `Segment::Match` later plain clauses and filters join, while no cut came after it.
    open: Option<usize>,
    /// The last `MATCH` clause bound.
    clause: usize,
    /// Expression nodes left to substitute.
    budget: usize,
}

impl<'q> Binder<'q> {
    fn new(query: &'q Query) -> Self {
        Binder {
            query,
            bindings: Vec::new(),
            scope: Names::default(),
            taken: HashSet::new(),
            dropped: HashMap::new(),
            pending_let: Vec::new(),
            conjuncts: Vec::new(),
            parameters: Vec::new(),
            segments: Vec::new(),
            open: None,
            clause: 0,
            budget: MAX_SUBSTITUTED_NODES,
        }
    }

    fn run(mut self, supplied: Option<&[&str]>) -> BResult<BoundQuery> {
        // The pattern size of every clause first, before any other work.
        for m in &self.query.matches {
            if let Some(edge) = m
                .patterns
                .iter()
                .flat_map(|path| &path.steps)
                .nth(MAX_MATCH_EDGES)
            {
                return Err(too_many_edges(edge.edge.span));
            }
        }
        let query = self.query;
        let mut matches = Vec::with_capacity(query.matches.len());
        for stage in &query.stages {
            match stage {
                Stage::Match(index) => {
                    let m = query
                        .matches
                        .get(*index)
                        .filter(|_| *index == matches.len())
                        .ok_or_else(|| {
                            semantic(query.span, "internal error: MATCH stages out of order")
                        })?;
                    self.clause = *index;
                    let segment = if m.optional {
                        self.open = None;
                        self.segments.push(Segment::Optional(*index));
                        self.segments.len() - 1
                    } else {
                        let segment = self.open_segment();
                        if let Some(Segment::Match(clauses)) = self.segments.get_mut(segment) {
                            clauses.push(*index);
                        }
                        segment
                    };
                    let bound = self.declare_clause(*index, m, segment)?;
                    self.clause_filters(*index, m, &bound)?;
                    matches.push(bound);
                }
                Stage::Let(clause) => self.let_clause(clause)?,
                Stage::Filter(clause) => {
                    let segment = self.open_segment();
                    self.conjuncts_of(
                        Ctx::refusing("in FILTER"),
                        &clause.condition,
                        ConjunctOrigin::Filter,
                        segment,
                    )?;
                }
                Stage::With(with) => self.with_clause(with)?,
            }
        }
        let ret = &query.ret;
        let columns = self.projection(ret.star, &ret.items, "RETURN")?;
        let order_by = self.order_by(query.order_by.as_ref(), &columns, ret.distinct, "RETURN")?;
        let offset = self.count(query.offset.as_ref(), "OFFSET")?;
        let limit = self.count(query.limit.as_ref(), "LIMIT")?;
        self.parameters.sort_by_key(|p| p.span.start);
        if let Some(supplied) = supplied {
            self.check_parameters(supplied)?;
        }
        let mut cross = Vec::new();
        for index in 0..self.conjuncts.len() {
            match self.local_binding(index) {
                Some(only) => {
                    if let Some(binding) = self.bindings.get_mut(only.0) {
                        binding.predicates.push(index);
                    }
                }
                None => cross.push(index),
            }
        }
        Ok(BoundQuery {
            bindings: self.bindings,
            matches,
            segments: self.segments,
            conjuncts: self.conjuncts,
            cross,
            columns,
            distinct: ret.distinct,
            order_by,
            offset,
            limit,
            parameters: self.parameters,
        })
    }

    /// The binding a conjunct is listed on (module docs): its only binding, a node or edge of
    /// the conjunct's segment, unless a `WHERE` / `FILTER` reads a group variable.
    fn local_binding(&self, index: usize) -> Option<BindingId> {
        let conjunct = self.conjuncts.get(index)?;
        let [only] = conjunct.bindings[..] else {
            return None;
        };
        let binding = self.bindings.get(only.0)?;
        let element = matches!(binding.kind, BindingKind::Node | BindingKind::Edge);
        let inline = matches!(
            conjunct.origin,
            ConjunctOrigin::InlineProperty | ConjunctOrigin::InlineWhere
        );
        (element
            && binding.column.is_none()
            && binding.segment == conjunct.segment
            && (!binding.group || inline))
            .then_some(only)
    }

    /// The open `Segment::Match`, opened when a cut came last.
    fn open_segment(&mut self) -> usize {
        match self.open {
            Some(segment) => segment,
            None => {
                self.segments.push(Segment::Match(Vec::new()));
                let segment = self.segments.len() - 1;
                self.open = Some(segment);
                segment
            }
        }
    }

    // ----- patterns -----

    fn declare_clause(
        &mut self,
        clause: usize,
        m: &MatchClause,
        segment: usize,
    ) -> BResult<BoundMatch> {
        let mut edges_here: Vec<BindingId> = Vec::new();
        let mut paths = Vec::with_capacity(m.patterns.len());
        for path in &m.patterns {
            let variable = match &path.variable {
                Some(v) => Some(self.declare_path(clause, v, path.span, segment)?),
                None => None,
            };
            let start = self.node(clause, &path.start, segment)?;
            let mut steps = Vec::with_capacity(path.steps.len());
            for step in &path.steps {
                let e = &step.edge;
                let edge = self.declare(
                    Site {
                        clause,
                        segment,
                        kind: BindingKind::Edge,
                        group: e.quantifier.is_some(),
                    },
                    e.variable.as_ref(),
                    e.span,
                    e.labels.as_ref(),
                )?;
                if let Some(var) = &e.variable {
                    if edges_here.contains(&edge) {
                        return Err(semantic(
                            var.span,
                            format!(
                                "the edge variable `{}` is used for more than one edge in the \
                                 same MATCH (an edge is bound once per MATCH)",
                                var.name
                            ),
                        )
                        .alternative(format!(
                            "give the second edge its own variable, or put the second pattern \
                             in a MATCH clause of its own to join on the same edge `{}`",
                            var.name
                        ))
                        .into());
                    }
                    edges_here.push(edge);
                }
                let node = self.node(clause, &step.node, segment)?;
                steps.push(BoundStep {
                    edge,
                    direction: e.direction,
                    node,
                    quantifier: e.quantifier,
                });
            }
            paths.push(BoundPath {
                variable,
                start,
                steps,
                span: path.span,
            });
        }
        Ok(BoundMatch {
            paths,
            optional: m.optional,
            segment,
            span: m.span,
        })
    }

    fn clause_filters(
        &mut self,
        clause: usize,
        m: &MatchClause,
        bound: &BoundMatch,
    ) -> BResult<()> {
        let segment = bound.segment;
        for (path, bound) in m.patterns.iter().zip(&bound.paths) {
            self.element_filters(
                clause,
                segment,
                bound.start,
                path.start.properties.as_ref(),
                path.start.where_clause.as_ref(),
            )?;
            for (step, bound_step) in path.steps.iter().zip(&bound.steps) {
                self.element_filters(
                    clause,
                    segment,
                    bound_step.edge,
                    step.edge.properties.as_ref(),
                    step.edge.where_clause.as_ref(),
                )?;
                self.element_filters(
                    clause,
                    segment,
                    bound_step.node,
                    step.node.properties.as_ref(),
                    step.node.where_clause.as_ref(),
                )?;
            }
        }
        if let Some(where_clause) = &m.where_clause {
            self.conjuncts_of(
                Ctx::refusing("in WHERE"),
                where_clause,
                ConjunctOrigin::Where,
                segment,
            )?;
        }
        Ok(())
    }

    fn node(&mut self, clause: usize, node: &NodePattern, segment: usize) -> BResult<BindingId> {
        self.declare(
            Site {
                clause,
                segment,
                kind: BindingKind::Node,
                group: false,
            },
            node.variable.as_ref(),
            node.span,
            node.labels.as_ref(),
        )
    }

    /// A new binding; a named one enters the scope.
    fn new_binding(
        &mut self,
        site: Site,
        name: Option<&Identifier>,
        span: Span,
        labels: Option<&LabelExpr>,
        column: Option<ColumnRef>,
    ) -> BindingId {
        let id = BindingId(self.bindings.len());
        let key = name.map(|v| {
            let key = if self.taken.contains(&v.name) {
                let mut key = format!("#{}", id.0);
                while self.taken.contains(&key) {
                    key.insert(0, '#');
                }
                key
            } else {
                v.name.clone()
            };
            self.taken.insert(v.name.clone());
            self.taken.insert(key.clone());
            key
        });
        if let Some(v) = name {
            self.scope.insert(v.clone(), Symbol::Binding(id));
            self.dropped.remove(&v.name);
        }
        self.bindings.push(Binding {
            id,
            kind: site.kind,
            name: name.cloned(),
            key,
            occurrences: vec![span],
            labels: labels.into_iter().cloned().collect(),
            predicates: Vec::new(),
            read: false,
            clause: site.clause,
            segment: site.segment,
            group: site.group,
            column,
        });
        id
    }

    /// The binding of a path variable: always new.
    fn declare_path(
        &mut self,
        clause: usize,
        variable: &Identifier,
        span: Span,
        segment: usize,
    ) -> BResult<BindingId> {
        if self.scope.get(&variable.name).is_some() {
            return Err(semantic(
                variable.span,
                format!(
                    "the path variable `{}` is already bound; a path variable names a new path",
                    variable.name
                ),
            )
            .alternative(format!(
                "give the path its own name, as in {}2 = ...",
                variable.name
            ))
            .into());
        }
        let site = Site {
            clause,
            segment,
            kind: BindingKind::Path,
            group: false,
        };
        Ok(self.new_binding(site, Some(variable), span, None, None))
    }

    /// The binding of a pattern element: a new one, or the existing one of a variable in scope.
    fn declare(
        &mut self,
        site: Site,
        variable: Option<&Identifier>,
        span: Span,
        labels: Option<&LabelExpr>,
    ) -> BResult<BindingId> {
        let existing = variable.and_then(|v| self.scope.get(&v.name).cloned());
        let (var, id) = match (variable, existing) {
            (Some(var), Some(Symbol::Binding(id))) => (var, id),
            (Some(var), Some(Symbol::Value(_))) => {
                return Err(semantic(
                    var.span,
                    format!(
                        "`{}` is a value defined by LET or WITH, not a {}; a pattern variable is \
                         a new name or a node or edge variable",
                        var.name,
                        kind_word(site.kind)
                    ),
                )
                .alternative(format!(
                    "give the pattern element its own name and compare it, as in ({}2 WHERE ...)",
                    var.name
                ))
                .into());
            }
            (_, _) => return Ok(self.new_binding(site, variable, span, labels, None)),
        };
        let binding = self.bindings.get(id.0).ok_or_else(|| {
            semantic(
                span,
                "internal error: a binding id out of range in the binder",
            )
        })?;
        if binding.kind != site.kind {
            return Err(kind_conflict(var, site.kind, binding.kind));
        }
        if binding.group || site.group {
            return Err(semantic(
                var.span,
                format!(
                    "the edge variable `{}` of a quantified edge is a group variable (the list of \
                     its edges) and cannot be bound again",
                    var.name
                ),
            )
            .alternative("give each edge pattern its own variable, and compare the edges in WHERE")
            .into());
        }
        let same_segment = binding.segment == site.segment;
        let key = binding.key.clone().unwrap_or_default();
        if let Some(binding) = self.bindings.get_mut(id.0) {
            binding.occurrences.push(span);
            if same_segment && let Some(labels) = labels {
                binding.labels.push(labels.clone());
            }
        }
        if !same_segment && let Some(labels) = labels {
            // A label on an occurrence in a later segment is a condition of that segment.
            let operand = Expr {
                kind: ExprKind::Variable(Identifier {
                    name: key,
                    span: var.span,
                }),
                span: var.span,
            };
            self.push_conjunct(Conjunct {
                predicate: Predicate::Expr(Expr {
                    kind: ExprKind::IsLabeled {
                        operand: Box::new(operand),
                        labels: labels.clone(),
                        negated: false,
                    },
                    span: labels.span,
                }),
                bindings: vec![id],
                origin: ConjunctOrigin::Label,
                clause: site.clause,
                segment: site.segment,
                span: labels.span,
            });
        }
        Ok(id)
    }

    /// The inline property map and inline `WHERE` of one pattern element.
    fn element_filters(
        &mut self,
        clause: usize,
        segment: usize,
        binding: BindingId,
        properties: Option<&MapLiteral>,
        where_clause: Option<&Expr>,
    ) -> BResult<()> {
        let ctx = Ctx {
            aliases: &[],
            aggregates: Aggregates::Refused("in a pattern"),
            element: Some(binding),
            inline_clause: Some(clause),
        };
        let first = self.conjuncts.len();
        if let Some(map) = properties {
            check_map_keys(map)?;
            for entry in &map.entries {
                let resolved = self.expression(ctx, &entry.value)?;
                let mut bindings = resolved.refs.bindings;
                bindings.push(binding);
                bindings.sort_unstable();
                bindings.dedup();
                self.push_conjunct(Conjunct {
                    predicate: Predicate::Property {
                        binding,
                        key: entry.key.clone(),
                        value: resolved.expr,
                    },
                    bindings,
                    origin: ConjunctOrigin::InlineProperty,
                    clause,
                    segment,
                    span: entry.key.span.to(entry.value.span),
                });
            }
        }
        if let Some(where_clause) = where_clause {
            self.conjuncts_of(ctx, where_clause, ConjunctOrigin::InlineWhere, segment)?;
        }
        let group = self.bindings.get(binding.0).is_some_and(|b| b.group);
        if group {
            let other = self
                .conjuncts
                .get(first..)
                .unwrap_or(&[])
                .iter()
                .find(|c| c.bindings.iter().any(|b| *b != binding));
            if let Some(conjunct) = other {
                return Err(not_yet(
                    conjunct.span,
                    "a condition inside a quantified edge that reads another variable",
                    "compare the edge with a value or a parameter inside the edge pattern, as in \
                     -[e WHERE e.weight > $w]->{1,3}, or test the list of edges in the WHERE of \
                     the MATCH clause",
                ));
            }
        }
        Ok(())
    }

    /// Splits `expr` at its top-level `AND`s and records each conjunct.
    fn conjuncts_of(
        &mut self,
        ctx: Ctx<'_>,
        expr: &Expr,
        origin: ConjunctOrigin,
        segment: usize,
    ) -> BResult<()> {
        for conjunct in split_and(expr) {
            let resolved = self.expression(ctx, conjunct)?;
            self.push_conjunct(Conjunct {
                predicate: Predicate::Expr(resolved.expr),
                bindings: resolved.refs.bindings,
                origin,
                clause: self.clause,
                segment,
                span: conjunct.span,
            });
        }
        Ok(())
    }

    fn push_conjunct(&mut self, conjunct: Conjunct) {
        self.conjuncts.push(conjunct);
        let index = self.conjuncts.len() - 1;
        if self.local_binding(index).is_none()
            && let Some(conjunct) = self.conjuncts.get(index)
        {
            let ids = conjunct.bindings.clone();
            self.mark_read(&ids);
        }
    }

    fn mark_read(&mut self, ids: &[BindingId]) {
        for id in ids {
            if let Some(binding) = self.bindings.get_mut(id.0) {
                binding.read = true;
            }
        }
    }

    // ----- LET and WITH -----

    fn let_clause(&mut self, clause: &LetClause) -> BResult<()> {
        let mut defined: Vec<(Identifier, Symbol)> = Vec::with_capacity(clause.bindings.len());
        self.pending_let = clause
            .bindings
            .iter()
            .map(|b| b.variable.name.clone())
            .collect();
        for definition in &clause.bindings {
            let var = &definition.variable;
            if self.scope.get(&var.name).is_some() {
                return Err(semantic(
                    var.span,
                    format!(
                        "the variable `{}` is already defined; LET defines a new variable",
                        var.name
                    ),
                )
                .alternative(format!(
                    "choose another name, as in LET {}2 = ...",
                    var.name
                ))
                .into());
            }
            if defined.iter().any(|(n, _)| n.name == var.name) {
                return Err(semantic(
                    var.span,
                    format!("the variable `{}` is defined twice in this LET", var.name),
                )
                .into());
            }
            let resolved = self.expression(Ctx::refusing("in LET"), &definition.value)?;
            let symbol = self.symbol_of(&definition.value, resolved);
            defined.push((var.clone(), symbol));
        }
        self.pending_let.clear();
        for (name, symbol) in defined {
            self.dropped.remove(&name.name);
            self.scope.insert(name, symbol);
        }
        Ok(())
    }

    /// What a name defined as `written` stands for: the binding of a bare name for one, else the
    /// resolved expression.
    fn symbol_of(&self, written: &Expr, resolved: Resolved) -> Symbol {
        if let ExprKind::Variable(var) = &written.kind
            && let Some(symbol) = self.scope.get(&var.name)
        {
            return symbol.clone();
        }
        Symbol::Value(Rc::new(Substitution {
            expr: resolved.expr,
            shape: resolved.shape,
            bindings: resolved.refs.bindings,
        }))
    }

    fn with_clause(&mut self, with: &WithClause) -> BResult<()> {
        let keyword = keyword_span(with.span, 4);
        let (columns, names) = self.projection_named(with.star, &with.items, "WITH")?;
        let aggregated = columns.iter().any(|c| c.aggregate);
        let materializing = aggregated
            || with.distinct
            || with.order_by.is_some()
            || with.offset.is_some()
            || with.limit.is_some();
        if materializing {
            let order_by =
                self.order_by(with.order_by.as_ref(), &columns, with.distinct, "WITH")?;
            let offset = self.count(with.offset.as_ref(), "OFFSET")?;
            let limit = self.count(with.limit.as_ref(), "LIMIT")?;
            let segment = self.segments.len();
            // The columns become new bindings in a scope of their own; the old scope is gone.
            let old = std::mem::take(&mut self.scope);
            let mut outputs = Vec::with_capacity(columns.len());
            for (index, (column, name)) in columns.iter().zip(&names).enumerate() {
                let kind = column
                    .variable
                    .and_then(|id| self.bindings.get(id.0))
                    .filter(|b| !b.group)
                    .map_or(BindingKind::Value, |b| match b.kind {
                        BindingKind::Node => BindingKind::Node,
                        BindingKind::Edge => BindingKind::Edge,
                        BindingKind::Path | BindingKind::Value => BindingKind::Value,
                    });
                let site = Site {
                    clause: self.clause,
                    segment,
                    kind,
                    group: false,
                };
                let reference = ColumnRef {
                    segment,
                    column: index,
                };
                outputs.push(self.new_binding(
                    site,
                    Some(name),
                    column.span,
                    None,
                    Some(reference),
                ));
            }
            let next = std::mem::replace(&mut self.scope, old);
            self.segments.push(Segment::With(Box::new(BoundWith {
                columns,
                distinct: with.distinct,
                order_by,
                offset,
                limit,
                outputs,
                span: with.span,
            })));
            self.open = None;
            self.replace_scope(next, keyword);
            if let Some(where_clause) = &with.where_clause {
                self.conjuncts_of(
                    Ctx::refusing("in the WHERE of WITH"),
                    where_clause,
                    ConjunctOrigin::Filter,
                    segment,
                )?;
            }
        } else {
            let mut next = Names::default();
            for (column, name) in columns.into_iter().zip(names) {
                let symbol = match column.variable {
                    Some(id) => Symbol::Binding(id),
                    None => {
                        let shape = shape_of(&column.expr);
                        Symbol::Value(Rc::new(Substitution {
                            expr: column.expr,
                            shape,
                            bindings: column.bindings,
                        }))
                    }
                };
                next.insert(name, symbol);
            }
            self.replace_scope(next, keyword);
            if let Some(where_clause) = &with.where_clause {
                let segment = self.open_segment();
                self.conjuncts_of(
                    Ctx::refusing("in the WHERE of WITH"),
                    where_clause,
                    ConjunctOrigin::Filter,
                    segment,
                )?;
            }
        }
        Ok(())
    }

    /// Makes `next` the scope; the names it drops are remembered for the error message.
    fn replace_scope(&mut self, next: Names, keyword: Span) {
        let old = std::mem::replace(&mut self.scope, next);
        for (name, _) in old.entries {
            if self.scope.get(&name.name).is_none() {
                self.dropped.insert(name.name, keyword);
            }
        }
        for name in self.scope.names() {
            self.dropped.remove(name);
        }
    }

    // ----- expressions -----

    /// Resolves `expr` and substitutes its `LET` / `WITH` names (module docs, "Limits").
    fn expression(&mut self, ctx: Ctx<'_>, expr: &Expr) -> BResult<Resolved> {
        let mut refs = Refs::default();
        self.resolve(ctx, expr, &mut refs)?;
        let refs = refs.finish();
        if !refs.substitute {
            return Ok(Resolved {
                expr: expr.clone(),
                shape: shape_of(expr),
                refs,
            });
        }
        let (expr, shape) = self.substitute(ctx.aliases, expr)?;
        if shape.height > MAX_HEIGHT {
            return Err(too_large(
                expr.span,
                &format!("its operator tree is deeper than {MAX_HEIGHT} levels"),
            ));
        }
        if shape.depth > MAX_DEPTH {
            return Err(too_large(
                expr.span,
                &format!("it nests deeper than {MAX_DEPTH} levels"),
            ));
        }
        Ok(Resolved { expr, refs, shape })
    }

    /// Resolves every name in `expr` (recursion bounded by the parser's `MAX_HEIGHT`), records
    /// its parameters and checks its map literals and aggregate calls.
    fn resolve(&mut self, ctx: Ctx<'_>, expr: &Expr, refs: &mut Refs) -> BResult<()> {
        match &expr.kind {
            ExprKind::Literal(_) => {}
            ExprKind::Parameter(name) => self.parameters.push(ParameterRef {
                name: name.clone(),
                span: expr.span,
            }),
            ExprKind::Variable(var) => self.variable(ctx, var, refs)?,
            ExprKind::List(items) => {
                for item in items {
                    self.resolve(ctx, item, refs)?;
                }
            }
            ExprKind::Map(map) => {
                check_map_keys(map)?;
                for entry in &map.entries {
                    self.resolve(ctx, &entry.value, refs)?;
                }
            }
            ExprKind::Property { target, key } => {
                if let ExprKind::Variable(var) = &target.kind {
                    self.check_group_property(ctx, var, key)?;
                }
                self.resolve(ctx, target, refs)?;
            }
            ExprKind::Unary { operand, .. }
            | ExprKind::IsNull { operand, .. }
            | ExprKind::IsLabeled { operand, .. } => self.resolve(ctx, operand, refs)?,
            ExprKind::Binary { left, right, .. } => {
                self.resolve(ctx, left, refs)?;
                self.resolve(ctx, right, refs)?;
            }
            ExprKind::Case(case) => {
                if let Some(operand) = &case.operand {
                    self.resolve(ctx, operand, refs)?;
                }
                for arm in &case.whens {
                    self.resolve(ctx, &arm.when, refs)?;
                    self.resolve(ctx, &arm.then, refs)?;
                }
                if let Some(else_result) = &case.else_result {
                    self.resolve(ctx, else_result, refs)?;
                }
            }
            ExprKind::FunctionCall { name, args, .. } => {
                let mut inner = ctx;
                if is_aggregate(name) {
                    if let Aggregates::Refused(place) = ctx.aggregates {
                        return Err(misplaced_aggregate(name, place));
                    }
                    refs.aggregate.get_or_insert(name.span);
                    inner.aggregates = Aggregates::Refused("inside another aggregate");
                }
                if let CallArgs::Exprs(args) = args {
                    for arg in args {
                        self.resolve(inner, arg, refs)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// `g.key` of a group variable `g` outside its own element reads the list: refused.
    fn check_group_property(
        &self,
        ctx: Ctx<'_>,
        var: &Identifier,
        key: &Identifier,
    ) -> BResult<()> {
        if ctx.aliases.iter().any(|(name, _)| *name == var.name) {
            return Ok(());
        }
        let Some(Symbol::Binding(id)) = self.scope.get(&var.name) else {
            return Ok(());
        };
        let group = self.bindings.get(id.0).is_some_and(|b| b.group);
        if !group || ctx.element == Some(*id) {
            return Ok(());
        }
        Err(semantic(
            var.span,
            format!(
                "`{}` is the edge variable of a quantified edge: outside its edge pattern it is \
                 the list of the matched edges, so `{}.{}` has no single value",
                var.name, var.name, key.name
            ),
        )
        .alternative(format!(
            "test each edge inside the edge pattern, as in -[{} WHERE {}.{} > 1]->{{1,3}}",
            var.name, var.name, key.name
        ))
        .into())
    }

    fn variable(&self, ctx: Ctx<'_>, var: &Identifier, refs: &mut Refs) -> BResult<()> {
        if let Some((_, column)) = ctx.aliases.iter().find(|(name, _)| *name == var.name) {
            refs.aliases.push(*column);
            return Ok(());
        }
        match self.scope.get(&var.name) {
            Some(Symbol::Binding(id)) => {
                let binding = self.bindings.get(id.0);
                if let Some(binding) = binding
                    && binding.kind == BindingKind::Path
                    && ctx.inline_clause == Some(binding.clause)
                {
                    return Err(semantic(
                        var.span,
                        format!(
                            "the path variable `{}` is not complete inside its own MATCH \
                             pattern",
                            var.name
                        ),
                    )
                    .alternative(
                        "test the path in the WHERE of the MATCH clause, as in \
                         MATCH p = (a)-->(b) WHERE path_length(p) > 0 RETURN p",
                    )
                    .into());
                }
                refs.bindings.push(*id);
                if binding.and_then(|b| b.key.as_deref()) != Some(var.name.as_str()) {
                    refs.substitute = true;
                }
                Ok(())
            }
            Some(Symbol::Value(substitution)) => {
                refs.bindings.extend_from_slice(&substitution.bindings);
                refs.substitute = true;
                Ok(())
            }
            None => Err(self.undefined(ctx, var)),
        }
    }

    /// The error of a name not in scope.
    fn undefined(&self, ctx: Ctx<'_>, var: &Identifier) -> Box<GqlErrorBuilder> {
        let name = &var.name;
        if self.pending_let.iter().any(|n| n == name) {
            return semantic(
                var.span,
                format!(
                    "the variable `{name}` is defined by the same LET; a definition reads only \
                     the variables defined before the LET"
                ),
            )
            .alternative(format!(
                "define `{name}` in a LET of its own first, as in LET {name} = ... LET y = {name} + 1"
            ))
            .into();
        }
        if self.dropped.contains_key(name) {
            return semantic(
                var.span,
                format!("the variable `{name}` is not in scope: the WITH before does not carry it"),
            )
            .alternative(format!(
                "add `{name}` to the WITH items, as in WITH ..., {name}"
            ))
            .into();
        }
        let later = self
            .query
            .matches
            .get(self.clause.saturating_add(1)..)
            .unwrap_or(&[])
            .iter()
            .any(|m| declares(m, name));
        if later {
            return semantic(
                var.span,
                format!(
                    "the variable `{name}` is not defined here: it is bound by a later MATCH clause"
                ),
            )
            .alternative("move the condition to the WHERE of that later MATCH clause")
            .into();
        }
        let in_scope = self
            .scope
            .names()
            .chain(ctx.aliases.iter().map(|(name, _)| name.as_str()));
        let mut error = semantic(var.span, format!("the variable `{name}` is not defined"));
        if let Some(close) = suggest(name, in_scope) {
            error = error.suggestion(close);
        }
        Box::new(error)
    }

    /// `expr` with every `LET` / `WITH` name substituted and every alias of a binding replaced
    /// by the binding's key (aliases of `ORDER BY` columns are kept), with its shape. Recursion
    /// is bounded by the height of the written expression; each substitution is charged to the
    /// budget.
    fn substitute(&mut self, aliases: &[(String, usize)], expr: &Expr) -> BResult<(Expr, Shape)> {
        let mut children = Vec::new();
        let kind = match &expr.kind {
            ExprKind::Literal(_) | ExprKind::Parameter(_) => expr.kind.clone(),
            ExprKind::Variable(var) => return self.substitute_variable(aliases, var, expr),
            ExprKind::List(items) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    let (e, s) = self.substitute(aliases, item)?;
                    children.push((s, true));
                    out.push(e);
                }
                ExprKind::List(out)
            }
            ExprKind::Map(map) => {
                let mut entries = Vec::with_capacity(map.entries.len());
                for entry in &map.entries {
                    let (value, s) = self.substitute(aliases, &entry.value)?;
                    children.push((s, true));
                    entries.push(MapEntry {
                        key: entry.key.clone(),
                        value,
                    });
                }
                ExprKind::Map(MapLiteral {
                    entries,
                    span: map.span,
                })
            }
            ExprKind::Property { target, key } => {
                let (target, s) = self.substitute(aliases, target)?;
                children.push((s, false));
                ExprKind::Property {
                    target: Box::new(target),
                    key: key.clone(),
                }
            }
            ExprKind::Unary { op, operand } => {
                let (operand, s) = self.substitute(aliases, operand)?;
                children.push((s, true));
                ExprKind::Unary {
                    op: *op,
                    operand: Box::new(operand),
                }
            }
            ExprKind::IsNull { operand, negated } => {
                let (operand, s) = self.substitute(aliases, operand)?;
                children.push((s, false));
                ExprKind::IsNull {
                    operand: Box::new(operand),
                    negated: *negated,
                }
            }
            ExprKind::IsLabeled {
                operand,
                labels,
                negated,
            } => {
                let (operand, s) = self.substitute(aliases, operand)?;
                children.push((s, false));
                ExprKind::IsLabeled {
                    operand: Box::new(operand),
                    labels: labels.clone(),
                    negated: *negated,
                }
            }
            ExprKind::Binary { op, left, right } => {
                let (left, ls) = self.substitute(aliases, left)?;
                let (right, rs) = self.substitute(aliases, right)?;
                children.push((ls, false));
                children.push((rs, true));
                ExprKind::Binary {
                    op: *op,
                    left: Box::new(left),
                    right: Box::new(right),
                }
            }
            ExprKind::Case(case) => {
                let mut one = |this: &mut Self, e: &Expr| -> BResult<Expr> {
                    let (e, s) = this.substitute(aliases, e)?;
                    children.push((s, true));
                    Ok(e)
                };
                let operand = match &case.operand {
                    Some(operand) => Some(one(self, operand)?),
                    None => None,
                };
                let mut whens = Vec::with_capacity(case.whens.len());
                for arm in &case.whens {
                    whens.push(CaseWhen {
                        when: one(self, &arm.when)?,
                        then: one(self, &arm.then)?,
                    });
                }
                let else_result = match &case.else_result {
                    Some(e) => Some(one(self, e)?),
                    None => None,
                };
                ExprKind::Case(Box::new(CaseExpr {
                    operand,
                    whens,
                    else_result,
                }))
            }
            ExprKind::FunctionCall {
                name,
                distinct,
                args,
            } => {
                let args = match args {
                    CallArgs::Star(span) => CallArgs::Star(*span),
                    CallArgs::Exprs(args) => {
                        let mut out = Vec::with_capacity(args.len());
                        for arg in args {
                            let (e, s) = self.substitute(aliases, arg)?;
                            children.push((s, true));
                            out.push(e);
                        }
                        CallArgs::Exprs(out)
                    }
                };
                ExprKind::FunctionCall {
                    name: name.clone(),
                    distinct: *distinct,
                    args,
                }
            }
        };
        Ok((
            Expr {
                kind,
                span: expr.span,
            },
            combine(&children),
        ))
    }

    fn substitute_variable(
        &mut self,
        aliases: &[(String, usize)],
        var: &Identifier,
        expr: &Expr,
    ) -> BResult<(Expr, Shape)> {
        if aliases.iter().any(|(name, _)| *name == var.name) {
            return Ok((expr.clone(), Shape::LEAF));
        }
        match self.scope.get(&var.name) {
            Some(Symbol::Binding(id)) => {
                let key = self
                    .bindings
                    .get(id.0)
                    .and_then(|b| b.key.clone())
                    .unwrap_or_else(|| var.name.clone());
                let reference = Expr {
                    kind: ExprKind::Variable(Identifier {
                        name: key,
                        span: var.span,
                    }),
                    span: expr.span,
                };
                Ok((reference, Shape::LEAF))
            }
            Some(Symbol::Value(substitution)) => {
                let substitution = Rc::clone(substitution);
                self.budget = match self.budget.checked_sub(substitution.shape.nodes) {
                    Some(left) => left,
                    None => {
                        return Err(too_large(
                            var.span,
                            &format!(
                                "the query substitutes more than {MAX_SUBSTITUTED_NODES} \
                                 expression nodes"
                            ),
                        ));
                    }
                };
                Ok((substitution.expr.clone(), substitution.shape))
            }
            // Resolved before; unreachable for a resolved expression.
            None => Ok((expr.clone(), Shape::LEAF)),
        }
    }

    // ----- result -----

    fn projection(
        &mut self,
        star: Option<Span>,
        items: &[ReturnItem],
        clause: &'static str,
    ) -> BResult<Vec<Column>> {
        self.projection_named(star, items, clause)
            .map(|(columns, _)| columns)
    }

    /// The columns of `RETURN` / `WITH` with the identifier each name comes from.
    fn projection_named(
        &mut self,
        star: Option<Span>,
        items: &[ReturnItem],
        clause: &'static str,
    ) -> BResult<(Vec<Column>, Vec<Identifier>)> {
        let mut columns = Vec::new();
        let mut names = Vec::new();
        if let Some(star) = star {
            if self.scope.entries.is_empty() {
                return Err(semantic(
                    star,
                    format!(
                        "{clause} * needs at least one named variable, and this query has none"
                    ),
                )
                .alternative(format!(
                    "name an element of the pattern, as in MATCH (n) {clause} *, or list the \
                     columns: {clause} count AS c"
                ))
                .into());
            }
            for (name, symbol) in &self.scope.entries {
                let column = match symbol {
                    Symbol::Binding(id) => {
                        let binding = self.bindings.get(id.0);
                        let expr = match binding.and_then(|b| b.name.as_ref().zip(b.key.as_ref())) {
                            Some((first, key)) if key == &name.name && first.name == name.name => {
                                Expr {
                                    kind: ExprKind::Variable(first.clone()),
                                    span: first.span,
                                }
                            }
                            _ => Expr {
                                kind: ExprKind::Variable(Identifier {
                                    name: binding
                                        .and_then(|b| b.key.clone())
                                        .unwrap_or_else(|| name.name.clone()),
                                    span: name.span,
                                }),
                                span: name.span,
                            },
                        };
                        let written = expr.span;
                        Column {
                            name: name.name.clone(),
                            expr,
                            variable: Some(*id),
                            bindings: vec![*id],
                            aliased: false,
                            aggregate: false,
                            written,
                            span: star,
                        }
                    }
                    Symbol::Value(substitution) => Column {
                        name: name.name.clone(),
                        expr: substitution.expr.clone(),
                        variable: None,
                        bindings: substitution.bindings.clone(),
                        aliased: false,
                        aggregate: false,
                        written: name.span,
                        span: star,
                    },
                };
                columns.push(column);
                names.push(name.clone());
            }
        }
        for item in items {
            let resolved = self.expression(Ctx::items(), &item.expr)?;
            let variable = match &item.expr.kind {
                ExprKind::Variable(var) => match self.scope.get(&var.name) {
                    Some(Symbol::Binding(id)) => Some(*id),
                    _ => None,
                },
                _ => None,
            };
            let name = match &item.alias {
                Some(alias) => alias.clone(),
                None => {
                    if clause == "WITH" && !matches!(item.expr.kind, ExprKind::Variable(_)) {
                        let text = self.query.text(item.expr.span);
                        return Err(semantic(
                            item.expr.span,
                            "a WITH item that is not a variable needs a name",
                        )
                        .alternative(format!("name it with AS, as in WITH {text} AS value"))
                        .into());
                    }
                    Identifier {
                        name: self.query.text(item.expr.span).to_string(),
                        span: item.expr.span,
                    }
                }
            };
            if columns.iter().any(|c| c.name == name.name) {
                let at = item.alias.as_ref().map_or(item.expr.span, |a| a.span);
                let n = &name.name;
                return Err(semantic(
                    at,
                    format!("the column name `{n}` is used twice in {clause}"),
                )
                .alternative(format!(
                    "give one of the columns another name with AS, as in {clause} ... AS {n}2"
                ))
                .into());
            }
            columns.push(Column {
                name: name.name.clone(),
                expr: resolved.expr,
                variable,
                bindings: resolved.refs.bindings,
                aliased: item.alias.is_some(),
                aggregate: resolved.refs.aggregate.is_some(),
                written: item.expr.span,
                span: item.span,
            });
            names.push(name);
        }
        if columns.iter().any(|c| c.aggregate) {
            self.check_grouping(&columns, items, star.is_some(), clause)?;
        }
        for column in &columns {
            let ids = column.bindings.clone();
            self.mark_read(&ids);
        }
        Ok((columns, names))
    }

    /// Implicit grouping: an aggregate item reads outside its aggregates only constants,
    /// parameters, grouping-key variables and grouping-key expressions (module docs).
    fn check_grouping(
        &self,
        columns: &[Column],
        items: &[ReturnItem],
        star: bool,
        clause: &'static str,
    ) -> BResult<()> {
        let keys: Vec<&Column> = columns.iter().filter(|c| !c.aggregate).collect();
        let key_variables: Vec<BindingId> = keys.iter().filter_map(|c| c.variable).collect();
        let key_texts: Vec<&str> = keys
            .iter()
            .filter(|c| !matches!(c.expr.kind, ExprKind::Variable(_)) || c.variable.is_none())
            .map(|c| self.query.text(c.written))
            .filter(|t| !t.is_empty())
            .collect();
        let star_columns = if star { self.scope.entries.len() } else { 0 };
        for (item, column) in items.iter().zip(columns.get(star_columns..).unwrap_or(&[])) {
            if column.aggregate {
                self.grouped(&item.expr, &key_variables, &key_texts, clause)?;
            }
        }
        Ok(())
    }

    /// Checks one aggregate item (recursion bounded by the expression height).
    fn grouped(
        &self,
        expr: &Expr,
        key_variables: &[BindingId],
        key_texts: &[&str],
        clause: &'static str,
    ) -> BResult<()> {
        let text = self.query.text(expr.span);
        if key_texts.contains(&text) {
            return Ok(());
        }
        match &expr.kind {
            ExprKind::FunctionCall { name, .. } if is_aggregate(name) => Ok(()),
            ExprKind::Variable(var) => {
                let grouped = match self.scope.get(&var.name) {
                    Some(Symbol::Binding(id)) => key_variables.contains(id),
                    Some(Symbol::Value(substitution)) => substitution
                        .bindings
                        .iter()
                        .all(|id| key_variables.contains(id)),
                    None => true,
                };
                if grouped {
                    return Ok(());
                }
                let name = &var.name;
                Err(semantic(
                    var.span,
                    format!(
                        "`{name}` is read outside an aggregate in an item that aggregates, so it \
                         must be a grouping key of {clause}, and it is not one"
                    ),
                )
                .alternative(format!(
                    "add `{name}` to the {clause} items (it then becomes a grouping key), or read \
                     it inside an aggregate, as in collect({name})"
                ))
                .into())
            }
            _ => {
                let mut result = Ok(());
                for_each_child(expr, &mut |child| {
                    if result.is_ok() {
                        result = self.grouped(child, key_variables, key_texts, clause);
                    }
                });
                result
            }
        }
    }

    fn order_by(
        &mut self,
        order_by: Option<&OrderBy>,
        columns: &[Column],
        distinct: bool,
        clause: &'static str,
    ) -> BResult<Vec<SortKey>> {
        let Some(order_by) = order_by else {
            return Ok(Vec::new());
        };
        let aggregated = columns.iter().any(|c| c.aggregate);
        let aliases: Vec<(String, usize)> = columns
            .iter()
            .enumerate()
            .filter(|(_, c)| c.aliased)
            .map(|(i, c)| (c.name.clone(), i))
            .collect();
        let ctx = Ctx {
            aliases: &aliases,
            ..Ctx::items()
        };
        let mut keys = Vec::with_capacity(order_by.items.len());
        for item in &order_by.items {
            let resolved = self.expression(ctx, &item.expr)?;
            let mut refs = resolved.refs;
            let text = self.query.text(item.expr.span);
            let column = match (&item.expr.kind, refs.aliases.as_slice()) {
                (ExprKind::Variable(_), [alias]) => Some(*alias),
                _ => columns
                    .iter()
                    .position(|c| self.query.text(c.written) == text && !text.is_empty()),
            };
            if column.is_none()
                && let Some(span) = refs.aggregate
            {
                return Err(semantic(
                    span,
                    format!("ORDER BY can only use an aggregate that {clause} computes"),
                )
                .alternative(format!(
                    "compute it as a column and order by its name, as in {clause} ..., \
                     {text} AS key ORDER BY key"
                ))
                .into());
            }
            if (distinct || aggregated) && column.is_none() {
                // Read by name: each must be a returned variable (reads through an alias are).
                let dropped = refs
                    .bindings
                    .iter()
                    .find(|id| !columns.iter().any(|c| c.variable == Some(**id)));
                if let Some(id) = dropped {
                    let name = self
                        .bindings
                        .get(id.0)
                        .and_then(|b| b.name.as_ref())
                        .map_or("", |n| n.name.as_str());
                    let after = if distinct {
                        format!("{clause} DISTINCT")
                    } else {
                        "an aggregation".to_string()
                    };
                    let fix = if distinct {
                        "drop DISTINCT"
                    } else {
                        "group by it too"
                    };
                    return Err(semantic(
                        item.expr.span,
                        format!(
                            "after {after}, ORDER BY can only use the returned columns, and \
                             `{name}` is not one of them"
                        ),
                    )
                    .alternative(format!(
                        "return the sort key too ({clause}{} ..., {text}) and order by it, or \
                         {fix}",
                        if distinct { " DISTINCT" } else { "" }
                    ))
                    .into());
                }
            }
            for alias in &refs.aliases {
                if let Some(c) = columns.get(*alias) {
                    refs.bindings.extend_from_slice(&c.bindings);
                }
            }
            let refs = refs.finish();
            self.mark_read(&refs.bindings);
            keys.push(SortKey {
                expr: resolved.expr,
                direction: item.direction,
                column,
                bindings: refs.bindings,
                aliases: refs.aliases,
                span: item.span,
            });
        }
        Ok(keys)
    }

    fn count(&mut self, clause: Option<&CountClause>, keyword: &str) -> BResult<Option<Count>> {
        let Some(clause) = clause else {
            return Ok(None);
        };
        let value = &clause.value;
        let refused = |code: &'static str, what: &str| {
            Err(semantic(
                value.span,
                format!("{keyword} must be a non-negative integer or a parameter, not {what}"),
            )
            .gqlstatus(code)
            .alternative(format!("{keyword} 10 or {keyword} $n"))
            .into())
        };
        let count = match &value.kind {
            ExprKind::Literal(Literal::Integer(n)) => match u64::try_from(*n) {
                Ok(n) => Count::Literal(n),
                Err(_) => return refused("22G02", "a negative number"),
            },
            ExprKind::Parameter(name) => {
                self.parameters.push(ParameterRef {
                    name: name.clone(),
                    span: value.span,
                });
                Count::Parameter {
                    name: name.clone(),
                    span: value.span,
                }
            }
            ExprKind::Literal(Literal::Float(_)) => return refused("22G03", "a float"),
            ExprKind::Literal(_) => return refused("22G03", "this literal"),
            _ => return refused("22G03", "an expression"),
        };
        Ok(Some(count))
    }

    fn check_parameters(&self, supplied: &[&str]) -> BResult<()> {
        let missing = self
            .parameters
            .iter()
            .find(|p| !supplied.contains(&p.name.as_str()));
        let Some(missing) = missing else {
            return Ok(());
        };
        let mut error = semantic(
            missing.span,
            format!("the parameter `${}` has no value", missing.name),
        )
        .alternative(format!(
            "pass it in the parameters of gql(), as in g.gql({}, #{{{}: value}})",
            crate::value::literal::rhai_string(&self.query.source),
            crate::gql::lower::rhai_map_key(&missing.name)
        ));
        if let Some(close) = suggest(&missing.name, supplied.iter().copied()) {
            error = error.suggestion(format!("${close}"));
        }
        Err(Box::new(error))
    }
}

/// Where a pattern element is declared.
#[derive(Clone, Copy)]
struct Site {
    clause: usize,
    segment: usize,
    kind: BindingKind,
    /// The edge of a quantified edge pattern.
    group: bool,
}

fn kind_word(kind: BindingKind) -> &'static str {
    match kind {
        BindingKind::Node => "node",
        BindingKind::Edge => "edge",
        BindingKind::Path => "path",
        BindingKind::Value => "value",
    }
}

/// The error of a variable used as `here` but bound as `there`.
fn kind_conflict(var: &Identifier, here: BindingKind, there: BindingKind) -> Box<GqlErrorBuilder> {
    let article = |kind: BindingKind| match kind {
        BindingKind::Node => "a node",
        BindingKind::Edge => "an edge",
        BindingKind::Path => "a path",
        BindingKind::Value => "a value",
    };
    let name = &var.name;
    let message = match there {
        BindingKind::Node | BindingKind::Edge => format!(
            "the variable `{name}` is used as {} here but is already bound to {}; a variable is \
             either a node or an edge",
            article(here),
            article(there)
        ),
        BindingKind::Path => format!(
            "the variable `{name}` is used as {} here but is a path variable; a path variable \
             cannot name a node or an edge",
            article(here)
        ),
        BindingKind::Value => format!(
            "the variable `{name}` is used as {} here but holds a value computed by WITH, not a \
             node or an edge",
            article(here)
        ),
    };
    Box::new(semantic(var.span, message))
}

/// The error of an aggregate call where none may stand.
fn misplaced_aggregate(name: &Identifier, place: &str) -> Box<GqlErrorBuilder> {
    let f = &name.name;
    let alternative = if place == "inside another aggregate" {
        format!(
            "aggregate in a WITH first and aggregate its column in RETURN, as in \
             MATCH (p)-->(f) WITH p, {f}(f) AS n RETURN max(n)"
        )
    } else {
        format!(
            "aggregate in a WITH and filter its column, as in \
             MATCH (p)-->(f) WITH p, {f}(f) AS n WHERE n > 1 RETURN p"
        )
    };
    Box::new(
        semantic(
            name.span,
            format!("the aggregate function {f}() cannot be used {place}"),
        )
        .alternative(alternative),
    )
}

/// Whether the `MATCH` clause `m` declares `name` (a pattern element or path variable).
fn declares(m: &MatchClause, name: &str) -> bool {
    let named = |v: &Option<Identifier>| v.as_ref().is_some_and(|v| v.name == name);
    m.patterns.iter().any(|path| {
        named(&path.variable)
            || named(&path.start.variable)
            || path
                .steps
                .iter()
                .any(|s| named(&s.edge.variable) || named(&s.node.variable))
    })
}

/// The shape of a node over `children` (`true`: a child the evaluator nests on; `false`: a link
/// of its left spine).
fn combine(children: &[(Shape, bool)]) -> Shape {
    let mut shape = Shape::LEAF;
    for (child, nests) in children {
        shape.height = shape.height.max(child.height.saturating_add(1));
        shape.depth = shape
            .depth
            .max(child.depth.saturating_add(usize::from(*nests)));
        shape.nodes = shape.nodes.saturating_add(child.nodes);
    }
    shape
}

/// The shape of a written expression (recursion bounded by its height).
fn shape_of(expr: &Expr) -> Shape {
    let mut children = Vec::new();
    let nests = !matches!(
        expr.kind,
        ExprKind::Property { .. } | ExprKind::IsNull { .. } | ExprKind::IsLabeled { .. }
    );
    let mut first = true;
    for_each_child(expr, &mut |child| {
        // The left operand of a binary operator is a link of the spine, the right one nests.
        let binary_left = matches!(expr.kind, ExprKind::Binary { .. }) && first;
        first = false;
        children.push((shape_of(child), nests && !binary_left));
    });
    combine(&children)
}

/// The top-level `AND` conjuncts of `expr`, left to right (iterative: an `AND` chain may be as
/// tall as `MAX_HEIGHT`).
fn split_and(expr: &Expr) -> Vec<&Expr> {
    let mut out = Vec::new();
    let mut stack = vec![expr];
    while let Some(e) = stack.pop() {
        match &e.kind {
            ExprKind::Binary {
                op: super::ast::BinaryOp::And,
                left,
                right,
            } => {
                stack.push(right);
                stack.push(left);
            }
            _ => out.push(e),
        }
    }
    out
}

/// A key written twice in one map is an error.
fn check_map_keys(map: &MapLiteral) -> BResult<()> {
    for (i, entry) in map.entries.iter().enumerate() {
        let earlier = map.entries.get(..i).unwrap_or(&[]);
        if earlier.iter().any(|e| e.key.name == entry.key.name) {
            return Err(semantic(
                entry.key.span,
                format!("the key `{}` appears twice in this map", entry.key.name),
            )
            .into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
