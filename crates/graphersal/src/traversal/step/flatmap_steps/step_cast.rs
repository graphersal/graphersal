use compact_str::CompactString;

use crate::graph::GraphStorage;
use crate::traversal::step::barrier_steps::numeric::{ValueSource, preview};
use crate::traversal::step::local_scope::{LocalItem, LocalMapped, is_null, map_local_elements};
use crate::value::Scope;
use crate::*;

/// What a coercion step converts to.
///
/// Resolved once, when the step is built, so the conversion matrix is consulted with a concrete
/// [`ValueType`] and a token that names no target fails with a message instead of silently doing
/// nothing.
#[derive(Clone, Debug)]
enum CastTarget {
    /// A concrete property type.
    ValueType(ValueType),
    /// The natural numeric type of the value — vanilla `asNumber()` with no argument.
    Natural,
    /// Vanilla `asBool()`: any number is truthy except `0` and `NaN`, and the texts `true` and
    /// `false` convert in any letter case. Deliberately wider than the strict coercion matrix.
    Truthy,
    /// Vanilla `asNumber(GType.INT | GType.LONG)`: a float truncates toward zero. Deliberately
    /// wider than the strict coercion matrix, which refuses a lossy narrowing.
    Integer,
    /// A token this step cannot convert to.
    Rejected(GType, &'static str),
}

impl CastTarget {
    /// Resolves a conversion target for `cast()`, which accepts every value type.
    fn general(target: GType) -> Self {
        match target {
            GType::Value(declared) => CastTarget::ValueType(declared),
            GType::Number => CastTarget::Natural,
            other => CastTarget::Rejected(
                other,
                "this token names a family or a graph element, not a single conversion target",
            ),
        }
    }

    /// Resolves a conversion target for `as_number()`, which only accepts a numeric one.
    fn numeric(target: Option<GType>) -> Self {
        match target {
            None | Some(GType::Number) => CastTarget::Natural,
            Some(GType::Value(ValueType::Int64)) => CastTarget::Integer,
            Some(GType::Value(ValueType::Float64)) => CastTarget::ValueType(ValueType::Float64),
            Some(other) => CastTarget::Rejected(
                other,
                "as_number() converts to a number; use as_string(), as_bool() or cast() instead",
            ),
        }
    }

    /// Renders the argument as the query wrote it, for `.profile()`.
    fn rendered(&self) -> Option<String> {
        match self {
            CastTarget::ValueType(declared) => Some(GType::Value(declared.clone()).token()),
            CastTarget::Natural | CastTarget::Truthy => None,
            CastTarget::Integer => Some(GType::LONG.token()),
            CastTarget::Rejected(token, _) => Some(token.token()),
        }
    }
}

/// The single coercion primitive behind `as_string()`, `as_bool()`, `as_number()` and `cast()`.
///
/// Classified as a `FlatMap` rather than a `Map` although it is 1-to-1 in the common case:
/// [`CastPolicy::Skip`] drops a value from the stream, which a `Map` step cannot express, and a
/// `Map` step is additionally short-circuited by a downstream `count()` — which would silently
/// skip the conversion and its errors.
///
/// **Null handling (user's guide: "Type Conversion and length()"):** `as_string()` and `as_bool()`
/// raise a cast error on `null` (TinkerPop: "Can't parse null"), while `as_number()` and the
/// Graphersal extension `cast()` keep `null` as `null`. This follows TinkerPop's per-step
/// behaviour; it is not uniform across the family, and `CastPolicy` still applies to the error.
#[derive(Clone)]
struct StepCast {
    /// The alias spelling the query used, so `.profile()` names the step as written (rule 11).
    name: CompactString,
    target: CastTarget,
    policy: CastPolicy,
    /// `as_string()` and `as_bool()` raise on `null`; `as_number()` and `cast()` keep it `null`.
    rejects_null: bool,
    /// `Scope::Local` converts each element of the list a traverser holds (`as_string(local)`).
    scope: Scope,
}

impl fmt::Display for StepCast {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
    }
}

