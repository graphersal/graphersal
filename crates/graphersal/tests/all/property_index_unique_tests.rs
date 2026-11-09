//! Unique property indexes (book page "Property Indexes", section "Unique indexes"; design
//! section 5, D4/D9/D11/D16 of the property index extensions): a unique index refuses every
//! write that would give a second element of the index a key it holds, at once, on every write
//! path (steps, folded creates, imports, applied change sets, merges, GQL, label changes); the
//! unit rolls back and leaves nothing (checked with the data and `check_property_indexes`).
//! `null` and values the index does not hold are exempt. A create over repeated values is
//! refused with a report; an index stored over them (a bypass, a load) is neither served nor
//! enforced.
#![cfg(feature = "script")]

use std::collections::HashMap;
use std::sync::Arc;

use graphersal::catalog::{
    Definition, DefinitionKind, PropertyIndexDefinition, PropertyIndexTransform,
};
use graphersal::error::{DefinitionError, GraphError};
use graphersal::prelude::*;
use graphersal::script::rhai::Dynamic;
use graphersal::storage::{ElementKind, Graph, GraphStorage, PropertyIndexRange};

use crate::script_host::{ScriptLimits, eval_value_with_limits};

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

fn run(graph: &Arc<Graph>, script: &str) -> Result<Dynamic, String> {
    eval_value_with_limits(
        graph.clone(),
        script,
        HashMap::new(),
        &ScriptLimits::default(),
    )
    .map_err(|err| err.to_string())
}

fn ok(graph: &Arc<Graph>, script: &str) -> Dynamic {
    run(graph, script).unwrap_or_else(|err| panic!("{script}: {err}"))
}

fn exec(graph: &Arc<Graph>, script: &str) {
    let _ = ok(graph, script);
}

/// Runs `script`, which must fail with a unique violation of `index`; answers the message.
fn violates(graph: &Arc<Graph>, script: &str, index: &str) -> String {
    let err = run(graph, script).expect_err(script);
    assert!(
        err.contains(&format!("The unique index '{index}' already holds")),
        "{script}: {err}"
    );
    check(graph);
    err
}

fn check(graph: &Graph) {
    graph.read().check_property_indexes().unwrap();
}

fn count(graph: &Arc<Graph>, query: &str) -> i64 {
    ok(graph, &format!("g.{query}.count().next()"))
        .as_int()
        .unwrap_or_else(|_| panic!("{query}: not a count"))
}

