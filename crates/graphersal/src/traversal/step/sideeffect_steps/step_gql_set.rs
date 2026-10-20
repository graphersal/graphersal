//! `gql:set(items)`: the items of one GQL `SET` clause (feature `gql`, internal), and what
//! `gql:set` and `gql:remove` share: the item target, its resolution on a row, the property
//! and label writes, the access requests.

use std::sync::Arc;

use super::step_gql_insert_node::{insert_rows, stored_value};
use crate::gql::eval::{Value, status};
use crate::gql::lexer::Span;
use crate::gql::runtime::{
    Binding, GqlElement, Row, RowExpr, SubqueryAnswers, SubqueryHost, eval_error, history_read,
    history_read_by, labels_read, labels_read_by,
};
use crate::graph::GraphStorage;
use crate::traversal::optimizer::path_analysis::PathRequirement;
use crate::*;

/// The span of an item that has none (a guard only: every item has its target).
const NO_SPAN: Span = Span {
    start: 0,
    end: 0,
    line: 1,
    column: 1,
};

/// The element a `SET` / `REMOVE` item changes: a bound variable, with what the plan states
/// about it statically (for the access requests, design C6 of stage 3c).
#[derive(Debug, Clone)]
pub(crate) struct GqlWriteTarget {
    /// The variable as written, shown in the errors.
    pub(crate) name: CompactString,
    /// Where the element comes from on a row (the current object, a path label, a row column).
    pub(crate) binding: Binding,
    /// The variable in the item, underlined by the errors.
    pub(crate) span: Span,
    /// The element kind the binder knows (`None`: unknown, a `FOR` item or a list column).
    pub(crate) element: Option<ElementKind>,
    /// For an edge target: the labels its pattern states (plain labels or a disjunction of
    /// names; empty when it states none). Ignored for vertices: a vertex has a label SET, so a
    /// request names no label of it (`access_flow_after`).
    pub(crate) labels: Vec<CompactString>,
}

/// One item of a GQL `SET` clause, applied left to right on each row (W22).
pub(crate) enum GqlSetItem<S: GraphStorage> {
    /// `n.k = expr`: sets `k`; a `NULL` value removes it (W20).
    Property {
        target: GqlWriteTarget,
        key: CompactString,
        value: RowExpr<S>,
        /// The whole item, for the profile text.
        span: Span,
    },
    /// `n = expr`: replaces every property by the map or the element's properties `expr`
    /// gives (W23); `NULL` entries are absent.
    Replace {
        target: GqlWriteTarget,
        value: RowExpr<S>,
        span: Span,
    },
    /// `n += expr`: merges the map or the element's properties into `n` (W24); a `NULL` entry
    /// removes its key.
    Merge {
        target: GqlWriteTarget,
        value: RowExpr<S>,
        span: Span,
    },
    /// `n:A:B` / `n IS A & B`: adds each label in order (idempotent, W26).
    Labels {
        target: GqlWriteTarget,
        labels: Vec<CompactString>,
        span: Span,
    },
}

impl<S: GraphStorage> GqlSetItem<S> {
    fn target(&self) -> &GqlWriteTarget {
        match self {
            GqlSetItem::Property { target, .. }
            | GqlSetItem::Replace { target, .. }
            | GqlSetItem::Merge { target, .. }
            | GqlSetItem::Labels { target, .. } => target,
        }
    }

    fn span(&self) -> Span {
        match self {
            GqlSetItem::Property { span, .. }
            | GqlSetItem::Replace { span, .. }
            | GqlSetItem::Merge { span, .. }
            | GqlSetItem::Labels { span, .. } => *span,
        }
    }

    fn value(&self) -> Option<&RowExpr<S>> {
        match self {
            GqlSetItem::Property { value, .. }
            | GqlSetItem::Replace { value, .. }
            | GqlSetItem::Merge { value, .. } => Some(value),
            GqlSetItem::Labels { .. } => None,
        }
    }

    fn label_names(&self) -> Option<&[CompactString]> {
        match self {
            GqlSetItem::Labels { labels, .. } => Some(labels),
            GqlSetItem::Property { .. } | GqlSetItem::Replace { .. } | GqlSetItem::Merge { .. } => {
                None
            }
        }
    }
}

