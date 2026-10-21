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
//!   float, strings, booleans, uuids, one temporal type. Any `NULL` operand gives `NULL`, never
//!   an error. **`=` / `<>` of two non-`NULL` values of different kinds** give FALSE / TRUE
//!   (`1 = '1'` is FALSE, `[1] = 1` is FALSE; stage 3f, the openCypher TCK, where ISO GQL calls
//!   them not comparable); `=` / `<>` also compare lists (same length, element-wise,
//!   three-valued: `[1, 2] = [null, 'foo']` is FALSE, `[1, 2] = [null, 2]` is `NULL`), maps
//!   (same keys, value-wise) and elements (the same vertex or edge); a vertex never equals an
//!   edge. **The ordering operators** `< <= > >=` give `NULL` across kinds (`1 < 'a'`) and order
//!   lists lexicographically, three-valued ([`order`]): the first pair of items that is not
//!   equal decides (`[1, 2] < [1, 3]` TRUE, `[1, 2] >= [3, null]` FALSE), a `NULL` or
//!   incomparable pair reached first gives `NULL` (`[1, 2] >= [1, null]`), a prefix sorts first
//!   (`[1, null] >= [1]` TRUE). Maps, elements and paths are not ordered (`NULL`).
//! - **`NaN`** (IEEE 754, the openCypher TCK): `NaN = x` is FALSE and `NaN <> x` TRUE for every
//!   non-`NULL` `x`, `NaN` itself included; `NaN < 1`, `NaN >= NaN` and the other orderings
//!   against a number are FALSE; against another kind they are `NULL` (`NaN > 'a'`).
//! - **`IN`:** TRUE when an item equals the value; else `NULL` when some comparison was `NULL`
//!   (`2 IN [1, NULL]`, `NULL IN [1]`); else FALSE (`NULL IN []` is FALSE, `1 IN ['1']` is FALSE).
//!   A right operand that is `NULL` or not a list gives `NULL`.
//! - **`STARTS WITH` / `ENDS WITH` / `CONTAINS`:** strings only; a `NULL` or non-string operand
//!   gives `NULL`.
//! - **Power** `x ^ y` (Cypher, stage 3f): always a float, left-associative (`2 ^ 3 ^ 2` is
//!   `64.0`), tighter than `* / %` and looser than a unary minus (`-3 ^ 2` is `9.0`); the rules
//!   of `power(x, y)` (2201F, 22003), `NULL` propagates, another kind is 22G03.
//! - **Arithmetic** `+ - * / %`: integer with integer stays integer (division truncates toward
//!   zero, `%` takes the dividend's sign); any float operand makes it float. Overflow is error
//!   22003 (numeric value out of range), never a wrap; a float result that is not finite from
//!   finite operands is 22003 too. Integer division or `%` by zero is error 22012; a float one
//!   follows IEEE 754 (stage 3f, the openCypher TCK, where ISO GQL raises 22012 for every
//!   numeric type): `1.0 / 0.0` is `Infinity`, `-1 / 0.0` `-Infinity`, `0.0 / 0.0` and
//!   `1.0 % 0.0` `NaN`.
//!   `NULL` propagates. Any other operand kind is error 22G03.
//! - **Concatenation:** `||` joins two strings or two lists. Cypher `+` is its alias on two
//!   strings and two lists; `+` of a list and one value of another kind appends it (`[1] + 2`
//!   is `[1, 2]`) or prepends it (`2 + [1]` is `[2, 1]`; stage 3f, the openCypher TCK), a
//!   `NULL` operand still gives `NULL`; `||` of a list and a non-list stays error 22G03.
//!   **`+` of a string and a number is refused** (22G03): there is no
//!   implicit number-to-text conversion (Neo4j converts; Graphersal never turns values into text
//!   behind the user's back).
//! - **`CASE`:** simple `CASE` matches a `WHEN` value only when `=` is TRUE, so `NULL` never
//!   matches; searched `CASE` takes the first `WHEN` that is TRUE; no match and no `ELSE` gives
//!   `NULL`.
//! - **`IS LABELED`:** a vertex tests its label set (`%`: the set is not empty), an edge its one
//!   label; `NULL` gives `NULL`; a non-element is error 22G03.
//! - **Property access:** on `NULL` gives `NULL`; on an element reads the property (missing:
//!   `NULL`); on a map reads the key (missing: `NULL`); on anything else error 22G03.
//! - **Indexing and slicing** (stage 3f, Cypher compatibility forms, the openCypher TCK
//!   `List1` / `List2` / `Map2` semantics): `x[i]` on a list takes the item `i` (an integer
//!   counted from 0, negative from the end: `x[-1]` is the last), out of range `NULL`; on a
//!   map the entry of a string key (missing: `NULL`), on a node or an edge the property of a
//!   string key (missing: `NULL`); the parser writes `x['key']` with a string literal as
//!   `x.key`. `x[a..b]` on a list takes the items from `a` up to `b` (excluded), each bound
//!   negative from the end and clamped to the list, an omitted one the start / the end; `a >=
//!   b` gives `[]`. A `NULL` target, index or bound gives `NULL`. A target of another kind (a
//!   string, a number, a boolean, a path) is error 22G03, and so is an index or bound of the
//!   wrong kind (a float index is not truncated, a map key is a string); these errors carry
//!   [`GqlHelpTopic::Subscript`]. Both are links of a [`Node::Chain`] (`a[0][1]` is a loop);
//!   an owned list gives its item up or is cut in place, a constant one is copied (the slice
//!   after [`Env::reserve`]).
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
//!   path never equals a value of another kind, and is not ordered (`NULL`).
//! - **Map projections** (stage 2c, Cypher) `v {.key, .*, x, key: expr}`: a `NULL` target gives
//!   `NULL` (where the record constructor `{key: v.key}` gives `{key: null}`); from a node or an
//!   edge `.key` reads its property (missing: `NULL`, the key kept) and `.*` every stored
//!   property in stored order (no id, no labels; a stored `null` left out, as `properties()`
//!   does); from a map `.key` reads the entry and `.*` every entry; any other target is error
//!   22G03. `x` is the variable's value under its name, `key: expr` the expression's. Keys come
//!   in written order with `.*` expanded at its place; a key written twice keeps its first place
//!   and takes the later value (design E12). An element inside stays an element.
//! - **List comprehensions** (stage 2c, Cypher) `[x IN list WHERE cond | expr]`: a `NULL` list
//!   gives `NULL`, a value that is not a list error 22G03; an item is kept only when `cond` is
//!   TRUE (FALSE and `NULL` drop it); `| expr` maps a kept item, else the item itself is kept;
//!   the result keeps the list's order. The variable is a **local** (design M4): compiled to
//!   `Node::Local` through [`Scope::local`] and bound per item through [`Env::local`] by a
//!   frame on the call stack (`LocalEnv`), so a comprehension allocates nothing per item
//!   besides the result values. The binder refuses a variable that shadows another name (E14),
//!   so a local never hides a row variable.
//! - **List predicates and `reduce`** (stage 2d, user decision D23, openCypher compatibility
//!   forms) compile to the same node with their form: `all` / `any` / `none` / `single(x IN
//!   list WHERE cond)` give `NULL` for a `NULL` list and error 22G03 for a value that is not a
//!   list; then a decided result returns at once (`all` at the first FALSE, `any` / `none` at
//!   the first TRUE, `single` at the second TRUE), else a `NULL` (UNKNOWN) condition for some
//!   item gives `NULL`, else `all` / `none` give TRUE, `any` FALSE, `single` whether exactly one
//!   item was TRUE (the openCypher TCK `Quantifier1`..`4` tables, which Neo4j passes; an empty
//!   list: `all` / `none` TRUE, `any` / `single` FALSE). `reduce(acc = init, x IN list | expr)`
//!   gives `NULL` for a `NULL` list, 22G03 for a non-list, else folds: `acc` starts at `init`
//!   (evaluated outside the locals) and takes `expr` per item; `acc` and `x` are two locals
//!   bound by two `LocalEnv` frames.
//! - **Aggregates** (`count`, `sum`, `avg`, `min`, `max`, `collect_list`, stage 3f
//!   `percentile_cont` / `percentile_disc`) are compiled only where
//!   the [`Scope`] admits them ([`Scope::aggregate`], the aggregating stage `aggregate.rs`); the
//!   compiled call reads its result through [`Env::aggregate`]. Elsewhere they are error 0A000.
//!
//! Errors are [`EvalError`]s carrying a GQLSTATUS ([`status`]), a message and the span of the
//! offending expression; the step that runs the evaluator maps them into a `TraverserError`.

