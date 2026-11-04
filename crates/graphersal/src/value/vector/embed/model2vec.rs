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
//! A tokenizer with a Unigram model (the potion models) runs on the compact Unigram of
//! `unigram.rs` inside `tokenizers`' own pipeline, and the pooling is this module's
//! transcription of `model2vec-rs`' (`StaticModel` takes nothing but a `tokenizers::Tokenizer`
//! with `tokenizers`' own model, many times larger): the same token ids and the same vector
//! bits. Every other tokenizer runs through `model2vec-rs` as before.
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
use safetensors::tensor::{Dtype, Metadata};
use serde_json::Value;
use tokenizers::{
    DecoderWrapper, NormalizerWrapper, PostProcessorWrapper, PreTokenizerWrapper, TokenizerImpl,
};

use super::identity::{identity_digest, identity_text, is_repo_name};
use super::sha256::{Sha256, hex};
use super::unigram::CompactUnigram;
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

/// A Model2Vec static embedding model (crate-internal: a model of a database is loaded from its
/// blob, `registry.rs`; [`ModelCache::check`](crate::persist::ModelCache::check) loads a
/// downloaded directory as a check): tokenize, look up one vector per token, mean-pool (and
/// L2-normalize when the model's config says so).
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
pub(crate) struct Model2Vec {
    info: EmbedderInfo,
    /// What tokenizes and pools (named `model` as the `model2vec-rs` model it replaces).
    model: Engine,
    max_tokens: usize,
    normalize: bool,
    /// Estimated bytes in memory: the f32 embedding matrix and the tokenizer (its compact
    /// arrays, else its file).
    memory_bytes: u64,
}

/// A `tokenizers` pipeline whose model is the compact Unigram.
type CompactTokenizer = TokenizerImpl<
    CompactUnigram,
    NormalizerWrapper,
    PreTokenizerWrapper,
    PostProcessorWrapper,
    DecoderWrapper,
>;

/// What tokenizes and pools.
enum Engine {
    /// A Unigram tokenizer: the compact model and this module's pooling.
    Compact(Box<CompactModel>),
    /// Any other tokenizer: `model2vec-rs` with `tokenizers`' own model.
    Reference(StaticModel),
}

/// The model of a Unigram tokenizer: what `model2vec-rs`' `StaticModel` holds, with the compact
/// tokenizer.
struct CompactModel {
    tokenizer: CompactTokenizer,
    /// The embedding matrix, row by row.
    embeddings: Vec<f32>,
    cols: usize,
    weights: Option<Vec<f32>>,
    mapping: Option<Vec<usize>>,
    normalize: bool,
}

/// A parsed `tokenizer.json`.
enum ParsedTokenizer {
    Compact(Box<CompactTokenizer>),
    Reference(Box<tokenizers::Tokenizer>),
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
pub(crate) struct Model2VecOptions {
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

#[allow(dead_code)]
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

#[allow(dead_code)]
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

    /// The three files of the model in `dir` that a load reads and the identity covers, in the
    /// order config, tokenizer, model (`config.json` or `config_sentence_transformers.json`,
    /// `tokenizer.json`, `model.safetensors`, in `dir` or its `0_StaticEmbedding` directory):
    /// what `graphersal embed-model import` copies into a Store (format spec 4.5). Nothing is
    /// read.
    ///
    /// # Errors
    ///
    /// [`EmbedError::Unavailable`] when the directory holds no model layout.
    pub fn model_files(dir: impl AsRef<Path>) -> Result<[PathBuf; 3], EmbedError> {
        let layout = resolve(dir.as_ref())?;
        Ok([layout.config, layout.tokenizer, layout.model])
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
        Self::from_files(name, files, options, &format!("'{}'", dir.display()))
    }

