//! The script traversal sources: the `GraphSource` factory (`ScriptGraphSource`) and the step
//! builder `ScriptGraphTraversalSource` with its options and graph I/O methods. The step builder
//! methods live in the child modules, split by step family.

use super::*;

mod dates;
mod filter_map;
#[cfg(feature = "gql")]
mod gql;
#[cfg(feature = "gql")]
pub(crate) use gql::gql_not_at_start_error;
mod navigation;
mod side_effects;
mod vectors;
pub(crate) use vectors::script_embed;

/// Script-facing `GraphSource` factory (`GraphSource::empty()`, `GraphSource::tinkerpop_modern()`,
/// ...; also reachable as a bare global function, e.g. `tinkerpop_modern()`), registered with Rhai
/// as `GraphSource`. In a normal session (`graphersal`, the playground, `eval_value`/`graph_scope`) no
/// scope variable is named `GraphSource`, so the dot form `GraphSource.tinkerpop_modern()` fails
/// with `Variable not found: GraphSource` — only `GraphSource::tinkerpop_modern()` (the `::`
/// static-module form) and the bare global `tinkerpop_modern()` work there. The dot form is only
/// reachable when a host explicitly pushes a `ScriptGraphSource` value into the scope under the
/// name `"GraphSource"` (`scope.push("GraphSource", ScriptGraphSource::default())`), which no
/// built-in session does; see `tests/all/file_tree_sample_tests.rs`'s
/// `file_tree_is_reachable_three_ways_from_rhai` for that one demonstrated case.
#[derive(Clone, Copy, Debug, Default)]
pub struct ScriptGraphSource {}

impl ScriptGraphSource {
    /// `GraphSource::empty()`: a new, empty graph, private to the script (writing its data or
    /// schema asks no authorizer). Called from Rust it allows everything; the DSL's
    /// `GraphSource::empty()` carries the session's authorizer from the engine.
    pub fn empty() -> ScriptGraphTraversalSource {
        ScriptGraphTraversalSource {
            g: Some(Arc::new(GraphSource::empty())),
            steps: vec![],
            options: std::collections::HashMap::new(),
            access: Arc::default(),
            private: true,
            anonymous: false,
            now: None,
        }
    }

    /// Wraps an already-constructed shared graph of any storage (an `Arc<Graph<S>>` or an
    /// `Arc<dyn AnyGraph>`, see [`IntoAnyGraph`]) as a traversal source that allows
    /// everything. A host builds its session's `g` with
    /// [`graph_scope_with_options`](crate::script::graph_scope_with_options), which attaches
    /// the [`Authorizer`](crate::graph::Authorizer) it passes.
    pub fn graph(g: impl IntoAnyGraph) -> ScriptGraphTraversalSource {
        ScriptGraphTraversalSource {
            g: Some(g.into_any_graph()),
            steps: vec![],
            options: std::collections::HashMap::new(),
            access: Arc::default(),
            private: false,
            anonymous: false,
            now: None,
        }
    }

    /// `GraphSource::tinkerpop_modern()`: the standard TinkerPop "modern" toy graph.
    pub fn tinkerpop_modern() -> ScriptGraphTraversalSource {
        ScriptGraphTraversalSource {
            g: Some(Arc::new(GraphSource::tinkerpop_modern())),
            steps: vec![],
            options: std::collections::HashMap::new(),
            access: Arc::default(),
            private: true,
            anonymous: false,
            now: None,
        }
    }

    /// `GraphSource::file_tree()`: a small, deterministic, filesystem-like sample tree
    /// (see `TraversalGraph::file_tree`).
    pub fn file_tree() -> ScriptGraphTraversalSource {
        ScriptGraphTraversalSource {
            g: Some(Arc::new(GraphSource::file_tree())),
            steps: vec![],
            options: std::collections::HashMap::new(),
            access: Arc::default(),
            private: true,
            anonymous: false,
            now: None,
        }
    }

    /// Placeholder for a `GraphSource.set_visualizer(...)` call on the template (uninstantiated)
    /// source, reachable only when a host pushes a `ScriptGraphSource` into the scope under the
    /// name `"GraphSource"` (see [`ScriptGraphSource`]); a no-op.
    pub fn set_visualizer_from_source(_source: &mut ScriptGraphSource, _format: VisualizeFormat) {}

    /// `GraphSource::file(path)`: loads a graph file, choosing the importer from its extension.
    /// The graph is private to the script. Called from Rust it allows everything; the DSL's
    /// `GraphSource::file(path)` asks the session's authorizer for `Read` on the `File`.
    #[cfg(feature = "io")]
    pub fn file(file: String) -> Result<ScriptGraphTraversalSource, Box<rhai::EvalAltResult>> {
        Self::file_with(Arc::default(), file)
    }