use std::borrow::Cow;
use std::cmp::Ordering;

use super::ast::{
    BinaryOp, CallArgs, CaseExpr, CastTarget, ComprehensionForm, Endpoint, Expr, ExprKind,
    LabelExpr, LabelExprKind, ListComprehension, ListPredicate, Literal, MapLiteral, MapProjection,
    ProjectionItem, Subscript, TrimSide, UnaryOp,
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

#[cfg(test)]
mod local_tests;

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
    /// Data exception: null value not allowed (an endpoint of an inserted edge that is `NULL`).
    pub(crate) const NULL_VALUE: &str = "22004";
    /// Dependent object error: edges still exist (a plain `DELETE` of a node that still has
    /// edges, stage 3d) [ISO].
    pub(crate) const EDGES_STILL_EXIST: &str = "G1001";
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
            "a subquery (EXISTS, COUNT { }, COLLECT { } or a pattern comprehension) cannot be \
             evaluated here",
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
    /// The one label of an edge (`type(r)`); `None` for a vertex. A row of the traversal also
    /// answers it for an edge a `DELETE` removed earlier in the query (E3 of stage 3d), where
    /// every other read is an error. Default: the first of [`Self::labels`].
    fn edge_type(&self, edge: &Self::Element) -> Option<CompactString> {
        self.labels(edge).into_iter().next()
    }
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
    /// FALSE, never `NULL`; `COUNT { }`: the integer count, never `NULL`; a pattern
    /// comprehension or `COLLECT { }`: the list, never `NULL`), or `None` when the row has not
    /// run it yet: the evaluation then
    /// stops with [`EvalError::subquery_pending`], and the step hosting the subquery
    /// (`runtime::SubqueryHost`) runs it and evaluates the row again. The default runs nothing.
    fn subquery(&self, index: usize) -> Option<Value<Self::Element>> {
        let _ = index;
        None
    }
    /// The value of the comprehension variable `index` ([`Scope::local`], design M4). Only the
    /// evaluator's own [`LocalEnv`] holds locals; every other row has none (`NULL`).
    fn local(&self, index: usize) -> Value<Self::Element> {
        let _ = index;
        Value::Null
    }
}