    /// Loads a model from its config and tokenizer bytes and its weights read as a stream
    /// ([`read_weights`]; a model a Store keeps, format spec 4.5, read from its tar without
    /// holding the blob): the same checks and identity as a directory load, with `options`'
    /// repository (checked as a repository name) in the identity. `location` names the model in
    /// messages (`the model acme/x of the store`).
    pub(crate) fn load_parts(
        name: CompactString,
        config: &[u8],
        tokenizer: &[u8],
        weights: Weights,
        options: &Model2VecOptions,
        location: &str,
    ) -> Result<Self, EmbedError> {
        let repo = options.repo.as_deref();
        if let Some(repo) = repo
            && !is_repo_name(repo)
        {
            return Err(unavailable(format!(
                "the model name {repo:?} is not a Hugging Face repository name (name or \
                 owner/name, letters, digits, '.', '_' and '-')"
            )));
        }
        let digest = identity_digest([
            (config.len() as u64, &sha256_hex(config)),
            (tokenizer.len() as u64, &sha256_hex(tokenizer)),
            (weights.size, &weights.sha256),
        ]);
        Self::from_parts(
            name,
            config,
            tokenizer,
            weights,
            identity_text(repo, &digest),
            options,
            location,
        )
    }

    /// The load of files read: `location` names them in messages.
    fn from_files(
        name: CompactString,
        files: ModelFiles,
        options: &Model2VecOptions,
        location: &str,
    ) -> Result<Self, EmbedError> {
        let weights = read_weights(&mut files.model.as_slice(), files.model.len() as u64)
            .map_err(|err| err.into_embed_error(location))?;
        Self::from_parts(
            name,
            &files.config,
            &files.tokenizer,
            weights,
            files.identity,
            options,
            location,
        )
    }

