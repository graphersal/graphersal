//! `gql:lookup(pattern, equality)`: the correlated lookup of a GQL component (feature `gql`,
//! internal).

use std::sync::Arc;

use crate::gql::eval::{Truth, Value, equals};
use crate::gql::runtime::{Binding, Row, RowExpr, RowValue, eval_error, history_read, labels_read};
use crate::graph::{GraphStorage, PropertyIndexRange};
use crate::traversal::context::tick_deadline;
use crate::traversal::optimizer::path_analysis::PathRequirement;
use crate::value::scalar::compare_scalars;
use crate::*;

/// What a [`StepGqlLookup`] looks the vertices up by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GqlLookupKey {
    /// `element_id(n) = e`: the vertex whose id is `e` (`GraphStorage::get_vertex_by_id`).
    Id,
    /// `n.key = e`: the property index on `key` of every scan label
    /// (`GraphStorage::vertices_by_property_index`).
    Property(CompactString),
}

/// Starts a component of a GQL pattern that is not the root scan at the vertices matching one
/// equality whose other side is known on the incoming row (design M3, E5): per traverser it
/// evaluates that side `e`, then
///
/// - `NULL`: no vertex (`x = NULL` is never TRUE);
/// - [`GqlLookupKey::Id`] and a string: the vertex with that id, if any;
/// - [`GqlLookupKey::Property`] and a string, a number (not `NaN`), a boolean or a uuid: the
///   vertices of the property index slice `[e, e]` of every scan label (a superset: numbers are
///   one slice across `int64` and `float64`, so `1` finds `1.0`), each kept only when GQL `=`
///   holds (the same equality as `gql:filter`, Q5);
/// - any other value, or a storage that serves no index for a scan label: every vertex of the
///   scan labels (every vertex without scan labels), each kept only when GQL `=` holds: exactly
///   the rows of the scan + filter it replaces, only slower.
///
/// The scan labels themselves are not checked here: the lowering emits them as `has_label()`
/// after it, followed by the node's other label factors and conjuncts, as after a scan.
///
/// A flat map: every output is derived from its input (rule 20: same path, bulk and sack, like
/// a mid-traversal `v()`). The value reads the step labels its expression reads (rule 13).
/// `context.tick()` runs once per candidate. Access: `Read` on vertex data, like `v()` (the arm
/// in `GraphStep::for_each_access_in`). Built only by the GQL lowering; not a DSL step.
pub(crate) struct StepGqlLookup<S: GraphStorage> {
    /// The whole query, for the error excerpt.
    query: Arc<str>,
    /// The node pattern as rendered, `(n:person)`.
    pattern: Arc<str>,
    /// The equality as written.
    equality: Arc<str>,
    /// The other side of the equality.
    value: Arc<RowExpr<S>>,
    /// The source of each variable slot the value reads.
    bindings: Arc<[Binding]>,
    /// What the vertices are looked up by.
    key: GqlLookupKey,
    /// The scan labels (empty: none).
    labels: Arc<[String]>,
    /// Whether an index served the lookup when it was planned (display only).
    indexed: bool,
    /// The step labels the value reads (sorted).
    path_labels: Arc<[CompactString]>,
    /// The labels it reads in every occurrence (group variables, sorted).
    history: Arc<[CompactString]>,
}

impl<S: GraphStorage> Clone for StepGqlLookup<S> {
    fn clone(&self) -> Self {
        Self {
            query: Arc::clone(&self.query),
            pattern: Arc::clone(&self.pattern),
            equality: Arc::clone(&self.equality),
            value: Arc::clone(&self.value),
            bindings: Arc::clone(&self.bindings),
            key: self.key.clone(),
            labels: Arc::clone(&self.labels),
            indexed: self.indexed,
            path_labels: Arc::clone(&self.path_labels),
            history: Arc::clone(&self.history),
        }
    }
}

