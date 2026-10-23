//! The Model2Vec embedder (feature `model2vec`, native only; book page "Embeddings", section
//! "Real models: Model2Vec"): a static embedding model (one vector per token, mean pooling)
//! loaded from a LOCAL directory through `model2vec-rs` and `tokenizers`. The library never
//! downloads anything; the CLI's `graphersal embed-model download <repo>` fetches a model and
//! writes the manifest [`Model2Vec::MANIFEST_FILE`] beside it.
//!
//! Load-time checks make every later lookup safe: the embedding matrix's shape is read from the
//! safetensors header and every token id of the vocabulary (through the optional `mapping`
//! tensor) must name one of its rows. `model2vec-rs` still panics when the tokenizer fails
//! (`expect("tokenization failed")`), so every call into it runs under `catch_unwind` and a
//! panic becomes [`EmbedError::Failed`].
//!
//! The model identity is `model2vec/<repo>@<12 hex>` (`<repo>` from the manifest, or given by
//! the host with [`Model2VecOptions::with_repo`]) or `model2vec@<12 hex>` without one: the first 12 hex digits of a SHA-256 over the three model
//! files (see [`Model2Vec::model_identity`]), never a path, so a copied or moved directory keeps
//! its identity.

use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use compact_str::CompactString;
use model2vec_rs::model::StaticModel;
use safetensors::SafeTensors;
use safetensors::tensor::Dtype;
use serde_json::Value;

use super::sha256::{Sha256, hex};
use super::{EmbedContext, EmbedError, Embedder, EmbedderInfo, MAX_EMBEDDER_NAME_BYTES};
use crate::value::vector::MAX_VECTOR_DIMENSION;
use crate::value::{Vector, VectorElement};

/// The tensor names `model2vec-rs` looks for, in its order.
const EMBEDDING_TENSORS: [&str; 3] = ["embeddings", "0", "embedding.weight"];

/// The texts handed to the model per call inside one [`Embedder::embed`] call: the context is
/// checked between them.
const CHUNK_TEXTS: usize = 64;

/// The help line every load failure ends with.
const DOWNLOAD_HINT: &str = "Download a model with `graphersal embed-model download <repo>` \
     (for example minishlab/potion-base-8M) and pass its directory";

/// A Model2Vec static embedding model loaded from a local directory: tokenize, look up one
/// vector per token, mean-pool (and L2-normalize when the model's config says so).
///
/// ```no_run
/// use std::sync::Arc;
/// use graphersal::vector::{Embedder, Embedders, Model2Vec};
///
/// let model = Model2Vec::load("potion", "models/minishlab/potion-base-8M").unwrap();
/// assert_eq!(model.info().dimension, 256);
/// let embedders = Embedders::builder().with(Arc::new(model)).unwrap().build();
/// # let _ = embedders;
/// ```
///
/// The directory holds one of the layouts `model2vec-rs` reads: `config.json`,
/// `tokenizer.json` and `model.safetensors` (the Model2Vec layout), or
/// `config_sentence_transformers.json` beside them or beside a `0_StaticEmbedding` directory
/// holding them (the sentence-transformers layout). Its [`EmbedderInfo`]: the dimension of the
/// model, f32 components, metric cosine, 256 texts per call, local, and NOT deterministic in the
/// registry's sense: the same build gives the same bits for the same text, but the output
/// depends on the versions of `model2vec-rs` and `tokenizers` it was built with, so two
/// Graphersal builds are not promised the same bits. The model identity is described in the
/// module docs and on [`Self::model_identity`].
pub struct Model2Vec {
    info: EmbedderInfo,
    model: StaticModel,
    max_tokens: usize,
    normalize: bool,
    /// Estimated bytes in memory: the f32 embedding matrix and the tokenizer file.
    memory_bytes: u64,
}

impl fmt::Debug for Model2Vec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Model2Vec")
            .field("info", &self.info)
            .field("max_tokens", &self.max_tokens)
            .field("normalize", &self.normalize)
            .finish_non_exhaustive()
    }
}

/// The options of a [`Model2Vec`] load ([`Model2Vec::options`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model2VecOptions {
    max_tokens: usize,
    normalize: Option<bool>,
    max_batch: usize,
    repo: Option<String>,
}

impl Default for Model2VecOptions {
    fn default() -> Self {
        Model2VecOptions {
            max_tokens: Model2Vec::DEFAULT_MAX_TOKENS,
            normalize: None,
            max_batch: Model2Vec::DEFAULT_MAX_BATCH,
            repo: None,
        }
    }
}