/// The row of a comprehension's `WHERE` and `|` parts (design M4): the variable `index` bound
/// to one item of the list, everything else read from `parent`. It holds a reference to the
/// item and to the row around it, so a nested comprehension stacks one more on the call stack:
/// no allocation per item, and one type however deep the nesting (`parent` is a trait object,
/// so the evaluator is not instantiated once per nesting level).
struct LocalEnv<'a, E> {
    parent: &'a (dyn Env<Element = E> + 'a),
    index: usize,
    value: &'a Value<E>,
}

impl<E: Clone + PartialEq> Env for LocalEnv<'_, E> {
    type Element = E;

    fn variable(&self, slot: usize) -> Value<E> {
        self.parent.variable(slot)
    }

    fn kind(&self, element: &E) -> ElementKind {
        self.parent.kind(element)
    }

    fn element_id(&self, element: &E) -> CompactString {
        self.parent.element_id(element)
    }

    fn property(&self, element: &E, key: &str) -> Option<ElementProperty> {
        self.parent.property(element, key)
    }

    fn has_label(&self, element: &E, label: &str) -> bool {
        self.parent.has_label(element, label)
    }

    fn has_any_label(&self, element: &E) -> bool {
        self.parent.has_any_label(element)
    }

    fn labels(&self, element: &E) -> Vec<CompactString> {
        self.parent.labels(element)
    }

    fn edge_type(&self, edge: &E) -> Option<CompactString> {
        self.parent.edge_type(edge)
    }

    fn aggregate(&self, index: usize) -> Value<E> {
        self.parent.aggregate(index)
    }

    fn properties(&self, element: &E) -> Vec<(CompactString, ElementProperty)> {
        self.parent.properties(element)
    }

    fn endpoints(&self, edge: &E) -> Option<(E, E)> {
        self.parent.endpoints(edge)
    }

    fn reserve(&self, bytes: u64) -> bool {
        self.parent.reserve(bytes)
    }

    fn subquery(&self, index: usize) -> Option<Value<E>> {
        self.parent.subquery(index)
    }

    fn local(&self, index: usize) -> Value<E> {
        if index == self.index {
            self.value.clone()
        } else {
            self.parent.local(index)
        }
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
    /// The index of the comprehension variable `name` ([`Env::local`]) when one is in scope
    /// here (design M4). Only [`compile`]'s own scope of a comprehension's parts has locals.
    fn local(&self, name: &str) -> Option<usize> {
        let _ = name;
        None
    }
    /// How many comprehension variables are in scope: the index the next one gets.
    fn locals(&self) -> usize {
        0
    }
}

/// The scope of a comprehension's `WHERE` and `|` parts: its variable, then `parent` (a trait
/// object, so [`compile`] is instantiated once however deep comprehensions nest).
struct LocalScope<'a> {
    parent: &'a (dyn Scope + 'a),
    name: &'a str,
    index: usize,
}

impl Scope for LocalScope<'_> {
    fn variable(&self, name: &str) -> Option<usize> {
        self.parent.variable(name)
    }

    fn parameter(&self, name: &str) -> Option<ElementProperty> {
        self.parent.parameter(name)
    }

    fn aggregate(&self, call: AggregateRef<'_>) -> Option<Result<usize, EvalError>> {
        self.parent.aggregate(call)
    }

    fn subquery(&self, span: Span) -> Option<Result<usize, EvalError>> {
        self.parent.subquery(span)
    }

    fn local(&self, name: &str) -> Option<usize> {
        if name == self.name {
            Some(self.index)
        } else {
            self.parent.local(name)
        }
    }

    fn locals(&self) -> usize {
        self.index.saturating_add(1)
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
    /// The value of a subquery (`EXISTS`, a pattern comprehension), read through
    /// [`Env::subquery`].
    Subquery(usize),
    /// A comprehension variable, read through [`Env::local`] (design M4).
    Local(usize),
    /// A map projection `target {.k, .*, x, k: expr}`.
    Projection(Box<Projection<E>>),
    /// A list comprehension `[x IN list WHERE cond | expr]`.
    Comprehension(Box<Comprehension<E>>),
}

/// A compiled map projection.
#[derive(Debug, Clone)]
struct Projection<E> {
    target: Compiled<E>,
    items: Vec<ProjectionPart<E>>,
}

/// One item of a [`Projection`], in written order.
#[derive(Debug, Clone)]
enum ProjectionPart<E> {
    /// `.key` (and `x`, whose key is its name, compiled as an entry).
    Property(CompactString),
    /// `.*`.
    All,
    /// `key: expr` and a variable item `x`.
    Entry(CompactString, Compiled<E>),
}

