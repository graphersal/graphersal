use crate::graph::{GraphNodeType, GraphStorage};
use crate::value::TraverserValue;

/// Diagnostic trait providing actionable advice for errors.
pub trait DiagnosticError: std::error::Error {
    /// Returns actionable advice or recommendations to fix the error.
    fn help(&self) -> Option<String>;

    /// Returns labelled detail lines rendered underneath the error message, such as the
    /// offending value or the graph element it was read from. Values may span several lines.
    fn details(&self) -> Vec<(String, String)> {
        Vec::new()
    }

    /// Renders the error message, its details and the cause chain, excluding help guidance.
    fn diagnostic_body(&self) -> String {
        let mut buf = format!("Error: {self}");
        push_details(&mut buf, &self.details());
        let mut current: Option<&dyn std::error::Error> = self.source();
        while let Some(src) = current {
            buf.push_str(&format!("\nCaused by: {src}"));
            current = src.source();
        }
        buf
    }
}

/// Appends labelled detail lines, aligning continuation lines under the first one so that
/// multi-line values (such as a plan and its caret marker) keep their relative offsets.
pub(crate) fn push_details(buf: &mut String, details: &[(String, String)]) {
    if details.is_empty() {
        return;
    }
    let width = details
        .iter()
        .map(|(key, _)| key.chars().count())
        .max()
        .unwrap_or(0);
    for (key, value) in details {
        let prefix = format!("  {key}:{} ", " ".repeat(width - key.chars().count()));
        let padding = " ".repeat(prefix.chars().count());
        for (idx, line) in value.lines().enumerate() {
            if idx == 0 {
                buf.push_str(&format!("\n{prefix}{line}"));
            } else {
                buf.push_str(&format!("\n{padding}{line}"));
            }
        }
    }
}

/// Formats a `DiagnosticError` into a multi-line string containing the main error message,
/// cause chain (if any), and actionable help guidance.
pub fn format_diagnostic<E>(err: &E) -> String
where
    E: DiagnosticError + ?Sized,
{
    let mut buf = err.diagnostic_body();
    if let Some(help) = err.help() {
        for (idx, line) in help.lines().enumerate() {
            if idx == 0 {
                buf.push_str(&format!("\nHelp: {line}"));
            } else {
                buf.push_str(&format!("\n      {line}"));
            }
        }
    }
    buf
}

/// Provenance of a value that triggered an error: the graph element it was read from.
///
/// Carried by value errors so that diagnostics can name the exact vertex, edge and property
/// responsible for a failure instead of reporting only the mismatched type.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ValueOrigin {
    /// Element kind the value belongs to: `vertex` or `edge`.
    pub element_type: &'static str,
    /// User-facing element identifier, when the element is still present in storage.
    pub element_id: Option<String>,
    /// Element label, when known.
    pub element_label: Option<String>,
    /// Property key the value was read from, when the value is a property.
    pub property: Option<String>,
}

impl ValueOrigin {
    /// Builds an origin pointing at a vertex, optionally at one of its properties.
    pub fn vertex(id: Option<String>, label: Option<String>, property: Option<String>) -> Self {
        Self {
            element_type: "vertex",
            element_id: id,
            element_label: label,
            property,
        }
    }

    /// Builds an origin pointing at an edge, optionally at one of its properties.
    pub fn edge(id: Option<String>, label: Option<String>, property: Option<String>) -> Self {
        Self {
            element_type: "edge",
            element_id: id,
            element_label: label,
            property,
        }
    }
}

impl ValueOrigin {
    /// Renders only the element part of the origin, without the property key.
    pub fn element(&self) -> String {
        let mut rendered = self.element_type.to_string();
        if let Some(id) = &self.element_id {
            rendered.push_str(&format!(" id=\"{id}\""));
        }
        if let Some(label) = &self.element_label {
            rendered.push_str(&format!(" label=\"{label}\""));
        }
        rendered
    }
}

impl std::fmt::Display for ValueOrigin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.element_type)?;
        if let Some(id) = &self.element_id {
            write!(f, " id=\"{id}\"")?;
        }
        if let Some(label) = &self.element_label {
            write!(f, " label=\"{label}\"")?;
        }
        if let Some(property) = &self.property {
            write!(f, " property=\"{property}\"")?;
        }
        Ok(())
    }
}

/// One traversal level of a step trace: the optimized physical plan of that level together
/// with the position of the step that failed inside it.
///
/// Nested traversals (`where_t()`, `not()`, `by()` sub-traversals) produce one frame per
/// level, so a failure is addressed by a path such as `#2.0` rather than a flat index.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct StepFrame {
    /// Rendered steps of the executed plan for this level, in optimized physical order.
    pub steps: Vec<String>,
    /// Position of the failing step within `steps`.
    pub position: usize,
    /// The same level as the user wrote it, when the optimizer tracked provenance.
    pub(crate) logical: Option<LogicalFrame>,
}

/// A traversal level as written by the user, with the steps a failure originates from.
///
/// Rule-based optimization reorders and folds steps, so one executed step can trace back to
/// several written ones: `v().has_label("person").count()` runs as a single fused scan.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub(crate) struct LogicalFrame {
    /// Rendered steps of the level before optimization.
    pub steps: Vec<String>,
    /// Positions in `steps` the failing step originates from, in ascending order.
    pub positions: Vec<usize>,
}

/// Renders a dotted step chain with a caret line underlining the marked steps.
fn render_marked(steps: &[String], marked: &[usize]) -> String {
    let plan = steps.join(".");
    if marked.is_empty() {
        return plan;
    }
    let mut caret = String::new();
    let mut column = 0;
    for (index, step) in steps.iter().enumerate() {
        let width = step.chars().count();
        if marked.contains(&index) {
            // Marked neighbours are underlined as one run, separator included.
            let start = if index > 0 && marked.contains(&(index - 1)) {
                column - 1
            } else {
                column
            };
            while caret.chars().count() < start {
                caret.push(' ');
            }
            while caret.chars().count() < column + width {
                caret.push('^');
            }
        }
        column += width + 1;
    }
    if caret.trim().is_empty() {
        return plan;
    }
    format!("{plan}\n{caret}")
}

impl StepFrame {
    /// Builds a frame from the rendered steps of one traversal level.
    pub fn new(steps: Vec<String>, position: usize) -> Self {
        Self {
            steps,
            position,
            logical: None,
        }
    }

    /// Attaches the level as the user wrote it, for reporting against the original query.
    pub(crate) fn with_logical(mut self, logical: LogicalFrame) -> Self {
        self.logical = Some(logical);
        self
    }

    /// Returns the name of the failing step, when the position is inside the plan.
    pub fn step_name(&self) -> Option<&str> {
        self.steps.get(self.position).map(|s| s.as_str())
    }

    /// Renders the plan of this level without the caret line.
    pub fn plan(&self) -> String {
        self.steps.join(".")
    }

    /// Renders the executed plan of this level followed by a caret line marking the failing step.
    pub fn render(&self) -> String {
        render_marked(&self.steps, &[self.position])
    }

    /// Renders the level as written by the user, marking the steps the failure originates from.
    ///
    /// Returns `None` when provenance is unavailable, or when optimization left this level
    /// unchanged and the executed plan already shows the user their own query.
    pub fn render_logical(&self) -> Option<String> {
        let logical = self.logical.as_ref()?;
        if logical.positions.is_empty() || logical.steps == self.steps {
            return None;
        }
        Some(render_marked(&logical.steps, &logical.positions))
    }
}

/// The id segments one `StepFailed` level contributes: its child index (when it is a child
/// traversal of the enclosing step) and its step index (`?` when unknown).
fn level_segments(child_index: Option<usize>, step_index: Option<usize>) -> Vec<String> {
    let mut segments = Vec::with_capacity(2);
    if let Some(child) = child_index {
        segments.push(child.to_string());
    }
    segments.push(
        step_index
            .map(|index| index.to_string())
            .unwrap_or_else(|| "?".to_string()),
    );
    segments
}

/// Renders the step id of a failure (`#3.1.0`, the numbering of `Metrics::id`), walking nested
/// step frames.
fn step_path(child: &Option<usize>, index: &Option<usize>, source: &TraverserError) -> String {
    let mut segments: Vec<String> = level_segments(*child, *index);
    let mut current = source;
    while let TraverserError::StepFailed {
        child_index,
        step_index,
        source,
        ..
    } = current
    {
        segments.extend(level_segments(*child_index, *step_index));
        current = source;
    }
    if segments.iter().all(|s| s == "?") {
        return String::new();
    }
    format!("#{} ", segments.join("."))
}

/// The message of a [`TraverserError::StepFailed`]: `Step #2 'max()' execution failed`. A GQL
/// query refused by its `gql:error` marker leads with the GQL error itself, since the marker is
/// no step the user wrote and the query never ran.
fn step_failed_message(
    child_index: &Option<usize>,
    step_index: &Option<usize>,
    step_name: &str,
    element_id: &Option<String>,
    source: &TraverserError,
) -> String {
    let path = step_path(child_index, step_index, source);
    let name = innermost_step_name(step_name, source);
    #[cfg(feature = "gql")]
    if let Some(gql) = gql_marker_cause(&name, source) {
        return format!("{gql} (step {path}'{name}' refused the query before the traversal ran)");
    }
    let element = element_id
        .as_ref()
        .map(|id| format!(" for element ID '{id}'"))
        .unwrap_or_default();
    format!("Step {path}'{name}' execution failed{element}")
}

/// The GQL error a `gql:error` marker raised, when `name` is the marker and `source` leads to
/// that error: its diagnostic leads with the GQL error and omits the `Caused by:` line.
#[cfg(feature = "gql")]
fn gql_marker_cause<'e>(name: &str, source: &'e TraverserError) -> Option<&'e TraverserError> {
    let root = source.root_cause();
    (name.starts_with("gql:error(") && matches!(root, TraverserError::Gql { .. })).then_some(root)
}

/// Returns the name of the innermost failing step of a nested step chain.
fn innermost_step_name(step_name: &str, source: &TraverserError) -> String {
    match source {
        TraverserError::StepFailed {
            spelling: crate::traversal::Spelling::Snake,
            step_name: inner,
            source,
            ..
        } => innermost_step_name(inner, source),
        _ => step_name.to_string(),
    }
}

/// Errors related to value operations, properties, types, and casting.
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum ValueError {
    /// A value failed to convert to the type a step required.
    #[error("Cast exception: expected type [{expected}], got type '{actual}'")]
    #[non_exhaustive]
    CastException {
        /// Type the step required.
        expected: String,
        /// Type the value actually had.
        actual: String,
        /// Rendered offending value, when it could be captured at the failure site.
        value: Option<String>,
        /// Graph element the offending value was read from, when it came from storage.
        ///
        /// Boxed so that the provenance, needed only to render a diagnostic, does not inflate
        /// every `Result` that carries a [`ValueError`].
        origin: Option<Box<ValueOrigin>>,
    },

    /// `min()`/`max()` met a value they cannot order against the others: a value of another
    /// kind than the one already seen (a number after a string), or a graph element, path or
    /// container. Numbers compare with numbers, text with text.
    #[error("Step '{step}' cannot order a {actual} value among {expected} values")]
    #[non_exhaustive]
    IncomparableValues {
        /// The failing step's name (`min` or `max`).
        step: &'static str,
        /// The kind(s) the step can order at this point, e.g. `number`.
        expected: String,
        /// The kind of the offending value.
        actual: String,
        /// Rendered offending value, when it could be captured at the failure site.
        value: Option<String>,
        /// Graph element the offending value was read from, when it came from storage.
        ///
        /// Boxed for the same reason as [`ValueError::CastException`]'s `origin`.
        origin: Option<Box<ValueOrigin>>,
    },

    /// A requested property does not exist on the element.
    #[error("Property '{key}' not found on element type '{element_type}'")]
    #[non_exhaustive]
    PropertyNotFound {
        /// The missing property's key.
        key: String,
        /// `"vertex"` or `"edge"`.
        element_type: &'static str,
    },

    /// A step reads a step label (`select("a")`, `where(P.eq("a"))`, `math("a + 1")`, ...) that
    /// no `as()` and no keyed side effect (`aggregate`/`store`/`group`/`tree`/
    /// `with_side_effect`) declares anywhere in the query: a plan-time error, raised before
    /// execution instead of silently returning nothing.
    #[error("Step label '{label}' is never declared in this query")]
    #[non_exhaustive]
    UndeclaredLabel {
        /// The label no step declares.
        label: String,
    },

    /// An operator was applied to two incompatible types.
    #[error("Invalid operation '{op}' between types '{type_a}' and '{type_b}'")]
    #[non_exhaustive]
    InvalidTypeOperation {
        /// The operator's name.
        op: &'static str,
        /// The left operand's type.
        type_a: &'static str,
        /// The right operand's type.
        type_b: &'static str,
    },

    /// `path()` was read on a traverser that never recorded one.
    #[error("Path is not defined or initialized for traverser")]
    PathNotDefined,

    /// A numeric value or conversion was invalid.
    #[error("Invalid numeric value or conversion: {reason}")]
    #[non_exhaustive]
    InvalidNumber {
        /// Why the value/conversion was invalid.
        reason: String,
    },

    /// A cast/coercion target type is not supported.
    #[error("Unsupported cast target '{target}'")]
    #[non_exhaustive]
    UnsupportedCastTarget {
        /// Rendered conversion target that cannot be produced.
        target: String,
        /// Why the target cannot be produced yet.
        reason: &'static str,
    },

    /// A `Scope.local` step that transforms each element of the list a traverser holds met an
    /// element it cannot take — for example a number in the list of `trim(Scope.local)`.
    ///
    /// The message carries TinkerPop's wording (`The trim(local) step can only take string or
    /// list of strings`), so a query ported from Gremlin fails with the text it expects.
    #[error(
        "Cast exception: The {step}(local) step can only take {accepts}, but element {index} of \
         the list has type '{actual}'"
    )]
    #[non_exhaustive]
    LocalElementType {
        /// The step's Gremlin name (`trim`, `toUpper`, `asString`, ...).
        step: &'static str,
        /// What the step accepts, in TinkerPop's words (`string or list of strings`).
        accepts: &'static str,
        /// 0-based position of the offending element in the list.
        index: usize,
        /// Type of the offending element.
        actual: String,
    },

    /// A label step met a traverser of the wrong kind: `add_label()`/`drop_label()` change the
    /// label set of a vertex, `set_label()` the label of an edge. The help names the step for
    /// the kind that arrived.
    #[error("The {step}() step takes {expected}, but got a value of type '{actual}'")]
    #[non_exhaustive]
    LabelStepElementKind {
        /// The step's name (`add_label`, `drop_label`, `set_label`).
        step: &'static str,
        /// The element kind the step takes, with its article (`a vertex` or `an edge`).
        expected: &'static str,
        /// Type of the incoming value (`edge`, `vertex`, `string`, ...).
        actual: String,
    },

    /// A list function step (`combine`, `conjoin`, `difference`, `disjunct`, `intersect`,
    /// `merge`, `product`) met an incoming value or an argument it cannot take: a `null`, a value
    /// that is not a list (or, for `merge`, not a map), or a child traversal without a result.
    #[error("The {step}() step takes a {expected} as its {operand}, but got {actual}")]
    #[non_exhaustive]
    ListFunctionInput {
        /// The step's name (`combine`, `merge`, ...).
        step: &'static str,
        /// Which input failed: `incoming value`, `argument` or `traversal argument`.
        operand: &'static str,
        /// What the step needed there: `list`, `map`, `list or map`.
        expected: &'static str,
        /// What it got (`null`, a type name, or that the traversal yielded nothing).
        actual: String,
    },

    /// A string step that combines several strings (`concat`) met an argument it cannot take:
    /// a constant that is neither a string nor `null`, or a child traversal that yields no result
    /// or a value that is not a string.
    #[error("The {step}() step takes a {expected} as its {operand}, but got {actual}")]
    #[non_exhaustive]
    StringFunctionInput {
        /// The step's name (`concat`).
        step: &'static str,
        /// Which input failed: `argument` or `traversal argument`.
        operand: &'static str,
        /// What the step needed there (`string or null`).
        expected: &'static str,
        /// What it got (a type name, or that the traversal yielded nothing).
        actual: String,
    },

    /// `sample(n).by(..)` weighs each traverser by its `by()` value, and that value was not a
    /// number of at least 0 (a string, a negative number, NaN, ...).
    #[error(
        "The sample() step weighs each traverser by its by() value, which must be a number of at \
         least 0, but got {actual}"
    )]
    #[non_exhaustive]
    SampleWeight {
        /// What the `by()` produced: a type name or the offending number.
        actual: String,
    },

    /// A JSON Path expression could not be parsed.
    #[error("Invalid JSONPath '{path}': {reason}")]
    #[non_exhaustive]
    InvalidJsonPath {
        /// The raw string the query passed.
        path: String,
        /// Why it could not be parsed.
        reason: String,
    },

    /// A `glob_path()` pattern could not be compiled.
    #[error("Invalid glob pattern '{pattern}' at position {position}: {reason}")]
    #[non_exhaustive]
    InvalidGlobPattern {
        /// The raw pattern the query passed.
        pattern: String,
        /// 0-based character (not byte) position of the problem.
        position: usize,
        /// Why it could not be compiled.
        reason: String,
    },

    /// A `TextP.regex()`/`TextP.notRegex()` expression could not be compiled.
    #[error("Invalid regular expression '{pattern}': {reason}")]
    #[non_exhaustive]
    InvalidRegex {
        /// The raw expression the query passed.
        pattern: String,
        /// Why it could not be compiled.
        reason: String,
    },

    /// `has_id(P)` (also `has(T.id, P)`) with an order predicate (`P.gt`, `P.gte`, `P.lt`,
    /// `P.lte`, `P.between`, `P.inside`, `P.outside`) on a number: element ids are strings, so
    /// the comparison could never hold. Raised at plan time, located on the step. Equality
    /// forms (`has_id(3)`, `P.eq(3)`, `P.neq(3)`, `P.within(1, 2)`, `P.without(1, 2)`, also nested
    /// in `and`/`or`/`not`) compare the number's literal text and stay valid.
    #[error("{step} compares element ids with a number, but element ids are strings")]
    #[non_exhaustive]
    NumericIdComparison {
        /// The step as written, e.g. `has_id(P.gt(3))`.
        step: String,
        /// The same step with every number operand as a string, e.g. `has_id(P.gt("3"))`.
        fix: String,
    },

    /// A TinkerPop `Operator` (`sum`, `max`, `addAll`, ...) could not combine two values.
    #[error("{}", operator_failed_message(operator, kind, left, right))]
    #[non_exhaustive]
    OperatorFailed {
        /// The operator's TinkerPop name (`sum`, `addAll`, ...).
        operator: &'static str,
        /// Why it failed.
        kind: OperatorFailure,
        /// The left operand, rendered with its type, e.g. `3 (int64)`.
        left: String,
        /// The right operand, rendered with its type.
        right: String,
    },

    /// A date step (`date_add`, `date_diff`, `date_trunc`, `at_zone`, ...) met a value it cannot
    /// take: an incoming value or an argument that is not a date or time, or a child traversal
    /// without a result.
    #[error("The {step}() step takes {expected} as its {operand}, but got {actual}")]
    #[non_exhaustive]
    TemporalInput {
        /// The step's name (`date_add`, `date_diff`, ...).
        step: &'static str,
        /// Which input failed: `incoming value`, `argument` or `traversal argument`.
        operand: &'static str,
        /// What the step needed there (`a date or time`, `a datetime`, ...).
        expected: &'static str,
        /// What it got: a type name with the value, `null`, or that the traversal yielded
        /// nothing.
        actual: String,
    },

    /// `as_date()` or `as_duration()` could not convert a value: text that is not a value of the
    /// target type, a number that is not an integer of epoch milliseconds, `null`, a list, ...
    /// The message starts like TinkerPop's (`Can't parse`).
    #[error("Can't parse {value} as a {target}: {reason}")]
    #[non_exhaustive]
    TemporalParse {
        /// The step's name (`as_date`, `as_duration`).
        step: &'static str,
        /// The target type (`datetime`, `date`, `duration`, ...).
        target: &'static str,
        /// The value, rendered with its type (`"abc" (string)`, `1.5 (float64)`, `null`).
        value: String,
        /// Why it does not convert.
        reason: String,
    },

    /// A date operation failed: a result outside the years -9999 to 9999, an unknown time
    /// zone, a unit or part the value's type does not have (hours of a `date`), or a temporal
    /// text that does not parse (`datetime("..")`).
    #[error("The {step}() step failed: {error}")]
    #[non_exhaustive]
    Temporal {
        /// The step or function's name (`date_add`, `datetime`, ...).
        step: &'static str,
        /// What went wrong.
        #[source]
        error: crate::value::temporal::TemporalError,
    },

    /// A vector could not be built or converted: an empty or too long list, a component that is
    /// not a finite number, an unknown element kind, a component beyond the range of the target
    /// kind (`vector([..])`, `as_vector()`, `quantize(..)`).
    #[error("The {step}() step failed: {error}")]
    #[non_exhaustive]
    Vector {
        /// The step or function's name (`vector`, `as_vector`, `quantize`).
        step: &'static str,
        /// What went wrong.
        #[source]
        error: crate::value::VectorError,
    },
}

/// Why an [`Operator`](crate::value::Operator) failed: the kind of a [`ValueError::OperatorFailed`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum OperatorFailure {
    /// An `int64` operation left the `i64` range (`sum`, `minus`, `mult`, `div` of `i64::MIN` by
    /// `-1`, `sumLong`). TinkerPop promotes or wraps; this engine reports an error.
    Overflow,
    /// An integer was divided by zero (`div`).
    DivisionByZero,
    /// The operands are not of the kind the operator accepts.
    #[non_exhaustive]
    IncompatibleOperands {
        /// What the operator accepts, e.g. `number`.
        expected: &'static str,
    },
}

fn operator_failed_message(
    operator: &str,
    kind: &OperatorFailure,
    left: &str,
    right: &str,
) -> String {
    match kind {
        OperatorFailure::Overflow => {
            format!(
                "Operator '{operator}' overflowed the int64 range combining {left} with {right}"
            )
        }
        OperatorFailure::DivisionByZero => {
            format!("Operator '{operator}' cannot divide {left} by {right}: division by zero")
        }
        OperatorFailure::IncompatibleOperands { expected } => {
            format!("Operator '{operator}' cannot combine {left} with {right}: expected {expected}")
        }
    }
}

fn operator_help(operator: &str, kind: &OperatorFailure) -> String {
    let mut help = match kind {
        OperatorFailure::Overflow => format!(
            "'{operator}' works on int64 and does not widen or wrap: the result is outside the int64 range. \
             Convert an operand to a float first (for example g.v().values(\"age\").as_number()) \
             or reduce the values before combining them."
        ),
        OperatorFailure::DivisionByZero => format!(
            "'{operator}' of two integers is an integer division, so a zero divisor is an error. \
             Filter the zero out first, or divide by a float (1.0 / 0.0 is infinity)."
        ),
        OperatorFailure::IncompatibleOperands { expected } => {
            format!("'{operator}' expects {expected}.")
        }
    };
    help.push_str(
        "\nOperators: sum, minus, mult and div take numbers (null as the right operand leaves the left one); \
         min and max take two numbers or two values of one kind (string, boolean, uuid); and and or take \
         booleans; sumLong takes int64 only; addAll takes two lists, two maps, or a list and a value; assign \
         accepts anything. Convert values with as_number(), as_bool() or as_string() first, for example \
         g.v().values(\"age\").as_number().",
    );
    help.push_str(
        "\nIn a sack the operand comes from by(), or is the traverser's own value without one: \
         g.with_sack(0).v().out_e().sack(Operator.sum).by(\"weight\") adds every edge weight to the \
         sack. To combine a sack with a value that is not a number yet, convert the operand, for \
         example g.with_sack(0).v().sack(Operator.sum).by(__.values(\"age\").as_number()).",
    );
    help.push_str(
        "\nfold(seed, Operator) and with_side_effect(name, initial, Operator) apply the operator to every \
         value of the stream: g.v().values(\"age\").fold(0, Operator.sum) adds the ages, and \
         g.with_side_effect(\"a\", 0, Operator.sum).v().aggregate(\"a\").by(\"age\").cap(\"a\") does the \
         same into a side effect. The seed decides the kind: 0 for sum, 1 for mult, true for and, [] for addAll.",
    );
    if operator == "normSack" {
        help.push_str(
            "\nbarrier(Barrier.normSack) scales numeric sacks (null sacks are skipped), so every \
             sack in the stream must be a number or null, for example \
             g.with_sack(1.0).v().out().barrier(Barrier.normSack).sack().",
        );
    }
    help
}

/// Names the coercion step that produces a value of the rendered type, for a runnable example
/// in a diagnostic.
///
/// Returns `None` when the expectation is not a value type at all — a step that wanted a vertex
/// is not repaired by a conversion.
fn coercion_step(expected: &str) -> Option<&'static str> {
    Some(match expected {
        "string" => "as_string()",
        "boolean" => "as_bool()",
        // `number` is the numeric family (`GType::NUMBER`) that numeric steps expect.
        "number" => "as_number(GType.LONG)",
        "int64" => "as_number(GType.LONG)",
        "float64" => "as_number(GType.DOUBLE)",
        // UUID has no dedicated `as_uuid()` step (the coercion vocabulary follows TinkerPop's
        // as_string()/as_bool()/as_number()); the generic cast() step produces it.
        "uuid" => "cast(GType.UUID)",
        _ => return None,
    })
}

/// What the coercion step named by [`coercion_step`] really converts, so the advice is not a guess.
fn coercion_note(expected: &str) -> Option<&'static str> {
    match expected {
        "boolean" => Some(
            "as_bool() turns 0 (and NaN) into false, any other number into true, and the texts \
             true/false in any letter case; any other text is an error.",
        ),
        "number" | "int64" => Some("as_number(GType.LONG) truncates a float toward zero."),
        _ => None,
    }
}

/// True for the stream kinds that hold a list: a folded stream (`array`) or a stored list value
/// (`array<...>`).
fn is_list_kind(actual: &str) -> bool {
    actual == "array" || actual.starts_with("array<")
}

/// Appended when a step that reads one value per traverser meets a whole list: `Scope.local`
/// applies the step to the list each traverser holds.
const LIST_SCOPE_LOCAL_HINT: &str = "\nThe value is a list (for example from fold()): reduce each \
     traverser's list with Scope.local, for example g.v().values(\"age\").fold().sum(Scope.local) \
     or g.v().fold().count(Scope.local), or unfold() it first. \
     A string step transforms each element of the list the same way, for example \
     g.v().values(\"name\").fold().to_upper(Scope.local). \
     To join the elements into one string use conjoin(), for example \
     g.v().values(\"name\").fold().conjoin(\", \"); to combine it with a second list use \
     combine(), merge(), intersect(), difference(), disjunct() or product(), for example \
     g.v().values(\"name\").fold().combine([\"dave\"]). reverse() reverses the list itself, for \
     example g.v().values(\"name\").fold().reverse().";

/// The snake_case DSL spelling of a Gremlin step name (`toUpper` → `to_upper`), for a runnable
/// example in a diagnostic.
fn snake_step_name(step: &str) -> String {
    let mut name = String::with_capacity(step.len() + 2);
    for c in step.chars() {
        if c.is_ascii_uppercase() {
            name.push('_');
            name.push(c.to_ascii_lowercase());
        } else {
            name.push(c);
        }
    }
    name
}

/// The `help()` text of [`ValueError::LocalElementType`].
fn local_element_help(step: &str, index: usize, actual: &str) -> String {
    let snake = snake_step_name(step);
    if step == "asString" {
        return format!(
            "{step}(local) converts each element of the list a traverser holds to a string, and \
             element {index} has type '{actual}', which has no string form.\n\
             Drop the null elements before converting, for example \
             g.v().values(\"age\").fold().unfold().not(__.is(typeOf(GType.NULL))).fold().as_string(Scope.local), \
             or convert the stream before folding it: g.v().values(\"age\").as_string().fold()."
        );
    }
    format!(
        "{step}(local) transforms each element of the list a traverser holds, so every element \
         must be a string (a null element stays null), but element {index} has type '{actual}'.\n\
         Convert the elements first, for example \
         g.v().values(\"name\").fold().unfold().as_string().fold().{snake}(Scope.local), \
         or keep only the strings with \
         g.v().values(\"name\").fold().unfold().is(typeOf(GType.STRING)).fold().{snake}(Scope.local)."
    )
}

/// A runnable call of a date step for its `help()` texts.
fn temporal_step_example(step: &str) -> &'static str {
    match step {
        "date_add" => "date_add(DT.day, 1)",
        "date_diff" => "date_diff(datetime(\"2023-08-01T00:00:00Z\"))",
        "age" => "age(datetime(\"2000-01-01T00:00:00Z\"))",
        "date_trunc" => "date_trunc(DT.month)",
        "date_part" => "date_part(DT.year)",
        "at_zone" => "at_zone(\"Europe/Berlin\")",
        "as_local" => "as_local()",
        "as_duration" => "as_duration()",
        _ => "date_add(DT.day, 1)",
    }
}

/// The `help()` text of [`ValueError::TemporalInput`].
fn temporal_input_help(step: &str, operand: &str, expected: &str, actual: &str) -> String {
    let example = temporal_step_example(step);
    if operand == "traversal argument" && actual.starts_with("no result") {
        return format!(
            "The traversal argument of {step}() runs on the incoming value and its first result \
             is the operand, so it must yield a value, for example \
             g.inject(datetime(\"2023-08-08T00:00:00Z\")).{step}(__.constant(datetime(\
             \"2023-08-01T00:00:00Z\")))."
        );
    }
    let types = "The date steps take date and time values (datetime, date, localdatetime, time; \
                 date_add() and date_trunc() also a duration). Build one with \
                 datetime(\"2023-08-02T00:00:00Z\"), date(\"2023-08-02\"), \
                 localdatetime(\"2023-08-02T09:30:00\"), time(\"09:30:00\") or duration(\"P1D\")";
    if actual.starts_with("string") {
        return format!(
            "{types}. A string is never read as a date by itself: convert it first with \
             as_date() (ISO 8601 text), for example \
             g.inject(\"2023-08-02T00:00:00Z\").as_date().{example}, or with \
             as_date(GType.DATE) (and GType.LOCALDATETIME, GType.TIME) for the other types."
        );
    }
    if actual.starts_with("int64") || actual.starts_with("float64") {
        return format!(
            "{types}. A number is not a date: as_date() reads an integer of milliseconds since \
             1970-01-01T00:00:00Z, for example g.inject(1690934400000).as_date().{example}."
        );
    }
    let specific = match step {
        "at_zone" => {
            " at_zone() places a datetime (the same instant) or a localdatetime (its wall clock) \
             in a zone; convert a date first with as_date(GType.LOCALDATETIME)."
        }
        "as_local" => {
            " as_local() reads the wall clock of a datetime (at its own offset, or in the zone \
             given)."
        }
        "date_diff" | "age" => {
            " Both operands must be of the same type; a null argument of date_diff() counts as \
             the epoch."
        }
        _ => "",
    };
    format!(
        "{types}, or convert a property with as_date(); keep only the values of one type with \
         is(P.typeOf(GType.DATETIME)) before the step.{specific} The step needed {expected} here."
    )
}

/// The `help()` text of [`ValueError::TemporalParse`].
fn temporal_parse_help(step: &str, target: &str) -> String {
    if step == "as_duration" {
        return "as_duration() reads an ISO 8601 duration such as \"P1Y2M3DT4H5M6S\", \"P2W\" or \
                \"PT1.5S\" (each part may have its own sign: \"P1M-2D\"); a duration passes \
                unchanged. Build one directly with duration(\"P1D\")."
            .to_string();
    }
    match target {
        "datetime" => "as_date() reads ISO 8601 / RFC 3339 text (\"2023-08-02T00:00:00Z\", \
                       \"2023-08-02T00:00:00-07:00\"; without an offset the text is UTC and a date \
                       alone is midnight UTC) or an integer number of milliseconds since \
                       1970-01-01T00:00:00Z, for example g.inject(1690934400000).as_date(); a \
                       date or localdatetime becomes the datetime at UTC. A float, null, a list \
                       or a map is not converted: as_number(GType.LONG) truncates a float first, \
                       and is(P.typeOf(GType.STRING)) keeps only the strings."
            .to_string(),
        "date" => "as_date(GType.DATE) reads a date text such as \"2023-08-02\", or takes the \
                   date of a datetime (at its own offset) or of a localdatetime."
            .to_string(),
        "localdatetime" => "as_date(GType.LOCALDATETIME) reads a text without an offset such as \
                            \"2023-08-02T09:30:00\" (a date alone is midnight), or takes the wall \
                            clock of a datetime (at its own offset) or a date at midnight."
            .to_string(),
        "time" => "as_date(GType.TIME) reads a time text such as \"09:30:00\", or takes the time \
                   of day of a datetime (at its own offset) or of a localdatetime."
            .to_string(),
        _ => "as_date() converts to datetime, date, localdatetime and time (as_date(GType.DATE), \
              ...); as_duration() reads durations."
            .to_string(),
    }
}

/// The `help()` text of [`ValueError::Temporal`].
fn temporal_help(step: &str, error: &crate::value::temporal::TemporalError) -> String {
    use crate::value::temporal::TemporalError;
    match error {
        TemporalError::OutOfRange { .. } => format!(
            "Dates and times cover the years -9999 to 9999 with a resolution of one \
             microsecond; {step}() left that range. Check the amount (date_add(DT.year, 10000) \
             goes too far) or keep the extreme values out with a filter such as \
             is(P.gt(datetime(\"1900-01-01T00:00:00Z\")))."
        ),
        TemporalError::UnknownZone { .. } => "A zone is an IANA name of the bundled time zone \
                                              database (\"Europe/Berlin\", \
                                              \"America/New_York\", \"UTC\"; the names are \
                                              case-sensitive) or a fixed UTC offset (\"+02:00\", \
                                              \"-07:00\", \"Z\"), for example \
                                              at_zone(\"Europe/Berlin\")."
            .to_string(),
        TemporalError::Unsupported { .. } => "Each type has its own units: a date has no hours \
                                              (convert it with as_date(GType.LOCALDATETIME) \
                                              first), a time has no days or months, a duration \
                                              has no weeks and no zone. DT.day_of_week, \
                                              DT.iso_day_of_week, DT.day_of_year, DT.iso_year, \
                                              DT.epoch and DT.offset are parts for date_part(), \
                                              not units; a zone argument applies to a datetime \
                                              only; both operands of date_diff() and age() must \
                                              be of the same type."
            .to_string(),
        TemporalError::Parse { .. } => "Write the ISO 8601 text of the type: \
                                        datetime(\"2023-08-02T00:00:00Z\") (an offset such as \
                                        +02:00 or a zone such as [Europe/Berlin]), \
                                        date(\"2023-08-02\"), \
                                        localdatetime(\"2023-08-02T09:30:00\"), \
                                        time(\"09:30:00\"), duration(\"P1DT2H\")."
            .to_string(),
    }
}

/// The `help()` text of [`ValueError::LabelStepElementKind`].
fn label_step_help(step: &str, actual: &str) -> String {
    let vertex_steps = "a vertex carries a label set, changed with add_label() and drop_label(), \
                        for example g.v().has_label(\"person\").add_label(\"employee\") or \
                        g.v(1).drop_label(\"person\")";
    let edge_step = "an edge has exactly one label, changed with set_label(), for example \
                     g.e().has_label(\"knows\").set_label(\"met\")";
    match (step, actual) {
        ("set_label", "vertex") => {
            format!("set_label() changes the label of an edge; {vertex_steps}.")
        }
        ("add_label" | "drop_label", "edge") => {
            format!("{step}() changes the label set of a vertex; {edge_step}.")
        }
        _ => format!(
            "{step}() changes the label of a graph element, not of a value of type '{actual}': \
             {vertex_steps}; {edge_step}. Keep the elements in the stream (no values(), \
             id() or label() before the step)."
        ),
    }
}

/// The `help()` text of [`ValueError::ListFunctionInput`].
fn list_function_help(step: &str, operand: &str, expected: &str, actual: &str) -> String {
    let argument = if step == "conjoin" {
        "\"-\""
    } else {
        "[\"x\"]"
    };
    match (operand, expected) {
        ("incoming value", "list or map") => format!(
            "{step}() works on the list or the map a traverser holds. Collect the stream into a \
             list with fold() first, for example g.v().values(\"name\").fold().{step}([\"x\"]), or \
             merge two maps, for example \
             g.v().project(\"name\").by(\"name\").{step}([\"other\": \"blueprint\"]) (Rhai: \
             #{{other: \"blueprint\"}})."
        ),
        ("incoming value", _) if actual == "null" => format!(
            "{step}() works on the list a traverser holds, and a null is not a list. Keep the \
             null values out before the step, for example \
             g.v().values(\"tags\").not(__.is(typeOf(GType.NULL))).{step}({argument})."
        ),
        ("incoming value", _) => format!(
            "{step}() works on the list a traverser holds (a fold(), a path(), a list-valued \
             property). Collect the stream into a list with fold() first, for example \
             g.v().values(\"name\").fold().{step}({argument})."
        ),
        ("traversal argument", _) => format!(
            "The child traversal of {step}() must yield a {expected} as its first result. End it \
             with fold() to turn its stream into a list, for example \
             g.v().values(\"name\").fold().{step}(__.v().values(\"name\").fold()); a traversal \
             that yields nothing or null cannot be used."
        ),
        (_, "map") => format!(
            "{step}() of a map merges another map into it: pass a map constant or a traversal \
             whose first result is a map, for example \
             g.v().has(\"name\", \"marko\").element_map().{step}(__.v().has(\"name\", \"lop\")\
             .element_map()); to merge lists, fold() the incoming stream into a list first."
        ),
        _ => format!(
            "The argument of {step}() is a {expected} constant such as [\"a\", \"b\"] or a child \
             traversal whose first result is a {expected}, for example \
             g.v().values(\"name\").fold().{step}(__.v().values(\"name\").fold()); null and \
             single values are not lists (wrap one value: [\"a\"])."
        ),
    }
}

/// The `help()` text of [`ValueError::StringFunctionInput`].
fn string_function_help(step: &str, operand: &str, actual: &str) -> String {
    match operand {
        "traversal argument" if actual.starts_with("no result") => format!(
            "A child traversal of {step}() must yield a value for every traverser: its first \
             result is appended. Make it always produce one, for example with coalesce(): \
             g.v().values(\"name\").{step}(__.coalesce(__.values(\"lang\"), __.constant(\"\"))), \
             or keep only the traversers it has a value for: \
             g.v().where(__.values(\"lang\")).values(\"name\").{step}(__.values(\"lang\"))."
        ),
        "traversal argument" => format!(
            "A child traversal of {step}() must yield a string (or null, which is skipped). \
             Convert its result with as_string(), for example \
             g.v().values(\"name\").{step}(\" \").{step}(__.values(\"age\").as_string()); a list \
             is joined into one string with conjoin(), for example \
             g.v().values(\"name\").{step}(__.out().values(\"name\").fold().conjoin(\",\"))."
        ),
        _ => format!(
            "The arguments of {step}() are strings (a null is skipped) or child traversals, for \
             example g.v().values(\"name\").{step}(\" is \", \"here\") or \
             g.v().values(\"name\").{step}(__.values(\"age\").as_string()); convert a number with \
             as_string() in a child traversal instead of passing it directly."
        ),
    }
}

/// Appended to a `CastException` help whose target is `uuid`: the only accepted form, and the
/// literal that builds a UUID in a query.
const UUID_CANONICAL_HINT: &str = "\nA UUID is written in canonical form: 8-4-4-4-12 lowercase hex \
     digits, no braces, for example UUID.from_string(\"550e8400-e29b-41d4-a716-446655440000\").";

/// Expected-kind text of a `select(Column.keys)` / `select(Column.values)` cast error; `help()`
/// keys its advice on it.
pub(crate) const SELECT_COLUMN_EXPECTED: &str = "map or entry";

/// Expected kinds of the cast error of the `T.key`/`T.value` tokens (`by(T.value)`, ...): a
/// property element or a map entry.
pub(crate) const T_TOKEN_EXPECTED: &[&str] = &[
    "vertexProperty",
    "edgeProperty",
    "pathProperty",
    "map entry",
];

/// Expected-kind text of the cast error of `key()`/`value()`: they read a property object.
const PROPERTY_OBJECT_EXPECTED: &str = "vertexProperty or edgeProperty or pathProperty";

/// Expected-kind text of the cast error of `drop()`: what it can remove.
const DROP_EXPECTED: &str = "vertex or vertexProperty or edge or edgeProperty";

/// Expected-kind text of the cast error of the key filters `has(key)`, `has(key, value)`,
/// `has(key, P)` and `hasNot(key)`.
const KEY_FILTER_EXPECTED: &str = "computed or vertexProperty or edgeProperty or vertex or edge";

/// Expected-kind text of the cast error of the vertex-to-adjacent steps (`out()`, `in()`,
/// `both()`, `outE()`, `inE()`, `bothE()`).
const ADJACENCY_EXPECTED: &str = "vertex or vertexProperty or computed (linked to vertex)";

/// True when the comma-free `expected` list (`a or b or c`) names a graph element kind.
fn expects_an_element(expected: &str) -> bool {
    expected
        .split(" or ")
        .any(|kind| kind == "vertex" || kind == "edge")
}

/// Appended to the cast error of `drop()` on a value that is not stored graph data.
const DROP_HINT: &str = "\ndrop() removes stored graph data: a vertex, an edge, or a property \
     from properties(key) (or a values(key) reference). A computed value, a list, a map or a path \
     is not stored, so there is nothing to remove. To drop the elements a list or a path holds, \
     unfold() it first, for example g.v(\"1\").out().fold().unfold().drop() or \
     g.v(\"1\").out().path().unfold().drop(); to remove a property keep the element and use \
     properties(\"key\").drop() or remove_property(\"key\").";

/// Appended to the cast error of a key filter on a `properties(jpath(..))` element.
const KEY_FILTER_PATH_PROPERTY_HINT: &str = "\nhas(key)/has_not(key) read a key of a vertex, an \
     edge, a map or an object-valued property. A path property element from \
     properties(jpath(..)) is the value at the end of the path: test the path on the element \
     instead, for example g.v().has(jpath(\"meta.a\"), 1) or g.v().has(jpath(\"meta.a\"), P.gt(0)), \
     or test the value itself with has_value(..) or value().is(..).";

/// Appended to the cast error of a key filter on one map entry (`unfold()` of a map).
const KEY_FILTER_ENTRY_HINT: &str = "\nhas(key)/has_not(key) read a key of a whole map, but \
     unfold() split the map into single key/value entries. Filter the map before unfold(), for \
     example g.v().value_map().has(\"name\").unfold(), or test an entry's key or value with \
     select(Column.keys).is(..) / select(Column.values).is(..), for example \
     g.v().value_map().unfold().where(__.select(Column.keys).is(\"name\")).";

/// Appended when a vertex-to-adjacent step (`inE()`, `out()`, ...) meets an edge.
const ADJACENCY_ON_EDGE_HINT: &str = "\nout()/in()/both() and out_e()/in_e()/both_e() start from a \
     vertex. From an edge, step to its vertices first with out_v()/in_v()/both_v()/other_v(), for \
     example g.v(\"1\").out_e().in_v().in_e() (the edges into the vertices the outgoing edges reach).";

/// True for the stream kinds that hold a map (`group()`, `project()`, ...) or one map entry
/// (`unfold()` of a map).
fn is_map_kind(actual: &str) -> bool {
    matches!(actual, "object" | "record" | "entry")
}

/// Appended when an element-only step (`properties()`, `key()`, `value()`, `id()`, `out()`, ...)
/// meets a map: those steps read graph elements, a map's keys and values are read by `Column`.
const MAP_COLUMN_HINT: &str = "\nA map (for example the result of group() or group_count()) is not a \
     graph element, so properties()/key()/value() do not apply to it. Read its keys or values with \
     unfold().select(Column.keys) / unfold().select(Column.values), one traverser per entry, for \
     example g.v().group().by(\"name\").by(__.out().count()).unfold().select(Column.keys); \
     select(Column.keys) without unfold() returns them all as one list.";

/// Appended to the cast error of `select(Column.keys)` / `select(Column.values)` on a non-map.
const SELECT_COLUMN_HINT: &str = "\nselect(Column.keys) / select(Column.values) reads a map \
     (group(), group_count(), project(), element_map(), value_map()) or one entry of it produced by \
     unfold(), for example g.v().group_count().by(\"name\").unfold().select(Column.keys). To read \
     a path label use select(\"label\") with a string; to read a property use values(\"key\"). \
     select(Column.keys) of a path() lists the step labels of each position, for example \
     g.v().as(\"a\").out().as(\"b\").path().select(Column.keys), and select(Column.values) \
     of a path() lists its objects, for example g.v(\"1\").out().path().select(Column.values).";

/// Maps a canonical [`ValueType`](crate::graph::ValueType) name onto the `GType` token that
/// names it, so a diagnostic can hand back a `typeOf()` query that finds every value of that type.
fn gtype_token(observed: &str) -> Option<&'static str> {
    Some(match observed {
        "null" => "NULL",
        "boolean" => "BOOLEAN",
        "int64" => "LONG",
        "float64" => "DOUBLE",
        "string" => "STRING",
        "uuid" => "UUID",
        _ => return None,
    })
}

/// True when a rendered [`ValueType`](crate::graph::ValueType) name denotes a nested container
/// (`object` or `array<...>`), so a `CastException` diagnostic can tell a genuine type mismatch
/// (a `string` where a `number` was wanted) apart from a query that reached a whole nested value
/// when it meant to reach one field inside it — the case `.json_path(...)` was built to resolve.
fn is_nested_container(observed: &str) -> bool {
    observed == "object" || observed.starts_with("array<")
}

/// The `help()` text of [`ValueError::InvalidGlobPattern`], keyed on its `reason`.
fn glob_pattern_help(pattern: &str, reason: &str) -> String {
    use crate::value::glob_path::{double_star_suggestion, reason as r};
    const SYNTAX: &str = "\nA pattern is a '/'-separated list of segments matched level by level below \
         the start vertex: '*' (any value), '?' (one character), '**' (any number of levels, a whole \
         segment only), '[a-z]' / '[!a-z]' (character classes), '{a,b}' (alternatives within one \
         segment) and '\\' to escape any of them, for example glob_path(\"**/src/*.{rs,toml}\").";
    let advice = if reason == r::DOUBLE_STAR_IN_SEGMENT {
        match double_star_suggestion(pattern) {
            Some(fixed) => format!(
                "'**' spans whole levels, so it must stand alone between slashes. Write \
                 glob_path(\"{fixed}\") instead, or plain glob_path(\"**\") to match every level \
                 below the start vertex. A literal '*' is written '\\*'."
            ),
            None => "'**' spans whole levels, so it must stand alone between slashes, for example \
                     glob_path(\"a*/**/*b\") instead of glob_path(\"a**b\"), or plain \
                     glob_path(\"**\")."
                .to_string(),
        }
    } else if reason == r::SLASH_IN_ALTERNATIVES {
        "Alternatives in '{..}' choose within one segment and cannot contain '/'. Split the \
         alternative into two queries, for example glob_path(\"src/a\") and glob_path(\"lib/b\") \
         instead of glob_path(\"{src/a,lib/b}\"), or use '**' to span levels, for example \
         glob_path(\"**/{a,b}\"). A value that itself contains '/' is matched with '\\/'."
            .to_string()
    } else if reason == r::NESTED_ALTERNATIVES {
        "Alternatives cannot be nested; flatten them into one group, for example \
         glob_path(\"{a,b1,b2}\") instead of glob_path(\"{a,b{1,2}}\"), or place two groups side \
         by side: glob_path(\"{a,b}{1,2}\")."
            .to_string()
    } else if reason == r::RESERVED_SEGMENT {
        "'.' and '..' as whole segments are reserved for relative navigation. The start vertex is \
         already the current level: glob_path(\"docs/*\") matches its children directly. To walk \
         back up use .in() or repeat(__.in()) before the step, for example \
         g.v(\"x\").in().glob_path(\"docs/*\"). A value that is literally '.' is matched with '\\.'."
            .to_string()
    } else if reason == r::TOO_MANY_SEGMENTS {
        "A pattern can have at most 63 segments (a run of '**/**' counts once). Split the walk \
         into two consecutive steps, each matching below the vertices the previous one emits, for \
         example glob_path(\"a/b/c\").glob_path(\"d/e/f\") instead of glob_path(\"a/b/c/d/e/f\")."
            .to_string()
    } else if reason == r::TOO_MANY_ALTERNATIVES {
        "The '{..}' groups of one segment expand to their product, capped at 256 alternatives. \
         Replace a group with a class or a wildcard, for example glob_path(\"file[0-9][0-9]\") \
         instead of glob_path(\"file{0,1,...,9}{0,1,...,9}\"), or split the query."
            .to_string()
    } else if reason == r::UNCLOSED_CLASS {
        "Close the character class with ']' in the same segment, for example \
         glob_path(\"file[0-9].txt\"); match a literal '[' with '\\['."
            .to_string()
    } else if reason == r::EMPTY_CLASS {
        "A character class needs at least one character; a ']' right after '[' or '[!' is taken \
         literally, so glob_path(\"[]]\") matches ']' and glob_path(\"[!]]\") anything else."
            .to_string()
    } else if reason == r::INVERTED_RANGE {
        "A range goes from the lower to the higher character, for example glob_path(\"[a-z]*\") \
         instead of glob_path(\"[z-a]*\")."
            .to_string()
    } else if reason == r::UNCLOSED_ALTERNATIVES {
        "Close the alternatives with '}', for example glob_path(\"*.{rs,toml}\"); match a literal \
         '{' with '\\{'."
            .to_string()
    } else if reason == r::TRAILING_ESCAPE {
        "'\\' escapes the character after it; match a literal backslash with '\\\\', for example \
         glob_path(\"dir\\\\\")."
            .to_string()
    } else if reason == r::EMPTY_PATTERN {
        "Pass a non-empty pattern, for example glob_path(\"*\") for the children of the start \
         vertex or glob_path(\"**\") for all of its descendants."
            .to_string()
    } else if reason == r::LEADING_SLASH
        || reason == r::TRAILING_SLASH
        || reason == r::EMPTY_SEGMENT
    {
        "The pattern is relative to the start vertex: drop the leading and trailing '/' and the \
         empty segments, for example glob_path(\"src/main.rs\") instead of \
         glob_path(\"/src//main.rs/\"). Use '**' to match any number of levels."
            .to_string()
    } else {
        "Fix the pattern at the reported position.".to_string()
    };
    format!("{advice}{SYNTAX}")
}

impl DiagnosticError for ValueError {
    fn help(&self) -> Option<String> {
        match self {
            ValueError::CastException {
                expected,
                actual,
                origin,
                ..
            } => {
                let convert = coercion_step(expected);
                let t_token = *expected == T_TOKEN_EXPECTED.join(" or ");
                // A type search does not help a `T.key`/`T.value` misuse: every value fails.
                let locate = gtype_token(actual).filter(|_| !t_token);
                match origin.as_ref().and_then(|origin| {
                    origin.property.as_ref().map(|property| (origin, property))
                }) {
                    Some((origin, property)) => {
                        let mut help = format!(
                            "Property '{property}' holds a {actual} on {}, but [{expected}] is required here.\n\
                             Filter the stream before this step (for example with .has(\"{property}\", ...)), \
                             or declare '{property}' in the graph schema so the type is enforced on write.",
                            origin.element()
                        );
                        if let Some(convert) = convert {
                            help.push_str(&format!(
                                "\nConvert the values in the query instead: values(\"{property}\").{convert}."
                            ));
                            help.push_str(&format!(
                                "\nOr fix the data in place: property(\"{property}\", __.values(\"{property}\").{convert})."
                            ));
                            if let Some(note) = coercion_note(expected) {
                                help.push_str(&format!("\n{note}"));
                            }
                        }
                        if let Some(locate) = locate {
                            help.push_str(&format!(
                                "\nFind every offending value with values(\"{property}\").is(typeOf(GType.{locate}))."
                            ));
                        }
                        if expected == "uuid" {
                            help.push_str(UUID_CANONICAL_HINT);
                        }
                        if is_nested_container(actual) {
                            help.push_str(&format!(
                                "\nReach one nested field directly instead of chaining values(): \
                                 json_path(\"{property}.subkey\") instead of \
                                 values(\"{property}\").values(\"subkey\").\n\
                                 A jpath key does the same inside any step that takes a property name: \
                                 values(jpath(\"{property}.subkey\")), has(jpath(\"{property}.subkey\"), v), \
                                 order().by(jpath(\"{property}.subkey\")); a missing path is simply absent."
                            ));
                        }
                        if is_list_kind(actual) {
                            help.push_str(&format!(
                                "\n'{property}' holds a list: reduce each traverser's list with \
                                 Scope.local, for example values(\"{property}\").sum(Scope.local) \
                                 or values(\"{property}\").count(Scope.local), or unfold() it first. \
                                 values(\"{property}\").conjoin(\", \") joins it into one string."
                            ));
                        }
                        Some(help)
                    }
                    None => {
                        let mut help =
                            "Check the value type before performing conversion or cast operation."
                                .to_string();
                        if let Some(convert) = convert {
                            help.push_str(&format!("\nConvert the stream first with .{convert}."));
                            if let Some(note) = coercion_note(expected) {
                                help.push_str(&format!("\n{note}"));
                            }
                        }
                        if let Some(locate) = locate {
                            help.push_str(&format!(
                                "\nFind every offending value with .is(typeOf(GType.{locate}))."
                            ));
                        }
                        // A string step on an element; not a step that takes elements itself (an
                        // adjacency or key step whose expected kinds name "computed" too).
                        if (expected == "string"
                            || (expected.contains("computed") && !expects_an_element(expected)))
                            && matches!(actual.as_str(), "vertex" | "edge")
                        {
                            help.push_str(
                                "\nThis step needs a string, and a graph element is not one. as_string() \
                                 renders an element as text (g.v().as_string() gives \"v[1]\", an edge \
                                 \"e[7][1-knows->2]\"); to work on a property text, read it first: \
                                 g.v().values(\"name\").to_upper().",
                            );
                        }
                        if expected == "uuid" {
                            help.push_str(UUID_CANONICAL_HINT);
                        }
                        if expected == "scalar, array or object" {
                            help.push_str(
                                "\nA sack holds data, not a graph element. Store the element's id or one \
                                 of its properties instead: sack(Operator.assign).by(T.id) or \
                                 sack(Operator.assign).by(\"name\")."
                            );
                        }
                        if t_token {
                            help.push_str(
                                "\nT.key and T.value read a property element or a map entry, as in \
                                 TinkerPop: produce one with .properties(...) (or unfold() a map) \
                                 first, for example g.v().properties(\"name\").order().by(T.value). \
                                 A plain value (a values(...) result, a number) is already the \
                                 value: use by() with no argument or by(__.identity()) instead, \
                                 for example g.v().values(\"age\").order().by().",
                            );
                        }
                        if expected == PROPERTY_OBJECT_EXPECTED {
                            help.push_str(
                                "\n.value()/.key() only read a property object, produced by \
                                 .properties(...) or a raw .values(key) reference — call one of \
                                 those first, for example .properties(\"name\").value() instead \
                                 of .value() directly on a vertex/edge.",
                            );
                        }
                        if expected == DROP_EXPECTED && actual != "pathProperty" {
                            help.push_str(DROP_HINT);
                        }
                        if expected == KEY_FILTER_EXPECTED && actual == "pathProperty" {
                            help.push_str(KEY_FILTER_PATH_PROPERTY_HINT);
                        }
                        if expected == KEY_FILTER_EXPECTED && actual == "entry" {
                            help.push_str(KEY_FILTER_ENTRY_HINT);
                        }
                        if expected == ADJACENCY_EXPECTED && actual == "edge" {
                            help.push_str(ADJACENCY_ON_EDGE_HINT);
                        }
                        if actual == "pathProperty" && expected == DROP_EXPECTED {
                            help.push_str(
                                "\nA property found by a path, properties(jpath(..)), holds no reference to \
                                 the element it came from, so drop() cannot remove it. Remove the value at \
                                 the path with remove_property(jpath(..)) on the element, for example \
                                 g.v().remove_property(jpath(\"meta.tags[0]\")) (an object key is removed, an \
                                 array element is removed with shift).",
                            );
                        }
                        if is_map_kind(actual)
                            && (expected.contains("vertex") || expected.contains("edge"))
                            && !(expected == KEY_FILTER_EXPECTED && actual == "entry")
                            && expected != DROP_EXPECTED
                        {
                            help.push_str(MAP_COLUMN_HINT);
                        }
                        if expected == SELECT_COLUMN_EXPECTED {
                            help.push_str(SELECT_COLUMN_HINT);
                        }
                        if is_list_kind(actual) && expected != DROP_EXPECTED {
                            help.push_str(LIST_SCOPE_LOCAL_HINT);
                        }
                        Some(help)
                    }
                }
            }
            ValueError::IncomparableValues { step, expected, actual, origin, .. } => {
                let property = origin.as_ref().and_then(|origin| origin.property.as_ref());
                let convert = coercion_step(expected);
                let locate = gtype_token(actual);
                let mut help = match property {
                    Some(property) => format!(
                        "Property '{property}' holds a {actual} where '{step}' can only order [{expected}] values."
                    ),
                    None => format!(
                        "'{step}' can only order [{expected}] values here, but the stream holds a {actual}."
                    ),
                };
                help.push_str(&format!(
                    "\n{step}() compares numbers with numbers and text with text (booleans and uuids \
                     with their own kind); graph elements, paths and collections are not ordered."
                ));
                match property {
                    Some(property) => {
                        help.push_str(&format!(
                            "\nFilter the stream before this step (for example with .has(\"{property}\", ...)), \
                             or declare '{property}' in the graph schema so the type is enforced on write."
                        ));
                        if let Some(convert) = convert {
                            help.push_str(&format!(
                                "\nConvert the values in the query instead: values(\"{property}\").{convert}."
                            ));
                            help.push_str(&format!(
                                "\nOr fix the data in place: property(\"{property}\", __.values(\"{property}\").{convert})."
                            ));
                            if let Some(note) = coercion_note(expected) {
                                help.push_str(&format!("\n{note}"));
                            }
                        }
                        if let Some(locate) = locate {
                            help.push_str(&format!(
                                "\nFind every offending value with values(\"{property}\").is(typeOf(GType.{locate}))."
                            ));
                        }
                    }
                    None => {
                        if let Some(convert) = convert {
                            help.push_str(&format!("\nConvert the stream first with .{convert}."));
                            if let Some(note) = coercion_note(expected) {
                                help.push_str(&format!("\n{note}"));
                            }
                        }
                        if let Some(locate) = locate {
                            help.push_str(&format!(
                                "\nFind every offending value with .is(typeOf(GType.{locate}))."
                            ));
                        }
                    }
                }
                help.push_str(&format!(
                    "\nTo compare a mixed property read it as one kind first, with \
                     values(\"p\").as_string() or values(\"p\").as_number(GType.LONG), for example \
                     g.v().values(\"name\").{step}()."
                ));
                Some(help)
            }
            ValueError::PropertyNotFound {
                key,
                element_type: "path",
            } => Some(format!(
                "No step label '{key}' is set on the path of this traverser.\n\
                 Label the position with as() before reading it, for example \
                 g.v(\"1\").as(\"{key}\").out().select(\"{key}\"), or read the whole path \
                 with path(). A label set several times (as(\"{key}\") inside repeat()) is read \
                 with select(Pop.all, \"{key}\") (every occurrence), Pop.first, Pop.last (the \
                 default) or Pop.mixed."
            )),
            ValueError::UndeclaredLabel { label: key } => Some(format!(
                "Step label '{key}' is never declared anywhere in this query: no as(\"{key}\") \
                 and no side-effect key '{key}' from aggregate(\"{key}\")/store(\"{key}\")/\
                 group(\"{key}\")/group_count(\"{key}\")/\
                 tree(\"{key}\") exists, so it can never resolve — this is caught before \
                 execution instead of silently returning nothing.\n\
                 Label the position with as() before reading it, for example \
                 g.v(\"1\").as(\"{key}\").out().select(\"{key}\"), or read the whole path \
                 with path(). A label that is declared but only sometimes set (e.g. inside \
                 union()/optional()/choose()) is not an error: the traverser is filtered instead.\n\
                 A label set several times (as(\"{key}\") inside repeat()) is read with \
                 select(Pop.all, \"{key}\") (every occurrence), Pop.first, Pop.last (the default) \
                 or Pop.mixed, for example g.v(\"1\").as(\"{key}\").repeat(__.out().as(\"{key}\"))\
                 .times(2).select(Pop.all, \"{key}\").\n\
                 A label can also be read with where(): g.v(\"1\").as(\"{key}\").out()\
                 .where(__.as(\"{key}\").values(\"name\")) starts the child at the object \
                 labelled '{key}', and a trailing as(\"{key}\") keeps the traverser only when the \
                 child ends at that object."
            )),
            ValueError::PropertyNotFound {
                key,
                element_type: "side_effect",
            } => Some(format!(
                "No side-effect key '{key}' has been populated yet.\n\
                 A key whose aggregate()/store()/subgraph() step ran but collected nothing is not \
                 an error: cap(\"{key}\") returns the empty collection. This error therefore means \
                 that no aggregate()/store()/subgraph()/tree()/with_side_effect() declares the key \
                 (usually a typo).\n\
                 Populate it upstream with aggregate(\"{key}\") or store(\"{key}\") (or \
                 with_side_effect(\"{key}\", ...) for a fixed value, tree(\"{key}\") for a \
                 hierarchical tree, group(\"{key}\")/group_count(\"{key}\") for a grouped map, \
                 for example g.v().group_count(\"{key}\").by(T.label).cap(\"{key}\")), then read it with cap(\"{key}\") or \
                 where(within(\"{key}\"))/where(without(\"{key}\")) — a bare string argument to \
                 within()/without() inside where() resolves against a populated side-effect key \
                 first and only falls back to a literal value comparison when no such key exists.\n\
                 aggregate()/store() keep duplicates; to collect each value once declare the key \
                 as a set first: g.with_side_effect(\"{key}\", Set.of()).v().both().values(\"name\")\
                 .aggregate(\"{key}\").cap(\"{key}\"); to combine them with an operator instead of \
                 collecting them declare a reducer: g.with_side_effect(\"{key}\", 0, Operator.sum).v()\
                 .aggregate(\"{key}\").by(\"age\").cap(\"{key}\")."
            )),
            ValueError::PropertyNotFound {
                key,
                element_type: "list",
            } => Some(format!(
                "A list has no property '{key}': by(\"{key}\") reads a property of an element or a \
                 map, and the value here is a list (for example the result of select(Pop.all, \"a\") \
                 or of a select(Pop.mixed, \"a\") over a label set several times, or of fold()).\n\
                 Ensure the property key exists on the specified element type or use .has() filter.\n\
                 List the keys actually present with .properties() (no arguments).\n\
                 Read the property of every element of the list with a child traversal, for example \
                 g.v().as(\"a\").out().as(\"a\").select(Pop.all, \"a\")\
                 .by(__.unfold().values(\"{key}\").fold()); by() applies to the whole selected list, \
                 never to each element."
            )),
            ValueError::PropertyNotFound { key, .. } => {
                Some(format!(
                    "Ensure the property key exists on the specified element type or use .has() filter.\n\
                     List the keys actually present with .properties() (no arguments).\n\
                     A key is a literal property name, so '{key}' is not read as a path even when it \
                     contains dots. To reach a field inside an object or array property use a jpath key, \
                     for example values(jpath(\"meta.k\")) or by(jpath(\"meta.k\")); a path that does not \
                     exist yields nothing instead of an error."
                ))
            }
            ValueError::InvalidTypeOperation { .. } => Some(
                "Ensure operand types are compatible for operation, or convert one side first \
                 with .as_string(), .as_bool() or .as_number(GType.LONG)."
                    .to_string(),
            ),
            ValueError::PathNotDefined => Some(
                "Paths are recorded automatically for the steps that read them: label a position \
                 with as() and read it with select(), for example \
                 g.v(\"1\").as(\"a\").out().select(\"a\"), or read the whole path with \
                 path(), for example g.v(\"1\").out().path()."
                    .to_string(),
            ),
            ValueError::InvalidNumber { .. } => {
                Some("Verify that numeric values are within valid bounds and correct type format.".to_string())
            }
            ValueError::UnsupportedCastTarget { target, reason } => Some(format!(
                "'{target}' cannot be produced by a conversion yet: {reason}.\n\
                 The supported targets are as_string(), as_bool(), as_number(), \
                 as_number(GType.LONG) and as_number(GType.DOUBLE)."
            )),
            ValueError::InvalidJsonPath { .. } => Some(
                "A property path is a singular JSONPath (RFC 9535): member names and integer array \
                 indices only, e.g. jpath(\"meta.tags[0]\"), jpath(\"$.meta.tags[-1]\") (negative \
                 counts from the end) or jpath(\"meta['a.b'].k\") (bracket notation for a name that \
                 holds a dot, a quote or a bracket, escaped with \\' and \\\\). Use it as a key \
                 wherever a property name is taken, for example g.v().values(jpath(\"meta.k\")) \
                 or g.v().has(jpath(\"meta.k\"), 1), and as a step with json_path(\"$.meta.tags[0]\") \
                 or .by(By::JsonPath(\"meta.k\")). Recursive descent ('..'), wildcards ('*'), slices \
                 and filter expressions ('[?(...)]') are not supported. A plain string \
                 key such as values(\"a.b\") is never parsed: it names a property that literally \
                 contains a dot. A path has at most 256 segments, and an index is at most \
                 9007199254740991 (2^53 - 1) from either end."
                    .to_string(),
            ),
            ValueError::InvalidGlobPattern { pattern, reason, .. } => {
                Some(glob_pattern_help(pattern, reason))
            }
            ValueError::SampleWeight { .. } => Some(
                "sample(n).by(..) draws each traverser with a probability proportional to its \
                 by() value, so the by() must yield a number of at least 0: a numeric property \
                 (g.e().sample(2).by(\"weight\")) or a traversal ending in a number \
                 (g.v().sample(1).by(__.out_e().count())). Convert a numeric string with \
                 as_number() inside the by(), for example \
                 g.v().sample(1).by(__.values(\"score\").as_number()), or drop by() for a uniform \
                 sample."
                    .to_string(),
            ),
            ValueError::NumericIdComparison { fix, .. } => Some(format!(
                "Element ids are strings, so an order comparison (P.gt, P.gte, P.lt, P.lte, \
                 P.between, P.inside, P.outside) with a number can never hold. Compare with a \
                 string instead: {fix}. Strings are ordered character by character, so \"10\" \
                 sorts before \"9\": P.gt(\"3\") keeps \"30\" but not \"10\". Equality needs no \
                 change: has_id(3), has_id(P.eq(3)), has_id(P.neq(3)), has_id(P.within(1, 2)) and \
                 has_id(P.without(1, 2)) compare the \
                 number's text."
            )),
            ValueError::InvalidRegex { .. } => Some(
                "TextP.regex()/TextP.not_regex() take a regular expression in the syntax of the \
                 Rust `regex` crate (https://docs.rs/regex/latest/regex/#syntax), not Java's: \
                 classes [a-z], \\d, \\w, \\s, groups (..), alternation |, quantifiers * + ? \
                 {n,m}, inline flags such as (?i). Look-around ((?=..), (?!..)) and \
                 back-references (\\1) are not supported. Escape a literal metacharacter with a \
                 backslash (in a script string the backslash itself is doubled): \
                 g.v().has(\"name\", TextP.regex(\"^ma\\\\.\")). The match is unanchored: \
                 write ^...$ to match the whole string."
                    .to_string(),
            ),
            ValueError::LocalElementType {
                step,
                index,
                actual,
                ..
            } => Some(local_element_help(step, *index, actual)),
            ValueError::LabelStepElementKind { step, actual, .. } => {
                Some(label_step_help(step, actual))
            }
            ValueError::ListFunctionInput {
                step,
                operand,
                expected,
                actual,
            } => Some(list_function_help(step, operand, expected, actual)),
            ValueError::StringFunctionInput {
                step,
                operand,
                actual,
                ..
            } => Some(string_function_help(step, operand, actual)),
            ValueError::OperatorFailed { operator, kind, .. } => Some(operator_help(operator, kind)),
            ValueError::TemporalInput {
                step,
                operand,
                expected,
                actual,
            } => Some(temporal_input_help(step, operand, expected, actual)),
            ValueError::TemporalParse { step, target, .. } => {
                Some(temporal_parse_help(step, target))
            }
            ValueError::Temporal { step, error } => Some(temporal_help(step, error)),
            ValueError::Vector { error, .. } => Some(error.help()),
        }
    }

    fn details(&self) -> Vec<(String, String)> {
        let (value, origin) = match self {
            ValueError::CastException { value, origin, .. }
            | ValueError::IncomparableValues { value, origin, .. } => (value, origin),
            _ => return Vec::new(),
        };
        let mut details = Vec::new();
        if let Some(value) = value {
            details.push(("value".to_string(), value.clone()));
        }
        if let Some(origin) = origin {
            details.push(("origin".to_string(), origin.to_string()));
        }
        details
    }
}

/// Errors related to graph structure, vertex/edge lookup, and graph constraints.
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum GraphError {
    /// A vertex ID does not exist in the graph.
    #[error("Vertex with ID '{id}' was not found in the graph")]
    #[non_exhaustive]
    VertexNotFound {
        /// The missing vertex's ID.
        id: String,
    },

    /// An edge ID does not exist in the graph.
    #[error("Edge with ID '{id}' was not found in the graph")]
    #[non_exhaustive]
    EdgeNotFound {
        /// The missing edge's ID.
        id: String,
    },

    /// `add_v()` was given a vertex ID that already exists.
    #[error("Vertex with ID '{id}' already exists in the graph")]
    #[non_exhaustive]
    VertexIdUniqueConstraint {
        /// The colliding ID.
        id: String,
    },

    /// `add_e()` was given an edge ID that already exists.
    #[error("Edge with ID '{id}' already exists in the graph")]
    #[non_exhaustive]
    EdgeIdUniqueConstraint {
        /// The colliding ID.
        id: String,
    },

    /// The element behind a handle was removed earlier, typically earlier in the same query
    /// (`sideEffect(__.drop())`, a dropped vertex's edges), and a later step needs its id, its
    /// label, its whole value, or writes to it.
    ///
    /// Reads of a removed element's *data* do not fail: its properties are absent, its adjacency
    /// is empty, and filters that need its data drop it (TinkerGraph behaves the same, except
    /// that `outV()`/`inV()` of a removed TinkerGraph edge still return its endpoints; here they
    /// are empty, a deviation listed with the one below).
    ///
    /// **Deviation from TinkerPop:** TinkerGraph still reports `id()` and `label()` of a removed
    /// element, because its element object keeps them. Graphersal's handle no longer has any
    /// data to read (a slot that was freed may already hold another element), so `id()`,
    /// `label()` and materializing the element in a terminal fail with this error instead. See
    /// `book/src/for_developers/tinkerpop_deviations.md`.
    #[error(
        "The {} was removed earlier in this query; its id, label and value are no longer available",
        match .kind { GraphNodeType::Vertex => "vertex", GraphNodeType::Edge => "edge" }
    )]
    #[non_exhaustive]
    ElementRemoved {
        /// Whether the removed element was a vertex or an edge.
        kind: GraphNodeType,
    },

    /// An element handle refers to storage in an invalid state.
    #[error("Invalid element state: {reason}")]
    #[non_exhaustive]
    InvalidElementState {
        /// What was invalid.
        reason: String,
    },

    /// A vertex label contained `"::"`, the multi-label delimiter of GraphML and GraphSON export.
    #[error(
        "Vertex label '{label}' is invalid: it contains \"::\", which GraphSON and GraphML vertex label export reserve as the multi-label delimiter"
    )]
    #[non_exhaustive]
    InvalidLabel {
        /// The rejected label.
        label: String,
    },

    /// A vertex/edge was written as a property value, which is never allowed.
    #[error(
        "Property '{property_name}' cannot hold {}: {}",
        match *.kind { "edge" => "an edge", "map with a non-string key" => "a map with a non-string key", _ => "a vertex" },
        if *.kind == "map with a non-string key" { "a property map is keyed by strings" } else { "vertices and edges are not property values" }
    )]
    #[non_exhaustive]
    InvalidPropertyValue {
        /// The property key that was assigned an element.
        property_name: String,
        /// `"vertex"`, `"edge"` or `"map with a non-string key"`.
        kind: &'static str,
    },

    /// `property(jpath(..), value)` could not write at the path (see [`PathWriteFault`]).
    #[error("Cannot write property path '{path}': {fault}")]
    #[non_exhaustive]
    InvalidPropertyPath {
        /// The canonical text of the full path that was written.
        path: String,
        /// Why the write was refused.
        fault: PathWriteFault,
    },

    /// A schema-level error, forwarded as-is.
    #[error(transparent)]
    Schema(#[from] SchemaError),

    /// A catalog definition (a saved query, ...) was refused, or does not exist.
    #[error(transparent)]
    Definition(#[from] DefinitionError),

    /// A commit hook's `before_commit` vetoed the commit: the whole unit (the traversal, the
    /// import, the applied change set) was rolled back and nothing of it is applied. `source`
    /// is the hook's own error, unchanged.
    #[error("The commit was rejected by the commit hook '{hook}': {source}")]
    #[non_exhaustive]
    CommitRejected {
        /// The [`CommitHook::name`](crate::graph::CommitHook::name) of the vetoing hook.
        hook: String,
        /// The error the hook returned.
        #[source]
        source: crate::graph::HookError,
    },

    /// [`GraphStorage::mark`] could not set a mark: no
    /// journal is registered, the name is empty or too long, or a unit is open.
    #[error("Cannot set a mark: {reason}")]
    #[non_exhaustive]
    MarkUnavailable {
        /// Why.
        reason: &'static str,
    },

    /// The graph's journal refused a mark (a name it has already recorded, a failed write).
    #[error("The mark was rejected by '{hook}': {source}")]
    #[non_exhaustive]
    MarkRejected {
        /// The name of the journal hook.
        hook: String,
        /// The journal's error.
        #[source]
        source: crate::graph::HookError,
    },

    /// The storage does not support an operation: a [`GraphStorage`] method the storage leaves
    /// at its refusing default (schemas, the catalog, compressed properties, commit hooks on a
    /// storage that is not atomic, see [`StorageCapabilities`](crate::graph::StorageCapabilities)),
    /// or an operation refused in the storage's current state (a write on the read-only `&G`
    /// view, a journal attached while a unit is open). `operation` names it.
    #[error("This graph storage does not support {operation}")]
    #[non_exhaustive]
    Unsupported {
        /// What is not supported.
        operation: &'static str,
    },

    /// A `u64` counter of the graph is at its maximum and cannot advance: the commit sequence
    /// number (the unit was rolled back, nothing of it is applied) or an auto-id sequence (no
    /// new auto id can be handed out). The counters never wrap around, because a reused commit
    /// number or id would corrupt a journal or a replica.
    #[error("The {sequence} of this graph is exhausted: it reached {max} and cannot advance", max = u64::MAX)]
    #[non_exhaustive]
    SequenceOverflow {
        /// Which counter: `"commit sequence number"`, `"vertex auto-id sequence"` or
        /// `"edge auto-id sequence"`.
        sequence: &'static str,
    },

    /// [`ChangeSetBuilder::build`](crate::changes::ChangeSetBuilder::build) refused a change set
    /// built outside the graph: a `commit_seq` of `0` with a commit time, a negative commit time,
    /// or a definition change without an image of its own kind and name.
    #[error("Invalid change set: {reason}")]
    #[non_exhaustive]
    InvalidChangeSet {
        /// What is wrong with it.
        reason: String,
    },

    /// A unit that is not a traversal (a GraphML or GraphSON import into a live graph, an
    /// applied change set, the commit of an outermost unit such as a
    /// [`transaction`](crate::graph::Transactional::transaction) closure) grew the memory in use over
    /// the memory budget `memory.limit`: the whole unit was rolled back. The same limit as
    /// [`TraverserError::ResourceLimitExceeded`] of a traversal, with the same `help()`
    /// advice for the host setting, naming the operation.
    #[error(
        "Resource limit '{limit}' exceeded by the {operation}{}",
        limit_detail(*.max, *.observed)
    )]
    #[non_exhaustive]
    ResourceLimitExceeded {
        /// What was stopped: `"GraphML import"`, `"GraphSON import"`, `"change set apply"` or
        /// `"commit"` (an outermost unit at its commit).
        operation: &'static str,
        /// The limit that was hit: `memory.limit`.
        limit: String,
        /// The configured maximum in bytes.
        max: Option<u64>,
        /// The bytes in use (or estimated) that crossed it.
        observed: Option<u64>,
    },
}

/// The `sequence` name of the commit sequence number in [`GraphError::SequenceOverflow`].
pub(crate) const COMMIT_SEQUENCE: &str = "commit sequence number";
/// The `operation` of [`GraphError::Unsupported`] for a script `withSideEffect(name, graph)`
/// whose target graph is not a `TraversalGraph`.
pub(crate) const SUBGRAPH_TARGET: &str =
    "being the subgraph() target of with_side_effect(name, graph) (only a TraversalGraph is)";
/// The `sequence` name of the vertex auto-id sequence in [`GraphError::SequenceOverflow`].
pub(crate) const VERTEX_ID_SEQUENCE: &str = "vertex auto-id sequence";
/// The `sequence` name of the edge auto-id sequence in [`GraphError::SequenceOverflow`].
pub(crate) const EDGE_ID_SEQUENCE: &str = "edge auto-id sequence";

/// Why a `property(jpath(..), value)` write was refused. Every fault is detected before anything
/// is changed, so a refused write leaves the property exactly as it was.
///
/// `at` is the canonical text of the path prefix whose value (or absence) caused the fault.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PathWriteFault {
    /// The first path segment must name the property (`jpath("meta.k")`), not an index.
    FirstSegmentNotName,
    /// A key segment met something that is not an object (an array, a scalar, `null`, a map with
    /// non-string keys).
    KeyOnNonObject {
        /// The path of the value that is not an object.
        at: String,
        /// The kind of that value.
        found: &'static str,
    },
    /// An index segment met something that is not an array.
    IndexOnNonArray {
        /// The path of the value that is not an array.
        at: String,
        /// The kind of that value.
        found: &'static str,
    },
    /// An index outside `0..=len` (`len` appends, and only as the last segment), or a negative
    /// index outside `-len..=-1`.
    IndexOutOfRange {
        /// The path of the array.
        at: String,
        /// The requested index.
        index: i64,
        /// The current length of the array.
        len: usize,
    },
    /// An index segment needs an existing array; only key segments create missing containers.
    MissingArray {
        /// The path of the array that does not exist.
        at: String,
    },
    /// `property(Cardinality, jpath(..), value)` is not supported.
    CardinalityWithPath,
}

impl std::fmt::Display for PathWriteFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PathWriteFault::FirstSegmentNotName => {
                write!(f, "the first segment must name the property, not an index")
            }
            PathWriteFault::KeyOnNonObject { at, found } => {
                write!(
                    f,
                    "'{at}' is {} {found}, not an object, so a key cannot be set on it",
                    article(found)
                )
            }
            PathWriteFault::IndexOnNonArray { at, found } => {
                write!(
                    f,
                    "'{at}' is {} {found}, not an array, so an index cannot be used on it",
                    article(found)
                )
            }
            PathWriteFault::IndexOutOfRange { at, index, len } => {
                write!(
                    f,
                    "index {index} is out of range for the array '{at}' of length {len}"
                )
            }
            PathWriteFault::MissingArray { at } => {
                write!(
                    f,
                    "the array '{at}' does not exist, and an index segment never creates one"
                )
            }
            PathWriteFault::CardinalityWithPath => {
                write!(
                    f,
                    "property(Cardinality, jpath(..), value) is not supported"
                )
            }
        }
    }
}

/// Constructors for the errors the [`GraphStorage`] contract asks
/// an implementation to return. The variants are `#[non_exhaustive]`, so a storage outside this
/// crate builds them here.
impl GraphError {
    /// [`GraphError::Unsupported`]: this storage does not support `operation` (for example
    /// `"schemas"` or `"commit hooks"`, the texts the trait's defaults use).
    pub fn unsupported(operation: &'static str) -> Self {
        GraphError::Unsupported { operation }
    }

    /// [`GraphError::ElementRemoved`]: a write (or an id/label read) through a handle whose
    /// element was removed.
    pub fn element_removed(kind: GraphNodeType) -> Self {
        GraphError::ElementRemoved { kind }
    }

    /// [`GraphError::VertexNotFound`] for the vertex id `id`.
    pub fn vertex_not_found(id: impl Into<String>) -> Self {
        GraphError::VertexNotFound { id: id.into() }
    }

    /// [`GraphError::EdgeNotFound`] for the edge id `id`.
    pub fn edge_not_found(id: impl Into<String>) -> Self {
        GraphError::EdgeNotFound { id: id.into() }
    }

    /// [`GraphError::VertexIdUniqueConstraint`]: `insert_vertex` was given an id already in use.
    pub fn duplicate_vertex_id(id: impl Into<String>) -> Self {
        GraphError::VertexIdUniqueConstraint { id: id.into() }
    }

    /// [`GraphError::EdgeIdUniqueConstraint`]: `insert_edge` was given an id already in use.
    pub fn duplicate_edge_id(id: impl Into<String>) -> Self {
        GraphError::EdgeIdUniqueConstraint { id: id.into() }
    }

    /// [`GraphError::InvalidLabel`]: a vertex label containing `"::"`.
    pub fn invalid_label(label: impl Into<String>) -> Self {
        GraphError::InvalidLabel {
            label: label.into(),
        }
    }

    /// [`GraphError::InvalidElementState`]: the storage found itself in a state it cannot serve.
    pub fn invalid_element_state(reason: impl Into<String>) -> Self {
        GraphError::InvalidElementState {
            reason: reason.into(),
        }
    }

    /// [`GraphError::InvalidPropertyValue`]: the property `property_name` was given a value it
    /// cannot hold. `kind` names what was found, with the texts the crate uses: `"vertex"`,
    /// `"edge"` or `"map with a non-string key"`. A storage normally gets this error from
    /// `graphersal::storage::check_property_value` instead of building it.
    pub fn invalid_property_value(property_name: impl Into<String>, kind: &'static str) -> Self {
        GraphError::InvalidPropertyValue {
            property_name: property_name.into(),
            kind,
        }
    }

    /// [`GraphError::InvalidPropertyPath`]: a write at `path` was refused for `fault`; the error
    /// carries the canonical text of the path. A storage normally gets this error from the path
    /// helpers of `graphersal::storage` (`write_path`, `write_path_in`, `path_property_name`).
    pub fn invalid_property_path(path: &crate::value::JsonPath, fault: PathWriteFault) -> Self {
        GraphError::InvalidPropertyPath {
            path: path.to_string(),
            fault,
        }
    }

    /// [`GraphError::MarkUnavailable`]: this storage cannot set a mark now, for `reason` (no
    /// journal, a bad name, an open unit).
    pub fn mark_unavailable(reason: &'static str) -> Self {
        GraphError::MarkUnavailable { reason }
    }

    /// [`GraphError::ElementRemoved`] for a vertex.
    pub(crate) fn vertex_removed() -> Self {
        GraphError::ElementRemoved {
            kind: GraphNodeType::Vertex,
        }
    }

    /// [`GraphError::ElementRemoved`] for an edge.
    pub(crate) fn edge_removed() -> Self {
        GraphError::ElementRemoved {
            kind: GraphNodeType::Edge,
        }
    }
}

/// The own help of a hook's veto that brings one: a [`HookRefusal`](crate::graph::HookRefusal),
/// or a Store's refusal (`persist::PersistError`: a store in maintenance mode, a backup, a
/// store frozen by damage or read-only for this version, a journal write failure).
fn refusal_help(source: &(dyn std::error::Error + Send + Sync + 'static)) -> Option<String> {
    if let Some(refusal) = source.downcast_ref::<crate::graph::HookRefusal>() {
        return Some(refusal.help().to_string());
    }
    #[cfg(feature = "persist")]
    if let Some(err) = source.downcast_ref::<crate::persist::PersistError>() {
        return err.help();
    }
    None
}

fn article(kind: &str) -> &'static str {
    if kind.starts_with(['a', 'e', 'i', 'o', 'u']) {
        "an"
    } else {
        "a"
    }
}

impl DiagnosticError for GraphError {
    fn help(&self) -> Option<String> {
        match self {
            GraphError::VertexNotFound { .. } => Some(
                "Verify that the vertex ID exists in the graph before referencing it. A vertex that was \
                 dropped earlier (also earlier in the same traversal, for example by \
                 side_effect(__.drop())) no longer exists, and neither do its edges; do not add an \
                 edge to it, or add the edge before the drop."
                    .to_string(),
            ),
            GraphError::EdgeNotFound { .. } => Some(
                "Verify that the edge ID exists in the graph before referencing it.".to_string(),
            ),
            GraphError::VertexIdUniqueConstraint { .. } => {
                Some(
                    "Ensure vertex IDs are unique when adding new vertices. To get the vertex \
                     with this id if it exists and create it otherwise, use mergeV, for example \
                     g.merge_v(#{\"T.id\": \"1\"}).option(Merge.onCreate, #{\"T.label\": \"person\"})."
                        .to_string(),
                )
            }
            GraphError::EdgeIdUniqueConstraint { .. } => {
                Some(
                    "Ensure edge IDs are unique when adding new edges. To get the edge with this \
                     id if it exists and create it otherwise, use mergeE, for example \
                     g.merge_e(#{\"T.id\": \"7\"}).option(Merge.onCreate, #{\"T.label\": \
                     \"knows\", \"Direction.OUT\": \"1\", \"Direction.IN\": \"2\"})."
                        .to_string(),
                )
            }
            GraphError::ElementRemoved { kind } => {
                let (kind, example) = match kind {
                    GraphNodeType::Vertex => (
                        "vertex",
                        "g.v(\"1\").as(\"a\").element_map().as(\"m\")\
                         .select(\"a\").side_effect(__.drop()).select(\"m\")",
                    ),
                    GraphNodeType::Edge => (
                        "edge",
                        "g.v(\"1\").out_e().as(\"e\").element_map().as(\"m\")\
                         .select(\"e\").side_effect(__.drop()).select(\"m\")",
                    ),
                };
                Some(format!(
                    "The {kind} was dropped earlier in the same query (by drop(), \
                     side_effect(__.drop()), or together with a dropped vertex), but a later step \
                     still holds it, through a step label (as()/select()), a side effect \
                     (aggregate()/store()/cap()), a path or a sack. A removed {kind} has no \
                     properties and no edges, its id(), label() and value cannot be returned, \
                     and it cannot be changed. Materialize what you need before the drop (element_map(), value_map(), \
                     project()) and keep that instead, for example {example}, or run the drop as \
                     a separate query after reading. Not every later read fails: values(), \
                     properties() and value_map() of the removed {kind} find no properties and \
                     give no value (or an empty map), and a step that walks its edges (out(), \
                     in_e(), both()) finds none, without an error, so such a read silently \
                     returns nothing."
                ))
            }
            GraphError::InvalidElementState { .. } => {
                Some("Ensure element properties and states are valid.".to_string())
            }
            GraphError::InvalidLabel { .. } => Some(
                "Rename the label to remove \"::\" — e.g. add_v(\"Admin_Role\") instead of \
                 add_v(\"Admin::Role\") — since GraphSON and GraphML export join a vertex's multiple labels \
                 with \"::\" (see labels()/has_label())."
                    .to_string(),
            ),
            GraphError::InvalidPropertyValue {
                property_name,
                kind,
            } if *kind == "map with a non-string key" => Some(format!(
                "A property value that is a map must have string keys, because a stored map is a \
                 plain object. Store the map under string keys, for example \
                 property(\"{property_name}\", #{{\"29\": 1}}), or keep the data in the stream and \
                 show a map with a non-string key with to_json(), for example \
                 g.v().group_count().by(\"age\").to_json()."
            )),
            GraphError::InvalidPropertyValue {
                property_name,
                kind,
            } => {
                let (step, lookup) = if *kind == "edge" {
                    ("out_e(\"knows\")", "g.e(id)")
                } else {
                    ("out(\"knows\")", "g.v(id)")
                };
                Some(format!(
                    "Store the {kind}'s id instead of the {kind} itself, e.g. \
                     property(\"{property_name}\", __.{step}.id()), and look the {kind} up \
                     again with {lookup}."
                ))
            }
            GraphError::InvalidPropertyPath { path, fault } => Some(match fault {
                PathWriteFault::FirstSegmentNotName => format!(
                    "Start the path with the property name, for example \
                     property(jpath(\"meta.tags[0]\"), \"x\"); '{path}' begins with an index."
                ),
                PathWriteFault::KeyOnNonObject { .. } => format!(
                    "A key segment needs an object. Read the current value first, for example \
                     g.v().values(jpath(\"{path}\")), or write a whole object at a shorter \
                     path, for example property(jpath(\"meta\"), #{{\"k\": 1}}). Missing keys are \
                     created, existing scalars and arrays are never overwritten by a deeper path."
                ),
                PathWriteFault::IndexOnNonArray { .. } => format!(
                    "An index segment needs an array. Use a key segment for an object, for example \
                     jpath(\"meta.tags\") instead of jpath(\"meta[0]\"), or write the array first \
                     with property(jpath(\"meta.tags\"), [\"x\"]). Written path: {path}."
                ),
                PathWriteFault::IndexOutOfRange { .. } => format!(
                    "Use an index inside the array, [len] to append as the last segment, or a \
                     negative index [-1] for the last element; arrays are never padded with null. \
                     Written path: {path}."
                ),
                PathWriteFault::MissingArray { .. } => format!(
                    "Create the array first, for example property(jpath(\"meta.tags\"), [\"x\"]), \
                     then address its elements. Only key segments create missing objects. Written \
                     path: {path}."
                ),
                PathWriteFault::CardinalityWithPath => {
                    "Graphersal stores one value per property; write a nested value with \
                     property(jpath(\"meta.tags[0]\"), \"x\") without a Cardinality, or build the \
                     whole value and write it with property(Cardinality.single, \"meta\", value)."
                        .to_string()
                }
            }),
            GraphError::Schema(err) => err.help(),
            GraphError::Definition(err) => err.help(),
            GraphError::CommitRejected { hook, source }
                if refusal_help(source.as_ref()).is_some() =>
            {
                refusal_help(source.as_ref()).map(|own| {
                    format!(
                        "The commit hook '{hook}' refused the commit, so the whole unit was \
                         rolled back and left nothing behind. {own}"
                    )
                })
            }
            GraphError::CommitRejected { .. } => Some(
                "A commit hook registered by the host application (for example the write-ahead \
                 log of a server) refused the changes, so the whole unit was rolled back: the \
                 query, transaction, whole script, import or applied change set left nothing \
                 behind. The hook's reason is in the message; once its cause is fixed (disk \
                 space, a replica, a business rule), run the query again unchanged. To see what would \
                 have been committed without committing, run it in a dry run (Rust: \
                 Transactional::dry_run), which calls no hooks."
                    .to_string(),
            ),
            GraphError::MarkUnavailable { .. } => Some(
                "A mark is a record in the graph's journal: register one first (Rust: \
                 persist::Journal::new(file, Durability::EveryCommit).attach(&mut graph), \
                 Python: graph.start_journal(path)), give it a name of 1 to 255 bytes and set \
                 it between queries, for example g.mark(\"before-import\"), not inside a \
                 transaction. A storage that keeps no journal (a custom GraphStorage without \
                 one) cannot set marks at all."
                    .to_string(),
            ),
            GraphError::MarkRejected { .. } => Some(
                "Mark names are unique within a journal: choose a new one, for example \
                 g.mark(\"before-import-2\"). When the message reports a write failure, the \
                 journal is unusable until the graph is recovered from its snapshot and journal."
                    .to_string(),
            ),
            GraphError::Unsupported { operation } if *operation == SUBGRAPH_TARGET => Some(
                "subgraph() writes only into a TraversalGraph, and every graph a script builds \
                 is one: let the subgraph land in a graph of the script, for example \
                 g.e().has_label(\"knows\").subgraph(\"sg\").cap(\"sg\") or \
                 g.e().has_label(\"knows\").to_graph(), and copy what the host graph needs \
                 into it with add_v()/add_e() or merge_v()/merge_e()."
                    .to_string(),
            ),
            GraphError::Unsupported { operation } => Some(format!(
                "The graph's storage does not implement {operation}: a GraphStorage \
                 implementation supports only what it implements of the trait (the trait's \
                 defaults refuse), and some operations are refused only in a state (on a \
                 read-only view, while a unit is open). Check the storage's documentation and \
                 what its GraphStorage::capabilities() report before using the operation (for \
                 example commit hooks and dry runs need change_capture, and therefore atomic), \
                 or run it on a TraversalGraph, the reference storage, which implements every \
                 operation of the trait."
            )),
            GraphError::SequenceOverflow { sequence } if *sequence == COMMIT_SEQUENCE => Some(
                "The graph was numbered close to the end of the commit sequence (for example by \
                 TraversalGraph::with_last_commit_seq(u64::MAX), or by replaying a change set with \
                 such a commit_seq), so no further unit can commit; the failed one was rolled \
                 back and left nothing behind. Rebuild the graph from its data with a small \
                 commit sequence number: export it and load it into a new TraversalGraph."
                    .to_string(),
            ),
            GraphError::SequenceOverflow { .. } => Some(
                "An explicit numeric id raises the auto-id sequence to it, so an element added \
                 with an id at the end of the range (for example \
                 g.add_v(\"person\").property(T.id, \"18446744073709551615\")) leaves no room \
                 for further auto ids; the failed query was rolled back. Give new elements \
                 explicit ids, for example g.add_v(\"person\").property(T.id, \"p-1\"), and \
                 prefer non-numeric ids for imported elements, which do not move the sequence."
                    .to_string(),
            ),
            GraphError::InvalidChangeSet { .. } => Some(
                "A change set built with ChangeSet::builder carries the position of a real commit \
                 (commit_seq from 1 on, with its commit time in microseconds since the Unix \
                 epoch) or is a never-committed one (both 0, like a dry run). A \
                 Mutation::set_definition needs a before or an after definition, both of the \
                 kind and name it names; build it with Mutation::set_definition(kind, name, \
                 before, after)."
                    .to_string(),
            ),
            GraphError::ResourceLimitExceeded {
                operation,
                limit,
                max,
                observed,
            } => Some(unit_resource_limit_help(operation, limit, *max, *observed)),
        }
    }
}

/// Errors encountered during graph import/export or IO operations.
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum IOError {
    /// A file could not be opened/read.
    #[error("Failed to read file '{path}': {source}")]
    #[non_exhaustive]
    FileRead {
        /// The file path.
        path: String,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },

    /// The GraphML XML could not be parsed.
    #[error("XML parsing error at position {position}: {message}")]
    #[non_exhaustive]
    XmlParse {
        /// Byte offset of the failure.
        position: u64,
        /// What went wrong.
        message: String,
    },

    /// A file could not be created or written.
    #[error("Failed to write file '{path}': {source}")]
    #[non_exhaustive]
    FileWrite {
        /// The file path (`<GraphSON output>` for a caller's writer).
        path: String,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },

    /// A GraphSON line (line form) or document (wrapped form) is not valid JSON or not a vertex.
    #[error("GraphSON parse error at line {line}: {message}")]
    #[non_exhaustive]
    GraphSONParse {
        /// The 1-based line (line form) or vertex position (wrapped form).
        line: usize,
        /// What went wrong (with the byte offset inside the line, where known).
        message: String,
    },

    /// The input is a GraphSON variant Graphersal does not read (typed GraphSON 1.0 with
    /// `@class`, or the TinkerPop 2 Blueprints format).
    #[error("Unsupported GraphSON variant at line {line}: {reason}")]
    #[non_exhaustive]
    UnsupportedGraphSON {
        /// The 1-based line where it was detected.
        line: usize,
        /// Which variant it is.
        reason: String,
    },

    /// One GraphSON line (line form) or the whole document (wrapped form) is larger than the
    /// importer's `max_text_bytes`.
    #[error("GraphSON input at line {line} is larger than the limit of {limit} bytes")]
    #[non_exhaustive]
    InputTooLarge {
        /// The 1-based line that is too long.
        line: usize,
        /// The limit, in bytes.
        limit: usize,
    },

    /// The file's extension is not a supported graph file format.
    #[error("Unsupported graph file format: '{path}'")]
    #[non_exhaustive]
    UnsupportedFormat {
        /// The offending file path.
        path: String,
    },

    /// A schema JSON file failed to parse.
    #[cfg(feature = "serde")]
    #[error("JSON parsing error: {0}")]
    JsonParse(#[from] serde_json::Error),

    /// A packed snapshot file (`.gsnap`) could not be read or written (feature `persist`).
    #[cfg(feature = "persist")]
    #[error("Snapshot '{path}': {source}")]
    #[non_exhaustive]
    Snapshot {
        /// The file path.
        path: String,
        /// The persistence error.
        #[source]
        source: Box<crate::persist::PersistError>,
    },

    /// A required XML attribute was missing on an element.
    #[error("Missing required XML attribute '{attribute}' on element '{element}'")]
    #[non_exhaustive]
    MissingAttribute {
        /// The XML element name.
        element: String,
        /// The missing attribute name.
        attribute: String,
    },

    /// An XML attribute's value did not have the expected shape.
    #[error(
        "Invalid attribute '{attribute}' value '{value}' for key '{key}' (expected '{expected_type}')"
    )]
    #[non_exhaustive]
    InvalidAttributeValue {
        /// The GraphML key this attribute is for.
        key: String,
        /// The XML attribute name.
        attribute: String,
        /// The value actually found.
        value: String,
        /// The expected value shape.
        expected_type: String,
    },

    /// A property holds `NaN`/`Infinity`, which GraphML export cannot represent.
    #[error("Property '{key}' holds the non-finite float {value}, which GraphML cannot represent")]
    #[non_exhaustive]
    NonFiniteFloat {
        /// The offending property's key.
        key: String,
        /// The non-finite value, rendered.
        value: String,
    },

    /// A property is declared as a union of an array/object and a text type (`string`/`uuid`):
    /// GraphML writes all of them as text, so an import cannot tell which one a value is.
    #[error(
        "Property '{key}' is declared as {declared}, a union of an array/object and a text type, \
         which GraphML import cannot tell apart"
    )]
    #[non_exhaustive]
    AmbiguousContainerImport {
        /// The offending property's key.
        key: String,
        /// The declared type, in canonical spelling.
        declared: String,
    },

    /// The value of a property declared as an array/object is not valid JSON text.
    #[error(
        "Property '{key}' is declared as {declared}, but its GraphML value is not valid JSON: \
         {reason}"
    )]
    #[non_exhaustive]
    InvalidJsonValue {
        /// The offending property's key.
        key: String,
        /// The declared type, in canonical spelling.
        declared: String,
        /// Why the text was rejected (with its byte offset).
        reason: String,
    },

    /// A graph-level error encountered while applying imported data, forwarded as-is.
    #[error(transparent)]
    Graph(#[from] GraphError),
}

impl DiagnosticError for IOError {
    fn help(&self) -> Option<String> {
        match self {
            IOError::FileRead { .. } => Some(
                "Check if the file path is correct and accessible with read permissions."
                    .to_string(),
            ),
            IOError::XmlParse { .. } => {
                Some("Verify that the XML file is well-formed.".to_string())
            }
            IOError::FileWrite { .. } => Some(
                "Check that the directory exists and is writable, and that the disk is not full."
                    .to_string(),
            ),
            IOError::GraphSONParse { .. } => Some(
                "GraphSON is read as one vertex JSON object per line (TinkerPop's default, what \
                 g.io(file).write() produces) or as one document {\"vertices\": [...]}. Fix the \
                 JSON on the named line; a number must fit int64/float64."
                    .to_string(),
            ),
            IOError::UnsupportedGraphSON { .. } => Some(
                "Re-export the graph from TinkerPop 3 as GraphSON 3.0 (the default of \
                 g.io(\"graph.json\").write()) or as GraphML, and import that file."
                    .to_string(),
            ),
            IOError::InputTooLarge { .. } => Some(
                "Write the graph in the line form (one vertex per line, TinkerPop's default) \
                 instead of one wrapped document, or raise the importer's limit with \
                 GraphSONImport::new().with_max_text_bytes(n)."
                    .to_string(),
            ),
            IOError::UnsupportedFormat { .. } => Some(
                "Graph files are read by their extension: GraphSON ('.json', '.jsonl', \
                 '.graphson') or GraphML ('.xml', '.graphml'), for example \
                 GraphSource::file(\"graph.json\") or GraphSource::file(\"graph.graphml\")."
                    .to_string(),
            ),
            #[cfg(feature = "serde")]
            IOError::JsonParse(_) => {
                Some("Verify that the JSON document is well-formed.".to_string())
            }
            #[cfg(feature = "persist")]
            IOError::Snapshot { source, .. } => source.help(),
            IOError::MissingAttribute { .. } => {
                Some("Ensure all required XML attributes are present.".to_string())
            }
            IOError::InvalidAttributeValue { .. } => {
                Some("Ensure attribute values match expected type formats.".to_string())
            }
            IOError::NonFiniteFloat { key, .. } => Some(format!(
                "GraphML writes numbers as finite decimal text, so NaN and infinities cannot be \
                 exported: overwrite or drop the property on the affected elements before \
                 exporting, e.g. .property(\"{key}\", 0.0), .remove_property(\"{key}\") (keeps the element) or \
                 .properties(\"{key}\").drop(). GraphSON keeps them: \
                 g.export_graphson(\"graph.json\")."
            )),
            IOError::AmbiguousContainerImport { key, .. } => Some(format!(
                "Declare '{key}' either as an array/object type (its GraphML text is then read \
                 as JSON) or as `string` (the text is kept as it is), not as a union of both, \
                 and set that schema on the target graph before importing. GraphSON keeps array/object \
                 values typed and needs no declaration: g.export_graphson(\"graph.json\")."
            )),
            IOError::InvalidJsonValue { key, .. } => Some(format!(
                "A property declared as an array/object is imported from its JSON text, as \
                 GraphML export writes it (e.g. [\"a\",\"b\"] or {{\"cpu\":2}}). Fix the value \
                 of '{key}' in the file, or declare '{key}' as `string` in the target graph's \
                 schema to import the text unchanged. GraphSON keeps array/object values typed: \
                 g.export_graphson(\"graph.json\")."
            )),
            IOError::Graph(err) => err.help(),
        }
    }
}

/// Errors encountered during script parsing, compilation, or execution.
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum ScriptError {
    /// A Rhai script failed to evaluate.
    #[error("Script evaluation error near '{script_snippet}': {message}")]
    #[non_exhaustive]
    Eval {
        /// The offending script text, or a snippet of it.
        script_snippet: String,
        /// Failing line, when known.
        line: Option<usize>,
        /// Failing column, when known.
        column: Option<usize>,
        /// The underlying failure message.
        message: String,
    },

    /// A step name is not registered with the DSL.
    #[error("Unsupported step '{step_name}' in script context")]
    #[non_exhaustive]
    UnsupportedStep {
        /// The unrecognized step name.
        step_name: String,
    },

    /// A step was called with the wrong argument shape.
    #[error("Argument mismatch for step '{step_name}': expected {expected}, got {actual}")]
    #[non_exhaustive]
    ArgumentMismatch {
        /// The step's name.
        step_name: String,
        /// The expected argument shape.
        expected: String,
        /// The argument shape actually given.
        actual: String,
    },

    /// A named parameter binding was invalid (e.g. shadows a DSL global).
    #[error("Invalid script parameter '{parameter}': {message}")]
    #[non_exhaustive]
    InvalidParameter {
        /// The offending parameter name.
        parameter: String,
        /// Why it was rejected.
        message: String,
    },

    /// A script run as one unit (`script::eval_value_atomic`, `script::eval_value_dry_run`)
    /// returned a closure or function pointer (also inside an array or a map). The script's graph is private while it runs, so
    /// a closure that captured `g` would point at an empty graph once the script ends: the
    /// script is rejected and everything it changed is rolled back.
    #[error("A script run as one unit cannot return a closure or function pointer ({location})")]
    #[non_exhaustive]
    ReturnedClosure {
        /// Where the closure was found: `the result`, or a path such as `result[2].f`.
        location: String,
    },

    /// `query(..)` was called after steps (`g.v().query("x")`) or on the anonymous spawn
    /// (`__.query("x")`): a saved query starts a traversal, `g.query("x")`; calling one
    /// mid-traversal is a later feature (`book/src/users_guide/saved_queries.md`).
    #[error("query(\"{name}\") can only start a traversal, it cannot follow {position}")]
    #[non_exhaustive]
    QueryNotAtStart {
        /// The saved query named in the call.
        name: String,
        /// Where it was called: `other steps` or `__ (an anonymous traversal)`.
        position: String,
    },

    /// Saved queries called each other deeper than the call depth bound (16), usually a
    /// query that calls itself, directly or through others.
    #[error("Saved query calls nested deeper than {limit} levels (calling '{name}')")]
    #[non_exhaustive]
    QueryDepthExceeded {
        /// The query whose call went past the bound.
        name: String,
        /// The bound.
        limit: usize,
    },

    /// A call passed an argument the saved query does not declare.
    #[error("Saved query '{query}' has no parameter '{argument}'")]
    #[non_exhaustive]
    UnknownQueryArgument {
        /// The saved query.
        query: String,
        /// The argument name as passed.
        argument: String,
        /// The parameters the query declares, in signature order.
        parameters: Vec<String>,
    },

    /// A call left out an argument whose parameter has no default.
    #[error(
        "Saved query '{query}' needs the argument '{parameter}' ({expected}), it has no default"
    )]
    #[non_exhaustive]
    MissingQueryArgument {
        /// The saved query.
        query: String,
        /// The required parameter.
        parameter: String,
        /// Its declared type (`int64`, `string`, ...).
        expected: String,
    },

    /// An argument does not satisfy its parameter's schema (type or constraint), or is not a
    /// data value (a traversal, a closure).
    #[error("Invalid argument '{parameter}' for saved query '{query}': {reason}")]
    #[non_exhaustive]
    InvalidQueryArgument {
        /// The saved query.
        query: String,
        /// The parameter.
        parameter: String,
        /// What is wrong with the value.
        reason: String,
    },

    /// The body of a saved query does not compile, or is not exactly one function named like
    /// the query whose parameters are the declared ones.
    #[error("Invalid body of saved query '{query}': {reason}")]
    #[non_exhaustive]
    InvalidQueryBody {
        /// The saved query.
        query: String,
        /// What is wrong with the body.
        reason: String,
    },

    /// A saved query written in GQL reads a `$name` that its `params` do not declare.
    #[error(
        "Saved query '{query}' reads the parameter ${parameter}, which its params do not declare"
    )]
    #[non_exhaustive]
    UndeclaredQueryParameter {
        /// The saved query.
        query: String,
        /// The parameter the query reads (without `$`).
        parameter: String,
        /// The declared parameters.
        declared: Vec<String>,
    },

    /// A saved query written in GQL declares a parameter that its query never reads.
    #[error(
        "Saved query '{query}' declares the parameter '{parameter}', which its GQL query never reads"
    )]
    #[non_exhaustive]
    UnusedQueryParameter {
        /// The saved query.
        query: String,
        /// The declared parameter.
        parameter: String,
    },

    /// A saved query returned a traversal with a writing step that it cannot run: a read-only
    /// query returned one, or any query returned an anonymous one (`__.add_v(..)`). A query
    /// that writes runs a returned writing traversal of the graph inside its call.
    #[error("Saved query '{query}' returned a traversal with a writing step ({location})")]
    #[non_exhaustive]
    QueryReturnedWrite {
        /// The saved query.
        query: String,
        /// Where in the result: `the result` or a path such as `result[1]`.
        location: String,
    },
}

/// The `actual` text of a [`ScriptError::ArgumentMismatch`] for a traversal given where a step
/// takes values (issue #9); the `help()` arm recognizes it by this prefix.
pub(crate) const TRAVERSAL_ARGUMENT: &str = "a traversal";

/// The help of a traversal given to a step that takes values (ids, labels, names, property
/// keys): the step's own value forms, and the step that runs the traversal instead.
fn traversal_argument_help(step_name: &str) -> String {
    match step_name {
        "has_value" => "has_value() compares property values with a value or a predicate, not \
             a traversal: g.v().has_value(\"marko\") or g.v().has_value(P.within(\"marko\", \
             \"josh\")). To test the values with a traversal, use filter(): \
             g.v().filter(__.values().is(\"marko\")), or has(key, traversal) for one property: \
             g.v().has(\"name\", __.is(\"marko\"))."
            .to_string(),
        "has_label" => "has_label() takes labels or a predicate, not a traversal: \
             g.v().has_label(\"person\") or g.v().has_label(P.within(\"person\", \"software\")). \
             To test the label with a traversal, use filter(): \
             g.v().filter(__.label().is(\"person\"))."
            .to_string(),
        "has_id" => "has_id() takes ids or a predicate, not a traversal: g.v().has_id(\"1\") or \
             g.v().has_id(P.within(\"1\", \"2\")). To test the id with a traversal, use \
             filter(): g.v().filter(__.id().is(\"1\"))."
            .to_string(),
        "has_key" => "has_key() takes property keys or a predicate, not a traversal: \
             g.v().properties().has_key(\"name\"). To test the key with a traversal, use \
             filter(): g.v().properties().filter(__.key().is(\"name\"))."
            .to_string(),
        "has_not" => "has_not() takes a property key (a string or a jpath(..)), not a \
             traversal: g.v().has_not(\"age\")."
            .to_string(),
        "has" => "has() takes a property key (a string or a jpath(..)) or T.id/T.label first, \
             not a traversal, then a value, a predicate or a traversal: \
             g.v().has(\"name\", \"marko\"), g.v().has(\"name\", __.is(\"marko\")), \
             g.v().has(T.id, \"1\") or g.v().has(T.label, __.is(\"person\"))."
            .to_string(),
        "values" | "properties" | "element_map" | "similarity" | "nearest" | "embed"
        | "embed_property" => format!(
            "{step_name}() takes property keys as strings or jpath(..) values, not a traversal, \
             for example g.v().values(\"name\") or g.v().values(jpath(\"meta.k\")). To read a \
             value through a traversal, project it: \
             g.v().project(\"name\", \"friends\").by(\"name\").by(__.out(\"knows\").count())."
        ),
        "out" | "in" | "both" | "out_e" | "in_e" | "both_e" | "to_e" => format!(
            "{step_name}() takes edge labels as strings, not a traversal, for example \
             g.v().out(\"knows\") or g.v().out(\"knows\", \"created\"). To follow only the \
             edges a traversal picks, filter them: \
             g.v().out_e().where(__.in_v().has(\"name\", \"josh\")).in_v()."
        ),
        _ => format!(
            "{step_name}() takes names (strings) here, not a traversal, for example \
             g.v().as(\"a\").out().as(\"b\").select(\"a\", \"b\"), \
             g.v().project(\"name\").by(\"name\") or g.add_v(\"person\"). Where a step runs \
             a traversal it takes it as its only argument or in by(): \
             g.v(\"1\").as(\"a\").select(__.constant(\"a\")), \
             g.v().project(\"n\").by(__.out().count()), g.inject(1).add_v(__.constant(\"person\")) \
             or g.v(\"1\").as(\"a\").v(\"2\").add_e(\"knows\").from(__.select(\"a\"))."
        ),
    }
}

/// The reason of the [`TraverserError::PermissionDenied`] a saved query's read-only
/// restriction gives; its help explains the rule instead of the host's authorizer.
pub(crate) const SAVED_QUERY_READ_ONLY: &str = "a saved query is read-only";

/// The start of the reason of the [`TraverserError::PermissionDenied`] a saved query that
/// writes gives for a write of the schema, the catalog or a file.
pub(crate) const SAVED_QUERY_DATA_ONLY: &str = "a saved query writes data only";

/// The start of the reason of the [`TraverserError::PermissionDenied`] a saved query that
/// writes gives for a data write its inferred effects do not cover (an inference bug).
pub(crate) const SAVED_QUERY_OUTSIDE_EFFECTS: &str =
    "a saved query writes only what its inferred effects name";

impl DiagnosticError for ScriptError {
    fn help(&self) -> Option<String> {
        match self {
            ScriptError::Eval { .. } => {
                Some("Check script syntax and expressions at indicated line/column.".to_string())
            }
            ScriptError::ReturnedClosure { location } => Some(format!(
                "Return data or a traversal instead of a closure: run the closure inside the \
                 script and return its result (for example `let f = |x| x + 1; f.call(1)` \
                 instead of `|x| x + 1`), or return the traversal itself \
                 (`g.v().has_label(\"person\")`), which runs against the real graph when it is \
                 rendered. The closure was found at {location}."
            )),
            ScriptError::UnsupportedStep { .. } => {
                Some("Verify step name and DSL script compatibility.".to_string())
            }
            ScriptError::ArgumentMismatch { step_name, .. }
                if matches!(step_name.as_str(), "value_map" | "valueMap") =>
            {
                Some(
                    "value_map() takes property keys as strings; a leading `true` adds id and \
                     label: g.v().value_map(true, \"name\"). A boolean is accepted only as the \
                     first argument, and a null key (`()`) is ignored. element_map() returns the \
                     same shape with plain values instead of lists: g.v().element_map(\"name\"). \
                     A key can also be a jpath(..) into a nested value, keyed by the canonical \
                     path text: g.v().value_map(jpath(\"meta.k\"))."
                        .to_string(),
                )
            }
            ScriptError::ArgumentMismatch { step_name, .. }
                if matches!(step_name.as_str(), "and" | "or" | "not" | "negate") =>
            {
                Some(
                    "P.and(), P.or(), P.not() and negate() combine predicates only. Combine two \
                     predicates with p.and(q) / p.or(q), for example \
                     g.v().has(\"age\", P.gt(18).and(P.lt(30))) or \
                     g.v().has(\"age\", P.lt(18).or(P.gt(65))); complement one with P.not(p) or \
                     p.negate(), for example g.v().where(P.not(P.within(\"a\"))). To combine \
                     traversals instead, use the traversal combinators: \
                     g.v().where(__.and(__.out(), __.has(\"age\"))) or \
                     g.v().and(__.out(\"knows\"), __.has(\"age\", P.gt(30)))."
                        .to_string(),
                )
            }
            ScriptError::ArgumentMismatch { step_name, .. }
                if matches!(step_name.as_str(), "choose" | "branch") =>
            {
                Some(
                    "choose(selector) and branch(selector) take a traversal or the tokens T.label / \
                     T.id as the selector, for example \
                     g.v().choose(T.label).option(\"person\", __.out(\"knows\")) or \
                     g.v().choose(__.values(\"age\")).option(P.gt(30), __.constant(\"old\")). \
                     The two- and three-argument forms take a traversal or a predicate as the \
                     condition, for example \
                     g.v().values(\"age\").choose(P.gt(30), __.constant(\"old\"), __.constant(\"young\")); \
                     without the third argument a traverser that fails the condition passes through \
                     unchanged. A property key (T.key), a string or a number is not a selector."
                        .to_string(),
                )
            }
            ScriptError::ArgumentMismatch { step_name, .. } if step_name == "add_e" => Some(
                "add_e() takes the edge label as a string, or as a traversal whose first result is \
                 the label, for example g.v(\"1\").add_e(\"knows\").to(\"2\") or \
                 g.v().as(\"a\").out_e().as(\"b\").in_v().add_e(__.select(\"b\").label()).to(\"a\"). \
                 A list of labels is not accepted."
                    .to_string(),
            ),
            ScriptError::ArgumentMismatch { step_name, .. } if step_name == "gql" => Some(
                "gql() takes the query as a string and, optionally, the values of its \
                 $parameters as a map from name to value, for example \
                 g.gql(\"MATCH (p:person {name: $name}) RETURN p.age\", #{name: \"marko\"}); () \
                 stands for no parameters. A parameter value is a plain value (a string, a \
                 number, a boolean, a list, a map or ()), never a traversal or a closure: run the \
                 traversal first and pass its result, for example \
                 let n = g.v().count().next(); \
                 g.gql(\"MATCH (p:person) RETURN p.name LIMIT $n\", #{n: n})."
                    .to_string(),
            ),
            ScriptError::ArgumentMismatch {
                step_name, actual, ..
            } if actual.starts_with(TRAVERSAL_ARGUMENT) =>
            {
                Some(traversal_argument_help(step_name))
            }
            ScriptError::ArgumentMismatch { step_name, .. }
                if matches!(step_name.as_str(), "property" | "has" | "values" | "has_not" | "has_property") =>
            {
                Some(
                    "A property key is a string (a literal name) or a jpath(..) value (a path into a \
                     nested value); a number, a map or another type is not a key. For example \
                     g.v().property(\"name\", \"x\"), g.v().property(jpath(\"meta.tags[0]\"), \"x\") or \
                     g.v().has(jpath(\"meta.k\"), 1). A dotted string such as \"meta.k\" is never \
                     parsed as a path: wrap it in jpath(\"meta.k\")."
                        .to_string(),
                )
            }
            ScriptError::ArgumentMismatch { step_name, .. } if step_name == "remove_property" => Some(
                "remove_property() takes property keys as strings or jpath(..) values, one key or an \
                 array of keys, for example g.v().has_label(\"person\").remove_property(\"age\") or \
                 g.v().remove_property([\"age\", \"name\"]); a jpath removes the value below a \
                 property, for example g.v().remove_property(jpath(\"meta.tags[0]\")) (an array \
                 element is removed with shift, a missing path is a no-op); without an argument it \
                 removes every property of the element. The step keeps the element, so \
                 values(\"name\") can follow. property(k, ()) stores a null and does not remove."
                    .to_string(),
            ),
            ScriptError::ArgumentMismatch { step_name, .. } if step_name == "to_map" => Some(
                "to_map() turns a map into a Rhai object map, whose keys are strings, so it fails \
                 for a map with a non-string key such as the result of group_count().by(\"age\"). \
                 Read an entry with an indexer, for example \
                 let m = g.v().group_count().by(\"age\").next(); m[29]; list the keys with m.keys(); \
                 or show the whole map with g.v().group_count().by(\"age\").to_json()."
                    .to_string(),
            ),
            ScriptError::ArgumentMismatch { step_name, .. } if step_name == "property_json" => Some(
                "property_json() takes JSON-compatible values, and a JSON object has string keys, so \
                 a map with a non-string key (for example 29 or a list) is rejected. Use string keys, \
                 for example property_json(#{\"29\": 1}); to show a map with a non-string key use \
                 to_json(), for example g.v().group_count().by(\"age\").to_json()."
                    .to_string(),
            ),
            ScriptError::ArgumentMismatch { step_name, .. } if step_name == "Set.of" => Some(
                "Set.of(...) and Set.of_list([...]) take plain values: strings, numbers, booleans, \
                 UUIDs or null (an element or a traversal is not a value of a set). For example \
                 g.with_side_effect(\"a\", Set.of(\"alice\", \"bob\")).v().both().values(\"name\")\
                 .aggregate(\"a\").cap(\"a\") or Set.of_list([\"alice\", \"bob\"]); Set.of() starts an \
                 empty set."
                    .to_string(),
            ),
            ScriptError::ArgumentMismatch { step_name, .. } if step_name == "glob_path" => Some(
                "glob_path(pattern, #{...}) accepts the options max_depth (an integer from 1: the \
                 deepest level below the start vertex the walk visits) and prune (an anonymous \
                 traversal: a visited vertex for which it yields a result is not descended into, \
                 but is still emitted when it matches); the keys edges, path and case_insensitive \
                 are reserved. For example \
                 g.v(\"root\").glob_path(\"**/*.rs\", #{max_depth: 3, prune: __.has(\"name\", \"target\")}).by(\"name\")."
                    .to_string(),
            ),
            ScriptError::ArgumentMismatch { step_name, .. } if step_name == "select" => Some(
                "select() reads step labels, map keys or side-effect keys: select(\"a\"), \
                 select(\"a\", \"b\") (a map), select(Column.keys) (the keys of a map). When a \
                 label was set several times, name the occurrence with a Pop as the FIRST argument: \
                 select(Pop.first, \"a\"), select(Pop.last, \"a\") (the default), \
                 select(Pop.all, \"a\") (a list of every occurrence) or select(Pop.mixed, \"a\"), \
                 for example g.v().as(\"a\").out().as(\"a\").select(Pop.all, \"a\") or \
                 g.v().as(\"a\").out().as(\"b\").select(Pop.first, \"a\", \"b\"). The labels after the \
                 Pop are strings (or one array of strings), and at least one is required. The key \
                 can also be computed by a traversal, with exactly one key: \
                 select(Pop.all, __.constant(\"a\")) or select(__.constant(\"a\")); its first \
                 result must be a string. The id and label entries of an elementMap() or \
                 valueMap(true) map are tokens, read with ONE token alone: select(T.id), \
                 select(T.label) (select(\"label\") reads a property named label), and \
                 select(Direction.OUT) / select(Direction.IN) for an edge's endpoints, for \
                 example g.v().element_map().select(T.label)."
                    .to_string(),
            ),
            ScriptError::ArgumentMismatch { step_name, actual, .. }
                if matches!(step_name.as_str(), "has_id" | "has_label" | "has_key")
                    && actual.contains("predicate mixed") =>
            {
                Some(
                    "has_id(), has_label() and has_key() take either plain values or ONE predicate, \
                     never both in one call. Use P.within to combine values, for example \
                     g.v().has_id(P.within([\"1\", \"2\"])) or g.v().has_label(P.neq(\"person\")); \
                     to combine conditions chain the steps: \
                     g.v().has_label(\"person\").has_id(P.neq(\"1\"))."
                        .to_string(),
                )
            }
            ScriptError::ArgumentMismatch { step_name, .. }
                if matches!(step_name.as_str(), "V" | "E" | "has_id") =>
            {
                Some(
                    "v(), e() and has_id() take ids (strings, numbers, UUIDs), elements returned by \
                     next()/to_list(), or lists of them. For example: \
                     let e = g.e(\"11\").next(); g.e(e).to_list() or g.v([\"1\", \"2\"]).to_list(). \
                     A map is accepted only as an element, that is, with an `id` key. \
                     has_id() also takes one predicate, for example g.v().has_id(P.neq(\"1\"))."
                        .to_string(),
                )
            }
            ScriptError::ArgumentMismatch { step_name, .. }
                if matches!(
                    step_name.as_str(),
                    "define_query"
                        | "define_queries"
                        | "query"
                        | "move_query"
                        | "describe_query"
                ) =>
            {
                Some(
                    "Saved queries take maps and strings: \
                     g.define_query(#{name: \"top\", folder: \"reports\", description: \"..\", \
                     params: #{limit: #{schema: #{type: \"integer\", minimum: 1}, default: 20}}, \
                     body: \"fn top(limit) { g.v().limit(limit) }\"}) (name and body required), \
                     g.define_queries(\"reports\", source, #{top: #{limit: \"integer\"}}), \
                     g.query(\"top\", #{limit: 5}) (named arguments in a map), \
                     g.move_query(\"top\", \"archive\") and g.describe_query(\"top\", \"..\") \
                     (\"\" or () for the root or no description). A query written in GQL takes \
                     language: \"gql\" and the query text as its body, as in \
                     g.define_query(#{name: \"older\", language: \"gql\", params: #{min: \
                     \"integer\"}, body: \"MATCH (p:person) WHERE p.age > $min RETURN p.name AS \
                     name\"}). See the book page \"Saved queries\"."
                        .to_string(),
                )
            }
            ScriptError::ArgumentMismatch { .. } => {
                Some("Check function call signature and argument types in script.".to_string())
            }
            ScriptError::InvalidParameter { .. } => Some(
                "Bind named parameters with `eval_with_params` using identifier names that do \
                 not shadow the DSL globals (`g`, `__`, `_g`, `params`, `keys`, `values` and every token \
                 class such as `P`, `TextP`, `V`, `T`, `By`, `Order`, `Pick`, `GType`, `Pop`), for \
                 example params = {\"label\": \"person\"} used as \
                 g.v().has_label(label).to_list(). Positional parameters are 1-based: supply one \
                 value per placeholder to `eval_with_positional_params`, so that \
                 g.v().has_label($1).to_list() is evaluated with a single-element vector."
                    .to_string(),
            ),
            ScriptError::QueryNotAtStart { name, .. } => Some(format!(
                "Call a saved query at the start of a traversal, on g: g.query(\"{name}\") (with \
                 arguments g.query(\"{name}\", #{{limit: 5}})), and continue its result there, for \
                 example g.query(\"{name}\").values(\"name\"). Calling a saved query on incoming \
                 traversers (g.v().query(..), __.query(..)) is not supported yet; filter in the \
                 query itself, or pass the values it needs as arguments: \
                 let ids = g.v().has_label(\"person\").id().to_list(); g.query(\"{name}\", #{{ids: ids}})."
            )),
            ScriptError::QueryDepthExceeded { limit, .. } => Some(format!(
                "Saved queries may call each other at most {limit} levels deep; this bound stops a \
                 query that calls itself (directly or through other queries) without an end. \
                 Check the chain of g.query(..) calls in the bodies (g.get_query(name)[\"body\"] \
                 shows a body) and end the recursion, or compute the repetition inside one query \
                 with repeat(), for example g.v(id).repeat(__.out()).times(3)."
            )),
            ScriptError::UnknownQueryArgument {
                query, parameters, ..
            } => Some(if parameters.is_empty() {
                format!(
                    "'{query}' takes no arguments: call it as g.query(\"{query}\"). \
                     g.get_query(\"{query}\") shows its declaration."
                )
            } else {
                format!(
                    "Arguments are named after the parameters of '{query}': {}. For example \
                     g.query(\"{query}\", #{{{}: ..}}). g.get_query(\"{query}\") shows each \
                     parameter's schema and default; names are case-sensitive.",
                    parameters.join(", "),
                    parameters[0]
                )
            }),
            ScriptError::MissingQueryArgument {
                query, parameter, ..
            } => Some(format!(
                "Pass every parameter without a default by name, for example \
                 g.query(\"{query}\", #{{{parameter}: ..}}). A parameter with a default may be \
                 left out; give one a default by redefining the query with \
                 params: #{{{parameter}: #{{schema: .., default: ..}}}}."
            )),
            ScriptError::InvalidQueryArgument {
                query, parameter, ..
            } => Some(format!(
                "An argument is a data value (a string, a number, a boolean, a UUID, a date or \
                 time, a list or a map, never a traversal or a closure) that satisfies its \
                 parameter's schema; types are exact (the only conversions: an int64 where a float64 \
                 is declared, the text of a declared format such as a date-time). \
                 g.get_query(\"{query}\")[\"params\"] shows the schema of '{parameter}'; convert \
                 the value before the call, for example with parse_int(\"5\"), \
                 UUID.from_string(\"..\") or datetime(\"2024-05-01T09:30:00+02:00\")."
            )),
            ScriptError::InvalidQueryBody { query, .. } => Some(format!(
                "The body of a saved query is the text of ONE Rhai function named like the query \
                 whose parameters are the declared ones, and nothing else, for example \
                 g.define_query(#{{name: \"{query}\", params: #{{limit: \"integer\"}}, body: \
                 \"fn {query}(limit) {{ g.v().limit(limit) }}\"}}). Several queries at once go \
                 through g.define_queries(folder, source) (one query per function, doc comments \
                 as descriptions). A body that writes data (it is then saved as a query that \
                 writes) names its steps and the saved queries it calls literally, without Fn(..), \
                 call, curry, eval, import or a query name computed at run time, and writes data \
                 only: never the schema, saved queries, indexes or files. A stored body that no \
                 longer compiles is listed by g.queries() with status \"error\"; redefine it \
                 to fix it. A query written in GQL is saved with language: \"gql\" and its body \
                 is the plain query text, for example g.define_query(#{{name: \"{query}\", \
                 language: \"gql\", params: #{{min: \"integer\"}}, body: \"MATCH (p:person) \
                 WHERE p.age > $min RETURN p.name AS name\"}})."
            )),
            ScriptError::UndeclaredQueryParameter {
                query,
                parameter,
                declared,
            } => Some(format!(
                "Declare every $name the GQL query reads in params (parameter name -> a type \
                 name such as \"string\" or \"integer\", or #{{schema: .., default: ..}}), for \
                 example g.define_query(#{{name: \"{query}\", language: \"gql\", params: \
                 #{{{parameter}: \"string\"}}, body: ..}}); {}. A value that never changes can \
                 be written into the query as a literal instead.",
                if declared.is_empty() {
                    "it declares none".to_string()
                } else {
                    format!("it declares {}", declared.join(", "))
                }
            )),
            ScriptError::UnusedQueryParameter { parameter, .. } => Some(format!(
                "Every declared parameter is read by the GQL query as ${parameter}: read it, for \
                 example MATCH (p:person) WHERE p.name = ${parameter} RETURN p.age AS age, or \
                 remove '{parameter}' from params. Names are case-sensitive."
            )),
            ScriptError::QueryReturnedWrite { query, .. } => Some(format!(
                "A read-only saved query cannot write, also not through a traversal it returns, \
                 and no saved query can return an anonymous writing traversal (__.add_v(..)). \
                 A query whose body writes data is saved as one that writes (define \"{query}\" \
                 again with g.define_query(..)); it runs a returned writing traversal of the \
                 graph inside its call and returns a read-only traversal over what it wrote. Or \
                 return a reading traversal (or its values) and write in a traversal of your \
                 own, for example let ids = g.query(\"{query}\").id().to_list(); \
                 g.v(ids).property(\"seen\", true).to_list()."
            )),
        }
    }
}

/// Errors related to graph schema validation, topological constraints, and DDL operations.
#[derive(thiserror::Error, Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum SchemaError {
    /// A vertex label isn't declared in `Closed` mode.
    #[error("Unknown vertex label '{label}' is not declared in the closed schema")]
    #[non_exhaustive]
    UnknownVertexLabel {
        /// The undeclared label.
        label: String,
        /// The labels the schema does declare.
        allowed: Vec<String>,
    },

    /// An edge label isn't declared in `Closed` mode.
    #[error("Unknown edge label '{label}' is not declared in the closed schema")]
    #[non_exhaustive]
    UnknownEdgeLabel {
        /// The undeclared label.
        label: String,
        /// The labels the schema does declare.
        allowed: Vec<String>,
    },

    /// An edge's source/target label pair isn't an allowed connection shape.
    #[error(
        "Disallowed edge connection: '(:{source_label})-[:{edge_label}]->(:{target_label})' is not defined in schema"
    )]
    #[non_exhaustive]
    InvalidEdgeTopology {
        /// The edge's source vertex label.
        source_label: String,
        /// The edge's label.
        edge_label: String,
        /// The edge's target vertex label.
        target_label: String,
        /// The connections the schema declares for the edge label, rendered
        /// `(:from)-[:edge]->(:to)`.
        allowed: Vec<String>,
    },

    /// A property isn't declared for this label in `Closed` mode.
    #[error(
        "Undeclared property '{property_name}' on {element_type} '{label}' is not declared and not admitted by the schema"
    )]
    #[non_exhaustive]
    UndeclaredProperty {
        /// `"vertex"` or `"edge"`.
        element_type: &'static str,
        /// The element's label.
        label: String,
        /// The undeclared property key.
        property_name: String,
    },

    /// A declared, non-nullable property is missing.
    #[error("Missing required property '{property_name}' on {element_type} '{label}'")]
    #[non_exhaustive]
    MissingRequiredProperty {
        /// `"vertex"` or `"edge"`.
        element_type: &'static str,
        /// The element's label.
        label: String,
        /// The missing property key.
        property_name: String,
    },

    /// Removing a declared, non-nullable (required) property would leave the element invalid.
    #[error("Cannot remove required property '{property_name}' from {element_type} '{label}'")]
    #[non_exhaustive]
    RequiredPropertyRemoval {
        /// `"vertex"` or `"edge"`.
        element_type: &'static str,
        /// The label that declares the property as required.
        label: String,
        /// The property key.
        property_name: String,
    },

    /// A stored value's logical type doesn't match the schema's declared type.
    #[error(
        "Type mismatch on {element_type} '{label}.{property_name}': expected {expected_type}, got {actual_type}"
    )]
    #[non_exhaustive]
    TypeMismatch {
        /// `"vertex"` or `"edge"`.
        element_type: &'static str,
        /// The element's label.
        label: String,
        /// The property key.
        property_name: String,
        /// The declared type.
        expected_type: String,
        /// The value's actual type.
        actual_type: String,
    },

    /// A written value could not be coerced to its declared type.
    #[error(
        "Cannot store {actual_value} in {element_type} '{label}.{property_name}' declared as {declared_type}: {reason}"
    )]
    #[non_exhaustive]
    CoercionFailed {
        /// `"vertex"` or `"edge"`.
        element_type: &'static str,
        /// The element's label.
        label: String,
        /// The property key.
        property_name: String,
        /// The declared type.
        declared_type: String,
        /// The value that failed to coerce, rendered.
        ///
        /// A `Box<str>` rather than a `String`: the rendering is never grown after it is built,
        /// and dropping the capacity word keeps [`SchemaError`] (and every error embedding it)
        /// below `clippy::result_large_err`'s threshold.
        actual_value: Box<str>,
        /// Why coercion failed; always one of the fixed reasons of the coercion rules.
        reason: &'static str,
    },

    /// A declared field constraint (`min`/`max`/`pattern`/...) was violated.
    #[error(
        "Constraint violation on '{label}.{property_name}': {constraint_rule} (actual: {actual_value})"
    )]
    #[non_exhaustive]
    ConstraintViolation {
        /// The element's label.
        label: String,
        /// The property key.
        property_name: String,
        /// The violated constraint, rendered.
        constraint_rule: String,
        /// The offending value, rendered.
        actual_value: String,
    },

    /// A field's `pattern` constraint is not a valid regular expression.
    #[error("Invalid schema pattern '{pattern}': {reason}")]
    #[non_exhaustive]
    InvalidPattern {
        /// The pattern's source text.
        pattern: String,
        /// Why it does not compile.
        reason: String,
    },

    /// A schema was applied to a graph whose existing data violates it. The graph and its
    /// previous schema are unchanged. The report lists every kind of violation, grouped by rule,
    /// with counts and a few sample ids.
    #[error("{report}")]
    #[non_exhaustive]
    ExistingDataViolatesSchema {
        /// The aggregated violations (boxed: keeps `SchemaError` small).
        report: Box<crate::graph::schema::SchemaViolationReport>,
    },

    /// A schema (JSON text, a map, a patch) is not valid in the schema format: unknown or
    /// unsupported keyword (anywhere, the envelope included), a wrong value, an unresolved or
    /// cyclic `$ref`, a pattern that does not compile, ...
    #[error("Invalid schema at {path}: {reason}")]
    #[non_exhaustive]
    InvalidSchema {
        /// Where, as a JSON Pointer into the input (`/vertices/person/schema/properties/age`).
        path: String,
        /// What is wrong there.
        reason: String,
        /// How to fix it (empty when there is nothing specific to say).
        hint: String,
    },
}

/// Why a catalog definition (a saved query, ...; see [`crate::catalog`]) was refused:
/// a rule of its name, folder, sizes or parameters, the catalog's size, a malformed stored or
/// exchanged form, or a definition that does not exist.
#[derive(thiserror::Error, Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum DefinitionError {
    /// The definition's name breaks a naming rule (empty, too long, not an identifier, a
    /// reserved word).
    #[error("Invalid {kind} name '{name}': {reason}")]
    #[non_exhaustive]
    InvalidName {
        /// The kind of the definition (`query`, `kind 7`).
        kind: String,
        /// The name as given.
        name: String,
        /// Which rule it breaks.
        reason: String,
    },
    /// A folder path breaks a path rule (empty segment, leading or trailing `/`, too deep or
    /// too long).
    #[error("Invalid folder '{folder}' of '{name}': {reason}")]
    #[non_exhaustive]
    InvalidFolder {
        /// The definition the folder belongs to.
        name: String,
        /// The folder path as given.
        folder: String,
        /// Which rule it breaks.
        reason: String,
    },
    /// A field of a definition is too large, empty when it must not be, or of the wrong kind.
    #[error("Invalid {field} of '{name}': {reason}")]
    #[non_exhaustive]
    InvalidField {
        /// The definition the field belongs to.
        name: String,
        /// The field (`description`, `source`, `dialect`, `meta`, `payload`, ...).
        field: String,
        /// What is wrong with it.
        reason: String,
    },
    /// A parameter of a saved query is invalid: its name, its schema, or its default.
    #[error("Invalid parameter '{parameter}': {reason}")]
    #[non_exhaustive]
    InvalidParameter {
        /// The parameter name as given.
        parameter: String,
        /// What is wrong with it.
        reason: String,
    },
    /// The catalog holds its maximum number of definitions; a new one cannot be added.
    #[error("The catalog already holds {limit} definitions, the maximum")]
    #[non_exhaustive]
    TooManyDefinitions {
        /// The maximum number of definitions.
        limit: usize,
    },
    /// No definition of this kind has this name.
    #[error("There is no {kind} named '{name}'")]
    #[non_exhaustive]
    NotFound {
        /// The kind looked for (`query`, `kind 7`).
        kind: String,
        /// The name looked for.
        name: String,
    },
    /// A compression rule covers the same element kind, label and path as another rule, or a
    /// property index the same element kind, label and key as another index.
    #[error("Conflicting {kind} '{name}': {reason}")]
    #[non_exhaustive]
    Conflict {
        /// What conflicts: `compression rule` or `property index`.
        kind: String,
        /// The rule (or index) being stored.
        name: String,
        /// The rule already stored.
        existing: String,
        /// What they share.
        reason: String,
    },
    /// A property index cannot be created because the graph's schema declares its key as
    /// values the index does not hold (for its type a vector, an array, an object, or a union
    /// of only such; with the transform `to_lower` anything but a string; book page "Property
    /// Indexes", sections "Index types" and "Case-insensitive search").
    #[error(
        "Cannot create a property index on {element} '{label}' key '{key}': the schema \
         declares '{key}' as {declared}, and an index of type '{index_type}'{} holds none of \
         its values",
        transform_clause(.transform)
    )]
    #[non_exhaustive]
    IndexTypeMismatch {
        /// The element kind the index covers.
        element: crate::graph::schema::ElementKind,
        /// The label the index covers.
        label: String,
        /// The top-level property key.
        key: String,
        /// The declared type, without `null` (`vector<f32>`, `array<string>`, `object`).
        declared: String,
        /// The index type (`memory-ordered`).
        index_type: Box<str>,
        /// The index transform (`to_lower`), `None` without one.
        transform: Option<Box<str>>,
    },
    /// The JSON (or value) form of a definition or a catalog is malformed.
    #[error("Malformed definition: {reason}")]
    #[non_exhaustive]
    Malformed {
        /// What is wrong, with the place (`/definitions/2/params`).
        reason: String,
    },
}

/// ` with the transform 'to_lower'` for a mismatch of an index with a transform, else empty.
fn transform_clause(transform: &Option<Box<str>>) -> String {
    match transform {
        Some(transform) => format!(" with the transform '{transform}'"),
        None => String::new(),
    }
}

impl DiagnosticError for DefinitionError {
    fn help(&self) -> Option<String> {
        use crate::graph::definition as d;
        Some(match self {
            DefinitionError::InvalidName { kind, .. } if kind == "property_index" => format!(
                "A property index's name is an identifier of 1 to {} bytes (ASCII letters, \
                 digits and '_', starting with a letter or '_'), for example \"person_title\". \
                 It names the index for g.drop_index; g.create_index(#{{label, key}}) without a \
                 name calls it <label>_<key>.",
                d::MAX_QUERY_NAME_BYTES
            ),
            DefinitionError::InvalidName { kind, .. } if kind == "compression" => format!(
                "A compression rule's name is an identifier of 1 to {} bytes (ASCII letters, \
                 digits and '_', starting with a letter or '_'), for example \"customer_cv\". \
                 It names the rule for g.drop_compression and g.recompress.",
                d::MAX_QUERY_NAME_BYTES
            ),
            DefinitionError::InvalidName { .. } => format!(
                "A saved query's name is also its Rhai function name and its MCP tool name: 1 to \
                 {} bytes of ASCII letters, digits and '_', starting with a letter or '_' (a \
                 letter before the first digit), not a Rhai keyword, for example \
                 \"top_customers\". Names are case-sensitive and unique per kind; group queries \
                 with a folder instead of a prefix in the name.",
                d::MAX_QUERY_NAME_BYTES
            ),
            DefinitionError::InvalidFolder { .. } => format!(
                "A folder is a '/'-separated path such as \"reports/sales\": non-empty segments \
                 without surrounding spaces, no leading or trailing '/', at most {} segments and \
                 {} bytes. Leave the folder out (or pass an empty one) for the root.",
                d::MAX_FOLDER_SEGMENTS,
                d::MAX_FOLDER_BYTES
            ),
            DefinitionError::InvalidField { field, .. } => match field.as_str() {
                "description" => format!(
                    "Keep the description to at most {} bytes; longer documentation belongs in \
                     the query's source as comments.",
                    d::MAX_DESCRIPTION_BYTES
                ),
                "source" => format!(
                    "The source is the text of one Rhai function, at most {} bytes; split a \
                     larger body into several saved queries that call each other.",
                    d::MAX_SOURCE_BYTES
                ),
                "path" => "A rule path names object keys only, from the top-level property \
                           down: \"cv\" or \"details.career.cv\" (or \
                           jpath(\"$.details.career.cv\")); an index such as [0] or [-1] is not \
                           allowed, arrays are not compressed."
                    .to_string(),
                "min_bytes" => format!(
                    "min_bytes is the shortest string (in UTF-8 bytes) that is compressed: 1 to \
                     {} (default {}; {} for codec \"dedup\"); shorter strings stay plain. To \
                     choose one, g.estimate_compression(#{{label: \"customer\", path: \"cv\"}}) \
                     measures every threshold on the current data without changing the graph \
                     (add codec: \"dedup\" for the dedup thresholds).",
                    crate::graph::compression::MAX_MIN_BYTES,
                    crate::graph::compression::DEFAULT_MIN_BYTES,
                    crate::graph::compression::DEFAULT_DEDUP_MIN_BYTES
                ),
                "dictionary" => "A dictionary belongs to codec \"lz4\" (it primes the \
                                 compression of similar texts). A \"dedup\" rule stores each \
                                 distinct value once and has none: leave dictionary out, for \
                                 example g.define_compression(#{name: \"symbol_path\", label: \
                                 \"symbol\", path: \"path\", codec: \"dedup\"}), or use codec \
                                 \"lz4\" to keep the dictionary."
                    .to_string(),
                "label" => "Name the vertex or edge label the rule (or index) applies to, for \
                            example \"customer\"; it need not exist yet."
                    .to_string(),
                "key" => format!(
                    "An index orders the elements by one top-level property: name it with 1 to \
                     {} bytes, for example \"title\" (a plain name, not a jpath; nested values \
                     are not indexed).",
                    crate::graph::property_index::MAX_INDEX_KEY_BYTES
                ),
                "transform" => format!(
                    "The transform says what an index keys its values by. This version knows \
                     {}; leave transform out to index the values themselves. \"to_lower\" \
                     indexes the lowered text of string values for case-insensitive search: \
                     g.create_index(#{{label: \"person\", key: \"name\", transform: \
                     \"to_lower\"}}), queried with \
                     g.v().has_label(\"person\").has(\"name\", __.to_lower().is(\"marko\")).",
                    crate::graph::property_index::PropertyIndexTransform::KNOWN_NAMES
                        .iter()
                        .map(|name| format!("\"{name}\""))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                "type" => format!(
                    "The index type says how the index keeps its entries. This version knows \
                     {}. \"memory-ordered\" (the default; leave type out for it) is an ordered \
                     index kept in memory and rebuilt from the data when the graph is opened, \
                     serving equality, P.within and ranges, for example \
                     g.create_index(#{{label: \"person\", key: \"age\", type: \
                     \"memory-ordered\"}}).",
                    crate::graph::property_index::PropertyIndexType::KNOWN_NAMES
                        .iter()
                        .map(|name| format!("\"{name}\""))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                "dialect" => format!(
                    "The dialect names the language of the source; this version writes \"{}\" \
                     and \"{}\" (Rhai) and \"{}\" and \"{}\" (GQL).",
                    d::QUERY_DIALECT,
                    d::WRITING_QUERY_DIALECT,
                    d::GQL_QUERY_DIALECT,
                    d::GQL_WRITING_QUERY_DIALECT
                ),
                _ => "Check the field against the limits of the catalog (book page \"Saved \
                      queries\")."
                    .to_string(),
            },
            DefinitionError::InvalidParameter { .. } => format!(
                "A parameter is {{\"schema\": <schema node>, \"default\": <value>}} (or just a \
                 type name such as \"integer\"): its name is a Rhai identifier that is not a \
                 DSL global (g, __, P, T, ...), its schema uses the node format of the graph \
                 schema (type, enum, minimum, maximum, pattern, ...) and its default, when \
                 given, satisfies that schema, for example \
                 {{\"limit\": {{\"schema\": {{\"type\": \"integer\", \"minimum\": 1}}, \
                 \"default\": 20}}}}. At most {} parameters.",
                d::MAX_PARAMETERS
            ),
            DefinitionError::TooManyDefinitions { .. } => {
                "Remove definitions that are no longer used before adding new ones.".to_string()
            }
            DefinitionError::NotFound { .. } => {
                "List the existing definitions first: g.queries() in the DSL (or \
                 g.queries(\"reports\") for one folder), GraphTraversalSource::list_definitions \
                 in Rust; names are case-sensitive, and a query is called by its name alone, \
                 never with its folder. Define a missing one with g.define_query(#{name: .., \
                 body: ..})."
                    .to_string()
            }
            DefinitionError::Conflict { kind, existing, .. } if kind == "property index" => {
                format!(
                    "One index per element kind, label and key, and '{existing}' already is it: \
                     queries use it as it is (see g.indexes()). To rename it, drop it first with \
                     g.drop_index(\"{existing}\")."
                )
            }
            DefinitionError::Conflict { existing, .. } => format!(
                "One rule per element kind, label and path: change the existing rule instead \
                 (define it again under its name '{existing}' with the new parameters), or drop \
                 it first with g.drop_compression(\"{existing}\")."
            ),
            DefinitionError::IndexTypeMismatch {
                label,
                key,
                declared,
                index_type,
                transform,
                ..
            } => {
                let mut help = if let Some(transform) = transform {
                    format!(
                        "An index with the transform '{transform}' holds the lowered text of \
                         string values only, for case-insensitive search with \
                         has(\"{key}\", __.to_lower().is(..)); '{key}' is declared as \
                         {declared}, so it would hold nothing. Index the values themselves \
                         instead (leave transform out: g.create_index(#{{label: \"{label}\", \
                         key: \"{key}\"}})), or keep the text you search in its own string \
                         property and index that with transform: \"{transform}\"."
                    )
                } else if declared.contains("vector<") {
                    format!(
                        "Vectors are not index keys: search them with nearest() or similarity() \
                         (an exact search, book page \"Vectors\"), for example \
                         g.v().has_label(\"{label}\").nearest(\"{key}\", query, 10) for the 10 \
                         closest; a vector index type is planned."
                    )
                } else {
                    format!(
                        "An index of type '{index_type}' holds scalar values only (null, \
                         boolean, int64, float64, string, uuid, date, localdatetime, datetime, \
                         time, duration), never an array, an object or a vector. Index a scalar \
                         top-level property instead: an index key is one top-level property \
                         name, nested fields are not indexed, so keep the value you filter on \
                         in its own top-level property (for example \
                         g.v().has_label(\"{label}\").property(\"{key}_main\", ..)) and index \
                         that."
                    )
                };
                help.push_str(
                    " If the schema's declaration is wrong, change it first (g.patch_schema(..)) \
                     and create the index again; a schema change made after an index exists \
                     never drops it.",
                );
                help
            }
            DefinitionError::Malformed { .. } => {
                "A definition in JSON is an object with \"kind\" (\"query\") and \"name\", plus \
                 for a query \"source\", \"params\" and optionally \"folder\", \
                 \"description\", \"dialect\" and \"meta\"; a compression rule \
                 (\"kind\": \"compression\") has \"label\", \"path\" and optionally \"element\", \
                 \"codec\", \"min_bytes\" and \"dictionary\"; a property index \
                 (\"kind\": \"property_index\") has \"label\", \"key\" and optionally \
                 \"element\", \"type\" and \"transform\"; a catalog is {\"definitions\": [..]}."
                    .to_string()
            }
        })
    }
}

/// True for a schema error's property name that is a nested canonical path (`$.a.b[1].c`) rather
/// than a top-level property name.
fn is_nested_schema_path(name: &str) -> bool {
    name.starts_with("$.") || name.starts_with("$[")
}

impl SchemaError {
    /// The aggregated violation report when this error is an
    /// [`ExistingDataViolatesSchema`](SchemaError::ExistingDataViolatesSchema).
    pub fn violation_report(&self) -> Option<&crate::graph::schema::SchemaViolationReport> {
        match self {
            SchemaError::ExistingDataViolatesSchema { report } => Some(report),
            _ => None,
        }
    }
}

impl DiagnosticError for SchemaError {
    fn help(&self) -> Option<String> {
        match self {
            SchemaError::ExistingDataViolatesSchema { report } => {
                let mut help = String::from(
                    "The schema was NOT applied; the graph and its previous schema are unchanged. Fix or \
                     migrate the offending data first (add the missing properties, correct the values), or \
                     relax the schema (make the key optional by removing it from \"required\", allow null \
                     with \"type\": [\"T\", \"null\"], declare the label or property), or apply it with mode \
                     \"none\" (no validation), or start from the schema of the data (g.infer_schema()). \
                     Check a schema without applying it with g.validate_schema(schema) (or \
                     g.validate_schema_patch(patch)): it lists the same violations and the change's \
                     compatibility.\n\
                     To make a key required that existing elements lack, backfill first, in one \
                     transaction: (in a closed schema, declare the key optional first with \
                     g.patch_schema(..)), write the values, then patch the key into \"required\". A schema \
                     \"default\" fills only elements created after it is set, never existing ones (no \
                     backfill).",
                );
                let unlabeled = report
                    .groups
                    .iter()
                    .any(|group| group.label == crate::graph::schema::UNLABELED);
                if unlabeled {
                    help.push_str(
                        "\nElements without a label: a closed schema admits only declared labels, and \
                         an unlabeled vertex or edge has no label to declare (g.infer_schema() skips \
                         it). Freeze the structure in mode open instead, which admits them unchecked \
                         (g.set_schema(g.infer_schema(), SchemaMode.open)), or label them and freeze \
                         again: g.v(\"id\").add_label(\"Label\") for a vertex and \
                         g.e(\"id\").set_label(\"label\") for an edge listed above (every unlabeled \
                         one: g.v().not(__.has_label()).add_label(\"Label\"), \
                         g.e().not(__.has_label()).set_label(\"label\")), then \
                         g.set_schema(g.infer_schema(), SchemaMode.closed); or recreate them with a \
                         label (add_v(\"Label\"), add_e(\"label\")) and drop the unlabeled ones.",
                    );
                }
                Some(help)
            }
            SchemaError::UnknownVertexLabel { allowed, .. } => Some(format!(
                "A closed schema admits only declared vertex labels: use one of [{}], declare the label \
                 (g.patch_schema(#{{vertices: #{{label: #{{}}}}}})), or set mode \"open\".",
                allowed.join(", ")
            )),
            SchemaError::UnknownEdgeLabel { allowed, .. } => Some(format!(
                "A closed schema admits only declared edge labels: use one of [{}], declare the label \
                 (g.patch_schema(#{{edges: #{{label: #{{}}}}}})), or set mode \"open\".",
                allowed.join(", ")
            )),
            SchemaError::InvalidEdgeTopology { allowed, .. } => Some(format!(
                "A closed schema admits an edge only between the labels of a declared connection (any \
                 label of each endpoint may match; \"*\" matches every label). Declared connections: \
                 [{}]. Add {{\"from\": .., \"to\": ..}} to the edge label's \"connections\", or connect \
                 vertices with matching labels.",
                allowed.join(", ")
            )),
            SchemaError::UndeclaredProperty { property_name, .. } if is_nested_schema_path(property_name) => {
                Some(format!(
                    "Declare '{property_name}' among the \"properties\" of its enclosing object in the \
                     schema, or let that object admit undeclared keys with \"additionalProperties\": true \
                     (a closed schema's default is false; an open schema's is true)."
                ))
            }
            SchemaError::UndeclaredProperty { property_name, .. } => Some(format!(
                "Declare '{property_name}' in the label's \"properties\", or let the label admit \
                 undeclared keys: \"additionalProperties\": true on its schema, or mode \"open\". With \
                 several labels the key is allowed when ANY label declares or admits it."
            )),
            SchemaError::MissingRequiredProperty { property_name, .. } if is_nested_schema_path(property_name) => {
                Some(format!(
                    "Provide a value for the nested key '{property_name}' (e.g. \
                     property(jpath(\"{property_name}\"), value)) or remove it from \"required\" of its \
                     enclosing object in the schema; a required nested key cannot be removed with \
                     remove_property(jpath(..)). A schema \"default\" below the top level is an \
                     annotation: it does not fill a missing nested key (only a label's top-level \
                     properties get their default when an element is created)."
                ))
            }
            SchemaError::MissingRequiredProperty { property_name, .. } => {
                Some(format!(
                    "Provide a value for '{property_name}' or remove it from the label's \"required\" in \
                     the schema, or give it a \"default\" (or \"defaultFunction\") in the schema: it is \
                     stored when an element is created without the property (mode open or closed), \
                     but never filled into existing elements, and never by an import or a property \
                     removal.\n\
                     A required property must be supplied when the element is created: \
                     add_v(\"L\").property(\"{property_name}\", value) works because the optimizer rule \
                     add_property_fold folds the directly following constant property() steps into \
                     add_v()/add_e(). If a property() after it is not folded (the rule is disabled with \
                     g.with(\"optimizer.disabled\", [\"add_property_fold\"]), the value is a traversal, a \
                     Cardinality or a jpath key, or another step sits in between), the element is created \
                     empty and this error is raised; move a constant property(\"{property_name}\", value) \
                     directly behind add_v()/add_e() and keep add_property_fold enabled."
                ))
            }
            SchemaError::RequiredPropertyRemoval { property_name, label, element_type } => {
                Some(format!(
                    "'{property_name}' is in \"required\" of {element_type} label '{label}', so it cannot \
                     be removed in open or closed mode. Overwrite it with a new value instead (null only \
                     when its type admits null), remove it from \"required\" in the schema \
                     (g.patch_schema(..)), or set the schema mode to \"none\"."
                ))
            }
            SchemaError::InvalidSchema { hint, .. } => Some(if hint.is_empty() {
                crate::graph::schema::parse::FORMAT_HELP.to_string()
            } else {
                format!("{hint}\n{}", crate::graph::schema::parse::FORMAT_HELP)
            }),
            SchemaError::TypeMismatch {
                property_name,
                expected_type,
                actual_type,
                ..
            } => {
                let mut help = if actual_type == "null" {
                    format!(
                        "'{property_name}' does not admit null: write a value of type \
                         '{expected_type}', remove the property instead (absence is governed by \
                         \"required\", not by null), or allow null in the schema with \"type\": \
                         [\"T\", \"null\"]."
                    )
                } else {
                    format!("Convert value to expected type '{expected_type}' before inserting.")
                };
                // A nested location is read with a jpath key; a plain name is a literal key.
                let key = if is_nested_schema_path(property_name) {
                    format!("jpath(\"{property_name}\")")
                } else {
                    format!("\"{property_name}\"")
                };
                if let Some(convert) = coercion_step(&expected_type.to_lowercase()) {
                    help.push_str(&format!(
                        "\nIn a query, convert the values with values({key}).{convert}."
                    ));
                }
                if let Some(declared) = expected_type.strip_prefix("vector<") {
                    let element = declared
                        .split([',', '>'])
                        .next()
                        .unwrap_or("f32")
                        .trim();
                    help.push_str(&format!(
                        "\nA list of numbers written to a declared vector becomes a vector of its \
                         element on write (mode open or closed), and a vector of another element \
                         is quantized; the dimension is never changed. In a query, convert the \
                         values with values({key}).as_vector().quantize(\"{element}\")."
                    ));
                }
                if expected_type == "uuid" && actual_type != "string" {
                    help.push_str(
                        "\nOnly a canonical UUID string is converted to a uuid on write; convert \
                         a string value explicitly with .cast(GType.UUID).",
                    );
                }
                if let Some(locate) = gtype_token(&actual_type.to_lowercase()) {
                    help.push_str(&format!(
                        "\nFind every offending value with values({key}).is(typeOf(GType.{locate}))."
                    ));
                }
                Some(help)
            }
            SchemaError::CoercionFailed {
                property_name,
                declared_type,
                ..
            } => Some(match declared_type.as_str() {
                "uuid" => "UUIDs are written as 8-4-4-4-12 lowercase hex, e.g. \
                           `550e8400-e29b-41d4-a716-446655440000`."
                    .to_string(),
                "float64" => "The integer is too large to be represented exactly as float64; \
                              store it in an int64 field."
                    .to_string(),
                _ => format!(
                    "Store a value of type '{declared_type}' in '{property_name}', or change the \
                     declared type in the schema."
                ),
            }),
            SchemaError::ConstraintViolation { constraint_rule, .. } => {
                Some(format!("Ensure value complies with validation constraint: {constraint_rule}."))
            }
            SchemaError::InvalidPattern { .. } => Some(
                "A schema `pattern` is a regular expression with JSON Schema semantics (matched \
                 anywhere in the value; anchor it with ^...$ for a whole-value match), in the syntax \
                 of the Rust `regex` crate: classes [a-z], \\d, \\w, \\s, groups (..), alternation |, \
                 quantifiers * + ? {n,m}. Look-around ((?=..), (?!..)) and back-references (\\1) are \
                 not supported. Escape a literal metacharacter with a backslash, e.g. \\. for a dot \
                 (in a JSON schema file the backslash itself is doubled: \"^\\\\d+$\")."
                    .to_string(),
            ),
        }
    }
}

/// Boxed form of [`TraverserError`], used as the `Err` type of [`TraverserResult`].
///
/// `TraverserError` wraps every sub-error (up to 128 bytes on a 64-bit target), so an
/// unboxed `Result<T, TraverserError>` costs at least that much to move regardless of which
/// variant is live, even on the `Ok` path. Boxing keeps that cost to a single pointer.
pub type BoxedTraverserError = Box<TraverserError>;

/// The error type traversal execution returns.
///
/// Boxed (see [`BoxedTraverserError`]) so the success path of every fallible traversal
/// operation stays cheap to move; only the (rare) error path pays for the allocation.
pub type TraverserResult<T> = Result<T, BoxedTraverserError>;

/// Converts a sub-error straight into [`BoxedTraverserError`] in one `?`-hop, without a
/// per-call-site `.map_err(Box::new)`.
///
/// `std` already provides `Box<T>: From<T>` (i.e. `From<TraverserError> for
/// Box<TraverserError>`), but `?` performs only a single `From::from` call, so a
/// `#[from]`-generated conversion (e.g. `ValueError -> TraverserError`) does not compose
/// automatically into `Box<TraverserError>` through two hops. A single blanket `impl<T> From<T>
/// for BoxedTraverserError where TraverserError: From<T>` was tried and rejected: it conflicts
/// (`E0119`) with `std`'s own `impl<T> From<T> for Box<T>` at `T = TraverserError`, since
/// `TraverserError: From<TraverserError>` holds reflexively. Five concrete impls, one per
/// `#[from]` variant of `TraverserError` itself, sidestep that conflict.
macro_rules! impl_from_suberror_for_boxed {
    ($($sub:ty => $variant:ident),+ $(,)?) => {
        $(
            impl From<$sub> for BoxedTraverserError {
                fn from(err: $sub) -> Self {
                    Box::new(TraverserError::$variant(err))
                }
            }
        )+
    };
}

impl_from_suberror_for_boxed! {
    ValueError => Value,
    GraphError => Graph,
    IOError => Io,
    ScriptError => Script,
    SchemaError => Schema,
}

/// Delegates diagnostics to the boxed error, so callers holding a [`BoxedTraverserError`] (the
/// common case now that fallible traversal calls return [`TraverserResult`]) do not need to
/// deref it explicitly to call `help()`/`details()`/`diagnostic_body()`.
impl DiagnosticError for BoxedTraverserError {
    fn help(&self) -> Option<String> {
        (**self).help()
    }

    fn details(&self) -> Vec<(String, String)> {
        (**self).details()
    }

    fn diagnostic_body(&self) -> String {
        (**self).diagnostic_body()
    }
}

/// Top-level error enum wrapping domain errors and traversal pipeline failures.
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum TraverserError {
    /// A value/property/cast error, forwarded as-is.
    #[error(transparent)]
    Value(#[from] ValueError),

    /// A graph structure/lookup error, forwarded as-is.
    #[error(transparent)]
    Graph(#[from] GraphError),

    /// An import/export/IO error, forwarded as-is.
    #[error(transparent)]
    Io(#[from] IOError),

    /// A script parsing/evaluation error, forwarded as-is.
    #[error(transparent)]
    Script(#[from] ScriptError),

    /// A schema validation error, forwarded as-is.
    #[error(transparent)]
    Schema(#[from] SchemaError),

    /// A named step in the pipeline failed; wraps the underlying cause with its query position.
    #[error(
        "{}",
        step_failed_message(.child_index, .step_index, .step_name, .element_id, .source)
    )]
    #[non_exhaustive]
    StepFailed {
        /// The failing step's rendered name.
        step_name: String,
        /// Position of the step inside its own traversal level, used to build the step path.
        step_index: Option<usize>,
        /// When the level is a child traversal: its index among the children of the step that
        /// holds it (`GraphStep::child_traversals_mut` order); `None` at the top level. Together
        /// with `step_index` it builds the step id of [`TraverserError::step_location`].
        child_index: Option<usize>,
        /// Optimized physical plan of the traversal level this step belongs to.
        ///
        /// Boxed so that the plan, needed only to render a diagnostic, keeps
        /// [`TraverserError`] small.
        frame: Option<Box<StepFrame>>,
        /// ID of the element being processed when the step failed, when known.
        element_id: Option<String>,
        /// The underlying cause.
        #[source]
        source: Box<TraverserError>,
        /// The spelling of the step names in this diagnostic (`g.with("render.spelling", ..)`):
        /// the rendered steps are stored in it, and [`DiagnosticError::help`] and
        /// [`DiagnosticError::diagnostic_body`] respell their whole text with it.
        spelling: crate::traversal::Spelling,
    },

    /// A query failed for a reason not captured by a more specific variant.
    #[error("Query execution error: {reason}")]
    #[non_exhaustive]
    QueryExecution {
        /// The query text, when available.
        query: String,
        /// Why it failed.
        reason: String,
    },

    /// A requested capability doesn't exist yet.
    #[error("Feature not implemented yet: {feature}")]
    #[non_exhaustive]
    NotImplemented {
        /// The missing feature's name.
        feature: &'static str,
    },

    /// The execution recorded more path positions than its path arena can address.
    #[error("Path tracking exhausted: the traversal recorded more than {capacity} path positions")]
    #[non_exhaustive]
    PathArenaExhausted {
        /// Maximum number of path positions one execution can record.
        capacity: u64,
    },

    /// The execution stored more distinct sack values than its sack arena can address.
    #[error("Sack storage exhausted: the traversal stored more than {capacity} sack values")]
    #[non_exhaustive]
    SackArenaExhausted {
        /// Maximum number of sack values one execution can store.
        capacity: u64,
    },

    /// The execution ran past its `evaluationTimeout` budget and was stopped cooperatively.
    #[error("Traversal exceeded evaluationTimeout of {limit_ms} ms (ran {elapsed_ms} ms)")]
    #[non_exhaustive]
    Timeout {
        /// The budget set with `g.with("evaluationTimeout", ms)`, in milliseconds.
        limit_ms: u64,
        /// How long the execution had run when it was stopped, in milliseconds.
        elapsed_ms: u64,
    },

    /// The host cancelled the execution through its [`CancelToken`](crate::CancelToken) (for
    /// example the Stop button of the dev server); it stopped cooperatively and its unit rolled
    /// back.
    #[error("Traversal cancelled after {elapsed_ms} ms")]
    #[non_exhaustive]
    Cancelled {
        /// How long the execution had run when it was stopped, in milliseconds.
        elapsed_ms: u64,
    },

    /// An embedding failed (the `embed` step or the DSL function `embed(text)`): an unknown
    /// embedder, a value that is not a string, a text with nothing to embed, an embedder that
    /// failed or answered wrongly. A cancelled or timed-out embedding is [`Self::Cancelled`] or
    /// [`Self::Timeout`] instead.
    #[error("Embedding failed: {0}")]
    Embed(crate::value::EmbedError),

    /// An execution option set with `with()` holds a value of the wrong shape.
    ///
    /// Raised when the traversal is executed, since `with()` itself cannot fail.
    #[error("Invalid value for option '{key}': {reason}")]
    #[non_exhaustive]
    InvalidOption {
        /// The option key, such as `evaluationTimeout`.
        key: String,
        /// What is wrong with the value.
        reason: String,
    },

    /// A query's own `g.with(key, ..)` attempted to set an option the host running this
    /// instance has locked via [`crate::graph::ExecutionPolicy::with_locked`]. Distinct from
    /// [`Self::InvalidOption`]: this is not about the value being malformed — no value the query
    /// supplies is accepted for this key at all, regardless of shape.
    ///
    /// Raised when the traversal is executed, same timing as `InvalidOption`, since `with()`
    /// itself cannot fail.
    #[error(
        "Option '{key}' is locked by this instance's execution policy and cannot be set by a query"
    )]
    #[non_exhaustive]
    LockedOption {
        /// The option key the query tried to set, such as `evaluationTimeout`.
        key: String,
    },

    /// The host's [`Authorizer`](crate::graph::Authorizer) denied a request
    /// ([`AccessRequest`](crate::graph::AccessRequest): an action on a resource). Raised once
    /// per execution, before anything runs, for the first request of the plan (in plan order,
    /// child traversals included) that was denied; also for a source method that is not a step
    /// (file I/O, schema loading) and for a file path outside the host's file root.
    #[error("Permission denied for '{step}' ({request}): {reason}")]
    #[non_exhaustive]
    PermissionDenied {
        /// The step or operation that made the request, such as `add_v`, `export_graphml` or
        /// `with("evaluationTimeout")`.
        step: String,
        /// The denied request; [`AccessRequestBuf::as_request`](crate::graph::AccessRequestBuf::as_request)
        /// gives its action and resource.
        request: Box<crate::graph::AccessRequestBuf>,
        /// Why it was denied, as the authorizer said (or the path is outside the file root).
        reason: String,
    },

    /// A `repeat()` without `times()` ran more iterations than `repeat.max_loops` allows,
    /// which usually means it walks a cycle or has no stop condition.
    #[error(
        "repeat(){} exceeded the loop limit of {max_loops} iterations",
        .loop_name.as_ref().map(|name| format!(" loop '{name}'")).unwrap_or_default()
    )]
    #[non_exhaustive]
    RepeatLimitExceeded {
        /// The limit set with `g.with("repeat.max_loops", n)`, 10 000 by default.
        max_loops: u32,
        /// The name given with `repeat(name, body)`, if any.
        loop_name: Option<String>,
        /// The loop body as written, such as `__.out()`, used to build the advice.
        body: String,
    },

    /// The bulk (multiplicity) of merged traversers, or their sum, exceeds `u64` or the largest
    /// `int64`, so no exact count can be produced.
    #[error("The number of traversers at step '{step}' is too large to count (more than 2^63 - 1)")]
    #[non_exhaustive]
    BulkOverflow {
        /// The step that was summing or multiplying bulks.
        step: String,
    },

    /// A step had to expand merged traversers back into individual values, and that would
    /// materialize more values than `bulk.max_expansion` allows.
    #[error(
        "Step '{step}' would have to materialize {requested} values, more than the limit of {limit} \
         (bulk.max_expansion)"
    )]
    #[non_exhaustive]
    BulkExpansionLimit {
        /// The step (or terminal) that had to expand.
        step: String,
        /// How many values the expansion needs.
        requested: u64,
        /// The limit set with `g.with("bulk.max_expansion", n)`, 10 000 000 by default.
        limit: u64,
    },

    /// A script or traversal ran past a configured resource limit and was stopped.
    ///
    /// One error family for every resource bound: the script limits of the Rhai engine
    /// (`script::ScriptLimits`, feature `script`, named `script.*`) and the traversal limits set with
    /// `g.with()` or a host's [`crate::graph::ExecutionPolicy`] (named after their option key,
    /// such as `traversal.max_traversers`).
    #[error("Resource limit '{limit}' exceeded{}", limit_detail(*.max, *.observed))]
    #[non_exhaustive]
    ResourceLimitExceeded {
        /// The limit that was hit: an option key such as `traversal.max_traversers`, or a
        /// script limit such as `script.max_operations`.
        limit: String,
        /// The configured maximum, when it is known where the error is raised.
        max: Option<u64>,
        /// The value that crossed the limit (for example the number of traversers), when known.
        observed: Option<u64>,
    },

    /// `loops(name)` names no enclosing `repeat()`.
    #[error("loops(\"{name}\") names no enclosing repeat(){}", active_loops(.active))]
    #[non_exhaustive]
    UnknownLoopName {
        /// The loop name that was asked for.
        name: String,
        /// Names of the enclosing named loops, innermost first.
        active: Vec<String>,
    },

    /// `times()`, `until()` or `emit()` was written without a `repeat()` to modulate.
    #[error("{modulator} modulates a repeat() step, but the traversal has no repeat() for it")]
    #[non_exhaustive]
    RepeatWithoutBody {
        /// The modulators as written, such as `times(2)`.
        modulator: String,
    },

    /// A `repeat()` body has `inject()` as one of its own steps, which TinkerPop forbids (the
    /// injected values would re-enter every iteration). Only a direct step of the body is rejected;
    /// an `inject()` nested in `union(..)`, `local(..)` and the likeis allowed. The error is raised when the
    /// traversal executes, not when it is built (user's guide: "Recursive Traversals").
    #[error("inject() is not allowed as a step of a repeat() body")]
    #[non_exhaustive]
    RepeatInject {
        /// The name given with `repeat(name, body)`, if any.
        loop_name: Option<String>,
    },

    /// `.option(pick, traversal)` was written without a preceding `choose(traversal)`/
    /// `branch(traversal)` to attach to, or the preceding step is the locked 3-argument
    /// `choose(predicate, onTrue, onFalse)` form (which already has its two branches fixed and
    /// rejects further `.option()` calls).
    #[error(
        "{modulator} modulates a choose(traversal)/branch(traversal) or merge_v()/merge_e() step, but the traversal has none for it (or it already has choose(predicate, onTrue, onFalse)'s two fixed branches)"
    )]
    #[non_exhaustive]
    OptionWithoutBranch {
        /// The modulator as written, such as `.option("person", __.out("knows"))`.
        modulator: String,
    },

    /// `otherV()` walked its traverser's recorded path backward and found no vertex earlier
    /// than the current edge, so there is nothing to call "the other side" of.
    #[error("other_v() found no vertex earlier in the traversal to compare against edge {edge}")]
    #[non_exhaustive]
    OtherVWithoutOrigin {
        /// Debug rendering of the edge id, for the error message.
        edge: String,
    },

    /// `range(low, high)` received a `low`/`high` combination that violates TinkerPop's
    /// `[low, high)` contract: `low` must be zero or positive, `high` must be `-1`
    /// ("unbounded", TinkerPop's own sentinel) or at least `low`. Also raised by
    /// `limit/skip/tail/range(Scope.local, …)` for invalid bounds or a negative count.
    #[error("Invalid range [{low}, {high}): {reason}")]
    #[non_exhaustive]
    InvalidRange {
        /// The `low` bound as given to `range()`.
        low: i64,
        /// The `high` bound as given to `range()`, or `-1` for "unbounded".
        high: i64,
        /// What specifically is wrong with the combination.
        reason: String,
    },

    /// A step received a `by()` or `from()`/`to()` modulator it cannot use at that position, such
    /// as a vertex-label list (`by(["person", "software"])`) on `order()`, a `by()`/`from()`/
    /// `to()` that follows a step that takes none at all (`out().by("name")`,
    /// `out().from("a")`, `count().by("name")`), or no step at all (`g.by(..)`, reported with
    /// `step` `g` or `__`), like TinkerPop, which accepts `by()` only after a `ByModulating` step
    /// and `from()`/`to()` only after a `FromToModulating` step (`addE()`).
    ///
    /// Raised when the traversal is executed, since step builders cannot fail — and, before
    /// that, by a plan-time validation pass that walks the whole plan (including nested child
    /// traversals never reached at runtime), so a rejected modulator inside an unreached branch
    /// still fails instead of silently succeeding.
    #[error("`{step}()` cannot use its {modulator} #{position}: {reason}")]
    #[non_exhaustive]
    InvalidModulator {
        /// The step the modulator modulates, such as `order`, `group` or `add_e`.
        step: String,
        /// The 1-based index of the offending modulator among that step's modulators of the same
        /// kind (`by()`s counted separately from `from()`/`to()`s).
        position: usize,
        /// The modulator kind, `"by()"` or `"from()/to()"`, rendered right after `step`.
        modulator: &'static str,
        /// Why the step cannot use this modulator.
        reason: String,
    },

    /// A step that configures the traversal source `g` (`with_sack()`, `with_side_effect()`,
    /// `with_path()`) was written inside a child traversal (`__.with_sack(1)`), where it has nothing to
    /// configure. In Gremlin these steps exist only on the traversal source; the script DSL can
    /// still record them on `__`, so the plan-time validation pass rejects them before
    /// execution, wherever the child sits (also in a branch the data never reaches).
    #[error(
        "`{step}()` configures the traversal source g and cannot be used inside a child traversal"
    )]
    #[non_exhaustive]
    SourceStepInChild {
        /// The source step, such as `with_sack` or `with_side_effect`.
        step: String,
    },

    /// A property write asked for a cardinality Graphersal cannot honour: `Cardinality.list` or
    /// `Cardinality.set` on a vertex (Graphersal stores one value per property key), or any
    /// cardinality on an edge (edge properties have none, as in TinkerPop's `AddPropertyStep`).
    /// Raised only when a value would actually be written, so a `list`/`set` call that writes
    /// nothing (`property(Cardinality.set, null)`, zero traversers) succeeds.
    #[error("{}", unsupported_cardinality_message(.step, *.cardinality, *.on_edge))]
    #[non_exhaustive]
    UnsupportedCardinality {
        /// The writing step, such as `property`.
        step: String,
        /// The cardinality that was asked for.
        cardinality: crate::value::Cardinality,
        /// `true` when the write targeted an edge, which takes no cardinality at all.
        on_edge: bool,
    },

    /// `property(T.id, ..)` / `property(T.label, ..)` was used where it cannot set the id/label
    /// of an element being created: not right after `addV()`/`addE()` (only other `property()`
    /// calls may come in between), or `T.label` after `addE()`, whose label comes from
    /// `addE(label)`.
    #[error("{}", token_property_outside_add_message(.token))]
    #[non_exhaustive]
    TokenPropertyOutsideAdd {
        /// The token as written: `T.id` or `T.label`.
        token: String,
    },

    /// A `mergeV()`/`mergeE()` map (the merge argument or an `option(Merge.onCreate|onMatch|
    /// outV|inV, ..)` map) is malformed: not a map, a hidden (`~`) key, an invalid `T.id`/`T.label` value,
    /// a token key the step does not take, or `T.id`/`T.label` in `option(Merge.onMatch, ..)`.
    /// A constant map is checked before the traversal executes (even when no traverser reaches
    /// the step); a map produced by a traversal is checked when it arrives.
    #[error("{reason} (in {argument} of {step}())")]
    #[non_exhaustive]
    InvalidMergeMap {
        /// The merge step: `mergeV` or `mergeE`.
        step: String,
        /// Where the problem is: `the merge() map`, `the option(onCreate) map`, `the
        /// option(onMatch) map`, or an option the step does not take such as `option(outV)`.
        argument: String,
        /// What is wrong, e.g. `Property key can not be a hidden key: ~id`.
        reason: String,
    },

    /// A key of the `option(Merge.onCreate, ..)` map is also in the merge argument with a
    /// different value (TinkerPop: the onCreate map may only add keys). Checked before execution
    /// when both maps are constants, otherwise when the element is about to be created.
    #[error(
        "option(onCreate) cannot override values from merge() argument: {key} is {merge_value} in {step}() but {on_create_value} in option(onCreate)"
    )]
    #[non_exhaustive]
    MergeOverride {
        /// The merge step: `mergeV` or `mergeE`.
        step: String,
        /// The conflicting key, such as `T.label` or `name`.
        key: String,
        /// The key's value in the merge argument, rendered for the message.
        merge_value: String,
        /// The key's value in the onCreate map, rendered for the message.
        on_create_value: String,
    },

    /// `mergeE()` could not resolve a vertex of the edge: the merge and onCreate maps name no
    /// out-/in-vertex where an edge must be created ("Out Vertex not specified"), an id names no
    /// vertex ("Vertex id could not be resolved from mergeE: 100"), or a `Merge.outV`/`Merge.inV`
    /// placeholder has no `option(Merge.outV|inV, ..)` or that option does not identify exactly
    /// one vertex.
    #[error("{reason}")]
    #[non_exhaustive]
    MergeVertexUnresolved {
        /// Which end of the edge: `Direction::Out` or `Direction::In`.
        direction: crate::value::Direction,
        /// What went wrong, e.g. `In Vertex not specified`.
        reason: String,
    },

    /// A traverser reached a `fail()` / `fail(message)` step, which stops the traversal on
    /// purpose (TinkerPop `FailStep`).
    #[error("{message} (fail() reached by {value})")]
    #[non_exhaustive]
    Fail {
        /// The message given to `fail(message)`, or `fail() step triggered`.
        message: String,
        /// A short rendering of the value of the traverser that reached the step.
        value: String,
    },

    /// A `math()` equation that does not parse (raised before execution, even when no traverser
    /// reaches the step) or that fails for one traverser: an unknown variable, a nested-access
    /// path that leads nowhere, a value that is not a number, or a division by zero.
    #[error("math({}) failed at column {column}: {message}", crate::value::literal::rhai_string(.expr))]
    #[non_exhaustive]
    MathExpression {
        /// The equation as written.
        expr: String,
        /// Byte offset of the offending token in `expr`.
        offset: usize,
        /// 1-based character column of the offending token in `expr`.
        column: usize,
        /// Width of the offending token in characters (at least 1), for the underline.
        width: usize,
        /// What went wrong, with the data the advice is built from.
        kind: MathErrorKind,
        /// The rendered reason.
        message: String,
    },

    /// A GQL query (feature `gql`) that does not parse or bind, or that uses a construct
    /// Graphersal refuses or does not support yet. Like a `math()` equation, the query is checked
    /// before the traversal runs, even when no traverser reaches the step. `details()` shows the
    /// offending source line with a caret under the span, the expected tokens, a "did you mean"
    /// suggestion and the GQLSTATUS code; `help()` gives the fix for the kind.
    #[cfg(feature = "gql")]
    #[error("GQL {kind} at line {line}, column {column}: {message}")]
    #[non_exhaustive]
    Gql {
        /// What went wrong.
        kind: GqlErrorKind,
        /// The rendered reason, such as ``unexpected `RETRUN` ``.
        message: String,
        /// Byte offset of the offending text in the query.
        offset: usize,
        /// 1-based line of the offending text (lines end at `\n`).
        line: usize,
        /// 1-based character column of the offending text in its line.
        column: usize,
        /// Width of the offending text in characters (at least 1), for the underline.
        width: usize,
        /// The five-character GQLSTATUS code (ISO/IEC 39075), such as `42001` for a syntax
        /// error; see [`GqlErrorKind`] for the codes in use.
        gqlstatus: String,
        /// The query text, the expected tokens, the suggestion and the alternative (boxed, so
        /// the error stays small).
        context: Box<GqlErrorContext>,
    },

    /// An error raised before any step ran and without a step to locate it at (an invalid or
    /// locked option, a denied `with("key")`), under a non-default `render.spelling`: the
    /// diagnostic of `source` respelled. Its message is that of `source`; see
    /// [`TraverserError::root_cause`] for the error itself. Built only by the respelling of an
    /// execution's error, so with the default spelling an error is never wrapped.
    #[error("{source}")]
    #[non_exhaustive]
    Spelled {
        /// The spelling of the step names in the diagnostic.
        spelling: crate::traversal::Spelling,
        /// The error itself.
        source: Box<TraverserError>,
    },
}

/// What went wrong in a `math()` equation: the kind of a [`TraverserError::MathExpression`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum MathErrorKind {
    /// The equation does not parse: a syntax error such as `2 +* 3`, an unclosed bracket, or a
    /// function called with the wrong number of arguments.
    Syntax,
    /// A name called like a function (`sqr(_)`) that is not one of the built-in functions.
    #[non_exhaustive]
    UnknownFunction {
        /// The name as written.
        name: String,
    },
    /// A variable that is neither `_` (the current value) nor a key of the current map, a side
    /// effect or a step label.
    #[non_exhaustive]
    UnknownVariable {
        /// The variable name.
        name: String,
    },
    /// A nested-access path (`_.price.total`, `a.items[0]`) that leads to no value: a missing
    /// key, an index out of range, or a step into a value that is neither a map nor a list.
    #[non_exhaustive]
    BadPath {
        /// The path up to and including the failing segment, such as `_.price.total`.
        path: String,
    },
    /// A variable or nested value that is not a number.
    #[non_exhaustive]
    NotANumber {
        /// The variable or nested path, such as `_` or `_.name`.
        path: String,
        /// The type of the value found.
        actual: String,
    },
    /// A variable or nested value that is `null`.
    #[non_exhaustive]
    Null {
        /// The variable or nested path.
        path: String,
    },
    /// `/` or `%` with a zero divisor, or `cot(x)` where `tan(x)` is zero (an error, as in
    /// TinkerPop, instead of infinity).
    DivisionByZero,
    /// An inconsistency of the expression engine itself, never caused by the equation.
    Internal,
}

/// The rarely read part of a [`TraverserError::Gql`] (feature `gql`): the query and what the
/// diagnostic and `help()` are built from.
#[cfg(feature = "gql")]
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct GqlErrorContext {
    /// The query as written.
    pub query: String,
    /// The tokens or constructs that would have been accepted at the error; may be empty.
    pub expected: Vec<String>,
    /// A close spelling of a misspelt keyword, function or variable ("did you mean ...").
    pub suggestion: Option<String>,
    /// The GQL form or the Gremlin alternative of a refused or not yet supported construct, in
    /// canonical snake_case step spelling, such as "use the Gremlin step drop(), for example
    /// g.v().has_label(\"person\").drop()". `help()` leads with it.
    pub alternative: Option<String>,
    /// Which general advice `help()` adds after the alternative (internal).
    pub(crate) topic: GqlHelpTopic,
}

/// The general advice `help()` gives after the specific alternative of a GQL error: the kind's
/// own by default, or one about the rule the query broke (feature `gql`, internal).
#[cfg(feature = "gql")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum GqlHelpTopic {
    /// The advice of the error's kind.
    #[default]
    Kind,
    /// What `WITH` and `LET` keep in scope.
    Scope,
    /// Grouping keys of an aggregating `RETURN` / `WITH`, and the columns after it.
    Grouping,
    /// What a node, edge, path or group variable of a pattern names.
    PatternVariable,
    /// A name used twice (a column, a `LET` variable, a map key).
    Names,
    /// The variable or the ordinal of a `FOR` / `UNWIND` (stage 2a).
    For,
    /// A subquery (`EXISTS`, a pattern predicate): its imports, its locals, its size (stage
    /// 2b).
    Subquery,
    /// A `COUNT { }` or `COLLECT { }` subquery: its value, its imports, its locals (stage 2d).
    CountCollect,
    /// A pattern comprehension `[(a)-->(b) | expr]`: its list, its imports, its locals (stage
    /// 2c).
    PatternComprehension,
    /// The variable of a list comprehension `[x IN list | expr]` reusing a name (stage 2c).
    ComprehensionVariable,
    /// An `INSERT` / `CREATE` pattern (stage 3a): what a new node, a new edge and a bound node
    /// reference may carry (designs W5-W7), a write inside a subquery expression (W3), an edge
    /// endpoint that is no node at run time.
    Insert,
    /// A `SET` / `REMOVE` clause (stage 3c): the item forms, a write inside a subquery
    /// expression, a value an item cannot write at run time.
    Set,
    /// A `[DETACH | NODETACH] DELETE` clause (stage 3d): its targets, a write inside a subquery
    /// expression, a target that holds no element at run time.
    Delete,
    /// A `MERGE` clause (stage 3e): its one pattern and what it may carry (design W30, W36),
    /// its `ON CREATE` / `ON MATCH` items, a write inside a subquery expression.
    Merge,
    /// `UNION` and `NEXT`: the column names of the operands, one kind of `UNION` per chain,
    /// what the statement after `NEXT` reads (stage 2d).
    Composite,
    /// A `CALL { }` subquery: its imports, its returned columns, its `RETURN` (stage 2d).
    Call,
    /// A value of the wrong kind in `sum()` / `avg()` (22G03).
    AggregateValue,
    /// A value or a percentile `percentile_cont()` / `percentile_disc()` cannot take (22G03,
    /// 22003; stage 3f).
    Percentile,
    /// Text and a number (or a boolean) joined by `+` or `||` (22G03): no implicit conversion.
    TextAndNumber,
    /// A subscript `x[i]` / `x[a..b]` of the wrong kind of value or index (22G03, stage 3f).
    Subscript,
    /// An operand whose kind is certain from the query text and that its operator or function
    /// never takes, refused before the query runs (22G03, stage 3f, `gql::eval::kinds`).
    StaticKind,
    /// Arithmetic with a temporal operand that has no meaning (22G03, stage 3g): what `+`,
    /// `-`, `*` and `/` take with dates, times and durations.
    TemporalArithmetic,
    /// A temporal constructor given a value it cannot build from (22G03, stage 3g): the
    /// arguments of `date()`, `localtime()`, `localdatetime()`, `datetime()`, `duration()` and
    /// `datetime.fromepoch*()`.
    TemporalConstruct,
    /// A component `x.key` a temporal value does not have (22G03, stage 3g): the components of
    /// each kind.
    TemporalComponent,
    /// A unit or value `X.truncate(unit, value [, map])` cannot take (22023, 22G03, stage 3g):
    /// the units of each type.
    TemporalTruncate,
    /// An operand `duration.between()` and `duration.in*()` cannot take (22G03, stage 3g).
    TemporalBetween,
    /// A temporal value with a part below the microsecond (0A000 "not yet", stage 3g, 3g.P0
    /// U1): values hold microseconds.
    TemporalPrecision,
}

/// What went wrong in a GQL query: the kind of a [`TraverserError::Gql`] (feature `gql`).
///
/// Each kind has a default GQLSTATUS code (ISO/IEC 39075, which derives its codes from the
/// SQLSTATE of ISO/IEC 9075); a single error may carry a more specific code of the same class.
/// The codes other than `42001` are **provisional** and may still change before the GQL support
/// is complete:
///
/// | Kind | Default GQLSTATUS | Meaning of the code |
/// |---|---|---|
/// | [`Syntax`](Self::Syntax) | `42001` | invalid syntax |
/// | [`Semantic`](Self::Semantic) | `42002` | invalid reference (provisional) |
/// | [`Refused`](Self::Refused) | `0A000` | feature not supported (provisional) |
/// | [`NotYet`](Self::NotYet) | `0A000` | feature not supported (provisional) |
/// | [`Limit`](Self::Limit) | `54001` | statement too complex (provisional, SQLSTATE) |
/// | [`Data`](Self::Data) | `22000` | data exception; an error carries the specific code: `22012` division by zero, `22003` numeric value out of range, `22G03` invalid value type, `G1001` dependent object error: edges still exist (a `DELETE` of a node with edges) |
#[cfg(feature = "gql")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum GqlErrorKind {
    /// The query does not parse: an unexpected token, a missing keyword, an unclosed bracket.
    Syntax,
    /// The query parses but does not bind: an unknown or redeclared variable, a misplaced
    /// parameter, a variable used as the wrong kind of element.
    Semantic,
    /// A construct `gql()` refuses for good: the undirected edge forms `~[e]~`, `<~`, `~>`,
    /// vendor procedures (`CALL db.labels()`) and commands (`LOAD CSV`, `CREATE DATABASE`),
    /// catalog commands (`CREATE INDEX`, `CREATE CONSTRAINT`), `CALL { } IN TRANSACTIONS` (one
    /// query is one unit), Cypher `id()`.
    Refused,
    /// A construct of a later version of the GQL support, such as a parenthesized path
    /// pattern nested in another one `(((a)-->(b)){2}){1,3}`, `sin(x)`, `EXCEPT`, or Cypher's
    /// `FOREACH` write clause.
    NotYet,
    /// The query exceeds a fixed bound of the parser, such as the nesting depth.
    Limit,
    /// The query is valid, but evaluating an expression on a row of the data failed while the
    /// traversal ran: a division by zero, a numeric overflow, an operand of the wrong kind
    /// (`'a' + 1`, `type(v)` of a node). Unlike the other kinds it is raised at run time, by the
    /// row it failed on.
    Data,
}

#[cfg(feature = "gql")]
impl GqlErrorKind {
    /// The default GQLSTATUS code of this kind (the table on [`GqlErrorKind`]).
    pub(crate) fn default_gqlstatus(self) -> &'static str {
        match self {
            GqlErrorKind::Syntax => "42001",
            GqlErrorKind::Semantic => "42002",
            GqlErrorKind::Refused | GqlErrorKind::NotYet => "0A000",
            GqlErrorKind::Limit => "54001",
            GqlErrorKind::Data => "22000",
        }
    }
}

#[cfg(feature = "gql")]
impl std::fmt::Display for GqlErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            GqlErrorKind::Syntax => "syntax error",
            GqlErrorKind::Semantic => "semantic error",
            GqlErrorKind::Refused => "unsupported construct",
            GqlErrorKind::NotYet => "construct not supported yet",
            GqlErrorKind::Limit => "limit exceeded",
            GqlErrorKind::Data => "data exception",
        })
    }
}

/// Renders an `UnsupportedCardinality` error.
fn unsupported_cardinality_message(
    step: &str,
    cardinality: crate::value::Cardinality,
    on_edge: bool,
) -> String {
    if on_edge {
        format!(
            "Property cardinality can only be set for a Vertex: {step}(Cardinality.{cardinality}, ..) was applied to an edge"
        )
    } else {
        format!(
            "Cardinality.{cardinality} is not supported: Graphersal stores one value per property key ({step}() would write a second value)"
        )
    }
}

/// Renders a `TokenPropertyOutsideAdd` error.
fn token_property_outside_add_message(token: &str) -> String {
    if token == "T.label" {
        format!(
            "property({token}, ..) can only set the label of a vertex add_v() is creating (an edge's label comes from add_e(label))"
        )
    } else {
        format!(
            "property({token}, ..) can only set the id of an element add_v()/add_e() is creating"
        )
    }
}

/// Renders the active loop names of an `UnknownLoopName` error.
fn active_loops(active: &[String]) -> String {
    if active.is_empty() {
        String::new()
    } else {
        format!(" (active loops: {})", active.join(", "))
    }
}

/// Converts a step-name argument (a `&str` or a pre-rendered `String`) into an owned `String`.
pub(crate) trait StepNameSource {
    /// Consumes this value, producing the step name.
    fn into_step_name(self) -> String;
}

impl StepNameSource for &str {
    fn into_step_name(self) -> String {
        self.to_string()
    }
}

impl StepNameSource for String {
    fn into_step_name(self) -> String {
        self
    }
}

impl<F: FnOnce() -> String> StepNameSource for F {
    fn into_step_name(self) -> String {
        self()
    }
}

/// Where a failing step sits in the optimized plan: the child traversal its level is (`None` at
/// the top level) and its position in that level. See `TraverserError::StepFailed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StepAt {
    pub(crate) child: Option<usize>,
    pub(crate) index: usize,
}

pub(crate) trait ResultExt<T> {
    #[allow(dead_code)]
    fn with_step<S: StepNameSource>(self, step_name: S) -> TraverserResult<T>;

    /// Wraps a step failure together with the position and physical plan of the traversal
    /// level it occurred in, so nested failures can be addressed by a step path.
    fn with_step_frame<S: StepNameSource, F: FnOnce() -> StepFrame>(
        self,
        step_name: S,
        at: StepAt,
        frame: F,
    ) -> TraverserResult<T>;

    #[allow(dead_code)]
    fn with_step_element<S: StepNameSource, EId: StepNameSource>(
        self,
        step_name: S,
        element_id: EId,
    ) -> TraverserResult<T>;
}

impl<T, E> ResultExt<T> for Result<T, E>
where
    E: Into<TraverserError>,
{
    fn with_step<S: StepNameSource>(self, step_name: S) -> TraverserResult<T> {
        self.map_err(|err| {
            Box::new(TraverserError::StepFailed {
                spelling: crate::traversal::Spelling::Snake,
                step_name: step_name.into_step_name(),
                step_index: None,
                child_index: None,
                frame: None,
                element_id: None,
                source: Box::new(err.into()),
            })
        })
    }

    fn with_step_frame<S: StepNameSource, F: FnOnce() -> StepFrame>(
        self,
        step_name: S,
        at: StepAt,
        frame: F,
    ) -> TraverserResult<T> {
        self.map_err(|err| {
            Box::new(TraverserError::StepFailed {
                spelling: crate::traversal::Spelling::Snake,
                step_name: step_name.into_step_name(),
                step_index: Some(at.index),
                child_index: at.child,
                frame: Some(Box::new(frame())),
                element_id: None,
                source: Box::new(err.into()),
            })
        })
    }

    fn with_step_element<S: StepNameSource, EId: StepNameSource>(
        self,
        step_name: S,
        element_id: EId,
    ) -> TraverserResult<T> {
        self.map_err(|err| {
            Box::new(TraverserError::StepFailed {
                spelling: crate::traversal::Spelling::Snake,
                step_name: step_name.into_step_name(),
                step_index: None,
                child_index: None,
                frame: None,
                element_id: Some(element_id.into_step_name()),
                source: Box::new(err.into()),
            })
        })
    }
}

/// Mirrors the `E: Into<TraverserError>` impl above for an error that is already boxed (the
/// common case now that most fallible calls return [`TraverserResult`]). `Box<TraverserError>`
/// has no `Into<TraverserError>` impl in `std` (unboxing is not a conversion `std` provides), so
/// this impl does not overlap with the generic one.
impl<T> ResultExt<T> for Result<T, BoxedTraverserError> {
    fn with_step<S: StepNameSource>(self, step_name: S) -> TraverserResult<T> {
        self.map_err(|err| {
            Box::new(TraverserError::StepFailed {
                spelling: crate::traversal::Spelling::Snake,
                step_name: step_name.into_step_name(),
                step_index: None,
                child_index: None,
                frame: None,
                element_id: None,
                source: err,
            })
        })
    }

    fn with_step_frame<S: StepNameSource, F: FnOnce() -> StepFrame>(
        self,
        step_name: S,
        at: StepAt,
        frame: F,
    ) -> TraverserResult<T> {
        self.map_err(|err| {
            Box::new(TraverserError::StepFailed {
                spelling: crate::traversal::Spelling::Snake,
                step_name: step_name.into_step_name(),
                step_index: Some(at.index),
                child_index: at.child,
                frame: Some(Box::new(frame())),
                element_id: None,
                source: err,
            })
        })
    }

    fn with_step_element<S: StepNameSource, EId: StepNameSource>(
        self,
        step_name: S,
        element_id: EId,
    ) -> TraverserResult<T> {
        self.map_err(|err| {
            Box::new(TraverserError::StepFailed {
                spelling: crate::traversal::Spelling::Snake,
                step_name: step_name.into_step_name(),
                step_index: None,
                child_index: None,
                frame: None,
                element_id: Some(element_id.into_step_name()),
                source: err,
            })
        })
    }
}

impl TraverserError {
    /// The `help()` text in the canonical snake_case spelling; [`DiagnosticError::help`]
    /// respells it with [`Self::spelling`].
    fn canonical_help(&self) -> Option<String> {
        match self {
            TraverserError::Value(err) => err.help(),
            TraverserError::Graph(err) => err.help(),
            TraverserError::Io(err) => err.help(),
            TraverserError::Script(err) => err.help(),
            TraverserError::Schema(err) => err.help(),
            TraverserError::StepFailed { .. } => match self.root_cause() {
                // The timeout advice rewrites the query itself, so it needs the outermost level.
                TraverserError::Timeout { limit_ms, .. } => {
                    Some(timeout_help(*limit_ms, self.outermost_steps()))
                }
                // The loop of a lowered GQL quantifier: advice on the quantifier, not on its
                // internal repeat() body.
                #[cfg(feature = "gql")]
                TraverserError::RepeatLimitExceeded {
                    max_loops, body, ..
                } if body.contains(GQL_TRAIL) => Some(gql_repeat_limit_help(
                    *max_loops,
                    self.innermost_failed_step().unwrap_or(body),
                    self.gql_subquery_place(),
                )),
                #[cfg(feature = "gql")]
                TraverserError::ResourceLimitExceeded {
                    limit, max, ..
                } if limit == "traversal.max_traversers"
                    && self
                        .outermost_steps()
                        .is_some_and(|steps| steps.iter().any(|s| s.contains("gql:"))) =>
                {
                    Some(gql_traverser_limit_help(
                        *max,
                        &self.outermost_steps().unwrap_or_default(),
                        self.gql_subquery_steps(),
                        self.gql_subquery_place(),
                    ))
                }
                // A value a `gql:*` step built (a function result, a collect_list() list): advice
                // on the GQL query, not on the Gremlin materializing steps.
                #[cfg(feature = "gql")]
                TraverserError::ResourceLimitExceeded {
                    limit,
                    max,
                    observed,
                    ..
                } if limit == "traversal.max_materialized_bytes"
                    && self
                        .innermost_failed_step()
                        .is_some_and(|step| step.starts_with("gql:")) =>
                {
                    Some(gql_materialized_limit_help(*max, *observed))
                }
                TraverserError::RepeatLimitExceeded {
                    max_loops,
                    loop_name,
                    body,
                } => Some(repeat_limit_help(
                    *max_loops,
                    loop_name.as_deref(),
                    body,
                    self.outermost_steps(),
                )),
                // A schema violation of a GQL INSERT, or of the create of a MERGE (stage 3e:
                // its creator is the INSERT routine): the fix is GQL text, not Gremlin steps.
                #[cfg(feature = "gql")]
                TraverserError::Schema(schema)
                | TraverserError::Graph(GraphError::Schema(schema))
                    if self
                        .innermost_failed_step()
                        .is_some_and(|step| step.starts_with("gql:insert_")) =>
                {
                    let merge = self.gql_merge_pattern();
                    gql_schema_help(schema, merge.as_deref()).or_else(|| schema.help())
                }
                // A schema violation of a GQL SET / REMOVE (stage 3c): the fix is GQL text too.
                #[cfg(feature = "gql")]
                TraverserError::Schema(schema)
                | TraverserError::Graph(GraphError::Schema(schema))
                    if self.innermost_failed_step().is_some_and(|step| {
                        step.starts_with("gql:set(") || step.starts_with("gql:remove(")
                    }) =>
                {
                    gql_set_schema_help(schema, false).or_else(|| schema.help())
                }
                // A schema violation of the ON CREATE / ON MATCH items of a MERGE (stage 3e):
                // SET items, so the SET fix in MERGE form.
                #[cfg(feature = "gql")]
                TraverserError::Schema(schema)
                | TraverserError::Graph(GraphError::Schema(schema))
                    if self
                        .innermost_failed_step()
                        .is_some_and(|step| step.starts_with("gql:merge(")) =>
                {
                    gql_set_schema_help(schema, true).or_else(|| schema.help())
                }
                // A GQL read, write or `RETURN` of an element a `DELETE` removed earlier (W27,
                // W28): the fix is GQL text, not the Gremlin `element_map()` advice.
                #[cfg(feature = "gql")]
                TraverserError::Graph(GraphError::ElementRemoved { kind })
                    if self
                        .innermost_failed_step()
                        .is_some_and(|step| step.starts_with("gql:")) =>
                {
                    Some(gql_element_removed_help(*kind))
                }
                TraverserError::RepeatWithoutBody { modulator } => {
                    Some(repeat_without_body_help(modulator, self.failing_level_prefix()))
                }
                TraverserError::RepeatInject { .. } => Some(repeat_inject_help()),
                TraverserError::OptionWithoutBranch { modulator } => {
                    Some(option_without_branch_help(modulator))
                }
                // The corrected query is the failing one with a valid max_depth.
                TraverserError::InvalidOption { key, .. } if key == GLOB_PATH_MAX_DEPTH => {
                    Some(glob_path_max_depth_help(self.outermost_steps()))
                }
                // The advice rewrites the failing query when the math() step is at its top level.
                TraverserError::MathExpression { expr, kind, .. } => {
                    Some(math_expression_help(expr, kind, self.outermost_steps()))
                }
                root => root.help(),
            },
            TraverserError::Spelled { source, .. } => source.canonical_help(),
            TraverserError::Timeout { limit_ms, .. } => Some(timeout_help(*limit_ms, None)),
            TraverserError::Cancelled { .. } => Some(cancelled_help()),
            TraverserError::Embed(err) => Some(err.help()),
            TraverserError::InvalidOption { key, .. } => Some(invalid_option_help(key)),
            TraverserError::LockedOption { key } => Some(locked_option_help(key)),
            TraverserError::PermissionDenied { reason, .. }
                if reason.starts_with(SAVED_QUERY_READ_ONLY) =>
            {
                Some(
                    "A read-only saved query (dialect graphersal-rhai/1 or graphersal-gql/1, or any \
                     query called from one) never writes: not the data, the schema, the saved queries or files, also \
                     not through a query it calls or a traversal it returns (a g.query(..) result \
                     cannot be continued with a writing step). A query whose body writes data \
                     becomes a query that writes when it is saved again: define it once more \
                     with g.define_query(..) and g.get_query(\"q\")[\"effects\"] lists what it \
                     may write. Or write in a traversal of your own after the call, for example \
                     let ids = g.query(\"q\").id().to_list(); \
                     g.v(ids).property(\"seen\", true).to_list(). A graph the query builds \
                     itself (GraphSource::empty(), __) stays writable."
                        .to_string(),
                )
            }
            TraverserError::PermissionDenied { reason, .. }
                if reason.starts_with(SAVED_QUERY_DATA_ONLY) =>
            {
                Some(
                    "A saved query that writes changes data only: never the schema, saved \
                     queries, indexes, compression rules or files. Change those in a script of \
                     your own, outside the query."
                        .to_string(),
                )
            }
            TraverserError::PermissionDenied { reason, .. }
                if reason.starts_with(SAVED_QUERY_OUTSIDE_EFFECTS) =>
            {
                Some(
                    "The effects of a saved query are inferred from its body when it is saved \
                     and checked on every write of a call; this write was not inferred, which is \
                     a bug of Graphersal. Please report the body of the query \
                     (g.get_query(\"q\")[\"body\"]). Until it is fixed, write in a traversal of \
                     your own after the call."
                        .to_string(),
                )
            }
            TraverserError::PermissionDenied { step, request, .. } => {
                Some(permission_denied_help(step, &request.as_request()))
            }
            TraverserError::RepeatLimitExceeded {
                max_loops,
                loop_name,
                body,
            } => Some(repeat_limit_help(*max_loops, loop_name.as_deref(), body, None)),
            TraverserError::BulkOverflow { step } => Some(bulk_overflow_help(step)),
            TraverserError::BulkExpansionLimit {
                step,
                requested,
                limit,
            } => Some(bulk_expansion_limit_help(step, *requested, *limit)),
            TraverserError::ResourceLimitExceeded {
                limit,
                max,
                observed,
            } => Some(resource_limit_help(limit, *max, *observed)),
            TraverserError::UnknownLoopName { name, active } => {
                Some(unknown_loop_name_help(name, active))
            }
            TraverserError::RepeatWithoutBody { modulator } => {
                Some(repeat_without_body_help(modulator, None))
            }
            TraverserError::RepeatInject { .. } => Some(repeat_inject_help()),
            TraverserError::OptionWithoutBranch { modulator } => {
                Some(option_without_branch_help(modulator))
            }
            TraverserError::QueryExecution { query, reason }
                if query == "add_e" && reason.contains("from()") =>
            {
                Some(add_e_endpoint_help())
            }
            TraverserError::QueryExecution { query, reason }
                if query == "tree" && reason.contains("already holds") =>
            {
                Some(tree_key_clash_help(reason))
            }
            TraverserError::QueryExecution { query, reason }
                if (query == "group" || query == "group_count")
                    && (reason.contains("already holds") || reason.contains("reduce differently")) =>
            {
                Some(group_key_clash_help(reason))
            }
            TraverserError::QueryExecution { query, reason }
                if query == "id"
                    && reason == crate::traversal::step::map_steps::step_id::PROPERTY_ELEMENT_ID_REASON =>
            {
                Some(
                    "Graphersal has no property ids, so id() cannot read one from properties(). A \
                     vertex property is identified by its vertex id and key: read the key with \
                     g.v().properties().key(), or the owning vertex id with \
                     g.v().properties().element().id(). label() and has_label() work on \
                     properties(): the label of a vertex property is its key. The same holds \
                     for by(T.id) over properties(): there is no id to sort, group or dedup by; \
                     use by(T.label) (the key) or by(__.element().id())."
                        .to_string(),
                )
            }
            TraverserError::QueryExecution { query, .. }
                if query == "by(T.label)" || query == "by(T.id)" =>
            {
                let token = &query["by(".len()..query.len() - 1];
                Some(format!(
                    "{token} reads the label or id of a graph element, so the stream reaching this \
                     by() must hold vertices, edges or vertex properties (the label of a vertex \
                     property is its key). The values here are plain data, which have no \
                     {token}: order or group the elements before extracting values, or use a \
                     property key or a child traversal instead, for example \
                     g.v().order().by(\"name\") or g.v().order().by(__.values(\"name\")). \
                     To sort plain values, use order() with no by()."
                ))
            }
            TraverserError::QueryExecution { query, .. } if query == "has_traversal" => Some(
                "has(key, traversal) runs the traversal on the value of a property, on the id \
                 (T.id) or on the label (T.label), for example \
                 g.v().has(\"name\", __.is(\"marko\")), g.v().has(T.id, __.is(\"1\")) or \
                 g.v().has(T.label, __.is(\"person\")) (Rust: has_traversal(\"name\", ..), \
                 has_traversal(RecordKey::Id, ..), has_traversal(RecordKey::Label, ..)). An edge's \
                 endpoints are no key here: test them with a traversal, for example \
                 g.e().where(__.out_v().has(\"name\", \"marko\"))."
                    .to_string(),
            ),
            TraverserError::QueryExecution { query, reason } if query == "to_graph" => {
                Some(to_graph_help(reason))
            }
            TraverserError::QueryExecution { query, .. } if query == "subgraph()" => {
                Some(
                    "subgraph() needs the name of the side effect that collects the edges. Use \
                     subgraph(\"sg\") and read the graph with cap(\"sg\"): \
                     g.e(0).subgraph(\"sg\").cap(\"sg\"), or the short terminal to_graph() that \
                     needs no label: g.e(0).to_graph()."
                        .to_string(),
                )
            }
            TraverserError::QueryExecution { query, reason } if query == "subgraph" => {
                Some(subgraph_help(reason))
            }
            TraverserError::QueryExecution { query, .. } if query == "property(map)" => {
                Some(property_map_help())
            }
            TraverserError::QueryExecution { .. } => {
                Some("Review traversal query steps and conditions.".to_string())
            }
            TraverserError::UnsupportedCardinality { step, on_edge, .. } => {
                Some(unsupported_cardinality_help(step, *on_edge))
            }
            TraverserError::TokenPropertyOutsideAdd { token } => {
                Some(token_property_outside_add_help(token))
            }
            TraverserError::InvalidMergeMap { step, reason, .. } => {
                Some(invalid_merge_map_help(step, reason))
            }
            TraverserError::MergeOverride { step, key, .. } => Some(merge_override_help(step, key)),
            TraverserError::MergeVertexUnresolved { direction, reason } => {
                Some(merge_vertex_unresolved_help(*direction, reason))
            }
            TraverserError::NotImplemented { .. } => {
                Some("This feature is not yet supported in current release.".to_string())
            }
            TraverserError::SackArenaExhausted { .. } => Some(
                "Every update of a sack to a new distinct value stores one more value, so the \
                 sack grew past what one execution can address.\n\
                 Keep sacks to a few distinct values (a counter that is incremented a bounded \
                 number of times, a flag), narrow the stream before the sack is updated, for \
                 example g.with_sack(0).v().limit(1000).out().sack(Operator.sum).by(__.constant(1)).sack(), \
                 or aggregate with a reducing step such as sum() or group_count() instead of \
                 carrying the running value in the sack."
                    .to_string(),
            ),
            TraverserError::PathArenaExhausted { .. } => Some(
                "Every traverser that reaches path(), simple_path(), cyclic_path() or other_v() \
                 records its whole path, so the traversal fans out into more positions than can \
                 be tracked.\n\
                 Cap the stream before it grows, for example g.v().out().limit(1000).path(), \
                 narrow the start set, for example g.v(\"1\").out().path() instead of \
                 g.v().out().path(), or remove the path consumer when only the end \
                 elements are needed.\n\
                 When the fan-out comes from a few starting elements each having many \
                 neighbours, cap it per starting element instead of across the whole stream with \
                 local(), for example g.v().local(__.out().limit(2)).path().\n\
                 When the paths walk in cycles, drop them early with simple_path(), for example \
                 g.v().both().both().simple_path().path(), or at every hop of a loop: \
                 g.v().repeat(__.both().simple_path()).times(2).path()."
                    .to_string(),
            ),
            TraverserError::OtherVWithoutOrigin { .. } => Some(
                "other_v() reads the vertex the traversal visited right before reaching this \
                 edge, so it needs a vertex earlier in the same traversal, for example \
                 g.v(\"1\").both_e(\"knows\").other_v() or out().out_e().other_v(). Starting \
                 directly from edges has no such vertex, for example g.e().other_v(); use \
                 both_v() there instead to get both endpoints, or out_v()/in_v() to pick one side \
                 unconditionally."
                    .to_string(),
            ),
            TraverserError::InvalidRange { .. } => Some(
                "range(low, high) slices the stream to [low, high): low must be zero or a \
                 positive integer, and high must be -1 (meaning \"no upper bound\", for example \
                 g.v().range(2, -1) to skip the first two and take the rest) or an integer at \
                 least as large as low, for example g.v().range(0, 10) for the first page or \
                 g.v().range(10, 20) for the next. To take a fixed number from the start, use \
                 limit(n) instead, for example g.v().limit(10). The same bounds apply to \
                 the Scope.local form, which slices the list or map each traverser holds, for \
                 example g.v().values(\"name\").fold().range(Scope.local, 1, 3); its \
                 limit(Scope.local, n), skip(Scope.local, n) and tail(Scope.local, n) take a \
                 count of zero or more, for example \
                 g.v().values(\"name\").fold().limit(Scope.local, 2)."
                    .to_string(),
            ),
            TraverserError::InvalidModulator {
                step,
                modulator,
                reason,
                ..
            } if *modulator == crate::traversal::with_reason::MODULATOR => {
                Some(with_modulator_help(step, reason))
            }
            TraverserError::InvalidModulator { step, reason, .. } => {
                Some(invalid_modulator_help(step, reason))
            }
            TraverserError::SourceStepInChild { step } => Some(source_step_in_child_help(step)),
            TraverserError::MathExpression { expr, kind, .. } => {
                Some(math_expression_help(expr, kind, None))
            }
            #[cfg(feature = "gql")]
            TraverserError::Gql {
                kind,
                gqlstatus,
                context,
                ..
            } => Some(gql_help(
                *kind,
                gqlstatus,
                &context.expected,
                context.suggestion.as_deref(),
                context.alternative.as_deref(),
                context.topic,
            )),
            TraverserError::Fail { .. } => Some(
                "The query stopped on purpose: a traverser reached its fail() step. If that \
                 branch should not fail, remove fail() or guard it so that only unexpected \
                 traversers reach it, for example \
                 g.v().coalesce(__.out(\"knows\"), __.fail(\"no friends\")) fails only for a \
                 vertex without an out(\"knows\") neighbour, and \
                 g.v().choose(__.has(\"age\"), __.values(\"age\"), __.fail(\"age missing\")) \
                 only for a vertex without an age. A mutating traversal that fails is rolled \
                 back as a whole."
                    .to_string(),
            ),
        }
    }

    /// The spelling of step names in this error's diagnostic: the one the outermost
    /// [`TraverserError::StepFailed`] carries (`g.with("render.spelling", ..)`), snake_case
    /// for every other error.
    pub fn spelling(&self) -> crate::traversal::Spelling {
        match self {
            TraverserError::StepFailed { spelling, .. }
            | TraverserError::Spelled { spelling, .. } => *spelling,
            _ => crate::traversal::Spelling::Snake,
        }
    }

    /// Stores the rendered steps of this error in `spelling` (step names and plan frames of
    /// every level) and marks the outermost step failure with it, so `help()` and the
    /// diagnostic body are respelled too. The steps are rendered in snake_case; called once
    /// per execution when `render.spelling` is not the default.
    pub(crate) fn respell(&mut self, spelling: crate::traversal::Spelling) {
        use crate::traversal::spelling::respell;
        if spelling == crate::traversal::Spelling::Snake
            || matches!(self, TraverserError::Spelled { .. })
        {
            return;
        }
        // The operation a permission denial names is data, not a call: rename it here.
        if let TraverserError::PermissionDenied { step, .. } = self.root_cause_mut() {
            *step = if step.contains('(') {
                respell(step, spelling).into_owned()
            } else {
                crate::traversal::spelling::camel_name(step).into_owned()
            };
        }
        if !matches!(self, TraverserError::StepFailed { .. }) {
            // No step frame carries the spelling: the wrapper does.
            let source =
                std::mem::replace(self, TraverserError::NotImplemented { feature: "respell" });
            *self = TraverserError::Spelled {
                spelling,
                source: Box::new(source),
            };
            return;
        }
        let mut outermost = true;
        let mut current = self;
        while let TraverserError::StepFailed {
            step_name,
            frame,
            source,
            spelling: marked,
            ..
        } = current
        {
            *step_name = respell(step_name, spelling).into_owned();
            if let Some(frame) = frame {
                for step in &mut frame.steps {
                    *step = respell(step, spelling).into_owned();
                }
                if let Some(logical) = &mut frame.logical {
                    for step in &mut logical.steps {
                        *step = respell(step, spelling).into_owned();
                    }
                }
            }
            if outermost {
                *marked = spelling;
                outermost = false;
            }
            current = source;
        }
    }
}

impl DiagnosticError for TraverserError {
    fn help(&self) -> Option<String> {
        let help = self.canonical_help()?;
        #[cfg(feature = "gql")]
        if matches!(self.root_cause(), TraverserError::Gql { .. })
            || self
                .innermost_failed_step()
                .is_some_and(|step| step.starts_with("gql:"))
        {
            // The GQL functions in the advice are GQL text; the Gremlin steps are respelled.
            // So also for a limit a `gql:*` step reached (`collect_list()` stays as written).
            let gql_function = |name: &str| crate::gql::parser::lookup_function(name).is_some();
            return Some(
                crate::traversal::spelling::respell_keeping(&help, self.spelling(), gql_function)
                    .into_owned(),
            );
        }
        Some(crate::traversal::spelling::respell(&help, self.spelling()).into_owned())
    }

    fn details(&self) -> Vec<(String, String)> {
        match self {
            TraverserError::Spelled { source, .. } => source.details(),
            TraverserError::Value(err) => err.details(),
            TraverserError::Graph(err) => err.details(),
            TraverserError::Io(err) => err.details(),
            TraverserError::Script(err) => err.details(),
            TraverserError::Schema(err) => err.details(),
            TraverserError::StepFailed { .. } => {
                let mut details = Vec::new();
                let mut path: Vec<String> = Vec::new();
                let mut current = self;
                while let TraverserError::StepFailed {
                    step_index,
                    child_index,
                    frame,
                    source,
                    ..
                } = current
                {
                    path.extend(level_segments(*child_index, *step_index));
                    if let Some(frame) = frame {
                        match frame.render_logical() {
                            Some(logical) => {
                                details.push((format!("at #{}", path.join(".")), logical));
                                details.push(("plan".to_string(), frame.render()));
                            }
                            None => {
                                details.push((format!("at #{}", path.join(".")), frame.render()))
                            }
                        }
                    }
                    current = source;
                }
                details
            }
            TraverserError::MathExpression {
                expr,
                column,
                width,
                ..
            } => vec![(
                "equation".to_string(),
                math_underline(expr, *column, *width),
            )],
            #[cfg(feature = "gql")]
            TraverserError::Gql {
                line,
                column,
                width,
                gqlstatus,
                context,
                ..
            } => gql_details(
                &context.query,
                *line,
                *column,
                *width,
                &context.expected,
                context.suggestion.as_deref(),
                gqlstatus,
            ),
            _ => Vec::new(),
        }
    }

    /// Renders nested step failures as a single step trace followed by the root cause,
    /// instead of one `Caused by:` line per traversal level.
    fn diagnostic_body(&self) -> String {
        let root = self.root_cause();
        // A GQL error's message and source excerpt are GQL text: never respelled
        // (`element_id(p)` stays as written, so the caret stays under its span).
        #[cfg(feature = "gql")]
        let gql_root = matches!(root, TraverserError::Gql { .. });
        #[cfg(not(feature = "gql"))]
        let gql_root = false;
        if let TraverserError::Spelled { spelling, source } = self {
            let body = source.diagnostic_body();
            if gql_root {
                return body;
            }
            return crate::traversal::spelling::respell(&body, *spelling).into_owned();
        }
        let mut buf = format!("Error: {self}");
        push_details(&mut buf, &self.details());

        if !std::ptr::eq(root, self) {
            // The message of a `gql:error` marker's failure is the GQL error already.
            if !(gql_root && self.to_string().starts_with(&root.to_string())) {
                buf.push_str(&format!("\nCaused by: {root}"));
            }
            push_details(&mut buf, &root.details());
        }

        let mut current: Option<&dyn std::error::Error> = std::error::Error::source(root);
        while let Some(src) = current {
            buf.push_str(&format!("\nCaused by: {src}"));
            current = src.source();
        }
        // The stored plan frames are respelled already (`TraverserError::respell`); this brings
        // the step names inside the root cause's own message in line with them. A GQL root
        // cause names no step, so its text is left as written.
        if gql_root {
            return buf;
        }
        match crate::traversal::spelling::respell(&buf, self.spelling()) {
            std::borrow::Cow::Borrowed(_) => buf,
            std::borrow::Cow::Owned(respelled) => respelled,
        }
    }
}

/// Example query the timeout advice is built on when the failing query is unknown.
const TIMEOUT_EXAMPLE_STEPS: [&str; 4] = ["v()", "out()", "out()", "count()"];

/// Advice for a traversal that ran past its `evaluationTimeout`, with every suggestion shown
/// as a rewrite of the failing query (`steps`, outermost level), or of an example query when
/// the query is unknown.
fn timeout_help(limit_ms: u64, steps: Option<Vec<String>>) -> String {
    let steps = steps.filter(|steps| !steps.is_empty()).unwrap_or_else(|| {
        TIMEOUT_EXAMPLE_STEPS
            .iter()
            .map(|s| s.to_string())
            .collect()
    });
    // Narrowing and capping the stream both belong right after the start step.
    let with_after_start = |inserted: &str| {
        let mut rewritten = steps.clone();
        rewritten.insert(1, inserted.to_string());
        runnable_glob_path_options(&rewritten.join("."))
    };
    let query = runnable_glob_path_options(&steps.join("."));
    let raised = limit_ms.saturating_mul(2).max(1);
    // A '**' walk of glob_path() is bounded by max_depth.
    let glob_path_advice = glob_path_query_with_max_depth(&steps)
        .map(|bounded| {
            format!(
                "Bound the '**' walk of glob_path() with max_depth, the deepest level below the \
                 start vertex it visits, for example g.{bounded}.\n"
            )
        })
        .unwrap_or_default();
    // A loop is the usual reason a small query runs long: bound it rather than the stream.
    let repeat_advice = if query.contains("repeat(") {
        "Bound every repeat() loop with times() or until(), and skip cycles with simple_path() \
         in its body, for example g.v().repeat(__.out().simple_path()).times(3) or \
         g.v().repeat(__.out().simple_path()).until(__.has(\"name\", \"<value>\")).\n\
         Equal traversers are merged automatically inside repeat(), so a loop over converging \
         paths stays cheap; path() and simple_path() record full paths, which prevents the \
         merge and keeps exponential path counts expensive, so leave them out when you only \
         need the reached elements or a count. A sack merge operator or with_bulk(false) turns \
         automatic merging off: add barrier() inside the loop body, for example \
         g.v().repeat(__.out().barrier()).times(3).count().\n"
    } else {
        ""
    };
    // A has() on a labelled start reads every element of the label: a property index serves it.
    let index_advice = property_index_advice(&query);
    // Duplicates pile up where many paths converge on one vertex: merge them between two
    // expansions of the failing query, or in the example query when it has none.
    let barriered = barrier_between_expansions(&steps)
        .map(|rewritten| runnable_glob_path_options(&rewritten.join(".")))
        .unwrap_or_else(|| "v().out().barrier().out().barrier().out().count()".to_string());
    format!(
        "The traversal ran longer than its evaluationTimeout budget of {limit_ms} ms. The budget \
         is checked between steps, every 1024 traversers and every 1024 values a step \
         materializes (fold(), group(), order(), a terminal list); the step named above is the \
         one that was running.\n\
         {repeat_advice}\
         {glob_path_advice}\
         {index_advice}\
         Merge duplicate traversers: barrier() merges equal traversers so later steps process \
         each distinct one once, for example g.{barriered}.\n\
         Narrow the start set with has_label() or has_id(), for example \
         g.{narrowed}.\n\
         Cap the stream before it fans out with limit(), for example g.{limited}.\n\
         Or raise the budget: g.with(\"evaluationTimeout\", {raised}).{query}. It is an administrative option: a script whose host restricts it needs Update on that Option to set it.\n\
         To see which step spends the time, run it with execute() and a profile: the profile \
         up to the timed-out step comes back with the error, for example \
         g.{query}.execute(#{{profile: true}}).\n\
         If the traversal mutates the graph, nothing of it was applied: a failing traversal \
         is rolled back.",
        narrowed = with_after_start("has_label(\"<label>\")"),
        limited = with_after_start("limit(1000)"),
    )
}

/// The first string literal in `text` after `marker` (`has_label("person")` -> `person`).
fn literal_after<'a>(text: &'a str, marker: &str) -> Option<&'a str> {
    let rest = &text[text.find(marker)? + marker.len()..];
    let rest = rest.strip_prefix('"')?;
    Some(&rest[..rest.find('"')?])
}

/// Advice to declare a property index when the failing query filters a labelled start on a
/// property (`v().has_label("person").has("title", ..)`), or `""`.
fn property_index_advice(query: &str) -> String {
    let label = literal_after(query, "has_label(").or_else(|| literal_after(query, "labels: ["));
    let key = literal_after(query, ".has(");
    match (label, key) {
        (Some(label), Some(key)) => format!(
            "A has() on a property reads every \"{label}\" element: declare a property index on \
             it, for example g.create_index(#{{label: \"{label}\", key: \"{key}\"}}); \
             equality, P.within and ranges on \"{key}\" then read only the matching elements, \
             and .profile() shows [index: ..] on the start step (book page Property Indexes).\n"
        ),
        _ => String::new(),
    }
}

/// `steps` with a `barrier()` between every two adjacent expansion steps (`out()`, `in()`,
/// `both()`, `outE()`, `inV()`, ...), or `None` when there is no such pair.
fn barrier_between_expansions(steps: &[String]) -> Option<Vec<String>> {
    const EXPANSIONS: [&str; 10] = [
        "out(", "in(", "both(", "out_e(", "in_e(", "both_e(", "out_v(", "in_v(", "both_v(",
        "other_v(",
    ];
    let is_expansion = |step: &String| EXPANSIONS.iter().any(|name| step.starts_with(name));
    let mut rewritten = Vec::with_capacity(steps.len() + 1);
    let mut inserted = false;
    for (index, step) in steps.iter().enumerate() {
        rewritten.push(step.clone());
        if is_expansion(step) && steps.get(index + 1).is_some_and(is_expansion) {
            rewritten.push("barrier()".to_string());
            inserted = true;
        }
    }
    inserted.then_some(rewritten)
}

/// Advice for `inject()` as a direct step of a `repeat()` body.
fn repeat_inject_help() -> String {
    "TinkerPop forbids inject() as a step of a repeat() body: the injected values would enter \
     again in every iteration. Inject before the loop, for example \
     g.inject(\"y\").repeat(__.out()).times(2), or wrap it so it is not a direct step of the \
     body, for example g.v().repeat(__.union(__.identity(), __.inject(\"y\"))).times(2)."
        .to_string()
}

/// Advice for a source step (`with_sack()`, `with_side_effect()`) written inside a child
/// traversal: move it to the `g` the outer traversal starts from.
fn source_step_in_child_help(step: &str) -> String {
    let example = if step == "with_sack" {
        "g.with_sack(1).v().local(__.out().sack()) instead of g.v().local(__.with_sack(1).out().sack())"
    } else if step == "with_path" {
        "g.with_path().v().local(__.out().path()) instead of g.v().local(__.with_path().out().path()); \
         with_path() changes nothing in Graphersal (paths are recorded as the query reads them), \
         so it can also simply be left out"
    } else {
        "g.with_side_effect(\"x\", 1).v().local(__.select(\"x\")) instead of \
         g.v().local(__.with_side_effect(\"x\", 1).select(\"x\"))"
    };
    format!(
        "{step}() sets up the traversal source g: it applies to the whole query, child \
         traversals included, so it is written once on g, before the first step, for example \
         {example}. A child traversal sees the sacks and side effects of the outer traversal."
    )
}

/// Advice for a `by()` or `from()`/`to()` modulator a step cannot use at its position.
/// Advice for a step-level `with(WithOptions..)` no step took.
fn with_modulator_help(step: &str, reason: &str) -> String {
    let start = if reason == crate::traversal::with_reason::NO_STEP {
        "with(WithOptions..) configures the step right before it, so it cannot start a traversal."
            .to_string()
    } else {
        format!("{step}() cannot take this with() option.")
    };
    format!(
        "{start} Two steps take a WithOptions option: value_map() takes WithOptions.tokens \
         (with no value, or ids, labels, all, none), for example \
         g.v().value_map(\"name\").with(WithOptions.tokens, WithOptions.labels); index() takes \
         WithOptions.indexer with WithOptions.list or WithOptions.map, for example \
         g.v().values(\"name\").fold().index().with(WithOptions.indexer, WithOptions.map). \
         To set an execution option of the whole traversal use g.with(\"key\", value) with a \
         string key instead, for example g.with(\"random.seed\", 7)."
    )
}

fn invalid_modulator_help(step: &str, reason: &str) -> String {
    if reason == crate::traversal::by_reason::NO_STEP {
        return format!(
            "by() modulates the step right before it, so it cannot start a traversal. Write the \
             step first and the by() after it, for example g.v().order().by(\"name\"). {BY_STEPS}"
        );
    }
    if reason == crate::traversal::args_reason::NO_STEP {
        return format!(
            "from()/to() modulates the step right before it, so it cannot start a traversal. \
             Write the step first and from()/to() after it, for example \
             g.v(\"1\").add_e(\"knows\").to(\"2\"). {FROM_TO_STEPS}"
        );
    }
    if reason == crate::traversal::args_reason::ONE_LABEL {
        return format!(
            "from()/to() of {step}() takes exactly one step label, a window between two labelled \
             positions of the path, not a list: write g.v().as(\"a\").out().as(\"b\").path()\
             .from(\"a\").to(\"b\"). The label is set with as() before the step. {FROM_TO_STEPS}"
        );
    }
    if reason.ends_with(crate::traversal::args_reason::NOT_ACCEPTED) {
        return from_to_not_accepted_help(step);
    }
    if reason.ends_with(crate::traversal::by_reason::NOT_ACCEPTED) {
        return by_not_accepted_help(step);
    }
    if reason == crate::traversal::by_reason::COLUMN {
        return column_modulator_help(step);
    }
    if step == "glob_path" {
        return glob_path_modulator_help(reason);
    }
    if step == "dedup" {
        return dedup_modulator_help(reason);
    }
    if step == "value_map" {
        return value_map_modulator_help(reason);
    }
    if step == "aggregate" || step == "store" {
        return aggregate_modulator_help(step);
    }
    if step == "group" || step == "group_count" {
        return group_modulator_help(step, reason);
    }
    if step == "sample" {
        return "sample(n) takes at most one by(), the weight each traverser is drawn with, and it \
                must project a number: a numeric property or a traversal ending in a number, for \
                example g.e().sample(2).by(\"weight\") or g.v().sample(1).by(__.out_e().count()). \
                An order or Count is not a weight; to sort and then sample, order first: \
                g.v().order().by(\"age\").sample(2). Drop by() for a uniform sample."
            .to_string();
    }
    if step == "sack" {
        return "sack(Operator) updates the sack with one operand per traverser, so it takes a single \
                by(), for example g.with_sack(0).v().out_e().sack(Operator.sum).by(\"weight\"). To \
                combine several values into one operand, compute it in one traversal, for example \
                g.with_sack(0).v().sack(Operator.sum).by(__.values(\"age\").sum())."
            .to_string();
    }
    if reason == crate::traversal::ring_reason::KIND {
        return ring_modulator_help(step);
    }
    format!(
        "A step's by() modulators are positional: the first by() of {step}() means one thing, \
         the second another, and each position accepts only certain kinds of argument (a \
         property key, T.id/T.label, a traversal such as __.values(\"name\"), ...). \
         A list of vertex labels, such as by([\"person\", \"software\"]), is not a projection \
         {step}() can use. To keep only elements with certain labels, filter them with \
         has_label(...) before the step instead, for example \
         g.v().has_label(\"person\", \"software\").order().by(\"name\")."
    )
}

/// Advice for a `by(Column.keys)` / `by(Column.values)` on a step other than `order()`.
fn column_modulator_help(step: &str) -> String {
    format!(
        "by(Column.keys) / by(Column.values) reads the key or value of a map entry (or the keys or \
         values of a whole map), and only order() takes it, as a sort key, not {step}(). Sort map \
         entries with it, for example \
         g.v().has_label(\"person\").group().by(\"name\").by(__.out_e().values(\"weight\").sum()).unfold().order().by(Column.values, Order.desc), \
         or sort the entries inside each map with order(Scope.local).by(Column.values). To read a \
         map's keys or values as data, use select(Column.keys) / select(Column.values) after the \
         step instead, for example g.v().group().by(\"name\").select(Column.keys). {BY_STEPS}"
    )
}

/// The steps that take `by()` modulators, named in every advice about a misplaced `by()`.
const BY_STEPS: &str = "The steps that take by() are order(), dedup(), group(), group_count(), \
     select() with step labels, project(), path(), simple_path(), cyclic_path(), where(), tree(), \
     math(), value_map(), aggregate(), store(), sample() (the weight) and glob_path().";

/// Advice for a `by()` after a step that does not take `by()` modulators: where it belongs, with
/// a corrected query for the common cases.
fn by_not_accepted_help(step: &str) -> String {
    let fix = match step {
        "count" => {
            "To count per value, use group_count() with the by() instead, for example \
             g.v().group_count().by(\"name\") rather than g.v().count().by(\"name\")."
        }
        "sum" | "max" | "min" | "mean" | "median" => {
            "To aggregate a property, read it with values() first, for example \
             g.v().values(\"age\").sum() rather than g.v().sum().by(\"age\")."
        }
        "select" => {
            "They read a map as it is; put the by() on the step that builds the map, for \
             example g.v().group().by(\"name\").select(Column.keys)."
        }
        _ => {
            "Move the by() to the step it was meant for, for example g.v().out().order().by(\"name\") \
             sorts the neighbours by name; to read a property of each element, use values(), for \
             example g.v().out().values(\"name\") rather than g.v().out().by(\"name\")."
        }
    };
    let what = match step {
        "select" => "select(Column.keys) / select(Column.values)".to_string(),
        _ => format!("{step}()"),
    };
    format!(
        "by() modulates the step right before it, and {what} does not take one. {fix} {BY_STEPS}"
    )
}

/// The steps that take `from()`/`to()` modulators, named in every advice about a misplaced
/// `from()`/`to()`.
const FROM_TO_STEPS: &str = "The steps that take from()/to() are add_e(), path(), \
     simple_path() and cyclic_path().";

/// Advice for a `from()`/`to()` after a step that does not take `from()`/`to()` modulators.
fn from_to_not_accepted_help(step: &str) -> String {
    format!(
        "from()/to() modulates the step right before it, and {step}() does not take one. \
         add_e() accepts from()/to(), to point a new edge at existing elements or \
         labelled positions, for example g.v(\"1\").add_e(\"knows\").to(\"2\") or \
         g.v(\"1\").as(\"a\").out(\"knows\").add_e(\"co-developer\").from(\"a\"). An endpoint \
         can also be a traversal, g.v(\"1\").add_e(\"knows\").to(__.v(\"2\")), or a side-effect key, \
         g.with_side_effect(\"b\", \"2\").v(\"1\").add_e(\"knows\").to(\"b\"). \
         path(), simple_path() and cyclic_path() take them as a window onto the path between two \
         labelled positions, for example \
         g.v().as(\"a\").out().as(\"b\").path().from(\"a\").to(\"b\"). {FROM_TO_STEPS}"
    )
}

/// Advice for an order or `Count` passed to a step whose `by()` modulators form TinkerPop's
/// traversal ring (`select()`, `project()`, `path()`, `simple_path()`, `cyclic_path()`, `tree()`,
/// `math()`).
fn ring_modulator_help(step: &str) -> String {
    let (what, example) = match step {
        "select" => (
            "one by() per selected key, cycling when there are fewer by() than keys",
            "g.v(\"1\").as(\"a\").out(\"knows\").as(\"b\").select(\"a\", \"b\").by(\"name\") \
             reads the name of both a and b",
        ),
        "project" => (
            "one by() per key, cycling when there are fewer by() than keys",
            "g.v().project(\"name\", \"out\").by(\"name\").by(__.out().count())",
        ),
        "path" => (
            "one by() per path object, cycling along the path",
            "g.v(\"1\").out().path().by(\"name\")",
        ),
        "simple_path" | "cyclic_path" => (
            "one by() per object of the path (or of its from()/to() window), cycling along it, \
             before the simple/cyclic test",
            "g.v(\"1\").both().both().simple_path().by(\"name\")",
        ),
        "where" => (
            "the first by() projects the current object, each following one a step-label operand \
             of the predicate in textual order, cycling when there are fewer by() than operands",
            "g.v(\"1\").as(\"a\").out().where(P.gt(\"a\")).by(\"age\") compares the age of \
             each vertex with the age of a",
        ),
        "tree" => (
            "one by() per tree level, cycling along each path",
            "g.v(\"1\").out().tree().by(\"name\")",
        ),
        "math" => (
            "one by() per variable of the equation, cycling when there are fewer by() than \
             variables",
            "g.v().as(\"a\").out(\"knows\").as(\"b\").math(\"a + b\").by(\"age\")",
        ),
        _ => (
            "one by() per value, cycling",
            "g.v(\"1\").out().path().by(\"name\")",
        ),
    };
    format!(
        "{step}() applies its by() modulators round-robin, like TinkerPop's traversal ring: \
         {what}. Each by() is a projection: a property key such as by(\"name\"), T.id / T.label, \
         or a traversal such as __.out().count(). An order or Count projects nothing: drop it, \
         and sort afterwards with order().by(...) if an order was meant. For example {example}."
    )
}

/// Advice for a `by()` modulator `dedup()` cannot use, keyed on the `reason` constants of its
/// step module (any other reason is the shared label-list rejection).
fn dedup_modulator_help(reason: &str) -> String {
    use crate::traversal::step::flatbarrier_steps::step_dedup::reason as r;
    const WHAT: &str = "dedup() takes one by(): the projection each traverser is deduplicated by, \
         a property key such as by(\"name\"), T.id / T.label, or a traversal such as \
         __.out().count() whose first result is the key.";
    const EXAMPLE: &str = "For example g.v().dedup().by(T.label) keeps the first vertex of each \
         label.";
    let advice = if reason == r::KIND {
        "An order or Count is not a projection: drop it, and sort after dedup() with \
         order().by(...) if an order was meant, for example \
         g.v().dedup().by(T.label).order().by(\"name\")."
    } else if reason == r::TOO_MANY {
        "There is no second by(): to deduplicate by several values at once, combine them in one \
         traversal, for example g.v().dedup().by(__.values(\"name\", \"age\").fold())."
    } else {
        "A list of vertex labels, such as by([\"person\", \"software\"]), is not a projection: to \
         keep only elements with certain labels, filter them with has_label(...) before the step \
         instead, for example g.v().has_label(\"person\", \"software\").dedup().by(\"name\")."
    };
    format!("{WHAT} {advice} {EXAMPLE}")
}

/// Advice for a second `by()` on `aggregate()` or `store()`.
fn aggregate_modulator_help(step: &str) -> String {
    format!(
        "{step}() collects one projection of each traverser, so it takes a single by(), for \
         example g.v().{step}(\"x\").by(\"name\").cap(\"x\"). To collect several values per \
         traverser, project them in one traversal, for example \
         g.v().{step}(\"x\").by(__.values(\"name\", \"age\").fold()).cap(\"x\")."
    )
}

/// Advice for a `by()` modulator `valueMap()` cannot use, keyed on the `reason` constants of its
/// step module (any other reason is the shared label-list rejection).
fn value_map_modulator_help(reason: &str) -> String {
    use crate::traversal::step::map_steps::step_value_map::reason as r;
    const WHAT: &str = "value_map() wraps each property value in a list, such as {name: [marko], \
         age: [29]}, and its by() is a traversal applied to each of those lists; the first result \
         replaces the list.";
    const EXAMPLE: &str = "For example g.v().value_map().by(__.unfold()) returns the plain \
         values, {name: marko, age: 29}; element_map() returns them too, together with id and \
         label.";
    let advice = if reason == r::TOO_MANY {
        "value_map() takes a single by(): it is applied to every property's list. To treat the \
         properties differently, select them separately, for example \
         g.v().value_map(\"name\").by(__.unfold()), or use project()."
    } else if reason == r::KIND {
        "A property key, T.id / T.label, an order, a JSON path, Value or Count cannot take a \
         list: pass a traversal instead, or select the keys as arguments, for example \
         g.v().value_map(\"name\").by(__.unfold())."
    } else {
        "A list of vertex labels, such as by([\"person\", \"software\"]), is not a traversal: to \
         keep only elements with certain labels, filter them with has_label(...) before the step \
         instead, for example g.v().has_label(\"person\").value_map()."
    };
    format!("{WHAT} {advice} {EXAMPLE}")
}

/// Advice for a `by()` modulator `group()`/`group_count()` cannot use, keyed on the `reason`
/// constants of its step module (any other reason is the shared label-list rejection).
fn group_modulator_help(step: &str, reason: &str) -> String {
    use crate::traversal::step::barrier_steps::step_group::reason as r;
    const WHAT: &str = "group()'s by() modulators are positional: the first by() reads the \
         grouping key from each element, and the second reads the value kept under that key \
         (group_count() and by(Count) ask for a count there instead).";
    let advice = if reason == r::TOO_MANY {
        "group_count() counts the elements per key, so it takes a single by(), the key; there is \
         no value projection to give. To group and keep a projection per key, use group() with a \
         second by(), for example g.v().group().by(\"name\").by(\"age\") instead of \
         g.v().group_count().by(\"name\").by(\"age\")."
    } else if reason == r::ORDER {
        "An order, such as Order.desc, is not a projection: group() applies each by() to a key \
         or a value, it does not sort the result. Drop it, and sort afterward instead, for \
         example g.v().group().by(\"name\").unfold().order().by(Order.desc)."
    } else {
        "A list of vertex labels, such as by([\"person\", \"software\"]), is not a projection: to \
         keep only elements with certain labels, filter them with has_label(...) before the step \
         instead, for example g.v().has_label(\"person\", \"software\").group().by(\"name\")."
    };
    format!("{WHAT} {advice} For example g.v().{step}().by(\"name\").")
}

/// Advice for a `by()` modulator `glob_path()` cannot use, keyed on the `reason` constants of
/// its step module.
fn glob_path_modulator_help(reason: &str) -> String {
    use crate::traversal::step::flatmap_steps::step_glob_path::reason as r;
    const POSITIONS: &str = "glob_path()'s by() modulators are positional: the first by() is the \
         required match key a named pattern segment is compared with, a property key such as \
         by(\"name\") or T.id / T.label; the optional second by() is a non-empty list of vertex \
         labels that every visited vertex below the start must carry.";
    const EXAMPLE: &str = "For example g.v(\"root\").glob_path(\"**/*.rs\").by(\"name\")\
         .by([\"directory\",\"file\"]).";
    let advice = if reason == r::MISSING_KEY {
        "Add the match key as the first by()."
    } else if reason == r::LABELS_AS_KEY {
        "The first by() is the key and the label list goes second: put a key such as by(\"name\") \
         before the label list."
    } else if reason == r::KEY_KIND {
        "A traversal, an order, a JSON path, Value or Count cannot be the match key."
    } else if reason == r::JPATH_KEY {
        "A jpath(..) path reaches into a nested value, but the match key is the stored value of \
         one property: pass the plain property name, by(\"name\")."
    } else if reason == r::FILTER_KIND {
        "The second by() only filters by vertex label: pass a list such as by([\"directory\", \
         \"file\"]), or drop it to visit every vertex."
    } else if reason == r::EMPTY_LABELS {
        "An empty label list would admit no vertex: list at least one label, or drop the second \
         by() to visit every vertex."
    } else if reason == r::TOO_MANY {
        "There is no third by(): drop it."
    } else {
        ""
    };
    if advice.is_empty() {
        format!("{POSITIONS} {EXAMPLE}")
    } else {
        format!("{POSITIONS} {advice} {EXAMPLE}")
    }
}

/// Advice for an option value of the wrong shape, with a valid value for the key.
/// The parenthesised `(limit N, reached M)` tail of a [`TraverserError::ResourceLimitExceeded`]
/// message; empty when neither number is known.
fn limit_detail(max: Option<u64>, observed: Option<u64>) -> String {
    match (max, observed) {
        (Some(max), Some(value)) => format!(" (limit {max}, reached {value})"),
        (Some(max), None) => format!(" (limit {max})"),
        (None, Some(value)) => format!(" (reached {value})"),
        (None, None) => String::new(),
    }
}

/// How a host sets the memory budget `memory.limit`: the closing advice of every memory-budget
/// help text (a traversal's and a unit's).
const MEMORY_BUDGET_SETTING: &str = "The budget is a host setting (off by default): the \
     graphersal command line takes --memory-limit <bytes|auto>, the Python binding \
     Graph(memory_limit=...), a Rust host ExecutionPolicy::with_default(\"memory.limit\", bytes) \
     or a preset of graph_scope_with_options; \"auto\" is the cgroup memory limit minus \
     'memory.headroom'. Setting it in a query is an administrative option: it needs Update on \
     that Option, and is refused when the host locked it.";

/// Help for [`GraphError::ResourceLimitExceeded`]: a unit that is not a traversal (an import,
/// an applied change set, an outermost commit) grew the memory in use over `memory.limit`.
/// Names the operation and closes with the same host advice as a traversal's memory budget.
fn unit_resource_limit_help(
    operation: &str,
    limit: &str,
    max: Option<u64>,
    observed: Option<u64>,
) -> String {
    let raised = max.map_or(1_000_000, |max| max.saturating_mul(2).max(1));
    format!(
        "The {operation} would take the memory in use over the memory budget '{limit}'{}{}, so \
         it was stopped and everything it changed was rolled back, instead of the process \
         running out of memory. The figure is the process's live heap when the host installs \
         the tracking allocator (the graphersal command line does), else the engine's estimate: \
         the graph's footprint when the unit started (see g.memory_usage()) plus the elements it \
         added. A unit that only removes or rewrites data is never stopped, so make room first: \
         drop data you do not need, for example \
         g.with(\"memory.limit\", {raised}).v().has_label(\"tmp\").drop(), then import or \
         apply again; or give the host more memory. {MEMORY_BUDGET_SETTING}",
        max.map(|max| format!(" ({max} bytes)")).unwrap_or_default(),
        observed
            .map(|observed| format!("; it reached {observed} bytes"))
            .unwrap_or_default(),
    )
}

/// Help for [`TraverserError::ResourceLimitExceeded`]: names the knob that raises the limit and
/// shows a runnable example for it.
fn resource_limit_help(limit: &str, max: Option<u64>, observed: Option<u64>) -> String {
    let raised = max.map_or(1_000_000, |max| max.saturating_mul(2).max(1));
    match limit {
        "traversal.max_traversers" => format!(
            "The traversal kept more traversers alive between two steps than \
             'traversal.max_traversers' allows{}. This usually means an expansion such as \
             out().out().out() or a repeat() multiplies the stream. Narrow it, for example \
             g.v().has_label(\"person\").out().limit(100), aggregate with count() or \
             group_count() instead of listing, or raise the limit when the traversal source is \
             created, for example g.with(\"traversal.max_traversers\", {raised}).v().out().out().count(). \
             A host can lock this option with ExecutionPolicy::with_locked(\"traversal.max_traversers\"); \
             9223372036854775807 is as good as unlimited. Setting it is an administrative option: \
             a script whose host restricts it needs Update on that Option.",
            max.map(|max| format!(" ({max})")).unwrap_or_default()
        ),
        "script.max_value_depth" => format!(
            "A value the script handed to the engine (a step argument such as inject(x) or \
             property(\"k\", x), a parameter, or the script's result) nests arrays and maps \
             deeper than 'script.max_value_depth' allows{}. Deeply nested values are read \
             recursively and would overflow the native stack, so the depth is bounded where a \
             value enters the engine. Flatten the value, for example keep a list of items \
             instead of wrapping a list in a list in a loop: let xs = []; for i in 0..10 {{ \
             xs.push(i); }} g.inject(xs).count(Scope.local).next(). A trusted host raises the limit \
             with ScriptLimits::default().with_max_value_depth({raised}) passed to \
             graphersal::script::engine_with_limits(&limits, authorizer); the graphersal \
             command line takes --max-value-depth {raised}, the Python binding \
             graph.set_limits(max_value_depth={raised}). ScriptLimits::unlimited() keeps this \
             bound, because the native stack cannot be raised by configuration.",
            max.map(|max| format!(" ({max} levels)"))
                .unwrap_or_default()
        ),
        "traversal.max_value_depth" => format!(
            "The traversal built or wrote a value that nests arrays, maps or paths deeper than \
             'traversal.max_value_depth' allows{}. A repeat() whose body wraps its value again \
             on every iteration (repeat(__.fold()), repeat(__.path()), repeat(__.group())) or a \
             jpath(..) write with many segments does this. Bound the loop, for example \
             g.inject(1).repeat(__.fold()).times(10).next(), write to a shorter path, or raise \
             the limit when the traversal source is created, for example \
             g.with(\"traversal.max_value_depth\", {raised}).inject(1).repeat(__.fold()).times(200).count(). \
             Deeply nested values are read recursively, so keep the limit well below the \
             thousands. A host can lock this option with \
             ExecutionPolicy::with_locked(\"traversal.max_value_depth\"). Setting it is an \
             administrative option: a script whose host restricts it needs Update on that Option.",
            max.map(|max| format!(" ({max} levels)"))
                .unwrap_or_default()
        ),
        "traversal.max_materialized_bytes" => format!(
            "A step had to materialize a value or buffer larger than \
             'traversal.max_materialized_bytes' allows{}{}, by the engine's size estimate. \
             Steps that expand merged traversers into single items (fold(), aggregate(), \
             store(), group() value lists, a terminal list) do this, and so does a repeat() \
             whose body doubles its value on every iteration (repeat(__.path()), \
             repeat(__.group())). Prefer a step that works on the multiplicity directly: count() \
             instead of fold(), for example g.v().repeat(__.both()).times(8).count(); \
             group_count() to see how often each result occurs, for example \
             g.v().repeat(__.both()).times(8).group_count(); dedup() or limit(n) before the \
             materializing step, for example g.v().repeat(__.both()).times(8).dedup().fold(); and \
             bound a loop that grows its value, for example \
             g.inject(1).repeat(__.path()).times(10).count(). If you have the memory, raise the \
             limit when the traversal source is created, for example \
             g.with(\"traversal.max_materialized_bytes\", {raised}).v().repeat(__.both()).times(8).fold(). \
             A host can lock this option with \
             ExecutionPolicy::with_locked(\"traversal.max_materialized_bytes\"). Setting it is an \
             administrative option: a script whose host restricts it needs Update on that Option.",
            max.map(|max| format!(" ({max} bytes)")).unwrap_or_default(),
            observed
                .map(|observed| format!("; this one needed at least {observed} bytes"))
                .unwrap_or_default(),
        ),
        "memory.limit" => format!(
            "The traversal would take the memory in use over the memory budget \
             'memory.limit'{}{}, so it was stopped and everything it changed was rolled back, \
             instead of the process running out of memory. The figure is the process's live \
             heap when the host installs the tracking allocator (the graphersal command line \
             does), else the engine's estimate: the graph's footprint (see g.memory_usage()) \
             plus what the traversal holds and adds. Make the traversal hold less: count() \
             instead of fold(), for example g.v().repeat(__.both()).times(8).count(); \
             dedup() or limit(n) before a materializing step, for example \
             g.v().out().dedup().fold(); bound a loop that grows its value, for example \
             g.inject(1).repeat(__.path()).times(10).count(); write in smaller batches, for example \
             g.v().has_label(\"person\").limit(1000).property(\"seen\", true). When the graph \
             itself is over the budget, every traversal fails at its first step: drop data you \
             do not need under a raised budget, for example \
             g.with(\"memory.limit\", {raised}).v().has_label(\"tmp\").drop(), or give the \
             host more memory. {MEMORY_BUDGET_SETTING}",
            max.map(|max| format!(" ({max} bytes)")).unwrap_or_default(),
            observed
                .map(|observed| format!("; it reached {observed} bytes"))
                .unwrap_or_default(),
        ),
        "traversal.max_string_bytes" => format!(
            "A step built a string longer than 'traversal.max_string_bytes' allows{}{}. \
             replace(), concat(), conjoin() and format() can multiply the length of their input \
             (replace() of a short text by a long one at every match, concat() of a string with \
             itself in a loop); the limit stops them before the string is allocated, as \
             'script.max_string_size' does for the strings a script builds. Shorten the input or \
             the replacement, for example g.v().values(\"name\").replace(\"a\", \"b\"), \
             bound a loop that grows a string, for example \
             g.inject(\"x\").repeat(__.concat(__.identity())).times(10), or raise the limit when \
             the traversal source is created, for example \
             g.with(\"traversal.max_string_bytes\", {raised}).v().values(\"name\").replace(\"a\", \"aa\"). \
             A host can lock this option with \
             ExecutionPolicy::with_locked(\"traversal.max_string_bytes\"). Setting it is an \
             administrative option: a script whose host restricts it needs Update on that Option.",
            max.map(|max| format!(" ({max} bytes)")).unwrap_or_default(),
            observed
                .map(|observed| format!("; this one would be {observed} bytes"))
                .unwrap_or_default(),
        ),
        other if other.starts_with("script.") => {
            let field = other.trim_start_matches("script.");
            format!(
                "The script ran past its '{other}' limit{}. Bound the loop or the data it builds. \
                 A trusted host raises the limit with ScriptLimits::default().with_{field}({raised}) \
                 passed to graphersal::script::engine_with_limits(&limits, authorizer), or removes all limits \
                 with ScriptLimits::unlimited(). The graphersal command line takes \
                 --{flag} <n>, for example graphersal --{flag} {raised} -e 'g.v().count().next()'.",
                max.map(|max| format!(" of {max}")).unwrap_or_default(),
                flag = field.replace('_', "-"),
            )
        }
        "memory.allocation" => format!(
            "The traversal needed a block of memory{} the system could not provide, so it was \
             stopped and everything it changed was rolled back, instead of the process running \
             out of memory. This is not an option that can be raised: the query builds too many \
             values at once. Narrow it before the step that grows the stream, for example \
             g.v().has_label(\"person\").out().limit(100), aggregate with count() or \
             group_count() instead of listing, or bound the run with a memory budget, for \
             example g.with(\"memory.limit\", 1000000000).v().out().out().count(). {MEMORY_BUDGET_SETTING}",
            observed
                .map(|elements| format!(" for {elements} items"))
                .unwrap_or_default()
        ),
        other => format!(
            "The limit '{other}' was exceeded. Raise it through the option or configuration of \
             the same name, for example g.with(\"{other}\", {raised}).v().count()."
        ),
    }
}

/// Help for [`TraverserError::Cancelled`].
fn cancelled_help() -> String {
    "The query was stopped on request (the Stop button) and everything it changed was rolled \
     back; nothing of it is kept. Run it again to get its results. To bound a slow query instead \
     of stopping it by hand, give it a budget: g.with(\"evaluationTimeout\", 5000).v().out().count()."
        .to_string()
}

/// Help for [`TraverserError::BulkOverflow`].
fn bulk_overflow_help(step: &str) -> String {
    format!(
        "Equal traversers are counted as one record with a multiplicity, and the multiplicity at \
         step '{step}' grew beyond 9223372036854775807 (2^63 - 1), the largest exact count. \
         Narrow the start set, for example g.v().has_label(\"person\").repeat(__.out()).times(8).count() \
         instead of g.v().repeat(__.out()).times(8).count(), or bound the loop with a smaller \
         times(n) or an until() condition."
    )
}

/// Help for [`TraverserError::BulkExpansionLimit`].
fn bulk_expansion_limit_help(step: &str, requested: u64, limit: u64) -> String {
    format!(
        "Step '{step}' needs every value as its own item, but equal traversers are kept as one \
         record with a multiplicity (bulk). It has to expand each merged record into one value \
         per unit of multiplicity, which would produce {requested} values, more than the limit \
         of {limit} set by 'bulk.max_expansion'. Steps such as fold(), aggregate(), store(), \
         group() value lists and a terminal list do this. Prefer a step that works on the \
         multiplicity directly: count() instead of fold(), for example \
         g.v().repeat(__.out()).times(8).count(); group_count() to see how often each result \
         occurs, for example g.v().repeat(__.out()).times(8).values(\"name\").group_count(); \
         dedup() before the step to keep one item per distinct result, for example \
         g.v().repeat(__.out()).times(8).dedup().values(\"name\").fold(); or limit(n) before \
         the step to keep only the first n, for example \
         g.v().repeat(__.out()).times(8).limit(100).values(\"name\").fold(). If you really need \
         all {requested} values and have the memory for them, raise the limit when the traversal \
         source is created, for example \
         g.with(\"bulk.max_expansion\", {requested}).v().repeat(__.out()).times(8).values(\"name\").fold(). It is an administrative option: a script whose host restricts it needs Update on that Option to set it."
    )
}

fn invalid_option_help(key: &str) -> String {
    match key {
        "bulk.merge" => "Set 'bulk.merge' to true or false when the traversal source is created, \
             for example g.with(\"bulk.merge\", false).v().out().out().count(). Merged and \
             unmerged runs return the same results; false is a diagnosis aid."
            .to_string(),
        "traversal.max_traversers" => "Set 'traversal.max_traversers' to a positive integer number \
             of traversers when the traversal source is created, for example \
             g.with(\"traversal.max_traversers\", 50000000).v().out().out().count(); the default is \
             10000000, and 9223372036854775807 is as good as unlimited."
            .to_string(),
        "traversal.max_materialized_bytes" => "Set 'traversal.max_materialized_bytes' to a \
             positive integer number of bytes when the traversal source is created, for example \
             g.with(\"traversal.max_materialized_bytes\", 2147483648).v().repeat(__.both()).times(8).fold(); \
             the default is 1073741824 (1 GiB; 268435456 on wasm32)."
            .to_string(),
        "memory.limit" => "Set 'memory.limit' to a positive integer number of bytes, or to \
             \"auto\" for the cgroup memory limit of the process minus 'memory.headroom', for \
             example g.with(\"memory.limit\", 4294967296).v().out().fold() or \
             g.with(\"memory.limit\", \"auto\").v().out().fold(). Without the option there is \
             no memory budget (the default)."
            .to_string(),
        "memory.headroom" => "Set 'memory.headroom' to a non-negative integer number of bytes: \
             what memory.limit \"auto\" keeps free below the cgroup limit, for example \
             g.with(\"memory.limit\", \"auto\").with(\"memory.headroom\", 268435456).v().out().fold(). \
             The default is a tenth of the cgroup limit, at least 67108864 (64 MiB) and at most \
             half of the limit."
            .to_string(),
        "traversal.max_string_bytes" => "Set 'traversal.max_string_bytes' to a positive integer \
             number of bytes when the traversal source is created, for example \
             g.with(\"traversal.max_string_bytes\", 67108864).v().values(\"name\").replace(\"a\", \"aa\"); \
             the default is 16777216 (16 MiB)."
            .to_string(),
        "traversal.max_value_depth" => "Set 'traversal.max_value_depth' to a positive integer \
             number of nesting levels when the traversal source is created, for example \
             g.with(\"traversal.max_value_depth\", 256).inject(1).repeat(__.fold()).times(200).count(); \
             the default is 128."
            .to_string(),
        "bulk.max_expansion" => "Set 'bulk.max_expansion' to a positive integer number of values \
             when the traversal source is created, for example \
             g.with(\"bulk.max_expansion\", 1000000).v().out().fold(); the default is 10000000."
            .to_string(),
        "bulk.one" => "Set 'bulk.one' to true or false when the traversal source is created, \
             for example g.with(\"bulk.one\", true).v().out().barrier().count(), or use \
             with_bulk(false), which sets it. With bulk.one a merge keeps bulk 1, so duplicates collapse at barrier()."
            .to_string(),
        GLOB_PATH_MAX_DEPTH => glob_path_max_depth_help(None),
        "random.seed" => "Set 'random.seed' to an integer when the traversal source is created, \
             for example g.with(\"random.seed\", 42).v().sample(2); the same seed makes coin(), \
             sample() and order().by(Order.shuffle) draw the same values on every run. Without \
             the option the draws are seeded from the operating system's entropy."
            .to_string(),
        "choose().option" => "choose() picks at most one option, so it rejects a Pick.any option \
             and a traversal as an option key. Test the selector's value with a predicate or a \
             literal instead, for example g.v().choose(__.values(\"age\")).option(P.gt(30), \
             __.constant(\"old\")).option(Pick.none, __.constant(\"young\")), or use branch(), \
             which accepts a traversal key and Pick.any, for example \
             g.v().branch(__.identity()).option(__.has_label(\"software\"), __.values(\"name\")). \
             With no matching option choose() passes the traverser through unchanged."
            .to_string(),
        "render.spelling" => "Set 'render.spelling' to \"snake\" (the default: has_label(), \
             group_count()) or \"camel\" (Gremlin camelCase: hasLabel(), groupCount()), for \
             example g.with(\"render.spelling\", \"camel\").v().has_label(\"person\").profile(). \
             It changes only how .profile() and error messages spell the steps, never a result."
            .to_string(),
        "repeat.order" => "Set 'repeat.order' to \"bfs\" (breadth first, the default) or \"dfs\" \
             (depth first) when the traversal source is created, for example \
             g.with(\"repeat.order\", \"dfs\").v(\"1\").repeat(__.out()).times(2)."
            .to_string(),
        "repeat.max_loops" => "Set 'repeat.max_loops' to a positive integer number of iterations \
             when the traversal source is created, for example \
             g.with(\"repeat.max_loops\", 100).v(\"1\").repeat(__.out()).until(__.has(\"name\", \"<value>\")); \
             the default is 10000 and times(n) is never limited."
            .to_string(),
        "optimizer.disabled" | "optimizer.enabled" => format!(
            "Set 'optimizer.disabled'/'optimizer.enabled' to an array of optimizer rule names \
             when the traversal source is created, for example \
             g.with(\"optimizer.disabled\", [\"filter_reorder\"]).v().has_label(\"person\").count(); \
             every name must be one of the rules a profile lists as \"Optimizer rules applied\" \
             (optimizer_rules_applied of g.v().has_label(\"person\").count().profile()) \
             ({rules} — see the book's Query Optimizer chapter for each rule's default on/off \
             state) or the reserved name \"all\", which expands to every one of them — e.g. \
             g.with(\"optimizer.disabled\", [\"all\"]).v().has_label(\"person\").count() \
             to measure a query's genuinely unoptimized plan; and the same name cannot appear in \
             both lists at once.",
            rules = crate::traversal::optimizer::KNOWN_RULES
                .iter()
                .map(|(name, _)| *name)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        "max_rows" | "max_items" => format!(
            "Set '{key}' to a positive integer for this call, for example \
             g.v().to_table(#{{{key}: 20}}). To show everything, remove both display limits \
             with no_limit: true instead, for example g.v().to_table(#{{no_limit: true}}); \
             0 is not a way to say \"unlimited\"."
        ),
        "no_limit" => "no_limit: true removes both display limits, so it cannot be combined \
             with max_rows or max_items in the same options map. Use either \
             g.v().to_table(#{no_limit: true}) to show everything, or \
             g.v().to_table(#{max_rows: 20, max_items: 10}) to set other limits; \
             no_limit: false keeps the session default."
            .to_string(),
        "options" => "Pass the display options as a map, for example \
             g.v().to_table(#{max_rows: 20, columns: [\"name\", \"age\"]}), or a list of columns, \
             for example g.v().to_table([\"name\", \"age\"])."
            .to_string(),
        "render.max_rows" | "render.max_items" => format!(
            "Set '{key}' to a positive integer, for example g.with(\"{key}\", 20).v(). It is the \
             session default of a display limit; to show everything for one call, use \
             g.v().to_table(#{{no_limit: true}})."
        ),
        _ => format!(
            "Set '{key}' to a non-negative integer number of milliseconds when the traversal \
             source is created, for example g.with(\"evaluationTimeout\", 5000).v().count(); \
             0 disables the timeout."
        ),
    }
}

/// The `InvalidOption` key of an out-of-range `glob_path()` `max_depth`.
const GLOB_PATH_MAX_DEPTH: &str =
    crate::traversal::step::flatmap_steps::step_glob_path::MAX_DEPTH_OPTION;

/// Splits a rendered `glob_path("..")` step right after the closing quote of its pattern
/// literal, so its options can be edited; `None` for any other step.
fn split_glob_path_step(step: &str) -> Option<(&str, &str)> {
    const PREFIX: &str = "glob_path(\"";
    let literal = step.strip_prefix(PREFIX)?;
    let mut escaped = false;
    for (i, c) in literal.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '"' => return Some(step.split_at(PREFIX.len() + i + 1)),
            _ => {}
        }
    }
    None
}

/// A rendered query with its `glob_path()` options maps written as Rhai map literals
/// (`#{max_depth: 3}`), so it can be run as it is.
fn runnable_glob_path_options(query: &str) -> String {
    query.replace("\", {", "\", #{")
}

/// The step with its out-of-range `max_depth` replaced by the nearest valid value; `None` when
/// the step is not a `glob_path()` with an out-of-range `max_depth`.
fn glob_path_step_with_valid_max_depth(step: &str) -> Option<String> {
    const OPTION: &str = ", {max_depth: ";
    let (head, rest) = split_glob_path_step(step)?;
    let value = rest.strip_prefix(OPTION)?;
    let end = value
        .char_indices()
        .find(|&(i, c)| !(c.is_ascii_digit() || (i == 0 && c == '-')))
        .map_or(value.len(), |(i, _)| i);
    let depth: i128 = value[..end].parse().ok()?;
    let valid = if depth < 1 {
        1
    } else if depth > i128::from(u32::MAX) {
        i128::from(u32::MAX)
    } else {
        return None;
    };
    Some(format!("{head}{OPTION}{valid}{}", &value[end..]))
}

/// Advice for an out-of-range `glob_path()` `max_depth`, with the failing query corrected when
/// it is known.
fn glob_path_max_depth_help(steps: Option<Vec<String>>) -> String {
    let corrected = steps.and_then(|steps| {
        let mut fixed = false;
        let rewritten: Vec<String> = steps
            .iter()
            .map(
                |step| match (fixed, glob_path_step_with_valid_max_depth(step)) {
                    (false, Some(valid)) => {
                        fixed = true;
                        valid
                    }
                    _ => step.clone(),
                },
            )
            .collect();
        fixed.then(|| runnable_glob_path_options(&rewritten.join(".")))
    });
    let example = corrected.unwrap_or_else(|| {
        "v(\"root\").glob_path(\"**/*.rs\", #{max_depth: 3}).by(\"name\")".to_string()
    });
    format!(
        "max_depth is the deepest level below the start vertex that glob_path() visits: the start \
         vertex is level 0 and its children are level 1, so max_depth is an integer from 1 to \
         4294967295. Leave it out to walk without a bound. For example g.{example}."
    )
}

/// The failing query with `max_depth: 3` added to its first `glob_path()` over `**` that has no
/// `max_depth` yet; `None` when there is no such step on the outermost level.
fn glob_path_query_with_max_depth(steps: &[String]) -> Option<String> {
    let position = steps.iter().position(|step| {
        split_glob_path_step(step)
            .is_some_and(|(head, rest)| head.contains("**") && !rest.starts_with(", {max_depth: "))
    })?;
    let mut rewritten = steps.to_vec();
    let (head, rest) = split_glob_path_step(&steps[position])?;
    rewritten[position] = match rest.strip_prefix(", {") {
        Some(options) => format!("{head}, {{max_depth: 3, {options}"),
        None => format!("{head}, {{max_depth: 3}}{rest}"),
    };
    Some(runnable_glob_path_options(&rewritten.join(".")))
}

/// Advice for a `g.with(key, ..)` a query attempted on a key the host has locked via
/// [`crate::graph::ExecutionPolicy::with_locked`]. Unlike `invalid_option_help`, this never
/// suggests "try a different value" — no value the query supplies is accepted for a locked key,
/// so the advice explains why (a host-level policy fixed at construction, not something the
/// query can influence) rather than how to satisfy it.
fn locked_option_help(key: &str) -> String {
    format!(
        "'{key}' is fixed by the execution policy of the environment running this query and \
         cannot be set or overridden with g.with(\"{key}\", ..), regardless of the value \
         supplied. This is a host-level safety ceiling (see `ExecutionPolicy::with_locked` in \
         the embedding application, and the \"Execution Options Reference\" book chapter for \
         which keys this instance may lock), not a malformed value — removing or changing the \
         g.with(\"{key}\", ..) call in the query has no effect either way, since the policy's own \
         default (or, absent one, the engine's default) applies regardless of what the query \
         asks for."
    )
}

/// Advice for a [`TraverserError::PermissionDenied`]: who decides, how a host allows the
/// request, and what the query can do instead.
fn permission_denied_help(step: &str, request: &crate::graph::AccessRequest<'_>) -> String {
    use crate::graph::{Action, Resource};
    let instead = match (request.action, request.resource) {
        (Action::Create | Action::Update | Action::Delete, Resource::Data { .. }) => format!(
            " A query that may not change the host graph can still build its own: a graph the \
             script builds itself is private and always writable, for example \
             GraphSource::empty().add_v(\"x\").next() or a subgraph() result; remove \
             {step}() from the query to only read."
        ),
        (Action::Read, Resource::Data { .. }) => {
            " Without reading data a script can only compute with its own values; it cannot \
             traverse the host graph."
                .to_string()
        }
        (_, Resource::Schema) => " The schema is protected separately from the data: \
             get_schema(), infer_schema(), validate_schema(..) and diff_schema(..) read it (Read), set_schema(..) and patch_schema(..) change it (Update)."
            .to_string(),
        (_, Resource::Definition { .. }) => {
            " The catalog of definitions (saved queries) is protected separately from the \
             data: listing and reading a definition asks Read, defining one Create (Update \
             to replace, move or describe it), removing one Delete, calling a saved query \
             Execute."
                .to_string()
        }
        (_, Resource::File { .. }) => {
            " File access (export_graphml, import_graphml, GraphSource::file, \
             Rhai's import) is decided per file. When the host set a file root \
             (AccessPolicy::with_file_root), use a relative path inside it, without '..'."
                .to_string()
        }
        (_, Resource::Option { .. }) => {
            " The administrative g.with() options (the resource ceilings evaluationTimeout, \
             repeat.max_loops, bulk.max_expansion, traversal.max_traversers and the engine \
             switches optimizer.disabled, optimizer.enabled, path.analysis, bulk.merge) are \
             the host's to set: remove the g.with(..) call; the host's settings apply."
                .to_string()
        }
        _ => String::new(),
    };
    format!(
        "'{step}' needs {request}, which the host running this script does not allow. \
         Everything is allowed by default; the host decides with an Authorizer, for example \
         the ready-made AccessPolicy::read_only().allow(ActionKind::{action}, \
         ResourceKind::{resource}) passed to script::engine_with_limits(..) and \
         script::graph_scope_with_options(..) (or eval_value_with_limits), or \
         GraphTraversalSource::with_authorizer(..) in Rust.{instead}",
        action = request.action.kind(),
        resource = request.resource.kind(),
    )
}

/// Advice for a `repeat()` that hit `repeat.max_loops`, with every suggestion written on the
/// failing loop body and, when known, the failing query.
fn repeat_limit_help(
    max_loops: u32,
    loop_name: Option<&str>,
    body: &str,
    steps: Option<Vec<String>>,
) -> String {
    let repeat = match loop_name {
        Some(name) => format!("repeat(\"{name}\", {body})"),
        None => format!("repeat({body})"),
    };
    let simple = match loop_name {
        Some(name) => format!("repeat(\"{name}\", {body}.simple_path())"),
        None => format!("repeat({body}.simple_path())"),
    };
    let steps = steps.filter(|steps| !steps.is_empty());
    let start = steps
        .as_deref()
        .and_then(glob_path_start)
        .unwrap_or_else(|| "v()".to_string());
    let query = steps
        .map(|steps| steps.join("."))
        .unwrap_or_else(|| format!("v().{repeat}"));
    let raised = u64::from(max_loops)
        .saturating_mul(2)
        .min(u64::from(u32::MAX));
    let repeat_barrier = match loop_name {
        Some(name) => format!("repeat(\"{name}\", {body}.barrier())"),
        None => format!("repeat({body}.barrier())"),
    };
    format!(
        "The loop ran {max_loops} iterations without stopping: it walks a cycle, or nothing \
         ends it.\n\
         Bound the number of iterations with times(), for example {repeat}.times(3).\n\
         Stop at a target with until(), for example {repeat}.until(__.has(\"name\", \"<value>\")).\n\
         On a cyclic graph, drop the paths that revisit an element with simple_path(), for \
         example {simple}.\n\
         Equal traversers are merged automatically inside repeat(), but path() and simple_path() \
         record full paths and prevent that merge, so exponential path counts stay expensive \
         with them. A sack merge operator or with_bulk(false) turns automatic merging off: add \
         barrier() inside the loop body, for example {repeat_barrier}.\n\
         Or raise the limit: g.with(\"repeat.max_loops\", {raised}).{query}. It is an administrative option: a script whose host restricts it needs Update on that Option to set it.\n\
         On a tree-shaped graph (directories, org charts, categories), walk the levels below a \
         vertex with glob_path() instead: it follows outgoing edges along a pattern, prunes \
         branches that cannot match and stops on cycles without a loop limit, for example \
         g.{start}.glob_path(\"**/*.rs\").by(\"name\"), or g.{start}.glob_path(\"**\").by(\"name\") \
         for every level below; bound how deep it walks with max_depth, for example \
         g.{start}.glob_path(\"**\", #{{max_depth: 3}}).by(\"name\")."
    )
}

/// The steps of a failing query before its loop (`repeat()` or a prefix `emit()`/`until()`/
/// `times()`), joined, as the start of a `glob_path()` rewrite; `None` when the loop is not on
/// that level or nothing precedes it.
fn glob_path_start(steps: &[String]) -> Option<String> {
    let loop_start = steps.iter().position(|step| {
        ["repeat(", "emit(", "until(", "times("]
            .iter()
            .any(|prefix| step.starts_with(prefix))
    })?;
    (loop_start > 0).then(|| steps[..loop_start].join("."))
}

/// Advice for `loops(name)` that names no enclosing loop.
fn unknown_loop_name_help(name: &str, active: &[String]) -> String {
    match active.first() {
        Some(innermost) => format!(
            "The enclosing named loops are: {}. Read one of them, for example \
             loops(\"{innermost}\"), or name the loop that loops(\"{name}\") should read: \
             repeat(\"{name}\", __.out()).",
            active.join(", ")
        ),
        None => format!(
            "No enclosing repeat() is named '{name}'. Name the loop, for example \
             repeat(\"{name}\", __.out()).until(__.loops(\"{name}\").is(2)), or read the \
             innermost loop with loops()."
        ),
    }
}

/// Advice for `times()`/`until()`/`emit()` without a `repeat()`, written on the steps of the
/// failing level before the modulator (`g.` or `__.` prefixed).
fn repeat_without_body_help(modulator: &str, prefix: Option<String>) -> String {
    let prefix = prefix.unwrap_or_else(|| "g.v()".to_string());
    let join = |rest: &str| {
        if prefix.ends_with('.') {
            format!("{prefix}{rest}")
        } else {
            format!("{prefix}.{rest}")
        }
    };
    format!(
        "Write the loop body with repeat(). Before repeat(), {modulator} is checked before each \
         iteration, for example {before}; after it, {modulator} is checked after each \
         iteration, for example {after}.",
        before = join(&format!("{modulator}.repeat(__.out())")),
        after = join(&format!("repeat(__.out()).{modulator}")),
    )
}

/// Advice for [`TraverserError::OptionWithoutBranch`]: `.option(pick, traversal)` written with
/// no `choose(traversal)`/`branch(traversal)` to attach to (or attached to the locked 3-argument
/// `choose(predicate, onTrue, onFalse)` form).
fn option_without_branch_help(modulator: &str) -> String {
    format!(
        "{modulator} must follow choose(traversal) or branch(traversal), for example \
         g.v().choose(__.label()).option(\"person\", __.out(\"knows\")).option(\"software\", \
         __.out(\"created\")), or g.v().branch(__.label()).option(\"person\", __.out(\"knows\")); \
         choose(predicate, onTrue, onFalse) (the 3-argument if/then/else form) already has its \
         two branches fixed and does not accept further .option() calls. \
         option(Merge.onCreate|onMatch, ..) must follow merge_v()/merge_e(), for example \
         g.merge_v(#{{\"T.label\": \"person\", \"name\": \"marko\"}}).option(Merge.onCreate, \
         #{{\"age\": 29}}); option(Merge.outV|inV, ..) must follow merge_e(), for example \
         g.merge_e(#{{\"Direction.OUT\": Merge.outV, \"Direction.IN\": \"2\"}}).option(Merge.outV, \
         #{{\"T.id\": \"1\"}})."
    )
}

/// Advice for [`TraverserError::InvalidMergeMap`], keyed on what is wrong with the map.
fn invalid_merge_map_help(step: &str, reason: &str) -> String {
    let specific = if reason.contains("hidden key: ~id") {
        format!(
            "Write the id with the reserved key \"T.id\" instead of \"~id\", for example \
             g.{step}(#{{\"T.id\": \"1\"}}). "
        )
    } else if reason.contains("hidden key: ~label")
        || reason.contains("Label can not be a hidden key")
    {
        format!(
            "Write the label with the reserved key \"T.label\" and a plain label value, for \
             example g.{step}(#{{\"T.label\": \"person\"}}). "
        )
    } else if reason.contains("option(onMatch)") {
        format!(
            "option(Merge.onMatch, ..) only updates properties of the matched element, for \
             example g.{step}(#{{\"name\": \"marko\"}}).option(Merge.onMatch, #{{\"age\": 30}}); \
             put T.id/T.label into the merge() map or option(Merge.onCreate, ..) instead. "
        )
    } else if reason.contains("vertex lookup map") {
        "The option(Merge.outV|inV, map) map is a vertex search (\"T.id\", \"T.label\" and \
         properties), for example option(Merge.outV, #{\"T.id\": \"1\"}); the edge's \
         Direction keys belong to the merge_e() map. "
            .to_string()
    } else if reason.contains("Direction.BOTH") {
        "An edge has two named ends: write \"Direction.OUT\" and \"Direction.IN\" instead of \
         \"Direction.BOTH\", for example g.merge_e(#{\"T.label\": \"knows\", \"Direction.OUT\": \
         \"1\", \"Direction.IN\": \"2\"}). "
            .to_string()
    } else if reason.contains("cannot take the placeholder") {
        "Each end takes its own placeholder: \"Direction.OUT\": Merge.outV and \"Direction.IN\": \
         Merge.inV, for example g.v(\"1\").as(\"a\").v(\"2\").as(\"b\").merge_e(#{\"Direction.OUT\": \
         Merge.outV, \"Direction.IN\": Merge.inV}).option(Merge.outV, \
         __.select(\"a\")).option(Merge.inV, __.select(\"b\")). "
            .to_string()
    } else if reason.contains("resolves an edge vertex of merge_e()") {
        "option(Merge.outV|inV, ..) belongs to merge_e(), for example \
         g.merge_e(#{\"T.label\": \"knows\", \"Direction.OUT\": Merge.outV, \"Direction.IN\": \
         \"2\"}).option(Merge.outV, #{\"T.id\": \"1\"}); merge_v() takes only \
         option(Merge.onCreate, ..) and option(Merge.onMatch, ..). "
            .to_string()
    } else if reason.contains("Direction keys") {
        "Vertices have no endpoints: Direction.OUT/Direction.IN keys belong to merge_e(), for \
         example g.merge_e(#{\"T.label\": \"knows\", \"Direction.OUT\": \"1\", \"Direction.IN\": \
         \"2\"}). "
            .to_string()
    } else if reason.contains("produced no map") || reason.contains("expects a map") {
        format!(
            "Pass a map, null or a traversal that yields a map, for example \
             g.{step}(#{{\"T.label\": \"person\", \"name\": \"marko\"}}) or \
             g.inject(#{{\"T.label\": \"person\", \"name\": \"marko\"}}).{step}(). "
        )
    } else {
        String::new()
    };
    let general = if step == "merge_e" {
        "A merge_e() map holds property keys plus the reserved keys \"T.id\", \"T.label\", \
         \"Direction.OUT\" and \"Direction.IN\", for example g.merge_e(#{\"T.label\": \"knows\", \
         \"Direction.OUT\": \"1\", \"Direction.IN\": \"2\"}).option(Merge.onCreate, \
         #{\"weight\": 0.5})"
    } else {
        "A merge_v() map holds property keys plus the reserved keys \"T.id\" and \"T.label\", for \
         example g.merge_v(#{\"T.label\": \"person\", \"name\": \"marko\"}).option(Merge.onCreate, \
         #{\"age\": 29})"
    };
    format!(
        "{specific}{general}; null or #{{}} matches every element. Keys starting with \"~\" and \
         other \"T.\"/\"Direction.\" keys are not allowed."
    )
}

/// Advice for [`TraverserError::MergeOverride`].
fn merge_override_help(step: &str, key: &str) -> String {
    let example = if step == "merge_e" {
        "g.merge_e(#{\"T.label\": \"knows\", \"Direction.OUT\": \"1\", \"Direction.IN\": \
         \"2\"}).option(Merge.onCreate, #{\"weight\": 0.5})"
    } else {
        "g.merge_v(#{\"T.label\": \"person\", \"name\": \"marko\"}).option(Merge.onCreate, \
         #{\"age\": 29})"
    };
    format!(
        "option(Merge.onCreate, ..) can only add keys to the merge() map: a key present in both \
         must have the same value there. Remove \"{key}\" from the onCreate map (the created \
         element inherits it from merge()), for example {example}, or change it in the merge() \
         map itself."
    )
}

/// `math("<expr>")` with a caret line under the offending token (1-based `column`, `width`
/// characters), for the `equation` detail of [`TraverserError::MathExpression`].
fn math_underline(expr: &str, column: usize, width: usize) -> String {
    const OPEN: usize = 6; // `math("`
    let (shown, column) = math_window(expr, column);
    format!(
        "math(\"{shown}\")\n{}{}",
        " ".repeat(OPEN + column.saturating_sub(1)),
        "^".repeat(width.clamp(1, crate::value::literal::MAX_LITERAL_CHARS))
    )
}

/// The part of a long equation shown above the underline: `MAX_LITERAL_CHARS` characters
/// around `column` (1-based), with `…` where it is cut, and the column inside that part.
fn math_window(expr: &str, column: usize) -> (String, usize) {
    let max = crate::value::literal::MAX_LITERAL_CHARS;
    let chars: Vec<char> = expr.chars().collect();
    if chars.len() <= max {
        return (expr.to_string(), column);
    }
    let start = column
        .saturating_sub(1)
        .saturating_sub(max / 2)
        .min(chars.len());
    let end = (start + max).min(chars.len());
    let mut shown = String::new();
    let mut shift = start;
    if start > 0 {
        shown.push('…');
        shift -= 1;
    }
    shown.extend(&chars[start..end]);
    if end < chars.len() {
        shown.push('…');
    }
    (shown, column.saturating_sub(shift))
}

/// `expr` with every identifier `name` (a whole token, not part of a longer name, a nested key
/// or a quoted string) replaced by `with`.
fn math_replace_identifier(expr: &str, name: &str, with: &str) -> String {
    let mut out = String::with_capacity(expr.len() + with.len());
    let mut rest = expr;
    // The previous character, to skip a key after `.` (`_.value`) and identifier tails.
    let mut prev: Option<char> = None;
    let mut quote: Option<char> = None;
    while let Some(c) = rest.chars().next() {
        if let Some(q) = quote {
            out.push(c);
            rest = &rest[c.len_utf8()..];
            if c == q {
                quote = None;
            } else if c == '\\'
                && let Some(escaped) = rest.chars().next()
            {
                out.push(escaped);
                rest = &rest[escaped.len_utf8()..];
            }
            prev = Some(c);
            continue;
        }
        if c == '"' || c == '\'' {
            quote = Some(c);
            out.push(c);
            rest = &rest[1..];
            prev = Some(c);
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let end = rest
                .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
                .unwrap_or(rest.len());
            let ident = &rest[..end];
            if ident == name && prev != Some('.') {
                out.push_str(with);
            } else {
                out.push_str(ident);
            }
            rest = &rest[end..];
            prev = ident.chars().last();
            continue;
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
        prev = Some(c);
    }
    out
}

/// The failing query with its top-level `math("<expr>")` step rewritten: `before` is inserted
/// right before the step, and the equation is replaced by `new_expr`. `None` when the query is
/// unknown, the `math()` step is not at its outermost level, or the step carries `by()`s.
fn math_rewrite_query(
    steps: Option<&[String]>,
    expr: &str,
    before: Option<&str>,
    new_expr: &str,
) -> Option<String> {
    let step = format!("math(\"{expr}\")");
    let mut rewritten: Vec<String> = Vec::new();
    let mut found = false;
    for s in steps? {
        if crate::traversal::source::is_display_only(s) {
            continue;
        }
        if !found && *s == step {
            found = true;
            if let Some(before) = before {
                rewritten.push(before.to_string());
            }
            rewritten.push(format!("math(\"{new_expr}\")"));
        } else {
            rewritten.push(s.clone());
        }
    }
    found.then(|| format!("g.{}", rewritten.join(".")))
}

/// Names of the built-in `math()` functions close to `name` (edit distance at most 2, or one a
/// prefix of the other), closest first.
fn math_close_functions(
    name: &str,
) -> Vec<&'static crate::value::math_expr::registry::FunctionDef> {
    fn distance(a: &str, b: &str) -> usize {
        let b: Vec<char> = b.chars().collect();
        let mut row: Vec<usize> = (0..=b.len()).collect();
        for (i, ca) in a.chars().enumerate() {
            let mut diagonal = row[0];
            row[0] = i + 1;
            for (j, cb) in b.iter().enumerate() {
                let above = row[j + 1];
                row[j + 1] = (diagonal + usize::from(ca != *cb))
                    .min(row[j] + 1)
                    .min(above + 1);
                diagonal = above;
            }
        }
        row[b.len()]
    }
    let lower = name.to_ascii_lowercase();
    let mut close: Vec<(
        usize,
        &'static crate::value::math_expr::registry::FunctionDef,
    )> = crate::value::math_expr::registry::FUNCTIONS
        .iter()
        .map(|f| (distance(&lower, f.name), f))
        .filter(|(d, f)| {
            *d <= 2
                || (lower.len() >= 2 && (f.name.starts_with(&lower) || lower.starts_with(f.name)))
        })
        .collect();
    close.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.name.cmp(b.1.name)));
    close.into_iter().map(|(_, f)| f).collect()
}

/// What the `math()` equation language is, shared by every `math()` advice.
fn math_language() -> String {
    let functions: Vec<&str> = crate::value::math_expr::registry::FUNCTIONS
        .iter()
        .map(|f| f.name)
        .collect();
    format!(
        "math() evaluates an equation like TinkerPop does: numbers, the operators + - * / % and \
         ^ (power), parentheses, the functions {} (log is the natural logarithm), the constants \
         pi, e, π and φ, the current value _ and variables that name a key of the current map, a \
         side effect or a step label. The result is always a double.",
        functions.join(", ")
    )
}

/// Advice for [`TraverserError::MathExpression`], with examples rewritten from the failing query
/// (`steps`, outermost level) when its `math()` step is at that level.
fn math_expression_help(expr: &str, kind: &MathErrorKind, steps: Option<Vec<String>>) -> String {
    let steps = steps.as_deref();
    let language = math_language();
    match kind {
        MathErrorKind::Syntax => format!(
            "{language} Fix the equation at the marked position, for example \
             g.v().values(\"age\").math(\"(_ + 1) * 2\")."
        ),
        MathErrorKind::UnknownFunction { name } => {
            let close = math_close_functions(name);
            match close.first() {
                Some(best) => {
                    let fixed = math_replace_identifier(expr, name, best.name);
                    let example = math_rewrite_query(steps, expr, None, &fixed)
                        .unwrap_or_else(|| format!("g.v().values(\"age\").math(\"{fixed}\")"));
                    format!(
                        "'{name}' is not a math() function; did you mean {}? For example \
                         {example}. {language}",
                        close
                            .iter()
                            .map(|f| format!("{} ({})", f.name, f.doc))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
                None => format!("'{name}' is not a math() function. {language}"),
            }
        }
        MathErrorKind::UnknownVariable { name } if name == "value" => {
            let fixed = math_replace_identifier(expr, "value", "_");
            let example = math_rewrite_query(steps, expr, None, &fixed)
                .unwrap_or_else(|| format!("g.v().values(\"age\").math(\"{fixed}\")"));
            format!(
                "The current value is named _ in math(), as in TinkerPop; value is not a name for \
                 it. For example {example}. {language}"
            )
        }
        MathErrorKind::UnknownVariable { name } => {
            let label = format!("as(\"{name}\")");
            let example =
                math_rewrite_query(steps, expr, Some(&label), expr).unwrap_or_else(|| {
                    format!("g.v().values(\"age\").as(\"{name}\").math(\"{expr}\")")
                });
            let nested = math_replace_identifier(expr, name, &format!("_.{name}"));
            format!(
                "math() reads a variable other than _ from, in this order, a key of the current \
                 map (project(\"{name}\", ...), value_map(), element_map()), a side effect \
                 (g.with_side_effect(\"{name}\", 10), aggregate(\"{name}\")) or a step label \
                 (as(\"{name}\")), and '{name}' is none of these for this traverser. Label the \
                 value it should read, for example {example}, or declare it as a side effect, \
                 for example g.with_side_effect(\"{name}\", 10).v().values(\"age\").math(\"{expr}\"). \
                 To read a key of the current value itself, use the nested form _.{name} (a \
                 Graphersal extension), for example g.v().element_map().math(\"{nested}\")."
            )
        }
        MathErrorKind::BadPath { path } => format!(
            "Nested access such as {path} is a Graphersal extension of math(): after a variable, \
             .key or [\"key\"] reads a key of a map (value_map(), element_map(), project(), a \
             map-valued property) or a property of a vertex or edge, and [n] an element of a \
             list (a negative n counts from the end). The path leads to no value for this \
             traverser: check the key or index against the value, for example \
             g.v().element_map().limit(1), and keep only traversers that have it before math(), \
             for example g.v().has(\"age\").element_map().math(\"_.age * 2\")."
        ),
        MathErrorKind::NotANumber { path, actual } => {
            let example = if path == "_" {
                math_rewrite_query(steps, expr, Some("as_number(GType.DOUBLE)"), expr)
            } else {
                None
            }
            .unwrap_or_else(|| {
                format!(
                    "g.v().values(\"age\").as_string().as_number(GType.DOUBLE).math(\"{expr}\")"
                )
            });
            format!(
                "math() computes with numbers only, and {path} is a {actual}. Convert the value \
                 with as_number(GType.DOUBLE) first, for example {example}, or inside the by() \
                 that reads it, for example by(__.values(\"age\").as_number(GType.DOUBLE))."
            )
        }
        MathErrorKind::Null { path } => {
            let example = if path == "_" { expr } else { "_ + 1" };
            format!(
                "{path} is null, and math() computes with numbers only. Keep only traversers that \
                 have the value before math(), for example \
                 g.v().has(\"age\").math(\"{example}\").by(\"age\"), or replace a missing value \
                 with a default inside the by(), for example \
                 by(__.coalesce(__.values(\"age\"), __.constant(0)))."
            )
        }
        MathErrorKind::DivisionByZero => format!(
            "math() follows TinkerPop: a zero divisor of / or % (or cot(x) where \
             tan(x) is 0) is an error, not infinity. Keep only traversers with a non-zero divisor \
             before math(), for example g.v().project(\"a\", \"b\").by(\"age\").by(__.out().count())\
             .where(__.select(\"b\").is(P.neq(0))).math(\"a / b\"). The failing equation was \
             math(\"{expr}\")."
        ),
        MathErrorKind::Internal => format!(
            "This is an internal error of the math() engine, not of the equation math(\"{expr}\"); \
             please report it together with the equation."
        ),
    }
}

impl TraverserError {
    /// Builds a [`TraverserError::MathExpression`] for the token at byte `offset` of `expr`
    /// (`width` characters long).
    pub(crate) fn math_expression(
        expr: &str,
        offset: usize,
        width: usize,
        kind: MathErrorKind,
        message: String,
    ) -> TraverserError {
        let column = expr.get(..offset).map_or(offset, |s| s.chars().count()) + 1;
        TraverserError::MathExpression {
            expr: expr.to_string(),
            offset,
            column,
            width: width.max(1),
            kind,
            message,
        }
    }
}

/// The most expected tokens `help()` lists; the `expected` detail lists them all.
#[cfg(feature = "gql")]
const GQL_HELP_EXPECTED: usize = 10;

/// The `details()` of a [`TraverserError::Gql`]: the offending source line (labelled `query`
/// for a one-line query, `line N` otherwise) with a caret line under the span, then the
/// expected tokens, the suggestion and the GQLSTATUS code.
#[cfg(feature = "gql")]
fn gql_details(
    query: &str,
    line: usize,
    column: usize,
    width: usize,
    expected: &[String],
    suggestion: Option<&str>,
    gqlstatus: &str,
) -> Vec<(String, String)> {
    let mut details = Vec::new();
    let mut lines = query.split('\n');
    let multi_line = query.trim_end_matches(['\n', '\r']).contains('\n');
    if let Some(text) = lines.nth(line.saturating_sub(1)) {
        // Tabs become spaces (one character each), so the caret stays under the span.
        let text: String = text
            .trim_end_matches('\r')
            .chars()
            .map(|c| if c == '\t' { ' ' } else { c })
            .collect();
        let line_chars = text.chars().count();
        let (shown, shown_column) = math_window(&text, column);
        // A span that runs past the end of its line is underlined up to the line's end.
        let rest = (line_chars + 1).saturating_sub(column).max(1);
        let width = width
            .min(rest)
            .clamp(1, crate::value::literal::MAX_LITERAL_CHARS);
        let label = if multi_line {
            format!("line {line}")
        } else {
            "query".to_string()
        };
        details.push((
            label,
            format!(
                "{shown}\n{}{}",
                " ".repeat(shown_column.saturating_sub(1)),
                "^".repeat(width)
            ),
        ));
    }
    if !expected.is_empty() {
        details.push(("expected".to_string(), expected.join(", ")));
    }
    if let Some(suggestion) = suggestion {
        details.push(("did you mean".to_string(), suggestion.to_string()));
    }
    details.push(("GQLSTATUS".to_string(), gqlstatus.to_string()));
    details
}

/// Advice for [`TraverserError::Gql`], per kind. The parser supplies the specific text (the
/// suggestion, the GQL form or Gremlin alternative); this adds the generic fix around it.
#[cfg(feature = "gql")]
fn gql_help(
    kind: GqlErrorKind,
    gqlstatus: &str,
    expected: &[String],
    suggestion: Option<&str>,
    alternative: Option<&str>,
    topic: GqlHelpTopic,
) -> String {
    let mut help = String::new();
    if let Some(alternative) = alternative {
        // One closing full stop; an ellipsis (`= ...`) stays as written.
        let alternative = alternative
            .strip_suffix('.')
            .filter(|rest| !rest.ends_with('.'))
            .unwrap_or(alternative);
        help.push_str(alternative);
        help.push_str(if alternative.ends_with('.') {
            " "
        } else {
            ". "
        });
    }
    if let Some(suggestion) = suggestion {
        help.push_str(&format!("Did you mean {suggestion}? "));
    }
    match kind {
        GqlErrorKind::Syntax => {
            if !expected.is_empty() {
                let mut listed = expected
                    .iter()
                    .take(GQL_HELP_EXPECTED)
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(", ");
                if expected.len() > GQL_HELP_EXPECTED {
                    listed.push_str(", ...");
                }
                help.push_str(&format!("Expected {listed} at the marked position. "));
            }
            if topic == GqlHelpTopic::Insert {
                help.push_str(GQL_INSERT_ADVICE);
                return help;
            }
            if topic == GqlHelpTopic::Set {
                help.push_str(GQL_SET_ADVICE);
                return help;
            }
            if topic == GqlHelpTopic::Delete {
                help.push_str(GQL_DELETE_ADVICE);
                return help;
            }
            if topic == GqlHelpTopic::Merge {
                help.push_str(GQL_MERGE_ADVICE);
                return help;
            }
            // A pattern predicate in a value position (stage 3f).
            if topic == GqlHelpTopic::Subquery {
                help.push_str(gql_semantic_advice(topic));
                return help;
            }
            help.push_str(
                "Fix the query at the marked position: a GQL read query is MATCH <pattern> \
                 [WHERE <condition>], then any further MATCH, OPTIONAL MATCH, WITH, LET or \
                 FILTER clauses, then RETURN <items>, for example \
                 g.gql(\"MATCH (p:person) WHERE p.age > 30 RETURN p.name\") or \
                 g.gql(\"MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) WITH p, count(s) \
                 AS n RETURN p.name, n\"). CALL (p) { .. RETURN .. } runs a query for each \
                 row, UNION combines the records of two queries and NEXT continues after a \
                 RETURN, as in g.gql(\"MATCH (p:person) RETURN p NEXT MATCH \
                 (p)-[:created]->(s) RETURN p.name, s.name\"). FOR x IN <list> (Cypher UNWIND <list> AS x) gives \
                 one row per item of a list, and a query may also start with FOR, LET, WITH, \
                 FILTER or RETURN, as in \
                 g.gql(\"FOR n IN ['marko', 'josh'] MATCH (p:person {name: n}) RETURN n, p.age\").",
            );
        }
        // Class 22 before the run: an OFFSET / LIMIT that is no non-negative integer.
        GqlErrorKind::Semantic if gqlstatus.starts_with("22") => help.push_str(
            "OFFSET and LIMIT take a non-negative integer: a literal, or a $parameter given \
             such a value in the parameters of gql(), for example \
             g.gql(\"MATCH (p:person) RETURN p.name ORDER BY p.name LIMIT $n\", #{n: 2}). \
             An expression without variables is evaluated once, as in \
             g.gql(\"MATCH (p:person) RETURN p.name LIMIT toInteger(ceil(1.7))\"); one that \
             reads the row (LIMIT p.age) is refused.",
        ),
        GqlErrorKind::Semantic => help.push_str(gql_semantic_advice(topic)),
        GqlErrorKind::Refused if topic == GqlHelpTopic::Call => {
            help.push_str(gql_semantic_advice(topic))
        }
        GqlErrorKind::Refused if alternative.is_some() => help.push_str(
            "gql() does not support this construct; write this part of the query as Gremlin \
             steps.",
        ),
        GqlErrorKind::Refused => help.push_str(
            "Graphersal does not support this construct in gql(); write this part of the query \
             as Gremlin steps, for example g.v().has_label(\"person\").values(\"name\").",
        ),
        // Stage 3g (U1): values hold microseconds; the fix is fewer digits, not Gremlin.
        GqlErrorKind::NotYet if topic == GqlHelpTopic::TemporalPrecision => {
            help.push_str(GQL_TEMPORAL_PRECISION_ADVICE)
        }
        GqlErrorKind::NotYet => help.push_str(
            "This construct comes in a later version of gql(); until then, write the query \
             without it or as Gremlin steps.",
        ),
        // A size limit (an operator chain, a pattern) brings its own fix as the alternative.
        GqlErrorKind::Limit if topic == GqlHelpTopic::Subquery => help.push_str(&format!(
            "The query is larger than gql() accepts: one query holds at most {} subqueries \
             (EXISTS, pattern predicates, pattern comprehensions, COUNT {{ }} and COLLECT {{ }}, \
             nested ones included) and runs at most {} subquery bodies, where a LET or WITH \
             variable holding a subquery counts once for every place that reads it; a CALL {{ }} \
             body counts toward neither (CALL bodies nest at most {} levels). One EXISTS may \
             hold several MATCH clauses and patterns, as in g.gql(\"MATCH (p:person) WHERE \
             EXISTS {{ MATCH (p)-[:knows]->(f) MATCH (f)-[:created]->(s) }} RETURN p.name\"), \
             and one CALL may compute what several COUNT {{ }} and COLLECT {{ }} over the same \
             pattern do, as in g.gql(\"MATCH (p:person) CALL (p) {{ OPTIONAL MATCH \
             (p)-[:created]->(s) RETURN count(s) AS n, collect_list(s.name) AS names }} RETURN \
             p.name, n, names\").",
            crate::gql::bind::MAX_SUBQUERIES,
            crate::gql::lower::MAX_LOWERED_SUBQUERIES,
            crate::gql::bind::MAX_BODY_DEPTH,
        )),
        GqlErrorKind::Limit if alternative.is_some() => {
            help.push_str("The query is larger than gql() accepts.")
        }
        GqlErrorKind::Limit => help.push_str(
            "The query nests deeper than gql() accepts. Flatten it: remove redundant \
             parentheses, and join conditions with AND / OR instead of nesting them.",
        ),
        GqlErrorKind::Data if topic == GqlHelpTopic::AggregateValue => help.push_str(
            "A row of the data gave the marked aggregate a value it cannot add: sum() and \
             avg() take numbers only (count() and collect_list() take values of any kind). \
             Aggregate a numeric property, as in g.gql(\"MATCH (p:person) RETURN sum(p.age)\"), \
             and keep values of another kind out with a comparison to a number, which is NULL \
             for text and so drops the row, as in \
             g.gql(\"MATCH (p) WHERE p.age >= 0 RETURN avg(p.age)\"), or inside the aggregate, \
             as in g.gql(\"MATCH (p) RETURN sum(CASE WHEN p.age >= 0 THEN p.age END)\") (an \
             aggregate skips NULL).",
        ),
        GqlErrorKind::Data if topic == GqlHelpTopic::Percentile => help.push_str(
            "A row of the data gave the marked percentile aggregate a value it cannot take: \
             percentile_cont(x, p) and percentile_disc(x, p) (Cypher percentileCont / \
             percentileDisc) take numbers x and a percentile p from 0.0 to 1.0, value first, \
             as in g.gql(\"MATCH (p:person) RETURN percentile_disc(p.age, 0.5) AS median\"); \
             give p as a constant or a $parameter, and keep values of another kind out with \
             a comparison to a number, as in \
             g.gql(\"MATCH (p) WHERE p.age >= 0 RETURN percentile_cont(p.age, 0.9) AS p90\").",
        ),
        GqlErrorKind::Data if topic == GqlHelpTopic::Insert => help.push_str(
            "A row of the data gave the marked endpoint of the new edge no node: an edge \
             connects two nodes, so a variable that OPTIONAL MATCH may leave NULL is filtered \
             first, as in g.gql(\"MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) FILTER s \
             IS NOT NULL INSERT (p)-[:made]->(s)\"), and a value that is no node is turned into \
             one by a MATCH, as in g.gql(\"FOR n IN ['lop'] MATCH (s:software {name: n}) \
             INSERT (:review {stars: 5})-[:about]->(s)\").",
        ),
        GqlErrorKind::Data if topic == GqlHelpTopic::Set => {
            help.push_str(
                "A row of the data gave the marked SET or REMOVE item a value it cannot write; \
                 keep such rows away from it with FILTER or WHERE, as in \
                 g.gql(\"MATCH (p:person) FILTER p.age IS NOT NULL SET p.next = p.age + 1\"). ",
            );
            help.push_str(GQL_SET_ADVICE);
        }
        // A node that still has edges (G1001): the step's alternative is the whole fix.
        GqlErrorKind::Data
            if topic == GqlHelpTopic::Delete
                && gqlstatus == crate::gql::eval::status::EDGES_STILL_EXIST =>
        {
            help.truncate(help.trim_end().len())
        }
        GqlErrorKind::Data if topic == GqlHelpTopic::Delete => help.push_str(GQL_DELETE_ADVICE),
        // E1 of stage 3e (a NULL property value) and a NULL bound node of the pattern.
        GqlErrorKind::Data if topic == GqlHelpTopic::Merge => {
            help.push_str(
                "A row of the data gave the marked part of the MERGE pattern a value it can \
                 neither match nor create: a property value that is NULL or a graph element, \
                 or a node bound before that is NULL or no node. A variable that OPTIONAL \
                 MATCH may leave NULL is filtered first, and a value that is no node is turned \
                 into one by a MATCH, as in g.gql(\"FOR n IN ['lop'] MATCH (s:software {name: \
                 n}) MERGE (:review {stars: 5})-[:about]->(s)\"). ",
            );
            help.push_str(GQL_MERGE_ADVICE);
        }
        GqlErrorKind::Data if topic == GqlHelpTopic::StaticKind => {
            help.push_str(GQL_STATIC_KIND_ADVICE)
        }
        GqlErrorKind::Data => {
            if topic == GqlHelpTopic::TextAndNumber {
                help.push_str(
                    "Convert the number or boolean to text first, with toString(x) or CAST(x AS STRING), \
                     and join text with ||, as in \
                     g.gql(\"MATCH (p:person) RETURN p.name || ' is ' || toString(p.age) AS s\"); \
                     or compute with numbers only, converting text with toInteger(x) or \
                     toFloat(x). ",
                );
            } else if topic == GqlHelpTopic::Subscript {
                help.push_str(GQL_SUBSCRIPT_ADVICE);
            } else if topic == GqlHelpTopic::TemporalArithmetic {
                help.push_str(GQL_TEMPORAL_ARITHMETIC_ADVICE);
            } else if topic == GqlHelpTopic::TemporalConstruct {
                help.push_str(GQL_TEMPORAL_CONSTRUCT_ADVICE);
            } else if topic == GqlHelpTopic::TemporalComponent {
                help.push_str(GQL_TEMPORAL_COMPONENT_ADVICE);
            } else if topic == GqlHelpTopic::TemporalTruncate {
                help.push_str(GQL_TEMPORAL_TRUNCATE_ADVICE);
            } else if topic == GqlHelpTopic::TemporalBetween {
                help.push_str(GQL_TEMPORAL_BETWEEN_ADVICE);
            } else if let Some(advice) = gql_data_status_advice(gqlstatus) {
                help.push_str(advice);
                help.push(' ');
            }
            help.push_str(GQL_DATA_ADVICE);
            if gqlstatus == crate::gql::eval::status::DIVISION_BY_ZERO {
                // Stage 3f: only an integer division fails on a zero divisor (and, 3g, a
                // duration's).
                help.push_str(
                    " Only an integer division (/ or %) fails on a zero divisor: a float one \
                     gives Infinity or NaN (IEEE 754), as in \
                     g.gql(\"MATCH (p:person) RETURN 100 / toFloat(p.age) AS r\"). A duration \
                     divided by zero fails whatever the number (d / 0 and d / 0.0).",
                );
            }
        }
    }
    help
}

/// The advice of an operand of a kind its place never takes, known from the query text
/// ([`GqlHelpTopic::StaticKind`], stage 3f): what each place takes, with runnable examples.
#[cfg(feature = "gql")]
const GQL_STATIC_KIND_ADVICE: &str = "The kind of the marked operand is certain from the query \
     text (a literal, a node, edge or path variable, or a value computed from them) and its \
     place never takes it, so gql() refuses the query before it runs. A condition (WHERE, \
     FILTER, AND, OR, XOR, NOT) takes a boolean: compare a value to get one, as in \
     g.gql(\"MATCH (p:person) WHERE p.age > 30 AND p.name STARTS WITH 'j' RETURN p.name\"); \
     IN takes a list, as in g.gql(\"MATCH (p:person) WHERE p.name IN ['marko', 'josh'] \
     RETURN p.age\"); type() takes an edge, labels() a node or an edge, length() and nodes() \
     a path and size() a string or a list, as in g.gql(\"MATCH p = (a:person)-[e:knows]->(b) \
     RETURN type(e), labels(a), length(p), size(nodes(p))\"); a property is read from a node, \
     an edge or a map, never from a path, a list or a number. The check never looks at the \
     value of a $parameter.";

/// The advice of a subscript of the wrong kind ([`GqlHelpTopic::Subscript`], stage 3f): what
/// `x[i]`, `m[k]` and `x[a..b]` read, and the conversions that fix the index.
#[cfg(feature = "gql")]
const GQL_SUBSCRIPT_ADVICE: &str = "x[i] reads the item i of a list, with an integer i \
     counted from 0 (negative from the end: x[-1] is the last item; out of range gives NULL); \
     convert another number or a text first with toInteger(i), as in \
     g.gql(\"RETURN [10, 20, 30][toInteger('1')] AS x\"). m[k] reads an entry of a map, or a \
     property of a node or an edge, by a string key, as in \
     g.gql(\"MATCH (p:person) RETURN p['na' || 'me'] AS name\"), converting a key with \
     toString(k). x[a..b] takes the items from a up to b (excluded) of a list, as in \
     g.gql(\"RETURN [1, 2, 3, 4][1..3] AS xs\"); a text is no list: use substring(s, start, \
     length) for a part of it. ";

/// The advice of a temporal constructor given a value it cannot build from
/// ([`GqlHelpTopic::TemporalConstruct`], stage 3g): what each constructor takes.
#[cfg(feature = "gql")]
const GQL_TEMPORAL_CONSTRUCT_ADVICE: &str = "date(), localtime(), localdatetime() and \
     datetime() (ISO local_time(), local_datetime(), zoned_datetime()) take ISO 8601 text, a map \
     of fields or a temporal value with the parts the result needs: date(x) of a date, a local \
     datetime or a datetime, localtime(x) of a local time, a local datetime or a datetime, \
     localdatetime(x) of a local datetime or a datetime, datetime(x) of a local datetime (read \
     at UTC) or a datetime; a date and a time are joined in a map, as in localdatetime({date: \
     d, hour: 12}); duration() takes text or a map of numbers, and datetime.fromepoch(seconds, \
     nanoseconds) and datetime.fromepochmillis(milliseconds) integers, as in \
     g.gql(\"RETURN date(localdatetime('2015-07-21T12:00')) AS d, \
     localdatetime({date: date('2015-07-21'), hour: 12}) AS t, \
     datetime.fromepochmillis(1500) AS e\"). ";

/// The advice of a component a temporal value does not have
/// ([`GqlHelpTopic::TemporalComponent`], stage 3g): the components of each kind.
#[cfg(feature = "gql")]
const GQL_TEMPORAL_COMPONENT_ADVICE: &str = "A component of a temporal value is read by its \
     name (in any case): a date, and the date of a local datetime or a datetime, has year, \
     quarter, month, week, weekYear, dayOfQuarter, day, ordinalDay and dayOfWeek; a time of \
     day has hour, minute, second, millisecond, microsecond and nanosecond; a datetime also \
     has timezone, offset, offsetMinutes, offsetSeconds, epochSeconds and epochMillis; a \
     duration has the totals years, quarters, months, weeks, days, hours, minutes, seconds, \
     milliseconds, microseconds and nanoseconds and the remainders such as monthsOfYear and \
     secondsOfMinute, as in g.gql(\"RETURN date('2015-07-21').week AS w, \
     datetime('2015-07-21T21:40+01:00').offsetMinutes AS o, duration('P1Y2M').months AS m\"). \
     A date has no time of day: join it with one first, as in localdatetime({date: d, hour: \
     12}).hour. ";

/// The advice of a unit or value `X.truncate()` cannot take
/// ([`GqlHelpTopic::TemporalTruncate`], stage 3g): the units of each type.
#[cfg(feature = "gql")]
const GQL_TEMPORAL_TRUNCATE_ADVICE: &str = "X.truncate(unit, value [, map]) takes a unit name \
     (in any case): date.truncate the units millennium, century, decade, year, weekYear, \
     quarter, month, week and day; localdatetime.truncate and datetime.truncate those and \
     hour, minute, second, millisecond, microsecond and nanosecond; localtime.truncate day \
     to nanosecond. The value is a temporal value with the parts the result needs (a date \
     gives a datetime at midnight), and the map sets fields of the result, as in \
     g.gql(\"RETURN date.truncate('month', date('2015-07-21')) AS d, \
     datetime.truncate('hour', datetime('2015-07-21T21:40:32+01:00'), {minute: 30}) AS t\"). \
     Millennia and centuries start at the years 2000 and 1900, as in Neo4j (the Gremlin step \
     date_trunc() starts them at 2001 and 1901). ";

/// The advice of an operand `duration.between()` cannot take
/// ([`GqlHelpTopic::TemporalBetween`], stage 3g).
#[cfg(feature = "gql")]
const GQL_TEMPORAL_BETWEEN_ADVICE: &str = "duration.between(a, b), duration.inMonths(a, b), \
     duration.inDays(a, b) and duration.inSeconds(a, b) take two dates, local times, local \
     datetimes or datetimes, never durations; kinds may be mixed (a value without a date \
     gives the times only, one without a time is read at midnight). Two local times have no \
     months or days: use duration.inSeconds(a, b) or duration.between(a, b) for them, as in \
     g.gql(\"RETURN duration.between(date('2015-01-01'), localdatetime('2015-03-15T12:00')) \
     AS u, duration.inSeconds(localtime('12:00'), localtime('13:30')) AS s\"). ";

/// The advice of a temporal value with a part below the microsecond
/// ([`GqlHelpTopic::TemporalPrecision`], stage 3g, 3g.P0 U1).
#[cfg(feature = "gql")]
const GQL_TEMPORAL_PRECISION_ADVICE: &str = "Temporal values hold microseconds, and gql() \
     never cuts digits silently: give at most six fraction digits, or the fields millisecond \
     and microsecond (nanosecond only in whole thousands, nanoseconds of a duration likewise), \
     as in g.gql(\"RETURN localtime('12:31:14.645876') AS t, localtime({hour: 12, minute: \
     31, second: 14, microsecond: 645876}) AS u, duration({seconds: 1, microseconds: 5}) AS \
     d\"). Nanosecond precision comes in a later version.";

/// The advice of temporal arithmetic without a meaning ([`GqlHelpTopic::TemporalArithmetic`],
/// stage 3g): the forms `+`, `-`, `*` and `/` take, and `duration.between` for the time between
/// two values.
#[cfg(feature = "gql")]
const GQL_TEMPORAL_ARITHMETIC_ADVICE: &str = "A duration is added to or subtracted from a \
     date, a local time, a local datetime, a datetime or another duration (x + d, d + x, x - d), \
     and multiplied or divided by a number (d * 2, d / 2); the time between two values is \
     duration.between(a, b) (or duration.inDays(a, b), duration.inSeconds(a, b)), as in \
     g.gql(\"RETURN date('2024-01-31') + duration('P1M') AS d, \
     duration.between(date('2024-01-01'), date('2024-03-15')) AS u\"); \
     a component such as u.days or d.year is a number to compute with. ";

/// The general advice of an error about an `INSERT` / `CREATE` pattern
/// ([`GqlHelpTopic::Insert`], stage 3a): what each part of the pattern may carry.
#[cfg(feature = "gql")]
const GQL_INSERT_ADVICE: &str = "INSERT (Cypher CREATE) creates its pattern once for each \
     row: a new node with the labels written, joined by & or :, and a property map; a new \
     edge with a direction and exactly one label; and a variable bound before, written alone \
     as (p), connects the existing node, as in g.gql(\"MATCH (p:person {name: 'marko'}) \
     INSERT (p)-[:likes]->(f:fruit&food {name: 'apple'}) RETURN f.name\"). A query may end \
     with the INSERT (it then returns no rows), and FOR gives it one row per item, as \
     in g.gql(\"FOR n IN ['ann', 'bob'] INSERT (:person {name: n})\"). To create a pattern \
     only where it is missing, MERGE finds it or creates it, as in \
     g.gql(\"FOR n IN ['ann', 'marko'] MERGE (:person {name: n})\").";

/// The general advice of an error about a `SET` / `REMOVE` clause ([`GqlHelpTopic::Set`],
/// stage 3c): the item forms.
#[cfg(feature = "gql")]
const GQL_SET_ADVICE: &str = "SET changes the nodes and edges of each row, item by item from \
     left to right: n.k = <value> sets a property (a NULL value removes it), n = {map} \
     replaces every property, n += {map} merges the map into them, and n:A:B adds labels to a \
     node (an edge has exactly one label); REMOVE n.k removes a property and REMOVE n:A a \
     label, as in g.gql(\"MATCH (p:person {name: 'marko'}) SET p.age = p.age + 1, p:employee \
     REMOVE p.nick RETURN p.age\"). A variable that OPTIONAL MATCH leaves NULL is skipped, \
     and a query may end with the SET or REMOVE (it then returns no rows), as in \
     g.gql(\"MATCH (s:software) SET s += {open: true}\").";

/// The advice of a schema violation a GQL `SET` / `REMOVE` raised (stage 3c), with a runnable
/// The general advice of an error about a `DELETE` clause ([`GqlHelpTopic::Delete`], stage
/// 3d): its targets, `DETACH`, and reading before the delete.
#[cfg(feature = "gql")]
const GQL_DELETE_ADVICE: &str = "DELETE removes, for each row, the nodes and edges its targets \
     give: a node or edge variable, a path (all its nodes and edges) or a list of them; a NULL \
     target is skipped, and an element given twice is removed once. A node that still has \
     edges is removed only together with them: DETACH DELETE removes its edges too, as in \
     g.gql(\"MATCH (s:software {name: 'lop'}) DETACH DELETE s\"). The rest of the query can \
     no longer read what it deleted, so read what it needs first, as in \
     g.gql(\"MATCH (p:person {name: 'vadas'}) WITH p, p.name AS name DETACH DELETE p RETURN \
     name\"); a query may end with the DELETE (it then returns no rows).";

/// The general advice of an error about a `MERGE` clause ([`GqlHelpTopic::Merge`], stage 3e):
/// match or create, what the pattern may carry, the `ON` items, rows seeing earlier creations.
#[cfg(feature = "gql")]
const GQL_MERGE_ADVICE: &str = "MERGE looks for its whole pattern for each row: every match \
     is a row, and when nothing matches the whole pattern is created, reusing the nodes bound \
     before. Its one pattern has nodes with the labels written, joined by & or :, and a \
     property map, edges with exactly one label (an edge without a direction matches either \
     direction and is created from left to right), and a node bound before written alone, as \
     (p); ON CREATE SET and ON MATCH SET items change what was created or matched, as in \
     g.gql(\"MERGE (p:person {name: 'ann'}) ON CREATE SET p.new = true ON MATCH SET p.new = \
     false RETURN p.new\"). Each row sees what the rows before it created, so FOR creates a \
     repeated item once, as in g.gql(\"FOR n IN ['ann', 'ann'] MERGE (:person {name: n})\").";

/// The help of a read of an element removed earlier in a GQL query (W27, W28 of stage 3d): what
/// still works after a `DELETE`, and the query that reads what it needs before it.
#[cfg(feature = "gql")]
fn gql_element_removed_help(kind: GraphNodeType) -> String {
    let (kind, still, example) = match kind {
        GraphNodeType::Vertex => (
            "node",
            "count(n) and n IS NULL",
            "MATCH (n:person {name: 'marko'}) WITH n, properties(n) AS p DETACH DELETE n \
             RETURN p",
        ),
        GraphNodeType::Edge => (
            "edge",
            "count(r), r IS NULL and type(r)",
            "MATCH (:person)-[r:knows]->() WITH r, properties(r) AS p DELETE r RETURN p\"), \
             or for a node g.gql(\"MATCH (n:person {name: 'marko'}) WITH n, properties(n) AS p \
             DETACH DELETE n RETURN p",
        ),
    };
    format!(
        "The {kind} was deleted earlier in this query (by DELETE or DETACH DELETE, or together \
         with a node a DETACH DELETE removed), but a later part of the query still reads it: a \
         deleted {kind} has no properties, labels or id any more and cannot be returned or \
         changed, and a pattern from it matches nothing; only {still} still work. Read what \
         you need before the DELETE and keep that instead, for example g.gql(\"{example}\"), or \
         run the DELETE as a separate query after reading."
    )
}

/// `SET` of the failing label and key: the conversion of a value of the wrong type, keeping a
/// required property, setting it before the label that requires it. `None` for the violations
/// whose own help fits (labels, connections, undeclared keys).
#[cfg(feature = "gql")]
///
/// `merge`: the violation came from an `ON CREATE SET` / `ON MATCH SET` item of a `MERGE`
/// (stage 3e): the examples are `MERGE .. ON MATCH SET` items.
fn gql_set_schema_help(error: &SchemaError, merge: bool) -> Option<String> {
    let set = if merge {
        "the ON CREATE SET or ON MATCH SET item"
    } else {
        "the SET"
    };
    match error {
        SchemaError::TypeMismatch {
            element_type,
            label,
            property_name,
            expected_type,
            actual_type,
        } if !is_nested_schema_path(property_name) && actual_type != "null" => {
            let (function, sample) = gql_conversion(expected_type)?;
            let example = gql_set_example(
                element_type,
                label,
                &format!("{{v}}.{} = {function}($v)", gql_key(property_name)),
                merge,
            );
            Some(format!(
                "Convert the value to the type '{expected_type}' where {set} writes it, with \
                 {function}(), as in g.gql(\"{example}\", #{{v: {sample}}}). {function}() gives \
                 NULL for a value it cannot convert, and SET of a NULL value removes the \
                 property; CAST(x AS ..) fails the query for it instead."
            ))
        }
        SchemaError::RequiredPropertyRemoval {
            element_type,
            label,
            property_name,
        } if !is_nested_schema_path(property_name) => {
            let key = gql_key(property_name);
            let keep = gql_set_example(
                element_type,
                label,
                &format!("{{v}} = {{{key}: {{v}}.{key}}}"),
                merge,
            );
            let merge = gql_set_example(
                element_type,
                label,
                &format!("{{v}} += {{{key}: 'value'}}"),
                merge,
            );
            Some(format!(
                "'{property_name}' is in \"required\" of {element_type} label '{label}': SET \
                 n.{key} = NULL and REMOVE n.{key} remove it, and SET n = {{map}} removes every \
                 key the map lacks. Keep it in the map, as in g.gql(\"{keep}\"), or merge with \
                 +=, which keeps the keys the map lacks, as in g.gql(\"{merge}\"), and give a \
                 value that may be NULL a fallback, as in SET n.{key} = coalesce($v, n.{key}). \
                 Or remove it from \"required\" in the schema (g.patch_schema(..)), or set the \
                 schema mode to \"none\"."
            ))
        }
        SchemaError::MissingRequiredProperty {
            element_type,
            label,
            property_name,
        } if *element_type == "vertex" && !is_nested_schema_path(property_name) => {
            let key = gql_key(property_name);
            let text = gql_key(label);
            let (example, same) = if merge {
                (
                    format!("MERGE (n {{name: 'marko'}}) ON MATCH SET n.{key} = 'value', n:{text}"),
                    "the same ON CREATE SET or ON MATCH SET",
                )
            } else {
                (
                    format!("MATCH (n) SET n.{key} = 'value', n:{text}"),
                    "the same SET",
                )
            };
            Some(format!(
                "The label '{label}' requires '{property_name}': set it before the label in \
                 {same} (the items apply from left to right), as in \
                 g.gql(\"{example}\"). Or remove it from the \
                 label's \"required\" in the schema (a \"default\" fills only elements \
                 created without the property, not a label added later)."
            ))
        }
        _ => None,
    }
}

/// A runnable `MATCH .. SET` on the modern graph changing the elements of `label`, `item`
/// with `{v}` for the variable; with `merge` a `MERGE .. ON MATCH SET` (stage 3e).
#[cfg(feature = "gql")]
fn gql_set_example(element_type: &str, label: &str, item: &str, merge: bool) -> String {
    let text = gql_key(label);
    let clause = if merge { "MERGE" } else { "MATCH" };
    let set = if merge { "ON MATCH SET" } else { "SET" };
    if element_type == "edge" {
        format!(
            "{clause} ()-[e:{text}]->() {set} {}",
            item.replace("{v}", "e")
        )
    } else {
        format!("{clause} (n:{text}) {set} {}", item.replace("{v}", "n"))
    }
}

/// The lenient conversion function to a schema type, with a sample text value for `$v`.
#[cfg(feature = "gql")]
fn gql_conversion(expected_type: &str) -> Option<(&'static str, &'static str)> {
    Some(match expected_type.to_lowercase().as_str() {
        "int64" | "number" => ("toInteger", "\"29\""),
        "float64" => ("toFloat", "\"1.5\""),
        "string" => ("toString", "29"),
        "boolean" => ("toBoolean", "\"true\""),
        _ => return None,
    })
}

/// The Rhai string literal `text` starts with, unquoted (`"a\"b" ..` gives `a"b`).
#[cfg(feature = "gql")]
fn rhai_string_prefix(text: &str) -> Option<String> {
    let mut chars = text.strip_prefix('"')?.chars();
    let mut out = String::new();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => out.push(chars.next()?),
            c => out.push(c),
        }
    }
    None
}

/// The variable written for the first node of `label` in the MERGE `pattern` (`(p:person
/// {..})` gives `p`); empty for an anonymous node or when there is none.
#[cfg(feature = "gql")]
fn merge_node_variable<'p>(pattern: &'p str, label: &str) -> &'p str {
    for (at, _) in pattern.match_indices('(') {
        let rest = &pattern[at + 1..];
        let end = rest
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(rest.len());
        let (variable, after) = rest.split_at(end);
        let labelled = after
            .strip_prefix(':')
            .and_then(|a| a.strip_prefix(label))
            .is_some_and(|a| !a.starts_with(|c: char| c.is_alphanumeric() || c == '_'));
        if labelled {
            return variable;
        }
    }
    ""
}

/// The advice of a schema violation a GQL `INSERT` raised (stage 3a), with a runnable
/// `INSERT` of the failing label and key: the conversion of a value of the wrong type, the
/// value of a missing required property. `None` for the violations whose advice names no
/// Gremlin step (labels, connections, undeclared keys): their own help fits. `merge` is the
/// pattern of the `MERGE` whose create raised it (stage 3e: the create is the INSERT routine),
/// whose node variable the examples keep.
#[cfg(feature = "gql")]
fn gql_schema_help(error: &SchemaError, merge: Option<&str>) -> Option<String> {
    let word = if merge.is_some() { "MERGE" } else { "INSERT" };
    let variable = |label: &str| merge.map_or("", |pattern| merge_node_variable(pattern, label));
    match error {
        SchemaError::TypeMismatch {
            element_type,
            label,
            property_name,
            expected_type,
            actual_type,
        } if !is_nested_schema_path(property_name) && actual_type != "null" => {
            let (function, sample) = gql_conversion(expected_type)?;
            let example = gql_insert_example(
                element_type,
                label,
                &format!("{}: {function}($v)", gql_key(property_name)),
                word,
                variable(label),
            );
            Some(format!(
                "Convert the value to the type '{expected_type}' where the {word} writes it, \
                 with {function}(), as in g.gql(\"{example}\", #{{v: {sample}}}). {function}() \
                 gives NULL for a value it cannot convert, and a NULL value is not stored; \
                 CAST(x AS ..) fails the query for it instead."
            ))
        }
        SchemaError::MissingRequiredProperty {
            element_type,
            label,
            property_name,
        } if !is_nested_schema_path(property_name) => {
            let key = gql_key(property_name);
            let example = gql_insert_example(
                element_type,
                label,
                &format!("{key}: 'value'"),
                word,
                variable(label),
            );
            // C8 of stage 3e: the ON CREATE items run after the create.
            let after = if word == "MERGE" {
                " pattern (the ON CREATE SET items run after the create, so they cannot give \
                 it; to match on other keys only, give it a \"default\" in the schema)"
            } else {
                ""
            };
            Some(format!(
                "Give '{property_name}' a value in the property map of the {word}{after}, as in \
                 g.gql(\"{example}\"); a NULL value (such as a property the matched element \
                 lacks) is not stored and so is missing too: give it a fallback, as in \
                 {{{key}: coalesce(n.{key}, 'unknown')}}. Or remove it from the label's \
                 \"required\" in the schema, or give it a \"default\" (or \"defaultFunction\") \
                 in the schema: it is stored when an element is created without the property."
            ))
        }
        _ => None,
    }
}

/// A runnable `INSERT` (or, `word`, `MERGE`) on the modern graph of one element of `label`
/// with the property map `entries` (a node, or an edge between two of its people).
#[cfg(feature = "gql")]
/// `variable` names the node.
fn gql_insert_example(
    element_type: &str,
    label: &str,
    entries: &str,
    word: &str,
    variable: &str,
) -> String {
    let label = gql_key(label);
    if element_type == "edge" {
        format!(
            "MATCH (a {{name: 'marko'}}), (b {{name: 'josh'}}) {word} (a)-[:{label} \
             {{{entries}}}]->(b)"
        )
    } else {
        format!("{word} ({variable}:{label} {{{entries}}})")
    }
}

/// A label or a property key as GQL text: back-quoted unless it is a plain name.
#[cfg(feature = "gql")]
pub(crate) fn gql_key(name: &str) -> String {
    let plain = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if plain {
        name.to_string()
    } else {
        format!("`{}`", name.replace('`', "``"))
    }
}

/// The general advice of a [`GqlErrorKind::Data`] error.
#[cfg(feature = "gql")]
const GQL_DATA_ADVICE: &str = "A row of the data made the marked expression fail. Keep such \
     rows away from it with a WHERE condition or a CASE expression, for example \
     g.gql(\"MATCH (p:person) WHERE p.age <> 0 RETURN 100 / p.age\"), and compare values of \
     one kind only (no implicit conversion between text and numbers).";

/// The advice of a stage 1c data exception by its GQLSTATUS code, naming the function that
/// fixes it (rule 6); `None` for the codes the general advice covers.
#[cfg(feature = "gql")]
fn gql_data_status_advice(gqlstatus: &str) -> Option<&'static str> {
    Some(match gqlstatus {
        // Invalid character value for cast.
        "22018" => {
            "CAST is strict: text that is not a value of the target type fails the query. The \
             Cypher conversions are lenient: toInteger(x), toFloat(x) and toBoolean(x) give \
             NULL for such text, and toIntegerOrNull(x), toFloatOrNull(x) and \
             toBooleanOrNull(x) give NULL for a value of any other kind too, as in \
             g.gql(\"MATCH (p:person) RETURN p.name, toInteger(p.name) AS n\")."
        }
        // Substring error.
        "22011" => {
            "substring(s, start, length), left(s, n) and right(s, n) take non-negative \
             integers (positions count characters from 0); a position or length past the end \
             gives '' or the rest of the string, never an error, as in \
             g.gql(\"MATCH (p:person) RETURN substring(p.name, 1, 100) AS s\")."
        }
        // Trim error.
        "22027" => {
            "TRIM(c FROM s) removes one character c; to remove any of several characters use \
             btrim(s, chars), ltrim(s, chars) or rtrim(s, chars), as in \
             g.gql(\"MATCH (p:person) RETURN btrim(p.name, 'mo') AS s\")."
        }
        // Invalid argument for natural logarithm.
        "2201E" => {
            "ln(x), log(x) and log10(x) take a positive number, and log(base, x) also a \
             positive base other than 1; keep other rows away, as in \
             g.gql(\"MATCH (p:person) WHERE p.age > 0 RETURN ln(p.age) AS l\")."
        }
        // Invalid argument for power function.
        "2201F" => {
            "sqrt(x) takes a number >= 0, and power(x, y) cannot raise 0 to a negative power \
             or a negative number to a power that is not an integer (nor can x ^ y); keep other \
             rows away, as in \
             g.gql(\"MATCH (p:person) RETURN CASE WHEN p.age >= 0 THEN sqrt(p.age) END AS r\")."
        }
        // Invalid parameter value.
        "22023" => {
            "An argument is outside the values the function accepts: range() takes a \
             step other than 0, round() a precision >= 0 and one of the modes UP, DOWN, \
             CEILING, FLOOR, HALF_UP, HALF_DOWN or HALF_EVEN (upper case), replace() a limit \
             >= 0 and the list trim(list, n) an n >= 0, as in \
             g.gql(\"MATCH (p:person) RETURN round(p.age / 3.0, 1, 'HALF_EVEN') AS r\")."
        }
        // Invalid datetime format (stage 3g).
        "22007" => {
            "A temporal constructor reads ISO 8601 text of its own type (date('2015-07-21'), \
             date('2015-W30-2'), localtime('21:40:32'), localdatetime('2015-07-21T21:40'), \
             datetime('2015-07-21T21:40+01:00'), duration('P14DT16H12M')) or a map of its \
             fields: year with month / day, week / dayOfWeek, ordinalDay or quarter / \
             dayOfQuarter; hour, minute, second, millisecond, microsecond, nanosecond; a \
             datetime's timezone as an offset ('+01:00') or a zone name ('Europe/Berlin'); a \
             base value as date, time or datetime; a duration's years .. nanoseconds, as in \
             g.gql(\"RETURN date({year: 2015, week: 30, dayOfWeek: 2}) AS d, \
             datetime({year: 1984, month: 10, day: 11, hour: 12, timezone: 'Europe/Berlin'}) \
             AS t, duration({days: 14, hours: 16}) AS u\")."
        }
        // Datetime field overflow (stage 3g).
        "22008" => {
            "Temporal fields stay within their ranges (month 1 to 12, day within its month, \
             week within its week-numbering year, hour 0 to 23, minute and second 0 to 59, \
             millisecond 0 to 999) and every value within the years -9999 to 9999, as in \
             g.gql(\"RETURN date({year: 1984, month: 10, day: 11}) AS d, \
             localtime({hour: 23, minute: 59, second: 59, millisecond: 999}) AS t\")."
        }
        _ => return None,
    })
}

/// The general advice of a [`GqlErrorKind::Semantic`] error (class `42`) about `topic`.
#[cfg(feature = "gql")]
fn gql_semantic_advice(topic: GqlHelpTopic) -> &'static str {
    match topic {
        GqlHelpTopic::Scope => {
            "WITH passes on only its columns: a variable it does not list is out of scope after \
             it (only the WHERE of a WITH without an aggregate still reads it), so list every \
             variable a later clause reads, as in \
             g.gql(\"MATCH (p:person)-[:created]->(s) WITH p, count(s) AS n RETURN p.name, n\"); \
             a WITH item other than a variable needs a name (AS). LET adds variables and keeps \
             all others in scope, each with a new name, as in \
             g.gql(\"MATCH (p:person) LET name = upper(p.name) RETURN p.age, name\")."
        }
        GqlHelpTopic::Grouping => {
            "A RETURN or WITH with an aggregate groups the rows by its other items (the \
             grouping keys): read a variable outside an aggregate only in such a key, and \
             after it (as after DISTINCT) only its columns remain, so ORDER BY names a column \
             or writes an item again inside an expression, as in g.gql(\"MATCH \
             (p:person)-[:created]->(s) RETURN p.name, count(s) AS n ORDER BY n\") or \
             g.gql(\"MATCH (p:person)-[:created]->(s) RETURN p.name, count(s) AS n ORDER BY \
             count(s) * 10 + size(p.name)\"). An aggregate takes no aggregate as its argument; aggregate in two steps \
             with WITH, as in g.gql(\"MATCH (p:person)-[:created]->(s) WITH p, count(s) AS n \
             RETURN max(n)\"). A computed key (not a variable or a property) is not repeated \
             in an item that aggregates: compute it in a WITH first and read it by name, as in \
             g.gql(\"MATCH (p:person)-[:created]->(s) WITH *, p.age + 1 AS k RETURN k, k + \
             count(s) AS n\")."
        }
        GqlHelpTopic::PatternVariable => {
            "In a MATCH clause a node variable names one node (written twice, it joins on the \
             same node), an edge variable one edge, a path variable the path after its `=`, \
             the variable of a quantified edge the list of its edges, and a variable inside \
             a quantified path pattern the list of its nodes or edges, one per iteration, as \
             in g.gql(\"MATCH p = (a:person)-[e:knows]->(b) RETURN p, e, b.name\") and \
             g.gql(\"MATCH (a:person) ((x)-[e:knows]->(y)){1,2} (b) RETURN a.name, y\"); a \
             name defined by LET or WITH is a value, which a pattern may continue from when it \
             holds a node or an edge, as in g.gql(\"MATCH (p:person) WITH collect(p) AS ps \
             WITH ps[0] AS first MATCH (first)-->(x) RETURN x.name\")."
        }
        GqlHelpTopic::Names => {
            "Give every column of a RETURN or WITH, every variable of a LET and every key of a \
             map a name of its own, as in \
             g.gql(\"MATCH (p:person) RETURN p.name AS name, p.age AS age\")."
        }
        GqlHelpTopic::For => {
            "FOR x IN <list> (Cypher UNWIND <list> AS x) gives one row per item of the list, \
             which it reads on the row before the FOR; the item x and, with WITH ORDINALITY i \
             (from 1) or WITH OFFSET i (from 0), its position i are two new variables with \
             names of their own; a node item continues a pattern by its name, as in \
             FOR n IN nodes(p) MATCH (n)-->(m), and any other item is compared in a property \
             map or WHERE, as in g.gql(\"FOR n IN ['marko', 'josh'] WITH ORDINALITY i MATCH \
             (p:person {name: n}) RETURN i, p.age\")."
        }
        GqlHelpTopic::Subquery => {
            "A subquery, EXISTS { MATCH <pattern> [WHERE <condition>] }, EXISTS { <pattern> } \
             or a Cypher pattern predicate such as (p)-[:created]->(), is TRUE when its body \
             has a match and FALSE otherwise, never NULL; its body reads the variables bound \
             before it, and the variables it binds itself stay inside it. A pattern predicate \
             binds no new variable: name new elements in an EXISTS { MATCH .. }, as in \
             g.gql(\"MATCH (p:person) WHERE NOT EXISTS { MATCH (p)-[:created]->(s) WHERE \
             s.lang = 'java' } RETURN p.name\"), and bind an element in the MATCH clause of the \
             query to read it after the subquery, as in \
             g.gql(\"MATCH (p:person)-[:created]->(s) RETURN p.name, s.name\")."
        }
        GqlHelpTopic::CountCollect => {
            "COUNT { <body> } is the number of matches of its body (or of the records its \
             RETURN gives), and COLLECT { <body> RETURN <expression> } the list of the values \
             it returns; the body reads the variables bound before it, and the variables it \
             binds itself stay inside it, as in g.gql(\"MATCH (p:person) RETURN p.name, \
             COUNT { (p)-[:knows]->() } AS friends, COLLECT { MATCH (p)-[:created]->(s) \
             RETURN s.name } AS made\"). Bind an element in the MATCH clause of the query to \
             read it after the subquery, or return several columns from a CALL subquery, as \
             in g.gql(\"MATCH (p:person) CALL (p) { MATCH (p)-[:created]->(s) RETURN s.name \
             AS made, s.lang AS lang } RETURN p.name, made, lang\")."
        }
        GqlHelpTopic::PatternComprehension => {
            "A pattern comprehension, [(a)-[:knows]->(f) WHERE <condition> | <expression>], \
             is the list of its | part evaluated for every match of its pattern (the empty \
             list when nothing matches); its pattern reads the variables bound before it, and \
             the variables it binds itself stay inside it. Read them in the | part, as in \
             g.gql(\"MATCH (p:person) RETURN p.name, [(p)-[:knows]->(f) | f.name] AS \
             friends\"), and bind an element in the MATCH clause of the query to read it after \
             the comprehension, as in g.gql(\"MATCH (p:person)-[:knows]->(f) RETURN p.name, \
             f.name\")."
        }
        GqlHelpTopic::ComprehensionVariable => {
            "A list comprehension, [x IN <list> WHERE <condition> | <expression>], binds its \
             variable x to each item of the list, inside the comprehension only; x must not \
             hide a name in scope: not a variable or column in scope, not the variable of a \
             comprehension around it, and not a name that the value of a LET or WITH variable \
             in scope reads (a name an earlier clause dropped may be used again), as in g.gql(\"MATCH (p:person) RETURN p.name, [a IN [p.age, \
             p.age + 1] WHERE a > 30 | a * 2] AS l\"). The same holds for the variable of \
             all(), any(), none() and single() and for both variables of reduce(), as in \
             g.gql(\"MATCH (p:person) RETURN p.name, all(a IN [p.age] WHERE a > 30) AS old, \
             reduce(s = 0, a IN [p.age, 1] | s + a) AS total\")."
        }
        GqlHelpTopic::Composite => {
            "UNION combines the records of queries that return the same column names, in any \
             order (the result keeps the order of the first query): UNION (or UNION DISTINCT) \
             removes duplicate records, UNION ALL keeps them, and one chain of UNIONs uses one \
             kind. Each query of a UNION has its own variables, ORDER BY, OFFSET and LIMIT. \
             NEXT passes the records of the statement before it on by their column names \
             (only those), so it sorts, filters or aggregates a UNION as a whole, as in \
             g.gql(\"MATCH (p:person) RETURN p.name AS name UNION MATCH (s:software) RETURN \
             s.name AS name NEXT RETURN name ORDER BY name LIMIT 3\")."
        }
        GqlHelpTopic::Call => {
            "A CALL subquery, CALL (a, b) { <query ending in RETURN, or in a write (INSERT, SET, \
             REMOVE, DELETE, MERGE / FINISH)> }, runs its query once for each incoming row: the body reads only the variables its scope clause names \
             (CALL (*) imports all of them, CALL { } none, and Cypher's CALL { WITH a .. } \
             those of its first WITH), and passes on only the columns of its RETURN, which \
             must be new names; its aggregates, ORDER BY and LIMIT apply to each row's \
             records, as in g.gql(\"MATCH (p:person) CALL (p) { MATCH (p)-[:created]->(s) \
             RETURN count(s) AS made } RETURN p.name, made\"). A row whose body returns no \
             record is dropped; OPTIONAL CALL keeps it with NULL columns."
        }
        GqlHelpTopic::Insert => GQL_INSERT_ADVICE,
        GqlHelpTopic::Set => GQL_SET_ADVICE,
        GqlHelpTopic::Delete => GQL_DELETE_ADVICE,
        GqlHelpTopic::Merge => GQL_MERGE_ADVICE,
        GqlHelpTopic::Kind
        | GqlHelpTopic::AggregateValue
        | GqlHelpTopic::Percentile
        | GqlHelpTopic::TextAndNumber
        | GqlHelpTopic::Subscript
        | GqlHelpTopic::StaticKind
        | GqlHelpTopic::TemporalArithmetic
        | GqlHelpTopic::TemporalConstruct
        | GqlHelpTopic::TemporalComponent
        | GqlHelpTopic::TemporalTruncate
        | GqlHelpTopic::TemporalBetween
        | GqlHelpTopic::TemporalPrecision => {
            "Declare every variable in a MATCH pattern before WHERE, RETURN or ORDER BY uses \
             it (LET, WITH and FOR define variables too), for example \
             g.gql(\"MATCH (p:person)-[:knows]->(f) RETURN p.name, f.name\"), and give every \
             $parameter a value in the parameters of gql()."
        }
    }
}

/// The internal step every lowered GQL quantifier has in its `repeat()` body (the trail check),
/// by which a loop limit is recognised as one of a GQL query.
#[cfg(feature = "gql")]
const GQL_TRAIL: &str = "gql:trail(";

/// A quantified edge of a GQL query, or a quantified path pattern (stage 2e), read back from
/// its lowered loop for the advice of a limit.
#[cfg(feature = "gql")]
#[derive(Debug, Clone, PartialEq, Eq)]
struct GqlQuantifiedEdge {
    /// The edge pattern without its quantifier, such as `-[e:knows]->`, or the parenthesized
    /// path pattern, such as `((x)-[e:knows]->(y)-[f]->())` (in walk order).
    pattern: String,
    /// A quantified path pattern, not a single quantified edge.
    group: bool,
    /// The lower bound of the quantifier.
    min: u64,
    /// The upper bound; `None` for none.
    max: Option<u64>,
}

#[cfg(feature = "gql")]
impl GqlQuantifiedEdge {
    /// The edge with its quantifier, such as `-[e:knows]->{2,}`.
    fn render(&self) -> String {
        format!("{}{}", self.pattern, quantifier_text(self.min, self.max))
    }

    /// The edge with an upper bound added, for the advice: `{m, 5m}` (`{0,5}` for `m = 0`).
    fn bounded(&self) -> String {
        let max = self.max.unwrap_or(self.min.max(1).saturating_mul(5));
        format!("{}{}", self.pattern, quantifier_text(self.min, Some(max)))
    }
}

/// `{m,}`, `{n}` or `{m,n}`.
#[cfg(feature = "gql")]
fn quantifier_text(min: u64, max: Option<u64>) -> String {
    match max {
        None => format!("{{{min},}}"),
        Some(max) if max == min => format!("{{{min}}}"),
        Some(max) => format!("{{{min},{max}}}"),
    }
}

/// The byte index of the `)` closing the `(` at `open` in rendered step text; quoted strings
/// (with `\` escapes) are skipped. `None` when it does not close.
#[cfg(feature = "gql")]
fn closing_paren(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (at, c) in text.get(open..)?.char_indices() {
        if in_string {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '(' => depth += 1,
            ')' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(open + at);
                }
            }
            _ => {}
        }
    }
    None
}

/// The contents of the quoted strings in rendered step arguments (`"a", "b"` -> `a`, `b`).
#[cfg(feature = "gql")]
fn quoted_strings(args: &str) -> Vec<String> {
    let mut strings = Vec::new();
    let mut current: Option<String> = None;
    let mut escaped = false;
    for c in args.chars() {
        match current.as_mut() {
            Some(text) if escaped => {
                text.push(c);
                escaped = false;
            }
            Some(_) if c == '\\' => escaped = true,
            Some(_) if c == '"' => strings.extend(current.take()),
            Some(text) => text.push(c),
            None if c == '"' => current = Some(String::new()),
            None => {}
        }
    }
    strings
}

/// Every quantified edge whose lowered loop is in `plan` (rendered steps joined with `.`), in
/// plan order. Reads the forms of `gql::lower` (quantified edge): `emit().repeat(B)` is the
/// lower bound 0, `repeat(B).emit()` 1, `repeat(B).emit(__.loops().is(P.gte(m)))` `m`, a
/// following `times(n)` the upper bound (and the lower one when no `emit` gives it); the body
/// `B` is the edge hop (`out_e`, `in_e`, or a `union` for either direction) with the trail
/// check and `as(e)` of its variable.
#[cfg(feature = "gql")]
fn gql_quantified_edges(plan: &str) -> Vec<GqlQuantifiedEdge> {
    let mut edges = Vec::new();
    let mut from = 0;
    while let Some(found) = plan.get(from..).and_then(|rest| rest.find("repeat(")) {
        let start = from + found;
        let open = start + "repeat".len();
        let Some(close) = closing_paren(plan, open) else {
            break;
        };
        from = close + 1;
        let body = plan.get(open + 1..close).unwrap_or_default();
        if !body.contains(GQL_TRAIL) {
            continue;
        }
        let mut min = plan
            .get(..start)
            .is_some_and(|prefix| prefix.ends_with("emit()."))
            .then_some(0);
        let mut max = None;
        let mut rest = plan.get(close + 1..).unwrap_or_default();
        let number = |text: &str| -> (Option<u64>, usize) {
            let digits = text.bytes().take_while(u8::is_ascii_digit).count();
            (text.get(..digits).and_then(|d| d.parse().ok()), digits)
        };
        loop {
            if let Some(after) = rest.strip_prefix(".emit()") {
                min = min.or(Some(1));
                rest = after;
            } else if let Some(after) = rest.strip_prefix(".emit(__.loops().is(P.gte(") {
                let (value, digits) = number(after);
                min = value.or(min);
                rest = after
                    .get(digits..)
                    .and_then(|r| r.strip_prefix(")))"))
                    .unwrap_or_default();
            } else if let Some(after) = rest.strip_prefix(".times(") {
                let (value, digits) = number(after);
                max = value;
                rest = after
                    .get(digits..)
                    .and_then(|r| r.strip_prefix(')'))
                    .unwrap_or_default();
            } else {
                break;
            }
        }
        let min = min.or(max).unwrap_or(1);
        let (pattern, group) = match gql_group_pattern(body) {
            Some(pattern) => (pattern, true),
            None => (gql_edge_pattern(body), false),
        };
        edges.push(GqlQuantifiedEdge {
            pattern,
            group,
            min,
            max,
        });
    }
    edges
}

/// The steps of rendered step text at the top level (split at `.` outside parentheses and
/// quoted strings), without a leading `__`.
#[cfg(feature = "gql")]
fn top_level_steps(text: &str) -> Vec<&str> {
    let mut steps = Vec::new();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut start = 0;
    for (at, c) in text.char_indices() {
        if in_string {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            '.' if depth == 0 => {
                steps.extend(text.get(start..at));
                start = at + 1;
            }
            _ => {}
        }
    }
    steps.extend(text.get(start..));
    steps.retain(|step| *step != "__" && !step.is_empty());
    steps
}

/// The parenthesized path pattern of a lowered quantified path pattern's loop body (stage
/// 2e), in walk order, such as `((x)-[e:knows]->(y)-[f]->())`: its hops (an edge step, the
/// steps on the edge up to the vertex step, or the `union` of either direction) and the node
/// parts between them (a variable from `as()`, labels from `has_label()`). `None` for the body
/// of a single quantified edge: one hop whose node parts hold nothing but an internal label (a
/// one-edge body with nothing on its nodes reads like one, which it equals).
#[cfg(feature = "gql")]
fn gql_group_pattern(body: &str) -> Option<String> {
    let steps = top_level_steps(body);
    let mut nodes: Vec<(Option<String>, Vec<String>)> = vec![(None, Vec::new())];
    let mut edges: Vec<String> = Vec::new();
    let mut on_nodes = false;
    let mut hop: Option<Vec<&str>> = None;
    for step in steps {
        if let Some(open) = hop.as_mut() {
            open.push(step);
            if ["in_v()", "inV()", "out_v()", "outV()"].contains(&step) {
                edges.push(gql_edge_pattern(&format!("__.{}", open.join("."))));
                nodes.push((None, Vec::new()));
                hop = None;
            }
            continue;
        }
        let name = step.split('(').next().unwrap_or_default();
        match name {
            "out_e" | "outE" | "in_e" | "inE" => hop = Some(vec![step]),
            "union" => {
                edges.push(gql_edge_pattern(&format!("__.{step}")));
                nodes.push((None, Vec::new()));
            }
            "as" => {
                let label = quoted_strings(step).into_iter().next().unwrap_or_default();
                if let Some(node) = nodes.last_mut()
                    && !label.starts_with('#')
                    && node.0.is_none()
                {
                    node.0 = Some(label);
                    on_nodes = true;
                }
            }
            "has_label" | "hasLabel" => {
                if let Some(node) = nodes.last_mut() {
                    node.1.push(quoted_strings(step).join("|"));
                    on_nodes = true;
                }
            }
            _ => on_nodes = true,
        }
    }
    if edges.len() < 2 && !on_nodes {
        return None;
    }
    let node = |(name, labels): &(Option<String>, Vec<String>)| {
        let mut text = name.clone().unwrap_or_default();
        for label in labels {
            text.push(':');
            text.push_str(label);
        }
        format!("({text})")
    };
    let mut pattern = String::from("(");
    for (at, n) in nodes.iter().enumerate() {
        pattern.push_str(&node(n));
        if let Some(edge) = edges.get(at) {
            pattern.push_str(edge);
        }
    }
    pattern.push(')');
    Some(pattern)
}

/// The GQL edge pattern of a lowered quantifier body (without the quantifier): the direction
/// and labels of its hop, the variable of its `as()` after the trail check (an internal `#..`
/// label is no variable), and ` WHERE ..` when the body filters the edge.
#[cfg(feature = "gql")]
fn gql_edge_pattern(body: &str) -> String {
    let (either, hop) = match body.strip_prefix("__.union(") {
        Some(branches) => (true, branches),
        None => (false, body),
    };
    let hop = hop.strip_prefix("__.").unwrap_or(hop);
    let (outgoing, name_len) = if hop.starts_with("out_e(") {
        (true, "out_e".len())
    } else if hop.starts_with("outE(") {
        (true, "outE".len())
    } else if hop.starts_with("in_e(") {
        (false, "in_e".len())
    } else if hop.starts_with("inE(") {
        (false, "inE".len())
    } else {
        (true, 0)
    };
    let hop_end = closing_paren(hop, name_len).filter(|_| name_len > 0);
    let labels = hop_end
        .and_then(|end| hop.get(name_len + 1..end))
        .map(quoted_strings)
        .unwrap_or_default();
    let trail = hop.find(GQL_TRAIL);
    let filtered = hop
        .get(hop_end.map_or(0, |end| end + 1)..trail.unwrap_or(0))
        .is_some_and(|between| between.contains("has") || between.contains("gql:filter"));
    let variable = trail
        .and_then(|at| closing_paren(hop, at + GQL_TRAIL.len() - 1))
        .and_then(|end| hop.get(end + 1..))
        .and_then(|after| after.strip_prefix(".as("))
        .map(quoted_strings)
        .and_then(|names| names.into_iter().next())
        .filter(|name| !name.starts_with('#'))
        .unwrap_or_default();
    let mut inner = variable;
    if !labels.is_empty() {
        inner.push(':');
        inner.push_str(&labels.join("|"));
    }
    if filtered {
        inner.push_str(" WHERE ..");
    }
    let inner = inner.trim_start().to_string();
    match (either, outgoing) {
        (true, _) => format!("-[{inner}]-"),
        (false, true) => format!("-[{inner}]->"),
        (false, false) => format!("<-[{inner}]-"),
    }
}

/// Where a resource limit inside a lowered GQL query was reached: outside any subquery, inside
/// an `EXISTS` subquery / pattern predicate (a `filter()` / `not()` child or a hosted child that
/// stops at its first match), inside a pattern comprehension (a hosted child ending in
/// `gql:value`, run to its end), a `COUNT { }` / `COLLECT { }` subquery or a `CALL { }` body
/// (the child of `flat_map()`). The innermost of them decides.
#[cfg(feature = "gql")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GqlSubqueryPlace {
    Outside,
    Exists,
    Comprehension,
    /// A `COUNT { }` subquery (a hosted child ending in `count()`, run to its end).
    Count,
    /// A `COLLECT { }` subquery (a hosted child ending in `gql:value` after its `RETURN`).
    Collect,
    /// A `CALL { }` body (the child of `flat_map()`, run to its end for each row).
    Call,
}

#[cfg(feature = "gql")]
impl GqlSubqueryPlace {
    /// The place as a phrase after the construct it qualifies (empty outside a subquery).
    fn phrase(self) -> &'static str {
        match self {
            GqlSubqueryPlace::Outside => "",
            GqlSubqueryPlace::Exists => " inside an EXISTS subquery (or pattern predicate)",
            GqlSubqueryPlace::Comprehension => " inside a pattern comprehension [(a)-->(b) | ..]",
            GqlSubqueryPlace::Count => " inside a COUNT { } subquery",
            GqlSubqueryPlace::Collect => " inside a COLLECT { } subquery",
            GqlSubqueryPlace::Call => " inside a CALL { } subquery",
        }
    }

    /// Why a loop there runs as long as it does (empty outside a subquery).
    fn why_it_runs(self) -> &'static str {
        match self {
            GqlSubqueryPlace::Outside => "",
            GqlSubqueryPlace::Exists => {
                " A subquery stops at its first match, so its loop runs this long on a row that \
                 has no match, or only a late one."
            }
            GqlSubqueryPlace::Comprehension => {
                " A pattern comprehension collects every match of its pattern for each row, so \
                 its loop runs until the row's last match is found."
            }
            GqlSubqueryPlace::Count => {
                " A COUNT { } subquery counts every match of its body for each row, so its loop \
                 runs until the row's last match is found."
            }
            GqlSubqueryPlace::Collect => {
                " A COLLECT { } subquery collects every row of its body for each row, so its \
                 loop runs until the row's last match is found."
            }
            GqlSubqueryPlace::Call => {
                " A CALL { } subquery runs its body for each incoming row and keeps every \
                 record it returns, so its loop runs until the row's last match is found."
            }
        }
    }
}

/// Advice for [`TraverserError::RepeatLimitExceeded`] raised by the loop of a GQL quantifier:
/// bound the quantifier or raise `repeat.max_loops` for the `gql()` call, never the internal
/// `repeat()` body. `step` is the failing loop as rendered.
#[cfg(feature = "gql")]
fn gql_repeat_limit_help(max_loops: u32, step: &str, place: GqlSubqueryPlace) -> String {
    let raised = u64::from(max_loops)
        .saturating_mul(2)
        .min(u64::from(u32::MAX));
    let subquery = place.why_it_runs();
    let place = place.phrase();
    let (edge, bounded, repeated) = match gql_quantified_edges(step).into_iter().next() {
        Some(edge) => (
            format!(
                "the quantified {} {}{place} of the gql() query",
                if edge.group { "path pattern" } else { "edge" },
                edge.render()
            ),
            format!("{}, ", edge.bounded()),
            if edge.group { "pattern" } else { "edge" },
        ),
        None => (
            format!("a quantified edge{place} of the gql() query"),
            String::new(),
            "edge",
        ),
    };
    format!(
        "The loop of {edge} ran {max_loops} iterations, the limit of repeat.max_loops. A \
         quantifier without an upper bound ({{m,}}, *, + or Cypher [*]) repeats the \
         {repeated} until every path has used all the edges it can reach (trail), which takes \
         more iterations than the limit on a large or densely connected graph; the result is \
         never cut short.{subquery} Give the quantifier an upper bound, as in {bounded}for example \
         g.gql(\"MATCH (a:person)-[:knows]->{{1,3}}(b) RETURN b.name\"). Or raise the limit for \
         the query, g.with(\"repeat.max_loops\", {raised}).gql(..), for example \
         g.with(\"repeat.max_loops\", {raised}).gql(\"MATCH (a)-[:knows]->{{1,}}(b) RETURN \
         b.name\"). It is an administrative option: a script whose host restricts it needs \
         Update on that Option to set it."
    )
}

/// Advice for `traversal.max_traversers` exceeded by a lowered GQL query (`steps`, the
/// outermost plan): narrow the pattern, bound its quantified edges, or raise the limit for the
/// `gql()` call.
#[cfg(feature = "gql")]
fn gql_traverser_limit_help(
    max: Option<u64>,
    steps: &[String],
    subquery: Option<&[String]>,
    place: GqlSubqueryPlace,
) -> String {
    let raised = max.map_or(1_000_000, |max| max.saturating_mul(2).max(1));
    // A child of a subquery fails on its own level, which the outermost plan shows only as its
    // host (a `gql:*` step renders no children): read the quantifiers of the failing level.
    let (edges, place) = match subquery {
        Some(level) => (
            gql_quantified_edges(&level.join(".")),
            format!(",{},", place.phrase()),
        ),
        None => (gql_quantified_edges(&steps.join(".")), String::new()),
    };
    let noun = match (edges.iter().any(|e| e.group), edges.iter().all(|e| e.group)) {
        (false, _) => "quantified edge",
        (true, true) => "quantified path pattern",
        (true, false) => "quantified edge or path pattern",
    };
    let quantified = match edges.as_slice() {
        [] => String::new(),
        [first, ..] => format!(
            "Each path a {noun} ({}){place} matches is a traverser of its own (trail \
             keeps them apart, so they do not merge), and their number grows with every \
             iteration: give it a lower upper bound, as in {}. ",
            edges
                .iter()
                .map(GqlQuantifiedEdge::render)
                .collect::<Vec<_>>()
                .join(", "),
            first.bounded()
        ),
    };
    format!(
        "The gql() query kept more traversers (rows in the making) alive between two steps \
         than 'traversal.max_traversers' allows{}. {quantified}Narrow the pattern with labels \
         and conditions, for example \
         g.gql(\"MATCH (p:person)-[:knows]->(f) WHERE f.age > 30 RETURN f.name\"), or raise \
         the limit for the query, g.with(\"traversal.max_traversers\", {raised}).gql(..), for \
         example g.with(\"traversal.max_traversers\", {raised}).gql(\"MATCH (a)-->(b) RETURN \
         count(*)\"). A host can lock this option with \
         ExecutionPolicy::with_locked(\"traversal.max_traversers\"); 9223372036854775807 is as \
         good as unlimited. Setting it is an administrative option: a script whose host \
         restricts it needs Update on that Option.",
        max.map(|max| format!(" ({max})")).unwrap_or_default()
    )
}

/// Advice for `traversal.max_materialized_bytes` exceeded by an internal `gql:*` step: a
/// function result that can outgrow its arguments, a `collect_list()` list or the list of a pattern
/// comprehension or a `COLLECT { }` subquery, refused before it
/// was built (design F6). Names the GQL fixes and raising the limit for the `gql()` call.
#[cfg(feature = "gql")]
fn gql_materialized_limit_help(max: Option<u64>, observed: Option<u64>) -> String {
    let raised = max.map_or(1_000_000, |max| max.saturating_mul(2).max(1));
    format!(
        "The gql() query built a value larger than 'traversal.max_materialized_bytes' \
         allows{}{}, by the engine's size estimate. A function whose result can be larger than \
         its arguments (range(), split(), replace(), || and CAST(x AS STRING) of a list or map) and \
         the list of collect_list() are checked before they are built, as is the list of a \
         pattern comprehension [(a)-->(b) | b.name], so the query stops instead of running out \
         of memory; the result is never cut short. The list of a COLLECT {{ .. RETURN x }} \
         subquery is checked the same way. Build smaller \
         values: bound range() and replace() by the data, as in \
         g.gql(\"MATCH (p:person) RETURN range(1, p.age / 10) AS r\"), count the values \
         instead of collecting them, as in g.gql(\"MATCH (p:person) RETURN count(p.name) AS \
         n\") or, for the matches of a pattern, g.gql(\"MATCH (p:person) OPTIONAL MATCH \
         (p)-->(f) RETURN p.name, count(f) AS n\") or its openCypher form \
         g.gql(\"MATCH (p:person) RETURN p.name, COUNT {{ (p)-->() }} AS n\"), or raise the \
         limit for the query, \
         g.with(\"traversal.max_materialized_bytes\", {raised}).gql(..), for example \
         g.with(\"traversal.max_materialized_bytes\", {raised}).gql(\"MATCH (p:person) RETURN \
         collect_list(p.name) AS names\"). A host can lock this option with \
         ExecutionPolicy::with_locked(\"traversal.max_materialized_bytes\"). Setting it is an \
         administrative option: a script whose host restricts it needs Update on that Option.",
        max.map(|max| format!(" ({max} bytes)")).unwrap_or_default(),
        observed
            .map(|observed| format!("; this one needed at least {observed} bytes"))
            .unwrap_or_default(),
    )
}

/// A GQL error under construction (feature `gql`): the parser and binder fill it with byte
/// offsets only and turn it into a [`TraverserError::Gql`] with [`Self::into_error`], which
/// computes line, column and width from the query text.
#[cfg(feature = "gql")]
#[derive(Debug, Clone)]
pub(crate) struct GqlErrorBuilder {
    kind: GqlErrorKind,
    span: std::ops::Range<usize>,
    message: String,
    expected: Vec<String>,
    suggestion: Option<String>,
    alternative: Option<String>,
    gqlstatus: Option<&'static str>,
    topic: GqlHelpTopic,
}

#[cfg(feature = "gql")]
impl GqlErrorBuilder {
    /// An error of `kind` for the bytes `span` of the query (an empty span, such as the end of
    /// the input, is underlined with one caret).
    pub(crate) fn new(
        kind: GqlErrorKind,
        span: std::ops::Range<usize>,
        message: impl Into<String>,
    ) -> Self {
        GqlErrorBuilder {
            kind,
            span,
            message: message.into(),
            expected: Vec::new(),
            suggestion: None,
            alternative: None,
            gqlstatus: None,
            topic: GqlHelpTopic::Kind,
        }
    }

    /// The tokens that would have been accepted, as shown to the user (`RETURN`, `)`).
    pub(crate) fn expected<I, S>(mut self, expected: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.expected = expected.into_iter().map(Into::into).collect();
        self
    }

    /// A close spelling for "did you mean ...".
    pub(crate) fn suggestion(mut self, suggestion: impl Into<String>) -> Self {
        self.suggestion = Some(suggestion.into());
        self
    }

    /// The GQL form or Gremlin alternative `help()` leads with (snake_case step names).
    pub(crate) fn alternative(mut self, alternative: impl Into<String>) -> Self {
        self.alternative = Some(alternative.into());
        self
    }

    /// [`alternative`](Self::alternative) unless an earlier stage gave one already.
    pub(crate) fn or_alternative(&mut self, alternative: impl Into<String>) {
        if self.alternative.is_none() {
            self.alternative = Some(alternative.into());
        }
    }

    /// A GQLSTATUS code more specific than the kind's default.
    pub(crate) fn gqlstatus(mut self, code: &'static str) -> Self {
        self.gqlstatus = Some(code);
        self
    }

    /// The general advice `help()` adds after the alternative, in place of the kind's own.
    pub(crate) fn topic(mut self, topic: GqlHelpTopic) -> Self {
        self.topic = topic;
        self
    }

    /// `to` in place of the topic `from` (or of none): an error a shared routine built for one
    /// clause, reported for another (stage 3e: the INSERT pattern checks inside `MERGE`).
    pub(crate) fn retopic(&mut self, from: GqlHelpTopic, to: GqlHelpTopic) {
        if self.topic == from || self.topic == GqlHelpTopic::Kind {
            self.topic = to;
        }
    }

    /// [`topic`](Self::topic) unless an earlier stage chose one already.
    pub(crate) fn or_topic(&mut self, topic: GqlHelpTopic) {
        if self.topic == GqlHelpTopic::Kind {
            self.topic = topic;
        }
    }

    /// The [`TraverserError::Gql`] for `query`. An offset past the end or inside a character
    /// is moved back to the nearest character boundary.
    pub(crate) fn into_error(self, query: &str) -> TraverserError {
        let floor = |mut at: usize| {
            at = at.min(query.len());
            while !query.is_char_boundary(at) {
                at -= 1;
            }
            at
        };
        let start = floor(self.span.start);
        let end = floor(self.span.end.max(start));
        let before = &query[..start];
        let line = before.matches('\n').count() + 1;
        let line_start = before.rfind('\n').map_or(0, |at| at + 1);
        let column = query[line_start..start].chars().count() + 1;
        let width = query[start..end].chars().count().max(1);
        TraverserError::Gql {
            kind: self.kind,
            message: self.message,
            offset: start,
            line,
            column,
            width,
            gqlstatus: self
                .gqlstatus
                .unwrap_or_else(|| self.kind.default_gqlstatus())
                .to_string(),
            context: Box::new(GqlErrorContext {
                query: query.to_string(),
                expected: self.expected,
                suggestion: self.suggestion,
                alternative: self.alternative,
                topic: self.topic,
            }),
        }
    }
}

/// Advice for [`TraverserError::MergeVertexUnresolved`].
fn merge_vertex_unresolved_help(direction: crate::value::Direction, reason: &str) -> String {
    let (key, token) = match direction {
        crate::value::Direction::In => ("Direction.IN", "inV"),
        crate::value::Direction::Out | crate::value::Direction::Both => ("Direction.OUT", "outV"),
    };
    let specific = if reason.contains("could not be resolved") && !reason.contains("option(") {
        format!(
            "Give \"{key}\" the id of an existing vertex (check it with g.v(id)), or create the \
             vertex first, for example g.merge_v(#{{\"T.id\": \"1\"}}). "
        )
    } else if reason.contains("matched") {
        format!(
            "The option(Merge.{token}, map) map is a vertex search that must find exactly one \
             vertex: name it by id, for example option(Merge.{token}, #{{\"T.id\": \"1\"}}), \
             or add properties until only one vertex matches. "
        )
    } else if reason.contains("option(") {
        format!(
            "\"{key}\": Merge.{token} is resolved from option(Merge.{token}, ..), which takes a \
             vertex search map such as #{{\"T.id\": \"1\"}} or a traversal yielding a vertex, \
             for example g.v(\"1\").as(\"a\").v(\"2\").as(\"b\").merge_e(#{{\"T.label\": \
             \"knows\", \"Direction.OUT\": Merge.outV, \"Direction.IN\": Merge.inV}})\
             .option(Merge.outV, __.select(\"a\")).option(Merge.inV, __.select(\"b\")). "
        )
    } else {
        String::new()
    };
    format!(
        "{specific}An edge merge_e() creates needs both vertices: give \"Direction.OUT\" and \
         \"Direction.IN\" an existing vertex id, for example g.merge_e(#{{\"T.label\": \"knows\", \
         \"Direction.OUT\": \"1\", \"Direction.IN\": \"2\"}}), in the merge() map or in \
         option(Merge.onCreate, ..), or use the placeholders Merge.outV/Merge.inV with \
         option(Merge.outV, __.select(\"a\")) / option(Merge.inV, __.select(\"b\"))."
    )
}

/// Advice for `add_e()` without the endpoints it needs.
/// `help()` of `tree("a")` meeting a side effect `a` that is not a tree.
fn tree_key_clash_help(reason: &str) -> String {
    format!(
        "{reason}.\n\
         tree(\"a\") adds every path to the tree stored under \"a\" and passes the traversers on, so \
         the key must be unused or already hold a tree (an earlier tree(\"a\")). aggregate()/store() \
         keep a list, with_side_effect(\"a\", Set.of()) a set and with_side_effect(\"a\", 0, Operator.sum) \
         a reduced value under their key.\n\
         Use another key for the tree, for example g.v().aggregate(\"a\").out().tree(\"t\").cap(\"t\"), \
         or read the existing bucket with cap(\"a\")."
    )
}

/// `help()` of `group("a")`/`groupCount("a")` meeting a side effect `a` it cannot add to.
fn group_key_clash_help(reason: &str) -> String {
    format!(
        "{reason}.\n\
         group(\"a\") and group_count(\"a\") add every traverser to the map stored under \"a\" and \
         pass the traversers on, so the key must be unused or filled only by group steps that \
         reduce a key the same way (two group_count(\"a\"), or group(\"a\") steps with the same kind \
         of value by()). aggregate()/store() keep a list, tree(\"a\") a tree and \
         with_side_effect(\"a\", ...) a seed under their key.\n\
         Use another key for the map, for example \
         g.v().aggregate(\"a\").group_count(\"m\").by(T.label).cap(\"m\"), or read both with \
         cap(\"a\", \"m\")."
    )
}

/// `help()` of `to_graph()` over a stream that is not made of edges.
fn to_graph_help(reason: &str) -> String {
    format!(
        "{reason}.\n\
         to_graph() builds a graph from edges: every edge is copied with both endpoint vertices \
         (ids, labels and properties included). Select edges first, for example \
         g.e(0).to_graph(), g.v(1).both_e().to_graph() or g.v(1).out_e().has_label(\"knows\").to_graph(). \
         subgraph(\"sg\").cap(\"sg\") is the long form of the same result."
    )
}

/// `help()` of `subgraph()` failing while it collects or writes its graph.
fn subgraph_help(reason: &str) -> String {
    format!(
        "{reason}.\n\
         subgraph(\"sg\") collects edges under the key \"sg\" and cap(\"sg\") returns them as a \
         graph, so the stream must be made of edges (select them with out_e()/in_e()/both_e(), for \
         example g.v(1).both_e().subgraph(\"sg\").cap(\"sg\")) and the key must be unused. To write into your own graph, declare it with \
         with_side_effect(\"sg\", target) where target is another graph (for example \
         GraphSource::empty() or an earlier to_graph() result), never the graph you traverse: \
         let target = g.e(0).to_graph(); g.with_side_effect(\"sg\", target).e().subgraph(\"sg\").to_list()."
    )
}

fn add_e_endpoint_help() -> String {
    "After a vertex, add_e(label) creates its edge from that vertex: to(..) names the in-vertex \
     (g.v(\"1\").add_e(\"knows\").to(\"2\")), from(..) the out-vertex \
     (g.v(\"2\").add_e(\"knows\").from(\"1\")), and neither makes a self-loop \
     (g.v(\"1\").add_e(\"self\")). As a start step add_e() has no incoming vertex, so it needs \
     both: g.add_e(\"knows\").from(\"1\").to(\"2\"). from()/to() take a vertex id or a step label \
     set with as(), for example g.v(\"1\").as(\"a\").v(\"2\").add_e(\"knows\").from(\"a\"). To \
     create the edge only when it does not exist yet, use mergeE, for example \
     g.merge_e(#{\"T.label\": \"knows\", \"Direction.OUT\": \"1\", \"Direction.IN\": \"2\"})."
        .to_string()
}

/// Advice for an invalid `property(map)` argument or map key.
fn property_map_help() -> String {
    "property(map) writes one property per map entry, for example \
     g.v(\"1\").property(#{\"name\": \"marko\", \"age\": 29}); null or #{} writes nothing. Keys \
     are plain property names: keys starting with \"T.\", \"Direction.\" or \"~\" are reserved. \
     To set the id or label of a new vertex, use property(T.id, v) / property(T.label, v) right \
     after add_v(), for example g.add_v(\"person\").property(T.id, \"p1\"); a property literally \
     named like a reserved key can still be written one at a time, for example \
     g.v(\"1\").property(\"T.label\", \"x\")."
        .to_string()
}

/// Advice for [`TraverserError::UnsupportedCardinality`].
fn unsupported_cardinality_help(step: &str, on_edge: bool) -> String {
    if on_edge && step == "merge_e" {
        "Edge properties have no cardinality: drop the Cardinality argument of option() and any \
         Cardinality.single(v) wrapper in the merge_e() maps, for example \
         g.merge_e(#{\"T.label\": \"knows\", \"Direction.OUT\": \"1\", \"Direction.IN\": \
         \"2\"}).option(Merge.onMatch, #{\"weight\": 0.5})."
            .to_string()
    } else if on_edge {
        "Edge properties have no cardinality: drop the Cardinality argument (and any \
         Cardinality.single(v) wrapper in a map), for example g.e(\"0\").property(\"weight\", 0.5) \
         or g.e(\"0\").property(#{\"weight\": 0.5})."
            .to_string()
    } else {
        "Graphersal stores one value per property key, so only Cardinality.single can be \
         written: it replaces the current value, for example \
         g.v(\"1\").property(Cardinality.single, \"age\", 30) (the same as \
         g.v(\"1\").property(\"age\", 30)). To keep several values, store them as one array \
         value: g.v(\"1\").property(\"tags\", [\"a\", \"b\"])."
            .to_string()
    }
}

/// Advice for [`TraverserError::TokenPropertyOutsideAdd`].
fn token_property_outside_add_help(token: &str) -> String {
    if token == "T.label" {
        "property(T.label, v) sets the label of the vertex add_v() is creating; write it right \
         after add_v() (other property() calls may come in between), for example \
         g.add_v().property(T.label, \"person\"), or pass the label to add_v(\"person\") directly. \
         An edge's label comes from add_e(label), for example g.v(\"1\").add_e(\"knows\").to(\"2\"). \
         The label of an existing element cannot be changed with property(); to store data \
         under the key \"label\", write a normal property: g.v(\"1\").property(\"label\", \"x\")."
            .to_string()
    } else {
        "property(T.id, v) sets the id of the element add_v()/add_e() is creating; write it right \
         after add_v()/add_e() (other property() calls may come in between), for example \
         g.add_v(\"person\").property(T.id, \"p1\"). The id of an existing element cannot be \
         changed; to store data on it, write a normal property, for example \
         g.v(\"1\").property(\"code\", 5)."
            .to_string()
    }
}

impl TraverserError {
    /// The steps before the failing step of the innermost failing level, as written, prefixed
    /// with `g.` for the outermost level and `__.` for a child traversal.
    fn failing_level_prefix(&self) -> Option<String> {
        let frames = self.step_trace();
        let frame = frames.last()?;
        let before: Vec<String> = match &frame.logical {
            Some(logical) => {
                let first = logical.positions.first().copied().unwrap_or(0);
                logical.steps.iter().take(first).cloned().collect()
            }
            None => frame.steps.iter().take(frame.position).cloned().collect(),
        };
        let start = if frames.len() <= 1 { "g." } else { "__." };
        Some(if before.is_empty() {
            start.to_string()
        } else {
            format!("{start}{}", before.join("."))
        })
    }

    /// Rendered steps of the outermost failing level: as the user wrote it when the optimizer
    /// rewrote it, otherwise as executed.
    /// The rendered name of the innermost failing step, the one the root cause came from.
    #[cfg(feature = "gql")]
    fn innermost_failed_step(&self) -> Option<&str> {
        let mut name = None;
        let mut current = self;
        while let TraverserError::StepFailed {
            step_name, source, ..
        } = current
        {
            name = Some(step_name.as_str());
            current = source;
        }
        name
    }

    /// When the innermost failure is a step of the create child of a GQL `MERGE` (stage 3e),
    /// the failing step right outside the innermost one being `gql:merge("<pattern>", ..)`: the
    /// pattern as written (`""` when its text cannot be read back).
    #[cfg(feature = "gql")]
    fn gql_merge_pattern(&self) -> Option<String> {
        let mut names: [Option<&str>; 2] = [None, None];
        let mut current = self;
        while let TraverserError::StepFailed {
            step_name, source, ..
        } = current
        {
            names = [names[1], Some(step_name.as_str())];
            current = source;
        }
        let outer = names[0]?.strip_prefix("gql:merge(")?;
        Some(rhai_string_prefix(outer).unwrap_or_default())
    }

    /// Where the innermost failure ran (feature `gql`): walking the failing levels from the
    /// innermost outwards, the first level that is the child of a GQL construct decides. The
    /// child of a `filter()` / `not()` step (the `[NOT] EXISTS` fast path) or of a `gql:*` step
    /// (a hosted subquery, the only children those steps hold) is a subquery, classified by
    /// its own steps: ending in `gql:value` a pattern comprehension (only its lowering and that
    /// of `COLLECT { }` emit that step, always last; a `COLLECT { }` body has its `RETURN`,
    /// `gql:project` / `gql:aggregate`, or a `union()` of operands on that level), ending in
    /// `count()` a `COUNT { }` subquery (only its lowering closes a child with it, possibly
    /// fused into the step before), else an `EXISTS` body or a pattern predicate. The child of
    /// `flat_map()` is a `CALL { }` body (the only GQL construct lowered to it). Other levels
    /// (`union()` operands, `optional()`, `repeat()` bodies, edge forms) are transparent.
    #[cfg(feature = "gql")]
    fn gql_subquery_place(&self) -> GqlSubqueryPlace {
        let mut levels: Vec<(&str, &[String])> = Vec::new();
        let mut current = self;
        while let TraverserError::StepFailed {
            step_name,
            frame,
            source,
            ..
        } = current
        {
            let steps = frame.as_deref().map_or(&[][..], |f| f.steps.as_slice());
            levels.push((step_name.as_str(), steps));
            current = source;
        }
        for pair in levels.windows(2).rev() {
            let [(parent, _), (_, level)] = pair else {
                continue;
            };
            if parent.starts_with("flat_map(") {
                return GqlSubqueryPlace::Call;
            }
            if parent.starts_with("filter(")
                || parent.starts_with("not(")
                || parent.starts_with("gql:")
            {
                return Self::gql_subquery_kind(level);
            }
        }
        GqlSubqueryPlace::Outside
    }

    /// The kind of the subquery whose child runs the steps `level` (see
    /// [`Self::gql_subquery_place`]).
    #[cfg(feature = "gql")]
    fn gql_subquery_kind(level: &[String]) -> GqlSubqueryPlace {
        match level.last() {
            Some(step) if step.starts_with("gql:value(") => {
                let returns = level.iter().any(|step| {
                    step.starts_with("gql:project(")
                        || step.starts_with("gql:aggregate(")
                        || step.starts_with("union(")
                });
                if returns {
                    GqlSubqueryPlace::Collect
                } else {
                    GqlSubqueryPlace::Comprehension
                }
            }
            // The optimizer may fuse the closing `count()` into the step before it
            // (`count_pushdown`: `v(labels: [..]).count()`).
            Some(step) if step == "count()" || step.ends_with(".count()") => {
                GqlSubqueryPlace::Count
            }
            _ => GqlSubqueryPlace::Exists,
        }
    }

    /// The steps of the failing level when it runs inside a GQL subquery or `CALL` body (see
    /// [`Self::gql_subquery_place`]).
    #[cfg(feature = "gql")]
    fn gql_subquery_steps(&self) -> Option<&[String]> {
        if self.gql_subquery_place() == GqlSubqueryPlace::Outside {
            return None;
        }
        self.step_trace().last().map(|frame| frame.steps.as_slice())
    }

    fn outermost_steps(&self) -> Option<Vec<String>> {
        let frame = self.step_trace().into_iter().next()?;
        Some(match &frame.logical {
            Some(logical) => logical.steps.clone(),
            None => frame.steps.clone(),
        })
    }

    /// Returns the outermost failing level as the user wrote it, before optimization.
    ///
    /// `None` when provenance was not tracked, or when the optimizer left the query unchanged
    /// and the executed plan already is the user's query.
    pub fn query(&self) -> Option<String> {
        match self {
            TraverserError::StepFailed { frame, .. } => frame
                .as_ref()
                .and_then(|frame| frame.logical.as_ref())
                .map(|logical| logical.steps.join(".")),
            _ => None,
        }
    }

    /// Collects the physical plan frames of nested step failures, outermost level first.
    pub fn step_trace(&self) -> Vec<&StepFrame> {
        let mut frames = Vec::new();
        let mut current = self;
        while let TraverserError::StepFailed { frame, source, .. } = current {
            if let Some(frame) = frame {
                frames.push(&**frame);
            }
            current = source;
        }
        frames
    }

    /// Returns the innermost error of a nested step failure chain (also through the
    /// [`TraverserError::Spelled`] wrapper).
    pub fn root_cause(&self) -> &TraverserError {
        let mut current = self;
        while let TraverserError::StepFailed { source, .. }
        | TraverserError::Spelled { source, .. } = current
        {
            current = source;
        }
        current
    }

    fn root_cause_mut(&mut self) -> &mut TraverserError {
        match self {
            TraverserError::StepFailed { source, .. } | TraverserError::Spelled { source, .. } => {
                source.root_cause_mut()
            }
            other => other,
        }
    }

    /// Where in the optimized plan the error happened: the id of the failing step (the same
    /// numbering as [`Metrics::id`](crate::Metrics::id), `<step>.<child>.<step>...`) and the
    /// chain of steps from the top level down to it. A plan-time error (a rejected modulator, an
    /// invalid constant argument, an undeclared step label) and a denied step are located at
    /// the step they are about, before anything runs. `None` for an error that has no step (an
    /// invalid or locked option, a script error).
    ///
    /// Ids are static positions in the plan `.profile()` shows (fused steps under their real
    /// names), so the location joins a partial profile of an [`Execution`](crate::Execution)
    /// by id.
    pub fn step_location(&self) -> Option<StepLocation> {
        let mut segments: Vec<String> = Vec::new();
        let mut step_path = Vec::new();
        let mut current = self;
        while let TraverserError::StepFailed {
            step_name,
            step_index,
            child_index,
            source,
            ..
        } = current
        {
            if let Some(index) = step_index {
                if let Some(child) = child_index
                    && !segments.is_empty()
                {
                    segments.push(child.to_string());
                }
                segments.push(index.to_string());
                step_path.push(StepPathEntry {
                    id: segments.join("."),
                    name: step_name.clone(),
                });
            }
            current = source;
        }
        let last = step_path.last()?;
        Some(StepLocation {
            step_id: last.id.clone(),
            step_path,
        })
    }
}

/// The location of a failing step in the optimized plan; see
/// [`TraverserError::step_location`].
///
/// `#[non_exhaustive]`: a source span of the written query is a planned, additive field.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct StepLocation {
    /// Id of the failing step, numbered like [`Metrics::id`](crate::Metrics::id) (`"3.1.0"`).
    pub step_id: String,
    /// The steps from the top level down to the failing one (the last entry).
    pub step_path: Vec<StepPathEntry>,
}

/// One step of a [`StepLocation::step_path`]. `#[non_exhaustive]`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct StepPathEntry {
    /// The step's id (`"3"`, `"3.1.0"`).
    pub id: String,
    /// The step's name in the optimized plan, the same text as [`Metrics::name`](crate::Metrics::name).
    pub name: String,
}

impl TraverserError {
    pub(crate) fn cast_exception<S: GraphStorage>(
        expected: &[&'static str],
        value: &TraverserValue<S>,
    ) -> TraverserError {
        TraverserError::Value(ValueError::CastException {
            expected: expected.join(" or "),
            actual: value.type_name().to_string(),
            value: None,
            origin: None,
        })
    }

    pub(crate) fn cast_exception_str(expected: &[&'static str], actual: String) -> TraverserError {
        TraverserError::Value(ValueError::CastException {
            expected: expected.join(" or "),
            actual,
            value: None,
            origin: None,
        })
    }

    /// Builds a cast error carrying the offending value and the graph element it came from.
    pub(crate) fn cast_exception_at(
        expected: &[&'static str],
        actual: String,
        value: Option<String>,
        origin: Option<ValueOrigin>,
    ) -> TraverserError {
        TraverserError::Value(ValueError::CastException {
            expected: expected.join(" or "),
            actual,
            value,
            origin: origin.map(Box::new),
        })
    }

    /// Builds a cast error whose expected type is computed rather than a fixed literal.
    pub(crate) fn cast_exception_target(
        expected: impl Into<String>,
        actual: String,
        value: Option<String>,
        origin: Option<ValueOrigin>,
    ) -> TraverserError {
        TraverserError::Value(ValueError::CastException {
            expected: expected.into(),
            actual,
            value,
            origin: origin.map(Box::new),
        })
    }

    /// Builds the error of a label step (`add_label`, `drop_label`, `set_label`) that met a
    /// value of the wrong kind.
    pub(crate) fn label_step_element_kind<S: GraphStorage>(
        step: &'static str,
        value: &TraverserValue<S>,
    ) -> TraverserError {
        TraverserError::Value(ValueError::LabelStepElementKind {
            step,
            expected: if step == "set_label" {
                "an edge"
            } else {
                "a vertex"
            },
            actual: value.kind_name(),
        })
    }

    /// Builds the error of a `Scope.local` step that met a list element it cannot take.
    pub(crate) fn local_element_type(
        step: &'static str,
        accepts: &'static str,
        index: usize,
        actual: String,
    ) -> TraverserError {
        TraverserError::Value(ValueError::LocalElementType {
            step,
            accepts,
            index,
            actual,
        })
    }

    /// Builds the error for a conversion target the engine cannot produce yet.
    pub(crate) fn unsupported_cast_target(
        target: impl Into<String>,
        reason: &'static str,
    ) -> TraverserError {
        TraverserError::Value(ValueError::UnsupportedCastTarget {
            target: target.into(),
            reason,
        })
    }

    #[allow(dead_code)]
    pub(crate) fn property_not_found(key: impl Into<String>) -> TraverserError {
        TraverserError::Value(ValueError::PropertyNotFound {
            key: key.into(),
            element_type: "element",
        })
    }

    pub(crate) fn property_not_found_on_element(
        key: impl Into<String>,
        element_type: &'static str,
    ) -> TraverserError {
        TraverserError::Value(ValueError::PropertyNotFound {
            key: key.into(),
            element_type,
        })
    }

    #[allow(dead_code)]
    pub(crate) fn wrong_query(reason: impl Into<String>) -> TraverserError {
        TraverserError::QueryExecution {
            query: "<query>".to_string(),
            reason: reason.into(),
        }
    }

    pub(crate) fn not_implemented(feature: &'static str) -> TraverserError {
        TraverserError::NotImplemented { feature }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The denials of a saved query that writes which the inference prevents (it refuses such
    /// bodies when they are saved) explain the rule, not the host's authorizer.
    #[test]
    fn saved_query_effect_denials_carry_their_help() {
        use crate::graph::{AccessRequest, Action, Resource};
        for (reason, expected) in [
            (
                format!("{SAVED_QUERY_DATA_ONLY}: \"q\" cannot create, change or delete .."),
                "A saved query that writes changes data only",
            ),
            (
                format!("{SAVED_QUERY_OUTSIDE_EFFECTS} (Create Data), and this write .."),
                "this write was not inferred, which is a bug of Graphersal",
            ),
        ] {
            let err = TraverserError::PermissionDenied {
                step: "drop".to_string(),
                request: Box::new(
                    AccessRequest::new(Action::Delete, Resource::data(None, None))
                        .to_owned_request(),
                ),
                reason,
            };
            let help = err.help().expect("help");
            assert!(help.contains(expected), "{help}");
        }
    }

    /// The embedding errors a query cannot trigger with the built-in embedder (they come from a
    /// host's embedder or its registration) carry their help through `TraverserError::Embed`.
    #[test]
    fn embed_error_help_for_host_embedders() {
        use crate::value::EmbedError;
        for (error, expected) in [
            (
                EmbedError::bad_output("2 vectors for 3 texts"),
                "broke its contract",
            ),
            (
                EmbedError::unavailable("the model file is missing"),
                "Retry later",
            ),
            (EmbedError::failed("CUDA out of memory"), "its message"),
            (
                EmbedError::Cancelled,
                "g.with(\"evaluationTimeout\", 60000)",
            ),
            (EmbedError::registry("a duplicate"), "registered once"),
        ] {
            let err = TraverserError::Embed(error);
            let help = err.help().expect("help");
            assert!(help.contains(expected), "{help}");
            assert!(format_diagnostic(&err).contains("Embedding failed: "));
        }
    }

    /// The `path` help (a label that is not set on the traverser's path) names the `Pop` forms; a
    /// named-field variant, so built by hand here.
    #[test]
    fn path_label_help_names_the_pop_forms() {
        let help = ValueError::PropertyNotFound {
            key: "x".to_string(),
            element_type: "path",
        }
        .help()
        .expect("help");
        assert!(help.contains("select(Pop.all, \"x\")"), "{help}");
        assert!(
            help.contains("Pop.first") && help.contains("Pop.mixed"),
            "{help}"
        );
    }

    /// `OperatorFailed` is a named-field variant, so it is built by hand here; the queries that
    /// reach it (`sack(Operator)`) are asserted in `tests/all/error_diagnostics_tests.rs`.
    #[test]
    fn operator_failed_message_and_help() {
        let incompatible = ValueError::OperatorFailed {
            operator: "sum",
            kind: OperatorFailure::IncompatibleOperands { expected: "number" },
            left: "3 (int64)".into(),
            right: "\"a\" (string)".into(),
        };
        assert_eq!(
            incompatible.to_string(),
            "Operator 'sum' cannot combine 3 (int64) with \"a\" (string): expected number"
        );
        let help = incompatible.help().expect("help");
        for needle in [
            "as_number()",
            "as_bool()",
            "min and max",
            "sumLong takes int64",
            "addAll",
        ] {
            assert!(help.contains(needle), "{needle} missing from: {help}");
        }
        let overflow = ValueError::OperatorFailed {
            operator: "sumLong",
            kind: OperatorFailure::Overflow,
            left: "9223372036854775807 (int64)".into(),
            right: "1 (int64)".into(),
        };
        assert!(overflow.to_string().contains("overflowed"));
        assert!(
            overflow
                .help()
                .expect("help")
                .contains("does not widen or wrap")
        );
        let zero = ValueError::OperatorFailed {
            operator: "div",
            kind: OperatorFailure::DivisionByZero,
            left: "1 (int64)".into(),
            right: "0 (int64)".into(),
        };
        assert!(zero.to_string().contains("division by zero"));
    }
    /// `SackArenaExhausted` needs 2^32 distinct sack values, so it is built by hand.
    #[test]
    fn sack_arena_exhausted_help_names_the_update_step() {
        let help = TraverserError::SackArenaExhausted { capacity: 1 }
            .help()
            .expect("help");
        assert!(
            help.contains(
                "g.with_sack(0).v().limit(1000).out().sack(Operator.sum).by(__.constant(1)).sack()"
            ),
            "{help}"
        );
    }

    use std::error::Error;

    /// Every public error enum stays within `clippy::result_large_err`'s 128-byte threshold, so
    /// a `Result<T, E>` carrying one stays cheap to move on the success path. Rarely-read
    /// diagnostic payloads (`ValueOrigin`, `StepFrame`) are boxed to keep it that way; a new
    /// variant or field that crosses the bound should box its payload the same way.
    #[test]
    fn test_error_enums_stay_small() {
        use std::mem::size_of;
        const LIMIT: usize = 128;
        let sizes = [
            ("ValueError", size_of::<ValueError>()),
            ("GraphError", size_of::<GraphError>()),
            ("IOError", size_of::<IOError>()),
            ("ScriptError", size_of::<ScriptError>()),
            ("SchemaError", size_of::<SchemaError>()),
            ("TraverserError", size_of::<TraverserError>()),
        ];
        for (name, size) in sizes {
            assert!(
                size <= LIMIT,
                "{name} is {size} bytes, above the {LIMIT}-byte bound"
            );
        }
    }

    #[test]
    fn test_value_error_display_and_help() {
        let err = ValueError::CastException {
            expected: "string".to_string(),
            actual: "number".to_string(),
            value: None,
            origin: None,
        };
        assert_eq!(
            err.to_string(),
            "Cast exception: expected type [string], got type 'number'"
        );
        assert!(err.help().is_some());
        assert!(err.help().unwrap().contains("Check the value type"));

        let err_prop = ValueError::PropertyNotFound {
            key: "age".to_string(),
            element_type: "vertex",
        };
        assert_eq!(
            err_prop.to_string(),
            "Property 'age' not found on element type 'vertex'"
        );
        assert!(err_prop.help().unwrap().contains(".has() filter"));
    }

    #[test]
    fn test_graph_error_display_and_help() {
        let err = GraphError::VertexNotFound {
            id: "v1".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "Vertex with ID 'v1' was not found in the graph"
        );
        assert!(
            err.help()
                .unwrap()
                .contains("Verify that the vertex ID exists")
        );
    }

    #[test]
    fn test_io_error_source_chain_and_help() {
        let io_source = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err = IOError::FileRead {
            path: "/tmp/test.graphml".to_string(),
            source: io_source,
        };
        assert!(
            err.to_string()
                .contains("Failed to read file '/tmp/test.graphml'")
        );
        assert!(err.source().is_some());
        assert!(
            err.help()
                .unwrap()
                .contains("Check if the file path is correct")
        );
    }

    #[test]
    fn test_schema_error_display_and_help() {
        let err = SchemaError::UnknownVertexLabel {
            label: "User".to_string(),
            allowed: vec!["Person".to_string(), "Software".to_string()],
        };
        assert_eq!(
            err.to_string(),
            "Unknown vertex label 'User' is not declared in the closed schema"
        );
        let help = err.help().unwrap();
        assert!(help.contains("[Person, Software]"), "{help}");
        assert!(help.contains("patch_schema"), "{help}");

        let err_edge = SchemaError::UnknownEdgeLabel {
            label: "manages".to_string(),
            allowed: vec!["knows".to_string(), "created".to_string()],
        };
        assert_eq!(
            err_edge.to_string(),
            "Unknown edge label 'manages' is not declared in the closed schema"
        );
        assert!(err_edge.help().unwrap().contains("[knows, created]"));

        let err_topo = SchemaError::InvalidEdgeTopology {
            source_label: "Person".to_string(),
            edge_label: "created".to_string(),
            target_label: "Person".to_string(),
            allowed: vec!["(:Person)-[:created]->(:Software)".to_string()],
        };
        assert_eq!(
            err_topo.to_string(),
            "Disallowed edge connection: '(:Person)-[:created]->(:Person)' is not defined in schema"
        );
        let help = err_topo.help().unwrap();
        assert!(
            help.contains("[(:Person)-[:created]->(:Software)]"),
            "{help}"
        );
        assert!(help.contains("connections"), "{help}");

        let err_prop = SchemaError::UndeclaredProperty {
            element_type: "vertex",
            label: "Person".to_string(),
            property_name: "nickname".to_string(),
        };
        assert_eq!(
            err_prop.to_string(),
            "Undeclared property 'nickname' on vertex 'Person' is not declared and not admitted by the schema"
        );
        let help = err_prop.help().unwrap();
        assert!(help.contains("\"additionalProperties\": true"), "{help}");
        assert!(!help.contains("Partial"), "{help}");

        let err_req = SchemaError::MissingRequiredProperty {
            element_type: "vertex",
            label: "Person".to_string(),
            property_name: "name".to_string(),
        };
        assert_eq!(
            err_req.to_string(),
            "Missing required property 'name' on vertex 'Person'"
        );
        let req_help = err_req.help().unwrap();
        assert!(
            req_help.starts_with(
                "Provide a value for 'name' or remove it from the label's \"required\""
            )
        );
        assert!(
            req_help.contains(
                "or give it a \"default\" (or \"defaultFunction\") in the schema: it is stored \
                 when an element is created without the property (mode open or closed), but never \
                 filled into existing elements, and never by an import or a property removal."
            ),
            "{req_help}"
        );
        assert!(req_help.contains("add_property_fold"), "{req_help}");

        let err_mismatch = SchemaError::TypeMismatch {
            element_type: "vertex",
            label: "Person".to_string(),
            property_name: "age".to_string(),
            expected_type: "int64".to_string(),
            actual_type: "string".to_string(),
        };
        assert_eq!(
            err_mismatch.to_string(),
            "Type mismatch on vertex 'Person.age': expected int64, got string"
        );
        assert_eq!(
            err_mismatch.help(),
            Some(
                "Convert value to expected type 'int64' before inserting.\n\
                 In a query, convert the values with values(\"age\").as_number(GType.LONG).\n\
                 Find every offending value with values(\"age\").is(typeOf(GType.STRING))."
                    .to_string()
            )
        );

        let err_null = SchemaError::TypeMismatch {
            element_type: "vertex",
            label: "Person".to_string(),
            property_name: "age".to_string(),
            expected_type: "int64".to_string(),
            actual_type: "null".to_string(),
        };
        assert!(
            err_null
                .help()
                .unwrap()
                .starts_with("'age' does not admit null")
        );

        let err_constraint = SchemaError::ConstraintViolation {
            label: "Person".to_string(),
            property_name: "age".to_string(),
            constraint_rule: "value >= 0".to_string(),
            actual_value: "-5".to_string(),
        };
        assert_eq!(
            err_constraint.to_string(),
            "Constraint violation on 'Person.age': value >= 0 (actual: -5)"
        );
        assert_eq!(
            err_constraint.help(),
            Some("Ensure value complies with validation constraint: value >= 0.".to_string())
        );

        let err_def = SchemaError::InvalidSchema {
            path: "/vertices/person/schema/properties/age".to_string(),
            reason: "unknown keyword 'minimun'".to_string(),
            hint: "The supported keywords are: ...".to_string(),
        };
        assert_eq!(
            err_def.to_string(),
            "Invalid schema at /vertices/person/schema/properties/age: unknown keyword 'minimun'"
        );
        let help = err_def.help().unwrap();
        assert!(
            help.starts_with("The supported keywords are: ..."),
            "{help}"
        );
        assert!(help.contains("JSON Schema 2020-12"), "{help}");
    }
    #[test]
    fn test_script_error_display_and_help() {
        let err = ScriptError::UnsupportedStep {
            step_name: "unknown_step".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "Unsupported step 'unknown_step' in script context"
        );
        assert!(err.help().unwrap().contains("Verify step name"));
    }

    #[test]
    fn test_traverser_error_propagation_and_cause_chain() {
        let val_err = ValueError::PathNotDefined;
        let trav_err: TraverserError = val_err.into();

        assert_eq!(
            trav_err.to_string(),
            "Path is not defined or initialized for traverser"
        );
        assert_eq!(trav_err.help(), ValueError::PathNotDefined.help());
        assert!(
            trav_err
                .help()
                .unwrap_or_default()
                .contains("g.v(\"1\").as(\"a\").out().select(\"a\")")
        );

        let step_failed = TraverserError::StepFailed {
            spelling: crate::traversal::Spelling::Snake,
            step_name: "out".to_string(),
            step_index: None,
            child_index: None,
            frame: None,
            element_id: Some("v1".to_string()),
            source: Box::new(trav_err),
        };

        assert!(
            step_failed
                .to_string()
                .contains("Step 'out' execution failed")
        );
        assert!(step_failed.source().is_some());
        assert_eq!(step_failed.help(), ValueError::PathNotDefined.help());
    }

    #[test]
    fn test_step_frame_marks_every_originating_step() {
        // `v().has_label("person").count()` executes as a single fused scan, so the failure
        // traces back to all three steps the user wrote.
        let frame = StepFrame::new(vec!["v(labels: \"person\", count_only)".to_string()], 0)
            .with_logical(LogicalFrame {
                steps: vec![
                    "v()".to_string(),
                    "has_label(\"person\")".to_string(),
                    "count()".to_string(),
                ],
                positions: vec![0, 1, 2],
            });

        let rendered = frame.render_logical().expect("the plans differ");
        let (plan, caret) = rendered.split_once('\n').expect("a caret line is rendered");
        assert_eq!(plan, "v().has_label(\"person\").count()");
        assert_eq!(
            caret,
            "^".repeat(plan.chars().count()),
            "a fully folded level is underlined as one run"
        );

        let partial =
            StepFrame::new(vec!["a".to_string(), "b".to_string()], 1).with_logical(LogicalFrame {
                steps: vec!["a()".to_string(), "b()".to_string(), "c()".to_string()],
                positions: vec![2],
            });
        assert_eq!(
            partial.render_logical().expect("the plans differ"),
            "a().b().c()\n        ^^^"
        );
    }

    #[test]
    fn test_step_frame_hides_an_unchanged_plan() {
        let frame = StepFrame::new(vec!["v()".to_string(), "max()".to_string()], 1).with_logical(
            LogicalFrame {
                steps: vec!["v()".to_string(), "max()".to_string()],
                positions: vec![1],
            },
        );
        assert_eq!(
            frame.render_logical(),
            None,
            "an untouched level must not be printed twice"
        );
    }

    #[test]
    fn test_result_ext_trait() {
        let res: Result<(), ValueError> = Err(ValueError::PathNotDefined);
        let step_res = res.with_step("values");
        assert!(step_res.is_err());
        if let Err(boxed) = step_res {
            if let TraverserError::StepFailed {
                step_name,
                element_id,
                source,
                ..
            } = *boxed
            {
                assert_eq!(step_name, "values");
                assert_eq!(element_id, None);
                assert!(source.to_string().contains("Path is not defined"));
            } else {
                panic!("Expected StepFailed error variant");
            }
        } else {
            panic!("Expected StepFailed error variant");
        }

        let res_elem: Result<(), ValueError> = Err(ValueError::PathNotDefined);
        let step_elem_res = res_elem.with_step_element("out", "v1");
        assert!(step_elem_res.is_err());
        if let Err(boxed) = step_elem_res {
            if let TraverserError::StepFailed {
                step_name,
                element_id,
                source,
                ..
            } = *boxed
            {
                assert_eq!(step_name, "out");
                assert_eq!(element_id, Some("v1".to_string()));
                assert!(source.to_string().contains("Path is not defined"));
            } else {
                panic!("Expected StepFailed error variant");
            }
        } else {
            panic!("Expected StepFailed error variant");
        }
    }

    /// D5/D6 regression: `TraverserResult<T>`'s `Err` side must stay a single boxed pointer.
    ///
    /// `TraverserError` itself is large (its fattest variants wrap `SchemaError`, up to 128 bytes
    /// on a 64-bit target), so an unboxed `Result<T, TraverserError>` costs at least that much to
    /// move on every call, even along the `Ok` path. Boxing the error keeps that cost to one
    /// pointer. The bound is `<=`, not `==`, so it doesn't need updating for unrelated,
    /// pointer-width-independent changes; it only fails if someone defeats the boxing (e.g. by
    /// adding a fat field directly to a hot-path `Ok` type, or by accidentally un-boxing the
    /// error again).
    #[test]
    fn traverser_result_err_side_stays_a_single_pointer() {
        assert!(
            std::mem::size_of::<TraverserError>() > 24,
            "this test assumes TraverserError is large enough that boxing it is worthwhile; \
             got {} bytes",
            std::mem::size_of::<TraverserError>()
        );
        assert!(
            std::mem::size_of::<TraverserResult<()>>() <= 24,
            "TraverserResult<()> grew to {} bytes; Box<TraverserError> should keep it small \
             regardless of TraverserError's own size",
            std::mem::size_of::<TraverserResult<()>>()
        );
    }

    // Help-wording tests that need a hand-built error value. Named-field error variants are
    // `#[non_exhaustive]`, so only code inside this crate can construct them; the integration
    // tests in `tests/all/error_diagnostics_tests.rs` drive errors through real queries instead.
    mod help_wording {
        use super::super::*;
        use crate::prelude::*;

        /// An incompatible operand pair is repaired by converting one side, so the help says so.
        #[test]
        fn an_invalid_type_operation_points_at_the_coercion_steps() {
            let err = ValueError::InvalidTypeOperation {
                op: "+",
                type_a: "string",
                type_b: "int64",
            };
            let report = format_diagnostic(&err);
            assert!(
                report.contains(
                    "Ensure operand types are compatible for operation, or convert one side first with .as_string(), .as_bool() or .as_number(GType.LONG)."
                ),
                "the help must name the coercion steps in: {report}"
            );
        }

        /// Compiles `pattern`, which must fail, and returns the rendered diagnostic.
        fn glob_report(pattern: &str) -> String {
            match crate::value::glob_path::GlobPattern::compile(pattern) {
                Err(err) => format_diagnostic(&TraverserError::from(err)),
                Ok(_) => panic!("'{pattern}' should not compile"),
            }
        }

        #[test]
        fn invalid_glob_pattern_display_names_pattern_position_and_reason() {
            let err = ValueError::InvalidGlobPattern {
                pattern: "a**b".to_string(),
                position: 1,
                reason: crate::value::glob_path::reason::DOUBLE_STAR_IN_SEGMENT.to_string(),
            };
            assert_eq!(
                err.to_string(),
                "Invalid glob pattern 'a**b' at position 1: '**' must be a whole segment"
            );
            let help = err.help().unwrap_or_default();
            assert!(
                help.starts_with(
                    "'**' spans whole levels, so it must stand alone between slashes. Write \
                     glob_path(\"a*/**/*b\") instead, or plain glob_path(\"**\") to match every \
                     level below the start vertex. A literal '*' is written '\\*'."
                ),
                "unexpected help: {help}"
            );
            assert!(help.contains("for example glob_path(\"**/src/*.{rs,toml}\")."));
        }

        #[test]
        fn invalid_glob_pattern_help_rewrites_misplaced_double_stars() {
            assert!(glob_report("a**").contains("Write glob_path(\"a*/**\") instead"));
            assert!(glob_report("**b").contains("Write glob_path(\"**/*b\") instead"));
            assert!(
                glob_report("src/a**b/c").contains("Write glob_path(\"src/a*/**/*b/c\") instead")
            );
        }

        #[test]
        fn invalid_glob_pattern_help_splits_a_slash_in_alternatives() {
            let report = glob_report("{src/a,lib/b}");
            assert!(
                report.contains("at position 4: alternatives '{..}' cannot contain '/'"),
                "{report}"
            );
            assert!(
                report.contains(
                    "Split the alternative into two queries, for example glob_path(\"src/a\") and \
                     glob_path(\"lib/b\") instead of glob_path(\"{src/a,lib/b}\"), or use '**' to \
                     span levels, for example glob_path(\"**/{a,b}\")."
                ),
                "{report}"
            );
        }

        #[test]
        fn invalid_glob_pattern_help_explains_reserved_dot_segments() {
            let report = glob_report("docs/..");
            assert!(
                report.contains("'.' and '..' are reserved as whole segments"),
                "{report}"
            );
            assert!(
                report.contains(
                    "To walk back up use .in() or repeat(__.in()) before the step, for example \
                     g.v(\"x\").in().glob_path(\"docs/*\")."
                ),
                "{report}"
            );
        }

        #[test]
        fn invalid_glob_pattern_help_splits_too_many_segments() {
            let report = glob_report(&vec!["a"; 64].join("/"));
            assert!(
                report.contains("at position 126: the pattern has more than 63 segments"),
                "{report}"
            );
            assert!(
                report.contains(
                    "Split the walk into two consecutive steps, each matching below the vertices \
                     the previous one emits, for example glob_path(\"a/b/c\").glob_path(\"d/e/f\")"
                ),
                "{report}"
            );
        }

        #[test]
        fn invalid_glob_pattern_help_closes_classes_and_alternatives() {
            let class = glob_report("file[0-9.txt");
            assert!(
                class.contains("at position 4: unclosed character class '['"),
                "{class}"
            );
            assert!(
                class.contains(
                    "Close the character class with ']' in the same segment, for example \
                     glob_path(\"file[0-9].txt\"); match a literal '[' with '\\['."
                ),
                "{class}"
            );
            let alternatives = glob_report("*.{rs,toml");
            assert!(
                alternatives.contains("at position 2: unclosed alternatives '{'"),
                "{alternatives}"
            );
            assert!(
                alternatives.contains(
                    "Close the alternatives with '}', for example glob_path(\"*.{rs,toml}\"); \
                     match a literal '{' with '\\{'."
                ),
                "{alternatives}"
            );
        }

        #[test]
        fn invalid_script_parameter_help_names_both_binding_forms() {
            let err = ScriptError::InvalidParameter {
                parameter: "g".to_string(),
                message: "'g' is reserved by the script scope for the Gremlin DSL".to_string(),
            };
            let rendered = format_diagnostic(&err);

            assert!(
                rendered.contains("Invalid script parameter 'g'"),
                "the message must name the offending parameter:\n{rendered}"
            );
            assert!(
                rendered.contains("`Order`") && rendered.contains("token class"),
                "the help must describe the token class globals:\n{rendered}"
            );
            assert!(
                rendered.contains("eval_with_params"),
                "the help must name the named-parameter entry point:\n{rendered}"
            );
            assert!(
                rendered.contains("eval_with_positional_params"),
                "the help must name the positional entry point:\n{rendered}"
            );
            assert!(
                rendered.contains("g.v().has_label(label).to_list()"),
                "the help must carry a runnable named-parameter example:\n{rendered}"
            );
            assert!(
                rendered.contains("g.v().has_label($1).to_list()"),
                "the help must carry a runnable positional example:\n{rendered}"
            );
            assert!(
                rendered.contains("1-based"),
                "the help must state that placeholders start at $1:\n{rendered}"
            );
        }

        #[test]
        fn mark_unavailable_help_covers_a_storage_without_a_journal() {
            let err = GraphError::mark_unavailable("this storage keeps no journal");
            let rendered = format_diagnostic(&err);
            assert!(
                rendered.contains("Cannot set a mark: this storage keeps no journal"),
                "{rendered}"
            );
            assert!(
                rendered.contains("A storage that keeps no journal (a custom GraphStorage"),
                "{rendered}"
            );
            assert!(rendered.contains("g.mark(\"before-import\")"), "{rendered}");
        }

        #[test]
        fn unsupported_graph_file_help_names_both_formats_and_their_extensions() {
            let err = IOError::UnsupportedFormat {
                path: "graph.csv".to_string(),
            };
            let rendered = format_diagnostic(&err);

            assert!(
                rendered.contains("Unsupported graph file format: 'graph.csv'"),
                "the message must name the offending file:\n{rendered}"
            );
            assert!(
                rendered.contains("GraphSON") && rendered.contains("GraphML"),
                "the help must name both supported graph formats:\n{rendered}"
            );
            for ext in [".json", ".jsonl", ".graphson", ".xml", ".graphml"] {
                assert!(
                    rendered.contains(ext),
                    "the help must name the extension {ext}:\n{rendered}"
                );
            }
            assert!(
                rendered.contains("GraphSource::file(\"graph.json\")"),
                "the help must carry a runnable example:\n{rendered}"
            );
        }

        #[test]
        fn graphson_errors_have_help() {
            for err in [
                IOError::GraphSONParse {
                    line: 2,
                    message: "expected ',' or '}' in an object at byte 7".to_string(),
                },
                IOError::UnsupportedGraphSON {
                    line: 1,
                    reason: "this is typed GraphSON 1.0 (@class)".to_string(),
                },
                IOError::InputTooLarge { line: 1, limit: 64 },
            ] {
                let rendered = format_diagnostic(&err);
                assert!(rendered.contains("line"), "{rendered}");
                assert!(
                    err.help()
                        .is_some_and(|h| h.contains("GraphSON") || h.contains("line form")),
                    "{rendered}"
                );
            }
            let too_large = IOError::InputTooLarge { line: 1, limit: 64 };
            assert!(
                too_large
                    .help()
                    .is_some_and(|h| h.contains("with_max_text_bytes")),
                "{too_large}"
            );
        }

        #[test]
        fn path_arena_exhaustion_help_names_limit_and_simple_path() {
            let err = TraverserError::PathArenaExhausted { capacity: 4 };
            assert!(err.to_string().contains("4 path positions"), "{err}");
            let help = err.help().expect("help");
            assert!(help.contains("g.v().out().limit(1000).path()"), "{help}");
            assert!(help.contains("g.v(\"1\").out().path()"), "{help}");
            assert!(help.contains("remove the path consumer"), "{help}");
            assert!(
                help.contains("g.v().both().both().simple_path().path()"),
                "{help}"
            );
            assert!(
                help.contains("g.v().repeat(__.both().simple_path()).times(2).path()"),
                "{help}"
            );
            assert!(
                help.contains("g.v().local(__.out().limit(2)).path()"),
                "{help}"
            );
        }

        #[test]
        fn timeout_help_without_a_query_uses_an_example() {
            let err = TraverserError::Timeout {
                limit_ms: 250,
                elapsed_ms: 251,
            };
            assert_eq!(
                err.to_string(),
                "Traversal exceeded evaluationTimeout of 250 ms (ran 251 ms)"
            );
            let help = err.help().expect("help");
            assert!(
                help.contains("g.v().limit(1000).out().out().count()"),
                "{help}"
            );
            assert!(
                help.contains("g.with(\"evaluationTimeout\", 500).v().out().out().count()"),
                "{help}"
            );
        }

        #[test]
        fn timeout_help_on_a_property_filter_points_to_a_property_index() {
            let steps = vec![
                "v(labels: [\"person\"]).has(\"title\", \"CEO\")".to_string(),
                "out()".to_string(),
                "count()".to_string(),
            ];
            let help = timeout_help(250, Some(steps));
            assert!(
                help.contains("g.create_index(#{label: \"person\", key: \"title\"})"),
                "{help}"
            );
            assert!(help.contains("Narrow the start set"), "{help}");
            // Without a property filter there is no index advice.
            assert!(!timeout_help(250, None).contains("create_index"));
        }

        #[test]
        fn timeout_help_without_a_repeat_does_not_mention_loops() {
            let err = TraverserError::Timeout {
                limit_ms: 250,
                elapsed_ms: 251,
            };
            assert!(!err.help().expect("help").contains("repeat()"));
        }

        #[test]
        fn bulk_expansion_limit_help_explains_why_and_what_to_do() {
            let err = TraverserError::BulkExpansionLimit {
                step: "fold".to_string(),
                requested: 2_505_037_961,
                limit: 100_000_000,
            };
            assert_eq!(
                err.to_string(),
                "Step 'fold' would have to materialize 2505037961 values, more than the limit of \
                 100000000 (bulk.max_expansion)"
            );
            let help = err.help().expect("help");
            assert!(help.contains("record with a multiplicity"), "{help}");
            assert!(
                help.contains("2505037961 values, more than the limit of 100000000"),
                "{help}"
            );
            for advice in ["count()", "group_count()", "dedup()", "limit(n)"] {
                assert!(help.contains(advice), "{advice}: {help}");
            }
            assert!(
                help.contains(
                    "g.with(\"bulk.max_expansion\", 2505037961).v().repeat(__.out()).times(8)"
                ),
                "{help}"
            );
            // The same advice reaches the user when the step failed inside a query.
            let wrapped = TraverserError::StepFailed {
                spelling: crate::traversal::Spelling::Snake,
                step_name: "fold".to_string(),
                step_index: None,
                child_index: None,
                frame: None,
                element_id: None,
                source: Box::new(err),
            };
            assert!(wrapped.help().expect("help").contains("bulk.max_expansion"));
        }

        #[test]
        fn bulk_overflow_help_names_the_step_and_the_remedy() {
            let err = TraverserError::BulkOverflow {
                step: "count".to_string(),
            };
            assert!(err.to_string().contains("step 'count'"));
            let help = err.help().expect("help");
            assert!(help.contains("9223372036854775807"), "{help}");
            assert!(help.contains("Narrow the start set"), "{help}");
            assert!(help.contains("times(n)"), "{help}");
        }

        #[test]
        fn bulk_option_help_shows_a_runnable_example() {
            for key in ["bulk.merge", "bulk.one", "bulk.max_expansion"] {
                let err = TraverserError::InvalidOption {
                    key: key.to_string(),
                    reason: "bad".to_string(),
                };
                assert!(
                    err.help()
                        .expect("help")
                        .contains(&format!("g.with(\"{key}\""))
                );
            }
        }

        #[test]
        fn allocation_failure_help_says_it_is_not_an_option_and_shows_a_narrower_query() {
            // Built by hand: a refused reservation cannot be provoked reliably from a query.
            let err = crate::traversal::capacity::allocation_error(1 << 40);
            assert_eq!(
                err.to_string(),
                "Resource limit 'memory.allocation' exceeded (reached 1099511627776)"
            );
            let help = err.help().expect("help");
            assert!(help.contains("for 1099511627776 items"), "{help}");
            assert!(help.contains("not an option that can be raised"), "{help}");
            assert!(
                help.contains("g.v().has_label(\"person\").out().limit(100)"),
                "{help}"
            );
            assert!(
                help.contains("g.with(\"memory.limit\", 1000000000)"),
                "{help}"
            );
        }

        #[test]
        fn resource_limit_help_names_the_option_and_shows_an_example() {
            let err = TraverserError::ResourceLimitExceeded {
                limit: "traversal.max_traversers".to_string(),
                max: Some(1000),
                observed: Some(1500),
            };
            assert_eq!(
                err.to_string(),
                "Resource limit 'traversal.max_traversers' exceeded (limit 1000, reached 1500)"
            );
            let help = err.help().expect("help");
            assert!(
                help.contains("g.with(\"traversal.max_traversers\", 2000).v().out().out().count()"),
                "{help}"
            );
            assert!(help.contains("ExecutionPolicy::with_locked"), "{help}");
        }

        #[test]
        fn resource_limit_help_for_a_script_limit_names_the_builder_and_the_flag() {
            let err = TraverserError::ResourceLimitExceeded {
                limit: "script.max_operations".to_string(),
                max: Some(10_000_000),
                observed: None,
            };
            assert_eq!(
                err.to_string(),
                "Resource limit 'script.max_operations' exceeded (limit 10000000)"
            );
            let help = err.help().expect("help");
            assert!(
                help.contains("ScriptLimits::default().with_max_operations(20000000)"),
                "{help}"
            );
            assert!(help.contains("ScriptLimits::unlimited()"), "{help}");
            assert!(
                help.contains("graphersal --max-operations 20000000 -e 'g.v().count().next()'"),
                "{help}"
            );
        }

        #[test]
        fn invalid_traverser_cap_help_shows_the_option() {
            let err = TraverserError::InvalidOption {
                key: "traversal.max_traversers".to_string(),
                reason: "expected a positive integer number of values, got 0".to_string(),
            };
            let help = err.help().expect("help");
            assert!(
                help.contains(
                    "g.with(\"traversal.max_traversers\", 50000000).v().out().out().count()"
                ),
                "{help}"
            );
        }

        #[test]
        fn repeat_limit_help_keeps_the_loop_name() {
            let err = TraverserError::RepeatLimitExceeded {
                max_loops: 10_000,
                loop_name: Some("walk".to_string()),
                body: "__.out()".to_string(),
            };
            assert_eq!(
                err.to_string(),
                "repeat() loop 'walk' exceeded the loop limit of 10000 iterations"
            );
            let help = err.help().expect("help");
            assert!(
                help.contains("repeat(\"walk\", __.out()).times(3)"),
                "{help}"
            );
            assert!(
                help.contains("repeat(\"walk\", __.out().simple_path())"),
                "{help}"
            );
            assert!(
                help.contains(
                    "g.with(\"repeat.max_loops\", 20000).v().repeat(\"walk\", __.out())."
                ),
                "{help}"
            );
            // Without the failing query, the glob_path() alternative starts from every vertex.
            assert!(
                help.ends_with(
                    "\nOn a tree-shaped graph (directories, org charts, categories), walk the \
                     levels below a vertex with glob_path() instead: it follows outgoing edges \
                     along a pattern, prunes branches that cannot match and stops on cycles \
                     without a loop limit, for example g.v().glob_path(\"**/*.rs\").by(\"name\"), \
                     or g.v().glob_path(\"**\").by(\"name\") for every level below; bound how \
                     deep it walks with max_depth, for example \
                     g.v().glob_path(\"**\", #{max_depth: 3}).by(\"name\")."
                ),
                "{help}"
            );
        }

        #[test]
        fn glob_path_max_depth_help_without_a_query_uses_an_example() {
            let err = TraverserError::InvalidOption {
                key: GLOB_PATH_MAX_DEPTH.to_string(),
                reason: "must be at least 1, got 0".to_string(),
            };
            assert_eq!(
                err.to_string(),
                "Invalid value for option 'glob_path.max_depth': must be at least 1, got 0"
            );
            assert_eq!(
                err.help().expect("help"),
                "max_depth is the deepest level below the start vertex that glob_path() visits: \
                 the start vertex is level 0 and its children are level 1, so max_depth is an \
                 integer from 1 to 4294967295. Leave it out to walk without a bound. For example \
                 g.v(\"root\").glob_path(\"**/*.rs\", #{max_depth: 3}).by(\"name\")."
            );
        }

        #[test]
        fn glob_path_option_rewrites_keep_other_steps() {
            let steps = |s: &[&str]| s.iter().map(|s| s.to_string()).collect::<Vec<_>>();
            // A pattern literal that contains a quote and a brace is not mistaken for options.
            assert_eq!(
                split_glob_path_step(r#"glob_path("a\", {b", {max_depth: 0}).by("name")"#),
                Some((r#"glob_path("a\", {b""#, r#", {max_depth: 0}).by("name")"#))
            );
            assert_eq!(split_glob_path_step("out()"), None);
            assert_eq!(
                glob_path_step_with_valid_max_depth(
                    r#"glob_path("**", {max_depth: -7}).by("name")"#
                ),
                Some(r#"glob_path("**", {max_depth: 1}).by("name")"#.to_string())
            );
            assert_eq!(
                glob_path_step_with_valid_max_depth(r#"glob_path("**", {max_depth: 7})"#),
                None
            );
            // Only a '**' walk without max_depth is bounded.
            assert_eq!(
                glob_path_query_with_max_depth(&steps(&[
                    "v()",
                    "glob_path(\"src/*\").by(\"name\")"
                ])),
                None
            );
            assert_eq!(
                glob_path_query_with_max_depth(&steps(&[
                    "v()",
                    "glob_path(\"**\", {max_depth: 9})"
                ])),
                None
            );
            assert_eq!(
                glob_path_query_with_max_depth(&steps(&[
                    "v()",
                    "glob_path(\"**/*.rs\").by(\"name\")",
                    "count()"
                ])),
                Some(
                    "v().glob_path(\"**/*.rs\", #{max_depth: 3}).by(\"name\").count()".to_string()
                )
            );
        }

        #[test]
        fn glob_path_start_keeps_the_steps_before_the_loop() {
            let steps = |s: &[&str]| s.iter().map(|s| s.to_string()).collect::<Vec<_>>();
            assert_eq!(
                glob_path_start(&steps(&[
                    "v(\"root\")",
                    "has_label(\"directory\")",
                    "repeat(__.out())"
                ])),
                Some("v(\"root\").has_label(\"directory\")".to_string())
            );
            assert_eq!(
                glob_path_start(&steps(&["v(\"root\")", "emit()", "repeat(__.out())"])),
                Some("v(\"root\")".to_string())
            );
            // The loop is nested in another step: no top-level position to rewrite.
            assert_eq!(
                glob_path_start(&steps(&["v(\"root\")", "local(__.repeat(__.out()))"])),
                None
            );
            assert_eq!(glob_path_start(&steps(&["repeat(__.out())"])), None);
        }

        /// Step 3 of the type system: storing a vertex as a property value is rejected, and the help
        /// shows how to store its id instead, built from the failing property name.
        #[test]
        fn invalid_property_value_help_stores_the_id_instead() {
            let graph = GraphSource::tinkerpop_modern();
            let err = graph
                .write()
                .traversal_mut()
                .v("1")
                .property_by("friend", __::out(None))
                .next()
                .expect_err("a vertex must not be stored as a property value");
            let root = err.root_cause();
            assert_eq!(
                root.to_string(),
                "Property 'friend' cannot hold a vertex: vertices and edges are not property values"
            );
            assert_eq!(
                root.help().expect("help"),
                "Store the vertex's id instead of the vertex itself, e.g. \
                 property(\"friend\", __.out(\"knows\").id()), and look the vertex up again with g.v(id)."
            );

            let edge_err = GraphError::InvalidPropertyValue {
                property_name: "via".to_string(),
                kind: "edge",
            };
            assert_eq!(
                edge_err.to_string(),
                "Property 'via' cannot hold an edge: vertices and edges are not property values"
            );
            assert_eq!(
                edge_err.help().expect("help"),
                "Store the edge's id instead of the edge itself, e.g. \
                 property(\"via\", __.out_e(\"knows\").id()), and look the edge up again with g.e(id)."
            );
        }

        /// A failed coercion on write names the declared type, the offending value and the reason, and
        /// the help shows the accepted form.
        #[test]
        fn coercion_failed_help_names_the_accepted_form() {
            let coercion_failed =
                |declared_type: &str, actual_value: &str, reason: &'static str| {
                    SchemaError::CoercionFailed {
                        element_type: "vertex",
                        label: "person".to_string(),
                        property_name: "uid".to_string(),
                        declared_type: declared_type.to_string(),
                        actual_value: actual_value.into(),
                        reason,
                    }
                };

            let uuid = coercion_failed(
                "uuid",
                "\"abc\"",
                "not a canonical UUID (8-4-4-4-12 lowercase hex)",
            );
            assert_eq!(
                uuid.to_string(),
                "Cannot store \"abc\" in vertex 'person.uid' declared as uuid: not a canonical UUID \
                 (8-4-4-4-12 lowercase hex)"
            );
            assert_eq!(
                uuid.help().expect("help"),
                "UUIDs are written as 8-4-4-4-12 lowercase hex, e.g. \
                 `550e8400-e29b-41d4-a716-446655440000`."
            );

            let float = coercion_failed(
                "float64",
                "9007199254740993",
                "integer cannot be represented exactly as float64",
            );
            assert_eq!(
                float.help().expect("help"),
                "The integer is too large to be represented exactly as float64; store it in an int64 \
                 field."
            );
        }

        /// A declared `uuid` given a non-string value: the help names the explicit conversion in both
        /// the Rust and the Rhai spelling, since only a canonical string converts on write.
        #[test]
        fn type_mismatch_on_uuid_help_names_the_cast_in_both_spellings() {
            let err = SchemaError::TypeMismatch {
                element_type: "vertex",
                label: "person".to_string(),
                property_name: "uid".to_string(),
                expected_type: "uuid".to_string(),
                actual_type: "int64".to_string(),
            };
            assert_eq!(
                err.help().expect("help"),
                "Convert value to expected type 'uuid' before inserting.\n\
                 In a query, convert the values with values(\"uid\").cast(GType.UUID).\n\
                 Only a canonical UUID string is converted to a uuid on write; convert a string value \
                 explicitly with .cast(GType.UUID).\n\
                 Find every offending value with values(\"uid\").is(typeOf(GType.LONG))."
            );

            // A mismatch on any other declared type does not mention it.
            let other = SchemaError::TypeMismatch {
                element_type: "vertex",
                label: "person".to_string(),
                property_name: "name".to_string(),
                expected_type: "string".to_string(),
                actual_type: "uuid".to_string(),
            };
            assert!(
                !other
                    .help()
                    .expect("help")
                    .contains("Only a canonical UUID string")
            );
        }

        #[test]
        fn ambiguous_container_import_help_names_both_ways_out() {
            let err = IOError::AmbiguousContainerImport {
                key: "tags".to_string(),
                declared: "union<string|array<int64>>".to_string(),
            };
            let rendered = format_diagnostic(&err);
            assert!(
                rendered.contains(
                    "Property 'tags' is declared as union<string|array<int64>>, a union of an \
                     array/object and a text type"
                ),
                "the message must name the property and its type:\n{rendered}"
            );
            let help = err.help().expect("help");
            assert!(
                help.contains("Declare 'tags' either as an array/object"),
                "{help}"
            );
            assert!(help.contains("before importing"), "{help}");
        }

        #[test]
        fn invalid_json_value_help_shows_the_expected_text() {
            let err = IOError::InvalidJsonValue {
                key: "config".to_string(),
                declared: "object".to_string(),
                reason: "expected ',' or '}' in an object at byte 7".to_string(),
            };
            let rendered = format_diagnostic(&err);
            assert!(
                rendered.contains(
                    "Property 'config' is declared as object, but its GraphML value is not valid \
                     JSON: expected ',' or '}' in an object at byte 7"
                ),
                "{rendered}"
            );
            let help = err.help().expect("help");
            assert!(help.contains(r#"{"cpu":2}"#), "{help}");
            assert!(help.contains("declare 'config' as `string`"), "{help}");
        }

        #[test]
        fn non_finite_float_export_help_names_the_repairing_steps() {
            let err = IOError::NonFiniteFloat {
                key: "w".to_string(),
                value: "NaN".to_string(),
            };
            let rendered = format_diagnostic(&err);

            assert!(
                rendered.contains("Property 'w' holds the non-finite float NaN"),
                "the message must name the property and the value:\n{rendered}"
            );
            assert!(
                rendered.contains(".property(\"w\", 0.0)")
                    && rendered.contains(".properties(\"w\").drop()")
                    && rendered.contains(".remove_property(\"w\")"),
                "the help must carry runnable repairs for the offending key:\n{rendered}"
            );
        }
    }

    // Cause-chain and help-coverage tests moved from `tests/all/error_handling_tests.rs`: they build
    // errors by hand, and named-field error variants are `#[non_exhaustive]`, so only code inside
    // this crate can construct them.
    mod constructed_errors {
        use super::super::*;

        /// Asserts that a rendered diagnostic carries `expected_help`, allowing for the indentation
        /// `format_diagnostic` adds to every line after the first.
        fn assert_help_rendered(formatted: &str, expected_help: &str) {
            for (index, line) in expected_help.lines().enumerate() {
                let expected_line = if index == 0 {
                    format!("Help: {line}")
                } else {
                    format!("      {line}")
                };
                assert!(
                    formatted.contains(&expected_line),
                    "missing help line `{expected_line}` in: {formatted}"
                );
            }
        }

        #[test]
        fn test_cause_chain_propagation() {
            use std::error::Error;

            let inner_graph_err = GraphError::VertexNotFound {
                id: "v_999".to_string(),
            };
            let step_err = TraverserError::StepFailed {
                spelling: crate::traversal::Spelling::Snake,
                step_name: "out".to_string(),
                step_index: None,
                child_index: None,
                frame: None,
                element_id: Some("v_1".to_string()),
                source: Box::new(TraverserError::Graph(inner_graph_err)),
            };

            assert!(step_err.source().is_some());
            let cause = step_err.source().unwrap();
            assert!(cause.to_string().contains("v_999"));

            let formatted = format_diagnostic(&step_err);
            assert!(formatted.starts_with("Error: Step 'out' execution failed"));
            assert!(
                formatted.contains("Caused by: Vertex with ID 'v_999' was not found in the graph")
            );
            assert!(formatted.contains(
                "Help: Verify that the vertex ID exists in the graph before referencing it."
            ));
        }

        #[test]
        fn test_diagnostic_error_help_coverage() {
            let err_v_not_found = GraphError::VertexNotFound {
                id: "100".to_string(),
            };
            assert!(err_v_not_found.help().is_some());
            assert!(!err_v_not_found.help().unwrap().is_empty());

            let err_prop_not_found = ValueError::PropertyNotFound {
                key: "age".to_string(),
                element_type: "vertex",
            };
            assert!(err_prop_not_found.help().is_some());
            assert!(!err_prop_not_found.help().unwrap().is_empty());

            let err_cast = ValueError::CastException {
                expected: "number".to_string(),
                actual: "string".to_string(),
                value: None,
                origin: None,
            };
            assert!(err_cast.help().is_some());
            assert!(!err_cast.help().unwrap().is_empty());

            let err_missing_attr = IOError::MissingAttribute {
                element: "node".to_string(),
                attribute: "id".to_string(),
            };
            assert!(err_missing_attr.help().is_some());
            assert!(!err_missing_attr.help().unwrap().is_empty());

            let err_invalid_attr = IOError::InvalidAttributeValue {
                key: "weight".to_string(),
                attribute: "value".to_string(),
                value: "not_a_number".to_string(),
                expected_type: "float".to_string(),
            };
            assert!(err_invalid_attr.help().is_some());
            assert!(!err_invalid_attr.help().unwrap().is_empty());
        }

        #[test]
        fn test_schema_error_diagnostics_and_formatting() {
            let variants: Vec<(SchemaError, &str, &str)> = vec![
                (
                    SchemaError::UnknownVertexLabel {
                        label: "Alien".to_string(),
                        allowed: vec!["Human".to_string(), "Robot".to_string()],
                    },
                    "Unknown vertex label 'Alien' is not declared in the closed schema",
                    "A closed schema admits only declared vertex labels: use one of [Human, Robot]",
                ),
                (
                    SchemaError::UnknownEdgeLabel {
                        label: "teleports_to".to_string(),
                        allowed: vec!["walks_to".to_string(), "flies_to".to_string()],
                    },
                    "Unknown edge label 'teleports_to' is not declared in the closed schema",
                    "A closed schema admits only declared edge labels: use one of [walks_to, flies_to]",
                ),
                (
                    SchemaError::InvalidEdgeTopology {
                        source_label: "Human".to_string(),
                        edge_label: "operates".to_string(),
                        target_label: "Planet".to_string(),
                        allowed: vec!["(:Human)-[:operates]->(:Vehicle)".to_string()],
                    },
                    "Disallowed edge connection: '(:Human)-[:operates]->(:Planet)' is not defined in schema",
                    "A closed schema admits an edge only between the labels of a declared connection",
                ),
                (
                    SchemaError::UndeclaredProperty {
                        element_type: "vertex",
                        label: "Human".to_string(),
                        property_name: "mana".to_string(),
                    },
                    "Undeclared property 'mana' on vertex 'Human' is not declared and not admitted by the schema",
                    "Declare 'mana' in the label's \"properties\"",
                ),
                (
                    SchemaError::MissingRequiredProperty {
                        element_type: "edge",
                        label: "created".to_string(),
                        property_name: "weight".to_string(),
                    },
                    "Missing required property 'weight' on edge 'created'",
                    "Provide a value for 'weight' or remove it from the label's \"required\"",
                ),
                (
                    SchemaError::TypeMismatch {
                        element_type: "vertex",
                        label: "Robot".to_string(),
                        property_name: "serial_no".to_string(),
                        expected_type: "int64".to_string(),
                        actual_type: "string".to_string(),
                    },
                    "Type mismatch on vertex 'Robot.serial_no': expected int64, got string",
                    "Convert value to expected type 'int64' before inserting.\n\
                     In a query, convert the values with values(\"serial_no\").as_number(GType.LONG).\n\
                     Find every offending value with values(\"serial_no\").is(typeOf(GType.STRING)).",
                ),
                (
                    SchemaError::ConstraintViolation {
                        label: "Robot".to_string(),
                        property_name: "battery".to_string(),
                        constraint_rule: "value <= 100".to_string(),
                        actual_value: "120".to_string(),
                    },
                    "Constraint violation on 'Robot.battery': value <= 100 (actual: 120)",
                    "Ensure value complies with validation constraint: value <= 100.",
                ),
                (
                    SchemaError::InvalidSchema {
                        path: "/mdoe".to_string(),
                        reason: "unknown key 'mdoe'".to_string(),
                        hint: "The envelope keys are $schema, meta, mode, vertices and edges."
                            .to_string(),
                    },
                    "Invalid schema at /mdoe: unknown key 'mdoe'",
                    "The envelope keys are $schema, meta, mode, vertices and edges.",
                ),
            ];

            for (schema_err, expected_msg, expected_help) in variants {
                assert_eq!(schema_err.to_string(), expected_msg);
                // A help may continue past the expected first line (the missing-property help
                // appends the add_property_fold advice).
                let help = schema_err.help().expect("schema errors carry help");
                assert!(help.starts_with(expected_help), "{help}");

                let formatted = format_diagnostic(&schema_err);
                assert!(
                    formatted.contains(&format!("Error: {}", expected_msg)),
                    "format_diagnostic missing main error message in: {}",
                    formatted
                );
                assert_help_rendered(&formatted, expected_help);

                // Verify conversion into TraverserError
                let trav_err: TraverserError = schema_err.into();
                assert_eq!(trav_err.to_string(), expected_msg);
                assert!(trav_err.help().expect("help").starts_with(expected_help));
                let formatted_trav = format_diagnostic(&trav_err);
                assert!(
                    formatted_trav.contains(&format!("Error: {}", expected_msg)),
                    "format_diagnostic on TraverserError missing error message in: {}",
                    formatted_trav
                );
                assert_help_rendered(&formatted_trav, expected_help);
            }
        }

        #[test]
        fn test_step_failure_context() {
            let inner_err = TraverserError::Value(ValueError::PropertyNotFound {
                key: "non_existent".to_string(),
                element_type: "vertex",
            });

            let step_err = TraverserError::StepFailed {
                spelling: crate::traversal::Spelling::Snake,
                step_name: "values".to_string(),
                step_index: None,
                child_index: None,
                frame: None,
                element_id: Some("vertex_42".to_string()),
                source: Box::new(inner_err),
            };

            if let TraverserError::StepFailed {
                step_name,
                element_id,
                source,
                ..
            } = &step_err
            {
                assert_eq!(step_name, "values");
                assert_eq!(element_id.as_deref(), Some("vertex_42"));
                assert!(source.help().is_some());
            } else {
                panic!("Expected StepFailed variant");
            }
        }
    }

    /// GQL errors (feature `gql`): line and column from byte offsets, the caret excerpt of the
    /// offending line only, the expected tokens, the suggestion, the GQLSTATUS code and the
    /// help per kind.
    #[cfg(feature = "gql")]
    mod gql {
        use super::super::*;

        fn report(err: &TraverserError) -> String {
            format_diagnostic(err)
        }

        #[test]
        fn single_line_syntax_error_underlines_the_token() {
            let query = "MATCH (n:person) RETRUN n.name";
            let err = GqlErrorBuilder::new(GqlErrorKind::Syntax, 17..23, "unexpected `RETRUN`")
                .expected(["WHERE", "RETURN", ","])
                .suggestion("RETURN")
                .into_error(query);
            let TraverserError::Gql {
                line,
                column,
                width,
                offset,
                ref gqlstatus,
                ..
            } = err
            else {
                panic!("expected a Gql error");
            };
            assert_eq!((line, column, width, offset), (1, 18, 6, 17));
            assert_eq!(gqlstatus, "42001");
            assert_eq!(
                err.to_string(),
                "GQL syntax error at line 1, column 18: unexpected `RETRUN`"
            );
            assert_eq!(
                report(&err),
                "Error: GQL syntax error at line 1, column 18: unexpected `RETRUN`\n  \
                 query:        MATCH (n:person) RETRUN n.name\n                \
                 \x20                ^^^^^^\n  \
                 expected:     WHERE, RETURN, ,\n  \
                 did you mean: RETURN\n  \
                 GQLSTATUS:    42001\n\
                 Help: Did you mean RETURN? Expected WHERE, RETURN, , at the marked position. \
                 Fix the query at the marked position: a GQL read query is MATCH <pattern> \
                 [WHERE <condition>], then any further MATCH, OPTIONAL MATCH, WITH, LET or \
                 FILTER clauses, then RETURN <items>, for example \
                 g.gql(\"MATCH (p:person) WHERE p.age > 30 RETURN p.name\") or \
                 g.gql(\"MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) WITH p, count(s) \
                 AS n RETURN p.name, n\"). CALL (p) { .. RETURN .. } runs a query for each \
                 row, UNION combines the records of two queries and NEXT continues after a \
                 RETURN, as in g.gql(\"MATCH (p:person) RETURN p NEXT MATCH \
                 (p)-[:created]->(s) RETURN p.name, s.name\"). FOR x IN <list> (Cypher UNWIND <list> AS x) gives \
                 one row per item of a list, and a query may also start with FOR, LET, WITH, \
                 FILTER or RETURN, as in \
                 g.gql(\"FOR n IN ['marko', 'josh'] MATCH (p:person {name: n}) RETURN n, p.age\")."
            );
        }

        #[test]
        fn multi_line_query_shows_only_the_offending_line() {
            let query = "MATCH (n:person)\n  WHERE n.age >\nRETURN n";
            // The end of line 2 (an empty span after `>`).
            let offset = query.find(">\n").map(|at| at + 1).unwrap_or(0);
            let err = GqlErrorBuilder::new(
                GqlErrorKind::Syntax,
                offset..offset,
                "expected an expression",
            )
            .into_error(query);
            assert_eq!(
                err.to_string(),
                "GQL syntax error at line 2, column 16: expected an expression"
            );
            let details = err.details();
            assert_eq!(details[0].0, "line 2");
            assert_eq!(details[0].1, "  WHERE n.age >\n               ^");
            assert!(!details[0].1.contains("MATCH"));
            assert!(
                !details
                    .iter()
                    .any(|(key, _)| key == "expected" || key == "did you mean")
            );
            assert_eq!(
                details.last(),
                Some(&("GQLSTATUS".to_string(), "42001".to_string()))
            );
            // No expected tokens: the help names none.
            assert!(!err.help().unwrap_or_default().contains("Expected"));
        }

        #[test]
        fn columns_count_characters_and_spans_stop_at_the_line_end() {
            let query = "MATCH (ä:person)\nRETURN äb, x\nLIMIT 1";
            let start = query.find('x').unwrap_or(0);
            // A span running into the next line is underlined up to the end of its own line.
            let err =
                GqlErrorBuilder::new(GqlErrorKind::Semantic, start..query.len(), "unknown `x`")
                    .suggestion("ä")
                    .into_error(query);
            let TraverserError::Gql { line, column, .. } = err else {
                panic!("expected a Gql error");
            };
            assert_eq!((line, column), (2, 12));
            assert_eq!(err.details()[0].1, "RETURN äb, x\n           ^");
            assert!(
                err.to_string()
                    .starts_with("GQL semantic error at line 2, column 12")
            );
            assert!(report(&err).contains("GQLSTATUS:    42002"));
            // An offset inside a character or past the end never panics.
            let err = GqlErrorBuilder::new(GqlErrorKind::Syntax, 8..1000, "x").into_error(query);
            assert!(matches!(err, TraverserError::Gql { offset: 7, .. }));
            #[allow(clippy::reversed_empty_ranges)] // a reversed span on purpose
            let err = GqlErrorBuilder::new(GqlErrorKind::Syntax, 1000..3, "x").into_error(query);
            assert!(matches!(
                err,
                TraverserError::Gql {
                    line: 3,
                    column: 8,
                    width: 1,
                    ..
                }
            ));
        }

        #[test]
        fn long_lines_are_windowed_around_the_span() {
            let query = format!("MATCH (n) WHERE {} RETURN n", "n.a = 1 AND ".repeat(20));
            let start = query.find("RETURN").unwrap_or(0) + 7;
            let err = GqlErrorBuilder::new(GqlErrorKind::Semantic, start..start + 1, "unknown")
                .into_error(&query);
            let excerpt = &err.details()[0].1;
            let mut rows = excerpt.lines();
            let shown = rows.next().unwrap_or_default();
            let caret = rows.next().unwrap_or_default();
            assert!(shown.starts_with('…'), "{shown}");
            assert!(shown.chars().count() <= crate::value::literal::MAX_LITERAL_CHARS + 2);
            // The caret stands under the `n` after RETURN.
            let at = caret.chars().count() - 1;
            assert_eq!(shown.chars().nth(at), Some('n'));
            assert_eq!(shown.chars().nth(at.saturating_sub(7)), Some('R'));
        }

        #[test]
        fn help_per_kind() {
            let help = |builder: GqlErrorBuilder| {
                builder
                    .into_error("MATCH (n) CREATE (m) RETURN n")
                    .help()
                    .unwrap_or_default()
            };
            let refused = help(
                GqlErrorBuilder::new(GqlErrorKind::Refused, 10..16, "CREATE is not supported")
                    .alternative(
                        "gql() is read-only; use the Gremlin mutation steps add_v(), \
                         property(), drop() or merge_v()",
                    ),
            );
            assert!(
                refused.starts_with(
                    "gql() is read-only; use the Gremlin mutation steps add_v(), property(), \
                     drop() or merge_v(). gql() does not support this construct"
                ),
                "{refused}"
            );
            let generic = help(GqlErrorBuilder::new(GqlErrorKind::Refused, 10..16, "no"));
            assert!(
                generic.contains("does not support this construct"),
                "{generic}"
            );
            let not_yet = help(
                GqlErrorBuilder::new(GqlErrorKind::NotYet, 0..5, "OPTIONAL MATCH")
                    .alternative("Use coalesce() or optional() in Gremlin"),
            );
            assert!(not_yet.starts_with("Use coalesce() or optional() in Gremlin. "));
            assert!(
                not_yet.contains("comes in a later version of gql()"),
                "{not_yet}"
            );
            let limit = help(GqlErrorBuilder::new(GqlErrorKind::Limit, 0..1, "too deep"));
            assert!(limit.contains("nests deeper than gql() accepts"), "{limit}");
            let semantic = help(
                GqlErrorBuilder::new(GqlErrorKind::Semantic, 28..29, "unknown").suggestion("n"),
            );
            assert!(semantic.starts_with("Did you mean n? Declare every variable"));
            // Many expected tokens: help lists the first ones, the detail all of them.
            let tokens: Vec<String> = (0..12).map(|i| format!("T{i}")).collect();
            let err = GqlErrorBuilder::new(GqlErrorKind::Syntax, 0..1, "bad")
                .expected(tokens)
                .into_error("x");
            let text = err.help().unwrap_or_default();
            assert!(
                text.contains("Expected T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, ... at"),
                "{text}"
            );
            assert!(report(&err).contains("T11\n"));
        }

        #[test]
        fn gqlstatus_defaults_per_kind_and_overrides() {
            let status = |err: TraverserError| match err {
                TraverserError::Gql { gqlstatus, .. } => gqlstatus,
                _ => String::new(),
            };
            for (kind, code) in [
                (GqlErrorKind::Syntax, "42001"),
                (GqlErrorKind::Semantic, "42002"),
                (GqlErrorKind::Refused, "0A000"),
                (GqlErrorKind::NotYet, "0A000"),
                (GqlErrorKind::Limit, "54001"),
            ] {
                assert_eq!(
                    status(GqlErrorBuilder::new(kind, 0..1, "m").into_error("q")),
                    code
                );
            }
            let err = GqlErrorBuilder::new(GqlErrorKind::Semantic, 0..1, "m")
                .gqlstatus("42004")
                .into_error("q");
            assert_eq!(status(err), "42004");
        }

        #[test]
        fn data_exception_keeps_its_code_and_names_the_guard() {
            let query = "MATCH (p) RETURN 1 / p.age";
            let err = GqlErrorBuilder::new(GqlErrorKind::Data, 17..26, "division by zero")
                .gqlstatus("22012")
                .into_error(query);
            let TraverserError::Gql {
                kind, gqlstatus, ..
            } = &err
            else {
                panic!("not a GQL error: {err:?}");
            };
            assert_eq!(*kind, GqlErrorKind::Data);
            assert_eq!(gqlstatus, "22012");
            assert_eq!(
                GqlErrorBuilder::new(GqlErrorKind::Data, 0..1, "m")
                    .into_error("q")
                    .to_string(),
                "GQL data exception at line 1, column 1: m"
            );
            let help = err.help().unwrap_or_default();
            assert!(
                help.starts_with("A row of the data made the marked"),
                "{help}"
            );
            assert!(
                help.contains("WHERE p.age <> 0 RETURN 100 / p.age"),
                "{help}"
            );
            assert!(report(&err).contains("22012"), "{}", report(&err));
        }

        #[test]
        fn help_is_respelled_under_camel_case() {
            let mut err = GqlErrorBuilder::new(GqlErrorKind::Refused, 0..1, "m")
                .alternative("use add_v() or merge_v()")
                .into_error("CREATE (n)");
            err.respell(crate::traversal::Spelling::Camel);
            let help = err.help().unwrap_or_default();
            assert!(help.starts_with("use addV() or mergeV(). "), "{help}");
        }
    }
}
