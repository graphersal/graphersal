//! Lists, elements and paths: `head`, `last`, `tail`, GQL's list `trim(list, n)`,
//! `range`, `keys`, `properties`, `startNode`, `endNode`, `nodes`, `relationships`, and the GQL
//! predicates `IS [NOT] SOURCE OF` / `IS [NOT] DESTINATION OF`.
//!
//! The contract of a family file is in the module docs of `functions/mod.rs`; unit tests use
//! the test `Env` and helpers of `functions/test_env.rs`.
//!
//! - `head` / `last` / `tail` / `trim(list, n)` take a list (anything else is 22G03): `head([])`
//!   and `last([])` are `NULL`, `tail([])` is `[]`, `trim(list, n)` drops the last `n` items
//!   (all of them when `n` exceeds the size; a negative `n` is 22023).
//! - `range(start, end [, step])` takes integers (a float is 22G03, design F3), includes `end`
//!   when the steps reach it, is empty when `step` points away from `end`, and refuses
//!   `step = 0` (22023). The item count is computed in `i128` and reserved (design F6) before
//!   the list is built.
//! - `keys` / `properties` read a vertex's or an edge's stored properties in stored order, a
//!   stored `null` being no property; on a map they give its keys / the map itself.
//! - `startNode` / `endNode` and the predicates read the stored endpoints of an edge, whatever
//!   the direction the pattern matched it in.
//! - `nodes` / `relationships` split a path into its vertices (even positions) and its edges
//!   (odd positions).

use super::super::ast::Endpoint;
use super::super::eval::{Env, EvalError, Truth, Value, status};
use super::super::lexer::Span;
use super::super::parser::Function;
use super::{
    Arg, any_null, function_not_implemented, integer_arg, list_bytes, reserve, wrong_kind,
};
use crate::{ElementKind, ElementProperty};

/// Evaluates the list, element or path function `function` (its family,
/// [`super::Family::Lists`], and `trim` with a list first argument) on its evaluated arguments.
pub(crate) fn call<En: Env>(
    function: Function,
    args: Vec<Arg<En::Element>>,
    env: &En,
    span: Span,
) -> Result<Value<En::Element>, EvalError> {
    if any_null(&args) {
        return Ok(Value::Null);
    }
    match function {
        Function::Head | Function::Last | Function::Tail => {
            let mut args = args.into_iter();
            let Some(arg) = args.next() else {
                return Ok(Value::Null);
            };
            let Value::List(items) = arg.value else {
                return Err(wrong_kind(function, "a list", &arg));
            };
            Ok(if function == Function::Head {
                items.into_iter().next().unwrap_or(Value::Null)
            } else if function == Function::Last {
                items.into_iter().next_back().unwrap_or(Value::Null)
            } else {
                Value::List(items.into_iter().skip(1).collect())
            })
        }
        Function::Trim => list_trim(args),
        Function::Range => range(args, env, span),
        Function::Keys | Function::Properties => keys_or_properties(function, args, env),
        Function::StartNode | Function::EndNode => {
            let Some(arg) = args.into_iter().next() else {
                return Ok(Value::Null);
            };
            let end = if function == Function::StartNode {
                Endpoint::Source
            } else {
                Endpoint::Destination
            };
            match &arg.value {
                Value::Element(element) if env.kind(element) == ElementKind::Edge => {
                    Ok(endpoint(env, element, end).map_or(Value::Null, Value::Element))
                }
                Value::Element(_) => Err(EvalError::invalid_type(
                    format!(
                        "{}() takes an edge, not a node; a node is its own endpoint",
                        name_of(function)
                    ),
                    arg.span,
                )),
                _ => Err(wrong_kind(function, "an edge", &arg)),
            }
        }
        Function::Nodes | Function::Relationships => {
            let Some(arg) = args.into_iter().next() else {
                return Ok(Value::Null);
            };
            let Value::Path(elements) = arg.value else {
                return Err(wrong_kind(function, "a path", &arg));
            };
            // Vertices at the even positions, edges at the odd ones.
            let skip = usize::from(function == Function::Relationships);
            Ok(Value::List(
                elements
                    .into_iter()
                    .skip(skip)
                    .step_by(2)
                    .map(Value::Element)
                    .collect(),
            ))
        }
        Function::ElementId
        | Function::Labels
        | Function::Type
        | Function::Coalesce
        | Function::Upper
        | Function::Lower
        | Function::CharLength
        | Function::Size
        | Function::Cardinality
        | Function::PathLength
        | Function::Count
        | Function::Sum
        | Function::Avg
        | Function::Min
        | Function::Max
        | Function::CollectList
        | Function::PercentileCont
        | Function::PercentileDisc
        | Function::Cast
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
        | Function::Temporal(_) => Err(function_not_implemented(function, span)),
    }
}

