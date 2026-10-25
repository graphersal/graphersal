//! The script side of `profile()` and `execute()`: [`TraversalMetrics`] as a Rhai value and
//! [`ScriptExecution`], the Rhai form of an [`Execution`](crate::Execution).
//!
//! Both are pure data in a script (`p.duration_ns`, `p.metrics[0].name`, `r.results`, `r.error`,
//! `r.profile`, `to_map()`, `to_json()`); only the session visualizer turns them into text
//! (`try_render_value`): a metrics value as the `profile()` text table, an execution as its
//! results followed by the table.

use std::collections::HashMap;

use compact_str::CompactString;
use rhai::Dynamic;

use crate::error::{BoxedTraverserError, DiagnosticError, TraverserError};
use crate::graph::schema::VisualizeFormat;
use crate::traversal::render::{RenderOptions, RenderedOutput};
use crate::traversal::source::ListItem;
use crate::value::{ElementProperty, PropertyMap};
use crate::{ExecOptions, ProfileType, StepLocation, TraversalMetrics};

/// The keys `execute(#{...})` accepts.
const EXECUTE_OPTION_KEYS: &str = "profile, profile_types";

/// The error of an [`ScriptExecution`] as data: everything a script, the playground or a binding
/// shows about it, computed once from the [`TraverserError`].
#[derive(Debug, Clone)]
struct ExecutionErrorData {
    /// The error and its root cause in one line, plain data: without the `Error: ` prefix and the
    /// plan frames of the rendered `diagnostic`.
    message: String,
    /// How to fix the query (the error's `help()`), when it has advice.
    help: Option<String>,
    /// The full diagnostic the CLI prints (message, plan frames, cause, help).
    diagnostic: String,
    /// The failing step, numbered like `Metrics.id`.
    location: Option<StepLocation>,
}

impl ExecutionErrorData {
    fn of(err: &TraverserError) -> Self {
        let root = err.root_cause();
        let message = if std::ptr::eq(root, err) {
            err.to_string()
        } else {
            format!("{err}: {root}")
        };
        Self {
            message,
            help: err.help(),
            diagnostic: crate::error::format_diagnostic(err),
            location: err.step_location(),
        }
    }

    /// `#{message, help?, step_id?, step_path?, diagnostic}`; an unknown part is absent.
    fn to_property(&self) -> ElementProperty {
        let mut map = PropertyMap::default();
        map.insert("message", text(&self.message));
        if let Some(help) = &self.help {
            map.insert("help", text(help));
        }
        if let Some(location) = &self.location {
            map.insert("step_id", text(&location.step_id));
            map.insert(
                "step_path",
                ElementProperty::Array(
                    location
                        .step_path
                        .iter()
                        .map(|entry| {
                            let mut step = PropertyMap::default();
                            step.insert("id", text(&entry.id));
                            step.insert("name", text(&entry.name));
                            ElementProperty::Object(Box::new(step))
                        })
                        .collect(),
                ),
            );
        }
        map.insert("diagnostic", text(&self.diagnostic));
        ElementProperty::Object(Box::new(map))
    }
}

fn text(value: &str) -> ElementProperty {
    ElementProperty::String(CompactString::new(value))
}

/// The result of the DSL terminal `execute()` / `execute(#{profile: true, profile_types: [..]})`:
/// the results `to_list()` would return, the error (`()` when the run succeeded) and the profile
/// (`()` unless requested; partial up to the failing step when the run failed).
///
/// `execute()` never throws: every error, also one in its options, is in `error`. Read it with
/// `r.results`, `r.error`, `r.profile`, `r.is_ok`, or as a whole with `to_map()` / `to_json()`.
/// As the final value of a session it renders as its results (in the traversal's visualizer
/// format) followed by the profile table; a failed execution renders as its diagnostic.
#[derive(Clone)]
pub struct ScriptExecution {
    items: Vec<ListItem>,
    error: Option<ExecutionErrorData>,
    profile: Option<TraversalMetrics>,
    /// Whether the run committed a change to the graph.
    mutated: bool,
    /// What the run changed (data, schema).
    changes: crate::graph::UnitChanges,
    /// The `with(..)` options of the traversal that ran, for its visualizer and display limits.
    options: HashMap<CompactString, ElementProperty>,
    /// Who a graph value among the results belongs to (the host or the script).
    origin: super::access::GraphOrigin,
}

