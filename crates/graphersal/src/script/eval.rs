//! Script evaluation entry points: the session scope (`graph_scope*`), the `eval_*` functions,
//! named and positional parameters and `RESERVED_PARAM_NAMES`.

use super::*;

/// Builds the scope a script runs in: the traversal source `g` over `g`, the anonymous
/// traversal spawn `__` and the DSL globals (`P`, `TextP`, `GType`, `V`, `By`, `CastPolicy`,
/// `ProfileType`, `Order`, `Column` and its bare tokens `keys` / `values`).
///
/// The scope allows everything ([`AllowAll`](crate::graph::AllowAll)). Use
/// [`graph_scope_with_options`] to restrict what the script may do.
pub fn graph_scope<'a>(g: impl IntoAnyGraph) -> Scope<'a> {
    graph_scope_with_options(g, &[], Arc::new(crate::graph::AllowAll))
}

/// Builds the same scope as [`graph_scope`], checked by `authorizer`, with `options` preset on
/// every traversal source the scope provides, as if each query started with `g.with(key, value)`
/// for each of them.
///
/// `authorizer` decides what every traversal of the script may do on `g` (asked once per
/// request of a plan, before anything runs). `__` and `_g` are scratch graphs private to the
/// script: writing to them asks no authorizer. A preset option is the host's, so an
/// administrative key (`evaluationTimeout`, ...) set here asks nothing; the script's own
/// `g.with(..)` of such a key asks for `Update` on its `Option`. The presets apply to `g`, `_g`
/// and `__` (also a traversal started from `__`, such as `__.inject(1).repeat(..)`); pass the
/// same authorizer and options to [`engine_with_options`]
/// so the sources a script creates itself (`GraphSource::empty()`, ...) get them too.
///
/// Script hosts use it to apply defaults such as an `evaluationTimeout`, or the session display
/// limits from [`render_scope_options`] ([`RENDER_MAX_ROWS`], [`RENDER_MAX_ITEMS`]; without them
/// every display is unbounded). A query's own `g.with(...)` overrides a default, because setting a
/// key again replaces its value:
///
/// ```rust
/// # use std::sync::Arc;
/// # use graphersal::prelude::*;
/// # use graphersal::auth::{AllowAll, Authorizer};
/// let graph = Arc::new(GraphSource::tinkerpop_modern());
/// let authorizer: Arc<dyn Authorizer> = Arc::new(AllowAll);
/// let mut scope = graphersal::script::graph_scope_with_options(
///     graph,
///     &[("evaluationTimeout", ElementProperty::from(30_000))],
///     authorizer.clone(),
/// );
/// let count = graphersal::script::engine_with_limits(&Default::default(), authorizer)
///     .eval_with_scope::<graphersal::script::rhai::Dynamic>(
///         &mut scope,
///         r#"g.with("evaluationTimeout", 0).v().count().next()"#,
///     )
///     .unwrap();
/// assert_eq!(count.as_int().unwrap(), 6);
/// ```
pub fn graph_scope_with_options<'a>(
    g: impl IntoAnyGraph,
    options: &[(&str, ElementProperty)],
    authorizer: Arc<dyn crate::graph::Authorizer>,
) -> Scope<'a> {
    let access = Arc::new(access::ScriptAccess::new(authorizer, options));
    let mut source = ScriptGraphSource::graph(g).with_access(access.clone(), false);
    for (key, value) in options {
        source
            .options
            .insert(CompactString::new(key), value.clone());
    }
    scope_with_access(source, access)
}

/// The scope of [`graph_scope_with_options`] around the session source `g` (its graph, access
/// and options as given): the scratch sources `__` and `_g` get `access` and its preset
/// options, then the DSL globals. Also the scope a saved query's body runs in
/// (`saved_query.rs`), with the caller's `g` under the read-only access.
pub(super) fn scope_with_access<'a>(
    g: ScriptGraphTraversalSource,
    access: Arc<access::ScriptAccess>,
) -> Scope<'a> {
    let scratch = |anonymous: bool| {
        let mut source = ScriptGraphSource::empty().with_access(access.clone(), true);
        source.anonymous = anonymous;
        for (key, value) in access.presets.iter() {
            source.options.insert(key.clone(), value.clone());
        }
        source
    };
    let mut scope: Scope = Scope::new();
    scope.push("g", g);
    scope.push("__", scratch(true));
    scope.push("_g", scratch(false));
    scope.push("P", ScriptP);
    scope.push("GType", ScriptGType);
    scope.push("UUID", ScriptUuidTokens);
    scope.push("Set", ScriptSetTokens);
    scope.push("Column", ScriptColumn);
    // The bare tokens are plain variables like the class markers, not scope constants: Rhai's
    // optimizer folds `${keys}` of a constant at compile time with the Rust type name instead of
    // the registered `to_string` (`Column.keys`).
    scope.push("keys", Column::Keys);
    scope.push("values", Column::Values);
    scope.push("TextP", ScriptTextP);
    scope.push("V", ScriptV);
    scope.push("By", ScriptBy);
    scope.push("CastPolicy", ScriptCastPolicy);
    scope.push("ProfileType", ScriptProfileType);
    scope.push("Order", ScriptOrder);
    scope.push("Pick", ScriptPick);
    scope.push("T", ScriptT);
    scope.push("Scope", ScriptScope);
    scope.push("Cardinality", ScriptCardinality);
    scope.push("Direction", ScriptDirection);
    scope.push("Merge", ScriptMerge);
    scope.push("Operator", ScriptOperator);
    scope.push("Barrier", ScriptBarrier);
    scope.push("WithOptions", ScriptWithOptions);
    scope.push("Pop", ScriptPop);
    scope.push("SchemaMode", ScriptSchemaMode);
    scope.push("first", Pop::First);
    scope.push("last", Pop::Last);
    scope.push("all", Pop::All);
    scope.push("mixed", Pop::Mixed);
    // `DT` and its bare units (`dateAdd(hour, 2)`), shadowable like `first`/`last`.
    scope.push("DT", ScriptDT);
    scope.push("second", crate::value::dt::DT::Second);
    scope.push("minute", crate::value::dt::DT::Minute);
    scope.push("hour", crate::value::dt::DT::Hour);
    scope.push("day", crate::value::dt::DT::Day);

    scope
}

