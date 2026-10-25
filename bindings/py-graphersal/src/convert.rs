//! `Dynamic` -> Python and Python -> `ElementProperty` conversion, per the design spec's Value
//! conversion / Parameter conversion sections.

use graphersal::prelude::{CompactString, ElementProperty, Number};
use graphersal::profile::TraversalMetrics;
use graphersal::script::rhai::Dynamic;
use graphersal::script::{
    ScriptEdge, ScriptExecution, ScriptUuid, ScriptVector, ScriptVertex, metrics_to_map,
};
use graphersal::temporal::{Date, DateTime, Duration, LocalDateTime, OffsetTime, Temporal, Time};
use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyInt, PyList};
use std::collections::HashMap;

/// Converts one script result value into the closest native Python value.
///
/// Every `Dynamic` a terminal (`to_list()`/`next()`) can produce funnels through
/// `element_property_to_dynamic` in `graphersal::script`, whose output is exactly the set of
/// variants matched below (unit, bool, int, float, string, array, map, `ScriptUuid`, the five
/// date and time types (see `temporal.rs`), and a vertex
/// or an edge, which become a `Vertex` / `Edge`; projections are plain maps). A variant this
/// matcher does not recognize raises `GraphersalError` naming it, rather than falling back to
/// `Debug` formatting (CLAUDE.md rule 14).
pub(crate) fn dynamic_to_pyobject(py: Python<'_>, value: Dynamic) -> PyResult<Py<PyAny>> {
    if value.is_unit() {
        return Ok(py.None());
    }
    if value.is_bool() {
        let b = value.as_bool().map_err(|_| type_name_error(&value))?;
        return Ok(b.into_pyobject(py)?.to_owned().into_any().unbind());
    }
    if value.is_int() {
        let n = value.as_int().map_err(|_| type_name_error(&value))?;
        return Ok(n.into_pyobject(py)?.into_any().unbind());
    }
    if value.is_float() {
        let n = value.as_float().map_err(|_| type_name_error(&value))?;
        return Ok(n.into_pyobject(py)?.into_any().unbind());
    }
    if value.is_string() {
        let s = value
            .into_immutable_string()
            .map_err(|_| PyTypeError::new_err("expected a Rhai string"))?;
        return Ok(s.as_str().into_pyobject(py)?.into_any().unbind());
    }
    if value.is_array() {
        let array = value
            .into_array()
            .map_err(|_| PyTypeError::new_err("expected a Rhai array"))?;
        let items = array
            .into_iter()
            .map(|item| dynamic_to_pyobject(py, item))
            .collect::<PyResult<Vec<_>>>()?;
        return Ok(PyList::new(py, items)?.into_any().unbind());
    }
    if value.is_map() {
        let map: graphersal::script::rhai::Map = value.cast();
        let dict = PyDict::new(py);
        for (key, item) in map {
            dict.set_item(key.as_str(), dynamic_to_pyobject(py, item)?)?;
        }
        return Ok(dict.into_any().unbind());
    }
    // `profile()` returns a `TraversalMetrics` value and `execute()` an `Execution`: both become
    // the dict of their `to_map()` (snake_case keys, durations as integer `duration_ns`).
    if value.is::<TraversalMetrics>() {
        let mut metrics = value.cast::<TraversalMetrics>();
        return dynamic_to_pyobject(py, metrics_to_map(&mut metrics));
    }
    if value.is::<ScriptExecution>() {
        let mut execution = value.cast::<ScriptExecution>();
        return dynamic_to_pyobject(py, execution.to_map());
    }
    if let Some(temporal) = script_temporal(&value) {
        return crate::temporal::temporal_to_pyobject(py, temporal);
    }
    if value.is::<ScriptUuid>() {
        let uuid = value.cast::<ScriptUuid>();
        let uuid = uuid::Uuid::from_u128(uuid.as_u128());
        return Ok(uuid.into_pyobject(py)?.into_any().unbind());
    }
    if value.is::<ScriptVector>() {
        let vector = value.cast::<ScriptVector>();
        return crate::vector::vector_to_pyobject(py, vector.vector().clone());
    }
    if value.is::<ScriptVertex>() {
        let vertex = value.cast::<ScriptVertex>();
        return Ok(crate::element::vertex_to_python(
            py,
            vertex.as_vertex_value(),
            property_to_pyobject,
        )?
        .into_any());
    }
    if value.is::<ScriptEdge>() {
        let edge = value.cast::<ScriptEdge>();
        return Ok(
            crate::element::edge_to_python(py, edge.as_edge_value(), property_to_pyobject)?
                .into_any(),
        );
    }
    Err(type_name_error(&value))
}

