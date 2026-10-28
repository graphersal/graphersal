use std::fmt::Debug;

use crate::graph::GraphStorage;
use crate::*;

#[derive(Clone)]
struct StepHasValue {
    value: ElementProperty,
}

impl fmt::Display for StepHasValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "has_value({})",
            crate::value::literal::rhai_literal(&self.value)
        )
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Keeps properties whose value equals the given value.
    pub fn has_value<T: Into<ElementProperty>>(self, value: T) -> Self {
        let step = StepHasValue {
            value: value.into(),
        };
        self.push_step(GraphStep::Filter("has_value", Box::new(step)))
    }
}

impl __ {
    /// Keeps properties whose value equals the given value. Starts an anonymous traversal
    /// (`__.has_value()`).
    pub fn has_value<'g, S: GraphStorage + 'g, T: Into<ElementProperty>>(
        value: T,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().has_value(value)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Keeps properties whose value equals the given value.
    pub fn has_value<T: Into<ElementProperty>>(self, value: T) -> Self {
        let step = GraphStep::Filter(
            "has_value",
            Box::new(StepHasValue {
                value: value.into(),
            }),
        );
        self.push_step(step)
    }
}

impl StepHasValue {
    /// Compares `self.value` against one property's own stored value, mirroring what
    /// `TraverserValue::Computed`'s match arm already did correctly for a scalar (D2 of the
    /// `has_value` TinkerPop-compliance fix).
    ///
    /// A scalar (`String`/`Number`/`Boolean`/`Uuid`/`Temporal`) is compared directly — this is the fix: the
    /// pre-existing bug compared only `Computed`-shaped traversers this way and threw a
    /// misleading `CastException` ("expected object") for every other traverser shape holding a
    /// scalar property value, which is the common case once `properties()` (this engine has no
    /// `Cardinality` concept) can hand a bare `VertexProperty`/`EdgeProperty` traverser straight
    /// to a `Filter` step. `Null` never matches (there is nothing to equal) and `Array` is left
    /// unmatched too, both preserved from the pre-existing `Computed` behaviour. A nested
    /// `Object`/`Vertex`/`Edge` value (this engine's own map/group-by shapes, e.g.
    /// `group().by(By::Count)`'s result — never a real per-property cardinality list, which this
    /// engine doesn't have) is searched: any of its own sub-values matching is a match, which is
    /// what every one of `test_value`'s call sites already relied on for that shape.
    fn matches_scalar_or_nested(&self, value: &ElementProperty) -> bool {
        match value {
            ElementProperty::Null => false,
            ElementProperty::Array(_) => false,
            ElementProperty::String(_)
            | ElementProperty::Compressed(_)
            | ElementProperty::Deduped(_)
            | ElementProperty::Number(_)
            | ElementProperty::Boolean(_)
            | ElementProperty::Uuid(_)
            | ElementProperty::Temporal(_)
            | ElementProperty::Vector(_) => self.value.eq(value),
            ElementProperty::Object(_) | ElementProperty::Vertex(_) | ElementProperty::Edge(_) => {
                value
                    .map_entries()
                    .is_some_and(|nested| nested.values().any(|r| self.value.eq(r)))
            }
            ElementProperty::Map(nested) => nested.values().any(|r| self.value.eq(r)),
        }
    }

