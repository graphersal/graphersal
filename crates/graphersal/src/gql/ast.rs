//! The abstract syntax tree of a GQL read query.
//!
//! Every node carries the [`Span`] of the text it was parsed from, and [`Query`] keeps the
//! source text, so every later stage (binder, planner, lowering, errors) can point at the
//! offending text and derive column names from it ([`Query::text`]).
//!
//! Stage 1a covers `MATCH` (comma-separated path patterns, optional `WHERE`), `RETURN`
//! (`DISTINCT`, items with `AS`, `*`), `ORDER BY`, `OFFSET` / `SKIP`, `LIMIT`. Stage 1b adds the
//! linear pipeline of clauses before `RETURN` ([`LinearQuery::stages`]: `[OPTIONAL] MATCH`, `WITH`,
//! `LET`, `FILTER`), quantified edges ([`EdgePattern::quantifier`]), path variables
//! ([`PathPattern::variable`]) and aggregate calls (an [`ExprKind::FunctionCall`] of an aggregate
//! function, `DISTINCT` and `count(*)` included). A query written in the 1a subset has exactly
//! the 1a tree: every `MATCH` a non-optional [`Stage::Match`], no path variable, no quantifier.
//! Stage 2a adds `FOR` / Cypher `UNWIND` ([`Stage::For`]) and queries that start with `FOR`,
//! `LET`, `WITH`, `FILTER` or `RETURN` (no `MATCH`: [`LinearQuery::stages`] may be empty or start
//! with another stage).
//! Stage 2b adds the `EXISTS` predicate and Cypher pattern predicates ([`ExprKind::Subquery`]:
//! `EXISTS { MATCH .. }`, `EXISTS { pattern }`, `EXISTS ( .. )`, `(a)-[:T]->(b)` where an
//! expression stands); the body holds `MATCH` clauses only.
//! Stage 2c adds the openCypher list and map expressions: map projections
//! ([`ExprKind::MapProjection`], `v {.name, .*, x, k: expr}`), list comprehensions
//! ([`ExprKind::ListComprehension`], `[x IN list WHERE cond | expr]`) and pattern comprehensions
//! ([`ExprKind::PatternComprehension`], `[(a)-[:T]->(b) WHERE cond | expr]`).
//! Stage 2d adds composite queries and inline subqueries: a [`Query`] is a [`CompositeQuery`],
//! blocks of [`LinearQuery`] operands joined by `UNION [ALL | DISTINCT]` ([`QueryBlock`]),
//! chained by `NEXT` ([`NextBlock`]), and a pipeline stage may be `[OPTIONAL] CALL (vars | *)
//! { query }` ([`Stage::Call`], [`CallClause`]; Cypher's importing `CALL { WITH a .. }` as
//! [`LinearQuery::importing`]). A linear query has exactly the tree of the stages before:
//! [`Query`] dereferences to its first linear query, which is the whole query then.

use super::lexer::Span;

/// A query: a [`CompositeQuery`] and the text it was parsed from. It dereferences to its first
/// [`LinearQuery`], which is the whole query when [`Query::is_linear`] holds (every query of the
/// stages before 2d).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Query {
    /// The query text as given (spans index into it).
    pub(crate) source: String,
    /// The statements: one linear query, or `UNION` / `NEXT` compositions of several.
    pub(crate) body: Box<CompositeQuery>,
}

impl Query {
    /// The source text of `span` (empty when the span does not fit the source, never a panic).
    pub(crate) fn text(&self, span: Span) -> &str {
        self.source.get(span.start..span.end).unwrap_or("")
    }

    /// Whether the query is one linear query (no `UNION`, no `NEXT`).
    #[cfg_attr(not(test), allow(dead_code, reason = "used by the tests"))]
    pub(crate) fn is_linear(&self) -> bool {
        self.body.is_linear()
    }
}

impl std::ops::Deref for Query {
    type Target = LinearQuery;

    /// The first linear query (the whole query when it is linear).
    fn deref(&self) -> &LinearQuery {
        &self.body.first.first
    }
}

/// `block (NEXT block)*` (stage 2d): the top level of a query and the body of a `CALL`. `NEXT`
/// binds looser than `UNION`, so each block is a whole `UNION` chain.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CompositeQuery {
    /// The first block.
    pub(crate) first: QueryBlock,
    /// The `NEXT` blocks in text order, each reading the result of the one before.
    pub(crate) next: Vec<NextBlock>,
    /// From the first clause to the end of the last block (a trailing `;` excluded).
    pub(crate) span: Span,
}

