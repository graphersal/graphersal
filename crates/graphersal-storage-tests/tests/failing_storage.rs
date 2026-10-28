//! A storage whose backend fails ([`Failing`]): the engine reports the storage's error as the
//! query's (or the unit's) error, never panics, never answers with made-up data, and the graph
//! stays usable once the backend is back. The exception is a failed rollback: behind a `Graph`
//! it poisons the graph (the storage is in an unknown state), which then refuses every read and
//! write; a bare storage is not poisoned (poisoning is a property of `Graph`).

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
    // The unit is closed. On a bare storage nothing is poisoned (see
    // `a_failed_rollback_poisons_the_graph` for a `Graph`): this storage serves the next unit.
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
fn a_read_failure_inside_the_glob_path_walk_fails_the_query_at_the_step() {
    let storage = modern();
    let faults = storage.faults();
    let walk = |storage: &Failing, labels: &[&str]| {
        GraphTraversalSource::new(storage)
            .v("1")
            // A literal part, so every child's name is read (a bare `*` reads none).
            .glob_path("*o*")
            .by("name")
            .by(labels.to_vec())
            .count()
            .next()
    };
    let all = ["person", "software"];
    for (switch, read) in [
        (
            &faults.adjacency,
            "failed to read the outgoing edges of a vertex",
        ),
        (
            &faults.element_read,
            "failed to read the properties of a vertex",
        ),
    ] {
        Faults::set(switch, true);
        let err = walk(&storage, &all).unwrap_err();
        let text = chain(&*err);
        assert!(text.contains(read), "{text}");
        assert!(
            text.contains("glob_path("),
            "located at glob_path(): {text}"
        );
        Faults::set(switch, false);
    }
    // marko's children are lop (software), vadas and josh (person).
    assert_eq!(walk(&storage, &all).unwrap(), Some(2i64.into()));
    assert_eq!(walk(&storage, &["software"]).unwrap(), Some(1i64.into()));
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

/// A traversal on a write view whose unit cannot be rolled back.
fn fail_with_a_broken_rollback(graph: &Graph<Failing>) -> String {
    let mut write = graph.write_checked().unwrap();
    let err = GraphTraversalSource::new(&mut *write)
        .add_v("ghost")
        .constant("x")
        .as_number(GType::LONG)
        .iterate()
        .unwrap_err();
    chain(&*err)
}

/// `answer` is `GraphError::Poisoned` naming the failed rollback.
fn assert_poisoned<T: std::fmt::Debug>(answer: Result<T, GraphError>) {
    match answer {
        Err(GraphError::Poisoned { cause, .. }) => {
            assert!(cause.contains("failed to roll a unit back"), "{cause}")
        }
        other => panic!("expected Poisoned, got {other:?}"),
    }
}

#[test]
fn a_failed_rollback_poisons_the_graph() {
    let graph = Graph::new(modern());
    let faults = graph.read().faults();
    assert!(!graph.is_poisoned());
    assert_eq!(graph.poison_cause(), None);
    Faults::set(&faults.rollback_unit, true);
    let text = fail_with_a_broken_rollback(&graph);
    assert!(
        text.contains("rolling the unit back failed as well"),
        "{text}"
    );
    // The backend is back, but the storage's state is unknown: the graph refuses everything.
    Faults::set(&faults.rollback_unit, false);
    assert!(graph.is_poisoned());
    assert!(
        graph
            .poison_cause()
            .is_some_and(|cause| cause.contains("failed to roll a unit back")),
        "{:?}",
        graph.poison_cause()
    );
    assert_poisoned(graph.read_checked().map(|_| ()));
    assert_poisoned(graph.write_checked().map(|_| ()));
    assert_poisoned(graph.try_write_checked().map(|_| ()));
    assert_poisoned(graph.begin_unit());
    assert!(graph.try_read().is_none());
    assert!(graph.try_write().is_none());
    let err = graph.read_checked().map(|_| ()).unwrap_err();
    assert!(
        err.help()
            .is_some_and(|help| help.contains("reopen the storage")),
        "{:?}",
        err.help()
    );
    // The infallible forms panic, as documented.
    let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = graph.read();
    }));
    assert!(read.is_err(), "read() panics on a poisoned graph");
    let write = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = graph.write();
    }));
    assert!(write.is_err(), "write() panics on a poisoned graph");
    // The storage can still be taken out to close or salvage it.
    let storage = graph.into_inner();
    assert_eq!(storage.vertex_count().unwrap(), 6);
}

