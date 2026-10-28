//! Schema inference: the schema a set of vertices and edges satisfies.
//!
//! Every value is observed at its position (a top-level property, a key of a nested object,
//! the items of an array), and each position becomes a [`PropertySchema`] that admits exactly
//! the kinds seen there. An object key is `required` only when every object observed at that
//! position had it, at any depth, also inside arrays and next to other kinds (`anyOf`), so the
//! inferred schema is valid for the data it came from, in `open` and in `closed` mode. An
//! unlabeled vertex or edge has no label to declare and is left out, so in `closed` mode
//! (which admits only declared labels) applying the schema fails with a report naming them.
//!
//! Inference reports the logical type of each stored value exactly: a string that looks like a
//! UUID, a date or an IP address is `string`; a real `uuid` value is
//! `{"type": "string", "format": "uuid"}`, a real `datetime` `{"type": "string", "format":
//! "date-time"}` (and `date`, `local-date-time`, `time`, `duration` for the other temporal
//! types; a position holding a string and a temporal value is an `anyOf`), a vector
//! `{"type": "array", "format": "vector", "element": .., "dimension": ..}` (the dimension when
//! every vector of that element at the position has the same one). No content-sniffing
//! detector may ever be added here
//! (CLAUDE.md rule 7). The inferred schema has mode `none` and no `additionalProperties`; set
//! the mode when applying it (`GraphSchema::with_mode`, the DSL's
//! `g.set_schema(g.infer_schema(), SchemaMode.closed)`): "freeze the structure you have".

use std::collections::{BTreeMap, BTreeSet};

use indexmap::IndexMap;

use crate::graph::GraphStorage;
use crate::graph::schema::{
    EdgeConnection, EdgeSchema, GraphSchema, JsonType, PropertySchema, VertexSchema,
};
use crate::value::temporal::TemporalKind;
use crate::value::{ElementProperty, PropertyMap, VectorElement};

/// What was observed at one position.
#[derive(Debug, Default, Clone)]
struct Observed {
    null: bool,
    boolean: bool,
    integer: bool,
    number: bool,
    string: bool,
    uuid: bool,
    /// The temporal types seen here (`format: date-time`, ...), in their order.
    temporal: BTreeSet<TemporalKind>,
    /// The vector elements seen here, each with its dimension (`None` once two differ).
    vectors: BTreeMap<VectorElement, Option<u32>>,
    /// Arrays seen here: the observation of their items (`None` while every array was empty).
    array: Option<Option<Box<Observed>>>,
    /// Objects seen here.
    object: Option<ObservedObject>,
}

/// The objects observed at one position: how many, and each key's count and values.
#[derive(Debug, Default, Clone)]
struct ObservedObject {
    count: usize,
    keys: IndexMap<String, (usize, Observed)>,
}

impl ObservedObject {
    fn observe_map<'a>(&mut self, entries: impl Iterator<Item = (&'a str, &'a ElementProperty)>) {
        self.count += 1;
        for (key, value) in entries {
            if !is_property_value(value) {
                continue;
            }
            let entry = self
                .keys
                .entry(key.to_string())
                .or_insert_with(|| (0, Observed::default()));
            entry.0 += 1;
            entry.1.observe(value);
        }
    }

    fn into_schema(self) -> PropertySchema {
        let mut node = PropertySchema::object();
        for (key, (count, observed)) in self.keys {
            if count == self.count {
                node.required.push(key.clone());
            }
            node.properties.insert(key, observed.into_schema());
        }
        node
    }
}

/// A graph element (at any depth) is never a property value; storage never holds one.
fn is_property_value(value: &ElementProperty) -> bool {
    crate::graph::schema::type_of(value).is_some()
}

impl Observed {
    fn observe(&mut self, value: &ElementProperty) {
        match value {
            ElementProperty::Null => self.null = true,
            ElementProperty::Boolean(_) => self.boolean = true,
            ElementProperty::Number(n) if n.is_i64() => self.integer = true,
            ElementProperty::Number(_) => self.number = true,
            // A compressed string is a string (D5).
            ElementProperty::String(_)
            | ElementProperty::Compressed(_)
            | ElementProperty::Deduped(_) => self.string = true,
            ElementProperty::Uuid(_) => self.uuid = true,
            ElementProperty::Temporal(value) => {
                self.temporal.insert(value.kind());
            }
            // A vector is `{"type": "array", "format": "vector", ..}` of its element, with its
            // dimension while every vector of that element here has the same one.
            ElementProperty::Vector(vector) => {
                let dimension = vector.dimension() as u32;
                self.vectors
                    .entry(vector.element())
                    .and_modify(|seen| {
                        if *seen != Some(dimension) {
                            *seen = None;
                        }
                    })
                    .or_insert(Some(dimension));
            }
            ElementProperty::Array(items) => {
                let slot = self.array.get_or_insert(None);
                for item in items {
                    slot.get_or_insert_with(Box::default).observe(item);
                }
            }
            ElementProperty::Object(map) => self
                .object
                .get_or_insert_with(ObservedObject::default)
                .observe_map(map.iter().map(|(k, v)| (k.as_str(), v))),
            // A map with non-string keys is an object without declarable keys.
            ElementProperty::Map(_) => {
                self.object
                    .get_or_insert_with(ObservedObject::default)
                    .count += 1;
            }
            ElementProperty::Vertex(_) | ElementProperty::Edge(_) => {}
        }
    }

