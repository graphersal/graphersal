//! Editing the properties of one element (the Graph panel's property editor): the element with
//! its properties as TYPED JSON, and a write of changed and removed top-level keys as ONE unit,
//! validated by the engine (schema enforcement, required properties, coercion) and refused
//! whole when any key fails. The same operation answers a dry run (`validateOnly`), so the
//! editor shows the engine's own errors next to their fields before it saves.
//!
//! Plain JSON loses the logical type of a value (`1` and `1.0` are one JS number, a uuid is a
//! string), so values cross this boundary in a **typed form** (never guessed from content, rule
//! 7): `{"type": T, "value": V}` with
//!
//! - `null` (no `value`), `boolean` (`true`/`false`), `string`;
//! - `int64`: the decimal text (`"42"`; a JS number cannot hold every `i64`), a JSON integer is
//!   accepted on input;
//! - `float64`: a JSON number, or `"NaN"`, `"Infinity"`, `"-Infinity"`;
//! - `uuid`: the canonical text (lowercase `8-4-4-4-12`);
//! - `date`, `localdatetime`, `datetime`, `time`, `duration`: the canonical ISO 8601 text
//!   (`2023-08-02T00:00:00Z`);
//! - `vector`: a list of JSON numbers (the dequantized values) with `"element"` (`f32`, `f16`,
//!   `i8`, `bit`; default `f32`): `{"type": "vector", "element": "i8", "value": [0.5, -1.0]}`;
//!   the values are quantized to the element on input;
//! - `array`: a list of typed values; `object`: a list of `[key, typed value]` pairs (the
//!   engine's key order).
//!
//! Forms:
//!
//! - **edit view** ([`Session::element_for_edit`]): `{kind, id, label, labels, source?, target?,
//!   properties: [[key, typed]], readOnly}` (`readOnly`: why the graph cannot be written now,
//!   [`Session::read_only_reason`], or `null`); `null` when the element does not exist;
//! - **update request** ([`Session::update_element`]): `{kind, id, set: [[key, typed]], remove:
//!   [key], validateOnly?}`; the answer is `{ok: true, changed, element}` (`changed`: a commit
//!   was made; a dry run answers `changed: false` and the element as it is) or `{ok: false,
//!   errors: [{field, message, help?}]}` with `field` the top-level key a failure belongs to or
//!   `general`.

use crate::error::SessionError;
use graphersal::prelude::*;
use graphersal::prelude::{CompactString, Number, PropertyMap};
use graphersal::storage::ElementKind;
use graphersal::temporal::{Temporal, TemporalKind};
use graphersal::vector::VectorElement;
use serde_json::{Map, Value, json};

use crate::Session;
use crate::queries::FieldError;

/// The most keys one update may set or remove.
pub const MAX_EDIT_KEYS: usize = 10_000;

/// The typed form of a stored value (module docs). A compressed string is its text.
pub fn typed_value(value: &ElementProperty) -> Value {
    match value.logical().as_ref() {
        ElementProperty::Null => json!({"type": "null"}),
        ElementProperty::Boolean(b) => json!({"type": "boolean", "value": b}),
        ElementProperty::String(s) => json!({"type": "string", "value": s.as_str()}),
        ElementProperty::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => json!({"type": "int64", "value": i.to_string()}),
            (None, Some(f)) if f.is_finite() => json!({"type": "float64", "value": f}),
            (None, Some(f)) => {
                let text = if f.is_nan() {
                    "NaN"
                } else if f > 0.0 {
                    "Infinity"
                } else {
                    "-Infinity"
                };
                json!({"type": "float64", "value": text})
            }
            (None, None) => json!({"type": "null"}),
        },
        ElementProperty::Uuid(_) => json!({
            "type": "uuid",
            "value": serde_json::to_value(value.logical().as_ref()).unwrap_or(Value::Null),
        }),
        ElementProperty::Temporal(t) => json!({"type": t.kind().name(), "value": t.to_string()}),
        // The element kind and the (dequantized) values; written back, the values are quantized
        // to the element again (the same vector).
        ElementProperty::Vector(v) => json!({
            "type": "vector",
            "element": v.element().name(),
            "value": serde_json::to_value(value.logical().as_ref()).unwrap_or(Value::Null),
        }),
        ElementProperty::Array(items) => json!({
            "type": "array",
            "value": items.iter().map(typed_value).collect::<Vec<_>>(),
        }),
        ElementProperty::Object(fields) => {
            json!({"type": "object", "value": typed_entries(fields)})
        }
        // Not storable as properties (vertices, edges, maps with non-string keys): shown as
        // their plain JSON text, never written back unless the user changes them.
        other => json!({
            "type": "string",
            "value": serde_json::to_string(other).unwrap_or_default(),
        }),
    }
}

