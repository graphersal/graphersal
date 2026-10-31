//! The migration of a Graphersal 0.4.0 store's embedding models (store format version 3 to 4,
//! format spec 15.1; book page "Store format versions"): every model kept as files under
//! `models/<id>/` becomes ONE blob (the deterministic model tar of `model_tar.rs`) referenced by
//! a catalog definition of kind 5, all of them added by ONE closed-store commit (principal
//! `upgrade`, attribute `upgrade` = `"<from>->4"`); then `models/` is removed.
//!
//! The steps (the writer lock held by the caller, through `StoreDir` only, so a `MemDir` store
//! of the playground migrates too):
//! 1. every model is checked against its manifest (every byte hashed) BEFORE anything is
//!    written; a damaged, incomplete or unconvertible one refuses the upgrade
//!    ([`StoreFailure::UpgradeModelDamaged`]);
//! 2. the `INTENT` `upgrade_models` (its `files` the model directories, `from` the old store
//!    format version) is written; a Graphersal 0.4.0 refuses the store from here on (an unknown
//!    operation, format spec 11.1), the caller then writes `GRAPH` with the new version;
//! 3. [`complete`]: per model the tar is written as a blob, one commit defines the models (and
//!    renames a renamed model's identity in the schema), the model directories are removed
//!    (each renamed to a `tmp-` name first), then `models/` and the `INTENT`.
//!
//! Every step is idempotent: a model whose content (the identity's 12 hex digits) is defined
//! already is not committed again, a blob of the same content is deduplicated, a removed
//! directory stays removed. An interrupted migration is completed by the next read-write open,
//! closed-store operation or `store upgrade` (`complete_intent`).
//!
//! The repository a definition needs: the manifest's; a model imported without one (identity
//! `model2vec@<12 hex>`), or a second revision of a repository another model of the store
//! takes already (the catalog keeps one per repository), gets `local/<12 hex>`: its identity
//! changes, and the schema's `embedder` naming the old identity is rewritten in the same
//! commit (the vectors stay as they are: the model's content is the same).

use super::files::{self, DirPath, EntryKind, GraphFile, Intent, TMP_PREFIX};
use super::model_catalog::{self, MAX_HEADER_BYTES, Prepared};
use super::models::{self, StoredModel};
use super::{ClosedCommitOptions, blobs, closed_commit};
use crate::graph::definition::Definition;
use crate::graph::schema::GraphSchema;
use crate::graph::system_journal::{Event, catalogue};
use crate::graph::{GraphStorage, Mutation, TraversalGraph};
use crate::persist::model_tar::{
    self, ManifestBase, ManifestFile, ModelManifest, TarContent, TarFile, TarStream,
};
use crate::persist::{PersistError, StoreFailure};
use crate::value::vector::embed::identity::{
    FAMILY, IDENTITY_ROLES, identity_digest, identity_text, parse_identity,
};

/// The `INTENT` operation of an interrupted model migration (format spec 11.1).
pub(crate) const UPGRADE_MODELS_OP: &str = "upgrade_models";
/// What the migration's errors name.
const OPERATION: &str = "upgrade the store";
/// The principal of the migration commit.
pub(crate) const UPGRADE_PRINCIPAL: &str = "upgrade";

/// A 0.4.0 model checked for the migration: its manifest and what its definition copies.
#[derive(Debug, Clone)]
pub(crate) struct LegacyModel {
    pub(crate) model: StoredModel,
    /// [`identity_digest`] of its config, tokenizer and weights.
    digest: String,
    dimension: u64,
    load_bytes: u64,
}

impl LegacyModel {
    /// The 12 hex digits of its identity.
    pub(crate) fn short(&self) -> &str {
        &self.digest[..self.digest.len().min(12)]
    }
}

/// A converted model (crate-internal form of the report's entry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Converted {
    pub(crate) repo: String,
    pub(crate) identity: String,
    pub(crate) former_identity: String,
    pub(crate) bytes: u64,
}

/// What [`complete`] did.
#[derive(Default)]
pub(crate) struct Migrated {
    /// Every model of `models/` with its definition (also one an interrupted run defined).
    pub(crate) converted: Vec<Converted>,
    /// The migration commit (`None`: nothing was committed by this run).
    pub(crate) commit_seq: Option<u64>,
    /// The catalog state after the migration (`None`: no `models/` to migrate).
    pub(crate) state: Option<TraversalGraph>,
}

fn damaged(dir: &DirPath, id: &str, reason: impl std::fmt::Display) -> PersistError {
    PersistError::store(
        OPERATION,
        StoreFailure::UpgradeModelDamaged,
        format!(
            "the embedding model {}/{}/{id} cannot be converted: {reason}",
            dir.display(),
            models::MODELS_DIR
        ),
    )
}

