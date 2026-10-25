//! The terminal steps of `ScriptGraphTraversalSource`: `profile`/`execute`, the visualizers
//! (`to_table()`, `visualize()`, ...), `to_list()`/`next()`/`to_graph()` and the collectors.

use super::plan::{self, TerminalKind};
use super::*;

impl ScriptGraphTraversalSource {
    /// Whether this value is a graph rather than a traversal: a source of a graph without any
    /// step (`g` itself, `g.e(0).to_graph()`, `g.e().subgraph("sg").cap("sg").next()`,
    /// `GraphSource::empty()`). A host shows such a final value as its graph
    /// ([`Self::graph_listing`]); an anonymous `__` is never a graph value.
    pub fn is_graph_value(&self) -> bool {
        !self.anonymous && self.steps.is_empty() && self.g.is_some()
    }

    /// The graph of a graph value ([`Self::is_graph_value`]) as one value,
    /// `{"vertices": [..], "edges": [..]}`, each element materialized like a `g.V()`/`g.E()`
    /// result: the form `cap("sg")` shows for a graph inside a traversal. At most `keep`
    /// vertices and `keep` edges are read (all with `None`), so a display passes its item limit
    /// plus one and never materializes a huge graph whole. Asks `Read` on `Data`.
    pub fn graph_listing(
        &self,
        keep: Option<usize>,
    ) -> crate::error::TraverserResult<ElementProperty> {
        self.restrictions().authorize(
            "graph",
            &crate::graph::AccessRequest::new(
                crate::graph::Action::Read,
                crate::graph::Resource::data(None, None),
            ),
        )?;
        match &self.g {
            Some(graph) => graph.listing(keep),
            None => crate::graph::subgraph::graph_listing(
                &crate::graph::TraversalGraph::default(),
                keep,
            ),
        }
    }

    /// `profile()`: runs the traversal and returns its [`crate::TraversalMetrics`] (TinkerPop's
    /// `profile()`: metrics only, no results). The session visualizer renders it as the text
    /// table of the optimized plan; a script reads it as data (`p.duration_ns`,
    /// `p.metrics[0].name`, `p.to_map()`, `p.to_json()`). A failing traversal is a script error
    /// carrying the full diagnostic; `execute(#{profile: true})` returns the partial profile
    /// together with the error instead.
    pub fn profile(&mut self) -> Result<crate::TraversalMetrics, Box<rhai::EvalAltResult>> {
        self.profile_with(ProfileType::none())
    }

    /// `profile(flags)` / `profile_with(flags)`: [`Self::profile`], additionally collecting the
    /// categories named by `flags` (currently only `ProfileType::Memory`, a `memory` section per
    /// step). Registered under the shared `"profile"` name (one overload per arity) and under the
    /// Rust API's `"profile_with"` name.
    pub fn profile_with(
        &mut self,
        flags: ProfileType,
    ) -> Result<crate::TraversalMetrics, Box<rhai::EvalAltResult>> {
        self.run(TerminalKind::Profile(flags))
            .into_metrics()
            .map_err(script_error)
    }

    /// Runs the plan with `terminal` on this source's graph ([`AnyGraph::run_plan`]: the write
    /// lock for a mutating plan, else the read lock), or on a scratch graph when the source has
    /// none ([`plan::scratch_graph`]).
    fn run(&self, terminal: TerminalKind) -> plan::TerminalOutput {
        let plan = plan::ScriptPlan {
            source: self,
            mutating: self.is_mutating(),
        };
        let terminal = plan::Terminal(terminal);
        match &self.g {
            Some(graph) => graph.run_plan(&plan, terminal),
            None => plan::scratch_graph(plan.mutating).run_plan(&plan, terminal),
        }
    }

    /// `execute()`: runs the traversal and returns an `Execution` (`results`, `error`, `profile`)
    /// instead of throwing; see [`ScriptExecution`].
    pub fn execute(&mut self) -> ScriptExecution {
        self.execute_with_options(crate::ExecOptions::new())
    }

    /// `execute(#{profile: true, profile_types: [ProfileType.Memory]})`: [`Self::execute`] with
    /// options. A malformed options map does not throw either: it is the execution's error.
    pub fn execute_dynamic(&mut self, options: Dynamic) -> ScriptExecution {
        match execution::parse_execute_options(options) {
            Ok(options) => self.execute_with_options(options),
            Err(err) => ScriptExecution::new(
                Vec::new(),
                Some(err),
                None,
                self.options.clone(),
                self.graph_origin(),
            ),
        }
    }

    fn execute_with_options(&mut self, options: crate::ExecOptions) -> ScriptExecution {
        let run = self.run(TerminalKind::Execute(options)).into_executed();
        ScriptExecution::new(
            run.items,
            run.error,
            run.profile,
            self.options.clone(),
            self.graph_origin(),
        )
        .with_mutated(run.mutated, run.changes)
    }

