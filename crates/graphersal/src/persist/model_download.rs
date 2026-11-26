//! The Hugging Face download of Model2Vec models into the download cache (feature
//! `model-download`, native only; book page "Embedding Models in a Store", section "Downloading
//! from a host"). `graphersal embed-model download` is a thin front end of this module.
//!
//! A download fetches the three files of a Model2Vec model (`config.json`, `tokenizer.json`,
//! `model.safetensors`) from `<endpoint>/<repo>/resolve/<revision>/<file>`, the model card
//! (`README.md`), the repository's `LICENSE*`/`NOTICE*` files and its metadata
//! (`<endpoint>/api/models/<repo>/revision/<revision>`: `cardData.license`,
//! `cardData.base_model`, `gated`). Redirects are followed by hand, so the hash Hugging Face
//! states on the first answer (`X-Linked-Etag`: SHA-256 of an LFS file; a 40-hex ETag: the git
//! blob id) is checked while the body streams into a `.part` file, and the token goes to the
//! endpoint's host only. Everything lands in a hidden staging directory beside the target; the
//! manifest [`ModelCache::MANIFEST_FILE`] (format 2) is written, the model loaded once as a
//! check, and only then the staging directory is renamed to `<cache>/<repo>`. Any failure, a
//! cancellation or a kill leaves no model behind: at most a hidden staging directory, which the
//! next download of the same repository removes.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use super::{ModelCache, ModelRole, NewModel};
use crate::catalog::BaseModel;
use crate::exec::CancelToken;

/// The largest file a download accepts when the server states no size.
const MAX_FILE_BYTES: u64 = 8 << 30;

/// The largest metadata answer read.
const MAX_METADATA_BYTES: u64 = 8 << 20;

/// The model card's file name.
const CARD_FILE: &str = "README.md";

/// What to download: a Hugging Face repository at a revision, from an endpoint, with an
/// optional token and cancellation ([`ModelCache::download`]).
///
/// The library reads no environment variable: the command line passes
/// `$GRAPHERSAL_HF_ENDPOINT` as [`with_endpoint`](Self::with_endpoint) and `$HF_TOKEN` as
/// [`with_token`](Self::with_token); a host decides for itself.
///
/// ```rust,no_run
/// use graphersal::persist::{DownloadEvent, ModelCache, ModelDownload};
///
/// let cache = ModelCache::at("/srv/graphersal/models");
/// let request = ModelDownload::new("minishlab/potion-base-8M").with_revision("main");
/// let report = cache
///     .download(&request, |event| {
///         if let DownloadEvent::FileDone { name, bytes, .. } = event {
///             println!("{name}: {bytes} bytes");
///         }
///     })
///     .unwrap();
/// println!("{} in {}", report.identity, report.dir.display());
/// for warning in &report.warnings {
///     eprintln!("warning: {warning}");
/// }
/// ```
#[derive(Clone)]
pub struct ModelDownload {
    repo: String,
    revision: String,
    endpoint: String,
    token: Option<String>,
    cancel: Option<CancelToken>,
}

impl std::fmt::Debug for ModelDownload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The token is a secret: never shown.
        f.debug_struct("ModelDownload")
            .field("repo", &self.repo)
            .field("revision", &self.revision)
            .field("endpoint", &self.endpoint)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("cancel", &self.cancel.is_some())
            .finish()
    }
}

impl ModelDownload {
    /// The default endpoint: `https://huggingface.co`.
    pub const DEFAULT_ENDPOINT: &'static str = "https://huggingface.co";

    /// The default revision: `main`.
    pub const DEFAULT_REVISION: &'static str = "main";

    /// The files of a Model2Vec model in the Model2Vec layout, in download order.
    pub const MODEL_FILES: [&'static str; 3] =
        ["config.json", "tokenizer.json", "model.safetensors"];

    /// The download of the repository `repo` (`owner/name`) at [`Self::DEFAULT_REVISION`] from
    /// [`Self::DEFAULT_ENDPOINT`], without a token.
    pub fn new(repo: impl Into<String>) -> Self {
        ModelDownload {
            repo: repo.into(),
            revision: Self::DEFAULT_REVISION.to_string(),
            endpoint: Self::DEFAULT_ENDPOINT.to_string(),
            token: None,
            cancel: None,
        }
    }

    /// The revision: a branch, tag or commit (letters, digits, `.`, `_`, `-`, `/`).
    pub fn with_revision(mut self, revision: impl Into<String>) -> Self {
        self.revision = revision.into();
        self
    }

    /// The endpoint (`scheme://host[:port]`, a mirror or a test server); a trailing `/` is
    /// dropped.
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into().trim_end_matches('/').to_string();
        self
    }

    /// A Hugging Face token for gated or private repositories: sent as a bearer token to the
    /// endpoint's host only (never to a redirect's other host), never logged.
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// A cancellation token: checked before every request and after every chunk of a body
    /// (cooperative: a request waiting for an answer finishes its wait first). A cancelled
    /// download fails with [`ModelDownloadError::Cancelled`] and leaves nothing behind.
    pub fn with_cancel(mut self, cancel: CancelToken) -> Self {
        self.cancel = Some(cancel);
        self
    }

    /// The repository.
    pub fn repo(&self) -> &str {
        &self.repo
    }

    /// The revision.
    pub fn revision(&self) -> &str {
        &self.revision
    }

