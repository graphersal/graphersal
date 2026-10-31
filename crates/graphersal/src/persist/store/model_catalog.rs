//! Embedding models as catalog definitions in a Store (format spec 4.5, 6.3 kind 5; book page
//! "Embedding Models in a Store"): an import writes the model's files as ONE blob (a
//! deterministic tar, `persist/model_tar.rs`) and commits a definition of kind
//! [`DefinitionKind::MODEL`] referencing it, in one unit. The definition follows the history:
//! every position has the models (and the default) of its moment. A model leaves the store's
//! current state by a removal commit, refused while the schema needs it; its blob goes with the
//! prune that drops the last history referencing it.
//!
//! Every function here is host level (no DSL, no MCP path): `Store::models`,
//! `Store::import_model`, `Store::set_default_model`, `Store::remove_model`.

use std::io::Read;
use std::sync::Arc;

use super::files::DirPath;
use super::models::{ModelRole, NewModel};
use super::{Store, blobs, licenses};
use crate::error::GraphError;
use crate::graph::definition::{Definition, DefinitionKind};
use crate::graph::model_definition::{BaseModel, ModelDefinition};
use crate::graph::schema::GraphSchema;
use crate::graph::system_journal::{Event, catalogue};
use crate::graph::{CommitMetadata, GraphStorage, Mutation, PersistentStorage, Transactional};
use crate::persist::model_tar::{
    self, MAX_MODEL_FILE_BYTES, MAX_MODEL_FILES, ManifestBase, ManifestFile, ModelManifest,
    TarContent, TarFile, TarStream, is_model_file_name, sha256_hex,
};
use crate::persist::{PersistError, StoreFailure};
use crate::value::BlobRef;
use crate::value::vector::embed::identity::{
    FAMILY, IDENTITY_ROLES, identity_digest, identity_text, is_repo_name, parse_identity,
};
use crate::value::vector::embed::registry::{self, ModelLoadState};

/// Where an imported model's files come from ([`Store::import_model`]).
#[derive(Debug)]
#[non_exhaustive]
pub enum ModelSource {
    /// A local directory (native only): a model downloaded by `graphersal embed-model
    /// download` (with its manifest `graphersal-embed-model.json`, card and licence files) or
    /// any directory of a Model2Vec model (`config.json`, `tokenizer.json`,
    /// `model.safetensors`, or the sentence-transformers layout). The files are read twice
    /// (hashed, then written); a file that changes in between fails the import.
    #[cfg(not(target_arch = "wasm32"))]
    Dir(std::path::PathBuf),
    /// Files given as streams with their metadata. They are read into memory first (the
    /// manifest inside the tar lists every file's hash before the files), under the process
    /// memory ceiling.
    Files(NewModel),
}

/// The options of [`Store::import_model`].
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ImportModelOptions {
    repo: Option<String>,
    license: Option<String>,
    replace: bool,
    default: bool,
    principal: Option<String>,
}

impl ImportModelOptions {
    /// The default options: the repository and licence from the source, no replace, not the
    /// default.
    pub fn new() -> Self {
        Self::default()
    }

    /// The Hugging Face repository (`owner/name`) when the source does not name it (a
    /// directory without a download manifest); it must equal the source's when both do.
    pub fn with_repo(mut self, repo: impl Into<String>) -> Self {
        self.repo = Some(repo.into());
        self
    }

    /// The licence (an SPDX id) when the source does not state it.
    pub fn with_license(mut self, license: impl Into<String>) -> Self {
        self.license = Some(license.into());
        self
    }

    /// Whether a new revision of a repository the store holds replaces the current one
    /// (default `false`: refused with [`StoreFailure::ModelRevisionExists`]).
    pub fn with_replace(mut self, replace: bool) -> Self {
        self.replace = replace;
        self
    }

    /// Whether the model becomes the default model (the one `embed` uses without a name).
    pub fn with_default(mut self, default: bool) -> Self {
        self.default = default;
        self
    }

    /// The principal recorded in the commit's metadata.
    pub fn with_principal(mut self, principal: impl Into<String>) -> Self {
        self.principal = Some(principal.into());
        self
    }
}

/// The state of a model ([`ModelInfo::state`]).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ModelState {
    /// Loaded in this process.
    Loaded,
    /// Not loaded (it is loaded at the first query that needs it).
    NotLoaded,
    /// Its blob is damaged, or its files do not load.
    Damaged {
        /// What is wrong.
        reason: String,
    },
    /// Its blob is missing.
    Missing,
    /// This build does not run it (a family or hash algorithm of a newer version, or a build
    /// without the `model2vec` feature).
    Unsupported {
        /// Why.
        reason: String,
    },
}

impl ModelState {
    /// The state's name: `loaded`, `not_loaded`, `damaged`, `missing`, `unsupported`.
    pub fn name(&self) -> &'static str {
        match self {
            ModelState::Loaded => "loaded",
            ModelState::NotLoaded => "not_loaded",
            ModelState::Damaged { .. } => "damaged",
            ModelState::Missing => "missing",
            ModelState::Unsupported { .. } => "unsupported",
        }
    }

    /// The reason of a damaged or unsupported model.
    pub fn reason(&self) -> Option<&str> {
        match self {
            ModelState::Damaged { reason } | ModelState::Unsupported { reason } => Some(reason),
            ModelState::Loaded | ModelState::NotLoaded | ModelState::Missing => None,
        }
    }
}

/// An embedding model of a store at its current position ([`Store::models`]).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ModelInfo {
    /// The repository: the model's name (`minishlab/potion-base-8M`).
    pub repo: String,
    /// The model identity the schema records (`model2vec/<repo>@<12 hex>`).
    pub identity: String,
    /// The revision asked for at download.
    pub revision: Option<String>,
    /// The commit the revision resolved to.
    pub commit: Option<String>,
    /// The vector dimension.
    pub dimension: u64,
    /// The licence (an SPDX id), `unknown` when not stated.
    pub license: String,
    /// Where the licence text comes from: `file`, `template` or `none`.
    pub license_text: String,
    /// The models this one was made from.
    pub base_models: Vec<BaseModel>,
    /// Whether the repository is gated.
    pub gated: bool,
    /// Whether this is the default model.
    pub default: bool,
    /// The bytes of its blob (the files as a tar).
    pub bytes: u64,
    /// The memory it takes loaded.
    pub load_bytes: u64,
    /// Loaded, not loaded, damaged, missing, unsupported.
    pub state: ModelState,
    /// The schema properties (`label.key`) whose vectors this model makes.
    pub needed_by: Vec<String>,
}

/// What [`Store::import_model`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ImportModelReport {
    /// The model as the store now has it.
    pub model: ModelInfo,
    /// The store held this model (the same identity) already: nothing was committed unless the
    /// default changed.
    pub already_present: bool,
    /// The commit that imported it (or changed the default); `None` when nothing changed.
    pub commit_seq: Option<u64>,
    /// The revision it replaced.
    pub replaced: Option<ModelInfo>,
    /// The schema properties (`label.key`) whose vectors the replace re-embedded with the new
    /// revision in the same unit (their vector rules named the old one); empty otherwise.
    pub reembedded: Vec<String>,
    /// Things to know: an unknown or non-commercial licence, a gated repository, a check that
    /// was skipped.
    pub warnings: Vec<String>,
}

/// What [`Store::set_default_model`] and [`Store::remove_model`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ModelChangeReport {
    /// The commit that made the change; `None` when nothing changed.
    pub commit_seq: Option<u64>,
    /// The time of that commit (microseconds since the Unix epoch); `None` when nothing
    /// changed.
    pub committed_at: Option<i64>,
    /// The model the change concerns (as it was removed, or as it is now the default).
    pub model: Option<ModelInfo>,
}

// ---------------------------------------------------------------------------------------------
// The schema's models
// ---------------------------------------------------------------------------------------------

/// Every model identity the schema's `embedder` keywords name, with every place (`label.key`)
/// naming it, in schema order.
pub(crate) fn schema_places(schema: &GraphSchema) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    let roots = schema
        .vertices
        .iter()
        .map(|(label, v)| (label, &v.schema))
        .chain(schema.edges.iter().map(|(label, e)| (label, &e.schema)));
    for (label, root) in roots {
        for (key, node) in &root.properties {
            let Some(model) = node.vector.as_ref().and_then(|v| v.embedder.as_deref()) else {
                continue;
            };
            let place = format!("{label}.{key}");
            match out.iter_mut().find(|(m, _)| m == model) {
                Some((_, places)) => places.push(place),
                None => out.push((model.to_string(), vec![place])),
            }
        }
    }
    out
}

/// The identity without a normalization suffix (`?normalize=..`): the definition's name.
pub(crate) fn base_identity(identity: &str) -> &str {
    identity.split('?').next().unwrap_or(identity)
}