    pub(crate) fn test_value<S: GraphStorage>(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &TraverserValue<S>,
    ) -> TraverserResult<bool> {
        match source {
            TraverserValue::Vertex(v) => {
                let vertex = context.storage.vertex_properties(*v)?;
                match vertex {
                    Some(vertex) => Ok(vertex.values().any(|r| self.matches_scalar_or_nested(r))),
                    None => Ok(false),
                }
            }

            TraverserValue::Computed(vertex, _) => Ok(self.matches_scalar_or_nested(vertex)),
            TraverserValue::PathProperty(prop) => Ok(self.matches_scalar_or_nested(&prop.1)),

            TraverserValue::VertexProperty(prop_name, node_index, _) => {
                let prop_name = prop_name.clone();
                let vertex = context.storage.vertex_properties(*node_index)?;
                match vertex {
                    Some(vertex) => {
                        let value = vertex.get(prop_name.as_str());
                        match value {
                            Some(value) => Ok(self.matches_scalar_or_nested(value)),
                            None => Err(Box::new(TraverserError::Value(
                                ValueError::PropertyNotFound {
                                    key: prop_name.to_string(),
                                    element_type: "vertex",
                                },
                            ))),
                        }
                    }
                    None => Err(Box::new(TraverserError::Value(
                        ValueError::PropertyNotFound {
                            key: prop_name.to_string(),
                            element_type: "vertex",
                        },
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
                            Some(value) => Ok(self.matches_scalar_or_nested(value)),
                            None => Err(Box::new(TraverserError::Value(
                                ValueError::PropertyNotFound {
                                    key: prop_name.to_string(),
                                    element_type: "edge",
                                },
                            ))),
                        }
                    }
                    None => Err(Box::new(TraverserError::Value(
                        ValueError::PropertyNotFound {
                            key: prop_name.to_string(),
                            element_type: "edge",
                        },
                    ))),
                }
            }

            TraverserValue::Edge(edge_index) => {
                let edge_map = context.storage.edge_properties(*edge_index)?;
                match edge_map {
                    Some(edge_map) => {
                        Ok(edge_map.values().any(|r| self.matches_scalar_or_nested(r)))
                    }
                    None => Err(Box::new(TraverserError::cast_exception_str(
                        &["edge"],
                        "null".to_string(),
                    ))),
                }
            }

            TraverserValue::Object(obj) => {
                for r in obj.values() {
                    let x = self.test_value(context, r)?;
                    if x {
                        return Ok(true);
                    }
                }
                Ok(false)
            }

            TraverserValue::Record { values, .. } => {
                for r in values {
                    let x = self.test_value(context, r)?;
                    if x {
                        return Ok(true);
                    }
                }
                Ok(false)
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
            TraverserValue::Entry(_) => Err(Box::new(TraverserError::cast_exception_str(
                &[
                    "computed",
                    "vertexProperty",
                    "edgeProperty",
                    "vertex",
                    "edge",
                ],
                "entry".to_string(),
            ))),
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
}

impl<'g, S: GraphStorage + 'g> GraphStepFilter<'g, S> for StepHasValue {
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<bool> {
        self.test_value(context, &source.value)
    }

    fn clone_box(&self) -> Box<dyn GraphStepFilter<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

/// `hasValue(P<V>)` (D1 of the `has_value` TinkerPop-compliance fix): the `P`-predicate overload
/// of [`StepHasValue`], mirroring vanilla `GraphTraversal.hasValue(P<Object>)`.
///
/// TinkerPop's real `hasValue()` overload set (verified against the vanilla Java/Groovy
/// reference and the Gremlin grammar) is: `hasValue(Object)` — already `StepHasValue` — and
/// `hasValue(Object... values)`/`hasValue(P<Object>)`, where the variadic form is sugar for "any
/// of these values" (`P.within(values)`), not a distinct predicate shape; the single-predicate
/// case (`hasValue(P.lt(0.3))`) covers it, and `P::WithIn` already exists for an explicit
/// multi-value OR (`hasValue(P.within(1, 2, 3))`), so no separate multi-value constructor is
/// added here — inventing one would duplicate `P::WithIn` for no gain.
///
/// Kept as its own step type (rather than folding into [`StepHasValue`] behind an internal enum)
/// because a literal comparison never needs `TraverserContext`, while a predicate
/// comparison always does (`P::test` coerces the candidate value; like `has(key, P)` it never
/// reads a path label); splitting the two keeps the literal path exactly as cheap as it already was and
/// mirrors this codebase's own `has()`/`has_p()` split.
struct StepHasValueP<B: Into<PValue> + Debug + Clone + Send + Sync> {
    predicate: P<B>,
}

impl<B: Into<PValue> + Debug + Clone + Send + Sync> fmt::Display for StepHasValueP<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "has_value({})",
            crate::value::predicate::render_predicate(&self.predicate)
        )
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `hasValue(P<V>)`: keeps every element (or bare property/computed value) with at least one
    /// property whose value satisfies `predicate`, e.g.
    /// `g.v(None).both_e(None).properties(None).dedup().has_key("weight").has_value_p(P::Lt(0.3))`.
    pub fn has_value_p<
        B: Into<PValue> + Debug + Clone + Send + Sync + 'static,
        T: Into<P<B>> + Debug,
    >(
        self,
        predicate: T,
    ) -> Self {
        let step = StepHasValueP {
            predicate: predicate.into(),
        };
        self.push_step(GraphStep::Filter("has_value", Box::new(step)))
    }
}

impl __ {
    /// `hasValue(P<V>)`, the anonymous-traversal counterpart of
    /// [`GraphTraversalSource::has_value_p`].
    pub fn has_value_p<
        'g,
        S: GraphStorage + 'g,
        B: Into<PValue> + Debug + Clone + Send + Sync + 'static,
        T: Into<P<B>> + Debug,
    >(
        predicate: T,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().has_value_p(predicate)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `hasValue(P<V>)`, the anonymous-traversal counterpart of
    /// [`GraphTraversalSource::has_value_p`].
    pub fn has_value_p<
        B: Into<PValue> + Debug + Clone + Send + Sync + 'static,
        T: Into<P<B>> + Debug,
    >(
        self,
        predicate: T,
    ) -> Self {
        let step = GraphStep::Filter(
            "has_value",
            Box::new(StepHasValueP {
                predicate: predicate.into(),
            }),
        );
        self.push_step(step)
    }
}

impl<B: Into<PValue> + Debug + Clone + Send + Sync> StepHasValueP<B> {
    /// Tests `predicate` against one property's own stored value.
    ///
    /// Same scalar-vs-nested split as [`StepHasValue::matches_scalar_or_nested`] (D2), the
    /// predicate-testing counterpart: a scalar is wrapped as a `Computed` traverser value and
    /// handed to `P::test` (which already resolves path-label references and numeric/boolean
    /// coercion correctly for that shape); a nested `Object`/`Vertex`/`Edge` map is searched,
    /// cloning its values first because they are commonly borrowed straight out of graph storage
    /// and `P::test` needs a mutable `TraverserContext` to run.
    fn matches_value<S: GraphStorage>(
        &self,
        context: &mut TraverserContext<'_, S>,
        value: &ElementProperty,
    ) -> TraverserResult<bool> {
        match value {
            ElementProperty::Null => Ok(false),
            ElementProperty::Array(_) => Ok(false),
            ElementProperty::String(_)
            | ElementProperty::Compressed(_)
            | ElementProperty::Deduped(_)
            | ElementProperty::Number(_)
            | ElementProperty::Boolean(_)
            | ElementProperty::Uuid(_)
            | ElementProperty::Temporal(_)
            | ElementProperty::Vector(_) => {
                let candidate = TraverserValue::Computed(value.to_logical(), None);
                self.predicate.test(context, &candidate)
            }
            ElementProperty::Object(_) | ElementProperty::Vertex(_) | ElementProperty::Edge(_) => {
                let values: Vec<ElementProperty> = value
                    .map_entries()
                    .map(|nested| nested.values().map(ElementProperty::to_logical).collect())
                    .unwrap_or_default();
                for v in &values {
                    if self.matches_value(context, v)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            ElementProperty::Map(nested) => {
                let values: Vec<ElementProperty> =
                    nested.values().map(ElementProperty::to_logical).collect();
                for v in &values {
                    if self.matches_value(context, v)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
        }
    }

    fn test_value<S: GraphStorage>(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &TraverserValue<S>,
    ) -> TraverserResult<bool> {
        match source {
            TraverserValue::Vertex(v) => {
                let values: Option<Vec<ElementProperty>> = context
                    .storage
                    .vertex_properties(*v)?
                    .map(|vertex| vertex.values().map(ElementProperty::to_logical).collect());
                match values {
                    Some(values) => {
                        for value in &values {
                            if self.matches_value(context, value)? {
                                return Ok(true);
                            }
                        }
                        Ok(false)
                    }
                    None => Ok(false),
                }
            }

            TraverserValue::Computed(vertex, _) => self.matches_value(context, vertex),
            TraverserValue::PathProperty(prop) => self.matches_value(context, &prop.1),

            TraverserValue::VertexProperty(prop_name, node_index, _) => {
                let prop_name = prop_name.clone();
                let value = context
                    .storage
                    .vertex_properties(*node_index)?
                    .and_then(|vertex| vertex.get(prop_name.as_str()))
                    .cloned();
                match value {
                    Some(value) => self.matches_value(context, &value),
                    None => Err(Box::new(TraverserError::property_not_found_on_element(
                        prop_name.to_string(),
                        "vertex",
                    ))),
                }
            }

            TraverserValue::EdgeProperty(prop_name, edge_index, _) => {
                let prop_name = prop_name.clone();
                let value = context
                    .storage
                    .edge_properties(*edge_index)?
                    .and_then(|edge| edge.get(prop_name.as_str()))
                    .cloned();
                match value {
                    Some(value) => self.matches_value(context, &value),
                    None => Err(Box::new(TraverserError::property_not_found_on_element(
                        prop_name.to_string(),
                        "edge",
                    ))),
                }
            }

            TraverserValue::Edge(edge_index) => {
                let values: Option<Vec<ElementProperty>> = context
                    .storage
                    .edge_properties(*edge_index)?
                    .map(|edge| edge.values().map(ElementProperty::to_logical).collect());
                match values {
                    Some(values) => {
                        for value in &values {
                            if self.matches_value(context, value)? {
                                return Ok(true);
                            }
                        }
                        Ok(false)
                    }
                    None => Err(Box::new(TraverserError::cast_exception_str(
                        &["edge"],
                        "null".to_string(),
                    ))),
                }
            }

            TraverserValue::Object(obj) => {
                for r in obj.values() {
                    if self.test_value(context, r)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }

            TraverserValue::Record { values, .. } => {
                for r in values {
                    if self.test_value(context, r)? {
                        return Ok(true);
                    }
                }
                Ok(false)
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
            TraverserValue::Entry(_) => Err(Box::new(TraverserError::cast_exception_str(
                &[
                    "computed",
                    "vertexProperty",
                    "edgeProperty",
                    "vertex",
                    "edge",
                ],
                "entry".to_string(),
            ))),
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
}

impl<'g, S: GraphStorage + 'g, B: Into<PValue> + Debug + Clone + Send + Sync + 'static>
    GraphStepFilter<'g, S> for StepHasValueP<B>
{
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<bool> {
        self.test_value(context, &source.value)
    }

    fn clone_box(&self) -> Box<dyn GraphStepFilter<'g, S> + 'g> {
        Box::new(StepHasValueP {
            predicate: self.predicate.clone(),
        })
    }
}

/// `hasValue(Object value, Object... otherValues)` (D1): the multi-value OR overload of
/// `hasValue()`, verified against the vanilla `GraphTraversal` javadoc alongside the predicate
/// overload. It keeps every element (or bare property/computed value) with at least one property
/// whose value equals *any* of `values` — semantically `P.within(values)` applied the same way
/// [`StepHasValue`] applies a single literal, so it gets its own step (mirroring the
/// `has_label_single`/`has_label` and `has_id_single`/`has_id` arity split already used in this
/// codebase for "one value" vs. "array of values") rather than forcing every single-value call
/// through a always-allocate `Vec` of one element.
#[derive(Clone)]
struct StepHasValueAny {
    values: Vec<ElementProperty>,
}

impl fmt::Display for StepHasValueAny {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "has_value(")?;
        for (i, v) in self.values.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            f.write_str(&crate::value::literal::rhai_literal(v))?;
        }
        write!(f, ")")
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `hasValue(v1, v2, ...)`: matches when any property's value equals any of `values`.
    pub fn has_value_any<T: Into<ElementProperty>>(self, values: Vec<T>) -> Self {
        let step = StepHasValueAny {
            values: values.into_iter().map(Into::into).collect(),
        };
        self.push_step(GraphStep::Filter("has_value", Box::new(step)))
    }
}

impl __ {
    /// `hasValue(v1, v2, ...)`, the anonymous-traversal counterpart of
    /// [`GraphTraversalSource::has_value_any`].
    pub fn has_value_any<'g, S: GraphStorage + 'g, T: Into<ElementProperty>>(
        values: Vec<T>,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().has_value_any(values)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `hasValue(v1, v2, ...)`, the anonymous-traversal counterpart of
    /// [`GraphTraversalSource::has_value_any`].
    pub fn has_value_any<T: Into<ElementProperty>>(self, values: Vec<T>) -> Self {
        let step = GraphStep::Filter(
            "has_value",
            Box::new(StepHasValueAny {
                values: values.into_iter().map(Into::into).collect(),
            }),
        );
        self.push_step(step)
    }
}

impl StepHasValueAny {
    /// Same scalar-vs-nested split as [`StepHasValue::matches_scalar_or_nested`] (D2), checking
    /// membership in `self.values` instead of equality with a single literal.
    fn matches_scalar_or_nested(&self, value: &ElementProperty) -> bool {
        match value {
            ElementProperty::Null => false,
            ElementProperty::Array(_) => false,
            ElementProperty::String(_)
            | ElementProperty::Compressed(_)
            | ElementProperty::Deduped(_)
            | ElementProperty::Number(_)
            | ElementProperty::Boolean(_)
            | ElementProperty::Uuid(_)
            | ElementProperty::Temporal(_)
            | ElementProperty::Vector(_) => self.values.iter().any(|v| v.eq(value)),
            ElementProperty::Object(_) | ElementProperty::Vertex(_) | ElementProperty::Edge(_) => {
                value.map_entries().is_some_and(|nested| {
                    nested.values().any(|r| self.values.iter().any(|v| v.eq(r)))
                })
            }
            ElementProperty::Map(nested) => {
                nested.values().any(|r| self.values.iter().any(|v| v.eq(r)))
            }
        }
    }

    fn test_value<S: GraphStorage>(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &TraverserValue<S>,
    ) -> TraverserResult<bool> {
        match source {
            TraverserValue::Vertex(v) => {
                let vertex = context.storage.vertex_properties(*v)?;
                match vertex {
                    Some(vertex) => Ok(vertex.values().any(|r| self.matches_scalar_or_nested(r))),
                    None => Ok(false),
                }
            }

            TraverserValue::Computed(vertex, _) => Ok(self.matches_scalar_or_nested(vertex)),
            TraverserValue::PathProperty(prop) => Ok(self.matches_scalar_or_nested(&prop.1)),

            TraverserValue::VertexProperty(prop_name, node_index, _) => {
                let prop_name = prop_name.clone();
                let vertex = context.storage.vertex_properties(*node_index)?;
                match vertex {
                    Some(vertex) => {
                        let value = vertex.get(prop_name.as_str());
                        match value {
                            Some(value) => Ok(self.matches_scalar_or_nested(value)),
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
                            Some(value) => Ok(self.matches_scalar_or_nested(value)),
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
                        Ok(edge_map.values().any(|r| self.matches_scalar_or_nested(r)))
                    }
                    None => Err(Box::new(TraverserError::cast_exception_str(
                        &["edge"],
                        "null".to_string(),
                    ))),
                }
            }

            TraverserValue::Object(obj) => {
                for r in obj.values() {
                    let x = self.test_value(context, r)?;
                    if x {
                        return Ok(true);
                    }
                }
                Ok(false)
            }

            TraverserValue::Record { values, .. } => {
                for r in values {
                    let x = self.test_value(context, r)?;
                    if x {
                        return Ok(true);
                    }
                }
                Ok(false)
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
            TraverserValue::Entry(_) => Err(Box::new(TraverserError::cast_exception_str(
                &[
                    "computed",
                    "vertexProperty",
                    "edgeProperty",
                    "vertex",
                    "edge",
                ],
                "entry".to_string(),
            ))),
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
}

impl<'g, S: GraphStorage + 'g> GraphStepFilter<'g, S> for StepHasValueAny {
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<bool> {
        self.test_value(context, &source.value)
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
            rust: |g| g.v("1").has_value(29).count().to_list(),
            rhai: [
                r#"g.V("1").hasValue(29).count()"#,
                r#"g.v("1").has_value(29).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[test]
    fn v_bool() {
        let graph = GraphSource::tinkerpop_modern();
        graph
            .write()
            .traversal_mut()
            .v("1")
            .property("deleted", true)
            .next()
            .unwrap();

        test_dual!(
            graph: graph,
            rust: |g| g.v("1").has_value(true).count().to_list(),
            rhai: [
                r#"g.V("1").hasValue(true).count()"#,
                r#"g.v("1").has_value(true).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[test]
    fn where_() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").where_t(__::has_value(29)).count().to_list(),
            rhai: [
                r#"g.V("1").where(__.hasValue(29)).count()"#,
                r#"g.v("1").where(__.has_value(29)).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[test]
    fn where_inner() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").where_t(__::element().has_value(29)).count().to_list(),
            rhai: [
                r#"g.V("1").where(__.element().hasValue(29)).count()"#,
                r#"g.v("1").where(__.element().has_value(29)).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[test]
    fn v_str() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").has_value("marko").count().to_list(),
            rhai: [
                r#"g.V("1").hasValue("marko").count()"#,
                r#"g.v("1").has_value("marko").count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[test]
    fn v_obj() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).group().by(By::Label).by(By::Count).has_value(2).count().to_list(),
            rhai: [
                r#"g.V().group().by(By::Label).by(By::Count).hasValue(2).count()"#,
                r#"g.v().group().by(By::Label).by(By::Count).has_value(2).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[test]
    fn v_obj_wrong() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).group().by(By::Label).by(By::Count).has_value(20).count().to_list(),
            rhai: [
                r#"g.V().group().by(By::Label).by(By::Count).hasValue(20).count()"#,
                r#"g.v().group().by(By::Label).by(By::Count).has_value(20).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
    }

    #[test]
    fn v_wrong() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").has_value(290).count().to_list(),
            rhai: [
                r#"g.V("1").hasValue(290).count()"#,
                r#"g.v("1").has_value(290).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
    }

    #[test]
    fn e() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).has_label("knows").has_value(0.5).count().to_list(),
            rhai: [
                r#"g.E().hasLabel("knows").hasValue(0.5).count()"#,
                r#"g.e().has_label("knows").has_value(0.5).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v(None).has_value("java").profile().unwrap();
        assert!(result.to_string().contains("has_value("));
    }

    // -- D1: `hasValue(v1, v2, ...)`, the multi-value OR overload --------------------------

    #[test]
    fn multi_value_any() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_value_any(vec![29, 27]).count().to_list(),
            rhai: [
                r#"g.V().hasValue([29, 27]).count()"#,
                r#"g.v().has_value_any([29, 27]).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
    }

    #[test]
    fn multi_value_any_none_match() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_value_any(vec![999, 998]).count().to_list(),
            rhai: [r#"g.V().hasValue([999, 998]).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
    }

    // -- D1: `hasValue(P<V>)`, the predicate overload ---------------------------------------

    #[test]
    fn predicate_number() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_value_p(P::Lt(30)).count().to_list(),
            rhai: [
                r#"g.V().hasValue(P.lt(30)).count()"#,
                r#"g.v().has_value_p(P::Lt(30)).count()"#,
            ],
            // marko(29), vadas(27)
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
    }

    #[test]
    fn predicate_string() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).has_value_p(P::Eq("marko")).count().to_list(),
            rhai: [
                r#"g.V().hasValue(P.eq("marko")).count()"#,
                r#"g.v().has_value_p(P::Eq("marko")).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[test]
    fn predicate_boolean() {
        let graph = GraphSource::tinkerpop_modern();
        graph
            .write()
            .traversal_mut()
            .v("1")
            .property("deleted", true)
            .next()
            .unwrap();

        test_dual!(
            graph: graph,
            rust: |g| g.v(None).has_value_p(P::Eq(true)).count().to_list(),
            rhai: [
                r#"g.V().hasValue(P.eq(true)).count()"#,
                r#"g.v().has_value_p(P::Eq(true)).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[test]
    fn predicate_where_inner() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").where_t(__::has_value_p(P::Lt(30))).count().to_list(),
            rhai: [
                r#"g.V("1").where(__.hasValue(P.lt(30))).count()"#,
                r#"g.v("1").where(__.has_value_p(P::Lt(30))).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[test]
    fn predicate_profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v(None).has_value_p(P::Lt(30)).profile().unwrap();
        assert!(result.to_string().contains("has_value("));
    }

    // -- D2: the scalar-comparison bug, exercised through `properties()` -------------------

    /// The already-broken case (D2): a bare scalar `VertexProperty` traverser used to throw a
    /// misleading "expected object, got float64"-shaped `CastException` instead of comparing.
    /// Same expectation as `v` above (marko's `age` is the only `29`), but reached through
    /// `properties()` so the traverser is actually `VertexProperty`, not `Vertex`.
    #[test]
    fn scalar_property_bug_fix() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).properties(None).has_value(29).count().to_list(),
            rhai: [
                r#"g.V().properties().hasValue(29).count()"#,
                r#"g.v().properties().has_value(29).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    /// Same bug, on an `EdgeProperty` traverser and a predicate instead of a literal.
    #[test]
    fn scalar_edge_property_predicate_bug_fix() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).properties(None).has_value_p(P::Lt(0.3)).count().to_list(),
            rhai: [
                r#"g.E().properties().hasValue(P.lt(0.3)).count()"#,
                r#"g.e().properties().has_value_p(P::Lt(0.3)).count()"#,
            ],
            // only the "created" edge marko->lop has weight 0.2 < 0.3
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    /// The motivating query: before this fix, `hasValue(P...)`
    /// didn't exist at all, and `properties()`-produced traversers hit the D2 `CastException` bug
    /// on top of that, forcing the `.value().is(P.lt(0.3))` workaround documented in
    /// `step_value.rs`'s `full_chain_bothe_properties_dedup_haskey_value` test. This is the exact
    /// literal query, with no workaround.
    #[test]
    fn roadmap_bothe_properties_dedup_haskey_hasvalue_predicate() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .both_e(None)
                .properties(None)
                .dedup()
                .has_key("weight")
                .has_value_p(P::Lt(0.3))
                .value()
                .to_list(),
            rhai: [
                r#"g.V().bothE().properties().dedup().hasKey("weight").hasValue(P.lt(0.3)).value()"#,
                r#"g.v().both_e().properties().dedup().has_key("weight").has_value_p(P::Lt(0.3)).value()"#,
            ],
            assert: |res| {
                let mut weights: Vec<f64> = res.iter().filter_map(|v| v.as_f64()).collect();
                weights.sort_by(|a, b| a.partial_cmp(b).unwrap());
                assert_eq!(weights, vec![0.2]);
            },
        );
    }

    use crate::traversal::test_utils::nested_doc_graph;

    #[test]
    fn object_valued_properties_match_any_nested_value() {
        test_dual!(
            graph: nested_doc_graph(),
            rust: |g| g.v(None).has_label("doc").properties("meta").has_value("x").to_list(),
            rhai: [
                r#"g.V().hasLabel("doc").properties("meta").hasValue("x")"#,
                r#"g.v().has_label("doc").properties("meta").has_value("x")"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 1);
                assert_eq!(res[0].as_object().and_then(|o| o.get("a")), Some(&1.into()));
            },
        );
        test_dual!(
            graph: nested_doc_graph(),
            rust: |g| g.e(None).properties("info").has_value(2).count().to_list(),
            rhai: [r#"g.E().properties("info").hasValue(2).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
        test_dual!(
            graph: nested_doc_graph(),
            rust: |g| g
                .v(None)
                .has_label("doc")
                .properties("meta")
                .has_value_p(P::Eq("y"))
                .count()
                .to_list(),
            rhai: [r#"g.V().hasLabel("doc").properties("meta").hasValue(P.eq("y")).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
        test_dual!(
            graph: nested_doc_graph(),
            rust: |g| g.e(None).properties("info").has_value_p(P::Gt(1)).count().to_list(),
            rhai: [r#"g.E().properties("info").hasValue(P.gt(1)).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
        // an edge matches when any of its own property values matches
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).has_value_p(P::Lt(0.3)).count().to_list(),
            rhai: [r#"g.E().hasValue(P.lt(0.3)).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e(None).has_value(0.4).count().to_list(),
            rhai: [r#"g.E().hasValue(0.4).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
    }

    #[test]
    fn maps_and_path_properties_match_by_their_values() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).project(vec!["n"]).by("name").has_value("marko").count().to_list(),
            rhai: [r#"g.V().project("n").by("name").hasValue("marko").count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .project(vec!["n"])
                .by("name")
                .has_value_p(P::Eq("marko"))
                .count()
                .to_list(),
            rhai: [r#"g.V().project("n").by("name").hasValue(P.eq("marko")).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).element_map(None).has_value("lop").count().to_list(),
            rhai: [r#"g.V().elementMap().hasValue("lop").count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .element_map(None)
                .has_value_p(P::Eq("lop"))
                .count()
                .to_list(),
            rhai: [r#"g.V().elementMap().hasValue(P.eq("lop")).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v("1")
                .properties("name".parse::<JsonPath>().unwrap())
                .has_value("marko")
                .count()
                .to_list(),
            rhai: [r#"g.V("1").properties(jpath("name")).hasValue("marko").count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v("1")
                .properties("name".parse::<JsonPath>().unwrap())
                .has_value_p(P::Neq("marko"))
                .count()
                .to_list(),
            rhai: [r#"g.V("1").properties(jpath("name")).hasValue(P.neq("marko")).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
    }

    #[test]
    fn null_and_list_values_never_match() {
        let graph = GraphSource::tinkerpop_modern();
        graph
            .write()
            .traversal_mut()
            .v("1")
            .property("tags", vec![1, 2])
            .property("gone", ElementProperty::Null)
            .next()
            .unwrap();
        test_dual!(
            graph: graph,
            rust: |g| g.v("1").properties(vec!["tags", "gone"]).has_value(1).count().to_list(),
            rhai: [r#"g.V("1").properties("tags", "gone").hasValue(1).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(0)),
        );
    }

    #[test]
    fn non_element_inputs_are_cast_errors() {
        for (rhai, ty) in [
            (r#"g.V("1").path().hasValue(1)"#, "path"),
            (r#"g.V("1").fold().hasValue(1)"#, "array"),
            (r#"g.V("1").valueMap().unfold().hasValue(1)"#, "entry"),
        ] {
            let rust: fn(
                GraphTraversalSource<'_, &TraversalGraph>,
            ) -> TraverserResult<Vec<ElementProperty>> = match ty {
                "path" => |g| g.v("1").path().has_value(1).to_list(),
                "array" => |g| g.v("1").fold().has_value(1).to_list(),
                _ => |g| g.v("1").value_map(None).unfold().has_value(1).to_list(),
            };
            test_dual_err!(
                graph: GraphSource::tinkerpop_modern(),
                rust: rust,
                rhai: [rhai],
                assert: |err| assert!(err.contains(&format!("got type '{ty}'")), "{err}"),
            );
        }
        for (rhai, ty) in [
            (r#"g.V("1").path().hasValue(P.eq(1))"#, "path"),
            (r#"g.V("1").fold().hasValue(P.eq(1))"#, "array"),
            (r#"g.V("1").valueMap().unfold().hasValue(P.eq(1))"#, "entry"),
        ] {
            let rust: fn(
                GraphTraversalSource<'_, &TraversalGraph>,
            ) -> TraverserResult<Vec<ElementProperty>> = match ty {
                "path" => |g| g.v("1").path().has_value_p(P::Eq(1)).to_list(),
                "array" => |g| g.v("1").fold().has_value_p(P::Eq(1)).to_list(),
                _ => |g| {
                    g.v("1")
                        .value_map(None)
                        .unfold()
                        .has_value_p(P::Eq(1))
                        .to_list()
                },
            };
            test_dual_err!(
                graph: GraphSource::tinkerpop_modern(),
                rust: rust,
                rhai: [rhai],
                assert: |err| assert!(err.contains(&format!("got type '{ty}'")), "{err}"),
            );
        }
    }
}