impl<S: GraphStorage> fmt::Display for StepGqlLookup<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // String literals, so the camelCase respelling leaves the GQL text alone.
        write!(
            f,
            "gql:lookup({}, {})",
            crate::value::literal::rhai_string(&self.pattern),
            crate::value::literal::rhai_string(&self.equality)
        )?;
        match &self.key {
            GqlLookupKey::Id => f.write_str(" [id]"),
            GqlLookupKey::Property(key) => {
                f.write_str(if self.indexed {
                    " [index: "
                } else {
                    " [scan: "
                })?;
                for (at, label) in self.labels.iter().enumerate() {
                    if at > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "vertex {label}.{key}")?;
                }
                f.write_str("]")
            }
        }
    }
}

/// The parts of a [`StepGqlLookup`] the lowering decides.
pub(crate) struct GqlLookup<S: GraphStorage> {
    /// The node pattern as rendered, `(n:person)`.
    pub(crate) pattern: String,
    /// The equality as written.
    pub(crate) equality: String,
    /// The other side of the equality.
    pub(crate) value: RowExpr<S>,
    /// What the vertices are looked up by.
    pub(crate) key: GqlLookupKey,
    /// The scan labels (empty: none).
    pub(crate) labels: Vec<String>,
    /// Whether an index served the lookup when it was planned (display only).
    pub(crate) indexed: bool,
}

impl<S: GraphStorage> StepGqlLookup<S> {
    pub(crate) fn new(query: Arc<str>, lookup: GqlLookup<S>, bindings: Arc<[Binding]>) -> Self {
        let path_labels = labels_read([&lookup.value], &bindings).into();
        let history = history_read([&lookup.value], &bindings).into();
        Self {
            query,
            pattern: lookup.pattern.into(),
            equality: lookup.equality.into(),
            value: Arc::new(lookup.value),
            bindings,
            key: lookup.key,
            labels: lookup.labels.into(),
            indexed: lookup.indexed,
            path_labels,
            history,
        }
    }

    /// Whether vertex `v` satisfies `lookup = value` with GQL `=` (TRUE only).
    fn holds(&self, storage: &S, v: S::VertexId, value: &RowValue<S>) -> bool {
        let found = match &self.key {
            GqlLookupKey::Id => storage
                .vertex_id(v)
                .map(|id| Value::Scalar(ElementProperty::String(CompactString::from(id)))),
            GqlLookupKey::Property(key) => {
                let stored = storage
                    .vertex_properties(v)
                    .and_then(|properties| properties.get(key.as_str()));
                // A plain number, string or boolean against a scalar: GQL `=` is the scalar
                // comparison, without cloning the stored value (the candidate loop of a lookup
                // no index serves).
                if let (
                    Some(
                        stored @ (ElementProperty::Number(_)
                        | ElementProperty::String(_)
                        | ElementProperty::Boolean(_)),
                    ),
                    Value::Scalar(wanted),
                ) = (stored, value)
                {
                    return compare_scalars(stored, wanted) == Some(std::cmp::Ordering::Equal);
                }
                stored.map(|stored| Value::from_property(stored.clone()))
            }
        };
        equals(&found.unwrap_or(Value::Null), value) == Truth::True
    }

    /// The vertices of the property index slice `[value, value]` of every scan label, each
    /// once (a superset); `None` when the value is not an index key the lookup serves, there
    /// is no scan label, or the storage serves no index for one of the labels.
    fn by_index(&self, storage: &S, key: &str, value: &RowValue<S>) -> Option<Vec<S::VertexId>> {
        let Value::Scalar(scalar) = value else {
            return None;
        };
        let servable = match scalar {
            ElementProperty::String(_) | ElementProperty::Boolean(_) | ElementProperty::Uuid(_) => {
                true
            }
            ElementProperty::Number(number) => !number.as_f64().is_some_and(f64::is_nan),
            _ => false,
        };
        if !servable || self.labels.is_empty() {
            return None;
        }
        let range = PropertyIndexRange::equal(scalar.clone());
        let mut found = Vec::new();
        let dedup = self.labels.len() > 1;
        let mut seen = std::collections::HashSet::new();
        for label in self.labels.iter() {
            for v in storage.vertices_by_property_index(label, key, &range)? {
                if !dedup || seen.insert(v) {
                    found.push(v);
                }
            }
        }
        Some(found)
    }

