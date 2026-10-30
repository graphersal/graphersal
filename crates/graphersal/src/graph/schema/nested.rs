//! Checking a value against one [`PropertySchema`] node, at any depth, and finding the node
//! declared at a JSONPath location (to coerce the leaf of a `property(jpath(..), v)`).
//!
//! # Rules (JSON Schema semantics on the engine's logical types)
//!
//! - **`type`**: the value's kind must be listed. `integer` admits an `int64` only (a `float64`
//!   `3.0` is not an integer), `number` a `float64` only (an `int64` is coerced on write, but a
//!   stored `int64` does not conform), `string` a `string`, `string` + `format: "uuid"` a
//!   `uuid` (a stored string does not conform; a canonical string is coerced on write), and
//!   `string` + `format: "date-time"` (`date`, `local-date-time`, `time`, `duration`) the
//!   temporal type of the format (a string that the type's parser accepts is coerced on
//!   write). A node without `type` admits every kind.
//! - **`null`** is a value like any other: a key holding `null` conforms only when its node
//!   admits `null` (`"type": ["string", "null"]`, an `anyOf` member, or no `type`).
//! - **Objects**: every present key is checked against its declared schema; an undeclared key is
//!   rejected when the object's `additionalProperties` is `false` (explicit, or `Closed` mode's
//!   default) ([`SchemaError::UndeclaredProperty`]); every `required` key must be present
//!   ([`SchemaError::MissingRequiredProperty`]). Required says nothing about null, nullable
//!   nothing about absence.
//! - **Arrays**: every item is checked against `items`.
//! - **Constraints** apply to the kind they belong to (`minimum` to numbers, `minLength` to
//!   strings, `formatMinimum` to the temporal values of the node's format, ...); `enum`/`const`
//!   compare JSON values (`1` equals `1.0`, a `uuid` and a temporal value compare as their
//!   canonical text).
//! - **`anyOf`**: at least one member must accept the value. When none does, the errors of the
//!   first member of the same container kind are reported (else a type mismatch against the
//!   whole union). To find the declared node *at a path through `anyOf`* (leaf coercion), the
//!   members that declare the location are collected: one distinct node is used; with several,
//!   the one the written value already conforms to wins; with none the write is refused
//!   ([`SchemaError::TypeMismatch`] naming the candidate types). The ambiguity is never
//!   resolved by guessing a coercion.
//!
//! Errors name the full canonical path (`$.a.b[1].c`) as the property name.
//!
//! # One walker, two modes
//!
//! The rules live once, in `walk`, which reports to a [`Sink`]: a write ([`validate_nested`])
//! stops at the first violation, the validation report (`schema/apply.rs`: the data scan of
//! `set_schema`/`patch_schema`, `validate_schema`, `validate_schema_patch`) collects every
//! violation inside a value: all keys, all items, at any depth. Per node, only the first
//! failing constraint is reported.

use std::cmp::Ordering;
use std::ops::ControlFlow;
use std::sync::Arc;

use crate::error::SchemaError;
use crate::graph::schema::types::value_type_name;
use crate::graph::schema::{Constraints, JsonType, PropertySchema, SchemaFormat, VectorSchema};
use crate::value::json_path::JsonPathSegment;
use crate::value::physical::{PhysicalValue, lower};
use crate::value::{ElementProperty, JsonPath, Number};

/// What every nested error needs to know about the property being checked.
pub(crate) struct NestedCtx<'a> {
    /// `"vertex"` or `"edge"`.
    pub(crate) element_type: &'static str,
    /// The declaring label (named in diagnostics).
    pub(crate) label: &'a str,
    /// The `additionalProperties` of an object node that states none (the mode's default).
    pub(crate) additional_default: bool,
    /// `false`: check only the types (`type`, `format`, `items`, `properties`, `anyOf`), as the
    /// GraphML import does to restore declared types; required keys, undeclared keys and
    /// constraints are left to the schema mode.
    pub(crate) full: bool,
}

/// The property name of a diagnostic: the plain key for a top-level property, the canonical
/// path (`$.a.b[1].c`) below it.
pub(crate) fn render(path: &[JsonPathSegment]) -> String {
    match path {
        [JsonPathSegment::Key(key)] => key.to_string(),
        _ => JsonPath(Arc::new(path.to_vec())).to_string(),
    }
}

