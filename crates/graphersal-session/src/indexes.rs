//! Property indexes in the session (`book/src/users_guide/property_indexes.md`; book page
//! "Property Indexes"): the operations behind the playground's Catalog ▸ Property indexes
//! manager, the dev server's `/api/indexes*` endpoints and its MCP tools `list_indexes`,
//! `create_index`, `drop_index`.
//!
//! Every operation works on the catalog of the graph the session serves (a Store's graph too:
//! a create or drop is a commit like any other, journaled and recorded in the dev server's
//! change feed). The index ENTRIES are never stored; the graph builds them from the data.
//!
//! JSON forms:
//!
//! - an **entry** (`list_indexes`): `{name, element: "vertex"|"edge", label, key, type,
//!   transform, served, reason, stats: {entries, distinctValues, skipped, bytes}}`;
//!   `transform` is `null` for an index of the values themselves (else `"to_lower"`, or the
//!   unknown name as read); `served` is false and `stats` `null` when the index is not served,
//!   and `reason` (else `null`) says why: the storage does not serve indexes (it keeps the
//!   definition in its catalog, no query reads it), or the index's `type`, `transform` or
//!   `element` is not known to this version (a newer one wrote it,
//!   [`PropertyIndexDefinition::unserved_reason`]); `skipped` counts elements whose non-null
//!   value the index does not hold (a `to_lower` index is used by queries only while it is 0);
//!   `bytes` is the storage's estimate of the index's memory;
//! - a **spec** (`create_index`): `{label, key, element?, name?, type?, transform?}`;
//!   `element` defaults to `"vertex"`, `name` to `<label>_<key>` (`<label>_<key>_edge` for an
//!   edge index, then `_to_lower` with that transform,
//!   [`PropertyIndexDefinition::default_name`]), `type` to `"memory-ordered"`
//!   ([`index_types`] lists the known ones), `transform` to none ([`index_transforms`]);
//! - **field errors** (a refused create): `[{field, message, help?}]` with `field` one of
//!   `name`, `element`, `label`, `key`, `type`, `transform` or `general`, the same shape as the
//!   other catalog editors. An unknown `type` is an error of `type`, an unknown `transform` of
//!   `transform`; a key the graph's schema declares as values the index cannot hold (a vector,
//!   an array, an object; with `to_lower` anything but a string;
//!   [`PropertyIndexDefinition::check_create`]) an error of `key`. A storage without property
//!   indexes refuses every create with its `Unsupported` diagnostic (field `general`), as
//!   `recompress` does on a storage without compressed properties.

use crate::error::SessionError;
use graphersal::catalog::{
    Definition, DefinitionKind, PropertyIndexDefinition, PropertyIndexTransform, PropertyIndexType,
};
use graphersal::prelude::*;
use graphersal::storage::{ElementKind, PropertyIndexInfo, PropertyIndexStats};
use serde_json::{Value, json};

use crate::Session;
use crate::queries::FieldError;

/// The keys a spec may have.
const KEYS: [&str; 6] = ["name", "element", "label", "key", "type", "transform"];

/// Why a storage that serves no property indexes at all does not serve one.
const STORAGE_SERVES_NONE: &str =
    "this storage keeps the definition but serves no property indexes; queries scan";

/// The index types this version knows, for a form's choice: `[{name, description}]`, the
/// default first.
pub fn index_types() -> Value {
    Value::Array(
        PropertyIndexType::KNOWN_NAMES
            .iter()
            .map(|name| {
                let ty = PropertyIndexType::from_name(name);
                json!({"name": name, "description": ty.description()})
            })
            .collect(),
    )
}

/// The index transforms this version knows, for a form's choice: `[{name, description}]`
/// (no transform, the default, is not listed).
pub fn index_transforms() -> Value {
    Value::Array(
        PropertyIndexTransform::KNOWN_NAMES
            .iter()
            .map(|name| {
                let transform = PropertyIndexTransform::from_name(name);
                json!({"name": name, "description": transform.description()})
            })
            .collect(),
    )
}

