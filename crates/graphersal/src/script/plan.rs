//! The script group of [`AnyGraph`](crate::graph::AnyGraph): what a script terminal and a
//! whole-script unit hand to the graph (`ScriptPlan`, `Terminal`, `UnitKind`, `UnitBody`) and
//! what they get back (`TerminalOutput`, `UnitResult`), and the generic code the blanket
//! `impl AnyGraph for Graph<S>` runs for them (`run_plan_on`, `run_unit_on`).
//!
//! The types are `pub` because they appear in the sealed trait's (hidden) methods; this module
//! is private, so no other crate can name or build them. The engine is monomorphised for `&S`
//! and `&mut S` inside `run_plan_on`: one indirect call per terminal, never one per element
//! (`book/src/for_developers/custom_storage.md`).

use super::*;
use crate::error::{BoxedTraverserError, GraphError, TraverserError, TraverserResult};
use crate::graph::{AnyGraph, ChangeSet};
use crate::traversal::source::{ExecutedItems, ListItem};

/// A script plan as a terminal hands it to [`AnyGraph::run_plan`]: the script source (its steps,
/// options, access and whether its graph is private) and whether it writes (the terminal then
/// takes the write lock instead of the read lock).
#[doc(hidden)]
pub struct ScriptPlan<'a> {
    pub(crate) source: &'a ScriptGraphTraversalSource,
    pub(crate) mutating: bool,
}

/// The terminal a script runs a plan with ([`AnyGraph::run_plan`]).
#[doc(hidden)]
pub struct Terminal(pub(crate) TerminalKind);

/// The terminal kinds: one per engine terminal the script layer calls.
pub(crate) enum TerminalKind {
    /// `to_list()` with the values materialized ([`ScriptGraphTraversalSource::to_typed_list`]).
    ToList,
    /// `to_list()` keeping graph values as shared handles (`toList()`, `next()`).
    ListWithGraphs,
    /// `profile()` / `profile(flags)`.
    Profile(ProfileType),
    /// `execute()` / `execute(#{..})`.
    Execute(crate::ExecOptions),
    /// A visualizer (`to_table()`, `visualize(..)`, a rendered final value).
    Visualize {
        format: VisualizeFormat,
        options: RenderOptions,
        may_stop_early: bool,
    },
    /// `iterate()`.
    Iterate,
    /// `to_graph()`.
    ToGraph,
}

/// The storage-independent result of a [`Terminal`].
#[doc(hidden)]
pub struct TerminalOutput(pub(crate) OutputKind);

/// The results, one per [`TerminalKind`].
pub(crate) enum OutputKind {
    List(TraverserResult<Vec<ElementProperty>>),
    Items(TraverserResult<Vec<ListItem>>),
    Metrics(TraverserResult<crate::TraversalMetrics>),
    Executed(ExecutedItems<ListItem>),
    Rendered(TraverserResult<RenderedOutput>),
    Unit(TraverserResult<()>),
    Graph(TraverserResult<Arc<Graph>>),
}

/// The error of an output of another kind than the terminal asked for: never produced, because
/// `run_plan_on` answers each terminal with its own kind; a terminal reports it instead of
/// panicking (rule 1).
fn mismatch() -> BoxedTraverserError {
    Box::new(TraverserError::NotImplemented {
        feature: "a script terminal answered with another kind of result",
    })
}

impl TerminalOutput {
    pub(crate) fn into_list(self) -> TraverserResult<Vec<ElementProperty>> {
        match self.0 {
            OutputKind::List(result) => result,
            _ => Err(mismatch()),
        }
    }

    pub(crate) fn into_items(self) -> TraverserResult<Vec<ListItem>> {
        match self.0 {
            OutputKind::Items(result) => result,
            _ => Err(mismatch()),
        }
    }

    pub(crate) fn into_metrics(self) -> TraverserResult<crate::TraversalMetrics> {
        match self.0 {
            OutputKind::Metrics(result) => result,
            _ => Err(mismatch()),
        }
    }

    pub(crate) fn into_executed(self) -> ExecutedItems<ListItem> {
        match self.0 {
            OutputKind::Executed(run) => run,
            _ => ExecutedItems {
                items: Vec::new(),
                error: Some(mismatch()),
                profile: None,
                mutated: false,
                changes: Default::default(),
            },
        }
    }

    pub(crate) fn into_rendered(self) -> TraverserResult<RenderedOutput> {
        match self.0 {
            OutputKind::Rendered(result) => result,
            _ => Err(mismatch()),
        }
    }

    pub(crate) fn into_unit(self) -> TraverserResult<()> {
        match self.0 {
            OutputKind::Unit(result) => result,
            _ => Err(mismatch()),
        }
    }

