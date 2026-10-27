//! The `graphersal.Store` class: one graph kept in one directory (`graphersal::persist::Store`,
//! book part "Persistence", page "Python"): every commit of `store.graph` is written to the
//! store's write-ahead log before it returns; checkpoints, marks, listing, verify, fork, full and
//! incremental backups (a backup opens read-only until it is restored; every checksum is read
//! in the store and in the copy), backups from memory of a damaged store, the damage policy
//! fixed at creation, in-place rollback with the attic, maintenance mode and repair, and the
//! embedding models the store keeps (listed with their licences, exported or left out, loaded
//! at open for the models the schema names: format spec 4.5).

use std::sync::{Arc, Mutex};

use graphersal::changes::Mark;
use graphersal::error::format_diagnostic;
use graphersal::persist::{
    self, BackupMarker, BackupMode, DamagePolicy, PersistError, RecoveryTarget, RollbackMode,
    SnapshotInfo, StoreFailure, StoreOptions,
};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use crate::embedder::{PyEmbedder, RegistryCell, registry_of};
use crate::error::map_diagnostic;
use crate::graph::Graph;

/// A Store directory, open read-write (its lock held) until ``close()``.
#[pyclass(module = "graphersal")]
pub struct Store {
    inner: Arc<Mutex<Option<persist::Store>>>,
    /// The embedders of `store.graph` (every `Graph` it hands out shares them).
    embedders: RegistryCell,
}

fn store_error(py: Python<'_>, err: PersistError) -> PyErr {
    let (text, cause) = failure_parts(&err);
    raise_with_cause(py, &text, cause)
}

/// A store failure, rendered, with the stable name of its cause (`StoreFailure::name`) when it
/// is a `PersistError::Store`: it crosses `py.detach` as plain data.
type Failure = (String, Option<&'static str>);

fn failure_parts(err: &PersistError) -> Failure {
    let cause = match err {
        PersistError::Store { cause, .. } => Some(cause.name()),
        _ => None,
    };
    (format_diagnostic(err), cause)
}

/// The Python exception of a store failure: `GraphersalError` (or a subclass) with `help` and
/// `cause` (the `StoreFailure` name, `None` for every other failure).
fn raise_with_cause(py: Python<'_>, text: &str, cause: Option<&'static str>) -> PyErr {
    let err = map_diagnostic(py, text);
    let _ = err.value(py).setattr("cause", cause);
    err
}

fn snapshot_dict<'py>(py: Python<'py>, s: &SnapshotInfo) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("commit_seq", s.commit_seq)?;
    dict.set_item("name", &s.name)?;
    dict.set_item("created_at", s.created_at)?;
    dict.set_item("last_commit_at", s.last_commit_at)?;
    dict.set_item("vertex_count", s.vertex_count)?;
    dict.set_item("edge_count", s.edge_count)?;
    Ok(dict)
}

/// Marks a fork or repair did not carry: ``[{"name", "commit_seq"}]`` in order.
fn marks_list<'py>(py: Python<'py>, marks: &[Mark]) -> PyResult<Bound<'py, PyList>> {
    let list = PyList::empty(py);
    for mark in marks {
        let dict = PyDict::new(py);
        dict.set_item("name", mark.name())?;
        dict.set_item("commit_seq", mark.commit_seq())?;
        list.append(dict)?;
    }
    Ok(list)
}

/// One stored model as ``store.models()`` returns it.
fn model_dict<'py>(
    py: Python<'py>,
    model: &persist::StoredModel,
    used: &[String],
) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("id", &model.id)?;
    dict.set_item("identity", &model.identity)?;
    dict.set_item("repo", &model.repo)?;
    dict.set_item("revision", &model.revision)?;
    dict.set_item("commit", &model.commit)?;
    dict.set_item("license", &model.license)?;
    dict.set_item("license_text", &model.license_text)?;
    dict.set_item("gated", model.gated)?;
    let bases = PyList::empty(py);
    for base in &model.base_models {
        let entry = PyDict::new(py);
        entry.set_item("repo", &base.repo)?;
        entry.set_item("license", &base.license)?;
        bases.append(entry)?;
    }
    dict.set_item("base_models", bases)?;
    dict.set_item("bytes", model.bytes)?;
    let files = PyList::empty(py);
    for file in &model.files {
        let entry = PyDict::new(py);
        entry.set_item("name", &file.name)?;
        entry.set_item("role", &file.role)?;
        entry.set_item("bytes", file.bytes)?;
        entry.set_item("sha256", &file.sha256)?;
        files.append(entry)?;
    }
    dict.set_item("files", files)?;
    let hex = model.id.strip_prefix("model2vec-").unwrap_or_default();
    let in_use = used.iter().any(|named| {
        named
            .split('?')
            .next()
            .and_then(|base| base.rsplit_once('@'))
            .is_some_and(|(_, h)| h == hex)
    });
    dict.set_item("used_by_schema", in_use)?;
    dict.set_item("problem", &model.problem)?;
    Ok(dict)
}

