//! Coercion on write: converts an incoming property value to the type a schema declares for it.
//!
//! Only four conversions exist, and all happen only because the schema *explicitly declares*
//! the target type (this is not content sniffing, see CLAUDE.md rule 7):
//!
//! - a canonical UUID string becomes a [`ElementProperty::Uuid`] where the node is
//!   `{"type": "string", "format": "uuid"}`;
//! - a string the temporal type's parser accepts (ISO 8601 / RFC 3339 text, not only the
//!   canonical form) becomes a [`ElementProperty::Temporal`] where the node is
//!   `{"type": "string", "format": "date-time"}` (`date`, `local-date-time`, `time`,
//!   `duration`); text the parser refuses is a coercion failure;
//! - an `int64` becomes a `float64` where the node admits `number` but not `integer`, when it
//!   is exactly representable;
//! - a list of numbers becomes a [`ElementProperty::Vector`] of the declared element where the
//!   node is `{"type": "array", "format": "vector", ..}`, and a vector of another element is
//!   quantized to the declared one (the dimension is not changed: validation checks it).
//!
//! Everything else passes through unchanged, and validation reports any remaining mismatch.
//! Reads never coerce. A schema's `default` is coerced like a written value when a create stores
//! it (`schema/defaults.rs` fills it in, the write path coerces it).

use crate::graph::schema::nested::{NestedCtx, validate_nested};
use crate::graph::schema::{JsonType, PropertySchema, SchemaFormat};
use std::borrow::Cow;

use crate::value::physical::{PhysicalValue, lift_temporal};
use crate::value::temporal::TemporalKind;
use crate::value::uuid::parse_canonical;
use crate::value::{ElementProperty, Number, Vector, VectorElement, VectorError};

/// Why a value could not be coerced to its declared type.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CoerceFailure {
    /// A short, human-readable reason, rendered into `SchemaError::CoercionFailed`.
    pub(crate) reason: &'static str,
    /// The offending leaf value (for a container, the item or field that failed), handed back
    /// so the diagnostic can show it without the caller cloning every value up front.
    pub(crate) value: ElementProperty,
}

/// The reason reported for a string that is not a canonical UUID.
pub(crate) const NOT_A_CANONICAL_UUID: &str = "not a canonical UUID (8-4-4-4-12 lowercase hex)";

/// The reason reported for a string that is not a value of a declared temporal type.
pub(crate) fn not_a_temporal(kind: TemporalKind) -> &'static str {
    match kind {
        TemporalKind::DateTime => {
            "not a datetime with its UTC offset or time zone (ISO 8601 / RFC 3339 such as \
             2024-05-01T09:30:00+02:00; a text without an offset is not read as UTC; years -9999 \
             to 9999)"
        }
        TemporalKind::Date => "not a date (ISO 8601 such as 2024-05-01, years -9999 to 9999)",
        TemporalKind::LocalDateTime => {
            "not a localdatetime (ISO 8601 without an offset such as 2024-05-01T09:30:00)"
        }
        TemporalKind::Time => "not a time (ISO 8601 without an offset such as 09:30:00)",
        TemporalKind::Duration => "not a duration (ISO 8601 such as P1Y2M3DT4H5M6S)",
        TemporalKind::OffsetTime => {
            "not an offsettime (ISO 8601 time with its UTC offset such as 09:30:00+02:00)"
        }
    }
}

/// The reason reported for an integer that has no exact `float64` counterpart.
pub(crate) const INEXACT_FLOAT64: &str = "integer cannot be represented exactly as float64";

/// The largest integer magnitude every `f64` represents exactly (2^53).
const MAX_EXACT_F64_INTEGER: u64 = 1 << 53;