impl ScriptExecution {
    pub(crate) fn new(
        items: Vec<ListItem>,
        error: Option<BoxedTraverserError>,
        profile: Option<TraversalMetrics>,
        options: HashMap<CompactString, ElementProperty>,
        origin: super::access::GraphOrigin,
    ) -> Self {
        Self {
            items,
            error: error.as_deref().map(ExecutionErrorData::of),
            profile,
            mutated: false,
            changes: crate::graph::UnitChanges::NONE,
            options,
            origin,
        }
    }

    /// Records whether and what the run changed (see [`crate::Execution::mutated`] and
    /// [`crate::Execution::changes`]).
    pub(crate) fn with_mutated(
        mut self,
        mutated: bool,
        changes: crate::graph::UnitChanges,
    ) -> Self {
        self.mutated = mutated;
        self.changes = changes;
        self
    }

    /// `r.changes`: what the run changed and kept, `#{data: bool, schema: bool, definitions:
    /// bool}` (see [`crate::Execution::changes`]).
    pub fn changes(&mut self) -> Dynamic {
        let mut map = rhai::Map::new();
        map.insert("data".into(), Dynamic::from_bool(self.changes.data));
        map.insert("schema".into(), Dynamic::from_bool(self.changes.schema));
        map.insert(
            "definitions".into(),
            Dynamic::from_bool(self.changes.definitions),
        );
        Dynamic::from_map(map)
    }

    /// `r.mutated`: whether the run changed the graph and kept the change (`false` for a
    /// read-only, failed or vetoed run). In a whole-script unit
    /// ([`eval_value_atomic`](crate::script::eval_value_atomic)) the change is part of the
    /// script's unit, which commits or rolls back at the end; see [`crate::Execution::mutated`].
    pub fn mutated(&mut self) -> bool {
        self.mutated
    }

    /// The results as script values (what `to_list()` returns).
    pub fn results(&mut self) -> rhai::Array {
        self.items
            .iter()
            .cloned()
            .map(|item| super::list_item_to_dynamic(item, &self.origin))
            .collect()
    }

    /// The error as a map `#{message, help, step_id, step_path, diagnostic}`, or `()`.
    pub fn error(&mut self) -> Dynamic {
        self.error
            .as_ref()
            .map(|err| super::element_property_to_dynamic(err.to_property()))
            .unwrap_or(Dynamic::UNIT)
    }

    /// The profile ([`TraversalMetrics`]), or `()` when none was requested.
    pub fn profile(&mut self) -> Dynamic {
        self.profile
            .clone()
            .map(Dynamic::from)
            .unwrap_or(Dynamic::UNIT)
    }

    /// Whether the run succeeded.
    pub fn is_ok(&mut self) -> bool {
        self.error.is_none()
    }

    /// The whole execution as one value tree: `{results, error, profile}` (`null` for an absent
    /// error or profile).
    fn to_property(&self) -> crate::error::TraverserResult<ElementProperty> {
        let mut map = PropertyMap::default();
        map.insert(
            "results",
            ElementProperty::Array(list_items_property(&self.items)?),
        );
        map.insert(
            "error",
            self.error
                .as_ref()
                .map_or(ElementProperty::Null, ExecutionErrorData::to_property),
        );
        map.insert(
            "profile",
            self.profile
                .as_ref()
                .map_or(ElementProperty::Null, TraversalMetrics::to_property),
        );
        Ok(ElementProperty::Object(Box::new(map)))
    }

    /// The results as values, in order: a vertex or an edge stays a
    /// [`ElementProperty::Vertex`]/[`ElementProperty::Edge`], a graph value becomes its listing
    /// (`{vertices, edges}`). For a host that renders them itself (the playground session).
    pub fn result_values(&self) -> crate::error::TraverserResult<Vec<ElementProperty>> {
        list_items_property(&self.items)
    }

    /// `to_map()`: the execution as a script map.
    pub fn to_map(&mut self) -> Dynamic {
        let mut map = rhai::Map::new();
        map.insert("results".into(), Dynamic::from(self.results()));
        map.insert("error".into(), self.error());
        map.insert(
            "profile".into(),
            self.profile
                .as_ref()
                .map(|p| super::element_property_to_dynamic(p.to_property()))
                .unwrap_or(Dynamic::UNIT),
        );
        map.insert("mutated".into(), Dynamic::from_bool(self.mutated));
        map.insert("changes".into(), self.changes());
        Dynamic::from_map(map)
    }

