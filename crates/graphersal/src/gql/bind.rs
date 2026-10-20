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
//!   `ORDER BY`, `OFFSET` / `SKIP` or `LIMIT`, decision C2) is a [`Segment::With`]; each `FOR`
//!   / `UNWIND` is a [`Segment::For`]. A stage 1a query is exactly one `Segment::Match` holding
//!   every clause.
//! - **`FOR` / `UNWIND` (stage 2a).** `FOR x IN list [WITH ORDINALITY i | WITH OFFSET i]` (Cypher
//!   `UNWIND list AS x`) is a cut: its variable and its ordinal are new bindings of kind
//!   [`BindingKind::Value`] standing for the columns `0` and `1` of the [`Segment::For`]
//!   ([`Binding::column`]), like the columns of a materializing `WITH`. A `FOR` variable written
//!   later as a node or edge variable (`FOR n IN nodes(p) MATCH (n)-->(m)`) takes that kind
//!   (design E3: the lowering continues the pattern from it, a value of another kind gives no
//!   row); the ordinal is always a number and never an element.
//! - **Unit row (design M1, E4).** A query may start with `FOR`, `LET`, `WITH`, `FILTER` or
//!   `RETURN` ([`BoundLinear::unit`]): it starts from one row that holds no binding, so its first
//!   expressions read only constants and parameters (`RETURN *` with no variable in scope is
//!   still an error).
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
//!   current element and does not set it. A condition inside a subquery body sets it only for
//!   the body's own locals: where it reads an import depends on where the body runs (the
//!   planner's `Labelling::subquery`), and the host expression reads the imports anyway. A
//!   repeated variable sets [`Binding::joined`]; whether
//!   the join needs a path label depends on the plan (continuing from the variable needs none,
//!   closing a cycle does), so the planner decides.
//! - **Columns.** In written order: the alias when given, else the expression text exactly as
//!   written (Neo4j's rule; the ISO naming rule of an unaliased column is still to be verified,
//!   so this is provisional). `RETURN *` / `WITH *` come first and expand to every named variable
//!   in scope in the order it entered the scope (design D10). A `WITH` item that is not a bare
//!   variable needs `AS` (Neo4j's rule). A column holding an aggregate call is
//!   [`Column::aggregate`]; the other columns are the grouping keys (implicit grouping).
//! - **`GROUP BY`** of `RETURN` (ISO GQL) changes nothing in the plan: each key must name a
//!   column without an aggregate (its name, or the text of its expression), every such column
//!   must be listed, and `GROUP BY ()` needs every column to aggregate (42002 otherwise). With
//!   keys and no aggregate, the result is one row per group: `DISTINCT`.
//! - **`ORDER BY`** keys may name a column alias (an alias shadows a variable of the same name,
//!   as in Neo4j) or read any variable in scope; a key whose text equals a column's expression
//!   text is that column. After `DISTINCT` or an aggregation a key may only read returned
//!   columns (Neo4j's rule), and an aggregate in a key must be a whole column.
//! - **`OFFSET` / `LIMIT`** are a non-negative integer literal or a parameter (Neo4j's rule); the
//!   lowering checks a parameter's value.
//! - **Parameters:** every `$name` reference with its span, in text order.
//! - **Subqueries (stage 2b, design M2):** `EXISTS { .. }`, `EXISTS ( .. )` and Cypher pattern
//!   predicates; see "Subqueries".
//! - **Composite queries and `CALL` (stage 2d):** [`BoundQuery::body`], see "Composite queries".
//!
//! # Scope
//!
//! The scope runs along the stages. A `MATCH` clause's inline maps, inline `WHERE` and `WHERE`
//! see the variables in scope before the clause and every variable of the clause (a path
//! variable only in the clause `WHERE`, once its pattern is complete). `LET` adds its names (its
//! definitions read the scope before the `LET`, so one definition cannot read another of the
//! same `LET`; redefining a name in scope is an error). `FOR` adds its variable and its ordinal
//! the same way (its list reads the scope before the `FOR`). `WITH` replaces the scope by its columns
//! (`WITH *` keeps it); a dropped name written again in a later pattern is a new binding (its
//! key is `#<id>`, decision C14). `RETURN` and its `ORDER BY` see the final scope.
//!
//! # Map projections and list comprehensions (stage 2c)
//!
//! A map projection `v {.k, .*, x, k: expr}` reads its target, its variable items `x` and its
//! entries like any expression; keys may repeat (design E12: the evaluator keeps the first place
//! and the later value). When a `LET` / `WITH` substitution changes a variable item, it becomes
//! the entry `x: <substituted>`, so its key stays the written name. A list comprehension
//! `[x IN list WHERE cond | expr]` resolves `list` in the scope around it and its `WHERE` and
//! `|` parts with `x` as a **local** (design M4): it reads no binding and is visible only in
//! those parts. A local may not shadow a name (E14, 42002): a variable or an `ORDER BY` alias
//! in scope, a local around it, or any name a binding of the query has or had (a substituted
//! expression names bindings by their keys, which a local of the same name would capture).
//! No aggregate stands in the `WHERE` / `|` parts (the list part may hold one). A local is not
//! readable inside a subquery in those parts yet (`NotYet`): its body is planned apart from the
//! item. Beside an aggregate, a projection's target, variable items and entries are checked like
//! any expression (only grouping keys outside the aggregates).
//!
//! # Pattern comprehensions
//!
//! A pattern comprehension `[p = (a)-->(b) WHERE cond | expr]` (design E13) is bound as a
//! subquery ([`SubqueryKind::Collect`], [`SubqueryForm::Comprehension`], its span the whole
//! expression, which the stored expression keeps as a reference): its pattern and `WHERE` are
//! one body clause, and its `|` part is resolved and substituted in the body's scope after the
//! clause ([`BoundSubquery::map`]), with no aggregate (42002, naming the list's `size()` and
//! `OPTIONAL MATCH` with an aggregate instead) and, like a body condition, without the
//! comprehension variables around it (`NotYet`). The bindings the `|` part reads that are older
//! than the body are imports; its locals read there are marked read. Everything else (scope,
//! imports, grouping, limits) follows "Subqueries".
//!
//! # Subqueries
//!
//! A subquery's body (its `MATCH` clauses, their inline filters and `WHERE`s) is bound where the
//! expression holding it is resolved, as a **nested scope**: every name in scope there is
//! visible inside; a name written inside that is in scope is that binding (a correlation, an
//! **import**: a node or edge variable continues it, a `LET` value is substituted), and a new name
//! is a **local** of the body ([`Binding::subquery`]), invisible after it (a later read is
//! 42002 naming the subquery; a later `MATCH` binding the same name makes a new binding).
//! Nested subqueries see the locals of the bodies around them. The pattern rules of a `MATCH`
//! clause hold in each body clause: edge uniqueness per clause (an edge bound outside may be
//! bound again inside, design E8), kinds, group and path variables (a path variable inside is a
//! local; one of a clause whose inline filter holds the subquery is not complete there).
//! Aggregates are refused in a body like in any `WHERE` or pattern.
//!
//! A **pattern predicate** (an alias of `EXISTS { pattern }`) may not introduce a named variable:
//! every name it writes must be in scope (42002, the alternative is the `EXISTS { MATCH .. }`
//! form).
//!
//! Each subquery is a [`BoundSubquery`] in [`BoundQuery::subqueries`] (text order) with its
//! imports (the older bindings its body reads in a pattern, a condition or a nested subquery).
//! The body clauses follow the query's own clauses in [`BoundQuery::matches`]; the body is one
//! segment of its own, numbered past [`BoundQuery::segments`] ([`BoundSubquery::segment`]), so
//! its conjuncts, its locals and its clauses plan like a [`Segment::Match`] with the imports
//! bound before it. The expression holding the subquery reads its imports: a conjunct's
//! bindings (so a conjunct importing only the element it filters is listed on that element), a
//! column's, a sort key's, a `FOR` list's; read flags follow from that as usual. The stored
//! expression keeps the written [`ExprKind::Subquery`] as a reference to its bound form
//! ([`BoundQuery::subquery_at`], by span); its AST body is not substituted and is never read
//! after binding. Beside an aggregate a subquery may import only grouping keys.
//!
//! The planner plans every body with its host (`planner.rs`, "Subqueries"); the lowering runs a
//! top-level `[NOT] EXISTS` conjunct as a `filter()` / `not()` child (stage 2b.T4) and a
//! subquery anywhere else in an expression as a child of the `gql:*` step evaluating it (the
//! hook, `runtime::SubqueryHost`, stage 2b.T5).
//!
//! # Composite queries (stage 2d)
//!
//! Every linear query is bound as a pipeline ([`BoundLinear`]: its segments, in
//! [`BoundQuery::segments`] with every other pipeline's, listed in
//! [`BoundLinear::pipeline`], and its result statement); [`BoundQuery`] dereferences to the
//! first, which is the whole query when it is linear (every query of the stages before, bound
//! exactly as before). Bindings, clauses, conjuncts and subqueries stay one table each for the
//! whole query, so a body reads an outer binding by its id. The `MATCH` clauses are numbered
//! in binding order over every pipeline, the clauses of `MATCH`-only subquery bodies after them.
//!
//! - **`UNION` (design E15).** Each operand of a block has a scope of its own (a name of one
//!   operand is unknown in the next, the same name a new binding). Every operand returns the
//!   same column names in any order; the columns of the later operands are reordered to the
//!   first operand's order before their `ORDER BY` binds, so a sort key's column index is the
//!   block's. `ORDER BY` / `OFFSET` / `LIMIT` belong to their operand. One kind per block:
//!   `UNION ALL` beside `UNION` / `UNION DISTINCT` is 42002 ([`BoundBlock::distinct`]).
//! - **`NEXT` (design E19).** Chained linear queries `q1 NEXT q2` are one pipeline: the `RETURN`
//!   of `q1` is bound exactly like a `WITH` of its items, `ORDER BY`, `OFFSET`, `LIMIT` (a
//!   projecting one a substitution, a materializing one a [`Segment::With`]; an item needs no
//!   `AS`, its name is its text; `GROUP BY` is checked as on a `RETURN`), `RETURN *` like
//!   `WITH *`. A block after a `UNION` block, or a `UNION` block after `NEXT`, starts each
//!   operand with a [`Segment::Rows`]: the records of the block before as new column bindings
//!   ([`Binding::column`]), a node or edge where every operand returns one in that column, else
//!   a value ([`BoundComposite::next`]).
//! - **`CALL` (design item 8).** `[OPTIONAL] CALL (a, b) { body }` imports exactly the named
//!   variables, `CALL (*)` every name in scope, `CALL { }` nothing unless an operand of its
//!   first block starts with a Cypher importing `WITH a, b` / `WITH *`
//!   ([`LinearQuery::importing`]); each operand of the body's first block starts with those
//!   names in scope (a `LET` / `WITH` name keeps its substitution), a block after `NEXT` with
//!   the rows before. Reading another outer name inside is 42002 naming the scope clause. The
//!   body is a [`BoundComposite`] of its own pipelines; [`BoundCall::imports`] are the outer
//!   bindings it reads (in a pattern, a condition, a column, a sort key, a `FOR` list or a
//!   nested body), marked read at the `CALL`. Its result columns (the first operand's of its
//!   last block) become new bindings of the [`Segment::Call`] ([`BoundCall::outputs`],
//!   [`Binding::column`]; a node or edge column keeps its kind, so a later pattern continues
//!   it), each a new name (42002 when in scope); the body's own names are invisible after it.
//!   The `CALL` is a cut; an `OPTIONAL CALL`'s columns may be `NULL` ([`Binding::optional`]).
//! - **Whole-query subquery bodies.** `EXISTS { .. }`, `COUNT { .. }` and `COLLECT { .. }` with
//!   clauses other than `MATCH` ([`BoundSubquery::query`]) bind as a composite whose operands
//!   start with every name in scope around the subquery (correlation as in a `MATCH`-only
//!   body: a written outer name is an import, a new name a local of the subquery). `COLLECT { }`
//!   ends in a `RETURN` of exactly one column. A `COUNT { }` of `MATCH` clauses binds like
//!   `EXISTS`.
//!
//! # Aggregates
//!
//! Aggregate calls stand in `RETURN` and `WITH` items only; a key of `ORDER BY` may hold one
//! only when the whole key is a column. Refused: an aggregate in `WHERE`, `FILTER`, `LET`, the
//! list of `FOR` / `UNWIND`, a
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
//! name, a `FOR` variable or ordinal already in scope (or both with one name), a `FOR` ordinal
//! used as a node or edge, a misplaced aggregate, a non-grouped read beside an aggregate, an
//! `ORDER BY` key reading a dropped variable after `DISTINCT` / aggregation, a duplicate key in a
//! map, a parameter that was not supplied, a subquery local read outside its body, a new
//! variable in a pattern predicate, a comprehension variable shadowing a name or read outside
//! its comprehension, an aggregate inside a comprehension's `WHERE` / `|` part (a pattern
//! comprehension's `|` part too); stage 2d: `UNION` operands with other column names (both
//! sets named), `UNION ALL` mixed with `UNION [DISTINCT]` in one block, a name the `RETURN`
//! before `NEXT` does not return, an outer name a `CALL` body does not import, a variable a
//! `CALL` scope clause (or importing `WITH`) does not know or names twice, a `CALL` column whose
//! name is in scope, a `CALL` body name read after it, a `COLLECT { }` without exactly one
//! returned column. `22G02` for a negative `OFFSET` / `LIMIT` literal,
//! `22G03` for one that is not an integer. Kind `NotYet` (`0A000`) for a condition inside a
//! quantified edge that reads another variable and a comprehension variable read inside a
//! subquery (a pattern comprehension's body and `|` part included).
//!
//! Stage 2d binds and plans composite queries, `CALL` and whole-query subquery bodies ahead of
//! their lowering; `UNION` and `NEXT` lower (2d.T4), `[OPTIONAL] CALL { }` (2d.T5), and
//! [`refuse_not_yet`] (which the lowering calls between planning and lowering) refuses the rest
//! with `NotYet`: the first in text order of a `COUNT { }`, `COLLECT { }` and a whole-query
//! `EXISTS` body.
//!
//! # Limits (kind `Limit`, `54001`)
//!
//! One `MATCH` clause holds at most [`MAX_MATCH_EDGES`] edge patterns over all its
//! comma-separated paths (a quantified edge counts once). Edge uniqueness checks every edge
//! against every earlier edge of the same clause, so the plan grows with the square of a clause's
//! edges; the limit keeps every query fast to prepare. It is checked for every clause before any
//! other work (the parser itself is linear in a long path). Separate `MATCH` clauses are not
//! limited together: edge uniqueness holds within one clause, and their cost adds up linearly.
//! The clauses of a subquery body are limited the same way, each on its own.
//!
//! One query holds at most [`MAX_SUBQUERIES`] subqueries, nested ones included: each becomes a
//! child traversal planned with its host.
//!
//! `CALL` bodies and whole-query subquery bodies nest at most [`MAX_BODY_DEPTH`] levels (the
//! parser allows [`MAX_DEPTH`]): binding recurses through a few kilobytes of stack per level in
//! an unoptimized build, and the deepest accepted query with the tallest expression inside
//! must fit a 2 MiB thread (the wasm32 worker's 1 MiB with its smaller frames).
//!
//! A substituted expression keeps the parser's bounds (height [`MAX_HEIGHT`], nesting
//! [`MAX_DEPTH`]), and one query substitutes at most [`MAX_SUBSTITUTED_NODES`] expression nodes
//! in all, so chained `LET`s that double an expression each cannot grow it exponentially.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use super::ast::{
    CallArgs, CallClause, CallScope, CaseExpr, CaseWhen, CompositeQuery, ComprehensionForm,
    CountClause, EdgeDirection, Expr, ExprKind, ForClause, GroupBy, Identifier, LabelExpr,
    LetClause, LinearQuery, ListComprehension, Literal, MapEntry, MapLiteral, MapProjection,
    MatchClause, NodePattern, OrderBy, OrdinalKind, ProjectionItem, Quantifier, Query, QueryBlock,
    ReduceAccumulator, ReturnItem, SortDirection, Stage, SubqueryExpr, SubqueryForm, SubqueryKind,
    UnionKind, WithClause,
};
use super::lexer::Span;
use super::parser::{FunctionKind, MAX_DEPTH, MAX_HEIGHT, close_to, lookup_function};
use crate::error::{GqlErrorBuilder, GqlErrorKind, GqlHelpTopic, TraverserError};