/// Builds the step together with the name `.profile()` will show.
///
/// `render_target` is false for the aliases whose name already fixes the target: vanilla writes
/// `asString()`, never `asString(string)`, and the plan has to read like the query.
fn cast_step<'g, S: GraphStorage + 'g>(
    alias: &'static str,
    target: CastTarget,
    policy: CastPolicy,
    render_target: bool,
    scope: Scope,
) -> GraphStep<'g, S> {
    let mut args: Vec<String> = Vec::new();
    if scope == Scope::Local {
        args.push("Scope.local".to_string());
    }
    if render_target && let Some(rendered) = target.rendered() {
        args.push(rendered);
    }
    match &policy {
        CastPolicy::Error => {}
        CastPolicy::Skip => args.push("CastPolicy.Skip".to_string()),
        CastPolicy::Default(value) => args.push(format!(
            "CastPolicy.Default({})",
            crate::value::literal::rhai_literal(value)
        )),
    }
    let name = CompactString::new(format!("{alias}({})", args.join(", ")));
    GraphStep::FlatMap(
        alias,
        Box::new(StepCast {
            name,
            target,
            policy,
            rejects_null: matches!(alias, "as_string" | "as_bool"),
            scope,
        }),
    )
}

macro_rules! cast_builders {
    ($target:ty) => {
        /// Converts the stream value to a string — vanilla `asString()`.
        pub fn as_string(self) -> Self {
            self.as_string_with(CastPolicy::default())
        }

        /// Converts the stream value to a string, deciding what happens on failure.
        pub fn as_string_with(self, policy: CastPolicy) -> Self {
            self.push_step(cast_step(
                "as_string",
                CastTarget::ValueType(ValueType::String),
                policy,
                false,
                Scope::Global,
            ))
        }

        /// `as_string(Scope)`: `Scope::Global` is `as_string()`; `Scope::Local` converts each
        /// element of the list a traverser holds to a string
        /// (`g.V().hasLabel("person").values("age").order().fold().as_string(Scope.local)` →
        /// `["27", "29", "32", "35"]`) and fails on a `null` element; any other value converts
        /// as by `as_string()`.
        pub fn as_string_scoped(self, scope: Scope) -> Self {
            self.push_step(cast_step(
                "as_string",
                CastTarget::ValueType(ValueType::String),
                CastPolicy::default(),
                false,
                scope,
            ))
        }

        /// Converts the stream value to a boolean — vanilla `asBool()`.
        ///
        /// A number is `false` for `0`, `-0.0` and `NaN` and `true` for every other number
        /// (infinities included); the texts `true` and `false` convert in any ASCII letter case;
        /// any other text, a `null`, a uuid or a collection is an error.
        ///
        /// **Deviation:** only those two literals convert from text (no `"1"`/`"yes"`), because a
        /// string is never interpreted by its content. The schema coercion matrix and `cast()`
        /// stay strict (`2` and `"TRUE"` do not cast to a boolean); see the mdBook page
        /// "Type conversion".
        pub fn as_bool(self) -> Self {
            self.as_bool_with(CastPolicy::default())
        }

        /// Converts the stream value to a boolean, deciding what happens on failure.
        pub fn as_bool_with(self, policy: CastPolicy) -> Self {
            self.push_step(cast_step(
                "as_bool",
                CastTarget::Truthy,
                policy,
                false,
                Scope::Global,
            ))
        }

        /// Converts the stream value to a number — vanilla `asNumber()` and `asNumber(GType)`.
        ///
        /// `as_number(None)` keeps the natural numeric type of each value; passing
        /// `GType::LONG` or `GType::DOUBLE` pins the result type.
        ///
        /// `GType::INT` and `GType::LONG` are the same `int64` target and truncate a float toward
        /// zero (`5.67` becomes `5`, `-5.67` becomes `-5`), as TinkerPop does.
        ///
        /// **Deviation:** a float that is `NaN`, infinite or outside the `int64` range is an error
        /// (TinkerPop's `Integer` target would wrap or narrow to its own width; Graphersal has one
        /// integer width). A string converts only when it parses as a whole integer (`"5"`, not
        /// `"5.7"`). `cast(GType.INT)` and the schema coercion matrix stay strict and refuse
        /// `5.43`.
        pub fn as_number<T: Into<Option<GType>>>(self, target: T) -> Self {
            self.as_number_with(target, CastPolicy::default())
        }

        /// Converts the stream value to a number, deciding what happens on failure.
        pub fn as_number_with<T: Into<Option<GType>>>(self, target: T, policy: CastPolicy) -> Self {
            self.push_step(cast_step(
                "as_number",
                CastTarget::numeric(target.into()),
                policy,
                true,
                Scope::Global,
            ))
        }

        /// Converts the stream value to any declared type — the Graphersal extension.
        ///
        /// Takes what the vanilla steps cannot name, including a [`ValueType`] straight from a schema.
        pub fn cast<T: Into<GType>>(self, target: T) -> Self {
            self.cast_with(target, CastPolicy::default())
        }

        /// Converts the stream value to any declared type, deciding what happens on failure.
        pub fn cast_with<T: Into<GType>>(self, target: T, policy: CastPolicy) -> Self {
            self.push_step(cast_step(
                "cast",
                CastTarget::general(target.into()),
                policy,
                true,
                Scope::Global,
            ))
        }
    };
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    cast_builders!(GraphTraversalSource);
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    cast_builders!(AnonymousTraversal);
}

