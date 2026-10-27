//! The `graphersal.Graph` class: a thin wrapper around the engine's script entry points
//! (`graphersal::script::eval_value_with_limits`, `eval_value_atomic`, `eval_value_dry_run`),
//! per the design spec's Public API and Concurrency sections, and the `DryRunResult` it returns
//! from `dry_run`.

use std::sync::{Arc, Mutex};

use graphersal::error::format_diagnostic;
use graphersal::exec::CancelToken;
use graphersal::io::{GraphExport, GraphImport, ImportReport};
use graphersal::persist;
use graphersal::prelude::ElementProperty;
use graphersal::schema::{GraphSchema, SchemaMode, SchemaPatch};
use graphersal::script::{
    ScriptLimits, eval_value_atomic, eval_value_dry_run, eval_value_with_limits,
    materialize_traversals,
};
use graphersal::storage::ElementKind;
use graphersal::vector::EmbedJob;
use graphersal::{GraphSource, prelude::Graph as EngineGraph};
use graphersal::{auth::Authorizer, exec::ExecutionPolicy, storage::GraphStorage};
use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::convert::{
    change_set_to_pyobject, dynamic_to_pyobject, python_params_to_element_properties,
};
use crate::embedder::{PyEmbedder, RegistryCell, registry_of};
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
    limits: Mutex<ScriptLimits>,
    /// The embedders of its scripts and `embed_property` (shared with the `Store` it came from).
    embedders: RegistryCell,
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

/// The `memory.limit` value of a `memory_limit=` argument: a positive number of bytes or
/// `"auto"` (the cgroup memory limit minus the headroom); `None` is no budget.
fn memory_limit_value(limit: Option<&Bound<'_, PyAny>>) -> PyResult<Option<ElementProperty>> {
    let Some(limit) = limit else {
        return Ok(None);
    };
    if let Ok(text) = limit.extract::<String>() {
        if text.eq_ignore_ascii_case("auto") {
            return Ok(Some(ElementProperty::from("auto")));
        }
        return Err(PyValueError::new_err(format!(
            "memory_limit must be a positive number of bytes or \"auto\", got {text:?}"
        )));
    }
    match limit.extract::<i64>() {
        Ok(bytes) if bytes >= 1 => Ok(Some(ElementProperty::from(bytes))),
        Ok(bytes) => Err(PyValueError::new_err(format!(
            "memory_limit must be a positive number of bytes or \"auto\", got {bytes}"
        ))),
        Err(_) => Err(PyTypeError::new_err(
            "memory_limit must be an int (bytes), \"auto\" or None",
        )),
    }
}

/// Puts the memory budget into the execution policy of `inner` (a default every traversal runs
/// under; a script's own `g.with("memory.limit", ..)` needs `Update` on that option). Nothing
/// changes without a limit.
fn with_memory_budget(
    inner: EngineGraph,
    memory_limit: Option<&Bound<'_, PyAny>>,
    memory_headroom: Option<u64>,
) -> PyResult<EngineGraph> {
    let Some(limit) = memory_limit_value(memory_limit)? else {
        if memory_headroom.is_some() {
            return Err(PyValueError::new_err(
                "memory_headroom applies only to memory_limit=\"auto\"",
            ));
        }
        return Ok(inner);
    };
    let graph = inner.into_inner();
    let mut policy: ExecutionPolicy = graph.execution_policy().with_default("memory.limit", limit);
    if let Some(headroom) = memory_headroom {
        policy = policy.with_default(
            "memory.headroom",
            i64::try_from(headroom).unwrap_or(i64::MAX),
        );
    }
    Ok(EngineGraph::new(graph.with_execution_policy(policy)))
}

impl Graph {
    /// A Python graph over a graph another owner keeps (a `Store`'s), with its registry.
    pub(crate) fn from_shared(inner: Arc<EngineGraph>, embedders: RegistryCell) -> Self {
        Graph {
            inner,
            limits: Mutex::new(ScriptLimits::default()),
            embedders,
        }
    }