/// The models the schema NEEDS (format spec 4.5): every Model2Vec identity a vector property's
/// `embedder` names, with the places naming it. Other embedder names (`hash-ngram/1`, a host's)
/// are no stored models.
pub(crate) fn needed_models(schema: Option<&GraphSchema>) -> Vec<(String, Vec<String>)> {
    let Some(schema) = schema else {
        return Vec::new();
    };
    schema_places(schema)
        .into_iter()
        .filter(|(identity, _)| parse_identity(identity).is_some())
        .collect()
}

/// The places (`label.key`) of the schema that need the model `identity`.
fn needed_by(schema: Option<&GraphSchema>, identity: &str) -> Vec<String> {
    needed_models(schema)
        .into_iter()
        .filter(|(named, _)| base_identity(named) == identity)
        .flat_map(|(_, places)| places)
        .collect()
}

/// The [`ModelInfo`] of a model definition.
pub(crate) fn model_info(
    definition: &Definition,
    model: &ModelDefinition,
    schema: Option<&GraphSchema>,
    state: ModelState,
) -> ModelInfo {
    ModelInfo {
        repo: model.repo().to_string(),
        identity: definition.name().to_string(),
        revision: model.revision().map(str::to_string),
        commit: model.commit().map(str::to_string),
        dimension: model.dimension(),
        license: model.license().unwrap_or("unknown").to_string(),
        license_text: model.license_text().unwrap_or("none").to_string(),
        base_models: model.base_models().to_vec(),
        gated: model.gated(),
        default: model.is_default(),
        bytes: model.blob().length(),
        load_bytes: model.load_bytes(),
        state: match model.unserved_reason() {
            Some(reason) => ModelState::Unsupported { reason },
            None => state,
        },
        needed_by: needed_by(schema, definition.name()),
    }
}

/// The state of a model's blob in the store at `root` by its length only (cheap: no read).
pub(crate) fn blob_state(root: &DirPath, blob: &BlobRef) -> ModelState {
    let path = blobs::blob_path(root, blob);
    match path.len() {
        Ok(len) if len == blob.length() => ModelState::NotLoaded,
        Ok(len) => ModelState::Damaged {
            reason: format!(
                "its blob has {len} bytes, its definition says {}",
                blob.length()
            ),
        },
        Err(_) => ModelState::Missing,
    }
}

/// The models of a catalog (definitions of kind 5 in `definitions`) with their states.
pub(crate) fn infos_of<'a>(
    definitions: impl IntoIterator<Item = &'a Definition>,
    schema: Option<&GraphSchema>,
    state: &dyn Fn(&ModelDefinition) -> ModelState,
) -> Vec<ModelInfo> {
    let mut out: Vec<ModelInfo> = definitions
        .into_iter()
        .filter(|d| d.kind() == DefinitionKind::MODEL)
        .filter_map(|d| d.as_model().map(|m| model_info(d, m, schema, state(m))))
        .collect();
    out.sort_by(|a, b| a.repo.cmp(&b.repo).then(a.identity.cmp(&b.identity)));
    out
}

// ---------------------------------------------------------------------------------------------
// Preparing an import
// ---------------------------------------------------------------------------------------------

/// A model ready to be written: its manifest, its files and what the definition copies.
pub(crate) struct Prepared {
    pub(crate) manifest: ModelManifest,
    pub(crate) files: Vec<TarFile>,
    pub(crate) identity: String,
    pub(crate) dimension: u64,
    pub(crate) load_bytes: u64,
}

impl Prepared {
    /// The definition of the prepared model referencing `blob`.
    pub(crate) fn definition(&self, blob: BlobRef, default: bool) -> Definition {
        let manifest = &self.manifest;
        Definition::model(
            self.identity.clone(),
            ModelDefinition::new(blob, manifest.repo.clone())
                .with_revision(manifest.revision.clone())
                .with_commit(manifest.commit.clone())
                .with_dimension(self.dimension)
                .with_load_bytes(self.load_bytes)
                .with_license(
                    Some(manifest.license.clone()),
                    Some(manifest.license_text.clone()),
                )
                .with_base_models(
                    manifest
                        .base_models
                        .iter()
                        .map(|b| BaseModel::new(b.repo.clone(), b.license.clone()))
                        .collect(),
                )
                .with_gated(manifest.gated)
                .with_default(default),
        )
    }

    /// The length of the tar these files make.
    pub(crate) fn tar_length(&self) -> u64 {
        model_tar::tar_length(self.files.iter().map(|f| f.size))
    }
}

fn invalid(reason: impl Into<String>) -> PersistError {
    PersistError::store("import a model", StoreFailure::InvalidModel, reason)
}

/// The metadata of a model being imported (from a download manifest and the options).
#[derive(Default)]
struct Meta {
    repo: Option<String>,
    revision: Option<String>,
    commit: Option<String>,
    license: Option<String>,
    base_models: Vec<BaseModel>,
    gated: bool,
}

/// One source file: name, role, size, SHA-256 and content.
struct SourceFile {
    name: String,
    role: ModelRole,
    size: u64,
    sha256: String,
    content: TarContent,
    /// The first bytes (the weights' safetensors header).
    head: Vec<u8>,
}

/// The most bytes of the weights file kept to read its safetensors header.
pub(crate) const MAX_HEADER_BYTES: usize = 16 << 20;

/// Reads `reader` whole into memory under the process memory ceiling.
fn read_in_memory(name: &str, reader: &mut dyn Read) -> Result<Vec<u8>, PersistError> {
    let mut out = Vec::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let n = reader
            .read(&mut buffer)
            .map_err(|err| PersistError::io("reading a model file", err))?;
        if n == 0 {
            break;
        }
        if out.len() as u64 + n as u64 > MAX_MODEL_FILE_BYTES {
            return Err(invalid(format!(
                "the file {name:?} is larger than {MAX_MODEL_FILE_BYTES} bytes"
            )));
        }
        if out.len() + n > out.capacity() {
            let grow = (out.capacity().max(n)) as u64;
            crate::traversal::memory_budget::ensure_room("model import", grow, 0)
                .map_err(PersistError::Graph)?;
        }
        out.extend_from_slice(&buffer[..n]);
    }
    Ok(out)
}

/// Hashes a file of the local file system (first pass of a directory import).
#[cfg(not(target_arch = "wasm32"))]
fn hash_path(path: &std::path::Path) -> Result<(u64, String, Vec<u8>), PersistError> {
    use crate::value::vector::embed::sha256::{Sha256, hex};
    let mut file = std::fs::File::open(path)
        .map_err(|err| invalid(format!("cannot read '{}': {err}", path.display())))?;
    let mut hash = Sha256::new();
    let mut head = Vec::new();
    let mut size = 0u64;
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let n = file
            .read(&mut buffer)
            .map_err(|err| invalid(format!("cannot read '{}': {err}", path.display())))?;
        if n == 0 {
            break;
        }
        if head.len() < MAX_HEADER_BYTES {
            let take = n.min(MAX_HEADER_BYTES - head.len());
            head.extend_from_slice(&buffer[..take]);
        }
        hash.update(&buffer[..n]);
        size += n as u64;
        if size > MAX_MODEL_FILE_BYTES {
            return Err(invalid(format!(
                "'{}' is larger than {MAX_MODEL_FILE_BYTES} bytes",
                path.display()
            )));
        }
    }
    Ok((size, hex(&hash.finish()), head))
}

/// The shape `(rows, columns)` of the embedding tensor in a safetensors file's header: an
/// 8-byte little-endian length followed by that many bytes of JSON naming each tensor's
/// `shape` (read with the dependency-free JSON reader; the model loader checks it again).
pub(crate) fn safetensors_shape(head: &[u8]) -> Option<(u64, u64)> {
    use crate::value::physical::{PhysicalValue, read_json};
    let length = u64::from_le_bytes(head.get(..8)?.try_into().ok()?);
    let end = 8usize.checked_add(usize::try_from(length).ok()?)?;
    let json = std::str::from_utf8(head.get(8..end)?).ok()?;
    let PhysicalValue::Object(tensors) = read_json(json, 8).ok()? else {
        return None;
    };
    for wanted in ["embeddings", "0", "embedding.weight"] {
        let Some((_, PhysicalValue::Object(fields))) = tensors.iter().find(|(k, _)| k == wanted)
        else {
            continue;
        };
        let Some((_, PhysicalValue::Array(shape))) = fields.iter().find(|(k, _)| k == "shape")
        else {
            return None;
        };
        let [PhysicalValue::Int(rows), PhysicalValue::Int(cols)] = shape.as_slice() else {
            return None;
        };
        return Some((u64::try_from(*rows).ok()?, u64::try_from(*cols).ok()?));
    }
    None
}