    /// The endpoint.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Whether `repo` is a Hugging Face repository name a download accepts: `owner/name` (or a
    /// bare `name`), each part 1 to 96 ASCII letters, digits, `.`, `_` and `-`, not starting
    /// with `.`. The same check [`Self::validate`] and a store's model import make; a host uses
    /// it for a repository name it gets on its own (an import's `--repo`).
    pub fn is_repo_name(repo: &str) -> bool {
        super::model_cache::is_repo_name(repo)
    }

    /// Checks the request the way a download starts: the repository name
    /// ([`Self::is_repo_name`]) and the revision (a branch, tag or commit of ASCII letters,
    /// digits, `.`, `_`, `-` and `/`, not starting with `/`, without `..`). A host calls it to
    /// refuse a bad request before it starts a download (a job, a thread); the download makes
    /// the same check first and fails with the same error, having fetched nothing. The
    /// endpoint is not checked here (a wrong one fails the first request).
    ///
    /// # Errors
    ///
    /// [`ModelDownloadError::InvalidRequest`] naming what is wrong (the text the command line
    /// prints as its usage error).
    ///
    /// ```rust
    /// use graphersal::persist::{ModelDownload, ModelDownloadError};
    ///
    /// assert!(ModelDownload::new("minishlab/potion-base-8M").validate().is_ok());
    /// let err = ModelDownload::new("../etc").validate().unwrap_err();
    /// assert!(matches!(err, ModelDownloadError::InvalidRequest { .. }));
    /// assert!(ModelDownload::new("acme/tiny").with_revision("../x").validate().is_err());
    /// ```
    pub fn validate(&self) -> Result<(), ModelDownloadError> {
        let repo = self.repo.as_str();
        if !Self::is_repo_name(repo) {
            return Err(ModelDownloadError::InvalidRequest {
                reason: format!(
                    "'{repo}' is not a Hugging Face repository name: owner/name of letters, \
                     digits, '.', '_' and '-' (for example minishlab/potion-base-8M)"
                ),
            });
        }
        let revision = self.revision.as_str();
        if !is_revision(revision) {
            return Err(ModelDownloadError::InvalidRequest {
                reason: format!(
                    "'{revision}' is not a revision: a branch, tag or commit of letters, digits, \
                     '.', '_', '-' and '/'"
                ),
            });
        }
        Ok(())
    }
}

/// What a download reports while it runs (the callback of [`ModelCache::download`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DownloadEvent<'a> {
    /// The request is valid and the staging directory made; the first request follows.
    #[non_exhaustive]
    Started {
        /// The repository.
        repo: &'a str,
        /// The revision asked for.
        revision: &'a str,
        /// The endpoint (without a trailing `/`).
        endpoint: &'a str,
    },
    /// A chunk of the file `name` arrived: `bytes` so far of `total` (when the server states
    /// it).
    #[non_exhaustive]
    Progress {
        /// The file.
        name: &'a str,
        /// The bytes received so far.
        bytes: u64,
        /// The size the server states.
        total: Option<u64>,
    },
    /// The body of the file `name` ended after `bytes` (its size and hash are checked next).
    #[non_exhaustive]
    FileDone {
        /// The file.
        name: &'a str,
        /// The bytes received.
        bytes: u64,
        /// The size the server states.
        total: Option<u64>,
    },
    /// Something the caller should know that does not stop the download (the repository's
    /// metadata could not be read, so its licence is unknown). Also in
    /// [`ModelDownloadReport::notes`].
    #[non_exhaustive]
    Note {
        /// The note's text.
        text: &'a str,
    },
}

/// One downloaded file of a model.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DownloadedFile {
    /// The file name (`config.json`, `README.md`, `LICENSE`, ...).
    pub name: String,
    /// Its role in the model.
    pub role: ModelRole,
    /// Its size in bytes.
    pub size: u64,
    /// Its SHA-256 (lowercase hex).
    pub sha256: String,
}

/// What a download fetched and recorded in the manifest ([`ModelCache::download`]).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ModelDownloadReport {
    /// The repository.
    pub repo: String,
    /// The revision asked for.
    pub revision: String,
    /// The commit the server resolved the revision to (`X-Repo-Commit`), when it said.
    pub commit: Option<String>,
    /// The model identity (`model2vec/<repo>@<12 hex>`), from the check load.
    pub identity: String,
    /// The dimension of the embedding tensor, when the safetensors header says.
    pub dimension: Option<u64>,
    /// The licence the repository's metadata states (an SPDX id), else `unknown`.
    pub license: String,
    /// Whether the repository has a licence file (downloaded with the model).
    pub license_file: bool,
    /// The base models the model card names (at most four) with their licences.
    pub base_models: Vec<BaseModel>,
    /// Whether the repository is gated.
    pub gated: bool,
    /// Every downloaded file, in download order.
    pub files: Vec<DownloadedFile>,
    /// Where the model is: the staging directory of a [`StagedModel`], the cache directory
    /// `<cache>/<repo>` once installed.
    pub dir: PathBuf,
    /// The notes of the download ([`DownloadEvent::Note`]).
    pub notes: Vec<String>,
    /// The warnings about passing the model on ([`model_licence_warnings`]): an unknown or
    /// non-commercial licence, custom terms, a gated repository.
    pub warnings: Vec<String>,
}

