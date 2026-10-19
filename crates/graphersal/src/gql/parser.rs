//! Recursive-descent parser of GQL read queries (stages 1a and 1b): tokens to the [`ast`](super::ast) tree.
//!
//! [`parse`] never panics on any input. Every failure, lexer errors included, is a
//! [`TraverserError::Gql`] built through [`GqlErrorBuilder`]: the span of the offending token,
//! the tokens that would have been accepted there, a "did you mean" suggestion for a misspelt
//! keyword or function name, and for a refused or not yet supported construct the GQL form or
//! the Gremlin alternative.
//!
//! # Grammar
//!
//! ```text
//! query      := match stage* return order_by? (OFFSET|SKIP expr)? (LIMIT expr)? ';'?
//! stage      := match | with | let | filter
//! match      := OPTIONAL? MATCH path (',' path)* (WHERE expr)?
//! with       := WITH projection order_by? (OFFSET|SKIP expr)? (LIMIT expr)? (WHERE expr)?
//! let        := LET var '=' expr (',' var '=' expr)*
//! filter     := FILTER WHERE? expr
//! path       := (var '=')? node (edge quantifier? node)*
//! node       := '(' var? ((':'|IS) labels)? map? (WHERE expr)? ')'
//! edge       := '-[' filler ']->' | '<-[' filler ']-' | '-[' filler ']-' | '<-[' filler ']->'
//!             | '-->' | '<--' | '--' | '<-->' | '->' | '<-' | '-' | '<->'
//! filler     := var? ((':'|IS) labels)? cypher_len? map? (WHERE expr)?
//! quantifier := '{' int '}' | '{' int? ',' int? '}' | '*' | '+'
//! cypher_len := '*' int? ('..' int?)?            -- edges only, inside the brackets
//! labels     := label_or (':' name)*          -- Cypher `:A:B`, plain names only
//! label_or   := label_and ('|' label_and)*
//! label_and  := label_not ('&' label_not)*
//! label_not  := '!' label_not | name | '%' | '(' label_or ')'
//! return     := RETURN projection
//! projection := DISTINCT? ('*' (',' item)* | item (',' item)*)
//! item       := expr (AS name)?
//! order_by   := ORDER BY expr (ASC|ASCENDING|DESC|DESCENDING)? (',' ...)*
//! call       := name '(' (expr (',' expr)*)? ')'
//!             | aggregate '(' DISTINCT? expr ')' | count '(' '*' ')'
//! ```
//!
//! Expressions, loosest first: `OR` < `XOR` < `AND` < `NOT` < comparison (`= <> != < <= > >=`,
//! not chainable: `a < b < c` is an error) < predicates (`IS [NOT] NULL`, `IS [NOT] LABELED`,
//! `IN`, `STARTS WITH`, `ENDS WITH`, `CONTAINS`; tighter than comparison as in openCypher, so
//! `a = b IS NULL` is `a = (b IS NULL)`) < additive (`+ - ||`) < multiplicative (`* / %`) <
//! unary (`- +`) < postfix (`.key`, Cypher `v:Label`, an alias of `v IS LABELED Label`) <
//! primary (literals, `[list]`, `{map}`, `$param`, variables, function calls, `CASE`, `( )`).
//! A minus directly before an integer or float literal is folded into the literal, so
//! `-9223372036854775808` is `i64::MIN`; the same digits without the minus are out of range.
//!
//! # Decisions
//!
//! - **`RETURN` is mandatory** (design D10); its absence is a syntax error whose alternative names
//!   `RETURN *` and the query's own variables. A query without `MATCH` is "not yet supported".
//! - **Result tail in GQL order:** `ORDER BY`, then `OFFSET` / `SKIP`, then `LIMIT`, each at most
//!   once, as GQL and Neo4j write them. Another order is a syntax error naming the right one; the
//!   standalone Cypher 25 forms before `RETURN` are "not yet supported".
//! - **Label `IS` form:** `(v IS A)`, `[e IS T]` (GQL `<is label expression>`) are accepted beside
//!   `:`; Cypher `:A:B` is an alias of `A&B` (plain names only, mixing with `| & !` is refused as
//!   ambiguous). In expressions `v IS [NOT] LABELED A` and Cypher `v:A`.
//! - **Functions are checked here:** the name (with "did you mean"), the argument count and
//!   `DISTINCT` / `*`, against the one table [`FUNCTIONS`] that the binder and the evaluator also
//!   read. Aggregates take `DISTINCT`; only `count` takes `*`, and `count(DISTINCT *)` is an
//!   error. Where an aggregate may stand (not nested, not in `WHERE`, ...) is the binder's check.
//!   `path_length(p)` has the Cypher alias `length(p)`. Cypher `id()` is refused for
//!   `element_id()` (D15).
//! - **The clause pipeline** starts with `[OPTIONAL] MATCH` (a leading `OPTIONAL MATCH` too);
//!   `WITH`, `LET` and `FILTER` before the first `MATCH` are "not yet supported", like a query
//!   without `MATCH`. `WITH` is Cypher's spelling of the GQL linear pipeline; its parts come in
//!   Cypher's order `ORDER BY`, `OFFSET` / `SKIP`, `LIMIT`, `WHERE`, each at most once (another
//!   order is a syntax error naming it). `OPTIONAL { .. }` / `OPTIONAL ( .. )` blocks are "not
//!   yet supported".
//! - **Quantifiers** are normalized to `min..=max` ([`Quantifier`]): GQL `{n}` = `{n,n}`, `{,n}` =
//!   `{0,n}`, `{m,}` and `{,}` unbounded, `*` = `{0,}`, `+` = `{1,}`; Cypher `[e*]` = `{1,}`,
//!   `[e*n]` = `{n}`, `[e*m..n]` = `{m,n}`, `[e*..n]` = `{1,n}`, `[e*m..]` = `{m,}` (Cypher's
//!   lower bound defaults to 1, GQL's postfix `*` to 0). Bounds are unsigned integer literals; a
//!   lower bound above the upper bound, a quantifier after a node, and both forms on one edge
//!   are syntax errors; Memgraph's `*BFS`, `*WSHORTEST`, ... stay refused, and quantified path
//!   patterns `((a)-->(b)){m,n}` stay "not yet supported". A GQL quantifier is not part of the
//!   edge's span; a Cypher one is.
//! - **Path variables:** `p = path`; the path's span starts at the variable.
//! - **Keywords are contextual** (lexer docs): where only a name can stand (label, property key,
//!   map key, alias, function name) a keyword is a name; as a variable it must be back-quoted.
//! - **Bounded nesting (kind `Limit`):** the recursion depth of the parser is limited to
//!   [`MAX_DEPTH`] and the height of every expression and label tree to [`MAX_HEIGHT`], so
//!   neither parsing nor any later recursive stage (binder, evaluator, `Drop`) can overflow the
//!   stack, whatever the input.

use super::ast::BinaryOp;
use super::ast::{
    CallArgs, CaseExpr, CaseWhen, CountClause, EdgeDirection, EdgePattern, Expr, ExprKind,
    FilterClause, Identifier, LabelExpr, LabelExprKind, LetBinding, LetClause, Literal, MapEntry,
    MapLiteral, MatchClause, NodePattern, OrderBy, PathPattern, PatternStep, Quantifier, Query,
    ReturnClause, ReturnItem, SortDirection, SortItem, Stage, UnaryOp, WithClause,
};
use super::lexer::{Keyword, LexError, Span, Token, TokenKind, tokenize};
use crate::error::{GqlErrorBuilder, GqlErrorKind, TraverserError};

/// The deepest nesting accepted: recursion levels of the parser (parentheses, `NOT`, unary
/// minus, lists, maps, `CASE`, function arguments, label expressions).
pub(crate) const MAX_DEPTH: usize = 128;

/// The greatest height of an expression or label expression tree, which bounds the recursion of
/// every later stage (and of `Drop`): a flat chain `a OR b OR ...` grows one level per operator.
pub(crate) const MAX_HEIGHT: usize = 512;

/// The alternative of every refused write clause.
const WRITE_ALTERNATIVE: &str = "gql() is read-only; use the Gremlin mutation steps (add_v(), \
                                 add_e(), property(), drop(), merge_v())";

/// The alternative of the refused undirected edge forms.
const TILDE_ALTERNATIVE: &str = "edges in Graphersal are directed; use -[e]- for either direction";

/// Whether a function computes a value per row or aggregates rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FunctionKind {
    /// One value per row.
    Scalar,
    /// Aggregates rows (`DISTINCT` and, for `count`, `*` allowed).
    Aggregate,
}

/// The GQL functions Graphersal knows, by meaning (aliases share one variant).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Function {
    /// `element_id(e)`, Cypher `elementId(e)`: the element's id as a string (D15).
    ElementId,
    /// `labels(v)`: the label set in stored order.
    Labels,
    /// `type(e)`: the edge label.
    Type,
    /// `coalesce(a, b, ...)`: the first non-NULL argument.
    Coalesce,
    /// `upper(s)`, Cypher `toUpper(s)`.
    Upper,
    /// `lower(s)`, Cypher `toLower(s)`.
    Lower,
    /// `char_length(s)` / `character_length(s)`.
    CharLength,
    /// Cypher `size(x)`: the length of a string or a list.
    Size,
    /// `cardinality(list)`.
    Cardinality,
    /// `path_length(p)`, Cypher `length(p)`: the number of edges of a path.
    PathLength,
    /// `count(x)`, `count(*)`.
    Count,
    /// `sum(x)`.
    Sum,
    /// `avg(x)`.
    Avg,
    /// `min(x)`.
    Min,
    /// `max(x)`.
    Max,
    /// `collect_list(x)`, Cypher `collect(x)`.
    CollectList,
}

/// One entry of [`FUNCTIONS`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct FunctionSpec {
    /// What it computes.
    pub(crate) function: Function,
    /// The accepted names, the GQL name first, Cypher aliases after (matched ASCII
    /// case-insensitively).
    pub(crate) names: &'static [&'static str],
    /// The fewest arguments.
    pub(crate) min_args: usize,
    /// The most arguments (`usize::MAX`: any number).
    pub(crate) max_args: usize,
    /// Scalar or aggregate.
    pub(crate) kind: FunctionKind,
    /// The Gremlin step that does the same after `gql()` (aggregates, for diagnostics).
    pub(crate) gremlin: &'static str,
}

impl FunctionSpec {
    /// The canonical (GQL) name.
    pub(crate) fn name(&self) -> &'static str {
        self.names.first().copied().unwrap_or("")
    }
}

const fn scalar(
    function: Function,
    names: &'static [&'static str],
    min: usize,
    max: usize,
) -> FunctionSpec {
    FunctionSpec {
        function,
        names,
        min_args: min,
        max_args: max,
        kind: FunctionKind::Scalar,
        gremlin: "",
    }
}

