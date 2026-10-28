//! Shared helper of the `Scope::Local` steps (`count(local)`, `sum(local)`, ...).
//!
//! TinkerPop's local steps treat the single incoming object as a collection: a list is worked on
//! element-wise, a map entry-wise, and any other value as a one-element collection. This module
//! decides which of those shapes a stream value has, resolving lazy property handles
//! (`VertexProperty`/`EdgeProperty`) whose stored value is a list or a map, so every local step
//! classifies its input the same way.

use crate::error::{TraverserError, TraverserResult};
use crate::graph::GraphStorage;
use crate::traversal::{Traverser, TraverserContext};
use crate::value::{ElementProperty, TraverserPath, TraverserValue};

/// The shape of one traverser's value, as a `Scope::Local` step sees it.
pub(crate) enum LocalShape<S: GraphStorage> {
    /// A list (`fold()`, a stored list property, a list constant); its elements, in order.
    /// Vertex/edge/property handles inside the list stay lazy.
    List(Vec<TraverserValue<S>>),
    /// A map (`group()`, `project()`, `select(a, b)`, `value_map()`, a stored map property); its
    /// entries, in map order.
    Map(Vec<(TraverserValue<S>, TraverserValue<S>)>),
    /// A path (`path()`), kept whole.
    Path(TraverserPath<S>),
    /// Anything else (numbers, strings, vertices, edges, map entries, null): a one-element
    /// collection holding the value itself.
    Scalar(TraverserValue<S>),
}

/// The stored value behind a lazy property handle, or `None` for any other value (and for a
/// property the element does not carry).
///
/// Borrowed from the graph storage, so a caller that only reads the value (a length, the numbers
/// of a list) does not clone it. `Err` when the storage cannot read the element's properties.
pub(crate) fn stored_value<'c, S: GraphStorage>(
    context: &'c TraverserContext<'_, S>,
    value: &TraverserValue<S>,
) -> Result<Option<&'c ElementProperty>, crate::error::GraphError> {
    Ok(match value {
        // An `id()`/`label()`/`labels()` handle reads no stored property.
        TraverserValue::VertexProperty(_, _, kind) | TraverserValue::EdgeProperty(_, _, kind)
            if !kind.reads_property() =>
        {
            None
        }
        TraverserValue::VertexProperty(property, node, _) => context
            .storage
            .vertex_properties(*node)?
            .and_then(|properties| properties.get(property)),
        TraverserValue::EdgeProperty(property, edge, _) => context
            .storage
            .edge_properties(*edge)?
            .and_then(|properties| properties.get(property)),
        TraverserValue::Computed(_, _)
        | TraverserValue::Vertex(_)
        | TraverserValue::Edge(_)
        | TraverserValue::Path(_)
        | TraverserValue::Record { .. }
        | TraverserValue::Object(_)
        | TraverserValue::PathProperty(_)
        | TraverserValue::Entry(_)
        | TraverserValue::Array(_)
        | TraverserValue::Schema(_)
        | TraverserValue::Graph(_) => None,
    })
}

/// Classifies an owned materialized value: a list or a map is unpacked, anything else becomes a
/// [`LocalShape::Scalar`] built by `fallback`.
fn classify_property<S: GraphStorage>(
    property: ElementProperty,
    fallback: impl FnOnce(ElementProperty) -> TraverserValue<S>,
) -> LocalShape<S> {
    match property {
        ElementProperty::Array(items) => {
            LocalShape::List(items.into_iter().map(TraverserValue::from).collect())
        }
        ElementProperty::Object(entries) => LocalShape::Map(
            entries
                .into_iter()
                .map(|(key, value)| {
                    (
                        TraverserValue::Computed(ElementProperty::String(key), None),
                        TraverserValue::from(value),
                    )
                })
                .collect(),
        ),
        ElementProperty::Map(entries) => LocalShape::Map(
            entries
                .into_iter()
                .map(|(key, value)| (TraverserValue::from(key), TraverserValue::from(value)))
                .collect(),
        ),
        ElementProperty::Null
        | ElementProperty::String(_)
        | ElementProperty::Compressed(_)
        | ElementProperty::Deduped(_)
        | ElementProperty::Number(_)
        | ElementProperty::Boolean(_)
        | ElementProperty::Uuid(_)
        | ElementProperty::Temporal(_)
        | ElementProperty::Vector(_)
        | ElementProperty::Vertex(_)
        | ElementProperty::Edge(_) => LocalShape::Scalar(fallback(property.into_logical())),
    }
}

