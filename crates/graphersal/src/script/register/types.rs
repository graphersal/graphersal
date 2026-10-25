//! Registration of the value and graph types that are neither token classes nor steps: the
//! `GraphSource` factory, `JsonPath`, the profiling/execution results, `RenderedText`,
//! `VisualizeFormat` and `GraphSchema`.

use super::Registry;
use crate::script::*;

/// The `GraphSource` factory: the `GraphSource::x()` static module, the bare global
/// functions and the `GraphSource` template type.
pub(super) fn register_graph_source(registry: &mut Registry) {
    let mut ds_module = Module::new();

    // Every factory takes the call context: the graph it builds is private to the script and
    // carries the session's authorizer from the engine tag (`script/access.rs`).
    // The docs (`docs.rs`) of the bare global forms, the same as of `GraphSource::x()`.
    let doc = |registry: &mut Registry, name: &str| {
        FuncRegistration::new(name).with_comments(registry.doc_comments("global", name))
    };
    doc(registry, "empty").set_into_module(&mut ds_module, |ctx: NativeCallContext| {
        ScriptGraphSource::empty_in(&ctx)
    });
    doc(registry, "tinkerpop_modern").set_into_module(&mut ds_module, |ctx: NativeCallContext| {
        ScriptGraphSource::tinkerpop_modern_in(&ctx)
    });
    doc(registry, "file_tree").set_into_module(&mut ds_module, |ctx: NativeCallContext| {
        ScriptGraphSource::file_tree_in(&ctx)
    });
    #[cfg(feature = "io")]
    doc(registry, "file")
        .set_into_module(&mut ds_module, |ctx: NativeCallContext, file: String| {
            ScriptGraphSource::file_in(&ctx, file)
        });

    registry.register_static_module("GraphSource", ds_module.clone().into());
    registry.register_global_module(ds_module.into());

    // Factories, not steps: one spelling on purpose (`tinkerpop_modern`, `file_tree`).
    registry.register_type_with_name::<ScriptGraphSource>("GraphSource");
    registry.register_fn("empty", |ctx: NativeCallContext, _: ScriptGraphSource| {
        ScriptGraphSource::empty_in(&ctx)
    });
    registry.register_fn(
        "tinkerpop_modern",
        |ctx: NativeCallContext, _: ScriptGraphSource| ScriptGraphSource::tinkerpop_modern_in(&ctx),
    );
    registry.register_fn(
        "file_tree",
        |ctx: NativeCallContext, _: ScriptGraphSource| ScriptGraphSource::file_tree_in(&ctx),
    );
    #[cfg(feature = "io")]
    registry.register_fn(
        "file",
        |ctx: NativeCallContext, _: ScriptGraphSource, file: String| {
            ScriptGraphSource::file_in(&ctx, file)
        },
    );
}