/// `operand IS [NOT] SOURCE OF edge` / `operand IS [NOT] DESTINATION OF edge`; `span` is the
/// whole predicate.
pub(crate) fn is_endpoint<En: Env>(
    operand: &Value<En::Element>,
    edge: &Value<En::Element>,
    end: Endpoint,
    negated: bool,
    env: &En,
    span: Span,
) -> Result<Value<En::Element>, EvalError> {
    let predicate = match end {
        Endpoint::Source => "IS SOURCE OF",
        Endpoint::Destination => "IS DESTINATION OF",
    };
    if operand.is_null() || edge.is_null() {
        return Ok(Value::Null);
    }
    let vertex = match operand {
        Value::Element(vertex) if env.kind(vertex) == ElementKind::Vertex => vertex,
        other => {
            return Err(EvalError::invalid_type(
                format!(
                    "{predicate} takes a node on its left, not {}",
                    describe(env, other)
                ),
                span,
            ));
        }
    };
    let edge = match edge {
        Value::Element(edge) if env.kind(edge) == ElementKind::Edge => edge,
        other => {
            return Err(EvalError::invalid_type(
                format!(
                    "{predicate} takes an edge on its right, not {}",
                    describe(env, other)
                ),
                span,
            ));
        }
    };
    let truth = match endpoint(env, edge, end) {
        Some(found) => Truth::of(found == *vertex),
        None => Truth::Unknown,
    };
    Ok(if negated { truth.not() } else { truth }.into_value())
}

/// The canonical name of `function` (for messages).
fn name_of(function: Function) -> &'static str {
    super::super::eval::function_name(function)
}

/// The kind of `value` for a message, telling a node from an edge.
fn describe<En: Env>(env: &En, value: &Value<En::Element>) -> &'static str {
    match value {
        Value::Element(element) => match env.kind(element) {
            ElementKind::Vertex => "a node",
            ElementKind::Edge => "an edge",
        },
        other => other.kind_name(),
    }
}

/// The stored source or target of `edge`; `None` when the storage gives no endpoints.
fn endpoint<En: Env>(env: &En, edge: &En::Element, end: Endpoint) -> Option<En::Element> {
    let (source, target) = env.endpoints(edge)?;
    Some(match end {
        Endpoint::Source => source,
        Endpoint::Destination => target,
    })
}

/// GQL's list `trim(list, n)`: the list without its last `n` items.
fn list_trim<E>(args: Vec<Arg<E>>) -> Result<Value<E>, EvalError> {
    let mut args = args.into_iter();
    let (Some(list), Some(count)) = (args.next(), args.next()) else {
        return Ok(Value::Null);
    };
    let Value::List(mut items) = list.value else {
        return Err(wrong_kind(Function::Trim, "a list", &list));
    };
    let n = integer_arg(Function::Trim, &count, "an integer count")?;
    let Ok(n) = usize::try_from(n) else {
        return Err(EvalError::new(
            status::INVALID_PARAMETER,
            format!("trim(list, n) takes a count of zero or more, not {n}"),
            count.span,
        ));
    };
    items.truncate(items.len().saturating_sub(n));
    Ok(Value::List(items))
}

/// `range(start, end [, step])`.
fn range<En: Env>(
    args: Vec<Arg<En::Element>>,
    env: &En,
    span: Span,
) -> Result<Value<En::Element>, EvalError> {
    let function = Function::Range;
    let (Some(start), Some(end)) = (args.first(), args.get(1)) else {
        return Ok(Value::Null);
    };
    let start = integer_arg(function, start, "integer bounds")?;
    let end = integer_arg(function, end, "integer bounds")?;
    let step = match args.get(2) {
        Some(arg) => {
            let step = integer_arg(function, arg, "an integer step")?;
            if step == 0 {
                return Err(EvalError::new(
                    status::INVALID_PARAMETER,
                    "range() takes a step other than 0",
                    arg.span,
                ));
            }
            step
        }
        None => 1,
    };
    let (start, end, step) = (i128::from(start), i128::from(end), i128::from(step));
    // The number of items: none when the step points away from `end`.
    let count = if (step > 0 && end < start) || (step < 0 && end > start) {
        0
    } else {
        (end - start) / step + 1
    };
    let bytes = list_bytes(u64::try_from(count).unwrap_or(u64::MAX));
    reserve(env, bytes, span)?;
    let Ok(len) = usize::try_from(count) else {
        return Err(EvalError::new(
            status::LIMIT,
            format!("range() would build {count} items, more than this platform can hold"),
            span,
        ));
    };
    let mut items = Vec::with_capacity(len);
    let mut current = start;
    for _ in 0..len {
        // Every item lies between `start` and `end`, both `i64`.
        let Ok(item) = i64::try_from(current) else {
            return Err(EvalError::out_of_range(span));
        };
        items.push(Value::integer(item));
        current += step;
    }
    Ok(Value::List(items))
}

