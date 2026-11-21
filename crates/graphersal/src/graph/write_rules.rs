//! The write rules of [`TraversalGraph`]'s write methods, in the same order, as free functions
//! over a small read view ([`WriteView`]): a storage that keeps its elements elsewhere (the
//! layered graph, `graph/layered/`) applies exactly the reference semantics without touching
//! `TraversalGraph`'s internals.
//!
//! Each function takes the write's arguments, reads what it needs through the view (the schema,
//! the element's labels and properties, incident edges) and returns what the storage stores
//! (normalized labels, coerced values, the whole resulting property of a path write) or the same
//! [`GraphError`] `TraversalGraph` returns for the same write on the same data. Nothing is
//! mutated: the caller stores the result, keeps its own indexes and undo log, and assigns ids.
//!
//! `TraversalGraph` does not call these functions (its write methods are the hot path and stay as
//! they are, layered-graph plan P3); the unit tests below hold the two in step by running every
//! rule against a `TraversalGraph` and comparing its answer with the real write.
//!
//! # Not covered here (the caller's part)
//!
//! Three checks of `TraversalGraph`'s write methods are tied to state a read view does not carry,
//! and are left out on purpose. A layered graph refuses a base that needs them until stage 4
//! (plan P5), so the delta never meets them:
//!
//! - the vector (derived) rules, `check_derived_*`: in `TraversalGraph` right after the
//!   property-value check of every property write;
//! - the compression rules, `compression_prepare_*` / `compression_normalize_*`: after the
//!   schema validation (delta values are stored uncompressed);
//! - unique property indexes, `property_indexes.unique_conflict`: after the compression step.
//!
//! Id uniqueness has the two check functions [`check_vertex_id_free`] and
//! [`check_edge_id_free`]; the auto-id sequences are the caller's (S1.3).

// The layered graph's write path (S1.3+) is the first caller outside the tests.
#![cfg_attr(not(test), allow(dead_code))]

use std::ops::Deref;

use compact_str::CompactString;

use crate::error::{GraphError, SchemaError};
use crate::graph::schema::defaults::{CreateContext, fill_defaults};
use crate::graph::schema::{GraphSchema, SchemaMode, SchemaValidator};
use crate::graph::write_checks::{
    check_label, check_properties, check_property_value, normalize_vertex_labels,
    path_property_name, remove_path, single_segment_name, write_path,
};
use crate::graph::{EdgeHandle, GraphNodeType, GraphStorage, TraversalGraph, VertexHandle};
use crate::value::json_path::JsonPathSegment;
use crate::value::{ElementProperty, JsonPath, PropertyMap};

/// What the write rules read from a storage: its schema, the state of one element, the edges
/// incident to a vertex and whether an id is taken. Every answer is about the storage as it is
/// before the write; `None` for an element means a removed element or a stale handle.
pub(crate) trait WriteView {
    /// The storage's vertex handle.
    type Vertex: Copy + Eq;
    /// The storage's edge handle.
    type Edge: Copy;

    /// The schema writes are checked against.
    fn schema(&self) -> impl Deref<Target = GraphSchema> + '_;

    /// A live vertex's label set (ordered, deduplicated, first = primary) and property map.
    fn vertex(&self, vertex: Self::Vertex) -> Option<(&[CompactString], &PropertyMap)>;

    /// A live edge: its label, properties and endpoints.
    fn edge(&self, edge: Self::Edge) -> Option<EdgeView<'_, Self::Vertex>>;

    /// The outgoing edges of a live vertex.
    fn out_edges(&self, vertex: Self::Vertex) -> impl Iterator<Item = Self::Edge> + '_;

    /// The incoming edges of a live vertex.
    fn in_edges(&self, vertex: Self::Vertex) -> impl Iterator<Item = Self::Edge> + '_;

    /// `true` when a live vertex has the id `id`.
    fn contains_vertex_id(&self, id: &str) -> bool;

    /// `true` when a live edge has the id `id`.
    fn contains_edge_id(&self, id: &str) -> bool;
}

/// One live edge as the write rules see it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct EdgeView<'a, V> {
    /// The label as a slice: one entry, or empty for an unlabeled edge (the form the schema
    /// validator takes for an edge's labels).
    pub(crate) label: &'a [CompactString],
    /// The property map; `None` when the edge never had one (an empty map in effect).
    pub(crate) properties: Option<&'a PropertyMap>,
    /// The source vertex.
    pub(crate) from: V,
    /// The target vertex.
    pub(crate) to: V,
}

/// The new label set of a label write, `None` when the write changes nothing (the label was
/// already there, or was not there to remove): `TraversalGraph` answers `Ok(false)` then.
pub(crate) type LabelChange = Option<Vec<CompactString>>;

/// What a path removal (`remove_*_property_path`) stores.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PathRemoval {
    /// Nothing to change: a missing path, an out-of-range index, a type mismatch on the way, or
    /// a one-segment path naming an absent property.
    Nothing,
    /// A one-segment path: drop this whole property (the checks of `drop_*_property` passed).
    Drop(CompactString),
    /// Store this whole top-level property in place of the old one.
    Store(CompactString, ElementProperty),
}

// ------------------------------------------------------------------------------------------
// Creates
// ------------------------------------------------------------------------------------------

/// The rules of `insert_vertex`: the label set to store (duplicates removed, each label checked)
/// and the properties coerced to and validated against the schema. The caller checks the id
/// afterwards ([`check_vertex_id_free`]), as `TraversalGraph` does.
pub(crate) fn insert_vertex<V: WriteView>(
    view: &V,
    labels: Vec<CompactString>,
    mut properties: PropertyMap,
) -> Result<(Vec<CompactString>, PropertyMap), GraphError> {
    let labels = normalize_vertex_labels(labels)?;
    check_properties(&properties)?;
    // (vector rules: `check_derived_map`, see the module docs)
    SchemaValidator::validate_vertex(&view.schema(), &labels, &mut properties)?;
    // (compression and unique property indexes, see the module docs)
    Ok((labels, properties))
}

/// The rules of `add_vertex_with`: the schema's create-time defaults filled in (from the labels
/// as given, before they are normalized, as `TraversalGraph` does), then [`insert_vertex`].
pub(crate) fn add_vertex_with<V: WriteView>(
    view: &V,
    labels: Vec<CompactString>,
    mut properties: PropertyMap,
    context: &CreateContext,
) -> Result<(Vec<CompactString>, PropertyMap), GraphError> {
    fill_defaults(
        &view.schema(),
        GraphNodeType::Vertex,
        &labels,
        &mut properties,
        context,
    );
    insert_vertex(view, labels, properties)
}

