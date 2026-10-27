//! The seven schema methods in Rust and in the DSL (both spellings): `get_schema`,
//! `set_schema`, `patch_schema`, `validate_schema`, `validate_schema_patch`, `infer_schema`,
//! `diff_schema`; units and change capture; the `changes` flags; `default` is never applied.
#![cfg(feature = "script")]

use std::sync::Arc;

use graphersal::auth::AccessPolicy;
use graphersal::changes::{ChangeSet, Mutation};
use graphersal::prelude::*;
use graphersal::schema::{
    JsonType, PropertySchema, SCHEMA_FORMAT_URL, SchemaChangeKind, SchemaDiff, SchemaPatch,
};
use graphersal::storage::UnitChanges;

use crate::schema_support::{props, schema};
use crate::script_host::{eval_value, eval_value_atomic, rhai};

const PERSON: &str = r#"{"mode": "open", "meta": {"version": "1"}, "vertices": {"person": {"schema": {
    "type": "object", "properties": {
      "name": {"type": "string", "minLength": 1},
      "age": {"type": ["integer", "null"], "minimum": 0, "default": 18}},
    "required": ["name"]}}}}"#;

fn modern() -> Arc<Graph> {
    Arc::new(GraphSource::tinkerpop_modern())
}

fn run(graph: &Arc<Graph>, script: &str) -> Result<rhai::Dynamic, String> {
    eval_value(graph.clone(), script).map_err(|e| e.to_string())
}

/// The schema of a script value (a Rhai map keeps its keys sorted, so the property order of a
/// schema that went through a map is alphabetical; equality ignores the order).
fn schema_of(value: rhai::Dynamic) -> GraphSchema {
    let property = graphersal::script::script_value_to_property(value, 128).unwrap();
    GraphSchema::from_value(&property).expect("a schema map")
}

// ---------------------------------------------------------------------------------------------
// Rust
// ---------------------------------------------------------------------------------------------

#[test]
fn the_rust_methods_round_trip_and_never_infer_on_get() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    assert_eq!(graph.traversal().get_schema().unwrap(), None);
    let person = schema(PERSON);
    graph.traversal_mut().set_schema(person.clone()).unwrap();
    assert_eq!(
        graph.traversal().get_schema().unwrap(),
        Some(person.clone())
    );

    // patch: closed mode, a new optional key; `null` removes a member.
    let patch = SchemaPatch::from_json(
        r#"{"mode": "closed", "meta": null, "vertices": {"person": {"schema": {"properties": {
            "nick": {"type": "string"}}}}, "software": {}}}"#,
    )
    .unwrap();
    let preview = graph.traversal().validate_schema_patch(&patch).unwrap();
    assert!(
        !preview.is_valid(),
        "closed: knows/created edges are undeclared"
    );
    assert!(!preview.compatible(), "open -> closed is stricter");
    assert_eq!(
        graph.traversal().get_schema().unwrap(),
        Some(person.clone()),
        "a dry run"
    );
    let err = graph.traversal_mut().patch_schema(&patch).unwrap_err();
    assert!(
        err.to_string().contains("Cannot apply the Closed schema"),
        "{err}"
    );
    let ok_patch = SchemaPatch::from_json(
        r#"{"vertices": {"person": {"schema": {"properties": {"nick": {"type": "string"}}}}}}"#,
    )
    .unwrap();
    let patched = graph.traversal_mut().patch_schema(&ok_patch).unwrap();
    assert!(
        patched.vertices["person"]
            .schema
            .properties
            .contains_key("nick")
    );
    assert_eq!(patched.meta, person.meta, "untouched members stay");
    assert_eq!(
        graph.traversal().get_schema().unwrap(),
        Some(patched.clone())
    );
    // property order is kept by a patch
    let keys: Vec<&String> = patched.vertices["person"]
        .schema
        .properties
        .keys()
        .collect();
    assert_eq!(keys, ["name", "age", "nick"]);

    // diff
    let diff = graph.traversal().diff_schema(&person).unwrap();
    assert_eq!(diff.changes.len(), 1);
    assert_eq!(diff.changes[0].kind, SchemaChangeKind::PropertyRemoved);
    assert_eq!(diff.changes[0].path.as_deref(), Some("$.nick"));
    assert!(
        diff.compatible(),
        "removing a declaration from an open object is compatible"
    );
    // ... while adding the optional key was compatible
    assert!(SchemaDiff::between(Some(&person), &patched).compatible());

    // infer never stores
    let inferred = graph.traversal().infer_schema().unwrap();
    assert_eq!(inferred.mode, SchemaMode::None);
    assert_eq!(graph.traversal().get_schema().unwrap(), Some(patched));
}