fn marker_dict<'py>(py: Python<'py>, m: &BackupMarker) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("commit_seq", m.commit_seq)?;
    dict.set_item("taken_at", m.taken_at)?;
    Ok(dict)
}

/// The options of a store created with the damage policy named `on_damage`.
fn create_options(on_damage: &str) -> PyResult<StoreOptions> {
    let policy = DamagePolicy::parse(on_damage).map_err(PyValueError::new_err)?;
    Ok(StoreOptions::new().with_damage_policy(policy))
}

fn target(
    commit_seq: Option<u64>,
    time: Option<i64>,
    mark: Option<String>,
) -> PyResult<RecoveryTarget> {
    Ok(match (commit_seq, time, mark) {
        (None, None, None) => RecoveryTarget::Latest,
        (Some(seq), None, None) => RecoveryTarget::CommitSeq(seq),
        (None, Some(time), None) => RecoveryTarget::Time(time),
        (None, None, Some(name)) => RecoveryTarget::Mark(name),
        _ => {
            return Err(PyValueError::new_err(
                "give at most one of commit_seq, time and mark",
            ));
        }
    })
}

impl Store {
    /// Runs `f` on the open store; a closed one raises.
    fn with<T: Send>(
        &self,
        py: Python<'_>,
        f: impl FnOnce(&persist::Store) -> Result<T, PersistError> + Send,
    ) -> PyResult<T> {
        let inner = Arc::clone(&self.inner);
        let result = py.detach(move || {
            let guard = inner.lock().map_err(|_| -> Failure {
                (
                    "Error: the store lock is poisoned".to_string(),
                    Some(StoreFailure::LockPoisoned.name()),
                )
            })?;
            let store = guard.as_ref().ok_or_else(|| -> Failure {
                (
                    "Error: the store is closed".to_string(),
                    Some(StoreFailure::Closed.name()),
                )
            })?;
            f(store).map_err(|err| failure_parts(&err))
        });
        result.map_err(|(text, cause)| raise_with_cause(py, &text, cause))
    }

    fn wrap(store: persist::Store, embedders: RegistryCell) -> Self {
        Store {
            inner: Arc::new(Mutex::new(Some(store))),
            embedders,
        }
    }

    /// [`Store::wrap`] of an opened store: the models it keeps that its schema names are
    /// registered beside the ``embedders=`` ones (which win); a model the schema names that
    /// neither provides is a ``UserWarning`` naming the command that adds it.
    fn wrap_opened(
        py: Python<'_>,
        store: persist::Store,
        embedders: RegistryCell,
    ) -> PyResult<Self> {
        #[cfg(feature = "model2vec")]
        {
            let registered = embedders.current();
            let found = py.detach(|| store.stored_embedders(&registered));
            if !found.loaded.is_empty() {
                embedders.replace(found.embedders);
            }
            for note in &found.notes {
                let category = py.get_type::<pyo3::exceptions::PyUserWarning>();
                let message = std::ffi::CString::new(note.replace('\0', " "))
                    .map_err(|err| PyValueError::new_err(err.to_string()))?;
                PyErr::warn(py, &category, &message, 1)?;
            }
        }
        #[cfg(not(feature = "model2vec"))]
        let _ = py;
        Ok(Store::wrap(store, embedders))
    }
}

#[pymethods]
impl Store {
    /// ``Store.create(path, *, on_damage="maintenance")``: creates a store for an empty graph
    /// in ``path`` (missing or empty) and opens it. ``on_damage`` is the store's damage policy,
    /// fixed for its life: ``"maintenance"`` (damage found on disk while it is open makes it
    /// read-only) or ``"continue"`` (it keeps committing and reports the damage).
    #[staticmethod]
    #[pyo3(signature = (path, *, on_damage="maintenance", embedders=None, default_embedder=None))]
    fn create(
        py: Python<'_>,
        path: &str,
        on_damage: &str,
        embedders: Option<Vec<PyRef<'_, PyEmbedder>>>,
        default_embedder: Option<&str>,
    ) -> PyResult<Self> {
        let registry = registry_of(embedders, default_embedder)?;
        let path = path.to_string();
        let options = create_options(on_damage)?;
        let store = py
            .detach(move || persist::Store::create(&path, options))
            .map_err(|err| store_error(py, err))?;
        Ok(Store::wrap(store, registry))
    }

