//! The download cache of embedding models (native only, `std::fs`): where `graphersal
//! embed-model download` puts a model, and where an import finds one by its repository name
//! (book page "Embedding Models in a Store"). The cache is never a database: a model serves a
//! database only once it is imported into it ([`Store::import_model`](super::Store::import_model),
//! [`Store::import_model_dir`](super::Store::import_model_dir)), so one download feeds any number
//! of databases without the network.

use std::path::{Path, PathBuf};

use super::{ModelSource, PersistError, StoreFailure};

/// What the errors of the cache name.
const OPERATION: &str = "find an embedding model";

/// The download cache of embedding models: a directory with one subdirectory per repository
/// (`<cache>/<owner>/<name>`), each a Model2Vec model with its download manifest
/// [`ModelCache::MANIFEST_FILE`], card and licence files.
///
/// [`ModelCache::new`] finds the user's cache the way the command line does:
/// `$GRAPHERSAL_EMBED_MODELS`, else `%LOCALAPPDATA%\graphersal\embed-models` (Windows), else
/// `$XDG_CACHE_HOME/graphersal/embed-models`, else `~/.cache/graphersal/embed-models`.
///
/// ```rust,no_run
/// use graphersal::persist::{ImportModelOptions, ModelCache, Store};
///
/// let cache = ModelCache::new().unwrap();
/// // A repository downloaded before (`graphersal embed-model download minishlab/potion-base-8M`)
/// // or a model directory.
/// let source = cache.source("minishlab/potion-base-8M").unwrap();
/// let report = Store::import_model_dir("my-store", source, ImportModelOptions::new()).unwrap();
/// println!("imported {}", report.model.identity);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCache {
    dir: PathBuf,
}

impl ModelCache {
    /// The file a download writes into a model directory: the repository, the revision and the
    /// files with their sizes and SHA-256 (and the licence the Hub states).
    pub const MANIFEST_FILE: &'static str = "graphersal-embed-model.json";

    /// The user's cache (see the type's docs for the lookup). Fails with
    /// [`PersistError::Io`] (`NotFound`) when no variable names a place (no home directory).
    pub fn new() -> Result<ModelCache, PersistError> {
        default_dir().map(ModelCache::at).ok_or_else(|| {
            PersistError::io(
                "finding the embedding model cache",
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "no models directory: set GRAPHERSAL_EMBED_MODELS; neither HOME nor \
                     XDG_CACHE_HOME is set",
                ),
            )
        })
    }

    /// The cache in `dir` (the command line's `--dir`).
    pub fn at(dir: impl Into<PathBuf>) -> ModelCache {
        ModelCache { dir: dir.into() }
    }

    /// The cache's directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Where the download of `repo` (`owner/name`) lives in the cache (it may not exist).
    pub fn path_of(&self, repo: &str) -> PathBuf {
        repo.split('/')
            .fold(self.dir.clone(), |path, part| path.join(part))
    }

    /// The model directory `model` names: a repository downloaded into the cache first, else
    /// `model` as a directory. Fails with [`StoreFailure::UnknownModel`] naming where it looked
    /// and the download command.
    pub fn resolve(&self, model: &str) -> Result<PathBuf, PersistError> {
        let mut looked = None;
        if is_repo_name(model) {
            let downloaded = self.path_of(model);
            if downloaded.is_dir() {
                return Ok(downloaded);
            }
            looked = Some(downloaded);
        }
        let path = Path::new(model);
        if path.is_dir() {
            return Ok(path.to_path_buf());
        }
        Err(PersistError::store(
            OPERATION,
            StoreFailure::UnknownModel,
            match looked {
                Some(downloaded) => format!(
                    "'{model}' is neither a downloaded model (looked in {}) nor a directory; \
                     download it first: graphersal embed-model download {model}",
                    downloaded.display()
                ),
                None => format!(
                    "no such directory '{model}', and it is not a Hugging Face repository name \
                     (owner/name)"
                ),
            },
        ))
    }

    /// [`Self::resolve`] as the source of an import ([`ModelSource::Dir`]).
    pub fn source(&self, model: &str) -> Result<ModelSource, PersistError> {
        self.resolve(model).map(ModelSource::Dir)
    }

    /// The model identity of the model in `dir` (`model2vec/<repo>@<12 hex>`, the repository
    /// from its download manifest, else `model2vec@<12 hex>`): its files are read and hashed,
    /// not parsed. Fails with [`StoreFailure::InvalidModel`] when a file is missing or
    /// unreadable.
    #[cfg(feature = "model2vec")]
    pub fn identity(dir: impl AsRef<Path>) -> Result<String, PersistError> {
        crate::value::vector::embed::Model2Vec::model_identity(dir).map_err(invalid)
    }

    /// Loads the model in `dir` once as a check (a download that is not a Model2Vec model fails
    /// here, not at its import) and returns its identity ([`Self::identity`]). Fails with
    /// [`StoreFailure::InvalidModel`] naming the file that does not load.
    #[cfg(feature = "model2vec")]
    pub fn check(dir: impl AsRef<Path>) -> Result<String, PersistError> {
        use crate::value::vector::embed::Embedder;
        let model = crate::value::vector::embed::Model2Vec::load("check", dir).map_err(invalid)?;
        Ok(model.info().model.to_string())
    }
}

