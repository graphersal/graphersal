//! Embedding models as catalog definitions (kind [`DefinitionKind::MODEL`], format spec 6.3 and
//! 4.5; book page "Embedding Models in a Store"): one definition per model, named by its
//! identity (`model2vec/<repo>@<12 hex>`), whose payload references the model's files (ONE blob,
//! a deterministic tar) and copies what is known about it (repository, revision, dimension,
//! licence, the memory it needs loaded). A definition follows the history like every catalog
//! entry: every position (commit, mark, snapshot, rollback, view) has exactly the models and the
//! default model of that moment.
//!
//! Catalog invariants (checked when a definition is stored): at most one model per repository
//! (the repository name is the user-visible name of the CURRENT revision) and at most one
//! default. A model of a family this version does not run, or whose blob uses a hash algorithm
//! it does not know, is kept as read, written back unchanged and not served
//! ([`ModelDefinition::unserved_reason`]).

use crate::error::{DefinitionError, GraphError};
use crate::graph::TraversalGraph;
use crate::graph::definition::{Definition, DefinitionBody, DefinitionFlags, DefinitionKind};
use crate::value::vector::embed::identity::{FAMILY, is_repo_name, parse_identity};
use crate::value::blob::BlobHashAlgo;
use crate::value::{BlobRef, ElementProperty, PropertyMap};

/// The longest model definition name (an identity).
pub const MAX_MODEL_NAME_BYTES: usize = 256;
/// The longest licence id kept.
const MAX_LICENSE_BYTES: usize = 64;
/// The most base models a definition keeps.
const MAX_BASE_MODELS: usize = 16;
/// The longest revision or commit text.
const MAX_REVISION_BYTES: usize = 255;

/// A model another one was made from (the `base_model` of its card), with its licence when
/// known.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct BaseModel {
    /// Its repository.
    pub repo: String,
    /// Its licence, when known.
    pub license: Option<String>,
}

impl BaseModel {
    /// A base model `repo` under `license`.
    pub fn new(repo: impl Into<String>, license: Option<String>) -> Self {
        BaseModel {
            repo: repo.into(),
            license,
        }
    }
}

/// An embedding model of the catalog (kind [`DefinitionKind::MODEL`]): the blob holding its
/// files and the fields copied from it at import. Made by `Store::import_model` (feature
/// `persist`); the definition's name is the model identity.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelDefinition {
    blob: BlobRef,
    family: String,
    repo: String,
    revision: Option<String>,
    commit: Option<String>,
    dimension: u64,
    load_bytes: u64,
    license: Option<String>,
    license_text: Option<String>,
    base_models: Vec<BaseModel>,
    gated: bool,
    default: bool,
    /// Unknown payload keys, kept in their order.
    extra: PropertyMap,
    /// The payload exactly as read when this version does not serve the model (written back
    /// unchanged).
    original: Option<PropertyMap>,
}

impl ModelDefinition {
    /// A Model2Vec model of the repository `repo` whose files are the tar `blob`.
    pub fn new(blob: BlobRef, repo: impl Into<String>) -> Self {
        ModelDefinition {
            blob,
            family: FAMILY.to_string(),
            repo: repo.into(),
            revision: None,
            commit: None,
            dimension: 0,
            load_bytes: 0,
            license: None,
            license_text: None,
            base_models: Vec::new(),
            gated: false,
            default: false,
            extra: PropertyMap::default(),
            original: None,
        }
    }

    /// The same model with the revision asked for at download.
    pub fn with_revision(mut self, revision: Option<String>) -> Self {
        self.revision = revision;
        self.original = None;
        self
    }

    /// The same model with the commit its revision resolved to.
    pub fn with_commit(mut self, commit: Option<String>) -> Self {
        self.commit = commit;
        self.original = None;
        self
    }

    /// The same model with its vector dimension.
    pub fn with_dimension(mut self, dimension: u64) -> Self {
        self.dimension = dimension;
        self.original = None;
        self
    }

    /// The same model with the bytes it takes loaded (measured at import; the memory check of
    /// a load uses it before reading the blob).
    pub fn with_load_bytes(mut self, bytes: u64) -> Self {
        self.load_bytes = bytes;
        self.original = None;
        self
    }

    /// The same model with its licence (an SPDX id) and where its text comes from (`file`,
    /// `template`, `none`).
    pub fn with_license(mut self, license: Option<String>, text: Option<String>) -> Self {
        self.license = license;
        self.license_text = text;
        self.original = None;
        self
    }