/// A compiled list comprehension: `index` is its variable's local index. The list predicates
/// and `reduce` (stage 2d, D23) compile to it too, told apart by `form`.
#[derive(Debug, Clone)]
struct Comprehension<E> {
    form: Form<E>,
    index: usize,
    list: Compiled<E>,
    filter: Option<Compiled<E>>,
    map: Option<Compiled<E>>,
}

/// What a [`Comprehension`] computes.
#[derive(Debug, Clone)]
enum Form<E> {
    /// The list of kept (and mapped) items.
    List,
    /// `all` / `any` / `none` / `single`: a truth value.
    Predicate(ListPredicate),
    /// `reduce`: the accumulator (local `index - 1`) after the last item, starting at `init`.
    Reduce(Box<Compiled<E>>),
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
    /// `[i]`: a list item, a map entry, an element property.
    Index(Compiled<E>),
    /// `[from..to]`, each bound optional.
    Slice(Box<SliceBounds<E>>),
}

/// The bounds of a [`LinkOp::Slice`], each optional.
type SliceBounds<E> = (Option<Compiled<E>>, Option<Compiled<E>>);

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
        | ExprKind::IsEndpoint { .. }
        | ExprKind::Index(_) => compile_chain(expr, scope)?,
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
        ExprKind::Index(index) => Some(&index.target),
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
                ExprKind::Index(index) => compile_subscript(&index.subscript, scope)?,
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

/// The link of `[i]` or `[a..b]`.
#[inline(never)]
fn compile_subscript<E: Clone>(
    subscript: &Subscript,
    scope: &impl Scope,
) -> Result<LinkOp<E>, EvalError> {
    Ok(match subscript {
        Subscript::At(index) => LinkOp::Index(compile(index, scope)?),
        Subscript::Slice { from, to } => {
            let bound =
                |bound: &Option<Expr>| bound.as_ref().map(|b| compile(b, scope)).transpose();
            LinkOp::Slice(Box::new((bound(from)?, bound(to)?)))
        }
    })
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
        ExprKind::Variable(name) => compile_variable(&name.name, scope, span)?,
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
        | ExprKind::IsEndpoint { .. }
        | ExprKind::Index(_) => compile_chain(expr, scope)?,
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
        ExprKind::MapProjection(projection) => compile_projection(projection, scope)?,
        ExprKind::ListComprehension(comprehension) => compile_comprehension(comprehension, scope)?,
        // A subquery collecting a list (stage 2c), named by the span of the whole expression.
        ExprKind::PatternComprehension(_) => match scope.subquery(span) {
            Some(Ok(index)) => Node::Subquery(index),
            Some(Err(error)) => return Err(error),
            None => {
                return Err(EvalError::new(
                    status::NOT_SUPPORTED,
                    "a pattern comprehension is not yet supported in this expression",
                    span,
                ));
            }
        },
    })
}

/// A variable: a comprehension variable in scope first (the binder refuses one shadowing any
/// other name, design E14), else a slot of the row.
fn compile_variable<E>(name: &str, scope: &impl Scope, span: Span) -> Result<Node<E>, EvalError> {
    if let Some(index) = scope.local(name) {
        return Ok(Node::Local(index));
    }
    match scope.variable(name) {
        Some(slot) => Ok(Node::Variable(slot)),
        None => Err(EvalError::new(
            status::SEMANTIC,
            format!("the variable {name} is not defined"),
            span,
        )),
    }
}

#[inline(never)]
fn compile_projection<E: Clone>(
    projection: &MapProjection,
    scope: &impl Scope,
) -> Result<Node<E>, EvalError> {
    let target = compile(&projection.target, scope)?;
    let mut items = Vec::with_capacity(projection.items.len());
    for item in &projection.items {
        items.push(match item {
            ProjectionItem::Property(key) => ProjectionPart::Property(key.name.as_str().into()),
            ProjectionItem::AllProperties(_) => ProjectionPart::All,
            ProjectionItem::Variable(var) => ProjectionPart::Entry(
                var.name.as_str().into(),
                Compiled {
                    node: compile_variable(&var.name, scope, var.span)?,
                    span: var.span,
                },
            ),
            ProjectionItem::Entry(entry) => ProjectionPart::Entry(
                entry.key.name.as_str().into(),
                compile(&entry.value, scope)?,
            ),
        });
    }
    Ok(Node::Projection(Box::new(Projection { target, items })))
}