    /// ``Store.open(path)``: opens the store in ``path`` read-write and runs its recovery
    /// (interrupted operations completed, the WAL replayed, an interrupted last write cut).
    /// One writer per directory: a second open raises.
    #[staticmethod]
    #[pyo3(signature = (path, *, embedders=None, default_embedder=None))]
    fn open(
        py: Python<'_>,
        path: &str,
        embedders: Option<Vec<PyRef<'_, PyEmbedder>>>,
        default_embedder: Option<&str>,
    ) -> PyResult<Self> {
        let registry = registry_of(embedders, default_embedder)?;
        let path = path.to_string();
        let store = py
            .detach(move || persist::Store::open(&path, StoreOptions::new()))
            .map_err(|err| store_error(py, err))?;
        Store::wrap_opened(py, store, registry)
    }

    /// ``Store.create_file(path)``: creates a SINGLE-FILE store (the whole store in the one
    /// file ``path``, whatever its extension) for an empty graph and opens it. ``Store.create``
    /// of a path ending in ``.gstore`` makes one too.
    #[staticmethod]
    #[pyo3(signature = (path, *, on_damage="maintenance", embedders=None, default_embedder=None))]
    fn create_file(
        py: Python<'_>,
        path: &str,
        on_damage: &str,
        embedders: Option<Vec<PyRef<'_, PyEmbedder>>>,
        default_embedder: Option<&str>,
    ) -> PyResult<Self> {
        let registry = registry_of(embedders, default_embedder)?;
        let path = path.to_string();
        let options = create_options(on_damage)?;
        let store = py
            .detach(move || persist::Store::create_file(&path, options))
            .map_err(|err| store_error(py, err))?;
        Ok(Store::wrap(store, registry))
    }

    /// ``Store.open_file(path)``: opens the single-file store ``path`` read-write
    /// (``Store.open`` of an existing file does the same).
    #[staticmethod]
    #[pyo3(signature = (path, *, embedders=None, default_embedder=None))]
    fn open_file(
        py: Python<'_>,
        path: &str,
        embedders: Option<Vec<PyRef<'_, PyEmbedder>>>,
        default_embedder: Option<&str>,
    ) -> PyResult<Self> {
        let registry = registry_of(embedders, default_embedder)?;
        let path = path.to_string();
        let store = py
            .detach(move || persist::Store::open_file(&path, StoreOptions::new()))
            .map_err(|err| store_error(py, err))?;
        Store::wrap_opened(py, store, registry)
    }

    /// ``Store.convert(source, target, *, single_file=False)``: copies the closed store in
    /// ``source`` into ``target`` (a new directory, or a single file: a ``.gstore`` path or
    /// ``single_file=True``), byte for byte, verified. Returns ``{"files", "bytes"}``.
    #[staticmethod]
    #[pyo3(signature = (source, target, *, single_file=false))]
    fn convert(
        py: Python<'_>,
        source: &str,
        target: &str,
        single_file: bool,
    ) -> PyResult<Py<PyAny>> {
        let (source, target) = (source.to_string(), target.to_string());
        let (files, bytes) = py
            .detach(move || {
                let to: Arc<dyn persist::StoreDir> = if single_file {
                    Arc::new(persist::SingleFileDir::new(&target))
                } else {
                    persist::store_dir_at(&target)
                };
                persist::Store::convert(source.as_str(), to)
            })
            .map_err(|err| store_error(py, err))?;
        let dict = PyDict::new(py);
        dict.set_item("files", files)?;
        dict.set_item("bytes", bytes)?;
        Ok(dict.into_any().unbind())
    }

    /// ``Store.upgrade(source, target=None, *, in_place=False, single_file=False)``: converts
    /// a store (or backup, or ``.gstore`` file) written by an older Graphersal (0.1.x) to the
    /// current format, without the older Graphersal. With ``target``: a new store there (a new
    /// directory, or a single file: a ``.gstore`` path or ``single_file=True``); ``source`` is
    /// not changed. With ``in_place=True``: ``source`` itself (back it up first). Everything
    /// is carried (graph, schema, catalog, history, marks, attic, identity); the source is
    /// verified first and a damaged one raises (``err.cause == "damaged_source"``: repair it
    /// instead). Returns ``{"from_version", "to_version", "already_current", "in_place",
    /// "backup", "files", "bytes"}``.
    #[staticmethod]
    #[pyo3(signature = (source, target=None, *, in_place=false, single_file=false))]
    fn upgrade(
        py: Python<'_>,
        source: &str,
        target: Option<&str>,
        in_place: bool,
        single_file: bool,
    ) -> PyResult<Py<PyAny>> {
        let source = source.to_string();
        let target = match (target, in_place) {
            (Some(target), false) => Some(target.to_string()),
            (None, true) => None,
            (Some(_), true) => {
                return Err(PyValueError::new_err(
                    "Store.upgrade takes a target OR in_place=True, not both",
                ));
            }
            (None, false) => {
                return Err(PyValueError::new_err(
                    "Store.upgrade needs a target (a new store) or in_place=True",
                ));
            }
        };
        let report = py
            .detach(move || match target {
                Some(target) => {
                    let to: Arc<dyn persist::StoreDir> = if single_file {
                        Arc::new(persist::SingleFileDir::new(&target))
                    } else {
                        persist::store_dir_at(&target)
                    };
                    persist::Store::upgrade(source.as_str(), to)
                }
                None => persist::Store::upgrade_in_place(source.as_str()),
            })
            .map_err(|err| store_error(py, err))?;
        let dict = PyDict::new(py);
        dict.set_item("from_version", report.from_version)?;
        dict.set_item("to_version", report.to_version)?;
        dict.set_item("already_current", report.already_current)?;
        dict.set_item("in_place", report.in_place)?;
        match &report.backup {
            Some(marker) => dict.set_item("backup", marker_dict(py, marker)?)?,
            None => dict.set_item("backup", py.None())?,
        }
        dict.set_item("files", report.files_copied)?;
        dict.set_item("bytes", report.bytes_copied)?;
        Ok(dict.into_any().unbind())
    }