/// The refusal of a model directory that does not load.
#[cfg(feature = "model2vec")]
fn invalid(err: crate::value::EmbedError) -> PersistError {
    PersistError::store(OPERATION, StoreFailure::InvalidModel, err.to_string())
}

/// A non-empty environment variable.
fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// The user's cache directory (see [`ModelCache`]).
fn default_dir() -> Option<PathBuf> {
    if let Some(dir) = env("GRAPHERSAL_EMBED_MODELS") {
        return Some(PathBuf::from(dir));
    }
    if cfg!(windows)
        && let Some(local) = env("LOCALAPPDATA")
    {
        return Some(PathBuf::from(local).join("graphersal").join("embed-models"));
    }
    if let Some(cache) = env("XDG_CACHE_HOME").filter(|c| Path::new(c).is_absolute()) {
        return Some(PathBuf::from(cache).join("graphersal").join("embed-models"));
    }
    env("HOME").or_else(|| env("USERPROFILE")).map(|home| {
        PathBuf::from(home)
            .join(".cache")
            .join("graphersal")
            .join("embed-models")
    })
}

/// Whether `repo` is a Hugging Face repository name: `owner/name` (or a bare `name`), each part
/// of ASCII letters, digits, `.`, `_`, `-`, not starting with `.`.
fn is_repo_name(repo: &str) -> bool {
    let part_ok = |part: &str| {
        !part.is_empty()
            && part.len() <= 96
            && !part.starts_with('.')
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    let mut parts = repo.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(name), None, None) => part_ok(name),
        (Some(owner), Some(name), None) => part_ok(owner) && part_ok(name),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repository_is_found_in_the_cache_before_a_directory() {
        let root = std::env::temp_dir().join(format!(
            "graphersal-model-cache-{:032x}",
            crate::value::uuid::new_v7()
        ));
        let cache = ModelCache::at(&root);
        assert_eq!(cache.path_of("acme/tiny"), root.join("acme").join("tiny"));
        let err = cache.resolve("acme/tiny").unwrap_err();
        assert!(
            err.to_string().contains("embed-model download acme/tiny"),
            "{err}"
        );
        std::fs::create_dir_all(root.join("acme").join("tiny")).unwrap();
        assert_eq!(
            cache.resolve("acme/tiny").unwrap(),
            root.join("acme").join("tiny")
        );
        let plain = root.join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert_eq!(cache.resolve(plain.to_str().unwrap()).unwrap(), plain);
        assert!(cache.resolve("no/such/dir").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
