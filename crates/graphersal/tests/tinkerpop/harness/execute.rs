//! Executes one TinkerPop scenario through the Rhai DSL and captures a typed raw outcome.
//!
//! No expectation is compared here (that is `compare.rs`); [`run`] only records what the engine
//! did: values, a script failure, a timeout or a panic.
//!
//! # Flow
//!
//! 1. `@GraphComputerOnly` → [`RawOutcome::Skipped`]; no other tag skips. A dataset that cannot be
//!    loaded (an unsupported test, or a `crew` scenario missing from the incompatibilities) is
//!    skipped with the dataset's reason.
//! 2. The traversal, the initializer and every `GraphCount` traversal are translated
//!    (`translate.rs`); parameter and side-effect values are parsed (`notation.rs`). A main
//!    traversal, initializer or notation that cannot be expressed → [`RawOutcome::Untranslatable`];
//!    an untranslatable `GraphCount` traversal only records an `Err` count.
//! 3. The scenario runs on a **fresh copy** of its dataset when it has an initializer, uses the
//!    empty graph, or any of its scripts contains a mutating step name ([`MUTATING_STEPS`]);
//!    otherwise on the shared template. After a shared-template run the template's vertex/edge
//!    counts are compared with the pre-run numbers; a change is recorded in
//!    [`ScenarioRun::harness_error`] and the template is reloaded.
//! 4. On a worker thread: initializer (on the *setup* engine, whose expression-depth limit is
//!    lifted, see below; its value is iterated and ignored), then the parameters and side effects
//!    are bound, then the traversal runs on the *user* engine (the DSL engine exactly as users
//!    get it), then the `GraphCount` traversals run on the same graph and scope.
//!
//! # Scope
//!
//! `graph_scope_with_options` with `evaluationTimeout` = [`EVALUATION_TIMEOUT_MS`] preset, plus
//! the constants `NaN` and `Infinity` (f64), each parameter under its Rhai name (the translator's
//! rename map) and each side effect as `se_<name>`.
//!
//! Parameter values: scalars natively; `v[x].id` / `.sid` and `e[..].id` / `.sid` → the id
//! string, looked up on the scenario's graph (vertices by `name`, edges by out-name, label and
//! in-name); `v[x]` / `e[..]` → the value of `g.V(<id>).next()` / `g.E(<id>).next()`, i.e. the
//! very `Dynamic` a user would get; lists and sets → arrays (Rhai has no set); maps → object maps
//! with string keys, where the token keys `t[id]`/`t[label]`/`D[OUT]`/`D[IN]` (`D[from]`/`D[to]`)
//! become the DSL's reserved string keys `"T.id"`/`"T.label"`/`"Direction.OUT"`/`"Direction.IN"`
//! (an `M[..]` or other non-string key is untranslatable); tokens as values (`t[id]`, `D[OUT]`,
//! `M[outV]`) → the value of their DSL spelling (`T.id`, `Direction.OUT`, `Merge.outV`), so a
//! token the DSL lacks fails in the engine as `Variable not found`;
//! `uuid[..]` → `UUID.fromString(..)`. Byte, short and BigInteger values collapse to int64 and
//! BigDecimal to float64 (a documented test-surface deviation, see the README); a value outside
//! those ranges, datetime and `vp[..]` / `p[..]` values are untranslatable.
//!
//! # Guards
//!
//! - The engine's own `evaluationTimeout` error → [`RawOutcome::Timeout`].
//! - Wall-clock: every scenario runs on a per-calling-thread worker thread; the caller waits at
//!   most [`WALL_CLOCK_LIMIT`]. A worker that misses the deadline is abandoned (detached, never
//!   joined) and replaced, and the scenario is a [`RawOutcome::Timeout`] — a hung scenario never
//!   hangs the suite.
//! - Panics: the scenario runs under `catch_unwind` → [`RawOutcome::Panic`] with the message and
//!   location. The default panic output is silenced only for harness worker threads while they
//!   run a scenario: a process-wide filtering hook is installed once and delegates every other
//!   panic (other tests, other threads) to the previously installed hook. It is never uninstalled,
//!   because tests run concurrently and a take/restore pair would race with them.

use std::cell::{Cell, RefCell};
use std::panic::{self, AssertUnwindSafe};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Once};
use std::time::Duration;

use graphersal::script::rhai::{self, Dynamic, Engine, EvalAltResult, Scope};
use graphersal::script::{ScriptGraphTraversalSource, ScriptMap, ScriptUuid};
use graphersal::storage::AnyGraph;
use graphersal::{ElementProperty, prelude::Number};

use super::dataset::{self, SkipReason};
use super::gherkin::{Assertion, GraphSetup, Iteration, Scenario};
use super::notation::{self, Dir, Expected, FloatWidth, IntWidth, MergeToken, TToken};
use super::translate::{self, Translation};

/// `evaluationTimeout` preset on every traversal source of a scenario's scope, in milliseconds.
pub const EVALUATION_TIMEOUT_MS: i64 = 10_000;

/// Wall-clock budget for a whole scenario (initializer, traversal and graph counts).
pub const WALL_CLOCK_LIMIT: Duration = Duration::from_secs(30);

/// Step names (as written in a translated script, both spellings) that mutate the graph.
pub const MUTATING_STEPS: &[&str] = &[
    "addV",
    "add_v",
    "addE",
    "add_e",
    "property",
    "property_json",
    "drop",
    "mergeV",
    "merge_v",
    "mergeE",
    "merge_e",
];