/// Why a download failed. Nothing is left behind in every case. Its text (`Display`) is the
/// whole message, some with a `Help:` line.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ModelDownloadError {
    /// The request is invalid (repository name, revision): nothing was fetched.
    #[error("{reason}")]
    #[non_exhaustive]
    InvalidRequest {
        /// The message.
        reason: String,
    },
    /// The server refused a file (HTTP 401 or 403): the repository does not exist, or it is
    /// gated or private and needs a token.
    #[error("{reason}")]
    #[non_exhaustive]
    Refused {
        /// The HTTP status.
        status: u16,
        /// The message.
        reason: String,
    },
    /// The repository at that revision has no such file (HTTP 404).
    #[error("{reason}")]
    #[non_exhaustive]
    NotFound {
        /// The message.
        reason: String,
    },
    /// The transfer failed: the server unreachable, another HTTP status, a redirect that
    /// cannot be followed, a body that broke off or is too large.
    #[error("{reason}")]
    #[non_exhaustive]
    Transfer {
        /// The HTTP status, when the server answered.
        status: Option<u16>,
        /// The message.
        reason: String,
    },
    /// A file is not the one the server describes (its SHA-256 or git blob id differs).
    #[error("{reason}")]
    #[non_exhaustive]
    Integrity {
        /// The message.
        reason: String,
    },
    /// The files arrived but do not load as a Model2Vec model.
    #[error("{reason}")]
    #[non_exhaustive]
    NotAModel {
        /// The message.
        reason: String,
    },
    /// A local file or directory could not be written, read or moved.
    #[error("{reason}")]
    #[non_exhaustive]
    Io {
        /// The message.
        reason: String,
    },
    /// The download was cancelled ([`ModelDownload::with_cancel`]).
    #[error("{reason}")]
    #[non_exhaustive]
    Cancelled {
        /// The message.
        reason: String,
    },
}

fn io_error(reason: String) -> ModelDownloadError {
    ModelDownloadError::Io { reason }
}

fn transfer(status: Option<u16>, reason: String) -> ModelDownloadError {
    ModelDownloadError::Transfer { status, reason }
}

/// A staging directory removed on drop unless it was moved into place.
struct Staging {
    path: PathBuf,
    done: bool,
}

impl Drop for Staging {
    fn drop(&mut self) {
        if !self.done {
            let _ = fs::remove_dir_all(&self.path);
            // The owner directory a failed first download created (removed only when empty).
            if let Some(parent) = self.path.parent() {
                let _ = fs::remove_dir(parent);
            }
        }
    }
}

/// A downloaded and checked model not yet in the cache ([`ModelCache::download_staged`]):
/// [`install`](Self::install) moves it to `<cache>/<repo>`, [`new_model`](Self::new_model)
/// reads it for a Store import instead. Dropping it removes the staging directory (keep it
/// alive until an import of its [`NewModel`] returned).
#[derive(Debug)]
pub struct StagedModel {
    staging: Staging,
    target: PathBuf,
    report: ModelDownloadReport,
}

impl std::fmt::Debug for Staging {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Staging")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl StagedModel {
    /// What was downloaded (its `dir` is the staging directory).
    pub fn report(&self) -> &ModelDownloadReport {
        &self.report
    }

    /// The staging directory holding the files and the manifest.
    pub fn dir(&self) -> &Path {
        &self.staging.path
    }

    /// The model as a [`NewModel`] for [`ModelSource::Files`](super::ModelSource::Files): its
    /// files (opened from the staging directory), repository, revision, commit, licence, base
    /// models and gated flag.
    pub fn new_model(&self) -> Result<NewModel, ModelDownloadError> {
        let report = &self.report;
        let mut new = NewModel::new()
            .with_repo(report.repo.as_str())
            .with_revision(report.revision.as_str())
            .with_license(report.license.as_str())
            .with_gated(report.gated);
        if let Some(commit) = &report.commit {
            new = new.with_commit(commit.as_str());
        }
        for base in &report.base_models {
            new = new.with_base_model(base.clone());
        }
        for file in &report.files {
            let reader = fs::File::open(self.staging.path.join(&file.name)).map_err(|err| {
                io_error(format!("cannot read the downloaded {}: {err}", file.name))
            })?;
            new = new.with_file(file.name.as_str(), file.role, reader);
        }
        Ok(new)
    }

    /// Moves the model to `<cache>/<repo>`, replacing an earlier download of the repository,
    /// and returns the report with `dir` set to it.
    pub fn install(mut self) -> Result<ModelDownloadReport, ModelDownloadError> {
        install(&self.staging, &self.target)?;
        self.staging.done = true;
        let mut report = self.report.clone();
        report.dir = self.target.clone();
        Ok(report)
    }
}

