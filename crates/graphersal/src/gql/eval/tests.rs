use super::*;
use crate::gql::parser::parse;

// ----- a mock row and graph -----

#[derive(Debug, Clone, Copy, PartialEq)]
enum El {
    V(u32),
    E(u32),
}

struct Row;

const VARIABLES: &[&str] = &["n", "m", "e", "x", "f"];

impl Env for Row {
    type Element = El;

    fn variable(&self, slot: usize) -> Value<El> {
        match slot {
            0 => Value::Element(El::V(1)),
            1 => Value::Element(El::V(2)),
            2 => Value::Element(El::E(1)),
            4 => Value::Element(El::E(2)),
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
        // Vertex and edge ids overlap, as in the modern graph.
        match element {
            El::V(id) | El::E(id) => id.to_string().into(),
        }
    }

    fn property(&self, element: &El, key: &str) -> Option<ElementProperty> {
        match (element, key) {
            (El::V(1), "name") => Some("marko".into()),
            (El::V(1), "age") => Some(29.into()),
            (El::V(1), "stored_null") => Some(ElementProperty::Null),
            (El::V(1), "tags") => Some(vec!["a", "b"].into()),
            (El::V(1), "address") => {
                let mut map = PropertyMap::new();
                map.insert("city", "Berlin".into());
                Some(ElementProperty::Object(Box::new(map)))
            }
            (El::E(1), "weight") => Some(0.5.into()),
            _ => None,
        }
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
            El::E(1) => vec!["knows".into()],
            El::E(2) => vec!["created".into()],
            _ => Vec::new(),
        }
    }
}

struct Names;

impl Scope for Names {
    fn variable(&self, name: &str) -> Option<usize> {
        VARIABLES.iter().position(|v| *v == name)
    }

    fn parameter(&self, name: &str) -> Option<ElementProperty> {
        match name {
            "p" => Some(30.into()),
            "s" => Some("ma".into()),
            "nothing" => Some(ElementProperty::Null),
            "list" => Some(vec![1i64, 2].into()),
            _ => None,
        }
    }
}

fn compiled(expr: &str) -> Result<Compiled<El>, EvalError> {
    let query = format!("MATCH (n) RETURN {expr}");
    let parsed = match parse(&query) {
        Ok(parsed) => parsed,
        Err(e) => panic!("{query:?} does not parse: {e}"),
    };
    compile(&parsed.ret.items[0].expr, &Names)
}

fn eval(expr: &str) -> Result<Value<El>, EvalError> {
    compiled(expr)?.eval(&Row)
}

fn value(expr: &str) -> Value<El> {
    match eval(expr) {
        Ok(value) => value,
        Err(e) => panic!("{expr:?} failed: {e:?}"),
    }
}

fn truth(expr: &str) -> Truth {
    match compiled(expr).and_then(|c| c.truth(&Row)) {
        Ok(truth) => truth,
        Err(e) => panic!("{expr:?} failed: {e:?}"),
    }
}

/// The GQLSTATUS of a failing expression.
fn status_of(expr: &str) -> &'static str {
    match eval(expr) {
        Ok(value) => panic!("{expr:?} gave {value:?}"),
        Err(e) => e.gqlstatus,
    }
}

fn int(value: i64) -> Value<El> {
    Value::integer(value)
}

fn float(value: f64) -> Value<El> {
    Value::Scalar(ElementProperty::Number(Number::from_f64(value)))
}

fn string(text: &str) -> Value<El> {
    Value::string(text)
}

fn boolean(flag: bool) -> Value<El> {
    Value::Scalar(ElementProperty::Boolean(flag))
}

use Truth::{False as F, True as T, Unknown as U};

fn literal(truth: Truth) -> &'static str {
    match truth {
        T => "true",
        F => "false",
        U => "null",
    }
}

// ----- three-valued logic -----

#[test]
fn truth_tables_are_complete() {
    let all = [T, F, U];
    for a in all {
        assert_eq!(truth(&format!("NOT {}", literal(a))), a.not());
        for b in all {
            let (la, lb) = (literal(a), literal(b));
            let and = match (a, b) {
                (F, _) | (_, F) => F,
                (T, T) => T,
                _ => U,
            };
            let or = match (a, b) {
                (T, _) | (_, T) => T,
                (F, F) => F,
                _ => U,
            };
            let xor = match (a, b) {
                (U, _) | (_, U) => U,
                _ => Truth::of(a != b),
            };
            assert_eq!(truth(&format!("{la} AND {lb}")), and, "{la} AND {lb}");
            assert_eq!(truth(&format!("{la} OR {lb}")), or, "{la} OR {lb}");
            assert_eq!(truth(&format!("{la} XOR {lb}")), xor, "{la} XOR {lb}");
            // The value form agrees with the condition form.
            assert_eq!(value(&format!("{la} AND {lb}")), and.into_value());
            assert_eq!(value(&format!("{la} OR {lb}")), or.into_value());
            assert_eq!(value(&format!("{la} XOR {lb}")), xor.into_value());
        }
    }
    assert_eq!(value("NOT null"), Value::Null);
}

