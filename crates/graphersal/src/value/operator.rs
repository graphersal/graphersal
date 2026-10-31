//! The TinkerPop `Operator` token and its pure semantics over [`ElementProperty`] values.
//!
//! `Operator` is the binary function behind `withSack(init, Operator)`, `sack(Operator)`,
//! `fold(seed, Operator)` and `withSideEffect(key, init, Operator)`. This module holds only the
//! token and [`Operator::apply`]; the steps that use it come later.

use std::fmt;

use compact_str::CompactString;

use crate::error::{OperatorFailure, ValueError};
use crate::value::scalar::compare_scalars;
use crate::value::{ElementProperty, N, Number, PropertyMap, ValueMap};

/// TinkerPop's `Operator` enum: a binary function over two values.
///
/// Closed by TinkerPop's definition, but `#[non_exhaustive]` (a later TinkerPop release may add
/// one). In the Rhai DSL the tokens are `Operator.sum` / `Operator::sum` and so on.
///
/// Deliberate deviations from TinkerPop 3.7/3.8, all documented in the book page "Sack and
/// operators" and in `tinkerpop_deviations.md`:
///
/// - integers are `int64` only: an integer overflow of `sum`/`minus`/`mult` is an error
///   ([`OperatorFailure::Overflow`]) instead of promotion to a wider type, and `sumLong` does not
///   wrap around;
/// - `div` of two integers truncates toward zero (Java `/`); a float operand gives IEEE results;
/// - `addAll` of a list and a scalar appends the scalar (3.8.2 feature files; 3.7.2 throws).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Operator {
    /// `Operator.sum`: addition of two numbers.
    ///
    /// Null rule inherited from TinkerPop's `NumberHelper`: if either operand is null the result
    /// is the **left** operand, so `sum(null, 1)` is null and `sum(1, null)` is 1. The same rule
    /// holds for `minus`, `mult` and `div`.
    Sum,
    /// `Operator.minus`: subtraction of two numbers (null rule as for [`Operator::Sum`]).
    Minus,
    /// `Operator.mult`: multiplication of two numbers (null rule as for [`Operator::Sum`]).
    Mult,
    /// `Operator.div`: division. Two integers divide as Java does (truncation toward zero; a zero
    /// divisor or `i64::MIN / -1` is an error); with a float operand the division is IEEE (`inf`,
    /// `NaN`, no error). Null rule as for [`Operator::Sum`].
    Div,
    /// `Operator.max`: the larger of two numbers (int and float widen to float) or of two values of
    /// the same scalar kind (string, boolean, uuid). A null operand yields the other operand.
    Max,
    /// `Operator.min`: the smaller of two values, same rules as [`Operator::Max`].
    Min,
    /// `Operator.assign`: the right operand, even when it is null.
    Assign,
    /// `Operator.and`: logical and of two booleans. One null operand yields the other, two nulls
    /// yield null.
    And,
    /// `Operator.or`: logical or of two booleans (null rule as for [`Operator::And`]).
    Or,
    /// `Operator.addAll`: list + list concatenates, map + map is putAll (the right value wins, the
    /// left key order is kept), list + scalar appends the scalar.
    AddAll,
    /// `Operator.sumLong`: addition of two `int64` values; overflow is an error (3.7.2 wraps).
    SumLong,
}

