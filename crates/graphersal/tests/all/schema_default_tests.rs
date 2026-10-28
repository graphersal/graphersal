//! Create-time defaults (`default`, `defaultFunction`; book page "Schema Enforcement"): stored
//! when a vertex or edge is created without the property, in mode `open`/`closed`, before the
//! element is validated; never by imports, change sets or replay; never on read.
#![cfg(all(feature = "script", feature = "io"))]

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use graphersal::changes::{ChangeSet, CommitHook, HookError, Mutation};
use graphersal::io::GraphImport;
use graphersal::prelude::*;
use graphersal::prelude::{Graph, PropertyMap};
use graphersal::schema::{DefaultFunction, PropertySchema, SchemaChangeKind, SchemaDiff};
use graphersal::storage::{AnyGraph, CreateContext, GraphStorage, IntoAnyGraph, Transactional};

use crate::forwarding::Forwarding;
use crate::script_host::{ScriptLimits, eval_value_with_limits};

/// `person` with every kind of default, `knows` with a numeric one.
const SCHEMA: &str = r#"{
  "mode": "open",
  "vertices": {
    "person": {"schema": {"type": "object", "properties": {
      "name": {"type": "string"},
      "active": {"type": "boolean", "default": true},
      "nick": {"type": ["string", "null"], "default": null},
      "level": {"type": "integer", "default": 1, "minimum": 0},
      "uid": {"type": "string", "format": "uuid", "defaultFunction": "uuidv4"},
      "seq": {"type": "string", "format": "uuid", "defaultFunction": "uuidv7"},
      "tags": {"type": "array", "items": {"type": "string"}, "default": ["new"]},
      "home": {"type": "object", "properties": {"city": {"type": "string", "default": "Nowhere"}}}
    }}}
  },
  "edges": {
    "knows": {"schema": {"type": "object", "properties": {
      "weight": {"type": "number", "default": 1}
    }}}
  }
}"#;

fn schema(json: &str) -> GraphSchema {
    GraphSchema::from_json(json).unwrap_or_else(|err| panic!("{}", format_diagnostic(&err)))
}

fn schema_in(json: &str, mode: SchemaMode) -> GraphSchema {
    schema(json).with_mode(mode)
}

/// An empty shared graph of storage `S` with `schema` set through the front end.
fn graph_of<S: GraphStorage + Default + Send + Sync + 'static>(
    storage: S,
    schema: GraphSchema,
) -> Arc<Graph<S>> {
    let graph = Arc::new(Graph::new(storage));
    AnyGraph::set_schema(&*graph, schema).expect("the schema applies");
    graph
}

fn graph(schema: GraphSchema) -> Arc<Graph<TraversalGraph>> {
    graph_of(TraversalGraph::new(), schema)
}

/// Runs a script on `graph`, failing the test on an error.
fn run(graph: impl IntoAnyGraph, script: &str) {
    let _ = eval_value_with_limits(graph, script, HashMap::new(), &ScriptLimits::default())
        .unwrap_or_else(|err| panic!("`{script}`: {err}"));
}

/// Runs a script on `graph` that must fail; its error text.
fn fails(graph: impl IntoAnyGraph, script: &str) -> String {
    eval_value_with_limits(graph, script, HashMap::new(), &ScriptLimits::default())
        .map(|value| panic!("`{script}` should fail, gave {value}"))
        .unwrap_err()
        .to_string()
}

/// The properties of every vertex with `label`, in iteration order.
fn vertex_props<S: GraphStorage>(graph: &Graph<S>, label: &str) -> Vec<PropertyMap> {
    let g = graph.read();
    g.vertices()
        .unwrap()
        .filter(|v| g.vertex_labels(*v).unwrap().iter().any(|l| l == label))
        .map(|v| g.vertex_properties(v).unwrap().cloned().unwrap_or_default())
        .collect()
}

fn one_vertex<S: GraphStorage>(graph: &Graph<S>, label: &str) -> PropertyMap {
    let mut all = vertex_props(graph, label);
    assert_eq!(all.len(), 1, "one {label} vertex");
    all.remove(0)
}

fn edge_props<S: GraphStorage>(graph: &Graph<S>) -> Vec<PropertyMap> {
    let g = graph.read();
    g.edges()
        .unwrap()
        .map(|e| g.edge_properties(e).unwrap().cloned().unwrap_or_default())
        .collect()
}