/// Step 1: every model of `models/` checked against its manifest (every byte hashed) and
/// prepared for the conversion; nothing is written. A damaged, incomplete or unconvertible
/// model refuses with [`StoreFailure::UpgradeModelDamaged`].
pub(crate) fn check_legacy(dir: &DirPath) -> Result<Vec<LegacyModel>, PersistError> {
    let mut out = Vec::new();
    for id in models::model_ids(dir)? {
        let (model, problems) = models::check(dir, &id);
        if let Some((path, reason)) = problems.into_iter().next() {
            return Err(damaged(dir, &id, format!("{path}: {reason}")));
        }
        let Some(model) = model else {
            return Err(damaged(dir, &id, "its manifest is not readable"));
        };
        if let Some(problem) = &model.problem {
            return Err(damaged(dir, &id, problem));
        }
        out.push(legacy_of(dir, model)?);
    }
    Ok(out)
}

/// The conversion data of one checked model.
pub(crate) fn legacy_of(dir: &DirPath, model: StoredModel) -> Result<LegacyModel, PersistError> {
    let id = model.id.clone();
    if model.files.len() > model_tar::MAX_MODEL_FILES {
        return Err(damaged(
            dir,
            &id,
            format!("more than {} files", model_tar::MAX_MODEL_FILES),
        ));
    }
    for file in &model.files {
        if !model_tar::is_model_file_name(&file.name)
            || file.bytes > model_tar::MAX_MODEL_FILE_BYTES
        {
            return Err(damaged(
                dir,
                &id,
                format!("the file {:?} cannot be put into a model tar", file.name),
            ));
        }
    }
    let by_role = |role: &str| {
        let mut matching = model.files.iter().filter(|f| f.role == role);
        match (matching.next(), matching.next()) {
            (Some(file), None) => Some(file),
            _ => None,
        }
    };
    let (Some(config), Some(tokenizer), Some(weights)) = (
        by_role(IDENTITY_ROLES[0]),
        by_role(IDENTITY_ROLES[1]),
        by_role(IDENTITY_ROLES[2]),
    ) else {
        return Err(damaged(
            dir,
            &id,
            "its manifest does not list exactly one config, tokenizer and model file",
        ));
    };
    let digest = identity_digest([
        (config.bytes, &config.sha256),
        (tokenizer.bytes, &tokenizer.sha256),
        (weights.bytes, &weights.sha256),
    ]);
    let head_len = usize::try_from(weights.bytes)
        .unwrap_or(usize::MAX)
        .min(MAX_HEADER_BYTES);
    let head = models::models_dir(dir)
        .join(&id)
        .join(&weights.name)
        .read_range(0, head_len)
        .map_err(|err| damaged(dir, &id, err))?;
    let Some((rows, cols)) = model_catalog::safetensors_shape(&head) else {
        return Err(damaged(
            dir,
            &id,
            format!(
                "the weights {:?} are not a safetensors file with an embedding tensor of two \
                 dimensions",
                weights.name
            ),
        ));
    };
    let load_bytes = rows
        .saturating_mul(cols)
        .saturating_mul(4)
        .saturating_add(tokenizer.bytes);
    Ok(LegacyModel {
        model,
        digest,
        dimension: cols,
        load_bytes,
    })
}

/// The repository each model's definition takes (format spec 15.1): its manifest's, unless
/// it has none or an earlier model (by directory name) took it; then `local/<12 hex>`.
fn repos_of(legacy: &[LegacyModel]) -> Vec<String> {
    let mut taken: Vec<String> = Vec::new();
    let mut out = Vec::with_capacity(legacy.len());
    for model in legacy {
        let repo = match &model.model.repo {
            Some(repo) if !taken.contains(repo) => repo.clone(),
            _ => format!("local/{}", model.short()),
        };
        taken.push(repo.clone());
        out.push(repo);
    }
    out
}

