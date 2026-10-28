use std::fmt::Debug;

use crate::graph::GraphStorage;
use crate::*;

#[derive(Clone)]
struct StepHasP<B: Into<PValue> + Debug + Clone + Send + Sync> {
    key: String,
    predicate: P<B>,
}

impl<B: Into<PValue> + Debug + Clone + Send + Sync> fmt::Display for StepHasP<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "has({}, {})",
            crate::value::literal::rhai_string(&self.key),
            crate::value::predicate::render_predicate(&self.predicate)
        )
    }
}

/// An operand of a predicate as the stored value it compares with; `None` for `NaN` (it compares
/// with nothing, the index is not used) and for a vector (not an index key: the filter scans).
fn index_operand(value: PValue) -> Option<ElementProperty> {
    Some(match value {
        PValue::String(text) => ElementProperty::String(text),
        PValue::Number(number) => {
            if number.as_f64().is_some_and(f64::is_nan) {
                return None;
            }
            ElementProperty::Number(number)
        }
        PValue::Boolean(flag) => ElementProperty::Boolean(flag),
        PValue::Uuid(bits) => ElementProperty::Uuid(bits),
        PValue::Null => ElementProperty::Null,
        PValue::Temporal(value) => ElementProperty::Temporal(value),
        // Not an index key (the index skips vectors): the index is not used, the filter scans.
        PValue::Vector(_) => return None,
    })
}

/// The slices of an ordered property index that hold every value `predicate` accepts (book
/// page "Property Indexes"): `eq`, `within`, `gt`, `gte`, `lt`, `lte`, `inside`, `between` and
/// `outside`; `None` for every other predicate (negations, text predicates, composites,
/// `typeOf`), which the index does not serve. Every operand is a value (`has(key, P)` and
/// `is(P)` never read a path label, `P::test`).
pub(crate) fn index_ranges<B: Into<PValue> + Clone>(
    predicate: &P<B>,
) -> Option<Vec<crate::graph::PropertyIndexRange>> {
    use crate::graph::PropertyIndexRange as R;
    use std::ops::Bound::{Excluded, Included, Unbounded};
    let operand = |value: &B| index_operand(value.clone().into());
    Some(match predicate {
        P::Eq(value) => vec![R::equal(operand(value)?)],
        P::WithIn(values) => values
            .iter()
            .map(|value| operand(value).map(R::equal))
            .collect::<Option<Vec<_>>>()?,
        P::Gt(value) => vec![R::new(Excluded(operand(value)?), Unbounded)],
        P::Gte(value) => vec![R::new(Included(operand(value)?), Unbounded)],
        P::Lt(value) => vec![R::new(Unbounded, Excluded(operand(value)?))],
        P::Lte(value) => vec![R::new(Unbounded, Included(operand(value)?))],
        P::Inside(low, high) => vec![R::new(Excluded(operand(low)?), Excluded(operand(high)?))],
        P::Between(low, high) => vec![R::new(Included(operand(low)?), Excluded(operand(high)?))],
        P::Outside(low, high) => vec![
            R::new(Unbounded, Excluded(operand(low)?)),
            R::new(Excluded(operand(high)?), Unbounded),
        ],
        P::Neq(_)
        | P::WithOut(_)
        | P::StartingWith(_)
        | P::NotStartingWith(_)
        | P::EndingWith(_)
        | P::NotEndingWith(_)
        | P::Containing(_)
        | P::NotContaining(_)
        | P::Regex(_)
        | P::NotRegex(_)
        | P::TypeOf(_)
        | P::And(_, _)
        | P::Or(_, _)
        | P::Not(_) => return None,
    })
}

/// The step for `has(key, P)`: the literal-name step for a name, the path step for a path.
fn has_p_step<'g, S: GraphStorage + 'g, B: Into<PValue> + Debug + Clone + Send + Sync + 'static>(
    key: PropKey,
    predicate: P<B>,
) -> GraphStep<'g, S> {
    match key {
        PropKey::Name(name) => GraphStep::Filter(
            "has_p",
            Box::new(StepHasP {
                key: name.to_string(),
                predicate,
            }),
        ),
        PropKey::Path(path) => super::step_has_path::has_path_p_step(path, predicate),
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `has(key, P)`: keeps a traverser whose `key` satisfies the predicate. On a vertex or edge
    /// `key` is a property of the element; on a map it is a map key.
    ///
    /// Deviation from TinkerPop (book "Property Elements"): on a property element
    /// (`properties("meta")`) TinkerPop tests a meta-property; Graphersal has none and tests the
    /// key inside the property's object value (`meta.a`), never the owning element's top-level
    /// `key`. A missing nested key filters out, a non-object value is a cast error.
    pub fn has_p<B: Into<PValue> + Debug + Clone + Send + Sync + 'static, T: Into<P<B>> + Debug>(
        self,
        key: impl Into<PropKey>,
        predicate: T,
    ) -> Self {
        self.push_step(has_p_step(key.into(), predicate.into()))
    }
}