impl ModelCache {
    /// Downloads a Model2Vec model from Hugging Face into the cache (`<cache>/<repo>`, an
    /// earlier download of the repository replaced): the three model files, the model card,
    /// the licence and notice files and the manifest [`Self::MANIFEST_FILE`], each hash the
    /// server states checked, the model loaded once as a check. `events` gets the progress
    /// ([`DownloadEvent`]). Any failure leaves no model (module docs above).
    ///
    /// Downloads of one repository into one cache must not run at the same time (each removes
    /// the staging leftovers of the repository); downloads of different repositories may.
    pub fn download(
        &self,
        request: &ModelDownload,
        events: impl FnMut(&DownloadEvent<'_>),
    ) -> Result<ModelDownloadReport, ModelDownloadError> {
        self.download_staged(request, events)?.install()
    }

    /// [`Self::download`] without the last step: the checked model stays in its staging
    /// directory (beside its place in the cache) until [`StagedModel::install`], or goes into
    /// a Store through [`StagedModel::new_model`] and never into the cache.
    pub fn download_staged(
        &self,
        request: &ModelDownload,
        mut events: impl FnMut(&DownloadEvent<'_>),
    ) -> Result<StagedModel, ModelDownloadError> {
        run_download(self, request, &mut events)
    }
}

/// Whether `revision` is a branch, tag or commit name this download accepts.
fn is_revision(revision: &str) -> bool {
    !revision.is_empty()
        && !revision.starts_with('/')
        && !revision.contains("..")
        && revision
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
}

fn cancelled(request: &ModelDownload) -> Result<(), ModelDownloadError> {
    match &request.cancel {
        Some(cancel) if cancel.is_cancelled() => Err(ModelDownloadError::Cancelled {
            reason: format!(
                "the download of {} was cancelled; nothing was kept",
                request.repo
            ),
        }),
        _ => Ok(()),
    }
}

/// One fetched file.
struct Fetched {
    name: String,
    size: u64,
    sha256: String,
    /// The commit the server resolved the revision to (`X-Repo-Commit`).
    commit: Option<String>,
}

fn run_download(
    cache: &ModelCache,
    request: &ModelDownload,
    events: &mut dyn FnMut(&DownloadEvent<'_>),
) -> Result<StagedModel, ModelDownloadError> {
    request.validate()?;
    let repo = request.repo.as_str();
    let revision = request.revision.as_str();
    cancelled(request)?;
    let endpoint = request.endpoint.as_str();
    let target = cache.path_of(repo);
    let parent = target
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| cache.dir().to_path_buf());
    fs::create_dir_all(&parent)
        .map_err(|err| io_error(format!("cannot create {}: {err}", parent.display())))?;
    let leaf = repo.rsplit('/').next().unwrap_or(repo);
    remove_stale_staging(&parent, leaf);
    let staging = Staging {
        path: parent.join(format!(
            ".{leaf}.download-{}-{:032x}",
            std::process::id(),
            crate::value::uuid::new_v7()
        )),
        done: false,
    };
    fs::create_dir_all(&staging.path)
        .map_err(|err| io_error(format!("cannot create {}: {err}", staging.path.display())))?;

    events(&DownloadEvent::Started {
        repo,
        revision,
        endpoint,
    });
    let client = Client::new(endpoint, request.token.clone());
    let encoded_revision = revision.replace('/', "%2F");
    let mut fetched: Vec<(Fetched, ModelRole)> = Vec::new();
    for name in ModelDownload::MODEL_FILES {
        let url = format!("{endpoint}/{repo}/resolve/{encoded_revision}/{name}");
        let file = client.fetch(&url, request, name, &staging.path, false, events)?;
        let file = file.ok_or_else(|| ModelDownloadError::NotFound {
            reason: format!("{repo} has no {name}"),
        })?;
        fetched.push((file, model_role(name)));
    }
    let commit = fetched.iter().find_map(|(f, _)| f.commit.clone());

    // What travels with the model: its metadata, card and licence files (format spec 4.5).
    let mut notes = Vec::new();
    let meta = client.metadata(endpoint, repo, Some(&encoded_revision));
    if let Some(note) = &meta.note {
        events(&DownloadEvent::Note { text: note });
        notes.push(note.clone());
    }
    let card_url = format!("{endpoint}/{repo}/resolve/{encoded_revision}/{CARD_FILE}");
    if let Some(card) = client.fetch(&card_url, request, CARD_FILE, &staging.path, true, events)? {
        fetched.push((card, ModelRole::Card));
    }
    for (name, role) in licence_files(&meta.siblings) {
        let url = format!("{endpoint}/{repo}/resolve/{encoded_revision}/{name}");
        let file = client
            .fetch(&url, request, &name, &staging.path, false, events)?
            .ok_or_else(|| ModelDownloadError::NotFound {
                reason: format!("{repo} lists {name} but does not serve it"),
            })?;
        fetched.push((file, role));
    }
    cancelled(request)?;
    let base_models: Vec<BaseModel> = meta
        .base_models
        .iter()
        .take(4)
        .map(|base| BaseModel::new(base.as_str(), client.metadata(endpoint, base, None).license))
        .collect();
    let license = meta
        .license
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let license_file = fetched.iter().any(|(_, role)| *role == ModelRole::License);
    let manifest = json!({
        "format": 2,
        "family": "model2vec",
        "repo": repo,
        "revision": revision,
        "commit": commit,
        "license": license,
        "license_text": if license_file { "file" } else { "none" },
        "base_models": base_models.iter().map(|base| match &base.license {
            Some(license) => json!({"repo": base.repo, "license": license}),
            None => json!({"repo": base.repo}),
        }).collect::<Vec<_>>(),
        "gated": meta.gated,
        "files": fetched.iter().map(|(f, role)| json!({
            "name": f.name,
            "role": role.as_str(),
            "size": f.size,
            "sha256": f.sha256,
        })).collect::<Vec<_>>(),
        "downloaded_at": now_text(),
    });
    let manifest_path = staging.path.join(ModelCache::MANIFEST_FILE);
    write_synced(
        &manifest_path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&manifest).unwrap_or_default()
        )
        .as_bytes(),
    )
    .map_err(|err| io_error(format!("cannot write {}: {err}", manifest_path.display())))?;

