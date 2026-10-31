//! The `graphersal.Graph` class: a thin wrapper around the engine's script entry points
//! (`graphersal::script::eval_value_with_limits`, `eval_value_atomic`, `eval_value_dry_run`),
//! per the design spec's Public API and Concurrency sections, and the `DryRunResult` it returns
//! from `dry_run`.

use std::sync::{Arc, Mutex};

use graphersal::error::format_diagnostic;
use graphersal::exec::MemoryLimit;
use graphersal::io::{GraphExport, GraphImport, ImportReport};
use graphersal::persist;
use graphersal::prelude::ElementProperty;
use graphersal::schema::{GraphSchema, SchemaMode, SchemaPatch};
use graphersal::script::{
    ScriptLimits, eval_value_atomic, eval_value_dry_run, eval_value_with_limits,
    materialize_traversals, with_traversal_presets,
};
use graphersal::{GraphSource, prelude::Graph as EngineGraph};
use graphersal::{auth::Authorizer, storage::GraphStorage};
use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::convert::{
    change_set_to_pyobject, dynamic_to_pyobject, python_params_to_element_properties,
};
use crate::error::{map_diagnostic, map_eval_error, map_io_error};
use crate::policy::AccessPolicy;
use crate::script_thread::on_script_thread;

/// A Graphersal graph. Scripts in the project's Gremlin dialect run against it through
/// ``execute``/``query`` (and ``dry_run`` to preview their changes); results come back as
/// native Python values.
#[pyclass(module = "graphersal")]
pub struct Graph {
    inner: Arc<EngineGraph>,
    /// Resource limits of every script this graph runs: the SAFE defaults until `set_limits`.
    limits: Mutex<GraphLimits>,
}

/// What a script run from Python may do: `policy` when the call names one, else everything (the
/// library's default; a Python program is a trusted local host that already has file access and
/// the graph itself, `Graph.export_graphml`). A program that runs scripts it did not write
/// passes `policy=` per call.
fn effective_authorizer(policy: Option<&AccessPolicy>) -> Arc<dyn Authorizer> {
    match policy {
        Some(policy) => Arc::new(policy.to_engine()),
        None => AccessPolicy::default_authorizer(),
    }
}

/// What `set_limits` sets: the script's resource limits and the per-query defaults of the
/// traversals it runs.
#[derive(Clone, Default)]
struct GraphLimits {
    script: ScriptLimits,
    /// The `traversal.max_materialized_bytes` default (`None`: the engine default).
    max_materialized_bytes: Option<u64>,
}

impl GraphLimits {
    /// The traversal presets of a run (`with_traversal_presets`).
    fn traversal_presets(&self) -> Vec<(&'static str, ElementProperty)> {
        self.max_materialized_bytes
            .map(|bytes| {
                (
                    "traversal.max_materialized_bytes",
                    ElementProperty::from(i64::try_from(bytes).unwrap_or(i64::MAX)),
                )
            })
            .into_iter()
            .collect()
    }
}

/// The process memory ceiling of a `set_memory_limit` argument: a positive number of bytes,
/// `"auto"` (the default: the cgroup memory limit minus the headroom, else 80 % of the physical
/// memory) or `"none"` (no ceiling); `None`: not given.
pub(crate) fn memory_limit_value(
    limit: Option<&Bound<'_, PyAny>>,
) -> PyResult<Option<MemoryLimit>> {
    let Some(limit) = limit else {
        return Ok(None);
    };
    if let Ok(text) = limit.extract::<String>() {
        let trimmed = text.trim();
        if trimmed.eq_ignore_ascii_case("auto") || trimmed.eq_ignore_ascii_case("none") {
            return trimmed
                .parse::<MemoryLimit>()
                .map(Some)
                .map_err(|err| PyValueError::new_err(err.to_string()));
        }
        return Err(PyValueError::new_err(format!(
            "memory_limit must be a positive number of bytes, \"auto\" or \"none\", got {text:?}"
        )));
    }
    match limit.extract::<i64>() {
        Ok(bytes) if bytes >= 1 => Ok(Some(MemoryLimit::Bytes(bytes as u64))),
        Ok(bytes) => Err(PyValueError::new_err(format!(
            "memory_limit must be a positive number of bytes, \"auto\" or \"none\", got {bytes}"
        ))),
        Err(_) => Err(PyTypeError::new_err(
            "memory_limit must be an int (bytes), \"auto\", \"none\" or None",
        )),
    }
}

/// Sets the process memory ceiling (`limit` `None`: unchanged) and the headroom of `auto`.
pub(crate) fn set_process_memory_limit(
    limit: Option<MemoryLimit>,
    headroom: Option<u64>,
) -> PyResult<()> {
    let runtime =
        |err: Box<graphersal::prelude::TraverserError>| PyValueError::new_err(err.to_string());
    if let Some(limit) = limit {
        graphersal::exec::set_memory_limit(limit).map_err(runtime)?;
        graphersal::exec::set_memory_headroom(headroom).map_err(runtime)?;
    }
    Ok(())
}

impl Graph {
    /// A Python graph over a graph another owner keeps (a `Store`'s; its models come with it).
    pub(crate) fn from_shared(inner: Arc<EngineGraph>) -> Self {
        Graph {
            inner,
            limits: Mutex::new(GraphLimits::default()),
        }
    }

    fn wrap(inner: EngineGraph) -> Self {
        Graph::from_shared(Arc::new(inner))
    }
}

#[pymethods]
impl Graph {
    /// ``Graph()``: a new, empty graph.
    ///
    /// The process memory ceiling is not a keyword of a graph: it is one setting of the whole
    /// process, ``graphersal.set_memory_limit``.
    ///
    /// Its scripts embed (``embed()``, text queries in ``nearest``/``similarity``) with the
    /// built-in ``"hash-ngram/1"``; embedding models serve a graph only from its database: a
    /// ``Store`` (``store.import_model``) or a ``.gsnap`` that carries them (``Graph.load``).
    #[new]
    fn new() -> Self {
        Graph::wrap(GraphSource::empty())
    }

    /// ``Graph.tinkerpop_modern()``: the standard TinkerPop "modern" toy graph (6 vertices,
    /// 6 edges).
    #[staticmethod]
    fn tinkerpop_modern() -> Self {
        Graph::wrap(GraphSource::tinkerpop_modern())
    }

    /// ``Graph.from_graphml(path, *, schema=None, mode=None)``: loads a graph from a GraphML file (see also
    /// ``Graph.from_graphson``). Raises ``GraphersalError`` when the file cannot be read or
    /// parsed.
    ///
    /// GraphML carries no schema. ``schema`` (a ``dict`` or JSON ``str`` in the schema format) is
    /// set on the new, empty graph first (``mode``, ``"none"``/``"open"``/``"closed"``, replaces
    /// its own mode), then the file is imported into it as one unit: properties the schema
    /// declares ``uuid``, ``array`` or ``object`` come back with that type instead of as strings,
    /// and in ``open``/``closed`` a violation fails the load with the usual report.
    #[staticmethod]
    #[pyo3(signature = (path, *, schema=None, mode=None))]
    fn from_graphml(
        py: Python<'_>,
        path: &str,
        schema: Option<&Bound<'_, PyAny>>,
        mode: Option<String>,
    ) -> PyResult<Self> {
        let inner = match load_schema_argument(py, schema, mode)? {
            None => GraphSource::from_graphml(path).map_err(|err| map_io_error(py, err))?,
            Some((text, mode)) => {
                let path = path.to_string();
                py.detach(move || {
                    let mut graph = new_graph_with_schema(&text, mode.as_deref())?;
                    graph
                        .import_graphml(&path)
                        .map_err(|err| format_diagnostic(&err))?;
                    Ok(graph)
                })
                .map_err(|text: String| map_diagnostic(py, &text))?
            }
        };
        Ok(Graph::wrap(inner))
    }

