//! `ElementProperty::Map` / `ValueMap`: a logical map whose keys are values. No query produces one
//! built directly; these tests build maps through the Rust API and hand
//! them to the script as parameters.
#![cfg(feature = "script")]

use crate::script_host::{ScriptMap, eval_value_with_params};
use graphersal::prelude::ValueMap;
use graphersal::prelude::*;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

fn int(n: i64) -> ElementProperty {
    ElementProperty::from(n)
}

fn text(s: &str) -> ElementProperty {
    ElementProperty::from(s)
}

/// `{29: 1, "a": 2}`.
fn mixed() -> ElementProperty {
    ElementProperty::from_entries([(int(29), int(1)), (text("a"), int(2))])
}

fn hash_of(value: &ElementProperty) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn eval(script: &str, map: ElementProperty) -> rhai::Dynamic {
    let graph = Arc::new(GraphSource::tinkerpop_modern());
    eval_value_with_params(graph, script, HashMap::from([("m".to_string(), map)]))
        .unwrap_or_else(|e| panic!("{script}: {e}"))
}

#[test]
fn constructors_canonicalize() {
    assert!(matches!(
        ElementProperty::from_entries([(text("a"), int(1))]),
        ElementProperty::Object(_)
    ));
    assert!(matches!(
        ElementProperty::from_entries([(int(29), int(1))]),
        ElementProperty::Map(_)
    ));
    assert!(matches!(mixed(), ElementProperty::Map(_)));
    let all_strings: ValueMap = [(text("a"), int(1))].into_iter().collect();
    assert!(matches!(
        all_strings.into_property(),
        ElementProperty::Object(_)
    ));
}

#[test]
fn equality_and_hash_ignore_entry_order() {
    let a = ElementProperty::from_entries([(int(1), text("x")), (text("k"), int(2))]);
    let b = ElementProperty::from_entries([(text("k"), int(2)), (int(1), text("x"))]);
    assert_eq!(a, b);
    assert_eq!(hash_of(&a), hash_of(&b));
    assert_ne!(a, ElementProperty::from_entries([(int(1), text("x"))]));
}

#[test]
fn string_keyed_objects_hash_without_regard_to_order() {
    let object = |pairs: [(&str, i64); 2]| {
        ElementProperty::from_entries(pairs.into_iter().map(|(k, v)| (text(k), int(v))))
    };
    let a = object([("a", 1), ("b", 2)]);
    let b = object([("b", 2), ("a", 1)]);
    assert_eq!(a, b);
    assert_eq!(hash_of(&a), hash_of(&b));
}

#[test]
fn keys_follow_java_equals() {
    let keys = |ks: Vec<ElementProperty>| {
        ks.into_iter()
            .map(|k| (k, int(0)))
            .collect::<ValueMap>()
            .len()
    };
    assert_eq!(
        keys(vec![int(1), ElementProperty::from(1.0f64)]),
        2,
        "1 and 1.0 are different keys"
    );
    assert_eq!(
        keys(vec![
            ElementProperty::from(f64::NAN),
            ElementProperty::from(f64::NAN)
        ]),
        1,
        "NaN equals NaN"
    );
    let l12 = ElementProperty::Array(vec![int(1), int(2)]);
    let l21 = ElementProperty::Array(vec![int(2), int(1)]);
    assert_eq!(keys(vec![l12, l21]), 2, "lists are ordered");
    assert_eq!(keys(vec![text("1"), int(1)]), 2, "\"1\" is not 1");
}

#[test]
fn get_str_reads_string_keys_only() {
    let ElementProperty::Map(map) = mixed() else {
        panic!("a map")
    };
    assert_eq!(map.get_str("a"), Some(&int(2)));
    assert_eq!(map.get_str("29"), None);
    assert_eq!(map.get(&int(29)), Some(&int(1)));
}

