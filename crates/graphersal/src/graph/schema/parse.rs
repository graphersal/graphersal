// Loading and printing the schema format; documented in the module docs of `crate::schema`.

use std::borrow::Cow;

use crate::error::{DiagnosticError, SchemaError};
use crate::graph::schema::{
    Annotations, Constraints, DefaultFunction, EdgeConnection, EdgeSchema, GraphSchema, JsonType,
    PropertySchema, SCHEMA_FORMAT_URL, SchemaFormat, SchemaMode, SchemaPattern, VectorSchema,
    VertexSchema,
};
use crate::value::physical::{PhysicalValue, lift_untyped, lower, read_json, to_json_text};
use crate::value::temporal::{Temporal, TemporalKind};
use crate::value::vector::MAX_VECTOR_DIMENSION;
use crate::value::{ElementProperty, Number, PropertyMap, VectorElement, VectorMetric};

/// How deeply a schema document may nest (arrays and objects), and how deeply `$ref`s may
/// expand. A schema can come from an untrusted source; this keeps it from exhausting the stack.
pub(crate) const MAX_SCHEMA_DEPTH: usize = 128;

/// How many nodes a schema may have after its `$ref`s are inlined (a reference used many times
/// is copied each time).
pub(crate) const MAX_SCHEMA_NODES: usize = 100_000;

/// The keywords a property node accepts.
const NODE_KEYWORDS: &str = "type, format, items, properties, required, additionalProperties, \
     anyOf, enum, const, minimum, maximum, exclusiveMinimum, exclusiveMaximum, formatMinimum, \
     formatMaximum, formatExclusiveMinimum, formatExclusiveMaximum, element, dimension, \
     metric, embedder (with format vector), minLength, \
     maxLength, pattern, minItems, maxItems, title, description, $comment, examples, \
     deprecated, default, defaultFunction, $ref (and $defs, $schema, $id at a label schema's root)";

/// The `format` of a vector node (on `array`, not one of the string formats).
pub(crate) const VECTOR_FORMAT: &str = "vector";

/// The advice of a misplaced vector keyword.
const VECTOR_HINT: &str = "A vector property is {\"type\": \"array\", \"format\": \"vector\", \
     \"element\": \"f32\", \"dimension\": 768} (element f32, f16, i8 or bit, default f32; \
     dimension optional; metric an optional annotation: cosine, dot, l2, hamming; embedder the \
     model identity of the embedder that produced the vectors, e.g. \"hash-ngram/1\").";

/// The general advice appended to every schema format error.
pub(crate) const FORMAT_HELP: &str = "The schema format is an envelope ($schema, meta, mode, \
     vertices, edges) around a strict subset of JSON Schema 2020-12 for each label's \
     properties; see the book page \"Schemas\" for the reference.";

type Fields<'a> = [(Cow<'a, str>, PhysicalValue<'a>)];

/// A JSON Pointer under construction.
#[derive(Default, Clone)]
struct Pointer(Vec<String>);

impl Pointer {
    fn render(&self) -> String {
        if self.0.is_empty() {
            return "/".to_string();
        }
        let mut out = String::new();
        for segment in &self.0 {
            out.push('/');
            out.push_str(&segment.replace('~', "~0").replace('/', "~1"));
        }
        out
    }

    fn child(&self, segment: impl Into<String>) -> Pointer {
        let mut next = self.clone();
        next.0.push(segment.into());
        next
    }
}

fn invalid(at: &Pointer, reason: impl Into<String>, hint: impl Into<String>) -> SchemaError {
    SchemaError::InvalidSchema {
        path: at.render(),
        reason: reason.into(),
        hint: hint.into(),
    }
}

fn kind_name(value: &PhysicalValue<'_>) -> &'static str {
    match value {
        PhysicalValue::Null => "null",
        PhysicalValue::Bool(_) => "a boolean",
        PhysicalValue::Int(_) | PhysicalValue::Float(_) => "a number",
        PhysicalValue::String(_) => "a string",
        PhysicalValue::Array(_) => "an array",
        PhysicalValue::Object(_) => "an object",
    }
}

/// The members of an object value, rejecting duplicate keys.
fn object<'v, 'a>(
    value: &'v PhysicalValue<'a>,
    at: &Pointer,
    what: &str,
) -> Result<&'v Fields<'a>, SchemaError> {
    let PhysicalValue::Object(fields) = value else {
        return Err(invalid(
            at,
            format!("{what} must be an object, found {}", kind_name(value)),
            "",
        ));
    };
    let duplicate = |key: &str| {
        invalid(
            at,
            format!("duplicate key '{key}'"),
            "Each key may appear once in a JSON object.",
        )
    };
    // A pairwise scan for the usual handful of keys; a set for large objects (an inferred
    // schema of a graph with 100k labels has 100k keys under `vertices`).
    if fields.len() <= 16 {
        for (index, (key, _)) in fields.iter().enumerate() {
            if fields[..index].iter().any(|(other, _)| other == key) {
                return Err(duplicate(key.as_ref()));
            }
        }
    } else {
        let mut seen = std::collections::HashSet::with_capacity(fields.len());
        for (key, _) in fields.iter() {
            if !seen.insert(key.as_ref()) {
                return Err(duplicate(key.as_ref()));
            }
        }
    }
    Ok(fields)
}

fn string<'v>(
    value: &'v PhysicalValue<'_>,
    at: &Pointer,
    what: &str,
) -> Result<&'v str, SchemaError> {
    match value {
        PhysicalValue::String(text) => Ok(text.as_ref()),
        other => Err(invalid(
            at,
            format!("{what} must be a string, found {}", kind_name(other)),
            "",
        )),
    }
}

fn number(value: &PhysicalValue<'_>, at: &Pointer, what: &str) -> Result<Number, SchemaError> {
    match value {
        PhysicalValue::Int(i) => Ok(Number::from_i64(*i)),
        PhysicalValue::Float(f) if f.is_finite() => Ok(Number::from_f64(*f)),
        other => Err(invalid(
            at,
            format!("{what} must be a number, found {}", kind_name(other)),
            "",
        )),
    }
}

