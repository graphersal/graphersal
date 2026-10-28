//! The schema renderers: every one shows nested objects (never a bare `object` row without its
//! keys) and the constraints; Mermaid draws nested objects as linked entities; PlantUML adds a
//! condensed `@startjson` view; the JSON Schema export is one standalone schema per label.
use graphersal::prelude::*;
use graphersal::schema::VisualizeFormat;

use crate::schema_support::schema;

fn modern() -> GraphSchema {
    TraversalGraph::tinkerpop_modern()
        .traversal()
        .infer_schema()
        .unwrap()
}

/// `person(name: string 1..80, age?: int64|null 0..150, address?: {street, zip ^\d{5}$,
/// geo?: {lat: number}}, tags: array<string> <= 10, roles: array<{name}>)`,
/// `knows(weight: number): person -> person`.
fn nested() -> GraphSchema {
    schema(
        r#"{"mode": "closed", "meta": {"version": "2"}, "vertices": {"person": {"schema": {
            "type": "object", "description": "A person",
            "properties": {
              "name": {"type": "string", "minLength": 1, "maxLength": 80},
              "age": {"type": ["integer", "null"], "minimum": 0, "maximum": 150, "default": null},
              "address": {"type": "object", "properties": {
                  "street": {"type": "string"},
                  "zip": {"type": "string", "pattern": "^\\d{5}$"},
                  "geo": {"type": "object", "properties": {"lat": {"type": "number"}}}},
                "required": ["street", "zip"], "additionalProperties": false},
              "tags": {"type": "array", "items": {"type": "string"}, "maxItems": 10},
              "roles": {"type": "array", "items": {"type": "object",
                  "properties": {"name": {"type": "string"}}, "required": ["name"]}}},
            "required": ["name", "tags"]}}},
          "edges": {"knows": {"connections": [{"from": "person", "to": "person"}],
            "schema": {"type": "object", "properties": {"weight": {"type": "number"}}}}}}"#,
    )
}

#[test]
fn test_schema_to_mermaid() {
    let mermaid = modern().to_mermaid();
    assert!(mermaid.starts_with("erDiagram"));
    assert!(mermaid.contains("person {"));
    assert!(mermaid.contains("software {"));
    assert!(mermaid.contains("person ||--o{ person : knows"));
    assert!(mermaid.contains("person ||--o{ software : created"));
    assert!(mermaid.contains("string name"));
    assert!(mermaid.contains("int64 age"));
}

/// Nested objects are entities of their own linked to their parent, not a bare `object`.
#[test]
fn test_mermaid_nested_objects_are_linked_entities() {
    let mermaid = nested().to_mermaid();
    assert!(mermaid.contains("person_address {"), "{mermaid}");
    assert!(
        mermaid.contains("person ||--|| person_address : address"),
        "{mermaid}"
    );
    assert!(mermaid.contains("person_address_geo {"), "{mermaid}");
    assert!(
        mermaid.contains("person_address ||--|| person_address_geo : geo"),
        "{mermaid}"
    );
    assert!(
        mermaid.contains("person ||--o{ person_roles : roles"),
        "{mermaid}"
    );
    assert!(mermaid.contains("array~string~ tags"), "{mermaid}");
    // constraints and requiredness as attribute comments
    assert!(
        mermaid.contains(r#"string name "required, length 1..80""#),
        "{mermaid}"
    );
    assert!(
        mermaid.contains(r#"int64 age "nullable, 0..150""#),
        "{mermaid}"
    );
}

#[test]
fn test_mermaid_types_have_no_angle_brackets() {
    let s = schema(
        r#"{"vertices": {"sample": {"schema": {"type": "object", "properties": {
            "scores": {"type": "array", "items": {"type": "integer"}},
            "matrix": {"type": "array", "items": {"type": "array", "items": {"type": "number"}}},
            "either": {"anyOf": [{"type": "string"}, {"type": "integer"}]}}}}}}"#,
    );
    let mermaid = s.to_mermaid();
    assert!(mermaid.contains("array~int64~ scores"), "{mermaid}");
    assert!(
        mermaid.contains("array~array~float64~~ matrix"),
        "{mermaid}"
    );
    assert!(mermaid.contains("union either"), "{mermaid}");
}

