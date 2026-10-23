//! Embedders: the abstraction that turns texts into [`Vector`]s ([`Embedder`]), its description
//! ([`EmbedderInfo`]), the checks of a running call ([`EmbedContext`]), its errors
//! ([`EmbedError`]) and the registry a host gives its traversals ([`Embedders`]). The built-in
//! embedder `hash-ngram/1` is [`HashNgram`] (`hash_ngram.rs`). Book page "Vectors", section
//! "Embeddings".
//!
//! The engine never calls an external model on its own: only embedders a host registered run,
//! and what they compute is stored as a plain vector value, so replay, recovery, change sets and
//! loads never call an embedder.

mod hash_ngram;
mod job;
pub(crate) mod provenance;

#[cfg(all(feature = "model2vec", not(target_arch = "wasm32")))]
mod model2vec;
#[cfg(all(feature = "model2vec", not(target_arch = "wasm32")))]
mod sha256;

pub use hash_ngram::HashNgram;
pub use job::{EmbedJob, EmbedJobError, EmbedReport};
#[cfg(all(feature = "model2vec", not(target_arch = "wasm32")))]
pub use model2vec::{Model2Vec, Model2VecOptions};

use std::cell::RefCell;
use std::fmt;
use std::sync::{Arc, OnceLock};

use compact_str::CompactString;

use super::{MAX_VECTOR_DIMENSION, Vector, VectorElement, VectorMetric};

thread_local! {
    /// The registry of the innermost [`Embedders::scope`] on this thread.
    static CURRENT: RefCell<Option<Arc<Embedders>>> = const { RefCell::new(None) };
}

/// The longest embedder name, in bytes.
pub const MAX_EMBEDDER_NAME_BYTES: usize = 128;

/// The texts per call an embedder takes when its [`EmbedderInfo`] says nothing else.
pub const DEFAULT_MAX_BATCH: usize = 64;

/// Turns texts into vectors.
///
/// Implementations: the built-in [`HashNgram`] and the host's own types (a model loaded from a
/// file, an HTTP client, a function of a binding language). An embedder is registered in an
/// [`Embedders`] registry, which the host hands to its traversals
/// ([`GraphTraversalSource::with_embedders`](crate::GraphTraversalSource::with_embedders), the
/// script entry points); the `embed` step and the DSL function `embed(text)` call it.
///
/// The call is synchronous and takes a batch: the step hands over up to
/// [`EmbedderInfo::max_batch`] texts per call. An implementation is shared between threads
/// (`Send + Sync`, `&self`); one that needs mutable state keeps it behind its own lock.
///
/// ```rust
/// use std::sync::Arc;
/// use graphersal::vector::{EmbedContext, EmbedError, Embedder, EmbedderInfo, Embedders};
/// use graphersal::vector::{Vector, VectorElement};
///
/// /// A toy embedder: the text length and the number of spaces.
/// struct Lengths(EmbedderInfo);
///
/// impl Embedder for Lengths {
///     fn info(&self) -> &EmbedderInfo {
///         &self.0
///     }
///     fn embed(&self, texts: &[&str], ctx: &EmbedContext<'_>) -> Result<Vec<Vector>, EmbedError> {
///         let mut out = Vec::with_capacity(texts.len());
///         for text in texts {
///             ctx.check()?;
///             let spaces = text.matches(' ').count() as f32;
///             out.push(
///                 Vector::from_f32(vec![text.len() as f32 + 1.0, spaces])
///                     .map_err(|err| EmbedError::failed(err.to_string()))?,
///             );
///         }
///         Ok(out)
///     }
/// }
///
/// let lengths = Lengths(EmbedderInfo::new("acme/lengths", 2, VectorElement::F32).with_deterministic(true));
/// let embedders = Embedders::builder().with(Arc::new(lengths)).unwrap().build();
/// assert!(embedders.get("acme/lengths").is_some());
/// assert_eq!(embedders.default_embedder().info().name, "hash-ngram/1");
/// ```
pub trait Embedder: Send + Sync + 'static {
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
    fn memory_bytes(&self) -> Option<u64> {
        None
    }
}