fn mismatch(
    ctx: &NestedCtx<'_>,
    path: &[JsonPathSegment],
    expected: String,
    value: &ElementProperty,
) -> SchemaError {
    SchemaError::TypeMismatch {
        element_type: ctx.element_type,
        label: ctx.label.to_string(),
        property_name: render(path),
        expected_type: expected,
        actual_type: value_type_name(value),
    }
}

/// The kind of a value, as `type` names it, given the `format` of the node's `string` and the
/// node's vector declaration (`None` for a graph element, for a string-like value the format
/// does not declare, for a vector the node does not declare and for a list where the node
/// declares a vector).
fn kind_of(
    value: &ElementProperty,
    format: Option<SchemaFormat>,
    vector: Option<&VectorSchema>,
) -> Option<JsonType> {
    Some(match value {
        ElementProperty::Null => JsonType::Null,
        ElementProperty::Boolean(_) => JsonType::Boolean,
        ElementProperty::Number(n) if n.is_i64() => JsonType::Integer,
        ElementProperty::Number(_) => JsonType::Number,
        ElementProperty::String(_)
        | ElementProperty::Compressed(_)
        | ElementProperty::Deduped(_)
            if format.is_none() =>
        {
            JsonType::String
        }
        // `string` + `format: uuid` is a uuid, `string` + `format: date-time` a datetime, ...
        ElementProperty::Uuid(_) | ElementProperty::Temporal(_)
            if format.is_some_and(|format| format.admits(value)) =>
        {
            JsonType::String
        }
        ElementProperty::String(_)
        | ElementProperty::Compressed(_)
        | ElementProperty::Deduped(_)
        | ElementProperty::Uuid(_)
        | ElementProperty::Temporal(_) => return None,
        // `array` + `format: vector` is a vector of the declared element and dimension; a
        // stored list does not conform (a list of numbers is coerced on write).
        ElementProperty::Vector(v) if vector.is_some_and(|declared| declared.admits(v)) => {
            JsonType::Array
        }
        ElementProperty::Vector(_) => return None,
        ElementProperty::Array(_) if vector.is_some() => return None,
        ElementProperty::Array(_) => JsonType::Array,
        ElementProperty::Object(_) | ElementProperty::Map(_) => JsonType::Object,
        ElementProperty::Vertex(_) | ElementProperty::Edge(_) | ElementProperty::Blob(_) => {
            return None;
        }
    })
}

/// Whether the node's own `type`/`format` admit the value (`anyOf` and content not looked at).
pub(crate) fn admits_type(node: &PropertySchema, value: &ElementProperty) -> bool {
    match &node.types {
        None => !matches!(value, ElementProperty::Vertex(_) | ElementProperty::Edge(_)),
        // `string` + `format` admits the format's logical type; without one, a plain string.
        Some(types) => kind_of(value, node.string_format(), node.vector.as_ref())
            .is_some_and(|kind| types.contains(&kind)),
    }
}

/// Where the walker puts the violations it finds: the write path stops at the first one
/// ([`FailFast`]), the report path collects every one (`Vec<SchemaError>`). Both run the same
/// rules (`walk`); the sink only decides whether to go on.
pub(crate) trait Sink: Sized {
    /// Takes one violation; `Break` stops the walk.
    fn report(&mut self, error: SchemaError) -> ControlFlow<()>;
    /// An empty sink of the same kind, for trying one `anyOf` member.
    fn fresh(&self) -> Self;
    /// Whether nothing was reported.
    fn is_clean(&self) -> bool;
    /// Reports everything `other` holds.
    fn absorb(&mut self, other: Self) -> ControlFlow<()>;
}

/// Keeps the first violation and stops.
#[derive(Default)]
pub(crate) struct FailFast(Option<SchemaError>);

impl FailFast {
    /// The first violation as a `Result`.
    pub(crate) fn into_result(self) -> Result<(), SchemaError> {
        self.0.map_or(Ok(()), Err)
    }
}

impl Sink for FailFast {
    fn report(&mut self, error: SchemaError) -> ControlFlow<()> {
        if self.0.is_none() {
            self.0 = Some(error);
        }
        ControlFlow::Break(())
    }
    fn fresh(&self) -> Self {
        FailFast(None)
    }
    fn is_clean(&self) -> bool {
        self.0.is_none()
    }
    fn absorb(&mut self, other: Self) -> ControlFlow<()> {
        match other.0 {
            Some(error) => self.report(error),
            None => ControlFlow::Continue(()),
        }
    }
}