fn uuid_of(value: Option<&ElementProperty>) -> u128 {
    match value {
        Some(ElementProperty::Uuid(bits)) => *bits,
        other => panic!("expected a uuid, found {other:?}"),
    }
}

fn version(uuid: u128) -> u128 {
    (uuid >> 76) & 0xF
}

fn variant(uuid: u128) -> u128 {
    (uuid >> 62) & 0b11
}

/// The deterministic part of a `person` created with only a name.
fn assert_fixed_defaults(props: &PropertyMap) {
    assert_eq!(props.get("active"), Some(&ElementProperty::Boolean(true)));
    assert_eq!(props.get("level"), Some(&ElementProperty::from(1i64)));
    assert_eq!(
        props.get("tags"),
        Some(&ElementProperty::Array(vec!["new".into()]))
    );
    // `default: null` is never stored, nested defaults are annotations.
    assert!(!props.contains_key("nick"), "{props:?}");
    assert!(!props.contains_key("home"), "{props:?}");
    assert_eq!(version(uuid_of(props.get("uid"))), 4);
    assert_eq!(version(uuid_of(props.get("seq"))), 7);
}

// ---------------------------------------------------------------------------------------------
// Create paths
// ---------------------------------------------------------------------------------------------

#[test]
fn issue_repro_add_v_stores_the_defaults() {
    let g = graph(schema(SCHEMA));
    run(
        g.clone(),
        r#"g.addV("person").property("name", "alice").iterate()"#,
    );
    let props = one_vertex(&g, "person");
    assert_eq!(props.get("name"), Some(&ElementProperty::from("alice")));
    assert_fixed_defaults(&props);
    // Reads show what is stored: the defaults are ordinary values now.
    let shown = eval_value_with_limits(
        g.clone(),
        r#"g.V().values("active").toList()"#,
        HashMap::new(),
        &ScriptLimits::default(),
    )
    .unwrap()
    .to_string();
    assert_eq!(shown, "[true]");
}