#[test]
fn and_or_short_circuit_and_non_booleans_are_errors() {
    // The right side would fail (division by zero) but is never evaluated.
    assert_eq!(truth("false AND 1 / 0 = 1"), F);
    assert_eq!(truth("true OR 1 / 0 = 1"), T);
    assert_eq!(status_of("null AND 1 / 0 = 1"), "22012");
    assert_eq!(status_of("1 AND true"), "22G03");
    assert_eq!(status_of("NOT 'x'"), "22G03");
    match compiled("n.name").and_then(|c| c.truth(&Row)) {
        Err(e) => assert_eq!(e.gqlstatus, "22G03"),
        Ok(t) => panic!("a string condition gave {t:?}"),
    }
    assert_eq!(truth("n.missing"), U);
}

#[test]
fn is_null_and_is_not_null() {
    assert_eq!(truth("null IS NULL"), T);
    assert_eq!(truth("null IS NOT NULL"), F);
    assert_eq!(truth("1 IS NULL"), F);
    assert_eq!(truth("n.missing IS NULL"), T);
    assert_eq!(truth("n.stored_null IS NULL"), T);
    assert_eq!(truth("n.name IS NOT NULL"), T);
    assert_eq!(truth("x IS NULL"), T);
    assert_eq!(truth("n IS NULL"), F);
}

// ----- comparisons -----

#[test]
fn comparisons_across_numeric_kinds() {
    assert_eq!(truth("1 = 1.0"), T);
    assert_eq!(truth("1 <> 1.0"), F);
    assert_eq!(truth("1 < 1.5"), T);
    assert_eq!(truth("2 >= 1.5"), T);
    assert_eq!(truth("n.age = 29.0"), T);
    assert_eq!(truth("n.age > 28.9"), T);
    assert_eq!(truth("n.age <= 29"), T);
    assert_eq!(truth("n.age < $p"), T);
    assert_eq!(truth("'a' < 'b'"), T);
    assert_eq!(truth("false < true"), T);
    assert_eq!(truth("'abc' = 'abc'"), T);
    assert_eq!(truth("'abc' <> 'abd'"), T);
}

#[test]
fn incomparable_kinds_and_null_give_null() {
    // Stage 3f (U1): `=` / `<>` of different kinds give FALSE / TRUE, the orderings `NULL`.
    let kinds = [("1", "'1'"), ("true", "1"), ("'a'", "[1]"), ("n", "1")];
    let nulls = [
        ("null", "1"),
        ("1", "null"),
        ("null", "null"),
        ("n.missing", "n.missing"),
    ];
    for op in ["=", "<>", "<", "<=", ">", ">="] {
        for (a, b) in kinds {
            let expr = format!("{a} {op} {b}");
            let expected = match op {
                "=" => F,
                "<>" => T,
                _ => U,
            };
            assert_eq!(truth(&expr), expected, "{expr}");
        }
        for (a, b) in nulls {
            let expr = format!("{a} {op} {b}");
            assert_eq!(truth(&expr), U, "{expr}");
        }
    }
    // Lists are ordered lexicographically (U2); maps and elements are not ordered.
    assert_eq!(truth("[1] < [2]"), T);
    assert_eq!(truth("n < m"), U);
}

#[test]
fn nan_is_incomparable() {
    // Stage 3f (U3): IEEE 754, as the openCypher TCK: NaN is never equal, never ordered.
    let nan: Value<El> = float(f64::NAN);
    assert_eq!(equals(&nan, &nan), F);
    assert_eq!(order(&nan, &float(1.0)), Order::Unordered);
    assert_eq!(order(&nan, &Value::string("a")), Order::Unknown);
}

#[test]
fn list_map_and_element_equality() {
    assert_eq!(truth("[1, 2] = [1, 2.0]"), T);
    assert_eq!(truth("[1, 2] = [1, 3]"), F);
    assert_eq!(truth("[1, 2] = [1]"), F);
    assert_eq!(truth("[1, null] = [1, null]"), U);
    assert_eq!(truth("[1, null] = [2, null]"), F);
    assert_eq!(truth("{a: 1, b: 2} = {b: 2, a: 1}"), T);
    assert_eq!(truth("{a: 1} = {b: 1}"), F);
    assert_eq!(truth("{a: 1} = {a: null}"), U);
    assert_eq!(truth("n.tags = ['a', 'b']"), T);
    assert_eq!(truth("n = n"), T);
    assert_eq!(truth("n = m"), F);
    assert_eq!(truth("n <> m"), T);
    // A vertex never equals an edge, even when their ids are the same.
    assert_eq!(truth("element_id(n) = element_id(e)"), T);
    assert_eq!(truth("n = e"), F);
}