/// Collects every violation, in walk order (keys in stored order, items by index, depth first).
impl Sink for Vec<SchemaError> {
    fn report(&mut self, error: SchemaError) -> ControlFlow<()> {
        self.push(error);
        ControlFlow::Continue(())
    }
    fn fresh(&self) -> Self {
        Vec::new()
    }
    fn is_clean(&self) -> bool {
        self.is_empty()
    }
    fn absorb(&mut self, other: Self) -> ControlFlow<()> {
        self.extend(other);
        ControlFlow::Continue(())
    }
}

/// Validates `value` against `node`, below the property whose path so far is `path` (its first
/// segment is the property name), failing on the first violation.
pub(crate) fn validate_nested(
    ctx: &NestedCtx<'_>,
    path: &mut Vec<JsonPathSegment>,
    node: &PropertySchema,
    value: &ElementProperty,
) -> Result<(), SchemaError> {
    let mut sink = FailFast::default();
    let _ = walk(ctx, path, node, value, &mut sink);
    sink.into_result()
}

/// The rules of the module docs, reporting every violation to `sink` until it says stop.
///
/// A value whose kind the node does not admit is one type mismatch: its content is not looked
/// at. Otherwise every item, every key (declared, undeclared) and every missing required key is
/// checked, then the node's constraints (the first failing one) and its `anyOf`.
pub(crate) fn walk<S: Sink>(
    ctx: &NestedCtx<'_>,
    path: &mut Vec<JsonPathSegment>,
    node: &PropertySchema,
    value: &ElementProperty,
    sink: &mut S,
) -> ControlFlow<()> {
    if !admits_type(node, value) {
        return sink.report(mismatch(ctx, path, node.non_null_type_name(), value));
    }
    match value {
        ElementProperty::Array(items) => {
            if let Some(item_node) = &node.items {
                for (index, item) in items.iter().enumerate() {
                    path.push(JsonPathSegment::Index(index as i64));
                    let flow = walk(ctx, path, item_node, item, sink);
                    path.pop();
                    flow?;
                }
            }
        }
        ElementProperty::Object(map) => {
            for (key, field) in map.iter() {
                path.push(JsonPathSegment::Key(key.clone()));
                let flow = match node.properties.get(key.as_str()) {
                    Some(declared) => walk(ctx, path, declared, field, sink),
                    None if ctx.full
                        && !node.admits_additional_properties(ctx.additional_default) =>
                    {
                        sink.report(SchemaError::UndeclaredProperty {
                            element_type: ctx.element_type,
                            label: ctx.label.to_string(),
                            property_name: render(path),
                        })
                    }
                    None => ControlFlow::Continue(()),
                };
                path.pop();
                flow?;
            }
            if ctx.full {
                for name in &node.required {
                    if !map.contains_key(name.as_str()) {
                        path.push(JsonPathSegment::Key(name.as_str().into()));
                        let property_name = render(path);
                        path.pop();
                        sink.report(SchemaError::MissingRequiredProperty {
                            element_type: ctx.element_type,
                            label: ctx.label.to_string(),
                            property_name,
                        })?;
                    }
                }
            }
        }
        _ => {}
    }
    if ctx.full
        && let Some(constraints) = node.constraints.as_deref()
        && let Err(error) = check_constraints(ctx.label, &render(path), constraints, value)
    {
        sink.report(error)?;
    }
    if let Some(members) = &node.any_of {
        walk_any_of(ctx, path, node, members, value, sink)?;
    }
    ControlFlow::Continue(())
}

/// At least one member must accept the value. When none does, the violations of the first
/// member of the same container kind are reported (all of them when the sink collects), else
/// one type mismatch against the whole union.
fn walk_any_of<S: Sink>(
    ctx: &NestedCtx<'_>,
    path: &mut Vec<JsonPathSegment>,
    node: &PropertySchema,
    members: &[PropertySchema],
    value: &ElementProperty,
    sink: &mut S,
) -> ControlFlow<()> {
    let mut first_same_kind: Option<S> = None;
    for member in members {
        let mut trial = sink.fresh();
        let _ = walk(ctx, path, member, value, &mut trial);
        if trial.is_clean() {
            return ControlFlow::Continue(());
        }
        let same_kind = admits_type(member, value)
            && matches!(
                value,
                ElementProperty::Object(_) | ElementProperty::Array(_)
            );
        if same_kind && first_same_kind.is_none() {
            first_same_kind = Some(trial);
        }
    }
    match first_same_kind {
        Some(trial) => sink.absorb(trial),
        None => sink.report(mismatch(ctx, path, node.non_null_type_name(), value)),
    }
}

