//! Create-time defaults: `default` and `defaultFunction` of a label's top-level properties (see
//! [`Annotations`](crate::graph::schema::Annotations) for the rules, the book page "Schema
//! Enforcement" for the guide).
//!
//! - [`fill_defaults`] adds the missing defaults to the properties of an element being created,
//!   before the write path coerces and validates them. Only the create paths call it
//!   ([`GraphStorage::add_vertex`](crate::graph::GraphStorage::add_vertex) /
//!   [`add_edge`](crate::graph::GraphStorage::add_edge) and their `_with` forms); imports, change
//!   sets, replay and loads store what they carry (`insert_vertex`/`insert_edge`).
//! - [`check_defaults`] is the check of a schema being set: every applied default must be valid
//!   for its property, and `defaultFunction` may only sit on a top-level property.

use compact_str::CompactString;

use crate::error::SchemaError;
use crate::graph::GraphNodeType;
use crate::graph::schema::coerce::coerce_to_declared;
use crate::graph::schema::nested::{self, NestedCtx};
use crate::graph::schema::node::CreateDefault;
use crate::graph::schema::{DefaultFunction, GraphSchema, PropertySchema, SchemaMode};
use crate::value::PropertyMap;
use crate::value::json_path::JsonPathSegment;
use crate::value::temporal::{Date, DateTime};

/// What a create needs besides the element: the time `defaultFunction` `now` and `today` store
/// (see [`GraphStorage::add_vertex_with`](crate::storage::GraphStorage::add_vertex_with)).
///
/// A query takes it once per execution (every element one traversal creates gets the same time):
/// a time the caller pinned
/// ([`GraphTraversalSource::with_now`](crate::GraphTraversalSource::with_now); a script's
/// creating steps pin the evaluation's `now()`, which hosts pin with `script::pin_now`), else
/// [`Self::for_storage`]: the start of the storage's outermost open unit
/// ([`GraphStorage::unit_started_at`](crate::storage::GraphStorage::unit_started_at)), else the
/// system clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct CreateContext {
    now: DateTime,
}

impl CreateContext {
    /// A context whose time is `now` (kept with its UTC offset, as the DSL's `now()` keeps a
    /// pinned time).
    pub fn at(now: DateTime) -> Self {
        CreateContext { now }
    }

    /// The context of a create on `storage` that no caller pinned: the start of its outermost
    /// open unit ([`GraphStorage::unit_started_at`](crate::storage::GraphStorage::unit_started_at)),
    /// or the system clock when the storage reports none (no unit open, or a storage that does
    /// not track it). `Err` when the storage cannot read its unit clock.
    pub fn for_storage<S: crate::graph::GraphStorage + ?Sized>(
        storage: &S,
    ) -> Result<Self, crate::error::GraphError> {
        let nanos = storage
            .unit_started_at()?
            .unwrap_or_else(crate::graph::undo::now_nanos);
        Ok(CreateContext::at(datetime_of_nanos(nanos)))
    }

    /// The time of `defaultFunction: "now"`.
    pub fn now(&self) -> DateTime {
        self.now
    }

    /// The time of `defaultFunction: "today"`: the UTC date of [`Self::now`] (the DSL's
    /// `today()`).
    pub fn today(&self) -> Date {
        self.now.with_offset(0).unwrap_or(self.now).local().date()
    }
}

/// The UTC `datetime` of `nanos` since the Unix epoch; a time outside the years -9999 to 9999
/// reads as the epoch (as the DSL's `now()` reads such a system clock).
pub(crate) fn datetime_of_nanos(nanos: i128) -> DateTime {
    DateTime::from_nanos(nanos, 0).unwrap_or(DateTime::EPOCH)
}

/// The system clock as a UTC `datetime`, with the digits the platform gives
/// ([`now_nanos`](crate::graph::undo::now_nanos)).
pub(crate) fn system_now() -> DateTime {
    datetime_of_nanos(crate::graph::undo::now_nanos())
}

