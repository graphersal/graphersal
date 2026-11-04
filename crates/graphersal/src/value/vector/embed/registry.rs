//! The model registry of a graph whose catalog holds embedding models (catalog kind 5; book
//! page "Embedding Models in a Store"): built from the CURRENT catalog, it holds the built-in
//! `hash-ngram/1` plus one embedder per served model definition, registered under its
//! repository name (and found by its identity through [`Embedders::by_model`]); the definition
//! marked `default` is the default (else `hash-ngram/1` stays the default).
//!
//! A [`Graph`](crate::graph::Graph) holds its registry; every lock guard of the graph makes the
//! registry's [`Embedders`] the thread's (the traversal source reads them when no explicit
//! embedders were passed). The Store rebuilds it after every commit that changes a model
//! definition (a commit hook; the rebuild never loads a model), after a reload, and builds one
//! for every view of a past position.
//!
//! Loading: a model the current schema needs is loaded eagerly at open (so the open can
//! refuse); every other model lazily at the first query that embeds with it. Before a load the
//! process memory ceiling is checked with the blob's length times [`LOAD_FACTOR`]
//! ([`load_room`]): nothing is allocated when it does not fit. The load streams the blob (the
//! whole tar is never held beside the model). Two positions (or graphs) using the
//! same blob share ONE load through a process-wide cache of weak references; when the last
//! holder goes, the model is unloaded (`model.unloaded`). Loading needs the `model2vec`
//! feature on a native target; any other build registers the models but refuses to embed with
//! them (the reason names the build).

// Without `persist` no Store builds a registry: the types stay, unused.
#![cfg_attr(not(feature = "persist"), allow(dead_code))]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};

use compact_str::CompactString;
use parking_lot::{Mutex, RwLock};

use super::{EmbedContext, EmbedError, Embedder, EmbedderInfo, Embedders};
use crate::error::GraphError;
use crate::graph::ChangeSet;
use crate::graph::definition::{Definition, DefinitionKind};
use crate::graph::model_definition::ModelDefinition;
use crate::graph::system_journal::{Event, SystemJournal, catalogue};
use crate::value::BlobRef;
use crate::value::{Vector, VectorElement};

/// The operation a model load's memory check names.
#[cfg_attr(
    not(all(feature = "model2vec", not(target_arch = "wasm32"))),
    allow(dead_code)
)]
pub(crate) const MODEL_LOAD_OPERATION: &str = "model load";

/// Where a registry reads a model's blob (a Store's `blobs/`).
pub(crate) trait ModelBytes: Send + Sync {
    /// The blob's bytes as a stream (the load never holds the whole blob). The stream checks
    /// what it read when it reaches the blob's end (its length and CRC-32C, and the SHA-256
    /// where the source checks it): a damaged blob is an error of the read that ends it, which
    /// [`Self::read_failure`] turns into the source's failure.
    #[cfg_attr(
        not(all(feature = "model2vec", not(target_arch = "wasm32"))),
        allow(dead_code)
    )]
    fn open(&self, blob: &BlobRef) -> Result<Box<dyn std::io::Read + Send + '_>, LoadFailure>;
    /// The failure of an error a stream of [`Self::open`] returned.
    #[cfg_attr(
        not(all(feature = "model2vec", not(target_arch = "wasm32"))),
        allow(dead_code)
    )]
    fn read_failure(&self, err: std::io::Error) -> LoadFailure;
    /// Checks the blob's length and CRC-32C by streaming it (no allocation): a load another
    /// graph made of the same hash is used only when this store's copy is intact too.
    fn check(&self, blob: &BlobRef) -> Result<(), LoadFailure>;
    /// The journal the load events go to.
    fn journal(&self) -> Option<Arc<SystemJournal>>;
}

/// Why a model did not load.
#[derive(Debug, Clone)]
pub(crate) enum LoadFailure {
    /// Its blob is missing.
    Missing,
    /// Its blob is damaged.
    Damaged(String),
    /// Its files do not form a model (a damaged tar, files that do not load, another identity).
    Invalid(String),
    /// It does not fit under the memory ceiling: the refusal's text, the ceiling and the
    /// smallest ceiling that fits.
    DoesNotFit {
        message: String,
        limit: u64,
        minimum: u64,
    },
    /// This build cannot run it.
    Unsupported(String),
}

impl LoadFailure {
    /// The failure of a memory check.
    pub(crate) fn does_not_fit(err: GraphError) -> Self {
        let (limit, minimum) = match &err {
            GraphError::ResourceLimitExceeded { max, observed, .. } => {
                (max.unwrap_or(0), observed.unwrap_or(0))
            }
            _ => (0, 0),
        };
        LoadFailure::DoesNotFit {
            message: err.to_string(),
            limit,
            minimum,
        }
    }

