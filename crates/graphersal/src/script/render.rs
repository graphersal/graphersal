//! Rendering of script results: `render_value`/`try_render_value`/`debug_value`, the session
//! display limits (`render.max_rows`/`render.max_items`) and `RenderedText`.

use super::*;

/// Renders an evaluated script value as terminal output.
///
/// A traversal that was never terminated is rendered with its configured visualizer (falling
/// back to `fallback`), a schema is visualized in the same format, and plain script values are
/// printed one per line.
///
/// `options` are the display limits of the host (`graphersal`, the playground); a traversal's own
/// session options ([`RENDER_MAX_ROWS`], [`RENDER_MAX_ITEMS`]) take precedence over them. A final
/// array or map shows its first `max_rows` items with nested collections cut to `max_items`; a
/// string returned by a cut explicit visualizer (`g.v().to_table()`) carries that cut's report. A
/// schema is never bounded. [`RenderOptions::unlimited`] renders everything.
pub fn render_value(
    value: Dynamic,
    fallback: VisualizeFormat,
    options: &RenderOptions,
) -> RenderedOutput {
    try_render_value(value, fallback, options).unwrap_or_else(RenderedOutput::complete)
}

/// The schema a script value holds: a map in the schema format whose `$schema` is the format
/// URL (what `g.get_schema()` and `g.infer_schema()` return), loaded; `None` for any other value.
/// Hosts use it to show a script's schema result as a schema (the session visualizer does).
pub fn schema_of_value(value: &Dynamic) -> Option<GraphSchema> {
    if !super::dynamic::is_schema_map(value) {
        return None;
    }
    let property = super::dynamic::script_value_to_property(
        value.clone(),
        crate::value::depth::DEFAULT_MAX_VALUE_DEPTH,
    )
    .ok()?;
    GraphSchema::from_value(&property).ok()
}