/// Labels and property names are user text: the Mermaid output must parse whatever they hold
/// (checked against the Mermaid 11 parser when this was written; the rules are kept here):
/// identifiers never start with a digit and are unique, the real text stays in an alias, a
/// comment or a quoted relationship label, an alias holds no `%` or `\\`, no quoted text holds
/// `"` or a line break, and an attribute type is letters, digits, `_` and `~` only.
#[test]
fn test_mermaid_survives_hostile_names() {
    let s = schema(
        r#"{"vertices": {
            "Док-умент": {"schema": {"type": "object", "required": ["a b"], "properties": {
                "a b": {"type": "string", "pattern": "^\\d+\"x#y;%%z$"},
                "1x": {"type": ["integer", "null"], "minimum": 0},
                "e": {"enum": ["x\ny", "q;r"]},
                "emb": {"type": "array", "format": "vector", "element": "i8", "dimension": 3},
                "addr": {"type": "object", "properties": {"zip-code": {"type": "string"}}}}}},
            "1st": {"schema": {"type": "object"}},
            "a-b": {"schema": {"type": "object"}},
            "a_b": {"schema": {"type": "object"}},
            "%\\%": {"schema": {"type": "object"}}},
          "edges": {
            "has part {x}": {"connections": [{"from": "Док-умент", "to": "1st"}]},
            "x:y \"z\"": {"connections": [{"from": "*", "to": "a-b"}]}}}"#,
    );
    let mermaid = s.to_mermaid();
    // unique identifiers, the text in an alias
    assert!(mermaid.contains("  a_b[\"a-b\"] {"), "{mermaid}");
    assert!(mermaid.contains("  a_b_2[\"a_b\"] {"), "{mermaid}");
    assert!(mermaid.contains("  _1st[\"1st\"] {"), "{mermaid}");
    assert!(mermaid.contains("[\"％＼％\"]"), "{mermaid}");
    assert!(
        mermaid.contains("  any_label[\"* (any label)\"] {"),
        "{mermaid}"
    );
    // attributes: a safe name with the real one in the comment; a vector's dimension there too
    assert!(
        mermaid.contains(r#"int64 _1x "1x: nullable, >= 0""#),
        "{mermaid}"
    );
    assert!(
        mermaid.contains(r#"string a_b "a b: required, pattern"#),
        "{mermaid}"
    );
    assert!(
        mermaid.contains(r#"vector~i8~ emb "dimension 3""#),
        "{mermaid}"
    );
    // relationship labels: quoted unless a plain word
    assert!(
        mermaid.contains(r#"||--o{ _1st : "has part {x}""#),
        "{mermaid}"
    );
    assert!(
        mermaid.contains(r#"any_label ||--o{ a_b : "x:y 'z'""#),
        "{mermaid}"
    );
    assert!(mermaid.contains(" : addr\n"), "{mermaid}");
    for line in mermaid.lines().skip(1) {
        // a quoted part never spans a line: the quotes of every line pair up
        assert_eq!(line.matches('"').count() % 2, 0, "{line}");
        let trimmed = line.trim_start();
        assert!(
            !trimmed.starts_with(|c: char| c.is_ascii_digit()),
            "identifier starts with a digit: {line}"
        );
    }
}

#[cfg(feature = "display")]
#[test]
fn test_schema_to_table_shows_nested_paths_and_constraints() {
    let table = nested().to_table();
    assert!(table.starts_with("Schema (mode: closed)"), "{table}");
    assert!(table.contains("=== Vertices ==="));
    assert!(table.contains("=== Edges ==="));
    for fragment in [
        "address.street",
        "address.zip",
        "pattern ^\\d{5}$",
        "address.geo.lat",
        "roles[].name",
        "length 1..80",
        "0..150",
        "items <= 10",
        "additional: false",
        "default null (annotation)",
        "person -> person",
    ] {
        assert!(table.contains(fragment), "{fragment}: {table}");
    }
    // The tinkerpop schema still renders its labels.
    let modern = modern().to_table();
    for label in ["person", "software", "knows", "created"] {
        assert!(modern.contains(label), "{modern}");
    }
}

#[cfg(feature = "display")]
/// A label's own description (in its root node) gets a label row before its properties, for a
/// vertex label as for an edge label; a label without one gets no extra row.
#[test]
fn test_label_description_has_its_own_row() {
    let s = schema(
        r#"{"vertices": {
             "person": {"schema": {"type": "object", "description": "A person",
                "properties": {"name": {"type": "string"}}}},
             "software": {"schema": {"type": "object", "properties": {"lang": {"type": "string"}}}}},
           "edges": {"knows": {"schema": {"type": "object", "title": "Acquaintance",
                "properties": {"weight": {"type": "number"}}}}}}"#,
    );
    let rows = s.property_rows();
    let person: Vec<_> = rows.iter().filter(|r| r.label == "person").collect();
    assert_eq!(person.len(), 2);
    assert_eq!(person[0].property, "-");
    assert_eq!(person[0].notes, "A person");
    assert_eq!(person[1].property, "name");
    assert_eq!(person[1].notes, "");
    let software: Vec<_> = rows.iter().filter(|r| r.label == "software").collect();
    assert_eq!(software.len(), 1);
    let knows: Vec<_> = rows.iter().filter(|r| r.label == "knows").collect();
    assert_eq!(knows[0].notes, "Acquaintance");
    assert_eq!(knows[1].property, "weight");
    assert!(s.to_table().contains("A person"));
    assert!(
        s.to_markdown()
            .contains("| person | - | - | - | - | - | A person |")
    );
}

#[cfg(feature = "display")]
#[test]
fn test_schema_to_table_empty_sections() {
    let s = schema(r#"{"vertices": {"person": {}}}"#);
    let table = s.to_table();
    assert!(table.contains("person"));
    assert!(table.contains("(no edge labels)"));
    let empty = GraphSchema::new().to_table();
    assert!(empty.contains("(no vertex labels)"));
    assert!(empty.contains("(no edge labels)"));
}

#[test]
fn test_schema_to_markdown() {
    let md = modern().to_markdown();
    assert!(md.contains("### Vertices"));
    assert!(md.contains("### Edges"));
    assert!(md.contains("| Label | Property | Type | Required | Nullable | Constraints | Notes |"));
    assert!(
        md.contains("| person | name | string | yes | no | - |  |"),
        "{md}"
    );
    assert!(
        md.contains("| person | age | int64 | yes | no | - |  |"),
        "{md}"
    );
    assert!(md.contains(
        "| Label | Connections | Property | Type | Required | Nullable | Constraints | Notes |"
    ));
    assert!(
        md.contains("| knows | person -> person | weight | float64 |"),
        "{md}"
    );

    let nested = nested().to_markdown();
    assert!(
        nested.contains("| person | address.zip | string | yes | no | pattern ^\\d{5}$ |"),
        "{nested}"
    );
    assert!(
        nested.contains("| person | age | int64 | no | yes | 0..150 | default null (annotation) |"),
        "{nested}"
    );
    // A `|` inside a cell is escaped.
    let piped = schema(
        r#"{"vertices": {"v": {"schema": {"type": "object", "properties": {
            "p": {"type": "string", "pattern": "a|b"}}}}}}"#,
    )
    .to_markdown();
    assert!(piped.contains("pattern a\\|b"), "{piped}");
}

