//! The embedding models a Store keeps (format spec 4.5, book page "Embedding Models in a
//! Store"): `models/<id>/` beside `GRAPH`, `snapshots/` and `wal/`, one directory per model,
//! named by the model's content address (`model2vec-<12 hex>`, the hex digits of its identity
//! `model2vec/<repo>@<12 hex>`), holding the model files, its card and licence files, and the
//! manifest `graphersal-embed-model.json` (written last; it lists every other file with its
//! size and SHA-256).
//!
//! Unlike the `WORKLOAD` sidecar, the models are part of what a store hands on: every copy
//! carries them (backups, the ZIP backup, fork, attic fork, repair, convert, restore, upgrade,
//! a packed snapshot), `verify` checks every file's SHA-256, prune and compact never touch
//! them. A model enters a store only by an explicit add and leaves it only by an explicit
//! remove. Files from a foreign store are hostile input: names, counts and sizes are bounded,
//! the manifest is parsed by the dependency-free JSON reader, and nothing here panics.
//!
//! An add or a copy writes into `models/tmp-<32 hex>/` and renames the finished directory into
//! place; a removal renames it to a `tmp-` name first. An open of a writable store removes
//! leftover `tmp-` directories, so a crash never leaves a half model under its id.

use std::io::{Read, Write};

use super::super::error::PersistError;
use super::files::{self, DirPath, EntryKind, TMP_PREFIX, read_bounded};
use crate::persist::StoreFailure;
use crate::value::physical;
use crate::value::vector::embed::identity::{
    FAMILY, IDENTITY_ROLES, identity_digest, identity_text, is_repo_name, parse_identity,
};
use crate::value::vector::embed::sha256::{Sha256, hex};
use crate::value::{ElementProperty, PropertyMap};

/// The directory of the models at the store root.
pub(crate) const MODELS_DIR: &str = "models";
/// A model's manifest file (the name `graphersal embed-model download` writes too).
pub(crate) const MODEL_MANIFEST: &str = "graphersal-embed-model.json";
/// The manifest format this build writes (1: the download manifest of Graphersal 0.3.0).
const MANIFEST_FORMAT: i64 = 2;
/// The largest manifest read.
const MAX_MANIFEST_BYTES: u64 = 1 << 20;
/// The largest model file accepted (as `graphersal embed-model download`).
pub(crate) const MAX_MODEL_FILE_BYTES: u64 = 8 << 30;
/// The most files one model holds (besides its manifest).
const MAX_MODEL_FILES: usize = 64;
/// The longest file name inside a model directory: `<id>.<file>` stays within the 64 bytes of a
/// plain name of a backup archive (format spec 12.8) and a packed snapshot's file name.
const MAX_FILE_NAME_BYTES: usize = 40;
/// The most base models kept.
const MAX_BASE_MODELS: usize = 16;
/// The streaming buffer.
const BUFFER: usize = 1 << 20;

/// What a model file is for ([`NewModel::with_file`]; its name in the model's tar manifest).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ModelRole {
    /// The model's configuration (`config.json`, or `config_sentence_transformers.json`):
    /// covered by the model identity.
    Config,
    /// The tokenizer (`tokenizer.json`): covered by the model identity.
    Tokenizer,
    /// The weights (`model.safetensors`): covered by the model identity.
    Weights,
    /// The model card (`README.md`): states the licence and the attribution.
    Card,
    /// A licence file of the repository (`LICENSE`, `LICENSE.txt`, ...), or the standard text
    /// of the stated licence the store wrote (`ModelInfo::license_text` `template`).
    License,
    /// A notice file of the repository (`NOTICE`, ...).
    Notice,
}

impl ModelRole {
    /// The role's name in the manifest: `config`, `tokenizer`, `model`, `card`, `license`,
    /// `notice`.
    pub fn as_str(self) -> &'static str {
        match self {
            ModelRole::Config => "config",
            ModelRole::Tokenizer => "tokenizer",
            ModelRole::Weights => "model",
            ModelRole::Card => "card",
            ModelRole::License => "license",
            ModelRole::Notice => "notice",
        }
    }
}

/// A model a store keeps ([`Store::models`](super::Store::models)): its identity, origin,
/// licence and files, from its manifest (format spec 4.5).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub(crate) struct StoredModel {
    /// The directory name under `models/`: `model2vec-<12 hex>`, the content address.
    pub id: String,
    /// The model identity a schema's `embedder` records: `model2vec/<repo>@<12 hex>` (or
    /// `model2vec@<12 hex>` without a repository).
    pub identity: String,
    /// The model family (`model2vec`).
    pub family: String,
    /// The Hugging Face repository the files come from.
    pub repo: Option<String>,
    /// The revision asked for (a branch, tag or commit).
    pub revision: Option<String>,
    /// The commit the revision resolved to.
    pub commit: Option<String>,
    /// The licence: an SPDX id as the repository states it (`mit`, `apache-2.0`), or
    /// `unknown`.
    pub license: String,
    /// Where the licence text comes from: `file` (a licence file of the repository),
    /// `template` (the standard text of [`Self::license`], written by the store because the
    /// repository has none) or `none`.
    pub license_text: String,
    /// The models this one was made from (`base_model` of its card), with their licences when
    /// known.
    pub base_models: Vec<BaseModel>,
    /// Whether the repository is gated (its terms had to be accepted to download it, and may
    /// restrict passing it on).
    pub gated: bool,
    /// When the model was added (RFC 3339 text), when the manifest says.
    pub added_at: Option<String>,
    /// Every file with its size and SHA-256 (the manifest not included).
    pub files: Vec<ModelFile>,
    /// The bytes of the files.
    pub bytes: u64,
    /// Why this entry cannot be used, when it cannot (an unreadable manifest, a manifest of a
    /// newer Graphersal); `None` for a usable model. `verify` checks the files.
    pub problem: Option<String>,
}

