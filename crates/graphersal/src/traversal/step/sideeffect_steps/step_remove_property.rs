use crate::graph::GraphStorage;
use crate::*;

/// `remove_property(keys...)`: removes the listed property keys (none listed: every property) of
/// each incoming vertex or edge and emits the same element, unchanged otherwise.
#[derive(Clone)]
struct StepRemoveProperty {
    /// `None` removes every property of the element.
    keys: Option<Vec<PropKey>>,
}

impl fmt::Display for StepRemoveProperty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.keys {
            None => write!(f, "remove_property()"),
            Some(keys) => {
                let quoted: Vec<String> = keys
                    .iter()
                    .map(crate::value::literal::rhai_prop_key)
                    .collect();
                match quoted.as_slice() {
                    [one] => write!(f, "remove_property({one})"),
                    _ => write!(f, "remove_property([{}])", quoted.join(", ")),
                }
            }
        }
    }
}

fn keys_of(args: PropKeys) -> Option<Vec<PropKey>> {
    match args {
        PropKeys::None => None,
        PropKeys::One(key) => Some(vec![key]),
        PropKeys::List(keys) => Some(keys),
    }
}

fn remove_property_step<'g, S: GraphStorage + 'g>(args: PropKeys) -> GraphStep<'g, S> {
    GraphStep::SideEffectMut(
        "remove_property",
        Box::new(StepRemoveProperty {
            keys: keys_of(args),
        }),
    )
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Removes properties from the incoming vertices or edges and lets the element continue.
    ///
    /// No argument (`()`/[`PropKeys::None`]) removes every property of the element; a string
    /// or a list of strings removes those keys, and a [`JsonPath`] key (`jpath("meta.tags[0]")`)
    /// removes the value at that path: the first segment is the property name, the rest descends.
    /// An object key is removed (an emptied parent object stays), an array element is removed
    /// with shift (`[-1]` is the last one). A key or path the element does not carry, an
    /// out-of-range index and a type mismatch on the way (a key step on an array, an index step
    /// on an object, a descent through a scalar) are all "the path does not exist": a no-op, never
    /// an error. A one-segment path is the same as the plain name. With a schema active the whole
    /// resulting property is stored through the schema layer like `property(k, v)` (see the
    /// jpath book page for what is checked); with no schema the removal is unchecked.
    /// A string stays a literal key: `"a.b"` removes a property named `a.b`.
    /// Unlike [`Self::drop`], which consumes the traverser and deletes the element or the
    /// property handle, this step emits the same element, so `values("name")` can follow.
    /// `property(k, null)` still stores a `null`; this is the explicit way to remove a property.
    ///
    /// A Graphersal extension (TinkerPop removes with `properties(k).drop()`). A plain-name removal
    /// is checked against a declared schema (Open and Closed): removing a required (non-nullable)
    /// declared property is rejected, like for `drop()`; optional/undeclared/absent ones are fine.
    /// `properties(jpath(..)).drop()` stays an error (a path property element holds no reference
    /// to its owner); use this step instead.
    /// A non-element input (a value, a map, a path, a property handle) is a cast error. The step
    /// is idempotent, so a bulk-n traverser removes once.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let mut graph = TraversalGraph::tinkerpop_modern();
    /// graph.traversal_mut().v("1").remove_property("age").next().unwrap();
    /// assert_eq!(graph.traversal().v("1").values("age").next().unwrap(), None);
    /// assert!(graph.traversal().v("1").values("name").next().unwrap().is_some());
    /// ```
    pub fn remove_property<T: Into<PropKeys>>(self, keys: T) -> Self {
        self.push_step(remove_property_step(keys.into()))
    }
}

impl __ {
    /// Anonymous `remove_property()`; see [`GraphTraversalSource::remove_property`].
    pub fn remove_property<'g, S: GraphStorage + 'g, T: Into<PropKeys>>(
        keys: T,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().remove_property(keys)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Anonymous `remove_property()`; see [`GraphTraversalSource::remove_property`].
    pub fn remove_property<T: Into<PropKeys>>(self, keys: T) -> Self {
        self.push_step(remove_property_step(keys.into()))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepSideEffectMut<'g, S> for StepRemoveProperty {
    fn property_access(
        &self,
        visit: &mut dyn FnMut(Option<&str>, crate::graph::workload::AccessKind),
    ) {
        match &self.keys {
            None => visit(None, crate::graph::workload::AccessKind::Remove),
            Some(keys) => keys
                .iter()
                .filter_map(|key| key.top_level())
                .for_each(|key| visit(Some(key), crate::graph::workload::AccessKind::Remove)),
        }
    }
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Option<Traverser<S>>,
    ) -> TraverserResult<Vec<Traverser<S>>> {
        let Some(source) = source else {
            return Ok(Vec::new());
        };
        match &source.value {
            TraverserValue::Vertex(node) => {
                let node = *node;
                match &self.keys {
                    Some(keys) => {
                        for key in keys {
                            match key {
                                PropKey::Name(name) => {
                                    context.storage.drop_vertex_property(name, node)?
                                }
                                PropKey::Path(path) => {
                                    context.storage.remove_vertex_property_path(node, path)?
                                }
                            }
                        }
                    }
                    None => {
                        let keys: Vec<CompactString> = context
                            .storage
                            .vertex_properties(node)
                            .map(|element| element.keys().cloned().collect())
                            .unwrap_or_default();
                        for key in &keys {
                            context.storage.drop_vertex_property(key, node)?;
                        }
                    }
                }
            }
            TraverserValue::Edge(edge) => {
                let edge = *edge;
                match &self.keys {
                    Some(keys) => {
                        for key in keys {
                            match key {
                                PropKey::Name(name) => {
                                    context.storage.drop_edge_property(name, edge)?
                                }
                                PropKey::Path(path) => {
                                    context.storage.remove_edge_property_path(edge, path)?
                                }
                            }
                        }
                    }
                    None => {
                        let keys: Vec<CompactString> = context
                            .storage
                            .edge_properties(edge)
                            .map(|element| element.keys().cloned().collect())
                            .unwrap_or_default();
                        for key in &keys {
                            context.storage.drop_edge_property(key, edge)?;
                        }
                    }
                }
            }
            other => {
                return Err(Box::new(TraverserError::cast_exception(
                    &["vertex", "edge"],
                    other,
                )));
            }
        }
        Ok(vec![source])
    }

    fn extends_path(&self) -> bool {
        false
    }

    fn clone_box(&self) -> Box<dyn GraphStepSideEffectMut<'g, S> + 'g> {
        Box::new(self.clone())
    }
}
