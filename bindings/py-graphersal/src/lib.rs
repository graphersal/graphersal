//! `py-graphersal`: PyO3 bindings that run Gremlin scripts through the Graphersal engine.
//!
//! This crate adds zero new query execution logic. It evaluates script text through
//! `graphersal::script::eval_value_with_limits` / `eval_value_atomic` / `eval_value_dry_run`
//! and converts the `Dynamic` result into native Python structures. The public Python names are
//! listed in `graphersal.pyi`.

// The extension installs the tracking allocator as its own global allocator (a `cdylib` may; a
// library crate cannot do it for its consumer). It counts only the Rust heap of this extension
// (graphs, queries, their results before conversion), never Python objects: Memory profiles
// (`.profile_with(ProfileType.Memory)`) report `tracked` bytes, and the memory budget compares the
// live Rust heap (one atomic load) instead of walking the graph once per traversal. The cost is a
// few counter increments per allocation (see `graphersal::alloc_tracking`).
#[global_allocator]
static ALLOC: ::graphersal::alloc_tracking::TrackingAllocator =
    ::graphersal::alloc_tracking::TrackingAllocator::new();

mod convert;
mod element;
mod embedder;
mod error;
mod graph;
mod policy;
mod script_thread;
mod store;
mod temporal;
mod vector;

use pyo3::prelude::*;

pub use element::{Edge, Vertex};
pub use embedder::PyEmbedder;
#[cfg(feature = "model2vec")]
pub use embedder::PyModel2Vec;
pub use error::{
    CommitRejectedError, EmbedPropertyError, GraphersalError, PermissionDeniedError,
    ReturnedClosureError, VectorFillWarning,
};
pub use graph::{DryRunResult, Graph};
pub use policy::AccessPolicy;
pub use store::Store;
pub use temporal::Interval;
pub use vector::PyVector;

/// ``graphersal.set_memory_limit(limit, *, headroom=None)``: sets the PROCESS memory ceiling
/// (``memory.limit``): ONE setting for every graph of this Python process, on by default.
/// ``limit`` is a positive number of bytes, ``"auto"`` (the default: the cgroup memory limit
/// minus ``headroom`` bytes, default a tenth of the limit, at least 64 MiB; else 80 % of the
/// physical memory) or ``"none"`` (no ceiling). A query, import or write that would take the
/// process's live Rust heap over it raises ``GraphersalError`` and is rolled back. Python
/// objects are not counted: leave room for them.
#[pyfunction]
#[pyo3(signature = (limit, *, headroom=None))]
fn set_memory_limit(limit: &Bound<'_, PyAny>, headroom: Option<u64>) -> PyResult<()> {
    let limit = graph::memory_limit_value(Some(limit))?;
    graph::set_process_memory_limit(limit, headroom)
}

/// ``graphersal.memory_limit()``: the process memory ceiling in effect as a dict: ``bytes``
/// (``None``: no ceiling), ``source`` (``"configured"``, ``"cgroup"``, ``"physical"``,
/// ``"wasm"``, ``"none"`` or ``"unknown"``), ``setting`` (``"auto"``, ``"none"`` or the bytes
/// as text) and ``headroom`` (``None``: the default).
#[pyfunction]
fn memory_limit(py: Python<'_>) -> PyResult<Py<PyAny>> {
    let limit = ::graphersal::exec::memory_limit()
        .map_err(|err| pyo3::exceptions::PyRuntimeError::new_err(err.to_string()))?;
    let dict = pyo3::types::PyDict::new(py);
    dict.set_item("bytes", limit.bytes())?;
    dict.set_item("source", limit.source().as_str())?;
    dict.set_item("setting", limit.setting().to_string())?;
    dict.set_item("headroom", limit.headroom())?;
    Ok(dict.into_any().unbind())
}

/// Python bindings of the Graphersal in-memory graph traversal engine: run Gremlin scripts against
/// a `Graph` and get native Python values back. See `Graph`, `AccessPolicy` and `GraphersalError`.
#[pymodule]
fn graphersal(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Graph>()?;
    m.add_function(wrap_pyfunction!(set_memory_limit, m)?)?;
    m.add_function(wrap_pyfunction!(memory_limit, m)?)?;
    m.add_class::<AccessPolicy>()?;
    m.add_class::<DryRunResult>()?;
    m.add_class::<Store>()?;
    m.add_class::<Vertex>()?;
    m.add_class::<Edge>()?;
    m.add_class::<Interval>()?;
    m.add_class::<PyVector>()?;
    m.add_class::<PyEmbedder>()?;
    #[cfg(feature = "model2vec")]
    m.add_class::<PyModel2Vec>()?;
    let error_type = py.get_type::<GraphersalError>();
    // A class-level default so every `GraphersalError` has `.help` even when raised through a
    // path that never calls `error::new_graphersal_error` (e.g. the `Dynamic` conversion
    // fallback in `convert.rs`, which raises via `GraphersalError::new_err` directly because it
    // never carries help text to begin with). An instance `setattr` in `new_graphersal_error`
    // still shadows this per Python's normal attribute lookup; the subclasses inherit it.
    error_type.setattr("help", py.None())?;
    m.add("GraphersalError", error_type)?;
    m.add(
        "PermissionDeniedError",
        py.get_type::<PermissionDeniedError>(),
    )?;
    m.add("CommitRejectedError", py.get_type::<CommitRejectedError>())?;
    m.add("EmbedPropertyError", py.get_type::<EmbedPropertyError>())?;
    m.add("VectorFillWarning", py.get_type::<VectorFillWarning>())?;
    m.add(
        "ReturnedClosureError",
        py.get_type::<ReturnedClosureError>(),
    )?;
    Ok(())
}
