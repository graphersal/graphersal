//! The three-valued GQL expression evaluator (stage 1a): pure, independent of the traversal
//! pipeline; the internal steps `gql:filter` and `gql:project` run it per row.
//!
//! An AST [`Expr`] is compiled once ([`compile`]) into a [`Compiled`] tree: function names are
//! resolved to [`Function`], variables to slots, parameters and literals (and lists / maps made
//! only of them) to constants, property keys and labels to [`CompactString`]s. Evaluating a row
//! ([`Compiled::eval`], [`Compiled::truth`]) then neither parses nor allocates names; it reads the
//! row and the graph through an [`Env`].
//!
//! # Values
//!
//! [`Value`] is a GQL value: `NULL`, a scalar [`ElementProperty`] (number, string, boolean, uuid,
//! temporal, vector, ...; never `Null`, `Array`, `Object` or a compressed string), a list, a map
//! (string keys, written order) or an element reference of the [`Env`] (a vertex or an edge, kept
//! as a handle). A stored property enters through [`Value::from_property`] (missing and stored
//! `null` both read as `NULL`) and leaves through [`Value::into_property`].
//!
//! # Semantics and decisions
//!
//! - **Three-valued logic:** `AND`, `OR`, `XOR`, `NOT` follow the SQL / GQL truth tables over
//!   TRUE, FALSE and UNKNOWN (`NULL`); `AND` / `OR` skip the right operand once the left decides.
//!   A filter row passes only on TRUE. A non-boolean operand is an error (22G03).
//! - **Comparisons** `= <> < <= > >=` use `compare_scalars`: numbers by value across integer and
//!   float, strings, booleans, uuids, one temporal type. Values of incomparable kinds, `NaN` and
//!   any `NULL` operand give `NULL`, never an error. `=` / `<>` also compare lists (same length,
//!   element-wise, three-valued), maps (same keys, value-wise) and elements (the same vertex or
//!   edge); a vertex never equals an edge.
//! - **`IN`:** TRUE when an item equals the value; else `NULL` when some comparison was `NULL`
//!   (`2 IN [1, NULL]`, `NULL IN [1]`); else FALSE (`NULL IN []` is FALSE). A right operand that
//!   is `NULL` or not a list gives `NULL`.
//! - **`STARTS WITH` / `ENDS WITH` / `CONTAINS`:** strings only; a `NULL` or non-string operand
//!   gives `NULL`.
//! - **Arithmetic** `+ - * / %`: integer with integer stays integer (division truncates toward
//!   zero, `%` takes the dividend's sign); any float operand makes it float. Overflow is error
//!   22003 (numeric value out of range), never a wrap; a float result that is not finite from
//!   finite operands is 22003 too. Division or `%` by zero (integer or float) is error 22012.
//!   `NULL` propagates. Any other operand kind is error 22G03.
//! - **Concatenation:** `||` joins two strings or two lists. Cypher `+` is its alias on two
//!   strings and two lists. **`+` of a string and a number is refused** (22G03): there is no
//!   implicit number-to-text conversion (Neo4j converts; Graphersal never turns values into text
//!   behind the user's back).
//! - **`CASE`:** simple `CASE` matches a `WHEN` value only when `=` is TRUE, so `NULL` never
//!   matches; searched `CASE` takes the first `WHEN` that is TRUE; no match and no `ELSE` gives
//!   `NULL`.
//! - **`IS LABELED`:** a vertex tests its label set (`%`: the set is not empty), an edge its one
//!   label; `NULL` gives `NULL`; a non-element is error 22G03.
//! - **Property access:** on `NULL` gives `NULL`; on an element reads the property (missing:
//!   `NULL`); on a map reads the key (missing: `NULL`); on anything else error 22G03.
//! - **Functions** (`parser::FUNCTIONS`, scalar ones): `element_id` (the id as a string),
//!   `labels` (a vertex's labels in stored order, primary first; an edge's one label as a
//!   one-item list), `type` (the edge label; **a vertex is error 22G03**, as Neo4j refuses a
//!   node; use `labels(v)`), `coalesce` (the first non-`NULL` argument, later ones not
//!   evaluated), `upper` / `lower`, `char_length`, `size` / `cardinality` (characters of a
//!   string, items of a list), `path_length` / Cypher `length` (the edges of a path). Every one
//!   returns `NULL` for a `NULL` argument and error 22G03 for an argument of the wrong kind.
//! - **Stage 1c functions** live in [`functions`] (one file per family; its module docs are the
//!   contract): the evaluator evaluates every argument left to right and dispatches there;
//!   `CAST` compiles to its own node carrying the target type, the SQL `TRIM` forms to theirs,
//!   `IS [NOT] SOURCE OF` / `DESTINATION OF` to a chain link. Results larger than their inputs
//!   ask [`Env::reserve`] first (design F6), `||` / `+` of two strings or lists included.
//! - **Paths:** a [`Value::Path`] is the elements of a matched path in text order (vertex, edge,
//!   vertex, ...). Two paths are equal when they hold the same elements in the same order; a
//!   path compared with a value of another kind gives `NULL`, like any two incomparable kinds.
//! - **Aggregates** (`count`, `sum`, `avg`, `min`, `max`, `collect_list`) are compiled only where
//!   the [`Scope`] admits them ([`Scope::aggregate`], the aggregating stage `aggregate.rs`); the
//!   compiled call reads its result through [`Env::aggregate`]. Elsewhere they are error 0A000.
//!
//! Errors are [`EvalError`]s carrying a GQLSTATUS ([`status`]), a message and the span of the
//! offending expression; the step that runs the evaluator maps them into a `TraverserError`.

use std::borrow::Cow;
use std::cmp::Ordering;

use super::ast::{
    BinaryOp, CallArgs, CaseExpr, CastTarget, Endpoint, Expr, ExprKind, LabelExpr, LabelExprKind,
    Literal, MapLiteral, TrimSide, UnaryOp,
};
use super::functions::{self, Arg, Family};
use super::lexer::Span;
use super::parser::{Function, FunctionKind, FunctionSpec, lookup_function};
use crate::error::GqlHelpTopic;
use crate::value::number::{N, Number};
use crate::value::scalar::compare_scalars;
use crate::{CompactString, ElementKind, ElementProperty, PropertyMap};

#[cfg(test)]
mod tests;

/// The GQLSTATUS codes the evaluator raises.
pub(crate) mod status {
    /// Data exception: division by zero.
    pub(crate) const DIVISION_BY_ZERO: &str = "22012";
    /// Data exception: numeric value out of range.
    pub(crate) const OUT_OF_RANGE: &str = "22003";
    /// Data exception: invalid value type (an operand or argument of the wrong kind).
    pub(crate) const INVALID_VALUE_TYPE: &str = "22G03";
    /// Syntax error or access rule violation: an unknown variable or parameter at compile time.
    pub(crate) const SEMANTIC: &str = "42002";
    /// Feature not supported: an aggregate function outside an aggregation (stage 1b), a
    /// function not implemented yet.
    pub(crate) const NOT_SUPPORTED: &str = "0A000";
    /// Data exception: invalid character value for cast (`CAST('x' AS INTEGER)`).
    pub(crate) const INVALID_CAST: &str = "22018";
    /// Data exception: substring error (a negative position or length).
    pub(crate) const SUBSTRING: &str = "22011";
    /// Data exception: trim error (a trim character that is not exactly one character).
    pub(crate) const TRIM: &str = "22027";
    /// Data exception: invalid argument for natural logarithm (`ln(0)`, `log10(-1)`).
    pub(crate) const LOGARITHM: &str = "2201E";
    /// Data exception: invalid argument for power function (`sqrt(-1)`, `power(0, -1)`).
    pub(crate) const POWER: &str = "2201F";
    /// Data exception: invalid parameter value (`range` step 0, a `round` mode or negative
    /// precision, a negative `replace` limit or list `trim` count).
    pub(crate) const INVALID_PARAMETER: &str = "22023";
    /// Program limit exceeded: [`Env::reserve`](super::Env::reserve) refused a result's size
    /// (the step reports the row's own limit error instead, see `runtime::Row`).
    pub(crate) const LIMIT: &str = "54001";
}