/// The model `legacy` as an import ready to be written as a blob, under `repo`.
pub(crate) fn prepared(dir: &DirPath, legacy: &LegacyModel, repo: &str) -> Prepared {
    let model = &legacy.model;
    let identity = identity_text(Some(repo), &legacy.digest);
    let base = models::models_dir(dir).join(&model.id);
    let manifest = ModelManifest {
        family: FAMILY.to_string(),
        identity: identity.clone(),
        repo: repo.to_string(),
        revision: model.revision.clone(),
        commit: model.commit.clone(),
        license: model.license.clone(),
        license_text: model.license_text.clone(),
        base_models: model
            .base_models
            .iter()
            .take(model_tar::MAX_BASE_MODELS)
            .map(|b| ManifestBase {
                repo: b.repo.clone(),
                license: b.license.clone(),
            })
            .collect(),
        gated: model.gated,
        files: {
            let mut listed: Vec<ManifestFile> = model
                .files
                .iter()
                .map(|f| ManifestFile {
                    name: f.name.clone(),
                    role: f.role.clone(),
                    size: f.bytes,
                    sha256: f.sha256.clone(),
                })
                .collect();
            listed.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
            listed
        },
    };
    let manifest_bytes = manifest.to_bytes();
    let mut files: Vec<TarFile> = model
        .files
        .iter()
        .map(|f| {
            let path = base.join(&f.name);
            TarFile {
                name: f.name.clone(),
                size: f.bytes,
                sha256: f.sha256.clone(),
                content: TarContent::Open(Box::new(move || path.open_read())),
            }
        })
        .collect();
    files.push(TarFile {
        name: model_tar::MODEL_MANIFEST.to_string(),
        size: manifest_bytes.len() as u64,
        sha256: model_tar::sha256_hex(&manifest_bytes),
        content: TarContent::Bytes(manifest_bytes),
    });
    Prepared {
        manifest,
        files,
        identity,
        dimension: legacy.dimension,
        load_bytes: legacy.load_bytes,
    }
}

/// The definition of `definitions` holding the model whose identity ends in `short`.
fn defined<'a>(definitions: &'a [Definition], short: &str) -> Option<&'a Definition> {
    definitions.iter().find(|d| {
        d.as_model().is_some()
            && parse_identity(d.name())
                .is_some_and(|parsed| parsed.short == short && parsed.normalize.is_none())
    })
}

/// `schema` with every vector property's `embedder` naming a key of `renames` (with or
/// without a normalization suffix) renamed; `None` when nothing changes.
fn renamed_schema(schema: &GraphSchema, renames: &[(String, String)]) -> Option<GraphSchema> {
    let mut out = schema.clone();
    let mut changed = false;
    let roots = out
        .vertices
        .values_mut()
        .map(|v| &mut v.schema)
        .chain(out.edges.values_mut().map(|e| &mut e.schema));
    for root in roots {
        for node in root.properties.values_mut() {
            let Some(vector) = node.vector.as_mut() else {
                continue;
            };
            let Some(embedder) = vector.embedder.as_mut() else {
                continue;
            };
            let base = model_catalog::base_identity(embedder).to_string();
            if let Some((_, new)) = renames.iter().find(|(old, _)| *old == base) {
                let suffix = embedder[base.len()..].to_string();
                *embedder = format!("{new}{suffix}");
                changed = true;
            }
        }
    }
    changed.then_some(out)
}

/// Step 2: the `INTENT` of the migration of `legacy`, written before `GRAPH` changes.
pub(crate) fn begin(
    dir: &DirPath,
    from_version: u32,
    legacy: &[LegacyModel],
) -> Result<(), PersistError> {
    Intent {
        op: UPGRADE_MODELS_OP.to_string(),
        files: legacy
            .iter()
            .map(|m| format!("{}/{}", models::MODELS_DIR, m.model.id))
            .collect(),
        started_at: crate::graph::undo::now_micros(),
        params: vec![("from".to_string(), from_version.to_string())],
    }
    .write(dir)
}

/// The old store format version an `INTENT` of the migration names (3 when it does not say).
pub(crate) fn intent_from(intent: &Intent) -> u32 {
    intent
        .param("from")
        .and_then(|v| v.parse().ok())
        .unwrap_or(3)
}

