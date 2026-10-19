//! The test row and helpers the family files' unit tests share (stage 1c): an [`Env`] over a
//! small fixed graph with every method implemented (`properties`, `endpoints` and a
//! configurable `reserve` included), and helpers that parse `MATCH (n) RETURN <expr>`, compile
//! the expression and evaluate it, so a test reads like the query it checks.
//!
//! The graph (vertex and edge ids overlap, as in the modern graph):
//!
//! | Element | Labels | Properties (stored order) |
//! |---|---|---|
//! | `V(1)` | `person`, `employee` | `name: 'marko'`, `age: 29`, `stored_null: null`, `tags: ['a', 'b']`, `address: {city: 'Berlin'}` |
//! | `V(2)` | `person` | `name: 'vadas'`, `age: 27` |
//! | `V(3)` | `software` | `name: 'lop'`, `lang: 'java'` |
//! | `V(4)` | (none) | (none) |
//! | `E(1)` | `knows`, `V(1) -> V(2)` | `weight: 0.5` |
//! | `E(2)` | `created`, `V(1) -> V(3)` | `weight: 0.4` |
//!
//! Variables ([`VARIABLES`]): `n` = `V(1)`, `m` = `V(2)`, `s` = `V(3)`, `u` = `V(4)`, `e` =
//! `E(1)`, `f` = `E(2)`, `p` = the path `V(1), E(1), V(2)`, `z` = the zero-length path `V(1)`,
//! `x` = `NULL`, `l` = `[1, 2, 3]`, `mp` = `{a: 1, b: 'hi'}`. Parameters: `$s` = `'ma'`,
//! `$nothing` = `NULL`, `$list` = `[1, 2]`, `$big` = `i64::MAX`, `$min` = `i64::MIN`.

#![allow(
    dead_code,
    reason = "shared by the family test modules; some helpers serve later tasks"
)]

use super::super::eval::{Compiled, Env, EvalError, Scope, Value, compile};
use super::super::parser::parse;
use crate::{CompactString, ElementKind, ElementProperty, PropertyMap};

/// A vertex or an edge of the test graph.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum El {
    /// A vertex.
    V(u32),
    /// An edge.
    E(u32),
}

/// The variables of the test row, by slot.
pub(crate) const VARIABLES: &[&str] = &["n", "m", "s", "u", "e", "f", "p", "z", "x", "l", "mp"];

/// The test row; `limit` is the most bytes [`Env::reserve`] grants per call.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TestRow {
    /// The most bytes one `reserve` call may ask for.
    pub(crate) limit: u64,
}

impl Default for TestRow {
    fn default() -> Self {
        TestRow { limit: 1 << 20 }
    }
}

impl TestRow {
    /// A row whose `reserve` grants at most `limit` bytes per call.
    pub(crate) fn with_limit(limit: u64) -> Self {
        TestRow { limit }
    }
}

fn stored(element: &El) -> Vec<(CompactString, ElementProperty)> {
    let entry = |key: &str, value: ElementProperty| (CompactString::from(key), value);
    match element {
        El::V(1) => {
            let mut address = PropertyMap::new();
            address.insert("city", "Berlin".into());
            vec![
                entry("name", "marko".into()),
                entry("age", 29.into()),
                entry("stored_null", ElementProperty::Null),
                entry("tags", vec!["a", "b"].into()),
                entry("address", ElementProperty::Object(Box::new(address))),
            ]
        }
        El::V(2) => vec![entry("name", "vadas".into()), entry("age", 27.into())],
        El::V(3) => vec![entry("name", "lop".into()), entry("lang", "java".into())],
        El::E(1) => vec![entry("weight", 0.5.into())],
        El::E(2) => vec![entry("weight", 0.4.into())],
        _ => Vec::new(),
    }
}

impl Env for TestRow {
    type Element = El;

    fn variable(&self, slot: usize) -> Value<El> {
        match VARIABLES.get(slot).copied() {
            Some("n") => Value::Element(El::V(1)),
            Some("m") => Value::Element(El::V(2)),
            Some("s") => Value::Element(El::V(3)),
            Some("u") => Value::Element(El::V(4)),
            Some("e") => Value::Element(El::E(1)),
            Some("f") => Value::Element(El::E(2)),
            Some("p") => Value::Path(vec![El::V(1), El::E(1), El::V(2)]),
            Some("z") => Value::Path(vec![El::V(1)]),
            Some("l") => list(vec![int(1), int(2), int(3)]),
            Some("mp") => map(&[("a", int(1)), ("b", string("hi"))]),
            _ => Value::Null,
        }
    }

    fn kind(&self, element: &El) -> ElementKind {
        match element {
            El::V(_) => ElementKind::Vertex,
            El::E(_) => ElementKind::Edge,
        }
    }