/// Like [`render_value`], but a traversal whose execution fails comes back as `Err` holding the
/// rendered diagnostic instead of as the output text, so a script host can report it as a
/// failure (for example on stderr, with a non-zero exit code).
pub fn try_render_value(
    value: Dynamic,
    fallback: VisualizeFormat,
    options: &RenderOptions,
) -> Result<RenderedOutput, String> {
    let value = match value.try_cast_result::<ScriptGraphTraversalSource>() {
        // An anonymous traversal (`__.out()`) is a template for a child position, not a query
        // of the session graph: it shows its DSL text.
        Ok(traversal) if traversal.anonymous => {
            let text = crate::value::literal::TokenLiteral::token_literal(&traversal);
            return Ok(RenderedOutput::complete(
                crate::traversal::spelling::respell(&text, options.spelling).into_owned(),
            ));
        }
        // A graph value (`g.e(0).to_graph()`, `cap("sg").next()`, `g`): its vertices and
        // edges, rendered like `cap("sg")` renders a graph inside a traversal, within the item
        // limit (only `max_items + 1` elements of each kind are read).
        Ok(traversal) if traversal.is_graph_value() => {
            let format = traversal.configured_visualizer().unwrap_or(fallback);
            let options = traversal
                .session_render_options(options)
                .map_err(|err| crate::error::format_diagnostic(&err))?;
            let listing = traversal
                .graph_listing(options.max_items.map(|n| n.saturating_add(1)))
                .map_err(|err| crate::error::format_diagnostic(&err))?;
            return crate::traversal::source::render_elements(
                &[listing],
                format,
                &options,
                1,
                false,
            )
            .map_err(|err| crate::error::format_diagnostic(&err));
        }
        Ok(mut traversal) => {
            let format = traversal.configured_visualizer().unwrap_or(fallback);
            return traversal
                .session_render_options(options)
                .and_then(|options| traversal.try_visualize_with_options(format, &options))
                .map_err(|err| crate::error::format_diagnostic(&err));
        }
        Err(value) => value,
    };

    // A schema map (`get_schema()`, `infer_schema()`, a `$schema`-tagged map): the schema
    // visualizer in the session's format.
    if let Some(schema) = schema_of_value(&value) {
        return Ok(RenderedOutput::complete(schema.visualize(fallback)));
    }

    // `profile()`: the text table of the optimized plan (never bounded: one row per step).
    let value = match value.try_cast_result::<crate::TraversalMetrics>() {
        Ok(metrics) => return Ok(RenderedOutput::complete(metrics.to_string())),
        Err(value) => value,
    };

    // `execute()`: the results, then the profile table; a failed run is its diagnostic.
    let value = match value.try_cast_result::<ScriptExecution>() {
        Ok(execution) => return execution.render(fallback, options),
        Err(value) => value,
    };

    let value = match value.try_cast_result::<RenderedText>() {
        Ok(rendered) => {
            return Ok(RenderedOutput::new(
                rendered.text.to_string(),
                rendered.truncation,
            ));
        }
        Err(value) => value,
    };

    let limit = crate::traversal::render::ItemLimit::new(options.max_items);
    let (rows, more_rows) = (options.max_rows, std::cell::Cell::new(false));
    let keep = |len: usize| match rows {
        Some(n) if len > n => {
            more_rows.set(true);
            n
        }
        _ => len,
    };

    let (text, rows_shown) = if value.is_unit() {
        (String::new(), 0)
    } else if value.is_array() {
        let items = value.cast::<rhai::Array>();
        let shown = keep(items.len());
        let text = items
            .iter()
            .take(shown)
            .map(|item| spelled_debug_value(item, &limit, options.spelling))
            .collect::<Vec<String>>()
            .join("\n");
        (text, shown)
    } else if value.is_string() {
        let text = value.into_immutable_string().unwrap_or_default();
        return Ok(RenderedOutput::complete(text.to_string()));
    } else if value.is_map() {
        // Renders a bare `Map` result (e.g. `g.memory_usage()`) as `key: value` lines instead of
        // Rhai's raw `#{...}` debug syntax, so a diagnostic method that isn't a traversal step
        // still prints something a human can read directly from `graphersal -e '...'`.
        let map = value.cast::<rhai::Map>();
        let mut keys: Vec<&str> = map.keys().map(|k| k.as_str()).collect();
        keys.sort_unstable();
        let shown = keep(keys.len());
        let text = keys
            .iter()
            .take(shown)
            .filter_map(|key| {
                map.get(*key).map(|value| {
                    format!(
                        "{key:?}: {}",
                        spelled_debug_value(value, &limit, options.spelling)
                    )
                })
            })
            .collect::<Vec<String>>()
            .join("\n");
        (text, shown)
    } else {
        (spelled_debug_value(&value, &limit, options.spelling), 1)
    };

    Ok(RenderedOutput::new(
        text,
        Truncation::of(
            rows_shown,
            more_rows.get(),
            limit.was_cut(),
            options.max_items,
        ),
    ))
}

/// Renders a script value the way Rhai's `Debug` does, except that a `Uuid` shows its canonical
/// string, a `JsonPath` its canonical text (`$.a.b`), a vertex or an edge `v[1]` /
/// `e[0][1-knows->2]` and a DSL token its literal (`Scope.local`, `P.gt(1)`, `__.out()`) at any
/// nesting level.
///
/// Rhai's own `Debug` knows nothing about registered custom types and prints only the Rust type
/// name for them, so a list of UUIDs would render as type names.
pub fn debug_value(value: &Dynamic) -> String {
    bounded_debug_value(value, &crate::traversal::render::ItemLimit::unlimited())
}

/// [`debug_value`] with every nested array or map cut to the item limit, marked with `…`.
pub(super) fn bounded_debug_value(
    value: &Dynamic,
    limit: &crate::traversal::render::ItemLimit,
) -> String {
    spelled_debug_value(value, limit, crate::traversal::Spelling::Snake)
}

