//! The abstract syntax tree of a GQL read query (stage 1a subset).
//!
//! Every node carries the [`Span`] of the text it was parsed from, and [`Query`] keeps the
//! source text, so every later stage (binder, planner, lowering, errors) can point at the
//! offending text and derive column names from it ([`Query::text`]).
//!
//! Stage 1a covers `MATCH` (comma-separated path patterns, optional `WHERE`), `RETURN`
//! (`DISTINCT`, items with `AS`, `*`), `ORDER BY`, `OFFSET` / `SKIP`, `LIMIT`. Stage 1b extends
//! the tree where noted: `OPTIONAL MATCH`, `WITH` / `LET` / `FILTER` (the query becomes a
//! pipeline of clauses), quantified edges and path variables (`p = ...`). Those are refused by the
//! parser until then, so no node exists for them yet.

use super::lexer::Span;

/// A query: one or more `MATCH` clauses, then the result statement.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Query {
    /// The query text as given (spans index into it).
    pub(crate) source: String,
    /// The `MATCH` clauses in order (stage 1b: a pipeline of clauses).
    pub(crate) matches: Vec<MatchClause>,
    /// `RETURN` (mandatory).
    pub(crate) ret: ReturnClause,
    /// `ORDER BY ...`.
    pub(crate) order_by: Option<OrderBy>,
    /// `OFFSET n` or its Cypher alias `SKIP n`.
    pub(crate) offset: Option<CountClause>,
    /// `LIMIT n`.
    pub(crate) limit: Option<CountClause>,
    /// The whole query.
    pub(crate) span: Span,
}

impl Query {
    /// The source text of `span` (empty when the span does not fit the source, never a panic).
    pub(crate) fn text(&self, span: Span) -> &str {
        self.source.get(span.start..span.end).unwrap_or("")
    }
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

/// `MATCH pattern, pattern, ... [WHERE expr]` (stage 1b: `OPTIONAL`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MatchClause {
    /// The comma-separated path patterns; repeated variables join them.
    pub(crate) patterns: Vec<PathPattern>,
    /// The clause's `WHERE`.
    pub(crate) where_clause: Option<Expr>,
    /// From `MATCH` to the end of the clause.
    pub(crate) span: Span,
}

/// A path pattern: a node, then any number of edge-node steps (stage 1b: a path variable
/// `p = ...`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PathPattern {
    /// The first node.
    pub(crate) start: NodePattern,
    /// The following edges, each with the node it leads to.
    pub(crate) steps: Vec<PatternStep>,
    /// The whole pattern.
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

/// `-[e:Label {k: expr} WHERE expr]->` or an abbreviated form (stage 1b: a quantifier).
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
    /// The whole edge, arrows included.
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
    /// `name([DISTINCT] args)` or `name(*)`.
    FunctionCall {
        /// The function name as written.
        name: Identifier,
        /// `DISTINCT` before the arguments (aggregates).
        distinct: bool,
        /// The arguments.
        args: CallArgs,
    },
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
    /// `(*)`, as in `count(*)`.
    #[allow(dead_code, reason = "count(*) is parsed from stage 1b (aggregates) on")]
    Star(Span),
    /// `(a, b, ...)`, possibly empty.
    Exprs(Vec<Expr>),
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
    /// From `RETURN` to the last item.
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
        let query = Query {
            source: source.into(),
            matches: vec![MatchClause {
                patterns: vec![PathPattern {
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
            ret: ReturnClause {
                distinct: false,
                star: None,
                items: vec![ReturnItem {
                    expr: item.clone(),
                    alias: None,
                    span: span(17, 23),
                }],
                span: span(10, 23),
            },
            order_by: None,
            offset: None,
            limit: None,
            span: span(0, 23),
        };
        assert_eq!(query.text(item.span), "a.name");
        assert_eq!(query.text(query.matches[0].patterns[0].span), "(a)");
        assert_eq!(query.text(span(20, 99)), "");
        assert_eq!(query.text(span(9, 3)), "");
    }
}
