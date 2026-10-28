use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::File;
use std::str::FromStr;

use compact_str::CompactString;
use quick_xml::escape::{EscapeError, unescape};
use quick_xml::events::{BytesEnd, BytesStart, BytesText, Event};
use quick_xml::reader::Reader;
use quick_xml::writer::Writer;

use crate::error::{GraphError, IOError};
use crate::graph::shape::{MapBuilder, ShapeTable};
use crate::graph::{GraphNodeType, GraphStorage, JsonType, SchemaFormat};
use crate::traversal::memory_budget::UnitBudget;
use crate::value::depth::DEFAULT_MAX_VALUE_DEPTH;
use crate::value::physical::{
    Encoding, PhysicalError, PhysicalValue, encoding_of, lift_declared, lift_untyped, lower,
    read_json, write_json,
};
use crate::value::{ElementProperty, PropertyMap};
use indexmap::IndexMap;

#[derive(Debug, PartialEq, Clone)]
enum GraphmlElementKind {
    Graph,
    Node,
    Edge,
    All,
}

impl FromStr for GraphmlElementKind {
    type Err = ();

    fn from_str(input: &str) -> Result<GraphmlElementKind, Self::Err> {
        match input {
            "graph" => Ok(GraphmlElementKind::Graph),
            "node" => Ok(GraphmlElementKind::Node),
            "edge" => Ok(GraphmlElementKind::Edge),
            "all" => Ok(GraphmlElementKind::All),
            _ => Err(()),
        }
    }
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum AttrType {
    Bool,
    String,
    Float,
    Double,
    Int,
    Long,
}

impl AttrType {
    fn as_str(&self) -> &'static str {
        match self {
            AttrType::Bool => "bool",
            AttrType::String => "string",
            AttrType::Float => "float",
            AttrType::Double => "double",
            AttrType::Int => "int",
            AttrType::Long => "long",
        }
    }
}

impl FromStr for AttrType {
    type Err = ();

    fn from_str(input: &str) -> Result<AttrType, Self::Err> {
        match input {
            "bool" => Ok(AttrType::Bool),
            "string" => Ok(AttrType::String),
            "float" => Ok(AttrType::Float),
            "double" => Ok(AttrType::Double),
            "int" => Ok(AttrType::Int),
            "long" => Ok(AttrType::Long),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone)]
struct Key {
    id: CompactString,
    element: GraphmlElementKind,
    attr_name: CompactString,
    attr_type: AttrType,
    default: Option<ElementProperty>,
}

#[derive(Debug, Clone)]
struct KeyInfo {
    id: CompactString,
    element: GraphmlElementKind,
    attr_name: CompactString,
    attr_type: AttrType,
    desc: Option<CompactString>,
}

#[derive(Debug, Default)]
struct Node {
    id: CompactString,
    label: Option<CompactString>,
}

#[derive(Debug, Default)]
struct Edge {
    /// GraphML's edge `id` attribute is optional; `None` lets the graph assign one.
    id: Option<CompactString>,
    source: CompactString,
    target: CompactString,
    label: Option<CompactString>,
}

/// GraphML importer: reads a GraphML file/stream into a [`GraphStorage`].
///
/// GraphML carries no schema: logical types are restored from the TARGET graph's stored schema
/// only, so set the schema first, then import. A property the stored schema declares is lifted
/// to its declared type (a canonical UUID string to a `uuid`, the JSON text of a declared
/// `array`/`object` to that container); an undeclared one keeps the type of its `attr.type` (a
/// `string` stays a string, whatever it looks like). Graph-level `<data>` values are ignored.
pub struct GraphMLImport {}

impl GraphMLImport {
    /// Imports GraphML from the file at `file`.
    pub fn start<S: GraphStorage>(&self, graph: &mut S, file: String) -> Result<(), IOError> {
        let f = File::open(&file).map_err(|err| IOError::FileRead {
            path: file.clone(),
            source: err,
        })?;
        let reader = Reader::from_reader(std::io::BufReader::with_capacity(64 * 1024, f));
        self.import_reader(graph, reader)
    }

    /// Imports GraphML from an already-open [`Reader`].
    pub fn start_reader<S: GraphStorage, R: std::io::BufRead>(
        &self,
        graph: &mut S,
        reader: Reader<R>,
    ) -> Result<(), IOError> {
        self.import_reader(graph, reader)
    }

    /// Parses GraphML from `reader`, writing vertices/edges/properties into `g`.
    ///
    /// The import is ONE unit of work on the live graph `g` (`book/src/users_guide/transactions.md`):
    /// on an atomic storage a failure leaves nothing behind, and its commit is
    /// captured and runs the commit hooks like a traversal (a veto is
    /// [`IOError::Graph`] holding [`GraphError::CommitRejected`]).
    /// A graph that is not published yet is loaded outside units by the constructors
    /// ([`GraphSource::from_graphml`](crate::GraphSource::from_graphml),
    /// [`GraphSource::from_graphml_reader`](crate::GraphSource::from_graphml_reader)).
    ///
    /// Under a memory budget (a `memory.limit` default of the graph's
    /// [`ExecutionPolicy`](crate::graph::ExecutionPolicy)) the import checks it every 4096
    /// elements and at its end: an import that grows the memory in use over it is rolled back
    /// and fails with [`IOError::Graph`] holding [`GraphError::ResourceLimitExceeded`]
    /// (operation `"GraphML import"`). Without a budget nothing is checked.
    pub fn import_reader<S: GraphStorage, R: std::io::BufRead>(
        &self,
        g: &mut S,
        reader: Reader<R>,
    ) -> Result<(), IOError> {
        self.import_reader_with(g, reader, None)
    }

    /// [`Self::import_reader`] whose memory budget also honours a host's preset options
    /// (`presets`, such as a script session's: they win over the policy's default unless the
    /// policy locks the key).
    pub(crate) fn import_reader_with<S: GraphStorage, R: std::io::BufRead>(
        &self,
        g: &mut S,
        reader: Reader<R>,
        presets: Option<&std::collections::HashMap<CompactString, crate::value::ElementProperty>>,
    ) -> Result<(), IOError> {
        let mut budget = UnitBudget::start(&*g, presets, GRAPHML_IMPORT)?;
        let mark = g.begin_unit()?;
        let imported = crate::graph::storage::close_on_unwind(
            g,
            |g| {
                self.import_outside_units(g, reader, &mut budget)?;
                crate::traversal::memory_budget::check(&budget, &*g).map_err(IOError::Graph)
            },
            |g| crate::graph::storage::rollback_unwound(g, mark),
        );
        match imported {
            Ok(()) => g.commit_unit(mark).map_err(IOError::Graph),
            Err(err) => Err(crate::graph::storage::rollback_failed_unit(g, mark, err)),
        }
    }