/// Converts a property value of a vertex or an edge into the closest native Python value, the
/// same mapping [`dynamic_to_pyobject`] applies to script values (a map with a non-string key
/// becomes a dict with those keys).
pub(crate) fn property_to_pyobject(py: Python<'_>, value: &ElementProperty) -> PyResult<Py<PyAny>> {
    Ok(match value.to_logical() {
        ElementProperty::Null => py.None(),
        ElementProperty::Boolean(b) => b.into_pyobject(py)?.to_owned().into_any().unbind(),
        ElementProperty::Number(n) => match n.as_i64() {
            Some(i) if n.is_i64() => i.into_pyobject(py)?.into_any().unbind(),
            _ => n
                .as_f64()
                .unwrap_or(f64::NAN)
                .into_pyobject(py)?
                .into_any()
                .unbind(),
        },
        ElementProperty::String(s) => s.as_str().into_pyobject(py)?.into_any().unbind(),
        ElementProperty::Uuid(u) => uuid::Uuid::from_u128(u)
            .into_pyobject(py)?
            .into_any()
            .unbind(),
        ElementProperty::Temporal(value) => crate::temporal::temporal_to_pyobject(py, value)?,
        ElementProperty::Vector(vector) => crate::vector::vector_to_pyobject(py, vector)?,
        ElementProperty::Array(items) => {
            let items = items
                .iter()
                .map(|item| property_to_pyobject(py, item))
                .collect::<PyResult<Vec<_>>>()?;
            PyList::new(py, items)?.into_any().unbind()
        }
        ElementProperty::Object(map) => {
            let dict = PyDict::new(py);
            for (key, item) in map.iter() {
                dict.set_item(key.as_str(), property_to_pyobject(py, item)?)?;
            }
            dict.into_any().unbind()
        }
        ElementProperty::Map(map) => {
            let dict = PyDict::new(py);
            for (key, item) in map.iter() {
                dict.set_item(
                    property_to_pyobject(py, key)?,
                    property_to_pyobject(py, item)?,
                )?;
            }
            dict.into_any().unbind()
        }
        ElementProperty::Vertex(vertex) => {
            crate::element::vertex_to_python(py, &vertex, property_to_pyobject)?.into_any()
        }
        ElementProperty::Edge(edge) => {
            crate::element::edge_to_python(py, &edge, property_to_pyobject)?.into_any()
        }
        // `to_logical()` leaves no compressed string; a later value kind is reported, never
        // guessed (CLAUDE.md rule 14).
        _ => {
            return Err(crate::error::GraphersalError::new_err(
                "unsupported property value kind: py-graphersal's property -> Python matcher does not recognize it",
            ));
        }
    })
}

/// Converts a serialized `ChangeSet` into plain Python data: `dict`/`list`/`str`/`int`/
/// `float`/`bool`/`None`, keys and type tags (`{"@type": "g:UUID", "@value": ".."}`) unchanged.
///
/// The change set goes through its own serde form (the documented, tagged JSON shape of
/// `graphersal::changes::ChangeSet`), never through `Debug` (CLAUDE.md rule 14). The JSON text is
/// read by Python's `json.loads`, which keeps the key order the change set writes (properties in
/// stored order); a `serde_json::Value` would sort the keys.
pub(crate) fn change_set_to_pyobject(
    py: Python<'_>,
    change_set: &graphersal::changes::ChangeSet,
) -> PyResult<Py<PyAny>> {
    let text = serde_json::to_string(change_set).map_err(|err| {
        crate::error::GraphersalError::new_err(format!("cannot serialize the change set: {err}"))
    })?;
    let loads = py.import("json")?.getattr("loads")?;
    Ok(loads.call1((text,))?.unbind())
}

