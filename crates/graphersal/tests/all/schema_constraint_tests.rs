//! Every supported constraint (`minLength`, `maxLength`, `pattern`, `minimum`, `maximum`,
//! `exclusiveMinimum`, `exclusiveMaximum`, `minItems`, `maxItems`, `enum`, `const`) in each
//! schema mode: enforced with a `ConstraintViolation` naming the rule and the actual value in
//! `Open` and `Closed`, ignored in `None`, and a failing write through a traversal leaves nothing
//! behind. Plus the regular-expression semantics of `pattern`.

use graphersal::prelude::*;
use graphersal::schema::{PropertySchema, SchemaPattern, ValueType, type_of};

use crate::schema_support::schema;

fn doc_schema(mode: &str) -> GraphSchema {
    schema(&format!(
        r#"{{
        "mode": "{mode}",
        "vertices": {{
            "doc": {{ "schema": {{ "type": "object", "properties": {{
                "title": {{ "type": "string", "minLength": 2, "maxLength": 5 }},
                "code":  {{ "type": "string", "pattern": "^ab" }},
                "score": {{ "type": "number", "minimum": 0.5, "maximum": 2.5 }},
                "ratio": {{ "type": "number", "exclusiveMinimum": 0, "exclusiveMaximum": 1 }},
                "tags":  {{ "type": "array", "items": {{ "type": "string" }},
                           "minItems": 2, "maxItems": 3 }},
                "level": {{ "type": "string", "enum": ["a", "b"] }},
                "kind":  {{ "type": "string", "const": "human" }}
            }} }} }}
        }}
    }}"#
    ))
}

fn graph(mode: &str) -> TraversalGraph {
    let mut graph = TraversalGraph::new();
    graph.set_schema(doc_schema(mode)).unwrap();
    graph
}

/// Adds one `doc` vertex with `key = value` through the Rust API.
fn add(graph: &mut TraversalGraph, key: &str, value: ElementProperty) -> TraverserResult<()> {
    graph
        .traversal_mut()
        .add_v("doc")
        .property(key, value)
        .to_list()
        .map(drop)
}

fn violation(graph: &mut TraversalGraph, key: &str, value: ElementProperty) -> String {
    let err = add(graph, key, value.clone())
        .expect_err(&format!("{key} = {value:?} violates a constraint"));
    let text = format_diagnostic(&*err);
    assert!(
        text.contains(&format!("Constraint violation on 'doc.{key}'")),
        "{text}"
    );
    assert!(
        text.contains("Ensure value complies with validation constraint"),
        "{text}"
    );
    text
}

fn strings(items: &[&str]) -> ElementProperty {
    ElementProperty::Array(items.iter().map(|s| (*s).into()).collect())
}

#[test]
fn every_constraint_is_enforced_in_open_and_closed_mode() {
    for mode in ["open", "closed"] {
        let mut g = graph(mode);
        let cases: Vec<(&str, ElementProperty, &str, &str)> = vec![
            ("title", "x".into(), "length >= 2", "length 1"),
            ("title", "toolong".into(), "length <= 5", "length 7"),
            ("code", "xab".into(), "matches pattern '^ab'", "xab"),
            ("score", 0.25.into(), "value >= 0.5", "0.25"),
            ("score", 3.into(), "value <= 2.5", "3"),
            ("ratio", 0.0.into(), "value > 0", "0"),
            ("ratio", 1.0.into(), "value < 1", "1"),
            ("tags", strings(&["a"]), "items >= 2", "items 1"),
            (
                "tags",
                strings(&["a", "b", "c", "d"]),
                "items <= 3",
                "items 4",
            ),
            ("level", "c".into(), r#"one of ["a", "b"]"#, r#""c""#),
            ("kind", "robot".into(), r#"equals "human""#, r#""robot""#),
        ];
        for (key, value, rule, actual) in cases {
            let text = violation(&mut g, key, value);
            assert!(text.contains(rule), "{mode} {key}: {text}");
            assert!(
                text.contains(&format!("(actual: {actual})")),
                "{mode} {key}: {text}"
            );
        }
        assert_eq!(
            g.vertex_count(),
            0,
            "{mode}: no failing write left a vertex"
        );

        // values on the (inclusive) bounds pass
        for (key, value) in [
            ("title", ElementProperty::from("ab")),
            ("title", "abcde".into()),
            ("code", "abz".into()),
            ("score", 0.5.into()),
            ("score", 2.5.into()),
            ("ratio", 0.5.into()),
            ("tags", strings(&["a", "b"])),
            ("tags", strings(&["a", "b", "c"])),
            ("level", "b".into()),
            ("kind", "human".into()),
        ] {
            add(&mut g, key, value.clone())
                .unwrap_or_else(|e| panic!("{mode} {key} = {value:?}: {e}"));
        }
        assert_eq!(g.vertex_count(), 10);
    }
}

/// A constraint applies only to the kind it belongs to: `minimum` ignores strings, `minLength`
/// ignores numbers (JSON Schema semantics), so a type array keeps its members' bounds apart.
#[test]
fn a_constraint_applies_only_to_its_own_kind() {
    let mut g = TraversalGraph::new();
    g.set_schema(schema(
        r#"{"mode": "open", "vertices": {"v": {"schema": {"type": "object", "properties": {
            "x": {"type": ["integer", "string"], "minimum": 10, "minLength": 3}}}}}}"#,
    ))
    .unwrap();
    let mut add = |value: ElementProperty| {
        g.traversal_mut()
            .add_v("v")
            .property("x", value)
            .to_list()
            .is_ok()
    };
    assert!(add(12.into()));
    assert!(!add(5.into()));
    assert!(add("abc".into()));
    assert!(!add("ab".into()));
}