/// The files and metadata of a [`ModelSource::Dir`].
#[cfg(not(target_arch = "wasm32"))]
fn dir_source(dir: &std::path::Path) -> Result<(Vec<SourceFile>, Meta), PersistError> {
    use crate::value::physical::{PhysicalValue, read_json};
    if !dir.is_dir() {
        return Err(invalid(format!(
            "the model directory '{}' does not exist",
            dir.display()
        )));
    }
    let mut meta = Meta::default();
    let manifest_path = dir.join("graphersal-embed-model.json");
    if manifest_path.is_file() {
        let bytes = std::fs::read(&manifest_path)
            .map_err(|err| invalid(format!("cannot read '{}': {err}", manifest_path.display())))?;
        let text = String::from_utf8(bytes).map_err(|_| {
            invalid(format!(
                "the manifest '{}' is not UTF-8",
                manifest_path.display()
            ))
        })?;
        let PhysicalValue::Object(fields) = read_json(&text, 8).map_err(|err| {
            invalid(format!(
                "the manifest '{}' is not JSON: {err}",
                manifest_path.display()
            ))
        })?
        else {
            return Err(invalid(format!(
                "the manifest '{}' is not a JSON object",
                manifest_path.display()
            )));
        };
        let text_of = |key: &str| {
            fields.iter().find_map(|(k, v)| match v {
                PhysicalValue::String(s) if k == key => Some(s.to_string()),
                _ => None,
            })
        };
        meta.repo = text_of("repo");
        meta.revision = text_of("revision");
        meta.commit = text_of("commit");
        meta.license = text_of("license");
        meta.gated = fields
            .iter()
            .any(|(k, v)| k == "gated" && matches!(v, PhysicalValue::Bool(true)));
        if let Some((_, PhysicalValue::Array(bases))) =
            fields.iter().find(|(k, _)| k == "base_models")
        {
            for base in bases {
                if let PhysicalValue::Object(entry) = base {
                    let field = |key: &str| {
                        entry.iter().find_map(|(k, v)| match v {
                            PhysicalValue::String(s) if k == key => Some(s.to_string()),
                            _ => None,
                        })
                    };
                    if let Some(repo) = field("repo")
                        && is_repo_name(&repo)
                    {
                        meta.base_models
                            .push(BaseModel::new(repo, field("license")));
                    }
                }
            }
        }
    }
    // The layouts model2vec-rs reads.
    let sub = dir.join("0_StaticEmbedding");
    let layouts = [
        (dir.join("config.json"), dir.to_path_buf()),
        (
            dir.join("config_sentence_transformers.json"),
            dir.to_path_buf(),
        ),
        (dir.join("config_sentence_transformers.json"), sub),
    ];
    let Some((config, base)) = layouts.into_iter().find(|(config, base)| {
        config.is_file()
            && base.join("tokenizer.json").is_file()
            && base.join("model.safetensors").is_file()
    }) else {
        return Err(invalid(format!(
            "'{}' holds no Model2Vec model (config.json, tokenizer.json and model.safetensors, \
             or the sentence-transformers layout)",
            dir.display()
        )));
    };
    let mut files = Vec::new();
    let mut add = |path: std::path::PathBuf, role: ModelRole| -> Result<(), PersistError> {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let (size, sha256, head) = hash_path(&path)?;
        files.push(SourceFile {
            name,
            role,
            size,
            sha256,
            content: TarContent::Path(path),
            head,
        });
        Ok(())
    };
    add(config, ModelRole::Config)?;
    add(base.join("tokenizer.json"), ModelRole::Tokenizer)?;
    add(base.join("model.safetensors"), ModelRole::Weights)?;
    let card = dir.join("README.md");
    if card.is_file() {
        add(card, ModelRole::Card)?;
    }
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().is_file())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    let mut licences = 0;
    for name in names {
        if !is_model_file_name(&name) || licences >= 8 {
            continue;
        }
        let upper = name.to_ascii_uppercase();
        let role = if upper.starts_with("LICENSE")
            || upper.starts_with("LICENCE")
            || upper.starts_with("COPYING")
        {
            ModelRole::License
        } else if upper.starts_with("NOTICE") {
            ModelRole::Notice
        } else {
            continue;
        };
        licences += 1;
        add(dir.join(&name), role)?;
    }
    Ok((files, meta))
}

/// The files and metadata of a [`ModelSource::Files`].
fn files_source(new: NewModel) -> Result<(Vec<SourceFile>, Meta), PersistError> {
    let parts = new.into_parts();
    let mut files = Vec::with_capacity(parts.files.len());
    for (name, role, mut reader) in parts.files {
        let bytes = read_in_memory(&name, &mut reader)?;
        files.push(SourceFile {
            size: bytes.len() as u64,
            sha256: sha256_hex(&bytes),
            head: bytes[..bytes.len().min(MAX_HEADER_BYTES)].to_vec(),
            content: TarContent::Bytes(bytes),
            name,
            role,
        });
    }
    Ok((
        files,
        Meta {
            repo: parts.repo,
            revision: parts.revision,
            commit: parts.commit,
            license: parts.license,
            base_models: parts.base_models,
            gated: parts.gated,
        },
    ))
}

/// Prepares an import: the files checked, the licence templates added, the manifest built, the
/// identity, dimension and load size computed. Nothing is written.
pub(crate) fn prepare(
    source: ModelSource,
    options: &ImportModelOptions,
) -> Result<Prepared, PersistError> {
    let (mut files, mut meta) = match source {
        #[cfg(not(target_arch = "wasm32"))]
        ModelSource::Dir(dir) => dir_source(&dir)?,
        ModelSource::Files(new) => files_source(new)?,
    };
    match (&options.repo, &meta.repo) {
        (Some(given), Some(stated)) if given != stated => {
            return Err(invalid(format!(
                "the repository {given:?} differs from the one the model's manifest names \
                 ({stated:?})"
            )));
        }
        (Some(given), _) => meta.repo = Some(given.clone()),
        (None, _) => {}
    }
    let Some(repo) = meta.repo.clone() else {
        return Err(invalid(
            "the model has no repository name: name its Hugging Face repository \
             (ImportModelOptions::with_repo; CLI --repo)",
        ));
    };
    if !is_repo_name(&repo) {
        return Err(invalid(format!(
            "{repo:?} is not a Hugging Face repository name (owner/name)"
        )));
    }
    if meta.license.is_none() {
        meta.license.clone_from(&options.license);
    }
    let license = meta
        .license
        .clone()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    let label_ok = |text: &str| text.len() <= 64 && text.chars().all(|c| c.is_ascii_graphic());
    if !label_ok(&license) {
        return Err(invalid(format!(
            "the licence {license:?} is not an SPDX id"
        )));
    }
    for base in &meta.base_models {
        if !is_repo_name(&base.repo) || !base.license.as_deref().is_none_or(label_ok) {
            return Err(invalid(format!(
                "the base model {:?} is not a repository name",
                base.repo
            )));
        }
    }
    for text in [&meta.revision, &meta.commit].into_iter().flatten() {
        if text.len() > 255 || text.chars().any(char::is_control) {
            return Err(invalid("a revision or commit that is not plain text"));
        }
    }
    if files.len() > MAX_MODEL_FILES {
        return Err(invalid(format!("at most {MAX_MODEL_FILES} files")));
    }
    for (index, file) in files.iter().enumerate() {
        if !is_model_file_name(&file.name) {
            return Err(invalid(format!(
                "the file name {:?} is not allowed (1 to 40 ASCII letters, digits, '.', '_' and \
                 '-', not starting with '.', not the manifest's name)",
                file.name
            )));
        }
        if files[..index].iter().any(|other| other.name == file.name) {
            return Err(invalid(format!("the file {:?} is given twice", file.name)));
        }
        if file.size > MAX_MODEL_FILE_BYTES {
            return Err(invalid(format!(
                "the file {:?} is larger than {MAX_MODEL_FILE_BYTES} bytes",
                file.name
            )));
        }
    }
    for role in [ModelRole::Config, ModelRole::Tokenizer, ModelRole::Weights] {
        let count = files.iter().filter(|f| f.role == role).count();
        if count != 1 {
            return Err(invalid(format!(
                "a Model2Vec model needs exactly one {} file ({count} given)",
                role.as_str()
            )));
        }
    }
    // The licence text: the repository's file, else the standard text of MIT or Apache-2.0;
    // every other base-model licence with a template as LICENSE-BASE-<licence>.
    let has_license_file = files.iter().any(|f| f.role == ModelRole::License);
    let add_text = |files: &mut Vec<SourceFile>, name: String, text: String| {
        let bytes = text.into_bytes();
        files.push(SourceFile {
            name,
            role: ModelRole::License,
            size: bytes.len() as u64,
            sha256: sha256_hex(&bytes),
            head: Vec::new(),
            content: TarContent::Bytes(bytes),
        });
    };
    let license_text = if has_license_file {
        "file"
    } else if let Some(template) = licenses::template(&license, Some(&repo))
        && !files.iter().any(|f| f.name == "LICENSE")
        && files.len() < MAX_MODEL_FILES
    {
        add_text(&mut files, "LICENSE".to_string(), template);
        "template"
    } else {
        "none"
    };
    let mut base_licenses: Vec<String> = Vec::new();
    for base in &meta.base_models {
        let Some(other) = base.license.as_deref().map(str::to_ascii_lowercase) else {
            continue;
        };
        if other != license.to_ascii_lowercase() && !base_licenses.contains(&other) {
            base_licenses.push(other);
        }
    }
    for other in base_licenses {
        let bases: Vec<&str> = meta
            .base_models
            .iter()
            .filter(|b| b.license.as_deref().map(str::to_ascii_lowercase) == Some(other.clone()))
            .map(|b| b.repo.as_str())
            .collect();
        let name = format!("LICENSE-BASE-{other}");
        let Some(text) = licenses::base_template(&other, &bases, Some(&repo)) else {
            continue;
        };
        if !is_model_file_name(&name)
            || files.iter().any(|f| f.name == name)
            || files.len() >= MAX_MODEL_FILES
        {
            continue;
        }
        add_text(&mut files, name, text);
    }
    let by_role = |role: ModelRole| files.iter().find(|f| f.role == role);
    let (Some(config), Some(tokenizer), Some(weights)) = (
        by_role(ModelRole::Config),
        by_role(ModelRole::Tokenizer),
        by_role(ModelRole::Weights),
    ) else {
        return Err(invalid("the model files are incomplete"));
    };
    let digest = identity_digest([
        (config.size, &config.sha256),
        (tokenizer.size, &tokenizer.sha256),
        (weights.size, &weights.sha256),
    ]);
    debug_assert_eq!(IDENTITY_ROLES, ["config", "tokenizer", "model"]);
    let identity = identity_text(Some(&repo), &digest);
    let Some((rows, cols)) = safetensors_shape(&weights.head) else {
        return Err(invalid(format!(
            "the weights {:?} are not a safetensors file with an embedding tensor of two \
             dimensions",
            weights.name
        )));
    };
    let load_bytes = rows
        .saturating_mul(cols)
        .saturating_mul(4)
        .saturating_add(tokenizer.size);
    let manifest = ModelManifest {
        family: FAMILY.to_string(),
        identity: identity.clone(),
        repo,
        revision: meta.revision,
        commit: meta.commit,
        license,
        license_text: license_text.to_string(),
        base_models: meta
            .base_models
            .iter()
            .map(|b| ManifestBase {
                repo: b.repo.clone(),
                license: b.license.clone(),
            })
            .collect(),
        gated: meta.gated,
        files: {
            let mut listed: Vec<ManifestFile> = files
                .iter()
                .map(|f| ManifestFile {
                    name: f.name.clone(),
                    role: f.role.as_str().to_string(),
                    size: f.size,
                    sha256: f.sha256.clone(),
                })
                .collect();
            listed.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
            listed
        },
    };
    let manifest_bytes = manifest.to_bytes();
    let mut tar_files: Vec<TarFile> = files
        .into_iter()
        .map(|f| TarFile {
            name: f.name,
            size: f.size,
            sha256: f.sha256,
            content: f.content,
        })
        .collect();
    tar_files.push(TarFile {
        name: model_tar::MODEL_MANIFEST.to_string(),
        size: manifest_bytes.len() as u64,
        sha256: sha256_hex(&manifest_bytes),
        content: TarContent::Bytes(manifest_bytes),
    });
    Ok(Prepared {
        manifest,
        files: tar_files,
        identity,
        dimension: cols,
        load_bytes,
    })
}

