//! The catalog of definitions in the engine (`book/src/users_guide/saved_queries.md`):
//! validation rules, storage, units (rollback, savepoints, dry run), change capture and
//! its serde, replay and `apply_change_set`, vetoes, authorization, opaque definitions and
//! unknown keys.

use std::sync::{Arc, Mutex};

use graphersal::auth::{AccessPolicy, ActionKind, ResourceKind};
use graphersal::catalog::{
    self, DefinitionFlags, MAX_DEFINITIONS, MAX_DESCRIPTION_BYTES, MAX_FOLDER_BYTES,
    MAX_PARAMETERS, MAX_SOURCE_BYTES, catalog_from_json, catalog_to_json,
};
use graphersal::catalog::{Definition, DefinitionKind, QueryDefinition, QueryParam};
use graphersal::changes::{ChangeSet, Mutation};
use graphersal::prelude::PropertyMap;
use graphersal::prelude::*;
use graphersal::schema::PropertySchema;
use graphersal::{
    auth::{AccessRequest, Authorizer, Denied},
    changes::{CommitHook, HookError, RollbackReason},
    storage::UnitChanges,
};

// ---------------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------------

const SOURCE: &str = "fn top(limit) { g.v().limit(limit) }";

fn obj(entries: &[(&str, ElementProperty)]) -> ElementProperty {
    let mut map = PropertyMap::default();
    for (key, value) in entries {
        map.insert(*key, value.clone());
    }
    ElementProperty::Object(Box::new(map))
}

/// `{"type": "integer", "minimum": 1, "maximum": 1000}`.
fn limit_schema() -> PropertySchema {
    PropertySchema::from_value(&obj(&[
        ("type", "integer".into()),
        ("minimum", 1.into()),
        ("maximum", 1000.into()),
    ]))
    .unwrap()
}

fn top_query() -> QueryDefinition {
    QueryDefinition::new(SOURCE)
        .with_folder("reports/sales")
        .with_description("The first vertices.")
        .with_param("limit", QueryParam::new(limit_schema()).with_default(20))
}

fn top() -> Definition {
    Definition::query("top", top_query())
}

fn opaque(name: &str, payload: &[u8]) -> Definition {
    Definition::opaque(
        DefinitionKind::from_code(42),
        DefinitionFlags::NONE,
        name,
        payload.to_vec(),
    )
}

fn names(graph: &TraversalGraph) -> Vec<(u8, String)> {
    graph
        .definitions()
        .unwrap()
        .map(|d| (d.kind().code(), d.name().to_string()))
        .collect()
}

/// A hook that keeps every accepted change set and vetoes while told to.
#[derive(Clone, Default)]
struct Recorder {
    accepted: Arc<Mutex<Vec<ChangeSet>>>,
    veto: Arc<Mutex<bool>>,
    rollbacks: Arc<Mutex<usize>>,
}

impl CommitHook for Recorder {
    fn name(&self) -> &str {
        "recorder"
    }

    fn before_commit(&mut self, changes: &ChangeSet) -> Result<(), HookError> {
        if *self.veto.lock().unwrap() {
            return Err("no catalog changes today".into());
        }
        self.accepted.lock().unwrap().push(changes.clone());
        Ok(())
    }

    fn after_rollback(&mut self, _reason: &RollbackReason) {
        *self.rollbacks.lock().unwrap() += 1;
    }
}

fn recorded(graph: &mut TraversalGraph) -> Recorder {
    let recorder = Recorder::default();
    graph
        .register_commit_hook(Box::new(recorder.clone()))
        .unwrap();
    recorder
}

fn definition_error(err: &TraverserError) -> &DefinitionError {
    match err.root_cause() {
        TraverserError::Graph(GraphError::Definition(err)) => err,
        other => panic!("not a definition error: {other:?}"),
    }
}

fn refused(definition: Definition) -> DefinitionError {
    let mut graph = TraversalGraph::new();
    let err = graph
        .traversal_mut()
        .set_definition(definition)
        .unwrap_err();
    assert_eq!(
        graph.definitions().unwrap().count(),
        0,
        "a refused definition is not stored"
    );
    definition_error(&err).clone()
}

// ---------------------------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------------------------

#[test]
fn query_names_follow_the_identifier_rules() {
    for bad in [
        "",
        "1top",
        "_1",
        "top-customers",
        "top customers",
        "fn",
        "let",
        "print",
    ] {
        let err = refused(Definition::query(bad, top_query()));
        assert!(
            matches!(err, DefinitionError::InvalidName { .. }),
            "{bad:?}: {err:?}"
        );
    }
    let long = "a".repeat(catalog::MAX_QUERY_NAME_BYTES + 1);
    assert!(matches!(
        refused(Definition::query(long, top_query())),
        DefinitionError::InvalidName { .. }
    ));
    let longest = "a".repeat(catalog::MAX_QUERY_NAME_BYTES);
    Definition::query(longest, top_query()).validate().unwrap();
    for good in ["top", "_top", "Top_2", "x"] {
        Definition::query(good, top_query()).validate().unwrap();
    }
}

