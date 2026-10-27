//! The Graph panel's edit mode: creating a vertex or an edge and deleting one, each as ONE unit
//! validated by the engine, and the label choices the creation dialog offers. Values travel in
//! the typed form of [`crate::edit`] (never guessed from content, rule 7); a dry run
//! (`validateOnly`) answers the same errors as a write, at their fields, without committing.
//!
//! Forms:
//!
//! - **label choices** ([`Session::label_choices`]): request `{kind: "vertex"}` or `{kind:
//!   "edge", from, to}` (the endpoint vertex ids); answer `{kind, mode, free, labels: [{label,
//!   declared, count, connects}], readOnly}`. `labels` are the choices in the order to offer
//!   them: for vertices the labels the schema declares, then the graph's other vertex labels by
//!   count; for an edge the declared edge labels whose `connections` allow the pair (any label of
//!   an endpoint matches, `*` matches all, no connections allow every pair), then the graph's
//!   other edge labels, then (outside `closed`, where topology is not enforced) the declared
//!   labels whose connections name other labels (`connects: false`). In mode `closed` only
//!   declared (and, for edges, connecting) labels are offered and `free` (a label typed by hand)
//!   is false. `count`: elements with that label in the graph; `connects`: `null` for a vertex
//!   label or an undeclared edge label;
//! - **create** ([`Session::create_element`]): `{kind: "vertex", labels: [label], id?,
//!   properties: [[key, typed]], validateOnly?}` or `{kind: "edge", label, from, to, id?,
//!   properties, validateOnly?}`; the answer is `{ok: true, created, element, drawn}` (`created`:
//!   a commit was made, false for a dry run; `element`: the edit view of [`crate::edit`], `null`
//!   for a dry run; `drawn`: what the drawing needs, `{kind, id, label, caption, source?,
//!   target?}`, `caption` the vertex's `name` caption as the graph view has it) or `{ok: false,
//!   errors: [{field, message, help?}]}` with `field` a property key, `label`, `id`, `from`/`to`
//!   or `general`. Every property is tried, so all bad values are reported at once. The engine
//!   creates it as a query does (`GraphStorage::add_vertex_with`/`add_edge_with`), so the schema's
//!   `default`/`defaultFunction` fill the properties the form leaves out;
//! - **delete** ([`Session::delete_element`]): `{kind, id, validateOnly?}`; the answer is `{ok:
//!   true, deleted, edges}` (`edges`: the edges removed with a vertex; a dry run counts them and
//!   deletes nothing, `deleted: false`) or `{ok: false, errors}`.
//!
//! A dry run of a creation never consumes an automatic id: it creates the element under a
//! temporary non-numeric id (the id sequences move only for numeric ids), and the unit is
//! always rolled back.

use crate::error::SessionError;
use std::collections::HashSet;

use graphersal::prelude::*;
use graphersal::prelude::{CompactString, PropertyMap};
use graphersal::schema::SchemaMode;
use graphersal::storage::{CreateContext, ElementKind};
use serde_json::{Value, json};

use crate::Session;
use crate::display::caption_text;
use crate::edit::{field_error, key_and_value, kind_of, refused, value_from_typed};
use crate::queries::FieldError;

/// The most properties one creation may carry.
pub const MAX_CREATE_KEYS: usize = 10_000;

/// The most labels a choice list holds (the largest by count are kept).
pub const MAX_LABEL_CHOICES: usize = 500;

/// The id a dry run creates its element under (with a suffix when the graph has it): not a
/// number, so the automatic id sequences do not move.
const DRY_RUN_ID: &str = "graphersal-dry-run";