#[test]
fn a_failed_rollback_of_a_transaction_on_a_write_view_poisons_the_graph() {
    let graph = Graph::new(modern());
    let faults = graph.read().faults();
    Faults::set(&faults.rollback_unit, true);
    let failed: Result<(), GraphError> = graph
        .write_checked()
        .unwrap()
        .transaction(|_| Err(GraphError::unsupported("the closure's own failure")));
    assert!(
        matches!(failed, Err(GraphError::RollbackFailed { .. })),
        "{failed:?}"
    );
    Faults::set(&faults.rollback_unit, false);
    assert_poisoned(graph.read_checked().map(|_| ()));
}

#[test]
fn a_panic_through_a_unit_whose_rollback_fails_poisons_the_graph() {
    let graph = Graph::new(modern());
    let faults = graph.read().faults();
    Faults::set(&faults.rollback_unit, true);
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _: Result<(), GraphError> = graph.write_checked().unwrap().transaction(|g| {
            GraphTraversalSource::new(&mut *g)
                .add_v("person")
                .iterate()
                .unwrap();
            panic!("an engine bug inside the unit")
        });
    }));
    assert!(unwound.is_err());
    Faults::set(&faults.rollback_unit, false);
    assert!(graph.is_poisoned());
    assert_poisoned(graph.write_checked().map(|_| ()));
}

#[test]
fn a_dry_run_whose_discard_fails_poisons_the_graph() {
    let graph = Graph::new(modern());
    let faults = graph.read().faults();
    Faults::set(&faults.discard_unit, true);
    let previewed: Result<((), _), GraphError> = graph.write_checked().unwrap().dry_run(|g| {
        GraphTraversalSource::new(&mut *g)
            .add_v("person")
            .iterate()
            .map_err(|_| GraphError::unsupported("this test"))
    });
    assert!(previewed.is_err(), "{previewed:?}");
    Faults::set(&faults.discard_unit, false);
    assert!(graph.is_poisoned());
    match graph.read_checked().map(|_| ()) {
        Err(GraphError::Poisoned { cause, .. }) => {
            assert!(cause.contains("failed to discard a unit"), "{cause}")
        }
        other => panic!("expected Poisoned, got {other:?}"),
    }
}

#[test]
fn a_failed_rollback_of_a_held_unit_poisons_the_graph_and_releases_the_lock() {
    let graph = Graph::new(modern());
    let faults = graph.read().faults();
    let mark = graph.begin_unit().unwrap();
    GraphTraversalSource::new(&mut *graph.write_checked().unwrap())
        .add_v("person")
        .iterate()
        .unwrap();
    Faults::set(&faults.rollback_unit, true);
    let failed = graph.rollback_unit(mark, &graphersal::changes::RollbackReason::failed(&"test"));
    assert!(
        matches!(failed, Err(GraphError::StorageFailed { .. })),
        "{failed:?}"
    );
    Faults::set(&faults.rollback_unit, false);
    assert!(!graph.holds_unit(), "the unit is closed");
    assert!(graph.is_poisoned());
    // Another thread is refused instead of waiting for a lock nobody releases.
    std::thread::scope(|scope| {
        scope
            .spawn(|| assert_poisoned(graph.read_checked().map(|_| ())))
            .join()
            .unwrap();
    });
}

#[test]
fn a_held_unit_poisoned_inside_refuses_its_commit_but_closes() {
    let graph = Graph::new(modern());
    let faults = graph.read().faults();
    let mark = graph.begin_unit().unwrap();
    Faults::set(&faults.rollback_unit, true);
    // A failing traversal inside the unit: its savepoint cannot be rolled back.
    let text = fail_with_a_broken_rollback(&graph);
    assert!(text.contains("failed to roll a unit back"), "{text}");
    Faults::set(&faults.rollback_unit, false);
    assert!(graph.is_poisoned());
    assert_poisoned(graph.write_checked().map(|_| ()));
    // The commit is refused (nothing of an unknown state is committed), but the unit closes
    // and the lock is released.
    assert_poisoned(graph.commit_unit(mark));
    assert!(!graph.holds_unit());
    std::thread::scope(|scope| {
        scope
            .spawn(|| assert_poisoned(graph.write_checked().map(|_| ())))
            .join()
            .unwrap();
    });
}

