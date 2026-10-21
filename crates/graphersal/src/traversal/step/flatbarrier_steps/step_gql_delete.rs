//! `gql:delete(targets)`: one GQL `DELETE` / `DETACH DELETE` / `NODETACH DELETE` clause
//! (feature `gql`, internal).

use std::collections::HashSet;
use std::sync::Arc;

use fnv::FnvBuildHasher;

use crate::gql::eval::{Value, status};
use crate::gql::lexer::Span;
use crate::gql::runtime::{
    Binding, GqlElement, RowExpr, RowValue, SubqueryAnswers, SubqueryHost, eval_error,
    history_read, labels_read, source_text,
};
use crate::graph::GraphStorage;
use crate::traversal::capacity::vec_capped;
use crate::traversal::optimizer::path_analysis::PathRequirement;
use crate::traversal::step::sideeffect_steps::step_gql_set::rewritten;
use crate::*;

/// One target of a GQL `DELETE` clause: an expression giving a node, an edge, a path, a list
/// of them (nested lists too) or `NULL`, with what the plan states about it statically (for the
/// access requests, design C3 of stage 3d).
pub(crate) struct GqlDeleteTarget<S: GraphStorage> {
    /// The target expression (a variable, a map member, a function call, ..); its span is the
    /// text the profile shows and the errors underline.
    pub(crate) value: RowExpr<S>,
    /// The element kind the binder knows: `Some(Vertex)` for a node variable, `Some(Edge)`
    /// for an edge variable, `None` for a path, a list, a map member or any other expression.
    pub(crate) element: Option<ElementKind>,
    /// For an edge target: the labels its pattern states (plain labels or a disjunction of
    /// names; empty when it states none). Ignored for vertices: a vertex has a label SET, so a
    /// request names no label of it (as `drop()` after `has_label()`).
    pub(crate) labels: Vec<CompactString>,
}

impl<S: GraphStorage> Clone for GqlDeleteTarget<S> {
    fn clone(&self) -> Self {
        Self {
            value: self.value.clone(),
            element: self.element,
            labels: self.labels.clone(),
        }
    }
}

/// Removes the targets of one `DELETE` clause (W29). A flat barrier: it first collects the
/// targets of EVERY row of the clause (a row of bulk n once: a delete is idempotent), then
/// checks, then removes, then emits every row unchanged (value, path, bulk and sack kept, so a
/// later `count(*)` counts each row). Targets: a node or an edge; a path gives each of its
/// elements; a list gives each item, nested lists too; `NULL` (an `OPTIONAL MATCH` that did not
/// match, also inside a list) is ignored (W25); an element already removed (earlier in the
/// query, or by an earlier row of this clause) is ignored; anything else is 22G03 naming the
/// target. Every element counts once, whatever the number of rows or targets giving it.
///
/// `NODETACH` (plain `DELETE`, the default): before anything is removed, every node to delete
/// must have no edge besides the edges this same clause deletes (per clause, W29: `MATCH
/// (n)-[r]-() DELETE n, r` works), else the clause fails with GQLSTATUS `G1001` (dependent
/// object error: edges still exist) and, as every failure of a unit, changes nothing (a
/// deliberate deviation from Neo4j, which checks at commit time; book `gql/semantics.md`,
/// "Differences from Neo4j"). `DETACH`
/// removes the nodes with all their edges. Edges go first, then nodes, through the storage's
/// `drop_edge` / `drop_vertex` (both idempotent, the write path of `drop()`).
///
/// Every removed edge's label goes into the execution's side table
/// (`TraverserContext::gql_deleted`), so `type(r)` of it still answers later in the query (E3
/// of stage 3d); any other read of a removed element is the W27 error (`runtime::Row`), a
/// returned one the W28 error (`runtime::check_returned`).
///
/// Asks `Delete` on the data of each target as the plan states it, like `drop()` on the same
/// elements (design C3): a node `Data { Vertex, None }`, an edge `Data { Edge, label }` per
/// label its pattern states (`Data { Edge, None }` without), anything else `Data { None, None }`
/// (`GraphStep::for_each_access_in`, through [`GraphStepFlatBarrier::gql_deletes`]). It reads
/// the step labels its targets read (rule 13). Built only by the GQL lowering; not a DSL step.
struct StepGqlDelete<'g, S: GraphStorage + 'g> {
    query: Arc<str>,
    targets: Arc<[GqlDeleteTarget<S>]>,
    /// `DETACH DELETE`: the nodes go with their edges.
    detach: bool,
    /// The clause keyword(s) as written (`DELETE`, `DETACH DELETE`, `NODETACH DELETE`), for
    /// the profile and the `DETACH DELETE` fix of the `G1001` error.
    keyword: Span,
    /// The variable slots of the targets (`targets` index `bindings`).
    bindings: Arc<[Binding]>,
    /// Every step label the targets read (sorted), for `path_requirement()`.
    labels: Arc<[CompactString]>,
    /// The step labels read in every occurrence, for `history_labels()`.
    history: Arc<[CompactString]>,
    host: SubqueryHost<'g, S>,
}

