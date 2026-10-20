//! Executes one TCK case through the Rust GQL API and records what happened (no comparison:
//! that is `classify.rs` with the result seam of `stand_in.rs`).
//!
//! Copied from the TinkerPop harness's `execute.rs` approach and adapted (W42: copy, never
//! share).
//!
//! # Flow
//!
//! 1. A case with a procedure step (`there exists a procedure`) or the upstream `@ignore` tag is
//!    not run ([`RawOutcome::NotRun`]; the scope table of 3b.T5 replaces
//!    [`temporary_not_run`]).
//! 2. On a worker thread, a **fresh empty graph** per case, on the storage selected by
//!    `GRAPHERSAL_TEST_STORAGE` (`storage.rs`): `TraversalGraph`, or `Forwarding` over one.
//!    `Given an empty graph` and `Given any graph` both start from it; `Given the <name> graph`
//!    runs `graphs/<name>/<name>.cypher` through `gql()` (design T7, its trailing `;` dropped).
//! 3. The steps in order: every `having executed` query through `gql()`; parameters through
//!    the notation seam ([`super::stand_in::parameter_value`]); a failure of any setup step is
//!    [`RawOutcome::Setup`] with its reason. Then the state is snapshotted, the query runs with
//!    the parameters (`gql_with`), the state is snapshotted again and the side effects are
//!    their diff (`state.rs`, design T8); every control query runs after it on the same graph.
//!
//! Every query is one traversal on `GraphTraversalSource::new(&mut storage)`: its own unit, so a
//! failing query leaves nothing behind.
//!
//! # Limits (the runner executes thousands of untrusted queries)
//!
//! Every traversal (setup, query, control query) gets [`LIMITS`] and an `evaluationTimeout` of
//! [`EVALUATION_TIMEOUT_MS`], set per execution with `with()`. A limit that fires is
//! [`ErrorRoot::ResourceLimit`] / [`ErrorRoot::Timeout`] (class `timeout`).
//!
//! # Guards
//!
//! - Wall-clock: every case runs on a per-calling-thread worker thread; the caller waits at most
//!   [`WALL_CLOCK_LIMIT`]. A worker that misses the deadline is cancelled through its
//!   [`CancelToken`] (every traversal it runs checks it, so it stops at its next check) and
//!   abandoned (detached, never joined); the case is [`RawOutcome::Timeout`].
//! - Panics: the case runs under `catch_unwind` -> [`RawOutcome::Panic`] with message and
//!   location. The default panic output is silenced only for harness worker threads while they
//!   run a case: a process-wide filtering hook is installed once and delegates every other
//!   panic to the previously installed hook; it is never uninstalled (tests run concurrently).

use std::cell::{Cell, RefCell};
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::Once;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use graphersal::error::{GqlErrorKind, GraphError};
use graphersal::exec::CancelToken;
use graphersal::storage::GraphStorage;
use graphersal::{ElementProperty, GraphTraversalSource, TraversalGraph, TraverserError};

use super::gherkin::{Case, Step};
use super::state::{self, SideEffects};
use super::storage::TestStorage;

/// `evaluationTimeout` of every traversal of a case, in milliseconds.
pub const EVALUATION_TIMEOUT_MS: i64 = 10_000;

/// Wall-clock budget for a whole case (setup, query and control queries).
pub const WALL_CLOCK_LIMIT: Duration = Duration::from_secs(30);

/// Traversal options set on every traversal of a case (tighter than the defaults: the TCK
/// graphs have at most a few dozen elements).
pub const LIMITS: [(&str, i64); 6] = [
    ("traversal.max_traversers", 4_000_000),
    ("traversal.max_materialized_bytes", 64 * 1024 * 1024),
    ("traversal.max_string_bytes", 1024 * 1024),
    ("bulk.max_expansion", 4_000_000),
    ("repeat.max_loops", 1_000),
    // Without a tracking allocator in this binary the engine's estimate is compared.
    ("memory.limit", 512 * 1024 * 1024),
];

/// Maximum length of a recorded message.
const MESSAGE_MAX: usize = 300;

/// When an error arose (design T9, P0 G7): by WHERE it arose, not by its GQLSTATUS class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Before the traversal ran: the `gql:error` marker carries a parse / bind / plan error.
    Compile,
    /// While the traversal ran.
    Runtime,
}

/// What the root cause of a failed query is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorRoot {
    /// A GQL error ([`TraverserError::Gql`]).
    Gql {
        /// Its kind.
        kind: GqlErrorKind,
        /// Its GQLSTATUS code.
        gqlstatus: String,
    },
    /// An element was read or written after it was removed.
    ElementRemoved,
    /// `evaluationTimeout` or a cancellation.
    Timeout,
    /// A resource limit fired, with the limit's name.
    ResourceLimit(String),
    /// Any other error.
    Other,
}