    #[cfg(feature = "io")]
    fn file_with(
        access: Arc<access::ScriptAccess>,
        file: String,
    ) -> Result<ScriptGraphTraversalSource, Box<rhai::EvalAltResult>> {
        let path = access
            .restrictions(false)
            .authorize_file(
                "GraphSource::file",
                crate::graph::Action::Read,
                std::path::Path::new(&file),
            )
            .map_err(script_error)?;
        match GraphSource::from_file(&path) {
            Ok(graph) => Ok(ScriptGraphSource::graph(Arc::new(graph)).with_access(access, true)),
            Err(err) => Err(Box::new(EvalAltResult::ErrorRuntime(
                rhai::Dynamic::from(crate::error::format_diagnostic(&err)),
                rhai::Position::NONE,
            ))),
        }
    }

    /// The DSL's `GraphSource::file(path)`, with the session's authorizer from `ctx`.
    #[cfg(feature = "io")]
    pub(crate) fn file_in(
        ctx: &NativeCallContext,
        file: String,
    ) -> Result<ScriptGraphTraversalSource, Box<rhai::EvalAltResult>> {
        Self::file_with(access::ScriptAccess::of_context(ctx), file)
    }

    /// The DSL's `GraphSource::empty()`: a private graph with the session's authorizer from `ctx`.
    pub(crate) fn empty_in(ctx: &NativeCallContext) -> ScriptGraphTraversalSource {
        Self::empty().with_access(access::ScriptAccess::of_context(ctx), true)
    }

    /// The DSL's `GraphSource::tinkerpop_modern()` (see [`Self::empty_in`]).
    pub(crate) fn tinkerpop_modern_in(ctx: &NativeCallContext) -> ScriptGraphTraversalSource {
        Self::tinkerpop_modern().with_access(access::ScriptAccess::of_context(ctx), true)
    }

    /// The DSL's `GraphSource::file_tree()` (see [`Self::empty_in`]).
    pub(crate) fn file_tree_in(ctx: &NativeCallContext) -> ScriptGraphTraversalSource {
        Self::file_tree().with_access(access::ScriptAccess::of_context(ctx), true)
    }

    /// A graph the script built (`schema.to_graph()`), private, with the authorizer from `ctx`.
    pub(crate) fn private_graph_in(
        ctx: &NativeCallContext,
        graph: TraversalGraph,
    ) -> ScriptGraphTraversalSource {
        Self::graph(Arc::new(Graph::new(graph)))
            .with_access(access::ScriptAccess::of_context(ctx), true)
    }
}

/// The explicit target graph of a script `withSideEffect(name, graph)` step (a `Debug` shell
/// around the shared graph handle, whose graph type has no `Debug`), and whether it is a graph
/// the script built itself (a `subgraph()` into a host graph asks for `Create` on `Data`).
/// Always a [`Graph<TraversalGraph>`](Graph): `subgraph()` writes through the engine's
/// `TraversalGraph` target, so a graph of another storage is refused when the step is built.
#[derive(Clone)]
pub(crate) struct ScriptGraphTarget {
    pub(crate) graph: Arc<Graph>,
    pub(crate) private: bool,
}

impl std::fmt::Debug for ScriptGraphTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<graph>")
    }
}

/// Script-facing traversal builder: wraps the Rust `GraphTraversalSource` for the Rhai DSL,
/// accumulating steps until a terminal (`to_list()`/`next()`/...) executes them.
///
/// It holds its graph as `Arc<dyn AnyGraph>`, so one DSL serves every storage: the host's `g`
/// may be any [`Graph<S>`](Graph), while the graphs a script builds itself (`GraphSource::*`,
/// `__`, `_g`, `subgraph()`/`cap()`/`to_graph()` results, `schema.to_graph()`) are always
/// `TraversalGraph`s. A terminal is one [`AnyGraph`] call, which runs the engine for the
/// concrete storage.
#[derive(Clone)]
pub struct ScriptGraphTraversalSource {
    pub(super) g: Option<Arc<dyn AnyGraph>>,
    pub(super) steps: Vec<Step>,
    pub(super) options: std::collections::HashMap<CompactString, ElementProperty>,
    /// The session's access (the host's authorizer and preset options); everything allowed by
    /// default.
    pub(super) access: Arc<access::ScriptAccess>,
    /// Whether `g` is a graph the script built itself (`GraphSource::empty()`, a `subgraph()`
    /// result, the scratch sources `__` and `_g`): writing to it is not a write to the host
    /// graph, so writing its data or schema asks no authorizer.
    pub(super) private: bool,
    /// Whether this is the anonymous spawn `__` (or a traversal started from it): a saved
    /// query cannot be called on it (`__.query(..)` fails with a clear error instead of
    /// looking the query up in the scratch graph).
    pub(super) anonymous: bool,
    /// The evaluation's `now()` when a creating step (`addV`, `addE`, `mergeV`, `mergeE`) was
    /// added in a script: the time of `defaultFunction` `now`/`today` for the elements its
    /// creating steps make, so it equals `now()` of the same evaluation (and a host's
    /// `pin_now`). `None`: the execution's time (the unit's start).
    pub(super) now: Option<crate::value::temporal::DateTime>,
}