/// The index of a [`Binding`] in [`BoundQuery::bindings`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct BindingId(pub(crate) usize);

/// What a binding holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BindingKind {
    /// A node pattern `(..)`, a `WITH` column holding a node variable, or a `FOR` variable
    /// written later as a node.
    Node,
    /// An edge pattern `-[..]->`, a `WITH` column holding an edge variable, or a `FOR` variable
    /// written later as an edge.
    Edge,
    /// A path variable `p = ...`.
    Path,
    /// A column of a materializing `WITH` that is not a node or edge variable, a `FOR` variable
    /// (until written as a node or edge) or a `FOR` ordinal.
    Value,
}

/// The column of a materializing `WITH` or of a `FOR` a binding stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ColumnRef {
    /// The [`Segment::With`] or [`Segment::For`] (index into [`BoundQuery::segments`]).
    pub(crate) segment: usize,
    /// The column: an index into [`BoundWith::columns`]; of a `FOR`, [`FOR_ITEM`] or
    /// [`FOR_ORDINAL`].
    pub(crate) column: usize,
}

/// The column of a [`Segment::For`] holding the item.
pub(crate) const FOR_ITEM: usize = 0;

/// The column of a [`Segment::For`] holding the ordinal (`WITH ORDINALITY` / `WITH OFFSET`).
pub(crate) const FOR_ORDINAL: usize = 1;

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
    /// The `MATCH` clause of its first appearance (for a `WITH` / `FOR` column: the last clause
    /// before it, `0` when none came before).
    pub(crate) clause: usize,
    /// The segment of its first appearance (index into [`BoundQuery::segments`]).
    pub(crate) segment: usize,
    /// The edge variable of a quantified edge: a group variable (module docs).
    pub(crate) group: bool,
    /// The materializing `WITH` or `FOR` column it stands for.
    pub(crate) column: Option<ColumnRef>,
    /// The subquery whose body declares it (index into [`BoundQuery::subqueries`]): a local
    /// of that body, invisible outside it; `None` for a binding of the query itself.
    pub(crate) subquery: Option<usize>,
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

    /// Whether it was first bound by an `OPTIONAL MATCH` of `segments`, or returned by an
    /// `OPTIONAL CALL` (it may be NULL).
    #[cfg_attr(not(test), allow(dead_code, reason = "read by the tests"))]
    pub(crate) fn optional(&self, segments: &[Segment]) -> bool {
        match segments.get(self.segment) {
            Some(Segment::Optional(_)) => true,
            Some(Segment::Call(call)) => call.optional,
            Some(Segment::Match(_) | Segment::With(_) | Segment::For(_) | Segment::Rows(_))
            | None => false,
        }
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
    /// `FOR` / `UNWIND`: one row per item of its list (module docs). It has no conjuncts of its
    /// own.
    For(Box<BoundFor>),
    /// `[OPTIONAL] CALL { }` (stage 2d): a cut whose returned columns enter the scope as new
    /// bindings (module docs, "CALL"). It has no conjuncts of its own.
    Call(Box<BoundCall>),
    /// The first segment of a pipeline reading the records of the block before it (stage 2d,
    /// design E19: `NEXT` after a `UNION` block, or a `UNION` block after `NEXT`): its columns
    /// are new bindings (module docs, "Composite queries"). It has no conjuncts of its own.
    Rows(Box<BoundRows>),
}

/// A bound `[OPTIONAL] CALL (..) { body }` (stage 2d).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BoundCall {
    /// `OPTIONAL CALL`: a row without body results is kept, its returned names `NULL`.
    pub(crate) optional: bool,
    /// The body; its pipelines' segments are in [`BoundQuery::segments`] too.
    pub(crate) body: Box<BoundComposite>,
    /// The bindings declared before the `CALL` that the body reads (its imports, through the
    /// variable scope clause or a Cypher importing `WITH`), sorted, without duplicates.
    pub(crate) imports: Vec<BindingId>,
    /// The binding of each returned column, in the column order of the body's result (the
    /// first operand's order of its last block): new bindings standing for the columns of this
    /// segment ([`Binding::column`]).
    pub(crate) outputs: Vec<BindingId>,
    /// The clause.
    pub(crate) span: Span,
}

/// The records of the block before a pipeline, read as columns (stage 2d, [`Segment::Rows`]).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BoundRows {
    /// The binding of each column of the block before, in its column order.
    pub(crate) outputs: Vec<BindingId>,
    /// The `NEXT` keyword that passes the records on.
    pub(crate) span: Span,
}

/// A bound linear query: one pipeline and its result statement (a `UNION` operand, a block
/// after `NEXT`, or the whole query). Chained linear queries `q1 NEXT q2` are desugared into
/// one (design E19), `q1`'s `RETURN` becoming a `WITH`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BoundLinear {
    /// Its segments in pipeline order (indexes into [`BoundQuery::segments`]). For a query
    /// without `CALL`, `UNION`, `NEXT` and whole-query subquery bodies: every segment, in order.
    pub(crate) pipeline: Vec<usize>,
    /// The `RETURN` columns in order (of a `UNION` operand after the first: reordered to the
    /// first operand's order, design E15).
    pub(crate) columns: Vec<Column>,
    /// `RETURN DISTINCT` (or a `GROUP BY` without aggregate).
    pub(crate) distinct: bool,
    /// The `ORDER BY` keys in priority order.
    pub(crate) order_by: Vec<SortKey>,
    /// `OFFSET` / `SKIP`.
    pub(crate) offset: Option<Count>,
    /// `LIMIT`.
    pub(crate) limit: Option<Count>,
    /// The pipeline starts without `[OPTIONAL] MATCH` (with `FOR`, `LET`, `WITH`, `FILTER`,
    /// `CALL` or `RETURN`) and without a [`Segment::Rows`]: it starts from one unit row that
    /// holds no binding (design M1). In a `CALL` body or a whole-query subquery body that row is
    /// the outer row.
    pub(crate) unit: bool,
    /// From its first clause to the end of its result statement.
    pub(crate) span: Span,
}

/// A bound `UNION` block: operands with the same column names (design E15).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BoundBlock {
    /// The first operand (its column order is the block's).
    pub(crate) first: BoundLinear,
    /// The further operands in text order, each with the `UNION` before it.
    pub(crate) unions: Vec<BoundUnion>,
    /// The block.
    pub(crate) span: Span,
}

impl BoundBlock {
    /// Every operand in text order.
    pub(crate) fn operands(&self) -> impl Iterator<Item = &BoundLinear> {
        std::iter::once(&self.first).chain(self.unions.iter().map(|u| &u.operand))
    }

    /// Whether the block removes duplicate records (`UNION` / `UNION DISTINCT`; one kind per
    /// block, design E15); `false` for one operand and for `UNION ALL`.
    pub(crate) fn distinct(&self) -> bool {
        self.unions
            .first()
            .is_some_and(|u| u.kind == UnionKind::Distinct)
    }
}

/// One `UNION [ALL | DISTINCT]` operand of a [`BoundBlock`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BoundUnion {
    /// `UNION ALL` or `UNION [DISTINCT]` (the same for every operand of a block).
    pub(crate) kind: UnionKind,
    /// The keywords.
    pub(crate) keyword: Span,
    /// The operand.
    pub(crate) operand: BoundLinear,
}

/// A bound composite query (stage 2d): the top level, a `CALL` body, a whole-query subquery
/// body. Chained linear queries are already desugared (design E19), so every block in
/// [`Self::next`] starts each of its operands with a [`Segment::Rows`] reading the block before.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BoundComposite {
    /// The first block.
    pub(crate) first: BoundBlock,
    /// The further blocks, each after a `NEXT` (`first` or the block before is a `UNION`
    /// block, or this one is).
    pub(crate) next: Vec<BoundBlock>,
    /// The whole composite.
    pub(crate) span: Span,
}

impl BoundComposite {
    /// The block whose records are the result.
    pub(crate) fn last(&self) -> &BoundBlock {
        self.next.last().unwrap_or(&self.first)
    }

    /// Whether it is one linear query (no `UNION` block, no `NEXT` left after desugaring).
    #[cfg_attr(not(test), allow(dead_code, reason = "used by the tests"))]
    pub(crate) fn is_linear(&self) -> bool {
        self.next.is_empty() && self.first.unions.is_empty()
    }

    /// Every linear query in it, in text order (not those of nested bodies).
    pub(crate) fn linears(&self) -> impl Iterator<Item = &BoundLinear> {
        std::iter::once(&self.first)
            .chain(&self.next)
            .flat_map(BoundBlock::operands)
    }
}

/// A bound `FOR x IN list [WITH ORDINALITY i | WITH OFFSET i]` (Cypher `UNWIND list AS x`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BoundFor {
    /// The binding of the item (column [`FOR_ITEM`]).
    pub(crate) variable: BindingId,
    /// The list (`LET` / `WITH` names substituted), read on the row before the `FOR`.
    pub(crate) list: Expr,
    /// The bindings the list reads, sorted, without duplicates.
    pub(crate) bindings: Vec<BindingId>,
    /// `WITH ORDINALITY i` / `WITH OFFSET i`: the base and the binding of the position (column
    /// [`FOR_ORDINAL`]).
    pub(crate) ordinal: Option<(OrdinalKind, BindingId)>,
    /// Written as Cypher `UNWIND` (for messages).
    pub(crate) unwind: bool,
    /// The clause.
    pub(crate) span: Span,
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

/// A bound subquery: `EXISTS { .. }`, `EXISTS ( .. )`, a pattern predicate or a pattern
/// comprehension (module docs, "Subqueries").
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BoundSubquery {
    /// What it computes.
    pub(crate) kind: SubqueryKind,
    /// How it was written.
    pub(crate) form: SubqueryForm,
    /// The `MATCH` clauses of its body in order (indexes into [`BoundQuery::matches`], past the
    /// clauses of the query itself).
    pub(crate) clauses: Vec<usize>,
    /// The segment of its body: an index past [`BoundQuery::segments`] (the body is planned
    /// with its host, never as a stage of the pipeline). Its clauses, its local bindings and its
    /// conjuncts carry it, so the body plans like a [`Segment::Match`] of these clauses.
    pub(crate) segment: usize,
    /// The bindings declared outside it that its body reads (correlations), sorted, without
    /// duplicates: the variables written in its patterns, its conditions and its nested
    /// subqueries that are not its own locals. The expression holding it reads them too.
    pub(crate) imports: Vec<BindingId>,
    /// The subquery whose body holds it; `None` for one in an expression of the query itself.
    pub(crate) parent: Option<usize>,
    /// From `EXISTS` to the closing bracket; for a pattern predicate the pattern; for a pattern
    /// comprehension from `[` to `]`. The [`ExprKind::Subquery`] (or
    /// [`ExprKind::PatternComprehension`]) left in a stored expression names it by this span
    /// ([`BoundQuery::subquery_at`]).
    pub(crate) span: Span,
    /// The expression after `|` of a pattern comprehension ([`SubqueryKind::Collect`]),
    /// resolved and substituted in the body's scope: evaluated once per match at the end of the
    /// body; `None` for `EXISTS`.
    pub(crate) map: Option<Expr>,
    /// The bindings [`Self::map`] reads (the body's locals and imports), sorted.
    pub(crate) map_bindings: Vec<BindingId>,
    /// A whole-query body (stage 2d: `EXISTS { .. RETURN .. }`, `COUNT { .. }`, `COLLECT { ..
    /// RETURN x }`), its locals bound in its own pipelines ([`Self::clauses`] empty then);
    /// `None` for a body of `MATCH` clauses. `COLLECT { }` returns exactly one column.
    pub(crate) query: Option<Box<BoundComposite>>,
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
    /// The pipeline before `RETURN`, cut into segments (module docs). Stage 2d: the segments of
    /// every pipeline (each [`BoundLinear::pipeline`] lists its own); for a query without
    /// `CALL`, `UNION`, `NEXT` and whole-query subquery bodies exactly its one pipeline.
    pub(crate) segments: Vec<Segment>,
    /// Every conjunct of every filter, in text order per clause.
    pub(crate) conjuncts: Vec<Conjunct>,
    /// Indexes into [`Self::conjuncts`] of those not listed on a binding (module docs).
    pub(crate) cross: Vec<usize>,
    /// Every parameter reference in text order.
    pub(crate) parameters: Vec<ParameterRef>,
    /// Every subquery in text order (a subquery before the ones nested in its body); module
    /// docs, "Subqueries".
    pub(crate) subqueries: Vec<BoundSubquery>,
    /// The statements (stage 2d): one linear query, or `UNION` blocks chained by `NEXT`. The
    /// query dereferences to its first linear query ([`BoundLinear`]: `columns`, `distinct`,
    /// `order_by`, `offset`, `limit`, `unit`), the whole query when [`BoundComposite::is_linear`].
    pub(crate) body: BoundComposite,
}

impl std::ops::Deref for BoundQuery {
    type Target = BoundLinear;

    /// The first linear query (the whole query when it is linear).
    fn deref(&self) -> &BoundLinear {
        &self.body.first.first
    }
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