/// What happened when a scenario ran; no comparison with the expectation yet.
#[derive(Debug, Clone, PartialEq)]
pub enum RawOutcome {
    /// Not run (tag or dataset), with the reason.
    Skipped(String),
    /// A text or notation value cannot be expressed for Graphersal, with the reason.
    Untranslatable(String),
    /// The traversal's values (`Next` keeps at most the first one).
    Values(Vec<ElementProperty>),
    /// The script (or the initializer) failed.
    ScriptError(ScriptFailure),
    /// The engine's `evaluationTimeout` or the harness's wall-clock guard fired.
    Timeout,
    /// A panic was caught (a CLAUDE.md rule-1 bug), with its message and location.
    Panic(String),
}

impl RawOutcome {
    /// Short, stable name of the outcome kind (for histograms).
    pub fn kind(&self) -> &'static str {
        match self {
            RawOutcome::Skipped(_) => "skipped",
            RawOutcome::Untranslatable(_) => "untranslatable",
            RawOutcome::Values(_) => "values",
            RawOutcome::ScriptError(ScriptFailure::Missing { .. }) => "error:missing",
            RawOutcome::ScriptError(ScriptFailure::Runtime(_)) => "error:runtime",
            RawOutcome::ScriptError(ScriptFailure::Other(_)) => "error:other",
            RawOutcome::Timeout => "timeout",
            RawOutcome::Panic(_) => "panic",
        }
    }
}

/// A classified script failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptFailure {
    /// A missing function/overload (`Function not found`), variable/token (`Variable not found`)
    /// or property getter (`Unknown property`).
    Missing {
        /// The missing name: a function name, a variable name, or `Type.property`.
        name: String,
        /// The full signature or message as reported by Rhai.
        signature: String,
    },
    /// A runtime error raised by the engine (a `TraverserError` diagnostic or another runtime
    /// error value), as text.
    Runtime(String),
    /// Any other script error (parse errors, expression depth, type mismatches, binding
    /// failures), as text.
    Other(String),
}

/// Everything recorded about one scenario run.
#[derive(Debug, Clone, PartialEq)]
pub struct ScenarioRun {
    /// The outcome of the main traversal.
    pub outcome: RawOutcome,
    /// The translated main traversal, when translation succeeded.
    pub script: Option<String>,
    /// For each `GraphCount` assertion: its (untranslated) traversal and the count obtained after
    /// the main traversal ran, or why none was obtained. Empty when the main traversal did not
    /// run to completion.
    pub graph_counts: Vec<(String, Result<i64, String>)>,
    /// The script evaluated to a native Rhai value instead of a traversal, which was converted
    /// with the engine's conversion rules.
    pub native_result: bool,
    /// The scenario ran on a fresh copy of its dataset (otherwise on the shared template).
    pub fresh_graph: bool,
    /// The failure in [`RawOutcome::ScriptError`] happened in the graph initializer.
    pub initializer_failed: bool,
    /// A harness-level problem, e.g. an undetected mutation of the shared template.
    pub harness_error: Option<String>,
    /// The graph the scenario ran on, in its state after the run (expectations such as `v[name]`
    /// are resolved against it). `None` when the scenario did not get to run on a graph.
    pub graph: Option<RunGraph>,
}

/// The graph a scenario ran on; compares by identity and prints opaquely (the graph itself has
/// neither `PartialEq` nor a useful `Debug`).
#[derive(Clone)]
pub struct RunGraph(pub Arc<dyn AnyGraph>);

impl PartialEq for RunGraph {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl std::fmt::Debug for RunGraph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RunGraph(..)")
    }
}

impl ScenarioRun {
    fn new(outcome: RawOutcome) -> Self {
        Self {
            outcome,
            script: None,
            graph_counts: Vec::new(),
            native_result: false,
            fresh_graph: false,
            initializer_failed: false,
            harness_error: None,
            graph: None,
        }
    }
}

/// Runs one scenario; never panics and never blocks longer than about [`WALL_CLOCK_LIMIT`].
pub fn run(scenario: &Scenario) -> ScenarioRun {
    if scenario.tags.iter().any(|t| t == "@GraphComputerOnly") {
        return ScenarioRun::new(RawOutcome::Skipped("@GraphComputerOnly".to_string()));
    }
    if let Err(SkipReason(reason)) = dataset_available(scenario.graph) {
        return ScenarioRun::new(RawOutcome::Skipped(reason));
    }
    let Some(traversal) = &scenario.traversal else {
        return ScenarioRun::new(RawOutcome::Skipped("scenario has no traversal".to_string()));
    };

    let job = match prepare(scenario, traversal) {
        Ok(job) => job,
        Err(run) => return *run,
    };

    let fresh = job.fresh;
    let script = job.script.clone();
    let graph = match if fresh {
        dataset::fresh(scenario.graph)
    } else {
        dataset::load(scenario.graph)
    } {
        Ok(graph) => graph,
        Err(SkipReason(reason)) => return ScenarioRun::new(RawOutcome::Skipped(reason)),
    };
    let before = (!fresh).then(|| dataset::counts(&graph));

    let result = dispatch(Job {
        graph: graph.clone(),
        ..job
    });

    let mut run = ScenarioRun::new(RawOutcome::Timeout);
    run.script = Some(script);
    run.fresh_graph = fresh;
    run.graph = Some(RunGraph(graph.clone()));
    if let Some(result) = result {
        run.outcome = result.outcome;
        run.graph_counts = result.graph_counts;
        run.native_result = result.native_result;
        run.initializer_failed = result.initializer_failed;
        if let Some(before) = before {
            let after = dataset::counts(&graph);
            if after != before {
                run.harness_error = Some(format!(
                    "shared {:?} template changed from {before:?} to {after:?} (vertices, edges): \
                     undetected mutation; template reloaded",
                    scenario.graph
                ));
                dataset::reload_template(scenario.graph);
            }
        }
    }
    run
}