#[test]
fn folders_follow_the_path_rules() {
    for bad in [
        "/reports",
        "reports/",
        "reports//sales",
        " reports",
        "reports/sales ",
        "a/b/c/d/e/f/g/h/i",
    ] {
        let err = refused(Definition::query("top", top_query().with_folder(bad)));
        assert!(
            matches!(err, DefinitionError::InvalidFolder { .. }),
            "{bad:?}: {err:?}"
        );
    }
    let long = "f".repeat(MAX_FOLDER_BYTES + 1);
    assert!(matches!(
        refused(Definition::query("top", top_query().with_folder(long))),
        DefinitionError::InvalidFolder { .. }
    ));
    Definition::query("top", top_query().with_folder("a/b/c/d/e/f/g/h"))
        .validate()
        .unwrap();
    // An empty folder is the root.
    let root = top_query().with_folder("");
    assert_eq!(root.folder(), None);
}

#[test]
fn sizes_are_bounded() {
    let field = |definition: Definition| match refused(definition) {
        DefinitionError::InvalidField { field, .. } => field,
        other => panic!("{other:?}"),
    };
    let description = "d".repeat(MAX_DESCRIPTION_BYTES + 1);
    assert_eq!(
        field(Definition::query(
            "top",
            top_query().with_description(description)
        )),
        "description"
    );
    let source = format!("fn top() {{ {} }}", " ".repeat(MAX_SOURCE_BYTES));
    assert_eq!(
        field(Definition::query("top", QueryDefinition::new(source))),
        "source"
    );
    assert_eq!(
        field(Definition::query("top", QueryDefinition::new("  "))),
        "source"
    );
    assert_eq!(
        field(Definition::query("top", top_query().with_dialect(""))),
        "dialect"
    );
    let meta = "m".repeat(catalog::MAX_META_BYTES + 1);
    assert_eq!(
        field(Definition::query("top", top_query().with_meta(meta))),
        "meta"
    );
    // Opaque definitions: a name is needed.
    assert!(matches!(
        refused(opaque("", b"x")),
        DefinitionError::InvalidName { .. }
    ));
}

#[test]
fn parameter_names_are_identifiers_that_shadow_no_global() {
    for bad in ["g", "P", "T", "__", "params", "keys", "fn", "1x", "a-b"] {
        let query = QueryDefinition::new(SOURCE).with_param(bad, QueryParam::new(limit_schema()));
        let err = refused(Definition::query("top", query));
        let DefinitionError::InvalidParameter { parameter, .. } = &err else {
            panic!("{bad:?}: {err:?}");
        };
        assert_eq!(parameter, bad);
    }
    // The bare Pop constants are not reserved.
    let query = QueryDefinition::new(SOURCE).with_param("first", QueryParam::new(limit_schema()));
    Definition::query("top", query).validate().unwrap();
}

#[test]
fn a_query_has_a_bounded_number_of_parameters() {
    let mut query = QueryDefinition::new(SOURCE);
    for index in 0..=MAX_PARAMETERS {
        query = query.with_param(format!("p{index}"), QueryParam::new(limit_schema()));
    }
    assert!(matches!(
        refused(Definition::query("top", query)),
        DefinitionError::InvalidParameter { .. }
    ));
}

#[test]
fn defaults_must_satisfy_their_schema() {
    let with_default = |default: ElementProperty| {
        Definition::query(
            "top",
            QueryDefinition::new(SOURCE).with_param(
                "limit",
                QueryParam::new(limit_schema()).with_default(default),
            ),
        )
    };
    for bad in [
        ElementProperty::from("twenty"),
        ElementProperty::from(5000),
        ElementProperty::from(0),
        ElementProperty::Null,
    ] {
        let err = refused(with_default(bad.clone()));
        let DefinitionError::InvalidParameter { reason, .. } = &err else {
            panic!("{bad:?}: {err:?}");
        };
        assert!(reason.contains("default"), "{reason}");
    }
    let err = refused(with_default(5000.into()));
    assert!(err.to_string().contains("top.limit"), "{err}");
    with_default(1000.into()).validate().unwrap();
}

#[test]
fn defaults_are_coerced_like_a_schema_write() {
    let id = "8f3e1d2c-0b4a-4c5d-9e6f-7a8b9c0d1e2f";
    let param = QueryParam::new(PropertySchema::uuid()).with_default(id);
    assert!(matches!(
        param.default_value(),
        Some(ElementProperty::Uuid(_))
    ));
    let param = QueryParam::new(PropertySchema::number()).with_default(3);
    assert_eq!(param.default_value(), Some(&ElementProperty::from(3.0)));
    // check_value coerces arguments the same way and names the place.
    assert_eq!(
        param.check_value("q", "x", 2.into()).unwrap(),
        ElementProperty::from(2.0)
    );
    let err = QueryParam::new(limit_schema())
        .check_value("top", "limit", 5000.into())
        .unwrap_err();
    assert!(err.to_string().contains("top.limit"), "{err}");
}

#[test]
fn parameter_declarations_are_read_strictly() {
    let param = QueryParam::from_value("limit", &"integer".into()).unwrap();
    assert!(param.is_required());
    assert_eq!(param.schema(), &PropertySchema::integer());
    let uuid = QueryParam::from_value("id", &"uuid".into()).unwrap();
    assert_eq!(uuid.schema(), &PropertySchema::uuid());
    let long = QueryParam::from_value(
        "limit",
        &obj(&[
            ("schema", obj(&[("type", "integer".into())])),
            ("default", 20.into()),
        ]),
    )
    .unwrap();
    assert_eq!(long.default_value(), Some(&ElementProperty::from(20)));
    for bad in [
        ElementProperty::from("integr"),
        obj(&[("default", 1.into())]),
        obj(&[("schema", obj(&[("type", "integr".into())]))]),
        obj(&[
            ("schema", obj(&[("type", "integer".into())])),
            ("defualt", 1.into()),
        ]),
        ElementProperty::from(3),
    ] {
        let err = QueryParam::from_value("limit", &bad).unwrap_err();
        assert!(
            matches!(err, DefinitionError::InvalidParameter { .. }),
            "{err:?}"
        );
    }
}