#[inline(never)]
fn compile_comprehension<E: Clone>(
    comprehension: &ListComprehension,
    scope: &impl Scope,
) -> Result<Node<E>, EvalError> {
    let list = compile(&comprehension.list, scope)?;
    // `reduce`: its initial value outside the locals, its accumulator the local before the
    // item variable (the item variable's scope is nested in the accumulator's).
    let (form, accumulator) = match &comprehension.form {
        ComprehensionForm::List => (Form::List, None),
        ComprehensionForm::Predicate(predicate) => (Form::Predicate(*predicate), None),
        ComprehensionForm::Reduce(accumulator) => (
            Form::Reduce(Box::new(compile(&accumulator.init, scope)?)),
            Some(LocalScope {
                parent: scope,
                name: &accumulator.variable.name,
                index: scope.locals(),
            }),
        ),
    };
    let outer: &dyn Scope = match &accumulator {
        Some(accumulator) => accumulator,
        None => scope,
    };
    let index = outer.locals();
    let local = LocalScope {
        parent: outer,
        name: &comprehension.variable.name,
        index,
    };
    let filter = comprehension
        .filter
        .as_ref()
        .map(|filter| compile(filter, &local))
        .transpose()?;
    let map = comprehension
        .map
        .as_ref()
        .map(|map| compile(map, &local))
        .transpose()?;
    Ok(Node::Comprehension(Box::new(Comprehension {
        form,
        index,
        list,
        filter,
        map,
    })))
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
            // A comprehension variable is no slot of the row.
            Node::Const(_) | Node::Aggregate(_) | Node::Subquery(_) | Node::Local(_) => {}
            Node::Variable(slot) => slots.push(*slot),
            Node::Projection(projection) => {
                projection.target.variables(slots);
                for item in &projection.items {
                    if let ProjectionPart::Entry(_, value) = item {
                        value.variables(slots);
                    }
                }
            }
            Node::Comprehension(comprehension) => {
                if let Form::Reduce(init) = &comprehension.form {
                    init.variables(slots);
                }
                comprehension.list.variables(slots);
                if let Some(filter) = &comprehension.filter {
                    filter.variables(slots);
                }
                if let Some(map) = &comprehension.map {
                    map.variables(slots);
                }
            }
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
                        LinkOp::Binary(_, right)
                        | LinkOp::IsEndpoint(_, _, right)
                        | LinkOp::Index(right) => right.variables(slots),
                        LinkOp::Slice(bounds) => {
                            for bound in [&bounds.0, &bounds.1].into_iter().flatten() {
                                bound.variables(slots);
                            }
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
            Node::Local(index) => env.local(*index),
            Node::Projection(projection) => projection.eval(env)?,
            Node::Comprehension(comprehension) => comprehension.eval(env)?,
            Node::Const(value) => value.clone(),
            Node::Case(case) => case.eval(env)?.into_owned(),
            Node::Chain(first, links) => eval_chain(first, links, env)?.into_owned(),
        })
    }
}

impl<E: Clone + PartialEq> Projection<E> {
    /// The map (design item 7): `NULL` for a `NULL` target; from a node or an edge its
    /// properties, from a map its entries; keys in written order with `.*` expanded at its
    /// place; a key written twice keeps its first place and takes the later value (E12).
    #[inline(never)]
    fn eval<En: Env<Element = E>>(&self, env: &En) -> Result<Value<E>, EvalError> {
        let target = self.target.eval_cow(env)?;
        let source = match &*target {
            Value::Null => return Ok(Value::Null),
            Value::Element(_) | Value::Map(_) => &*target,
            other => {
                return Err(EvalError::invalid_type(
                    format!(
                        "a map projection reads a node, an edge or a map, not {}",
                        other.kind_name()
                    ),
                    self.target.span,
                ));
            }
        };
        let mut entries: Vec<(CompactString, Value<E>)> = Vec::with_capacity(self.items.len());
        for item in &self.items {
            match item {
                ProjectionPart::Property(key) => {
                    let value = match source {
                        Value::Element(element) => env
                            .property(element, key)
                            .map(Value::from_property)
                            .unwrap_or(Value::Null),
                        Value::Map(map) => map
                            .iter()
                            .find(|(name, _)| name == key)
                            .map(|(_, value)| value.clone())
                            .unwrap_or(Value::Null),
                        _ => Value::Null,
                    };
                    put_entry(&mut entries, key.clone(), value);
                }
                ProjectionPart::All => match source {
                    Value::Element(element) => {
                        for (key, value) in env.properties(element) {
                            // A stored `null` is no property (it reads as NULL).
                            if !matches!(value, ElementProperty::Null) {
                                put_entry(&mut entries, key, Value::from_property(value));
                            }
                        }
                    }
                    Value::Map(map) => {
                        for (key, value) in map {
                            put_entry(&mut entries, key.clone(), value.clone());
                        }
                    }
                    _ => {}
                },
                ProjectionPart::Entry(key, value) => {
                    put_entry(&mut entries, key.clone(), value.eval(env)?);
                }
            }
        }
        Ok(Value::Map(entries))
    }
}

/// Adds `key: value` to a map being built: a key already there keeps its place and takes the
/// new value (E12).
fn put_entry<E>(entries: &mut Vec<(CompactString, Value<E>)>, key: CompactString, value: Value<E>) {
    match entries.iter_mut().find(|(name, _)| *name == key) {
        Some(entry) => entry.1 = value,
        None => entries.push((key, value)),
    }
}