// ----- IN -----

#[test]
fn in_follows_gql_null_semantics() {
    assert_eq!(truth("1 IN [1, 2]"), T);
    assert_eq!(truth("1.0 IN [1, 2]"), T);
    assert_eq!(truth("3 IN [1, 2]"), F);
    assert_eq!(truth("3 IN [1, null]"), U);
    assert_eq!(truth("1 IN [1, null]"), T);
    assert_eq!(truth("null IN [1, 2]"), U);
    assert_eq!(truth("null IN []"), F);
    assert_eq!(truth("1 IN []"), F);
    assert_eq!(truth("1 IN null"), U);
    assert_eq!(truth("1 IN 1"), U);
    assert_eq!(truth("'1' IN [1]"), F);
    assert_eq!(truth("[1] IN [[1], [2]]"), T);
    assert_eq!(truth("2 IN $list"), T);
    assert_eq!(truth("'a' IN n.tags"), T);
    assert_eq!(truth("n IN [m, n]"), T);
}

// ----- string predicates and concatenation -----

#[test]
fn string_predicates() {
    assert_eq!(truth("n.name STARTS WITH 'ma'"), T);
    assert_eq!(truth("n.name STARTS WITH $s"), T);
    assert_eq!(truth("n.name ENDS WITH 'ko'"), T);
    assert_eq!(truth("n.name CONTAINS 'rk'"), T);
    assert_eq!(truth("n.name CONTAINS 'x'"), F);
    assert_eq!(truth("n.name STARTS WITH ''"), T);
    for expr in [
        "n.missing STARTS WITH 'a'",
        "n.name ENDS WITH null",
        "n.age CONTAINS '2'",
        "'12' STARTS WITH 1",
        "n.tags CONTAINS 'a'",
    ] {
        assert_eq!(truth(expr), U, "{expr}");
    }
}

#[test]
fn concatenation_and_string_plus() {
    assert_eq!(value("'a' || 'b'"), string("ab"));
    assert_eq!(value("n.name || '!'"), string("marko!"));
    assert_eq!(value("'a' + 'b'"), string("ab"));
    assert_eq!(
        value("[1] || [2, 3]"),
        Value::List(vec![int(1), int(2), int(3)])
    );
    assert_eq!(value("[1] + [2]"), Value::List(vec![int(1), int(2)]));
    assert_eq!(value("'a' || null"), Value::Null);
    assert_eq!(value("null + 'a'"), Value::Null);
    assert_eq!(value("n.missing || 'a'"), Value::Null);
    // No implicit number-to-text conversion.
    assert_eq!(status_of("'a' + 1"), "22G03");
    assert_eq!(status_of("1 + 'a'"), "22G03");
    assert_eq!(status_of("'a' || 1"), "22G03");
    // Stage 3f (U6): `[1] + 2` appends; `||` of a list and a number still fails.
    assert_eq!(status_of("[1] || 2"), "22G03");
    match eval("'a' + 1") {
        Err(e) => assert!(
            e.message.contains("never turned into text"),
            "{}",
            e.message
        ),
        Ok(v) => panic!("{v:?}"),
    }
}

// ----- arithmetic -----

#[test]
fn integer_and_float_arithmetic() {
    assert_eq!(value("1 + 2"), int(3));
    assert_eq!(value("5 - 7"), int(-2));
    assert_eq!(value("6 * 7"), int(42));
    assert_eq!(value("7 / 2"), int(3));
    assert_eq!(value("-7 / 2"), int(-3));
    assert_eq!(value("7 % 3"), int(1));
    assert_eq!(value("-7 % 3"), int(-1));
    assert_eq!(value("1 + 0.5"), float(1.5));
    assert_eq!(value("7 / 2.0"), float(3.5));
    assert_eq!(value("7.5 % 2"), float(1.5));
    assert_eq!(value("n.age + 1"), int(30));
    assert_eq!(value("-n.age"), int(-29));
    assert_eq!(value("+n.age"), int(29));
    assert_eq!(value("-(1.5)"), float(-1.5));
    assert_eq!(value("1 + 2 * 3"), int(7));
    assert_eq!(value("-9223372036854775808 % -1"), int(0));
}