#[test]
fn an_invalid_patch_result_is_a_format_error() {
    let mut graph = TraversalGraph::new();
    let patch = SchemaPatch::from_json(r#"{"mdoe": "closed"}"#).unwrap();
    let err = graph.traversal_mut().patch_schema(&patch).unwrap_err();
    assert!(matches!(
        err.root_cause(),
        TraverserError::Schema(SchemaError::InvalidSchema { .. })
    ));
    assert!(SchemaPatch::from_json("{").is_err());
}

#[test]
fn an_authorizer_on_the_source_guards_the_schema_methods() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let read_only = std::sync::Arc::new(AccessPolicy::read_only());
    {
        let g = graph.traversal().with_authorizer(read_only.clone());
        assert!(g.get_schema().is_ok());
        assert!(g.infer_schema().is_ok());
    }
    {
        let mut g = graph.traversal_mut().with_authorizer(read_only);
        let err = g.set_schema(schema(PERSON)).unwrap_err();
        assert!(
            matches!(err.root_cause(), TraverserError::PermissionDenied { .. }),
            "{err}"
        );
    }
    let denied = std::sync::Arc::new(AccessPolicy::deny_all());
    let g = graph.traversal().with_authorizer(denied);
    assert!(g.get_schema().is_err() && g.diff_schema(&GraphSchema::new()).is_err());
}

/// A schema change is a database change: one unit (rolled back with an enclosing one),
/// captured as `SetSchema` with the canonical JSON, advancing the commit sequence.
#[test]
fn a_schema_change_is_a_unit_with_a_set_schema_mutation() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let seq = graph.last_commit_seq();
    graph.traversal_mut().set_schema(schema(PERSON)).unwrap();
    assert_eq!(graph.last_commit_seq(), seq + 1);
    let (_, changes) = graph
        .dry_run(|g| {
            g.traversal_mut()
                .patch_schema(&SchemaPatch::from_json(r#"{"mode": "none"}"#).unwrap())
                .map(drop)
        })
        .unwrap();
    match changes.mutations() {
        [Mutation::SetSchema { before, after, .. }] => {
            assert_eq!(before.mode, SchemaMode::Open);
            assert_eq!(after.mode, SchemaMode::None);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        graph.get_schema().unwrap().unwrap().mode,
        SchemaMode::Open,
        "rolled back"
    );
    #[cfg(feature = "serde")]
    {
        let json = serde_json::to_string(&changes).unwrap();
        assert!(json.contains(r#""op":"set_schema""#), "{json}");
        assert!(
            json.contains(SCHEMA_FORMAT_URL),
            "the canonical format: {json}"
        );
        let back: ChangeSet = serde_json::from_str(&json).unwrap();
        assert_eq!(back.mutations(), changes.mutations());
    }
    // Inside a transaction: in effect immediately, gone with the rollback.
    let result: Result<(), GraphError> = graph.transaction(|g| {
        g.set_schema(GraphSchema::new().with_mode(SchemaMode::Closed))?;
        assert_eq!(g.get_schema().unwrap().unwrap().mode, SchemaMode::Closed);
        // The schema of the unit applies at once: this fails and rolls the unit back.
        g.add_vertex(None, vec!["undeclared".into()], PropertyMap::default())
            .map(drop)
    });
    assert!(result.is_err());
    assert_eq!(graph.get_schema().unwrap().unwrap().mode, SchemaMode::Open);
}

/// The backfill recipe of the book: make a key required in one transaction.
#[test]
fn backfill_then_require_in_one_transaction() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let inferred = graph.traversal().infer_schema().unwrap();
    graph
        .set_schema(inferred.with_mode(SchemaMode::Closed))
        .unwrap();
    // `email` is new: requiring it right away fails on the existing persons.
    let require = SchemaPatch::from_json(
        r#"{"vertices": {"person": {"schema": {"properties": {"email": {"type": "string"}},
            "required": ["name", "age", "email"]}}}}"#,
    )
    .unwrap();
    assert!(graph.traversal_mut().patch_schema(&require).is_err());
    let declare = SchemaPatch::from_json(
        r#"{"vertices": {"person": {"schema": {"properties": {"email": {"type": "string"}}}}}}"#,
    )
    .unwrap();
    graph
        .transaction(|g| -> TraverserResult<()> {
            g.traversal_mut().patch_schema(&declare)?;
            g.traversal_mut()
                .v(None)
                .has_label("person")
                .property("email", "unknown@example.org")
                .to_list()?;
            g.traversal_mut().patch_schema(&require)?;
            Ok(())
        })
        .unwrap();
    assert!(
        graph.get_schema().unwrap().unwrap().vertices["person"]
            .schema
            .is_required("email")
    );
}