impl<E: Clone + PartialEq> Comprehension<E> {
    /// The list (design item 7): `NULL` for a `NULL` list, error 22G03 for a value that is not a
    /// list; an item is kept only when the `WHERE` condition is TRUE; the result keeps the
    /// list's order. The variable is bound per item through a [`LocalEnv`] (design M4).
    #[inline(never)]
    fn eval<En: Env<Element = E>>(&self, env: &En) -> Result<Value<E>, EvalError> {
        let items = match self.list.eval(env)? {
            Value::Null => return Ok(Value::Null),
            Value::List(items) => items,
            other => {
                let what = match &self.form {
                    Form::List => "a list comprehension",
                    Form::Predicate(ListPredicate::All) => "all()",
                    Form::Predicate(ListPredicate::Any) => "any()",
                    Form::Predicate(ListPredicate::None) => "none()",
                    Form::Predicate(ListPredicate::Single) => "single()",
                    Form::Reduce(_) => "reduce()",
                };
                return Err(EvalError::invalid_type(
                    format!("{what} iterates over a list, not {}", other.kind_name()),
                    self.list.span,
                ));
            }
        };
        match &self.form {
            Form::List => {}
            Form::Predicate(predicate) => return self.predicate(*predicate, &items, env),
            Form::Reduce(init) => return self.reduce(init, items, env),
        }
        let mut out = Vec::with_capacity(if self.filter.is_some() {
            0
        } else {
            items.len()
        });
        for item in items {
            let local = LocalEnv {
                parent: env,
                index: self.index,
                value: &item,
            };
            if let Some(filter) = &self.filter
                && filter.truth(&local)? != Truth::True
            {
                continue;
            }
            match &self.map {
                Some(map) => out.push(map.eval(&local)?),
                None => out.push(item),
            }
        }
        Ok(Value::List(out))
    }

    /// `all` / `any` / `none` / `single(x IN list WHERE cond)` over the items of a list that is
    /// not `NULL`, three-valued as the openCypher TCK (`Quantifier1`..`4`) and Neo4j give them:
    /// a decided result returns at once (`all` at the first FALSE, `any` / `none` at the first
    /// TRUE, `single` at the second TRUE; later items are not evaluated); otherwise an UNKNOWN
    /// condition anywhere gives `NULL`; otherwise `all` and `none` are TRUE, `any` FALSE and
    /// `single` whether exactly one item was TRUE. An empty list: `all` / `none` TRUE, `any` /
    /// `single` FALSE.
    #[inline(never)]
    fn predicate<En: Env<Element = E>>(
        &self,
        predicate: ListPredicate,
        items: &[Value<E>],
        env: &En,
    ) -> Result<Value<E>, EvalError> {
        let mut unknown = false;
        let mut matched = false;
        for item in items {
            let local = LocalEnv {
                parent: env,
                index: self.index,
                value: item,
            };
            let truth = match &self.filter {
                Some(filter) => filter.truth(&local)?,
                None => truth_of(item, self.list.span)?,
            };
            match (predicate, truth) {
                (ListPredicate::All, Truth::False) => return Ok(Truth::False.into_value()),
                (ListPredicate::Any, Truth::True) => return Ok(Truth::True.into_value()),
                (ListPredicate::None, Truth::True) => return Ok(Truth::False.into_value()),
                (ListPredicate::Single, Truth::True) if matched => {
                    return Ok(Truth::False.into_value());
                }
                (ListPredicate::Single, Truth::True) => matched = true,
                (_, Truth::Unknown) => unknown = true,
                (ListPredicate::All, Truth::True)
                | (
                    ListPredicate::Any | ListPredicate::None | ListPredicate::Single,
                    Truth::False,
                ) => {}
            }
        }
        let truth = if unknown {
            Truth::Unknown
        } else {
            Truth::of(match predicate {
                ListPredicate::All | ListPredicate::None => true,
                ListPredicate::Any => false,
                ListPredicate::Single => matched,
            })
        };
        Ok(truth.into_value())
    }

    /// `reduce(acc = init, x IN list | expr)` over the items of a list that is not `NULL`: the
    /// accumulator starts at `init` and takes the value of `expr` for each item in list order,
    /// with both locals bound by two [`LocalEnv`] frames (no allocation besides the values
    /// `expr` builds); an empty list gives `init`.
    #[inline(never)]
    fn reduce<En: Env<Element = E>>(
        &self,
        init: &Compiled<E>,
        items: Vec<Value<E>>,
        env: &En,
    ) -> Result<Value<E>, EvalError> {
        let mut accumulator = init.eval(env)?;
        let Some(map) = &self.map else {
            return Ok(accumulator);
        };
        for item in &items {
            let next = {
                let outer = LocalEnv {
                    parent: env,
                    index: self.index.saturating_sub(1),
                    value: &accumulator,
                };
                let local = LocalEnv {
                    parent: &outer,
                    index: self.index,
                    value: item,
                };
                map.eval(&local)?
            };
            accumulator = next;
        }
        Ok(accumulator)
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
            LinkOp::Index(index) => {
                let at = index.eval_cow(env)?;
                subscript(value, &at, index.span, env, link.span)?
            }
            LinkOp::Slice(bounds) => slice(value, bounds, env, link.span)?,
        };
        value = Cow::Owned(next);
        left_span = link.span;
    }
    Ok(value)
}