/// Checks that the dataset can be provided without loading a copy of it.
fn dataset_available(setup: GraphSetup) -> Result<(), SkipReason> {
    match setup {
        GraphSetup::Modern | GraphSetup::Grateful | GraphSetup::Crew | GraphSetup::Sink => {
            dataset::load(setup).map(|_| ())
        }
        GraphSetup::Empty => Ok(()),
        GraphSetup::Unsupported => Err(SkipReason(dataset::UNSUPPORTED.to_string())),
    }
}

/// Everything a worker needs to run one scenario (all `Send`).
struct Job {
    graph: Arc<dyn AnyGraph>,
    fresh: bool,
    initializer: Option<String>,
    params: Vec<(String, Expected)>,
    side_effects: Vec<(String, Expected)>,
    script: String,
    iteration: Iteration,
    /// `(gremlin text, translated script or the reason it is untranslatable)`.
    graph_counts: Vec<(String, Result<String, String>)>,
}

/// What a worker reports back.
struct JobResult {
    outcome: RawOutcome,
    graph_counts: Vec<(String, Result<i64, String>)>,
    native_result: bool,
    initializer_failed: bool,
}

/// Translates and parses everything up front; an error is the finished run.
fn prepare(scenario: &Scenario, traversal: &str) -> Result<Job, Box<ScenarioRun>> {
    let untranslatable =
        |reason: String| Box::new(ScenarioRun::new(RawOutcome::Untranslatable(reason)));
    let param_names: Vec<String> = scenario.params.iter().map(|(n, _)| n.clone()).collect();
    let side_effect_names: Vec<String> = scenario
        .side_effects
        .iter()
        .map(|(n, _)| n.clone())
        .collect();

    let (script, renames) = match translate::translate(traversal, &param_names, &side_effect_names)
    {
        Translation::Ok {
            script,
            param_names,
        } => (script, param_names),
        Translation::Unsupported { reason } => return Err(untranslatable(reason)),
    };

    let initializer = match &scenario.initializer {
        None => None,
        Some(text) => match translate::translate(text, &param_names, &[]) {
            Translation::Ok { script, .. } => Some(script),
            Translation::Unsupported { reason } => {
                let mut run = untranslatable(format!("initializer: {reason}"));
                run.script = Some(script);
                return Err(run);
            }
        },
    };

    let graph_counts: Vec<(String, Result<String, String>)> = scenario
        .assertions
        .iter()
        .filter_map(|a| match a {
            Assertion::GraphCount { traversal, .. } => Some(traversal),
            _ => None,
        })
        .map(|text| {
            let translated = match translate::translate(text, &param_names, &[]) {
                Translation::Ok { script, .. } => Ok(script),
                Translation::Unsupported { reason } => Err(format!("untranslatable: {reason}")),
            };
            (text.clone(), translated)
        })
        .collect();

    let mut params = Vec::with_capacity(scenario.params.len());
    for (name, value) in &scenario.params {
        let rhai_name = renames
            .iter()
            .find(|(gremlin, _)| gremlin == name)
            .map_or_else(|| name.clone(), |(_, rhai)| rhai.clone());
        match notation::parse(value) {
            Ok(expected) => params.push((rhai_name, expected)),
            Err(e) => {
                let mut run = untranslatable(format!("parameter {name}: {e}"));
                run.script = Some(script);
                return Err(run);
            }
        }
    }
    let mut side_effects = Vec::with_capacity(scenario.side_effects.len());
    for (name, value) in &scenario.side_effects {
        match notation::parse(value) {
            Ok(expected) => side_effects.push((format!("se_{name}"), expected)),
            Err(e) => {
                let mut run = untranslatable(format!("side effect {name}: {e}"));
                run.script = Some(script);
                return Err(run);
            }
        }
    }

    let fresh = scenario.initializer.is_some()
        || scenario.graph == GraphSetup::Empty
        || contains_mutating_step(&script)
        || graph_counts
            .iter()
            .any(|(_, s)| s.as_ref().is_ok_and(|s| contains_mutating_step(s)));

    Ok(Job {
        graph: super::storage::wrap(Default::default()),
        fresh,
        initializer,
        params,
        side_effects,
        script,
        iteration: scenario.iteration.unwrap_or(Iteration::ToList),
        graph_counts,
    })
}

/// Whether `script` calls one of [`MUTATING_STEPS`] (an identifier directly followed by `(`,
/// outside string literals).
pub fn contains_mutating_step(script: &str) -> bool {
    let bytes = script.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'"' || c == b'\'' {
            i += 1;
            while i < bytes.len() && bytes[i] != c {
                i += if bytes[i] == b'\\' { 2 } else { 1 };
            }
            i += 1;
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let ident = &script[start..i];
            let rest = script[i..].trim_start();
            if rest.starts_with('(') && MUTATING_STEPS.contains(&ident) {
                return true;
            }
        } else {
            i += 1;
        }
    }
    false
}

// ---------------------------------------------------------------------------------------------
// Worker threads, wall-clock guard, panic capture
// ---------------------------------------------------------------------------------------------

/// A worker thread owned by one calling thread.
struct Worker {
    jobs: Sender<Job>,
    results: Receiver<JobResult>,
}

thread_local! {
    /// The calling thread's worker; replaced after a wall-clock timeout.
    static WORKER: RefCell<Option<Worker>> = const { RefCell::new(None) };
    /// True on a worker thread while it runs a scenario (silences the panic hook).
    static IN_SCENARIO: Cell<bool> = const { Cell::new(false) };
    /// Location of the last panic caught on this worker thread.
    static PANIC_LOCATION: RefCell<Option<String>> = const { RefCell::new(None) };
    /// `(user engine, setup engine)` of this worker thread, built once.
    static ENGINES: (Engine, Engine) = {
        let user = graphersal::script::engine_with_limits(&graphersal::script::ScriptLimits::default(), std::sync::Arc::new(graphersal::auth::AllowAll));
        let mut setup = graphersal::script::engine_with_limits(&graphersal::script::ScriptLimits::default(), std::sync::Arc::new(graphersal::auth::AllowAll));
        setup.set_max_expr_depths(0, 0);
        (user, setup)
    };
}