impl CompositeQuery {
    /// Whether it is one linear query (no `UNION`, no `NEXT`).
    #[cfg_attr(not(test), allow(dead_code, reason = "used by the tests"))]
    pub(crate) fn is_linear(&self) -> bool {
        self.next.is_empty() && self.first.unions.is_empty()
    }
}

/// `linear (UNION [ALL | DISTINCT] linear)*` (stage 2d).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct QueryBlock {
    /// The first operand.
    pub(crate) first: LinearQuery,
    /// The further operands in text order, each with the `UNION` before it.
    pub(crate) unions: Vec<UnionOperand>,
    /// From the first operand to the end of the last.
    pub(crate) span: Span,
}

/// One `UNION [ALL | DISTINCT] linear` of a [`QueryBlock`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct UnionOperand {
    /// Whether duplicates are kept (`UNION ALL`) or removed (`UNION`, `UNION DISTINCT`).
    pub(crate) kind: UnionKind,
    /// The keywords: `UNION`, `UNION ALL` or `UNION DISTINCT`.
    pub(crate) keyword: Span,
    /// The operand after them.
    pub(crate) query: LinearQuery,
}

/// The set semantics of a `UNION`. Closed by definition: GQL's `UNION` has exactly these two
/// forms (a bare `UNION` is `UNION DISTINCT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnionKind {
    /// `UNION` or `UNION DISTINCT`: duplicate records removed.
    Distinct,
    /// `UNION ALL`: every record kept.
    All,
}

/// `NEXT block` (stage 2d): the block reads the records the block before returned.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NextBlock {
    /// The `NEXT` keyword.
    pub(crate) keyword: Span,
    /// The block after it.
    pub(crate) block: QueryBlock,
}

/// A linear query: a pipeline of clauses, then the result statement.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LinearQuery {
    /// Every `[OPTIONAL] MATCH` clause of this linear query in text order ([`Stage::Match`]
    /// indexes into it; a `CALL` body holds its own).
    pub(crate) matches: Vec<MatchClause>,
    /// The clauses before `RETURN` in text order (empty for a query that is only a result
    /// statement). A 1a / 1b query starts with a [`Stage::Match`]; a 2a query may start with
    /// [`Stage::For`], [`Stage::Let`], [`Stage::With`] or [`Stage::Filter`]. A 1a query
    /// holds one `Stage::Match(i)` per entry of `matches`, in order.
    pub(crate) stages: Vec<Stage>,
    /// `RETURN` (mandatory).
    pub(crate) ret: ReturnClause,
    /// `ORDER BY ...`.
    pub(crate) order_by: Option<OrderBy>,
    /// `OFFSET n` or its Cypher alias `SKIP n`.
    pub(crate) offset: Option<CountClause>,
    /// `LIMIT n`.
    pub(crate) limit: Option<CountClause>,
    /// Cypher's importing `WITH` (stage 2d): set only on an operand of the first block of a
    /// `CALL` body written without a variable scope clause ([`CallScope::Implicit`]) whose first
    /// clause is `WITH` of plain variables or `*` alone (no alias, expression, `DISTINCT`,
    /// `ORDER BY`, `OFFSET`, `LIMIT` or `WHERE`). That `WITH` is not in [`Self::stages`]: it
    /// names the outer variables the operand imports.
    pub(crate) importing: Option<ImportingWith>,
    /// The whole linear query.
    pub(crate) span: Span,
}

/// Cypher's importing `WITH a, b` / `WITH *` at the start of a `CALL { }` body (an alias of the
/// GQL variable scope clause `CALL (a, b) { }` / `CALL (*) { }`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ImportingWith {
    /// `WITH *`: every variable in scope.
    pub(crate) star: Option<Span>,
    /// The imported variables in written order (empty with `*`).
    pub(crate) variables: Vec<Identifier>,
    /// From `WITH` to its last item.
    pub(crate) span: Span,
}

/// A name as written: an identifier, a back-quoted identifier (unescaped) or a keyword used as a
/// name.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Identifier {
    /// The name.
    pub(crate) name: String,
    /// Where it stands (back-quotes included).
    pub(crate) span: Span,
}