/// The warnings about passing a model on.
fn licence_warnings(manifest: &ModelManifest) -> Vec<String> {
    let mut out = Vec::new();
    let lower = manifest.license.to_ascii_lowercase();
    let repo = &manifest.repo;
    if lower == "unknown" {
        out.push(format!(
            "the licence of {repo} is unknown: check the repository before you hand on a store \
             or file that carries the model"
        ));
    } else if lower.contains("-nc")
        || lower.contains("noncommercial")
        || lower.contains("non-commercial")
    {
        out.push(format!(
            "{repo} is under {}, a NON-COMMERCIAL licence: a store or file carrying it may not \
             be used or handed on commercially",
            manifest.license
        ));
    }
    if manifest.gated {
        out.push(format!(
            "{repo} is a GATED repository: the terms you accepted to download it may restrict \
             passing it on"
        ));
    }
    if manifest.license_text == "template" {
        out.push(format!(
            "the repository has no licence file; the standard {} text was put beside the model \
             (LICENSE, marked as a template)",
            manifest.license
        ));
    }
    out
}

// ---------------------------------------------------------------------------------------------
// The Store API
// ---------------------------------------------------------------------------------------------

/// The model definitions and the schema of the graph (one read lock).
fn catalog_of<S: PersistentStorage>(
    graph: &crate::graph::Graph<S>,
) -> Result<(Vec<Definition>, Option<GraphSchema>), PersistError> {
    let read = graph.read_checked().map_err(PersistError::Graph)?;
    let definitions = model_definitions(&*read).map_err(PersistError::Graph)?;
    let schema = read.get_schema().map_err(PersistError::Graph)?;
    Ok((definitions, schema))
}

/// The refusal of an operation on a model the store does not hold.
pub(crate) fn unknown(operation: &'static str, repo: &str) -> PersistError {
    PersistError::store(
        operation,
        StoreFailure::UnknownModel,
        format!("the store holds no model {repo:?} at its current position"),
    )
}

// ---------------------------------------------------------------------------------------------
// The planner: the catalog changes of a model change, shared by the open store (applied in one
// unit) and the closed store (one commit appended to the WAL, `closed_commit.rs`)
// ---------------------------------------------------------------------------------------------

/// The model definitions (catalog kind 5) of a catalog.
pub(crate) fn model_definitions<G: GraphStorage + ?Sized>(
    g: &G,
) -> Result<Vec<Definition>, GraphError> {
    Ok(g.definitions()?
        .filter(|d| d.kind() == DefinitionKind::MODEL)
        .cloned()
        .collect())
}

/// The definition of the model of `repo` in `definitions`.
pub(crate) fn of_repo<'a>(definitions: &'a [Definition], repo: &str) -> Option<&'a Definition> {
    definitions
        .iter()
        .find(|d| d.as_model().is_some_and(|m| m.repo() == repo))
}

/// The default model's definition in `definitions`.
fn default_of(definitions: &[Definition]) -> Option<&Definition> {
    definitions
        .iter()
        .find(|d| d.as_model().is_some_and(ModelDefinition::is_default))
}

/// What an import of the model `identity` of `repo` finds in the catalog.
pub(crate) enum ImportPlan {
    /// The same identity is defined already: nothing is imported (at most the default changes).
    Present,
    /// A new definition; `old` is the revision of the same repository it replaces;
    /// `replaces_rules`: vector rules name `old`, so the replacing unit re-embeds their
    /// properties (it needs the data: book page "Vector Rules").
    New {
        old: Option<Definition>,
        replaces_rules: bool,
    },
}

/// Plans an import (format spec 4.5): the same identity present, or a new revision, refused
/// while the store holds another revision of the repository and `replace` is not set
/// ([`StoreFailure::ModelRevisionExists`]), and while a schema property's vectors were made by
/// the revision it would replace without a vector rule ([`StoreFailure::ModelInUse`], K3).
pub(crate) fn plan_import(
    definitions: &[Definition],
    schema: Option<&GraphSchema>,
    identity: &str,
    repo: &str,
    replace: bool,
) -> Result<ImportPlan, PersistError> {
    const OP: &str = "import a model";
    if definitions
        .iter()
        .any(|d| d.name() == identity && d.as_model().is_some())
    {
        return Ok(ImportPlan::Present);
    }
    let old = of_repo(definitions, repo).cloned();
    if let Some(old) = &old {
        let using = needed_by(schema, old.name());
        if !replace {
            return Err(PersistError::store(
                OP,
                StoreFailure::ModelRevisionExists,
                format!(
                    "the store holds the revision {} of {repo}; the import is the revision \
                     {identity}. {}",
                    old.name(),
                    if using.is_empty() {
                        "No schema property uses the current revision: replacing it changes \
                         no data"
                            .to_string()
                    } else {
                        format!(
                            "The vectors of {} were made by the current revision: a replace \
                             re-embeds them",
                            using.join(", ")
                        )
                    }
                ),
            ));
        }
        if !using.is_empty() {
            // K3: vectors without a rule have no source text to re-embed.
            let unruled = crate::graph::vector_rules::places_without_rule(schema, old.name());
            if !unruled.is_empty() {
                return Err(PersistError::store(
                    OP,
                    StoreFailure::ModelInUse,
                    format!(
                        "the vectors of {} were made by {} and have no vector rule \
                         (\"from\"), so a replace cannot re-embed them: add the rule \
                         first (\"from\": \"<source property>\" beside \"embedder\"), \
                         or remove \"embedder\" from them",
                        unruled.join(", "),
                        old.name()
                    ),
                ));
            }
            // K1: the rules re-embed the properties in the replacing unit, which needs the
            // data: an open store does it in its unit, a closed store is opened for it.
            return Ok(ImportPlan::New {
                old: Some(old.clone()),
                replaces_rules: true,
            });
        }
    }
    Ok(ImportPlan::New {
        old,
        replaces_rules: false,
    })
}