/// The rules of `insert_edge`: both endpoints live, the properties checked, then coerced to and
/// validated against the schema, with the closed-mode topology check (ANY label of an endpoint
/// satisfies a declared connection). Returns the properties to store. The caller checks the id
/// afterwards ([`check_edge_id_free`]).
pub(crate) fn insert_edge<V: WriteView>(
    view: &V,
    label: Option<&str>,
    from: V::Vertex,
    to: V::Vertex,
    mut properties: PropertyMap,
) -> Result<PropertyMap, GraphError> {
    let (Some((source_labels, _)), Some((target_labels, _))) = (view.vertex(from), view.vertex(to))
    else {
        return Err(GraphError::vertex_removed());
    };
    check_properties(&properties)?;
    // (vector rules: `check_derived_map`)
    SchemaValidator::validate_edge(
        &view.schema(),
        label,
        source_labels,
        target_labels,
        &mut properties,
    )?;
    // (compression and unique property indexes)
    Ok(properties)
}

/// The rules of `add_edge_with`: the schema's create-time defaults of `label` filled in, then
/// [`insert_edge`].
pub(crate) fn add_edge_with<V: WriteView>(
    view: &V,
    label: Option<&str>,
    from: V::Vertex,
    to: V::Vertex,
    mut properties: PropertyMap,
    context: &CreateContext,
) -> Result<PropertyMap, GraphError> {
    let label_slice: Vec<CompactString> = label.map(CompactString::from).into_iter().collect();
    fill_defaults(
        &view.schema(),
        GraphNodeType::Edge,
        &label_slice,
        &mut properties,
        context,
    );
    insert_edge(view, label, from, to, properties)
}

/// The id check of `insert_vertex` with an explicit id (after the schema check):
/// [`GraphError::VertexIdUniqueConstraint`] when a live vertex has it.
pub(crate) fn check_vertex_id_free<V: WriteView>(view: &V, id: &str) -> Result<(), GraphError> {
    if view.contains_vertex_id(id) {
        Err(GraphError::VertexIdUniqueConstraint { id: id.to_string() })
    } else {
        Ok(())
    }
}

/// The id check of `insert_edge` with an explicit id (after the schema check):
/// [`GraphError::EdgeIdUniqueConstraint`] when a live edge has it.
pub(crate) fn check_edge_id_free<V: WriteView>(view: &V, id: &str) -> Result<(), GraphError> {
    if view.contains_edge_id(id) {
        Err(GraphError::EdgeIdUniqueConstraint { id: id.to_string() })
    } else {
        Ok(())
    }
}

// ------------------------------------------------------------------------------------------
// Labels
// ------------------------------------------------------------------------------------------

/// The rules of `add_vertex_label`: the label checked (before the vertex is even looked up),
/// the vertex live, then the vertex as stored must be valid under the new label set and, in
/// closed mode, every incident edge must still match a declared connection. Returns the new
/// label set (the label appended), or `None` when the vertex already has it.
pub(crate) fn add_vertex_label<V: WriteView>(
    view: &V,
    vertex: V::Vertex,
    label: &str,
) -> Result<LabelChange, GraphError> {
    check_label(label)?;
    let Some((labels, properties)) = view.vertex(vertex) else {
        return Err(GraphError::vertex_removed());
    };
    if labels.iter().any(|l| l.as_str() == label) {
        return Ok(None);
    }
    let mut new_labels = labels.to_vec();
    new_labels.push(CompactString::from(label));
    SchemaValidator::check_vertex(&view.schema(), &new_labels, properties)?;
    check_incident_topology(view, vertex, &new_labels)?;
    // (unique property indexes of the new label)
    Ok(Some(new_labels))
}

/// The rules of `remove_vertex_label`: the vertex live, then (when it has the label) the vertex
/// as stored must be valid under the remaining labels (closed: not the last label, no property
/// left undeclared) and, in closed mode, every incident edge must still match. Returns the
/// remaining label set, or `None` when the vertex does not have the label.
pub(crate) fn remove_vertex_label<V: WriteView>(
    view: &V,
    vertex: V::Vertex,
    label: &str,
) -> Result<LabelChange, GraphError> {
    let Some((labels, properties)) = view.vertex(vertex) else {
        return Err(GraphError::vertex_removed());
    };
    if !labels.iter().any(|l| l.as_str() == label) {
        return Ok(None);
    }
    let new_labels: Vec<CompactString> = labels
        .iter()
        .filter(|l| l.as_str() != label)
        .cloned()
        .collect();
    SchemaValidator::check_vertex(&view.schema(), &new_labels, properties)?;
    check_incident_topology(view, vertex, &new_labels)?;
    Ok(Some(new_labels))
}

/// The rules of `set_edge_label`: the edge live; `false` (nothing to change) when it already has
/// `label`; otherwise, unless the schema mode is `None`, the edge as stored must be valid under
/// the new label (closed: a declared label and topology; open/closed: required keys and declared
/// types), with both endpoints live. Nothing is coerced. The label itself is not checked (edge
/// labels may contain `::`). `true`: store the new label.
pub(crate) fn set_edge_label<V: WriteView>(
    view: &V,
    edge: V::Edge,
    label: &str,
) -> Result<bool, GraphError> {
    let Some(current) = view.edge(edge) else {
        return Err(GraphError::edge_removed());
    };
    if current.label.first().map(CompactString::as_str) == Some(label) {
        return Ok(false);
    }
    let schema = view.schema();
    if schema.mode != SchemaMode::None {
        let (Some((source_labels, _)), Some((target_labels, _))) =
            (view.vertex(current.from), view.vertex(current.to))
        else {
            return Err(GraphError::vertex_removed());
        };
        let empty = PropertyMap::default();
        SchemaValidator::check_edge(
            &schema,
            Some(label),
            source_labels,
            target_labels,
            current.properties.unwrap_or(&empty),
        )?;
    }
    // (unique property indexes of the new label)
    Ok(true)
}