/// A failure of [`compile`] or of an evaluation.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EvalError {
    /// The GQLSTATUS code (one of [`status`]).
    pub(crate) gqlstatus: &'static str,
    /// What went wrong.
    pub(crate) message: String,
    /// The offending expression.
    pub(crate) span: Span,
    /// The general advice of the error's `help()` (the kind's own by default).
    pub(crate) topic: GqlHelpTopic,
}

impl EvalError {
    pub(crate) fn new(gqlstatus: &'static str, message: impl Into<String>, span: Span) -> Self {
        Self {
            gqlstatus,
            message: message.into(),
            span,
            topic: GqlHelpTopic::Kind,
        }
    }

    /// The error with `topic` as the general advice of its `help()`.
    pub(crate) fn with_topic(mut self, topic: GqlHelpTopic) -> Self {
        self.topic = topic;
        self
    }

    /// A 22G03 error (an operand or argument of the wrong kind).
    pub(crate) fn invalid_type(message: impl Into<String>, span: Span) -> Self {
        Self::new(status::INVALID_VALUE_TYPE, message, span)
    }

    /// A 22003 error (numeric value out of range).
    pub(crate) fn out_of_range(span: Span) -> Self {
        Self::new(
            status::OUT_OF_RANGE,
            "numeric value out of range: the result does not fit",
            span,
        )
    }

    /// A 22012 error.
    pub(crate) fn division_by_zero(span: Span) -> Self {
        Self::new(status::DIVISION_BY_ZERO, "division by zero", span)
    }

    /// The stop of an evaluation that needs a subquery the row has not run yet
    /// ([`Env::subquery`]): the hosting step runs it and evaluates again, so a row never reports
    /// it; an [`Env`] without a host (none in the pipeline) reports it as 0A000.
    pub(crate) fn subquery_pending(span: Span) -> Self {
        Self::new(
            status::NOT_SUPPORTED,
            "a subquery (EXISTS) cannot be evaluated here",
            span,
        )
    }
}

/// A truth value of three-valued logic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Truth {
    /// TRUE.
    True,
    /// FALSE.
    False,
    /// UNKNOWN, the boolean `NULL`.
    Unknown,
}

impl Truth {
    /// TRUE or FALSE.
    pub(crate) fn of(flag: bool) -> Self {
        if flag { Truth::True } else { Truth::False }
    }

    /// `NOT self`.
    pub(crate) fn not(self) -> Self {
        match self {
            Truth::True => Truth::False,
            Truth::False => Truth::True,
            Truth::Unknown => Truth::Unknown,
        }
    }

    /// `self AND other`.
    pub(crate) fn and(self, other: Self) -> Self {
        match (self, other) {
            (Truth::False, _) | (_, Truth::False) => Truth::False,
            (Truth::True, Truth::True) => Truth::True,
            _ => Truth::Unknown,
        }
    }

    /// `self OR other`.
    pub(crate) fn or(self, other: Self) -> Self {
        match (self, other) {
            (Truth::True, _) | (_, Truth::True) => Truth::True,
            (Truth::False, Truth::False) => Truth::False,
            _ => Truth::Unknown,
        }
    }

    /// `self XOR other`.
    pub(crate) fn xor(self, other: Self) -> Self {
        match (self, other) {
            (Truth::Unknown, _) | (_, Truth::Unknown) => Truth::Unknown,
            (a, b) => Truth::of(a != b),
        }
    }

    /// The value: a boolean, or `NULL` for UNKNOWN.
    pub(crate) fn into_value<E>(self) -> Value<E> {
        match self {
            Truth::True => Value::Scalar(ElementProperty::Boolean(true)),
            Truth::False => Value::Scalar(ElementProperty::Boolean(false)),
            Truth::Unknown => Value::Null,
        }
    }
}

/// A GQL value over element references `E` (see the module docs).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Value<E> {
    /// `NULL`.
    Null,
    /// A scalar: never `Null`, `Array`, `Object`, `Compressed` or `Deduped`.
    Scalar(ElementProperty),
    /// A list.
    List(Vec<Value<E>>),
    /// A map with string keys in written order (keys are unique).
    Map(Vec<(CompactString, Value<E>)>),
    /// A vertex or an edge.
    Element(E),
    /// A path: its elements in text order, vertices at the even positions and edges at the odd
    /// ones, starting and ending with a vertex (a zero-length path is one vertex).
    Path(Vec<E>),
}

impl<E> Value<E> {
    /// A stored or parameter value as a GQL value: `null` is `NULL`, an array a list, a
    /// string-keyed object a map, a compressed string the plain string.
    pub(crate) fn from_property(value: ElementProperty) -> Self {
        match value.into_logical() {
            ElementProperty::Null => Value::Null,
            ElementProperty::Array(items) => {
                Value::List(items.into_iter().map(Value::from_property).collect())
            }
            ElementProperty::Object(map) => Value::Map(
                (*map)
                    .into_iter()
                    .map(|(key, value)| (key, Value::from_property(value)))
                    .collect(),
            ),
            other => Value::Scalar(other),
        }
    }

    /// The value as a property value; `element` turns an element reference into one.
    #[cfg_attr(not(test), allow(dead_code, reason = "for stage 1b (aggregates, LET)"))]
    pub(crate) fn into_property(
        self,
        element: &mut impl FnMut(E) -> ElementProperty,
    ) -> ElementProperty {
        match self {
            Value::Null => ElementProperty::Null,
            Value::Scalar(value) => value,
            Value::List(items) => ElementProperty::Array(
                items
                    .into_iter()
                    .map(|item| item.into_property(element))
                    .collect(),
            ),
            Value::Map(entries) => {
                let mut map = PropertyMap::with_capacity(entries.len());
                for (key, value) in entries {
                    map.insert(key, value.into_property(element));
                }
                ElementProperty::Object(Box::new(map))
            }
            Value::Element(handle) => element(handle),
            Value::Path(elements) => {
                ElementProperty::Array(elements.into_iter().map(&mut *element).collect())
            }
        }
    }

    /// Whether it is `NULL`.
    pub(crate) fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// A string value.
    pub(crate) fn string(text: impl Into<CompactString>) -> Self {
        Value::Scalar(ElementProperty::String(text.into()))
    }

    /// An integer value.
    pub(crate) fn integer(value: i64) -> Self {
        Value::Scalar(ElementProperty::Number(Number::from_i64(value)))
    }

    /// A float value.
    pub(crate) fn float(value: f64) -> Self {
        Value::Scalar(ElementProperty::Number(Number::from_f64(value)))
    }

    /// A boolean value.
    pub(crate) fn boolean(value: bool) -> Self {
        Value::Scalar(ElementProperty::Boolean(value))
    }

    /// The text of a string value.
    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Value::Scalar(value) => value.as_str(),
            _ => None,
        }
    }

    /// A short name of the value's kind, for error messages.
    pub(crate) fn kind_name(&self) -> &'static str {
        match self {
            Value::Null => "NULL",
            Value::Scalar(ElementProperty::Number(_)) => "a number",
            Value::Scalar(ElementProperty::String(_)) => "a string",
            Value::Scalar(ElementProperty::Boolean(_)) => "a boolean",
            Value::Scalar(_) => "a scalar value",
            Value::List(_) => "a list",
            Value::Map(_) => "a map",
            Value::Element(_) => "an element",
            Value::Path(_) => "a path",
        }
    }
}