/// One clause of the pipeline before `RETURN`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Stage {
    /// `[OPTIONAL] MATCH`: the index of the clause in [`LinearQuery::matches`].
    Match(usize),
    /// `WITH ...` (Cypher's form of the GQL linear pipeline).
    With(Box<WithClause>),
    /// `LET v = expr, ...`.
    Let(LetClause),
    /// `FILTER [WHERE] expr`.
    Filter(FilterClause),
    /// `FOR x IN expr [WITH ORDINALITY i | WITH OFFSET i]`, Cypher `UNWIND expr AS x`.
    For(ForClause),
    /// `[OPTIONAL] CALL (vars | *) { query }` and Cypher `CALL { .. }` (stage 2d).
    Call(Box<CallClause>),
}

/// `[OPTIONAL] CALL [(vars) | (*)] { query }`: an inline subquery run once per incoming row
/// (stage 2d, design item 8).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CallClause {
    /// `OPTIONAL CALL`: the `OPTIONAL` keyword.
    pub(crate) optional: Option<Span>,
    /// The variable scope clause.
    pub(crate) scope: CallScope,
    /// The body: a whole query ending in `RETURN` (the parser refuses a body without one).
    pub(crate) body: Box<CompositeQuery>,
    /// From `OPTIONAL` (or `CALL`) to the closing `}`.
    pub(crate) span: Span,
}

/// The variable scope clause of a [`CallClause`]: which outer variables the body reads.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CallScope {
    /// `CALL (a, b) { }`, `CALL () { }`: exactly these variables (none for `()`).
    Variables {
        /// The imported variables in written order.
        variables: Vec<Identifier>,
        /// From `(` to `)`.
        span: Span,
    },
    /// `CALL (*) { }`: every variable in scope (the span of `(*)`).
    All(Span),
    /// `CALL { }` without a scope clause: imports nothing (Neo4j Cypher 25), except what a
    /// Cypher importing `WITH` names ([`LinearQuery::importing`]).
    Implicit,
}

/// `[OPTIONAL] MATCH pattern, pattern, ... [WHERE expr]`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MatchClause {
    /// `OPTIONAL MATCH`.
    pub(crate) optional: bool,
    /// The comma-separated path patterns; repeated variables join them.
    pub(crate) patterns: Vec<PathPattern>,
    /// The clause's `WHERE`.
    pub(crate) where_clause: Option<Expr>,
    /// From `OPTIONAL` (or `MATCH`) to the end of the clause.
    pub(crate) span: Span,
}

/// `WITH [DISTINCT] items [ORDER BY ..] [OFFSET|SKIP n] [LIMIT n] [WHERE expr]`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WithClause {
    /// `DISTINCT`.
    pub(crate) distinct: bool,
    /// The `*` item (every variable in scope), when written.
    pub(crate) star: Option<Span>,
    /// The explicit items in written order.
    pub(crate) items: Vec<ReturnItem>,
    /// `ORDER BY ...`.
    pub(crate) order_by: Option<OrderBy>,
    /// `OFFSET n` or `SKIP n`.
    pub(crate) offset: Option<CountClause>,
    /// `LIMIT n`.
    pub(crate) limit: Option<CountClause>,
    /// The `WHERE` after the projection (reads the projected names).
    pub(crate) where_clause: Option<Expr>,
    /// From `WITH` to the end of the clause.
    pub(crate) span: Span,
}

/// `LET v = expr, w = expr, ...`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LetClause {
    /// The definitions in written order (at least one).
    pub(crate) bindings: Vec<LetBinding>,
    /// From `LET` to the last definition.
    pub(crate) span: Span,
}

/// One `v = expr` of a [`LetClause`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LetBinding {
    /// The new variable.
    pub(crate) variable: Identifier,
    /// Its value.
    pub(crate) value: Expr,
    /// From the variable to the end of the value.
    pub(crate) span: Span,
}

/// `FILTER [WHERE] expr`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FilterClause {
    /// The condition.
    pub(crate) condition: Expr,
    /// From `FILTER` to the end of the condition.
    pub(crate) span: Span,
}

/// `FOR x IN expr [WITH ORDINALITY i | WITH OFFSET i]` or Cypher `UNWIND expr AS x` (one
/// meaning for both spellings: one row per list item).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ForClause {
    /// The new variable bound to each item.
    pub(crate) variable: Identifier,
    /// The list expression.
    pub(crate) list: Expr,
    /// `WITH ORDINALITY i` / `WITH OFFSET i` (GQL only).
    pub(crate) ordinal: Option<Ordinal>,
    /// Written as Cypher `UNWIND expr AS x`.
    pub(crate) unwind: bool,
    /// From `FOR` (or `UNWIND`) to the end of the clause.
    pub(crate) span: Span,
}

