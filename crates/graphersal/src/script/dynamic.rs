//! Conversions between Rhai `Dynamic` values and engine values: property keys, ids,
//! predicates (`ArgP` -> `P`), `by()` arguments, child traversals, JSON, and the argument
//! checkers and mismatch diagnostics the step builder methods share.

use super::*;

/// The options map of `glob_path(pattern, #{...})`, checked for its keys and value types when
/// the step is built; `max_depth` is range-checked by the Rust step when it executes.
#[derive(Debug, Clone, Default)]
pub(super) struct ScriptGlobPathOptions {
    max_depth: Option<i64>,
    pub(super) prune: Option<ScriptGraphTraversalSource>,
}

/// The keys `glob_path(pattern, #{...})` accepts.
const GLOB_PATH_OPTION_KEYS: &str = "max_depth, prune";
/// The keys reserved for future `glob_path()` options: rejected, never ignored.
const GLOB_PATH_RESERVED_KEYS: [&str; 3] = ["edges", "path", "case_insensitive"];

/// The display name of a script value's type (`int64`, `Predicate`, `GraphTraversalSource`):
/// what a diagnostic says a wrong argument is, never the Rust type (`f64`,
/// `graphersal::script::tokens::predicate::ArgP`).
pub(crate) fn type_name_of(arg: &Dynamic) -> String {
    super::register::display_type_name(arg.type_name())
}

/// The text of a script value inside a diagnostic or a step argument read as text: a primitive
/// as Rhai prints it, any other script type (a token, a traversal, a predicate) by its display
/// type name, since Rhai prints such a value as its Rust type path.
pub(crate) fn dynamic_text(arg: &Dynamic) -> String {
    if arg.is_array() || arg.is_map() {
        let mut out = String::new();
        write_dynamic_literal(&mut out, arg, crate::value::depth::DEFAULT_MAX_VALUE_DEPTH);
        out
    } else if let Some(value) = dynamic_temporal(arg) {
        crate::value::literal::rhai_temporal(&value)
    } else if let Some(vector) = arg.read_lock::<ScriptVector>() {
        vector
            .0
            .literal(Some(crate::value::literal::MAX_LITERAL_ITEMS))
    } else if arg.type_name().contains("::") {
        type_name_of(arg)
    } else {
        arg.to_string()
    }
}

/// [`dynamic_text`] of a value inside a list or map: strings quoted, as Rhai prints a container,
/// but every script type by its display name; `levels` bounds the nesting it follows.
fn write_dynamic_literal(out: &mut String, arg: &Dynamic, levels: usize) {
    if let Some(items) = arg.read_lock::<rhai::Array>() {
        let Some(inner) = levels.checked_sub(1) else {
            out.push_str("[…]");
            return;
        };
        out.push('[');
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            write_dynamic_literal(out, item, inner);
        }
        out.push(']');
    } else if let Some(map) = arg.read_lock::<rhai::Map>() {
        let Some(inner) = levels.checked_sub(1) else {
            out.push_str("#{…}");
            return;
        };
        out.push_str("#{");
        for (i, (key, item)) in map.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&crate::value::literal::rhai_key(key));
            out.push_str(": ");
            write_dynamic_literal(out, item, inner);
        }
        out.push('}');
    } else if arg.is_string() {
        out.push_str(&crate::value::literal::rhai_string(&arg.to_string()));
    } else {
        out.push_str(&dynamic_text(arg));
    }
}

pub(super) fn dynamic_to_string(arg: Dynamic) -> String {
    if arg.is_string() {
        arg.into_string().unwrap_or_else(|s| s.to_string())
    } else if let Some(uuid) = arg.read_lock::<ScriptUuid>() {
        uuid.canonical()
    } else if let Some(value) = dynamic_temporal(&arg) {
        value.to_string()
    } else {
        dynamic_text(&arg)
    }
}

pub(super) fn dynamic_to_compact_string(arg: Dynamic) -> CompactString {
    if arg.is_string() {
        if let Some(imm) = arg.read_lock::<rhai::ImmutableString>() {
            return CompactString::new(imm.as_str());
        }
        CompactString::new(arg.into_string().unwrap_or_else(|s| s.to_string()))
    } else if let Some(uuid) = arg.read_lock::<ScriptUuid>() {
        CompactString::new(uuid.canonical())
    } else {
        CompactString::new(dynamic_text(&arg))
    }
}

/// The key of `property(key, ..)`: a `JsonPath` or a string; anything else is a clear script
/// error (a number or a map used to fail as "function not found").
pub(super) fn property_write_key(
    step: &str,
    arg: Dynamic,
) -> Result<PropKey, Box<rhai::EvalAltResult>> {
    if let Some(path) = arg.read_lock::<crate::value::JsonPath>() {
        return Ok(PropKey::Path(path.clone()));
    }
    match arg.into_immutable_string() {
        Ok(name) => Ok(PropKey::Name(CompactString::new(name.as_str()))),
        // A traversal as the key (`property(select("a"), v)`) is a TinkerPop form this engine
        // does not implement: reported as an unknown overload, as before this step took a
        // `Dynamic` key.
        Err(type_name) if type_name.ends_with("ScriptGraphTraversalSource") => {
            Err(Box::new(rhai::EvalAltResult::ErrorFunctionNotFound(
                format!("{step} (GraphTraversalSource, ..)"),
                rhai::Position::NONE,
            )))
        }
        Err(type_name) => Err(script_error_from_script(
            crate::error::ScriptError::ArgumentMismatch {
                step_name: step.to_string(),
                expected: "a property name string or a jpath(..)".to_string(),
                actual: super::register::display_type_name(type_name),
            },
        )),
    }
}

/// A string argument that may be `()` (`null`): `None` for `()`, the string otherwise; any other
/// value is a `ScriptError::ArgumentMismatch` naming `step`.
pub(super) fn nullable_string(
    step: &str,
    arg: Dynamic,
) -> Result<Option<CompactString>, Box<rhai::EvalAltResult>> {
    if arg.is_unit() {
        return Ok(None);
    }
    if arg.is_string() {
        return Ok(Some(dynamic_to_compact_string(arg)));
    }
    Err(script_error_from_script(
        crate::error::ScriptError::ArgumentMismatch {
            step_name: step.to_string(),
            expected: "a string or ()".to_string(),
            actual: type_name_of(&arg),
        },
    ))
}

/// What a property-key argument takes, in the diagnostic of a refused one.
const PROPERTY_KEY: &str = "a property key (a string or a jpath(..))";

/// A property key of `step` from a script value: a `JsonPath` (from `jpath("...")`) is a path
/// key, anything else is the literal name exactly as [`dynamic_to_compact_string`] reads it. A
/// string is never parsed as a path. A traversal is refused ([`refuse_traversal`]).
pub(super) fn dynamic_to_prop_key(
    step: &str,
    arg: Dynamic,
) -> Result<PropKey, Box<rhai::EvalAltResult>> {
    refuse_traversal(step, PROPERTY_KEY, &arg)?;
    if let Some(path) = arg.read_lock::<crate::value::JsonPath>() {
        return Ok(PropKey::Path(path.clone()));
    }
    Ok(PropKey::Name(dynamic_to_compact_string(arg)))
}

/// [`dynamic_to_prop_key`] for every item of `arg`.
pub(super) fn vec_dynamic_to_prop_key(
    step: &str,
    arg: Vec<Dynamic>,
) -> Result<Vec<PropKey>, Box<rhai::EvalAltResult>> {
    arg.into_iter()
        .map(|key| dynamic_to_prop_key(step, key))
        .collect()
}

pub(super) fn vec_dynamic_to_string(arg: Vec<Dynamic>) -> Vec<String> {
    arg.into_iter().map(dynamic_to_string).collect()
}