const fn aggregate(
    function: Function,
    names: &'static [&'static str],
    gremlin: &'static str,
) -> FunctionSpec {
    FunctionSpec {
        function,
        names,
        min_args: 1,
        max_args: 1,
        kind: FunctionKind::Aggregate,
        gremlin,
    }
}

/// Every function of the GQL support: the one list the parser, binder and evaluator read.
pub(crate) const FUNCTIONS: &[FunctionSpec] = &[
    scalar(Function::ElementId, &["element_id", "elementId"], 1, 1),
    scalar(Function::Labels, &["labels"], 1, 1),
    scalar(Function::Type, &["type"], 1, 1),
    scalar(Function::Coalesce, &["coalesce"], 1, usize::MAX),
    scalar(Function::Upper, &["upper", "toUpper"], 1, 1),
    scalar(Function::Lower, &["lower", "toLower"], 1, 1),
    scalar(
        Function::CharLength,
        &["char_length", "character_length"],
        1,
        1,
    ),
    scalar(Function::Size, &["size"], 1, 1),
    scalar(Function::Cardinality, &["cardinality"], 1, 1),
    scalar(Function::PathLength, &["path_length", "length"], 1, 1),
    aggregate(Function::Count, &["count"], "count()"),
    aggregate(Function::Sum, &["sum"], "sum()"),
    aggregate(Function::Avg, &["avg"], "mean()"),
    aggregate(Function::Min, &["min"], "min()"),
    aggregate(Function::Max, &["max"], "max()"),
    aggregate(
        Function::CollectList,
        &["collect_list", "collect"],
        "fold()",
    ),
];

/// The function spelled `name` (ASCII case-insensitive), if any.
pub(crate) fn lookup_function(name: &str) -> Option<&'static FunctionSpec> {
    FUNCTIONS
        .iter()
        .find(|spec| spec.names.iter().any(|n| n.eq_ignore_ascii_case(name)))
}

/// Parses `query` into its syntax tree.
pub(crate) fn parse(query: &str) -> Result<Query, TraverserError> {
    let tokens = tokenize(query).map_err(|e| lex_error(e).into_error(query))?;
    Parser::new(query, tokens)
        .query()
        .map_err(|e| e.into_error(query))
}

fn lex_error(e: LexError) -> GqlErrorBuilder {
    GqlErrorBuilder::new(GqlErrorKind::Syntax, e.span.start..e.span.end, e.message())
}

type PResult<T> = Result<T, Box<GqlErrorBuilder>>;

fn err(kind: GqlErrorKind, span: Span, message: impl Into<String>) -> GqlErrorBuilder {
    GqlErrorBuilder::new(kind, span.start..span.end, message)
}

/// Binding power of the infix operators (higher binds tighter).
const BP_OR: u8 = 1;
const BP_XOR: u8 = 2;
const BP_AND: u8 = 3;
const BP_NOT: u8 = 4;
const BP_COMPARE: u8 = 5;
const BP_PREDICATE: u8 = 6;
const BP_ADD: u8 = 7;
const BP_MUL: u8 = 8;
const BP_UNARY: u8 = 9;

/// What follows an operand in an expression.
#[derive(Clone, Copy)]
enum Infix {
    Binary(BinaryOp, u8),
    /// `<-` read as `<` and a unary minus.
    LtMinus,
    Is,
    StartsWith,
    EndsWith,
}

/// The inside of a node or edge pattern.
struct Filler {
    variable: Option<Identifier>,
    labels: Option<LabelExpr>,
    properties: Option<MapLiteral>,
    where_clause: Option<Expr>,
    /// Cypher's `*m..n` (edges only).
    quantifier: Option<Quantifier>,
}