/// Coerces `value` to the declared `node`, recursively through `items`, `properties` and
/// `anyOf`.
///
/// Free of any validator context on purpose, so that the physical codec's `lift` can reuse it.
/// A value that needs no conversion, or that no conversion applies to, is returned unchanged;
/// the caller's check then decides whether it conforms.
pub(crate) fn coerce_to_declared(
    value: ElementProperty,
    node: &PropertySchema,
) -> Result<ElementProperty, CoerceFailure> {
    if matches!(value, ElementProperty::Null) {
        return Ok(value);
    }
    if node.types.is_none()
        && let Some(members) = &node.any_of
    {
        return Ok(coerce_to_union(value, members));
    }
    let Some(types) = &node.types else {
        return Ok(value);
    };
    match value {
        ElementProperty::String(text) if node.string_format() == Some(SchemaFormat::Uuid) => {
            match parse_canonical(&text) {
                Some(uuid) => Ok(ElementProperty::Uuid(uuid)),
                None => Err(CoerceFailure {
                    reason: NOT_A_CANONICAL_UUID,
                    value: ElementProperty::String(text),
                }),
            }
        }
        // Plain guards, not `if let` guards: those need a newer Rust than the MSRV.
        ElementProperty::String(text)
            if node
                .string_format()
                .and_then(SchemaFormat::temporal_kind)
                .is_some() =>
        {
            let Some(kind) = node.string_format().and_then(SchemaFormat::temporal_kind) else {
                return Ok(ElementProperty::String(text));
            };
            match lift_temporal(&PhysicalValue::String(Cow::Borrowed(text.as_str())), kind) {
                Ok(temporal) => Ok(temporal),
                Err(_) => Err(CoerceFailure {
                    reason: not_a_temporal(kind),
                    value: ElementProperty::String(text),
                }),
            }
        }
        ElementProperty::Number(number)
            if types.contains(&JsonType::Number) && !types.contains(&JsonType::Integer) =>
        {
            match number.as_i64() {
                Some(integer) => int_to_float64(integer),
                None => Ok(ElementProperty::Number(number)),
            }
        }
        // A list of numbers where the node declares a vector: a vector of the declared element
        // (quantized from the f32 values), so a property is stored quantized from the start.
        ElementProperty::Array(items) if node.vector.is_some() => {
            let Some(declared) = &node.vector else {
                return Ok(ElementProperty::Array(items));
            };
            let vector = match Vector::from_values(&items) {
                Ok(vector) => vector,
                Err(err) => {
                    return Err(CoerceFailure {
                        reason: match err {
                            VectorError::Dimension { .. } => NOT_A_VECTOR_LENGTH,
                            _ => NOT_A_VECTOR,
                        },
                        value: ElementProperty::Array(items),
                    });
                }
            };
            quantize_to(vector, declared.element)
        }
        // A vector of another element: quantized to the declared one.
        ElementProperty::Vector(vector)
            if node
                .vector
                .as_ref()
                .is_some_and(|declared| vector.element() != declared.element) =>
        {
            match &node.vector {
                Some(declared) => quantize_to(vector, declared.element),
                None => Ok(ElementProperty::Vector(vector)),
            }
        }
        ElementProperty::Array(items) => match &node.items {
            Some(item_node) => items
                .into_iter()
                .map(|item| coerce_to_declared(item, item_node))
                .collect::<Result<Vec<_>, _>>()
                .map(ElementProperty::Array),
            None => Ok(ElementProperty::Array(items)),
        },
        ElementProperty::Object(mut map) => {
            for (key, slot) in map.iter_mut() {
                if let Some(field) = node.properties.get(key.as_str()) {
                    let incoming = std::mem::replace(slot, ElementProperty::Null);
                    *slot = coerce_to_declared(incoming, field)?;
                }
            }
            Ok(ElementProperty::Object(map))
        }
        other => Ok(other),
    }
}

/// The reason reported for a list that is not a list of finite numbers where a vector is
/// declared.
pub(crate) const NOT_A_VECTOR: &str =
    "not a list of finite numbers (the property declares a vector)";

/// The reason reported for an empty or too long list where a vector is declared.
pub(crate) const NOT_A_VECTOR_LENGTH: &str = "a vector has 1 to 65536 components";

/// The reason reported for a vector that does not fit the declared element.
pub(crate) const NOT_IN_ELEMENT_RANGE: &str =
    "a component lies beyond the range of the declared element (f16: 65504)";

