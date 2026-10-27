//! The job `embed_property` (book page "Vectors", section "Embedding a property"): embeds one
//! property of many elements and stores the vectors in another, page by page, without holding
//! the graph's lock while the embedder runs.
//!
//! Phases: 0. collect the ids of the elements to embed (read lock, once); then per page of
//! `batch` ids: 1. read their texts (read lock, short), 2. embed them (no lock), 3. write the
//! vectors in one unit (write lock), re-reading each text first: a text changed or an element
//! removed since phase 1 is skipped and counted. Each page is atomic, the job as a whole is
//! not; with `only_missing` (the default) a second run continues where a failed one stopped.
//! Inside an open unit (an explicit transaction, a whole-script unit) every phase runs under
//! the lock the caller holds, the pages becoming savepoints of that unit.
//!
//! Provenance: the job needs an attached schema (mode `open` or `closed`) and records which
//! model produced the vectors in it (the vector keyword `embedder`, `provenance.rs`), in the
//! unit of the first page.

use std::time::Duration;

use compact_str::CompactString;

use super::provenance::{self, Provenance};
use super::{EmbedContext, EmbedError, Embedder, EmbedderInfo, Embedders, checked_embed};
use crate::error::{GraphError, TraverserError};
use crate::graph::schema::ElementKind;
use crate::graph::{Graph, GraphNodeType, GraphStorage, Transactional};
use crate::traversal::cancel::CancelToken;
use crate::value::{ElementProperty, PropKey, PropertyMap, Vector};

/// What `embed_property` does: embed the property `from` of every element with `label` and
/// store the vector in the property `to` (book page "Vectors", section "Embedding a
/// property"). Run it with [`Graph::embed_property`] (two-phase: the embedder runs without the
/// graph's lock) or [`Self::run_in`] (on a storage the caller holds, inside a transaction).
///
/// ```rust
/// use std::sync::Arc;
/// use graphersal::prelude::*;
/// use graphersal::exec::CancelToken;
/// use graphersal::vector::{EmbedJob, Embedders};
///
/// let graph = GraphSource::tinkerpop_modern();
/// {
///     let mut lock = graph.write();
///     let mut g = lock.traversal_mut();
///     let schema = g.infer_schema().unwrap().with_mode(SchemaMode::Open);
///     g.set_schema(schema).unwrap();
/// }
/// let job = EmbedJob::new("person", "name", "emb");
/// let report = graph
///     .embed_property(&job, &Embedders::default(), &CancelToken::new(), &mut |_| {})
///     .unwrap();
/// assert_eq!(report.embedded, 4);
/// assert!(report.schema_changed); // "emb" is declared with embedder "hash-ngram/1"
/// // A second run finds nothing to do: only elements without "emb" are embedded.
/// let again = graph
///     .embed_property(&job, &Embedders::default(), &CancelToken::new(), &mut |_| {})
///     .unwrap();
/// assert_eq!(again.embedded, 0);
/// ```
#[non_exhaustive]
pub struct EmbedJob {
    pub(crate) label: CompactString,
    pub(crate) from: PropKey,
    pub(crate) to: CompactString,
    pub(crate) element: ElementKind,
    pub(crate) embedder: Option<CompactString>,
    pub(crate) only_missing: bool,
    pub(crate) batch: Option<usize>,
    /// The front end's check of `Update` on the schema (the DSL's authorizer), asked when the
    /// job has to change the schema.
    pub(crate) schema_update: Option<SchemaUpdateCheck>,
}

/// A front end's permission check for the schema change of a job.
pub(crate) type SchemaUpdateCheck =
    std::sync::Arc<dyn Fn() -> Result<(), Box<TraverserError>> + Send + Sync>;

impl std::fmt::Debug for EmbedJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmbedJob")
            .field("label", &self.label)
            .field("from", &self.from)
            .field("to", &self.to)
            .field("element", &self.element)
            .field("embedder", &self.embedder)
            .field("only_missing", &self.only_missing)
            .field("batch", &self.batch)
            .finish_non_exhaustive()
    }
}