impl __ {
    /// Anonymous `as_string()`.
    pub fn as_string<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().as_string()
    }

    /// Anonymous `as_string(Scope)`.
    pub fn as_string_scoped<'g, S: GraphStorage + 'g>(scope: Scope) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().as_string_scoped(scope)
    }

    /// Anonymous `as_bool()`.
    pub fn as_bool<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().as_bool()
    }

    /// Anonymous `as_number()`.
    pub fn as_number<'g, S: GraphStorage + 'g, T: Into<Option<GType>>>(
        target: T,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().as_number(target)
    }

    /// Anonymous `cast()`.
    pub fn cast<'g, S: GraphStorage + 'g, T: Into<GType>>(target: T) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().cast(target)
    }
}

/// Converts a value to its natural numeric type: an integer stays an integer, and a string
/// becomes one only if it parses as one whole.
fn to_natural_number(value: &ElementProperty) -> TraverserResult<ElementProperty> {
    match value {
        ElementProperty::Number(_) | ElementProperty::Null => Ok(value.clone()),
        ElementProperty::Boolean(_) => value.cast(&ValueType::Int64),
        // TinkerPop `asNumber()` of a date: its epoch milliseconds (`int64`).
        ElementProperty::Temporal(crate::value::temporal::Temporal::DateTime(_)) => {
            value.cast(&ValueType::Int64)
        }
        ElementProperty::String(text) => {
            if text.parse::<i64>().is_ok() {
                value.cast(&ValueType::Int64)
            } else {
                value.cast(&ValueType::Float64)
            }
        }
        // A container carried as a computed value: report the family that was expected rather
        // than whichever concrete numeric type happened to be tried first.
        other => Err(Box::new(TraverserError::cast_exception_target(
            "number",
            crate::graph::schema::value_type_name(other),
            Some(preview(other)),
            None,
        ))),
    }
}

/// `asBool()`: `0`/`-0.0`/`NaN` are false, every other number true, `true`/`false` text converts
/// in any ASCII letter case; everything else fails with the strict matrix's cast error.
fn truthy(value: &ElementProperty) -> TraverserResult<ElementProperty> {
    match value {
        ElementProperty::Boolean(_) => Ok(value.clone()),
        ElementProperty::Number(number) => {
            let falsy = match (number.as_i64(), number.as_f64()) {
                (Some(int), _) => int == 0,
                (None, Some(float)) => float == 0.0 || float.is_nan(),
                (None, None) => false,
            };
            Ok(ElementProperty::Boolean(!falsy))
        }
        ElementProperty::String(text) if text.eq_ignore_ascii_case("true") => {
            Ok(ElementProperty::Boolean(true))
        }
        ElementProperty::String(text) if text.eq_ignore_ascii_case("false") => {
            Ok(ElementProperty::Boolean(false))
        }
        // Null, any other text, a uuid: the strict matrix refuses all of them.
        other => other.cast(&ValueType::Boolean),
    }
}