// ---------------------------------------------------------------------------------------------
// default is applied on create only
// ---------------------------------------------------------------------------------------------

/// The `age` values of the person vertices by name (`None` when the vertex has no `age`).
fn ages(graph: &TraversalGraph) -> Vec<(String, Option<ElementProperty>)> {
    let stored = graph
        .traversal()
        .v(None)
        .value_map(None::<()>)
        .to_list()
        .unwrap();
    let mut out: Vec<(String, Option<ElementProperty>)> = stored
        .iter()
        .map(|value| {
            let ElementProperty::Object(map) = value else {
                panic!("{stored:?}")
            };
            let first = |key: &str| match map.get(key) {
                Some(ElementProperty::Array(items)) => items.first().cloned(),
                other => other.cloned(),
            };
            (format!("{:?}", first("name")), first("age"))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[test]
fn default_is_applied_on_create_only() {
    // A person created before the schema has a default for `age` ...
    let mut graph = TraversalGraph::new();
    let mut without_default = schema(PERSON);
    without_default
        .vertices
        .get_mut("person")
        .unwrap()
        .schema
        .properties["age"] = PropertySchema::of_types(&[JsonType::Integer, JsonType::Null])
        .with_constraint(|c| c.minimum = Some(Number::from_i64(0)));
    graph.set_schema(without_default).unwrap();
    graph
        .traversal_mut()
        .add_v("person")
        .property("name", "Bob")
        .to_list()
        .unwrap();
    graph.set_schema(schema(PERSON)).unwrap();
    // ... and one created after it.
    graph
        .traversal_mut()
        .add_v("person")
        .property("name", "Ann")
        .to_list()
        .unwrap();
    // Written when the element is created, never filled into the existing one (no backfill),
    // and never substituted on read.
    let ann = format!("{:?}", Some(ElementProperty::from("Ann")));
    let bob = format!("{:?}", Some(ElementProperty::from("Bob")));
    assert_eq!(
        ages(&graph),
        vec![(ann.clone(), Some(18.into())), (bob.clone(), None)]
    );
    assert_eq!(
        graph.traversal().v(None).values("age").to_list().unwrap(),
        vec![ElementProperty::from(18)],
        "only Ann's stored age is read"
    );
    // A required key with a default is satisfied by its default on create ...
    let mut with_default = schema(PERSON);
    with_default.vertices.get_mut("person").unwrap().schema = with_default.vertices["person"]
        .schema
        .clone()
        .with_required_property(
            "level",
            PropertySchema::integer().with_annotation(|a| a.default = Some(1.into())),
        );
    let mut fresh = TraversalGraph::new();
    fresh.set_schema(with_default).unwrap();
    let created = fresh
        .traversal_mut()
        .add_v("person")
        .property("name", "A")
        .values("level")
        .to_list()
        .unwrap();
    assert_eq!(created, vec![ElementProperty::from(1)]);
    // ... while the primitive `insert_vertex` stores exactly what it is given, so it is still
    // required.
    let err = fresh
        .insert_vertex(None, vec!["person".into()], props(&[("name", "A".into())]))
        .unwrap_err();
    assert!(matches!(
        err,
        GraphError::Schema(SchemaError::MissingRequiredProperty { ref property_name, .. }) if property_name == "level"
    ));
    // Changing a default changes no data and is compatible.
    let mut changed = schema(PERSON);
    changed
        .vertices
        .get_mut("person")
        .unwrap()
        .schema
        .properties["age"] = PropertySchema::of_types(&[JsonType::Integer, JsonType::Null])
        .with_constraint(|c| c.minimum = Some(Number::from_i64(0)))
        .with_annotation(|a| a.default = Some(21.into()));
    let diff = graph.traversal().diff_schema(&changed).unwrap();
    assert_eq!(diff.changes.len(), 1);
    assert_eq!(diff.changes[0].kind, SchemaChangeKind::DefaultChanged);
    assert!(diff.compatible());
    graph.set_schema(changed).unwrap();
    assert_eq!(ages(&graph), vec![(ann, Some(18.into())), (bob, None)]);
}

/// The diff is static: an optional key added to an open object is compatible, and existing data
/// that holds the key with another type is found by `validate_schema`.
#[test]
fn a_compatible_diff_can_still_conflict_with_existing_data() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    graph
        .traversal_mut()
        .set_schema(schema(
            r#"{"mode": "open", "vertices": {"person": {"schema": {"type": "object"}}}}"#,
        ))
        .unwrap();
    let proposed = schema(
        r#"{"mode": "open", "vertices": {"person": {"schema": {"type": "object",
            "properties": {"age": {"type": "string"}}}}}}"#,
    );
    let validation = graph.traversal().validate_schema(&proposed).unwrap();
    assert!(validation.compatible(), "{:#?}", validation.diff);
    assert!(!validation.is_valid(), "every person's age is an int64");
    assert!(graph.traversal_mut().set_schema(proposed).is_err());
}