/// The target of an item on one row.
pub(crate) enum Resolved<S: GraphStorage> {
    /// `NULL` (an `OPTIONAL MATCH` that did not match): the item is a no-op (W25).
    Null,
    /// A vertex of the graph.
    Vertex(S::VertexId),
    /// An edge of the graph.
    Edge(S::EdgeId),
}

/// What `gql:set` and `gql:remove` share: the query, the item targets as row bindings, the
/// labels they read.
pub(crate) struct WriteItems {
    pub(crate) query: Arc<str>,
    /// The clause: `SET` or `REMOVE`, named by the errors.
    pub(crate) clause: &'static str,
    /// Per item, its target.
    pub(crate) targets: Vec<GqlWriteTarget>,
    /// Per item, its target's binding (slot `i` = item `i`).
    pub(crate) bindings: Arc<[Binding]>,
    /// Per item, its text in the query (the profile shows them joined by `, `).
    pub(crate) texts: Vec<Span>,
    /// Per item, whether it is a label item (asks `Update` on `Data { Vertex, None }`, D3).
    pub(crate) label_items: Vec<bool>,
}

impl Clone for WriteItems {
    fn clone(&self) -> Self {
        Self {
            query: Arc::clone(&self.query),
            clause: self.clause,
            targets: self.targets.clone(),
            bindings: Arc::clone(&self.bindings),
            texts: self.texts.clone(),
            label_items: self.label_items.clone(),
        }
    }
}

impl WriteItems {
    pub(crate) fn new(
        query: Arc<str>,
        clause: &'static str,
        items: impl IntoIterator<Item = (GqlWriteTarget, Span, bool)>,
    ) -> Self {
        let mut targets = Vec::new();
        let mut texts = Vec::new();
        let mut label_items = Vec::new();
        for (target, span, label_item) in items {
            targets.push(target);
            texts.push(span);
            label_items.push(label_item);
        }
        let bindings: Arc<[Binding]> = targets.iter().map(|t| t.binding.clone()).collect();
        Self {
            query,
            clause,
            targets,
            bindings,
            texts,
            label_items,
        }
    }

    /// The GQL text of the items (`n.age = n.age + 1, n:Admin`), for the profile; shown in a
    /// string literal, so respelling leaves it alone.
    pub(crate) fn text(&self) -> String {
        let mut text = String::new();
        for (i, span) in self.texts.iter().enumerate() {
            if i > 0 {
                text.push_str(", ");
            }
            text.push_str(self.query.get(span.start..span.end).unwrap_or_default());
        }
        text
    }

    /// The `Update` requests of the items in item order, `labels_of(item)` of them for a label
    /// item (one per label name): what the Gremlin twins (`property()`, `remove_property()`,
    /// `add_label()`, `drop_label()`) ask on the same elements (design C6 of stage 3c, D3).
    pub(crate) fn updates_with(
        &self,
        labels_of: impl Fn(usize) -> usize,
    ) -> Vec<(Option<ElementKind>, Option<&str>)> {
        let mut out = Vec::with_capacity(self.targets.len());
        for (i, target) in self.targets.iter().enumerate() {
            if self.label_items.get(i).copied().unwrap_or(false) {
                for _ in 0..labels_of(i).max(1) {
                    out.push((Some(ElementKind::Vertex), None));
                }
                continue;
            }
            match target.element {
                Some(ElementKind::Edge) if !target.labels.is_empty() => {
                    for label in &target.labels {
                        out.push((Some(ElementKind::Edge), Some(label.as_str())));
                    }
                }
                element => out.push((element, None)),
            }
        }
        out
    }

    /// Whether an item changes labels (the flow after the step forgets its labels).
    pub(crate) fn relabels(&self) -> bool {
        self.label_items.iter().any(|label_item| *label_item)
    }

    /// The step labels the targets read (sorted).
    pub(crate) fn labels(&self) -> Vec<CompactString> {
        labels_read_by(&self.bindings)
    }

    /// The step labels the targets read in every occurrence (sorted).
    pub(crate) fn history(&self) -> Vec<CompactString> {
        history_read_by(&self.bindings)
    }

