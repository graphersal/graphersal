//! Embedders (crate-internal): the abstraction that turns texts into [`Vector`]s
//! ([`Embedder`]), its description ([`EmbedderInfo`]), the checks of a running call
//! ([`EmbedContext`]), its errors ([`EmbedError`], public) and the registry of a graph
//! ([`Embedders`]). The built-in embedder `hash-ngram/1` is [`HashNgram`] (`hash_ngram.rs`); every
//! other embedder is a model kept in the graph's database (`registry.rs`, book page "Embedding
//! Models in a Store"). Book page "Vectors", section "Embeddings".
//!
//! Only the built-in and the database's models run (0.5.0 removed the host registry), and what
//! they compute is stored as a plain vector value, so replay, recovery, change sets and loads
//! never call an embedder.

mod hash_ngram;
pub(crate) mod provenance;
pub(crate) mod registry;

pub(crate) mod identity;
#[cfg(all(feature = "model2vec", not(target_arch = "wasm32")))]
mod model2vec;
// The model tar (format spec 4.5): written and verified by a Store (`persist`), read by the
// model load (`model2vec`), so it lives beside the models rather than in `persist` (issue #24).
#[cfg(any(
    feature = "persist",
    all(feature = "model2vec", not(target_arch = "wasm32"))
))]
pub(crate) mod model_tar;
pub(crate) mod sha256;
#[cfg(all(test, feature = "script"))]
mod tests;

pub(crate) use hash_ngram::HashNgram;
#[cfg(all(feature = "model2vec", not(target_arch = "wasm32")))]
pub(crate) use model2vec::{Model2Vec, Model2VecOptions};

use std::fmt;
use std::sync::{Arc, OnceLock};

use compact_str::CompactString;

use crate::graph::note_text::NoteText;

use super::{MAX_VECTOR_DIMENSION, Vector, VectorElement, VectorMetric};

/// The longest embedder name, in bytes.
pub(crate) const MAX_EMBEDDER_NAME_BYTES: usize = 128;

/// The texts per call an embedder takes when its [`EmbedderInfo`] says nothing else.
pub(crate) const DEFAULT_MAX_BATCH: usize = 64;

/// Turns texts into vectors: the built-in [`HashNgram`] and the models of a graph's database
/// (`registry.rs`). The call is synchronous and takes a batch of up to
/// [`EmbedderInfo::max_batch`] texts; an implementation is shared between threads.
pub(crate) trait Embedder: Send + Sync + 'static {
    /// Name, shape and properties of the embedder. Constant for the embedder's lifetime (an
    /// embedder that learns its dimension from a model file builds its info when it loads it).
    fn info(&self) -> &EmbedderInfo;

    /// Exactly one vector per text, in the same order, each of [`EmbedderInfo::dimension`]
    /// components of [`EmbedderInfo::element`]. The caller checks this
    /// ([`EmbedError::BadOutput`]). A long-running implementation calls
    /// [`EmbedContext::check`] between pieces of work, so a cancelled or timed-out execution
    /// stops.
    fn embed(&self, texts: &[&str], ctx: &EmbedContext<'_>) -> Result<Vec<Vector>, EmbedError>;

    /// The bytes this embedder keeps in memory (a loaded model's weights and tokenizer), as far
    /// as it knows them, or `None` (the default: a remote provider, a function without state).
    /// An estimate for a host's "process memory" report, never used for a decision; it may be
    /// called at any time and must be cheap.
    #[allow(dead_code)]
    fn memory_bytes(&self) -> Option<u64> {
        None
    }
}