impl __ {
    /// Graphersal form of has(key, predicate): keeps elements whose property matches the predicate
    /// (a bare value means P.eq). Starts an anonymous traversal (`__.has_p()`).
    pub fn has_p<
        'g,
        S: GraphStorage + 'g,
        B: Into<PValue> + Debug + Clone + Send + Sync + 'static,
        T: Into<P<B>> + Debug,
    >(
        key: impl Into<PropKey>,
        predicate: T,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().has_p(key, predicate)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Graphersal form of has(key, predicate): keeps elements whose property matches the predicate
    /// (a bare value means P.eq).
    pub fn has_p<B: Into<PValue> + Debug + Clone + Send + Sync + 'static, T: Into<P<B>> + Debug>(
        self,
        key: impl Into<PropKey>,
        predicate: T,
    ) -> Self {
        self.push_step(has_p_step(key.into(), predicate.into()))
    }
}

impl<'g, S: GraphStorage + 'g, B: Into<PValue> + Debug + Clone + Send + Sync + 'static>
    GraphStepFilter<'g, S> for StepHasP<B>
{
    fn property_access(
        &self,
        visit: &mut dyn FnMut(Option<&str>, crate::graph::workload::AccessKind),
    ) {
        visit(
            Some(&self.key),
            crate::graph::workload::AccessKind::FilterScan,
        );
    }
    fn index_condition(&self) -> Option<crate::traversal::step::IndexCondition<'_>> {
        index_ranges(&self.predicate).map(|ranges| crate::traversal::step::IndexCondition {
            key: self.key.as_str(),
            transform: None,
            ranges,
        })
    }

    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<bool> {
        match &source.value {
            TraverserValue::Vertex(node_index) => {
                let vertex = context.storage.vertex_properties(*node_index)?;
                if let Some(vertex) = vertex {
                    let value = vertex.get(self.key.as_str());
                    if value.is_some() {
                        let traverser_value = TraverserValue::VertexProperty(
                            CompactString::new(&self.key),
                            *node_index,
                            PropertyHandleKind::Value,
                        );
                        Ok(self.predicate.test(context, &traverser_value)?)
                    } else {
                        Ok(false)
                    }
                } else {
                    Ok(false)
                }
            }

            TraverserValue::Computed(vertex, _) => match vertex {
                ElementProperty::Object(vertex) => {
                    let value = vertex.get(self.key.as_str());
                    if let Some(value) = value {
                        let traverser_value = TraverserValue::Computed(value.to_logical(), None);
                        Ok(self.predicate.test(context, &traverser_value)?)
                    } else {
                        Ok(false)
                    }
                }
                x => Err(Box::new(TraverserError::cast_exception_str(
                    &["object"],
                    crate::graph::schema::value_type_name(x),
                ))),
            },

            TraverserValue::VertexProperty(prop_name, node_index, _) => {
                let prop_name = prop_name.clone();
                let vertex = context.storage.vertex_properties(*node_index)?;
                match vertex {
                    Some(vertex) => {
                        let value = vertex.get(prop_name.as_str());
                        match value {
                            Some(value) => match value {
                                // The key addresses the property's own (object) value,
                                // never another property of the owning element: only the
                                // nested leaf is cloned, to hand the predicate an owned value.
                                ElementProperty::Object(object) => {
                                    match object.get(self.key.as_str()) {
                                        Some(nested) => {
                                            let traverser_value =
                                                TraverserValue::Computed(nested.to_logical(), None);
                                            self.predicate.test(context, &traverser_value)
                                        }
                                        None => Ok(false),
                                    }
                                }
                                x => Err(Box::new(TraverserError::cast_exception_str(
                                    &["object"],
                                    crate::graph::schema::value_type_name(x),
                                ))),
                            },
                            None => Err(Box::new(TraverserError::property_not_found_on_element(
                                prop_name.to_string(),
                                "vertex",
                            ))),
                        }
                    }
                    None => Err(Box::new(TraverserError::property_not_found_on_element(
                        prop_name.to_string(),
                        "vertex",
                    ))),
                }
            }

            TraverserValue::EdgeProperty(prop_name, edge_index, _) => {
                let prop_name = prop_name.clone();
                let edge = context.storage.edge_properties(*edge_index)?;
                match edge {
                    Some(edge) => {
                        let value = edge.get(prop_name.as_str());
                        match value {
                            Some(value) => match value {
                                // The key addresses the property's own (object) value,
                                // never another property of the owning element: only the
                                // nested leaf is cloned, to hand the predicate an owned value.
                                ElementProperty::Object(object) => {
                                    match object.get(self.key.as_str()) {
                                        Some(nested) => {
                                            let traverser_value =
                                                TraverserValue::Computed(nested.to_logical(), None);
                                            self.predicate.test(context, &traverser_value)
                                        }
                                        None => Ok(false),
                                    }
                                }
                                x => Err(Box::new(TraverserError::cast_exception_str(
                                    &["object"],
                                    crate::graph::schema::value_type_name(x),
                                ))),
                            },
                            None => Err(Box::new(TraverserError::property_not_found_on_element(
                                prop_name.to_string(),
                                "edge",
                            ))),
                        }
                    }
                    None => Err(Box::new(TraverserError::property_not_found_on_element(
                        prop_name.to_string(),
                        "edge",
                    ))),
                }
            }

            TraverserValue::Edge(edge_index) => {
                let edge_map = context.storage.edge_properties(*edge_index)?;
                match edge_map {
                    Some(edge_map) => {
                        let value: Option<_> = edge_map.get(self.key.as_str());
                        if value.is_some() {
                            let traverser_value = TraverserValue::EdgeProperty(
                                CompactString::new(&self.key),
                                *edge_index,
                                PropertyHandleKind::Value,
                            );
                            Ok(self.predicate.test(context, &traverser_value)?)
                        } else {
                            Ok(false)
                        }
                    }
                    None => Err(Box::new(TraverserError::cast_exception_str(
                        &["edge"],
                        "null".to_string(),
                    ))),
                }
            }

            TraverserValue::Object(obj) => {
                let key_tv = TraverserValue::Computed(
                    ElementProperty::String(CompactString::new(&self.key)),
                    None,
                );
                if let Some(val) = obj.get(&key_tv) {
                    self.predicate.test(context, val)
                } else {
                    Ok(false)
                }
            }

            TraverserValue::Record { keys, values } => {
                if let Some(idx) = keys.iter().position(|k| k.as_str() == self.key.as_str()) {
                    self.predicate.test(context, &values[idx])
                } else {
                    Ok(false)
                }
            }

            TraverserValue::Path(_) => Err(Box::new(TraverserError::cast_exception_str(
                &[
                    "computed",
                    "vertexProperty",
                    "edgeProperty",
                    "vertex",
                    "edge",
                ],
                "path".to_string(),
            ))),
            TraverserValue::Array(_) => Err(Box::new(TraverserError::cast_exception_str(
                &[
                    "computed",
                    "vertexProperty",
                    "edgeProperty",
                    "vertex",
                    "edge",
                ],
                "array".to_string(),
            ))),
            kind @ (TraverserValue::PathProperty(_) | TraverserValue::Entry(_)) => {
                Err(Box::new(TraverserError::cast_exception_str(
                    &[
                        "computed",
                        "vertexProperty",
                        "edgeProperty",
                        "vertex",
                        "edge",
                    ],
                    kind.type_name().to_string(),
                )))
            }
            TraverserValue::Schema(_) => Err(Box::new(TraverserError::cast_exception_str(
                &[
                    "computed",
                    "vertexProperty",
                    "edgeProperty",
                    "vertex",
                    "edge",
                ],
                "schema".to_string(),
            ))),
            TraverserValue::Graph(_) => Err(Box::new(TraverserError::cast_exception_str(
                &[
                    "computed",
                    "vertexProperty",
                    "edgeProperty",
                    "vertex",
                    "edge",
                ],
                "graph".to_string(),
            ))),
        }
    }

    fn clone_box(&self) -> Box<dyn GraphStepFilter<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_p("lang", "java").count().to_list(),
            rhai: [
                r#"g.V().has_p("lang", "java").count()"#,
                r#"g.v().has_p("lang", "java").count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
    }

    #[test]
    fn where_() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).where_t(__::has_p("lang", "java")).count().to_list(),
            rhai: [
                r#"g.V().where(__.has_p("lang", "java")).count()"#,
                r#"g.v().where(__.has_p("lang", "java")).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
    }

    #[test]
    fn where_inner() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).where_t(__::element().has_p("lang", "java")).count().to_list(),
            rhai: [
                r#"g.V().where(__.element().has_p("lang", "java")).count()"#,
                r#"g.v().where(__.element().has_p("lang", "java")).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
    }

    #[test]
    fn v_1() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_p("lang", P::Eq("java")).count().to_list(),
            rhai: [
                r#"g.V().has_p("lang", P::Eq("java")).count()"#,
                r#"g.v().has_p("lang", P::Eq("java")).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
    }

    #[test]
    fn v_select() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").as_("A").out("created").in_("created").is(P::Eq("A")).id().to_list(),
            rhai: [
                r#"g.V("1").as("A").out("created").in("created").is(P::Eq("A")).id()"#,
                r#"g.v("1").as("A").out("created").in("created").is(P::Eq("A")).id()"#,
            ],
            // `is()` compares with the literal "A" (TinkerPop), never the label `A`.
            assert: |res| assert!(res.is_empty()),
        );
    }

    #[test]
    fn v_within() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_p("lang", P::WithIn(vec!["a", "java"])).count().to_list(),
            rhai: [
                r#"g.V().has_p("lang", P::WithIn(["a", "java"])).count()"#,
                r#"g.v().has_p("lang", P::WithIn(["a", "java"])).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g
            .v(None)
            .has_p("lang", P::WithIn(vec!["a", "java"]))
            .profile()
            .unwrap();
        assert!(
            result
                .to_string()
                .contains(r#"has("lang", P.within(["a", "java"]))"#),
            "{result}"
        );
    }

    use crate::traversal::test_utils::nested_doc_graph;

    /// `has(key, P)` on an object-valued property tests the NESTED value, as the literal form
    /// `has(key, value)` does, never the element's top-level property of that name: d1 has
    /// `meta.a = 1` and a top-level `a = 100`.
    #[test]
    fn nested_key_of_an_object_valued_vertex_property_is_tested() {
        test_dual!(
            graph: nested_doc_graph(),
            rust: |g| g
                .v(None)
                .has_label("doc")
                .properties("meta")
                .has_p("a", P::Eq(1))
                .count()
                .to_list(),
            rhai: [
                r#"g.V().hasLabel("doc").properties("meta").has("a", P.eq(1)).count()"#,
                r#"g.v().has_label("doc").properties("meta").has("a", P::eq(1)).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
        test_dual!(
            graph: nested_doc_graph(),
            rust: |g| g
                .v(None)
                .has_label("doc")
                .properties("meta")
                .has_p("a", P::Eq(100))
                .count()
                .to_list(),
            rhai: [r#"g.V().hasLabel("doc").properties("meta").has("a", P.eq(100)).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
    }

    /// Edge counterpart of the test above: the edge has `info.w = 2` and a top-level `w = 200`.
    #[test]
    fn nested_key_of_an_object_valued_edge_property_is_tested() {
        test_dual!(
            graph: nested_doc_graph(),
            rust: |g| g.e(None).properties("info").has_p("w", P::Eq(2)).count().to_list(),
            rhai: [
                r#"g.E().properties("info").has("w", P.eq(2)).count()"#,
                r#"g.e().properties("info").has("w", P::eq(2)).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
        test_dual!(
            graph: nested_doc_graph(),
            rust: |g| g.e(None).properties("info").has_p("w", P::Eq(200)).count().to_list(),
            rhai: [r#"g.E().properties("info").has("w", P.eq(200)).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
        // the element-level has() still reads the top-level key
        test_dual!(
            graph: nested_doc_graph(),
            rust: |g| g.e(None).has_p("w", P::Eq(200)).count().to_list(),
            rhai: [r#"g.E().has("w", P.eq(200)).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    /// A predicate that compares (`gt`, `within`, a text predicate) sees the nested value too, on
    /// the property element.
    #[test]
    fn nested_key_with_comparing_and_text_predicates() {
        test_dual!(
            graph: nested_doc_graph(),
            rust: |g| g.v(None).has_label("doc").properties("meta").has_p("b", P::StartingWith("x")).count().to_list(),
            rhai: [
                r#"g.V().hasLabel("doc").properties("meta").has("b", TextP.startingWith("x")).count()"#,
                r#"g.v().has_label("doc").properties("meta").has("b", TextP::starting_with("x")).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
        test_dual!(
            graph: nested_doc_graph(),
            rust: |g| g.v(None).has_label("doc").properties("meta").has_p("a", P::Lt(50)).count().to_list(),
            rhai: [r#"g.V().hasLabel("doc").properties("meta").has("a", P.lt(50)).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[test]
    fn a_missing_nested_key_filters_out_and_a_scalar_value_is_a_cast_error() {
        test_dual!(
            graph: nested_doc_graph(),
            rust: |g| g
                .v(None)
                .has_label("doc")
                .properties("meta")
                .has_p("zz", P::Gt(0))
                .count()
                .to_list(),
            rhai: [r#"g.V().hasLabel("doc").properties("meta").has("zz", P.gt(0)).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
        test_dual!(
            graph: nested_doc_graph(),
            rust: |g| g.e(None).properties("info").has_p("zz", P::Gt(0)).count().to_list(),
            rhai: [r#"g.E().properties("info").has("zz", P.gt(0)).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
        test_dual_err!(
            graph: nested_doc_graph(),
            rust: |g| g.v(None).has_label("plain").properties("meta").has_p("a", P::Gt(0)).to_list(),
            rhai: [r#"g.V().hasLabel("plain").properties("meta").has("a", P.gt(0))"#],
            assert: |err| assert!(err.contains("expected type [object], got type 'string'"), "{err}"),
        );
        test_dual_err!(
            graph: nested_doc_graph(),
            rust: |g| g.e(None).properties("note").has_p("a", P::Gt(0)).to_list(),
            rhai: [r#"g.E().properties("note").has("a", P.gt(0))"#],
            assert: |err| assert!(err.contains("expected type [object], got type 'string'"), "{err}"),
        );
    }

    #[test]
    fn edges_and_maps_are_tested_by_key() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).has_p("weight", P::Gte(0.5)).count().to_list(),
            rhai: [
                r#"g.E().has("weight", P.gte(0.5)).count()"#,
                r#"g.e().has("weight", P::gte(0.5)).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(3)),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).has_p("nope", P::Gte(0.5)).count().to_list(),
            rhai: [r#"g.E().has("nope", P.gte(0.5)).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .project(vec!["n", "a"])
                .by("name")
                .by("age")
                .has_p("a", P::Gt(30))
                .count()
                .to_list(),
            rhai: [r#"g.V().project("n", "a").by("name").by("age").has("a", P.gt(30)).count()"#],
            // josh (32), peter (35); lop/ripple have no age and project() leaves the key out
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).element_map(None).has_p("age", P::Lt(29)).count().to_list(),
            rhai: [r#"g.V().elementMap().has("age", P.lt(29)).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).element_map(None).has_p("zz", P::Lt(29)).count().to_list(),
            rhai: [r#"g.V().elementMap().has("zz", P.lt(29)).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
        test_dual!(
            graph: GraphSource::empty(),
            rust: |g| g
                .inject(vec![ElementProperty::from_entries(vec![("a".into(), 3.into())])])
                .has_p("a", P::Gt(2))
                .count()
                .to_list(),
            rhai: [r#"g.inject(#{a: 3}).has("a", P.gt(2)).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[test]
    fn non_element_inputs_are_cast_errors() {
        test_dual_err!(
            graph: GraphSource::empty(),
            rust: |g| g.inject(vec![5]).has_p("a", P::Gt(1)).to_list(),
            rhai: [r#"g.inject(5).has("a", P.gt(1))"#],
            assert: |err| assert!(err.contains("got type 'int64'"), "{err}"),
        );
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").path().has_p("a", P::Gt(1)).to_list(),
            rhai: [r#"g.V("1").path().has("a", P.gt(1))"#],
            assert: |err| assert!(err.contains("got type 'path'"), "{err}"),
        );
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").fold().has_p("a", P::Gt(1)).to_list(),
            rhai: [r#"g.V("1").fold().has("a", P.gt(1))"#],
            assert: |err| assert!(err.contains("got type 'array'"), "{err}"),
        );
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").value_map(None).unfold().has_p("a", P::Gt(1)).to_list(),
            rhai: [r#"g.V("1").valueMap().unfold().has("a", P.gt(1))"#],
            assert: |err| assert!(err.contains("got type 'entry'"), "{err}"),
        );
    }
}