/// Saturates a script integer into the `i32` range of a string index, so an out-of-range index
/// clamps to the string instead of wrapping around to the other sign.
pub(super) fn clamp_i32(value: i64) -> i32 {
    i32::try_from(value).unwrap_or(if value < 0 { i32::MIN } else { i32::MAX })
}

/// Collects the ids that `V()`, `E()` and `hasId()` take from their script arguments.
///
/// An array in the first position flattens recursively (`g.E([e1, e2])`; a later array matches
/// nothing, as in TinkerPop); a map with an `"id"` key is an element as
/// returned by `next()`/`toList()` and contributes that id (`g.E(e)`); `()` (null) contributes the
/// empty id, which matches no element (a lone `g.E(null)` stays empty instead of widening to every
/// edge); strings, integers and UUIDs convert as before. Any other map is rejected with
/// `ScriptError::ArgumentMismatch` rather than stringified into an id that never matches.
///
/// **Deviations from TinkerPop 3.8.2** (user's guide: "Id Arguments of V(), E() and hasId()"):
/// - Only the FIRST argument flattens an array. TinkerPop's `hasId(id, otherIds...)` keeps a
///   later list as one id that matches nothing; Graphersal does the same in effect but models it
///   as the empty id.
/// - `()` (null) contributes the empty id (matches nothing). `g.E(null)` and `hasId(null)` stay
///   empty instead of widening to every element.
/// - Elements and maps with an `"id"` key (as returned by `next()`/`toList()`) are accepted as
///   ids, an extension; other maps are an error.
///
/// Nested id lists flatten; a list nested deeper than `max_depth` levels (the engine's
/// [`ScriptLimits::max_value_depth`]) fails with the `script.max_value_depth` diagnostic instead
/// of recursing further.
pub(super) fn ids_from_dynamic(
    step_name: &str,
    args: Vec<Dynamic>,
    max_depth: usize,
) -> Result<Vec<CompactString>, Box<rhai::EvalAltResult>> {
    fn collect(
        step_name: &str,
        arg: Dynamic,
        out: &mut Vec<CompactString>,
        levels: usize,
        max_depth: usize,
    ) -> Result<(), Box<rhai::EvalAltResult>> {
        if arg.is_array() {
            let inner = levels
                .checked_sub(1)
                .ok_or_else(|| value_too_deep_error(max_depth))?;
            for item in arg.cast::<rhai::Array>() {
                collect(step_name, item, out, inner, max_depth)?;
            }
        } else if arg.is_map() {
            let map = arg.cast::<rhai::Map>();
            match map.get("id") {
                Some(id) if !id.is_map() && !id.is_array() => {
                    out.push(dynamic_to_compact_string(id.clone()))
                }
                _ => {
                    return Err(Box::new(rhai::EvalAltResult::ErrorRuntime(
                        Dynamic::from(crate::error::format_diagnostic(
                            &crate::error::ScriptError::ArgumentMismatch {
                                step_name: step_name.to_string(),
                                expected:
                                    "ids (strings, numbers, UUIDs), elements or lists of them"
                                        .to_string(),
                                actual: "a map without an `id` key".to_string(),
                            },
                        )),
                        rhai::Position::NONE,
                    )));
                }
            }
        } else if let Some(id) = element_id_of(&arg) {
            out.push(id);
        } else {
            out.push(dynamic_to_compact_string(arg));
        }
        Ok(())
    }
    let mut out = Vec::with_capacity(args.len());
    for (position, arg) in args.into_iter().enumerate() {
        if position > 0 && arg.is_array() {
            // TinkerPop flattens a collection only in the first position (`hasId(id, otherIds...)`
            // keeps a later list as one id that matches nothing): the empty id matches no element.
            out.push(CompactString::default());
        } else {
            collect(step_name, arg, &mut out, max_depth, max_depth)?;
        }
    }
    Ok(out)
}

/// Refuses a traversal where `step` takes values (ids, labels, keys, property values): its
/// text would be the type name `GraphTraversalSource`, which matches nothing, so the step would
/// filter everything out without an error (issue #9). `expected` names what the step takes.
pub(super) fn refuse_traversal(
    step: &str,
    expected: &str,
    arg: &Dynamic,
) -> Result<(), Box<rhai::EvalAltResult>> {
    if !arg.is::<ScriptGraphTraversalSource>() {
        return Ok(());
    }
    let actual = match super::render::token_literal(arg) {
        Some(literal) => format!("{} ({literal})", crate::error::TRAVERSAL_ARGUMENT),
        None => crate::error::TRAVERSAL_ARGUMENT.to_string(),
    };
    Err(script_error_from_script(
        crate::error::ScriptError::ArgumentMismatch {
            step_name: step.to_string(),
            expected: expected.to_string(),
            actual,
        },
    ))
}

/// The arguments of `hasId`/`hasLabel`/`hasKey`: plain values, or one predicate.
pub(super) enum TokenArgs {
    Literals(Vec<Dynamic>),
    Predicate(ArgP),
}

/// Splits the arguments of a token filter. A predicate is never stringified into an id, label or
/// key: `P.eq(x)` and `P.within(xs)` become the literal form (so the optimizer can still fold it
/// into the source step), any other predicate is kept, and a predicate mixed with other
/// arguments is an argument error. A traversal is refused ([`refuse_traversal`]).
pub(super) fn split_token_args(
    step_name: &str,
    args: Vec<Dynamic>,
) -> Result<TokenArgs, Box<rhai::EvalAltResult>> {
    for arg in &args {
        refuse_traversal(step_name, "values or a single predicate", arg)?;
    }
    let predicates = args.iter().filter(|a| a.is::<ArgP>()).count();
    if predicates == 0 {
        return Ok(TokenArgs::Literals(args));
    }
    if args.len() != 1 {
        return Err(Box::new(rhai::EvalAltResult::ErrorRuntime(
            Dynamic::from(crate::error::format_diagnostic(
                &crate::error::ScriptError::ArgumentMismatch {
                    step_name: step_name.to_string(),
                    expected: "values, or a single predicate".to_string(),
                    actual: "a predicate mixed with other arguments".to_string(),
                },
            )),
            rhai::Position::NONE,
        )));
    }
    // `P.eq(x)` / `P.within(xs)` become literals, checked like any literal (an element given
    // by its map is its id); the other predicates compare values.
    let predicate = read_argp(&args[0])?;
    Ok(match predicate {
        ArgP::Eq(value) => TokenArgs::Literals(vec![value]),
        ArgP::WithIn(values) => TokenArgs::Literals(values),
        other => {
            check_operands(&other)?;
            TokenArgs::Predicate(other)
        }
    })
}

/// Converts a script predicate to the engine predicate.
pub(super) fn argp_to_p(arg: &ArgP) -> P<PValue> {
    match arg {
        ArgP::Eq(d) => P::Eq(dynamic_to_pvalue(d)),
        ArgP::Neq(d) => P::Neq(dynamic_to_pvalue(d)),
        ArgP::Lt(d) => P::Lt(dynamic_to_pvalue(d)),
        ArgP::Gt(d) => P::Gt(dynamic_to_pvalue(d)),
        ArgP::Lte(d) => P::Lte(dynamic_to_pvalue(d)),
        ArgP::Gte(d) => P::Gte(dynamic_to_pvalue(d)),
        ArgP::WithIn(v) => P::WithIn(vec_dynamic_to_pvalue(v)),
        ArgP::WithOut(v) => P::WithOut(vec_dynamic_to_pvalue(v)),
        ArgP::Inside(a, b) => P::Inside(dynamic_to_pvalue(a), dynamic_to_pvalue(b)),
        ArgP::Outside(a, b) => P::Outside(dynamic_to_pvalue(a), dynamic_to_pvalue(b)),
        ArgP::Between(a, b) => P::Between(dynamic_to_pvalue(a), dynamic_to_pvalue(b)),
        ArgP::StartingWith(d) => P::StartingWith(dynamic_to_pvalue(d)),
        ArgP::NotStartingWith(d) => P::NotStartingWith(dynamic_to_pvalue(d)),
        ArgP::EndingWith(d) => P::EndingWith(dynamic_to_pvalue(d)),
        ArgP::NotEndingWith(d) => P::NotEndingWith(dynamic_to_pvalue(d)),
        ArgP::Containing(d) => P::Containing(dynamic_to_pvalue(d)),
        ArgP::NotContaining(d) => P::NotContaining(dynamic_to_pvalue(d)),
        ArgP::Regex(regex) => P::Regex(regex.clone()),
        ArgP::NotRegex(regex) => P::NotRegex(regex.clone()),
        ArgP::TypeOf(target) => P::type_of(target.clone()),
        ArgP::And(a, b) => argp_to_p(a).and(argp_to_p(b)),
        ArgP::Or(a, b) => argp_to_p(a).or(argp_to_p(b)),
        ArgP::Not(inner) => P::not(argp_to_p(inner)),
    }
}

