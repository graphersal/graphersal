//! Embedders (book page "Vectors", section "Embeddings") from Python: ``graphersal.Embedder``
//! (a Python callable as an engine `Embedder`), ``graphersal.Model2Vec`` (the engine's
//! `vector::Model2Vec`, loaded from a local directory: the binding never downloads) and the
//! per-graph registry the scripts and ``Graph.embed_property`` use.
//!
//! **GIL.** A Python embedder is called from inside the engine: by the `embed` step while the
//! traversal holds the graph's lock, by ``embed_property`` without it. It attaches to the
//! interpreter for the call (`Python::try_attach`), so the binding keeps one rule everywhere:
//! no path waits for a graph lock while it holds the GIL (every lock is taken inside
//! `py.detach`). Otherwise a thread holding the GIL and waiting for the lock and a traversal
//! holding the lock and waiting for the GIL would wait for each other forever.
//!
//! **Registry.** A graph's registry is an immutable `Arc<Embedders>` behind a small lock
//! ([`RegistryCell`]); ``register_embedder`` builds a new registry and swaps it in, so a running
//! execution keeps the registry it started with. The scripts get it through
//! `Embedders::scope`, the engine entry points (`eval_value_with_limits`, `eval_value_atomic`,
//! `eval_value_dry_run`) building their own engine inside that scope.

use std::sync::{Arc, RwLock};

use graphersal::vector::{
    EmbedContext, EmbedError, Embedder, EmbedderInfo, Embedders, Vector, VectorElement,
    VectorMetric,
};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyList, PyTuple};

use crate::vector::{PyVector, buffer_matrix, python_vector, values_of};

/// The registry of a graph, shared by every Python `Graph` object over the same engine graph
/// (`Store.graph` hands out a new one per access).
#[derive(Clone)]
pub(crate) struct RegistryCell(Arc<RwLock<Arc<Embedders>>>);

impl RegistryCell {
    pub(crate) fn new(embedders: Embedders) -> Self {
        RegistryCell(Arc::new(RwLock::new(Arc::new(embedders))))
    }

    /// The registry now (a running execution keeps the one it took).
    pub(crate) fn current(&self) -> Arc<Embedders> {
        match self.0.read() {
            Ok(guard) => Arc::clone(&guard),
            Err(poisoned) => Arc::clone(&poisoned.into_inner()),
        }
    }

    /// Replaces the registry (a store's models added at open).
    #[cfg_attr(not(feature = "model2vec"), allow(dead_code))]
    pub(crate) fn replace(&self, next: Arc<Embedders>) {
        let mut guard = match self.0.write() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        *guard = next;
    }

    /// Registers `embedder` (as the default with `default`): a new registry swapped in.
    pub(crate) fn register(&self, embedder: Arc<dyn Embedder>, default: bool) -> PyResult<()> {
        let mut guard = match self.0.write() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let current = Arc::clone(&guard);
        let name = embedder.info().name.clone();
        let mut builder = Embedders::builder();
        for info in current.infos() {
            // The built-in is in every builder already.
            if info.name.starts_with("hash-ngram") {
                continue;
            }
            if let Some(existing) = current.get(&info.name) {
                builder = builder.with(Arc::clone(existing)).map_err(registry_error)?;
            }
        }
        builder = builder.with(embedder).map_err(registry_error)?;
        let default_name = if default {
            name
        } else {
            current.default_embedder().info().name.clone()
        };
        let next = builder
            .default_name(&default_name)
            .map_err(registry_error)?
            .build();
        *guard = Arc::new(next);
        Ok(())
    }
}

/// The registry of a graph constructor's ``embedders=``/``default_embedder=`` keywords.
pub(crate) fn registry_of(
    embedders: Option<Vec<PyRef<'_, PyEmbedder>>>,
    default_embedder: Option<&str>,
) -> PyResult<RegistryCell> {
    let mut builder = Embedders::builder();
    for embedder in embedders.unwrap_or_default() {
        builder = builder
            .with(Arc::clone(&embedder.inner))
            .map_err(registry_error)?;
    }
    if let Some(name) = default_embedder {
        builder = builder.default_name(name).map_err(registry_error)?;
    }
    Ok(RegistryCell::new(builder.build()))
}

/// A registry refusal (an invalid info, a reserved or duplicate name, an unknown default) as a
/// ``ValueError`` with the engine's help.
fn registry_error(err: EmbedError) -> PyErr {
    PyValueError::new_err(format!("{err}. {}", err.help()))
}