    /// The target of item `item` on `row`: `NULL` (a no-op, W25), a vertex or an edge. A
    /// target removed earlier in the query is the storage's `ElementRemoved` error (W27, as
    /// for `property()` on it); anything else than an element is 22G03.
    pub(crate) fn resolve<S: GraphStorage>(
        &self,
        context: &TraverserContext<'_, S>,
        row: &Traverser<S>,
        item: usize,
    ) -> TraverserResult<Resolved<S>> {
        let value = Row::new(context, row, &self.bindings)
            .bound(item)
            .map(std::borrow::Cow::into_owned);
        match value {
            None | Some(TraverserValue::Computed(ElementProperty::Null, _)) => Ok(Resolved::Null),
            Some(TraverserValue::Vertex(vertex)) => {
                if context.storage.contains_vertex(vertex) {
                    Ok(Resolved::Vertex(vertex))
                } else {
                    Err(Box::new(
                        GraphError::element_removed(GraphNodeType::Vertex).into(),
                    ))
                }
            }
            Some(TraverserValue::Edge(edge)) => {
                if context.storage.contains_edge(edge) {
                    Ok(Resolved::Edge(edge))
                } else {
                    Err(Box::new(
                        GraphError::element_removed(GraphNodeType::Edge).into(),
                    ))
                }
            }
            Some(other) => Err(Box::new(self.not_an_element(item, &other.kind_name()))),
        }
    }

    #[inline(never)]
    fn not_an_element(&self, item: usize, kind: &str) -> TraverserError {
        let (name, span) = match self.targets.get(item) {
            Some(target) => (target.name.as_str(), target.span),
            None => ("?", NO_SPAN),
        };
        GqlErrorBuilder::new(
            GqlErrorKind::Data,
            span.start..span.end,
            format!(
                "{} changes a node or an edge, but `{name}` holds a value of type '{kind}'",
                self.clause
            ),
        )
        .gqlstatus(status::INVALID_VALUE_TYPE)
        .alternative(format!(
            "keep only the rows where `{name}` holds an element, or find the node first with \
             a MATCH and {} it, as in g.gql(\"FOR n IN ['marko'] MATCH (x {{name: n}}) {} \
             RETURN x.name\")",
            self.clause.to_ascii_lowercase(),
            if self.clause == "SET" {
                "SET x.k = 1"
            } else {
                "REMOVE x.age"
            }
        ))
        .topic(GqlHelpTopic::Set)
        .into_error(&self.query)
    }

    /// The error of a label item on an edge (W26: an edge has exactly one label); `label` the
    /// first label of the item.
    #[inline(never)]
    pub(crate) fn edge_label_item(&self, item: usize, label: &str) -> TraverserError {
        let (name, span) = match self.targets.get(item) {
            Some(target) => (target.name.as_str(), target.span),
            None => ("?", NO_SPAN),
        };
        GqlErrorBuilder::new(
            GqlErrorKind::Data,
            span.start..span.end,
            format!(
                "{} of a label changes the label set of a node, but `{name}` holds an edge",
                self.clause
            ),
        )
        .gqlstatus(status::INVALID_VALUE_TYPE)
        .alternative(format!(
            "an edge has exactly one label, changed with the Gremlin step set_label(), for \
             example g.e().has_label(\"knows\").set_label({})",
            crate::value::literal::rhai_string(label)
        ))
        .topic(GqlHelpTopic::Set)
        .into_error(&self.query)
    }
}

/// `query` with the bytes of `span` replaced by `with`, as a runnable `g.gql(..)` (rule 6: the
/// fix built from the failing query); `None` when the span does not fit the query.
pub(crate) fn rewritten(query: &str, span: Span, with: &str) -> Option<String> {
    let before = query.get(..span.start)?;
    let after = query.get(span.end..)?;
    let quoted = crate::value::literal::rhai_query(&format!("{before}{with}{after}"))?;
    Some(format!("g.gql({quoted})"))
}

/// Sets `key` on `target` (a stored value).
pub(crate) fn set_property<S: GraphStorage>(
    storage: &mut S,
    target: &Resolved<S>,
    key: &CompactString,
    value: ElementProperty,
) -> TraverserResult<()> {
    match target {
        Resolved::Null => {}
        Resolved::Vertex(v) => storage.set_vertex_property(*v, key.clone(), value)?,
        Resolved::Edge(e) => storage.set_edge_property(*e, key.clone(), value)?,
    }
    Ok(())
}