/// A name argument of `step` (a label, a step label, a side-effect or projection key) as
/// [`dynamic_to_compact_string`] reads it; a traversal is refused ([`refuse_traversal`]).
pub(super) fn name_arg(
    step: &str,
    arg: Dynamic,
) -> Result<CompactString, Box<rhai::EvalAltResult>> {
    refuse_traversal(step, "names (strings)", &arg)?;
    Ok(dynamic_to_compact_string(arg))
}

/// [`name_arg`] for every item of `arg`.
pub(super) fn vec_dynamic_to_compact_string(
    step: &str,
    arg: Vec<Dynamic>,
) -> Result<Vec<CompactString>, Box<rhai::EvalAltResult>> {
    arg.into_iter().map(|name| name_arg(step, name)).collect()
}

/// A `to_list()`/`next()` result as a script value: a graph becomes a traversal source over it,
/// private to the script unless `origin` says it is a host graph.
pub(super) fn list_item_to_dynamic(
    item: crate::traversal::source::ListItem,
    origin: &access::GraphOrigin,
) -> Dynamic {
    match item {
        crate::traversal::source::ListItem::Value(value) => element_property_to_dynamic(value),
        crate::traversal::source::ListItem::Graph(graph) => Dynamic::from(origin.source(graph)),
    }
}

/// Wraps a traversal failure into a script runtime error carrying the full diagnostic report.
pub(super) fn script_error(err: crate::error::BoxedTraverserError) -> Box<rhai::EvalAltResult> {
    Box::new(rhai::EvalAltResult::ErrorRuntime(
        Dynamic::from(crate::error::format_diagnostic(&err)),
        rhai::Position::NONE,
    ))
}

/// A compressed string as a script string; out of line so the recursive conversion keeps a
/// small stack frame.
#[inline(never)]
fn compressed_to_dynamic(text: &crate::value::CompressedText) -> Dynamic {
    Dynamic::from(text.text().into_string())
}

/// Converts a materialized [`ElementProperty`] into the closest native Rhai value.
///
/// This is the inverse of [`dynamic_to_element_property`], so values handed to a script by
/// `to_list()` / `next()` can be fed straight back into the DSL.
pub(crate) fn element_property_to_dynamic(value: ElementProperty) -> Dynamic {
    match value {
        ElementProperty::Null => Dynamic::UNIT,
        ElementProperty::String(value) => Dynamic::from(value.to_string()),
        // A script value is always plain (D5).
        ElementProperty::Compressed(text) => compressed_to_dynamic(&text),
        ElementProperty::Deduped(text) => Dynamic::from(text.as_str().to_string()),
        ElementProperty::Boolean(value) => Dynamic::from_bool(value),
        ElementProperty::Uuid(value) => Dynamic::from(ScriptUuid(value)),
        // One script type per temporal type (`datetime`, `date`, ...): a result fed back into
        // the DSL stays a temporal value of its type.
        ElementProperty::Temporal(value) => temporal_to_dynamic(value),
        // The Rhai vector type: prints as its literal, fed back into the DSL as a vector.
        ElementProperty::Vector(value) => Dynamic::from(ScriptVector(value)),
        ElementProperty::Number(value) => {
            if let Some(int) = value.as_i64() {
                Dynamic::from_int(int)
            } else if let Some(float) = value.as_f64() {
                Dynamic::from_float(float)
            } else {
                Dynamic::UNIT
            }
        }
        ElementProperty::Array(values) => Dynamic::from_array(
            values
                .into_iter()
                .map(element_property_to_dynamic)
                .collect::<Vec<Dynamic>>(),
        ),
        ElementProperty::Object(element) => Dynamic::from_map(element_to_map(*element)),
        ElementProperty::Vertex(vertex) => Dynamic::from(ScriptVertex(*vertex)),
        ElementProperty::Edge(edge) => Dynamic::from(ScriptEdge(*edge)),
        ElementProperty::Map(map) => Dynamic::from(ScriptMap(*map)),
    }
}

/// Converts a graph element (vertex, edge or projection) into a Rhai object map, keeping the
/// element keys addressable from script code as `item.name` or `item["name"]`.
fn element_to_map(element: crate::PropertyMap) -> rhai::Map {
    element
        .into_iter()
        .map(|(key, value)| (key.as_str().into(), element_property_to_dynamic(value)))
        .collect()
}

/// Converts a plain script value (what `to_list()` / `next()` return, a literal, a map, a list)
/// into an [`ElementProperty`]: the inverse of the terminals' conversion, so a host can turn a
/// script result into data (e.g. JSON through `ElementProperty`'s `serde` form). A `Uuid` stays a
/// `Uuid`, a value-keyed map stays a map; a DSL token becomes its Gremlin spelling. Any other
/// script type (a traversal, `TraversalMetrics`, an `Execution`) has no value form and becomes
/// its display string, so a host checks for those first.
///
/// Fails with [`TraverserError::ResourceLimitExceeded`](crate::TraverserError::ResourceLimitExceeded)
/// (`script.max_value_depth`) when the value nests deeper than `max_depth` levels of arrays and
/// maps (pass [`ScriptLimits::max_value_depth`], by default 128): a deeper value would overflow
/// the native stack of the code that reads it.
pub fn script_value_to_property(
    value: Dynamic,
    max_depth: usize,
) -> Result<ElementProperty, crate::TraverserError> {
    let max_depth = if max_depth == 0 {
        crate::value::depth::DEFAULT_MAX_VALUE_DEPTH
    } else {
        max_depth
    };
    convert_bounded(value, max_depth).map_err(|TooDeep| value_too_deep(max_depth))
}

/// The error of a script value nested deeper than `max` levels.
pub(super) fn value_too_deep(max: usize) -> crate::TraverserError {
    crate::TraverserError::ResourceLimitExceeded {
        limit: limits::LIMIT_MAX_VALUE_DEPTH.to_string(),
        max: Some(max as u64),
        observed: None,
    }
}

/// [`value_too_deep`] as a script error.
fn value_too_deep_error(max: usize) -> Box<rhai::EvalAltResult> {
    Box::new(rhai::EvalAltResult::ErrorRuntime(
        Dynamic::from(crate::error::format_diagnostic(&value_too_deep(max))),
        rhai::Position::NONE,
    ))
}

/// Converts a script value handed to the engine (a step argument, a token argument), bounded by
/// the value-depth limit of the engine running `ctx` ([`ScriptLimits::max_value_depth`]).
pub(super) fn value_arg(
    ctx: &NativeCallContext,
    arg: Dynamic,
) -> Result<ElementProperty, Box<rhai::EvalAltResult>> {
    dynamic_to_element_property(arg, access::value_depth_of(ctx))
}