    /// The import itself, without opening a unit: for a graph nobody else sees yet (nothing is
    /// recorded, captured or reported to hooks; on error the caller drops the graph). `budget`
    /// is the memory budget of a live import (checked every few thousand elements); `None`
    /// for a load.
    pub(crate) fn import_outside_units<S: GraphStorage, R: std::io::BufRead>(
        &self,
        g: &mut S,
        mut reader: Reader<R>,
        budget: &mut Option<UnitBudget>,
    ) -> Result<(), IOError> {
        let mut buf = Vec::new();

        let mut keys: HashMap<CompactString, Key> = HashMap::new();
        let mut current_node: GraphmlElementKind = GraphmlElementKind::All;
        let mut current_key_id: Option<CompactString> = None;
        let mut in_default = false;
        let mut has_defaults = false;

        let mut node: Node = Node::default();
        let mut edge: Edge = Edge::default();
        // The property map being read, built `Shaped` (see `MapBuilder`) in the importer's own
        // shape tables, one per element kind (the graph adopts the shapes).
        let mut data = MapBuilder::default();
        let mut node_shapes = ShapeTable::new();
        let mut edge_shapes = ShapeTable::new();
        let mut data_key: Option<CompactString> = None;
        let mut node_map: HashMap<CompactString, S::VertexId> = HashMap::with_capacity(65536);

        // quick-xml reports one run of character data as several `Text` and `GeneralRef` (entity
        // or character reference) events. They are collected here, still escaped, and handled as
        // one text once the run ends, so `a &amp; b` imports as `a & b`.
        let mut text = String::new();
        let mut text_pending = false;

        loop {
            let event = reader
                .read_event_into(&mut buf)
                .map_err(|e| IOError::XmlParse {
                    position: reader.buffer_position(),
                    message: e.to_string(),
                })?;

            match event {
                Event::Text(e) => {
                    text.push_str(&e);
                    text_pending = true;
                    buf.clear();
                    continue;
                }
                Event::GeneralRef(e) => {
                    text.push('&');
                    text.push_str(&e);
                    text.push(';');
                    text_pending = true;
                    buf.clear();
                    continue;
                }
                _ => (),
            }

            if text_pending {
                text_pending = false;
                let content = unescape(&text).map_err(|err| IOError::XmlParse {
                    position: reader.buffer_position(),
                    message: err.to_string(),
                })?;

                if in_default {
                    if let Some(ref k_id) = current_key_id
                        && let Some(tmp_key) = keys.get_mut(k_id)
                    {
                        let prop_val = parse_attr_value(
                            &tmp_key.id,
                            &tmp_key.attr_name,
                            &tmp_key.attr_type,
                            &content,
                        )?;
                        tmp_key.default = Some(prop_val);
                        has_defaults = true;
                    }
                } else if let Some(ref k_id) = data_key {
                    if let Some(tmp_key) = keys.get(k_id) {
                        let prop_val = parse_attr_value(
                            &tmp_key.id,
                            &tmp_key.attr_name,
                            &tmp_key.attr_type,
                            &content,
                        )?;
                        data.insert(tmp_key.attr_name.clone(), prop_val);
                    }
                    data_key = None;
                }
                text.clear();
            }

            match event {
                Event::Eof => break,

                Event::Start(e) => {
                    self.process_start(
                        &reader,
                        &e,
                        &mut keys,
                        &mut current_node,
                        &mut current_key_id,
                        &mut in_default,
                        &mut node,
                        &mut edge,
                        &mut data,
                        &mut data_key,
                    )?;
                }

                Event::Empty(e) => {
                    self.process_start(
                        &reader,
                        &e,
                        &mut keys,
                        &mut current_node,
                        &mut current_key_id,
                        &mut in_default,
                        &mut node,
                        &mut edge,
                        &mut data,
                        &mut data_key,
                    )?;
                    match e.name().as_ref() {
                        "node" => {
                            if current_node == GraphmlElementKind::Node {
                                self.finalize_node(
                                    g,
                                    &mut node,
                                    &mut data,
                                    &mut node_shapes,
                                    &mut node_map,
                                    &keys,
                                    has_defaults,
                                    budget,
                                )?;
                                current_node = GraphmlElementKind::All;
                            }
                        }
                        "edge" => {
                            if current_node == GraphmlElementKind::Edge {
                                self.finalize_edge(
                                    g,
                                    &mut edge,
                                    &mut data,
                                    &mut edge_shapes,
                                    &node_map,
                                    &keys,
                                    has_defaults,
                                    budget,
                                )?;
                                current_node = GraphmlElementKind::All;
                            }
                        }
                        "key" => {
                            current_key_id = None;
                        }
                        "data" => {
                            insert_empty_data(&keys, &mut data, &mut data_key)?;
                        }
                        _ => (),
                    }
                }

                Event::End(e) => match e.name().as_ref() {
                    "data" => {
                        insert_empty_data(&keys, &mut data, &mut data_key)?;
                    }
                    "default" => {
                        in_default = false;
                    }
                    "key" => {
                        current_key_id = None;
                    }
                    "node" => {
                        if current_node == GraphmlElementKind::Node {
                            self.finalize_node(
                                g,
                                &mut node,
                                &mut data,
                                &mut node_shapes,
                                &mut node_map,
                                &keys,
                                has_defaults,
                                budget,
                            )?;
                            current_node = GraphmlElementKind::All;
                        }
                    }
                    "edge" if current_node == GraphmlElementKind::Edge => {
                        self.finalize_edge(
                            g,
                            &mut edge,
                            &mut data,
                            &mut edge_shapes,
                            &node_map,
                            &keys,
                            has_defaults,
                            budget,
                        )?;
                        current_node = GraphmlElementKind::All;
                    }
                    _ => (),
                },

                _ => (),
            }
            buf.clear();
        }

        g.shrink_to_fit()?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn process_start<R: std::io::BufRead>(
        &self,
        reader: &Reader<R>,
        e: &BytesStart,
        keys: &mut HashMap<CompactString, Key>,
        current_node: &mut GraphmlElementKind,
        current_key_id: &mut Option<CompactString>,
        in_default: &mut bool,
        node: &mut Node,
        edge: &mut Edge,
        data: &mut MapBuilder,
        data_key: &mut Option<CompactString>,
    ) -> Result<(), IOError> {
        match e.name().as_ref() {
            "key" => {
                let (id, element_raw, attr_name, attr_type_raw) = extract_key_attrs(reader, e)?;
                let element = GraphmlElementKind::from_str(element_raw.as_str()).map_err(|_| {
                    IOError::InvalidAttributeValue {
                        key: id.to_string(),
                        attribute: "for".to_string(),
                        value: element_raw.to_string(),
                        expected_type: "graph|node|edge|all".to_string(),
                    }
                })?;
                let attr_type = AttrType::from_str(attr_type_raw.as_str()).map_err(|_| {
                    IOError::InvalidAttributeValue {
                        key: id.to_string(),
                        attribute: "attr.type".to_string(),
                        value: attr_type_raw.to_string(),
                        expected_type: "bool|string|float|double|int|long".to_string(),
                    }
                })?;

                let key = Key {
                    id: id.clone(),
                    element,
                    attr_name,
                    attr_type,
                    default: None,
                };
                *current_key_id = Some(id.clone());
                keys.insert(id, key);
            }
            "default" => {
                *in_default = true;
            }
            "node" => {
                *current_node = GraphmlElementKind::Node;
                node.id = extract_attr(reader, e, "node", "id")?;
                node.label = None;
                data.clear();
            }
            "edge" => {
                *current_node = GraphmlElementKind::Edge;
                let (id, source, target) = extract_edge_attrs(reader, e)?;
                edge.id = id;
                edge.source = source;
                edge.target = target;
                edge.label = None;
                data.clear();
            }
            "data" => {
                *data_key = Some(extract_attr(reader, e, "data", "key")?);
            }
            _ => (),
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)] // the import state is passed piecewise to keep borrows disjoint
    fn finalize_node<S: GraphStorage>(
        &self,
        g: &mut S,
        node: &mut Node,
        data: &mut MapBuilder,
        shapes: &mut ShapeTable,
        node_map: &mut HashMap<CompactString, S::VertexId>,
        keys: &HashMap<CompactString, Key>,
        has_defaults: bool,
        budget: &mut Option<UnitBudget>,
    ) -> Result<(), IOError> {
        if has_defaults {
            for key in keys.values() {
                if matches!(
                    key.element,
                    GraphmlElementKind::Node | GraphmlElementKind::All
                ) && let Some(ref def_val) = key.default
                    && !data.contains_key(key.attr_name.as_str())
                {
                    data.insert(key.attr_name.clone(), def_val.clone());
                }
            }
        }

        if let Some(label) = data.get("labelV") {
            node.label = label.as_str().map(CompactString::new);
            data.shift_remove("labelV");
        }

        let vertex_data = data.finish(shapes);

        let v_id = std::mem::take(&mut node.id);
        // Multiple labels are encoded as a single "::"-delimited `labelV` value (D9); split it
        // back into the vertex's ordered label set. A single-label vertex has no delimiter and
        // round-trips to a one-element vec, unchanged from before multi-label support.
        let labels = match node.label.take() {
            Some(joined) => joined
                .split("::")
                .map(CompactString::new)
                .collect::<Vec<_>>(),
            None => Vec::new(),
        };
        let mut vertex_data = vertex_data;
        lift_declared_properties(g, GraphNodeType::Vertex, &labels, &mut vertex_data)?;
        let index = g
            .insert_vertex(Some(v_id.clone()), labels, vertex_data)
            .map_err(IOError::Graph)?;

        node_map.insert(v_id, index);
        crate::traversal::memory_budget::tick(budget, &*g).map_err(IOError::Graph)
    }

    #[allow(clippy::too_many_arguments)] // see finalize_node
    fn finalize_edge<S: GraphStorage>(
        &self,
        g: &mut S,
        edge: &mut Edge,
        data: &mut MapBuilder,
        shapes: &mut ShapeTable,
        node_map: &HashMap<CompactString, S::VertexId>,
        keys: &HashMap<CompactString, Key>,
        has_defaults: bool,
        budget: &mut Option<UnitBudget>,
    ) -> Result<(), IOError> {
        if has_defaults {
            for key in keys.values() {
                if matches!(
                    key.element,
                    GraphmlElementKind::Edge | GraphmlElementKind::All
                ) && let Some(ref def_val) = key.default
                    && !data.contains_key(key.attr_name.as_str())
                {
                    data.insert(key.attr_name.clone(), def_val.clone());
                }
            }
        }

        if let Some(label) = data.get("labelE") {
            edge.label = label.as_str().map(CompactString::new);
            data.shift_remove("labelE");
        }

        let source_idx = node_map.get(&edge.source).ok_or_else(|| {
            IOError::Graph(GraphError::VertexNotFound {
                id: edge.source.to_string(),
            })
        })?;

        let target_idx = node_map.get(&edge.target).ok_or_else(|| {
            IOError::Graph(GraphError::VertexNotFound {
                id: edge.target.to_string(),
            })
        })?;

        let edge_data = data.finish(shapes);
        // An absent or empty `id` lets the graph assign one.
        let edge_id = edge.id.take().filter(|id| !id.is_empty());
        let edge_label = edge.label.take();
        let mut edge_data = edge_data;
        if let Some(label) = &edge_label {
            lift_declared_properties(
                g,
                GraphNodeType::Edge,
                std::slice::from_ref(label),
                &mut edge_data,
            )?;
        }

        g.insert_edge(edge_id, edge_label, *source_idx, *target_idx, edge_data)
            .map_err(IOError::Graph)?;

        crate::traversal::memory_budget::tick(budget, &*g).map_err(IOError::Graph)
    }
}

/// The operation [`GraphError::ResourceLimitExceeded`] names for a GraphML import.
pub(crate) const GRAPHML_IMPORT: &str = "GraphML import";

/// GraphML exporter and its output options. Build it with [`GraphMLExport::new`].
///
/// The document carries the elements only, never the graph's schema: GraphML is an exchange
/// format other clients read, and they could not use it. Save the schema separately (its
/// JSON in the schema format) and set it on the target before importing, so that declared
/// logical types (`uuid`, `array`, `object`) come back exactly.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct GraphMLExport {
    /// Pretty-print (indent) the output XML.
    pub beautify: bool,
}

impl GraphMLExport {
    /// A new exporter with default options (`beautify: false`).
    pub fn new() -> Self {
        Self { beautify: false }
    }

