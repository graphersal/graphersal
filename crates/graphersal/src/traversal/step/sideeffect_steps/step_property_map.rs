//! `property(map)` / `property(Cardinality, map)`: one property write per map entry.

use super::step_property::check_cardinality;
use crate::graph::GraphStorage;
use crate::*;

/// One entry of a `property(map)` write: the key, the value, and the entry's own cardinality
/// (`Cardinality.single(v)` in the DSL), which wins over the step's default cardinality.
#[derive(Clone, Debug)]
pub(crate) struct PropertyEntry {
    pub(crate) key: CompactString,
    pub(crate) value: ElementProperty,
    pub(crate) cardinality: Option<Cardinality>,
}

/// Checks a `property(map)` key: keys that start with the reserved `T.` / `Direction.` prefixes
/// or with `~` (a hidden key) are rejected, so a token key is never silently written as data.
pub(crate) fn check_property_key(key: &str) -> Result<(), String> {
    if key.starts_with("T.") || key.starts_with("Direction.") {
        Err(format!(
            "map key \"{key}\" is reserved for a token and cannot be written by property(map); \
             use property(T.id, v) / property(T.label, v) right after add_v() instead"
        ))
    } else if key.starts_with('~') {
        Err(format!("Property key can not be a hidden key: {key}"))
    } else {
        Ok(())
    }
}

/// Where a property write goes.
#[derive(Clone, Copy)]
pub(crate) enum WriteTarget<S: GraphStorage> {
    Vertex(S::VertexId),
    Edge(S::EdgeId),
}

/// Writes `entries` to `target`, each with its own cardinality or else `default`. Every
/// cardinality is checked before the first write, so a rejected entry leaves the element
/// unchanged (no half-applied map).
pub(crate) fn write_entries<S: GraphStorage>(
    storage: &mut S,
    target: WriteTarget<S>,
    entries: &[PropertyEntry],
    default: Option<Cardinality>,
    step: &str,
) -> TraverserResult<()> {
    let on_edge = matches!(target, WriteTarget::Edge(_));
    for entry in entries {
        check_cardinality(step, entry.cardinality.or(default), on_edge)?;
    }
    for entry in entries {
        match target {
            WriteTarget::Vertex(node) => {
                storage.set_vertex_property(node, entry.key.clone(), entry.value.clone())?
            }
            WriteTarget::Edge(edge) => {
                storage.set_edge_property(edge, entry.key.clone(), entry.value.clone())?
            }
        }
    }
    Ok(())
}

#[derive(Clone)]
struct StepPropertyMap {
    entries: Vec<PropertyEntry>,
    default: Option<Cardinality>,
    /// A construction-time problem with the argument (not a map, a reserved key), raised when a
    /// traverser reaches the step, since builders cannot fail.
    invalid: Option<String>,
}

impl fmt::Display for StepPropertyMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("property(")?;
        if let Some(cardinality) = self.default {
            write!(f, "Cardinality.{cardinality}, ")?;
        }
        let mut map = crate::value::PropertyMap::default();
        for entry in &self.entries {
            map.insert(entry.key.clone(), entry.value.clone());
        }
        f.write_str(&crate::value::literal::rhai_literal(
            &ElementProperty::Object(Box::new(map)),
        ))?;
        f.write_str(")")
    }
}

impl StepPropertyMap {
    fn from_entries(entries: Vec<PropertyEntry>, default: Option<Cardinality>) -> Self {
        let invalid = entries
            .iter()
            .find_map(|entry| check_property_key(&entry.key).err());
        Self {
            entries,
            default,
            invalid,
        }
    }

    fn from_value(map: ElementProperty, default: Option<Cardinality>) -> Self {
        match map {
            ElementProperty::Null => Self::from_entries(Vec::new(), default),
            ElementProperty::Object(object) => Self::from_entries(
                object
                    .into_iter()
                    .map(|(key, value)| PropertyEntry {
                        key,
                        value,
                        cardinality: None,
                    })
                    .collect(),
                default,
            ),
            other => Self {
                entries: Vec::new(),
                default,
                invalid: Some(format!(
                    "property(map) takes a map or null, got {}",
                    crate::graph::type_of(&other)
                        .map(|ty| ty.to_string())
                        .unwrap_or_else(|| "unknown".to_string())
                )),
            },
        }
    }