/// A vector in the declared element.
fn quantize_to(vector: Vector, element: VectorElement) -> Result<ElementProperty, CoerceFailure> {
    match vector.quantize(element) {
        Ok(quantized) => Ok(ElementProperty::Vector(quantized)),
        Err(_) => Err(CoerceFailure {
            reason: NOT_IN_ELEMENT_RANGE,
            value: ElementProperty::Vector(vector),
        }),
    }
}

/// Converts an integer to `float64` when the conversion loses nothing.
fn int_to_float64(integer: i64) -> Result<ElementProperty, CoerceFailure> {
    let float = integer as f64;
    if integer.unsigned_abs() <= MAX_EXACT_F64_INTEGER && float as i64 == integer {
        Ok(ElementProperty::Number(Number::from_f64(float)))
    } else {
        Err(CoerceFailure {
            reason: INEXACT_FLOAT64,
            value: ElementProperty::Number(Number::from_i64(integer)),
        })
    }
}

/// Whether `value`'s types conform to `node` as they are (no coercion, no constraints).
pub(crate) fn conforms_to(node: &PropertySchema, value: &ElementProperty) -> bool {
    let ctx = NestedCtx {
        element_type: "vertex",
        label: "",
        additional_default: true,
        full: false,
    };
    validate_nested(&ctx, &mut Vec::new(), node, value).is_ok()
}

