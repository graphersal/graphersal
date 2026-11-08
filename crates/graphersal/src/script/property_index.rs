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
const KEYS: [&str; 7] = [
    "name",
    "element",
    "label",
    "key",
    "type",
    "transform",
    "unique",
];

/// Why a storage that does not serve property indexes at all serves none.
const STORAGE_SERVES_NONE: &str = "this storage keeps the definition but serves no property \
                                   indexes; queries scan";

/// A transform as the DSL shows it: its name (`"to_lower"`, `"each"`), a chain as the array of
/// its names (`["each", "to_lower"]`).
fn transform_value(transform: &PropertyIndexTransform) -> Dynamic {
    match transform {
        PropertyIndexTransform::Chain(chain) => Dynamic::from_array(
            chain
                .steps()
                .iter()
                .map(|step| Dynamic::from(step.as_str().to_string()))
                .collect(),
        ),
        PropertyIndexTransform::ToLower
        | PropertyIndexTransform::Each
        | PropertyIndexTransform::Unknown(_) => Dynamic::from(transform.as_str().to_string()),
    }
}

/// The `transform` of `create_index`: `()` (none), a name, or a chain as an array of names
/// applied in order (`["each", "to_lower"]`).
fn transform_of(
    operation: &str,
    value: Dynamic,
) -> Result<Option<PropertyIndexTransform>, Box<EvalAltResult>> {
    let mismatch = |actual: String| {
        fail(ScriptError::ArgumentMismatch {
            step_name: operation.to_string(),
            expected: "transform as a name (\"to_lower\", \"each\"), an array of names applied \
                       in order ([\"each\", \"to_lower\"]) or () for none"
                .to_string(),
            actual,
        })
    };
    if value.is_array() {
        let actual = type_name_of(&value);
        let Some(items) = value.try_cast::<rhai::Array>() else {
            return Err(mismatch(actual));
        };
        let mut names = Vec::with_capacity(items.len());
        for item in items {
            if !item.is_string() {
                return Err(mismatch(format!(
                    "an array holding {}",
                    type_name_of(&item)
                )));
            }
            names.push(item.to_string());
        }
        return PropertyIndexTransform::from_names(&names)
            .map(Some)
            .ok_or_else(|| mismatch("an empty array".to_string()));
    }
    if value.is_unit() || value.is_string() {
        return Ok(optional_text(operation, "transform", value)?
            .map(|name| PropertyIndexTransform::from_name(&name)));
    }
    Err(mismatch(type_name_of(&value)))
}