impl<'g, S: GraphStorage + 'g> Clone for StepGqlDelete<'g, S> {
    fn clone(&self) -> Self {
        Self {
            query: Arc::clone(&self.query),
            targets: Arc::clone(&self.targets),
            detach: self.detach,
            keyword: self.keyword,
            bindings: Arc::clone(&self.bindings),
            labels: Arc::clone(&self.labels),
            history: Arc::clone(&self.history),
            host: self.host.clone(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepGqlDelete<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // GQL text in a string literal, so the camelCase respelling leaves it alone.
        let keyword = self
            .query
            .get(self.keyword.start..self.keyword.end)
            .unwrap_or(if self.detach {
                "DETACH DELETE"
            } else {
                "DELETE"
            });
        let mut text = String::from(keyword);
        for (i, target) in self.targets.iter().enumerate() {
            text.push_str(if i == 0 { " " } else { ", " });
            text.push_str(source_text(&self.query, &target.value));
        }
        write!(
            f,
            "gql:delete({})",
            crate::value::literal::rhai_string(&text)
        )
    }
}

/// The elements one clause removes, each once, in the order the rows give them.
struct Collected<S: GraphStorage> {
    seen: HashSet<GqlElement<S>, FnvBuildHasher>,
    /// The vertices with the target that gave each first (for the `G1001` error).
    vertices: Vec<(S::VertexId, usize)>,
    edges: Vec<S::EdgeId>,
}

impl<'g, S: GraphStorage + 'g> StepGqlDelete<'g, S> {
    /// Adds the elements `value` (of target `target`) gives to `out`: an element, the elements
    /// of a path, the items of a list (nested lists through an explicit stack, so a deep value
    /// cannot overflow the call stack); `NULL` and removed elements are skipped.
    fn collect(
        &self,
        storage: &S,
        target: usize,
        value: RowValue<S>,
        out: &mut Collected<S>,
    ) -> TraverserResult<()> {
        let add = |element: GqlElement<S>, out: &mut Collected<S>| match element {
            GqlElement::Vertex(v) => {
                if storage.contains_vertex(v) && out.seen.insert(element) {
                    out.vertices.push((v, target));
                }
            }
            GqlElement::Edge(e) => {
                if storage.contains_edge(e) && out.seen.insert(element) {
                    out.edges.push(e);
                }
            }
        };
        let mut stack = vec![value];
        while let Some(value) = stack.pop() {
            match value {
                Value::Null => {}
                Value::Element(element) => add(element, out),
                Value::Path(elements) => {
                    for element in elements {
                        add(element, out);
                    }
                }
                // Reversed onto the stack, so the items come off it in list order.
                Value::List(items) => stack.extend(items.into_iter().rev()),
                Value::Scalar(_) | Value::Map(_) => {
                    return Err(Box::new(self.not_an_element(target, value.kind_name())));
                }
            }
        }
        Ok(())
    }

    #[inline(never)]
    fn not_an_element(&self, target: usize, kind: &str) -> TraverserError {
        let span = self
            .targets
            .get(target)
            .map(|t| t.value.span())
            .unwrap_or(self.keyword);
        let text = self.query.get(span.start..span.end).unwrap_or("?");
        GqlErrorBuilder::new(
            GqlErrorKind::Data,
            span.start..span.end,
            format!("DELETE removes nodes, edges and paths, but `{text}` gives {kind} on this row"),
        )
        .gqlstatus(status::INVALID_VALUE_TYPE)
        .alternative(
            "give DELETE a node, an edge, a path or a list of them, and keep the rows where \
             the target holds something else away from it with FILTER or WHERE, as in \
             g.gql(\"MATCH (n:person {name: 'marko'}) DETACH DELETE n\")",
        )
        .topic(GqlHelpTopic::Delete)
        .into_error(&self.query)
    }

    /// The `G1001` error: the node target `target` gave still has an edge the clause does not
    /// delete.
    #[inline(never)]
    fn edges_still_exist(&self, target: usize) -> TraverserError {
        let span = self
            .targets
            .get(target)
            .map(|t| t.value.span())
            .unwrap_or(self.keyword);
        let text = self.query.get(span.start..span.end).unwrap_or("n");
        let fixed = rewritten(&self.query, self.keyword, "DETACH DELETE");
        GqlErrorBuilder::new(
            GqlErrorKind::Data,
            span.start..span.end,
            format!(
                "`{text}` is a node that still has edges, and DELETE removes only nodes without \
                 edges (besides the edges deleted in the same clause)"
            ),
        )
        .gqlstatus(status::EDGES_STILL_EXIST)
        .alternative(format!(
            "remove the node together with its edges with DETACH DELETE, as in {}, or delete \
             its edges in the same clause, as in g.gql(\"MATCH (s:software {{name: 'ripple'}})\
             <-[r]-() DELETE r, s\")",
            fixed.unwrap_or_else(|| format!("DETACH DELETE {text}"))
        ))
        .topic(GqlHelpTopic::Delete)
        .into_error(&self.query)
    }

    /// The first node of `collected` (and the target that gave it) with an edge the clause
    /// does not delete, and the edges looked at.
    fn first_attached(&self, storage: &S, collected: &Collected<S>) -> (Option<usize>, usize) {
        let mut looked = 0usize;
        for (vertex, target) in &collected.vertices {
            for edge in storage.out_edges(*vertex).chain(storage.in_edges(*vertex)) {
                looked = looked.saturating_add(1);
                if !collected.seen.contains(&GqlElement::Edge(edge)) {
                    return (Some(*target), looked);
                }
            }
        }
        (None, looked)
    }
}