/// The `[key, typed]` pairs of a property map, in the engine's order.
fn typed_entries(fields: &PropertyMap) -> Vec<Value> {
    fields
        .iter()
        .map(|(key, value)| json!([key.as_str(), typed_value(value)]))
        .collect()
}

/// A value from its typed form (module docs); the error says what is wrong, for the editor.
pub fn value_from_typed(typed: &Value) -> Result<ElementProperty, SessionError> {
    let ty = typed
        .get("type")
        .and_then(Value::as_str)
        .ok_or("a typed value needs a \"type\"")?;
    let value = typed.get("value").unwrap_or(&Value::Null);
    match ty {
        "null" => Ok(ElementProperty::Null),
        "boolean" => value
            .as_bool()
            .map(ElementProperty::Boolean)
            .ok_or_else(|| SessionError::invalid("a boolean is true or false")),
        "string" => value
            .as_str()
            .map(ElementProperty::from)
            .ok_or_else(|| SessionError::invalid("a string value must be text")),
        "int64" => {
            let parsed = match value {
                Value::String(text) => text.trim().parse::<i64>().ok(),
                Value::Number(n) => n.as_i64(),
                _ => None,
            };
            parsed
                .map(|i| ElementProperty::Number(Number::from_i64(i)))
                .ok_or_else(|| format!("{value} is not an int64 (a whole number)"))
                .map_err(SessionError::from)
        }
        "float64" => {
            let parsed = match value {
                Value::Number(n) => n.as_f64(),
                Value::String(text) => match text.trim() {
                    "NaN" => Some(f64::NAN),
                    "Infinity" => Some(f64::INFINITY),
                    "-Infinity" => Some(f64::NEG_INFINITY),
                    other => other.parse::<f64>().ok(),
                },
                _ => None,
            };
            parsed
                .map(|f| ElementProperty::Number(Number::from_f64(f)))
                .ok_or_else(|| format!("{value} is not a float64 (a number)"))
                .map_err(SessionError::from)
        }
        "uuid" => {
            let text = value.as_str().ok_or("a uuid value must be text")?;
            ElementProperty::uuid_from_string(text)
                .map_err(|_| {
                    format!(
                        "\"{text}\" is not a canonical uuid (lowercase 8-4-4-4-12 hex digits, e.g. \
                     550e8400-e29b-41d4-a716-446655440000)"
                    )
                })
                .map_err(SessionError::from)
        }
        "array" => value
            .as_array()
            .ok_or("an array value must be a list")?
            .iter()
            .map(value_from_typed)
            .collect::<Result<Vec<_>, _>>()
            .map(ElementProperty::Array),
        "object" => {
            let mut fields = PropertyMap::default();
            for pair in value
                .as_array()
                .ok_or("an object value must be a list of pairs")?
            {
                let (key, item) = key_and_value(pair)?;
                fields.insert(CompactString::from(key), value_from_typed(item)?);
            }
            Ok(ElementProperty::Object(Box::new(fields)))
        }
        kind @ ("date" | "localdatetime" | "datetime" | "time" | "offsettime" | "duration") => {
            let text = value
                .as_str()
                .ok_or_else(|| format!("a {kind} value must be text"))?;
            let kind = match kind {
                "date" => TemporalKind::Date,
                "localdatetime" => TemporalKind::LocalDateTime,
                "datetime" => TemporalKind::DateTime,
                "time" => TemporalKind::Time,
                "offsettime" => TemporalKind::OffsetTime,
                _ => TemporalKind::Duration,
            };
            // As a stored value: a datetime needs its offset or zone (no guessed instant).
            Temporal::parse_strict(kind, text)
                .map(ElementProperty::Temporal)
                .map_err(|err| err.to_string())
                .map_err(SessionError::from)
        }
        "vector" => {
            let element = match typed.get("element") {
                None | Some(Value::Null) => VectorElement::F32,
                Some(Value::String(name)) => VectorElement::from_name(name).ok_or_else(|| {
                    format!("unknown vector element \"{name}\": f32, f16, i8 or bit")
                })?,
                Some(_) => {
                    return Err("a vector's element is f32, f16, i8 or bit"
                        .to_string()
                        .into());
                }
            };
            let items = value
                .as_array()
                .ok_or("a vector value must be a list of numbers")?;
            let mut numbers = Vec::with_capacity(items.len());
            for item in items {
                numbers.push(
                    item.as_f64()
                        .ok_or("a vector value must be a list of numbers")?,
                );
            }
            Vector::from_f64(&numbers)
                .and_then(|vector| vector.quantize(element))
                .map(ElementProperty::Vector)
                .map_err(|err| err.to_string())
                .map_err(SessionError::from)
        }
        other => Err(format!(
            "unknown type \"{other}\": null, boolean, int64, float64, string, uuid, datetime, \
             localdatetime, date, time, offsettime, duration, vector, array or object"
        )
        .into()),
    }
}