/// Closed mode: checks that every edge incident to `vertex` still matches a declared connection
/// if `vertex` had the label set `new_labels` (other endpoints keep their labels; a self-loop
/// sees `new_labels` at both ends). Other modes check nothing. The generic form of
/// `TraversalGraph::check_incident_topology`, which walks its own topology.
pub(crate) fn check_incident_topology<V: WriteView>(
    view: &V,
    vertex: V::Vertex,
    new_labels: &[CompactString],
) -> Result<(), SchemaError> {
    let schema = view.schema();
    if !matches!(schema.mode, SchemaMode::Closed) {
        return Ok(());
    }
    let labels_of = |other: V::Vertex| -> &[CompactString] {
        if other == vertex {
            new_labels
        } else {
            view.vertex(other).map(|(labels, _)| labels).unwrap_or(&[])
        }
    };
    for edge in view.out_edges(vertex).chain(view.in_edges(vertex)) {
        let Some(edge) = view.edge(edge) else {
            continue;
        };
        SchemaValidator::check_topology(
            &schema,
            edge.label.first().map(CompactString::as_str),
            labels_of(edge.from),
            labels_of(edge.to),
        )?;
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------
// Properties
// ------------------------------------------------------------------------------------------

/// The rules of `set_vertex_property`: the value checked (before the vertex is looked up), the
/// vertex live, then the value coerced (first declaring label, in label order) and validated
/// (every declaring label). Returns the value to store under `key`.
pub(crate) fn set_vertex_property<V: WriteView>(
    view: &V,
    vertex: V::Vertex,
    key: &str,
    value: ElementProperty,
) -> Result<ElementProperty, GraphError> {
    check_property_value(key, &value)?;
    // (vector rules: `check_derived_vertex`)
    let Some((labels, _)) = view.vertex(vertex) else {
        return Err(GraphError::vertex_removed());
    };
    let value =
        SchemaValidator::validate_element_property(&view.schema(), "vertex", labels, key, value)?;
    // (compression and unique property indexes)
    Ok(value)
}

/// The rules of `set_edge_property`; see [`set_vertex_property`].
pub(crate) fn set_edge_property<V: WriteView>(
    view: &V,
    edge: V::Edge,
    key: &str,
    value: ElementProperty,
) -> Result<ElementProperty, GraphError> {
    check_property_value(key, &value)?;
    // (vector rules: `check_derived_edge`)
    let Some(current) = view.edge(edge) else {
        return Err(GraphError::edge_removed());
    };
    let value = SchemaValidator::validate_element_property(
        &view.schema(),
        "edge",
        current.label,
        key,
        value,
    )?;
    Ok(value)
}

/// The rules of `set_vertex_property_path`: the path's property name, the value checked, the
/// vertex live, then the leaf coerced at the type the schema declares at that nested location,
/// written into a copy of the stored property, and the complete resulting property validated
/// (siblings are not re-coerced). Returns the property name and the whole value to store.
///
/// In schema mode `None` `TraversalGraph` writes in place instead; coercion and validation are
/// no-ops there, so the stored result and the faults are the same.
pub(crate) fn set_vertex_property_path<V: WriteView>(
    view: &V,
    vertex: V::Vertex,
    path: &JsonPath,
    value: ElementProperty,
) -> Result<(CompactString, ElementProperty), GraphError> {
    let name = path_property_name(path)?;
    check_property_value(name, &value)?;
    // (vector rules: `check_derived_vertex`)
    let Some((labels, properties)) = view.vertex(vertex) else {
        return Err(GraphError::vertex_removed());
    };
    let schema = view.schema();
    let value = SchemaValidator::coerce_path_leaf(&schema, "vertex", labels, path, value)?;
    let (key, resulting) = write_path(properties.get(name.as_str()), path, value)?;
    SchemaValidator::check_element_property(&schema, "vertex", labels, &key, &resulting)?;
    // (unique property indexes, compression)
    Ok((key, resulting))
}

/// The rules of `set_edge_property_path`; see [`set_vertex_property_path`].
pub(crate) fn set_edge_property_path<V: WriteView>(
    view: &V,
    edge: V::Edge,
    path: &JsonPath,
    value: ElementProperty,
) -> Result<(CompactString, ElementProperty), GraphError> {
    let name = path_property_name(path)?;
    check_property_value(name, &value)?;
    // (vector rules: `check_derived_edge`)
    let Some(current) = view.edge(edge) else {
        return Err(GraphError::edge_removed());
    };
    let schema = view.schema();
    let value = SchemaValidator::coerce_path_leaf(&schema, "edge", current.label, path, value)?;
    let existing = current
        .properties
        .and_then(|properties| properties.get(name.as_str()));
    let (key, resulting) = write_path(existing, path, value)?;
    SchemaValidator::check_element_property(&schema, "edge", current.label, &key, &resulting)?;
    Ok((key, resulting))
}

/// The rules of `remove_vertex_property_path`: a one-segment path is [`drop_vertex_property`];
/// otherwise the vertex live, the removal applied to a copy of the stored property, and the
/// schema must still accept the result (required nested keys, `minItems`, ...; nothing is
/// coerced). A missing path is no change.
pub(crate) fn remove_vertex_property_path<V: WriteView>(
    view: &V,
    vertex: V::Vertex,
    path: &JsonPath,
) -> Result<PathRemoval, GraphError> {
    if let Some(name) = single_segment_name(path) {
        return Ok(if drop_vertex_property(view, vertex, name)? {
            PathRemoval::Drop(CompactString::from(name))
        } else {
            PathRemoval::Nothing
        });
    }
    // (vector rules: `check_derived_vertex` on the first segment)
    let Some((labels, properties)) = view.vertex(vertex) else {
        return Err(GraphError::vertex_removed());
    };
    let existing = match path.segments().first() {
        Some(JsonPathSegment::Key(name)) => properties.get(name.as_str()),
        _ => None,
    };
    match remove_path(existing, path) {
        Some((key, resulting)) => {
            SchemaValidator::check_element_property(
                &view.schema(),
                "vertex",
                labels,
                &key,
                &resulting,
            )?;
            Ok(PathRemoval::Store(key, resulting))
        }
        None => Ok(PathRemoval::Nothing),
    }
}

/// The rules of `remove_edge_property_path`; see [`remove_vertex_property_path`].
pub(crate) fn remove_edge_property_path<V: WriteView>(
    view: &V,
    edge: V::Edge,
    path: &JsonPath,
) -> Result<PathRemoval, GraphError> {
    if let Some(name) = single_segment_name(path) {
        return Ok(if drop_edge_property(view, edge, name)? {
            PathRemoval::Drop(CompactString::from(name))
        } else {
            PathRemoval::Nothing
        });
    }
    // (vector rules: `check_derived_edge` on the first segment)
    let Some(current) = view.edge(edge) else {
        return Err(GraphError::edge_removed());
    };
    let existing = match (path.segments().first(), current.properties) {
        (Some(JsonPathSegment::Key(name)), Some(properties)) => properties.get(name.as_str()),
        _ => None,
    };
    match remove_path(existing, path) {
        Some((key, resulting)) => {
            SchemaValidator::check_element_property(
                &view.schema(),
                "edge",
                current.label,
                &key,
                &resulting,
            )?;
            Ok(PathRemoval::Store(key, resulting))
        }
        None => Ok(PathRemoval::Nothing),
    }
}

/// The rules of `drop_vertex_property`: a present property that a label requires cannot be
/// removed ([`SchemaError::RequiredPropertyRemoval`]); then the vertex must be live. `true`:
/// remove the property; `false`: the vertex does not have it (no change).
pub(crate) fn drop_vertex_property<V: WriteView>(
    view: &V,
    vertex: V::Vertex,
    key: &str,
) -> Result<bool, GraphError> {
    // (vector rules: `check_derived_vertex`)
    let Some((labels, properties)) = view.vertex(vertex) else {
        return Err(GraphError::vertex_removed());
    };
    if !properties.contains_key(key) {
        return Ok(false);
    }
    SchemaValidator::check_removal(&view.schema(), "vertex", labels, key)?;
    Ok(true)
}

/// The rules of `drop_edge_property`; see [`drop_vertex_property`].
pub(crate) fn drop_edge_property<V: WriteView>(
    view: &V,
    edge: V::Edge,
    key: &str,
) -> Result<bool, GraphError> {
    // (vector rules: `check_derived_edge`)
    let Some(current) = view.edge(edge) else {
        return Err(GraphError::edge_removed());
    };
    if !current
        .properties
        .is_some_and(|properties| properties.contains_key(key))
    {
        return Ok(false);
    }
    SchemaValidator::check_removal(&view.schema(), "edge", current.label, key)?;
    Ok(true)
}

// ------------------------------------------------------------------------------------------
// Drops
// ------------------------------------------------------------------------------------------

/// The rules of `drop_vertex`: none. A stale handle is a no-op (idempotent drop, never an
/// error) and no schema check applies (no required-edge or topology rule exists for a drop; the
/// incident edges go with the vertex). `true`: drop the vertex and its incident edges.
pub(crate) fn drop_vertex<V: WriteView>(view: &V, vertex: V::Vertex) -> bool {
    view.vertex(vertex).is_some()
}

/// The rules of `drop_edge`: none, as for [`drop_vertex`]. `true`: drop the edge.
pub(crate) fn drop_edge<V: WriteView>(view: &V, edge: V::Edge) -> bool {
    view.edge(edge).is_some()
}

// ------------------------------------------------------------------------------------------
// The reference view
// ------------------------------------------------------------------------------------------

/// `TraversalGraph` as a read view: the reference the unit tests compare against.
impl WriteView for TraversalGraph {
    type Vertex = VertexHandle;
    type Edge = EdgeHandle;

    fn schema(&self) -> impl Deref<Target = GraphSchema> + '_ {
        self.schema.read()
    }

    fn vertex(&self, vertex: VertexHandle) -> Option<(&[CompactString], &PropertyMap)> {
        self.vertex_weight(vertex)
            .map(|weight| (self.labels.set(weight.1), &weight.2))
    }

    fn edge(&self, edge: EdgeHandle) -> Option<EdgeView<'_, VertexHandle>> {
        let weight = self.edge_weight(edge)?;
        let (from, to) = GraphStorage::edge_endpoints(self, edge).ok().flatten()?;
        Some(EdgeView {
            label: self.labels.name_slice(weight.1),
            properties: weight.2.as_ref(),
            from,
            to,
        })
    }

    fn out_edges(&self, vertex: VertexHandle) -> impl Iterator<Item = EdgeHandle> + '_ {
        GraphStorage::out_edges(self, vertex).into_iter().flatten()
    }

    fn in_edges(&self, vertex: VertexHandle) -> impl Iterator<Item = EdgeHandle> + '_ {
        GraphStorage::in_edges(self, vertex).into_iter().flatten()
    }

    fn contains_vertex_id(&self, id: &str) -> bool {
        self.index.contains_node(&self.graph, id)
    }

    fn contains_edge_id(&self, id: &str) -> bool {
        self.index.get_edge(&self.graph, id).is_some()
    }
}

