//! The value-depth limit: how deeply arrays, maps and paths may nest inside one value.
//!
//! Almost every consumer of a value (conversion to a script value, hashing in `dedup()`, schema
//! inference, rendering, `Drop`) walks it recursively, so a value nested thousands of levels deep
//! overflows the native stack and aborts the process. Instead of making every consumer
//! iterative, the depth is bounded where a value enters the engine (a script value, a host
//! parameter, a `jpath(..)` write, a `repeat()` loop that wraps its value again on every
//! iteration): no value deeper than the limit can exist afterwards, so every recursive reader
//! stays bounded. The measurement here is iterative and stops at the first level past the
//! limit, so it is safe on any input.
//!
//! The depth of a scalar (also an element handle) is 0; each array, object, map, map entry or
//! path around a value adds one level: `[1]` has depth 1, `[[1]]` and `{"a": [1]}` depth 2.

use crate::graph::GraphStorage;
use crate::{ElementProperty, TraverserValue};

/// Default of the value-depth limit (`ScriptLimits::max_value_depth`, the execution option
/// `traversal.max_value_depth`): 128 nested levels, the recursion limit `serde_json` applies to
/// JSON input, far beyond any real document and well inside the native stack of every thread.
pub const DEFAULT_MAX_VALUE_DEPTH: usize = 128;

/// Whether `value` is an array, object, map or element map (a value with children).
#[inline]
fn is_container(value: &ElementProperty) -> bool {
    matches!(
        value,
        ElementProperty::Array(_)
            | ElementProperty::Object(_)
            | ElementProperty::Map(_)
            | ElementProperty::Vertex(_)
            | ElementProperty::Edge(_)
    )
}

/// `true` when `value` nests deeper than `max` levels (see the module docs for the counting).
/// Iterative, and stops at the first level past `max`; a scalar costs one `match`.
pub(crate) fn property_exceeds_depth(value: &ElementProperty, max: usize) -> bool {
    if !is_container(value) {
        return false;
    }
    let mut stack: Vec<(&ElementProperty, usize)> = vec![(value, 1)];
    while let Some((current, depth)) = stack.pop() {
        if depth > max {
            return true;
        }
        let child = depth + 1;
        match current {
            // A vector is one value (its components are not nested values).
            ElementProperty::Vector(_) | ElementProperty::Blob(_) => {}
            ElementProperty::Array(items) => {
                stack.extend(items.iter().filter(|c| is_container(c)).map(|c| (c, child)));
            }
            ElementProperty::Object(_) | ElementProperty::Vertex(_) | ElementProperty::Edge(_) => {
                stack.extend(
                    current
                        .map_entries()
                        .into_iter()
                        .flat_map(|element| element.values())
                        .filter(|c| is_container(c))
                        .map(|c| (c, child)),
                );
            }
            ElementProperty::Map(map) => {
                for (key, item) in map.iter() {
                    for c in [key, item] {
                        if is_container(c) {
                            stack.push((c, child));
                        }
                    }
                }
            }
            ElementProperty::Null
            | ElementProperty::String(_)
            | ElementProperty::Compressed(_)
            | ElementProperty::Deduped(_)
            | ElementProperty::Number(_)
            | ElementProperty::Boolean(_)
            | ElementProperty::Uuid(_)
            | ElementProperty::Temporal(_) => {}
        }
    }
    false
}