    pub(crate) fn into_graph(self) -> TraverserResult<Arc<Graph>> {
        match self.0 {
            OutputKind::Graph(result) => result,
            _ => Err(mismatch()),
        }
    }
}

/// Runs `plan` with `terminal` on `graph`: lowers the plan for `&mut S` under the write lock
/// (a mutating plan) or for `&S` under the read lock, and runs the terminal. The body of
/// [`AnyGraph::run_plan`] for every `Graph<S>`.
pub(crate) fn run_plan_on<S: GraphStorage>(
    graph: &Graph<S>,
    plan: &ScriptPlan<'_>,
    terminal: Terminal,
) -> TerminalOutput {
    // Inside a unit the thread holds, the traversal reads the unit's pinned time: the storage's
    // unit opens only at the unit's first write (`Graph::begin_unit`).
    let started_at = graph.held_unit_started_at();
    // A view of the graph this thread may not have (a commit hook or an embedder querying
    // the graph its unit is changing) is the terminal's error, never a panic.
    if plan.mutating {
        let mut storage = match graph.write_checked() {
            Ok(storage) => storage,
            Err(err) => return terminal.failed(err),
        };
        let gts = pin_unit_clock(plan.source.ast_convert_to_gts(&mut *storage), started_at);
        run_terminal(gts, terminal)
    } else {
        let storage = match graph.read_checked() {
            Ok(storage) => storage,
            Err(err) => return terminal.failed(err),
        };
        let gts = pin_unit_clock(plan.source.ast_convert_to_gts(&*storage), started_at);
        run_terminal(gts, terminal)
    }
}

/// Pins `gts` to the held unit's time `started_at` (`with_now`) unless it is pinned already:
/// the time `GraphStorage::unit_started_at` reports once the storage's unit is open, so the
/// unit's traversals read one clock value before and after its first write.
fn pin_unit_clock<'g, T: GraphStorage + 'g>(
    gts: GraphTraversalSource<'g, T>,
    started_at: Option<i128>,
) -> GraphTraversalSource<'g, T> {
    match started_at {
        Some(nanos) if gts.create_time.is_none() => {
            gts.with_now(crate::graph::schema::defaults::datetime_of_nanos(nanos))
        }
        _ => gts,
    }
}

impl Terminal {
    /// The output of this terminal's kind failing with `err` before it ran.
    fn failed(self, err: GraphError) -> TerminalOutput {
        let err: BoxedTraverserError = Box::new(TraverserError::from(err));
        TerminalOutput(match self.0 {
            TerminalKind::ToList => OutputKind::List(Err(err)),
            TerminalKind::ListWithGraphs => OutputKind::Items(Err(err)),
            TerminalKind::Profile(_) => OutputKind::Metrics(Err(err)),
            TerminalKind::Execute(_) => OutputKind::Executed(ExecutedItems {
                items: Vec::new(),
                error: Some(err),
                profile: None,
                mutated: false,
                changes: Default::default(),
            }),
            TerminalKind::Visualize { .. } => OutputKind::Rendered(Err(err)),
            TerminalKind::Iterate => OutputKind::Unit(Err(err)),
            TerminalKind::ToGraph => OutputKind::Graph(Err(err)),
        })
    }
}

/// Runs `terminal` on the lowered traversal `gts`.
fn run_terminal<'g, T: GraphStorage + 'g>(
    gts: GraphTraversalSource<'g, T>,
    terminal: Terminal,
) -> TerminalOutput {
    TerminalOutput(match terminal.0 {
        TerminalKind::ToList => OutputKind::List(gts.to_list()),
        TerminalKind::ListWithGraphs => OutputKind::Items(gts.into_list_with_graphs()),
        TerminalKind::Profile(flags) => OutputKind::Metrics(gts.profile_with(flags)),
        TerminalKind::Execute(options) => {
            OutputKind::Executed(gts.execute_items(&options, crate::traversal::source::list_item))
        }
        TerminalKind::Visualize {
            format,
            options,
            may_stop_early,
        } => OutputKind::Rendered(gts.visualize_bounded(format, &options, may_stop_early)),
        TerminalKind::Iterate => OutputKind::Unit(gts.iterate()),
        TerminalKind::ToGraph => OutputKind::Graph(gts.to_graph()),
    })
}

/// How [`AnyGraph::run_unit`] closes the whole-script unit.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq)]
pub enum UnitKind {
    /// Commit when the body succeeds, roll back when it fails (`eval_value_atomic`).
    Atomic,
    /// Always roll back, without hooks, and report what the body changed
    /// (`eval_value_dry_run`).
    DryRun,
    /// [`Self::Atomic`] for the call of a saved query that writes: the unit (a savepoint inside
    /// an open one) carries this metadata to its commit (the query's name and arguments).
    Call(crate::graph::CommitMetadata),
}