/// The entry of one index (module docs).
fn entry(name: &str, index: &PropertyIndexDefinition, stats: Option<PropertyIndexStats>) -> Value {
    let served = stats.is_some();
    let reason = (!served).then(|| {
        index
            .unserved_reason()
            .unwrap_or_else(|| STORAGE_SERVES_NONE.to_string())
    });
    let stats = stats.map(|stats| {
        json!({
            "entries": stats.entries,
            "distinctValues": stats.distinct_values,
            "skipped": stats.skipped,
            "bytes": stats.bytes,
        })
    });
    json!({
        "name": name,
        "element": index.element_name(),
        "label": index.label(),
        "key": index.key(),
        "type": index.index_type().as_str(),
        "transform": index.transform().map(PropertyIndexTransform::as_str),
        "served": served,
        "reason": reason,
        "stats": stats,
    })
}

/// The form field a catalog error of a property index belongs to.
fn field_of(err: &DefinitionError) -> FieldError {
    let mut error = FieldError::of(err);
    match err {
        DefinitionError::InvalidField { field, .. }
            if matches!(field.as_str(), "label" | "key" | "type" | "transform") =>
        {
            error.field = field.clone();
        }
        DefinitionError::IndexTypeMismatch { .. } => error.field = "key".to_string(),
        _ => {}
    }
    error
}

impl Session {
    /// The property indexes of the graph, by name, with their current stats (module docs).
    /// Fails when the graph cannot be read ([`SessionError::Graph`]).
    pub fn list_indexes(&self) -> Result<Value, SessionError> {
        Ok(self.graph.indexes()?)
    }

    /// The index `name` as its entry, `null` when there is none. Fails like
    /// [`Self::list_indexes`].
    pub fn get_index(&self, name: &str) -> Result<Value, SessionError> {
        Ok(self.graph.index(name)?)
    }

    /// How many property indexes the graph holds. Fails like [`Self::list_indexes`].
    pub fn index_count(&self) -> Result<usize, SessionError> {
        Ok(self.graph.count_kind(DefinitionKind::PROPERTY_INDEX)?)
    }

    /// Creates a property index from a spec (module docs) in ONE commit, which also builds it.
    /// An existing index of that name is not overwritten (drop it first). Answers `{ok: true,
    /// index: <entry>}` or `{ok: false, errors: [{field, message, help?}]}`, never fails
    /// otherwise.
    pub fn create_index(&mut self, spec: &Value) -> Value {
        match self.create_index_checked(spec) {
            Ok(entry) => json!({"ok": true, "index": entry}),
            Err(errors) => json!({
                "ok": false,
                "errors": errors.iter().map(FieldError::to_value).collect::<Vec<_>>(),
            }),
        }
    }

    fn create_index_checked(&mut self, spec: &Value) -> Result<Value, Vec<FieldError>> {
        let definition = index_definition(spec)?;
        let name = definition.name().to_string();
        self.graph.store_index(definition)?;
        self.get_index(&name)
            .map_err(|err| vec![FieldError::new("general", err.to_string())])
    }

    /// Drops the property index `name`: `true`, or `false` when there was none.
    pub fn drop_index(&mut self, name: &str) -> Result<bool, SessionError> {
        self.graph
            .drop_definition(DefinitionKind::PROPERTY_INDEX, name)
    }
}

