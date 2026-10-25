//! Property indexes in the DSL (book page "Property Indexes"): plain calls on the source, not
//! steps, in both spellings: `g.create_index(#{..})`, `g.indexes()`, `g.drop_index(name)`.

use super::saved_query::{fail, graph_fail, optional_text};
use super::*;
use crate::error::ScriptError;
use crate::graph::definition::{Definition, DefinitionKind};
use crate::graph::{
    AccessRequest, Action, ElementKind, PropertyIndexDefinition, PropertyIndexTransform,
    PropertyIndexType, Resource,
};

/// The keys `create_index` accepts.
const KEYS: [&str; 6] = ["name", "element", "label", "key", "type", "transform"];

/// Why a storage that does not serve property indexes at all serves none.
const STORAGE_SERVES_NONE: &str = "this storage keeps the definition but serves no property \
                                   indexes; queries scan";

/// One index as a map: name, element, label, key, type, transform (`()` without one), served
/// (with `reason` when it is not served) and its current stats (entries, distinct_values,
/// skipped, bytes; zero when it is not served).
fn index_entry(
    definition: &Definition,
    stats: Option<crate::graph::PropertyIndexStats>,
) -> Option<rhai::Map> {
    let index = definition.as_property_index()?;
    let mut map = rhai::Map::new();
    map.insert("name".into(), definition.name().to_string().into());
    map.insert("element".into(), index.element_name().to_string().into());
    map.insert("label".into(), index.label().to_string().into());
    map.insert("key".into(), index.key().to_string().into());
    map.insert(
        "type".into(),
        index.index_type().as_str().to_string().into(),
    );
    map.insert(
        "transform".into(),
        index
            .transform()
            .map(|transform| Dynamic::from(transform.as_str().to_string()))
            .unwrap_or(Dynamic::UNIT),
    );
    map.insert("served".into(), Dynamic::from_bool(stats.is_some()));
    if stats.is_none() {
        let reason = index
            .unserved_reason()
            .unwrap_or_else(|| STORAGE_SERVES_NONE.to_string());
        map.insert("reason".into(), reason.into());
    }
    let stats = stats.unwrap_or_default();
    let int = |n: u64| Dynamic::from_int(i64::try_from(n).unwrap_or(i64::MAX));
    map.insert("entries".into(), int(stats.entries));
    map.insert("distinct_values".into(), int(stats.distinct_values));
    map.insert("skipped".into(), int(stats.skipped));
    map.insert("bytes".into(), int(stats.bytes));
    Some(map)
}

impl ScriptGraphTraversalSource {
    /// Asks the session's authorizer for `action` on the property index `name` (`None`: every
    /// index).
    fn require_index(
        &self,
        operation: &str,
        action: Action<'_>,
        name: Option<&str>,
    ) -> Result<(), Box<EvalAltResult>> {
        self.require(
            operation,
            AccessRequest::new(
                action,
                Resource::definition(Some(DefinitionKind::PROPERTY_INDEX), name),
            ),
        )
    }

    fn index_graph(&self, operation: &str) -> Result<&Arc<dyn AnyGraph>, Box<EvalAltResult>> {
        self.g.as_ref().ok_or_else(|| {
            Box::new(EvalAltResult::ErrorRuntime(
                Dynamic::from(format!("{operation}: no graph associated with this source")),
                rhai::Position::NONE,
            ))
        })
    }