pub(crate) use crate::graph::model_definition::BaseModel;

/// One file of a [`StoredModel`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub(crate) struct ModelFile {
    /// The file name in the model's directory.
    pub name: String,
    /// What it is for ([`ModelRole::as_str`]; a role a newer Graphersal wrote is kept as text).
    pub role: String,
    /// Its size in bytes.
    pub bytes: u64,
    /// Its SHA-256, lowercase hex.
    pub sha256: String,
}

/// What [`Store::add_model`](super::Store::add_model) did.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
#[allow(dead_code)]
pub(crate) struct AddModelReport {
    /// The model as the store keeps it.
    pub model: StoredModel,
    /// The store held the same model already (the same files: the same id); nothing was
    /// written.
    pub already_stored: bool,
}

/// The 0.4 name of [`BlobCopy`](crate::blob::BlobCopy): whether a shareable copy carries the
/// store's models (and, since 0.5, its blobs).
pub type ModelCopy = super::blobs::BlobCopy;

/// A model to import into a store (`ModelSource::Files`,
/// [`Store::import_model`](super::Store::import_model)): its files (read as streams) and what is
/// known about its origin and licence.
pub struct NewModel {
    files: Vec<(String, ModelRole, Box<dyn Read + Send>)>,
    repo: Option<String>,
    revision: Option<String>,
    commit: Option<String>,
    license: Option<String>,
    base_models: Vec<BaseModel>,
    gated: bool,
}

impl std::fmt::Debug for NewModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewModel")
            .field(
                "files",
                &self
                    .files
                    .iter()
                    .map(|(name, role, _)| (name.as_str(), *role))
                    .collect::<Vec<_>>(),
            )
            .field("repo", &self.repo)
            .field("license", &self.license)
            .finish_non_exhaustive()
    }
}

impl Default for NewModel {
    fn default() -> Self {
        Self::new()
    }
}

/// The parts of a [`NewModel`] (the import reads them).
pub(crate) struct NewModelParts {
    pub(crate) files: Vec<(String, ModelRole, Box<dyn Read + Send>)>,
    pub(crate) repo: Option<String>,
    pub(crate) revision: Option<String>,
    pub(crate) commit: Option<String>,
    pub(crate) license: Option<String>,
    pub(crate) base_models: Vec<BaseModel>,
    pub(crate) gated: bool,
}

impl NewModel {
    /// The parts.
    pub(crate) fn into_parts(self) -> NewModelParts {
        NewModelParts {
            files: self.files,
            repo: self.repo,
            revision: self.revision,
            commit: self.commit,
            license: self.license,
            base_models: self.base_models,
            gated: self.gated,
        }
    }

    /// A model without files yet (a Model2Vec model needs one file of each of the roles
    /// [`ModelRole::Config`], [`ModelRole::Tokenizer`] and [`ModelRole::Weights`]).
    pub fn new() -> Self {
        NewModel {
            files: Vec::new(),
            repo: None,
            revision: None,
            commit: None,
            license: None,
            base_models: Vec::new(),
            gated: false,
        }
    }

    /// Adds the file `name` (1 to 40 bytes of ASCII letters, digits, `.`, `_` and `-`, not
    /// starting with `.`), read from `content`, in the role `role`.
    pub fn with_file(
        mut self,
        name: impl Into<String>,
        role: ModelRole,
        content: impl Read + Send + 'static,
    ) -> Self {
        self.files.push((name.into(), role, Box::new(content)));
        self
    }

    /// The Hugging Face repository (`owner/name`): part of the model identity.
    pub fn with_repo(mut self, repo: impl Into<String>) -> Self {
        self.repo = Some(repo.into());
        self
    }

    /// The revision the files were downloaded at.
    pub fn with_revision(mut self, revision: impl Into<String>) -> Self {
        self.revision = Some(revision.into());
        self
    }

    /// The commit the revision resolved to.
    pub fn with_commit(mut self, commit: impl Into<String>) -> Self {
        self.commit = Some(commit.into());
        self
    }

    /// The licence as an SPDX id (`mit`, `apache-2.0`; default `unknown`). Without a
    /// [`ModelRole::License`] file, the store writes the standard text of an MIT or Apache-2.0
    /// licence as `LICENSE` (`ModelInfo::license_text` `template`).
    pub fn with_license(mut self, license: impl Into<String>) -> Self {
        self.license = Some(license.into());
        self
    }