// ---------------------------------------------------------------------------------------------
// DSL
// ---------------------------------------------------------------------------------------------

#[test]
fn the_dsl_methods_in_both_spellings_take_maps_and_json_text() {
    let graph = modern();
    assert!(run(&graph, "g.get_schema()").unwrap().is_unit());
    assert!(run(&graph, "g.get_schema()").unwrap().is_unit());
    let _ = run(&graph, &format!("g.set_schema(`{PERSON}`)")).unwrap();
    assert_eq!(
        schema_of(run(&graph, "g.get_schema()").unwrap()),
        schema(PERSON)
    );
    // A map works too, and the result of get_schema feeds set_schema unchanged.
    let _ = run(
        &graph,
        "let s = g.get_schema(); s.mode = \"none\"; g.set_schema(s)",
    )
    .unwrap();
    assert_eq!(
        graph.read().get_schema().unwrap().unwrap().mode,
        SchemaMode::None
    );
    let _ = run(&graph, r#"g.patch_schema(#{mode: "open"})"#).unwrap();
    let patched = run(&graph, r#"g.patch_schema(`{"meta": {"status": "draft"}}`)"#).unwrap();
    assert!(schema_of(patched).to_json().contains(r#""status":"draft""#));

    let validation = run(
        &graph,
        r#"g.validate_schema(#{mode: "closed", vertices: #{person: #{}}})"#,
    )
    .unwrap()
    .cast::<rhai::Map>();
    assert_eq!(validation["compatible"].as_bool(), Ok(false));
    let violations = validation["violations"].clone().cast::<rhai::Map>();
    assert!(violations["total_violations"].as_int().unwrap() > 0);
    let groups = violations["groups"].clone().into_array().unwrap();
    let first = groups[0].clone().cast::<rhai::Map>();
    for key in ["kind", "element", "label", "field", "count", "sample_ids"] {
        assert!(first.contains_key(key), "{key}");
    }
    let diff = validation["diff"].clone().into_array().unwrap();
    assert!(!diff.is_empty());

    let patch_check = run(
        &graph,
        r#"g.validate_schema_patch(#{vertices: #{person: #{schema: #{properties: #{nick: #{type: "string"}}}}}})"#,
    )
    .unwrap()
    .cast::<rhai::Map>();
    assert_eq!(
        patch_check["violations"].clone().cast::<rhai::Map>()["total_violations"].as_int(),
        Ok(0)
    );

    let diff = run(&graph, r#"g.diff_schema(#{mode: "none"})"#)
        .unwrap()
        .cast::<rhai::Map>();
    assert_eq!(diff["compatible"].as_bool(), Ok(true));
    let changes = diff["changes"].clone().into_array().unwrap();
    let kinds: Vec<String> = changes
        .iter()
        .map(|c| c.clone().cast::<rhai::Map>()["kind"].to_string())
        .collect();
    assert!(kinds.contains(&"label_removed".to_string()), "{kinds:?}");
}

#[test]
fn dsl_errors_name_the_problem() {
    let graph = modern();
    let err = run(&graph, r#"g.set_schema(#{mdoe: "closed"})"#).unwrap_err();
    assert!(err.contains("Invalid schema at /mdoe"), "{err}");
    let err = run(&graph, "g.set_schema(42)").unwrap_err();
    assert!(err.contains("a schema (a map or JSON text)"), "{err}");
    let err = run(&graph, "g.patch_schema([1])").unwrap_err();
    assert!(err.contains("a schema patch"), "{err}");
    let err = run(
        &graph,
        r#"g.set_schema(`{"vertices": {"p": {"schema": {"type": "object",
        "properties": {"d": {"type": "string", "format": "email"}}}}}}`)"#,
    )
    .unwrap_err();
    assert!(
        err.contains("unsupported format 'email'")
            && err.contains("The supported formats are uuid, date-time"),
        "{err}"
    );
}

/// A whole-script unit: the schema change rolls back with the script, and an execution
/// inside it reports what it changed.
#[test]
fn a_script_unit_rolls_the_schema_back_and_executions_report_changes() {
    let graph = modern();
    let failed = eval_value_atomic(
        graph.clone(),
        r#"g.set_schema(#{mode: "open"}); g.V("nope").next().unwrap()"#,
        Default::default(),
        &graphersal::script::ScriptLimits::default(),
    );
    assert!(failed.is_err());
    assert!(
        graph.read().get_schema().unwrap().is_none(),
        "rolled back with the script"
    );

    let changes = |script: &str| {
        let map = run(&graph, script).unwrap().cast::<rhai::Map>();
        (
            map["data"].as_bool().unwrap(),
            map["schema"].as_bool().unwrap(),
        )
    };
    assert_eq!(changes("g.V().count().execute().changes"), (false, false));
    assert_eq!(changes(r#"g.addV("x").execute().changes"#), (true, false));
    let map = run(&graph, r#"g.addV("y").execute().to_map()"#)
        .unwrap()
        .cast::<rhai::Map>();
    assert_eq!(map["mutated"].as_bool(), Ok(true));
    assert!(map.contains_key("changes"));
}

#[test]
fn rust_executions_report_changes() {
    let graph = TraversalGraph::tinkerpop_modern();
    let mut g = GraphSource::tinkerpop_modern();
    let read = graph.traversal().v(None).count().execute();
    assert_eq!(read.changes(), UnitChanges::NONE);
    let write = g.get_mut().traversal_mut().add_v("x").execute();
    assert!(write.mutated());
    assert_eq!(write.changes(), UnitChanges::new(true, false));
    // A unit that changed the schema reports it.
    let mut tg = TraversalGraph::new();
    let mark = tg.begin_unit().unwrap();
    tg.set_schema(GraphSchema::new().with_mode(SchemaMode::Open))
        .unwrap();
    assert_eq!(
        tg.unit_changes(mark).unwrap(),
        UnitChanges::new(false, true)
    );
    tg.add_vertex(None, vec!["a".into()], PropertyMap::default())
        .unwrap();
    assert_eq!(tg.unit_changes(mark).unwrap(), UnitChanges::new(true, true));
    tg.commit_unit(mark).unwrap();
}