/// Whether a script value is a schema in its data shape: a map whose `$schema` is the format
/// URL ([`crate::graph::schema::SCHEMA_FORMAT_URL`]), as `get_schema()`/`infer_schema()` return it.
pub(crate) fn is_schema_map(arg: &Dynamic) -> bool {
    arg.read_lock::<rhai::Map>().is_some_and(|map| {
        map.get("$schema")
            .and_then(|url| {
                url.read_lock::<rhai::ImmutableString>()
                    .map(|s| s.to_string())
            })
            .is_some_and(|url| url == crate::graph::schema::SCHEMA_FORMAT_URL)
    })
}

/// A schema argument: a map or JSON text in the schema format (strictly loaded).
pub(super) fn schema_arg(
    ctx: &NativeCallContext,
    arg: Dynamic,
) -> Result<GraphSchema, Box<rhai::EvalAltResult>> {
    let parsed = if arg.is_string() {
        GraphSchema::from_json(&dynamic_text(&arg))
    } else if arg.is_map() {
        GraphSchema::from_value(&value_arg(ctx, arg)?)
    } else {
        return Err(Box::new(rhai::EvalAltResult::ErrorMismatchDataType(
            "a schema (a map or JSON text)".to_string(),
            type_name_of(&arg),
            rhai::Position::NONE,
        )));
    };
    parsed.map_err(|err| script_error(Box::new(crate::TraverserError::Schema(err))))
}

/// A schema mode argument: a `SchemaMode` token (`SchemaMode.closed`) or one of the strings
/// `"none"`, `"open"`, `"closed"`.
pub(super) fn schema_mode_arg(arg: Dynamic) -> Result<SchemaMode, Box<rhai::EvalAltResult>> {
    if let Some(mode) = arg.clone().try_cast::<SchemaMode>() {
        return Ok(mode);
    }
    if arg.is_string() {
        let name = dynamic_text(&arg);
        return SchemaMode::parse(&name)
            .map_err(|err| script_error(Box::new(crate::TraverserError::Schema(err))));
    }
    Err(Box::new(rhai::EvalAltResult::ErrorMismatchDataType(
        "a schema mode (SchemaMode.none/open/closed or \"none\"/\"open\"/\"closed\")".to_string(),
        type_name_of(&arg),
        rhai::Position::NONE,
    )))
}

/// A schema patch argument (JSON Merge Patch): a map or JSON text.
pub(super) fn patch_arg(
    ctx: &NativeCallContext,
    arg: Dynamic,
) -> Result<crate::graph::schema::SchemaPatch, Box<rhai::EvalAltResult>> {
    if arg.is_string() {
        crate::graph::schema::SchemaPatch::from_json(&dynamic_text(&arg))
            .map_err(|err| script_error(Box::new(crate::TraverserError::Schema(err))))
    } else if arg.is_map() {
        Ok(crate::graph::schema::SchemaPatch::from_value(value_arg(
            ctx, arg,
        )?))
    } else {
        Err(Box::new(rhai::EvalAltResult::ErrorMismatchDataType(
            "a schema patch (a map or JSON text)".to_string(),
            type_name_of(&arg),
            rhai::Position::NONE,
        )))
    }
}

/// [`value_arg`] for every item of `args`.
pub(super) fn value_args(
    ctx: &NativeCallContext,
    args: Vec<Dynamic>,
) -> Result<Vec<ElementProperty>, Box<rhai::EvalAltResult>> {
    let max = access::value_depth_of(ctx);
    args.into_iter()
        .map(|arg| dynamic_to_element_property(arg, max))
        .collect()
}

/// Converts a script value into an [`ElementProperty`], failing with the `script.max_value_depth`
/// error when it nests deeper than `max_depth` levels.
pub(super) fn dynamic_to_element_property(
    arg: Dynamic,
    max_depth: usize,
) -> Result<ElementProperty, Box<rhai::EvalAltResult>> {
    convert_bounded(arg, max_depth).map_err(|TooDeep| value_too_deep_error(max_depth))
}

/// `true` when a script value nests deeper than `levels` levels of arrays/maps. The recursion
/// stops one level past `levels`, so it is bounded by the limit, not by the value.
pub(super) fn dynamic_exceeds_depth(value: &Dynamic, levels: usize) -> bool {
    if value.is_array()
        && let Some(items) = value.read_lock::<rhai::Array>()
    {
        return match levels.checked_sub(1) {
            None => true,
            Some(inner) => items.iter().any(|item| dynamic_exceeds_depth(item, inner)),
        };
    }
    if value.is_map()
        && let Some(map) = value.read_lock::<rhai::Map>()
    {
        return match levels.checked_sub(1) {
            None => true,
            Some(inner) => map.values().any(|item| dynamic_exceeds_depth(item, inner)),
        };
    }
    if let Some(map) = value.read_lock::<ScriptMap>() {
        return crate::value::depth::property_exceeds_depth(&map.0.clone().into_property(), levels);
    }
    false
}

/// A script value nested deeper than the limit (the conversion stopped there).
struct TooDeep;

/// The conversion behind [`dynamic_to_element_property`]: `levels` is how many more levels of
/// arrays/maps may nest at this position, so the recursion never goes deeper than the limit.
fn convert_bounded(arg: Dynamic, levels: usize) -> Result<ElementProperty, TooDeep> {
    Ok(if arg.is_string() {
        ElementProperty::String(dynamic_to_compact_string(arg))
    } else if arg.is_float() {
        if let Ok(val) = arg.as_float() {
            ElementProperty::Number(Number::from_f64(val))
        } else {
            ElementProperty::String(CompactString::new(dynamic_text(&arg)))
        }
    } else if arg.is_int() {
        if let Ok(val) = arg.as_int() {
            ElementProperty::Number(Number::from_i64(val))
        } else {
            ElementProperty::String(CompactString::new(dynamic_text(&arg)))
        }
    } else if arg.is_bool() {
        if let Ok(val) = arg.as_bool() {
            ElementProperty::Boolean(val)
        } else {
            ElementProperty::Boolean(false)
        }
    } else if arg.is_array() {
        let inner = levels.checked_sub(1).ok_or(TooDeep)?;
        let arr = arg.cast::<rhai::Array>();
        ElementProperty::Array(
            arr.into_iter()
                .map(|item| convert_bounded(item, inner))
                .collect::<Result<_, _>>()?,
        )
    } else if arg.is_map() {
        let inner = levels.checked_sub(1).ok_or(TooDeep)?;
        let map = arg.cast::<rhai::Map>();
        ElementProperty::Object(Box::new(
            map.into_iter()
                .map(|(key, value)| {
                    Ok((
                        CompactString::new(key.as_str()),
                        convert_bounded(value, inner)?,
                    ))
                })
                .collect::<Result<_, _>>()?,
        ))
    } else if arg.is_unit() {
        ElementProperty::Null
    } else if let Some(uuid) = arg.read_lock::<ScriptUuid>() {
        ElementProperty::Uuid(uuid.0)
    } else if let Some(value) = dynamic_temporal(&arg) {
        ElementProperty::Temporal(value)
    } else if let Some(vector) = arg.read_lock::<ScriptVector>() {
        ElementProperty::Vector(vector.0.clone())
    } else if let Some(vertex) = arg.read_lock::<ScriptVertex>() {
        ElementProperty::Vertex(Box::new(vertex.0.clone()))
    } else if let Some(edge) = arg.read_lock::<ScriptEdge>() {
        ElementProperty::Edge(Box::new(edge.0.clone()))
    } else if let Some(vertex) = arg.read_lock::<ScriptVertexRef>() {
        // An endpoint stands for its vertex: id and label, no properties.
        ElementProperty::Vertex(Box::new(crate::value::VertexValue::new(
            vertex.0.id(),
            vertex
                .0
                .label()
                .map(CompactString::new)
                .into_iter()
                .collect(),
            crate::value::PropertyMap::default(),
        )))
    } else if let Some(map) = arg.read_lock::<ScriptMap>() {
        // An engine map value: already a property, measured without recursion.
        let property = map.0.clone().into_property();
        if crate::value::depth::property_exceeds_depth(&property, levels) {
            return Err(TooDeep);
        }
        property
    } else if let Some(token) = token_placeholder(&arg) {
        ElementProperty::String(token)
    } else {
        ElementProperty::String(CompactString::new(dynamic_text(&arg)))
    })
}