    /// ``Graph.from_graphson(path, *, schema=None, mode=None)``: loads a graph from a GraphSON 3.0 file (one vertex per line,
    /// TinkerPop's default, or one ``{"vertices": [...]}`` document; 2.0 and untyped 1.0 read
    /// too). Data the import skips (dates, types Graphersal cannot hold, edges to missing
    /// vertices) is not an error: it is reported in one ``UserWarning``. Raises
    /// ``GraphersalError`` when the file cannot be read or parsed. ``schema`` and ``mode`` work
    /// as in ``Graph.from_graphml`` (set first, then the file imported as one unit; a violation
    /// in ``open``/``closed`` fails the load).
    #[staticmethod]
    #[pyo3(signature = (path, *, schema=None, mode=None))]
    fn from_graphson(
        py: Python<'_>,
        path: &str,
        schema: Option<&Bound<'_, PyAny>>,
        mode: Option<String>,
    ) -> PyResult<Self> {
        let (inner, report) = match load_schema_argument(py, schema, mode)? {
            None => graphersal::io::GraphSONImport::new()
                .load_file(path)
                .map_err(|err| map_io_error(py, err))?,
            Some((text, mode)) => {
                let path = path.to_string();
                py.detach(move || {
                    let mut graph = new_graph_with_schema(&text, mode.as_deref())?;
                    let report = graph
                        .import_graphson(&path)
                        .map_err(|err| format_diagnostic(&err))?;
                    Ok((graph, report))
                })
                .map_err(|text: String| map_diagnostic(py, &text))?
            }
        };
        warn_graphson_skips(py, path, &report)?;
        Ok(Graph::wrap(inner))
    }

    /// ``graph.import_graphml(path)``: imports a GraphML file into this live graph as one unit:
    /// either every element of the file is added, or (a parse error, a schema violation) the
    /// graph is left unchanged and ``GraphersalError`` is raised. Properties the stored schema
    /// declares ``uuid``, ``array`` or ``object`` are restored with that type.
    fn import_graphml(&self, py: Python<'_>, path: &str) -> PyResult<()> {
        let graph = Arc::clone(&self.inner);
        let path = path.to_string();
        py.detach(move || {
            graph
                .write()
                .import_graphml(&path)
                .map_err(|err| format_diagnostic(&err))
        })
        .map_err(|text| map_diagnostic(py, &text))
    }

    /// ``graph.import_graphson(path)``: imports a GraphSON file into this live graph as one unit
    /// (all or nothing, like ``import_graphml``). Skipped data is reported in one
    /// ``UserWarning``, as by ``Graph.from_graphson``.
    fn import_graphson(&self, py: Python<'_>, path: &str) -> PyResult<()> {
        let graph = Arc::clone(&self.inner);
        let target = path.to_string();
        let report = py
            .detach(move || {
                graph
                    .write()
                    .import_graphson(&target)
                    .map_err(|err| format_diagnostic(&err))
            })
            .map_err(|text| map_diagnostic(py, &text))?;
        warn_graphson_skips(py, path, &report)
    }

    /// ``graph.export_graphson(path)``: writes this graph out as GraphSON 3.0 in the line form,
    /// which TinkerGraph's ``g.io(path).read()`` loads. Values keep their types (``uuid``,
    /// ``array`` and ``object`` too); the file carries no schema.
    fn export_graphson(&self, py: Python<'_>, path: &str) -> PyResult<()> {
        let graph = Arc::clone(&self.inner);
        let target = path.to_string();
        // The read lock is taken off the GIL (another Python thread may hold the write lock).
        py.detach(move || {
            graph
                .export_graphson(&target)
                .map_err(|err| format_diagnostic(&err))
        })
        .map_err(|text| map_diagnostic(py, &text))
    }

    /// ``graph.export_graphml(path)``: writes this graph out as GraphML. GraphML carries no
    /// schema: keep the schema JSON separately and set it on the target graph before the import
    /// (``graph.set_schema(..)`` then ``g.import_graphml(..)``) to restore declared types.
    fn export_graphml(&self, py: Python<'_>, path: &str) -> PyResult<()> {
        let graph = Arc::clone(&self.inner);
        let target = path.to_string();
        py.detach(move || {
            graph
                .export_graphml(&target)
                .map_err(|err| format_diagnostic(&err))
        })
        .map_err(|text| map_diagnostic(py, &text))
    }

    /// ``graph.save(path, *, name=None)``: writes this graph as a packed snapshot (the
    /// lossless binary format, ``.gsnap``): data with every value type, the schema, the lineage
    /// id, the commit position and the auto-id sequences. ``Graph.load(path)`` reads it back.
    /// Returns ``{"commit_seq", "vertex_count", "edge_count"}``.
    #[pyo3(signature = (path, *, name=None))]
    fn save(&self, py: Python<'_>, path: &str, name: Option<String>) -> PyResult<Py<PyAny>> {
        let graph = Arc::clone(&self.inner);
        let target = path.to_string();
        let info = py
            .detach(move || -> Result<_, String> {
                let file = std::fs::File::create(&target)
                    .map_err(|err| format!("Error: cannot create '{target}': {err}"))?;
                let mut options = persist::WriteOptions::new();
                if let Some(name) = name {
                    options = options.with_name(name);
                }
                let mut writer = std::io::BufWriter::new(file);
                let info = persist::write_snapshot(&*graph.read(), &mut writer, &options)
                    .map_err(|err| format_diagnostic(&err))?;
                writer
                    .into_inner()
                    .map_err(|err| format!("Error: cannot write '{target}': {err}"))?
                    .sync_all()
                    .map_err(|err| format!("Error: cannot sync '{target}': {err}"))?;
                Ok(info)
            })
            .map_err(|text| map_diagnostic(py, &text))?;
        let dict = PyDict::new(py);
        dict.set_item("commit_seq", info.commit_seq)?;
        dict.set_item("vertex_count", info.vertex_count)?;
        dict.set_item("edge_count", info.edge_count)?;
        Ok(dict.into_any().unbind())
    }

    /// ``Graph.load(path)``: reads a packed snapshot (``.gsnap``, written by ``graph.save``
    /// or ``store.export``). No schema validation (the checksums protect the data); a damaged
    /// file raises ``GraphersalError`` naming the section and byte. A snapshot whose size
    /// estimate exceeds the process memory ceiling (``graphersal.set_memory_limit``) is refused
    /// before any element is read.
    ///
    /// The embedding models the file carries serve the graph (``embed()``, text queries,
    /// ``graph.embedders()``), their blobs kept in memory; the models of a Graphersal 0.4.0
    /// file are converted in memory (the file stays as it is). What an earlier release left
    /// that this version does not keep current (a model the schema names that the file lacks,
    /// vectors without a vector rule, saved queries calling a removed function, models stored
    /// but none the default) is one ``UserWarning`` per note.
    #[staticmethod]
    fn load(py: Python<'_>, path: &str) -> PyResult<Self> {
        // The engine checks the process memory ceiling itself: a snapshot whose size estimate
        // exceeds it is refused before any element is read.
        let options = persist::ReadOptions::new();
        let source = path.to_string();
        let (graph, report) = py
            .detach(move || -> Result<_, String> {
                if !std::path::Path::new(&source).is_file() {
                    return Err(format!("Error: cannot open '{source}': no such file"));
                }
                persist::read_snapshot_file(&source, &options)
                    .map_err(|err| format_diagnostic(&err))
            })
            .map_err(|text| map_diagnostic(py, &text))?;
        for note in report.notes(path) {
            crate::error::warn(py, &note)?;
        }
        Ok(Graph::from_shared(Arc::new(graph)))
    }

    /// ``graph.start_journal(path, *, durability="every_commit")``: attaches a journal
    /// (write-ahead log) to this graph, writing to a NEW file at ``path`` (an existing file is
    /// replaced). From now on every committed change is written (and, per ``durability``,
    /// fsynced) before it becomes final; a failed write rolls the change back and raises.
    /// ``durability``: ``"every_commit"`` (fsync per commit, the default), ``"os"`` (the
    /// operating system decides) or a number of seconds (fsync at most that often). Save a
    /// snapshot at the same position first (``graph.save``) so ``Graph.recover`` has a base.
    #[pyo3(signature = (path, *, durability=None))]
    fn start_journal(
        &self,
        py: Python<'_>,
        path: &str,
        durability: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        let durability = match durability {
            None => persist::Durability::EveryCommit,
            Some(value) => {
                if let Ok(text) = value.extract::<String>() {
                    match text.as_str() {
                        "every_commit" => persist::Durability::EveryCommit,
                        "os" => persist::Durability::Os,
                        other => {
                            return Err(PyValueError::new_err(format!(
                                "durability must be \"every_commit\", \"os\" or seconds, got {other:?}"
                            )));
                        }
                    }
                } else {
                    let seconds: f64 = value.extract().map_err(|_| {
                        PyTypeError::new_err("durability must be a str or a number of seconds")
                    })?;
                    if !seconds.is_finite() || seconds < 0.0 {
                        return Err(PyValueError::new_err(
                            "durability seconds must be a non-negative number",
                        ));
                    }
                    persist::Durability::Interval(std::time::Duration::from_secs_f64(seconds))
                }
            }
        };
        let graph = Arc::clone(&self.inner);
        let target = path.to_string();
        py.detach(move || -> Result<(), String> {
            let file = std::fs::File::create(&target)
                .map_err(|err| format!("Error: cannot create '{target}': {err}"))?;
            persist::Journal::for_file(file, durability)
                .attach(&mut graph.write())
                .map(|_| ())
                .map_err(|err| format_diagnostic(&err))
        })
        .map_err(|text| map_diagnostic(py, &text))
    }