/// Adds the create-time default of every top-level property the element's labels declare and
/// `properties` lacks (an explicit value, `null` included, is kept). The first label, in label
/// order, that declares a key decides (the coercion rule): its `default`/`defaultFunction`, or
/// nothing when it has none. Nothing in [`SchemaMode::None`] (nothing is enforced there).
///
/// `labels` is a vertex's label set, or an edge's single label (empty when it has none).
pub(crate) fn fill_defaults(
    schema: &GraphSchema,
    kind: GraphNodeType,
    labels: &[CompactString],
    properties: &mut PropertyMap,
    context: &CreateContext,
) {
    if schema.mode == SchemaMode::None {
        return;
    }
    for (position, label) in labels.iter().enumerate() {
        let Some(root) = schema.label_root(kind, label.as_str()) else {
            continue;
        };
        for (key, node) in &root.properties {
            let Some(default) = node.create_default() else {
                continue;
            };
            if properties.contains_key(key) {
                continue;
            }
            let decided_before = labels[..position].iter().any(|earlier| {
                schema
                    .label_root(kind, earlier.as_str())
                    .is_some_and(|root| root.properties.contains_key(key))
            });
            if !decided_before {
                properties.insert(key.as_str(), default.value(context));
            }
        }
    }
}

/// The check of a schema being set: every top-level `default`/`defaultFunction` produces a value
/// valid for its property (after the write coercion: a canonical string default of a `uuid`
/// property is fine), and no `defaultFunction` sits anywhere else (a label's root, a nested
/// property, array items, an `anyOf` member), where nothing would apply it. A `default` below
/// the top level stays an annotation and is not checked. A vector's `embedder` is likewise
/// refused anywhere but on a label's top-level property. Fails with
/// [`SchemaError::InvalidSchema`] naming the keyword's location.
pub(crate) fn check_defaults(schema: &GraphSchema) -> Result<(), SchemaError> {
    let additional_default = schema.mode.default_additional_properties();
    let labels = schema
        .vertices
        .iter()
        .map(|(label, v)| ("vertex", "vertices", label, &v.schema))
        .chain(
            schema
                .edges
                .iter()
                .map(|(label, e)| ("edge", "edges", label, &e.schema)),
        );
    for (element_type, section, label, root) in labels {
        let root_at = format!("/{section}/{}/schema", escape(label));
        if let Some(at) = misplaced_function(root, &root_at, true) {
            return Err(misplaced(at));
        }
        if let Some(at) = misplaced_embedder(root, &root_at, true) {
            return Err(misplaced_embedder_error(at));
        }
        for (key, node) in &root.properties {
            let at = format!("{root_at}/properties/{}", escape(key));
            for (name, child) in children(node) {
                if let Some(found) = misplaced_function(child, &format!("{at}/{name}"), false) {
                    return Err(misplaced(found));
                }
                if let Some(found) = misplaced_embedder(child, &format!("{at}/{name}"), false) {
                    return Err(misplaced_embedder_error(found));
                }
            }
            let Some(default) = node.create_default() else {
                continue;
            };
            let (keyword, hint) = match default {
                CreateDefault::Value(_) => ("default", DEFAULT_HINT.to_string()),
                CreateDefault::Function(function) => ("defaultFunction", function_hint(function)),
            };
            let fail = |reason: String| SchemaError::InvalidSchema {
                path: format!("{at}/{keyword}"),
                reason: format!("the {keyword} does not fit the property: {reason}"),
                hint: hint.clone(),
            };
            // `now`/`today` are checked with the current time: a value the property admits.
            let value = coerce_to_declared(default.value(&CreateContext::at(system_now())), node)
                .map_err(|failure| fail(failure.reason.to_string()))?;
            let ctx = NestedCtx {
                element_type,
                label,
                additional_default,
                full: true,
            };
            let mut path = vec![JsonPathSegment::Key(key.as_str().into())];
            nested::validate_nested(&ctx, &mut path, node, &value)
                .map_err(|err| fail(err.to_string()))?;
        }
    }
    Ok(())
}

