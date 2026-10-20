use std::borrow::Cow;
use std::cmp::Ordering;
use std::fmt::Debug;

use compact_str::CompactString;

use crate::error::TraverserResult;
use crate::graph::schema::{ValueType, type_of};
use crate::value::{ElementProperty, GType, Number, PathRef, StreamKind, Temporal, TraverserValue};
use crate::{Traverser, TraverserContext};

/// A predicate (`P.eq`, `P.gt`, `P.within`, ...), the argument of `has()`, `is()` and `where()`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum P<T: Into<PValue>> {
    /// Equal to.
    Eq(T),
    /// Not equal to.
    Neq(T),
    /// Less than.
    Lt(T),
    /// Greater than.
    Gt(T),
    /// Less than or equal to.
    Lte(T),
    /// Greater than or equal to.
    Gte(T),
    /// Equal to one of the given values.
    WithIn(Vec<T>),
    /// Equal to none of the given values.
    WithOut(Vec<T>),
    /// Between two bounds, exclusive.
    Inside(T, T),
    /// Outside two bounds, exclusive.
    Outside(T, T),
    /// Between two bounds, inclusive of the lower bound.
    Between(T, T),
    /// A string starting with the given prefix.
    StartingWith(T),
    /// A string not starting with the given prefix.
    NotStartingWith(T),
    /// A string ending with the given suffix.
    EndingWith(T),
    /// A string not ending with the given suffix.
    NotEndingWith(T),
    /// A string containing the given substring.
    Containing(T),
    /// A string not containing the given substring.
    NotContaining(T),
    /// A string in which the regular expression matches somewhere (TinkerPop
    /// `TextP.regex`); the dialect is the Rust `regex` crate's, see [`P::regex`].
    Regex(TextRegex),
    /// A string in which the regular expression matches nowhere (TinkerPop
    /// `TextP.notRegex`); `true` for a value that is not a string.
    NotRegex(TextRegex),
    /// Tests the type a value *has* at runtime, as vanilla `P.typeOf(GType)` does.
    ///
    /// This is a check on the observed value, never on a schema declaration: it is what locates
    /// the values a data-fix has to convert.
    TypeOf(GType),
    /// Both predicates hold; the right one is evaluated only when the left one holds.
    And(Box<P<T>>, Box<P<T>>),
    /// At least one predicate holds; the right one is evaluated only when the left one fails.
    Or(Box<P<T>>, Box<P<T>>),
    /// The complement of the predicate.
    Not(Box<P<T>>),
}

impl<T: Into<PValue>> P<T> {
    /// Combines two predicates: the result holds when both do (TinkerPop `P.and(P)`).
    ///
    /// Evaluation is left to right and short-circuits, as TinkerPop's `AndP` does: when `self`
    /// fails, `other` is not evaluated. An incomparable operand is plain `false` (never an
    /// error), so the logic is two-valued. Composites are never pushed into an index or a
    /// source filter.
    ///
    /// **Deviation from TinkerPop 3.8.2:** none in the result; there is no third (error) value.
    ///
    /// ```
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let locked = graph.read();
    /// let names = locked
    ///     .traversal()
    ///     .v(None)
    ///     .has_p("age", P::Gt(27).and(P::Lt(33)))
    ///     .values("name")
    ///     .to_list()
    ///     .unwrap();
    /// assert_eq!(names.len(), 2);
    /// ```
    #[must_use]
    pub fn and(self, other: P<T>) -> Self {
        P::And(Box::new(self), Box::new(other))
    }

    /// Combines two predicates: the result holds when either does (TinkerPop `P.or(P)`).
    ///
    /// Evaluation is left to right and short-circuits: when `self` holds, `other` is not
    /// evaluated.
    ///
    /// ```
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let locked = graph.read();
    /// let names = locked
    ///     .traversal()
    ///     .v(None)
    ///     .has_p("age", P::Lt(28).or(P::Gt(33)))
    ///     .values("name")
    ///     .to_list()
    ///     .unwrap();
    /// assert_eq!(names.len(), 2);
    /// ```
    #[must_use]
    pub fn or(self, other: P<T>) -> Self {
        P::Or(Box::new(self), Box::new(other))
    }

    /// The complement of this predicate (TinkerPop `P.negate()`).
    ///
    /// **Deviation from TinkerPop 3.8.2:** the plain boolean complement of the result over the
    /// same stream value, never the "opposite operator" (`gt` becomes `lte`). The two differ
    /// only for incomparable operands (`NaN`, a string against a number): `P::Gt(1).negate()`
    /// is `true` for `"foo"` here. Documented in the user's guide page "Predicates".
    ///
    /// ```
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let locked = graph.read();
    /// let names = locked
    ///     .traversal()
    ///     .v(None)
    ///     .has_p("age", P::Gt(29).negate())
    ///     .values("name")
    ///     .to_list()
    ///     .unwrap();
    /// assert_eq!(names.len(), 2);
    /// ```
    #[must_use]
    pub fn negate(self) -> Self {
        P::Not(Box::new(self))
    }

    /// The complement of a predicate (TinkerPop `P.not(P)`); see [`negate`](Self::negate) for
    /// the deviation from TinkerPop (plain complement, not operator flipping).
    // The name mirrors TinkerPop's `P.not(P)` on purpose; it is an associated constructor taking
    // the predicate to negate, not the unary `std::ops::Not` operator.
    #[allow(clippy::should_implement_trait)]
    #[must_use]
    pub fn not(predicate: P<T>) -> Self {
        P::Not(Box::new(predicate))
    }
}

impl P<PValue> {
    /// Builds TinkerPop's `TextP.regex(pattern)`: holds for a string in which `pattern` matches
    /// somewhere (anchor it with `^...$` for a whole-string match). Fails with
    /// [`ValueError::InvalidRegex`](crate::error::ValueError::InvalidRegex) when `pattern` does
    /// not compile.
    ///
    /// **Deviation from TinkerPop 3.8.2:** the pattern is compiled by the Rust
    /// [`regex`](https://docs.rs/regex/latest/regex/#syntax) crate, with its own dialect, not
    /// by `java.util.regex`. Matching is unanchored (like Java's `Matcher.find()`) and linear in
    /// the input; the pattern is compiled once, when the predicate is built.
    ///
    /// ```
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let locked = graph.read();
    /// let names = locked
    ///     .traversal()
    ///     .v(None)
    ///     .has_p("name", P::regex("^mar").unwrap())
    ///     .values("name")
    ///     .to_list()
    ///     .unwrap();
    /// assert_eq!(names, vec![ElementProperty::from("marko")]);
    /// ```
    pub fn regex(pattern: &str) -> Result<Self, crate::error::BoxedTraverserError> {
        Ok(P::Regex(TextRegex::new(pattern)?))
    }

    /// Builds TinkerPop's `TextP.notRegex(pattern)`: the complement of [`regex`](Self::regex)
    /// (also `true` for a value that is not a string).
    pub fn not_regex(pattern: &str) -> Result<Self, crate::error::BoxedTraverserError> {
        Ok(P::NotRegex(TextRegex::new(pattern)?))
    }
}