    // Load it once: a repository that is not a Model2Vec model fails here, not later.
    let identity =
        ModelCache::check(&staging.path).map_err(|err| ModelDownloadError::NotAModel {
            reason: format!(
                "{repo} was downloaded but does not load as a Model2Vec model: {}",
                reason_of(&err)
            ),
        })?;
    let dimension = safetensors_dimension(&staging.path.join("model.safetensors"));
    let warnings = model_licence_warnings(repo, &license, meta.gated);
    let report = ModelDownloadReport {
        repo: repo.to_string(),
        revision: revision.to_string(),
        commit,
        identity,
        dimension,
        license,
        license_file,
        base_models,
        gated: meta.gated,
        files: fetched
            .into_iter()
            .map(|(f, role)| DownloadedFile {
                name: f.name,
                role,
                size: f.size,
                sha256: f.sha256,
            })
            .collect(),
        dir: staging.path.clone(),
        notes,
        warnings,
    };
    cancelled(request)?;
    Ok(StagedModel {
        staging,
        target,
        report,
    })
}

/// The reason of a refusal, without the operation it names.
fn reason_of(err: &super::PersistError) -> String {
    match err {
        super::PersistError::Store { reason, .. } => reason.clone(),
        other => other.to_string(),
    }
}

/// The warnings about passing on the model of `repo` under `license` (an SPDX id or
/// `unknown`): an unknown or non-commercial licence, custom terms (`other`), a gated
/// repository. The texts the command line prints after `warning: `.
pub fn model_licence_warnings(repo: &str, license: &str, gated: bool) -> Vec<String> {
    let mut warnings = Vec::new();
    let lower = license.to_ascii_lowercase();
    if lower == "unknown" {
        warnings.push(format!(
            "the licence of {repo} is unknown (its model card names none, or its metadata \
             could not be read): check the repository before you hand on a store or file that \
             carries the model"
        ));
    } else if lower.contains("-nc")
        || lower.contains("noncommercial")
        || lower.contains("non-commercial")
    {
        warnings.push(format!(
            "{repo} is under {license}, a NON-COMMERCIAL licence: a store or file carrying it \
             may not be used or handed on commercially"
        ));
    } else if lower == "other" {
        warnings.push(format!(
            "{repo} is under custom terms (licence \"other\"): read its model card before you \
             hand on a store or file that carries the model"
        ));
    }
    if gated {
        warnings.push(format!(
            "{repo} is a GATED repository: the terms you accepted to download it may restrict \
             passing it on"
        ));
    }
    warnings
}

/// The role of one of the three model files by its name.
fn model_role(name: &str) -> ModelRole {
    match name {
        "tokenizer.json" => ModelRole::Tokenizer,
        "model.safetensors" => ModelRole::Weights,
        _ => ModelRole::Config,
    }
}

/// The licence and notice files of a repository's file list: `LICENSE*`, `LICENCE*`,
/// `COPYING*` (role licence) and `NOTICE*` (role notice) at its root, names a Store accepts.
fn licence_files(siblings: &[String]) -> Vec<(String, ModelRole)> {
    siblings
        .iter()
        .filter(|name| !name.contains('/') && is_plain_file_name(name))
        .filter_map(|name| {
            let upper = name.to_ascii_uppercase();
            if upper.starts_with("LICENSE")
                || upper.starts_with("LICENCE")
                || upper.starts_with("COPYING")
            {
                Some((name.clone(), ModelRole::License))
            } else if upper.starts_with("NOTICE") {
                Some((name.clone(), ModelRole::Notice))
            } else {
                None
            }
        })
        .take(8)
        .collect()
}

/// A file name a Store keeps in a model directory (format spec 4.5).
fn is_plain_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 40
        && !name.starts_with('.')
        && !name.starts_with("tmp-")
        && name != ModelCache::MANIFEST_FILE
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// The dimension of the embedding tensor of a safetensors file (its header: the 2-dimensional
/// tensor `embeddings`, `0` or `embedding.weight`); `None` when it does not say.
fn safetensors_dimension(path: &Path) -> Option<u64> {
    let mut file = fs::File::open(path).ok()?;
    let mut len = [0u8; 8];
    file.read_exact(&mut len).ok()?;
    let len = u64::from_le_bytes(len);
    if len > 100 << 20 {
        return None;
    }
    let mut header = vec![0u8; usize::try_from(len).ok()?];
    file.read_exact(&mut header).ok()?;
    let header: Value = serde_json::from_slice(&header).ok()?;
    ["embeddings", "0", "embedding.weight"]
        .iter()
        .find_map(|name| {
            let shape = header.get(name)?.get("shape")?.as_array()?;
            match shape.as_slice() {
                [_, cols] => cols.as_u64(),
                _ => None,
            }
        })
}

/// Moves the finished staging directory to `target`, replacing an older download.
fn install(staging: &Staging, target: &Path) -> Result<(), ModelDownloadError> {
    let old = target.with_file_name(format!(
        ".{}.old-{}-{:032x}",
        target
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
        std::process::id(),
        crate::value::uuid::new_v7()
    ));
    let replaced = target.exists();
    if replaced {
        fs::rename(target, &old).map_err(|err| {
            io_error(format!(
                "cannot replace the earlier download {}: {err}",
                target.display()
            ))
        })?;
    }
    if let Err(err) = fs::rename(&staging.path, target) {
        if replaced {
            let _ = fs::rename(&old, target);
        }
        return Err(io_error(format!(
            "cannot move the model to {}: {err}",
            target.display()
        )));
    }
    if replaced {
        let _ = fs::remove_dir_all(&old);
    }
    if let Some(parent) = target.parent() {
        sync_dir(parent);
    }
    Ok(())
}