/// [`bounded_debug_value`] with the token literals in `spelling` (the session's
/// `render.spelling`: `TextP.startingWith(..)`, `__.outE()` under camel).
fn spelled_debug_value(
    value: &Dynamic,
    limit: &crate::traversal::render::ItemLimit,
    spelling: crate::traversal::Spelling,
) -> String {
    debug_at_depth(
        value,
        limit,
        crate::value::depth::DEFAULT_MAX_VALUE_DEPTH,
        spelling,
    )
}

/// The canonical DSL literal of a token value, `None` for any other value: every token class of
/// the graph scope (`Scope.local`, `Column.keys`, `GType.LONG`, `Order.desc`, `Pick.any`,
/// `ProfileType.Memory`, `SchemaMode.closed`, `T.id`, `By.Count`, `Operator.sum`,
/// `Barrier.normSack`, `Direction.OUT`, `Merge.onCreate`, `Cardinality.list`,
/// `Cardinality.single(5)`, `Pop.first`, `WithOptions.tokens`, `CastPolicy.Skip`, `V.Table`), a
/// predicate (`P.gt(1)`, `TextP.containing("a")`) and an anonymous traversal (`__.out()`).
///
/// The one place that recognizes a token value: the session visualizer uses it, and every
/// token type's `to_string`/`to_debug` (and so `${..}`) prints the same
/// [`TokenLiteral`](crate::value::literal::TokenLiteral) text.
pub(crate) fn token_literal(value: &Dynamic) -> Option<String> {
    use crate::value::literal::TokenLiteral;
    macro_rules! try_tokens {
        ($($ty:ty),* $(,)?) => {
            $(
                if let Some(token) = value.read_lock::<$ty>() {
                    return Some(token.token_literal());
                }
            )*
        };
    }
    try_tokens!(
        TraversalScope,
        Column,
        GType,
        Order,
        Pop,
        crate::value::dt::DT,
        Operator,
        Barrier,
        crate::value::WithOptions,
        Direction,
        Merge,
        Cardinality,
        SchemaMode,
        CastPolicy,
        ProfileType,
        VisualizeFormat,
        ArgP,
        ArgBy,
        Pick,
        CardinalityValue,
        ScriptSet,
    );
    super::tokens::for_each_marker!(try_tokens);
    if let Some(traversal) = value.read_lock::<ScriptGraphTraversalSource>()
        && traversal.anonymous
    {
        return Some(traversal.token_literal());
    }
    None
}

/// A script value as a Rhai literal inside a token's text (`Cardinality.single(5)`,
/// `CastPolicy.Default("x")`): a token as its literal, any other value through the engine's
/// literal printer.
pub(crate) fn value_literal(value: &Dynamic) -> String {
    if let Some(token) = token_literal(value) {
        return token;
    }
    match super::dynamic::script_value_to_property(
        value.clone(),
        crate::value::depth::DEFAULT_MAX_VALUE_DEPTH,
    ) {
        Ok(property) => crate::value::literal::rhai_literal(&property),
        Err(_) => debug_value(value),
    }
}