    /// The journal's `cause` field.
    pub(crate) fn cause(&self) -> &'static str {
        match self {
            LoadFailure::Missing => "missing",
            LoadFailure::Damaged(_) => "damaged",
            LoadFailure::Invalid(_) => "invalid",
            LoadFailure::DoesNotFit { .. } => "does_not_fit",
            LoadFailure::Unsupported(_) => "unsupported",
        }
    }

    /// The reason as text.
    pub(crate) fn reason(&self) -> String {
        match self {
            LoadFailure::Missing => "its blob is missing".to_string(),
            LoadFailure::Damaged(reason)
            | LoadFailure::Invalid(reason)
            | LoadFailure::Unsupported(reason) => reason.clone(),
            LoadFailure::DoesNotFit { message, .. } => message.clone(),
        }
    }
}

/// The load state of a model's blob in a registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ModelLoadState {
    Loaded,
    NotLoaded,
    Failed(String),
    Unsupported(String),
}

// ---------------------------------------------------------------------------------------------
// The process-wide cache of loaded models
// ---------------------------------------------------------------------------------------------

/// A loaded model, shared by every registry that uses its blob.
pub(crate) struct LoadedModel {
    #[cfg(all(feature = "model2vec", not(target_arch = "wasm32")))]
    model: super::Model2Vec,
    /// What it holds in memory: counted at its load when a tracking allocator is installed
    /// ([`Self::exact`]), else the loader's estimate.
    bytes: u64,
    /// `bytes` was counted by the tracking allocator (the live bytes the load left on its
    /// thread), not estimated.
    exact: bool,
    repo: String,
    identity: String,
    /// The texts per second a calibration batch ran at right after the load (the time
    /// estimate of a vector fill, book page "Vector Rules").
    texts_per_second: Option<f64>,
    /// Where it was loaded from: its journal gets `model.unloaded`.
    source: Weak<dyn ModelBytes>,
}

impl Drop for LoadedModel {
    fn drop(&mut self) {
        LOADED_BYTES.fetch_sub(self.bytes, Ordering::AcqRel);
        if let Some(journal) = self.source.upgrade().and_then(|source| source.journal()) {
            journal.emit(
                &catalogue::MODEL_UNLOADED,
                Event::new(format!("the model {} was unloaded", self.repo))
                    .field("repo", self.repo.as_str())
                    .field("identity", self.identity.as_str()),
            );
        }
    }
}

/// The bytes of every model loaded in this process (each blob once).
static LOADED_BYTES: AtomicU64 = AtomicU64::new(0);

/// The loaded models by blob hash.
fn cache() -> &'static Mutex<HashMap<Vec<u8>, Weak<LoadedModel>>> {
    static CACHE: OnceLock<Mutex<HashMap<Vec<u8>, Weak<LoadedModel>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The bytes of every embedding model loaded in this process from a catalog (each blob counted
/// once): what the estimated memory figures add when no tracking allocator counts them.
pub(crate) fn loaded_model_bytes() -> u64 {
    LOADED_BYTES.load(Ordering::Acquire)
}

/// The embedding models loaded in this process (each blob once), as memory report components
/// (`embedding_model`, the identity as detail; exact when counted at the load).
pub(crate) fn loaded_models() -> Vec<crate::graph::MemoryComponent> {
    let mut models: Vec<_> = cache()
        .lock()
        .values()
        .filter_map(Weak::upgrade)
        .map(|model| {
            let part = if model.exact {
                crate::graph::MemoryComponent::exact("embedding_model", model.bytes)
            } else {
                crate::graph::MemoryComponent::estimated("embedding_model", model.bytes)
            };
            part.with_detail(model.identity.as_str())
        })
        .collect();
    models.sort_by(|a, b| a.detail.cmp(&b.detail));
    models
}

/// The texts per second the loaded model `identity` measured at its load, if it is loaded in
/// this process.
pub(crate) fn measured_texts_per_second(identity: &str) -> Option<f64> {
    cache()
        .lock()
        .values()
        .filter_map(Weak::upgrade)
        .find(|model| model.identity == identity)
        .and_then(|model| model.texts_per_second)
}

/// Loads the model of `definition` (named `identity`), or takes the load another registry made
/// of the same blob.
fn load(
    identity: &str,
    model: &ModelDefinition,
    source: &Arc<dyn ModelBytes>,
) -> Result<Arc<LoadedModel>, LoadFailure> {
    if let Some(reason) = model.unserved_reason() {
        return Err(LoadFailure::Unsupported(reason));
    }
    let key = model.blob().hash().to_vec();
    let cached = cache().lock().get(&key).and_then(Weak::upgrade);
    if let Some(loaded) = cached {
        source.check(model.blob())?;
        return Ok(loaded);
    }
    let loaded = Arc::new(load_uncached(identity, model, source)?);
    LOADED_BYTES.fetch_add(loaded.bytes, Ordering::AcqRel);
    let mut cache = cache().lock();
    cache.retain(|_, weak| weak.strong_count() > 0);
    if let Some(other) = cache.get(&key).and_then(Weak::upgrade) {
        // Another thread loaded it meanwhile: use that one (ours is dropped).
        return Ok(other);
    }
    cache.insert(key, Arc::downgrade(&loaded));
    Ok(loaded)
}

/// The memory a model takes loaded per byte of its blob, before it was loaded once (user
/// decision 2026-11-04): measured 930.8 MiB in memory for the 506.4 MiB blob of
/// `minishlab/potion-multilingual-128M` (1.84: the f32 embedding matrix plus the tokenizer's
/// in-memory form, many times its `tokenizer.json`), plus a 20 % reserve. The streaming load
/// holds no copy of the blob beside it.
pub(crate) const LOAD_FACTOR: f64 = 2.2;

/// The memory a load of `model` needs, before it is loaded: its blob's length times
/// [`LOAD_FACTOR`]. Nothing about the size is stored: the definition's `load_bytes` (the
/// import's estimate, kept for older versions) is not read.
pub(crate) fn load_room(model: &ModelDefinition) -> u64 {
    let needed = (model.blob().length() as f64 * LOAD_FACTOR).ceil();
    if needed >= u64::MAX as f64 {
        u64::MAX
    } else {
        needed as u64
    }
}

/// The memory check of a load: [`load_room`] against the process ceiling, with the loaded
/// models counted when no tracking allocator does.
#[cfg_attr(
    not(all(feature = "model2vec", not(target_arch = "wasm32"))),
    allow(dead_code)
)]
pub(crate) fn check_room(model: &ModelDefinition) -> Result<(), GraphError> {
    crate::traversal::memory_budget::ensure_room(
        MODEL_LOAD_OPERATION,
        load_room(model),
        loaded_model_bytes(),
    )
}