/// Convenience helper to evaluate a Gremlin Rhai script string against a given graph and render
/// the result for display.
///
/// Use [`eval_value`] instead when the script result is to be consumed programmatically: this
/// helper renders, it does not return data. Everything is allowed (see [`eval_value`]).
pub fn eval(graph: impl IntoAnyGraph, script: &str) -> Result<String, Box<rhai::EvalAltResult>> {
    let value = eval_value(graph, script)?;
    Ok(render_value(value, VisualizeFormat::Auto, &RenderOptions::unlimited()).text)
}

/// Evaluates a Gremlin Rhai script string against a given graph and returns the raw script
/// value.
///
/// A script ending in `toList()` yields a Rhai array of native values, `next()` yields a single
/// value, and a script ending on a traversal that was never terminated yields the pipeline
/// itself, which [`render_value`] turns into its default visualization.
///
/// Runs under the SAFE [`ScriptLimits::default`]; a limit hit fails with the diagnostic of a
/// [`TraverserError::ResourceLimitExceeded`](crate::TraverserError::ResourceLimitExceeded). Use
/// [`eval_value_with_limits`] to choose the limits.
///
/// Everything is allowed ([`AllowAll`](crate::graph::AllowAll)). For a script the host did
/// not write, use [`eval_value_with_limits`] with an [`Authorizer`](crate::graph::Authorizer)
/// such as [`AccessPolicy::read_only`](crate::graph::AccessPolicy::read_only): a denied request
/// fails with the diagnostic of
/// [`TraverserError::PermissionDenied`](crate::TraverserError::PermissionDenied) before its
/// traversal runs.
///
/// ```rust
/// # use std::sync::Arc;
/// # use graphersal::prelude::*;
/// let graph = Arc::new(GraphSource::tinkerpop_modern());
/// let count = graphersal::script::eval_value(graph.clone(), "g.V().count().next()");
/// assert_eq!(count.unwrap().as_int().unwrap(), 6);
/// ```
pub fn eval_value(
    graph: impl IntoAnyGraph,
    script: &str,
) -> Result<Dynamic, Box<rhai::EvalAltResult>> {
    eval_value_with_limits(
        graph,
        script,
        Default::default(),
        &ScriptLimits::default(),
        Arc::new(crate::graph::AllowAll),
    )
}

/// The user-facing form of a script evaluation error, the one place every `eval_*` entry (and
/// a front end that evaluates on its own engine) passes a failure through:
///
/// - a Rhai resource-limit error becomes the structured diagnostic of
///   [`TraverserError::ResourceLimitExceeded`](crate::TraverserError::ResourceLimitExceeded),
///   the same string form every other engine error takes;
/// - the type names in Rhai's own type and call errors (`Function not found: to (i64,
///   graphersal::value::merge::Merge)`, `Data type incorrect: f64 (expecting ..)`) become the
///   display names of the DSL (`int64`, `Merge`, `float64`, `Predicate`,
///   `GraphTraversalSource`): a diagnostic never shows a Rust type path or a Rust primitive name.
///
/// Any other error passes through unchanged.
pub fn map_eval_error(
    err: Box<rhai::EvalAltResult>,
    limits: &ScriptLimits,
) -> Box<rhai::EvalAltResult> {
    use super::register::display_type_names_in as names;
    if let Some(limit) = resource_limit_from_rhai(&err, limits) {
        return Box::new(EvalAltResult::ErrorRuntime(
            Dynamic::from(crate::error::format_diagnostic(&limit)),
            err.position(),
        ));
    }
    Box::new(match *err {
        EvalAltResult::ErrorFunctionNotFound(signature, position) => {
            let signature = names(&signature);
            match snake_case_twin(&signature) {
                Some(snake) => EvalAltResult::ErrorRuntime(
                    Dynamic::from(format!(
                        "Function not found: {signature}\nHelp: Graphersal's own functions are \
                         spelled in snake_case only since 0.3.0 (both spellings are kept for \
                         TinkerPop's steps alone): write `{snake}(..)`."
                    )),
                    position,
                ),
                None => EvalAltResult::ErrorFunctionNotFound(signature, position),
            }
        }
        EvalAltResult::ErrorMismatchDataType(expected, actual, position) => {
            EvalAltResult::ErrorMismatchDataType(names(&expected), names(&actual), position)
        }
        EvalAltResult::ErrorMismatchOutputType(expected, actual, position) => {
            EvalAltResult::ErrorMismatchOutputType(names(&expected), names(&actual), position)
        }
        EvalAltResult::ErrorIndexingType(type_name, position) => {
            EvalAltResult::ErrorIndexingType(names(&type_name), position)
        }
        EvalAltResult::ErrorInFunctionCall(name, source, inner, position) => {
            EvalAltResult::ErrorInFunctionCall(
                name,
                source,
                map_eval_error(inner, limits),
                position,
            )
        }
        other => other,
    })
}