/// What the evaluator reads from the row and the graph.
pub(crate) trait Env {
    /// A reference to a vertex or an edge; equal exactly when it is the same element.
    type Element: Clone + PartialEq;

    /// The value bound to the variable in `slot` (from [`Scope::variable`]); `NULL` when unbound.
    fn variable(&self, slot: usize) -> Value<Self::Element>;
    /// Whether `element` is a vertex or an edge.
    fn kind(&self, element: &Self::Element) -> ElementKind;
    /// The element's id as a string.
    fn element_id(&self, element: &Self::Element) -> CompactString;
    /// The element's property `key`; `None` when it has none.
    fn property(&self, element: &Self::Element, key: &str) -> Option<ElementProperty>;
    /// Whether `label` is in the element's label set (an edge: its one label).
    fn has_label(&self, element: &Self::Element, label: &str) -> bool;
    /// Whether the element has at least one label (`%`).
    fn has_any_label(&self, element: &Self::Element) -> bool;
    /// The element's labels in stored order, the primary label first (an edge: its one label).
    fn labels(&self, element: &Self::Element) -> Vec<CompactString>;
    /// The result of the aggregate call `index` ([`Scope::aggregate`]) for the current group;
    /// `NULL` when the row is not an aggregated one.
    fn aggregate(&self, index: usize) -> Value<Self::Element> {
        let _ = index;
        Value::Null
    }
    /// The element's stored properties in stored order, as stored (a stored `null` included:
    /// callers skip it; compressed values as stored: [`Value::from_property`] reads them).
    /// Default: none.
    fn properties(&self, element: &Self::Element) -> Vec<(CompactString, ElementProperty)> {
        let _ = element;
        Vec::new()
    }
    /// The stored `(source, target)` vertices of an edge; `None` for a vertex. Default: `None`.
    fn endpoints(&self, edge: &Self::Element) -> Option<(Self::Element, Self::Element)> {
        let _ = edge;
        None
    }
    /// Asks for `bytes` more memory for a result a function is about to build (design F6):
    /// `false` refuses it. A row of the traversal checks `traversal.max_materialized_bytes`
    /// and the memory budget and keeps the limit error for its step; the default allows at
    /// most the default of `traversal.max_materialized_bytes`.
    fn reserve(&self, bytes: u64) -> bool {
        bytes <= crate::traversal::context::DEFAULT_MAX_MATERIALIZED_BYTES
    }
    /// The value of subquery `index` ([`Scope::subquery`]) for this row (`EXISTS`: TRUE or
    /// FALSE, never `NULL`), or `None` when the row has not run it yet: the evaluation then
    /// stops with [`EvalError::subquery_pending`], and the step hosting the subquery
    /// (`runtime::SubqueryHost`) runs it and evaluates the row again. The default runs nothing.
    fn subquery(&self, index: usize) -> Option<Value<Self::Element>> {
        let _ = index;
        None
    }
}

/// An aggregate call met by [`compile`] ([`Scope::aggregate`]).
#[derive(Debug, Clone, Copy)]
pub(crate) struct AggregateRef<'a> {
    /// The aggregate function (`count`, `sum`, ...).
    pub(crate) function: Function,
    /// `DISTINCT` before the argument.
    pub(crate) distinct: bool,
    /// The argument; `*` only for `count`.
    pub(crate) args: &'a CallArgs,
    /// The whole call.
    pub(crate) span: Span,
}

/// What [`compile`] resolves names against.
pub(crate) trait Scope {
    /// The slot of the variable `name`, if it is in scope.
    fn variable(&self, name: &str) -> Option<usize>;
    /// The value of the parameter `$name`, if one was given.
    fn parameter(&self, name: &str) -> Option<ElementProperty>;
    /// Where the result of an aggregate call comes from: the index [`Env::aggregate`] reads, an
    /// error, or `None` when no aggregate is allowed here (the default: [`compile`] then fails
    /// with 0A000).
    fn aggregate(&self, call: AggregateRef<'_>) -> Option<Result<usize, EvalError>> {
        let _ = call;
        None
    }
    /// Where the value of the subquery written at `span` comes from: the index
    /// [`Env::subquery`] reads, an error, or `None` when no subquery can run here (the default:
    /// [`compile`] then fails with 0A000).
    fn subquery(&self, span: Span) -> Option<Result<usize, EvalError>> {
        let _ = span;
        None
    }
}

/// A compiled expression, evaluated per row against an [`Env`].
#[derive(Debug, Clone)]
pub(crate) struct Compiled<E> {
    node: Node<E>,
    span: Span,
}

#[derive(Debug, Clone)]
enum Node<E> {
    Const(Value<E>),
    Variable(usize),
    List(Vec<Compiled<E>>),
    Map(Vec<(CompactString, Compiled<E>)>),
    Unary(UnaryOp, Box<Compiled<E>>),
    /// A left spine: `first`, then each link applied to the value so far (`a + b - c` is `a`,
    /// `+ b`, `- c`). Binary operators, property access, `IS [NOT] NULL` and `IS [NOT] LABELED`
    /// are all links, so a long chain is a loop, not a recursion.
    Chain(Box<Compiled<E>>, Vec<Link<E>>),
    Case(Box<Case<E>>),
    Call(Function, Vec<Compiled<E>>),
    /// `CAST(operand AS target)`.
    Cast(CastTarget, Box<Compiled<E>>),
    /// The SQL forms of `TRIM`: side, character, source.
    Trim(TrimSide, Option<Box<Compiled<E>>>, Box<Compiled<E>>),
    /// The result of an aggregate call, read through [`Env::aggregate`].
    Aggregate(usize),
    /// The value of a subquery (`EXISTS`), read through [`Env::subquery`].
    Subquery(usize),
}

/// One link of a [`Node::Chain`].
#[derive(Debug, Clone)]
struct Link<E> {
    op: LinkOp<E>,
    /// The expression this link completes (the value so far, the operator and its operand).
    span: Span,
}

#[derive(Debug, Clone)]
enum LinkOp<E> {
    Binary(BinaryOp, Compiled<E>),
    Property(CompactString),
    IsNull(bool),
    IsLabeled(Labels, bool),
    /// `IS [NOT] SOURCE OF edge` / `IS [NOT] DESTINATION OF edge`.
    IsEndpoint(Endpoint, bool, Compiled<E>),
}

#[derive(Debug, Clone)]
struct Case<E> {
    operand: Option<Compiled<E>>,
    whens: Vec<(Compiled<E>, Compiled<E>)>,
    else_result: Option<Compiled<E>>,
}

/// A compiled label expression.
#[derive(Debug, Clone)]
pub(crate) enum Labels {
    /// A label name.
    Label(CompactString),
    /// `%`.
    Wildcard,
    /// `!x`.
    Not(Box<Labels>),
    /// `a & b`.
    And(Box<Labels>, Box<Labels>),
    /// `a | b`.
    Or(Box<Labels>, Box<Labels>),
}

impl Labels {
    /// Compiles a label expression.
    pub(crate) fn compile(labels: &LabelExpr) -> Self {
        match &labels.kind {
            LabelExprKind::Label(name) => Labels::Label(name.name.as_str().into()),
            LabelExprKind::Wildcard => Labels::Wildcard,
            LabelExprKind::Not(inner) => Labels::Not(Box::new(Labels::compile(inner))),
            LabelExprKind::And(a, b) => {
                Labels::And(Box::new(Labels::compile(a)), Box::new(Labels::compile(b)))
            }
            LabelExprKind::Or(a, b) => {
                Labels::Or(Box::new(Labels::compile(a)), Box::new(Labels::compile(b)))
            }
        }
    }

