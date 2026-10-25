use crate::graph::GraphStorage;
use crate::*;

/// The value written by `property()`: either a constant computed at pipeline-construction time,
/// or a sub-traversal evaluated once per traverser through the `By::Traversal` machinery.
enum PropertyValue<'g, S: GraphStorage + 'g> {
    Constant(ElementProperty),
    Traversal(AnonymousTraversal<'g, S>),
}

impl<'g, S: GraphStorage + 'g> Clone for PropertyValue<'g, S> {
    fn clone(&self) -> Self {
        match self {
            PropertyValue::Constant(value) => PropertyValue::Constant(value.clone()),
            PropertyValue::Traversal(traversal) => PropertyValue::Traversal(traversal.clone()),
        }
    }
}

/// The graph element `property()` will write to, resolved once up front so the write-target
/// check happens before a sub-traversal is evaluated for nothing.
enum PropertyTarget<S: GraphStorage> {
    Vertex(S::VertexId),
    Edge(S::EdgeId),
}

struct StepProperty<'g, S: GraphStorage + 'g> {
    /// A plain name keeps the exact old behaviour; a path writes below the stored property
    /// (`property(jpath(..), v)`).
    property: PropKey,
    value: PropertyValue<'g, S>,
    /// The `Cardinality` argument of `property(Cardinality, key, value)`; `None` for the plain
    /// two-argument form.
    cardinality: Option<Cardinality>,
    /// `property(traversal, value)`: the key is the first result of this traversal, run on the
    /// incoming traverser, instead of `property` (which is then unused).
    key: Option<AnonymousTraversal<'g, S>>,
}