/// `asNumber(GType.INT | GType.LONG)`: a float truncates toward zero when the result fits `i64`;
/// text and booleans keep the strict conversion; `null` stays `null`.
fn truncate_to_int64(value: &ElementProperty) -> TraverserResult<ElementProperty> {
    match value {
        ElementProperty::Number(number) => match (number.as_i64(), number.as_f64()) {
            (Some(_), _) => Ok(value.clone()),
            (None, Some(float))
                if float.is_finite()
                    && (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0)
                        .contains(&float) =>
            {
                Ok(ElementProperty::Number(Number::from_i64(
                    float.trunc() as i64
                )))
            }
            _ => value.cast(&ValueType::Int64),
        },
        ElementProperty::Null => Ok(ElementProperty::Null),
        other => other.cast(&ValueType::Int64),
    }
}

impl StepCast {
    /// `as_string()` / `as_bool()` raise on `null`; `as_number()` and `cast()` keep it `null`.
    fn rejects_null(&self) -> bool {
        self.rejects_null
    }

    /// Converts one value. `Ok(None)` means [`CastPolicy::Skip`] dropped it.
    fn convert<S: GraphStorage>(
        &self,
        context: &TraverserContext<'_, S>,
        source: &TraverserValue<S>,
    ) -> TraverserResult<Option<ElementProperty>> {
        // `as_string()` of a graph element or a property element is its TinkerPop `toString`
        // form; every other target keeps rejecting them.
        if matches!(self.target, CastTarget::ValueType(ValueType::String))
            && let Some(text) = element_text(context, source)?
        {
            return Ok(Some(bounded_string(context, text)?));
        }
        let (value, origin) = read_value(context, source)?;

        let converted = match (&value, &self.target) {
            // A graph element, a path and a container never convert to a scalar; they are
            // reported in the stream vocabulary they are carried in, not as the shape they
            // would materialize into.
            (None, target) => Err(Box::new(TraverserError::cast_exception_target(
                target_name(target),
                source.type_name().to_string(),
                None,
                None,
            ))),
            // TinkerPop: `asString()` and `asBool()` reject a `null` ("Can't parse null"), while
            // `asNumber()` and the `cast()` extension keep it `null`.
            (
                Some(ElementProperty::Null),
                CastTarget::ValueType(ValueType::String) | CastTarget::Truthy,
            ) if self.rejects_null() => Err(Box::new(TraverserError::cast_exception_target(
                target_name(&self.target),
                "null".to_string(),
                None,
                None,
            ))),
            (Some(value), CastTarget::Natural) => to_natural_number(value),
            (Some(value), CastTarget::Truthy) => truthy(value),
            (Some(value), CastTarget::Integer) => truncate_to_int64(value),
            (Some(value), CastTarget::ValueType(declared)) => value.cast(declared),
            // Rejected targets already returned in `process`; report them the same way if reached.
            (Some(_), CastTarget::Rejected(token, reason)) => Err(Box::new(
                TraverserError::unsupported_cast_target(token.to_string(), reason),
            )),
        };

        match converted {
            // `as_string()` of a container renders it: bounded by `traversal.max_string_bytes`.
            Ok(converted) => Ok(Some(bounded_string(context, converted)?)),
            // An unsupported target is never the value's fault, so the policy does not apply.
            Err(err)
                if matches!(
                    *err,
                    TraverserError::Value(ValueError::UnsupportedCastTarget { .. })
                ) =>
            {
                Err(err)
            }
            Err(err) => match &self.policy {
                CastPolicy::Error => Err(Box::new(enrich(*err, context, value.as_ref(), &origin))),
                CastPolicy::Skip => Ok(None),
                CastPolicy::Default(fallback) => Ok(Some(fallback.clone())),
            },
        }
    }