    /// The load of the config and tokenizer bytes and the decoded weights, with the identity
    /// computed from the three files. The tokenizer is parsed ONCE: checked against the
    /// embedding matrix, then handed to `model2vec-rs` (`StaticModel::from_owned`, the same
    /// construction its `from_bytes` ends in, so the vectors are the same bits).
    fn from_parts(
        name: CompactString,
        config_bytes: &[u8],
        tokenizer_bytes: &[u8],
        weights: Weights,
        identity: String,
        options: &Model2VecOptions,
        location: &str,
    ) -> Result<Self, EmbedError> {
        let config: Value = serde_json::from_slice(config_bytes).map_err(|err| {
            unavailable(format!(
                "the config of the model in {location} is not JSON: {err}"
            ))
        })?;
        let config_normalize = config
            .get("normalize")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let normalize = options.normalize.unwrap_or(config_normalize);
        let (rows, cols) = (weights.rows, weights.cols);
        let tokenizer = parse_tokenizer(location, tokenizer_bytes)?;
        match &tokenizer {
            ParsedTokenizer::Compact(tokenizer) => {
                check_compact_vocabulary(location, tokenizer, &weights)?
            }
            ParsedTokenizer::Reference(tokenizer) => {
                check_vocabulary(location, tokenizer, &weights)?
            }
        }
        if cols == 0 || cols > MAX_VECTOR_DIMENSION {
            return Err(unavailable(format!(
                "the model in {location} has dimension {cols}; a vector has 1 to \
                 {MAX_VECTOR_DIMENSION} components"
            )));
        }
        let Weights {
            embeddings,
            weights: token_weights,
            mapping,
            ..
        } = weights;
        // `model2vec-rs` reads the mapping `as usize` (its `from_bytes`); the vocabulary check
        // above refused every value that names no row.
        let mapping = mapping.map(|m| m.into_iter().map(|row| row as usize).collect::<Vec<_>>());
        let matrix_bytes = (rows as u64)
            .saturating_mul(cols as u64)
            .saturating_mul(4)
            .saturating_add(token_weights.as_ref().map_or(0, |w| w.len() as u64 * 4))
            .saturating_add(mapping.as_ref().map_or(0, |m| m.len() as u64 * 8));
        let (engine, tokenizer_memory) = match tokenizer {
            ParsedTokenizer::Compact(tokenizer) => {
                if embeddings.len() != rows.saturating_mul(cols) {
                    return Err(unavailable(format!(
                        "the model in {location} does not load: embeddings length {} != rows \
                         {rows} * cols {cols}",
                        embeddings.len()
                    )));
                }
                let tokenizer_memory = tokenizer.get_model().heap_bytes();
                let model = CompactModel {
                    tokenizer: *tokenizer,
                    embeddings,
                    cols,
                    weights: token_weights,
                    mapping,
                    normalize,
                };
                (Engine::Compact(Box::new(model)), tokenizer_memory)
            }
            ParsedTokenizer::Reference(tokenizer) => {
                let loaded = catch_unwind(AssertUnwindSafe(|| {
                    StaticModel::from_owned(
                        *tokenizer,
                        embeddings,
                        rows,
                        cols,
                        normalize,
                        token_weights,
                        mapping,
                    )
                }));
                let model = match loaded {
                    Ok(Ok(model)) => model,
                    Ok(Err(err)) => {
                        return Err(unavailable(format!(
                            "the model in {location} does not load: {err:#}"
                        )));
                    }
                    Err(_) => {
                        return Err(unavailable(format!(
                            "the model in {location} does not load (model2vec-rs failed while \
                             reading it)"
                        )));
                    }
                };
                (Engine::Reference(model), tokenizer_bytes.len() as u64)
            }
        };
        let mut identity = identity;
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
            model: engine,
            max_tokens: options.max_tokens,
            normalize,
            memory_bytes: matrix_bytes.saturating_add(tokenizer_memory),
        })
    }

    /// Whether the tokenizer runs on the compact Unigram model (tests).
    #[cfg(test)]
    fn is_compact(&self) -> bool {
        matches!(self.model, Engine::Compact(_))
    }

    /// The token ids pooled for each text (after the unknown ids are dropped and the list is
    /// cut to `max_tokens`), by the compact model; `None` for another tokenizer (tests).
    #[cfg(test)]
    fn compact_token_ids(&self, texts: &[&str]) -> Option<Vec<Vec<u32>>> {
        match &self.model {
            Engine::Compact(model) => model.token_ids(texts, self.max_tokens).ok(),
            Engine::Reference(_) => None,
        }
    }

    /// Embeds one chunk of texts; `offset` is the position of its first text in the batch.
    fn embed_chunk(&self, texts: &[&str], offset: usize) -> Result<Vec<Vector>, EmbedError> {
        let encoded = match &self.model {
            Engine::Compact(model) => model.encode(texts, self.max_tokens)?,
            Engine::Reference(model) => {
                let owned: Vec<String> = texts.iter().map(|text| (*text).to_string()).collect();
                catch_unwind(AssertUnwindSafe(|| {
                    model.encode_with_args(&owned, Some(self.max_tokens), owned.len().max(1))
                }))
                .map_err(|_| EmbedError::failed("the Model2Vec tokenizer failed on the texts"))?
            }
        };
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
    let digest = identity_digest([
        (config.len() as u64, &sha256_hex(&config)),
        (tokenizer.len() as u64, &sha256_hex(&tokenizer)),
        (model.len() as u64, &sha256_hex(&model)),
    ]);
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
    let identity = identity_text(repo.as_deref(), &digest);
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

/// The SHA-256 hex digest of `bytes`.
fn sha256_hex(bytes: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(bytes);
    hex(&hash.finish())
}

/// The largest safetensors header read (the `safetensors` crate's own limit).
const MAX_SAFETENSORS_HEADER: u64 = 100_000_000;

/// The bytes read per step while the weights stream (a multiple of every element size, so an
/// element never spans two steps).
const WEIGHTS_CHUNK: usize = 1 << 16;

/// The tensors a load takes from a `model.safetensors` read as a stream ([`read_weights`]):
/// the embedding matrix decoded to f32, the optional per-token `weights` and `mapping`, and the
/// file's size and SHA-256 (for the model identity).
pub(crate) struct Weights {
    embeddings: Vec<f32>,
    rows: usize,
    cols: usize,
    weights: Option<Vec<f32>>,
    /// The `mapping` tensor as read (i64 or i32 widened): checked against the rows.
    mapping: Option<Vec<i64>>,
    size: u64,
    sha256: String,
}

/// Why [`read_weights`] failed.
#[derive(Debug)]
pub(crate) enum WeightsError {
    /// The source failed (a read error, or the source's own check: a damaged blob).
    Read(std::io::Error),
    /// The file is not a model's safetensors file: the reason, without the location.
    Invalid(String),
}

impl WeightsError {
    /// The error of a load from `location`.
    pub(crate) fn into_embed_error(self, location: &str) -> EmbedError {
        match self {
            WeightsError::Read(err) => unavailable(format!(
                "the model.safetensors in {location} cannot be read: {err}"
            )),
            WeightsError::Invalid(reason) => unavailable(reason.replace("{location}", location)),
        }
    }
}

/// A reader that hashes what passes through it and counts it.
struct Hashing<'r> {
    inner: &'r mut dyn std::io::Read,
    hash: Sha256,
    read: u64,
}