/// The description of an [`Embedder`]: its registry name, the shape of its vectors and how it
/// behaves. Built at runtime ([`Self::new`] and the `with_*` builders), so a host can construct
/// it after loading a model whose dimension it learns only then.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub(crate) struct EmbedderInfo {
    /// The registry name (`"hash-ngram/1"`, `"acme/minilm"`): letters, digits and `.`, `_`, `-`,
    /// `/`, `:`, at most [`MAX_EMBEDDER_NAME_BYTES`] bytes; `hash-ngram` and `hash-ngram/*` are
    /// reserved for the built-in embedder.
    pub(crate) name: CompactString,
    /// The stable identity of what computes the vectors (`"hash-ngram/1"`,
    /// `"acme/minilm@3f2a9c41b7de"`), independent of the host's registry name and of any file
    /// path: embedders of the same model give comparable vectors, so it is the identity that
    /// records which model produced a property's vectors. Same characters as a name plus `@`,
    /// `?` and `=`, at most [`MAX_EMBEDDER_NAME_BYTES`] bytes. Defaults to the name.
    pub(crate) model: CompactString,
    /// The number of components of every vector, 1 to [`MAX_VECTOR_DIMENSION`].
    pub(crate) dimension: usize,
    /// The element kind of every vector.
    pub(crate) element: VectorElement,
    /// The metric the embedder recommends for comparing its vectors.
    pub(crate) metric: VectorMetric,
    /// The most texts one call takes (at least 1): the `embed` step hands over batches of this
    /// size.
    pub(crate) max_batch: usize,
    /// Whether the same text gives the same bits everywhere, every time.
    pub(crate) deterministic: bool,
    /// Whether the texts never leave the process (no network call, no other process).
    pub(crate) local: bool,
}

impl EmbedderInfo {
    /// The description of an embedder `name` of vectors of `dimension` components of
    /// `element`, with the defaults: metric cosine, [`DEFAULT_MAX_BATCH`] texts per call, not
    /// deterministic, local. Checked when the embedder is registered
    /// ([`EmbeddersBuilder::with`]).
    pub(crate) fn new(
        name: impl Into<CompactString>,
        dimension: usize,
        element: VectorElement,
    ) -> Self {
        let name = name.into();
        EmbedderInfo {
            model: name.clone(),
            name,
            dimension,
            element,
            metric: VectorMetric::Cosine,
            max_batch: DEFAULT_MAX_BATCH,
            deterministic: false,
            local: true,
        }
    }

    /// Sets the model identity ([`Self::model`]; the default is the name).
    pub(crate) fn with_model(mut self, model: impl Into<CompactString>) -> Self {
        self.model = model.into();
        self
    }

    /// Sets the recommended metric.
    pub(crate) fn with_metric(mut self, metric: VectorMetric) -> Self {
        self.metric = metric;
        self
    }

    /// Sets the most texts per call (at least 1).
    pub(crate) fn with_max_batch(mut self, max_batch: usize) -> Self {
        self.max_batch = max_batch;
        self
    }

    /// Sets whether the embedder is deterministic.
    pub(crate) fn with_deterministic(mut self, deterministic: bool) -> Self {
        self.deterministic = deterministic;
        self
    }

    /// Sets whether the texts stay in the process.
    pub(crate) fn with_local(mut self, local: bool) -> Self {
        self.local = local;
        self
    }

    /// The registration check: a valid name, a dimension of 1 to [`MAX_VECTOR_DIMENSION`] and a
    /// batch size of at least 1.
    pub(crate) fn check(&self) -> Result<(), String> {
        check_text("embedder name", &self.name, "")?;
        check_text("model identity", &self.model, "@?=")?;
        if self.dimension == 0 || self.dimension > MAX_VECTOR_DIMENSION {
            return Err(format!(
                "the embedder {} has dimension {}; a vector has 1 to {MAX_VECTOR_DIMENSION} \
                 components",
                quoted(&self.name),
                self.dimension
            ));
        }
        if self.max_batch == 0 {
            return Err(format!(
                "the embedder {} has max_batch 0; it must take at least one text per call",
                quoted(&self.name)
            ));
        }
        Ok(())
    }
}

/// A name as a quoted literal, cut after 64 characters.
fn quoted(name: &str) -> String {
    let cut: String = name.chars().take(64).collect();
    crate::value::literal::rhai_string(&cut)
}

/// Whether `name` is the built-in's or one reserved for a later version of it.
fn is_reserved(name: &str) -> bool {
    name == "hash-ngram" || name.starts_with("hash-ngram/")
}

