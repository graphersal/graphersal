//! Registration of the `GraphTraversalSource` type with its terminal steps, visualizers,
//! options and graph I/O methods.

use super::Registry;
use crate::script::*;

/// Registers the terminal steps.
pub(super) fn register(registry: &mut Registry) {
    registry
        .register_type_with_name::<ScriptGraphTraversalSource>("GraphTraversalSource")
        // An anonymous traversal prints as its DSL text (`__.out()`); a traversal of a graph
        // keeps its type name (it is shown by running it, not as text).
        .register_fn_as(
            &["to_string", "to_debug"],
            |traversal: &mut ScriptGraphTraversalSource| {
                if traversal.anonymous {
                    crate::value::literal::TokenLiteral::token_literal(&*traversal)
                } else {
                    "GraphTraversalSource".to_string()
                }
            },
        )
        .register_fn("with", ScriptGraphTraversalSource::with)
        .register_fn("with", ScriptGraphTraversalSource::with_option_flag)
        .register_fn("with", ScriptGraphTraversalSource::with_option_value)
        .register_fn_as(
            &["memory_usage", "memoryUsage"],
            ScriptGraphTraversalSource::memory_usage,
        )
        .register_fn_as(
            &["memory_report", "memoryReport"],
            ScriptGraphTraversalSource::memory_report,
        )
        .register_fn_as(
            &["memory_report", "memoryReport"],
            ScriptGraphTraversalSource::memory_report_with,
        )
        // One word, so one spelling serves both conventions.
        .register_fn("statistics", ScriptGraphTraversalSource::statistics)
        .register_fn("embedders", ScriptGraphTraversalSource::embedders)
        .register_fn_as(
            &["embed_property", "embedProperty"],
            ScriptGraphTraversalSource::embed_property,
        )
        // One word, so one spelling serves both conventions.
        .register_fn("mark", ScriptGraphTraversalSource::mark)
        .register_fn_as(&["to_list", "toList"], ScriptGraphTraversalSource::to_list)
        .register_fn("next", ScriptGraphTraversalSource::next)
        // One word, so one spelling serves both conventions.
        .register_fn("iterate", ScriptGraphTraversalSource::iterate)
        .register_fn_as(
            &["to_graph", "toGraph"],
            ScriptGraphTraversalSource::to_graph,
        )
        .register_fn("profile", ScriptGraphTraversalSource::profile)
        .register_fn("profile", ScriptGraphTraversalSource::profile_with)
        // `profile_with` is also registered under its own Rust-API name, as a convenience alias
        // for a query ported straight from the Rust API's `.profile_with(flags)` spelling — the
        // DSL-parity goal (rule 2) is that either spelling transfers unmodified.
        .register_fn("profile_with", ScriptGraphTraversalSource::profile_with)
        .register_fn("execute", ScriptGraphTraversalSource::execute)
        .register_fn("execute", ScriptGraphTraversalSource::execute_dynamic)
        .register_fn("visualize", ScriptGraphTraversalSource::visualize_default)
        .register_fn("visualize", ScriptGraphTraversalSource::visualize)
        .register_fn("visualize", ScriptGraphTraversalSource::visualize_dynamic)
        .register_fn_as(
            &["to_table", "toTable"],
            ScriptGraphTraversalSource::to_table,
        )
        .register_fn_as(
            &["to_table", "toTable"],
            ScriptGraphTraversalSource::to_table_dynamic,
        )
        .register_fn_as(
            &["to_markdown", "toMarkdown"],
            ScriptGraphTraversalSource::to_markdown,
        )
        .register_fn_as(
            &["to_markdown", "toMarkdown"],
            ScriptGraphTraversalSource::to_markdown_dynamic,
        )
        .register_fn_as(
            &["to_mermaid", "toMermaid"],
            ScriptGraphTraversalSource::to_mermaid,
        )
        .register_fn_as(
            &["to_mermaid", "toMermaid"],
            ScriptGraphTraversalSource::to_mermaid_dynamic,
        )
        .register_fn_as(
            &["to_plantuml", "toPlantuml", "toPlantUML"],
            ScriptGraphTraversalSource::to_plantuml,
        )
        .register_fn_as(
            &["to_plantuml", "toPlantuml", "toPlantUML"],
            ScriptGraphTraversalSource::to_plantuml_dynamic,
        )
        .register_fn_as(
            &["to_json_schema", "toJsonSchema"],
            ScriptGraphTraversalSource::to_json_schema,
        )
        .register_fn_as(
            &["to_json_schema", "toJsonSchema"],
            ScriptGraphTraversalSource::to_json_schema_dynamic,
        )
        .register_fn_as(&["to_json", "toJson"], ScriptGraphTraversalSource::to_json)
        .register_fn_as(
            &["to_json", "toJson"],
            ScriptGraphTraversalSource::to_json_dynamic,
        )
        .register_fn_as(&["to_tree", "toTree"], ScriptGraphTraversalSource::to_tree)
        .register_fn_as(
            &["to_tree", "toTree"],
            ScriptGraphTraversalSource::to_tree_dynamic,
        )
        // The seven schema methods: plain calls on the source, not steps (like `statistics`),
        // except `infer_schema`, which after steps is the step of the same name.
        .register_fn_as(
            &["get_schema", "getSchema"],
            ScriptGraphTraversalSource::get_schema,
        )
        .register_fn_as(
            &["set_schema", "setSchema"],
            ScriptGraphTraversalSource::set_schema,
        )
        .register_fn_as(
            &["set_schema", "setSchema"],
            ScriptGraphTraversalSource::set_schema_with_mode,
        )
        .register_fn_as(
            &["patch_schema", "patchSchema"],
            ScriptGraphTraversalSource::patch_schema,
        )
        .register_fn_as(
            &["validate_schema", "validateSchema"],
            ScriptGraphTraversalSource::validate_schema,
        )
        .register_fn_as(
            &["validate_schema", "validateSchema"],
            ScriptGraphTraversalSource::validate_schema_with_mode,
        )
        .register_fn_as(
            &["validate_schema_patch", "validateSchemaPatch"],
            ScriptGraphTraversalSource::validate_schema_patch,
        )
        .register_fn_as(
            &["infer_schema", "inferSchema"],
            ScriptGraphTraversalSource::infer_schema,
        )
        .register_fn_as(
            &["diff_schema", "diffSchema"],
            ScriptGraphTraversalSource::diff_schema,
        )
        .register_fn_as(
            &["set_visualizer", "setVisualizer"],
            ScriptGraphTraversalSource::set_visualizer,
        )
        // Saved queries (`script/saved_query.rs`): plain calls on the source, not steps.
        // `query` and `queries` are one word, so one spelling serves both conventions.
        .register_fn_as(
            &["define_query", "defineQuery"],
            ScriptGraphTraversalSource::define_query,
        )
        .register_fn_as(
            &["define_queries", "defineQueries"],
            ScriptGraphTraversalSource::define_queries,
        )
        .register_fn_as(
            &["define_queries", "defineQueries"],
            ScriptGraphTraversalSource::define_queries_with,
        )
        .register_fn("query", ScriptGraphTraversalSource::query)
        .register_fn("query", ScriptGraphTraversalSource::query_with)
        .register_fn("queries", ScriptGraphTraversalSource::queries)
        .register_fn("queries", ScriptGraphTraversalSource::queries_in)
        .register_fn_as(
            &["get_query", "getQuery"],
            ScriptGraphTraversalSource::get_query,
        )
        .register_fn_as(
            &["drop_query", "dropQuery"],
            ScriptGraphTraversalSource::drop_query,
        )
        .register_fn_as(
            &["move_query", "moveQuery"],
            ScriptGraphTraversalSource::move_query,
        )
        .register_fn_as(
            &["describe_query", "describeQuery"],
            ScriptGraphTraversalSource::describe_query,
        )
        // Compression rules (`script/compression.rs`): plain calls on the source, not steps.
        // `compressions` and `recompress` are one word, so one spelling serves both conventions.
        .register_fn_as(
            &["define_compression", "defineCompression"],
            ScriptGraphTraversalSource::define_compression,
        )
        .register_fn("compressions", ScriptGraphTraversalSource::compressions)
        .register_fn_as(
            &["drop_compression", "dropCompression"],
            ScriptGraphTraversalSource::drop_compression,
        )
        .register_fn("recompress", ScriptGraphTraversalSource::recompress)
        .register_fn_as(
            &["estimate_compression", "estimateCompression"],
            ScriptGraphTraversalSource::estimate_compression,
        )
        // Property indexes (`script/property_index.rs`): plain calls on the source, not steps.
        // `indexes` is one word, so one spelling serves both conventions.
        .register_fn_as(
            &["create_index", "createIndex"],
            ScriptGraphTraversalSource::create_index,
        )
        .register_fn("indexes", ScriptGraphTraversalSource::indexes)
        .register_fn_as(
            &["drop_index", "dropIndex"],
            ScriptGraphTraversalSource::drop_index,
        );

    #[cfg(feature = "io")]
    registry
        .register_fn_as(
            &["export_graphml", "exportGraphml"],
            ScriptGraphTraversalSource::export_graphml,
        )
        .register_fn_as(
            &["import_graphml", "importGraphml"],
            ScriptGraphTraversalSource::import_graphml,
        );
    #[cfg(feature = "io")]
    registry
        .register_fn_as(
            &["export_graphson", "exportGraphson"],
            ScriptGraphTraversalSource::export_graphson,
        )
        .register_fn_as(
            &["import_graphson", "importGraphson"],
            ScriptGraphTraversalSource::import_graphson,
        );
    #[cfg(all(feature = "io", feature = "persist"))]
    registry.register_fn_as(
        &["export_snapshot", "exportSnapshot"],
        ScriptGraphTraversalSource::export_snapshot,
    );
}