struct Parser<'a> {
    src: &'a str,
    tokens: Vec<Token<'a>>,
    pos: usize,
    eof: Token<'a>,
    /// The span of the last consumed token.
    last: Span,
    /// Current recursion depth.
    depth: usize,
    /// A minus read from a `<-` token, applied to the next operand.
    pending_minus: Option<Span>,
    /// Variables declared in patterns, in order (for the missing-`RETURN` alternative).
    variables: Vec<String>,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str, tokens: Vec<Token<'a>>) -> Self {
        let end = Span {
            start: src.len(),
            end: src.len(),
            line: 1,
            column: 1,
        };
        let eof = tokens.last().cloned().unwrap_or(Token {
            kind: TokenKind::Eof,
            span: end,
            text: "",
        });
        let first = tokens.first().map_or(end, |t| t.span);
        Parser {
            src,
            tokens,
            pos: 0,
            eof,
            last: Span {
                end: first.start,
                ..first
            },
            depth: 0,
            pending_minus: None,
            variables: Vec::new(),
        }
    }

    // ----- token access -----

    fn peek(&self) -> &Token<'a> {
        self.nth(0)
    }

    fn nth(&self, n: usize) -> &Token<'a> {
        self.tokens
            .get(self.pos.saturating_add(n))
            .unwrap_or(&self.eof)
    }

    fn advance(&mut self) -> Token<'a> {
        let token = self.peek().clone();
        if self.pos < self.tokens.len() && token.kind != TokenKind::Eof {
            self.pos += 1;
        }
        self.last = token.span;
        token
    }

    fn at(&self, kind: &TokenKind) -> bool {
        std::mem::discriminant(&self.peek().kind) == std::mem::discriminant(kind)
    }

    fn at_kw(&self, keyword: Keyword) -> bool {
        self.peek().keyword() == Some(keyword)
    }

    fn eat(&mut self, kind: &TokenKind) -> Option<Span> {
        self.at(kind).then(|| self.advance().span)
    }

    fn eat_kw(&mut self, keyword: Keyword) -> Option<Span> {
        self.at_kw(keyword).then(|| self.advance().span)
    }

    fn expect(&mut self, kind: &TokenKind, expected: &[&str]) -> PResult<Span> {
        match self.eat(kind) {
            Some(span) => Ok(span),
            None => Err(self.unexpected(expected)),
        }
    }

    fn expect_kw(&mut self, keyword: Keyword, expected: &[&str]) -> PResult<Span> {
        match self.eat_kw(keyword) {
            Some(span) => Ok(span),
            None => Err(self.unexpected(expected)),
        }
    }

    /// A name where only a name can stand: identifiers, back-quoted identifiers, keywords.
    fn expect_name(&mut self, expected: &[&str]) -> PResult<Identifier> {
        match self.peek().name() {
            Some(name) => {
                let name = name.to_string();
                let span = self.advance().span;
                Ok(Identifier { name, span })
            }
            None => Err(self.unexpected(expected)),
        }
    }

    // ----- errors -----

    /// The error for an unexpected current token: a targeted one when the token names a refused
    /// or not yet supported construct, else a syntax error with the expected tokens and a "did
    /// you mean" for a misspelt keyword.
    fn unexpected(&self, expected: &[&str]) -> Box<GqlErrorBuilder> {
        if let Some(e) = self.construct_error() {
            return e;
        }
        let token = self.peek();
        let mut builder = err(
            GqlErrorKind::Syntax,
            token.span,
            format!("unexpected {}", describe(token)),
        )
        .expected(expected.iter().copied());
        if token.kind == TokenKind::Ident
            && let Some(suggestion) = suggest_keyword(token.text, expected)
        {
            builder = builder.suggestion(suggestion);
        }
        // A keyword written in lower or mixed case where a name could stand is likely meant as a
        // name (`order`, `end`); upper case is likely a misplaced clause.
        if let TokenKind::Keyword(_) = token.kind
            && token.text != token.text.to_ascii_uppercase()
            && expected
                .iter()
                .any(|e| *e == "an expression" || *e == "a variable")
        {
            builder = builder.alternative(format!(
                "{} is a keyword; to use it as a name, write it back-quoted: `{}`",
                token.text, token.text
            ));
        }
        Box::new(builder)
    }

    /// A targeted error when the current token starts a refused or not yet supported construct.
    fn construct_error(&self) -> Option<Box<GqlErrorBuilder>> {
        let token = self.peek();
        let span = token.span;
        let text = token.text.to_ascii_uppercase();
        let refused = |message: String, alternative: &str| {
            let mut b = err(GqlErrorKind::Refused, span, message);
            if !alternative.is_empty() {
                b = b.alternative(alternative);
            }
            Some(Box::new(b))
        };
        let not_yet = |what: &str, alternative: &str| {
            let mut b = err(
                GqlErrorKind::NotYet,
                span,
                format!("{what} is not yet supported in this version"),
            );
            if !alternative.is_empty() {
                b = b.alternative(alternative);
            }
            Some(Box::new(b))
        };
        match token.kind {
            TokenKind::Tilde | TokenKind::TildeLeft | TokenKind::TildeRight => {
                return refused(
                    format!("undirected edges (`{}`) are not supported", token.text),
                    TILDE_ALTERNATIVE,
                );
            }
            TokenKind::Keyword(_) => {}
            _ => return None,
        }
        match token.keyword()? {
            Keyword::Insert
            | Keyword::Create
            | Keyword::Set
            | Keyword::Remove
            | Keyword::Delete
            | Keyword::Merge => refused(
                format!("{text} writes to the graph; gql() is read-only"),
                WRITE_ALTERNATIVE,
            ),
            Keyword::Detach | Keyword::Nodetach => {
                let mut whole = span;
                if self.nth(1).keyword() == Some(Keyword::Delete) {
                    whole = span.to(self.nth(1).span);
                }
                let mut b = err(
                    GqlErrorKind::Refused,
                    whole,
                    format!("{text} DELETE writes to the graph; gql() is read-only"),
                );
                b = b.alternative(WRITE_ALTERNATIVE);
                Some(Box::new(b))
            }
            Keyword::Call if self.nth(1).kind == TokenKind::LBrace => not_yet(
                "CALL { } (a subquery)",
                "run the inner query as its own gql() call, or write it as Gremlin steps",
            ),
            Keyword::Call | Keyword::Yield => refused(
                format!("{text}: procedure calls are vendor extensions, not part of GQL"),
                "write what the procedure does as Gremlin steps, for example \
                 g.v().label().dedup() instead of CALL db.labels()",
            ),
            Keyword::Show => refused(
                "SHOW commands are vendor extensions, not part of GQL".into(),
                "",
            ),
            Keyword::Use => refused(
                "USE is not supported: gql() runs on the graph of its traversal source".into(),
                "",
            ),
            Keyword::Cypher => not_yet("a CYPHER version prefix", "remove the prefix"),
            Keyword::Next => not_yet(
                "NEXT",
                "chain Gremlin steps after gql(), for example \
                 g.gql(\"MATCH (p:person) RETURN p\").select(\"p\").out(\"created\")",
            ),
            Keyword::Union | Keyword::Except | Keyword::Intersect => not_yet(
                &text,
                "combine the results in Gremlin, for example \
                 g.v().union(__.has_label(\"person\"), __.has_label(\"software\"))",
            ),
            Keyword::For | Keyword::Unwind => not_yet(
                &text,
                "Gremlin unfold(), for example g.inject([1, 2, 3]).unfold()",
            ),
            Keyword::Exists => not_yet(
                "EXISTS",
                "add the pattern to the MATCH clause, or use Gremlin where(), for example \
                 g.v().has_label(\"person\").where(__.out(\"created\"))",
            ),
            Keyword::Shortest => not_yet(
                "shortest path search",
                "Gremlin repeat(), for example \
                 g.v(\"1\").repeat(__.out().simple_path()).until(__.has_id(\"5\")).path().limit(1)",
            ),
            Keyword::Nulls => not_yet(
                "NULLS FIRST / NULLS LAST",
                "NULLs sort last ascending and first descending",
            ),
            _ => None,
        }
    }

    fn not_yet(&self, span: Span, what: &str, alternative: &str) -> Box<GqlErrorBuilder> {
        let mut b = err(
            GqlErrorKind::NotYet,
            span,
            format!("{what} is not yet supported in this version"),
        );
        if !alternative.is_empty() {
            b = b.alternative(alternative);
        }
        Box::new(b)
    }

    fn syntax(&self, span: Span, message: impl Into<String>) -> Box<GqlErrorBuilder> {
        Box::new(err(GqlErrorKind::Syntax, span, message))
    }

    fn enter(&mut self) -> PResult<()> {
        if self.depth >= MAX_DEPTH {
            return Err(self.too_deep(self.peek().span));
        }
        self.depth += 1;
        Ok(())
    }

    fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    fn too_deep(&self, span: Span) -> Box<GqlErrorBuilder> {
        Box::new(err(
            GqlErrorKind::Limit,
            span,
            format!("the query nests deeper than {MAX_DEPTH} levels"),
        ))
    }

    /// The height of a new tree node over children of height `child`; an error past the limit.
    fn height(&self, child: usize, span: Span) -> PResult<usize> {
        let height = child.saturating_add(1);
        if height > MAX_HEIGHT {
            let b = err(
                GqlErrorKind::Limit,
                span,
                format!("the expression is too large: its operator tree is deeper than {MAX_HEIGHT} levels"),
            )
            .alternative(
                "split long AND / OR chains, and test many alternatives with IN, for example \
                 p.name IN ['marko', 'vadas']",
            );
            return Err(Box::new(b));
        }
        Ok(height)
    }

    // ----- query and clauses -----

    fn query(&mut self) -> PResult<Query> {
        let start = self.peek().span;
        let mut matches = Vec::new();
        let mut stages = Vec::new();
        loop {
            match self.peek().keyword() {
                Some(Keyword::Match | Keyword::Optional) => {
                    let clause = self.match_clause()?;
                    stages.push(Stage::Match(matches.len()));
                    matches.push(clause);
                    continue;
                }
                Some(keyword @ (Keyword::With | Keyword::Let | Keyword::Filter)) => {
                    if stages.is_empty() {
                        return Err(self.leading_stage());
                    }
                    stages.push(self.pipeline_stage(keyword)?);
                    continue;
                }
                _ => {}
            }
            if self.at_kw(Keyword::Return) {
                if stages.is_empty() {
                    return Err(self.not_yet(
                        self.peek().span,
                        "a query without MATCH",
                        "start with a MATCH clause, or use Gremlin inject(), for example \
                         g.inject(1)",
                    ));
                }
                break;
            }
            if stages.is_empty() {
                return Err(self.unexpected(&["MATCH"]));
            }
            if matches!(self.peek().kind, TokenKind::Eof | TokenKind::Semicolon) {
                return Err(self.missing_return());
            }
            if let Some(Keyword::Order | Keyword::Offset | Keyword::Skip | Keyword::Limit) =
                self.peek().keyword()
            {
                if let Some(Stage::With(_)) = stages.last() {
                    return Err(self.misplaced_with_tail());
                }
                return Err(self.not_yet(
                    self.peek().span,
                    &format!("{} before RETURN", self.peek().text.to_ascii_uppercase()),
                    "write ORDER BY, OFFSET and LIMIT after RETURN, for example \
                     MATCH (p:person) RETURN p.name ORDER BY p.name LIMIT 2",
                ));
            }
            return Err(self.after_stage(stages.last(), &matches));
        }
        let ret = self.return_clause()?;
        let order_by = if self.at_kw(Keyword::Order) {
            Some(self.order_by()?)
        } else {
            None
        };
        let offset = self.offset_clause()?;
        let limit = self.limit_clause()?;
        let end = self.last;
        self.eat(&TokenKind::Semicolon);
        if !self.at(&TokenKind::Eof) {
            return Err(self.after_return(
                &ret,
                order_by.is_some(),
                offset.is_some(),
                limit.is_some(),
            ));
        }
        Ok(Query {
            source: self.src.to_string(),
            matches,
            stages,
            ret,
            order_by,
            offset,
            limit,
            span: start.to(end),
        })
    }

    /// `OFFSET n` / `SKIP n`, when the current token starts it.
    fn offset_clause(&mut self) -> PResult<Option<CountClause>> {
        match self.peek().keyword() {
            Some(Keyword::Offset | Keyword::Skip) => {
                let keyword = self.advance().span;
                let value = self.expr()?;
                Ok(Some(CountClause {
                    span: keyword.to(self.last),
                    value,
                }))
            }
            _ => Ok(None),
        }
    }

    /// `LIMIT n`, when the current token starts it.
    fn limit_clause(&mut self) -> PResult<Option<CountClause>> {
        let Some(keyword) = self.eat_kw(Keyword::Limit) else {
            return Ok(None);
        };
        let value = self.expr()?;
        Ok(Some(CountClause {
            span: keyword.to(self.last),
            value,
        }))
    }

    /// The error for a token that continues no clause of the pipeline: the tokens the last
    /// clause and the pipeline accept, and a "did you mean" over the clause keywords.
    fn after_stage(&self, last: Option<&Stage>, matches: &[MatchClause]) -> Box<GqlErrorBuilder> {
        // The 1a lists stay as they were; the pipeline keywords only feed the suggestion.
        let expected: Vec<&str> = match last {
            Some(Stage::Match(i)) => match matches.get(*i) {
                Some(MatchClause {
                    where_clause: None, ..
                }) => vec!["`,`", "WHERE", "MATCH", "RETURN"],
                _ => vec!["MATCH", "RETURN"],
            },
            Some(Stage::With(with)) => {
                let mut expected = Vec::new();
                let tail = with.order_by.is_some()
                    || with.offset.is_some()
                    || with.limit.is_some()
                    || with.where_clause.is_some();
                if !tail {
                    expected.push("`,`");
                    if with.items.last().is_some_and(|item| item.alias.is_none()) {
                        expected.push("AS");
                    }
                }
                if with.where_clause.is_none() {
                    expected.push("WHERE");
                }
                expected.extend(["MATCH", "RETURN"]);
                expected
            }
            Some(Stage::Let(_)) => vec!["`,`", "MATCH", "RETURN"],
            Some(Stage::Filter(_)) | None => vec!["MATCH", "RETURN"],
        };
        let error = self.unexpected(&expected);
        let token = self.peek();
        if token.kind == TokenKind::Ident
            && suggest_keyword(token.text, &expected).is_none()
            && let Some(suggestion) =
                suggest_keyword(token.text, &["OPTIONAL MATCH", "WITH", "LET", "FILTER"])
        {
            return Box::new((*error).suggestion(suggestion));
        }
        error
    }

    /// The error for `WITH`, `LET` or `FILTER` as the first clause.
    fn leading_stage(&self) -> Box<GqlErrorBuilder> {
        let token = self.peek();
        self.not_yet(
            token.span,
            &format!(
                "a query that starts with {}",
                token.text.to_ascii_uppercase()
            ),
            "start with a MATCH clause, or use Gremlin inject(), for example g.inject(1)",
        )
    }

    /// `WITH`, `LET` or `FILTER` after the first clause.
    fn pipeline_stage(&mut self, keyword: Keyword) -> PResult<Stage> {
        Ok(match keyword {
            Keyword::Let => Stage::Let(self.let_clause()?),
            Keyword::Filter => Stage::Filter(self.filter_clause()?),
            _ => Stage::With(Box::new(self.with_clause()?)),
        })
    }

    /// `WITH [DISTINCT] items [ORDER BY ..] [OFFSET|SKIP n] [LIMIT n] [WHERE expr]`.
    fn with_clause(&mut self) -> PResult<WithClause> {
        let start = self.expect_kw(Keyword::With, &["WITH"])?;
        let (distinct, star, items) = self.projection()?;
        let order_by = if self.at_kw(Keyword::Order) {
            Some(self.order_by()?)
        } else {
            None
        };
        let offset = self.offset_clause()?;
        let limit = self.limit_clause()?;
        let where_clause = if self.eat_kw(Keyword::Where).is_some() {
            Some(self.expr()?)
        } else {
            None
        };
        Ok(WithClause {
            distinct,
            star,
            items,
            order_by,
            offset,
            limit,
            where_clause,
            span: start.to(self.last),
        })
    }

    /// `ORDER BY`, `OFFSET` / `SKIP` or `LIMIT` after a `WITH` clause that could not take it
    /// there (out of order or repeated).
    fn misplaced_with_tail(&self) -> Box<GqlErrorBuilder> {
        let token = self.peek();
        let keyword = match token.keyword() {
            Some(Keyword::Order) => "ORDER BY".to_string(),
            _ => token.text.to_ascii_uppercase(),
        };
        let b = err(
            GqlErrorKind::Syntax,
            token.span,
            format!(
                "{keyword} is out of place: WITH takes ORDER BY, OFFSET / SKIP, LIMIT and WHERE \
                 in this order, each at most once"
            ),
        )
        .alternative(
            "write the parts of WITH in the order ORDER BY, OFFSET, LIMIT, WHERE, for example \
             WITH p ORDER BY p.age OFFSET 1 LIMIT 2 WHERE p.age > 30",
        );
        Box::new(b)
    }

    /// `LET v = expr (, v = expr)*`.
    fn let_clause(&mut self) -> PResult<LetClause> {
        let start = self.expect_kw(Keyword::Let, &["LET"])?;
        let mut bindings = Vec::new();
        loop {
            let variable = match self.peek().kind {
                TokenKind::Ident | TokenKind::QuotedIdent(_) => {
                    let name = self.peek().name().unwrap_or("").to_string();
                    let span = self.advance().span;
                    if !self.variables.contains(&name) {
                        self.variables.push(name.clone());
                    }
                    Identifier { name, span }
                }
                _ => return Err(self.unexpected(&["a variable"])),
            };
            self.expect(&TokenKind::Eq, &["`=`"])?;
            let value = self.expr()?;
            bindings.push(LetBinding {
                span: variable.span.to(self.last),
                variable,
                value,
            });
            if self.eat(&TokenKind::Comma).is_none() {
                break;
            }
        }
        Ok(LetClause {
            bindings,
            span: start.to(self.last),
        })
    }

    /// `FILTER [WHERE] expr`.
    fn filter_clause(&mut self) -> PResult<FilterClause> {
        let start = self.expect_kw(Keyword::Filter, &["FILTER"])?;
        self.eat_kw(Keyword::Where);
        let condition = self.expr()?;
        Ok(FilterClause {
            condition,
            span: start.to(self.last),
        })
    }

    /// The error for a token after a complete result statement.
    fn after_return(
        &self,
        ret: &ReturnClause,
        has_order: bool,
        has_offset: bool,
        has_limit: bool,
    ) -> Box<GqlErrorBuilder> {
        let token = self.peek();
        let span = token.span;
        match token.keyword() {
            Some(Keyword::Order) if has_offset || has_limit => {
                let mut b = err(
                    GqlErrorKind::Syntax,
                    span,
                    "ORDER BY must come before OFFSET / SKIP and LIMIT",
                );
                b = b.alternative(
                    "write the result clauses in the order ORDER BY, OFFSET, LIMIT, for example \
                     RETURN p.name ORDER BY p.name OFFSET 1 LIMIT 2",
                );
                return Box::new(b);
            }
            Some(Keyword::Offset | Keyword::Skip) if has_limit => {
                let mut b = err(
                    GqlErrorKind::Syntax,
                    span,
                    format!("{} must come before LIMIT", token.text.to_ascii_uppercase()),
                );
                b = b.alternative(
                    "write the result clauses in the order ORDER BY, OFFSET, LIMIT, for example \
                     RETURN p.name ORDER BY p.name OFFSET 1 LIMIT 2",
                );
                return Box::new(b);
            }
            Some(
                Keyword::Match
                | Keyword::Return
                | Keyword::Optional
                | Keyword::With
                | Keyword::Let
                | Keyword::Filter,
            ) => {
                let mut b = err(
                    GqlErrorKind::Syntax,
                    span,
                    format!(
                        "{} after RETURN: RETURN ends the query",
                        token.text.to_ascii_uppercase()
                    ),
                );
                b = b.alternative(
                    "put every MATCH before the one RETURN, or chain Gremlin steps after gql()",
                );
                return Box::new(b);
            }
            _ => {}
        }
        let mut expected = Vec::new();
        if !has_order && !has_offset && !has_limit {
            expected.push("`,`");
            if ret.items.last().is_some_and(|item| item.alias.is_none()) {
                expected.push("AS");
            }
            expected.push("ORDER BY");
        }
        if !has_offset && !has_limit {
            expected.extend(["OFFSET", "SKIP"]);
        }
        if !has_limit {
            expected.push("LIMIT");
        }
        expected.push("end of input");
        self.unexpected(&expected)
    }

    fn missing_return(&self) -> Box<GqlErrorBuilder> {
        let alternative = if self.variables.is_empty() {
            "end the query with a RETURN statement, such as RETURN * (every variable) or \
             RETURN a, b"
                .to_string()
        } else {
            let named: Vec<&str> = self.variables.iter().take(3).map(String::as_str).collect();
            format!(
                "end the query with a RETURN statement, such as RETURN * (every variable) or \
                 RETURN {}",
                named.join(", ")
            )
        };
        let b = err(
            GqlErrorKind::Syntax,
            self.peek().span,
            "missing RETURN: a GQL query ends with a RETURN statement",
        )
        .expected(["RETURN"])
        .alternative(alternative);
        Box::new(b)
    }

    fn match_clause(&mut self) -> PResult<MatchClause> {
        let optional = self.eat_kw(Keyword::Optional);
        if let Some(keyword) = optional
            && matches!(self.peek().kind, TokenKind::LBrace | TokenKind::LParen)
        {
            return Err(self.not_yet(
                keyword.to(self.peek().span),
                "an OPTIONAL block (OPTIONAL { } / OPTIONAL ( ))",
                "write each optional part as OPTIONAL MATCH, for example \
                 MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) RETURN p, s",
            ));
        }
        let keyword = self.expect_kw(Keyword::Match, &["MATCH"])?;
        let start = optional.unwrap_or(keyword);
        self.check_match_prefix()?;
        let mut patterns = vec![self.path_pattern()?];
        while self.eat(&TokenKind::Comma).is_some() {
            patterns.push(self.path_pattern()?);
        }
        let where_clause = if self.eat_kw(Keyword::Where).is_some() {
            Some(self.expr()?)
        } else {
            None
        };
        Ok(MatchClause {
            optional: optional.is_some(),
            patterns,
            where_clause,
            span: start.to(self.last),
        })
    }

    /// Refuses GQL path search prefixes and match modes (`ANY SHORTEST`, `TRAIL`, `DIFFERENT
    /// EDGES`, ...) by name.
    fn check_match_prefix(&self) -> PResult<()> {
        let token = self.peek();
        if token.kind != TokenKind::Ident || self.nth(1).kind == TokenKind::Eq {
            return Ok(());
        }
        const PREFIXES: &[&str] = &[
            "ANY",
            "ALL",
            "WALK",
            "TRAIL",
            "ACYCLIC",
            "SIMPLE",
            "DIFFERENT",
            "REPEATABLE",
            "KEEP",
        ];
        let word = token.text.to_ascii_uppercase();
        if PREFIXES.contains(&word.as_str()) {
            let span = token.span.to(self.nth(1).span);
            let what = if self.nth(1).keyword() == Some(Keyword::Shortest) {
                "shortest path search"
            } else {
                "a path search prefix or match mode"
            };
            return Err(self.not_yet(
                span,
                what,
                "MATCH binds each edge at most once per clause (DIFFERENT EDGES, the GQL default); \
                 write the pattern without a prefix",
            ));
        }
        Ok(())
    }

    fn path_pattern(&mut self) -> PResult<PathPattern> {
        let variable = self.path_variable();
        let token = self.peek();
        if token.kind == TokenKind::Ident
            && (token.text.eq_ignore_ascii_case("shortestPath")
                || token.text.eq_ignore_ascii_case("allShortestPaths"))
        {
            return Err(self.not_yet(
                token.span,
                "shortest path search",
                "Gremlin repeat(), for example \
                 g.v(\"1\").repeat(__.out().simple_path()).until(__.has_id(\"5\")).path().limit(1)",
            ));
        }
        let start = self.node_pattern()?;
        self.check_node_quantifier()?;
        let mut steps = Vec::new();
        while let Some(mut edge) = self.edge_pattern()? {
            self.edge_quantifier(&mut edge)?;
            let node = self.node_pattern()?;
            self.check_node_quantifier()?;
            steps.push(PatternStep { edge, node });
        }
        let first = variable.as_ref().map_or(start.span, |v| v.span);
        Ok(PathPattern {
            span: first.to(self.last),
            variable,
            start,
            steps,
        })
    }

    /// The path variable of `p = ...` with its `=`, if written.
    fn path_variable(&mut self) -> Option<Identifier> {
        let token = self.peek();
        if !matches!(token.kind, TokenKind::Ident | TokenKind::QuotedIdent(_))
            || self.nth(1).kind != TokenKind::Eq
        {
            return None;
        }
        let name = token.name().unwrap_or("").to_string();
        let span = self.advance().span;
        self.advance();
        if !self.variables.contains(&name) {
            self.variables.push(name.clone());
        }
        Some(Identifier { name, span })
    }

    /// Refuses a quantifier after a node pattern: GQL quantifies edges (and parenthesized
    /// paths, not yet supported), never a single node.
    fn check_node_quantifier(&self) -> PResult<()> {
        let token = self.peek();
        if matches!(
            token.kind,
            TokenKind::LBrace | TokenKind::Star | TokenKind::Plus
        ) {
            let b = err(
                GqlErrorKind::Syntax,
                token.span,
                "a quantifier cannot follow a node pattern; it repeats the edge before it",
            )
            .alternative("quantify the edge, for example MATCH (a)-[:knows]->{1,3}(b) RETURN b");
            return Err(Box::new(b));
        }
        Ok(())
    }

    /// A GQL quantifier after `edge` (`{m,n}`, `{m,}`, `{,n}`, `{n}`, `*`, `+`), if written.
    fn edge_quantifier(&mut self, edge: &mut EdgePattern) -> PResult<()> {
        let start = self.peek().span;
        let (min, max) = match self.peek().kind {
            TokenKind::Star => {
                self.advance();
                (0, None)
            }
            TokenKind::Plus => {
                self.advance();
                (1, None)
            }
            TokenKind::LBrace => self.braced_quantifier()?,
            _ => return Ok(()),
        };
        let span = start.to(self.last);
        if let Some(cypher) = &edge.quantifier {
            let b = err(
                GqlErrorKind::Syntax,
                span,
                "an edge takes one quantifier: the Cypher form inside the brackets or a GQL \
                 quantifier after the edge, not both",
            )
            .alternative(format!(
                "keep one of them, for example drop `{}` from the brackets",
                self.text(cypher.span)
            ));
            return Err(Box::new(b));
        }
        edge.quantifier = Some(self.quantifier(min, max, false, span)?);
        Ok(())
    }

    /// `{n}`, `{m,n}`, `{m,}`, `{,n}`, `{,}` from the `{`: the bounds.
    fn braced_quantifier(&mut self) -> PResult<(u64, Option<u64>)> {
        self.expect(&TokenKind::LBrace, &["`{`"])?;
        let lower = self.bound();
        if self.eat(&TokenKind::Comma).is_none() {
            let Some(n) = lower else {
                return Err(self.unexpected(&["an integer", "`,`"]));
            };
            self.expect(&TokenKind::RBrace, &["`,`", "`}`"])?;
            return Ok((n, Some(n)));
        }
        let upper = self.bound();
        if upper.is_none() {
            self.expect(&TokenKind::RBrace, &["an integer", "`}`"])?;
        } else {
            self.expect(&TokenKind::RBrace, &["`}`"])?;
        }
        Ok((lower.unwrap_or(0), upper))
    }

    /// An unsigned integer literal of a quantifier bound, if the current token is one.
    fn bound(&mut self) -> Option<u64> {
        match self.peek().kind {
            TokenKind::Integer(n) => {
                self.advance();
                Some(n)
            }
            _ => None,
        }
    }

    /// The quantifier `min..=max` written at `span`; an error when `min > max`.
    fn quantifier(
        &self,
        min: u64,
        max: Option<u64>,
        cypher: bool,
        span: Span,
    ) -> PResult<Quantifier> {
        if let Some(max) = max
            && min > max
        {
            let b = err(
                GqlErrorKind::Syntax,
                span,
                format!("the quantifier's lower bound {min} is greater than its upper bound {max}"),
            )
            .alternative(if cypher {
                format!("write the smaller bound first, for example *{max}..{min}")
            } else {
                format!("write the smaller bound first, for example {{{max},{min}}}")
            });
            return Err(Box::new(b));
        }
        Ok(Quantifier {
            min,
            max,
            cypher,
            span,
        })
    }

    /// Cypher's variable length inside the brackets of an edge, from the `*`: `*` (`{1,}`),
    /// `*n` (`{n}`), `*m..n`, `*..n` (`{1,n}`), `*m..` (`{m,}`); Memgraph's `*BFS` and the like
    /// are refused.
    fn cypher_quantifier(&mut self) -> PResult<Quantifier> {
        let star = self.peek().span;
        let next = self.nth(1);
        const MEMGRAPH: &[&str] = &["BFS", "DFS", "WSHORTEST", "ALLSHORTEST", "KSHORTEST"];
        if next.kind == TokenKind::Ident
            && MEMGRAPH.iter().any(|m| m.eq_ignore_ascii_case(next.text))
        {
            let b = err(
                GqlErrorKind::Refused,
                star.to(next.span),
                format!(
                    "*{} is a vendor path search extension, not part of GQL",
                    next.text
                ),
            )
            .alternative(
                "Gremlin repeat(), for example \
                 g.v(\"1\").repeat(__.out().simple_path()).until(__.has_id(\"5\")).path()",
            );
            return Err(Box::new(b));
        }
        self.advance();
        let lower = self.bound();
        let (min, max) = if self.eat(&TokenKind::DotDot).is_some() {
            (lower.unwrap_or(1), self.bound())
        } else {
            match lower {
                Some(n) => (n, Some(n)),
                None => (1, None),
            }
        };
        self.quantifier(min, max, true, star.to(self.last))
    }

    /// A variable declaration in a pattern: an identifier or a back-quoted identifier.
    fn pattern_variable(&mut self) -> PResult<Option<Identifier>> {
        let token = self.peek();
        match &token.kind {
            TokenKind::Ident | TokenKind::QuotedIdent(_) => {
                let name = token.name().unwrap_or("").to_string();
                let span = self.advance().span;
                if !self.variables.contains(&name) {
                    self.variables.push(name.clone());
                }
                Ok(Some(Identifier { name, span }))
            }
            _ => Ok(None),
        }
    }

    /// The variable, `:labels` / `IS labels`, inline map and `WHERE` of a node or edge.
    fn filler(&mut self, close: &TokenKind, close_text: &'static str) -> PResult<Filler> {
        let variable = self.pattern_variable()?;
        let token = self.peek();
        if let TokenKind::Keyword(k) = token.kind
            && variable.is_none()
            && !matches!(k, Keyword::Is | Keyword::Where)
        {
            if let Some(e) = self.construct_error() {
                return Err(e);
            }
            let b = err(
                GqlErrorKind::Syntax,
                token.span,
                format!("unexpected keyword {}", token.text),
            )
            .expected(["a variable", "`:`", "IS", "`{`", "WHERE", close_text])
            .alternative(format!(
                "{} is a keyword; to use it as a variable, write it back-quoted: `{}`",
                token.text, token.text
            ));
            return Err(Box::new(b));
        }
        let labels = if self.eat(&TokenKind::Colon).is_some() || self.eat_kw(Keyword::Is).is_some()
        {
            Some(self.label_expr()?.0)
        } else {
            None
        };
        let quantifier = if close == &TokenKind::RBracket && self.at(&TokenKind::Star) {
            Some(self.cypher_quantifier()?)
        } else {
            None
        };
        let properties = if self.at(&TokenKind::LBrace) {
            Some(self.map_literal()?.0)
        } else if let TokenKind::Parameter(_) = self.peek().kind {
            let b = err(
                GqlErrorKind::Syntax,
                self.peek().span,
                "a parameter cannot stand for the inline property map",
            )
            .alternative("write the properties one by one, for example {name: $name}");
            return Err(Box::new(b));
        } else {
            None
        };
        let where_clause = if self.eat_kw(Keyword::Where).is_some() {
            Some(self.expr()?)
        } else {
            None
        };
        if !self.at(close) {
            let mut expected = Vec::new();
            if labels.is_none() && properties.is_none() && where_clause.is_none() {
                if variable.is_none() {
                    expected.push("a variable");
                }
                expected.extend(["`:`", "IS"]);
            }
            if properties.is_none() && where_clause.is_none() {
                expected.push("`{`");
            }
            if where_clause.is_none() {
                expected.push("WHERE");
            }
            expected.push(close_text);
            return Err(self.unexpected(&expected));
        }
        Ok(Filler {
            variable,
            labels,
            properties,
            where_clause,
            quantifier,
        })
    }

    fn node_pattern(&mut self) -> PResult<NodePattern> {
        let open = self.expect(&TokenKind::LParen, &["`(`"])?;
        if self.at(&TokenKind::LParen) {
            return Err(self.not_yet(
                open.to(self.peek().span),
                "a parenthesized path pattern ((a)-->(b)){m,n}",
                "write the path out, or use Gremlin repeat(), for example \
                 g.v(\"1\").repeat(__.out(\"knows\")).times(2)",
            ));
        }
        let Filler {
            variable,
            labels,
            properties,
            where_clause,
            quantifier: _,
        } = self.filler(&TokenKind::RParen, "`)`")?;
        self.expect(&TokenKind::RParen, &["`)`"])?;
        Ok(NodePattern {
            variable,
            labels,
            properties,
            where_clause,
            span: open.to(self.last),
        })
    }

    /// The edge after a node, or `None` at the end of the path.
    fn edge_pattern(&mut self) -> PResult<Option<EdgePattern>> {
        let start = self.peek().span;
        let edge = |direction, abbreviated, span| EdgePattern {
            direction,
            abbreviated,
            variable: None,
            labels: None,
            properties: None,
            where_clause: None,
            quantifier: None,
            span,
        };
        match self.peek().kind {
            TokenKind::Minus => {
                self.advance();
                match self.peek().kind {
                    TokenKind::LBracket => self.bracketed_edge(start, false).map(Some),
                    TokenKind::RightArrow => {
                        self.advance();
                        Ok(Some(edge(EdgeDirection::Right, true, start.to(self.last))))
                    }
                    TokenKind::Minus => {
                        self.advance();
                        Ok(Some(edge(EdgeDirection::Any, true, start.to(self.last))))
                    }
                    TokenKind::LParen => {
                        Ok(Some(edge(EdgeDirection::Any, true, start.to(self.last))))
                    }
                    _ => Err(self.unexpected(&["`[`", "`->`", "`-`", "`(`"])),
                }
            }
            TokenKind::LeftArrow => {
                self.advance();
                match self.peek().kind {
                    TokenKind::LBracket => self.bracketed_edge(start, true).map(Some),
                    TokenKind::Minus => {
                        self.advance();
                        Ok(Some(edge(EdgeDirection::Left, true, start.to(self.last))))
                    }
                    TokenKind::RightArrow => {
                        self.advance();
                        Ok(Some(edge(EdgeDirection::Any, true, start.to(self.last))))
                    }
                    TokenKind::LParen => {
                        Ok(Some(edge(EdgeDirection::Left, true, start.to(self.last))))
                    }
                    // GQL `<->` (left or right), lexed as `<-` `>`.
                    TokenKind::Gt if self.last.touches(self.peek().span) => {
                        self.advance();
                        Ok(Some(edge(EdgeDirection::Any, true, start.to(self.last))))
                    }
                    _ => Err(self.unexpected(&["`[`", "`-`", "`->`", "`(`"])),
                }
            }
            TokenKind::RightArrow => {
                self.advance();
                Ok(Some(edge(EdgeDirection::Right, true, start.to(self.last))))
            }
            TokenKind::Tilde | TokenKind::TildeLeft | TokenKind::TildeRight => {
                Err(self.unexpected(&[]))
            }
            _ => Ok(None),
        }
    }

    /// `[filler]` and the closing arrow, after `-` (`left == false`) or `<-`.
    fn bracketed_edge(&mut self, start: Span, left: bool) -> PResult<EdgePattern> {
        self.expect(&TokenKind::LBracket, &["`[`"])?;
        let Filler {
            variable,
            labels,
            properties,
            where_clause,
            quantifier,
        } = self.filler(&TokenKind::RBracket, "`]`")?;
        self.expect(&TokenKind::RBracket, &["`]`"])?;
        let direction = match (left, &self.peek().kind) {
            (false, TokenKind::RightArrow) => EdgeDirection::Right,
            (false, TokenKind::Minus) => EdgeDirection::Any,
            (true, TokenKind::Minus) => EdgeDirection::Left,
            (true, TokenKind::RightArrow) => EdgeDirection::Any,
            (false, _) => return Err(self.unexpected(&["`->`", "`-`"])),
            (true, _) => return Err(self.unexpected(&["`-`", "`->`"])),
        };
        self.advance();
        Ok(EdgePattern {
            direction,
            abbreviated: false,
            variable,
            labels,
            properties,
            where_clause,
            quantifier,
            span: start.to(self.last),
        })
    }

    // ----- label expressions -----

    /// A label expression after `:`, `IS` or `IS LABELED`, with Cypher `:A:B` chaining.
    fn label_expr(&mut self) -> PResult<(LabelExpr, usize)> {
        let (mut expr, mut height) = self.label_or()?;
        if !self.at(&TokenKind::Colon) {
            return Ok((expr, height));
        }
        // Cypher `:A:B`: plain names only (mixing with `| & !` is ambiguous).
        if !matches!(expr.kind, LabelExprKind::Label(_)) {
            return Err(self.mixed_colon());
        }
        while self.eat(&TokenKind::Colon).is_some() {
            let (right, _) = self.label_or()?;
            if !matches!(right.kind, LabelExprKind::Label(_)) {
                return Err(self.mixed_colon_at(right.span));
            }
            let span = expr.span.to(right.span);
            height = self.height(height, span)?;
            expr = LabelExpr {
                kind: LabelExprKind::And(Box::new(expr), Box::new(right)),
                span,
            };
        }
        Ok((expr, height))
    }

    fn mixed_colon(&self) -> Box<GqlErrorBuilder> {
        self.mixed_colon_at(self.peek().span)
    }

    fn mixed_colon_at(&self, span: Span) -> Box<GqlErrorBuilder> {
        let b = err(
            GqlErrorKind::Syntax,
            span,
            "the Cypher form :A:B takes plain label names and cannot be mixed with | & !",
        )
        .alternative("write the conjunction with &, for example (v:A&(B|C))");
        Box::new(b)
    }

    fn label_or(&mut self) -> PResult<(LabelExpr, usize)> {
        let (mut expr, mut height) = self.label_and()?;
        while self.eat(&TokenKind::Pipe).is_some() {
            let (right, right_height) = self.label_and()?;
            let span = expr.span.to(right.span);
            height = self.height(height.max(right_height), span)?;
            expr = LabelExpr {
                kind: LabelExprKind::Or(Box::new(expr), Box::new(right)),
                span,
            };
        }
        Ok((expr, height))
    }

    fn label_and(&mut self) -> PResult<(LabelExpr, usize)> {
        let (mut expr, mut height) = self.label_not()?;
        while self.eat(&TokenKind::Ampersand).is_some() {
            let (right, right_height) = self.label_not()?;
            let span = expr.span.to(right.span);
            height = self.height(height.max(right_height), span)?;
            expr = LabelExpr {
                kind: LabelExprKind::And(Box::new(expr), Box::new(right)),
                span,
            };
        }
        Ok((expr, height))
    }

    fn label_not(&mut self) -> PResult<(LabelExpr, usize)> {
        self.enter()?;
        let result = self.label_not_inner();
        self.leave();
        result
    }

    fn label_not_inner(&mut self) -> PResult<(LabelExpr, usize)> {
        let start = self.peek().span;
        match self.peek().kind {
            TokenKind::Bang => {
                self.advance();
                let (operand, height) = self.label_not()?;
                let span = start.to(operand.span);
                let height = self.height(height, span)?;
                Ok((
                    LabelExpr {
                        kind: LabelExprKind::Not(Box::new(operand)),
                        span,
                    },
                    height,
                ))
            }
            TokenKind::Percent => {
                self.advance();
                Ok((
                    LabelExpr {
                        kind: LabelExprKind::Wildcard,
                        span: start,
                    },
                    1,
                ))
            }
            TokenKind::LParen => {
                self.advance();
                let (inner, height) = self.label_or()?;
                self.expect(&TokenKind::RParen, &["`)`", "`|`", "`&`"])?;
                Ok((
                    LabelExpr {
                        kind: inner.kind,
                        span: start.to(self.last),
                    },
                    height,
                ))
            }
            _ => {
                let name = self.expect_name(&["a label", "`!`", "`%`", "`(`"])?;
                Ok((
                    LabelExpr {
                        span: name.span,
                        kind: LabelExprKind::Label(name),
                    },
                    1,
                ))
            }
        }
    }

    // ----- RETURN / ORDER BY -----

    fn return_clause(&mut self) -> PResult<ReturnClause> {
        let start = self.expect_kw(Keyword::Return, &["RETURN"])?;
        let (distinct, star, items) = self.projection()?;
        Ok(ReturnClause {
            distinct,
            star,
            items,
            span: start.to(self.last),
        })
    }

    /// `DISTINCT? ('*' (',' item)* | item (',' item)*)` after `RETURN` or `WITH`.
    fn projection(&mut self) -> PResult<(bool, Option<Span>, Vec<ReturnItem>)> {
        let distinct = self.eat_kw(Keyword::Distinct).is_some();
        let mut star = None;
        let mut items = Vec::new();
        if let Some(span) = self.eat(&TokenKind::Star) {
            star = Some(span);
            if self.eat(&TokenKind::Comma).is_none() {
                return Ok((distinct, star, items));
            }
        }
        loop {
            if self.at(&TokenKind::Eof) && items.is_empty() && star.is_none() {
                return Err(self.unexpected(&["`*`", "an expression"]));
            }
            let expr = self.expr()?;
            let alias = if self.eat_kw(Keyword::As).is_some() {
                Some(self.expect_name(&["an alias"])?)
            } else {
                None
            };
            items.push(ReturnItem {
                span: expr.span.to(self.last),
                expr,
                alias,
            });
            if self.eat(&TokenKind::Comma).is_none() {
                break;
            }
        }
        Ok((distinct, star, items))
    }

    fn order_by(&mut self) -> PResult<OrderBy> {
        let start = self.expect_kw(Keyword::Order, &["ORDER BY"])?;
        self.expect_kw(Keyword::By, &["BY"])?;
        let mut items = Vec::new();
        loop {
            let expr = self.expr()?;
            let direction = match self.peek().keyword() {
                Some(Keyword::Asc | Keyword::Ascending) => {
                    self.advance();
                    SortDirection::Asc
                }
                Some(Keyword::Desc | Keyword::Descending) => {
                    self.advance();
                    SortDirection::Desc
                }
                _ => SortDirection::Asc,
            };
            if self.at_kw(Keyword::Nulls) {
                return Err(self.unexpected(&[]));
            }
            items.push(SortItem {
                span: expr.span.to(self.last),
                expr,
                direction,
            });
            if self.eat(&TokenKind::Comma).is_none() {
                break;
            }
        }
        Ok(OrderBy {
            items,
            span: start.to(self.last),
        })
    }

    // ----- expressions -----

    fn expr(&mut self) -> PResult<Expr> {
        Ok(self.expr_bp(0)?.0)
    }

    /// An expression whose operators bind at least as tight as `min_bp`, with its tree height.
    fn expr_bp(&mut self, min_bp: u8) -> PResult<(Expr, usize)> {
        self.enter()?;
        let result = self.expr_bp_inner(min_bp);
        self.leave();
        result
    }

    // The recursive path of an expression (expr_bp -> expr_bp_inner -> prefix -> primary -> a
    // nested form -> expr_bp) is kept to thin dispatch functions: every arm with many
    // temporaries lives in a function of its own, so each nesting level costs little stack, also
    // in unoptimized builds (which give every temporary its own slot).

    fn expr_bp_inner(&mut self, min_bp: u8) -> PResult<(Expr, usize)> {
        let mut acc = self.prefix(min_bp)?;
        let mut compared = false;
        while let Some((infix, bp)) = self.infix() {
            if bp < min_bp {
                break;
            }
            if bp == BP_COMPARE && compared {
                return Err(self.chained_comparison());
            }
            compared = bp == BP_COMPARE;
            acc = match infix {
                Infix::Binary(op, bp) => self.binary_rhs(acc, op, bp + 1)?,
                Infix::LtMinus => self.lt_minus_rhs(acc)?,
                Infix::StartsWith => self.string_rhs(acc, BinaryOp::StartsWith)?,
                Infix::EndsWith => self.string_rhs(acc, BinaryOp::EndsWith)?,
                Infix::Is => self.is_rhs(acc)?,
            };
        }
        Ok(acc)
    }

    fn chained_comparison(&self) -> Box<GqlErrorBuilder> {
        let b = err(
            GqlErrorKind::Syntax,
            self.peek().span,
            "comparisons cannot be chained",
        )
        .alternative("join two comparisons with AND, for example a < b AND b < c");
        Box::new(b)
    }

    /// The operator token, then its right operand at `rbp`.
    fn binary_rhs(&mut self, left: (Expr, usize), op: BinaryOp, rbp: u8) -> PResult<(Expr, usize)> {
        self.advance();
        let right = self.expr_bp(rbp)?;
        self.binary(op, left, right)
    }

    /// `a<-1`: the `<-` token is `<` and a unary minus on the right operand.
    fn lt_minus_rhs(&mut self, left: (Expr, usize)) -> PResult<(Expr, usize)> {
        let arrow = self.advance().span;
        self.pending_minus = Some(Span {
            start: arrow.start.saturating_add(1),
            column: arrow.column.saturating_add(1),
            ..arrow
        });
        let right = self.expr_bp(BP_COMPARE + 1)?;
        self.binary(BinaryOp::Lt, left, right)
    }

    /// `STARTS WITH` / `ENDS WITH` and the right operand.
    fn string_rhs(&mut self, left: (Expr, usize), op: BinaryOp) -> PResult<(Expr, usize)> {
        self.advance();
        self.expect_kw(Keyword::With, &["WITH"])?;
        let right = self.expr_bp(BP_PREDICATE + 1)?;
        self.binary(op, left, right)
    }

    fn binary(
        &self,
        op: BinaryOp,
        (left, left_height): (Expr, usize),
        (right, right_height): (Expr, usize),
    ) -> PResult<(Expr, usize)> {
        let span = left.span.to(right.span);
        let height = self.height(left_height.max(right_height), span)?;
        Ok((
            Expr {
                kind: ExprKind::Binary {
                    op,
                    left: Box::new(left),
                    right: Box::new(right),
                },
                span,
            },
            height,
        ))
    }

    /// `IS [NOT] NULL`, `IS [NOT] LABELED labels`.
    fn is_rhs(&mut self, (operand, height): (Expr, usize)) -> PResult<(Expr, usize)> {
        self.advance();
        let negated = self.eat_kw(Keyword::Not).is_some();
        if self.eat_kw(Keyword::Null).is_some() {
            let span = operand.span.to(self.last);
            let height = self.height(height, span)?;
            return Ok((
                Expr {
                    kind: ExprKind::IsNull {
                        operand: Box::new(operand),
                        negated,
                    },
                    span,
                },
                height,
            ));
        }
        if self.eat_kw(Keyword::Labeled).is_some() {
            let (labels, label_height) = self.label_expr()?;
            let span = operand.span.to(labels.span);
            let height = self.height(height.max(label_height), span)?;
            return Ok((
                Expr {
                    kind: ExprKind::IsLabeled {
                        operand: Box::new(operand),
                        labels,
                        negated,
                    },
                    span,
                },
                height,
            ));
        }
        if let Some(Keyword::True | Keyword::False | Keyword::Unknown) = self.peek().keyword() {
            return Err(self.not_yet(
                self.peek().span,
                "a truth value test (IS TRUE / IS FALSE / IS UNKNOWN)",
                "compare with = TRUE / = FALSE, or test IS NULL",
            ));
        }
        if negated {
            Err(self.unexpected(&["NULL", "LABELED"]))
        } else {
            Err(self.unexpected(&["NOT", "NULL", "LABELED"]))
        }
    }

    /// The infix operator at the current token and its binding power.
    fn infix(&self) -> Option<(Infix, u8)> {
        let token = self.peek();
        let binary = |op, bp| Some((Infix::Binary(op, bp), bp));
        match &token.kind {
            TokenKind::Keyword(k) => match k {
                Keyword::Or => binary(BinaryOp::Or, BP_OR),
                Keyword::Xor => binary(BinaryOp::Xor, BP_XOR),
                Keyword::And => binary(BinaryOp::And, BP_AND),
                Keyword::Is => Some((Infix::Is, BP_PREDICATE)),
                Keyword::In => binary(BinaryOp::In, BP_PREDICATE),
                Keyword::Contains => binary(BinaryOp::Contains, BP_PREDICATE),
                Keyword::Starts if self.nth(1).keyword() == Some(Keyword::With) => {
                    Some((Infix::StartsWith, BP_PREDICATE))
                }
                Keyword::Ends if self.nth(1).keyword() == Some(Keyword::With) => {
                    Some((Infix::EndsWith, BP_PREDICATE))
                }
                _ => None,
            },
            TokenKind::Eq => binary(BinaryOp::Eq, BP_COMPARE),
            TokenKind::Ne => binary(BinaryOp::Ne, BP_COMPARE),
            TokenKind::Lt => binary(BinaryOp::Lt, BP_COMPARE),
            TokenKind::Le => binary(BinaryOp::Le, BP_COMPARE),
            TokenKind::Gt => binary(BinaryOp::Gt, BP_COMPARE),
            TokenKind::Ge => binary(BinaryOp::Ge, BP_COMPARE),
            TokenKind::LeftArrow => Some((Infix::LtMinus, BP_COMPARE)),
            TokenKind::Plus => binary(BinaryOp::Add, BP_ADD),
            TokenKind::Minus => binary(BinaryOp::Sub, BP_ADD),
            TokenKind::Concat => binary(BinaryOp::Concat, BP_ADD),
            TokenKind::Star => binary(BinaryOp::Mul, BP_MUL),
            TokenKind::Slash => binary(BinaryOp::Div, BP_MUL),
            TokenKind::Percent => binary(BinaryOp::Mod, BP_MUL),
            _ => None,
        }
    }

    /// A prefix operator with its operand, or a postfix expression.
    fn prefix(&mut self, min_bp: u8) -> PResult<(Expr, usize)> {
        if let Some(minus) = self.pending_minus.take() {
            return self.negate(minus);
        }
        match self.peek().kind {
            TokenKind::Keyword(Keyword::Not) => self.not_prefix(min_bp),
            TokenKind::Minus => {
                let minus = self.advance().span;
                self.negate(minus)
            }
            TokenKind::Plus => self.plus_prefix(),
            _ => {
                let (primary, height) = self.primary()?;
                self.postfix(primary, height)
            }
        }
    }

    fn not_prefix(&mut self, min_bp: u8) -> PResult<(Expr, usize)> {
        let start = self.peek().span;
        // Allowed where a NOT operand or anything looser may stand (`NOT NOT x`), not as the
        // operand of a comparison or a tighter operator (`a = NOT b`).
        if min_bp > BP_NOT + 1 {
            let b = err(
                GqlErrorKind::Syntax,
                start,
                "NOT binds looser than this operator",
            )
            .alternative("put the negated condition in parentheses: (NOT x)");
            return Err(Box::new(b));
        }
        self.advance();
        let operand = self.expr_bp(BP_NOT + 1)?;
        self.unary(UnaryOp::Not, start, operand)
    }

    fn plus_prefix(&mut self) -> PResult<(Expr, usize)> {
        let start = self.advance().span;
        let operand = self.expr_bp(BP_UNARY)?;
        self.unary(UnaryOp::Plus, start, operand)
    }

    /// The operand of a unary minus at `minus`; a number literal right after it is folded.
    fn negate(&mut self, minus: Span) -> PResult<(Expr, usize)> {
        if let Some(literal) = self.folded_number(minus)? {
            return self.postfix(literal, 1);
        }
        let operand = self.expr_bp(BP_UNARY)?;
        self.unary(UnaryOp::Minus, minus, operand)
    }

    /// A number literal right after a minus, negated (`-9223372036854775808` is `i64::MIN`).
    fn folded_number(&mut self, minus: Span) -> PResult<Option<Expr>> {
        let token = self.peek();
        let literal = match token.kind {
            TokenKind::Integer(n) => match 0i64.checked_sub_unsigned(n) {
                Some(value) => Literal::Integer(value),
                None => return Err(self.syntax(token.span, out_of_range(token.text))),
            },
            TokenKind::Float(f) => Literal::Float(-f),
            _ => return Ok(None),
        };
        self.advance();
        Ok(Some(Expr {
            kind: ExprKind::Literal(literal),
            span: minus.to(self.last),
        }))
    }

    fn unary(
        &self,
        op: UnaryOp,
        start: Span,
        (operand, height): (Expr, usize),
    ) -> PResult<(Expr, usize)> {
        let span = start.to(operand.span);
        let height = self.height(height, span)?;
        Ok((
            Expr {
                kind: ExprKind::Unary {
                    op,
                    operand: Box::new(operand),
                },
                span,
            },
            height,
        ))
    }

    /// Property access `.key` and Cypher `:Label` after a primary.
    fn postfix(&mut self, mut expr: Expr, mut height: usize) -> PResult<(Expr, usize)> {
        loop {
            match self.peek().kind {
                TokenKind::Dot => {
                    self.advance();
                    let key = self.expect_name(&["a property name"])?;
                    let span = expr.span.to(key.span);
                    if self.at(&TokenKind::LParen) {
                        return Err(self.namespaced_function(&expr, &key, span));
                    }
                    height = self.height(height, span)?;
                    expr = Expr {
                        kind: ExprKind::Property {
                            target: Box::new(expr),
                            key,
                        },
                        span,
                    };
                }
                TokenKind::Colon => {
                    self.advance();
                    let (labels, label_height) = self.label_expr()?;
                    let span = expr.span.to(labels.span);
                    height = self.height(height.max(label_height), span)?;
                    expr = Expr {
                        kind: ExprKind::IsLabeled {
                            operand: Box::new(expr),
                            labels,
                            negated: false,
                        },
                        span,
                    };
                }
                TokenKind::LBracket => {
                    return Err(self.not_yet(
                        self.peek().span,
                        "list indexing and slicing (x[0], x[1..2])",
                        "",
                    ));
                }
                TokenKind::LBrace if matches!(expr.kind, ExprKind::Variable(_)) => {
                    return Err(self.not_yet(
                        expr.span.to(self.peek().span),
                        "a map projection (v {.name})",
                        "return the properties one by one, for example RETURN v.name AS name",
                    ));
                }
                _ => return Ok((expr, height)),
            }
        }
    }

    /// The error for `a.b.c(...)`: a vendor function or procedure.
    fn namespaced_function(
        &self,
        target: &Expr,
        key: &Identifier,
        span: Span,
    ) -> Box<GqlErrorBuilder> {
        let mut root = target;
        while let ExprKind::Property { target, .. } = &root.kind {
            root = target;
        }
        let name = format!("{}()", self.text(span));
        let vendor = matches!(&root.kind, ExprKind::Variable(id)
            if ["apoc", "db", "dbms", "gds", "genai", "tx"]
                .iter()
                .any(|v| v.eq_ignore_ascii_case(&id.name)));
        let message = if vendor {
            format!("{name} is a vendor function, not part of GQL")
        } else {
            format!(
                "{name}: namespaced functions are not supported (`.{}` is not a property read here)",
                key.name
            )
        };
        let b = err(GqlErrorKind::Refused, span, message).alternative(
            "write what the function does as Gremlin steps after gql(), or use one of the GQL \
             functions (element_id, labels, type, coalesce, upper, lower, char_length, size)",
        );
        Box::new(b)
    }

    fn text(&self, span: Span) -> &str {
        self.src.get(span.start..span.end).unwrap_or("")
    }

    /// Dispatches on the first token of a primary expression (nested forms in own functions).
    fn primary(&mut self) -> PResult<(Expr, usize)> {
        match &self.peek().kind {
            TokenKind::LParen => self.parenthesized(),
            TokenKind::LBracket => self.list(),
            TokenKind::LBrace => self.map_expr(),
            TokenKind::Keyword(Keyword::Case) => self.case_expr(),
            TokenKind::Ident | TokenKind::QuotedIdent(_) | TokenKind::Keyword(_)
                if self.nth(1).kind == TokenKind::LParen
                    && self.peek().keyword() != Some(Keyword::Exists) =>
            {
                self.function_call()
            }
            _ => self.atom().map(|atom| (atom, 1)),
        }
    }

    /// A literal, parameter or variable (never recursive).
    fn atom(&mut self) -> PResult<Expr> {
        let token = self.peek();
        let span = token.span;
        let kind = match &token.kind {
            TokenKind::Integer(n) => match i64::try_from(*n) {
                Ok(value) => ExprKind::Literal(Literal::Integer(value)),
                Err(_) => return Err(self.syntax(span, out_of_range(token.text))),
            },
            TokenKind::Float(f) => ExprKind::Literal(Literal::Float(*f)),
            TokenKind::Str(s) => ExprKind::Literal(Literal::String(s.clone())),
            TokenKind::Parameter(name) => ExprKind::Parameter(name.clone()),
            TokenKind::Keyword(Keyword::True) => ExprKind::Literal(Literal::Bool(true)),
            TokenKind::Keyword(Keyword::False) => ExprKind::Literal(Literal::Bool(false)),
            TokenKind::Keyword(Keyword::Null) => ExprKind::Literal(Literal::Null),
            TokenKind::Ident
                if self.nth(1).kind == TokenKind::LBrace
                    && ["count", "collect", "exists"]
                        .iter()
                        .any(|n| n.eq_ignore_ascii_case(token.text)) =>
            {
                return Err(self.not_yet(
                    span.to(self.nth(1).span),
                    &format!("a {} {{ }} subquery", token.text.to_ascii_uppercase()),
                    "add the pattern to the MATCH clause, or use Gremlin where(), for example \
                     g.v().has_label(\"person\").where(__.out(\"created\"))",
                ));
            }
            TokenKind::Ident | TokenKind::QuotedIdent(_) => ExprKind::Variable(Identifier {
                name: token.name().unwrap_or("").to_string(),
                span,
            }),
            _ => return Err(self.unexpected(&["an expression"])),
        };
        self.advance();
        Ok(Expr { kind, span })
    }

    fn parenthesized(&mut self) -> PResult<(Expr, usize)> {
        let open = self.advance().span;
        let (inner, height) = self.expr_bp(0)?;
        self.expect(&TokenKind::RParen, &["`)`", "an operator"])?;
        Ok((
            Expr {
                kind: inner.kind,
                span: open.to(self.last),
            },
            height,
        ))
    }

    fn list(&mut self) -> PResult<(Expr, usize)> {
        let open = self.advance().span;
        self.check_list_comprehension(open)?;
        let (items, height) = self.expr_list(&TokenKind::RBracket, &["`,`", "`]`"])?;
        let span = open.to(self.last);
        let height = self.height(height, span)?;
        Ok((
            Expr {
                kind: ExprKind::List(items),
                span,
            },
            height,
        ))
    }

    fn check_list_comprehension(&self, open: Span) -> PResult<()> {
        if matches!(
            self.peek().kind,
            TokenKind::Ident | TokenKind::QuotedIdent(_)
        ) && self.nth(1).keyword() == Some(Keyword::In)
        {
            return Err(self.not_yet(
                open,
                "a list comprehension [x IN list | ...]",
                "Gremlin unfold() and fold(), for example \
                 g.inject([1, 2, 3]).unfold().is(P.gt(1)).fold()",
            ));
        }
        Ok(())
    }

    /// Comma-separated expressions up to and including `close` (possibly none), with the
    /// greatest height among them.
    fn expr_list(&mut self, close: &TokenKind, expected: &[&str]) -> PResult<(Vec<Expr>, usize)> {
        let mut items = Vec::new();
        let mut height = 0;
        if self.eat(close).is_some() {
            return Ok((items, height));
        }
        loop {
            let (item, item_height) = self.expr_bp(0)?;
            height = height.max(item_height);
            items.push(item);
            if self.eat(&TokenKind::Comma).is_none() {
                break;
            }
        }
        self.expect(close, expected)?;
        Ok((items, height))
    }

    fn map_expr(&mut self) -> PResult<(Expr, usize)> {
        let (map, height) = self.map_literal()?;
        Ok((
            Expr {
                span: map.span,
                kind: ExprKind::Map(map),
            },
            height,
        ))
    }

    fn map_literal(&mut self) -> PResult<(MapLiteral, usize)> {
        let open = self.expect(&TokenKind::LBrace, &["`{`"])?;
        let mut entries = Vec::new();
        let mut height = 0;
        if self.eat(&TokenKind::RBrace).is_none() {
            loop {
                let key = self.expect_name(&["a property name"])?;
                self.expect(&TokenKind::Colon, &["`:`"])?;
                let (value, value_height) = self.expr_bp(0)?;
                height = height.max(value_height);
                entries.push(MapEntry { key, value });
                if self.eat(&TokenKind::Comma).is_none() {
                    break;
                }
            }
            self.expect(&TokenKind::RBrace, &["`,`", "`}`"])?;
        }
        let span = open.to(self.last);
        let height = self.height(height, span)?;
        Ok((MapLiteral { entries, span }, height))
    }

    fn case_expr(&mut self) -> PResult<(Expr, usize)> {
        let start = self.advance().span;
        let mut height = 0;
        let operand = if self.at_kw(Keyword::When) {
            None
        } else {
            let (operand, operand_height) = self.expr_bp(0)?;
            height = operand_height;
            Some(operand)
        };
        let mut whens = Vec::new();
        while self.eat_kw(Keyword::When).is_some() {
            let (when, when_height) = self.expr_bp(0)?;
            self.expect_kw(Keyword::Then, &["THEN"])?;
            let (then, then_height) = self.expr_bp(0)?;
            height = height.max(when_height).max(then_height);
            whens.push(CaseWhen { when, then });
        }
        if whens.is_empty() {
            return Err(self.unexpected(&["WHEN"]));
        }
        let else_result = if self.eat_kw(Keyword::Else).is_some() {
            let (result, result_height) = self.expr_bp(0)?;
            height = height.max(result_height);
            Some(result)
        } else {
            None
        };
        self.case_end(&else_result)?;
        let span = start.to(self.last);
        let height = self.height(height, span)?;
        Ok((
            Expr {
                kind: ExprKind::Case(Box::new(CaseExpr {
                    operand,
                    whens,
                    else_result,
                })),
                span,
            },
            height,
        ))
    }

    fn case_end(&mut self, else_result: &Option<Expr>) -> PResult<Span> {
        let expected: &[&str] = if else_result.is_some() {
            &["END"]
        } else {
            &["WHEN", "ELSE", "END"]
        };
        self.expect_kw(Keyword::End, expected)
    }

    fn function_call(&mut self) -> PResult<(Expr, usize)> {
        let spec = self.function_spec()?;
        let token = self.advance();
        let name = Identifier {
            name: token.name().unwrap_or("").to_string(),
            span: token.span,
        };
        self.expect(&TokenKind::LParen, &["`(`"])?;
        if spec.kind == FunctionKind::Aggregate {
            return self.aggregate_call(spec, name);
        }
        self.check_call_modifiers(spec)?;
        let (args, height) = self.expr_list(&TokenKind::RParen, &["`,`", "`)`"])?;
        self.finish_call(spec, name, false, args, height)
    }

    /// The arguments of an aggregate after its `(`: `DISTINCT` allowed, `*` only for `count`
    /// (and not with `DISTINCT`). Where an aggregate may stand is the binder's check.
    fn aggregate_call(&mut self, spec: &FunctionSpec, name: Identifier) -> PResult<(Expr, usize)> {
        let distinct = self.eat_kw(Keyword::Distinct).is_some();
        if self.at(&TokenKind::Star) {
            return self.count_star(spec, name, distinct);
        }
        let (args, height) = self.expr_list(&TokenKind::RParen, &["`,`", "`)`"])?;
        self.finish_call(spec, name, distinct, args, height)
    }

    /// `count(*)` from the `*`.
    fn count_star(
        &mut self,
        spec: &FunctionSpec,
        name: Identifier,
        distinct: bool,
    ) -> PResult<(Expr, usize)> {
        let star = self.peek().span;
        if spec.function != Function::Count {
            return Err(self.syntax(
                star,
                format!("only count(*) takes `*`, not {}()", spec.name()),
            ));
        }
        if distinct {
            let b = err(
                GqlErrorKind::Syntax,
                star,
                "count(DISTINCT *) is not allowed: DISTINCT needs an expression",
            )
            .alternative(
                "count(*) counts the rows; count(DISTINCT x) counts the distinct values of x",
            );
            return Err(Box::new(b));
        }
        self.advance();
        self.expect(&TokenKind::RParen, &["`)`"])?;
        let span = name.span.to(self.last);
        Ok((
            Expr {
                kind: ExprKind::FunctionCall {
                    name,
                    distinct: false,
                    args: CallArgs::Star(star),
                },
                span,
            },
            1,
        ))
    }

    /// The table entry of the function named by the current token, or the error for an unknown,
    /// refused or not yet supported one.
    fn function_spec(&self) -> PResult<&'static FunctionSpec> {
        let token = self.peek();
        let name = token.name().unwrap_or("");
        let span = token.span;
        let spec = lookup_function(name);
        if spec.is_none() && name.eq_ignore_ascii_case("id") {
            return Err(self.cypher_id(span));
        }
        if name.eq_ignore_ascii_case("shortestPath")
            || name.eq_ignore_ascii_case("allShortestPaths")
        {
            return Err(self.not_yet(
                span,
                "shortest path search",
                "Gremlin repeat(), for example \
                 g.v(\"1\").repeat(__.out().simple_path()).until(__.has_id(\"5\")).path().limit(1)",
            ));
        }
        let Some(spec) = spec else {
            let mut b = err(
                GqlErrorKind::Syntax,
                span,
                format!("unknown function {name}()"),
            );
            if let Some(close) = suggest_function(name) {
                b = b.suggestion(format!("{close}()"));
            }
            return Err(Box::new(b));
        };
        Ok(spec)
    }

    /// Refuses `DISTINCT` and `*` in a scalar function call.
    fn check_call_modifiers(&self, spec: &FunctionSpec) -> PResult<()> {
        let token = self.peek();
        if token.keyword() == Some(Keyword::Distinct) {
            return Err(self.syntax(
                token.span,
                format!(
                    "DISTINCT is only allowed in aggregate functions, not in {}()",
                    spec.name()
                ),
            ));
        }
        if token.kind == TokenKind::Star {
            return Err(self.syntax(
                token.span,
                format!("only count(*) takes `*`, not {}()", spec.name()),
            ));
        }
        Ok(())
    }

    fn finish_call(
        &self,
        spec: &FunctionSpec,
        name: Identifier,
        distinct: bool,
        args: Vec<Expr>,
        height: usize,
    ) -> PResult<(Expr, usize)> {
        let span = name.span.to(self.last);
        if args.len() < spec.min_args || args.len() > spec.max_args {
            let wanted = match (spec.min_args, spec.max_args) {
                (min, usize::MAX) => format!("at least {min} argument{}", plural(min)),
                (min, max) if min == max => format!("{min} argument{}", plural(min)),
                (min, max) => format!("{min} to {max} arguments"),
            };
            return Err(self.syntax(
                span,
                format!("{}() takes {wanted}, got {}", spec.name(), args.len()),
            ));
        }
        let height = self.height(height, span)?;
        Ok((
            Expr {
                kind: ExprKind::FunctionCall {
                    name,
                    distinct,
                    args: CallArgs::Exprs(args),
                },
                span,
            },
            height,
        ))
    }

    /// Cypher `id(v)`, refused for `element_id(v)` (D15), naming the argument as written.
    fn cypher_id(&self, span: Span) -> Box<GqlErrorBuilder> {
        let mut argument = "v".to_string();
        // Read the argument text up to the matching `)` without parsing it.
        let mut depth = 0usize;
        let mut end = None;
        let open = self.nth(1).span;
        for i in 1.. {
            let token = self.nth(i);
            match token.kind {
                TokenKind::LParen => depth += 1,
                TokenKind::RParen => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        end = Some(token.span.start);
                        break;
                    }
                }
                TokenKind::Eof => break,
                _ => {}
            }
        }
        if let Some(end) = end {
            let text = self.src.get(open.end..end).unwrap_or("").trim();
            if !text.is_empty() {
                argument = text.to_string();
            }
        }
        let b = err(
            GqlErrorKind::Refused,
            span,
            "id() is not supported: in Cypher it returns an integer, Graphersal ids are strings",
        )
        .alternative(format!(
            "use element_id({argument}), the element's id as a string"
        ));
        Box::new(b)
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn out_of_range(text: &str) -> String {
    format!(
        "integer literal {text} is out of range (the largest integer is {})",
        i64::MAX
    )
}