    /// ``graph.mark(name)``: sets a named mark in the graph's journal at the current position
    /// (after the last commit); ``Graph.recover(.., mark=name)`` stops there. Needs a journal
    /// (``start_journal``); a name is unique within its journal. Returns ``{"name",
    /// "commit_seq", "time"}`` (time in microseconds since the Unix epoch).
    fn mark(&self, py: Python<'_>, name: &str) -> PyResult<Py<PyAny>> {
        let graph = Arc::clone(&self.inner);
        let name = name.to_string();
        let mark = py
            .detach(move || {
                graph
                    .write()
                    .mark(&name)
                    .map_err(|err| format_diagnostic(&err))
            })
            .map_err(|text| map_diagnostic(py, &text))?;
        let dict = PyDict::new(py);
        dict.set_item("name", mark.name())?;
        dict.set_item("commit_seq", mark.commit_seq())?;
        dict.set_item("time", mark.time())?;
        Ok(dict.into_any().unbind())
    }

    /// ``Graph.recover(snapshot, journals, *, commit_seq=None, time=None, mark=None)``: loads
    /// the packed snapshot at ``snapshot`` (a path, or ``None`` to start from an empty graph)
    /// and replays the journal files ``journals`` (paths, in order) up to the end, or up to
    /// ``commit_seq``, ``time`` (microseconds since the Unix epoch) or the mark ``mark`` (at most
    /// one target). An incomplete last record (an interrupted write) is ignored; damage raises
    /// ``GraphersalError`` with its location. Returns ``(graph, report)``, the report a ``dict``
    /// with ``commit_seq``, ``commits_replayed``, ``marks`` (names), ``torn_tail`` and
    /// ``reached_end``. The recovered graph has no journal: call ``start_journal`` (after a
    /// ``save``) to continue.
    #[staticmethod]
    #[pyo3(signature = (snapshot, journals, *, commit_seq=None, time=None, mark=None))]
    fn recover(
        py: Python<'_>,
        snapshot: Option<String>,
        journals: Vec<String>,
        commit_seq: Option<u64>,
        time: Option<i64>,
        mark: Option<String>,
    ) -> PyResult<(Self, Py<PyAny>)> {
        let target = match (commit_seq, time, mark) {
            (None, None, None) => persist::RecoveryTarget::Latest,
            (Some(seq), None, None) => persist::RecoveryTarget::CommitSeq(seq),
            (None, Some(time), None) => persist::RecoveryTarget::Time(time),
            (None, None, Some(name)) => persist::RecoveryTarget::Mark(name),
            _ => {
                return Err(PyValueError::new_err(
                    "give at most one of commit_seq, time and mark",
                ));
            }
        };
        let recovered = py
            .detach(move || -> Result<_, String> {
                let open = |path: &str| {
                    std::fs::File::open(path)
                        .map(|file| std::io::BufReader::with_capacity(1 << 20, file))
                        .map_err(|err| format!("Error: cannot open '{path}': {err}"))
                };
                let mut recovery = persist::Recovery::new().target(target);
                if let Some(path) = &snapshot {
                    recovery = recovery.snapshot(open(path)?);
                }
                for path in &journals {
                    recovery = recovery.journal(open(path)?);
                }
                recovery.run().map_err(|err| format_diagnostic(&err))
            })
            .map_err(|text| map_diagnostic(py, &text))?;
        let report = PyDict::new(py);
        report.set_item("snapshot_commit_seq", recovered.report.snapshot_commit_seq)?;
        report.set_item("commit_seq", recovered.report.commit_seq)?;
        report.set_item("commits_replayed", recovered.report.commits_replayed)?;
        let marks: Vec<String> = recovered
            .report
            .marks
            .iter()
            .map(|m| m.name().to_string())
            .collect();
        report.set_item("marks", marks)?;
        report.set_item(
            "torn_tail",
            recovered.report.torn_tail.map(|torn| torn.bytes),
        )?;
        report.set_item("reached_end", recovered.report.reached_end)?;
        let graph = Graph::wrap(EngineGraph::new(recovered.graph));
        Ok((graph, report.into_any().unbind()))
    }

    /// ``graph.set_limits(*, max_operations=None, max_string_size=None, max_array_size=None,
    /// max_map_size=None, max_value_depth=None, max_materialized_bytes=None, unlimited=False)``:
    /// changes the resource limits of the scripts this graph runs. A new graph runs under the SAFE defaults (10M operations,
    /// 16 MiB strings, 100k array items, 100k map entries, 128 levels of nested values);
    /// ``unlimited=True`` starts from no limits instead (the value depth stays 128). Each given
    /// keyword then overrides one field (``0`` means unlimited; for ``max_value_depth`` the
    /// default, since the depth is never unlimited). A script that hits a limit raises
    /// ``GraphersalError`` whose ``help`` names the field to raise.
    ///
    /// ``max_materialized_bytes`` is the default of the per-query memory limit
    /// ``traversal.max_materialized_bytes`` of every traversal these scripts run (the estimated
    /// size of one value or buffer a query materializes; ``None``: the library default, 1 GiB;
    /// ``0`` means unlimited). A query's own ``g.with("traversal.max_materialized_bytes", n)``
    /// overrides it. The process memory ceiling is ``graphersal.set_memory_limit``.
    #[pyo3(signature = (*, max_operations=None, max_string_size=None, max_array_size=None, max_map_size=None, max_value_depth=None, max_materialized_bytes=None, unlimited=false))]
    #[allow(clippy::too_many_arguments)]
    fn set_limits(
        &self,
        max_operations: Option<u64>,
        max_string_size: Option<usize>,
        max_array_size: Option<usize>,
        max_map_size: Option<usize>,
        max_value_depth: Option<usize>,
        max_materialized_bytes: Option<u64>,
        unlimited: bool,
    ) {
        let mut limits = if unlimited {
            ScriptLimits::unlimited()
        } else {
            ScriptLimits::default()
        };
        if let Some(n) = max_operations {
            limits.max_operations = n;
        }
        if let Some(n) = max_string_size {
            limits.max_string_size = n;
        }
        if let Some(n) = max_array_size {
            limits.max_array_size = n;
        }
        if let Some(n) = max_map_size {
            limits.max_map_size = n;
        }
        if let Some(n) = max_value_depth {
            limits.max_value_depth = n;
        }
        let limits = GraphLimits {
            script: limits,
            // 0 means unlimited, like the CLI's --max-materialized-bytes 0.
            max_materialized_bytes: max_materialized_bytes
                .map(|n| if n == 0 { u64::MAX } else { n }),
        };
        // The whole value is replaced, so a lock poisoned by an earlier panic holds a
        // consistent value again afterwards: written through, then the poison cleared.
        *self
            .limits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = limits;
        self.limits.clear_poison();
    }