/// The date or time value a script value holds (one Rhai type per temporal type).
fn script_temporal(value: &Dynamic) -> Option<Temporal> {
    if let Some(v) = value.read_lock::<DateTime>() {
        return Some(Temporal::DateTime(*v));
    }
    if let Some(v) = value.read_lock::<Date>() {
        return Some(Temporal::Date(*v));
    }
    if let Some(v) = value.read_lock::<LocalDateTime>() {
        return Some(Temporal::LocalDateTime(*v));
    }
    if let Some(v) = value.read_lock::<Time>() {
        return Some(Temporal::Time(*v));
    }
    if let Some(v) = value.read_lock::<OffsetTime>() {
        return Some(Temporal::OffsetTime(*v));
    }
    value
        .read_lock::<Duration>()
        .map(|v| Temporal::Duration(*v))
}

fn type_name_error(value: &Dynamic) -> PyErr {
    crate::error::GraphersalError::new_err(format!(
        "unsupported script result value of type '{}': py-graphersal's Dynamic -> Python matcher does not recognize it",
        value.type_name()
    ))
}

/// Converts the `params` keyword argument into named `ElementProperty` bindings.
///
/// Only scalars (`int`, `float`, `str`, `bool`, `None`, `uuid.UUID`, the `datetime` values and
/// `Interval`, see `temporal.rs`) and a `Vertex` / `Edge` (by its
/// id and label) are accepted; a
/// `list`/`dict` value raises `GraphersalError` naming the offending key, per the design spec.
pub(crate) fn python_params_to_element_properties(
    params: &Bound<'_, PyDict>,
) -> PyResult<HashMap<String, ElementProperty>> {
    let mut out = HashMap::with_capacity(params.len());
    for (key, value) in params.iter() {
        let key: String = key
            .extract()
            .map_err(|_| PyTypeError::new_err("params keys must be strings"))?;
        let element = python_value_to_element_property(&value).map_err(|err| {
            crate::error::GraphersalError::new_err(format!("params['{key}']: {err}"))
        })?;
        out.insert(key, element);
    }
    Ok(out)
}