/// Removes the leftovers of interrupted downloads of `leaf` in `parent`.
fn remove_stale_staging(parent: &Path, leaf: &str) {
    let prefixes = [format!(".{leaf}.download-"), format!(".{leaf}.old-")];
    for entry in fs::read_dir(parent).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if prefixes.iter().any(|p| name.starts_with(p.as_str())) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = fs::File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(handle) = fs::File::open(dir) {
        let _ = handle.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// Now, as the canonical UTC date-time text (whole seconds).
fn now_text() -> String {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_micros()).unwrap_or(i64::MAX))
        .unwrap_or(0);
    crate::value::temporal::DateTime::from_micros(micros - micros % 1_000_000, 0)
        .map(|t| t.to_string())
        .unwrap_or_default()
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// What the Hugging Face API says about a repository (the parts a Store records).
#[derive(Debug, Default)]
struct HubMetadata {
    /// `cardData.license` (an SPDX id), when stated.
    license: Option<String>,
    /// `cardData.base_model` (a string or a list).
    base_models: Vec<String>,
    /// `gated`: `false`, or `"auto"`/`"manual"`.
    gated: bool,
    /// The repository's files (`siblings[].rfilename`).
    siblings: Vec<String>,
    /// Why the metadata is missing, when it is.
    note: Option<String>,
}

impl HubMetadata {
    fn from_json(value: &Value) -> Self {
        let card = value.get("cardData");
        let license = card
            .and_then(|c| c.get("license"))
            .and_then(|l| match l {
                Value::String(text) => Some(text.clone()),
                Value::Array(items) => items.first().and_then(Value::as_str).map(str::to_string),
                _ => None,
            })
            .filter(|l| {
                !l.trim().is_empty() && l.len() <= 64 && l.chars().all(|c| c.is_ascii_graphic())
            });
        let base_models = match card.and_then(|c| c.get("base_model")) {
            Some(Value::String(base)) => vec![base.clone()],
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
            _ => Vec::new(),
        }
        .into_iter()
        .filter(|base| super::model_cache::is_repo_name(base))
        .collect();
        let gated = match value.get("gated") {
            Some(Value::Bool(gated)) => *gated,
            Some(Value::String(_)) => true,
            _ => false,
        };
        let siblings = value
            .get("siblings")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|s| s.get("rfilename").and_then(Value::as_str))
            .map(str::to_string)
            .collect();
        HubMetadata {
            license,
            base_models,
            gated,
            siblings,
            note: None,
        }
    }
}

/// The HTTP client of a download.
struct Client {
    agent: ureq::Agent,
    /// The authority (`host[:port]`) of the endpoint: the only one the token is sent to.
    endpoint_authority: String,
    token: Option<String>,
}

/// The `scheme://authority` part and the authority of an absolute URL.
fn origin_of(url: &str) -> Option<(&str, &str)> {
    let scheme_end = url.find("://")?;
    let rest = &url[scheme_end + 3..];
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let origin_len = scheme_end + 3 + authority_end;
    Some((&url[..origin_len], &rest[..authority_end]))
}

/// `location` (a redirect target) resolved against `base`.
fn resolve_location(base: &str, location: &str) -> Option<String> {
    if location.starts_with("http://") || location.starts_with("https://") {
        return Some(location.to_string());
    }
    let (origin, _) = origin_of(base)?;
    if let Some(rest) = location.strip_prefix("//") {
        let scheme = &base[..base.find("://")?];
        return Some(format!("{scheme}://{rest}"));
    }
    if location.starts_with('/') {
        return Some(format!("{origin}{location}"));
    }
    let path = base[origin.len()..].split(['?', '#']).next().unwrap_or("/");
    let dir = &path[..path.rfind('/').map_or(0, |i| i + 1)];
    Some(format!("{origin}{dir}{location}"))
}

/// The header `name` of a response, as text.
fn header(response: &ureq::http::Response<ureq::Body>, name: &str) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

/// The hash a server states for a file.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Stated {
    /// The SHA-256 of the content (Hugging Face LFS files: `X-Linked-Etag`).
    Sha256(String),
    /// The git blob id: SHA-1 of `blob <size>\0` + the content (Hugging Face files kept in git).
    GitSha1(String),
}

/// The hash inside an ETag value (`"<hex>"`, `W/"<hex>"`): 64 hex digits are a SHA-256, 40 a
/// git blob id; anything else is not a content hash.
fn etag_hash(value: &str) -> Option<Stated> {
    let hex = value.trim().trim_start_matches("W/").trim_matches('"');
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match hex.len() {
        64 => Some(Stated::Sha256(hex.to_lowercase())),
        40 => Some(Stated::GitSha1(hex.to_lowercase())),
        _ => None,
    }
}