    /// Converts one element of the list `as_string(local)` works on. A `null` element has no
    /// string form and is an error (TinkerPop: `Can't parse`); every other element converts as
    /// the single-value step converts it.
    fn convert_element<S: GraphStorage>(
        &self,
        context: &TraverserContext<'_, S>,
        index: usize,
        item: LocalItem<'_, S>,
    ) -> TraverserResult<ElementProperty> {
        let null_element = || {
            Box::new(TraverserError::local_element_type(
                "asString",
                "non-null elements (Can't parse null as a string)",
                index,
                "null".to_string(),
            ))
        };
        let converted = match item {
            LocalItem::Stored(ElementProperty::Null) => return Err(null_element()),
            LocalItem::Stored(stored) => {
                self.convert(context, &TraverserValue::Computed(stored.clone(), None))?
            }
            LocalItem::Value(value) => {
                if is_null(context, value)? {
                    return Err(null_element());
                }
                self.convert(context, value)?
            }
        };
        // The local form always runs with `CastPolicy::Error`, so nothing is skipped; a skip
        // keeps the element's slot as `null` rather than shifting the list.
        Ok(converted.unwrap_or(ElementProperty::Null))
    }
}

use crate::traversal::step::local_scope::bounded_string;

impl<'g, S: GraphStorage + 'g> GraphStepFlatMap<'g, S> for StepCast {
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
        output: &mut Vec<Traverser<S>>,
    ) -> TraverserResult<()> {
        // A target that names nothing is a query-authoring mistake, not a bad value: it is
        // reported whatever the policy says, so an unimplemented target can never pass a value
        // through unchanged.
        if let CastTarget::Rejected(token, reason) = &self.target {
            return Err(Box::new(TraverserError::unsupported_cast_target(
                token.to_string(),
                reason,
            )));
        }

        let path = source.path;
        let bulk = source.bulk;
        let sack = source.sack;
        let value = match self.scope {
            Scope::Global => source.value,
            Scope::Local => {
                let context: &TraverserContext<'_, S> = context;
                match map_local_elements(context, source.value, |index, item| {
                    self.convert_element(context, index, item)
                })? {
                    LocalMapped::Done(list) => {
                        output.push(Traverser::with_parts(list, path, bulk, sack));
                        return Ok(());
                    }
                    LocalMapped::Single(value) => value,
                }
            }
        };

        if let Some(converted) = self.convert(context, &value)? {
            output.push(Traverser::with_parts(
                TraverserValue::Computed(converted, None),
                path,
                bulk,
                sack,
            ));
        }
        Ok(())
    }

    fn materializes(&self) -> bool {
        self.scope == Scope::Local
    }

    fn clone_box(&self) -> Box<dyn GraphStepFlatMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

/// The TinkerPop `toString` form of a graph element or property element, or `None` for any other
/// value (a property handle that stands for a plain value included).
///
/// `v[<id>]`, `e[<id>][<out id>-<label>-><in id>]`, `vp[<key>-><value>]` and `p[<key>-><value>]`,
/// where the value text is the ordinary string cast of the stored value (`29`, not `29.0`).
pub(crate) fn element_text<S: GraphStorage>(
    context: &TraverserContext<'_, S>,
    source: &TraverserValue<S>,
) -> TraverserResult<Option<ElementProperty>> {
    let storage = &*context.storage;
    let text = match source {
        TraverserValue::Vertex(vertex) => {
            format!(
                "v[{}]",
                crate::graph::storage::stored_vertex_id(storage, *vertex)?
            )
        }
        TraverserValue::Edge(edge) => {
            let id = crate::graph::storage::stored_edge_id(storage, *edge)?;
            let label = storage.edge_label(*edge)?.unwrap_or_default();
            match storage.edge_endpoints(*edge)? {
                Some((out, into)) => format!(
                    "e[{id}][{}-{label}->{}]",
                    crate::graph::storage::stored_vertex_id(storage, out)?,
                    crate::graph::storage::stored_vertex_id(storage, into)?
                ),
                None => format!("e[{id}]"),
            }
        }
        TraverserValue::PathProperty(prop) => {
            format!("p[{}->{}]", prop.0, property_text(Some(&prop.1))?)
        }
        TraverserValue::VertexProperty(key, vertex, PropertyHandleKind::Element) => {
            let stored = storage
                .vertex_properties(*vertex)?
                .and_then(|p| p.get(key.as_str()));
            format!("vp[{key}->{}]", property_text(stored)?)
        }
        TraverserValue::EdgeProperty(key, edge, PropertyHandleKind::Element) => {
            let stored = storage
                .edge_properties(*edge)?
                .and_then(|p| p.get(key.as_str()));
            format!("p[{key}->{}]", property_text(stored)?)
        }
        _ => return Ok(None),
    };
    Ok(Some(ElementProperty::from(text)))
}