    /// Sets [`Self::beautify`].
    pub fn with_beautify(mut self, beautify: bool) -> Self {
        self.beautify = beautify;
        self
    }

    /// Exports `graph` as GraphML to the file at `path`.
    pub fn export_file<S: GraphStorage>(&self, graph: &S, path: &str) -> Result<(), IOError> {
        let f = File::create(path).map_err(|err| IOError::FileRead {
            path: path.to_string(),
            source: err,
        })?;
        let writer = std::io::BufWriter::with_capacity(128 * 1024, f);
        self.export_writer(graph, writer)
    }

    /// Exports `g` as GraphML to `writer`.
    pub fn export_writer<S: GraphStorage, W: std::io::Write>(
        &self,
        g: &S,
        writer: W,
    ) -> Result<(), IOError> {
        // Step 1: one pass over the data to build the node/edge keys. Per label, a key whose
        // values all have one type gets that type's attr.type, mixed types get `string`; the
        // labels' types are then promoted into one key type.
        let empty = PropertyMap::default();
        let mut node_types: IndexMap<CompactString, IndexMap<CompactString, Option<AttrType>>> =
            IndexMap::new();
        for vertex in g.vertices()? {
            let labels = g.vertex_labels(vertex)?;
            let properties = g.vertex_properties(vertex)?.unwrap_or(&empty);
            if labels.is_empty() {
                observe_key_types(
                    node_types.entry(CompactString::new("")).or_default(),
                    properties,
                );
            }
            for label in labels {
                observe_key_types(node_types.entry(label.clone()).or_default(), properties);
            }
        }
        let mut edge_types: IndexMap<CompactString, IndexMap<CompactString, Option<AttrType>>> =
            IndexMap::new();
        for edge in g.edges()? {
            let label = CompactString::new(g.edge_label(edge)?.unwrap_or(""));
            observe_key_types(
                edge_types.entry(label).or_default(),
                g.edge_properties(edge)?.unwrap_or(&empty),
            );
        }

        let mut node_keys: IndexMap<CompactString, KeyInfo> = IndexMap::new();
        let mut edge_keys: IndexMap<CompactString, KeyInfo> = IndexMap::new();

        // Check if any vertex has a label
        let has_node_labels = !node_types.is_empty();
        if has_node_labels {
            node_keys.insert(
                CompactString::new("labelV"),
                KeyInfo {
                    id: CompactString::new("labelV"),
                    element: GraphmlElementKind::Node,
                    attr_name: CompactString::new("labelV"),
                    attr_type: AttrType::String,
                    desc: None,
                },
            );
        }

        for keys in node_types.values() {
            for (prop_name, attr_type) in keys {
                let attr_type = attr_type.unwrap_or(AttrType::String);
                if let Some(existing) = node_keys.get_mut(prop_name.as_str()) {
                    existing.attr_type = promote_type(existing.attr_type, attr_type);
                } else {
                    node_keys.insert(
                        prop_name.clone(),
                        KeyInfo {
                            id: prop_name.clone(),
                            element: GraphmlElementKind::Node,
                            attr_name: prop_name.clone(),
                            attr_type,
                            desc: None,
                        },
                    );
                }
            }
        }

        // Check if any edge has a label
        let has_edge_labels = !edge_types.is_empty();
        if has_edge_labels {
            let key_id = if node_keys.contains_key("labelE") {
                CompactString::new("e_labelE")
            } else {
                CompactString::new("labelE")
            };
            edge_keys.insert(
                CompactString::new("labelE"),
                KeyInfo {
                    id: key_id,
                    element: GraphmlElementKind::Edge,
                    attr_name: CompactString::new("labelE"),
                    attr_type: AttrType::String,
                    desc: None,
                },
            );
        }

        for keys in edge_types.values() {
            for (prop_name, attr_type) in keys {
                let attr_type = attr_type.unwrap_or(AttrType::String);
                if let Some(existing) = edge_keys.get_mut(prop_name.as_str()) {
                    existing.attr_type = promote_type(existing.attr_type, attr_type);
                } else {
                    let key_id = if node_keys.contains_key(prop_name.as_str()) {
                        CompactString::from(format!("e_{prop_name}"))
                    } else {
                        prop_name.clone()
                    };
                    edge_keys.insert(
                        prop_name.clone(),
                        KeyInfo {
                            id: key_id,
                            element: GraphmlElementKind::Edge,
                            attr_name: prop_name.clone(),
                            attr_type,
                            desc: None,
                        },
                    );
                }
            }
        }
        // Step 2: Streaming XML Write in a single pass
        let mut xml = if self.beautify {
            Writer::new_with_indent(writer, b' ', 2)
        } else {
            Writer::new(writer)
        };

        let mut root = BytesStart::new("graphml");
        root.push_attribute(("xmlns", "http://graphml.graphdrawing.org/xmlns"));
        xml.write_event(Event::Start(root))
            .map_err(|e| IOError::XmlParse {
                position: 0,
                message: e.to_string(),
            })?;

        for key_info in node_keys.values() {
            let mut key_elem = BytesStart::new("key");
            key_elem.push_attribute(("id", key_info.id.as_str()));
            key_elem.push_attribute((
                "for",
                match key_info.element {
                    GraphmlElementKind::Node => "node",
                    GraphmlElementKind::Edge => "edge",
                    GraphmlElementKind::Graph => "graph",
                    GraphmlElementKind::All => "all",
                },
            ));
            key_elem.push_attribute(("attr.name", key_info.attr_name.as_str()));
            key_elem.push_attribute(("attr.type", key_info.attr_type.as_str()));

            if let Some(ref desc) = key_info.desc {
                xml.write_event(Event::Start(key_elem))
                    .map_err(|e| IOError::XmlParse {
                        position: 0,
                        message: e.to_string(),
                    })?;
                let desc_elem = BytesStart::new("desc");
                xml.write_event(Event::Start(desc_elem))
                    .map_err(|e| IOError::XmlParse {
                        position: 0,
                        message: e.to_string(),
                    })?;
                xml.write_event(Event::Text(BytesText::new(desc)))
                    .map_err(|e| IOError::XmlParse {
                        position: 0,
                        message: e.to_string(),
                    })?;
                xml.write_event(Event::End(BytesEnd::new("desc")))
                    .map_err(|e| IOError::XmlParse {
                        position: 0,
                        message: e.to_string(),
                    })?;
                xml.write_event(Event::End(BytesEnd::new("key")))
                    .map_err(|e| IOError::XmlParse {
                        position: 0,
                        message: e.to_string(),
                    })?;
            } else {
                xml.write_event(Event::Empty(key_elem))
                    .map_err(|e| IOError::XmlParse {
                        position: 0,
                        message: e.to_string(),
                    })?;
            }
        }

        for key_info in edge_keys.values() {
            let mut key_elem = BytesStart::new("key");
            key_elem.push_attribute(("id", key_info.id.as_str()));
            key_elem.push_attribute((
                "for",
                match key_info.element {
                    GraphmlElementKind::Node => "node",
                    GraphmlElementKind::Edge => "edge",
                    GraphmlElementKind::Graph => "graph",
                    GraphmlElementKind::All => "all",
                },
            ));
            key_elem.push_attribute(("attr.name", key_info.attr_name.as_str()));
            key_elem.push_attribute(("attr.type", key_info.attr_type.as_str()));

            if let Some(ref desc) = key_info.desc {
                xml.write_event(Event::Start(key_elem))
                    .map_err(|e| IOError::XmlParse {
                        position: 0,
                        message: e.to_string(),
                    })?;
                let desc_elem = BytesStart::new("desc");
                xml.write_event(Event::Start(desc_elem))
                    .map_err(|e| IOError::XmlParse {
                        position: 0,
                        message: e.to_string(),
                    })?;
                xml.write_event(Event::Text(BytesText::new(desc)))
                    .map_err(|e| IOError::XmlParse {
                        position: 0,
                        message: e.to_string(),
                    })?;
                xml.write_event(Event::End(BytesEnd::new("desc")))
                    .map_err(|e| IOError::XmlParse {
                        position: 0,
                        message: e.to_string(),
                    })?;
                xml.write_event(Event::End(BytesEnd::new("key")))
                    .map_err(|e| IOError::XmlParse {
                        position: 0,
                        message: e.to_string(),
                    })?;
            } else {
                xml.write_event(Event::Empty(key_elem))
                    .map_err(|e| IOError::XmlParse {
                        position: 0,
                        message: e.to_string(),
                    })?;
            }
        }

        let mut graph_elem = BytesStart::new("graph");
        graph_elem.push_attribute(("id", "G"));
        graph_elem.push_attribute(("edgedefault", "directed"));
        xml.write_event(Event::Start(graph_elem))
            .map_err(|e| IOError::XmlParse {
                position: 0,
                message: e.to_string(),
            })?;

        for v_idx in g.vertices()? {
            if let Some(v_id) = g.vertex_id(v_idx)? {
                // Multiple labels are joined into a single "::"-delimited `labelV` value (D9). A
                // single-label vertex writes an unqualified value with no delimiter, byte-for-byte
                // identical to the pre-multi-label output.
                let v_labels = g.vertex_labels(v_idx)?;
                let joined_label = if v_labels.is_empty() {
                    None
                } else {
                    Some(
                        v_labels
                            .iter()
                            .map(|l| l.as_str())
                            .collect::<Vec<_>>()
                            .join("::"),
                    )
                };
                let default_props = PropertyMap::default();
                let v_props = g.vertex_properties(v_idx)?.unwrap_or(&default_props);

                let mut node_elem = BytesStart::new("node");
                node_elem.push_attribute(("id", v_id));

                let has_data = joined_label.is_some() || !v_props.is_empty();
                if !has_data {
                    let _ = xml.write_event(Event::Empty(node_elem));
                } else {
                    let _ = xml.write_event(Event::Start(node_elem));

                    if let Some(lbl) = &joined_label
                        && let Some(k_info) = node_keys.get("labelV")
                    {
                        let mut data_elem = BytesStart::new("data");
                        data_elem.push_attribute(("key", k_info.id.as_str()));
                        let _ = xml.write_event(Event::Start(data_elem));
                        let _ = xml.write_event(Event::Text(BytesText::new(lbl.as_str())));
                        let _ = xml.write_event(Event::End(BytesEnd::new("data")));
                    }

                    for (p_key, p_val) in v_props {
                        if p_key == "labelV" {
                            continue;
                        }
                        if let Some(k_info) = node_keys.get(p_key.as_str())
                            && let Some(text) = physical_text(p_key, p_val)?
                        {
                            let mut data_elem = BytesStart::new("data");
                            data_elem.push_attribute(("key", k_info.id.as_str()));
                            let _ = xml.write_event(Event::Start(data_elem));
                            let _ = xml.write_event(Event::Text(BytesText::new(&text)));
                            let _ = xml.write_event(Event::End(BytesEnd::new("data")));
                        }
                    }

                    let _ = xml.write_event(Event::End(BytesEnd::new("node")));
                }
            }
        }

        for e_idx in g.edges()? {
            if let Some((src_idx, tgt_idx)) = g.edge_endpoints(e_idx)?
                && let (Some(src_id), Some(tgt_id)) = (g.vertex_id(src_idx)?, g.vertex_id(tgt_idx)?)
            {
                let e_label = g.edge_label(e_idx)?;
                let e_props_opt = g.edge_properties(e_idx)?;

                let mut edge_elem = BytesStart::new("edge");
                // GraphML's edge `id` is optional: an edge without a stored id omits it.
                if let Some(e_id) = g.edge_id(e_idx)? {
                    edge_elem.push_attribute(("id", e_id));
                }
                edge_elem.push_attribute(("source", src_id));
                edge_elem.push_attribute(("target", tgt_id));

                let has_props =
                    e_label.is_some() || e_props_opt.as_ref().is_some_and(|p| !p.is_empty());

                if !has_props {
                    let _ = xml.write_event(Event::Empty(edge_elem));
                } else {
                    let _ = xml.write_event(Event::Start(edge_elem));

                    if let Some(lbl) = e_label
                        && let Some(k_info) = edge_keys.get("labelE")
                    {
                        let mut data_elem = BytesStart::new("data");
                        data_elem.push_attribute(("key", k_info.id.as_str()));
                        let _ = xml.write_event(Event::Start(data_elem));
                        let _ = xml.write_event(Event::Text(BytesText::new(lbl)));
                        let _ = xml.write_event(Event::End(BytesEnd::new("data")));
                    }

                    if let Some(e_props) = e_props_opt {
                        for (p_key, p_val) in e_props {
                            if p_key == "labelE" {
                                continue;
                            }
                            if let Some(k_info) = edge_keys.get(p_key.as_str())
                                && let Some(text) = physical_text(p_key, p_val)?
                            {
                                let mut data_elem = BytesStart::new("data");
                                data_elem.push_attribute(("key", k_info.id.as_str()));
                                let _ = xml.write_event(Event::Start(data_elem));
                                let _ = xml.write_event(Event::Text(BytesText::new(&text)));
                                let _ = xml.write_event(Event::End(BytesEnd::new("data")));
                            }
                        }
                    }

                    let _ = xml.write_event(Event::End(BytesEnd::new("edge")));
                }
            }
        }

        xml.write_event(Event::End(BytesEnd::new("graph")))
            .map_err(|e| IOError::XmlParse {
                position: 0,
                message: e.to_string(),
            })?;

        xml.write_event(Event::End(BytesEnd::new("graphml")))
            .map_err(|e| IOError::XmlParse {
                position: 0,
                message: e.to_string(),
            })?;

        Ok(())
    }
}

/// Maps a type onto the GraphML `attr.type` of its physical encoding.
///
/// `int64` maps to `long`, because GraphML's `int` is 32-bit. A `uuid` is carried as its
/// canonical string, and a container (which has no leaf encoding, and GraphML no container
/// attribute type) as its JSON text, so both map to `string`.
fn encoding_to_attr_type(ty: &crate::graph::ValueType) -> AttrType {
    match encoding_of(ty) {
        Some(Encoding::Boolean) => AttrType::Bool,
        Some(Encoding::Int64) => AttrType::Long,
        Some(Encoding::Float64) => AttrType::Double,
        Some(Encoding::String) | Some(Encoding::Null) | None => AttrType::String,
    }
}

/// Records the `attr.type` of each non-null property value of one element of a label: the
/// type seen, or `string` once two different types were seen (`None`: only nulls so far).
fn observe_key_types(
    keys: &mut IndexMap<CompactString, Option<AttrType>>,
    properties: &PropertyMap,
) {
    for (key, value) in properties {
        let slot = keys.entry(key.clone()).or_insert(None);
        let Some(ty) = crate::graph::schema::type_of(value) else {
            continue;
        };
        if ty == crate::graph::ValueType::Null {
            continue;
        }
        let attr = encoding_to_attr_type(&ty);
        *slot = Some(match *slot {
            Some(previous) if previous != attr => AttrType::String,
            _ => attr,
        });
    }
}

fn promote_type(a: AttrType, b: AttrType) -> AttrType {
    if a == b {
        return a;
    }
    match (a, b) {
        (AttrType::Int, AttrType::Long) | (AttrType::Long, AttrType::Int) => AttrType::Long,
        (AttrType::Double, _) | (_, AttrType::Double) => AttrType::Double,
        (AttrType::Float, AttrType::Int | AttrType::Long)
        | (AttrType::Int | AttrType::Long, AttrType::Float) => AttrType::Double,
        _ => AttrType::String,
    }
}

/// The text a property value is written as inside a `<data>` element, or `None` for `Null`,
/// which is written as an absent `<data>`.
///
/// Every value is written as its own physical text, whatever its key's promoted `attr.type`:
/// `true`/`false`, a decimal integer, a float's shortest round-trip rendering, a string as is
/// (the XML writer escapes it), a `uuid` canonically and a container as JSON text.
fn physical_text<'a>(
    key: &str,
    value: &'a ElementProperty,
) -> Result<Option<Cow<'a, str>>, IOError> {
    let non_finite = |value: f64| IOError::NonFiniteFloat {
        key: key.to_string(),
        value: value.to_string(),
    };
    Ok(match lower(value) {
        PhysicalValue::Null => None,
        PhysicalValue::Bool(b) => Some(Cow::Borrowed(if b { "true" } else { "false" })),
        PhysicalValue::Int(i) => Some(Cow::Owned(i.to_string())),
        PhysicalValue::Float(f) if !f.is_finite() => return Err(non_finite(f)),
        PhysicalValue::Float(f) => Some(Cow::Owned(f.to_string())),
        PhysicalValue::String(text) => Some(text),
        container @ (PhysicalValue::Array(_) | PhysicalValue::Object(_)) => {
            let mut text = String::new();
            write_json(&container, &mut text).map_err(|err| match err {
                PhysicalError::NonFiniteFloat(f) => non_finite(f),
                other => IOError::InvalidAttributeValue {
                    key: key.to_string(),
                    attribute: key.to_string(),
                    value: other.to_string(),
                    expected_type: "string".to_string(),
                },
            })?;
            Some(Cow::Owned(text))
        }
    })
}