/// Classifies a traverser value into the collection shape a `Scope::Local` step works on.
///
/// Lists: `Array` (`fold()`), `Computed(ElementProperty::Array)` (`cap()`, constants) and a
/// property handle whose stored value is an array. Maps: `Object`, `Record`,
/// `Computed(ElementProperty::Object)` and a property handle holding an object. `Path` is its own
/// shape. Everything else is a [`LocalShape::Scalar`].
pub(crate) fn classify<S: GraphStorage>(
    context: &TraverserContext<'_, S>,
    value: TraverserValue<S>,
) -> TraverserResult<LocalShape<S>> {
    Ok(match value {
        TraverserValue::Array(items) => LocalShape::List(items),
        TraverserValue::Object(map) => LocalShape::Map(map.into_iter().collect()),
        TraverserValue::Record { keys, values } => LocalShape::Map(
            keys.iter()
                .map(|key| {
                    TraverserValue::Computed(ElementProperty::String(key.to_compact()), None)
                })
                .zip(values)
                .collect(),
        ),
        TraverserValue::Path(path) => LocalShape::Path(path),
        TraverserValue::Computed(property, node) => classify_property(property, |property| {
            TraverserValue::Computed(property, node)
        }),
        TraverserValue::VertexProperty(_, _, _) | TraverserValue::EdgeProperty(_, _, _) => {
            match stored_value(context, &value)? {
                Some(
                    stored @ (ElementProperty::Array(_)
                    | ElementProperty::Object(_)
                    | ElementProperty::Map(_)),
                ) => {
                    // Only a stored collection is cloned; a scalar property stays a lazy handle.
                    classify_property(stored.clone(), |property| {
                        TraverserValue::Computed(property, None)
                    })
                }
                Some(_) | None => LocalShape::Scalar(value),
            }
        }
        TraverserValue::Vertex(_)
        | TraverserValue::Edge(_)
        | TraverserValue::PathProperty(_)
        | TraverserValue::Entry(_)
        | TraverserValue::Schema(_)
        | TraverserValue::Graph(_) => LocalShape::Scalar(value),
    })
}

/// The number of elements `count(local)` reports for `value`: the size of a list or map, the
/// length of a path, and `1` for any other value. Borrows; nothing is cloned.
pub(crate) fn local_len<S: GraphStorage>(
    context: &TraverserContext<'_, S>,
    value: &TraverserValue<S>,
) -> TraverserResult<usize> {
    fn property_len(property: &ElementProperty) -> usize {
        match property {
            ElementProperty::Array(items) => items.len(),
            ElementProperty::Object(entries) => entries.len(),
            ElementProperty::Map(entries) => entries.len(),
            ElementProperty::Null
            | ElementProperty::String(_)
            | ElementProperty::Compressed(_)
            | ElementProperty::Deduped(_)
            | ElementProperty::Number(_)
            | ElementProperty::Boolean(_)
            | ElementProperty::Uuid(_)
            | ElementProperty::Temporal(_)
            | ElementProperty::Vector(_)
            | ElementProperty::Vertex(_)
            | ElementProperty::Edge(_) => 1,
        }
    }
    Ok(match value {
        TraverserValue::Array(items) => items.len(),
        TraverserValue::Object(map) => map.len(),
        TraverserValue::Record { values, .. } => values.len(),
        TraverserValue::Path(path) => path.len(),
        TraverserValue::Computed(property, _) => property_len(property),
        TraverserValue::VertexProperty(_, _, _) | TraverserValue::EdgeProperty(_, _, _) => {
            stored_value(context, value)?.map_or(1, property_len)
        }
        TraverserValue::Vertex(_)
        | TraverserValue::Edge(_)
        | TraverserValue::PathProperty(_)
        | TraverserValue::Entry(_)
        | TraverserValue::Schema(_)
        | TraverserValue::Graph(_) => 1,
    })
}