    /// ``graph.limits()``: the limits ``set_limits`` set, as a dict with the same keys
    /// (``max_operations``, ``max_string_size``, ``max_array_size``, ``max_map_size``,
    /// ``max_value_depth``, ``max_materialized_bytes``). ``0`` means unlimited, as in
    /// ``set_limits``; ``max_materialized_bytes`` is ``None`` while the library default applies.
    /// The process memory ceiling is ``graphersal.memory_limit()``.
    fn limits<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let limits = self
            .limits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let dict = PyDict::new(py);
        dict.set_item("max_operations", limits.script.max_operations)?;
        dict.set_item("max_string_size", limits.script.max_string_size)?;
        dict.set_item("max_array_size", limits.script.max_array_size)?;
        dict.set_item("max_map_size", limits.script.max_map_size)?;
        dict.set_item("max_value_depth", limits.script.max_value_depth)?;
        dict.set_item(
            "max_materialized_bytes",
            limits
                .max_materialized_bytes
                .map(|n| if n == u64::MAX { 0 } else { n }),
        )?;
        Ok(dict)
    }

    /// ``graph.execute(script, params=None, *, atomic=False, policy=None)``: evaluates a
    /// script against this graph and returns its result as native Python values.
    ///
    /// - ``params``: a dict of scalar values bound as script variables (and as the ``params``
    ///   map).
    /// - ``atomic``: ``False`` (the default) makes every traversal its own unit: a failing
    ///   traversal rolls back only itself, and the statements before it stay. ``True`` makes the
    ///   whole script one unit: when it fails, everything it changed is rolled back; no other
    ///   reader or writer of the graph runs while it does.
    /// - ``policy``: an ``AccessPolicy`` limiting what the script may do; ``None`` (the
    ///   default) allows everything, since a Python program is a trusted local host.
    ///
    /// Raises ``GraphersalError`` (or a subclass) when the script fails. The GIL is released
    /// while the script runs.
    #[pyo3(signature = (script, params=None, *, atomic=false, policy=None))]
    fn execute(
        &self,
        py: Python<'_>,
        script: &str,
        params: Option<&Bound<'_, PyDict>>,
        atomic: bool,
        policy: Option<PyRef<'_, AccessPolicy>>,
    ) -> PyResult<Py<PyAny>> {
        self.run(py, script, params, atomic, policy.as_deref())
    }

    /// ``graph.query(script, params=None, *, atomic=False, policy=None)``: identical to
    /// ``execute``; offered as a read-oriented alias for call sites that only run queries (it
    /// does not restrict the script by itself: pass ``policy=AccessPolicy.read_only()`` for
    /// that).
    #[pyo3(signature = (script, params=None, *, atomic=false, policy=None))]
    fn query(
        &self,
        py: Python<'_>,
        script: &str,
        params: Option<&Bound<'_, PyDict>>,
        atomic: bool,
        policy: Option<PyRef<'_, AccessPolicy>>,
    ) -> PyResult<Py<PyAny>> {
        self.run(py, script, params, atomic, policy.as_deref())
    }

    /// ``graph.dry_run(script, params=None, *, policy=None)``: runs the script as one unit
    /// and then ALWAYS rolls everything back, returning a ``DryRunResult`` with ``.result`` and
    /// ``.changes``.
    ///
    /// ``result`` is what ``execute`` would have returned. ``changes`` is the would-be change as
    /// plain data: ``{"commit_seq": 0, "committed_at": 0, "node_sequence": .., "edge_sequence":
    /// .., "mutations": [...]}``. ``commit_seq`` and ``committed_at`` are ``0`` (never
    /// committed), ``node_sequence``/``edge_sequence`` the auto-id sequences after the run. Each
    /// mutation is a dict with an ``"op"`` key (``"add_vertex"``, ``"drop_vertex"``, ``"add_edge"``, ``"drop_edge"``,
    /// ``"set_property"``, ``"remove_property"``, ``"add_label"``, ``"remove_label"``,
    /// ``"set_schema"``) in execution order, with before and after values. Values JSON cannot
    /// hold carry a type tag: a uuid is ``{"@type": "g:UUID", "@value": "<uuid>"}``.
    ///
    /// The graph is left exactly as it was (only auto-generated ids are not reused); no commit
    /// hook runs. A traversal the script returns without a terminal runs inside the dry run, as
    /// with ``.to_list()``. A failing script raises like ``execute`` and returns no change set;
    /// a returned closure raises ``ReturnedClosureError``. The GIL is released while the script
    /// runs.
    #[pyo3(signature = (script, params=None, *, policy=None))]
    fn dry_run(
        &self,
        py: Python<'_>,
        script: &str,
        params: Option<&Bound<'_, PyDict>>,
        policy: Option<PyRef<'_, AccessPolicy>>,
    ) -> PyResult<DryRunResult> {
        let ScriptRequest {
            script,
            params,
            limits,
            presets,
            authorizer,
        } = self.request(script, params, policy.as_deref())?;
        let graph = Arc::clone(&self.inner);
        let AssertUngil(result) = py.detach(move || {
            on_script_thread(move || {
                AssertUngil(with_traversal_presets(&presets, || {
                    eval_value_dry_run(graph, &script, params, &limits, authorizer)
                }))
            })
        });
        match result {
            Ok((value, change_set)) => Ok(DryRunResult {
                result: dynamic_to_pyobject(py, value)?,
                changes: change_set_to_pyobject(py, &change_set)?,
            }),
            Err(err) => Err(map_eval_error(py, &err)),
        }
    }

    // The schema methods: thin wrappers over the engine's `GraphTraversalSource` methods. A
    // schema (or patch) is a ``dict`` or JSON ``str`` in; a ``dict`` (the canonical schema
    // format) out. Each call is one unit of its own; an optional ``policy`` is asked like for a
    // script.
    /// ``graph.get_schema(*, policy=None)``: the stored schema as a ``dict``, or ``None`` when
    /// the graph stores none. Never inferred (that is ``infer_schema``).
    #[pyo3(signature = (*, policy=None))]
    fn get_schema(
        &self,
        py: Python<'_>,
        policy: Option<PyRef<'_, AccessPolicy>>,
    ) -> PyResult<Py<PyAny>> {
        let authorizer = effective_authorizer(policy.as_deref());
        let graph = Arc::clone(&self.inner);
        let result = py.detach(move || {
            let guard = graph.read();
            guard
                .traversal()
                .with_authorizer(authorizer)
                .get_schema()
                .map(|schema| schema.map(|s| s.to_json()))
                .map_err(|err| format_diagnostic(&*err))
        });
        match result.map_err(|text| map_diagnostic(py, &text))? {
            Some(json) => json_loads(py, &json),
            None => Ok(py.None()),
        }
    }

    /// ``graph.set_schema(schema, mode=None, *, policy=None)``: replaces the stored schema (a
    /// ``dict`` or JSON ``str`` in the schema format). ``mode`` (``"none"``, ``"open"``,
    /// ``"closed"``) replaces the schema's own mode: ``graph.set_schema(graph.infer_schema(),
    /// mode="closed")`` freezes the structure the graph has. Existing data is validated first;
    /// on violations nothing changes and ``GraphersalError`` carries the grouped report. An empty
    /// schema (``{}``) removes the schema. A schema that fills vectors (a vector rule added or
    /// changed) issues ``VectorFillWarning`` with the preview's text first.
    #[pyo3(signature = (schema, mode=None, *, policy=None))]
    fn set_schema(
        &self,
        py: Python<'_>,
        schema: &Bound<'_, PyAny>,
        mode: Option<String>,
        policy: Option<PyRef<'_, AccessPolicy>>,
    ) -> PyResult<()> {
        let text = json_text(py, schema)?;
        let authorizer = effective_authorizer(policy.as_deref());
        if text.contains("\"from\"") {
            let graph = Arc::clone(&self.inner);
            let (proposed, mode) = (text.clone(), mode.clone());
            let preview = Arc::clone(&authorizer);
            let warning = py
                .detach(move || {
                    let schema = schema_with_mode(&proposed, mode.as_deref())?;
                    graph
                        .read()
                        .traversal()
                        .with_authorizer(preview)
                        .validate_schema(&schema)
                        .map(|validation| validation.fill_warning())
                        .map_err(|err| format_diagnostic(&*err))
                })
                .map_err(|text| map_diagnostic(py, &text))?;
            warn_vector_fill(py, warning)?;
        }
        let graph = Arc::clone(&self.inner);
        py.detach(move || {
            let schema = schema_with_mode(&text, mode.as_deref())?;
            graph
                .write()
                .traversal_mut()
                .with_authorizer(authorizer)
                .set_schema(schema)
                .map_err(|err| format_diagnostic(&*err))
        })
        .map_err(|text| map_diagnostic(py, &text))
    }

    /// ``graph.patch_schema(patch, *, policy=None)``: applies a JSON Merge Patch (RFC 7396;
    /// ``dict`` or JSON ``str``) to the stored schema and stores the result like
    /// ``set_schema``; returns the new schema ``dict``.
    #[pyo3(signature = (patch, *, policy=None))]
    fn patch_schema(
        &self,
        py: Python<'_>,
        patch: &Bound<'_, PyAny>,
        policy: Option<PyRef<'_, AccessPolicy>>,
    ) -> PyResult<Py<PyAny>> {
        let text = json_text(py, patch)?;
        let authorizer = effective_authorizer(policy.as_deref());
        if text.contains("\"from\"") || text.contains("\"embedder\"") {
            let graph = Arc::clone(&self.inner);
            let proposed = text.clone();
            let preview = Arc::clone(&authorizer);
            let warning = py
                .detach(move || {
                    let patch =
                        SchemaPatch::from_json(&proposed).map_err(|err| format_diagnostic(&err))?;
                    graph
                        .read()
                        .traversal()
                        .with_authorizer(preview)
                        .validate_schema_patch(&patch)
                        .map(|validation| validation.fill_warning())
                        .map_err(|err| format_diagnostic(&*err))
                })
                .map_err(|text| map_diagnostic(py, &text))?;
            warn_vector_fill(py, warning)?;
        }
        let graph = Arc::clone(&self.inner);
        let json = py
            .detach(move || {
                let patch = SchemaPatch::from_json(&text).map_err(|err| format_diagnostic(&err))?;
                graph
                    .write()
                    .traversal_mut()
                    .with_authorizer(authorizer)
                    .patch_schema(&patch)
                    .map(|schema| schema.to_json())
                    .map_err(|err| format_diagnostic(&*err))
            })
            .map_err(|text| map_diagnostic(py, &text))?;
        json_loads(py, &json)
    }

    /// ``graph.validate_schema(schema, mode=None, *, policy=None)``: a dry run of
    /// ``set_schema(schema, mode)``: ``{"violations": {...}, "diff": [...], "compatible":
    /// bool}``. Nothing is applied.
    #[pyo3(signature = (schema, mode=None, *, policy=None))]
    fn validate_schema(
        &self,
        py: Python<'_>,
        schema: &Bound<'_, PyAny>,
        mode: Option<String>,
        policy: Option<PyRef<'_, AccessPolicy>>,
    ) -> PyResult<Py<PyAny>> {
        let text = json_text(py, schema)?;
        let authorizer = effective_authorizer(policy.as_deref());
        let graph = Arc::clone(&self.inner);
        let json = py
            .detach(move || {
                let schema = schema_with_mode(&text, mode.as_deref())?;
                graph
                    .read()
                    .traversal()
                    .with_authorizer(authorizer)
                    .validate_schema(&schema)
                    .map(|validation| value_json(&validation.to_value()))
                    .map_err(|err| format_diagnostic(&*err))
            })
            .map_err(|text| map_diagnostic(py, &text))?;
        json_loads(py, &json)
    }

    /// ``graph.validate_schema_patch(patch, *, policy=None)``: ``validate_schema`` of the stored
    /// schema patched with ``patch``. Nothing is applied.
    #[pyo3(signature = (patch, *, policy=None))]
    fn validate_schema_patch(
        &self,
        py: Python<'_>,
        patch: &Bound<'_, PyAny>,
        policy: Option<PyRef<'_, AccessPolicy>>,
    ) -> PyResult<Py<PyAny>> {
        let text = json_text(py, patch)?;
        let authorizer = effective_authorizer(policy.as_deref());
        let graph = Arc::clone(&self.inner);
        let json = py
            .detach(move || {
                let patch = SchemaPatch::from_json(&text).map_err(|err| format_diagnostic(&err))?;
                graph
                    .read()
                    .traversal()
                    .with_authorizer(authorizer)
                    .validate_schema_patch(&patch)
                    .map(|validation| value_json(&validation.to_value()))
                    .map_err(|err| format_diagnostic(&*err))
            })
            .map_err(|text| map_diagnostic(py, &text))?;
        json_loads(py, &json)
    }

    /// ``graph.infer_schema(*, policy=None)``: the schema the graph's data satisfies, as a
    /// ``dict`` (mode ``"none"``); stores nothing.
    #[pyo3(signature = (*, policy=None))]
    fn infer_schema(
        &self,
        py: Python<'_>,
        policy: Option<PyRef<'_, AccessPolicy>>,
    ) -> PyResult<Py<PyAny>> {
        let authorizer = effective_authorizer(policy.as_deref());
        let graph = Arc::clone(&self.inner);
        let json = py
            .detach(move || {
                graph
                    .read()
                    .traversal()
                    .with_authorizer(authorizer)
                    .infer_schema()
                    .map(|schema| schema.to_json())
                    .map_err(|err| format_diagnostic(&*err))
            })
            .map_err(|text| map_diagnostic(py, &text))?;
        json_loads(py, &json)
    }

    /// ``graph.diff_schema(schema, *, policy=None)``: ``{"changes": [...], "compatible":
    /// bool}`` from the stored schema to ``schema``.
    #[pyo3(signature = (schema, *, policy=None))]
    fn diff_schema(
        &self,
        py: Python<'_>,
        schema: &Bound<'_, PyAny>,
        policy: Option<PyRef<'_, AccessPolicy>>,
    ) -> PyResult<Py<PyAny>> {
        let text = json_text(py, schema)?;
        let authorizer = effective_authorizer(policy.as_deref());
        let graph = Arc::clone(&self.inner);
        let json = py
            .detach(move || {
                let schema =
                    GraphSchema::from_json(&text).map_err(|err| format_diagnostic(&err))?;
                graph
                    .read()
                    .traversal()
                    .with_authorizer(authorizer)
                    .diff_schema(&schema)
                    .map(|diff| value_json(&diff.to_value()))
                    .map_err(|err| format_diagnostic(&*err))
            })
            .map_err(|text| map_diagnostic(py, &text))?;
        json_loads(py, &json)
    }

    // Saved queries: thin wrappers over the DSL calls (`g.queries`, `g.get_query`,
    // `g.define_query`, `g.drop_query`, `g.query`), run like ``execute`` with the arguments
    // bound as script parameters (never spliced into the text; a ``dict`` travels as JSON text
    // and is read back with ``parse_json``). Each is one unit of its own.
    /// ``graph.queries(folder=None, *, policy=None)``: the saved queries (in ``folder`` and
    /// below it), each a ``dict`` with ``name``, ``folder``, ``description``, ``params``
    /// (``name``, ``schema``, ``default``, ``required``), ``status`` and ``error``, ``writes``,
    /// ``effects``, ``denied_writes``, ``language`` (``"rhai"`` or ``"gql"``) and ``dialect``.
    #[pyo3(signature = (folder=None, *, policy=None))]
    fn queries(
        &self,
        py: Python<'_>,
        folder: Option<String>,
        policy: Option<PyRef<'_, AccessPolicy>>,
    ) -> PyResult<Py<PyAny>> {
        let params = PyDict::new(py);
        params.set_item("query_folder", folder.unwrap_or_default())?;
        self.run(
            py,
            "g.queries(query_folder)",
            Some(&params),
            false,
            policy.as_deref(),
        )
    }

    /// ``graph.get_query(name, *, policy=None)``: the saved query ``name`` as a ``dict`` (the
    /// ``queries()`` entry plus ``body``, ``dialect``, ``meta``), or ``None``.
    #[pyo3(signature = (name, *, policy=None))]
    fn get_query(
        &self,
        py: Python<'_>,
        name: &str,
        policy: Option<PyRef<'_, AccessPolicy>>,
    ) -> PyResult<Py<PyAny>> {
        let params = PyDict::new(py);
        params.set_item("query_name", name)?;
        self.run(
            py,
            "g.get_query(query_name)",
            Some(&params),
            false,
            policy.as_deref(),
        )
    }

    /// ``graph.define_query(spec, *, policy=None)``: stores (or replaces) a saved query, like
    /// the DSL's ``g.define_query(#{..})``: ``spec`` is a ``dict`` with ``name``, ``body`` (the
    /// text of one Rhai function named like the query, or with ``"language": "gql"`` one plain
    /// GQL query whose ``$name`` parameters are exactly the declared ones), and optionally
    /// ``language`` (``"rhai"``, the default, or ``"gql"``), ``folder``, ``description``,
    /// ``params`` (parameter name -> a type name such as ``"integer"`` or ``{"schema": {...},
    /// "default": ...}``) and ``meta``. Raises ``GraphersalError`` with the engine's diagnostic
    /// when the definition is refused.
    #[pyo3(signature = (spec, *, policy=None))]
    fn define_query(
        &self,
        py: Python<'_>,
        spec: &Bound<'_, PyDict>,
        policy: Option<PyRef<'_, AccessPolicy>>,
    ) -> PyResult<()> {
        let params = PyDict::new(py);
        params.set_item("query_spec", json_text(py, spec.as_any())?)?;
        self.run(
            py,
            "g.define_query(parse_json(query_spec))",
            Some(&params),
            false,
            policy.as_deref(),
        )?;
        Ok(())
    }

    /// ``graph.drop_query(name, *, policy=None)``: removes the saved query; ``False`` when
    /// there is none.
    #[pyo3(signature = (name, *, policy=None))]
    fn drop_query(
        &self,
        py: Python<'_>,
        name: &str,
        policy: Option<PyRef<'_, AccessPolicy>>,
    ) -> PyResult<Py<PyAny>> {
        let params = PyDict::new(py);
        params.set_item("query_name", name)?;
        self.run(
            py,
            "g.drop_query(query_name)",
            Some(&params),
            false,
            policy.as_deref(),
        )
    }

    /// ``graph.define_compression(name, label, path, element="vertex", min_bytes=None,
    /// dictionary=None, codec=None, *, policy=None)``: stores (or replaces) a compression rule,
    /// like the DSL's ``g.define_compression(#{..})``: strings of ``element``s labelled
    /// ``label`` at ``path`` (a jpath of names such as ``"details.career.cv"``) are kept
    /// compressed in memory (``codec="lz4"``, the default: ``min_bytes`` 256 and a dictionary
    /// unless given) or stored once per distinct value (``codec="dedup"``: ``min_bytes`` 25, no
    /// dictionary) and read plain. The label's values are re-encoded at once.
    #[pyo3(signature = (name, label, path, element="vertex", min_bytes=None, dictionary=None, codec=None, *, policy=None))]
    #[allow(clippy::too_many_arguments)]
    fn define_compression(
        &self,
        py: Python<'_>,
        name: &str,
        label: &str,
        path: &str,
        element: &str,
        min_bytes: Option<i64>,
        dictionary: Option<bool>,
        codec: Option<&str>,
        policy: Option<PyRef<'_, AccessPolicy>>,
    ) -> PyResult<()> {
        let params = PyDict::new(py);
        params.set_item("rule_name", name)?;
        params.set_item("rule_label", label)?;
        params.set_item("rule_path", path)?;
        params.set_item("rule_element", element)?;
        let mut script = String::from(
            "g.define_compression(#{name: rule_name, label: rule_label, path: rule_path, \
             element: rule_element",
        );
        if let Some(min_bytes) = min_bytes {
            params.set_item("rule_min_bytes", min_bytes)?;
            script.push_str(", min_bytes: rule_min_bytes");
        }
        if let Some(dictionary) = dictionary {
            params.set_item("rule_dictionary", dictionary)?;
            script.push_str(", dictionary: rule_dictionary");
        }
        if let Some(codec) = codec {
            params.set_item("rule_codec", codec)?;
            script.push_str(", codec: rule_codec");
        }
        script.push_str("})");
        self.run(py, &script, Some(&params), false, policy.as_deref())?;
        Ok(())
    }

    /// ``graph.compressions(*, policy=None)``: the compression rules as a ``list`` of ``dict``
    /// (``name``, ``element``, ``label``, ``path``, ``codec``, ``min_bytes``, ``dictionary``,
    /// ``served``, ``reason`` when it is not applied (a codec, element kind or path of a newer
    /// version: kept, values stay plain) and the current ``compressed_values``,
    /// ``plain_bytes``, ``stored_bytes``, ``dictionary_bytes``, ``distinct_values``).
    #[pyo3(signature = (*, policy=None))]
    fn compressions(
        &self,
        py: Python<'_>,
        policy: Option<PyRef<'_, AccessPolicy>>,
    ) -> PyResult<Py<PyAny>> {
        self.run(py, "g.compressions()", None, false, policy.as_deref())
    }

    /// ``graph.drop_compression(name, *, policy=None)``: removes the compression rule and
    /// decompresses its values; ``False`` when there is none.
    #[pyo3(signature = (name, *, policy=None))]
    fn drop_compression(
        &self,
        py: Python<'_>,
        name: &str,
        policy: Option<PyRef<'_, AccessPolicy>>,
    ) -> PyResult<Py<PyAny>> {
        let params = PyDict::new(py);
        params.set_item("rule_name", name)?;
        self.run(
            py,
            "g.drop_compression(rule_name)",
            Some(&params),
            false,
            policy.as_deref(),
        )
    }

    /// ``graph.recompress(name, *, policy=None)``: rebuilds the rule's dictionary from the
    /// current values and re-encodes them; changes no data.
    #[pyo3(signature = (name, *, policy=None))]
    fn recompress(
        &self,
        py: Python<'_>,
        name: &str,
        policy: Option<PyRef<'_, AccessPolicy>>,
    ) -> PyResult<()> {
        let params = PyDict::new(py);
        params.set_item("rule_name", name)?;
        self.run(
            py,
            "g.recompress(rule_name)",
            Some(&params),
            false,
            policy.as_deref(),
        )?;
        Ok(())
    }

    /// ``graph.estimate_compression(label, path, element="vertex", min_bytes=None,
    /// dictionary=None, codec=None, *, policy=None)``: what compression rules for ``element``s
    /// labelled ``label`` at ``path`` would hold over the current values, like the DSL's
    /// ``g.estimate_compression(#{..})``, without changing the graph. ``min_bytes``,
    /// ``dictionary`` and ``codec`` (``"lz4"``, ``"dedup"`` or a list of them) narrow the
    /// candidates (default: ``lz4`` with and without a dictionary at 64, 128, 256, 512 and 1024
    /// bytes, plus the current rule's parameters). A ``dict`` with ``values_found``,
    /// ``string_values``, ``histogram``, ``candidates`` (``codec``, ``min_bytes``,
    /// ``dictionary``, ``compressed_values``, ``plain_bytes``, ``stored_bytes``,
    /// ``dictionary_bytes``, ``distinct_values``, ``saved_bytes``, ``ratio``,
    /// ``decode_bytes_per_sec``, ...), ``best`` (an index into ``candidates`` or ``None``),
    /// ``current`` and ``warnings``.
    #[pyo3(signature = (label, path, element="vertex", min_bytes=None, dictionary=None, codec=None, *, policy=None))]
    #[allow(clippy::too_many_arguments)]
    fn estimate_compression(
        &self,
        py: Python<'_>,
        label: &str,
        path: &str,
        element: &str,
        min_bytes: Option<i64>,
        dictionary: Option<bool>,
        codec: Option<Bound<'_, PyAny>>,
        policy: Option<PyRef<'_, AccessPolicy>>,
    ) -> PyResult<Py<PyAny>> {
        let params = PyDict::new(py);
        params.set_item("est_label", label)?;
        params.set_item("est_path", path)?;
        params.set_item("est_element", element)?;
        let mut script = String::from(
            "g.estimate_compression(#{label: est_label, path: est_path, element: est_element",
        );
        if let Some(min_bytes) = min_bytes {
            params.set_item("est_min_bytes", min_bytes)?;
            script.push_str(", min_bytes: est_min_bytes");
        }
        if let Some(dictionary) = dictionary {
            params.set_item("est_dictionary", dictionary)?;
            script.push_str(", dictionary: est_dictionary");
        }
        if let Some(codec) = codec {
            // A codec name, or a list of them (a script parameter is a scalar: one each).
            if let Ok(name) = codec.extract::<String>() {
                params.set_item("est_codec", name)?;
                script.push_str(", codec: est_codec");
            } else {
                let names: Vec<String> = codec.extract().map_err(|_| {
                    pyo3::exceptions::PyTypeError::new_err(
                        "codec is a codec name (\"lz4\", \"dedup\") or a list of them",
                    )
                })?;
                let mut list = Vec::with_capacity(names.len());
                for (at, name) in names.into_iter().enumerate() {
                    let key = format!("est_codec_{at}");
                    params.set_item(key.as_str(), name)?;
                    list.push(key);
                }
                script.push_str(&format!(", codec: [{}]", list.join(", ")));
            }
        }
        script.push_str("})");
        self.run(py, &script, Some(&params), false, policy.as_deref())
    }

    /// The memory report, like the DSL's ``g.memory_report(#{..})``: where the graph's memory
    /// goes per label and top-level property, without changing the graph. A ``dict`` with
    /// ``properties`` (one per element kind, label and path: ``elements``, ``value_types``,
    /// ``bytes``, ``sizes``, ``strings``, ``vectors``, ...), ``labels``, ``shapes``,
    /// ``indexes``, ``compression_rules``, ``structure``, ``property_bytes``,
    /// ``estimated_total_bytes``, ``tracked_live_bytes``, ``sampled``, ... (book page "Memory
    /// Usage"). ``max_elements``/``sample_every`` sample a large graph (figures extrapolated per
    /// label); ``limit`` keeps the property rows and the label rows with the most bytes (default
    /// 500), ``max_rows``/``max_labels`` one list each; the rows left out are counted with their
    /// bytes (``rows_omitted``, ``rows_omitted_bytes``, ``labels_omitted``, ...).
    #[pyo3(signature = (*, sample_every=None, max_elements=None, max_rows=None, max_labels=None, limit=None, policy=None))]
    #[allow(clippy::too_many_arguments)]
    fn memory_report(
        &self,
        py: Python<'_>,
        sample_every: Option<i64>,
        max_elements: Option<i64>,
        max_rows: Option<i64>,
        max_labels: Option<i64>,
        limit: Option<i64>,
        policy: Option<PyRef<'_, AccessPolicy>>,
    ) -> PyResult<Py<PyAny>> {
        let params = PyDict::new(py);
        let mut fields = Vec::new();
        for (key, value) in [
            ("sample_every", sample_every),
            ("max_elements", max_elements),
            ("max_rows", max_rows),
            ("max_labels", max_labels),
            ("limit", limit),
        ] {
            if let Some(value) = value {
                let name = format!("mr_{key}");
                params.set_item(name.as_str(), value)?;
                fields.push(format!("{key}: {name}"));
            }
        }
        let script = format!("g.memory_report(#{{{}}})", fields.join(", "));
        self.run(py, &script, Some(&params), false, policy.as_deref())
    }

    /// ``graph.create_index(label, key, element="vertex", name=None, type="memory-ordered", *,
    /// transform=None, policy=None)``: declares (or replaces) a property index, like the DSL's
    /// ``g.create_index(#{..})``: the ``element``s labelled ``label``, ordered by their
    /// top-level property ``key``; a ``has()`` on that key after ``has_label(label)`` then reads
    /// the index. ``name`` defaults to ``<label>_<key>`` (``<label>_<key>_edge`` for edges,
    /// then ``_to_lower`` with that transform), ``type`` to ``"memory-ordered"`` (the only index
    /// type this version knows). ``transform="to_lower"`` indexes the lowered text of string
    /// values for a case-insensitive search, ``has(key, __.to_lower().is("alice"))``. The index
    /// is built at once and rebuilt from the data whenever the graph is loaded. An unknown type
    /// or transform, or a key the graph's schema declares as values the index cannot hold (a
    /// vector, an array, an object; with ``to_lower`` any non-string), is refused.
    #[pyo3(signature = (label, key, element="vertex", name=None, r#type="memory-ordered", *, transform=None, policy=None))]
    #[allow(clippy::too_many_arguments)]
    fn create_index(
        &self,
        py: Python<'_>,
        label: &str,
        key: &str,
        element: &str,
        name: Option<&str>,
        r#type: &str,
        transform: Option<&str>,
        policy: Option<PyRef<'_, AccessPolicy>>,
    ) -> PyResult<()> {
        let params = PyDict::new(py);
        params.set_item("index_label", label)?;
        params.set_item("index_key", key)?;
        params.set_item("index_element", element)?;
        params.set_item("index_type", r#type)?;
        let mut spec = String::from(
            "label: index_label, key: index_key, element: index_element, type: index_type",
        );
        if let Some(name) = name {
            params.set_item("index_name", name)?;
            spec.push_str(", name: index_name");
        }
        if let Some(transform) = transform {
            params.set_item("index_transform", transform)?;
            spec.push_str(", transform: index_transform");
        }
        let script = format!("g.create_index(#{{{spec}}})");
        self.run(py, &script, Some(&params), false, policy.as_deref())?;
        Ok(())
    }

    /// ``graph.indexes(*, policy=None)``: the property indexes as a ``list`` of ``dict``
    /// (``name``, ``element``, ``label``, ``key``, ``type``, ``transform`` (``None`` without
    /// one), ``served``, ``reason`` when it is not served, and the current ``entries``,
    /// ``distinct_values``, ``skipped``, ``bytes``).
    #[pyo3(signature = (*, policy=None))]
    fn indexes(
        &self,
        py: Python<'_>,
        policy: Option<PyRef<'_, AccessPolicy>>,
    ) -> PyResult<Py<PyAny>> {
        self.run(py, "g.indexes()", None, false, policy.as_deref())
    }

    /// ``graph.drop_index(name, *, policy=None)``: drops the property index; ``False`` when
    /// there is none.
    #[pyo3(signature = (name, *, policy=None))]
    fn drop_index(
        &self,
        py: Python<'_>,
        name: &str,
        policy: Option<PyRef<'_, AccessPolicy>>,
    ) -> PyResult<Py<PyAny>> {
        let params = PyDict::new(py);
        params.set_item("index_name", name)?;
        self.run(
            py,
            "g.drop_index(index_name)",
            Some(&params),
            false,
            policy.as_deref(),
        )
    }

    /// ``graph.call_query(name, params=None, *, policy=None)``: calls the saved query ``name``
    /// with named arguments (``g.query(name, #{..})``; ``graph.query`` is the alias of
    /// ``execute``) and returns its result like ``execute`` (a traversal's results as a
    /// ``list``, a value as it is). The arguments are checked against the parameter schemas
    /// (defaults fill the omitted ones). A query whose body writes data (``writes`` in
    /// ``queries()``) changes only its inferred ``effects``, as one transaction; a query written
    /// in GQL returns its rows as ``dict``s like ``g.gql()``.
    #[pyo3(signature = (name, params=None, *, policy=None))]
    fn call_query(
        &self,
        py: Python<'_>,
        name: &str,
        params: Option<&Bound<'_, PyDict>>,
        policy: Option<PyRef<'_, AccessPolicy>>,
    ) -> PyResult<Py<PyAny>> {
        let bound = PyDict::new(py);
        bound.set_item("query_name", name)?;
        // A uuid.UUID or a date argument travels as its text (a date as its canonical ISO
        // 8601 text); the parameter's schema turns it back.
        let args = match params {
            Some(args) => {
                let texts = PyDict::new(py);
                for (key, value) in args.iter() {
                    if let Some(vector) = crate::vector::python_vector(&value) {
                        // A vector travels as its list of values; a vector parameter's schema
                        // turns it back (quantized to its element).
                        let values: Vec<f64> =
                            vector.iter_f32().map(crate::vector::short_f64).collect();
                        texts.set_item(key, values)?;
                        continue;
                    }
                    match crate::temporal::python_temporal_text(&value) {
                        Some(text) => texts.set_item(key, text)?,
                        None => texts.set_item(key, value)?,
                    }
                }
                let dumps = py.import("json")?.getattr("dumps")?;
                let kwargs = PyDict::new(py);
                kwargs.set_item("default", py.import("builtins")?.getattr("str")?)?;
                dumps.call((texts,), Some(&kwargs))?.extract::<String>()?
            }
            None => "{}".to_string(),
        };
        bound.set_item("query_args", args)?;
        self.run(
            py,
            "g.query(query_name, parse_json(query_args))",
            Some(&bound),
            false,
            policy.as_deref(),
        )
    }

    // The embedders (book pages "Vectors", "Embedding Models in a Store"): the built-in
    // `hash-ngram/1` plus the models of the graph's database (a Store's, a `.gsnap`'s).

    /// ``graph.embedders()``: this graph's embedders as a ``list`` of ``dict`` (``name``,
    /// ``model``, ``dimension``, ``element``, ``metric``, ``max_batch``, ``deterministic``,
    /// ``local``, ``default``), the default first: the built-in ``"hash-ngram/1"`` (always
    /// there) and the embedding models of its database, named by their repository. The same
    /// list as the DSL's ``g.embedders()``.
    fn embedders(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        self.run(py, "g.embedders()", None, false, None)
    }
}