/// Whether a declared node admits an `array`/`object` (also through `anyOf`).
fn contains_container(kinds: &[(JsonType, Option<SchemaFormat>)]) -> bool {
    kinds
        .iter()
        .any(|(kind, _)| matches!(kind, JsonType::Array | JsonType::Object))
}

/// Whether a declared node admits a value that GraphML writes as plain text (a `string` or a
/// `uuid` or a temporal value).
fn admits_text(kinds: &[(JsonType, Option<SchemaFormat>)]) -> bool {
    kinds.iter().any(|(kind, _)| *kind == JsonType::String)
}

/// Restores logical types on an imported element's properties from the graph's stored schema.
///
/// Each value arrives as the same-name logical value of its `attr.type`. A property the stored
/// schema declares for `labels` (first declaring label wins) is lifted to the declared type,
/// whatever the schema's mode; an undeclared property is left unchanged (CLAUDE.md rule 7: a
/// string is never parsed on a guess).
///
/// A declared `array`/`object` (also as a union member) is exported as its JSON text, so a
/// string value of such a property is read as JSON first and then lifted. A union that admits
/// both a container and a text type (`string`/`uuid`) is rejected
/// ([`IOError::AmbiguousContainerImport`]): its text could be either.
fn lift_declared_properties<S: GraphStorage>(
    g: &S,
    kind: GraphNodeType,
    labels: &[CompactString],
    data: &mut PropertyMap,
) -> Result<(), IOError> {
    for (key, slot) in data.iter_mut() {
        let Some(declared) = g.declared_property_schema(kind, labels, key)? else {
            continue;
        };
        // A node without `type` (`{}`, or `anyOf` of such) declares no type to restore.
        let Some(kinds) = declared.declared_kinds() else {
            continue;
        };
        if contains_container(&kinds) {
            if admits_text(&kinds) {
                return Err(IOError::AmbiguousContainerImport {
                    key: key.to_string(),
                    declared: declared.non_null_type_name(),
                });
            }
            if let ElementProperty::String(text) = slot {
                let parsed = read_json(text, DEFAULT_MAX_VALUE_DEPTH).map_err(|err| {
                    IOError::InvalidJsonValue {
                        key: key.to_string(),
                        declared: declared.non_null_type_name(),
                        reason: err.to_string(),
                    }
                })?;
                *slot = lift_untyped(parsed);
            }
        }
        let value = std::mem::replace(slot, ElementProperty::Null);
        match lift_declared(value.clone(), &declared) {
            Ok(lifted) => *slot = lifted,
            Err(_) => {
                // The value's own physical text is the `<data>` text it was parsed from.
                let original = physical_text(key, &value)?
                    .map(Cow::into_owned)
                    .unwrap_or_default();
                return Err(IOError::InvalidAttributeValue {
                    key: key.to_string(),
                    attribute: key.to_string(),
                    value: original,
                    expected_type: declared.non_null_type_name(),
                });
            }
        }
    }
    Ok(())
}