/// Removes `key` from `target` (a missing key is a no-op): through the storage's
/// `drop_*_property`, never a stored `NULL` (user decision 2026-10-20).
pub(crate) fn remove_property<S: GraphStorage>(
    storage: &mut S,
    target: &Resolved<S>,
    key: &str,
) -> TraverserResult<()> {
    match target {
        Resolved::Null => {}
        Resolved::Vertex(v) => storage.drop_vertex_property(key, *v)?,
        Resolved::Edge(e) => storage.drop_edge_property(key, *e)?,
    }
    Ok(())
}

/// The property keys `target` carries now.
fn keys_of<S: GraphStorage>(storage: &S, target: &Resolved<S>) -> Vec<CompactString> {
    let properties = match target {
        Resolved::Null => None,
        Resolved::Vertex(v) => storage.vertex_properties(*v),
        Resolved::Edge(e) => storage.edge_properties(*e),
    };
    properties
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default()
}

/// Sets the value of a `SET` item clause: the entries of the map an `=` / `+=` item applies,
/// `None` for a key the item removes.
type Entries = Vec<(CompactString, Option<ElementProperty>)>;

/// Applies `SET` items left to right on every row (W22), each seeing the earlier ones:
/// `n.k = expr` sets `k` (a `NULL` value removes it, W20, through the storage's
/// `drop_*_property`: never a stored `NULL`); `n = expr` replaces every property with the map
/// or the element's properties `expr` gives (W23; keys not in it are removed, `NULL` entries
/// are absent; the whole map is evaluated and every value checked before the item's first
/// write, the `property(map)` rule; an element's properties are a snapshot taken before it,
/// so `SET n = n` changes nothing); `n += expr` merges (W24, a `NULL` entry removes its key);
/// `n:A:B` adds each label (idempotent, W26). Values are the INSERT values (W21: maps and
/// mixed lists stored, a node, an edge or a path 22G03). A `NULL` right side of `=` / `+=` is
/// 22004 (D4 of stage 3c). The storage write path applies the schema (write-time coercion,
/// enforcement).
///
/// Targets: a `NULL` target makes the item a no-op whatever its right side (W25, not
/// evaluated); a target removed earlier in the query is the storage's `ElementRemoved`
/// error (W27); a value that is not an element 22G03; a label item on an edge 22G03 (an edge
/// has one label: `set_label()`).
///
/// Emits each row unchanged (the current object stays, no new path position). A traverser
/// of bulk n is n rows (W15: `SET n.c = n.c + 1` over a row of bulk 3 adds 3), each emitted
/// with bulk 1 (derived, rule 20). Asks `Update` on the data of each item's target as the plan
/// states it, like `property()` (design C6); a label item `Update` on `Data { Vertex, None }`
/// like `add_label()` (D3). Built only by the GQL lowering; not a DSL step.
struct StepGqlSet<'g, S: GraphStorage + 'g> {
    items: Arc<[GqlSetItem<S>]>,
    write: WriteItems,
    /// The variable slots of the values (`values` index `bindings`).
    bindings: Arc<[Binding]>,
    /// Every step label the targets and the values read (sorted), for `path_requirement()`.
    labels: Arc<[CompactString]>,
    /// The step labels read in every occurrence, for `history_labels()`.
    history: Arc<[CompactString]>,
    host: SubqueryHost<'g, S>,
}

impl<'g, S: GraphStorage + 'g> Clone for StepGqlSet<'g, S> {
    fn clone(&self) -> Self {
        Self {
            items: Arc::clone(&self.items),
            write: self.write.clone(),
            bindings: Arc::clone(&self.bindings),
            labels: Arc::clone(&self.labels),
            history: Arc::clone(&self.history),
            host: self.host.clone(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepGqlSet<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "gql:set({})",
            crate::value::literal::rhai_string(&self.write.text())
        )
    }
}

