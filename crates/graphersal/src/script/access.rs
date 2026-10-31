//! What a script may do: [`ScriptAccess`] (the host's [`Authorizer`] and preset options), the
//! private-graph relaxation, [`GraphOrigin`] (whether a graph value a traversal returns belongs
//! to the host), the engine tag that carries the access to the `GraphSource::x()`
//! constructors, and the module resolver behind Rhai's `import`.
//!
//! The private-graph rule maps onto the request model like this: a graph the script built
//! itself (`GraphSource::empty()`, `__`, `_g`, a `subgraph()` result, ...) is not the host's,
//! so a request that writes data or the schema of it ([`AccessRequest::writes_graph`]:
//! `Create`/`Update`/`Delete` on `Data` or `Schema`) is allowed without asking the host's
//! authorizer. Reads, files and options of it are still asked. A plan that writes a
//! `subgraph()` into a host graph (`withSideEffect(name, g)`) gets no relaxation and asks for
//! `Create` on `Data` for it.

use super::*;
use crate::graph::{AccessRequest, Action, AllowAll, Authorizer, Resource};
use crate::traversal::permission_check::Restrictions;
use std::collections::HashMap;

/// The access of one script session: the host's authorizer and the options it preset on the
/// session's traversal sources (trusted, never checked as the query's own `g.with()`).
#[derive(Clone)]
pub(crate) struct ScriptAccess {
    pub(crate) authorizer: Arc<dyn Authorizer>,
    pub(crate) presets: Arc<HashMap<CompactString, ElementProperty>>,
    /// The engine's [`ScriptLimits::max_value_depth`] (only on the engine tag, [`install`]);
    /// `0` reads as the default ([`value_depth_of`]).
    pub(crate) max_value_depth: usize,
    /// Whether this is the access of a saved query's body (or of a value it returned): the
    /// authorizer is the read-only [`saved_query::ReadOnly`] wrapper, so nothing built from it
    /// can write the host graph.
    pub(crate) read_only: bool,
    /// How many saved-query calls are active around this access (0 outside a query); bounded
    /// by [`saved_query::MAX_QUERY_DEPTH`].
    pub(crate) query_depth: usize,
    /// The host graph of the engine ([`set_engine_graph`]): the global `embed(text)` embeds
    /// with its models when no lock guard of a graph attached them to the thread. Weak: the
    /// engine never keeps a graph alive.
    pub(crate) graph: Option<std::sync::Weak<dyn crate::graph::AnyGraph>>,
}

impl Default for ScriptAccess {
    /// Everything allowed ([`AllowAll`]), no presets.
    fn default() -> Self {
        Self {
            authorizer: Arc::new(AllowAll),
            presets: Arc::default(),
            max_value_depth: 0,
            read_only: false,
            query_depth: 0,
            graph: None,
        }
    }
}

impl ScriptAccess {
    /// The access of a session with `authorizer` and preset `options`.
    pub(crate) fn new(
        authorizer: Arc<dyn Authorizer>,
        options: &[(&str, ElementProperty)],
    ) -> Self {
        Self {
            authorizer,
            max_value_depth: 0,
            read_only: false,
            query_depth: 0,
            graph: None,
            presets: Arc::new(
                options
                    .iter()
                    .map(|(key, value)| (CompactString::new(key), value.clone()))
                    .collect(),
            ),
        }
    }

    /// The access the engine of `context` carries ([`install`]); everything allowed when the
    /// engine has none.
    pub(crate) fn of_context(context: &NativeCallContext) -> Arc<ScriptAccess> {
        context
            .tag()
            .and_then(|tag| {
                tag.read_lock::<EngineAccess>()
                    .map(|access| access.0.clone())
            })
            .unwrap_or_default()
    }

    /// The embedders the global `embed(text)` calls: those of the graph whose lock guard this
    /// thread holds, else those of the engine's host graph ([`set_engine_graph`]), else the
    /// default registry (`hash-ngram/1`).
    pub(crate) fn embedders(&self) -> Arc<crate::value::Embedders> {
        if let Some(attached) = crate::value::vector::embed::registry::attached() {
            return attached;
        }
        match self.graph.as_ref().and_then(std::sync::Weak::upgrade) {
            Some(graph) => graph.model_embedders(),
            None => crate::value::Embedders::default_registry(),
        }
    }