/// The registered snake_case name of a camelCase call the engine did not find (`createIndex`
/// -> `create_index`): the camelCase twins of Graphersal's extensions were removed in 0.3.0
/// (rule 2), so a script written before gets the name to use instead.
fn snake_case_twin(signature: &str) -> Option<String> {
    let name = signature.split([' ', '(']).next()?.trim();
    if !name.starts_with(|c: char| c.is_ascii_lowercase())
        || !name.chars().any(|c| c.is_ascii_uppercase())
        || !name.chars().all(|c| c.is_ascii_alphanumeric())
    {
        return None;
    }
    let mut snake = String::with_capacity(name.len() + 4);
    for c in name.chars() {
        if c.is_ascii_uppercase() {
            snake.push('_');
            snake.push(c.to_ascii_lowercase());
        } else {
            snake.push(c);
        }
    }
    super::registered_step_names()
        .contains(&snake)
        .then_some(snake)
}

/// [`eval_value_with_params`] under explicit [`ScriptLimits`] instead of the SAFE defaults and
/// checked by `authorizer` instead of allowing everything: the entry for a script the host did
/// not write. A denied request fails with the diagnostic of
/// [`TraverserError::PermissionDenied`](crate::TraverserError::PermissionDenied) before its
/// traversal runs.
///
/// ```rust
/// # use std::sync::Arc;
/// # use graphersal::prelude::*;
/// # use graphersal::script::ScriptLimits;
/// # use graphersal::auth::{AccessPolicy, Authorizer};
/// let graph = Arc::new(GraphSource::tinkerpop_modern());
/// let limits = ScriptLimits::default().with_max_operations(1_000);
/// let read_only: Arc<dyn Authorizer> = Arc::new(AccessPolicy::read_only());
/// let looped = graphersal::script::eval_value_with_limits(
///     graph.clone(),
///     "let n = 0; loop { n += 1; }",
///     Default::default(),
///     &limits,
///     read_only.clone(),
/// );
/// assert!(looped.is_err());
/// let denied = graphersal::script::eval_value_with_limits(
///     graph,
///     r#"g.addV("x").next()"#,
///     Default::default(),
///     &limits,
///     read_only,
/// );
/// assert!(denied.unwrap_err().to_string().contains("Permission denied"));
/// ```
pub fn eval_value_with_limits(
    graph: impl IntoAnyGraph,
    script: &str,
    params: std::collections::HashMap<String, ElementProperty>,
    limits: &ScriptLimits,
    authorizer: Arc<dyn crate::graph::Authorizer>,
) -> Result<Dynamic, Box<rhai::EvalAltResult>> {
    let engine = engine_with_limits(limits, authorizer.clone());
    let mut scope = graph_scope_with_options(graph, &[], authorizer);
    push_named_params(&mut scope, params, limits.effective_max_value_depth())?;
    engine
        .eval_with_scope::<Dynamic>(&mut scope, script)
        .map_err(|err| map_eval_error(err, limits))
}