    /// Whether `element` matches.
    pub(crate) fn matches<En: Env>(&self, env: &En, element: &En::Element) -> bool {
        match self {
            Labels::Label(label) => env.has_label(element, label),
            Labels::Wildcard => env.has_any_label(element),
            Labels::Not(inner) => !inner.matches(env, element),
            Labels::And(a, b) => a.matches(env, element) && b.matches(env, element),
            Labels::Or(a, b) => a.matches(env, element) || b.matches(env, element),
        }
    }
}

/// Compiles `expr`, resolving variables and parameters against `scope`.
///
/// Left-deep operator chains (`a OR b OR ...`, `n.a.b.c`), the only trees taller than the
/// parser's nesting bound (`parser::MAX_DEPTH`), become one `Node::Chain` built without
/// recursion along the spine; everything else recurses, so neither compiling nor evaluating
/// goes deeper than about `MAX_DEPTH` levels, even in unoptimized builds with large frames.
pub(crate) fn compile<E: Clone>(expr: &Expr, scope: &impl Scope) -> Result<Compiled<E>, EvalError> {
    let node = match &expr.kind {
        ExprKind::Binary { .. }
        | ExprKind::Property { .. }
        | ExprKind::IsNull { .. }
        | ExprKind::IsLabeled { .. }
        | ExprKind::IsEndpoint { .. } => compile_chain(expr, scope)?,
        ExprKind::Unary { op, operand } => compile_unary(*op, operand, scope)?,
        _ => compile_other(expr, scope)?,
    };
    Ok(Compiled {
        node,
        span: expr.span,
    })
}

/// The left operand of a chain link, if `expr` is one.
fn chain_left(expr: &Expr) -> Option<&Expr> {
    match &expr.kind {
        ExprKind::Binary { left, .. } => Some(left),
        ExprKind::Property { target, .. } => Some(target),
        ExprKind::IsNull { operand, .. }
        | ExprKind::IsLabeled { operand, .. }
        | ExprKind::IsEndpoint { operand, .. } => Some(operand),
        _ => None,
    }
}

#[inline(never)]
fn compile_chain<E: Clone>(expr: &Expr, scope: &impl Scope) -> Result<Node<E>, EvalError> {
    let mut spine = Vec::new();
    let mut bottom = expr;
    while let Some(left) = chain_left(bottom) {
        spine.push(bottom);
        bottom = left;
    }
    let first = compile(bottom, scope)?;
    let links = spine
        .iter()
        .rev()
        .map(|link| {
            let op = match &link.kind {
                ExprKind::Binary { op, right, .. } => LinkOp::Binary(*op, compile(right, scope)?),
                ExprKind::Property { key, .. } => LinkOp::Property(key.name.as_str().into()),
                ExprKind::IsNull { negated, .. } => LinkOp::IsNull(*negated),
                ExprKind::IsLabeled {
                    labels, negated, ..
                } => LinkOp::IsLabeled(Labels::compile(labels), *negated),
                ExprKind::IsEndpoint {
                    edge, end, negated, ..
                } => LinkOp::IsEndpoint(*end, *negated, compile(edge, scope)?),
                // `chain_left` admits only the kinds above.
                _ => LinkOp::IsNull(false),
            };
            Ok(Link {
                op,
                span: link.span,
            })
        })
        .collect::<Result<Vec<_>, EvalError>>()?;
    Ok(Node::Chain(Box::new(first), links))
}

#[inline(never)]
fn compile_unary<E: Clone>(
    op: UnaryOp,
    operand: &Expr,
    scope: &impl Scope,
) -> Result<Node<E>, EvalError> {
    Ok(Node::Unary(op, Box::new(compile(operand, scope)?)))
}

#[inline(never)]
fn compile_other<E: Clone>(expr: &Expr, scope: &impl Scope) -> Result<Node<E>, EvalError> {
    let span = expr.span;
    Ok(match &expr.kind {
        ExprKind::Literal(literal) => Node::Const(literal_value(literal)),
        ExprKind::Parameter(name) => match scope.parameter(name) {
            Some(value) => Node::Const(Value::from_property(value)),
            None => {
                return Err(EvalError::new(
                    status::SEMANTIC,
                    format!("no value was given for the parameter ${name}"),
                    span,
                ));
            }
        },
        ExprKind::Variable(name) => match scope.variable(&name.name) {
            Some(slot) => Node::Variable(slot),
            None => {
                return Err(EvalError::new(
                    status::SEMANTIC,
                    format!("the variable {} is not defined", name.name),
                    span,
                ));
            }
        },
        ExprKind::List(items) => compile_list(items, scope)?,
        ExprKind::Map(map) => compile_map(map, scope)?,
        ExprKind::Case(case) => compile_case(case, scope)?,
        ExprKind::FunctionCall {
            name,
            args,
            distinct,
        } => compile_call(name.name.as_str(), args, *distinct, scope, span)?,
        ExprKind::Binary { .. }
        | ExprKind::Property { .. }
        | ExprKind::IsNull { .. }
        | ExprKind::IsLabeled { .. }
        | ExprKind::IsEndpoint { .. } => compile_chain(expr, scope)?,
        ExprKind::Unary { op, operand } => compile_unary(*op, operand, scope)?,
        // Answered by the step hosting the expression (`runtime::SubqueryHost`); a scope
        // without hosts (the lookup value, which the planner never gives a subquery) refuses it.
        ExprKind::Subquery(sub) => match scope.subquery(sub.span) {
            Some(Ok(index)) => Node::Subquery(index),
            Some(Err(error)) => return Err(error),
            None => {
                return Err(EvalError::new(
                    status::NOT_SUPPORTED,
                    "a subquery (EXISTS) is not yet supported in this expression",
                    span,
                ));
            }
        },
    })
}

#[inline(never)]
fn compile_list<E: Clone>(items: &[Expr], scope: &impl Scope) -> Result<Node<E>, EvalError> {
    let items = items
        .iter()
        .map(|item| compile(item, scope))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(if items.iter().all(Compiled::is_const) {
        Node::Const(Value::List(
            items.into_iter().map(Compiled::into_const).collect(),
        ))
    } else {
        Node::List(items)
    })
}

#[inline(never)]
fn compile_map<E: Clone>(map: &MapLiteral, scope: &impl Scope) -> Result<Node<E>, EvalError> {
    let mut entries: Vec<(CompactString, Compiled<E>)> = Vec::with_capacity(map.entries.len());
    for entry in &map.entries {
        let value = compile(&entry.value, scope)?;
        // A repeated key keeps its first position and its last value.
        match entries
            .iter_mut()
            .find(|(key, _)| key.as_str() == entry.key.name)
        {
            Some(slot) => slot.1 = value,
            None => entries.push((entry.key.name.as_str().into(), value)),
        }
    }
    Ok(if entries.iter().all(|(_, value)| value.is_const()) {
        Node::Const(Value::Map(
            entries
                .into_iter()
                .map(|(key, value)| (key, value.into_const()))
                .collect(),
        ))
    } else {
        Node::Map(entries)
    })
}

#[inline(never)]
fn compile_case<E: Clone>(case: &CaseExpr, scope: &impl Scope) -> Result<Node<E>, EvalError> {
    let operand = case
        .operand
        .as_ref()
        .map(|operand| compile(operand, scope))
        .transpose()?;
    let whens = case
        .whens
        .iter()
        .map(|arm| Ok((compile(&arm.when, scope)?, compile(&arm.then, scope)?)))
        .collect::<Result<Vec<_>, EvalError>>()?;
    let else_result = case
        .else_result
        .as_ref()
        .map(|result| compile(result, scope))
        .transpose()?;
    Ok(Node::Case(Box::new(Case {
        operand,
        whens,
        else_result,
    })))
}