/// The description of an [`Embedder`]: its registry name, the shape of its vectors and how it
/// behaves. Built at runtime ([`Self::new`] and the `with_*` builders), so a host can construct
/// it after loading a model whose dimension it learns only then.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct EmbedderInfo {
    /// The registry name (`"hash-ngram/1"`, `"acme/minilm"`): letters, digits and `.`, `_`, `-`,
    /// `/`, `:`, at most [`MAX_EMBEDDER_NAME_BYTES`] bytes; `hash-ngram` and `hash-ngram/*` are
    /// reserved for the built-in embedder.
    pub name: CompactString,
    /// The stable identity of what computes the vectors (`"hash-ngram/1"`,
    /// `"acme/minilm@3f2a9c41b7de"`), independent of the host's registry name and of any file
    /// path: embedders of the same model give comparable vectors, so it is the identity that
    /// records which model produced a property's vectors. Same characters as a name plus `@`,
    /// `?` and `=`, at most [`MAX_EMBEDDER_NAME_BYTES`] bytes. Defaults to the name.
    pub model: CompactString,
    /// The number of components of every vector, 1 to [`MAX_VECTOR_DIMENSION`].
    pub dimension: usize,
    /// The element kind of every vector.
    pub element: VectorElement,
    /// The metric the embedder recommends for comparing its vectors.
    pub metric: VectorMetric,
    /// The most texts one call takes (at least 1): the `embed` step hands over batches of this
    /// size.
    pub max_batch: usize,
    /// Whether the same text gives the same bits everywhere, every time.
    pub deterministic: bool,
    /// Whether the texts never leave the process (no network call, no other process).
    pub local: bool,
}