    /// ``Store.open_backup(path)``: opens the BACKUP in ``path`` read-only (no lock, nothing
    /// written): listing, verify, fork and backups of it work; every write raises.
    #[staticmethod]
    #[pyo3(signature = (path, *, embedders=None, default_embedder=None))]
    fn open_backup(
        py: Python<'_>,
        path: &str,
        embedders: Option<Vec<PyRef<'_, PyEmbedder>>>,
        default_embedder: Option<&str>,
    ) -> PyResult<Self> {
        let registry = registry_of(embedders, default_embedder)?;
        let path = path.to_string();
        let store = py
            .detach(move || persist::Store::open_backup(&path, StoreOptions::new()))
            .map_err(|err| store_error(py, err))?;
        Store::wrap_opened(py, store, registry)
    }

    /// ``Store.restore_backup(path)``: makes the backup in ``path`` the LIVE store, in place
    /// (the same graph id; then ``Store.open(path)``). Returns the cleared marker
    /// ``{"commit_seq", "taken_at"}``.
    #[staticmethod]
    fn restore_backup(py: Python<'_>, path: &str) -> PyResult<Py<PyAny>> {
        let path = path.to_string();
        let marker = py
            .detach(move || persist::Store::restore_backup(&path))
            .map_err(|err| store_error(py, err))?;
        Ok(marker_dict(py, &marker)?.into_any().unbind())
    }

    /// ``store.backup_marker``: ``None`` for a store; ``{"commit_seq", "taken_at"}`` for a
    /// backup opened with ``Store.open_backup``.
    #[getter]
    fn backup_marker(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        let marker = self.with(py, |store| Ok(store.backup_marker()))?;
        marker
            .map(|m| Ok(marker_dict(py, &m)?.into_any().unbind()))
            .transpose()
    }

    /// ``store.graph``: the store's graph; every commit of it is durable before it returns.
    #[getter]
    fn graph(&self, py: Python<'_>) -> PyResult<Graph> {
        let embedders = self.embedders.clone();
        self.with(py, |store| {
            Ok(Graph::from_shared(Arc::clone(store.graph()), embedders))
        })
    }

    /// ``store.open_report()``: what the open did: ``{"clean_close", "commit_seq",
    /// "commits_replayed", "torn_tail_bytes"}``.
    fn open_report(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let report = self.with(py, |store| Ok(store.open_report().clone()))?;
        let dict = PyDict::new(py);
        dict.set_item("clean_close", report.clean_close)?;
        dict.set_item("commit_seq", report.commit_seq)?;
        dict.set_item("commits_replayed", report.commits_replayed)?;
        dict.set_item("torn_tail_bytes", report.torn_tail.map(|t| t.bytes))?;
        Ok(dict.into_any().unbind())
    }

    /// ``store.workload_report()``: the workload statistics of the store's graph (book page
    /// "Workload Statistics"): ``{"level", "queries", "properties", "locks", "jobs"}``, every
    /// row with its ``channel`` (Python's own queries count under ``"python"``) and the
    /// windows ``last_hour`` / ``last_two_days`` / ``last_ninety_days``. ``history`` (``"minutes"``,
    /// ``"hours"``, ``"days"``) adds every row's buckets of that tier (``"history"``: ``{"start",
    /// "count", "failed", "items", "total_micros", "max_micros"}``, ``start`` in microseconds since
    /// the Unix epoch) and ``"history_tier"``.
    #[pyo3(signature = (history=None))]
    fn workload_report(&self, py: Python<'_>, history: Option<&str>) -> PyResult<Py<PyAny>> {
        let tier = match history {
            None => None,
            Some(name) => Some(
                graphersal::workload::HistoryTier::from_name(name).ok_or_else(|| {
                    pyo3::exceptions::PyValueError::new_err(format!(
                        "unknown history tier '{name}': use minutes, hours or days"
                    ))
                })?,
            ),
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_micros() as i64);
        let report = self.with(py, move |store| {
            let stats = store.workload_stats();
            Ok(match tier {
                Some(tier) => stats.report_with_history(now, tier),
                None => stats.report(now),
            })
        })?;
        crate::convert::property_to_pyobject(py, &report.to_property())
    }