    /// The same model with these base models.
    pub fn with_base_models(mut self, base_models: Vec<BaseModel>) -> Self {
        self.base_models = base_models;
        self.original = None;
        self
    }

    /// The same model, gated or not.
    pub fn with_gated(mut self, gated: bool) -> Self {
        self.gated = gated;
        self.original = None;
        self
    }

    /// The same model, the default or not.
    pub fn with_default(mut self, default: bool) -> Self {
        if self.default != default {
            if let Some(original) = &mut self.original {
                if default {
                    original.insert("default", ElementProperty::Boolean(true));
                } else {
                    original.shift_remove("default");
                }
            }
            self.default = default;
        }
        self
    }

    /// The blob holding the model's files.
    pub fn blob(&self) -> &BlobRef {
        &self.blob
    }

    /// The model family (`model2vec`).
    pub fn family(&self) -> &str {
        &self.family
    }

    /// The repository: the model's user-visible name.
    pub fn repo(&self) -> &str {
        &self.repo
    }

    /// The revision asked for at download.
    pub fn revision(&self) -> Option<&str> {
        self.revision.as_deref()
    }

    /// The commit the revision resolved to.
    pub fn commit(&self) -> Option<&str> {
        self.commit.as_deref()
    }

    /// The vector dimension (`0`: not recorded).
    pub fn dimension(&self) -> u64 {
        self.dimension
    }

    /// The bytes the model takes loaded.
    pub fn load_bytes(&self) -> u64 {
        self.load_bytes
    }

    /// The licence (an SPDX id), when recorded.
    pub fn license(&self) -> Option<&str> {
        self.license.as_deref()
    }

    /// Where the licence text comes from (`file`, `template`, `none`).
    pub fn license_text(&self) -> Option<&str> {
        self.license_text.as_deref()
    }

    /// The models this one was made from.
    pub fn base_models(&self) -> &[BaseModel] {
        &self.base_models
    }

    /// Whether the repository is gated.
    pub fn gated(&self) -> bool {
        self.gated
    }

    /// Whether this is the default model (at most one in a catalog).
    pub fn is_default(&self) -> bool {
        self.default
    }

    /// The unknown payload keys, kept in their order.
    pub fn extra(&self) -> &PropertyMap {
        &self.extra
    }

    /// Whether this version serves the model (a known family and blob hash algorithm).
    pub fn served(&self) -> bool {
        self.unserved_reason().is_none()
    }

    /// Why this version does not serve the model: a family or a blob hash algorithm a newer
    /// version wrote. `None`: served.
    pub fn unserved_reason(&self) -> Option<String> {
        if self.family != FAMILY {
            return Some(format!(
                "a model of the family {:?}, which this version does not run",
                self.family
            ));
        }
        if let BlobHashAlgo::Unknown(code) = self.blob.hash_algo() {
            return Some(format!(
                "its blob uses the hash algorithm {code}, which this version does not know"
            ));
        }
        None
    }

    /// The payload map: keys `blob`, `family`, `repo`, `revision`, `commit`, `dimension`,
    /// `load_bytes`, `license`, `license_text`, `base_models`, `gated`, `default` (absent ones
    /// left out; `default` only when true), then the unknown keys. A model this version does not
    /// serve is written back exactly as read.
    pub fn to_payload(&self) -> PropertyMap {
        if let Some(original) = &self.original {
            return original.clone();
        }
        let mut map = PropertyMap::default();
        map.insert("blob", ElementProperty::Blob(Box::new(self.blob.clone())));
        map.insert("family", self.family.as_str().into());
        map.insert("repo", self.repo.as_str().into());
        if let Some(revision) = &self.revision {
            map.insert("revision", revision.as_str().into());
        }
        if let Some(commit) = &self.commit {
            map.insert("commit", commit.as_str().into());
        }
        map.insert(
            "dimension",
            ElementProperty::from(i64::try_from(self.dimension).unwrap_or(i64::MAX)),
        );
        map.insert(
            "load_bytes",
            ElementProperty::from(i64::try_from(self.load_bytes).unwrap_or(i64::MAX)),
        );
        if let Some(license) = &self.license {
            map.insert("license", license.as_str().into());
        }
        if let Some(text) = &self.license_text {
            map.insert("license_text", text.as_str().into());
        }
        map.insert(
            "base_models",
            ElementProperty::Array(
                self.base_models
                    .iter()
                    .map(|base| {
                        let mut entry = PropertyMap::default();
                        entry.insert("repo", base.repo.as_str().into());
                        if let Some(license) = &base.license {
                            entry.insert("license", license.as_str().into());
                        }
                        ElementProperty::Object(Box::new(entry))
                    })
                    .collect(),
            ),
        );
        map.insert("gated", ElementProperty::Boolean(self.gated));
        if self.default {
            map.insert("default", ElementProperty::Boolean(true));
        }
        for (key, value) in &self.extra {
            map.insert_if_absent(key, || value.clone());
        }
        map
    }