#[test]
fn a_failed_rollback_on_a_bare_storage_poisons_no_graph() {
    // A rollback failing on a bare storage while no write view of a graph is open poisons
    // nothing; a graph's own failure poisons only that graph.
    let healthy = Graph::new(modern());
    let mut bare = modern();
    Faults::set(&bare.faults().rollback_unit, true);
    let failed: Result<(), GraphError> =
        bare.transaction(|_| Err(GraphError::unsupported("the closure's own failure")));
    assert!(matches!(failed, Err(GraphError::RollbackFailed { .. })));
    {
        let _view = healthy.read_checked().unwrap();
    }
    let broken = Graph::new(modern());
    Faults::set(&broken.read().faults().rollback_unit, true);
    fail_with_a_broken_rollback(&broken);
    assert!(broken.is_poisoned());
    assert!(!healthy.is_poisoned());
    assert_eq!(healthy.read_checked().unwrap().vertex_count().unwrap(), 6);
}

// Poison isolation: a failed rollback poisons exactly the graph whose storage failed, never
// another graph on the same thread, and nothing for a bare storage.

/// A traversal on `storage` (a graph's write view or a bare storage) whose unit fails and is
/// rolled back.
fn fail_a_traversal_on<S: GraphStorage>(storage: &mut S) -> String {
    let err = GraphTraversalSource::new(storage)
        .add_v("ghost")
        .constant("x")
        .as_number(GType::LONG)
        .iterate()
        .unwrap_err();
    chain(&*err)
}

/// Two graphs, each with its rollback switch on.
fn two_broken_graphs() -> (Graph<Failing>, Graph<Failing>) {
    let a = Graph::new(modern());
    let b = Graph::new(modern());
    Faults::set(&a.read().faults().rollback_unit, true);
    Faults::set(&b.read().faults().rollback_unit, true);
    (a, b)
}

#[test]
fn nested_write_views_of_two_graphs_poison_only_the_inner_failing_one() {
    let (outer, inner) = two_broken_graphs();
    {
        let _outer_view = outer.write_checked().unwrap();
        let mut inner_view = inner.write_checked().unwrap();
        fail_a_traversal_on(&mut *inner_view);
    }
    assert!(inner.is_poisoned());
    assert!(!outer.is_poisoned());
    assert_eq!(outer.read_checked().unwrap().vertex_count().unwrap(), 6);
}

#[test]
fn nested_write_views_of_two_graphs_poison_only_the_outer_failing_one() {
    let (outer, inner) = two_broken_graphs();
    {
        let mut outer_view = outer.write_checked().unwrap();
        let _inner_view = inner.write_checked().unwrap();
        // The outer graph's storage fails while the inner graph's view is the innermost one.
        fail_a_traversal_on(&mut *outer_view);
    }
    assert!(outer.is_poisoned());
    assert!(!inner.is_poisoned());
    assert_eq!(inner.read_checked().unwrap().vertex_count().unwrap(), 6);
}

#[test]
fn write_views_dropped_out_of_order_poison_only_their_own_graph() {
    // The first graph's view fails and is dropped first, while the second one's is still open.
    let (first, second) = two_broken_graphs();
    let mut first_view = first.write_checked().unwrap();
    let second_view = second.write_checked().unwrap();
    fail_a_traversal_on(&mut *first_view);
    drop(first_view);
    assert!(first.is_poisoned());
    assert!(!second.is_poisoned());
    drop(second_view);
    assert!(!second.is_poisoned());

    // The second graph's view fails, the first graph's (opened before it) is dropped first.
    let (first, second) = two_broken_graphs();
    let first_view = first.write_checked().unwrap();
    let mut second_view = second.write_checked().unwrap();
    fail_a_traversal_on(&mut *second_view);
    drop(first_view);
    assert!(!first.is_poisoned());
    assert!(!second.is_poisoned(), "poisoned when its own view ends");
    drop(second_view);
    assert!(second.is_poisoned());
    assert!(!first.is_poisoned());
}

#[test]
fn a_bare_storage_failing_beside_a_graphs_write_view_poisons_nothing() {
    let graph = Graph::new(modern());
    let mut bare = modern();
    Faults::set(&bare.faults().rollback_unit, true);
    {
        let _view = graph.write_checked().unwrap();
        let text = fail_a_traversal_on(&mut bare);
        assert!(text.contains("failed to roll a unit back"), "{text}");
        let failed: Result<(), GraphError> =
            bare.transaction(|_| Err(GraphError::unsupported("the closure's own failure")));
        assert!(matches!(failed, Err(GraphError::RollbackFailed { .. })));
        let previewed: Result<((), _), GraphError> = bare.dry_run(|_| Ok(()));
        assert!(
            previewed.is_ok(),
            "the discard switch is off: {previewed:?}"
        );
    }
    assert!(!graph.is_poisoned());
    assert_eq!(graph.read_checked().unwrap().vertex_count().unwrap(), 6);
}