/// Design F6: `||` (and Cypher `+`) of two strings or two lists, and `+` of a list and an item,
/// asks the row for the result's size before building it ([`Env::reserve`]).
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
        // `+` of a list and one item (stage 3f).
        (Value::List(items), item) | (item, Value::List(items)) if !item.is_null() => {
            functions::list_bytes((items.len() as u64).saturating_add(1))
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

/// The integer of a list index or slice bound: `Ok(None)` for `NULL`, 22G03 for any other
/// kind (a float is not truncated: `x[1.0]` is an error, as in Neo4j).
fn integer_index<E>(value: &Value<E>, what: &str, span: Span) -> Result<Option<i64>, EvalError> {
    match value {
        Value::Null => Ok(None),
        Value::Scalar(ElementProperty::Number(number)) => match number.n {
            N::NegInt(index) => Ok(Some(index)),
            N::Float(_) => Err(not_an_integer(what, value, span)),
        },
        other => Err(not_an_integer(what, other, span)),
    }
}

fn not_an_integer<E>(what: &str, value: &Value<E>, span: Span) -> EvalError {
    let kind = match value {
        Value::Scalar(ElementProperty::Number(_)) => "a float",
        other => other.kind_name(),
    };
    EvalError::invalid_type(format!("{what} is an integer, not {kind}"), span)
        .with_topic(GqlHelpTopic::Subscript)
}

/// `index` counted from the end when negative (`-1` is the last of `len` items), clamped to
/// `0..=len` (a slice bound).
fn clamp_index(index: i64, len: usize) -> usize {
    let len_i = i64::try_from(len).unwrap_or(i64::MAX);
    let from_start = if index < 0 {
        len_i.saturating_add(index)
    } else {
        index
    };
    usize::try_from(from_start.clamp(0, len_i)).unwrap_or(0)
}

/// `target[at]` (stage 3f, module docs "Indexing and slicing"): a list item by an integer
/// (negative from the end, out of range `NULL`), a map entry or an element property by a
/// string; a `NULL` target or index gives `NULL`; any other kind of target or index is 22G03.
/// An owned list gives its item up without a copy.
#[inline(never)]
fn subscript<En: Env>(
    target: Cow<'_, Value<En::Element>>,
    at: &Value<En::Element>,
    at_span: Span,
    env: &En,
    span: Span,
) -> Result<Value<En::Element>, EvalError> {
    let key = |what: &str| -> Result<Option<&str>, EvalError> {
        match at {
            Value::Null => Ok(None),
            other => match other.as_str() {
                Some(key) => Ok(Some(key)),
                None => Err(EvalError::invalid_type(
                    format!(
                        "{what} is read with a string key, not {}",
                        other.kind_name()
                    ),
                    at_span,
                )
                .with_topic(GqlHelpTopic::Subscript)),
            },
        }
    };
    match &*target {
        Value::Null => Ok(Value::Null),
        Value::List(items) => {
            let Some(index) = integer_index(at, "a list index", at_span)? else {
                return Ok(Value::Null);
            };
            let len = items.len();
            let position = if index < 0 {
                i64::try_from(len)
                    .ok()
                    .and_then(|len| len.checked_add(index))
            } else {
                Some(index)
            };
            let Some(position) = position
                .and_then(|p| usize::try_from(p).ok())
                .filter(|p| *p < len)
            else {
                return Ok(Value::Null);
            };
            Ok(match target {
                Cow::Owned(Value::List(mut items)) => items.swap_remove(position),
                _ => items.get(position).cloned().unwrap_or(Value::Null),
            })
        }
        Value::Map(entries) => {
            let Some(key) = key("a map")? else {
                return Ok(Value::Null);
            };
            Ok(entries
                .iter()
                .find(|(name, _)| name.as_str() == key)
                .map(|(_, value)| value.clone())
                .unwrap_or(Value::Null))
        }
        Value::Element(element) => {
            let Some(key) = key("a property of a node or an edge")? else {
                return Ok(Value::Null);
            };
            Ok(env
                .property(element, key)
                .map(Value::from_property)
                .unwrap_or(Value::Null))
        }
        other => Err(EvalError::invalid_type(
            format!(
                "cannot index {}: [ ] reads an item of a list, an entry of a map or a \
                 property of a node or an edge",
                other.kind_name()
            ),
            span,
        )
        .with_topic(GqlHelpTopic::Subscript)),
    }
}

/// `target[from..to]` (stage 3f, module docs "Indexing and slicing"): the items from `from`
/// up to `to` (excluded), each bound negative from the end and clamped to the list, an omitted
/// one the start / the end; `from >= to` gives `[]`. A `NULL` target or bound gives `NULL`,
/// a target that is no list or a bound that is no integer 22G03. An owned list is cut in
/// place; a borrowed one (a constant) is copied after [`Env::reserve`].
#[inline(never)]
fn slice<En: Env>(
    target: Cow<'_, Value<En::Element>>,
    bounds: &SliceBounds<En::Element>,
    env: &En,
    span: Span,
) -> Result<Value<En::Element>, EvalError> {
    let mut ends = [None, None];
    let mut null = false;
    for (end, bound) in ends.iter_mut().zip([&bounds.0, &bounds.1]) {
        if let Some(bound) = bound {
            match integer_index(&*bound.eval_cow(env)?, "a slice bound", bound.span)? {
                Some(index) => *end = Some(index),
                None => null = true,
            }
        }
    }
    let len = match &*target {
        Value::Null => return Ok(Value::Null),
        Value::List(items) => items.len(),
        other => {
            return Err(EvalError::invalid_type(
                format!("a slice [a..b] reads a list, not {}", other.kind_name()),
                span,
            )
            .with_topic(GqlHelpTopic::Subscript));
        }
    };
    if null {
        return Ok(Value::Null);
    }
    let from = ends[0].map_or(0, |index| clamp_index(index, len));
    let to = ends[1].map_or(len, |index| clamp_index(index, len));
    if from >= to {
        return Ok(Value::List(Vec::new()));
    }
    match target {
        Cow::Owned(Value::List(mut items)) => {
            items.truncate(to);
            items.drain(..from);
            Ok(Value::List(items))
        }
        Cow::Borrowed(Value::List(items)) => {
            functions::reserve(env, functions::list_bytes((to - from) as u64), span)?;
            Ok(Value::List(items.get(from..to).unwrap_or(&[]).to_vec()))
        }
        _ => Ok(Value::Null),
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
                // Different kinds, or a `NaN`: never equal (the openCypher TCK, IEEE 754).
                _ => Truth::False,
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
        // Two non-NULL values of different kinds are never equal (the openCypher TCK).
        _ => Truth::False,
    }
}

/// The outcome of an ordering comparison `< <= > >=` of two values ([`order`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Order {
    /// The values are ordered: the operator tests this ordering.
    Is(Ordering),
    /// Numbers that IEEE 754 leaves unordered (a `NaN` operand): every operator is FALSE.
    Unordered,
    /// A `NULL` operand, or kinds that are not ordered against each other: `NULL`.
    Unknown,
}