fn count(value: &PhysicalValue<'_>, at: &Pointer, what: &str) -> Result<u64, SchemaError> {
    match value {
        PhysicalValue::Int(i) if *i >= 0 => Ok(*i as u64),
        other => Err(invalid(
            at,
            format!(
                "{what} must be a non-negative integer, found {}",
                match other {
                    PhysicalValue::Int(i) => i.to_string(),
                    other => kind_name(other).to_string(),
                }
            ),
            "",
        )),
    }
}

fn boolean(value: &PhysicalValue<'_>, at: &Pointer, what: &str) -> Result<bool, SchemaError> {
    match value {
        PhysicalValue::Bool(b) => Ok(*b),
        other => Err(invalid(
            at,
            format!("{what} must be true or false, found {}", kind_name(other)),
            "",
        )),
    }
}

/// The value of `defaultFunction`: one of the closed set of functions.
fn default_function(
    value: &PhysicalValue<'_>,
    at: &Pointer,
) -> Result<DefaultFunction, SchemaError> {
    let name = string(value, at, "defaultFunction")?;
    if let Some(function) = DefaultFunction::from_name(name) {
        return Ok(function);
    }
    Err(invalid(
        at,
        format!("unknown defaultFunction '{name}'"),
        "The supported functions are uuidv4 (random) and uuidv7 (time-ordered) on a \
         {\"type\": \"string\", \"format\": \"uuid\"} property, now (the unit's start) on a \
         {\"type\": \"string\", \"format\": \"date-time\"} property and today (its UTC date) \
         on a {\"type\": \"string\", \"format\": \"date\"} property.",
    ))
}

/// A node has at most one of `default` and `defaultFunction`.
fn check_one_default(annotations: &Annotations, at: &Pointer) -> Result<(), SchemaError> {
    if annotations.default.is_some() && annotations.default_function.is_some() {
        return Err(invalid(
            at,
            "default and defaultFunction on the same property",
            "Keep one: default stores a fixed value, defaultFunction computes one for every \
             created element.",
        ));
    }
    Ok(())
}

/// A JSON value the schema keeps verbatim (`enum`, `const`, `default`, `examples`, `meta`).
fn json_value(value: &PhysicalValue<'_>) -> ElementProperty {
    lift_untyped(value.clone())
}

impl GraphSchema {
    /// Loads a schema from its JSON text. Strict: see the [module docs](crate::schema).
    ///
    /// ```
    /// use graphersal::schema::{GraphSchema, SchemaMode};
    /// let schema = GraphSchema::from_json(
    ///     r#"{"mode": "closed", "vertices": {"person": {"schema": {"type": "object",
    ///         "properties": {"name": {"type": "string"}}, "required": ["name"]}}}}"#,
    /// )
    /// .unwrap();
    /// assert_eq!(schema.mode, SchemaMode::Closed);
    /// assert!(GraphSchema::from_json(r#"{"mdoe": "closed"}"#).is_err());
    /// ```
    pub fn from_json(text: &str) -> Result<GraphSchema, SchemaError> {
        let value =
            read_json(text, MAX_SCHEMA_DEPTH).map_err(|err| SchemaError::InvalidSchema {
                path: "/".to_string(),
                reason: format!("not valid JSON: {err}"),
                hint: String::new(),
            })?;
        Self::from_physical(&value)
    }

    /// Loads a schema from a value (a map as a Rhai map or a Python dict converts to). A `uuid`
    /// inside it is read as its canonical string. Strict: see the [module docs](crate::schema).
    pub fn from_value(value: &ElementProperty) -> Result<GraphSchema, SchemaError> {
        Self::from_physical(&lower(value))
    }

    pub(crate) fn from_physical(value: &PhysicalValue<'_>) -> Result<GraphSchema, SchemaError> {
        let root = Pointer::default();
        let fields = object(value, &root, "a schema")?;
        let mut schema = GraphSchema::new();
        for (key, field) in fields {
            let at = root.child(key.as_ref());
            match key.as_ref() {
                "$schema" => {
                    let url = string(field, &at, "$schema")?;
                    if url != SCHEMA_FORMAT_URL {
                        return Err(invalid(
                            &at,
                            format!("unsupported schema format '{url}'"),
                            format!(
                                "This engine reads the format \"{SCHEMA_FORMAT_URL}\"; omit \
                                 $schema or write that URL. A label's JSON Schema dialect \
                                 ($schema inside a label's \"schema\") is a different key."
                            ),
                        ));
                    }
                }
                "meta" => {
                    object(field, &at, "meta")?;
                    schema.meta = Some(json_value(field));
                }
                "mode" => {
                    let name = string(field, &at, "mode")?;
                    schema.mode = SchemaMode::parse(name)?;
                }
                "vertices" => {
                    for (label, entry) in object(field, &at, "vertices")? {
                        let at = at.child(label.as_ref());
                        check_label(label, &at)?;
                        schema
                            .vertices
                            .insert(label.to_string(), parse_vertex(entry, &at)?);
                    }
                }
                "edges" => {
                    for (label, entry) in object(field, &at, "edges")? {
                        let at = at.child(label.as_ref());
                        check_label(label, &at)?;
                        schema
                            .edges
                            .insert(label.to_string(), parse_edge(entry, &at)?);
                    }
                }
                other => {
                    let hint = match other {
                        "version" => {
                            "There is no schema version field: keep your own version in \
                             \"meta\" (e.g. \"meta\": {\"version\": \"2.3\"})."
                        }
                        "comment" | "description" | "title" => {
                            "Put free text in \"meta\", or in a label schema's \"description\"."
                        }
                        _ => {
                            "The envelope keys are $schema, meta, mode, vertices and edges \
                             (a typo is never ignored)."
                        }
                    };
                    return Err(invalid(&at, format!("unknown key '{other}'"), hint));
                }
            }
        }
        Ok(schema)
    }