/// The changes clearing the default flag of every model but `keep` and `removed`.
fn clear_default_changes(
    definitions: &[Definition],
    keep: &str,
    removed: Option<&str>,
) -> Vec<Mutation> {
    definitions
        .iter()
        .filter(|d| d.name() != keep && Some(d.name()) != removed)
        .filter_map(|d| {
            d.as_model().filter(|m| m.is_default()).map(|m| {
                Mutation::set_definition(
                    DefinitionKind::MODEL,
                    d.name(),
                    Some(d.clone()),
                    Some(Definition::model(d.name(), m.clone().with_default(false))),
                )
            })
        })
        .collect()
}

/// The changes of an import (format spec 4.5): the replaced revision `old` dropped, the default
/// flag of every other model cleared when `new` is the default, `new` defined.
pub(crate) fn import_changes(
    definitions: &[Definition],
    old: Option<&Definition>,
    new: Definition,
) -> Vec<Mutation> {
    let mut changes = Vec::new();
    if let Some(old) = old {
        changes.push(Mutation::set_definition(
            DefinitionKind::MODEL,
            old.name(),
            Some(old.clone()),
            None,
        ));
    }
    if new.as_model().is_some_and(ModelDefinition::is_default) {
        changes.extend(clear_default_changes(
            definitions,
            new.name(),
            old.map(Definition::name),
        ));
    }
    let name = new.name().to_string();
    changes.push(Mutation::set_definition(
        DefinitionKind::MODEL,
        name,
        None,
        Some(new),
    ));
    changes
}

/// The changes making the model `target` (a definition name) the default, or clearing the
/// default (`None`); empty when nothing changes.
pub(crate) fn default_changes(definitions: &[Definition], target: Option<&str>) -> Vec<Mutation> {
    if default_of(definitions).map(Definition::name) == target {
        return Vec::new();
    }
    let mut changes = clear_default_changes(definitions, target.unwrap_or_default(), None);
    if let Some(target) = target
        && let Some(definition) = definitions.iter().find(|d| d.name() == target)
        && let Some(model) = definition.as_model()
    {
        changes.push(Mutation::set_definition(
            DefinitionKind::MODEL,
            target,
            Some(definition.clone()),
            Some(Definition::model(target, model.clone().with_default(true))),
        ));
    }
    changes
}

/// Plans the removal of the model of `repo`: its definition, refused for a repository the
/// catalog does not hold and while the schema needs it ([`StoreFailure::ModelInUse`]).
pub(crate) fn plan_remove(
    definitions: &[Definition],
    schema: Option<&GraphSchema>,
    repo: &str,
) -> Result<Definition, PersistError> {
    const OP: &str = "remove a model";
    let definition = of_repo(definitions, repo).ok_or_else(|| unknown(OP, repo))?;
    let using = needed_by(schema, definition.name());
    if !using.is_empty() {
        return Err(PersistError::store(
            OP,
            StoreFailure::ModelInUse,
            format!(
                "the schema's vector properties {} use the model {} ({repo})",
                using.join(", "),
                definition.name()
            ),
        ));
    }
    Ok(definition.clone())
}

/// The change removing `definition`.
pub(crate) fn remove_changes(definition: &Definition) -> Vec<Mutation> {
    vec![Mutation::set_definition(
        definition.kind(),
        definition.name(),
        Some(definition.clone()),
        None,
    )]
}

/// Applies planned catalog changes inside an open unit.
fn apply_changes<G: GraphStorage + ?Sized>(
    g: &mut G,
    changes: &[Mutation],
) -> Result<(), GraphError> {
    for change in changes {
        if let Mutation::SetDefinition {
            kind, name, after, ..
        } = change
        {
            match after {
                Some(after) => g.set_definition((**after).clone())?,
                None => {
                    g.remove_definition(*kind, name)?;
                }
            }
        }
    }
    Ok(())
}

/// The journal event of an import that defined `definition` (replacing the revision named
/// `replaced`).
pub(crate) fn import_event(
    definition: &Definition,
    replaced: Option<&str>,
) -> (&'static catalogue::EventKind, Event) {
    let identity = definition.name();
    let (repo, bytes, default) = definition
        .as_model()
        .map_or((String::new(), 0, false), |m| {
            (m.repo().to_string(), m.blob().length(), m.is_default())
        });
    match replaced {
        Some(old) => (
            &catalogue::MODEL_REPLACED,
            Event::new(format!(
                "the model {repo} was replaced by the revision {identity}"
            ))
            .field("repo", repo.as_str())
            .field("old_identity", old)
            .field("new_identity", identity),
        ),
        None => (
            &catalogue::MODEL_IMPORTED,
            Event::new(format!("the model {repo} was imported"))
                .field("repo", repo.as_str())
                .field("identity", identity)
                .count("bytes", bytes)
                .field("default", default),
        ),
    }
}

/// The journal event of a default change from the repository `from` to `to`.
pub(crate) fn default_event(
    from: Option<&str>,
    to: Option<&str>,
) -> (&'static catalogue::EventKind, Event) {
    (
        &catalogue::MODEL_DEFAULT_CHANGED,
        Event::new(format!(
            "the default model changed from {} to {}",
            from.unwrap_or("none"),
            to.unwrap_or("none")
        ))
        .field("from", from.unwrap_or(""))
        .field("to", to.unwrap_or("")),
    )
}

/// The journal event of the removal of `definition`.
pub(crate) fn remove_event(definition: &Definition) -> (&'static catalogue::EventKind, Event) {
    let repo = definition
        .as_model()
        .map(|m| m.repo().to_string())
        .unwrap_or_default();
    (
        &catalogue::MODEL_REMOVED,
        Event::new(format!("the model {repo} was removed"))
            .field("repo", repo.as_str())
            .field("identity", definition.name()),
    )
}

/// The repository of the default model in `definitions`.
pub(crate) fn default_repo(definitions: &[Definition]) -> Option<String> {
    default_of(definitions).and_then(|d| d.as_model().map(|m| m.repo().to_string()))
}

/// Writes the files of a prepared import as one blob through `write` (the open store's
/// [`Store::write_blob`], or the closed store's blob writer) and checks the model by a load
/// where the build can run it (a failed load refuses the import; the blob stays an orphan for
/// the next prune). Returns the definition (with the `default` flag) and the warnings of the
/// check.
pub(crate) fn write_model_blob(
    prepared: Prepared,
    default: bool,
    write: &mut dyn FnMut(&mut dyn Read) -> Result<BlobRef, PersistError>,
    dir: &DirPath,
    journal: Option<&Arc<crate::graph::system_journal::SystemJournal>>,
) -> Result<(Definition, Vec<String>), PersistError> {
    let blob_length = prepared.tar_length();
    let Prepared {
        manifest,
        files,
        identity,
        dimension,
        load_bytes,
    } = prepared;
    let mut stream = TarStream::new(files);
    let blob = write(&mut stream)?;
    if blob.length() != blob_length {
        return Err(invalid(format!(
            "the model's tar is {} bytes, {blob_length} were expected",
            blob.length()
        )));
    }
    let prepared = Prepared {
        manifest,
        files: Vec::new(),
        identity,
        dimension,
        load_bytes,
    };
    let definition = prepared.definition(blob, default);
    let check_source = StoreModelBytes::new(dir.clone());
    if let Some(journal) = journal {
        let _ = check_source.journal.set(Arc::downgrade(journal));
    }
    let check_source: Arc<dyn registry::ModelBytes> = Arc::new(check_source);
    let mut warnings = Vec::new();
    match registry::check_load(&check_source, &definition) {
        registry::CheckLoad::Loaded | registry::CheckLoad::NotAvailable => {}
        registry::CheckLoad::Skipped(reason) => warnings.push(reason),
        registry::CheckLoad::Failed(reason) => {
            return Err(invalid(format!("the model does not load: {reason}")));
        }
    }
    Ok((definition, warnings))
}

/// The metadata of a model change's commit.
fn metadata_of(principal: Option<&str>) -> CommitMetadata {
    match principal {
        Some(principal) => CommitMetadata::new().with_principal(principal.to_string()),
        None => CommitMetadata::new(),
    }
}

impl ImportModelOptions {
    /// Whether a new revision replaces the current one (crate-internal reader).
    pub(crate) fn replace(&self) -> bool {
        self.replace
    }

    /// Whether the model becomes the default (crate-internal reader).
    pub(crate) fn default_model(&self) -> bool {
        self.default
    }

    /// The principal of the commit (crate-internal reader).
    pub(crate) fn principal(&self) -> Option<&str> {
        self.principal.as_deref()
    }
}

/// The licence warnings of a prepared import (crate-internal: the closed-store import).
pub(crate) fn warnings_of(prepared: &Prepared) -> Vec<String> {
    licence_warnings(&prepared.manifest)
}