impl fmt::Debug for ScriptGraphTraversalSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.steps)
    }
}

/// The traversal as an anonymous traversal in the DSL (`__.out().has("name", "x")`): the steps
/// lowered once and rendered in the canonical step text of `.profile()`. A script sees it for an
/// anonymous traversal (`__.out()`) as a final value, through `to_string()` and in `${..}`.
impl crate::value::literal::TokenLiteral for ScriptGraphTraversalSource {
    fn token_literal(&self) -> String {
        self.ast_convert_to_anonymous::<crate::graph::TraversalGraph>()
            .to_string()
    }
}

impl ScriptGraphTraversalSource {
    /// `g.import_graphml(path)`: imports GraphML into this source's graph. Asks for `Read` on the
    /// `File` (resolved under the host's file root, if any) and `Create` on `Data`.
    #[cfg(feature = "io")]
    pub fn import_graphml(&mut self, path: String) -> Result<String, Box<rhai::EvalAltResult>> {
        self.require(
            "import_graphml",
            crate::graph::AccessRequest::new(
                crate::graph::Action::Create,
                crate::graph::Resource::data(None, None),
            ),
        )?;
        let file = self.require_file("import_graphml", crate::graph::Action::Read, &path)?;
        if let Some(ref g) = self.g {
            open_for_import(&file)
                .and_then(|mut reader| g.import_graphml(&mut reader, Some(&self.access.presets)))
                .map_err(|err| {
                    Box::new(rhai::EvalAltResult::ErrorRuntime(
                        rhai::Dynamic::from(crate::error::format_diagnostic(&err)),
                        rhai::Position::NONE,
                    ))
                })?;
            Ok(format!("Imported GraphML from {}", path))
        } else {
            Err(Box::new(rhai::EvalAltResult::ErrorRuntime(
                rhai::Dynamic::from("No graph associated with traversal source to import into"),
                rhai::Position::NONE,
            )))
        }
    }

    /// `g.import_graphson(path)`: imports GraphSON (line or wrapped
    /// form) into this source's graph as one unit and returns the import report as text (counts,
    /// then one line per kind of skipped or converted data). Asks for `Read` on the `File`
    /// (resolved under the host's file root, if any) and `Create` on `Data`.
    #[cfg(feature = "io")]
    pub fn import_graphson(&mut self, path: String) -> Result<String, Box<rhai::EvalAltResult>> {
        self.require(
            "import_graphson",
            crate::graph::AccessRequest::new(
                crate::graph::Action::Create,
                crate::graph::Resource::data(None, None),
            ),
        )?;
        let file = self.require_file("import_graphson", crate::graph::Action::Read, &path)?;
        if let Some(ref g) = self.g {
            let report = open_for_import(&file)
                .and_then(|mut reader| g.import_graphson(&mut reader, Some(&self.access.presets)))
                .map_err(|err| {
                    Box::new(rhai::EvalAltResult::ErrorRuntime(
                        rhai::Dynamic::from(crate::error::format_diagnostic(&err)),
                        rhai::Position::NONE,
                    ))
                })?;
            Ok(format!("Imported GraphSON from {path}: {report}"))
        } else {
            Err(Box::new(rhai::EvalAltResult::ErrorRuntime(
                rhai::Dynamic::from("No graph associated with traversal source to import into"),
                rhai::Position::NONE,
            )))
        }
    }

    /// `g.with(key, value)` must behave like every other traversal-source builder call here
    /// (`add_step` et al.): clone first, mutate the clone, leave `self` untouched. `g` in the
    /// REPL/CLI scope is one persistent Rhai variable for the whole session
    /// (`graph_scope_with_options` pushes it once) — a Rhai method taking `&mut self` mutates
    /// that actual variable, not a temporary, so mutating `self.options` in place here (as a
    /// prior version of this method did) permanently poisoned `g` for every later statement in
    /// the same session, even ones that never called `.with()` at all. [`Self::set_visualizer`]
    /// is the one place that *does* want that persistence and mutates `self.options` directly for
    /// it, deliberately bypassing this method rather than reusing it.
    ///
    /// The value is a script value handed to the engine, bounded by
    /// [`ScriptLimits::max_value_depth`].
    pub fn with(
        ctx: NativeCallContext,
        this: &mut Self,
        key: String,
        value: rhai::Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let value = value_arg(&ctx, value)?;
        Ok(this.with_value(key, value))
    }