#[test]
fn snake_case_add_v_without_properties_stores_the_defaults() {
    let g = graph(schema(SCHEMA));
    run(g.clone(), r#"g.add_v("person").iterate()"#);
    assert_fixed_defaults(&one_vertex(&g, "person"));
}

#[test]
fn explicit_values_and_an_explicit_null_win() {
    let g = graph(schema(SCHEMA));
    run(
        g.clone(),
        r#"g.addV("person").property("active", false).property("nick", ()).property("level", 7)
            .property("uid", UUID.fromString("550e8400-e29b-41d4-a716-446655440000")).iterate()"#,
    );
    let props = one_vertex(&g, "person");
    assert_eq!(props.get("active"), Some(&ElementProperty::Boolean(false)));
    assert_eq!(props.get("nick"), Some(&ElementProperty::Null));
    assert_eq!(props.get("level"), Some(&ElementProperty::from(7i64)));
    assert_eq!(
        uuid_of(props.get("uid")),
        0x550e8400_e29b_41d4_a716_446655440000
    );
    // The key without a value still gets its default.
    assert_eq!(version(uuid_of(props.get("seq"))), 7);
}

#[test]
fn a_property_step_that_is_not_folded_overwrites_the_default() {
    let g = graph(schema(SCHEMA));
    // A traversal-valued property is set after the vertex is created (with its defaults).
    run(
        g.clone(),
        r#"g.addV("person").property("active", __.constant(false)).iterate()"#,
    );
    let props = one_vertex(&g, "person");
    assert_eq!(props.get("active"), Some(&ElementProperty::Boolean(false)));
    assert_eq!(props.get("level"), Some(&ElementProperty::from(1i64)));
}

#[test]
fn the_rust_api_creates_with_defaults() {
    let mut tg = TraversalGraph::new();
    tg.traversal_mut().set_schema(schema(SCHEMA)).unwrap();
    tg.traversal_mut()
        .add_v("person")
        .property("name", "bob")
        .to_list()
        .unwrap();
    // The trait's create path too; the primitive `insert_vertex` stores exactly what it is given.
    let create = CreateContext::for_storage(&tg).unwrap();
    tg.add_vertex_with(
        Some("c".into()),
        vec!["person".into()],
        PropertyMap::default(),
        &create,
    )
    .unwrap();
    tg.insert_vertex(
        Some("raw".into()),
        vec!["person".into()],
        PropertyMap::default(),
    )
    .unwrap();
    let g = Graph::new(tg);
    let all = vertex_props(&g, "person");
    assert_eq!(all.len(), 3);
    let raw = g
        .read()
        .get_vertex_by_id("raw")
        .unwrap()
        .and_then(|v| g.read().vertex_properties(v).unwrap().cloned())
        .unwrap();
    assert!(raw.is_empty(), "insert_vertex applies no default: {raw:?}");
    for props in all.iter().filter(|p| !p.is_empty()) {
        assert_fixed_defaults(props);
    }
}

#[test]
fn merge_v_applies_defaults_on_create_only() {
    let g = graph(schema(SCHEMA));
    run(
        g.clone(),
        r#"g.mergeV(#{"T.label": "person", "name": "bob"}).option(Merge.onCreate, #{"level": 3}).iterate()"#,
    );
    let props = one_vertex(&g, "person");
    assert_eq!(props.get("level"), Some(&ElementProperty::from(3i64)));
    assert_eq!(props.get("active"), Some(&ElementProperty::Boolean(true)));
    // Remove a defaulted property; a merge that matches does not put it back.
    run(
        g.clone(),
        r#"g.V().hasLabel("person").properties("active").drop().iterate();
           g.mergeV(#{"T.label": "person", "name": "bob"}).option(Merge.onMatch, #{"level": 4}).iterate()"#,
    );
    let props = one_vertex(&g, "person");
    assert!(!props.contains_key("active"), "{props:?}");
    assert_eq!(props.get("level"), Some(&ElementProperty::from(4i64)));
}

#[test]
fn add_e_and_merge_e_apply_edge_defaults() {
    let g = graph(schema(SCHEMA));
    run(
        g.clone(),
        r#"g.addV("person").property(T.id, "a").iterate();
           g.addV("person").property(T.id, "b").iterate();
           g.addE("knows").from(__.V("a")).to(__.V("b")).iterate();
           g.mergeE(#{"T.label": "knows", "Direction.OUT": "b", "Direction.IN": "a"}).iterate();
           g.addE("knows").from(__.V("a")).to(__.V("a")).property("weight", 0.5).iterate()"#,
    );
    let mut weights: Vec<String> = edge_props(&g)
        .iter()
        .map(|p| format!("{:?}", p.get("weight")))
        .collect();
    weights.sort();
    // The integer default is coerced to the declared float64, as a written value would be.
    let one = format!("{:?}", Some(ElementProperty::from(1.0f64)));
    let half = format!("{:?}", Some(ElementProperty::from(0.5f64)));
    let mut expected = vec![one.clone(), one, half];
    expected.sort();
    assert_eq!(weights, expected);
}

#[test]
fn multi_label_vertices_take_the_first_declaring_label() {
    let multi = r#"{"mode": "open", "vertices": {
      "A": {"schema": {"type": "object", "properties": {"x": {"type": "string", "default": "from A"}}}},
      "B": {"schema": {"type": "object", "properties": {"x": {"type": "string", "default": "from B"},
                                                        "y": {"type": "integer", "default": 2}}}},
      "C": {"schema": {"type": "object", "properties": {"x": {"type": "string"}}}}
    }}"#;
    let g = graph(schema(multi));
    run(
        g.clone(),
        r#"g.addV(["A", "B"]).property(T.id, "ab").iterate()"#,
    );
    run(
        g.clone(),
        r#"g.addV(["B", "A"]).property(T.id, "ba").iterate()"#,
    );
    run(
        g.clone(),
        r#"g.addV(["C", "B"]).property(T.id, "cb").iterate()"#,
    );
    let props = |id: &str| {
        let r = g.read();
        r.get_vertex_by_id(id)
            .unwrap()
            .and_then(|v| r.vertex_properties(v).unwrap().cloned())
            .unwrap()
    };
    assert_eq!(props("ab").get("x"), Some(&"from A".into()));
    assert_eq!(props("ab").get("y"), Some(&ElementProperty::from(2i64)));
    assert_eq!(props("ba").get("x"), Some(&"from B".into()));
    // C declares x first without a default: C decides, nothing is filled for x.
    assert_eq!(props("cb").get("x"), None);
    assert_eq!(props("cb").get("y"), Some(&ElementProperty::from(2i64)));
}

#[test]
fn a_required_property_is_satisfied_by_its_default() {
    let required = r#"{"mode": "closed", "vertices": {"task": {"schema": {"type": "object",
      "properties": {"state": {"type": "string", "enum": ["open", "done"], "default": "open"},
                     "title": {"type": "string"}},
      "required": ["state", "title"]}}}}"#;
    let g = graph(schema(required));
    run(
        g.clone(),
        r#"g.addV("task").property("title", "write docs").iterate()"#,
    );
    assert_eq!(one_vertex(&g, "task").get("state"), Some(&"open".into()));
    // A required key without a default is still missing.
    let err = fails(g.clone(), r#"g.addV("task").iterate()"#);
    assert!(err.contains("title"), "{err}");
}