    /// `to_json()`: the execution as compact JSON text.
    pub fn to_json(&mut self) -> Result<String, Box<rhai::EvalAltResult>> {
        let value = self.to_property().map_err(super::script_error)?;
        Ok(crate::value::physical::to_json_text(&value).unwrap_or_default())
    }

    /// The session rendering: the results in the traversal's visualizer format (`fallback`
    /// when it has none) within the display limits, then the profile table. A failed execution
    /// is `Err` holding its diagnostic, followed by the partial profile table when there is one.
    pub(crate) fn render(
        &self,
        fallback: VisualizeFormat,
        base: &RenderOptions,
    ) -> Result<RenderedOutput, String> {
        if let Some(err) = &self.error {
            let mut text = err.diagnostic.clone();
            if let Some(profile) = &self.profile {
                text.push_str("\n\n");
                text.push_str(&profile.to_string());
            }
            return Err(text);
        }
        let mut rendered = self.render_results(fallback, base)?;
        if let Some(profile) = &self.profile {
            if !rendered.text.is_empty() {
                rendered.text.push_str("\n\n");
            }
            rendered.text.push_str(&profile.to_string());
        }
        Ok(rendered)
    }

    /// Renders only the results, as the session visualizer would: in the traversal's visualizer
    /// format (`fallback` when it has none, e.g. [`VisualizeFormat::Json`]), cut to the display
    /// limits (`base`, overridden by the traversal's own `render.max_rows` / `render.max_items`
    /// options). The profile is not part of it. A failed execution is `Err` holding its
    /// diagnostic.
    ///
    /// For hosts that show results and metrics separately (the browser playground).
    pub fn render_results(
        &self,
        fallback: VisualizeFormat,
        base: &RenderOptions,
    ) -> Result<RenderedOutput, String> {
        if let Some(err) = &self.error {
            return Err(err.diagnostic.clone());
        }
        let format = self
            .options
            .get("visualizer")
            .and_then(|value| value.as_str())
            .and_then(VisualizeFormat::from_name)
            .unwrap_or(fallback);
        let options = super::render_options_from(&self.options, base)
            .map_err(|err| crate::error::format_diagnostic(&err))?;
        let mut elements = list_items_property(&self.items)
            .map_err(|err| crate::error::format_diagnostic(&err))?;
        let more_rows = match options.max_rows {
            Some(n) if elements.len() > n => {
                elements.truncate(n);
                true
            }
            _ => false,
        };
        let rows_shown = elements.len();
        crate::traversal::source::render_elements(
            &elements, format, &options, rows_shown, more_rows,
        )
        .map_err(|err| crate::error::format_diagnostic(&err))
    }
}

/// The results as plain values (a graph value becomes its element lists).
fn list_items_property(items: &[ListItem]) -> crate::error::TraverserResult<Vec<ElementProperty>> {
    items
        .iter()
        .map(|item| match item {
            ListItem::Value(value) => Ok(value.clone()),
            ListItem::Graph(graph) => crate::graph::subgraph::graph_to_property(graph),
        })
        .collect()
}

/// Parses the options map of `execute(#{...})`: `profile` (bool) and `profile_types` (a list of
/// `ProfileType` values or their names, e.g. `[ProfileType.Memory]` or `["memory"]`; implies
/// `profile: true`). Unknown keys and malformed values are errors.
pub(crate) fn parse_execute_options(options: Dynamic) -> Result<ExecOptions, BoxedTraverserError> {
    let invalid = |key: &str, reason: String| {
        Box::new(TraverserError::InvalidOption {
            key: format!("execute.{key}"),
            reason,
        })
    };
    let Some(map) = options.read_lock::<rhai::Map>() else {
        return Err(invalid(
            "options",
            format!(
                "expected a map such as #{{profile: true}}, got {}",
                super::type_name_of(&options)
            ),
        ));
    };
    let mut profile: Option<bool> = None;
    let mut types = ProfileType::none();
    let mut types_given = false;
    for (key, value) in map.iter() {
        match key.as_str() {
            "profile" => match value.as_bool() {
                Ok(flag) => profile = Some(flag),
                Err(actual) => {
                    return Err(invalid(
                        "profile",
                        format!(
                            "expected true or false, got {}",
                            super::register::display_type_name(actual)
                        ),
                    ));
                }
            },
            "profile_types" => {
                let Some(list) = value.read_lock::<rhai::Array>() else {
                    return Err(invalid(
                        "profile_types",
                        format!(
                            "expected a list such as [ProfileType.Memory], got {}",
                            super::type_name_of(value)
                        ),
                    ));
                };
                for item in list.iter() {
                    types |= profile_type_of(item).ok_or_else(|| {
                        invalid(
                            "profile_types",
                            format!(
                                "unknown profile type {}; known: ProfileType.Memory (\"memory\")",
                                super::debug_value(item)
                            ),
                        )
                    })?;
                }
                types_given = true;
            }
            other => {
                return Err(invalid(
                    other,
                    format!("unknown option; execute() accepts {EXECUTE_OPTION_KEYS}"),
                ));
            }
        }
    }
    // `profile_types` asks for a profile; `profile: false` with types is a contradiction.
    match (profile, types_given) {
        (Some(false), true) => Err(invalid(
            "profile_types",
            "profile types need profiling: drop `profile: false` or the types".to_string(),
        )),
        (Some(true), _) | (None, true) => Ok(ExecOptions::profile(types)),
        (Some(false), false) | (None, false) => Ok(ExecOptions::new()),
    }
}