fn spawn_worker() -> Option<Worker> {
    let (job_tx, job_rx) = mpsc::channel::<Job>();
    let (res_tx, res_rx) = mpsc::channel::<JobResult>();
    std::thread::Builder::new()
        .name("tinkerpop-scenario".to_string())
        .stack_size(64 * 1024 * 1024)
        .spawn(move || {
            while let Ok(job) = job_rx.recv() {
                if res_tx.send(execute_guarded(job)).is_err() {
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
fn dispatch(job: Job) -> Option<JobResult> {
    install_panic_hook();
    WORKER.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = spawn_worker();
        }
        let Some(worker) = slot.as_ref() else {
            return Some(JobResult::failure(ScriptFailure::Other(
                "harness: cannot spawn a worker thread".to_string(),
            )));
        };
        if worker.jobs.send(job).is_err() {
            *slot = None;
            return Some(JobResult::failure(ScriptFailure::Other(
                "harness: worker thread is gone".to_string(),
            )));
        }
        match worker.results.recv_timeout(WALL_CLOCK_LIMIT) {
            Ok(result) => Some(result),
            Err(RecvTimeoutError::Timeout) => {
                // Abandon the hung worker: dropping both channel ends lets it exit once (if ever)
                // its scenario returns.
                *slot = None;
                None
            }
            Err(RecvTimeoutError::Disconnected) => {
                *slot = None;
                Some(JobResult::failure(ScriptFailure::Other(
                    "harness: worker thread died".to_string(),
                )))
            }
        }
    })
}

fn install_panic_hook() {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if IN_SCENARIO.with(Cell::get) {
                let location = info.location().map(|l| l.to_string());
                PANIC_LOCATION.with(|slot| *slot.borrow_mut() = location);
            } else {
                previous(info);
            }
        }));
    });
}

fn execute_guarded(job: Job) -> JobResult {
    guarded(|| execute(job))
}

/// Runs `f` under `catch_unwind` with the panic output silenced; a panic becomes
/// [`RawOutcome::Panic`] with its message and location.
fn guarded(f: impl FnOnce() -> JobResult) -> JobResult {
    install_panic_hook();
    IN_SCENARIO.with(|f| f.set(true));
    PANIC_LOCATION.with(|slot| *slot.borrow_mut() = None);
    let result = panic::catch_unwind(AssertUnwindSafe(f));
    IN_SCENARIO.with(|f| f.set(false));
    match result {
        Ok(result) => result,
        Err(payload) => {
            let message = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "<non-string panic payload>".to_string());
            let location = PANIC_LOCATION.with(|slot| slot.borrow_mut().take());
            let message = match location {
                Some(location) => format!("{message} (at {location})"),
                None => message,
            };
            JobResult {
                outcome: RawOutcome::Panic(message),
                graph_counts: Vec::new(),
                native_result: false,
                initializer_failed: false,
            }
        }
    }
}