impl Hashing<'_> {
    /// Reads exactly `buf.len()` bytes: `Invalid` naming the file when it ends first.
    fn fill(&mut self, buf: &mut [u8]) -> Result<(), WeightsError> {
        let mut filled = 0;
        while filled < buf.len() {
            match self.inner.read(&mut buf[filled..]) {
                Ok(0) => {
                    return Err(WeightsError::Invalid(
                        "the model.safetensors in {location} does not parse: it ends early"
                            .to_string(),
                    ));
                }
                Ok(n) => filled += n,
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(WeightsError::Read(err)),
            }
        }
        self.hash.update(buf);
        self.read += buf.len() as u64;
        Ok(())
    }

    /// Reads (and hashes) `n` bytes, handing each step to `each`.
    fn stream(
        &mut self,
        mut n: u64,
        buf: &mut [u8],
        mut each: impl FnMut(&[u8]),
    ) -> Result<(), WeightsError> {
        while n > 0 {
            let take = usize::try_from(n.min(buf.len() as u64)).unwrap_or(buf.len());
            self.fill(&mut buf[..take])?;
            each(&buf[..take]);
            n -= take as u64;
        }
        Ok(())
    }
}

/// What a tensor the load reads becomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Embeddings,
    Weights,
    Mapping,
}