fn compile_call<E: Clone>(
    name: &str,
    args: &CallArgs,
    distinct: bool,
    scope: &impl Scope,
    span: Span,
) -> Result<Node<E>, EvalError> {
    let Some(spec) = lookup_function(name) else {
        return Err(EvalError::new(
            status::SEMANTIC,
            format!("unknown function {name}()"),
            span,
        ));
    };
    if spec.kind == FunctionKind::Aggregate {
        return compile_aggregate_call(spec, args, distinct, scope, span);
    }
    let exprs = match args {
        CallArgs::Exprs(exprs) if spec.function != Function::Cast => exprs,
        CallArgs::Cast {
            operand, target, ..
        } if spec.function == Function::Cast => {
            return Ok(Node::Cast(*target, Box::new(compile(operand, scope)?)));
        }
        CallArgs::Trim {
            side,
            character,
            source,
        } if spec.function == Function::Trim => {
            let character = match character {
                Some(character) => Some(Box::new(compile(character, scope)?)),
                None => None,
            };
            return Ok(Node::Trim(
                *side,
                character,
                Box::new(compile(source, scope)?),
            ));
        }
        CallArgs::Star(_) => {
            return Err(EvalError::new(
                status::SEMANTIC,
                format!("{}(*) is not a function call", spec.name()),
                span,
            ));
        }
        CallArgs::Exprs(_) | CallArgs::Cast { .. } | CallArgs::Trim { .. } => {
            return Err(EvalError::new(
                status::SEMANTIC,
                format!("{}() is not called with its own argument form", spec.name()),
                span,
            ));
        }
    };
    if exprs.len() < spec.min_args || exprs.len() > spec.max_args {
        return Err(EvalError::new(
            status::SEMANTIC,
            format!("{}() does not take {} arguments", spec.name(), exprs.len()),
            span,
        ));
    }
    let args = exprs
        .iter()
        .map(|arg| compile(arg, scope))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Node::Call(spec.function, args))
}

#[inline(never)]
fn compile_aggregate_call<E>(
    spec: &FunctionSpec,
    args: &CallArgs,
    distinct: bool,
    scope: &impl Scope,
    span: Span,
) -> Result<Node<E>, EvalError> {
    let not_here = || {
        EvalError::new(
            status::NOT_SUPPORTED,
            format!(
                "the aggregate function {}() is not supported here",
                spec.name()
            ),
            span,
        )
    };
    match args {
        CallArgs::Star(_) if spec.function != Function::Count => {
            return Err(EvalError::new(
                status::SEMANTIC,
                format!("only count(*) takes `*`, not {}()", spec.name()),
                span,
            ));
        }
        CallArgs::Star(_) => {}
        CallArgs::Exprs(exprs) if exprs.len() < spec.min_args || exprs.len() > spec.max_args => {
            return Err(EvalError::new(
                status::SEMANTIC,
                format!("{}() does not take {} arguments", spec.name(), exprs.len()),
                span,
            ));
        }
        CallArgs::Exprs(_) => {}
        CallArgs::Cast { .. } | CallArgs::Trim { .. } => {
            return Err(EvalError::new(
                status::SEMANTIC,
                format!("{}() takes one expression", spec.name()),
                span,
            ));
        }
    }
    let call = AggregateRef {
        function: spec.function,
        distinct,
        args,
        span,
    };
    match scope.aggregate(call) {
        Some(index) => Ok(Node::Aggregate(index?)),
        None => Err(not_here()),
    }
}

fn literal_value<E>(literal: &Literal) -> Value<E> {
    match literal {
        Literal::Null => Value::Null,
        Literal::Bool(flag) => Value::Scalar(ElementProperty::Boolean(*flag)),
        Literal::Integer(value) => Value::integer(*value),
        Literal::Float(value) => Value::Scalar(ElementProperty::Number(Number::from_f64(*value))),
        Literal::String(text) => Value::string(text.as_str()),
    }
}

impl<E> Compiled<E> {
    /// Whether it is a constant.
    pub(crate) fn is_const(&self) -> bool {
        matches!(self.node, Node::Const(_))
    }

    /// The constant, when it is one.
    pub(crate) fn as_const(&self) -> Option<&Value<E>> {
        match &self.node {
            Node::Const(value) => Some(value),
            _ => None,
        }
    }

    fn into_const(self) -> Value<E> {
        match self.node {
            Node::Const(value) => value,
            _ => Value::Null,
        }
    }

    /// The source span.
    pub(crate) fn span(&self) -> Span {
        self.span
    }

    /// The slot, when the expression is exactly one variable (`RETURN p`): the internal steps
    /// then pass the bound value through as it is, with no conversion.
    pub(crate) fn as_variable(&self) -> Option<usize> {
        match self.node {
            Node::Variable(slot) => Some(slot),
            _ => None,
        }
    }

    /// Adds every slot the expression reads to `slots` (repeats included): what a step running
    /// it declares to the path analysis. Recursive like `eval`, so at most about
    /// `parser::MAX_DEPTH` levels deep.
    pub(crate) fn variables(&self, slots: &mut Vec<usize>) {
        match &self.node {
            // An aggregate's argument is compiled apart (`aggregate::AggregateCall`).
            // A subquery's body declares its own reads (the host's child traversal).
            Node::Const(_) | Node::Aggregate(_) | Node::Subquery(_) => {}
            Node::Variable(slot) => slots.push(*slot),
            Node::List(items) | Node::Call(_, items) => {
                items.iter().for_each(|item| item.variables(slots));
            }
            Node::Map(entries) => entries.iter().for_each(|(_, value)| value.variables(slots)),
            Node::Unary(_, operand) | Node::Cast(_, operand) => operand.variables(slots),
            Node::Trim(_, character, source) => {
                if let Some(character) = character {
                    character.variables(slots);
                }
                source.variables(slots);
            }
            Node::Chain(first, links) => {
                first.variables(slots);
                for link in links {
                    match &link.op {
                        LinkOp::Binary(_, right) | LinkOp::IsEndpoint(_, _, right) => {
                            right.variables(slots)
                        }
                        LinkOp::Property(_) | LinkOp::IsNull(_) | LinkOp::IsLabeled(_, _) => {}
                    }
                }
            }
            Node::Case(case) => {
                if let Some(operand) = &case.operand {
                    operand.variables(slots);
                }
                for (when, then) in &case.whens {
                    when.variables(slots);
                    then.variables(slots);
                }
                if let Some(result) = &case.else_result {
                    result.variables(slots);
                }
            }
        }
    }
}

impl<E: Clone + PartialEq> Compiled<E> {
    /// The value of the expression for the row of `env`.
    pub(crate) fn eval<En: Env<Element = E>>(&self, env: &En) -> Result<Value<E>, EvalError> {
        self.eval_cow(env).map(Cow::into_owned)
    }

    /// The truth value of a condition (`WHERE`): a boolean or `NULL`; anything else is error
    /// 22G03.
    pub(crate) fn truth<En: Env<Element = E>>(&self, env: &En) -> Result<Truth, EvalError> {
        match &self.node {
            Node::Unary(UnaryOp::Not, operand) => Ok(operand.truth(env)?.not()),
            _ => self.truth_of_value(env),
        }
    }

    #[inline(never)]
    fn truth_of_value<En: Env<Element = E>>(&self, env: &En) -> Result<Truth, EvalError> {
        truth_of(&*self.eval_cow(env)?, self.span)
    }