    /// The session display limits of this traversal: `base`, overridden by the
    /// [`RENDER_MAX_ROWS`] / [`RENDER_MAX_ITEMS`] options the traversal carries (set on the scope
    /// by [`graph_scope_with_options`], or per query with `g.with("render.max_rows", n)`).
    ///
    /// Fails with [`crate::error::TraverserError::InvalidOption`] when such an option is not a
    /// positive integer.
    pub fn session_render_options(
        &self,
        base: &RenderOptions,
    ) -> crate::error::TraverserResult<RenderOptions> {
        render_options_from(&self.options, base)
    }

    /// Renders the traversal in `format` with the session display limits.
    pub fn visualize(&mut self, format: VisualizeFormat) -> RenderedText {
        self.display(format, None)
            .unwrap_or_else(RenderedText::diagnostic)
    }

    /// Renders the traversal and hands a failed execution back as an error instead of rendering
    /// the diagnostic as the output text, so a script host can tell a result from a failure (e.g.
    /// for its exit code). `options` are used as given; the session limits are not consulted.
    pub fn try_visualize_with_options(
        &mut self,
        format: VisualizeFormat,
        options: &RenderOptions,
    ) -> crate::error::TraverserResult<RenderedOutput> {
        // A nested child traversal can mutate too: then the whole stream must be produced.
        let may_stop_early = !self.is_mutating();
        self.run(TerminalKind::Visualize {
            format,
            options: options.clone(),
            may_stop_early,
        })
        .into_rendered()
    }

    /// An explicit visualizer call (`to_table()`, `to_json(#{..})`, `visualize(fmt, #{..})`, ...).
    ///
    /// The session limits apply, replaced by a per-call options map when one is given. A failed
    /// execution renders as its diagnostic text (`Err` holds that text); malformed per-call options
    /// are a script error. The returned [`RenderedText`] carries its own truncation report, so the
    /// host that prints it as a script's final value can show the truncation notice
    /// ([`try_render_value`]) without any side channel.
    fn display(
        &mut self,
        format: VisualizeFormat,
        call_options: Option<&Dynamic>,
    ) -> Result<RenderedText, ImmutableString> {
        let session = self
            .session_render_options(&RenderOptions::default())
            .map_err(|err| ImmutableString::from(crate::error::format_diagnostic(&err)))?;
        let options = match call_options {
            Some(arg) => dynamic_to_render_options(arg, &session)
                .map_err(|err| ImmutableString::from(crate::error::format_diagnostic(&err)))?,
            None => session,
        };
        match self.try_visualize_with_options(format, &options) {
            Ok(rendered) => Ok(RenderedText::new(rendered)),
            Err(err) => Err(crate::error::format_diagnostic(&err).into()),
        }
    }

    /// [`Self::display`] with a per-call options map, whose malformed values fail the script.
    fn display_with(
        &mut self,
        format: VisualizeFormat,
        arg: Dynamic,
    ) -> Result<RenderedText, Box<rhai::EvalAltResult>> {
        let session = self.session_render_options(&RenderOptions::default());
        if let Ok(session) = &session {
            // Validate the per-call map first, so that it fails the script instead of rendering.
            dynamic_to_render_options(&arg, session).map_err(|err| {
                Box::new(rhai::EvalAltResult::ErrorRuntime(
                    Dynamic::from(crate::error::format_diagnostic(&err)),
                    rhai::Position::NONE,
                ))
            })?;
        }
        Ok(self
            .display(format, Some(&arg))
            .unwrap_or_else(RenderedText::diagnostic))
    }

    /// Renders the traversal with the configured visualizer, falling back to `fallback` when
    /// the pipeline does not override it.
    pub fn visualize_default_with(&mut self, fallback: VisualizeFormat) -> RenderedText {
        let format = self.configured_visualizer().unwrap_or(fallback);
        self.visualize(format)
    }

    /// Renders the traversal with the configured visualizer, or [`VisualizeFormat::Auto`].
    ///
    /// This is the terminal step applied implicitly when a script ends on a traversal that was
    /// never terminated explicitly.
    pub fn visualize_default(&mut self) -> RenderedText {
        self.visualize_default_with(VisualizeFormat::Auto)
    }

    /// `visualize(fmt, #{..})`: renders in `format` with per-call options (table layout and the
    /// display limits `max_rows`, `max_items`, `no_limit`).
    pub fn visualize_dynamic(
        &mut self,
        format: VisualizeFormat,
        arg: Dynamic,
    ) -> Result<RenderedText, Box<rhai::EvalAltResult>> {
        self.display_with(format, arg)
    }

