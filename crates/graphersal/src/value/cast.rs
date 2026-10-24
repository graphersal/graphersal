use crate::error::{TraverserError, TraverserResult};
use crate::graph::schema::{ValueType, value_type_name};
use crate::value::temporal::{DateTime, Temporal, TemporalKind};
use crate::value::uuid as uuid_codec;
use crate::value::{ElementProperty, Number};

/// What a coercion step does with a value it cannot convert.
///
/// There is no safe default beyond failing: a silent skip aborts a whole operation on one bad
/// element under a closed schema and disappears without trace under an open one, so the choice
/// belongs to the caller (design decision D5).
#[derive(Debug, Clone, PartialEq, Default)]
#[non_exhaustive]
pub enum CastPolicy {
    /// Fail the traversal, reporting the offending value and where it came from.
    #[default]
    Error,
    /// Drop the value from the stream.
    Skip,
    /// Replace the value with a fallback.
    Default(ElementProperty),
}

impl ElementProperty {
    /// Converts this value to `target`, following the conversion matrix of design decision D5.
    ///
    /// The conversion is deliberately strict, because a lenient one corrupts data silently:
    ///
    /// | From → To | Rule |
    /// | --- | --- |
    /// | `Null → T` | identity — a cast of null yields null, whatever the target |
    /// | `Int64 → Float64` | always allowed (widening) |
    /// | `Float64 → Int64` | only when the fractional part is zero and the value fits `i64` |
    /// | `* → String` | always allowed; the rendering is owned by the source type |
    /// | `String → Int64` / `Float64` | parses the **entire** input — no trimming, no locale, no partial parse |
    /// | `String → Boolean` | only the exact literals `true` and `false` |
    /// | `Boolean → Int64` / `Float64` | `false → 0`, `true → 1` |
    /// | `Int64` / `Float64 → Boolean` | only from exactly `0` or `1` |
    /// | `Array` / `Object` / `Vertex` / `Edge` → scalar | never |
    /// | temporal → `String` | the canonical ISO 8601 text (`2023-08-02T00:00:00Z`) |
    /// | `DateTime` → `Int64` / `Float64` | milliseconds since the Unix epoch, rounded down (TinkerPop `asNumber()`) |
    /// | `String` → temporal | parses the text of that type (an explicit conversion, never applied implicitly) |
    /// | `Int64` → `DateTime` | the instant that many milliseconds after the epoch, in UTC (TinkerPop `asDate(number)`) |
    /// | temporal → another temporal type | never (the date steps convert between them) |
    ///
    /// Returns [`ValueError::UnsupportedCastTarget`](crate::error::ValueError::UnsupportedCastTarget)
    /// for a target no conversion can produce yet, so an unimplemented target fails loudly
    /// instead of passing the value through unchanged.
    ///
    /// # Examples
    ///
    /// ```
    /// use graphersal::prelude::*;
    /// use graphersal::schema::ValueType;
    ///
    /// let age = ElementProperty::from("999");
    /// assert_eq!(age.cast(&ValueType::Int64).unwrap(), ElementProperty::from(999i64));
    ///
    /// // A partial parse is a failure, not a truncation.
    /// assert!(ElementProperty::from("12abc").cast(&ValueType::Int64).is_err());
    /// ```
    pub fn cast(&self, target: &ValueType) -> TraverserResult<ElementProperty> {
        // Null is the one value that crosses every target unchanged: nullability is governed by
        // the schema's `nullable`, never by a conversion default.
        if matches!(self, ElementProperty::Null) {
            return Ok(ElementProperty::Null);
        }
        // A compressed string casts as the string it holds.
        if let ElementProperty::Compressed(text) = self {
            return ElementProperty::String(text.text()).cast(target);
        }
        if let ElementProperty::Deduped(text) = self {
            return ElementProperty::String(text.as_str().into()).cast(target);
        }

        match target {
            ValueType::Null => self.cast_failure(target),
            ValueType::Boolean => self.cast_to_boolean(target),
            ValueType::Int64 => self.cast_to_int64(target),
            ValueType::Float64 => self.cast_to_float64(target),
            ValueType::String => self.cast_to_string(target),
            ValueType::Uuid => self.cast_to_uuid(target),
            ValueType::Date
            | ValueType::LocalDateTime
            | ValueType::DateTime
            | ValueType::Time
            | ValueType::OffsetTime
            | ValueType::Duration => self.cast_to_temporal(target),
            ValueType::Vector(element) => self.cast_to_vector(*element),

            // A container target would have to describe how to build the container, which a
            // 1-to-1 coercion cannot express; a union target names no single type at all.
            ValueType::Array(_) | ValueType::Object => {
                Err(Box::new(TraverserError::unsupported_cast_target(
                    target.to_string(),
                    "a container is built by fold() or project(), not by a conversion",
                )))
            }
            ValueType::Union(_) => Err(Box::new(TraverserError::unsupported_cast_target(
                target.to_string(),
                "a union names no single target; convert to one of its members",
            ))),
        }
    }