impl EmbedJob {
    /// Embeds the property `from` (a name or a [`JsonPath`](crate::value::JsonPath)) of the
    /// vertices with `label` into their top-level property `to`, with the registry's default
    /// embedder, skipping the elements that have `to` already, in pages of the embedder's
    /// `max_batch`.
    pub fn new(
        label: impl Into<CompactString>,
        from: impl Into<PropKey>,
        to: impl Into<CompactString>,
    ) -> Self {
        EmbedJob {
            label: label.into(),
            from: from.into(),
            to: to.into(),
            element: ElementKind::Vertex,
            embedder: None,
            only_missing: true,
            batch: None,
            schema_update: None,
        }
    }

    /// Embeds edges (`ElementKind::Edge`) instead of vertices.
    pub fn element(mut self, kind: ElementKind) -> Self {
        self.element = kind;
        self
    }

    /// The registry name of the embedder (default: the registry's default).
    pub fn embedder(mut self, name: impl Into<CompactString>) -> Self {
        self.embedder = Some(name.into());
        self
    }

    /// `false` embeds every element again, also the ones that have `to` (default `true`).
    pub fn only_missing(mut self, yes: bool) -> Self {
        self.only_missing = yes;
        self
    }

    /// The elements per page (one embedder call and one unit each; at least 1). Default: the
    /// embedder's [`EmbedderInfo::max_batch`].
    pub fn batch(mut self, n: usize) -> Self {
        self.batch = Some(n.max(1));
        self
    }

    /// Runs the job on a storage the caller holds (`&mut S` of a write lock, inside a
    /// [`transaction`](Transactional::transaction)): every phase runs under that access, so the
    /// embedder runs while the caller's lock is held, and each page is a nested unit (a
    /// savepoint) of the caller's open unit. Outside a unit each page is a unit of its own.
    pub fn run_in<S: GraphStorage>(
        &self,
        storage: &mut S,
        embedders: &Embedders,
        cancel: &CancelToken,
        progress: &mut dyn FnMut(&EmbedReport),
    ) -> Result<EmbedReport, EmbedJobError> {
        run(&mut Held(storage), self, embedders, cancel, progress)
    }
}

/// What a run of `embed_property` did (also the partial report of a failed run,
/// [`EmbedJobError::report`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct EmbedReport {
    /// Elements whose vector was stored.
    pub embedded: u64,
    /// Elements without the property `from` (or `null` there), or whose text has nothing to
    /// embed ([`EmbedError::NoFeatures`]).
    pub skipped_no_text: u64,
    /// Elements whose `from` is not a string (nothing is converted).
    pub not_text: u64,
    /// Elements whose text changed between the read and the write (another writer).
    pub skipped_changed: u64,
    /// Elements removed between the collection and the write.
    pub skipped_removed: u64,
    /// Committed units (pages; the unit that only declared the property counts too).
    pub units: u64,
    /// Whether the job declared the property (or its `embedder`) in the schema.
    pub schema_changed: bool,
    /// How long the job ran.
    pub elapsed: Duration,
}

/// A failed `embed_property`: the error and what the job did before it (earlier pages stay
/// committed; the failing page was rolled back). Rerunning the job with `only_missing`
/// continues where it stopped.
#[derive(Debug, thiserror::Error)]
#[error("{error}")]
#[non_exhaustive]
pub struct EmbedJobError {
    /// Why the job stopped (an embedder error, a schema violation, `Cancelled`, ...).
    pub error: Box<TraverserError>,
    /// The counts up to the failure.
    pub report: EmbedReport,
}

impl EmbedJobError {
    /// The help text of [`Self::error`].
    pub fn help(&self) -> Option<String> {
        crate::error::DiagnosticError::help(&*self.error)
    }
}