/// What the body of a whole-script unit returns: the script's value or its error.
#[doc(hidden)]
pub struct UnitBody(pub(crate) Result<Dynamic, Box<rhai::EvalAltResult>>);

/// What [`AnyGraph::run_unit`] returns: the body's value (with the change set of a dry run) or
/// the error that closed the unit.
#[doc(hidden)]
pub struct UnitResult(pub(crate) Result<(Dynamic, Option<ChangeSet>), Box<rhai::EvalAltResult>>);

/// The body of [`AnyGraph::run_unit`] for every `Graph<S>`: the whole-script unit of
/// `eval_value_atomic` / `eval_value_dry_run` / `eval_job_unit` and of a writing saved-query
/// call.
///
/// The unit is held by the current thread across the script's locks ([`Graph::begin_unit`]):
/// the body's terminals lock the same shared graph and get views of the held lock, so the
/// script runs inside the unit. Until its first write the unit holds the lock upgradable and
/// other threads keep reading; from its first write on, the lock is exclusive until the unit
/// closes, so nobody sees its uncommitted changes. Inside a unit the thread already holds (a
/// saved query called by a whole-script unit) it is a savepoint. A panic in the body closes the
/// unit (rollback, or discard for a dry run) before it continues, which also releases the lock.
pub(crate) fn run_unit_on<S: GraphStorage + Default + Send + Sync + 'static>(
    graph: &Graph<S>,
    kind: UnitKind,
    body: &mut dyn FnMut() -> UnitBody,
) -> UnitResult {
    let as_script_error = |err: GraphError| script_error(Box::new(TraverserError::from(err)));
    // A view of the graph this thread may not have (a commit hook of this graph running a
    // whole-script unit on it) is the script's error, never a panic.
    let mark = match graph.begin_unit() {
        Ok(mark) => mark,
        Err(err) => return UnitResult(Err(as_script_error(err))),
    };
    let dry_run = kind == UnitKind::DryRun;
    // A panic (an engine bug) closes the unit before it continues to the host. The close
    // cannot fail here: this thread holds the unit it just opened.
    let evaluated = crate::graph::storage::close_on_unwind(
        &mut (),
        |_| body().0,
        |_| {
            let _ = match dry_run {
                true => graph.discard_unit(mark).map(|_| ()),
                false => graph.rollback_unit(
                    mark,
                    &crate::graph::RollbackReason::failed(&crate::graph::storage::UNWOUND),
                ),
            };
        },
    );
    UnitResult(match (kind, evaluated) {
        (UnitKind::DryRun, Ok(value)) => graph
            .discard_unit(mark)
            .map(|changes| (value, Some(changes)))
            .map_err(as_script_error),
        (UnitKind::DryRun, Err(err)) => {
            // The unit is closed the same way and its change set dropped; on a storage
            // without change capture its `Unsupported` is dropped too: the script's own error
            // is the one to report (as `Transactional::dry_run` does).
            let _ = graph.discard_unit(mark);
            Err(err)
        }
        (UnitKind::Atomic, Ok(value)) => graph
            .commit_unit(mark)
            .map(|()| (value, None))
            .map_err(as_script_error),
        (UnitKind::Call(metadata), Ok(value)) => graph
            .commit_unit_with(mark, metadata)
            .map(|()| (value, None))
            .map_err(as_script_error),
        (UnitKind::Atomic | UnitKind::Call(_), Err(err)) => {
            // Cannot fail: this thread holds the unit (see above).
            let _ = graph.rollback_unit(mark, &crate::graph::RollbackReason::failed(&err));
            Err(err)
        }
    })
}

/// The graph a source without one (`g: None`, such as the `mergeV()` identity child) runs on:
/// a process-wide empty scratch graph for a plan that only reads, a fresh one for a plan that
/// writes (so a write never reaches the next run).
pub(crate) fn scratch_graph(mutating: bool) -> Arc<dyn AnyGraph> {
    static EMPTY: std::sync::OnceLock<Arc<Graph>> = std::sync::OnceLock::new();
    if mutating {
        Arc::new(Graph::new(TraversalGraph::new()))
    } else {
        EMPTY
            .get_or_init(|| Arc::new(Graph::new(TraversalGraph::new())))
            .clone()
    }
}

/// Whether `a` and `b` are the same shared graph: their `Arc` DATA pointers are equal (a fat
/// `Arc<dyn AnyGraph>` pointer also carries a vtable pointer, which may differ for one graph).
pub(crate) fn same_graph<A: ?Sized, B: ?Sized>(a: &Arc<A>, b: &Arc<B>) -> bool {
    std::ptr::eq(Arc::as_ptr(a) as *const (), Arc::as_ptr(b) as *const ())
}