#[test]
fn arithmetic_propagates_null() {
    for expr in [
        "1 + null",
        "null - 1",
        "n.missing * 2",
        "null / 0",
        "1 % null",
        "-null",
        "-n.missing",
    ] {
        assert_eq!(value(expr), Value::Null, "{expr}");
    }
}

#[test]
fn overflow_and_division_by_zero_are_data_exceptions() {
    for expr in [
        "9223372036854775807 + 1",
        "-9223372036854775808 - 1",
        "9223372036854775807 * 2",
        "-9223372036854775808 / -1",
        "-(-9223372036854775808)",
        "1.0e308 * 10",
    ] {
        assert_eq!(status_of(expr), "22003", "{expr}");
    }
    for expr in ["1 / 0", "1 % 0", "n.age / (n.age - 29)"] {
        assert_eq!(status_of(expr), "22012", "{expr}");
    }
    // Stage 3f (U3): a float divided by zero follows IEEE 754 (the openCypher TCK).
    for (expr, expected) in [
        ("1.5 / 0", f64::INFINITY),
        ("1 / 0.0", f64::INFINITY),
        ("-1 / 0.0", f64::NEG_INFINITY),
    ] {
        assert_eq!(eval(expr).ok(), Some(float(expected)), "{expr}");
    }
    for expr in ["1.0 % 0.0", "0.0 / 0.0"] {
        match eval(expr) {
            Ok(Value::Scalar(ElementProperty::Number(n))) => assert!(super::float(&n).is_nan()),
            other => panic!("{expr}: {other:?}"),
        }
    }
    for expr in ["'a' - 1", "true * 2", "[1] / 1", "-'a'", "n - 1"] {
        assert_eq!(status_of(expr), "22G03", "{expr}");
    }
}

#[test]
fn error_spans_point_at_the_expression() {
    let query = "MATCH (n) RETURN 1 + (2 / 0)";
    let parsed = parse(query).expect("parses");
    let compiled: Compiled<El> = compile(&parsed.ret.items[0].expr, &Names).expect("compiles");
    let error = compiled.eval(&Row).expect_err("fails");
    assert_eq!(&query[error.span.start..error.span.end], "(2 / 0)");
    assert_eq!(
        &query[compiled.span().start..compiled.span().end],
        "1 + (2 / 0)"
    );
}

// ----- CASE -----

#[test]
fn simple_and_searched_case() {
    assert_eq!(
        value("CASE 2 WHEN 1 THEN 'a' WHEN 2 THEN 'b' ELSE 'c' END"),
        string("b")
    );
    assert_eq!(value("CASE 2 WHEN 2.0 THEN 'num' END"), string("num"));
    assert_eq!(value("CASE 3 WHEN 1 THEN 'a' ELSE 'c' END"), string("c"));
    assert_eq!(value("CASE 3 WHEN 1 THEN 'a' END"), Value::Null);
    // NULL never matches a WHEN of a simple CASE.
    assert_eq!(
        value("CASE null WHEN null THEN 'a' ELSE 'b' END"),
        string("b")
    );
    assert_eq!(value("CASE n.missing WHEN null THEN 'a' END"), Value::Null);
    assert_eq!(
        value("CASE WHEN n.age < 18 THEN 'minor' WHEN n.age >= 18 THEN 'adult' END"),
        string("adult")
    );
    assert_eq!(
        value("CASE WHEN null THEN 1 WHEN false THEN 2 ELSE 3 END"),
        int(3)
    );
    assert_eq!(value("CASE WHEN n.missing > 1 THEN 1 END"), Value::Null);
    // The first TRUE arm wins; later arms are not evaluated.
    assert_eq!(
        value("CASE WHEN true THEN 1 WHEN 1 / 0 = 1 THEN 2 END"),
        int(1)
    );
    assert_eq!(status_of("CASE WHEN 1 THEN 1 END"), "22G03");
}

// ----- labels -----

#[test]
fn is_labeled() {
    assert_eq!(truth("n IS LABELED person"), T);
    assert_eq!(truth("n:employee"), T);
    assert_eq!(truth("n IS LABELED software"), F);
    assert_eq!(truth("n IS NOT LABELED software"), T);
    assert_eq!(truth("n IS LABELED person & employee"), T);
    assert_eq!(truth("n IS LABELED person & software"), F);
    assert_eq!(truth("n IS LABELED software | person"), T);
    assert_eq!(truth("n IS LABELED !software"), T);
    assert_eq!(truth("n IS LABELED %"), T);
    assert_eq!(truth("m IS LABELED %"), F);
    assert_eq!(truth("m IS LABELED !%"), T);
    assert_eq!(truth("e IS LABELED knows"), T);
    assert_eq!(truth("e IS LABELED knows & created"), F);
    assert_eq!(truth("e IS LABELED knows | created"), T);
    assert_eq!(truth("x IS LABELED person"), U);
    assert_eq!(truth("x IS NOT LABELED person"), U);
    assert_eq!(status_of("1 IS LABELED person"), "22G03");
}