/// Finishes a `<data>` element that carried no text event (`<data key="k"></data>` or
/// `<data key="k"/>`): on a `string` key the value is the empty string, an explicit value that
/// wins over a `<default>`. On any other key type an empty element stays "no value" (an empty
/// text is not a number or a boolean). Always clears `data_key`.
fn insert_empty_data(
    keys: &HashMap<CompactString, Key>,
    data: &mut MapBuilder,
    data_key: &mut Option<CompactString>,
) -> Result<(), IOError> {
    if let Some(k_id) = data_key.take()
        && let Some(key) = keys.get(&k_id)
        && matches!(key.attr_type, AttrType::String)
    {
        let value = parse_attr_value(&key.id, &key.attr_name, &key.attr_type, "")?;
        data.insert(key.attr_name.clone(), value);
    }
    Ok(())
}

/// Parses a `<data>`/`<default>` text as the physical value of its key's `attr.type`, and lifts
/// it without a declared type: `bool` becomes a `boolean`, `int`/`long` an `int64`,
/// `float`/`double` a `float64` and `string` a `string`. String content is never inspected, so a
/// UUID-looking string stays a `string` until a declaration lifts it (CLAUDE.md rule 7).
fn parse_attr_value(
    key_id: &str,
    attr_name: &str,
    attr_type: &AttrType,
    content: &str,
) -> Result<ElementProperty, IOError> {
    let invalid = |expected_type: &str| IOError::InvalidAttributeValue {
        key: key_id.to_string(),
        attribute: attr_name.to_string(),
        value: content.to_string(),
        expected_type: expected_type.to_string(),
    };
    let physical = match attr_type {
        AttrType::Bool => match content.trim().to_ascii_lowercase().as_str() {
            "true" | "1" => PhysicalValue::Bool(true),
            "false" | "0" => PhysicalValue::Bool(false),
            _ => return Err(invalid("boolean (true/false/1/0)")),
        },
        AttrType::String => PhysicalValue::String(Cow::Borrowed(content)),
        AttrType::Float | AttrType::Double => PhysicalValue::Float(
            content
                .trim()
                .parse()
                .map_err(|_| invalid(attr_type.as_str()))?,
        ),
        AttrType::Int | AttrType::Long => PhysicalValue::Int(
            content
                .trim()
                .parse()
                .map_err(|_| invalid(attr_type.as_str()))?,
        ),
    };
    Ok(lift_untyped(physical))
}

