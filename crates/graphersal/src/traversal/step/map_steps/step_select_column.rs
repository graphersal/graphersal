//! `select(Column::Keys)` / `select(Column::Values)`: TinkerPop's `select(Column)`.
//!
//! Unlike label-based `select("a")` this step never reads `Traverser.path`, so it keeps the
//! default `path_requirement()` (`None`) and the path analysis records nothing for it.
//!
//! # Map versus entry
//!
//! TinkerPop distinguishes a `Map` from a `Map.Entry`; so does Graphersal:
//!
//! - a map is a `TraverserValue::Object` or `TraverserValue::Record` (`group()`,
//!   `group_count()`, `project()`, `element_map()`, `select("a", "b")`, a nested object property):
//!   `select(Column::Keys)` emits ONE traverser holding a list of all its keys, and
//!   `select(Column::Values)` one list of all its values, whatever the number of entries — a
//!   one-group `group()` result still yields a one-element list;
//! - an entry is a `TraverserValue::Entry`, produced only by `unfold()` of a map: the step
//!   emits that entry's key or value as a scalar.
//!
//! - a path (`TraverserValue::Path`, from `path()`): `select(Column::Keys)` emits ONE traverser
//!   holding a list with one list of step labels per position, in position order (TinkerPop
//!   `Path.labels()`); an unlabelled position is an empty list. The labels of one position keep
//!   the order of the `as()` calls. `select(Column::Values)` emits ONE traverser holding the
//!   list of the path's objects, in position order (TinkerPop `Path.objects()`; a
//!   `by()`-modulated path holds the modulated values), so keys and values line up by index.
//!
//! Anything else fails with a cast error whose help names `unfold()` and `select(Column.keys)`.

use crate::graph::GraphStorage;
use crate::graph::schema::value_type_name;
use crate::*;

#[derive(Clone)]
struct StepSelectColumn {
    column: Column,
}

impl fmt::Display for StepSelectColumn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "select(Column.{})", self.column)
    }
}

/// The physical `select(Column)` step, shared by the source and anonymous builders.
pub(super) fn select_column_step<'g, S: GraphStorage + 'g>(column: Column) -> GraphStep<'g, S> {
    GraphStep::Map("select", Box::new(StepSelectColumn { column }))
}

/// The cast error of `select(Column)` on a value that is neither a map nor an entry.
fn select_column_error(actual: String) -> Box<TraverserError> {
    Box::new(TraverserError::cast_exception_target(
        crate::error::SELECT_COLUMN_EXPECTED,
        actual,
        None,
        None,
    ))
}

impl StepSelectColumn {
    /// Picks the requested half out of a map value; hands `value` back when it is not map-shaped.
    fn pick_from_map<S: GraphStorage>(
        &self,
        value: TraverserValue<S>,
    ) -> Result<TraverserValue<S>, TraverserValue<S>> {
        match value {
            TraverserValue::Entry(entry) => {
                let (key, value) = *entry;
                Ok(match self.column {
                    Column::Keys => key,
                    Column::Values => value,
                })
            }
            TraverserValue::Object(obj) => Ok(TraverserValue::Array(match self.column {
                Column::Keys => obj.0.into_keys().collect(),
                Column::Values => obj.0.into_values().collect(),
            })),
            TraverserValue::Record { keys, values } => {
                Ok(TraverserValue::Array(match self.column {
                    Column::Keys => keys
                        .iter()
                        .map(|k| {
                            TraverserValue::Computed(ElementProperty::String(k.to_compact()), None)
                        })
                        .collect(),
                    Column::Values => values,
                }))
            }
            TraverserValue::Computed(ElementProperty::Object(obj), _) => {
                self.pick_from_map(TraverserValue::from(ElementProperty::Object(obj)))
            }
            TraverserValue::Computed(ElementProperty::Map(map), _) => {
                self.pick_from_map(TraverserValue::from(ElementProperty::Map(map)))
            }
            TraverserValue::Path(path) => Ok(match self.column {
                Column::Keys => TraverserValue::path_labels(&path),
                Column::Values => {
                    TraverserValue::Array(path.steps.into_iter().map(|step| step.value).collect())
                }
            }),
            other => Err(other),
        }
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepSelectColumn {
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        // A lazy `values(key)` reference resolves first: a nested object property is a map,
        // the same way `select("key")` reads one; any other stored value is reported by its type.
        let stored = match &source.value {
            TraverserValue::VertexProperty(prop, index, _) => Some(
                context
                    .storage
                    .vertex_properties(*index)?
                    .and_then(|p| p.get(prop.as_str())),
            ),
            TraverserValue::EdgeProperty(prop, index, _) => Some(
                context
                    .storage
                    .edge_properties(*index)?
                    .and_then(|p| p.get(prop.as_str())),
            ),
            _ => None,
        };
        let value = match stored {
            Some(Some(property @ (ElementProperty::Object(_) | ElementProperty::Map(_)))) => {
                TraverserValue::from(property)
            }
            Some(Some(property)) => return Err(select_column_error(value_type_name(property))),
            Some(None) => return Err(select_column_error("null".to_string())),
            None => source.value,
        };

        match self.pick_from_map(value) {
            Ok(value) => Ok(Traverser::with_parts(
                value,
                source.path,
                source.bulk,
                source.sack,
            )),
            Err(TraverserValue::Computed(value, _)) => {
                Err(select_column_error(value_type_name(&value)))
            }
            Err(other) => Err(select_column_error(other.type_name().to_string())),
        }
    }

    /// A whole map yields a newly built list of its keys/values.
    fn materializes(&self) -> bool {
        true
    }

    fn clone_box(&self) -> Box<dyn GraphStepMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}