/// Keeps the elements `start..end` of an owned vector, moving them: the tail is truncated and the
/// head drained, so no element is cloned.
fn keep_vec<T>(items: &mut Vec<T>, start: usize, end: usize) {
    items.truncate(end);
    items.drain(..start.min(items.len()));
}

/// Keeps the entries `start..end` of an owned map in entry order, moving them.
fn keep_map<K, V, H>(entries: &mut indexmap::IndexMap<K, V, H>, start: usize, end: usize) {
    entries.truncate(end);
    entries.drain(..start.min(entries.len()));
}

/// Slices a stored (borrowed) collection property, cloning only the selected elements.
fn slice_stored<S: GraphStorage>(
    stored: &ElementProperty,
    bounds: impl FnOnce(usize) -> (usize, usize),
) -> Option<TraverserValue<S>> {
    match stored {
        ElementProperty::Array(items) => {
            let (start, end) = bounds(items.len());
            Some(TraverserValue::Computed(
                ElementProperty::Array(items[start..end].to_vec()),
                None,
            ))
        }
        ElementProperty::Object(entries) => {
            let (start, end) = bounds(entries.len());
            let sliced: crate::value::PropertyMap = entries
                .iter()
                .skip(start)
                .take(end - start)
                .map(|(key, value)| (key.clone(), value.to_logical()))
                .collect();
            Some(TraverserValue::Computed(
                ElementProperty::Object(Box::new(sliced)),
                None,
            ))
        }
        ElementProperty::Map(entries) => {
            let (start, end) = bounds(entries.len());
            let sliced = ElementProperty::from_entries(
                entries
                    .iter()
                    .skip(start)
                    .take(end - start)
                    .map(|(key, value)| (key.clone(), value.clone())),
            );
            Some(TraverserValue::Computed(sliced, None))
        }
        ElementProperty::Null
        | ElementProperty::String(_)
        | ElementProperty::Compressed(_)
        | ElementProperty::Deduped(_)
        | ElementProperty::Number(_)
        | ElementProperty::Boolean(_)
        | ElementProperty::Uuid(_)
        | ElementProperty::Temporal(_)
        | ElementProperty::Vector(_)
        | ElementProperty::Vertex(_)
        | ElementProperty::Edge(_) => None,
    }
}

/// The result of [`slice_local`].
pub(crate) enum LocalSlice<S: GraphStorage> {
    /// The sliced collection, of the same kind as the input (a list stays a list — even with one
    /// element left —, a `Record` stays a `Record`, an object stays an object), or a
    /// non-collection value passed through unchanged.
    Value(TraverserValue<S>),
    /// The input was a path, which the range family does not slice (upstream returns a sub-path).
    Path,
}