    /// A model this one was made from.
    pub fn with_base_model(mut self, base: BaseModel) -> Self {
        self.base_models.push(base);
        self
    }

    /// Whether the repository is gated.
    pub fn with_gated(mut self, gated: bool) -> Self {
        self.gated = gated;
        self
    }
}

fn io(context: &'static str) -> impl Fn(std::io::Error) -> PersistError {
    move |err| PersistError::io(context, err)
}

/// Whether `name` is a model's directory name: `model2vec-<12 lowercase hex>`.
pub(crate) fn is_model_id(name: &str) -> bool {
    name.strip_prefix(FAMILY)
        .and_then(|rest| rest.strip_prefix('-'))
        .is_some_and(|hex| {
            hex.len() == 12
                && hex
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
}

/// Whether `name` may name a file of a model directory.
pub(crate) fn is_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_FILE_NAME_BYTES
        && !name.starts_with('.')
        && !name.starts_with(TMP_PREFIX)
        && name != MODEL_MANIFEST
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// The store directory name of the model with the identity (or id) `key`.
pub(crate) fn id_of(key: &str) -> Option<String> {
    if is_model_id(key) {
        return Some(key.to_string());
    }
    parse_identity(key).map(|parsed| format!("{FAMILY}-{}", parsed.short))
}

/// The SHA-256 and length of a stream, at most `max` bytes (a longer stream gives
/// `max + 1`).
fn hash_stream(reader: &mut dyn Read, max: u64) -> std::io::Result<(u64, String)> {
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; BUFFER];
    let mut len = 0u64;
    let limit = max.saturating_add(1);
    while len < limit {
        let want = buf
            .len()
            .min(usize::try_from(limit - len).unwrap_or(usize::MAX));
        let n = match reader.read(&mut buf[..want]) {
            Ok(0) => break,
            Ok(n) => n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        hash.update(&buf[..n]);
        len += n as u64;
    }
    Ok((len, hex(&hash.finish())))
}

/// Copies `reader` into `out` (at most `max` bytes, a longer stream gives `max + 1`),
/// returning the length and SHA-256.
fn copy_hashed(
    reader: &mut dyn Read,
    out: &mut dyn Write,
    max: u64,
) -> std::io::Result<(u64, String)> {
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; BUFFER];
    let mut len = 0u64;
    let limit = max.saturating_add(1);
    while len < limit {
        let want = buf
            .len()
            .min(usize::try_from(limit - len).unwrap_or(usize::MAX));
        let n = match reader.read(&mut buf[..want]) {
            Ok(0) => break,
            Ok(n) => n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        hash.update(&buf[..n]);
        out.write_all(&buf[..n])?;
        len += n as u64;
    }
    Ok((len, hex(&hash.finish())))
}

// ---------------------------------------------------------------------------------------------
// The manifest
// ---------------------------------------------------------------------------------------------

fn text_of(object: &PropertyMap, key: &str) -> Option<String> {
    match object.get(key) {
        Some(ElementProperty::String(text)) => Some(text.to_string()),
        _ => None,
    }
}

/// Parses a model manifest (format spec 4.5) of the directory `id`. A manifest of a newer
/// format or an unknown family is returned with [`StoredModel::problem`] set (it is newer
/// content, never damage); one that does not parse, or whose file list is malformed, is an
/// error.
pub(crate) fn parse_manifest(id: &str, bytes: &[u8]) -> Result<StoredModel, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "the manifest is not UTF-8".to_string())?;
    let value = physical::lift_untyped(
        physical::read_json(text, 8).map_err(|err| format!("the manifest is not JSON: {err}"))?,
    );
    let ElementProperty::Object(object) = value else {
        return Err("the manifest is not a JSON object".into());
    };
    let format = match object.get("format") {
        Some(ElementProperty::Number(n)) => n.as_i64().unwrap_or(0),
        _ => 0,
    };
    let mut model = StoredModel {
        id: id.to_string(),
        identity: text_of(&object, "identity").unwrap_or_default(),
        family: text_of(&object, "family").unwrap_or_else(|| FAMILY.to_string()),
        repo: text_of(&object, "repo"),
        revision: text_of(&object, "revision"),
        commit: text_of(&object, "commit"),
        license: text_of(&object, "license").unwrap_or_else(|| "unknown".to_string()),
        license_text: text_of(&object, "license_text").unwrap_or_else(|| "none".to_string()),
        gated: matches!(object.get("gated"), Some(ElementProperty::Boolean(true))),
        added_at: text_of(&object, "added_at").or_else(|| text_of(&object, "downloaded_at")),
        ..StoredModel::default()
    };
    if let Some(ElementProperty::Array(bases)) = object.get("base_models") {
        for base in bases.iter().take(MAX_BASE_MODELS) {
            if let ElementProperty::Object(base) = base
                && let Some(repo) = text_of(base, "repo")
            {
                model.base_models.push(BaseModel {
                    repo,
                    license: text_of(base, "license"),
                });
            }
        }
    }
    let Some(ElementProperty::Array(files)) = object.get("files") else {
        return Err("the manifest has no \"files\" list".into());
    };
    if files.len() > MAX_MODEL_FILES {
        return Err(format!(
            "the manifest lists {} files (at most {MAX_MODEL_FILES})",
            files.len()
        ));
    }
    for file in files {
        let ElementProperty::Object(file) = file else {
            return Err("a file entry is not an object".into());
        };
        let name = text_of(file, "name").unwrap_or_default();
        if !is_file_name(&name) {
            return Err(format!("the file name {name:?} is not allowed"));
        }
        if model.files.iter().any(|f| f.name == name) {
            return Err(format!("the file {name:?} is listed twice"));
        }
        let bytes = match file.get("size") {
            Some(ElementProperty::Number(n)) => n
                .as_i64()
                .and_then(|n| u64::try_from(n).ok())
                .filter(|n| *n <= MAX_MODEL_FILE_BYTES)
                .ok_or_else(|| format!("the size of {name:?} is not a valid size"))?,
            _ => return Err(format!("the file {name:?} has no size")),
        };
        let sha256 = text_of(file, "sha256")
            .filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| format!("the file {name:?} has no SHA-256"))?
            .to_ascii_lowercase();
        // The download manifest of 0.3.0 (format 1) names the files without roles: the three
        // model files by their names.
        let role = text_of(file, "role").unwrap_or_else(|| {
            match name.as_str() {
                "config.json" | "config_sentence_transformers.json" => "config",
                "tokenizer.json" => "tokenizer",
                "model.safetensors" => "model",
                _ => "other",
            }
            .to_string()
        });
        model.bytes = model.bytes.saturating_add(bytes);
        model.files.push(ModelFile {
            name,
            role,
            bytes,
            sha256,
        });
    }
    if format > MANIFEST_FORMAT {
        model.problem = Some(format!(
            "its manifest has the format {format}, written by a newer Graphersal: kept and \
             copied, not used"
        ));
        return Ok(model);
    }
    if model.family != FAMILY {
        model.problem = Some(format!(
            "a model of the family {:?}, which this Graphersal does not know: kept and copied, \
             not used",
            model.family
        ));
        return Ok(model);
    }
    // The identity follows from the files the manifest lists (format spec 4.5).
    let roles: Vec<&ModelFile> = IDENTITY_ROLES
        .iter()
        .filter_map(|role| {
            let mut matching = model.files.iter().filter(|f| f.role == *role);
            match (matching.next(), matching.next()) {
                (Some(file), None) => Some(file),
                _ => None,
            }
        })
        .collect();
    let [config, tokenizer, weights] = roles.as_slice() else {
        return Err(
            "the manifest does not list exactly one config, tokenizer and model file".into(),
        );
    };
    let digest = identity_digest([
        (config.bytes, &config.sha256),
        (tokenizer.bytes, &tokenizer.sha256),
        (weights.bytes, &weights.sha256),
    ]);
    let expected_id = format!("{FAMILY}-{}", &digest[..12]);
    if id != expected_id {
        return Err(format!(
            "the files the manifest lists are the model {expected_id}, not {id}"
        ));
    }
    if let Some(repo) = &model.repo
        && !is_repo_name(repo)
    {
        return Err(format!("the repo {repo:?} is not a repository name"));
    }
    let identity = identity_text(model.repo.as_deref(), &digest);
    if !model.identity.is_empty() && model.identity != identity {
        return Err(format!(
            "the manifest names the identity {:?}, its files are {identity:?}",
            model.identity
        ));
    }
    model.identity = identity;
    Ok(model)
}

