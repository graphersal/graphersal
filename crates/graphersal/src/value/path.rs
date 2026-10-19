use compact_str::CompactString;

use crate::graph::GraphStorage;
use crate::value::TraverserValue;

/// An owned, materialized path: the objects a traverser went through, as `path()` returns them.
///
/// During execution paths live in the per-execution path arena; this type only exists inside
/// `TraverserValue::Path`, so it never refers back to an execution.
pub struct TraverserPath<S: GraphStorage> {
    pub(crate) steps: Vec<TraverserPathStep<S>>,
}

impl<S: GraphStorage> Default for TraverserPath<S> {
    fn default() -> Self {
        Self { steps: Vec::new() }
    }
}

impl<S: GraphStorage> std::fmt::Debug for TraverserPath<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TraverserPath")
            .field("steps", &self.steps)
            .finish()
    }
}

impl<S: GraphStorage> Clone for TraverserPath<S> {
    fn clone(&self) -> Self {
        Self {
            steps: self.steps.clone(),
        }
    }
}

impl<S: GraphStorage> PartialEq for TraverserPath<S> {
    fn eq(&self, other: &Self) -> bool {
        self.steps == other.steps
    }
}

impl<S: GraphStorage> Eq for TraverserPath<S> {}

impl<S: GraphStorage> core::hash::Hash for TraverserPath<S> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.steps.hash(state);
    }
}

pub(crate) struct TraverserPathStep<S: GraphStorage> {
    pub(crate) value: TraverserValue<S>,
    pub(crate) label: Option<Vec<CompactString>>,
}

impl<S: GraphStorage> std::fmt::Debug for TraverserPathStep<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TraverserPathStep")
            .field("value", &self.value)
            .field("label", &self.label)
            .finish()
    }
}

impl<S: GraphStorage> Clone for TraverserPathStep<S> {
    fn clone(&self) -> Self {
        Self {
            value: self.value.clone(),
            label: self.label.clone(),
        }
    }
}

impl<S: GraphStorage> PartialEq for TraverserPathStep<S> {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value && self.label == other.label
    }
}

impl<S: GraphStorage> Eq for TraverserPathStep<S> {}

impl<S: GraphStorage> core::hash::Hash for TraverserPathStep<S> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.value.hash(state);
        self.label.hash(state);
    }
}

impl<S: GraphStorage> TraverserPath<S> {
    /// A path of `objects`, first position first, with no step labels.
    pub(crate) fn from_objects(objects: impl IntoIterator<Item = TraverserValue<S>>) -> Self {
        Self {
            steps: objects
                .into_iter()
                .map(|value| TraverserPathStep { value, label: None })
                .collect(),
        }
    }

    /// Number of objects in the path.
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// Returns `true` when the path holds no object.
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Iterates the objects of the path, first position first.
    pub fn objects(&self) -> impl Iterator<Item = &TraverserValue<S>> {
        self.steps.iter().map(|step| &step.value)
    }

    /// Returns the step labels attached to the object at `position`, if any.
    pub fn labels(&self, position: usize) -> Option<&[CompactString]> {
        self.steps
            .get(position)
            .and_then(|step| step.label.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::TraversalGraph;
    use crate::value::ElementProperty;
    use fnv::FnvHasher;
    use std::hash::{Hash, Hasher};

    fn calculate_hash<T: Hash>(t: &T) -> u64 {
        let mut s = FnvHasher::default();
        t.hash(&mut s);
        s.finish()
    }

    #[test]
    fn test_traverser_path_equality_and_hash() {
        let val1: TraverserValue<TraversalGraph> =
            TraverserValue::Computed(ElementProperty::String("a".into()), None);
        let val2: TraverserValue<TraversalGraph> =
            TraverserValue::Computed(ElementProperty::String("b".into()), None);

        let path1 = TraverserPath {
            steps: vec![TraverserPathStep {
                value: val1.clone(),
                label: Some(vec![CompactString::new("label1")]),
            }],
        };

        let path1_clone = path1.clone();

        let path2 = TraverserPath {
            steps: vec![TraverserPathStep {
                value: val2,
                label: Some(vec![CompactString::new("label1")]),
            }],
        };

        assert_eq!(path1, path1);
        assert_eq!(path1, path1_clone);
        assert_ne!(path1, path2);
        assert_eq!(calculate_hash(&path1), calculate_hash(&path1_clone));
    }
}
