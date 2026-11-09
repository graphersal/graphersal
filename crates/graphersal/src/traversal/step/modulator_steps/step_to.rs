use crate::graph::GraphStorage;
use crate::*;

/// A `to()` modulator: step labels / vertex ids / vertices, or one child traversal (`to(__.V(..))`),
/// moved into the modulated step when the modulator is pushed (see `push_step_into`).
struct StepTo<'g, S: GraphStorage + 'g> {
    args: Option<Vec<EndpointArg>>,
    traversal: Option<AnonymousTraversal<'g, S>>,
}

impl<'g, S: GraphStorage + 'g> Clone for StepTo<'g, S> {
    fn clone(&self) -> Self {
        Self {
            args: self.args.clone(),
            traversal: self.traversal.clone(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepTo<'g, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.args, &self.traversal) {
            (_, Some(traversal)) => write!(f, "to({traversal})"),
            (Some(args), None) => write!(f, "to({})", crate::value::literal::endpoint_args(args)),
            (None, None) => write!(f, "to()"),
        }
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Modulates the step right before it (TinkerPop's `FromToModulating`). After `add_e()` it
    /// names the vertex (or vertices, for a list of step labels/ids) an edge is created *to*.
    /// `add_e()` may be followed by `property()` calls before `from()`/`to()`; an endpoint may also
    /// be a vertex value (as for [`GraphTraversalSource::from`]), a traversal
    /// ([`GraphTraversalSource::to_by`]) or a side-effect key. Deviations from
    /// TinkerPop (list of names, plain vertex ids, string-compared ids) are on the "addE() endpoints"
    /// page of the user's guide and in [`GraphTraversalSource::add_e`].
    /// After `path()`, `simple_path()` or `cyclic_path()` it names the step label where the
    /// window onto the path ends (exactly one label; the last position carrying it, the final
    /// position without `to()`). See [`GraphTraversalSource::from`] for which steps take it and
    /// what a rejected call does.
    pub fn to<T: Into<EndpointArgs>>(self, args: T) -> Self {
        let args = args.into().0;
        self.push_step(GraphStep::Modulator(
            "to",
            Box::new(StepTo {
                args,
                traversal: None,
            }),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Modulates the step right before it, inside a child traversal (`__.add_e(...).to(...)`);
    /// see [`GraphTraversalSource::to`] for the accepted arguments and the steps that take
    /// `to()`.
    pub fn to<T: Into<EndpointArgs>>(self, args: T) -> Self {
        let args = args.into().0;
        let step = GraphStep::Modulator(
            "to",
            Box::new(StepTo {
                args,
                traversal: None,
            }),
        );
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `to(traversal)`: the endpoint of the `add_e()` before it is the first result of
    /// `traversal`, run for each incoming traverser (a vertex or the id of a vertex). Only
    /// `add_e()` takes a traversal; see [`GraphTraversalSource::add_e_by`].
    pub fn to_by(self, traversal: AnonymousTraversal<'g, S>) -> Self {
        self.push_step(GraphStep::Modulator(
            "to",
            Box::new(StepTo {
                args: None,
                traversal: Some(traversal),
            }),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `to(traversal)` inside a child traversal; see [`GraphTraversalSource::to_by`].
    pub fn to_by(self, traversal: AnonymousTraversal<'g, S>) -> Self {
        self.push_step(GraphStep::Modulator(
            "to",
            Box::new(StepTo {
                args: None,
                traversal: Some(traversal),
            }),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepModulator<'g, S> for StepTo<'g, S> {
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