/// `[key, typed]` of a pair.
pub(crate) fn key_and_value(pair: &Value) -> Result<(&str, &Value), SessionError> {
    match pair.as_array().map(Vec::as_slice) {
        Some([Value::String(key), value]) => Ok((key.as_str(), value)),
        _ => Err("expected a [key, typed value] pair".to_string().into()),
    }
}

pub(crate) fn kind_of(kind: &str) -> Result<ElementKind, SessionError> {
    match kind {
        "vertex" => Ok(ElementKind::Vertex),
        "edge" => Ok(ElementKind::Edge),
        other => {
            Err(format!("unknown element kind \"{other}\": expected \"vertex\" or \"edge\"").into())
        }
    }
}

/// What a write of one key does.
enum Op {
    Set(CompactString, ElementProperty),
    Remove(CompactString),
}

/// Why an update did not commit.
enum EditFailed {
    /// Failures of single keys (and general ones), shown in the editor.
    Fields(Vec<FieldError>),
    /// The unit failed as a whole (a vetoed commit: a read-only view, a damaged store).
    Graph(GraphError),
}

impl From<GraphError> for EditFailed {
    fn from(err: GraphError) -> Self {
        EditFailed::Graph(err)
    }
}

pub(crate) fn field_error(field: &str, err: &GraphError) -> FieldError {
    let mut error = FieldError::new(field, err.to_string());
    error.help = err.help();
    error
}

/// Applies `ops` to the element, collecting the failure of each key (every key is tried, so
/// the editor shows all of them at once). Each storage call validates atomically; the caller's
/// unit rolls back the successful ones when any failed.
fn apply<S: GraphStorage>(
    graph: &mut S,
    kind: ElementKind,
    id: &str,
    ops: &[Op],
) -> Vec<FieldError> {
    let mut errors = Vec::new();
    match kind {
        ElementKind::Vertex => {
            let v = match graph.get_vertex_by_id(id) {
                Ok(Some(v)) => v,
                Ok(None) => return vec![gone(kind)],
                Err(err) => return vec![field_error("general", &err)],
            };
            for op in ops {
                let (key, result) = match op {
                    Op::Set(key, value) => (
                        key,
                        graph.set_vertex_property(v, key.clone(), value.clone()),
                    ),
                    Op::Remove(key) => (key, graph.drop_vertex_property(key, v)),
                };
                if let Err(err) = result {
                    errors.push(field_error(key, &err));
                }
            }
        }
        ElementKind::Edge => {
            let e = match graph.get_edge_by_id(id) {
                Ok(Some(e)) => e,
                Ok(None) => return vec![gone(kind)],
                Err(err) => return vec![field_error("general", &err)],
            };
            for op in ops {
                let (key, result) = match op {
                    Op::Set(key, value) => {
                        (key, graph.set_edge_property(e, key.clone(), value.clone()))
                    }
                    Op::Remove(key) => (key, graph.drop_edge_property(key, e)),
                };
                if let Err(err) = result {
                    errors.push(field_error(key, &err));
                }
            }
        }
    }
    errors
}

fn gone(kind: ElementKind) -> FieldError {
    FieldError::new(
        "general",
        format!(
            "this {} no longer exists (dropped since it was drawn): reload the drawing",
            kind.as_str()
        ),
    )
}

/// `{ok: false, errors}`.
pub(crate) fn refused(errors: Vec<FieldError>) -> Value {
    json!({
        "ok": false,
        "errors": errors.iter().map(FieldError::to_value).collect::<Vec<_>>(),
    })
}