/// [`eval_value_with_limits`] with the WHOLE script as one unit of atomicity: either every
/// change the script makes stays, or none does.
///
/// By default (every other `eval_*` entry) the unit is one traversal: a failing statement rolls
/// back only itself, and the statements before it stay (TinkerPop-like). This host option
/// (`book/src/users_guide/transactions.md`; the script cannot opt in itself) turns the script into
/// one transaction, the script-side counterpart of
/// [`Transactional::transaction`](crate::graph::Transactional::transaction):
///
/// - every traversal the script runs is a nested unit (a savepoint): a failing one rolls back
///   only itself, and a script that handles the failure (`execute()`, which never throws, or a
///   `try`/`catch`) goes on;
/// - when the script fails (an uncaught error, a resource limit) everything it changed is rolled
///   back and the error is returned;
/// - when it succeeds the script commits once: one commit sequence number and one
///   [`ChangeSet`](crate::ChangeSet) for the [commit hooks](crate::graph::CommitHook); a
///   `before_commit` veto rolls everything back and fails the call with the diagnostic of
///   [`GraphError::CommitRejected`](crate::error::GraphError::CommitRejected).
///
/// The calling thread holds the unit for the whole script ([`Graph::begin_unit`](crate::Graph::begin_unit)):
/// no other writer (and no other whole-script unit) of `graph` runs between its statements
/// (serializable, no isolation levels). Other readers go on while the script only reads; from
/// its first write until it ends they wait, so nobody sees its uncommitted changes. Auto-generated ids are never
/// reused, also after a rollback. A traversal the script returns without a terminal is not part
/// of the unit: it runs when the caller renders it ([`render_value`]), after the commit, as a
/// unit of its own.
///
/// ```rust
/// # use std::sync::Arc;
/// # use graphersal::prelude::*;
/// # use graphersal::script::{ScriptLimits, eval_value_atomic};
/// # use graphersal::auth::AllowAll;
/// let graph = Arc::new(GraphSource::tinkerpop_modern());
/// let failed = eval_value_atomic(
///     graph.clone(),
///     r#"g.addV("person").property("name", "ann").next();
///        g.V().values("name").asNumber(GType.LONG).toList();"#,
///     Default::default(),
///     &ScriptLimits::default(),
///     Arc::new(AllowAll),
/// );
/// // The second statement fails, so the first one is rolled back too.
/// assert!(failed.is_err());
/// assert_eq!(graph.read().vertex_count(), 6);
/// ```
pub fn eval_value_atomic(
    graph: impl IntoAnyGraph,
    script: &str,
    params: std::collections::HashMap<String, ElementProperty>,
    limits: &ScriptLimits,
    authorizer: Arc<dyn crate::graph::Authorizer>,
) -> Result<Dynamic, Box<rhai::EvalAltResult>> {
    let shared = graph.into_any_graph();
    let mut params = Some(params);
    let mut authorizer = Some(authorizer);
    // The body runs once; the options only move the owned arguments into it.
    let mut body = || {
        let (Some(params), Some(authorizer)) = (params.take(), authorizer.take()) else {
            return crate::script::plan::UnitBody(Err(body_ran_twice()));
        };
        crate::script::plan::UnitBody(eval_in_unit(&shared, script, params, limits, authorizer))
    };
    shared
        .run_unit(crate::script::plan::UnitKind::Atomic, &mut body)
        .0
        .map(|(value, _)| value)
}

/// [`eval_value_atomic`] for a scheduled job: the script is one unit carrying `metadata` to its
/// commit (the job's name), and a traversal it returns runs inside that unit (a job's result is
/// not shown: what it does is its writes).
pub(crate) fn eval_job_unit(
    shared: Arc<dyn AnyGraph>,
    script: &str,
    params: std::collections::HashMap<String, ElementProperty>,
    limits: &ScriptLimits,
    authorizer: Arc<dyn crate::graph::Authorizer>,
    metadata: crate::graph::CommitMetadata,
) -> Result<(), Box<rhai::EvalAltResult>> {
    let mut params = Some(params);
    let mut authorizer = Some(authorizer);
    let mut body = || {
        let (Some(params), Some(authorizer)) = (params.take(), authorizer.take()) else {
            return crate::script::plan::UnitBody(Err(body_ran_twice()));
        };
        crate::script::plan::UnitBody(
            eval_in_unit(&shared, script, params, limits, authorizer).and_then(|value| {
                materialize_traversals(value, limits.effective_max_value_depth())
            }),
        )
    };
    shared
        .run_unit(crate::script::plan::UnitKind::Call(metadata), &mut body)
        .0
        .map(|_| ())
}