    /// ``store.checkpoint(name=None)``: writes a snapshot of the current state; returns its
    /// listing ``{"commit_seq", "name", ...}``.
    #[pyo3(signature = (name=None))]
    fn checkpoint(&self, py: Python<'_>, name: Option<String>) -> PyResult<Py<PyAny>> {
        let info = self.with(py, move |store| store.checkpoint(name.as_deref()))?;
        Ok(snapshot_dict(py, &info)?.into_any().unbind())
    }

    /// ``store.mark(name)``: names the current position (unique in the store's history).
    fn mark(&self, py: Python<'_>, name: &str) -> PyResult<Py<PyAny>> {
        let name = name.to_string();
        let mark = self.with(py, move |store| store.mark(&name))?;
        let dict = PyDict::new(py);
        dict.set_item("name", mark.name())?;
        dict.set_item("commit_seq", mark.commit_seq())?;
        dict.set_item("time", mark.time())?;
        Ok(dict.into_any().unbind())
    }

    /// ``store.snapshots()``: the snapshots, oldest first (dicts as ``checkpoint`` returns).
    fn snapshots(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let snapshots = self.with(py, |store| store.snapshots())?;
        let list = PyList::empty(py);
        for s in &snapshots {
            list.append(snapshot_dict(py, s)?)?;
        }
        Ok(list.into_any().unbind())
    }

    /// ``store.marks()``: ``[{"name", "commit_seq", "time"}]`` in order.
    fn marks(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let marks = self.with(py, |store| store.marks())?;
        let list = PyList::empty(py);
        for mark in &marks {
            let dict = PyDict::new(py);
            dict.set_item("name", mark.name())?;
            dict.set_item("commit_seq", mark.commit_seq())?;
            dict.set_item("time", mark.time())?;
            list.append(dict)?;
        }
        Ok(list.into_any().unbind())
    }

    /// ``store.prune(up_to)``: removes snapshots and WAL only needed for targets before commit
    /// ``up_to`` (the last two snapshots always stay). ``{"kept_from", "snapshots_removed",
    /// "segments_removed", "marks_unreachable"}`` (the marks whose history is gone now).
    fn prune(&self, py: Python<'_>, up_to: u64) -> PyResult<Py<PyAny>> {
        let report = self.with(py, move |store| store.prune(up_to))?;
        let dict = PyDict::new(py);
        dict.set_item("kept_from", report.kept_from)?;
        dict.set_item("snapshots_removed", report.snapshots_removed.clone())?;
        dict.set_item("segments_removed", report.segments_removed.clone())?;
        dict.set_item("marks_unreachable", report.marks_unreachable.clone())?;
        Ok(dict.into_any().unbind())
    }

    /// ``store.compact()``: a prune up to the current commit, then (a single-file store) the
    /// file rewritten without its garbage. ``{"snapshots_removed", "segments_removed",
    /// "marks_unreachable", "compacted", "bytes_before", "bytes_after", "bytes_freed"}``.
    fn compact(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let report = self.with(py, |store| store.compact())?;
        let dict = PyDict::new(py);
        dict.set_item("snapshots_removed", report.prune.snapshots_removed.clone())?;
        dict.set_item("segments_removed", report.prune.segments_removed.clone())?;
        dict.set_item("marks_unreachable", report.prune.marks_unreachable.clone())?;
        dict.set_item("compacted", report.compaction.is_some())?;
        dict.set_item("bytes_before", report.compaction.map(|c| c.bytes_before))?;
        dict.set_item("bytes_after", report.compaction.map(|c| c.bytes_after))?;
        dict.set_item("bytes_freed", report.bytes_freed())?;
        Ok(dict.into_any().unbind())
    }