/// Checks the syntax of an embedder name or model identity (`what`, not the reservation):
/// ASCII letters, digits, `. _ - / :` and the characters of `extra`; not empty; at most
/// [`MAX_EMBEDDER_NAME_BYTES`] bytes.
fn check_text(what: &str, text: &str, extra: &str) -> Result<(), String> {
    if text.is_empty() {
        return Err(format!("an {what} is empty"));
    }
    if text.len() > MAX_EMBEDDER_NAME_BYTES {
        return Err(format!(
            "the {what} {} is longer than {MAX_EMBEDDER_NAME_BYTES} bytes",
            quoted(text)
        ));
    }
    let allowed = |c: char| {
        c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | ':') || extra.contains(c)
    };
    if let Some(bad) = text.chars().find(|c| !allowed(*c)) {
        return Err(format!(
            "the {what} {} holds {bad:?}; it has letters, digits and . _ - / : {extra} only",
            quoted(text)
        ));
    }
    Ok(())
}

/// Checks the syntax of a model identity ([`EmbedderInfo::model`]: what the schema keyword
/// `embedder` holds); the reason of a refusal otherwise.
pub(crate) fn check_model_identity(model: &str) -> Result<(), String> {
    check_text("model identity", model, "@?=")
}

/// The checks of the execution an embedder runs in: its cancellation token and deadline.
///
/// Passed to [`Embedder::embed`]; an implementation calls [`Self::check`] between pieces of
/// work. The `embed` step passes the context of its traversal (`evaluationTimeout`, the host's
/// [`CancelToken`](crate::CancelToken)); a host calling an embedder directly builds one with
/// [`Self::new`] and the `with_*` builders.
#[derive(Clone, Default)]
#[non_exhaustive]
pub(crate) struct EmbedContext<'a> {
    deadline: Option<&'a crate::traversal::context::Deadline>,
    cancel: Option<CancelSource<'a>>,
    until: Option<web_time::Instant>,
}

/// The token an [`EmbedContext`] reads.
#[derive(Clone)]
#[allow(dead_code)]
enum CancelSource<'a> {
    Borrowed(&'a crate::CancelToken),
    Owned(crate::CancelToken),
}

impl CancelSource<'_> {
    fn is_cancelled(&self) -> bool {
        match self {
            CancelSource::Borrowed(token) => token.is_cancelled(),
            CancelSource::Owned(token) => token.is_cancelled(),
        }
    }
}

impl fmt::Debug for EmbedContext<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EmbedContext")
            .field("deadline", &self.deadline.is_some())
            .field("cancel", &self.cancel.is_some())
            .field("timeout", &self.until.is_some())
            .finish()
    }
}

impl EmbedContext<'static> {
    /// A context without any check: [`Self::check`] always passes.
    #[allow(dead_code)]
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

#[allow(dead_code)]
impl<'a> EmbedContext<'a> {
    /// Also stops when `token` is cancelled.
    pub(crate) fn with_cancel(self, token: &'a crate::CancelToken) -> EmbedContext<'a> {
        EmbedContext {
            cancel: Some(CancelSource::Borrowed(token)),
            ..self
        }
    }

    /// Also stops once `timeout` has passed from now.
    pub(crate) fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.until = web_time::Instant::now().checked_add(timeout);
        self
    }

    /// The context of a traversal step: its deadline (which carries the cancellation token).
    pub(crate) fn of_deadline(deadline: Option<&'a crate::traversal::context::Deadline>) -> Self {
        EmbedContext {
            deadline,
            cancel: None,
            until: None,
        }
    }

    /// The context of a script function: the current thread's cancellation token
    /// ([`CancelToken::scope`](crate::CancelToken::scope)), if any.
    #[cfg_attr(not(feature = "script"), allow(dead_code))]
    pub(crate) fn of_current_thread() -> EmbedContext<'static> {
        EmbedContext {
            deadline: None,
            cancel: crate::CancelToken::current().map(CancelSource::Owned),
            until: None,
        }
    }

    /// `Err(EmbedError::Cancelled)` when the execution was cancelled or its deadline passed.
    pub(crate) fn check(&self) -> Result<(), EmbedError> {
        if let Some(deadline) = self.deadline
            && deadline.check().is_err()
        {
            return Err(EmbedError::Cancelled);
        }
        if self.cancel.as_ref().is_some_and(CancelSource::is_cancelled) {
            return Err(EmbedError::Cancelled);
        }
        if let Some(until) = self.until
            && web_time::Instant::now() >= until
        {
            return Err(EmbedError::Cancelled);
        }
        Ok(())
    }
}