/// A compiled regular expression, the operand of [`P::Regex`] and [`P::NotRegex`] (TinkerPop
/// `TextP.regex`/`TextP.notRegex`).
///
/// **Deviation from TinkerPop 3.8.2:** the expression is compiled by the Rust
/// [`regex`](https://docs.rs/regex/latest/regex/#syntax) crate and follows that crate's dialect
/// instead of Java's `java.util.regex`. Matching is unanchored (like Java's `Matcher.find()`)
/// and linear in the input. The expression is compiled once, when the predicate is built.
#[derive(Clone)]
pub struct TextRegex {
    regex: std::sync::Arc<regex::Regex>,
}

impl TextRegex {
    /// Compiles `pattern`; fails with
    /// [`ValueError::InvalidRegex`](crate::error::ValueError::InvalidRegex) when it is not a
    /// valid expression (or compiles to more than an internal size limit).
    pub fn new(pattern: &str) -> Result<Self, crate::error::BoxedTraverserError> {
        regex::RegexBuilder::new(pattern)
            .size_limit(crate::graph::schema::types::PATTERN_SIZE_LIMIT)
            .build()
            .map(|regex| Self {
                regex: std::sync::Arc::new(regex),
            })
            .map_err(|err| {
                Box::new(crate::error::TraverserError::Value(
                    crate::error::ValueError::InvalidRegex {
                        pattern: pattern.to_string(),
                        reason: crate::graph::schema::types::pattern_error_reason(&err),
                    },
                ))
            })
    }

    /// The expression's source text.
    pub fn as_str(&self) -> &str {
        self.regex.as_str()
    }

    /// `true` when the expression matches somewhere in `text`.
    pub fn is_match(&self, text: &str) -> bool {
        self.regex.is_match(text)
    }
}

impl Debug for TextRegex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.regex.as_str())
    }
}

impl P<PValue> {
    /// Builds the vanilla `P.typeOf(GType)` predicate.
    ///
    /// Accepts a [`GType`] token or a [`ValueType`] directly, so both spellings read the same:
    ///
    /// ```
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let locked = graph.read();
    /// let mixed = locked
    ///     .traversal()
    ///     .v(None)
    ///     .has_label("person")
    ///     .values("age")
    ///     .is(P::type_of(GType::STRING))
    ///     .to_list()
    ///     .unwrap();
    /// assert!(mixed.is_empty());
    /// ```
    pub fn type_of(target: impl Into<GType>) -> Self {
        P::TypeOf(target.into())
    }
}

/// A stream value read as a comparable scalar (see `P::compare`).
enum Scalar<'a> {
    Null,
    Bool(bool),
    Number(&'a Number),
    Str(Cow<'a, str>),
    Uuid(u128),
    Temporal(Temporal),
    Vector(&'a crate::value::Vector),
}

impl<'a> Scalar<'a> {
    /// The scalar a property value holds; `None` for a container or an element.
    fn of(property: &'a ElementProperty) -> Option<Self> {
        match property {
            ElementProperty::Null => Some(Scalar::Null),
            ElementProperty::Boolean(b) => Some(Scalar::Bool(*b)),
            ElementProperty::Number(n) => Some(Scalar::Number(n)),
            ElementProperty::String(s) => Some(Scalar::Str(Cow::Borrowed(s.as_str()))),
            // A compressed string is compared on its text (decompressed once per test).
            ElementProperty::Compressed(text) => {
                Some(Scalar::Str(Cow::Owned(text.text().into_string())))
            }
            ElementProperty::Deduped(text) => Some(Scalar::Str(Cow::Borrowed(text.as_str()))),
            ElementProperty::Uuid(u) => Some(Scalar::Uuid(*u)),
            ElementProperty::Temporal(t) => Some(Scalar::Temporal(*t)),
            ElementProperty::Vector(v) => Some(Scalar::Vector(v)),
            ElementProperty::Object(_)
            | ElementProperty::Map(_)
            | ElementProperty::Array(_)
            | ElementProperty::Vertex(_)
            | ElementProperty::Edge(_) => None,
        }
    }
}

/// A literal operand of a predicate [`P`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum PValue {
    /// Stored as a [`CompactString`] rather than a `String`: `P::test()` clones the comparand
    /// once per traverser for the whole lifetime of the filter (see `test()`'s `P::Eq`/`P::Neq`
    /// arms), and `CompactString::clone()` is a stack copy with no heap traffic for short values
    /// (the common case for a comparand like a name or an id), where `String::clone()` would
    /// always allocate.
    String(CompactString),
    /// A numeric literal.
    Number(Number),
    /// A boolean literal.
    Boolean(bool),
    /// A UUID literal (`UUID.fromString(..)` in the DSL, [`ElementProperty::uuid_from_string`]
    /// in Rust). It matches only a stored `Uuid` with the same bits, never a `String`.
    Uuid(u128),
    /// A date or time literal ([`Temporal`]). It compares with a stored value of the same
    /// temporal type only (a `datetime` by its instant), never with a string.
    Temporal(Temporal),
    /// A vector literal (`vector([..])`, a Graphersal extension). It equals a stored vector of
    /// the same element kind, dimension and components, and orders among vectors by their order
    /// (kind, dimension, components); never against a list.
    Vector(crate::value::Vector),
    /// The `null` literal (`P.eq(null)`; `()` in the DSL). Following TinkerPop's comparability
    /// rules, `null` equals only `null`, `lte`/`gte` hold only for `null` itself, and `lt`/`gt`
    /// never hold.
    Null,
}

/// Renders a predicate the way the query wrote it, for a step name in `.profile()`.
///
/// `typeOf()` is spelled as vanilla spells it rather than as the enum debugs, so the physical
/// plan reads back as the query (`AGENTS.md` rule 11).
pub(crate) fn render_predicate<T: Into<PValue> + Clone>(predicate: &P<T>) -> String {
    use crate::value::literal::{rhai_pvalue, rhai_string};
    let one = |value: &T| rhai_pvalue(&value.clone().into());
    let many = |values: &[T]| values.iter().map(&one).collect::<Vec<_>>().join(", ");
    match predicate {
        P::Eq(v) => format!("P.eq({})", one(v)),
        P::Neq(v) => format!("P.neq({})", one(v)),
        P::Lt(v) => format!("P.lt({})", one(v)),
        P::Gt(v) => format!("P.gt({})", one(v)),
        P::Lte(v) => format!("P.lte({})", one(v)),
        P::Gte(v) => format!("P.gte({})", one(v)),
        P::WithIn(values) => format!("P.within([{}])", many(values)),
        P::WithOut(values) => format!("P.without([{}])", many(values)),
        P::Inside(a, b) => format!("P.inside({}, {})", one(a), one(b)),
        P::Outside(a, b) => format!("P.outside({}, {})", one(a), one(b)),
        P::Between(a, b) => format!("P.between({}, {})", one(a), one(b)),
        P::StartingWith(v) => format!("TextP.starting_with({})", one(v)),
        P::NotStartingWith(v) => format!("TextP.not_starting_with({})", one(v)),
        P::EndingWith(v) => format!("TextP.ending_with({})", one(v)),
        P::NotEndingWith(v) => format!("TextP.not_ending_with({})", one(v)),
        P::Containing(v) => format!("TextP.containing({})", one(v)),
        P::NotContaining(v) => format!("TextP.not_containing({})", one(v)),
        P::Regex(regex) => format!("TextP.regex({})", rhai_string(regex.as_str())),
        P::NotRegex(regex) => format!("TextP.not_regex({})", rhai_string(regex.as_str())),
        P::TypeOf(target) => format!("P.typeOf({})", target.token()),
        P::And(a, b) => format!("{}.and({})", render_predicate(a), render_predicate(b)),
        P::Or(a, b) => format!("{}.or({})", render_predicate(a), render_predicate(b)),
        P::Not(inner) => format!("P.not({})", render_predicate(inner)),
    }
}

/// How [`P::test_resolving`] reads a bare-string operand of `eq`/`neq`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Operands {
    /// `where(P)`: a step label on the path, then a vertex id, then a value.
    PathLabels,
    /// `has(key, P)`, `is(P)` and the other value filters: a vertex id, then a value; never a
    /// step label.
    Values,
    /// `hasId(P)`/`hasLabel(P)`/`hasKey(P)` and resolved side effects: always a value.
    Literal,
}