    /// `g.create_index(#{label, key, element?, name?, type?, transform?})` /
    /// `g.create_index(..)`: declares (or replaces) a property index: the `element`s
    /// (`"vertex"`, the default, or `"edge"`) labelled `label`, ordered by their top-level
    /// property `key` (a plain name). `name` defaults to
    /// [`PropertyIndexDefinition::default_name`] (`<label>_<key>`, `_edge` for edges,
    /// `_to_lower` with that transform), `type` to `"memory-ordered"` (the only type this
    /// version knows), `transform` to none (`"to_lower"`: the lowered text of string values,
    /// for `has(key, __.to_lower().is(..))`). The index is built in the same
    /// unit. Asks for `Create` on the index (`Update` when it replaces one); a storage without
    /// property indexes refuses it, and so does
    /// [`PropertyIndexDefinition::check_create`] (an unknown type, a key the stored schema
    /// declares as values the type cannot hold).
    pub fn create_index(&mut self, spec: Dynamic) -> Result<(), Box<EvalAltResult>> {
        let operation = "create_index";
        let actual = type_name_of(&spec);
        let mismatch = |expected: String, actual: String| {
            fail(ScriptError::ArgumentMismatch {
                step_name: operation.to_string(),
                expected,
                actual,
            })
        };
        let Some(mut spec) = spec.try_cast::<rhai::Map>() else {
            return Err(mismatch(
                "a map #{label, key, element?, name?, type?, transform?}".into(),
                actual,
            ));
        };
        if let Some(unknown) = spec.keys().find(|key| !KEYS.contains(&key.as_str())) {
            return Err(mismatch(
                format!("only the keys {}", KEYS.join(", ")),
                format!("the key '{unknown}'"),
            ));
        }
        let mut take = |key: &str| spec.remove(key).unwrap_or(Dynamic::UNIT);
        let label = optional_text(operation, "label", take("label"))?.ok_or_else(|| {
            mismatch(
                "the key label (the vertex or edge label)".into(),
                "no label".into(),
            )
        })?;
        let key = optional_text(operation, "key", take("key"))?.ok_or_else(|| {
            mismatch(
                "the key key (the top-level property name)".into(),
                "no key".into(),
            )
        })?;
        let element = match optional_text(operation, "element", take("element"))?.as_deref() {
            None | Some("vertex") => ElementKind::Vertex,
            Some("edge") => ElementKind::Edge,
            Some(other) => {
                return Err(mismatch(
                    "element \"vertex\" or \"edge\"".into(),
                    format!("\"{other}\""),
                ));
            }
        };
        let index_type = match optional_text(operation, "type", take("type"))? {
            None => PropertyIndexType::MemoryOrdered,
            Some(name) => PropertyIndexType::from_name(&name),
        };
        let transform = optional_text(operation, "transform", take("transform"))?
            .map(|name| PropertyIndexTransform::from_name(&name));
        let index = PropertyIndexDefinition::new(element, label, key)
            .with_type(index_type)
            .with_transform(transform);
        let name = match optional_text(operation, "name", take("name"))? {
            Some(name) => name,
            None => index.default_name(),
        };
        let definition = Definition::property_index(name.as_str(), index.clone());
        definition
            .validate()
            .map_err(|err| graph_fail(crate::error::GraphError::from(err)))?;
        let graph = self.index_graph(operation)?;
        let exists = graph
            .definition(DefinitionKind::PROPERTY_INDEX, &name)
            .is_some();
        let action = if exists {
            Action::Update
        } else {
            Action::Create
        };
        self.require_index(operation, action, Some(&name))?;
        if !graph.capabilities().property_indexes {
            return Err(graph_fail(crate::error::GraphError::unsupported(
                "property indexes",
            )));
        }
        index
            .check_create(&name, graph.get_schema().as_ref())
            .map_err(|err| graph_fail(crate::error::GraphError::from(err)))?;
        graph
            .set_definitions(vec![definition])
            .map_err(script_error)
    }

    /// `g.indexes()`: every property index as a map (name, element, label, key, type,
    /// transform, served, `reason` when it is not served, entries, distinct_values, skipped,
    /// bytes), by name. Asks for
    /// `Read` on the indexes.
    pub fn indexes(&mut self) -> Result<rhai::Array, Box<EvalAltResult>> {
        self.require_index("indexes", Action::Read, None)?;
        let graph = self.index_graph("indexes")?;
        let served = graph.property_indexes();
        Ok(graph
            .definitions(Some(DefinitionKind::PROPERTY_INDEX))
            .iter()
            .filter_map(|definition| {
                let stats = served
                    .iter()
                    .find(|info| info.name == definition.name())
                    .map(|info| info.stats);
                index_entry(definition, stats)
            })
            .map(Dynamic::from_map)
            .collect())
    }

    /// `g.drop_index(name)`: drops the property index; `false` when there
    /// is none. Asks for `Delete` on it.
    pub fn drop_index(&mut self, name: &str) -> Result<bool, Box<EvalAltResult>> {
        self.require_index("drop_index", Action::Delete, Some(name))?;
        self.index_graph("drop_index")?
            .remove_definition(DefinitionKind::PROPERTY_INDEX, name)
            .map_err(script_error)
    }
}