/// The text of a request field that names an element id (a string, or a number as text).
fn id_text(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// The field a failed write belongs to: `label`, `id`, or the requested property key whose
/// value (or a value nested in it) failed; `None` for a failure of the whole request.
fn field_of(err: &GraphError, keys: &PropertyMap) -> Option<String> {
    let property = match err {
        GraphError::VertexIdUniqueConstraint { .. } | GraphError::EdgeIdUniqueConstraint { .. } => {
            return Some("id".to_string());
        }
        GraphError::InvalidLabel { .. } => return Some("label".to_string()),
        GraphError::InvalidPropertyValue { property_name, .. } => property_name,
        GraphError::Schema(schema) => match schema {
            SchemaError::UnknownVertexLabel { .. }
            | SchemaError::UnknownEdgeLabel { .. }
            | SchemaError::InvalidEdgeTopology { .. } => return Some("label".to_string()),
            SchemaError::UndeclaredProperty { property_name, .. }
            | SchemaError::MissingRequiredProperty { property_name, .. }
            | SchemaError::TypeMismatch { property_name, .. }
            | SchemaError::CoercionFailed { property_name, .. }
            | SchemaError::ConstraintViolation { property_name, .. } => property_name,
            _ => return None,
        },
        _ => return None,
    };
    // A nested failure names its path (`address.city`, `tags[2]`): the requested key it starts
    // with is the field; the longest one wins (keys may contain dots).
    keys.keys()
        .filter(|key| {
            property == key.as_str()
                || property
                    .strip_prefix(key.as_str())
                    .is_some_and(|rest| rest.starts_with('.') || rest.starts_with('['))
        })
        .max_by_key(|key| key.len())
        .map(|key| key.to_string())
        .or_else(|| Some(property.to_string()))
}

/// The properties of a creation request; field errors for malformed values.
fn parse_properties(request: &Value) -> Result<PropertyMap, Vec<FieldError>> {
    let pairs = match request.get("properties") {
        None | Some(Value::Null) => &[][..],
        Some(Value::Array(pairs)) => pairs.as_slice(),
        Some(_) => {
            return Err(vec![FieldError::new(
                "general",
                "\"properties\" must be a list of [key, typed value] pairs",
            )]);
        }
    };
    if pairs.len() > MAX_CREATE_KEYS {
        return Err(vec![FieldError::new(
            "general",
            format!("at most {MAX_CREATE_KEYS} properties can be set at once"),
        )]);
    }
    let mut properties = PropertyMap::default();
    let mut errors = Vec::new();
    for pair in pairs {
        match key_and_value(pair) {
            Ok(("", _)) => errors.push(FieldError::new(
                "general",
                "a property name cannot be empty",
            )),
            Ok((key, _)) if properties.contains_key(key) => {
                errors.push(FieldError::new(key, format!("\"{key}\" is set twice")));
            }
            Ok((key, typed)) => match value_from_typed(typed) {
                Ok(value) => {
                    properties.insert(CompactString::from(key), value);
                }
                Err(message) => errors.push(FieldError::new(key, message)),
            },
            Err(message) => errors.push(FieldError::new("general", message)),
        }
    }
    if errors.is_empty() {
        Ok(properties)
    } else {
        Err(errors)
    }
}

/// An id no element of `kind` has, for a dry run.
fn dry_run_id<S: GraphStorage>(g: &S, kind: ElementKind) -> CompactString {
    let taken = |id: &str| match kind {
        ElementKind::Vertex => g.get_vertex_by_id(id).is_some(),
        ElementKind::Edge => g.get_edge_by_id(id).is_some(),
    };
    let mut id = DRY_RUN_ID.to_string();
    let mut n = 1u32;
    while taken(&id) {
        n = n.saturating_add(1);
        id = format!("{DRY_RUN_ID}-{n}");
    }
    CompactString::from(id)
}

/// What a creation writes.
enum Create {
    Vertex {
        id: Option<CompactString>,
        labels: Vec<CompactString>,
    },
    Edge {
        id: Option<CompactString>,
        label: CompactString,
        from: String,
        to: String,
    },
}

/// Adds the element, trying again without each property that fails, so every bad value is
/// reported at once. `Ok(id)` only when nothing failed; the caller's unit rolls back otherwise.
fn add<S: GraphStorage>(
    g: &mut S,
    what: &Create,
    mut properties: PropertyMap,
    dry_run: bool,
) -> Result<String, Vec<FieldError>> {
    let mut errors: Vec<FieldError> = Vec::new();
    // `defaultFunction` `now`/`today`: the start of the caller's unit, as for a query.
    let create = CreateContext::for_storage(&*g);
    // At most one attempt per property, plus the last one.
    for _ in 0..=properties.len() {
        let attempt = match what {
            Create::Vertex { id, labels } => {
                let id = match (id, dry_run) {
                    (Some(id), _) => Some(id.clone()),
                    (None, true) => Some(dry_run_id(g, ElementKind::Vertex)),
                    (None, false) => None,
                };
                g.add_vertex_with(id, labels.clone(), properties.clone(), &create)
                    .map(|v| g.vertex_id(v).unwrap_or_default().to_string())
            }
            Create::Edge {
                id,
                label,
                from,
                to,
            } => {
                let Some(source) = g.get_vertex_by_id(from) else {
                    return Err(vec![gone_endpoint("from", from)]);
                };
                let Some(target) = g.get_vertex_by_id(to) else {
                    return Err(vec![gone_endpoint("to", to)]);
                };
                let id = match (id, dry_run) {
                    (Some(id), _) => Some(id.clone()),
                    (None, true) => Some(dry_run_id(g, ElementKind::Edge)),
                    (None, false) => None,
                };
                g.add_edge_with(
                    id,
                    Some(label.clone()),
                    source,
                    target,
                    properties.clone(),
                    &create,
                )
                .map(|e| g.edge_id(e).unwrap_or_default().to_string())
            }
        };
        let err = match attempt {
            Ok(id) if errors.is_empty() => return Ok(id),
            Ok(_) => return Err(errors),
            Err(err) => err,
        };
        let field = field_of(&err, &properties);
        if field
            .as_deref()
            .is_some_and(|f| errors.iter().any(|e| e.field == f))
        {
            // A key already reported (a required one whose value was bad): nothing new.
            return Err(errors);
        }
        errors.push(field_error(field.as_deref().unwrap_or("general"), &err));
        match field {
            Some(key) if properties.shift_remove(key.as_str()).is_some() => continue,
            _ => return Err(errors),
        }
    }
    Err(errors)
}

fn gone_endpoint(field: &str, id: &str) -> FieldError {
    FieldError::new(
        field,
        format!(
            "the vertex {id} no longer exists (dropped since it was drawn): reload the drawing"
        ),
    )
}

/// One label choice.
fn choice(label: &str, declared: bool, count: usize, connects: Option<bool>) -> Value {
    json!({"label": label, "declared": declared, "count": count, "connects": connects})
}

/// The labels the creation dialog offers in `g` (module docs) for `request` (`{kind:
/// "vertex"}` or `{kind: "edge", from, to}`), without `readOnly` (the session's to add). An
/// unknown kind or a missing endpoint is an error.
pub fn label_choices_in<S: GraphStorage>(g: &S, request: &Value) -> Result<Value, SessionError> {
    let kind = kind_of(request.get("kind").and_then(Value::as_str).unwrap_or(""))?;
    let schema = g.get_schema()?.unwrap_or_default();
    let closed = schema.mode == SchemaMode::Closed;
    let stats = g.statistics()?;
    let mut counts: Vec<(String, usize)> = match (kind, stats) {
        (ElementKind::Vertex, Some(s)) => s
            .vertex_label_counts()
            .into_iter()
            .map(|(l, c)| (l.to_string(), c))
            .collect(),
        (ElementKind::Edge, Some(s)) => s
            .edge_label_counts()
            .into_iter()
            .map(|(l, c)| (l.to_string(), c))
            .collect(),
        (_, None) => Vec::new(),
    };
    counts.retain(|(l, _)| !l.is_empty());
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let count_of = |label: &str| {
        counts
            .iter()
            .find(|(l, _)| l == label)
            .map_or(0, |(_, c)| *c)
    };
    let mut labels = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    match kind {
        ElementKind::Vertex => {
            for label in schema.vertices.keys() {
                if seen.insert(label.clone()) {
                    labels.push(choice(label, true, count_of(label), None));
                }
            }
            if !closed {
                for (label, count) in &counts {
                    if seen.insert(label.clone()) {
                        labels.push(choice(label, false, *count, None));
                    }
                }
            }
        }
        ElementKind::Edge => {
            let endpoint = |field: &str| -> Result<Vec<String>, SessionError> {
                let id = id_text(request.get(field))
                    .ok_or_else(|| format!("an edge's label choices need \"{field}\""))?;
                let v = g
                    .get_vertex_by_id(&id)
                    .ok_or_else(|| format!("the vertex {id} does not exist"))?;
                Ok(g.vertex_labels(v).iter().map(|l| l.to_string()).collect())
            };
            let (from, to) = (endpoint("from")?, endpoint("to")?);
            let matches = |declared: &str, labels: &[String]| {
                declared == "*" || labels.iter().any(|l| l == declared)
            };
            let mut elsewhere = Vec::new();
            for (label, edge) in &schema.edges {
                let connects = edge.connections.is_empty()
                    || edge
                        .connections
                        .iter()
                        .any(|c| matches(&c.from, &from) && matches(&c.to, &to));
                if !seen.insert(label.clone()) {
                    continue;
                }
                if connects {
                    labels.push(choice(label, true, count_of(label), Some(true)));
                } else {
                    elsewhere.push(label.clone());
                }
            }
            if !closed {
                for (label, count) in &counts {
                    if seen.insert(label.clone()) {
                        labels.push(choice(label, false, *count, None));
                    }
                }
                for label in elsewhere {
                    labels.push(choice(&label, true, count_of(&label), Some(false)));
                }
            }
        }
    }
    labels.truncate(MAX_LABEL_CHOICES);
    Ok(json!({
        "kind": kind.as_str(),
        "mode": schema.mode.as_str(),
        "free": !closed,
        "labels": labels,
    }))
}

/// Creates the element `what` with `properties` in `g` as ONE unit (`validate_only`: a dry
/// run, always rolled back, no automatic id used): `Ok(Some(id))` of the stored element,
/// `Ok(None)` for a dry run that would succeed, or the field errors (nothing stored).
fn create_in<S: GraphStorage>(
    g: &mut S,
    what: &Create,
    properties: PropertyMap,
    validate_only: bool,
) -> Result<Option<String>, Vec<FieldError>> {
    if validate_only {
        return match g.dry_run(|g| Ok::<_, GraphError>(add(g, what, properties, true))) {
            Ok((Ok(_), _)) => Ok(None),
            Ok((Err(errors), _)) => Err(errors),
            Err(err) => Err(vec![field_error("general", &err)]),
        };
    }
    match g.transaction(|g| add(g, what, properties, false).map_err(CreateFailed::Fields)) {
        Ok(id) => Ok(Some(id)),
        Err(CreateFailed::Fields(errors)) => Err(errors),
        Err(CreateFailed::Graph(err)) => Err(vec![field_error("general", &err)]),
    }
}

/// `{kind, id, label, caption, source?, target?}` of the element `id` of `kind` in `g`, as the
/// graph view draws it; `null` when there is none.
pub fn drawn_in<S: GraphStorage>(g: &S, kind: ElementKind, id: &str) -> Value {
    match kind {
        ElementKind::Vertex => {
            let Some(v) = g.get_vertex_by_id(id) else {
                return Value::Null;
            };
            json!({
                "kind": "vertex",
                "id": id,
                "label": g.vertex_labels(v).first().map_or("", |l| l.as_str()),
                "caption": g.vertex_properties(v)
                    .and_then(|p| p.get("name"))
                    .and_then(|value| caption_text(value.logical().as_ref())),
            })
        }
        ElementKind::Edge => {
            let Some(e) = g.get_edge_by_id(id) else {
                return Value::Null;
            };
            let (source, target) = g
                .edge_endpoints(e)
                .map(|(s, t)| {
                    (
                        g.vertex_id(s).unwrap_or_default().to_string(),
                        g.vertex_id(t).unwrap_or_default().to_string(),
                    )
                })
                .unwrap_or_default();
            json!({
                "kind": "edge",
                "id": id,
                "label": g.edge_label(e).unwrap_or_default(),
                "caption": null,
                "source": source,
                "target": target,
            })
        }
    }
}

/// How many edges deleting the element `id` of `kind` in `g` takes with it (a vertex's
/// incident edges, a loop once; 0 for an edge); `None` when there is no such element.
pub fn incident_edges<S: GraphStorage>(g: &S, kind: ElementKind, id: &str) -> Option<usize> {
    match kind {
        ElementKind::Vertex => {
            let v = g.get_vertex_by_id(id)?;
            // A loop is one edge, met from both ends.
            let mut ids: HashSet<String> = HashSet::new();
            for e in g.out_edges(v).chain(g.in_edges(v)) {
                ids.insert(g.edge_id(e).unwrap_or_default().to_string());
            }
            Some(ids.len())
        }
        ElementKind::Edge => g.get_edge_by_id(id).map(|_| 0),
    }
}

/// Deletes the element `id` of `kind` (a vertex with its edges) from `g` as ONE unit
/// (`validate_only`: a dry run, always rolled back): `Ok(deleted)`, whether a commit happened
/// (always `false` for a dry run).
pub fn delete_in<S: GraphStorage>(
    g: &mut S,
    kind: ElementKind,
    id: &str,
    validate_only: bool,
) -> Result<bool, GraphError> {
    let drop = |g: &mut S| -> Result<(), GraphError> {
        match kind {
            ElementKind::Vertex => match g.get_vertex_by_id(id) {
                Some(v) => g.drop_vertex(v),
                None => Ok(()),
            },
            ElementKind::Edge => match g.get_edge_by_id(id) {
                Some(e) => g.drop_edge(e),
                None => Ok(()),
            },
        }
    };
    if validate_only {
        return g.dry_run(drop).map(|_| false);
    }
    let before = g.last_commit_seq()?;
    g.transaction(drop)?;
    // The commit happened: a position that cannot be read afterwards counts as a change.
    Ok(g.last_commit_seq().map_or(true, |after| after != before))
}

/// The element a creation request of `kind` describes (module docs) and its properties; the
/// field errors of a malformed request.
fn parse_create(
    kind: ElementKind,
    request: &Value,
) -> Result<(Create, PropertyMap), Vec<FieldError>> {
    let id = match request.get("id") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if s.trim().is_empty() => None,
        other => match id_text(other) {
            Some(id) => Some(CompactString::from(id)),
            None => return Err(vec![FieldError::new("id", "an id is text")]),
        },
    };
    let what = match kind {
        ElementKind::Vertex => {
            let labels = match request.get("labels") {
                None | Some(Value::Null) => Vec::new(),
                Some(Value::Array(labels)) => {
                    let mut out = Vec::new();
                    for label in labels {
                        match label.as_str() {
                            Some("") => {
                                return Err(vec![FieldError::new(
                                    "label",
                                    "a label cannot be empty",
                                )]);
                            }
                            Some(l) => out.push(CompactString::from(l)),
                            None => {
                                return Err(vec![FieldError::new("label", "a label is text")]);
                            }
                        }
                    }
                    out
                }
                Some(_) => {
                    return Err(vec![FieldError::new(
                        "label",
                        "\"labels\" must be a list of labels",
                    )]);
                }
            };
            Create::Vertex { id, labels }
        }
        ElementKind::Edge => {
            let label = match request.get("label").and_then(Value::as_str) {
                Some(l) if !l.is_empty() => CompactString::from(l),
                _ => return Err(vec![FieldError::new("label", "an edge needs a label")]),
            };
            let (Some(from), Some(to)) = (id_text(request.get("from")), id_text(request.get("to")))
            else {
                return Err(vec![FieldError::new(
                    "general",
                    "an edge needs \"from\" and \"to\" vertex ids",
                )]);
            };
            Create::Edge {
                id,
                label,
                from,
                to,
            }
        }
    };
    let properties = parse_properties(request)?;
    Ok((what, properties))
}