#[test]
fn closed_mode_applies_defaults_and_none_mode_does_not() {
    let g = graph(schema_in(SCHEMA, SchemaMode::Closed));
    run(g.clone(), r#"g.addV("person").iterate()"#);
    assert_fixed_defaults(&one_vertex(&g, "person"));

    let g = graph(schema_in(SCHEMA, SchemaMode::None));
    run(g.clone(), r#"g.addV("person").iterate()"#);
    assert!(one_vertex(&g, "person").is_empty());
}

#[test]
fn a_canonical_string_default_of_a_uuid_property_is_stored_as_a_uuid() {
    let json = r#"{"mode": "open", "vertices": {"n": {"schema": {"type": "object", "properties":
      {"ref": {"type": "string", "format": "uuid", "default": "550e8400-e29b-41d4-a716-446655440000"}}}}}}"#;
    let g = graph(schema(json));
    run(g.clone(), r#"g.addV("n").iterate()"#);
    assert_eq!(
        uuid_of(one_vertex(&g, "n").get("ref")),
        0x550e8400_e29b_41d4_a716_446655440000
    );
}

#[test]
fn every_created_element_gets_its_own_generated_uuids() {
    let g = graph(schema(SCHEMA));
    run(
        g.clone(),
        r#"for i in 0..100 { g.addV("seed").iterate(); }
           g.V().hasLabel("seed").addV("person").iterate()"#,
    );
    let people = vertex_props(&g, "person");
    assert_eq!(people.len(), 100);
    let v4: HashSet<u128> = people.iter().map(|p| uuid_of(p.get("uid"))).collect();
    let v7: HashSet<u128> = people.iter().map(|p| uuid_of(p.get("seq"))).collect();
    assert_eq!(v4.len(), 100, "uuidv4: a new value per element");
    assert_eq!(v7.len(), 100, "uuidv7: a new value per element");
    for uuid in &v4 {
        assert_eq!((version(*uuid), variant(*uuid)), (4, 0b10));
    }
    let now = web_time_millis();
    for uuid in &v7 {
        assert_eq!((version(*uuid), variant(*uuid)), (7, 0b10));
        let stamp = uuid >> 80;
        assert!(
            stamp.abs_diff(now) < 600_000,
            "unix milliseconds: {stamp} vs {now}"
        );
    }
}

fn web_time_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------------------------
// Storage-independent: the trait's default `add_vertex_with`/`add_edge_with`
// ---------------------------------------------------------------------------------------------

#[test]
fn a_foreign_storage_gets_the_same_defaults() {
    let g = graph_of(Forwarding::new(TraversalGraph::new()), schema(SCHEMA));
    run(
        g.clone(),
        r#"g.addV("person").property(T.id, "a").property("name", "alice").iterate();
           g.mergeV(#{"T.label": "person", "T.id": "b"}).iterate();
           g.addE("knows").from(__.V("a")).to(__.V("b")).iterate()"#,
    );
    let people = vertex_props(&g, "person");
    assert_eq!(people.len(), 2);
    for props in &people {
        assert_fixed_defaults(props);
    }
    assert_eq!(
        edge_props(&g)[0].get("weight"),
        Some(&ElementProperty::from(1.0f64))
    );
}

// ---------------------------------------------------------------------------------------------
// What never applies defaults
// ---------------------------------------------------------------------------------------------

/// SCHEMA without generated values, so whole states can be compared.
const FIXED: &str = r#"{"mode": "open", "vertices": {"person": {"schema": {"type": "object",
  "properties": {"name": {"type": "string"}, "active": {"type": "boolean", "default": true}}}}}}"#;

