use std::sync::Arc;

use crate::graph::GraphStorage;
use crate::*;

struct StepProject<'g, S: GraphStorage + 'g> {
    args: Option<Vec<String>>,
    precomputed_keys: Option<Arc<Vec<RecordKey>>>,
    by: Option<Vec<By<'g, S>>>,
    by_check: ByModulatorCheck,
}

impl<'g, S: GraphStorage + 'g> Clone for StepProject<'g, S> {
    fn clone(&self) -> Self {
        Self {
            args: self.args.clone(),
            precomputed_keys: self.precomputed_keys.clone(),
            by: self.by.clone(),
            by_check: self.by_check.clone(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> StepProject<'g, S> {
    fn new(args: Option<Vec<String>>) -> Self {
        let precomputed_keys = args
            .as_ref()
            .map(|a| Arc::new(a.iter().map(|s| RecordKey::from(s.as_str())).collect()));
        Self {
            args,
            precomputed_keys,
            by: None,
            by_check: ByModulatorCheck::default(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepProject<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut by = "".to_string();
        if let Some(by_list) = self.by.as_ref() {
            by_list
                .iter()
                .for_each(|b| by.push_str(&crate::traversal::render_by(b)));
        }
        write!(
            f,
            "project({}){}",
            display_option_vec_string(&self.args),
            by
        )
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Turns each traverser into a map with the given keys, one by() per key in order.
    pub fn project<T: Into<ArgStrings>>(self, args: T) -> Self {
        let args = match args.into() {
            ArgStrings::None => None,
            ArgStrings::String(v) => Some(vec![v]),
            ArgStrings::List(v) => Some(v),
        };
        self.push_step(GraphStep::Map("project", Box::new(StepProject::new(args))))
    }
}

impl __ {
    /// Turns each traverser into a map with the given keys, one by() per key in order. Starts an
    /// anonymous traversal (`__.project()`).
    pub fn project<'g, S: GraphStorage + 'g, T: Into<ArgStrings>>(
        labels: T,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().project(labels)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Turns each traverser into a map with the given keys, one by() per key in order.
    pub fn project<T: Into<ArgStrings>>(self, args: T) -> Self {
        let args = match args.into() {
            ArgStrings::None => None,
            ArgStrings::String(v) => Some(vec![v]),
            ArgStrings::List(v) => Some(v),
        };
        let step = GraphStep::Map("project", Box::new(StepProject::new(args)));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepProject<'g, S> {
    fn property_access(
        &self,
        visit: &mut dyn FnMut(Option<&str>, crate::graph::workload::AccessKind),
    ) {
        for key in self.by.iter().flatten().filter_map(By::property_key) {
            visit(Some(key), crate::graph::workload::AccessKind::Read);
        }
    }
    /// `project()` applies its `by()` modulators as TinkerPop's traversal ring: round-robin over
    /// the keys, so one `by()` applies to every key, `project("a", "b", "c").by("name").by("age")`
    /// reads a: name, b: age, c: name, and without `by()` every key holds the incoming object
    /// itself. Extra `by()`s beyond the keys are never reached. A key whose `by()` is unproductive
    /// (such as `by("age")` on a vertex without `age`) is left out of the map; the traverser is
    /// kept, TinkerPop 3.6+'s "productive `by()`" rule for `project()`.
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        self.by_check.check("project")?;
        let keys = match &self.precomputed_keys {
            Some(keys) if !keys.is_empty() => keys,
            _ => {
                return Err(Box::new(TraverserError::Value(
                    ValueError::PropertyNotFound {
                        key: "keys".to_string(),
                        element_type: "project",
                    },
                )));
            }
        };
        let by_list = self.by.as_deref().unwrap_or_default();

        let mut values = Vec::with_capacity(keys.len());
        // Allocated only once some key's `by()` turns out unproductive: the keys that stay.
        let mut kept: Option<Vec<RecordKey>> = None;
        for (i, key) in keys.iter().enumerate() {
            let produced = if by_list.is_empty() {
                Some(source.value.clone())
            } else {
                TraverserValue::fetch_traverser_value_by(
                    context,
                    &source,
                    &source.value,
                    &by_list[i % by_list.len()],
                )?
            };
            match produced {
                Some(value) => {
                    values.push(value);
                    if let Some(kept) = kept.as_mut() {
                        kept.push(key.clone());
                    }
                }
                None => {
                    if kept.is_none() {
                        kept = Some(keys[..i].to_vec());
                    }
                }
            }
        }

        let keys = match kept {
            Some(kept) => Arc::new(kept),
            None => Arc::clone(keys),
        };
        Ok(Traverser::with_parts(
            TraverserValue::Record { keys, values },
            source.path,
            source.bulk,
            source.sack,
        ))
    }

    fn push_modulator_by(&mut self, args: By<'g, S>, _modulator_name: &str) -> ByModulation {
        self.by_check.observe_ring(&args);
        if let Some(x) = self.by.as_mut() {
            x.push(args);
        } else {
            self.by = Some(vec![args]);
        }
        ByModulation::Accepted
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut crate::traversal::AnonymousTraversal<'g, S>> {
        self.by
            .iter_mut()
            .flatten()
            .filter_map(By::traversal_mut)
            .collect()
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &crate::traversal::optimizer::path_analysis::PathAnalysis,
        _downstream: &crate::traversal::optimizer::path_analysis::PathRequirement,
    ) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        // `by(__...)` children run on the incoming traverser, path included.
        match &mut self.by {
            Some(by_list) => By::analyze_all_paths(by_list, analysis),
            None => crate::traversal::optimizer::path_analysis::PathRequirement::None,
        }
    }

    fn clone_box(&self) -> Box<dyn GraphStepMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "script")]
    #[test]
    fn a_first() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1")
                .project(vec!["id", "name", "out", "in"])
                .by(By::Id)
                .by("name")
                .by(__::out_e(None).count())
                .by(__::in_e(None).count())
                .to_list(),
            rhai: [
                r#"g.V("1").project(["id", "name", "out", "in"]).by(By::Id).by("name").by(__.outE().count()).by(__.inE().count())"#,
                r#"g.v("1").project(["id", "name", "out", "in"]).by(By::Id).by("name").by(__.out_e().count()).by(__.in_e().count())"#,
            ],
            assert: |res| {
                match &res[0] {
                    ElementProperty::Object(obj) => {
                        assert_eq!(obj.get("id").unwrap().as_string(), Some("1"));
                        assert_eq!(obj.get("name").unwrap().as_string(), Some("marko"));
                        assert_eq!(obj.get("out").unwrap().as_i64(), Some(3));
                        assert_eq!(obj.get("in").unwrap().as_i64(), Some(0));
                    },
                    _ => panic!(),
                }
            },
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g
            .v("1")
            .project(vec!["id", "name", "out", "in"])
            .by(By::Id)
            .by("name")
            .by(__::out_e(None).count())
            .by(__::in_e(None).count())
            .profile()
            .unwrap();
        assert!(result.to_string().contains("project("));
    }
}