/// The memory the model `identity` takes, measured at its load (counted by the tracking
/// allocator, else the loader's estimate), when it is loaded in this process.
pub(crate) fn loaded_bytes(identity: &str) -> Option<u64> {
    cache()
        .lock()
        .values()
        .filter_map(Weak::upgrade)
        .find(|model| model.identity == identity)
        .map(|model| model.bytes)
}

#[cfg(all(feature = "model2vec", not(target_arch = "wasm32")))]
fn load_uncached(
    identity: &str,
    model: &ModelDefinition,
    source: &Arc<dyn ModelBytes>,
) -> Result<LoadedModel, LoadFailure> {
    check_room(model).map_err(LoadFailure::does_not_fit)?;
    let started = web_time::Instant::now();
    let options = super::Model2VecOptions::default().with_repo(model.repo());
    // What the model holds is counted once, here, when a tracking allocator is installed: the
    // tokenizer's in-memory form is many times its file (the estimate counts the file).
    let live_before = crate::traversal::memory_budget::thread_live_bytes();
    let loaded = load_streamed(model, source, &options)?;
    if loaded.info().model != identity {
        return Err(LoadFailure::Invalid(format!(
            "its files are the model {}, not {identity}",
            loaded.info().model
        )));
    }
    let counted = crate::traversal::memory_budget::tracked_live_bytes().and_then(|_| {
        u64::try_from(crate::traversal::memory_budget::thread_live_bytes() - live_before).ok()
    });
    let memory = counted
        .filter(|bytes| *bytes > 0)
        .or_else(|| Embedder::memory_bytes(&loaded))
        .unwrap_or_else(|| load_room(model));
    let texts_per_second = calibrate(&loaded);
    let journal = source.journal();
    if let Some(journal) = &journal {
        journal.emit(
            &catalogue::MODEL_LOADED,
            Event::new(format!("the model {} was loaded", model.repo()))
                .field("repo", model.repo())
                .field("identity", identity)
                .count("load_bytes", memory)
                .count("ms", started.elapsed().as_millis() as u64),
        );
    }
    Ok(LoadedModel {
        model: loaded,
        bytes: memory,
        exact: counted.is_some_and(|bytes| bytes > 0),
        repo: model.repo().to_string(),
        identity: identity.to_string(),
        texts_per_second,
        source: Arc::downgrade(source),
    })
}