/// The operations of an update request, removals first; field errors for malformed values.
fn parse_ops(request: &Value) -> Result<Vec<Op>, Vec<FieldError>> {
    let mut ops = Vec::new();
    let mut errors = Vec::new();
    let remove = match request.get("remove") {
        None | Some(Value::Null) => &[][..],
        Some(Value::Array(keys)) => keys.as_slice(),
        Some(_) => {
            return Err(vec![FieldError::new(
                "general",
                "\"remove\" must be a list of keys",
            )]);
        }
    };
    let set = match request.get("set") {
        None | Some(Value::Null) => &[][..],
        Some(Value::Array(pairs)) => pairs.as_slice(),
        Some(_) => {
            return Err(vec![FieldError::new(
                "general",
                "\"set\" must be a list of [key, typed value] pairs",
            )]);
        }
    };
    if remove.len() + set.len() > MAX_EDIT_KEYS {
        return Err(vec![FieldError::new(
            "general",
            format!("at most {MAX_EDIT_KEYS} keys can be changed at once"),
        )]);
    }
    for key in remove {
        match key.as_str() {
            Some(key) => ops.push(Op::Remove(CompactString::from(key))),
            None => errors.push(FieldError::new("general", "a removed key must be text")),
        }
    }
    for pair in set {
        match key_and_value(pair) {
            Ok(("", _)) => errors.push(FieldError::new(
                "general",
                "a property name cannot be empty",
            )),
            Ok((key, typed)) => match value_from_typed(typed) {
                Ok(value) => ops.push(Op::Set(CompactString::from(key), value)),
                Err(message) => errors.push(FieldError::new(key, message)),
            },
            Err(message) => errors.push(FieldError::new("general", message)),
        }
    }
    if errors.is_empty() {
        Ok(ops)
    } else {
        Err(errors)
    }
}

/// The element `id` of `kind` in `g` as the property editor's edit view (module docs) without
/// `readOnly` (the session's to add); `Value::Null` when there is none, `Err` when the storage
/// cannot read it.
pub fn element_view<S: GraphStorage>(
    g: &S,
    kind: ElementKind,
    id: &str,
) -> Result<Value, GraphError> {
    let mut view = Map::new();
    view.insert("kind".into(), json!(kind.as_str()));
    match kind {
        ElementKind::Vertex => {
            let Some(v) = g.get_vertex_by_id(id)? else {
                return Ok(Value::Null);
            };
            let labels: Vec<&str> = g.vertex_labels(v)?.iter().map(|l| l.as_str()).collect();
            view.insert("id".into(), json!(g.vertex_id(v)?.unwrap_or_default()));
            view.insert(
                "label".into(),
                json!(labels.first().copied().unwrap_or_default()),
            );
            view.insert("labels".into(), json!(labels));
            let props = g
                .vertex_properties(v)?
                .map(typed_entries)
                .unwrap_or_default();
            view.insert("properties".into(), Value::Array(props));
        }
        ElementKind::Edge => {
            let Some(e) = g.get_edge_by_id(id)? else {
                return Ok(Value::Null);
            };
            let label = g.edge_label(e)?.unwrap_or_default();
            view.insert("id".into(), json!(g.edge_id(e)?.unwrap_or_default()));
            view.insert("label".into(), json!(label));
            view.insert("labels".into(), json!([label]));
            if let Some((s, t)) = g.edge_endpoints(e)? {
                view.insert("source".into(), json!(g.vertex_id(s)?.unwrap_or_default()));
                view.insert("target".into(), json!(g.vertex_id(t)?.unwrap_or_default()));
            }
            let props = g.edge_properties(e)?.map(typed_entries).unwrap_or_default();
            view.insert("properties".into(), Value::Array(props));
        }
    }
    Ok(Value::Object(view))
}

/// Writes `ops` to the element `id` of `kind` in `g` as ONE unit (`validate_only`: a dry run,
/// always rolled back): `Ok(changed)` (whether a commit happened; always `false` for a dry
/// run), or the failure of every key that failed (nothing applied).
fn update_in<S: GraphStorage>(
    g: &mut S,
    kind: ElementKind,
    id: &str,
    ops: &[Op],
    validate_only: bool,
) -> Result<bool, Vec<FieldError>> {
    if validate_only {
        return match g.dry_run(|g| Ok::<_, GraphError>(apply(g, kind, id, ops))) {
            Ok((errors, _)) if errors.is_empty() => Ok(false),
            Ok((errors, _)) => Err(errors),
            Err(err) => Err(vec![field_error("general", &err)]),
        };
    }
    let before = g
        .last_commit_seq()
        .map_err(|err| vec![field_error("general", &err)])?;
    let stored = g.transaction(|g| {
        let errors = apply(g, kind, id, ops);
        if errors.is_empty() {
            Ok(())
        } else {
            Err(EditFailed::Fields(errors))
        }
    });
    match stored {
        // The commit happened: a position that cannot be read afterwards counts as a change
        // (the front end refreshes rather than show a stale element).
        Ok(()) => Ok(g.last_commit_seq().map_or(true, |after| after != before)),
        Err(EditFailed::Fields(errors)) => Err(errors),
        Err(EditFailed::Graph(err)) => Err(vec![field_error("general", &err)]),
    }
}