    fn wrap(inner: EngineGraph, embedders: RegistryCell) -> Self {
        Graph {
            inner: Arc::new(inner),
            limits: Mutex::new(ScriptLimits::default()),
            embedders,
        }
    }
}

#[pymethods]
impl Graph {
    /// ``Graph(*, memory_limit=None, memory_headroom=None)``: a new, empty graph.
    ///
    /// ``memory_limit`` is the memory budget of every traversal on this graph (``memory.limit``):
    /// a positive number of bytes, or ``"auto"`` for the cgroup memory limit of the process
    /// (Linux containers) minus ``memory_headroom`` bytes (default a tenth of the limit, at least
    /// 64 MiB). A traversal that would exceed it raises ``GraphersalError`` and is rolled back,
    /// instead of the process running out of memory. ``None`` (the default): no budget. The
    /// figure is the exact live Rust heap of the extension (it installs the tracking allocator),
    /// which does not count Python objects: with ``"auto"``, ``memory_headroom`` must also cover
    /// the Python side of the process.
    ///
    /// ``embedders`` is a list of ``Embedder`` (also ``Model2Vec``) the graph's scripts
    /// (``embed()``, text queries in ``nearest``/``similarity``) and ``embed_property`` call,
    /// beside the built-in ``"hash-ngram/1"``; ``default_embedder`` names the one used when a
    /// call names none (default ``"hash-ngram/1"``). A duplicate or reserved name raises
    /// ``ValueError``. Every graph constructor and loader takes the same two keywords.
    #[new]
    #[pyo3(signature = (*, memory_limit=None, memory_headroom=None, embedders=None, default_embedder=None))]
    fn new(
        memory_limit: Option<&Bound<'_, PyAny>>,
        memory_headroom: Option<u64>,
        embedders: Option<Vec<PyRef<'_, PyEmbedder>>>,
        default_embedder: Option<&str>,
    ) -> PyResult<Self> {
        let registry = registry_of(embedders, default_embedder)?;
        Ok(Graph::wrap(
            with_memory_budget(GraphSource::empty(), memory_limit, memory_headroom)?,
            registry,
        ))
    }

    /// ``Graph.tinkerpop_modern(*, memory_limit=None, memory_headroom=None)``: the standard
    /// TinkerPop "modern" toy graph (6 vertices, 6 edges). The keywords are those of ``Graph()``.
    #[staticmethod]
    #[pyo3(signature = (*, memory_limit=None, memory_headroom=None, embedders=None, default_embedder=None))]
    fn tinkerpop_modern(
        memory_limit: Option<&Bound<'_, PyAny>>,
        memory_headroom: Option<u64>,
        embedders: Option<Vec<PyRef<'_, PyEmbedder>>>,
        default_embedder: Option<&str>,
    ) -> PyResult<Self> {
        let registry = registry_of(embedders, default_embedder)?;
        Ok(Graph::wrap(
            with_memory_budget(
                GraphSource::tinkerpop_modern(),
                memory_limit,
                memory_headroom,
            )?,
            registry,
        ))
    }