impl JobResult {
    fn failure(failure: ScriptFailure) -> Self {
        Self {
            outcome: RawOutcome::ScriptError(failure),
            graph_counts: Vec::new(),
            native_result: false,
            initializer_failed: false,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Execution proper (runs on the worker thread)
// ---------------------------------------------------------------------------------------------

/// The typed values of an evaluated script, and whether it was a native value.
struct Evaluated {
    values: Vec<ElementProperty>,
    native: bool,
}

enum EvalError {
    Timeout,
    Failure(ScriptFailure),
}

impl EvalError {
    fn into_outcome(self) -> RawOutcome {
        match self {
            EvalError::Timeout => RawOutcome::Timeout,
            EvalError::Failure(f) => RawOutcome::ScriptError(f),
        }
    }

    fn describe(self) -> String {
        match self {
            EvalError::Timeout => "timeout".to_string(),
            EvalError::Failure(ScriptFailure::Missing { signature, .. }) => {
                format!("missing: {signature}")
            }
            EvalError::Failure(ScriptFailure::Runtime(m) | ScriptFailure::Other(m)) => m,
        }
    }
}

fn execute(job: Job) -> JobResult {
    ENGINES.with(|(user, setup)| execute_with(job, user, setup))
}

fn execute_with(job: Job, user: &Engine, setup: &Engine) -> JobResult {
    let mut result = JobResult {
        outcome: RawOutcome::Values(Vec::new()),
        graph_counts: Vec::new(),
        native_result: false,
        initializer_failed: false,
    };

    if let Some(init) = &job.initializer {
        let mut scope = graphersal::script::graph_scope_with_options(
            job.graph.clone(),
            &[],
            std::sync::Arc::new(graphersal::auth::AllowAll),
        );
        push_constants(&mut scope);
        if let Err(e) = evaluate(setup, &mut scope, init) {
            result.outcome = e.into_outcome();
            result.initializer_failed = true;
            return result;
        }
    }

    let mut scope = graphersal::script::graph_scope_with_options(
        job.graph.clone(),
        &[(
            "evaluationTimeout",
            ElementProperty::Number(Number::from_i64(EVALUATION_TIMEOUT_MS)),
        )],
        std::sync::Arc::new(graphersal::auth::AllowAll),
    );
    push_constants(&mut scope);
    let seeds = job
        .side_effects
        .iter()
        .map(|(name, expected)| (name, expected, true));
    let params = job
        .params
        .iter()
        .map(|(name, expected)| (name, expected, false));
    for (name, expected, is_seed) in params.chain(seeds) {
        match bind_param_or_seed(user, &job.graph, expected, is_seed) {
            Ok(value) => {
                scope.push_dynamic(name.clone(), value);
            }
            Err(BindError::Untranslatable(reason)) => {
                result.outcome = RawOutcome::Untranslatable(format!("binding {name}: {reason}"));
                return result;
            }
            Err(BindError::Failure(failure)) => {
                result.outcome = RawOutcome::ScriptError(failure);
                return result;
            }
        }
    }

    match evaluate(user, &mut scope, &job.script) {
        Ok(mut evaluated) => {
            if job.iteration == Iteration::Next {
                evaluated.values.truncate(1);
            }
            result.native_result = evaluated.native;
            result.outcome = RawOutcome::Values(evaluated.values);
        }
        Err(e) => {
            result.outcome = e.into_outcome();
            return result;
        }
    }

    for (text, translated) in &job.graph_counts {
        let count = match translated {
            Err(reason) => Err(reason.clone()),
            Ok(script) => match evaluate(user, &mut scope, script) {
                Err(e) => Err(e.describe()),
                // Upstream counts the traversal's results (`toList().size()`), it does not
                // append a `count()` step.
                Ok(evaluated) => i64::try_from(evaluated.values.len())
                    .map_err(|_| "result count exceeds i64".to_string()),
            },
        };
        result.graph_counts.push((text.clone(), count));
    }
    result
}

fn push_constants(scope: &mut Scope<'_>) {
    scope.push_constant("NaN", f64::NAN);
    scope.push_constant("Infinity", f64::INFINITY);
}

/// Evaluates `script` and collects its values: a traversal through the typed hook, a native
/// value through the engine's conversion rules.
fn evaluate(engine: &Engine, scope: &mut Scope<'_>, script: &str) -> Result<Evaluated, EvalError> {
    let value = engine
        .eval_with_scope::<Dynamic>(scope, script)
        .map_err(|e| classify(&e))?;
    if value.is::<ScriptGraphTraversalSource>() {
        let mut traversal = value.cast::<ScriptGraphTraversalSource>();
        let values = traversal.to_typed_list().map_err(|e| {
            let text = graphersal::error::format_diagnostic(&e);
            if is_timeout_text(&text) {
                EvalError::Timeout
            } else {
                EvalError::Failure(ScriptFailure::Runtime(text))
            }
        })?;
        Ok(Evaluated {
            values,
            native: false,
        })
    } else if value.is_unit() {
        Ok(Evaluated {
            values: Vec::new(),
            native: true,
        })
    } else {
        Ok(Evaluated {
            values: vec![dynamic_to_element_property(value)],
            native: true,
        })
    }
}

fn is_timeout_text(text: &str) -> bool {
    text.contains("exceeded evaluationTimeout")
}

/// Maps a Rhai error to the harness's failure classes.
fn classify(err: &EvalAltResult) -> EvalError {
    match err {
        EvalAltResult::ErrorInFunctionCall(_, _, inner, _) => classify(inner),
        EvalAltResult::ErrorFunctionNotFound(signature, _) => {
            let name = signature
                .split(" (")
                .next()
                .unwrap_or(signature)
                .trim()
                .to_string();
            EvalError::Failure(ScriptFailure::Missing {
                name,
                signature: signature.clone(),
            })
        }
        EvalAltResult::ErrorVariableNotFound(name, _) => {
            EvalError::Failure(ScriptFailure::Missing {
                name: name.clone(),
                signature: name.clone(),
            })
        }
        EvalAltResult::ErrorPropertyNotFound(name, _) => {
            EvalError::Failure(ScriptFailure::Missing {
                name: name.clone(),
                signature: format!("property {name}"),
            })
        }
        EvalAltResult::ErrorDotExpr(message, _) if message.starts_with("Unknown property '") => {
            EvalError::Failure(ScriptFailure::Missing {
                name: unknown_property_name(message).unwrap_or_else(|| message.clone()),
                signature: message.clone(),
            })
        }
        EvalAltResult::ErrorRuntime(value, _) => {
            let text = value.to_string();
            if is_timeout_text(&text) {
                EvalError::Timeout
            } else {
                EvalError::Failure(ScriptFailure::Runtime(text))
            }
        }
        other => EvalError::Failure(ScriptFailure::Other(other.to_string())),
    }
}

/// `"Unknown property 'shuffle' - a getter is not registered for type 'Order'"` → `Order.shuffle`.
fn unknown_property_name(message: &str) -> Option<String> {
    let quoted: Vec<&str> = message.split('\'').collect();
    match quoted.as_slice() {
        [_, property, _, ty, ..] => Some(format!("{ty}.{property}")),
        [_, property, ..] => Some((*property).to_string()),
        _ => None,
    }
}

/// The date or time a script value holds (one script type per temporal type).
fn temporal_of(value: &Dynamic) -> Option<graphersal::temporal::Temporal> {
    use graphersal::temporal::{Date, DateTime, Duration, LocalDateTime, Temporal, Time};
    if let Some(v) = value.read_lock::<DateTime>() {
        return Some(Temporal::DateTime(*v));
    }
    if let Some(v) = value.read_lock::<Date>() {
        return Some(Temporal::Date(*v));
    }
    if let Some(v) = value.read_lock::<LocalDateTime>() {
        return Some(Temporal::LocalDateTime(*v));
    }
    if let Some(v) = value.read_lock::<Time>() {
        return Some(Temporal::Time(*v));
    }
    value
        .read_lock::<Duration>()
        .map(|v| Temporal::Duration(*v))
}

/// Local equivalent of the engine's private `dynamic_to_element_property` (`script/dynamic.rs`).
fn dynamic_to_element_property(value: Dynamic) -> ElementProperty {
    if value.is_string() {
        ElementProperty::String(value.to_string().into())
    } else if let Ok(f) = value.as_float() {
        ElementProperty::Number(Number::from_f64(f))
    } else if let Ok(i) = value.as_int() {
        ElementProperty::Number(Number::from_i64(i))
    } else if let Ok(b) = value.as_bool() {
        ElementProperty::Boolean(b)
    } else if value.is_array() {
        let array = value.cast::<rhai::Array>();
        ElementProperty::Array(array.into_iter().map(dynamic_to_element_property).collect())
    } else if value.is_map() {
        let map = value.cast::<rhai::Map>();
        ElementProperty::Object(Box::new(
            map.into_iter()
                .map(|(k, v)| {
                    (
                        graphersal::prelude::CompactString::from(k.as_str()),
                        dynamic_to_element_property(v),
                    )
                })
                .collect(),
        ))
    } else if value.is_unit() {
        ElementProperty::Null
    } else if let Some(uuid) = value.read_lock::<ScriptUuid>() {
        ElementProperty::Uuid(uuid.as_u128())
    } else if let Some(temporal) = temporal_of(&value) {
        ElementProperty::Temporal(temporal)
    } else if let Some(map) = value.read_lock::<ScriptMap>() {
        ElementProperty::from_entries(
            map.as_value_map()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone())),
        )
    } else {
        ElementProperty::String(value.to_string().into())
    }
}

// ---------------------------------------------------------------------------------------------
// Parameter binding
// ---------------------------------------------------------------------------------------------

enum BindError {
    Untranslatable(String),
    Failure(ScriptFailure),
}

/// A Rhai string literal for `s`.
fn rhai_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Evaluates a small helper script on a plain scope over `graph` and returns the raw value.
fn eval_helper(
    engine: &Engine,
    graph: &Arc<dyn AnyGraph>,
    script: &str,
) -> Result<Dynamic, BindError> {
    let mut scope = graphersal::script::graph_scope_with_options(
        graph.clone(),
        &[],
        std::sync::Arc::new(graphersal::auth::AllowAll),
    );
    engine
        .eval_with_scope::<Dynamic>(&mut scope, script)
        .map_err(|e| match classify(&e) {
            EvalError::Failure(f) => BindError::Failure(f),
            EvalError::Timeout => BindError::Failure(ScriptFailure::Other(format!(
                "parameter binding timed out: {script}"
            ))),
        })
}

/// The single id string returned by the lookup traversal `script`.
fn lookup_id(
    engine: &Engine,
    graph: &Arc<dyn AnyGraph>,
    script: &str,
    what: &str,
) -> Result<String, BindError> {
    let value = eval_helper(engine, graph, script)?;
    let binding_error =
        |m: String| BindError::Failure(ScriptFailure::Other(format!("parameter binding: {m}")));
    if !value.is::<ScriptGraphTraversalSource>() {
        return Err(binding_error(format!(
            "lookup of {what} did not yield a traversal"
        )));
    }
    let ids = value
        .cast::<ScriptGraphTraversalSource>()
        .to_typed_list()
        .map_err(|e| binding_error(format!("lookup of {what} failed: {e}")))?;
    match ids.as_slice() {
        [ElementProperty::String(id)] => Ok(id.to_string()),
        [] => Err(binding_error(format!("{what} not found in the graph"))),
        [_] => Err(binding_error(format!(
            "{what} has a non-string id: {ids:?}"
        ))),
        _ => Err(binding_error(format!(
            "{what} is ambiguous ({} matches)",
            ids.len()
        ))),
    }
}

fn vertex_id(engine: &Engine, graph: &Arc<dyn AnyGraph>, name: &str) -> Result<String, BindError> {
    lookup_id(
        engine,
        graph,
        &format!("g.V().has(\"name\", {}).id()", rhai_str(name)),
        &format!("v[{name}]"),
    )
}

fn edge_id(
    engine: &Engine,
    graph: &Arc<dyn AnyGraph>,
    out: &str,
    label: &str,
    inv: &str,
) -> Result<String, BindError> {
    lookup_id(
        engine,
        graph,
        &format!(
            "g.E().hasLabel({}).where(__.outV().has(\"name\", {})).where(__.inV().has(\"name\", {})).id()",
            rhai_str(label),
            rhai_str(out),
            rhai_str(inv)
        ),
        &format!("e[{out}-{label}->{inv}]"),
    )
}

/// Binds a parameter, or a side-effect seed: a seed given as a set (`s[..]`) becomes a
/// `Set.ofList([..])` so the side effect collects each value once; a set as a plain parameter
/// stays an array (a set is not a value type here).
fn bind_param_or_seed(
    engine: &Engine,
    graph: &Arc<dyn AnyGraph>,
    expected: &Expected,
    is_seed: bool,
) -> Result<Dynamic, BindError> {
    let list = bind(engine, graph, expected)?;
    if !(is_seed && matches!(expected, Expected::Set(_))) {
        return Ok(list);
    }
    let mut scope = graphersal::script::graph_scope_with_options(
        graph.clone(),
        &[],
        std::sync::Arc::new(graphersal::auth::AllowAll),
    );
    scope.push("items", list);
    engine
        .eval_with_scope::<Dynamic>(&mut scope, "Set.ofList(items)")
        .map_err(|e| match classify(&e) {
            EvalError::Failure(f) => BindError::Failure(f),
            EvalError::Timeout => BindError::Failure(ScriptFailure::Other(
                "parameter binding timed out: Set.ofList".to_string(),
            )),
        })
}

/// Converts a parsed parameter / side-effect value to the Rhai value bound in the scope.
fn bind(
    engine: &Engine,
    graph: &Arc<dyn AnyGraph>,
    expected: &Expected,
) -> Result<Dynamic, BindError> {
    let unsupported = |what: &str| {
        Err(BindError::Untranslatable(format!(
            "unsupported literal type: {what}"
        )))
    };
    match expected {
        Expected::Null => Ok(Dynamic::UNIT),
        Expected::Bool(b) => Ok(Dynamic::from_bool(*b)),
        Expected::Str(s) => Ok(Dynamic::from(s.clone())),
        Expected::Int { value, width } => match width {
            // Test-surface deviation: byte/short/bigint collapse to int64 (tests/tinkerpop/README.md).
            IntWidth::Byte
            | IntWidth::Short
            | IntWidth::BigInt
            | IntWidth::Int
            | IntWidth::Long => match i64::try_from(*value) {
                Ok(v) => Ok(Dynamic::from_int(v)),
                Err(_) => unsupported("biginteger"),
            },
        },
        Expected::Float {
            value,
            width: FloatWidth::Float | FloatWidth::Double,
        } => Ok(Dynamic::from_float(*value)),
        // Test-surface deviation: a BigDecimal collapses to float64 when it fits one.
        Expected::BigDecimal(text) => match text.trim().parse::<f64>() {
            Ok(v) if v.is_finite() => Ok(Dynamic::from_float(v)),
            _ => unsupported("bigdecimal"),
        },
        Expected::DateTime(text) => eval_helper(
            engine,
            graph,
            &format!("datetime({})", rhai_str(text.trim())),
        ),
        Expected::VertexId(name) | Expected::VertexSid(name) => {
            vertex_id(engine, graph, name).map(Dynamic::from)
        }
        Expected::EdgeId { out, label, inv } | Expected::EdgeSid { out, label, inv } => {
            edge_id(engine, graph, out, label, inv).map(Dynamic::from)
        }
        Expected::Vertex(name) => {
            let id = vertex_id(engine, graph, name)?;
            eval_helper(engine, graph, &format!("g.V({}).next()", rhai_str(&id)))
        }
        Expected::Edge { out, label, inv } => {
            let id = edge_id(engine, graph, out, label, inv)?;
            eval_helper(engine, graph, &format!("g.E({}).next()", rhai_str(&id)))
        }
        Expected::Uuid(text) => eval_helper(
            engine,
            graph,
            &format!("UUID.fromString({})", rhai_str(text.trim())),
        ),
        Expected::TokenT(t) => eval_helper(engine, graph, token_t(*t)),
        Expected::Direction(d) => eval_helper(engine, graph, direction(*d)),
        Expected::Merge(m) => eval_helper(engine, graph, merge(*m)),
        Expected::List(items) | Expected::Set(items) => items
            .iter()
            .map(|item| bind(engine, graph, item))
            .collect::<Result<rhai::Array, _>>()
            .map(Dynamic::from_array),
        Expected::Map(entries) => {
            let mut map = rhai::Map::new();
            for (key, value) in entries {
                // Token keys are the DSL's reserved string keys ("T.label", "Direction.OUT").
                let key: &str = match key {
                    Expected::Str(key) => key.as_str(),
                    Expected::TokenT(t) => token_t(*t),
                    Expected::Direction(d) => direction(*d),
                    other => {
                        return Err(BindError::Untranslatable(format!(
                            "map with non-string key {} (Rhai object-map keys are strings)",
                            describe_key(other)
                        )));
                    }
                };
                map.insert(key.into(), bind(engine, graph, value)?);
            }
            Ok(Dynamic::from_map(map))
        }
        Expected::VertexProperty { .. } => Err(BindError::Untranslatable(
            "vertex property (vp[..]) parameter value".to_string(),
        )),
        Expected::Path(_) => Err(BindError::Untranslatable(
            "path (p[..]) parameter value".to_string(),
        )),
    }
}

fn token_t(t: TToken) -> &'static str {
    match t {
        TToken::Id => "T.id",
        TToken::Label => "T.label",
    }
}