/// Reads a `model.safetensors` of `size` bytes from `reader` as a stream: the header, then the
/// data in order, decoding only the tensors a load uses (the embedding matrix, `weights`,
/// `mapping`) into their final vectors, so the file is never held in memory. The checks are the
/// `safetensors` crate's (header size, offsets contiguous from 0, each tensor's bytes = its
/// shape times its element size, the data ending where the file ends) and the load's (an
/// embedding tensor of two dimensions in f32, f16 or i8; a mapping in i64 or i32; weights in
/// f64, f32 or f16); the decode is `model2vec-rs`'s, so the vectors are the same bits. The whole
/// file is read (and hashed) even past the last tensor used.
pub(crate) fn read_weights(
    reader: &mut dyn std::io::Read,
    size: u64,
) -> Result<Weights, WeightsError> {
    let parse = |why: &str| {
        WeightsError::Invalid(format!(
            "the model.safetensors in {{location}} does not parse: {why}"
        ))
    };
    let mut src = Hashing {
        inner: reader,
        hash: Sha256::new(),
        read: 0,
    };
    let mut head = [0u8; 8];
    if size < 8 {
        return Err(parse("it is shorter than its header"));
    }
    src.fill(&mut head)?;
    let header_len = u64::from_le_bytes(head);
    if header_len > MAX_SAFETENSORS_HEADER || header_len > size - 8 {
        return Err(parse("its header length is invalid"));
    }
    let mut header = vec![0u8; header_len as usize];
    src.fill(&mut header)?;
    let text = std::str::from_utf8(&header).map_err(|_| parse("its header is not UTF-8"))?;
    let metadata: Metadata = serde_json::from_str(text)
        .map_err(|err| parse(&format!("its header is invalid: {err}")))?;
    drop(header);
    let data_len = size - 8 - header_len;
    // The `safetensors` validation: sorted by offsets, contiguous from 0, sizes from shapes.
    let mut tensors: Vec<(String, &safetensors::tensor::TensorInfo)> =
        metadata.tensors().into_iter().collect();
    tensors.sort_by(|a, b| a.1.data_offsets.cmp(&b.1.data_offsets));
    let mut end = 0usize;
    for (name, info) in &tensors {
        let (start, stop) = info.data_offsets;
        if start != end || stop < start {
            return Err(parse(&format!(
                "the offsets of the tensor {name:?} are invalid"
            )));
        }
        end = stop;
        let bytes = info
            .shape
            .iter()
            .try_fold(1usize, |n, d| n.checked_mul(*d))
            .and_then(|n| n.checked_mul(info.dtype.size()));
        if bytes != Some(stop - start) {
            return Err(parse(&format!(
                "the tensor {name:?} does not have the bytes of its shape"
            )));
        }
    }
    if end as u64 != data_len {
        return Err(parse("its data does not end where the file ends"));
    }
    let invalid = |why: String| WeightsError::Invalid(why);
    let Some(embedding) = EMBEDDING_TENSORS
        .iter()
        .find_map(|name| metadata.info(name))
    else {
        return Err(invalid(
            "the model.safetensors in {location} has no embedding tensor (\"embeddings\")"
                .to_string(),
        ));
    };
    let [rows, cols] = embedding.shape[..] else {
        return Err(invalid(
            "the embedding tensor in {location} is not 2-dimensional".to_string(),
        ));
    };
    if !matches!(embedding.dtype, Dtype::F32 | Dtype::F16 | Dtype::I8) {
        return Err(invalid(format!(
            "the embedding tensor in {{location}} has the element type {:?} (f32, f16 or i8 \
             expected)",
            embedding.dtype
        )));
    }
    let mapping_info = metadata.info("mapping");
    if let Some(info) = mapping_info
        && !matches!(info.dtype, Dtype::I64 | Dtype::I32)
    {
        return Err(invalid(format!(
            "the mapping tensor in {{location}} has the element type {:?} (i32 or i64 expected)",
            info.dtype
        )));
    }
    let weights_info = metadata.info("weights");
    if let Some(info) = weights_info
        && !matches!(info.dtype, Dtype::F64 | Dtype::F32 | Dtype::F16)
    {
        return Err(invalid(format!(
            "the model in {{location}} does not load: unsupported weights dtype: {:?}",
            info.dtype
        )));
    }
    // The tensors to decode, in data order.
    let mut wanted: Vec<(Role, &safetensors::tensor::TensorInfo)> =
        vec![(Role::Embeddings, embedding)];
    if let Some(info) = weights_info {
        wanted.push((Role::Weights, info));
    }
    if let Some(info) = mapping_info {
        wanted.push((Role::Mapping, info));
    }
    wanted.sort_by_key(|(_, info)| info.data_offsets);
    let elements = |info: &safetensors::tensor::TensorInfo| {
        (info.data_offsets.1 - info.data_offsets.0) / info.dtype.size().max(1)
    };
    let reserve_f32 = |n: usize| {
        let mut out: Vec<f32> = Vec::new();
        out.try_reserve_exact(n)
            .map_err(|_| parse("a tensor does not fit in memory"))?;
        Ok::<_, WeightsError>(out)
    };
    let mut embeddings = reserve_f32(elements(embedding))?;
    let mut token_weights: Option<Vec<f32>> = None;
    let mut mapping: Option<Vec<i64>> = None;
    let mut buf = vec![0u8; WEIGHTS_CHUNK];
    let mut at = 0u64;
    for (role, info) in wanted {
        let (start, stop) = (info.data_offsets.0 as u64, info.data_offsets.1 as u64);
        src.stream(start - at, &mut buf, |_| {})?;
        let n = stop - start;
        match role {
            Role::Embeddings => {
                let dtype = info.dtype;
                src.stream(n, &mut buf, |chunk| {
                    decode_floats(dtype, chunk, &mut embeddings)
                })?;
            }
            Role::Weights => {
                let mut out = reserve_f32(elements(info))?;
                let dtype = info.dtype;
                src.stream(n, &mut buf, |chunk| decode_floats(dtype, chunk, &mut out))?;
                token_weights = Some(out);
            }
            Role::Mapping => {
                let mut out: Vec<i64> = Vec::new();
                out.try_reserve_exact(elements(info))
                    .map_err(|_| parse("a tensor does not fit in memory"))?;
                let dtype = info.dtype;
                src.stream(n, &mut buf, |chunk| match dtype {
                    Dtype::I64 => out.extend(
                        chunk
                            .as_chunks::<8>()
                            .0
                            .iter()
                            .map(|b| i64::from_le_bytes(*b)),
                    ),
                    _ => out.extend(
                        chunk
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .map(|b| i64::from(i32::from_le_bytes(*b))),
                    ),
                })?;
                mapping = Some(out);
            }
        }
        at = stop;
    }
    src.stream(data_len - at, &mut buf, |_| {})?;
    debug_assert_eq!(src.read, size);
    Ok(Weights {
        embeddings,
        rows,
        cols,
        weights: token_weights,
        mapping,
        size,
        sha256: hex(&src.hash.finish()),
    })
}