impl<S: GraphStorage> Graph<S> {
    /// Runs `job` (see [`EmbedJob`]) two-phase: the texts are read under a short read lock, the
    /// embedder runs without any lock of this graph, the vectors are written page by page in
    /// units of their own under the write lock, so other readers and writers go on while a
    /// slow embedder runs. `progress` gets the report after every page; `cancel` stops the job
    /// between pages and inside the embedder ([`EmbedContext::check`]).
    ///
    /// When the current thread holds a unit of this graph ([`Graph::begin_unit`]: a
    /// whole-script unit of the script layer), or the storage is inside an open unit that
    /// spans locks opened without one, the job runs every phase under one write lock instead,
    /// the pages becoming savepoints of that unit (inside a held unit, that write makes the
    /// unit exclusive until it closes). A unit another thread holds is not this job's: each
    /// page's write waits for it, as any writer does. Never call it while holding a guard of this graph (a deadlock): inside a
    /// transaction use [`EmbedJob::run_in`].
    ///
    /// Fails with [`EmbedJobError`] carrying the partial report.
    pub fn embed_property(
        &self,
        job: &EmbedJob,
        embedders: &Embedders,
        cancel: &CancelToken,
        progress: &mut dyn FnMut(&EmbedReport),
    ) -> Result<EmbedReport, EmbedJobError> {
        // A unit another thread holds shows in `unit_started_at` too: not this job's unit.
        if self.holds_unit() || (!self.unit_held() && self.read().unit_started_at().is_some()) {
            return job.run_in(&mut *self.write(), embedders, cancel, progress);
        }
        run(&mut Shared(self), job, embedders, cancel, progress)
    }
}

/// How the job reaches the storage: through the graph's locks, phase by phase, or through an
/// access the caller holds for the whole job.
trait Access<S: GraphStorage> {
    fn read<R>(&mut self, f: impl FnOnce(&S) -> R) -> R;
    fn write<R>(&mut self, f: impl FnOnce(&mut S) -> R) -> R;
}

struct Shared<'a, S: GraphStorage>(&'a Graph<S>);

impl<S: GraphStorage> Access<S> for Shared<'_, S> {
    fn read<R>(&mut self, f: impl FnOnce(&S) -> R) -> R {
        f(&self.0.read())
    }
    fn write<R>(&mut self, f: impl FnOnce(&mut S) -> R) -> R {
        f(&mut self.0.write())
    }
}

struct Held<'a, S: GraphStorage>(&'a mut S);

impl<S: GraphStorage> Access<S> for Held<'_, S> {
    fn read<R>(&mut self, f: impl FnOnce(&S) -> R) -> R {
        f(self.0)
    }
    fn write<R>(&mut self, f: impl FnOnce(&mut S) -> R) -> R {
        f(self.0)
    }
}

/// The error of a page's unit ([`Transactional::transaction`] needs `E: From<GraphError>`).
struct PageError(Box<TraverserError>);

impl From<GraphError> for PageError {
    fn from(err: GraphError) -> Self {
        PageError(Box::new(TraverserError::from(err)))
    }
}

/// What a page's unit did.
#[derive(Default)]
struct PageCounts {
    embedded: u64,
    changed: u64,
    removed: u64,
    schema_changed: bool,
}

/// The text at `from` in an element's properties.
enum SourceText {
    Absent,
    NotString,
    Found(String),
}

fn text_at(properties: Option<&PropertyMap>, from: &PropKey) -> SourceText {
    let Some(properties) = properties else {
        return SourceText::Absent;
    };
    let value = match from {
        PropKey::Name(name) => properties.get(name),
        PropKey::Path(path) => {
            crate::value::json_path::descend_properties(properties, path.segments())
        }
    };
    match value {
        None | Some(ElementProperty::Null) => SourceText::Absent,
        Some(value) => match value.text() {
            Some(text) => SourceText::Found(text.into_owned()),
            None => SourceText::NotString,
        },
    }
}

fn node_type(kind: ElementKind) -> GraphNodeType {
    match kind {
        ElementKind::Vertex => GraphNodeType::Vertex,
        ElementKind::Edge => GraphNodeType::Edge,
    }
}

/// The element `id` of `kind` and its properties, `None` when it is gone.
fn element<'s, S: GraphStorage>(
    storage: &'s S,
    kind: ElementKind,
    id: &str,
) -> Option<Option<&'s PropertyMap>> {
    match kind {
        ElementKind::Vertex => storage
            .get_vertex_by_id(id)
            .map(|v| storage.vertex_properties(v)),
        ElementKind::Edge => storage
            .get_edge_by_id(id)
            .map(|e| storage.edge_properties(e)),
    }
}