impl<'g, S: GraphStorage + 'g> StepGqlSet<'g, S> {
    /// Evaluates `value` on `row`.
    fn evaluate(
        &self,
        context: &mut TraverserContext<'_, S>,
        row: &Traverser<S>,
        answers: &mut SubqueryAnswers<S>,
        value: &RowExpr<S>,
    ) -> TraverserResult<crate::gql::runtime::RowValue<S>> {
        self.host
            .evaluate(context, row, &self.bindings, answers, |r| value.eval(r))?
            .map_err(|error| Box::new(eval_error(&self.write.query, error)))
    }

    /// The entries an `=` / `+=` item applies: the map or the properties of the element
    /// `value` gives, every value checked (W21) before the item writes anything.
    fn entries(
        &self,
        context: &TraverserContext<'_, S>,
        item: usize,
        value_expr: &RowExpr<S>,
        value: crate::gql::runtime::RowValue<S>,
    ) -> TraverserResult<Entries> {
        let copy = |map: Option<&PropertyMap>| -> Entries {
            map.map(|map| {
                map.iter()
                    .map(|(key, value)| {
                        let value = match value {
                            // A stored Graphersal null reads as NULL in GQL: absent.
                            ElementProperty::Null => None,
                            other => Some(other.clone()),
                        };
                        (key.clone(), value)
                    })
                    .collect()
            })
            .unwrap_or_default()
        };
        match value {
            Value::Map(entries) => {
                let mut out = crate::traversal::capacity::vec_capped(entries.len());
                for (key, value) in entries {
                    if matches!(value, Value::Null) {
                        out.push((key, None));
                        continue;
                    }
                    match stored_value(value) {
                        Ok(property) => out.push((key, Some(property))),
                        Err(kind) => {
                            return Err(Box::new(self.not_a_value(value_expr, &key, kind, false)));
                        }
                    }
                }
                Ok(out)
            }
            Value::Element(GqlElement::Vertex(v)) => {
                if !context.storage.contains_vertex(v) {
                    return Err(Box::new(
                        GraphError::element_removed(GraphNodeType::Vertex).into(),
                    ));
                }
                Ok(copy(context.storage.vertex_properties(v)))
            }
            Value::Element(GqlElement::Edge(e)) => {
                if !context.storage.contains_edge(e) {
                    return Err(Box::new(
                        GraphError::element_removed(GraphNodeType::Edge).into(),
                    ));
                }
                Ok(copy(context.storage.edge_properties(e)))
            }
            Value::Null => Err(Box::new(self.null_map(item, value_expr))),
            Value::Scalar(_) | Value::List(_) | Value::Path(_) => {
                let kind = match value {
                    Value::List(_) => "a list",
                    Value::Path(_) => "a path",
                    _ => "a single value",
                };
                Err(Box::new(self.not_a_map(item, value_expr, kind)))
            }
        }
    }

    /// `element`: the value itself is the node or edge (not a list or map holding one), so
    /// the advice stores its `element_id()` in the failing query.
    #[inline(never)]
    fn not_a_value(
        &self,
        value: &RowExpr<S>,
        key: &str,
        kind: &str,
        element: bool,
    ) -> TraverserError {
        let span = value.span();
        let query = &self.write.query;
        let fixed = element
            .then(|| {
                let text = query.get(span.start..span.end)?;
                rewritten(query, span, &format!("element_id({text})"))
            })
            .flatten();
        GqlErrorBuilder::new(
            GqlErrorKind::Data,
            span.start..span.end,
            format!(
                "the value of property `{key}` is {kind}, and a property cannot hold a graph \
                 element"
            ),
        )
        .gqlstatus(status::INVALID_VALUE_TYPE)
        .alternative(match fixed {
            Some(fixed) => format!(
                "store a value of it instead, such as its id, as in {fixed}, or its properties \
                 with properties(..)"
            ),
            None => format!(
                "store a value of it instead, such as SET n.{key} = element_id(..) or SET \
                 n.{key} = properties(..)"
            ),
        })
        .topic(GqlHelpTopic::Set)
        .into_error(&self.write.query)
    }

    fn operator(&self, item: usize) -> &'static str {
        match self.items.get(item) {
            Some(GqlSetItem::Merge { .. }) => "+=",
            _ => "=",
        }
    }

    fn target_name(&self, item: usize) -> &str {
        self.write
            .targets
            .get(item)
            .map_or("n", |target| target.name.as_str())
    }

    #[inline(never)]
    fn null_map(&self, item: usize, value: &RowExpr<S>) -> TraverserError {
        let span = value.span();
        let (name, op) = (self.target_name(item), self.operator(item));
        GqlErrorBuilder::new(
            GqlErrorKind::Data,
            span.start..span.end,
            format!("the right side of SET {name} {op} .. is NULL, not a map"),
        )
        .gqlstatus(status::NULL_VALUE)
        .alternative({
            let query = &self.write.query;
            let text = query.get(span.start..span.end).unwrap_or("$p");
            if op == "+=" {
                let fixed = rewritten(query, span, &format!("coalesce({text}, {{}})"));
                format!(
                    "SET {name} += {{}} changes nothing: give a NULL map that default with \
                     coalesce($p, {{}}), as in {}",
                    fixed.unwrap_or_else(|| format!("SET {name} += coalesce($p, {{}})"))
                )
            } else {
                let fixed = rewritten(
                    query,
                    span,
                    &format!("coalesce({text}, properties({name}))"),
                );
                format!(
                    "SET {name} = {{}} removes every property; to keep them when the map is \
                     NULL, give it the element's own properties as a default with \
                     coalesce($p, properties({name})), as in {}",
                    fixed.unwrap_or_else(|| {
                        format!("SET {name} = coalesce($p, properties({name}))")
                    })
                )
            }
        })
        .topic(GqlHelpTopic::Set)
        .into_error(&self.write.query)
    }

    #[inline(never)]
    fn not_a_map(&self, item: usize, value: &RowExpr<S>, kind: &str) -> TraverserError {
        let span = value.span();
        let (name, op) = (self.target_name(item), self.operator(item));
        GqlErrorBuilder::new(
            GqlErrorKind::Data,
            span.start..span.end,
            format!(
                "the right side of SET {name} {op} .. is {kind}, but it must be a map or a node \
                 or an edge"
            ),
        )
        .gqlstatus(status::INVALID_VALUE_TYPE)
        .alternative({
            let query = &self.write.query;
            let item_span = self.items.get(item).map(GqlSetItem::span);
            let fixed = (kind != "a path")
                .then(|| {
                    let item_span = item_span?;
                    let text = query.get(span.start..span.end)?;
                    rewritten(query, item_span, &format!("{name}.k = {text}"))
                })
                .flatten();
            match fixed {
                Some(fixed) => format!(
                    "set one property with SET {name}.k = <value>, as in {fixed}, or give a \
                     map, as in SET {name} {op} {{k: 1}}"
                ),
                None => format!(
                    "set one property with SET {name}.k = <value>, or give a map, as in SET \
                     {name} {op} {{k: 1}}"
                ),
            }
        })
        .topic(GqlHelpTopic::Set)
        .into_error(&self.write.query)
    }

    /// Applies item `index` on `row`.
    fn apply(
        &self,
        context: &mut TraverserContext<'_, S>,
        row: &Traverser<S>,
        answers: &mut SubqueryAnswers<S>,
        index: usize,
        item: &GqlSetItem<S>,
    ) -> TraverserResult<()> {
        let target = self.write.resolve(context, row, index)?;
        if matches!(target, Resolved::Null) {
            return Ok(());
        }
        match item {
            GqlSetItem::Property { key, value, .. } => {
                let evaluated = self.evaluate(context, row, answers, value)?;
                if matches!(evaluated, Value::Null) {
                    return remove_property(context.storage, &target, key);
                }
                let element = matches!(evaluated, Value::Element(_));
                match stored_value(evaluated) {
                    Ok(property) => set_property(context.storage, &target, key, property),
                    Err(kind) => Err(Box::new(self.not_a_value(value, key, kind, element))),
                }
            }
            GqlSetItem::Replace { value, .. } | GqlSetItem::Merge { value, .. } => {
                let evaluated = self.evaluate(context, row, answers, value)?;
                let entries = self.entries(context, index, value, evaluated)?;
                if matches!(item, GqlSetItem::Replace { .. }) {
                    for key in keys_of(&*context.storage, &target) {
                        let kept = entries
                            .iter()
                            .any(|(k, value)| *k == key && value.is_some());
                        if !kept {
                            remove_property(context.storage, &target, &key)?;
                        }
                    }
                }
                for (key, value) in entries {
                    match value {
                        Some(value) => set_property(context.storage, &target, &key, value)?,
                        None => remove_property(context.storage, &target, &key)?,
                    }
                }
                Ok(())
            }
            GqlSetItem::Labels { labels, .. } => match target {
                Resolved::Vertex(v) => {
                    for label in labels {
                        context.storage.add_vertex_label(v, label.clone())?;
                    }
                    Ok(())
                }
                Resolved::Edge(_) => Err(Box::new(
                    self.write
                        .edge_label_item(index, labels.first().map_or("T", |l| l.as_str())),
                )),
                Resolved::Null => Ok(()),
            },
        }
    }
}