impl<T: Into<PValue> + Debug + Clone> P<T> {
    /// What testing this predicate under `where(P)` reads from the traverser path: a string
    /// argument of a comparison (`eq`/`neq`/`lt`/`lte`/`gt`/`gte`) names a step label when one is
    /// set (`where(P.gt("a"))`), so it is a label requirement. The value filters (`has`, `is`,
    /// ...) never read the path ([`test`](Self::test)) and do not call this.
    pub(crate) fn path_requirement(
        &self,
    ) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        match self {
            P::Eq(value)
            | P::Neq(value)
            | P::Lt(value)
            | P::Gt(value)
            | P::Lte(value)
            | P::Gte(value) => match value.clone().into() {
                PValue::String(label) => {
                    crate::traversal::optimizer::path_analysis::PathRequirement::labels([label])
                }
                _ => crate::traversal::optimizer::path_analysis::PathRequirement::None,
            },
            P::And(a, b) | P::Or(a, b) => a.path_requirement().union(b.path_requirement()),
            P::Not(inner) => inner.path_requirement(),
            _ => crate::traversal::optimizer::path_analysis::PathRequirement::None,
        }
    }

    /// `where(P)`: tests the predicate resolving a bare-string operand against the path labels,
    /// then the side effects (see [`resolve_side_effect`](Self::resolve_side_effect)), then as a
    /// literal. A composite resolves every child on its own, so
    /// `where(P.not(P.within("a")))` reads the side effect `a` through the `not`. `and`/`or`
    /// short-circuit left to right.
    pub(crate) fn test_where<S: crate::graph::GraphStorage>(
        &self,
        context: &mut TraverserContext<'_, S>,
        arg_value: &TraverserValue<S>,
        arg_path: PathRef,
    ) -> TraverserResult<bool> {
        match self {
            P::And(a, b) => Ok(a.test_where(context, arg_value, arg_path)?
                && b.test_where(context, arg_value, arg_path)?),
            P::Or(a, b) => Ok(a.test_where(context, arg_value, arg_path)?
                || b.test_where(context, arg_value, arg_path)?),
            P::Not(inner) => Ok(!inner.test_where(context, arg_value, arg_path)?),
            _ => {
                // `lt`/`lte`/`gt`/`gte` with a bare string resolve a path label here (`eq`/`neq`
                // do in `test`); `has(k, P.gt("x"))`/`is(..)` never come through `test_where`.
                if let Some(label) = self.ordering_label()
                    && let Some(labelled) = context.paths.by_label(arg_path, label.as_str())
                {
                    return self.compare_to_labelled(context, arg_value, &labelled);
                }
                if let Some(matched) = self.resolve_side_effect(context, arg_value, arg_path)? {
                    return Ok(matched);
                }
                self.test_with_labels(context, arg_value, arg_path)
            }
        }
    }

    /// The bare-string operand of `lt`/`lte`/`gt`/`gte` (a candidate step label).
    fn ordering_label(&self) -> Option<CompactString> {
        match self {
            P::Lt(v) | P::Gt(v) | P::Lte(v) | P::Gte(v) => match v.clone().into() {
                PValue::String(label) => Some(label),
                _ => None,
            },
            _ => None,
        }
    }

    /// The bare-string operand of any of the six comparisons (a candidate step label).
    fn comparison_label(&self) -> Option<CompactString> {
        match self {
            P::Eq(v) | P::Neq(v) => match v.clone().into() {
                PValue::String(label) => Some(label),
                _ => None,
            },
            _ => self.ordering_label(),
        }
    }

    /// The number of `by()` ring entries the label operands of this predicate consume: one per
    /// comparison leaf with a bare-string operand, counted in textual order whatever
    /// short-circuiting skips.
    fn label_operand_count(&self) -> usize {
        match self {
            P::And(a, b) | P::Or(a, b) => a.label_operand_count() + b.label_operand_count(),
            P::Not(inner) => inner.label_operand_count(),
            other => usize::from(other.comparison_label().is_some()),
        }
    }

    /// Compares the stream value with a labelled path object by this comparison (`labelled` is
    /// the operand): two scalars by the usual comparability rules, anything else (elements,
    /// paths, collections) by identity for `eq`/`neq` and incomparable (false) for an ordering.
    fn compare_to_labelled<S: crate::graph::GraphStorage>(
        &self,
        context: &mut TraverserContext<'_, S>,
        start: &TraverserValue<S>,
        labelled: &TraverserValue<S>,
    ) -> TraverserResult<bool> {
        let operand = match labelled {
            TraverserValue::Vertex(_)
            | TraverserValue::Edge(_)
            | TraverserValue::Path(_)
            | TraverserValue::Array(_) => None,
            other => {
                let storage: &S = context.storage;
                match other.into_node_property(storage)? {
                    ElementProperty::Null => Some(PValue::Null),
                    ElementProperty::Boolean(b) => Some(PValue::Boolean(b)),
                    ElementProperty::Number(n) => Some(PValue::Number(n)),
                    ElementProperty::String(s) => Some(PValue::String(s)),
                    ElementProperty::Uuid(u) => Some(PValue::Uuid(u)),
                    ElementProperty::Temporal(t) => Some(PValue::Temporal(t)),
                    ElementProperty::Vector(v) => Some(PValue::Vector(v)),
                    _ => None,
                }
            }
        };
        match operand {
            Some(operand) => match self {
                P::Eq(_) => P::Eq(operand).test_literal(context, start),
                P::Neq(_) => P::Neq(operand).test_literal(context, start),
                P::Lt(_) => P::Lt(operand).test_literal(context, start),
                P::Gt(_) => P::Gt(operand).test_literal(context, start),
                P::Lte(_) => P::Lte(operand).test_literal(context, start),
                P::Gte(_) => P::Gte(operand).test_literal(context, start),
                _ => Ok(false),
            },
            None => {
                let same = start == labelled;
                Ok(match self {
                    P::Eq(_) => same,
                    P::Neq(_) => !same,
                    _ => false,
                })
            }
        }
    }

    /// `where(P).by(..)`: like [`test_where`](Self::test_where), with the `by()` ring applied
    /// (TinkerPop's `WherePredicateStep` traversal ring).
    ///
    /// The current object is projected through `by[0]`; each comparison leaf whose operand is a
    /// bare string naming a path label projects the labelled object through the next ring entry,
    /// `by[(k + 1) % by.len()]` where `k` is the leaf's index among the label operands in textual
    /// order (`base` is the index of the first label operand of this subtree, `0` at the root;
    /// the indexes do not depend on short-circuiting). A leaf without a label operand
    /// (`gt(30)`, `within(..)`, an unknown side effect) projects only the current object.
    /// `Ok(None)` means a `by()` was unproductive: the whole `where` is false, the traverser is
    /// dropped, also under a `not`. `by` must not be empty.
    pub(crate) fn test_where_by<'g, S: crate::graph::GraphStorage + 'g>(
        &self,
        context: &mut TraverserContext<'_, S>,
        arg_value: &TraverserValue<S>,
        arg_path: PathRef,
        by: &[crate::traversal::By<'g, S>],
        child_needs_path: bool,
        base: usize,
    ) -> TraverserResult<Option<bool>> {
        use crate::traversal::step::path_window::project_object;
        match self {
            P::And(a, b) => Ok(
                match a.test_where_by(context, arg_value, arg_path, by, child_needs_path, base)? {
                    Some(true) => b.test_where_by(
                        context,
                        arg_value,
                        arg_path,
                        by,
                        child_needs_path,
                        base + a.label_operand_count(),
                    )?,
                    other => other,
                },
            ),
            P::Or(a, b) => Ok(
                match a.test_where_by(context, arg_value, arg_path, by, child_needs_path, base)? {
                    Some(false) => b.test_where_by(
                        context,
                        arg_value,
                        arg_path,
                        by,
                        child_needs_path,
                        base + a.label_operand_count(),
                    )?,
                    other => other,
                },
            ),
            P::Not(inner) => Ok(inner
                .test_where_by(context, arg_value, arg_path, by, child_needs_path, base)?
                .map(|held| !held)),
            _ => {
                let Some(first) = by.first() else {
                    return Ok(Some(self.test_where(context, arg_value, arg_path)?));
                };
                let Some(start) = project_object(first, child_needs_path, context, arg_value)?
                else {
                    return Ok(None);
                };
                if let Some(label) = self.comparison_label()
                    && let Some(labelled) = context.paths.by_label(arg_path, label.as_str())
                {
                    let ring = &by[(base + 1) % by.len()];
                    let Some(operand) = project_object(ring, child_needs_path, context, &labelled)?
                    else {
                        return Ok(None);
                    };
                    return Ok(Some(self.compare_to_labelled(context, &start, &operand)?));
                }
                if let Some(matched) = self.resolve_side_effect(context, &start, arg_path)? {
                    return Ok(Some(matched));
                }
                Ok(Some(self.test_with_labels(context, &start, arg_path)?))
            }
        }
    }

    /// `where(within(key))`/`where(without(key))` (and, through
    /// `resolve_comparison_side_effect`, `eq`/`neq`/`lt`/`gt`/`lte`/`gte` with a bare string): resolves a single bare-string `within`/
    /// `without` argument against a side-effect bucket populated by `aggregate()`/`store()`,
    /// mirroring how `P::Eq`/`P::Neq` (in `test_with_labels`) already resolve a bare string
    /// against a step label via `context.paths.by_label` first. `Ok(None)` when this predicate
    /// isn't a single-key `within`/`without`, or the key names no known side effect — the caller
    /// falls back to `test_with_labels`'s ordinary literal-value comparison.
    ///
    /// Deliberately **not** called from [`test`](Self::test) itself: `test()` is also the
    /// evaluation path for `has(key, P)`/`is(P)`/`hasValue(P)`/`all(P)`/`any(P)`, whose string
    /// arguments are always literal property values, never side-effect-bucket names. Only
    /// `where(P)` (`StepWhereP`) calls this, before falling back to `test_with_labels()`, so a name that
    /// happens to match both a literal value and a side-effect key can't change `has()`'s
    /// behavior.
    ///
    /// **Resolution order (deviation from a plain literal compare; user's guide: "Predicates:
    /// Comparison and Resolution"):** path label, then side effect, then literal.
    pub(crate) fn resolve_side_effect<S: crate::graph::GraphStorage>(
        &self,
        context: &mut TraverserContext<'_, S>,
        arg_value: &TraverserValue<S>,
        arg_path: PathRef,
    ) -> TraverserResult<Option<bool>> {
        let (values, negate) = match self {
            P::WithIn(values) => (values, false),
            P::WithOut(values) => (values, true),
            P::Eq(_) | P::Neq(_) | P::Lt(_) | P::Gt(_) | P::Lte(_) | P::Gte(_) => {
                return self.resolve_comparison_side_effect(context, arg_value, arg_path);
            }
            _ => return Ok(None),
        };
        if values.len() != 1 {
            return Ok(None);
        }
        let key = match values[0].clone().into() {
            PValue::String(key) => key,
            _ => return Ok(None),
        };
        let Some(bucket) = context.side_effect.get(key.as_str()) else {
            return Ok(None);
        };
        // Handle against handle compares by identity; anything else by materialized value. The
        // argument is materialized at most once, a handle member only when the argument is not
        // the same kind of handle (never the whole bucket per traverser when it can be avoided).
        let storage: &S = context.storage;
        let mut materialized: Option<ElementProperty> = None;
        let mut contains = false;
        for member in bucket.members() {
            if side_effect_member_matches(storage, arg_value, &member, &mut materialized)? {
                contains = true;
                break;
            }
        }
        Ok(Some(contains != negate))
    }

    /// `where(P.eq("key"))` and the other comparisons with a bare-string operand: TinkerPop
    /// resolves the string through the path labels first, then the side effects. A path label
    /// is resolved before this (in `test_where`/`test_where_by`); a side effect holding a scalar replaces the operand
    /// (compared by the usual comparability rules), a collection takes part in `eq`/`neq` as a
    /// whole value and is incomparable for the ordering operators. `Ok(None)` when the string
    /// names neither, so the literal comparison applies.
    fn resolve_comparison_side_effect<S: crate::graph::GraphStorage>(
        &self,
        context: &mut TraverserContext<'_, S>,
        arg_value: &TraverserValue<S>,
        arg_path: PathRef,
    ) -> TraverserResult<Option<bool>> {
        let operand: PValue = match self {
            P::Eq(v) | P::Neq(v) | P::Lt(v) | P::Gt(v) | P::Lte(v) | P::Gte(v) => v.clone().into(),
            _ => return Ok(None),
        };
        let PValue::String(key) = operand else {
            return Ok(None);
        };
        if context.paths.by_label(arg_path, key.as_str()).is_some() {
            return Ok(None);
        }
        let Some(bucket) = context.side_effect.get(key.as_str()) else {
            return Ok(None);
        };
        let single: Option<std::borrow::Cow<'_, TraverserValue<S>>> = match bucket {
            crate::traversal::side_effect::SideEffectValue::Value(value) => {
                Some(std::borrow::Cow::Borrowed(value))
            }
            // A reduced bucket holds the accumulated value (`withSideEffect("a", 0, Operator.sum)`).
            crate::traversal::side_effect::SideEffectValue::Reduced { value, .. } => {
                Some(std::borrow::Cow::Owned(TraverserValue::from(value.clone())))
            }
            crate::traversal::side_effect::SideEffectValue::List(_)
            | crate::traversal::side_effect::SideEffectValue::Set(_)
            | crate::traversal::side_effect::SideEffectValue::Subgraph { .. }
            | crate::traversal::side_effect::SideEffectValue::Group(_) => None,
        };
        let replaced = match single.as_deref() {
            Some(TraverserValue::Computed(ElementProperty::Null, _)) => PValue::Null,
            Some(TraverserValue::Computed(ElementProperty::Boolean(b), _)) => PValue::Boolean(*b),
            Some(TraverserValue::Computed(ElementProperty::Number(n), _)) => {
                PValue::Number(n.clone())
            }
            Some(TraverserValue::Computed(ElementProperty::String(s), _)) => {
                PValue::String(s.clone())
            }
            Some(TraverserValue::Computed(ElementProperty::Uuid(u), _)) => PValue::Uuid(*u),
            Some(TraverserValue::Computed(ElementProperty::Temporal(t), _)) => PValue::Temporal(*t),
            Some(TraverserValue::Computed(ElementProperty::Vector(v), _)) => {
                PValue::Vector(v.clone())
            }
            _ => {
                // A collection or an element takes part in `eq`/`neq` as a whole value.
                let bucket = bucket.to_traverser_value();
                let equal = match (arg_value, &bucket) {
                    (
                        TraverserValue::Vertex(_) | TraverserValue::Edge(_),
                        TraverserValue::Vertex(_) | TraverserValue::Edge(_),
                    ) => *arg_value == bucket,
                    _ => {
                        let storage: &S = context.storage;
                        arg_value.into_node_property(storage)?
                            == bucket.into_node_property(storage)?
                    }
                };
                return Ok(match self {
                    P::Eq(_) => Some(equal),
                    P::Neq(_) => Some(!equal),
                    _ => Some(false),
                });
            }
        };
        let matched = match self {
            P::Eq(_) => P::Eq(replaced).test_literal(context, arg_value)?,
            P::Neq(_) => P::Neq(replaced).test_literal(context, arg_value)?,
            P::Lt(_) => P::Lt(replaced).test_literal(context, arg_value)?,
            P::Gt(_) => P::Gt(replaced).test_literal(context, arg_value)?,
            P::Lte(_) => P::Lte(replaced).test_literal(context, arg_value)?,
            P::Gte(_) => P::Gte(replaced).test_literal(context, arg_value)?,
            _ => return Ok(None),
        };
        Ok(Some(matched))
    }

    /// Tests the predicate against a value: what `has(key, P)`, `is(P)`, `hasValue(P)`,
    /// `all(P)`/`any(P)`/`none(P)` and a `has(jpath, P)` need. Every operand is a value, as in
    /// TinkerPop's `HasContainer`/`IsStep`: a bare string never names a step label, so
    /// `as("vadas")..has("name", P.eq("vadas"))` compares the property with the string `"vadas"`
    /// (only `where(P)` resolves labels, [`test_where`](Self::test_where)). A vertex compared
    /// with a string by `eq`/`neq` compares its id.
    pub(crate) fn test<S: crate::graph::GraphStorage>(
        &self,
        context: &mut TraverserContext<'_, S>,
        arg_value: &TraverserValue<S>,
    ) -> TraverserResult<bool> {
        self.test_resolving(context, arg_value, PathRef::default(), Operands::Values)
    }

    /// The literal fallback of `where(P)`: like [`test`](Self::test), but a bare-string operand
    /// of `eq`/`neq` first names a step label on `arg_path` (`where(P.eq("a"))`).
    fn test_with_labels<S: crate::graph::GraphStorage>(
        &self,
        context: &mut TraverserContext<'_, S>,
        arg_value: &TraverserValue<S>,
        arg_path: PathRef,
    ) -> TraverserResult<bool> {
        self.test_resolving(context, arg_value, arg_path, Operands::PathLabels)
    }

    /// Tests the predicate with every string operand a plain literal: a bare string never names a
    /// step label or a vertex id. This is what `hasId(P)`/`hasLabel(P)`/`hasKey(P)` need, where
    /// the operand is data (an id, a label, a key) and a step label of the same spelling must not
    /// shadow it.
    pub(crate) fn test_literal<S: crate::graph::GraphStorage>(
        &self,
        context: &mut TraverserContext<'_, S>,
        arg_value: &TraverserValue<S>,
    ) -> TraverserResult<bool> {
        self.test_resolving(context, arg_value, PathRef::default(), Operands::Literal)
    }

    /// `true` for a predicate that holds when its operand does *not* match (`neq`, `without`,
    /// `notStartingWith`, ...). Over a set of candidates (a label set, a key set) such a
    /// predicate must hold for all of them, a positive one for any.
    pub(crate) fn is_negated(&self) -> bool {
        matches!(
            self,
            P::Neq(_)
                | P::WithOut(_)
                | P::NotStartingWith(_)
                | P::NotEndingWith(_)
                | P::NotContaining(_)
                | P::NotRegex(_)
        )
    }

    /// Tests the predicate over a *set* of string candidates (a vertex label set, a key set) with
    /// every operand literal. A leaf holds when any candidate satisfies it, a negated leaf
    /// (`neq`, `without`, `notX`) only when all do; an empty set holds only for a negated leaf.
    /// A composite combines the set results of its children: `and` is `&&`, `or` is `||`, `not`
    /// is `!` (short-circuiting left to right).
    pub(crate) fn test_set<S: crate::graph::GraphStorage>(
        &self,
        context: &mut TraverserContext<'_, S>,
        candidates: &[&str],
    ) -> TraverserResult<bool> {
        match self {
            P::And(a, b) => {
                Ok(a.test_set(context, candidates)? && b.test_set(context, candidates)?)
            }
            P::Or(a, b) => Ok(a.test_set(context, candidates)? || b.test_set(context, candidates)?),
            P::Not(inner) => Ok(!inner.test_set(context, candidates)?),
            _ => {
                let negated = self.is_negated();
                for candidate in candidates {
                    let value = TraverserValue::Computed(
                        ElementProperty::String(CompactString::new(candidate)),
                        None,
                    );
                    let holds = self.test_literal(context, &value)?;
                    if holds != negated {
                        return Ok(holds);
                    }
                }
                Ok(negated)
            }
        }
    }

    /// The string a text predicate (`startingWith`, `endingWith`, `containing` and their
    /// negations) tests, or `None` when the value is not a string.
    ///
    /// TinkerPop's `TextP` only matches a `String`: any other value (a number, a uuid, a list, a
    /// map, a path, a vertex or an edge) makes the predicate `false` (and its negation `true`),
    /// never an error. A property handle or a path property element reads its stored value.
    fn text_operand<'ctx, S: crate::graph::GraphStorage>(
        context: &'ctx TraverserContext<'_, S>,
        arg_value: &'ctx TraverserValue<S>,
    ) -> TraverserResult<Option<std::borrow::Cow<'ctx, str>>> {
        match arg_value {
            TraverserValue::Computed(_, _)
            | TraverserValue::VertexProperty(_, _, _)
            | TraverserValue::EdgeProperty(_, _, _) => Traverser::as_string_ref(context, arg_value),
            TraverserValue::PathProperty(prop) => Ok(prop.1.text()),
            TraverserValue::Vertex(_)
            | TraverserValue::Edge(_)
            | TraverserValue::Path(_)
            | TraverserValue::Record { .. }
            | TraverserValue::Object(_)
            | TraverserValue::Entry(_)
            | TraverserValue::Array(_)
            | TraverserValue::Schema(_)
            | TraverserValue::Graph(_) => Ok(None),
        }
    }

    fn test_resolving<S: crate::graph::GraphStorage>(
        &self,
        context: &mut TraverserContext<'_, S>,
        arg_value: &TraverserValue<S>,
        arg_path: PathRef,
        operands: Operands,
    ) -> TraverserResult<bool> {
        match self {
            // `compare_value.clone()` is a `PValue` clone: cheap (a stack copy, no allocator
            // call) for the common `PValue::String` case since `PValue::String` is backed by a
            // `CompactString`, which is why this still clones on every call instead of borrowing
            // the comparand fixed at construction time (see `PValue::String`'s doc comment).
            P::Eq(compare_value) => {
                let expected: PValue = compare_value.clone().into();
                if let PValue::String(label) = &expected
                    && operands != Operands::Literal
                {
                    // Under `where(P)` a bare string first names a step label
                    // (`where(P.eq("a"))`); then (also for `is(P)`) a vertex id; then it is a
                    // plain string value.
                    if operands == Operands::PathLabels
                        && let Some(labelled) = context.paths.by_label(arg_path, label.as_str())
                    {
                        return Ok(arg_value.eq(&labelled));
                    }
                    if let TraverserValue::Vertex(node_index) = arg_value {
                        let id = crate::graph::storage::stored_vertex_id(
                            &*context.storage,
                            *node_index,
                        )?;
                        return Ok(id == label.as_str());
                    }
                }
                Ok(Self::compare(context, arg_value, &expected)? == Some(Ordering::Equal))
            }

            P::Neq(compare_value) => Ok(!P::Eq(compare_value.clone().into())
                .test_resolving(context, arg_value, arg_path, operands)?),

            P::Lt(expected) => Ok(Self::compare(context, arg_value, &expected.clone().into())?
                == Some(Ordering::Less)),

            P::Gt(expected) => Ok(Self::compare(context, arg_value, &expected.clone().into())?
                == Some(Ordering::Greater)),

            P::Lte(expected) => Ok(matches!(
                Self::compare(context, arg_value, &expected.clone().into())?,
                Some(Ordering::Less | Ordering::Equal)
            )),

            P::Gte(expected) => Ok(matches!(
                Self::compare(context, arg_value, &expected.clone().into())?,
                Some(Ordering::Greater | Ordering::Equal)
            )),

            P::WithIn(expected) => {
                for candidate in expected {
                    if Self::compare(context, arg_value, &candidate.clone().into())?
                        == Some(Ordering::Equal)
                    {
                        return Ok(true);
                    }
                }
                Ok(false)
            }

            P::WithOut(compare_value) => Ok(!P::WithIn(compare_value.clone())
                .test_resolving(context, arg_value, arg_path, operands)?),

            P::Inside(low, high) => Ok(Self::compare(context, arg_value, &low.clone().into())?
                == Some(Ordering::Greater)
                && Self::compare(context, arg_value, &high.clone().into())?
                    == Some(Ordering::Less)),

            P::Outside(low, high) => Ok(Self::compare(context, arg_value, &low.clone().into())?
                == Some(Ordering::Less)
                || Self::compare(context, arg_value, &high.clone().into())?
                    == Some(Ordering::Greater)),

            P::Between(low, high) => {
                Ok(matches!(
                    Self::compare(context, arg_value, &low.clone().into())?,
                    Some(Ordering::Greater | Ordering::Equal)
                ) && Self::compare(context, arg_value, &high.clone().into())?
                    == Some(Ordering::Less))
            }

            P::StartingWith(value) => match value.clone().into() {
                PValue::String(value) => Ok(Self::text_operand(context, arg_value)?
                    .is_some_and(|a_v| a_v.starts_with(value.as_str()))),
                PValue::Number(_)
                | PValue::Boolean(_)
                | PValue::Uuid(_)
                | PValue::Temporal(_)
                | PValue::Vector(_)
                | PValue::Null => Ok(false),
            },

            P::NotStartingWith(value) => Ok(!P::StartingWith(value.clone().into())
                .test_resolving(context, arg_value, arg_path, operands)?),

            P::EndingWith(value) => match value.clone().into() {
                PValue::String(value) => Ok(Self::text_operand(context, arg_value)?
                    .is_some_and(|a_v| a_v.ends_with(value.as_str()))),
                PValue::Number(_)
                | PValue::Boolean(_)
                | PValue::Uuid(_)
                | PValue::Temporal(_)
                | PValue::Vector(_)
                | PValue::Null => Ok(false),
            },

            P::NotEndingWith(value) => Ok(!P::EndingWith(value.clone().into())
                .test_resolving(context, arg_value, arg_path, operands)?),

            P::Containing(value) => match value.clone().into() {
                PValue::String(value) => Ok(Self::text_operand(context, arg_value)?
                    .is_some_and(|a_v| a_v.contains(value.as_str()))),
                PValue::Number(_)
                | PValue::Boolean(_)
                | PValue::Uuid(_)
                | PValue::Temporal(_)
                | PValue::Vector(_)
                | PValue::Null => Ok(false),
            },

            P::NotContaining(value) => Ok(!P::Containing(value.clone().into())
                .test_resolving(context, arg_value, arg_path, operands)?),

            P::Regex(regex) => {
                Ok(Self::text_operand(context, arg_value)?
                    .is_some_and(|text| regex.is_match(&text)))
            }

            P::NotRegex(regex) => {
                Ok(!Self::text_operand(context, arg_value)?
                    .is_some_and(|text| regex.is_match(&text)))
            }

            P::TypeOf(target) => Self::test_type_of(context, arg_value, target),

            P::And(a, b) => Ok(a.test_resolving(context, arg_value, arg_path, operands)?
                && b.test_resolving(context, arg_value, arg_path, operands)?),

            P::Or(a, b) => Ok(a.test_resolving(context, arg_value, arg_path, operands)?
                || b.test_resolving(context, arg_value, arg_path, operands)?),

            P::Not(inner) => Ok(!inner.test_resolving(context, arg_value, arg_path, operands)?),
        }
    }

    /// Orders the stream value against a predicate operand by TinkerPop's comparability rules
    /// (`semantics/Comparability.feature`); `None` means incomparable, so every test but `neq`
    /// is false.
    ///
    /// Numbers compare by value across `int64`/`float64`; `NaN` is incomparable with everything
    /// (itself included); strings, booleans, UUIDs and each temporal type compare with their own
    /// kind only; `null`
    /// compares only with `null`. A graph element, path or container is incomparable with every
    /// operand (never an error). String content is never read as a number (rule 7). The stream
    /// value is borrowed from storage, nothing is cloned.
    ///
    /// **Deviations from TinkerPop 3.8.2** (user's guide: "Predicates: Comparison and
    /// Resolution"): an incomparable pair is `false` for every test but `neq`, never an error (so
    /// `where(111)` on a vertex yields false instead of raising); an element, path or container
    /// compared with a number is false; a string is never read as a number or a date.
    fn compare<S: crate::graph::GraphStorage>(
        context: &TraverserContext<'_, S>,
        actual: &TraverserValue<S>,
        expected: &PValue,
    ) -> TraverserResult<Option<Ordering>> {
        let Some(actual) = Self::read_scalar(context, actual)? else {
            return Ok(None);
        };
        Ok(match (&actual, expected) {
            (Scalar::Number(a), PValue::Number(b)) => crate::value::scalar::compare_numbers(a, b),
            (Scalar::Str(a), PValue::String(b)) => Some(a.as_ref().cmp(b.as_str())),
            (Scalar::Bool(a), PValue::Boolean(b)) => Some(a.cmp(b)),
            (Scalar::Uuid(a), PValue::Uuid(b)) => Some(a.cmp(b)),
            // Temporal values compare within one temporal type only (TinkerPop compares dates
            // with dates); a `datetime` by its instant.
            (Scalar::Temporal(a), PValue::Temporal(b)) if a.kind() == b.kind() => Some(a.cmp(b)),
            // Vectors with vectors only (a Graphersal extension), by their total order.
            (Scalar::Vector(a), PValue::Vector(b)) => Some((*a).cmp(b)),
            (Scalar::Null, PValue::Null) => Some(Ordering::Equal),
            _ => None,
        })
    }

    /// Reads the scalar a stream value holds, in place. `None` for anything that is not a
    /// property value (element, path, container, schema, entry). An unset property reads as
    /// `null`, as [`is_null`](crate::traversal::step::local_scope::is_null) does.
    fn read_scalar<'c, S: crate::graph::GraphStorage>(
        context: &'c TraverserContext<'_, S>,
        value: &'c TraverserValue<S>,
    ) -> TraverserResult<Option<Scalar<'c>>> {
        Ok(match value {
            TraverserValue::Computed(property, _) => Scalar::of(property),
            TraverserValue::PathProperty(prop) => Scalar::of(&prop.1),
            TraverserValue::VertexProperty(prop, vertex_index, kind) => {
                if *kind == crate::value::PropertyHandleKind::Label {
                    context
                        .storage
                        .vertex_label(*vertex_index)
                        .map(|l| Scalar::Str(Cow::Borrowed(l)))
                } else if *kind == crate::value::PropertyHandleKind::Id {
                    let id =
                        crate::graph::storage::stored_vertex_id(&*context.storage, *vertex_index)?;
                    Some(Scalar::Str(Cow::Borrowed(id)))
                } else if !kind.reads_property() {
                    None
                } else {
                    match context
                        .storage
                        .vertex_properties(*vertex_index)
                        .and_then(|p| p.get(prop.as_str()))
                    {
                        Some(property) => Scalar::of(property),
                        None => Some(Scalar::Null),
                    }
                }
            }
            TraverserValue::EdgeProperty(prop, edge_index, kind) => {
                if *kind == crate::value::PropertyHandleKind::Label {
                    context
                        .storage
                        .edge_label(*edge_index)
                        .map(|l| Scalar::Str(Cow::Borrowed(l)))
                } else if *kind == crate::value::PropertyHandleKind::Id {
                    let id = crate::graph::storage::stored_edge_id(&*context.storage, *edge_index)?;
                    Some(Scalar::Str(Cow::Borrowed(id)))
                } else if !kind.reads_property() {
                    None
                } else {
                    match context
                        .storage
                        .edge_properties(*edge_index)
                        .and_then(|p| p.get(prop.as_str()))
                    {
                        Some(property) => Scalar::of(property),
                        None => Some(Scalar::Null),
                    }
                }
            }
            TraverserValue::Vertex(_)
            | TraverserValue::Edge(_)
            | TraverserValue::Path(_)
            | TraverserValue::Object(_)
            | TraverserValue::Record { .. }
            | TraverserValue::Array(_)
            | TraverserValue::Schema(_)
            | TraverserValue::Graph(_)
            | TraverserValue::Entry(_) => None,
        })
    }

    /// Evaluates `typeOf()` against a stream value.
    ///
    /// Lazy by construction (`AGENTS.md` rule 9): a property's type is read straight from
    /// storage, and only an array or a map — the two shapes whose type depends on their
    /// contents — is ever materialized, and only when the token names a container type.
    fn test_type_of<S: crate::graph::GraphStorage>(
        context: &mut TraverserContext<'_, S>,
        arg_value: &TraverserValue<S>,
        target: &GType,
    ) -> TraverserResult<bool> {
        // A graph element is not a property value, so it answers only to its own token: a vertex
        // is `GType::VERTEX`, never `GType::MAP`, even though its properties form a map.
        if let GType::Stream(kind) = target {
            return Ok(matches!(
                (kind, arg_value),
                (StreamKind::Vertex, TraverserValue::Vertex(_))
                    | (StreamKind::Edge, TraverserValue::Edge(_))
                    | (StreamKind::Path, TraverserValue::Path(_))
                    | (StreamKind::Graph, TraverserValue::Graph(_))
            ));
        }

        match arg_value {
            // A graph element carried as a value has no `ValueType`, so it matches no
            // `GType::Value(..)` token.
            TraverserValue::Computed(value, _) => {
                Ok(type_of(value).is_some_and(|observed| target.matches(&observed)))
            }

            TraverserValue::PathProperty(prop) => {
                Ok(type_of(&prop.1).is_some_and(|observed| target.matches(&observed)))
            }

            TraverserValue::VertexProperty(prop, vertex_index, kind) => {
                // `id` and `label` are rendered as strings by every step that reads them.
                if matches!(
                    kind,
                    crate::value::PropertyHandleKind::Id | crate::value::PropertyHandleKind::Label
                ) {
                    return Ok(target.matches(&ValueType::String));
                }
                if !kind.reads_property() {
                    let value = arg_value.into_node_property(context.storage)?;
                    return Ok(type_of(&value).is_some_and(|observed| target.matches(&observed)));
                }
                let observed = context
                    .storage
                    .vertex_properties(*vertex_index)
                    .and_then(|p| p.get(prop.as_str()))
                    .map_or(Some(ValueType::Null), type_of);
                Ok(observed.is_some_and(|observed| target.matches(&observed)))
            }

            TraverserValue::EdgeProperty(prop, edge_index, kind) => {
                if matches!(
                    kind,
                    crate::value::PropertyHandleKind::Id | crate::value::PropertyHandleKind::Label
                ) {
                    return Ok(target.matches(&ValueType::String));
                }
                if !kind.reads_property() {
                    let value = arg_value.into_node_property(context.storage)?;
                    return Ok(type_of(&value).is_some_and(|observed| target.matches(&observed)));
                }
                let observed = context
                    .storage
                    .edge_properties(*edge_index)
                    .and_then(|p| p.get(prop.as_str()))
                    .map_or(Some(ValueType::Null), type_of);
                Ok(observed.is_some_and(|observed| target.matches(&observed)))
            }

            TraverserValue::Array(_) => match target {
                GType::List => Ok(true),
                GType::Value(ValueType::Array(_)) => {
                    let value = arg_value.into_node_property(context.storage)?;
                    Ok(type_of(&value).is_some_and(|observed| target.matches(&observed)))
                }
                _ => Ok(false),
            },

            TraverserValue::Object(_) | TraverserValue::Record { .. } => match target {
                GType::Map => Ok(true),
                GType::Value(ValueType::Object) => {
                    let value = arg_value.into_node_property(context.storage)?;
                    Ok(type_of(&value).is_some_and(|observed| target.matches(&observed)))
                }
                _ => Ok(false),
            },

            // A map entry (TinkerPop `Map.Entry`) is not itself a map.
            TraverserValue::Vertex(_)
            | TraverserValue::Edge(_)
            | TraverserValue::Path(_)
            | TraverserValue::Entry(_)
            | TraverserValue::Schema(_)
            | TraverserValue::Graph(_) => Ok(false),
        }
    }
}