/// The checked definition of a spec (module docs): every wrong field reported at once.
pub(crate) fn index_definition(spec: &Value) -> Result<Definition, Vec<FieldError>> {
    let Some(object) = spec.as_object() else {
        return Err(vec![FieldError::new(
            "general",
            "the index is a JSON object {label, key, element?, name?, type?, transform?}",
        )]);
    };
    if let Some(unknown) = object.keys().find(|key| !KEYS.contains(&key.as_str())) {
        return Err(vec![FieldError::new(
            "general",
            format!("unknown key '{unknown}' (the keys are {})", KEYS.join(", ")),
        )]);
    }
    let mut errors = Vec::new();
    let mut text = |key: &str| match spec.get(key) {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) if text.trim().is_empty() => None,
        Some(Value::String(text)) => Some(text.trim().to_string()),
        Some(_) => {
            errors.push(FieldError::new(key, format!("{key} must be a string")));
            None
        }
    };
    let name = text("name");
    let label = text("label").unwrap_or_default();
    let key = text("key").unwrap_or_default();
    let element = text("element");
    let index_type = text("type");
    let transform = text("transform");
    if label.is_empty() {
        errors.push(FieldError::new(
            "label",
            "name the vertex or edge label the index covers, for example person",
        ));
    }
    if key.is_empty() {
        errors.push(FieldError::new(
            "key",
            "name the top-level property the index orders by, for example age",
        ));
    }
    let element = match element.as_deref() {
        None | Some("vertex") => ElementKind::Vertex,
        Some("edge") => ElementKind::Edge,
        Some(other) => {
            errors.push(FieldError::new(
                "element",
                format!("'{other}' is not vertex or edge"),
            ));
            ElementKind::Vertex
        }
    };
    if !errors.is_empty() {
        return Err(errors);
    }
    let index = PropertyIndexDefinition::new(element, label, key)
        .with_type(index_type.as_deref().map_or(
            PropertyIndexType::MemoryOrdered,
            PropertyIndexType::from_name,
        ))
        .with_transform(transform.as_deref().map(PropertyIndexTransform::from_name));
    let name = name.unwrap_or_else(|| index.default_name());
    // The name and the index each on their own, so both wrong fields are reported (the index
    // checked under its default name when the given one is wrong).
    let mut errors = Vec::new();
    let probe = PropertyIndexDefinition::new(element, "probe", "probe");
    let mut checked_as = name.clone();
    if let Err(err) = Definition::property_index(name.as_str(), probe).validate() {
        errors.push(field_of(&err));
        checked_as = index.default_name();
    }
    if let Err(err) = Definition::property_index(checked_as, index.clone()).validate() {
        errors.push(field_of(&err));
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(Definition::property_index(name, index))
}

/// The property indexes of `g`, by name, as their entries (module docs).
pub fn indexes_in<S: GraphStorage>(g: &S) -> Value {
    let served = g.property_indexes();
    Value::Array(
        g.definitions()
            .filter_map(|definition| {
                let index = definition.as_property_index()?;
                Some(entry(
                    definition.name(),
                    index,
                    stats_of(&served, definition.name()),
                ))
            })
            .collect(),
    )
}

/// The property index `name` of `g` as its entry, `null` when there is none.
pub fn index_in<S: GraphStorage>(g: &S, name: &str) -> Value {
    match g
        .definition(DefinitionKind::PROPERTY_INDEX, name)
        .and_then(Definition::as_property_index)
    {
        Some(index) => entry(name, index, stats_of(&g.property_indexes(), name)),
        None => Value::Null,
    }
}

fn stats_of(served: &[PropertyIndexInfo], name: &str) -> Option<PropertyIndexStats> {
    served
        .iter()
        .find(|info| info.name == name)
        .map(|info| info.stats)
}