/// `jpath(..)`, the profiling and execution results and `RenderedText`.
pub(super) fn register_values(registry: &mut Registry) {
    // `jpath("a.b[1].c")`: a JSONPath property key (RFC 9535 singular query). A plain string stays
    // a literal property name; only this constructor makes a path. An invalid path is a normal
    // script error carrying the `InvalidJsonPath` diagnostic. `jpath` is a function, not a pushed
    // scope global, so it is deliberately not in `RESERVED_PARAM_NAMES` (a parameter named `jpath`
    // merely shadows the call syntax in its own scope, like any user variable).
    registry
        .register_type_with_name::<crate::value::JsonPath>("JsonPath")
        .register_fn(
            "jpath",
            |path: &str| -> Result<crate::value::JsonPath, Box<rhai::EvalAltResult>> {
                path.parse::<crate::value::JsonPath>()
                    .map_err(|err| script_error(Box::new(err)))
            },
        )
        .register_fn_as(
            &["to_string", "to_debug"],
            |path: &mut crate::value::JsonPath| path.to_string(),
        )
        .register_fn(
            "==",
            |a: crate::value::JsonPath, b: crate::value::JsonPath| a == b,
        )
        .register_fn(
            "!=",
            |a: crate::value::JsonPath, b: crate::value::JsonPath| a != b,
        );

    // `TraversalMetrics` (`profile()`) and `Execution` (`execute()`): pure data with getters.
    execution::register(registry);

    // `RenderedText` (`to_table()`, `to_json()`, `visualize()`, ...): prints and compares like the
    // plain string it used to be, but carries its own truncation report instead of a side channel
    // keyed by string identity (see `try_render_value`).
    registry
        .register_type_with_name::<RenderedText>("RenderedText")
        .register_fn("to_string", |v: &mut RenderedText| v.text.to_string())
        .register_fn("to_debug", |v: &mut RenderedText| {
            format!("{:?}", v.as_str())
        })
        .register_fn("==", |a: RenderedText, b: rhai::ImmutableString| {
            a.as_str() == b.as_str()
        })
        .register_fn("==", |a: rhai::ImmutableString, b: RenderedText| {
            a.as_str() == b.as_str()
        })
        .register_fn("==", |a: RenderedText, b: RenderedText| {
            a.as_str() == b.as_str()
        })
        .register_fn("!=", |a: RenderedText, b: rhai::ImmutableString| {
            a.as_str() != b.as_str()
        })
        .register_fn("!=", |a: rhai::ImmutableString, b: RenderedText| {
            a.as_str() != b.as_str()
        })
        .register_fn("!=", |a: RenderedText, b: RenderedText| {
            a.as_str() != b.as_str()
        })
        .register_fn("+", |a: RenderedText, b: rhai::ImmutableString| {
            format!("{}{b}", a.as_str())
        })
        .register_fn("+", |a: rhai::ImmutableString, b: RenderedText| {
            format!("{a}{}", b.as_str())
        })
        .register_fn("+", |a: RenderedText, b: RenderedText| {
            format!("{}{}", a.as_str(), b.as_str())
        });
}

/// `VisualizeFormat` and the schema meta-graph functions.
///
/// A schema is a plain map in the script (its canonical format, from `get_schema()` /
/// `infer_schema()`); the session visualizer renders it (`--format`), so it has no rendering
/// methods. The meta-graph functions take such a map (or any map in the schema format).
pub(super) fn register_schema(registry: &mut Registry) {
    registry.register_type_with_name::<VisualizeFormat>("VisualizeFormat");

    fn loaded(
        ctx: &NativeCallContext,
        map: &mut rhai::Map,
    ) -> Result<GraphSchema, Box<rhai::EvalAltResult>> {
        schema_arg(ctx, Dynamic::from_map(map.clone()))
    }

    registry
        // The meta-graphs are private to the script (`script/access.rs`).
        .register_fn_as(
            &["to_graph"],
            |ctx: NativeCallContext, s: &mut rhai::Map| {
                let schema = loaded(&ctx, s)?;
                Ok::<_, Box<rhai::EvalAltResult>>(ScriptGraphSource::private_graph_in(
                    &ctx,
                    schema.to_graph(),
                ))
            },
        )
        // Schema functions, not steps: one spelling on purpose.
        .register_fn(
            "subgraph",
            |ctx: NativeCallContext, s: &mut rhai::Map, label: String| {
                let schema = loaded(&ctx, s)?;
                Ok::<_, Box<rhai::EvalAltResult>>(ScriptGraphSource::private_graph_in(
                    &ctx,
                    schema.subgraph(&label).to_graph(),
                ))
            },
        )
        .register_fn(
            "subgraph_labels",
            |ctx: NativeCallContext, s: &mut rhai::Map, labels: Vec<Dynamic>| {
                let schema = loaded(&ctx, s)?;
                let label_strings = vec_dynamic_to_string(labels);
                Ok::<_, Box<rhai::EvalAltResult>>(ScriptGraphSource::private_graph_in(
                    &ctx,
                    schema.subgraph_labels(&label_strings).to_graph(),
                ))
            },
        );
}