#[test]
fn a_failure_in_a_held_unit_poisons_only_its_graph_beside_another_held_unit() {
    let (a, b) = two_broken_graphs();
    let mark_b = b.begin_unit().unwrap();
    GraphTraversalSource::new(&mut *b.write_checked().unwrap())
        .add_v("person")
        .iterate()
        .unwrap();
    let mark_a = a.begin_unit().unwrap();
    {
        // B's write view stays open while A's savepoint fails to roll back.
        let _b_view = b.write_checked().unwrap();
        fail_a_traversal_on(&mut *a.write_checked().unwrap());
    }
    assert!(a.is_poisoned());
    assert!(!b.is_poisoned());
    assert_poisoned(a.commit_unit(mark_a));
    b.commit_unit(mark_b).unwrap();
    assert_eq!(b.read_checked().unwrap().vertex_count().unwrap(), 7);

    // The close of A's held unit fails while B holds a unit with a write view open.
    let (a, b) = two_broken_graphs();
    let mark_b = b.begin_unit().unwrap();
    let mark_a = a.begin_unit().unwrap();
    GraphTraversalSource::new(&mut *a.write_checked().unwrap())
        .add_v("person")
        .iterate()
        .unwrap();
    {
        let _b_view = b.write_checked().unwrap();
        let failed = a.rollback_unit(
            mark_a,
            &graphersal::changes::RollbackReason::failed(&"test"),
        );
        assert!(matches!(failed, Err(GraphError::StorageFailed { .. })));
    }
    assert!(a.is_poisoned());
    assert!(!b.is_poisoned());
    b.commit_unit(mark_b).unwrap();
}

#[test]
fn a_failure_on_one_thread_poisons_only_that_threads_graph() {
    let (broken, healthy) = two_broken_graphs();
    Faults::set(&healthy.read().faults().rollback_unit, false);
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let mut view = broken.write_checked().unwrap();
            barrier.wait();
            fail_a_traversal_on(&mut *view);
        });
        scope.spawn(|| {
            let mut view = healthy.write_checked().unwrap();
            barrier.wait();
            let err = GraphTraversalSource::new(&mut *view)
                .add_v("ghost")
                .constant("x")
                .as_number(GType::LONG)
                .iterate();
            assert!(err.is_err(), "fails, but rolls back cleanly");
        });
    });
    assert!(broken.is_poisoned());
    assert!(!healthy.is_poisoned());
    assert_eq!(healthy.read_checked().unwrap().vertex_count().unwrap(), 6);
}

#[test]
fn graphs_of_different_storage_types_on_one_thread_stay_isolated() {
    let failing = Graph::new(modern());
    Faults::set(&failing.read().faults().rollback_unit, true);
    let plain = Graph::new(TraversalGraph::tinkerpop_modern());
    // The plain graph's view is the innermost one while the failing graph's storage fails.
    {
        let mut failing_view = failing.write_checked().unwrap();
        let _plain_view = plain.write_checked().unwrap();
        fail_a_traversal_on(&mut *failing_view);
    }
    assert!(failing.is_poisoned());
    assert!(!plain.is_poisoned());
    // And the other way round: a plain graph's failing traversal (its rollback succeeds) inside
    // a failing graph's view poisons neither.
    let failing = Graph::new(modern());
    {
        let _failing_view = failing.write_checked().unwrap();
        let mut plain_view = plain.write_checked().unwrap();
        fail_a_traversal_on(&mut *plain_view);
    }
    assert!(!failing.is_poisoned());
    assert!(!plain.is_poisoned());
}

#[test]
fn a_boxed_storage_is_attributed_to_its_graph() {
    // `Box<G>` forwards `storage_identity` to the boxed storage; a traversal on the box (or on
    // the boxed storage itself) is attributed to the graph that holds the box.
    let other = Graph::new(modern());
    Faults::set(&other.read().faults().rollback_unit, true);
    let boxed = Graph::new(Box::new(modern()));
    Faults::set(&boxed.read().faults().rollback_unit, true);
    {
        let _other_view = other.write_checked().unwrap();
        let mut view = boxed.write_checked().unwrap();
        fail_a_traversal_on(&mut **view);
    }
    assert!(boxed.is_poisoned());
    assert!(!other.is_poisoned());
    let boxed = Graph::new(Box::new(modern()));
    Faults::set(&boxed.read().faults().rollback_unit, true);
    {
        let mut view = boxed.write_checked().unwrap();
        fail_a_traversal_on(&mut *view);
    }
    assert!(boxed.is_poisoned());
}