    // Recursive over the tree minus its left spines (see `Node::Chain`), so at most about
    // `parser::MAX_DEPTH` levels deep; the arms live in functions of their own.
    fn eval_cow<'a, En: Env<Element = E>>(
        &'a self,
        env: &En,
    ) -> Result<Cow<'a, Value<E>>, EvalError> {
        match &self.node {
            Node::Const(value) => Ok(Cow::Borrowed(value)),
            Node::Chain(first, links) => eval_chain(first, links, env),
            Node::Case(case) => case.eval(env),
            _ => self.eval_other(env).map(Cow::Owned),
        }
    }

    /// The value as a function argument, with its span.
    fn arg<En: Env<Element = E>>(&self, env: &En) -> Result<Arg<E>, EvalError> {
        Ok(Arg {
            value: self.eval(env)?,
            span: self.span,
        })
    }

    #[inline(never)]
    fn eval_other<En: Env<Element = E>>(&self, env: &En) -> Result<Value<E>, EvalError> {
        Ok(match &self.node {
            Node::Variable(slot) => env.variable(*slot),
            Node::List(items) => Value::List(
                items
                    .iter()
                    .map(|item| item.eval(env))
                    .collect::<Result<_, _>>()?,
            ),
            Node::Map(entries) => Value::Map(
                entries
                    .iter()
                    .map(|(key, value)| Ok((key.clone(), value.eval(env)?)))
                    .collect::<Result<_, EvalError>>()?,
            ),
            Node::Unary(UnaryOp::Not, operand) => operand.truth(env)?.not().into_value(),
            Node::Unary(op, operand) => unary(*op, &*operand.eval_cow(env)?, self.span)?,
            Node::Call(function, args) => call(*function, args, env, self.span)?,
            Node::Cast(target, operand) => {
                functions::cast(*target, operand.arg(env)?, env, self.span)?
            }
            Node::Trim(side, character, source) => {
                let character = match character {
                    Some(character) => Some(character.arg(env)?),
                    None => None,
                };
                functions::trim(*side, character, source.arg(env)?, env, self.span)?
            }
            Node::Aggregate(index) => env.aggregate(*index),
            Node::Subquery(index) => match env.subquery(*index) {
                Some(value) => value,
                None => return Err(EvalError::subquery_pending(self.span)),
            },
            Node::Const(value) => value.clone(),
            Node::Case(case) => case.eval(env)?.into_owned(),
            Node::Chain(first, links) => eval_chain(first, links, env)?.into_owned(),
        })
    }
}

/// Evaluates a left spine bottom-up, without recursion along it.
#[inline(never)]
fn eval_chain<'a, En: Env>(
    first: &'a Compiled<En::Element>,
    links: &'a [Link<En::Element>],
    env: &En,
) -> Result<Cow<'a, Value<En::Element>>, EvalError> {
    let mut value = first.eval_cow(env)?;
    let mut left_span = first.span;
    for link in links {
        let next = match &link.op {
            LinkOp::Binary(op @ (BinaryOp::And | BinaryOp::Or | BinaryOp::Xor), right) => {
                logical(*op, truth_of(&value, left_span)?, right, env)?.into_value()
            }
            LinkOp::Binary(op, right) => {
                let right = right.eval_cow(env)?;
                if matches!(op, BinaryOp::Concat | BinaryOp::Add) {
                    reserve_concat(&value, &right, env, link.span)?;
                }
                binary(*op, &value, &right, link.span)?
            }
            LinkOp::Property(key) => property(&value, key, env, link.span)?,
            LinkOp::IsNull(negated) => Truth::of(value.is_null() != *negated).into_value(),
            LinkOp::IsLabeled(labels, negated) => {
                is_labeled(&value, labels, *negated, env, link.span)?
            }
            LinkOp::IsEndpoint(end, negated, edge) => {
                let edge = edge.eval_cow(env)?;
                functions::is_endpoint(&value, &edge, *end, *negated, env, link.span)?
            }
        };
        value = Cow::Owned(next);
        left_span = link.span;
    }
    Ok(value)
}

/// Design F6: `||` (and Cypher `+`) of two strings or two lists asks the row for the result's
/// size before building it ([`Env::reserve`]).
fn reserve_concat<En: Env>(
    left: &Value<En::Element>,
    right: &Value<En::Element>,
    env: &En,
    span: Span,
) -> Result<(), EvalError> {
    let bytes = match (left, right) {
        (Value::List(a), Value::List(b)) => {
            functions::list_bytes((a.len() as u64).saturating_add(b.len() as u64))
        }
        _ => match (left.as_str(), right.as_str()) {
            (Some(a), Some(b)) => (a.len() as u64).saturating_add(b.len() as u64),
            _ => return Ok(()),
        },
    };
    functions::reserve(env, bytes, span)
}

/// `left op right` for `AND` / `OR` / `XOR`; the right side is skipped once `left` decides.
#[inline(never)]
fn logical<En: Env>(
    op: BinaryOp,
    left: Truth,
    right: &Compiled<En::Element>,
    env: &En,
) -> Result<Truth, EvalError> {
    Ok(match op {
        BinaryOp::And if left == Truth::False => Truth::False,
        BinaryOp::And => left.and(right.truth(env)?),
        BinaryOp::Or if left == Truth::True => Truth::True,
        BinaryOp::Or => left.or(right.truth(env)?),
        _ => left.xor(right.truth(env)?),
    })
}

#[inline(never)]
fn is_labeled<En: Env>(
    operand: &Value<En::Element>,
    labels: &Labels,
    negated: bool,
    env: &En,
    span: Span,
) -> Result<Value<En::Element>, EvalError> {
    match operand {
        Value::Null => Ok(Value::Null),
        Value::Element(element) => {
            Ok(Truth::of(labels.matches(env, element) != negated).into_value())
        }
        other => Err(EvalError::invalid_type(
            format!(
                "IS LABELED tests a node or an edge, not {}",
                other.kind_name()
            ),
            span,
        )),
    }
}

impl<E: Clone + PartialEq> Case<E> {
    #[inline(never)]
    fn eval<'a, En: Env<Element = E>>(&'a self, env: &En) -> Result<Cow<'a, Value<E>>, EvalError> {
        match &self.operand {
            Some(operand) => {
                let operand = operand.eval_cow(env)?;
                for (when, then) in &self.whens {
                    if equals(&operand, &*when.eval_cow(env)?) == Truth::True {
                        return then.eval_cow(env);
                    }
                }
            }
            None => {
                for (when, then) in &self.whens {
                    if when.truth(env)? == Truth::True {
                        return then.eval_cow(env);
                    }
                }
            }
        }
        match &self.else_result {
            Some(result) => result.eval_cow(env),
            None => Ok(Cow::Owned(Value::Null)),
        }
    }
}

fn truth_of<E>(value: &Value<E>, span: Span) -> Result<Truth, EvalError> {
    match value {
        Value::Null => Ok(Truth::Unknown),
        Value::Scalar(ElementProperty::Boolean(flag)) => Ok(Truth::of(*flag)),
        other => Err(EvalError::invalid_type(
            format!("a condition must be a boolean, not {}", other.kind_name()),
            span,
        )),
    }
}

#[inline(never)]
fn property<En: Env>(
    target: &Value<En::Element>,
    key: &str,
    env: &En,
    span: Span,
) -> Result<Value<En::Element>, EvalError> {
    match target {
        Value::Null => Ok(Value::Null),
        Value::Element(element) => Ok(env
            .property(element, key)
            .map(Value::from_property)
            .unwrap_or(Value::Null)),
        Value::Map(entries) => Ok(entries
            .iter()
            .find(|(name, _)| name.as_str() == key)
            .map(|(_, value)| value.clone())
            .unwrap_or(Value::Null)),
        other => Err(EvalError::invalid_type(
            format!("cannot read the property {key} of {}", other.kind_name()),
            span,
        )),
    }
}