    /// `to_table()`: renders the traversal with the session display limits.
    pub fn to_table(&mut self) -> RenderedText {
        self.visualize(VisualizeFormat::Table)
    }

    /// `to_table(#{..})`: [`Self::to_table`] with a per-call options map (table layout and the display
    /// limits `max_rows`, `max_items`, `no_limit`), which replaces the session default.
    pub fn to_table_dynamic(
        &mut self,
        arg: Dynamic,
    ) -> Result<RenderedText, Box<rhai::EvalAltResult>> {
        self.display_with(VisualizeFormat::Table, arg)
    }

    /// `to_markdown()`: renders the traversal with the session display limits.
    pub fn to_markdown(&mut self) -> RenderedText {
        self.visualize(VisualizeFormat::Markdown)
    }

    /// `to_markdown(#{..})`: [`Self::to_markdown`] with a per-call options map (table layout and the display
    /// limits `max_rows`, `max_items`, `no_limit`), which replaces the session default.
    pub fn to_markdown_dynamic(
        &mut self,
        arg: Dynamic,
    ) -> Result<RenderedText, Box<rhai::EvalAltResult>> {
        self.display_with(VisualizeFormat::Markdown, arg)
    }

    /// `to_mermaid()`: renders the traversal with the session display limits.
    pub fn to_mermaid(&mut self) -> RenderedText {
        self.visualize(VisualizeFormat::Mermaid)
    }

    /// `to_mermaid(#{..})`: [`Self::to_mermaid`] with a per-call options map (table layout and the display
    /// limits `max_rows`, `max_items`, `no_limit`), which replaces the session default.
    pub fn to_mermaid_dynamic(
        &mut self,
        arg: Dynamic,
    ) -> Result<RenderedText, Box<rhai::EvalAltResult>> {
        self.display_with(VisualizeFormat::Mermaid, arg)
    }

    /// `to_plantuml()`: renders the traversal with the session display limits.
    pub fn to_plantuml(&mut self) -> RenderedText {
        self.visualize(VisualizeFormat::PlantUML)
    }

    /// `to_plantuml(#{..})`: [`Self::to_plantuml`] with a per-call options map (table layout and the display
    /// limits `max_rows`, `max_items`, `no_limit`), which replaces the session default.
    pub fn to_plantuml_dynamic(
        &mut self,
        arg: Dynamic,
    ) -> Result<RenderedText, Box<rhai::EvalAltResult>> {
        self.display_with(VisualizeFormat::PlantUML, arg)
    }

    /// `to_json_schema()`: renders the traversal with the session display limits.
    pub fn to_json_schema(&mut self) -> RenderedText {
        self.visualize(VisualizeFormat::JsonSchema)
    }

    /// `to_json_schema(#{..})`: [`Self::to_json_schema`] with a per-call options map (table layout and the display
    /// limits `max_rows`, `max_items`, `no_limit`), which replaces the session default.
    pub fn to_json_schema_dynamic(
        &mut self,
        arg: Dynamic,
    ) -> Result<RenderedText, Box<rhai::EvalAltResult>> {
        self.display_with(VisualizeFormat::JsonSchema, arg)
    }

    /// `to_json()`: renders the traversal with the session display limits.
    pub fn to_json(&mut self) -> RenderedText {
        self.visualize(VisualizeFormat::Json)
    }

    /// `to_json(#{..})`: [`Self::to_json`] with a per-call options map (table layout and the display
    /// limits `max_rows`, `max_items`, `no_limit`), which replaces the session default.
    pub fn to_json_dynamic(
        &mut self,
        arg: Dynamic,
    ) -> Result<RenderedText, Box<rhai::EvalAltResult>> {
        self.display_with(VisualizeFormat::Json, arg)
    }

    /// `to_tree()`: renders the traversal with the session display limits.
    pub fn to_tree(&mut self) -> RenderedText {
        self.visualize(VisualizeFormat::Tree)
    }

    /// `to_tree(#{..})`: [`Self::to_tree`] with a per-call options map (table layout and the display
    /// limits `max_rows`, `max_items`, `no_limit`), which replaces the session default.
    pub fn to_tree_dynamic(
        &mut self,
        arg: Dynamic,
    ) -> Result<RenderedText, Box<rhai::EvalAltResult>> {
        self.display_with(VisualizeFormat::Tree, arg)
    }