impl From<PValue> for P<PValue> {
    fn from(value: PValue) -> Self {
        P::Eq(value)
    }
}

impl From<&PValue> for P<PValue> {
    fn from(value: &PValue) -> Self {
        P::Eq(value.clone())
    }
}

impl From<Vec<PValue>> for P<PValue> {
    fn from(value: Vec<PValue>) -> Self {
        P::WithIn(value)
    }
}

impl From<&'static str> for P<PValue> {
    fn from(value: &str) -> Self {
        P::Eq(value.into())
    }
}

impl From<Vec<&'static str>> for P<PValue> {
    fn from(value: Vec<&str>) -> Self {
        P::WithIn(value.into_iter().map(|r| r.into()).collect())
    }
}

impl From<&str> for PValue {
    fn from(value: &str) -> Self {
        PValue::String(CompactString::new(value))
    }
}

impl From<String> for PValue {
    fn from(value: String) -> Self {
        PValue::String(value.into())
    }
}

impl From<&String> for PValue {
    fn from(value: &String) -> Self {
        PValue::String(CompactString::new(value))
    }
}

impl From<usize> for PValue {
    fn from(value: usize) -> Self {
        PValue::Number(value.into())
    }
}

impl From<i32> for PValue {
    fn from(value: i32) -> Self {
        PValue::Number(value.into())
    }
}