/// Runs a script like [`eval_value_atomic`] and then ALWAYS rolls everything back: a preview
/// of what the script would change, the script-side counterpart of
/// [`Transactional::dry_run`](crate::graph::Transactional::dry_run). Returns the script's result
/// together with the [`ChangeSet`](crate::ChangeSet) of the changes it made and that were
/// undone, in execution order (call [`ChangeSet::compact`](crate::ChangeSet::compact) for the
/// net effect).
///
/// - The graph is left as it was, including its schema; only the auto-id sequences keep their
///   progress (an id once handed out is never reused).
/// - No commit hook is called (nothing is committed or attempted), the change set's
///   `commit_seq` is `0` and [`TraversalGraph::last_commit_seq`](crate::TraversalGraph::last_commit_seq)
///   does not move.
/// - The whole script is one unit, as in [`eval_value_atomic`]: every traversal it runs is a
///   savepoint (a failing one that the script handles rolls back only itself and does not
///   appear in the change set). The calling thread holds the unit for the whole script, as
///   there: other readers go on while it only reads and wait from its first write until it
///   ends.
/// - A failing script returns its error and no change set (everything is rolled back).
/// - A result that holds a closure or function pointer fails with the diagnostic of
///   [`ScriptError::ReturnedClosure`](crate::error::ScriptError::ReturnedClosure).
/// - A traversal the script returns without a terminal (also inside a returned array or map)
///   runs inside the dry run, as if it ended in `toList()`, so its result reflects the
///   previewed changes ([`materialize_traversals`]).
///
/// ```rust
/// # use std::sync::Arc;
/// # use graphersal::prelude::*;
/// # use graphersal::script::{ScriptLimits, eval_value_dry_run};
/// # use graphersal::auth::AllowAll;
/// let graph = Arc::new(GraphSource::tinkerpop_modern());
/// let (count, changes) = eval_value_dry_run(
///     graph.clone(),
///     r#"g.addV("person").property("name", "ann").next(); g.V().count().next()"#,
///     Default::default(),
///     &ScriptLimits::default(),
///     Arc::new(AllowAll),
/// )
/// .unwrap();
/// assert_eq!(count.as_int().unwrap(), 7);
/// assert_eq!(changes.len(), 1);
/// // The graph is untouched.
/// assert_eq!(graph.read().vertex_count(), 6);
/// ```
pub fn eval_value_dry_run(
    graph: impl IntoAnyGraph,
    script: &str,
    params: std::collections::HashMap<String, ElementProperty>,
    limits: &ScriptLimits,
    authorizer: Arc<dyn crate::graph::Authorizer>,
) -> Result<(Dynamic, crate::graph::ChangeSet), Box<rhai::EvalAltResult>> {
    let shared = graph.into_any_graph();
    let mut params = Some(params);
    let mut authorizer = Some(authorizer);
    let mut body = || {
        let (Some(params), Some(authorizer)) = (params.take(), authorizer.take()) else {
            return crate::script::plan::UnitBody(Err(body_ran_twice()));
        };
        crate::script::plan::UnitBody(
            eval_in_unit(&shared, script, params, limits, authorizer)
                .and_then(|value| materialize_traversals(value, limits.max_value_depth)),
        )
    };
    match shared
        .run_unit(crate::script::plan::UnitKind::DryRun, &mut body)
        .0?
    {
        (value, Some(changes)) => Ok((value, changes)),
        // `UnitKind::DryRun` always reports its change set.
        (_, None) => Err(script_error(Box::new(
            crate::TraverserError::NotImplemented {
                feature: "a dry run without its change set",
            },
        ))),
    }
}

/// The error of a whole-script unit whose body is called a second time: never produced
/// ([`AnyGraph::run_unit`] calls its body once), reported instead of panicking (rule 1).
fn body_ran_twice() -> Box<rhai::EvalAltResult> {
    script_error(Box::new(crate::TraverserError::NotImplemented {
        feature: "a whole-script unit whose body runs twice",
    }))
}

/// Evaluates `script` with `params` on `graph` inside a whole-script unit
/// ([`AnyGraph::run_unit`]) and rejects a result that holds a closure: a closure would run
/// after the unit closed (outside its atomicity, and after a dry run's changes were undone), so
/// it fails like a failing script (the caller rolls everything back).
fn eval_in_unit(
    graph: &Arc<dyn AnyGraph>,
    script: &str,
    params: std::collections::HashMap<String, ElementProperty>,
    limits: &ScriptLimits,
    authorizer: Arc<dyn crate::graph::Authorizer>,
) -> Result<Dynamic, Box<rhai::EvalAltResult>> {
    let engine = engine_with_limits(limits, authorizer.clone());
    let mut scope = graph_scope_with_options(graph.clone(), &[], authorizer);
    let max_depth = limits.effective_max_value_depth();
    let evaluated = push_named_params(&mut scope, params, max_depth).and_then(|()| {
        engine
            .eval_with_scope::<Dynamic>(&mut scope, script)
            .map_err(|err| map_eval_error(err, limits))
    })?;
    // The result is walked recursively below (closures, materializing).
    if dynamic_exceeds_depth(&evaluated, max_depth) {
        return Err(script_error(Box::new(value_too_deep(max_depth))));
    }
    match returned_closure(&evaluated, "the result") {
        Some(location) => Err(script_error_from_script(
            crate::error::ScriptError::ReturnedClosure { location },
        )),
        None => Ok(evaluated),
    }
}