/// The position variable of a [`ForClause`]: `WITH ORDINALITY i` or `WITH OFFSET i`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Ordinal {
    /// Which base the position counts from.
    pub(crate) kind: OrdinalKind,
    /// The new variable bound to the position.
    pub(crate) variable: Identifier,
    /// From `WITH` to the variable.
    pub(crate) span: Span,
}

/// The base of an [`Ordinal`]. Closed by definition: GQL defines exactly these two forms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OrdinalKind {
    /// `WITH ORDINALITY i`: 1, 2, ...
    Ordinality,
    /// `WITH OFFSET i`: 0, 1, ...
    Offset,
}

/// A path pattern: `[p =] node (edge node)*`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PathPattern {
    /// The path variable of `p = ...`.
    pub(crate) variable: Option<Identifier>,
    /// The first node.
    pub(crate) start: NodePattern,
    /// The following edges, each with the node it leads to.
    pub(crate) steps: Vec<PatternStep>,
    /// The whole pattern (the `p =` of a path variable included).
    pub(crate) span: Span,
}

/// One edge of a path pattern and the node after it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PatternStep {
    /// The edge.
    pub(crate) edge: EdgePattern,
    /// The node it leads to.
    pub(crate) node: NodePattern,
}

/// `(v:Label {k: expr} WHERE expr)`, every part optional.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NodePattern {
    /// The element variable.
    pub(crate) variable: Option<Identifier>,
    /// The label expression after `:` (or `IS`).
    pub(crate) labels: Option<LabelExpr>,
    /// The inline property map.
    pub(crate) properties: Option<MapLiteral>,
    /// The inline `WHERE`.
    pub(crate) where_clause: Option<Expr>,
    /// From `(` to `)`.
    pub(crate) span: Span,
}

/// The direction of an edge pattern. Undirected GQL edges (`~`) are refused: Graphersal edges
/// are always directed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EdgeDirection {
    /// `-[..]->`, `-->`.
    Right,
    /// `<-[..]-`, `<--`.
    Left,
    /// `-[..]-`, `--`, `-` (either direction; also GQL `<-[..]->`, `<->` and Cypher `<-->`).
    Any,
}

/// `-[e:Label {k: expr} WHERE expr]->` or an abbreviated form, possibly quantified.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EdgePattern {
    /// The direction.
    pub(crate) direction: EdgeDirection,
    /// Whether it is written without brackets (`-->`, `<--`, `--`, `<-->`, `->`, `<-`, `-`,
    /// `<->`).
    pub(crate) abbreviated: bool,
    /// The element variable.
    pub(crate) variable: Option<Identifier>,
    /// The label expression.
    pub(crate) labels: Option<LabelExpr>,
    /// The inline property map.
    pub(crate) properties: Option<MapLiteral>,
    /// The inline `WHERE`.
    pub(crate) where_clause: Option<Expr>,
    /// The quantifier: GQL `{m,n}`, `*`, `+` after the edge, or Cypher `*m..n` inside its
    /// brackets.
    pub(crate) quantifier: Option<Quantifier>,
    /// The whole edge, arrows included (a GQL quantifier after the edge is not part of it).
    pub(crate) span: Span,
}

/// How often a quantified edge repeats: `min..=max` times (`max` `None`: unbounded). The
/// parser normalizes every spelling (module docs of `parser.rs`) and guarantees `min <= max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Quantifier {
    /// The fewest repetitions (0 allows the zero-length match).
    pub(crate) min: u64,
    /// The most repetitions; `None` for no upper bound.
    pub(crate) max: Option<u64>,
    /// Written in Cypher's bracket form (`[e*m..n]`, lower bound 1 by default), not GQL's.
    pub(crate) cypher: bool,
    /// The quantifier text (`{1,3}`, `*`, `+`, or `*1..3` inside the brackets).
    pub(crate) span: Span,
}

/// A label expression.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LabelExpr {
    /// What it is.
    pub(crate) kind: LabelExprKind,
    /// Its text (parentheses included when written).
    pub(crate) span: Span,
}

/// The kinds of [`LabelExpr`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum LabelExprKind {
    /// A label name.
    Label(Identifier),
    /// `%`: any label (the label set is not empty).
    Wildcard,
    /// `!expr`.
    Not(Box<LabelExpr>),
    /// `a & b`; also Cypher `:A:B`, whose span covers `A:B`.
    And(Box<LabelExpr>, Box<LabelExpr>),
    /// `a | b`.
    Or(Box<LabelExpr>, Box<LabelExpr>),
}