/// The value text inside `vp[..]`/`p[..]`: the string cast of a scalar, `null` for a missing or
/// null value. A list or map value has no string form and is a cast failure like anywhere else.
fn property_text(stored: Option<&ElementProperty>) -> TraverserResult<String> {
    match stored {
        None | Some(ElementProperty::Null) => Ok("null".to_string()),
        Some(value) => match value.cast(&ValueType::String)? {
            ElementProperty::String(text) => Ok(text.to_string()),
            other => Err(Box::new(TraverserError::cast_exception_target(
                "string",
                crate::graph::schema::value_type_name(&other),
                None,
                None,
            ))),
        },
    }
}

/// Renders a conversion target for a diagnostic.
fn target_name(target: &CastTarget) -> String {
    match target {
        CastTarget::ValueType(declared) => declared.to_string(),
        CastTarget::Natural => "number".to_string(),
        CastTarget::Truthy => "boolean".to_string(),
        CastTarget::Integer => "int64".to_string(),
        CastTarget::Rejected(token, _) => token.to_string(),
    }
}

/// Reads the value to convert together with a lazy reference to the element it came from.
///
/// Returns `None` for a stream value that carries no property value at all — a graph element, a
/// path or a container. Those never convert to a scalar, and materializing one only to fail
/// would cost a full property map per stream item (rule 9).
fn read_value<S: GraphStorage>(
    context: &TraverserContext<'_, S>,
    source: &TraverserValue<S>,
) -> TraverserResult<(Option<ElementProperty>, ValueSource<S>)> {
    match source {
        TraverserValue::Computed(value, _) => Ok((Some(value.clone()), ValueSource::Anonymous)),

        TraverserValue::VertexProperty(prop, vertex_index, kind) => {
            let origin = ValueSource::Vertex(*vertex_index, Some(prop.clone()));
            // `id` and `label` are not stored as properties; every step that reads them renders
            // them as strings.
            if !kind.reads_property() {
                return Ok((Some(source.into_node_property(context.storage)?), origin));
            }
            let value = context
                .storage
                .vertex_properties(*vertex_index)?
                .and_then(|p| p.get(prop.as_str()))
                .map(ElementProperty::to_logical)
                .unwrap_or(ElementProperty::Null);
            Ok((Some(value), origin))
        }

        TraverserValue::EdgeProperty(prop, edge_index, kind) => {
            let origin = ValueSource::Edge(*edge_index, Some(prop.clone()));
            if !kind.reads_property() {
                return Ok((Some(source.into_node_property(context.storage)?), origin));
            }
            let value = context
                .storage
                .edge_properties(*edge_index)?
                .and_then(|p| p.get(prop.as_str()))
                .map(ElementProperty::to_logical)
                .unwrap_or(ElementProperty::Null);
            Ok((Some(value), origin))
        }

        TraverserValue::Vertex(vertex_index) => {
            Ok((None, ValueSource::Vertex(*vertex_index, None)))
        }
        TraverserValue::Edge(edge_index) => Ok((None, ValueSource::Edge(*edge_index, None))),
        TraverserValue::Path(_)
        | TraverserValue::Array(_)
        | TraverserValue::Object(_)
        | TraverserValue::Entry(_)
        | TraverserValue::Record { .. }
        | TraverserValue::Schema(_)
        | TraverserValue::Graph(_) => Ok((None, ValueSource::Anonymous)),
        TraverserValue::PathProperty(prop) => Ok((Some(prop.1.clone()), ValueSource::Anonymous)),
    }
}