/// Runs every traversal in a script result that was returned without a terminal (also inside
/// arrays and maps, recursively) as if it ended in `toList()`, replacing it by the resulting
/// array; every other value is returned unchanged. A bare traversal source (`g`) yields an empty
/// array.
///
/// For hosts that hand script results on as data (a language binding, an API) rather than
/// rendering them ([`render_value`]); a traversal failure is returned as the script error a
/// `toList()` would raise.
///
/// ```rust
/// # use std::sync::Arc;
/// # use graphersal::prelude::*;
/// let graph = Arc::new(GraphSource::tinkerpop_modern());
/// let value = graphersal::script::eval_value(
///     graph,
///     r#"[g.V().has("name", "marko").values("age"), 1]"#,
/// )
/// .unwrap();
/// let value = graphersal::script::materialize_traversals(value, 0).unwrap();
/// let items = value.into_array().unwrap();
/// assert_eq!(items[0].clone().into_array().unwrap()[0].as_int().unwrap(), 29);
/// assert_eq!(items[1].as_int().unwrap(), 1);
/// ```
///
/// A result nested deeper than `max_depth` levels of arrays and maps fails with the
/// `script.max_value_depth` diagnostic instead of being walked: pass the
/// [`ScriptLimits::max_value_depth`] the script ran under (`0` reads as the default, 128).
pub fn materialize_traversals(
    value: Dynamic,
    max_depth: usize,
) -> Result<Dynamic, Box<rhai::EvalAltResult>> {
    let max = if max_depth == 0 {
        crate::value::depth::DEFAULT_MAX_VALUE_DEPTH
    } else {
        max_depth
    };
    if dynamic_exceeds_depth(&value, max) {
        return Err(script_error(Box::new(value_too_deep(max))));
    }
    materialize_checked(value)
}

/// [`materialize_traversals`] on a value whose depth was checked.
fn materialize_checked(mut value: Dynamic) -> Result<Dynamic, Box<rhai::EvalAltResult>> {
    if value.is::<ScriptGraphTraversalSource>() {
        let mut source = value.cast::<ScriptGraphTraversalSource>();
        return source.to_list().map(Dynamic::from_array);
    }
    if let Ok(mut items) = value.as_array_mut() {
        for item in items.iter_mut() {
            *item = materialize_checked(std::mem::take(item))?;
        }
    } else if let Ok(mut map) = value.as_map_mut() {
        for item in map.values_mut() {
            *item = materialize_checked(std::mem::take(item))?;
        }
    }
    Ok(value)
}

/// Where `value` holds a closure or function pointer (`FnPtr`), also inside arrays and maps:
/// `location` extended by the index or key path, or `None`.
fn returned_closure(value: &Dynamic, location: &str) -> Option<String> {
    if value.is::<rhai::FnPtr>() {
        return Some(location.to_string());
    }
    let base = if location == "the result" {
        "result"
    } else {
        location
    };
    if let Some(items) = value.read_lock::<rhai::Array>() {
        return items
            .iter()
            .enumerate()
            .find_map(|(index, item)| returned_closure(item, &format!("{base}[{index}]")));
    }
    if let Some(map) = value.read_lock::<rhai::Map>() {
        return map
            .iter()
            .find_map(|(key, item)| returned_closure(item, &format!("{base}.{key}")));
    }
    None
}

/// Names [`graph_scope`] reserves for the DSL globals, which a parameter may not shadow. The
/// list lives with the catalog of definitions (a saved query's parameters follow the same rule
/// without the `script` feature); see its documentation there.
pub(super) use crate::graph::definition::RESERVED_PARAM_NAMES;

/// Evaluates a Gremlin Rhai script string with named parameter bindings and renders the result
/// for display.
///
/// Each parameter is bound as a script variable of the same name and is additionally reachable
/// through the `params` object map, so a driver can pass user input as data instead of splicing
/// it into the script text:
///
/// ```rust
/// # use std::collections::HashMap;
/// # use std::sync::Arc;
/// # use graphersal::prelude::*;
/// let graph = Arc::new(GraphSource::tinkerpop_modern());
/// let params = HashMap::from([(
///     "target_label".to_string(),
///     ElementProperty::String("person".into()),
/// )]);
/// let rendered = graphersal::script::eval_with_params(
///     graph,
///     r#"g.v().has_label(target_label).values("name").to_list()"#,
///     params,
/// )
/// .unwrap();
/// assert!(rendered.contains("marko"));
/// ```
pub fn eval_with_params(
    graph: impl IntoAnyGraph,
    script: &str,
    params: std::collections::HashMap<String, ElementProperty>,
) -> Result<String, Box<rhai::EvalAltResult>> {
    let value = eval_value_with_params(graph, script, params)?;
    Ok(render_value(value, VisualizeFormat::Auto, &RenderOptions::unlimited()).text)
}

/// Evaluates a Gremlin Rhai script string with named parameter bindings and returns the raw
/// script value.
///
/// This is the programmatic counterpart of [`eval_with_params`], mirroring the relation between
/// [`eval_value`] and [`eval`]. Everything is allowed; see [`eval_value_with_limits`] for an
/// [`Authorizer`](crate::graph::Authorizer).
pub fn eval_value_with_params(
    graph: impl IntoAnyGraph,
    script: &str,
    params: std::collections::HashMap<String, ElementProperty>,
) -> Result<Dynamic, Box<rhai::EvalAltResult>> {
    eval_value_with_limits(
        graph,
        script,
        params,
        &ScriptLimits::default(),
        Arc::new(crate::graph::AllowAll),
    )
}

