use crate::graph::GraphStorage;
use crate::*;

/// What `has(.., traversal)` runs its traversal on: a property's value, the id or the label.
#[derive(Clone)]
enum HasTarget {
    /// `has(key, traversal)`: the value of the property `key`.
    Property(PropKey),
    /// `has(T.id, traversal)`: the element's id.
    Id,
    /// `has(T.label, traversal)`: the element's label.
    Label,
    /// A token that names nothing to test (`Direction.OUT`, `Direction.IN`): fails in
    /// [`GraphStepFilter::plan_check`], as TinkerPop's `has(T, Traversal)` takes only `T.id`
    /// and `T.label`.
    Unsupported(RecordKey),
}

impl From<HasKey> for HasTarget {
    fn from(key: HasKey) -> Self {
        match key {
            HasKey::Property(key) => HasTarget::Property(key),
            HasKey::Token(RecordKey::Name(name)) => HasTarget::Property(PropKey::Name(name)),
            HasKey::Token(RecordKey::Id) => HasTarget::Id,
            HasKey::Token(RecordKey::Label) => HasTarget::Label,
            HasKey::Token(token @ (RecordKey::Out | RecordKey::In)) => {
                HasTarget::Unsupported(token)
            }
        }
    }
}

/// The DSL spelling of a token key: `T.id`, `T.label`, `Direction.OUT`, `Direction.IN`.
fn token_text(token: &RecordKey) -> &'static str {
    match token {
        RecordKey::Id => "T.id",
        RecordKey::Label => "T.label",
        RecordKey::Out => "Direction.OUT",
        RecordKey::In => "Direction.IN",
        RecordKey::Name(_) => "a name",
    }
}

/// `has(key, traversal)` (TinkerPop's `has(String, Traversal)`) and `has(T.id|T.label,
/// traversal)` (TinkerPop's `has(T, Traversal)`): keeps a traverser when the child traversal,
/// run on the value of its property `key` (or its id, or its label), yields at least one
/// result; an element without the property is filtered out. Exactly TinkerPop's expansion
/// `filter(values(key).traversal)` (`filter(id().traversal)`, `filter(label().traversal)`): the
/// step holds ONE child, `values(key)` (`id()`, `label()`) followed by the steps of
/// `traversal`, so step ids, paths, authorization and profiles are those of that filter (the
/// child's first step, id `N.0.0`, is the `values(key)`). It renders as written:
/// `has("name", __.to_lower().is("alice"))`, `has(T.id, __.is("1"))`.
///
/// The `property_index` rule serves the shape `has(key, __.to_lower().is(P))` (book page
/// "Property Indexes", section "Case-insensitive search") from an index with the transform
/// `to_lower` ([`GraphStepFilter::index_condition`]); the id and label forms are never served.
struct StepHasTraversal<'g, S: GraphStorage + 'g> {
    target: HasTarget,
    /// `values(key)` (`id()`, `label()`) followed by the steps of the user's traversal.
    traversal: AnonymousTraversal<'g, S>,
}