/// A `ProfileType` value, or the snake_case name of one (`"memory"`).
fn profile_type_of(value: &Dynamic) -> Option<ProfileType> {
    if let Some(flags) = value.read_lock::<ProfileType>() {
        return Some(*flags);
    }
    let name = value.read_lock::<rhai::ImmutableString>()?;
    match name.as_str() {
        "memory" | "Memory" => Some(ProfileType::Memory),
        _ => None,
    }
}

/// Registers [`TraversalMetrics`] and [`ScriptExecution`] with their getters and conversions.
pub(crate) fn register(registry: &mut super::register::Registry) {
    registry
        .register_type_with_name::<TraversalMetrics>("TraversalMetrics")
        .register_get("duration_ns", |m: &mut TraversalMetrics| {
            i64::try_from(m.duration().as_nanos()).unwrap_or(i64::MAX)
        })
        .register_get("metrics", |m: &mut TraversalMetrics| {
            metrics_field(m, "metrics")
        })
        .register_get("optimizer_rules_applied", |m: &mut TraversalMetrics| {
            m.optimizer_rules_applied()
                .iter()
                .map(|rule| Dynamic::from(rule.clone()))
                .collect::<rhai::Array>()
        })
        .register_fn_as(&["to_map", "toMap"], metrics_to_map)
        .register_fn_as(&["to_json"], |m: &mut TraversalMetrics| m.to_json())
        .register_fn_as(
            &["to_table", "to_string", "to_debug"],
            |m: &mut TraversalMetrics| m.to_string(),
        );

    registry
        .register_type_with_name::<ScriptExecution>("Execution")
        .register_get("results", ScriptExecution::results)
        .register_get("error", ScriptExecution::error)
        .register_get("profile", ScriptExecution::profile)
        .register_get("mutated", ScriptExecution::mutated)
        .register_get("changes", ScriptExecution::changes)
        .register_get("is_ok", ScriptExecution::is_ok)
        .register_get("isOk", ScriptExecution::is_ok)
        .register_fn_as(&["to_map", "toMap"], ScriptExecution::to_map)
        .register_fn_as(&["to_json"], ScriptExecution::to_json)
        .register_fn("to_string", |e: &mut ScriptExecution| {
            e.render(VisualizeFormat::Auto, &RenderOptions::default())
                .map(|rendered| rendered.text)
                .unwrap_or_else(|diagnostic| diagnostic)
        })
        .register_fn("to_debug", |e: &mut ScriptExecution| {
            e.to_json().unwrap_or_else(|err| err.to_string())
        });
}

/// `to_map()` of a [`TraversalMetrics`]: the metrics as a script map (the shape of
/// [`TraversalMetrics::to_property`]). Public for hosts that convert script values further (the
/// Python binding).
pub fn metrics_to_map(metrics: &mut TraversalMetrics) -> Dynamic {
    super::element_property_to_dynamic(metrics.to_property())
}

/// One field of the map form of `metrics` (the getters share the map conversion, so a script
/// sees the same keys through `p.metrics` and `p.to_map().metrics`).
fn metrics_field(metrics: &TraversalMetrics, key: &str) -> Dynamic {
    match metrics.to_property() {
        ElementProperty::Object(map) => map
            .get(key)
            .cloned()
            .map(super::element_property_to_dynamic)
            .unwrap_or(Dynamic::UNIT),
        _ => Dynamic::UNIT,
    }
}