/// Evaluates a Gremlin Rhai script string with positional parameter bindings (`$1`, `$2`, ...)
/// and renders the result for display.
///
/// Placeholders are 1-based and additionally reachable through the `params` array as
/// `params[0]`, `params[1]`, ...:
///
/// ```rust
/// # use std::sync::Arc;
/// # use graphersal::prelude::*;
/// let graph = Arc::new(GraphSource::tinkerpop_modern());
/// let rendered = graphersal::script::eval_with_positional_params(
///     graph,
///     r#"g.v().has_label($1).has("age", P.gt($2)).values("name").to_list()"#,
///     vec![
///         ElementProperty::String("person".into()),
///         ElementProperty::Number(graphersal::prelude::Number::from_i64(30)),
///     ],
/// )
/// .unwrap();
/// assert!(rendered.contains("josh"));
/// ```
pub fn eval_with_positional_params(
    graph: impl IntoAnyGraph,
    script: &str,
    params: Vec<ElementProperty>,
) -> Result<String, Box<rhai::EvalAltResult>> {
    let value = eval_value_with_positional_params(graph, script, params)?;
    Ok(render_value(value, VisualizeFormat::Auto, &RenderOptions::unlimited()).text)
}

/// Evaluates a Gremlin Rhai script string with positional parameter bindings and returns the
/// raw script value.
///
/// This is the programmatic counterpart of [`eval_with_positional_params`]. Everything is
/// allowed.
pub fn eval_value_with_positional_params(
    graph: impl IntoAnyGraph,
    script: &str,
    params: Vec<ElementProperty>,
) -> Result<Dynamic, Box<rhai::EvalAltResult>> {
    let (script, referenced) = rewrite_positional_placeholders(script);
    validate_positional_indices(&referenced, params.len())?;

    let limits = ScriptLimits::default();
    let engine = engine();
    let mut scope = graph_scope(graph);
    push_positional_params(&mut scope, params, limits.effective_max_value_depth())?;
    engine
        .eval_with_scope::<Dynamic>(&mut scope, &script)
        .map_err(|err| map_eval_error(err, &limits))
}

/// Binds each named parameter as a script variable and collects them into the `params` map.
///
/// Parameters are bound in name order so that a script seeing several invalid names always
/// fails on the same one.
///
/// A parameter nested deeper than `max_depth` fails with the `script.max_value_depth` diagnostic
/// before it is converted (the conversion and every later reader recurse).
fn push_named_params(
    scope: &mut Scope<'_>,
    params: std::collections::HashMap<String, ElementProperty>,
    max_depth: usize,
) -> Result<(), Box<rhai::EvalAltResult>> {
    let mut entries: Vec<(String, ElementProperty)> = params.into_iter().collect();
    entries.sort_by(|(left, _), (right, _)| left.cmp(right));

    let mut map = rhai::Map::new();
    for (name, value) in entries {
        validate_param_name(&name)?;
        check_param_depth(&value, max_depth)?;
        let value = element_property_to_dynamic(value);
        map.insert(name.as_str().into(), value.clone());
        scope.push_dynamic(name, value);
    }
    scope.push_dynamic("params", Dynamic::from_map(map));

    Ok(())
}

/// Binds each positional parameter under its placeholder variable and collects them into the
/// `params` array.
/// `max_depth` bounds each parameter as in [`push_named_params`].
fn push_positional_params(
    scope: &mut Scope<'_>,
    params: Vec<ElementProperty>,
    max_depth: usize,
) -> Result<(), Box<rhai::EvalAltResult>> {
    for value in &params {
        check_param_depth(value, max_depth)?;
    }
    let values: Vec<Dynamic> = params
        .into_iter()
        .map(element_property_to_dynamic)
        .collect();

    for (offset, value) in values.iter().enumerate() {
        scope.push_dynamic(positional_param_name(offset + 1), value.clone());
    }
    scope.push_dynamic("params", Dynamic::from_array(values));
    Ok(())
}

/// Fails with the `script.max_value_depth` diagnostic when a host parameter nests deeper than
/// `max_depth` levels. Measured without recursion.
fn check_param_depth(
    value: &ElementProperty,
    max_depth: usize,
) -> Result<(), Box<rhai::EvalAltResult>> {
    if crate::value::depth::property_exceeds_depth(value, max_depth) {
        return Err(script_error(Box::new(value_too_deep(max_depth))));
    }
    Ok(())
}

/// Rejects parameter names that the Rhai parser cannot resolve as a variable, or that would
/// shadow a DSL global such as `g` or `P`.
fn validate_param_name(name: &str) -> Result<(), Box<rhai::EvalAltResult>> {
    if RESERVED_PARAM_NAMES.contains(&name) {
        return Err(param_error(
            name,
            format!("'{name}' is reserved by the script scope for the Gremlin DSL"),
        ));
    }

    let mut chars = name.chars();
    let valid = match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {
            chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        _ => false,
    };

    if valid {
        Ok(())
    } else {
        Err(param_error(
            name,
            "a named parameter must be an identifier: ASCII letters, digits and underscores, \
             not starting with a digit"
                .to_string(),
        ))
    }
}