fn set_step<'g, S: GraphStorage + 'g>(
    query: Arc<str>,
    items: Vec<GqlSetItem<S>>,
    bindings: Arc<[Binding]>,
    children: Vec<AnonymousTraversal<'g, S>>,
) -> GraphStep<'g, S> {
    let write = WriteItems::new(
        query,
        "SET",
        items.iter().map(|item| {
            (
                item.target().clone(),
                item.span(),
                matches!(item, GqlSetItem::Labels { .. }),
            )
        }),
    );
    let values = || items.iter().filter_map(GqlSetItem::value);
    let mut labels = write.labels();
    labels.extend(labels_read(values(), &bindings));
    labels.sort_unstable();
    labels.dedup();
    let mut history = write.history();
    history.extend(history_read(values(), &bindings));
    history.sort_unstable();
    history.dedup();
    GraphStep::SideEffectMut(
        "gql:set",
        Box::new(StepGqlSet {
            items: items.into(),
            write,
            bindings,
            labels: labels.into(),
            history: history.into(),
            host: SubqueryHost::new(children),
        }),
    )
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Appends `gql:set(items)`: the items of one `SET` clause, their values reading the
    /// variable slots `bindings` and hosting the subqueries `children` (child `i` answers the
    /// values' subquery `i`); see [`StepGqlSet`].
    pub(crate) fn gql_set(
        self,
        query: Arc<str>,
        items: Vec<GqlSetItem<S>>,
        bindings: Arc<[Binding]>,
        children: Vec<AnonymousTraversal<'g, S>>,
    ) -> Self {
        self.push_step(set_step(query, items, bindings, children))
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Appends `gql:set(items)` hosting the subqueries `children`; see [`StepGqlSet`].
    pub(crate) fn gql_set(
        self,
        query: Arc<str>,
        items: Vec<GqlSetItem<S>>,
        bindings: Arc<[Binding]>,
        children: Vec<AnonymousTraversal<'g, S>>,
    ) -> Self {
        self.push_step(set_step(query, items, bindings, children))
    }
}