/// Slices the list or map a traverser holds to the element range `bounds(len)` returns
/// (`start <= end <= len`), for the `limit/skip/tail/range(Scope.local)` family.
///
/// Owned lists and maps are moved and truncated/drained, never cloned element-wise; a lazy
/// property handle holding a list or map clones only the selected slice (into a `Computed`
/// value). A `Record` keeps its `Arc` key slice when nothing is cut and gets a new one otherwise.
/// Any non-collection value — including a scalar property handle, which stays lazy — is returned
/// unchanged, as TinkerPop's `RangeLocalStep` does (`range(local, 20, 30)` on `29` → `29`).
pub(crate) fn slice_local<S: GraphStorage>(
    context: &TraverserContext<'_, S>,
    value: TraverserValue<S>,
    bounds: impl FnOnce(usize) -> (usize, usize),
) -> Result<LocalSlice<S>, crate::error::GraphError> {
    Ok(LocalSlice::Value(match value {
        TraverserValue::Array(mut items) => {
            let (start, end) = bounds(items.len());
            keep_vec(&mut items, start, end);
            TraverserValue::Array(items)
        }
        TraverserValue::Object(mut map) => {
            let (start, end) = bounds(map.len());
            keep_map(&mut map.0, start, end);
            TraverserValue::Object(map)
        }
        TraverserValue::Record { keys, mut values } => {
            let len = values.len();
            let (start, end) = bounds(len);
            if start == 0 && end == len {
                TraverserValue::Record { keys, values }
            } else {
                keep_vec(&mut values, start, end);
                let keys = std::sync::Arc::new(
                    keys.iter()
                        .skip(start)
                        .take(end - start)
                        .cloned()
                        .collect::<Vec<_>>(),
                );
                TraverserValue::Record { keys, values }
            }
        }
        TraverserValue::Computed(ElementProperty::Array(mut items), node) => {
            let (start, end) = bounds(items.len());
            keep_vec(&mut items, start, end);
            TraverserValue::Computed(ElementProperty::Array(items), node)
        }
        TraverserValue::Computed(ElementProperty::Object(mut entries), node) => {
            let (start, end) = bounds(entries.len());
            keep_map(entries.indexed_mut(), start, end);
            TraverserValue::Computed(ElementProperty::Object(entries), node)
        }
        TraverserValue::Path(_) => return Ok(LocalSlice::Path),
        TraverserValue::VertexProperty(_, _, _) | TraverserValue::EdgeProperty(_, _, _) => {
            match stored_value(context, &value)?.and_then(|stored| slice_stored(stored, bounds)) {
                Some(sliced) => sliced,
                None => value,
            }
        }
        TraverserValue::Computed(_, _)
        | TraverserValue::Vertex(_)
        | TraverserValue::Edge(_)
        | TraverserValue::PathProperty(_)
        | TraverserValue::Entry(_)
        | TraverserValue::Schema(_)
        | TraverserValue::Graph(_) => value,
    }))
}

/// True when `value` is a null: a computed `null`, or a property handle whose element does not
/// carry the property or stores `null` in it. The `id`/`label` handles are never null.
pub(crate) fn is_null<S: GraphStorage>(
    context: &TraverserContext<'_, S>,
    value: &TraverserValue<S>,
) -> Result<bool, crate::error::GraphError> {
    Ok(match value {
        TraverserValue::Computed(property, _) => matches!(property, ElementProperty::Null),
        TraverserValue::VertexProperty(key, _, _) | TraverserValue::EdgeProperty(key, _, _) => {
            key != "id"
                && key != "label"
                && matches!(
                    stored_value(context, value)?,
                    None | Some(ElementProperty::Null)
                )
        }
        TraverserValue::Vertex(_)
        | TraverserValue::Edge(_)
        | TraverserValue::Path(_)
        | TraverserValue::Record { .. }
        | TraverserValue::Object(_)
        | TraverserValue::PathProperty(_)
        | TraverserValue::Entry(_)
        | TraverserValue::Array(_)
        | TraverserValue::Schema(_)
        | TraverserValue::Graph(_) => false,
    })
}