#[test]
fn test_schema_to_tree() {
    let tree = nested().to_tree();
    assert!(tree.starts_with("Schema (mode: closed)"), "{tree}");
    assert!(tree.contains("├── vertex person"), "{tree}");
    assert!(tree.contains("└── edge knows [person -> person]"), "{tree}");
    assert!(
        tree.contains("name: string (required, length 1..80)"),
        "{tree}"
    );
    assert!(
        tree.contains("│   ├── address: object (additional: false)"),
        "{tree}"
    );
    assert!(
        tree.contains("zip: string (required, pattern ^\\d{5}$)"),
        "{tree}"
    );
    assert!(tree.contains("geo: object"), "{tree}");
    assert!(tree.contains("[] items: object"), "{tree}");
    assert_eq!(nested().visualize(VisualizeFormat::Tree), tree);
}

#[test]
fn test_schema_to_json_schema_is_standalone_per_label() {
    let text = nested().to_json_schema();
    let parsed = GraphSchema::from_json(&format!(
        r#"{{"vertices": {{"v": {{"schema": {}}}}}}}"#,
        // each $defs entry is a valid label schema of our subset
        {
            let start = text.find("\"vertex:person\": ").unwrap() + "\"vertex:person\": ".len();
            let end = text.find(",\n    \"edge:knows\"").unwrap();
            text[start..end].to_string()
        }
    ));
    assert!(parsed.is_ok(), "{text}");
    assert!(text.contains("\"$schema\": \"https://json-schema.org/draft/2020-12/schema\""));
    // The mode's default is written out on every object that states none.
    let person = &parsed.unwrap().vertices["v"].schema;
    assert_eq!(person.additional_properties, Some(false));
    assert_eq!(
        person.properties["address"].properties["geo"].additional_properties,
        Some(false)
    );
    assert_eq!(
        person.properties["roles"]
            .items
            .as_ref()
            .unwrap()
            .additional_properties,
        Some(false)
    );
    // Required stays required, nullable stays nullable: valid for our own data.
    assert_eq!(person.required, vec!["name", "tags"]);
    assert!(person.properties["age"].admits_null());
}