    /// Parses a canonical RFC 9562 UUID string (8-4-4-4-12 lowercase hex) into a `Uuid` value.
    ///
    /// This is the Rust spelling of the Gremlin literal `UUID.fromString("...")`. It shares the
    /// parser and the error with `cast(GType::UUID)`: only the exact canonical form parses (no
    /// braces, no uppercase, no partial input), and anything else is a `CastException`.
    ///
    /// A `Uuid` never equals a `String`, so a filter on a UUID property needs a UUID literal:
    ///
    /// ```
    /// use graphersal::prelude::*;
    ///
    /// # fn main() -> Result<(), Box<TraverserError>> {
    /// let uid = ElementProperty::uuid_from_string("550e8400-e29b-41d4-a716-446655440000")?;
    /// let graph = GraphSource::empty();
    /// graph.write().traversal_mut().add_v("person").property("uid", uid.clone()).next()?;
    ///
    /// let locked = graph.read();
    /// let found = locked.traversal().v(None).has("uid", uid).count().next()?;
    /// assert_eq!(found, Some(ElementProperty::from(1i64)));
    ///
    /// // The plain string is a different value and matches nothing.
    /// let by_string = locked
    ///     .traversal()
    ///     .v(None)
    ///     .has("uid", "550e8400-e29b-41d4-a716-446655440000")
    ///     .count()
    ///     .next()?;
    /// assert_eq!(by_string, Some(ElementProperty::from(0i64)));
    ///
    /// // Uppercase is not canonical.
    /// assert!(ElementProperty::uuid_from_string("550E8400-E29B-41D4-A716-446655440000").is_err());
    /// # Ok(())
    /// # }
    /// ```
    pub fn uuid_from_string(s: &str) -> TraverserResult<ElementProperty> {
        ElementProperty::from(s).cast(&ValueType::Uuid)
    }

    fn cast_to_string(&self, target: &ValueType) -> TraverserResult<ElementProperty> {
        match self {
            ElementProperty::String(_) => Ok(self.clone()),
            ElementProperty::Boolean(value) => {
                Ok(ElementProperty::from(if *value { "true" } else { "false" }))
            }
            ElementProperty::Number(value) => Ok(ElementProperty::from(value.to_string())),
            ElementProperty::Uuid(value) => {
                Ok(ElementProperty::from(uuid_codec::format_canonical(*value)))
            }
            ElementProperty::Temporal(value) => Ok(ElementProperty::from(value.to_string())),
            // The DSL literal (`vector([0.1, 0.2])`), every component.
            ElementProperty::Vector(value) => Ok(ElementProperty::from(value.to_string())),
            _ => self.cast_failure(target),
        }
    }

    /// Converts to a temporal type: the same type passes, a string is parsed as that type, an
    /// integer is epoch milliseconds for a `datetime`; anything else fails.
    fn cast_to_temporal(&self, target: &ValueType) -> TraverserResult<ElementProperty> {
        let Some(kind) = target.temporal_kind() else {
            return self.cast_failure(target);
        };
        match self {
            ElementProperty::Temporal(value) if value.kind() == kind => Ok(self.clone()),
            ElementProperty::String(text) => match Temporal::parse_as(kind, text) {
                Ok(value) => Ok(ElementProperty::Temporal(value)),
                Err(_) => self.cast_failure(target),
            },
            ElementProperty::Number(number) if kind == TemporalKind::DateTime => {
                match number.as_i64().map(DateTime::from_epoch_millis) {
                    Some(Ok(value)) => Ok(ElementProperty::Temporal(Temporal::DateTime(value))),
                    _ => self.cast_failure(target),
                }
            }
            _ => self.cast_failure(target),
        }
    }

    /// Converts to a vector of `element`: a list of numbers becomes an f32 vector quantized to
    /// `element`, a vector is quantized to it (`as_vector()`); anything else fails.
    fn cast_to_vector(
        &self,
        element: crate::value::VectorElement,
    ) -> TraverserResult<ElementProperty> {
        let converted = match self {
            ElementProperty::Vector(vector) => vector.quantize(element),
            ElementProperty::Array(items) => {
                crate::value::Vector::from_values(items).and_then(|vector| vector.quantize(element))
            }
            _ => return self.cast_failure(&ValueType::Vector(element)),
        };
        converted
            .map(ElementProperty::Vector)
            .map_err(|error| crate::value::vector::vector_error("as_vector", error))
    }