/// Coerces a value against `anyOf` members.
///
/// A value that already conforms to a member is left alone. Otherwise the first member, in
/// declaration order, for which coercion succeeds *and* yields a conforming value wins. When no
/// member takes the value it is returned unchanged, and validation reports the mismatch.
fn coerce_to_union(value: ElementProperty, members: &[PropertySchema]) -> ElementProperty {
    if members.iter().any(|member| conforms_to(member, &value)) {
        return value;
    }
    for member in members {
        if let Ok(coerced) = coerce_to_declared(value.clone(), member)
            && conforms_to(member, &coerced)
        {
            return coerced;
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    const CANONICAL: &str = "550e8400-e29b-41d4-a716-446655440000";
    const CANONICAL_BITS: u128 = 0x550e8400_e29b_41d4_a716_446655440000;

    fn int(value: i64) -> ElementProperty {
        ElementProperty::Number(Number::from_i64(value))
    }

    fn float(value: f64) -> ElementProperty {
        ElementProperty::Number(Number::from_f64(value))
    }

    fn text(value: &str) -> ElementProperty {
        ElementProperty::from(value)
    }

    fn is_float(value: &ElementProperty) -> bool {
        matches!(value, ElementProperty::Number(n) if n.is_f64())
    }

    #[test]
    fn null_passes_unchanged_whatever_the_declaration() {
        for declared in [
            PropertySchema::uuid(),
            PropertySchema::number(),
            PropertySchema::string(),
            PropertySchema::integer(),
        ] {
            assert_eq!(
                coerce_to_declared(ElementProperty::Null, &declared),
                Ok(ElementProperty::Null)
            );
        }
    }

    #[test]
    fn canonical_string_becomes_uuid() {
        assert_eq!(
            coerce_to_declared(text(CANONICAL), &PropertySchema::uuid()),
            Ok(ElementProperty::Uuid(CANONICAL_BITS))
        );
    }

    #[test]
    fn non_canonical_string_fails_for_uuid() {
        for bad in [
            "not-a-uuid",
            "550E8400-E29B-41D4-A716-446655440000",
            "{550e8400-e29b-41d4-a716-446655440000}",
            "",
        ] {
            assert_eq!(
                coerce_to_declared(text(bad), &PropertySchema::uuid()),
                Err(CoerceFailure {
                    reason: NOT_A_CANONICAL_UUID,
                    value: text(bad),
                }),
                "{bad:?} must not coerce"
            );
        }
    }

    #[test]
    fn exact_integer_becomes_float64() {
        let coerced = coerce_to_declared(int(3), &PropertySchema::number()).unwrap();
        assert!(is_float(&coerced));
        assert_eq!(coerced, float(3.0));
        let limit = 1_i64 << 53;
        assert_eq!(
            coerce_to_declared(int(-limit), &PropertySchema::number()),
            Ok(float(-limit as f64))
        );
    }

    #[test]
    fn integer_stays_where_integer_is_admitted() {
        let both = PropertySchema {
            types: Some(vec![JsonType::Integer, JsonType::Number]),
            ..PropertySchema::default()
        };
        assert_eq!(coerce_to_declared(int(3), &both), Ok(int(3)));
    }

    #[test]
    fn inexact_integer_fails_for_float64() {
        for too_big in [(1_i64 << 53) + 1, i64::MAX, i64::MIN] {
            assert_eq!(
                coerce_to_declared(int(too_big), &PropertySchema::number()),
                Err(CoerceFailure {
                    reason: INEXACT_FLOAT64,
                    value: int(too_big),
                }),
                "{too_big} must not coerce"
            );
        }
    }

    #[test]
    fn arrays_coerce_item_by_item() {
        let declared = PropertySchema::array(PropertySchema::uuid());
        assert_eq!(
            coerce_to_declared(ElementProperty::Array(vec![text(CANONICAL)]), &declared),
            Ok(ElementProperty::Array(vec![ElementProperty::Uuid(
                CANONICAL_BITS
            )]))
        );
        assert_eq!(
            coerce_to_declared(
                ElementProperty::Array(vec![text(CANONICAL), text("nope")]),
                &declared
            ),
            Err(CoerceFailure {
                reason: NOT_A_CANONICAL_UUID,
                value: text("nope"),
            })
        );
    }

    #[test]
    fn objects_coerce_declared_keys_only() {
        let declared = PropertySchema::object().with_property("w", PropertySchema::number());
        let mut map = crate::value::PropertyMap::default();
        map.insert("w", int(2));
        map.insert("other", int(5));
        let coerced =
            coerce_to_declared(ElementProperty::Object(Box::new(map)), &declared).unwrap();
        let coerced = coerced.as_object().unwrap();
        assert!(is_float(&coerced["w"]));
        assert!(matches!(&coerced["other"], ElementProperty::Number(n) if n.is_i64()));
    }

    #[test]
    fn any_of_keeps_a_conforming_value() {
        let declared =
            PropertySchema::any_of(vec![PropertySchema::uuid(), PropertySchema::string()]);
        assert_eq!(
            coerce_to_declared(text(CANONICAL), &declared),
            Ok(text(CANONICAL))
        );
    }

    #[test]
    fn any_of_takes_the_first_member_that_coerces() {
        let declared =
            PropertySchema::any_of(vec![PropertySchema::integer(), PropertySchema::uuid()]);
        assert_eq!(
            coerce_to_declared(text(CANONICAL), &declared),
            Ok(ElementProperty::Uuid(CANONICAL_BITS))
        );
        let declared =
            PropertySchema::any_of(vec![PropertySchema::boolean(), PropertySchema::number()]);
        assert_eq!(coerce_to_declared(int(4), &declared), Ok(float(4.0)));
    }

    #[test]
    fn any_of_leaves_an_unconvertible_value_unchanged() {
        let declared =
            PropertySchema::any_of(vec![PropertySchema::uuid(), PropertySchema::integer()]);
        assert_eq!(
            coerce_to_declared(text("nope"), &declared),
            Ok(text("nope"))
        );
    }

    #[test]
    fn nothing_else_converts() {
        assert_eq!(
            coerce_to_declared(
                ElementProperty::Uuid(CANONICAL_BITS),
                &PropertySchema::string()
            ),
            Ok(ElementProperty::Uuid(CANONICAL_BITS))
        );
        assert_eq!(
            coerce_to_declared(float(3.0), &PropertySchema::integer()),
            Ok(float(3.0))
        );
        assert_eq!(
            coerce_to_declared(text("42"), &PropertySchema::integer()),
            Ok(text("42"))
        );
        assert_eq!(
            coerce_to_declared(int(1), &PropertySchema::boolean()),
            Ok(int(1))
        );
    }
}