/// [`create_in`] of a creation request of `kind` (module docs): the request parsed first (a
/// malformed one is answered with its field errors, nothing is written).
pub(crate) fn create_request_in<S: GraphStorage>(
    g: &mut S,
    kind: ElementKind,
    request: &Value,
    validate_only: bool,
) -> Result<Option<String>, Vec<FieldError>> {
    let (what, properties) = parse_create(kind, request)?;
    create_in(g, &what, properties, validate_only)
}

impl Session {
    /// The labels the creation dialog offers (module docs): `{kind: "vertex"}` or `{kind:
    /// "edge", from, to}`. An unknown kind or a missing endpoint is an error.
    pub fn label_choices(&self, request: &Value) -> Result<Value, SessionError> {
        let mut choices = self.graph.label_choices(request)?;
        if let Value::Object(map) = &mut choices {
            map.insert("readOnly".into(), json!(self.read_only_reason()?));
        }
        Ok(choices)
    }

    /// Creates a vertex or an edge as ONE unit (module docs); `validateOnly: true` runs the same
    /// write as a dry run. Never fails otherwise: every problem is a field error.
    pub fn create_element(&mut self, request: &Value) -> Value {
        let kind = match kind_of(request.get("kind").and_then(Value::as_str).unwrap_or("")) {
            Ok(kind) => kind,
            Err(message) => return refused(vec![FieldError::new("general", message)]),
        };
        let validate_only = request.get("validateOnly").and_then(Value::as_bool) == Some(true);
        if let Some(reason) = self.write_refusal() {
            return refused(vec![FieldError::new("general", reason)]);
        }
        if validate_only {
            return match self.graph.create_element(kind, request, true) {
                Ok(_) => json!({"ok": true, "created": false, "element": null, "drawn": null}),
                Err(errors) => refused(errors),
            };
        }
        let stored = self.graph.create_element(kind, request, false);
        // Without a checkpoint thread (wasm32) the automatic checkpoint runs here.
        self.run_due_checkpoint();
        match stored {
            Ok(id) => {
                let id = id.unwrap_or_default();
                // The element is stored; a failure to read it back is said beside the answer
                // (`readError`), never an element of `null` without a reason.
                match (
                    self.element_for_edit(kind.as_str(), &id),
                    self.drawn(kind, &id),
                ) {
                    (Ok(element), Ok(drawn)) => json!({
                        "ok": true,
                        "created": true,
                        "element": element,
                        "drawn": drawn,
                    }),
                    (Err(err), _) | (_, Err(err)) => json!({
                        "ok": true,
                        "created": true,
                        "element": null,
                        "drawn": null,
                        "readError": err.to_string(),
                    }),
                }
            }
            Err(errors) => refused(errors),
        }
    }

