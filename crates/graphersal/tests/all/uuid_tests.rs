//! Integration tests for `Uuid` as a first-class runtime value in the Rust API (type system
//! step 4).
//!
//! Every test here has a Rhai twin in `script_tests.rs` (module `uuid_values`), which exercises
//! the same behavior through `UUID.fromString(..)` and its other spellings.

use graphersal::prelude::*;
#[cfg(feature = "serde")]
use graphersal::schema::PropertySchema;
use graphersal::schema::{ValueType, type_of};

const UID_1: &str = "550e8400-e29b-41d4-a716-446655440000";
const UID_2: &str = "00000000-0000-0000-0000-00000000000a";
const UID_4: &str = "ffffffff-0000-0000-0000-000000000000";

fn uuid(s: &str) -> ElementProperty {
    ElementProperty::uuid_from_string(s).expect("canonical UUID literal")
}

fn bits(s: &str) -> u128 {
    uuid(s).as_uuid().expect("a Uuid value")
}

/// The modern graph with a `uid` UUID on vertices 1, 2, 4 and 6. Vertices 2 and 6 share one
/// UUID, so `dedup()` and `group_count()` have a duplicate to fold.
fn modern_with_uids() -> Graph {
    let graph = GraphSource::tinkerpop_modern();
    for (id, uid) in [("1", UID_1), ("2", UID_2), ("4", UID_4), ("6", UID_2)] {
        graph
            .write()
            .traversal_mut()
            .v(id)
            .property("uid", uuid(uid))
            .next()
            .expect("write uid");
    }
    graph
}

fn count(value: Option<ElementProperty>) -> i64 {
    value
        .and_then(|value| {
            value
                .as_number()
                .and_then(|n| n.as_number())
                .and_then(|n| n.as_i64())
        })
        .expect("a count")
}

#[test]
fn uuid_from_string_builds_a_uuid_value() {
    let value = uuid(UID_1);
    assert_eq!(
        value,
        ElementProperty::Uuid(0x550e8400_e29b_41d4_a716_446655440000)
    );
    assert_eq!(type_of(&value), Some(ValueType::Uuid));
    // Shares the parser with `cast(GType::UUID)`.
    assert_eq!(
        ElementProperty::from(UID_1).cast(&ValueType::Uuid).unwrap(),
        value
    );
}

#[test]
fn a_uuid_literal_finds_the_vertex() {
    let graph = modern_with_uids();
    let locked = graph.read();
    let names = locked
        .traversal()
        .v(None)
        .has("uid", uuid(UID_1))
        .values("name")
        .to_list()
        .unwrap();
    assert_eq!(names, vec![ElementProperty::from("marko")]);

    let by_p = locked
        .traversal()
        .v(None)
        .has_p("uid", P::Eq(PValue::Uuid(bits(UID_1))))
        .count()
        .next()
        .unwrap();
    assert_eq!(count(by_p), 1);

    let by_is = locked
        .traversal()
        .v(None)
        .values("uid")
        .is(P::Eq(PValue::Uuid(bits(UID_2))))
        .count()
        .next()
        .unwrap();
    assert_eq!(count(by_is), 2);

    let by_within = locked
        .traversal()
        .v(None)
        .has_p(
            "uid",
            P::WithIn(vec![PValue::Uuid(bits(UID_1)), PValue::Uuid(bits(UID_4))]),
        )
        .count()
        .next()
        .unwrap();
    assert_eq!(count(by_within), 2);
}

#[test]
fn a_plain_string_never_matches_a_stored_uuid() {
    let graph = modern_with_uids();
    let locked = graph.read();
    let by_has = locked
        .traversal()
        .v(None)
        .has("uid", UID_1)
        .count()
        .next()
        .unwrap();
    assert_eq!(count(by_has), 0);

    let by_p = locked
        .traversal()
        .v(None)
        .has_p("uid", P::Eq(PValue::from(UID_1)))
        .count()
        .next()
        .unwrap();
    assert_eq!(count(by_p), 0);
}

#[test]
fn a_value_read_back_matches_again() {
    let graph = modern_with_uids();
    let locked = graph.read();
    let read = locked
        .traversal()
        .v("1")
        .values("uid")
        .next()
        .unwrap()
        .expect("a uid");
    assert_eq!(read, uuid(UID_1));
    let found = locked
        .traversal()
        .v(None)
        .has("uid", read)
        .count()
        .next()
        .unwrap();
    assert_eq!(count(found), 1);
}