#[test]
fn the_catalog_is_bounded() {
    let mut graph = TraversalGraph::new();
    for index in 0..MAX_DEFINITIONS {
        graph
            .set_definition(Definition::query(
                format!("q{index}"),
                QueryDefinition::new("fn x() { 1 }"),
            ))
            .unwrap();
    }
    let err = graph
        .set_definition(Definition::query(
            "one_more",
            QueryDefinition::new("fn x() { 1 }"),
        ))
        .unwrap_err();
    assert!(matches!(
        err,
        GraphError::Definition(DefinitionError::TooManyDefinitions { .. })
    ));
    assert!(err.help().unwrap().contains("Remove definitions"));
    // Replacing one is still possible.
    graph
        .set_definition(Definition::query(
            "q0",
            QueryDefinition::new("fn y() { 2 }"),
        ))
        .unwrap();
    assert_eq!(graph.definitions().unwrap().count(), MAX_DEFINITIONS);
}

// ---------------------------------------------------------------------------------------------
// Storage and the Rust API
// ---------------------------------------------------------------------------------------------

#[test]
fn define_list_get_replace_move_describe_remove() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    graph.traversal_mut().set_definition(top()).unwrap();
    graph
        .traversal_mut()
        .set_definition(opaque("idx", b"\x01\x02"))
        .unwrap();
    graph
        .traversal_mut()
        .define_query("alpha", QueryDefinition::new("fn alpha() { 1 }"))
        .unwrap();
    // By kind, then by name.
    assert_eq!(
        names(&graph),
        [
            (1, "alpha".to_string()),
            (1, "top".to_string()),
            (42, "idx".to_string())
        ]
    );
    {
        let g = graph.traversal();
        assert_eq!(
            g.list_definitions(Some(DefinitionKind::QUERY))
                .unwrap()
                .len(),
            2
        );
        assert_eq!(g.list_definitions(None).unwrap().len(), 3);
        assert_eq!(
            g.get_definition(DefinitionKind::QUERY, "top").unwrap(),
            Some(top())
        );
        assert_eq!(
            g.get_definition(DefinitionKind::QUERY, "Top").unwrap(),
            None
        );
    }

    graph
        .traversal_mut()
        .move_query("top", Some("archive"))
        .unwrap();
    graph
        .traversal_mut()
        .describe_query("top", Some("Moved."))
        .unwrap();
    let stored = graph
        .definition(DefinitionKind::QUERY, "top")
        .unwrap()
        .unwrap();
    let query = stored.as_query().unwrap();
    assert_eq!(query.folder(), Some("archive"));
    assert_eq!(query.description(), Some("Moved."));
    graph.traversal_mut().move_query("top", None).unwrap();
    graph.traversal_mut().describe_query("top", None).unwrap();
    let query = graph
        .definition(DefinitionKind::QUERY, "top")
        .unwrap()
        .unwrap()
        .as_query()
        .unwrap()
        .clone();
    assert_eq!((query.folder(), query.description()), (None, None));

    let err = graph
        .traversal_mut()
        .move_query("missing", Some("x"))
        .unwrap_err();
    assert!(matches!(
        definition_error(&err),
        DefinitionError::NotFound { .. }
    ));
    // An invalid move is refused and changes nothing.
    assert!(graph.traversal_mut().move_query("top", Some("/x")).is_err());
    assert_eq!(
        graph
            .definition(DefinitionKind::QUERY, "top")
            .unwrap()
            .unwrap()
            .as_query()
            .unwrap()
            .folder(),
        None
    );

    assert!(
        graph
            .traversal_mut()
            .remove_definition(DefinitionKind::QUERY, "alpha")
            .unwrap()
    );
    assert!(
        !graph
            .traversal_mut()
            .remove_definition(DefinitionKind::QUERY, "alpha")
            .unwrap()
    );
    assert_eq!(graph.definitions().unwrap().count(), 2);
}

#[test]
fn a_read_only_source_and_a_shared_reference_cannot_define() {
    let mut graph = TraversalGraph::new();
    graph.set_definition(top()).unwrap();
    let mut g = graph.traversal();
    assert!(g.set_definition(top()).is_err());
    assert!(g.remove_definition(DefinitionKind::QUERY, "top").is_err());
    assert!(g.move_query("top", Some("x")).is_err());
    let shared = std::sync::Arc::new(TraversalGraph::new());
    let mut second = shared.clone();
    assert!(
        second.set_definition(top()).is_err(),
        "a shared Arc is read-only"
    );
    assert_eq!(graph.definitions().unwrap().count(), 1);
}

// ---------------------------------------------------------------------------------------------
// Units
// ---------------------------------------------------------------------------------------------