    /// The subquery written at `span` (the span of an [`ExprKind::Subquery`] in a stored
    /// expression): its index into [`Self::subqueries`].
    pub(crate) fn subquery_at(&self, span: Span) -> Option<usize> {
        self.subqueries.iter().position(|s| s.span == span)
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

/// The most subqueries (`EXISTS`, pattern predicates, pattern comprehensions) one query may hold,
/// nested ones included
/// (module docs, "Limits"). Each is planned and run as a child traversal of its own; 64 lie far
/// beyond any written query and keep preparing a query fast.
pub(crate) const MAX_SUBQUERIES: usize = 64;

/// The most `CALL` bodies and whole-query subquery bodies (`EXISTS` / `COUNT` / `COLLECT` with
/// clauses other than `MATCH`) one may nest in another (module docs, "Limits"). Binding (and
/// every later stage) recurses once per level through frames of a few kilobytes in an
/// unoptimized build; 16 levels lie far beyond any written query and keep the deepest query
/// with the tallest expression inside within a 2 MiB thread stack (the wasm32 worker has 1
/// MiB, with smaller frames).
pub(crate) const MAX_BODY_DEPTH: usize = 16;

/// The provisional segment of subquery `k`'s body while binding is `SUBQUERY_SEGMENTS + k`; the
/// binder renumbers it past the query's own segments at the end ([`BoundSubquery::segment`]).
const SUBQUERY_SEGMENTS: usize = usize::MAX / 2;

/// The provisional id of the `k`-th clause of the `MATCH`-only subquery bodies while binding is
/// `SUBQUERY_CLAUSES + k`; the binder renumbers it past the clauses of every pipeline at the
/// end (for a query without `CALL`, `UNION`, `NEXT` and whole-query bodies: past the query's own
/// clauses, as in stage 2b).
const SUBQUERY_CLAUSES: usize = usize::MAX / 2;

/// What binding a subquery body yields: its `MATCH` clauses (a body of `MATCH` clauses), the
/// resolved `|` part of a pattern comprehension, a whole-query body with the names at the end
/// of its pipelines.
type SubqueryBody = (
    Vec<usize>,
    Option<Resolved>,
    Option<(Box<BoundComposite>, Vec<String>)>,
);

/// The error of a `COLLECT { }` whose body does not end in a `RETURN` of exactly one column.
fn collect_needs_one_column(span: Span) -> Box<GqlErrorBuilder> {
    Box::new(
        semantic(
            span,
            "COLLECT { } needs a body that ends in a RETURN of exactly one column",
        )
        .alternative(
            "return the one value to collect, as in MATCH (p:person) RETURN COLLECT { MATCH \
             (p)-[:created]->(s) RETURN s.name } AS names",
        )
        .topic(GqlHelpTopic::Subquery),
    )
}

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

/// The error of a query with more than [`MAX_SUBQUERIES`] subqueries, at the first one past the
/// limit.
fn too_many_subqueries(span: Span) -> Box<GqlErrorBuilder> {
    Box::new(
        GqlErrorBuilder::new(
            GqlErrorKind::Limit,
            span.start..span.end,
            format!(
                "the query holds more than {MAX_SUBQUERIES} subqueries (EXISTS and pattern \
                 predicates), pattern comprehensions included"
            ),
        )
        .alternative(
            "join the patterns into fewer subqueries (one EXISTS { MATCH .. } may hold several \
             MATCH clauses and patterns), or add the patterns to the MATCH clause of the query; \
             a pattern comprehension is a subquery too: collect the matches of several patterns \
             with OPTIONAL MATCH and collect_list() instead",
        )
        .topic(GqlHelpTopic::Subquery),
    )
}

/// Refuses (kind `NotYet`, `0A000`) what stage 2d binds and plans but does not lower yet (the
/// lowering follows in task 2d.T5c; `UNION` and `NEXT` lower since 2d.T4, `[OPTIONAL] CALL { }`
/// since 2d.T5): the first in text order of a `COUNT { }`, a `COLLECT { }` and a whole-query
/// `EXISTS { }` body. Called on a bound query
/// after it is planned (so every bound query goes through the planner) and before it is
/// lowered.
pub(crate) fn refuse_not_yet(query: &Query, bound: &BoundQuery) -> Result<(), TraverserError> {
    not_yet_in(bound).map_err(|e| e.into_error(&query.source))
}

fn not_yet_in(bound: &BoundQuery) -> BResult<()> {
    let subqueries = bound
        .subqueries
        .iter()
        .filter_map(|s| subquery_not_yet(s).map(|error| (s.span, error)));
    match subqueries.min_by_key(|(span, _)| span.start) {
        Some((_, error)) => Err(error),
        None => Ok(()),
    }
}

/// The "not yet" error of the stage 2d subquery forms that bind before they run: `COUNT { }`,
/// `COLLECT { }` and a whole-query body (`EXISTS { MATCH .. RETURN .. }`); `None` for the
/// others.
fn subquery_not_yet(subquery: &BoundSubquery) -> Option<Box<GqlErrorBuilder>> {
    const WHOLE_QUERY: &str = "write the body as MATCH clauses with a WHERE, as in \
         MATCH (p:person) WHERE EXISTS { MATCH (p)-[:created]->(s) WHERE s.lang = 'java' } \
         RETURN p.name";
    let (what, alternative) = match subquery.kind {
        SubqueryKind::Count => (
            "a COUNT { } subquery",
            "count the matches of the pattern with OPTIONAL MATCH and count(), as in \
             MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) RETURN p.name, \
             count(s). To test only whether the pattern matches (COUNT { .. } > 0), use \
             EXISTS, as in MATCH (p:person) WHERE EXISTS { (p)-[:created]->() } RETURN \
             p.name",
        ),
        SubqueryKind::Collect if subquery.form != SubqueryForm::Comprehension => (
            "a COLLECT { } subquery",
            "collect the matches of the pattern with OPTIONAL MATCH and collect_list(), \
             as in MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) \
             RETURN p.name, collect_list(s.name)",
        ),
        SubqueryKind::Exists | SubqueryKind::Collect if subquery.query.is_some() => (
            "an EXISTS { } body that is a whole query (clauses other than MATCH)",
            WHOLE_QUERY,
        ),
        SubqueryKind::Exists | SubqueryKind::Collect => return None,
    };
    Some(not_yet(subquery.span, what, alternative))
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
        ExprKind::IsEndpoint { operand, edge, .. } => {
            f(operand);
            f(edge);
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
        ExprKind::FunctionCall { args, .. } => args.iter().for_each(f),
        ExprKind::MapProjection(projection) => {
            f(&projection.target);
            for item in &projection.items {
                if let ProjectionItem::Entry(entry) = item {
                    f(&entry.value);
                }
            }
        }
        ExprKind::ListComprehension(comprehension) => {
            if let Some(init) = comprehension.init() {
                f(init);
            }
            f(&comprehension.list);
            if let Some(filter) = &comprehension.filter {
                f(filter);
            }
            if let Some(map) = &comprehension.map {
                f(map);
            }
        }
        // The body of a subquery (a pattern comprehension too) is a nested scope, bound on its
        // own, not a child expression of this scope.
        ExprKind::Subquery(_) | ExprKind::PatternComprehension(_) => {}
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

/// Why a name left the scope (for the message of a later read).
#[derive(Debug, Clone, Copy)]
enum Dropper {
    /// A `WITH` that does not carry it.
    With,
    /// A `RETURN` before `NEXT` that does not return it (design E19).
    Next,
}

/// Where a name that is not in scope any more was bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hidden {
    /// Inside an `EXISTS` body or a pattern predicate.
    Subquery,
    /// Inside a pattern comprehension.
    Comprehension,
    /// Inside a `CALL` body that does not return it.
    Call,
}

/// What the operands of a composite's first block start from.
#[derive(Clone, Copy)]
enum Entry<'a> {
    /// The query itself: nothing in scope.
    Top,
    /// A `CALL` body: the names of `outer` its variable scope clause (or a Cypher importing
    /// `WITH`) imports.
    Call {
        scope: &'a CallScope,
        outer: &'a Names,
    },
    /// A whole-query subquery body: every name in scope around it (`outer`).
    Subquery { outer: &'a Names },
}

/// The records of a block read by the next one (design E19, [`Segment::Rows`]).
struct RowsInput {
    /// Each column's name (spanned by its item in the block's first operand) and kind.
    columns: Vec<(Identifier, BindingKind)>,
    /// The `NEXT` keyword.
    keyword: Span,
}

/// How a pipeline starts.
enum Start<'a> {
    /// With these names in scope (none at the top, the imports of a `CALL` body, every outer
    /// name in a whole-query subquery body).
    Names(Names),
    /// With the records of the block before as columns.
    Rows(&'a RowsInput),
}

/// The binder state one pipeline owns (saved around a nested pipeline).
struct PipelineState {
    scope: Names,
    dropped: HashMap<String, Dropper>,
    hidden: HashMap<String, Hidden>,
    open: Option<usize>,
    pipeline: Vec<usize>,
    bound_clauses: usize,
    later_clauses: Vec<(usize, String)>,
    later_in_body: Vec<String>,
}

/// Where the binder stood when a nested body started: everything past these counts was
/// bound inside it.
#[derive(Clone, Copy)]
struct Marks {
    binding: usize,
    conjunct: usize,
    segment: usize,
    own: usize,
    sub: usize,
    subquery: usize,
}

struct Binder<'q> {
    query: &'q Query,
    bindings: Vec<Binding>,
    /// The names in scope now.
    scope: Names,
    /// Every name and key given to a binding so far (keys stay unique, C14).
    taken: HashSet<String>,
    /// Names a `WITH` (or a `RETURN` before `NEXT`) dropped from the scope.
    dropped: HashMap<String, Dropper>,
    /// The names of the `LET` being bound (its definitions cannot read them).
    pending_let: Vec<String>,
    conjuncts: Vec<Conjunct>,
    parameters: Vec<ParameterRef>,
    segments: Vec<Segment>,
    /// The segments of the pipeline being bound, in order.
    pipeline: Vec<usize>,
    /// The `Segment::Match` later plain clauses and filters join, while no cut came after it.
    open: Option<usize>,
    /// The last `MATCH` clause bound (`0` before the first).
    clause: usize,
    /// How many `MATCH` clauses of the linear query being bound are bound.
    bound_clauses: usize,
    /// The names each `MATCH` clause of the linear query being bound declares, with the
    /// clause's index in it (for the "bound by a later MATCH clause" message).
    later_clauses: Vec<(usize, String)>,
    /// The `MATCH` clauses of every pipeline (the query's own, of `UNION` operands, `NEXT`
    /// blocks, `CALL` bodies and whole-query subquery bodies), by clause id; `None` while a
    /// clause is bound.
    own: Vec<Option<BoundMatch>>,
    /// Expression nodes left to substitute.
    budget: usize,
    /// Every subquery bound or being bound, in text order (the placeholder of one being bound
    /// gets its clauses and imports when its body is done).
    subqueries: Vec<BoundSubquery>,
    /// The subquery whose body is being bound (its new bindings are its locals).
    subquery: Option<usize>,
    /// The `MATCH` clauses of the subquery bodies, in order; `None` while a clause is bound.
    sub_matches: Vec<Option<BoundMatch>>,
    /// The `MATCH` clauses whose path variables are not complete where a subquery body is
    /// bound (it stands in an inline filter of such a clause).
    incomplete_paths: Vec<usize>,
    /// Names bound only inside a subquery body or a `CALL` body that is done (for the
    /// message).
    hidden: HashMap<String, Hidden>,
    /// Names the later `MATCH` clauses of the subquery bodies being bound declare (innermost
    /// last), for the "bound by a later MATCH clause" message inside a body.
    later_in_body: Vec<String>,
    /// The variables of the list comprehensions whose `WHERE` / `|` part is being resolved,
    /// innermost last (design M4: locals of one expression, never bindings).
    locals: Vec<Identifier>,
    /// The comprehension variables around a subquery body being bound: not readable inside it
    /// (not yet), named by the error.
    shelved_locals: Vec<Identifier>,
    /// The scope around each `CALL` body being bound, innermost last (for the message of an
    /// outer name the body does not import).
    calls: Vec<Rc<Names>>,
    /// How many `CALL` / whole-query subquery bodies enclose what is being bound.
    body_depth: usize,
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
            pipeline: Vec::new(),
            open: None,
            clause: 0,
            bound_clauses: 0,
            later_clauses: Vec::new(),
            own: Vec::new(),
            budget: MAX_SUBSTITUTED_NODES,
            subqueries: Vec::new(),
            subquery: None,
            sub_matches: Vec::new(),
            incomplete_paths: Vec::new(),
            hidden: HashMap::new(),
            later_in_body: Vec::new(),
            locals: Vec::new(),
            shelved_locals: Vec::new(),
            calls: Vec::new(),
            body_depth: 0,
        }
    }