/// One index as a map: name, element, label (`()` for a label-less index), key, type,
/// transform (`()` without one; a chain as the array of its names), unique, served
/// (with `reason` when it is not served: a unique index over repeated values says
/// `duplicates: ..`) and its current stats (entries, distinct_values, skipped, bytes; zero
/// when it is not served).
fn index_entry(
    definition: &Definition,
    stats: Option<crate::graph::PropertyIndexStats>,
    duplicates: Option<crate::graph::UniqueIndexDuplicates>,
) -> Option<rhai::Map> {
    let index = definition.as_property_index()?;
    let mut map = rhai::Map::new();
    map.insert("name".into(), definition.name().to_string().into());
    map.insert("element".into(), index.element_name().to_string().into());
    map.insert(
        "label".into(),
        index
            .label()
            .map(|label| Dynamic::from(label.to_string()))
            .unwrap_or(Dynamic::UNIT),
    );
    map.insert("key".into(), index.key().to_string().into());
    map.insert(
        "type".into(),
        index.index_type().as_str().to_string().into(),
    );
    map.insert(
        "transform".into(),
        index
            .transform()
            .map(transform_value)
            .unwrap_or(Dynamic::UNIT),
    );
    map.insert("unique".into(), Dynamic::from_bool(index.is_unique()));
    map.insert("served".into(), Dynamic::from_bool(stats.is_some()));
    if stats.is_none() {
        let reason = index
            .unserved_reason()
            .or_else(|| duplicates.map(|report| report.unserved_reason()))
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
    /// Asks the session's authorizer for `action` on the property index `name` of catalog
    /// `kind` ([`DefinitionKind::PROPERTY_INDEX`] or, for a label-less index,
    /// [`DefinitionKind::PROPERTY_INDEX_EXT`]; `name` `None`: every index of the kind).
    fn require_index(
        &self,
        operation: &str,
        action: Action<'_>,
        kind: DefinitionKind,
        name: Option<&str>,
    ) -> Result<(), Box<EvalAltResult>> {
        self.require(
            operation,
            AccessRequest::new(action, Resource::definition(Some(kind), name)),
        )
    }

    /// The catalog kind holding the property index `name`, if any (index names are unique
    /// across both index kinds).
    fn index_kind(
        &self,
        graph: &Arc<dyn AnyGraph>,
        name: &str,
    ) -> Result<Option<DefinitionKind>, Box<EvalAltResult>> {
        for kind in [
            DefinitionKind::PROPERTY_INDEX,
            DefinitionKind::PROPERTY_INDEX_EXT,
        ] {
            if graph.definition(kind, name).map_err(graph_fail)?.is_some() {
                return Ok(Some(kind));
            }
        }
        Ok(None)
    }

    fn index_graph(&self, operation: &str) -> Result<&Arc<dyn AnyGraph>, Box<EvalAltResult>> {
        self.g.as_ref().ok_or_else(|| {
            Box::new(EvalAltResult::ErrorRuntime(
                Dynamic::from(format!("{operation}: no graph associated with this source")),
                rhai::Position::NONE,
            ))
        })
    }

    /// `g.create_index(#{key, label?, element?, name?, type?, transform?, unique?})` /
    /// `g.create_index(..)`: declares (or replaces) a property index: the `element`s
    /// (`"vertex"`, the default, or `"edge"`) labelled `label`, ordered by their top-level
    /// property `key` (a plain name). Without `label` (omitted or `()`) the index is
    /// LABEL-LESS: every element of the kind, unlabeled ones included
    /// ([`PropertyIndexDefinition::label_less`]). `name` defaults to
    /// [`PropertyIndexDefinition::default_name`] (`<label>_<key>`, `<key>` label-less, `_edge`
    /// for edges, `_to_lower` with that transform, `_each`, `_each_to_lower`), `type` to
    /// `"memory-ordered"` (the only type this version knows), `transform` to none
    /// (`"to_lower"`: the lowered text of string values, for `has(key, __.to_lower().is(..))`;
    /// `"each"`: every element of an array value, for `has(key, __.unfold().is(..))`; the
    /// chain `["each", "to_lower"]` for `has(key, __.unfold().to_lower().is(..))`). The index is
    /// built in the same unit. `unique: true` makes it a unique constraint (a write that
    /// would repeat a key fails; refused over data that already repeats one, and with
    /// `each`). Asks for `Create` on the index (`Update` when it replaces one; of kind
    /// `property_index_ext` for a label-less index, a unique one or one with `each`); a storage without
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
                "a map #{key, label?, element?, name?, type?, transform?, unique?}".into(),
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
        // No label: a label-less index (every element of the kind).
        let label = optional_text(operation, "label", take("label"))?;
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
        let transform = transform_of(operation, take("transform"))?;
        let unique = take("unique");
        let unique = if unique.is_unit() {
            false
        } else {
            let actual = type_name_of(&unique);
            unique
                .as_bool()
                .map_err(|_| mismatch("unique as a boolean (true, false) or ()".into(), actual))?
        };
        let index = match label {
            Some(label) => PropertyIndexDefinition::new(element, label, key),
            None => PropertyIndexDefinition::label_less(element, key),
        }
        .with_type(index_type)
        .with_transform(transform)
        .with_unique(unique);
        let name = match optional_text(operation, "name", take("name"))? {
            Some(name) => name,
            None => index.default_name(),
        };
        let definition = Definition::property_index(name.as_str(), index.clone());
        definition
            .validate()
            .map_err(|err| graph_fail(crate::error::GraphError::from(err)))?;
        let graph = self.index_graph(operation)?;
        let existing = self.index_kind(graph, &name)?;
        // Replacing an index of the other kind deletes that one.
        if let Some(kind) = existing.filter(|kind| *kind != definition.kind()) {
            self.require_index(operation, Action::Delete, kind, Some(&name))?;
        }
        let action = if existing == Some(definition.kind()) {
            Action::Update
        } else {
            Action::Create
        };
        self.require_index(operation, action, definition.kind(), Some(&name))?;
        if !graph.capabilities().map_err(graph_fail)?.property_indexes {
            return Err(graph_fail(crate::error::GraphError::unsupported(
                "property indexes",
            )));
        }
        index
            .check_create(&name, graph.get_schema().map_err(graph_fail)?.as_ref())
            .map_err(|err| graph_fail(crate::error::GraphError::from(err)))?;
        // One unit: a unique index over repeated values is refused and leaves nothing.
        graph.create_index(&name, index).map_err(script_error)
    }

    /// `g.indexes()`: every property index as a map (name, element, label, key, type,
    /// transform, unique, served, `reason` when it is not served, entries, distinct_values, skipped,
    /// bytes), by name. Asks for
    /// `Read` on the indexes.
    pub fn indexes(&mut self) -> Result<rhai::Array, Box<EvalAltResult>> {
        self.require_index(
            "indexes",
            Action::Read,
            DefinitionKind::PROPERTY_INDEX,
            None,
        )?;
        self.require_index(
            "indexes",
            Action::Read,
            DefinitionKind::PROPERTY_INDEX_EXT,
            None,
        )?;
        let graph = self.index_graph("indexes")?;
        let served = graph.property_indexes().map_err(graph_fail)?;
        let mut definitions = graph
            .definitions(Some(DefinitionKind::PROPERTY_INDEX))
            .map_err(graph_fail)?;
        definitions.extend(
            graph
                .definitions(Some(DefinitionKind::PROPERTY_INDEX_EXT))
                .map_err(graph_fail)?,
        );
        definitions.sort_by(|a, b| a.name().cmp(b.name()));
        let mut entries = rhai::Array::with_capacity(definitions.len());
        for definition in &definitions {
            let stats = served
                .iter()
                .find(|info| info.name == definition.name())
                .map(|info| info.stats);
            let duplicates = match stats {
                Some(_) => None,
                None => graph
                    .unique_index_duplicates(definition.name())
                    .map_err(graph_fail)?,
            };
            if let Some(entry) = index_entry(definition, stats, duplicates) {
                entries.push(Dynamic::from_map(entry));
            }
        }
        Ok(entries)
    }

    /// `g.drop_index(name)`: drops the property index (per-label or label-less); `false` when
    /// there is none. Asks for `Delete` on it (of the kind holding it; `property_index` when
    /// there is none).
    pub fn drop_index(&mut self, name: &str) -> Result<bool, Box<EvalAltResult>> {
        let graph = Arc::clone(self.index_graph("drop_index")?);
        let kind = self
            .index_kind(&graph, name)?
            .unwrap_or(DefinitionKind::PROPERTY_INDEX);
        self.require_index("drop_index", Action::Delete, kind, Some(name))?;
        graph.remove_definition(kind, name).map_err(script_error)
    }
}