impl Model2VecOptions {
    /// The most tokens of one text that are pooled (at least 1; default
    /// [`Model2Vec::DEFAULT_MAX_TOKENS`]): a longer text is cut. Not part of the model
    /// identity: only texts longer than the limit come out differently.
    pub fn with_max_tokens(mut self, max_tokens: usize) -> Self {
        self.max_tokens = max_tokens.max(1);
        self
    }

    /// Overrides whether the vectors are L2-normalized (default: the `normalize` of the model's
    /// config, `true` when it says nothing). An override that differs from the config becomes
    /// part of the model identity (`?normalize=false`).
    pub fn with_normalize(mut self, normalize: bool) -> Self {
        self.normalize = Some(normalize);
        self
    }

    /// The most texts the `embed` step hands over per call (at least 1; default
    /// [`Model2Vec::DEFAULT_MAX_BATCH`]).
    pub fn with_max_batch(mut self, max_batch: usize) -> Self {
        self.max_batch = max_batch.max(1);
        self
    }

    /// The Hugging Face repository the model files come from (`name` or `owner/name`, e.g.
    /// `minishlab/potion-base-8M`), for a directory the host downloaded itself (without the
    /// [`Model2Vec::MANIFEST_FILE`] that `graphersal embed-model download` writes). The model
    /// identity becomes `model2vec/<repo>@<12 hex>`, the same identity a download of the same
    /// files by the command line gives, so vectors embedded by one front end find their model
    /// in the other. A directory whose manifest names ANOTHER repository fails to load (one of
    /// the two is wrong); the same one is fine.
    pub fn with_repo(mut self, repo: impl Into<String>) -> Self {
        self.repo = Some(repo.into());
        self
    }

    /// Loads the model in `dir` under the registry name `name` with these options.
    ///
    /// # Errors
    ///
    /// [`EmbedError::Unavailable`] when the directory holds no readable model (a missing file,
    /// an unknown layout, a config, tokenizer or safetensors file that does not parse, a
    /// vocabulary id outside the embedding matrix, a dimension above
    /// [`MAX_VECTOR_DIMENSION`], a [`Self::with_repo`] that is no repository name or differs
    /// from the manifest's); the reason names the file and how to get a model. The registry
    /// checks the name when the embedder is registered.
    pub fn load(
        self,
        name: impl Into<CompactString>,
        dir: impl AsRef<Path>,
    ) -> Result<Model2Vec, EmbedError> {
        Model2Vec::load_with(name.into(), dir.as_ref(), &self)
    }
}

/// The three files of a model, by their role.
struct Layout {
    config: PathBuf,
    tokenizer: PathBuf,
    model: PathBuf,
}

/// The files read from a model directory, with their identity.
struct ModelFiles {
    config: Vec<u8>,
    tokenizer: Vec<u8>,
    model: Vec<u8>,
    /// The identity with the config's own normalization.
    identity: String,
}

impl Model2Vec {
    /// The file `graphersal embed-model download` writes into a model directory: the Hugging
    /// Face repository, the revision and the files with their sizes and SHA-256. A load reads
    /// only its `repo`, for the model identity.
    pub const MANIFEST_FILE: &'static str = "graphersal-embed-model.json";

    /// The default most tokens per text (as `model2vec-rs`).
    pub const DEFAULT_MAX_TOKENS: usize = 512;

    /// The default most texts per call.
    pub const DEFAULT_MAX_BATCH: usize = 256;

    /// Loads the model in `dir` under the registry name `name`, with the default options
    /// ([`Self::options`] for others).
    ///
    /// # Errors
    ///
    /// See [`Model2VecOptions::load`].
    pub fn load(name: impl Into<CompactString>, dir: impl AsRef<Path>) -> Result<Self, EmbedError> {
        Model2VecOptions::default().load(name, dir)
    }

    /// The options of a load, starting from the defaults.
    pub fn options() -> Model2VecOptions {
        Model2VecOptions::default()
    }