/// One element of the list a per-element `Scope::Local` step transforms.
pub(crate) enum LocalItem<'a, S: GraphStorage> {
    /// An element of a stream list (`fold()`); may be a lazy handle.
    Value(&'a TraverserValue<S>),
    /// An element of a materialized list (a list constant, `cap()`) or of a stored list property.
    Stored(&'a ElementProperty),
}

/// What [`map_local_elements`] / [`map_local_strings`] made of a traverser value.
pub(crate) enum LocalMapped<S: GraphStorage> {
    /// The finished result: the list of the mapped elements, in order (or `null` for a `null`
    /// input of [`map_local_strings`]).
    Done(TraverserValue<S>),
    /// The value is not a list: returned unchanged, for the step's single-value (global) path.
    Single(TraverserValue<S>),
}

/// Maps every element of the list a traverser holds through `map`, for the per-element
/// `Scope::Local` steps (`to_upper(local)`, `as_string(local)`, ...).
///
/// Lists are `Array` (`fold()`), `Computed(ElementProperty::Array)` and a property handle whose
/// stored value is an array (borrowed, never cloned whole). The result is a new list of the same
/// length. Any other value is handed back as [`LocalMapped::Single`]. `map` gets each element's
/// 0-based index, for the diagnostic of an element it cannot take; the first error stops the map.
pub(crate) fn map_local_elements<S: GraphStorage>(
    context: &TraverserContext<'_, S>,
    value: TraverserValue<S>,
    mut map: impl FnMut(usize, LocalItem<'_, S>) -> TraverserResult<ElementProperty>,
) -> TraverserResult<LocalMapped<S>> {
    let mapped = match &value {
        TraverserValue::Array(items) => items
            .iter()
            .enumerate()
            .map(|(index, item)| map(index, LocalItem::Value(item)))
            .collect::<TraverserResult<Vec<_>>>()?,
        TraverserValue::Computed(ElementProperty::Array(items), _) => items
            .iter()
            .enumerate()
            .map(|(index, item)| map(index, LocalItem::Stored(item)))
            .collect::<TraverserResult<Vec<_>>>()?,
        TraverserValue::VertexProperty(_, _, _) | TraverserValue::EdgeProperty(_, _, _) => {
            match stored_value(context, &value)? {
                Some(ElementProperty::Array(items)) => items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| map(index, LocalItem::Stored(item)))
                    .collect::<TraverserResult<Vec<_>>>()?,
                Some(_) | None => return Ok(LocalMapped::Single(value)),
            }
        }
        TraverserValue::Computed(_, _)
        | TraverserValue::Vertex(_)
        | TraverserValue::Edge(_)
        | TraverserValue::Path(_)
        | TraverserValue::Record { .. }
        | TraverserValue::Object(_)
        | TraverserValue::PathProperty(_)
        | TraverserValue::Entry(_)
        | TraverserValue::Schema(_)
        | TraverserValue::Graph(_) => return Ok(LocalMapped::Single(value)),
    };
    Ok(LocalMapped::Done(TraverserValue::Computed(
        ElementProperty::Array(mapped),
        None,
    )))
}

/// What a local string step accepts, in TinkerPop's words (its error message).
const STRING_ACCEPTS: &str = "string or list of strings";

/// Applies a string transformation `f` to one list element of a local string step: a string is
/// transformed, a `null` stays `null`, anything else fails with
/// [`ValueError::LocalElementType`](crate::error::ValueError::LocalElementType) naming `step`.
fn local_string_item<S: GraphStorage>(
    context: &TraverserContext<'_, S>,
    step: &'static str,
    index: usize,
    item: LocalItem<'_, S>,
    f: &impl Fn(&str) -> TraverserResult<ElementProperty>,
) -> TraverserResult<ElementProperty> {
    match item {
        LocalItem::Stored(ElementProperty::Null) => Ok(ElementProperty::Null),
        LocalItem::Stored(ElementProperty::String(text)) => {
            bounded_string(context, f(text.as_str())?)
        }
        LocalItem::Stored(ElementProperty::Compressed(text)) => {
            let value = text.with_text(|text| f(text))?;
            bounded_string(context, value)
        }
        LocalItem::Stored(ElementProperty::Deduped(text)) => {
            bounded_string(context, f(text.as_str())?)
        }
        LocalItem::Stored(
            other @ (ElementProperty::Number(_)
            | ElementProperty::Boolean(_)
            | ElementProperty::Uuid(_)
            | ElementProperty::Temporal(_)
            | ElementProperty::Vector(_)
            | ElementProperty::Array(_)
            | ElementProperty::Object(_)
            | ElementProperty::Map(_)
            | ElementProperty::Vertex(_)
            | ElementProperty::Edge(_)),
        ) => Err(Box::new(TraverserError::local_element_type(
            step,
            STRING_ACCEPTS,
            index,
            crate::graph::schema::value_type_name(other),
        ))),
        LocalItem::Value(value) => {
            if is_null(context, value)? {
                return Ok(ElementProperty::Null);
            }
            let text = match value {
                TraverserValue::Computed(_, _)
                | TraverserValue::VertexProperty(_, _, _)
                | TraverserValue::EdgeProperty(_, _, _) => {
                    Traverser::as_string_ref(context, value)?
                }
                // Elements, paths and containers are never strings.
                TraverserValue::Vertex(_)
                | TraverserValue::Edge(_)
                | TraverserValue::Path(_)
                | TraverserValue::Record { .. }
                | TraverserValue::Object(_)
                | TraverserValue::PathProperty(_)
                | TraverserValue::Entry(_)
                | TraverserValue::Array(_)
                | TraverserValue::Schema(_)
                | TraverserValue::Graph(_) => None,
            };
            match text {
                Some(text) => bounded_string(context, f(&text)?),
                None => Err(Box::new(TraverserError::local_element_type(
                    step,
                    STRING_ACCEPTS,
                    index,
                    Traverser::observed_type_name(context, value)?,
                ))),
            }
        }
    }
}

/// `value` after checking that a string result of a string-building step is within
/// `traversal.max_string_bytes` (one comparison; any other value passes unchanged). For steps
/// whose result cannot be much longer than their input (`to_upper()`, `substring()`, the trims);
/// a step that can multiply the length (`replace()`, `concat()`) checks before it builds.
#[inline]
pub(crate) fn bounded_string<S: GraphStorage>(
    context: &TraverserContext<'_, S>,
    value: ElementProperty,
) -> TraverserResult<ElementProperty> {
    if let ElementProperty::String(text) = &value {
        context.check_string_bytes(text.len())?;
    }
    Ok(value)
}

/// The `Scope::Local` form of a per-value string step (`to_upper`, `trim`, `length`, `replace`,
/// `substring`, ...): a list becomes the list of `f` applied to each string element (a `null`
/// element stays `null`, any other element is an error naming the Gremlin `step`), a `null`
/// input stays `null`, and any other value is handed back as [`LocalMapped::Single`] for the
/// step's ordinary single-string path.
pub(crate) fn map_local_strings<S: GraphStorage>(
    context: &TraverserContext<'_, S>,
    value: TraverserValue<S>,
    step: &'static str,
    f: impl Fn(&str) -> TraverserResult<ElementProperty>,
) -> TraverserResult<LocalMapped<S>> {
    if is_null(context, &value)? {
        return Ok(LocalMapped::Done(TraverserValue::Computed(
            ElementProperty::Null,
            None,
        )));
    }
    map_local_elements(context, value, |index, item| {
        local_string_item(context, step, index, item, &f)
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::value::PropertyHandleKind;
    use compact_str::CompactString;

    use super::*;
    use crate::TraversalGraph;
    use crate::traversal::context::{TraversalStrategy, TraverserContext};
    use crate::value::StreamMap;

    /// The shape's name, so a test can compare shapes without a `_ =>` arm.
    fn shape_name<S: GraphStorage>(shape: &LocalShape<S>) -> (&'static str, usize) {
        match shape {
            LocalShape::List(items) => ("list", items.len()),
            LocalShape::Map(entries) => ("map", entries.len()),
            LocalShape::Path(path) => ("path", path.len()),
            LocalShape::Scalar(_) => ("scalar", 1),
        }
    }

    fn modern_with_collections() -> TraversalGraph {
        let mut storage = TraversalGraph::tinkerpop_modern();
        let list = ElementProperty::Array(vec![1i64.into(), 2i64.into(), 3i64.into()]);
        let mut map = crate::value::PropertyMap::default();
        map.insert(CompactString::new("a"), 1i64.into());
        map.insert(CompactString::new("b"), 2i64.into());
        let _ = storage.traversal_mut().v("1").property("list", list).next();
        let _ = storage
            .traversal_mut()
            .v("1")
            .property("map", ElementProperty::Object(Box::new(map)))
            .next();
        storage
    }

    #[test]
    fn classify_every_kind() {
        let mut storage = modern_with_collections();
        let node = GraphStorage::get_vertex_by_id(&storage, "1")
            .unwrap()
            .expect("vertex 1");
        let edge = GraphStorage::get_edge_by_id(&storage, "0")
            .unwrap()
            .expect("edge 0");
        let context =
            TraverserContext::new(&mut storage, TraversalStrategy::none(), Default::default());

        let number = || TraverserValue::<TraversalGraph>::Computed(7i64.into(), None);
        let mut object: StreamMap<TraversalGraph> = StreamMap::default();
        object.insert(number(), number());
        let cases: Vec<(TraverserValue<TraversalGraph>, (&str, usize))> = vec![
            (TraverserValue::Array(vec![number(), number()]), ("list", 2)),
            (
                TraverserValue::Computed(
                    ElementProperty::Array(vec![1i64.into(), ElementProperty::Null]),
                    None,
                ),
                ("list", 2),
            ),
            (
                TraverserValue::VertexProperty("list".into(), node, PropertyHandleKind::Value),
                ("list", 3),
            ),
            (
                TraverserValue::VertexProperty("map".into(), node, PropertyHandleKind::Value),
                ("map", 2),
            ),
            (
                TraverserValue::VertexProperty("age".into(), node, PropertyHandleKind::Value),
                ("scalar", 1),
            ),
            (
                TraverserValue::VertexProperty("missing".into(), node, PropertyHandleKind::Value),
                ("scalar", 1),
            ),
            (
                TraverserValue::EdgeProperty("weight".into(), edge, PropertyHandleKind::Value),
                ("scalar", 1),
            ),
            (TraverserValue::Object(object), ("map", 1)),
            (
                TraverserValue::Record {
                    keys: Arc::new(vec!["a".into(), "b".into(), "c".into()]),
                    values: vec![number(), number(), number()],
                },
                ("map", 3),
            ),
            (TraverserValue::Path(TraverserPath::default()), ("path", 0)),
            (number(), ("scalar", 1)),
            (
                TraverserValue::Computed(ElementProperty::Null, None),
                ("scalar", 1),
            ),
            (TraverserValue::Vertex(node), ("scalar", 1)),
            (TraverserValue::Edge(edge), ("scalar", 1)),
            (
                TraverserValue::Entry(Box::new((number(), number()))),
                ("scalar", 1),
            ),
        ];
        for (value, expected) in cases {
            let rendered = format!("{value:?}");
            let len = local_len(&context, &value).expect("local_len");
            let shape = classify(&context, value).expect("classify");
            assert_eq!(shape_name(&shape), expected, "classify({rendered})");
            let expected_len = match expected.0 {
                "scalar" => 1,
                _ => expected.1,
            };
            assert_eq!(len, expected_len, "local_len({rendered})");
        }
    }

    /// A scalar property handle is not materialized by `classify`: it stays the lazy handle.
    #[test]
    fn scalar_property_stays_lazy() {
        let mut storage = TraversalGraph::tinkerpop_modern();
        let node = GraphStorage::get_vertex_by_id(&storage, "1")
            .unwrap()
            .expect("vertex 1");
        let context =
            TraverserContext::new(&mut storage, TraversalStrategy::none(), Default::default());
        let shape = classify(
            &context,
            TraverserValue::VertexProperty("age".into(), node, PropertyHandleKind::Value),
        )
        .expect("classify");
        match shape {
            LocalShape::Scalar(TraverserValue::VertexProperty(key, _, _)) => assert_eq!(key, "age"),
            LocalShape::Scalar(other) => panic!("expected a lazy handle, got {other:?}"),
            LocalShape::List(_) | LocalShape::Map(_) | LocalShape::Path(_) => {
                panic!("expected a scalar")
            }
        }
    }
}