/// Loads the model of `model` from its blob read as a stream: the tar entry by entry
/// (`model_tar::TarReader`), the weights decoded while they stream (`model2vec::read_weights`),
/// the config and tokenizer files read whole (small), every other file skipped. The blob is
/// read to its end, so the source's own check (length, CRC-32C) runs, BEFORE the tokenizer is
/// parsed: a damaged blob is `Damaged`, never a parse failure. A failure while the blob streams
/// reads the rest first: when the source then reports damage, that is the failure.
#[cfg(all(feature = "model2vec", not(target_arch = "wasm32")))]
fn load_streamed(
    model: &ModelDefinition,
    source: &Arc<dyn ModelBytes>,
    options: &super::Model2VecOptions,
) -> Result<super::Model2Vec, LoadFailure> {
    use super::model_tar::{MAX_MANIFEST_BYTES, MODEL_MANIFEST, TarReader, TarStreamError};
    use super::model2vec::{WeightsError, read_weights};

    let location = format!("the model {} of the store", model.repo());
    let reader = source.open(model.blob())?;
    let mut tar = TarReader::new(reader);
    let invalid_tar =
        |reason: String| LoadFailure::Damaged(format!("its tar is invalid: {reason}"));
    // A failure while streaming: damage of the blob wins over what the bytes looked like.
    let stream_failure = |tar: &mut TarReader<_>, failure: LoadFailure| match tar.drain() {
        Err(err) => source.read_failure(err),
        Ok(()) => failure,
    };
    let tar_error = |tar: &mut TarReader<_>, err: TarStreamError| match err {
        TarStreamError::Read(err) => source.read_failure(err),
        TarStreamError::Invalid(reason) => stream_failure(tar, invalid_tar(reason)),
    };
    // The entries before the manifest (sorted names: `config*.json`, a card, licences) are kept
    // until it names their roles; after it, only the files of the three roles are read.
    let mut early: Vec<(String, Vec<u8>)> = Vec::new();
    let mut manifest: Option<(Vec<u8>, super::model_tar::ModelManifest)> = None;
    let mut config: Option<Vec<u8>> = None;
    let mut tokenizer: Option<Vec<u8>> = None;
    let mut weights: Option<super::model2vec::Weights> = None;
    let role_of = |manifest: &super::model_tar::ModelManifest, name: &str| {
        ["config", "tokenizer", "model"]
            .into_iter()
            .find(|role| manifest.file_of(role).is_some_and(|f| f.name == name))
    };
    let weights_failure = |err: WeightsError| match err {
        WeightsError::Read(err) => source.read_failure(err),
        other @ WeightsError::Invalid(_) => LoadFailure::Invalid(other_reason(other, &location)),
    };
    loop {
        let entry = match tar.next_entry() {
            Ok(entry) => entry,
            Err(err) => return Err(tar_error(&mut tar, err)),
        };
        let Some((name, size)) = entry else {
            break;
        };
        if name == MODEL_MANIFEST {
            let bytes = match tar.read_entry(MAX_MANIFEST_BYTES) {
                Ok(bytes) => bytes,
                Err(err) => return Err(tar_error(&mut tar, err)),
            };
            // Checked against the entries at the end; parsed now for the roles.
            let parsed = match super::model_tar::ModelManifest::parse(&bytes) {
                Ok(parsed) => parsed,
                Err(reason) => return Err(stream_failure(&mut tar, invalid_tar(reason))),
            };
            for (early_name, data) in std::mem::take(&mut early) {
                match role_of(&parsed, &early_name) {
                    Some("config") => config = Some(data),
                    Some("tokenizer") => tokenizer = Some(data),
                    Some(_) => match read_weights(&mut data.as_slice(), data.len() as u64) {
                        Ok(read) => weights = Some(read),
                        Err(err) => return Err(stream_failure(&mut tar, weights_failure(err))),
                    },
                    None => {}
                }
            }
            manifest = Some((bytes, parsed));
            continue;
        }
        let role = match &manifest {
            Some((_, parsed)) => role_of(parsed, &name),
            None => {
                // Before the manifest: kept whole (never a large file in a tar this version
                // writes).
                match tar.read_entry(size) {
                    Ok(data) => early.push((name, data)),
                    Err(err) => return Err(tar_error(&mut tar, err)),
                }
                continue;
            }
        };
        match role {
            Some("model") => match read_weights(&mut tar, size) {
                Ok(read) => weights = Some(read),
                Err(err) => return Err(stream_failure(&mut tar, weights_failure(err))),
            },
            Some(role) => match tar.read_entry(size) {
                Ok(data) if role == "config" => config = Some(data),
                Ok(data) => tokenizer = Some(data),
                Err(err) => return Err(tar_error(&mut tar, err)),
            },
            None => {}
        }
    }
    if let Err(err) = tar.finish() {
        return Err(tar_error(&mut tar, err));
    }
    // The blob was read to its end and checked by its source: what follows is about the
    // model's files, not damage.
    let Some((manifest_bytes, _)) = manifest else {
        return Err(invalid_tar("the archive holds no manifest".to_string()));
    };
    tar.check_manifest(&manifest_bytes).map_err(invalid_tar)?;
    let missing = |role: &str| LoadFailure::Invalid(format!("it has no {role} file"));
    let config = config.ok_or_else(|| missing("config"))?;
    let tokenizer = tokenizer.ok_or_else(|| missing("tokenizer"))?;
    let weights = weights.ok_or_else(|| missing("model"))?;
    super::Model2Vec::load_parts(
        CompactString::from(model.repo()),
        &config,
        &tokenizer,
        weights,
        options,
        &location,
    )
    .map_err(|err| LoadFailure::Invalid(err.to_string()))
}