/// Builds the `gql:delete` step; see [`StepGqlDelete`].
fn delete_step<'g, S: GraphStorage + 'g>(
    query: Arc<str>,
    keyword: Span,
    detach: bool,
    targets: Vec<GqlDeleteTarget<S>>,
    bindings: Arc<[Binding]>,
    children: Vec<AnonymousTraversal<'g, S>>,
) -> GraphStep<'g, S> {
    let labels = labels_read(targets.iter().map(|t| &t.value), &bindings).into();
    let history = history_read(targets.iter().map(|t| &t.value), &bindings).into();
    GraphStep::FlatBarrier(
        "gql:delete",
        Box::new(StepGqlDelete {
            query,
            targets: targets.into(),
            detach,
            keyword,
            bindings,
            labels,
            history,
            host: SubqueryHost::new(children),
        }),
    )
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Appends `gql:delete(targets)`: one `DELETE` clause (`keyword` its keyword(s) as written,
    /// `detach` for `DETACH DELETE`), the targets reading the variable slots `bindings` and
    /// hosting the subqueries `children` (child `i` answers the targets' subquery `i`); see
    /// [`StepGqlDelete`].
    pub(crate) fn gql_delete(
        self,
        query: Arc<str>,
        keyword: Span,
        detach: bool,
        targets: Vec<GqlDeleteTarget<S>>,
        bindings: Arc<[Binding]>,
        children: Vec<AnonymousTraversal<'g, S>>,
    ) -> Self {
        self.push_step(delete_step(
            query, keyword, detach, targets, bindings, children,
        ))
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Appends `gql:delete(targets)` (a `DELETE` in a `CALL { }` body); see
    /// [`GraphTraversalSource::gql_delete`].
    pub(crate) fn gql_delete(
        self,
        query: Arc<str>,
        keyword: Span,
        detach: bool,
        targets: Vec<GqlDeleteTarget<S>>,
        bindings: Arc<[Binding]>,
        children: Vec<AnonymousTraversal<'g, S>>,
    ) -> Self {
        self.push_step(delete_step(
            query, keyword, detach, targets, bindings, children,
        ))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatBarrier<'g, S> for StepGqlDelete<'g, S> {
    /// It holds every element of the clause (each once) before removing them.
    fn materializes(&self) -> bool {
        true
    }

    fn gql_deletes(&self) -> Vec<(Option<ElementKind>, Option<&str>)> {
        fn push<'a>(
            out: &mut Vec<(Option<ElementKind>, Option<&'a str>)>,
            request: (Option<ElementKind>, Option<&'a str>),
        ) {
            if !out.contains(&request) {
                out.push(request);
            }
        }
        let mut out = Vec::with_capacity(self.targets.len());
        for target in self.targets.iter() {
            match target.element {
                Some(ElementKind::Edge) if !target.labels.is_empty() => {
                    for label in &target.labels {
                        push(&mut out, (Some(ElementKind::Edge), Some(label.as_str())));
                    }
                }
                element => push(&mut out, (element, None)),
            }
        }
        out
    }

    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Vec<Traverser<S>>,
    ) -> TraverserResult<Option<Vec<Traverser<S>>>> {
        let mut collected = Collected {
            seen: HashSet::default(),
            vertices: Vec::new(),
            edges: vec_capped(source.len()),
        };
        for row in &source {
            context.tick()?;
            let mut answers = SubqueryAnswers::default();
            for (index, target) in self.targets.iter().enumerate() {
                let value = self
                    .host
                    .evaluate(context, row, &self.bindings, &mut answers, |r| {
                        target.value.eval(r)
                    })?
                    .map_err(|error| Box::new(eval_error(&self.query, error)))?;
                self.collect(&*context.storage, index, value, &mut collected)?;
            }
        }
        if !self.detach {
            let (attached, looked) = self.first_attached(&*context.storage, &collected);
            context.tick_n(looked)?;
            if let Some(target) = attached {
                return Err(Box::new(self.edges_still_exist(target)));
            }
        }
        // Edges first (their labels kept for `type(r)`), then the vertices; a DETACH takes the
        // remaining edges of each vertex with it, whose labels are kept too.
        let mut removed_any = false;
        for edge in &collected.edges {
            context.tick()?;
            if let Some(label) = context.storage.edge_label(*edge).map(CompactString::from) {
                context.gql_deleted.edge_types.insert(*edge, label);
            }
            context.storage.drop_edge(*edge)?;
            removed_any = true;
        }
        let mut incident: Vec<(S::EdgeId, CompactString)> = Vec::new();
        for (vertex, _) in &collected.vertices {
            context.tick()?;
            if self.detach {
                let storage = &*context.storage;
                incident.clear();
                incident.extend(
                    storage
                        .out_edges(*vertex)
                        .chain(storage.in_edges(*vertex))
                        .filter_map(|e| Some((e, CompactString::from(storage.edge_label(e)?)))),
                );
                context.tick_n(incident.len())?;
                context.gql_deleted.edge_types.extend(incident.drain(..));
            }
            context.storage.drop_vertex(*vertex)?;
            removed_any = true;
        }
        if removed_any {
            context.gql_deleted.any = true;
        }
        Ok(Some(source))
    }

    fn path_requirement(&self) -> PathRequirement {
        PathRequirement::labels(self.labels.iter().cloned())
    }

    fn history_labels(&self) -> &[CompactString] {
        &self.history
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut AnonymousTraversal<'g, S>> {
        self.host.children_mut()
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &crate::traversal::optimizer::path_analysis::PathAnalysis,
        _downstream: &PathRequirement,
    ) -> PathRequirement {
        self.host.analyze_paths(analysis)
    }

    fn clone_box(&self) -> Box<dyn GraphStepFlatBarrier<'g, S> + 'g> {
        Box::new(self.clone())
    }
}