    /// The restriction of a source with this access (`private`: the relaxation applies).
    pub(crate) fn restrictions(&self, private_graph: bool) -> Restrictions {
        Restrictions {
            authorizer: self.authorizer.clone(),
            private_graph,
            preset_options: self.presets.clone(),
            required: Vec::new(),
            journal: None,
        }
    }
}

/// The value-depth limit of the engine running `context` ([`ScriptLimits::max_value_depth`],
/// installed by [`install`]); the default for an engine without one.
pub(crate) fn value_depth_of(context: &NativeCallContext) -> usize {
    context
        .tag()
        .and_then(|tag| {
            tag.read_lock::<EngineAccess>()
                .map(|access| access.0.max_value_depth)
        })
        .filter(|depth| *depth > 0)
        .unwrap_or(crate::value::depth::DEFAULT_MAX_VALUE_DEPTH)
}

/// The engine tag value carrying a session's access (`Engine::set_default_tag`): read by the
/// functions that create a traversal source without one (`GraphSource::empty()`,
/// `GraphSource::file(path)`, `schema.to_graph()`, ...), and the clock of `now()`
/// ([`ScriptClock`]). A script cannot read or change the tag.
#[derive(Clone)]
pub(crate) struct EngineAccess(pub(crate) Arc<ScriptAccess>, pub(crate) ScriptClock);

/// Where `now()` reads the time: the system clock, or a time the host pinned
/// ([`crate::script::pin_now`]).
#[derive(Clone, Copy, Debug)]
pub(crate) enum ClockSource {
    /// The system clock (UTC, through `web_time`).
    System,
    /// A fixed time, for deterministic hosts and tests.
    Pinned(crate::value::temporal::DateTime),
}

impl ClockSource {
    /// The current time of this source, in UTC (a pinned time keeps its offset).
    fn read(self) -> crate::value::temporal::DateTime {
        match self {
            ClockSource::Pinned(now) => now,
            // A system clock outside the years -9999 to 9999 reads as the epoch; the digits are
            // those the platform clock gives (nanoseconds at most).
            ClockSource::System => crate::graph::schema::defaults::system_now(),
        }
    }
}

/// The time `now()` returns in one script evaluation: read from its [`ClockSource`] when the
/// evaluation starts. Rhai clones the engine's default tag into the state of every evaluation
/// (`eval*`, `run*`, `call_fn*`), and this `Clone` reads the clock, so every `now()` of one
/// evaluation returns the same value, its start time (PostgreSQL `now()`: the start of the
/// transaction), and the next evaluation a new one. A saved query's body runs with the caller's
/// time (`saved_query.rs` pins it).
#[derive(Debug)]
pub(crate) struct ScriptClock {
    pub(crate) source: ClockSource,
    pub(crate) now: crate::value::temporal::DateTime,
}

impl ScriptClock {
    /// A clock of `source`, read now.
    pub(crate) fn new(source: ClockSource) -> Self {
        ScriptClock {
            source,
            now: source.read(),
        }
    }

    /// A clock that keeps this one's time (the caller's `now()` for a saved query's body).
    pub(crate) fn same_time(&self) -> Self {
        ScriptClock::new(ClockSource::Pinned(self.now))
    }
}

impl Clone for ScriptClock {
    /// Reads the clock again: see the type's documentation.
    fn clone(&self) -> Self {
        ScriptClock::new(self.source)
    }
}

/// The time `now()` returns in the evaluation of `context` (the system clock when the engine
/// carries no access).
pub(crate) fn now_of(context: &NativeCallContext) -> crate::value::temporal::DateTime {
    context
        .tag()
        .and_then(|tag| tag.read_lock::<EngineAccess>().map(|access| access.1.now))
        .unwrap_or_else(|| ClockSource::System.read())
}

/// The engine tag for a saved query's body called from `context`: the caller's access as
/// given, the caller's `now()`.
pub(crate) fn nested_tag(context: &NativeCallContext, access: Arc<ScriptAccess>) -> EngineAccess {
    let clock = context
        .tag()
        .and_then(|tag| tag.read_lock::<EngineAccess>().map(|a| a.1.same_time()))
        .unwrap_or_else(|| ScriptClock::new(ClockSource::System));
    EngineAccess(access, clock)
}