/// The string a `Merge`/`Direction`/`Cardinality` token becomes when it is used as a *value*
/// (in a map, `inject()`, `constant()`, ...).
///
/// `Merge.outV`/`Merge.inV` become the reserved placeholders `"Merge.outV"`/`"Merge.inV"`, which
/// the merge steps recognize under a `Direction.*` key. Every other token (and a
/// `Cardinality.single(v)` wrapper) has no value meaning; since [`dynamic_to_element_property`]
/// is infallible, it is kept as its Gremlin spelling (`"Direction.OUT"`, `"Merge.onCreate"`,
/// `"Cardinality.single"`, `"Cardinality.single(5)"`) rather than an opaque Rust type name, so
/// the result is at least recognizable. Returns `None` for any other value.
pub(super) fn token_placeholder(arg: &Dynamic) -> Option<CompactString> {
    if let Some(merge) = arg.read_lock::<Merge>() {
        Some(compact_str::format_compact!("Merge.{}", *merge))
    } else if let Some(direction) = arg.read_lock::<Direction>() {
        Some(compact_str::format_compact!("Direction.{}", *direction))
    } else if let Some(cardinality) = arg.read_lock::<Cardinality>() {
        Some(compact_str::format_compact!("Cardinality.{}", *cardinality))
    } else if let Some(operator) = arg.read_lock::<Operator>() {
        Some(compact_str::format_compact!("{}", *operator))
    } else if let Some(barrier) = arg.read_lock::<Barrier>() {
        Some(compact_str::format_compact!("{}", *barrier))
    } else {
        arg.read_lock::<CardinalityValue>().map(|wrapped| {
            compact_str::format_compact!("Cardinality.{}({})", wrapped.cardinality, wrapped.value)
        })
    }
}

/// Converts a Rhai value produced by `#{...}` / array literals into a raw [`serde_json::Value`]
/// for [`ScriptGraphTraversalSource::property_json`].
///
/// This mirrors [`dynamic_to_element_property`], but targets JSON directly rather than
/// round-tripping through [`ElementProperty`], since `property_json` folds nested arrays/objects
/// itself via `json_to_element_property`. `levels` is how many more levels of arrays/maps may
/// nest (the engine's value-depth limit `max` at the top); a deeper value fails with the
/// `script.max_value_depth` error instead of recursing further.
#[cfg(feature = "serde")]
pub(super) fn dynamic_to_json_value(
    arg: Dynamic,
    levels: usize,
    max: usize,
) -> Result<serde_json::Value, Box<rhai::EvalAltResult>> {
    let inner = || {
        levels
            .checked_sub(1)
            .ok_or_else(|| value_too_deep_error(max))
    };
    Ok(if arg.is_unit() {
        serde_json::Value::Null
    } else if arg.is_bool() {
        serde_json::Value::Bool(arg.as_bool().unwrap_or(false))
    } else if arg.is_int() {
        serde_json::Value::from(arg.as_int().unwrap_or_default())
    } else if arg.is_float() {
        let float = arg.as_float().unwrap_or_default();
        serde_json::Number::from_f64(float)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null)
    } else if arg.is_string() {
        serde_json::Value::String(dynamic_to_compact_string(arg).to_string())
    } else if arg.is_array() {
        let inner = inner()?;
        let arr = arg.cast::<rhai::Array>();
        serde_json::Value::Array(
            arr.into_iter()
                .map(|item| dynamic_to_json_value(item, inner, max))
                .collect::<Result<_, _>>()?,
        )
    } else if arg.is_map() {
        let inner = inner()?;
        let map = arg.cast::<rhai::Map>();
        let object: serde_json::Map<String, serde_json::Value> = map
            .into_iter()
            .map(|(key, value)| Ok((key.to_string(), dynamic_to_json_value(value, inner, max)?)))
            .collect::<Result<_, Box<rhai::EvalAltResult>>>()?;
        serde_json::Value::Object(object)
    } else if let Some(uuid) = arg.read_lock::<ScriptUuid>() {
        // JSON is the physical level and has no UUID type: the canonical string goes in, and a
        // field declared `uuid` in `Open`/`Closed` mode is coerced back to a `Uuid` on write.
        serde_json::Value::String(uuid.canonical())
    } else if let Some(value) = dynamic_temporal(&arg) {
        // The same for a temporal value: its canonical text (coerced back by a declared
        // temporal format).
        serde_json::Value::String(value.to_string())
    } else if let Some(vector) = arg.read_lock::<ScriptVector>() {
        // A vector is the list of its values (coerced back by a declared vector).
        serde_json::Value::Array(
            vector
                .0
                .iter_f32()
                .filter_map(|value| {
                    serde_json::Number::from_f64(crate::value::vector::tagged::short_f64(value))
                        .map(serde_json::Value::Number)
                })
                .collect(),
        )
    } else if let Some(map) = arg.read_lock::<ScriptMap>() {
        // JSON objects are string-keyed by definition: a map with a non-string key has no JSON
        // property form (a `ScriptMap` only exists when a key is not a string).
        let inner = inner()?;
        let mut object = serde_json::Map::new();
        for (key, value) in map.0.iter() {
            match key.as_str() {
                Some(name) => {
                    object.insert(
                        name.to_string(),
                        dynamic_to_json_value(
                            element_property_to_dynamic(value.clone()),
                            inner,
                            max,
                        )?,
                    );
                }
                None => {
                    return Err(script_error_from_script(
                        crate::error::ScriptError::ArgumentMismatch {
                            step_name: "property_json".to_string(),
                            expected: "JSON-compatible values (objects have string keys)"
                                .to_string(),
                            actual: format!("a map with the non-string key {}", map_key_text(key)),
                        },
                    ));
                }
            }
        }
        serde_json::Value::Object(object)
    } else {
        serde_json::Value::String(dynamic_text(&arg))
    })
}

fn dynamic_to_pvalue(arg: &Dynamic) -> PValue {
    if arg.is_string() {
        PValue::String(dynamic_to_string(arg.clone()).into())
    } else if arg.is_float() {
        if let Ok(val) = arg.as_float() {
            PValue::Number(Number::from_f64(val))
        } else {
            PValue::String(dynamic_text(arg).into())
        }
    } else if arg.is_int() {
        if let Ok(val) = arg.as_int() {
            PValue::Number(Number::from_i64(val))
        } else {
            PValue::String(dynamic_text(arg).into())
        }
    } else if arg.is_bool() {
        if let Ok(val) = arg.as_bool() {
            PValue::Boolean(val)
        } else {
            PValue::Boolean(false)
        }
    } else if let Some(uuid) = arg.read_lock::<ScriptUuid>() {
        PValue::Uuid(uuid.0)
    } else if let Some(value) = dynamic_temporal(arg) {
        PValue::Temporal(value)
    } else if let Some(vector) = arg.read_lock::<ScriptVector>() {
        PValue::Vector(vector.0.clone())
    } else if arg.is_unit() {
        // `()` is the DSL's `null` (`P.eq(())` is TinkerPop's `P.eq(null)`).
        PValue::Null
    } else {
        PValue::String(dynamic_text(arg).into())
    }
}

fn vec_dynamic_to_pvalue(arg: &[Dynamic]) -> Vec<PValue> {
    arg.iter().map(dynamic_to_pvalue).collect()
}