/// A failed query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryError {
    /// When it arose.
    pub phase: Phase,
    /// Its root cause.
    pub root: ErrorRoot,
    /// The first line of its message.
    pub message: String,
}

impl QueryError {
    /// Whether the query was refused as unsupported (`Refused` / `NotYet`, GQLSTATUS 0A000).
    pub fn is_unsupported(&self) -> bool {
        matches!(
            &self.root,
            ErrorRoot::Gql {
                kind: GqlErrorKind::Refused | GqlErrorKind::NotYet,
                ..
            }
        )
    }

    /// Whether a limit of the harness stopped the query.
    pub fn is_limit(&self) -> bool {
        matches!(self.root, ErrorRoot::Timeout | ErrorRoot::ResourceLimit(_))
    }

    /// The GQLSTATUS code of a GQL error.
    pub fn gqlstatus(&self) -> Option<&str> {
        match &self.root {
            ErrorRoot::Gql { gqlstatus, .. } => Some(gqlstatus),
            _ => None,
        }
    }
}

/// What one query did.
#[derive(Debug, Clone, PartialEq)]
pub enum QueryOutcome {
    /// It ran; its rows (each a map column -> value).
    Rows(Vec<ElementProperty>),
    /// It failed.
    Error(QueryError),
}

/// The run of the query under test.
#[derive(Debug, Clone, PartialEq)]
pub struct MainRun {
    /// What the query did.
    pub outcome: QueryOutcome,
    /// The state diff around it.
    pub side_effects: SideEffects,
    /// What each control query did, in order.
    pub controls: Vec<QueryOutcome>,
}

/// What happened when a case ran; no comparison with the expectation yet.
#[derive(Debug, Clone, PartialEq)]
pub enum RawOutcome {
    /// Not run, with the reason (procedures, `@ignore`).
    NotRun(String),
    /// A setup step failed (named graph, `having executed`, a parameter value), with the reason.
    Setup(String),
    /// The query ran (successfully or not).
    Ran(MainRun),
    /// The wall-clock guard fired.
    Timeout,
    /// A panic was caught (a root rule-1 bug), with its message and location.
    Panic(String),
}

/// Why a case is not run, until the scope table of 3b.T5 (`scope.rs`, ids only) replaces this.
pub fn temporary_not_run(case: &Case) -> Option<String> {
    if case.is_ignored() {
        return Some("upstream @ignore".to_string());
    }
    case.steps
        .iter()
        .any(|s| matches!(s.step, Step::Procedure { .. }))
        .then(|| "vendor procedures".to_string())
}

/// Runs one case; never panics and never blocks longer than about [`WALL_CLOCK_LIMIT`].
pub fn run(case: &Case) -> RawOutcome {
    if let Some(reason) = temporary_not_run(case) {
        return RawOutcome::NotRun(reason);
    }
    let job = Job {
        case: case.clone(),
        storage: super::storage::current(),
        cancel: CancelToken::new(),
    };
    dispatch(job).unwrap_or(RawOutcome::Timeout)
}

/// Root of the vendored named graphs.
fn graphs_root() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/opencypher/graphs")
}

/// Everything a worker needs to run one case (all `Send`).
struct Job {
    case: Case,
    storage: TestStorage,
    cancel: CancelToken,
}

// ---------------------------------------------------------------------------------------------
// Worker threads, wall-clock guard, panic capture
// ---------------------------------------------------------------------------------------------

/// A worker thread owned by one calling thread.
struct Worker {
    jobs: Sender<Job>,
    results: Receiver<RawOutcome>,
}

thread_local! {
    /// The calling thread's worker; replaced after a wall-clock timeout.
    static WORKER: RefCell<Option<Worker>> = const { RefCell::new(None) };
    /// True on a worker thread while it runs a case (silences the panic hook).
    static IN_CASE: Cell<bool> = const { Cell::new(false) };
    /// Location of the last panic caught on this worker thread.
    static PANIC_LOCATION: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn spawn_worker() -> Option<Worker> {
    let (job_tx, job_rx) = mpsc::channel::<Job>();
    let (res_tx, res_rx) = mpsc::channel::<RawOutcome>();
    std::thread::Builder::new()
        .name("opencypher-case".to_string())
        .stack_size(64 * 1024 * 1024)
        .spawn(move || {
            while let Ok(job) = job_rx.recv() {
                if res_tx.send(guarded(&job)).is_err() {
                    break;
                }
            }
        })
        .ok()?;
    Some(Worker {
        jobs: job_tx,
        results: res_rx,
    })
}

/// Sends `job` to this thread's worker and waits at most [`WALL_CLOCK_LIMIT`]; `None` on timeout.
fn dispatch(job: Job) -> Option<RawOutcome> {
    install_panic_hook();
    let cancel = job.cancel.clone();
    WORKER.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = spawn_worker();
        }
        let Some(worker) = slot.as_ref() else {
            return Some(RawOutcome::Panic(
                "harness: cannot spawn a worker thread".to_string(),
            ));
        };
        if worker.jobs.send(job).is_err() {
            *slot = None;
            return Some(RawOutcome::Panic(
                "harness: worker thread is gone".to_string(),
            ));
        }
        match worker.results.recv_timeout(WALL_CLOCK_LIMIT) {
            Ok(result) => Some(result),
            Err(RecvTimeoutError::Timeout) => {
                // Stop the hung case at its next check and abandon the worker: dropping both
                // channel ends lets it exit once its case returns.
                cancel.cancel();
                *slot = None;
                None
            }
            Err(RecvTimeoutError::Disconnected) => {
                *slot = None;
                Some(RawOutcome::Panic("harness: worker thread died".to_string()))
            }
        }
    })
}