#[test]
fn a_catalog_change_outside_a_unit_is_a_commit_of_its_own() {
    let mut graph = TraversalGraph::new();
    let start = graph.last_commit_seq();
    graph.set_definition(top()).unwrap();
    assert_eq!(graph.last_commit_seq(), start + 1);
    // Storing an equal definition again is not a change.
    graph.set_definition(top()).unwrap();
    assert_eq!(graph.last_commit_seq(), start + 1);
    // Removing a missing one neither.
    assert!(!graph.remove_definition(DefinitionKind::QUERY, "x").unwrap());
    assert_eq!(graph.last_commit_seq(), start + 1);
    assert!(
        graph
            .remove_definition(DefinitionKind::QUERY, "top")
            .unwrap()
    );
    assert_eq!(graph.last_commit_seq(), start + 2);
}

#[test]
fn rollback_and_savepoints_restore_the_catalog() {
    let mut graph = TraversalGraph::new();
    graph.set_definition(top()).unwrap();
    let failed: Result<(), GraphError> = graph.transaction(|g| {
        g.set_definition(Definition::query(
            "other",
            QueryDefinition::new("fn other() { 1 }"),
        ))?;
        g.remove_definition(DefinitionKind::QUERY, "top")?;
        g.set_definition(opaque("idx", b"\x00"))?;
        // An invalid definition fails the transaction.
        g.set_definition(Definition::query("1x", top_query()))
    });
    assert!(failed.is_err());
    assert_eq!(names(&graph), [(1, "top".to_string())]);
    assert_eq!(
        graph.definition(DefinitionKind::QUERY, "top").unwrap(),
        Some(&top())
    );

    // A savepoint: the inner change is undone, the outer one kept.
    let mark = graph.begin_unit().unwrap();
    graph
        .set_definition(Definition::query(
            "kept",
            QueryDefinition::new("fn kept() { 1 }"),
        ))
        .unwrap();
    let inner = graph.begin_unit().unwrap();
    let replaced = Definition::query("top", QueryDefinition::new("fn top() { 2 }"));
    graph.set_definition(replaced).unwrap();
    assert_eq!(
        graph.unit_changes(inner).unwrap(),
        UnitChanges::NONE.with_definitions(true)
    );
    graph
        .rollback_unit(inner, &RollbackReason::failed(&"savepoint"))
        .unwrap();
    assert!(graph.unit_changes(mark).unwrap().definitions);
    assert!(!graph.unit_changes(mark).unwrap().data && !graph.unit_changes(mark).unwrap().schema);
    graph.commit_unit(mark).unwrap();
    assert_eq!(
        graph.definition(DefinitionKind::QUERY, "top").unwrap(),
        Some(&top())
    );
    assert!(
        graph
            .definition(DefinitionKind::QUERY, "kept")
            .unwrap()
            .is_some()
    );
}