/// Why an embedding could not be computed.
///
/// Inside a traversal it is [`TraverserError::Embed`](crate::TraverserError::Embed), located at
/// the `embed` step (or the text query); the traversal rolls back as every failing traversal
/// does. The embedders are the built-in `hash-ngram/1` and the models kept in the graph's
/// database (book page "Embedding Models in a Store"): `g.embedders()` lists them.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum EmbedError {
    /// The name is neither the built-in `hash-ngram/1` nor a model of the graph's database.
    #[error("there is no embedder {name}")]
    #[non_exhaustive]
    UnknownEmbedder {
        /// The name asked for (quoted, cut after 64 characters).
        name: String,
    },
    /// The input (or the property) is not a string.
    #[error("embed() takes a string, not {found}")]
    #[non_exhaustive]
    NotText {
        /// What was found (its type and a preview).
        found: String,
    },
    /// The text at `index` of the batch has nothing to embed (`hash-ngram/1`: an empty text or
    /// one of separators only).
    #[error("the text at position {index} of the batch has nothing to embed (no word)")]
    #[non_exhaustive]
    NoFeatures {
        /// Its position in the batch handed to the embedder.
        index: usize,
    },
    /// The embedder returned the wrong number of vectors, or one of the wrong dimension or
    /// element kind (a bug of the embedder).
    #[error("the embedder returned a wrong answer: {reason}")]
    #[non_exhaustive]
    BadOutput {
        /// What is wrong.
        reason: String,
    },
    /// The embedder cannot run now or could not be set up (a model file that does not load, a
    /// service that is down).
    #[error("the embedder is unavailable: {reason}")]
    #[non_exhaustive]
    Unavailable {
        /// Why.
        reason: String,
    },
    /// The embedder failed.
    #[error("the embedder failed: {reason}")]
    #[non_exhaustive]
    Failed {
        /// Its own message.
        reason: String,
    },
    /// The execution was cancelled or ran past its deadline while it embedded.
    #[error("the embedding was cancelled")]
    Cancelled,
    /// A text query (`nearest("emb", "text", 5)`) needs a schema attached to the graph (mode
    /// `open` or `closed`): it reads which model produced a property's vectors there.
    #[error("{operation} needs a schema attached to the graph (mode open or closed); {found}")]
    #[non_exhaustive]
    SchemaNotAttached {
        /// The operation (`nearest`, `similarity`).
        operation: String,
        /// What the graph has (`it has no schema`, `its schema has mode none`).
        found: String,
    },
    /// A text query on a property whose schema declares no `embedder`, so no model is known
    /// to embed the text with.
    #[error("the schema declares no embedder for the vector property {key}{on}")]
    #[non_exhaustive]
    NoDeclaredEmbedder {
        /// The property key (quoted).
        key: String,
        /// ` of "label"` when one element's label was found without it, else empty.
        on: String,
    },
    /// The elements a text query meets declare different embedder models for its property:
    /// one query text cannot be compared with vectors of two models.
    #[error(
        "the vector property {key} comes from the model {first} and from {second}; one text \
         query cannot search both"
    )]
    #[non_exhaustive]
    ModelConflict {
        /// The property key (quoted).
        key: String,
        /// The first model met and a label declaring it (`"a/1" on "person"`).
        first: String,
        /// The other model and a label declaring it.
        second: String,
    },
    /// The schema declares a model for the property, and no embedder of the graph has it: its
    /// database does not keep that model (an imported model is found by its identity).
    #[error("no model of this database has the identity {model} the schema declares for {key}")]
    #[non_exhaustive]
    ModelNotRegistered {
        /// The declared model identity (quoted).
        model: String,
        /// The property key (quoted).
        key: String,
    },
    /// A text query needs the key form with a top-level property name
    /// (`similarity("emb", "text")`): only a declared property says which model to embed with.
    #[error("a text query needs a top-level property name to search, not {found}")]
    #[non_exhaustive]
    TextQueryKey {
        /// What was given instead (`no key`, `jpath("$.a.emb")`).
        found: String,
    },
}