/// A schema or patch argument as JSON text: a ``str`` as it is, anything else through
/// ``json.dumps`` (a ``dict``; a ``uuid.UUID`` is not JSON and fails there).
fn json_text(py: Python<'_>, value: &Bound<'_, PyAny>) -> PyResult<String> {
    if let Ok(text) = value.extract::<String>() {
        return Ok(text);
    }
    let dumps = py.import("json")?.getattr("dumps")?;
    dumps.call1((value,))?.extract()
}

/// JSON text as Python data (``json.loads``: ``dict`` keys keep the engine's order).
fn json_loads(py: Python<'_>, text: &str) -> PyResult<Py<PyAny>> {
    let loads = py.import("json")?.getattr("loads")?;
    Ok(loads.call1((text,))?.unbind())
}

/// An engine value (a validation report, a diff) as compact JSON text.
fn value_json(value: &ElementProperty) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
}

/// What ``Graph.dry_run`` returns: the script's ``result`` and the would-be ``changes``.
/// Immutable; more fields may be added later.
#[pyclass(module = "graphersal", frozen)]
pub struct DryRunResult {
    /// What ``execute`` would have returned, as native Python values.
    #[pyo3(get)]
    result: Py<PyAny>,
    /// The would-be change set as plain data: ``{"commit_seq": 0, "committed_at": 0,
    /// "node_sequence": .., "edge_sequence": .., "mutations": [...]}``.
    #[pyo3(get)]
    changes: Py<PyAny>,
}