impl<S: PersistentStorage + Default> Store<S> {
    /// The embedding models of the store's CURRENT position (format spec 4.5): each with its
    /// repository (its name), identity, licence, size, whether it is the default, its state
    /// (loaded, not loaded, damaged, missing, unsupported) and the schema properties that need
    /// it. A view of a past position has the models of that moment (its catalog).
    pub fn models(&self) -> Result<Vec<ModelInfo>, PersistError> {
        let (definitions, schema) = catalog_of(&self.graph)?;
        let dir = &self.core.shared.dir;
        let registry = self.graph.model_registry();
        Ok(infos_of(&definitions, schema.as_ref(), &|model| {
            let state = registry
                .as_ref()
                .map(|r| r.state_of(model.blob()))
                .unwrap_or(ModelLoadState::NotLoaded);
            match state {
                ModelLoadState::Loaded => ModelState::Loaded,
                ModelLoadState::Failed(reason) => ModelState::Damaged { reason },
                ModelLoadState::Unsupported(reason) => ModelState::Unsupported { reason },
                ModelLoadState::NotLoaded => blob_state(dir, model.blob()),
            }
        }))
    }

    /// Imports an embedding model into this open store (the explicit command a model enters a
    /// store by; book page "Embedding Models in a Store"): its files are written as ONE blob (a
    /// deterministic tar with a manifest, format spec 4.5), then one commit defines it (catalog
    /// kind 5, named by its identity `model2vec/<repo>@<12 hex>`). The same identity defined
    /// already commits nothing ([`ImportModelReport::already_present`]; with
    /// [`ImportModelOptions::with_default`] only the default changes). Another revision of the
    /// same repository is refused ([`StoreFailure::ModelRevisionExists`]) unless
    /// [`ImportModelOptions::with_replace`]: then the new revision replaces the old one in the
    /// same commit (and becomes the default if the old one was); the vector rules naming the old
    /// revision switch to the new one and re-embed every element of their labels in that
    /// commit, under the write lock (book page "Vector Rules"); a property whose vectors the
    /// old revision made WITHOUT a rule refuses the replace ([`StoreFailure::ModelInUse`],
    /// naming it). In a build that can run the model (feature
    /// `model2vec`, native) it is loaded once as a check, when it fits under the memory
    /// ceiling. Refused on a read-only store, a backup and in maintenance mode. A CLOSED store
    /// takes the same import without loading its graph: [`Store::import_model_dir`].
    pub fn import_model(
        &self,
        source: ModelSource,
        options: ImportModelOptions,
    ) -> Result<ImportModelReport, PersistError> {
        self.core.writable("import a model")?;
        let prepared = prepare(source, &options)?;
        self.import_prepared(prepared, &options)
    }

    /// [`Store::import_model`] of a prepared import (also the closed store's replace that must
    /// re-embed, `Store::import_model_dir`).
    pub(crate) fn import_prepared(
        &self,
        prepared: Prepared,
        options: &ImportModelOptions,
    ) -> Result<ImportModelReport, PersistError> {
        const OP: &str = "import a model";
        self.core.writable(OP)?;
        let mut warnings = licence_warnings(&prepared.manifest);
        let (definitions, schema) = catalog_of(&self.graph)?;
        let identity = prepared.identity.clone();
        let repo = prepared.manifest.repo.clone();
        let dir = self.core.shared.dir.clone();
        let (old, replaces_rules) = match plan_import(
            &definitions,
            schema.as_ref(),
            &identity,
            &repo,
            options.replace,
        )? {
            ImportPlan::Present => {
                // The same model: at most the default changes.
                let mut commit_seq = None;
                if options.default {
                    commit_seq = self.change_default(Some(&repo), options.principal.as_deref())?;
                }
                let (definitions, schema) = catalog_of(&self.graph)?;
                let info = definitions
                    .iter()
                    .find(|d| d.name() == identity)
                    .and_then(|d| {
                        d.as_model()
                            .map(|m| model_info(d, m, schema.as_ref(), blob_state(&dir, m.blob())))
                    })
                    .ok_or_else(|| unknown(OP, &repo))?;
                return Ok(ImportModelReport {
                    model: info,
                    already_present: true,
                    commit_seq,
                    replaced: None,
                    reembedded: Vec::new(),
                    warnings,
                });
            }
            ImportPlan::New {
                old,
                replaces_rules,
            } => (old, replaces_rules),
        };
        let default = options.default
            || old
                .as_ref()
                .is_some_and(|o| o.as_model().is_some_and(ModelDefinition::is_default));
        let mut pending = None;
        let dimension = prepared.dimension;
        let (definition, checked) = write_model_blob(
            prepared,
            default,
            &mut |reader| {
                let written = self.write_blob(reader)?;
                let blob = written.blob().clone();
                pending = Some(written);
                Ok(blob)
            },
            &dir,
            Some(&self.core.shared.system_journal),
        )?;
        warnings.extend(checked);
        let old_name = old.as_ref().map(|o| o.name().to_string());
        let reembedded = match &old_name {
            Some(old_name) if replaces_rules => needed_by(schema.as_ref(), old_name),
            _ => Vec::new(),
        };
        // The rule pass of the replacing unit embeds with the new revision: the registry
        // serves it before the commit (its hook rebuilds it again after).
        let registry = self.graph.model_registry().filter(|_| replaces_rules);
        if let Some(registry) = &registry {
            let mut next: Vec<Definition> = definitions
                .iter()
                .filter(|d| Some(d.name()) != old_name.as_deref())
                .cloned()
                .collect();
            next.push(definition.clone());
            registry.rebuild(next);
        }
        let committed = (|| {
            let mut write = self.graph.write_checked().map_err(PersistError::Graph)?;
            write
                .transaction_with(metadata_of(options.principal.as_deref()), |g| {
                    // Planned again inside the unit: the changes continue the catalog as it is.
                    let definitions = model_definitions(g)?;
                    let old = of_repo(&definitions, &repo).filter(|d| d.name() != identity);
                    let changes = import_changes(&definitions, old, definition.clone());
                    apply_changes(g, &changes)?;
                    if replaces_rules
                        && let Some(old_name) = &old_name
                        && let Some(current) = g.get_schema()?
                        && let Some(next) = crate::graph::vector_rules::replace_model_in_schema(
                            &current, old_name, &identity, dimension,
                        )
                    {
                        g.set_schema(next)?;
                    }
                    Ok::<(), GraphError>(())
                })
                .map_err(PersistError::Graph)?;
            write.last_commit_seq().map_err(PersistError::Graph)
        })();
        let commit_seq = match committed {
            Ok(seq) => seq,
            Err(err) => {
                if let Some(registry) = &registry
                    && let Ok((current, _)) = catalog_of(&self.graph)
                {
                    registry.rebuild(current);
                }
                return Err(err);
            }
        };
        drop(pending);
        let schema = catalog_of(&self.graph)?.1;
        let model = definition
            .as_model()
            .map(|m| model_info(&definition, m, schema.as_ref(), blob_state(&dir, m.blob())))
            .ok_or_else(|| unknown(OP, &repo))?;
        let replaced = old.as_ref().and_then(|o| {
            o.as_model()
                .map(|m| model_info(o, m, schema.as_ref(), blob_state(&dir, m.blob())))
        });
        let (kind, event) = import_event(&definition, old.as_ref().map(Definition::name));
        self.core
            .shared
            .system_journal
            .emit(kind, event.commit(commit_seq));
        Ok(ImportModelReport {
            model,
            already_present: false,
            commit_seq: Some(commit_seq),
            replaced,
            reembedded,
            warnings,
        })
    }

    /// Makes the model of the repository `repo` the default model (the one `embed` and text
    /// queries use without a model name), or clears the default (`None`): one commit. The
    /// default follows the history like the models. Nothing changes (no commit) when it is the
    /// default already. Refused for a repository the store does not hold
    /// ([`StoreFailure::UnknownModel`]) and on a read-only store. A CLOSED store:
    /// [`Store::set_default_model_dir`].
    pub fn set_default_model(&self, repo: Option<&str>) -> Result<ModelChangeReport, PersistError> {
        const OP: &str = "set the default model";
        self.core.writable(OP)?;
        let (definitions, _) = catalog_of(&self.graph)?;
        if let Some(repo) = repo
            && of_repo(&definitions, repo).is_none()
        {
            return Err(unknown(OP, repo));
        }
        let Some(commit_seq) = self.change_default(repo, None)? else {
            return Ok(ModelChangeReport {
                commit_seq: None,
                committed_at: None,
                model: None,
            });
        };
        let (definitions, schema) = catalog_of(&self.graph)?;
        let dir = &self.core.shared.dir;
        let model = repo.and_then(|repo| {
            of_repo(&definitions, repo).and_then(|d| {
                d.as_model()
                    .map(|m| model_info(d, m, schema.as_ref(), blob_state(dir, m.blob())))
            })
        });
        Ok(ModelChangeReport {
            commit_seq: Some(commit_seq),
            committed_at: Some(self.last_committed_at()?),
            model,
        })
    }

    /// The time of the store's last commit.
    fn last_committed_at(&self) -> Result<i64, PersistError> {
        let read = self.graph.read_checked().map_err(PersistError::Graph)?;
        Ok(PersistentStorage::last_committed_at(&*read))
    }

