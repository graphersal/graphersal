//! The result alignment (root CLAUDE.md rule 15, "Result for shared state"): a method that
//! touches the graph's lock, a Store or a file answers a failure as its typed error, never as an
//! empty or default value. Each test drives one former silent answer to its error.

use std::sync::{Arc, Barrier};

use graphersal::catalog::DefinitionKind;
use graphersal::changes::RollbackReason;
use graphersal::prelude::*;
use graphersal::storage::{AnyGraph, IntoAnyGraph};

fn modern() -> Arc<Graph> {
    Arc::new(Graph::new(TraversalGraph::tinkerpop_modern()))
}

fn view_held<T: std::fmt::Debug>(answer: Result<T, GraphError>) {
    match answer {
        Err(GraphError::GraphViewHeld { .. }) => {}
        other => panic!("expected GraphViewHeld, got {other:?}"),
    }
}

/// Every read of the dyn boundary on the thread that holds the graph's unit beside a live
/// write view is `GraphViewHeld`: formerly `None`, empty, `0` or no capabilities plus a log line.
#[test]
fn any_graph_reads_answer_a_forbidden_view_with_its_error() {
    let graph = modern();
    let any: Arc<dyn AnyGraph> = graph.clone().into_any_graph();
    let mark = graph.begin_unit().unwrap();
    {
        let _write = graph.write();
        view_held(any.capabilities());
        view_held(any.last_commit_seq());
        view_held(any.statistics());
        view_held(any.memory_usage());
        view_held(any.get_schema());
        view_held(any.definitions(None));
        view_held(any.definition(DefinitionKind::QUERY, "anything"));
        view_held(any.compression_stats("anything"));
        view_held(any.property_indexes());
    }
    // Without the aliasing view the same reads answer.
    assert_eq!(any.statistics().unwrap().unwrap().vertex_count(), 6);
    assert!(any.get_schema().unwrap().is_none());
    assert!(any.definitions(None).unwrap().is_empty());
    graph
        .rollback_unit(mark, &RollbackReason::failed(&"test"))
        .unwrap();
}

/// `try_write_checked` tells the three answers apart: free, busy, forbidden.
#[test]
fn try_write_checked_tells_busy_from_forbidden() {
    let graph = modern();
    // Free.
    assert!(graph.try_write_checked().unwrap().is_some());
    // Busy: another thread reads.
    let held = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let reader = {
        let (graph, held, release) = (graph.clone(), held.clone(), release.clone());
        std::thread::spawn(move || {
            let guard = graph.read();
            held.wait();
            release.wait();
            drop(guard);
        })
    };
    held.wait();
    assert!(graph.try_write_checked().unwrap().is_none());
    assert!(graph.try_write().is_none());
    release.wait();
    reader.join().unwrap();
    // Forbidden: inside this thread's unit, beside a read view; `try_write` says only `None`.
    let mark = graph.begin_unit().unwrap();
    {
        let _view = graph.read();
        let err = graph.try_write_checked().err().expect("forbidden");
        assert!(matches!(err, GraphError::GraphViewHeld { .. }), "{err:?}");
        assert!(err.to_string().contains("write the graph"), "{err}");
        assert!(graph.try_write().is_none());
    }
    // Inside the unit without another view: the unit's write view.
    {
        let mut write = graph.try_write_checked().unwrap().expect("the unit's view");
        GraphTraversalSource::new(&mut *write)
            .add_v("person")
            .iterate()
            .unwrap();
    }
    graph.commit_unit(mark).unwrap();
    assert_eq!(graph.read().vertex_count(), 7);
}

#[cfg(feature = "persist")]
mod store {
    use graphersal::TraversalGraph;
    use graphersal::catalog::{Definition, JobDefinition, JobTask};
    use graphersal::persist::{MemDir, SingleFileDir, Store, StoreOptions};
    use graphersal::storage::PersistentStorage;