/// Stores the checked property index `definition` in `g` in ONE commit (which builds it): the
/// name must be free; a storage without property indexes refuses it (`Unsupported`, field
/// `general`); [`PropertyIndexDefinition::check_create`] against `g`'s stored schema refuses
/// an unknown type (field `type`) or transform (field `transform`) and a key declared as
/// values the index cannot hold (field `key`).
pub(crate) fn store_index<S: GraphStorage>(
    g: &mut S,
    definition: Definition,
) -> Result<(), Vec<FieldError>> {
    if !g.capabilities().property_indexes {
        let err = GraphError::unsupported("property indexes");
        return Err(vec![FieldError::new("general", format_diagnostic(&err))]);
    }
    let name = definition.name().to_string();
    if g.definition(DefinitionKind::PROPERTY_INDEX, &name)
        .is_some()
    {
        return Err(vec![FieldError::new(
            "name",
            format!("a property index named '{name}' exists already; choose another name"),
        )]);
    }
    if let Some(index) = definition.as_property_index()
        && let Err(err) = index.check_create(&name, g.get_schema().as_ref())
    {
        return Err(vec![field_of(&err)]);
    }
    let stored = g.transaction(|unit| unit.set_definition(definition));
    match stored {
        Ok(()) => Ok(()),
        Err(GraphError::Definition(err)) => Err(vec![field_of(&err)]),
        Err(err) => Err(vec![FieldError::new("general", format_diagnostic(&err))]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modern() -> Session {
        let mut s = Session::new();
        s.load_sample("modern").unwrap();
        s
    }

    #[test]
    fn create_list_and_drop() {
        let mut s = modern();
        let created = s.create_index(&json!({"label": "person", "key": "age"}));
        assert_eq!(created["ok"], true, "{created}");
        assert_eq!(created["index"]["name"], "person_age");
        assert_eq!(created["index"]["element"], "vertex");
        assert_eq!(created["index"]["served"], true);
        assert_eq!(created["index"]["stats"]["entries"], 4);
        assert_eq!(created["index"]["stats"]["distinctValues"], 4);
        assert!(created["index"]["stats"]["bytes"].as_u64().unwrap() > 0);
        let edge = s.create_index(&json!({
            "element": "edge", "label": "knows", "key": "weight", "name": "w"
        }));
        assert_eq!(edge["index"]["name"], "w", "{edge}");
        assert_eq!(edge["index"]["stats"]["entries"], 2);
        let list = s.list_indexes().unwrap();
        assert_eq!(list.as_array().unwrap().len(), 2);
        assert_eq!(list[0]["name"], "person_age");
        assert_eq!(s.index_count().unwrap(), 2);
        assert_eq!(s.info().unwrap()["propertyIndexes"], 2);
        assert_eq!(s.info().unwrap()["catalog"]["property_index"], 2);
        let memory = &s.statistics().unwrap()["memory"];
        assert!(
            memory["property_index_bytes"].as_u64().unwrap() > 0,
            "{memory}"
        );
        // A query served by the index gives the scan's answer.
        let out = s.execute(
            r#"g.v().has_label("person").has("age", 29).values("name").to_list()"#,
            &crate::RunOptions::default(),
        );
        assert!(out.contains("marko"), "{out}");
        assert!(s.drop_index("person_age").unwrap());
        assert!(!s.drop_index("person_age").unwrap());
        assert_eq!(s.get_index("person_age").unwrap(), Value::Null);
        assert_eq!(s.index_count().unwrap(), 1);
    }

    #[test]
    fn field_errors_name_their_inputs() {
        let mut s = modern();
        let refused = s.create_index(&json!({"label": "", "key": 3, "element": "node"}));
        assert_eq!(refused["ok"], false);
        let fields: Vec<&str> = refused["errors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["field"].as_str().unwrap())
            .collect();
        for field in ["label", "key", "element"] {
            assert!(fields.contains(&field), "{field}: {refused}");
        }
        let refused = s.create_index(&json!({"name": "bad name", "label": "a::b", "key": "k"}));
        assert_eq!(refused["errors"][0]["field"], "name", "{refused}");
        assert_eq!(refused["errors"][1]["field"], "label", "{refused}");
        let refused = s.create_index(&json!({"label": "person", "key": "age", "unique": true}));
        assert_eq!(refused["errors"][0]["field"], "general", "{refused}");
        assert_eq!(s.create_index(&json!("person"))["ok"], false);
        // The same name again: a name error (drop it first); the same (element, label, key)
        // under another name: the conflict belongs to the key.
        assert_eq!(
            s.create_index(&json!({"label": "person", "key": "age"}))["ok"],
            true
        );
        let again =
            s.create_index(&json!({"label": "person", "key": "name", "name": "person_age"}));
        assert_eq!(again["errors"][0]["field"], "name", "{again}");
        let conflict = s.create_index(&json!({"label": "person", "key": "age", "name": "ages"}));
        assert_eq!(conflict["errors"][0]["field"], "key", "{conflict}");
        assert!(
            conflict["errors"][0]["help"]
                .as_str()
                .unwrap()
                .contains("drop_index"),
            "{conflict}"
        );
        assert_eq!(s.index_count().unwrap(), 1);
    }

    #[test]
    fn the_type_is_listed_checked_and_mapped_to_its_field() {
        let mut s = modern();
        let created = s.create_index(&json!({"label": "person", "key": "age"}));
        assert_eq!(created["index"]["type"], "memory-ordered", "{created}");
        assert_eq!(created["index"]["reason"], Value::Null, "{created}");
        let typed = s.create_index(&json!({
            "label": "software", "key": "lang", "type": "memory-ordered"
        }));
        assert_eq!(typed["ok"], true, "{typed}");
        assert_eq!(typed["index"]["type"], "memory-ordered");
        // An unknown type: an error of the field `type`, with the known types in its help.
        let refused = s.create_index(&json!({"label": "person", "key": "name", "type": "hash"}));
        assert_eq!(refused["ok"], false);
        assert_eq!(refused["errors"][0]["field"], "type", "{refused}");
        assert!(
            refused["errors"][0]["help"]
                .as_str()
                .unwrap()
                .contains("memory-ordered"),
            "{refused}"
        );
        let refused = s.create_index(&json!({"label": "person", "key": "name", "type": 3}));
        assert_eq!(refused["errors"][0]["field"], "type", "{refused}");
        // A key the schema declares as a vector: an error of the field `key`.
        s.execute(
            r#"g.set_schema(#{mode: "open", vertices: #{doc: #{schema: #{type: "object",
                 properties: #{emb: #{type: "array", format: "vector", element: "f32"},
                               tags: #{type: "array"}}}}}})"#,
            &crate::RunOptions::default(),
        );
        let refused = s.create_index(&json!({"label": "doc", "key": "emb"}));
        assert_eq!(refused["ok"], false, "{refused}");
        assert_eq!(refused["errors"][0]["field"], "key", "{refused}");
        assert!(
            refused["errors"][0]["message"]
                .as_str()
                .unwrap()
                .contains("vector<f32>"),
            "{refused}"
        );
        assert!(
            refused["errors"][0]["help"]
                .as_str()
                .unwrap()
                .contains("nearest()"),
            "{refused}"
        );
        let refused = s.create_index(&json!({"label": "doc", "key": "tags"}));
        assert_eq!(refused["errors"][0]["field"], "key", "{refused}");
        assert_eq!(s.index_count().unwrap(), 2);
        // The known types for a form.
        assert_eq!(index_types()[0]["name"], "memory-ordered");
        assert!(index_types()[0]["description"].is_string());
    }

    #[test]
    fn the_transform_is_stored_listed_named_and_checked() {
        let mut s = modern();
        let created =
            s.create_index(&json!({"label": "person", "key": "name", "transform": "to_lower"}));
        assert_eq!(created["ok"], true, "{created}");
        assert_eq!(
            created["index"]["name"], "person_name_to_lower",
            "{created}"
        );
        assert_eq!(created["index"]["transform"], "to_lower", "{created}");
        assert_eq!(created["index"]["stats"]["entries"], 4, "{created}");
        assert_eq!(created["index"]["stats"]["skipped"], 0, "{created}");
        // A plain index on the same key coexists; its transform is null.
        let plain = s.create_index(&json!({"label": "person", "key": "name"}));
        assert_eq!(plain["index"]["name"], "person_name", "{plain}");
        assert_eq!(plain["index"]["transform"], Value::Null, "{plain}");
        let edge = s.create_index(&json!({
            "element": "edge", "label": "knows", "key": "note", "transform": "to_lower"
        }));
        assert_eq!(edge["index"]["name"], "knows_note_edge_to_lower", "{edge}");
        // An unknown transform: an error of the field `transform`.
        let refused =
            s.create_index(&json!({"label": "person", "key": "name", "transform": "upper"}));
        assert_eq!(refused["errors"][0]["field"], "transform", "{refused}");
        let refused = s.create_index(&json!({"label": "person", "key": "name", "transform": 1}));
        assert_eq!(refused["errors"][0]["field"], "transform", "{refused}");
        // A key declared as an integer: an error of the field `key`.
        s.execute(
            r#"g.set_schema(#{mode: "open", vertices: #{item: #{schema: #{type: "object",
                 properties: #{code: #{type: "integer"}}}}}})"#,
            &crate::RunOptions::default(),
        );
        let refused =
            s.create_index(&json!({"label": "item", "key": "code", "transform": "to_lower"}));
        assert_eq!(refused["errors"][0]["field"], "key", "{refused}");
        assert_eq!(s.index_count().unwrap(), 3);
        // The served query.
        let out = s.execute(
            r#"g.v().has_label("person").has("name", __.to_lower().is("josh")).values("name").to_list()"#,
            &crate::RunOptions::default(),
        );
        assert!(out.contains("josh"), "{out}");
        assert_eq!(index_transforms()[0]["name"], "to_lower");
        assert!(index_transforms()[0]["description"].is_string());
    }

    #[test]
    fn an_unknown_type_is_listed_not_served_with_its_reason() {
        let mut tg = TraversalGraph::tinkerpop_modern();
        let mut payload = PropertyMap::default();
        payload.insert("label", "person".into());
        payload.insert("key", "age".into());
        payload.insert("type", "future-x".into());
        let index = PropertyIndexDefinition::from_payload(&payload).unwrap();
        tg.transaction(|g| g.set_definition(Definition::property_index("future", index)))
            .unwrap();
        let listed = indexes_in(&tg);
        assert_eq!(listed[0]["type"], "future-x", "{listed}");
        assert_eq!(listed[0]["served"], false, "{listed}");
        assert_eq!(listed[0]["stats"], Value::Null, "{listed}");
        assert!(
            listed[0]["reason"]
                .as_str()
                .unwrap()
                .contains("index type 'future-x' is not known to this version"),
            "{listed}"
        );
        // A storage without indexes says so.
        let entry = entry(
            "x",
            &PropertyIndexDefinition::new(ElementKind::Vertex, "a", "b"),
            None,
        );
        assert!(
            entry["reason"]
                .as_str()
                .unwrap()
                .contains("serves no property indexes"),
            "{entry}"
        );
    }

    #[test]
    fn indexes_on_a_foreign_storage_equal_those_of_a_traversal_graph() {
        let mut tg = TraversalGraph::tinkerpop_modern();
        let mut fw = graphersal_storage_tests::forwarding::Forwarding::new(
            TraversalGraph::tinkerpop_modern(),
        );
        let definition = || index_definition(&json!({"label": "person", "key": "age"})).unwrap();
        store_index(&mut tg, definition()).unwrap();
        store_index(&mut fw, definition()).unwrap();
        assert_eq!(indexes_in(&tg), indexes_in(&fw));
        assert_eq!(index_in(&fw, "person_age")["stats"]["entries"], 4);
        assert_eq!(index_in(&fw, "none"), Value::Null);
        // A storage without the capability: the entry of a stored definition says `null`.
        let entry = entry(
            "x",
            definition().as_property_index().unwrap(),
            None::<PropertyIndexStats>,
        );
        assert_eq!(entry["served"], false);
        assert_eq!(entry["stats"], Value::Null);
    }
}