    /// ``store.compaction_advice(*, min_ratio=None, min_reclaimable=None, min_size=None)``:
    /// whether ``compact()`` is worth it now (cheap: no data is read). ``{"recommended",
    /// "reason", "decided_by", "compactable", "total_bytes", "live_bytes", "garbage_bytes",
    /// "garbage_ratio", "prune_bytes", "prune_first", "reclaimable_bytes", "space_needed",
    /// "free_space", "marks_unreachable"}`` (the marks a prune to now would make unreachable:
    /// ``compact()`` does not ask, check this first); the defaults: 50 % and 64 MiB to gain, a
    /// store of at least 64 MiB.
    #[pyo3(signature = (*, min_ratio=None, min_reclaimable=None, min_size=None))]
    fn compaction_advice(
        &self,
        py: Python<'_>,
        min_ratio: Option<f64>,
        min_reclaimable: Option<u64>,
        min_size: Option<u64>,
    ) -> PyResult<Py<PyAny>> {
        let mut policy = persist::CompactionPolicy::new();
        if let Some(ratio) = min_ratio {
            if !(0.0..=1.0).contains(&ratio) {
                return Err(PyValueError::new_err("min_ratio must be within 0.0..=1.0"));
            }
            policy = policy.with_min_garbage_ratio(ratio);
        }
        if let Some(bytes) = min_reclaimable {
            policy = policy.with_min_reclaimable_bytes(bytes);
        }
        if let Some(bytes) = min_size {
            policy = policy.with_min_total_bytes(bytes);
        }
        let a = self.with(py, move |store| store.compaction_advice_with(policy))?;
        let dict = PyDict::new(py);
        dict.set_item("recommended", a.recommended)?;
        dict.set_item("reason", &a.reason)?;
        dict.set_item("decided_by", a.decided_by.name())?;
        dict.set_item("compactable", a.compactable)?;
        dict.set_item("total_bytes", a.total_bytes)?;
        dict.set_item("live_bytes", a.live_bytes)?;
        dict.set_item("garbage_bytes", a.garbage_bytes)?;
        dict.set_item("garbage_ratio", a.garbage_ratio)?;
        dict.set_item("prune_bytes", a.prune_bytes)?;
        dict.set_item("prune_first", a.prune_first)?;
        dict.set_item("reclaimable_bytes", a.reclaimable_bytes)?;
        dict.set_item("space_needed", a.space_needed)?;
        dict.set_item("free_space", a.free_space)?;
        dict.set_item("marks_unreachable", a.prune.marks_unreachable.clone())?;
        Ok(dict.into_any().unbind())
    }

    /// ``store.models()``: the embedding models the store keeps (format spec 4.5), each
    /// ``{"id", "identity", "repo", "revision", "commit", "license", "license_text", "gated",
    /// "base_models": [{"repo", "license"}], "bytes", "files": [{"name", "role", "bytes",
    /// "sha256"}], "used_by_schema", "problem"}``. ``license_text`` says where the licence text
    /// comes from: ``"file"`` (the repository's), ``"template"`` (the standard text of the
    /// licence, written beside the model) or ``"none"``. Every copy of the store carries them:
    /// handing the store on redistributes them.
    fn models(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let (models, used) = self.with(py, |store| {
            Ok((store.models()?, store.info()?.schema_models))
        })?;
        let list = PyList::empty(py);
        for model in &models {
            list.append(model_dict(py, model, &used)?)?;
        }
        Ok(list.into_any().unbind())
    }

    /// ``store.export(path, *, commit_seq=None, without_models=False)``: writes the latest
    /// stored snapshot (or the one at ``commit_seq``) as one packed file (``.gsnap``), with the
    /// store's embedding models and their licences unless ``without_models=True``. Returns
    /// ``{"commit_seq", "models": [identity]}`` (the models the file carries).
    #[pyo3(signature = (path, *, commit_seq=None, without_models=false))]
    fn export(
        &self,
        py: Python<'_>,
        path: &str,
        commit_seq: Option<u64>,
        without_models: bool,
    ) -> PyResult<Py<PyAny>> {
        let path = path.to_string();
        let copy = if without_models {
            persist::ModelCopy::Exclude
        } else {
            persist::ModelCopy::Include
        };
        let file = std::fs::File::create(&path).map_err(|err| {
            pyo3::exceptions::PyOSError::new_err(format!("cannot create {path}: {err}"))
        })?;
        let (info, models) = self.with(py, move |store| {
            let mut writer = std::io::BufWriter::new(file);
            let result = store.export_packed_with(commit_seq, &mut writer, copy);
            drop(writer);
            if result.is_err() {
                let _ = std::fs::remove_file(&path);
            }
            let models = if without_models {
                Vec::new()
            } else {
                store.models()?
            };
            Ok((result?, models))
        })?;
        let dict = PyDict::new(py);
        dict.set_item("commit_seq", info.commit_seq)?;
        dict.set_item(
            "models",
            models
                .iter()
                .map(|m| m.identity.clone())
                .collect::<Vec<_>>(),
        )?;
        Ok(dict.into_any().unbind())
    }

    /// ``store.verify()``: scrubs every checksum; ``{"ok", "problems": [str], "notes": [str]}``.
    fn verify(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let report = self.with(py, |store| store.verify())?;
        let dict = PyDict::new(py);
        dict.set_item("ok", report.is_ok())?;
        let problems: Vec<String> = report
            .problems
            .iter()
            .map(|p| format!("{}: {}", p.path, p.reason))
            .collect();
        dict.set_item("problems", problems)?;
        dict.set_item("notes", report.notes.clone())?;
        Ok(dict.into_any().unbind())
    }