    /// A job whose stored schedule does not parse (written without the DSL's checks, or by a
    /// newer version) is listed as not scheduled with the reason: formerly dropped silently.
    #[test]
    fn an_unparsable_job_schedule_is_listed_not_dropped() {
        let mut graph = TraversalGraph::new();
        // The load sink stores what it is given (no validation), as a load from disk does.
        graph
            .load_definition(Definition::job(
                "nightly",
                JobDefinition::new("every night at three", JobTask::Checkpoint),
            ))
            .unwrap();
        graph
            .load_definition(Definition::job(
                "hourly",
                JobDefinition::new("0 * * * *", JobTask::Checkpoint),
            ))
            .unwrap();
        graph.finish_load();
        let mut packed = Vec::new();
        graph.write_snapshot(&mut packed).unwrap();
        let store =
            Store::create_from_packed(MemDir::new(), packed.as_slice(), StoreOptions::new())
                .unwrap();
        let found = store.user_jobs().unwrap();
        let names: Vec<&str> = found.jobs.iter().map(|job| job.name()).collect();
        assert_eq!(names, ["hourly"]);
        assert_eq!(found.not_scheduled.len(), 1);
        assert_eq!(found.not_scheduled[0].name, "nightly");
        assert!(
            found.not_scheduled[0].reason.contains("does not parse"),
            "{}",
            found.not_scheduled[0].reason
        );
    }

    /// Nothing at a path is "not a store"; a missing path is never an error.
    #[test]
    fn a_missing_path_is_not_a_store_and_not_an_error() {
        let path = std::env::temp_dir().join(format!(
            "graphersal-result-api-missing-{}",
            std::process::id()
        ));
        assert!(!Store::is_store(&path).unwrap());
        assert!(!SingleFileDir::is_single_file(&path).unwrap());
    }

    #[cfg(unix)]
    mod unreadable {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        use std::path::{Path, PathBuf};

        use graphersal::persist::{PersistError, SingleFileDir, Store, StoreOptions};

        /// A directory removed (with its permissions restored first) on drop.
        struct Scratch(PathBuf);

        impl Scratch {
            fn new(name: &str) -> Self {
                let path = std::env::temp_dir().join(format!(
                    "graphersal-result-api-{}-{name}",
                    std::process::id()
                ));
                let _ = fs::remove_dir_all(&path);
                Self(path)
            }
        }

        impl Drop for Scratch {
            fn drop(&mut self) {
                restore(&self.0);
                let _ = fs::remove_dir_all(&self.0);
            }
        }

        fn restore(path: &Path) {
            if let Ok(entries) = fs::read_dir(path) {
                for entry in entries.flatten() {
                    let _ = fs::set_permissions(entry.path(), fs::Permissions::from_mode(0o755));
                }
            }
        }

        /// `chmod 000` on `path`; `false` when the process can read it anyway (root).
        fn lock_out(path: &Path) -> bool {
            fs::set_permissions(path, fs::Permissions::from_mode(0o000)).unwrap();
            let readable = if path.is_dir() {
                fs::read_dir(path).is_ok()
            } else {
                fs::read(path).is_ok()
            };
            if readable {
                fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
            }
            !readable
        }

        fn is_io(err: &PersistError) -> bool {
            matches!(err, PersistError::Io { .. })
        }

        /// An attic or models directory that cannot be read fails the store info: formerly an
        /// empty attic or model list.
        #[test]
        fn an_unreadable_attic_or_models_dir_fails_the_info() {
            for part in ["attic", "models"] {
                let dir = Scratch::new(part);
                let store = Store::create(&dir.0, StoreOptions::new()).unwrap();
                assert!(store.info().is_ok());
                let locked = dir.0.join(part);
                fs::create_dir_all(&locked).unwrap();
                if !lock_out(&locked) {
                    return; // root reads everything: nothing to test
                }
                let err = store.info().expect_err(part);
                assert!(is_io(&err), "{part}: {err:?}");
                fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
                assert!(store.info().is_ok());
                store.close().unwrap();
            }
        }

        /// A directory or file that cannot be read is an I/O error, never "not a store".
        #[test]
        fn an_unreadable_path_is_an_error_not_a_non_store() {
            let dir = Scratch::new("closed-dir");
            fs::create_dir_all(dir.0.join("inner")).unwrap();
            let inner = dir.0.join("inner");
            if !lock_out(&inner) {
                return;
            }
            // `inner/GRAPH` cannot be looked up.
            let err = Store::is_store(&inner).expect_err("unreadable");
            assert!(is_io(&err), "{err:?}");
            fs::set_permissions(&inner, fs::Permissions::from_mode(0o755)).unwrap();
            assert!(!Store::is_store(&inner).unwrap());

            let file = dir.0.join("closed.gstore");
            fs::write(&file, b"not a store at all").unwrap();
            assert!(!SingleFileDir::is_single_file(&file).unwrap());
            if !lock_out(&file) {
                return;
            }
            let err = SingleFileDir::is_single_file(&file).expect_err("unreadable");
            assert!(is_io(&err), "{err:?}");
        }
    }
}