    /// ``Graph.from_graphml(path, *, schema=None, mode=None, memory_limit=None,
    /// memory_headroom=None)``: loads a graph from a GraphML file (see also
    /// ``Graph.from_graphson``). Raises ``GraphersalError`` when the file cannot be read or
    /// parsed.
    ///
    /// GraphML carries no schema. ``schema`` (a ``dict`` or JSON ``str`` in the schema format) is
    /// set on the new, empty graph first (``mode``, ``"none"``/``"open"``/``"closed"``, replaces
    /// its own mode), then the file is imported into it as one unit: properties the schema
    /// declares ``uuid``, ``array`` or ``object`` come back with that type instead of as strings,
    /// and in ``open``/``closed`` a violation fails the load with the usual report. The other
    /// keywords are those of ``Graph()``; the budget applies to the traversals after the load,
    /// not to the load.
    #[staticmethod]
    #[pyo3(signature = (path, *, schema=None, mode=None, memory_limit=None, memory_headroom=None, embedders=None, default_embedder=None))]
    #[allow(clippy::too_many_arguments)]
    fn from_graphml(
        py: Python<'_>,
        path: &str,
        schema: Option<&Bound<'_, PyAny>>,
        mode: Option<String>,
        memory_limit: Option<&Bound<'_, PyAny>>,
        memory_headroom: Option<u64>,
        embedders: Option<Vec<PyRef<'_, PyEmbedder>>>,
        default_embedder: Option<&str>,
    ) -> PyResult<Self> {
        let registry = registry_of(embedders, default_embedder)?;
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
        Ok(Graph::wrap(
            with_memory_budget(inner, memory_limit, memory_headroom)?,
            registry,
        ))
    }