    /// Parses a canonical UUID string, or accepts a value that already is one.
    ///
    /// Only the exact canonical form parses (`AGENTS.md` D7): no braces, no uppercase, no
    /// partial input — a near-miss is a `CastException`, not a best-effort guess.
    fn cast_to_uuid(&self, target: &ValueType) -> TraverserResult<ElementProperty> {
        match self {
            ElementProperty::Uuid(_) => Ok(self.clone()),
            ElementProperty::String(value) => match uuid_codec::parse_canonical(value) {
                Some(parsed) => Ok(ElementProperty::Uuid(parsed)),
                None => self.cast_failure(target),
            },
            _ => self.cast_failure(target),
        }
    }

    fn cast_to_boolean(&self, target: &ValueType) -> TraverserResult<ElementProperty> {
        match self {
            ElementProperty::Boolean(_) => Ok(self.clone()),
            ElementProperty::String(value) => match value.as_str() {
                "true" => Ok(ElementProperty::Boolean(true)),
                "false" => Ok(ElementProperty::Boolean(false)),
                _ => self.cast_failure(target),
            },
            ElementProperty::Number(value) => match (value.as_i64(), value.as_f64()) {
                (Some(0), _) => Ok(ElementProperty::Boolean(false)),
                (Some(1), _) => Ok(ElementProperty::Boolean(true)),
                (None, Some(0.0)) => Ok(ElementProperty::Boolean(false)),
                (None, Some(1.0)) => Ok(ElementProperty::Boolean(true)),
                _ => self.cast_failure(target),
            },
            _ => self.cast_failure(target),
        }
    }

    fn cast_to_int64(&self, target: &ValueType) -> TraverserResult<ElementProperty> {
        match self {
            ElementProperty::Number(value) => match value.as_i64() {
                Some(value) => Ok(ElementProperty::Number(Number::from_i64(value))),
                None => match value.as_f64() {
                    // Narrowing is allowed only when nothing is lost: a non-zero fraction or an
                    // out-of-range magnitude is data loss, and data loss is a failure.
                    Some(value)
                        if value.fract() == 0.0
                            && value >= i64::MIN as f64
                            && value <= i64::MAX as f64 =>
                    {
                        Ok(ElementProperty::Number(Number::from_i64(value as i64)))
                    }
                    _ => self.cast_failure(target),
                },
            },
            ElementProperty::String(value) => match value.parse::<i64>() {
                Ok(value) => Ok(ElementProperty::Number(Number::from_i64(value))),
                Err(_) => self.cast_failure(target),
            },
            ElementProperty::Boolean(value) => {
                Ok(ElementProperty::Number(Number::from_i64(if *value {
                    1
                } else {
                    0
                })))
            }
            // TinkerPop `asNumber()` of a date: epoch milliseconds.
            ElementProperty::Temporal(Temporal::DateTime(value)) => Ok(ElementProperty::Number(
                Number::from_i64(value.epoch_millis()),
            )),
            _ => self.cast_failure(target),
        }
    }

    fn cast_to_float64(&self, target: &ValueType) -> TraverserResult<ElementProperty> {
        match self {
            // Widening never fails: an integer always has an exact float64 counterpart here.
            ElementProperty::Number(value) => match (value.as_f64(), value.as_i64()) {
                (Some(value), _) => Ok(ElementProperty::Number(Number::from_f64(value))),
                (None, Some(value)) => Ok(ElementProperty::Number(Number::from_f64(value as f64))),
                (None, None) => self.cast_failure(target),
            },
            ElementProperty::String(value) => match value.parse::<f64>() {
                Ok(value) => Ok(ElementProperty::Number(Number::from_f64(value))),
                Err(_) => self.cast_failure(target),
            },
            ElementProperty::Boolean(value) => {
                Ok(ElementProperty::Number(Number::from_f64(if *value {
                    1.0
                } else {
                    0.0
                })))
            }
            ElementProperty::Temporal(Temporal::DateTime(value)) => Ok(ElementProperty::Number(
                Number::from_f64(value.epoch_millis() as f64),
            )),
            _ => self.cast_failure(target),
        }
    }