/// Appends the floats of `chunk` (whole elements of `dtype`: f32, f16, i8 or f64) to `out` as
/// `model2vec-rs` decodes them.
fn decode_floats(dtype: Dtype, chunk: &[u8], out: &mut Vec<f32>) {
    match dtype {
        Dtype::F32 => out.extend(
            chunk
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes(*b)),
        ),
        Dtype::F16 => out.extend(
            chunk
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| f16_to_f32(u16::from_le_bytes(*b))),
        ),
        Dtype::F64 => out.extend(
            chunk
                .as_chunks::<8>()
                .0
                .iter()
                .map(|b| f64::from_le_bytes(*b) as f32),
        ),
        // i8 (the only other type the checks let through).
        _ => out.extend(chunk.iter().map(|b| f32::from(*b as i8))),
    }
}

/// An IEEE half-precision value as f32, exactly (every f16 is an f32). NaN keeps its payload
/// and becomes quiet, as the `half` crate `model2vec-rs` uses converts it.
fn f16_to_f32(bits: u16) -> f32 {
    let sign = u32::from(bits & 0x8000) << 16;
    let exponent = u32::from((bits >> 10) & 0x1f);
    let mantissa = u32::from(bits & 0x03ff);
    match (exponent, mantissa) {
        (0, 0) => f32::from_bits(sign),
        (0, _) => {
            // Subnormal: mantissa x 2^-24, exact in f32.
            let value = mantissa as f32 * f32::from_bits(0x3380_0000);
            if sign == 0 { value } else { -value }
        }
        (0x1f, 0) => f32::from_bits(sign | 0x7f80_0000),
        (0x1f, _) => f32::from_bits(sign | 0x7fc0_0000 | (mantissa << 13)),
        _ => f32::from_bits(sign | ((exponent + 112) << 23) | (mantissa << 13)),
    }
}