fn install_panic_hook() {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if IN_CASE.with(Cell::get) {
                let location = info.location().map(|l| l.to_string());
                PANIC_LOCATION.with(|slot| *slot.borrow_mut() = location);
            } else {
                previous(info);
            }
        }));
    });
}

/// Runs the job under `catch_unwind` with the panic output silenced and its cancel token as the
/// thread's current one; a panic becomes [`RawOutcome::Panic`] with its message and location.
fn guarded(job: &Job) -> RawOutcome {
    install_panic_hook();
    IN_CASE.with(|f| f.set(true));
    PANIC_LOCATION.with(|slot| *slot.borrow_mut() = None);
    let result = panic::catch_unwind(AssertUnwindSafe(|| {
        job.cancel.scope(|| execute(&job.case, job.storage))
    }));
    IN_CASE.with(|f| f.set(false));
    match result {
        Ok(outcome) => outcome,
        Err(payload) => {
            let message = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "<non-string panic payload>".to_string());
            let location = PANIC_LOCATION.with(|slot| slot.borrow_mut().take());
            RawOutcome::Panic(match location {
                Some(location) => format!("{message} (at {location})"),
                None => message,
            })
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Execution proper (runs on the worker thread)
// ---------------------------------------------------------------------------------------------

fn execute(case: &Case, storage: TestStorage) -> RawOutcome {
    match storage {
        TestStorage::TraversalGraph => execute_on(TraversalGraph::default(), case),
        TestStorage::Forwarding => execute_on(
            crate::forwarding::Forwarding::new(TraversalGraph::default()),
            case,
        ),
    }
}

fn execute_on<S: GraphStorage>(mut graph: S, case: &Case) -> RawOutcome {
    let mut params: Vec<(String, ElementProperty)> = Vec::new();
    let mut main: Option<MainRun> = None;
    for step in &case.steps {
        match &step.step {
            Step::EmptyGraph | Step::AnyGraph => {}
            Step::NamedGraph(name) => {
                let file = graphs_root().join(name).join(format!("{name}.cypher"));
                let text = match std::fs::read_to_string(&file) {
                    Ok(text) => text,
                    Err(e) => return RawOutcome::Setup(format!("named graph {name}: {e}")),
                };
                let query = text.trim_end().trim_end_matches(';');
                if let QueryOutcome::Error(e) = run_query(&mut graph, query, &[]) {
                    return RawOutcome::Setup(format!("named graph {name}: {}", e.message));
                }
            }
            Step::HavingExecuted(query) => {
                if let QueryOutcome::Error(e) = run_query(&mut graph, query, &[]) {
                    return RawOutcome::Setup(format!("having executed: {}", e.message));
                }
            }
            Step::Parameters(table) => {
                for (name, raw) in table {
                    match super::stand_in::parameter_value(raw) {
                        Ok(value) => params.push((name.clone(), value)),
                        Err(e) => return RawOutcome::Setup(format!("parameter {name}: {e}")),
                    }
                }
            }
            Step::Procedure { .. } => return RawOutcome::NotRun("vendor procedures".to_string()),
            Step::ExecutingQuery(query) => {
                let before = state::snapshot(&graph);
                let outcome = run_query(&mut graph, query, &params);
                let after = state::snapshot(&graph);
                main = Some(MainRun {
                    outcome,
                    side_effects: state::diff(&before, &after),
                    controls: Vec::new(),
                });
            }
            Step::ExecutingControlQuery(query) => {
                let outcome = run_query(&mut graph, query, &params);
                if let Some(main) = main.as_mut() {
                    main.controls.push(outcome);
                }
            }
            Step::Result { .. }
            | Step::ResultEmpty
            | Step::Error { .. }
            | Step::NoSideEffects
            | Step::SideEffects(_) => {}
        }
    }
    match main {
        Some(main) => RawOutcome::Ran(main),
        // The parser's validation guarantees exactly one query; reaching this is a harness bug.
        None => RawOutcome::Panic("harness: the case has no query".to_string()),
    }
}

/// Runs one query as one traversal with the harness limits.
fn run_query<S: GraphStorage>(
    graph: &mut S,
    query: &str,
    params: &[(String, ElementProperty)],
) -> QueryOutcome {
    let mut source = GraphTraversalSource::new(&mut *graph);
    for (key, value) in LIMITS {
        source = source.with(key, value);
    }
    let rows = source
        .with("evaluationTimeout", EVALUATION_TIMEOUT_MS)
        .gql_with(query, params.iter().cloned())
        .to_list();
    match rows {
        Ok(rows) => QueryOutcome::Rows(rows),
        Err(e) => QueryOutcome::Error(describe(&e)),
    }
}

/// The innermost failing step name of a step-failure chain.
fn innermost_step(error: &TraverserError) -> Option<&str> {
    let mut current = error;
    let mut name = None;
    loop {
        match current {
            TraverserError::StepFailed {
                step_name, source, ..
            } => {
                name = Some(step_name.as_str());
                current = source;
            }
            TraverserError::Spelled { source, .. } => current = source,
            _ => return name,
        }
    }
}

/// Classifies a traversal error by phase and root cause.
pub fn describe(error: &TraverserError) -> QueryError {
    let root = match error.root_cause() {
        TraverserError::Gql {
            kind, gqlstatus, ..
        } => ErrorRoot::Gql {
            kind: *kind,
            gqlstatus: gqlstatus.clone(),
        },
        TraverserError::Timeout { .. } | TraverserError::Cancelled { .. } => ErrorRoot::Timeout,
        TraverserError::ResourceLimitExceeded { limit, .. } => {
            ErrorRoot::ResourceLimit(limit.clone())
        }
        TraverserError::Graph(GraphError::ResourceLimitExceeded { limit, .. }) => {
            ErrorRoot::ResourceLimit(limit.clone())
        }
        TraverserError::Graph(GraphError::ElementRemoved { .. }) => ErrorRoot::ElementRemoved,
        _ => ErrorRoot::Other,
    };
    let compile = matches!(root, ErrorRoot::Gql { .. })
        && innermost_step(error).is_some_and(|name| name.starts_with("gql:error("));
    // The first line of the error, plus that of its root cause when the step failure hides it.
    let first = |e: &TraverserError| {
        let text = e.to_string();
        text.lines().next().unwrap_or("").trim().to_string()
    };
    let mut line = first(error);
    let cause = first(error.root_cause());
    if !line.contains(&cause) {
        line = format!("{line}: {cause}");
    }
    let message = if line.chars().count() > MESSAGE_MAX {
        let mut cut: String = line.chars().take(MESSAGE_MAX).collect();
        cut.push('…');
        cut
    } else {
        line.to_string()
    };
    QueryError {
        phase: if compile {
            Phase::Compile
        } else {
            Phase::Runtime
        },
        root,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(text: &str) -> QueryOutcome {
        run_query(&mut TraversalGraph::default(), text, &[])
    }

    fn error(text: &str) -> QueryError {
        match query(text) {
            QueryOutcome::Error(e) => e,
            QueryOutcome::Rows(rows) => panic!("{text}: no error, rows {rows:?}"),
        }
    }

    #[test]
    fn parse_and_bind_errors_are_compile_time_and_row_errors_runtime() {
        let syntax = error("RETURN 1 +* 2");
        assert_eq!(syntax.phase, Phase::Compile);
        assert_eq!(syntax.gqlstatus(), Some("42001"));
        let unbound = error("RETURN x");
        assert_eq!(unbound.phase, Phase::Compile);
        let data = error("UNWIND [1, 0] AS x RETURN 1 / x");
        assert_eq!(data.phase, Phase::Runtime);
        assert_eq!(data.gqlstatus(), Some("22012"));
        let not_yet = error("MATCH (n) SET n.x = 1");
        assert!(not_yet.is_unsupported(), "{not_yet:?}");
        assert_eq!(not_yet.phase, Phase::Compile);
    }

    #[test]
    fn a_missing_parameter_is_a_compile_time_error() {
        let e = error("RETURN $p AS p");
        assert_eq!(e.phase, Phase::Compile, "{e:?}");
    }

    #[test]
    fn the_harness_limits_stop_a_runaway_query() {
        let e = error("UNWIND range(1, 3000) AS a UNWIND range(1, 3000) AS b RETURN count(*)");
        assert!(e.is_limit(), "{e:?}");
    }
}