/// `enum` and `const` compare JSON values: `1` equals `1.0`, and a uuid compares as its
/// canonical string.
#[test]
fn enum_and_const_compare_json_values() {
    let mut g = TraversalGraph::new();
    g.set_schema(schema(
        r#"{"mode": "open", "vertices": {"v": {"schema": {"type": "object", "properties": {
            "n": {"type": "number", "enum": [1, 2.5]},
            "id": {"type": "string", "format": "uuid",
                   "const": "550e8400-e29b-41d4-a716-446655440000"}}}}}}"#,
    ))
    .unwrap();
    let mut add = |key: &str, value: ElementProperty| {
        g.traversal_mut()
            .add_v("v")
            .property(key, value)
            .to_list()
            .is_ok()
    };
    // The int64 1 is coerced to float64 1.0 and equals the enum's 1.
    assert!(add("n", 1.into()));
    assert!(add("n", 2.5.into()));
    assert!(!add("n", 2.into()));
    assert!(add("id", "550e8400-e29b-41d4-a716-446655440000".into()));
    assert!(!add("id", "650e8400-e29b-41d4-a716-446655440000".into()));
}

#[test]
fn mode_none_describes_but_does_not_enforce() {
    let mut g = graph("none");
    add(&mut g, "title", "x".into()).unwrap();
    add(&mut g, "score", 99.into()).unwrap();
    add(&mut g, "tags", strings(&["a"])).unwrap();
    add(&mut g, "level", "zzz".into()).unwrap();
    assert_eq!(g.vertex_count(), 4);
}

#[test]
fn an_existing_violation_blocks_switching_to_an_enforcing_mode() {
    let mut g = graph("none");
    add(&mut g, "title", "x".into()).unwrap();
    let err = g
        .set_schema(doc_schema("open"))
        .expect_err("the stored title is too short");
    let text = err.to_string();
    assert!(text.contains("title"), "{text}");
    assert_eq!(
        g.get_schema().unwrap().unwrap().mode,
        SchemaMode::None,
        "the old schema stays"
    );
}