/// The text of an invalid weights file at `location`.
#[cfg(all(feature = "model2vec", not(target_arch = "wasm32")))]
fn other_reason(err: super::model2vec::WeightsError, location: &str) -> String {
    err.into_embed_error(location).to_string()
}

/// The texts per second of a fixed calibration batch (64 short sentences, a few ms).
#[cfg(all(feature = "model2vec", not(target_arch = "wasm32")))]
fn calibrate(model: &super::Model2Vec) -> Option<f64> {
    const SENTENCE: &str = "A graph database keeps vertices and edges with their properties.";
    let texts = [SENTENCE; 64];
    let started = web_time::Instant::now();
    model.embed(&texts, &super::EmbedContext::new()).ok()?;
    let seconds = started.elapsed().as_secs_f64();
    (seconds > 0.0).then(|| texts.len() as f64 / seconds)
}

#[cfg(not(all(feature = "model2vec", not(target_arch = "wasm32"))))]
fn load_uncached(
    _identity: &str,
    model: &ModelDefinition,
    _source: &Arc<dyn ModelBytes>,
) -> Result<LoadedModel, LoadFailure> {
    Err(LoadFailure::Unsupported(unsupported_build(model.repo())))
}

/// The reason a build without a model loader gives.
pub(crate) fn unsupported_build(repo: &str) -> String {
    format!(
        "this build cannot run the model {repo} (it has no Model2Vec loader: the feature \
         `model2vec` on a native target); text queries with it are unavailable"
    )
}

/// Whether this build can load models.
pub(crate) const CAN_LOAD: bool = cfg!(all(feature = "model2vec", not(target_arch = "wasm32")));

/// The result of [`check_load`].
pub(crate) enum CheckLoad {
    /// It loaded (and was dropped again unless something holds it).
    Loaded,
    /// This build cannot load models: nothing checked.
    NotAvailable,
    /// Not checked (it does not fit now): the reason.
    Skipped(String),
    /// It does not load: the reason.
    Failed(String),
}

/// Loads the model of `definition` once as a check (an import).
pub(crate) fn check_load(source: &Arc<dyn ModelBytes>, definition: &Definition) -> CheckLoad {
    let Some(model) = definition.as_model() else {
        return CheckLoad::Failed("it is not a model definition".into());
    };
    if !CAN_LOAD {
        return CheckLoad::NotAvailable;
    }
    match load(definition.name(), model, source) {
        Ok(_) => CheckLoad::Loaded,
        Err(LoadFailure::DoesNotFit { message, .. }) => {
            CheckLoad::Skipped(format!("the model was not loaded as a check: {message}"))
        }
        Err(LoadFailure::Unsupported(_)) => CheckLoad::NotAvailable,
        Err(failure) => CheckLoad::Failed(failure.reason()),
    }
}

// ---------------------------------------------------------------------------------------------
// The embedder of a stored model
// ---------------------------------------------------------------------------------------------

/// One served model definition as an embedder: loads its model at the first use.
pub(crate) struct StoredModelEmbedder {
    info: EmbedderInfo,
    identity: String,
    model: ModelDefinition,
    source: Arc<dyn ModelBytes>,
    loaded: Mutex<Option<Arc<LoadedModel>>>,
    failed: Mutex<Option<LoadFailure>>,
}

impl std::fmt::Debug for StoredModelEmbedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredModelEmbedder")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

impl StoredModelEmbedder {
    fn new(identity: &str, model: &ModelDefinition, source: Arc<dyn ModelBytes>) -> Option<Self> {
        let dimension = usize::try_from(model.dimension()).ok().filter(|d| *d > 0)?;
        let info = EmbedderInfo::new(model.repo(), dimension, VectorElement::F32)
            .with_model(identity)
            .with_max_batch(256)
            .with_deterministic(false)
            .with_local(true);
        info.check().ok()?;
        Some(StoredModelEmbedder {
            info,
            identity: identity.to_string(),
            model: model.clone(),
            source,
            loaded: Mutex::new(None),
            failed: Mutex::new(None),
        })
    }

    /// The loaded model (loading it now if needed).
    pub(crate) fn ensure_loaded(&self) -> Result<Arc<LoadedModel>, LoadFailure> {
        let mut loaded = self.loaded.lock();
        if let Some(model) = loaded.as_ref() {
            return Ok(model.clone());
        }
        match load(&self.identity, &self.model, &self.source) {
            Ok(model) => {
                *loaded = Some(model.clone());
                *self.failed.lock() = None;
                Ok(model)
            }
            Err(failure) => {
                // A refusal of the ceiling may pass later; every other failure is kept.
                if !matches!(failure, LoadFailure::DoesNotFit { .. }) {
                    *self.failed.lock() = Some(failure.clone());
                }
                Err(failure)
            }
        }
    }