// ----- property access -----

#[test]
fn property_access() {
    assert_eq!(value("n.name"), string("marko"));
    assert_eq!(value("n.missing"), Value::Null);
    assert_eq!(value("n.stored_null"), Value::Null);
    assert_eq!(value("e.weight"), float(0.5));
    assert_eq!(value("x.name"), Value::Null);
    assert_eq!(value("x.name.first"), Value::Null);
    assert_eq!(value("n.address.city"), string("Berlin"));
    assert_eq!(value("n.address.zip"), Value::Null);
    assert_eq!(value("{a: {b: 1}}.a.b"), int(1));
    assert_eq!(value("{a: 1}.b"), Value::Null);
    assert_eq!(value("n.tags"), Value::List(vec![string("a"), string("b")]));
    assert_eq!(status_of("n.name.first"), "22G03");
    assert_eq!(status_of("[1].a"), "22G03");
}

// ----- literals, parameters, constants -----

#[test]
fn literals_lists_maps_and_parameters() {
    assert_eq!(value("$p"), int(30));
    assert_eq!(value("$nothing"), Value::Null);
    assert_eq!(value("$list"), Value::List(vec![int(1), int(2)]));
    assert_eq!(
        value("[1, 'a', null, n.age]"),
        Value::List(vec![int(1), string("a"), Value::Null, int(29)])
    );
    assert_eq!(
        value("{name: n.name, k: 1}"),
        Value::Map(vec![("name".into(), string("marko")), ("k".into(), int(1))])
    );
    // A repeated key keeps its first position and its last value.
    assert_eq!(
        value("{a: 1, b: 2, a: 3}"),
        Value::Map(vec![("a".into(), int(3)), ("b".into(), int(2))])
    );
    assert_eq!(
        value("[n, e]"),
        Value::List(vec![Value::Element(El::V(1)), Value::Element(El::E(1))])
    );
    // Lists and maps of constants are folded once, at compile time.
    let folded = compiled("[1, [2, 'x'], {a: $p}]").expect("compiles");
    assert!(folded.is_const());
    assert_eq!(
        folded.as_const(),
        Some(&Value::List(vec![
            int(1),
            Value::List(vec![int(2), string("x")]),
            Value::Map(vec![("a".into(), int(30))]),
        ]))
    );
    assert!(!compiled("[1, n.age]").expect("compiles").is_const());
    assert_eq!(value("true"), boolean(true));
}

#[test]
fn unknown_names_fail_at_compile_time() {
    for (expr, status) in [
        ("nope", "42002"),
        ("$nope", "42002"),
        ("n.age + nope", "42002"),
    ] {
        match compiled(expr) {
            Err(e) => assert_eq!(e.gqlstatus, status, "{expr}"),
            Ok(c) => panic!("{expr:?} compiled: {c:?}"),
        }
    }
}

// ----- functions -----

#[test]
fn element_functions() {
    assert_eq!(value("element_id(n)"), string("1"));
    assert_eq!(value("elementId(e)"), string("1"));
    assert_eq!(
        value("labels(n)"),
        Value::List(vec![string("person"), string("employee")])
    );
    assert_eq!(value("labels(m)"), Value::List(Vec::new()));
    assert_eq!(value("labels(e)"), Value::List(vec![string("knows")]));
    assert_eq!(value("type(e)"), string("knows"));
    assert_eq!(value("type(f)"), string("created"));
    for expr in ["element_id(x)", "labels(x)", "type(x)", "type(n.missing)"] {
        assert_eq!(value(expr), Value::Null, "{expr}");
    }
    assert_eq!(status_of("type(n)"), "22G03");
    match eval("type(n)") {
        Err(e) => assert!(e.message.contains("labels(v)"), "{}", e.message),
        Ok(v) => panic!("{v:?}"),
    }
    assert_eq!(status_of("element_id('1')"), "22G03");
    assert_eq!(status_of("labels(1)"), "22G03");
}