/// The operand of `P.and/or/not`: it must be a predicate. A scalar or a traversal fails with
/// `ScriptError::ArgumentMismatch` (its `help()` names the predicate and traversal combinators).
pub(super) fn predicate_operand(
    step_name: &str,
    arg: Dynamic,
) -> Result<ArgP, Box<rhai::EvalAltResult>> {
    match arg.clone().try_cast::<ArgP>() {
        Some(p) => Ok(p),
        None => Err(Box::new(rhai::EvalAltResult::ErrorRuntime(
            Dynamic::from(crate::error::format_diagnostic(
                &crate::error::ScriptError::ArgumentMismatch {
                    step_name: step_name.to_string(),
                    expected: "a predicate (P.gt(1), TextP.containing(\"a\"), ...)".to_string(),
                    actual: type_name_of(&arg),
                },
            )),
            rhai::Position::NONE,
        ))),
    }
}

/// [`read_argp`] with every operand checked ([`check_operands`]): what a step comparing values
/// takes (`has(key, P)`, `is(P)`, `where(P)`, ...).
pub(super) fn try_dynamic_to_argp(arg: &Dynamic) -> Result<ArgP, Box<rhai::EvalAltResult>> {
    let predicate = read_argp(arg)?;
    check_operands(&predicate)?;
    Ok(predicate)
}

/// A single value the engine compares a predicate operand as (issue #14): a string, a number,
/// a boolean, a character, `()`, a UUID, a date or time, a vector. Anything else (a list, a map,
/// a traversal, ...) used to be compared as its text, which matches by accident or never.
fn is_operand_value(value: &Dynamic) -> bool {
    value.is_string()
        || value.is_int()
        || value.is_float()
        || value.is_bool()
        || value.is_char()
        || value.is_unit()
        || value.is::<ScriptUuid>()
        || is_temporal(value)
        || value.is::<ScriptVector>()
}

/// Refuses a predicate with an operand that is not a single value ([`is_operand_value`]),
/// naming the predicate: `P.gt(["a"])` fails instead of comparing with the text `["a"]`. The
/// items of `P.within([..])` / `P.without([..])` are its operands; `and`/`or`/`not` are checked
/// through.
pub(super) fn check_operands(predicate: &ArgP) -> Result<(), Box<rhai::EvalAltResult>> {
    let check = |name: &str, operands: &[&Dynamic]| match operands
        .iter()
        .find(|value| !is_operand_value(value))
    {
        None => Ok(()),
        Some(value) => Err(script_error_from_script(
            crate::error::ScriptError::ArgumentMismatch {
                step_name: name.to_string(),
                expected: "a single value to compare with (a string, number, boolean, \
                               date or time, UUID, vector or ())"
                    .to_string(),
                actual: type_name_of(value),
            },
        )),
    };
    match predicate {
        ArgP::Eq(value) => check("P.eq", &[value]),
        ArgP::Neq(value) => check("P.neq", &[value]),
        ArgP::Lt(value) => check("P.lt", &[value]),
        ArgP::Gt(value) => check("P.gt", &[value]),
        ArgP::Lte(value) => check("P.lte", &[value]),
        ArgP::Gte(value) => check("P.gte", &[value]),
        ArgP::WithIn(values) => check("P.within", &values.iter().collect::<Vec<_>>()),
        ArgP::WithOut(values) => check("P.without", &values.iter().collect::<Vec<_>>()),
        ArgP::Inside(low, high) => check("P.inside", &[low, high]),
        ArgP::Outside(low, high) => check("P.outside", &[low, high]),
        ArgP::Between(low, high) => check("P.between", &[low, high]),
        ArgP::StartingWith(value) => check("TextP.starting_with", &[value]),
        ArgP::NotStartingWith(value) => check("TextP.not_starting_with", &[value]),
        ArgP::EndingWith(value) => check("TextP.ending_with", &[value]),
        ArgP::NotEndingWith(value) => check("TextP.not_ending_with", &[value]),
        ArgP::Containing(value) => check("TextP.containing", &[value]),
        ArgP::NotContaining(value) => check("TextP.not_containing", &[value]),
        ArgP::Regex(_) | ArgP::NotRegex(_) | ArgP::TypeOf(_) => Ok(()),
        ArgP::And(left, right) | ArgP::Or(left, right) => {
            check_operands(left)?;
            check_operands(right)
        }
        ArgP::Not(inner) => check_operands(inner),
    }
}

/// The predicate `arg` holds, or a bare value as `P.eq(value)`; its operands unchecked.
fn read_argp(arg: &Dynamic) -> Result<ArgP, Box<rhai::EvalAltResult>> {
    if let Some(p) = arg.clone().read_lock::<ArgP>() {
        Ok((*p).clone())
    } else {
        let type_name = arg.type_name();
        match type_name {
            "string" | "bool" | "i64" | "f64" => Ok(ArgP::Eq(arg.clone())),
            _ if arg.is::<ScriptUuid>() || is_temporal(arg) || arg.is::<ScriptVector>() => {
                Ok(ArgP::Eq(arg.clone()))
            }
            _ => Err(Box::new(rhai::EvalAltResult::ErrorMismatchDataType(
                "a predicate (P.eq(1), ...) or a value".to_string(),
                type_name_of(arg),
                rhai::Position::NONE,
            ))),
        }
    }
}

pub(super) fn try_dynamic_to_by(arg: Dynamic) -> Result<ArgBy, Box<rhai::EvalAltResult>> {
    // `.by(Column.keys)` / `.by(Column.values)` — an `order()` sort key.
    if let Some(column) = arg.clone().try_cast::<Column>() {
        return Ok(ArgBy::Column(column));
    }
    // `.by(jpath("a.b[1]"))`: a path key reads the value at the path.
    if let Some(path) = arg.read_lock::<crate::value::JsonPath>() {
        return Ok(ArgBy::JsonPath(path.clone()));
    }
    let type_name = arg.type_name().to_string();
    match type_name.as_str() {
        "string" | "bool" | "i64" | "f64" => Ok(ArgBy::Property(dynamic_to_compact_string(arg))),
        t if t == std::any::type_name::<ArgBy>() => {
            if let Some(by) = arg.clone().read_lock::<ArgBy>() {
                match &*by {
                    ArgBy::Property(prop) => Ok(ArgBy::Property(prop.clone())),
                    ArgBy::Label => Ok(ArgBy::Label),
                    ArgBy::Id => Ok(ArgBy::Id),
                    ArgBy::Value => Ok(ArgBy::Value),
                    ArgBy::Key => Ok(ArgBy::Key),
                    ArgBy::Count => Ok(ArgBy::Count),
                    ArgBy::Traversal(val) => Ok(ArgBy::Traversal(val.clone())),
                    ArgBy::JsonPath(path) => Ok(ArgBy::JsonPath(path.clone())),
                    ArgBy::Order(order) => Ok(ArgBy::Order(order.clone())),
                    ArgBy::Labels(labels) => Ok(ArgBy::Labels(labels.clone())),
                    ArgBy::Column(column) => Ok(ArgBy::Column(*column)),
                }
            } else {
                Err(Box::new(rhai::EvalAltResult::ErrorMismatchDataType(
                    "By".to_string(),
                    super::register::display_type_name(&type_name),
                    rhai::Position::NONE,
                )))
            }
        }
        t if t == std::any::type_name::<ScriptGraphTraversalSource>() => {
            if let Some(val) = arg.clone().read_lock::<ScriptGraphTraversalSource>() {
                Ok(ArgBy::Traversal(val.clone()))
            } else {
                Err(Box::new(rhai::EvalAltResult::ErrorMismatchDataType(
                    "GraphTraversalSource".to_string(),
                    super::register::display_type_name(&type_name),
                    rhai::Position::NONE,
                )))
            }
        }
        // `.by(Order.asc)`/`.by(Order.desc)` — a direction-only modulator, for `order()`.
        t if t == std::any::type_name::<Order>() => {
            if let Some(order) = arg.clone().read_lock::<Order>() {
                Ok(ArgBy::Order(order.clone()))
            } else {
                Err(Box::new(rhai::EvalAltResult::ErrorMismatchDataType(
                    "Order".to_string(),
                    super::register::display_type_name(&type_name),
                    rhai::Position::NONE,
                )))
            }
        }
        // `.by(["directory", "file"])` — a list of vertex labels.
        "array" => {
            let items = arg.into_array().map_err(|actual| {
                Box::new(rhai::EvalAltResult::ErrorMismatchDataType(
                    "array".to_string(),
                    super::register::display_type_name(actual),
                    rhai::Position::NONE,
                ))
            })?;
            let mut labels = Vec::with_capacity(items.len());
            for item in items {
                if item.is_string() {
                    labels.push(dynamic_to_compact_string(item));
                } else {
                    return Err(by_labels_mismatch(&type_name_of(&item)));
                }
            }
            Ok(ArgBy::Labels(labels))
        }
        _ => Err(Box::new(rhai::EvalAltResult::ErrorMismatchDataType(
            "a property key string, a list of label strings, a By token, a Column, an Order or a traversal"
                .to_string(),
            super::register::display_type_name(&type_name),
            rhai::Position::NONE,
        ))),
    }
}