/// An attribute value with its entity and character references resolved, and nothing else
/// changed: unlike quick-xml's `normalized_value`, tabs and line breaks are kept as they are, so
/// an imported id or key round-trips byte for byte.
fn attribute_value(raw: &str) -> Result<Cow<'_, str>, EscapeError> {
    unescape(raw)
}

fn extract_attr<R: std::io::BufRead>(
    reader: &Reader<R>,
    element: &BytesStart,
    elem_name: &'static str,
    attr_name: &'static str,
) -> Result<CompactString, IOError> {
    for attr in element.attributes() {
        let a = attr.map_err(|err| IOError::XmlParse {
            position: reader.buffer_position(),
            message: err.to_string(),
        })?;
        if a.key.local_name().as_ref() == attr_name {
            let cow = attribute_value(&a.value).map_err(|err| IOError::XmlParse {
                position: reader.buffer_position(),
                message: err.to_string(),
            })?;
            return Ok(CompactString::new(cow));
        }
    }
    Err(IOError::MissingAttribute {
        element: elem_name.to_string(),
        attribute: attr_name.to_string(),
    })
}

fn extract_key_attrs<R: std::io::BufRead>(
    reader: &Reader<R>,
    e: &BytesStart,
) -> Result<(CompactString, CompactString, CompactString, CompactString), IOError> {
    let mut id = None;
    let mut element_raw = None;
    let mut attr_name = None;
    let mut attr_type_raw = None;

    for attr in e.attributes() {
        let a = attr.map_err(|err| IOError::XmlParse {
            position: reader.buffer_position(),
            message: err.to_string(),
        })?;
        let key_name = a.key.local_name();
        let key_bytes = key_name.as_ref();
        if matches!(key_bytes, "id" | "for" | "attr.name" | "attr.type") {
            let val =
                CompactString::new(attribute_value(&a.value).map_err(|err| IOError::XmlParse {
                    position: reader.buffer_position(),
                    message: err.to_string(),
                })?);

            match key_bytes {
                "id" => id = Some(val),
                "for" => element_raw = Some(val),
                "attr.name" => attr_name = Some(val),
                "attr.type" => attr_type_raw = Some(val),
                _ => (),
            }
        }
    }

    let id = id.ok_or_else(|| IOError::MissingAttribute {
        element: "key".to_string(),
        attribute: "id".to_string(),
    })?;
    let element_raw = element_raw.ok_or_else(|| IOError::MissingAttribute {
        element: "key".to_string(),
        attribute: "for".to_string(),
    })?;
    let attr_name = attr_name.ok_or_else(|| IOError::MissingAttribute {
        element: "key".to_string(),
        attribute: "attr.name".to_string(),
    })?;
    let attr_type_raw = attr_type_raw.ok_or_else(|| IOError::MissingAttribute {
        element: "key".to_string(),
        attribute: "attr.type".to_string(),
    })?;

    Ok((id, element_raw, attr_name, attr_type_raw))
}