    fn state(&self) -> ModelLoadState {
        if self.loaded.lock().is_some() {
            return ModelLoadState::Loaded;
        }
        match &*self.failed.lock() {
            Some(LoadFailure::Unsupported(reason)) => ModelLoadState::Unsupported(reason.clone()),
            Some(failure) => ModelLoadState::Failed(failure.reason()),
            None => ModelLoadState::NotLoaded,
        }
    }

    fn loaded_bytes(&self) -> u64 {
        self.loaded.lock().as_ref().map_or(0, |m| m.bytes)
    }
}

impl Embedder for StoredModelEmbedder {
    fn info(&self) -> &EmbedderInfo {
        &self.info
    }

    fn memory_bytes(&self) -> Option<u64> {
        self.loaded.lock().as_ref().map(|m| m.bytes)
    }

    fn embed(&self, texts: &[&str], ctx: &EmbedContext<'_>) -> Result<Vec<Vector>, EmbedError> {
        let loaded = self.ensure_loaded().map_err(|failure| {
            if let Some(journal) = self.source.journal()
                && !matches!(failure, LoadFailure::Unsupported(_))
            {
                journal.emit(
                    &catalogue::MODEL_UNAVAILABLE,
                    Event::new(format!(
                        "the model {} is unavailable: {}",
                        self.model.repo(),
                        failure.reason()
                    ))
                    .field("repo", self.model.repo())
                    .field("identity", self.identity.as_str())
                    .field("cause", failure.cause())
                    .field("recommendation", recommendation(self.model.repo())),
                );
            }
            EmbedError::unavailable(format!(
                "the model {} ({}) does not load: {}",
                self.model.repo(),
                self.identity,
                failure.reason()
            ))
        })?;
        #[cfg(all(feature = "model2vec", not(target_arch = "wasm32")))]
        {
            loaded.model.embed(texts, ctx)
        }
        #[cfg(not(all(feature = "model2vec", not(target_arch = "wasm32"))))]
        {
            let _ = (loaded, texts, ctx);
            Err(EmbedError::unavailable(unsupported_build(
                self.model.repo(),
            )))
        }
    }
}

/// The commands that bring a model back: the `recommendation` of the journal's `model.*`
/// records, in the note layout (a headline, then each command on its own line under a
/// `# comment`).
pub(crate) fn recommendation(repo: &str) -> String {
    crate::graph::note_text::NoteText::new(
        "download the model again and import it into the store, or restore the store from a \
         backup or replica",
    )
    .command(
        "download the model's files again",
        &format!("graphersal embed-model download {repo}"),
    )
    .command(
        "import them into the store in place of the damaged copy",
        &format!("graphersal embed-model import {repo} --into <store> --replace"),
    )
    .finish()
}

// ---------------------------------------------------------------------------------------------
// The registry
// ---------------------------------------------------------------------------------------------

/// The models of one graph (see the module docs).
pub(crate) struct ModelRegistry {
    source: Arc<dyn ModelBytes>,
    state: RwLock<RegistryState>,
}

impl std::fmt::Debug for ModelRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelRegistry").finish_non_exhaustive()
    }
}

#[derive(Default)]
struct RegistryState {
    embedders: Arc<Embedders>,
    models: Vec<Arc<StoredModelEmbedder>>,
    /// The model definitions of the current catalog.
    definitions: Vec<Definition>,
}

/// A needed model that did not load at open.
#[derive(Debug)]
pub(crate) struct NeededFailure {
    pub(crate) identity: String,
    pub(crate) repo: Option<String>,
    pub(crate) places: Vec<String>,
    pub(crate) failure: LoadFailure,
    /// The memory a load needs ([`load_room`]).
    pub(crate) needed: u64,
}

impl ModelRegistry {
    /// The registry of the model definitions in `definitions` (other kinds ignored).
    pub(crate) fn new(
        source: Arc<dyn ModelBytes>,
        definitions: impl IntoIterator<Item = Definition>,
    ) -> Arc<Self> {
        let registry = Arc::new(ModelRegistry {
            source,
            state: RwLock::new(RegistryState::default()),
        });
        registry.rebuild(
            definitions
                .into_iter()
                .filter(|d| d.kind() == DefinitionKind::MODEL)
                .collect(),
        );
        registry
    }