#[cfg(test)]
impl Engine {
    /// `model2vec-rs`' `StaticModel::encode_with_args` on either engine (tests compare them).
    fn encode_with_args(
        &self,
        sentences: &[String],
        max_length: Option<usize>,
        batch_size: usize,
    ) -> Vec<Vec<f32>> {
        match self {
            Engine::Reference(model) => model.encode_with_args(sentences, max_length, batch_size),
            Engine::Compact(model) => {
                let max_tokens = max_length.unwrap_or(usize::MAX);
                let mut out = Vec::new();
                for batch in sentences.chunks(batch_size.max(1)) {
                    let texts: Vec<&str> = batch.iter().map(String::as_str).collect();
                    match model.encode(&texts, max_tokens) {
                        Ok(vectors) => out.extend(vectors),
                        Err(err) => panic!("tokenization failed: {err}"),
                    }
                }
                out
            }
        }
    }
}

impl CompactModel {
    /// The token ids of `texts` as `model2vec-rs`' `encode_with_args` pools them: each text cut
    /// to `max_tokens` times the median token length in characters, the texts encoded as one
    /// batch without special tokens, the unknown id dropped, the ids cut to `max_tokens`.
    fn token_ids(&self, texts: &[&str], max_tokens: usize) -> Result<Vec<Vec<u32>>, EmbedError> {
        let model = self.tokenizer.get_model();
        let median = model.median_token_length();
        let truncated: Vec<&str> = texts
            .iter()
            .map(|text| truncate_chars(text, max_tokens.saturating_mul(median)))
            .collect();
        let encodings = catch_unwind(AssertUnwindSafe(|| {
            self.tokenizer.encode_batch_fast::<&str>(truncated, false)
        }))
        .map_err(|_| EmbedError::failed("the Model2Vec tokenizer failed on the texts"))?
        .map_err(|err| {
            EmbedError::failed(format!(
                "the Model2Vec tokenizer failed on the texts: {err}"
            ))
        })?;
        let unk_id = model.unk_id();
        Ok(encodings
            .into_iter()
            .map(|encoding| {
                let mut ids = encoding.get_ids().to_vec();
                if let Some(unk_id) = unk_id {
                    ids.retain(|id| *id as usize != unk_id);
                }
                ids.truncate(max_tokens);
                ids
            })
            .collect())
    }

    /// The vectors of `texts` (`model2vec-rs`' `encode_with_args` and `pool_ids`, the same
    /// operations in the same order, so the same bits).
    fn encode(&self, texts: &[&str], max_tokens: usize) -> Result<Vec<Vec<f32>>, EmbedError> {
        self.token_ids(texts, max_tokens)?
            .iter()
            .map(|ids| self.pool(ids))
            .collect()
    }

    /// The mean of the (weighted) rows of `ids`, L2-normalized when the model says so.
    fn pool(&self, ids: &[u32]) -> Result<Vec<f32>, EmbedError> {
        let dim = self.cols;
        let mut sum = vec![0.0_f32; dim];
        let mut count = 0usize;
        for &id in ids {
            let token = id as usize;
            let row_index = self
                .mapping
                .as_ref()
                .and_then(|m| m.get(token))
                .copied()
                .unwrap_or(token);
            let scale = self
                .weights
                .as_ref()
                .and_then(|w| w.get(token))
                .copied()
                .unwrap_or(1.0);
            let row = row_index
                .checked_mul(dim)
                .and_then(|start| self.embeddings.get(start..start.checked_add(dim)?))
                .ok_or_else(|| {
                    EmbedError::failed(format!(
                        "the token id {id} has no row in the embedding matrix"
                    ))
                })?;
            for (s, &v) in sum.iter_mut().zip(row.iter()) {
                *s += v * scale;
            }
            count += 1;
        }
        let denom = count.max(1) as f32;
        for x in &mut sum {
            *x /= denom;
        }
        if self.normalize {
            let norm = sum.iter().map(|&v| v * v).sum::<f32>().sqrt().max(1e-12);
            for x in &mut sum {
                *x /= norm;
            }
        }
        Ok(sum)
    }
}