/// Three-valued `a = b` (see the module docs).
pub(crate) fn equals<E: PartialEq>(a: &Value<E>, b: &Value<E>) -> Truth {
    match (a, b) {
        (Value::Null, _) | (_, Value::Null) => Truth::Unknown,
        (Value::Scalar(a), Value::Scalar(b)) => match compare_scalars(a, b) {
            Some(order) => Truth::of(order == Ordering::Equal),
            None => match (a, b) {
                (ElementProperty::Vector(_), ElementProperty::Vector(_))
                | (ElementProperty::Map(_), ElementProperty::Map(_))
                | (ElementProperty::Vertex(_), ElementProperty::Vertex(_))
                | (ElementProperty::Edge(_), ElementProperty::Edge(_)) => Truth::of(a == b),
                _ => Truth::Unknown,
            },
        },
        (Value::List(a), Value::List(b)) => {
            if a.len() != b.len() {
                return Truth::False;
            }
            a.iter()
                .zip(b)
                .fold(Truth::True, |acc, (a, b)| acc.and(equals(a, b)))
        }
        (Value::Map(a), Value::Map(b)) => {
            if a.len() != b.len() {
                return Truth::False;
            }
            let mut result = Truth::True;
            for (key, value) in a {
                match b.iter().find(|(other, _)| other == key) {
                    Some((_, other)) => result = result.and(equals(value, other)),
                    None => return Truth::False,
                }
            }
            result
        }
        (Value::Element(a), Value::Element(b)) => Truth::of(a == b),
        (Value::Path(a), Value::Path(b)) => Truth::of(a == b),
        _ => Truth::Unknown,
    }
}

/// The order of two values for `< <= > >=`: `None` when incomparable or `NULL`.
fn order<E>(a: &Value<E>, b: &Value<E>) -> Option<Ordering> {
    match (a, b) {
        (Value::Scalar(a), Value::Scalar(b)) => compare_scalars(a, b),
        _ => None,
    }
}

#[inline(never)]
fn unary<E>(op: UnaryOp, operand: &Value<E>, span: Span) -> Result<Value<E>, EvalError> {
    match (operand, op) {
        (Value::Null, _) => Ok(Value::Null),
        (Value::Scalar(ElementProperty::Number(number)), UnaryOp::Plus) => {
            Ok(Value::Scalar(ElementProperty::Number(number.clone())))
        }
        (Value::Scalar(ElementProperty::Number(number)), UnaryOp::Minus) => match &number.n {
            N::NegInt(value) => value
                .checked_neg()
                .map(Value::integer)
                .ok_or_else(|| EvalError::out_of_range(span)),
            N::Float(value) => Ok(Value::Scalar(ElementProperty::Number(Number::from_f64(
                -value.0,
            )))),
        },
        (other, UnaryOp::Not) => Ok(truth_of(other, span)?.not().into_value()),
        (other, _) => Err(EvalError::invalid_type(
            format!("a sign applies to a number, not {}", other.kind_name()),
            span,
        )),
    }
}

#[inline(never)]
fn binary<E: Clone + PartialEq>(
    op: BinaryOp,
    left: &Value<E>,
    right: &Value<E>,
    span: Span,
) -> Result<Value<E>, EvalError> {
    let compared = |test: fn(Ordering) -> bool| match order(left, right) {
        Some(order) => Truth::of(test(order)).into_value(),
        None => Value::Null,
    };
    Ok(match op {
        BinaryOp::And => truth_of(left, span)?
            .and(truth_of(right, span)?)
            .into_value(),
        BinaryOp::Or => truth_of(left, span)?
            .or(truth_of(right, span)?)
            .into_value(),
        BinaryOp::Xor => truth_of(left, span)?
            .xor(truth_of(right, span)?)
            .into_value(),
        BinaryOp::Eq => equals(left, right).into_value(),
        BinaryOp::Ne => equals(left, right).not().into_value(),
        BinaryOp::Lt => compared(Ordering::is_lt),
        BinaryOp::Le => compared(Ordering::is_le),
        BinaryOp::Gt => compared(Ordering::is_gt),
        BinaryOp::Ge => compared(Ordering::is_ge),
        BinaryOp::In => contains(right, left).into_value(),
        BinaryOp::StartsWith => text_test(left, right, |a, b| a.starts_with(b)),
        BinaryOp::EndsWith => text_test(left, right, |a, b| a.ends_with(b)),
        BinaryOp::Contains => text_test(left, right, |a, b| a.contains(b)),
        BinaryOp::Concat => concat(left, right, "||", span)?,
        BinaryOp::Add => match (left, right) {
            (
                Value::Scalar(ElementProperty::Number(a)),
                Value::Scalar(ElementProperty::Number(b)),
            ) => arithmetic(op, a, b, span)?,
            (
                Value::Scalar(ElementProperty::String(_)),
                Value::Scalar(ElementProperty::String(_)),
            )
            | (Value::List(_), Value::List(_))
            | (Value::Null, _)
            | (_, Value::Null) => concat(left, right, "+", span)?,
            _ => return Err(add_error(left, right, span)),
        },
        BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => match (left, right) {
            (Value::Null, _) | (_, Value::Null) => Value::Null,
            (
                Value::Scalar(ElementProperty::Number(a)),
                Value::Scalar(ElementProperty::Number(b)),
            ) => arithmetic(op, a, b, span)?,
            _ => {
                return Err(EvalError::invalid_type(
                    format!(
                        "arithmetic needs two numbers, not {} and {}",
                        left.kind_name(),
                        right.kind_name()
                    ),
                    span,
                ));
            }
        },
    })
}

#[cold]
fn add_error<E>(left: &Value<E>, right: &Value<E>, span: Span) -> EvalError {
    let text = matches!(left, Value::Scalar(ElementProperty::String(_)))
        || matches!(right, Value::Scalar(ElementProperty::String(_)));
    if text {
        let message = format!(
            "+ joins two strings or adds two numbers, not {} and {}; a number is never turned \
             into text implicitly",
            left.kind_name(),
            right.kind_name()
        );
        // The help names toString(x) / CAST(x AS STRING).
        return EvalError::invalid_type(message, span).with_topic(GqlHelpTopic::TextAndNumber);
    }
    let message = format!(
        "+ adds two numbers or joins two strings or two lists, not {} and {}",
        left.kind_name(),
        right.kind_name()
    );
    EvalError::invalid_type(message, span)
}

/// Text joined with a number or a boolean: the help names toString(x) / CAST(x AS STRING).
fn text_and_scalar<E>(left: &Value<E>, right: &Value<E>) -> bool {
    let convertible = |value: &Value<E>| {
        matches!(
            value,
            Value::Scalar(ElementProperty::Number(_) | ElementProperty::Boolean(_))
        )
    };
    (left.as_str().is_some() && convertible(right))
        || (right.as_str().is_some() && convertible(left))
}

/// `needle IN haystack` (see the module docs).
fn contains<E: PartialEq>(haystack: &Value<E>, needle: &Value<E>) -> Truth {
    let Value::List(items) = haystack else {
        return Truth::Unknown;
    };
    let mut result = Truth::False;
    for item in items {
        match equals(needle, item) {
            Truth::True => return Truth::True,
            Truth::Unknown => result = Truth::Unknown,
            Truth::False => {}
        }
    }
    result
}

fn text_test<E>(left: &Value<E>, right: &Value<E>, test: fn(&str, &str) -> bool) -> Value<E> {
    match (left.as_str(), right.as_str()) {
        (Some(a), Some(b)) => Truth::of(test(a, b)).into_value(),
        _ => Value::Null,
    }
}