/// Converts `value_map()`'s arguments into property keys and the token flag.
///
/// A boolean is accepted only as the first argument (`valueMap(true, "a")`; `true` adds `id` and
/// `label`). A null (`()`) is dropped wherever it stands: TinkerPop's null key matches no property,
/// so `valueMap(null)` alone equals `valueMap()` (all keys). Every other argument is rejected with
/// `ScriptError::ArgumentMismatch` instead of being stringified into a key that never matches.
pub(super) fn value_map_keys(
    step_name: &str,
    args: Vec<Dynamic>,
) -> Result<(Vec<PropKey>, bool), Box<rhai::EvalAltResult>> {
    let mut keys = Vec::with_capacity(args.len());
    let mut tokens = false;
    for (position, arg) in args.into_iter().enumerate() {
        if let Some(key) = arg.read_lock::<rhai::ImmutableString>() {
            keys.push(PropKey::Name(CompactString::new(key.as_str())));
            continue;
        }
        if let Some(path) = arg.read_lock::<crate::value::JsonPath>() {
            keys.push(PropKey::Path(path.clone()));
            continue;
        }
        if arg.is_unit() {
            continue;
        }
        if position == 0
            && let Ok(flag) = arg.as_bool()
        {
            tokens = flag;
            continue;
        }
        let actual = if let Ok(flag) = arg.as_bool() {
            format!("a boolean (`{flag}`) that is not the first argument")
        } else if let Some(uuid) = arg.read_lock::<ScriptUuid>() {
            format!("a UUID (`{}`)", uuid.canonical())
        } else {
            format!(
                "a value of type {} (`{}`)",
                type_name_of(&arg),
                dynamic_text(&arg)
            )
        };
        return Err(Box::new(rhai::EvalAltResult::ErrorRuntime(
            Dynamic::from(crate::error::format_diagnostic(
                &crate::error::ScriptError::ArgumentMismatch {
                    step_name: step_name.to_string(),
                    expected: "property keys as strings, optionally preceded by a boolean"
                        .to_string(),
                    actual,
                },
            )),
            rhai::Position::NONE,
        )));
    }
    Ok((keys, tokens))
}

/// Checks the options map of `glob_path(pattern, #{...})`: every key must be a supported option
/// with a value of its type.
pub(super) fn glob_path_options(
    options: rhai::Map,
) -> Result<ScriptGlobPathOptions, Box<rhai::EvalAltResult>> {
    let mismatch = |expected: String, actual: String| {
        Box::new(rhai::EvalAltResult::ErrorRuntime(
            Dynamic::from(crate::error::format_diagnostic(
                &crate::error::ScriptError::ArgumentMismatch {
                    step_name: "glob_path".to_string(),
                    expected,
                    actual,
                },
            )),
            rhai::Position::NONE,
        ))
    };
    let describe = |value: &Dynamic| {
        format!(
            "a value of type {} (`{}`)",
            type_name_of(value),
            dynamic_text(value)
        )
    };
    let mut parsed = ScriptGlobPathOptions::default();
    for (key, value) in options {
        match key.as_str() {
            "max_depth" => match value.as_int() {
                Ok(depth) => parsed.max_depth = Some(depth),
                Err(_) => {
                    return Err(mismatch("max_depth as an integer, for example #{max_depth: 3}".to_string(), describe(&value)))
                }
            },
            "prune" => match value.read_lock::<ScriptGraphTraversalSource>() {
                Some(prune) => parsed.prune = Some(prune.clone()),
                None => {
                    return Err(mismatch(
                        "prune as an anonymous traversal, for example #{prune: __.has(\"name\", \"target\")}".to_string(),
                        describe(&value),
                    ))
                }
            },
            reserved if GLOB_PATH_RESERVED_KEYS.contains(&reserved) => {
                return Err(mismatch(
                    format!("an options map with the keys {GLOB_PATH_OPTION_KEYS}"),
                    format!("the reserved key `{reserved}`, which is not implemented yet"),
                ))
            }
            unknown => {
                return Err(mismatch(
                    format!("an options map with the keys {GLOB_PATH_OPTION_KEYS}"),
                    format!("the unknown key `{unknown}`"),
                ))
            }
        }
    }
    Ok(parsed)
}

impl ScriptGlobPathOptions {
    /// The Rust options, with the `prune` child converted like every other child traversal.
    pub(super) fn convert_to_anonymous<'g, S: crate::graph::GraphStorage + 'g>(
        &self,
    ) -> crate::GlobPathOptions<'g, S> {
        let mut options = crate::GlobPathOptions::default();
        if let Some(depth) = self.max_depth {
            options = options.max_depth_raw(depth);
        }
        if let Some(prune) = &self.prune {
            options = options.prune(prune.ast_convert_to_anonymous());
        }
        options
    }
}

/// The script error for a `by([..])` label list holding a non-string element.
fn by_labels_mismatch(actual: &str) -> Box<rhai::EvalAltResult> {
    Box::new(rhai::EvalAltResult::ErrorRuntime(
        Dynamic::from(crate::error::format_diagnostic(
            &crate::error::ScriptError::ArgumentMismatch {
                step_name: "by".to_string(),
                expected: "a list of vertex-label strings".to_string(),
                actual: format!("a list with an element of type {actual}"),
            },
        )),
        rhai::Position::NONE,
    ))
}

/// The script error for a `has(T.id|T.label, ..)` call with an unsupported token or value.
pub(super) fn has_t_mismatch(expected: &str, actual: String) -> Box<rhai::EvalAltResult> {
    Box::new(rhai::EvalAltResult::ErrorRuntime(
        Dynamic::from(crate::error::format_diagnostic(
            &crate::error::ScriptError::ArgumentMismatch {
                step_name: "has".to_string(),
                expected: expected.to_string(),
                actual,
            },
        )),
        rhai::Position::NONE,
    ))
}

/// The entries of a DSL `property(map)`: a `Cardinality.x(v)` value becomes `v` with that
/// cardinality; every other value converts through [`dynamic_to_element_property`] (each value
/// at most `max_depth` levels deep).
pub(super) fn property_entries_of(
    map: rhai::Map,
    max_depth: usize,
) -> Result<Vec<PropertyEntry>, Box<rhai::EvalAltResult>> {
    map.into_iter()
        .map(|(key, value)| {
            let (value, cardinality) = match value.read_lock::<CardinalityValue>() {
                Some(wrapped) => (wrapped.value.clone(), Some(wrapped.cardinality)),
                None => (value.clone(), None),
            };
            Ok(PropertyEntry {
                key: CompactString::new(key.as_str()),
                value: dynamic_to_element_property(value, max_depth)?,
                cardinality,
            })
        })
        .collect()
}

