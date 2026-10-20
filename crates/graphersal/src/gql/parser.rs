//! Recursive-descent parser of GQL read queries (stages 1a to 2d): tokens to the [`ast`](super::ast) tree.
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
//! query      := composite ';'?
//! composite  := block (NEXT block)*                 -- NEXT binds looser than UNION
//! block      := linear (UNION (ALL | DISTINCT)? linear)*   -- ALL: a contextual word
//! linear     := stage* return order_by? (OFFSET|SKIP expr)? (LIMIT expr)?
//! stage      := match | with | let | filter | for | call
//! call       := OPTIONAL? CALL ('(' (var (',' var)* | '*')? ')')? '{' composite '}'
//! match      := OPTIONAL? MATCH path (',' path)* (WHERE expr)?
//! with       := WITH projection order_by? (OFFSET|SKIP expr)? (LIMIT expr)? (WHERE expr)?
//! let        := LET var '=' expr (',' var '=' expr)*
//! filter     := FILTER WHERE? expr
//! for        := FOR var IN expr (WITH (ORDINALITY | OFFSET) var)?   -- ORDINALITY: a contextual word
//!             | UNWIND expr AS var
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
//! return     := RETURN projection group_by?
//! group_by   := GROUP BY ('(' ')' | expr (',' expr)*)   -- GROUP: a name, not a reserved word
//! projection := DISTINCT? ('*' (',' item)* | item (',' item)*)
//! item       := expr (AS name)?
//! order_by   := ORDER BY expr (ASC|ASCENDING|DESC|DESCENDING)? (',' ...)*
//! fcall      := name '(' (expr (',' expr)*)? ')'
//!             | aggregate '(' DISTINCT? expr ')' | count '(' '*' ')'
//!             | CAST '(' expr AS type ')'                 -- type: one or two words
//!             | TRIM '(' (LEADING|TRAILING|BOTH)? expr? FROM expr ')'
//! is_pred    := expr IS NOT? (NULL | LABELED labels | SOURCE OF expr | DESTINATION OF expr)
//! exists     := EXISTS ('{' body '}' | '(' body ')') | (COUNT | COLLECT) '{' body '}'
//! body       := match+ | path (',' path)* (WHERE expr)?   -- MATCH without OPTIONAL (stage 2b)
//!             | composite                                 -- braces only (stage 2d)
//! pattern_pred := path                 -- where an expression stands; found by lookahead
//! map_proj   := var '{' (proj_item (',' proj_item)*)? '}'     -- Cypher map projection
//! proj_item  := '.' name | '.' '*' | var | name ':' expr
//! list_comp  := '[' var IN expr (WHERE expr)? ('|' expr)? ']'
//! pattern_comp := '[' path (WHERE expr)? '|' expr ']'      -- path with an edge, `p =` allowed
//! ```
//!
//! Expressions, loosest first: `OR` < `XOR` < `AND` < `NOT` < comparison (`= <> != < <= > >=`,
//! not chainable: `a < b < c` is an error) < predicates (`IS [NOT] NULL`, `IS [NOT] LABELED`,
//! `IS [NOT] SOURCE OF` / `DESTINATION OF` (GQL G112; the right operand binds like the one of
//! `IN`),
//! `IN`, `STARTS WITH`, `ENDS WITH`, `CONTAINS`; tighter than comparison as in openCypher, so
//! `a = b IS NULL` is `a = (b IS NULL)`) < additive (`+ - ||`) < multiplicative (`* / %`) <
//! unary (`- +`) < postfix (`.key`, Cypher `v:Label`, an alias of `v IS LABELED Label`, a map
//! projection `v {..}`) < primary (literals, `[list]`, `{map}`, `$param`, variables, function
//! calls, `CASE`, `( )`, `EXISTS { }` / `EXISTS ( )`, pattern predicates, list and pattern
//! comprehensions).
//! A minus directly before an integer or float literal is folded into the literal, so
//! `-9223372036854775808` is `i64::MIN`; the same digits without the minus are out of range.
//!
//! # Decisions
//!
//! - **`RETURN` is mandatory** (design D10); its absence is a syntax error whose alternative names
//!   `RETURN *` and the query's own variables.
//! - **A query may start without `MATCH`** (stage 2a, design M1 / E4): with `FOR` / `UNWIND`,
//!   `LET`, `WITH`, `FILTER` or directly with `RETURN` (one unit row); [`LinearQuery::stages`] is then
//!   empty or starts with that stage. ISO GQL allows `FILTER` as the first statement too, and
//!   it is a plain filter of the unit row, so it is accepted like the others. A first token that
//!   starts no clause expects every clause a query may start with (`MATCH`, `OPTIONAL MATCH`,
//!   `WITH`, `LET`, `FILTER`, `FOR`, `UNWIND`, `RETURN`), the same list as after a clause.
//! - **Result tail in GQL order:** `ORDER BY`, then `OFFSET` / `SKIP`, then `LIMIT`, each at most
//!   once, as GQL and Neo4j write them. Another order is a syntax error naming the right one; the
//!   standalone Cypher 25 forms before `RETURN` are "not yet supported".
//! - **`GROUP BY`** (ISO GQL, stage 1c) follows the `RETURN` items, before `ORDER BY`. Grouping
//!   stays implicit (the items without an aggregate are the keys); the binder checks that the
//!   keys list exactly those items (by column name or by the item's expression text, a
//!   convenience beyond ISO's variable references) and that `()` stands alone with aggregates
//!   only. On `WITH` (Cypher, no ISO counterpart) it stays a syntax error explaining implicit
//!   grouping.
//! - **Label `IS` form:** `(v IS A)`, `[e IS T]` (GQL `<is label expression>`) are accepted beside
//!   `:`; Cypher `:A:B` is an alias of `A&B` (plain names only, mixing with `| & !` is refused as
//!   ambiguous). In expressions `v IS [NOT] LABELED A` and Cypher `v:A`.
//! - **Functions are checked here:** the name (with "did you mean"), the argument count and
//!   `DISTINCT` / `*`, against the one table [`FUNCTIONS`] that the binder and the evaluator also
//!   read. Aggregates take `DISTINCT`; only `count` takes `*`, and `count(DISTINCT *)` is an
//!   error. Where an aggregate may stand (not nested, not in `WHERE`, ...) is the binder's check.
//!   `path_length(p)` has the Cypher alias `length(p)`. Cypher `id()` is refused for
//!   `element_id()` (D15). Stage 1c appends the scalar functions of the design's catalogue
//!   (conversions, strings, numbers, lists / elements / paths, including the openCypher
//!   compatibility functions `nodes(p)` / `relationships(p)` of D21, which reverse C12) after
//!   the older rows, so the "did you mean" of a misspelt older name is unchanged. Names of
//!   functions outside the support get a targeted error from [`NOT_YET_FUNCTIONS`] (reason and
//!   alternative; `rand()` / `randomUUID()` are refused for good) instead of "unknown
//!   function"; Cypher's `^` operator is "not yet" naming `power(x, y)`.
//! - **Special argument forms** (design K2): `CAST` and `TRIM` are ordinary [`FUNCTIONS`] rows
//!   whose arguments are read differently. `CAST(x AS type)` takes the type names of
//!   `CAST_TYPES` (one or two contextual words, `SIGNED INTEGER`, `DOUBLE PRECISION`, `CHAR
//!   VARYING`); a GQL type Graphersal has no values of (`DATE`, `LIST`, `INT32`, ...) is "not
//!   yet supported", any other name a syntax error with a suggestion. `TRIM`'s SQL forms
//!   (`LEADING` / `TRAILING` / `BOTH` and `FROM` are contextual words: a side word before `)`,
//!   `,`, `.` or an operator is a variable) become [`CallArgs::Trim`]; `TRIM(s)` and `trim(list,
//!   n)` stay plain calls; `TRIM(LEADING s)` without `FROM` is a syntax error naming the form.
//!   `SUBSTRING(s FROM a FOR b)` is refused with `substring(s, start, length)` (0-based) and
//!   `left` / `right`. `IS SOURCE OF` / `IS DESTINATION OF` are contextual words after `IS`.
//! - **The clause pipeline** usually starts with `[OPTIONAL] MATCH` (a leading `OPTIONAL MATCH`
//!   too). `WITH` is Cypher's spelling of the GQL linear pipeline; its parts come in
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
//! - **`FOR` / `UNWIND`** ([`ForClause`]): GQL `FOR x IN expr [WITH ORDINALITY i | WITH OFFSET
//!   i]` and its Cypher alias `UNWIND expr AS x` (one meaning, no ordinal). The variables are
//!   plain names (a keyword must be back-quoted). `ORDINALITY` is a contextual word, not a
//!   keyword, so `ordinality` stays usable as a variable: after a `FOR` clause, `WITH OFFSET`
//!   always starts the ordinal form, `WITH ORDINALITY` does when a name follows it, or when the
//!   query ends or a clause keyword follows and no variable `ordinality` was declared (then the
//!   name is missing: a syntax error); otherwise `WITH` starts a `WITH` clause. A missing `IN`
//!   (or `AS` after `UNWIND`) is a syntax error naming the form; an ordinal after `UNWIND` is
//!   refused naming `FOR`. The binder checks the names (redefinition, `i` equal to `x`).
//! - **`EXISTS` and pattern predicates** (stage 2b, design item 6, [`SubqueryExpr`]): ISO
//!   `EXISTS { MATCH .. }`, `EXISTS { pattern, .. [WHERE c] }` and the same in parentheses, and
//!   the Cypher pattern predicate `(a)-[:T]->(b)` where an expression stands (an alias of
//!   `EXISTS { pattern }`, one path, no `WHERE` of its own; that it introduces no named variable
//!   is the binder's check). The body holds non-optional `MATCH` clauses only: `OPTIONAL MATCH`
//!   and the other clauses (`WITH`, `RETURN`, `ORDER BY`, ..., a nested query) are "not yet
//!   supported" there; stage 2d reads a braced body with such clauses as a whole query (below).
//!   Cypher 4 `exists(value)` (a token after `EXISTS (` that starts no body) is refused for
//!   good, naming `n.p IS NOT NULL`. A subquery is one level taller than the tallest expression or label tree
//!   of its body, so [`MAX_HEIGHT`] bounds the body with its host; names declared inside do
//!   not feed the missing-`RETURN` alternative.
//! - **Pattern-predicate lookahead.** At `(` where an expression stands, a bounded token scan
//!   (no parse, no backtracking: re-parsing a failed speculative pattern as an expression is
//!   exponential in the nesting) decides: skip the balanced group, then read the edge. One that
//!   contains `->` (`-->`, `->`, `-[..]->`, `<-->`, `<->`) is never an operator: a pattern. A
//!   strong edge (`--` / `<--` with adjacent characters, `-[..]-`, `<-[..]-`) is a pattern when
//!   a node `(` follows (after a quantifier, if any). A weak edge (`-` or `<-` alone, `- -` with
//!   a space) followed by `(` continues the scan (at most 64 edges); anything else is a
//!   parenthesized expression. So `(a) - (b)` and `(a)-1` stay arithmetic, `(a)<-(b)` stays
//!   `a < -b`, `(a) - [1]` stays list arithmetic; `(a)--(b)`, `(a)-->(b)` and `(a)-(b)-->(c)` are
//!   patterns (Cypher's reading wins over `a - -b`, which is written without parentheses).
//! - **Map projections and comprehensions** (stage 2c, design item 7; openCypher compatibility
//!   forms, the GQL counterpart of a map projection is the record constructor `{k: expr}`): a
//!   variable followed by `{` is a map projection ([`MapProjection`]; items `.key`, `.*`, a
//!   variable, `key: expr`; a keyword as a variable item must be back-quoted). `[` followed by a
//!   name and `IN` is a list comprehension ([`ListComprehension`]), so a list whose first item is
//!   a membership test is written with parentheses, `[(x IN l), y]`. `[` followed by a path
//!   pattern (the pattern-predicate lookahead, after an optional `p =`) whose end is followed by
//!   `WHERE` or `|` is a pattern comprehension ([`PatternComprehension`]); without them the
//!   bracket stays a list literal holding a pattern predicate, as in stage 2b. A token scan
//!   ([`Parser::pattern_end`]) finds the pattern's end, no backtracking. Inside the list and the
//!   `WHERE` of a comprehension, a `|` at the comprehension's own bracket level ends the part
//!   instead of continuing a label expression (`[x IN l WHERE x:A | x.name]`); a label
//!   alternative there is written in parentheses, `x:(A|B)`.
//! - **List predicates and `reduce`** (stage 2d, user decision D23; openCypher compatibility
//!   forms): `all` / `any` / `none` / `single(x IN l WHERE c)` (the `WHERE` is required) and
//!   `reduce(acc = init, x IN l | expr)` are recognized by name (any case, before `(`, ahead of
//!   the function table, [`list_function`]) and parse into a [`ListComprehension`] with its
//!   [`ComprehensionForm`]; inside `reduce` a `|` at its own bracket level ends the list, as in
//!   a list comprehension.
//! - **Record constructor** (stage 2c): `RECORD {k: expr, ...}` is the map literal
//!   `{k: expr, ...}` with the ISO keyword written out ([`Parser::record_ahead`]). `RECORD` is
//!   contextual (an identifier, any case): it is the keyword when `{` follows and then `}` or a
//!   name and `:` (`RECORD {}`, `record {name: p.name}`); any other `{` after it (`.key`, `.*`,
//!   a variable item) keeps it a variable with a map projection, as `record {.name}`. So a
//!   variable named `record` is read everywhere except before an empty projection or one that
//!   starts with a `key: expr` item; there it is back-quoted, `` `record` {k: 1} ``.
//! - **Composite queries** (stage 2d, design item 8): a [`Query`] is a [`CompositeQuery`] of
//!   linear queries joined by `UNION [ALL | DISTINCT]` and chained by `NEXT` (`NEXT` binds looser:
//!   `q1 UNION q2 NEXT q3` reads the union). `ORDER BY`, `OFFSET` and `LIMIT` belong to their own
//!   operand. A bare `UNION` is `UNION DISTINCT`; whether the operands' columns agree and whether
//!   `ALL` and `DISTINCT` are mixed is the binder's check. `UNION` or `NEXT` where no `RETURN`
//!   ends the query before it is a syntax error naming the form; `NEXT YIELD`, `EXCEPT` and
//!   `INTERSECT` are "not yet supported". The expected tokens after a clause and after `RETURN`
//!   still list the stage 2a clauses only.
//! - **`CALL { }`** ([`CallClause`]): `[OPTIONAL] CALL (a, b) { .. }`, `CALL () { .. }`, `CALL (*)
//!   { .. }` and `CALL { .. }` without a scope clause (imports nothing, Neo4j Cypher 25), a
//!   pipeline stage anywhere, also first. The body is a whole composite query ending in
//!   `RETURN`; a body without `RETURN` (a unit subquery, which only writes) is refused, an empty
//!   one a syntax error. Cypher's importing `WITH` (a `CALL` without a scope clause whose
//!   operand of the body's first block starts with `WITH` of plain variables, or `*` alone, and
//!   nothing else) moves into [`LinearQuery::importing`]; any other first `WITH` stays a clause
//!   (the binder finds its unknown names). `IN [n CONCURRENT] TRANSACTIONS` after the body is
//!   refused (a batched write); `CALL name(..)` stays a refused vendor procedure. Each body is
//!   one [`MAX_DEPTH`] level.
//! - **`COUNT { }`, `COLLECT { }` and whole-query bodies** (stage 2d, user decision D23; openCypher
//!   compatibility forms): `COUNT` / `COLLECT` before `{` (contextual words, any case) start a
//!   [`SubqueryExpr`] of kind [`SubqueryKind::Count`] / [`SubqueryKind::Collect`] with the
//!   bodies of `EXISTS { }`. A braced body holding a clause other than `MATCH` at its own
//!   bracket level (`OPTIONAL`, `WITH` that is not `STARTS WITH` / `ENDS WITH`, `LET`, `FILTER`,
//!   `FOR`, `UNWIND`, `RETURN`, `CALL`; a bounded token scan, [`Parser::whole_query_body`]) is a
//!   whole composite query ([`SubqueryExpr::query`]) and needs `RETURN` ("not yet supported"
//!   without one, naming its first such clause); a body of `MATCH` clauses or a pattern list,
//!   and every parenthesized body, keep the stage 2b tree and errors. The binder checks what each
//!   kind accepts.
//! - **Keywords are contextual** (lexer docs): where only a name can stand (label, property key,
//!   map key, alias, function name) a keyword is a name; as a variable it must be back-quoted.
//! - **Bounded nesting (kind `Limit`):** the recursion depth of the parser is limited to
//!   [`MAX_DEPTH`] and the height of every expression and label tree to [`MAX_HEIGHT`], so
//!   neither parsing nor any later recursive stage (binder, evaluator, `Drop`) can overflow the
//!   stack, whatever the input.