    /// Calls `check` with every vertex of the scan labels (every vertex without scan labels),
    /// each once.
    fn scan(
        &self,
        storage: &S,
        mut check: impl FnMut(S::VertexId) -> TraverserResult<()>,
    ) -> TraverserResult<()> {
        match &self.labels[..] {
            [] => storage.vertices().try_for_each(check),
            [label] => storage.vertices_by_label(label).try_for_each(check),
            labels => {
                let mut seen = std::collections::HashSet::new();
                for label in labels {
                    for v in storage.vertices_by_label(label) {
                        if seen.insert(v) {
                            check(v)?;
                        }
                    }
                }
                Ok(())
            }
        }
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Appends `gql:lookup(..)`; see [`StepGqlLookup`].
    pub(crate) fn gql_lookup(
        self,
        query: Arc<str>,
        lookup: GqlLookup<S>,
        bindings: Arc<[Binding]>,
    ) -> Self {
        self.push_step(GraphStep::FlatMap(
            "gql:lookup",
            Box::new(StepGqlLookup::new(query, lookup, bindings)),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Appends `gql:lookup(..)`; see [`StepGqlLookup`].
    pub(crate) fn gql_lookup(
        self,
        query: Arc<str>,
        lookup: GqlLookup<S>,
        bindings: Arc<[Binding]>,
    ) -> Self {
        self.push_step(GraphStep::FlatMap(
            "gql:lookup",
            Box::new(StepGqlLookup::new(query, lookup, bindings)),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatMap<'g, S> for StepGqlLookup<S> {
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
        output: &mut Vec<Traverser<S>>,
    ) -> TraverserResult<()> {
        let value = {
            let row = Row::new(context, &source, &self.bindings);
            let value = self.value.eval(&row);
            // A limit error the row kept (a refused `Env::reserve`) wins over the evaluator's
            // fallback error.
            row.finish()?;
            value.map_err(|error| Box::new(eval_error(&self.query, error)))?
        };
        if matches!(value, Value::Null) {
            // `x = NULL` is never TRUE.
            return Ok(());
        }
        let storage: &S = &*context.storage;
        let deadline = &mut context.deadline;
        let mut emit = |v: S::VertexId| {
            output.push(Traverser::with_parts(
                TraverserValue::Vertex(v),
                source.path,
                source.bulk,
                source.sack,
            ));
        };
        let candidates = match (&self.key, &value) {
            (GqlLookupKey::Id, Value::Scalar(ElementProperty::String(id))) => {
                // The vertex with that id is the only one whose `element_id` equals it.
                if let Some(v) = storage.get_vertex_by_id(id) {
                    emit(v);
                }
                return Ok(());
            }
            (GqlLookupKey::Id, _) => None,
            (GqlLookupKey::Property(key), _) => self.by_index(storage, key, &value),
        };
        match candidates {
            Some(candidates) => {
                for v in candidates {
                    tick_deadline(deadline)?;
                    if self.holds(storage, v, &value) {
                        emit(v);
                    }
                }
                Ok(())
            }
            None => self.scan(storage, |v| {
                tick_deadline(deadline)?;
                if self.holds(storage, v, &value) {
                    emit(v);
                }
                Ok(())
            }),
        }
    }

    fn path_requirement(&self) -> PathRequirement {
        PathRequirement::labels(self.path_labels.iter().cloned())
    }

    fn history_labels(&self) -> &[CompactString] {
        &self.history
    }

    fn clone_box(&self) -> Box<dyn GraphStepFlatMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}