/// [`update_in`] of an update request (`set`, `remove`; module docs): the request's operations
/// parsed first (malformed values are field errors, nothing is written).
pub(crate) fn update_request_in<S: GraphStorage>(
    g: &mut S,
    kind: ElementKind,
    id: &str,
    request: &Value,
    validate_only: bool,
) -> Result<bool, Vec<FieldError>> {
    let ops = parse_ops(request)?;
    update_in(g, kind, id, &ops, validate_only)
}

impl Session {
    /// Why the served graph cannot be written now, or `None`: a read-only view of a past state
    /// is open, the store is a backup, in maintenance mode (damage found at open), frozen by
    /// damage found while open, or read-only for a reason the engine gives (a critical
    /// definition or segment it does not know). A write would be refused anyway (a commit hook
    /// vetoes it); the editor says why before the user types.
    ///
    /// Fails when the store cannot say whether it is read-only (a poisoned store lock,
    /// [`graphersal::persist::StoreFailure::LockPoisoned`]); the editor's write refusal turns that
    /// into a refusal.
    pub fn read_only_reason(&self) -> Result<Option<String>, SessionError> {
        if !self.store_view().is_null() {
            return Ok(Some(
                "a read-only view of a past state is open: go back to the live graph (Store \
                 menu) to edit"
                    .to_string(),
            ));
        }
        let Some(store) = self.active_store() else {
            return Ok(None);
        };
        if let Some(marker) = store.backup_marker() {
            return Ok(Some(format!(
                "this is a backup (up to commit {}): it is read-only; restore it or fork it to \
                 edit",
                marker.commit_seq
            )));
        }
        if store.maintenance().is_some() {
            return Ok(Some(
                "the store is in maintenance mode (damage found when it was opened): it is \
                 read-only until it is repaired"
                    .to_string(),
            ));
        }
        if store.is_frozen_by_damage() {
            return Ok(Some(
                "damage was found on disk while the store was open: it is read-only now (damage \
                 policy maintenance); back it up and repair it"
                    .to_string(),
            ));
        }
        Ok(store
            .read_only_reason()?
            .map(|reason| format!("the store is read-only: {reason}")))
    }

    /// Why a write is refused now: [`Self::read_only_reason`], or the error that kept it from
    /// answering (a write is refused then too, with that error as its reason).
    pub(crate) fn write_refusal(&self) -> Option<String> {
        match self.read_only_reason() {
            Ok(reason) => reason,
            Err(err) => Some(err.to_string()),
        }
    }

    /// The element `id` of `kind` (`"vertex"` or `"edge"`) as the property editor's edit view
    /// (module docs); `Value::Null` when there is none. An unknown kind is an error.
    pub fn element_for_edit(&self, kind: &str, id: &str) -> Result<Value, SessionError> {
        let kind = kind_of(kind)?;
        let mut view = self.graph.element_view(kind, id)?;
        if let Value::Object(map) = &mut view {
            map.insert("readOnly".into(), json!(self.read_only_reason()?));
        }
        Ok(view)
    }

    /// Writes the changed (`set`) and removed (`remove`) top-level properties of one element
    /// as ONE unit (module docs): every key is tried, and when any fails (schema enforcement,
    /// a required property removed, a malformed value) nothing is applied and the answer
    /// lists each failure under its key. `validateOnly: true` runs the same writes as a dry
    /// run (always rolled back, no hooks) and answers the errors only. Never fails otherwise.
    pub fn update_element(&mut self, request: &Value) -> Value {
        let kind = match kind_of(request.get("kind").and_then(Value::as_str).unwrap_or("")) {
            Ok(kind) => kind,
            Err(message) => return refused(vec![FieldError::new("general", message)]),
        };
        let Some(id) = request.get("id").and_then(|id| match id {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        }) else {
            return refused(vec![FieldError::new(
                "general",
                "the request names no element id",
            )]);
        };
        let validate_only = request.get("validateOnly").and_then(Value::as_bool) == Some(true);
        if let Some(reason) = self.write_refusal() {
            return refused(vec![FieldError::new("general", reason)]);
        }
        let current = |s: &Self| {
            s.element_for_edit(kind.as_str(), &id)
                .unwrap_or(Value::Null)
        };
        let outcome = self.graph.update_element(kind, &id, request, validate_only);
        if !validate_only {
            // Without a checkpoint thread (wasm32) the automatic checkpoint runs here.
            self.run_due_checkpoint();
        }
        match outcome {
            Ok(changed) => json!({"ok": true, "changed": changed, "element": current(self)}),
            Err(errors) => refused(errors),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modern() -> Session {
        let mut s = Session::new();
        s.load_sample("modern").expect("modern");
        s
    }

    fn prop<'a>(view: &'a Value, key: &str) -> Option<&'a Value> {
        view["properties"]
            .as_array()?
            .iter()
            .find(|pair| pair[0] == key)
            .map(|pair| &pair[1])
    }