/// `keys(x)` / `properties(x)` of a vertex, an edge or a map.
fn keys_or_properties<En: Env>(
    function: Function,
    args: Vec<Arg<En::Element>>,
    env: &En,
) -> Result<Value<En::Element>, EvalError> {
    let keys = function == Function::Keys;
    let Some(arg) = args.into_iter().next() else {
        return Ok(Value::Null);
    };
    match arg.value {
        Value::Element(element) => {
            let stored = env.properties(&element);
            let mut out_keys = Vec::new();
            let mut out_map = Vec::new();
            for (key, value) in stored {
                // A stored `null` is no property (it reads as NULL).
                if matches!(value, ElementProperty::Null) {
                    continue;
                }
                if keys {
                    out_keys.push(Value::string(key));
                } else {
                    out_map.push((key, Value::from_property(value)));
                }
            }
            Ok(if keys {
                Value::List(out_keys)
            } else {
                Value::Map(out_map)
            })
        }
        Value::Map(entries) if keys => Ok(Value::List(
            entries
                .into_iter()
                .map(|(key, _)| Value::string(key))
                .collect(),
        )),
        Value::Map(entries) => Ok(Value::Map(entries)),
        _ => Err(wrong_kind(function, "a node, an edge or a map", &arg)),
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::eval::status;
    use super::super::test_env::*;

    fn v(id: u32) -> super::Value<El> {
        super::Value::Element(El::V(id))
    }

    fn e(id: u32) -> super::Value<El> {
        super::Value::Element(El::E(id))
    }

    #[test]
    fn head_last_tail() {
        check_values(&[
            ("head([1, 2, 3])", int(1)),
            ("last([1, 2, 3])", int(3)),
            ("tail([1, 2, 3])", list(vec![int(2), int(3)])),
            ("head([])", null()),
            ("last([])", null()),
            ("tail([])", list(vec![])),
            ("tail([1])", list(vec![])),
            ("head(l)", int(1)),
            ("last(l)", int(3)),
            ("head([NULL, 1])", null()),
            ("last(['a', [1, 2]])", list(vec![int(1), int(2)])),
            ("head(x)", null()),
            ("last($nothing)", null()),
            ("tail(x)", null()),
            ("head($list)", int(1)),
        ]);
        check_statuses(&[
            ("head('abc')", status::INVALID_VALUE_TYPE),
            ("last(1)", status::INVALID_VALUE_TYPE),
            ("tail(mp)", status::INVALID_VALUE_TYPE),
            ("head(p)", status::INVALID_VALUE_TYPE),
            ("tail(n)", status::INVALID_VALUE_TYPE),
        ]);
        let error = eval("head('abc')").unwrap_err();
        assert_eq!(error.message, "head() takes a list, not a string");
    }

    #[test]
    fn list_trim_drops_the_last_items() {
        check_values(&[
            ("trim([1, 2, 3], 1)", list(vec![int(1), int(2)])),
            ("trim([1, 2, 3], 0)", list(vec![int(1), int(2), int(3)])),
            ("trim([1, 2, 3], 3)", list(vec![])),
            ("trim([1, 2, 3], 7)", list(vec![])),
            ("trim([1, 2, 3], $big)", list(vec![])),
            ("trim([], 1)", list(vec![])),
            ("trim(l, NULL)", null()),
            ("trim([1], x)", null()),
        ]);
        check_statuses(&[
            ("trim([1, 2], -1)", status::INVALID_PARAMETER),
            ("trim([1, 2], $min)", status::INVALID_PARAMETER),
            ("trim([1, 2], 1.0)", status::INVALID_VALUE_TYPE),
            ("trim([1, 2], '1')", status::INVALID_VALUE_TYPE),
        ]);
    }

    #[test]
    fn range_counts_like_neo4j() {
        let ints = |values: &[i64]| list(values.iter().map(|v| int(*v)).collect());
        check_values(&[
            ("range(0, 5)", ints(&[0, 1, 2, 3, 4, 5])),
            ("range(0, 10, 3)", ints(&[0, 3, 6, 9])),
            ("range(10, 0, -3)", ints(&[10, 7, 4, 1])),
            ("range(2, 2)", ints(&[2])),
            ("range(2, 2, -5)", ints(&[2])),
            ("range(5, 0)", ints(&[])),
            ("range(0, 5, -1)", ints(&[])),
            ("range(-2, 2, 2)", ints(&[-2, 0, 2])),
            (
                "range($big - 2, $big)",
                ints(&[i64::MAX - 2, i64::MAX - 1, i64::MAX]),
            ),
            (
                "range($min, $min + 4, 2)",
                ints(&[i64::MIN, i64::MIN + 2, i64::MIN + 4]),
            ),
            ("range($big, $min, $min)", ints(&[i64::MAX, -1])),
            (
                "range($min, $big, $big)",
                ints(&[i64::MIN, -1, i64::MAX - 1]),
            ),
            ("range(NULL, 2)", null()),
            ("range(0, x)", null()),
            ("range(0, 2, NULL)", null()),
            // NULL wins over a step of 0 (design F2).
            ("range(NULL, 2, 0)", null()),
        ]);
        check_statuses(&[
            ("range(0, 5, 0)", status::INVALID_PARAMETER),
            ("range(0.0, 5)", status::INVALID_VALUE_TYPE),
            ("range(0, 5.5)", status::INVALID_VALUE_TYPE),
            ("range(0, 5, 1.0)", status::INVALID_VALUE_TYPE),
            ("range('0', 5)", status::INVALID_VALUE_TYPE),
            // Reserved before building: never an allocation of 2^63 items.
            ("range(0, $big)", status::LIMIT),
            ("range($min, $big)", status::LIMIT),
            ("range($big, $min, -1)", status::LIMIT),
        ]);
        // The reservation is the list's size, refused by a tight row.
        let tight = TestRow::with_limit(super::list_bytes(3));
        assert_eq!(eval_in(&tight, "range(1, 3)"), Ok(ints(&[1, 2, 3])));
        let error = eval_in(&tight, "range(1, 4)").unwrap_err();
        assert_eq!(error.gqlstatus, status::LIMIT);
        assert_eq!(eval_in(&tight, "range(4, 1)"), Ok(ints(&[])));
        let error = eval("range(0, 5, 0)").unwrap_err();
        assert_eq!(error.message, "range() takes a step other than 0");
    }

    #[test]
    fn keys_and_properties_of_elements_and_maps() {
        let keys = |names: &[&str]| list(names.iter().map(|k| string(k)).collect());
        check_values(&[
            // Stored order; the stored null is no key; labels never appear.
            ("keys(n)", keys(&["name", "age", "tags", "address"])),
            ("keys(m)", keys(&["name", "age"])),
            ("keys(e)", keys(&["weight"])),
            ("keys(u)", keys(&[])),
            ("keys(mp)", keys(&["a", "b"])),
            ("keys({b: 1, a: NULL})", keys(&["b", "a"])),
            ("keys({})", keys(&[])),
            ("keys(x)", null()),
            (
                "properties(n)",
                map(&[
                    ("name", string("marko")),
                    ("age", int(29)),
                    ("tags", list(vec![string("a"), string("b")])),
                    ("address", map(&[("city", string("Berlin"))])),
                ]),
            ),
            ("properties(f)", map(&[("weight", float(0.4))])),
            ("properties(u)", map(&[])),
            ("properties(mp)", map(&[("a", int(1)), ("b", string("hi"))])),
            ("properties(x)", null()),
            ("properties(n).address.city", string("Berlin")),
        ]);
        check_statuses(&[
            ("keys(l)", status::INVALID_VALUE_TYPE),
            ("keys('a')", status::INVALID_VALUE_TYPE),
            ("keys(p)", status::INVALID_VALUE_TYPE),
            ("properties(1)", status::INVALID_VALUE_TYPE),
            ("properties(z)", status::INVALID_VALUE_TYPE),
        ]);
        let error = eval("properties(p)").unwrap_err();
        assert_eq!(
            error.message,
            "properties() takes a node, an edge or a map, not a path"
        );
    }

    #[test]
    fn start_node_and_end_node_are_the_stored_endpoints() {
        check_values(&[
            ("startNode(e)", v(1)),
            ("endNode(e)", v(2)),
            ("endNode(f)", v(3)),
            ("startNode(x)", null()),
            ("endNode($nothing)", null()),
            ("startNode(e) = n", boolean(true)),
            ("properties(endNode(f)).lang", string("java")),
        ]);
        check_statuses(&[
            ("startNode(n)", status::INVALID_VALUE_TYPE),
            ("endNode(p)", status::INVALID_VALUE_TYPE),
            ("endNode(1)", status::INVALID_VALUE_TYPE),
        ]);
        let error = eval("startNode(n)").unwrap_err();
        assert_eq!(
            error.message,
            "startNode() takes an edge, not a node; a node is its own endpoint"
        );
    }

    #[test]
    fn nodes_and_relationships_split_a_path() {
        check_values(&[
            ("nodes(p)", list(vec![v(1), v(2)])),
            ("relationships(p)", list(vec![e(1)])),
            ("nodes(z)", list(vec![v(1)])),
            ("relationships(z)", list(vec![])),
            ("nodes(x)", null()),
            ("relationships($nothing)", null()),
            ("size(nodes(p))", int(2)),
            ("head(relationships(p)) = e", boolean(true)),
            ("last(nodes(p)) = m", boolean(true)),
        ]);
        check_statuses(&[
            ("nodes(n)", status::INVALID_VALUE_TYPE),
            ("relationships(e)", status::INVALID_VALUE_TYPE),
            ("nodes([n, m])", status::INVALID_VALUE_TYPE),
        ]);
    }

    #[test]
    fn is_source_of_and_is_destination_of() {
        check_values(&[
            ("n IS SOURCE OF e", boolean(true)),
            ("m IS SOURCE OF e", boolean(false)),
            ("m IS DESTINATION OF e", boolean(true)),
            ("n IS DESTINATION OF e", boolean(false)),
            ("n IS NOT SOURCE OF e", boolean(false)),
            ("m IS NOT SOURCE OF e", boolean(true)),
            ("s IS NOT DESTINATION OF e", boolean(true)),
            ("s IS DESTINATION OF f", boolean(true)),
            ("x IS SOURCE OF e", null()),
            ("n IS SOURCE OF x", null()),
            ("x IS NOT DESTINATION OF e", null()),
            // NULL before the kind check (design F2).
            ("x IS SOURCE OF n", null()),
            ("startNode(e) IS SOURCE OF e", boolean(true)),
        ]);
        check_statuses(&[
            ("e IS SOURCE OF e", status::INVALID_VALUE_TYPE),
            ("n IS SOURCE OF m", status::INVALID_VALUE_TYPE),
            ("1 IS DESTINATION OF e", status::INVALID_VALUE_TYPE),
            ("p IS SOURCE OF e", status::INVALID_VALUE_TYPE),
            ("n IS NOT DESTINATION OF p", status::INVALID_VALUE_TYPE),
        ]);
        let error = eval("n IS SOURCE OF m").unwrap_err();
        assert_eq!(
            error.message,
            "IS SOURCE OF takes an edge on its right, not a node"
        );
        let error = eval("e IS DESTINATION OF e").unwrap_err();
        assert_eq!(
            error.message,
            "IS DESTINATION OF takes a node on its left, not an edge"
        );
    }

    #[test]
    fn a_default_env_gives_null_endpoints() {
        // An `Env` without `endpoints` (its default) answers NULL, never an error.
        use super::super::super::eval::Env;
        use crate::{CompactString, ElementKind, ElementProperty};

        struct Bare;
        impl Env for Bare {
            type Element = El;
            fn variable(&self, _: usize) -> super::Value<El> {
                super::Value::Null
            }
            fn kind(&self, element: &El) -> ElementKind {
                match element {
                    El::V(_) => ElementKind::Vertex,
                    El::E(_) => ElementKind::Edge,
                }
            }
            fn element_id(&self, _: &El) -> CompactString {
                CompactString::default()
            }
            fn property(&self, _: &El, _: &str) -> Option<ElementProperty> {
                None
            }
            fn has_label(&self, _: &El, _: &str) -> bool {
                false
            }
            fn has_any_label(&self, _: &El) -> bool {
                false
            }
            fn labels(&self, _: &El) -> Vec<CompactString> {
                Vec::new()
            }
        }
        let span = super::Span {
            start: 0,
            end: 0,
            line: 1,
            column: 1,
        };
        let edge = super::Arg { value: e(1), span };
        assert_eq!(
            super::call(super::Function::StartNode, vec![edge], &Bare, span),
            Ok(null())
        );
        assert_eq!(
            super::is_endpoint(&v(1), &e(1), super::Endpoint::Source, true, &Bare, span),
            Ok(null())
        );
    }
}