/// Step 3 (and the completion of an interrupted migration): the models of `models/` as blobs
/// and ONE commit defining them, then `models/` and the `INTENT` removed. `dir`'s `GRAPH` is
/// of the current version; the caller holds the writer lock (or `dir` is a new copy). A store
/// without `models/` only loses its `INTENT`.
pub(crate) fn complete(
    dir: &DirPath,
    graph_file: GraphFile,
    from_version: u32,
) -> Result<Migrated, PersistError> {
    let models_dir = models::models_dir(dir);
    let present = models_dir
        .try_kind()
        .map_err(|err| PersistError::io("reading the store's models", err))?
        == Some(EntryKind::Dir);
    if !present {
        Intent::clear(dir)?;
        return Ok(Migrated::default());
    }
    let legacy = check_legacy(dir)?;
    let repos = repos_of(&legacy);
    let mut converted: Vec<Converted> = Vec::new();
    let options = ClosedCommitOptions::new()
        .with_principal(UPGRADE_PRINCIPAL)
        .with_attribute(
            "upgrade",
            &format!("{from_version}->{}", super::STORE_VERSION),
        );
    let committed = closed_commit::commit_catalog_locked(
        dir,
        graph_file,
        OPERATION,
        &options,
        &mut |view, dir| {
            converted.clear();
            let state = &view.state;
            let mut definitions =
                model_catalog::model_definitions(state).map_err(PersistError::Graph)?;
            let mut mutations = Vec::new();
            let mut events = Vec::new();
            let mut renames: Vec<(String, String)> = Vec::new();
            for (legacy, repo) in legacy.iter().zip(&repos) {
                let former = legacy.model.identity.clone();
                if let Some(definition) = defined(&definitions, legacy.short()) {
                    let model = definition.as_model();
                    converted.push(Converted {
                        repo: model.map(|m| m.repo().to_string()).unwrap_or_default(),
                        identity: definition.name().to_string(),
                        bytes: model.map_or(0, |m| m.blob().length()),
                        former_identity: former.clone(),
                    });
                    if former != definition.name() {
                        renames.push((former, definition.name().to_string()));
                    }
                    continue;
                }
                let prepared = prepared(dir, legacy, repo);
                let expected = prepared.tar_length();
                let Prepared {
                    manifest,
                    files,
                    identity,
                    dimension,
                    load_bytes,
                } = prepared;
                let mut stream = TarStream::new(files);
                let blob = blobs::write_blob(dir, &mut stream, &mut |_| {})
                    .map_err(|err| damaged(dir, &legacy.model.id, err))?
                    .blob;
                if blob.length() != expected {
                    return Err(damaged(
                        dir,
                        &legacy.model.id,
                        format!(
                            "its tar is {} bytes, {expected} were expected",
                            blob.length()
                        ),
                    ));
                }
                let bytes = blob.length();
                let definition = Prepared {
                    manifest,
                    files: Vec::new(),
                    identity: identity.clone(),
                    dimension,
                    load_bytes,
                }
                .definition(blob, false);
                mutations.extend(model_catalog::import_changes(
                    &definitions,
                    None,
                    definition.clone(),
                ));
                definitions.push(definition);
                events.push((
                    &catalogue::MODEL_MIGRATED,
                    Event::new(format!(
                        "the embedding model {former} of the store's models/ directory became \
                         the blob of the model {identity}"
                    ))
                    .field("repo", repo.as_str())
                    .field("identity", identity.as_str())
                    .field("former_identity", former.as_str())
                    .count("bytes", bytes),
                ));
                if former != identity {
                    renames.push((former.clone(), identity.clone()));
                }
                converted.push(Converted {
                    repo: repo.clone(),
                    identity,
                    former_identity: former,
                    bytes,
                });
            }
            if !renames.is_empty()
                && let Some(schema) = state.get_schema().map_err(PersistError::Graph)?
                && let Some(after) = renamed_schema(&schema, &renames)
            {
                mutations.push(Mutation::set_schema(schema, after));
            }
            Ok(closed_commit::CatalogPlan { mutations, events })
        },
    )?;
    // Step 6: the model directories, each renamed to a temporary name first (an open removes
    // what an interruption leaves), then `models/` itself.
    remove_models_dir(dir)?;
    Intent::clear(dir)?;
    Ok(Migrated {
        converted,
        commit_seq: committed.commit_seq,
        state: Some(committed.state),
    })
}

/// Removes `models/` (idempotent): every entry renamed to `tmp-<32 hex>` and removed, then the
/// directory.
pub(crate) fn remove_models_dir(dir: &DirPath) -> Result<(), PersistError> {
    let io = |err| PersistError::io("removing the migrated models directory", err);
    let models_dir = models::models_dir(dir);
    if models_dir.try_kind().map_err(io)? != Some(EntryKind::Dir) {
        return Ok(());
    }
    for (name, _) in models_dir.list().map_err(io)? {
        let entry = models_dir.join(&name);
        let doomed = if name.starts_with(TMP_PREFIX) {
            entry
        } else {
            let tmp = models_dir.join(format!("{TMP_PREFIX}{:032x}", crate::value::uuid::new_v7()));
            entry.rename_to(&tmp).map_err(io)?;
            tmp
        };
        files::remove_path(&doomed).map_err(io)?;
    }
    files::remove_path(&models_dir).map_err(io)?;
    files::sync_dir(dir)
}

/// The migration of the `models/` a NEW store of the current version received from an older
/// source (a `.gsnap` of Graphersal 0.4.0 with model rows, a repair of a 0.4.0 store): the
/// same steps as the upgrade's, under the same `INTENT`. Nothing to do without `models/`.
pub(crate) fn migrate_new_store(dir: &DirPath) -> Result<Migrated, PersistError> {
    let present = models::models_dir(dir)
        .try_kind()
        .map_err(|err| PersistError::io("reading the store's models", err))?
        == Some(EntryKind::Dir);
    if !present {
        return Ok(Migrated::default());
    }
    let legacy = check_legacy(dir)?;
    begin(dir, 3, &legacy)?;
    let graph_file = files::read_graph_file(dir)?.file;
    complete(dir, graph_file, 3)
}
