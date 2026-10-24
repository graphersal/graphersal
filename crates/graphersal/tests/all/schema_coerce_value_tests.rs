//! `GraphSchema::coerce_value`: a value as a write would store it, without writing.

use graphersal::CompactString;
use graphersal::error::SchemaError;
use graphersal::prelude::*;
use graphersal::schema::{EdgeSchema, PropertySchema};
use graphersal::storage::ElementKind;

fn schema_in(mode: SchemaMode) -> GraphSchema {
    GraphSchema::new()
        .with_mode(mode)
        .with_vertex(
            "event",
            PropertySchema::object()
                .with_property("day", PropertySchema::date())
                .with_property("id", PropertySchema::uuid())
                .with_property("score", PropertySchema::number()),
        )
        .with_vertex(
            "note",
            PropertySchema::object().with_property("day", PropertySchema::string()),
        )
        .with_edge(
            "at",
            EdgeSchema::new(
                PropertySchema::object().with_property("when", PropertySchema::datetime()),
            ),
        )
}

fn labels(names: &[&str]) -> Vec<CompactString> {
    names.iter().map(|n| CompactString::from(*n)).collect()
}

#[test]
fn coerces_like_a_write_in_open_and_closed_mode() {
    for mode in [SchemaMode::Open, SchemaMode::Closed] {
        let schema = schema_in(mode);
        let event = labels(&["event"]);
        let coerce = |key: &str, value: ElementProperty| {
            schema.coerce_value(ElementKind::Vertex, &event, key, value)
        };
        let day = coerce("day", "2024-01-05".into()).unwrap();
        assert!(matches!(day, ElementProperty::Temporal(_)), "{day:?}");
        let id = coerce("id", "910a2dec-8902-4cc1-beeb-8da1658eec67".into()).unwrap();
        assert!(matches!(id, ElementProperty::Uuid(_)));
        let score = coerce("score", ElementProperty::from(2i64)).unwrap();
        assert!(matches!(score, ElementProperty::Number(n) if n.is_f64()));
        // Undeclared keys and values that need no conversion come back unchanged.
        assert_eq!(
            coerce("other", "2024-01-05".into()).unwrap(),
            "2024-01-05".into()
        );
        assert_eq!(
            coerce("day", ElementProperty::Null).unwrap(),
            ElementProperty::Null
        );
        let when = schema
            .coerce_value(
                ElementKind::Edge,
                &labels(&["at"]),
                "when",
                "2024-01-05T10:00Z".into(),
            )
            .unwrap();
        assert!(matches!(when, ElementProperty::Temporal(_)));
    }
}

#[test]
fn the_first_declaring_label_decides_and_mode_none_never_coerces() {
    let schema = schema_in(SchemaMode::Open);
    let text = ElementProperty::from("2024-01-05");
    let note_first = schema
        .coerce_value(
            ElementKind::Vertex,
            &labels(&["note", "event"]),
            "day",
            text.clone(),
        )
        .unwrap();
    assert_eq!(note_first, text);
    let event_first = schema
        .coerce_value(
            ElementKind::Vertex,
            &labels(&["event", "note"]),
            "day",
            text.clone(),
        )
        .unwrap();
    assert!(matches!(event_first, ElementProperty::Temporal(_)));
    let none = schema_in(SchemaMode::None)
        .coerce_value(
            ElementKind::Vertex,
            &labels(&["event"]),
            "day",
            text.clone(),
        )
        .unwrap();
    assert_eq!(none, text);
}

#[test]
fn a_value_the_type_refuses_is_the_write_error() {
    let schema = schema_in(SchemaMode::Open);
    let err = schema
        .coerce_value(
            ElementKind::Vertex,
            &labels(&["event"]),
            "day",
            "05.01.2024".into(),
        )
        .unwrap_err();
    assert!(matches!(err, SchemaError::CoercionFailed { .. }), "{err}");
    assert!(err.to_string().contains("event.day"), "{err}");
}

#[test]
fn equals_what_a_write_stores() {
    let mut graph = TraversalGraph::new();
    let schema = schema_in(SchemaMode::Open);
    graph.traversal_mut().set_schema(schema.clone()).unwrap();
    graph
        .traversal_mut()
        .add_v("event")
        .property("day", "2024-01-05")
        .property("score", 3i64)
        .iterate()
        .unwrap();
    let stored = graph
        .traversal()
        .v(None::<()>)
        .values(vec!["day", "score"])
        .to_list()
        .unwrap();
    let event = labels(&["event"]);
    let coerced = vec![
        schema
            .coerce_value(ElementKind::Vertex, &event, "day", "2024-01-05".into())
            .unwrap(),
        schema
            .coerce_value(
                ElementKind::Vertex,
                &event,
                "score",
                ElementProperty::from(3i64),
            )
            .unwrap(),
    ];
    assert_eq!(stored, coerced);
}