    #[test]
    fn typed_values_round_trip_every_logical_type() {
        let typed = json!({"type": "object", "value": [
            ["s", {"type": "string", "value": "1.5"}],
            ["i", {"type": "int64", "value": "9007199254740993"}],
            ["f", {"type": "float64", "value": 1.0}],
            ["inf", {"type": "float64", "value": "-Infinity"}],
            ["b", {"type": "boolean", "value": false}],
            ["n", {"type": "null"}],
            ["u", {"type": "uuid", "value": "550e8400-e29b-41d4-a716-446655440000"}],
            ["a", {"type": "array", "value": [{"type": "int64", "value": "1"}]}],
        ]});
        let value = value_from_typed(&typed).expect("valid");
        assert_eq!(typed_value(&value), typed);
        // A string never becomes another type by its content (rule 7).
        let s = value_from_typed(&json!({"type": "string", "value": "42"})).unwrap();
        assert_eq!(s, ElementProperty::from("42"));
        assert!(value_from_typed(&json!({"type": "int64", "value": "4.2"})).is_err());
        assert!(value_from_typed(&json!({"type": "uuid", "value": "X"})).is_err());
        assert!(value_from_typed(&json!({"type": "date", "value": "x"})).is_err());
    }

    #[test]
    fn date_and_time_values_are_typed_by_their_kind() {
        let typed = json!({"type": "object", "value": [
            ["at", {"type": "datetime", "value": "2023-08-02T09:30:00+02:00"}],
            ["local", {"type": "localdatetime", "value": "2023-08-02T09:30:00"}],
            ["on", {"type": "date", "value": "2024-02-29"}],
            ["t", {"type": "time", "value": "09:30:00.250"}],
            ["took", {"type": "duration", "value": "P1M2DT3H"}],
        ]});
        let value = value_from_typed(&typed).expect("valid");
        assert_eq!(typed_value(&value), typed);
        // A stored datetime needs its offset or zone: the instant is never guessed.
        let err = value_from_typed(&json!({"type": "datetime", "value": "2023-08-02T09:30"}))
            .unwrap_err();
        assert!(err.to_string().contains("2023-08-02T09:30"), "{err}");
        let zoned = value_from_typed(
            &json!({"type": "datetime", "value": "2023-08-02T09:30[Europe/Berlin]"}),
        )
        .unwrap();
        // The zone name is kept (temporal v2).
        assert_eq!(
            typed_value(&zoned),
            json!({"type": "datetime", "value": "2023-08-02T09:30:00+02:00[Europe/Berlin]"})
        );
        let time = json!({"type": "offsettime", "value": "09:30:00+02:00"});
        assert_eq!(typed_value(&value_from_typed(&time).unwrap()), time);
        assert!(value_from_typed(&json!({"type": "offsettime", "value": "09:30"})).is_err());
        assert!(value_from_typed(&json!({"type": "duration", "value": 5})).is_err());
        let unknown = value_from_typed(&json!({"type": "instant", "value": "x"})).unwrap_err();
        assert!(unknown.to_string().contains("datetime"), "{unknown}");
    }

    #[test]
    fn a_date_property_is_written_and_shown_typed() {
        let mut s = modern();
        let answer = s.update_element(&json!({
            "kind": "vertex", "id": "1",
            "set": [["born", {"type": "date", "value": "1990-05-01"}]],
        }));
        assert_eq!(answer["ok"], true, "{answer}");
        let born = prop(&answer["element"], "born").unwrap().clone();
        assert_eq!(born, json!({"type": "date", "value": "1990-05-01"}));
        let refused = s.update_element(&json!({
            "kind": "vertex", "id": "1",
            "set": [["at", {"type": "datetime", "value": "2023-08-02"}]],
            "validateOnly": true,
        }));
        assert_eq!(refused["ok"], false, "{refused}");
        assert_eq!(refused["errors"][0]["field"], "at", "{refused}");
    }

    #[test]
    fn edit_view_has_typed_properties_and_endpoints() {
        let s = modern();
        let v = s.element_for_edit("vertex", "1").unwrap();
        assert_eq!(v["label"], "person");
        assert_eq!(prop(&v, "age").unwrap()["type"], "int64");
        assert_eq!(v["readOnly"], Value::Null);
        let e = s.element_for_edit("edge", "0").unwrap();
        assert_eq!(e["source"], "1");
        assert_eq!(prop(&e, "weight").unwrap()["type"], "float64");
        assert_eq!(s.element_for_edit("vertex", "nope").unwrap(), Value::Null);
        assert!(s.element_for_edit("graph", "1").is_err());
    }