#[pymethods]
impl DryRunResult {
    /// ``DryRunResult(result=<repr of the result>, changes=<N mutations>)``.
    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        let result = self.result.bind(py).repr()?;
        let mutations = self
            .changes
            .bind(py)
            .get_item("mutations")
            .and_then(|mutations| mutations.len())
            .unwrap_or(0);
        let noun = if mutations == 1 {
            "mutation"
        } else {
            "mutations"
        };
        Ok(format!(
            "DryRunResult(result={result}, changes=<{mutations} {noun}>)"
        ))
    }
}

/// Wraps a `!Send` value so it can cross `Python::detach`'s `Ungil` bound and come back from the
/// script worker thread (`script_thread::on_script_thread`).
///
/// `rhai::Dynamic`/`EvalAltResult` hold `Rc`, so `eval_value`/`eval_value_with_params`'s
/// `Result<Dynamic, Box<EvalAltResult>>` is not `Send`, and `Python::detach` (the GIL-release
/// point this method uses around a traversal — see the design spec's Concurrency section)
/// requires its closure's return type to be `Ungil`, which on stable Rust (no `nightly` pyo3
/// feature, not enabled here) is exactly `Send`. `detach` never spawns an OS thread; it only lets
/// other Python threads run while this thread blocks in Rust, so the wrapped `Rc`s are still only
/// ever touched by this one thread, before and after the call. Wrapping the return value this way
/// is the documented pattern for a `!Send` value that is sound to move across such a boundary.
///
/// The script itself runs on the calling thread's script worker (a large stack, see
/// `script_thread`): the value is created there and moved to the calling thread through a
/// channel after the job finished, so its `Rc`s are touched by one thread at a time, the
/// channel's send/receive ordering them (happens-before); the worker keeps no reference to it.
struct AssertUngil<T>(T);
unsafe impl<T> Send for AssertUngil<T> {}

