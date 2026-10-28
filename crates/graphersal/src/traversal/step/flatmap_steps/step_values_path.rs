//! `values(..)` / `properties(..)` with at least one [`JsonPath`](crate::value::JsonPath) key.
//!
//! The builders in `step_values.rs` / `step_properties.rs` use these steps only when a key is a
//! path; a call with plain names builds the existing literal-name step, so nothing changes for
//! it. The keys are processed in the given order; a name key is handled by the very same code as
//! before (it delegates to a one-key literal-name step), a path key reads the value at the path.
//!
//! A path that does not exist (missing key, out-of-range index, type mismatch on the way)
//! produces nothing, it is never an error. The read descends by reference into the stored
//! property and clones only the value the path ends on.
//!
//! `properties(path)` yields a [`TraverserValue::PathProperty`]: a materialized property element
//! whose `key()` is the canonical path text (`$.a.b[1]`) and whose `value()` is the leaf (a path
//! has no stored property to point at, so no lazy handle exists). Only a vertex or an edge is a
//! valid input, a missing path produces nothing. A Graphersal extension, a plain name behaves
//! exactly as in TinkerPop; see `book/src/users_guide/jpath_keys.md` and `tinkerpop_deviations.md`.

use super::step_properties::StepProperties;
use super::step_values::StepValues;
use crate::graph::GraphStorage;
use crate::value::json_path::JsonPathSegment;
use crate::*;

/// One key of the step with the one-key literal-name step that handles it (for a name).
#[derive(Clone)]
struct KeyPlan<T> {
    key: PropKey,
    by_name: Option<T>,
}

fn plan_values(keys: Vec<PropKey>) -> Vec<KeyPlan<StepValues>> {
    keys.into_iter()
        .map(|key| {
            let by_name = key.as_name().map(|n| StepValues {
                args: Some(vec![n.to_string()]),
            });
            KeyPlan { key, by_name }
        })
        .collect()
}

fn plan_properties(keys: Vec<PropKey>) -> Vec<KeyPlan<StepProperties>> {
    keys.into_iter()
        .map(|key| {
            let by_name = key.as_name().map(|n| StepProperties {
                args: Some(vec![n.to_string()]),
            });
            KeyPlan { key, by_name }
        })
        .collect()
}

fn render_keys<T>(plans: &[KeyPlan<T>]) -> String {
    plans
        .iter()
        .map(|p| crate::value::literal::rhai_prop_key(&p.key))
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Clone)]
struct StepValuesKeys {
    plans: Vec<KeyPlan<StepValues>>,
}

impl fmt::Display for StepValuesKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "values({})", render_keys(&self.plans))
    }
}

#[derive(Clone)]
struct StepPropertiesKeys {
    plans: Vec<KeyPlan<StepProperties>>,
}

impl fmt::Display for StepPropertiesKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "properties({})", render_keys(&self.plans))
    }
}

pub(super) fn values_keys_step<'g, S: GraphStorage + 'g>(keys: Vec<PropKey>) -> GraphStep<'g, S> {
    GraphStep::FlatMap(
        "values",
        Box::new(StepValuesKeys {
            plans: plan_values(keys),
        }),
    )
}

pub(super) fn properties_keys_step<'g, S: GraphStorage + 'g>(
    keys: Vec<PropKey>,
) -> GraphStep<'g, S> {
    GraphStep::FlatMap(
        "properties",
        Box::new(StepPropertiesKeys {
            plans: plan_properties(keys),
        }),
    )
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatMap<'g, S> for StepValuesKeys {
    fn property_access(
        &self,
        visit: &mut dyn FnMut(Option<&str>, crate::graph::workload::AccessKind),
    ) {
        for plan in &self.plans {
            if let Some(key) = plan.key.top_level() {
                visit(Some(key), crate::graph::workload::AccessKind::Read);
            }
        }
    }
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
        output: &mut Vec<Traverser<S>>,
    ) -> TraverserResult<()> {
        let count = self.plans.len();
        let mut source = Some(source);
        for (i, plan) in self.plans.iter().enumerate() {
            // The last key takes the traverser; earlier ones work on a copy.
            let this = if i + 1 == count {
                source.take()
            } else {
                source.clone()
            };
            let Some(this) = this else { break };
            match (&plan.key, &plan.by_name) {
                (PropKey::Name(_), Some(by_name)) => by_name.process(context, this, output)?,
                (PropKey::Path(path), _) => {
                    read_path_values(context, this, path.segments(), output)?;
                }
                (PropKey::Name(_), None) => {}
            }
        }
        Ok(())
    }

    fn clone_box(&self) -> Box<dyn GraphStepFlatMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