/// `{key: expr, ...}`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MapLiteral {
    /// The entries in written order.
    pub(crate) entries: Vec<MapEntry>,
    /// From `{` to `}`.
    pub(crate) span: Span,
}

/// One `key: value` entry of a [`MapLiteral`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MapEntry {
    /// The key.
    pub(crate) key: Identifier,
    /// The value.
    pub(crate) value: Expr,
}

/// An expression.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Expr {
    /// What it is.
    pub(crate) kind: ExprKind,
    /// Its text (the default column name of a `RETURN` item).
    pub(crate) span: Span,
}

/// A literal value.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Literal {
    /// `NULL`.
    Null,
    /// `TRUE` / `FALSE`.
    Bool(bool),
    /// An integer (the parser folds a leading minus, so `i64::MIN` is reachable).
    Integer(i64),
    /// A float.
    Float(f64),
    /// A string (unescaped).
    String(String),
}

/// The kinds of [`Expr`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ExprKind {
    /// A literal.
    Literal(Literal),
    /// `[a, b, ...]`.
    List(Vec<Expr>),
    /// `{k: v, ...}`.
    Map(MapLiteral),
    /// `$name`.
    Parameter(String),
    /// A variable reference.
    Variable(Identifier),
    /// `target.key`.
    Property {
        /// The expression whose property is read.
        target: Box<Expr>,
        /// The property key.
        key: Identifier,
    },
    /// A prefix operator.
    Unary {
        /// The operator.
        op: UnaryOp,
        /// Its operand.
        operand: Box<Expr>,
    },
    /// An infix operator.
    Binary {
        /// The operator.
        op: BinaryOp,
        /// Left operand.
        left: Box<Expr>,
        /// Right operand.
        right: Box<Expr>,
    },
    /// `expr IS [NOT] NULL`.
    IsNull {
        /// The tested expression.
        operand: Box<Expr>,
        /// `IS NOT NULL`.
        negated: bool,
    },
    /// `expr IS [NOT] LABELED labels`; also Cypher `v:A` in an expression.
    IsLabeled {
        /// The tested expression.
        operand: Box<Expr>,
        /// The label expression.
        labels: LabelExpr,
        /// `IS NOT LABELED`.
        negated: bool,
    },
    /// `CASE ... END`.
    Case(Box<CaseExpr>),
    /// `expr IS [NOT] SOURCE OF edge` / `expr IS [NOT] DESTINATION OF edge` (GQL feature G112):
    /// whether the vertex is the stored source / target of the edge.
    IsEndpoint {
        /// The tested expression (a vertex).
        operand: Box<Expr>,
        /// The edge.
        edge: Box<Expr>,
        /// `SOURCE` or `DESTINATION`.
        end: Endpoint,
        /// `IS NOT ...`.
        negated: bool,
    },
    /// `name([DISTINCT] args)` or `name(*)`.
    FunctionCall {
        /// The function name as written.
        name: Identifier,
        /// `DISTINCT` before the arguments (aggregates).
        distinct: bool,
        /// The arguments.
        args: CallArgs,
    },
    /// A subquery in an expression: `EXISTS { .. }`, `EXISTS ( .. )` or a Cypher pattern
    /// predicate (stage 2b).
    Subquery(Box<SubqueryExpr>),
    /// `target {.key, .*, x, key: expr}`: a Cypher map projection (stage 2c).
    MapProjection(Box<MapProjection>),
    /// `[x IN list WHERE cond | expr]`: a Cypher list comprehension (stage 2c).
    ListComprehension(Box<ListComprehension>),
    /// `[(a)-[:T]->(b) WHERE cond | expr]`: a Cypher pattern comprehension (stage 2c).
    PatternComprehension(Box<PatternComprehension>),
}

/// A map projection `target {items}` (stage 2c, design item 7): a map built from the
/// properties of a vertex, an edge or a map, and from further entries.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MapProjection {
    /// The projected value: a variable as written (the binder may substitute a `LET` / `WITH`
    /// name by its expression).
    pub(crate) target: Expr,
    /// The items in written order (possibly none: `v {}`).
    pub(crate) items: Vec<ProjectionItem>,
    /// From `{` to `}`.
    pub(crate) braces: Span,
}