    /// The schema in its canonical data shape: what [`Self::to_json`] writes, a Rhai map or a
    /// Python dict shows, and [`Self::from_value`] reads back. Keys are always written in the
    /// same order; `$schema`, `mode`, `vertices` and `edges` are always present.
    pub fn to_value(&self) -> ElementProperty {
        let mut root = PropertyMap::default();
        root.insert("$schema", ElementProperty::from(SCHEMA_FORMAT_URL));
        if let Some(meta) = &self.meta {
            root.insert("meta", meta.clone());
        }
        root.insert("mode", ElementProperty::from(self.mode.as_str()));
        let mut vertices = PropertyMap::default();
        for (label, vertex) in &self.vertices {
            let mut entry = PropertyMap::default();
            entry.insert("schema", vertex.schema.to_value());
            vertices.insert(label.as_str(), object_value(entry));
        }
        root.insert("vertices", object_value(vertices));
        let mut edges = PropertyMap::default();
        for (label, edge) in &self.edges {
            let mut entry = PropertyMap::default();
            if !edge.connections.is_empty() {
                entry.insert(
                    "connections",
                    ElementProperty::Array(
                        edge.connections
                            .iter()
                            .map(|c| {
                                let mut conn = PropertyMap::default();
                                conn.insert("from", ElementProperty::from(c.from.as_str()));
                                conn.insert("to", ElementProperty::from(c.to.as_str()));
                                object_value(conn)
                            })
                            .collect(),
                    ),
                );
            }
            entry.insert("schema", edge.schema.to_value());
            edges.insert(label.as_str(), object_value(entry));
        }
        root.insert("edges", object_value(edges));
        object_value(root)
    }

    /// The schema as compact JSON text (the canonical form, see [`Self::to_value`]).
    pub fn to_json(&self) -> String {
        to_json_text(&self.to_value()).unwrap_or_else(|_| "{}".to_string())
    }

    /// The schema as indented JSON text (two spaces per level).
    pub fn to_json_pretty(&self) -> String {
        pretty_json(&self.to_value())
    }
}