#[test]
fn a_non_canonical_literal_is_a_cast_error_with_the_canonical_hint() {
    for bad in ["550E8400-E29B-41D4-A716-446655440000", "abc"] {
        let err = ElementProperty::uuid_from_string(bad).expect_err("must not parse");
        assert!(matches!(
            *err,
            TraverserError::Value(ValueError::CastException { .. })
        ));
        let report = format_diagnostic(&err);
        assert!(
            report.contains("8-4-4-4-12 lowercase hex")
                && report.contains(r#"UUID.from_string("550e8400-e29b-41d4-a716-446655440000")"#),
            "missing canonical-format hint in: {report}"
        );
    }
}

#[test]
fn nested_values_stay_uuids() {
    let graph = modern_with_uids();
    let locked = graph.read();
    let map = locked
        .traversal()
        .v("1")
        .value_map(vec!["uid"])
        .next()
        .unwrap()
        .expect("a value map");
    let ElementProperty::Object(map) = map else {
        panic!("value_map() yields an object, got {map:?}");
    };
    // valueMap() wraps each value in a one-element list, as in TinkerPop.
    assert_eq!(
        map.get("uid"),
        Some(&ElementProperty::Array(vec![uuid(UID_1)]))
    );

    let map = locked
        .traversal()
        .v("1")
        .element_map(vec!["uid"])
        .next()
        .unwrap()
        .expect("an element map");
    let ElementProperty::Object(map) = map else {
        panic!("element_map() yields an object, got {map:?}");
    };
    assert_eq!(map.get("uid"), Some(&uuid(UID_1)));
}

#[test]
fn order_sorts_uuids_like_their_canonical_strings() {
    let graph = modern_with_uids();
    let locked = graph.read();
    let ordered = locked
        .traversal()
        .v(None)
        .values("uid")
        .order()
        .to_list()
        .unwrap();

    let mut expected = vec![UID_1, UID_2, UID_4, UID_2];
    expected.sort_unstable();
    let expected: Vec<ElementProperty> = expected.into_iter().map(uuid).collect();
    assert_eq!(ordered, expected);
}

#[test]
fn dedup_and_group_count_key_by_uuid() {
    let graph = modern_with_uids();
    let locked = graph.read();
    let distinct = locked
        .traversal()
        .v(None)
        .values("uid")
        .dedup()
        .count()
        .next()
        .unwrap();
    assert_eq!(count(distinct), 3);

    let groups = locked
        .traversal()
        .v(None)
        .values("uid")
        .group_count()
        .next()
        .unwrap()
        .expect("a group count");
    let ElementProperty::Map(groups) = groups else {
        panic!("group_count() yields a map with typed keys, got {groups:?}");
    };
    // Map keys keep their type: a UUID key is a UUID, not its canonical text.
    let uid = |text: &str| ElementProperty::uuid_from_string(text).expect("canonical UUID literal");
    assert_eq!(groups.len(), 3);
    assert_eq!(groups.get(&uid(UID_2)), Some(&ElementProperty::from(2i64)));
    assert_eq!(groups.get(&uid(UID_1)), Some(&ElementProperty::from(1i64)));
    assert_eq!(groups.get_str(UID_2), None, "no string key");
}

#[test]
fn string_steps_on_a_uuid_raise_a_cast_exception() {
    let graph = modern_with_uids();
    let locked = graph.read();
    let results = [
        locked.traversal().v("1").values("uid").to_lower().to_list(),
        locked.traversal().v("1").values("uid").to_upper().to_list(),
        locked
            .traversal()
            .v("1")
            .values("uid")
            .substring(0, 3)
            .to_list(),
        locked.traversal().v("1").values("uid").length().to_list(),
        locked.traversal().v("1").values("uid").trim().to_list(),
        locked.traversal().v("1").values("uid").l_trim().to_list(),
        locked.traversal().v("1").values("uid").r_trim().to_list(),
        locked
            .traversal()
            .v("1")
            .values("uid")
            .replace("a", "b")
            .to_list(),
    ];
    for result in results {
        let err = result.expect_err("a string step must not accept a uuid");
        match err.root_cause() {
            TraverserError::Value(ValueError::CastException { actual, .. }) => {
                assert_eq!(actual, "uuid");
            }
            other => panic!("expected a CastException, got {other:?}"),
        }
    }
}

#[test]
fn text_predicates_on_a_uuid_match_nothing_without_error() {
    let graph = modern_with_uids();
    let locked = graph.read();
    let found = locked
        .traversal()
        .v(None)
        .has_p("uid", P::Containing(PValue::from("550e")))
        .count()
        .next()
        .unwrap();
    assert_eq!(count(found), 0);
}

#[test]
fn as_string_yields_canonical_strings() {
    let graph = modern_with_uids();
    let locked = graph.read();
    let strings = locked
        .traversal()
        .v("1")
        .values("uid")
        .as_string()
        .to_list()
        .unwrap();
    assert_eq!(strings, vec![ElementProperty::from(UID_1)]);
}

#[cfg(feature = "serde")]
mod property_json {
    use super::*;

    fn person_schema(mode: SchemaMode) -> GraphSchema {
        GraphSchema::new().with_mode(mode).with_vertex(
            "person",
            PropertySchema::object().with_property("uid", PropertySchema::uuid().nullable()),
        )
    }

    fn stored_uid(graph: &Graph) -> ElementProperty {
        graph
            .read()
            .traversal()
            .v("1")
            .values("uid")
            .next()
            .unwrap()
            .expect("a uid")
    }

    /// JSON has no UUID type, so a canonical string written through `property_json` stays a
    /// string unless a schema declares the field `uuid`.
    #[test]
    fn property_json_stores_a_string_without_a_schema_and_a_uuid_when_declared() {
        let graph = GraphSource::tinkerpop_modern();
        graph
            .write()
            .traversal_mut()
            .v("1")
            .property_json(serde_json::json!({ "uid": UID_1 }))
            .next()
            .unwrap();
        assert_eq!(stored_uid(&graph), ElementProperty::from(UID_1));

        let graph = GraphSource::tinkerpop_modern();
        graph
            .write()
            .set_schema(person_schema(SchemaMode::Open))
            .unwrap();
        graph
            .write()
            .traversal_mut()
            .v("1")
            .property_json(serde_json::json!({ "uid": UID_1 }))
            .next()
            .unwrap();
        assert_eq!(stored_uid(&graph), uuid(UID_1));
    }
}