/// [`bounded_debug_value`] with `levels` more levels of arrays/maps to show: a script can build
/// a value nested deeper than any limit the engine enforces (it never reaches the engine), so
/// the display stops there with `[…]` / `#{…}` instead of recursing further.
fn debug_at_depth(
    value: &Dynamic,
    limit: &crate::traversal::render::ItemLimit,
    levels: usize,
    spelling: crate::traversal::Spelling,
) -> String {
    if let Some(uuid) = value.read_lock::<ScriptUuid>() {
        return uuid.canonical();
    }
    // A date or time shows its DSL literal (`datetime("2023-08-02T00:00:00Z")`), so it is told
    // apart from a string.
    if let Some(temporal) = dynamic_temporal(value) {
        return crate::value::literal::rhai_temporal(&temporal);
    }
    // A vector shows its DSL literal (`vector([0.1, 0.2])`, cut like a list), never an array.
    if let Some(vector) = value.read_lock::<ScriptVector>() {
        return vector.0.literal(Some(limit.keep(vector.0.dimension())));
    }
    // A path value shows its canonical text (`$.a.b[1]`), not the Rust type name.
    if let Some(path) = value.read_lock::<crate::value::JsonPath>() {
        return path.to_string();
    }
    if let Some(map) = value.read_lock::<ScriptMap>() {
        return script_map_text(&map, limit);
    }
    // A vertex or an edge value prints as TinkerPop's console prints it: `v[1]`,
    // `e[0][1-knows->2]`.
    if let Some(vertex) = value.read_lock::<ScriptVertex>() {
        return vertex_text(&vertex.0);
    }
    if let Some(edge) = value.read_lock::<ScriptEdge>() {
        return edge_text(&edge.0);
    }
    if let Some(vertex) = value.read_lock::<ScriptVertexRef>() {
        return format!("v[{}]", vertex.0.id());
    }
    if let Some(token) = token_literal(value) {
        return crate::traversal::spelling::respell(&token, spelling).into_owned();
    }
    if let Some(rendered) = value.read_lock::<RenderedText>() {
        return format!("{:?}", rendered.as_str());
    }
    if value.is_array()
        && let Some(items) = value.read_lock::<rhai::Array>()
    {
        let Some(inner) = levels.checked_sub(1) else {
            return format!("[{}]", crate::traversal::render::CUT_MARKER);
        };
        let keep = limit.keep(items.len());
        let mut rendered: Vec<String> = items
            .iter()
            .take(keep)
            .map(|item| debug_at_depth(item, limit, inner, spelling))
            .collect();
        if keep < items.len() {
            rendered.push(crate::traversal::render::CUT_MARKER.to_string());
        }
        return format!("[{}]", rendered.join(", "));
    }
    if value.is_map()
        && let Some(map) = value.read_lock::<rhai::Map>()
    {
        let Some(inner) = levels.checked_sub(1) else {
            return format!("#{{{}}}", crate::traversal::render::CUT_MARKER);
        };
        let keep = limit.keep(map.len());
        let mut entries: Vec<String> = map
            .iter()
            .take(keep)
            .map(|(key, value)| {
                format!(
                    "{:?}: {}",
                    key.as_str(),
                    debug_at_depth(value, limit, inner, spelling)
                )
            })
            .collect();
        if keep < map.len() {
            entries.push(crate::traversal::render::CUT_MARKER.to_string());
        }
        return format!("#{{{}}}", entries.join(", "));
    }
    format!("{value:?}")
}

/// Builds an `InvalidOption` error for a render option.
fn invalid_render_option(
    key: &str,
    reason: impl Into<String>,
) -> Box<crate::error::TraverserError> {
    Box::new(crate::error::TraverserError::InvalidOption {
        key: key.to_string(),
        reason: reason.into(),
    })
}

/// Reads a session display limit (`render.max_rows` / `render.max_items`): a positive integer.
/// `base`, overridden by the [`RENDER_MAX_ROWS`] / [`RENDER_MAX_ITEMS`] entries of a traversal's
/// options map; see [`ScriptGraphTraversalSource::session_render_options`].
pub(super) fn render_options_from(
    options_map: &std::collections::HashMap<CompactString, ElementProperty>,
    base: &RenderOptions,
) -> crate::error::TraverserResult<RenderOptions> {
    let mut options = base.clone();
    if let Some(value) = options_map.get(RENDER_MAX_ROWS) {
        options.max_rows = Some(positive_render_option(RENDER_MAX_ROWS, value)?);
    }
    if let Some(value) = options_map.get(RENDER_MAX_ITEMS) {
        options.max_items = Some(positive_render_option(RENDER_MAX_ITEMS, value)?);
    }
    Ok(options)
}