    /// ``store.fork(path, *, commit_seq=None, time=None, mark=None)``: a NEW store in ``path``
    /// from the state at the target (default: the latest), with a new lineage id. Returns its
    /// snapshot's dict plus ``"marks_not_carried"``: ``[{"name", "commit_seq"}]``, the marks of
    /// the forked history (a fork starts without marks; they stay targets of this store).
    #[pyo3(signature = (path, *, commit_seq=None, time=None, mark=None))]
    fn fork(
        &self,
        py: Python<'_>,
        path: &str,
        commit_seq: Option<u64>,
        time: Option<i64>,
        mark: Option<String>,
    ) -> PyResult<Py<PyAny>> {
        let target = target(commit_seq, time, mark)?;
        let path = path.to_string();
        let report = self.with(py, move |store| store.fork(target, &path))?;
        let dict = snapshot_dict(py, &report.snapshot)?;
        dict.set_item(
            "marks_not_carried",
            marks_list(py, &report.marks_not_carried)?,
        )?;
        Ok(dict.into_any().unbind())
    }

    /// ``store.backup(path, *, full=False, from_memory=False)``: a backup taken without
    /// blocking writers: full into an empty or new directory, INCREMENTAL into an existing
    /// backup of this store (only what is new is copied); ``full=True`` requires an empty or
    /// new directory. Every checksum is read in the store and again in the copy; damage raises
    /// (nothing of the backup is kept) and is damage found while the store is open (its damage
    /// policy applies). ``from_memory=True``: only after damage was found, the graph in memory
    /// is written into the backup, nothing is written to the store. The backup keeps the
    /// lineage and opens read-only until ``Store.restore_backup``. Returns ``{"commit_seq",
    /// "bytes", "incremental", "from_memory", "previous_commit_seq", "snapshots_copied",
    /// "segments_copied", "tail_bytes", "warnings"}``.
    #[pyo3(signature = (path, *, full=false, from_memory=false))]
    fn backup(
        &self,
        py: Python<'_>,
        path: &str,
        full: bool,
        from_memory: bool,
    ) -> PyResult<Py<PyAny>> {
        let path = path.to_string();
        let mode = if full {
            BackupMode::Full
        } else {
            BackupMode::Auto
        };
        let report = self.with(py, move |store| {
            if from_memory {
                store.backup_from_memory(&path, mode)
            } else {
                store.backup_with(&path, mode)
            }
        })?;
        let dict = PyDict::new(py);
        dict.set_item("commit_seq", report.commit_seq)?;
        dict.set_item("bytes", report.bytes)?;
        dict.set_item("incremental", report.incremental)?;
        dict.set_item("previous_commit_seq", report.previous_commit_seq)?;
        dict.set_item("snapshots_copied", report.snapshots_copied.clone())?;
        dict.set_item("segments_copied", report.segments_copied.clone())?;
        dict.set_item("tail_bytes", report.tail_bytes)?;
        dict.set_item("from_memory", report.from_memory)?;
        dict.set_item("warnings", report.warnings.clone())?;
        Ok(dict.into_any().unbind())
    }

    /// ``store.damage_policy``: ``"maintenance"`` or ``"continue"``, fixed when the store was
    /// created (``Store.create(path, on_damage=...)``); nothing changes it.
    #[getter]
    fn damage_policy(&self, py: Python<'_>) -> PyResult<String> {
        self.with(py, |store| Ok(store.damage_policy().name().to_string()))
    }

    /// ``store.store_id``: the store's identity as a canonical UUID string, fixed when the
    /// store was created; a fork, repair or conversion is another store with a new one, a
    /// restored backup keeps it. An incremental backup requires the same id.
    #[getter]
    fn store_id(&self, py: Python<'_>) -> PyResult<String> {
        let id = self.with(py, |store| Ok(store.store_id()))?;
        let h = format!("{id:032x}");
        Ok(format!(
            "{}-{}-{}-{}-{}",
            &h[0..8],
            &h[8..12],
            &h[12..16],
            &h[16..20],
            &h[20..32]
        ))
    }

    /// ``store.damage_found``: the damage found on disk while the store is open (by a backup,
    /// a checkpoint, ``verify``), in order: ``[{"found_by", "at", "problems": [str]}]``; empty
    /// while none was.
    #[getter]
    fn damage_found(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let found = self.with(py, |store| Ok(store.damage_found()))?;
        let list = PyList::empty(py);
        for item in found {
            let dict = PyDict::new(py);
            dict.set_item("found_by", item.found_by)?;
            dict.set_item("at", item.at)?;
            let problems: Vec<String> = item
                .problems
                .iter()
                .map(|p| format!("{}: {}", p.path, p.reason))
                .collect();
            dict.set_item("problems", problems)?;
            list.append(dict)?;
        }
        Ok(list.into_any().unbind())
    }