/// One item of a [`MapProjection`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ProjectionItem {
    /// `.key`: the target's property or entry `key`, under that key.
    Property(Identifier),
    /// `.*`: every property or entry of the target (the span of `.*`).
    AllProperties(Span),
    /// `x`: the variable's value under its name.
    Variable(Identifier),
    /// `key: expr`.
    Entry(MapEntry),
}

/// A list comprehension `[x IN list WHERE cond | expr]` (stage 2c): the items of `list` for
/// which `cond` is TRUE, each mapped through `expr`. `variable` is local to `filter` and `map`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ListComprehension {
    /// The item variable.
    pub(crate) variable: Identifier,
    /// The list iterated over.
    pub(crate) list: Expr,
    /// The `WHERE` condition.
    pub(crate) filter: Option<Expr>,
    /// The expression after `|`; the item itself when absent.
    pub(crate) map: Option<Expr>,
}

/// A pattern comprehension `[p = (a)-[:T]->(b) WHERE cond | expr]` (stage 2c): `expr` for every
/// match of the pattern, as a list. Like a subquery (stage 2b), the names bound before it are
/// imports and the new ones are local to it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PatternComprehension {
    /// The pattern (one path with at least one edge, a path variable allowed) and its `WHERE`,
    /// as one non-optional `MATCH` clause: the shape of a subquery body
    /// ([`SubqueryExpr::matches`]). Its span covers the pattern and the `WHERE`.
    pub(crate) body: MatchClause,
    /// The expression after `|`.
    pub(crate) map: Expr,
}

/// A subquery in an expression (stage 2b, design item 6): its body is bound as a nested query
/// whose names are the outer ones (imports) or local ones.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SubqueryExpr {
    /// What the subquery computes.
    pub(crate) kind: SubqueryKind,
    /// How it was written.
    pub(crate) form: SubqueryForm,
    /// The body: one or more non-optional `MATCH` clauses, each with its own `WHERE`. The
    /// pattern forms (`EXISTS { pattern, .. WHERE c }`, a pattern predicate) are one clause
    /// whose span is the pattern list (and its `WHERE`). Empty when [`Self::query`] holds the
    /// body.
    pub(crate) matches: Vec<MatchClause>,
    /// A whole-query body (stage 2d): `{ .. }` holding other clauses than `MATCH` (`OPTIONAL
    /// MATCH`, `WITH`, `LET`, `FILTER`, `FOR`, `CALL`, ...), ending in `RETURN`, possibly a
    /// `UNION` / `NEXT` composition. `None` for a body of `MATCH` clauses only, which is
    /// [`Self::matches`] exactly as in stage 2b.
    pub(crate) query: Option<Box<CompositeQuery>>,
    /// From `EXISTS` (`COUNT`, `COLLECT`) to the closing bracket; for a pattern predicate the
    /// pattern.
    pub(crate) span: Span,
}

/// What a [`SubqueryExpr`] computes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubqueryKind {
    /// TRUE when the body has at least one match, else FALSE (never `NULL`).
    Exists,
    /// The list of one value per match of the body: a [`PatternComprehension`] (stage 2c; the
    /// binder binds it as a subquery of this kind) or the openCypher `COLLECT { .. RETURN x }`
    /// (stage 2d, [`SubqueryForm::Braces`]).
    Collect,
    /// The openCypher `COUNT { .. }` (stage 2d): the number of matches (or of returned rows) of
    /// the body.
    Count,
}

/// The spelling of a [`SubqueryExpr`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubqueryForm {
    /// `EXISTS { MATCH .. }` or `EXISTS { pattern, .. [WHERE cond] }`; also `COUNT { .. }` and
    /// `COLLECT { .. }`.
    Braces,
    /// `EXISTS ( MATCH .. )` or `EXISTS ( pattern, .. [WHERE cond] )`.
    Parens,
    /// A Cypher pattern predicate, `(a)-[:T]->(b)` where an expression stands: an alias of
    /// `EXISTS { pattern }` that may not introduce a named variable (the binder's check).
    PatternPredicate,
    /// A pattern comprehension `[(a)-->(b) WHERE cond | expr]` ([`SubqueryKind::Collect`]).
    Comprehension,
}

/// Prefix operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnaryOp {
    /// `NOT`.
    Not,
    /// `-`.
    Minus,
    /// `+`.
    Plus,
}