    /// The model identity of the model in `dir` with its own normalization:
    /// `model2vec/<repo>@<12 hex>` when the directory holds a [`Self::MANIFEST_FILE`] naming the
    /// repository, else `model2vec@<12 hex>`. The hex digits start the SHA-256 of the text
    /// `model2vec-identity/1\n` followed by one line `<role> <size> <sha256 hex>\n` per file,
    /// for the roles `config`, `tokenizer` and `model` in that order. It reads (and hashes) the
    /// files but does not parse them.
    ///
    /// # Errors
    ///
    /// [`EmbedError::Unavailable`] when a file is missing or unreadable, or the manifest is
    /// invalid.
    pub fn model_identity(dir: impl AsRef<Path>) -> Result<String, EmbedError> {
        read_files(dir.as_ref(), None).map(|files| files.identity)
    }

    /// The most tokens of one text that are pooled.
    pub fn max_tokens(&self) -> usize {
        self.max_tokens
    }

    /// Whether the vectors are L2-normalized.
    pub fn normalize(&self) -> bool {
        self.normalize
    }

    fn load_with(
        name: CompactString,
        dir: &Path,
        options: &Model2VecOptions,
    ) -> Result<Self, EmbedError> {
        let files = read_files(dir, options.repo.as_deref())?;
        let config: Value = serde_json::from_slice(&files.config).map_err(|err| {
            unavailable(format!(
                "the config of the model in '{}' is not JSON: {err}",
                dir.display()
            ))
        })?;
        let config_normalize = config
            .get("normalize")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let normalize = options.normalize.unwrap_or(config_normalize);
        let (rows, cols) = check_model(dir, &files.tokenizer, &files.model)?;
        if cols == 0 || cols > MAX_VECTOR_DIMENSION {
            return Err(unavailable(format!(
                "the model in '{}' has dimension {cols}; a vector has 1 to \
                 {MAX_VECTOR_DIMENSION} components",
                dir.display()
            )));
        }
        let loaded = catch_unwind(AssertUnwindSafe(|| {
            StaticModel::from_bytes(
                &files.tokenizer,
                &files.model,
                &files.config,
                Some(normalize),
            )
        }));
        let model = match loaded {
            Ok(Ok(model)) => model,
            Ok(Err(err)) => {
                return Err(unavailable(format!(
                    "the model in '{}' does not load: {err:#}",
                    dir.display()
                )));
            }
            Err(_) => {
                return Err(unavailable(format!(
                    "the model in '{}' does not load (model2vec-rs failed while reading it)",
                    dir.display()
                )));
            }
        };
        let mut identity = files.identity;
        if normalize != config_normalize {
            identity.push_str(if normalize {
                "?normalize=true"
            } else {
                "?normalize=false"
            });
        }
        if identity.len() > MAX_EMBEDDER_NAME_BYTES {
            return Err(unavailable(format!(
                "the model identity {identity:?} is longer than {MAX_EMBEDDER_NAME_BYTES} bytes \
                 (the repository name in {} is too long)",
                Model2Vec::MANIFEST_FILE
            )));
        }
        tracing::debug!(rows, cols, %identity, "loaded a Model2Vec model");
        Ok(Model2Vec {
            info: EmbedderInfo::new(name, cols, VectorElement::F32)
                .with_model(identity)
                .with_max_batch(options.max_batch)
                .with_deterministic(false)
                .with_local(true),
            model,
            max_tokens: options.max_tokens,
            normalize,
            memory_bytes: (rows as u64)
                .saturating_mul(cols as u64)
                .saturating_mul(4)
                .saturating_add(files.tokenizer.len() as u64),
        })
    }

    /// Embeds one chunk of texts; `offset` is the position of its first text in the batch.
    fn embed_chunk(&self, texts: &[&str], offset: usize) -> Result<Vec<Vector>, EmbedError> {
        let owned: Vec<String> = texts.iter().map(|text| (*text).to_string()).collect();
        let encoded = catch_unwind(AssertUnwindSafe(|| {
            self.model
                .encode_with_args(&owned, Some(self.max_tokens), owned.len().max(1))
        }))
        .map_err(|_| EmbedError::failed("the Model2Vec tokenizer failed on the texts"))?;
        if encoded.len() != texts.len() {
            return Err(EmbedError::bad_output(format!(
                "model2vec-rs returned {} vectors for {} texts",
                encoded.len(),
                texts.len()
            )));
        }
        let mut out = Vec::with_capacity(encoded.len());
        for (index, components) in encoded.into_iter().enumerate() {
            // The mean of no token is the zero vector: the text held nothing the vocabulary
            // knows (an empty text, unknown tokens only).
            if components.iter().all(|c| *c == 0.0) {
                return Err(EmbedError::no_features(offset + index));
            }
            out.push(Vector::from_f32(components).map_err(|err| {
                EmbedError::failed(format!("the model gave an invalid vector: {err}"))
            })?);
        }
        Ok(out)
    }
}