    /// `{kind, id, label, caption, source?, target?}` of an element, as the graph view draws it.
    fn drawn(&self, kind: ElementKind, id: &str) -> Result<Value, SessionError> {
        Ok(self.graph.drawn(kind, id)?)
    }

    /// Deletes a vertex (with its edges) or an edge as ONE unit (module docs); `validateOnly:
    /// true` counts what would go and deletes nothing.
    pub fn delete_element(&mut self, request: &Value) -> Value {
        let kind = match kind_of(request.get("kind").and_then(Value::as_str).unwrap_or("")) {
            Ok(kind) => kind,
            Err(message) => return refused(vec![FieldError::new("general", message)]),
        };
        let Some(id) = id_text(request.get("id")) else {
            return refused(vec![FieldError::new(
                "general",
                "the request names no element id",
            )]);
        };
        let validate_only = request.get("validateOnly").and_then(Value::as_bool) == Some(true);
        if let Some(reason) = self.write_refusal() {
            return refused(vec![FieldError::new("general", reason)]);
        }
        let gone = || {
            refused(vec![FieldError::new(
                "general",
                format!(
                    "this {} no longer exists (dropped since it was drawn): reload the drawing",
                    kind.as_str()
                ),
            )])
        };
        let edges = match self.graph.incident_edges(kind, &id) {
            Ok(Some(edges)) => edges,
            Ok(None) => return gone(),
            Err(err) => return refused(vec![FieldError::new("general", err.to_string())]),
        };
        if validate_only {
            return match self.graph.delete_element(kind, &id, true) {
                Ok(_) => json!({"ok": true, "deleted": false, "edges": edges}),
                Err(err) => refused(vec![field_error("general", &err)]),
            };
        }
        let stored = self.graph.delete_element(kind, &id, false);
        self.run_due_checkpoint();
        match stored {
            Ok(deleted) => json!({"ok": true, "deleted": deleted, "edges": edges}),
            Err(err) => refused(vec![field_error("general", &err)]),
        }
    }
}