#[cfg(feature = "serde")]
#[test]
fn lowers_to_a_graphson_map() {
    let json = graphersal::ElementProperty::from_entries([(int(29), int(1)), (text("a"), int(2))]);
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let rendered = lock.traversal().inject(vec![json]).to_json().expect("json");
    let compact: String = rendered.split_whitespace().collect();
    assert!(
        compact.contains(r#"{"@type":"g:Map","@value":[29,1,"a",2]}"#),
        "got {rendered}"
    );
    // A string-keyed map stays a plain JSON object.
    let plain = lock
        .traversal()
        .inject(vec![ElementProperty::from_entries([(text("a"), int(1))])])
        .to_json()
        .expect("json");
    let plain: String = plain.split_whitespace().collect();
    assert!(
        plain.contains(r#"{"a":1}"#) && !plain.contains("@type"),
        "got {plain}"
    );
}

#[test]
fn renders_with_typed_keys_in_a_table_and_the_console_form() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let table = lock
        .traversal()
        .inject(vec![mixed()])
        .to_table()
        .expect("table");
    assert!(table.contains("29") && table.contains('a'), "got {table}");

    let dynamic = eval("m", mixed());
    assert_eq!(
        crate::script_host::debug_value(&dynamic),
        r#"{29: 1, "a": 2}"#
    );
    assert_eq!(
        eval("m.to_string()", mixed()).into_string().unwrap(),
        r#"{29: 1, "a": 2}"#
    );
}

#[cfg(feature = "display")]
#[test]
fn a_string_and_a_number_with_the_same_text_stay_separate_columns() {
    let map = ElementProperty::from_entries([(int(29), int(1)), (text("29"), int(2))]);
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let table = lock
        .traversal()
        .inject(vec![map])
        .to_table()
        .expect("table");
    assert!(
        table.contains("\"29\"") && table.contains(" 29 "),
        "got {table}"
    );
}

#[test]
fn a_map_is_a_script_map_with_an_indexer() {
    let dynamic = eval("m", mixed());
    assert!(dynamic.clone().try_cast::<ScriptMap>().is_some());
    assert_eq!(eval("m[29]", mixed()).as_int().unwrap(), 1);
    assert_eq!(eval(r#"m["a"]"#, mixed()).as_int().unwrap(), 2);
    assert!(eval("m[30]", mixed()).is_unit(), "a missing key is ()");
    assert_eq!(eval("m.len()", mixed()).as_int().unwrap(), 2);
    assert!(!eval("m.is_empty()", mixed()).as_bool().unwrap());
    assert!(!eval("m.isEmpty()", mixed()).as_bool().unwrap());
    assert!(eval("m.contains(29)", mixed()).as_bool().unwrap());
    assert!(eval("m.containsKey(29)", mixed()).as_bool().unwrap());
    assert!(eval("m.contains_key(29)", mixed()).as_bool().unwrap());
    assert!(!eval(r#"m.contains("29")"#, mixed()).as_bool().unwrap());
    assert_eq!(eval("m.keys()", mixed()).into_array().unwrap().len(), 2);
    assert_eq!(
        eval("m.keys()[0]", mixed()).as_int().unwrap(),
        29,
        "typed key, insertion order"
    );
    assert_eq!(eval("m.values()[1]", mixed()).as_int().unwrap(), 2);
    assert_eq!(
        eval("let n = 0; for pair in m { n += pair[1]; } n", mixed())
            .as_int()
            .unwrap(),
        3
    );
    assert!(eval("m == m", mixed()).as_bool().unwrap());
}

/// Finding for the notes: whether `m.name` falls back to the string indexer.
#[test]
fn property_access_falls_back_to_the_string_indexer() {
    assert_eq!(eval("m.a", mixed()).as_int().unwrap(), 2);
}

#[test]
fn to_map_requires_string_keys_and_explains() {
    let graph = Arc::new(GraphSource::tinkerpop_modern());
    let err = eval_value_with_params(
        graph,
        "m.to_map()",
        HashMap::from([("m".to_string(), mixed())]),
    )
    .expect_err("29 is not a string key");
    let text = err.to_string();
    assert!(text.contains("to_map") && text.contains("29"), "got {text}");
    assert!(
        text.contains("m.keys()") && text.contains("to_json()"),
        "help missing: {text}"
    );
}

#[test]
fn a_script_map_converts_back_to_a_map_value() {
    let result = eval("[m]", mixed()).into_array().unwrap();
    assert!(result[0].clone().try_cast::<ScriptMap>().is_some());
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let back = lock.traversal().inject(vec![mixed()]).to_list().unwrap();
    assert_eq!(back, vec![mixed()]);
}

#[test]
fn a_map_cannot_be_stored_as_a_property() {
    let graph = GraphSource::tinkerpop_modern();
    let err = graph
        .write()
        .traversal_mut()
        .v("1")
        .property("m", mixed())
        .next()
        .expect_err("a map with a non-string key is not storable");
    match err.root_cause() {
        TraverserError::Graph(GraphError::InvalidPropertyValue {
            property_name,
            kind,
            ..
        }) => {
            assert_eq!(property_name, "m");
            assert_eq!(*kind, "map with a non-string key");
        }
        other => panic!("expected InvalidPropertyValue, got {other:?}"),
    }
    assert_eq!(
        err.root_cause().to_string(),
        "Property 'm' cannot hold a map with a non-string key: a property map is keyed by strings"
    );
    let help = err.root_cause().help().expect("help");
    assert!(
        help.contains("to_json()") && help.contains(r#"group_count().by("age")"#),
        "got {help}"
    );

    // The same through the script, where the map arrives as a `ScriptMap`.
    let graph = Arc::new(GraphSource::tinkerpop_modern());
    let err = eval_value_with_params(
        graph,
        r#"g.v("1").property("m", m).next()"#,
        HashMap::from([("m".to_string(), mixed())]),
    )
    .expect_err("a ScriptMap with a non-string key is not storable");
    assert!(
        err.to_string()
            .contains("cannot hold a map with a non-string key"),
        "got {err}"
    );
}

#[test]
fn type_of_a_map_is_object() {
    assert_eq!(
        graphersal::schema::type_of(&mixed()).map(|t| t.to_string()),
        Some("object".to_string())
    );
}

#[test]
fn select_keys_and_unfold_return_typed_keys() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let keys = lock
        .traversal()
        .inject(vec![mixed()])
        .select(Column::Keys)
        .to_list()
        .unwrap();
    assert_eq!(keys, vec![ElementProperty::Array(vec![int(29), text("a")])]);

    let entries = lock
        .traversal()
        .inject(vec![mixed()])
        .unfold()
        .select(Column::Keys)
        .to_list()
        .unwrap();
    assert_eq!(entries, vec![int(29), text("a")]);
}

#[test]
fn local_scope_counts_the_entries() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let n = lock
        .traversal()
        .inject(vec![mixed()])
        .count_scoped(Scope::Local)
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(n, int(2));
}

#[test]
fn json_path_reaches_string_keys_only() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let a = lock
        .traversal()
        .inject(vec![mixed()])
        .json_path("$.a")
        .to_list()
        .unwrap();
    assert_eq!(a, vec![int(2)]);
    let none = lock
        .traversal()
        .inject(vec![mixed()])
        .json_path("$.29")
        .to_list()
        .unwrap();
    assert!(none.is_empty());
}

#[test]
fn display_text_table() {
    let rendered = |value: ElementProperty| {
        let graph = GraphSource::tinkerpop_modern();
        let lock = graph.read();
        lock.traversal()
            .inject(vec![ElementProperty::from_entries([(value, int(0))])])
            .to_table()
            .unwrap()
    };
    assert!(rendered(ElementProperty::from(2.0f64)).contains("2.0"));
    assert!(rendered(ElementProperty::Boolean(true)).contains("true"));
    assert!(rendered(ElementProperty::Null).contains("null"));
    assert!(rendered(ElementProperty::Array(vec![int(1), int(2)])).contains("[1,2]"));
}