    #[test]
    fn update_sets_and_removes_in_one_commit() {
        let mut s = modern();
        let before = s.graph().last_commit_seq().unwrap();
        let answer = s.update_element(&json!({
            "kind": "vertex", "id": "1",
            "set": [["age", {"type": "int64", "value": "30"}],
                    ["tags", {"type": "array", "value": [{"type": "string", "value": "x"}]}]],
            "remove": ["name"],
        }));
        assert_eq!(answer["ok"], true, "{answer}");
        assert_eq!(answer["changed"], true);
        assert_eq!(s.graph().last_commit_seq().unwrap(), before + 1);
        let v = &answer["element"];
        assert_eq!(prop(v, "age").unwrap()["value"], "30");
        assert!(prop(v, "name").is_none());
        assert_eq!(prop(v, "tags").unwrap()["type"], "array");
    }

    #[test]
    fn a_dry_run_changes_nothing() {
        let mut s = modern();
        let before = s.graph().last_commit_seq().unwrap();
        let answer = s.update_element(&json!({
            "kind": "edge", "id": "0", "validateOnly": true,
            "set": [["weight", {"type": "float64", "value": 0.9}]],
        }));
        assert_eq!(answer["ok"], true, "{answer}");
        assert_eq!(answer["changed"], false);
        assert_eq!(s.graph().last_commit_seq().unwrap(), before);
        let e = s.element_for_edit("edge", "0").unwrap();
        assert_eq!(prop(&e, "weight").unwrap()["value"], 0.5);
    }

