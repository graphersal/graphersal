//! `gql:insert_edge(label, from, to, properties)`: one edge of a GQL `INSERT` / `CREATE`
//! pattern (feature `gql`, internal).

use std::sync::Arc;

use super::step_gql_insert_node::{GqlInsertProperty, InsertProperties, insert_rows};
use crate::gql::eval::status;
use crate::gql::lexer::Span;
use crate::gql::runtime::{Binding, Row, labels_read_by};
use crate::graph::GraphStorage;
use crate::traversal::optimizer::path_analysis::PathRequirement;
use crate::*;

/// One endpoint of an edge a GQL `INSERT` creates: the node written at that end of the edge.
#[derive(Debug, Clone)]
pub(crate) struct GqlEndpoint {
    /// The node's variable as written, or the name the lowering gave an anonymous node: shown
    /// in the profile and in the errors.
    pub(crate) name: CompactString,
    /// Where the node comes from: a node inserted just before in the same pattern (the
    /// current object or its label), a node bound earlier (a path label, a row column).
    pub(crate) binding: Binding,
    /// The node pattern in the query, underlined by the errors.
    pub(crate) span: Span,
}

/// Creates one edge per row: `INSERT (a)-[:KNOWS {since: 2020}]->(b)`, from the node `from`
/// holds to the node `to` holds (the lowering swaps them for `<-[..]-`). The property map is
/// evaluated on the row like `gql:insert_node`'s (NULL skipped, W20; maps and mixed lists
/// accepted, W21), then the edge is created in one `add_edge_with` call (schema defaults and
/// enforcement, W38). The new edge is the current object (a new path position).
///
/// An endpoint that holds `NULL` (an `OPTIONAL MATCH` that did not match) is a data error
/// (22004, W5), one that holds anything but a node 22G03, a node removed earlier in the query
/// the storage's `ElementRemoved` error, exactly as a Gremlin `add_e` to a removed vertex.
///
/// A traverser of bulk n is n rows: n edges, each emitted with bulk 1 (W15; derived
/// traversers, rule 20). Asks `Create` on `Data { Edge, label }`, exactly like `add_e(label)`
/// (the endpoints were reached, and their reads requested, by the steps that bound them).
/// Built only by the GQL lowering; not a DSL step.
struct StepGqlInsertEdge<'g, S: GraphStorage + 'g> {
    label: CompactString,
    from: GqlEndpoint,
    to: GqlEndpoint,
    /// The bindings of `from` (slot 0) and `to` (slot 1).
    endpoints: Arc<[Binding]>,
    properties: InsertProperties<'g, S>,
    /// Every step label the endpoints and the values read (sorted), for `path_requirement()`.
    labels: Arc<[CompactString]>,
    create_time: Option<crate::value::temporal::DateTime>,
}

impl<'g, S: GraphStorage + 'g> Clone for StepGqlInsertEdge<'g, S> {
    fn clone(&self) -> Self {
        Self {
            label: self.label.clone(),
            from: self.from.clone(),
            to: self.to.clone(),
            endpoints: Arc::clone(&self.endpoints),
            properties: self.properties.clone(),
            labels: Arc::clone(&self.labels),
            create_time: self.create_time,
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepGqlInsertEdge<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use crate::value::literal::rhai_string;
        write!(
            f,
            "gql:insert_edge({}, {}, {}",
            rhai_string(&self.label),
            rhai_string(&self.from.name),
            rhai_string(&self.to.name)
        )?;
        let text = self.properties.text();
        if !text.is_empty() {
            write!(f, ", {}", rhai_string(&text))?;
        }
        write!(f, ")")
    }
}

