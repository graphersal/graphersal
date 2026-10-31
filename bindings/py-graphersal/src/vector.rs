//! Vectors (book "Vectors") as Python values: ``graphersal.Vector``, both ways. A vector
//! property read back is a ``graphersal.Vector``; a ``graphersal.Vector`` passed as a parameter
//! is a vector in the script.
//!
//! A vector is built from a list (or tuple) of numbers or from any object that exposes a 1-D
//! buffer of ``float32`` or ``float64`` values (a ``numpy`` array, ``array.array('f')``, a
//! ``memoryview``). The buffer is read through Python's ``memoryview``, so the extension needs no
//! ``numpy`` and stays within the stable ABI it is built for.

use graphersal::vector::{Vector, VectorElement};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyList, PyTuple};

/// A vector (an embedding): a fixed-length list of numbers stored as one value, with the element
/// kind ``"f32"`` (default), ``"f16"``, ``"i8"`` (symmetric scalar quantization) or ``"bit"``.
/// Immutable; equal when kind, dimension and components are equal.
///
/// ``Vector(values, element="f32")``: ``values`` is a list of numbers or a 1-D ``float32`` /
/// ``float64`` buffer (``numpy.ndarray``, ``array.array``); another element quantizes them.
#[pyclass(
    module = "graphersal",
    name = "Vector",
    frozen,
    eq,
    hash,
    skip_from_py_object
)]
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PyVector {
    pub(crate) vector: Vector,
}

fn element_of(name: &str) -> PyResult<VectorElement> {
    VectorElement::from_name(name).ok_or_else(|| {
        PyValueError::new_err(format!(
            "unknown vector element {name:?}: the kinds are \"f32\", \"f16\", \"i8\" and \"bit\""
        ))
    })
}

fn value_error(err: graphersal::vector::VectorError) -> PyErr {
    PyValueError::new_err(format!("{err}. {}", err.help()))
}

/// `memoryview(values)`, or `None` for an object that exposes no buffer.
fn memoryview_of<'py>(values: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyAny>>> {
    let py = values.py();
    Ok(py
        .import("builtins")?
        .getattr("memoryview")?
        .call1((values,))
        .ok())
}

/// The f64 values of a memoryview, flat in C order: format `f` (float32) or `d` (float64).
fn view_floats(view: &Bound<'_, PyAny>) -> PyResult<Vec<f64>> {
    let format: String = view.getattr("format")?.extract()?;
    let itemsize: usize = view.getattr("itemsize")?.extract()?;
    let bytes: Vec<u8> = view.call_method0("tobytes")?.extract()?;
    let format = format.trim_start_matches(['@', '=', '<']);
    match (format, itemsize) {
        ("f", 4) => Ok(bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f64::from(f32::from_ne_bytes(*c)))
            .collect()),
        ("d", 8) => Ok(bytes
            .as_chunks::<8>()
            .0
            .iter()
            .map(|c| f64::from_ne_bytes(*c))
            .collect()),
        _ => Err(PyValueError::new_err(format!(
            "a vector buffer holds float32 or float64 values, not the buffer format {format:?} \
             (convert it: numpy_array.astype('float32'))"
        ))),
    }
}

/// The f64 values of a buffer object (`memoryview(values)`): format `f` (float32) or `d`
/// (float64), one dimension (or C-contiguous, read flat).
fn buffer_values(values: &Bound<'_, PyAny>) -> PyResult<Option<Vec<f64>>> {
    let Some(view) = memoryview_of(values)? else {
        return Ok(None);
    };
    let ndim: usize = view.getattr("ndim")?.extract()?;
    if ndim > 1 {
        return Err(PyValueError::new_err(
            "a vector is one-dimensional: pass one row (array[i]) of a 2-D array",
        ));
    }
    view_floats(&view).map(Some)
}

/// A script parameter given as a float32/float64 buffer (a one-dimensional numpy array, an
/// `array.array("f")`): the f32 vector of its values. `Ok(None)` for an object that is no such
/// buffer (the caller reports the unsupported type); a 2-D array or a NaN is an error.
pub(crate) fn python_buffer_vector(value: &Bound<'_, PyAny>) -> Result<Option<Vector>, String> {
    let py = value.py();
    let Ok(view) = py
        .import("builtins")
        .and_then(|builtins| builtins.getattr("memoryview"))
        .and_then(|memoryview| memoryview.call1((value,)))
    else {
        return Ok(None);
    };
    let format: String = view
        .getattr("format")
        .and_then(|f| f.extract())
        .unwrap_or_default();
    if !matches!(format.trim_start_matches(['@', '=', '<']), "f" | "d") {
        return Ok(None);
    }
    let numbers = buffer_values(value)
        .map_err(|err| err.to_string())?
        .unwrap_or_default();
    Vector::from_f64(&numbers)
        .map(Some)
        .map_err(|err| format!("{err}. {}", err.help()))
}