fn element_of(name: &str) -> PyResult<VectorElement> {
    VectorElement::from_name(name).ok_or_else(|| {
        PyValueError::new_err(format!(
            "unknown vector element {name:?}: the kinds are \"f32\", \"f16\", \"i8\" and \"bit\""
        ))
    })
}

fn metric_of(name: &str) -> PyResult<VectorMetric> {
    VectorMetric::from_name(name).ok_or_else(|| {
        PyValueError::new_err(format!(
            "unknown vector metric {name:?}: the metrics are \"cosine\", \"dot\", \"l2\" and \
             \"hamming\""
        ))
    })
}

/// An embedder: turns texts into vectors for the ``embed`` step, ``embed(text)``,
/// ``Graph.embed_property`` and text queries. Register it with ``Graph(embedders=[..])`` or
/// ``graph.register_embedder``.
///
/// ``Embedder(name, fn, *, dimension, element="f32", metric="cosine", max_batch=64,
/// deterministic=False, local=True, model=None)``: ``fn(texts)`` gets a ``list[str]`` (at most
/// ``max_batch`` texts) and returns one row per text: a list of rows (each a list of floats or a
/// 1-D float buffer) or ONE 2-D ``float32``/``float64`` buffer of shape ``(len(texts),
/// dimension)`` (a numpy array). ``model`` (default: ``name``) is the model identity the schema
/// records for the vectors it produces: keep it stable across runs and hosts and change it
/// whenever the model changes, so a text query embeds with the model the vectors came from.
#[pyclass(module = "graphersal", name = "Embedder", subclass, frozen)]
pub struct PyEmbedder {
    pub(crate) inner: Arc<dyn Embedder>,
}

/// A Python callable as an engine embedder.
struct CallableEmbedder {
    info: EmbedderInfo,
    func: Py<PyAny>,
}

impl Embedder for CallableEmbedder {
    fn info(&self) -> &EmbedderInfo {
        &self.info
    }

    fn embed(&self, texts: &[&str], ctx: &EmbedContext<'_>) -> Result<Vec<Vector>, EmbedError> {
        ctx.check()?;
        // `try_attach`: no interpreter (being finalized) is an unavailable embedder, never a
        // panic.
        Python::try_attach(|py| {
            let list = PyList::new(py, texts)
                .map_err(|err| EmbedError::failed(describe_exception(py, &err)))?;
            match self.func.call1(py, (list,)) {
                Ok(out) => read_output(out.bind(py), texts.len(), &self.info),
                Err(err) => Err(EmbedError::failed(describe_exception(py, &err))),
            }
        })
        .unwrap_or_else(|| {
            Err(EmbedError::unavailable(
                "the Python interpreter is not running (it is shutting down)",
            ))
        })
    }
}

/// `TypeName: message` of a Python exception (also `KeyboardInterrupt`, `SystemExit`).
fn describe_exception(py: Python<'_>, err: &PyErr) -> String {
    let kind = err
        .get_type(py)
        .name()
        .map(|name| name.to_string())
        .unwrap_or_else(|_| "Exception".to_string());
    let message = err
        .value(py)
        .str()
        .map(|text| text.to_string())
        .unwrap_or_default();
    if message.is_empty() {
        format!("the Python embedder raised {kind}")
    } else {
        format!("the Python embedder raised {kind}: {message}")
    }
}

