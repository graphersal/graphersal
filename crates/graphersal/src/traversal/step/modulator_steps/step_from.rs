use crate::graph::GraphStorage;
use crate::*;

/// A `from()` modulator: step labels / vertex ids / vertices, or one child traversal (`from(__.V(..))`),
/// moved into the modulated step when the modulator is pushed (see `push_step_into`).
struct StepFrom<'g, S: GraphStorage + 'g> {
    args: Option<Vec<EndpointArg>>,
    traversal: Option<AnonymousTraversal<'g, S>>,
}

impl<'g, S: GraphStorage + 'g> Clone for StepFrom<'g, S> {
    fn clone(&self) -> Self {
        Self {
            args: self.args.clone(),
            traversal: self.traversal.clone(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepFrom<'g, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.args, &self.traversal) {
            (_, Some(traversal)) => write!(f, "from({traversal})"),
            (Some(args), None) => match args.as_slice() {
                [one] => write!(f, "from({})", crate::value::literal::endpoint_arg(one)),
                _ => write!(f, "from([{}])", crate::value::literal::endpoint_args(args)),
            },
            (None, None) => write!(f, "from()"),
        }
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Modulates the step right before it (TinkerPop's `FromToModulating`). After `add_e()` it
    /// names the vertex (or vertices, for a list of step labels/ids) an edge is created *from*.
    /// `add_e()` may be followed by `property()` calls before `from()`/`to()`; an endpoint may also
    /// be a vertex value (an [`ElementProperty::Vertex`] a query returned, a
    /// [`VertexValue`](crate::prelude::VertexValue) or an edge's
    /// [`VertexRef`](crate::prelude::VertexRef): that vertex, by its id, never read as a step label
    /// or side-effect key), a traversal ([`GraphTraversalSource::from_by`]) or a side-effect key. Deviations from
    /// TinkerPop (list of names, plain vertex ids, string-compared ids) are on the "addE() endpoints"
    /// page of the user's guide and in [`GraphTraversalSource::add_e`].
    /// After `path()`, `simple_path()` or `cyclic_path()` it names the step label where the
    /// window onto the path starts (exactly one label; the last position carrying it, position 0
    /// without `from()`); see the "Path windows" page of the user's guide.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let lock = graph.read();
    /// let paths = lock
    ///     .traversal()
    ///     .v("1")
    ///     .as_("a")
    ///     .out(None)
    ///     .as_("b")
    ///     .out(None)
    ///     .path()
    ///     .from("b")
    ///     .by("name")
    ///     .to_list()
    ///     .unwrap();
    /// assert_eq!(paths.len(), 2);
    /// ```
    ///
    /// A `from()` after any other step, or with no step before it, fails with
    /// [`TraverserError::InvalidModulator`] when the traversal executes, or earlier still when
    /// the plan-time validation pass finds it in a
    /// branch that never runs.
    pub fn from<T: Into<EndpointArgs>>(self, args: T) -> Self {
        let args = args.into().0;
        self.push_step(GraphStep::Modulator(
            "from",
            Box::new(StepFrom {
                args,
                traversal: None,
            }),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Modulates the step right before it, inside a child traversal (`__.add_e(...).from(...)`);
    /// see [`GraphTraversalSource::from`] for the accepted arguments and the steps that take
    /// `from()`.
    pub fn from<T: Into<EndpointArgs>>(self, args: T) -> Self {
        let args = args.into().0;
        let step = GraphStep::Modulator(
            "from",
            Box::new(StepFrom {
                args,
                traversal: None,
            }),
        );
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `from(traversal)`: the endpoint of the `add_e()` before it is the first result of
    /// `traversal`, run for each incoming traverser (a vertex or the id of a vertex). Only
    /// `add_e()` takes a traversal; see [`GraphTraversalSource::add_e_by`].
    pub fn from_by(self, traversal: AnonymousTraversal<'g, S>) -> Self {
        self.push_step(GraphStep::Modulator(
            "from",
            Box::new(StepFrom {
                args: None,
                traversal: Some(traversal),
            }),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `from(traversal)` inside a child traversal; see [`GraphTraversalSource::from_by`].
    pub fn from_by(self, traversal: AnonymousTraversal<'g, S>) -> Self {
        self.push_step(GraphStep::Modulator(
            "from",
            Box::new(StepFrom {
                args: None,
                traversal: Some(traversal),
            }),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepModulator<'g, S> for StepFrom<'g, S> {
    fn get_args(&mut self) -> Option<Vec<EndpointArg>> {
        self.args.take()
    }

    fn get_traversal(&mut self) -> Option<AnonymousTraversal<'g, S>> {
        self.traversal.take()
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut crate::traversal::AnonymousTraversal<'g, S>> {
        self.traversal.as_mut().into_iter().collect()
    }

    fn clone_box(&self) -> Box<dyn GraphStepModulator<'g, S> + 'g> {
        Box::new(self.clone())
    }
}