    /// Materializes the traversal result as a native Rhai array.
    ///
    /// Every stream item is converted into the closest native script value: scalars become
    /// Rhai primitives, arrays become Rhai arrays, and vertices, edges and projections become
    /// object maps. Results can therefore be bound to a variable and processed further
    /// (`let people = g.V().hasLabel("person").toList(); people[0].name`) instead of being
    /// handed back as pre-rendered text. Rendering is the job of [`Self::visualize`] and its
    /// format shortcuts.
    ///
    /// Traversal failures are reported as script errors carrying the full diagnostic, so a
    /// broken query never masquerades as data.
    pub fn to_list(&mut self) -> Result<rhai::Array, Box<rhai::EvalAltResult>> {
        let items = self.collect_items().map_err(script_error)?;
        let origin = self.graph_origin();
        Ok(items
            .into_iter()
            .map(|item| list_item_to_dynamic(item, &origin))
            .collect())
    }

    /// Materializes the first traversal result as a native Rhai value.
    ///
    /// Returns `()` when the traversal yields nothing, mirroring [`Self::to_list`] conversion
    /// rules for the value itself.
    // This is the Gremlin/TinkerPop `next()` terminal step, registered with Rhai under that exact
    // name for DSL parity (see CLAUDE.md). It is not meant to implement `std::iter::Iterator`.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Dynamic, Box<rhai::EvalAltResult>> {
        let items = self.collect_items().map_err(script_error)?;
        let origin = self.graph_origin();
        Ok(items
            .into_iter()
            .next()
            .map(|item| list_item_to_dynamic(item, &origin))
            .unwrap_or(Dynamic::UNIT))
    }

    /// `iterate()`: runs the traversal for its side effects and returns `()` (the TinkerPop
    /// terminal of the same name). The results are discarded, never materialized; the run is one
    /// unit like every terminal (a failure rolls back and is a script error). See
    /// [`GraphTraversalSource::iterate`].
    pub fn iterate(&mut self) -> Result<(), Box<rhai::EvalAltResult>> {
        self.run(TerminalKind::Iterate)
            .into_unit()
            .map_err(script_error)
    }

    /// `to_graph()`: the graph built from the traversal's edges, usable as a new
    /// traversal source (`let sub = g.e(0).to_graph(); sub.v().to_json()`). Our extension: the
    /// short form of `subgraph("sg").cap("sg")`. A stream of anything but edges is an error; a
    /// `cap("sg")` of a `subgraph("sg")` is returned as is. See [`GraphTraversalSource::to_graph`].
    pub fn to_graph(&mut self) -> Result<ScriptGraphTraversalSource, Box<rhai::EvalAltResult>> {
        let graph = self.run(TerminalKind::ToGraph).into_graph();
        let origin = self.graph_origin();
        graph
            .map(|graph| origin.source(graph))
            .map_err(script_error)
    }

    /// [`Self::collect_elements`], keeping a graph value as a shared handle.
    fn collect_items(
        &mut self,
    ) -> crate::error::TraverserResult<Vec<crate::traversal::source::ListItem>> {
        self.run(TerminalKind::ListWithGraphs).into_items()
    }

    /// Executes the pipeline and returns the stream values as typed [`ElementProperty`] values.
    ///
    /// This is the typed counterpart of [`Self::to_list`] for Rust hosts (test harnesses,
    /// drivers) that evaluate a script without a terminal step and need the result with its
    /// element identity intact: a vertex stays an [`ElementProperty::Vertex`] and an edge an
    /// [`ElementProperty::Edge`] instead of becoming a plain object map. It is not a traversal
    /// step and is deliberately not registered with Rhai.
    ///
    /// ```rust
    /// # use std::sync::Arc;
    /// # use graphersal::prelude::*;
    /// use graphersal::script::{engine_with_limits, graph_scope_with_options};
    /// use graphersal::script::{ScriptGraphTraversalSource, ScriptLimits};
    /// use graphersal::auth::{AccessPolicy, Authorizer};
    ///
    /// let read_only: Arc<dyn Authorizer> = Arc::new(AccessPolicy::read_only());
    /// let graph = Arc::new(GraphSource::tinkerpop_modern());
    /// let mut scope = graph_scope_with_options(graph, &[], read_only.clone());
    /// let value = engine_with_limits(&ScriptLimits::default(), read_only)
    ///     .eval_with_scope::<graphersal::script::rhai::Dynamic>(&mut scope, r#"g.V("1")"#)
    ///     .unwrap();
    /// let mut traversal = value.cast::<ScriptGraphTraversalSource>();
    /// let values = traversal.to_typed_list().unwrap();
    /// assert!(matches!(values.as_slice(), [ElementProperty::Vertex(_)]));
    /// ```
    pub fn to_typed_list(&mut self) -> crate::error::TraverserResult<Vec<ElementProperty>> {
        self.collect_elements()
    }

    /// Executes the pipeline against the bound graph and materializes the stream values.
    fn collect_elements(&mut self) -> crate::error::TraverserResult<Vec<ElementProperty>> {
        self.run(TerminalKind::ToList).into_list()
    }
}