impl EmbedError {
    /// [`Self::Unavailable`] with `reason`.
    pub(crate) fn unavailable(reason: impl Into<String>) -> Self {
        EmbedError::Unavailable {
            reason: reason.into(),
        }
    }

    /// [`Self::Failed`] with `reason`.
    pub(crate) fn failed(reason: impl Into<String>) -> Self {
        EmbedError::Failed {
            reason: reason.into(),
        }
    }

    /// [`Self::BadOutput`] with `reason`.
    pub(crate) fn bad_output(reason: impl Into<String>) -> Self {
        EmbedError::BadOutput {
            reason: reason.into(),
        }
    }

    /// [`Self::NoFeatures`] for the text at `index` of the batch.
    pub(crate) fn no_features(index: usize) -> Self {
        EmbedError::NoFeatures { index }
    }

    pub(crate) fn unknown(name: &str) -> Self {
        EmbedError::UnknownEmbedder { name: quoted(name) }
    }

    pub(crate) fn not_text(found: impl Into<String>) -> Self {
        EmbedError::NotText {
            found: found.into(),
        }
    }

    /// The help text of this error, in the canonical snake_case DSL style.
    pub fn help(&self) -> String {
        match self {
            EmbedError::UnknownEmbedder { .. } => NoteText::new(
                "g.embedders() lists the embedders of this database: the built-in \
                 \"hash-ngram/1\" and the models imported into it, named by their repository: \
                 g.v().values(\"name\").embed(#{embedder: \"hash-ngram/1\"}). A model enters a \
                 database by an import (Python: store.import_model(\"<repo>\")).",
            )
            .command(
                "import a model into the store",
                "graphersal embed-model import <repo> --into <store>",
            )
            .finish(),
            EmbedError::NotText { .. } => "embed() takes strings only; nothing is converted \
                 implicitly. Convert the value first with as_string(): \
                 g.v().values(\"age\").as_string().embed(), or embed a string property: \
                 g.v().embed(\"name\")."
                .to_string(),
            EmbedError::NoFeatures { .. } => "The text is empty or holds no word (hash-ngram/1 \
                 embeds words of letters and digits). Leave such texts out before embedding: \
                 g.v().has(\"name\", P.neq(\"\")).embed(\"name\")."
                .to_string(),
            EmbedError::BadOutput { .. } => "The embedder broke its contract (one vector per \
                 text, of the dimension and element kind its model states). This is a bug: \
                 report it with the model's repository (g.embedders() names it)."
                .to_string(),
            EmbedError::Unavailable { .. } => NoteText::new(
                "The model cannot run (its blob is missing or damaged, it does not fit under the \
                 memory ceiling, or this build has no model loader). Bring it back: download it \
                 again and import it into the store, or restore the store from a backup. \
                 g.embedders() lists the embedders of this database.",
            )
            .command(
                "download the model's files again",
                "graphersal embed-model download <repo>",
            )
            .command(
                "import them into the store in place of the damaged copy",
                "graphersal embed-model import <repo> --into <store> --replace",
            )
            .finish(),
            EmbedError::Failed { .. } => "The embedder reported the error above; its message \
                 says what went wrong."
                .to_string(),
            EmbedError::Cancelled => "The embedding stopped because the execution was cancelled \
                 or ran past its evaluationTimeout: g.with(\"evaluationTimeout\", 60000)."
                .to_string(),
            EmbedError::SchemaNotAttached { .. } => "The schema records which embedder model \
                 produced a vector property, so a text query needs one \
                 attached (mode open or closed). Attach the structure the graph has: \
                 g.set_schema(g.infer_schema(), SchemaMode.open). Or search with a vector \
                 instead of a text: g.v().nearest(\"emb\", embed(\"graph indexes\"), 5). A \
                 vector rule in the schema declares the model and keeps the vectors filled: \
                 \"emb\": {\"type\": \"array\", \"format\": \"vector\", \"embedder\": \
                 \"hash-ngram/1\", \"from\": \"text\"} (book page \"Vector Rules\")."
                .to_string(),
            EmbedError::NoDeclaredEmbedder { .. } => "A text query embeds its text with the \
                 embedder model the schema declares for the property (its \"embedder\"). \
                 Or pass a vector: g.v().nearest(\"emb\", embed(\"graph indexes\"), 5). The \
                 vector rule declares it and fills the property from its text at every write: \
                 g.patch_schema(#{vertices: #{doc: #{schema: #{type: \"object\", properties: \
                 #{emb: #{type: \"array\", format: \"vector\", embedder: \"hash-ngram/1\", \
                 from: \"text\"}}}}}})."
                .to_string(),
            EmbedError::ModelConflict { .. } => "The labels the query meets store vectors of \
                 different models in this property. Search one label at a time: \
                 g.v().has_label(\"doc\").nearest(\"emb\", \"graph indexes\", 5)."
                .to_string(),
            EmbedError::ModelNotRegistered { .. } => "The schema says which model produced \
                 the property's vectors, and this database does not keep that model \
                 (g.embedders() lists the ones it keeps with their model). Import it: \
                 graphersal embed-model import <repo> --into <store> (Python: \
                 store.import_model(\"<repo>\")). Or pass a vector computed elsewhere: \
                 g.v().nearest(\"emb\", vector([0.1, 0.2]), 5)."
                .to_string(),
            EmbedError::TextQueryKey { .. } => "Name the vector property, so the schema says \
                 which model embeds the text: g.v().similarity(\"emb\", \"graph indexes\"). \
                 With a computed vector every form works: \
                 g.v().values(\"emb\").similarity(embed(\"graph indexes\"))."
                .to_string(),
        }
    }
}