/// The vectors of a Python embedder's answer `out` for `n` texts.
fn read_output(
    out: &Bound<'_, PyAny>,
    n: usize,
    info: &EmbedderInfo,
) -> Result<Vec<Vector>, EmbedError> {
    let dimension = info.dimension;
    let rows: Vec<Vec<f64>> = if out.is_instance_of::<PyList>() || out.is_instance_of::<PyTuple>() {
        let count = out.len().map_err(|err| bad(err.to_string()))?;
        if count != n {
            return Err(bad(format!("it returned {count} rows for {n} texts")));
        }
        let mut rows = Vec::with_capacity(n);
        for (index, row) in out
            .try_iter()
            .map_err(|err| bad(err.to_string()))?
            .enumerate()
        {
            let row = row.map_err(|err| bad(err.to_string()))?;
            let values = match python_vector(&row) {
                Some(vector) => vector.iter_f32().map(f64::from).collect(),
                None => values_of(&row).map_err(|err| bad(format!("row {index}: {err}")))?,
            };
            if values.len() != dimension {
                return Err(bad(format!(
                    "row {index} has {} values, the embedder's dimension is {dimension}",
                    values.len()
                )));
            }
            rows.push(values);
        }
        rows
    } else {
        match buffer_matrix(out).map_err(|err| bad(err.to_string()))? {
            Some((shape, values)) => {
                let [count, columns] = shape[..] else {
                    return Err(bad(format!(
                        "it returned a {}-D buffer; return a list of rows or a 2-D array of \
                         shape ({n}, {dimension})",
                        shape.len()
                    )));
                };
                if count != n || columns != dimension {
                    return Err(bad(format!(
                        "it returned an array of shape ({count}, {columns}) for {n} texts, \
                         expected ({n}, {dimension})"
                    )));
                }
                values
                    .chunks(dimension.max(1))
                    .map(<[f64]>::to_vec)
                    .collect()
            }
            None => {
                let kind = out
                    .get_type()
                    .name()
                    .map(|name| name.to_string())
                    .unwrap_or_default();
                return Err(bad(format!(
                    "it returned a {kind}; return a list of rows (lists of floats) or a 2-D \
                     float32/float64 array of shape ({n}, {dimension})"
                )));
            }
        }
    };
    rows.iter()
        .enumerate()
        .map(|(index, values)| {
            Vector::from_f64(values)
                .and_then(|vector| vector.quantize(info.element))
                .map_err(|err| bad(format!("row {index}: {err}")))
        })
        .collect()
}

fn bad(reason: String) -> EmbedError {
    EmbedError::bad_output(reason)
}

#[pymethods]
impl PyEmbedder {
    #[new]
    #[pyo3(signature = (name, r#fn, *, dimension, element="f32", metric="cosine", max_batch=64, deterministic=false, local=true, model=None))]
    #[allow(clippy::too_many_arguments)]
    fn py_new(
        name: String,
        r#fn: Bound<'_, PyAny>,
        dimension: usize,
        element: &str,
        metric: &str,
        max_batch: usize,
        deterministic: bool,
        local: bool,
        model: Option<String>,
    ) -> PyResult<Self> {
        if !r#fn.is_callable() {
            return Err(pyo3::exceptions::PyTypeError::new_err(
                "Embedder(name, fn, ...): fn must be callable (fn(texts: list[str]) -> rows)",
            ));
        }
        let mut info = EmbedderInfo::new(name, dimension, element_of(element)?)
            .with_metric(metric_of(metric)?)
            .with_max_batch(max_batch)
            .with_deterministic(deterministic)
            .with_local(local);
        if let Some(model) = model {
            info = info.with_model(model);
        }
        let inner: Arc<dyn Embedder> = Arc::new(CallableEmbedder {
            info,
            func: r#fn.unbind(),
        });
        // The registry's checks of the info (name, model, dimension, max_batch, reserved
        // names) at construction, not first at registration.
        Embedders::builder()
            .with(Arc::clone(&inner))
            .map_err(registry_error)?;
        Ok(PyEmbedder { inner })
    }

    /// The registry name.
    #[getter]
    fn name(&self) -> String {
        self.inner.info().name.to_string()
    }

    /// The model identity the schema records (``model2vec/<repo>@<hash>`` for a Model2Vec).
    #[getter]
    fn model(&self) -> String {
        self.inner.info().model.to_string()
    }

    /// The number of components of every vector.
    #[getter]
    fn dimension(&self) -> usize {
        self.inner.info().dimension
    }

    /// The element kind of the vectors: ``"f32"``, ``"f16"``, ``"i8"`` or ``"bit"``.
    #[getter]
    fn element(&self) -> &'static str {
        self.inner.info().element.name()
    }

    /// The metric the vectors are meant for (the default of ``nearest``/``similarity``).
    #[getter]
    fn metric(&self) -> &'static str {
        self.inner.info().metric.name()
    }

    /// The most texts per call.
    #[getter]
    fn max_batch(&self) -> usize {
        self.inner.info().max_batch
    }

    /// Whether the same text always gives the same bits.
    #[getter]
    fn deterministic(&self) -> bool {
        self.inner.info().deterministic
    }

    /// Whether the embedder runs locally (sends no text anywhere).
    #[getter]
    fn local(&self) -> bool {
        self.inner.info().local
    }

    /// ``embedder.embed(texts)``: the vectors of ``texts`` (a ``list[str]``) as a list of
    /// ``graphersal.Vector``, in pages of ``max_batch`` texts, checked like inside the engine
    /// (one vector per text of the stated dimension and element). Raises ``GraphersalError``.
    fn embed(&self, py: Python<'_>, texts: Vec<String>) -> PyResult<Vec<PyVector>> {
        let inner = Arc::clone(&self.inner);
        let result = py.detach(move || {
            let info = inner.info();
            let mut out = Vec::with_capacity(texts.len());
            for page in texts.chunks(info.max_batch.max(1)) {
                let refs: Vec<&str> = page.iter().map(String::as_str).collect();
                let vectors = inner.embed(&refs, &EmbedContext::new())?;
                if vectors.len() != refs.len() {
                    return Err(EmbedError::bad_output(format!(
                        "it returned {} vectors for {} texts",
                        vectors.len(),
                        refs.len()
                    )));
                }
                if let Some(index) = vectors
                    .iter()
                    .position(|v| v.dimension() != info.dimension || v.element() != info.element)
                {
                    return Err(EmbedError::bad_output(format!(
                        "the vector at position {index} does not have the stated dimension \
                         {} and element {}",
                        info.dimension,
                        info.element.name()
                    )));
                }
                out.extend(vectors);
            }
            Ok(out)
        });
        match result {
            Ok(vectors) => Ok(vectors
                .into_iter()
                .map(|vector| PyVector { vector })
                .collect()),
            Err(err) => Err(crate::error::map_diagnostic(
                py,
                &format!("Error: {err}\nHelp: {}", err.help()),
            )),
        }
    }

    fn __repr__(&self) -> String {
        let info = self.inner.info();
        format!(
            "Embedder(name={:?}, model={:?}, dimension={}, element='{}', metric='{}')",
            info.name.as_str(),
            info.model.as_str(),
            info.dimension,
            info.element.name(),
            info.metric.name()
        )
    }
}