    /// [`Self::with`] with an engine value (no conversion).
    pub(crate) fn with_value(&mut self, key: String, value: ElementProperty) -> Self {
        let mut res = self.clone();
        res.options.insert(CompactString::new(key), value);
        res
    }

    /// `with(WithOptions.tokens)`: the step-level `with()` modulator, which configures the step
    /// before it (unlike `with(key, value)` with a string key, an execution option); see
    /// [`crate::GraphTraversalSource::with_option`].
    pub fn with_option_flag(&mut self, key: crate::value::WithOptions) -> Self {
        self.add_step(Step::WithOption(key, None))
    }

    /// `with(WithOptions.tokens, WithOptions.ids)` / `with(WithOptions.indexer,
    /// WithOptions.map)`; see [`Self::with_option_flag`].
    pub fn with_option_value(
        &mut self,
        key: crate::value::WithOptions,
        value: crate::value::WithOptions,
    ) -> Self {
        self.add_step(Step::WithOption(key, Some(value)))
    }

    /// Rhai binding for `GraphStorage::memory_usage()`. Returns a Rhai map with keys
    /// `structure_bytes`, `index_bytes`, `property_bytes`, `total_bytes`, `total_mb` and the
    /// compressed-property counters `compressed_values`, `compressed_plain_bytes`,
    /// `compressed_stored_bytes`, `compression_dictionary_bytes`, the `dedup` rules' counters
    /// `deduped_values`, `deduped_plain_bytes` and `dedup_table_bytes` (part of
    /// `property_bytes`), the property indexes' `property_index_bytes` and the shared property
    /// key lists' `shape_bytes` (part of `property_bytes`) when the backing storage supports
    /// introspection (`graphersal::TraversalGraph` always does), or `()` (Rhai unit) otherwise. This is a plain method call, not a traversal step — it is not
    /// chained with `.to_list()`/`.next()`. Asks for `Read` on `Data`.
    pub fn memory_usage(&mut self) -> Result<rhai::Dynamic, Box<rhai::EvalAltResult>> {
        self.require(
            "memory_usage",
            crate::graph::AccessRequest::new(
                crate::graph::Action::Read,
                crate::graph::Resource::data(None, None),
            ),
        )?;
        let Some(g) = self.g.as_ref() else {
            return Ok(Dynamic::UNIT);
        };
        let Some(stats) = g.memory_usage() else {
            return Ok(Dynamic::UNIT);
        };
        let mut map = rhai::Map::new();
        map.insert(
            "structure_bytes".into(),
            Dynamic::from_int(stats.structure_bytes as i64),
        );
        map.insert(
            "index_bytes".into(),
            Dynamic::from_int(stats.index_bytes as i64),
        );
        map.insert(
            "property_bytes".into(),
            Dynamic::from_int(stats.property_bytes as i64),
        );
        map.insert(
            "total_bytes".into(),
            Dynamic::from_int(stats.total_bytes() as i64),
        );
        let int = |n: u64| Dynamic::from_int(i64::try_from(n).unwrap_or(i64::MAX));
        map.insert("workload_bytes".into(), int(stats.workload_bytes));
        map.insert("compressed_values".into(), int(stats.compressed_values));
        map.insert(
            "compressed_plain_bytes".into(),
            int(stats.compressed_plain_bytes),
        );
        map.insert(
            "compressed_stored_bytes".into(),
            int(stats.compressed_stored_bytes),
        );
        map.insert(
            "compression_dictionary_bytes".into(),
            int(stats.compression_dictionary_bytes),
        );
        map.insert(
            "property_index_bytes".into(),
            int(stats.property_index_bytes),
        );
        map.insert("deduped_values".into(), int(stats.deduped_values));
        map.insert("deduped_plain_bytes".into(), int(stats.deduped_plain_bytes));
        map.insert("dedup_table_bytes".into(), int(stats.dedup_table_bytes));
        map.insert("shape_bytes".into(), int(stats.shape_bytes));
        map.insert("total_mb".into(), Dynamic::from_float(stats.total_mb()));
        Ok(Dynamic::from_map(map))
    }