    /// ``store.frozen_by_damage``: whether the store turned read-only because damage was found
    /// on disk while it is open (its damage policy ``"maintenance"``).
    #[getter]
    fn frozen_by_damage(&self, py: Python<'_>) -> PyResult<bool> {
        self.with(py, |store| Ok(store.is_frozen_by_damage()))
    }

    /// ``store.maintenance``: ``None`` for a healthy store; the damage report (text) when it
    /// opened read-only in maintenance mode because damage was found.
    #[getter]
    fn maintenance(&self, py: Python<'_>) -> PyResult<Option<String>> {
        self.with(py, |store| Ok(store.maintenance().map(ToString::to_string)))
    }

    /// ``store.rollback(*, commit_seq=None, time=None, mark=None, delete=False)``: rolls the
    /// store back IN PLACE to the target; the history after it moves to the attic (``delete``:
    /// deleted). Returns ``{"target", "previous_commit_seq", "attic"}``.
    #[pyo3(signature = (*, commit_seq=None, time=None, mark=None, delete=false))]
    fn rollback(
        &self,
        py: Python<'_>,
        commit_seq: Option<u64>,
        time: Option<i64>,
        mark: Option<String>,
        delete: bool,
    ) -> PyResult<Py<PyAny>> {
        let target = target(commit_seq, time, mark)?;
        if target == RecoveryTarget::Latest {
            return Err(PyValueError::new_err(
                "give one of commit_seq, time and mark",
            ));
        }
        let mode = if delete {
            RollbackMode::Delete
        } else {
            RollbackMode::Move
        };
        let report = self.with(py, move |store| store.rollback_to(target, mode))?;
        let dict = PyDict::new(py);
        dict.set_item("target", report.target)?;
        dict.set_item("previous_commit_seq", report.previous_commit_seq)?;
        dict.set_item("attic", report.attic)?;
        Ok(dict.into_any().unbind())
    }

    /// ``store.attic()``: the attic entries (history moved aside by rollbacks):
    /// ``[{"id", "target", "last_commit_seq", "time"}]``.
    fn attic(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let entries = self.with(py, |store| store.attic_list())?;
        let list = PyList::empty(py);
        for entry in &entries {
            let dict = PyDict::new(py);
            dict.set_item("id", &entry.id)?;
            dict.set_item("target", entry.target)?;
            dict.set_item("last_commit_seq", entry.last_commit_seq)?;
            dict.set_item("time", entry.time)?;
            list.append(dict)?;
        }
        Ok(list.into_any().unbind())
    }

    /// ``store.attic_restore(id)``: undoes the rollback of attic entry ``id`` (only while
    /// nothing was committed since); returns the commit the store is at afterwards.
    fn attic_restore(&self, py: Python<'_>, id: &str) -> PyResult<u64> {
        let id = id.to_string();
        self.with(py, move |store| store.attic_restore(&id))
    }

    /// ``store.repair_to(path)``: builds a repaired copy of this store in a NEW directory (a
    /// damaged store's files are never changed). Returns ``{"commit_seq", "lossless",
    /// "marks_not_carried", "lineage_approximate_before", "report"}`` (the report as text:
    /// repaired, lost, diverged, marks not carried; ``marks_not_carried`` is ``[{"name",
    /// "commit_seq"}]``, the repaired store starts without marks; ``lineage_approximate_before``
    /// is the commit before which the lineage is approximate when both GRAPH copies were
    /// damaged, else ``None``).
    fn repair_to(&self, py: Python<'_>, path: &str) -> PyResult<Py<PyAny>> {
        let path = path.to_string();
        let report = self.with(py, move |store| store.repair_to(&path))?;
        let dict = PyDict::new(py);
        dict.set_item("commit_seq", report.commit_seq)?;
        dict.set_item("lossless", report.is_lossless())?;
        dict.set_item(
            "marks_not_carried",
            marks_list(py, &report.marks_not_carried)?,
        )?;
        dict.set_item(
            "lineage_approximate_before",
            report.damage.lineage_approximate_before,
        )?;
        dict.set_item("report", report.to_string())?;
        Ok(dict.into_any().unbind())
    }

    /// ``store.close()``: a clean close (WAL synced, lock released); the graph refuses commits
    /// afterwards. Closing twice is fine.
    fn close(&self, py: Python<'_>) -> PyResult<()> {
        let inner = Arc::clone(&self.inner);
        py.detach(move || {
            let store = inner.lock().ok().and_then(|mut guard| guard.take());
            match store {
                Some(store) => store.close().map_err(|err| failure_parts(&err)),
                None => Ok(()),
            }
        })
        .map_err(|(text, cause)| raise_with_cause(py, &text, cause))
    }

    fn __enter__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    #[pyo3(signature = (*_args))]
    fn __exit__(&self, py: Python<'_>, _args: &Bound<'_, pyo3::types::PyTuple>) -> PyResult<bool> {
        self.close(py)?;
        Ok(false)
    }
}