/// Attaches the offending value and its element to a conversion failure.
fn enrich<S: GraphStorage>(
    err: TraverserError,
    context: &TraverserContext<'_, S>,
    value: Option<&ElementProperty>,
    origin: &ValueSource<S>,
) -> TraverserError {
    match err {
        TraverserError::Value(ValueError::CastException {
            expected, actual, ..
        }) => match origin.origin(context) {
            Ok(origin) => {
                TraverserError::cast_exception_target(expected, actual, value.map(preview), origin)
            }
            Err(error) => error.into(),
        },
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modern() -> Graph {
        GraphSource::tinkerpop_modern()
    }

    /// The modern graph with one `age` stored as a string, reproducing the mixed-type field the
    /// coercion steps exist to repair.
    fn modern_with_string_age() -> Graph {
        let graph = GraphSource::tinkerpop_modern();
        {
            let mut writable = graph.write();
            let _ = writable
                .traversal_mut()
                .v("4")
                .property("age", "999")
                .next();
        }
        graph
    }

    #[test]
    fn as_string_renders_every_scalar() {
        let graph = modern();
        let locked = graph.read();
        let mut ages = locked
            .traversal()
            .v(None)
            .has_label("person")
            .values("age")
            .as_string()
            .to_list()
            .unwrap();
        ages.sort_by_key(|value| format!("{value:?}"));
        assert_eq!(
            ages,
            vec![
                ElementProperty::from("27"),
                ElementProperty::from("29"),
                ElementProperty::from("32"),
                ElementProperty::from("35"),
            ]
        );
    }

    #[test]
    fn as_number_normalizes_a_mixed_field() {
        let graph = modern_with_string_age();
        let locked = graph.read();
        let max = locked
            .traversal()
            .v(None)
            .has_label("person")
            .values("age")
            .as_number(GType::LONG)
            .max()
            .next()
            .unwrap();
        assert_eq!(max, Some(ElementProperty::from(999i64)));
    }

    #[test]
    fn as_number_without_a_target_keeps_the_natural_type() {
        let graph = modern();
        let locked = graph.read();
        let values = locked
            .traversal()
            .inject(vec!["12", "1.5"])
            .as_number(None)
            .to_list()
            .unwrap();
        assert_eq!(
            values,
            vec![ElementProperty::from(12i64), ElementProperty::from(1.5f64)]
        );
    }

    #[test]
    fn as_bool_takes_the_exact_literals() {
        let graph = modern();
        let locked = graph.read();
        let values = locked
            .traversal()
            .inject(vec!["true", "false"])
            .as_bool()
            .to_list()
            .unwrap();
        assert_eq!(
            values,
            vec![
                ElementProperty::Boolean(true),
                ElementProperty::Boolean(false)
            ]
        );
    }

    #[test]
    fn the_default_policy_fails_and_names_the_offending_element() {
        let graph = modern_with_string_age();
        let locked = graph.read();
        let err = locked
            .traversal()
            .v(None)
            .has_label("person")
            .values("age")
            .as_bool()
            .to_list()
            .expect_err("an age is not a boolean");
        let report = crate::error::format_diagnostic(&err);
        assert!(report.contains("boolean"), "missing target in: {report}");
        assert!(
            report.contains("origin: vertex"),
            "missing origin in: {report}"
        );
    }

    #[test]
    fn skip_drops_the_values_that_do_not_convert() {
        let graph = modern_with_string_age();
        let locked = graph.read();
        let values = locked
            .traversal()
            .v(None)
            .has_label("person")
            .values("name")
            .as_number_with(GType::LONG, CastPolicy::Skip)
            .to_list()
            .unwrap();
        assert!(values.is_empty(), "no name parses as a number: {values:?}");
    }

    #[test]
    fn a_default_replaces_the_values_that_do_not_convert() {
        let graph = modern();
        let locked = graph.read();
        let values = locked
            .traversal()
            .inject(vec!["12", "nope"])
            .as_number_with(
                GType::LONG,
                CastPolicy::Default(ElementProperty::from(0i64)),
            )
            .to_list()
            .unwrap();
        assert_eq!(
            values,
            vec![ElementProperty::from(12i64), ElementProperty::from(0i64)]
        );
    }

    #[test]
    fn an_unsupported_target_fails_whatever_the_policy_says() {
        for policy in [
            CastPolicy::Error,
            CastPolicy::Skip,
            CastPolicy::Default(ElementProperty::from("fallback")),
        ] {
            let graph = modern();
            let locked = graph.read();
            let err = locked
                .traversal()
                .inject(vec!["x"])
                .cast_with(
                    GType::from(ValueType::Array(Box::new(ValueType::String))),
                    policy.clone(),
                )
                .to_list()
                .expect_err("an unsupported target must never pass the value through");
            assert!(
                crate::error::format_diagnostic(&err).contains("array<string>"),
                "the target must be named for policy {policy:?}"
            );
        }
    }

    /// Unlike a truly unimplemented target, a UUID cast is a real conversion now: a malformed
    /// value is a per-value `CastException`, so `CastPolicy` governs it like any other target.
    #[test]
    fn a_uuid_cast_failure_honours_the_policy() {
        let graph = modern();
        let locked = graph.read();

        let skipped = locked
            .traversal()
            .inject(vec!["x"])
            .cast_with(GType::UUID, CastPolicy::Skip)
            .to_list()
            .unwrap();
        assert!(skipped.is_empty());

        let defaulted = locked
            .traversal()
            .inject(vec!["x"])
            .cast_with(
                GType::UUID,
                CastPolicy::Default(ElementProperty::from("fallback")),
            )
            .to_list()
            .unwrap();
        assert_eq!(defaulted, vec![ElementProperty::from("fallback")]);

        let err = locked
            .traversal()
            .inject(vec!["x"])
            .cast_with(GType::UUID, CastPolicy::Error)
            .to_list()
            .expect_err("a malformed UUID must fail under the default policy");
        assert!(crate::error::format_diagnostic(&err).contains("uuid"));
    }

    #[test]
    fn as_number_rejects_a_non_numeric_token() {
        let graph = modern();
        let locked = graph.read();
        let err = locked
            .traversal()
            .inject(vec!["x"])
            .as_number(GType::STRING)
            .to_list()
            .expect_err("as_number() converts to a number");
        assert!(crate::error::format_diagnostic(&err).contains("as_number()"));
    }

    #[test]
    fn a_graph_element_only_converts_to_its_string_form() {
        let graph = modern();
        let locked = graph.read();
        let texts = locked
            .traversal()
            .v(None)
            .limit(1)
            .as_string()
            .to_list()
            .unwrap();
        assert_eq!(texts, vec![ElementProperty::from("v[1]")]);
        let err = locked
            .traversal()
            .v(None)
            .as_bool()
            .to_list()
            .expect_err("a vertex is not a boolean");
        assert!(
            crate::error::format_diagnostic(&err).contains("got type 'vertex'"),
            "a vertex must be reported as a vertex"
        );
    }

    #[test]
    fn profile_names_the_step_as_written() {
        let graph = modern();
        let locked = graph.read();

        let plan = locked
            .traversal()
            .v(None)
            .has_label("person")
            .values("age")
            .as_number(GType::LONG)
            .max()
            .profile()
            .unwrap()
            .to_string();
        assert!(
            plan.contains("as_number(GType.LONG)"),
            "the alias and its argument must survive optimization: {plan}"
        );

        let plan = locked
            .traversal()
            .inject(vec!["1"])
            .as_string()
            .profile()
            .unwrap()
            .to_string();
        assert!(plan.contains("as_string()"), "plan: {plan}");

        let plan = locked
            .traversal()
            .inject(vec!["1"])
            .as_number_with(None, CastPolicy::Skip)
            .profile()
            .unwrap()
            .to_string();
        assert!(plan.contains("as_number(CastPolicy.Skip)"), "plan: {plan}");
    }

    #[test]
    fn count_does_not_skip_the_conversion() {
        let graph = modern_with_string_age();
        let locked = graph.read();
        // A `Map` step is short-circuited by a downstream count(); a coercion must not be, or a
        // skipped value would still be counted and a failure would go unreported.
        let counted = locked
            .traversal()
            .v(None)
            .has_label("person")
            .values("name")
            .as_number_with(GType::LONG, CastPolicy::Skip)
            .count()
            .next()
            .unwrap();
        assert_eq!(counted, Some(ElementProperty::from(0i64)));
    }

    #[test]
    fn the_anonymous_form_is_available() {
        let graph = modern();
        let locked = graph.read();
        let values = locked
            .traversal()
            .inject(vec!["12"])
            .where_t(__::as_number::<_, GType>(GType::LONG))
            .to_list()
            .unwrap();
        assert_eq!(values, vec![ElementProperty::from("12")]);
    }
}
