//! A storage whose backend fails ([`Failing`]): the engine reports the storage's error as the
//! query's (or the unit's) error, never panics, never answers with made-up data, and the graph
//! stays usable once the backend is back.

use std::error::Error as _;

use graphersal::catalog::PropertyIndexDefinition;
use graphersal::prelude::*;
use graphersal::storage::{ElementKind, GraphError};
use graphersal_storage_tests::failing::{Failing, Faults};

/// The error with every source in its chain, as one text.
fn chain(err: &(dyn std::error::Error + 'static)) -> String {
    let mut text = err.to_string();
    let mut source = err.source();
    while let Some(next) = source {
        text.push_str(" | ");
        text.push_str(&next.to_string());
        source = next.source();
    }
    text
}

fn modern() -> Failing {
    Failing::new(TraversalGraph::tinkerpop_modern())
}

fn person_count(storage: &Failing) -> TraverserResult<Option<ElementProperty>> {
    GraphTraversalSource::new(storage)
        .v(None)
        .has_label("person")
        .count()
        .next()
}

#[test]
fn a_statistics_failure_fails_the_query_that_reads_them() {
    let storage = modern();
    let faults = storage.faults();
    Faults::set(&faults.statistics, true);
    let err = person_count(&storage).unwrap_err();
    let text = chain(&*err);
    assert!(
        text.contains("failed to read its statistics"),
        "the storage's error is the query's: {text}"
    );
    Faults::set(&faults.statistics, false);
    assert_eq!(person_count(&storage).unwrap(), Some(4i64.into()));
}

#[test]
fn a_property_index_listing_failure_fails_every_run_before_it_executes() {
    let mut storage = modern();
    let faults = storage.faults();
    Faults::set(&faults.property_indexes, true);
    let err = GraphTraversalSource::new(&mut storage)
        .add_v("person")
        .iterate()
        .unwrap_err();
    assert!(
        chain(&*err).contains("failed to read its property indexes"),
        "{err}"
    );
    Faults::set(&faults.property_indexes, false);
    assert_eq!(
        storage.vertex_count().unwrap(),
        6,
        "the failed run changed nothing"
    );
    assert_eq!(person_count(&storage).unwrap(), Some(4i64.into()));
}

#[test]
fn an_index_lookup_failure_fails_the_query_instead_of_scanning() {
    let mut storage = modern();
    let index = PropertyIndexDefinition::new(ElementKind::Vertex, "person", "age");
    GraphTraversalSource::new(&mut storage)
        .create_index("person_age", index)
        .unwrap();
    let names = |storage: &Failing| {
        GraphTraversalSource::new(storage)
            .v(None)
            .has_label("person")
            .has("age", 29)
            .values("name")
            .to_list()
    };
    assert_eq!(
        names(&storage).unwrap(),
        vec![ElementProperty::from("marko")]
    );
    let faults = storage.faults();
    Faults::set(&faults.index_lookup, true);
    let err = names(&storage).unwrap_err();
    assert!(
        chain(&*err).contains("failed to read a property index"),
        "{err}"
    );
    Faults::set(&faults.index_lookup, false);
    assert_eq!(
        names(&storage).unwrap(),
        vec![ElementProperty::from("marko")]
    );
}

#[test]
fn a_unit_that_cannot_open_fails_before_anything_changes() {
    let mut storage = modern();
    let faults = storage.faults();
    Faults::set(&faults.begin_unit, true);
    let err = GraphTraversalSource::new(&mut storage)
        .add_v("person")
        .iterate()
        .unwrap_err();
    assert!(chain(&*err).contains("failed to open a unit"), "{err}");
    let failed: Result<(), GraphError> = storage.transaction(|_| Ok(()));
    assert!(
        matches!(failed, Err(GraphError::StorageFailed { .. })),
        "{failed:?}"
    );
    Faults::set(&faults.begin_unit, false);
    assert_eq!(storage.vertex_count().unwrap(), 6);
    GraphTraversalSource::new(&mut storage)
        .add_v("person")
        .iterate()
        .unwrap();
    assert_eq!(storage.vertex_count().unwrap(), 7);
}

#[test]
fn a_held_unit_whose_storage_unit_cannot_open_fails_its_write() {
    let graph = Graph::new(modern());
    let faults = graph.read().faults();
    let mark = graph.begin_unit().unwrap();
    Faults::set(&faults.begin_unit, true);
    // The storage's unit opens at the unit's first write: that write fails with its error.
    let refused = graph.write_checked().map(|_| ());
    assert!(
        matches!(refused, Err(GraphError::StorageFailed { .. })),
        "{refused:?}"
    );
    Faults::set(&faults.begin_unit, false);
    {
        let mut write = graph.write_checked().unwrap();
        GraphTraversalSource::new(&mut *write)
            .add_v("person")
            .iterate()
            .unwrap();
    }
    graph.commit_unit(mark).unwrap();
    assert_eq!(graph.read().vertex_count().unwrap(), 7);
}

#[test]
fn a_failed_rollback_closes_the_unit_and_reports_both_errors() {
    let mut storage = modern();
    let faults = storage.faults();
    Faults::set(&faults.rollback_unit, true);
    let err = GraphTraversalSource::new(&mut storage)
        .add_v("ghost")
        .constant("x")
        .as_number(GType::LONG)
        .iterate()
        .unwrap_err();
    let text = chain(&*err);
    assert!(
        text.contains("rolling the unit back failed as well"),
        "{text}"
    );
    assert!(text.contains("failed to roll a unit back"), "{text}");
    Faults::set(&faults.rollback_unit, false);
    // The unit is closed: the graph serves the next unit normally.
    assert_eq!(
        storage.vertex_count().unwrap(),
        6,
        "the wrapped graph rolled back"
    );
    GraphTraversalSource::new(&mut storage)
        .add_v("person")
        .iterate()
        .unwrap();
    assert_eq!(storage.vertex_count().unwrap(), 7);
    assert_eq!(person_count(&storage).unwrap(), Some(5i64.into()));
}

#[test]
fn a_failed_rollback_of_a_transaction_replaces_the_closure_error() {
    let mut storage = modern();
    let faults = storage.faults();
    Faults::set(&faults.rollback_unit, true);
    let failed: Result<(), GraphError> = storage.transaction(|g| {
        GraphTraversalSource::new(&mut *g)
            .add_v("person")
            .iterate()
            .map_err(|_| GraphError::unsupported("this test"))?;
        Err(GraphError::unsupported("the closure's own failure"))
    });
    assert!(
        matches!(failed, Err(GraphError::RollbackFailed { .. })),
        "{failed:?}"
    );
    Faults::set(&faults.rollback_unit, false);
    assert_eq!(storage.vertex_count().unwrap(), 6);
    let ok: Result<(), GraphError> = storage.transaction(|_| Ok(()));
    assert!(ok.is_ok());
}

#[test]
fn a_schema_or_policy_failure_fails_the_run() {
    let mut storage = modern();
    let faults = storage.faults();
    Faults::set(&faults.execution_policy, true);
    let err = person_count(&storage).unwrap_err();
    assert!(
        chain(&*err).contains("failed to read its execution policy"),
        "{err}"
    );
    Faults::set(&faults.execution_policy, false);
    Faults::set(&faults.get_schema, true);
    // A create reads the schema for its defaults.
    let err = GraphTraversalSource::new(&mut storage)
        .add_v("person")
        .iterate()
        .unwrap_err();
    assert!(chain(&*err).contains("failed to read its schema"), "{err}");
    Faults::set(&faults.get_schema, false);
    assert_eq!(storage.vertex_count().unwrap(), 6);
}

#[test]
fn storage_failed_has_help_and_its_source() {
    let err = GraphError::storage_failed("read its statistics", std::io::Error::other("disk"));
    assert!(
        err.to_string()
            .contains("failed to read its statistics: disk")
    );
    assert!(err.source().is_some());
}

#[test]
fn an_element_scan_failure_fails_the_query_at_its_source_step() {
    let storage = modern();
    let faults = storage.faults();
    Faults::set(&faults.element_scan, true);
    let names = |storage: &Failing| {
        GraphTraversalSource::new(storage)
            .v(None)
            .values("name")
            .to_list()
    };
    let err = names(&storage).unwrap_err();
    let text = chain(&*err);
    assert!(text.contains("failed to scan its vertices"), "{text}");
    assert!(text.contains("'v()'"), "located at the source step: {text}");
    let err = GraphTraversalSource::new(&storage)
        .v(None)
        .has_label("person")
        .values("name")
        .to_list()
        .unwrap_err();
    assert!(
        chain(&*err).contains("failed to scan its vertices by label"),
        "{err}"
    );
    Faults::set(&faults.element_scan, false);
    assert_eq!(names(&storage).unwrap().len(), 6);
}

#[test]
fn a_property_read_failure_fails_the_query_at_the_reading_step() {
    let storage = modern();
    let faults = storage.faults();
    Faults::set(&faults.element_read, true);
    let err = GraphTraversalSource::new(&storage)
        .v(None)
        .has("name", "marko")
        .to_list()
        .unwrap_err();
    let text = chain(&*err);
    assert!(
        text.contains("failed to read the properties of a vertex"),
        "{text}"
    );
    let err = GraphTraversalSource::new(&storage)
        .e(None)
        .values("weight")
        .to_list()
        .unwrap_err();
    assert!(
        chain(&*err).contains("failed to read the properties of an edge"),
        "{err}"
    );
    Faults::set(&faults.element_read, false);
    let found = GraphTraversalSource::new(&storage)
        .v(None)
        .has("name", "marko")
        .to_list()
        .unwrap();
    assert_eq!(found.len(), 1);
}

#[test]
fn an_adjacency_failure_fails_the_query_at_the_expanding_step() {
    let storage = modern();
    let faults = storage.faults();
    Faults::set(&faults.adjacency, true);
    let out = |storage: &Failing| {
        GraphTraversalSource::new(storage)
            .v(None)
            .out(None)
            .count()
            .next()
    };
    let err = out(&storage).unwrap_err();
    let text = chain(&*err);
    assert!(
        text.contains("failed to read the outgoing edges of a vertex"),
        "{text}"
    );
    assert!(text.contains("'out()'"), "located at out(): {text}");
    Faults::set(&faults.adjacency, false);
    assert_eq!(out(&storage).unwrap(), Some(6i64.into()));
}

#[test]
fn a_unit_clock_failure_fails_the_create_and_changes_nothing() {
    let mut storage = modern();
    let faults = storage.faults();
    Faults::set(&faults.unit_started_at, true);
    let err = GraphTraversalSource::new(&mut storage)
        .add_v("person")
        .iterate()
        .unwrap_err();
    assert!(
        chain(&*err).contains("failed to read the unit clock"),
        "{err}"
    );
    Faults::set(&faults.unit_started_at, false);
    assert_eq!(storage.vertex_count().unwrap(), 6);
}