    fn run(mut self, supplied: Option<&[&str]>) -> BResult<BoundQuery> {
        let query = self.query;
        let (body, _) = self.composite(&query.body, Entry::Top)?;
        self.parameters.sort_by_key(|p| p.span.start);
        if let Some(supplied) = supplied {
            self.check_parameters(supplied)?;
        }
        let matches = self.finish_clauses()?;
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
            parameters: self.parameters,
            subqueries: self.subqueries,
            body: *body,
        })
    }

    /// The clauses of every pipeline, then those of the subquery bodies (renumbered past them);
    /// the provisional segments of the bodies renumbered past every pipeline's segments.
    fn finish_clauses(&mut self) -> BResult<Vec<BoundMatch>> {
        let internal = || semantic(self.query.span, "internal error: a clause left unbound");
        let mut matches = Vec::with_capacity(self.own.len() + self.sub_matches.len());
        for m in std::mem::take(&mut self.own) {
            matches.push(m.ok_or_else(internal)?);
        }
        let own = matches.len();
        let outer = self.segments.len();
        let segment = |segment: &mut usize| {
            if *segment >= SUBQUERY_SEGMENTS {
                *segment = *segment - SUBQUERY_SEGMENTS + outer;
            }
        };
        let clause = |clause: &mut usize| {
            if *clause >= SUBQUERY_CLAUSES {
                *clause = *clause - SUBQUERY_CLAUSES + own;
            }
        };
        for m in std::mem::take(&mut self.sub_matches) {
            let mut m = m.ok_or_else(internal)?;
            segment(&mut m.segment);
            matches.push(m);
        }
        for b in &mut self.bindings {
            segment(&mut b.segment);
            clause(&mut b.clause);
        }
        for c in &mut self.conjuncts {
            segment(&mut c.segment);
            clause(&mut c.clause);
        }
        for s in &mut self.subqueries {
            segment(&mut s.segment);
            s.clauses.iter_mut().for_each(clause);
        }
        Ok(matches)
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

    /// Appends `segment` to the segments and to the pipeline being bound; returns its index.
    fn push_segment(&mut self, segment: Segment) -> usize {
        let index = self.segments.len();
        self.segments.push(segment);
        self.pipeline.push(index);
        index
    }

    /// Appends `segment`, which the caller numbered `expected` (its column bindings refer to
    /// it); an internal error when something came between.
    fn push_numbered(&mut self, segment: Segment, expected: usize, span: Span) -> BResult<()> {
        if self.push_segment(segment) == expected {
            Ok(())
        } else {
            Err(Box::new(semantic(
                span,
                "internal error: a segment out of order in the binder",
            )))
        }
    }

    /// The open `Segment::Match`, opened when a cut came last.
    fn open_segment(&mut self) -> usize {
        match self.open {
            Some(segment) => segment,
            None => {
                let segment = self.push_segment(Segment::Match(Vec::new()));
                self.open = Some(segment);
                segment
            }
        }
    }

    fn marks(&self) -> Marks {
        Marks {
            binding: self.bindings.len(),
            conjunct: self.conjuncts.len(),
            segment: self.segments.len(),
            own: self.own.len(),
            sub: self.sub_matches.len(),
            subquery: self.subqueries.len(),
        }
    }

    // ----- composite queries (stage 2d) -----

    /// Binds `composite` (module docs, "Composite queries"); returns it with the names in scope
    /// at the end of each of its pipelines (for the message of a later read outside it).
    /// Recursion: one level per nested `CALL` / whole-query body, which the parser bounds by
    /// `MAX_DEPTH`.
    #[inline(never)]
    fn composite(
        &mut self,
        composite: &CompositeQuery,
        entry: Entry<'_>,
    ) -> BResult<(Box<BoundComposite>, Vec<String>)> {
        let mut bound: Vec<BoundBlock> = Vec::with_capacity(composite.next.len() + 1);
        let mut names = Vec::new();
        let mut at = 0;
        while at <= composite.next.len() {
            at = self.composite_block(composite, at, entry, &mut bound, &mut names)?;
        }
        let mut bound = bound.into_iter();
        let first = bound.next().ok_or_else(|| {
            Box::new(semantic(
                composite.span,
                "internal error: a query without a block",
            ))
        })?;
        let composite = Box::new(BoundComposite {
            first,
            next: bound.collect(),
            span: composite.span,
        });
        Ok((composite, names))
    }

    /// Binds the block of `composite` at `at` (`0`: the first, `k`: the one after the `k`-th
    /// `NEXT`) onto `bound`, a `UNION` block or the chain of linear queries starting there
    /// (design E19); returns the index after it.
    #[inline(never)]
    fn composite_block(
        &mut self,
        composite: &CompositeQuery,
        at: usize,
        entry: Entry<'_>,
        bound: &mut Vec<BoundBlock>,
        names: &mut Vec<String>,
    ) -> BResult<usize> {
        let block_at = |i: usize| match i.checked_sub(1) {
            None => Some(&composite.first),
            Some(k) => composite.next.get(k).map(|n| &n.block),
        };
        let Some(block) = block_at(at) else {
            return Ok(at.saturating_add(1));
        };
        let rows = match (bound.last(), at.checked_sub(1)) {
            (Some(before), Some(k)) => composite
                .next
                .get(k)
                .map(|n| Box::new(self.rows_input(before, n.keyword))),
            _ => None,
        };
        if !block.unions.is_empty() {
            let union = self.union_block(block, entry, rows.as_deref(), names)?;
            bound.push(union);
            return Ok(at.saturating_add(1));
        }
        // Chained linear queries are one pipeline (design E19).
        let mut chain = vec![&block.first];
        let mut span = block.span;
        let mut next = at.saturating_add(1);
        while let Some(more) = block_at(next).filter(|b| b.unions.is_empty()) {
            chain.push(&more.first);
            span = span.to(more.span);
            next = next.saturating_add(1);
        }
        let start = self.start(entry, rows.as_deref(), &block.first)?;
        let (first, end) = self.linear_chain(&chain, start, None)?;
        names.extend(end);
        bound.push(BoundBlock {
            first: *first,
            unions: Vec::new(),
            span,
        });
        Ok(next)
    }

    /// A `UNION` block (design E15): one kind of `UNION`, the same column names in every
    /// operand, reordered to the first operand's order.
    fn union_block(
        &mut self,
        block: &QueryBlock,
        entry: Entry<'_>,
        rows: Option<&RowsInput>,
        names: &mut Vec<String>,
    ) -> BResult<BoundBlock> {
        self.check_union_kinds(block)?;
        let start = self.start(entry, rows, &block.first)?;
        let (first, end) = self.linear_chain(&[&block.first], start, None)?;
        let first = *first;
        names.extend(end);
        let order: Vec<String> = first.columns.iter().map(|c| c.name.clone()).collect();
        let mut unions = Vec::with_capacity(block.unions.len());
        for operand in &block.unions {
            let start = self.start(entry, rows, &operand.query)?;
            let (bound, end) = self.linear_chain(&[&operand.query], start, Some(&order))?;
            names.extend(end);
            unions.push(BoundUnion {
                kind: operand.kind,
                keyword: operand.keyword,
                operand: *bound,
            });
        }
        Ok(BoundBlock {
            first,
            unions,
            span: block.span,
        })
    }

    /// One kind of `UNION` per block (design E15, 42002).
    #[inline(never)]
    fn check_union_kinds(&self, block: &QueryBlock) -> BResult<()> {
        if let Some(kind) = block.unions.first().map(|u| u.kind)
            && let Some(other) = block.unions.iter().find(|u| u.kind != kind)
        {
            let written = self.query.text(other.keyword);
            return Err(semantic(
                other.keyword,
                format!(
                    "{written} is mixed with {} in one query; every UNION between the same \
                     results keeps duplicates (UNION ALL) or removes them (UNION, UNION \
                     DISTINCT)",
                    union_word(kind)
                ),
            )
            .alternative(
                "write the same kind between every operand, or combine the parts with NEXT, \
                 as in RETURN 1 AS x UNION ALL RETURN 1 AS x NEXT RETURN x UNION RETURN 2 AS x",
            )
            .topic(GqlHelpTopic::Names)
            .into());
        }
        Ok(())
    }

    /// The records of `block` as the input of the block after `keyword` (design E19): each
    /// column's name and kind (a node or edge when every operand returns one there).
    fn rows_input(&self, block: &BoundBlock, keyword: Span) -> RowsInput {
        let columns = block
            .first
            .columns
            .iter()
            .enumerate()
            .map(|(i, column)| {
                let kind = self.block_column_kind(block, i);
                let name = Identifier {
                    name: column.name.clone(),
                    span: column.span,
                };
                (name, kind)
            })
            .collect();
        RowsInput { columns, keyword }
    }

    /// The kind of column `i` of `block`: a node or edge when every operand returns a node
    /// (edge) variable there, else a value.
    fn block_column_kind(&self, block: &BoundBlock, i: usize) -> BindingKind {
        let mut kinds = block.operands().map(|operand| {
            operand
                .columns
                .get(i)
                .map_or(BindingKind::Value, |c| self.column_kind(c))
        });
        let first = kinds.next().unwrap_or(BindingKind::Value);
        if kinds.all(|kind| kind == first) {
            first
        } else {
            BindingKind::Value
        }
    }

    /// The kind of the binding a column becomes: the element's kind for a node or edge
    /// variable (not a group variable), else a value.
    fn column_kind(&self, column: &Column) -> BindingKind {
        column
            .variable
            .and_then(|id| self.bindings.get(id.0))
            .filter(|b| !b.group)
            .map_or(BindingKind::Value, |b| match b.kind {
                BindingKind::Node => BindingKind::Node,
                BindingKind::Edge => BindingKind::Edge,
                BindingKind::Path | BindingKind::Value => BindingKind::Value,
            })
    }

    /// How an operand of a composite's block starts: the rows of the block before, or the
    /// names `entry` gives it.
    fn start<'r>(
        &self,
        entry: Entry<'_>,
        rows: Option<&'r RowsInput>,
        operand: &LinearQuery,
    ) -> BResult<Start<'r>> {
        if let Some(rows) = rows {
            return Ok(Start::Rows(rows));
        }
        let names = match entry {
            Entry::Top => Names::default(),
            Entry::Subquery { outer } => outer.clone(),
            Entry::Call { scope, outer } => match scope {
                CallScope::Variables { variables, .. } => imported(outer, variables),
                CallScope::All(_) => outer.clone(),
                CallScope::Implicit => match &operand.importing {
                    None => Names::default(),
                    Some(importing) if importing.star.is_some() => outer.clone(),
                    Some(importing) => {
                        self.check_imports(&importing.variables, "WITH")?;
                        imported(outer, &importing.variables)
                    }
                },
            },
        };
        Ok(Start::Names(names))
    }

    /// The variables a `CALL` imports must be in scope, each once (42002).
    fn check_imports(&self, variables: &[Identifier], clause: &str) -> BResult<()> {
        for (i, var) in variables.iter().enumerate() {
            if self.scope.get(&var.name).is_none() {
                return Err(self.undefined(Ctx::refusing("in CALL"), var));
            }
            if variables
                .get(..i)
                .unwrap_or(&[])
                .iter()
                .any(|v| v.name == var.name)
            {
                return Err(semantic(
                    var.span,
                    format!(
                        "the variable `{}` is imported twice by this {clause}",
                        var.name
                    ),
                )
                .alternative(format!("name `{}` once", var.name))
                .topic(GqlHelpTopic::Scope)
                .into());
            }
        }
        Ok(())
    }

    /// Binds chained linear queries `q1 NEXT q2 ..` as one pipeline (design E19: each `RETURN`
    /// before `NEXT` is bound as a `WITH`), from `start`; `align` holds the column names of a
    /// `UNION`'s first operand (design E15). Returns it with the names in scope at its end.
    #[inline(never)]
    fn linear_chain(
        &mut self,
        chain: &[&LinearQuery],
        start: Start<'_>,
        align: Option<&[String]>,
    ) -> BResult<(Box<BoundLinear>, Vec<String>)> {
        let saved = Box::new(PipelineState {
            scope: std::mem::take(&mut self.scope),
            dropped: std::mem::take(&mut self.dropped),
            hidden: std::mem::take(&mut self.hidden),
            open: self.open.take(),
            pipeline: std::mem::take(&mut self.pipeline),
            bound_clauses: self.bound_clauses,
            later_clauses: std::mem::take(&mut self.later_clauses),
            later_in_body: std::mem::take(&mut self.later_in_body),
        });
        let result = self.linear_chain_in(chain, start, align);
        let end: Vec<String> = self.scope.names().map(str::to_string).collect();
        self.scope = saved.scope;
        self.dropped = saved.dropped;
        self.hidden = saved.hidden;
        self.open = saved.open;
        let pipeline = std::mem::replace(&mut self.pipeline, saved.pipeline);
        self.bound_clauses = saved.bound_clauses;
        self.later_clauses = saved.later_clauses;
        self.later_in_body = saved.later_in_body;
        let mut linear = result?;
        linear.pipeline = pipeline;
        Ok((linear, end))
    }

    fn linear_chain_in(
        &mut self,
        chain: &[&LinearQuery],
        start: Start<'_>,
        align: Option<&[String]>,
    ) -> BResult<Box<BoundLinear>> {
        let (Some(first), Some(last)) = (chain.first(), chain.last()) else {
            return Err(Box::new(semantic(
                self.query.span,
                "internal error: an empty chain of linear queries",
            )));
        };
        let unit = match start {
            Start::Names(names) => {
                self.scope = names;
                !matches!(first.stages.first(), Some(Stage::Match(_)))
            }
            Start::Rows(rows) => {
                self.rows_segment(rows)?;
                false
            }
        };
        for (i, linear) in chain.iter().enumerate() {
            self.enter_linear(linear)?;
            for stage in &linear.stages {
                self.stage(linear, stage)?;
            }
            if i + 1 < chain.len() {
                self.return_before_next(linear)?;
            }
        }
        self.result_statement(last, align, unit, first.span.to(last.span))
    }

    /// The `RETURN` of the last linear query of a pipeline and its `ORDER BY`, `OFFSET`,
    /// `LIMIT`.
    #[inline(never)]
    fn result_statement(
        &mut self,
        last: &LinearQuery,
        align: Option<&[String]>,
        unit: bool,
        span: Span,
    ) -> BResult<Box<BoundLinear>> {
        let ret = &last.ret;
        let mut columns = self.projection(ret.star, &ret.items, "RETURN")?;
        if let Some(order) = align {
            columns = self.align_columns(columns, order, ret.span)?;
        }
        let distinct = ret.distinct || self.group_by(ret.group_by.as_ref(), &columns)?;
        let order_by = self.order_by(last.order_by.as_ref(), &columns, distinct, "RETURN")?;
        let offset = self.count(last.offset.as_ref(), "OFFSET")?;
        let limit = self.count(last.limit.as_ref(), "LIMIT")?;
        Ok(Box::new(BoundLinear {
            pipeline: Vec::new(),
            columns,
            distinct,
            order_by,
            offset,
            limit,
            unit,
            span,
        }))
    }

    /// Starts binding the clauses of `linear`: the pattern size of each clause first (before
    /// any other work on it), and no comprehension variable around a subquery body declared
    /// by one of them.
    fn enter_linear(&mut self, linear: &LinearQuery) -> BResult<()> {
        for m in &linear.matches {
            if let Some(edge) = m
                .patterns
                .iter()
                .flat_map(|path| &path.steps)
                .nth(MAX_MATCH_EDGES)
            {
                return Err(too_many_edges(edge.edge.span));
            }
        }
        if let Some(var) = linear
            .matches
            .iter()
            .flat_map(declared_names)
            .find(|var| self.shelved_locals.iter().any(|l| l.name == var.name))
        {
            return Err(local_in_subquery(var));
        }
        self.bound_clauses = 0;
        self.later_clauses = linear
            .matches
            .iter()
            .enumerate()
            .flat_map(|(i, m)| declared_names(m).map(move |var| (i, var.name.clone())))
            .collect();
        Ok(())
    }

    /// The records of the block before as the first segment (design E19): each column a new
    /// binding.
    fn rows_segment(&mut self, rows: &RowsInput) -> BResult<()> {
        let segment = self.segments.len();
        let mut outputs = Vec::with_capacity(rows.columns.len());
        for (column, (name, kind)) in rows.columns.iter().enumerate() {
            let site = Site {
                clause: self.clause,
                segment,
                kind: *kind,
                group: false,
            };
            let reference = ColumnRef { segment, column };
            outputs.push(self.new_binding(site, Some(name), name.span, None, Some(reference)));
        }
        let span = rows.keyword;
        self.push_numbered(
            Segment::Rows(Box::new(BoundRows { outputs, span })),
            segment,
            span,
        )?;
        self.open = None;
        Ok(())
    }

    /// One stage of the pipeline of `linear`.
    fn stage(&mut self, linear: &LinearQuery, stage: &Stage) -> BResult<()> {
        match stage {
            Stage::Match(index) => self.match_stage(linear, *index),
            Stage::Let(clause) => self.let_clause(clause),
            Stage::Filter(clause) => self.filter_stage(&clause.condition),
            Stage::With(with) => self.with_clause(&WithParts::of(with)),
            Stage::For(clause) => self.for_clause(clause),
            Stage::Call(clause) => self.call_clause(clause),
        }
    }

    /// `FILTER`: conjuncts of the open segment.
    #[inline(never)]
    fn filter_stage(&mut self, condition: &Expr) -> BResult<()> {
        let segment = self.open_segment();
        self.conjuncts_of(
            Ctx::refusing("in FILTER"),
            condition,
            ConjunctOrigin::Filter,
            segment,
        )
    }

    /// `[OPTIONAL] MATCH`: clause `index` of `linear`.
    #[inline(never)]
    fn match_stage(&mut self, linear: &LinearQuery, index: usize) -> BResult<()> {
        let index = &index;
        {
            let m = linear
                .matches
                .get(*index)
                .filter(|_| *index == self.bound_clauses)
                .ok_or_else(|| {
                    semantic(linear.span, "internal error: MATCH stages out of order")
                })?;
            let clause = self.own.len();
            self.own.push(None);
            self.clause = clause;
            self.bound_clauses = index.saturating_add(1);
            let segment = if m.optional {
                self.open = None;
                self.push_segment(Segment::Optional(clause))
            } else {
                let segment = self.open_segment();
                if let Some(Segment::Match(clauses)) = self.segments.get_mut(segment) {
                    clauses.push(clause);
                }
                segment
            };
            let bound = self.declare_clause(clause, m, segment)?;
            self.clause_filters(clause, m, &bound)?;
            if let Some(slot) = self.own.get_mut(clause) {
                *slot = Some(bound);
            }
        }
        Ok(())
    }

    /// The `RETURN` of `linear` before `NEXT`, bound as a `WITH` of the same items (design
    /// E19: projecting is a substitution, materializing a [`Segment::With`]).
    fn return_before_next(&mut self, linear: &LinearQuery) -> BResult<()> {
        let ret = &linear.ret;
        self.with_clause(&WithParts {
            distinct: ret.distinct,
            star: ret.star,
            items: &ret.items,
            group_by: ret.group_by.as_ref(),
            order_by: linear.order_by.as_ref(),
            offset: linear.offset.as_ref(),
            limit: linear.limit.as_ref(),
            where_clause: None,
            span: ret.span,
            clause: "RETURN",
            dropper: Dropper::Next,
        })
    }

    /// The columns of a `UNION` operand after the first in the first operand's `order`
    /// (design E15): the same names, else 42002 naming both sets.
    fn align_columns(
        &self,
        columns: Vec<Column>,
        order: &[String],
        ret: Span,
    ) -> BResult<Vec<Column>> {
        let same = columns.len() == order.len()
            && order.iter().all(|n| columns.iter().any(|c| c.name == *n));
        if !same {
            let list = |names: &mut dyn Iterator<Item = &str>| {
                names
                    .map(|n| format!("`{n}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let theirs = list(&mut columns.iter().map(|c| c.name.as_str()));
            let first = list(&mut order.iter().map(String::as_str));
            let detail = match order
                .iter()
                .find(|n| !columns.iter().any(|c| c.name == **n))
            {
                Some(missing) => format!("`{missing}` is missing"),
                None => match columns.iter().find(|c| !order.contains(&c.name)) {
                    Some(extra) => format!("`{}` is not a column of the first", extra.name),
                    None => "the column counts differ".to_string(),
                },
            };
            return Err(semantic(
                ret,
                format!(
                    "the operands of UNION must return the same column names: the first \
                     returns {first}, this one returns {theirs} ({detail})"
                ),
            )
            .alternative(
                "name the columns alike with AS in every operand (their order may differ), as \
                 in MATCH (p:person) RETURN p.name AS name UNION MATCH (s:software) RETURN \
                 s.name AS name",
            )
            .topic(GqlHelpTopic::Names)
            .into());
        }
        let mut slots: Vec<Option<Column>> = columns.into_iter().map(Some).collect();
        order
            .iter()
            .map(|name| {
                slots
                    .iter_mut()
                    .find(|slot| slot.as_ref().is_some_and(|c| c.name == *name))
                    .and_then(Option::take)
            })
            .collect::<Option<Vec<Column>>>()
            .ok_or_else(|| Box::new(semantic(ret, "internal error: a UNION column not found")))
    }

    /// `[OPTIONAL] CALL (..) { body }` (module docs, "CALL"): the body bound as a composite
    /// with the imported names in scope, its returned columns new bindings of a
    /// [`Segment::Call`].
    #[inline(never)]
    fn call_clause(&mut self, clause: &CallClause) -> BResult<()> {
        if let CallScope::Variables { variables, .. } = &clause.scope {
            self.check_imports(variables, "CALL")?;
        }
        self.enter_body(clause.span)?;
        let outer = Rc::new(self.scope.clone());
        let marks = self.marks();
        self.calls.push(Rc::clone(&outer));
        let body = self.composite(
            &clause.body,
            Entry::Call {
                scope: &clause.scope,
                outer: &outer,
            },
        );
        self.calls.pop();
        self.body_depth = self.body_depth.saturating_sub(1);
        let (body, names) = body?;
        self.call_segment(clause, body, names, marks)
    }

    /// The [`Segment::Call`] of a bound `CALL` body: its imports and its returned columns.
    #[inline(never)]
    fn call_segment(
        &mut self,
        clause: &CallClause,
        body: Box<BoundComposite>,
        names: Vec<String>,
        marks: Marks,
    ) -> BResult<()> {
        let imports = self.reads_since(marks, &body);
        self.mark_read_here(&imports);
        // The returned columns: new names of the outer scope.
        let last = body.last();
        let segment = self.segments.len();
        let mut outputs = Vec::with_capacity(last.first.columns.len());
        for (i, column) in last.first.columns.iter().enumerate() {
            let name = Identifier {
                name: column.name.clone(),
                span: column.span,
            };
            if self.scope.get(&name.name).is_some() {
                return Err(semantic(
                    column.span,
                    format!(
                        "the CALL body returns `{}`, which is already defined outside it; a \
                         returned name must be new",
                        name.name
                    ),
                )
                .alternative(format!(
                    "rename the column inside the body, as in RETURN ... AS {}2",
                    name.name
                ))
                .topic(GqlHelpTopic::Scope)
                .into());
            }
            let site = Site {
                clause: self.clause,
                segment,
                kind: self.block_column_kind(last, i),
                group: false,
            };
            let reference = ColumnRef { segment, column: i };
            outputs.push(self.new_binding(site, Some(&name), name.span, None, Some(reference)));
        }
        self.push_numbered(
            Segment::Call(Box::new(BoundCall {
                optional: clause.optional.is_some(),
                body,
                imports,
                outputs,
                span: clause.span,
            })),
            segment,
            clause.span,
        )?;
        self.open = None;
        for name in names {
            if self.scope.get(&name).is_none() {
                self.hidden.entry(name).or_insert(Hidden::Call);
            }
        }
        Ok(())
    }

    /// The bindings older than `marks` that what was bound since reads (the imports of a
    /// `CALL` body or a whole-query subquery body `body`), sorted, without duplicates.
    fn reads_since(&self, marks: Marks, body: &BoundComposite) -> Vec<BindingId> {
        let mut ids: Vec<BindingId> = Vec::new();
        let elements = |m: &BoundMatch, ids: &mut Vec<BindingId>| {
            for path in &m.paths {
                ids.extend(path.variable);
                ids.push(path.start);
                ids.extend(path.steps.iter().flat_map(|step| [step.edge, step.node]));
            }
        };
        for c in self.conjuncts.get(marks.conjunct..).unwrap_or(&[]) {
            ids.extend_from_slice(&c.bindings);
        }
        for m in self.own.get(marks.own..).unwrap_or(&[]).iter().flatten() {
            elements(m, &mut ids);
        }
        for m in self
            .sub_matches
            .get(marks.sub..)
            .unwrap_or(&[])
            .iter()
            .flatten()
        {
            elements(m, &mut ids);
        }
        for s in self.subqueries.get(marks.subquery..).unwrap_or(&[]) {
            ids.extend_from_slice(&s.imports);
            ids.extend_from_slice(&s.map_bindings);
        }
        let result = |columns: &[Column], keys: &[SortKey], ids: &mut Vec<BindingId>| {
            ids.extend(columns.iter().flat_map(|c| c.bindings.iter().copied()));
            ids.extend(keys.iter().flat_map(|k| k.bindings.iter().copied()));
        };
        for segment in self.segments.get(marks.segment..).unwrap_or(&[]) {
            match segment {
                Segment::For(f) => ids.extend_from_slice(&f.bindings),
                Segment::With(with) => result(&with.columns, &with.order_by, &mut ids),
                Segment::Call(call) => ids.extend_from_slice(&call.imports),
                Segment::Match(_) | Segment::Optional(_) | Segment::Rows(_) => {}
            }
        }
        for linear in body.linears() {
            result(&linear.columns, &linear.order_by, &mut ids);
        }
        ids.retain(|id| id.0 < marks.binding);
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// Marks `ids` read where they are read now: inside a subquery body only the body's own
    /// locals (as for a conjunct there).
    fn mark_read_here(&mut self, ids: &[BindingId]) {
        let body = self.subquery;
        let ids: Vec<BindingId> = ids
            .iter()
            .copied()
            .filter(|id| {
                body.is_none() || self.bindings.get(id.0).is_some_and(|b| b.subquery == body)
            })
            .collect();
        self.mark_read(&ids);
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
                        .topic(GqlHelpTopic::PatternVariable)
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
            self.hidden.remove(&v.name);
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
            subquery: self.subquery,
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
                "give the path its own name, as in {}2 = (a)-->(b)",
                variable.name
            ))
            .topic(GqlHelpTopic::PatternVariable)
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
                .topic(GqlHelpTopic::PatternVariable)
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
        let for_column = if binding.kind == BindingKind::Value {
            self.for_column(id)
        } else {
            None
        };
        if for_column == Some(FOR_ORDINAL) {
            return Err(ordinal_as_element(var, site.kind));
        }
        // A FOR item written as a node or edge takes that kind (design E3); as the edge of a
        // quantified edge it is refused below (a group variable is always a new name).
        let retype = for_column == Some(FOR_ITEM)
            && matches!(site.kind, BindingKind::Node | BindingKind::Edge);
        if binding.kind != site.kind && !retype {
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
            .topic(GqlHelpTopic::PatternVariable)
            .into());
        }
        let same_segment = binding.segment == site.segment;
        let key = binding.key.clone().unwrap_or_default();
        if let Some(binding) = self.bindings.get_mut(id.0) {
            if retype {
                binding.kind = site.kind;
            }
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
            // Inside a subquery body only the body's own locals: whether an import is read
            // away from the current element depends on where the body runs, which the planner
            // decides (`Labelling::subquery`); the host expression reads the imports anyway.
            let body = self.subquery;
            let ids: Vec<BindingId> = conjunct
                .bindings
                .iter()
                .copied()
                .filter(|id| {
                    body.is_none() || self.bindings.get(id.0).is_some_and(|b| b.subquery == body)
                })
                .collect();
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
                    "choose another name, as in LET {}2 = <value>",
                    var.name
                ))
                .topic(GqlHelpTopic::Scope)
                .into());
            }
            if defined.iter().any(|(n, _)| n.name == var.name) {
                return Err(semantic(
                    var.span,
                    format!("the variable `{}` is defined twice in this LET", var.name),
                )
                .topic(GqlHelpTopic::Names)
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

    /// `FOR x IN list [WITH ORDINALITY i | WITH OFFSET i]` / `UNWIND list AS x` (module docs):
    /// a cut whose variable and ordinal enter the scope as the columns of a [`Segment::For`].
    fn for_clause(&mut self, clause: &ForClause) -> BResult<()> {
        let keyword = if clause.unwind { "UNWIND" } else { "FOR" };
        let place = if clause.unwind { "in UNWIND" } else { "in FOR" };
        // The list reads the scope before the FOR.
        let list = self.expression(Ctx::refusing(place), &clause.list)?;
        let var = &clause.variable;
        self.check_new_for_name(var, keyword, clause.unwind)?;
        if let Some(ordinal) = &clause.ordinal {
            let name = &ordinal.variable;
            if name.name == var.name {
                let spelled = ordinal_word(ordinal.kind);
                return Err(semantic(
                    name.span,
                    format!(
                        "`{}` names both the item and its position; WITH {spelled} defines a \
                         second variable",
                        name.name
                    ),
                )
                .alternative(format!(
                    "give the position its own name, as in FOR {} IN ... WITH {spelled} {}_index",
                    var.name, var.name
                ))
                .topic(GqlHelpTopic::For)
                .into());
            }
            self.check_new_for_name(name, keyword, clause.unwind)?;
        }
        self.mark_read(&list.refs.bindings);
        self.open = None;
        let segment = self.segments.len();
        let column = |this: &mut Self, name: &Identifier, column: usize| {
            let site = Site {
                clause: this.clause,
                segment,
                kind: BindingKind::Value,
                group: false,
            };
            let reference = ColumnRef { segment, column };
            this.new_binding(site, Some(name), name.span, None, Some(reference))
        };
        let variable = column(self, var, FOR_ITEM);
        let ordinal = clause
            .ordinal
            .as_ref()
            .map(|o| (o.kind, column(self, &o.variable, FOR_ORDINAL)));
        self.push_numbered(
            Segment::For(Box::new(BoundFor {
                variable,
                list: list.expr,
                bindings: list.refs.bindings,
                ordinal,
                unwind: clause.unwind,
                span: clause.span,
            })),
            segment,
            clause.span,
        )
    }

    /// A `FOR` variable or ordinal must be a new name (42002 like `LET`).
    fn check_new_for_name(&self, var: &Identifier, keyword: &str, unwind: bool) -> BResult<()> {
        if self.scope.get(&var.name).is_none() {
            return Ok(());
        }
        let example = if unwind {
            format!("UNWIND <list> AS {}2", var.name)
        } else {
            format!("FOR {}2 IN <list>", var.name)
        };
        Err(semantic(
            var.span,
            format!(
                "the variable `{}` is already defined; {keyword} defines a new variable",
                var.name
            ),
        )
        .alternative(format!("choose another name, as in {example}"))
        .topic(GqlHelpTopic::For)
        .into())
    }

    /// The column of a [`Segment::For`] the binding `id` stands for.
    fn for_column(&self, id: BindingId) -> Option<usize> {
        let column = self.bindings.get(id.0)?.column?;
        matches!(self.segments.get(column.segment), Some(Segment::For(_))).then_some(column.column)
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

    /// A `WITH`, or a `RETURN` before `NEXT` read as one (design E19).
    fn with_clause(&mut self, with: &WithParts<'_>) -> BResult<()> {
        let (columns, names) = self.projection_named(with.star, with.items, with.clause)?;
        let distinct = with.distinct || self.group_by(with.group_by, &columns)?;
        let aggregated = columns.iter().any(|c| c.aggregate);
        let materializing = aggregated
            || distinct
            || with.order_by.is_some()
            || with.offset.is_some()
            || with.limit.is_some();
        if materializing {
            let order_by = self.order_by(with.order_by, &columns, distinct, with.clause)?;
            let offset = self.count(with.offset, "OFFSET")?;
            let limit = self.count(with.limit, "LIMIT")?;
            let segment = self.segments.len();
            // The columns become new bindings in a scope of their own; the old scope is gone.
            let old = std::mem::take(&mut self.scope);
            let mut outputs = Vec::with_capacity(columns.len());
            for (index, (column, name)) in columns.iter().zip(&names).enumerate() {
                let kind = self.column_kind(column);
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
            self.push_numbered(
                Segment::With(Box::new(BoundWith {
                    columns,
                    distinct,
                    order_by,
                    offset,
                    limit,
                    outputs,
                    span: with.span,
                })),
                segment,
                with.span,
            )?;
            self.open = None;
            self.replace_scope(next, with.dropper);
            if let Some(where_clause) = with.where_clause {
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
            self.replace_scope(next, with.dropper);
            if let Some(where_clause) = with.where_clause {
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
    fn replace_scope(&mut self, next: Names, dropper: Dropper) {
        let old = std::mem::replace(&mut self.scope, next);
        for (name, _) in old.entries {
            if self.scope.get(&name.name).is_none() {
                self.dropped.insert(name.name, dropper);
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
        // A thin dispatch (rare forms in functions of their own): one level of a tall
        // expression costs small frames only.
        match &expr.kind {
            ExprKind::Literal(_) => {}
            ExprKind::Parameter(name) => self.parameters.push(ParameterRef {
                name: name.clone(),
                span: expr.span,
            }),
            ExprKind::Variable(var) => self.variable(ctx, var, refs)?,
            ExprKind::Property { target, key } => {
                if let ExprKind::Variable(var) = &target.kind {
                    self.check_group_property(ctx, var, key)?;
                }
                self.resolve(ctx, target, refs)?;
            }
            ExprKind::Unary { operand, .. }
            | ExprKind::IsNull { operand, .. }
            | ExprKind::IsLabeled { operand, .. } => self.resolve(ctx, operand, refs)?,
            ExprKind::Binary { left, right, .. }
            | ExprKind::IsEndpoint {
                operand: left,
                edge: right,
                ..
            } => {
                self.resolve(ctx, left, refs)?;
                self.resolve(ctx, right, refs)?;
            }
            ExprKind::FunctionCall { name, args, .. } => {
                self.resolve_call(ctx, name, args, refs)?
            }
            ExprKind::List(_)
            | ExprKind::Map(_)
            | ExprKind::Case(_)
            | ExprKind::Subquery(_)
            | ExprKind::MapProjection(_)
            | ExprKind::ListComprehension(_)
            | ExprKind::PatternComprehension(_) => self.resolve_other(ctx, expr, refs)?,
        }
        Ok(())
    }

    /// A function call (an aggregate only where aggregates may stand).
    #[inline(never)]
    fn resolve_call(
        &mut self,
        ctx: Ctx<'_>,
        name: &Identifier,
        args: &CallArgs,
        refs: &mut Refs,
    ) -> BResult<()> {
        let mut inner = ctx;
        if is_aggregate(name) {
            if let Aggregates::Refused(place) = ctx.aggregates {
                return Err(misplaced_aggregate(name, place));
            }
            refs.aggregate.get_or_insert(name.span);
            inner.aggregates = Aggregates::Refused("inside another aggregate");
        }
        for arg in args.iter() {
            self.resolve(inner, arg, refs)?;
        }
        Ok(())
    }

    /// The forms [`Self::resolve`] does not handle itself.
    #[inline(never)]
    fn resolve_other(&mut self, ctx: Ctx<'_>, expr: &Expr, refs: &mut Refs) -> BResult<()> {
        match &expr.kind {
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
            ExprKind::Subquery(subquery) => {
                let index = self.subquery_expr(ctx, subquery)?;
                if let Some(bound) = self.subqueries.get(index) {
                    refs.bindings.extend_from_slice(&bound.imports);
                }
            }
            ExprKind::MapProjection(projection) => {
                self.resolve(ctx, &projection.target, refs)?;
                for item in &projection.items {
                    match item {
                        ProjectionItem::Property(_) | ProjectionItem::AllProperties(_) => {}
                        ProjectionItem::Variable(var) => self.variable(ctx, var, refs)?,
                        ProjectionItem::Entry(entry) => self.resolve(ctx, &entry.value, refs)?,
                    }
                }
            }
            ExprKind::ListComprehension(comprehension) => {
                self.comprehension(ctx, comprehension, refs)?;
            }
            ExprKind::PatternComprehension(comprehension) => {
                let subquery = SubqueryExpr {
                    kind: SubqueryKind::Collect,
                    form: SubqueryForm::Comprehension,
                    matches: vec![comprehension.body.clone()],
                    query: None,
                    span: expr.span,
                };
                let index = self.subquery_with(ctx, &subquery, Some(&comprehension.map))?;
                if let Some(bound) = self.subqueries.get(index) {
                    refs.bindings.extend_from_slice(&bound.imports);
                }
            }
            ExprKind::Literal(_)
            | ExprKind::Parameter(_)
            | ExprKind::Variable(_)
            | ExprKind::Property { .. }
            | ExprKind::Unary { .. }
            | ExprKind::IsNull { .. }
            | ExprKind::IsLabeled { .. }
            | ExprKind::Binary { .. }
            | ExprKind::IsEndpoint { .. }
            | ExprKind::FunctionCall { .. } => self.resolve(ctx, expr, refs)?,
        }
        Ok(())
    }

    /// A list comprehension: its list in the scope around it, its `WHERE` and `|` parts with
    /// its variable as a local (design M4) that may not shadow any other name (E14), no
    /// aggregate inside them. The list predicates and `reduce` (stage 2d, D23) bind the same
    /// way; `reduce`'s initial value lies in the scope around it, its accumulator is a second
    /// local of the `|` part (E14 too, and not the item variable's name).
    #[inline(never)]
    fn comprehension(
        &mut self,
        ctx: Ctx<'_>,
        comprehension: &ListComprehension,
        refs: &mut Refs,
    ) -> BResult<()> {
        if let Some(init) = comprehension.init() {
            self.resolve(ctx, init, refs)?;
        }
        self.resolve(ctx, &comprehension.list, refs)?;
        let place = match &comprehension.form {
            ComprehensionForm::List => IN_COMPREHENSION,
            ComprehensionForm::Predicate(_) => IN_LIST_PREDICATE,
            ComprehensionForm::Reduce(_) => IN_REDUCE,
        };
        let inner = Ctx {
            aggregates: Aggregates::Refused(place),
            ..ctx
        };
        let pushed = self.locals.len();
        let mut result = Ok(());
        if let ComprehensionForm::Reduce(accumulator) = &comprehension.form {
            result = self.check_local(ctx, &accumulator.variable, comprehension);
            if result.is_ok() {
                self.locals.push(accumulator.variable.clone());
            }
        }
        if result.is_ok() {
            result = self.check_local(ctx, &comprehension.variable, comprehension);
        }
        if result.is_ok() {
            self.locals.push(comprehension.variable.clone());
            for part in [&comprehension.filter, &comprehension.map]
                .into_iter()
                .flatten()
            {
                result = self.resolve(inner, part, refs);
                if result.is_err() {
                    break;
                }
            }
        }
        self.locals.truncate(pushed);
        result
    }

    /// A comprehension variable may not shadow a name (design E14, 42002): a variable or an
    /// alias in scope, another comprehension variable around it, or any name a binding of the
    /// query has or had (a substituted expression refers to bindings by those names, so a
    /// local of the same name would capture them). The same holds for the variables of the
    /// list predicates and of `reduce` (`within`, for the message).
    fn check_local(
        &self,
        ctx: Ctx<'_>,
        var: &Identifier,
        within: &ListComprehension,
    ) -> BResult<()> {
        let name = &var.name;
        let in_scope = self.scope.get(name).is_some()
            || ctx.aliases.iter().any(|(alias, _)| alias == name)
            || self.locals.iter().any(|local| local.name == *name)
            || self.shelved_locals.iter().any(|local| local.name == *name);
        let used = self.taken.contains(name.as_str())
            || self.dropped.contains_key(name)
            || self.hidden.contains_key(name)
            || self.pending_let.iter().any(|n| n == name);
        if !in_scope && !used {
            return Ok(());
        }
        let (what, alternative) = match &within.form {
            ComprehensionForm::List => (
                format!("the comprehension variable `{name}`"),
                format!(
                    "give the comprehension a variable of its own, as in [{name}2 IN list WHERE \
                     {name}2 > 0 | {name}2]"
                ),
            ),
            ComprehensionForm::Predicate(predicate) => {
                let f = predicate.name();
                (
                    format!("the variable `{name}` of {f}()"),
                    format!(
                        "give {f}() a variable of its own, as in {f}({name}2 IN list WHERE \
                         {name}2 > 0)"
                    ),
                )
            }
            ComprehensionForm::Reduce(accumulator) if accumulator.variable.span == var.span => (
                format!("the accumulator `{name}` of reduce()"),
                format!(
                    "give reduce() an accumulator of its own, as in reduce({name}2 = 0, item IN \
                     list | {name}2 + item)"
                ),
            ),
            ComprehensionForm::Reduce(_) => (
                format!("the variable `{name}` of reduce()"),
                format!(
                    "give reduce() a variable of its own, as in reduce(total = 0, {name}2 IN \
                     list | total + {name}2)"
                ),
            ),
        };
        let rule = match within.form {
            ComprehensionForm::List => "a comprehension variable must be a new name",
            ComprehensionForm::Predicate(_) | ComprehensionForm::Reduce(_) => {
                "it must be a new name"
            }
        };
        let message = if in_scope {
            format!("{what} shadows the variable `{name}` in scope; {rule}")
        } else {
            format!("{what} is a name this query already uses; {rule}")
        };
        Err(semantic(var.span, message)
            .alternative(alternative)
            .topic(GqlHelpTopic::ComprehensionVariable)
            .into())
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
        .topic(GqlHelpTopic::PatternVariable)
        .into())
    }

    fn variable(&self, ctx: Ctx<'_>, var: &Identifier, refs: &mut Refs) -> BResult<()> {
        // A comprehension variable reads no binding (it shadows nothing: `check_local`).
        if self.locals.iter().any(|local| local.name == var.name) {
            return Ok(());
        }
        if let Some((_, column)) = ctx.aliases.iter().find(|(name, _)| *name == var.name) {
            refs.aliases.push(*column);
            return Ok(());
        }
        match self.scope.get(&var.name) {
            Some(Symbol::Binding(id)) => {
                let binding = self.bindings.get(id.0);
                if let Some(binding) = binding
                    && binding.kind == BindingKind::Path
                    && (ctx.inline_clause == Some(binding.clause)
                        || self.incomplete_paths.contains(&binding.clause))
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
                    .topic(GqlHelpTopic::PatternVariable)
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
        if self.shelved_locals.iter().any(|local| local.name == *name) {
            return local_in_subquery(var);
        }
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
            .topic(GqlHelpTopic::Scope)
            .into();
        }
        match self.dropped.get(name) {
            Some(Dropper::With) => {
                return semantic(
                    var.span,
                    format!(
                        "the variable `{name}` is not in scope: the WITH before does not carry it"
                    ),
                )
                .alternative(format!(
                    "add `{name}` to the WITH items, as in WITH ..., {name}"
                ))
                .topic(GqlHelpTopic::Scope)
                .into();
            }
            Some(Dropper::Next) => {
                return semantic(
                    var.span,
                    format!(
                        "the variable `{name}` is not in scope: the RETURN before NEXT does not \
                         return it"
                    ),
                )
                .alternative(format!(
                    "add `{name}` to the RETURN items before NEXT, as in RETURN ..., {name} \
                     NEXT ..."
                ))
                .topic(GqlHelpTopic::Scope)
                .into();
            }
            None => {}
        }
        if self.later_in_body.iter().any(|n| n == name) {
            return semantic(
                var.span,
                format!(
                    "the variable `{name}` is not defined here: it is bound by a later MATCH \
                     clause of the subquery"
                ),
            )
            .alternative(
                "move the condition to the WHERE of that later MATCH clause inside the subquery",
            )
            .topic(GqlHelpTopic::Subquery)
            .into();
        }
        if self.hidden.get(name) == Some(&Hidden::Call) {
            return semantic(
                var.span,
                format!(
                    "the variable `{name}` is not in scope: it is bound inside a CALL body, \
                     which passes on only the columns of its RETURN"
                ),
            )
            .alternative(format!(
                "return it from the body, as in CALL {{ MATCH ({name}) RETURN {name} }}"
            ))
            .topic(GqlHelpTopic::Scope)
            .into();
        }
        if let Some(&hidden) = self.hidden.get(name) {
            if hidden == Hidden::Comprehension {
                return semantic(
                    var.span,
                    format!(
                        "the variable `{name}` is not in scope: it is bound inside a pattern \
                         comprehension, whose variables are visible only inside it"
                    ),
                )
                .alternative(format!(
                    "bind `{name}` in the MATCH clause of the query to use it here, or read it \
                     in the | part of the comprehension, as in [(a)-->({name}) | {name}.name]"
                ))
                .topic(GqlHelpTopic::PatternComprehension)
                .into();
            }
            return semantic(
                var.span,
                format!(
                    "the variable `{name}` is not in scope: it is bound inside a subquery (EXISTS \
                     or a pattern predicate), whose variables are visible only inside it"
                ),
            )
            .alternative(format!(
                "bind `{name}` in the MATCH clause of the query to use it here, or test it \
                 inside the subquery"
            ))
            .topic(GqlHelpTopic::Subquery)
            .into();
        }
        let later = self
            .later_clauses
            .iter()
            .any(|(clause, declared)| *clause >= self.bound_clauses && declared == name);
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
        if let Some(outer) = self.calls.last()
            && outer.get(name).is_some()
        {
            return semantic(
                var.span,
                format!(
                    "the variable `{name}` is not imported into the CALL body: a CALL body sees \
                     only the variables its scope clause names"
                ),
            )
            .alternative(format!(
                "name it in the variable scope clause, as in CALL ({name}) {{ ... }}, or import \
                 every variable with CALL (*) {{ ... }}"
            ))
            .topic(GqlHelpTopic::Scope)
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
        // A thin dispatch: each form in a function of its own, so one level of a tall
        // expression costs two small frames, not one frame holding every form's temporaries
        // (unoptimized builds give each a stack slot).
        match &expr.kind {
            // The body is bound (and substituted) on its own ([`BoundQuery::subqueries`]); the
            // expression keeps the written subquery as a reference to it. A pattern
            // comprehension is bound (and its `|` part substituted) on its own like a
            // subquery ([`BoundSubquery::map`]).
            ExprKind::Literal(_)
            | ExprKind::Parameter(_)
            | ExprKind::Subquery(_)
            | ExprKind::PatternComprehension(_) => Ok((expr.clone(), Shape::LEAF)),
            ExprKind::Variable(var) => self.substitute_variable(aliases, var, expr),
            ExprKind::Binary { .. } => self.substitute_binary(aliases, expr),
            ExprKind::Unary { .. }
            | ExprKind::Property { .. }
            | ExprKind::IsNull { .. }
            | ExprKind::IsLabeled { .. }
            | ExprKind::IsEndpoint { .. } => self.substitute_operand(aliases, expr),
            ExprKind::List(_) | ExprKind::Map(_) | ExprKind::Case(_) => {
                self.substitute_collection(aliases, expr)
            }
            ExprKind::FunctionCall { .. } => self.substitute_call(aliases, expr),
            ExprKind::MapProjection(_) => self.substitute_projection(aliases, expr),
            ExprKind::ListComprehension(_) => self.substitute_comprehension(aliases, expr),
        }
    }

    #[inline(never)]
    fn substitute_binary(
        &mut self,
        aliases: &[(String, usize)],
        expr: &Expr,
    ) -> BResult<(Expr, Shape)> {
        let ExprKind::Binary { op, left, right } = &expr.kind else {
            return Err(not_dispatched());
        };
        let (left, ls) = self.substitute(aliases, left)?;
        let (right, rs) = self.substitute(aliases, right)?;
        let kind = ExprKind::Binary {
            op: *op,
            left: Box::new(left),
            right: Box::new(right),
        };
        Ok((
            Expr {
                kind,
                span: expr.span,
            },
            combine(&[(ls, false), (rs, true)]),
        ))
    }

    /// The forms over one operand (and `IS [NOT] SOURCE / DESTINATION OF` over two).
    #[inline(never)]
    fn substitute_operand(
        &mut self,
        aliases: &[(String, usize)],
        expr: &Expr,
    ) -> BResult<(Expr, Shape)> {
        let (kind, shape) = match &expr.kind {
            ExprKind::Unary { op, operand } => {
                let (operand, s) = self.substitute(aliases, operand)?;
                let kind = ExprKind::Unary {
                    op: *op,
                    operand: Box::new(operand),
                };
                (kind, combine(&[(s, true)]))
            }
            ExprKind::Property { target, key } => {
                let (target, s) = self.substitute(aliases, target)?;
                let kind = ExprKind::Property {
                    target: Box::new(target),
                    key: key.clone(),
                };
                (kind, combine(&[(s, false)]))
            }
            ExprKind::IsNull { operand, negated } => {
                let (operand, s) = self.substitute(aliases, operand)?;
                let kind = ExprKind::IsNull {
                    operand: Box::new(operand),
                    negated: *negated,
                };
                (kind, combine(&[(s, false)]))
            }
            ExprKind::IsLabeled {
                operand,
                labels,
                negated,
            } => {
                let (operand, s) = self.substitute(aliases, operand)?;
                let kind = ExprKind::IsLabeled {
                    operand: Box::new(operand),
                    labels: labels.clone(),
                    negated: *negated,
                };
                (kind, combine(&[(s, false)]))
            }
            ExprKind::IsEndpoint {
                operand,
                edge,
                end,
                negated,
            } => {
                let (operand, os) = self.substitute(aliases, operand)?;
                let (edge, es) = self.substitute(aliases, edge)?;
                let kind = ExprKind::IsEndpoint {
                    operand: Box::new(operand),
                    edge: Box::new(edge),
                    end: *end,
                    negated: *negated,
                };
                (kind, combine(&[(os, false), (es, true)]))
            }
            ExprKind::Literal(_)
            | ExprKind::Parameter(_)
            | ExprKind::Variable(_)
            | ExprKind::Binary { .. }
            | ExprKind::List(_)
            | ExprKind::Map(_)
            | ExprKind::Case(_)
            | ExprKind::FunctionCall { .. }
            | ExprKind::Subquery(_)
            | ExprKind::MapProjection(_)
            | ExprKind::ListComprehension(_)
            | ExprKind::PatternComprehension(_) => return Err(not_dispatched()),
        };
        Ok((
            Expr {
                kind,
                span: expr.span,
            },
            shape,
        ))
    }

    /// A list, a map, a `CASE`.
    #[inline(never)]
    fn substitute_collection(
        &mut self,
        aliases: &[(String, usize)],
        expr: &Expr,
    ) -> BResult<(Expr, Shape)> {
        let mut children = Vec::new();
        let kind = match &expr.kind {
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
            ExprKind::Literal(_)
            | ExprKind::Parameter(_)
            | ExprKind::Variable(_)
            | ExprKind::Property { .. }
            | ExprKind::Unary { .. }
            | ExprKind::Binary { .. }
            | ExprKind::IsNull { .. }
            | ExprKind::IsLabeled { .. }
            | ExprKind::IsEndpoint { .. }
            | ExprKind::FunctionCall { .. }
            | ExprKind::Subquery(_)
            | ExprKind::MapProjection(_)
            | ExprKind::ListComprehension(_)
            | ExprKind::PatternComprehension(_) => return Err(not_dispatched()),
        };
        Ok((
            Expr {
                kind,
                span: expr.span,
            },
            combine(&children),
        ))
    }

    #[inline(never)]
    fn substitute_call(
        &mut self,
        aliases: &[(String, usize)],
        expr: &Expr,
    ) -> BResult<(Expr, Shape)> {
        let ExprKind::FunctionCall {
            name,
            distinct,
            args,
        } = &expr.kind
        else {
            return Err(not_dispatched());
        };
        let mut children = Vec::new();
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
            CallArgs::Cast {
                operand,
                target,
                target_span,
            } => {
                let (operand, s) = self.substitute(aliases, operand)?;
                children.push((s, true));
                CallArgs::Cast {
                    operand: Box::new(operand),
                    target: *target,
                    target_span: *target_span,
                }
            }
            CallArgs::Trim {
                side,
                character,
                source,
            } => {
                let character = match character {
                    Some(character) => {
                        let (character, s) = self.substitute(aliases, character)?;
                        children.push((s, true));
                        Some(Box::new(character))
                    }
                    None => None,
                };
                let (source, s) = self.substitute(aliases, source)?;
                children.push((s, true));
                CallArgs::Trim {
                    side: *side,
                    character,
                    source: Box::new(source),
                }
            }
        };
        let kind = ExprKind::FunctionCall {
            name: name.clone(),
            distinct: *distinct,
            args,
        };
        Ok((
            Expr {
                kind,
                span: expr.span,
            },
            combine(&children),
        ))
    }

    #[inline(never)]
    fn substitute_projection(
        &mut self,
        aliases: &[(String, usize)],
        expr: &Expr,
    ) -> BResult<(Expr, Shape)> {
        let ExprKind::MapProjection(projection) = &expr.kind else {
            return Err(not_dispatched());
        };
        let mut children = Vec::new();
        let (target, s) = self.substitute(aliases, &projection.target)?;
        children.push((s, true));
        let mut items = Vec::with_capacity(projection.items.len());
        for item in &projection.items {
            items.push(match item {
                ProjectionItem::Property(_) | ProjectionItem::AllProperties(_) => item.clone(),
                // The key stays the written name; the value is substituted.
                ProjectionItem::Variable(var) => {
                    let written = Expr {
                        kind: ExprKind::Variable(var.clone()),
                        span: var.span,
                    };
                    let (value, s) = self.substitute(aliases, &written)?;
                    children.push((s, true));
                    if value == written {
                        item.clone()
                    } else {
                        ProjectionItem::Entry(MapEntry {
                            key: var.clone(),
                            value,
                        })
                    }
                }
                ProjectionItem::Entry(entry) => {
                    let (value, s) = self.substitute(aliases, &entry.value)?;
                    children.push((s, true));
                    ProjectionItem::Entry(MapEntry {
                        key: entry.key.clone(),
                        value,
                    })
                }
            });
        }
        let kind = ExprKind::MapProjection(Box::new(MapProjection {
            target,
            items,
            braces: projection.braces,
        }));
        Ok((
            Expr {
                kind,
                span: expr.span,
            },
            combine(&children),
        ))
    }

    /// The variable is a local of the `WHERE` / `|` parts: it is in no scope, so
    /// `substitute_variable` leaves it as written.
    #[inline(never)]
    fn substitute_comprehension(
        &mut self,
        aliases: &[(String, usize)],
        expr: &Expr,
    ) -> BResult<(Expr, Shape)> {
        let ExprKind::ListComprehension(comprehension) = &expr.kind else {
            return Err(not_dispatched());
        };
        let mut children = Vec::new();
        let mut one = |this: &mut Self, e: &Expr| -> BResult<Expr> {
            let (e, s) = this.substitute(aliases, e)?;
            children.push((s, true));
            Ok(e)
        };
        let form = match &comprehension.form {
            ComprehensionForm::Reduce(accumulator) => {
                ComprehensionForm::Reduce(Box::new(ReduceAccumulator {
                    variable: accumulator.variable.clone(),
                    init: one(self, &accumulator.init)?,
                }))
            }
            form @ (ComprehensionForm::List | ComprehensionForm::Predicate(_)) => form.clone(),
        };
        let list = one(self, &comprehension.list)?;
        let filter = match &comprehension.filter {
            Some(filter) => Some(one(self, filter)?),
            None => None,
        };
        let map = match &comprehension.map {
            Some(map) => Some(one(self, map)?),
            None => None,
        };
        let kind = ExprKind::ListComprehension(Box::new(ListComprehension {
            form,
            variable: comprehension.variable.clone(),
            list,
            filter,
            map,
        }));
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

    // ----- subqueries -----

    /// Binds the body of `subquery` as a nested scope (module docs, "Subqueries") and returns its
    /// index in [`BoundQuery::subqueries`]. Recursion: one level per nested subquery, which the
    /// parser bounds by `MAX_DEPTH` and the host expression's height by `MAX_HEIGHT`.
    #[inline(never)]
    fn subquery_expr(&mut self, ctx: Ctx<'_>, subquery: &SubqueryExpr) -> BResult<usize> {
        self.subquery_with(ctx, subquery, None)
    }

    /// [`Self::subquery_expr`] with the `|` expression `map` of a pattern comprehension, resolved
    /// in the body's scope after its clauses (module docs, "Pattern comprehensions").
    fn subquery_with(
        &mut self,
        ctx: Ctx<'_>,
        subquery: &SubqueryExpr,
        map: Option<&Expr>,
    ) -> BResult<usize> {
        // An expression is resolved once; a repeat (none today) would read the same body.
        if let Some(index) = self.subqueries.iter().position(|s| s.span == subquery.span) {
            return Ok(index);
        }
        if subquery.kind == SubqueryKind::Collect
            && subquery.form == SubqueryForm::Braces
            && subquery.query.is_none()
        {
            return Err(collect_needs_one_column(subquery.span));
        }
        if self.subqueries.len() >= MAX_SUBQUERIES {
            return Err(too_many_subqueries(subquery.span));
        }
        for m in &subquery.matches {
            if let Some(edge) = m
                .patterns
                .iter()
                .flat_map(|path| &path.steps)
                .nth(MAX_MATCH_EDGES)
            {
                return Err(too_many_edges(edge.edge.span));
            }
        }
        // A comprehension variable is not visible inside a subquery yet: a body naming one
        // would silently bind a new variable instead.
        if let Some(var) = subquery
            .matches
            .iter()
            .flat_map(declared_names)
            .find(|var| self.locals.iter().any(|local| local.name == var.name))
        {
            return Err(local_in_subquery(var));
        }
        if subquery.form == SubqueryForm::PatternPredicate {
            self.check_pattern_predicate(ctx, subquery)?;
        }
        let index = self.subqueries.len();
        let segment = SUBQUERY_SEGMENTS.saturating_add(index);
        self.subqueries.push(BoundSubquery {
            kind: subquery.kind,
            form: subquery.form,
            clauses: Vec::new(),
            segment,
            imports: Vec::new(),
            parent: self.subquery,
            span: subquery.span,
            map: None,
            map_bindings: Vec::new(),
            query: None,
        });
        // The nested scope: the names in scope now, plus the body's own (locals).
        let scope = self.scope.clone();
        let marks = self.marks();
        let dropped = self.dropped.clone();
        let clause = self.clause;
        let parent = self.subquery.replace(index);
        let incomplete = ctx.inline_clause.is_some();
        if let Some(inline) = ctx.inline_clause {
            self.incomplete_paths.push(inline);
        }
        let first_binding = self.bindings.len();
        let first_conjunct = self.conjuncts.len();
        let first_clause = self.sub_matches.len();
        let shelved = self.shelved_locals.len();
        let locals = std::mem::take(&mut self.locals);
        self.shelved_locals.extend(locals.iter().cloned());
        let body = match subquery.query.as_deref() {
            Some(query) => self.whole_query_body(query, &scope),
            None => self.subquery_body(subquery, segment).and_then(|clauses| {
                // The `|` part sees the body's names; like a body condition it marks only the
                // body's own locals read (an import is read by the host expression anyway).
                let resolved = match map {
                    Some(map) => {
                        Some(self.expression(Ctx::refusing(IN_PATTERN_COMPREHENSION), map)?)
                    }
                    None => None,
                };
                if let Some(resolved) = &resolved {
                    let own: Vec<BindingId> = resolved
                        .refs
                        .bindings
                        .iter()
                        .copied()
                        .filter(|id| {
                            self.bindings
                                .get(id.0)
                                .is_some_and(|b| b.subquery == Some(index))
                        })
                        .collect();
                    self.mark_read(&own);
                }
                Ok((clauses, resolved, None))
            }),
        };
        self.shelved_locals.truncate(shelved);
        self.locals = locals;
        if incomplete {
            self.incomplete_paths.pop();
        }
        self.subquery = parent;
        self.clause = clause;
        self.dropped = dropped;
        let inner = std::mem::replace(&mut self.scope, scope);
        let (clauses, map, query) = body?;
        let hidden = if subquery.form == SubqueryForm::Comprehension {
            Hidden::Comprehension
        } else {
            Hidden::Subquery
        };
        let names = query
            .as_ref()
            .map_or(&[][..], |(_, names)| names.as_slice());
        for name in inner
            .entries
            .into_iter()
            .map(|(n, _)| n.name)
            .chain(names.iter().cloned())
        {
            if self.scope.get(&name).is_none() {
                self.hidden.insert(name, hidden);
            }
        }
        // Imports: every binding older than the body that the body (nested bodies included)
        // reads, in a pattern or in a condition.
        let outside = |id: &BindingId| id.0 < first_binding;
        let mut imports: Vec<BindingId> = self
            .conjuncts
            .get(first_conjunct..)
            .unwrap_or(&[])
            .iter()
            .flat_map(|c| c.bindings.iter().copied())
            .filter(outside)
            .collect();
        for m in self
            .sub_matches
            .get(first_clause..)
            .unwrap_or(&[])
            .iter()
            .flatten()
        {
            for path in &m.paths {
                let elements = std::iter::once(path.start)
                    .chain(path.variable)
                    .chain(path.steps.iter().flat_map(|step| [step.edge, step.node]));
                imports.extend(elements.filter(outside));
            }
        }
        if let Some(map) = &map {
            imports.extend(map.refs.bindings.iter().copied().filter(outside));
        }
        let query = match query {
            Some((query, _)) => {
                if subquery.kind == SubqueryKind::Collect && query.last().first.columns.len() != 1 {
                    return Err(collect_needs_one_column(subquery.span));
                }
                imports = self.reads_since(marks, &query);
                Some(query)
            }
            None => None,
        };
        imports.sort_unstable();
        imports.dedup();
        if let Some(bound) = self.subqueries.get_mut(index) {
            bound.clauses = clauses;
            bound.imports = imports;
            bound.query = query;
            if let Some(map) = map {
                bound.map = Some(map.expr);
                bound.map_bindings = map.refs.bindings;
            }
        }
        Ok(index)
    }

    /// A whole-query subquery body (stage 2d): a composite whose operands start with every
    /// name of `outer` in scope; its new bindings are the subquery's locals.
    #[inline(never)]
    fn whole_query_body(&mut self, query: &CompositeQuery, outer: &Names) -> BResult<SubqueryBody> {
        self.enter_body(query.span)?;
        let body = self.composite(query, Entry::Subquery { outer });
        self.body_depth = self.body_depth.saturating_sub(1);
        let (bound, names) = body?;
        Ok((Vec::new(), None, Some((bound, names))))
    }

    /// One more nested body ([`MAX_BODY_DEPTH`]); the caller leaves it.
    fn enter_body(&mut self, span: Span) -> BResult<()> {
        if self.body_depth >= MAX_BODY_DEPTH {
            return Err(Box::new(
                GqlErrorBuilder::new(
                    GqlErrorKind::Limit,
                    span.start..span.end,
                    format!(
                        "the query nests CALL {{ }} and subquery bodies deeper than \
                         {MAX_BODY_DEPTH} levels"
                    ),
                )
                .alternative(
                    "un-nest the bodies: a CALL body may follow another in the same query, as \
                     in CALL { .. RETURN a } CALL (a) { .. RETURN b }, and NEXT passes the \
                     result of one query to the next",
                ),
            ));
        }
        self.body_depth += 1;
        Ok(())
    }

    /// The `MATCH` clauses of a subquery body, bound like the clauses of one `Segment::Match`
    /// (each with its own edge uniqueness, design E8); returns their indexes.
    fn subquery_body(&mut self, subquery: &SubqueryExpr, segment: usize) -> BResult<Vec<usize>> {
        let base = SUBQUERY_CLAUSES;
        let mut clauses = Vec::with_capacity(subquery.matches.len());
        let outer = self.later_in_body.len();
        for (i, m) in subquery.matches.iter().enumerate() {
            self.later_in_body.truncate(outer);
            let later = subquery.matches.get(i.saturating_add(1)..).unwrap_or(&[]);
            self.later_in_body.extend(
                later
                    .iter()
                    .flat_map(declared_names)
                    .map(|var| var.name.to_string()),
            );
            let slot = self.sub_matches.len();
            let clause = base.saturating_add(slot);
            self.sub_matches.push(None);
            self.clause = clause;
            let bound = self.declare_clause(clause, m, segment)?;
            self.clause_filters(clause, m, &bound)?;
            if let Some(entry) = self.sub_matches.get_mut(slot) {
                *entry = Some(bound);
            }
            clauses.push(clause);
        }
        self.later_in_body.truncate(outer);
        Ok(clauses)
    }

    /// A pattern predicate may not introduce a named variable (design 6): every name it writes
    /// must be in scope.
    fn check_pattern_predicate(&self, ctx: Ctx<'_>, subquery: &SubqueryExpr) -> BResult<()> {
        for path in subquery.matches.iter().flat_map(|m| &m.patterns) {
            let names = path.variable.iter().chain(&path.start.variable).chain(
                path.steps
                    .iter()
                    .flat_map(|step| step.edge.variable.iter().chain(&step.node.variable)),
            );
            for var in names {
                if self.scope.get(&var.name).is_some() {
                    continue;
                }
                let name = &var.name;
                if self.pending_let.iter().any(|n| n == name)
                    || self.dropped.contains_key(name)
                    || self.hidden.contains_key(name)
                {
                    return Err(self.undefined(ctx, var));
                }
                let pattern = self.query.text(subquery.span);
                return Err(semantic(
                    var.span,
                    format!(
                        "the pattern predicate introduces the new variable `{name}`; a pattern \
                         used as a condition may only name variables bound before it"
                    ),
                )
                .alternative(format!(
                    "write it as EXISTS {{ MATCH {pattern} }}, whose MATCH may bind new \
                     variables, or leave `{name}` out of the pattern"
                ))
                .topic(GqlHelpTopic::Subquery)
                .into());
            }
        }
        Ok(())
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
                        .topic(GqlHelpTopic::Scope)
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
                .topic(GqlHelpTopic::Names)
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

    /// `GROUP BY` of `RETURN` (module docs): checks the keys against the columns; returns
    /// whether the rows must be made distinct (keys listed and no column aggregates).
    fn group_by(&self, group_by: Option<&GroupBy>, columns: &[Column]) -> BResult<bool> {
        let Some(group_by) = group_by else {
            return Ok(false);
        };
        let mut listed = vec![false; columns.len()];
        for key in &group_by.keys {
            let text = self.query.text(key.span);
            let found = columns.iter().position(|c| {
                let by_name = match &key.kind {
                    ExprKind::Variable(var) => c.name == var.name,
                    _ => c.name == text,
                };
                by_name || (!text.is_empty() && self.query.text(c.written) == text)
            });
            let Some(index) = found else {
                return Err(semantic(
                    key.span,
                    format!(
                        "`{text}` is not a RETURN item: GROUP BY lists the RETURN items without \
                         an aggregate"
                    ),
                )
                .alternative(format!(
                    "return it as an item too (RETURN {text}, ...) and keep it in GROUP BY, or \
                     remove it from GROUP BY"
                ))
                .topic(GqlHelpTopic::Grouping)
                .into());
            };
            if columns.get(index).is_some_and(|c| c.aggregate) {
                return Err(semantic(
                    key.span,
                    format!(
                        "`{text}` is an aggregating RETURN item, and GROUP BY lists only the items \
                         without an aggregate"
                    ),
                )
                .alternative(format!("remove `{text}` from GROUP BY"))
                .topic(GqlHelpTopic::Grouping)
                .into());
            }
            if let Some(flag) = listed.get_mut(index) {
                *flag = true;
            }
        }
        let missing = columns
            .iter()
            .zip(&listed)
            .find(|(c, listed)| !c.aggregate && !**listed);
        if let Some((column, _)) = missing {
            let name = &column.name;
            let message = if group_by.keys.is_empty() {
                format!(
                    "GROUP BY () groups all rows into one, so every RETURN item must aggregate, \
                     and `{name}` does not"
                )
            } else {
                format!(
                    "the RETURN item `{name}` has no aggregate, so it is a grouping key, and \
                     GROUP BY does not list it"
                )
            };
            return Err(semantic(column.span, message)
                .alternative(format!(
                    "add it to GROUP BY (GROUP BY ..., {name}), or remove GROUP BY: grouping is \
                     implicit, the items without an aggregate are the keys"
                ))
                .topic(GqlHelpTopic::Grouping)
                .into());
        }
        Ok(!group_by.keys.is_empty() && !columns.iter().any(|c| c.aggregate))
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
            ExprKind::Subquery(_) | ExprKind::PatternComprehension(_) => {
                let span = match &expr.kind {
                    ExprKind::Subquery(subquery) => subquery.span,
                    _ => expr.span,
                };
                let imports = self
                    .subqueries
                    .iter()
                    .find(|s| s.span == span)
                    .map_or(&[][..], |s| s.imports.as_slice());
                let outside = imports.iter().find(|id| !key_variables.contains(id));
                let Some(binding) = outside.and_then(|id| self.bindings.get(id.0)) else {
                    return Ok(());
                };
                let name = binding.name.as_ref().map_or("", |n| n.name.as_str());
                Err(semantic(
                    span,
                    format!(
                        "the subquery reads `{name}` in an item that aggregates, outside the \
                         aggregates, so `{name}` must be a grouping key of {clause}, and it is \
                         not one"
                    ),
                )
                .alternative(format!(
                    "add `{name}` to the {clause} items (it then becomes a grouping key), or move \
                     the subquery into a column of its own"
                ))
                .topic(GqlHelpTopic::Grouping)
                .into())
            }
            ExprKind::Variable(var) => self.grouped_variable(var, key_variables, clause),
            // In written order; a variable item `x` reads `x` like a variable.
            ExprKind::MapProjection(projection) => {
                self.grouped(&projection.target, key_variables, key_texts, clause)?;
                for item in &projection.items {
                    match item {
                        ProjectionItem::Variable(var) => {
                            self.grouped_variable(var, key_variables, clause)?;
                        }
                        ProjectionItem::Entry(entry) => {
                            self.grouped(&entry.value, key_variables, key_texts, clause)?;
                        }
                        ProjectionItem::Property(_) | ProjectionItem::AllProperties(_) => {}
                    }
                }
                Ok(())
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

    /// A variable read outside the aggregates of an aggregating item must be a grouping key (a
    /// comprehension variable is in no scope: it reads nothing of the row).
    fn grouped_variable(
        &self,
        var: &Identifier,
        key_variables: &[BindingId],
        clause: &'static str,
    ) -> BResult<()> {
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
        .topic(GqlHelpTopic::Grouping)
        .into())
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
                .topic(GqlHelpTopic::Grouping)
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
                    .topic(GqlHelpTopic::Grouping)
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

/// The parts of a `WITH`, or of a `RETURN` before `NEXT` read as one (design E19).
struct WithParts<'w> {
    distinct: bool,
    star: Option<Span>,
    items: &'w [ReturnItem],
    /// The `GROUP BY` of a `RETURN`.
    group_by: Option<&'w GroupBy>,
    order_by: Option<&'w OrderBy>,
    offset: Option<&'w CountClause>,
    limit: Option<&'w CountClause>,
    where_clause: Option<&'w Expr>,
    span: Span,
    /// `WITH` or `RETURN` (for the messages).
    clause: &'static str,
    /// What drops the names it does not carry.
    dropper: Dropper,
}

impl<'w> WithParts<'w> {
    fn of(with: &'w WithClause) -> Self {
        WithParts {
            distinct: with.distinct,
            star: with.star,
            items: &with.items,
            group_by: None,
            order_by: with.order_by.as_ref(),
            offset: with.offset.as_ref(),
            limit: with.limit.as_ref(),
            where_clause: with.where_clause.as_ref(),
            span: with.span,
            clause: "WITH",
            dropper: Dropper::With,
        }
    }
}

/// The internal error of a substitution helper handed a form it does not handle.
fn not_dispatched() -> Box<GqlErrorBuilder> {
    Box::new(GqlErrorBuilder::new(
        GqlErrorKind::Semantic,
        0..0,
        "internal error: an expression form substituted by the wrong helper",
    ))
}

/// The names of `outer` that `variables` import, in written order (each checked to be there).
fn imported(outer: &Names, variables: &[Identifier]) -> Names {
    let mut names = Names::default();
    for var in variables {
        if let Some(symbol) = outer.get(&var.name) {
            names.insert(var.clone(), symbol.clone());
        }
    }
    names
}

/// `UNION ALL` or `UNION`.
fn union_word(kind: UnionKind) -> &'static str {
    match kind {
        UnionKind::All => "UNION ALL",
        UnionKind::Distinct => "UNION",
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
    Box::new(semantic(var.span, message).topic(GqlHelpTopic::PatternVariable))
}

/// `ORDINALITY` / `OFFSET` as written.
fn ordinal_word(kind: OrdinalKind) -> &'static str {
    match kind {
        OrdinalKind::Ordinality => "ORDINALITY",
        OrdinalKind::Offset => "OFFSET",
    }
}

/// The error of a `FOR` ordinal written as a node or edge variable.
fn ordinal_as_element(var: &Identifier, here: BindingKind) -> Box<GqlErrorBuilder> {
    let name = &var.name;
    Box::new(
        semantic(
            var.span,
            format!(
                "the variable `{name}` is the position of a FOR item (a number), not a {}",
                kind_word(here)
            ),
        )
        .alternative(format!(
            "give the pattern element its own name, as in ({name}2), and compare the position \
             in WHERE"
        ))
        .topic(GqlHelpTopic::For),
    )
}

/// The error of an aggregate call where none may stand.
/// The place of an aggregate inside a list comprehension's `WHERE` / `|` part (refused).
const IN_COMPREHENSION: &str = "inside the WHERE or | part of a list comprehension";

/// The place of an aggregate inside the `WHERE` of `all()` / `any()` / `none()` / `single()`.
const IN_LIST_PREDICATE: &str = "inside the WHERE part of all(), any(), none() or single()";

/// The place of an aggregate inside the `|` part of `reduce()`.
const IN_REDUCE: &str = "inside the | part of reduce()";

const IN_PATTERN_COMPREHENSION: &str = "inside the | part of a pattern comprehension";

/// A comprehension variable named inside a subquery body (not yet: the body is a child
/// traversal planned apart from the item being iterated).
fn local_in_subquery(var: &Identifier) -> Box<GqlErrorBuilder> {
    let name = &var.name;
    not_yet(
        var.span,
        &format!("reading the comprehension variable `{name}` inside a subquery"),
        &format!(
            "test `{name}` outside the subquery, as in [{name} IN list WHERE {name} > 0 AND \
             EXISTS {{ MATCH (n)-->() }}]"
        ),
    )
}

fn misplaced_aggregate(name: &Identifier, place: &str) -> Box<GqlErrorBuilder> {
    let f = &name.name;
    let alternative = if place == "inside another aggregate" {
        format!(
            "aggregate in a WITH first and aggregate its column in RETURN, as in \
             MATCH (p)-->(f) WITH p, {f}(f) AS n RETURN max(n)"
        )
    } else if place == IN_PATTERN_COMPREHENSION {
        format!(
            "aggregate the list the comprehension builds, as in \
             MATCH (p) RETURN size([(p)-->(f) | f.age]) AS n, or aggregate in a WITH after an \
             OPTIONAL MATCH, as in MATCH (p) OPTIONAL MATCH (p)-->(f) RETURN p, {f}(f.age) AS n"
        )
    } else if place == IN_COMPREHENSION {
        format!(
            "aggregate in a WITH first and use its column in the comprehension, as in \
             MATCH (p)-->(f) WITH p, {f}(f.age) AS n RETURN [x IN [1, 2, 3] WHERE x < n]"
        )
    } else if place == IN_LIST_PREDICATE {
        format!(
            "aggregate in a WITH first and use its column in the condition, as in \
             MATCH (p)-->(f) WITH p, {f}(f.age) AS n RETURN all(x IN [1, 2, 3] WHERE x < n)"
        )
    } else if place == IN_REDUCE {
        format!(
            "aggregate in a WITH first and use its column in the | part, as in \
             MATCH (p)-->(f) WITH p, {f}(f.age) AS n RETURN reduce(s = 0, x IN [1, 2, 3] | s + \
             x * n)"
        )
    } else if place == "in FOR" || place == "in UNWIND" {
        format!(
            "aggregate in a WITH first and iterate over its column, as in \
             MATCH (p)-->(f) WITH p, {f}(f) AS fs FOR x IN fs RETURN p, x"
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
        .alternative(alternative)
        .topic(GqlHelpTopic::Grouping),
    )
}

/// The variables `m` names: path, node and edge variables of every path.
fn declared_names(m: &MatchClause) -> impl Iterator<Item = &Identifier> {
    m.patterns.iter().flat_map(|path| {
        path.variable.iter().chain(&path.start.variable).chain(
            path.steps
                .iter()
                .flat_map(|s| s.edge.variable.iter().chain(&s.node.variable)),
        )
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
        // The left operand of a binary operator (or of `IS SOURCE OF`) is a link of the spine,
        // the right one nests.
        let binary_left = matches!(
            expr.kind,
            ExprKind::Binary { .. } | ExprKind::IsEndpoint { .. }
        ) && first;
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
            .topic(GqlHelpTopic::Names)
            .into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