    #[test]
    fn a_schema_violation_refuses_the_whole_update_with_field_errors() {
        let mut s = modern();
        s.set_schema(
            r#"{"mode": "open", "vertices": {"person": {"schema": {"type": "object",
                "properties": {"name": {"type": "string"}, "age": {"type": "integer", "minimum": 0}},
                "required": ["name"]}}}}"#,
            None,
        )
        .expect("schema");
        let before = s.graph().last_commit_seq().unwrap();
        for validate_only in [true, false] {
            let answer = s.update_element(&json!({
                "kind": "vertex", "id": "1", "validateOnly": validate_only,
                "set": [["age", {"type": "int64", "value": "-1"}],
                        ["nick", {"type": "string", "value": "m"}]],
                "remove": ["name"],
            }));
            assert_eq!(answer["ok"], false, "{answer}");
            let fields: Vec<&str> = answer["errors"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["field"].as_str().unwrap())
                .collect();
            assert!(fields.contains(&"age"), "{answer}");
            assert!(fields.contains(&"name"), "{answer}");
            assert!(!fields.contains(&"nick"), "{answer}");
        }
        // Nothing was applied, not even the valid key.
        assert_eq!(s.graph().last_commit_seq().unwrap(), before);
        let v = s.element_for_edit("vertex", "1").unwrap();
        assert!(prop(&v, "nick").is_none());
        assert_eq!(prop(&v, "name").unwrap()["value"], "marko");
    }

    #[test]
    fn malformed_requests_are_refused_without_writing() {
        let mut s = modern();
        let answer = s.update_element(&json!({
            "kind": "vertex", "id": "1",
            "set": [["age", {"type": "int64", "value": "old"}]],
        }));
        assert_eq!(answer["errors"][0]["field"], "age");
        let gone = s.update_element(&json!({"kind": "vertex", "id": "99", "set": []}));
        assert_eq!(gone["ok"], false);
        assert!(
            gone["errors"][0]["message"]
                .as_str()
                .unwrap()
                .contains("no longer exists")
        );
        assert_eq!(
            s.update_element(&json!({"kind": "x", "id": "1"}))["ok"],
            false
        );
    }

    #[test]
    fn a_read_only_view_refuses_edits() {
        let mut s = Session::new();
        s.set_memory_stores(true);
        s.load_sample("modern").expect("modern");
        assert_eq!(s.read_only_reason().unwrap(), None);
        s.execute(
            "g.v(\"1\").property(\"age\", 30).iterate()",
            &Default::default(),
        );
        s.store_open_view(&json!({"target": {"commit": 0}}))
            .expect("view");
        let reason = s.read_only_reason().unwrap().expect("read-only");
        assert!(reason.contains("read-only view"), "{reason}");
        let v = s.element_for_edit("vertex", "1").unwrap();
        assert_eq!(v["readOnly"], json!(reason));
        let answer = s.update_element(&json!({
            "kind": "vertex", "id": "1",
            "set": [["age", {"type": "int64", "value": "31"}]],
        }));
        assert_eq!(answer["ok"], false);
        s.store_close_view().expect("live");
        assert_eq!(s.read_only_reason().unwrap(), None);
    }

    /// The modern graph twice: as a `TraversalGraph` and wrapped in the `Forwarding` storage
    /// (not `TraversalGraph`, own handle types), for the generic-helper comparisons.
    fn modern_pair() -> (
        TraversalGraph,
        graphersal_storage_tests::forwarding::Forwarding,
    ) {
        (
            TraversalGraph::tinkerpop_modern(),
            graphersal_storage_tests::forwarding::Forwarding::new(
                TraversalGraph::tinkerpop_modern(),
            ),
        )
    }

    #[test]
    fn edits_on_a_foreign_storage_equal_those_of_a_traversal_graph() {
        let (mut tg, mut fw) = modern_pair();
        assert_eq!(
            element_view(&tg, ElementKind::Vertex, "1").unwrap(),
            element_view(&fw, ElementKind::Vertex, "1").unwrap()
        );
        assert_eq!(
            element_view(&tg, ElementKind::Edge, "0").unwrap(),
            element_view(&fw, ElementKind::Edge, "0").unwrap()
        );
        let ops = [
            Op::Set("age".into(), ElementProperty::from(30i64)),
            Op::Remove("name".into()),
        ];
        // A dry run changes nothing, a real one commits once.
        for validate_only in [true, false] {
            let a = update_in(&mut tg, ElementKind::Vertex, "1", &ops, validate_only);
            let b = update_in(&mut fw, ElementKind::Vertex, "1", &ops, validate_only);
            assert_eq!(a.as_ref().ok(), Some(&!validate_only));
            assert_eq!(b.as_ref().ok(), Some(&!validate_only));
            assert_eq!(
                element_view(&tg, ElementKind::Vertex, "1").unwrap(),
                element_view(&fw, ElementKind::Vertex, "1").unwrap()
            );
        }
        assert_eq!(fw.last_commit_seq().unwrap(), tg.last_commit_seq());
        let bad = [Op::Set("weight".into(), ElementProperty::from(1i64))];
        let errors = |r: Result<bool, Vec<FieldError>>| {
            r.unwrap_err()
                .iter()
                .map(FieldError::to_value)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            errors(update_in(&mut tg, ElementKind::Edge, "none", &bad, false)),
            errors(update_in(&mut fw, ElementKind::Edge, "none", &bad, false))
        );
    }

    #[test]
    fn vectors_are_typed_with_their_element_and_round_trip() {
        let i8 = Vector::from_f32(vec![0.5, -1.0, 0.25])
            .unwrap()
            .quantize(VectorElement::I8)
            .unwrap();
        let value = ElementProperty::Vector(i8.clone());
        let typed = typed_value(&value);
        assert_eq!(typed["type"], "vector");
        assert_eq!(typed["element"], "i8");
        assert_eq!(typed["value"].as_array().map(Vec::len), Some(3));
        // Written back: the same vector (the dequantized values quantize to the same codes).
        assert_eq!(value_from_typed(&typed).unwrap(), value);
        // Without an element: f32.
        let plain = value_from_typed(&json!({"type": "vector", "value": [1, 2.5]})).unwrap();
        assert_eq!(
            plain,
            ElementProperty::Vector(Vector::from_f32(vec![1.0, 2.5]).unwrap())
        );
        for bad in [
            json!({"type": "vector", "value": []}),
            json!({"type": "vector", "value": [1, "x"]}),
            json!({"type": "vector", "value": "1, 2"}),
            json!({"type": "vector", "element": "f64", "value": [1]}),
            json!({"type": "vector", "element": "f16", "value": [1e10]}),
        ] {
            assert!(value_from_typed(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_json_list_written_to_a_declared_vector_becomes_one() {
        let mut s = Session::new();
        s.load_sample("empty").expect("empty");
        s.set_schema(
            r#"{"mode": "open", "vertices": {"doc": {"schema": {"type": "object", "properties": {
                "emb": {"type": "array", "format": "vector", "element": "f16", "dimension": 2}}}}}}"#,
            None,
        )
        .expect("schema");
        let out = s.execute(
            r#"g.add_v("doc").property(T.id, "d").iterate()"#,
            &crate::RunOptions::default(),
        );
        assert!(out.contains("\"ok\":true"), "{out}");
        let request = json!({"kind": "vertex", "id": "d",
            "set": [["emb", {"type": "array", "value": [
                {"type": "float64", "value": 0.5}, {"type": "float64", "value": 1.0}]}]]});
        let answer = s.update_element(&request);
        assert_eq!(answer["ok"], true, "{answer}");
        let view = s.element_for_edit("vertex", "d").unwrap();
        let emb = prop(&view, "emb").expect("emb");
        assert_eq!(emb["type"], "vector");
        assert_eq!(emb["element"], "f16");
    }
}