    /// One commit making `repo` the default (or clearing it), with the journal record; `None`
    /// when it is the default already (nothing committed).
    fn change_default(
        &self,
        repo: Option<&str>,
        principal: Option<&str>,
    ) -> Result<Option<u64>, PersistError> {
        let (definitions, _) = catalog_of(&self.graph)?;
        let from = default_repo(&definitions);
        let mut changed = false;
        let commit_seq = {
            let mut write = self.graph.write_checked().map_err(PersistError::Graph)?;
            write
                .transaction_with(metadata_of(principal), |g| {
                    let definitions = model_definitions(g)?;
                    let target = repo.and_then(|repo| of_repo(&definitions, repo));
                    let changes = default_changes(&definitions, target.map(Definition::name));
                    changed = !changes.is_empty();
                    apply_changes(g, &changes)
                })
                .map_err(PersistError::Graph)?;
            write.last_commit_seq().map_err(PersistError::Graph)?
        };
        if !changed {
            return Ok(None);
        }
        let (kind, event) = default_event(from.as_deref(), repo);
        self.core
            .shared
            .system_journal
            .emit(kind, event.commit(commit_seq));
        Ok(Some(commit_seq))
    }

    /// Removes the model of the repository `repo` from the store's current state: one commit
    /// dropping its definition (earlier positions keep it; its blob is removed by the prune
    /// that drops the last history referencing it). Refused while the current schema needs it
    /// ([`StoreFailure::ModelInUse`], naming the properties), for a repository the store does
    /// not hold ([`StoreFailure::UnknownModel`]) and on a read-only store. A CLOSED store:
    /// [`Store::remove_model_dir`].
    pub fn remove_model(&self, repo: &str) -> Result<ModelChangeReport, PersistError> {
        const OP: &str = "remove a model";
        self.core.writable(OP)?;
        let (definitions, schema) = catalog_of(&self.graph)?;
        let definition = plan_remove(&definitions, schema.as_ref(), repo)?;
        let dir = &self.core.shared.dir;
        let info = definition
            .as_model()
            .map(|m| model_info(&definition, m, schema.as_ref(), blob_state(dir, m.blob())));
        let name = definition.name().to_string();
        let commit_seq = {
            let mut write = self.graph.write_checked().map_err(PersistError::Graph)?;
            write
                .transaction(|g| -> Result<bool, GraphError> {
                    g.remove_definition(DefinitionKind::MODEL, &name)
                })
                .map_err(PersistError::Graph)?;
            write.last_commit_seq().map_err(PersistError::Graph)?
        };
        let (kind, event) = remove_event(&definition);
        self.core
            .shared
            .system_journal
            .emit(kind, event.commit(commit_seq));
        Ok(ModelChangeReport {
            commit_seq: Some(commit_seq),
            committed_at: Some(self.last_committed_at()?),
            model: info,
        })
    }
}

/// The model definitions and the schema of the latest state in the files of `dir` (a closed
/// store, a copy, a backup): the newest snapshot's definitions table with the WAL's catalog
/// and schema changes after it applied.
pub(crate) fn catalog_in_files(
    dir: &DirPath,
) -> Result<(Vec<Definition>, Option<GraphSchema>), PersistError> {
    super::maintenance::models_in_files(dir)
}

/// The models of the latest state in the files of `dir` with their blob states (by length).
pub(crate) fn infos_in_files(dir: &DirPath) -> Result<Vec<ModelInfo>, PersistError> {
    let (definitions, schema) = catalog_in_files(dir)?;
    Ok(infos_of(&definitions, schema.as_ref(), &|model| {
        blob_state(dir, model.blob())
    }))
}

/// The store's model blobs reader for the registry (`value/vector/embed/registry.rs`).
pub(crate) struct StoreModelBytes {
    pub(crate) dir: DirPath,
    /// Set once the store's system journal exists (after the open's checks).
    pub(crate) journal:
        std::sync::OnceLock<std::sync::Weak<crate::graph::system_journal::SystemJournal>>,
}

impl StoreModelBytes {
    pub(crate) fn new(dir: DirPath) -> Self {
        StoreModelBytes {
            dir,
            journal: std::sync::OnceLock::new(),
        }
    }
}

impl registry::ModelBytes for StoreModelBytes {
    fn read(&self, blob: &BlobRef) -> Result<Vec<u8>, registry::LoadFailure> {
        blobs::read_blob(&self.dir, blob).map_err(|err| match err {
            blobs::BlobError::Fault(blobs::BlobFault::Missing) => registry::LoadFailure::Missing,
            blobs::BlobError::Fault(fault) => {
                registry::LoadFailure::Damaged(format!("its blob is damaged ({})", fault.name()))
            }
            blobs::BlobError::Unsupported => registry::LoadFailure::Unsupported(
                "its blob uses a hash algorithm this version does not know".to_string(),
            ),
            blobs::BlobError::DoesNotFit(err) => registry::LoadFailure::does_not_fit(err),
            blobs::BlobError::Io(err) => {
                registry::LoadFailure::Damaged(format!("its blob cannot be read: {err}"))
            }
        })
    }

    fn check(&self, blob: &BlobRef) -> Result<(), registry::LoadFailure> {
        blobs::check_blob(&self.dir, blob, false).map_err(|err| match err {
            blobs::BlobError::Fault(blobs::BlobFault::Missing) => registry::LoadFailure::Missing,
            blobs::BlobError::Fault(fault) => {
                registry::LoadFailure::Damaged(format!("its blob is damaged ({})", fault.name()))
            }
            blobs::BlobError::Unsupported => registry::LoadFailure::Unsupported(
                "its blob uses a hash algorithm this version does not know".to_string(),
            ),
            blobs::BlobError::DoesNotFit(err) => registry::LoadFailure::does_not_fit(err),
            blobs::BlobError::Io(err) => {
                registry::LoadFailure::Damaged(format!("its blob cannot be read: {err}"))
            }
        })
    }

    fn journal(&self) -> Option<Arc<crate::graph::system_journal::SystemJournal>> {
        self.journal.get().and_then(std::sync::Weak::upgrade)
    }
}

// ---------------------------------------------------------------------------------------------
// The open: the models the schema needs
// ---------------------------------------------------------------------------------------------

/// The models of the live graph of an open store, with their states.
pub(crate) fn infos_live(
    dir: &DirPath,
    graph: &dyn super::StoreGraph,
) -> Result<Vec<ModelInfo>, PersistError> {
    let (definitions, schema) = live_catalog(graph)?;
    let registry = graph.model_registry();
    Ok(infos_of(&definitions, schema.as_ref(), &|model| {
        let state = registry
            .as_ref()
            .map(|r| r.state_of(model.blob()))
            .unwrap_or(ModelLoadState::NotLoaded);
        match state {
            ModelLoadState::Loaded => ModelState::Loaded,
            ModelLoadState::Failed(reason) => ModelState::Damaged { reason },
            ModelLoadState::Unsupported(reason) => ModelState::Unsupported { reason },
            ModelLoadState::NotLoaded => blob_state(dir, model.blob()),
        }
    }))
}

/// The model definitions and the schema of a store's live graph (one read lock).
pub(crate) fn live_catalog(
    graph: &dyn super::StoreGraph,
) -> Result<(Vec<Definition>, Option<GraphSchema>), PersistError> {
    let read = graph.read();
    Ok((read.model_definitions()?, read.schema()?))
}

/// What the open found about the models ([`check_at_open`]).
pub(crate) struct ModelsAtOpen {
    /// The registry to attach to the graph (`None`: the catalog holds no model).
    pub(crate) registry: Option<Arc<registry::ModelRegistry>>,
    /// Where the registry reads blobs; its journal is set once the store's exists.
    pub(crate) source: Option<Arc<StoreModelBytes>>,
    /// The store opens read-only for this reason (a model the schema needs that this build
    /// cannot run, or of a newer family).
    pub(crate) read_only: Option<String>,
    /// The journal records to write once the store's journal exists.
    pub(crate) events: Vec<(
        &'static crate::graph::system_journal::catalogue::EventKind,
        Event,
    )>,
}

impl ModelsAtOpen {
    /// Attaches the registry to `graph` (and its commit hook when `writable`) and writes the
    /// deferred journal records into `journal`.
    pub(crate) fn finish(
        self,
        graph: &dyn super::StoreGraph,
        journal: &Arc<crate::graph::system_journal::SystemJournal>,
        writable: bool,
    ) -> Result<(), PersistError> {
        if let Some(source) = &self.source {
            let _ = source.journal.set(Arc::downgrade(journal));
        }
        if let Some(registry) = self.registry {
            if writable {
                graph.write().register_guard_hook(registry.hook())?;
            }
            graph.attach_model_registry(registry);
        }
        for (kind, event) in self.events {
            journal.emit(kind, event);
        }
        Ok(())
    }
}

