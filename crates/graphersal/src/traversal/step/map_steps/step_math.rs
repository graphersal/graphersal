//! `math()`: evaluates an exp4j equation (TinkerPop's `MathStep`) per traverser with the own
//! expression engine in `value::math_expr`, parsed once when the step is built.

use crate::graph::GraphStorage;
use crate::traversal::optimizer::path_analysis::{PathAnalysis, PathRequirement};
use crate::value::math_expr::{
    MathEvalError, MathEvalErrorKind, MathExpr, MathParseError, MathParseErrorKind, PathSeg, VarRef,
};
use crate::*;

/// The name of the current traverser's value in an equation.
const CURRENT: &str = "_";

/// Variables kept in a stack buffer per traverser; equations with more use one heap buffer.
const INLINE_SLOTS: usize = 8;

struct StepMath<'g, S: GraphStorage + 'g> {
    /// The equation as written.
    expr: String,
    /// The equation compiled once, or why it does not parse (raised before execution).
    parsed: Result<MathExpr, MathParseError>,
    by: Option<Vec<By<'g, S>>>,
    by_check: ByModulatorCheck,
}

impl<'g, S: GraphStorage + 'g> Clone for StepMath<'g, S> {
    fn clone(&self) -> Self {
        Self {
            expr: self.expr.clone(),
            parsed: self.parsed.clone(),
            by: self.by.clone(),
            by_check: self.by_check.clone(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> StepMath<'g, S> {
    pub fn new(expr: &str) -> Self {
        Self {
            expr: expr.to_string(),
            parsed: MathExpr::parse(expr),
            by: None,
            by_check: ByModulatorCheck::default(),
        }
    }

    /// The parse error as a [`TraverserError::MathExpression`].
    fn parse_error(&self, err: &MathParseError) -> BoxedTraverserError {
        let (kind, width) = match &err.kind {
            MathParseErrorKind::UnknownFunction { name, .. } => (
                MathErrorKind::UnknownFunction { name: name.clone() },
                name.chars().count(),
            ),
            _ => (MathErrorKind::Syntax, 1),
        };
        Box::new(TraverserError::math_expression(
            &self.expr,
            err.offset,
            width,
            kind,
            err.kind.to_string(),
        ))
    }

    /// An evaluation error (division by zero) as a [`TraverserError::MathExpression`].
    fn eval_error(&self, err: &MathEvalError) -> BoxedTraverserError {
        let (kind, width) = match err.kind {
            MathEvalErrorKind::DivisionByZero => (MathErrorKind::DivisionByZero, 1),
            MathEvalErrorKind::CotangentDivisionByZero => (MathErrorKind::DivisionByZero, 3),
            MathEvalErrorKind::MalformedProgram => (MathErrorKind::Internal, 1),
        };
        Box::new(TraverserError::math_expression(
            &self.expr,
            err.offset,
            width,
            kind,
            err.kind.to_string(),
        ))
    }

    /// A variable error at the occurrence `var` (`depth` path segments rendered).
    fn var_error(
        &self,
        name: &str,
        var: &VarRef,
        depth: usize,
        kind: MathErrorKind,
        message: String,
    ) -> BoxedTraverserError {
        let width = render_path(name, var.path.get(..depth).unwrap_or(&var.path))
            .chars()
            .count();
        Box::new(TraverserError::math_expression(
            &self.expr, var.offset, width, kind, message,
        ))
    }

    /// Resolves every variable of `expr` for `source` (applying the `by()` ring), then evaluates
    /// the equation. `Ok(None)` when a `by()` is unproductive: the traverser is filtered.
    fn evaluate<'t>(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &'t Traverser<S>,
        expr: &MathExpr,
        slots: &mut [Option<Slot<'t, S>>],
    ) -> TraverserResult<Option<f64>> {
        let by_list = self.by.as_deref().unwrap_or_default();
        for (i, name) in expr.variables().iter().enumerate() {
            let by = if by_list.is_empty() {
                None
            } else {
                by_list.get(i % by_list.len())
            };
            let slot = if name.as_str() == CURRENT {
                // `_` is the current traverser; its `by()` runs on it, path included.
                match by {
                    None => Slot::Borrowed(&source.value),
                    Some(by) => match TraverserValue::fetch_traverser_value_by(
                        context,
                        source,
                        &source.value,
                        by,
                    )? {
                        Some(projected) => Slot::Owned(projected),
                        None => return Ok(None),
                    },
                }
            } else {
                match scope_value(context, source, name)? {
                    Some(scoped) => match by {
                        None => Slot::Owned(scoped),
                        Some(by) => {
                            // Any other variable's `by()` runs on that object alone.
                            let start = Traverser::start(context, scoped);
                            match TraverserValue::fetch_traverser_value_by(
                                context,
                                &start,
                                &start.value,
                                by,
                            )? {
                                Some(projected) => Slot::Owned(projected),
                                None => return Ok(None),
                            }
                        }
                    },
                    None => {
                        let first = expr.var_refs().iter().find(|r| usize::from(r.slot) == i);
                        match first {
                            Some(VarRef { constant: Some(c), .. }) => Slot::Constant(*c),
                            Some(var) => {
                                return Err(self.var_error(
                                    name,
                                    var,
                                    0,
                                    MathErrorKind::UnknownVariable { name: name.to_string() },
                                    format!(
                                        "variable '{name}' is not '_', a key of the current map, a side \
                                         effect or a step label"
                                    ),
                                ))
                            }
                            None => return Err(self.eval_error(&malformed())),
                        }
                    }
                }
            };
            match slots.get_mut(i) {
                Some(cell) => *cell = Some(slot),
                None => return Err(self.eval_error(&malformed())),
            }
        }

        let names = expr.variables();
        let ctx: &TraverserContext<'_, S> = context;
        let storage: &S = ctx.storage;
        let slots: &[Option<Slot<'t, S>>] = slots;
        let result = expr.eval(|var: &VarRef| -> Result<f64, Failure> {
            let index = usize::from(var.slot);
            let (Some(Some(slot)), Some(name)) = (slots.get(index), names.get(index)) else {
                return Err(Failure::Eval(malformed()));
            };
            let start = match slot {
                Slot::Constant(c) => return Ok(*c),
                Slot::Borrowed(value) => Cursor::Value(value),
                Slot::Owned(value) => Cursor::Value(value),
            };
            let mut cursor = start;
            for (depth, seg) in var.path.iter().enumerate() {
                cursor = match cursor.step(storage, seg).map_err(Failure::storage)? {
                    Some(next) => next,
                    None => {
                        let path = render_path(name, var.path.get(..=depth).unwrap_or(&var.path));
                        let why = cursor.explain(storage, seg).map_err(Failure::storage)?;
                        let message = format!("{path} leads to no value: {why}");
                        return Err(Failure::Traverser(self.var_error(
                            name,
                            var,
                            depth + 1,
                            MathErrorKind::BadPath { path },
                            message,
                        )));
                    }
                };
            }
            cursor.number(storage).map_err(|leaf| {
                let path = render_path(name, &var.path);
                let (kind, message) = match leaf {
                    Leaf::Storage(err) => return Failure::storage(err),
                    Leaf::Null => (
                        MathErrorKind::Null { path: path.clone() },
                        format!("the variable {path} for math() must resolve to a number, but it is null"),
                    ),
                    Leaf::Other(actual) => (
                        MathErrorKind::NotANumber {
                            path: path.clone(),
                            actual: actual.to_string(),
                        },
                        format!("the variable {path} for math() must resolve to a number, but it is a {actual}"),
                    ),
                };
                Failure::Traverser(self.var_error(name, var, var.path.len(), kind, message))
            })
        });
        match result {
            Ok(value) => Ok(Some(value)),
            Err(Failure::Eval(err)) => Err(self.eval_error(&err)),
            Err(Failure::Traverser(err)) => Err(err),
        }
    }
}

/// The error of [`MathExpr::eval`]'s resolver: a variable error, or an evaluation error.
enum Failure {
    Eval(MathEvalError),
    Traverser(BoxedTraverserError),
}

impl Failure {
    /// A storage read error while resolving a variable: the query fails with it.
    fn storage(err: GraphError) -> Self {
        Failure::Traverser(Box::new(err.into()))
    }
}

impl From<MathEvalError> for Failure {
    fn from(err: MathEvalError) -> Self {
        Failure::Eval(err)
    }
}

/// An engine inconsistency (never caused by the equation).
fn malformed() -> MathEvalError {
    MathEvalError {
        offset: 0,
        kind: MathEvalErrorKind::MalformedProgram,
    }
}

/// The value of one equation variable for the current traverser, after its `by()`.
enum Slot<'t, S: GraphStorage> {
    /// The current traverser's own value (`_` without `by()`), not copied.
    Borrowed(&'t TraverserValue<S>),
    /// A scoped or projected value.
    Owned(TraverserValue<S>),
    /// An unresolved `pi`/`e`: the constant.
    Constant(f64),
}

/// The value of `name` in TinkerPop's `Scoping` order: a key of the current map, then a side
/// effect, then a step label (`Pop.last`). `None` when found nowhere.
fn scope_value<S: GraphStorage>(
    context: &TraverserContext<'_, S>,
    source: &Traverser<S>,
    name: &str,
) -> Result<Option<TraverserValue<S>>, GraphError> {
    let storage: &S = context.storage;
    let key = PathSeg::Key(name.into());
    let from_map = match &source.value {
        TraverserValue::Object(_)
        | TraverserValue::Record { .. }
        | TraverserValue::Computed(ElementProperty::Object(_) | ElementProperty::Map(_), _) => {
            Cursor::Value(&source.value).step(storage, &key)?
        }
        // A `values(key)` reference to a map-valued property is a map too.
        TraverserValue::VertexProperty(..) | TraverserValue::EdgeProperty(..) => {
            match Cursor::Value(&source.value).resolve(storage)? {
                Some(Cursor::Property(
                    map @ (ElementProperty::Object(_) | ElementProperty::Map(_)),
                )) => Cursor::Property(map).step(storage, &key)?,
                _ => None,
            }
        }
        TraverserValue::Computed(..)
        | TraverserValue::Vertex(_)
        | TraverserValue::Edge(_)
        | TraverserValue::Path(_)
        | TraverserValue::PathProperty(_)
        | TraverserValue::Entry(_)
        | TraverserValue::Array(_)
        | TraverserValue::Schema(_)
        | TraverserValue::Graph(_) => None,
    };
    if let Some(found) = from_map {
        return Ok(Some(found.to_owned_value()));
    }
    if let Some(value) = context.side_effect.get(name) {
        return Ok(Some(value.to_traverser_value()));
    }
    Ok(context.paths.by_label(source.path, name))
}

/// A borrowed position inside a variable's value, for nested access without materializing it.
enum Cursor<'a, S: GraphStorage> {
    Value(&'a TraverserValue<S>),
    Property(&'a ElementProperty),
}

impl<S: GraphStorage> Clone for Cursor<'_, S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<S: GraphStorage> Copy for Cursor<'_, S> {}

/// Why a leaf is not a number.
enum Leaf {
    Null,
    Other(&'static str),
    /// The storage could not read the property.
    Storage(GraphError),
}

/// Normalizes a list index (a negative index counts from the end).
fn list_index(index: i64, len: usize) -> Option<usize> {
    let len = i64::try_from(len).ok()?;
    let i = if index < 0 {
        len.checked_add(index)?
    } else {
        index
    };
    if (0..len).contains(&i) {
        usize::try_from(i).ok()
    } else {
        None
    }
}

impl<'a, S: GraphStorage> Cursor<'a, S> {
    /// Reads a lazy property reference from storage; every other position is itself.
    /// `None` for a reference to a missing property.
    fn resolve(self, storage: &'a S) -> Result<Option<Cursor<'a, S>>, GraphError> {
        Ok(match self {
            Cursor::Value(TraverserValue::VertexProperty(name, id, _)) => storage
                .vertex_properties(*id)?
                .and_then(|properties| properties.get(name.as_str()))
                .map(Cursor::Property),
            Cursor::Value(TraverserValue::EdgeProperty(name, id, _)) => storage
                .edge_properties(*id)?
                .and_then(|properties| properties.get(name.as_str()))
                .map(Cursor::Property),
            Cursor::Value(TraverserValue::Computed(property, _)) => {
                Some(Cursor::Property(property))
            }
            other => Some(other),
        })
    }

    /// One nested-access step; `None` when it leads to no value.
    fn step(self, storage: &'a S, seg: &PathSeg) -> Result<Option<Cursor<'a, S>>, GraphError> {
        let Some(at) = self.resolve(storage)? else {
            return Ok(None);
        };
        Ok(match (at, seg) {
            (Cursor::Value(TraverserValue::Vertex(id)), PathSeg::Key(key)) => storage
                .vertex_properties(*id)?
                .and_then(|properties| properties.get(key.as_str()))
                .map(Cursor::Property),
            (Cursor::Value(TraverserValue::Edge(id)), PathSeg::Key(key)) => storage
                .edge_properties(*id)?
                .and_then(|properties| properties.get(key.as_str()))
                .map(Cursor::Property),
            (at, seg) => at.step_in_value(seg),
        })
    }

    /// [`step`](Self::step) on a resolved position that is not an element.
    fn step_in_value(self, seg: &PathSeg) -> Option<Cursor<'a, S>> {
        match (self, seg) {
            (Cursor::Value(TraverserValue::Object(map)), PathSeg::Key(key)) => {
                let lookup = TraverserValue::Computed(ElementProperty::String(key.clone()), None);
                map.get(&lookup).map(Cursor::Value)
            }
            (Cursor::Value(TraverserValue::Record { keys, values }), PathSeg::Key(key)) => {
                let i = keys.iter().position(|k| k == key)?;
                values.get(i).map(Cursor::Value)
            }
            (Cursor::Value(TraverserValue::Array(items)), PathSeg::Index(index)) => items
                .get(list_index(*index, items.len())?)
                .map(Cursor::Value),
            (
                Cursor::Property(
                    value @ (ElementProperty::Object(_)
                    | ElementProperty::Vertex(_)
                    | ElementProperty::Edge(_)),
                ),
                PathSeg::Key(key),
            ) => value.map_entries()?.get(key.as_str()).map(Cursor::Property),
            // A non-string key is unreachable by path: only string keys are addressed.
            (Cursor::Property(ElementProperty::Map(map)), PathSeg::Key(key)) => {
                map.get_str(key.as_str()).map(Cursor::Property)
            }
            (Cursor::Property(ElementProperty::Array(items)), PathSeg::Index(index)) => items
                .get(list_index(*index, items.len())?)
                .map(Cursor::Property),
            _ => None,
        }
    }

    /// Why [`step`](Self::step) found nothing: the available keys, the list length, or the
    /// kind of value that has neither.
    fn explain(self, storage: &'a S, seg: &PathSeg) -> Result<String, GraphError> {
        let Some(at) = self.resolve(storage)? else {
            return Ok("the property it reads does not exist".to_string());
        };
        let keys: Option<Vec<String>> = match at {
            Cursor::Value(TraverserValue::Object(map)) => Some(
                map.keys()
                    .map(|k| match k {
                        TraverserValue::Computed(key, _) => {
                            crate::value::literal::rhai_literal(key)
                        }
                        other => other.kind_name().to_string(),
                    })
                    .collect(),
            ),
            Cursor::Value(TraverserValue::Record { keys, .. }) => {
                Some(keys.iter().map(|k| k.to_string()).collect())
            }
            Cursor::Value(TraverserValue::Vertex(id)) => Some(
                storage
                    .vertex_properties(*id)?
                    .map_or_else(Vec::new, |p| p.keys().map(|k| k.to_string()).collect()),
            ),
            Cursor::Value(TraverserValue::Edge(id)) => Some(
                storage
                    .edge_properties(*id)?
                    .map_or_else(Vec::new, |p| p.keys().map(|k| k.to_string()).collect()),
            ),
            Cursor::Property(
                value @ (ElementProperty::Object(_)
                | ElementProperty::Vertex(_)
                | ElementProperty::Edge(_)),
            ) => value
                .map_entries()
                .map(|map| map.keys().map(|k| k.to_string()).collect()),
            _ => None,
        };
        let len = match at {
            Cursor::Value(TraverserValue::Array(items)) => Some(items.len()),
            Cursor::Property(ElementProperty::Array(items)) => Some(items.len()),
            _ => None,
        };
        Ok(match (seg, keys, len) {
            (PathSeg::Key(key), Some(keys), _) => {
                format!(
                    "no key '{key}'; the available keys are: {}",
                    keys.join(", ")
                )
            }
            (PathSeg::Index(index), _, Some(len)) => {
                format!("index {index} is out of range for a list of {len} elements")
            }
            (PathSeg::Key(key), None, _) => format!("a {} has no key '{key}'", at.type_name()),
            (PathSeg::Index(index), _, None) => {
                format!("a {} has no index {index}", at.type_name())
            }
        })
    }

    /// The number at this position.
    fn number(self, storage: &'a S) -> Result<f64, Leaf> {
        match self.resolve(storage).map_err(Leaf::Storage)? {
            None | Some(Cursor::Property(ElementProperty::Null)) => Err(Leaf::Null),
            Some(Cursor::Property(ElementProperty::Number(n))) => {
                // Exact for every `f64`; an `i64` converts like Java's `Number.doubleValue()`.
                Ok(n.as_f64()
                    .or_else(|| n.as_i64().map(|i| i as f64))
                    .unwrap_or(f64::NAN))
            }
            Some(other) => Err(Leaf::Other(other.type_name())),
        }
    }

    /// The kind of value at this position, for diagnostics.
    fn type_name(self) -> &'static str {
        match self {
            Cursor::Property(property) => match property {
                ElementProperty::Null => "null",
                ElementProperty::String(_)
                | ElementProperty::Compressed(_)
                | ElementProperty::Deduped(_) => "string",
                ElementProperty::Number(_) => "number",
                ElementProperty::Boolean(_) => "boolean",
                ElementProperty::Uuid(_) => "uuid",
                ElementProperty::Temporal(value) => value.kind().name(),
                ElementProperty::Vector(_) => "vector",
                ElementProperty::Object(_) | ElementProperty::Map(_) => "map",
                ElementProperty::Array(_) => "list",
                ElementProperty::Vertex(_) => "vertex",
                ElementProperty::Edge(_) => "edge",
            },
            Cursor::Value(value) => match value {
                TraverserValue::Computed(..)
                | TraverserValue::VertexProperty(..)
                | TraverserValue::EdgeProperty(..) => "property",
                TraverserValue::Vertex(_) => "vertex",
                TraverserValue::Edge(_) => "edge",
                TraverserValue::Path(_) => "path",
                TraverserValue::Record { .. } | TraverserValue::Object(_) => "map",
                TraverserValue::PathProperty(_) => "path property",
                TraverserValue::Entry(_) => "map entry",
                TraverserValue::Array(_) => "list",
                TraverserValue::Schema(_) => "schema",
                TraverserValue::Graph(_) => "graph",
            },
        }
    }

    /// An owned copy of the value at this position.
    fn to_owned_value(self) -> TraverserValue<S> {
        match self {
            Cursor::Value(value) => value.clone(),
            Cursor::Property(property) => TraverserValue::from(property),
        }
    }
}

/// Renders a variable with its nested-access path as it can be written: `_.price.total`,
/// `a.items[0]`, `_["unit price"]`.
fn render_path(name: &str, path: &[PathSeg]) -> String {
    let mut out = name.to_string();
    for seg in path {
        match seg {
            PathSeg::Key(key) => {
                let identifier = key
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                    && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
                if identifier {
                    out.push('.');
                    out.push_str(key);
                } else {
                    out.push_str("[\"");
                    out.push_str(&key.replace('\\', "\\\\").replace('"', "\\\""));
                    out.push_str("\"]");
                }
            }
            PathSeg::Index(index) => out.push_str(&format!("[{index}]")),
        }
    }
    out
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepMath<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut by = "".to_string();
        if let Some(by_list) = self.by.as_ref() {
            by_list
                .iter()
                .for_each(|b| by.push_str(&crate::traversal::render_by(b)));
        }
        write!(
            f,
            "math({}){}",
            crate::value::literal::rhai_string(&self.expr),
            by
        )
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Evaluates the exp4j equation `expr` for every traverser (TinkerPop's `math()`); the
    /// result is always a double. `_` is the current value; any other variable names a key of
    /// the current map, a side effect or a step label, and the `by()` modulators apply
    /// round-robin to the distinct variables in order of first appearance. Graphersal extension:
    /// nested access after a variable, such as `_.price.total` or `a.items[0]`.
    pub fn math(self, expr: &str) -> Self {
        self.push_step(GraphStep::Map("math", Box::new(StepMath::new(expr))))
    }
}

impl __ {
    /// Anonymous [`GraphTraversalSource::math`].
    pub fn math<'g, S: GraphStorage + 'g>(expr: &str) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().math(expr)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// See [`GraphTraversalSource::math`].
    pub fn math(self, expr: &str) -> Self {
        let step = GraphStep::Map("math", Box::new(StepMath::new(expr)));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepMath<'g, S> {
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        // Only reached without `by()` (`may_filter()` is false), where nothing is filtered.
        self.process_filter(context, source)?.ok_or_else(|| {
            Box::new(TraverserError::QueryExecution {
                query: format!("math(\"{}\")", self.expr),
                reason: "math() dropped a traverser without a by() modulator".to_string(),
            })
        })
    }

    /// `math()` applies its `by()` modulators as TinkerPop's traversal ring: round-robin over the
    /// equation's distinct variables in order of first appearance, so one `by()` applies to every
    /// variable and `math("a + b + c").by("age").by(__.out().count())` reads a: age,
    /// b: out-count, c: age. `_` is the current traverser (its `by()` runs on it, path
    /// included); any other variable is, in TinkerPop's `Scoping` order, a key of the current
    /// map, a side-effect key or a path label (its `by()` runs on that object alone). A
    /// traverser for which any `by()` is unproductive (such as `by("age")` on a vertex without
    /// `age`) is filtered out, TinkerPop 3.6+'s "productive `by()`" rule for `math()`. A nested
    /// path (`_.price.total`) is applied after the variable's `by()`.
    fn process_filter(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Option<Traverser<S>>> {
        self.by_check.check("math")?;
        let expr = match &self.parsed {
            Ok(expr) => expr,
            Err(err) => return Err(self.parse_error(err)),
        };
        let count = expr.variables().len();
        let value = if count <= INLINE_SLOTS {
            let mut slots: [Option<Slot<'_, S>>; INLINE_SLOTS] = std::array::from_fn(|_| None);
            let slots = slots.get_mut(..count).unwrap_or_default();
            self.evaluate(context, &source, expr, slots)?
        } else {
            let mut slots: Vec<Option<Slot<'_, S>>> = (0..count).map(|_| None).collect();
            self.evaluate(context, &source, expr, &mut slots)?
        };
        Ok(value.map(|value| {
            Traverser::with_parts(
                TraverserValue::Computed(ElementProperty::Number(Number::from_f64(value)), None),
                source.path,
                source.bulk,
                source.sack,
            )
        }))
    }

    fn may_filter(&self) -> bool {
        self.by.as_ref().is_some_and(|by| !by.is_empty())
    }

    fn push_modulator_by(&mut self, args: By<'g, S>, _modulator_name: &str) -> ByModulation {
        self.by_check.observe_ring(&args);
        if let Some(x) = self.by.as_mut() {
            x.push(args);
        } else {
            self.by = Some(vec![args]);
        }
        ByModulation::Accepted
    }

    fn path_requirement(&self) -> PathRequirement {
        // Every variable other than `_` (the current object) may be a step label.
        match &self.parsed {
            Ok(expr) if expr.variables().iter().any(|name| name.as_str() != CURRENT) => {
                PathRequirement::labels(
                    expr.variables()
                        .iter()
                        .filter(|name| name.as_str() != CURRENT)
                        .map(|name| name.as_str()),
                )
            }
            Ok(_) | Err(_) => PathRequirement::None,
        }
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &PathAnalysis,
        _downstream: &PathRequirement,
    ) -> PathRequirement {
        match &mut self.by {
            Some(by_list) => By::analyze_all_paths(by_list, analysis),
            None => PathRequirement::None,
        }
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut crate::traversal::AnonymousTraversal<'g, S>> {
        self.by
            .iter_mut()
            .flatten()
            .filter_map(By::traversal_mut)
            .collect()
    }

    fn plan_check(&self) -> TraverserResult<()> {
        match &self.parsed {
            Ok(_) => Ok(()),
            Err(err) => Err(self.parse_error(err)),
        }
    }

    fn clone_box(&self) -> Box<dyn GraphStepMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}