// ---------------------------------------------------------------------------------------------
// Listing and reading
// ---------------------------------------------------------------------------------------------

/// The `models/` directory of the store at `root`.
pub(crate) fn models_dir(root: &DirPath) -> DirPath {
    root.join(MODELS_DIR)
}

/// The model directories under `models/` (valid ids, sorted); `tmp-` entries and other names
/// are not models (verify reports the other names).
pub(crate) fn model_ids(root: &DirPath) -> Result<Vec<String>, PersistError> {
    let dir = models_dir(root);
    // No models directory is no model; one that cannot be read is the error.
    if dir.try_kind().map_err(io("reading the store's models"))? != Some(EntryKind::Dir) {
        return Ok(Vec::new());
    }
    let mut ids: Vec<String> = dir
        .list()
        .map_err(io("listing the store's models"))?
        .into_iter()
        .filter(|(name, kind)| *kind == EntryKind::Dir && is_model_id(name))
        .map(|(name, _)| name)
        .collect();
    ids.sort();
    Ok(ids)
}

/// The manifest bytes and the parsed manifest of the model `id`.
pub(crate) fn read_model_manifest(
    root: &DirPath,
    id: &str,
) -> Result<(Vec<u8>, StoredModel), String> {
    let path = models_dir(root).join(id).join(MODEL_MANIFEST);
    let bytes = read_bounded(&path, MAX_MANIFEST_BYTES).map_err(|err| err.to_string())?;
    let model = parse_manifest(id, &bytes)?;
    Ok((bytes, model))
}