fn elapsed_ms(started: web_time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Embeds the texts of `items`, dropping (and counting in `skipped`) every text the embedder
/// finds nothing to embed in ([`EmbedError::NoFeatures`]) and calling it again with the rest.
fn embed_page(
    embedder: &dyn Embedder,
    items: &mut Vec<(CompactString, String)>,
    ctx: &EmbedContext<'_>,
    skipped: &mut u64,
    started: web_time::Instant,
) -> Result<Vec<Vector>, Box<TraverserError>> {
    loop {
        if items.is_empty() {
            return Ok(Vec::new());
        }
        let texts: Vec<&str> = items.iter().map(|(_, text)| text.as_str()).collect();
        match checked_embed(embedder, &texts, ctx) {
            Ok(vectors) => return Ok(vectors),
            Err(EmbedError::NoFeatures { index }) if index < items.len() => {
                items.remove(index);
                *skipped += 1;
            }
            Err(EmbedError::NoFeatures { index }) => {
                return Err(Box::new(TraverserError::Embed(EmbedError::bad_output(
                    format!(
                        "no features reported for position {index} of a batch of {}",
                        items.len()
                    ),
                ))));
            }
            Err(EmbedError::Cancelled) => {
                return Err(Box::new(TraverserError::Cancelled {
                    elapsed_ms: elapsed_ms(started),
                }));
            }
            Err(other) => return Err(Box::new(TraverserError::Embed(other))),
        }
    }
}

/// Whether the stored schema declares `to` for (`kind`, `label`) as vectors of `info`'s model
/// and dimension, any element kind (the write quantizes): the cheap check of every page;
/// otherwise the page plans again.
fn declared_for<S: GraphStorage>(
    storage: &S,
    kind: GraphNodeType,
    label: &str,
    to: &str,
    info: &EmbedderInfo,
) -> bool {
    storage
        .declared_property_schema(kind, &[CompactString::new(label)], to)
        .and_then(|node| node.vector)
        .is_some_and(|vector| {
            vector.embedder.as_deref() == Some(info.model.as_str())
                && vector
                    .dimension
                    .is_none_or(|dimension| dimension as usize == info.dimension)
        })
}

/// The schema change a page's unit makes first: none when the property is declared for the
/// model already, else the planned change (refused when the front end may not change it).
fn ensure_declared<S: GraphStorage>(
    storage: &mut S,
    job: &EmbedJob,
    info: &EmbedderInfo,
    allowed: bool,
) -> Result<bool, PageError> {
    let kind = node_type(job.element);
    if declared_for(storage, kind, &job.label, &job.to, info) {
        return Ok(false);
    }
    match provenance::plan(
        storage.get_schema().as_ref(),
        kind,
        &job.label,
        &job.to,
        info,
    )
    .map_err(PageError)?
    {
        Provenance::Declared => Ok(false),
        Provenance::Change(schema) => {
            if !allowed && let Some(check) = &job.schema_update {
                check().map_err(PageError)?;
            }
            storage.set_schema(schema)?;
            Ok(true)
        }
    }
}

fn run<S: GraphStorage, A: Access<S>>(
    access: &mut A,
    job: &EmbedJob,
    embedders: &Embedders,
    cancel: &CancelToken,
    progress: &mut dyn FnMut(&EmbedReport),
) -> Result<EmbedReport, EmbedJobError> {
    let started = web_time::Instant::now();
    let mut report = EmbedReport::default();
    let fail = |error: Box<TraverserError>, report: &EmbedReport| EmbedJobError {
        error,
        report: EmbedReport {
            elapsed: started.elapsed(),
            ..report.clone()
        },
    };
    let embedder = match embedders.resolve(job.embedder.as_deref()) {
        Ok(embedder) => embedder.clone(),
        Err(err) => return Err(fail(Box::new(TraverserError::Embed(err)), &report)),
    };
    let info = embedder.info().clone();
    let kind = node_type(job.element);

    // Phase 0: the schema check and the ids (read lock, once).
    let collected = access.read(|storage| {
        let plan = provenance::plan(
            storage.get_schema().as_ref(),
            kind,
            &job.label,
            &job.to,
            &info,
        )?;
        let keep = |properties: Option<&PropertyMap>| {
            !job.only_missing || !properties.is_some_and(|p| p.contains_key(&job.to))
        };
        let ids: Vec<CompactString> = match job.element {
            ElementKind::Vertex => storage
                .vertices_by_label(&job.label)
                .filter(|v| keep(storage.vertex_properties(*v)))
                .filter_map(|v| storage.vertex_id(v).map(CompactString::new))
                .collect(),
            ElementKind::Edge => storage
                .edges_by_label(&job.label)
                .filter(|e| keep(storage.edge_properties(*e)))
                .filter_map(|e| storage.edge_id(e).map(CompactString::new))
                .collect(),
        };
        Ok::<_, Box<TraverserError>>((plan, ids))
    });
    let (plan, ids) = match collected {
        Ok(collected) => collected,
        Err(err) => return Err(fail(err, &report)),
    };
    // The front end's check of the schema change, asked once when the plan needs one.
    let schema_allowed = match (&plan, &job.schema_update) {
        (Provenance::Change(_), Some(check)) => match check() {
            Ok(()) => true,
            Err(denied) => return Err(fail(denied, &report)),
        },
        _ => false,
    };

    let batch = job.batch.unwrap_or(info.max_batch).max(1);
    let ctx = EmbedContext::new().with_cancel(cancel);
    let mut pages: Vec<&[CompactString]> = ids.chunks(batch).collect();
    if pages.is_empty() && matches!(plan, Provenance::Change(_)) {
        // Nothing to embed: one unit declares the property all the same.
        pages.push(&[]);
    }
    for page in pages {
        if cancel.is_cancelled() {
            let error = TraverserError::Cancelled {
                elapsed_ms: elapsed_ms(started),
            };
            return Err(fail(Box::new(error), &report));
        }
        // Phase 1.1: the texts (read lock, short).
        let mut items: Vec<(CompactString, String)> = Vec::with_capacity(page.len());
        access.read(|storage| {
            for id in page {
                match element(storage, job.element, id) {
                    None => report.skipped_removed += 1,
                    Some(properties) => match text_at(properties, &job.from) {
                        SourceText::Absent => report.skipped_no_text += 1,
                        SourceText::NotString => report.not_text += 1,
                        SourceText::Found(text) => items.push((id.clone(), text)),
                    },
                }
            }
        });
        // Phase 1.2: the embedder, without any lock.
        let vectors = match embed_page(
            &*embedder,
            &mut items,
            &ctx,
            &mut report.skipped_no_text,
            started,
        ) {
            Ok(vectors) => vectors,
            Err(err) => return Err(fail(err, &report)),
        };
        // Phase 1.3: one unit per page (write lock); the text is read again.
        let written = access.write(|storage| {
            storage.transaction(|storage| {
                let mut counts = PageCounts {
                    schema_changed: ensure_declared(storage, job, &info, schema_allowed)?,
                    ..PageCounts::default()
                };
                for ((id, text), vector) in items.iter().zip(vectors) {
                    let current = match element(storage, job.element, id) {
                        None => {
                            counts.removed += 1;
                            continue;
                        }
                        Some(properties) => text_at(properties, &job.from),
                    };
                    if !matches!(&current, SourceText::Found(now) if now == text) {
                        counts.changed += 1;
                        continue;
                    }
                    let value = ElementProperty::Vector(vector);
                    match job.element {
                        ElementKind::Vertex => {
                            if let Some(v) = storage.get_vertex_by_id(id) {
                                storage.set_vertex_property(v, job.to.clone(), value)?;
                            }
                        }
                        ElementKind::Edge => {
                            if let Some(e) = storage.get_edge_by_id(id) {
                                storage.set_edge_property(e, job.to.clone(), value)?;
                            }
                        }
                    }
                    counts.embedded += 1;
                }
                Ok::<_, PageError>(counts)
            })
        });
        match written {
            Ok(counts) => {
                report.embedded += counts.embedded;
                report.skipped_changed += counts.changed;
                report.skipped_removed += counts.removed;
                report.schema_changed |= counts.schema_changed;
                report.units += 1;
            }
            Err(PageError(err)) => return Err(fail(err, &report)),
        }
        report.elapsed = started.elapsed();
        progress(&report);
    }
    report.elapsed = started.elapsed();
    Ok(report)
}