impl EmbedderInfo {
    /// The description of an embedder `name` of vectors of `dimension` components of
    /// `element`, with the defaults: metric cosine, [`DEFAULT_MAX_BATCH`] texts per call, not
    /// deterministic, local. Checked when the embedder is registered
    /// ([`EmbeddersBuilder::with`]).
    pub fn new(name: impl Into<CompactString>, dimension: usize, element: VectorElement) -> Self {
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
    pub fn with_model(mut self, model: impl Into<CompactString>) -> Self {
        self.model = model.into();
        self
    }

    /// Sets the recommended metric.
    pub fn with_metric(mut self, metric: VectorMetric) -> Self {
        self.metric = metric;
        self
    }

    /// Sets the most texts per call (at least 1).
    pub fn with_max_batch(mut self, max_batch: usize) -> Self {
        self.max_batch = max_batch;
        self
    }

    /// Sets whether the embedder is deterministic.
    pub fn with_deterministic(mut self, deterministic: bool) -> Self {
        self.deterministic = deterministic;
        self
    }

    /// Sets whether the texts stay in the process.
    pub fn with_local(mut self, local: bool) -> Self {
        self.local = local;
        self
    }

    /// The registration check: a valid name, a dimension of 1 to [`MAX_VECTOR_DIMENSION`] and a
    /// batch size of at least 1.
    fn check(&self) -> Result<(), EmbedError> {
        check_text("embedder name", &self.name, "")?;
        check_text("model identity", &self.model, "@?=")?;
        if self.dimension == 0 || self.dimension > MAX_VECTOR_DIMENSION {
            return Err(EmbedError::registry(format!(
                "the embedder {} has dimension {}; a vector has 1 to {MAX_VECTOR_DIMENSION} \
                 components",
                quoted(&self.name),
                self.dimension
            )));
        }
        if self.max_batch == 0 {
            return Err(EmbedError::registry(format!(
                "the embedder {} has max_batch 0; it must take at least one text per call",
                quoted(&self.name)
            )));
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
fn check_text(what: &str, text: &str, extra: &str) -> Result<(), EmbedError> {
    if text.is_empty() {
        return Err(EmbedError::registry(format!("an {what} is empty")));
    }
    if text.len() > MAX_EMBEDDER_NAME_BYTES {
        return Err(EmbedError::registry(format!(
            "the {what} {} is longer than {MAX_EMBEDDER_NAME_BYTES} bytes",
            quoted(text)
        )));
    }
    let allowed = |c: char| {
        c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | ':') || extra.contains(c)
    };
    if let Some(bad) = text.chars().find(|c| !allowed(*c)) {
        return Err(EmbedError::registry(format!(
            "the {what} {} holds {bad:?}; it has letters, digits and . _ - / : {extra} only",
            quoted(text)
        )));
    }
    Ok(())
}

/// Checks the syntax of a model identity ([`EmbedderInfo::model`]: what the schema keyword
/// `embedder` holds); the reason of a refusal otherwise.
pub(crate) fn check_model_identity(model: &str) -> Result<(), String> {
    check_text("model identity", model, "@?=").map_err(|err| err.to_string())
}

/// The checks of the execution an embedder runs in: its cancellation token and deadline.
///
/// Passed to [`Embedder::embed`]; an implementation calls [`Self::check`] between pieces of
/// work. The `embed` step passes the context of its traversal (`evaluationTimeout`, the host's
/// [`CancelToken`](crate::CancelToken)); a host calling an embedder directly builds one with
/// [`Self::new`] and the `with_*` builders.
#[derive(Clone, Default)]
#[non_exhaustive]
pub struct EmbedContext<'a> {
    deadline: Option<&'a crate::traversal::context::Deadline>,
    cancel: Option<CancelSource<'a>>,
    until: Option<web_time::Instant>,
}

/// The token an [`EmbedContext`] reads.
#[derive(Clone)]
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
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'a> EmbedContext<'a> {
    /// Also stops when `token` is cancelled.
    pub fn with_cancel(self, token: &'a crate::CancelToken) -> EmbedContext<'a> {
        EmbedContext {
            cancel: Some(CancelSource::Borrowed(token)),
            ..self
        }
    }

    /// Also stops once `timeout` has passed from now.
    pub fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
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
    pub fn check(&self) -> Result<(), EmbedError> {
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

/// Why an embedding could not be computed, or an embedder not registered.
///
/// Inside a traversal it is [`TraverserError::Embed`](crate::TraverserError::Embed), located at
/// the `embed` step; the traversal rolls back as every failing traversal does. An embedder's own
/// implementation returns [`Self::Unavailable`], [`Self::Failed`], [`Self::NoFeatures`],
/// [`Self::BadOutput`] or [`Self::Cancelled`] through the constructors ([`Self::unavailable`],
/// [`Self::failed`], ...), also from a fallible constructor of its own (a model file that does
/// not load is [`Self::unavailable`]).
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum EmbedError {
    /// The name is not in the registry.
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
    /// [`EmbedContext::check`] saw the execution cancelled or past its deadline.
    #[error("the embedding was cancelled")]
    Cancelled,
    /// A registry rule was broken: a duplicate or reserved name, an unknown default, an invalid
    /// [`EmbedderInfo`].
    #[error("{reason}")]
    #[non_exhaustive]
    Registry {
        /// The rule and the offending value.
        reason: String,
    },
    /// `embed_property` or a text query (`nearest("emb", "text", 5)`) needs a schema attached
    /// to the graph (mode `open` or `closed`): it records and reads which model produced a
    /// property's vectors there.
    #[error("{operation} needs a schema attached to the graph (mode open or closed); {found}")]
    #[non_exhaustive]
    SchemaNotAttached {
        /// The operation (`embed_property`, `nearest`, `similarity`).
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
    /// The schema declares a model for the property, and no registered embedder has it
    /// ([`Embedders::by_model`]).
    #[error("no registered embedder has the model {model} the schema declares for {key}")]
    #[non_exhaustive]
    ModelNotRegistered {
        /// The declared model identity (quoted).
        model: String,
        /// The property key (quoted).
        key: String,
    },
    /// `embed_property` would write vectors of one model into a property the schema declares
    /// for another.
    #[error("the schema declares {property} as vectors of the model {declared}; {embedder}")]
    #[non_exhaustive]
    EmbedderMismatch {
        /// The label and the property key (`"doc"."emb"`).
        property: String,
        /// The declared model (quoted).
        declared: String,
        /// The embedder asked for and its model (`the embedder "x" has the model "y"`).
        embedder: String,
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
    pub fn unavailable(reason: impl Into<String>) -> Self {
        EmbedError::Unavailable {
            reason: reason.into(),
        }
    }

    /// [`Self::Failed`] with `reason`.
    pub fn failed(reason: impl Into<String>) -> Self {
        EmbedError::Failed {
            reason: reason.into(),
        }
    }

    /// [`Self::BadOutput`] with `reason`.
    pub fn bad_output(reason: impl Into<String>) -> Self {
        EmbedError::BadOutput {
            reason: reason.into(),
        }
    }

    /// [`Self::NoFeatures`] for the text at `index` of the batch.
    pub fn no_features(index: usize) -> Self {
        EmbedError::NoFeatures { index }
    }

    pub(crate) fn registry(reason: impl Into<String>) -> Self {
        EmbedError::Registry {
            reason: reason.into(),
        }
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
            EmbedError::UnknownEmbedder { .. } => "g.embedders() lists the embedders this host \
                 registered (the built-in \"hash-ngram/1\" is always there): \
                 g.v().values(\"name\").embed(#{embedder: \"hash-ngram/1\"}). A host registers \
                 its own through Embedders::builder() and GraphTraversalSource::with_embedders \
                 (Rust) or the session's embedders."
                .to_string(),
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
                 text, of the dimension and element kind its info states). This is a bug of \
                 the embedder's implementation: report it to whoever provides it."
                .to_string(),
            EmbedError::Unavailable { .. } => "The embedder cannot run now. Retry later, or \
                 check the host's configuration of it (its model file, its service); \
                 g.embedders() lists the embedders that are registered."
                .to_string(),
            EmbedError::Failed { .. } => "The embedder reported the error above; its message \
                 says what went wrong."
                .to_string(),
            EmbedError::Cancelled => "The embedding stopped because the execution was cancelled \
                 or ran past its evaluationTimeout: g.with(\"evaluationTimeout\", 60000)."
                .to_string(),
            EmbedError::Registry { .. } => "Each embedder is registered once under a name of \
                 letters, digits and . _ - / : (at most 128 bytes); \"hash-ngram/1\" and every \
                 \"hash-ngram/*\" name belong to the built-in embedder; the default must be a \
                 registered name; an embedder's dimension is 1 to 65536 and it takes at least \
                 one text per call."
                .to_string(),
            EmbedError::SchemaNotAttached { .. } => "The schema records which embedder model \
                 produced a vector property, so embed_property and a text query need one \
                 attached (mode open or closed). Attach the structure the graph has: \
                 g.set_schema(g.infer_schema(), SchemaMode.open). Or search with a vector \
                 instead of a text: g.v().nearest(\"emb\", embed(\"graph indexes\"), 5)."
                .to_string(),
            EmbedError::NoDeclaredEmbedder { .. } => "A text query embeds its text with the \
                 embedder model the schema declares for the property (its \"embedder\"). \
                 embed_property declares it when it fills the property: \
                 g.embed_property(#{label: \"doc\", from: \"text\", to: \"emb\"}). Or pass a \
                 vector: g.v().nearest(\"emb\", embed(\"graph indexes\"), 5)."
                .to_string(),
            EmbedError::ModelConflict { .. } => "The labels the query meets store vectors of \
                 different models in this property. Search one label at a time: \
                 g.v().has_label(\"doc\").nearest(\"emb\", \"graph indexes\", 5)."
                .to_string(),
            EmbedError::ModelNotRegistered { .. } => "The schema says which model produced \
                 the property's vectors, and the host has no embedder of that model \
                 (g.embedders() lists the registered ones with their model). The host \
                 registers it: Embedders::builder().with(..) and \
                 GraphTraversalSource::with_embedders in Rust, the session's embedders, or a \
                 flag of the command line such as --embedder NAME=REPO. Or pass a vector \
                 computed elsewhere: g.v().nearest(\"emb\", vector([0.1, 0.2]), 5)."
                .to_string(),
            EmbedError::EmbedderMismatch { .. } => "Vectors of different models cannot be \
                 compared, so a property holds vectors of one model. Embed into another \
                 property: g.embed_property(#{label: \"doc\", from: \"text\", to: \"emb2\", \
                 embedder: \"acme/minilm\"}), or change the property's \"embedder\" in the \
                 schema deliberately (g.patch_schema(..)) and re-embed every element with \
                 only_missing: false."
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
pub struct Embedders {
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
    pub fn builder() -> EmbeddersBuilder {
        EmbeddersBuilder {
            embedders: Embedders::default(),
        }
    }

    /// The embedder registered as `name`.
    pub fn get(&self, name: &str) -> Option<&Arc<dyn Embedder>> {
        self.entries.iter().find(|e| e.info().name == name)
    }

    /// The default embedder (the one a step without an `embedder` option calls).
    pub fn default_embedder(&self) -> &Arc<dyn Embedder> {
        // `default` always indexes `entries`: the builder sets it from a found position.
        &self.entries[self.default.min(self.entries.len().saturating_sub(1))]
    }

    /// The first registered embedder whose [`EmbedderInfo::model`] is `model`.
    pub fn by_model(&self, model: &str) -> Option<&Arc<dyn Embedder>> {
        self.entries.iter().find(|e| e.info().model == model)
    }

    /// The descriptions of every registered embedder, in registration order (the built-in
    /// first).
    pub fn infos(&self) -> impl Iterator<Item = &EmbedderInfo> {
        self.entries.iter().map(|e| e.info())
    }

    /// The embedder `name` asks for, or the default for `None`.
    pub(crate) fn resolve(&self, name: Option<&str>) -> Result<&Arc<dyn Embedder>, EmbedError> {
        match name {
            None => Ok(self.default_embedder()),
            Some(name) => self.get(name).ok_or_else(|| EmbedError::unknown(name)),
        }
    }

    /// Runs `run` with this registry as the current thread's embedders: a script engine and
    /// scope built by `run` on this thread, and a traversal source run by it, without a
    /// registry of their own
    /// ([`GraphTraversalSource::with_embedders`](crate::GraphTraversalSource::with_embedders),
    /// `script::set_engine_embedders`, `script::set_scope_embedders`, which win) take this
    /// one, so a host gives the script entry points that build their own engine
    /// (`script::eval_value_with_limits`, `eval_value_atomic`, `eval_value_dry_run`, ...) its
    /// embedders without new parameters. A script source keeps the registry it was built with,
    /// also after the scope ends. Scopes nest; the innermost applies, and the previous one is
    /// restored afterwards (also when `run` panics). Like
    /// [`CancelToken::scope`](crate::exec::CancelToken::scope).
    ///
    /// ```rust
    /// # #[cfg(feature = "script")] {
    /// use std::sync::Arc;
    /// use graphersal::prelude::*;
    /// use graphersal::auth::AllowAll;
    /// use graphersal::script::{ScriptLimits, eval_value_with_limits};
    /// use graphersal::vector::{Embedders, HashNgram};
    ///
    /// let small = HashNgram::with_dimension("acme/small", 4).unwrap();
    /// let embedders = Arc::new(
    ///     Embedders::builder().with(Arc::new(small)).unwrap().default_name("acme/small").unwrap().build(),
    /// );
    /// let graph = Arc::new(GraphSource::tinkerpop_modern());
    /// let dimension = embedders.scope(|| {
    ///     eval_value_with_limits(
    ///         graph,
    ///         r#"g.v().embed("name").next().dimension"#,
    ///         Default::default(),
    ///         &ScriptLimits::default(),
    ///         Arc::new(AllowAll),
    ///     )
    /// });
    /// assert_eq!(dimension.unwrap().as_int().unwrap(), 4);
    /// # }
    /// ```
    pub fn scope<R>(self: &Arc<Self>, run: impl FnOnce() -> R) -> R {
        struct Restore(Option<Arc<Embedders>>);
        impl Drop for Restore {
            fn drop(&mut self) {
                let previous = self.0.take();
                // Never panics: `try_with` fails only while the thread is being torn down.
                let _ = CURRENT.try_with(|current| {
                    if let Ok(mut current) = current.try_borrow_mut() {
                        *current = previous;
                    }
                });
            }
        }
        let previous = CURRENT
            .try_with(|current| {
                current
                    .try_borrow_mut()
                    .ok()
                    .and_then(|mut current| current.replace(Arc::clone(self)))
            })
            .ok()
            .flatten();
        let _restore = Restore(previous);
        run()
    }

    /// The registry a source or engine without an explicit one uses: the innermost
    /// [`Self::scope`] of this thread, else the process-wide default registry (built once).
    pub(crate) fn current() -> Arc<Embedders> {
        if let Some(scoped) = CURRENT
            .try_with(|current| current.try_borrow().ok().and_then(|c| c.clone()))
            .ok()
            .flatten()
        {
            return scoped;
        }
        static DEFAULT: OnceLock<Arc<Embedders>> = OnceLock::new();
        DEFAULT
            .get_or_init(|| Arc::new(Embedders::default()))
            .clone()
    }
}

/// Builds an [`Embedders`] registry ([`Embedders::builder`]).
#[derive(Debug)]
pub struct EmbeddersBuilder {
    embedders: Embedders,
}

impl EmbeddersBuilder {
    /// Registers `embedder` under its [`EmbedderInfo::name`]. Fails with
    /// [`EmbedError::Registry`] for an invalid info, a name already registered and a reserved
    /// name (`hash-ngram`, `hash-ngram/*`).
    pub fn with(mut self, embedder: Arc<dyn Embedder>) -> Result<Self, EmbedError> {
        let info = embedder.info();
        info.check()?;
        if is_reserved(&info.name) {
            return Err(EmbedError::registry(format!(
                "the embedder name {} is reserved for the built-in hash-ngram embedder",
                quoted(&info.name)
            )));
        }
        if self.embedders.get(&info.name).is_some() {
            return Err(EmbedError::registry(format!(
                "an embedder {} is already registered",
                quoted(&info.name)
            )));
        }
        self.embedders.entries.push(embedder);
        Ok(self)
    }

    /// Makes the registered embedder `name` the default. Fails with [`EmbedError::Registry`]
    /// when no embedder of that name is registered (yet).
    pub fn default_name(mut self, name: &str) -> Result<Self, EmbedError> {
        let Some(position) = self
            .embedders
            .entries
            .iter()
            .position(|e| e.info().name == name)
        else {
            return Err(EmbedError::registry(format!(
                "the default embedder {} is not registered",
                quoted(name)
            )));
        };
        self.embedders.default = position;
        Ok(self)
    }

    /// The registry.
    pub fn build(self) -> Embedders {
        self.embedders
    }
}