impl From<i64> for PValue {
    fn from(value: i64) -> Self {
        PValue::Number(value.into())
    }
}

impl From<f64> for PValue {
    fn from(value: f64) -> Self {
        PValue::Number(value.into())
    }
}

impl From<bool> for PValue {
    fn from(value: bool) -> Self {
        PValue::Boolean(value)
    }
}

impl From<crate::value::Vector> for PValue {
    fn from(value: crate::value::Vector) -> Self {
        PValue::Vector(value)
    }
}

impl From<Temporal> for PValue {
    fn from(value: Temporal) -> Self {
        PValue::Temporal(value)
    }
}

macro_rules! temporal_pvalue {
    ($($ty:ident),*) => {$(
        impl From<crate::value::temporal::$ty> for PValue {
            fn from(value: crate::value::temporal::$ty) -> Self {
                PValue::Temporal(value.into())
            }
        }
    )*};
}

temporal_pvalue!(Date, Time, LocalDateTime, DateTime, Duration);

impl From<bool> for P<PValue> {
    fn from(value: bool) -> Self {
        P::Eq(PValue::Boolean(value))
    }
}

impl From<usize> for P<PValue> {
    fn from(value: usize) -> Self {
        P::Eq(PValue::Number(value.into()))
    }
}

impl From<i32> for P<PValue> {
    fn from(value: i32) -> Self {
        P::Eq(PValue::Number(value.into()))
    }
}