impl Operator {
    /// The TinkerPop spelling of the operator (`"sum"`, ..., `"addAll"`, `"sumLong"`).
    pub fn name(self) -> &'static str {
        match self {
            Operator::Sum => "sum",
            Operator::Minus => "minus",
            Operator::Mult => "mult",
            Operator::Div => "div",
            Operator::Max => "max",
            Operator::Min => "min",
            Operator::Assign => "assign",
            Operator::And => "and",
            Operator::Or => "or",
            Operator::AddAll => "addAll",
            Operator::SumLong => "sumLong",
        }
    }

    /// Applies the operator to two values and returns the result.
    ///
    /// # Errors
    ///
    /// [`ValueError::OperatorFailed`] when an integer operation overflows, an integer is divided by
    /// zero, or the operands are not of the kind the operator accepts.
    pub fn apply(
        self,
        left: &ElementProperty,
        right: &ElementProperty,
    ) -> Result<ElementProperty, ValueError> {
        self.apply_owned(left.clone(), right.clone())
    }

    /// Owning form of [`Operator::apply`]: `addAll` extends the left list in place, so folding a
    /// growing list is linear, not quadratic.
    pub(crate) fn apply_owned(
        self,
        left: ElementProperty,
        right: ElementProperty,
    ) -> Result<ElementProperty, ValueError> {
        match self {
            Operator::Assign => Ok(right),
            Operator::Sum => self.arithmetic(left, right),
            Operator::Minus => self.arithmetic(left, right),
            Operator::Mult => self.arithmetic(left, right),
            Operator::Div => self.arithmetic(left, right),
            Operator::Max | Operator::Min => self.extremum(left, right),
            Operator::And | Operator::Or => self.logic(left, right),
            Operator::AddAll => self.add_all(left, right),
            Operator::SumLong => self.sum_long(left, right),
        }
    }

    fn fail(
        self,
        kind: OperatorFailure,
        left: &ElementProperty,
        right: &ElementProperty,
    ) -> ValueError {
        ValueError::OperatorFailed {
            operator: self.name(),
            kind,
            left: describe(left),
            right: describe(right),
        }
    }

    fn incompatible(
        self,
        expected: &'static str,
        left: &ElementProperty,
        right: &ElementProperty,
    ) -> ValueError {
        self.fail(
            OperatorFailure::IncompatibleOperands { expected },
            left,
            right,
        )
    }

    fn arithmetic(
        self,
        left: ElementProperty,
        right: ElementProperty,
    ) -> Result<ElementProperty, ValueError> {
        if matches!(left, ElementProperty::Null) || matches!(right, ElementProperty::Null) {
            return Ok(left);
        }
        let (ElementProperty::Number(a), ElementProperty::Number(b)) = (&left, &right) else {
            return Err(self.incompatible("number", &left, &right));
        };
        let result = match (&a.n, &b.n) {
            (N::NegInt(a), N::NegInt(b)) => {
                let (a, b) = (*a, *b);
                let checked = match self {
                    Operator::Sum => a.checked_add(b),
                    Operator::Minus => a.checked_sub(b),
                    Operator::Mult => a.checked_mul(b),
                    _ => {
                        if b == 0 {
                            return Err(self.fail(OperatorFailure::DivisionByZero, &left, &right));
                        }
                        a.checked_div(b)
                    }
                };
                match checked {
                    Some(value) => Number::from(value),
                    None => return Err(self.fail(OperatorFailure::Overflow, &left, &right)),
                }
            }
            (a, b) => {
                let (a, b) = (as_f64(a), as_f64(b));
                Number::from(match self {
                    Operator::Sum => a + b,
                    Operator::Minus => a - b,
                    Operator::Mult => a * b,
                    _ => a / b,
                })
            }
        };
        Ok(ElementProperty::Number(result))
    }

    fn extremum(
        self,
        left: ElementProperty,
        right: ElementProperty,
    ) -> Result<ElementProperty, ValueError> {
        if matches!(left, ElementProperty::Null) {
            return Ok(right);
        }
        if matches!(right, ElementProperty::Null) {
            return Ok(left);
        }
        let want_max = self == Operator::Max;
        if let (ElementProperty::Number(a), ElementProperty::Number(b)) = (&left, &right) {
            return Ok(ElementProperty::Number(match (&a.n, &b.n) {
                (N::NegInt(x), N::NegInt(y)) => {
                    Number::from(if want_max { *x.max(y) } else { *x.min(y) })
                }
                (x, y) => {
                    let (x, y) = (as_f64(x), as_f64(y));
                    // Java's Math.min/max: NaN wins.
                    Number::from(if x.is_nan() || y.is_nan() {
                        f64::NAN
                    } else if want_max {
                        x.max(y)
                    } else {
                        x.min(y)
                    })
                }
            }));
        }
        match compare_scalars(&left, &right) {
            Some(ordering) => {
                let left_wins = if want_max {
                    ordering.is_ge()
                } else {
                    ordering.is_le()
                };
                Ok(if left_wins { left } else { right })
            }
            None => Err(self.incompatible(
                "two numbers or two values of one scalar kind",
                &left,
                &right,
            )),
        }
    }

    fn logic(
        self,
        left: ElementProperty,
        right: ElementProperty,
    ) -> Result<ElementProperty, ValueError> {
        match (&left, &right) {
            (ElementProperty::Null, _) => {
                if matches!(right, ElementProperty::Null | ElementProperty::Boolean(_)) {
                    Ok(right)
                } else {
                    Err(self.incompatible("boolean", &left, &right))
                }
            }
            (_, ElementProperty::Null) => {
                if matches!(left, ElementProperty::Boolean(_)) {
                    Ok(left)
                } else {
                    Err(self.incompatible("boolean", &left, &right))
                }
            }
            (ElementProperty::Boolean(a), ElementProperty::Boolean(b)) => {
                Ok(ElementProperty::Boolean(if self == Operator::And {
                    *a && *b
                } else {
                    *a || *b
                }))
            }
            _ => Err(self.incompatible("boolean", &left, &right)),
        }
    }

    fn sum_long(
        self,
        left: ElementProperty,
        right: ElementProperty,
    ) -> Result<ElementProperty, ValueError> {
        if let (ElementProperty::Number(a), ElementProperty::Number(b)) = (&left, &right)
            && let (N::NegInt(a), N::NegInt(b)) = (&a.n, &b.n)
        {
            return match a.checked_add(*b) {
                Some(sum) => Ok(ElementProperty::Number(Number::from(sum))),
                None => Err(self.fail(OperatorFailure::Overflow, &left, &right)),
            };
        }
        Err(self.incompatible("two int64 values", &left, &right))
    }

    fn add_all(
        self,
        left: ElementProperty,
        right: ElementProperty,
    ) -> Result<ElementProperty, ValueError> {
        match (left, right) {
            (ElementProperty::Array(mut items), ElementProperty::Array(more)) => {
                items.extend(more);
                Ok(ElementProperty::Array(items))
            }
            (ElementProperty::Array(mut items), right) if is_scalar(&right) => {
                items.push(right);
                Ok(ElementProperty::Array(items))
            }
            (ElementProperty::Object(mut target), ElementProperty::Object(more)) => {
                for (key, value) in *more {
                    target.insert(key, value);
                }
                Ok(ElementProperty::Object(target))
            }
            (
                left @ (ElementProperty::Object(_) | ElementProperty::Map(_)),
                right @ (ElementProperty::Object(_) | ElementProperty::Map(_)),
            ) => {
                let mut target = into_value_map(left);
                for (key, value) in into_value_map(right) {
                    target.insert(key, value);
                }
                Ok(target.into_property())
            }
            (left, right) => {
                Err(self.incompatible("two lists, two maps, or a list and a value", &left, &right))
            }
        }
    }
}