/// The rows of `source` for a `SET` / `REMOVE` step: bulk-1 copies of it (W15, the bulk
/// checked like a bulk expansion); none without an input (nothing to change).
pub(crate) fn write_rows<S: GraphStorage>(
    context: &TraverserContext<'_, S>,
    source: Option<Traverser<S>>,
    step: &str,
) -> TraverserResult<Vec<Traverser<S>>> {
    match source {
        Some(source) => Ok(insert_rows(context, Some(source), step)?.0),
        None => Ok(Vec::new()),
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepSideEffectMut<'g, S> for StepGqlSet<'g, S> {
    fn gql_updates(&self) -> Vec<(Option<ElementKind>, Option<&str>)> {
        self.write.updates_with(|item| {
            self.items
                .get(item)
                .and_then(GqlSetItem::label_names)
                .map_or(1, <[CompactString]>::len)
        })
    }

    fn gql_relabels(&self) -> bool {
        self.write.relabels()
    }

    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Option<Traverser<S>>,
    ) -> TraverserResult<Vec<Traverser<S>>> {
        let rows = write_rows(context, source, "gql:set")?;
        for row in &rows {
            context.tick()?;
            let mut answers = SubqueryAnswers::default();
            for (index, item) in self.items.iter().enumerate() {
                self.apply(context, row, &mut answers, index, item)?;
            }
        }
        Ok(rows)
    }

    fn extends_path(&self) -> bool {
        false
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

    fn clone_box(&self) -> Box<dyn GraphStepSideEffectMut<'g, S> + 'g> {
        Box::new(self.clone())
    }
}