fn concat<E: Clone>(
    left: &Value<E>,
    right: &Value<E>,
    operator: &str,
    span: Span,
) -> Result<Value<E>, EvalError> {
    match (left, right) {
        (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
        (Value::List(a), Value::List(b)) => {
            let mut items = Vec::with_capacity(a.len() + b.len());
            items.extend(a.iter().cloned());
            items.extend(b.iter().cloned());
            Ok(Value::List(items))
        }
        _ => match (left.as_str(), right.as_str()) {
            (Some(a), Some(b)) => {
                let mut text = CompactString::with_capacity(a.len() + b.len());
                text.push_str(a);
                text.push_str(b);
                Ok(Value::string(text))
            }
            _ => {
                let error = EvalError::invalid_type(
                    format!(
                        "{operator} joins two strings or two lists, not {} and {}",
                        left.kind_name(),
                        right.kind_name()
                    ),
                    span,
                );
                Err(if text_and_scalar(left, right) {
                    error.with_topic(GqlHelpTopic::TextAndNumber)
                } else {
                    error
                })
            }
        },
    }
}

fn arithmetic<E>(op: BinaryOp, a: &Number, b: &Number, span: Span) -> Result<Value<E>, EvalError> {
    match (&a.n, &b.n) {
        (N::NegInt(a), N::NegInt(b)) => {
            let (a, b) = (*a, *b);
            let result = match op {
                BinaryOp::Add => a.checked_add(b),
                BinaryOp::Sub => a.checked_sub(b),
                BinaryOp::Mul => a.checked_mul(b),
                BinaryOp::Div if b == 0 => return Err(EvalError::division_by_zero(span)),
                BinaryOp::Div => a.checked_div(b),
                BinaryOp::Mod if b == 0 => return Err(EvalError::division_by_zero(span)),
                // `i64::MIN % -1` is 0 mathematically; only `checked_rem` reports it as overflow.
                BinaryOp::Mod => Some(a.checked_rem(b).unwrap_or(0)),
                _ => None,
            };
            result
                .map(Value::integer)
                .ok_or_else(|| EvalError::out_of_range(span))
        }
        _ => {
            let (a, b) = (float(a), float(b));
            let result = match op {
                BinaryOp::Add => a + b,
                BinaryOp::Sub => a - b,
                BinaryOp::Mul => a * b,
                BinaryOp::Div if b == 0.0 => return Err(EvalError::division_by_zero(span)),
                BinaryOp::Div => a / b,
                BinaryOp::Mod if b == 0.0 => return Err(EvalError::division_by_zero(span)),
                BinaryOp::Mod => a % b,
                _ => f64::NAN,
            };
            if result.is_infinite() && a.is_finite() && b.is_finite() {
                return Err(EvalError::out_of_range(span));
            }
            Ok(Value::Scalar(ElementProperty::Number(Number::from_f64(
                result,
            ))))
        }
    }
}

/// A number as a float.
pub(crate) fn float(number: &Number) -> f64 {
    match &number.n {
        N::NegInt(value) => *value as f64,
        N::Float(value) => value.0,
    }
}

#[inline(never)]
fn call<En: Env>(
    function: Function,
    args: &[Compiled<En::Element>],
    env: &En,
    span: Span,
) -> Result<Value<En::Element>, EvalError> {
    if function == Function::Coalesce {
        for arg in args {
            let value = arg.eval_cow(env)?;
            if !value.is_null() {
                return Ok(value.into_owned());
            }
        }
        return Ok(Value::Null);
    }
    if !matches!(
        functions::family(function),
        Family::Core | Family::Aggregate
    ) {
        // Stage 1c (`gql/functions/`): every argument, left to right.
        let args = args
            .iter()
            .map(|arg| arg.arg(env))
            .collect::<Result<Vec<_>, _>>()?;
        return functions::call(function, args, env, span);
    }
    let Some(arg) = args.first() else {
        return Ok(Value::Null);
    };
    let value = arg.eval_cow(env)?;
    let wrong = |what: &str| {
        Err(EvalError::invalid_type(
            format!(
                "{}() takes {what}, not {}",
                function_name(function),
                value.kind_name()
            ),
            span,
        ))
    };
    if value.is_null() {
        return Ok(Value::Null);
    }
    match function {
        Function::ElementId => match &*value {
            Value::Element(element) => Ok(Value::string(env.element_id(element))),
            _ => wrong("a node or an edge"),
        },
        Function::Labels => match &*value {
            Value::Element(element) => Ok(Value::List(
                env.labels(element).into_iter().map(Value::string).collect(),
            )),
            _ => wrong("a node or an edge"),
        },
        Function::Type => match &*value {
            Value::Element(element) if env.kind(element) == ElementKind::Edge => Ok(env
                .labels(element)
                .into_iter()
                .next()
                .map(Value::string)
                .unwrap_or(Value::Null)),
            Value::Element(_) => Err(EvalError::invalid_type(
                "type() takes an edge, not a node; use labels(v) for a node's labels",
                span,
            )),
            _ => wrong("an edge"),
        },
        Function::Upper | Function::Lower => match value.as_str() {
            Some(text) if function == Function::Upper => Ok(Value::string(text.to_uppercase())),
            Some(text) => Ok(Value::string(text.to_lowercase())),
            None => wrong("a string"),
        },
        Function::CharLength => match value.as_str() {
            Some(text) => Ok(length(text.chars().count(), span)?),
            None => wrong("a string"),
        },
        Function::Size | Function::Cardinality => match &*value {
            Value::List(items) => length(items.len(), span),
            other => match other.as_str() {
                Some(text) => length(text.chars().count(), span),
                None => wrong("a string or a list"),
            },
        },
        Function::PathLength => match &*value {
            // Vertices at the even positions, edges at the odd ones.
            Value::Path(elements) => length(elements.len() / 2, span),
            _ => wrong("a path"),
        },
        // Handled above: it evaluates its arguments lazily.
        Function::Coalesce => Ok(Value::Null),
        // Handled above (`gql/functions/`).
        Function::Cast
        | Function::ToInteger
        | Function::ToIntegerOrNull
        | Function::ToFloat
        | Function::ToFloatOrNull
        | Function::ToBoolean
        | Function::ToBooleanOrNull
        | Function::ToString
        | Function::ToStringOrNull
        | Function::Left
        | Function::Right
        | Function::Substring
        | Function::Trim
        | Function::Btrim
        | Function::Ltrim
        | Function::Rtrim
        | Function::Replace
        | Function::Split
        | Function::Reverse
        | Function::ByteLength
        | Function::IsEmpty
        | Function::NullIf
        | Function::Abs
        | Function::Floor
        | Function::Ceil
        | Function::Round
        | Function::Sign
        | Function::Sqrt
        | Function::Exp
        | Function::Ln
        | Function::Log
        | Function::Log10
        | Function::Power
        | Function::Mod
        | Function::Head
        | Function::Last
        | Function::Tail
        | Function::Range
        | Function::Keys
        | Function::Properties
        | Function::StartNode
        | Function::EndNode
        | Function::Nodes
        | Function::Relationships => functions::call(
            function,
            vec![Arg {
                value: value.into_owned(),
                span: arg.span,
            }],
            env,
            span,
        ),
        Function::Count
        | Function::Sum
        | Function::Avg
        | Function::Min
        | Function::Max
        | Function::CollectList => Err(EvalError::new(
            status::NOT_SUPPORTED,
            format!(
                "the aggregate function {}() is not supported here",
                function_name(function)
            ),
            span,
        )),
    }
}

fn length<E>(count: usize, span: Span) -> Result<Value<E>, EvalError> {
    i64::try_from(count)
        .map(Value::integer)
        .map_err(|_| EvalError::out_of_range(span))
}

/// The canonical name of `function` (its first spelling in `FUNCTIONS`).
pub(crate) fn function_name(function: Function) -> &'static str {
    super::parser::FUNCTIONS
        .iter()
        .find(|spec| spec.function == function)
        .map(|spec| spec.name())
        .unwrap_or("function")
}
