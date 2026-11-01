//! The check points of the process memory ceiling (`memory.limit`) outside traversals: every
//! allocation proportional to data checks the ceiling BEFORE it allocates (or, for growth that
//! cannot be sized up front, every few thousand elements), and fails with an error that names
//! the operation, the ceiling and what was needed. Nothing is half-written: a refused open
//! changes nothing on disk, a refused load drops the half-built graph, a refused checkpoint
//! leaves the store as it was.
//!
//! This binary installs no tracking allocator: the checks run on the engine's estimate. Every
//! test lowers the ceiling for its own thread with the test-only
//! `graphersal::exec::memory_limit_scope` (never the process setting).
#![cfg(feature = "persist")]

use std::sync::Arc;

use graphersal::error::DiagnosticError;
use graphersal::exec::{self, memory_limit_scope};
use graphersal::persist::{self, PersistError, ReadOptions, RecoveryTarget, Store, StoreOptions};
use graphersal::prelude::*;

/// The packed snapshot of `graph` and the manifest's estimate of its memory.
fn packed(graph: &TraversalGraph) -> (Vec<u8>, u64) {
    let mut bytes = Vec::new();
    persist::write_snapshot(graph, &mut bytes, &persist::WriteOptions::new()).unwrap();
    let info = persist::read_snapshot_info(bytes.as_slice()).unwrap();
    (bytes, info.memory_bytes)
}

/// A graph of `n` vertices `person` with a name each.
fn people(n: usize) -> TraversalGraph {
    let mut graph = TraversalGraph::new();
    for i in 0..n {
        graph
            .traversal_mut()
            .add_v("person")
            .property("name", format!("person {i}"))
            .iterate()
            .unwrap();
    }
    graph
}

/// The `GraphError::ResourceLimitExceeded` fields of `err`: operation, max, observed, needed.
fn limit_of(err: &GraphError) -> (&'static str, Option<u64>, Option<u64>, Option<u64>) {
    match err {
        GraphError::ResourceLimitExceeded {
            operation,
            max,
            observed,
            needed,
            ..
        } => (operation, *max, *observed, *needed),
        other => panic!("expected ResourceLimitExceeded, got {other:?}"),
    }
}

/// A fresh directory under the system temp dir, removed on drop.
#[cfg(not(target_arch = "wasm32"))]
struct Temp(std::path::PathBuf);

#[cfg(not(target_arch = "wasm32"))]
impl Temp {
    fn new(name: &str) -> Self {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "graphersal-memcheck-{}-{}-{name}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn join(&self, part: &str) -> std::path::PathBuf {
        self.0.join(part)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Every file below `root` with its bytes, sorted by path (a byte-equal listing).
///
/// On Windows an open store holds an OS lock (the `LOCK` file of a directory store, the whole
/// file of a single-file store) that makes a read through another handle fail with raw OS error
/// 33 (`ERROR_LOCK_VIOLATION`) or 32 (`ERROR_SHARING_VIOLATION`); such a file is listed with its
/// size and modification time instead of its bytes. Every other read error panics.
#[cfg(not(target_arch = "wasm32"))]
fn listing(root: &std::path::Path) -> Vec<(String, Vec<u8>)> {
    fn contents(path: &std::path::Path) -> Vec<u8> {
        match std::fs::read(path) {
            Err(err) if cfg!(windows) && matches!(err.raw_os_error(), Some(32 | 33)) => {
                let meta = std::fs::metadata(path).unwrap();
                let modified = meta
                    .modified()
                    .unwrap()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos();
                format!("<os-locked: {} bytes, modified {modified}>", meta.len()).into_bytes()
            }
            read => read.unwrap(),
        }
    }
    fn walk(dir: &std::path::Path, base: &std::path::Path, out: &mut Vec<(String, Vec<u8>)>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(&path, base, out);
            } else {
                let name = path.strip_prefix(base).unwrap().display().to_string();
                out.push((name, contents(&path)));
            }
        }
    }
    let mut out = Vec::new();
    if root.is_dir() {
        walk(root, root, &mut out);
    } else {
        out.push((String::new(), contents(root)));
    }
    out
}

fn store_options() -> StoreOptions {
    StoreOptions::new().with_checkpoint_after_bytes(0)
}

