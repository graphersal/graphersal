use compact_str::CompactString;
use std::collections::HashMap;
use web_time::Instant;

use crate::GraphSource;
use crate::error::{ResultExt, StepAt, StepFrame, TraverserError, TraverserResult};
use crate::graph::{
    Definition, DefinitionKind, Graph, GraphNodeType, GraphSchema, GraphStorage, QueryDefinition,
    SchemaDiff, SchemaPatch, SchemaValidation, TraversalGraph, VisualizeFormat,
};
use crate::traversal::bulk;
use crate::traversal::context::{
    ExecutionOptions, ProfileType, TraversalStrategy, TraverserContext, tick_deadline,
};
use crate::traversal::execution::{ExecOptions, Execution};
use crate::traversal::metrics::TraversalMetrics;
use crate::traversal::optimizer::LevelPlan;
use crate::traversal::optimizer::path_analysis::PathMode;
#[cfg(not(feature = "alloc-tracking"))]
use crate::traversal::step::estimate_traversers_bytes;
use crate::traversal::step::{GraphStep, MemoryFigures, ProfileContext};
use crate::traversal::traverser::Traverser;
use crate::value::{ElementProperty, PathRef, PropertyMap, StreamMap, TraverserValue};
use fnv::FnvBuildHasher;
use indexmap::IndexMap;

#[cfg(feature = "io")]
use crate::error::IOError;
#[cfg(feature = "io")]
use crate::io::GraphExport;

/// Mode used for a step that was never analysed: recording the full path is always correct.
static FULL_PATH_MODE: PathMode = PathMode::Full;

/// Where a run of steps sits in the plan: the child traversal its level is (`None` at the top
/// level, see `GraphStep::child_traversals_mut`) and the position of the run's first step in
/// that level (non-zero when a short-circuiting evaluation hands the rest of a level over).
/// Together with a step's own position it is the step id that profiling and error locations
/// share.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LevelAt {
    pub(crate) child: Option<usize>,
    pub(crate) first: usize,
}

/// Adds the bulk of `source` to the input count of top-level step `index`.
#[inline(never)]
fn note_step_input<S: GraphStorage>(inputs: &mut Vec<u64>, index: usize, source: &[Traverser<S>]) {
    if inputs.len() <= index {
        inputs.resize(index + 1, 0);
    }
    let bulk = source
        .iter()
        .fold(0u64, |sum, traverser| sum.saturating_add(traverser.bulk));
    if let Some(slot) = inputs.get_mut(index) {
        *slot = slot.saturating_add(bulk);
    }
}

impl LevelAt {
    /// The top-level traversal.
    pub(crate) const TOP: LevelAt = LevelAt {
        child: None,
        first: 0,
    };

    /// The child traversal at `index` of the enclosing step, from its first step.
    pub(crate) fn child(index: usize) -> Self {
        LevelAt {
            child: Some(index),
            first: 0,
        }
    }

    /// The location of the step at `logical` (counted from the first step of the run).
    fn step(self, logical: usize) -> StepAt {
        StepAt {
            child: self.child,
            index: self.first + logical,
        }
    }
}

/// One run of a pipeline: its traversers (with bulk) or the error that stopped it, the profile
/// recorded up to that point, and what a materializing caller expands with: the expansion limits
/// (`bulk.max_expansion`, `traversal.max_materialized_bytes`) and what is left of the run's
/// `evaluationTimeout`.
struct RunOutcome<S: GraphStorage> {
    result: TraverserResult<Vec<Traverser<S>>>,
    profile: ProfileContext,
    expansion: Expansion,
    /// The traversers that reached each top-level step (`TraverserContext::step_inputs`), when
    /// the run records workload statistics.
    step_inputs: Option<Vec<u64>>,
}

/// The bounds a terminal expands a run's traversers under: the run's limits and its deadline,
/// so the expansion of a terminal list counts against the same `evaluationTimeout`.
pub(crate) struct Expansion {
    limits: bulk::ExpansionLimits,
    deadline: Option<crate::traversal::context::Deadline>,
}

impl Expansion {
    /// No bound (a run that failed before its options were parsed has nothing to expand).
    const UNBOUNDED: Self = Self {
        limits: bulk::ExpansionLimits::UNBOUNDED,
        deadline: None,
    };

    /// Expands `traversers` (see [`bulk::expand`]); `step` names the boundary in an error.
    pub(crate) fn expand<S: GraphStorage>(
        &mut self,
        traversers: Vec<Traverser<S>>,
        step: &str,
    ) -> TraverserResult<Vec<Traverser<S>>> {
        bulk::expand(traversers, self.limits, &mut self.deadline, step)
    }
}

/// What [`GraphTraversalSource::execute_items`] produced: the materialized results (empty when
/// the run failed), the error, and the profile when one was requested.
pub(crate) struct ExecutedItems<T> {
    pub(crate) items: Vec<T>,
    pub(crate) error: Option<crate::error::BoxedTraverserError>,
    pub(crate) profile: Option<TraversalMetrics>,
    /// Whether the run committed a change (its unit advanced the commit sequence number).
    pub(crate) mutated: bool,
    /// What the run changed and kept (see [`Execution::changes`]).
    pub(crate) changes: crate::graph::UnitChanges,
}

/// A fused source count scaled by the incoming bulk (see the `Init` arm of
/// `execute_optimized_steps`).
fn scale_fused(count: usize, factor: u64) -> TraverserResult<usize> {
    let scaled = bulk::scale_count(count as u64, factor, "count")?;
    usize::try_from(scaled).map_err(|_| {
        Box::new(TraverserError::BulkOverflow {
            step: "count".to_string(),
        })
    })
}

/// Records that a step folded `total` traversers into the single count traverser of the fused
/// `count()` after it, and returns `total`. The profile then shows the step's real output (`Out`)
/// and the `count()`'s real input (`In`) instead of the one count traverser between them.
#[inline]
fn note_collapsed(slot: &mut Option<usize>, total: u64) -> u64 {
    *slot = Some(usize::try_from(total).unwrap_or(usize::MAX));
    total
}

/// The path mode of the step at `physical` position. A level that was never analysed (no modes)
/// records full paths, which is the behaviour without the analysis.
#[inline]
fn path_mode_at(path_modes: &[PathMode], physical: usize) -> &PathMode {
    path_modes.get(physical).unwrap_or(&FULL_PATH_MODE)
}

/// What `.profile()` records about a step: its plain name and, when it records anything, its path
/// recording mode (`full`, `labels(a,b)`), so the path requirement analysis is observable (the
/// table renders it as `out() [path: full]`).
fn profile_step<'g, S: GraphStorage + 'g>(
    step: &GraphStep<'g, S>,
    mode: &PathMode,
) -> (String, Option<String>) {
    let recording = (!mode.is_none()).then(|| mode.to_string());
    (step.get_name(), recording)
}

/// Renders the physical plan of one traversal level for diagnostics, marking the failing step.
///
/// Modulator steps are omitted so that positions line up with the logical step indices used by
/// profiling, and the plan reflects the optimized steps as they are actually executed.
/// Position of a step inside the full step list, given its index among non-modulator steps.
///
/// Profiling and step paths count logical steps only, while the optimizer indexes provenance by
/// physical position, modulators included.
fn physical_index<'g, S: GraphStorage + 'g>(steps: &[GraphStep<'g, S>], position: usize) -> usize {
    let mut seen = 0;
    for (index, step) in steps.iter().enumerate() {
        if matches!(step, GraphStep::Modulator(_, _)) {
            continue;
        }
        if seen == position {
            return index;
        }
        seen += 1;
    }
    steps.len()
}

/// The step a modulator at `physical` modulates: the last one before it that is neither a
/// modulator nor an `as()` label (see `push_step_into`), or `None` when there is none.
fn modulated_step<'a, 'g, S: GraphStorage + 'g>(
    steps: &'a [GraphStep<'g, S>],
    physical: usize,
) -> Option<(usize, &'a GraphStep<'g, S>)> {
    let before = steps.get(..physical).unwrap_or(steps);
    if matches!(
        steps.get(physical),
        Some(GraphStep::Modulator("from" | "to", _))
    ) {
        // `from()`/`to()` modulate the `add_e()` before any `property()` calls in between.
        return crate::traversal::from_to_target(before)
            .and_then(|index| before.get(index).map(|s| (index, s)));
    }
    before
        .iter()
        .enumerate()
        .rev()
        .find(|(_, s)| !matches!(s, GraphStep::Modulator(_, _) | GraphStep::As(_, _)))
}