    fn into_step<'g, S: GraphStorage + 'g>(self) -> GraphStep<'g, S> {
        GraphStep::SideEffectMut("property", Box::new(self))
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `property(map)`: writes one property per entry of `map` (an [`ElementProperty::Object`]),
    /// in map order; [`ElementProperty::Null`] or an empty map writes nothing. Keys starting with
    /// `T.`, `Direction.` or `~` are rejected when the traversal executes.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let mut graph = TraversalGraph::new();
    /// let mut map = PropertyMap::default();
    /// map.insert("name", "foo".into());
    /// map.insert("age", 42.into());
    /// graph.traversal_mut().add_v(None).property_map(ElementProperty::Object(Box::new(map))).next().unwrap();
    /// assert_eq!(graph.traversal().v(None).has("name", "foo").count().next().unwrap(), Some(ElementProperty::from(1)));
    /// ```
    pub fn property_map(self, map: ElementProperty) -> Self {
        self.push_step(StepPropertyMap::from_value(map, None).into_step())
    }

    /// `property(Cardinality, map)`: [`Self::property_map`] with a cardinality for every entry.
    /// `Cardinality::List`/`Set` fail with [`TraverserError::UnsupportedCardinality`] as soon as
    /// an entry would be written; on an edge any cardinality fails.
    pub fn property_map_with(self, cardinality: Cardinality, map: ElementProperty) -> Self {
        self.push_step(StepPropertyMap::from_value(map, Some(cardinality)).into_step())
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `property(map)`; see [`GraphTraversalSource::property_map`].
    pub fn property_map(self, map: ElementProperty) -> Self {
        self.push_step(StepPropertyMap::from_value(map, None).into_step())
    }

    /// `property(Cardinality, map)`; see [`GraphTraversalSource::property_map_with`].
    pub fn property_map_with(self, cardinality: Cardinality, map: ElementProperty) -> Self {
        self.push_step(StepPropertyMap::from_value(map, Some(cardinality)).into_step())
    }

    /// `property([Cardinality,] map)` with a cardinality per entry.
    #[cfg_attr(not(feature = "script"), allow(dead_code))]
    pub(crate) fn property_entries(
        self,
        default: Option<Cardinality>,
        entries: Vec<PropertyEntry>,
    ) -> Self {
        self.push_step(StepPropertyMap::from_entries(entries, default).into_step())
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepSideEffectMut<'g, S> for StepPropertyMap {
    fn property_access(
        &self,
        visit: &mut dyn FnMut(Option<&str>, crate::graph::workload::AccessKind),
    ) {
        for entry in &self.entries {
            visit(Some(&entry.key), crate::graph::workload::AccessKind::Write);
        }
    }
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Option<Traverser<S>>,
    ) -> TraverserResult<Vec<Traverser<S>>> {
        // No incoming traverser (an empty stream): nothing to write to, nothing to emit.
        let Some(source) = source else {
            return Ok(Vec::new());
        };
        if let Some(reason) = &self.invalid {
            return Err(Box::new(TraverserError::QueryExecution {
                query: "property(map)".to_string(),
                reason: reason.clone(),
            }));
        }
        let target: WriteTarget<S> = match &source.value {
            TraverserValue::Vertex(node) => WriteTarget::Vertex(*node),
            TraverserValue::Edge(edge) => WriteTarget::Edge(*edge),
            other => {
                return Err(Box::new(TraverserError::cast_exception(
                    &["vertex", "edge"],
                    other,
                )));
            }
        };
        write_entries(
            context.storage,
            target,
            &self.entries,
            self.default,
            "property",
        )?;
        Ok(vec![source])
    }

    fn extends_path(&self) -> bool {
        false
    }

    fn clone_box(&self) -> Box<dyn GraphStepSideEffectMut<'g, S> + 'g> {
        Box::new(self.clone())
    }
}