/// Infix operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BinaryOp {
    /// `OR`.
    Or,
    /// `XOR`.
    Xor,
    /// `AND`.
    And,
    /// `=`.
    Eq,
    /// `<>` (Cypher `!=`).
    Ne,
    /// `<`.
    Lt,
    /// `<=`.
    Le,
    /// `>`.
    Gt,
    /// `>=`.
    Ge,
    /// `+`.
    Add,
    /// `-`.
    Sub,
    /// `*`.
    Mul,
    /// `/`.
    Div,
    /// `%`.
    Mod,
    /// `||` (string concatenation).
    Concat,
    /// `IN`.
    In,
    /// `STARTS WITH`.
    StartsWith,
    /// `ENDS WITH`.
    EndsWith,
    /// `CONTAINS`.
    Contains,
}

/// A `CASE` expression: simple (`CASE x WHEN v THEN ...`) when `operand` is set, searched
/// (`CASE WHEN cond THEN ...`) otherwise.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CaseExpr {
    /// The operand of a simple `CASE`.
    pub(crate) operand: Option<Expr>,
    /// The `WHEN ... THEN ...` arms in order (at least one).
    pub(crate) whens: Vec<CaseWhen>,
    /// The `ELSE` result.
    pub(crate) else_result: Option<Expr>,
}

/// One `WHEN ... THEN ...` arm.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CaseWhen {
    /// The value (simple `CASE`) or condition (searched `CASE`).
    pub(crate) when: Expr,
    /// The result.
    pub(crate) then: Expr,
}

/// The arguments of a function call.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CallArgs {
    /// `(*)`, as in `count(*)` (only `count` takes it).
    Star(Span),
    /// `(a, b, ...)`, possibly empty.
    Exprs(Vec<Expr>),
    /// `CAST(operand AS target)`.
    Cast {
        /// The converted expression.
        operand: Box<Expr>,
        /// The target type.
        target: CastTarget,
        /// The type name as written (one or two words).
        target_span: Span,
    },
    /// The SQL forms of `TRIM`: `TRIM([LEADING | TRAILING | BOTH] [character] FROM source)`.
    /// The plain forms `TRIM(s)` / `trim(list, n)` stay [`CallArgs::Exprs`].
    Trim {
        /// The side trimmed (`BOTH` when not written).
        side: TrimSide,
        /// The trimmed character (the space when not written).
        character: Option<Box<Expr>>,
        /// The trimmed string.
        source: Box<Expr>,
    },
}

impl CallArgs {
    /// The argument expressions in written order (`TRIM(c FROM s)`: `c`, then `s`).
    pub(crate) fn iter(&self) -> impl Iterator<Item = &Expr> {
        let (list, first, second): (&[Expr], Option<&Expr>, Option<&Expr>) = match self {
            CallArgs::Star(_) => (&[], None, None),
            CallArgs::Exprs(args) => (args, None, None),
            CallArgs::Cast { operand, .. } => (&[], Some(operand), None),
            CallArgs::Trim {
                character, source, ..
            } => (&[], character.as_deref(), Some(source)),
        };
        list.iter().chain(first).chain(second)
    }
}

/// The target type of `CAST` (the types Graphersal has values of; other GQL types are refused
/// by the parser).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CastTarget {
    /// `INTEGER` (`INT`, `INT64`, `BIGINT`, `SIGNED INTEGER`).
    Integer,
    /// `FLOAT` (`FLOAT64`, `DOUBLE`, `DOUBLE PRECISION`, `REAL`).
    Float,
    /// `STRING` (`VARCHAR`, `CHAR VARYING`, `CHARACTER VARYING`).
    String,
    /// `BOOLEAN` (`BOOL`).
    Boolean,
}

impl CastTarget {
    /// The canonical GQL type name.
    #[cfg_attr(not(test), allow(dead_code, reason = "used by the tests"))]
    pub(crate) fn name(self) -> &'static str {
        match self {
            CastTarget::Integer => "INTEGER",
            CastTarget::Float => "FLOAT",
            CastTarget::String => "STRING",
            CastTarget::Boolean => "BOOLEAN",
        }
    }
}

/// The side of a SQL `TRIM`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrimSide {
    /// `LEADING`: the start only.
    Leading,
    /// `TRAILING`: the end only.
    Trailing,
    /// `BOTH` (the default): both ends.
    Both,
}

/// Which end of an edge `IS SOURCE OF` / `IS DESTINATION OF` tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Endpoint {
    /// `SOURCE`: the stored source (Cypher `startNode(e)`).
    Source,
    /// `DESTINATION`: the stored target (Cypher `endNode(e)`).
    Destination,
}

