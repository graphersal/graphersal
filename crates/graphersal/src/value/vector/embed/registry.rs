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
//! process memory ceiling is checked with the size the definition records (`blob length +
//! load_bytes`): nothing is allocated when it does not fit. Two positions (or graphs) using the
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
    /// The blob's bytes, checked by length and CRC-32C.
    #[cfg_attr(
        not(all(feature = "model2vec", not(target_arch = "wasm32"))),
        allow(dead_code)
    )]
    fn read(&self, blob: &BlobRef) -> Result<Vec<u8>, LoadFailure>;
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
    bytes: u64,
    repo: String,
    identity: String,
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

/// The memory check of a load: `blob length + load_bytes` against the process ceiling, with
/// the loaded models counted when no tracking allocator does.
#[cfg_attr(
    not(all(feature = "model2vec", not(target_arch = "wasm32"))),
    allow(dead_code)
)]
pub(crate) fn check_room(model: &ModelDefinition) -> Result<(), GraphError> {
    let needed = model.blob().length().saturating_add(model.load_bytes());
    crate::traversal::memory_budget::ensure_room(MODEL_LOAD_OPERATION, needed, loaded_model_bytes())
}

#[cfg(all(feature = "model2vec", not(target_arch = "wasm32")))]
fn load_uncached(
    identity: &str,
    model: &ModelDefinition,
    source: &Arc<dyn ModelBytes>,
) -> Result<LoadedModel, LoadFailure> {
    check_room(model).map_err(LoadFailure::does_not_fit)?;
    let started = web_time::Instant::now();
    let bytes = source.read(model.blob())?;
    let tar = crate::persist::model_tar::read_tar(&bytes)
        .map_err(|reason| LoadFailure::Damaged(format!("its tar is invalid: {reason}")))?;
    let file = |role: &str| {
        tar.file_of(role)
            .ok_or_else(|| LoadFailure::Invalid(format!("it has no {role} file")))
    };
    let (config, tokenizer, weights) = (file("config")?, file("tokenizer")?, file("model")?);
    let options = super::Model2VecOptions::default().with_repo(model.repo());
    let loaded = super::Model2Vec::load_bytes(
        CompactString::from(model.repo()),
        config,
        tokenizer,
        weights,
        &options,
        &format!("the model {} of the store", model.repo()),
    )
    .map_err(|err| LoadFailure::Invalid(err.to_string()))?;
    if loaded.info().model != identity {
        return Err(LoadFailure::Invalid(format!(
            "its files are the model {}, not {identity}",
            loaded.info().model
        )));
    }
    drop(bytes);
    let memory = Embedder::memory_bytes(&loaded).unwrap_or(model.load_bytes());
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
        repo: model.repo().to_string(),
        identity: identity.to_string(),
        source: Arc::downgrade(source),
    })
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

/// The commands that bring a model back.
pub(crate) fn recommendation(repo: &str) -> String {
    format!(
        "graphersal embed-model download {repo}, then graphersal embed-model import {repo} \
         --into <store> --replace; or restore the store from a backup or replica"
    )
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
    /// The definition's `blob length + load_bytes`.
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
                needed: model.map_or(0, |m| m.blob().length().saturating_add(m.load_bytes())),
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