/// Calls `embedder` on `texts` and checks its answer: one vector per text, each of the
/// dimension and element kind of its info.
pub(crate) fn checked_embed(
    embedder: &dyn Embedder,
    texts: &[&str],
    ctx: &EmbedContext<'_>,
) -> Result<Vec<Vector>, EmbedError> {
    let vectors = embedder.embed(texts, ctx)?;
    let info = embedder.info();
    if vectors.len() != texts.len() {
        return Err(EmbedError::bad_output(format!(
            "{} returned {} vectors for {} texts",
            quoted(&info.name),
            vectors.len(),
            texts.len()
        )));
    }
    if let Some((index, vector)) = vectors
        .iter()
        .enumerate()
        .find(|(_, v)| v.dimension() != info.dimension || v.element() != info.element)
    {
        return Err(EmbedError::bad_output(format!(
            "{} returned a {} vector of dimension {} at position {index}, its info states {} \
             and {}",
            quoted(&info.name),
            vector.element().name(),
            vector.dimension(),
            info.element.name(),
            info.dimension
        )));
    }
    Ok(vectors)
}

/// The embedders a host gives its traversals: an immutable name -> embedder map with a default.
/// Shared as `Arc<Embedders>`.
///
/// [`Default`] holds the built-in `hash-ngram/1` ([`HashNgram`]), which is the default; a host
/// adds its own through [`Self::builder`], which starts from that content.
#[derive(Clone)]
pub(crate) struct Embedders {
    /// In registration order; the built-in first.
    entries: Vec<Arc<dyn Embedder>>,
    /// The position of the default in `entries`.
    default: usize,
}

impl fmt::Debug for Embedders {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Embedders")
            .field(
                "names",
                &self
                    .entries
                    .iter()
                    .map(|e| e.info().name.as_str())
                    .collect::<Vec<_>>(),
            )
            .field("default", &self.default_embedder().info().name.as_str())
            .finish()
    }
}

impl Default for Embedders {
    /// The built-in `hash-ngram/1`, the default.
    fn default() -> Self {
        Embedders {
            entries: vec![Arc::new(HashNgram::new())],
            default: 0,
        }
    }
}

impl Embedders {
    /// A builder that starts from the default content (`hash-ngram/1`, the default).
    pub(crate) fn builder() -> EmbeddersBuilder {
        EmbeddersBuilder {
            embedders: Embedders::default(),
        }
    }