/// The numbers of `values`: a list or tuple of `int`/`float`, else a float buffer.
pub(crate) fn values_of(values: &Bound<'_, PyAny>) -> PyResult<Vec<f64>> {
    if values.is_instance_of::<PyList>() || values.is_instance_of::<PyTuple>() {
        let mut out = Vec::new();
        for (index, item) in values.try_iter()?.enumerate() {
            let item = item?;
            if item.is_instance_of::<pyo3::types::PyBool>() {
                return Err(PyValueError::new_err(format!(
                    "component {index} of the vector is a bool, not a number"
                )));
            }
            out.push(item.extract::<f64>().map_err(|_| {
                PyValueError::new_err(format!("component {index} of the vector is not a number"))
            })?);
        }
        return Ok(out);
    }
    match buffer_values(values)? {
        Some(out) => Ok(out),
        None => Err(PyValueError::new_err(
            "Vector(values) takes a list of numbers or a float32/float64 buffer (numpy array)",
        )),
    }
}

#[pymethods]
impl PyVector {
    #[new]
    #[pyo3(signature = (values, element="f32"))]
    fn py_new(values: &Bound<'_, PyAny>, element: &str) -> PyResult<Self> {
        let element = element_of(element)?;
        let numbers = values_of(values)?;
        let vector = Vector::from_f64(&numbers)
            .and_then(|vector| vector.quantize(element))
            .map_err(value_error)?;
        Ok(PyVector { vector })
    }

    /// The element kind: ``"f32"``, ``"f16"``, ``"i8"`` or ``"bit"``.
    #[getter]
    fn element(&self) -> &'static str {
        self.vector.element().name()
    }

    /// The number of components.
    #[getter]
    fn dimension(&self) -> usize {
        self.vector.dimension()
    }

    fn __len__(&self) -> usize {
        self.vector.dimension()
    }

    /// The components as a list of floats (dequantized: an ``i8`` component is
    /// ``code * scale``, a ``bit`` one ``1.0`` or ``-1.0``).
    fn tolist(&self) -> Vec<f64> {
        self.vector.iter_f32().map(short_f64).collect()
    }

    /// This vector in another element kind (``quantize("f32")`` dequantizes).
    fn quantize(&self, element: &str) -> PyResult<Self> {
        let element = element_of(element)?;
        self.vector
            .quantize(element)
            .map(|vector| PyVector { vector })
            .map_err(value_error)
    }

    fn __repr__(&self) -> String {
        const SHOWN: usize = 16;
        let mut values: Vec<String> = self
            .vector
            .iter_f32()
            .take(SHOWN)
            .map(|value| {
                // Python's float text: always a point or an exponent.
                let text = value.to_string();
                if text.contains(['.', 'e', 'i', 'N']) {
                    text
                } else {
                    format!("{text}.0")
                }
            })
            .collect();
        if self.vector.dimension() > SHOWN {
            values.push(format!("... ({} items)", self.vector.dimension()));
        }
        format!(
            "Vector([{}], element='{}')",
            values.join(", "),
            self.vector.element().name()
        )
    }
}

/// The float nearest to an f32's shortest text (`0.1`, not `0.10000000149011612`), which reads
/// back as the same f32; the engine's JSON writes the same numbers.
pub(crate) fn short_f64(value: f32) -> f64 {
    match value.to_string().parse::<f64>() {
        Ok(short) if short as f32 == value => short,
        _ => f64::from(value),
    }
}

/// A vector as a Python object.
pub(crate) fn vector_to_pyobject(py: Python<'_>, vector: Vector) -> PyResult<Py<PyAny>> {
    Ok(Bound::new(py, PyVector { vector })?.into_any().unbind())
}

/// The vector a Python value holds, if it is a ``graphersal.Vector``.
pub(crate) fn python_vector(value: &Bound<'_, PyAny>) -> Option<Vector> {
    value
        .cast::<PyVector>()
        .ok()
        .map(|vector| vector.get().vector.clone())
}