    /// Rhai binding for `GraphStorage::statistics()`: a map with `vertex_count`, `edge_count` and
    /// the per-label count maps `vertex_labels`, `primary_vertex_labels`, `edge_labels`, or `()`
    /// when the storage keeps no statistics. One name serves both spellings. Like
    /// [`Self::memory_usage`] a plain method call, not a traversal step. Asks for `Read` on `Data`.
    pub fn statistics(&mut self) -> Result<rhai::Dynamic, Box<rhai::EvalAltResult>> {
        self.require(
            "statistics",
            crate::graph::AccessRequest::new(
                crate::graph::Action::Read,
                crate::graph::Resource::data(None, None),
            ),
        )?;
        let Some(g) = self.g.as_ref() else {
            return Ok(Dynamic::UNIT);
        };
        let Some(stats) = g.statistics() else {
            return Ok(Dynamic::UNIT);
        };
        let count = |n: usize| Dynamic::from_int(i64::try_from(n).unwrap_or(i64::MAX));
        let labels = |counts: Vec<(&str, usize)>| {
            let mut map = rhai::Map::new();
            for (label, n) in counts {
                map.insert(label.into(), count(n));
            }
            Dynamic::from_map(map)
        };
        let mut map = rhai::Map::new();
        map.insert("vertex_count".into(), count(stats.vertex_count()));
        map.insert("edge_count".into(), count(stats.edge_count()));
        map.insert("vertex_labels".into(), labels(stats.vertex_label_counts()));
        map.insert(
            "primary_vertex_labels".into(),
            labels(stats.primary_vertex_label_counts()),
        );
        map.insert("edge_labels".into(), labels(stats.edge_label_counts()));
        Ok(Dynamic::from_map(map))
    }

    /// `g.mark(name)`: sets a named mark in the graph's journal at the current position (right
    /// after the last commit) and returns `#{name, commit_seq, time}`; a recovery can stop at
    /// it. Fails when the graph has no journal (the host attaches one), the name is empty or
    /// already used. One word, so one spelling serves both conventions. Asks for `Update` on
    /// `Data`.
    pub fn mark(&mut self, name: String) -> Result<rhai::Dynamic, Box<rhai::EvalAltResult>> {
        self.require(
            "mark",
            crate::graph::AccessRequest::new(
                crate::graph::Action::Update,
                crate::graph::Resource::data(None, None),
            ),
        )?;
        let Some(g) = self.g.as_ref() else {
            return Err(Box::new(EvalAltResult::ErrorRuntime(
                rhai::Dynamic::from("No graph associated with traversal source to mark"),
                rhai::Position::NONE,
            )));
        };
        let mark = g.mark(&name).map_err(|err| {
            Box::new(EvalAltResult::ErrorRuntime(
                rhai::Dynamic::from(crate::error::format_diagnostic(&err)),
                rhai::Position::NONE,
            ))
        })?;
        let mut map = rhai::Map::new();
        map.insert("name".into(), Dynamic::from(mark.name().to_string()));
        map.insert(
            "commit_seq".into(),
            Dynamic::from_int(i64::try_from(mark.commit_seq()).unwrap_or(i64::MAX)),
        );
        map.insert("time".into(), Dynamic::from_int(mark.time()));
        Ok(Dynamic::from_map(map))
    }

    /// `g.export_snapshot(path)`: writes this source's graph as a
    /// packed snapshot (`.gsnap`: data, schema, lineage id, commit position; read it back
    /// with `GraphSource::file(path)`). Asks for `Read` on `Data` and `Create` on the `File`
    /// (resolved under the host's file root, if any).
    #[cfg(all(feature = "io", feature = "persist"))]
    pub fn export_snapshot(&mut self, path: String) -> Result<String, Box<rhai::EvalAltResult>> {
        self.require(
            "export_snapshot",
            crate::graph::AccessRequest::new(
                crate::graph::Action::Read,
                crate::graph::Resource::data(None, None),
            ),
        )?;
        let file = self.require_file("export_snapshot", crate::graph::Action::Create, &path)?;
        let fail = |err: crate::error::IOError| {
            Box::new(EvalAltResult::ErrorRuntime(
                rhai::Dynamic::from(crate::error::format_diagnostic(&err)),
                rhai::Position::NONE,
            ))
        };
        let Some(g) = self.g.as_ref() else {
            return Err(Box::new(EvalAltResult::ErrorRuntime(
                rhai::Dynamic::from("No graph associated with traversal source to export"),
                rhai::Position::NONE,
            )));
        };
        let out = std::fs::File::create(&file).map_err(|source| {
            fail(crate::error::IOError::FileWrite {
                path: path.clone(),
                source,
            })
        })?;
        let mut writer = std::io::BufWriter::new(out);
        let info = match g.write_snapshot(&mut writer) {
            Ok(info) => info,
            Err(source) => {
                // A refused or failed snapshot (a storage without snapshots) leaves no partial
                // file behind.
                drop(writer);
                let _ = std::fs::remove_file(&file);
                return Err(fail(crate::error::IOError::Snapshot {
                    path: path.clone(),
                    source: Box::new(source),
                }));
            }
        };
        std::io::Write::flush(&mut writer).map_err(|source| {
            fail(crate::error::IOError::FileWrite {
                path: path.clone(),
                source,
            })
        })?;
        Ok(format!(
            "Exported a snapshot to {path} ({} vertices, {} edges, commit {})",
            info.vertex_count, info.edge_count, info.commit_seq
        ))
    }