#[test]
fn scalar_functions() {
    assert_eq!(
        value("coalesce(n.missing, null, n.name, 1 / 0)"),
        string("marko")
    );
    assert_eq!(value("coalesce(null, n.missing)"), Value::Null);
    assert_eq!(value("upper(n.name)"), string("MARKO"));
    assert_eq!(value("toUpper('straße')"), string("STRASSE"));
    assert_eq!(value("lower('ÄB')"), string("äb"));
    assert_eq!(value("toLower(null)"), Value::Null);
    assert_eq!(value("char_length('héllo')"), int(5));
    assert_eq!(value("character_length('')"), int(0));
    assert_eq!(value("size('héllo')"), int(5));
    assert_eq!(value("size([1, 2, null])"), int(3));
    assert_eq!(value("cardinality(n.tags)"), int(2));
    assert_eq!(value("cardinality('ab')"), int(2));
    for expr in [
        "upper(n.missing)",
        "char_length(null)",
        "size(null)",
        "cardinality(x)",
    ] {
        assert_eq!(value(expr), Value::Null, "{expr}");
    }
    for expr in [
        "upper(1)",
        "lower(n)",
        "char_length([1])",
        "size(1)",
        "cardinality(true)",
    ] {
        assert_eq!(status_of(expr), "22G03", "{expr}");
    }
}

// ----- conversions -----

#[test]
fn values_round_trip_through_properties() {
    let mut map = PropertyMap::new();
    map.insert(
        "k",
        ElementProperty::Array(vec![1.into(), ElementProperty::Null]),
    );
    let stored = ElementProperty::Object(Box::new(map));
    let value: Value<El> = Value::from_property(stored.clone());
    assert_eq!(
        value,
        Value::Map(vec![("k".into(), Value::List(vec![int(1), Value::Null]))])
    );
    assert_eq!(value.into_property(&mut |_| ElementProperty::Null), stored);
    let element: Value<El> = Value::List(vec![Value::Element(El::V(7))]);
    assert_eq!(
        element.into_property(&mut |el| match el {
            El::V(id) | El::E(id) => ElementProperty::from(id as i64),
        }),
        ElementProperty::Array(vec![7.into()])
    );
}

#[test]
fn deep_expressions_evaluate_without_overflow() {
    // Operator chains as tall as the parser lets them grow (`MAX_HEIGHT`), compiled and
    // evaluated on a small stack: the recursive frames of the evaluator stay small even in
    // unoptimized builds (the parser itself runs on the test thread).
    let terms = crate::gql::parser::MAX_HEIGHT - 12;
    let queries: Vec<(String, Value<El>)> = vec![
        (vec!["1"; terms].join(" + "), int(terms as i64)),
        (vec!["true"; terms].join(" AND "), boolean(true)),
        (
            vec!["n.age"; terms].join(" - "),
            int(29 - 29 * (terms as i64 - 1)),
        ),
        (format!("n{}", ".k".repeat(terms)), Value::Null),
        (format!("x{}", " IS NULL".repeat(terms)), boolean(false)),
        // Nesting through parentheses is bounded by the parser's `MAX_DEPTH`.
        (
            format!("{}1{}", "1 + (".repeat(60), ")".repeat(60)),
            int(61),
        ),
        // Each `-(` is two parser levels.
        (format!("{}1{}", "-(".repeat(60), ")".repeat(60)), int(1)),
    ];
    let parsed: Vec<_> = queries
        .iter()
        .map(|(expr, _)| parse(&format!("MATCH (n) RETURN {expr}")).expect("parses"))
        .collect();
    let results = std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(move || {
            parsed
                .iter()
                .map(|query| {
                    let compiled: Compiled<El> =
                        compile(&query.ret.items[0].expr, &Names).expect("compiles");
                    compiled.eval(&Row).expect("evaluates")
                })
                .collect::<Vec<_>>()
        })
        .expect("spawns")
        .join()
        .expect("no stack overflow");
    for ((expr, expected), result) in queries.iter().zip(results) {
        assert_eq!(&result, expected, "{}", &expr[..20]);
    }
}

// ----- indexing and slicing (stage 3f) -----

fn ints(values: &[i64]) -> Value<El> {
    Value::List(values.iter().map(|v| int(*v)).collect())
}

/// The message of a failing subscript (with its help topic).
fn subscript_message(expr: &str) -> String {
    match eval(expr) {
        Ok(value) => panic!("{expr:?} gave {value:?}"),
        Err(e) => {
            assert_eq!(e.topic, GqlHelpTopic::Subscript, "{expr}");
            e.message
        }
    }
}