fn direction(d: Dir) -> &'static str {
    match d {
        Dir::Out => "Direction.OUT",
        Dir::In => "Direction.IN",
    }
}

fn merge(m: MergeToken) -> &'static str {
    match m {
        MergeToken::OutV => "Merge.outV",
        MergeToken::InV => "Merge.inV",
    }
}

fn describe_key(key: &Expected) -> String {
    match key {
        Expected::TokenT(t) => format!("t[{}]", &token_t(*t)[2..]),
        Expected::Direction(d) => format!("D[{}]", &direction(*d)["Direction.".len()..]),
        Expected::Merge(m) => format!("M[{}]", &merge(*m)["Merge.".len()..]),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::gherkin;

    fn scenario(text: &str) -> Scenario {
        let feature = gherkin::parse_feature("t.feature", text).expect("parses");
        feature.scenarios.into_iter().next().expect("one scenario")
    }

    fn feature(body: &str) -> String {
        format!("Feature: T\n\n  Scenario: s\n{body}")
    }

    #[test]
    fn panic_is_caught_with_message_and_location() {
        let result = guarded(|| panic!("boom"));
        match result.outcome {
            RawOutcome::Panic(message) => {
                assert!(message.starts_with("boom (at "), "{message}");
                assert!(message.contains("execute.rs"), "{message}");
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(!IN_SCENARIO.with(Cell::get));
    }

    #[test]
    fn mutating_step_detection() {
        assert!(contains_mutating_step("g.addV(\"person\")"));
        assert!(contains_mutating_step("g.V().property (\"a\", 1)"));
        assert!(contains_mutating_step("g.V().sideEffect(__.drop())"));
        assert!(!contains_mutating_step("g.V().properties(\"a\")"));
        assert!(!contains_mutating_step("g.V().has(\"name\", \"drop()\")"));
        assert!(!contains_mutating_step("g.V().values(\"property\")"));
    }

    #[test]
    fn modern_traversal_returns_typed_vertices() {
        let run = run(&scenario(&feature(
            "    Given the modern graph\n    And the traversal of:\n      \"\"\"\n      g.V().has(\"name\", \"marko\")\n      \"\"\"\n    When iterated to list\n    Then the result should be empty\n",
        )));
        assert!(!run.fresh_graph);
        match &run.outcome {
            RawOutcome::Values(v) => assert!(matches!(v.as_slice(), [ElementProperty::Vertex(_)])),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn parameters_bind_ids_and_elements() {
        let run = run(&scenario(&feature(
            "    Given the modern graph\n    And using the parameter vid1 defined as \"v[marko].id\"\n    And using the parameter v1 defined as \"v[josh]\"\n    And using the parameter eid defined as \"e[marko-knows->vadas].id\"\n    And the traversal of:\n      \"\"\"\n      g.V(vid1).outE().hasId(eid).inV()\n      \"\"\"\n    When iterated next\n    Then the result should be empty\n",
        )));
        match &run.outcome {
            RawOutcome::Values(v) => assert_eq!(v.len(), 1, "{v:?}"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn missing_step_and_token_are_classified() {
        let run = run(&scenario(&feature(
            "    Given the modern graph\n    And the traversal of:\n      \"\"\"\n      g.V().noSuchStep()\n      \"\"\"\n    When iterated to list\n    Then the result should be empty\n",
        )));
        match &run.outcome {
            RawOutcome::ScriptError(ScriptFailure::Missing { name, .. }) => {
                assert_eq!(name, "noSuchStep")
            }
            other => panic!("unexpected {other:?}"),
        }
        let run = super::run(&scenario(&feature(
            "    Given the modern graph\n    And the traversal of:\n      \"\"\"\n      g.V().has(\"age\", NoSuchToken.x)\n      \"\"\"\n    When iterated to list\n    Then the result should be empty\n",
        )));
        match &run.outcome {
            RawOutcome::ScriptError(ScriptFailure::Missing { name, .. }) => {
                assert_eq!(name, "NoSuchToken")
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn mutating_scenario_runs_on_a_fresh_copy_with_graph_counts() {
        let run = run(&scenario(&feature(
            "    Given the modern graph\n    And the traversal of:\n      \"\"\"\n      g.V().drop()\n      \"\"\"\n    When iterated to list\n    Then the result should be empty\n    And the graph should return 0 for count of \"g.V()\"\n",
        )));
        assert!(run.fresh_graph);
        assert_eq!(run.outcome, RawOutcome::Values(Vec::new()));
        assert_eq!(run.graph_counts, vec![("g.V()".to_string(), Ok(0))]);
        let template = dataset::load(GraphSetup::Modern).expect("modern");
        assert_eq!(dataset::counts(&template), (6, 6));
    }

    #[test]
    fn initializer_runs_on_the_empty_graph() {
        let run = run(&scenario(&feature(
            "    Given the empty graph\n    And the graph initializer of:\n      \"\"\"\n      g.addV(\"person\").property(\"name\", \"a\")\n      \"\"\"\n    And the traversal of:\n      \"\"\"\n      g.V().values(\"name\")\n      \"\"\"\n    When iterated to list\n    Then the result should be empty\n",
        )));
        assert_eq!(
            run.outcome,
            RawOutcome::Values(vec![ElementProperty::String("a".into())])
        );
    }

    #[test]
    fn graph_computer_only_and_crew_are_skipped() {
        let run = run(&scenario(
            "Feature: T\n\n  @GraphComputerOnly\n  Scenario: s\n    Given the modern graph\n    And the traversal of:\n      \"\"\"\n      g.V()\n      \"\"\"\n    When iterated to list\n    Then the result should be empty\n",
        ));
        assert_eq!(
            run.outcome,
            RawOutcome::Skipped("@GraphComputerOnly".into())
        );
        let run = super::run(&scenario(&feature(
            "    Given the crew graph\n    And the traversal of:\n      \"\"\"\n      g.V()\n      \"\"\"\n    When iterated to list\n    Then the result should be empty\n",
        )));
        assert_eq!(
            run.outcome,
            RawOutcome::Skipped(dataset::CREW_MULTI_PROPERTIES.into())
        );
    }

    #[test]
    fn map_parameter_with_token_keys_binds_reserved_string_keys() {
        // `t[label]` / `t[id]` keys bind as the DSL's reserved keys "T.label" / "T.id".
        let run = run(&scenario(&feature(
            "    Given the empty graph\n    And using the parameter xx1 defined as \"m[{\\\"t[label]\\\": \\\"person\\\", \\\"t[id]\\\": \\\"7\\\"}]\"\n    And the traversal of:\n      \"\"\"\n      g.mergeV(xx1).id()\n      \"\"\"\n    When iterated to list\n    Then the result should be empty\n",
        )));
        match &run.outcome {
            RawOutcome::Values(values) => assert_eq!(values, &vec![ElementProperty::from("7")]),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn map_parameter_with_direction_key_binds_and_merge_key_is_untranslatable() {
        let direction = run(&scenario(&feature(
            "    Given the empty graph\n    And using the parameter xx1 defined as \"m[{\\\"D[OUT]\\\": 1}]\"\n    And the traversal of:\n      \"\"\"\n      g.inject(xx1).select(keys)\n      \"\"\"\n    When iterated to list\n    Then the result should be empty\n",
        )));
        match &direction.outcome {
            RawOutcome::Values(values) => assert_eq!(
                values,
                &vec![ElementProperty::Array(vec![ElementProperty::from(
                    "Direction.OUT"
                )])]
            ),
            other => panic!("unexpected {other:?}"),
        }
        let merge = run(&scenario(&feature(
            "    Given the empty graph\n    And using the parameter xx1 defined as \"m[{\\\"M[outV]\\\": 1}]\"\n    And the traversal of:\n      \"\"\"\n      g.inject(xx1)\n      \"\"\"\n    When iterated to list\n    Then the result should be empty\n",
        )));
        match &merge.outcome {
            RawOutcome::Untranslatable(reason) => assert!(reason.contains("M[outV]"), "{reason}"),
            other => panic!("unexpected {other:?}"),
        }
    }
}