    /// ``Graph.from_graphson(path, *, schema=None, mode=None, memory_limit=None,
    /// memory_headroom=None)``: loads a graph from a GraphSON 3.0 file (one vertex per line,
    /// TinkerPop's default, or one ``{"vertices": [...]}`` document; 2.0 and untyped 1.0 read
    /// too). Data the import skips (dates, types Graphersal cannot hold, edges to missing
    /// vertices) is not an error: it is reported in one ``UserWarning``. Raises
    /// ``GraphersalError`` when the file cannot be read or parsed. ``schema`` and ``mode`` work
    /// as in ``Graph.from_graphml`` (set first, then the file imported as one unit; a violation
    /// in ``open``/``closed`` fails the load). The other keywords are those of ``Graph()``.
    #[staticmethod]
    #[pyo3(signature = (path, *, schema=None, mode=None, memory_limit=None, memory_headroom=None, embedders=None, default_embedder=None))]
    #[allow(clippy::too_many_arguments)]
    fn from_graphson(
        py: Python<'_>,
        path: &str,
        schema: Option<&Bound<'_, PyAny>>,
        mode: Option<String>,
        memory_limit: Option<&Bound<'_, PyAny>>,
        memory_headroom: Option<u64>,
        embedders: Option<Vec<PyRef<'_, PyEmbedder>>>,
        default_embedder: Option<&str>,
    ) -> PyResult<Self> {
        let registry = registry_of(embedders, default_embedder)?;
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
        Ok(Graph::wrap(
            with_memory_budget(inner, memory_limit, memory_headroom)?,
            registry,
        ))
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
        // The read lock is taken off the GIL (see `embedder.rs`).
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

    /// ``Graph.load(path, *, memory_limit=None, memory_headroom=None)``: reads a packed
    /// snapshot written by ``graph.save``. No schema validation (the checksums protect the
    /// data); a damaged file raises ``GraphersalError`` naming the section and byte. A numeric
    /// ``memory_limit`` also refuses a snapshot whose size estimate does not fit; the keywords
    /// are those of ``Graph()``.
    #[staticmethod]
    #[pyo3(signature = (path, *, memory_limit=None, memory_headroom=None, embedders=None, default_embedder=None))]
    fn load(
        py: Python<'_>,
        path: &str,
        memory_limit: Option<&Bound<'_, PyAny>>,
        memory_headroom: Option<u64>,
        embedders: Option<Vec<PyRef<'_, PyEmbedder>>>,
        default_embedder: Option<&str>,
    ) -> PyResult<Self> {
        let registry = registry_of(embedders, default_embedder)?;
        let mut options = persist::ReadOptions::new();
        if let Some(ElementProperty::Number(bytes)) = memory_limit_value(memory_limit)?
            && let Some(bytes) = bytes.as_i64()
        {
            options = options.with_memory_limit(u64::try_from(bytes).unwrap_or(0));
        }
        let source = path.to_string();
        let graph = py
            .detach(move || -> Result<_, String> {
                let file = std::fs::File::open(&source)
                    .map_err(|err| format!("Error: cannot open '{source}': {err}"))?;
                persist::read_snapshot(std::io::BufReader::with_capacity(1 << 20, file), &options)
                    .map_err(|err| format_diagnostic(&err))
            })
            .map_err(|text| map_diagnostic(py, &text))?;
        Ok(Graph::wrap(
            with_memory_budget(EngineGraph::new(graph), memory_limit, memory_headroom)?,
            registry,
        ))
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
    #[pyo3(signature = (snapshot, journals, *, commit_seq=None, time=None, mark=None, embedders=None, default_embedder=None))]
    #[allow(clippy::too_many_arguments)]
    fn recover(
        py: Python<'_>,
        snapshot: Option<String>,
        journals: Vec<String>,
        commit_seq: Option<u64>,
        time: Option<i64>,
        mark: Option<String>,
        embedders: Option<Vec<PyRef<'_, PyEmbedder>>>,
        default_embedder: Option<&str>,
    ) -> PyResult<(Self, Py<PyAny>)> {
        let registry = registry_of(embedders, default_embedder)?;
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
        let graph = Graph::wrap(EngineGraph::new(recovered.graph), registry);
        Ok((graph, report.into_any().unbind()))
    }

    /// ``graph.set_limits(*, max_operations=None, max_string_size=None, max_array_size=None,
    /// max_map_size=None, max_value_depth=None, unlimited=False)``: changes the resource limits
    /// of the scripts this graph runs. A new graph runs under the SAFE defaults (10M operations,
    /// 16 MiB strings, 100k array items, 100k map entries, 128 levels of nested values);
    /// ``unlimited=True`` starts from no limits instead (the value depth stays 128). Each given
    /// keyword then overrides one field (``0`` means unlimited; for ``max_value_depth`` the
    /// default, since the depth is never unlimited). A script that hits a limit raises
    /// ``GraphersalError`` whose ``help`` names the field to raise.
    #[pyo3(signature = (*, max_operations=None, max_string_size=None, max_array_size=None, max_map_size=None, max_value_depth=None, unlimited=false))]
    fn set_limits(
        &self,
        max_operations: Option<u64>,
        max_string_size: Option<usize>,
        max_array_size: Option<usize>,
        max_map_size: Option<usize>,
        max_value_depth: Option<usize>,
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
        // The whole value is replaced, so a lock poisoned by an earlier panic holds a
        // consistent value again afterwards: written through, then the poison cleared.
        *self
            .limits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = limits;
        self.limits.clear_poison();
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
            authorizer,
        } = self.request(script, params, policy.as_deref())?;
        let graph = Arc::clone(&self.inner);
        let embedders = self.embedders.current();
        let AssertUngil(result) = py.detach(move || {
            on_script_thread(move || {
                AssertUngil(
                    embedders
                        .scope(|| eval_value_dry_run(graph, &script, params, &limits, authorizer)),
                )
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
    /// schema (``{}``) removes the schema.
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
    /// label), ``max_rows``/``max_labels`` keep the rows with the most bytes.
    #[pyo3(signature = (*, sample_every=None, max_elements=None, max_rows=None, max_labels=None, policy=None))]
    fn memory_report(
        &self,
        py: Python<'_>,
        sample_every: Option<i64>,
        max_elements: Option<i64>,
        max_rows: Option<i64>,
        max_labels: Option<i64>,
        policy: Option<PyRef<'_, AccessPolicy>>,
    ) -> PyResult<Py<PyAny>> {
        let params = PyDict::new(py);
        let mut fields = Vec::new();
        for (key, value) in [
            ("sample_every", sample_every),
            ("max_elements", max_elements),
            ("max_rows", max_rows),
            ("max_labels", max_labels),
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

    // The embedders (book page "Vectors", section "Embeddings"). The registry is the graph's
    // (shared with the `Store` it came from); each run takes the registry of its start.

    /// ``graph.register_embedder(embedder, *, default=False)``: adds an ``Embedder`` (also a
    /// ``Model2Vec``) to this graph's embedders (``default=True`` makes it the default). A
    /// running execution keeps the embedders it started with; the next one sees the new
    /// registry. A name already registered, or a reserved one (``"hash-ngram"``,
    /// ``"hash-ngram/*"``), raises ``ValueError``.
    #[pyo3(signature = (embedder, *, default=false))]
    fn register_embedder(&self, embedder: PyRef<'_, PyEmbedder>, default: bool) -> PyResult<()> {
        // The registry lock is never held while waiting for the GIL or a graph lock: the swap
        // only clones `Arc`s.
        self.embedders
            .register(Arc::clone(&embedder.inner), default)
    }

    /// ``graph.embedders()``: this graph's embedders as a ``list`` of ``dict`` (``name``,
    /// ``model``, ``dimension``, ``element``, ``metric``, ``max_batch``, ``deterministic``,
    /// ``local``, ``default``), the default first; the built-in ``"hash-ngram/1"`` is always
    /// there. The same list as the DSL's ``g.embedders()``.
    fn embedders(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        self.run(py, "g.embedders()", None, false, None)
    }

    /// ``graph.embed_property(*, label, source, target, element="vertex", embedder=None,
    /// only_missing=True, batch=None)``: embeds the text property ``source`` of every
    /// ``element`` (``"vertex"`` or ``"edge"``) labelled ``label`` and stores the vector in its
    /// property ``target``, page by page (``batch`` elements per page, default the embedder's
    /// ``max_batch``): the texts are read under a short read lock, the embedder runs without
    /// any lock (other threads keep reading and writing), each page is written as one unit.
    /// ``embedder`` names a registered embedder (default: the graph's default);
    /// ``only_missing=False`` embeds again the elements that have ``target`` already.
    ///
    /// The graph needs a schema attached (mode ``open`` or ``closed``): the job declares
    /// ``target`` there as vectors of the embedder's model (its ``embedder`` keyword), which is
    /// what a text query (``nearest("emb", "a question", 5)``) embeds with later. Returns the
    /// report ``dict``: ``embedded``, ``skipped_no_text``, ``not_text``, ``skipped_changed``,
    /// ``skipped_removed``, ``units``, ``schema_changed``, ``elapsed_ms``, ``embedder``,
    /// ``model``. A failure raises ``EmbedPropertyError`` (a ``GraphersalError``) whose
    /// ``report`` holds the counts up to the failure: earlier pages stay, the failing one is
    /// rolled back, and a second run continues where it stopped. The GIL is released for the
    /// whole job; Ctrl+C (``KeyboardInterrupt``) cancels it between pages and is raised with
    /// the same ``report`` attribute.
    #[pyo3(signature = (*, label, source, target, element="vertex", embedder=None, only_missing=true, batch=None))]
    #[allow(clippy::too_many_arguments)]
    fn embed_property(
        &self,
        py: Python<'_>,
        label: &str,
        source: &str,
        target: &str,
        element: &str,
        embedder: Option<&str>,
        only_missing: bool,
        batch: Option<usize>,
    ) -> PyResult<Py<PyAny>> {
        let kind = match element {
            "vertex" => ElementKind::Vertex,
            "edge" => ElementKind::Edge,
            other => {
                return Err(PyValueError::new_err(format!(
                    "element must be \"vertex\" or \"edge\", got {other:?}"
                )));
            }
        };
        let registry = self.embedders.current();
        // The embedder of the report, resolved like the job does (an unknown name fails in the
        // job, with its diagnostic).
        let resolved = match embedder {
            None => Some(Arc::clone(registry.default_embedder())),
            Some(name) => registry.get(name).cloned(),
        };
        let mut job = EmbedJob::new(label, source, target)
            .element(kind)
            .only_missing(only_missing);
        if let Some(name) = embedder {
            job = job.embedder(name);
        }
        if let Some(batch) = batch {
            job = job.batch(batch);
        }
        let graph = Arc::clone(&self.inner);
        let cancel = CancelToken::new();
        let job_cancel = cancel.clone();
        let (outcome, interrupted) = py.detach(move || {
            let (done, finished) = std::sync::mpsc::channel();
            std::thread::scope(|threads| {
                threads.spawn(move || {
                    let result = graph.embed_property(&job, &registry, &job_cancel, &mut |_| {});
                    let _ = done.send(result);
                });
                let mut interrupted = None;
                loop {
                    match finished.recv_timeout(std::time::Duration::from_millis(50)) {
                        Ok(result) => return (Some(result), interrupted),
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                            // Signals (Ctrl+C) reach Python only through `check_signals`, on
                            // the main thread, with the GIL: taken briefly, never while
                            // waiting for a graph lock.
                            if interrupted.is_none()
                                && let Some(Err(err)) = Python::try_attach(|py| py.check_signals())
                            {
                                cancel.cancel();
                                interrupted = Some(err);
                            }
                        }
                        // The job thread panicked (a bug): the scope resumes the panic.
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                            return (None, interrupted);
                        }
                    }
                }
            })
        });
        let report_of = |report: &graphersal::vector::EmbedReport| -> PyResult<Py<PyAny>> {
            let dict = PyDict::new(py);
            dict.set_item("embedded", report.embedded)?;
            dict.set_item("skipped_no_text", report.skipped_no_text)?;
            dict.set_item("not_text", report.not_text)?;
            dict.set_item("skipped_changed", report.skipped_changed)?;
            dict.set_item("skipped_removed", report.skipped_removed)?;
            dict.set_item("units", report.units)?;
            dict.set_item("schema_changed", report.schema_changed)?;
            dict.set_item(
                "elapsed_ms",
                u64::try_from(report.elapsed.as_millis()).unwrap_or(u64::MAX),
            )?;
            dict.set_item(
                "embedder",
                resolved.as_ref().map(|e| e.info().name.to_string()),
            )?;
            dict.set_item(
                "model",
                resolved.as_ref().map(|e| e.info().model.to_string()),
            )?;
            Ok(dict.into_any().unbind())
        };
        let Some(outcome) = outcome else {
            return Err(map_diagnostic(
                py,
                "Error: the embed_property job ended without a result",
            ));
        };
        match (outcome, interrupted) {
            (Ok(report), None) => report_of(&report),
            (Ok(report), Some(err)) => {
                let _ = err.value(py).setattr("report", report_of(&report)?);
                Err(err)
            }
            (Err(failed), Some(err)) => {
                let _ = err.value(py).setattr("report", report_of(&failed.report)?);
                Err(err)
            }
            (Err(failed), None) => {
                let report = report_of(&failed.report)?;
                Err(crate::error::embed_property_error(
                    py,
                    &format_diagnostic(&*failed.error),
                    report,
                ))
            }
        }
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
            .map(|limits| limits.clone())
            .map_err(|_| {
                pyo3::exceptions::PyRuntimeError::new_err(
                    "the graph's limits lock is poisoned: a thread panicked while it held it",
                )
            })?;
        let params = match params {
            None => Default::default(),
            Some(params) => python_params_to_element_properties(params)?,
        };
        Ok(ScriptRequest {
            script: script.to_string(),
            params,
            limits,
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
        // The registry now: a later `register_embedder` does not reach this run.
        let embedders = self.embedders.current();
        let AssertUngil(result) = py.detach(move || {
            on_script_thread(move || {
                let ScriptRequest {
                    script,
                    params,
                    limits,
                    authorizer,
                } = request;
                let eval = if atomic {
                    eval_value_atomic
                } else {
                    eval_value_with_limits
                };
                // A traversal returned without a terminal runs as with `.to_list()`: Python gets
                // data, never a traversal object (after the commit for `atomic=True`).
                // The engine and scope `eval` builds take the graph's registry
                // (`Embedders::scope`).
                AssertUngil(embedders.scope(|| {
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