/// One entry of the explicit stack of [`traverser_value_exceeds_depth`].
enum Node<'a, S: GraphStorage> {
    Stream(&'a TraverserValue<S>),
    Property(&'a ElementProperty),
}

/// `true` when the stream value `value` nests deeper than `max` levels. An element handle or a
/// scalar costs one `match`; containers are walked iteratively up to the first level past `max`.
pub(crate) fn traverser_value_exceeds_depth<S: GraphStorage>(
    value: &TraverserValue<S>,
    max: usize,
) -> bool {
    match value {
        TraverserValue::Computed(property, _) => return property_exceeds_depth(property, max),
        TraverserValue::VertexProperty(..)
        | TraverserValue::Vertex(_)
        | TraverserValue::Edge(_)
        | TraverserValue::EdgeProperty(..)
        | TraverserValue::Schema(_)
        | TraverserValue::Graph(_) => return false,
        TraverserValue::PathProperty(_)
        | TraverserValue::Path(_)
        | TraverserValue::Record { .. }
        | TraverserValue::Object(_)
        | TraverserValue::Entry(_)
        | TraverserValue::Array(_) => {}
    }
    let mut stack: Vec<(Node<'_, S>, usize)> = vec![(Node::Stream(value), 0)];
    while let Some((node, depth)) = stack.pop() {
        let value = match node {
            Node::Property(property) => {
                if property_exceeds_depth(property, max.saturating_sub(depth)) {
                    return true;
                }
                continue;
            }
            Node::Stream(value) => value,
        };
        let child = depth + 1;
        match value {
            TraverserValue::Computed(property, _) => stack.push((Node::Property(property), depth)),
            TraverserValue::VertexProperty(..)
            | TraverserValue::Vertex(_)
            | TraverserValue::Edge(_)
            | TraverserValue::EdgeProperty(..)
            | TraverserValue::Schema(_)
            | TraverserValue::Graph(_) => {}
            TraverserValue::PathProperty(entry) => {
                if child > max {
                    return true;
                }
                stack.push((Node::Property(&entry.1), child));
            }
            TraverserValue::Path(path) => {
                if child > max {
                    return true;
                }
                stack.extend(path.steps.iter().map(|s| (Node::Stream(&s.value), child)));
            }
            TraverserValue::Record { values, .. } => {
                if child > max {
                    return true;
                }
                stack.extend(values.iter().map(|v| (Node::Stream(v), child)));
            }
            TraverserValue::Object(map) => {
                if child > max {
                    return true;
                }
                for (key, item) in map {
                    stack.push((Node::Stream(key), child));
                    stack.push((Node::Stream(item), child));
                }
            }
            TraverserValue::Entry(entry) => {
                if child > max {
                    return true;
                }
                stack.push((Node::Stream(&entry.0), child));
                stack.push((Node::Stream(&entry.1), child));
            }
            TraverserValue::Array(items) => {
                if child > max {
                    return true;
                }
                stack.extend(items.iter().map(|v| (Node::Stream(v), child)));
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TraversalGraph;

    fn nested(depth: usize) -> ElementProperty {
        let mut value = ElementProperty::from(1);
        for _ in 0..depth {
            value = ElementProperty::Array(vec![value]);
        }
        value
    }

    #[test]
    fn scalars_have_depth_zero() {
        assert!(!property_exceeds_depth(&ElementProperty::from(1), 0));
        assert!(property_exceeds_depth(&nested(1), 0));
        assert!(!property_exceeds_depth(&nested(1), 1));
    }

    #[test]
    fn the_limit_is_inclusive() {
        assert!(!property_exceeds_depth(&nested(128), 128));
        assert!(property_exceeds_depth(&nested(129), 128));
        // The walk is iterative and stops early. (Not much deeper here: dropping the test value
        // itself recurses, which is exactly why such values must never be built.)
        assert!(property_exceeds_depth(&nested(2_000), 128));
        assert!(!property_exceeds_depth(&nested(2_000), 2_000));
    }

    #[test]
    fn stream_values_count_their_own_levels() {
        type V = TraverserValue<TraversalGraph>;
        let leaf: V = TraverserValue::Computed(nested(2), None);
        let wrapped: V = TraverserValue::Array(vec![TraverserValue::Array(vec![leaf])]);
        assert!(!traverser_value_exceeds_depth(&wrapped, 4));
        assert!(traverser_value_exceeds_depth(&wrapped, 3));
        let mut graph = TraversalGraph::default();
        let id = graph
            .insert_vertex(None, vec![], Default::default())
            .expect("a vertex");
        let vertex: V = TraverserValue::Vertex(id);
        assert!(!traverser_value_exceeds_depth(&vertex, 0));
    }
}