/// An empty list is a valid `array` of strings (it has no element of another type), so
/// `minItems: 0` is reachable; `minItems: 1` still rejects it.
#[test]
fn an_empty_list_conforms_to_a_declared_array() {
    let json = r#"{ "mode": "open", "vertices": { "v": { "schema": { "type": "object",
        "properties": { "tags": { "type": "array", "items": { "type": "string" },
        "minItems": 0 } } } } } }"#;
    let mut g = TraversalGraph::new();
    g.set_schema(schema(json)).unwrap();
    g.traversal_mut()
        .add_v("v")
        .property("tags", strings(&[]))
        .to_list()
        .unwrap();
    // nested: an empty array inside an object conforms too
    let json = r#"{ "mode": "closed", "vertices": { "v": { "schema": { "type": "object",
        "properties": { "doc": { "type": "object", "properties": {
            "ids": { "type": "array", "items": { "type": "integer" } } } } } } } } }"#;
    let mut g = TraversalGraph::new();
    g.set_schema(schema(json)).unwrap();
    let mut doc = PropertyMap::default();
    doc.insert("ids", strings(&[]));
    g.traversal_mut()
        .add_v("v")
        .property("doc", ElementProperty::Object(Box::new(doc)))
        .to_list()
        .unwrap();
    // minItems: 1 still rejects it, as a constraint violation (not a type mismatch)
    let json = r#"{ "mode": "open", "vertices": { "v": { "schema": { "type": "object",
        "properties": { "tags": { "type": "array", "items": { "type": "string" },
        "minItems": 1 } } } } } }"#;
    let mut g = TraversalGraph::new();
    g.set_schema(schema(json)).unwrap();
    let err = g
        .traversal_mut()
        .add_v("v")
        .property("tags", strings(&[]))
        .to_list()
        .expect_err("minItems: 1");
    let cause = err.root_cause().to_string();
    assert!(cause.contains("items >= 1"), "{cause}");
    // the type of an empty array invents no null item
    assert_eq!(
        type_of(&strings(&[])),
        Some(ValueType::Array(Box::new(ValueType::Union(vec![]))))
    );
}

/// The same through the Rhai DSL: an empty list literal satisfies an array of strings.
#[cfg(feature = "script")]
#[test]
fn an_empty_list_conforms_to_a_declared_array_in_rhai() {
    let json = r#"{ "mode": "open", "vertices": { "v": { "schema": { "type": "object",
        "properties": { "tags": { "type": "array", "items": { "type": "string" },
        "minItems": 0 } } } } } }"#;
    for script in [
        r#"g.addV("v").property("tags", []).count().next()"#,
        r#"g.add_v("v").property("tags", []).count().next()"#,
    ] {
        let mut graph = TraversalGraph::new();
        graph.set_schema(schema(json)).unwrap();
        let graph = std::sync::Arc::new(Graph::new(graph));
        let result = crate::script_host::eval_value(graph, script);
        assert!(result.is_ok(), "{script}: {:?}", result.err());
    }
}

/// Validates one string against `pattern` on a fresh `Open` graph.
fn accepts(pattern: &str, value: &str) -> bool {
    let json = format!(
        r#"{{ "mode": "open", "vertices": {{ "v": {{ "schema": {{ "type": "object",
            "properties": {{ "p": {{ "type": "string", "pattern": {pattern:?} }} }} }} }} }} }}"#
    );
    let mut g = TraversalGraph::new();
    g.set_schema(schema(&json)).unwrap();
    g.traversal_mut()
        .add_v("v")
        .property("p", value)
        .to_list()
        .is_ok()
}

#[test]
fn the_pattern_is_a_regular_expression_with_json_schema_semantics() {
    // anchors and literals
    assert!(accepts("^ab", "abc") && !accepts("^ab", "cab"));
    assert!(accepts("bc$", "abc") && !accepts("bc$", "bca"));
    assert!(accepts("^abc$", "abc") && !accepts("^abc$", "abcd"));
    assert!(accepts("b", "abc") && !accepts("b", "xyz"));
    assert!(
        accepts("", "anything"),
        "an empty pattern matches everything"
    );
    assert!(
        !accepts("^$", "anything") && accepts("^$", ""),
        "`^$` matches only the empty string"
    );
    // character classes
    assert!(accepts("^[a-z]+$", "abc") && !accepts("^[a-z]+$", "aBc"));
    assert!(!accepts("^[a-z]+$", ""), "`+` needs one character");
    assert!(accepts("^[a-z]*$", ""), "`*` allows none");
    assert!(accepts(r"^[\d-]+$", "12-34") && !accepts(r"^[\d-]+$", "12a"));
    assert!(accepts(r"^[\w]+$", "a_1") && !accepts(r"^[\w]+$", "a b"));
    assert!(accepts(r"^[\s]+$", " \t") && !accepts(r"^[\s]+$", "x"));
    assert!(accepts(r"^[a\.]+$", "a.a") && !accepts(r"^[a\.]+$", "ab"));
    // digit shorthands
    assert!(accepts(r"^\d+$", "123") && !accepts(r"^\d+$", "") && !accepts(r"^\d+$", "1a"));
    assert!(accepts(r"^\d*$", ""));
    // dates
    assert!(accepts(r"^\d{4}-\d{2}-\d{2}$", "2026-10-06"));
    assert!(!accepts(r"^\d{4}-\d{2}-\d{2}$", "2026/10/06"));
    assert!(!accepts(r"^\d{4}-\d{2}-\d{2}$", "26-10-06"));
    // e-mail
    let email = r"^[\w.+-]+@[\w-]+\.[\w.]+$";
    assert!(accepts(email, "a.b+c@example.org"));
    assert!(!accepts(email, "a@b"));
    assert!(!accepts(email, "@example.org"));
    assert!(!accepts(email, "a b@example.org"));
}