fn python_value_to_element_property(value: &Bound<'_, PyAny>) -> Result<ElementProperty, String> {
    if value.is_none() {
        return Ok(ElementProperty::Null);
    }
    // Checked before `i64`/`f64`: Python's `bool` is a subclass of `int`, so extracting an int
    // first would silently turn `True`/`False` into `1`/`0`.
    if let Ok(b) = value.cast::<PyBool>() {
        return Ok(ElementProperty::Boolean(b.is_true()));
    }
    // An `int` outside the int64 range is an error, never a silent (lossy) fall-through to the
    // `f64` arm below, which would accept it as a float.
    if value.is_instance_of::<PyInt>() {
        return value
            .extract::<i64>()
            .map(|i| ElementProperty::Number(Number::from_i64(i)))
            .map_err(|_| "int is outside the int64 range".to_string());
    }
    if let Ok(f) = value.extract::<f64>() {
        return Ok(ElementProperty::Number(Number::from_f64(f)));
    }
    if let Ok(s) = value.extract::<String>() {
        return Ok(ElementProperty::String(CompactString::from(s)));
    }
    if let Ok(u) = value.extract::<uuid::Uuid>() {
        return Ok(ElementProperty::Uuid(u.as_u128()));
    }
    if let Some(temporal) = crate::temporal::python_to_temporal(value)? {
        return Ok(ElementProperty::Temporal(temporal));
    }
    if let Some(vector) = crate::vector::python_vector(value) {
        return Ok(ElementProperty::Vector(vector));
    }
    if let Some(element) = crate::element::python_element_to_property(value) {
        return Ok(element);
    }
    // A float32/float64 buffer (a numpy embedding) is an f32 vector, like Vector(array).
    if let Some(vector) = crate::vector::python_buffer_vector(value)? {
        return Ok(ElementProperty::Vector(vector));
    }
    Err(format!(
        "unsupported parameter type '{}'; only int, float, str, bool, None, uuid.UUID, datetime.datetime, datetime.date, datetime.time, datetime.timedelta (pandas.Timestamp and pandas.Timedelta too), graphersal.Interval, graphersal.Vector (or a float32/float64 numpy array), Vertex and Edge are accepted",
        value
            .get_type()
            .name()
            .map(|n| n.to_string())
            .unwrap_or_else(|_| "<unknown>".to_string())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use graphersal::script::rhai::Map as RhaiMap;
    use pyo3::types::{PyFloat, PyString};

    fn with_gil<F: FnOnce(Python<'_>)>(f: F) {
        Python::attach(f);
    }

    /// `profile()` and `execute()` results become plain dicts with snake_case keys.
    #[test]
    fn converts_metrics_and_executions_to_dicts() {
        let graph = std::sync::Arc::new(graphersal::GraphSource::tinkerpop_modern());
        let metrics = graphersal::script::eval_value(graph.clone(), "g.V().out().profile()")
            .expect("profile");
        let execution = graphersal::script::eval_value(
            graph,
            r#"g.V().values("name").sum().execute(#{profile: true})"#,
        )
        .expect("execute never throws");
        with_gil(|py| {
            let obj = dynamic_to_pyobject(py, metrics).unwrap();
            let dict = obj.bind(py).cast::<PyDict>().unwrap();
            assert!(dict.get_item("duration_ns").unwrap().is_some());
            let steps = dict.get_item("metrics").unwrap().unwrap();
            assert_eq!(steps.len().unwrap(), 2);

            let obj = dynamic_to_pyobject(py, execution).unwrap();
            let dict = obj.bind(py).cast::<PyDict>().unwrap();
            let error = dict.get_item("error").unwrap().unwrap();
            let step_id = error.get_item("step_id").unwrap();
            assert_eq!(step_id.extract::<String>().unwrap(), "2");
            // The plain message, without the rendered "Error: " prefix of the diagnostic.
            let message = error
                .get_item("message")
                .unwrap()
                .extract::<String>()
                .unwrap();
            assert!(
                message.starts_with("Step #2 'sum()' execution failed: "),
                "{message}"
            );
            let diagnostic = error
                .get_item("diagnostic")
                .unwrap()
                .extract::<String>()
                .unwrap();
            assert!(diagnostic.starts_with("Error: "), "{diagnostic}");
            assert!(
                dict.get_item("profile")
                    .unwrap()
                    .unwrap()
                    .is_instance_of::<PyDict>()
            );
        });
    }

    #[test]
    fn converts_unit_to_none() {
        with_gil(|py| {
            let obj = dynamic_to_pyobject(py, Dynamic::UNIT).unwrap();
            assert!(obj.bind(py).is_none());
        });
    }

    #[test]
    fn converts_bool() {
        with_gil(|py| {
            let obj = dynamic_to_pyobject(py, Dynamic::from(true)).unwrap();
            assert!(obj.bind(py).is_instance_of::<PyBool>());
            assert!(obj.bind(py).extract::<bool>().unwrap());
        });
    }

    #[test]
    fn converts_int() {
        with_gil(|py| {
            let obj = dynamic_to_pyobject(py, Dynamic::from_int(42)).unwrap();
            assert!(obj.bind(py).is_instance_of::<PyInt>());
            assert_eq!(obj.bind(py).extract::<i64>().unwrap(), 42);
        });
    }

    #[test]
    fn converts_float() {
        with_gil(|py| {
            let obj = dynamic_to_pyobject(py, Dynamic::from_float(3.5)).unwrap();
            assert!(obj.bind(py).is_instance_of::<PyFloat>());
            assert_eq!(obj.bind(py).extract::<f64>().unwrap(), 3.5);
        });
    }

    #[test]
    fn converts_string() {
        with_gil(|py| {
            let obj = dynamic_to_pyobject(py, Dynamic::from(String::from("hi"))).unwrap();
            assert!(obj.bind(py).is_instance_of::<PyString>());
            assert_eq!(obj.bind(py).extract::<String>().unwrap(), "hi");
        });
    }

    #[test]
    fn converts_array_to_list() {
        with_gil(|py| {
            let arr: Vec<Dynamic> = vec![Dynamic::from_int(1), Dynamic::from_int(2)];
            let obj = dynamic_to_pyobject(py, Dynamic::from_array(arr)).unwrap();
            let list = obj.bind(py).cast::<PyList>().unwrap();
            assert_eq!(list.len(), 2);
        });
    }

    #[test]
    fn converts_map_to_dict() {
        with_gil(|py| {
            let mut map = RhaiMap::new();
            map.insert("name".into(), Dynamic::from(String::from("alice")));
            let obj = dynamic_to_pyobject(py, Dynamic::from_map(map)).unwrap();
            let dict = obj.bind(py).cast::<PyDict>().unwrap();
            assert_eq!(
                dict.get_item("name")
                    .unwrap()
                    .unwrap()
                    .extract::<String>()
                    .unwrap(),
                "alice"
            );
        });
    }

    #[test]
    fn converts_script_uuid() {
        // `ScriptUuid` has no public constructor outside `graphersal` (by design); evaluate the
        // same `UUID.fromString(..)` a script would use to produce one, matching how this
        // variant actually reaches the conversion matcher in `execute()`/`query()`.
        use std::sync::Arc;
        let graph = Arc::new(graphersal::GraphSource::empty());
        let dynamic = graphersal::script::eval_value(
            graph,
            "UUID.fromString(\"550e8400-e29b-41d4-a716-446655440000\")",
        )
        .unwrap();
        with_gil(|py| {
            let obj = dynamic_to_pyobject(py, dynamic).unwrap();
            let extracted = obj.bind(py).extract::<uuid::Uuid>().unwrap();
            assert_eq!(
                extracted.to_string(),
                "550e8400-e29b-41d4-a716-446655440000"
            );
        });
    }

    #[test]
    fn params_convert_each_supported_scalar_type() {
        with_gil(|py| {
            let dict = PyDict::new(py);
            dict.set_item("a", 1_i64).unwrap();
            dict.set_item("b", 1.5_f64).unwrap();
            dict.set_item("c", "hello").unwrap();
            dict.set_item("d", true).unwrap();
            dict.set_item("e", py.None()).unwrap();
            let params = python_params_to_element_properties(&dict).unwrap();
            assert!(matches!(params["a"], ElementProperty::Number(_)));
            assert!(matches!(params["b"], ElementProperty::Number(_)));
            assert!(matches!(params["c"], ElementProperty::String(_)));
            assert!(matches!(params["d"], ElementProperty::Boolean(true)));
            assert!(matches!(params["e"], ElementProperty::Null));
        });
    }

    #[test]
    fn params_reject_ints_outside_int64_instead_of_widening_to_float() {
        with_gil(|py| {
            let dict = PyDict::new(py);
            let big = py.eval(c"2 ** 70", None, None).unwrap();
            dict.set_item("big", big).unwrap();
            let err = python_params_to_element_properties(&dict).unwrap_err();
            assert!(err.to_string().contains("int64"), "{err}");
        });
    }

    #[test]
    fn params_reject_list_values() {
        with_gil(|py| {
            let dict = PyDict::new(py);
            let list = PyList::new(py, [1, 2, 3]).unwrap();
            dict.set_item("bad", list).unwrap();
            let err = python_params_to_element_properties(&dict).unwrap_err();
            assert!(err.to_string().contains("bad"));
        });
    }
}