/// The models of the store at `root` (their manifests; a model whose manifest cannot be read
/// is listed with its [`StoredModel::problem`]).
pub(crate) fn list(root: &DirPath) -> Result<Vec<StoredModel>, PersistError> {
    Ok(model_ids(root)?
        .into_iter()
        .map(|id| match read_model_manifest(root, &id) {
            Ok((_, model)) => model,
            Err(reason) => StoredModel {
                problem: Some(format!("its manifest is not readable: {reason}")),
                family: FAMILY.to_string(),
                license: "unknown".to_string(),
                license_text: "none".to_string(),
                id,
                ..StoredModel::default()
            },
        })
        .collect())
}

/// The model of the store at `root` that `key` names (its identity, with or without a
/// normalization suffix, or its id).
pub(crate) fn find(root: &DirPath, key: &str) -> Result<Option<StoredModel>, PersistError> {
    let Some(id) = id_of(key) else {
        return Ok(None);
    };
    if !models_dir(root).join(&id).is_dir() {
        return Ok(None);
    }
    Ok(list(root)?.into_iter().find(|m| m.id == id))
}

// ---------------------------------------------------------------------------------------------
// Checking (verify, copies)
// ---------------------------------------------------------------------------------------------

/// Every problem of the model `id` (paths relative to the store): an unreadable or invalid
/// manifest, a missing, extra, resized or changed file. Reads every byte. Returns the model
/// and the problems; the model is `None` when its manifest is not readable.
pub(crate) fn check(root: &DirPath, id: &str) -> (Option<StoredModel>, Vec<(String, String)>) {
    let rel = format!("{MODELS_DIR}/{id}");
    let model = match read_model_manifest(root, id) {
        Ok((_, model)) => model,
        Err(reason) => return (None, vec![(format!("{rel}/{MODEL_MANIFEST}"), reason)]),
    };
    let mut problems = Vec::new();
    let dir = models_dir(root).join(id);
    match dir.list() {
        Ok(entries) => {
            for (name, _) in entries {
                if name != MODEL_MANIFEST && !model.files.iter().any(|f| f.name == name) {
                    problems.push((
                        format!("{rel}/{name}"),
                        "a file the model's manifest does not list".to_string(),
                    ));
                }
            }
        }
        Err(err) => problems.push((rel.clone(), err.to_string())),
    }
    for file in &model.files {
        let path = dir.join(&file.name);
        let result = path
            .open_read()
            .and_then(|mut reader| hash_stream(&mut reader, file.bytes));
        match result {
            Ok((len, sha256)) if len == file.bytes && sha256 == file.sha256 => {}
            Ok((len, _)) if len != file.bytes => problems.push((
                format!("{rel}/{}", file.name),
                format!(
                    "its size differs from the model's manifest ({} bytes)",
                    file.bytes
                ),
            )),
            Ok(_) => problems.push((
                format!("{rel}/{}", file.name),
                "its SHA-256 differs from the model's manifest".to_string(),
            )),
            Err(err) => problems.push((format!("{rel}/{}", file.name), err.to_string())),
        }
    }
    (Some(model), problems)
}

/// Problems of the `models/` directory itself: entries that are no model directory (a file, a
/// name that is no model id; `tmp-` entries are left to the next open).
pub(crate) fn stray_entries(root: &DirPath) -> Vec<(String, String)> {
    let dir = models_dir(root);
    let Ok(entries) = dir.list() else {
        return Vec::new();
    };
    entries
        .into_iter()
        .filter(|(name, kind)| {
            !name.starts_with(TMP_PREFIX) && !(*kind == EntryKind::Dir && is_model_id(name))
        })
        .map(|(name, _)| {
            (
                format!("{MODELS_DIR}/{name}"),
                "not a model directory (models/ holds only model2vec-<12 hex> directories)"
                    .to_string(),
            )
        })
        .collect()
}

/// Checks a model file read as a stream (an entry of a backup archive) against `model`'s
/// manifest: problems as text.
#[cfg_attr(not(feature = "persist-zip"), allow(dead_code))]
pub(crate) fn check_stream(model: &StoredModel, name: &str, reader: &mut dyn Read) -> Vec<String> {
    let Some(file) = model.files.iter().find(|f| f.name == name) else {
        return vec!["a file the model's manifest does not list".into()];
    };
    match hash_stream(reader, file.bytes) {
        Ok((len, sha256)) if len == file.bytes && sha256 == file.sha256 => Vec::new(),
        Ok(_) => vec!["its size or SHA-256 differs from the model's manifest".into()],
        Err(err) => vec![err.to_string()],
    }
}

// ---------------------------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------------------------

/// A model directory being written: `models/tmp-<32 hex>/`, removed on drop unless it was
/// renamed into place.
struct NewModelDir {
    tmp: DirPath,
    done: bool,
}

impl NewModelDir {
    fn begin(root: &DirPath) -> Result<Self, PersistError> {
        let tmp =
            models_dir(root).join(format!("{TMP_PREFIX}{:032x}", crate::value::uuid::new_v7()));
        tmp.create_dir_all()
            .map_err(io("creating a model directory"))?;
        Ok(NewModelDir { tmp, done: false })
    }