fn read_path_values<S: GraphStorage>(
    context: &mut TraverserContext<'_, S>,
    source: Traverser<S>,
    segments: &[JsonPathSegment],
    output: &mut Vec<Traverser<S>>,
) -> TraverserResult<()> {
    let readable = matches!(
        source.value,
        TraverserValue::Vertex(_)
            | TraverserValue::Edge(_)
            | TraverserValue::VertexProperty(..)
            | TraverserValue::EdgeProperty(..)
            | TraverserValue::Computed(..)
            | TraverserValue::Object(_)
            | TraverserValue::Record { .. }
            | TraverserValue::Array(_)
    );
    if !readable {
        return Err(Box::new(TraverserError::cast_exception(
            &["vertex", "edge", "object"],
            &source.value,
        )));
    }
    // A path naming only a stored property keeps the lazy reference `values(name)` emits, so a
    // downstream filter can drop the traverser before the property is fetched.
    if let [JsonPathSegment::Key(key)] = segments {
        match &source.value {
            TraverserValue::Vertex(node) => {
                if context
                    .storage
                    .vertex_properties(*node)?
                    .is_some_and(|props| props.contains_key(key.as_str()))
                {
                    output.push(Traverser::with_parts(
                        TraverserValue::VertexProperty(
                            key.clone(),
                            *node,
                            PropertyHandleKind::Value,
                        ),
                        source.path,
                        source.bulk,
                        source.sack,
                    ));
                }
                return Ok(());
            }
            TraverserValue::Edge(edge) => {
                if context
                    .storage
                    .edge_properties(*edge)?
                    .is_some_and(|props| props.contains_key(key.as_str()))
                {
                    output.push(Traverser::with_parts(
                        TraverserValue::EdgeProperty(key.clone(), *edge, PropertyHandleKind::Value),
                        source.path,
                        source.bulk,
                        source.sack,
                    ));
                }
                return Ok(());
            }
            _ => {}
        }
    }
    if let Some(leaf) = TraverserValue::resolve_json_path(context, &source.value, segments)? {
        output.push(Traverser::with_parts(
            leaf,
            source.path,
            source.bulk,
            source.sack,
        ));
    }
    Ok(())
}

/// `properties(jpath(..))`: one [`TraverserValue::PathProperty`] (canonical path text + the leaf)
/// per input whose path exists; a missing path produces nothing. The leaf is cloned out of
/// storage (a path has no stored property to point at, so no lazy handle is possible).
fn read_path_properties<S: GraphStorage>(
    context: &mut TraverserContext<'_, S>,
    source: Traverser<S>,
    path: &JsonPath,
    output: &mut Vec<Traverser<S>>,
) -> TraverserResult<()> {
    if !matches!(
        source.value,
        TraverserValue::Vertex(_) | TraverserValue::Edge(_)
    ) {
        return Err(Box::new(TraverserError::cast_exception(
            &["vertex", "edge"],
            &source.value,
        )));
    }
    if let Some(leaf) = TraverserValue::resolve_json_path(context, &source.value, path.segments())?
    {
        let leaf = leaf.into_node_property(context.storage)?;
        output.push(Traverser::with_parts(
            TraverserValue::PathProperty(Box::new((CompactString::new(path.to_string()), leaf))),
            source.path,
            source.bulk,
            source.sack,
        ));
    }
    Ok(())
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatMap<'g, S> for StepPropertiesKeys {
    fn property_access(
        &self,
        visit: &mut dyn FnMut(Option<&str>, crate::graph::workload::AccessKind),
    ) {
        for plan in &self.plans {
            if let Some(key) = plan.key.top_level() {
                visit(Some(key), crate::graph::workload::AccessKind::Read);
            }
        }
    }
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
        output: &mut Vec<Traverser<S>>,
    ) -> TraverserResult<()> {
        let count = self.plans.len();
        let mut source = Some(source);
        for (i, plan) in self.plans.iter().enumerate() {
            let this = if i + 1 == count {
                source.take()
            } else {
                source.clone()
            };
            let Some(this) = this else { break };
            match (&plan.key, &plan.by_name) {
                (PropKey::Name(_), Some(by_name)) => by_name.process(context, this, output)?,
                (PropKey::Path(path), _) => {
                    read_path_properties(context, this, path, output)?;
                }
                (PropKey::Name(_), None) => {}
            }
        }
        Ok(())
    }

    fn clone_box(&self) -> Box<dyn GraphStepFlatMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}