    /// One node per observed kind, in a fixed order (`null` last).
    fn members(self) -> Vec<PropertySchema> {
        let mut members = Vec::new();
        if self.boolean {
            members.push(PropertySchema::boolean());
        }
        if self.integer {
            members.push(PropertySchema::integer());
        }
        if self.number {
            members.push(PropertySchema::number());
        }
        if self.string {
            members.push(PropertySchema::string());
        }
        if self.uuid {
            members.push(PropertySchema::uuid());
        }
        for kind in &self.temporal {
            members.push(PropertySchema::temporal(*kind));
        }
        for (element, dimension) in &self.vectors {
            members.push(PropertySchema::vector(*element, *dimension));
        }
        if let Some(items) = self.array {
            members.push(match items {
                Some(items) => PropertySchema::array(items.into_schema()),
                None => PropertySchema::of(JsonType::Array),
            });
        }
        if let Some(object) = self.object {
            members.push(object.into_schema());
        }
        if self.null {
            members.push(PropertySchema::null());
        }
        members
    }

    fn into_schema(self) -> PropertySchema {
        // A `string`, a `uuid` and the temporal types at one position cannot share a `type`
        // array (the format applies to every string): then each kind is an `anyOf` member.
        let split = usize::from(self.string) + usize::from(self.uuid) + self.temporal.len() > 1
            // Likewise lists and the vector elements: each is an `array` with its own keywords.
            || usize::from(self.array.is_some()) + self.vectors.len() > 1;
        let members = self.members();
        if members.len() == 1 {
            return members.into_iter().next().unwrap_or_default();
        }
        if split {
            return PropertySchema::any_of(members);
        }
        // Otherwise one node with a type array; container keywords sit side by side.
        let mut node = PropertySchema {
            types: Some(Vec::new()),
            ..PropertySchema::default()
        };
        for member in members {
            if let Some(types) = &mut node.types {
                types.extend(member.types.into_iter().flatten());
            }
            if member.format.is_some() {
                node.format = member.format;
            }
            if member.items.is_some() {
                node.items = member.items;
            }
            if member.vector.is_some() {
                node.vector = member.vector;
            }
            if !member.properties.is_empty() || !member.required.is_empty() {
                node.properties = member.properties;
                node.required = member.required;
            }
        }
        node
    }
}

/// Builds a schema from observed vertices and edges: the one implementation behind
/// `infer_schema()` on a graph and the `infer_schema()` step on a stream.
#[derive(Debug, Default)]
pub struct SchemaInferer {
    vertices: IndexMap<String, ObservedObject>,
    edges: IndexMap<String, (ObservedObject, Vec<EdgeConnection>)>,
}

impl SchemaInferer {
    /// An inferer that has observed nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// Observes a vertex: its properties count for every one of its labels (an unlabeled
    /// vertex has no label to declare and is skipped).
    pub fn observe_vertex(
        &mut self,
        labels: &[compact_str::CompactString],
        properties: &PropertyMap,
    ) {
        for label in labels {
            self.vertices
                .entry(label.to_string())
                .or_default()
                .observe_map(properties.iter().map(|(k, v)| (k.as_str(), v)));
        }
    }

    /// Observes an edge with its label and the label sets of its endpoints. The connection is
    /// recorded between the endpoints' first (primary) labels, which satisfies the "any label"
    /// topology rule; an unlabeled edge is skipped, and so is the connection of an unlabeled
    /// endpoint.
    pub fn observe_edge(
        &mut self,
        label: Option<&str>,
        source_labels: &[compact_str::CompactString],
        target_labels: &[compact_str::CompactString],
        properties: &PropertyMap,
    ) {
        let Some(label) = label.filter(|l| !l.is_empty()) else {
            return;
        };
        let (observed, connections) = self.edges.entry(label.to_string()).or_default();
        observed.observe_map(properties.iter().map(|(k, v)| (k.as_str(), v)));
        if let (Some(from), Some(to)) = (source_labels.first(), target_labels.first())
            && !connections
                .iter()
                .any(|c| c.from == from.as_str() && c.to == to.as_str())
        {
            connections.push(EdgeConnection::new(from.as_str(), to.as_str()));
        }
    }

    /// The schema of everything observed (mode `none`).
    pub fn finish(self) -> GraphSchema {
        let mut schema = GraphSchema::new();
        for (label, observed) in self.vertices {
            schema
                .vertices
                .insert(label, VertexSchema::new(observed.into_schema()));
        }
        for (label, (observed, connections)) in self.edges {
            let mut edge = EdgeSchema::new(observed.into_schema());
            edge.connections = connections;
            schema.edges.insert(label, edge);
        }
        schema
    }

    /// Infers the schema of every vertex and edge of `storage`; `Err` when the storage cannot
    /// read its elements.
    pub fn infer_from_storage<S: GraphStorage>(
        storage: &S,
    ) -> Result<GraphSchema, crate::error::GraphError> {
        let mut inferer = SchemaInferer::new();
        let empty = PropertyMap::default();
        for vertex in storage.vertices()? {
            inferer.observe_vertex(
                storage.vertex_labels(vertex)?,
                storage.vertex_properties(vertex)?.unwrap_or(&empty),
            );
        }
        for edge in storage.edges()? {
            let Some((from, to)) = storage.edge_endpoints(edge)? else {
                continue;
            };
            inferer.observe_edge(
                storage.edge_label(edge)?,
                storage.vertex_labels(from)?,
                storage.vertex_labels(to)?,
                storage.edge_properties(edge)?.unwrap_or(&empty),
            );
        }
        Ok(inferer.finish())
    }
}