    /// Asks the session's authorizer for `action` on the schema.
    fn require_schema(
        &self,
        operation: &str,
        action: crate::graph::Action<'_>,
    ) -> Result<(), Box<rhai::EvalAltResult>> {
        self.require(
            operation,
            crate::graph::AccessRequest::new(action, crate::graph::Resource::Schema),
        )
    }

    /// The graph of this source, or a script error naming `operation`.
    fn schema_graph(
        &self,
        operation: &str,
    ) -> Result<&Arc<dyn AnyGraph>, Box<rhai::EvalAltResult>> {
        self.g.as_ref().ok_or_else(|| {
            Box::new(EvalAltResult::ErrorRuntime(
                rhai::Dynamic::from(format!("{operation}: no graph associated with this source")),
                rhai::Position::NONE,
            ))
        })
    }

    /// `g.get_schema()`: the stored schema as a map (its canonical format),
    /// `()` without one; never inferred. Asks for `Read` on `Schema`.
    pub fn get_schema(&mut self) -> Result<Dynamic, Box<rhai::EvalAltResult>> {
        self.require_schema("get_schema", crate::graph::Action::Read)?;
        Ok(match self.schema_graph("get_schema")?.get_schema() {
            Some(schema) => element_property_to_dynamic(schema.to_value()),
            None => Dynamic::UNIT,
        })
    }

    /// `g.set_schema(schema)`: replaces the stored schema with a map or
    /// JSON text in the schema format. Existing data is validated immediately; on violations
    /// nothing changes. Runs in the current unit (a whole-script unit, a transaction) or as one
    /// of its own. Asks for `Update` on `Schema`.
    pub fn set_schema(
        ctx: NativeCallContext,
        source: &mut Self,
        schema: Dynamic,
    ) -> Result<(), Box<rhai::EvalAltResult>> {
        source.require_schema("set_schema", crate::graph::Action::Update)?;
        let schema = schema_arg(&ctx, schema)?;
        source
            .schema_graph("set_schema")?
            .set_schema(schema)
            .map_err(script_error)
    }

    /// `g.set_schema(schema, mode)`: [`Self::set_schema`] with the
    /// schema's mode replaced by `mode` (a `SchemaMode` token or `"none"`/`"open"`/`"closed"`).
    /// The one-liner that freezes the structure a graph has:
    /// `g.set_schema(g.infer_schema(), SchemaMode.closed)`; it always succeeds on the data the
    /// schema was inferred from, except for elements without a label in `closed` mode (the
    /// report names them).
    pub fn set_schema_with_mode(
        ctx: NativeCallContext,
        source: &mut Self,
        schema: Dynamic,
        mode: Dynamic,
    ) -> Result<(), Box<rhai::EvalAltResult>> {
        source.require_schema("set_schema", crate::graph::Action::Update)?;
        let mode = schema_mode_arg(mode)?;
        let schema = schema_arg(&ctx, schema)?.with_mode(mode);
        source
            .schema_graph("set_schema")?
            .set_schema(schema)
            .map_err(script_error)
    }

    /// `g.patch_schema(patch)`: applies a JSON Merge Patch (a map or
    /// JSON text) to the stored schema and stores the result like `set_schema`; returns the new
    /// schema. Asks for `Update` on `Schema`.
    pub fn patch_schema(
        ctx: NativeCallContext,
        source: &mut Self,
        patch: Dynamic,
    ) -> Result<Dynamic, Box<rhai::EvalAltResult>> {
        source.require_schema("patch_schema", crate::graph::Action::Update)?;
        let patch = patch_arg(&ctx, patch)?;
        let schema = source
            .schema_graph("patch_schema")?
            .patch_schema(&patch)
            .map_err(script_error)?;
        Ok(element_property_to_dynamic(schema.to_value()))
    }

    /// `g.validate_schema(schema)`: a dry run of `set_schema`:
    /// `#{violations, diff, compatible}`. Nothing is applied. Asks for `Read` on `Schema`.
    pub fn validate_schema(
        ctx: NativeCallContext,
        source: &mut Self,
        schema: Dynamic,
    ) -> Result<Dynamic, Box<rhai::EvalAltResult>> {
        source.require_schema("validate_schema", crate::graph::Action::Read)?;
        let schema = schema_arg(&ctx, schema)?;
        let validation = source
            .schema_graph("validate_schema")?
            .validate_schema(&schema)
            .map_err(script_error)?;
        Ok(element_property_to_dynamic(validation.to_value()))
    }