/// Rejects placeholders that no supplied parameter can satisfy, before the script is parsed.
fn validate_positional_indices(
    referenced: &[usize],
    supplied: usize,
) -> Result<(), Box<rhai::EvalAltResult>> {
    for index in referenced {
        if *index == 0 {
            return Err(param_error(
                "$0",
                "positional parameters are 1-based, so the first placeholder is `$1`".to_string(),
            ));
        }
        if *index > supplied {
            return Err(param_error(
                &format!("${index}"),
                format!(
                    "the script references `${index}` but only {supplied} parameter(s) were supplied"
                ),
            ));
        }
    }

    Ok(())
}

/// Wraps a parameter binding failure into a script runtime error carrying the full diagnostic.
fn param_error(parameter: &str, message: String) -> Box<rhai::EvalAltResult> {
    Box::new(rhai::EvalAltResult::ErrorRuntime(
        Dynamic::from(crate::error::format_diagnostic(
            &crate::error::ScriptError::InvalidParameter {
                parameter: parameter.to_string(),
                message,
            },
        )),
        rhai::Position::NONE,
    ))
}

/// Scope variable name a positional placeholder is bound under.
fn positional_param_name(index: usize) -> String {
    format!("__param_{index}")
}

/// Rewrites `$1`-style positional placeholders into the scope variable names the Rhai parser
/// accepts, returning the rewritten script and every index it referenced, in first-use order.
///
/// `$` is not a legal identifier character in Rhai, so a positional script is normalized before
/// it reaches the parser. Placeholders inside string literals, character literals and comments
/// are left untouched; a backtick template string counts as a literal in full, interpolation
/// included, so `$1` is only substituted in code position.
fn rewrite_positional_placeholders(script: &str) -> (String, Vec<usize>) {
    #[derive(Clone, Copy)]
    enum Mode {
        Code,
        LineComment,
        BlockComment,
        DoubleString,
        TemplateString,
        CharLiteral,
    }

    let chars: Vec<char> = script.chars().collect();
    let mut rewritten = String::with_capacity(script.len());
    let mut referenced: Vec<usize> = Vec::new();
    let mut mode = Mode::Code;
    let mut depth = 0usize;
    let mut cursor = 0usize;

    while cursor < chars.len() {
        let current = chars[cursor];
        let next = chars.get(cursor + 1).copied();

        match mode {
            Mode::Code => match current {
                '/' if next == Some('/') => {
                    mode = Mode::LineComment;
                    rewritten.push(current);
                }
                '/' if next == Some('*') => {
                    mode = Mode::BlockComment;
                    depth = 1;
                    rewritten.push(current);
                }
                '"' => {
                    mode = Mode::DoubleString;
                    rewritten.push(current);
                }
                '`' => {
                    mode = Mode::TemplateString;
                    rewritten.push(current);
                }
                '\'' => {
                    mode = Mode::CharLiteral;
                    rewritten.push(current);
                }
                '$' if next.is_some_and(|c| c.is_ascii_digit()) => {
                    let start = cursor + 1;
                    let mut end = start;
                    while end < chars.len() && chars[end].is_ascii_digit() {
                        end += 1;
                    }
                    let digits: String = chars[start..end].iter().collect();
                    match digits.parse::<usize>() {
                        Ok(index) => {
                            if !referenced.contains(&index) {
                                referenced.push(index);
                            }
                            rewritten.push_str(&positional_param_name(index));
                        }
                        // An index too large to address any parameter is left for the parser.
                        Err(_) => {
                            rewritten.push(current);
                            rewritten.push_str(&digits);
                        }
                    }
                    cursor = end;
                    continue;
                }
                _ => rewritten.push(current),
            },
            Mode::LineComment => {
                rewritten.push(current);
                if current == '\n' {
                    mode = Mode::Code;
                }
            }
            Mode::BlockComment => {
                rewritten.push(current);
                if current == '/' && next == Some('*') {
                    depth += 1;
                } else if current == '*' && next == Some('/') {
                    depth -= 1;
                    rewritten.push('/');
                    cursor += 2;
                    if depth == 0 {
                        mode = Mode::Code;
                    }
                    continue;
                }
            }
            Mode::DoubleString | Mode::CharLiteral => {
                rewritten.push(current);
                if current == '\\' {
                    if let Some(escaped) = next {
                        rewritten.push(escaped);
                        cursor += 2;
                        continue;
                    }
                } else if (matches!(mode, Mode::DoubleString) && current == '"')
                    || (matches!(mode, Mode::CharLiteral) && current == '\'')
                {
                    mode = Mode::Code;
                }
            }
            Mode::TemplateString => {
                rewritten.push(current);
                if current == '`' {
                    mode = Mode::Code;
                }
            }
        }

        cursor += 1;
    }

    (rewritten, referenced)
}