/// The order of two values for `< <= > >=` (see the module docs): scalars of one comparable
/// kind by value, lists lexicographically (the first pair of items that is not equal decides,
/// a list that is a prefix of the other sorts first), everything else [`Order::Unknown`].
pub(crate) fn order<E>(a: &Value<E>, b: &Value<E>) -> Order {
    match (a, b) {
        (Value::Scalar(a), Value::Scalar(b)) => match compare_scalars(a, b) {
            Some(ordering) => Order::Is(ordering),
            None => match (a, b) {
                (ElementProperty::Number(_), ElementProperty::Number(_)) => Order::Unordered,
                _ => Order::Unknown,
            },
        },
        (Value::List(a), Value::List(b)) => order_lists(a, b),
        _ => Order::Unknown,
    }
}

/// Lexicographic order of two lists; it recurses along nested lists like [`equals`].
fn order_lists<E>(a: &[Value<E>], b: &[Value<E>]) -> Order {
    for (a, b) in a.iter().zip(b) {
        match order(a, b) {
            Order::Is(Ordering::Equal) => {}
            decided => return decided,
        }
    }
    Order::Is(a.len().cmp(&b.len()))
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
        Order::Is(order) => Truth::of(test(order)).into_value(),
        Order::Unordered => Truth::False.into_value(),
        Order::Unknown => Value::Null,
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
            // Cypher `+` of a list and one other value appends or prepends it (stage 3f).
            (Value::List(items), item) => {
                let mut list = Vec::with_capacity(items.len() + 1);
                list.extend(items.iter().cloned());
                list.push(item.clone());
                Value::List(list)
            }
            (item, Value::List(items)) => {
                let mut list = Vec::with_capacity(items.len() + 1);
                list.push(item.clone());
                list.extend(items.iter().cloned());
                Value::List(list)
            }
            _ => return Err(add_error(left, right, span)),
        },
        BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod | BinaryOp::Pow => {
            match (left, right) {
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
            }
        }
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
        "+ adds two numbers, joins two strings or two lists, or adds an item to a list, not {} \
         and {}",
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
    if op == BinaryOp::Pow {
        return power(float(a), float(b), span);
    }
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
                // IEEE 754: a float divided by zero is `NaN` or an infinity (the openCypher TCK).
                BinaryOp::Div => a / b,
                BinaryOp::Mod => a % b,
                _ => f64::NAN,
            };
            let by_zero = op == BinaryOp::Div && b == 0.0;
            if result.is_infinite() && a.is_finite() && b.is_finite() && !by_zero {
                return Err(EvalError::out_of_range(span));
            }
            Ok(Value::Scalar(ElementProperty::Number(Number::from_f64(
                result,
            ))))
        }
    }
}

/// Cypher `x ^ y` (stage 3f): always a float, with the rules of `power(x, y)`: 0 to a negative
/// exponent and a negative base to a non-integer exponent are error 2201F, a result that is
/// not finite from finite operands 22003.
fn power<E>(x: f64, y: f64, span: Span) -> Result<Value<E>, EvalError> {
    if x == 0.0 && y < 0.0 {
        return Err(EvalError::new(
            status::POWER,
            format!(
                "^ raises 0 to a negative exponent ({})",
                Number::from_f64(y)
            ),
            span,
        ));
    }
    if x < 0.0 && y.is_finite() && y.fract() != 0.0 {
        return Err(EvalError::new(
            status::POWER,
            format!(
                "^ raises a negative base ({}) to a non-integer exponent ({})",
                Number::from_f64(x),
                Number::from_f64(y)
            ),
            span,
        ));
    }
    let result = x.powf(y);
    if !result.is_finite() && x.is_finite() && y.is_finite() {
        return Err(EvalError::out_of_range(span));
    }
    Ok(Value::float(result))
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
                .edge_type(element)
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
        | Function::CollectList
        | Function::PercentileCont
        | Function::PercentileDisc => Err(EvalError::new(
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
