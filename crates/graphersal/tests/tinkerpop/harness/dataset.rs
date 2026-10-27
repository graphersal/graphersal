//! Graph loading for the TinkerPop feature suite.
//!
//! - `modern` and `grateful` are imported from the vendored GraphML files (`tests/tinkerpop/data/`),
//!   **not** from `GraphSource::tinkerpop_modern()`: the builder's edge ids are `"0"`…`"5"`, while
//!   upstream (and the vendored `tinkerpop-modern.xml`) uses `"7"`…`"12"`.
//! - `sink` is imported from the vendored GraphSON 3.0 file (`tinkerpop-sink.json`).
//! - `crew` exists only as GraphSON upstream and is importable, but its scenarios are deliberate
//!   incompatibilities (group `multi-meta-properties` in `scope.rs`), excluded before they run:
//!   crew is the multi-/meta-property dataset, and Graphersal has no list cardinality (the
//!   GraphSON import maps several entries to one array and meta-properties to `_meta`). Loading
//!   it still yields [`CREW_MULTI_PROPERTIES`], the guard for a crew scenario that is not listed
//!   there (e.g. one added by a suite upgrade).
//! - `empty` is a fresh empty graph every time.
//!
//! Each file dataset has one shared **template** per process, created lazily and cached in a
//! `OnceLock` (read-only scenarios run on it), and a way to obtain a **fresh, independent copy**
//! ([`fresh`], for mutating scenarios). `TraversalGraph` is not `Clone`, so a copy is a re-import
//! of the file bytes, which are read from disk once and cached. [`reload_template`] replaces a
//! template that a scenario changed by accident.
//!
//! Every graph is built on the storage that `GRAPHERSAL_TEST_STORAGE` selects
//! ([`super::storage`]) and handed out as `Arc<dyn AnyGraph>`.

use std::sync::{Arc, Mutex, OnceLock};

use graphersal::GraphSource;
use graphersal::io::GraphImport;
use graphersal::storage::AnyGraph;

use super::storage;

use super::gherkin::GraphSetup;

/// Why a scenario cannot run on its dataset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkipReason(pub String);

/// Skip reason for the `crew` dataset (multi- and meta-properties).
pub const CREW_MULTI_PROPERTIES: &str =
    "crew needs multi-/meta-properties (list cardinality), which Graphersal does not have";
/// Skip reason for `Given an unsupported test`.
pub const UNSUPPORTED: &str = "unsupported test (upstream)";

/// One file dataset (GraphML, or GraphSON for `.json`): its file, the cached bytes and the shared
/// template.
struct Dataset {
    file: &'static str,
    bytes: OnceLock<Result<Vec<u8>, String>>,
    template: OnceLock<Mutex<Result<Arc<dyn AnyGraph>, String>>>,
}

impl Dataset {
    const fn new(file: &'static str) -> Self {
        Self {
            file,
            bytes: OnceLock::new(),
            template: OnceLock::new(),
        }
    }

    fn bytes(&self) -> Result<&[u8], String> {
        self.bytes
            .get_or_init(|| {
                let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/tinkerpop/data")
                    .join(self.file);
                std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))
            })
            .as_ref()
            .map(Vec::as_slice)
            .map_err(Clone::clone)
    }

    /// Imports a new, independent graph from the cached file bytes.
    fn import(&self) -> Result<Arc<dyn AnyGraph>, String> {
        let bytes = self.bytes()?;
        let mut graph = GraphSource::empty();
        if self.file.ends_with(".json") {
            graph
                .import_graphson_reader(bytes)
                .map_err(|e| format!("import {}: {e}", self.file))?;
        } else {
            graph
                .import_graphml_reader(bytes)
                .map_err(|e| format!("import {}: {e}", self.file))?;
        }
        Ok(storage::wrap(graph.into_inner()))
    }

    fn template_slot(&self) -> &Mutex<Result<Arc<dyn AnyGraph>, String>> {
        self.template.get_or_init(|| Mutex::new(self.import()))
    }

    fn template(&self) -> Result<Arc<dyn AnyGraph>, String> {
        let slot = self.template_slot();
        let guard = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.clone()
    }

    fn reload(&self) {
        let fresh = self.import();
        let slot = self.template_slot();
        let mut guard = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = fresh;
    }
}

static MODERN: Dataset = Dataset::new("tinkerpop-modern.xml");
static GRATEFUL: Dataset = Dataset::new("grateful-dead.xml");
static SINK: Dataset = Dataset::new("tinkerpop-sink.json");

/// The file dataset behind `setup`, or why there is none.
fn dataset(setup: GraphSetup) -> Result<Option<&'static Dataset>, SkipReason> {
    match setup {
        GraphSetup::Modern => Ok(Some(&MODERN)),
        GraphSetup::Grateful => Ok(Some(&GRATEFUL)),
        GraphSetup::Empty => Ok(None),
        GraphSetup::Crew => Err(SkipReason(CREW_MULTI_PROPERTIES.to_string())),
        GraphSetup::Sink => Ok(Some(&SINK)),
        GraphSetup::Unsupported => Err(SkipReason(UNSUPPORTED.to_string())),
    }
}