/// Everything one script run needs, gathered while the GIL is held.
struct ScriptRequest {
    script: String,
    params: std::collections::HashMap<String, ElementProperty>,
    limits: ScriptLimits,
    /// The traversal presets of `set_limits` (`with_traversal_presets`).
    presets: Vec<(&'static str, ElementProperty)>,
    authorizer: Arc<dyn Authorizer>,
}

impl Graph {
    fn request(
        &self,
        script: &str,
        params: Option<&Bound<'_, PyDict>>,
        policy: Option<&AccessPolicy>,
    ) -> PyResult<ScriptRequest> {
        // A poisoned lock is an error, never the default limits (which may be looser than the
        // ones the host set).
        let limits = self
            .limits
            .lock()
            .map(|limits| (limits.script.clone(), limits.traversal_presets()))
            .map_err(|_| {
                pyo3::exceptions::PyRuntimeError::new_err(
                    "the graph's limits lock is poisoned: a thread panicked while it held it",
                )
            })?;
        let params = match params {
            None => Default::default(),
            Some(params) => python_params_to_element_properties(params)?,
        };
        let (limits, presets) = limits;
        Ok(ScriptRequest {
            script: script.to_string(),
            params,
            limits,
            presets,
            authorizer: effective_authorizer(policy),
        })
    }