    /// Reads a payload map (the inverse of [`Self::to_payload`]). `blob`, `family` and `repo`
    /// are required; a known key of the wrong type is an error (the definition is then kept
    /// opaque); unknown keys are kept. Not validated.
    pub fn from_payload(payload: &PropertyMap) -> Result<ModelDefinition, DefinitionError> {
        Self::read_fields(payload, &[])
    }

    pub(crate) fn read_fields(
        map: &PropertyMap,
        skip: &[&str],
    ) -> Result<ModelDefinition, DefinitionError> {
        let malformed = |key: &str, reason: &str| DefinitionError::Malformed {
            reason: format!("/{key}: {reason}"),
        };
        let text = |key: &str| -> Result<Option<String>, DefinitionError> {
            match map.get(key) {
                None => Ok(None),
                Some(value) => value
                    .text()
                    .map(|text| Some(text.into_owned()))
                    .ok_or_else(|| malformed(key, "must be a string")),
            }
        };
        let count = |key: &str| -> Result<u64, DefinitionError> {
            match map.get(key) {
                None => Ok(0),
                Some(value) => value
                    .as_i64()
                    .and_then(|n| u64::try_from(n).ok())
                    .ok_or_else(|| malformed(key, "must be a non-negative integer")),
            }
        };
        let flag = |key: &str| -> Result<bool, DefinitionError> {
            match map.get(key) {
                None => Ok(false),
                Some(ElementProperty::Boolean(b)) => Ok(*b),
                Some(_) => Err(malformed(key, "must be a boolean")),
            }
        };
        let blob = match map.get("blob") {
            Some(ElementProperty::Blob(blob)) => (**blob).clone(),
            Some(other @ (ElementProperty::Object(_) | ElementProperty::Map(_))) => {
                // The exchange form read back from JSON: the tagged or the bare payload.
                let lowered = crate::value::physical::lower(other);
                let payload = match &lowered {
                    crate::value::physical::PhysicalValue::Object(fields)
                        if fields.iter().any(|(k, _)| k == "@value") =>
                    {
                        fields
                            .iter()
                            .find(|(k, _)| k == "@value")
                            .map(|(_, v)| v.clone())
                            .unwrap_or(crate::value::physical::PhysicalValue::Null)
                    }
                    _ => lowered.clone(),
                };
                crate::value::blob::from_payload(&payload)
                    .map_err(|reason| malformed("blob", &reason))?
            }
            Some(_) => return Err(malformed("blob", "must be a blob reference")),
            None => {
                return Err(DefinitionError::Malformed {
                    reason: "'blob' is missing".to_string(),
                });
            }
        };
        let family = text("family")?.ok_or_else(|| DefinitionError::Malformed {
            reason: "'family' is missing".to_string(),
        })?;
        let repo = text("repo")?.ok_or_else(|| DefinitionError::Malformed {
            reason: "'repo' is missing".to_string(),
        })?;
        let mut base_models = Vec::new();
        match map.get("base_models") {
            None => {}
            Some(ElementProperty::Array(items)) => {
                for item in items {
                    let Some(entry) = item.as_object() else {
                        return Err(malformed("base_models", "an entry is not an object"));
                    };
                    let field = |key: &str| entry.get(key).and_then(|v| v.text()).map(|t| t.into_owned());
                    let Some(repo) = field("repo") else {
                        return Err(malformed("base_models", "an entry has no repo"));
                    };
                    base_models.push(BaseModel {
                        repo,
                        license: field("license"),
                    });
                }
            }
            Some(_) => return Err(malformed("base_models", "must be an array")),
        }
        let model = ModelDefinition {
            blob,
            family,
            repo,
            revision: text("revision")?,
            commit: text("commit")?,
            dimension: count("dimension")?,
            load_bytes: count("load_bytes")?,
            license: text("license")?,
            license_text: text("license_text")?,
            base_models,
            gated: flag("gated")?,
            default: flag("default")?,
            extra: {
                const KNOWN: [&str; 12] = [
                    "blob",
                    "family",
                    "repo",
                    "revision",
                    "commit",
                    "dimension",
                    "load_bytes",
                    "license",
                    "license_text",
                    "base_models",
                    "gated",
                    "default",
                ];
                let mut extra = PropertyMap::default();
                for (key, value) in map {
                    if !KNOWN.contains(&key.as_str()) && !skip.contains(&key.as_str()) {
                        extra.insert(key.clone(), value.to_logical());
                    }
                }
                extra
            },
            original: None,
        };
        let mut model = model;
        if !model.served() {
            let mut original = PropertyMap::default();
            for (key, value) in map {
                if !skip.contains(&key.as_str()) {
                    original.insert(key.clone(), value.to_logical());
                }
            }
            model.original = Some(original);
        }
        Ok(model)
    }