    fn element_id(&self, element: &El) -> CompactString {
        match element {
            El::V(id) | El::E(id) => id.to_string().into(),
        }
    }

    fn property(&self, element: &El, key: &str) -> Option<ElementProperty> {
        stored(element)
            .into_iter()
            .find(|(name, _)| name.as_str() == key)
            .map(|(_, value)| value)
    }

    fn has_label(&self, element: &El, label: &str) -> bool {
        self.labels(element).iter().any(|l| l == label)
    }

    fn has_any_label(&self, element: &El) -> bool {
        !self.labels(element).is_empty()
    }

    fn labels(&self, element: &El) -> Vec<CompactString> {
        match element {
            El::V(1) => vec!["person".into(), "employee".into()],
            El::V(2) => vec!["person".into()],
            El::V(3) => vec!["software".into()],
            El::E(1) => vec!["knows".into()],
            El::E(2) => vec!["created".into()],
            _ => Vec::new(),
        }
    }

    fn properties(&self, element: &El) -> Vec<(CompactString, ElementProperty)> {
        stored(element)
    }

    fn endpoints(&self, edge: &El) -> Option<(El, El)> {
        match edge {
            El::E(1) => Some((El::V(1), El::V(2))),
            El::E(2) => Some((El::V(1), El::V(3))),
            _ => None,
        }
    }

    fn reserve(&self, bytes: u64) -> bool {
        bytes <= self.limit
    }
}

struct TestScope;

impl Scope for TestScope {
    fn variable(&self, name: &str) -> Option<usize> {
        VARIABLES.iter().position(|v| *v == name)
    }

    fn parameter(&self, name: &str) -> Option<ElementProperty> {
        match name {
            "s" => Some("ma".into()),
            "nothing" => Some(ElementProperty::Null),
            "list" => Some(vec![1i64, 2].into()),
            "big" => Some(i64::MAX.into()),
            "min" => Some(i64::MIN.into()),
            _ => None,
        }
    }
}

/// `expr` compiled from `MATCH (n) RETURN <expr>` (a parse error panics: tests write valid
/// queries; a compile error is returned).
pub(crate) fn compiled(expr: &str) -> Result<Compiled<El>, EvalError> {
    let query = format!("MATCH (n) RETURN {expr}");
    let parsed = match parse(&query) {
        Ok(parsed) => parsed,
        Err(e) => panic!("{query:?} does not parse: {e}"),
    };
    compile(&parsed.ret.items[0].expr, &TestScope)
}

/// `expr` evaluated on `row`.
pub(crate) fn eval_in(row: &TestRow, expr: &str) -> Result<Value<El>, EvalError> {
    compiled(expr)?.eval(row)
}

/// `expr` evaluated on the default row.
pub(crate) fn eval(expr: &str) -> Result<Value<El>, EvalError> {
    eval_in(&TestRow::default(), expr)
}

/// The value of `expr` (an error panics).
pub(crate) fn value(expr: &str) -> Value<El> {
    match eval(expr) {
        Ok(value) => value,
        Err(e) => panic!("{expr:?} failed: {e:?}"),
    }
}

/// The GQLSTATUS of a failing `expr` (a value panics).
pub(crate) fn status_of(expr: &str) -> &'static str {
    match eval(expr) {
        Ok(value) => panic!("{expr:?} gave {value:?}"),
        Err(e) => e.gqlstatus,
    }
}

/// Asserts the value of every `(expr, expected)`.
pub(crate) fn check_values(cases: &[(&str, Value<El>)]) {
    for (expr, expected) in cases {
        assert_eq!(&value(expr), expected, "{expr}");
    }
}

/// Asserts the GQLSTATUS of every `(expr, status)`.
pub(crate) fn check_statuses(cases: &[(&str, &str)]) {
    for (expr, expected) in cases {
        assert_eq!(status_of(expr), *expected, "{expr}");
    }
}

/// `NULL`.
pub(crate) fn null() -> Value<El> {
    Value::Null
}

/// An integer.
pub(crate) fn int(value: i64) -> Value<El> {
    Value::integer(value)
}

/// A float.
pub(crate) fn float(value: f64) -> Value<El> {
    Value::float(value)
}

/// A string.
pub(crate) fn string(text: &str) -> Value<El> {
    Value::string(text)
}

/// A boolean.
pub(crate) fn boolean(value: bool) -> Value<El> {
    Value::boolean(value)
}

/// A list.
pub(crate) fn list(items: Vec<Value<El>>) -> Value<El> {
    Value::List(items)
}

/// A map in the given key order.
pub(crate) fn map(entries: &[(&str, Value<El>)]) -> Value<El> {
    Value::Map(
        entries
            .iter()
            .map(|(key, value)| (CompactString::from(*key), value.clone()))
            .collect(),
    )
}