/// `text` cut to its first `chars` characters (`model2vec-rs`' `truncate_str`).
fn truncate_chars(text: &str, chars: usize) -> &str {
    text.char_indices()
        .nth(chars)
        .map_or(text, |(at, _)| text.get(..at).unwrap_or(text))
}

/// Parses a `tokenizer.json` (under `catch_unwind`: `tokenizers` may panic on odd input): with
/// the compact Unigram model when the file declares a Unigram model (`"type": "Unigram"`), else
/// (or when that parse fails) with `tokenizers`' own models.
fn parse_tokenizer(location: &str, bytes: &[u8]) -> Result<ParsedTokenizer, EmbedError> {
    match catch_unwind(AssertUnwindSafe(|| {
        serde_json::from_slice::<CompactTokenizer>(bytes)
    })) {
        Ok(Ok(tokenizer)) => return Ok(ParsedTokenizer::Compact(Box::new(tokenizer))),
        Ok(Err(err)) => {
            tracing::debug!(%err, "the tokenizer runs on tokenizers' own model");
        }
        Err(_) => {
            tracing::debug!("the compact tokenizer parse failed; tokenizers' own model is used");
        }
    }
    match catch_unwind(AssertUnwindSafe(|| {
        tokenizers::Tokenizer::from_bytes(bytes)
    })) {
        Ok(Ok(tokenizer)) => Ok(ParsedTokenizer::Reference(Box::new(tokenizer))),
        Ok(Err(err)) => Err(unavailable(format!(
            "the tokenizer.json in {location} does not load: {err}"
        ))),
        Err(_) => Err(unavailable(format!(
            "the tokenizer.json in {location} does not load"
        ))),
    }
}

/// Checks the tokenizer against the embedding matrix: every token id of the vocabulary (through
/// the `mapping` tensor when there is one) must name a row.
fn check_vocabulary(
    location: &str,
    tokenizer: &tokenizers::Tokenizer,
    weights: &Weights,
) -> Result<(), EmbedError> {
    for (token, id) in tokenizer.get_vocab(true) {
        check_row(location, id, || token, weights)?;
    }
    Ok(())
}

/// [`check_vocabulary`] for the compact tokenizer: the same ids (every distinct token of the
/// model and every added token), without building the vocabulary map.
fn check_compact_vocabulary(
    location: &str,
    tokenizer: &CompactTokenizer,
    weights: &Weights,
) -> Result<(), EmbedError> {
    let token = |id: u32| tokenizer.id_to_token(id).unwrap_or_default();
    for id in tokenizer.get_model().distinct_ids() {
        check_row(location, id, || token(id), weights)?;
    }
    for id in tokenizer.get_added_vocabulary().get_vocab().values() {
        check_row(location, *id, || token(*id), weights)?;
    }
    Ok(())
}

/// Checks that the token id `id` (named by `token` in the message) has a row.
fn check_row(
    location: &str,
    id: u32,
    token: impl FnOnce() -> String,
    weights: &Weights,
) -> Result<(), EmbedError> {
    let rows = weights.rows;
    let id = id as usize;
    let row = match weights.mapping.as_ref().and_then(|m| m.get(id)) {
        Some(row) => usize::try_from(*row).ok(),
        None => Some(id),
    };
    if row.is_none_or(|row| row >= rows) {
        let token = token();
        return Err(unavailable(format!(
            "the token {token:?} (id {id}) of the tokenizer in {location} has no row in the \
             embedding matrix of {rows} rows: tokenizer.json and model.safetensors do not \
             belong together"
        )));
    }
    Ok(())
}

#[cfg(test)]
#[path = "model2vec_tests.rs"]
mod tests;