use super::ast::BinaryOp;
use super::ast::{
    CallArgs, CallClause, CallScope, CaseExpr, CaseWhen, CastTarget, CompositeQuery,
    ComprehensionForm, CountClause, EdgeDirection, EdgePattern, Endpoint, Expr, ExprKind,
    FilterClause, ForClause, GroupBy, Identifier, ImportingWith, LabelExpr, LabelExprKind,
    LetBinding, LetClause, LinearQuery, ListComprehension, ListPredicate, Literal, MapEntry,
    MapLiteral, MapProjection, MatchClause, NextBlock, NodePattern, OrderBy, Ordinal, OrdinalKind,
    PathPattern, PatternComprehension, PatternStep, ProjectionItem, Quantifier, Query, QueryBlock,
    ReduceAccumulator, ReturnClause, ReturnItem, SortDirection, SortItem, Stage, SubqueryExpr,
    SubqueryForm, SubqueryKind, TrimSide, UnaryOp, UnionKind, UnionOperand, WithClause,
};
use super::lexer::{Keyword, LexError, LexErrorKind, Span, Token, TokenKind, tokenize};
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
/// The clauses that may follow a clause of the pipeline, in the order the expected tokens
/// name them; a query may start with any of them too (stage 2a, design E4).
const NEXT_CLAUSES: [&str; 8] = [
    "MATCH",
    "OPTIONAL MATCH",
    "WITH",
    "LET",
    "FILTER",
    "FOR",
    "UNWIND",
    "RETURN",
];

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
    // ----- stage 1c: conversions (`functions/convert.rs`) -----
    /// `CAST(x AS type)`: the strict GQL conversion; the target is in [`CallArgs::Cast`].
    Cast,
    /// Cypher `toInteger(x)` (lenient: an unconvertible string is `NULL`).
    ToInteger,
    /// Cypher `toIntegerOrNull(x)` (every failure is `NULL`).
    ToIntegerOrNull,
    /// Cypher `toFloat(x)`.
    ToFloat,
    /// Cypher `toFloatOrNull(x)`.
    ToFloatOrNull,
    /// Cypher `toBoolean(x)`.
    ToBoolean,
    /// Cypher `toBooleanOrNull(x)`.
    ToBooleanOrNull,
    /// Cypher `toString(x)`.
    ToString,
    /// Cypher `toStringOrNull(x)`.
    ToStringOrNull,
    // ----- stage 1c: strings (`functions/strings.rs`) -----
    /// `left(s, n)`: the first `n` characters.
    Left,
    /// `right(s, n)`: the last `n` characters.
    Right,
    /// Cypher `substring(s, start [, length])` (0-based).
    Substring,
    /// `trim(s)`, the SQL forms of `TRIM` ([`CallArgs::Trim`]), and GQL's list trim
    /// `trim(list, n)` (dispatched by the first argument's kind, design F13).
    Trim,
    /// `btrim(s [, chars])`.
    Btrim,
    /// `ltrim(s [, chars])`.
    Ltrim,
    /// `rtrim(s [, chars])`.
    Rtrim,
    /// Cypher `replace(s, search, replacement [, limit])`.
    Replace,
    /// Cypher `split(s, delimiter)`.
    Split,
    /// Cypher `reverse(x)`: a string or a list.
    Reverse,
    /// `byte_length(s)` / `octet_length(s)`: UTF-8 bytes.
    ByteLength,
    /// Cypher `isEmpty(x)`: a string, list or map.
    IsEmpty,
    /// `nullif(a, b)`.
    NullIf,
    // ----- stage 1c: numbers (`functions/numbers.rs`) -----
    /// `abs(x)`.
    Abs,
    /// `floor(x)`.
    Floor,
    /// `ceil(x)` / `ceiling(x)`.
    Ceil,
    /// Cypher `round(x [, precision [, mode]])`.
    Round,
    /// Cypher `sign(x)`.
    Sign,
    /// `sqrt(x)`.
    Sqrt,
    /// `exp(x)`.
    Exp,
    /// `ln(x)`: the natural logarithm.
    Ln,
    /// `log(x)` (Cypher: natural) / `log(base, x)` (GQL: general), by arity (design F12).
    Log,
    /// `log10(x)`.
    Log10,
    /// `power(x, y)`.
    Power,
    /// `mod(a, b)`.
    Mod,
    // ----- stage 1c: lists, elements, paths (`functions/lists.rs`) -----
    /// Cypher `head(list)`.
    Head,
    /// Cypher `last(list)`.
    Last,
    /// Cypher `tail(list)`.
    Tail,
    /// Cypher `range(start, end [, step])`.
    Range,
    /// Cypher `keys(x)`: a vertex's, edge's or map's keys.
    Keys,
    /// Cypher `properties(x)`: a vertex's or edge's properties as a map.
    Properties,
    /// Cypher `startNode(e)`: the stored source vertex.
    StartNode,
    /// Cypher `endNode(e)`: the stored target vertex.
    EndNode,
    /// Cypher `nodes(p)`: the vertices of a path (design D21).
    Nodes,
    /// Cypher `relationships(p)`: the edges of a path (design D21).
    Relationships,
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
    }
}

const fn aggregate(function: Function, names: &'static [&'static str]) -> FunctionSpec {
    FunctionSpec {
        function,
        names,
        min_args: 1,
        max_args: 1,
        kind: FunctionKind::Aggregate,
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
    aggregate(Function::Count, &["count"]),
    aggregate(Function::Sum, &["sum"]),
    aggregate(Function::Avg, &["avg"]),
    aggregate(Function::Min, &["min"]),
    aggregate(Function::Max, &["max"]),
    aggregate(Function::CollectList, &["collect_list", "collect"]),
    // Stage 1c: appended, so the suggestions above stay the first of their distance.
    scalar(Function::Cast, &["cast"], 1, 1),
    scalar(Function::ToInteger, &["toInteger"], 1, 1),
    scalar(Function::ToIntegerOrNull, &["toIntegerOrNull"], 1, 1),
    scalar(Function::ToFloat, &["toFloat"], 1, 1),
    scalar(Function::ToFloatOrNull, &["toFloatOrNull"], 1, 1),
    scalar(Function::ToBoolean, &["toBoolean"], 1, 1),
    scalar(Function::ToBooleanOrNull, &["toBooleanOrNull"], 1, 1),
    scalar(Function::ToString, &["toString"], 1, 1),
    scalar(Function::ToStringOrNull, &["toStringOrNull"], 1, 1),
    scalar(Function::Left, &["left"], 2, 2),
    scalar(Function::Right, &["right"], 2, 2),
    scalar(Function::Substring, &["substring"], 2, 3),
    scalar(Function::Trim, &["trim"], 1, 2),
    scalar(Function::Btrim, &["btrim"], 1, 2),
    scalar(Function::Ltrim, &["ltrim"], 1, 2),
    scalar(Function::Rtrim, &["rtrim"], 1, 2),
    scalar(Function::Replace, &["replace"], 3, 4),
    scalar(Function::Split, &["split"], 2, 2),
    scalar(Function::Reverse, &["reverse"], 1, 1),
    scalar(Function::ByteLength, &["byte_length", "octet_length"], 1, 1),
    scalar(Function::IsEmpty, &["isEmpty"], 1, 1),
    scalar(Function::NullIf, &["nullif"], 2, 2),
    scalar(Function::Abs, &["abs"], 1, 1),
    scalar(Function::Floor, &["floor"], 1, 1),
    scalar(Function::Ceil, &["ceil", "ceiling"], 1, 1),
    scalar(Function::Round, &["round"], 1, 3),
    scalar(Function::Sign, &["sign"], 1, 1),
    scalar(Function::Sqrt, &["sqrt"], 1, 1),
    scalar(Function::Exp, &["exp"], 1, 1),
    scalar(Function::Ln, &["ln"], 1, 1),
    scalar(Function::Log, &["log"], 1, 2),
    scalar(Function::Log10, &["log10"], 1, 1),
    scalar(Function::Power, &["power"], 2, 2),
    scalar(Function::Mod, &["mod"], 2, 2),
    scalar(Function::Head, &["head"], 1, 1),
    scalar(Function::Last, &["last"], 1, 1),
    scalar(Function::Tail, &["tail"], 1, 1),
    scalar(Function::Range, &["range"], 2, 3),
    scalar(Function::Keys, &["keys"], 1, 1),
    scalar(Function::Properties, &["properties"], 1, 1),
    scalar(Function::StartNode, &["startNode"], 1, 1),
    scalar(Function::EndNode, &["endNode"], 1, 1),
    scalar(Function::Nodes, &["nodes"], 1, 1),
    scalar(Function::Relationships, &["relationships"], 1, 1),
];

/// A function name `gql()` knows but does not support (yet): the reason and the alternative
/// instead of "unknown function" (an LLM reads the `Help:` line and retries).
pub(crate) struct UnsupportedFunction {
    /// The names, matched ASCII case-insensitively.
    pub(crate) names: &'static [&'static str],
    /// What it is, for the message (`... is not yet supported` / `... is not supported`).
    pub(crate) what: &'static str,
    /// The alternative.
    pub(crate) alternative: &'static str,
    /// Refused for good (non-deterministic) rather than not yet supported.
    pub(crate) refused: bool,
}

const fn not_yet_function(
    names: &'static [&'static str],
    what: &'static str,
    alternative: &'static str,
) -> UnsupportedFunction {
    UnsupportedFunction {
        names,
        what,
        alternative,
        refused: false,
    }
}