/// Indented JSON text of a value (two spaces per level), through the physical codec.
pub(crate) fn pretty_json(value: &ElementProperty) -> String {
    fn write(value: &PhysicalValue<'_>, depth: usize, out: &mut String) {
        let pad = |depth: usize| "  ".repeat(depth);
        match value {
            PhysicalValue::Array(items) if !items.is_empty() => {
                out.push_str("[\n");
                for (index, item) in items.iter().enumerate() {
                    out.push_str(&pad(depth + 1));
                    write(item, depth + 1, out);
                    if index + 1 < items.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                out.push_str(&pad(depth));
                out.push(']');
            }
            PhysicalValue::Object(fields) if !fields.is_empty() => {
                out.push_str("{\n");
                for (index, (key, field)) in fields.iter().enumerate() {
                    out.push_str(&pad(depth + 1));
                    let mut key_text = String::new();
                    let _ = crate::value::physical::write_json(
                        &PhysicalValue::String(Cow::Borrowed(key.as_ref())),
                        &mut key_text,
                    );
                    out.push_str(&key_text);
                    out.push_str(": ");
                    write(field, depth + 1, out);
                    if index + 1 < fields.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                out.push_str(&pad(depth));
                out.push('}');
            }
            other => {
                let mut text = String::new();
                if crate::value::physical::write_json(other, &mut text).is_err() {
                    text = "null".to_string();
                }
                out.push_str(&text);
            }
        }
    }
    let mut out = String::new();
    write(&lower(value), 0, &mut out);
    out
}

fn object_value(map: PropertyMap) -> ElementProperty {
    ElementProperty::Object(Box::new(map))
}

fn check_label(label: &str, at: &Pointer) -> Result<(), SchemaError> {
    if label.is_empty() {
        return Err(invalid(at, "a label must not be empty", ""));
    }
    Ok(())
}

fn parse_vertex(value: &PhysicalValue<'_>, at: &Pointer) -> Result<VertexSchema, SchemaError> {
    let mut vertex = VertexSchema::default();
    for (key, field) in object(value, at, "a vertex label entry")? {
        let at = at.child(key.as_ref());
        match key.as_ref() {
            "schema" => vertex.schema = parse_label_schema(field, &at)?,
            other => return Err(label_key_error(other, &at, "schema")),
        }
    }
    Ok(vertex)
}

fn parse_edge(value: &PhysicalValue<'_>, at: &Pointer) -> Result<EdgeSchema, SchemaError> {
    let mut edge = EdgeSchema::default();
    for (key, field) in object(value, at, "an edge label entry")? {
        let at = at.child(key.as_ref());
        match key.as_ref() {
            "schema" => edge.schema = parse_label_schema(field, &at)?,
            "connections" => {
                let PhysicalValue::Array(items) = field else {
                    return Err(invalid(
                        &at,
                        format!("connections must be an array, found {}", kind_name(field)),
                        "Write \"connections\": [{\"from\": \"person\", \"to\": \"software\"}].",
                    ));
                };
                for (index, item) in items.iter().enumerate() {
                    let at = at.child(index.to_string());
                    let (mut from, mut to) = (None, None);
                    for (key, end) in object(item, &at, "a connection")? {
                        let at = at.child(key.as_ref());
                        match key.as_ref() {
                            "from" => from = Some(string(end, &at, "from")?.to_string()),
                            "to" => to = Some(string(end, &at, "to")?.to_string()),
                            other => {
                                return Err(invalid(
                                    &at,
                                    format!("unknown key '{other}' in a connection"),
                                    "A connection has exactly the keys \"from\" and \"to\" \
                                     (vertex labels, or \"*\" for any label).",
                                ));
                            }
                        }
                    }
                    match (from, to) {
                        (Some(from), Some(to)) if !from.is_empty() && !to.is_empty() => {
                            edge.connections.push(EdgeConnection { from, to })
                        }
                        _ => {
                            return Err(invalid(
                                &at,
                                "a connection needs a non-empty \"from\" and \"to\"",
                                "Use \"*\" to allow any vertex label at one end.",
                            ));
                        }
                    }
                }
            }
            other => return Err(label_key_error(other, &at, "connections, schema")),
        }
    }
    Ok(edge)
}

fn label_key_error(key: &str, at: &Pointer, allowed: &str) -> SchemaError {
    let hint = match key {
        "properties" => {
            "A label's properties are a JSON Schema under \"schema\": {\"schema\": {\"type\": \
             \"object\", \"properties\": {..}, \"required\": [..]}}."
                .to_string()
        }
        "comment" | "description" | "title" => {
            "Describe a label inside its schema: {\"schema\": {\"type\": \"object\", \
             \"description\": \"..\"}}."
                .to_string()
        }
        _ => format!("A label entry has the keys: {allowed}."),
    };
    invalid(at, format!("unknown key '{key}'"), hint)
}

/// A label's `schema`: an `object` node at a document root (`$defs`, `$schema`, `$id` allowed).
fn parse_label_schema(
    value: &PhysicalValue<'_>,
    at: &Pointer,
) -> Result<PropertySchema, SchemaError> {
    let fields = object(value, at, "a label schema")?;
    let defs = match fields.iter().find(|(key, _)| key == "$defs") {
        Some((_, defs)) => Some(object(defs, &at.child("$defs"), "$defs")?),
        None => None,
    };
    let mut parser = NodeParser {
        defs,
        defs_at: at.child("$defs"),
        expanding: Vec::new(),
        nodes: 0,
    };
    // Every definition must be valid, also one nothing references.
    if let Some(defs) = defs {
        for (name, def) in defs {
            parser.expanding.push(name.to_string());
            parser.node(def, &at.child("$defs").child(name.as_ref()), 1)?;
            parser.expanding.pop();
        }
        parser.nodes = 0;
    }
    let schema = parser.node_with(value, at, 0, true)?;
    if !super::elements::is_object_root(&schema)
        || schema.any_of.is_some()
        || schema.items.is_some()
        || schema.format.is_some()
        || schema.vector.is_some()
        || schema.constraints.as_deref().is_some_and(|c| !c.is_empty())
    {
        return Err(invalid(
            at,
            "a label's schema must be an object node",
            "Write {\"type\": \"object\", \"properties\": {..}, \"required\": [..]}: a label's \
             schema describes its property map. type must be exactly \"object\"; anyOf, enum, \
             const and the value constraints belong to the properties inside it.",
        ));
    }
    Ok(schema)
}

struct NodeParser<'d, 'a> {
    defs: Option<&'d Fields<'a>>,
    defs_at: Pointer,
    /// The `$defs` names being inlined (cycle detection).
    expanding: Vec<String>,
    nodes: usize,
}

/// Keywords JSON Schema has that the subset rejects, with the reason.
fn rejected_keyword(key: &str) -> Option<&'static str> {
    Some(match key {
        "oneOf" => {
            "oneOf (exactly one member) is not supported; use anyOf (at least one member), \
             which is equivalent when the members cannot both match"
        }
        "allOf" | "not" | "if" | "then" | "else" => {
            "allOf, not and if/then/else are not supported; write the combined constraints on \
             one node"
        }
        "prefixItems" | "contains" | "minContains" | "maxContains" => {
            "tuples and contains are not supported; an array has one item schema (items)"
        }
        "uniqueItems" | "multipleOf" => {
            "uniqueItems and multipleOf are not supported yet (planned as an addition)"
        }
        "patternProperties"
        | "propertyNames"
        | "minProperties"
        | "maxProperties"
        | "dependentRequired"
        | "dependentSchemas"
        | "unevaluatedProperties"
        | "unevaluatedItems" => {
            "this object keyword is not supported; declare the keys with properties, required \
             and additionalProperties"
        }
        "$dynamicRef" | "$dynamicAnchor" | "$anchor" | "$recursiveRef" | "$vocabulary" => {
            "only local references \"$ref\": \"#/$defs/Name\" are supported"
        }
        "readOnly" | "writeOnly" | "contentEncoding" | "contentMediaType" | "contentSchema" => {
            "this annotation is not supported; the accepted annotations are title, description, \
             $comment, examples, deprecated, default and defaultFunction"
        }
        "nullable" => {
            "nullable is not JSON Schema; allow null with \"type\": [\"string\", \"null\"] (and \
             make a key optional by leaving it out of required)"
        }
        "variants" => "variants is gone; a union is anyOf: [{..}, {..}]",
        "min" | "max" => "the bounds are minimum and maximum",
        "min_length" | "max_length" => "the length bounds are minLength and maxLength",
        "min_items" | "max_items" => "the item bounds are minItems and maxItems",
        "metadata" | "total_occurrences" | "comment" => {
            "use the annotations title, description, $comment or examples"
        }
        "definitions" => "definitions is spelled $defs in JSON Schema 2020-12",
        _ => return None,
    })
}

impl NodeParser<'_, '_> {
    fn node(
        &mut self,
        value: &PhysicalValue<'_>,
        at: &Pointer,
        depth: usize,
    ) -> Result<PropertySchema, SchemaError> {
        self.node_with(value, at, depth, false)
    }

    fn node_with(
        &mut self,
        value: &PhysicalValue<'_>,
        at: &Pointer,
        depth: usize,
        root: bool,
    ) -> Result<PropertySchema, SchemaError> {
        if depth > MAX_SCHEMA_DEPTH {
            return Err(invalid(
                at,
                format!("schemas nested deeper than {MAX_SCHEMA_DEPTH} levels"),
                "",
            ));
        }
        self.nodes += 1;
        if self.nodes > MAX_SCHEMA_NODES {
            return Err(invalid(
                at,
                format!("more than {MAX_SCHEMA_NODES} schema nodes (after inlining $ref)"),
                "A definition referenced many times is copied each time; simplify the schema.",
            ));
        }
        if let PhysicalValue::Bool(_) = value {
            return Err(invalid(
                at,
                "a boolean schema (true/false) is not supported",
                "Write {} for \"anything\".",
            ));
        }
        let fields = object(value, at, "a schema node")?;
        if let Some((_, reference)) = fields.iter().find(|(key, _)| key == "$ref") {
            return self.reference(fields, reference, at, depth, root);
        }
        let mut node = PropertySchema::default();
        let mut constraints = Constraints::default();
        let mut annotations = Annotations::default();
        // Kind-specific keywords seen, to check them against `type` afterwards.
        let mut uses: Vec<(&str, JsonType)> = Vec::new();
        // `formatMinimum`, ...: their text is read once the node's `format` is known.
        let mut format_bounds: Vec<(&'static str, String)> = Vec::new();
        // `format: "vector"` and its keywords `element`, `dimension`, `metric`.
        let mut vector_format = false;
        let mut vector_keywords: Vec<&'static str> = Vec::new();
        let mut element = None;
        let mut dimension = None;
        let mut metric = None;
        let mut embedder = None;
        for (key, field) in fields {
            let at = at.child(key.as_ref());
            let key = key.as_ref();
            match key {
                "type" => node.types = Some(parse_types(field, &at)?),
                "format" => {
                    let name = string(field, &at, "format")?;
                    if name == VECTOR_FORMAT {
                        vector_format = true;
                        uses.push(("format", JsonType::Array));
                        continue;
                    }
                    node.format = Some(SchemaFormat::from_name(name).ok_or_else(|| {
                        invalid(
                            &at,
                            format!("unsupported format '{name}'"),
                            "The supported formats are uuid, date-time (an instant with its \
                             offset), date, local-date-time (a date and time without a zone), \
                             time (a time of day without a zone) and duration on a string, and \
                             vector on an array. Remove any other format (the value is a plain \
                             string), so a later version cannot change what this schema \
                             accepts.",
                        )
                    })?);
                    uses.push(("format", JsonType::String));
                }
                "element" => {
                    let name = string(field, &at, key)?;
                    element = Some(VectorElement::from_name(name).ok_or_else(|| {
                        invalid(
                            &at,
                            format!("unknown vector element '{name}'"),
                            "The vector elements are f32, f16, i8 and bit: \
                             {\"type\": \"array\", \"format\": \"vector\", \"element\": \"i8\"}.",
                        )
                    })?);
                    vector_keywords.push("element");
                    uses.push(("element", JsonType::Array));
                }
                "dimension" => {
                    let n = count(field, &at, key)?;
                    if n == 0 || n > MAX_VECTOR_DIMENSION as u64 {
                        return Err(invalid(
                            &at,
                            format!("dimension must be between 1 and {MAX_VECTOR_DIMENSION}"),
                            "Write the number of components of the vectors: \
                             \"dimension\": 768.",
                        ));
                    }
                    dimension = Some(n as u32);
                    vector_keywords.push("dimension");
                    uses.push(("dimension", JsonType::Array));
                }
                "metric" => {
                    let name = string(field, &at, key)?;
                    metric = Some(VectorMetric::from_name(name).ok_or_else(|| {
                        invalid(
                            &at,
                            format!("unknown vector metric '{name}'"),
                            "The metrics are cosine, dot, l2 and hamming.",
                        )
                    })?);
                    vector_keywords.push("metric");
                    uses.push(("metric", JsonType::Array));
                }
                "embedder" => {
                    let model = string(field, &at, key)?;
                    if let Err(reason) = crate::value::vector::embed::check_model_identity(model) {
                        return Err(invalid(
                            &at,
                            reason,
                            "embedder is the model identity of the embedder that produced the \
                             vectors, as g.embedders() lists it under model: \
                             \"embedder\": \"hash-ngram/1\".",
                        ));
                    }
                    embedder = Some(model.to_string());
                    vector_keywords.push("embedder");
                    uses.push(("embedder", JsonType::Array));
                }
                "formatMinimum"
                | "formatMaximum"
                | "formatExclusiveMinimum"
                | "formatExclusiveMaximum" => {
                    let text = string(field, &at, key)?;
                    format_bounds.push((key_static(key), text.to_string()));
                    uses.push((key_static(key), JsonType::String));
                }
                "items" => {
                    if let PhysicalValue::Array(_) = field {
                        return Err(invalid(
                            &at,
                            "items must be one schema (tuples are not supported)",
                            "Write \"items\": {\"type\": \"string\"}.",
                        ));
                    }
                    node.items = Some(Box::new(self.node(field, &at, depth + 1)?));
                    uses.push(("items", JsonType::Array));
                }
                "properties" => {
                    for (name, property) in object(field, &at, "properties")? {
                        let property = self.node(property, &at.child(name.as_ref()), depth + 1)?;
                        node.properties.insert(name.to_string(), property);
                    }
                    uses.push(("properties", JsonType::Object));
                }
                "required" => {
                    let PhysicalValue::Array(names) = field else {
                        return Err(invalid(
                            &at,
                            format!(
                                "required must be an array of keys, found {}",
                                kind_name(field)
                            ),
                            "Write \"required\": [\"name\", \"age\"] on the object node.",
                        ));
                    };
                    for (index, name) in names.iter().enumerate() {
                        let name = string(name, &at.child(index.to_string()), "a required key")?;
                        if node.required.iter().any(|known| known == name) {
                            return Err(invalid(&at, format!("'{name}' is listed twice"), ""));
                        }
                        node.required.push(name.to_string());
                    }
                    uses.push(("required", JsonType::Object));
                }
                "additionalProperties" => {
                    match field {
                        PhysicalValue::Bool(allowed) => node.additional_properties = Some(*allowed),
                        PhysicalValue::Object(_) => {
                            return Err(invalid(
                                &at,
                                "additionalProperties with a schema (a map of T) is not supported",
                                "Use \"additionalProperties\": true (any undeclared key, any \
                                 value) or false. A Pydantic dict[str, T] field becomes \
                                 {\"type\": \"object\", \"additionalProperties\": true}.",
                            ));
                        }
                        other => {
                            return Err(invalid(
                                &at,
                                format!(
                                    "additionalProperties must be true or false, found {}",
                                    kind_name(other)
                                ),
                                "",
                            ));
                        }
                    }
                    uses.push(("additionalProperties", JsonType::Object));
                }
                "anyOf" => {
                    let PhysicalValue::Array(members) = field else {
                        return Err(invalid(
                            &at,
                            format!(
                                "anyOf must be an array of schemas, found {}",
                                kind_name(field)
                            ),
                            "",
                        ));
                    };
                    if members.is_empty() {
                        return Err(invalid(&at, "anyOf must not be empty", ""));
                    }
                    let mut parsed = Vec::with_capacity(members.len());
                    for (index, member) in members.iter().enumerate() {
                        parsed.push(self.node(member, &at.child(index.to_string()), depth + 1)?);
                    }
                    node.any_of = Some(parsed);
                }
                "minimum" => {
                    constraints.minimum = Some(number(field, &at, key)?);
                    uses.push((key_static(key), JsonType::Number));
                }
                "maximum" => {
                    constraints.maximum = Some(number(field, &at, key)?);
                    uses.push((key_static(key), JsonType::Number));
                }
                "exclusiveMinimum" => {
                    constraints.exclusive_minimum = Some(number(field, &at, key)?);
                    uses.push((key_static(key), JsonType::Number));
                }
                "exclusiveMaximum" => {
                    constraints.exclusive_maximum = Some(number(field, &at, key)?);
                    uses.push((key_static(key), JsonType::Number));
                }
                "minLength" => {
                    constraints.min_length = Some(count(field, &at, key)?);
                    uses.push((key_static(key), JsonType::String));
                }
                "maxLength" => {
                    constraints.max_length = Some(count(field, &at, key)?);
                    uses.push((key_static(key), JsonType::String));
                }
                "pattern" => {
                    let source = string(field, &at, key)?;
                    let pattern = SchemaPattern::new(source).map_err(|err| {
                        invalid(&at, err.to_string(), err.help().unwrap_or_default())
                    })?;
                    constraints.pattern = Some(pattern);
                    uses.push((key_static(key), JsonType::String));
                }
                "minItems" => {
                    constraints.min_items = Some(count(field, &at, key)?);
                    uses.push((key_static(key), JsonType::Array));
                }
                "maxItems" => {
                    constraints.max_items = Some(count(field, &at, key)?);
                    uses.push((key_static(key), JsonType::Array));
                }
                "enum" => {
                    let PhysicalValue::Array(values) = field else {
                        return Err(invalid(
                            &at,
                            format!(
                                "enum must be an array of values, found {}",
                                kind_name(field)
                            ),
                            "",
                        ));
                    };
                    if values.is_empty() {
                        return Err(invalid(&at, "enum must not be empty", ""));
                    }
                    constraints.enum_values = Some(values.iter().map(json_value).collect());
                }
                "const" => constraints.const_value = Some(json_value(field)),
                "title" => annotations.title = Some(string(field, &at, key)?.to_string()),
                "description" => {
                    annotations.description = Some(string(field, &at, key)?.to_string())
                }
                "$comment" => annotations.comment = Some(string(field, &at, key)?.to_string()),
                "examples" => {
                    let PhysicalValue::Array(values) = field else {
                        return Err(invalid(
                            &at,
                            format!("examples must be an array, found {}", kind_name(field)),
                            "",
                        ));
                    };
                    annotations.examples = Some(values.iter().map(json_value).collect());
                }
                "deprecated" => annotations.deprecated = Some(boolean(field, &at, key)?),
                "default" => annotations.default = Some(json_value(field)),
                "defaultFunction" => {
                    annotations.default_function = Some(default_function(field, &at)?)
                }
                "$defs" | "$schema" | "$id" if root => {
                    if key != "$defs" {
                        string(field, &at, key)?;
                    }
                }
                "$defs" | "$schema" | "$id" => {
                    return Err(invalid(
                        &at,
                        format!("{key} is only allowed at the root of a label's schema"),
                        "Move the definitions to the label schema's root and reference them \
                         with \"$ref\": \"#/$defs/Name\".",
                    ));
                }
                other => {
                    return Err(match rejected_keyword(other) {
                        Some(reason) => invalid(
                            &at,
                            format!("unsupported keyword '{other}'"),
                            format!("{reason}."),
                        ),
                        None => invalid(
                            &at,
                            format!("unknown keyword '{other}'"),
                            format!("The supported keywords are: {NODE_KEYWORDS}."),
                        ),
                    });
                }
            }
        }
        if let Some(types) = &node.types {
            for (keyword, kind) in &uses {
                let applies = types.contains(kind)
                    || (*kind == JsonType::Number && types.contains(&JsonType::Integer));
                if !applies {
                    return Err(invalid(
                        at,
                        format!(
                            "{keyword} applies to {} values, but the type is {}",
                            match kind {
                                JsonType::Number => "integer/number",
                                other => other.as_str(),
                            },
                            types_text(types)
                        ),
                        "Remove the keyword or add the type it applies to.",
                    ));
                }
            }
        } else if node.format.is_some() {
            return Err(invalid(
                &at.child("format"),
                "format needs \"type\": \"string\" on the same node",
                "Write {\"type\": \"string\", \"format\": \"uuid\"}.",
            ));
        } else if vector_format {
            return Err(invalid(
                &at.child("format"),
                "format vector needs \"type\": \"array\" on the same node",
                VECTOR_HINT,
            ));
        }
        if let Some(keyword) = vector_keywords.first().filter(|_| !vector_format) {
            return Err(invalid(
                &at.child(*keyword),
                format!("{keyword} needs \"format\": \"vector\" on the same node"),
                VECTOR_HINT,
            ));
        }
        if vector_format {
            if node.items.is_some() {
                return Err(invalid(
                    &at.child("items"),
                    "items is not allowed next to format vector",
                    "A vector's components are numbers of its element kind; remove items. \
                     Write {\"type\": \"array\", \"format\": \"vector\", \"element\": \"f32\", \
                     \"dimension\": 768}.",
                ));
            }
            node.vector = Some(VectorSchema {
                element: element.unwrap_or(VectorElement::F32),
                dimension,
                metric,
                embedder,
            });
        }
        for (keyword, text) in format_bounds {
            let bound_at = at.child(keyword);
            let Some(kind) = node.format.and_then(SchemaFormat::temporal_kind) else {
                return Err(invalid(
                    &bound_at,
                    format!("{keyword} needs a date or time format on the same node"),
                    format!(
                        "{keyword} bounds a value of a temporal format: write {{\"type\": \
                         \"string\", \"format\": \"date-time\", \"{keyword}\": \
                         \"2020-01-01T00:00:00Z\"}} (or the format date, local-date-time, time \
                         or duration with a value of that type). For numbers use minimum and \
                         maximum."
                    ),
                ));
            };
            let bound = Temporal::parse_strict(kind, &text).map_err(|err| {
                invalid(
                    &bound_at,
                    format!("{keyword} is not a {kind}: {err}"),
                    format!(
                        "The bound is the text of a value of the node's format ({}), for \
                         example \"{}\".",
                        kind.name(),
                        example_text(kind)
                    ),
                )
            })?;
            let slot = match keyword {
                "formatMinimum" => &mut constraints.format_minimum,
                "formatMaximum" => &mut constraints.format_maximum,
                "formatExclusiveMinimum" => &mut constraints.format_exclusive_minimum,
                _ => &mut constraints.format_exclusive_maximum,
            };
            *slot = Some(bound);
        }
        check_one_default(&annotations, at)?;
        node.constraints = (!constraints.is_empty()).then(|| Box::new(constraints));
        node.annotations = (!annotations.is_empty()).then(|| Box::new(annotations));
        Ok(node)
    }

    /// Inlines `{"$ref": "#/$defs/Name", ..annotations}`.
    fn reference(
        &mut self,
        fields: &Fields<'_>,
        reference: &PhysicalValue<'_>,
        at: &Pointer,
        depth: usize,
        root: bool,
    ) -> Result<PropertySchema, SchemaError> {
        let ref_at = at.child("$ref");
        let target = string(reference, &ref_at, "$ref")?;
        let Some(encoded) = target.strip_prefix("#/$defs/") else {
            return Err(invalid(
                &ref_at,
                format!("unsupported reference '{target}'"),
                "Only local references to the label schema's own definitions are supported: \
                 \"$ref\": \"#/$defs/Name\".",
            ));
        };
        let name = encoded.replace("~1", "/").replace("~0", "~");
        let Some((_, definition)) = self
            .defs
            .and_then(|defs| defs.iter().find(|(key, _)| key.as_ref() == name))
        else {
            return Err(invalid(
                &ref_at,
                format!("'{target}' is not defined"),
                "Define it under \"$defs\" at the root of the same label schema.",
            ));
        };
        if self.expanding.contains(&name) {
            return Err(invalid(
                &ref_at,
                format!(
                    "'{target}' refers to itself (a cycle through {})",
                    self.expanding.join(" -> ")
                ),
                "Recursive schemas are not supported: references are inlined when the schema \
                 is loaded.",
            ));
        }
        self.expanding.push(name.clone());
        let def_at = self.defs_at.child(name.as_str());
        let resolved = self.node(definition, &def_at, depth + 1);
        self.expanding.pop();
        let mut node = resolved?;
        // Only annotations may sit next to `$ref`; they override the definition's.
        let mut siblings = Vec::new();
        for (key, field) in fields {
            if key == "$ref" || (root && matches!(key.as_ref(), "$defs" | "$schema" | "$id")) {
                continue;
            }
            if !matches!(
                key.as_ref(),
                "title"
                    | "description"
                    | "$comment"
                    | "examples"
                    | "deprecated"
                    | "default"
                    | "defaultFunction"
            ) {
                return Err(invalid(
                    &at.child(key.as_ref()),
                    format!("'{key}' next to $ref is not supported"),
                    "Only annotations (title, description, $comment, examples, deprecated, \
                     default, defaultFunction) may sit next to $ref; put other keywords into \
                     the definition.",
                ));
            }
            siblings.push((key.clone(), field.clone()));
        }
        if !siblings.is_empty() {
            let extra = self.node(&PhysicalValue::Object(siblings), at, depth)?;
            if let Some(extra) = extra.annotations {
                let mut annotations = node.annotations.take().map(|a| *a).unwrap_or_default();
                let extra = *extra;
                annotations.title = extra.title.or(annotations.title);
                annotations.description = extra.description.or(annotations.description);
                annotations.comment = extra.comment.or(annotations.comment);
                annotations.examples = extra.examples.or(annotations.examples);
                annotations.deprecated = extra.deprecated.or(annotations.deprecated);
                // A `default`/`defaultFunction` next to `$ref` replaces both of the definition's.
                if extra.default.is_some() || extra.default_function.is_some() {
                    annotations.default = extra.default;
                    annotations.default_function = extra.default_function;
                }
                node.annotations = Some(Box::new(annotations));
            }
        }
        Ok(node)
    }
}

/// `'static` names of the kind-specific keywords (for the applicability check).
fn key_static(key: &str) -> &'static str {
    match key {
        "minimum" => "minimum",
        "maximum" => "maximum",
        "exclusiveMinimum" => "exclusiveMinimum",
        "exclusiveMaximum" => "exclusiveMaximum",
        "minLength" => "minLength",
        "maxLength" => "maxLength",
        "pattern" => "pattern",
        "minItems" => "minItems",
        "formatMinimum" => "formatMinimum",
        "formatMaximum" => "formatMaximum",
        "formatExclusiveMinimum" => "formatExclusiveMinimum",
        "formatExclusiveMaximum" => "formatExclusiveMaximum",
        _ => "maxItems",
    }
}

/// A value of a temporal type as an example in a diagnostic.
fn example_text(kind: TemporalKind) -> &'static str {
    match kind {
        TemporalKind::DateTime => "2024-05-01T09:30:00+02:00",
        TemporalKind::Date => "2024-05-01",
        TemporalKind::LocalDateTime => "2024-05-01T09:30:00",
        TemporalKind::Time => "09:30:00",
        TemporalKind::Duration => "P1DT2H",
        TemporalKind::OffsetTime => "09:30:00+02:00",
    }
}