impl<'g, S: GraphStorage + 'g> Clone for StepProperty<'g, S> {
    fn clone(&self) -> Self {
        Self {
            property: self.property.clone(),
            value: self.value.clone(),
            cardinality: self.cardinality,
            key: self.key.clone(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepProperty<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let key = match (&self.key, &self.property) {
            (Some(traversal), _) => format!("{traversal}"),
            (None, key) => crate::value::literal::rhai_prop_key(key),
        };
        let value = match &self.value {
            PropertyValue::Constant(value) => crate::value::literal::rhai_literal(value),
            PropertyValue::Traversal(traversal) => traversal.to_string(),
        };
        match self.cardinality {
            Some(cardinality) => write!(f, "property(Cardinality.{cardinality}, {key}, {value})"),
            None => write!(f, "property({key}, {value})"),
        }
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Sets a property. A plain name sets the property on the element; a [`JsonPath`] writes
    /// *below* the stored property named by its first segment: missing object keys are created,
    /// `[n]` replaces an array element, `[len]` appends (last segment only), and a type mismatch
    /// or an out-of-range index fails with `GraphError::InvalidPropertyPath` without changing
    /// anything. An index segment never creates a missing array.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let mut graph = TraversalGraph::new();
    /// let path: JsonPath = "meta.tags".parse().unwrap();
    /// graph.traversal_mut().add_v("p").property(path, ElementProperty::Array(vec!["a".into()])).next().unwrap();
    /// let append: JsonPath = "meta.tags[1]".parse().unwrap();
    /// graph.traversal_mut().v(None).property(append.clone(), "b").next().unwrap();
    /// assert_eq!(graph.traversal().v(None).values(append).next().unwrap(), Some("b".into()));
    /// ```
    pub fn property<T: Into<ElementProperty>>(
        self,
        property: impl Into<PropKey>,
        value: T,
    ) -> Self {
        self.push_step(GraphStep::SideEffectMut(
            "property",
            Box::new(StepProperty {
                property: property.into(),
                value: PropertyValue::Constant(value.into()),
                cardinality: None,
                key: None,
            }),
        ))
    }

    /// Sets a property from a per-traverser sub-traversal instead of a constant. Vanilla
    /// semantics for a nested traversal used as an argument: only its first result is used; an
    /// empty result leaves the property untouched (see [`AnonymousTraversal`]).
    pub fn property_by(
        self,
        property: impl Into<PropKey>,
        value: AnonymousTraversal<'g, S>,
    ) -> Self {
        self.push_step(GraphStep::SideEffectMut(
            "property",
            Box::new(StepProperty {
                property: property.into(),
                value: PropertyValue::Traversal(value),
                cardinality: None,
                key: None,
            }),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Sets a property on the incoming element (Cardinality.single only); a null value is stored as
    /// null, use remove_property() to remove.
    pub fn property<T: Into<ElementProperty>>(
        self,
        property: impl Into<PropKey>,
        value: T,
    ) -> Self {
        let step = GraphStep::SideEffectMut(
            "property",
            Box::new(StepProperty {
                property: property.into(),
                value: PropertyValue::Constant(value.into()),
                cardinality: None,
                key: None,
            }),
        );
        self.push_step(step)
    }

    /// Sets a property on the incoming element (Cardinality.single only); a null value is stored as
    /// null, use remove_property() to remove.
    pub fn property_by(
        self,
        property: impl Into<PropKey>,
        value: AnonymousTraversal<'g, S>,
    ) -> Self {
        let step = GraphStep::SideEffectMut(
            "property",
            Box::new(StepProperty {
                property: property.into(),
                value: PropertyValue::Traversal(value),
                cardinality: None,
                key: None,
            }),
        );
        self.push_step(step)
    }
}

/// Builds a `property(traversal, value)` step: the key comes from `key`.
fn key_traversal_step<'g, S: GraphStorage + 'g>(
    key: AnonymousTraversal<'g, S>,
    value: PropertyValue<'g, S>,
) -> GraphStep<'g, S> {
    GraphStep::SideEffectMut(
        "property",
        Box::new(StepProperty {
            property: PropKey::Name(CompactString::default()),
            value,
            cardinality: None,
            key: Some(key),
        }),
    )
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `property(traversal, value)`: sets a property whose key is the first result of `key`,
    /// run on the incoming traverser (TinkerPop's `AddPropertyStep` with a key traversal); it
    /// must be a string, no result or another value is a cast error.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let mut graph = TraversalGraph::tinkerpop_modern();
    /// graph.traversal_mut().v("1").property_key_by(__::constant("nick"), "mk").next().unwrap();
    /// assert_eq!(graph.traversal().v("1").values("nick").next().unwrap(), Some("mk".into()));
    /// ```
    pub fn property_key_by<T: Into<ElementProperty>>(
        self,
        key: AnonymousTraversal<'g, S>,
        value: T,
    ) -> Self {
        self.push_step(key_traversal_step(
            key,
            PropertyValue::Constant(value.into()),
        ))
    }

    /// `property(traversal, traversal)`: [`Self::property_key_by`] with the value taken from
    /// the first result of `value` (no result leaves the property untouched, as in
    /// [`Self::property_by`]).
    pub fn property_key_by_traversal(
        self,
        key: AnonymousTraversal<'g, S>,
        value: AnonymousTraversal<'g, S>,
    ) -> Self {
        self.push_step(key_traversal_step(key, PropertyValue::Traversal(value)))
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `property(traversal, value)` in a child traversal; see
    /// [`GraphTraversalSource::property_key_by`].
    pub fn property_key_by<T: Into<ElementProperty>>(
        self,
        key: AnonymousTraversal<'g, S>,
        value: T,
    ) -> Self {
        self.push_step(key_traversal_step(
            key,
            PropertyValue::Constant(value.into()),
        ))
    }

    /// `property(traversal, traversal)` in a child traversal; see
    /// [`GraphTraversalSource::property_key_by_traversal`].
    pub fn property_key_by_traversal(
        self,
        key: AnonymousTraversal<'g, S>,
        value: AnonymousTraversal<'g, S>,
    ) -> Self {
        self.push_step(key_traversal_step(key, PropertyValue::Traversal(value)))
    }
}

/// Builds a `property(Cardinality, key, value)` step.
fn cardinality_step<'g, S: GraphStorage + 'g>(
    cardinality: Cardinality,
    property: impl Into<PropKey>,
    value: PropertyValue<'g, S>,
) -> GraphStep<'g, S> {
    GraphStep::SideEffectMut(
        "property",
        Box::new(StepProperty {
            property: property.into(),
            value,
            cardinality: Some(cardinality),
            key: None,
        }),
    )
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `property(Cardinality, key, value)`: sets a vertex property with an explicit
    /// cardinality. `Cardinality::Single` is the same as [`Self::property`] (replace).
    /// `Cardinality::List`/`Cardinality::Set` fail with
    /// [`TraverserError::UnsupportedCardinality`] when a value would be written, since
    /// Graphersal stores one value per property key; on an edge every cardinality fails
    /// (edge properties have none).
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let mut graph = TraversalGraph::tinkerpop_modern();
    /// graph.traversal_mut().v("1").property_with(Cardinality::Single, "age", 30).next().unwrap();
    /// let age = graph.traversal().v("1").values("age").next().unwrap();
    /// assert_eq!(age, Some(ElementProperty::from(30)));
    /// ```
    pub fn property_with<T: Into<ElementProperty>>(
        self,
        cardinality: Cardinality,
        property: impl Into<PropKey>,
        value: T,
    ) -> Self {
        self.push_step(cardinality_step(
            cardinality,
            property,
            PropertyValue::Constant(value.into()),
        ))
    }

    /// `property(Cardinality, key, traversal)`: [`Self::property_with`] with the value taken
    /// from the first result of a per-traverser sub-traversal, like [`Self::property_by`].
    pub fn property_by_with(
        self,
        cardinality: Cardinality,
        property: impl Into<PropKey>,
        value: AnonymousTraversal<'g, S>,
    ) -> Self {
        self.push_step(cardinality_step(
            cardinality,
            property,
            PropertyValue::Traversal(value),
        ))
    }

    /// `property(T.id, v)`: sets the id of the element the preceding `add_v()`/`add_e()` creates
    /// (only other `property()` steps may come in between). A string is used as-is, an integer
    /// becomes its decimal string. Anywhere else the traversal fails with
    /// [`TraverserError::TokenPropertyOutsideAdd`] when it executes.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let mut graph = TraversalGraph::new();
    /// graph.traversal_mut().add_v("person").property("name", "mike").property_id("p1").next().unwrap();
    /// assert_eq!(graph.traversal().v("p1").count().next().unwrap(), Some(ElementProperty::from(1)));
    /// ```
    pub fn property_id<T: Into<ElementProperty>>(mut self, value: T) -> Self {
        attach_token(&mut self.steps, AddToken::Id, value.into());
        self
    }

    /// `property(T.label, v)`: replaces the label of the vertex the preceding `add_v()` creates
    /// (only other `property()` steps may come in between). After `add_e()` or anywhere else the
    /// traversal fails with [`TraverserError::TokenPropertyOutsideAdd`] when it executes.
    pub fn property_label<T: Into<ElementProperty>>(mut self, label: T) -> Self {
        attach_token(&mut self.steps, AddToken::Label, label.into());
        self
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `property(Cardinality, key, value)`; see [`GraphTraversalSource::property_with`].
    pub fn property_with<T: Into<ElementProperty>>(
        self,
        cardinality: Cardinality,
        property: impl Into<PropKey>,
        value: T,
    ) -> Self {
        self.push_step(cardinality_step(
            cardinality,
            property,
            PropertyValue::Constant(value.into()),
        ))
    }

    /// `property(Cardinality, key, traversal)`; see [`GraphTraversalSource::property_by_with`].
    pub fn property_by_with(
        self,
        cardinality: Cardinality,
        property: impl Into<PropKey>,
        value: AnonymousTraversal<'g, S>,
    ) -> Self {
        self.push_step(cardinality_step(
            cardinality,
            property,
            PropertyValue::Traversal(value),
        ))
    }

    /// `property(T.id, v)`; see [`GraphTraversalSource::property_id`].
    pub fn property_id<T: Into<ElementProperty>>(mut self, value: T) -> Self {
        attach_token(&mut self.steps, AddToken::Id, value.into());
        self
    }

    /// `property(T.label, v)`; see [`GraphTraversalSource::property_label`].
    pub fn property_label<T: Into<ElementProperty>>(mut self, label: T) -> Self {
        attach_token(&mut self.steps, AddToken::Label, label.into());
        self
    }
}

/// Which token `property(T.x, v)` names.
#[derive(Clone, Copy)]
enum AddToken {
    Id,
    Label,
}

impl AddToken {
    fn spelling(self) -> &'static str {
        match self {
            AddToken::Id => "T.id",
            AddToken::Label => "T.label",
        }
    }
}

/// Folds `property(T.id|T.label, value)` into the `add_v()`/`add_e()` it follows, walking back
/// over `property()` steps, modulators (`from()`/`to()`) and `as()` labels. Otherwise (no add
/// step, an invalid value, or `T.label` after `add_e()`) pushes a [`StepTokenPropertyRejected`]
/// that fails when the traversal executes, since builders cannot fail.
fn attach_token<'g, S: GraphStorage + 'g>(
    steps: &mut Vec<GraphStep<'g, S>>,
    token: AddToken,
    value: ElementProperty,
) {
    let rendered = format!(
        "property({}, {})",
        token.spelling(),
        crate::value::literal::rhai_literal(&value)
    );
    let converted = match (token, &value) {
        (_, ElementProperty::String(text)) if !text.is_empty() => Ok(text.clone()),
        (AddToken::Id, ElementProperty::Number(number)) => match number.as_i64() {
            Some(int) => Ok(compact_str::format_compact!("{int}")),
            None => Err(TokenRejection::Value(value.clone())),
        },
        _ => Err(TokenRejection::Value(value.clone())),
    };
    let rejection = match converted {
        Err(rejection) => rejection,
        Ok(text) => {
            let target = steps.iter_mut().rev().find_map(|step| match step {
                GraphStep::Modulator(_, _) | GraphStep::As(_, _) => None,
                GraphStep::SideEffectMut("property", _) => None,
                GraphStep::SideEffectMut(_, step) => Some(step.as_add_element_mut()),
                _ => Some(None),
            });
            match target.flatten() {
                Some(add) => match token {
                    AddToken::Id => {
                        add.set_id(text);
                        return;
                    }
                    AddToken::Label => {
                        if add.set_label(text) {
                            return;
                        }
                        TokenRejection::OutsideAdd
                    }
                },
                None => TokenRejection::OutsideAdd,
            }
        }
    };
    steps.push(GraphStep::SideEffectMut(
        "property",
        Box::new(StepTokenPropertyRejected {
            rendered,
            token,
            rejection,
        }),
    ));
}

/// Why a `property(T.id|T.label, v)` could not be folded into an add step.
#[derive(Clone)]
enum TokenRejection {
    /// No `add_v()`/`add_e()` to fold into (or `T.label` after `add_e()`).
    OutsideAdd,
    /// The value is not a usable id (string or integer) / label (non-empty string).
    Value(ElementProperty),
}

/// A `property(T.id|T.label, v)` that could not fold into an add step: kept in the plan so the
/// profile shows it, and fails when a traverser reaches it (the `StepOptionWithoutBranch`
/// pattern).
#[derive(Clone)]
struct StepTokenPropertyRejected {
    rendered: String,
    token: AddToken,
    rejection: TokenRejection,
}

impl fmt::Display for StepTokenPropertyRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.rendered)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepSideEffectMut<'g, S> for StepTokenPropertyRejected {
    fn process(
        &self,
        _context: &mut TraverserContext<'_, S>,
        _source: Option<Traverser<S>>,
    ) -> TraverserResult<Vec<Traverser<S>>> {
        match &self.rejection {
            TokenRejection::OutsideAdd => Err(Box::new(TraverserError::TokenPropertyOutsideAdd {
                token: self.token.spelling().to_string(),
            })),
            TokenRejection::Value(value) => {
                let expected: &[&'static str] = match self.token {
                    AddToken::Id => &["string", "integer"],
                    AddToken::Label => &["non-empty string"],
                };
                let actual = crate::graph::type_of(value)
                    .map(|ty| ty.to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                Err(Box::new(TraverserError::cast_exception_str(
                    expected, actual,
                )))
            }
        }
    }

    fn extends_path(&self) -> bool {
        false
    }

    fn clone_box(&self) -> Box<dyn GraphStepSideEffectMut<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

/// The write target of a property step, checked against the requested cardinality before any
/// value is written.
pub(crate) fn check_cardinality(
    step: &str,
    cardinality: Option<Cardinality>,
    on_edge: bool,
) -> TraverserResult<()> {
    match cardinality {
        None => Ok(()),
        Some(cardinality) if on_edge => Err(Box::new(TraverserError::UnsupportedCardinality {
            step: step.to_string(),
            cardinality,
            on_edge: true,
        })),
        Some(Cardinality::Single) => Ok(()),
        Some(cardinality @ (Cardinality::List | Cardinality::Set)) => {
            Err(Box::new(TraverserError::UnsupportedCardinality {
                step: step.to_string(),
                cardinality,
                on_edge: false,
            }))
        }
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepSideEffectMut<'g, S> for StepProperty<'g, S> {
    fn property_access(
        &self,
        visit: &mut dyn FnMut(Option<&str>, crate::graph::workload::AccessKind),
    ) {
        // `property(traversal, value)`: the key is known only when it runs.
        if self.key.is_none()
            && let Some(key) = self.property.top_level()
        {
            visit(Some(key), crate::graph::workload::AccessKind::Write);
        }
    }
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Option<Traverser<S>>,
    ) -> TraverserResult<Vec<Traverser<S>>> {
        // No incoming traverser (an empty stream): nothing to write to, nothing to emit.
        let Some(source) = source else {
            return Ok(Vec::new());
        };

        // Fail fast on a non-writable element before evaluating a sub-traversal for nothing.
        let target: PropertyTarget<S> = match &source.value {
            TraverserValue::Vertex(node_index) => PropertyTarget::Vertex(*node_index),
            TraverserValue::Edge(edge_index) => PropertyTarget::Edge(*edge_index),
            t => {
                return Err(Box::new(TraverserError::cast_exception(
                    &["vertex", "edge"],
                    t,
                )));
            }
        };
        // Edge properties take no cardinality at all: fail before evaluating the value.
        if matches!(target, PropertyTarget::Edge(_)) {
            check_cardinality("property", self.cardinality, true)?;
        }

        // `property(traversal, value)`: the key is the traversal's first result, a string.
        let resolved_key;
        let property: &PropKey = match &self.key {
            None => &self.property,
            Some(key) => {
                let found = key.first(context, &source)?;
                match found.map(|t| t.value.into_node_property(context.storage)) {
                    Some(Ok(ElementProperty::String(name))) => {
                        resolved_key = PropKey::Name(name);
                        &resolved_key
                    }
                    Some(Ok(other)) => {
                        return Err(Box::new(TraverserError::cast_exception_str(
                            &["string"],
                            crate::graph::schema::value_type_name(&other).to_string(),
                        )));
                    }
                    Some(Err(err)) => return Err(err),
                    None => {
                        return Err(Box::new(TraverserError::cast_exception_str(
                            &["string"],
                            "null".to_string(),
                        )));
                    }
                }
            }
        };

        let value = match &self.value {
            PropertyValue::Constant(value) => Some(value.clone()),
            PropertyValue::Traversal(traversal) => {
                let results = traversal.apply_to(context, &source)?;
                match results.into_iter().next() {
                    Some(first) => Some(first.value.into_node_property(context.storage)?),
                    None => None,
                }
            }
        };

        let value = match value {
            Some(value) => value,
            // An empty sub-traversal result leaves the property untouched: writing `Null` would
            // turn a failed conversion into data loss.
            None => return Ok(vec![source]),
        };
        // A cardinality never combines with a path key (v1): checked once a value would be written.
        if let (Some(_), PropKey::Path(path)) = (self.cardinality, property) {
            return Err(Box::new(TraverserError::Graph(
                GraphError::InvalidPropertyPath {
                    path: path.to_string(),
                    fault: PathWriteFault::CardinalityWithPath,
                },
            )));
        }
        // A value is about to be written: list/set cannot be honoured.
        check_cardinality("property", self.cardinality, false)?;
        // A stored value is read back recursively: bound its nesting before it is written.
        context.check_written_depth(&self.property, &value)?;

        match (target, property) {
            (PropertyTarget::Vertex(node_index), PropKey::Name(name)) => {
                context
                    .storage
                    .set_vertex_property(node_index, name.clone(), value)?;
            }
            (PropertyTarget::Edge(edge_index), PropKey::Name(name)) => {
                context
                    .storage
                    .set_edge_property(edge_index, name.clone(), value)?;
            }
            (PropertyTarget::Vertex(node_index), PropKey::Path(path)) => {
                context
                    .storage
                    .set_vertex_property_path(node_index, path, value)?;
            }
            (PropertyTarget::Edge(edge_index), PropKey::Path(path)) => {
                context
                    .storage
                    .set_edge_property_path(edge_index, path, value)?;
            }
        }
        Ok(vec![source])
    }

    fn constant_property(&self) -> Option<(&CompactString, &ElementProperty)> {
        if self.key.is_some() {
            return None;
        }
        match (&self.property, &self.value, self.cardinality) {
            (PropKey::Name(name), PropertyValue::Constant(value), None) => Some((name, value)),
            _ => None,
        }
    }

    /// The key traversal first, then the value traversal (own arguments in argument order).
    fn child_traversals_mut(&mut self) -> Vec<&mut crate::traversal::AnonymousTraversal<'g, S>> {
        let mut children = Vec::new();
        if let Some(key) = &mut self.key {
            children.push(key);
        }
        if let PropertyValue::Traversal(traversal) = &mut self.value {
            children.push(traversal);
        }
        children
    }

    fn extends_path(&self) -> bool {
        false
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &crate::traversal::optimizer::path_analysis::PathAnalysis,
        _downstream: &crate::traversal::optimizer::path_analysis::PathRequirement,
    ) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        // The key and value traversals run on the incoming traverser; only their first result
        // is used.
        self.child_traversals_mut().into_iter().fold(
            crate::traversal::optimizer::path_analysis::PathRequirement::None,
            |need, child| {
                need.union(child.analyze_paths(
                    analysis,
                    crate::traversal::optimizer::path_analysis::PathRequirement::None,
                ))
            },
        )
    }

    fn clone_box(&self) -> Box<dyn GraphStepSideEffectMut<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple() {
        let mut graph = TraversalGraph::tinkerpop_modern();
        let result = graph.traversal().v("1").values("color").next().unwrap();
        assert_eq!(result, None);

        let result = graph
            .traversal_mut()
            .v("1")
            .property("color", "red")
            .values("color")
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(result, ElementProperty::String("red".into()));
    }

    #[test]
    fn profile() {
        let mut graph = TraversalGraph::tinkerpop_modern();
        let result = graph
            .traversal_mut()
            .v("1")
            .property("color", "red")
            .values("color")
            .profile()
            .unwrap();
        assert!(result.to_string().contains("property("));
    }

    #[test]
    fn traversal_value_normalizes_mixed_types() {
        let mut graph = TraversalGraph::tinkerpop_modern();
        graph
            .traversal_mut()
            .v("4")
            .property("age", "999")
            .next()
            .unwrap();

        graph
            .traversal_mut()
            .v(None)
            .has_label("person")
            .property_by("age", __::values("age").as_number(GType::LONG))
            .next()
            .unwrap();

        let traversers = graph
            .traversal()
            .v(None)
            .has_label("person")
            .infer_schema_step()
            .internal_to_list()
            .unwrap();
        let schema = traversers[0].as_schema().expect("schema value");
        assert_eq!(
            schema.vertices["person"].schema.properties["age"].type_name(),
            "int64"
        );
    }

    #[test]
    fn traversal_value_takes_only_first_result() {
        let mut graph = TraversalGraph::tinkerpop_modern();

        let ages = graph
            .traversal()
            .v("1")
            .out("knows")
            .values("age")
            .to_list()
            .unwrap();
        assert!(
            ages.len() > 1,
            "expected v(\"1\").out(\"knows\") to reach more than one vertex"
        );

        // Vanilla semantics: a nested traversal used as a value only yields its first result.
        graph
            .traversal_mut()
            .v("1")
            .property_by("first_known_age", __::out("knows").values("age"))
            .next()
            .unwrap();
        let stored = graph
            .traversal()
            .v("1")
            .values("first_known_age")
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(stored, ages[0]);

        // Producing a list requires an explicit `.fold()`.
        graph
            .traversal_mut()
            .v("1")
            .property_by("all_known_ages", __::out("knows").values("age").fold())
            .next()
            .unwrap();
        let stored_all = graph
            .traversal()
            .v("1")
            .values("all_known_ages")
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(stored_all, ElementProperty::Array(ages));
    }

    #[test]
    fn traversal_value_profile_shows_nested_traversal_and_is_not_reordered() {
        let mut graph = TraversalGraph::tinkerpop_modern();
        let result = graph
            .traversal_mut()
            .v(None)
            .has_label("person")
            .property_by("age", __::values("age").as_number(GType::LONG))
            .profile()
            .unwrap();
        let plan = result.to_string();

        assert!(
            plan.contains("property(\"age\""),
            "missing property step in: {plan}"
        );
        assert!(
            plan.contains("values(\"age\")"),
            "missing nested traversal in: {plan}"
        );
        assert!(
            plan.contains("as_number"),
            "missing coercion step in: {plan}"
        );

        // Rule 10: a SideEffectMut step carrying a nested traversal must not be reordered or
        // folded by any optimizer rule. `has_label()` itself is folded into the source `v()` by
        // `SourceFilterPushdownRule`, so the physical plan pins that step ahead of `property()`.
        let source_pos = plan
            .find("v(labels: [\"person\"])")
            .expect("folded source step in plan");
        let property_pos = plan
            .find("property(\"age\"")
            .expect("property step in plan");
        assert!(
            source_pos < property_pos,
            "the source step must stay before the mutating property() step in: {plan}"
        );
    }

    #[test]
    fn traversal_value_empty_leaves_property_unchanged() {
        let mut graph = TraversalGraph::tinkerpop_modern();
        let before = graph.traversal().v("1").values("nickname").next().unwrap();
        assert_eq!(before, None);

        graph
            .traversal_mut()
            .v("1")
            .property_by("nickname", __::values("nickname"))
            .next()
            .unwrap();

        let after = graph.traversal().v("1").values("nickname").next().unwrap();
        assert_eq!(after, None);
    }
}