    /// The embedder registered as `name`.
    pub(crate) fn get(&self, name: &str) -> Option<&Arc<dyn Embedder>> {
        self.entries.iter().find(|e| e.info().name == name)
    }

    /// The default embedder (the one a step without an `embedder` option calls).
    pub(crate) fn default_embedder(&self) -> &Arc<dyn Embedder> {
        // `default` always indexes `entries`: the builder sets it from a found position.
        &self.entries[self.default.min(self.entries.len().saturating_sub(1))]
    }

    /// The first registered embedder whose [`EmbedderInfo::model`] is `model`.
    pub(crate) fn by_model(&self, model: &str) -> Option<&Arc<dyn Embedder>> {
        self.entries.iter().find(|e| e.info().model == model)
    }

    /// The descriptions of every registered embedder, in registration order (the built-in
    /// first).
    #[allow(dead_code)]
    pub(crate) fn infos(&self) -> impl Iterator<Item = &EmbedderInfo> {
        self.entries.iter().map(|e| e.info())
    }

    /// The embedder `name` asks for, or the default for `None`.
    pub(crate) fn resolve(&self, name: Option<&str>) -> Result<&Arc<dyn Embedder>, EmbedError> {
        match name {
            None => Ok(self.default_embedder()),
            Some(name) => self.get(name).ok_or_else(|| EmbedError::unknown(name)),
        }
    }

    /// The registry of the graph whose lock guard this thread holds (its database's models,
    /// `registry.rs`), else the process-wide default registry (built once: `hash-ngram/1`).
    pub(crate) fn current() -> Arc<Embedders> {
        if let Some(models) = registry::attached() {
            return models;
        }
        Self::default_registry()
    }

    /// The process-wide default registry (built once): `hash-ngram/1` only.
    pub(crate) fn default_registry() -> Arc<Embedders> {
        static DEFAULT: OnceLock<Arc<Embedders>> = OnceLock::new();
        DEFAULT
            .get_or_init(|| Arc::new(Embedders::default()))
            .clone()
    }
}

/// Builds an [`Embedders`] registry ([`Embedders::builder`]).
#[derive(Debug)]
pub(crate) struct EmbeddersBuilder {
    embedders: Embedders,
}

impl EmbeddersBuilder {
    /// Registers `embedder` under its [`EmbedderInfo::name`]. Fails with
    /// [`EmbedError::Registry`] for an invalid info, a name already registered and a reserved
    /// name (`hash-ngram`, `hash-ngram/*`).
    pub(crate) fn with(mut self, embedder: Arc<dyn Embedder>) -> Result<Self, String> {
        let info = embedder.info();
        info.check()?;
        if is_reserved(&info.name) {
            return Err(format!(
                "the embedder name {} is reserved for the built-in hash-ngram embedder",
                quoted(&info.name)
            ));
        }
        if self.embedders.get(&info.name).is_some() {
            return Err(format!(
                "an embedder {} is already registered",
                quoted(&info.name)
            ));
        }
        self.embedders.entries.push(embedder);
        Ok(self)
    }

    /// Makes the registered embedder `name` the default. Fails with [`EmbedError::Registry`]
    /// when no embedder of that name is registered (yet).
    pub(crate) fn default_name(mut self, name: &str) -> Result<Self, String> {
        let Some(position) = self
            .embedders
            .entries
            .iter()
            .position(|e| e.info().name == name)
        else {
            return Err(format!(
                "the default embedder {} is not registered",
                quoted(name)
            ));
        };
        self.embedders.default = position;
        Ok(self)
    }

    /// Whether `name` cannot be registered: taken or reserved.
    #[cfg_attr(
        not(all(
            feature = "persist",
            feature = "model2vec",
            not(target_arch = "wasm32")
        )),
        allow(dead_code)
    )]
    pub(crate) fn is_taken(&self, name: &str) -> bool {
        is_reserved(name) || self.embedders.get(name).is_some()
    }

    /// The registry.
    pub(crate) fn build(self) -> Embedders {
        self.embedders
    }
}