// A storage read that fails while the engine builds the diagnostic of another error: the
// original error is reported, only without the detail that could not be read.

/// Runs `query` once to count its property reads, then again with every read after the first
/// `count - 1` failing (the last read, the diagnostic's), and returns both errors' texts.
fn with_the_last_read_failing(
    storage: &Failing,
    query: impl Fn(&Failing) -> Result<(), Box<TraverserError>>,
) -> (String, String) {
    let faults = storage.faults();
    faults.fail_element_reads_after(None);
    let healthy = chain(&*query(storage).unwrap_err());
    let reads = faults
        .element_reads
        .load(std::sync::atomic::Ordering::SeqCst);
    assert!(reads > 0, "the query reads properties");
    faults.fail_element_reads_after(Some(reads - 1));
    let faulty = chain(&*query(storage).unwrap_err());
    faults.fail_element_reads_after(None);
    (healthy, faulty)
}

#[test]
fn ids_and_labels_unreadable_for_a_value_origin_keep_the_original_error() {
    // The value origin of a cast error (`as_number`), a numeric step's (`sum`) and an
    // incomparable value (`max`) names the element by its id and label: a storage that cannot
    // read them leaves them out, the original error stays.
    type Query = dyn Fn(&Failing) -> Result<(), Box<TraverserError>>;
    let cast: &Query = &|s| {
        GraphTraversalSource::new(s)
            .v("1")
            .values("name")
            .as_number(GType::LONG)
            .iterate()
    };
    let sum: &Query = &|s| {
        GraphTraversalSource::new(s)
            .v("1")
            .values("name")
            .sum()
            .iterate()
    };
    let max: &Query = &|s| {
        GraphTraversalSource::new(s)
            .v(None)
            .values(vec!["age", "name"])
            .max()
            .iterate()
    };
    let storage = modern();
    let faults = storage.faults();
    for query in [cast, sum, max] {
        let healthy = query(&storage).unwrap_err();
        let healthy_origin = origin_of(&healthy);
        assert!(
            healthy_origin
                .as_ref()
                .is_some_and(|origin| origin.element_id.is_some() && origin.element_label.is_some()),
            "{healthy_origin:?}"
        );
        Faults::set(&faults.element_identity, true);
        let faulty = query(&storage).unwrap_err();
        Faults::set(&faults.element_identity, false);
        assert_eq!(
            chain(&*faulty),
            chain(&*healthy),
            "the original error is reported"
        );
        let origin = origin_of(&faulty).expect("the origin is still there");
        assert_eq!(origin.element_id, None, "the id could not be read");
        assert_eq!(origin.element_label, None, "the label could not be read");
        assert_eq!(origin.element_type, "vertex");
        assert_eq!(origin.property.as_deref(), Some("name"));
    }
}

/// The value origin of a cast or incomparable-value error.
fn origin_of(err: &TraverserError) -> Option<ValueOrigin> {
    match err.root_cause() {
        TraverserError::Value(ValueError::CastException { origin, .. })
        | TraverserError::Value(ValueError::IncomparableValues { origin, .. }) => {
            origin.as_deref().cloned()
        }
        other => panic!("expected a cast or incomparable-value error, got {other:?}"),
    }
}

#[test]
fn a_value_unreadable_for_a_type_name_keeps_the_original_error() {
    // A string step (`to_upper`), a list function (`conjoin`) and the `T.value` token name the
    // type of the value they refused; the last property read of each query is that diagnostic's
    // read, so failing it leaves the type out of the original error.
    type Query = dyn Fn(&Failing) -> Result<(), Box<TraverserError>>;
    let string_step: &Query = &|s| {
        GraphTraversalSource::new(s)
            .v("1")
            .values("age")
            .to_upper()
            .iterate()
    };
    let list_function: &Query = &|s| {
        GraphTraversalSource::new(s)
            .v("1")
            .values("age")
            .conjoin("-")
            .iterate()
    };
    let t_token: &Query = &|s| {
        GraphTraversalSource::new(s)
            .v("1")
            .values("age")
            .order()
            .by(By::Value)
            .iterate()
    };
    let storage = modern();
    for query in [string_step, list_function, t_token] {
        let (healthy, faulty) = with_the_last_read_failing(&storage, query);
        assert!(healthy.contains("int64"), "{healthy}");
        assert!(
            !faulty.contains("unavailable"),
            "not the storage's error: {faulty}"
        );
        assert_eq!(
            faulty,
            healthy.replace("int64", "unknown"),
            "the original error without the type"
        );
    }
}
