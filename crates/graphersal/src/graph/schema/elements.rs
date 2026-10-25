use super::node::{JsonType, PropertySchema};

/// The schema of one vertex label: the JSON Schema of its property map.
///
/// In the schema format: `"vertices": {"person": {"schema": {"type": "object", ...}}}`.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct VertexSchema {
    /// The JSON Schema of the vertex's properties: an `object` node (its `properties`,
    /// `required` and `additionalProperties` describe the top-level property keys).
    pub schema: PropertySchema,
}

impl Default for VertexSchema {
    fn default() -> Self {
        Self::new(PropertySchema::object())
    }
}

impl VertexSchema {
    /// A vertex label whose properties follow `schema` (an `object` node).
    pub fn new(schema: PropertySchema) -> Self {
        Self { schema }
    }
}

/// One allowed `(from) -[edge]-> (to)` shape of an edge label: `{"from": "person", "to":
/// "software"}`. `*` matches every vertex label.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct EdgeConnection {
    /// The label of the source vertex (or `*`).
    pub from: String,
    /// The label of the target vertex (or `*`).
    pub to: String,
}

impl EdgeConnection {
    /// The connection `from -> to`.
    pub fn new(from: impl Into<String>, to: impl Into<String>) -> Self {
        Self {
            from: from.into(),
            to: to.into(),
        }
    }
}

/// The schema of one edge label: its allowed connections and the JSON Schema of its property
/// map.
///
/// In the schema format: `"edges": {"knows": {"connections": [{"from": "person", "to":
/// "person"}], "schema": {"type": "object", ...}}}`.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct EdgeSchema {
    /// The allowed endpoint label pairs. Checked in `Closed` mode only, and only when the list
    /// is not empty.
    pub connections: Vec<EdgeConnection>,
    /// The JSON Schema of the edge's properties (an `object` node).
    pub schema: PropertySchema,
}

impl Default for EdgeSchema {
    fn default() -> Self {
        Self::new(PropertySchema::object())
    }
}

impl EdgeSchema {
    /// An edge label whose properties follow `schema` (an `object` node), without
    /// connections.
    pub fn new(schema: PropertySchema) -> Self {
        Self {
            connections: Vec::new(),
            schema,
        }
    }

    /// Adds an allowed connection.
    pub fn with_connection(mut self, from: impl Into<String>, to: impl Into<String>) -> Self {
        self.connections.push(EdgeConnection::new(from, to));
        self
    }
}

/// Whether a label's schema root is an `object` node (the only valid root).
pub(crate) fn is_object_root(schema: &PropertySchema) -> bool {
    schema.types.as_deref() == Some(&[JsonType::Object])
}

/// How strictly a [`GraphSchema`](crate::graph::GraphSchema) is enforced on mutation.
///
/// The vocabulary follows GQL / PG-Schema graph types: an *open* graph type constrains only
/// what it declares, a *closed* one admits nothing else. The mode governs undeclared labels and
/// edge topology, and it is the default `additionalProperties` of every object node that does
/// not state its own (`open`: `true`, `closed`: `false`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum SchemaMode {
    /// No enforcement: the schema is descriptive only (GraphML import still restores declared
    /// logical types from it).
    #[default]
    None,
    /// Open graph type: declared labels and properties are validated; undeclared labels are
    /// allowed unchecked; objects admit undeclared keys unless they say
    /// `"additionalProperties": false`.
    Open,
    /// Closed graph type: every label must be declared, edges must match a declared
    /// connection, and objects admit no undeclared key unless they say
    /// `"additionalProperties": true`.
    Closed,
}

impl SchemaMode {
    /// The mode's name as the schema format spells it: `none`, `open` or `closed`.
    pub fn as_str(&self) -> &'static str {
        match self {
            SchemaMode::None => "none",
            SchemaMode::Open => "open",
            SchemaMode::Closed => "closed",
        }
    }

    /// Parses `none`, `open` or `closed` (case-sensitive).
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "none" => Some(SchemaMode::None),
            "open" => Some(SchemaMode::Open),
            "closed" => Some(SchemaMode::Closed),
            _ => None,
        }
    }

    /// Parses `none`, `open` or `closed` (case-sensitive) like [`Self::from_name`]; anything
    /// else is the schema format's error for an unknown mode
    /// ([`SchemaError::InvalidSchema`](crate::error::SchemaError::InvalidSchema) at `/mode`).
    /// The bindings use it for the mode argument of `set_schema(schema, mode)`.
    pub fn parse(name: &str) -> Result<Self, crate::error::SchemaError> {
        Self::from_name(name).ok_or_else(|| crate::error::SchemaError::InvalidSchema {
            path: "/mode".to_string(),
            reason: format!("unknown mode '{name}'"),
            hint: "The mode is one of \"none\", \"open\", \"closed\" (lowercase); in the DSL also \
                   the tokens SchemaMode.none, SchemaMode.open, SchemaMode.closed."
                .to_string(),
        })
    }

    /// The `additionalProperties` of an object node that does not state its own.
    pub fn default_additional_properties(&self) -> bool {
        !matches!(self, SchemaMode::Closed)
    }
}

impl std::fmt::Display for SchemaMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Identifies whether something concerns a vertex or an edge.
///
/// Closed on purpose: a property graph has exactly two element kinds, so this enum is
/// deliberately *not* `#[non_exhaustive]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ElementKind {
    /// A vertex.
    #[cfg_attr(feature = "serde", serde(rename = "vertex"))]
    Vertex,
    /// An edge.
    #[cfg_attr(feature = "serde", serde(rename = "edge"))]
    Edge,
}

impl ElementKind {
    /// Returns the canonical lowercase name of this element kind (`"vertex"` / `"edge"`).
    ///
    /// This matches the serialized form and the `element_type` wording used by
    /// [`SchemaError`](crate::error::SchemaError) diagnostics.
    #[inline]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Vertex => "vertex",
            Self::Edge => "edge",
        }
    }
}

impl std::fmt::Display for ElementKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
