// Layout of the script DSL (a plain comment, so the public module docs stay as they are):
//
// - `eval.rs`: `graph_scope*`, the `eval_*` entry points, named/positional parameters and
//   `RESERVED_PARAM_NAMES`.
// - `engine.rs`: `engine()`/`engine_with_limits()` (limits, expression depths, keyword-token
//   rewriting) and `registered_step_names()`.
// - `register/`: the Rhai registrations by category (`types.rs`, `tokens.rs`, `terminals.rs`,
//   `steps.rs`) and the `register_variadic!` macros, collected by `registry.rs` into modules
//   built once per thread and attached to every engine (`new_engine`); `docs.rs` is the doc
//   text (one sentence + a checked example) of every function, attached at registration.
// - `tokens/`: one file per token class (scope marker, static module, script value type).
// - `step.rs`: the step AST `Step`.
// - `source/`: `ScriptGraphSource` and `ScriptGraphTraversalSource` (`mod.rs`) with its step
//   builder methods split by step family (`side_effects.rs`, `filter_map.rs`, `navigation.rs`).
// - `connective.rs`: folds the infix `a.and().b` / `a.or().b` into `and()`/`or()` before lowering.
// - `convert.rs`: `Step` -> engine steps, one lowering (`lower`) behind `ast_convert_to_gts` and
//   `ast_convert_to_anonymous`.
// - `terminals.rs`: `profile`/`execute`, the visualizers, `to_list`/`next`/`to_graph`.
// - `mutating.rs`: `is_mutating()`.
// - `plan.rs`: the script group of `AnyGraph` (`ScriptPlan`/`Terminal`/`TerminalOutput`,
//   `run_plan_on`, the whole-script unit `run_unit_on`) and the scratch graph of a source
//   without one.
// - `saved_query.rs`: saved queries (`define_query`, `query`, `queries`, ...): the access a
//   call runs under (read-only, or the inferred effects), the compiled-body cache, argument
//   binding; `effects.rs` infers a body's effects from its AST.
// - `compression.rs`: compression rules (`define_compression`, `compressions`,
//   `drop_compression`, `recompress`).
// - `property_index.rs`: property indexes (`create_index`, `indexes`, `drop_index`).
// - `memory_report.rs`: `memory_report` (the per-label, per-property memory report).
// - `dynamic.rs`: `Dynamic` conversions and the shared argument diagnostics.
// - `render.rs`: `render_value`/`try_render_value`/`debug_value`, display limits, `RenderedText`.
// - `execution.rs`, `limits.rs`: `ScriptExecution`/metrics, `ScriptLimits`.
// - `surface_tests.rs`: the snapshot of the registered surface
//   (`tests/snapshots/script_surface.txt`).
//
// Child modules import this module's namespace (`use super::*` / `use crate::script::*`); the
// public items are re-exported below under their `graphersal::script::X` paths.

use crate::value::PropKey;
use compact_str::CompactString;
use core::fmt;
use std::sync::Arc;

use rhai::plugin::*;
use rhai::{Dynamic, Engine, FuncRegistration, LexError, Module, Scope, Token};

/// Re-export of the scripting engine crate, so embedders can work with the values that
/// [`eval_value`] returns without pinning their own `rhai` version.
pub use rhai;

mod access;
mod compression;
mod connective;
mod convert;
mod dynamic;
mod effects;
#[cfg(test)]
mod effects_tests;
mod engine;
mod eval;
mod execution;
mod fake;
mod limits;
mod memory_report;
mod mutating;
pub(crate) mod plan;
mod property_index;
mod register;
mod render;
mod saved_query;
#[cfg(feature = "gql")]
mod saved_query_gql;
mod source;
mod step;
#[cfg(all(test, feature = "io", feature = "serde"))]
mod surface_tests;
mod terminals;
#[cfg(test)]
mod tests;
mod tokens;
pub use access::{pin_now, set_engine_embedders, set_scope_embedders};
pub use dynamic::script_value_to_property;
use dynamic::*;
pub use engine::{engine, engine_with_limits, engine_with_options, registered_step_names};
pub use eval::{
    eval, eval_value, eval_value_atomic, eval_value_dry_run, eval_value_with_limits,
    eval_value_with_params, eval_value_with_positional_params, eval_with_params,
    eval_with_positional_params, graph_scope, graph_scope_with_options, map_eval_error,
    materialize_traversals,
};
pub use execution::{ScriptExecution, metrics_to_map};
pub use limits::{ScriptLimits, resource_limit_from_rhai};
#[doc(hidden)]
pub use register::doc_examples;
use render::*;
pub use render::{
    RENDER_MAX_ITEMS, RENDER_MAX_ROWS, RENDER_SPELLING, RenderedText, debug_value,
    render_scope_options, render_value, schema_of_value, set_scope_render_options,
    try_render_value,
};
pub use saved_query::{
    SavedQueryEffects, check_saved_query, saved_query_effects, with_inferred_dialect,
};
pub(crate) use source::ScriptGraphTarget;
pub use source::{ScriptGraphSource, ScriptGraphTraversalSource};
use step::*;
use tokens::*;
pub use tokens::{
    ArgBy, ArgP, Pick, ScriptBarrier, ScriptBy, ScriptCardinality, ScriptCastPolicy, ScriptColumn,
    ScriptDT, ScriptDirection, ScriptEdge, ScriptGType, ScriptMap, ScriptMerge, ScriptOperator,
    ScriptOrder, ScriptP, ScriptPick, ScriptPop, ScriptProfileType, ScriptSchemaMode, ScriptScope,
    ScriptSet, ScriptSetTokens, ScriptT, ScriptTextP, ScriptUuid, ScriptUuidTokens, ScriptV,
    ScriptVector, ScriptVertex, ScriptVertexRef, ScriptWithOptions,
};

use crate::graph::schema::{GraphSchema, SchemaMode, VisualizeFormat};
use crate::graph::{AnyGraph, IntoAnyGraph};
use crate::traversal::render::{RenderOptions, RenderedOutput, Truncation};
use crate::traversal::step::flatmap_steps::step_choose::OptionToken;
use crate::traversal::step::map_steps::list_function::{ListArg, ListFunction};
use crate::traversal::step::sideeffect_steps::step_property_map::PropertyEntry;
use crate::value::Scope as TraversalScope;
use crate::value::{Barrier, Cardinality, Direction, Merge, Operator, Pop};
use crate::{
    AnonymousTraversal, ArgStrings, By, CastPolicy, Column, ElementProperty, GType, Graph,
    GraphSource, GraphStorage, GraphTraversalSource, Number, Order, P, PValue, ProfileType,
    TraversalGraph,
};