#[test]
fn imports_store_what_they_carry() {
    let mut g = Graph::new(TraversalGraph::new());
    AnyGraph::set_schema(&g, schema(FIXED)).unwrap();
    g.import_graphson_reader(
        r#"{"id":"x","label":"person","properties":{"name":[{"id":0,"value":"zed"}]}}"#.as_bytes(),
    )
    .unwrap();
    let props = one_vertex(&g, "person");
    assert_eq!(props.get("name"), Some(&"zed".into()));
    assert!(!props.contains_key("active"), "{props:?}");
}

#[test]
fn applying_a_change_set_stores_what_it_carries() {
    let mut tg = TraversalGraph::new();
    tg.traversal_mut().set_schema(schema(FIXED)).unwrap();
    let mut name = PropertyMap::default();
    name.insert("name", "zed".into());
    let mut builder = ChangeSet::builder(1, 0);
    builder.push(Mutation::add_vertex("x", ["person"], name));
    let changes = builder.build().unwrap();
    tg.apply_change_set(&changes).unwrap();
    let g = Graph::new(tg);
    assert!(!one_vertex(&g, "person").contains_key("active"));
}

thread_local! {
    static RECORDED: RefCell<Vec<ChangeSet>> = const { RefCell::new(Vec::new()) };
}

struct Recorder;

impl CommitHook for Recorder {
    fn before_commit(&mut self, changes: &ChangeSet) -> Result<(), HookError> {
        RECORDED.with(|recorded| recorded.borrow_mut().push(changes.clone()));
        Ok(())
    }
}

#[test]
fn the_change_set_records_the_stored_defaults_and_replay_reproduces_them() {
    let mut tg = TraversalGraph::new();
    tg.register_commit_hook(Box::new(Recorder)).unwrap();
    tg.traversal_mut().set_schema(schema(SCHEMA)).unwrap();
    tg.traversal_mut().add_v("person").to_list().unwrap();
    let id = tg
        .vertices()
        .unwrap()
        .next()
        .and_then(|v| tg.vertex_id(v).unwrap().map(str::to_string))
        .unwrap();
    let recorded = RECORDED.with(|r| std::mem::take(&mut *r.borrow_mut()));
    assert_eq!(recorded.len(), 2, "set_schema, then add_v");
    let added = recorded[1]
        .mutations()
        .iter()
        .find_map(|m| match m {
            Mutation::AddVertex { properties, .. } => Some(properties.clone()),
            _ => None,
        })
        .expect("an AddVertex");
    assert_fixed_defaults(&added);
    let stored = tg
        .get_vertex_by_id(&id)
        .unwrap()
        .and_then(|v| tg.vertex_properties(v).unwrap().cloned())
        .unwrap();
    assert_eq!(added, stored, "the change set holds the generated values");

    // Replay (unpublished graph, no hooks): exactly the recorded values, nothing regenerated.
    let mut replayed = TraversalGraph::new();
    for changes in &recorded {
        replayed = replayed.replay(changes).unwrap();
    }
    let again = replayed
        .get_vertex_by_id(&id)
        .unwrap()
        .and_then(|v| replayed.vertex_properties(v).unwrap().cloned())
        .unwrap();
    assert_eq!(again, stored);
}

#[test]
fn a_rolled_back_create_leaves_nothing() {
    let mut tg = TraversalGraph::new();
    tg.traversal_mut().set_schema(schema(SCHEMA)).unwrap();
    let result: Result<(), GraphError> = tg.transaction(|g| {
        let create = CreateContext::for_storage(&*g).unwrap();
        g.add_vertex_with(None, vec!["person".into()], PropertyMap::default(), &create)?;
        Err(GraphError::unsupported("veto"))
    });
    assert!(result.is_err());
    assert_eq!(tg.vertices().unwrap().count(), 0);
    // A failing traversal after the create rolls back too.
    let g = graph(schema(SCHEMA));
    fails(
        g.clone(),
        r#"g.addV("person").property("level", -1).iterate()"#,
    );
    assert!(vertex_props(&g, "person").is_empty());
}