    /// Writes the exchange-form fields (the payload keys) into `map`.
    pub(crate) fn write_fields(&self, map: &mut PropertyMap) {
        for (key, value) in self.to_payload() {
            map.insert(key, value);
        }
    }

    /// The rules of [`Definition::validate`] for a model named `name`: a name of 1 to
    /// [`MAX_MODEL_NAME_BYTES`] bytes that is the model's identity (`model2vec/<repo>@<12 hex>`
    /// naming this definition's repository) for the family this version runs; a repository
    /// name; bounded licence, revision and base model fields.
    pub(crate) fn validate(&self, name: &str) -> Result<(), DefinitionError> {
        let invalid_name = |reason: String| DefinitionError::InvalidName {
            kind: DefinitionKind::MODEL.to_string(),
            name: name.to_string(),
            reason,
        };
        let field = |field: &str, reason: String| DefinitionError::InvalidField {
            name: name.to_string(),
            field: field.to_string(),
            reason,
        };
        if name.is_empty() || name.len() > MAX_MODEL_NAME_BYTES {
            return Err(invalid_name(format!(
                "a model name is 1 to {MAX_MODEL_NAME_BYTES} bytes"
            )));
        }
        if self.family == FAMILY {
            match parse_identity(name) {
                Some(parsed) if parsed.normalize.is_none() => {
                    if parsed.repo != Some(self.repo.as_str()) {
                        return Err(invalid_name(format!(
                            "the identity names the repository {:?}, the definition {:?}",
                            parsed.repo.unwrap_or(""),
                            self.repo
                        )));
                    }
                }
                _ => {
                    return Err(invalid_name(
                        "it is not a model identity model2vec/<repo>@<12 hex>".to_string(),
                    ));
                }
            }
        }
        if !is_repo_name(&self.repo) {
            return Err(field(
                "repo",
                format!("{:?} is not a repository name (owner/name)", self.repo),
            ));
        }
        let label_ok =
            |text: &str| text.len() <= MAX_LICENSE_BYTES && text.chars().all(|c| c.is_ascii_graphic());
        for (key, value) in [("license", &self.license), ("license_text", &self.license_text)] {
            if let Some(value) = value
                && !label_ok(value)
            {
                return Err(field(key, format!("{value:?} is not a licence id")));
            }
        }
        for (key, value) in [("revision", &self.revision), ("commit", &self.commit)] {
            if let Some(value) = value
                && (value.len() > MAX_REVISION_BYTES || value.chars().any(char::is_control))
            {
                return Err(field(key, "it is not plain text of at most 255 bytes".into()));
            }
        }
        if self.base_models.len() > MAX_BASE_MODELS {
            return Err(field(
                "base_models",
                format!("at most {MAX_BASE_MODELS} base models"),
            ));
        }
        for base in &self.base_models {
            if !is_repo_name(&base.repo) || !base.license.as_deref().is_none_or(label_ok) {
                return Err(field(
                    "base_models",
                    format!("the base model {:?} is not a repository name", base.repo),
                ));
            }
        }
        Ok(())
    }
}

/// Checks a model definition against the catalog it is about to join: no other model of the
/// same repository (the current revision), no second default.
pub(crate) fn check_conflict(
    graph: &TraversalGraph,
    definition: &Definition,
) -> Result<(), GraphError> {
    let Some(model) = definition.as_model() else {
        return Ok(());
    };
    for other in graph.definitions.iter() {
        if other.kind() != DefinitionKind::MODEL || other.name() == definition.name() {
            continue;
        }
        let Some(existing) = other.as_model() else {
            continue;
        };
        if existing.repo() == model.repo() {
            return Err(DefinitionError::Conflict {
                kind: "model".to_string(),
                name: definition.name().to_string(),
                existing: other.name().to_string(),
                reason: format!(
                    "the model {} is the current revision of {}: at most one revision of a \
                     repository is in the catalog (replace it)",
                    other.name(),
                    model.repo()
                ),
            }
            .into());
        }
        if existing.is_default() && model.is_default() {
            return Err(DefinitionError::Conflict {
                kind: "model".to_string(),
                name: definition.name().to_string(),
                existing: other.name().to_string(),
                reason: format!(
                    "the model {} is the default already: at most one default model",
                    other.name()
                ),
            }
            .into());
        }
    }
    Ok(())
}

