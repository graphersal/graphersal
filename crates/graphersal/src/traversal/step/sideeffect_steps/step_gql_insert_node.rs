//! `gql:insert_node(labels, properties)`: one node of a GQL `INSERT` / `CREATE` pattern (feature
//! `gql`, internal).

use std::sync::Arc;

use crate::gql::eval::{Value, status};
use crate::gql::runtime::{
    Binding, GqlElement, RowExpr, RowValue, SubqueryAnswers, SubqueryHost, eval_error,
    history_read, labels_read, source_text,
};
use crate::graph::GraphStorage;
use crate::traversal::optimizer::path_analysis::PathRequirement;
use crate::*;

/// One property of an element a GQL `INSERT` creates: the key and its value expression.
pub(crate) struct GqlInsertProperty<S: GraphStorage> {
    /// The property key as written.
    pub(crate) key: CompactString,
    /// The value, evaluated per row.
    pub(crate) value: RowExpr<S>,
}

/// The properties of an inserted element and what they read, shared by `gql:insert_node` and
/// `gql:insert_edge`.
pub(crate) struct InsertProperties<'g, S: GraphStorage + 'g> {
    pub(crate) query: Arc<str>,
    entries: Arc<[GqlInsertProperty<S>]>,
    pub(crate) bindings: Arc<[Binding]>,
    /// The step labels the values read (sorted), for `path_requirement()` (rule 13).
    pub(crate) labels: Vec<CompactString>,
    /// The step labels the values read in every occurrence, for `history_labels()`.
    pub(crate) history: Arc<[CompactString]>,
    /// The subqueries the values hold.
    pub(crate) host: SubqueryHost<'g, S>,
}