    fn cast_failure(&self, target: &ValueType) -> TraverserResult<ElementProperty> {
        Err(Box::new(TraverserError::cast_exception_target(
            target.to_string(),
            value_type_name(self),
            Some(crate::value::literal::rhai_literal(self)),
            None,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int(value: i64) -> ElementProperty {
        ElementProperty::Number(Number::from_i64(value))
    }

    fn float(value: f64) -> ElementProperty {
        ElementProperty::Number(Number::from_f64(value))
    }

    fn text(value: &str) -> ElementProperty {
        ElementProperty::from(value)
    }

    #[test]
    fn null_casts_to_null_whatever_the_target() {
        for target in [
            ValueType::Null,
            ValueType::Boolean,
            ValueType::Int64,
            ValueType::Float64,
            ValueType::String,
        ] {
            assert_eq!(
                ElementProperty::Null.cast(&target).unwrap(),
                ElementProperty::Null,
                "null must survive a cast to {target}"
            );
        }
    }

    #[test]
    fn int64_widens_to_float64() {
        assert_eq!(int(7).cast(&ValueType::Float64).unwrap(), float(7.0));
        assert_eq!(int(-7).cast(&ValueType::Float64).unwrap(), float(-7.0));
    }

    #[test]
    fn float64_narrows_to_int64_only_without_loss() {
        assert_eq!(float(7.0).cast(&ValueType::Int64).unwrap(), int(7));
        assert!(float(7.5).cast(&ValueType::Int64).is_err());
        assert!(float(1e300).cast(&ValueType::Int64).is_err());
    }

    #[test]
    fn everything_scalar_renders_to_string() {
        assert_eq!(int(29).cast(&ValueType::String).unwrap(), text("29"));
        assert_eq!(float(1.5).cast(&ValueType::String).unwrap(), text("1.5"));
        assert_eq!(
            ElementProperty::Boolean(true)
                .cast(&ValueType::String)
                .unwrap(),
            text("true")
        );
        assert_eq!(
            text("marko").cast(&ValueType::String).unwrap(),
            text("marko")
        );
    }

    #[test]
    fn string_to_number_parses_the_whole_input() {
        assert_eq!(text("999").cast(&ValueType::Int64).unwrap(), int(999));
        assert_eq!(text("-999").cast(&ValueType::Int64).unwrap(), int(-999));
        assert_eq!(text("1.5").cast(&ValueType::Float64).unwrap(), float(1.5));

        // No partial parse, no trimming, no locale.
        assert!(text("12abc").cast(&ValueType::Int64).is_err());
        assert!(text(" 12").cast(&ValueType::Int64).is_err());
        assert!(text("12 ").cast(&ValueType::Int64).is_err());
        assert!(text("1,5").cast(&ValueType::Float64).is_err());
        assert!(text("1.5").cast(&ValueType::Int64).is_err());
        assert!(text("").cast(&ValueType::Int64).is_err());
    }

    #[test]
    fn string_to_boolean_takes_the_exact_literals_only() {
        assert_eq!(
            text("true").cast(&ValueType::Boolean).unwrap(),
            ElementProperty::Boolean(true)
        );
        assert_eq!(
            text("false").cast(&ValueType::Boolean).unwrap(),
            ElementProperty::Boolean(false)
        );
        assert!(text("TRUE").cast(&ValueType::Boolean).is_err());
        assert!(text("yes").cast(&ValueType::Boolean).is_err());
        assert!(text("1").cast(&ValueType::Boolean).is_err());
    }

    #[test]
    fn boolean_converts_to_zero_and_one() {
        assert_eq!(
            ElementProperty::Boolean(false)
                .cast(&ValueType::Int64)
                .unwrap(),
            int(0)
        );
        assert_eq!(
            ElementProperty::Boolean(true)
                .cast(&ValueType::Int64)
                .unwrap(),
            int(1)
        );
        assert_eq!(
            ElementProperty::Boolean(false)
                .cast(&ValueType::Float64)
                .unwrap(),
            float(0.0)
        );
        assert_eq!(
            ElementProperty::Boolean(true)
                .cast(&ValueType::Float64)
                .unwrap(),
            float(1.0)
        );
        assert_eq!(
            ElementProperty::Boolean(true)
                .cast(&ValueType::Boolean)
                .unwrap(),
            ElementProperty::Boolean(true)
        );
    }

    #[test]
    fn number_to_boolean_takes_zero_and_one_only() {
        assert_eq!(
            int(0).cast(&ValueType::Boolean).unwrap(),
            ElementProperty::Boolean(false)
        );
        assert_eq!(
            int(1).cast(&ValueType::Boolean).unwrap(),
            ElementProperty::Boolean(true)
        );
        assert_eq!(
            float(0.0).cast(&ValueType::Boolean).unwrap(),
            ElementProperty::Boolean(false)
        );
        assert_eq!(
            float(1.0).cast(&ValueType::Boolean).unwrap(),
            ElementProperty::Boolean(true)
        );
        assert!(int(2).cast(&ValueType::Boolean).is_err());
        assert!(float(0.5).cast(&ValueType::Boolean).is_err());
    }

    #[test]
    fn containers_and_graph_elements_never_cast_to_a_scalar() {
        let containers = [
            ElementProperty::Array(vec![int(1)]),
            ElementProperty::Object(Default::default()),
            ElementProperty::Vertex(Default::default()),
            ElementProperty::Edge(Default::default()),
        ];
        for value in containers {
            for target in [
                ValueType::Boolean,
                ValueType::Int64,
                ValueType::Float64,
                ValueType::String,
            ] {
                assert!(
                    value.cast(&target).is_err(),
                    "{value:?} must not cast to {target}"
                );
            }
        }
    }

    #[test]
    fn a_cast_to_null_fails_for_a_present_value() {
        assert!(text("marko").cast(&ValueType::Null).is_err());
        assert!(int(1).cast(&ValueType::Null).is_err());
    }

    #[test]
    fn container_and_union_targets_fail_loudly() {
        let targets = [
            ValueType::Array(Box::new(ValueType::String)),
            ValueType::Object,
            ValueType::Union(vec![ValueType::Int64, ValueType::String]),
        ];
        for target in targets {
            let err = text("anything")
                .cast(&target)
                .expect_err("{target} must not silently pass the value through");
            assert!(
                matches!(
                    *err,
                    TraverserError::Value(crate::error::ValueError::UnsupportedCastTarget { .. })
                ),
                "{target} must report an unsupported target, got: {err}"
            );
        }
    }

    #[test]
    fn a_failure_reports_the_expected_target_and_the_offending_value() {
        let err = text("12abc")
            .cast(&ValueType::Int64)
            .expect_err("must fail");
        let rendered = err.to_string();
        assert!(rendered.contains("int64"), "missing target in: {rendered}");
        assert!(
            rendered.contains("string"),
            "missing source type in: {rendered}"
        );
    }

    #[test]
    fn type_of_reports_the_exact_logical_type() {
        use crate::graph::schema::type_of;
        assert_eq!(type_of(&text("999")), Some(ValueType::String));
        assert_eq!(type_of(&int(999)), Some(ValueType::Int64));
        assert_eq!(type_of(&float(1.5)), Some(ValueType::Float64));
        assert_eq!(type_of(&ElementProperty::Null), Some(ValueType::Null));
    }

    #[test]
    fn a_canonical_uuid_string_casts_to_uuid() {
        let canonical = "550e8400-e29b-41d4-a716-446655440000";
        let cast = text(canonical).cast(&ValueType::Uuid).unwrap();
        assert_eq!(crate::graph::schema::type_of(&cast), Some(ValueType::Uuid));
        assert_eq!(cast.as_uuid(), Some(0x550e8400_e29b_41d4_a716_446655440000));

        // Casting a UUID to itself, and back to string, round-trips byte-identical.
        assert_eq!(cast.clone().cast(&ValueType::Uuid).unwrap(), cast);
        assert_eq!(cast.cast(&ValueType::String).unwrap(), text(canonical));
    }

    #[test]
    fn a_malformed_uuid_string_fails_the_cast() {
        for malformed in [
            "550e8400-e29b-41d4-a716-44665544000",    // 35 characters
            "550e8400-e29b-41d4-a716-4466554400000",  // 37 characters
            "550e8400-e29b-41d4-a716-44665544000g",   // non-hex character
            "550E8400-E29B-41D4-A716-446655440000",   // uppercase not accepted
            "{550e8400-e29b-41d4-a716-446655440000}", // braces
        ] {
            let err = text(malformed)
                .cast(&ValueType::Uuid)
                .expect_err(&format!("{malformed} must not parse as a UUID"));
            assert!(matches!(
                *err,
                TraverserError::Value(crate::error::ValueError::CastException { .. })
            ));
        }
    }

    #[test]
    fn uuid_never_casts_from_a_non_string_non_uuid_value() {
        for value in [int(1), float(1.0), ElementProperty::Boolean(true)] {
            assert!(value.cast(&ValueType::Uuid).is_err());
        }
    }

    #[test]
    fn the_default_policy_is_to_fail() {
        assert_eq!(CastPolicy::default(), CastPolicy::Error);
    }
}