/// Pins (or, with `None`, unpins) the clock of `now()` and `today()` on an engine built by
/// [`engine_with_options`](crate::script::engine_with_options) and its siblings: every
/// evaluation then returns `now` (the host's deterministic tests, a replay). Without a pin
/// `now()` is the system time at the start of each evaluation.
pub fn pin_now(engine: &mut Engine, now: Option<crate::value::temporal::DateTime>) {
    let source = match now {
        Some(now) => ClockSource::Pinned(now),
        None => ClockSource::System,
    };
    let access = engine
        .default_tag()
        .read_lock::<EngineAccess>()
        .map(|access| access.0.clone())
        .unwrap_or_default();
    engine.set_default_tag(Dynamic::from(EngineAccess(
        access,
        ScriptClock::new(source),
    )));
}

/// Makes `graph` the host graph of `engine` (built by
/// [`engine_with_options`](crate::script::engine_with_options) or a sibling): the global DSL
/// function `embed(text)` then embeds with the embedding models of `graph`'s database (book page
/// "Embedding Models in a Store"; `g.embedders()` lists them) also outside a traversal. The
/// `eval*` entry points that take a graph do this themselves; a host that builds its own engine
/// and scope ([`graph_scope_with_options`](crate::script::graph_scope_with_options)) calls it
/// with the scope's graph, and again when it serves another graph. The engine keeps a weak
/// reference only. Without a host graph `embed(text)` has the built-in `hash-ngram/1`.
///
/// ```rust
/// use std::sync::Arc;
/// use graphersal::prelude::*;
///
/// let graph = Arc::new(GraphSource::tinkerpop_modern());
/// let mut engine = graphersal::script::engine();
/// graphersal::script::set_engine_graph(&mut engine, graph.clone());
/// let mut scope = graphersal::script::graph_scope(graph);
/// let dimension = engine
///     .eval_with_scope::<graphersal::script::rhai::INT>(&mut scope, r#"embed("marko").dimension"#)
///     .unwrap();
/// assert_eq!(dimension, 256);
/// ```
pub fn set_engine_graph(engine: &mut Engine, graph: impl crate::graph::IntoAnyGraph) {
    let graph: Arc<dyn crate::graph::AnyGraph> = graph.into_any_graph();
    let (access, clock) = engine
        .default_tag()
        .read_lock::<EngineAccess>()
        .map(|tag| ((*tag.0).clone(), tag.1.clone()))
        .unwrap_or_else(|| {
            (
                ScriptAccess::default(),
                ScriptClock::new(ClockSource::System),
            )
        });
    let access = ScriptAccess {
        graph: Some(Arc::downgrade(&graph)),
        ..access
    };
    engine.set_default_tag(Dynamic::from(EngineAccess(Arc::new(access), clock)));
}

/// Installs `authorizer` on `engine`: the tag for the source constructors (also carrying the
/// host's preset `options` for the sources they create and the value-depth limit
/// `max_value_depth` for the value conversions, [`value_depth_of`]) and the module resolver of
/// `import` ([`FileImports`]: `Execute` on the module's `File`, resolved by the authorizer).
pub(crate) fn install(
    engine: &mut Engine,
    authorizer: Arc<dyn Authorizer>,
    options: &[(&str, ElementProperty)],
    max_value_depth: usize,
) {
    let mut access = ScriptAccess::new(authorizer.clone(), options);
    access.max_value_depth = max_value_depth;
    engine.set_default_tag(Dynamic::from(EngineAccess(
        Arc::new(access),
        ScriptClock::new(ClockSource::System),
    )));
    engine.set_module_resolver(FileImports { authorizer });
}

/// The resolver of Rhai's `import "module"`: asks the authorizer for `Execute` on the module's
/// `File` (the path as written, with the `.rhai` extension), resolves it
/// ([`Authorizer::resolve_file`], e.g. under a file root) and loads it with Rhai's file
/// resolver. On wasm32-unknown-unknown (no file system) an allowed import finds no module.
pub(crate) struct FileImports {
    authorizer: Arc<dyn Authorizer>,
}

impl rhai::ModuleResolver for FileImports {
    fn resolve(
        &self,
        engine: &Engine,
        source: Option<&str>,
        path: &str,
        pos: rhai::Position,
    ) -> Result<rhai::Shared<Module>, Box<EvalAltResult>> {
        let file = std::path::Path::new(path).with_extension("rhai");
        let restrictions = Restrictions {
            authorizer: self.authorizer.clone(),
            ..Restrictions::default()
        };
        let resolved = restrictions
            .authorize_file("import", Action::Execute, &file)
            .map_err(|err| {
                Box::new(EvalAltResult::ErrorRuntime(
                    Dynamic::from(crate::error::format_diagnostic(&err)),
                    pos,
                ))
            })?;
        #[cfg(any(not(target_family = "wasm"), not(target_os = "unknown")))]
        {
            let resolver = rhai::module_resolvers::FileModuleResolver::new();
            if resolved == file {
                resolver.resolve(engine, source, path, pos)
            } else {
                resolver.resolve(engine, source, &resolved.to_string_lossy(), pos)
            }
        }
        #[cfg(all(target_family = "wasm", target_os = "unknown"))]
        {
            let _ = (engine, source, resolved);
            Err(Box::new(EvalAltResult::ErrorModuleNotFound(
                path.to_string(),
                pos,
            )))
        }
    }
}