/// Users a@x, b@x and c@x (vertex ids 1, 2, 3), a unique index on User.email.
fn users() -> Arc<Graph> {
    let graph = Arc::new(Graph::new(TraversalGraph::new()));
    exec(
        &graph,
        r#"g.add_v("User").property(T.id, "1").property("email", "a@x").iterate();
           g.add_v("User").property(T.id, "2").property("email", "b@x").iterate();
           g.add_v("User").property(T.id, "3").property("email", "c@x").iterate();
           g.create_index(#{label: "User", key: "email", unique: true})"#,
    );
    check(&graph);
    graph
}

fn emails(graph: &Arc<Graph>) -> Vec<String> {
    let mut all: Vec<String> = ok(
        graph,
        r#"g.v().has_label("User").values("email").to_list()"#,
    )
    .into_array()
    .unwrap()
    .into_iter()
    .map(|value| value.to_string())
    .collect();
    all.sort();
    all
}

fn abc() -> Vec<String> {
    vec!["a@x".to_string(), "b@x".to_string(), "c@x".to_string()]
}

// ---------------------------------------------------------------------------------------------
// The definition
// ---------------------------------------------------------------------------------------------

#[test]
fn a_unique_index_is_listed_stored_as_the_extended_kind_and_read_like_any_index() {
    let graph = users();
    let rows = ok(&graph, "g.indexes()").into_array().unwrap();
    let row = rows[0]
        .clone()
        .try_cast::<graphersal::script::rhai::Map>()
        .unwrap();
    assert_eq!(row["unique"].as_bool(), Ok(true));
    assert_eq!(row["served"].as_bool(), Ok(true));
    let stored = graph
        .read()
        .definition(DefinitionKind::PROPERTY_INDEX_EXT, "User_email")
        .unwrap()
        .cloned()
        .expect("kind 6");
    assert!(stored.flags().is_critical());
    let index = stored.as_property_index().unwrap();
    assert!(index.is_unique());
    assert_eq!(
        index.to_payload().get("unique"),
        Some(&ElementProperty::Boolean(true))
    );
    // Not unique: no payload key.
    let plain = PropertyIndexDefinition::new(ElementKind::Vertex, "User", "email");
    assert!(plain.to_payload().get("unique").is_none());
    assert_eq!(
        PropertyIndexDefinition::from_payload(&index.to_payload()).unwrap(),
        index.clone()
    );
    // A lookup reads it; a point lookup holds one element.
    let profile = ok(
        &graph,
        r#"g.v().has_label("User").has("email", "b@x").profile().to_string()"#,
    )
    .to_string();
    assert!(
        profile.contains("[index: vertex User.email, exact]"),
        "{profile}"
    );
    assert_eq!(
        count(&graph, r#"v().has_label("User").has("email", "b@x")"#),
        1
    );
    let read = graph.read();
    let found: Vec<_> = read
        .vertices_by_property_index(
            Some("User"),
            "email",
            &PropertyIndexRange::equal("b@x".into()),
        )
        .unwrap()
        .unwrap()
        .collect();
    assert_eq!(found.len(), 1);
    let info = read.property_indexes().unwrap();
    assert!(info[0].unique);
}

#[test]
fn a_payload_unique_of_the_wrong_type_is_malformed_and_kind_2_refuses_it() {
    let mut payload = PropertyIndexDefinition::new(ElementKind::Vertex, "User", "email")
        .with_unique(true)
        .to_payload();
    payload.insert("unique", "yes".into());
    assert!(matches!(
        PropertyIndexDefinition::from_payload(&payload),
        Err(DefinitionError::Malformed { .. })
    ));
    // A unique index is never kind 2.
    let index =
        PropertyIndexDefinition::new(ElementKind::Vertex, "User", "email").with_unique(true);
    assert_eq!(
        Definition::property_index("u", index).kind(),
        DefinitionKind::PROPERTY_INDEX_EXT
    );
}

#[test]
fn unique_with_each_is_refused_at_create_and_never_served_when_stored() {
    let graph = Arc::new(Graph::new(TraversalGraph::new()));
    let err = run(
        &graph,
        r#"g.create_index(#{key: "tags", transform: "each", unique: true})"#,
    )
    .unwrap_err();
    assert!(err.contains("Invalid unique of 'tags_each'"), "{err}");
    assert!(err.contains("cannot use the transform each"), "{err}");
    assert!(graph.read().definitions().unwrap().next().is_none());
    // Stored without the create check: kept, not served, with its reason.
    let index = PropertyIndexDefinition::label_less(ElementKind::Vertex, "tags")
        .with_transform(Some(PropertyIndexTransform::Each))
        .with_unique(true);
    assert!(index.unserved_reason().unwrap().contains("each"));
    graph
        .write()
        .set_definition(Definition::property_index("tags_each", index))
        .unwrap();
    assert!(graph.read().property_indexes().unwrap().is_empty());
    let rows = ok(&graph, "g.indexes()").into_array().unwrap();
    let row = rows[0]
        .clone()
        .try_cast::<graphersal::script::rhai::Map>()
        .unwrap();
    assert_eq!(row["served"].as_bool(), Ok(false));
    assert!(row["reason"].to_string().contains("transform each"));
    check(&graph);
}

// ---------------------------------------------------------------------------------------------
// Write paths
// ---------------------------------------------------------------------------------------------

#[test]
fn add_v_with_a_repeated_key_fails_and_the_query_leaves_nothing() {
    let graph = users();
    violates(
        &graph,
        r#"g.add_v("User").property("email", "a@x").iterate()"#,
        "User_email",
    );
    // An earlier write of the same query rolls back with it.
    violates(
        &graph,
        r#"g.add_v("User").property("email", "new@x").add_v("User").property("email", "b@x").iterate()"#,
        "User_email",
    );
    assert_eq!(emails(&graph), abc());
    assert_eq!(count(&graph, "v()"), 3);
    // Another label is not in the index.
    exec(
        &graph,
        r#"g.add_v("Admin").property("email", "a@x").iterate()"#,
    );
    check(&graph);
}

#[test]
fn a_property_write_on_an_existing_element_fails_and_keeps_the_old_value() {
    let graph = users();
    let message = violates(
        &graph,
        r#"g.v("2").property("email", "a@x").iterate()"#,
        "User_email",
    );
    assert!(message.contains("for the vertex '1'"), "{message}");
    assert_eq!(emails(&graph), abc());
    // Its own value again is fine; a fresh value is fine.
    exec(&graph, r#"g.v("2").property("email", "b@x").iterate()"#);
    exec(&graph, r#"g.v("2").property("email", "d@x").iterate()"#);
    exec(&graph, r#"g.v("1").property("email", "b@x").iterate()"#);
    check(&graph);
    // A single-segment jpath write is a top-level write.
    violates(
        &graph,
        r#"g.v("3").property(jpath("email"), "d@x").iterate()"#,
        "User_email",
    );
}

#[test]
fn edge_indexes_refuse_add_e_and_edge_property_writes() {
    let graph = users();
    exec(
        &graph,
        r#"g.create_index(#{element: "edge", label: "knows", key: "since", unique: true});
           g.v("1").add_e("knows").to(__.v("2")).property("since", 2001).iterate()"#,
    );
    violates(
        &graph,
        r#"g.v("2").add_e("knows").to(__.v("3")).property("since", 2001).iterate()"#,
        "knows_since_edge",
    );
    // An int and a float of the same value are equal, as has() finds them.
    violates(
        &graph,
        r#"g.v("2").add_e("knows").to(__.v("3")).property("since", 2001.0).iterate()"#,
        "knows_since_edge",
    );
    exec(
        &graph,
        r#"g.v("2").add_e("knows").to(__.v("3")).property("since", 2002).iterate()"#,
    );
    violates(
        &graph,
        r#"g.e().has("since", 2002).property("since", 2001).iterate()"#,
        "knows_since_edge",
    );
    assert_eq!(count(&graph, "e()"), 2);
    // A label-less edge index covers every edge label.
    exec(
        &graph,
        r#"g.create_index(#{element: "edge", key: "code", unique: true});
           g.v("1").add_e("likes").to(__.v("3")).property("code", "k1").iterate()"#,
    );
    violates(
        &graph,
        r#"g.v("3").add_e("hates").to(__.v("1")).property("code", "k1").iterate()"#,
        "code_edge",
    );
    // An edge relabelled into a per-label index.
    exec(
        &graph,
        r#"g.v("3").add_e("other").to(__.v("1")).property("since", 2001).iterate()"#,
    );
    let mut write = graph.write();
    let other = write
        .edges_by_label("other")
        .unwrap()
        .next()
        .expect("the other edge");
    let err = write.set_edge_label(other, "knows".into()).unwrap_err();
    assert!(matches!(err, GraphError::UniqueViolation { .. }), "{err}");
    assert_eq!(write.edge_label(other).unwrap(), Some("other"));
    write.check_property_indexes().unwrap();
}

#[test]
fn the_rust_api_create_and_insert_paths_refuse_and_change_nothing() {
    let mut graph = TraversalGraph::new();
    let mut props = PropertyMap::default();
    props.insert("email", "a@x".into());
    graph
        .add_vertex(Some("1".into()), vec!["User".into()], props.clone())
        .unwrap();
    graph
        .traversal_mut()
        .create_index(
            "user_email",
            PropertyIndexDefinition::new(ElementKind::Vertex, "User", "email").with_unique(true),
        )
        .unwrap();
    // Outside a unit: the write itself changes nothing.
    let err = graph
        .add_vertex(Some("2".into()), vec!["User".into()], props.clone())
        .unwrap_err();
    match &err {
        GraphError::UniqueViolation {
            index,
            element,
            key,
            value,
            existing,
            ..
        } => {
            assert_eq!(index, "user_email");
            assert_eq!(*element, ElementKind::Vertex);
            assert_eq!(key, "email");
            assert_eq!(value, "\"a@x\"");
            assert_eq!(existing, "1");
        }
        other => panic!("{other:?}"),
    }
    let help = graphersal::error::DiagnosticError::help(&err).unwrap();
    assert!(help.contains(r#"g.v("1").value_map()"#), "{help}");
    assert!(help.contains("intermediate value"), "{help}");
    assert!(graph.get_vertex_by_id("2").unwrap().is_none());
    let err = graph
        .insert_vertex(Some("2".into()), vec!["User".into()], props)
        .unwrap_err();
    assert!(matches!(err, GraphError::UniqueViolation { .. }));
    // The auto-id sequence did not move.
    let v = graph
        .add_vertex(None, vec!["User".into()], PropertyMap::default())
        .unwrap();
    assert_eq!(graph.vertex_id(v).unwrap(), Some("2"));
    graph.check_property_indexes().unwrap();
}

#[test]
fn labels_added_into_a_per_label_unique_index_are_checked() {
    let graph = users();
    exec(
        &graph,
        r#"g.add_v("Guest").property(T.id, "9").property("email", "a@x").iterate()"#,
    );
    violates(
        &graph,
        r#"g.v("9").add_label("User").iterate()"#,
        "User_email",
    );
    assert_eq!(count(&graph, r#"v().has_label("User")"#), 3);
    violates(
        &graph,
        r#"g.gql("MATCH (n:Guest) SET n:User").iterate()"#,
        "User_email",
    );
    assert_eq!(count(&graph, r#"v().has_label("User")"#), 3);
    // Removing a label never repeats a key.
    exec(&graph, r#"g.v("1").drop_label("User").iterate()"#);
    exec(&graph, r#"g.v("9").add_label("User").iterate()"#);
    check(&graph);
}

#[test]
fn merge_v_and_merge_e_creates_are_checked_and_matches_are_not() {
    let graph = users();
    // A match changes nothing.
    exec(
        &graph,
        r#"g.merge_v(#{"T.label": "User", "email": "a@x"}).iterate()"#,
    );
    assert_eq!(count(&graph, "v()"), 3);
    violates(
        &graph,
        r#"g.merge_v(#{"T.label": "User", "name": "zed"}).option(Merge.onCreate, #{"T.label": "User", "name": "zed", "email": "b@x"}).iterate()"#,
        "User_email",
    );
    violates(
        &graph,
        r#"g.merge_v(#{"T.id": "1"}).option(Merge.onMatch, #{"email": "c@x"}).iterate()"#,
        "User_email",
    );
    exec(
        &graph,
        r#"g.create_index(#{element: "edge", label: "knows", key: "tag", unique: true});
           g.merge_e(#{"T.label": "knows", "Direction.OUT": "1", "Direction.IN": "2", "tag": "t"}).iterate()"#,
    );
    violates(
        &graph,
        r#"g.merge_e(#{"T.label": "knows", "Direction.OUT": "2", "Direction.IN": "3"}).option(Merge.onCreate, #{"T.label": "knows", "Direction.OUT": "2", "Direction.IN": "3", "tag": "t"}).iterate()"#,
        "knows_tag_edge",
    );
    assert_eq!(emails(&graph), abc());
    assert_eq!(count(&graph, "e()"), 1);
}

#[test]
fn gql_insert_set_and_merge_are_checked() {
    let graph = users();
    violates(
        &graph,
        r#"g.gql("INSERT (:User {email: 'a@x'})").iterate()"#,
        "User_email",
    );
    violates(
        &graph,
        r#"g.gql("MATCH (n:User {email: 'b@x'}) SET n.email = 'c@x'").iterate()"#,
        "User_email",
    );
    violates(
        &graph,
        r#"g.gql("MERGE (n:User {name: 'q'}) ON CREATE SET n.email = 'a@x' RETURN n").iterate()"#,
        "User_email",
    );
    assert_eq!(emails(&graph), abc());
    assert_eq!(count(&graph, "v()"), 3);
    exec(
        &graph,
        r#"g.gql("INSERT (:User {email: 'd@x'})").iterate()"#,
    );
    check(&graph);
}

#[cfg(feature = "io")]
#[test]
fn graphson_and_graphml_imports_fail_as_one_unit() {
    use graphersal::io::{GraphExport, GraphImport};
    let source = TraversalGraph::new();
    let mut source = source;
    for (id, email) in [("10", "new@x"), ("11", "a@x")] {
        let mut props = PropertyMap::default();
        props.insert("email", email.into());
        source
            .insert_vertex(Some(id.into()), vec!["User".into()], props)
            .unwrap();
    }
    let mut graphson = Vec::new();
    source.export_graphson_writer(&mut graphson).unwrap();
    let mut graphml = Vec::new();
    source.export_graphml_writer(&mut graphml).unwrap();
    let graph = users();
    let err = graph
        .write()
        .import_graphson_reader(std::io::Cursor::new(graphson.clone()))
        .unwrap_err();
    assert!(
        err.to_string().contains("unique index 'User_email'"),
        "{err}"
    );
    assert_eq!(count(&graph, "v()"), 3, "the import left nothing");
    check(&graph);
    let err = graph
        .write()
        .import_graphml_reader(std::io::Cursor::new(graphml.clone()))
        .unwrap_err();
    assert!(
        err.to_string().contains("unique index 'User_email'"),
        "{err}"
    );
    assert_eq!(count(&graph, "v()"), 3, "the import left nothing");
    check(&graph);
    // Through the dyn boundary (the session's live import) as well.
    let any: Arc<dyn graphersal::storage::AnyGraph> = graph.clone();
    assert!(any.import_graphson(&mut graphson.as_slice()).is_err());
    assert!(any.import_graphml(&mut graphml.as_slice()).is_err());
    assert_eq!(emails(&graph), abc());
    check(&graph);
}

#[test]
fn an_applied_change_set_that_repeats_a_key_fails_and_leaves_nothing() {
    use graphersal::changes::{ChangeSet, ElementRef, Mutation};
    let graph = users();
    let mut props = PropertyMap::default();
    props.insert("email", "new@x".into());
    let mut repeat = PropertyMap::default();
    repeat.insert("email", "b@x".into());
    let mut builder = ChangeSet::builder(0, 0);
    builder
        .push(Mutation::add_vertex("20", ["User"], props))
        .push(Mutation::set_property(
            ElementRef::vertex("1"),
            "email",
            Some("a@x".into()),
            "z@x".into(),
        ))
        .push(Mutation::add_vertex("21", ["User"], repeat));
    let changes = builder.build().unwrap();
    let err = graph.write().apply_change_set(&changes).unwrap_err();
    assert!(matches!(err, GraphError::UniqueViolation { .. }), "{err}");
    assert_eq!(emails(&graph), abc());
    assert_eq!(count(&graph, "v()"), 3);
    check(&graph);
}

// ---------------------------------------------------------------------------------------------
// Semantics
// ---------------------------------------------------------------------------------------------

#[test]
fn a_swap_needs_an_intermediate_value() {
    let graph = users();
    violates(
        &graph,
        r#"g.v("1").property("email", "b@x").v("2").property("email", "a@x").iterate()"#,
        "User_email",
    );
    assert_eq!(emails(&graph), abc());
    exec(
        &graph,
        r#"g.v("1").property("email", "tmp@x").v("2").property("email", "a@x").v("1").property("email", "b@x").iterate()"#,
    );
    assert_eq!(
        ok(&graph, r#"g.v("1").values("email").next()"#).to_string(),
        "b@x"
    );
    assert_eq!(emails(&graph), abc());
    check(&graph);
}

#[test]
fn null_and_skipped_values_are_not_part_of_uniqueness() {
    let graph = users();
    exec(
        &graph,
        r#"g.add_v("User").property("email", ()).iterate();
           g.add_v("User").property("email", ()).iterate();
           g.add_v("User").property("email", ["a@x"]).iterate();
           g.add_v("User").property("email", ["a@x"]).iterate();
           g.add_v("User").iterate()"#,
    );
    check(&graph);
    let info = graph.read().property_indexes().unwrap();
    assert_eq!(info[0].stats.skipped, 2, "the two arrays");
    // to_lower: case-insensitive; non-strings are skipped.
    exec(
        &graph,
        r#"g.create_index(#{key: "login", unique: true, transform: "to_lower"});
           g.add_v("User").property("login", "Alice").iterate();
           g.add_v("Other").property("login", 7).iterate();
           g.add_v("Other").property("login", 7).iterate()"#,
    );
    violates(
        &graph,
        r#"g.add_v("Admin").property("login", "ALICE").iterate()"#,
        "login_to_lower",
    );
    check(&graph);
}

#[test]
fn a_label_less_unique_index_covers_the_whole_element_kind() {
    let graph = users();
    exec(
        &graph,
        r#"g.create_index(#{key: "email", unique: true, name: "any_email"})"#,
    );
    violates(
        &graph,
        r#"g.add_v("Admin").property("email", "c@x").iterate()"#,
        "any_email",
    );
    violates(
        &graph,
        r#"g.add_v().property("email", "c@x").iterate()"#,
        "any_email",
    );
    exec(
        &graph,
        r#"g.add_v("Admin").property("email", "d@x").iterate()"#,
    );
    check(&graph);
}

#[test]
fn a_dropped_unique_index_allows_repeats_again() {
    let graph = users();
    exec(&graph, r#"g.drop_index("User_email")"#);
    exec(
        &graph,
        r#"g.add_v("User").property("email", "a@x").iterate()"#,
    );
    assert_eq!(count(&graph, r#"v().has("email", "a@x")"#), 2);
    check(&graph);
}

// ---------------------------------------------------------------------------------------------
// Create over repeated values, and stored over them
// ---------------------------------------------------------------------------------------------

#[test]
fn a_create_over_repeated_values_is_refused_with_a_report() {
    let graph = Arc::new(Graph::new(TraversalGraph::new()));
    // 12 repeated values: v0 three times (ids 0-0, 0-1, 0-2, 0-3), the others twice.
    let mut script = String::new();
    for value in 0..12 {
        let copies = if value == 0 { 4 } else { 2 };
        for copy in 0..copies {
            script.push_str(&format!(
                r#"g.add_v("User").property(T.id, "{value}-{copy}").property("email", "v{value}@X").iterate();"#
            ));
        }
    }
    script.push_str(r#"g.add_v("User").property("email", "single@x").iterate();"#);
    exec(&graph, &script);
    let err = run(
        &graph,
        r#"g.create_index(#{label: "User", key: "email", unique: true})"#,
    )
    .unwrap_err();
    assert!(
        err.contains("Cannot create the unique index 'User_email': 12 value(s) of 'email'"),
        "{err}"
    );
    assert!(
        err.contains(r#""v0@X" (4 elements: 0-0, 0-1, 0-2, ...)"#),
        "{err}"
    );
    assert!(err.contains("and 2 more"), "{err}");
    assert!(
        err.contains(
            r#"g.v().has_label("User").has("email").group_count().by("email").unfold().where(__.select(Column.values).is(P.gt(1)))"#
        ),
        "{err}"
    );
    // Nothing was created.
    assert!(graph.read().definitions().unwrap().next().is_none());
    // The suggested query lists them.
    let listed = ok(
        &graph,
        r#"g.v().has_label("User").has("email").group_count().by("email").unfold().where(__.select(Column.values).is(P.gt(1))).count().next()"#,
    );
    assert_eq!(listed.as_int(), Ok(12));
    // to_lower: compared lowered.
    exec(
        &graph,
        r#"g.add_v("Login").property("name", "Bob").iterate();
           g.add_v("Login").property("name", "bob").iterate()"#,
    );
    let err = run(
        &graph,
        r#"g.create_index(#{label: "Login", key: "name", unique: true, transform: "to_lower"})"#,
    )
    .unwrap_err();
    assert!(err.contains(r#""bob" (2 elements"#), "{err}");
    assert!(
        err.contains(r#".group_count().by(__.values("name").to_lower())"#),
        "{err}"
    );
    let listed = ok(
        &graph,
        r#"g.v().has_label("Login").has("name").group_count().by(__.values("name").to_lower()).unfold().where(__.select(Column.values).is(P.gt(1))).count().next()"#,
    );
    assert_eq!(listed.as_int(), Ok(1));
    // Through the Rust source, the error and its fields.
    let err = graph
        .write()
        .traversal_mut()
        .create_index(
            "u",
            PropertyIndexDefinition::new(ElementKind::Vertex, "User", "email").with_unique(true),
        )
        .unwrap_err();
    match err.root_cause() {
        TraverserError::Graph(GraphError::Definition(DefinitionError::UniqueIndexDuplicates {
            name,
            duplicates,
            ..
        })) => {
            assert_eq!(name, "u");
            assert_eq!(duplicates.total, 12);
            assert_eq!(duplicates.values.len(), 10);
            assert_eq!(duplicates.values[0].examples.len(), 3);
            assert_eq!(duplicates.values[0].count, 4);
        }
        other => panic!("{other:?}"),
    }
    check(&graph);
}

#[test]
fn a_unique_index_stored_over_repeated_values_is_not_served_nor_enforced() {
    let graph = users();
    exec(
        &graph,
        r#"g.drop_index("User_email"); g.add_v("User").property("email", "a@x").iterate()"#,
    );
    graph
        .write()
        .set_definition(Definition::property_index(
            "User_email",
            PropertyIndexDefinition::new(ElementKind::Vertex, "User", "email").with_unique(true),
        ))
        .expect("a store never checks the data");
    assert!(graph.read().property_indexes().unwrap().is_empty());
    let report = graph
        .read()
        .unique_index_duplicates("User_email")
        .unwrap()
        .unwrap();
    assert_eq!(report.total, 1);
    let rows = ok(&graph, "g.indexes()").into_array().unwrap();
    let row = rows[0]
        .clone()
        .try_cast::<graphersal::script::rhai::Map>()
        .unwrap();
    assert_eq!(row["served"].as_bool(), Ok(false));
    let reason = row["reason"].to_string();
    assert!(reason.starts_with("duplicates:"), "{reason}");
    assert!(
        reason.contains("drop the index and create it again"),
        "{reason}"
    );
    // Not enforced: more repeats pass; queries scan.
    exec(
        &graph,
        r#"g.add_v("User").property("email", "a@x").iterate()"#,
    );
    assert_eq!(
        count(&graph, r#"v().has_label("User").has("email", "a@x")"#),
        3
    );
    check(&graph);
    // A rollback of the store brings nothing back.
    graph
        .write()
        .remove_definition(DefinitionKind::PROPERTY_INDEX_EXT, "User_email")
        .unwrap();
    assert!(
        graph
            .read()
            .unique_index_duplicates("User_email")
            .unwrap()
            .is_none()
    );
    check(&graph);
}

#[cfg(feature = "persist")]
mod store {
    use super::*;
    use graphersal::persist::{MemDir, Store, StoreOptions};
    use graphersal::system_journal::JournalQuery;

    fn options() -> StoreOptions {
        StoreOptions::new().with_checkpoint_after_bytes(0)
    }

    #[test]
    fn a_store_replays_committed_writes_under_the_constraint() {
        let dir = MemDir::new();
        {
            let store = Store::create(&dir, options()).unwrap();
            let graph = store.graph();
            exec(
                graph,
                r#"g.add_v("User").property(T.id, "1").property("email", "a@x").iterate();
                   g.add_v("User").property(T.id, "2").property("email", "b@x").iterate();
                   g.create_index(#{label: "User", key: "email", unique: true})"#,
            );
            store.checkpoint(None).unwrap();
            // After the checkpoint: a swap through an intermediate value, a refused write.
            exec(
                graph,
                r#"g.v("1").property("email", "t@x").v("2").property("email", "a@x").v("1").property("email", "b@x").iterate()"#,
            );
            assert!(
                run(
                    graph,
                    r#"g.add_v("User").property("email", "a@x").iterate()"#
                )
                .is_err()
            );
            store.close().unwrap();
        }
        let store = Store::open(&dir, options()).unwrap();
        assert!(store.open_report().commits_replayed >= 1);
        let graph = store.graph();
        check(graph);
        assert_eq!(
            ok(graph, r#"g.v("1").values("email").next()"#).to_string(),
            "b@x"
        );
        let info = graph.read().property_indexes().unwrap();
        assert!(info[0].unique);
        violates(
            graph,
            r#"g.add_v("User").property("email", "a@x").iterate()"#,
            "User_email",
        );
    }

    #[test]
    fn a_store_with_repeated_values_under_a_unique_index_opens_and_journals_it() {
        let dir = MemDir::new();
        {
            let store = Store::create(&dir, options()).unwrap();
            let graph = store.graph();
            exec(
                graph,
                r#"g.add_v("User").property(T.id, "1").property("email", "a@x").iterate();
                   g.add_v("User").property(T.id, "2").property("email", "a@x").iterate()"#,
            );
            // The bypass: a store never checks the data.
            graph
                .write()
                .set_definition(Definition::property_index(
                    "User_email",
                    PropertyIndexDefinition::new(ElementKind::Vertex, "User", "email")
                        .with_unique(true),
                ))
                .unwrap();
            store.checkpoint(None).unwrap();
            store.close().unwrap();
        }
        let store = Store::open(&dir, options()).expect("the open never fails for it");
        let graph = store.graph();
        assert!(graph.read().property_indexes().unwrap().is_empty());
        let report = graph
            .read()
            .unique_index_duplicates("User_email")
            .unwrap()
            .unwrap();
        assert_eq!(report.values[0].examples, vec!["1", "2"]);
        check(graph);
        let records = store
            .journal()
            .unwrap()
            .query(&JournalQuery::new())
            .unwrap()
            .records;
        let record = records
            .iter()
            .find(|record| record.kind == "index.unique_duplicates")
            .unwrap_or_else(|| panic!("{records:?}"));
        let field = |name: &str| {
            record
                .fields
                .iter()
                .find(|(key, _)| key == name)
                .and_then(|(_, value)| value.text().map(|text| text.into_owned()))
                .unwrap_or_default()
        };
        assert_eq!(field("name"), "User_email");
        assert!(field("recommendation").contains("g.drop_index(\"User_email\")"));
        // The fix: deduplicate, drop, create again.
        exec(
            graph,
            r#"g.v("2").property("email", "b@x").iterate();
               g.drop_index("User_email");
               g.create_index(#{label: "User", key: "email", unique: true})"#,
        );
        assert!(graph.read().property_indexes().unwrap()[0].unique);
        check(graph);
    }
}