fn extract_edge_attrs<R: std::io::BufRead>(
    reader: &Reader<R>,
    e: &BytesStart,
) -> Result<(Option<CompactString>, CompactString, CompactString), IOError> {
    let mut id = None;
    let mut source = None;
    let mut target = None;

    for attr in e.attributes() {
        let a = attr.map_err(|err| IOError::XmlParse {
            position: reader.buffer_position(),
            message: err.to_string(),
        })?;
        let key_name = a.key.local_name();
        let key_bytes = key_name.as_ref();
        if matches!(key_bytes, "id" | "source" | "target") {
            let val =
                CompactString::new(attribute_value(&a.value).map_err(|err| IOError::XmlParse {
                    position: reader.buffer_position(),
                    message: err.to_string(),
                })?);

            match key_bytes {
                "id" => id = Some(val),
                "source" => source = Some(val),
                "target" => target = Some(val),
                _ => (),
            }
        }
    }

    let source = source.ok_or_else(|| IOError::MissingAttribute {
        element: "edge".to_string(),
        attribute: "source".to_string(),
    })?;
    let target = target.ok_or_else(|| IOError::MissingAttribute {
        element: "edge".to_string(),
        attribute: "target".to_string(),
    })?;

    Ok((id, source, target))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::TraversalGraph;

    fn parse_xml_str(xml: &str) -> Result<TraversalGraph, IOError> {
        let mut tg = TraversalGraph::new();
        let importer = GraphMLImport {};
        let reader = Reader::from_str(xml);
        importer.start_reader(&mut tg, reader)?;
        Ok(tg)
    }

    #[test]
    fn test_valid_graphml_import() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
        <graphml xmlns="http://graphml.graphdrawing.org/xmlns">
          <key id="name" for="node" attr.name="name" attr.type="string"/>
          <key id="age" for="node" attr.name="age" attr.type="int"/>
          <key id="active" for="node" attr.name="active" attr.type="bool"/>
          <key id="weight" for="edge" attr.name="weight" attr.type="double"/>
          <graph id="G" edgedefault="directed">
            <node id="1">
              <data key="name">Alice</data>
              <data key="age">30</data>
              <data key="active">true</data>
            </node>
            <node id="2">
              <data key="name">Bob</data>
              <data key="age">25</data>
              <data key="active">0</data>
            </node>
            <edge id="e1" source="1" target="2">
              <data key="weight">0.85</data>
            </edge>
          </graph>
        </graphml>"#;

        let res = parse_xml_str(xml);
        assert!(res.is_ok());
        let g = res.unwrap();
        assert_eq!(g.vertex_count(), 2);
        assert_eq!(g.edge_count(), 1);

        let v1_idx = GraphStorage::get_vertex_by_id(&g, "1").unwrap().unwrap();
        let v1_props = GraphStorage::vertex_properties(&g, v1_idx)
            .unwrap()
            .unwrap();
        assert_eq!(
            v1_props.get("active"),
            Some(&ElementProperty::Boolean(true))
        );

        let v2_idx = GraphStorage::get_vertex_by_id(&g, "2").unwrap().unwrap();
        let v2_props = GraphStorage::vertex_properties(&g, v2_idx)
            .unwrap()
            .unwrap();
        assert_eq!(
            v2_props.get("active"),
            Some(&ElementProperty::Boolean(false))
        );
    }

    #[test]
    fn test_default_property_fallback() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
        <graphml xmlns="http://graphml.graphdrawing.org/xmlns">
          <key id="active" for="node" attr.name="active" attr.type="bool">
            <default>true</default>
          </key>
          <key id="role" for="node" attr.name="role" attr.type="string">
            <default>member</default>
          </key>
          <graph id="G" edgedefault="directed">
            <node id="1">
              <data key="role">admin</data>
            </node>
            <node id="2">
            </node>
          </graph>
        </graphml>"#;

        let res = parse_xml_str(xml);
        assert!(res.is_ok());
        let g = res.unwrap();

        let v1_idx = GraphStorage::get_vertex_by_id(&g, "1").unwrap().unwrap();
        let v1_props = GraphStorage::vertex_properties(&g, v1_idx)
            .unwrap()
            .unwrap();
        assert_eq!(
            v1_props.get("active"),
            Some(&ElementProperty::Boolean(true))
        );
        assert_eq!(
            v1_props.get("role"),
            Some(&ElementProperty::String("admin".into()))
        );

        let v2_idx = GraphStorage::get_vertex_by_id(&g, "2").unwrap().unwrap();
        let v2_props = GraphStorage::vertex_properties(&g, v2_idx)
            .unwrap()
            .unwrap();
        assert_eq!(
            v2_props.get("active"),
            Some(&ElementProperty::Boolean(true))
        );
        assert_eq!(
            v2_props.get("role"),
            Some(&ElementProperty::String("member".into()))
        );
    }

    #[test]
    fn test_two_pass_type_inference_and_promotion() {
        assert_eq!(promote_type(AttrType::Int, AttrType::Long), AttrType::Long);
        assert_eq!(
            promote_type(AttrType::Int, AttrType::Double),
            AttrType::Double
        );
        assert_eq!(
            promote_type(AttrType::Float, AttrType::Double),
            AttrType::Double
        );
        assert_eq!(
            promote_type(AttrType::Bool, AttrType::Int),
            AttrType::String
        );
        assert_eq!(
            promote_type(AttrType::String, AttrType::Int),
            AttrType::String
        );
    }

    #[test]
    fn test_graphml_export_and_import_roundtrip() {
        let mut graph = TraversalGraph::new();

        let idx1 = {
            let mut data1 = PropertyMap::default();
            data1.insert("name", "Alice".into());
            data1.insert("age", (30i64).into());
            data1.insert("active", true.into());
            graph
                .insert_vertex(Some("1".into()), vec!["person".into()], data1)
                .unwrap()
        };

        let idx2 = {
            let mut data2 = PropertyMap::default();
            data2.insert("name", "Bob".into());
            data2.insert("age", (25i64).into());
            data2.insert("active", false.into());
            graph
                .insert_vertex(Some("2".into()), vec!["person".into()], data2)
                .unwrap()
        };

        let mut edge_data = PropertyMap::default();
        edge_data.insert("weight", (0.75f64).into());
        graph
            .insert_edge(None, Some("knows".into()), idx1, idx2, edge_data)
            .unwrap();

        let mut xml_bytes = Vec::new();
        let exporter = GraphMLExport::new();
        let export_res = exporter.export_writer(&graph, &mut xml_bytes);
        assert!(export_res.is_ok());

        let xml_str = String::from_utf8(xml_bytes).unwrap();
        assert!(
            xml_str.contains(
                "<key id=\"name\" for=\"node\" attr.name=\"name\" attr.type=\"string\"/>"
            )
        );
        assert!(
            xml_str.contains(
                "<key id=\"active\" for=\"node\" attr.name=\"active\" attr.type=\"bool\"/>"
            )
        );
        assert!(xml_str.contains(
            "<key id=\"labelV\" for=\"node\" attr.name=\"labelV\" attr.type=\"string\"/>"
        ));

        let imported_g = parse_xml_str(&xml_str).unwrap();

        assert_eq!(imported_g.vertex_count(), 2);
        assert_eq!(imported_g.edge_count(), 1);

        let v1_idx = GraphStorage::get_vertex_by_id(&imported_g, "1")
            .unwrap()
            .unwrap();
        assert_eq!(
            GraphStorage::vertex_label(&imported_g, v1_idx).unwrap(),
            Some("person")
        );
        let v1_props = GraphStorage::vertex_properties(&imported_g, v1_idx)
            .unwrap()
            .unwrap();
        assert_eq!(
            v1_props.get("name"),
            Some(&ElementProperty::String("Alice".into()))
        );
        assert_eq!(
            v1_props.get("age"),
            Some(&ElementProperty::Number((30i64).into()))
        );
        assert_eq!(
            v1_props.get("active"),
            Some(&ElementProperty::Boolean(true))
        );
    }

    #[test]
    fn test_file_not_found() {
        let mut graph = TraversalGraph::new();
        let importer = GraphMLImport {};
        let res = importer.start(
            &mut graph,
            "/non_existent_file_path_12345.graphml".to_string(),
        );
        assert!(matches!(res, Err(IOError::FileRead { .. })));
    }

    #[test]
    fn test_malformed_xml() {
        let xml = r#"<graphml><node id="1"><unclosed_tag></node></graphml>"#;
        let res = parse_xml_str(xml);
        assert!(matches!(res, Err(IOError::XmlParse { .. })));
    }

    #[test]
    fn test_missing_node_attribute() {
        let xml = r#"<graphml><graph><node><data key="name">Alice</data></node></graph></graphml>"#;
        let res = parse_xml_str(xml);
        if let Err(IOError::MissingAttribute { element, attribute }) = res {
            assert_eq!(element, "node");
            assert_eq!(attribute, "id");
        } else {
            panic!("Expected MissingAttribute for node id");
        }
    }

    #[test]
    fn test_missing_edge_attribute() {
        let xml = r#"<graphml><graph><node id="1"/><edge id="e1" source="1"><data key="w">1.0</data></edge></graph></graphml>"#;
        let res = parse_xml_str(xml);
        if let Err(IOError::MissingAttribute { element, attribute }) = res {
            assert_eq!(element, "edge");
            assert_eq!(attribute, "target");
        } else {
            panic!("Expected MissingAttribute for edge target");
        }
    }

    #[test]
    fn test_invalid_attr_type() {
        let xml = r#"<graphml><key id="k1" for="node" attr.name="foo" attr.type="invalid_type"/></graphml>"#;
        let res = parse_xml_str(xml);
        if let Err(IOError::InvalidAttributeValue { key, attribute, .. }) = res {
            assert_eq!(key, "k1");
            assert_eq!(attribute, "attr.type");
        } else {
            panic!("Expected InvalidAttributeValue for attr.type");
        }
    }

    #[test]
    fn test_invalid_number_value() {
        let xml = r#"<graphml>
          <key id="age" for="node" attr.name="age" attr.type="int"/>
          <graph><node id="1"><data key="age">thirty</data></node></graph>
        </graphml>"#;
        let res = parse_xml_str(xml);
        if let Err(IOError::InvalidAttributeValue {
            key,
            attribute,
            value,
            ..
        }) = res
        {
            assert_eq!(key, "age");
            assert_eq!(attribute, "age");
            assert_eq!(value, "thirty");
        } else {
            panic!("Expected InvalidAttributeValue for number parsing");
        }
    }

    #[test]
    fn test_invalid_bool_value() {
        let xml = r#"<graphml>
          <key id="active" for="node" attr.name="active" attr.type="bool"/>
          <graph><node id="1"><data key="active">maybe</data></node></graph>
        </graphml>"#;
        let res = parse_xml_str(xml);
        if let Err(IOError::InvalidAttributeValue { key, value, .. }) = res {
            assert_eq!(key, "active");
            assert_eq!(value, "maybe");
        } else {
            panic!("Expected InvalidAttributeValue for bool parsing");
        }
    }

    #[test]
    fn test_edge_non_existent_vertex() {
        let xml = r#"<graphml><graph>
          <node id="1"/>
          <edge id="e1" source="1" target="999"/>
        </graph></graphml>"#;
        let res = parse_xml_str(xml);
        if let Err(IOError::Graph(GraphError::VertexNotFound { id })) = res {
            assert_eq!(id, "999");
        } else {
            panic!("Expected GraphError::VertexNotFound for target vertex 999");
        }
    }
}
