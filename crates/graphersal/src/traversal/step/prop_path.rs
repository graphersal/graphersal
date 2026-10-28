//! Shared read access for property keys given as a [`JsonPath`](crate::value::JsonPath).
//!
//! A path key never changes how a plain-name key is read; the steps that accept a
//! [`PropKey`](crate::value::PropKey) keep their literal-name implementation and switch to the
//! path variants in this module only for a `PropKey::Path`. Reads descend into the stored value by
//! reference and clone at most the value the path ends on (rule 4).

use crate::graph::GraphStorage;
use crate::value::json_path::{JsonPathSegment, descend_properties, descend_property};
use crate::value::{JsonPath, PropKey, PropKeys};
use crate::*;

/// Applies `f` to the value `segments` leads to inside `value`, borrowed from storage where
/// possible. `Ok(None)` is "the path does not exist": a missing key, an out-of-range index or a
/// type mismatch on the way is an absent property, never an error. Only a traverser value that
/// cannot hold properties at all (a path, an entry, ...) is a cast error.
///
/// For a vertex/edge the first segment names the stored property; for a property reference
/// (`values("meta")` before the next step) the segments start inside that property's value; for a
/// computed or materialized value they start at its root.
pub(crate) fn peek_path<S: GraphStorage, R>(
    context: &TraverserContext<'_, S>,
    value: &TraverserValue<S>,
    segments: &[JsonPathSegment],
    f: impl FnOnce(&ElementProperty) -> R,
) -> TraverserResult<Option<R>> {
    match value {
        TraverserValue::Vertex(node) => Ok(context
            .storage
            .vertex_properties(*node)?
            .and_then(|props| descend_properties(props, segments))
            .map(f)),
        TraverserValue::Edge(edge) => Ok(context
            .storage
            .edge_properties(*edge)?
            .and_then(|props| descend_properties(props, segments))
            .map(f)),
        TraverserValue::Computed(root, _) => Ok(descend_property(root, segments).map(f)),
        TraverserValue::VertexProperty(name, node, _) => Ok(context
            .storage
            .vertex_properties(*node)?
            .and_then(|props| props.get(name.as_str()))
            .and_then(|root| descend_property(root, segments))
            .map(f)),
        TraverserValue::EdgeProperty(name, edge, _) => Ok(context
            .storage
            .edge_properties(*edge)?
            .and_then(|props| props.get(name.as_str()))
            .and_then(|root| descend_property(root, segments))
            .map(f)),
        TraverserValue::Object(_) | TraverserValue::Record { .. } => {
            match TraverserValue::resolve_json_path(context, value, segments)? {
                Some(leaf) => {
                    let leaf = leaf.into_node_property(context.storage)?;
                    Ok(Some(f(&leaf)))
                }
                None => Ok(None),
            }
        }
        other => Err(Box::new(TraverserError::cast_exception(
            &["vertex", "edge", "object"],
            other,
        ))),
    }
}

/// The path keys of a map-building step (`valueMap`, `elementMap`) next to its plain names.
///
/// `names` are the literal property names (a plain-name call keeps the existing implementation
/// untouched); `paths` pairs every path key with its canonical text, the key it gets in the
/// result map. `display` keeps the keys in the order they were written, for `.profile()`.
#[derive(Clone)]
pub(crate) struct PathKeys {
    pub(crate) paths: Vec<(JsonPath, CompactString)>,
    display: Vec<PropKey>,
}

impl PathKeys {
    /// Splits the key arguments: plain names (`None` for "all keys", `Some` for a name list) and,
    /// when any key is a path, the path keys. With a path present the name list is `Some` even if
    /// it is empty, because `valueMap(jpath(..))` selects only that path.
    pub(crate) fn split(keys: PropKeys) -> (Option<Vec<String>>, Option<PathKeys>) {
        match keys.into_names() {
            Ok(names) => (names, None),
            Err(keys) => {
                let names = keys
                    .iter()
                    .filter_map(|k| k.as_name().map(str::to_string))
                    .collect();
                let paths = keys
                    .iter()
                    .filter_map(|k| {
                        k.as_path()
                            .map(|p| (p.clone(), CompactString::new(p.to_string())))
                    })
                    .collect();
                (
                    Some(names),
                    Some(PathKeys {
                        paths,
                        display: keys,
                    }),
                )
            }
        }
    }

    /// The step arguments as written: `"name", jpath("$.a.b")`.
    pub(crate) fn render(&self) -> String {
        self.display
            .iter()
            .map(crate::value::literal::rhai_prop_key)
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Appends one entry per path that exists in `props` (key = canonical path text, value =
    /// `wrap(leaf)`); a missing path adds nothing. Reads by reference, clones the leaf only.
    pub(crate) fn append<S: GraphStorage>(
        &self,
        props: Option<&PropertyMap>,
        keys: &mut Vec<RecordKey>,
        values: &mut Vec<TraverserValue<S>>,
        wrap: impl Fn(ElementProperty) -> TraverserValue<S>,
    ) {
        let Some(props) = props else { return };
        for (path, text) in &self.paths {
            if let Some(leaf) = descend_properties(props, path.segments()) {
                keys.push(RecordKey::Name(text.clone()));
                values.push(wrap(leaf.to_logical()));
            }
        }
    }

    /// Like [`append`](Self::append) for an already materialized map value (`Object`, `Record`).
    pub(crate) fn append_from_value<S: GraphStorage>(
        &self,
        context: &TraverserContext<'_, S>,
        value: &TraverserValue<S>,
        keys: &mut Vec<RecordKey>,
        values: &mut Vec<TraverserValue<S>>,
    ) -> Result<(), crate::error::GraphError> {
        for (path, text) in &self.paths {
            if let Some(leaf) = TraverserValue::resolve_json_path(context, value, path.segments())?
            {
                keys.push(RecordKey::Name(text.clone()));
                values.push(leaf);
            }
        }
        Ok(())
    }
}