/// The argument of `mergeV(..)` / `option(Merge.x, ..)`: a map (entries with their
/// `Cardinality.x(v)` cardinality), `()` (no entries), a traversal, or any other value (rejected
/// by the step before execution). Values are at most `max_depth` levels deep.
pub(super) fn script_merge_arg(
    arg: Dynamic,
    max_depth: usize,
) -> Result<ScriptMergeArg, Box<rhai::EvalAltResult>> {
    if arg.is_map() {
        Ok(ScriptMergeArg::Entries(property_entries_of(
            arg.cast::<rhai::Map>(),
            max_depth,
        )?))
    } else if arg.is_unit() {
        Ok(ScriptMergeArg::Entries(Vec::new()))
    } else if arg.is::<ScriptGraphTraversalSource>() {
        Ok(ScriptMergeArg::Traversal(try_dynamic_to_gts(arg)?))
    } else {
        Ok(ScriptMergeArg::Value(dynamic_to_element_property(
            arg, max_depth,
        )?))
    }
}

pub(super) fn try_dynamic_to_gts(
    arg: Dynamic,
) -> Result<ScriptGraphTraversalSource, Box<rhai::EvalAltResult>> {
    let type_name = arg.type_name().to_string();
    if type_name == std::any::type_name::<ScriptGraphTraversalSource>() {
        if let Some(val) = arg.read_lock::<ScriptGraphTraversalSource>() {
            return Ok(val.clone());
        }
    } else if is_schema_map(&arg) {
        // `__.infer_schema()` on the empty anonymous source evaluates eagerly to a schema map;
        // as a traversal argument it is the `infer_schema()` step.
        let mut gts = ScriptGraphSource::empty();
        return Ok(gts.add_step(Step::InferSchema));
    }
    Err(Box::new(rhai::EvalAltResult::ErrorMismatchDataType(
        "a traversal (__.out(), ...)".to_string(),
        super::register::display_type_name(&type_name),
        rhai::Position::NONE,
    )))
}

/// The `ArgumentMismatch` of a malformed `select(Pop, ...)` call (its `help()` shows the forms).
pub(super) fn select_pop_mismatch(actual: String) -> Box<rhai::EvalAltResult> {
    script_error_from_script(crate::error::ScriptError::ArgumentMismatch {
        step_name: "select".to_string(),
        expected: "select(\"a\"), select(Pop.all, \"a\") or select(Column.keys): a Pop first, then step labels as strings"
            .to_string(),
        actual,
    })
}

/// An anonymous one-step traversal, the child that a sugar form of `choose()`/`branch()` expands to.
pub(super) fn one_step_source(step: Step) -> ScriptGraphTraversalSource {
    ScriptGraphTraversalSource {
        g: None,
        steps: vec![step],
        options: Default::default(),
        access: Default::default(),
        private: true,
        anonymous: true,
        now: None,
    }
}

fn choose_mismatch(step_name: &str, expected: &str, arg: &Dynamic) -> Box<rhai::EvalAltResult> {
    script_error_from_script(crate::error::ScriptError::ArgumentMismatch {
        step_name: step_name.to_string(),
        expected: expected.to_string(),
        actual: type_name_of(arg),
    })
}

/// The selector of `choose(selector)` / `branch(selector)`: a traversal, or the `T.label` /
/// `T.id` tokens (TinkerPop's `choose(Function)` with `T`, the same as `__.label()` / `__.id()`).
pub(super) fn selector_to_gts(
    step_name: &str,
    arg: Dynamic,
) -> Result<ScriptGraphTraversalSource, Box<rhai::EvalAltResult>> {
    if arg.is::<ScriptGraphTraversalSource>() {
        return try_dynamic_to_gts(arg);
    }
    match arg.clone().try_cast::<ArgBy>() {
        Some(ArgBy::Label) => Ok(one_step_source(Step::Label)),
        Some(ArgBy::Id) => Ok(one_step_source(Step::Id)),
        _ => Err(choose_mismatch(
            step_name,
            "a traversal, T.label or T.id",
            &arg,
        )),
    }
}

/// The condition of `choose(condition, onTrue[, onFalse])`: a traversal, or a predicate, which
/// is `__.is(predicate)` on the current value (TinkerPop's `choose(Predicate, ..)`).
pub(super) fn predicate_to_gts(
    step_name: &str,
    arg: Dynamic,
) -> Result<ScriptGraphTraversalSource, Box<rhai::EvalAltResult>> {
    if arg.is::<ScriptGraphTraversalSource>() {
        return try_dynamic_to_gts(arg);
    }
    match arg.clone().try_cast::<ArgP>() {
        Some(p) => {
            check_operands(&p)?;
            Ok(one_step_source(Step::Is(p)))
        }
        None => Err(choose_mismatch(
            step_name,
            "a traversal or a predicate (P.gt(1), ...)",
            &arg,
        )),
    }
}

pub(super) fn try_vec_dynamic_to_gts(
    arg: Vec<Dynamic>,
) -> Result<Vec<ScriptGraphTraversalSource>, Box<rhai::EvalAltResult>> {
    arg.into_iter().map(try_dynamic_to_gts).collect()
}

//     match arg {
//         ArgP::Eq(dynamic) => func(P::Eq(dynamic_to_pvalue(dynamic))),
//         ArgP::Neq(dynamic) => func(P::Neq(dynamic_to_pvalue(dynamic))),
//         ArgP::Lt(dynamic) => func(P::Lt(dynamic_to_pvalue(dynamic))),
//         ArgP::Gt(dynamic) => func(P::Gt(dynamic_to_pvalue(dynamic))),
//         ArgP::Lte(dynamic) => func(P::Lte(dynamic_to_pvalue(dynamic))),
//         ArgP::Gte(dynamic) => func(P::Gte(dynamic_to_pvalue(dynamic))),
//         ArgP::WithIn(vec) => func(P::WithIn(vec_dynamic_to_pvalue(vec))),
//         ArgP::WithOut(vec) => func(P::WithOut(vec_dynamic_to_pvalue(vec))),
//         ArgP::Inside(dynamic, dynamic1) => func(P::Inside(dynamic_to_pvalue(dynamic), dynamic_to_pvalue(dynamic1))),
//         ArgP::Outside(dynamic, dynamic1) => func(P::Outside(dynamic_to_pvalue(dynamic), dynamic_to_pvalue(dynamic1))),
//         ArgP::Between(dynamic, dynamic1) => func(P::Between(dynamic_to_pvalue(dynamic), dynamic_to_pvalue(dynamic1))),
//         ArgP::StartingWith(dynamic) => func(P::StartingWith(dynamic_to_pvalue(dynamic))),
//         ArgP::NotStartingWith(dynamic) => func(P::NotStartingWith(dynamic_to_pvalue(dynamic))),
//         ArgP::EndingWith(dynamic) => func(P::EndingWith(dynamic_to_pvalue(dynamic))),
//         ArgP::NotEndingWith(dynamic) => func(P::NotEndingWith(dynamic_to_pvalue(dynamic))),
//         ArgP::Containing(dynamic) => func(P::Containing(dynamic_to_pvalue(dynamic))),
//         ArgP::NotContaining(dynamic) => func(P::NotContaining(dynamic_to_pvalue(dynamic))),
//     }
// }

/// The id of a vertex, an edge or an edge endpoint value (`g.V(v)`, `has_id(e)`); `None` for any
/// other value.
pub(crate) fn element_id_of(arg: &Dynamic) -> Option<CompactString> {
    if let Some(vertex) = arg.read_lock::<ScriptVertex>() {
        Some(CompactString::new(vertex.0.id()))
    } else if let Some(edge) = arg.read_lock::<ScriptEdge>() {
        Some(CompactString::new(edge.0.id()))
    } else {
        arg.read_lock::<ScriptVertexRef>()
            .map(|vertex| CompactString::new(vertex.0.id()))
    }
}