impl<'g, S: GraphStorage + 'g> Clone for InsertProperties<'g, S> {
    fn clone(&self) -> Self {
        Self {
            query: Arc::clone(&self.query),
            entries: Arc::clone(&self.entries),
            bindings: Arc::clone(&self.bindings),
            labels: self.labels.clone(),
            history: Arc::clone(&self.history),
            host: self.host.clone(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> InsertProperties<'g, S> {
    pub(crate) fn new(
        query: Arc<str>,
        entries: Vec<GqlInsertProperty<S>>,
        bindings: Arc<[Binding]>,
        children: Vec<AnonymousTraversal<'g, S>>,
    ) -> Self {
        let labels = labels_read(entries.iter().map(|entry| &entry.value), &bindings);
        let history = history_read(entries.iter().map(|entry| &entry.value), &bindings).into();
        Self {
            query,
            entries: entries.into(),
            bindings,
            labels,
            history,
            host: SubqueryHost::new(children),
        }
    }

    /// The GQL text of the property map (`{name: n.name, age: 3}`), empty when there are no
    /// properties: the profile shows it in a string literal, so respelling leaves it alone.
    pub(crate) fn text(&self) -> String {
        if self.entries.is_empty() {
            return String::new();
        }
        let mut text = String::from("{");
        for (i, entry) in self.entries.iter().enumerate() {
            if i > 0 {
                text.push_str(", ");
            }
            text.push_str(&entry.key);
            text.push_str(": ");
            text.push_str(source_text(&self.query, &entry.value));
        }
        text.push('}');
        text
    }

    /// The property map of one row of `source`: every value evaluated in written order, a
    /// `NULL` value skipped (W20: `INSERT ({k: NULL})` stores no `k`). Any value Graphersal
    /// stores is accepted, maps and mixed lists included (W21, a deliberate deviation from
    /// Neo4j); a node, an edge or a path is no property value (22G03).
    pub(crate) fn evaluate(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<PropertyMap> {
        let mut map = PropertyMap::with_capacity(self.entries.len());
        let mut answers = SubqueryAnswers::default();
        for entry in self.entries.iter() {
            let value = self
                .host
                .evaluate(context, source, &self.bindings, &mut answers, |row| {
                    entry.value.eval(row)
                })?
                .map_err(|error| Box::new(eval_error(&self.query, error)))?;
            if matches!(value, Value::Null) {
                continue;
            }
            match stored_value(value) {
                Ok(property) => {
                    map.insert(entry.key.clone(), property);
                }
                Err(kind) => return Err(Box::new(self.not_a_value(entry, kind))),
            }
        }
        Ok(map)
    }

    #[inline(never)]
    fn not_a_value(&self, entry: &GqlInsertProperty<S>, kind: &str) -> TraverserError {
        let span = entry.value.span();
        GqlErrorBuilder::new(
            GqlErrorKind::Data,
            span.start..span.end,
            format!(
                "the value of property `{}` is {kind}, and a property cannot hold a graph element",
                entry.key
            ),
        )
        .gqlstatus(status::INVALID_VALUE_TYPE)
        .alternative(format!(
            "store a value of it instead, such as {{{}: element_id(..)}} or {{{}: properties(..)}}",
            entry.key, entry.key
        ))
        .into_error(&self.query)
    }
}

/// `value` as a stored property value; `Err` names the kind of graph element it holds (at any
/// depth): elements are handles of this graph, not values.
fn stored_value<S: GraphStorage>(value: RowValue<S>) -> Result<ElementProperty, &'static str> {
    match value {
        Value::Null => Ok(ElementProperty::Null),
        Value::Scalar(property) => Ok(property),
        Value::List(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(stored_value(item)?);
            }
            Ok(ElementProperty::Array(out))
        }
        Value::Map(entries) => {
            let mut map = PropertyMap::with_capacity(entries.len());
            for (key, value) in entries {
                map.insert(key, stored_value(value)?);
            }
            Ok(ElementProperty::Object(Box::new(map)))
        }
        Value::Element(GqlElement::Vertex(_)) => Err("a node"),
        Value::Element(GqlElement::Edge(_)) => Err("an edge"),
        Value::Path(_) => Err("a path"),
    }
}

/// The traversers one incoming traverser stands for: `source` expanded to bulk-1 copies (W15:
/// a write step treats a traverser of bulk n as n rows), or one seed holding `null` when the
/// step starts the traversal. `generated` tells the caller to emit start traversers.
///
/// The bulk is an untrusted count (a cross product merged by Gremlin bulking can carry any
/// number): above 1 it is checked like a bulk expansion at `step` (`bulk.max_expansion`,
/// `traversal.max_materialized_bytes`, the memory budget) before a single copy is made.
pub(crate) fn insert_rows<S: GraphStorage>(
    context: &TraverserContext<'_, S>,
    source: Option<Traverser<S>>,
    step: &str,
) -> TraverserResult<(Vec<Traverser<S>>, bool)> {
    match source {
        Some(source) => {
            if source.bulk > 1 {
                crate::traversal::bulk::ExpansionLimits::of(&context.options).check_copies(
                    &source.value,
                    source.bulk,
                    step,
                )?;
            }
            Ok((expand_rows(source), false))
        }
        None => Ok((
            vec![Traverser::start(
                context,
                TraverserValue::Computed(ElementProperty::Null, None),
            )],
            true,
        )),
    }
}

/// `source` as bulk-1 copies, one per unit of its bulk (derived: its path and sack). The caller
/// bounds the bulk ([`insert_rows`]); the up-front capacity is capped either way.
pub(crate) fn expand_rows<S: GraphStorage>(mut source: Traverser<S>) -> Vec<Traverser<S>> {
    let copies = source.bulk.max(1);
    source.bulk = 1;
    let mut rows = Vec::with_capacity(crate::traversal::capacity::capped_u64(copies));
    for _ in 1..copies {
        rows.push(source.clone());
    }
    rows.push(source);
    rows
}

/// Creates one vertex per row: `INSERT (n:Person&Admin {name: $name})`. The property map is
/// evaluated on the row (NULL values skipped, W20; maps and mixed lists accepted, W21), then
/// the vertex is created with ALL its labels and properties in one `add_vertex_with` call, so
/// the schema's create-time defaults and its enforcement (`required`, closed mode, write-time
/// coercion) see the full element (W38). The new vertex is the current object (a new path
/// position); the lowering labels it with `as(var)` when a later step reads it.
///
/// A traverser of bulk n is n rows: n vertices, each evaluated on its own, each emitted with
/// bulk 1 (W15; derived traversers, rule 20: the parent's path and sack). Started with no
/// input (the first step of a traversal) it creates one vertex from a `null` seed and emits a
/// generated traverser. `defaultFunction` `now` / `today` use the pinned time
/// ([`GraphStepSideEffectMut::set_create_time`]) or the execution's.
///
/// Asks `Create` on `Data { Vertex, label }` per label, `Data { Vertex, None }` when label-less,
/// exactly like `add_v(labels)`. Built only by the GQL lowering; not a DSL step.
struct StepGqlInsertNode<'g, S: GraphStorage + 'g> {
    labels: Arc<[CompactString]>,
    properties: InsertProperties<'g, S>,
    create_time: Option<crate::value::temporal::DateTime>,
}

impl<'g, S: GraphStorage + 'g> Clone for StepGqlInsertNode<'g, S> {
    fn clone(&self) -> Self {
        Self {
            labels: Arc::clone(&self.labels),
            properties: self.properties.clone(),
            create_time: self.create_time,
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepGqlInsertNode<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let labels: Vec<&str> = self.labels.iter().map(CompactString::as_str).collect();
        write!(
            f,
            "gql:insert_node({}",
            crate::value::literal::rhai_string_array(&labels)
        )?;
        let text = self.properties.text();
        if !text.is_empty() {
            write!(f, ", {}", crate::value::literal::rhai_string(&text))?;
        }
        write!(f, ")")
    }
}

fn insert_node_step<'g, S: GraphStorage + 'g>(
    query: Arc<str>,
    labels: Vec<CompactString>,
    properties: Vec<GqlInsertProperty<S>>,
    bindings: Arc<[Binding]>,
    children: Vec<AnonymousTraversal<'g, S>>,
) -> GraphStep<'g, S> {
    GraphStep::SideEffectMut(
        "gql:insert_node",
        Box::new(StepGqlInsertNode {
            labels: labels.into(),
            properties: InsertProperties::new(query, properties, bindings, children),
            create_time: None,
        }),
    )
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Appends `gql:insert_node(labels, properties)` hosting the subqueries `children` (child
    /// `i` answers the values' subquery `i`); see [`StepGqlInsertNode`].
    pub(crate) fn gql_insert_node(
        self,
        query: Arc<str>,
        labels: Vec<CompactString>,
        properties: Vec<GqlInsertProperty<S>>,
        bindings: Arc<[Binding]>,
        children: Vec<AnonymousTraversal<'g, S>>,
    ) -> Self {
        self.push_step(insert_node_step(
            query, labels, properties, bindings, children,
        ))
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Appends `gql:insert_node(labels, properties)` hosting the subqueries `children`; see
    /// [`StepGqlInsertNode`].
    pub(crate) fn gql_insert_node(
        self,
        query: Arc<str>,
        labels: Vec<CompactString>,
        properties: Vec<GqlInsertProperty<S>>,
        bindings: Arc<[Binding]>,
        children: Vec<AnonymousTraversal<'g, S>>,
    ) -> Self {
        self.push_step(insert_node_step(
            query, labels, properties, bindings, children,
        ))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepSideEffectMut<'g, S> for StepGqlInsertNode<'g, S> {
    fn set_create_time(&mut self, now: crate::value::temporal::DateTime) {
        self.create_time = Some(now);
    }

    fn static_labels(&self) -> Vec<&str> {
        self.labels.iter().map(CompactString::as_str).collect()
    }

    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Option<Traverser<S>>,
    ) -> TraverserResult<Vec<Traverser<S>>> {
        let (rows, generated) = insert_rows(context, source, "gql:insert_node")?;
        let mut output = crate::traversal::capacity::vec_capped(rows.len());
        for row in rows {
            context.tick()?;
            let properties = self.properties.evaluate(context, &row)?;
            let create = context.create_context_or(self.create_time);
            let vertex =
                context
                    .storage
                    .add_vertex_with(None, self.labels.to_vec(), properties, &create)?;
            output.push(if generated {
                Traverser::start(context, TraverserValue::Vertex(vertex))
            } else {
                row.split(TraverserValue::Vertex(vertex))
            });
        }
        Ok(output)
    }

    fn path_requirement(&self) -> PathRequirement {
        PathRequirement::labels(self.properties.labels.iter().cloned())
    }

    fn history_labels(&self) -> &[CompactString] {
        &self.properties.history
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut AnonymousTraversal<'g, S>> {
        self.properties.host.children_mut()
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &crate::traversal::optimizer::path_analysis::PathAnalysis,
        _downstream: &PathRequirement,
    ) -> PathRequirement {
        self.properties.host.analyze_paths(analysis)
    }

    fn clone_box(&self) -> Box<dyn GraphStepSideEffectMut<'g, S> + 'g> {
        Box::new(self.clone())
    }
}