#[test]
fn a_snapshot_over_the_ceiling_is_refused_before_any_element_is_read() {
    let (bytes, needed) = packed(&people(200));
    let refused = memory_limit_scope(needed - 1, || {
        persist::read_snapshot(bytes.as_slice(), &ReadOptions::new())
    });
    match refused {
        Err(PersistError::MemoryBudget {
            needed: got,
            limit,
            operation,
            in_use,
            setting,
            ..
        }) => {
            assert_eq!(got, needed);
            assert_eq!(limit, needed - 1);
            assert_eq!(operation, "snapshot load");
            assert_eq!(in_use, None, "estimated mode: no live heap");
            assert_eq!(setting, "memory.limit");
        }
        Err(other) => panic!("expected MemoryBudget, got {other:?}"),
        Ok(_) => panic!("the load was not refused"),
    }
    let err = memory_limit_scope(needed - 1, || {
        persist::read_snapshot(bytes.as_slice(), &ReadOptions::new())
            .err()
            .unwrap()
    });
    assert!(
        err.to_string()
            .contains(&format!("The snapshot load needs about {needed} bytes")),
        "{err}"
    );
    // With room for the estimate and the segment being decoded, the same load runs.
    let loaded = memory_limit_scope(needed * 2, || {
        persist::read_snapshot(bytes.as_slice(), &ReadOptions::new())
    })
    .map_err(|err| err.to_string())
    .unwrap();
    assert_eq!(loaded.vertex_count(), 200);
}

/// The estimated growth check of a load counts the graph once: the slots reserved up front are
/// part of the manifest's estimate, not of the empty graph's footprint, so a ceiling a tenth
/// above the estimate is enough (it needed about 1.4 times the estimate before).
#[test]
fn a_load_fits_under_a_ceiling_near_its_estimate() {
    let (bytes, needed) = packed(&people(5000));
    let loaded = memory_limit_scope(needed + needed / 10, || {
        persist::read_snapshot(bytes.as_slice(), &ReadOptions::new())
    })
    .map_err(|err| err.to_string())
    .unwrap();
    assert_eq!(loaded.vertex_count(), 5000);
}