#[test]
fn list_indexing_counts_from_zero_and_from_the_end() {
    assert_eq!(value("[1, 2, 3][0]"), int(1));
    assert_eq!(value("[1, 2, 3][2]"), int(3));
    assert_eq!(value("[1, 2, 3][-1]"), int(3));
    assert_eq!(value("[1, 2, 3][-3]"), int(1));
    assert_eq!(value("[1, 2, 3][3]"), Value::Null);
    assert_eq!(value("[1, 2, 3][-4]"), Value::Null);
    assert_eq!(value("[1, 2, 3][-9223372036854775808]"), Value::Null);
    assert_eq!(value("[1, 2, 3][9223372036854775807]"), Value::Null);
    assert_eq!(value("[][0]"), Value::Null);
    assert_eq!(value("[[1], [2, 3]][1][0]"), int(2));
    assert_eq!(value("$list[1]"), int(2));
    assert_eq!(value("[1, 2, 3][$list[0]]"), int(2));
    assert_eq!(value("n.tags[1]"), string("b"));
    // NULL target or index.
    assert_eq!(value("x[0]"), Value::Null);
    assert_eq!(value("[1][null]"), Value::Null);
    assert_eq!(value("[1][$nothing]"), Value::Null);
    assert_eq!(value("null[0]"), Value::Null);
    // Precedence: binds like `.`, tighter than unary minus and `+`.
    assert_eq!(value("[1, 2][0] + 1"), int(2));
    assert_eq!(value("-[1, 2][1]"), int(-2));
    assert_eq!(value("[{a: 5}][0].a"), int(5));
    assert_eq!(value("{a: [7, 8]}.a[1]"), int(8));
    assert_eq!(value("size([[1, 2]][0])"), int(2));
}

#[test]
fn map_and_element_subscripts_read_by_a_string_key() {
    assert_eq!(value("{name: 'x', Name: 'y'}['Name']"), string("y"));
    assert_eq!(value("{name: 'x'}['na' || 'me']"), string("x"));
    assert_eq!(value("{name: 'x'}['nAMe']"), Value::Null);
    assert_eq!(value("{a: 1}[$s]"), Value::Null);
    assert_eq!(value("{a: 1}[null]"), Value::Null);
    assert_eq!(value("n['na' || 'me']"), string("marko"));
    assert_eq!(value("n['missing' || '']"), Value::Null);
    assert_eq!(value("e['weight' || '']"), float(0.5));
    assert_eq!(value("n['address']['city']"), string("Berlin"));
    assert_eq!(value("n[null]"), Value::Null);
    // A string literal key is the property access.
    assert_eq!(value("n['name']"), string("marko"));
}

#[test]
fn list_slices_clamp_their_bounds() {
    assert_eq!(value("[1, 2, 3, 4, 5][1..3]"), ints(&[2, 3]));
    assert_eq!(value("[1, 2, 3][1..]"), ints(&[2, 3]));
    assert_eq!(value("[1, 2, 3][..2]"), ints(&[1, 2]));
    assert_eq!(value("[1, 2, 3][..]"), ints(&[1, 2, 3]));
    assert_eq!(value("[1, 2, 3][0..1]"), ints(&[1]));
    assert_eq!(value("[1, 2, 3][0..0]"), ints(&[]));
    assert_eq!(value("[1, 2, 3][-3..-1]"), ints(&[1, 2]));
    assert_eq!(value("[1, 2, 3][3..1]"), ints(&[]));
    assert_eq!(value("[1, 2, 3][-5..5]"), ints(&[1, 2, 3]));
    assert_eq!(
        value("[1, 2, 3][-9223372036854775808..9223372036854775807]"),
        ints(&[1, 2, 3])
    );
    assert_eq!(value("$list[1..]"), ints(&[2]));
    // A list built per row is cut in place; the result is the same.
    assert_eq!(value("[n.age, 1, 2][1..]"), ints(&[1, 2]));
    assert_eq!(value("[n.age, 1, 2][-1]"), int(2));
    for null in [
        "[1, 2, 3][null..null]",
        "[1, 2, 3][1..null]",
        "[1, 2, 3][null..3]",
        "[1, 2, 3][..null]",
        "[1, 2, 3][null..]",
        "x[1..2]",
        "null[..]",
    ] {
        assert_eq!(value(null), Value::Null, "{null}");
    }
}