impl Embedder for Model2Vec {
    fn info(&self) -> &EmbedderInfo {
        &self.info
    }

    fn memory_bytes(&self) -> Option<u64> {
        Some(self.memory_bytes)
    }

    fn embed(&self, texts: &[&str], ctx: &EmbedContext<'_>) -> Result<Vec<Vector>, EmbedError> {
        let mut out = Vec::with_capacity(texts.len());
        for (chunk_index, chunk) in texts.chunks(CHUNK_TEXTS).enumerate() {
            ctx.check()?;
            out.extend(self.embed_chunk(chunk, chunk_index * CHUNK_TEXTS)?);
        }
        Ok(out)
    }
}

fn unavailable(reason: String) -> EmbedError {
    EmbedError::unavailable(format!("{reason}. {DOWNLOAD_HINT}"))
}

/// Finds the layout of the model in `dir`.
fn resolve(dir: &Path) -> Result<Layout, EmbedError> {
    if !dir.is_dir() {
        return Err(unavailable(format!(
            "the model directory '{}' does not exist",
            dir.display()
        )));
    }
    let sub = dir.join("0_StaticEmbedding");
    let candidates = [
        (dir.join("config.json"), dir.to_path_buf()),
        (
            dir.join("config_sentence_transformers.json"),
            dir.to_path_buf(),
        ),
        (dir.join("config_sentence_transformers.json"), sub),
    ];
    for (config, base) in candidates {
        let tokenizer = base.join("tokenizer.json");
        let model = base.join("model.safetensors");
        if config.is_file() && tokenizer.is_file() && model.is_file() {
            return Ok(Layout {
                config,
                tokenizer,
                model,
            });
        }
    }
    let missing: Vec<&str> = ["config.json", "tokenizer.json", "model.safetensors"]
        .into_iter()
        .filter(|file| !dir.join(file).is_file())
        .collect();
    Err(unavailable(format!(
        "'{}' holds no Model2Vec model: {} missing (a model directory holds config.json, \
         tokenizer.json and model.safetensors, or the sentence-transformers layout with \
         config_sentence_transformers.json)",
        dir.display(),
        missing.join(", ")
    )))
}

/// Reads the three files of the model in `dir` and computes its identity, with the repository
/// `repo` the host names (checked against the manifest's) or else the manifest's. The one place
/// of the identity rule.
fn read_files(dir: &Path, repo: Option<&str>) -> Result<ModelFiles, EmbedError> {
    if let Some(repo) = repo
        && !is_repo_name(repo)
    {
        return Err(unavailable(format!(
            "the model name {repo:?} is not a Hugging Face repository name (name or owner/name, \
             letters, digits, '.', '_' and '-')"
        )));
    }
    let layout = resolve(dir)?;
    let read = |path: &Path| {
        std::fs::read(path)
            .map_err(|err| unavailable(format!("cannot read '{}': {err}", path.display())))
    };
    let config = read(&layout.config)?;
    let tokenizer = read(&layout.tokenizer)?;
    let model = read(&layout.model)?;
    let mut hash = Sha256::new();
    hash.update(b"model2vec-identity/1\n");
    for (role, bytes) in [
        ("config", &config),
        ("tokenizer", &tokenizer),
        ("model", &model),
    ] {
        let mut file_hash = Sha256::new();
        file_hash.update(bytes);
        let line = format!("{role} {} {}\n", bytes.len(), hex(&file_hash.finish()));
        hash.update(line.as_bytes());
    }
    let digest = hex(&hash.finish());
    let short = &digest[..12];
    let repo = match (repo, manifest_repo(dir)?) {
        (Some(given), Some(manifest)) if given != manifest => {
            return Err(unavailable(format!(
                "the model name {given:?} differs from the repository {manifest:?} the manifest \
                 '{}' names; pass the manifest's name or none",
                dir.join(Model2Vec::MANIFEST_FILE).display()
            )));
        }
        (Some(given), _) => Some(given.to_string()),
        (None, manifest) => manifest,
    };
    let identity = match repo {
        Some(repo) => format!("model2vec/{repo}@{short}"),
        None => format!("model2vec@{short}"),
    };
    Ok(ModelFiles {
        config,
        tokenizer,
        model,
        identity,
    })
}