fn positive_render_option(
    key: &str,
    value: &ElementProperty,
) -> crate::error::TraverserResult<usize> {
    match value {
        ElementProperty::Number(n) => match n.as_i64() {
            Some(v) if v >= 1 => Ok(usize::try_from(v).unwrap_or(usize::MAX)),
            _ => Err(invalid_render_option(
                key,
                format!("expected a positive integer, got {n}"),
            )),
        },
        other => Err(invalid_render_option(
            key,
            format!(
                "expected a positive integer, got {}",
                crate::value::physical::to_json_text(other).unwrap_or_default()
            ),
        )),
    }
}

/// Reads a per-call display limit (`max_rows: n` / `max_items: n`): an integer of at least 1.
fn call_limit(key: &str, value: &Dynamic) -> crate::error::TraverserResult<usize> {
    match value.as_int() {
        Ok(v) if v >= 1 => Ok(usize::try_from(v).unwrap_or(usize::MAX)),
        Ok(v) => Err(invalid_render_option(
            key,
            format!(
                "{v} is not a positive number of {}",
                if key.contains("row") { "rows" } else { "items" }
            ),
        )),
        Err(type_name) => Err(invalid_render_option(
            key,
            format!(
                "expected a positive integer, got {}",
                super::register::display_type_name(type_name)
            ),
        )),
    }
}

/// Parses the options argument of an explicit visualizer (`to_table(#{..})`, `to_json(#{..})`,
/// `visualize(fmt, #{..})`, ...) on top of the session display limits.
///
/// An array is a column list. A map may hold the table layout keys (`columns`, `max_width`,
/// `truncate`, `row_numbers`) and the display limits: `max_rows: n` / `max_items: n` (n ≥ 1)
/// replace the session default for this call, `no_limit: true` removes both limits, and
/// `no_limit: false` keeps the session default. `0` and `no_limit: true` combined with a limit are
/// errors.
pub(super) fn dynamic_to_render_options(
    arg: &Dynamic,
    session: &RenderOptions,
) -> crate::error::TraverserResult<RenderOptions> {
    let mut options = RenderOptions {
        max_rows: session.max_rows,
        max_items: session.max_items,
        ..RenderOptions::default()
    };

    if arg.is_array() {
        let arr = arg.clone().cast::<rhai::Array>();
        let cols: Vec<String> = arr.into_iter().map(dynamic_to_string).collect();
        options.columns = Some(cols);
        return Ok(options);
    }

    if arg.is_map() {
        let map = arg.clone().cast::<rhai::Map>();
        let mut max_rows = None;
        let mut max_items = None;
        let mut no_limit = false;
        for (key, val) in map {
            match key.as_str() {
                "columns" => {
                    if val.is_array() {
                        let arr = val.cast::<rhai::Array>();
                        let cols: Vec<String> = arr.into_iter().map(dynamic_to_string).collect();
                        options.columns = Some(cols);
                    }
                }
                "max_width" | "maxWidth" => {
                    if let Ok(w) = val.as_int()
                        && w >= 0
                    {
                        options.max_width = Some(w as usize);
                    }
                }
                "truncate" => {
                    if let Ok(t) = val.as_int()
                        && t >= 0
                    {
                        options.truncate = Some(t as usize);
                    }
                }
                "row_numbers" | "rowNumbers" => {
                    if let Ok(b) = val.as_bool() {
                        options.row_numbers = b;
                    }
                }
                "max_rows" | "maxRows" => max_rows = Some(call_limit("max_rows", &val)?),
                "max_items" | "maxItems" => max_items = Some(call_limit("max_items", &val)?),
                "no_limit" | "noLimit" => {
                    no_limit = val.as_bool().map_err(|type_name| {
                        invalid_render_option(
                            "no_limit",
                            format!(
                                "expected true or false, got {}",
                                super::register::display_type_name(type_name)
                            ),
                        )
                    })?;
                }
                _ => {}
            }
        }
        if no_limit {
            if max_rows.is_some() || max_items.is_some() {
                return Err(invalid_render_option(
                    "no_limit",
                    "no_limit: true removes every display limit and cannot be combined with max_rows or max_items",
                ));
            }
            options.max_rows = None;
            options.max_items = None;
        }
        if max_rows.is_some() {
            options.max_rows = max_rows;
        }
        if max_items.is_some() {
            options.max_items = max_items;
        }
        return Ok(options);
    }

    Err(invalid_render_option(
        "options",
        format!(
            "expected an array of columns or an options map, got {}",
            type_name_of(arg)
        ),
    ))
}