impl<'g, S: GraphStorage + 'g> Clone for StepHasTraversal<'g, S> {
    fn clone(&self) -> Self {
        Self {
            target: self.target.clone(),
            traversal: self.traversal.clone(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepHasTraversal<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.target {
            HasTarget::Property(PropKey::Name(name)) => {
                write!(f, "has({}, __", crate::value::literal::rhai_string(name))?
            }
            HasTarget::Property(PropKey::Path(path)) => {
                write!(f, "has({}, __", crate::value::literal::rhai_jpath(path))?
            }
            HasTarget::Id => f.write_str("has(T.id, __")?,
            HasTarget::Label => f.write_str("has(T.label, __")?,
            HasTarget::Unsupported(token) => write!(f, "has({}, __", token_text(token))?,
        }
        // The user's steps: every step after the leading `values(key)`, modulators left out
        // like in every child rendering.
        let mut any = false;
        for step in self.traversal.steps.iter().skip(1) {
            if matches!(step, GraphStep::Modulator(_, _)) {
                continue;
            }
            any = true;
            write!(f, ".{}", step.get_name())?;
        }
        if !any {
            f.write_str(".identity()")?;
        }
        f.write_str(")")
    }
}

/// The step of `has(key, traversal)`, `has(T.id, traversal)` and `has(T.label, traversal)`.
fn has_traversal_step<'g, S: GraphStorage + 'g>(
    target: HasTarget,
    traversal: AnonymousTraversal<'g, S>,
) -> GraphStep<'g, S> {
    let mut child = match &target {
        HasTarget::Property(key) => AnonymousTraversal::new().values(key.clone()),
        HasTarget::Id => AnonymousTraversal::new().id(),
        HasTarget::Label => AnonymousTraversal::new().label(),
        // Never runs: `plan_check` refuses the step first.
        HasTarget::Unsupported(_) => AnonymousTraversal::new(),
    };
    child.steps.extend(traversal.steps);
    GraphStep::Filter(
        "has_traversal",
        Box::new(StepHasTraversal {
            target,
            traversal: child,
        }),
    )
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `has(key, traversal)`: keeps the traversers whose property `key` (a name or a
    /// [`JsonPath`]) has a value for which `traversal` yields at least one result, i.e.
    /// `filter(values(key).traversal)` (TinkerPop's `has(String, Traversal)`). An element
    /// without the property is filtered out. `has("name", __.to_lower().is("marko"))` is a
    /// case-insensitive search, served by a property index with the transform `to_lower`.
    ///
    /// With [`RecordKey::Id`] or [`RecordKey::Label`] as the key the traversal runs on the
    /// element's id or label instead, i.e. `filter(id().traversal)` / `filter(label().traversal)`
    /// (TinkerPop's `has(T, Traversal)`): `has_traversal(RecordKey::Label, __.is("person"))`
    /// keeps the persons. [`RecordKey::Out`] and [`RecordKey::In`] name nothing to test and
    /// fail when the traversal runs.
    pub fn has_traversal<K: Into<HasKey>>(
        self,
        key: K,
        traversal: AnonymousTraversal<'g, S>,
    ) -> Self {
        self.push_step(has_traversal_step(HasTarget::from(key.into()), traversal))
    }
}

impl __ {
    /// Anonymous `has(key, traversal)`, see [`GraphTraversalSource::has_traversal`].
    pub fn has_traversal<'g, S: GraphStorage + 'g, K: Into<HasKey>>(
        key: K,
        traversal: AnonymousTraversal<'g, S>,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().has_traversal(key, traversal)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `has(key, traversal)` on an anonymous traversal, see
    /// [`GraphTraversalSource::has_traversal`].
    pub fn has_traversal<K: Into<HasKey>>(
        self,
        key: K,
        traversal: AnonymousTraversal<'g, S>,
    ) -> Self {
        self.push_step(has_traversal_step(HasTarget::from(key.into()), traversal))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFilter<'g, S> for StepHasTraversal<'g, S> {
    /// `has(key, __.to_lower().is(P))` on a literal name, `P` an equality, `P.within` or a
    /// range: the condition a `to_lower` index serves (the constants of `P` as given, compared
    /// with the lowered values). Any other child is not served.
    fn index_condition(&self) -> Option<crate::traversal::step::IndexCondition<'_>> {
        let HasTarget::Property(key) = &self.target else {
            return None;
        };
        let key = key.as_name()?;
        let [values, transform, predicate] = self.traversal.steps.as_slice() else {
            return None;
        };
        let (
            GraphStep::FlatMap("values", _),
            GraphStep::Map(_, transform),
            GraphStep::Filter("is", predicate),
        ) = (values, transform, predicate)
        else {
            return None;
        };
        let transform = transform.index_transform()?;
        let ranges = predicate.value_index_ranges()?;
        Some(crate::traversal::step::IndexCondition {
            key,
            transform: Some(transform),
            ranges,
        })
    }

    fn plan_check(&self) -> TraverserResult<()> {
        match &self.target {
            HasTarget::Unsupported(token) => Err(Box::new(TraverserError::QueryExecution {
                query: "has_traversal".to_string(),
                reason: format!(
                    "{} names nothing a traversal can test: has(.., traversal) takes a \
                     property key, T.id or T.label",
                    token_text(token)
                ),
            })),
            HasTarget::Property(_) | HasTarget::Id | HasTarget::Label => Ok(()),
        }
    }

    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<bool> {
        self.traversal.exists(context, source)
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut crate::traversal::AnonymousTraversal<'g, S>> {
        vec![&mut self.traversal]
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &crate::traversal::optimizer::path_analysis::PathAnalysis,
        _downstream: &crate::traversal::optimizer::path_analysis::PathRequirement,
    ) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        // Existence test only: the child's output does not continue the path.
        self.traversal.analyze_paths(
            analysis,
            crate::traversal::optimizer::path_analysis::PathRequirement::None,
        )
    }

    fn clone_box(&self) -> Box<dyn GraphStepFilter<'g, S> + 'g> {
        Box::new(self.clone())
    }
}