#[test]
fn test_schema_to_plantuml_has_topology_and_condensed_json_views() {
    let plantuml = modern().to_plantuml();
    assert!(plantuml.starts_with("@startuml"));
    assert!(plantuml.contains("entity \"person\" as person"));
    assert!(plantuml.contains("entity \"software\" as software"));
    assert!(plantuml.contains("person --> person : \"knows\""));
    assert!(plantuml.contains("person --> software : \"created\""));
    assert!(plantuml.contains("@enduml\n\n@startjson"));
    assert!(plantuml.ends_with("@endjson"));

    let nested = nested().to_plantuml();
    assert!(
        nested.contains("* name : string [length 1..80]"),
        "{nested}"
    );
    assert!(
        nested.contains("address.zip : string [pattern ^\\d{5}$]"),
        "{nested}"
    );
    let json_view = &nested[nested.find("@startjson").unwrap()..];
    assert!(
        json_view.contains(r#""name": "string, required, length 1..80""#),
        "{json_view}"
    );
    assert!(
        json_view.contains(r#""address (object, additional: false)": {"#),
        "{json_view}"
    );
    assert!(
        json_view.contains(r#""zip": "string, required, pattern ^\\d{5}$""#),
        "{json_view}"
    );
    assert!(
        json_view.contains(r#""knows (person -> person)": {"#),
        "{json_view}"
    );
}

#[test]
fn test_schema_visualize_dispatcher() {
    let schema = modern();
    assert!(!schema.visualize(VisualizeFormat::Auto).is_empty());
    assert!(
        schema
            .visualize(VisualizeFormat::Markdown)
            .contains("### Vertices")
    );
    assert!(
        schema
            .visualize(VisualizeFormat::Mermaid)
            .contains("erDiagram")
    );
    let json = schema.visualize(VisualizeFormat::Json);
    assert_eq!(GraphSchema::from_json(&json).unwrap(), schema, "{json}");
    assert!(
        schema
            .visualize(VisualizeFormat::JsonSchema)
            .contains("https://json-schema.org/draft/2020-12/schema")
    );
    assert!(
        schema
            .visualize(VisualizeFormat::PlantUML)
            .contains("@startuml")
    );
    #[cfg(feature = "display")]
    assert!(
        schema
            .visualize(VisualizeFormat::Table)
            .contains("=== Vertices ===")
    );
}