/// A Model2Vec static embedding model loaded from a LOCAL directory (the binding never
/// downloads; fetch the files first, e.g. ``huggingface_hub.snapshot_download``).
///
/// ``Model2Vec(name, path, *, model=None, max_tokens=512, normalize=None, max_batch=256)``:
/// ``name`` is the registry name, ``path`` the model directory (``config.json``,
/// ``tokenizer.json``, ``model.safetensors``). ``model`` is the Hugging Face repository the files
/// come from (``"minishlab/potion-base-8M"``): the model identity becomes
/// ``model2vec/<model>@<12 hex>``, the same as a ``graphersal embed-model download`` of the same
/// files, so a graph embedded with the command line finds its model here. Without ``model`` the
/// directory's ``graphersal-embed-model.json`` names it, else the identity is
/// ``model2vec@<12 hex>``; a ``model`` that differs from that file's raises. Loading releases the
/// GIL; a directory holding no readable model raises ``GraphersalError``.
#[cfg(feature = "model2vec")]
#[pyclass(module = "graphersal", name = "Model2Vec", extends = PyEmbedder, frozen)]
pub struct PyModel2Vec;

#[cfg(feature = "model2vec")]
#[pymethods]
impl PyModel2Vec {
    #[new]
    #[pyo3(signature = (name, path, *, model=None, max_tokens=512, normalize=None, max_batch=256))]
    fn py_new(
        py: Python<'_>,
        name: String,
        path: std::path::PathBuf,
        model: Option<String>,
        max_tokens: usize,
        normalize: Option<bool>,
        max_batch: usize,
    ) -> PyResult<PyClassInitializer<Self>> {
        let loaded = py.detach(move || {
            let mut options = graphersal::vector::Model2Vec::options()
                .with_max_tokens(max_tokens)
                .with_max_batch(max_batch);
            if let Some(normalize) = normalize {
                options = options.with_normalize(normalize);
            }
            if let Some(model) = model {
                options = options.with_repo(model);
            }
            options.load(name, &path)
        });
        let model = loaded.map_err(|err| {
            crate::error::map_diagnostic(py, &format!("Error: {err}\nHelp: {}", err.help()))
        })?;
        let inner: Arc<dyn Embedder> = Arc::new(model);
        Embedders::builder()
            .with(Arc::clone(&inner))
            .map_err(registry_error)?;
        Ok(PyClassInitializer::from(PyEmbedder { inner }).add_subclass(PyModel2Vec))
    }
}