#[test]
fn a_new_default_fills_no_existing_element() {
    let g = graph(schema(
        r#"{"mode": "open", "vertices": {"person": {"schema": {"type": "object",
      "properties": {"name": {"type": "string"}}}}}}"#,
    ));
    run(
        g.clone(),
        r#"g.addV("person").property("name", "old").iterate()"#,
    );
    AnyGraph::set_schema(&*g, schema(FIXED)).unwrap();
    assert!(!one_vertex(&g, "person").contains_key("active"));
}

// ---------------------------------------------------------------------------------------------
// The schema format
// ---------------------------------------------------------------------------------------------

fn person_with(property: &str) -> String {
    format!(
        r#"{{"mode": "open", "vertices": {{"p": {{"schema": {{"type": "object", "properties": {{"a": {property}}}}}}}}}}}"#
    )
}

fn invalid(result: Result<impl std::fmt::Debug, impl DiagnosticError>) -> String {
    match result {
        Ok(value) => panic!("expected an error, got {value:?}"),
        Err(err) => format_diagnostic(&err),
    }
}

#[test]
fn default_function_values_outside_the_closed_set_are_rejected() {
    // `now` and `today` belong to the set (their type checks: `schema_default_date_tests`).
    for name in ["now", "today"] {
        GraphSchema::from_json(&person_with(&format!(
            r#"{{"type": "string", "defaultFunction": "{name}"}}"#
        )))
        .unwrap_or_else(|err| panic!("{name}: {}", format_diagnostic(&err)));
    }
    let text = invalid(GraphSchema::from_json(&person_with(
        r#"{"type": "string", "defaultFunction": "uuidv1"}"#,
    )));
    assert!(text.contains("unknown defaultFunction 'uuidv1'"), "{text}");
    let text = invalid(GraphSchema::from_json(&person_with(
        r#"{"type": "string", "defaultFunction": 4}"#,
    )));
    assert!(text.contains("defaultFunction"), "{text}");
}

#[test]
fn default_and_default_function_together_are_rejected() {
    let text = invalid(GraphSchema::from_json(&person_with(
        r#"{"type": "string", "format": "uuid", "default": "550e8400-e29b-41d4-a716-446655440000", "defaultFunction": "uuidv4"}"#,
    )));
    assert!(
        text.contains("default and defaultFunction on the same property"),
        "{text}"
    );
}

#[test]
fn an_invalid_default_is_rejected_when_the_schema_is_set() {
    let cases = [
        (
            r#"{"type": "integer", "default": "x"}"#,
            "/vertices/p/schema/properties/a/default",
        ),
        (
            r#"{"type": "integer", "minimum": 0, "default": -1}"#,
            "/default",
        ),
        (
            r#"{"type": "string", "enum": ["x"], "default": "y"}"#,
            "/default",
        ),
        (
            r#"{"type": "string", "format": "uuid", "default": "not-a-uuid"}"#,
            "/default",
        ),
        (
            r#"{"type": "string", "defaultFunction": "uuidv4"}"#,
            "/defaultFunction",
        ),
        (
            r#"{"type": "integer", "defaultFunction": "uuidv7"}"#,
            "/defaultFunction",
        ),
        (
            r#"{"type": "object", "properties": {"b": {"type": "string", "format": "uuid", "defaultFunction": "uuidv4"}}}"#,
            "/vertices/p/schema/properties/a/properties/b/defaultFunction",
        ),
        (
            r#"{"type": "array", "items": {"type": "string", "format": "uuid", "defaultFunction": "uuidv4"}}"#,
            "/properties/a/items/defaultFunction",
        ),
    ];
    for (property, location) in cases {
        // The format accepts it; setting (and validating, patching) it refuses it.
        let parsed = schema(&person_with(property));
        let mut tg = TraversalGraph::new();
        let text = invalid(tg.traversal_mut().set_schema(parsed.clone()));
        assert!(text.contains("Invalid schema at"), "{property}: {text}");
        assert!(text.contains(location), "{property}: {text}");
        let text = invalid(tg.traversal().validate_schema(&parsed));
        assert!(text.contains(location), "{property}: {text}");
        assert!(tg.traversal().get_schema().unwrap().is_none());
    }
    // A `default: null` is never stored, so it is not checked; a nested `default` stays an
    // annotation and is not checked either.
    let mut tg = TraversalGraph::new();
    tg.traversal_mut()
        .set_schema(schema(&person_with(
            r#"{"type": "integer", "default": null}"#,
        )))
        .unwrap();
    tg.traversal_mut()
        .set_schema(schema(&person_with(
            r#"{"type": "object", "properties": {"b": {"type": "integer", "default": "x"}}}"#,
        )))
        .unwrap();
}