/// Why a creation did not commit.
enum CreateFailed {
    /// Failures of single fields (and general ones), shown in the dialog.
    Fields(Vec<FieldError>),
    /// The unit failed as a whole (a vetoed commit: a read-only view, a damaged store).
    Graph(GraphError),
}

impl From<GraphError> for CreateFailed {
    fn from(err: GraphError) -> Self {
        CreateFailed::Graph(err)
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

    fn fields(answer: &Value) -> Vec<String> {
        answer["errors"]
            .as_array()
            .map(|errors| {
                errors
                    .iter()
                    .map(|e| e["field"].as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn labels_of(answer: &Value) -> Vec<String> {
        answer["labels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["label"].as_str().unwrap().to_string())
            .collect()
    }

    const SCHEMA: &str = r#"{"mode": "closed",
        "vertices": {
            "person": {"schema": {"type": "object",
                "properties": {"name": {"type": "string"}, "age": {"type": "integer", "minimum": 0}},
                "required": ["name"]}},
            "software": {"schema": {"type": "object",
                "properties": {"name": {"type": "string"}, "lang": {"type": "string"}}}}},
        "edges": {
            "knows": {"connections": [{"from": "person", "to": "person"}],
                "schema": {"type": "object", "properties": {"weight": {"type": "number"}}}},
            "created": {"connections": [{"from": "person", "to": "software"}],
                "schema": {"type": "object", "properties": {"weight": {"type": "number"}}}}}}"#;

    #[test]
    fn creates_a_vertex_in_one_commit_and_a_dry_run_uses_no_id() {
        let mut s = modern();
        let before = s.graph().last_commit_seq().unwrap();
        let request = json!({
            "kind": "vertex", "labels": ["person"], "validateOnly": true,
            "properties": [["name", {"type": "string", "value": "ann"}],
                           ["age", {"type": "int64", "value": "41"}]],
        });
        for _ in 0..3 {
            let checked = s.create_element(&request);
            assert_eq!(checked["ok"], true, "{checked}");
            assert_eq!(checked["created"], false);
        }
        assert_eq!(s.graph().last_commit_seq().unwrap(), before);
        let mut real = request.clone();
        real["validateOnly"] = json!(false);
        let answer = s.create_element(&real);
        assert_eq!(answer["ok"], true, "{answer}");
        assert_eq!(answer["created"], true);
        assert_eq!(s.graph().last_commit_seq().unwrap(), before + 1);
        // The dry runs consumed no automatic id: the modern graph's next vertex id is 7.
        assert_eq!(answer["drawn"]["id"], "7", "{answer}");
        assert_eq!(answer["drawn"]["caption"], "ann");
        assert_eq!(answer["drawn"]["label"], "person");
        assert_eq!(answer["element"]["properties"][1][1]["type"], "int64");
        assert_eq!(s.graph().vertex_count().unwrap(), 7);
    }

    #[test]
    fn creates_an_edge_between_two_vertices() {
        let mut s = modern();
        let answer = s.create_element(&json!({
            "kind": "edge", "label": "knows", "from": "1", "to": "6",
            "properties": [["weight", {"type": "float64", "value": 0.3}]],
        }));
        assert_eq!(answer["ok"], true, "{answer}");
        assert_eq!(answer["drawn"]["source"], "1");
        assert_eq!(answer["drawn"]["target"], "6");
        assert_eq!(answer["drawn"]["label"], "knows");
        let gone =
            s.create_element(&json!({"kind": "edge", "label": "x", "from": "1", "to": "99"}));
        assert_eq!(fields(&gone), vec!["to"], "{gone}");
        let unlabeled = s.create_element(&json!({"kind": "edge", "from": "1", "to": "2"}));
        assert_eq!(fields(&unlabeled), vec!["label"]);
    }

    #[test]
    fn schema_errors_are_reported_at_their_fields_and_nothing_is_written() {
        let mut s = modern();
        s.set_schema(SCHEMA, None).expect("schema");
        let before = s.graph().last_commit_seq().unwrap();
        for validate_only in [true, false] {
            // Two bad values and an undeclared key: all three reported.
            let answer = s.create_element(&json!({
                "kind": "vertex", "labels": ["person"], "validateOnly": validate_only,
                "properties": [["name", {"type": "int64", "value": "1"}],
                               ["age", {"type": "int64", "value": "-1"}],
                               ["nick", {"type": "string", "value": "x"}]],
            }));
            assert_eq!(answer["ok"], false, "{answer}");
            let f = fields(&answer);
            assert!(f.contains(&"age".to_string()), "{answer}");
            assert!(f.contains(&"nick".to_string()), "{answer}");
            assert!(f.contains(&"name".to_string()), "{answer}");
            // A missing required key and an undeclared label.
            let missing = s.create_element(&json!({
                "kind": "vertex", "labels": ["person"], "validateOnly": validate_only,
            }));
            assert_eq!(fields(&missing), vec!["name"], "{missing}");
            let label = s.create_element(&json!({
                "kind": "vertex", "labels": ["robot"], "validateOnly": validate_only,
            }));
            assert_eq!(fields(&label), vec!["label"], "{label}");
            // Topology: created goes person -> software only.
            let topology = s.create_element(&json!({
                "kind": "edge", "label": "created", "from": "3", "to": "1",
                "validateOnly": validate_only,
            }));
            assert_eq!(fields(&topology), vec!["label"], "{topology}");
            let id = s.create_element(&json!({
                "kind": "vertex", "labels": ["person"], "id": "1", "validateOnly": validate_only,
                "properties": [["name", {"type": "string", "value": "x"}]],
            }));
            assert_eq!(fields(&id), vec!["id"], "{id}");
        }
        assert_eq!(s.graph().last_commit_seq().unwrap(), before);
        assert_eq!(s.graph().vertex_count().unwrap(), 6);
    }

    #[test]
    fn label_choices_follow_the_schema_and_the_graph() {
        let mut s = modern();
        let v = s.label_choices(&json!({"kind": "vertex"})).unwrap();
        assert_eq!(v["free"], true);
        assert_eq!(labels_of(&v), vec!["person", "software"]);
        assert_eq!(v["labels"][0]["count"], 4);
        let e = s
            .label_choices(&json!({"kind": "edge", "from": "1", "to": "2"}))
            .unwrap();
        assert_eq!(labels_of(&e), vec!["created", "knows"]);

        s.set_schema(SCHEMA, None).expect("schema");
        let closed = s
            .label_choices(&json!({"kind": "edge", "from": "1", "to": "3"}))
            .unwrap();
        assert_eq!(closed["mode"], "closed");
        assert_eq!(closed["free"], false);
        assert_eq!(labels_of(&closed), vec!["created"]);
        assert_eq!(closed["labels"][0]["connects"], true);
        let back = s
            .label_choices(&json!({"kind": "edge", "from": "3", "to": "1"}))
            .unwrap();
        assert!(labels_of(&back).is_empty(), "{back}");

        // Open: topology is not enforced, so the other declared labels come last.
        let open = SCHEMA.replace("\"closed\"", "\"open\"");
        s.set_schema(&open, None).expect("schema");
        let open = s
            .label_choices(&json!({"kind": "edge", "from": "1", "to": "3"}))
            .unwrap();
        assert_eq!(labels_of(&open), vec!["created", "knows"]);
        assert_eq!(open["labels"][1]["connects"], false);
        assert!(
            s.label_choices(&json!({"kind": "edge", "from": "1"}))
                .is_err()
        );
        assert!(s.label_choices(&json!({"kind": "x"})).is_err());
    }

    #[test]
    fn deletes_a_vertex_with_its_edges_in_one_commit() {
        let mut s = modern();
        let before = s.graph().last_commit_seq().unwrap();
        let preview = s.delete_element(&json!({"kind": "vertex", "id": "1", "validateOnly": true}));
        assert_eq!(preview["ok"], true, "{preview}");
        assert_eq!(preview["edges"], 3);
        assert_eq!(preview["deleted"], false);
        assert_eq!(s.graph().vertex_count().unwrap(), 6);
        let answer = s.delete_element(&json!({"kind": "vertex", "id": "1"}));
        assert_eq!(answer["deleted"], true, "{answer}");
        assert_eq!(s.graph().last_commit_seq().unwrap(), before + 1);
        assert_eq!(s.graph().vertex_count().unwrap(), 5);
        assert_eq!(s.graph().edge_count().unwrap(), 3);
        let left = s.graph().graph_view(10, 10).unwrap().edge_ids[0].clone();
        let edge = s.delete_element(&json!({"kind": "edge", "id": left}));
        assert_eq!(edge["deleted"], true, "{edge}");
        let gone = s.delete_element(&json!({"kind": "vertex", "id": "1"}));
        assert_eq!(gone["ok"], false);
    }

    #[test]
    fn a_read_only_view_refuses_creating_and_deleting() {
        let mut s = Session::new();
        s.set_memory_stores(true);
        s.load_sample("modern").expect("modern");
        s.execute(
            "g.v(\"1\").property(\"age\", 30).iterate()",
            &Default::default(),
        );
        s.store_open_view(&json!({"target": {"commit": 0}}))
            .expect("view");
        let created = s.create_element(&json!({"kind": "vertex", "labels": ["person"]}));
        assert_eq!(created["ok"], false);
        assert!(
            created["errors"][0]["message"]
                .as_str()
                .unwrap()
                .contains("read-only view")
        );
        let deleted = s.delete_element(&json!({"kind": "vertex", "id": "1", "validateOnly": true}));
        assert_eq!(deleted["ok"], false);
        let choices = s.label_choices(&json!({"kind": "vertex"})).unwrap();
        assert!(choices["readOnly"].is_string());
        s.store_close_view().expect("live");
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
    fn creation_and_deletion_on_a_foreign_storage_equal_those_of_a_traversal_graph() {
        let (mut tg, mut fw) = modern_pair();
        for request in [
            json!({"kind": "vertex"}),
            json!({"kind": "edge", "from": "1", "to": "3"}),
        ] {
            assert_eq!(
                label_choices_in(&tg, &request).unwrap(),
                label_choices_in(&fw, &request).unwrap()
            );
        }
        let what = Create::Edge {
            id: None,
            label: "knows".into(),
            from: "1".into(),
            to: "6".into(),
        };
        let mut properties = PropertyMap::default();
        properties.insert("weight", 0.5f64.into());
        for validate_only in [true, false] {
            let a = create_in(&mut tg, &what, properties.clone(), validate_only).ok();
            let b = create_in(&mut fw, &what, properties.clone(), validate_only).ok();
            assert_eq!(a, b);
            assert_eq!(a.as_ref().map(Option::is_some), Some(!validate_only));
        }
        let id = tg
            .edges()
            .last()
            .and_then(|e| tg.edge_id(e))
            .unwrap()
            .to_string();
        assert_eq!(
            drawn_in(&tg, ElementKind::Edge, &id),
            drawn_in(&fw, ElementKind::Edge, &id)
        );
        assert_eq!(
            drawn_in(&tg, ElementKind::Vertex, "1"),
            drawn_in(&fw, ElementKind::Vertex, "1")
        );
        assert_eq!(incident_edges(&fw, ElementKind::Vertex, "1"), Some(4));
        for validate_only in [true, false] {
            let a = delete_in(&mut tg, ElementKind::Vertex, "1", validate_only).ok();
            let b = delete_in(&mut fw, ElementKind::Vertex, "1", validate_only).ok();
            assert_eq!(a, Some(!validate_only));
            assert_eq!(b, Some(!validate_only));
        }
        assert_eq!((fw.vertex_count(), fw.edge_count()), (5, 3));
        assert_eq!(incident_edges(&fw, ElementKind::Vertex, "1"), None);
    }
}