/// The shared template of `setup` (read-only use only); `Empty` yields a fresh empty graph.
///
/// A dataset whose file cannot be imported is reported as a skip with the import error, so the
/// suite keeps running and the problem is visible in the histogram.
pub fn load(setup: GraphSetup) -> Result<Arc<dyn AnyGraph>, SkipReason> {
    match dataset(setup)? {
        Some(ds) => ds.template().map_err(SkipReason),
        None => Ok(storage::wrap(Default::default())),
    }
}

/// A fresh copy of `setup`'s dataset that no other scenario sees.
pub fn fresh(setup: GraphSetup) -> Result<Arc<dyn AnyGraph>, SkipReason> {
    match dataset(setup)? {
        Some(ds) => ds.import().map_err(SkipReason),
        None => Ok(storage::wrap(Default::default())),
    }
}

/// Replaces the shared template of `setup` with a freshly imported one (after a scenario changed
/// it). A no-op for datasets without a template.
pub fn reload_template(setup: GraphSetup) {
    if let Ok(Some(ds)) = dataset(setup) {
        ds.reload();
    }
}

/// `(vertex_count, edge_count)` of `graph`.
///
/// Read from the storage's statistics (one call through `AnyGraph`); a storage without them
/// reports `(0, 0)`, which the template-change check then cannot see.
pub fn counts(graph: &Arc<dyn AnyGraph>) -> (usize, usize) {
    graph
        .statistics()
        .unwrap()
        .map_or((0, 0), |s| (s.vertex_count(), s.edge_count()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use graphersal::prelude::*;

    fn ids(graph: &Arc<dyn AnyGraph>, script: &str) -> Vec<String> {
        let mut scope = graphersal::script::graph_scope_with_options(
            graph.clone(),
            &[],
            std::sync::Arc::new(graphersal::auth::AllowAll),
        );
        let value: graphersal::script::rhai::Dynamic = graphersal::script::engine_with_limits(
            &graphersal::script::ScriptLimits::default(),
            std::sync::Arc::new(graphersal::auth::AllowAll),
        )
        .eval_with_scope(&mut scope, script)
        .expect("script evaluates");
        value
            .cast::<graphersal::script::ScriptGraphTraversalSource>()
            .to_typed_list()
            .expect("traversal executes")
            .into_iter()
            .map(|v| match v {
                ElementProperty::String(s) => s.to_string(),
                other => panic!("id is not a string: {other:?}"),
            })
            .collect()
    }

    #[test]
    fn modern_loads_with_upstream_ids() {
        let graph = load(GraphSetup::Modern).expect("modern loads");
        assert_eq!(counts(&graph), (6, 6));
        let mut edge_ids = ids(&graph, "g.E().id()");
        edge_ids.sort_by_key(|id| id.parse::<u32>().unwrap_or(u32::MAX));
        assert_eq!(edge_ids, ["7", "8", "9", "10", "11", "12"]);
        assert_eq!(ids(&graph, r#"g.V().has("name", "marko").id()"#), ["1"]);
    }

    #[test]
    fn grateful_loads() {
        let graph = load(GraphSetup::Grateful).expect("grateful loads");
        assert_eq!(counts(&graph).0, 808);
    }

    #[test]
    fn fresh_copy_is_independent_of_the_template() {
        let template = load(GraphSetup::Modern).expect("modern loads");
        let copy = fresh(GraphSetup::Modern).expect("modern copy");
        assert!(!Arc::ptr_eq(&template, &copy));
        ids(&copy, "g.V().drop()");
        assert_eq!(counts(&copy), (0, 0));
        assert_eq!(counts(&template), (6, 6));
    }

    #[test]
    fn empty_is_fresh_crew_is_skipped_and_sink_loads() {
        let a = load(GraphSetup::Empty).expect("empty");
        let b = load(GraphSetup::Empty).expect("empty");
        assert!(!Arc::ptr_eq(&a, &b));
        assert_eq!(counts(&a), (0, 0));
        assert_eq!(
            load(GraphSetup::Crew).err(),
            Some(SkipReason(CREW_MULTI_PROPERTIES.into()))
        );
        assert_eq!(
            fresh(GraphSetup::Crew).err(),
            Some(SkipReason(CREW_MULTI_PROPERTIES.into()))
        );
        // `sink` is imported from GraphSON: three vertices, its self-loop once.
        let sink = fresh(GraphSetup::Sink).expect("sink");
        assert_eq!(counts(&sink), (3, 3));
        assert_eq!(
            load(GraphSetup::Unsupported).err(),
            Some(SkipReason(UNSUPPORTED.into()))
        );
    }
}