/// The `repo` of the manifest in `dir`, if there is one.
fn manifest_repo(dir: &Path) -> Result<Option<String>, EmbedError> {
    let path = dir.join(Model2Vec::MANIFEST_FILE);
    if !path.is_file() {
        return Ok(None);
    }
    let invalid = |why: String| {
        unavailable(format!(
            "the manifest '{}' is invalid: {why} (download the model again)",
            path.display()
        ))
    };
    let bytes = std::fs::read(&path).map_err(|err| invalid(err.to_string()))?;
    let manifest: Value = serde_json::from_slice(&bytes).map_err(|err| invalid(err.to_string()))?;
    let Some(repo) = manifest.get("repo").and_then(Value::as_str) else {
        return Err(invalid("it has no \"repo\" string".to_string()));
    };
    if !is_repo_name(repo) {
        return Err(invalid(format!(
            "the repo {repo:?} is not a Hugging Face repository name"
        )));
    }
    Ok(Some(repo.to_string()))
}

/// Whether `repo` is a Hugging Face repository name: `name` or `owner/name`, each part of
/// ASCII letters, digits, `.`, `_` and `-`, not starting with `.`.
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

/// Checks the tokenizer against the embedding matrix: every token id of the vocabulary (through
/// the `mapping` tensor when there is one) must name a row. Returns the matrix's rows and
/// columns.
fn check_model(dir: &Path, tokenizer: &[u8], model: &[u8]) -> Result<(usize, usize), EmbedError> {
    let tensors = SafeTensors::deserialize(model).map_err(|err| {
        unavailable(format!(
            "the model.safetensors in '{}' does not parse: {err}",
            dir.display()
        ))
    })?;
    let Some(embeddings) = EMBEDDING_TENSORS
        .iter()
        .find_map(|name| tensors.tensor(name).ok())
    else {
        return Err(unavailable(format!(
            "the model.safetensors in '{}' has no embedding tensor (\"embeddings\")",
            dir.display()
        )));
    };
    let [rows, cols] = embeddings.shape() else {
        return Err(unavailable(format!(
            "the embedding tensor in '{}' is not 2-dimensional",
            dir.display()
        )));
    };
    let (rows, cols) = (*rows, *cols);
    if !matches!(embeddings.dtype(), Dtype::F32 | Dtype::F16 | Dtype::I8) {
        return Err(unavailable(format!(
            "the embedding tensor in '{}' has the element type {:?} (f32, f16 or i8 expected)",
            dir.display(),
            embeddings.dtype()
        )));
    }
    let mapping: Option<Vec<i64>> = match tensors.tensor("mapping") {
        Err(_) => None,
        Ok(view) => {
            let raw = view.data();
            Some(match view.dtype() {
                Dtype::I64 => raw
                    .as_chunks::<8>()
                    .0
                    .iter()
                    .map(|b| i64::from_le_bytes(*b))
                    .collect(),
                Dtype::I32 => raw
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|b| i64::from(i32::from_le_bytes(*b)))
                    .collect(),
                other => {
                    return Err(unavailable(format!(
                        "the mapping tensor in '{}' has the element type {other:?} (i32 or i64 \
                         expected)",
                        dir.display()
                    )));
                }
            })
        }
    };
    let parsed = catch_unwind(AssertUnwindSafe(|| {
        tokenizers::Tokenizer::from_bytes(tokenizer)
    }));
    let tokenizer = match parsed {
        Ok(Ok(tokenizer)) => tokenizer,
        Ok(Err(err)) => {
            return Err(unavailable(format!(
                "the tokenizer.json in '{}' does not load: {err}",
                dir.display()
            )));
        }
        Err(_) => {
            return Err(unavailable(format!(
                "the tokenizer.json in '{}' does not load",
                dir.display()
            )));
        }
    };
    for (token, id) in tokenizer.get_vocab(true) {
        let id = id as usize;
        let row = match mapping.as_ref().and_then(|m| m.get(id)) {
            Some(row) => usize::try_from(*row).ok(),
            None => Some(id),
        };
        if row.is_none_or(|row| row >= rows) {
            return Err(unavailable(format!(
                "the token {token:?} (id {id}) of the tokenizer in '{}' has no row in the \
                 embedding matrix of {rows} rows: tokenizer.json and model.safetensors do not \
                 belong together",
                dir.display()
            )));
        }
    }
    Ok((rows, cols))
}