/// The functions outside the GQL support with a targeted error (design "In or out").
pub(crate) const NOT_YET_FUNCTIONS: &[UnsupportedFunction] = &[
    UnsupportedFunction {
        names: &["rand", "randomUUID"],
        what: "a non-deterministic function",
        alternative: "gql() functions are deterministic; sample rows with Gremlin after gql(), \
                      for example g.gql(\"MATCH (n) RETURN n\").sample(1), or coin(0.5)",
        refused: true,
    },
    not_yet_function(
        &[
            "sin", "cos", "tan", "cot", "asin", "acos", "atan", "atan2", "degrees", "radians",
            "pi", "e", "haversin",
        ],
        "a trigonometric function",
        "Gremlin math() after gql(), for example \
         g.gql(\"MATCH (n:person) RETURN n.age AS a\").select(\"a\").math(\"sin(_)\")",
    ),
    not_yet_function(
        &[
            "date",
            "datetime",
            "localdatetime",
            "localtime",
            "time",
            "timestamp",
            "duration",
            "date_trunc",
            "datetime_trunc",
            "current_date",
            "current_time",
            "current_timestamp",
        ],
        "a date or time function",
        "dates and times are planned for version 0.2.0; ISO-8601 strings compare correctly as \
         text, for example WHERE n.since >= '2020-01-01'",
    ),
    not_yet_function(
        &[
            "toIntegerList",
            "toFloatList",
            "toStringList",
            "toBooleanList",
        ],
        "a list conversion function",
        "convert the items one by one in Gremlin after gql(), for example \
         g.gql(\"MATCH (n) RETURN n.tags AS t\").select(\"t\").unfold().as_number(), or with \
         FOR in gql(), as in g.gql(\"FOR t IN ['1', '2'] RETURN collect_list(toInteger(t)) AS l\")",
    ),
    not_yet_function(
        &["normalize"],
        "Unicode normalization",
        "store normalized text, or compare with lower() / upper()",
    ),
    not_yet_function(
        &["elements"],
        "a GQL path function pending verification",
        "nodes(p) and relationships(p) return the vertices and the edges of a path",
    ),
    not_yet_function(
        &["percentileCont", "percentileDisc", "stDev", "stDevP"],
        "a statistical aggregate function",
        "collect the values with collect_list(x) and compute it after gql()",
    ),
];

/// Cypher's list functions that bind a variable inside the call (stage 2d, D23): the predicate,
/// or `None` for `reduce`; `None` outside them. Matched ASCII case-insensitively. They are no
/// [`FUNCTIONS`] entries: their arguments are no expression list.
pub(crate) fn list_function(name: &str) -> Option<Option<ListPredicate>> {
    [
        ("all", Some(ListPredicate::All)),
        ("any", Some(ListPredicate::Any)),
        ("none", Some(ListPredicate::None)),
        ("single", Some(ListPredicate::Single)),
        ("reduce", None),
    ]
    .into_iter()
    .find(|(spelling, _)| spelling.eq_ignore_ascii_case(name))
    .map(|(_, form)| form)
}

/// The not yet supported function spelled `name`, if any.
pub(crate) fn lookup_unsupported_function(name: &str) -> Option<&'static UnsupportedFunction> {
    NOT_YET_FUNCTIONS
        .iter()
        .find(|f| f.names.iter().any(|n| n.eq_ignore_ascii_case(name)))
}

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
    if e.kind == LexErrorKind::UnexpectedChar('^') {
        // Cypher's exponentiation operator.
        return GqlErrorBuilder::new(
            GqlErrorKind::NotYet,
            e.span.start..e.span.end,
            "the exponentiation operator `^` is not yet supported in this version",
        )
        .alternative("use power(x, y), as in power(2, 10)");
    }
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
    /// The greatest height of an expression or label tree parsed by [`Parser::expr`] or a
    /// pattern filler since the innermost enclosing subquery began: the height of a
    /// [`SubqueryExpr`] is one more, so the `MAX_HEIGHT` bound covers its body too.
    inner_height: usize,
    /// How many brackets (`(`, `[`, `{`) the consumed tokens leave open.
    open: usize,
    /// While the list or the `WHERE` of a comprehension is parsed: the [`Parser::open`] count
    /// inside its `[`. A `|` at that level ends the part instead of continuing a label
    /// expression.
    bar_ends: Option<usize>,
}

/// How an edge ahead of a node pattern in an expression reads (the pattern-predicate lookahead).
#[derive(Clone, Copy, PartialEq, Eq)]
enum EdgeAhead {
    /// It contains `->` (or is `<->`): no expression operator reads it, so a pattern follows.
    Decisive,
    /// `--`, `<--` (the two characters adjacent) or a bracketed edge `-[..]-` / `<-[..]-`: a
    /// pattern when a node pattern follows it.
    Strong,
    /// `-` or `<-` alone, or `- -` with a space: arithmetic or a comparison unless a strong or
    /// decisive edge follows later in the chain.
    Weak,
}

/// Where a (composite) query is parsed: what ends it and what a body without `RETURN` means.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Body {
    /// The whole query (ends at the end of input or `;`).
    Top,
    /// The body of a `CALL { }` (ends at `}`; it needs a `RETURN`).
    Call,
    /// A whole-query body of `EXISTS` / `COUNT` / `COLLECT` (ends at `}`), with the subquery's
    /// kind for messages.
    Subquery(SubqueryKind),
}

/// The body of a [`SubqueryExpr`]: `MATCH` clauses, or a whole query.
type SubqueryBody = (Vec<MatchClause>, Option<Box<CompositeQuery>>);