    /// Syncs the directory and renames it to `models/<id>`; `false` (and the directory
    /// removed) when that model exists already.
    fn finish(mut self, root: &DirPath, id: &str) -> Result<bool, PersistError> {
        files::sync_dir(&self.tmp)?;
        let target = models_dir(root).join(id);
        if target.exists() {
            return Ok(false);
        }
        self.tmp
            .rename_to(&target)
            .map_err(io("moving a model into place"))?;
        self.done = true;
        files::sync_dir(&models_dir(root))?;
        Ok(true)
    }
}

impl Drop for NewModelDir {
    fn drop(&mut self) {
        if !self.done {
            let _ = files::remove_path(&self.tmp);
        }
    }
}

/// Removes the `models/tmp-*` leftovers of an interrupted add, copy or removal (the open of a
/// writable store). Returns how many.
pub(crate) fn remove_tmp(root: &DirPath) -> usize {
    let dir = models_dir(root);
    let Ok(entries) = dir.list() else {
        return 0;
    };
    let mut removed = 0;
    for (name, _) in entries {
        if name.starts_with(TMP_PREFIX) && files::remove_path(&dir.join(&name)).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Removes the model `key` names (identity or id) from the store at `root`: its directory is
/// renamed to a `tmp-` name, then removed. Returns the model removed.
pub(crate) fn remove(root: &DirPath, key: &str) -> Result<StoredModel, PersistError> {
    let Some(model) = find(root, key)? else {
        return Err(unknown_model("remove a model", key));
    };
    let dir = models_dir(root);
    let doomed = dir.join(format!("{TMP_PREFIX}{:032x}", crate::value::uuid::new_v7()));
    dir.join(&model.id)
        .rename_to(&doomed)
        .map_err(io("removing a model"))?;
    files::sync_dir(&dir)?;
    files::remove_path(&doomed).map_err(io("removing a model"))?;
    files::sync_dir(&dir)?;
    Ok(model)
}

/// The refusal of an operation on a model the store does not hold.
pub(crate) fn unknown_model(operation: &'static str, key: &str) -> PersistError {
    PersistError::store(
        operation,
        StoreFailure::UnknownModel,
        format!("the store holds no model {key:?}"),
    )
}

/// Copies the model `id` from the store at `from` into the store at `to` (a new or existing
/// store): every file streamed with its SHA-256 checked against the manifest, the manifest
/// byte for byte, into a temporary directory renamed into place. A model `to` holds already is
/// left as it is. A file that does not match its manifest stops the copy (damage of the
/// source).
pub(crate) fn copy_model(
    from: &DirPath,
    to: &DirPath,
    id: &str,
) -> Result<StoredModel, PersistError> {
    let (manifest, model) = read_model_manifest(from, id).map_err(|reason| {
        PersistError::corrupt(
            format!("{}/{MODELS_DIR}/{id}/{MODEL_MANIFEST}", from.display()),
            0,
            format!(
                "{reason} (remove the model with `graphersal store models remove`, or make the \
                 copy without models)"
            ),
        )
    })?;
    if models_dir(to).join(id).is_dir() {
        return Ok(model);
    }
    models_dir(to)
        .create_dir_all()
        .map_err(io("creating the models directory"))?;
    let dir = NewModelDir::begin(to)?;
    let source = models_dir(from).join(id);
    for file in &model.files {
        let path = source.join(&file.name);
        let mut reader = path.open_read().map_err(io("reading a model file"))?;
        let mut out = dir
            .tmp
            .join(&file.name)
            .create(true)
            .map_err(io("copying a model file"))?;
        let (len, sha256) =
            copy_hashed(&mut reader, &mut out, file.bytes).map_err(io("copying a model file"))?;
        if len != file.bytes || sha256 != file.sha256 {
            return Err(PersistError::corrupt(
                path.display(),
                0,
                "the model file's size or SHA-256 differs from its manifest (remove the model \
                 with `graphersal store models remove` and add it again, or make the copy \
                 without models)",
            ));
        }
        out.sync().map_err(io("copying a model file"))?;
    }
    dir.tmp
        .join(MODEL_MANIFEST)
        .create(true)
        .and_then(|mut out| {
            out.write_all(&manifest)?;
            out.sync()
        })
        .map_err(io("copying a model manifest"))?;
    dir.finish(to, id)?;
    Ok(model)
}

/// Copies every model of the store at `from` into the store at `to` ([`copy_model`]).
pub(crate) fn copy_all(from: &DirPath, to: &DirPath) -> Result<Vec<StoredModel>, PersistError> {
    model_ids(from)?
        .iter()
        .map(|id| copy_model(from, to, id))
        .collect()
}

/// [`copy_all`] into a store just created at `to` (a fork, attic fork, repair, upgrade): on a
/// failure the new store is removed (`existed`: whether its directory existed before).
pub(crate) fn carry_into_new(
    from: &DirPath,
    to: &DirPath,
    existed: bool,
) -> Result<Vec<StoredModel>, PersistError> {
    copy_all(from, to).inspect_err(|_| files::undo_new_dir(to, existed))
}

/// [`copy_all`] that skips a model that does not copy (a backup from memory, made because the
/// store's files are damaged): the models copied, and a warning per model skipped.
pub(crate) fn copy_all_lenient(from: &DirPath, to: &DirPath) -> (Vec<StoredModel>, Vec<String>) {
    let mut copied = Vec::new();
    let mut warnings = Vec::new();
    for id in model_ids(from).unwrap_or_default() {
        match copy_model(from, to, &id) {
            Ok(model) => copied.push(model),
            Err(err) => warnings.push(format!("the model {id} was not copied: {err}")),
        }
    }
    (copied, warnings)
}

/// Makes the models of `to` those of `from` (an incremental backup): copies the ones `to`
/// lacks, removes the ones `from` no longer holds. Returns the ids copied and removed.
pub(crate) fn sync_models(
    from: &DirPath,
    to: &DirPath,
) -> Result<(Vec<String>, Vec<String>), PersistError> {
    let wanted = model_ids(from)?;
    let present = model_ids(to)?;
    let mut copied = Vec::new();
    for id in &wanted {
        if !present.contains(id) {
            copy_model(from, to, id)?;
            copied.push(id.clone());
        }
    }
    let mut removed = Vec::new();
    for id in present {
        if !wanted.contains(&id) {
            remove(to, &id)?;
            removed.push(id);
        }
    }
    Ok((copied, removed))
}

/// The file name of a model file in a packed snapshot (format spec 8, segment-table kind
/// [`PACKED_MODEL_KIND`]): `<id>.<file>`, a plain name of at most 64 bytes.
pub(crate) fn packed_name(id: &str, file: &str) -> String {
    format!("{id}.{file}")
}

/// The model id and file name of a packed snapshot's model file name (`None`: not one).
pub(crate) fn parse_packed_name(name: &str) -> Option<(&str, &str)> {
    let (id, file) = name.split_once('.')?;
    (is_model_id(id) && (file == MODEL_MANIFEST || is_file_name(file))).then_some((id, file))
}

/// The segment-table kind of a model file in a packed snapshot (format spec 7.5, 8): an
/// IGNORABLE kind, so a reader without models checks the file's length and CRC and otherwise
/// ignores it.
pub(crate) const PACKED_MODEL_KIND: u8 = 3;

/// Installs a model whose files a packed snapshot carried (written into `staged`, a
/// `models/tmp-*` directory of the store at `root`): the manifest must parse, list exactly the
/// staged files, and every file must match it. Returns the model.
pub(crate) fn install_staged(
    root: &DirPath,
    id: &str,
    staged: &DirPath,
) -> Result<StoredModel, PersistError> {
    let location = format!("packed snapshot model {id}");
    let manifest = read_bounded(&staged.join(MODEL_MANIFEST), MAX_MANIFEST_BYTES)
        .map_err(|err| PersistError::corrupt(&location, 0, format!("no manifest: {err}")))?;
    let model = parse_manifest(id, &manifest)
        .map_err(|reason| PersistError::corrupt(&location, 0, reason))?;
    let entries = staged.list().map_err(io("reading a staged model"))?;
    for (name, _) in &entries {
        if name != MODEL_MANIFEST && !model.files.iter().any(|f| &f.name == name) {
            return Err(PersistError::corrupt(
                &location,
                0,
                format!("the file {name:?} is not in the model's manifest"),
            ));
        }
    }
    for file in &model.files {
        let path = staged.join(&file.name);
        let (len, sha256) = path
            .open_read()
            .and_then(|mut reader| hash_stream(&mut reader, file.bytes))
            .map_err(|err| PersistError::corrupt(&location, 0, err.to_string()))?;
        if len != file.bytes || sha256 != file.sha256 {
            return Err(PersistError::corrupt(
                &location,
                0,
                format!("the file {:?} differs from the model's manifest", file.name),
            ));
        }
    }
    files::sync_dir(staged)?;
    let target = models_dir(root).join(id);
    if !target.exists() {
        staged
            .rename_to(&target)
            .map_err(io("moving a model into place"))?;
        files::sync_dir(&models_dir(root))?;
    } else {
        files::remove_path(staged).map_err(io("removing a staged model"))?;
    }
    Ok(model)
}

/// Where the bytes of a model file a packed export carries come from.
pub(crate) enum PackedSource {
    /// In memory (the manifest).
    Bytes(Vec<u8>),
    /// A file of the store.
    File(DirPath),
}

/// One model file a packed export carries as a segment-table row of kind
/// [`PACKED_MODEL_KIND`] (format spec 4.5, 8).
pub(crate) struct PackedModelFile {
    /// The row's segment kind: [`PACKED_MODEL_KIND`], or
    /// [`PACKED_BLOB_KIND`](super::blobs::PACKED_BLOB_KIND) for a blob.
    pub(crate) kind: u8,
    /// The row's path: [`packed_name`] (a blob: its hash hex).
    pub(crate) name: String,
    /// The model id.
    pub(crate) id: String,
    /// The file's name in the model directory.
    pub(crate) file: String,
    pub(crate) source: PackedSource,
    pub(crate) len: u64,
    pub(crate) crc: u32,
}

/// Every file of every model of the store at `root` as a packed export carries it (each
/// model's files, then its manifest), each read once to check its SHA-256 against the manifest
/// and compute its CRC-32C (the manifest of the packed snapshot names both before the bytes).
/// A model that does not check stops the export.
pub(crate) fn packed_files(
    root: &DirPath,
) -> Result<(Vec<PackedModelFile>, Vec<StoredModel>), PersistError> {
    use super::super::codec::{crc, crc_append};
    let mut out = Vec::new();
    let mut models = Vec::new();
    for id in model_ids(root)? {
        let hint = "(remove the model with `graphersal store models remove`, or export without \
                    models)";
        let (manifest, model) = read_model_manifest(root, &id).map_err(|reason| {
            PersistError::corrupt(
                format!("{}/{MODELS_DIR}/{id}/{MODEL_MANIFEST}", root.display()),
                0,
                format!("{reason} {hint}"),
            )
        })?;
        for file in &model.files {
            let path = models_dir(root).join(&id).join(&file.name);
            let mut reader = path.open_read().map_err(io("reading a model file"))?;
            let mut hash = Sha256::new();
            let mut buf = vec![0u8; BUFFER];
            let (mut len, mut value) = (0u64, 0u32);
            loop {
                let n = match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(err) => return Err(PersistError::io("reading a model file", err)),
                };
                len += n as u64;
                if len > file.bytes {
                    break;
                }
                hash.update(&buf[..n]);
                value = crc_append(value, &buf[..n]);
            }
            if len != file.bytes || hex(&hash.finish()) != file.sha256 {
                return Err(PersistError::corrupt(
                    path.display(),
                    0,
                    format!("the file's size or SHA-256 differs from the model's manifest {hint}"),
                ));
            }
            out.push(PackedModelFile {
                kind: PACKED_MODEL_KIND,
                name: packed_name(&id, &file.name),
                id: id.clone(),
                file: file.name.clone(),
                source: PackedSource::File(path),
                len,
                crc: value,
            });
        }
        out.push(PackedModelFile {
            kind: PACKED_MODEL_KIND,
            name: packed_name(&id, MODEL_MANIFEST),
            id: id.clone(),
            file: MODEL_MANIFEST.to_string(),
            len: manifest.len() as u64,
            crc: crc(&manifest),
            source: PackedSource::Bytes(manifest),
        });
        models.push(model);
    }
    Ok((out, models))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_names() {
        assert!(is_model_id("model2vec-0123456789ab"));
        assert!(!is_model_id("model2vec-0123456789AB"));
        assert!(!is_model_id("model2vec-0123456789a"));
        assert!(!is_model_id("tmp-model2vec-0123456789ab"));
        assert!(is_file_name("model.safetensors"));
        assert!(is_file_name("LICENSE.txt"));
        assert!(!is_file_name(MODEL_MANIFEST));
        assert!(!is_file_name(".hidden"));
        assert!(!is_file_name("a/b"));
        assert!(!is_file_name(&"x".repeat(41)));
        assert_eq!(
            id_of("model2vec/o/n@0123456789ab?normalize=false").as_deref(),
            Some("model2vec-0123456789ab")
        );
        assert_eq!(id_of("hash-ngram/1"), None);
        let packed = packed_name("model2vec-0123456789ab", "model.safetensors");
        assert!(packed.len() <= 64);
        assert_eq!(
            parse_packed_name(&packed),
            Some(("model2vec-0123456789ab", "model.safetensors"))
        );
        assert_eq!(parse_packed_name("v-000000.seg"), None);
    }

    #[test]
    fn hostile_manifests_are_errors_never_panics() {
        let id = "model2vec-0123456789ab";
        for text in [
            "",
            "null",
            "[]",
            "{}",
            "{\"files\": 3}",
            "{\"files\": [1]}",
            "{\"files\": [{\"name\": \"../x\", \"size\": 1, \"sha256\": \"\"}]}",
            "{\"files\": [{\"name\": \"a\", \"size\": -1}]}",
            "{\"files\": [{\"name\": \"a\", \"size\": 1e30}]}",
            "{\"files\": [{\"name\": \"a\", \"size\": 99999999999999999}]}",
            "[[[[[[[[[[[[[[[[[[[[[[[[[[]]]]]]]]]]]]]]]]]]]]]]]]]",
            "\u{0}\u{1}",
        ] {
            assert!(parse_manifest(id, text.as_bytes()).is_err(), "{text}");
        }
        assert!(parse_manifest(id, &[0xff, 0xfe]).is_err());
    }

    #[test]
    fn a_newer_manifest_is_kept_not_damage() {
        let text = r#"{"format": 99, "files": []}"#;
        let model = parse_manifest("model2vec-0123456789ab", text.as_bytes()).unwrap();
        assert!(model.problem.unwrap().contains("newer"));
    }
}