/// `RETURN [DISTINCT] items`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ReturnClause {
    /// `DISTINCT`.
    pub(crate) distinct: bool,
    /// The `*` item (every named variable), when written.
    pub(crate) star: Option<Span>,
    /// The explicit items in written order.
    pub(crate) items: Vec<ReturnItem>,
    /// `GROUP BY ...` after the items (ISO GQL `<group by clause>`); grouping stays implicit,
    /// the binder only checks that the keys are the items without an aggregate.
    pub(crate) group_by: Option<GroupBy>,
    /// From `RETURN` to the last item.
    pub(crate) span: Span,
}

/// `GROUP BY key, ...` or the empty grouping set `GROUP BY ()`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GroupBy {
    /// The grouping keys in written order (empty: `()`): a column name or an item's expression.
    pub(crate) keys: Vec<Expr>,
    /// From `GROUP` to the last key.
    pub(crate) span: Span,
}

/// `expr [AS alias]`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ReturnItem {
    /// The expression.
    pub(crate) expr: Expr,
    /// The `AS` alias (else the column name is the expression text).
    pub(crate) alias: Option<Identifier>,
    /// The whole item.
    pub(crate) span: Span,
}

/// `ORDER BY item, ...`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OrderBy {
    /// The sort keys in priority order.
    pub(crate) items: Vec<SortItem>,
    /// From `ORDER` to the last item.
    pub(crate) span: Span,
}

/// The direction of a sort key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SortDirection {
    /// `ASC` / `ASCENDING`, the default.
    Asc,
    /// `DESC` / `DESCENDING`.
    Desc,
}

/// `expr [ASC|DESC]` (stage 1b may add `NULLS FIRST|LAST`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SortItem {
    /// The sort key.
    pub(crate) expr: Expr,
    /// The direction (`Asc` when not written).
    pub(crate) direction: SortDirection,
    /// The whole item.
    pub(crate) span: Span,
}

/// `OFFSET n` / `SKIP n` / `LIMIT n`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CountClause {
    /// The count (a literal or a parameter; the binder checks it).
    pub(crate) value: Expr,
    /// From the keyword to the value.
    pub(crate) span: Span,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(start: usize, end: usize) -> Span {
        Span {
            start,
            end,
            line: 1,
            column: start + 1,
        }
    }

    #[test]
    fn query_text_is_read_by_span_and_never_panics() {
        let source = "MATCH (a) RETURN a.name";
        let item = Expr {
            kind: ExprKind::Property {
                target: Box::new(Expr {
                    kind: ExprKind::Variable(Identifier {
                        name: "a".into(),
                        span: span(17, 18),
                    }),
                    span: span(17, 18),
                }),
                key: Identifier {
                    name: "name".into(),
                    span: span(19, 23),
                },
            },
            span: span(17, 23),
        };
        let linear = LinearQuery {
            matches: vec![MatchClause {
                optional: false,
                patterns: vec![PathPattern {
                    variable: None,
                    start: NodePattern {
                        variable: Some(Identifier {
                            name: "a".into(),
                            span: span(7, 8),
                        }),
                        labels: None,
                        properties: None,
                        where_clause: None,
                        span: span(6, 9),
                    },
                    steps: Vec::new(),
                    span: span(6, 9),
                }],
                where_clause: None,
                span: span(0, 9),
            }],
            stages: vec![Stage::Match(0)],
            ret: ReturnClause {
                distinct: false,
                star: None,
                items: vec![ReturnItem {
                    expr: item.clone(),
                    alias: None,
                    span: span(17, 23),
                }],
                group_by: None,
                span: span(10, 23),
            },
            order_by: None,
            offset: None,
            limit: None,
            importing: None,
            span: span(0, 23),
        };
        let query = Query {
            source: source.into(),
            body: Box::new(CompositeQuery {
                first: QueryBlock {
                    first: linear,
                    unions: Vec::new(),
                    span: span(0, 23),
                },
                next: Vec::new(),
                span: span(0, 23),
            }),
        };
        assert!(query.is_linear());
        assert_eq!(query.text(item.span), "a.name");
        assert_eq!(query.text(query.matches[0].patterns[0].span), "(a)");
        assert_eq!(query.text(span(20, 99)), "");
        assert_eq!(query.text(span(9, 3)), "");
    }
}