impl fmt::Display for Operator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

fn as_f64(n: &N) -> f64 {
    match n {
        N::NegInt(value) => *value as f64,
        N::Float(value) => **value,
    }
}

fn is_scalar(value: &ElementProperty) -> bool {
    matches!(
        value,
        ElementProperty::Null
            | ElementProperty::String(_)
            | ElementProperty::Number(_)
            | ElementProperty::Boolean(_)
            | ElementProperty::Uuid(_)
            | ElementProperty::Temporal(_)
    )
}

fn into_value_map(value: ElementProperty) -> ValueMap {
    match value {
        ElementProperty::Object(object) => {
            let object: PropertyMap = *object;
            object
                .into_iter()
                .map(|(key, value): (CompactString, ElementProperty)| {
                    (ElementProperty::String(key), value)
                })
                .collect()
        }
        ElementProperty::Map(map) => *map,
        _ => ValueMap::new(),
    }
}

/// `value (kind)` for a diagnostic message.
pub(crate) fn describe(value: &ElementProperty) -> String {
    let kind = match value {
        ElementProperty::Null => "null",
        ElementProperty::String(_)
        | ElementProperty::Compressed(_)
        | ElementProperty::Deduped(_) => "string",
        ElementProperty::Number(n) if n.is_i64() => "int64",
        ElementProperty::Number(_) => "float64",
        ElementProperty::Boolean(_) => "boolean",
        ElementProperty::Uuid(_) => "uuid",
        ElementProperty::Temporal(value) => value.kind().name(),
        ElementProperty::Vector(_) => "vector",
        ElementProperty::Blob(_) => "blob",
        ElementProperty::Object(_) | ElementProperty::Map(_) => "map",
        ElementProperty::Array(_) => "list",
        ElementProperty::Vertex(_) => "vertex",
        ElementProperty::Edge(_) => "edge",
    };
    let mut rendered = crate::value::literal::rhai_literal(value);
    if rendered.chars().count() > 48 {
        rendered = rendered.chars().take(48).collect::<String>() + "…";
    }
    format!("{rendered} ({kind})")
}

#[cfg(test)]
mod tests {
    use super::Operator;

    #[test]
    fn display_is_the_tinkerpop_name() {
        assert_eq!(Operator::AddAll.to_string(), "addAll");
        assert_eq!(Operator::SumLong.to_string(), "sumLong");
    }
}