    /// `g.validate_schema(schema, mode)`: the dry run of
    /// `set_schema(schema, mode)`.
    pub fn validate_schema_with_mode(
        ctx: NativeCallContext,
        source: &mut Self,
        schema: Dynamic,
        mode: Dynamic,
    ) -> Result<Dynamic, Box<rhai::EvalAltResult>> {
        source.require_schema("validate_schema", crate::graph::Action::Read)?;
        let mode = schema_mode_arg(mode)?;
        let schema = schema_arg(&ctx, schema)?.with_mode(mode);
        let validation = source
            .schema_graph("validate_schema")?
            .validate_schema(&schema)
            .map_err(script_error)?;
        Ok(element_property_to_dynamic(validation.to_value()))
    }

    /// `g.validate_schema_patch(patch)`: `validate_schema` of
    /// the stored schema patched with `patch`. Asks for `Read` on `Schema`.
    pub fn validate_schema_patch(
        ctx: NativeCallContext,
        source: &mut Self,
        patch: Dynamic,
    ) -> Result<Dynamic, Box<rhai::EvalAltResult>> {
        source.require_schema("validate_schema_patch", crate::graph::Action::Read)?;
        let patch = patch_arg(&ctx, patch)?;
        let validation = source
            .schema_graph("validate_schema_patch")?
            .validate_schema_patch(&patch)
            .map_err(script_error)?;
        Ok(element_property_to_dynamic(validation.to_value()))
    }

    /// `g.diff_schema(schema)`: `#{changes, compatible}` from the
    /// stored schema to `schema`. Asks for `Read` on `Schema`.
    pub fn diff_schema(
        ctx: NativeCallContext,
        source: &mut Self,
        schema: Dynamic,
    ) -> Result<Dynamic, Box<rhai::EvalAltResult>> {
        source.require_schema("diff_schema", crate::graph::Action::Read)?;
        let schema = schema_arg(&ctx, schema)?;
        let diff = source
            .schema_graph("diff_schema")?
            .diff_schema(&schema)
            .map_err(script_error)?;
        Ok(element_property_to_dynamic(diff.to_value()))
    }

    /// `g.infer_schema()`: on an empty pipeline, the schema the whole graph's
    /// data satisfies, as a map (asks for `Read` on `Schema`); after steps
    /// (`g.v().has_label("person").infer_schema()`), the `infer_schema()` step over the stream.
    pub fn infer_schema(&mut self) -> Result<Dynamic, Box<rhai::EvalAltResult>> {
        if !self.steps.is_empty() {
            return Ok(Dynamic::from(self.add_step(Step::InferSchema)));
        }
        self.require_schema("infer_schema", crate::graph::Action::Read)?;
        let schema = self
            .schema_graph("infer_schema")?
            .infer_schema()
            .map_err(script_error)?;
        Ok(element_property_to_dynamic(schema.to_value()))
    }
    /// Overrides the visualization format used whenever the pipeline is rendered without an
    /// explicit format, i.e. by `visualize()` and by the implicit rendering of a traversal that
    /// was never terminated.
    ///
    /// The option travels with every pipeline derived from this source, so setting it once on
    /// `g` applies to all following queries of the session — this is the one deliberate exception
    /// to [`Self::with`]'s per-statement, non-persistent contract: a session-wide display
    /// preference is meant to stick, unlike a per-query option such as `optimizer.disabled`.
    /// Mutates `self.options` directly rather than going through `with` (which now clones first,
    /// precisely so ordinary options *don't* persist this way — see its doc comment), since `g`
    /// itself is the actual, long-lived Rhai scope variable this needs to change.
    pub fn set_visualizer(&mut self, format: VisualizeFormat) -> Self {
        self.options.insert(
            CompactString::new("visualizer"),
            ElementProperty::from(format.as_name()),
        );
        self.clone()
    }

    /// Returns the format set by [`Self::set_visualizer`], if the pipeline carries one.
    pub fn configured_visualizer(&self) -> Option<VisualizeFormat> {
        self.options
            .get("visualizer")
            .and_then(|value| value.as_str())
            .and_then(VisualizeFormat::from_name)
    }

    /// Reads the first boolean-valued option found under any of `keys`, or `default`.
    pub fn get_bool_option(&self, keys: &[&str], default: bool) -> bool {
        for key in keys {
            if let Some(val) = self.options.get(*key)
                && let Some(b) = val.as_boolean()
            {
                return b;
            }
        }
        default
    }