impl Client {
    fn new(endpoint: &str, token: Option<String>) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_recv_response(Some(Duration::from_secs(120)))
            .user_agent(concat!("graphersal/", env!("CARGO_PKG_VERSION")))
            .build()
            .new_agent();
        Client {
            agent,
            endpoint_authority: origin_of(endpoint)
                .map(|(_, authority)| authority.to_string())
                .unwrap_or_default(),
            token,
        }
    }

    /// A GET of `url`, with the token when `url` is on the endpoint's host.
    fn get(&self, url: &str) -> ureq::RequestBuilder<ureq::typestate::WithoutBody> {
        let mut request = self.agent.get(url);
        if origin_of(url).is_some_and(|(_, a)| a == self.endpoint_authority)
            && let Some(token) = &self.token
        {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        request
    }

    /// The Hugging Face metadata of `repo` (at `revision`, URL-encoded; `None`: the default
    /// branch): `cardData.license`, `cardData.base_model`, `gated` and the file list. Best
    /// effort: an unreachable or unreadable answer gives empty metadata and a note.
    fn metadata(&self, endpoint: &str, repo: &str, revision: Option<&str>) -> HubMetadata {
        let url = match revision {
            Some(revision) => format!("{endpoint}/api/models/{repo}/revision/{revision}"),
            None => format!("{endpoint}/api/models/{repo}"),
        };
        let unreadable = |why: String| HubMetadata {
            note: Some(format!(
                "the metadata of {repo} could not be read ({why}): its licence is unknown"
            )),
            ..HubMetadata::default()
        };
        let response = match self.get(&url).call() {
            Ok(response) => response,
            Err(err) => return unreadable(err.to_string()),
        };
        let status = response.status().as_u16();
        if status != 200 {
            return unreadable(format!("HTTP {status}"));
        }
        let mut body = Vec::new();
        if let Err(err) = response
            .into_body()
            .into_reader()
            .take(MAX_METADATA_BYTES)
            .read_to_end(&mut body)
        {
            return unreadable(err.to_string());
        }
        let Ok(value) = serde_json::from_slice::<Value>(&body) else {
            return unreadable("not JSON".to_string());
        };
        HubMetadata::from_json(&value)
    }

    /// Downloads `url` (the file `name` of the request's repository) into `dir/name`,
    /// following redirects itself so the stated hash of the first answer is seen and the token
    /// goes to the endpoint's host only. `optional`: a 404 is `None`.
    fn fetch(
        &self,
        url: &str,
        request: &ModelDownload,
        name: &str,
        dir: &Path,
        optional: bool,
        events: &mut dyn FnMut(&DownloadEvent<'_>),
    ) -> Result<Option<Fetched>, ModelDownloadError> {
        let repo = request.repo.as_str();
        let revision = request.revision.as_str();
        let mut url = url.to_string();
        let mut stated: Option<Stated> = None;
        let mut expected_size: Option<u64> = None;
        let mut commit: Option<String> = None;
        for _ in 0..10 {
            cancelled(request)?;
            let response = self
                .get(&url)
                .call()
                .map_err(|err| transfer(None, format!("cannot fetch {name} of {repo}: {err}")))?;
            let status = response.status().as_u16();
            if stated.is_none() {
                stated = header(&response, "x-linked-etag").and_then(|v| etag_hash(&v));
            }
            if expected_size.is_none() {
                expected_size =
                    header(&response, "x-linked-size").and_then(|v| v.trim().parse().ok());
            }
            if commit.is_none() {
                commit = header(&response, "x-repo-commit");
            }
            match status {
                200 => {
                    if stated.is_none() {
                        stated = header(&response, "etag").and_then(|v| etag_hash(&v));
                    }
                    if expected_size.is_none() {
                        expected_size =
                            header(&response, "content-length").and_then(|v| v.trim().parse().ok());
                    }
                    let (sha256, size) = stream(
                        response,
                        request,
                        name,
                        dir,
                        expected_size,
                        stated.as_ref(),
                        events,
                    )?;
                    return Ok(Some(Fetched {
                        name: name.to_string(),
                        size,
                        sha256,
                        commit,
                    }));
                }
                301 | 302 | 303 | 307 | 308 => {
                    let location = header(&response, "location").ok_or_else(|| {
                        transfer(
                            Some(status),
                            format!("{name} of {repo}: a redirect without a Location"),
                        )
                    })?;
                    url = resolve_location(&url, &location).ok_or_else(|| {
                        transfer(
                            Some(status),
                            format!("{name} of {repo}: cannot follow the redirect"),
                        )
                    })?;
                }
                401 | 403 => {
                    return Err(ModelDownloadError::Refused {
                        status,
                        reason: format!(
                            "{repo} refused the download of {name} (HTTP {status})\nHelp: the \
                             repository does not exist (Hugging Face answers 401 then), or it \
                             is gated or private and needs a Hugging Face token in HF_TOKEN \
                             (and the licence accepted on its page)"
                        ),
                    });
                }
                404 if optional => return Ok(None),
                404 => {
                    return Err(ModelDownloadError::NotFound {
                        reason: format!(
                            "{repo} at revision {revision} has no {name} (HTTP 404)\nHelp: \
                             check the repository name and the revision; a Model2Vec model \
                             holds config.json, tokenizer.json and model.safetensors at its \
                             root (for example minishlab/potion-base-8M)"
                        ),
                    });
                }
                _ => {
                    return Err(transfer(
                        Some(status),
                        format!("the download of {name} of {repo} failed: HTTP {status}"),
                    ));
                }
            }
        }
        Err(transfer(
            None,
            format!("{name} of {repo}: too many redirects"),
        ))
    }
}

/// Streams the body into `dir/name.part`, checks size and hash, renames it to `dir/name`.
fn stream(
    response: ureq::http::Response<ureq::Body>,
    request: &ModelDownload,
    name: &str,
    dir: &Path,
    expected_size: Option<u64>,
    stated: Option<&Stated>,
    events: &mut dyn FnMut(&DownloadEvent<'_>),
) -> Result<(String, u64), ModelDownloadError> {
    let part = dir.join(format!("{name}.part"));
    let mut file = fs::File::create(&part)
        .map_err(|err| io_error(format!("cannot create {}: {err}", part.display())))?;
    let mut reader = response.into_body().into_reader();
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    // A git blob id needs the size up front: checked only when the server states it.
    let mut git = match (stated, expected_size) {
        (Some(Stated::GitSha1(_)), Some(size)) => {
            let mut context = ring::digest::Context::new(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY);
            context.update(format!("blob {size}\0").as_bytes());
            Some(context)
        }
        _ => None,
    };
    let mut buffer = vec![0u8; 256 * 1024];
    let mut total: u64 = 0;
    let limit = expected_size.unwrap_or(MAX_FILE_BYTES);
    loop {
        cancelled(request)?;
        let read = reader
            .read(&mut buffer)
            .map_err(|err| transfer(None, format!("the download of {name} broke off: {err}")))?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > limit {
            return Err(transfer(
                None,
                format!("{name} is larger than {}", human_bytes(limit)),
            ));
        }
        digest.update(&buffer[..read]);
        if let Some(git) = git.as_mut() {
            git.update(&buffer[..read]);
        }
        file.write_all(&buffer[..read])
            .map_err(|err| io_error(format!("cannot write {}: {err}", part.display())))?;
        events(&DownloadEvent::Progress {
            name,
            bytes: total,
            total: expected_size,
        });
    }
    file.sync_all()
        .map_err(|err| io_error(format!("cannot write {}: {err}", part.display())))?;
    drop(file);
    events(&DownloadEvent::FileDone {
        name,
        bytes: total,
        total: expected_size,
    });
    if let Some(expected) = expected_size
        && total != expected
    {
        return Err(transfer(
            None,
            format!("the download of {name} broke off: {total} of {expected} bytes"),
        ));
    }
    let sha256 = hex(digest.finish().as_ref());
    let mismatch = match stated {
        Some(Stated::Sha256(expected)) if *expected != sha256 => {
            Some(format!("SHA-256 {sha256}, the server states {expected}"))
        }
        Some(Stated::GitSha1(expected)) => git
            .map(|git| hex(git.finish().as_ref()))
            .filter(|actual| actual != expected)
            .map(|actual| format!("git blob id {actual}, the server states {expected}")),
        _ => None,
    };
    if let Some(mismatch) = mismatch {
        return Err(ModelDownloadError::Integrity {
            reason: format!(
                "{name}: the download is not the file the server describes ({mismatch}); \
                 nothing was kept"
            ),
        });
    }
    fs::rename(&part, dir.join(name))
        .map_err(|err| io_error(format!("cannot rename {}: {err}", part.display())))?;
    Ok((sha256, total))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revisions() {
        for ok in ["main", "v1.0", "refs/pr/1", "0123abcd"] {
            assert!(is_revision(ok), "{ok}");
        }
        for bad in ["", "/main", "a/../b", "a b", "x?y"] {
            assert!(!is_revision(bad), "{bad}");
        }
    }

    #[test]
    fn redirects_resolve() {
        let base = "https://huggingface.co/a/b/resolve/main/model.safetensors";
        assert_eq!(
            resolve_location(base, "https://cdn.example/x?y=1").as_deref(),
            Some("https://cdn.example/x?y=1")
        );
        assert_eq!(
            resolve_location(base, "/api/resolve-cache/models/a/b/c/config.json").as_deref(),
            Some("https://huggingface.co/api/resolve-cache/models/a/b/c/config.json")
        );
        assert_eq!(
            resolve_location(base, "other.json").as_deref(),
            Some("https://huggingface.co/a/b/resolve/main/other.json")
        );
        assert_eq!(
            resolve_location("http://127.0.0.1:9/x", "//cdn/y").as_deref(),
            Some("http://cdn/y")
        );
        assert_eq!(
            origin_of("http://127.0.0.1:8080/a/b"),
            Some(("http://127.0.0.1:8080", "127.0.0.1:8080"))
        );
    }

    #[test]
    fn etags() {
        let hex = "a".repeat(64);
        assert_eq!(
            etag_hash(&format!("\"{hex}\"")),
            Some(Stated::Sha256(hex.clone()))
        );
        assert_eq!(
            etag_hash(&format!("W/\"{hex}\"")),
            Some(Stated::Sha256(hex))
        );
        let sha1 = "b".repeat(40);
        assert_eq!(
            etag_hash(&format!("\"{sha1}\"")),
            Some(Stated::GitSha1(sha1))
        );
        assert_eq!(etag_hash("\"0123abcd\""), None);
        assert_eq!(etag_hash(&format!("\"{}\"", "z".repeat(64))), None);
    }

    #[test]
    fn licence_files_and_warnings() {
        let siblings: Vec<String> = ["LICENSE", "NOTICE.txt", "sub/LICENSE", "README.md", ".x"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            licence_files(&siblings),
            vec![
                ("LICENSE".to_string(), ModelRole::License),
                ("NOTICE.txt".to_string(), ModelRole::Notice)
            ]
        );
        assert!(model_licence_warnings("a/b", "mit", false).is_empty());
        assert_eq!(model_licence_warnings("a/b", "unknown", true).len(), 2);
        assert!(model_licence_warnings("a/b", "cc-by-nc-4.0", false)[0].contains("NON-COMMERCIAL"));
        assert!(model_licence_warnings("a/b", "other", false)[0].contains("custom terms"));
    }

    #[test]
    fn the_token_is_not_in_the_debug_text() {
        let request = ModelDownload::new("a/b").with_token("secret-token");
        assert!(!format!("{request:?}").contains("secret-token"));
    }
}