/// How a token is named in "unexpected ...".
fn describe(token: &Token<'_>) -> String {
    if token.kind == TokenKind::Eof {
        return token.kind.describe().into();
    }
    let mut text: String = token.text.chars().take(30).collect();
    if token.text.chars().count() > 30 {
        text.push_str("...");
    }
    match token.kind {
        TokenKind::Keyword(_) => format!("keyword {text}"),
        _ => format!("`{text}`"),
    }
}

/// The edit distance of two strings (Levenshtein, over characters).
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row.first().copied().unwrap_or(0);
        if let Some(first) = row.first_mut() {
            *first = i + 1;
        }
        for (j, cb) in b.iter().enumerate() {
            let above = row.get(j + 1).copied().unwrap_or(usize::MAX);
            let left = row.get(j).copied().unwrap_or(usize::MAX);
            let cost = usize::from(ca != *cb);
            let value = (diagonal.saturating_add(cost))
                .min(above.saturating_add(1))
                .min(left.saturating_add(1));
            diagonal = above;
            if let Some(cell) = row.get_mut(j + 1) {
                *cell = value;
            }
        }
    }
    row.last().copied().unwrap_or(0)
}

/// Whether `word` is close enough to `candidate` (case-insensitive) to suggest it (also the
/// binder's variable and parameter suggestions).
pub(crate) fn close_to(word: &str, candidate: &str) -> Option<usize> {
    let distance = edit_distance(&word.to_ascii_lowercase(), &candidate.to_ascii_lowercase());
    let allowed = if candidate.chars().count() <= 4 { 1 } else { 2 };
    (distance > 0 && distance <= allowed).then_some(distance)
}

/// The expected keyword closest to `word`, as written in `expected` (`ORDER BY` for `ODER`).
fn suggest_keyword(word: &str, expected: &[&str]) -> Option<String> {
    expected
        .iter()
        .filter_map(|e| {
            let first = e.split(' ').next().unwrap_or("");
            Keyword::from_word(first)?;
            close_to(word, first).map(|d| (d, *e))
        })
        .min_by_key(|(d, _)| *d)
        .map(|(_, e)| e.to_string())
}

/// The function name closest to `word` (every accepted spelling).
fn suggest_function(word: &str) -> Option<&'static str> {
    FUNCTIONS
        .iter()
        .flat_map(|spec| spec.names.iter().copied())
        .chain(["element_id"])
        .filter_map(|name| close_to(word, name).map(|d| (d, name)))
        .min_by_key(|(d, _)| *d)
        .map(|(_, name)| name)
}

#[cfg(test)]
mod tests;