impl From<i64> for P<PValue> {
    fn from(value: i64) -> Self {
        P::Eq(PValue::Number(value.into()))
    }
}

impl From<f64> for P<PValue> {
    fn from(value: f64) -> Self {
        P::Eq(PValue::Number(value.into()))
    }
}

/// Whether one side-effect member equals the `where(within(..))` argument: two element handles
/// by identity, two plain values directly, anything else by materialized value (the argument's
/// snapshot is computed once into `materialized`).
fn side_effect_member_matches<S: crate::graph::GraphStorage>(
    storage: &S,
    arg: &TraverserValue<S>,
    member: &TraverserValue<S>,
    materialized: &mut Option<ElementProperty>,
) -> TraverserResult<bool> {
    match (arg, member) {
        (TraverserValue::Computed(a, _), TraverserValue::Computed(m, _)) => return Ok(a == m),
        (TraverserValue::Vertex(_), TraverserValue::Vertex(_))
        | (TraverserValue::Edge(_), TraverserValue::Edge(_))
        | (TraverserValue::VertexProperty(..), TraverserValue::VertexProperty(..))
        | (TraverserValue::EdgeProperty(..), TraverserValue::EdgeProperty(..)) => {
            return Ok(arg == member);
        }
        (TraverserValue::Vertex(_), TraverserValue::Edge(_))
        | (TraverserValue::Edge(_), TraverserValue::Vertex(_)) => return Ok(false),
        _ => {}
    }
    if materialized.is_none() {
        *materialized = Some(arg.into_node_property(storage)?);
    }
    let member = member.into_node_property(storage)?;
    Ok(materialized.as_ref() == Some(&member))
}