/// `[A-Z][a-z]+` is two classes.
#[test]
fn a_two_class_pattern_is_matched_as_a_regex() {
    assert!(accepts("^[A-Z][a-z]+$", "Hello"));
    assert!(!accepts("^[A-Z][a-z]+$", "hello"));
    assert!(!accepts("^[A-Z][a-z]+$", "H"));
}

/// A `.` is any character, as in a regex; `\.` is a literal dot.
#[test]
fn a_dot_matches_any_character() {
    assert!(accepts("^a.c$", "abc"));
    assert!(accepts(r"^a\.c$", "a.c") && !accepts(r"^a\.c$", "abc"));
}

/// Unanchored like JSON Schema.
#[test]
fn groups_alternation_quantifiers_and_unanchored_matching() {
    assert!(accepts("@", "a@b") && !accepts("@", "ab"));
    assert!(accepts("^(cat|dog)s?$", "dogs") && !accepts("^(cat|dog)s?$", "cow"));
    assert!(accepts("^x{2,3}$", "xxx") && !accepts("^x{2,3}$", "xxxx"));
    assert!(accepts("[0-9]", "abc1def"), "unanchored: a digit anywhere");
    assert!(!accepts("^[0-9]+$", "12a"));
}

/// An invalid expression rejects the schema when it is loaded, before any value is validated:
/// from the schema format (an `InvalidSchema` naming the location and the pattern problem) and
/// from the Rust API (`SchemaPattern::new`, a `SchemaError::InvalidPattern` with a help text).
#[test]
fn an_invalid_pattern_is_a_schema_error() {
    for pattern in ["[a-", "(unclosed", r"(?=look)ahead", r"(a)\1"] {
        let json = format!(
            r#"{{ "mode": "open", "vertices": {{ "v": {{ "schema": {{ "type": "object",
                "properties": {{ "p": {{ "type": "string", "pattern": {pattern:?} }} }} }} }} }} }}"#
        );
        let err = GraphSchema::from_json(&json).expect_err(pattern);
        let text = err.to_string();
        assert!(
            text.contains("/vertices/v/schema/properties/p/pattern"),
            "{pattern}: {text}"
        );
        assert!(text.contains("Invalid schema pattern"), "{pattern}: {text}");
        let help = err.help().expect("help");
        assert!(help.contains("regular expression"), "{pattern}: {help}");

        let err = SchemaPattern::new(pattern).expect_err(pattern);
        assert!(
            matches!(&err, SchemaError::InvalidPattern { pattern: p, .. } if p == pattern),
            "{err:?}"
        );
        let help = err.help().expect("InvalidPattern has a help text");
        assert!(help.contains("anchor it with ^...$"), "{help}");
        assert!(help.contains("Look-around"), "{help}");
    }
}

/// A pattern built in Rust round-trips through the schema format unchanged and keeps
/// validating.
#[test]
fn a_rust_built_pattern_round_trips_and_validates() {
    let pattern = SchemaPattern::new("^[a-z]+$").unwrap();
    assert_eq!(pattern.as_str(), "^[a-z]+$");
    assert!(pattern.is_match("abc") && !pattern.is_match("Abc"));
    let node = PropertySchema::string().with_constraint(|c| c.pattern = Some(pattern.clone()));
    let schema = GraphSchema::new()
        .with_mode(SchemaMode::Open)
        .with_vertex("v", PropertySchema::object().with_property("p", node));
    let json = schema.to_json();
    assert!(json.contains(r#""pattern":"^[a-z]+$""#), "{json}");
    let back = GraphSchema::from_json(&json).unwrap();
    assert_eq!(back, schema);
}