    /// `g.export_graphml(path)`: exports this source's graph as GraphML. Asks for `Read` on
    /// `Data` and `Create` on the `File` (resolved under the host's file root, if any).
    #[cfg(feature = "io")]
    pub fn export_graphml(&mut self, path: String) -> Result<String, Box<rhai::EvalAltResult>> {
        self.require(
            "export_graphml",
            crate::graph::AccessRequest::new(
                crate::graph::Action::Read,
                crate::graph::Resource::data(None, None),
            ),
        )?;
        let file = self.require_file("export_graphml", crate::graph::Action::Create, &path)?;
        let beautify =
            self.get_bool_option(&["io.graphml.beautify", "io.beautify", "beautify"], false);
        if let Some(ref g) = self.g {
            create_for_export(&file, true)
                .and_then(|mut writer| {
                    g.export_graphml(&mut writer, beautify)?;
                    flush_export(&file, writer)
                })
                .map_err(|err| {
                    Box::new(rhai::EvalAltResult::ErrorRuntime(
                        rhai::Dynamic::from(crate::error::format_diagnostic(&err)),
                        rhai::Position::NONE,
                    ))
                })?;
            Ok(format!("Exported GraphML to {}", path))
        } else {
            Err(Box::new(rhai::EvalAltResult::ErrorRuntime(
                rhai::Dynamic::from("No graph associated with traversal source to export"),
                rhai::Position::NONE,
            )))
        }
    }

    /// `g.export_graphson(path)`: exports this source's graph as
    /// GraphSON 3.0 (line form). Asks for `Read` on `Data` and `Create` on the `File` (resolved
    /// under the host's file root, if any).
    #[cfg(feature = "io")]
    pub fn export_graphson(&mut self, path: String) -> Result<String, Box<rhai::EvalAltResult>> {
        self.require(
            "export_graphson",
            crate::graph::AccessRequest::new(
                crate::graph::Action::Read,
                crate::graph::Resource::data(None, None),
            ),
        )?;
        let file = self.require_file("export_graphson", crate::graph::Action::Create, &path)?;
        if let Some(ref g) = self.g {
            create_for_export(&file, false)
                .and_then(|mut writer| {
                    g.export_graphson(&mut writer)?;
                    flush_export(&file, writer)
                })
                .map_err(|err| {
                    Box::new(rhai::EvalAltResult::ErrorRuntime(
                        rhai::Dynamic::from(crate::error::format_diagnostic(&err)),
                        rhai::Position::NONE,
                    ))
                })?;
            Ok(format!("Exported GraphSON to {path}"))
        } else {
            Err(Box::new(rhai::EvalAltResult::ErrorRuntime(
                rhai::Dynamic::from("No graph associated with traversal source to export"),
                rhai::Position::NONE,
            )))
        }
    }

    /// This source with the time of the evaluation `ctx` belongs to (see the `now` field).
    pub(crate) fn stamped(mut self, ctx: &NativeCallContext) -> Self {
        self.now = Some(access::now_of(ctx));
        self
    }

    pub(super) fn add_step(&mut self, step: Step) -> Self {
        let mut res = self.clone();
        res.steps.push(step);
        res
    }
}

/// Opens the GraphML/GraphSON file `path` for an import (the errors of the importers' own file
/// functions).
#[cfg(feature = "io")]
fn open_for_import(
    path: &std::path::Path,
) -> Result<std::io::BufReader<std::fs::File>, crate::error::IOError> {
    std::fs::File::open(path)
        .map(|file| std::io::BufReader::with_capacity(128 * 1024, file))
        .map_err(|source| crate::error::IOError::FileRead {
            path: path.to_string_lossy().into_owned(),
            source,
        })
}

/// Creates the file `path` for an export (the errors of the exporters' own file functions: the
/// GraphML exporter reports a failed create as a read error, `graphml`).
#[cfg(feature = "io")]
fn create_for_export(
    path: &std::path::Path,
    graphml: bool,
) -> Result<std::io::BufWriter<std::fs::File>, crate::error::IOError> {
    let path_text = || path.to_string_lossy().into_owned();
    std::fs::File::create(path)
        .map(|file| std::io::BufWriter::with_capacity(128 * 1024, file))
        .map_err(|source| {
            if graphml {
                crate::error::IOError::FileRead {
                    path: path_text(),
                    source,
                }
            } else {
                crate::error::IOError::FileWrite {
                    path: path_text(),
                    source,
                }
            }
        })
}

/// Flushes the writer of an export to `path`.
#[cfg(feature = "io")]
fn flush_export(
    path: &std::path::Path,
    mut writer: std::io::BufWriter<std::fs::File>,
) -> Result<(), crate::error::IOError> {
    std::io::Write::flush(&mut writer).map_err(|source| crate::error::IOError::FileWrite {
        path: path.to_string_lossy().into_owned(),
        source,
    })
}