/// Logical position (as counted by profiling and plan frames) of the step the modulator at
/// `physical` modulates; `logical` is the number of logical steps before `physical`.
fn modulated_position<'g, S: GraphStorage + 'g>(
    steps: &[GraphStep<'g, S>],
    physical: usize,
    logical: usize,
) -> usize {
    match modulated_step(steps, physical) {
        Some((index, _)) => {
            let after = steps
                .get(index..physical)
                .unwrap_or(&[])
                .iter()
                .filter(|s| !matches!(s, GraphStep::Modulator(_, _)))
                .count();
            logical.saturating_sub(after)
        }
        None => 0,
    }
}

/// Name of the step the modulator at `physical` modulates, for the failure frame.
fn target_name<'g, S: GraphStorage + 'g>(steps: &[GraphStep<'g, S>], physical: usize) -> String {
    match modulated_step(steps, physical) {
        Some((_, step)) => step.get_name(),
        None => "by()".to_string(),
    }
}

/// Renders the executed plan of one traversal level for diagnostics, marking the failing step and,
/// when the optimizer rewrote the level, the steps it originates from in the user's own query.
///
/// Modulator steps are omitted so that positions line up with the logical step indices used by
/// profiling. Display-only steps (the `limit(n) [display]` a bounded visualizer appends) are
/// omitted too, unless the failure is that step itself: an error location shows the user's
/// query, not the visualizer's bound (`displaying_results.md`). They are always appended last,
/// so the step indices of the user's steps do not change.
fn plan_frame<'g, S: GraphStorage + 'g>(
    steps: &[GraphStep<'g, S>],
    level: Option<&LevelPlan<'g, S>>,
    position: usize,
) -> StepFrame {
    let (names, positions) = without_display_steps(
        steps
            .iter()
            .filter(|step| !matches!(step, GraphStep::Modulator(_, _)))
            .map(|step| step.get_name())
            .collect(),
        &[position],
    );
    let frame = StepFrame::new(names, positions.first().copied().unwrap_or(position));
    match level {
        Some(level) => {
            let (logical, positions) = level.resolve(steps, physical_index(steps, position));
            let (logical, positions) = without_display_steps(logical, &positions);
            frame.with_logical(crate::error::LogicalFrame {
                steps: logical,
                positions,
            })
        }
        None => frame,
    }
}

/// Removes the display-only steps (rendered with the `" [display]"` mark, see
/// `StepLimit::display`) from the rendered steps of one level, except a marked one, and maps
/// the `marked` positions onto the remaining steps.
pub(crate) fn without_display_steps(
    names: Vec<String>,
    marked: &[usize],
) -> (Vec<String>, Vec<usize>) {
    if !names.iter().any(|name| is_display_only(name)) {
        return (names, marked.to_vec());
    }
    let mut kept = Vec::with_capacity(names.len());
    let mut remapped = Vec::with_capacity(marked.len());
    for (index, name) in names.into_iter().enumerate() {
        let is_marked = marked.contains(&index);
        if is_display_only(&name) && !is_marked {
            continue;
        }
        if is_marked {
            remapped.push(kept.len());
        }
        kept.push(name);
    }
    (kept, remapped)
}

/// Whether a rendered step is a display-only step a bounded visualizer appended.
pub(crate) fn is_display_only(name: &str) -> bool {
    name.ends_with(crate::traversal::render::DISPLAY_MARK)
}

/// The step at `physical` as a failure location: its rendered name and logical position. A
/// modulator (a rejected `by()`) points at the step it modulates; with no step before it, the
/// location is the modulator itself, as the only step of a one-step frame (`None` position).
fn fault_position<'g, S: GraphStorage + 'g>(
    steps: &[GraphStep<'g, S>],
    physical: usize,
) -> (String, Option<usize>) {
    let logical_of = |index: usize| {
        steps
            .iter()
            .take(index)
            .filter(|step| !matches!(step, GraphStep::Modulator(_, _)))
            .count()
    };
    match steps.get(physical) {
        Some(GraphStep::Modulator(_, modulator)) => match modulated_step(steps, physical) {
            Some((index, step)) => (step.get_name(), Some(logical_of(index))),
            None => (modulator.to_string(), None),
        },
        Some(step) => (step.get_name(), Some(logical_of(physical))),
        None => (String::new(), None),
    }
}

/// Depth-first search, in plan order (a step before its child traversals), for the first step
/// `fault` matches. On success `path` holds one `(name, location, frame)` per level, outermost
/// first. Child levels are addressed by `AnonymousTraversal::child_index`, so the plan must be
/// numbered (`GraphStep::number_children`).
fn find_fault<'g, S: GraphStorage + 'g>(
    steps: &mut [GraphStep<'g, S>],
    level: Option<&LevelPlan<'g, S>>,
    at: LevelAt,
    fault: &dyn Fn(&GraphStep<'g, S>) -> bool,
    path: &mut Vec<(String, StepAt, StepFrame)>,
) -> bool {
    for physical in 0..steps.len() {
        let mut found = fault(&steps[physical]);
        if !found {
            for child in steps[physical].child_traversals_mut() {
                let child_at = LevelAt::child(child.child_index);
                let child_level = child.level.clone();
                if find_fault(
                    &mut child.steps,
                    child_level.as_deref(),
                    child_at,
                    fault,
                    path,
                ) {
                    found = true;
                    break;
                }
            }
        }
        if found {
            let (name, logical) = fault_position(steps, physical);
            let entry = match logical {
                Some(logical) => (name, at.step(logical), plan_frame(steps, level, logical)),
                None => (name.clone(), at.step(0), StepFrame::new(vec![name], 0)),
            };
            path.insert(0, entry);
            return true;
        }
    }
    false
}

/// Gives an error raised before any step runs (a plan-time check, a permission denial) the
/// location of the step it is about: the step `fault` matches first (plan order, child
/// traversals depth first) becomes the `TraverserError::StepFailed` chain a run-time failure of
/// that step would carry (`at #N`, [`TraverserError::step_location`]). Returns `err` unchanged
/// when no step matches.
/// The one label `labels` names (repeated names count once), or `None` for none or several.
/// The candidates of a `v()` source that reads a property index ([`IndexAccess`]): the vertices
/// of every label in every range of the access, each once. `None` when the storage serves no
/// index for one of the labels (the source scans its labels instead).
///
/// [`IndexAccess`]: crate::traversal::step::IndexAccess
fn index_vertices<S: GraphStorage>(
    storage: &S,
    labels: &[String],
    access: &crate::traversal::step::IndexAccess,
) -> Result<Option<Vec<S::VertexId>>, crate::error::GraphError> {
    let dedup = labels.len() > 1 || access.ranges.len() > 1;
    let mut seen: std::collections::HashSet<S::VertexId> = std::collections::HashSet::new();
    let mut out = Vec::new();
    for label in labels {
        for range in &access.ranges {
            let found = match &access.transform {
                None => storage.vertices_by_property_index(label, &access.key, range)?,
                Some(transform) => storage.vertices_by_transformed_property_index(
                    label,
                    &access.key,
                    transform,
                    range,
                )?,
            };
            let Some(found) = found else {
                return Ok(None);
            };
            for node in found {
                if !dedup || seen.insert(node) {
                    out.push(node);
                }
            }
        }
    }
    Ok(Some(out))
}

/// The edge counterpart of [`index_vertices`].
fn index_edges<S: GraphStorage>(
    storage: &S,
    labels: &[String],
    access: &crate::traversal::step::IndexAccess,
) -> Result<Option<Vec<S::EdgeId>>, crate::error::GraphError> {
    let dedup = labels.len() > 1 || access.ranges.len() > 1;
    let mut seen: std::collections::HashSet<S::EdgeId> = std::collections::HashSet::new();
    let mut out = Vec::new();
    for label in labels {
        for range in &access.ranges {
            let found = match &access.transform {
                None => storage.edges_by_property_index(label, &access.key, range)?,
                Some(transform) => storage.edges_by_transformed_property_index(
                    label,
                    &access.key,
                    transform,
                    range,
                )?,
            };
            let Some(found) = found else {
                return Ok(None);
            };
            for edge in found {
                if !dedup || seen.insert(edge) {
                    out.push(edge);
                }
            }
        }
    }
    Ok(Some(out))
}

fn single_label(labels: &[String]) -> Option<&str> {
    let (first, rest) = labels.split_first()?;
    rest.iter().all(|l| l == first).then_some(first.as_str())
}

pub(crate) fn locate_pre_run_error<'g, S: GraphStorage + 'g>(
    steps: &mut [GraphStep<'g, S>],
    level: Option<&LevelPlan<'g, S>>,
    err: crate::error::BoxedTraverserError,
    fault: &dyn Fn(&GraphStep<'g, S>) -> bool,
) -> crate::error::BoxedTraverserError {
    let mut path = Vec::new();
    if !find_fault(steps, level, LevelAt::TOP, fault, &mut path) {
        return err;
    }
    let mut located: TraverserResult<()> = Err(err);
    while let Some((name, at, frame)) = path.pop() {
        located = located.with_step_frame(name, at, || frame);
    }
    match located {
        Err(err) => err,
        // Unreachable: `located` starts as an error and every wrapping keeps it one.
        Ok(()) => Box::new(TraverserError::wrong_query("unlocated plan error")),
    }
}

/// Resolves the embedder of every `embed` step of `steps` (child traversals included) in
/// `registry` and binds it to the step; an unknown name is [`TraverserError::Embed`] located at
/// its step. A text query of `similarity`/`nearest` binds the candidate embedders of the model
/// the attached schema declares for its property (`schema` is read once, only when a text
/// query exists). Runs before the permission check, on the plan as written.
fn bind_embedders<'g, S: GraphStorage + 'g>(
    steps: &mut [GraphStep<'g, S>],
    registry: &crate::value::Embedders,
    schema: &dyn Fn() -> Option<crate::graph::schema::GraphSchema>,
) -> TraverserResult<()> {
    type Schema = Option<Option<crate::graph::schema::GraphSchema>>;
    fn walk<'g, S: GraphStorage + 'g>(
        steps: &mut [GraphStep<'g, S>],
        registry: &crate::value::Embedders,
        schema: &dyn Fn() -> Option<crate::graph::schema::GraphSchema>,
        loaded: &mut Schema,
    ) -> Result<(), crate::value::EmbedError> {
        for step in steps.iter_mut() {
            if let GraphStep::Map(_, map) = step
                && let Some(requested) = map.embedder_request()
            {
                let embedder = registry.resolve(requested.as_deref())?.clone();
                map.bind_embedder(embedder);
            }
            let (name, text) = match step {
                GraphStep::Map(name, map) => (*name, map.text_query_mut()),
                GraphStep::FlatBarrier(name, step) => (*name, step.text_query_mut()),
                _ => ("", None),
            };
            if let Some((key, text)) = text {
                let schema = loaded.get_or_insert_with(schema).as_ref();
                text.bind(name, key, schema, registry)?;
            }
            for child in step.child_traversals_mut() {
                walk(&mut child.steps, registry, schema, loaded)?;
            }
        }
        Ok(())
    }
    walk(steps, registry, schema, &mut None).map_err(|err| {
        let fault = |step: &GraphStep<'g, S>| match step {
            GraphStep::Map(_, map) => {
                map.embedder_request()
                    .is_some_and(|requested| registry.resolve(requested.as_deref()).is_err())
                    || map.text_query().is_some_and(|text| text.bind_failed())
            }
            GraphStep::FlatBarrier(_, step) => {
                step.text_query().is_some_and(|text| text.bind_failed())
            }
            _ => false,
        };
        GraphStep::number_children(steps);
        locate_pre_run_error(steps, None, Box::new(TraverserError::Embed(err)), &fault)
    })
}

/// [`locate_pre_run_error`] for an error of the plan-time validation pass
/// (`PathAnalysis::analyze_query`): an undeclared step label points at the step that reads it,
/// any other error at the step whose plan check (`GraphStep::modulator_check`) raises the same
/// error.
fn locate_plan_error<'g, S: GraphStorage + 'g>(
    steps: &mut [GraphStep<'g, S>],
    level: Option<&LevelPlan<'g, S>>,
    err: crate::error::BoxedTraverserError,
) -> crate::error::BoxedTraverserError {
    match &*err {
        TraverserError::StepFailed { .. } => err,
        TraverserError::Value(crate::error::ValueError::UndeclaredLabel { label }) => {
            let label = label.clone();
            locate_pre_run_error(steps, level, err, &|step| {
                step.required_labels()
                    .is_some_and(|(labels, _)| labels.iter().any(|l| *l == label))
            })
        }
        other => {
            let text = other.to_string();
            locate_pre_run_error(steps, level, err, &|step| {
                step.modulator_check()
                    .is_err_and(|found| found.to_string() == text)
            })
        }
    }
}

impl GraphSource {
    /// A new, empty graph.
    pub fn empty() -> Graph {
        Graph::new(TraversalGraph::new())
    }

    /// Loads a graph from a GraphML file.
    ///
    /// The graph is not published yet, so the load runs outside units (no undo log, no change
    /// capture, no commit hooks; `book/src/users_guide/transactions.md`): on error the half-built graph
    /// is dropped. Importing into a live graph ([`GraphImport`](crate::io::GraphImport)) is one captured unit instead.
    #[cfg(feature = "io")]
    pub fn from_graphml(path: impl AsRef<std::path::Path>) -> Result<Graph, IOError> {
        let path = path.as_ref();
        let file = std::fs::File::open(path).map_err(|err| IOError::FileRead {
            path: path.to_string_lossy().into_owned(),
            source: err,
        })?;
        Self::from_graphml_reader(std::io::BufReader::with_capacity(128 * 1024, file))
    }

    /// Loads a graph from GraphML text read from `reader`, outside units like
    /// [`Self::from_graphml`].
    #[cfg(feature = "io")]
    pub fn from_graphml_reader<R: std::io::BufRead>(reader: R) -> Result<Graph, IOError> {
        let mut graph = TraversalGraph::new();
        graph.load_outside_units(|graph| {
            crate::io::GraphMLImport {}.import_outside_units(
                graph,
                quick_xml::Reader::from_reader(reader),
                &mut None,
            )
        })?;
        Ok(Graph::new(graph))
    }

    /// Loads a graph from a GraphSON file (line or wrapped form), outside units like
    /// [`Self::from_graphml`].
    ///
    /// Skipped data (dates, unsupported types, dangling edges, ...) is not an error; it is logged
    /// as a `tracing` warning. Use [`GraphSONImport::load`](crate::io::GraphSONImport::load) to get
    /// the [`ImportReport`](crate::io::ImportReport) itself.
    #[cfg(feature = "io")]
    pub fn from_graphson(path: impl AsRef<std::path::Path>) -> Result<Graph, IOError> {
        let path = path.as_ref();
        let file = std::fs::File::open(path).map_err(|err| IOError::FileRead {
            path: path.to_string_lossy().into_owned(),
            source: err,
        })?;
        Self::from_graphson_reader(std::io::BufReader::with_capacity(128 * 1024, file))
    }

    /// Loads a graph from GraphSON text read from `reader`, outside units like
    /// [`Self::from_graphson`].
    #[cfg(feature = "io")]
    pub fn from_graphson_reader<R: std::io::BufRead>(reader: R) -> Result<Graph, IOError> {
        let (graph, report) = crate::io::GraphSONImport::new().load(reader)?;
        if report.has_skips() {
            tracing::warn!(%report, "GraphSON import skipped data");
        }
        Ok(graph)
    }

    /// Loads a graph file, choosing the importer from the file extension (case-insensitive):
    /// GraphSON for `.json`, `.jsonl` and `.graphson`, GraphML for `.xml` and `.graphml`, and
    /// with the `persist` feature a packed snapshot for `.gsnap` (`GraphSource::from_snapshot`).
    ///
    /// Any other extension fails with [`IOError::UnsupportedFormat`]. Front ends (the script
    /// `file()` source, `graphersal --graph <path>`) go through this function so that the list of
    /// accepted extensions lives in one place.
    #[cfg(feature = "io")]
    pub fn from_file(path: impl AsRef<std::path::Path>) -> Result<Graph, IOError> {
        let path = path.as_ref();
        if Self::is_graphson_path(path) {
            Self::from_graphson(path)
        } else if Self::is_graphml_path(path) {
            Self::from_graphml(path)
        } else if Self::is_snapshot_path(path) {
            #[cfg(feature = "persist")]
            {
                Self::from_snapshot(path)
            }
            #[cfg(not(feature = "persist"))]
            {
                Err(IOError::UnsupportedFormat {
                    path: path.to_string_lossy().into_owned(),
                })
            }
        } else {
            Err(IOError::UnsupportedFormat {
                path: path.to_string_lossy().into_owned(),
            })
        }
    }

    /// Whether [`Self::from_file`] reads `path` as a packed snapshot (`.gsnap`,
    /// case-insensitive; only with the `persist` feature).
    #[cfg(feature = "io")]
    pub fn is_snapshot_path(path: impl AsRef<std::path::Path>) -> bool {
        cfg!(feature = "persist")
            && path
                .as_ref()
                .to_string_lossy()
                .to_lowercase()
                .ends_with(".gsnap")
    }

    /// Loads a packed snapshot file (`persist::read_snapshot`): the graph with its schema,
    /// lineage id, commit position and auto-id sequences.
    #[cfg(all(feature = "io", feature = "persist"))]
    pub fn from_snapshot(path: impl AsRef<std::path::Path>) -> Result<Graph, IOError> {
        let path = path.as_ref();
        let file = std::fs::File::open(path).map_err(|err| IOError::FileRead {
            path: path.to_string_lossy().into_owned(),
            source: err,
        })?;
        let graph = TraversalGraph::read_snapshot(std::io::BufReader::with_capacity(1 << 20, file))
            .map_err(|source| IOError::Snapshot {
                path: path.to_string_lossy().into_owned(),
                source: Box::new(source),
            })?;
        Ok(Graph::new(graph))
    }

    /// Whether [`Self::from_file`] reads `path` as GraphSON (`.json`, `.jsonl`, `.graphson`,
    /// case-insensitive).
    #[cfg(feature = "io")]
    pub fn is_graphson_path(path: impl AsRef<std::path::Path>) -> bool {
        let lower = path.as_ref().to_string_lossy().to_lowercase();
        [".json", ".jsonl", ".graphson"]
            .iter()
            .any(|ext| lower.ends_with(ext))
    }

    /// Whether [`Self::from_file`] reads `path` as GraphML (`.xml`, `.graphml`,
    /// case-insensitive).
    #[cfg(feature = "io")]
    pub fn is_graphml_path(path: impl AsRef<std::path::Path>) -> bool {
        let lower = path.as_ref().to_string_lossy().to_lowercase();
        lower.ends_with(".xml") || lower.ends_with(".graphml")
    }

    /// The standard TinkerPop "modern" toy graph.
    // Based on the following schema: https://tinkerpop.apache.org/docs/current/images/tinkerpop-modern.png
    pub fn tinkerpop_modern() -> Graph {
        Graph::new(TraversalGraph::tinkerpop_modern())
    }

    /// A small, deterministic, filesystem-like sample tree (37 vertices, 38 edges): vertex ids
    /// are paths from `"root"`, labels are `directory`/`file`/`symlink`, `contains` edges link
    /// parent to child and two `points_to` edges add a cycle and a diamond. See
    /// [`TraversalGraph::file_tree`] for the full shape.
    pub fn file_tree() -> Graph {
        Graph::new(TraversalGraph::file_tree())
    }

    /// The TinkerPop "crew" toy graph. Not yet populated; currently an empty graph.
    // Based on the https://github.com/apache/tinkerpop/blob/master/data/tinkerpop-crew.json
    // https://tinkerpop.apache.org/docs/current/images/the-crew-graph.png
    pub fn tinkerpop_crew() -> Graph {
        GraphSource::empty()
    }

    /// A synthetic graph of about 110k vertices/edges, for benchmarking.
    pub fn large_110k() -> Graph {
        let top_level_count = 10;
        let second_level_count = 10;
        let inner_level = 5;

        let mut g = TraversalGraph::new();

        {
            let mut prev_vertex_ids: Vec<crate::graph::VertexHandle> = vec![];
            for level in 0..inner_level {
                let num_of_nodes = if level == 0 {
                    top_level_count
                } else {
                    second_level_count
                };

                if prev_vertex_ids.is_empty() {
                    for index in 0..num_of_nodes {
                        let id: String = format!("V_{}_{}", level, index);
                        let mut value = PropertyMap::default();
                        value.insert("id", ElementProperty::String(id.clone().into()));
                        value.insert("label", ElementProperty::String(id.clone().into()));
                        value.insert(
                            "level",
                            ElementProperty::String(format!("{}", level).into()),
                        );
                        let node_index = g
                            .insert_vertex(Some(id.clone().into()), vec![id.clone().into()], value)
                            .unwrap();
                        prev_vertex_ids.push(node_index);
                    }
                } else {
                    let mut current_vertex_ids: Vec<crate::graph::VertexHandle> = vec![];
                    let mut node_counter = 0;
                    for prev_node_index in prev_vertex_ids.into_iter() {
                        for _index in 0..num_of_nodes {
                            let id: String = format!("V_{}_{}", level, node_counter);
                            node_counter += 1;
                            let mut value = PropertyMap::default();
                            value.insert("id", ElementProperty::String(id.clone().into()));
                            value.insert("label", ElementProperty::String(id.clone().into()));
                            value.insert(
                                "level",
                                ElementProperty::String(format!("{}", level).into()),
                            );
                            let node_index = g
                                .insert_vertex(
                                    Some(id.clone().into()),
                                    vec![id.clone().into()],
                                    value,
                                )
                                .unwrap();
                            g.insert_edge(
                                None::<CompactString>,
                                None::<CompactString>,
                                prev_node_index,
                                node_index,
                                PropertyMap::default(),
                            )
                            .unwrap();
                            current_vertex_ids.push(node_index);
                        }
                    }
                    prev_vertex_ids = current_vertex_ids;
                }
            }
        }
        // Built element by element: release the spare slots, as a load does.
        g.shrink_to_fit();
        Graph::new(g)
    }
}

/// Allocator counters snapshotted immediately before a step runs, when `.profile()` is active
/// (D1/D2 of the richer-memory-diagnostics spec). Bundled into one `Copy` value so the call sites
/// that need a "before" snapshot (`execute_optimized_steps`'s main loop and every profiled arm of
/// `exists_in_level`) read all three thread-locals at one point. A step must never be measured
/// against `AllocSnapshot::default()`: the delta would be the thread's whole counter, added up
/// once per call of a nested step.
#[derive(Clone, Copy, Default)]
#[cfg_attr(not(feature = "alloc-tracking"), allow(dead_code))]
struct AllocSnapshot {
    allocated: u64,
    live: i64,
    alloc_count: u64,
}

impl AllocSnapshot {
    /// Zeroed when `ProfileType::Memory` wasn't requested for this run (the values are never read
    /// by `step_memory_metrics` in that case — no thread-local reads at all, per D2) or
    /// `alloc-tracking` is not compiled in (same reasoning `alloc_before` used before this spec).
    #[inline]
    #[allow(unused_variables)]
    fn capture(profile_memory: bool) -> Self {
        #[cfg(feature = "alloc-tracking")]
        {
            if !profile_memory {
                return Self::default();
            }
            Self {
                allocated: crate::alloc_tracking::thread_allocated_bytes(),
                live: crate::alloc_tracking::thread_live_bytes(),
                alloc_count: crate::alloc_tracking::thread_alloc_count(),
            }
        }
        #[cfg(not(feature = "alloc-tracking"))]
        {
            Self::default()
        }
    }
}

impl<'g, S: GraphStorage + 'g> crate::GraphTraversalSource<'g, S> {
    /// A new traversal source over `storage`, with an empty step pipeline.
    pub fn new(storage: S) -> Self {
        debug_assert!(
            storage.capabilities().is_ok_and(|c| c.is_consistent()),
            "inconsistent StorageCapabilities (change_capture requires atomic): {:?}",
            storage.capabilities()
        );
        Self {
            storage,
            strategy: TraversalStrategy::Diagnostics,
            steps: Vec::new(),
            side_effect: HashMap::new(),
            options: HashMap::new(),
            profile_type: ProfileType::none(),
            sack: None,
            restrictions: None,
            create_time: None,
            embedders: None,
        }
    }

    /// Gives this traversal the embedders its `embed` steps call (book page "Vectors", section
    /// "Embeddings"). Without it the traversal uses the default registry, which holds only the
    /// built-in `hash-ngram/1`.
    ///
    /// ```
    /// use std::sync::Arc;
    /// use graphersal::prelude::*;
    /// use graphersal::vector::{Embedders, HashNgram};
    ///
    /// let small = HashNgram::with_dimension("acme/small", 8).unwrap();
    /// let embedders = Embedders::builder()
    ///     .with(Arc::new(small))
    ///     .unwrap()
    ///     .default_name("acme/small")
    ///     .unwrap()
    ///     .build();
    /// let graph = GraphSource::tinkerpop_modern();
    /// let g = graph.read();
    /// let vector = g
    ///     .traversal()
    ///     .with_embedders(Arc::new(embedders))
    ///     .v(None::<()>)
    ///     .embed("name", None)
    ///     .next()
    ///     .unwrap()
    ///     .unwrap();
    /// assert_eq!(vector.as_vector().map(|v| v.dimension()), Some(8));
    /// ```
    pub fn with_embedders(mut self, embedders: std::sync::Arc<crate::value::Embedders>) -> Self {
        self.embedders = Some(embedders);
        self
    }

    /// Pins the time `defaultFunction: "now"` stores (and `"today"` takes its UTC date from) for
    /// every element this traversal creates. Without it the time is the start of the storage's
    /// outermost open unit
    /// ([`GraphStorage::unit_started_at`](crate::storage::GraphStorage::unit_started_at): with
    /// the implicit unit of a traversal, its start), else the system clock. For deterministic
    /// tests and for hosts that keep one clock (the script DSL pins the evaluation's `now()`,
    /// which hosts pin with `script::pin_now`).
    ///
    /// ```
    /// use graphersal::prelude::*;
    /// use graphersal::temporal::DateTime;
    /// let mut graph = TraversalGraph::new();
    /// let schema = GraphSchema::from_json(
    ///     r#"{"mode": "open", "vertices": {"event": {"schema": {"type": "object",
    ///         "properties": {"at": {"type": "string", "format": "date-time",
    ///                               "defaultFunction": "now"}}}}}}"#,
    /// )
    /// .unwrap();
    /// graph.traversal_mut().set_schema(schema).unwrap();
    /// let at: DateTime = "2026-10-15T12:00:00Z".parse().unwrap();
    /// graph.traversal_mut().with_now(at).add_v("event").iterate().unwrap();
    /// let stored = graph.traversal().v(None::<()>).values("at").next().unwrap();
    /// assert_eq!(stored, Some(ElementProperty::Temporal(at.into())));
    /// ```
    pub fn with_now(mut self, now: crate::value::temporal::DateTime) -> Self {
        self.create_time = Some(now);
        self
    }

    /// Checks this traversal with `authorizer`: every execution first asks it for every
    /// [`AccessRequest`](crate::graph::AccessRequest) of the plan as written (child traversals
    /// included) and fails with [`TraverserError::PermissionDenied`] on the first denial,
    /// before anything runs. The options set so far are the host's presets and stay allowed;
    /// an administrative `with(..)` set afterwards is an `Update` on
    /// [`Resource::Option`](crate::graph::Resource::Option).
    ///
    /// Without it everything is allowed (the Rust API's default); this is for a host that
    /// hands a source to code it does not trust. The script DSL attaches the same check to
    /// every traversal a script runs.
    ///
    /// ```
    /// use std::sync::Arc;
    /// use graphersal::prelude::*;
    /// use graphersal::auth::AccessPolicy;
    /// let graph = GraphSource::tinkerpop_modern();
    /// let mut g = graph.write();
    /// let denied = g
    ///     .traversal_mut()
    ///     .with_authorizer(Arc::new(AccessPolicy::read_only()))
    ///     .add_v("person")
    ///     .to_list()
    ///     .unwrap_err();
    /// // The denial names the step (`at #0`); its cause is the permission error.
    /// assert!(matches!(denied.root_cause(), TraverserError::PermissionDenied { .. }));
    /// assert_eq!(g.vertex_count(), 6);
    /// ```
    pub fn with_authorizer(self, authorizer: std::sync::Arc<dyn crate::graph::Authorizer>) -> Self {
        let preset_options = std::sync::Arc::new(self.options.clone());
        self.restricted(crate::traversal::permission_check::Restrictions {
            authorizer,
            private_graph: false,
            preset_options,
            required: Vec::new(),
        })
    }

    /// Attaches a prepared restriction (the script DSL's entry).
    pub(crate) fn restricted(
        mut self,
        restrictions: crate::traversal::permission_check::Restrictions,
    ) -> Self {
        self.restrictions = Some(std::sync::Arc::new(restrictions));
        self
    }

    /// Gives every traverser the sack `initial` (TinkerPop `withSack(initial)`).
    ///
    /// Traversers generated by a start step (`v()`, `e()`, `inject()`, a first `add_v()`) and by
    /// a reducing barrier (`count()`, `sum()`, `fold()`, `group()`, `cap()`, ...) start with
    /// `initial`; every traverser derived from another keeps the sack of its parent, and child
    /// traversals see the sack of the traverser they run for. Read it with `sack()`. Sacks are
    /// immutable values: equal scalar sacks share one stored copy, so the sack costs four bytes
    /// per traverser. Without `with_sack` the sack of every traverser is `null`.
    ///
    /// A vertex or edge is not a valid sack value: execution fails with a cast error. Calling
    /// `with_sack` again replaces the initial value.
    ///
    /// Deviations from TinkerPop (book page "Sack and operators"): no `Supplier` or split
    /// operator form, so a container sack is shared and never cloned per traverser; a container
    /// sack never merges with another traverser (each stored container is distinct).
    ///
    /// ```
    /// use graphersal::prelude::*;
    /// let graph = GraphSource::tinkerpop_modern();
    /// let g = graph.read();
    /// let sacks = g.traversal().with_sack(5).v(None::<()>).sack().to_list().unwrap();
    /// assert_eq!(sacks.len(), 6);
    /// ```
    pub fn with_sack<T: Into<ElementProperty>>(mut self, initial: T) -> Self {
        self.sack = Some(crate::value::SackSpec {
            initial: initial.into(),
            merge: None,
        });
        self
    }

    /// Gives every traverser the sack `initial` and combines the sacks of equal traversers with
    /// `merge` when they merge (TinkerPop `withSack(initial, Operator)`; the operator is the
    /// **merge** operator, not a split).
    ///
    /// Merging becomes part of the result: equal traversers (same object, same path) merge
    /// whatever their sacks are, the merged sack is `merge(first, next)` in encounter order, and
    /// the bulks add up. To keep the result independent of where the optimizer happens to put
    /// merge points, only an explicit `barrier()` / `barrier(n)` / `barrier_with(..)` merges; the
    /// `repeat()` frontier and the `lazy_barrier` rule pass through. A repeat body that wants
    /// merging writes `repeat(__.out().barrier())`. `bulk.merge=false` disables the merge at
    /// the barrier as well, so the result of such a plan then differs. Plans with full paths
    /// (`path()`) and mutating plans never merge, so the operator never combines there.
    ///
    /// See `with_sack` for the rest of the sack semantics and the book page "Sack and
    /// operators" for the deviations from TinkerPop.
    ///
    /// ```
    /// use graphersal::prelude::*;
    /// let graph = GraphSource::tinkerpop_modern();
    /// let g = graph.read();
    /// // lop is reached by three edges: its sack is 3 and its bulk 3, so it is read three times.
    /// let sacks = g
    ///     .traversal()
    ///     .with_sack_merge(1, Operator::Sum)
    ///     .v(None::<()>)
    ///     .out(None)
    ///     .barrier()
    ///     .sack()
    ///     .to_list()
    ///     .unwrap();
    /// assert_eq!(sacks.len(), 6);
    /// ```
    pub fn with_sack_merge<T: Into<ElementProperty>>(
        mut self,
        initial: T,
        merge: crate::value::Operator,
    ) -> Self {
        self.sack = Some(crate::value::SackSpec {
            initial: initial.into(),
            merge: Some(merge),
        });
        self
    }

    /// `withBulk(bool)`: `with_bulk(false)` sets the option `bulk.one` (TinkerPop `ONE_BULK`),
    /// `with_bulk(true)` clears it.
    ///
    /// Under `bulk.one` a merge still collapses equal traversers, but the merged traverser keeps
    /// bulk 1, so duplicates are emitted once. A traverser with a sack and no merge operator
    /// never merges, and only an explicit `barrier()` merges (the automatic merge points are
    /// off). This is the one place where merging is visible on purpose.
    pub fn with_bulk(self, bulk: bool) -> Self {
        self.with(crate::traversal::context::BULK_ONE, !bulk)
    }

    #[cfg(test)]
    pub(crate) fn take_steps(mut self) -> Vec<GraphStep<'g, S>> {
        std::mem::take(&mut self.steps)
    }

    /// Stops the traversal from capturing the query as written before optimization.
    ///
    /// Diagnostics are on by default: when a step fails, the error points at the query the user
    /// wrote, not only at the plan the optimizer produced. Tracking that provenance costs a step
    /// rendering and a couple of small allocations per rewritten traversal level — negligible for
    /// real traversals, but measurable for very small queries executed in a tight loop, which is
    /// the only case where turning it off is worth the loss.
    pub fn without_diagnostics(mut self) -> Self {
        self.strategy = self.strategy.and(TraversalStrategy::Diagnostics.not());
        self
    }

    pub(crate) fn push_step(mut self, step: GraphStep<'g, S>) -> Self {
        crate::traversal::push_step_into(&mut self.steps, step, "g");
        self
    }

    /// Sets a configuration option on the traversal source.
    ///
    /// Setting a key again replaces its value. Unknown keys are ignored. The recognised keys:
    ///
    /// The host that constructed this traversal's underlying storage may have locked any of
    /// these keys via [`crate::graph::ExecutionPolicy::with_locked`] — see the "Execution Options
    /// Reference" book chapter. A locked key's value here is never honored: `with()` still
    /// accepts the call (it has no error channel), but the traversal fails with
    /// [`crate::error::TraverserError::LockedOption`] when executed, before this key's own value
    /// is even inspected.
    ///
    /// - `evaluationTimeout` (as in TinkerPop): the wall-clock budget of one execution in
    ///   milliseconds, covering every nested traversal. `0` or no value means no timeout. On
    ///   expiry execution stops cooperatively with [`crate::error::TraverserError::Timeout`]; the budget is
    ///   checked between steps and every 1024 traversers, never inside a single storage call.
    ///   A value that is not a non-negative integer fails with
    ///   [`crate::error::TraverserError::InvalidOption`] when the traversal is executed, since `with()` itself
    ///   cannot fail. A mutating step is never interrupted; a timeout rolls back every change
    ///   the traversal made (implicit auto-commit, see [`Self::to_list`]).
    /// - `path.analysis` (boolean, default `true`): record only the path positions that a step
    ///   reads; `false` records full paths.
    /// - `io.graphml.beautify` / `io.beautify` / `beautify` (boolean): indent GraphML exports.
    /// - `optimizer.disabled` (array of strings): names of optimizer rules (see the "Query
    ///   Optimizer" book chapter for the full list and each rule's default on/off state) to leave
    ///   out of this execution's plan —
    ///   useful to compare a query's plan/performance with and without a specific rule, or to
    ///   work around a rule producing an unwanted plan for one query. The reserved name `"all"`
    ///   disables every known rule at once (`g.with("optimizer.disabled", ["all"])`), without
    ///   hand-listing them. Disabling any subset of these rules never changes a
    ///   query's *result*, only its *plan*. An unknown name fails
    ///   with [`crate::error::TraverserError::InvalidOption`] when the traversal is executed, same
    ///   timing as `evaluationTimeout`'s validation.
    /// - `optimizer.enabled` (array of strings): the inverse of `optimizer.disabled` — force-enables
    ///   named rules that default to off for this execution (there are none yet; every rule this
    ///   crate ships defaults to on). A name present in both `optimizer.enabled` and
    ///   `optimizer.disabled` at once fails with `InvalidOption` as a contradictory request.
    ///   `.profile()`/`.profile_with()` render the rules that actually fired for a query as an
    ///   "Optimizer rules applied" line, naming them the same way.
    /// - `render.max_rows` / `render.max_items` (positive integers): read only by the script
    ///   DSL's display paths, as the session default of the display limits (see
    ///   [`crate::traversal::render::RenderOptions`]). The Rust API takes its limits from the
    ///   `RenderOptions` passed to [`Self::visualize_with`] instead.
    ///
    /// ```rust
    /// # use graphersal::prelude::*;
    /// let graph = TraversalGraph::tinkerpop_modern();
    /// let count = graph
    ///     .traversal()
    ///     .with("evaluationTimeout", 5000)
    ///     .v(None)
    ///     .out(None)
    ///     .count()
    ///     .next()
    ///     .unwrap();
    /// assert!(count.is_some());
    /// ```
    ///
    /// ```rust
    /// # use graphersal::prelude::*;
    /// let graph = TraversalGraph::tinkerpop_modern();
    /// // Disable filter_reorder for this query: the result is unaffected, only the plan is.
    /// let names = graph
    ///     .traversal()
    ///     .with("optimizer.disabled", vec!["filter_reorder"])
    ///     .v(None)
    ///     .has_label("person")
    ///     .values("name")
    ///     .to_list()
    ///     .unwrap();
    /// assert_eq!(names.len(), 4);
    /// ```
    pub fn with<K: Into<CompactString>, V: Into<ElementProperty>>(
        mut self,
        key: K,
        value: V,
    ) -> Self {
        self.options.insert(key.into(), value.into());
        self
    }

    /// Resolves a boolean option with key hierarchy and fallback logic.
    pub fn get_bool_option(&self, keys: &[&str], default: bool) -> bool {
        for key in keys {
            if let Some(val) = self.options.get(*key)
                && let Some(b) = val.as_boolean()
            {
                return b;
            }
        }
        default
    }

    /// Exports this traversal's graph as GraphML to the file at `path`.
    #[cfg(feature = "io")]
    pub fn export_graphml(&self, path: &str) -> Result<(), IOError>
    where
        S: GraphExport,
    {
        self.storage.export_graphml(path)
    }

    /// Exports this traversal's graph as GraphML to `writer`.
    #[cfg(feature = "io")]
    pub fn export_graphml_writer<W: std::io::Write>(&self, writer: W) -> Result<(), IOError>
    where
        S: GraphExport,
    {
        let beautify =
            self.get_bool_option(&["io.graphml.beautify", "io.beautify", "beautify"], false);
        self.storage
            .export_graphml_writer_with_options(writer, beautify)
    }

    /// Exports this traversal's graph as GraphSON 3.0 (line form) to the file at `path`.
    #[cfg(feature = "io")]
    pub fn export_graphson(&self, path: &str) -> Result<(), IOError>
    where
        S: GraphExport,
    {
        self.storage.export_graphson(path)
    }

    /// Exports this traversal's graph as GraphSON 3.0 (line form) to `writer`.
    #[cfg(feature = "io")]
    pub fn export_graphson_writer<W: std::io::Write>(&self, writer: W) -> Result<(), IOError>
    where
        S: GraphExport,
    {
        self.storage.export_graphson_writer(writer)
    }

    /// Asks the attached authorizer (if any) for `action` on the schema.
    fn authorize_schema(
        &self,
        operation: &str,
        action: crate::graph::Action<'_>,
    ) -> TraverserResult<()> {
        if let Some(restrictions) = &self.restrictions {
            restrictions.authorize(
                operation,
                &crate::graph::AccessRequest::new(action, crate::graph::Resource::Schema),
            )?;
        }
        Ok(())
    }

    /// The schema the graph stores, `None` without one; never an inferred schema (that is
    /// [`Self::infer_schema`]). Not a traversal step: a plain call, like [`Self::statistics`].
    /// Asks an attached authorizer for `Read` on the schema.
    ///
    /// ```
    /// use graphersal::prelude::*;
    /// let graph = TraversalGraph::tinkerpop_modern();
    /// assert!(graph.traversal().get_schema().unwrap().is_none());
    /// ```
    pub fn get_schema(&self) -> TraverserResult<Option<GraphSchema>> {
        self.authorize_schema("get_schema", crate::graph::Action::Read)?;
        Ok(self.storage.get_schema()?)
    }

    /// Replaces the stored schema. In mode `open`/`closed` the existing data is validated
    /// immediately; on violations nothing changes and the error carries the aggregated report
    /// ([`SchemaError::ExistingDataViolatesSchema`](crate::error::SchemaError)). The empty
    /// schema ([`GraphSchema::is_empty`]) removes the schema.
    ///
    /// Create-time defaults (`default`/`defaultFunction`, see
    /// [`Annotations`](crate::schema::Annotations)) are checked first: each must give a valid
    /// value of its top-level property, and `defaultFunction` sits only on a top-level property
    /// ([`SchemaError::InvalidSchema`](crate::error::SchemaError) otherwise). They apply to
    /// elements created from now on; existing elements are never filled in (no backfill).
    ///
    /// Runs in the current unit (inside a transaction or a whole-script unit it is rolled back
    /// with it), or as a unit of its own: recorded as a
    /// [`Mutation::SetSchema`](crate::graph::Mutation::SetSchema), captured and seen by the
    /// commit hooks (which may veto it). A read-only source (`graph.traversal()`) rejects it; use
    /// `traversal_mut()`. Asks an attached authorizer for `Update` on the schema.
    ///
    /// ```
    /// use graphersal::prelude::*;
    /// let mut graph = TraversalGraph::tinkerpop_modern();
    /// let schema = GraphSchema::from_json(
    ///     r#"{"mode": "open", "vertices": {"person": {"schema": {"type": "object",
    ///         "properties": {"age": {"type": "integer", "minimum": 0}}}}}}"#,
    /// )
    /// .unwrap();
    /// graph.traversal_mut().set_schema(schema).unwrap();
    /// assert!(graph.traversal().add_v("person").property("age", -1).to_list().is_err());
    /// ```
    ///
    /// **Freeze the structure you have**: the inferred schema with a mode
    /// ([`GraphSchema::with_mode`]). It always applies to the data it was inferred from, in
    /// `Open` and in `Closed` mode; the one exception is `Closed` with vertices or edges that
    /// have no label (nothing declares them), which fails with a report naming them.
    ///
    /// ```
    /// use graphersal::prelude::*;
    /// let mut graph = TraversalGraph::tinkerpop_modern();
    /// let frozen = graph.traversal().infer_schema().unwrap().with_mode(SchemaMode::Closed);
    /// graph.traversal_mut().set_schema(frozen).unwrap();
    /// assert!(graph.traversal().add_v("robot").to_list().is_err()); // undeclared label
    /// ```
    pub fn set_schema(&mut self, schema: GraphSchema) -> TraverserResult<()> {
        self.authorize_schema("set_schema", crate::graph::Action::Update)?;
        crate::graph::schema::defaults::check_defaults(&schema)?;
        self.atomically(|this| {
            this.storage.set_schema(schema)?;
            Ok(())
        })
    }

    /// Applies a JSON Merge Patch (RFC 7396) to the stored schema (`{}` without one) and stores
    /// the result, exactly like [`Self::set_schema`]; returns the new schema. Asks an attached
    /// authorizer for `Update` on the schema.
    pub fn patch_schema(&mut self, patch: &SchemaPatch) -> TraverserResult<GraphSchema> {
        self.authorize_schema("patch_schema", crate::graph::Action::Update)?;
        let schema = patch.apply(self.storage.get_schema()?.as_ref())?;
        crate::graph::schema::defaults::check_defaults(&schema)?;
        let stored = schema.clone();
        self.atomically(|this| {
            this.storage.set_schema(stored)?;
            Ok(())
        })?;
        Ok(schema)
    }

    /// Dry run of [`Self::set_schema`]: the violations of the current data under `schema`
    /// (grouped by rule, the same report a failing `set_schema` carries) and the
    /// [diff](SchemaDiff) from the stored schema with its compatibility. Nothing is applied.
    /// Asks an attached authorizer for `Read` on the schema.
    pub fn validate_schema(&self, schema: &GraphSchema) -> TraverserResult<SchemaValidation> {
        self.authorize_schema("validate_schema", crate::graph::Action::Read)?;
        crate::graph::schema::defaults::check_defaults(schema)?;
        self.validation(schema.clone())
    }

    /// [`Self::validate_schema`] of the stored schema patched with `patch` (see
    /// [`Self::patch_schema`]); nothing is applied.
    pub fn validate_schema_patch(&self, patch: &SchemaPatch) -> TraverserResult<SchemaValidation> {
        self.authorize_schema("validate_schema_patch", crate::graph::Action::Read)?;
        let schema = patch.apply(self.storage.get_schema()?.as_ref())?;
        crate::graph::schema::defaults::check_defaults(&schema)?;
        self.validation(schema)
    }

    fn validation(&self, schema: GraphSchema) -> TraverserResult<SchemaValidation> {
        let violations = crate::graph::schema::apply::scan_storage(&self.storage, &schema)?;
        let diff = SchemaDiff::between(self.storage.get_schema()?.as_ref(), &schema);
        Ok(SchemaValidation {
            schema,
            violations,
            diff,
        })
    }

    /// The schema the graph's data satisfies (see
    /// [`SchemaInferer`](crate::graph::schema::SchemaInferer)): mode `none`, every label, every
    /// key required that all elements of a label have. Stores nothing. Asks an attached
    /// authorizer for `Read` on the schema. (The step of the same name infers over a stream:
    /// [`Self::infer_schema_step`].)
    pub fn infer_schema(&self) -> TraverserResult<GraphSchema> {
        self.authorize_schema("infer_schema", crate::graph::Action::Read)?;
        Ok(self.storage.infer_schema()?)
    }

    /// The changes from the stored schema to `schema` and whether they are backward compatible
    /// (see [`SchemaDiff`]). Looks at no data; asks an attached authorizer for `Read` on the
    /// schema.
    pub fn diff_schema(&self, schema: &GraphSchema) -> TraverserResult<SchemaDiff> {
        self.authorize_schema("diff_schema", crate::graph::Action::Read)?;
        Ok(SchemaDiff::between(
            self.storage.get_schema()?.as_ref(),
            schema,
        ))
    }

    /// Asks the attached authorizer (if any) for `action` on a catalog definition.
    fn authorize_definition(
        &self,
        operation: &str,
        action: crate::graph::Action<'_>,
        kind: Option<DefinitionKind>,
        name: Option<&str>,
    ) -> TraverserResult<()> {
        if let Some(restrictions) = &self.restrictions {
            restrictions.authorize(
                operation,
                &crate::graph::AccessRequest::new(
                    action,
                    crate::graph::Resource::definition(kind, name),
                ),
            )?;
        }
        Ok(())
    }

    /// The definitions of the graph's catalog (saved queries, ...; see
    /// [`crate::catalog`]), of `kind` or of every kind (`None`), by kind and then by
    /// name. Not a step: a plain call like [`Self::get_schema`]. Asks an attached authorizer
    /// for `Read` on the definitions of that kind.
    ///
    /// ```
    /// use graphersal::prelude::*;
    /// use graphersal::catalog::{DefinitionKind, QueryDefinition};
    /// let mut graph = TraversalGraph::tinkerpop_modern();
    /// let query = QueryDefinition::new("fn people() { g.v().has_label(\"person\") }");
    /// graph.traversal_mut().define_query("people", query).unwrap();
    /// let listed = graph.traversal().list_definitions(Some(DefinitionKind::QUERY)).unwrap();
    /// assert_eq!(listed[0].name(), "people");
    /// ```
    pub fn list_definitions(
        &self,
        kind: Option<DefinitionKind>,
    ) -> TraverserResult<Vec<Definition>> {
        self.authorize_definition("list_definitions", crate::graph::Action::Read, kind, None)?;
        Ok(self
            .storage
            .definitions()?
            .filter(|definition| kind.is_none_or(|kind| definition.kind() == kind))
            .cloned()
            .collect())
    }

    /// The definition of `kind` named `name`, `None` when there is none. Asks an attached
    /// authorizer for `Read` on it.
    pub fn get_definition(
        &self,
        kind: DefinitionKind,
        name: &str,
    ) -> TraverserResult<Option<Definition>> {
        self.authorize_definition(
            "get_definition",
            crate::graph::Action::Read,
            Some(kind),
            Some(name),
        )?;
        Ok(self.storage.definition(kind, name)?.cloned())
    }

    /// Stores `definition` in the catalog, replacing the one of the same kind and name. It is
    /// [validated](Definition::validate) first: name, folder, sizes, parameters with their
    /// schemas and defaults. The source of a saved query is NOT compiled here (the core has no
    /// Rhai): the script layer compiles it when a query is defined through the DSL, and every
    /// call compiles it again.
    ///
    /// Runs in the current unit or as a unit of its own, like [`Self::set_schema`]: recorded
    /// as a [`Mutation::SetDefinition`](crate::graph::Mutation::SetDefinition), captured and
    /// seen by the commit hooks (which may veto it). A read-only source rejects it. Asks an
    /// attached authorizer for `Create` on the definition when it is new, `Update` when it
    /// replaces one.
    pub fn set_definition(&mut self, definition: Definition) -> TraverserResult<()> {
        let action = if self
            .storage
            .definition(definition.kind(), definition.name())?
            .is_some()
        {
            crate::graph::Action::Update
        } else {
            crate::graph::Action::Create
        };
        self.authorize_definition(
            "set_definition",
            action,
            Some(definition.kind()),
            Some(definition.name()),
        )?;
        self.atomically(|this| {
            this.storage.set_definition(definition)?;
            Ok(())
        })
    }

    /// [`Self::set_definition`] of the saved query `query` named `name`.
    pub fn define_query(&mut self, name: &str, query: QueryDefinition) -> TraverserResult<()> {
        self.set_definition(Definition::query(name, query))
    }

    /// Removes the definition of `kind` named `name`; `false` when there is none. A unit like
    /// [`Self::set_definition`]; asks an attached authorizer for `Delete` on it.
    pub fn remove_definition(&mut self, kind: DefinitionKind, name: &str) -> TraverserResult<bool> {
        self.authorize_definition(
            "remove_definition",
            crate::graph::Action::Delete,
            Some(kind),
            Some(name),
        )?;
        self.atomically(|this| Ok(this.storage.remove_definition(kind, name)?))
    }

    /// [`Self::set_definition`] of the compression rule `rule` named `name` (book page
    /// "Compressed Properties"): strings at the rule's path are kept compressed in memory. The
    /// explicit pass runs in the same unit: every value of the rule's `(element, label)` is
    /// re-encoded (compressed when it qualifies, decompressed when it no longer does). A second
    /// rule with the same element kind, label and path fails with
    /// [`DefinitionError::Conflict`](crate::error::DefinitionError::Conflict).
    ///
    /// ```
    /// use graphersal::prelude::*;
    /// use graphersal::{catalog::CompressionDefinition, storage::ElementKind};
    ///
    /// let mut graph = TraversalGraph::new();
    /// let rule = CompressionDefinition::new(ElementKind::Vertex, "customer", "cv").unwrap();
    /// graph.traversal_mut().define_compression("customer_cv", rule).unwrap();
    /// let cv = "Senior engineer, led the platform team. ".repeat(20);
    /// graph.traversal_mut().add_v("customer").property("cv", cv.as_str()).to_list().unwrap();
    /// // Reads are plain.
    /// let read = graph.traversal().v(None).values("cv").to_list().unwrap();
    /// assert_eq!(read, vec![ElementProperty::from(cv.as_str())]);
    /// assert_eq!(graph.memory_usage().unwrap().unwrap().compressed_values, 1);
    /// ```
    pub fn define_compression(
        &mut self,
        name: &str,
        rule: crate::graph::CompressionDefinition,
    ) -> TraverserResult<()> {
        self.set_definition(Definition::compression(name, rule))
    }

    /// Re-runs the explicit pass of the compression rule `name`: rebuilds its dictionary from
    /// the current values and re-encodes them. Changes no logical data, so a unit holding only
    /// a `recompress` commits nothing. Fails with
    /// [`DefinitionError::NotFound`](crate::error::DefinitionError::NotFound) when there is no
    /// such rule; asks an attached authorizer for `Update` on it.
    pub fn recompress(&mut self, name: &str) -> TraverserResult<()> {
        self.authorize_definition(
            "recompress",
            crate::graph::Action::Update,
            Some(DefinitionKind::COMPRESSION),
            Some(name),
        )?;
        if self.storage.recompress(name)? {
            Ok(())
        } else if let Some(reason) = self
            .storage
            .definition(DefinitionKind::COMPRESSION, name)?
            .and_then(|definition| definition.as_compression())
            .and_then(|rule| rule.unserved_reason())
        {
            // A rule of a newer version: kept, never applied (format spec 16).
            Err(
                crate::error::GraphError::from(crate::error::DefinitionError::InvalidField {
                    name: name.to_string(),
                    field: "codec".to_string(),
                    reason,
                })
                .into(),
            )
        } else {
            Err(
                crate::error::GraphError::from(crate::error::DefinitionError::NotFound {
                    kind: DefinitionKind::COMPRESSION.to_string(),
                    name: name.to_string(),
                })
                .into(),
            )
        }
    }

    /// What the compression rule `name` holds right now (`None`: no such rule). Asks an
    /// attached authorizer for `Read` on it.
    pub fn compression_stats(
        &self,
        name: &str,
    ) -> TraverserResult<Option<crate::graph::CompressionStats>> {
        self.authorize_definition(
            "compression_stats",
            crate::graph::Action::Read,
            Some(DefinitionKind::COMPRESSION),
            Some(name),
        )?;
        Ok(self.storage.compression_stats(name)?)
    }

    /// Estimates compression rules for one `(element, label, path)` before defining one (book
    /// page "Compressed Properties", section "Estimating the gain"): every candidate of
    /// `request` (default: `lz4` with and without a dictionary at several thresholds, plus the
    /// current rule's parameters) measured over the current values, with what each would save
    /// and how fast it reads. Read-only: no unit, no commit, the catalog and the data
    /// unchanged. Asks an attached authorizer for `Read` on the data of that element kind and
    /// label. An invalid request fails with its
    /// [`DefinitionError`](crate::error::DefinitionError); a storage without compressed
    /// properties with [`GraphError::Unsupported`](crate::error::GraphError::Unsupported); a
    /// cancelled one ([`CancelToken`](crate::CancelToken), the request's or the running
    /// thread's) with [`TraverserError::Cancelled`](crate::TraverserError::Cancelled).
    ///
    /// ```
    /// use graphersal::prelude::*;
    /// use graphersal::catalog::{CompressionDefinition, CompressionEstimateRequest};
    /// use graphersal::storage::ElementKind;
    ///
    /// let mut graph = TraversalGraph::new();
    /// let cv = "Senior engineer, led the platform team. ".repeat(20);
    /// graph.traversal_mut().add_v("customer").property("cv", cv.as_str()).to_list().unwrap();
    /// let request = CompressionEstimateRequest::new(ElementKind::Vertex, "customer", "cv").unwrap();
    /// let estimate = graph.traversal().estimate_compression(&request).unwrap();
    /// let best = estimate.candidates[estimate.best.unwrap()].clone();
    /// // What the estimate promised is what the rule holds.
    /// let rule = CompressionDefinition::new(ElementKind::Vertex, "customer", "cv").unwrap();
    /// graph.traversal_mut().define_compression("cv", best.candidate.apply_to(rule)).unwrap();
    /// let stats = graph.traversal().compression_stats("cv").unwrap().unwrap();
    /// assert_eq!(stats.stored_bytes, best.stored_bytes);
    /// ```
    pub fn estimate_compression(
        &self,
        request: &crate::graph::CompressionEstimateRequest,
    ) -> TraverserResult<crate::graph::CompressionEstimate> {
        if let Some(restrictions) = &self.restrictions {
            restrictions.authorize(
                "estimate_compression",
                &crate::graph::AccessRequest::new(
                    crate::graph::Action::Read,
                    crate::graph::Resource::data(Some(request.element()), Some(request.label())),
                ),
            )?;
        }
        request.validate().map_err(crate::error::GraphError::from)?;
        let estimate = match (
            request.cancel_token(),
            crate::traversal::cancel::CancelToken::current(),
        ) {
            (None, Some(token)) => self
                .storage
                .estimate_compression(&request.clone().with_cancel(token))?,
            _ => self.storage.estimate_compression(request)?,
        };
        if estimate.cancelled {
            return Err(TraverserError::Cancelled {
                elapsed_ms: estimate.elapsed_micros / 1000,
            }
            .into());
        }
        Ok(estimate)
    }

    /// Declares the property index `name` (book page "Property Indexes"): the `element`s
    /// labelled `label`, ordered by their top-level property `key`. A `v()`/`e()` with that
    /// label and a `has()` on that key (equality, `P.within`, a range) then reads the index
    /// instead of every element of the label; `.profile()` shows `[index: ..]` on the source.
    /// The index is built in the same unit, kept exact on every write, rollback and replay, and
    /// rebuilt from the data when a graph is loaded (its entries are never stored).
    ///
    /// A [`set_definition`](Self::set_definition) of a [`Definition::property_index`] (a
    /// catalog entry of kind [`DefinitionKind::PROPERTY_INDEX`]); a second index on the same
    /// `(element, label, key, type)` fails with
    /// [`DefinitionError::Conflict`](crate::error::DefinitionError::Conflict). A storage
    /// without [`property_indexes`](crate::graph::StorageCapabilities::property_indexes)
    /// refuses it with [`GraphError::Unsupported`](crate::error::GraphError::Unsupported).
    /// Asks an attached authorizer for `Create` on it (`Update` when it replaces one). The
    /// index is first checked by
    /// [`PropertyIndexDefinition::check_create`](crate::graph::PropertyIndexDefinition::check_create)
    /// against the stored schema: an unknown index type, or a key the schema declares as values
    /// the type cannot hold (a vector, an array, an object), is refused.
    ///
    /// ```
    /// use graphersal::prelude::*;
    /// use graphersal::{catalog::PropertyIndexDefinition, storage::ElementKind};
    ///
    /// let mut graph = TraversalGraph::tinkerpop_modern();
    /// let index = PropertyIndexDefinition::new(ElementKind::Vertex, "person", "age");
    /// graph.traversal_mut().create_index("person_age", index).unwrap();
    /// let names = graph
    ///     .traversal()
    ///     .v(None)
    ///     .has_label("person")
    ///     .has("age", 29)
    ///     .values("name")
    ///     .to_list()
    ///     .unwrap();
    /// assert_eq!(names, vec![ElementProperty::from("marko")]);
    /// ```
    pub fn create_index(
        &mut self,
        name: &str,
        index: crate::graph::PropertyIndexDefinition,
    ) -> TraverserResult<()> {
        if !self.storage.capabilities()?.property_indexes {
            return Err(crate::error::GraphError::unsupported("property indexes").into());
        }
        let action = if self
            .storage
            .definition(DefinitionKind::PROPERTY_INDEX, name)?
            .is_some()
        {
            crate::graph::Action::Update
        } else {
            crate::graph::Action::Create
        };
        self.authorize_definition(
            "create_index",
            action,
            Some(DefinitionKind::PROPERTY_INDEX),
            Some(name),
        )?;
        index
            .check_create(name, self.storage.get_schema()?.as_ref())
            .map_err(crate::error::GraphError::from)?;
        self.set_definition(Definition::property_index(name, index))
    }

    /// Drops the property index `name`; `false` when there is none. Asks an attached
    /// authorizer for `Delete` on it.
    pub fn drop_index(&mut self, name: &str) -> TraverserResult<bool> {
        self.remove_definition(DefinitionKind::PROPERTY_INDEX, name)
    }

    /// The property indexes of the catalog, by name, each with what it holds right now
    /// (`None`: the storage does not serve it). Asks an attached authorizer for `Read` on them.
    pub fn list_indexes(
        &self,
    ) -> TraverserResult<Vec<(Definition, Option<crate::graph::PropertyIndexStats>)>> {
        self.authorize_definition(
            "list_indexes",
            crate::graph::Action::Read,
            Some(DefinitionKind::PROPERTY_INDEX),
            None,
        )?;
        let served = self.storage.property_indexes()?;
        Ok(self
            .storage
            .definitions()?
            .filter(|definition| definition.kind() == DefinitionKind::PROPERTY_INDEX)
            .map(|definition| {
                let stats = served
                    .iter()
                    .find(|info| info.name == definition.name())
                    .map(|info| info.stats);
                (definition.clone(), stats)
            })
            .collect())
    }

    /// Moves the saved query `name` into `folder` (`None` or `""`: the root). Callers name a
    /// query without its folder, so moving never breaks one. Fails with
    /// [`DefinitionError::NotFound`](crate::error::DefinitionError::NotFound) when there is no
    /// such query; asks an attached authorizer for `Update` on it.
    pub fn move_query(&mut self, name: &str, folder: Option<&str>) -> TraverserResult<()> {
        let folder = folder.map(str::to_string);
        self.update_query("move_query", name, |query| query.set_folder(folder))
    }

    /// Sets (or with `None` / `""` removes) the description of the saved query `name`. Fails
    /// like [`Self::move_query`]; asks an attached authorizer for `Update` on it.
    pub fn describe_query(&mut self, name: &str, description: Option<&str>) -> TraverserResult<()> {
        let description = description.map(str::to_string);
        self.update_query("describe_query", name, |query| {
            query.set_description(description)
        })
    }

    /// Changes the stored saved query `name` with `change` and stores it again.
    fn update_query(
        &mut self,
        operation: &str,
        name: &str,
        change: impl FnOnce(&mut QueryDefinition),
    ) -> TraverserResult<()> {
        self.authorize_definition(
            operation,
            crate::graph::Action::Update,
            Some(DefinitionKind::QUERY),
            Some(name),
        )?;
        let mut definition = self
            .storage
            .definition(DefinitionKind::QUERY, name)?
            .cloned()
            .ok_or_else(|| crate::error::DefinitionError::NotFound {
                kind: DefinitionKind::QUERY.to_string(),
                name: name.to_string(),
            })
            .map_err(crate::error::GraphError::from)?;
        let Some(query) = definition.as_query_mut() else {
            return Err(crate::error::GraphError::from(
                crate::error::DefinitionError::InvalidField {
                    name: name.to_string(),
                    field: "payload".to_string(),
                    reason: "it is kept opaque (this version could not decode it), so it \
                             cannot be changed"
                        .to_string(),
                },
            )
            .into());
        };
        change(query);
        self.atomically(|this| {
            this.storage.set_definition(definition)?;
            Ok(())
        })
    }
    /// An on-demand snapshot of the underlying storage's approximate memory footprint, or `None`
    /// if the backing [`GraphStorage`] does not support introspection (the trait default); the
    /// storage's error when it cannot measure itself now.
    /// A thin pass-through to [`GraphStorage::memory_usage`], so any storage backend plugged into
    /// this source gets this uniformly, not just [`crate::TraversalGraph`] used through a
    /// concrete type.
    ///
    /// This is a plain method call, not a traversal step: it returns a single value, computed
    /// once, never a stream of traversers, so it is never chained with a terminal like
    /// `.to_list()`/`.next()`.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = TraversalGraph::new();
    /// let g = graph.traversal();
    /// let stats = g.memory_usage().unwrap().expect("TraversalGraph always supports introspection");
    /// assert!(stats.total_bytes() < u64::MAX);
    /// ```
    pub fn memory_usage(&self) -> TraverserResult<Option<crate::graph::MemoryStats>> {
        Ok(self.storage.memory_usage()?)
    }

    /// The memory report (book page "Memory Usage", section "The memory report"): where the
    /// graph's memory goes, per element kind and label and per top-level property, with the
    /// property indexes, compression rules and, when the storage reports it
    /// ([`GraphStorage::memory_structure`]), slots and index bytes. One walk over the elements
    /// under the caller's read access, read-only; full or sampled
    /// ([`MemoryReportRequest`](crate::graph::MemoryReportRequest)). Works for any storage: the
    /// per-label and per-property parts use the read methods only.
    ///
    /// Asks an attached authorizer for `Read` on `Data`. Fails with
    /// [`GraphError::Unsupported`](crate::error::GraphError::Unsupported) for an invalid request
    /// and with [`TraverserError::Cancelled`](crate::TraverserError::Cancelled) when the
    /// request's token (or the running thread's [`CancelToken`](crate::CancelToken)) is
    /// cancelled.
    ///
    /// ```
    /// use graphersal::prelude::*;
    /// use graphersal::storage::{ElementKind, MemoryReportRequest, PropertyTarget};
    ///
    /// let graph = TraversalGraph::tinkerpop_modern();
    /// let report = graph.traversal().memory_report(&MemoryReportRequest::new()).unwrap();
    /// let name = PropertyTarget::top_level(ElementKind::Vertex, Some("person"), "name");
    /// let row = report.property(&name).unwrap();
    /// assert_eq!((row.elements, row.label_elements), (4, 4));
    /// assert_eq!(row.value_types[0].name, "string");
    /// // The rows add up to the property bytes of `memory_usage()`.
    /// assert_eq!(report.property_bytes(), graph.memory_usage().unwrap().unwrap().property_bytes);
    /// ```
    pub fn memory_report(
        &self,
        request: &crate::graph::MemoryReportRequest,
    ) -> TraverserResult<crate::graph::MemoryReport> {
        if let Some(restrictions) = &self.restrictions {
            restrictions.authorize(
                "memory_report",
                &crate::graph::AccessRequest::new(
                    crate::graph::Action::Read,
                    crate::graph::Resource::data(None, None),
                ),
            )?;
        }
        request.validate()?;
        let (report, cancelled) = match (
            request.cancel_token(),
            crate::traversal::cancel::CancelToken::current(),
        ) {
            (None, Some(token)) => crate::graph::memory_report::walk::compute(
                &self.storage,
                &request.clone().with_cancel(token),
            ),
            _ => crate::graph::memory_report::walk::compute(&self.storage, request),
        }?;
        if cancelled {
            return Err(TraverserError::Cancelled {
                elapsed_ms: report.elapsed_micros / 1000,
            }
            .into());
        }
        Ok(report)
    }

    /// The element counts the underlying storage keeps up to date, or `None` if the backing
    /// [`GraphStorage`] keeps none (the trait default); the storage's error when it cannot read
    /// them now. A thin pass-through to
    /// [`GraphStorage::statistics`]; like [`Self::memory_usage`] a plain method call, not a step.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = TraversalGraph::tinkerpop_modern();
    /// let g = graph.traversal();
    /// let stats = g.statistics().unwrap().expect("TraversalGraph keeps statistics");
    /// assert_eq!(stats.primary_vertex_label_count("software"), 2);
    /// ```
    pub fn statistics(&self) -> TraverserResult<Option<&crate::graph::GraphStatistics>> {
        Ok(self.storage.statistics()?)
    }

    /// Executes the traversal and returns its first result, or `None` if it yields nothing.
    pub fn next(self) -> TraverserResult<Option<ElementProperty>> {
        let list = self.to_list()?;
        Ok(list.into_iter().next())
    }

    /// Executes the traversal and materializes every result, except that a graph value (the
    /// `cap()` of a `subgraph()`) stays a shared handle. The script layer uses it, so `next()`
    /// can hand a real graph to a script instead of its `{"vertices", "edges"}` listing.
    #[cfg(feature = "script")]
    pub(crate) fn into_list_with_graphs(mut self) -> TraverserResult<Vec<ListItem>> {
        self.atomically(|this| {
            let traversers = this.internal_to_list_expanded("to_list")?;
            let mut results = Vec::with_capacity(traversers.len());
            for traverser in traversers {
                results.push(match traverser.value {
                    TraverserValue::Graph(graph) => ListItem::Graph(graph),
                    value => ListItem::Value(
                        value
                            .into_node_property(&this.storage)
                            .map_err(|err| this.locate_result_error(err))?,
                    ),
                });
            }
            Ok(results)
        })
    }

    /// Executes the traversal and materializes every result.
    ///
    /// The traversal runs as one unit of atomicity (implicit auto-commit, see
    /// [`GraphStorage::begin_unit`]): when it fails, also while materializing, every change it
    /// made is rolled back before the error is returned, on a storage that supports it
    /// ([`TraversalGraph`] does). A failing traversal leaves nothing
    /// behind.
    ///
    /// Deviation from TinkerPop: TinkerGraph without transactions keeps the changes a traversal
    /// made before it failed (book page "Transactions").
    pub fn to_list(mut self) -> TraverserResult<Vec<ElementProperty>> {
        self.atomically(|this| {
            let traversers = this.internal_to_list_expanded("to_list")?;
            let mut results = Vec::with_capacity(traversers.len());
            for traverser in traversers {
                results.push(
                    traverser
                        .value
                        .into_node_property(&this.storage)
                        .map_err(|err| this.locate_result_error(err))?,
                );
            }
            Ok(results)
        })
    }

    /// Executes the traversal for its side effects and discards its results (the TinkerPop
    /// `iterate()` terminal): nothing is materialized and bulk is never expanded.
    ///
    /// Like [`Self::to_list`] the traversal runs as one unit of atomicity: committed when it
    /// succeeds, rolled back when it fails, so a failing traversal leaves nothing behind.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let mut graph = TraversalGraph::new();
    /// graph.traversal_mut().add_v("person").property("name", "ada").iterate().unwrap();
    /// assert_eq!(graph.vertex_count(), 1);
    /// // A failing traversal leaves nothing behind.
    /// assert!(graph.traversal_mut().add_v("x").fail(Some("stop")).iterate().is_err());
    /// assert_eq!(graph.vertex_count(), 1);
    /// ```
    pub fn iterate(mut self) -> TraverserResult<()> {
        self.atomically(|this| this.internal_to_list_with_context().map(drop))
    }

    /// An error raised while a terminal materializes a result (a vertex dropped earlier in the
    /// query, returned by `optional(__.drop())`) is about the value the last step produced: it
    /// gets that step's location, like an error inside the run.
    fn locate_result_error(
        &self,
        err: crate::error::BoxedTraverserError,
    ) -> crate::error::BoxedTraverserError {
        if matches!(*err, TraverserError::StepFailed { .. }) {
            return err;
        }
        let Some(physical) = self
            .steps
            .iter()
            .rposition(|step| !matches!(step, GraphStep::Modulator(_, _)))
        else {
            return err;
        };
        let logical = self.steps[..physical]
            .iter()
            .filter(|step| !matches!(step, GraphStep::Modulator(_, _)))
            .count();
        let at = LevelAt::TOP.step(logical);
        let mut located = Err::<(), _>(err)
            .with_step_frame(self.steps[physical].get_name(), at, || {
                plan_frame(&self.steps, None, logical)
            })
            .map_or_else(
                |err| err,
                |()| Box::new(TraverserError::wrong_query("located")),
            );
        located.respell(crate::traversal::context::requested_spelling(&self.options));
        located
    }

    /// Runs `body` (a terminal: execution plus materialization) as one unit of atomicity of the
    /// storage: committed when it succeeds, rolled back when it fails. The run itself is a
    /// nested unit (see `run`), so a caller that only runs gets the same guarantee.
    pub(crate) fn atomically<R>(
        &mut self,
        body: impl FnOnce(&mut Self) -> TraverserResult<R>,
    ) -> TraverserResult<R> {
        let mark = self.storage.begin_unit()?;
        let result = crate::graph::storage::close_on_unwind(self, body, |this| {
            crate::graph::storage::rollback_unwound(&mut this.storage, mark)
        });
        match result {
            Ok(value) => {
                // A commit hook may veto: then the unit is rolled back and its error returned.
                self.storage.commit_unit(mark)?;
                Ok(value)
            }
            Err(err) => Err(crate::graph::storage::rollback_failed_unit(
                &mut self.storage,
                mark,
                err,
            )),
        }
    }

    /// Visualizes the result of the traversal using the specified format, without display limits.
    ///
    /// If the pipeline produces schema objects (e.g. via `schema_step()`), the schema is formatted
    /// according to `VisualizeFormat`. For standard element streams, formatted text, tables, or JSON is produced.
    pub fn visualize(self, format: VisualizeFormat) -> TraverserResult<String> {
        Ok(self
            .visualize_with(format, &crate::traversal::render::RenderOptions::default())?
            .text)
    }

    /// Visualizes the result of the traversal using the specified format and render options.
    ///
    /// With [`RenderOptions::max_rows`](crate::traversal::render::RenderOptions::max_rows) set to
    /// `n`, an internal `limit(n + 1)` is appended to the pipeline before it is optimized and
    /// executed, so no more than `n + 1` results are produced and materialized; the extra one
    /// only tells whether more exist. Barriers before it (`group()`, `order()`, ...) still see
    /// the whole stream. No limit is appended, and the traversal runs completely with only the
    /// output cut, when the pipeline mutates the graph or already ends on a single-result step
    /// (`count()`, `fold()`, `group()`, a schema, ...).
    ///
    /// [`RenderOptions::max_items`](crate::traversal::render::RenderOptions::max_items) cuts
    /// every nested collection inside a row. Tables, markdown and trees mark the cut with `…`;
    /// JSON and the JSON-per-line fallback formats stay valid data with no marker. What was cut is
    /// reported in [`RenderedOutput::truncation`](crate::traversal::render::RenderedOutput),
    /// never in the text. A schema result is never bounded.
    pub fn visualize_with(
        self,
        format: VisualizeFormat,
        options: &crate::traversal::render::RenderOptions,
    ) -> TraverserResult<crate::traversal::render::RenderedOutput> {
        self.visualize_bounded(format, options, true)
    }

    /// [`Self::visualize_with`], where the caller can forbid the appended display limit, for
    /// example because it knows a nested child traversal mutates the graph.
    pub(crate) fn visualize_bounded(
        mut self,
        format: VisualizeFormat,
        options: &crate::traversal::render::RenderOptions,
        may_stop_early: bool,
    ) -> TraverserResult<crate::traversal::render::RenderedOutput> {
        let max_rows = options.max_rows;
        self = self.with_display_limit(max_rows, may_stop_early);
        self.atomically(|this| this.render_bounded(format, options, max_rows))
    }

    /// The body of [`Self::visualize_bounded`] after the display limit is appended.
    fn render_bounded(
        &mut self,
        format: VisualizeFormat,
        options: &crate::traversal::render::RenderOptions,
        max_rows: Option<usize>,
    ) -> TraverserResult<crate::traversal::render::RenderedOutput> {
        use crate::traversal::render::RenderedOutput;

        let mut traversers = self.internal_to_list_expanded("display")?;
        let schemas: Vec<&GraphSchema> = traversers
            .iter()
            .filter_map(|t| t.value.as_schema())
            .collect();

        if !schemas.is_empty() {
            let rendered: Vec<String> = schemas.iter().map(|s| s.visualize(format)).collect();
            return Ok(RenderedOutput::complete(rendered.join("\n\n")));
        }

        let more_rows = match max_rows {
            Some(n) if traversers.len() > n => {
                traversers.truncate(n);
                true
            }
            _ => false,
        };
        let rows_shown = traversers.len();

        // A stream of paths renders as one row per path, in a `path` column listing its objects.
        let paths_as_rows = matches!(
            format,
            VisualizeFormat::Auto | VisualizeFormat::Table | VisualizeFormat::Markdown
        ) && !traversers.is_empty()
            && traversers
                .iter()
                .all(|t| matches!(t.value, TraverserValue::Path(_)));

        let mut elements = Vec::with_capacity(traversers.len());
        for traverser in traversers {
            let element = traverser
                .value
                .into_node_property(&self.storage)
                .map_err(|err| self.locate_result_error(err))?;
            if paths_as_rows {
                let mut row = PropertyMap::default();
                row.insert(CompactString::new("path"), element);
                elements.push(ElementProperty::Object(Box::new(row)));
            } else {
                elements.push(element);
            }
        }

        render_elements(&elements, format, options, rows_shown, more_rows)
    }
}

/// Renders materialized results in `format` within the item limit of `options`: the formatting
/// half of [`GraphTraversalSource::visualize_with`], shared with the script DSL's rendering of an
/// `execute()` result. `rows_shown`/`more_rows` describe the row cut the caller already made.
pub(crate) fn render_elements(
    elements: &[ElementProperty],
    format: VisualizeFormat,
    options: &crate::traversal::render::RenderOptions,
    rows_shown: usize,
    more_rows: bool,
) -> TraverserResult<crate::traversal::render::RenderedOutput> {
    use crate::traversal::render::{ItemLimit, RenderedOutput, Truncation, cut_nested};

    let limit = ItemLimit::new(options.max_items);
    let text = match format {
        VisualizeFormat::Auto | VisualizeFormat::Table | VisualizeFormat::Markdown => {
            crate::traversal::table::format_elements_as_table(elements, format, options, &limit)?
        }
        VisualizeFormat::Json => {
            #[cfg(feature = "serde")]
            {
                // Value JSON: a non-finite float is GraphSON's typed `g:Double`.
                use crate::value::physical::TypedFloats;
                let result = if limit.is_limited() {
                    let cut: Vec<ElementProperty> =
                        elements.iter().map(|el| cut_nested(el, &limit)).collect();
                    serde_json::to_string_pretty(&TypedFloats(&cut))
                } else {
                    serde_json::to_string_pretty(&TypedFloats(elements))
                };
                result.map_err(|e| {
                    Box::new(crate::error::TraverserError::Io(
                        crate::error::IOError::JsonParse(e),
                    ))
                })?
            }
            #[cfg(not(feature = "serde"))]
            {
                let _ = cut_nested;
                "JSON serialization requires the 'serde' feature flag".to_string()
            }
        }
        VisualizeFormat::Tree => {
            crate::traversal::tree_render::format_elements_as_tree(elements, &limit)?
        }
        // Schema-only formats have no rendering for traversal results: fall back to one
        // compact JSON value per line.
        VisualizeFormat::JsonSchema | VisualizeFormat::Mermaid | VisualizeFormat::PlantUML => {
            crate::traversal::table::format_elements_as_lines(elements, &limit)
        }
    };

    Ok(RenderedOutput::new(
        text,
        Truncation::of(rows_shown, more_rows, limit.was_cut(), options.max_items),
    ))
}

impl<'g, S: GraphStorage + 'g> crate::GraphTraversalSource<'g, S> {
    /// Appends the display limit `limit(max_rows + 1)` when `max_rows` is set and the pipeline
    /// allows it ([`Self::may_stop_early`]).
    pub(crate) fn with_display_limit(self, max_rows: Option<usize>, may_stop_early: bool) -> Self {
        match max_rows {
            Some(n) if may_stop_early && self.may_stop_early() => {
                self.display_limit(n.saturating_add(1))
            }
            _ => self,
        }
    }

    /// Whether a bounded visualizer may append its `limit(n + 1)`.
    ///
    /// Not when the pipeline mutates the graph (`add_v()`, `add_e()`, `property()`, `drop()`, ...),
    /// so every element is still updated, and not when the last step already yields at most one
    /// result (a barrier such as `count()`, `fold()`, `group()` or `schema()`), where a limit only
    /// hides a terminal step from the optimizer (`count_pushdown`).
    fn may_stop_early(&self) -> bool {
        let mutates = self.steps.iter().any(GraphStep::mutates);
        let single_result = matches!(
            self.steps
                .iter()
                .rev()
                .find(|step| !matches!(step, GraphStep::Modulator(_, _))),
            Some(GraphStep::Barrier(_, _) | GraphStep::Count(_, _))
        );
        !mutates && !single_result
    }

    /// Exports or visualizes the traversal output as a tabular/formatted string.
    pub fn to_table(self) -> TraverserResult<String> {
        self.visualize(VisualizeFormat::Table)
    }

    /// Exports or visualizes the traversal output as a table with custom render options; see
    /// [`Self::visualize_with`] for the display limits.
    pub fn to_table_with(
        self,
        options: &crate::traversal::render::RenderOptions,
    ) -> TraverserResult<crate::traversal::render::RenderedOutput> {
        self.visualize_with(VisualizeFormat::Table, options)
    }

    /// Exports or visualizes the traversal schema or elements as Markdown tables.
    pub fn to_markdown(self) -> TraverserResult<String> {
        self.visualize(VisualizeFormat::Markdown)
    }

    /// Exports or visualizes the traversal output as Markdown tables with custom render options;
    /// see [`Self::visualize_with`] for the display limits.
    pub fn to_markdown_with(
        self,
        options: &crate::traversal::render::RenderOptions,
    ) -> TraverserResult<crate::traversal::render::RenderedOutput> {
        self.visualize_with(VisualizeFormat::Markdown, options)
    }

    /// Exports or visualizes the traversal schema as a Mermaid ER diagram.
    pub fn to_mermaid(self) -> TraverserResult<String> {
        self.visualize(VisualizeFormat::Mermaid)
    }

    /// Exports or visualizes the traversal schema as a PlantUML ER diagram.
    pub fn to_plantuml(self) -> TraverserResult<String> {
        self.visualize(VisualizeFormat::PlantUML)
    }

    /// Exports or visualizes the traversal schema as a JSON Schema (Draft 2020-12).
    pub fn to_json_schema(self) -> TraverserResult<String> {
        self.visualize(VisualizeFormat::JsonSchema)
    }

    /// Exports or visualizes the traversal output or schema as JSON.
    pub fn to_json(self) -> TraverserResult<String> {
        self.visualize(VisualizeFormat::Json)
    }

    /// Visualizes the traversal output as an indented hierarchical tree (Unix `tree`-style box
    /// drawing), most useful after `tree()`, but a generic renderer for any nested-object result
    /// (`group()`, `valueMap()`, `elementMap()`, `select()`). A non-nested result (e.g. a plain
    /// `g.V()` list) is not an error — it degrades to one root line per element.
    pub fn to_tree(self) -> TraverserResult<String> {
        self.visualize(VisualizeFormat::Tree)
    }

    /// Runs the traversal and returns its [`TraversalMetrics`]: one entry per step of the
    /// optimized plan (fused steps under their real names), with timings and counts, nested
    /// child-traversal steps, and the optimizer rules that fired. TinkerPop's `profile()`: the
    /// results themselves are not returned (use [`Self::execute_with`] for both).
    ///
    /// Equivalent to `.profile_with(ProfileType::none())`: no memory figures. Use
    /// [`Self::profile_with`]`(ProfileType::Memory)` to additionally collect the
    /// [`Metrics::memory`](crate::Metrics::memory) section. The [`Display`](std::fmt::Display)
    /// impl of the result renders the text table the CLI shows.
    ///
    /// A failing traversal fails here as well; [`Self::execute_with`] returns the partial profile
    /// together with the error instead.
    pub fn profile(self) -> TraverserResult<TraversalMetrics> {
        self.profile_with(ProfileType::none())
    }

    /// [`Self::profile`], additionally collecting the categories named by `flags` (currently only
    /// [`ProfileType::Memory`], which adds a [`Metrics::memory`](crate::Metrics::memory) section
    /// to every step). Without it the executor skips every allocator/estimation call, and no
    /// step carries a memory section.
    pub fn profile_with(self, flags: ProfileType) -> TraverserResult<TraversalMetrics> {
        // The metrics need the full run, not the materialized results.
        let run = self.execute_items(&ExecOptions::profile(flags), |_, _| Ok(()));
        match (run.error, run.profile) {
            (Some(err), _) => Err(err),
            (None, Some(profile)) => Ok(profile),
            (None, None) => Err(Box::new(TraverserError::NotImplemented {
                feature: "a profile for a run without profiling",
            })),
        }
    }

    /// Runs the traversal and returns an [`Execution`]: the results [`Self::to_list`] would
    /// return, the error when the run failed, and the profile when [`Self::execute_with`]
    /// requested one. Never fails itself: every error, including one that prevents the run
    /// from starting (an invalid option, a plan check), is inside the returned value
    /// ([`Execution::into_result`] converts it for `?`).
    pub fn execute(self) -> Execution {
        self.execute_with(ExecOptions::new())
    }

    /// [`Self::execute`] with options, e.g. `ExecOptions::profile(ProfileType::none())` for the
    /// results together with their [`TraversalMetrics`] (partial, up to the failing step, when
    /// the run fails). Profiling instruments every step, so a profiled run is slightly slower;
    /// its results are identical.
    pub fn execute_with(self, options: ExecOptions) -> Execution {
        let run = self.execute_items(&options, |value, storage| value.into_node_property(storage));
        Execution {
            results: run.items,
            error: run.error,
            profile: run.profile,
            mutated: run.mutated,
            changes: run.changes,
        }
    }

    /// The engine of [`Self::execute_with`] and [`Self::profile_with`]: runs the traversal with
    /// profiling as `options` request, expands the bulk and turns every result into a `T` with
    /// `materialize` (the script DSL keeps graph values as handles this way). Every failure ends
    /// up in [`ExecutedItems::error`], with the profile recorded up to it.
    pub(crate) fn execute_items<T>(
        mut self,
        options: &ExecOptions,
        mut materialize: impl FnMut(TraverserValue<S>, &S) -> TraverserResult<T>,
    ) -> ExecutedItems<T> {
        if let Some(flags) = options.profile_types() {
            self.strategy = self.strategy.or(TraversalStrategy::ProfileEnabled);
            self.profile_type = flags;
        }
        let start = Instant::now();
        // One unit around the run AND the materialization: an error in either rolls back.
        let not_run = |err: crate::error::GraphError| ExecutedItems {
            items: Vec::new(),
            error: Some(Box::new(TraverserError::from(err))),
            profile: None,
            mutated: false,
            changes: crate::graph::UnitChanges::NONE,
        };
        let seq_before = match self.storage.last_commit_seq() {
            Ok(seq) => seq,
            Err(err) => return not_run(err),
        };
        let mark = match self.storage.begin_unit() {
            Ok(mark) => mark,
            Err(err) => return not_run(err),
        };
        let (profile, materialized) = crate::graph::storage::close_on_unwind(
            &mut self,
            |this| {
                let mut outcome = this.run();
                let expansion = &mut outcome.expansion;
                let expanded = outcome
                    .result
                    .and_then(|traversers| expansion.expand(traversers, "execute"));
                let duration = start.elapsed();
                let profile = options
                    .profile_types()
                    .map(|_| outcome.profile.into_metrics(duration));
                let materialized = expanded.and_then(|traversers| {
                    let mut items = Vec::with_capacity(traversers.len());
                    for traverser in traversers {
                        items.push(
                            materialize(traverser.value, &this.storage)
                                .map_err(|err| this.locate_result_error(err))?,
                        );
                    }
                    Ok(items)
                });
                (profile, materialized)
            },
            |this| crate::graph::storage::rollback_unwound(&mut this.storage, mark),
        );
        // Asked before the unit closes: whether the run changed something that is kept. For a
        // run nested in an enclosing unit (an explicit transaction, a whole-script unit) this is
        // the answer, since only the outermost commit advances the commit sequence number. A
        // storage that cannot tell fails the run (rolled back below).
        let (materialized, changes) = match materialized {
            Ok(items) => match self.storage.unit_changes(mark) {
                Ok(changes) => (Ok(items), changes),
                Err(err) => (
                    Err(Box::new(TraverserError::from(err))),
                    crate::graph::UnitChanges::NONE,
                ),
            },
            Err(err) => (Err(err), crate::graph::UnitChanges::NONE),
        };
        let changed = changes.any();
        let committed = match materialized {
            // A commit hook may veto: then the unit is rolled back and the run failed.
            Ok(items) => self
                .storage
                .commit_unit(mark)
                .map(|()| items)
                .map_err(|err| Box::new(TraverserError::from(err))),
            Err(err) => Err(crate::graph::storage::rollback_failed_unit(
                &mut self.storage,
                mark,
                err,
            )),
        };
        let mutated = changed
            || match self.storage.last_commit_seq() {
                Ok(seq_after) => seq_after > seq_before,
                // The unit is closed (committed or rolled back) and its outcome is what the run
                // reports; only whether it advanced the position is unknown. Assume it did, so
                // a front end refreshes its views rather than show a stale graph.
                Err(err) => {
                    tracing::warn!("reading the commit position after a run failed: {err}");
                    committed.is_ok()
                }
            };
        match committed {
            Ok(items) => ExecutedItems {
                items,
                error: None,
                profile,
                mutated,
                // A storage that counts commits but not unit changes: data, at least.
                changes: if mutated && !changed {
                    crate::graph::UnitChanges::new(true, false)
                } else {
                    changes
                },
            },
            Err(err) => ExecutedItems {
                items: Vec::new(),
                error: Some(err),
                profile,
                mutated: false,
                changes: crate::graph::UnitChanges::NONE,
            },
        }
    }

    fn is_count_step(step: Option<&GraphStep<'g, S>>) -> bool {
        if let Some(step) = step {
            matches!(step, GraphStep::Count(_, _))
        } else {
            false
        }
    }

    /// Test support: executes the traversal and returns the raw, still-lazy [`Traverser`]s,
    /// without materializing their values.
    ///
    /// The traversers are returned *with* their [`bulk`](Traverser::bulk): one traverser may
    /// stand for several identical results (terminals that hand out values expand the bulk
    /// first, see `internal_to_list_expanded`). The unit tests use it to count traversers and
    /// their bulk (`traversal/bulk_tests.rs`).
    #[cfg(test)]
    pub(crate) fn internal_to_list(&mut self) -> TraverserResult<Vec<Traverser<S>>> {
        Ok(self.internal_to_list_with_context()?.0)
    }

    /// Runs the traversal at a materialization boundary: every traverser is expanded into one
    /// traverser per unit of bulk, bounded by `bulk.max_expansion`. `step` names the boundary
    /// in the `BulkExpansionLimit` error.
    pub(crate) fn internal_to_list_expanded(
        &mut self,
        step: &str,
    ) -> TraverserResult<Vec<Traverser<S>>> {
        let (traversers, mut expansion) = self.internal_to_list_with_context()?;
        expansion.expand(traversers, step)
    }

    /// Runs the traversal; returns the traversers (with bulk) and the bounds a materializing
    /// caller must expand them with.
    pub(crate) fn internal_to_list_with_context(
        &mut self,
    ) -> TraverserResult<(Vec<Traverser<S>>, Expansion)> {
        let outcome = self.run();
        Ok((outcome.result?, outcome.expansion))
    }

    /// Runs the traversal once. The profile recorded so far comes back whether or not the run
    /// succeeded (empty unless profiling was enabled), so a failed run still has its partial
    /// profile.
    fn run(&mut self) -> RunOutcome<S> {
        let started = Instant::now();
        let outcome = self.run_once();
        // The query statistics (book page "Workload Statistics"): one sample per run of the
        // optimized plan, when the graph's lock guard put statistics on this thread.
        if let Some((stats, origin)) = crate::graph::workload::scope::attached() {
            self.record_query(&stats, origin.channel, started, &outcome);
        }
        outcome
    }

    /// Counts one run in the query statistics: its fingerprint (the optimized plan with its
    /// literals replaced), result count (with bulk), duration and failure. Out of line: only a
    /// graph with statistics pays the rendering.
    #[inline(never)]
    fn record_query(
        &mut self,
        stats: &crate::graph::workload::WorkloadStats,
        channel: crate::graph::workload::Channel,
        started: Instant,
        outcome: &RunOutcome<S>,
    ) {
        use crate::graph::workload::{SeriesKey, SeriesKind, fingerprint};
        let plan: Vec<String> = self.steps.iter().map(GraphStep::get_name).collect();
        let key = SeriesKey::new(
            SeriesKind::Query(fingerprint::normalize(&plan.join(".")).into()),
            channel,
        );
        let items = match &outcome.result {
            Ok(traversers) => traversers
                .iter()
                .fold(0u64, |sum, traverser| sum.saturating_add(traverser.bulk)),
            Err(_) => 0,
        };
        let mut sample =
            crate::graph::workload::Sample::new(items, started.elapsed().as_micros() as u64);
        if outcome.result.is_err() {
            sample = sample.failed();
        }
        let now = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .map_or(0, |d| d.as_micros() as i64);
        stats.record(key, now, sample);
        for (property, items) in self.property_uses(outcome.step_inputs.as_deref()) {
            let key = SeriesKey::new(SeriesKind::Property(property), channel);
            stats.record(key, now, crate::graph::workload::Sample::new(items, 0));
        }
    }

    /// The properties the optimized plan used and how, each once with the traversers that
    /// reached the steps using it (top-level steps; uses inside child traversals count with no
    /// items). The element kind and label come from what the plan states before each step
    /// (`access_flow_after`, labels of `v(labels)` / `e(labels)` / `has_label`); a `by(key)`
    /// counts as a sort/group on or after `order`/`group`/`group_count`/`dedup`, else as a read; a
    /// filter on the key a source's index serves counts as an index filter.
    fn property_uses(
        &mut self,
        inputs: Option<&[u64]>,
    ) -> Vec<(crate::graph::workload::PropertyUse, u64)> {
        use crate::graph::workload::{AccessKind, PropertyUse};
        let mut uses: Vec<(PropertyUse, u64)> = Vec::new();
        let mut add = |flow: &crate::traversal::step::AccessFlow,
                       key: Option<&str>,
                       access: AccessKind,
                       items: u64| {
            let label = match flow.labels.as_slice() {
                [one] => Some(CompactString::from(one.as_str())),
                _ => None,
            };
            let property = PropertyUse {
                element: flow.element,
                label,
                key: CompactString::from(key.unwrap_or("*")),
                access,
            };
            match uses.iter_mut().find(|(known, _)| *known == property) {
                Some((_, count)) => *count = count.saturating_add(items),
                None => uses.push((property, items)),
            }
        };
        let mut flow = crate::traversal::step::AccessFlow::default();
        let mut indexed: Vec<String> = Vec::new();
        let mut previous = "";
        let mut logical = 0usize;
        for step in &mut self.steps {
            let items = match step {
                GraphStep::Modulator(_, _) => 0,
                _ => inputs
                    .and_then(|inputs| inputs.get(logical))
                    .copied()
                    .unwrap_or(0),
            };
            // A source's own filters (folded `has`, an index) apply to what it yields; for the
            // statistics its labels name the vertices too (they are counts, not a permission).
            if let GraphStep::Init(_, init) = step {
                flow = crate::traversal::step::AccessFlow {
                    labels: init.labels().map(<[String]>::to_vec).unwrap_or_default(),
                    ..step.access_flow_after(flow)
                };
            }
            let sorts = |name: &str| matches!(name, "order" | "group" | "group_count" | "dedup");
            let ordering = sorts(previous) || sorts(step.kind_name());
            step.property_access(&mut |key, access| {
                let access = match (access, key) {
                    (AccessKind::FilterIndex, Some(key)) => {
                        indexed.push(key.to_string());
                        access
                    }
                    (AccessKind::FilterScan, Some(key)) if indexed.iter().any(|k| k == key) => {
                        AccessKind::FilterIndex
                    }
                    (AccessKind::OrderGroup, _) if !ordering => AccessKind::Read,
                    (access, _) => access,
                };
                add(&flow, key, access, items);
            });
            Self::child_property_uses(&mut *step, &mut add);
            if let GraphStep::Modulator(_, _) = step {
                continue;
            }
            previous = step.kind_name();
            flow = match step {
                GraphStep::Init(_, _) => flow,
                GraphStep::Filter("has_label", filter) => crate::traversal::step::AccessFlow {
                    element: flow.element,
                    labels: filter.labels().map(<[String]>::to_vec).unwrap_or_default(),
                },
                _ => step.access_flow_after(flow),
            };
            logical += 1;
        }
        uses
    }

    /// The property uses inside the child traversals of `step`, with no flow and no items.
    fn child_property_uses(
        step: &mut GraphStep<'g, S>,
        add: &mut impl FnMut(
            &crate::traversal::step::AccessFlow,
            Option<&str>,
            crate::graph::workload::AccessKind,
            u64,
        ),
    ) {
        let flow = crate::traversal::step::AccessFlow::default();
        for child in step.child_traversals_mut() {
            for nested in child.steps.iter_mut() {
                nested.property_access(&mut |key, access| add(&flow, key, access, 0));
                Self::child_property_uses(nested, add);
            }
        }
    }

    /// [`Self::run`] without the query statistics.
    fn run_once(&mut self) -> RunOutcome<S> {
        // The permission check runs on the plan as written, before the optimizer can fuse or
        // move a step, and before anything executes.
        // An error before the run is respelled like one inside it (`render.spelling`).
        let failed_before_run = |mut err: crate::error::BoxedTraverserError, options| {
            err.respell(crate::traversal::context::requested_spelling(options));
            RunOutcome {
                result: Err(err),
                profile: ProfileContext::new(),
                expansion: Expansion::UNBOUNDED,
                step_inputs: None,
            }
        };
        // The `embed` steps resolve their embedder first, so the permission check asks for the
        // embedder that runs and the profile names it.
        let embedders = self
            .embedders
            .clone()
            .unwrap_or_else(crate::value::Embedders::current);
        let storage = &self.storage;
        // The schema is read only when a text query needs it; a storage that cannot read it
        // fails the run with its own error (checked first: a bind error after a failed read
        // would only be its consequence).
        let schema_error = std::cell::Cell::new(None);
        let read_schema = || {
            storage.get_schema().unwrap_or_else(|err| {
                schema_error.set(Some(err));
                None
            })
        };
        let bound = bind_embedders(&mut self.steps, &embedders, &read_schema);
        if let Some(err) = schema_error.take() {
            return failed_before_run(Box::new(TraverserError::from(err)), &self.options);
        }
        if let Err(err) = bound {
            return failed_before_run(err, &self.options);
        }
        if let Some(restrictions) = &self.restrictions
            && let Err(err) = restrictions.check(&mut self.steps, &self.options)
        {
            return failed_before_run(err, &self.options);
        }
        let policy = match self.storage.execution_policy() {
            Ok(policy) => policy,
            Err(err) => {
                return failed_before_run(Box::new(TraverserError::from(err)), &self.options);
            }
        };
        let mut options = match ExecutionOptions::parse(&self.options, &policy) {
            Ok(options) => options,
            Err(err) => return failed_before_run(err, &self.options),
        };
        // The memory budget's starting point: the live heap with a tracking allocator, else one
        // walk of the graph's footprint, here and never per check.
        options.memory = match options.memory.map(|budget| budget.start(&self.storage)) {
            Some(Err(err)) => {
                return failed_before_run(Box::new(TraverserError::from(err)), &self.options);
            }
            started => started.and_then(Result::ok),
        };
        let path_analysis = options.path_analysis;
        let limits = bulk::ExpansionLimits::of(&options);
        let spelling = options.render_spelling;
        // Implicit auto-commit: the run is one unit of atomicity; any error inside it (a step,
        // the schema, a resource limit, `evaluationTimeout`) rolls back what it changed.
        let mark = match self.storage.begin_unit() {
            Ok(mark) => mark,
            Err(err) => {
                return failed_before_run(Box::new(TraverserError::from(err)), &self.options);
            }
        };
        let count_inputs = crate::graph::workload::scope::attached().is_some();
        let (result, mut profile, deadline, step_inputs) = crate::graph::storage::close_on_unwind(
            self,
            |this| {
                let mut context = TraverserContext::with_options(
                    &mut this.storage,
                    this.strategy,
                    this.side_effect.clone(),
                    options,
                );
                if count_inputs {
                    context.step_inputs = Some(Vec::new());
                }
                context.profile_type = this.profile_type;
                context.create_context = this
                    .create_time
                    .map(crate::graph::schema::defaults::CreateContext::at);
                let result = match &this.sack {
                    Some(spec) => context.sacks.intern(spec.initial.clone()).map(|sack| {
                        context.initial_sack = sack;
                        context.sack_merge = spec.merge;
                    }),
                    None => Ok(()),
                }
                .and_then(|()| Self::execute_steps(&mut this.steps, &mut context, path_analysis));
                #[cfg(test)]
                crate::traversal::unwind_probe::after_steps();
                (
                    result,
                    std::mem::take(&mut context.profile),
                    context.deadline.take(),
                    context.step_inputs.take(),
                )
            },
            |this| crate::graph::storage::rollback_unwound(&mut this.storage, mark),
        );
        // Steps render in snake_case; `render.spelling` respells the diagnostics once, here.
        profile.respell(spelling);
        let result = result.map_err(|mut err| {
            err.respell(spelling);
            err
        });
        let result = match result {
            // Nested inside a terminal's unit this commit only closes a savepoint; called on
            // its own it is the outermost commit, which a commit hook may veto.
            Ok(value) => self
                .storage
                .commit_unit(mark)
                .map(|()| value)
                .map_err(|err| Box::new(TraverserError::from(err))),
            Err(err) => Err(crate::graph::storage::rollback_failed_unit(
                &mut self.storage,
                mark,
                err,
            )),
        };
        RunOutcome {
            result,
            profile,
            expansion: Expansion { limits, deadline },
            step_inputs,
        }
    }

    /// Runs the traversal and returns how many path nodes the execution recorded, so tests can
    /// assert that a query without a path consumer records none.
    #[cfg(test)]
    pub(crate) fn internal_path_node_count(&mut self) -> TraverserResult<usize> {
        let policy = self.storage.execution_policy()?;
        let options = ExecutionOptions::parse(&self.options, &policy)?;
        let path_analysis = options.path_analysis;
        let mut context = TraverserContext::with_options(
            &mut self.storage,
            self.strategy,
            self.side_effect.clone(),
            options,
        );
        if let Some(spec) = &self.sack {
            context.initial_sack = context.sacks.intern(spec.initial.clone())?;
            context.sack_merge = spec.merge;
        }
        Self::execute_steps(&mut self.steps, &mut context, path_analysis)?;
        Ok(context.paths.node_count())
    }

    pub(crate) fn execute_steps(
        steps: &mut Vec<GraphStep<'g, S>>,
        context: &mut TraverserContext<'_, S>,
        path_analysis: bool,
    ) -> TraverserResult<Vec<Traverser<S>>> {
        // The optimizer captures the level as written whenever a rule rewrites it: once steps are
        // folded and reordered, the query the user actually wrote is no longer recoverable.
        // `default_pipeline_with` consults this execution's `optimizer.disabled`/
        // `optimizer.enabled` options (D3 of the rule-visibility spec) when building the pipeline.
        // With the property indexes the storage serves (none: one empty `Vec`, no allocation, and
        // the `property_index` rule is left out of the pipeline).
        let optimizer = crate::traversal::optimizer::TraversalOptimizer::for_run(
            &context.options,
            context.storage.property_indexes()?,
        )
        .with_diagnostics(context.strategy.contains(TraversalStrategy::Diagnostics));
        let level = optimizer.apply(steps);
        // Fired-rule names, top-level and nested (the same `&self` optimizes every nested level
        // via `step.optimize_nested`), for the profile's `optimizer_rules_applied`. Collected
        // regardless of whether profiling was requested; only a profile hands it out.
        context.profile.applied_optimizer_rules = optimizer
            .fired_rules()
            .into_iter()
            .map(str::to_string)
            .collect();
        // Step ids are positions in the optimized plan: number the child traversals now.
        GraphStep::number_children(steps);
        // The path requirement analysis runs after every rule, on the plan that executes.
        // Its errors are about one step of the plan: they carry that step's location.
        let (path_modes, effects) =
            match optimizer.analyze_paths(steps, path_analysis, &context.side_effect) {
                Ok(analysed) => analysed,
                Err(err) => return Err(locate_plan_error(steps, level.as_ref(), err)),
            };
        // A plan that mutates never merges: one logical traverser must stay one mutation.
        context.bulk_merge = context.options.bulk_merge && !effects.mutates;
        // A sack merge operator or `bulk.one` makes merging visible: only an explicit barrier
        // merges then, so the result never depends on where the optimizer put a merge point.
        context.bulk_one = context.options.bulk_one;
        context.auto_merge =
            context.options.bulk_merge && context.sack_merge.is_none() && !context.bulk_one;
        // A start step (`g.union(..)`) runs once on a seed traverser holding `null`.
        let initial = match steps.first_mut() {
            Some(GraphStep::FlatMap(_, step)) if step.is_start() => {
                step.mark_start();
                vec![Traverser::start(
                    context,
                    TraverserValue::Computed(ElementProperty::Null, None),
                )]
            }
            _ => Vec::new(),
        };
        Self::execute_optimized_steps(
            steps,
            initial,
            context,
            LevelAt::TOP,
            level.as_ref(),
            &path_modes,
        )
    }

    /// Computes a step's `.profile()` memory figures (D2-D4 of the memory-inspection spec, D1-D2
    /// of the richer-memory-diagnostics spec). Called only from inside the existing `is_profile`
    /// gate, so this never runs on the unprofiled path. `profile_memory` additionally gates
    /// whether `ProfileType::Memory` was requested for this run (D2 of the `ProfileType` bitmask
    /// spec): when it is `false`, this returns `None` immediately (the step gets no memory section),
    /// without touching any allocator thread-local.
    ///
    /// A non-materializing step (`GraphStep::materializes() == false`, the overwhelming majority
    /// per rule 4's lazy-evaluation discipline) always reports `0` bytes with no per-item work at
    /// all — `materializes()` is a static, O(1) fact about the step. A materializing step's bytes
    /// are either a real allocator delta (`alloc-tracking` feature compiled in: `alloc_before` was
    /// captured immediately before the step ran, per the call site) or a `size_of`/capacity-based
    /// estimate over the step's own output (feature off). `memory_source` is a whole-run,
    /// compile-time choice, not a per-step runtime one (D4): every node in a given `.profile()`
    /// call reports the same variant. Under `Tracked`, `bytes_retained`/`alloc_count` are always
    /// `Some`; under `Estimated` they are always `None`.
    ///
    /// **Known imprecision of the tracked path (documented, not silently assumed exact):** this
    /// only measures materializing steps, the same gating the estimation path uses. A step that
    /// does not self-report as materializing may still cause a small amount of incidental
    /// allocation (e.g. growth of the shared traverser buffer); that allocation is not attributed
    /// to any single step's `bytes_materialized`, so a fully-lazy query chain reports `0` for every
    /// node even under `Tracked`, rather than a chain of near-zero noise values. This is a
    /// deliberate simplification: exact per-step attribution of *incidental* allocator churn would
    /// require bracketing every single step regardless of category, at a measurable
    /// always-on cost the estimation-path steps don't need to pay.
    #[inline]
    fn step_memory_metrics(
        #[allow(unused_variables)] current_step: &GraphStep<'g, S>,
        #[allow(unused_variables)] output: &[Traverser<S>],
        #[allow(unused_variables)] alloc_before: AllocSnapshot,
        profile_memory: bool,
    ) -> Option<MemoryFigures> {
        // `ProfileType::Memory` gates collection, not just rendering: when it was not requested,
        // skip the allocator/estimation work entirely; the step gets no memory section.
        if !profile_memory {
            return None;
        }
        // Under `alloc-tracking`, `alloc_before` was already captured unconditionally whenever
        // `profile_memory` is set (the counter reads are effectively free once `TrackingAllocator`
        // is installed), so every step's real delta is measured here regardless of `materializes()` —
        // including expansion steps like `out()`/`in()` that grow the shared traverser buffer
        // without owning any new *property* data. `materializes()` stays the gate for the
        // *estimated* path only, where computing `estimate_traversers_bytes` for every step would
        // cost real CPU for something usually zero, per rule 4.
        #[cfg(feature = "alloc-tracking")]
        {
            let allocated_after = crate::alloc_tracking::thread_allocated_bytes();
            let live_after = crate::alloc_tracking::thread_live_bytes();
            let alloc_count_after = crate::alloc_tracking::thread_alloc_count();
            Some(MemoryFigures::tracked(
                allocated_after.saturating_sub(alloc_before.allocated),
                live_after - alloc_before.live,
                alloc_count_after.saturating_sub(alloc_before.alloc_count),
            ))
        }
        #[cfg(not(feature = "alloc-tracking"))]
        {
            if !current_step.materializes() {
                return Some(MemoryFigures::estimated(0));
            }
            Some(MemoryFigures::estimated(estimate_traversers_bytes(output)))
        }
    }

    /// One `v()`/`e()` step of [`Self::execute_optimized_steps`]: the elements it finds become
    /// `source` (a mid-traversal step crosses them with the incoming traversers); returns the
    /// step's output count. Out of line, so the frame of the executor, which nested child
    /// traversals re-enter recursively, does not carry the source step's locals.
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    fn execute_init_step(
        context: &mut TraverserContext<'_, S>,
        step: &(dyn crate::traversal::step::GraphStepInit<'g, S> + 'g),
        source: &mut Vec<Traverser<S>>,
        physical: usize,
        steps: &[GraphStep<'g, S>],
        level: Option<&LevelPlan<'g, S>>,
        step_at: StepAt,
        logical_step_idx: usize,
        is_count_next_step: bool,
        collapsed: &mut Option<usize>,
        mode: &PathMode,
    ) -> TraverserResult<usize> {
        let mut count_out: usize;
        // A storage read of this source step fails it, located at the step.
        // The success path stays a plain match (no call per element).
        macro_rules! located {
            ($e:expr) => {
                match $e {
                    Ok(value) => Ok(value),
                    Err(err) => Err(err).with_step_frame(
                        || step.get_name(),
                        step_at,
                        || plan_frame(steps, level, logical_step_idx),
                    ),
                }
            };
        }
        // Mid-traversal (`g.V("1").as("a").V("2")`, or `__.V()` in a child that
        // receives a traverser), each incoming traverser maps to every element the
        // step finds, as in TinkerPop; only the first step of a traversal that
        // receives nothing is a start step.
        let incoming = (physical > 0 || !source.is_empty()).then(|| std::mem::take(source));
        let init_result: (GraphNodeType, &Option<Vec<String>>) = step.process(context);
        let init_labels = step.labels();
        let init_ids = step.ids();
        // Folded `has(key, value)` conditions: checked while scanning, before a
        // traverser exists (never together with a fused count, see the rules).
        let property_filters = step.property_filters();
        let count_only = step.is_count_only();
        let group_count_by_label = step.is_group_count_by_label();
        // A fused `count()`/`groupCount()` counts one pass over the graph. Mid-
        // traversal every incoming traverser makes its own pass (`union(__.V().count())`
        // counts incoming x elements), so the counts are scaled by the incoming bulk
        // instead of crossing the single count traverser with the stream.
        let fused_factor = match &incoming {
            Some(incoming) if count_only || group_count_by_label => {
                Some(bulk::total_bulk(context, incoming, "count")?)
            }
            _ => None,
        };

        match init_result.0 {
            GraphNodeType::Vertex => {
                if group_count_by_label {
                    let mut tmp: IndexMap<String, usize, FnvBuildHasher> = IndexMap::default();
                    if let Some(ids) = init_ids {
                        for id_value in ids {
                            if let Some(node_index) =
                                located!(context.storage.get_vertex_by_id(id_value))?
                                && let Some(v_lbl) =
                                    located!(context.storage.vertex_label(node_index))?
                            {
                                if let Some(labels) = init_labels
                                    && !located!(crate::graph::storage::vertex_has_any_label(
                                        &*context.storage,
                                        node_index,
                                        labels
                                    ))?
                                {
                                    continue;
                                }
                                *tmp.entry(v_lbl.to_string()).or_insert(0) += 1;
                            }
                        }
                    } else if let Some(labels) = init_labels {
                        // D5, Bug 2: a vertex carrying more than one of the requested
                        // labels must be counted once, grouped under its own primary
                        // label — not once per requested label it happens to match.
                        let mut seen: std::collections::HashSet<S::VertexId> =
                            std::collections::HashSet::new();
                        for label in labels {
                            for node_index in located!(context.storage.vertices_by_label(label))? {
                                if seen.insert(node_index)
                                    && let Some(v_lbl) =
                                        located!(context.storage.vertex_label(node_index))?
                                {
                                    *tmp.entry(v_lbl.to_string()).or_insert(0) += 1;
                                }
                            }
                        }
                    } else {
                        // `label()` is the primary label: a multi-label vertex
                        // counts once, under its first label, exactly as the
                        // unoptimized `group_count().by(T.label)` groups it. Read
                        // from the storage's statistics; a storage without them is
                        // scanned instead.
                        match context.storage.statistics().with_step_frame(
                            || step.get_name(),
                            step_at,
                            || plan_frame(steps, level, logical_step_idx),
                        )? {
                            Some(stats) => {
                                for (label, cnt) in stats.primary_vertex_label_counts() {
                                    tmp.insert(label.to_string(), cnt);
                                }
                            }
                            None => {
                                for node in located!(context.storage.vertices())? {
                                    if let Some(label) =
                                        located!(context.storage.vertex_label(node))?
                                    {
                                        *tmp.entry(label.to_string()).or_insert(0) += 1;
                                    }
                                }
                            }
                        }
                    }
                    if let Some(factor) = fused_factor {
                        for count in tmp.values_mut() {
                            *count = scale_fused(*count, factor)?;
                        }
                        tmp.retain(|_, count| *count > 0);
                    }
                    if is_count_next_step {
                        *source = vec![Traverser::start(
                            context,
                            TraverserValue::Computed(ElementProperty::Number(1.into()), None),
                        )];
                    } else {
                        let mut count_result = StreamMap::with_capacity(tmp.len());
                        for (label, count) in tmp {
                            count_result.insert(
                                TraverserValue::Computed(
                                    ElementProperty::String(label.into()),
                                    None,
                                ),
                                TraverserValue::Computed(
                                    ElementProperty::Number(count.into()),
                                    None,
                                ),
                            );
                        }
                        *source = vec![Traverser::start(
                            context,
                            TraverserValue::Object(count_result),
                        )];
                    }
                    count_out = 1;
                } else if count_only {
                    let count = if let Some(ids) = init_ids {
                        let mut cnt = 0;
                        for id_value in ids {
                            if let Some(node_index) =
                                located!(context.storage.get_vertex_by_id(id_value))?
                            {
                                if let Some(labels) = init_labels
                                    && !located!(crate::graph::storage::vertex_has_any_label(
                                        &*context.storage,
                                        node_index,
                                        labels
                                    ))?
                                {
                                    continue;
                                }
                                cnt += 1;
                            }
                        }
                        cnt
                    } else if let Some(labels) = init_labels {
                        match (
                            single_label(labels),
                            context.storage.statistics().with_step_frame(
                                || step.get_name(),
                                step_at,
                                || plan_frame(steps, level, logical_step_idx),
                            )?,
                        ) {
                            // One label: the statistics hold its count.
                            (Some(label), Some(stats)) => stats.vertex_label_count(label),
                            (Some(label), None) => {
                                located!(context.storage.vertices_by_label(label))?.count()
                            }
                            // D5, Bug 2: a vertex matching more than one requested
                            // label must be counted once, not once per matching label.
                            (None, _) => {
                                let mut seen: std::collections::HashSet<S::VertexId> =
                                    std::collections::HashSet::new();
                                for label in labels {
                                    for node_index in
                                        located!(context.storage.vertices_by_label(label))?
                                    {
                                        seen.insert(node_index);
                                    }
                                }
                                seen.len()
                            }
                        }
                    } else {
                        located!(context.storage.vertex_count())?
                    };
                    let count = match fused_factor {
                        Some(factor) => scale_fused(count, factor)?,
                        None => count,
                    };

                    *source = vec![Traverser::start(
                        context,
                        TraverserValue::Computed(ElementProperty::Number(count.into()), None),
                    )];
                    count_out = count;
                } else if let Some(ids) = init_ids {
                    for id_value in ids {
                        if let Some(node_index) =
                            located!(context.storage.get_vertex_by_id(id_value))?
                        {
                            if let Some(labels) = init_labels
                                && !located!(crate::graph::storage::vertex_has_any_label(
                                    &*context.storage,
                                    node_index,
                                    labels
                                ))?
                            {
                                continue;
                            }
                            tick_deadline(&mut context.deadline).with_step_frame(
                                || step.get_name(),
                                step_at,
                                || plan_frame(steps, level, logical_step_idx),
                            )?;
                            if !(property_filters.is_empty()
                                || located!(crate::traversal::step::PropertyFilter::all_vertex(
                                    property_filters,
                                    &*context.storage,
                                    node_index,
                                ))?)
                            {
                                continue;
                            }
                            let v1 = TraverserValue::Vertex(node_index);
                            source.push(Traverser::start(context, v1));
                        }
                    }
                    count_out = source.len();
                } else if let Some(candidates) = match (init_labels, step.index_access()) {
                    (Some(labels), Some(access)) => {
                        index_vertices(&*context.storage, labels, access).with_step_frame(
                            || step.get_name(),
                            step_at,
                            || plan_frame(steps, level, logical_step_idx),
                        )?
                    }
                    _ => None,
                } {
                    // A property index serves a condition (book page "Property
                    // Indexes"): its candidates (a superset) are checked like the
                    // label scan's elements.
                    for node_index in candidates {
                        tick_deadline(&mut context.deadline).with_step_frame(
                            || step.get_name(),
                            step_at,
                            || plan_frame(steps, level, logical_step_idx),
                        )?;
                        if !(property_filters.is_empty()
                            || located!(crate::traversal::step::PropertyFilter::all_vertex(
                                property_filters,
                                &*context.storage,
                                node_index,
                            ))?)
                        {
                            continue;
                        }
                        let v1 = TraverserValue::Vertex(node_index);
                        source.push(Traverser::start(context, v1));
                    }
                    count_out = source.len();
                } else if let Some(labels) = init_labels {
                    let single = single_label(labels);
                    if let Some(stats) = context.storage.statistics().with_step_frame(
                        || step.get_name(),
                        step_at,
                        || plan_frame(steps, level, logical_step_idx),
                    )? && property_filters.is_empty()
                    {
                        let expected: usize = match single {
                            Some(label) => stats.vertex_label_count(label),
                            None => labels
                                .iter()
                                .map(|label| stats.vertex_label_count(label))
                                .sum(),
                        };
                        source.reserve(expected);
                    }
                    // D5, Bug 2: a vertex carrying more than one requested label must
                    // be pushed exactly once, not once per matching label — otherwise
                    // `has_label("a","b").count()` over-counts a `{a,b}`-tagged vertex.
                    // One label needs no such check: the index holds a vertex once.
                    let mut seen: std::collections::HashSet<S::VertexId> =
                        std::collections::HashSet::new();
                    let labels: &[String] = match single {
                        Some(_) => &labels[..1],
                        None => labels,
                    };
                    for label in labels {
                        for node_index in located!(context.storage.vertices_by_label(label))? {
                            if single.is_none() && !seen.insert(node_index) {
                                continue;
                            }
                            tick_deadline(&mut context.deadline).with_step_frame(
                                || step.get_name(),
                                step_at,
                                || plan_frame(steps, level, logical_step_idx),
                            )?;
                            if !(property_filters.is_empty()
                                || located!(crate::traversal::step::PropertyFilter::all_vertex(
                                    property_filters,
                                    &*context.storage,
                                    node_index,
                                ))?)
                            {
                                continue;
                            }
                            let v1 = TraverserValue::Vertex(node_index);
                            source.push(Traverser::start(context, v1));
                        }
                    }
                    count_out = source.len();
                } else {
                    let v_count = located!(context.storage.vertex_count())?;
                    if property_filters.is_empty() {
                        source.reserve(v_count);
                    }
                    for node_index in located!(context.storage.vertices())? {
                        tick_deadline(&mut context.deadline).with_step_frame(
                            || step.get_name(),
                            step_at,
                            || plan_frame(steps, level, logical_step_idx),
                        )?;
                        if !(property_filters.is_empty()
                            || located!(crate::traversal::step::PropertyFilter::all_vertex(
                                property_filters,
                                &*context.storage,
                                node_index,
                            ))?)
                        {
                            continue;
                        }
                        let v1 = TraverserValue::Vertex(node_index);
                        source.push(Traverser::start(context, v1));
                    }
                    count_out = source.len();
                }
            }
            GraphNodeType::Edge => {
                if group_count_by_label {
                    let mut tmp: IndexMap<String, usize, FnvBuildHasher> = IndexMap::default();
                    if let Some(ids) = init_ids {
                        for id_value in ids {
                            if let Some(edge_index) =
                                located!(context.storage.get_edge_by_id(id_value))?
                                && let Some(e_lbl) =
                                    located!(context.storage.edge_label(edge_index))?
                            {
                                if let Some(labels) = init_labels
                                    && !labels.iter().any(|target| target == e_lbl)
                                {
                                    continue;
                                }
                                *tmp.entry(e_lbl.to_string()).or_insert(0) += 1;
                            }
                        }
                    } else if let Some(labels) = init_labels {
                        for label in labels {
                            let cnt = located!(context.storage.edges_by_label(label))?.count();
                            if cnt > 0 {
                                *tmp.entry(label.clone()).or_insert(0) += cnt;
                            }
                        }
                    } else {
                        match context.storage.statistics().with_step_frame(
                            || step.get_name(),
                            step_at,
                            || plan_frame(steps, level, logical_step_idx),
                        )? {
                            Some(stats) => {
                                for (label, cnt) in stats.edge_label_counts() {
                                    tmp.insert(label.to_string(), cnt);
                                }
                            }
                            None => {
                                for edge in located!(context.storage.edges())? {
                                    if let Some(label) = located!(context.storage.edge_label(edge))?
                                    {
                                        *tmp.entry(label.to_string()).or_insert(0) += 1;
                                    }
                                }
                            }
                        }
                    }
                    if let Some(factor) = fused_factor {
                        for count in tmp.values_mut() {
                            *count = scale_fused(*count, factor)?;
                        }
                        tmp.retain(|_, count| *count > 0);
                    }
                    if is_count_next_step {
                        *source = vec![Traverser::start(
                            context,
                            TraverserValue::Computed(ElementProperty::Number(1.into()), None),
                        )];
                    } else {
                        let mut count_result = StreamMap::with_capacity(tmp.len());
                        for (label, count) in tmp {
                            count_result.insert(
                                TraverserValue::Computed(
                                    ElementProperty::String(label.into()),
                                    None,
                                ),
                                TraverserValue::Computed(
                                    ElementProperty::Number(count.into()),
                                    None,
                                ),
                            );
                        }
                        *source = vec![Traverser::start(
                            context,
                            TraverserValue::Object(count_result),
                        )];
                    }
                    count_out = 1;
                } else if count_only {
                    let count = if let Some(ids) = init_ids {
                        let mut cnt = 0;
                        for id_value in ids {
                            if let Some(edge_index) =
                                located!(context.storage.get_edge_by_id(id_value))?
                            {
                                if let Some(labels) = init_labels {
                                    let e_lbl = located!(context.storage.edge_label(edge_index))?;
                                    if !e_lbl
                                        .is_some_and(|l| labels.iter().any(|target| target == l))
                                    {
                                        continue;
                                    }
                                }
                                cnt += 1;
                            }
                        }
                        cnt
                    } else if let Some(labels) = init_labels {
                        // An edge has exactly one label: the counts of the distinct
                        // requested labels add up.
                        let mut distinct: Vec<&str> = labels.iter().map(|l| l.as_str()).collect();
                        distinct.sort_unstable();
                        distinct.dedup();
                        match context.storage.statistics().with_step_frame(
                            || step.get_name(),
                            step_at,
                            || plan_frame(steps, level, logical_step_idx),
                        )? {
                            Some(stats) => distinct
                                .iter()
                                .map(|label| stats.edge_label_count(label))
                                .sum(),
                            None => distinct
                                .iter()
                                .map(|label| {
                                    located!(context.storage.edges_by_label(label))
                                        .map(Iterator::count)
                                })
                                .sum::<TraverserResult<usize>>()?,
                        }
                    } else {
                        located!(context.storage.edge_count())?
                    };
                    let count = match fused_factor {
                        Some(factor) => scale_fused(count, factor)?,
                        None => count,
                    };

                    *source = vec![Traverser::start(
                        context,
                        TraverserValue::Computed(ElementProperty::Number(count.into()), None),
                    )];
                    count_out = count;
                } else if let Some(ids) = init_ids {
                    for id_value in ids {
                        if let Some(edge_index) =
                            located!(context.storage.get_edge_by_id(id_value))?
                        {
                            if let Some(labels) = init_labels {
                                let e_lbl = located!(context.storage.edge_label(edge_index))?;
                                if !e_lbl.is_some_and(|l| labels.iter().any(|target| target == l)) {
                                    continue;
                                }
                            }
                            tick_deadline(&mut context.deadline).with_step_frame(
                                || step.get_name(),
                                step_at,
                                || plan_frame(steps, level, logical_step_idx),
                            )?;
                            if !(property_filters.is_empty()
                                || located!(crate::traversal::step::PropertyFilter::all_edge(
                                    property_filters,
                                    &*context.storage,
                                    edge_index,
                                ))?)
                            {
                                continue;
                            }
                            let v1 = TraverserValue::Edge(edge_index);
                            source.push(Traverser::start(context, v1));
                        }
                    }
                    count_out = source.len();
                } else if let Some(candidates) = match (init_labels, step.index_access()) {
                    (Some(labels), Some(access)) => index_edges(&*context.storage, labels, access)
                        .with_step_frame(
                            || step.get_name(),
                            step_at,
                            || plan_frame(steps, level, logical_step_idx),
                        )?,
                    _ => None,
                } {
                    // A property index serves a condition (see the vertex case).
                    for edge_index in candidates {
                        tick_deadline(&mut context.deadline).with_step_frame(
                            || step.get_name(),
                            step_at,
                            || plan_frame(steps, level, logical_step_idx),
                        )?;
                        if !(property_filters.is_empty()
                            || located!(crate::traversal::step::PropertyFilter::all_edge(
                                property_filters,
                                &*context.storage,
                                edge_index,
                            ))?)
                        {
                            continue;
                        }
                        let v1 = TraverserValue::Edge(edge_index);
                        source.push(Traverser::start(context, v1));
                    }
                    count_out = source.len();
                } else if let Some(labels) = init_labels {
                    for label in labels {
                        for edge_index in located!(context.storage.edges_by_label(label))? {
                            tick_deadline(&mut context.deadline).with_step_frame(
                                || step.get_name(),
                                step_at,
                                || plan_frame(steps, level, logical_step_idx),
                            )?;
                            if !(property_filters.is_empty()
                                || located!(crate::traversal::step::PropertyFilter::all_edge(
                                    property_filters,
                                    &*context.storage,
                                    edge_index,
                                ))?)
                            {
                                continue;
                            }
                            let v1 = TraverserValue::Edge(edge_index);
                            source.push(Traverser::start(context, v1));
                        }
                    }
                    count_out = source.len();
                } else {
                    let e_count = located!(context.storage.edge_count())?;
                    if property_filters.is_empty() {
                        source.reserve(e_count);
                    }
                    for edge_index in located!(context.storage.edges())? {
                        tick_deadline(&mut context.deadline).with_step_frame(
                            || step.get_name(),
                            step_at,
                            || plan_frame(steps, level, logical_step_idx),
                        )?;
                        if !(property_filters.is_empty()
                            || located!(crate::traversal::step::PropertyFilter::all_edge(
                                property_filters,
                                &*context.storage,
                                edge_index,
                            ))?)
                        {
                            continue;
                        }
                        let v1 = TraverserValue::Edge(edge_index);
                        source.push(Traverser::start(context, v1));
                    }
                    count_out = source.len();
                }
            }
        }
        // A bare `count()` immediately after this `Init` step, not folded into it by
        // `CountPushdownRule` — either the rule is disabled for this execution
        // (`optimizer.disabled`), or a future rule ever stops firing here for some
        // other reason. `count_only`/`group_count_by_label` already produce a single
        // count traverser directly above without ever materializing a full element
        // list, so this only applies to the plain id/label/unfiltered fetch branches,
        // matching the same `is_count_next_step` collapse every other step category
        // already performs (see `GraphStep::Append`/`GraphStep::Map` below). Keeps a
        // disabled `count_pushdown` correct, just less optimal: still counts, just via
        // materializing first instead of the index-backed fast path.
        if let Some(incoming) = incoming.filter(|_| fused_factor.is_none()) {
            let found = std::mem::take(source);
            // The cross product leaves `incoming x found` traversers alive: check it
            // against `traversal.max_traversers` (and the memory budget) BEFORE
            // building it, then reserve exactly that many, fallibly: a product the
            // allocator cannot provide (the limit raised to "unlimited") is an error,
            // never an abort of the process.
            let product = incoming.len().saturating_mul(found.len());
            context
                .check_traverser_limit(product)
                .and_then(|()| context.check_memory_budget(product))
                .and_then(|()| crate::traversal::capacity::try_reserve_exact(source, product))
                .with_step_frame(
                    || step.get_name(),
                    step_at,
                    || plan_frame(steps, level, logical_step_idx),
                )?;
            for traverser in &incoming {
                tick_deadline(&mut context.deadline).with_step_frame(
                    || step.get_name(),
                    step_at,
                    || plan_frame(steps, level, logical_step_idx),
                )?;
                for element in &found {
                    let mut next = Traverser::with_parts(
                        element.value.clone(),
                        traverser.path,
                        traverser.bulk,
                        traverser.sack,
                    );
                    next.extend_path(&mut context.paths, mode)?;
                    source.push(next);
                }
            }
            count_out = source.len();
        }
        if is_count_next_step && !count_only && !group_count_by_label {
            let materialized = bulk::total_bulk(context, source, "count")?;
            *source = vec![Traverser::start(
                context,
                TraverserValue::Computed(
                    bulk::count_property(note_collapsed(collapsed, materialized), "count")?,
                    None,
                ),
            )];
            count_out = 1;
        }
        // New traversers start their path here; ones already on the stream keep theirs.
        if matches!(mode, PathMode::Full) {
            for traverser in source.iter_mut() {
                if traverser.path.is_none() {
                    traverser.path =
                        context
                            .paths
                            .push(PathRef::NONE, traverser.value.clone(), None)?;
                }
            }
        }
        Ok(count_out)
    }

    pub(crate) fn execute_optimized_steps(
        steps: &[GraphStep<'g, S>],
        initial_source: Vec<Traverser<S>>,
        context: &mut TraverserContext<'_, S>,
        at: LevelAt,
        level: Option<&LevelPlan<'g, S>>,
        path_modes: &[PathMode],
    ) -> TraverserResult<Vec<Traverser<S>>> {
        let strategy = context.strategy;
        let is_profile = strategy.contains(TraversalStrategy::ProfileEnabled);
        let profile_memory = is_profile && context.profile_type.contains(ProfileType::Memory);

        let mut source: Vec<Traverser<S>> = initial_source;

        let mut physical = 0;
        let mut logical_step_idx = 0;
        // Whether every incoming traverser already carries an empty path. Traversers handed in by
        // an enclosing level may carry one, so this starts out `false`.
        let mut incoming_untracked = false;

        // The traversers a step folded into one count for the fused `count()` after it: that
        // step's real output, and the real input of the `count()` (see `note_collapsed`).
        let mut counted_in: Option<usize> = None;

        while let Some(current_step) = steps.get(physical) {
            let mode = path_mode_at(path_modes, physical);
            let mut collapsed: Option<usize> = None;
            // Whether this step has path work to do on each output: record (`Full`), or drop an
            // incoming path nobody reads anymore (`None`). `Labels` leaves paths untouched, and a
            // stream that is already untracked needs nothing, so the common query without a path
            // consumer runs the same per-item code as before path tracking existed.
            let touch_paths = match mode {
                PathMode::Full => true,
                PathMode::Labels(_) => false,
                PathMode::None => !incoming_untracked,
            };
            let mut next_physical = physical + 1;
            while let Some(GraphStep::Modulator(_, modulator)) = steps.get(next_physical) {
                // The loop never visits a modulator on its own, so a `by()` no step took fails
                // here, before the step it follows runs and whatever the stream holds.
                let logical = logical_step_idx
                    + usize::from(!matches!(current_step, GraphStep::Modulator(_, _)));
                let target = modulated_position(steps, next_physical, logical);
                modulator.check().with_step_frame(
                    || target_name(steps, next_physical),
                    at.step(target),
                    || plan_frame(steps, level, target),
                )?;
                next_physical += 1;
            }
            let next_step = steps.get(next_physical);
            let is_count_next_step = Self::is_count_step(next_step);

            // The step's position in the plan: its id segment and its failure location.
            let step_at = at.step(logical_step_idx);

            // Step boundary: the only place a mutating step can be interrupted, so each one is
            // applied completely or not at all.
            if !matches!(current_step, GraphStep::Modulator(_, _)) {
                context.check_deadline().with_step_frame(
                    || current_step.get_name(),
                    step_at,
                    || plan_frame(steps, level, logical_step_idx),
                )?;
            }

            if !matches!(current_step, GraphStep::Modulator(_, _)) && is_profile {
                context
                    .profile
                    .enter_step(step_at.child, step_at.index, || {
                        profile_step(current_step, mode)
                    });
            }
            context.path_mode = mode.clone();

            let start = Instant::now();
            let count_before = match current_step {
                GraphStep::Count(_, _) => counted_in.take().unwrap_or(source.len()),
                _ => source.len(),
            };
            // The property statistics' items: the traversers (with bulk) reaching each
            // top-level step, counted only when the run records workload statistics.
            if at.child.is_none()
                && !matches!(current_step, GraphStep::Modulator(_, _))
                && let Some(inputs) = context.step_inputs.as_mut()
            {
                note_step_input(inputs, step_at.index, &source);
            }
            let mut count_out = 0;
            let alloc_before = AllocSnapshot::capture(profile_memory);

            match current_step {
                GraphStep::Init(_, step) => {
                    count_out = Self::execute_init_step(
                        context,
                        &**step,
                        &mut source,
                        physical,
                        steps,
                        level,
                        step_at,
                        logical_step_idx,
                        is_count_next_step,
                        &mut collapsed,
                        mode,
                    )?;
                }
                GraphStep::Append(_name, step) => {
                    let added = step.process(context).with_step_frame(
                        || step.get_name(),
                        step_at,
                        || plan_frame(steps, level, logical_step_idx),
                    )?;
                    for traverser in source.iter_mut() {
                        traverser.keep_path(mode);
                    }
                    source.reserve(added.len());
                    for traverser in added {
                        source.push(traverser.start_path(&mut context.paths, mode)?);
                    }
                    if is_count_next_step {
                        let total = bulk::total_bulk(context, &source, "count")?;
                        source = vec![
                            Traverser::start(
                                context,
                                TraverserValue::Computed(
                                    bulk::count_property(
                                        note_collapsed(&mut collapsed, total),
                                        "count",
                                    )?,
                                    None,
                                ),
                            )
                            .start_path(&mut context.paths, mode)?,
                        ];
                    }
                    count_out = source.len();
                }
                GraphStep::Map(_name, step) => {
                    if is_count_next_step && !step.may_filter() {
                        let total = bulk::total_bulk(context, &source, "count")?;
                        source = vec![
                            Traverser::start(
                                context,
                                TraverserValue::Computed(
                                    bulk::count_property(
                                        note_collapsed(&mut collapsed, total),
                                        "count",
                                    )?,
                                    None,
                                ),
                            )
                            .start_path(&mut context.paths, mode)?,
                        ];
                        count_out = count_before;
                    } else {
                        let extends = step.extends_path();
                        let mut result: Vec<Traverser<S>> = Vec::with_capacity(count_before);
                        let batch = step.batch_size();
                        if batch > 1 {
                            // One call per batch of at most `batch` traversers (an embedder's
                            // `max_batch`), in input order; the buffer is bounded by `batch`.
                            let mut pending = source.into_iter();
                            loop {
                                let chunk: Vec<Traverser<S>> =
                                    pending.by_ref().take(batch).collect();
                                if chunk.is_empty() {
                                    break;
                                }
                                context.tick().with_step_frame(
                                    || step.get_name(),
                                    step_at,
                                    || plan_frame(steps, level, logical_step_idx),
                                )?;
                                let mapped = step.process_batch(context, chunk).with_step_frame(
                                    || step.get_name(),
                                    step_at,
                                    || plan_frame(steps, level, logical_step_idx),
                                )?;
                                for mut mapped in mapped {
                                    if extends {
                                        mapped.extend_path(&mut context.paths, mode)?;
                                    } else {
                                        mapped.keep_path(mode);
                                    }
                                    result.push(mapped);
                                }
                            }
                        } else if step.may_filter() {
                            for src in source.into_iter() {
                                context.tick().with_step_frame(
                                    || step.get_name(),
                                    step_at,
                                    || plan_frame(steps, level, logical_step_idx),
                                )?;
                                let mapped = step.process_filter(context, src).with_step_frame(
                                    || step.get_name(),
                                    step_at,
                                    || plan_frame(steps, level, logical_step_idx),
                                )?;
                                if let Some(mut mapped) = mapped {
                                    if extends {
                                        mapped.extend_path(&mut context.paths, mode)?;
                                    } else {
                                        mapped.keep_path(mode);
                                    }
                                    result.push(mapped);
                                }
                            }
                        } else if touch_paths {
                            for src in source.into_iter() {
                                context.tick().with_step_frame(
                                    || step.get_name(),
                                    step_at,
                                    || plan_frame(steps, level, logical_step_idx),
                                )?;
                                let mut mapped = step.process(context, src).with_step_frame(
                                    || step.get_name(),
                                    step_at,
                                    || plan_frame(steps, level, logical_step_idx),
                                )?;
                                if extends {
                                    mapped.extend_path(&mut context.paths, mode)?;
                                } else {
                                    mapped.keep_path(mode);
                                }
                                result.push(mapped);
                            }
                        } else {
                            for src in source.into_iter() {
                                context.tick().with_step_frame(
                                    || step.get_name(),
                                    step_at,
                                    || plan_frame(steps, level, logical_step_idx),
                                )?;
                                result.push(step.process(context, src).with_step_frame(
                                    || step.get_name(),
                                    step_at,
                                    || plan_frame(steps, level, logical_step_idx),
                                )?);
                            }
                        }
                        count_out = result.len();
                        if is_count_next_step {
                            let total = bulk::total_bulk(context, &result, "count")?;
                            source = vec![Traverser::start(
                                context,
                                TraverserValue::Computed(
                                    bulk::count_property(
                                        note_collapsed(&mut collapsed, total),
                                        "count",
                                    )?,
                                    None,
                                ),
                            )];
                        } else {
                            source = result;
                        }
                    }
                }
                GraphStep::Barrier(_name, step) => {
                    // A seeded reducing barrier (`fold()`/`group()`/`groupCount()`/`tree()`/
                    // `cap()`, `always_emits() == true`) always produces exactly one row, whether
                    // or not any traverser reached it (TinkerPop: `fold()` -> `[]`,
                    // `group()`/`groupCount()`/`tree()` -> `{}`, `cap()` reads only side effects).
                    // A fused `count()` can therefore learn the output count without
                    // materializing that row.
                    if is_count_next_step && step.always_emits() {
                        source = vec![Traverser::start(
                            context,
                            TraverserValue::Computed(ElementProperty::Number(1.into()), None),
                        )];
                        count_out = 1;
                    } else {
                        match step.process(context, source).with_step_frame(
                            || step.get_name(),
                            step_at,
                            || plan_frame(steps, level, logical_step_idx),
                        )? {
                            Some(traverser) => {
                                if is_count_next_step {
                                    source = vec![Traverser::start(
                                        context,
                                        TraverserValue::Computed(
                                            ElementProperty::Number(1.into()),
                                            None,
                                        ),
                                    )];
                                } else {
                                    source = vec![traverser.start_path(&mut context.paths, mode)?];
                                }
                                count_out = 1;
                            }
                            None => {
                                if is_count_next_step {
                                    source = vec![Traverser::start(
                                        context,
                                        TraverserValue::Computed(
                                            ElementProperty::Number(0.into()),
                                            None,
                                        ),
                                    )];
                                } else {
                                    source = Vec::new();
                                }
                                count_out = 0;
                            }
                        }
                    }
                }
                GraphStep::FlatBarrier(_name, step) => {
                    let r = step.process(context, source).with_step_frame(
                        || step.get_name(),
                        step_at,
                        || plan_frame(steps, level, logical_step_idx),
                    )?;
                    source = r.unwrap_or_default();
                    if touch_paths {
                        for traverser in source.iter_mut() {
                            traverser.keep_path(mode);
                        }
                    }
                    if is_count_next_step {
                        let total = bulk::total_bulk(context, &source, "count")?;
                        source = vec![
                            Traverser::start(
                                context,
                                TraverserValue::Computed(
                                    bulk::count_property(
                                        note_collapsed(&mut collapsed, total),
                                        "count",
                                    )?,
                                    None,
                                ),
                            )
                            .start_path(&mut context.paths, mode)?,
                        ];
                    }
                    count_out = source.len();
                }
                GraphStep::Filter(_name, step) => {
                    let mut result: Vec<Traverser<S>> = vec![];
                    // `count` is the logical count (sum of bulks), `kept` the traverser objects.
                    let mut count: u64 = 0;
                    let mut kept: usize = 0;
                    for v in source.into_iter() {
                        context.tick().with_step_frame(
                            || step.get_name(),
                            step_at,
                            || plan_frame(steps, level, logical_step_idx),
                        )?;
                        let surviving = step.surviving_bulk(context, &v).with_step_frame(
                            || step.get_name(),
                            step_at,
                            || plan_frame(steps, level, logical_step_idx),
                        )?;
                        if surviving > 0 {
                            kept += 1;
                            count = count.checked_add(surviving).ok_or_else(|| {
                                Box::new(TraverserError::BulkOverflow {
                                    step: "count".to_string(),
                                })
                            })?;
                            if !is_count_next_step {
                                let mut v = v;
                                v.bulk = surviving;
                                if touch_paths {
                                    v.keep_path(mode);
                                }
                                result.push(v);
                            }
                        }
                    }
                    if is_count_next_step {
                        source = vec![
                            Traverser::start(
                                context,
                                TraverserValue::Computed(
                                    bulk::count_property(
                                        note_collapsed(&mut collapsed, count),
                                        "count",
                                    )?,
                                    None,
                                ),
                            )
                            .start_path(&mut context.paths, mode)?,
                        ];
                    } else {
                        source = result;
                    }
                    count_out = kept;
                }
                GraphStep::FlatMap(_name, step) => {
                    let mut result: Vec<Traverser<S>> = vec![];
                    let mut count: u64 = 0;
                    let mut produced_total: usize = 0;
                    if step.is_batch() {
                        // A branching step with a stateful child sees the whole stream at once.
                        context.tick_n(source.len()).with_step_frame(
                            || step.get_name(),
                            step_at,
                            || plan_frame(steps, level, logical_step_idx),
                        )?;
                        step.process_batch(context, std::mem::take(&mut source), &mut result)
                            .with_step_frame(
                                || step.get_name(),
                                step_at,
                                || plan_frame(steps, level, logical_step_idx),
                            )?;
                        produced_total = result.len();
                        context.tick_n(produced_total).with_step_frame(
                            || step.get_name(),
                            step_at,
                            || plan_frame(steps, level, logical_step_idx),
                        )?;
                        if touch_paths {
                            let extends = step.extends_path();
                            for item in &mut result {
                                if extends {
                                    item.extend_path(&mut context.paths, mode)?;
                                } else {
                                    item.keep_path(mode);
                                }
                            }
                        }
                        if is_count_next_step {
                            let total = bulk::total_bulk(context, &result, "count")?;
                            source = vec![
                                Traverser::start(
                                    context,
                                    TraverserValue::Computed(
                                        bulk::count_property(
                                            note_collapsed(&mut collapsed, total),
                                            "count",
                                        )?,
                                        None,
                                    ),
                                )
                                .start_path(&mut context.paths, mode)?,
                            ];
                        } else {
                            source = result;
                        }
                    } else if is_count_next_step {
                        for v in source.into_iter() {
                            context.tick().with_step_frame(
                                || step.get_name(),
                                step_at,
                                || plan_frame(steps, level, logical_step_idx),
                            )?;
                            let counted = step.count(context, v).with_step_frame(
                                || step.get_name(),
                                step_at,
                                || plan_frame(steps, level, logical_step_idx),
                            )?;
                            count = count.checked_add(counted).ok_or_else(|| {
                                Box::new(TraverserError::BulkOverflow {
                                    step: "count".to_string(),
                                })
                            })?;
                        }
                        produced_total = usize::try_from(count).unwrap_or(usize::MAX);
                        source = vec![
                            Traverser::start(
                                context,
                                TraverserValue::Computed(
                                    bulk::count_property(
                                        note_collapsed(&mut collapsed, count),
                                        "count",
                                    )?,
                                    None,
                                ),
                            )
                            .start_path(&mut context.paths, mode)?,
                        ];
                    } else {
                        let extends = step.extends_path();
                        for v in source.into_iter() {
                            context.tick().with_step_frame(
                                || step.get_name(),
                                step_at,
                                || plan_frame(steps, level, logical_step_idx),
                            )?;
                            let before = result.len();
                            step.process(context, v, &mut result).with_step_frame(
                                || step.get_name(),
                                step_at,
                                || plan_frame(steps, level, logical_step_idx),
                            )?;
                            let produced = result.len() - before;
                            // One tick per produced child, counted in bulk.
                            context.tick_n(produced).with_step_frame(
                                || step.get_name(),
                                step_at,
                                || plan_frame(steps, level, logical_step_idx),
                            )?;
                            produced_total += produced;
                            if touch_paths && produced > 0 {
                                for item in &mut result[before..] {
                                    if extends {
                                        item.extend_path(&mut context.paths, mode)?;
                                    } else {
                                        item.keep_path(mode);
                                    }
                                }
                            }
                        }
                        source = result;
                    }
                    count_out = produced_total;
                }
                GraphStep::As(_name, step) => {
                    for traverser in source.iter_mut() {
                        step.process(context, traverser).with_step_frame(
                            || step.get_name(),
                            step_at,
                            || plan_frame(steps, level, logical_step_idx),
                        )?;
                    }
                    count_out = source.len();
                }
                GraphStep::SideEffect(_, step) => {
                    step.declare_side_effect(&mut context.side_effect);
                    for traverser in source.iter_mut() {
                        context.tick().with_step_frame(
                            || step.get_name(),
                            step_at,
                            || plan_frame(steps, level, logical_step_idx),
                        )?;
                        step.process(context, traverser).with_step_frame(
                            || step.get_name(),
                            step_at,
                            || plan_frame(steps, level, logical_step_idx),
                        )?;
                        if touch_paths {
                            traverser.keep_path(mode);
                        }
                    }
                    step.finish_batch(context).with_step_frame(
                        || step.get_name(),
                        step_at,
                        || plan_frame(steps, level, logical_step_idx),
                    )?;
                    if is_count_next_step {
                        let total = bulk::total_bulk(context, &source, "count")?;
                        source = vec![
                            Traverser::start(
                                context,
                                TraverserValue::Computed(
                                    bulk::count_property(
                                        note_collapsed(&mut collapsed, total),
                                        "count",
                                    )?,
                                    None,
                                ),
                            )
                            .start_path(&mut context.paths, mode)?,
                        ];
                    }
                    count_out = source.len();
                }
                GraphStep::SideEffectMut(name, step) => {
                    let extends = step.extends_path();
                    tracing::trace!(">>> step: {name}; source: {source:?}");
                    // A mutating step is applied completely or not at all: the deadline is
                    // suspended while it runs (nested traversals included) and checked again at
                    // the next step boundary; a timeout there rolls back the whole traversal.
                    let deadline = context.deadline.take();
                    let applied = (|| -> TraverserResult<(Vec<Traverser<S>>, u64)> {
                        let mut result: Vec<Traverser<S>> = vec![];
                        let mut count: u64 = 0;
                        // Only the first step of the root traversal is a start step
                        // (`g.addV(..)`, `g.addE(..).from(..).to(..)`, `g.mergeV(..)`): it runs
                        // once with no incoming traverser. Anywhere else an empty stream means
                        // the upstream produced nothing, and a mutating step over nothing does
                        // nothing (`g.V().has("name", "nobody").addE("x").from("a")` yields
                        // nothing, as in TinkerPop), so it is not called at all.
                        let is_start = physical == 0 && at.child.is_none();
                        if source.is_empty() {
                            if !is_start {
                                // Nothing to mutate, nothing to emit.
                                return Ok((result, count));
                            }
                            let tmp_res = step.process(context, None).with_step_frame(
                                || step.get_name(),
                                step_at,
                                || plan_frame(steps, level, logical_step_idx),
                            )?;
                            count =
                                count.saturating_add(bulk::total_bulk(context, &tmp_res, name)?);
                            if !is_count_next_step {
                                for mut item in tmp_res.into_iter() {
                                    if extends {
                                        item.extend_path(&mut context.paths, mode)?;
                                    } else {
                                        item.keep_path(mode);
                                    }
                                    result.push(item);
                                }
                            }
                        } else {
                            for mut traverser in source.into_iter() {
                                // Defensive: a plan that mutates never merges (`bulk_merge` is
                                // off), so the bulk is 1 here. Should one arrive anyway it is
                                // applied once per unit, and a bulk of 1 costs no clone.
                                let copies = traverser.bulk.max(1);
                                traverser.bulk = 1;
                                let mut pending = Some(traverser);
                                for copy in 0..copies {
                                    let unit = if copy + 1 == copies {
                                        pending.take()
                                    } else {
                                        pending.clone()
                                    };
                                    let tmp_res = step.process(context, unit).with_step_frame(
                                        || step.get_name(),
                                        step_at,
                                        || plan_frame(steps, level, logical_step_idx),
                                    )?;
                                    count = count
                                        .saturating_add(bulk::total_bulk(context, &tmp_res, name)?);
                                    if !is_count_next_step {
                                        for mut item in tmp_res.into_iter() {
                                            if extends {
                                                item.extend_path(&mut context.paths, mode)?;
                                            } else {
                                                item.keep_path(mode);
                                            }
                                            result.push(item);
                                        }
                                    }
                                }
                            }
                        }
                        Ok((result, count))
                    })();
                    context.deadline = deadline;
                    let (result, count) = applied?;
                    count_out = usize::try_from(count).unwrap_or(usize::MAX);
                    if is_count_next_step {
                        source = vec![
                            Traverser::start(
                                context,
                                TraverserValue::Computed(
                                    bulk::count_property(
                                        note_collapsed(&mut collapsed, count),
                                        name,
                                    )?,
                                    None,
                                ),
                            )
                            .start_path(&mut context.paths, mode)?,
                        ];
                    } else {
                        source = result;
                    }
                    tracing::trace!("<<< step: {name}; source: {source:?}");
                }
                GraphStep::Count(_, _step) => {
                    if logical_step_idx == 0 {
                        let count = bulk::total_bulk(context, &source, "count")?;
                        source = vec![
                            Traverser::start(
                                context,
                                TraverserValue::Computed(
                                    bulk::count_property(count, "count")?,
                                    None,
                                ),
                            )
                            .start_path(&mut context.paths, mode)?,
                        ];
                    }
                    // The count traverser was created by the step before a fused `count()`; it
                    // starts its own path, like any other barrier output.
                    if matches!(mode, PathMode::Full) {
                        let counted = std::mem::take(&mut source);
                        for traverser in counted {
                            source.push(traverser.start_path(&mut context.paths, mode)?);
                        }
                    }
                    count_out = 1;
                }
                GraphStep::Modulator(_, step) => {
                    // A modulator was handed to its step at build time; only a `by()` no step took
                    // fails here. Every step of a level is visited, whatever the stream holds.
                    let target = modulated_position(steps, physical, logical_step_idx);
                    step.check().with_step_frame(
                        || target_name(steps, physical),
                        at.step(target),
                        || plan_frame(steps, level, target),
                    )?;
                }
            }

            let duration = start.elapsed();
            if let Some(folded) = collapsed {
                count_out = folded;
                counted_in = Some(folded);
            }

            if !matches!(current_step, GraphStep::Modulator(_, _)) {
                context
                    .check_traverser_limit(source.len())
                    .and_then(|()| context.check_memory_budget(source.len()))
                    .with_step_frame(
                        || current_step.get_name(),
                        step_at,
                        || plan_frame(steps, level, logical_step_idx),
                    )?;
                if is_profile {
                    let memory = Self::step_memory_metrics(
                        current_step,
                        &source,
                        alloc_before,
                        profile_memory,
                    );
                    // Merged traversers carry a bulk: the row's logical output is the sum of the
                    // bulks (only meaningful when the step's output is `source` itself).
                    let bulk_extra = if context.bulk_active && count_out == source.len() {
                        let logical =
                            bulk::total_bulk(context, &source, "profile").unwrap_or(u64::MAX);
                        logical.saturating_sub(source.len() as u64)
                    } else {
                        0
                    };
                    context.profile.exit_step(
                        duration,
                        count_before,
                        count_out,
                        bulk_extra,
                        memory,
                    );
                }
                logical_step_idx += 1;
            }

            if !matches!(current_step, GraphStep::Modulator(_, _)) {
                // After a `None` step every output path is empty: either it was dropped here, or
                // the stream already was untracked.
                incoming_untracked = mode.is_none();
            }
            physical = next_physical;
        }

        Ok(source)
    }

    pub(crate) fn execute_optimized_steps_exists(
        steps: &[GraphStep<'g, S>],
        source: &Traverser<S>,
        context: &mut TraverserContext<'_, S>,
        child: usize,
        level: Option<&LevelPlan<'g, S>>,
        path_modes: &[PathMode],
    ) -> TraverserResult<bool> {
        Self::exists_in_level(
            steps,
            steps,
            source,
            context,
            Some(child),
            0,
            level,
            path_modes,
            None,
        )
    }

    /// The first traverser `steps` produce from `source`, or `None` when they produce nothing,
    /// like TinkerPop's `TraversalUtil.produce()`. Uses the same short-circuiting, depth-first
    /// evaluation as [`Self::execute_optimized_steps_exists`], so the child stops at its first
    /// result instead of running to completion; a level holding a barrier (`fold()`, `count()`,
    /// `order()`, ...) is run in full and its first output is taken. Depth-first order visits
    /// the outputs in the same order as the batched execution, so "first" is the same traverser
    /// `to_list()` would put first.
    pub(crate) fn execute_optimized_steps_first(
        steps: &[GraphStep<'g, S>],
        source: &Traverser<S>,
        context: &mut TraverserContext<'_, S>,
        child: usize,
        level: Option<&LevelPlan<'g, S>>,
        path_modes: &[PathMode],
    ) -> TraverserResult<Option<Traverser<S>>> {
        let mut first = None;
        Self::exists_in_level(
            steps,
            steps,
            source,
            context,
            Some(child),
            0,
            level,
            path_modes,
            Some(&mut first),
        )?;
        Ok(first)
    }

    /// Fails when a level holds a `by()` no step took (`StepByRejected`), with the frame of the
    /// step it follows. A plain scan: every other modulator passes.
    fn check_modulators(
        steps: &[GraphStep<'g, S>],
        level: Option<&LevelPlan<'g, S>>,
        child: Option<usize>,
    ) -> TraverserResult<()> {
        let mut logical = 0;
        for (physical, step) in steps.iter().enumerate() {
            match step {
                GraphStep::Modulator(_, modulator) => {
                    let target = modulated_position(steps, physical, logical);
                    modulator.check().with_step_frame(
                        || target_name(steps, physical),
                        StepAt {
                            child,
                            index: target,
                        },
                        || plan_frame(steps, level, target),
                    )?;
                }
                _ => logical += 1,
            }
        }
        Ok(())
    }

    /// Short-circuiting evaluation of one traversal level.
    ///
    /// `level` keeps the full plan of the level while `steps` walks its remaining suffix, so a
    /// failure can report the whole level plan with the failing step marked. `child` is the
    /// child index of the level (`None` at the top level) and `position` the position of
    /// `steps[0]` in it: together the id of the step being evaluated.
    ///
    /// `first`, when given, receives the traverser that made the level productive (see
    /// [`Self::execute_optimized_steps_first`]); an existence check passes `None` and clones
    /// nothing.
    #[allow(clippy::too_many_arguments)]
    fn exists_in_level(
        steps_of_level: &[GraphStep<'g, S>],
        steps: &[GraphStep<'g, S>],
        source: &Traverser<S>,
        context: &mut TraverserContext<'_, S>,
        child: Option<usize>,
        position: usize,
        level: Option<&LevelPlan<'g, S>>,
        path_modes: &[PathMode],
        mut first: Option<&mut Option<Traverser<S>>>,
    ) -> TraverserResult<bool> {
        if steps.is_empty() {
            if let Some(first) = first {
                *first = Some(source.clone());
            }
            return Ok(true);
        }
        // `steps` is a suffix of `steps_of_level`; the modes are indexed by physical position.
        let physical = steps_of_level.len() - steps.len();
        let suffix_modes = path_modes.get(physical..).unwrap_or(&[]);
        let mode = path_mode_at(path_modes, physical);

        // Short-circuiting may never reach a later step, so a `by()` no step took is checked
        // once on entry to the level, whatever the traverser does.
        if physical == 0 {
            Self::check_modulators(steps_of_level, level, child)?;
        }

        if steps.iter().any(|s| {
            matches!(
                s,
                GraphStep::Barrier(_, _)
                    | GraphStep::FlatBarrier(_, _)
                    | GraphStep::Count(_, _)
                    | GraphStep::Append(_, _)
            )
        }) {
            let res = Self::execute_optimized_steps(
                steps,
                vec![source.clone()],
                context,
                LevelAt {
                    child,
                    first: position,
                },
                level,
                suffix_modes,
            )?;
            return Ok(Self::take_first(res, first));
        }

        let strategy = context.strategy;
        let is_profile = strategy.contains(TraversalStrategy::ProfileEnabled);
        let profile_memory = is_profile && context.profile_type.contains(ProfileType::Memory);

        let current_step = &steps[0];
        let remaining_steps = &steps[1..];
        context.path_mode = mode.clone();

        // Short-circuiting evaluation recurses once per step and traverser, so the deadline is
        // counted here like a per-traverser site rather than read on every entry.
        if !matches!(current_step, GraphStep::Modulator(_, _)) {
            context.tick().with_step_frame(
                || current_step.get_name(),
                StepAt {
                    child,
                    index: position,
                },
                || plan_frame(steps_of_level, level, position),
            )?;
        }

        if matches!(current_step, GraphStep::Modulator(_, _)) {
            return Self::exists_in_level(
                steps_of_level,
                remaining_steps,
                source,
                context,
                child,
                position,
                level,
                path_modes,
                first,
            );
        }

        match current_step {
            GraphStep::Filter(_, step) => {
                if is_profile {
                    context
                        .profile
                        .enter_step(child, position, || profile_step(current_step, mode));
                }
                let alloc_before = AllocSnapshot::capture(profile_memory);
                let start = Instant::now();
                let matches = step.process(context, source).with_step_frame(
                    || step.get_name(),
                    StepAt {
                        child,
                        index: position,
                    },
                    || plan_frame(steps_of_level, level, position),
                )?;
                if is_profile {
                    let duration = start.elapsed();
                    let count_out = if matches { 1 } else { 0 };
                    let memory =
                        Self::step_memory_metrics(current_step, &[], alloc_before, profile_memory);
                    context.profile.exit_step(duration, 1, count_out, 0, memory);
                }
                if matches {
                    Self::exists_in_level(
                        steps_of_level,
                        remaining_steps,
                        source,
                        context,
                        child,
                        position + 1,
                        level,
                        path_modes,
                        first,
                    )
                } else {
                    Ok(false)
                }
            }
            GraphStep::Map(_, step) => {
                if is_profile {
                    context
                        .profile
                        .enter_step(child, position, || profile_step(current_step, mode));
                }
                let alloc_before = AllocSnapshot::capture(profile_memory);
                let start = Instant::now();
                let mapped = step
                    .process_filter(context, source.clone())
                    .with_step_frame(
                        || step.get_name(),
                        StepAt {
                            child,
                            index: position,
                        },
                        || plan_frame(steps_of_level, level, position),
                    )?;
                if is_profile {
                    let duration = start.elapsed();
                    let count_out = usize::from(mapped.is_some());
                    let output = mapped.as_slice();
                    let memory = Self::step_memory_metrics(
                        current_step,
                        output,
                        alloc_before,
                        profile_memory,
                    );
                    context.profile.exit_step(duration, 1, count_out, 0, memory);
                }
                let Some(mut mapped) = mapped else {
                    return Ok(false);
                };
                if step.extends_path() {
                    mapped.extend_path(&mut context.paths, mode)?;
                } else {
                    mapped.keep_path(mode);
                }
                Self::exists_in_level(
                    steps_of_level,
                    remaining_steps,
                    &mapped,
                    context,
                    child,
                    position + 1,
                    level,
                    path_modes,
                    first,
                )
            }
            GraphStep::FlatMap(_, step) => {
                if is_profile {
                    context
                        .profile
                        .enter_step(child, position, || profile_step(current_step, mode));
                }
                let alloc_before = AllocSnapshot::capture(profile_memory);
                let start = Instant::now();
                // A buffer of the execution's pool, owned by this call while it iterates the
                // produced items and recurses per item (a nested `FlatMap` takes its own), so a
                // child run per traverser reuses buffers instead of allocating one per call.
                let mut buf = context.take_exists_scratch();
                step.process(context, source.clone(), &mut buf)
                    .with_step_frame(
                        || step.get_name(),
                        StepAt {
                            child,
                            index: position,
                        },
                        || plan_frame(steps_of_level, level, position),
                    )?;
                if is_profile {
                    let duration = start.elapsed();
                    let count_out = buf.len();
                    let output = &buf[..];
                    let memory = Self::step_memory_metrics(
                        current_step,
                        output,
                        alloc_before,
                        profile_memory,
                    );
                    context.profile.exit_step(duration, 1, count_out, 0, memory);
                }
                let extends = step.extends_path();
                let mut found = false;
                // In production order, so the first productive item is the one the batched
                // execution would emit first (`execute_optimized_steps_first` relies on it).
                for mut item in buf.drain(..) {
                    if extends {
                        item.extend_path(&mut context.paths, mode)?;
                    } else {
                        item.keep_path(mode);
                    }
                    if Self::exists_in_level(
                        steps_of_level,
                        remaining_steps,
                        &item,
                        context,
                        child,
                        position + 1,
                        level,
                        path_modes,
                        first.as_deref_mut(),
                    )? {
                        found = true;
                        break;
                    }
                }
                // Empty here (the drain removes every item, visited or not), with the capacity it
                // grew to: back to the pool for the next `FlatMap` step of the execution.
                context.give_back_exists_scratch(buf);
                Ok(found)
            }
            GraphStep::As(_, step) => {
                if is_profile {
                    context
                        .profile
                        .enter_step(child, position, || profile_step(current_step, mode));
                }
                let alloc_before = AllocSnapshot::capture(profile_memory);
                let start = Instant::now();
                let mut s = source.clone();
                step.process(context, &mut s).with_step_frame(
                    || step.get_name(),
                    StepAt {
                        child,
                        index: position,
                    },
                    || plan_frame(steps_of_level, level, position),
                )?;
                if is_profile {
                    let duration = start.elapsed();
                    let memory =
                        Self::step_memory_metrics(current_step, &[], alloc_before, profile_memory);
                    context.profile.exit_step(duration, 1, 1, 0, memory);
                }
                Self::exists_in_level(
                    steps_of_level,
                    remaining_steps,
                    &s,
                    context,
                    child,
                    position + 1,
                    level,
                    path_modes,
                    first,
                )
            }
            GraphStep::SideEffect(_, step) => {
                if is_profile {
                    context
                        .profile
                        .enter_step(child, position, || profile_step(current_step, mode));
                }
                let alloc_before = AllocSnapshot::capture(profile_memory);
                let start = Instant::now();
                step.declare_side_effect(&mut context.side_effect);
                step.process(context, source).with_step_frame(
                    || step.get_name(),
                    StepAt {
                        child,
                        index: position,
                    },
                    || plan_frame(steps_of_level, level, position),
                )?;
                step.finish_batch(context).with_step_frame(
                    || step.get_name(),
                    StepAt {
                        child,
                        index: position,
                    },
                    || plan_frame(steps_of_level, level, position),
                )?;
                if is_profile {
                    let duration = start.elapsed();
                    let memory =
                        Self::step_memory_metrics(current_step, &[], alloc_before, profile_memory);
                    context.profile.exit_step(duration, 1, 1, 0, memory);
                }
                Self::exists_in_level(
                    steps_of_level,
                    remaining_steps,
                    source,
                    context,
                    child,
                    position + 1,
                    level,
                    path_modes,
                    first,
                )
            }
            _ => {
                let traversers = Self::execute_optimized_steps(
                    steps,
                    vec![source.clone()],
                    context,
                    LevelAt {
                        child,
                        first: position,
                    },
                    level,
                    suffix_modes,
                )?;
                Ok(Self::take_first(traversers, first))
            }
        }
    }

    /// Whether a fully executed level produced anything; stores its first traverser in `first`
    /// when a caller asked for it.
    fn take_first(traversers: Vec<Traverser<S>>, first: Option<&mut Option<Traverser<S>>>) -> bool {
        let productive = !traversers.is_empty();
        if let Some(first) = first {
            *first = traversers.into_iter().next();
        }
        productive
    }
}

/// Materializes one result for the script DSL, keeping a graph value as a shared handle (the
/// `materialize` function of [`GraphTraversalSource::execute_items`] behind `execute()`).
#[cfg(feature = "script")]
pub(crate) fn list_item<S: GraphStorage>(
    value: TraverserValue<S>,
    storage: &S,
) -> TraverserResult<ListItem> {
    Ok(match value {
        TraverserValue::Graph(graph) => ListItem::Graph(graph),
        value => ListItem::Value(value.into_node_property(storage)?),
    })
}

/// One result of [`GraphTraversalSource::into_list_with_graphs`].
#[cfg(feature = "script")]
#[derive(Clone)]
pub(crate) enum ListItem {
    /// A materialized value.
    Value(ElementProperty),
    /// A graph value, as a shared handle.
    Graph(std::sync::Arc<crate::graph::Graph>),
}