#[cfg(test)]
mod tests {
    //! Every rule is run against a `TraversalGraph` used as the read view, then the real write
    //! runs on the same graph (the rule reads the state before the write, as a twin graph would):
    //! both must fail with the same error (variant and message), or both succeed with the rule
    //! returning exactly what the write stored.

    use super::*;
    use crate::graph::schema::defaults::datetime_of_nanos;
    use std::fmt::Debug;

    const OPEN: &str = r#"{
      "mode": "open",
      "vertices": {
        "person": {"schema": {"type": "object", "properties": {
          "name": {"type": "string"},
          "age": {"type": "integer"},
          "score": {"type": "number"},
          "uid": {"type": "string", "format": "uuid"},
          "born": {"type": "string", "format": "date"},
          "level": {"type": "integer", "default": 1},
          "meta": {"type": "object", "properties": {
            "n": {"type": "number"},
            "at": {"type": "string", "format": "date"}}, "required": ["n"]}
        }, "required": ["name"]}},
        "software": {"schema": {"type": "object", "properties": {"lang": {"type": "string"}}}}
      },
      "edges": {
        "knows": {"schema": {"type": "object", "properties": {
          "weight": {"type": "number"}, "since": {"type": "integer", "default": 2000}}}},
        "created": {"schema": {"type": "object", "properties": {"weight": {"type": "number"}}}},
        "rated": {"schema": {"type": "object", "properties": {"stars": {"type": "integer"}},
                  "required": ["stars"]}}
      }
    }"#;

    const CLOSED: &str = r#"{
      "mode": "closed",
      "vertices": {
        "person": {"schema": {"type": "object", "properties": {
          "name": {"type": "string"},
          "age": {"type": "integer"},
          "level": {"type": "integer", "default": 1},
          "meta": {"type": "object", "properties": {"n": {"type": "number"}}}
        }, "required": ["name"]}},
        "software": {"schema": {"type": "object", "properties": {
          "name": {"type": "string"}, "lang": {"type": "string"}}}},
        "any": {"schema": {"type": "object", "additionalProperties": true}}
      },
      "edges": {
        "knows": {"connections": [{"from": "person", "to": "person"}],
                  "schema": {"type": "object", "properties": {
                    "weight": {"type": "number"}, "since": {"type": "integer", "default": 2000}}}},
        "created": {"connections": [{"from": "person", "to": "software"}],
                    "schema": {"type": "object", "properties": {"weight": {"type": "number"}}}},
        "rated": {"connections": [{"from": "person", "to": "software"}],
                  "schema": {"type": "object", "properties": {
                    "weight": {"type": "number"}, "stars": {"type": "integer"}},
                    "required": ["stars"]}}
      }
    }"#;

    /// The three schema modes: none, open, closed.
    const MODES: [Option<&str>; 3] = [None, Some(OPEN), Some(CLOSED)];

    /// The modern graph under `schema`, with peter (6) and his edge 5 dropped; returns the
    /// graph and the two stale handles.
    fn graph(schema: Option<&str>) -> (TraversalGraph, VertexHandle, EdgeHandle) {
        let mut g = TraversalGraph::tinkerpop_modern();
        let peter = v(&g, "6");
        let e12 = e(&g, "5");
        GraphStorage::drop_edge(&mut g, e12).unwrap();
        GraphStorage::drop_vertex(&mut g, peter).unwrap();
        if let Some(schema) = schema {
            GraphStorage::set_schema(&mut g, GraphSchema::from_json(schema).unwrap()).unwrap();
        }
        (g, peter, e12)
    }

    fn v(g: &TraversalGraph, id: &str) -> VertexHandle {
        GraphStorage::get_vertex_by_id(g, id).unwrap().unwrap()
    }

    fn e(g: &TraversalGraph, id: &str) -> EdgeHandle {
        GraphStorage::get_edge_by_id(g, id).unwrap().unwrap()
    }

    fn context() -> CreateContext {
        CreateContext::at(datetime_of_nanos(1_700_000_000_000_000_000))
    }

    fn vertex_value() -> ElementProperty {
        ElementProperty::Vertex(Box::default())
    }

    fn props(entries: &[(&str, ElementProperty)]) -> PropertyMap {
        let mut map = PropertyMap::default();
        for (key, value) in entries {
            map.insert(CompactString::from(*key), value.clone());
        }
        map
    }

    fn entries(map: Option<&PropertyMap>) -> Vec<(String, ElementProperty)> {
        map.map(|map| {
            map.iter()
                .map(|(key, value)| (key.to_string(), value.clone()))
                .collect()
        })
        .unwrap_or_default()
    }

    fn path(text: &str) -> JsonPath {
        text.parse().expect("valid path")
    }

    fn same_error(rule: &dyn std::fmt::Display, write: &dyn std::fmt::Display) {
        assert_eq!(rule.to_string(), write.to_string());
    }

    /// Both fail alike (`None`), or both succeed (`Some`); anything else is a mismatch.
    fn agree<T: Debug, W: Debug>(
        what: &str,
        rule: Result<T, GraphError>,
        write: Result<W, GraphError>,
    ) -> Option<(T, W)> {
        match (rule, write) {
            (Ok(rule), Ok(write)) => Some((rule, write)),
            (Err(rule), Err(write)) => {
                assert_eq!(
                    std::mem::discriminant(&rule),
                    std::mem::discriminant(&write),
                    "{what}: {rule:?} vs {write:?}"
                );
                same_error(&rule, &write);
                None
            }
            (rule, write) => panic!("{what}: rule {rule:?}, write {write:?}"),
        }
    }

    // --- creates -------------------------------------------------------------------------

    fn insert_v(g: &mut TraversalGraph, what: &str, labels: &[&str], map: PropertyMap) -> bool {
        let labels: Vec<CompactString> = labels.iter().map(|l| CompactString::from(*l)).collect();
        let rule = insert_vertex(&*g, labels.clone(), map.clone());
        let write = GraphStorage::insert_vertex(g, None, labels, map);
        let Some(((labels, stored), handle)) = agree(what, rule, write) else {
            return false;
        };
        assert_eq!(
            GraphStorage::vertex_labels(g, handle).unwrap(),
            labels,
            "{what}"
        );
        assert_eq!(
            entries(GraphStorage::vertex_properties(g, handle).unwrap()),
            entries(Some(&stored)),
            "{what}"
        );
        true
    }

    #[test]
    fn insert_vertex_matches_in_every_mode() {
        for (mode, schema) in MODES.into_iter().enumerate() {
            let (mut g, _, _) = graph(schema);
            let name = ("name", ElementProperty::from("ann"));
            let cases: Vec<(&str, Vec<&str>, PropertyMap)> = vec![
                ("plain", vec!["person"], props(&[name.clone()])),
                (
                    "duplicates",
                    vec!["person", "person"],
                    props(&[name.clone()]),
                ),
                ("bad label", vec!["person", "a::b"], props(&[name.clone()])),
                (
                    "vertex value",
                    vec!["person"],
                    props(&[name.clone(), ("f", vertex_value())]),
                ),
                (
                    "missing required",
                    vec!["person"],
                    props(&[("age", 3_i64.into())]),
                ),
                (
                    "type mismatch",
                    vec!["person"],
                    props(&[name.clone(), ("age", "x".into())]),
                ),
                (
                    "int to number",
                    vec!["person"],
                    props(&[name.clone(), ("score", 3_i64.into())]),
                ),
                (
                    "uuid",
                    vec!["person"],
                    props(&[
                        name.clone(),
                        ("uid", "123e4567-e89b-12d3-a456-426614174000".into()),
                    ]),
                ),
                (
                    "bad uuid",
                    vec!["person"],
                    props(&[name.clone(), ("uid", "nope".into())]),
                ),
                (
                    "date",
                    vec!["person"],
                    props(&[name.clone(), ("born", "2024-05-01".into())]),
                ),
                (
                    "bad date",
                    vec!["person"],
                    props(&[name.clone(), ("born", "05/01".into())]),
                ),
                (
                    "undeclared",
                    vec!["person"],
                    props(&[name.clone(), ("zzz", 1_i64.into())]),
                ),
                ("unknown label", vec!["robot"], props(&[name.clone()])),
                ("unlabeled", vec![], props(&[])),
                (
                    "two labels",
                    vec!["person", "any"],
                    props(&[name.clone(), ("x", 1_i64.into())]),
                ),
            ];
            for (what, labels, map) in cases {
                insert_v(&mut g, &format!("mode {mode}: {what}"), &labels, map);
            }
        }
        // The coercions really happen (not only agree): open mode.
        let (g, _, _) = graph(Some(OPEN));
        let (_, stored) = insert_vertex(
            &g,
            vec!["person".into()],
            props(&[
                ("name", "ann".into()),
                ("born", "2024-05-01".into()),
                ("uid", "123e4567-e89b-12d3-a456-426614174000".into()),
            ]),
        )
        .unwrap();
        assert!(matches!(
            stored.get("born"),
            Some(ElementProperty::Temporal(_))
        ));
        assert!(matches!(stored.get("uid"), Some(ElementProperty::Uuid(_))));
    }

    #[test]
    fn add_vertex_with_fills_defaults_as_the_write_does() {
        for (mode, schema) in MODES.into_iter().enumerate() {
            let (mut g, _, _) = graph(schema);
            for (what, labels, map) in [
                ("default", vec!["person"], props(&[("name", "ann".into())])),
                (
                    "explicit",
                    vec!["person"],
                    props(&[("name", "ann".into()), ("level", 7_i64.into())]),
                ),
                (
                    "duplicates",
                    vec!["person", "person"],
                    props(&[("name", "ann".into())]),
                ),
                ("bad label", vec!["a::b"], props(&[])),
            ] {
                let what = format!("mode {mode}: {what}");
                let labels: Vec<CompactString> =
                    labels.into_iter().map(CompactString::from).collect();
                let rule = add_vertex_with(&g, labels.clone(), map.clone(), &context());
                let write = GraphStorage::add_vertex_with(&mut g, None, labels, map, &context());
                if let Some(((labels, stored), handle)) = agree(&what, rule, write) {
                    assert_eq!(GraphStorage::vertex_labels(&g, handle).unwrap(), labels);
                    assert_eq!(
                        entries(GraphStorage::vertex_properties(&g, handle).unwrap()),
                        entries(Some(&stored)),
                        "{what}"
                    );
                    if mode > 0 && what.ends_with("default") {
                        assert_eq!(stored.get("level"), Some(&ElementProperty::from(1_i64)));
                    }
                }
            }
        }
    }

    fn insert_e(
        g: &mut TraversalGraph,
        what: &str,
        label: Option<&str>,
        from: VertexHandle,
        to: VertexHandle,
        map: PropertyMap,
        defaults: bool,
    ) {
        let (rule, write) = if defaults {
            (
                add_edge_with(&*g, label, from, to, map.clone(), &context()),
                GraphStorage::add_edge_with(
                    g,
                    None,
                    label.map(Into::into),
                    from,
                    to,
                    map,
                    &context(),
                ),
            )
        } else {
            (
                insert_edge(&*g, label, from, to, map.clone()),
                GraphStorage::insert_edge(g, None, label.map(Into::into), from, to, map),
            )
        };
        if let Some((stored, handle)) = agree(what, rule, write) {
            assert_eq!(
                entries(GraphStorage::edge_properties(g, handle).unwrap()),
                entries(Some(&stored)),
                "{what}"
            );
        }
    }

    #[test]
    fn insert_edge_matches_in_every_mode() {
        for (mode, schema) in MODES.into_iter().enumerate() {
            for defaults in [false, true] {
                let (mut g, peter, _) = graph(schema);
                let (marko, vadas, lop) = (v(&g, "1"), v(&g, "2"), v(&g, "3"));
                let w = |x: ElementProperty| props(&[("weight", x)]);
                let cases: Vec<(&str, Option<&str>, VertexHandle, VertexHandle, PropertyMap)> = vec![
                    ("plain", Some("knows"), marko, vadas, w(0.5_f64.into())),
                    (
                        "int to number",
                        Some("knows"),
                        marko,
                        vadas,
                        w(1_i64.into()),
                    ),
                    ("type mismatch", Some("knows"), marko, vadas, w("x".into())),
                    (
                        "vertex value",
                        Some("knows"),
                        marko,
                        vadas,
                        w(vertex_value()),
                    ),
                    ("topology", Some("knows"), marko, lop, w(1.0_f64.into())),
                    (
                        "topology ok",
                        Some("created"),
                        marko,
                        lop,
                        w(1.0_f64.into()),
                    ),
                    ("reversed", Some("created"), lop, marko, w(1.0_f64.into())),
                    (
                        "missing required",
                        Some("rated"),
                        marko,
                        lop,
                        w(1.0_f64.into()),
                    ),
                    ("unknown label", Some("likes"), marko, vadas, props(&[])),
                    ("unlabeled", None, marko, vadas, props(&[])),
                    (
                        "undeclared",
                        Some("knows"),
                        marko,
                        vadas,
                        props(&[("zzz", 1_i64.into())]),
                    ),
                    ("stale source", Some("knows"), peter, vadas, props(&[])),
                    (
                        "stale target",
                        Some("knows"),
                        marko,
                        peter,
                        w(vertex_value()),
                    ),
                    ("self loop", Some("knows"), marko, marko, props(&[])),
                ];
                for (what, label, from, to, map) in cases {
                    let what = format!("mode {mode}, defaults {defaults}: {what}");
                    insert_e(&mut g, &what, label, from, to, map, defaults);
                }
            }
        }
    }

    #[test]
    fn id_checks_give_the_write_errors() {
        let (mut g, _, _) = graph(Some(OPEN));
        let rule = check_vertex_id_free(&g, "1");
        let write = GraphStorage::insert_vertex(
            &mut g,
            Some("1".into()),
            vec!["person".into()],
            props(&[("name", "ann".into())]),
        );
        assert!(agree("vertex id", rule, write).is_none());
        assert!(check_vertex_id_free(&g, "99").is_ok());
        let (marko, vadas) = (v(&g, "1"), v(&g, "2"));
        let rule = check_edge_id_free(&g, "0");
        let write = GraphStorage::insert_edge(
            &mut g,
            Some("0".into()),
            Some("knows".into()),
            marko,
            vadas,
            props(&[]),
        );
        assert!(agree("edge id", rule, write).is_none());
        assert!(check_edge_id_free(&g, "5").is_ok(), "12 was dropped");
    }

    // --- labels ----------------------------------------------------------------------------

    fn add_label(g: &mut TraversalGraph, what: &str, vertex: VertexHandle, label: &str) {
        let rule = add_vertex_label(&*g, vertex, label);
        let write = GraphStorage::add_vertex_label(g, vertex, label.into());
        if let Some((change, changed)) = agree(what, rule, write) {
            assert_eq!(change.is_some(), changed, "{what}");
            if let Some(labels) = change {
                assert_eq!(
                    GraphStorage::vertex_labels(g, vertex).unwrap(),
                    labels,
                    "{what}"
                );
            }
        }
    }

    fn remove_label(g: &mut TraversalGraph, what: &str, vertex: VertexHandle, label: &str) {
        let rule = remove_vertex_label(&*g, vertex, label);
        let write = GraphStorage::remove_vertex_label(g, vertex, label);
        if let Some((change, changed)) = agree(what, rule, write) {
            assert_eq!(change.is_some(), changed, "{what}");
            if let Some(labels) = change {
                assert_eq!(
                    GraphStorage::vertex_labels(g, vertex).unwrap(),
                    labels,
                    "{what}"
                );
            }
        }
    }

    #[test]
    fn vertex_label_writes_match_in_every_mode() {
        for (mode, schema) in MODES.into_iter().enumerate() {
            let (mut g, peter, _) = graph(schema);
            let (marko, vadas, lop) = (v(&g, "1"), v(&g, "2"), v(&g, "3"));
            let m = |what: &str| format!("mode {mode}: {what}");
            add_label(&mut g, &m("present"), marko, "person");
            add_label(&mut g, &m("bad label"), marko, "x::y");
            add_label(&mut g, &m("bad label, stale"), peter, "x::y");
            add_label(&mut g, &m("stale"), peter, "any");
            add_label(&mut g, &m("unknown"), marko, "robot");
            add_label(&mut g, &m("second"), marko, "software");
            add_label(&mut g, &m("any"), vadas, "any");
            // Closed: vadas keeps valid properties under `any`, but marko-knows->vadas breaks.
            remove_label(&mut g, &m("topology"), vadas, "person");
            remove_label(&mut g, &m("absent"), vadas, "software");
            remove_label(&mut g, &m("last label"), lop, "software");
            remove_label(&mut g, &m("stale"), peter, "person");
            remove_label(&mut g, &m("primary"), marko, "person");
        }
    }

    #[test]
    fn incident_topology_matches_the_traversal_graph_check() {
        let (mut g, _, _) = graph(Some(CLOSED));
        let marko = v(&g, "1");
        GraphStorage::insert_edge(&mut g, None, Some("knows".into()), marko, marko, props(&[]))
            .unwrap();
        let label_sets: [&[&str]; 6] = [
            &[],
            &["person"],
            &["software"],
            &["any"],
            &["person", "software"],
            &["software", "person"],
        ];
        let mut failures = 0;
        for id in ["1", "2", "3", "4", "5"] {
            let vertex = v(&g, id);
            for labels in label_sets {
                let labels: Vec<CompactString> =
                    labels.iter().map(|l| CompactString::from(*l)).collect();
                let rule = check_incident_topology(&g, vertex, &labels);
                let reference = g.check_incident_topology(vertex.index(), &labels);
                match (rule, reference) {
                    (Ok(()), Ok(())) => {}
                    (Err(rule), Err(reference)) => {
                        failures += 1;
                        assert_eq!(
                            std::mem::discriminant(&rule),
                            std::mem::discriminant(&reference)
                        );
                        same_error(&rule, &reference);
                    }
                    (rule, reference) => panic!("{id} {labels:?}: {rule:?} vs {reference:?}"),
                }
            }
        }
        assert!(failures > 0, "the cases include violations");
        // Other modes check nothing.
        for schema in [None, Some(OPEN)] {
            let (g, _, _) = graph(schema);
            assert!(check_incident_topology(&g, v(&g, "1"), &[]).is_ok());
        }
    }

    fn edge_label(g: &mut TraversalGraph, what: &str, edge: EdgeHandle, label: &str) {
        let rule = set_edge_label(&*g, edge, label);
        let write = GraphStorage::set_edge_label(g, edge, label.into());
        if let Some((rule, write)) = agree(what, rule, write) {
            assert_eq!(rule, write, "{what}");
            assert_eq!(GraphStorage::edge_label(g, edge).unwrap(), Some(label));
        }
    }

    #[test]
    fn set_edge_label_matches_in_every_mode() {
        for (mode, schema) in MODES.into_iter().enumerate() {
            let (mut g, _, e12) = graph(schema);
            let m = |what: &str| format!("mode {mode}: {what}");
            let (e7, e9) = (e(&g, "0"), e(&g, "1"));
            edge_label(&mut g, &m("unchanged"), e7, "knows");
            edge_label(&mut g, &m("stale"), e12, "knows");
            edge_label(&mut g, &m("topology"), e9, "knows");
            edge_label(&mut g, &m("topology, target"), e7, "created");
            edge_label(&mut g, &m("missing required"), e9, "rated");
            edge_label(&mut g, &m("unknown"), e7, "likes");
            edge_label(&mut g, &m("double colon"), e7, "a::b");
            edge_label(&mut g, &m("back"), e7, "knows");
        }
    }

    // --- properties ------------------------------------------------------------------------

    fn set_vp(
        g: &mut TraversalGraph,
        what: &str,
        vertex: VertexHandle,
        key: &str,
        value: ElementProperty,
    ) {
        let rule = set_vertex_property(&*g, vertex, key, value.clone());
        let write = GraphStorage::set_vertex_property(g, vertex, key.into(), value);
        if let Some((stored, ())) = agree(what, rule, write) {
            let props = GraphStorage::vertex_properties(g, vertex).unwrap();
            assert_eq!(props.and_then(|p| p.get(key)), Some(&stored), "{what}");
        }
    }

    fn set_ep(
        g: &mut TraversalGraph,
        what: &str,
        edge: EdgeHandle,
        key: &str,
        value: ElementProperty,
    ) {
        let rule = set_edge_property(&*g, edge, key, value.clone());
        let write = GraphStorage::set_edge_property(g, edge, key.into(), value);
        if let Some((stored, ())) = agree(what, rule, write) {
            let props = GraphStorage::edge_properties(g, edge).unwrap();
            assert_eq!(props.and_then(|p| p.get(key)), Some(&stored), "{what}");
        }
    }

    #[test]
    fn property_writes_match_in_every_mode() {
        for (mode, schema) in MODES.into_iter().enumerate() {
            let (mut g, peter, e12) = graph(schema);
            let (marko, lop) = (v(&g, "1"), v(&g, "3"));
            let m = |what: &str| format!("mode {mode}: {what}");
            set_vp(&mut g, &m("age"), marko, "age", 30_i64.into());
            set_vp(&mut g, &m("age mismatch"), marko, "age", "x".into());
            set_vp(&mut g, &m("int to number"), marko, "score", 3_i64.into());
            set_vp(
                &mut g,
                &m("uuid"),
                marko,
                "uid",
                "123e4567-e89b-12d3-a456-426614174000".into(),
            );
            set_vp(&mut g, &m("bad uuid"), marko, "uid", "nope".into());
            set_vp(&mut g, &m("date"), marko, "born", "2024-05-01".into());
            set_vp(&mut g, &m("bad date"), marko, "born", "bad".into());
            set_vp(&mut g, &m("undeclared"), marko, "zzz", 1_i64.into());
            set_vp(
                &mut g,
                &m("null required"),
                marko,
                "name",
                ElementProperty::Null,
            );
            set_vp(&mut g, &m("vertex value"), marko, "f", vertex_value());
            set_vp(
                &mut g,
                &m("nested required"),
                marko,
                "meta",
                ElementProperty::Object(Box::default()),
            );
            set_vp(&mut g, &m("software lang"), lop, "lang", 1_i64.into());
            set_vp(&mut g, &m("stale"), peter, "age", 1_i64.into());
            set_vp(
                &mut g,
                &m("stale, vertex value"),
                peter,
                "f",
                vertex_value(),
            );
            let e7 = e(&g, "0");
            set_ep(&mut g, &m("weight"), e7, "weight", 1_i64.into());
            set_ep(&mut g, &m("weight mismatch"), e7, "weight", "x".into());
            set_ep(&mut g, &m("undeclared"), e7, "zzz", 1_i64.into());
            set_ep(&mut g, &m("vertex value"), e7, "f", vertex_value());
            set_ep(&mut g, &m("stale"), e12, "weight", 1_i64.into());
            set_ep(&mut g, &m("stale, vertex value"), e12, "f", vertex_value());
        }
    }

    fn set_vpath(
        g: &mut TraversalGraph,
        what: &str,
        vertex: VertexHandle,
        at: &str,
        value: ElementProperty,
    ) {
        let at = path(at);
        let rule = set_vertex_property_path(&*g, vertex, &at, value.clone());
        let write = GraphStorage::set_vertex_property_path(g, vertex, &at, value);
        if let Some(((key, stored), ())) = agree(what, rule, write) {
            let props = GraphStorage::vertex_properties(g, vertex).unwrap();
            assert_eq!(
                props.and_then(|p| p.get(key.as_str())),
                Some(&stored),
                "{what}"
            );
        }
    }

    fn set_epath(
        g: &mut TraversalGraph,
        what: &str,
        edge: EdgeHandle,
        at: &str,
        value: ElementProperty,
    ) {
        let at = path(at);
        let rule = set_edge_property_path(&*g, edge, &at, value.clone());
        let write = GraphStorage::set_edge_property_path(g, edge, &at, value);
        if let Some(((key, stored), ())) = agree(what, rule, write) {
            let props = GraphStorage::edge_properties(g, edge).unwrap();
            assert_eq!(
                props.and_then(|p| p.get(key.as_str())),
                Some(&stored),
                "{what}"
            );
        }
    }

    fn remove_vpath(g: &mut TraversalGraph, what: &str, vertex: VertexHandle, at: &str) {
        let at = path(at);
        let before = entries(GraphStorage::vertex_properties(g, vertex).unwrap());
        let rule = remove_vertex_property_path(&*g, vertex, &at);
        let write = GraphStorage::remove_vertex_property_path(g, vertex, &at);
        if let Some((removal, ())) = agree(what, rule, write) {
            let props = GraphStorage::vertex_properties(g, vertex).unwrap();
            check_removal_outcome(what, removal, props, before);
        }
    }

    fn remove_epath(g: &mut TraversalGraph, what: &str, edge: EdgeHandle, at: &str) {
        let at = path(at);
        let before = entries(GraphStorage::edge_properties(g, edge).unwrap());
        let rule = remove_edge_property_path(&*g, edge, &at);
        let write = GraphStorage::remove_edge_property_path(g, edge, &at);
        if let Some((removal, ())) = agree(what, rule, write) {
            let props = GraphStorage::edge_properties(g, edge).unwrap();
            check_removal_outcome(what, removal, props, before);
        }
    }

    fn check_removal_outcome(
        what: &str,
        removal: PathRemoval,
        after: Option<&PropertyMap>,
        before: Vec<(String, ElementProperty)>,
    ) {
        match removal {
            PathRemoval::Nothing => assert_eq!(entries(after), before, "{what}"),
            PathRemoval::Drop(key) => {
                assert!(before.iter().any(|(k, _)| k == key.as_str()), "{what}");
                assert!(after.and_then(|p| p.get(key.as_str())).is_none(), "{what}");
            }
            PathRemoval::Store(key, value) => {
                assert_eq!(
                    after.and_then(|p| p.get(key.as_str())),
                    Some(&value),
                    "{what}"
                );
            }
        }
    }

    #[test]
    fn path_writes_match_in_every_mode() {
        for (mode, schema) in MODES.into_iter().enumerate() {
            let (mut g, peter, e12) = graph(schema);
            let marko = v(&g, "1");
            let m = |what: &str| format!("mode {mode}: {what}");
            set_vpath(&mut g, &m("first segment"), marko, "[0].a", 1_i64.into());
            set_vpath(&mut g, &m("key on string"), marko, "name.x", 1_i64.into());
            set_vpath(&mut g, &m("missing array"), marko, "tags[0]", 1_i64.into());
            set_vpath(&mut g, &m("vertex value"), marko, "meta.n", vertex_value());
            set_vpath(&mut g, &m("leaf to number"), marko, "meta.n", 3_i64.into());
            set_vpath(
                &mut g,
                &m("leaf date"),
                marko,
                "meta.at",
                "2024-05-01".into(),
            );
            set_vpath(&mut g, &m("leaf bad date"), marko, "meta.at", "bad".into());
            set_vpath(
                &mut g,
                &m("nested undeclared"),
                marko,
                "meta.zzz",
                1_i64.into(),
            );
            set_vpath(&mut g, &m("one segment"), marko, "age", 31_i64.into());
            set_vpath(&mut g, &m("stale"), peter, "meta.n", 1_i64.into());
            remove_vpath(&mut g, &m("nested required"), marko, "meta.n");
            remove_vpath(&mut g, &m("nested"), marko, "meta.at");
            remove_vpath(&mut g, &m("missing"), marko, "meta.nope");
            remove_vpath(&mut g, &m("type mismatch on the way"), marko, "name.x");
            remove_vpath(&mut g, &m("required"), marko, "name");
            remove_vpath(&mut g, &m("drop"), marko, "age");
            remove_vpath(&mut g, &m("drop absent"), marko, "age");
            remove_vpath(&mut g, &m("stale"), peter, "meta.n");
            remove_vpath(&mut g, &m("stale, one segment"), peter, "age");
            let e7 = e(&g, "0");
            set_epath(&mut g, &m("edge undeclared"), e7, "meta.k", 1_i64.into());
            set_epath(
                &mut g,
                &m("edge key on number"),
                e7,
                "weight.x",
                1_i64.into(),
            );
            set_epath(&mut g, &m("edge first segment"), e7, "[1]", 1_i64.into());
            set_epath(&mut g, &m("edge stale"), e12, "meta.k", 1_i64.into());
            remove_epath(&mut g, &m("edge nested"), e7, "meta.k");
            remove_epath(&mut g, &m("edge missing"), e7, "nope.k");
            remove_epath(&mut g, &m("edge drop"), e7, "weight");
            remove_epath(&mut g, &m("edge stale"), e12, "meta.k");
        }
    }

    #[test]
    fn property_drops_match_in_every_mode() {
        for (mode, schema) in MODES.into_iter().enumerate() {
            let (mut g, peter, e12) = graph(schema);
            let (marko, lop) = (v(&g, "1"), v(&g, "3"));
            let m = |what: &str| format!("mode {mode}: {what}");
            let rated = GraphStorage::insert_edge(
                &mut g,
                None,
                Some("rated".into()),
                marko,
                lop,
                props(&[("stars", 5_i64.into())]),
            )
            .unwrap();
            let cases: Vec<(String, VertexHandle, &str)> = vec![
                (m("required"), marko, "name"),
                (m("present"), marko, "age"),
                (m("absent"), marko, "age"),
                (m("stale"), peter, "age"),
            ];
            for (what, vertex, key) in cases {
                let had = GraphStorage::vertex_properties(&g, vertex)
                    .unwrap()
                    .is_some_and(|props| props.contains_key(key));
                let rule = drop_vertex_property(&g, vertex, key);
                let write = GraphStorage::drop_vertex_property(&mut g, key, vertex);
                if let Some((present, ())) = agree(&what, rule, write) {
                    let props = GraphStorage::vertex_properties(&g, vertex)
                        .unwrap()
                        .unwrap();
                    assert!(!props.contains_key(key), "{what}");
                    assert_eq!(present, had, "{what}");
                }
            }
            let e7 = e(&g, "0");
            for (what, edge, key) in [
                (m("edge required"), rated, "stars"),
                (m("edge present"), e7, "weight"),
                (m("edge absent"), e7, "weight"),
                (m("edge stale"), e12, "weight"),
            ] {
                let rule = drop_edge_property(&g, edge, key);
                let write = GraphStorage::drop_edge_property(&mut g, key, edge);
                agree(&what, rule, write);
            }
            // A present property reports `true`, an absent one `false`.
            assert!(drop_vertex_property(&g, v(&g, "2"), "age").unwrap());
            assert!(!drop_vertex_property(&g, v(&g, "2"), "lang").unwrap());
        }
    }

    #[test]
    fn element_drops_have_no_rules_but_liveness() {
        for schema in MODES {
            let (mut g, peter, e12) = graph(schema);
            assert!(!drop_vertex(&g, peter));
            assert!(!drop_edge(&g, e12));
            let (marko, e7) = (v(&g, "1"), e(&g, "0"));
            assert!(drop_edge(&g, e7));
            GraphStorage::drop_edge(&mut g, e7).unwrap();
            assert!(!drop_edge(&g, e7));
            // Closed mode: dropping a vertex is never refused, required properties or not.
            assert!(drop_vertex(&g, marko));
            GraphStorage::drop_vertex(&mut g, marko).unwrap();
            assert!(!drop_vertex(&g, marko));
        }
    }
}