/// The hint of a `default` that does not fit its property.
const DEFAULT_HINT: &str = "A default is stored when an element is created without the \
     property, so it must be a valid value of the property's schema.";

/// The hint of a `defaultFunction` that does not fit its property: the declaration its value
/// needs.
fn function_hint(function: DefaultFunction) -> String {
    let declaration = match function.temporal_format() {
        Some(format) => format!(
            "{{\"type\": \"string\", \"format\": \"{}\"}}",
            format.as_str()
        ),
        None => "{\"type\": \"string\", \"format\": \"uuid\"}".to_string(),
    };
    let produces = match function {
        DefaultFunction::Now => "a datetime (the unit's start)",
        DefaultFunction::Today => "a date (the UTC date of the unit's start)",
        DefaultFunction::Uuidv4 | DefaultFunction::Uuidv7 => "a uuid",
    };
    format!(
        "defaultFunction {} produces {produces} for every created element, so the property \
         must admit it: declare {declaration}.",
        function.as_str()
    )
}

/// The child nodes of `node` with the pointer segment(s) leading to them.
fn children(node: &PropertySchema) -> Vec<(String, &PropertySchema)> {
    let mut out = Vec::new();
    if let Some(items) = &node.items {
        out.push(("items".to_string(), items.as_ref()));
    }
    for (name, property) in &node.properties {
        out.push((format!("properties/{}", escape(name)), property));
    }
    if let Some(members) = &node.any_of {
        for (index, member) in members.iter().enumerate() {
            out.push((format!("anyOf/{index}"), member));
        }
    }
    out
}

/// The location of the first `defaultFunction` in `node` (itself, then its children, depth
/// first), as a JSON Pointer. For a label's `root`, its `properties` are the top level the
/// caller checks, so they are skipped.
fn misplaced_function(node: &PropertySchema, at: &str, root: bool) -> Option<String> {
    if node
        .annotations()
        .is_some_and(|a| a.default_function.is_some())
    {
        return Some(format!("{at}/defaultFunction"));
    }
    children(node)
        .into_iter()
        .filter(|(name, _)| !(root && name.starts_with("properties/")))
        .find_map(|(name, child)| misplaced_function(child, &format!("{at}/{name}"), false))
}

/// The location of the first vector `embedder` in `node` (itself, then its children, depth
/// first), as a JSON Pointer; like [`misplaced_function`], a label root's `properties` are the
/// top level and skipped.
fn misplaced_embedder(node: &PropertySchema, at: &str, root: bool) -> Option<String> {
    if node.vector.as_ref().is_some_and(|v| v.embedder.is_some()) {
        return Some(format!("{at}/embedder"));
    }
    children(node)
        .into_iter()
        .filter(|(name, _)| !(root && name.starts_with("properties/")))
        .find_map(|(name, child)| misplaced_embedder(child, &format!("{at}/{name}"), false))
}

fn misplaced_embedder_error(at: String) -> SchemaError {
    SchemaError::InvalidSchema {
        path: at,
        reason: "embedder applies only to a label's top-level vector properties".to_string(),
        hint: "Declare the vector directly under the label schema's \"properties\": \
               {\"emb\": {\"type\": \"array\", \"format\": \"vector\", \"dimension\": 256, \
               \"embedder\": \"hash-ngram/1\"}}; embed_property and the text queries of \
               nearest() and similarity() read it there."
            .to_string(),
    }
}

/// The location (JSON Pointer below `at`) of the first `defaultFunction` in `node` or any node
/// below it; a saved query's parameter schema must have none.
pub(crate) fn default_function_in(node: &PropertySchema, at: &str) -> Option<String> {
    misplaced_function(node, at, false)
}

fn misplaced(at: String) -> SchemaError {
    SchemaError::InvalidSchema {
        path: at,
        reason: "defaultFunction applies only to a label's top-level properties".to_string(),
        hint: "Put it on a property directly under the label schema's \"properties\"; values \
               inside an object or array are not filled in."
            .to_string(),
    }
}

/// JSON Pointer escaping of one segment (`~` -> `~0`, `/` -> `~1`).
fn escape(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}