/// Where the graph values a traversal returns (`cap("sg")`, `to_graph()`, ...) come from: a
/// graph is the host's when it is one of the traversal's host graphs (its own graph when that
/// is the host's, or an explicit `withSideEffect(name, g)` target of the host); every other
/// graph was built by the traversal and is private to the script.
#[derive(Clone, Default)]
pub(crate) struct GraphOrigin {
    access: Arc<ScriptAccess>,
    host_graphs: Vec<Arc<dyn AnyGraph>>,
}

impl GraphOrigin {
    /// The traversal source a returned `graph` becomes.
    pub(crate) fn source(&self, graph: Arc<dyn AnyGraph>) -> ScriptGraphTraversalSource {
        let private = !self
            .host_graphs
            .iter()
            .any(|host| super::plan::same_graph(host, &graph));
        ScriptGraphSource::graph(graph).with_access(self.access.clone(), private)
    }
}

impl ScriptGraphTraversalSource {
    /// This source with the access `access`; `private` says whether its graph is one the
    /// script built itself.
    pub(crate) fn with_access(mut self, access: Arc<ScriptAccess>, private: bool) -> Self {
        self.access = access;
        self.private = private;
        self
    }

    /// Whether the plan writes a `subgraph()` into a host graph (`withSideEffect(name, g)`).
    fn writes_into_host_graph(&self) -> bool {
        self.steps.iter().any(|step| {
            matches!(step, Step::Source(SourceStep::SideEffectGraph(_, target)) if !target.private)
        })
    }

    /// The restriction every execution of this source runs under: the host's authorizer, with
    /// the private-graph relaxation on a graph the script built itself (unless the plan also
    /// writes into a host graph, which then asks for `Create` on `Data`).
    pub(crate) fn restrictions(&self) -> Restrictions {
        let into_host = self.writes_into_host_graph();
        let mut restrictions = self.access.restrictions(self.private && !into_host);
        // A denial on the host graph is recorded in its system journal.
        if !self.private {
            restrictions.journal = self.g.as_ref().and_then(|g| g.system_journal().ok());
        }
        if into_host {
            restrictions.required.push((
                "with_side_effect",
                AccessRequest::new(Action::Create, Resource::data(None, None)),
            ));
        }
        restrictions
    }

    /// Fails with `PermissionDenied` (as a script error) unless this source may make `request`
    /// for `operation` (a source method that is not a step: `export_graphml`, `statistics`).
    pub(crate) fn require(
        &self,
        operation: &str,
        request: AccessRequest<'_>,
    ) -> Result<(), Box<EvalAltResult>> {
        self.restrictions()
            .authorize(operation, &request)
            .map_err(script_error)
    }

    /// The file `operation` may `action` for `path` (asks for `action` on the `File`, then
    /// resolves it, e.g. under the host's file root): the import/export methods and a backup
    /// job's directory take a path; a schema is never read from a file.
    pub(crate) fn require_file(
        &self,
        operation: &str,
        action: Action<'_>,
        path: &str,
    ) -> Result<std::path::PathBuf, Box<EvalAltResult>> {
        self.restrictions()
            .authorize_file(operation, action, std::path::Path::new(path))
            .map_err(script_error)
    }

    /// Who the graph values this source's traversal returns belong to.
    pub(crate) fn graph_origin(&self) -> GraphOrigin {
        let mut host_graphs: Vec<Arc<dyn AnyGraph>> = self
            .steps
            .iter()
            .filter_map(|step| match step {
                Step::Source(SourceStep::SideEffectGraph(_, target)) if !target.private => {
                    Some(target.graph.clone() as Arc<dyn AnyGraph>)
                }
                _ => None,
            })
            .collect();
        if !self.private
            && let Some(graph) = &self.g
        {
            host_graphs.push(graph.clone());
        }
        GraphOrigin {
            access: self.access.clone(),
            host_graphs,
        }
    }
}