#[test]
fn the_stricter_of_read_options_and_the_ceiling_bounds_a_load() {
    let (bytes, needed) = packed(&people(50));
    let setting_of = |result: Result<TraversalGraph, PersistError>| match result {
        Err(PersistError::MemoryBudget { setting, limit, .. }) => (setting, limit),
        Err(other) => panic!("expected MemoryBudget, got {other:?}"),
        Ok(_) => panic!("the load was not refused"),
    };
    // The read option is lower: it is named.
    let read = ReadOptions::new().with_memory_limit(needed / 4);
    let (setting, limit) = setting_of(memory_limit_scope(needed / 2, || {
        persist::read_snapshot(bytes.as_slice(), &read)
    }));
    assert_eq!(
        (setting, limit),
        ("ReadOptions::with_memory_limit", needed / 4)
    );
    // The ceiling is lower: it is named.
    let read = ReadOptions::new().with_memory_limit(needed / 2);
    let (setting, limit) = setting_of(memory_limit_scope(needed / 4, || {
        persist::read_snapshot(bytes.as_slice(), &read)
    }));
    assert_eq!((setting, limit), ("memory.limit", needed / 4));
    // Only the read option refuses (the ceiling fits).
    let (setting, _) = setting_of(memory_limit_scope(needed * 4, || {
        persist::read_snapshot(bytes.as_slice(), &read)
    }));
    assert_eq!(setting, "ReadOptions::with_memory_limit");
    // A larger read option does not lift the ceiling.
    let read = ReadOptions::new().with_memory_limit(needed * 4);
    let (setting, _) = setting_of(memory_limit_scope(needed / 2, || {
        persist::read_snapshot(bytes.as_slice(), &read)
    }));
    assert_eq!(setting, "memory.limit");
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn a_store_that_does_not_fit_refuses_the_open_and_stays_unchanged() {
    use graphersal::persist::{FsDir, MemDir, SingleFileDir, StoreDir};
    let temp = Temp::new("open");
    let (_, needed) = packed(&people(300));
    let backends: Vec<(Arc<dyn StoreDir>, Option<std::path::PathBuf>)> = vec![
        (
            Arc::new(FsDir::new(temp.join("dir"))),
            Some(temp.join("dir")),
        ),
        (Arc::new(MemDir::named("memcheck-open")), None),
        (
            Arc::new(SingleFileDir::new(temp.join("one.gstore"))),
            Some(temp.join("one.gstore")),
        ),
    ];
    for (dir, path) in backends {
        Store::create_with(&dir, people(300), store_options())
            .unwrap()
            .close()
            .unwrap();
        let before = path.as_deref().map(listing);
        let refused = memory_limit_scope(needed / 2, || Store::open(&dir, store_options()));
        match refused {
            Err(PersistError::MemoryBudget {
                operation,
                setting,
                needed: got,
                ..
            }) => {
                assert_eq!(operation, "store open");
                assert_eq!(setting, "memory.limit");
                assert!(got >= needed / 2, "{got} vs {needed}");
            }
            Err(other) => panic!("expected MemoryBudget, got {other:?}"),
            Ok(_) => panic!("the open was not refused"),
        }
        if let Some(path) = &path {
            assert!(before == Some(listing(path)), "the files changed");
        }
        // Without the lower ceiling it opens, writable, with every element.
        let store = Store::open(&dir, store_options()).unwrap();
        assert_eq!(store.graph().read().vertex_count(), 300);
        assert!(store.maintenance().is_none(), "never maintenance mode");
        store.close().unwrap();
    }
}

/// A store whose snapshot is small and whose WAL holds `commits` commits of three vertices.
fn store_with_a_large_wal(dir: &Arc<dyn persist::StoreDir>, commits: usize) {
    let store = Store::create_with(dir, TraversalGraph::new(), store_options()).unwrap();
    for i in 0..commits {
        let mut graph = store.graph().write();
        let g = graph.traversal_mut();
        g.add_v("person")
            .property("name", format!("a{i}"))
            .add_v("person")
            .property("name", format!("b{i}"))
            .add_v("person")
            .property("name", format!("c{i}"))
            .iterate()
            .unwrap();
    }
    store.close().unwrap();
}

#[test]
fn a_wal_that_grows_past_the_ceiling_fails_the_open() {
    let dir: Arc<dyn persist::StoreDir> = Arc::new(persist::MemDir::named("memcheck-wal"));
    store_with_a_large_wal(&dir, 2000);
    // The empty snapshot fits; 6000 replayed vertices (at least 128 bytes each) do not.
    let refused = memory_limit_scope(300_000, || Store::open(&dir, store_options()));
    match refused {
        Err(PersistError::Graph(err)) => {
            let (operation, max, observed, needed) = limit_of(&err);
            assert_eq!(operation, "store open");
            assert_eq!(max, Some(300_000));
            assert!(observed.is_some_and(|o| o > 300_000), "{observed:?}");
            assert_eq!(needed, None, "stopped while it grew");
            let help = err.help().unwrap();
            assert!(help.contains("The graph does not fit"), "{help}");
        }
        Err(other) => panic!("expected Graph(ResourceLimitExceeded), got {other:?}"),
        Ok(_) => panic!("the open was not refused"),
    }
    let store = Store::open(&dir, store_options()).unwrap();
    assert_eq!(store.graph().read().vertex_count(), 6000);
    assert!(store.maintenance().is_none());
    store.close().unwrap();
}

#[test]
fn recovery_over_the_ceiling_fails_and_names_recovery() {
    let dir: Arc<dyn persist::StoreDir> = Arc::new(persist::MemDir::named("memcheck-recovery"));
    store_with_a_large_wal(&dir, 2000);
    let refused = memory_limit_scope(300_000, || {
        Store::open_read_only(&dir, RecoveryTarget::Latest)
    });
    match refused {
        Err(PersistError::Graph(err)) => {
            assert_eq!(limit_of(&err).0, "recovery");
            assert!(
                err.to_string().contains("exceeded by the recovery"),
                "{err}"
            );
        }
        Err(other) => panic!("expected Graph(ResourceLimitExceeded), got {other:?}"),
        Ok(_) => panic!("the recovery was not refused"),
    }
    let recovered = Store::open_read_only(&dir, RecoveryTarget::Latest).unwrap();
    assert_eq!(recovered.graph.vertex_count(), 6000);
}

/// GraphSON text of `n` vertices (the line form).
#[cfg(feature = "io")]
fn graphson_text(n: usize) -> Vec<u8> {
    let mut out = Vec::new();
    graphersal::io::GraphExport::export_graphson_writer(&people(n), &mut out).unwrap();
    out
}

#[test]
#[cfg(feature = "io")]
fn a_graphson_load_into_a_new_graph_stops_at_the_ceiling() {
    let text = graphson_text(5000);
    let limit = (text.len() / 4) as u64;
    let refused = memory_limit_scope(limit, || GraphSource::from_graphson_reader(text.as_slice()));
    match refused {
        Err(graphersal::error::IOError::Graph(err)) => {
            let (operation, max, _, needed) = limit_of(&err);
            assert_eq!(operation, "GraphSON load");
            assert_eq!(max, Some(limit));
            assert_eq!(needed, None);
        }
        Err(other) => panic!("expected IOError::Graph, got {other:?}"),
        Ok(_) => panic!("the load was not stopped"),
    }
    // The estimate counts the text read: far above it the load runs.
    let graph = memory_limit_scope(text.len() as u64 * 4 + (1 << 20), || {
        GraphSource::from_graphson_reader(text.as_slice())
    })
    .unwrap();
    assert_eq!(graph.read().vertex_count(), 5000);
}

#[test]
#[cfg(feature = "io")]
fn a_graphml_load_into_a_new_graph_stops_at_the_ceiling() {
    use graphersal::io::GraphExport;
    let mut text = Vec::new();
    people(5000).export_graphml_writer(&mut text).unwrap();
    let limit = (text.len() / 4) as u64;
    let refused = memory_limit_scope(limit, || GraphSource::from_graphml_reader(text.as_slice()));
    match refused {
        Err(graphersal::error::IOError::Graph(err)) => {
            assert_eq!(limit_of(&err).0, "GraphML load");
        }
        Err(other) => panic!("expected IOError::Graph, got {other:?}"),
        Ok(_) => panic!("the load was not stopped"),
    }
    let graph = memory_limit_scope(text.len() as u64 * 4 + (1 << 20), || {
        GraphSource::from_graphml_reader(text.as_slice())
    })
    .unwrap();
    assert_eq!(graph.read().vertex_count(), 5000);
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn a_checkpoint_that_does_not_fit_fails_and_leaves_the_store_unchanged() {
    let temp = Temp::new("checkpoint");
    for (name, merge_bytes) in [("merge", None), ("from-graph", Some(1))] {
        let path = temp.join(name);
        let dir: Arc<dyn persist::StoreDir> = Arc::new(persist::FsDir::new(&path));
        let mut options = store_options();
        if let Some(bytes) = merge_bytes {
            // Beyond this WAL size the live graph is encoded instead of merged.
            options = options.with_checkpoint_merge_bytes(bytes);
        }
        let store = Store::create_with(&dir, people(100), options.clone()).unwrap();
        for i in 0..1500 {
            store
                .graph()
                .write()
                .traversal_mut()
                .add_v("person")
                .property("name", format!("late {i}"))
                .add_v("person")
                .property("name", format!("later {i}"))
                .add_v("person")
                .property("name", format!("latest {i}"))
                .iterate()
                .unwrap();
        }
        let snapshots = store.info().unwrap().snapshots.len();
        let before = listing(&path);
        let refused = memory_limit_scope(100_000, || store.checkpoint(None));
        match refused {
            Err(PersistError::Graph(err)) => {
                assert_eq!(limit_of(&err).0, "checkpoint", "{name}: {err}");
            }
            Err(other) => panic!("{name}: expected Graph(ResourceLimitExceeded), got {other:?}"),
            Ok(_) => panic!("{name}: the checkpoint was not refused"),
        }
        assert_eq!(store.info().unwrap().snapshots.len(), snapshots, "{name}");
        assert!(before == listing(&path), "{name}: the files changed");
        assert!(
            store.damage_found().unwrap().is_empty(),
            "{name}: not damage"
        );
        // The store goes on: a write, and the checkpoint without the lower ceiling.
        store
            .graph()
            .write()
            .traversal_mut()
            .add_v("person")
            .iterate()
            .unwrap();
        store.checkpoint(None).unwrap();
        store.close().unwrap();
    }
}

#[test]
#[cfg(all(feature = "persist-zip", not(target_arch = "wasm32")))]
fn a_backup_zip_verify_over_the_ceiling_fails_cleanly() {
    let dir: Arc<dyn persist::StoreDir> = Arc::new(persist::MemDir::named("memcheck-zip"));
    let store = Store::create_with(&dir, people(10), store_options()).unwrap();
    let mut archive = std::io::Cursor::new(Vec::new());
    let refused = memory_limit_scope(16, || store.backup_zip(&mut archive));
    match refused {
        Err(PersistError::Graph(err)) => {
            assert_eq!(limit_of(&err).0, "backup verify", "{err}");
        }
        Err(other) => panic!("expected Graph(ResourceLimitExceeded), got {other:?}"),
        Ok(_) => panic!("the backup was not refused"),
    }
    assert!(store.damage_found().unwrap().is_empty(), "not damage");
    let mut archive = std::io::Cursor::new(Vec::new());
    store.backup_zip(&mut archive).unwrap();
    store.close().unwrap();
}

#[cfg(feature = "script")]
fn run_script(
    graph: &Arc<Graph>,
    script: &str,
) -> Result<graphersal::script::rhai::Dynamic, String> {
    crate::script_host::eval_value_with_limits(
        graph.clone(),
        script,
        std::collections::HashMap::new(),
        &crate::script_host::ScriptLimits::unlimited(),
    )
    .map_err(|err| err.to_string())
}

#[test]
#[cfg(feature = "script")]
fn product_is_checked_before_it_is_built() {
    let graph = Arc::new(GraphSource::empty());
    let small = "g.inject([1, 2, 3]).product([\"a\", \"b\"]).next()";
    assert!(run_script(&graph, small).is_ok());
    let big = "let xs = []; for i in 0..1000 { xs.push(i); } \
               g.inject(xs).product(xs).count(Scope.local).next()";
    // The per-query limit.
    let limited = big.replace(
        "g.inject(xs)",
        "g.with(\"traversal.max_materialized_bytes\", 100000).inject(xs)",
    );
    let err = run_script(&graph, &limited).unwrap_err();
    assert!(
        err.contains("Resource limit 'traversal.max_materialized_bytes' exceeded"),
        "{err}"
    );
    // The process ceiling.
    let err = memory_limit_scope(1_000_000, || run_script(&graph, big)).unwrap_err();
    assert!(
        err.contains("Resource limit 'memory.limit' exceeded"),
        "{err}"
    );
    // Without either bound it is built.
    let count = run_script(&graph, big).unwrap();
    assert_eq!(count.as_int().unwrap(), 1_000_000);
}

#[test]
#[cfg(feature = "script")]
fn product_obeys_the_evaluation_timeout() {
    let graph = Arc::new(GraphSource::empty());
    let script = "let xs = []; for i in 0..1500 { xs.push(i); } \
                  g.with(\"evaluationTimeout\", 1).inject(xs).product(xs).count(Scope.local).next()";
    let err = run_script(&graph, script).unwrap_err();
    assert!(
        err.contains("Timeout") || err.contains("timed out") || err.contains("evaluationTimeout"),
        "{err}"
    );
}

#[test]
fn check_memory_for_reports_the_operation_and_what_is_needed() {
    memory_limit_scope(10_000, || {
        exec::check_memory_for("model load", 10_000).unwrap();
        let err = exec::check_memory_for("model load", 10_001).unwrap_err();
        assert_eq!(
            limit_of(&err),
            ("model load", Some(10_000), Some(10_001), Some(10_001))
        );
        let text = err.to_string();
        assert!(text.contains("exceeded by the model load"), "{text}");
        assert!(text.contains("it needs 10001 more bytes"), "{text}");
        let help = err.help().unwrap();
        assert!(
            help.contains("The model load needs about 10001 more bytes"),
            "{help}"
        );
        assert!(
            help.contains("raise 'memory.limit' to at least 10001 bytes"),
            "{help}"
        );
        let room = exec::memory_room().unwrap();
        assert_eq!(room.limit(), Some(10_000));
        assert_eq!(room.live_bytes(), None, "no tracking allocator here");
        assert_eq!(room.available(), Some(10_000));
    });
}