#[test]
fn dry_run_reports_and_discards_catalog_changes() {
    let mut graph = TraversalGraph::new();
    graph.set_definition(top()).unwrap();
    let seq = graph.last_commit_seq();
    let ((), changes) = graph
        .dry_run(|g| -> Result<(), GraphError> {
            g.remove_definition(DefinitionKind::QUERY, "top")?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        graph.definition(DefinitionKind::QUERY, "top").unwrap(),
        Some(&top())
    );
    assert_eq!(graph.last_commit_seq(), seq);
    let [
        Mutation::SetDefinition {
            kind,
            name,
            before,
            after,
            ..
        },
    ] = changes.mutations()
    else {
        panic!("{changes:?}");
    };
    assert_eq!((*kind, name.as_str()), (DefinitionKind::QUERY, "top"));
    assert_eq!(before.as_deref(), Some(&top()));
    assert!(after.is_none());
}

#[test]
fn a_data_unit_reports_no_catalog_change() {
    // The catalog is untouched by a data unit that fails, and a data unit reports no catalog
    // change.
    let mut graph = TraversalGraph::tinkerpop_modern();
    graph.set_definition(top()).unwrap();
    let execution = graph.traversal_mut().add_v("x").execute();
    assert!(execution.changes().data);
    assert!(!execution.changes().definitions);
    assert_eq!(names(&graph), [(1, "top".to_string())]);
}

// ---------------------------------------------------------------------------------------------
// Change capture, replay, apply
// ---------------------------------------------------------------------------------------------

#[test]
fn change_sets_carry_before_and_after_in_execution_order() {
    let mut graph = TraversalGraph::new();
    let recorder = recorded(&mut graph);
    graph.set_definition(top()).unwrap();
    let moved = graph
        .definition(DefinitionKind::QUERY, "top")
        .unwrap()
        .cloned()
        .map(|mut d| {
            d.as_query_mut().unwrap().set_folder(Some("archive".into()));
            d
        })
        .unwrap();
    graph
        .transaction(|g| -> Result<(), GraphError> {
            g.set_definition(moved.clone())?;
            let v = g.add_vertex(Some("v".into()), vec!["n".into()], PropertyMap::default())?;
            let _ = v;
            g.remove_definition(DefinitionKind::QUERY, "top")?;
            Ok(())
        })
        .unwrap();
    let sets = recorder.accepted.lock().unwrap().clone();
    assert_eq!(sets.len(), 2);
    let [Mutation::SetDefinition { before, after, .. }] = sets[0].mutations() else {
        panic!("{:?}", sets[0]);
    };
    assert!(before.is_none());
    assert_eq!(after.as_deref(), Some(&top()));
    let ops: Vec<String> = sets[1]
        .mutations()
        .iter()
        .map(|m| match m {
            Mutation::SetDefinition { before, after, .. } => {
                format!("def:{}->{}", before.is_some(), after.is_some())
            }
            Mutation::AddVertex { .. } => "add_vertex".to_string(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(ops, ["def:true->true", "add_vertex", "def:true->false"]);
    let Mutation::SetDefinition { before, after, .. } = &sets[1].mutations()[0] else {
        unreachable!()
    };
    assert_eq!(before.as_deref(), Some(&top()));
    assert_eq!(after.as_deref(), Some(&moved));

    // compact(): the changes of one definition merge; here into a removal of the original.
    let compacted = sets[1].compact();
    let defs: Vec<_> = compacted
        .mutations()
        .iter()
        .filter_map(|m| match m {
            Mutation::SetDefinition { before, after, .. } => Some((before.clone(), after.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(defs, [(Some(Box::new(top())), None)]);
    // Set and removed again in one change set: nothing left.
    let both = graph
        .dry_run(|g| -> Result<(), GraphError> {
            g.set_definition(top())?;
            g.remove_definition(DefinitionKind::QUERY, "top")?;
            Ok(())
        })
        .unwrap()
        .1;
    assert_eq!(both.len(), 2);
    assert!(both.compact().is_empty());
}

#[test]
fn replay_and_apply_reproduce_the_catalog() {
    let mut source = TraversalGraph::new();
    let recorder = recorded(&mut source);
    source.set_definition(top()).unwrap();
    source.set_definition(opaque("idx", b"\x07\x08")).unwrap();
    source
        .set_definition(Definition::query(
            "gone",
            QueryDefinition::new("fn gone() { 1 }"),
        ))
        .unwrap();
    source
        .remove_definition(DefinitionKind::QUERY, "gone")
        .unwrap();
    let sets = recorder.accepted.lock().unwrap().clone();

    let mut replayed = TraversalGraph::new();
    for set in &sets {
        replayed = replayed.replay(set).unwrap();
    }
    assert_eq!(names(&replayed), names(&source));
    assert_eq!(replayed.last_commit_seq(), source.last_commit_seq());

    let mut live = TraversalGraph::new();
    let live_recorder = recorded(&mut live);
    for set in &sets {
        live.apply_change_set(set).unwrap();
    }
    assert_eq!(names(&live), names(&source));
    assert_eq!(live_recorder.accepted.lock().unwrap().len(), sets.len());

    // Removing a definition that is not there is an error, like a missing vertex.
    let err = TraversalGraph::new()
        .apply_change_set(&sets[3])
        .unwrap_err();
    assert!(matches!(
        err,
        GraphError::Definition(DefinitionError::NotFound { .. })
    ));
}

#[test]
fn a_hook_can_veto_a_catalog_change() {
    let mut graph = TraversalGraph::new();
    let recorder = recorded(&mut graph);
    graph.set_definition(top()).unwrap();
    *recorder.veto.lock().unwrap() = true;
    let seq = graph.last_commit_seq();
    let err = graph
        .traversal_mut()
        .remove_definition(DefinitionKind::QUERY, "top")
        .unwrap_err();
    assert!(matches!(
        err.root_cause(),
        TraverserError::Graph(GraphError::CommitRejected { .. })
    ));
    assert_eq!(
        graph.definition(DefinitionKind::QUERY, "top").unwrap(),
        Some(&top())
    );
    assert_eq!(graph.last_commit_seq(), seq);
    assert_eq!(*recorder.rollbacks.lock().unwrap(), 1);
}

#[cfg(feature = "serde")]
#[test]
fn change_sets_with_definitions_round_trip_through_serde() {
    let mut graph = TraversalGraph::new();
    let recorder = recorded(&mut graph);
    let uuid_param = QueryParam::new(PropertySchema::uuid())
        .with_default("8f3e1d2c-0b4a-4c5d-9e6f-7a8b9c0d1e2f");
    let query = top_query().with_param("id", uuid_param).with_meta("free");
    graph
        .transaction(|g| -> Result<(), GraphError> {
            g.set_definition(Definition::query("top", query.clone()))?;
            g.set_definition(
                opaque("idx", b"\xff\x00").with_flags(DefinitionFlags::from_bits(0x81)),
            )?;
            g.remove_definition(DefinitionKind::QUERY, "top")?;
            Ok(())
        })
        .unwrap();
    let set = recorder.accepted.lock().unwrap()[0].clone();
    let text = serde_json::to_string(&set).unwrap();
    assert!(text.contains("\"op\":\"set_definition\""), "{text}");
    assert!(
        text.contains("g:UUID"),
        "a uuid default keeps its type: {text}"
    );
    let back: ChangeSet = serde_json::from_str(&text).unwrap();
    assert_eq!(back, set);
    // It replays.
    let replayed = TraversalGraph::new().replay(&back).unwrap();
    assert_eq!(names(&replayed), [(42, "idx".to_string())]);
    assert_eq!(
        replayed
            .definition(DefinitionKind::from_code(42), "idx")
            .unwrap()
            .unwrap()
            .flags()
            .bits(),
        0x81
    );
}

// ---------------------------------------------------------------------------------------------
// Opaque definitions and unknown keys
// ---------------------------------------------------------------------------------------------

#[test]
fn opaque_definitions_survive_changes_of_others() {
    let mut graph = TraversalGraph::new();
    let payload: Vec<u8> = (0..=255).collect();
    let critical = opaque("index_a", &payload).with_flags(
        DefinitionFlags::NONE
            .with_critical(true)
            .with_critical(true),
    );
    assert!(critical.flags().is_critical());
    graph.set_definition(critical.clone()).unwrap();
    graph.set_definition(top()).unwrap();
    graph
        .set_definition(Definition::query(
            "top",
            QueryDefinition::new("fn top() { 2 }"),
        ))
        .unwrap();
    graph
        .remove_definition(DefinitionKind::QUERY, "top")
        .unwrap();
    let kept = graph
        .definition(DefinitionKind::from_code(42), "index_a")
        .unwrap()
        .unwrap();
    assert_eq!(kept, &critical);
    assert_eq!(kept.opaque_payload(), Some(payload.as_slice()));
    // Through the exchange form, byte for byte.
    let back = Definition::from_json(&kept.to_json()).unwrap();
    assert_eq!(back.opaque_payload(), Some(payload.as_slice()));
    assert_eq!(back, critical);
    // An opaque saved query cannot be moved.
    let mut graph = TraversalGraph::new();
    graph
        .set_definition(Definition::opaque(
            DefinitionKind::QUERY,
            DefinitionFlags::NONE,
            "undecodable",
            vec![1, 2, 3],
        ))
        .unwrap();
    let err = graph
        .traversal_mut()
        .move_query("undecodable", Some("x"))
        .unwrap_err();
    assert!(matches!(
        definition_error(&err),
        DefinitionError::InvalidField { .. }
    ));
}

#[test]
fn unknown_keys_are_kept_through_payload_and_json() {
    let mut payload = top_query().to_payload();
    let keys: Vec<&str> = payload.keys().map(|k| k.as_str()).collect();
    assert_eq!(
        keys,
        ["folder", "description", "dialect", "source", "params"]
    );
    assert!(matches!(
        payload.get("params"),
        Some(ElementProperty::String(_))
    ));
    payload.insert("tags", ElementProperty::from(vec!["a", "b"]));
    payload.insert(
        "writes",
        ElementProperty::Boolean(true), // a key of a future kind; kept, not interpreted
    );
    let query = QueryDefinition::from_payload(&payload).unwrap();
    assert_eq!(query.extra().len(), 2);
    assert_eq!(query.to_payload(), payload);
    let definition = Definition::query("top", query);
    definition.validate().unwrap();
    let back = Definition::from_json(&definition.to_json()).unwrap();
    assert_eq!(back, definition);

    // Unknown keys inside a stored parameter object are kept as well.
    let params = r#"{"limit":{"schema":{"type":"integer"},"default":3,"widget":"slider"}}"#;
    let mut payload = PropertyMap::default();
    payload.insert("source", SOURCE.into());
    payload.insert("params", params.into());
    let query = QueryDefinition::from_payload(&payload).unwrap();
    assert_eq!(query.param("limit").unwrap().extra().len(), 1);
    assert_eq!(query.params_to_json(), params);

    // A known key of the wrong type is an error.
    payload.insert("folder", 3.into());
    assert!(matches!(
        QueryDefinition::from_payload(&payload),
        Err(DefinitionError::Malformed { .. })
    ));
}

#[test]
fn the_exchange_form_of_a_catalog() {
    let definitions = vec![top(), opaque("idx", b"\x01")];
    let text = catalog_to_json(&definitions);
    assert!(
        text.starts_with("{\"definitions\":[{\"kind\":\"query\",\"name\":\"top\""),
        "{text}"
    );
    assert_eq!(catalog_from_json(&text).unwrap(), definitions);
    let twice = catalog_to_json(&[top(), top()]);
    assert!(matches!(
        catalog_from_json(&twice),
        Err(DefinitionError::Malformed { .. })
    ));
    for bad in [
        "[]",
        "{}",
        "{\"definitions\": {}}",
        "{\"definitions\": [{\"kind\": \"query\"}]}",
        "{\"definitions\": [{\"kind\": \"tree\", \"name\": \"x\"}]}",
        "{\"definitions\": [{\"kind\": 7, \"name\": \"x\"}]}",
        "{\"definitions\": [{\"kind\": 7, \"name\": \"x\", \"payload\": \"zz\"}]}",
        "{\"definitions\": [{\"kind\": 300, \"name\": \"x\", \"payload\": \"\"}]}",
        "not json",
    ] {
        let err = catalog_from_json(bad).unwrap_err();
        assert!(
            matches!(err, DefinitionError::Malformed { .. }),
            "{bad}: {err:?}"
        );
    }
    // A definition JSON with the numeric kind of a query reads as a query.
    let numeric = top()
        .to_json()
        .replacen("\"kind\":\"query\"", "\"kind\":1", 1);
    assert_eq!(Definition::from_json(&numeric).unwrap(), top());
}

// ---------------------------------------------------------------------------------------------
// Authorization
// ---------------------------------------------------------------------------------------------

/// Records every request and denies the ones `deny` matches.
struct Recording {
    seen: Mutex<Vec<String>>,
    deny: fn(&AccessRequest<'_>) -> bool,
}

impl Authorizer for Recording {
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Denied> {
        self.seen.lock().unwrap().push(request.to_string());
        if (self.deny)(request) {
            Err(Denied::new("not here"))
        } else {
            Ok(())
        }
    }
}

#[test]
fn catalog_operations_ask_the_authorizer() {
    let mut graph = TraversalGraph::new();
    let recording = Arc::new(Recording {
        seen: Mutex::new(Vec::new()),
        deny: |_| false,
    });
    let auth = || recording.clone() as Arc<dyn Authorizer>;
    graph
        .traversal_mut()
        .with_authorizer(auth())
        .set_definition(top())
        .unwrap();
    graph
        .traversal_mut()
        .with_authorizer(auth())
        .set_definition(top())
        .unwrap();
    graph
        .traversal_mut()
        .with_authorizer(auth())
        .move_query("top", Some("x"))
        .unwrap();
    graph
        .traversal()
        .with_authorizer(auth())
        .list_definitions(None)
        .unwrap();
    graph
        .traversal()
        .with_authorizer(auth())
        .get_definition(DefinitionKind::QUERY, "top")
        .unwrap();
    graph
        .traversal_mut()
        .with_authorizer(auth())
        .remove_definition(DefinitionKind::QUERY, "top")
        .unwrap();
    assert_eq!(
        *recording.seen.lock().unwrap(),
        [
            "Create Definition(query \"top\")",
            "Update Definition(query \"top\")",
            "Update Definition(query \"top\")",
            "Read Definition",
            "Read Definition(query \"top\")",
            "Delete Definition(query \"top\")",
        ]
    );
}

#[test]
fn a_read_only_policy_lists_but_does_not_manage() {
    let mut graph = TraversalGraph::new();
    graph.set_definition(top()).unwrap();
    let read_only = || Arc::new(AccessPolicy::read_only()) as Arc<dyn Authorizer>;
    assert_eq!(
        graph
            .traversal()
            .with_authorizer(read_only())
            .list_definitions(None)
            .unwrap()
            .len(),
        1
    );
    let policy = AccessPolicy::read_only();
    assert!(policy.allows(ActionKind::Execute, ResourceKind::Definition));
    assert!(!policy.allows(ActionKind::Create, ResourceKind::Definition));
    for operation in 0..3 {
        let mut g = graph.traversal_mut().with_authorizer(read_only());
        let result = match operation {
            0 => g.set_definition(Definition::query(
                "new",
                QueryDefinition::new("fn new() { 1 }"),
            )),
            1 => g.describe_query("top", Some("x")),
            _ => g
                .remove_definition(DefinitionKind::QUERY, "top")
                .map(|_| ()),
        };
        let err = result.unwrap_err();
        let TraverserError::PermissionDenied { request, .. } = err.root_cause() else {
            panic!("{err:?}");
        };
        assert_eq!(
            request.as_request().resource.kind(),
            ResourceKind::Definition
        );
        let help = err.help().unwrap_or_default();
        assert!(help.contains("catalog of definitions"), "{help}");
    }
    assert_eq!(names(&graph), [(1, "top".to_string())]);
    // A policy without the resource denies even listing.
    let none = Arc::new(AccessPolicy::read_only().deny_resource(ResourceKind::Definition));
    assert!(
        graph
            .traversal()
            .with_authorizer(none)
            .list_definitions(None)
            .is_err()
    );
}

// ---------------------------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------------------------

#[test]
fn definition_errors_carry_help() {
    let cases = [
        (
            refused(Definition::query("1x", top_query())),
            "Rhai function name",
        ),
        (
            refused(Definition::query("top", top_query().with_folder("/x"))),
            "reports/sales",
        ),
        (
            refused(Definition::query("top", QueryDefinition::new(""))),
            "one Rhai function",
        ),
        (
            refused(Definition::query(
                "top",
                QueryDefinition::new(SOURCE).with_param("g", QueryParam::new(limit_schema())),
            )),
            "DSL global",
        ),
    ];
    for (err, needle) in cases {
        let help = err.help().unwrap();
        assert!(help.contains(needle), "{err}: {help}");
    }
}

// ---------------------------------------------------------------------------------------------
// Persistence (round trips: persist_tests.rs, store_tests.rs; format spec 6.3, 9.3, 16)
// ---------------------------------------------------------------------------------------------

#[cfg(feature = "persist")]
#[test]
fn a_journal_refuses_a_critical_definition_of_an_unknown_kind_without_poisoning() {
    use graphersal::persist::{Durability, Journal, SharedBuffer};

    let mut graph = TraversalGraph::tinkerpop_modern();
    Journal::new(SharedBuffer::new(), Durability::EveryCommit)
        .attach(&mut graph)
        .unwrap();
    // A saved query and a non-critical unknown kind are journaled.
    graph.set_definition(top()).unwrap();
    graph.set_definition(opaque("x", &[1, 2])).unwrap();
    // A critical definition of a kind this version does not know is refused (format spec 16
    // rule 3: this version could not keep it consistent).
    let critical = opaque("y", &[3]).with_flags(DefinitionFlags::NONE.with_critical(true));
    let err = graph.set_definition(critical).unwrap_err();
    assert!(matches!(err, GraphError::CommitRejected { .. }), "{err:?}");
    assert!(err.to_string().contains("critical"), "{err}");
    assert_eq!(graph.definitions().unwrap().count(), 2);
    // The journal still takes commits.
    graph.traversal_mut().add_v("after").to_list().unwrap();
}

#[test]
fn a_payload_key_named_like_an_envelope_key_is_refused() {
    let mut payload = PropertyMap::default();
    payload.insert("source", SOURCE.into());
    payload.insert("flags", 1.into());
    let query = QueryDefinition::from_payload(&payload).unwrap();
    let err = Definition::query("top", query).validate().unwrap_err();
    assert!(err.to_string().contains("reserved"), "{err}");
}

// ---------------------------------------------------------------------------------------------
// Host kinds (128..=255) and the public payload codec
// ---------------------------------------------------------------------------------------------

#[test]
fn host_kinds_are_128_to_255_and_never_known_to_the_library() {
    assert_eq!(DefinitionKind::HOST_FIRST.code(), 128);
    assert_eq!(DefinitionKind::HOST_LAST.code(), 255);
    assert_eq!(DefinitionKind::host(127), None);
    assert_eq!(DefinitionKind::host(0), None);
    assert_eq!(DefinitionKind::host(1), None);
    for code in 128..=255u8 {
        let kind = DefinitionKind::host(code).unwrap();
        assert_eq!(kind, DefinitionKind::from_code(code));
        assert!(kind.is_host() && !kind.is_known(), "{code}");
        assert_eq!(kind.name(), None);
    }
    for code in 0..128u8 {
        assert!(!DefinitionKind::from_code(code).is_host(), "{code}");
    }
}

/// A host payload with every kind of value the codec carries.
#[cfg(feature = "persist")]
fn host_payload() -> PropertyMap {
    let mut nested = PropertyMap::default();
    nested.insert("depth", 2.into());
    nested.insert("list", ElementProperty::from(vec![1i64, 2, 3]));
    let mut payload = PropertyMap::default();
    payload.insert("label", "person".into());
    payload.insert("keys", ElementProperty::from(vec!["name", "age"]));
    payload.insert("unique", true.into());
    payload.insert("ratio", 0.5.into());
    payload.insert("id", ElementProperty::Uuid(0x1234_5678));
    payload.insert("nothing", ElementProperty::Null);
    payload.insert("options", ElementProperty::Object(Box::new(nested)));
    payload
}

#[cfg(feature = "persist")]
#[test]
fn the_payload_codec_round_trips_and_rejects_bad_input() {
    use graphersal::catalog::{decode_payload, encode_payload};

    let payload = host_payload();
    let bytes = encode_payload(&payload).unwrap();
    let back = decode_payload(&bytes).unwrap();
    assert_eq!(back, payload);
    // Insertion order is kept.
    let keys: Vec<&str> = back.keys().map(|k| k.as_str()).collect();
    assert_eq!(
        keys,
        [
            "label", "keys", "unique", "ratio", "id", "nothing", "options"
        ]
    );
    // The empty map is one byte (count 0).
    assert_eq!(encode_payload(&PropertyMap::default()).unwrap(), [0]);

    // The library's own kinds use the same codec: a saved query's payload decodes with it.
    let map = top_query().to_payload();
    assert_eq!(decode_payload(&encode_payload(&map).unwrap()).unwrap(), map);

    // A reserved key is refused both ways.
    for key in catalog::RESERVED_PAYLOAD_KEYS {
        let mut bad = PropertyMap::default();
        bad.insert(key, 1.into());
        let err = encode_payload(&bad).unwrap_err();
        assert!(err.to_string().contains("reserved"), "{err}");
    }
    let mut plain = PropertyMap::default();
    plain.insert("kinx", 1.into());
    let mut reserved = encode_payload(&plain).unwrap();
    let at = reserved.windows(4).position(|w| w == b"kinx").unwrap();
    reserved[at + 3] = b'd';
    let err = decode_payload(&reserved).unwrap_err();
    assert!(err.to_string().contains("reserved"), "{err}");

    // Trailing bytes and every truncation are errors; no single-byte change panics.
    let mut trailing = bytes.clone();
    trailing.push(0);
    let err = decode_payload(&trailing).unwrap_err();
    assert!(err.to_string().contains("trailing"), "{err}");
    for cut in 0..bytes.len() {
        assert!(decode_payload(&bytes[..cut]).is_err(), "cut at {cut}");
    }
    for at in 0..bytes.len() {
        for value in [0u8, 0x7f, 0x80, 0xff] {
            let mut changed = bytes.clone();
            changed[at] = value;
            let _ = decode_payload(&changed);
        }
    }
    // A huge declared count is refused before allocating.
    let err = decode_payload(&[0xff, 0xff, 0xff, 0xff, 0x0f]).unwrap_err();
    assert!(matches!(err, DefinitionError::Malformed { .. }), "{err:?}");
}

#[test]
fn a_host_definition_is_stored_opaque_and_survives_units_and_the_exchange_form() {
    let kind = DefinitionKind::host(200).unwrap();
    let host = Definition::opaque(
        kind,
        DefinitionFlags::NONE,
        "person_name",
        vec![3, 1, 4, 1, 5, 9, 2, 6],
    );
    host.validate().unwrap();
    assert!(host.is_opaque());

    let mut graph = TraversalGraph::new();
    graph.set_definition(host.clone()).unwrap();
    assert_eq!(graph.definition(kind, "person_name").unwrap(), Some(&host));
    // A failed transaction restores it.
    let failed: Result<(), GraphError> = graph.transaction(|g| {
        g.remove_definition(kind, "person_name")?;
        g.set_definition(Definition::query("1x", top_query()))
    });
    assert!(failed.is_err());
    assert_eq!(graph.definition(kind, "person_name").unwrap(), Some(&host));

    // The exchange form carries the numeric kind and the bytes.
    let text = catalog_to_json(graph.definitions().unwrap());
    assert!(text.contains("\"kind\":200"), "{text}");
    assert_eq!(catalog_from_json(&text).unwrap(), vec![host]);
}