/// `schema` with every vector property's `embedder` that names a model of `catalog` by its
/// repository replaced by that model's identity (the schema records the concrete model; an
/// identity, `hash-ngram/1` or a host embedder's name stay as they are).
pub(crate) fn resolve_model_names(
    catalog: &crate::graph::definition::Catalog,
    mut schema: crate::graph::schema::GraphSchema,
) -> crate::graph::schema::GraphSchema {
    let models: Vec<(String, String)> = catalog
        .iter()
        .filter_map(|d| {
            d.as_model()
                .filter(|m| m.served())
                .map(|m| (m.repo().to_string(), d.name().to_string()))
        })
        .collect();
    if models.is_empty() {
        return schema;
    }
    let roots = schema
        .vertices
        .values_mut()
        .map(|v| &mut v.schema)
        .chain(schema.edges.values_mut().map(|e| &mut e.schema));
    for root in roots {
        for node in root.properties.values_mut() {
            if let Some(vector) = node.vector.as_mut()
                && let Some(named) = vector.embedder.as_mut()
                && let Some((_, identity)) = models.iter().find(|(repo, _)| repo == named)
            {
                *named = identity.clone();
            }
        }
    }
    schema
}

impl Definition {
    /// An embedding model named by its identity `name` (kind [`DefinitionKind::MODEL`], not
    /// critical: a reader that does not know the kind keeps it and serves no model).
    pub fn model(name: impl Into<String>, model: ModelDefinition) -> Self {
        Definition::from_parts(
            DefinitionKind::MODEL,
            DefinitionFlags::NONE,
            name.into(),
            DefinitionBody::Model(model),
        )
    }

    /// The model, when this is a decoded one.
    pub fn as_model(&self) -> Option<&ModelDefinition> {
        match self.body() {
            DefinitionBody::Model(model) => Some(model),
            DefinitionBody::Query(_)
            | DefinitionBody::Compression(_)
            | DefinitionBody::PropertyIndex(_)
            | DefinitionBody::Job(_)
            | DefinitionBody::Opaque(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob() -> BlobRef {
        BlobRef::new(BlobHashAlgo::Sha256, &[0xab; 32], 4096, 9).unwrap()
    }

    fn model() -> ModelDefinition {
        ModelDefinition::new(blob(), "acme/tiny")
            .with_revision(Some("main".into()))
            .with_dimension(8)
            .with_load_bytes(1234)
            .with_license(Some("mit".into()), Some("template".into()))
            .with_base_models(vec![BaseModel::new("acme/base", Some("apache-2.0".into()))])
            .with_default(true)
    }

    #[test]
    fn payloads_round_trip_and_validate() {
        let model = model();
        let back = ModelDefinition::from_payload(&model.to_payload()).unwrap();
        assert_eq!(back, model);
        let definition = Definition::model("model2vec/acme/tiny@0123456789ab", model.clone());
        definition.validate().unwrap();
        let json = definition.to_json();
        assert_eq!(Definition::from_json(&json).unwrap(), definition);
        for bad in [
            "acme/tiny",
            "model2vec/acme/other@0123456789ab",
            "model2vec/acme/tiny@0123456789ab?normalize=false",
        ] {
            assert!(Definition::model(bad, model.clone()).validate().is_err(), "{bad}");
        }
    }

    #[test]
    fn an_unknown_family_and_unknown_keys_are_kept_and_written_back() {
        let mut payload = model().to_payload();
        payload.insert("family", "future-family".into());
        payload.insert("zzz", ElementProperty::from(7i64));
        let read = ModelDefinition::from_payload(&payload).unwrap();
        assert!(!read.served());
        assert!(read.unserved_reason().unwrap().contains("future-family"));
        assert_eq!(read.to_payload(), payload);
        let mut payload = model().to_payload();
        payload.insert("zzz", ElementProperty::from(7i64));
        let read = ModelDefinition::from_payload(&payload).unwrap();
        assert!(read.served());
        assert_eq!(read.to_payload(), payload);
    }
}