/// Compares two numbers by value (`1` equals `1.0`); `None` for a NaN.
pub(crate) fn compare_numbers(a: &Number, b: &Number) -> Option<Ordering> {
    match (a.as_i64(), b.as_i64()) {
        (Some(x), Some(y)) => Some(x.cmp(&y)),
        _ => {
            let x = a.as_f64().or_else(|| a.as_i64().map(|i| i as f64))?;
            let y = b.as_f64().or_else(|| b.as_i64().map(|i| i as f64))?;
            x.partial_cmp(&y)
        }
    }
}

/// A number for a diagnostic.
pub(crate) fn number_text(n: &Number) -> String {
    match n.as_i64() {
        Some(i) => i.to_string(),
        None => n.as_f64().map(|f| f.to_string()).unwrap_or_default(),
    }
}

/// JSON equality of two values (`enum`/`const`): numbers by value, objects regardless of key
/// order, a `uuid` as its canonical string.
pub(crate) fn json_equal(a: &ElementProperty, b: &ElementProperty) -> bool {
    fn eq(a: &PhysicalValue<'_>, b: &PhysicalValue<'_>) -> bool {
        match (a, b) {
            (PhysicalValue::Int(x), PhysicalValue::Int(y)) => x == y,
            (PhysicalValue::Int(x), PhysicalValue::Float(y))
            | (PhysicalValue::Float(y), PhysicalValue::Int(x)) => (*x as f64) == *y,
            (PhysicalValue::Float(x), PhysicalValue::Float(y)) => x == y,
            (PhysicalValue::Array(x), PhysicalValue::Array(y)) => {
                x.len() == y.len() && x.iter().zip(y).all(|(x, y)| eq(x, y))
            }
            (PhysicalValue::Object(x), PhysicalValue::Object(y)) => {
                x.len() == y.len()
                    && x.iter().all(|(key, value)| {
                        y.iter()
                            .find(|(other, _)| other == key)
                            .is_some_and(|(_, other)| eq(value, other))
                    })
            }
            (a, b) => a == b,
        }
    }
    eq(&lower(a), &lower(b))
}

/// A value for a diagnostic (Rhai literal style).
fn preview(value: &ElementProperty) -> String {
    crate::traversal::step::barrier_steps::numeric::preview(value)
}