/// How many edges the pattern-predicate lookahead follows through a chain of weak edges before
/// it decides on an expression (a pattern this long is refused by the binder anyway).
const PREDICATE_LOOKAHEAD_EDGES: usize = 64;

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
            inner_height: 0,
            open: 0,
            bar_ends: None,
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
        match token.kind {
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                self.open = self.open.saturating_add(1);
            }
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                self.open = self.open.saturating_sub(1);
            }
            _ => {}
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
        // `GROUP BY`: the grouping keys are the items without an aggregate.
        if token.kind == TokenKind::Ident
            && text == "GROUP"
            && self.nth(1).text.eq_ignore_ascii_case("BY")
        {
            let b = err(
                GqlErrorKind::Syntax,
                span.to(self.nth(1).span),
                "GROUP BY is not needed: grouping is implicit, the RETURN or WITH items without \
                 an aggregate are the grouping keys",
            )
            .alternative(
                "remove GROUP BY and list the keys as items next to the aggregates, as in \
                 MATCH (p:person)-[:created]->(s) RETURN p.name, count(s)",
            );
            return Some(Box::new(b));
        }
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
            // An inline `CALL { }` / `CALL (..) { }` where no clause stands: a plain syntax error.
            Keyword::Call if matches!(self.nth(1).kind, TokenKind::LBrace | TokenKind::LParen) => {
                None
            }
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
            Keyword::Next | Keyword::Union => {
                let b = err(
                    GqlErrorKind::Syntax,
                    span,
                    format!("{text} must follow a complete query that ends with RETURN"),
                )
                .alternative(if text == "NEXT" {
                    "end the statement before NEXT with RETURN, for example \
                     MATCH (p:person) RETURN p NEXT MATCH (p)-[:created]->(s) RETURN s.name"
                } else {
                    "end each query of the UNION with RETURN, for example \
                     MATCH (p:person) RETURN p.name AS name \
                     UNION MATCH (s:software) RETURN s.name AS name"
                });
                Some(Box::new(b))
            }
            Keyword::Except | Keyword::Intersect => not_yet(
                &text,
                "combine the results in Gremlin, for example \
                 g.v().union(__.has_label(\"person\"), __.has_label(\"software\"))",
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

    // The functions from `query` down to `return_clause` are thin and pass boxes (module rule
    // "Bounded recursion"): their frames lie under every nested expression of the first linear
    // query, so the large values are built in functions called after the deep part returns.

    fn query(&mut self) -> PResult<Query> {
        let body = self.composite(Body::Top)?;
        self.finish_query(body)
    }

    /// The end of the whole query after its statements: an optional `;`, then the end of input.
    fn finish_query(&mut self, body: Box<CompositeQuery>) -> PResult<Query> {
        self.eat(&TokenKind::Semicolon);
        if !self.at(&TokenKind::Eof) {
            return Err(self.after_query(&body, Body::Top));
        }
        Ok(Query {
            source: self.src.to_string(),
            body,
        })
    }

    /// `block (NEXT block)*`: a whole query, at the top level or as a body.
    fn composite(&mut self, ctx: Body) -> PResult<Box<CompositeQuery>> {
        let first = self.block(ctx)?;
        self.composite_rest(first, ctx)
    }

    /// The `NEXT` blocks of a [`CompositeQuery`] after its first block.
    fn composite_rest(
        &mut self,
        first: Box<QueryBlock>,
        ctx: Body,
    ) -> PResult<Box<CompositeQuery>> {
        let start = first.span;
        let mut next = Vec::new();
        while let Some(keyword) = self.eat_kw(Keyword::Next) {
            if self.at_kw(Keyword::Yield) {
                return Err(self.not_yet(
                    keyword.to(self.peek().span),
                    "NEXT YIELD",
                    "leave YIELD out: the statement after NEXT reads every column the RETURN \
                     before it returns, by name",
                ));
            }
            let block = *self.block(ctx)?;
            next.push(NextBlock { keyword, block });
        }
        Ok(Box::new(CompositeQuery {
            first: *first,
            next,
            span: start.to(self.last),
        }))
    }

    /// `linear (UNION [ALL | DISTINCT] linear)*`.
    fn block(&mut self, ctx: Body) -> PResult<Box<QueryBlock>> {
        let first = self.linear(ctx)?;
        self.block_rest(first, ctx)
    }

    /// The rest of a [`QueryBlock`] after its first operand: `(UNION [ALL | DISTINCT] linear)*`.
    fn block_rest(&mut self, first: Box<LinearQuery>, ctx: Body) -> PResult<Box<QueryBlock>> {
        let start = first.span;
        // The names of one operand are not in scope in the next (missing-RETURN advice).
        let base = self.variables.len();
        let mut unions = Vec::new();
        while let Some(union) = self.eat_kw(Keyword::Union) {
            let (kind, keyword) = if is_word(self.peek(), "ALL") {
                (UnionKind::All, union.to(self.advance().span))
            } else if let Some(distinct) = self.eat_kw(Keyword::Distinct) {
                (UnionKind::Distinct, union.to(distinct))
            } else {
                (UnionKind::Distinct, union)
            };
            self.variables.truncate(base);
            let query = *self.linear(ctx)?;
            unions.push(UnionOperand {
                kind,
                keyword,
                query,
            });
        }
        Ok(Box::new(QueryBlock {
            first: *first,
            unions,
            span: start.to(self.last),
        }))
    }

    /// A linear query: the clause pipeline, `RETURN` and its tail.
    fn linear(&mut self, ctx: Body) -> PResult<Box<LinearQuery>> {
        // Thin on purpose (module rule "Bounded recursion"): every clause and every error is
        // parsed in a function of its own, so this frame stays small under nested bodies.
        let start = self.peek().span;
        let mut matches = Vec::new();
        let mut stages = Vec::new();
        // The first clause that is not a plain MATCH, for a subquery body without RETURN.
        let mut other: Option<Span> = None;
        while self.pipeline_clause(&mut matches, &mut stages, &mut other)? {}
        if !self.at_kw(Keyword::Return) {
            return Err(self.missing_clause(ctx, &stages, &matches, other));
        }
        self.result_statement(start, matches, stages)
    }

    /// One clause of the pipeline before `RETURN`, when the current token starts one (`false`
    /// otherwise). `other` records the keywords of the first clause that is not a plain `MATCH`.
    fn pipeline_clause(
        &mut self,
        matches: &mut Vec<MatchClause>,
        stages: &mut Vec<Stage>,
        other: &mut Option<Span>,
    ) -> PResult<bool> {
        let keyword = self.peek().keyword();
        let call = keyword == Some(Keyword::Call)
            || (keyword == Some(Keyword::Optional) && self.nth(1).keyword() == Some(Keyword::Call));
        if other.is_none() && (call || stage_keyword(keyword)) {
            let mut span = self.peek().span;
            if keyword == Some(Keyword::Optional) {
                span = span.to(self.nth(1).span);
            }
            *other = Some(span);
        }
        let stage = match keyword {
            _ if call => Stage::Call(Box::new(self.call_clause()?)),
            Some(Keyword::Match | Keyword::Optional) => {
                let clause = self.match_clause()?;
                matches.push(clause);
                Stage::Match(matches.len() - 1)
            }
            Some(keyword @ (Keyword::With | Keyword::Let | Keyword::Filter)) => {
                self.pipeline_stage(keyword)?
            }
            Some(Keyword::For | Keyword::Unwind) => Stage::For(self.for_clause()?),
            _ => return Ok(false),
        };
        stages.push(stage);
        Ok(true)
    }

    /// The error at a token that continues no clause of the pipeline and is not `RETURN`.
    fn missing_clause(
        &self,
        ctx: Body,
        stages: &[Stage],
        matches: &[MatchClause],
        other: Option<Span>,
    ) -> Box<GqlErrorBuilder> {
        if self.at(&TokenKind::RBrace) {
            match ctx {
                Body::Call => return self.call_without_return(stages.is_empty()),
                Body::Subquery(_) if other.is_none() && !stages.is_empty() => {
                    return self.missing_return();
                }
                Body::Subquery(kind) => {
                    if let Some(span) = other {
                        let words: Vec<&str> = self.text(span).split_whitespace().collect();
                        return body_without_return(
                            span,
                            &words.join(" ").to_ascii_uppercase(),
                            kind,
                        );
                    }
                }
                Body::Top => {}
            }
        }
        if stages.is_empty() {
            return self.unexpected(&NEXT_CLAUSES);
        }
        if matches!(self.peek().kind, TokenKind::Eof | TokenKind::Semicolon) {
            return self.missing_return();
        }
        if let Some(Keyword::Order | Keyword::Offset | Keyword::Skip | Keyword::Limit) =
            self.peek().keyword()
        {
            if let Some(Stage::With(_)) = stages.last() {
                return self.misplaced_with_tail();
            }
            return self.not_yet(
                self.peek().span,
                &format!("{} before RETURN", self.peek().text.to_ascii_uppercase()),
                "write ORDER BY, OFFSET and LIMIT after RETURN, for example \
                 MATCH (p:person) RETURN p.name ORDER BY p.name LIMIT 2",
            );
        }
        self.after_stage(stages.last(), matches)
    }

    /// `RETURN` and its tail, closing the linear query that started at `start`.
    fn result_statement(
        &mut self,
        start: Span,
        matches: Vec<MatchClause>,
        stages: Vec<Stage>,
    ) -> PResult<Box<LinearQuery>> {
        let ret = self.return_clause()?;
        self.result_tail(start, matches, stages, ret)
    }

    /// `ORDER BY`, `OFFSET` and `LIMIT` after `ret`, closing the linear query.
    fn result_tail(
        &mut self,
        start: Span,
        matches: Vec<MatchClause>,
        stages: Vec<Stage>,
        ret: ReturnClause,
    ) -> PResult<Box<LinearQuery>> {
        let order_by = if self.at_kw(Keyword::Order) {
            Some(self.order_by()?)
        } else {
            None
        };
        let offset = self.offset_clause()?;
        let limit = self.limit_clause()?;
        Ok(Box::new(LinearQuery {
            matches,
            stages,
            ret,
            order_by,
            offset,
            limit,
            importing: None,
            span: start.to(self.last),
        }))
    }

    /// The error for a token after a complete (composite) query or body: the tokens the last
    /// result statement accepts, then the end of the query (or the closing `}` of a body).
    fn after_query(&self, query: &CompositeQuery, ctx: Body) -> Box<GqlErrorBuilder> {
        let block = query.next.last().map_or(&query.first, |next| &next.block);
        let last = block
            .unions
            .last()
            .map_or(&block.first, |operand| &operand.query);
        let end = if ctx == Body::Top {
            "end of input"
        } else {
            "`}`"
        };
        self.after_return(
            &last.ret,
            last.order_by.is_some(),
            last.offset.is_some(),
            last.limit.is_some(),
            end,
        )
    }

    /// The error for a `CALL` body that ends without `RETURN` (`empty`: holds no clause).
    fn call_without_return(&self, empty: bool) -> Box<GqlErrorBuilder> {
        let span = self.peek().span;
        if empty {
            let b = err(
                GqlErrorKind::Syntax,
                span,
                "the CALL { } body is empty: it holds a query ending in RETURN",
            )
            .expected(NEXT_CLAUSES)
            .alternative(
                "write the subquery between the braces, for example MATCH (p:person) \
                 CALL (p) { MATCH (p)-[:created]->(s) RETURN count(s) AS made } RETURN p.name, made",
            );
            return Box::new(b);
        }
        let b = err(
            GqlErrorKind::Refused,
            span,
            "a CALL { } body without RETURN (a unit subquery) only makes sense for writes; \
             gql() is read-only",
        )
        .expected(["RETURN"])
        .alternative(
            "end the body with RETURN and the columns the query reads after it, for example \
             MATCH (p:person) CALL (p) { MATCH (p)-[:created]->(s) RETURN count(s) AS made } \
             RETURN p.name, made",
        );
        Box::new(b)
    }

    /// `[OPTIONAL] CALL [(vars) | (*)] { query } [IN TRANSACTIONS ..]`, at `OPTIONAL` or `CALL`.
    fn call_clause(&mut self) -> PResult<CallClause> {
        let optional = self.eat_kw(Keyword::Optional);
        let start = optional.unwrap_or(self.peek().span);
        if !matches!(self.nth(1).kind, TokenKind::LParen | TokenKind::LBrace) {
            // A procedure call (`CALL db.labels()`): refused by name at `CALL`.
            return Err(self.unexpected(&["CALL"]));
        }
        self.advance();
        let scope = if self.at(&TokenKind::LParen) {
            self.call_scope()?
        } else {
            CallScope::Implicit
        };
        self.expect(&TokenKind::LBrace, &["`{`"])?;
        self.enter()?;
        let saved_variables = self.variables.len();
        let result = self.composite(Body::Call);
        self.variables.truncate(saved_variables);
        self.leave();
        let mut body = result?;
        if self.eat(&TokenKind::RBrace).is_none() {
            return Err(self.after_query(&body, Body::Call));
        }
        let span = start.to(self.last);
        if let Some(e) = self.in_transactions() {
            return Err(e);
        }
        if matches!(scope, CallScope::Implicit) {
            importing_with(&mut body.first.first);
            for operand in &mut body.first.unions {
                importing_with(&mut operand.query);
            }
        }
        Ok(CallClause {
            optional,
            scope,
            body,
            span,
        })
    }

    /// The variable scope clause `(a, b)`, `()` or `(*)` of a `CALL`, at its `(`.
    fn call_scope(&mut self) -> PResult<CallScope> {
        let open = self.advance().span;
        if self.eat(&TokenKind::Star).is_some() {
            self.expect(&TokenKind::RParen, &["`)`"])?;
            return Ok(CallScope::All(open.to(self.last)));
        }
        let mut variables = Vec::new();
        if self.eat(&TokenKind::RParen).is_none() {
            loop {
                match self.peek().kind {
                    TokenKind::Ident | TokenKind::QuotedIdent(_) => {
                        let name = self.peek().name().unwrap_or("").to_string();
                        let span = self.advance().span;
                        variables.push(Identifier { name, span });
                    }
                    _ if variables.is_empty() => {
                        return Err(self.unexpected(&["a variable", "`*`", "`)`"]));
                    }
                    _ => return Err(self.unexpected(&["a variable"])),
                }
                if self.eat(&TokenKind::Comma).is_none() {
                    self.expect(&TokenKind::RParen, &["`,`", "`)`"])?;
                    break;
                }
            }
        }
        Ok(CallScope::Variables {
            variables,
            span: open.to(self.last),
        })
    }

    /// The refusal of Cypher's `IN [n CONCURRENT] TRANSACTIONS` after a `CALL` body (a batched
    /// write), when the tokens start it.
    fn in_transactions(&self) -> Option<Box<GqlErrorBuilder>> {
        if !self.at_kw(Keyword::In) {
            return None;
        }
        let end = (1..=3)
            .map(|i| self.nth(i))
            .take_while(|t| !matches!(t.kind, TokenKind::Eof))
            .find(|t| is_word(t, "TRANSACTIONS"))?;
        let b = err(
            GqlErrorKind::Refused,
            self.peek().span.to(end.span),
            "CALL { } IN TRANSACTIONS commits writes in batches; gql() is read-only",
        )
        .alternative("remove IN TRANSACTIONS: a read-only subquery needs no batches");
        Some(Box::new(b))
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
    /// clause accepts, then the clauses that may follow it, with a "did you mean" over them.
    fn after_stage(&self, last: Option<&Stage>, matches: &[MatchClause]) -> Box<GqlErrorBuilder> {
        let mut expected: Vec<&str> = match last {
            Some(Stage::Match(i)) => match matches.get(*i) {
                Some(MatchClause {
                    where_clause: None, ..
                }) => vec!["`,`", "WHERE"],
                _ => Vec::new(),
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
                    expected.push("ORDER BY");
                }
                if with.offset.is_none() && with.limit.is_none() && with.where_clause.is_none() {
                    expected.extend(["OFFSET", "SKIP"]);
                }
                if with.limit.is_none() && with.where_clause.is_none() {
                    expected.push("LIMIT");
                }
                if with.where_clause.is_none() {
                    expected.push("WHERE");
                }
                expected
            }
            Some(Stage::Let(_)) => vec!["`,`"],
            Some(Stage::For(clause)) if !clause.unwind && clause.ordinal.is_none() => {
                vec!["WITH ORDINALITY", "WITH OFFSET"]
            }
            Some(Stage::Filter(_) | Stage::For(_) | Stage::Call(_)) | None => Vec::new(),
        };
        expected.extend(NEXT_CLAUSES);
        self.unexpected(&expected)
    }

    /// `WITH`, `LET` or `FILTER`, first or after another clause.
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

    /// `FOR x IN expr [WITH ORDINALITY i | WITH OFFSET i]` or `UNWIND expr AS x`, at `FOR` /
    /// `UNWIND`.
    fn for_clause(&mut self) -> PResult<ForClause> {
        let keyword = self.advance();
        let start = keyword.span;
        if keyword.keyword() == Some(Keyword::Unwind) {
            let list = self.expr()?;
            if !self.at_kw(Keyword::As) {
                return Err(self.missing_for_word(
                    "AS",
                    "UNWIND needs AS and a variable after the list",
                    "write UNWIND list AS x, for example UNWIND [1, 2, 3] AS x RETURN x (GQL: \
                     FOR x IN [1, 2, 3] RETURN x)",
                ));
            }
            self.advance();
            let variable = self.for_variable()?;
            if let Some(ordinal) = self.ordinal()? {
                let b = err(
                    GqlErrorKind::Syntax,
                    ordinal.span,
                    "WITH ORDINALITY / WITH OFFSET belong to GQL FOR, not to UNWIND",
                )
                .alternative(
                    "write FOR x IN list WITH ORDINALITY i, for example \
                     FOR x IN ['a', 'b'] WITH ORDINALITY i RETURN x, i",
                );
                return Err(Box::new(b));
            }
            return Ok(ForClause {
                variable,
                list,
                ordinal: None,
                unwind: true,
                span: start.to(self.last),
            });
        }
        let variable = self.for_variable()?;
        if !self.at_kw(Keyword::In) {
            return Err(self.missing_for_word(
                "IN",
                "FOR needs IN between the variable and the list",
                "write FOR x IN list, for example FOR x IN [1, 2, 3] RETURN x",
            ));
        }
        self.advance();
        let list = self.expr()?;
        let ordinal = self.ordinal()?;
        Ok(ForClause {
            variable,
            list,
            ordinal,
            unwind: false,
            span: start.to(self.last),
        })
    }

    /// The new variable of a `FOR` clause (or of its ordinal).
    fn for_variable(&mut self) -> PResult<Identifier> {
        match self.peek().kind {
            TokenKind::Ident | TokenKind::QuotedIdent(_) => {
                let name = self.peek().name().unwrap_or("").to_string();
                let span = self.advance().span;
                if !self.variables.contains(&name) {
                    self.variables.push(name.clone());
                }
                Ok(Identifier { name, span })
            }
            _ => Err(self.unexpected(&["a variable"])),
        }
    }

    /// The syntax error for a missing `IN` (`FOR`) or `AS` (`UNWIND`) at the current token.
    fn missing_for_word(
        &self,
        word: &str,
        message: &str,
        alternative: &str,
    ) -> Box<GqlErrorBuilder> {
        if let Some(e) = self.construct_error() {
            return e;
        }
        let token = self.peek();
        let b = err(
            GqlErrorKind::Syntax,
            token.span,
            format!("{message}, found {}", describe(token)),
        )
        .expected([word])
        .alternative(alternative);
        Box::new(b)
    }

    /// `WITH ORDINALITY i` / `WITH OFFSET i` after a `FOR` list, when the tokens start it
    /// (module docs: a `WITH` clause otherwise).
    fn ordinal(&mut self) -> PResult<Option<Ordinal>> {
        if !self.at_kw(Keyword::With) {
            return Ok(None);
        }
        let word = self.nth(1);
        let kind = if word.keyword() == Some(Keyword::Offset) {
            OrdinalKind::Offset
        } else if is_word(word, "ORDINALITY") && self.ordinality_form() {
            OrdinalKind::Ordinality
        } else {
            return Ok(None);
        };
        let with = self.advance().span;
        let word = self.advance();
        let spelled = if kind == OrdinalKind::Offset {
            "OFFSET"
        } else {
            "ORDINALITY"
        };
        if !matches!(
            self.peek().kind,
            TokenKind::Ident | TokenKind::QuotedIdent(_)
        ) {
            let token = self.peek();
            let mut alternative = format!(
                "name the position, for example FOR x IN ['a', 'b'] WITH {spelled} i RETURN x, i"
            );
            // A keyword in lower or mixed case is likely meant as the name.
            if token.keyword().is_some() && token.text != token.text.to_ascii_uppercase() {
                alternative.push_str(&format!(
                    "; {} is a keyword: as a name, write it back-quoted: `{}`",
                    token.text, token.text
                ));
            }
            let b = err(
                GqlErrorKind::Syntax,
                with.to(word.span),
                format!("WITH {spelled} needs a variable name for the position"),
            )
            .expected(["a variable"])
            .alternative(alternative);
            return Err(Box::new(b));
        }
        let variable = self.for_variable()?;
        Ok(Some(Ordinal {
            kind,
            span: with.to(variable.span),
            variable,
        }))
    }

    /// Whether `WITH ORDINALITY` at the current token is the ordinal of a `FOR` clause rather
    /// than a `WITH` clause projecting a variable `ordinality` (module docs).
    fn ordinality_form(&self) -> bool {
        let after = self.nth(2);
        match &after.kind {
            TokenKind::Ident | TokenKind::QuotedIdent(_) => true,
            TokenKind::Eof | TokenKind::Semicolon => !self.declared(self.nth(1).text),
            TokenKind::Keyword(keyword) => {
                matches!(
                    keyword,
                    Keyword::Match
                        | Keyword::Optional
                        | Keyword::With
                        | Keyword::Let
                        | Keyword::Filter
                        | Keyword::For
                        | Keyword::Unwind
                        | Keyword::Return
                        | Keyword::Call
                ) && !self.declared(self.nth(1).text)
            }
            _ => false,
        }
    }

    /// Whether a variable named `name` was declared so far.
    fn declared(&self, name: &str) -> bool {
        self.variables.iter().any(|v| v == name)
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
        end: &'static str,
    ) -> Box<GqlErrorBuilder> {
        let token = self.peek();
        let span = token.span;
        if self.at_group_by() {
            let b = err(
                GqlErrorKind::Syntax,
                span.to(self.nth(1).span),
                "GROUP BY must come right after the RETURN items, before ORDER BY, OFFSET and \
                 LIMIT",
            )
            .alternative(
                "write GROUP BY directly after the items, for example \
                 MATCH (p:person)-[:created]->(s) RETURN p.name AS n, count(s) AS c GROUP BY n \
                 ORDER BY c",
            );
            return Box::new(b);
        }
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
                | Keyword::Filter
                | Keyword::For
                | Keyword::Unwind,
            ) => {
                return self.clause_after_return();
            }
            Some(Keyword::Call)
                if matches!(self.nth(1).kind, TokenKind::LParen | TokenKind::LBrace) =>
            {
                return self.clause_after_return();
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
        expected.push(end);
        self.unexpected(&expected)
    }

    /// The error for a clause after a complete result statement.
    fn clause_after_return(&self) -> Box<GqlErrorBuilder> {
        let token = self.peek();
        let b = err(
            GqlErrorKind::Syntax,
            token.span,
            format!(
                "{} after RETURN: RETURN ends the query",
                token.text.to_ascii_uppercase()
            ),
        )
        .alternative("put every MATCH before the one RETURN, or chain Gremlin steps after gql()");
        Box::new(b)
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
            if edge.quantifier.is_none() && !self.at(&TokenKind::LParen) {
                return Err(self.unexpected(&["`{`", "`*`", "`+`", "`(`"]));
            }
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
            let (labels, height) = self.label_expr()?;
            self.inner_height = self.inner_height.max(height);
            Some(labels)
        } else {
            None
        };
        let quantifier = if close == &TokenKind::RBracket && self.at(&TokenKind::Star) {
            Some(self.cypher_quantifier()?)
        } else {
            None
        };
        let properties = if self.at(&TokenKind::LBrace) {
            let (map, height) = self.map_literal()?;
            self.inner_height = self.inner_height.max(height);
            Some(map)
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
            // Cypher `[e:T*1..3]`: the quantifier follows the labels, inside the brackets.
            if close == &TokenKind::RBracket
                && quantifier.is_none()
                && properties.is_none()
                && where_clause.is_none()
            {
                expected.push("`*`");
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
                "quantify a single edge, as in MATCH (a)-[:knows]->{1,3}(b) RETURN b; for a \
                 longer pattern, write the path out, or use Gremlin repeat(), for example \
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
        while self.bar_ends != Some(self.open) && self.eat(&TokenKind::Pipe).is_some() {
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
        let span = start.to(self.last);
        let group_by = if self.at_group_by() {
            Some(self.group_by()?)
        } else {
            None
        };
        Ok(ReturnClause {
            distinct,
            star,
            items,
            group_by,
            span,
        })
    }

    /// Whether the current tokens are `GROUP BY` (`GROUP` is a name, not a reserved word).
    fn at_group_by(&self) -> bool {
        self.peek().kind == TokenKind::Ident
            && self.peek().text.eq_ignore_ascii_case("GROUP")
            && self.nth(1).keyword() == Some(Keyword::By)
    }

    /// `GROUP BY ('(' ')' | expr (',' expr)*)` (ISO GQL `<group by clause>`; the binder checks
    /// the keys against the items).
    fn group_by(&mut self) -> PResult<GroupBy> {
        let start = self.advance().span;
        self.expect_kw(Keyword::By, &["BY"])?;
        let mut keys = Vec::new();
        if self.at(&TokenKind::LParen) && self.nth(1).kind == TokenKind::RParen {
            self.advance();
            self.advance();
        } else {
            loop {
                keys.push(self.expr()?);
                if self.eat(&TokenKind::Comma).is_none() {
                    break;
                }
            }
        }
        Ok(GroupBy {
            keys,
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
        let (expr, height) = self.expr_bp(0)?;
        self.inner_height = self.inner_height.max(height);
        Ok(expr)
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

    /// `IS [NOT] NULL`, `IS [NOT] LABELED labels`, `IS [NOT] SOURCE OF e`,
    /// `IS [NOT] DESTINATION OF e`.
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
        if let Some(end) = endpoint_word(self.peek())
            && is_word(self.nth(1), "OF")
        {
            return self.is_endpoint_rhs((operand, height), end, negated);
        }
        if let Some(Keyword::True | Keyword::False | Keyword::Unknown) = self.peek().keyword() {
            return Err(self.not_yet(
                self.peek().span,
                "a truth value test (IS TRUE / IS FALSE / IS UNKNOWN)",
                "compare with = TRUE / = FALSE, or test IS NULL",
            ));
        }
        if negated {
            Err(self.unexpected(&["NULL", "LABELED", "SOURCE", "DESTINATION"]))
        } else {
            Err(self.unexpected(&["NOT", "NULL", "LABELED", "SOURCE", "DESTINATION"]))
        }
    }

    /// `SOURCE OF e` / `DESTINATION OF e` after `IS [NOT]`.
    fn is_endpoint_rhs(
        &mut self,
        (operand, height): (Expr, usize),
        end: Endpoint,
        negated: bool,
    ) -> PResult<(Expr, usize)> {
        self.advance();
        self.advance();
        let (edge, edge_height) = self.expr_bp(BP_PREDICATE + 1)?;
        let span = operand.span.to(edge.span);
        let height = self.height(height.max(edge_height), span)?;
        Ok((
            Expr {
                kind: ExprKind::IsEndpoint {
                    operand: Box::new(operand),
                    edge: Box::new(edge),
                    end,
                    negated,
                },
                span,
            },
            height,
        ))
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

    /// Property access `.key`, Cypher `:Label` and a map projection `v {..}` after a primary.
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
                    (expr, height) = self.map_projection(expr, height)?;
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
            "write what the function does as Gremlin steps after gql(), or use one of the gql() \
             functions: conversions (CAST, toString, toInteger, ...), strings (substring, \
             replace, split, btrim, upper, lower, ...), numbers (round, abs, sqrt, ...), lists \
             and elements (head, last, range, keys, properties, nodes, ...)",
        );
        Box::new(b)
    }

    fn text(&self, span: Span) -> &str {
        self.src.get(span.start..span.end).unwrap_or("")
    }

    /// Dispatches on the first token of a primary expression (nested forms in own functions).
    fn primary(&mut self) -> PResult<(Expr, usize)> {
        match &self.peek().kind {
            TokenKind::LParen if self.pattern_ahead() => self.pattern_predicate(),
            TokenKind::LParen => self.parenthesized(),
            TokenKind::LBracket => self.list(),
            TokenKind::LBrace => self.map_expr(),
            TokenKind::Ident if self.record_ahead() => self.record_constructor(),
            TokenKind::Keyword(Keyword::Case) => self.case_expr(),
            // One arm for `EXISTS`, `COUNT {` and `COLLECT {`: every arm costs a stack slot of
            // this recursive function.
            _ if self.keyword_subquery_ahead() => self.keyword_subquery(),
            TokenKind::Ident | TokenKind::QuotedIdent(_) | TokenKind::Keyword(_)
                if self.nth(1).kind == TokenKind::LParen =>
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
            TokenKind::Ident | TokenKind::QuotedIdent(_) => ExprKind::Variable(Identifier {
                name: token.name().unwrap_or("").to_string(),
                span,
            }),
            _ => return Err(self.unexpected(&["an expression"])),
        };
        self.advance();
        Ok(Expr { kind, span })
    }

    // ----- subqueries: EXISTS and pattern predicates -----

    /// The token at index `i` (the end of input past the last one).
    fn token_at(&self, i: usize) -> &Token<'a> {
        self.tokens.get(i).unwrap_or(&self.eof)
    }

    /// The index after the bracket group that opens at token `i` (`(`, `[` or `{`, nested groups
    /// of any kind counted alike), or `None` when token `i` opens none or the group is not
    /// closed.
    fn skip_group(&self, i: usize) -> Option<usize> {
        let mut depth = 0usize;
        let mut j = i;
        loop {
            match self.token_at(j).kind {
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => depth += 1,
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        return j.checked_add(1);
                    }
                }
                TokenKind::Eof => return None,
                _ => {}
            }
            if depth == 0 {
                return None;
            }
            j = j.checked_add(1)?;
        }
    }

    /// The edge starting at token `i` (right after a node pattern) and the index after it, if
    /// the tokens there read as one.
    fn edge_ahead(&self, i: usize) -> Option<(usize, EdgeAhead)> {
        let first = self.token_at(i);
        let next = self.token_at(i.saturating_add(1));
        let adjacent = if first.span.touches(next.span) {
            EdgeAhead::Strong
        } else {
            EdgeAhead::Weak
        };
        let after = i.saturating_add(2);
        match (&first.kind, &next.kind) {
            (TokenKind::RightArrow, _) => Some((i.saturating_add(1), EdgeAhead::Decisive)),
            (TokenKind::Minus | TokenKind::LeftArrow, TokenKind::LBracket) => {
                let close = self.skip_group(i.saturating_add(1))?;
                match self.token_at(close).kind {
                    TokenKind::RightArrow => Some((close.saturating_add(1), EdgeAhead::Decisive)),
                    TokenKind::Minus => Some((close.saturating_add(1), EdgeAhead::Strong)),
                    _ => None,
                }
            }
            (TokenKind::Minus | TokenKind::LeftArrow, TokenKind::RightArrow) => {
                Some((after, EdgeAhead::Decisive))
            }
            (TokenKind::LeftArrow, TokenKind::Gt) if adjacent == EdgeAhead::Strong => {
                Some((after, EdgeAhead::Decisive))
            }
            (TokenKind::Minus | TokenKind::LeftArrow, TokenKind::Minus) => Some((after, adjacent)),
            (TokenKind::Minus | TokenKind::LeftArrow, TokenKind::LParen) => {
                Some((i.saturating_add(1), EdgeAhead::Weak))
            }
            _ => None,
        }
    }

    /// At `(` where an expression stands: whether a Cypher pattern predicate starts here (a
    /// node pattern, then an edge no expression operator could mean). A bounded token scan, no
    /// parse: `(a) - (b)` and `(a)<-(b)` stay arithmetic / a comparison, `(a)-->(b)`,
    /// `(a)--(b)`, `(a)-[:T]-(b)` and `(a)-(b)-->(c)` are patterns (module docs).
    fn pattern_ahead(&self) -> bool {
        self.pattern_ahead_at(self.pos)
    }

    /// [`Parser::pattern_ahead`] at token `i`.
    fn pattern_ahead_at(&self, mut i: usize) -> bool {
        for _ in 0..=PREDICATE_LOOKAHEAD_EDGES {
            if self.token_at(i).kind != TokenKind::LParen {
                return false;
            }
            let Some((after, edge)) = self.skip_group(i).and_then(|j| self.edge_ahead(j)) else {
                return false;
            };
            if edge == EdgeAhead::Decisive {
                return true;
            }
            let mut j = after;
            match self.token_at(j).kind {
                TokenKind::LBrace => match self.skip_group(j) {
                    Some(k) => j = k,
                    None => return false,
                },
                TokenKind::Star | TokenKind::Plus => j = j.saturating_add(1),
                _ => {}
            }
            if self.token_at(j).kind != TokenKind::LParen {
                return false;
            }
            if edge == EdgeAhead::Strong {
                return true;
            }
            i = j;
        }
        false
    }

    /// A Cypher pattern predicate `(a)-[:T]->(b)`: an alias of `EXISTS { pattern }`.
    fn pattern_predicate(&mut self) -> PResult<(Expr, usize)> {
        self.enter()?;
        let saved_height = std::mem::replace(&mut self.inner_height, 0);
        let saved_variables = self.variables.len();
        let result = self.path_pattern();
        let inner = std::mem::replace(&mut self.inner_height, saved_height);
        self.variables.truncate(saved_variables);
        self.leave();
        let pattern = result?;
        let span = pattern.span;
        let clause = MatchClause {
            optional: false,
            patterns: vec![pattern],
            where_clause: None,
            span,
        };
        let body = (vec![clause], None);
        self.subquery(
            SubqueryKind::Exists,
            SubqueryForm::PatternPredicate,
            body,
            span,
            inner,
        )
    }

    /// The subquery expression of a parsed body whose own trees are at most `inner` high.
    fn subquery(
        &self,
        kind: SubqueryKind,
        form: SubqueryForm,
        (matches, query): SubqueryBody,
        span: Span,
        inner: usize,
    ) -> PResult<(Expr, usize)> {
        let height = self.height(inner, span)?;
        let subquery = SubqueryExpr {
            kind,
            form,
            matches,
            query,
            span,
        };
        Ok((
            Expr {
                kind: ExprKind::Subquery(Box::new(subquery)),
                span,
            },
            height,
        ))
    }

    /// `EXISTS { .. }` or `EXISTS ( .. )`, at `EXISTS`.
    fn exists_expr(&mut self) -> PResult<(Expr, usize)> {
        let keyword = self.advance().span;
        let (form, close, close_text) = match self.peek().kind {
            TokenKind::LBrace => (SubqueryForm::Braces, TokenKind::RBrace, "`}`"),
            TokenKind::LParen => (SubqueryForm::Parens, TokenKind::RParen, "`)`"),
            _ => return Err(self.unexpected(&["`{`", "`(`"])),
        };
        if form == SubqueryForm::Parens && !self.subquery_body_ahead() {
            return Err(self.exists_on_value(keyword));
        }
        self.subquery_after(SubqueryKind::Exists, form, keyword, &close, close_text)
    }

    /// Whether `EXISTS`, `COUNT {` or `COLLECT {` starts here.
    fn keyword_subquery_ahead(&self) -> bool {
        let token = self.peek();
        token.keyword() == Some(Keyword::Exists)
            || (self.nth(1).kind == TokenKind::LBrace && counted_kind(token).is_some())
    }

    /// `EXISTS ..`, or `COUNT { .. }` / `COLLECT { .. }` (openCypher compatibility forms, stage
    /// 2d), at the word.
    fn keyword_subquery(&mut self) -> PResult<(Expr, usize)> {
        let Some(kind) = counted_kind(self.peek()) else {
            return self.exists_expr();
        };
        let keyword = self.advance().span;
        self.subquery_after(
            kind,
            SubqueryForm::Braces,
            keyword,
            &TokenKind::RBrace,
            "`}`",
        )
    }

    /// The subquery whose word (`EXISTS`, `COUNT`, `COLLECT`) is `keyword`, at its opening
    /// bracket.
    fn subquery_after(
        &mut self,
        kind: SubqueryKind,
        form: SubqueryForm,
        keyword: Span,
        close: &TokenKind,
        close_text: &'static str,
    ) -> PResult<(Expr, usize)> {
        self.advance();
        self.enter()?;
        let saved_height = std::mem::replace(&mut self.inner_height, 0);
        let saved_variables = self.variables.len();
        let result = self.subquery_body(kind, form, close, close_text);
        let inner = std::mem::replace(&mut self.inner_height, saved_height);
        self.variables.truncate(saved_variables);
        self.leave();
        let body = result?;
        self.subquery(kind, form, body, keyword.to(self.last), inner)
    }

    /// After `EXISTS (`: whether the next token can start a body (a pattern, `MATCH`, a clause
    /// refused inside a body, or the closing parenthesis) rather than a value (Cypher 4
    /// `exists(n.p)`).
    fn subquery_body_ahead(&self) -> bool {
        let token = self.nth(1);
        match &token.kind {
            TokenKind::LParen | TokenKind::RParen => true,
            TokenKind::Ident | TokenKind::QuotedIdent(_) => self.nth(2).kind == TokenKind::Eq,
            TokenKind::Keyword(k) => matches!(
                k,
                Keyword::Match
                    | Keyword::Optional
                    | Keyword::With
                    | Keyword::Let
                    | Keyword::Filter
                    | Keyword::For
                    | Keyword::Unwind
                    | Keyword::Return
                    | Keyword::Call
            ),
            _ => false,
        }
    }

    /// The refusal of Cypher 4 `exists(value)`, at its `(`.
    fn exists_on_value(&self, keyword: Span) -> Box<GqlErrorBuilder> {
        let open = self.peek().span;
        let (span, argument) = match self.skip_group(self.pos) {
            Some(after) => {
                let close = self.token_at(after.saturating_sub(1)).span;
                let inside = Span {
                    start: open.end,
                    end: close.start,
                    ..open
                };
                (keyword.to(close), self.text(inside).trim())
            }
            None => (keyword.to(open), ""),
        };
        let mut alternative = "test a property with n.p IS NOT NULL".to_string();
        if !argument.is_empty() && argument.len() <= 80 && !argument.contains('\n') {
            alternative.push_str(&format!(", here {argument} IS NOT NULL"));
        }
        alternative.push_str(
            "; EXISTS tests a graph pattern, as in \
             MATCH (p:person) WHERE EXISTS { (p)-[:created]->() } RETURN p.name",
        );
        let b = err(
            GqlErrorKind::Refused,
            span,
            "exists(value), the Cypher 4 property test, is not supported: EXISTS takes a graph \
             pattern or MATCH clauses",
        )
        .alternative(alternative);
        Box::new(b)
    }

    /// The body of `EXISTS { .. }` / `EXISTS ( .. )` (`COUNT { .. }`, `COLLECT { .. }`) after
    /// its opening bracket, through `close`: `MATCH` clauses, a comma-separated pattern list
    /// with an optional `WHERE`, or in braces a whole query ending in `RETURN` (stage 2d).
    fn subquery_body(
        &mut self,
        kind: SubqueryKind,
        form: SubqueryForm,
        close: &TokenKind,
        close_text: &'static str,
    ) -> PResult<SubqueryBody> {
        if form == SubqueryForm::Braces && self.whole_query_body() {
            return self.whole_query_subquery(kind);
        }
        let mut matches = Vec::new();
        let block = matches!(
            self.peek().keyword(),
            Some(Keyword::Match | Keyword::Optional)
        );
        if block {
            while let Some(Keyword::Match | Keyword::Optional) = self.peek().keyword() {
                if self.at_kw(Keyword::Optional) {
                    return Err(self.not_yet(
                        self.peek().span.to(self.nth(1).span),
                        &format!("OPTIONAL MATCH inside {}", kind_name(kind)),
                        "leave the OPTIONAL MATCH out: an optional part never changes whether \
                         EXISTS finds a match",
                    ));
                }
                matches.push(self.match_clause()?);
            }
        } else {
            if let Some(e) = self.subquery_clause_error(kind, form) {
                return Err(e);
            }
            self.check_match_prefix()?;
            let path_variable = matches!(
                self.peek().kind,
                TokenKind::Ident | TokenKind::QuotedIdent(_)
            ) && self.nth(1).kind == TokenKind::Eq;
            if !self.at(&TokenKind::LParen) && !path_variable {
                return Err(self.unexpected(&["MATCH", "`(`"]));
            }
            let start = self.peek().span;
            let mut patterns = vec![self.path_pattern()?];
            while self.eat(&TokenKind::Comma).is_some() {
                patterns.push(self.path_pattern()?);
            }
            let where_clause = if self.eat_kw(Keyword::Where).is_some() {
                Some(self.expr()?)
            } else {
                None
            };
            matches.push(MatchClause {
                optional: false,
                patterns,
                where_clause,
                span: start.to(self.last),
            });
        }
        if self.eat(close).is_some() {
            return Ok((matches, None));
        }
        if let Some(e) = self.subquery_clause_error(kind, form) {
            return Err(e);
        }
        let mut expected = Vec::new();
        if matches.last().is_some_and(|m| m.where_clause.is_none()) {
            expected.extend(["`,`", "WHERE"]);
        }
        if block {
            expected.push("MATCH");
        }
        expected.push(close_text);
        Err(self.unexpected(&expected))
    }

    /// After the `{` of a subquery: whether its body is a whole query (stage 2d) rather than
    /// `MATCH` clauses or a pattern list: a bounded token scan up to the closing `}` finds a
    /// clause keyword other than `MATCH` at the body's own bracket level (`WITH` after `STARTS`
    /// / `ENDS` is an operator). `ORDER BY`, `OFFSET`, `LIMIT`, `UNION` and `NEXT` without a
    /// `RETURN` keep the stage 2b errors.
    fn whole_query_body(&self) -> bool {
        // The brackets open inside the body; a closing one that does not match (an unclosed
        // group in a broken pattern) ends the scan: the stage 2b parse reports it.
        let mut open: Vec<&TokenKind> = Vec::new();
        let mut i = self.pos;
        loop {
            let token = self.token_at(i);
            match &token.kind {
                kind @ (TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace) => {
                    open.push(kind);
                }
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                    let matching = match open.pop() {
                        Some(TokenKind::LParen) => token.kind == TokenKind::RParen,
                        Some(TokenKind::LBracket) => token.kind == TokenKind::RBracket,
                        Some(_) => token.kind == TokenKind::RBrace,
                        None => false,
                    };
                    if !matching {
                        return false;
                    }
                }
                TokenKind::Eof => return false,
                TokenKind::Keyword(keyword) if open.is_empty() => match keyword {
                    Keyword::Optional
                    | Keyword::Let
                    | Keyword::Filter
                    | Keyword::For
                    | Keyword::Unwind
                    | Keyword::Return
                    | Keyword::Call => return true,
                    Keyword::With => {
                        let operator = i.checked_sub(1).is_some_and(|before| {
                            matches!(
                                self.token_at(before).keyword(),
                                Some(Keyword::Starts | Keyword::Ends)
                            )
                        });
                        if !operator {
                            return true;
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
            i = match i.checked_add(1) {
                Some(next) => next,
                None => return false,
            };
        }
    }

    /// A whole-query body of `EXISTS` / `COUNT` / `COLLECT { .. }` after its `{`, through `}`.
    fn whole_query_subquery(&mut self, kind: SubqueryKind) -> PResult<SubqueryBody> {
        let ctx = Body::Subquery(kind);
        let body = self.composite(ctx)?;
        if self.eat(&TokenKind::RBrace).is_none() {
            return Err(self.after_query(&body, ctx));
        }
        Ok((Vec::new(), Some(body)))
    }

    /// The "not yet" error for a clause other than `MATCH` inside a pattern body or a
    /// parenthesized body (braces take a whole query, stage 2d).
    fn subquery_clause_error(
        &self,
        kind: SubqueryKind,
        form: SubqueryForm,
    ) -> Option<Box<GqlErrorBuilder>> {
        let token = self.peek();
        let word = match token.keyword()? {
            Keyword::With
            | Keyword::Let
            | Keyword::Filter
            | Keyword::For
            | Keyword::Unwind
            | Keyword::Return
            | Keyword::Offset
            | Keyword::Skip
            | Keyword::Limit => token.text.to_ascii_uppercase(),
            Keyword::Order => "ORDER BY".to_string(),
            Keyword::Call | Keyword::Union | Keyword::Next => token.text.to_ascii_uppercase(),
            _ => return None,
        };
        let open = if form == SubqueryForm::Parens {
            format!("{} ( )", kind_name(kind))
        } else {
            format!("{} {{ }}", kind_name(kind))
        };
        Some(self.not_yet(
            token.span,
            &format!("{word} inside {open} (a nested query)"),
            "EXISTS takes MATCH clauses in this version; write the condition into the WHERE \
             of a MATCH clause, as in MATCH (p:person) WHERE EXISTS { MATCH \
             (p)-[:created]->(s) WHERE s.lang = 'java' } RETURN p.name",
        ))
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
        if self.list_comprehension_ahead() {
            return self.list_comprehension();
        }
        if self.pattern_comprehension_ahead() {
            return self.pattern_comprehension();
        }
        let open = self.advance().span;
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

    // ----- stage 2c: map projections and comprehensions -----

    /// At `{` after the variable `target`: the map projection `target {items}`.
    fn map_projection(&mut self, target: Expr, height: usize) -> PResult<(Expr, usize)> {
        self.enter()?;
        let result = self.projection_items();
        self.leave();
        let (items, braces, items_height) = result?;
        let span = target.span.to(braces);
        let height = self.height(height.max(items_height), span)?;
        Ok((
            Expr {
                kind: ExprKind::MapProjection(Box::new(MapProjection {
                    target,
                    items,
                    braces,
                })),
                span,
            },
            height,
        ))
    }

    /// The items of a map projection from its `{` through its `}`, the span of the braces and the
    /// greatest height of the item values.
    fn projection_items(&mut self) -> PResult<(Vec<ProjectionItem>, Span, usize)> {
        let open = self.advance().span;
        let mut items = Vec::new();
        let mut height = 0;
        if self.eat(&TokenKind::RBrace).is_some() {
            return Ok((items, open.to(self.last), height));
        }
        loop {
            if let Some(dot) = self.eat(&TokenKind::Dot) {
                if let Some(star) = self.eat(&TokenKind::Star) {
                    items.push(ProjectionItem::AllProperties(dot.to(star)));
                } else {
                    let key = self.expect_name(&["a property name", "`*`"])?;
                    items.push(ProjectionItem::Property(key));
                }
            } else {
                let variable = matches!(
                    self.peek().kind,
                    TokenKind::Ident | TokenKind::QuotedIdent(_)
                );
                let key = self.expect_name(&["`.`", "a variable", "a key"])?;
                if self.eat(&TokenKind::Colon).is_some() {
                    let (value, value_height) = self.expr_bp(0)?;
                    height = height.max(value_height);
                    items.push(ProjectionItem::Entry(MapEntry { key, value }));
                } else if variable {
                    items.push(ProjectionItem::Variable(key));
                } else {
                    return Err(self.unexpected(&["`:`"]));
                }
            }
            if self.eat(&TokenKind::Comma).is_none() {
                break;
            }
        }
        self.expect(&TokenKind::RBrace, &["`,`", "`}`"])?;
        Ok((items, open.to(self.last), height))
    }

    /// At `[`: whether a list comprehension starts (a name, then `IN`).
    fn list_comprehension_ahead(&self) -> bool {
        matches!(
            self.nth(1).kind,
            TokenKind::Ident | TokenKind::QuotedIdent(_)
        ) && self.nth(2).keyword() == Some(Keyword::In)
    }

    /// `[x IN list WHERE cond | expr]`, at `[`.
    fn list_comprehension(&mut self) -> PResult<(Expr, usize)> {
        let open = self.advance().span;
        self.enter()?;
        let saved_bar = self.bar_ends;
        let result = self.comprehension_parts();
        self.bar_ends = saved_bar;
        self.leave();
        let (comprehension, height) = result?;
        let span = open.to(self.last);
        let height = self.height(height, span)?;
        Ok((
            Expr {
                kind: ExprKind::ListComprehension(Box::new(comprehension)),
                span,
            },
            height,
        ))
    }

    /// The parts of a list comprehension after its `[` through its `]`, with the greatest
    /// height among them.
    fn comprehension_parts(&mut self) -> PResult<(ListComprehension, usize)> {
        let token = self.advance();
        let variable = Identifier {
            name: token.name().unwrap_or("").to_string(),
            span: token.span,
        };
        self.advance();
        self.bar_ends = Some(self.open);
        let (list, mut height) = self.expr_bp(0)?;
        let filter = if self.eat_kw(Keyword::Where).is_some() {
            let (filter, filter_height) = self.expr_bp(0)?;
            height = height.max(filter_height);
            Some(filter)
        } else {
            None
        };
        let map = if self.eat(&TokenKind::Pipe).is_some() {
            self.bar_ends = None;
            let (map, map_height) = self.expr_bp(0)?;
            height = height.max(map_height);
            Some(map)
        } else {
            None
        };
        let expected: &[&str] = match (&filter, &map) {
            (_, Some(_)) => &["`]`"],
            (Some(_), None) => &["`|`", "`]`"],
            (None, None) => &["WHERE", "`|`", "`]`"],
        };
        self.comprehension_close(expected)?;
        Ok((
            ListComprehension {
                form: ComprehensionForm::List,
                variable,
                list,
                filter,
                map,
            },
            height,
        ))
    }

    /// At `[`: whether a pattern comprehension starts: a path pattern (after an optional
    /// `p =`) by the pattern-predicate lookahead, whose end is followed by `WHERE` or `|`.
    fn pattern_comprehension_ahead(&self) -> bool {
        let mut i = self.pos.saturating_add(1);
        if matches!(
            self.token_at(i).kind,
            TokenKind::Ident | TokenKind::QuotedIdent(_)
        ) && self.token_at(i.saturating_add(1)).kind == TokenKind::Eq
        {
            i = i.saturating_add(2);
        }
        if !self.pattern_ahead_at(i) {
            return false;
        }
        self.pattern_end(i).is_some_and(|end| {
            let token = self.token_at(end);
            token.kind == TokenKind::Pipe || token.keyword() == Some(Keyword::Where)
        })
    }

    /// The index after the path pattern whose first node pattern opens at token `i`, read as
    /// [`Parser::path_pattern`] reads it (node groups, edges as [`Parser::edge_ahead`] reads
    /// them, GQL quantifiers); `None` when the tokens there are no complete pattern. A token
    /// scan, no parse: every step moves forward, so it ends.
    fn pattern_end(&self, i: usize) -> Option<usize> {
        if self.token_at(i).kind != TokenKind::LParen {
            return None;
        }
        let mut end = self.skip_group(i)?;
        while let Some((after, _)) = self.edge_ahead(end) {
            let mut j = after;
            match self.token_at(j).kind {
                TokenKind::LBrace => j = self.skip_group(j)?,
                TokenKind::Star | TokenKind::Plus => j = j.saturating_add(1),
                _ => {}
            }
            if self.token_at(j).kind != TokenKind::LParen {
                return None;
            }
            end = self.skip_group(j)?;
        }
        Some(end)
    }

    /// `[p = (a)-[:T]->(b) WHERE cond | expr]`, at `[`. Like a subquery, the names declared in
    /// it do not feed the missing-`RETURN` alternative, and it is one level taller than the
    /// tallest tree inside it.
    fn pattern_comprehension(&mut self) -> PResult<(Expr, usize)> {
        let open = self.advance().span;
        self.enter()?;
        let saved_height = std::mem::replace(&mut self.inner_height, 0);
        let saved_variables = self.variables.len();
        let saved_bar = self.bar_ends;
        let result = self.pattern_comprehension_parts();
        self.bar_ends = saved_bar;
        let inner = std::mem::replace(&mut self.inner_height, saved_height);
        self.variables.truncate(saved_variables);
        self.leave();
        let comprehension = result?;
        let span = open.to(self.last);
        let height = self.height(inner, span)?;
        Ok((
            Expr {
                kind: ExprKind::PatternComprehension(Box::new(comprehension)),
                span,
            },
            height,
        ))
    }

    /// The parts of a pattern comprehension after its `[` through its `]`.
    fn pattern_comprehension_parts(&mut self) -> PResult<PatternComprehension> {
        let pattern = self.path_pattern()?;
        self.bar_ends = Some(self.open);
        let where_clause = if self.eat_kw(Keyword::Where).is_some() {
            Some(self.expr()?)
        } else {
            None
        };
        let body = MatchClause {
            optional: false,
            span: pattern.span.to(self.last),
            patterns: vec![pattern],
            where_clause,
        };
        let expected: &[&str] = if body.where_clause.is_some() {
            &["`|`"]
        } else {
            &["WHERE", "`|`"]
        };
        self.expect(&TokenKind::Pipe, expected)?;
        self.bar_ends = None;
        let map = self.expr()?;
        self.comprehension_close(&["`]`"])?;
        Ok(PatternComprehension { body, map })
    }

    /// The `]` closing a comprehension; a `|` in its place most likely ended a label
    /// alternative too early (module docs), which the error explains.
    fn comprehension_close(&mut self, expected: &[&str]) -> PResult<()> {
        if self.eat(&TokenKind::RBracket).is_some() {
            return Ok(());
        }
        let error = self.unexpected(expected);
        if !self.at(&TokenKind::Pipe) {
            return Err(error);
        }
        Err(Box::new((*error).alternative(
            "a `|` in a comprehension ends its list or its WHERE condition; write a label \
             alternative there in parentheses, as in [x IN l WHERE x:(A|B) | x.name]",
        )))
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

    /// At a name: whether the record constructor `RECORD {k: expr, ...}` starts. `RECORD` (any
    /// case, not back-quoted) followed by `{` and then `}` or `name :` is the constructor;
    /// followed by `{` and anything else (`.key`, `.*`, a variable item) it stays a variable
    /// with a map projection (module docs, "Record constructor").
    fn record_ahead(&self) -> bool {
        is_word(self.peek(), "RECORD")
            && self.nth(1).kind == TokenKind::LBrace
            && (self.nth(2).kind == TokenKind::RBrace
                || (self.nth(2).name().is_some() && self.nth(3).kind == TokenKind::Colon))
    }

    /// `RECORD {k: expr, ...}`, at `RECORD`: the same map value as the literal `{k: expr, ...}`.
    fn record_constructor(&mut self) -> PResult<(Expr, usize)> {
        let keyword = self.advance().span;
        let (map, height) = self.map_literal()?;
        Ok((
            Expr {
                span: keyword.to(map.span),
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
        if let Some(form) = list_function(self.peek().name().unwrap_or("")) {
            return self.list_function(form);
        }
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
        match spec.function {
            Function::Cast => return self.cast_call(name),
            Function::Trim => return self.trim_call(spec, name),
            Function::Substring => return self.substring_call(spec, name),
            _ => {}
        }
        let (args, height) = self.expr_list(&TokenKind::RParen, &["`,`", "`)`"])?;
        self.finish_call(spec, name, false, args, height)
    }

    /// Cypher's `all` / `any` / `none` / `single(x IN list WHERE cond)` (`form` the predicate)
    /// and `reduce(acc = init, x IN list | expr)` (`form` `None`), at the name (stage 2d, user
    /// decision D23): one [`ListComprehension`] node with its [`ComprehensionForm`]. One
    /// [`MAX_DEPTH`] level, like a list comprehension.
    #[inline(never)]
    fn list_function(&mut self, form: Option<ListPredicate>) -> PResult<(Expr, usize)> {
        let name = self.advance().span;
        self.expect(&TokenKind::LParen, &["`(`"])?;
        self.enter()?;
        let saved_bar = self.bar_ends;
        let result = self.list_function_parts(form);
        self.bar_ends = saved_bar;
        self.leave();
        let (comprehension, height) = result?;
        let span = name.to(self.last);
        let height = self.height(height, span)?;
        Ok((
            Expr {
                kind: ExprKind::ListComprehension(Box::new(comprehension)),
                span,
            },
            height,
        ))
    }

    /// The parts of [`Self::list_function`] after its `(` through its `)`, with the greatest
    /// height among them. Inside `reduce` a `|` at its own bracket level ends the list, as in a
    /// list comprehension.
    fn list_function_parts(
        &mut self,
        form: Option<ListPredicate>,
    ) -> PResult<(ListComprehension, usize)> {
        let mut height = 0;
        let accumulator = match form {
            Some(_) => None,
            None => {
                let variable = self.local_variable()?;
                self.expect(&TokenKind::Eq, &["`=`"])?;
                let (init, init_height) = self.expr_bp(0)?;
                height = init_height;
                self.expect(&TokenKind::Comma, &["`,`"])?;
                Some(ReduceAccumulator { variable, init })
            }
        };
        let variable = self.local_variable()?;
        self.expect_kw(Keyword::In, &["IN"])?;
        if accumulator.is_some() {
            self.bar_ends = Some(self.open);
        }
        let (list, list_height) = self.expr_bp(0)?;
        height = height.max(list_height);
        let (filter, map) = if accumulator.is_some() {
            self.expect(&TokenKind::Pipe, &["`|`"])?;
            self.bar_ends = None;
            let (map, map_height) = self.expr_bp(0)?;
            height = height.max(map_height);
            (None, Some(map))
        } else {
            self.expect_kw(Keyword::Where, &["WHERE"])?;
            let (filter, filter_height) = self.expr_bp(0)?;
            height = height.max(filter_height);
            (Some(filter), None)
        };
        self.expect(&TokenKind::RParen, &["`)`"])?;
        let form = match (form, accumulator) {
            (Some(predicate), _) => ComprehensionForm::Predicate(predicate),
            (None, Some(accumulator)) => ComprehensionForm::Reduce(Box::new(accumulator)),
            (None, None) => ComprehensionForm::List,
        };
        Ok((
            ListComprehension {
                form,
                variable,
                list,
                filter,
                map,
            },
            height,
        ))
    }

    /// The local variable of a list function: a plain or back-quoted name.
    fn local_variable(&mut self) -> PResult<Identifier> {
        if !matches!(
            self.peek().kind,
            TokenKind::Ident | TokenKind::QuotedIdent(_)
        ) {
            return Err(self.unexpected(&["a variable"]));
        }
        let token = self.advance();
        Ok(Identifier {
            name: token.name().unwrap_or("").to_string(),
            span: token.span,
        })
    }

    /// `CAST(operand AS type)` after its `(`.
    fn cast_call(&mut self, name: Identifier) -> PResult<(Expr, usize)> {
        let (operand, height) = self.expr_bp(0)?;
        self.expect_kw(Keyword::As, &["AS"])?;
        let (target, target_span) = self.cast_target()?;
        self.expect(&TokenKind::RParen, &["`)`"])?;
        let span = name.span.to(self.last);
        let height = self.height(height, span)?;
        Ok((
            Expr {
                kind: ExprKind::FunctionCall {
                    name,
                    distinct: false,
                    args: CallArgs::Cast {
                        operand: Box::new(operand),
                        target,
                        target_span,
                    },
                },
                span,
            },
            height,
        ))
    }

    /// The type name of `CAST`: one or two words (`CAST_TYPES`); a GQL type Graphersal has
    /// no values of is "not yet supported", an unknown name a syntax error with a suggestion.
    fn cast_target(&mut self) -> PResult<(CastTarget, Span)> {
        let first = self.peek().clone();
        let Some(word) = first.name() else {
            return Err(self.unexpected(&["a type name"]));
        };
        let second = self.nth(1).clone();
        let two = second
            .name()
            .filter(|_| second.kind != TokenKind::RParen)
            .map(|next| format!("{word} {next}"));
        let found = |text: &str| {
            CAST_TYPES
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(text))
                .map(|(_, target)| *target)
        };
        let (target, span, written) = match two
            .as_deref()
            .and_then(|two| found(two).map(|target| (target, two.to_string())))
        {
            Some((target, written)) => {
                self.advance();
                self.advance();
                (target, first.span.to(second.span), written)
            }
            None => match found(word) {
                Some(target) => {
                    self.advance();
                    (target, first.span, word.to_string())
                }
                None => (None, first.span, word.to_string()),
            },
        };
        if let Some(target) = target {
            return Ok((target, span));
        }
        let supported = "CAST supports INTEGER, FLOAT, STRING and BOOLEAN";
        if CAST_TYPES
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(&written))
            || UNSUPPORTED_CAST_TYPES
                .iter()
                .any(|name| name.eq_ignore_ascii_case(word))
        {
            return Err(self.not_yet(
                span,
                &format!("CAST to {}", written.to_ascii_uppercase()),
                supported,
            ));
        }
        let mut b = err(
            GqlErrorKind::Syntax,
            first.span,
            format!("unknown type {word} in CAST"),
        )
        .expected(["INTEGER", "FLOAT", "STRING", "BOOLEAN"])
        .alternative(supported);
        if let Some(close) = CAST_TYPES
            .iter()
            .filter(|(_, target)| target.is_some())
            .filter_map(|(name, _)| close_to(word, name).map(|d| (d, *name)))
            .min_by_key(|(d, _)| *d)
            .map(|(_, name)| name)
        {
            b = b.suggestion(close);
        }
        Err(Box::new(b))
    }

    /// `TRIM(...)` after its `(`: the SQL forms `TRIM([LEADING | TRAILING | BOTH] [c] FROM s)`
    /// become [`CallArgs::Trim`]; `TRIM(s)` and `trim(list, n)` are ordinary calls. The side
    /// words and `FROM` are contextual: a side word followed by `)`, `,`, `.` or an operator is
    /// a variable.
    fn trim_call(&mut self, spec: &FunctionSpec, name: Identifier) -> PResult<(Expr, usize)> {
        let side = trim_side(self.peek()).filter(|_| starts_operand(self.nth(1)));
        if let Some(side) = side {
            self.advance();
            let character = if is_word(self.peek(), "FROM") && starts_operand(self.nth(1)) {
                None
            } else {
                let (character, height) = self.expr_bp(0)?;
                if !is_word(self.peek(), "FROM") {
                    let b = err(
                        GqlErrorKind::Syntax,
                        self.peek().span,
                        format!("unexpected {}", describe(self.peek())),
                    )
                    .expected(["FROM"])
                    .alternative(
                        "write TRIM(LEADING 'x' FROM s), TRIM(LEADING FROM s) or ltrim(s)",
                    );
                    return Err(Box::new(b));
                }
                Some((character, height))
            };
            return self.trim_from(name, side, character);
        }
        if is_word(self.peek(), "FROM") && starts_operand(self.nth(1)) {
            return self.trim_from(name, TrimSide::Both, None);
        }
        let first = self.expr_bp(0)?;
        if is_word(self.peek(), "FROM") {
            return self.trim_from(name, TrimSide::Both, Some(first));
        }
        self.rest_of_call(spec, name, first)
    }

    /// From the `FROM` of a SQL `TRIM` to its `)`.
    fn trim_from(
        &mut self,
        name: Identifier,
        side: TrimSide,
        character: Option<(Expr, usize)>,
    ) -> PResult<(Expr, usize)> {
        self.advance();
        let (source, source_height) = self.expr_bp(0)?;
        self.expect(&TokenKind::RParen, &["`)`"])?;
        let span = name.span.to(self.last);
        let child = character
            .as_ref()
            .map_or(0, |(_, height)| *height)
            .max(source_height);
        let height = self.height(child, span)?;
        Ok((
            Expr {
                kind: ExprKind::FunctionCall {
                    name,
                    distinct: false,
                    args: CallArgs::Trim {
                        side,
                        character: character.map(|(character, _)| Box::new(character)),
                        source: Box::new(source),
                    },
                },
                span,
            },
            height,
        ))
    }

    /// `substring(...)` after its `(`; the SQL form `SUBSTRING(s FROM a [FOR b])` is refused
    /// with the alternative.
    fn substring_call(&mut self, spec: &FunctionSpec, name: Identifier) -> PResult<(Expr, usize)> {
        let first = self.expr_bp(0)?;
        if is_word(self.peek(), "FROM") || self.at_kw(Keyword::For) {
            let b = err(
                GqlErrorKind::Syntax,
                self.peek().span,
                "SUBSTRING(s FROM a FOR b) is SQL, not GQL",
            )
            .alternative(
                "use substring(s, start, length) with a 0-based start, as in \
                 substring('graph', 1, 3) = 'rap', or left(s, n) / right(s, n)",
            );
            return Err(Box::new(b));
        }
        self.rest_of_call(spec, name, first)
    }

    /// The arguments after the first one (already parsed) to the `)`, then the call.
    fn rest_of_call(
        &mut self,
        spec: &FunctionSpec,
        name: Identifier,
        (first, mut height): (Expr, usize),
    ) -> PResult<(Expr, usize)> {
        let mut args = vec![first];
        while self.eat(&TokenKind::Comma).is_some() {
            let (arg, arg_height) = self.expr_bp(0)?;
            height = height.max(arg_height);
            args.push(arg);
        }
        self.expect(&TokenKind::RParen, &["`,`", "`)`"])?;
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
        if spec.is_none()
            && let Some(unsupported) = lookup_unsupported_function(name)
        {
            return Err(unsupported_function(unsupported, name, span));
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

/// The type names of `CAST`, one or two words; `None`: a GQL type Graphersal has no values of.
const CAST_TYPES: &[(&str, Option<CastTarget>)] = &[
    ("INTEGER", Some(CastTarget::Integer)),
    ("INT", Some(CastTarget::Integer)),
    ("INT64", Some(CastTarget::Integer)),
    ("INTEGER64", Some(CastTarget::Integer)),
    ("BIGINT", Some(CastTarget::Integer)),
    ("SIGNED INTEGER", Some(CastTarget::Integer)),
    ("SIGNED INTEGER64", Some(CastTarget::Integer)),
    ("SIGNED BIGINT", Some(CastTarget::Integer)),
    ("FLOAT", Some(CastTarget::Float)),
    ("FLOAT64", Some(CastTarget::Float)),
    ("DOUBLE", Some(CastTarget::Float)),
    ("DOUBLE PRECISION", Some(CastTarget::Float)),
    ("REAL", Some(CastTarget::Float)),
    ("STRING", Some(CastTarget::String)),
    ("VARCHAR", Some(CastTarget::String)),
    ("CHAR VARYING", Some(CastTarget::String)),
    ("CHARACTER VARYING", Some(CastTarget::String)),
    ("BOOLEAN", Some(CastTarget::Boolean)),
    ("BOOL", Some(CastTarget::Boolean)),
    ("UNSIGNED INTEGER", None),
    ("ZONED DATETIME", None),
    ("LOCAL DATETIME", None),
    ("ZONED TIME", None),
    ("LOCAL TIME", None),
];

/// GQL type names (first word) that `CAST` does not support yet: "not yet", not "unknown".
const UNSUPPORTED_CAST_TYPES: &[&str] = &[
    "INT8",
    "INT16",
    "INT32",
    "INT128",
    "INT256",
    "SMALLINT",
    "UINT",
    "UINT8",
    "UINT16",
    "UINT32",
    "UINT64",
    "UINT128",
    "UINT256",
    "USMALLINT",
    "UBIGINT",
    "UNSIGNED",
    "SIGNED",
    "INTEGER8",
    "INTEGER16",
    "INTEGER32",
    "INTEGER128",
    "INTEGER256",
    "DECIMAL",
    "DEC",
    "FLOAT16",
    "FLOAT32",
    "FLOAT128",
    "FLOAT256",
    "CHAR",
    "CHARACTER",
    "BYTES",
    "BINARY",
    "VARBINARY",
    "DATE",
    "TIME",
    "DATETIME",
    "TIMESTAMP",
    "LOCAL",
    "ZONED",
    "DURATION",
    "LIST",
    "ARRAY",
    "RECORD",
    "PATH",
    "NODE",
    "VERTEX",
    "EDGE",
    "RELATIONSHIP",
    "ANY",
    "NOTHING",
    "NULL",
    "MAP",
];

/// The not yet supported (or refused) function `name` as its targeted error.
fn unsupported_function(
    unsupported: &UnsupportedFunction,
    name: &str,
    span: Span,
) -> Box<GqlErrorBuilder> {
    let (kind, message) = if unsupported.refused {
        (
            GqlErrorKind::Refused,
            format!(
                "{name}() is not supported: {} breaks LET / WITH substitution and plan determinism",
                unsupported.what
            ),
        )
    } else {
        (
            GqlErrorKind::NotYet,
            format!(
                "{name}() ({}) is not yet supported in this version",
                unsupported.what
            ),
        )
    };
    Box::new(err(kind, span, message).alternative(unsupported.alternative))
}

/// The "not yet" error of a whole-query subquery body that ends without `RETURN`, at its first
/// clause other than `MATCH` (`span`, its keywords `word`).
fn body_without_return(span: Span, word: &str, kind: SubqueryKind) -> Box<GqlErrorBuilder> {
    let b = err(
        GqlErrorKind::NotYet,
        span,
        format!(
            "{word} in the body of {} {{ }} without RETURN is not yet supported in this version",
            kind_name(kind)
        ),
    )
    .alternative(
        "end the body with RETURN (a body of other clauses than MATCH is a whole query), or \
         write only MATCH clauses, as in MATCH (p:person) WHERE EXISTS { MATCH \
         (p)-[:created]->(s) WHERE s.lang = 'java' } RETURN p.name",
    );
    Box::new(b)
}

/// Whether `keyword` starts a pipeline clause other than `MATCH`, `RETURN` and `CALL`.
fn stage_keyword(keyword: Option<Keyword>) -> bool {
    matches!(
        keyword,
        Some(
            Keyword::Optional
                | Keyword::With
                | Keyword::Let
                | Keyword::Filter
                | Keyword::For
                | Keyword::Unwind
        )
    )
}

/// The word of a subquery kind, for messages.
fn kind_name(kind: SubqueryKind) -> &'static str {
    match kind {
        SubqueryKind::Exists => "EXISTS",
        SubqueryKind::Count => "COUNT",
        SubqueryKind::Collect => "COLLECT",
    }
}

/// The subquery kind a word before `{` names: `COUNT` or `COLLECT` (contextual words, any
/// case).
fn counted_kind(token: &Token<'_>) -> Option<SubqueryKind> {
    if is_word(token, "COUNT") {
        Some(SubqueryKind::Count)
    } else if is_word(token, "COLLECT") {
        Some(SubqueryKind::Collect)
    } else {
        None
    }
}

/// Moves a Cypher importing `WITH` at the start of `query` (an operand of a `CALL { }` body
/// without a scope clause) into [`LinearQuery::importing`]: a `WITH` of plain variables without
/// aliases, or of `*` alone, with no `DISTINCT`, `ORDER BY`, `OFFSET`, `LIMIT` or `WHERE`. Any
/// other first `WITH` stays a clause of the body.
fn importing_with(query: &mut LinearQuery) {
    let Some(Stage::With(with)) = query.stages.first() else {
        return;
    };
    let plain = !with.distinct
        && with.order_by.is_none()
        && with.offset.is_none()
        && with.limit.is_none()
        && with.where_clause.is_none();
    let variables: Option<Vec<Identifier>> = with
        .items
        .iter()
        .map(|item| match (&item.expr.kind, &item.alias) {
            (ExprKind::Variable(name), None) => Some(name.clone()),
            _ => None,
        })
        .collect();
    let importing = match (plain, with.star, variables) {
        (true, Some(star), Some(variables)) if variables.is_empty() => ImportingWith {
            star: Some(star),
            variables,
            span: with.span,
        },
        (true, None, Some(variables)) if !variables.is_empty() => ImportingWith {
            star: None,
            variables,
            span: with.span,
        },
        _ => return,
    };
    query.stages.remove(0);
    query.importing = Some(importing);
}

/// Whether `token` is the contextual word `word` (an identifier, any case).
fn is_word(token: &Token<'_>, word: &str) -> bool {
    token.kind == TokenKind::Ident && token.text.eq_ignore_ascii_case(word)
}

/// The `TRIM` side `token` spells, if any.
fn trim_side(token: &Token<'_>) -> Option<TrimSide> {
    [
        ("LEADING", TrimSide::Leading),
        ("TRAILING", TrimSide::Trailing),
        ("BOTH", TrimSide::Both),
    ]
    .into_iter()
    .find(|(word, _)| is_word(token, word))
    .map(|(_, side)| side)
}

/// The end `token` spells after `IS [NOT]`, if any.
fn endpoint_word(token: &Token<'_>) -> Option<Endpoint> {
    if is_word(token, "SOURCE") {
        Some(Endpoint::Source)
    } else if is_word(token, "DESTINATION") {
        Some(Endpoint::Destination)
    } else {
        None
    }
}

/// Whether `token` can start an operand (so a contextual word before it is not a variable):
/// not `)`, `,`, `.`, `:`, `[`, the end, or an infix operator.
fn starts_operand(token: &Token<'_>) -> bool {
    match &token.kind {
        TokenKind::RParen
        | TokenKind::Comma
        | TokenKind::Dot
        | TokenKind::Colon
        | TokenKind::LBracket
        | TokenKind::Eof
        | TokenKind::Eq
        | TokenKind::Ne
        | TokenKind::Lt
        | TokenKind::Le
        | TokenKind::Gt
        | TokenKind::Ge
        | TokenKind::Plus
        | TokenKind::Concat
        | TokenKind::Star
        | TokenKind::Slash
        | TokenKind::Percent => false,
        TokenKind::Keyword(keyword) => !matches!(
            keyword,
            Keyword::And
                | Keyword::Or
                | Keyword::Xor
                | Keyword::Is
                | Keyword::In
                | Keyword::Contains
                | Keyword::Starts
                | Keyword::Ends
                | Keyword::As
        ),
        _ => true,
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

/// The function name closest to `word` (every accepted spelling); a word of at least five
/// characters that begins a name suggests that name first (Cypher 9's `toInt` -> `toInteger`).
fn suggest_function(word: &str) -> Option<&'static str> {
    let names = || {
        FUNCTIONS
            .iter()
            .flat_map(|spec| spec.names.iter().copied())
            .chain(["element_id"])
    };
    let lower = word.to_ascii_lowercase();
    if word.chars().count() >= 5
        && let Some(name) = names().find(|name| name.to_ascii_lowercase().starts_with(&lower))
    {
        return Some(name);
    }
    names()
        .filter_map(|name| close_to(word, name).map(|d| (d, name)))
        .min_by_key(|(d, _)| *d)
        .map(|(_, name)| name)
}

#[cfg(test)]
mod tests;