/// Session option key of the default `max_rows` display limit (a positive integer). See
/// [`graph_scope_with_options`] and [`render_scope_options`].
pub const RENDER_MAX_ROWS: &str = "render.max_rows";
/// Session option key of the default `max_items` display limit (a positive integer).
pub const RENDER_MAX_ITEMS: &str = "render.max_items";
/// Session option key of the spelling of step names in diagnostics: `"snake"` (the default) or
/// `"camel"`. Unlike the display limits it is a traversal option the engine reads
/// (`g.with("render.spelling", "camel")`), see [`Spelling`](crate::traversal::Spelling).
pub const RENDER_SPELLING: &str = crate::traversal::context::RENDER_SPELLING;

/// The scope options that make `options`' display limits the session default of every display
/// in a script: the implicit visualizer, explicit `to_*()` calls and `visualize(..)`.
///
/// Pass them to [`graph_scope_with_options`]. An unlimited `options` yields no entries, which is
/// the library default.
pub fn render_scope_options(options: &RenderOptions) -> Vec<(&'static str, ElementProperty)> {
    let as_prop = |n: usize| ElementProperty::from(i64::try_from(n).unwrap_or(i64::MAX));
    let mut entries = Vec::with_capacity(2);
    if let Some(n) = options.max_rows {
        entries.push((RENDER_MAX_ROWS, as_prop(n)));
    }
    if let Some(n) = options.max_items {
        entries.push((RENDER_MAX_ITEMS, as_prop(n)));
    }
    if options.spelling != crate::traversal::Spelling::Snake {
        entries.push((
            RENDER_SPELLING,
            ElementProperty::from(options.spelling.as_str()),
        ));
    }
    entries
}

/// Replaces the session display limits and step-name spelling of a scope built by
/// [`graph_scope_with_options`] (its `g`
/// and `_g` sources), for example after a REPL `/set max-rows 20`. Traversals already stored in
/// script variables keep the limits they were built with.
pub fn set_scope_render_options(scope: &mut Scope, options: &RenderOptions) {
    for name in ["g", "_g"] {
        if let Some(value) = scope.get_mut(name)
            && let Some(mut source) = value.write_lock::<ScriptGraphTraversalSource>()
        {
            source.options.remove(RENDER_MAX_ROWS);
            source.options.remove(RENDER_MAX_ITEMS);
            source.options.remove(RENDER_SPELLING);
            for (key, value) in render_scope_options(options) {
                source.options.insert(CompactString::new(key), value);
            }
        }
    }
}

/// An explicit visualizer's script result (`to_table()`, `to_json()`, `visualize()`, ...).
///
/// It carries its own [`Truncation`] report instead of a side channel keyed by string identity,
/// so a script holding several rendered values (e.g. `let a = ...; let b = ...;`) does not lose
/// one's notice to the other. Registered with Rhai as `RenderedText`; it prints as its text like
/// a plain string (`to_string`/`to_debug`), so DSL scripts that only ever print or return it see
/// no difference.
#[derive(Debug, Clone)]
pub struct RenderedText {
    pub(super) text: ImmutableString,
    truncation: Option<Truncation>,
}

impl RenderedText {
    pub(super) fn new(rendered: RenderedOutput) -> Self {
        Self {
            text: ImmutableString::from(rendered.text),
            truncation: rendered.truncation,
        }
    }

    /// Wraps a diagnostic (a failed render) as untruncated text.
    pub(super) fn diagnostic(text: ImmutableString) -> Self {
        Self {
            text,
            truncation: None,
        }
    }

    pub(super) fn as_str(&self) -> &str {
        &self.text
    }
}

impl std::fmt::Display for RenderedText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}