impl<'g, S: GraphStorage + 'g> StepGqlInsertEdge<'g, S> {
    /// The vertex endpoint `slot` (0 `from`, 1 `to`) holds on `row` (see the type docs).
    fn endpoint(
        &self,
        context: &TraverserContext<'_, S>,
        row: &Traverser<S>,
        slot: usize,
    ) -> TraverserResult<S::VertexId> {
        let value = Row::new(context, row, &self.endpoints)
            .bound(slot)
            .map(std::borrow::Cow::into_owned);
        match value {
            Some(TraverserValue::Vertex(vertex)) => {
                if context.storage.contains_vertex(vertex) {
                    Ok(vertex)
                } else {
                    Err(Box::new(
                        GraphError::element_removed(GraphNodeType::Vertex).into(),
                    ))
                }
            }
            None | Some(TraverserValue::Computed(ElementProperty::Null, _)) => Err(Box::new(
                self.endpoint_error(slot, "is NULL", status::NULL_VALUE),
            )),
            Some(_) => Err(Box::new(self.endpoint_error(
                slot,
                "is not a node",
                status::INVALID_VALUE_TYPE,
            ))),
        }
    }

    #[inline(never)]
    fn endpoint_error(&self, slot: usize, what: &str, code: &'static str) -> TraverserError {
        let (end, endpoint) = if slot == 0 {
            ("source", &self.from)
        } else {
            ("destination", &self.to)
        };
        let builder = GqlErrorBuilder::new(
            GqlErrorKind::Data,
            endpoint.span.start..endpoint.span.end,
            format!(
                "the {end} node `{}` of the inserted `:{}` edge {what}",
                endpoint.name, self.label
            ),
        )
        .gqlstatus(code);
        let builder = if code == status::NULL_VALUE {
            builder.alternative(format!(
                "keep only the rows where it is bound before the INSERT: FILTER {} IS NOT NULL",
                endpoint.name
            ))
        } else {
            builder
        };
        builder.into_error(&self.properties.query)
    }
}

#[allow(clippy::too_many_arguments)]
fn insert_edge_step<'g, S: GraphStorage + 'g>(
    query: Arc<str>,
    label: CompactString,
    from: GqlEndpoint,
    to: GqlEndpoint,
    properties: Vec<GqlInsertProperty<S>>,
    bindings: Arc<[Binding]>,
    children: Vec<AnonymousTraversal<'g, S>>,
) -> GraphStep<'g, S> {
    let endpoints: Arc<[Binding]> = Arc::from([from.binding.clone(), to.binding.clone()]);
    let properties = InsertProperties::new(query, properties, bindings, children);
    let mut labels = labels_read_by(&endpoints);
    labels.extend(properties.labels.iter().cloned());
    labels.sort_unstable();
    labels.dedup();
    GraphStep::SideEffectMut(
        "gql:insert_edge",
        Box::new(StepGqlInsertEdge {
            label,
            from,
            to,
            endpoints,
            properties,
            labels: labels.into(),
            create_time: None,
        }),
    )
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Appends `gql:insert_edge(label, from, to, properties)` hosting the subqueries
    /// `children` (child `i` answers the values' subquery `i`); see [`StepGqlInsertEdge`].
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn gql_insert_edge(
        self,
        query: Arc<str>,
        label: CompactString,
        from: GqlEndpoint,
        to: GqlEndpoint,
        properties: Vec<GqlInsertProperty<S>>,
        bindings: Arc<[Binding]>,
        children: Vec<AnonymousTraversal<'g, S>>,
    ) -> Self {
        self.push_step(insert_edge_step(
            query, label, from, to, properties, bindings, children,
        ))
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Appends `gql:insert_edge(label, from, to, properties)` hosting the subqueries
    /// `children`; see [`StepGqlInsertEdge`].
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn gql_insert_edge(
        self,
        query: Arc<str>,
        label: CompactString,
        from: GqlEndpoint,
        to: GqlEndpoint,
        properties: Vec<GqlInsertProperty<S>>,
        bindings: Arc<[Binding]>,
        children: Vec<AnonymousTraversal<'g, S>>,
    ) -> Self {
        self.push_step(insert_edge_step(
            query, label, from, to, properties, bindings, children,
        ))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepSideEffectMut<'g, S> for StepGqlInsertEdge<'g, S> {
    fn set_create_time(&mut self, now: crate::value::temporal::DateTime) {
        self.create_time = Some(now);
    }

    fn static_labels(&self) -> Vec<&str> {
        vec![self.label.as_str()]
    }

    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Option<Traverser<S>>,
    ) -> TraverserResult<Vec<Traverser<S>>> {
        let (rows, generated) = insert_rows(context, source);
        let mut output = Vec::with_capacity(rows.len());
        for row in rows {
            let from = self.endpoint(context, &row, 0)?;
            let to = self.endpoint(context, &row, 1)?;
            let properties = self.properties.evaluate(context, &row)?;
            let create = context.create_context_or(self.create_time);
            let edge = context.storage.add_edge_with(
                None,
                Some(self.label.clone()),
                from,
                to,
                properties,
                &create,
            )?;
            output.push(if generated {
                Traverser::start(context, TraverserValue::Edge(edge))
            } else {
                row.split(TraverserValue::Edge(edge))
            });
        }
        Ok(output)
    }

    fn path_requirement(&self) -> PathRequirement {
        PathRequirement::labels(self.labels.iter().cloned())
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