#[test]
fn an_invalid_default_is_rejected_by_the_dsl_and_a_patch() {
    let g = graph(schema(FIXED));
    let err = fails(
        g.clone(),
        r#"g.set_schema(#{mode: "open", vertices: #{p: #{schema: #{type: "object", properties: #{a: #{type: "integer", default: "x"}}}}}})"#,
    );
    assert!(err.contains("default does not fit the property"), "{err}");
    let err = fails(
        g.clone(),
        r#"g.patch_schema(#{vertices: #{person: #{schema: #{properties: #{active: #{default: 3}}}}}})"#,
    );
    assert!(err.contains("default does not fit the property"), "{err}");
    let err = fails(
        g.clone(),
        r#"g.set_schema(#{mode: "open", vertices: #{p: #{schema: #{type: "object", properties: #{a: #{type: "string", defaultFunction: "today"}}}}}})"#,
    );
    assert!(
        err.contains(r#"declare {"type": "string", "format": "date"}"#),
        "{err}"
    );
}

#[test]
fn the_canonical_form_round_trips_default_function_and_the_builders_match() {
    let parsed = schema(SCHEMA);
    let again = GraphSchema::from_json(&parsed.to_json()).unwrap();
    assert_eq!(parsed, again);
    let text = parsed.to_json();
    assert!(text.contains(r#""defaultFunction":"uuidv4""#), "{text}");

    let built = PropertySchema::uuid().with_default_function(DefaultFunction::Uuidv7);
    let loaded = schema(
        r#"{"vertices": {"x": {"schema": {"type": "object", "properties": {"k": {"type": "string", "format": "uuid", "defaultFunction": "uuidv7"}}}}}}"#,
    );
    assert_eq!(built, loaded.vertices["x"].schema.properties["k"]);
    assert_eq!(
        PropertySchema::from_value(&built.to_value()).unwrap(),
        built
    );
    assert_eq!(
        PropertySchema::boolean()
            .with_default(true.into())
            .annotations()
            .and_then(|a| a.default.clone()),
        Some(ElementProperty::Boolean(true))
    );
    assert_eq!(
        DefaultFunction::from_name("uuidv7"),
        Some(DefaultFunction::Uuidv7)
    );
    assert_eq!(DefaultFunction::Uuidv4.as_str(), "uuidv4");
}

#[test]
fn inference_never_infers_defaults() {
    let g = graph(schema(SCHEMA));
    run(g.clone(), r#"g.addV("person").iterate()"#);
    let inferred = g.read().traversal_infer();
    assert!(!inferred.contains("default"), "{inferred}");
}

/// `infer_schema()` of a read guard, as JSON text.
trait InferText {
    fn traversal_infer(&self) -> String;
}

impl InferText for TraversalGraph {
    fn traversal_infer(&self) -> String {
        self.traversal().infer_schema().unwrap().to_json()
    }
}

#[test]
fn a_changed_default_is_a_compatible_default_changed_entry() {
    let before = schema(FIXED);
    let after = schema(&FIXED.replace(r#""default": true"#, r#""default": false"#));
    let diff = SchemaDiff::between(Some(&before), &after);
    assert!(diff.compatible());
    let change = diff
        .changes
        .iter()
        .find(|c| c.kind == SchemaChangeKind::DefaultChanged)
        .expect("a default_changed entry");
    assert_eq!(change.before.as_deref(), Some("default true"));
    assert_eq!(change.after.as_deref(), Some("default false"));
    assert_eq!(change.kind.as_str(), "default_changed");
    assert!(
        !diff
            .changes
            .iter()
            .any(|c| c.kind == SchemaChangeKind::AnnotationChanged)
    );

    let generated = schema(SCHEMA);
    let removed = schema(&SCHEMA.replace(r#", "defaultFunction": "uuidv4""#, ""));
    let diff = SchemaDiff::between(Some(&generated), &removed);
    assert!(diff.compatible());
    let change = diff
        .changes
        .iter()
        .find(|c| c.kind == SchemaChangeKind::DefaultChanged)
        .expect("a default_changed entry");
    assert_eq!(change.before.as_deref(), Some("defaultFunction uuidv4"));
    assert_eq!(change.after, None);
}