fn types_text(types: &[JsonType]) -> String {
    if types.len() == 1 {
        format!("\"{}\"", types[0].as_str())
    } else {
        format!(
            "[{}]",
            types
                .iter()
                .map(|t| format!("\"{}\"", t.as_str()))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

fn parse_types(value: &PhysicalValue<'_>, at: &Pointer) -> Result<Vec<JsonType>, SchemaError> {
    let one = |name: &str| -> Result<JsonType, SchemaError> {
        JsonType::from_name(name).ok_or_else(|| {
            let hint = match name {
                "int64" | "long" | "int" => "int64 is written \"integer\" in a schema.",
                "float64" | "double" | "float" => "float64 is written \"number\" in a schema.",
                "uuid" => "uuid is written {\"type\": \"string\", \"format\": \"uuid\"}.",
                "datetime" | "date-time" | "timestamp" => {
                    "datetime is written {\"type\": \"string\", \"format\": \"date-time\"}."
                }
                "date" => "date is written {\"type\": \"string\", \"format\": \"date\"}.",
                "localdatetime" | "local-date-time" => {
                    "localdatetime is written {\"type\": \"string\", \"format\": \
                     \"local-date-time\"}."
                }
                "time" => "time is written {\"type\": \"string\", \"format\": \"time\"}.",
                "offsettime" | "offset-time" | "timetz" => {
                    "offsettime is written {\"type\": \"string\", \"format\": \"offset-time\"}."
                }
                "duration" | "interval" => {
                    "duration is written {\"type\": \"string\", \"format\": \"duration\"}."
                }
                "vector" | "embedding" => {
                    "a vector is written {\"type\": \"array\", \"format\": \"vector\", \
                     \"element\": \"f32\", \"dimension\": 768}."
                }
                "union" => {
                    "A union is written anyOf: [{..}, {..}] (or a type array for \
                            plain types: \"type\": [\"string\", \"null\"])."
                }
                _ => {
                    "The types are null, boolean, integer (int64), number (float64), string \
                      (uuid, datetime, date, localdatetime, time, offsettime and duration with a \
                      \"format\"), \
                      array and object."
                }
            };
            invalid(at, format!("unknown type '{name}'"), hint)
        })
    };
    match value {
        PhysicalValue::String(name) => Ok(vec![one(name)?]),
        PhysicalValue::Array(names) if !names.is_empty() => {
            let mut types = Vec::with_capacity(names.len());
            for (index, name) in names.iter().enumerate() {
                let ty = one(string(name, &at.child(index.to_string()), "a type name")?)?;
                if types.contains(&ty) {
                    return Err(invalid(
                        at,
                        format!("type '{}' is listed twice", ty.as_str()),
                        "",
                    ));
                }
                types.push(ty);
            }
            Ok(types)
        }
        other => Err(invalid(
            at,
            format!(
                "type must be a type name or a non-empty array of them, found {}",
                kind_name(other)
            ),
            "",
        )),
    }
}

impl PropertySchema {
    /// The node in its canonical JSON shape (keywords in a fixed order).
    pub fn to_value(&self) -> ElementProperty {
        let mut map = PropertyMap::default();
        let annotations = self.annotations.as_deref();
        if let Some(a) = annotations {
            if let Some(title) = &a.title {
                map.insert("title", ElementProperty::from(title.as_str()));
            }
            if let Some(description) = &a.description {
                map.insert("description", ElementProperty::from(description.as_str()));
            }
            if let Some(comment) = &a.comment {
                map.insert("$comment", ElementProperty::from(comment.as_str()));
            }
        }
        if let Some(types) = &self.types {
            let value = if types.len() == 1 {
                ElementProperty::from(types[0].as_str())
            } else {
                ElementProperty::Array(
                    types
                        .iter()
                        .map(|t| ElementProperty::from(t.as_str()))
                        .collect(),
                )
            };
            map.insert("type", value);
        }
        if let Some(format) = self.format {
            map.insert("format", ElementProperty::from(format.as_str()));
        }
        if let Some(vector) = &self.vector {
            map.insert("format", ElementProperty::from(VECTOR_FORMAT));
            map.insert("element", ElementProperty::from(vector.element.name()));
            if let Some(dimension) = vector.dimension {
                map.insert("dimension", count_value(u64::from(dimension)));
            }
            if let Some(metric) = vector.metric {
                map.insert("metric", ElementProperty::from(metric.name()));
            }
            if let Some(embedder) = &vector.embedder {
                map.insert("embedder", ElementProperty::from(embedder.as_str()));
            }
        }
        if let Some(c) = self.constraints.as_deref() {
            if let Some(values) = &c.enum_values {
                map.insert("enum", ElementProperty::Array(values.clone()));
            }
            if let Some(value) = &c.const_value {
                map.insert("const", value.clone());
            }
            let numbers = [
                ("minimum", &c.minimum),
                ("exclusiveMinimum", &c.exclusive_minimum),
                ("maximum", &c.maximum),
                ("exclusiveMaximum", &c.exclusive_maximum),
            ];
            for (key, bound) in numbers {
                if let Some(bound) = bound {
                    map.insert(key, ElementProperty::Number(bound.clone()));
                }
            }
            let temporals = [
                ("formatMinimum", &c.format_minimum),
                ("formatExclusiveMinimum", &c.format_exclusive_minimum),
                ("formatMaximum", &c.format_maximum),
                ("formatExclusiveMaximum", &c.format_exclusive_maximum),
            ];
            for (key, bound) in temporals {
                if let Some(bound) = bound {
                    map.insert(key, ElementProperty::from(bound.to_string().as_str()));
                }
            }
            let counts = [("minLength", c.min_length), ("maxLength", c.max_length)];
            for (key, n) in counts {
                if let Some(n) = n {
                    map.insert(key, count_value(n));
                }
            }
            if let Some(pattern) = &c.pattern {
                map.insert("pattern", ElementProperty::from(pattern.as_str()));
            }
            for (key, n) in [("minItems", c.min_items), ("maxItems", c.max_items)] {
                if let Some(n) = n {
                    map.insert(key, count_value(n));
                }
            }
        }
        if let Some(items) = &self.items {
            map.insert("items", items.to_value());
        }
        if !self.properties.is_empty() {
            let mut properties = PropertyMap::default();
            for (name, property) in &self.properties {
                properties.insert(name.as_str(), property.to_value());
            }
            map.insert("properties", object_value(properties));
        }
        if !self.required.is_empty() {
            map.insert(
                "required",
                ElementProperty::Array(
                    self.required
                        .iter()
                        .map(|name| ElementProperty::from(name.as_str()))
                        .collect(),
                ),
            );
        }
        if let Some(allowed) = self.additional_properties {
            map.insert("additionalProperties", ElementProperty::Boolean(allowed));
        }
        if let Some(members) = &self.any_of {
            map.insert(
                "anyOf",
                ElementProperty::Array(members.iter().map(PropertySchema::to_value).collect()),
            );
        }
        if let Some(a) = annotations {
            if let Some(default) = &a.default {
                map.insert("default", default.clone());
            }
            if let Some(function) = a.default_function {
                map.insert("defaultFunction", ElementProperty::from(function.as_str()));
            }
            if let Some(examples) = &a.examples {
                map.insert("examples", ElementProperty::Array(examples.clone()));
            }
            if let Some(deprecated) = a.deprecated {
                map.insert("deprecated", ElementProperty::Boolean(deprecated));
            }
        }
        object_value(map)
    }

    /// Loads one node from a value (strict, `$ref` is not available outside a label schema).
    pub fn from_value(value: &ElementProperty) -> Result<PropertySchema, SchemaError> {
        let mut parser = NodeParser {
            defs: None,
            defs_at: Pointer::default(),
            expanding: Vec::new(),
            nodes: 0,
        };
        parser.node(&lower(value), &Pointer::default(), 0)
    }
}

fn count_value(n: u64) -> ElementProperty {
    ElementProperty::Number(Number::from_i64(i64::try_from(n).unwrap_or(i64::MAX)))
}