/// Checks the assertion keywords that apply to the value's kind.
pub(crate) fn check_constraints(
    label: &str,
    property_name: &str,
    constraints: &Constraints,
    value: &ElementProperty,
) -> Result<(), SchemaError> {
    let violation = |rule: String, actual: String| SchemaError::ConstraintViolation {
        label: label.to_string(),
        property_name: property_name.to_string(),
        constraint_rule: rule,
        actual_value: actual,
    };
    match value {
        // A compressed string is checked on its text (decompressed into the scratch buffer).
        ElementProperty::String(_)
        | ElementProperty::Compressed(_)
        | ElementProperty::Deduped(_) => value
            .with_text(|s| -> Result<(), SchemaError> {
                let length = s.chars().count() as u64;
                if let Some(min) = constraints.min_length
                    && length < min
                {
                    return Err(violation(
                        format!("length >= {min}"),
                        format!("length {length}"),
                    ));
                }
                if let Some(max) = constraints.max_length
                    && length > max
                {
                    return Err(violation(
                        format!("length <= {max}"),
                        format!("length {length}"),
                    ));
                }
                if let Some(pattern) = &constraints.pattern
                    && !pattern.is_match(s)
                {
                    return Err(violation(
                        format!("matches pattern '{pattern}'"),
                        s.to_string(),
                    ));
                }
                Ok(())
            })
            .unwrap_or(Ok(()))?,
        ElementProperty::Number(n) => {
            let bounds = [
                (
                    &constraints.minimum,
                    ">=",
                    [Ordering::Greater, Ordering::Equal],
                ),
                (
                    &constraints.exclusive_minimum,
                    ">",
                    [Ordering::Greater, Ordering::Greater],
                ),
                (
                    &constraints.maximum,
                    "<=",
                    [Ordering::Less, Ordering::Equal],
                ),
                (
                    &constraints.exclusive_maximum,
                    "<",
                    [Ordering::Less, Ordering::Less],
                ),
            ];
            for (bound, op, allowed) in bounds {
                if let Some(bound) = bound {
                    let ok = compare_numbers(n, bound).is_some_and(|o| allowed.contains(&o));
                    if !ok {
                        return Err(violation(
                            format!("value {op} {}", number_text(bound)),
                            number_text(n),
                        ));
                    }
                }
            }
        }
        ElementProperty::Temporal(temporal) => {
            let bounds = [
                (
                    &constraints.format_minimum,
                    ">=",
                    [Ordering::Greater, Ordering::Equal],
                ),
                (
                    &constraints.format_exclusive_minimum,
                    ">",
                    [Ordering::Greater, Ordering::Greater],
                ),
                (
                    &constraints.format_maximum,
                    "<=",
                    [Ordering::Less, Ordering::Equal],
                ),
                (
                    &constraints.format_exclusive_maximum,
                    "<",
                    [Ordering::Less, Ordering::Less],
                ),
            ];
            for (bound, op, allowed) in bounds {
                // A bound applies to a value of its own type (the node's `format`, checked when
                // the schema is loaded): a `datetime` by its instant, a `duration` by its length.
                if let Some(bound) = bound
                    && bound.kind() == temporal.kind()
                    && !allowed.contains(&temporal.cmp(bound))
                {
                    return Err(violation(
                        format!("value {op} {bound}"),
                        temporal.to_string(),
                    ));
                }
            }
        }
        ElementProperty::Array(items) => {
            let len = items.len() as u64;
            if let Some(min) = constraints.min_items
                && len < min
            {
                return Err(violation(format!("items >= {min}"), format!("items {len}")));
            }
            if let Some(max) = constraints.max_items
                && len > max
            {
                return Err(violation(format!("items <= {max}"), format!("items {len}")));
            }
        }
        _ => {}
    }
    if let Some(values) = &constraints.enum_values
        && !values.iter().any(|allowed| json_equal(allowed, value))
    {
        let listed: Vec<String> = values.iter().take(10).map(preview).collect();
        let more = if values.len() > 10 { ", …" } else { "" };
        return Err(violation(
            format!("one of [{}{more}]", listed.join(", ")),
            preview(value),
        ));
    }
    if let Some(expected) = &constraints.const_value
        && !json_equal(expected, value)
    {
        return Err(violation(
            format!("equals {}", preview(expected)),
            preview(value),
        ));
    }
    Ok(())
}

/// The node declared at `full_path[depth..]` below `node`, through `properties`, `items` and
/// `anyOf` (see the module docs for the `anyOf` rule).
///
/// `Ok(None)`: the location is not declared (an undeclared nested key, an index into a
/// non-array, ...); nothing to coerce to. `Err`: the location is declared by several `anyOf`
/// members with different nodes and `value` conforms to none of them.
pub(crate) fn declared_node_at<'a>(
    ctx: &NestedCtx<'_>,
    full_path: &[JsonPathSegment],
    node: &'a PropertySchema,
    depth: usize,
    value: &ElementProperty,
) -> Result<Option<&'a PropertySchema>, SchemaError> {
    let Some(segment) = full_path.get(depth) else {
        return Ok(Some(node));
    };
    let own = match segment {
        JsonPathSegment::Key(key) => node.properties.get(key.as_str()),
        JsonPathSegment::Index(_) => node.items.as_deref(),
    };
    if let Some(child) = own {
        return declared_node_at(ctx, full_path, child, depth + 1, value);
    }
    let Some(members) = &node.any_of else {
        return Ok(None);
    };
    let mut candidates: Vec<&'a PropertySchema> = Vec::new();
    for member in members {
        if let Some(found) = declared_node_at(ctx, full_path, member, depth, value)?
            && !candidates.contains(&found)
        {
            candidates.push(found);
        }
    }
    match candidates.len() {
        0 => Ok(None),
        1 => Ok(candidates.pop()),
        _ => {
            let types_only = NestedCtx {
                element_type: ctx.element_type,
                label: ctx.label,
                additional_default: true,
                full: false,
            };
            let exact = candidates.iter().find(|candidate| {
                validate_nested(&types_only, &mut Vec::new(), candidate, value).is_ok()
            });
            match exact {
                Some(candidate) => Ok(Some(*candidate)),
                None => Err(mismatch(
                    ctx,
                    full_path,
                    format!(
                        "union<{}>",
                        candidates
                            .iter()
                            .map(|c| c.non_null_type_name())
                            .collect::<Vec<_>>()
                            .join("|")
                    ),
                    value,
                )),
            }
        }
    }
}