    /// A registry serving `embedders` as they are (crate unit tests: a mock embedder attached to
    /// a graph like a database's model).
    #[cfg(test)]
    pub(crate) fn of_embedders(embedders: Embedders) -> Arc<Self> {
        struct NoBytes;
        impl ModelBytes for NoBytes {
            fn open(&self, _: &BlobRef) -> Result<Box<dyn std::io::Read + Send + '_>, LoadFailure> {
                Err(LoadFailure::Missing)
            }
            fn read_failure(&self, err: std::io::Error) -> LoadFailure {
                LoadFailure::Damaged(err.to_string())
            }
            fn check(&self, _: &BlobRef) -> Result<(), LoadFailure> {
                Err(LoadFailure::Missing)
            }
            fn journal(&self) -> Option<Arc<SystemJournal>> {
                None
            }
        }
        let registry = ModelRegistry::new(Arc::new(NoBytes), Vec::new());
        registry.serve_for_test(embedders);
        registry
    }

    /// Serves `embedders` as they are until the next rebuild (crate unit tests).
    #[cfg(test)]
    pub(crate) fn serve_for_test(&self, embedders: Embedders) {
        self.state.write().embedders = Arc::new(embedders);
    }

    /// The embedders the graph's traversals use.
    pub(crate) fn embedders(&self) -> Arc<Embedders> {
        self.state.read().embedders.clone()
    }

    /// Rebuilds the registry for the model definitions `definitions`; an embedder of an
    /// unchanged definition is kept (with its loaded model). Never loads.
    pub(crate) fn rebuild(&self, definitions: Vec<Definition>) {
        let mut state = self.state.write();
        let mut models: Vec<Arc<StoredModelEmbedder>> = Vec::new();
        let mut default_name: Option<String> = None;
        for definition in &definitions {
            let Some(model) = definition.as_model() else {
                continue;
            };
            if !model.served() {
                continue;
            }
            let kept = state.models.iter().find(|m| {
                m.identity == definition.name()
                    && m.model.blob() == model.blob()
                    && m.model.repo() == model.repo()
            });
            let embedder = match kept {
                Some(kept) => Some(kept.clone()),
                None => StoredModelEmbedder::new(definition.name(), model, self.source.clone())
                    .map(Arc::new),
            };
            if let Some(embedder) = embedder {
                if model.is_default() {
                    default_name = Some(embedder.info.name.to_string());
                }
                models.push(embedder);
            }
        }
        let (embedders, registered) = build(&models, default_name.as_deref());
        state.embedders = Arc::new(embedders);
        state.models = registered;
        state.definitions = definitions;
    }

    /// Applies a committed change set: its model definition changes (catalog kind 5) rebuild
    /// the registry (cheap: never loads).
    pub(crate) fn apply(&self, changes: &ChangeSet) {
        let mut touched = false;
        let mut definitions = self.state.read().definitions.clone();
        for mutation in changes.mutations() {
            if let crate::graph::Mutation::SetDefinition {
                kind, name, after, ..
            } = mutation
                && *kind == DefinitionKind::MODEL
            {
                touched = true;
                definitions.retain(|d| d.name() != name);
                if let Some(after) = after {
                    definitions.push((**after).clone());
                }
            }
        }
        if touched {
            self.rebuild(definitions);
        }
    }

    /// Loads every model the schema needs (`needed`: identity and the places naming it), at
    /// open: the failures, each with its places.
    pub(crate) fn load_needed(&self, needed: &[(String, Vec<String>)]) -> Vec<NeededFailure> {
        let state = self.state.read();
        let mut failures = Vec::new();
        for (identity, places) in needed {
            let base = identity.split('?').next().unwrap_or(identity);
            let definition = state.definitions.iter().find(|d| d.name() == base);
            let model = definition.and_then(Definition::as_model);
            let fail = |failure: LoadFailure| NeededFailure {
                identity: base.to_string(),
                repo: model.map(|m| m.repo().to_string()).or_else(|| {
                    super::identity::parse_identity(base)
                        .and_then(|p| p.repo)
                        .map(str::to_string)
                }),
                places: places.clone(),
                failure,
                needed: model.map_or(0, load_room),
            };
            let Some(model) = model else {
                failures.push(fail(LoadFailure::Missing));
                continue;
            };
            if let Some(reason) = model.unserved_reason() {
                failures.push(fail(LoadFailure::Unsupported(reason)));
                continue;
            }
            let Some(embedder) = state.models.iter().find(|m| m.identity == base) else {
                failures.push(fail(LoadFailure::Invalid(
                    "its definition cannot be served (no dimension or an invalid name)".into(),
                )));
                continue;
            };
            if let Err(failure) = embedder.ensure_loaded() {
                failures.push(fail(failure));
            }
        }
        failures
    }

    /// The load state of the model whose blob is `blob`.
    pub(crate) fn state_of(&self, blob: &BlobRef) -> ModelLoadState {
        self.state
            .read()
            .models
            .iter()
            .find(|m| m.model.blob() == blob)
            .map_or(ModelLoadState::NotLoaded, |m| m.state())
    }

    /// The bytes of the models this registry holds loaded.
    pub(crate) fn model_bytes(&self) -> u64 {
        self.state
            .read()
            .models
            .iter()
            .map(|m| m.loaded_bytes())
            .sum()
    }

    /// The commit hook that keeps the registry in step with the catalog.
    pub(crate) fn hook(self: &Arc<Self>) -> Box<dyn crate::graph::CommitHook> {
        Box::new(RegistryHook(Arc::downgrade(self)))
    }
}