    fn run(
        &self,
        py: Python<'_>,
        script: &str,
        params: Option<&Bound<'_, PyDict>>,
        atomic: bool,
        policy: Option<&AccessPolicy>,
    ) -> PyResult<Py<PyAny>> {
        let request = self.request(script, params, policy)?;
        let graph = Arc::clone(&self.inner);
        let AssertUngil(result) = py.detach(move || {
            on_script_thread(move || {
                let ScriptRequest {
                    script,
                    params,
                    limits,
                    presets,
                    authorizer,
                } = request;
                let eval = if atomic {
                    eval_value_atomic
                } else {
                    eval_value_with_limits
                };
                // A traversal returned without a terminal runs as with `.to_list()`: Python gets
                // data, never a traversal object (after the commit for `atomic=True`).
                // The engine `eval` builds serves the graph's embedding models, and the presets
                // of `set_limits` reach every traversal source of the script
                // (`with_traversal_presets`).
                AssertUngil(with_traversal_presets(&presets, || {
                    eval(graph, &script, params, &limits, authorizer)
                        .and_then(|value| materialize_traversals(value, limits.max_value_depth))
                }))
            })
        });
        match result {
            Ok(dynamic) => dynamic_to_pyobject(py, dynamic),
            Err(err) => Err(map_eval_error(py, &err)),
        }
    }
}

/// A schema from its JSON text, with its mode replaced by `mode` (`"none"`, `"open"`,
/// `"closed"`) when given; an error is the rendered diagnostic.
fn schema_with_mode(text: &str, mode: Option<&str>) -> Result<GraphSchema, String> {
    let schema = GraphSchema::from_json(text).map_err(|err| format_diagnostic(&err))?;
    match mode {
        None => Ok(schema),
        Some(name) => SchemaMode::parse(name)
            .map(|mode| schema.with_mode(mode))
            .map_err(|err| format_diagnostic(&err)),
    }
}

/// The ``schema=``/``mode=`` keywords of a file load: the schema's JSON text and the mode, or
/// `None` without a schema (a ``mode`` alone is an error: there is nothing to apply it to).
fn load_schema_argument(
    py: Python<'_>,
    schema: Option<&Bound<'_, PyAny>>,
    mode: Option<String>,
) -> PyResult<Option<(String, Option<String>)>> {
    match schema {
        Some(schema) if !schema.is_none() => Ok(Some((json_text(py, schema)?, mode))),
        _ if mode.is_some() => Err(PyValueError::new_err(
            "mode applies only together with schema=",
        )),
        _ => Ok(None),
    }
}

/// A new, empty graph with `text` (schema JSON, its mode replaced by `mode`) set on it; an
/// error is the rendered diagnostic.
fn new_graph_with_schema(text: &str, mode: Option<&str>) -> Result<EngineGraph, String> {
    let schema = schema_with_mode(text, mode)?;
    let graph = GraphSource::empty();
    graph
        .write()
        .set_schema(schema)
        .map_err(|err| format_diagnostic(&err))?;
    Ok(graph)
}

/// Issues the ``VectorFillWarning`` of a schema change that fills vectors (the preview's one
/// text, book page "Vector Rules"); an error when the warnings filter turns it into one.
fn warn_vector_fill(py: Python<'_>, warning: Option<String>) -> PyResult<()> {
    if let Some(warning) = warning {
        let text = std::ffi::CString::new(warning).unwrap_or_default();
        PyErr::warn(
            py,
            &py.get_type::<crate::error::VectorFillWarning>(),
            &text,
            1,
        )?;
    }
    Ok(())
}

/// Reports what a GraphSON import of `path` skipped as one ``UserWarning`` (nothing when it
/// skipped nothing).
fn warn_graphson_skips(py: Python<'_>, path: &str, report: &ImportReport) -> PyResult<()> {
    if report.has_skips() {
        let text = std::ffi::CString::new(format!("GraphSON import of '{path}': {report}"))
            .unwrap_or_default();
        PyErr::warn(
            py,
            &py.get_type::<pyo3::exceptions::PyUserWarning>(),
            &text,
            1,
        )?;
    }
    Ok(())
}