#[test]
fn subscripts_of_the_wrong_kind_are_data_exceptions() {
    let any_target = "[ ] reads an item of a list, an entry of a map or a property of a node or \
                      an edge";
    for (expr, message) in [
        (
            "[1, 2][1.0]",
            "a list index is an integer, not a float".to_string(),
        ),
        (
            "[1, 2][true]",
            "a list index is an integer, not a boolean".into(),
        ),
        (
            "[1, 2]['1' || '']",
            "a list index is an integer, not a string".into(),
        ),
        (
            "[1, 2][[1]]",
            "a list index is an integer, not a list".into(),
        ),
        (
            "[1, 2][{x: 3}]",
            "a list index is an integer, not a map".into(),
        ),
        (
            "{a: 1}[0]",
            "a map is read with a string key, not a number".into(),
        ),
        (
            "{a: 1}[12.3]",
            "a map is read with a string key, not a number".into(),
        ),
        (
            "n[1]",
            "a property of a node or an edge is read with a string key, not a number".into(),
        ),
        ("'abc'[0]", format!("cannot index a string: {any_target}")),
        ("123[0]", format!("cannot index a number: {any_target}")),
        (
            "true[null]",
            format!("cannot index a boolean: {any_target}"),
        ),
        (
            "[1, 2][0.5..]",
            "a slice bound is an integer, not a float".into(),
        ),
        (
            "[1, 2][..'a']",
            "a slice bound is an integer, not a string".into(),
        ),
        (
            "'abc'[0..1]",
            "a slice [a..b] reads a list, not a string".into(),
        ),
        (
            "{a: 1}[..1]",
            "a slice [a..b] reads a list, not a map".into(),
        ),
        (
            "n[0..1]",
            "a slice [a..b] reads a list, not an element".into(),
        ),
    ] {
        assert_eq!(status_of(expr), "22G03", "{expr}");
        assert_eq!(subscript_message(expr), message, "{expr}");
    }
    // The error points at the index (a kind of index) or the whole subscript (a kind of target).
    let query = "MATCH (n) RETURN [1][true]";
    let e = compiled("[1][true]")
        .and_then(|c| c.eval(&Row))
        .unwrap_err();
    assert_eq!(&query[e.span.start..e.span.end], "true");
    let query = "MATCH (n) RETURN 'abc'[0]";
    let e = compiled("'abc'[0]").and_then(|c| c.eval(&Row)).unwrap_err();
    assert_eq!(&query[e.span.start..e.span.end], "'abc'[0]");
}

#[test]
fn a_long_subscript_chain_evaluates_as_a_loop() {
    // `[[[...1...]]][0][0]...`: the subscripts are links of one chain.
    let depth = 60;
    let expr = format!(
        "{}1{}{}",
        "[".repeat(depth),
        "]".repeat(depth),
        "[0]".repeat(depth)
    );
    assert_eq!(value(&expr), int(1));
}

#[test]
fn plus_appends_or_prepends_one_item_to_a_list() {
    // Stage 3f (U6), the openCypher TCK `List4` / `Precedence3`.
    assert_eq!(value("[1] + 2"), Value::List(vec![int(1), int(2)]));
    assert_eq!(value("2 + [1]"), Value::List(vec![int(2), int(1)]));
    assert_eq!(
        value("[false, true] + false"),
        Value::List(vec![
            Value::Scalar(ElementProperty::Boolean(false)),
            Value::Scalar(ElementProperty::Boolean(true)),
            Value::Scalar(ElementProperty::Boolean(false)),
        ])
    );
    assert_eq!(value("[1] + 'a'"), Value::List(vec![int(1), string("a")]));
    assert_eq!(value("[] + n"), Value::List(vec![Value::Element(El::V(1))]));
    assert_eq!(
        value("[1] + {a: 2}"),
        Value::List(vec![int(1), value("{a: 2}")])
    );
    // A list item stays one item: `+` of two lists concatenates.
    assert_eq!(
        value("[[1]] + [2]"),
        Value::List(vec![value("[1]"), int(2)])
    );
    // NULL still propagates; `||` and `-` keep refusing a list and a scalar.
    assert_eq!(value("[1] + null"), Value::Null);
    assert_eq!(value("null + [1]"), Value::Null);
    assert_eq!(status_of("1 || [1]"), "22G03");
    assert_eq!(status_of("[1] - 1"), "22G03");
    assert_eq!(status_of("'a' + 1"), "22G03");
}

#[test]
fn power_is_a_float_with_the_rules_of_power() {
    // Stage 3f (U14), the openCypher TCK `Precedence2`.
    assert_eq!(value("2 ^ 10"), float(1024.0));
    assert_eq!(value("4 ^ 3 * 2 ^ 3"), float(512.0));
    assert_eq!(value("4 ^ 3 % 2 ^ 3"), float(0.0));
    assert_eq!(value("4 ^ 3 - 2 ^ 3"), float(56.0));
    assert_eq!(value("-3 ^ 2"), float(9.0));
    assert_eq!(value("-(3 ^ 2)"), float(-9.0));
    assert_eq!(value("2 ^ 3 ^ 2"), float(64.0));
    assert_eq!(value("2 ^ -1"), float(0.5));
    assert_eq!(value("4 ^ 0.5"), float(2.0));
    assert_eq!(value("null ^ 2"), Value::Null);
    assert_eq!(value("2 ^ n.missing"), Value::Null);
    assert_eq!(status_of("0 ^ -1"), "2201F");
    assert_eq!(status_of("(-8) ^ 0.5"), "2201F");
    assert_eq!(status_of("10 ^ 1000"), "22003");
    assert_eq!(status_of("'a' ^ 2"), "22G03");
    assert_eq!(status_of("[2] ^ 2"), "22G03");
}