/// The registry of `models` (the built-in `hash-ngram/1` included) with `default` as the
/// default: a model whose name cannot be registered is left out (logged).
fn build(
    models: &[Arc<StoredModelEmbedder>],
    default: Option<&str>,
) -> (Embedders, Vec<Arc<StoredModelEmbedder>>) {
    let mut builder = Embedders::builder();
    let mut registered = Vec::new();
    for model in models {
        if builder.is_taken(&model.info.name) {
            tracing::warn!(
                "the model {} is not served: its name {} is taken",
                model.identity,
                model.info.name
            );
            continue;
        }
        builder = match builder.with(model.clone() as Arc<dyn Embedder>) {
            Ok(next) => next,
            Err(err) => {
                tracing::warn!("the model {} is not served: {err}", model.identity);
                return build(&registered, None);
            }
        };
        registered.push(model.clone());
    }
    if let Some(name) = default
        && registered.iter().any(|m| m.info.name == name)
    {
        builder = match builder.default_name(name) {
            Ok(next) => next,
            Err(err) => {
                tracing::warn!("the default model {name} is not served: {err}");
                return build(&registered, None);
            }
        };
    }
    (builder.build(), registered)
}

/// Rebuilds the registry after a commit that changed a model definition (never vetoes).
struct RegistryHook(Weak<ModelRegistry>);

impl crate::graph::CommitHook for RegistryHook {
    fn name(&self) -> &str {
        "model-registry"
    }

    fn before_commit(&mut self, _changes: &ChangeSet) -> Result<(), crate::graph::HookError> {
        Ok(())
    }

    fn after_commit(&mut self, changes: &ChangeSet) {
        if let Some(registry) = self.0.upgrade() {
            registry.apply(changes);
        }
    }
}

thread_local! {
    /// The registry of the graph whose lock guard this thread holds (the innermost one).
    static ATTACHED: std::cell::RefCell<Option<Arc<Embedders>>> =
        const { std::cell::RefCell::new(None) };
}

/// While alive, makes a graph's model registry the embedders of the thread (a lock guard of a
/// graph with models holds one); dropping restores the previous ones.
pub(crate) struct AttachedModels(Option<Arc<Embedders>>);

impl AttachedModels {
    pub(crate) fn new(embedders: Arc<Embedders>) -> Self {
        let previous = ATTACHED
            .try_with(|current| {
                current
                    .try_borrow_mut()
                    .ok()
                    .and_then(|mut current| current.replace(embedders))
            })
            .ok()
            .flatten();
        AttachedModels(previous)
    }
}

impl Drop for AttachedModels {
    fn drop(&mut self) {
        let previous = self.0.take();
        let _ = ATTACHED.try_with(|current| {
            if let Ok(mut current) = current.try_borrow_mut() {
                *current = previous;
            }
        });
    }
}

/// The embedders of the graph whose lock guard this thread holds.
pub(crate) fn attached() -> Option<Arc<Embedders>> {
    ATTACHED
        .try_with(|current| current.try_borrow().ok().and_then(|c| c.clone()))
        .ok()
        .flatten()
}

/// Keeps [`CompactString`] in use for the non-loader build.
#[allow(dead_code)]
fn _name(name: &str) -> CompactString {
    CompactString::from(name)
}

#[cfg(test)]
mod size_tests {
    use super::*;

    #[test]
    fn a_load_asks_for_its_blob_times_the_load_factor() {
        let blob = |length: u64| {
            BlobRef::new(
                crate::value::blob::BlobHashAlgo::Sha256,
                &[7u8; 32],
                length,
                0,
            )
            .unwrap()
        };
        // The measured model: 506.4 MiB blob, 930.8 MiB loaded (1.84); 2.2 keeps a reserve.
        let blob_bytes = (506.4 * 1024.0 * 1024.0) as u64;
        let model = ModelDefinition::new(blob(blob_bytes), "minishlab/x").with_load_bytes(1);
        let room = load_room(&model);
        assert!(room >= (930.8 * 1024.0 * 1024.0) as u64);
        assert_eq!(room, (blob_bytes as f64 * 2.2).ceil() as u64);
        // The definition's stored estimate plays no part.
        let other =
            ModelDefinition::new(blob(blob_bytes), "minishlab/x").with_load_bytes(u64::MAX / 2);
        assert_eq!(load_room(&other), room);
        assert_eq!(load_room(&ModelDefinition::new(blob(0), "a/b")), 0);
    }
}