/// How [`check_at_open`] runs.
pub(crate) struct OpenCheck<'a> {
    /// No needed-model check (a closed store's model change).
    pub(crate) unchecked: bool,
    /// Whether a refusal is written into the closed store's journal (not for a read-only
    /// open: it writes nothing).
    pub(crate) journal_writable: bool,
    pub(crate) instance_name: Option<&'a str>,
}

/// Checks the models of a loaded state at an open or a view (format spec 4.5): every model the
/// current schema NEEDS is loaded now (a build that cannot run models checks its blob by
/// length and CRC-32C and opens read-only); a needed model that is not in the catalog, whose
/// blob is missing or damaged, or that does not load refuses the open
/// ([`StoreFailure::ModelUnavailable`], [`StoreFailure::ModelDoesNotFit`]) with the commands
/// that fix it, and writes `model.load_failed` into the closed store's journal (best effort).
/// A model nothing needs that is missing or damaged is a journal record; the open goes on.
pub(crate) fn check_at_open(
    dir: &DirPath,
    definitions: Vec<Definition>,
    schema: Option<&GraphSchema>,
    check: &OpenCheck<'_>,
) -> Result<ModelsAtOpen, PersistError> {
    let needed = if check.unchecked {
        Vec::new()
    } else {
        needed_models(schema)
    };
    let mut out = ModelsAtOpen {
        registry: None,
        source: None,
        read_only: None,
        events: Vec::new(),
    };
    let source = Arc::new(StoreModelBytes::new(dir.clone()));
    let registry = registry::ModelRegistry::new(source.clone(), definitions.clone());
    let refuse = |failure: &registry::NeededFailure| -> PersistError {
        refusal(dir, &definitions, failure, check)
    };
    if !needed.is_empty() {
        let mut reasons = Vec::new();
        if registry::CAN_LOAD {
            let failures = registry.load_needed(&needed);
            // The loaded ones are journalled once the store's journal exists.
            for (identity, _) in &needed {
                let base = base_identity(identity);
                if failures.iter().any(|f| f.identity == base) {
                    continue;
                }
                if let Some(model) = definitions
                    .iter()
                    .find(|d| d.name() == base)
                    .and_then(Definition::as_model)
                {
                    out.events.push((
                        &catalogue::MODEL_LOADED,
                        Event::new(format!("the model {} was loaded", model.repo()))
                            .field("repo", model.repo())
                            .field("identity", base)
                            .count("load_bytes", model.load_bytes())
                            .count("ms", 0),
                    ));
                }
            }
            for failure in failures {
                match &failure.failure {
                    // A newer family or algorithm: read-only, never damage (16 rule 9).
                    registry::LoadFailure::Unsupported(reason) => reasons.push(format!(
                        "the model {} the schema needs cannot be run by this version: {reason}",
                        failure.identity
                    )),
                    _ => return Err(refuse(&failure)),
                }
            }
        } else {
            // This build cannot load models: their blobs are checked, the store opens
            // read-only without text queries (Q5.3).
            for (identity, places) in &needed {
                let base = base_identity(identity);
                let model = definitions
                    .iter()
                    .find(|d| d.name() == base)
                    .and_then(Definition::as_model);
                let Some(model) = model else {
                    return Err(refuse(&registry::NeededFailure {
                        identity: base.to_string(),
                        repo: parse_identity(base)
                            .and_then(|p| p.repo)
                            .map(str::to_string),
                        places: places.clone(),
                        failure: registry::LoadFailure::Missing,
                        needed: 0,
                    }));
                };
                if let Some(reason) = model.unserved_reason() {
                    reasons.push(format!(
                        "the model {base} the schema needs cannot be run by this version: \
                         {reason}"
                    ));
                    continue;
                }
                if let Err(err) = blobs::check_blob(dir, model.blob(), false) {
                    let failure = match err {
                        blobs::BlobError::Io(err) => {
                            return Err(PersistError::io("checking a model blob", err));
                        }
                        blobs::BlobError::Fault(blobs::BlobFault::Missing) => {
                            registry::LoadFailure::Missing
                        }
                        blobs::BlobError::Fault(fault) => registry::LoadFailure::Damaged(format!(
                            "its blob is damaged ({})",
                            fault.name()
                        )),
                        blobs::BlobError::DoesNotFit(err) => {
                            registry::LoadFailure::does_not_fit(err)
                        }
                        blobs::BlobError::Unsupported => registry::LoadFailure::Unsupported(
                            "its blob uses a hash algorithm this version does not know".into(),
                        ),
                    };
                    return Err(refuse(&registry::NeededFailure {
                        identity: base.to_string(),
                        repo: Some(model.repo().to_string()),
                        places: places.clone(),
                        failure,
                        needed: model.blob().length().saturating_add(model.load_bytes()),
                    }));
                }
                reasons.push(registry::unsupported_build(model.repo()));
            }
        }
        if !reasons.is_empty() {
            out.read_only = Some(reasons.join("; "));
        }
    }
    // The models nothing needs: a missing or damaged blob is a journal record only.
    for definition in &definitions {
        let Some(model) = definition.as_model() else {
            continue;
        };
        if needed
            .iter()
            .any(|(identity, _)| base_identity(identity) == definition.name())
        {
            continue;
        }
        let cause = match blob_state(dir, model.blob()) {
            ModelState::Missing => "missing",
            ModelState::Damaged { .. } => "damaged",
            _ => continue,
        };
        out.events.push((
            &catalogue::MODEL_UNAVAILABLE,
            Event::new(format!(
                "the model {} is {cause}: queries with it fail",
                model.repo()
            ))
            .field("repo", model.repo())
            .field("identity", definition.name())
            .field("cause", cause)
            .field("recommendation", registry::recommendation(model.repo())),
        ));
    }
    out.registry = Some(registry);
    out.source = Some(source);
    Ok(out)
}

/// The refusal of an open for a needed model, with its journal record (best effort).
fn refusal(
    dir: &DirPath,
    definitions: &[Definition],
    failure: &registry::NeededFailure,
    check: &OpenCheck<'_>,
) -> PersistError {
    let repo = failure
        .repo
        .clone()
        .unwrap_or_else(|| failure.identity.clone());
    let places = failure.places.join(", ");
    if check.journal_writable {
        super::system_journal::append_closed(
            dir,
            check.instance_name,
            &catalogue::MODEL_LOAD_FAILED,
            Event::new(format!(
                "the model {repo} the schema needs did not load: the store did not open"
            ))
            .field("repo", repo.as_str())
            .field("identity", failure.identity.as_str())
            .field("cause", failure.failure.cause())
            .count("needed", failure.needed)
            .field("recommendation", registry::recommendation(&repo)),
        );
    }
    if let registry::LoadFailure::DoesNotFit { limit, minimum, .. } = &failure.failure {
        return PersistError::store(
            "open the store",
            StoreFailure::ModelDoesNotFit,
            format!(
                "the model {repo} (needed by {places}) needs {} bytes to load (its blob and the \
                 loaded model), which does not fit under the process memory ceiling of {limit} \
                 bytes: set the ceiling to at least {minimum} bytes",
                failure.needed
            ),
        );
    }
    let what = match &failure.failure {
        registry::LoadFailure::Missing
            if definitions.iter().all(|d| d.name() != failure.identity) =>
        {
            "is not in the database at this position".to_string()
        }
        registry::LoadFailure::Missing => "is damaged (missing)".to_string(),
        registry::LoadFailure::Damaged(reason) | registry::LoadFailure::Invalid(reason) => {
            format!("is damaged ({reason})")
        }
        registry::LoadFailure::Unsupported(reason) => reason.clone(),
        registry::LoadFailure::DoesNotFit { message, .. } => message.clone(),
    };
    PersistError::store(
        "open the store",
        StoreFailure::ModelUnavailable,
        format!(
            "the model {repo} (needed by {places}) {what}: run `graphersal embed-model download \
             {repo}` then `graphersal embed-model import {repo} --into {} --replace`, or \
             restore the store from a backup or replica",
            dir.display()
        ),
    )
}

/// The model registry of a recovered past position (a view): its models, the needed ones
/// loaded; `None` when its catalog holds none. A needed model that is missing, damaged or does
/// not fit refuses the view like an open.
pub(crate) fn models_of_view<S: PersistentStorage>(
    dir: &DirPath,
    graph: &S,
) -> Result<Option<Arc<registry::ModelRegistry>>, PersistError> {
    let definitions: Vec<Definition> = graph
        .definitions()
        .map_err(PersistError::Graph)?
        .filter(|d| d.kind() == DefinitionKind::MODEL)
        .cloned()
        .collect();
    let schema = graph.get_schema().map_err(PersistError::Graph)?;
    let check = OpenCheck {
        unchecked: false,
        journal_writable: false,
        instance_name: None,
    };
    if definitions.is_empty() && needed_models(schema.as_ref()).is_empty() {
        return Ok(None);
    }
    let at_open = check_at_open(dir, definitions, schema.as_ref(), &check)?;
    Ok(at_open.registry)
}
