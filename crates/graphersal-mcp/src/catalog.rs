//! The built-in tool catalog: every tool's name, title, kind, description and input schema,
//! and the read-only selection.

use serde_json::{Value, json};

use crate::answer::with_paging;
use crate::config::{McpConfig, offered_read_only};
use crate::tools::confirm_lock_arg;

/// What a tool does, for its annotations and for `--mcp-read-only`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Read,
    Write,
    Destructive,
}

pub(crate) struct Tool {
    pub(crate) name: &'static str,
    pub(crate) title: &'static str,
    pub(crate) kind: Kind,
    pub(crate) description: String,
    pub(crate) input: Value,
}

impl Tool {
    pub(crate) fn listing(&self, read_only: bool) -> Value {
        // In read-only mode `query` only reads.
        let kind = if read_only { Kind::Read } else { self.kind };
        json!({
            "name": self.name,
            "title": self.title,
            "description": self.description,
            "inputSchema": self.input,
            "annotations": {
                "title": self.title,
                "readOnlyHint": kind == Kind::Read,
                "destructiveHint": kind == Kind::Destructive,
                "idempotentHint": kind == Kind::Read,
                "openWorldHint": false,
            },
        })
    }
}

/// The schema format in one line, for the tool descriptions.
pub(crate) const SCHEMA_SHAPE: &str = "{\"mode\": \"open\", \"vertices\": {\"person\": {\"schema\": \
{\"type\": \"object\", \"properties\": {\"name\": {\"type\": \"string\"}, \"age\": {\"type\": \
\"integer\"}}, \"required\": [\"name\"]}}}, \"edges\": {\"knows\": {\"connections\": [{\"from\": \
\"person\", \"to\": \"person\"}], \"schema\": {\"type\": \"object\", \"properties\": {\"weight\": \
{\"type\": \"number\"}}}}}} (each label's properties are a JSON Schema object under \"schema\"; \
mode none/open/closed)";

pub(crate) fn schema_arg(what: &str) -> Value {
    json!({
        "type": "object",
        "description": format!("{what}, in Graphersal's schema format: {SCHEMA_SHAPE}. infer_schema shows the full shape for the current data. A JSON string is accepted too."),
    })
}

pub(crate) fn mode_arg() -> Value {
    json!({
        "type": "string",
        "enum": ["none", "open", "closed"],
        "description": "Replaces the schema's own mode: none (stored, not enforced), open \
                        (declared fields enforced), closed (also rejects undeclared labels and \
                        properties).",
    })
}

/// The arguments of `get_schema` / `infer_schema`.
pub(crate) fn schema_read_args() -> Value {
    json!({
        "type": "object",
        "properties": {
            "labels": {"type": "array", "items": {"type": "string"}},
            "format": {"enum": ["summary", "json"]},
        },
    })
}

pub(crate) fn no_args() -> Value {
    json!({"type": "object", "properties": {}})
}

pub(crate) fn all_tools() -> Vec<Tool> {
    let tool = |name, title, kind, input| Tool {
        name,
        title,
        kind,
        description: description(name, false).to_string(),
        input,
    };
    vec![
        tool(
            "query",
            "Run a query",
            Kind::Destructive,
            with_paging(
                json!({
                    "type": "object",
                    "properties": {
                        "script": {"type": "string", "description": "The Graphersal script (Gremlin in Rhai syntax)."},
                        "profile": {"type": "boolean", "description": "Also return the per-step metrics of the optimized plan (default false)."},
                    },
                    "required": ["script"],
                }),
                "top-level results",
            ),
        ),
        tool(
            "gql",
            "Run a GQL query",
            Kind::Destructive,
            with_paging(
                json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "The GQL query (MATCH ... RETURN ..., or a write such as INSERT ..., SET ... or MERGE ...), as plain text."},
                        "params": {"type": "object", "description": "Parameter name -> value; $name in the query reads the entry name."},
                        "profile": {"type": "boolean", "description": "Also return the per-step metrics of the Gremlin plan the query runs as (default false)."},
                    },
                    "required": ["query"],
                }),
                "rows",
            ),
        ),
        tool(
            "list_saved_queries",
            "Saved queries",
            Kind::Read,
            json!({
                "type": "object",
                "properties": {
                    "folder": {"type": "string", "description": "Only the queries in this folder and below it (e.g. \"reports\" lists reports and reports/sales)."},
                    "search": {"type": "string", "description": "Only the queries whose name or description contains this text (case-insensitive)."},
                },
            }),
        ),
        tool(
            "run_saved_query",
            "Run a saved query",
            Kind::Destructive,
            with_paging(
                json!({
                    "type": "object",
                    "properties": {
                        "name": {"type": "string", "description": "The saved query's name (list_saved_queries)."},
                        "params": {"type": "object", "description": "Parameter name -> value; omit a parameter to use its default."},
                    },
                    "required": ["name"],
                }),
                "top-level results",
            ),
        ),
        tool(
            "list_compressions",
            "Compression rules",
            Kind::Read,
            no_args(),
        ),
        tool(
            "estimate_compression",
            "Estimate a compression rule",
            Kind::Read,
            json!({
                "type": "object",
                "properties": {
                    "label": {"type": "string", "description": "The vertex or edge label."},
                    "path": {"type": "string", "description": "The property path: a name or a jpath of names (details.career.cv)."},
                    "element": {"type": "string", "enum": ["vertex", "edge"], "description": "Default vertex."},
                    "codec": {
                        "description": "Measure only this codec, or these codecs (default: lz4 only). dedup stores each distinct value once: values repeated across many elements (any type on a top-level property).",
                        "oneOf": [
                            {"type": "string", "enum": ["lz4", "dedup"]},
                            {"type": "array", "items": {"type": "string", "enum": ["lz4", "dedup"]}},
                        ],
                    },
                    "minBytes": {"type": "integer", "minimum": 0, "description": "Measure only this threshold (default: several; 0 only for dedup)."},
                    "dictionary": {"type": "boolean", "description": "Measure only with (true) or without (false) a dictionary (default: both)."},
                },
                "required": ["label", "path"],
            }),
        ),
        tool(
            "define_compression",
            "Define a compression rule",
            Kind::Write,
            json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "The rule's name (an identifier, e.g. customer_cv)."},
                    "label": {"type": "string", "description": "The vertex or edge label."},
                    "path": {"type": "string", "description": "The property path: a name or a jpath of names (details.career.cv)."},
                    "element": {"type": "string", "enum": ["vertex", "edge"], "description": "Default vertex."},
                    "codec": {"type": "string", "enum": ["lz4", "dedup"], "description": "lz4 (default) compresses each value; dedup stores each distinct value once."},
                    "minBytes": {"type": "integer", "minimum": 0, "description": "Shorter strings stay plain (lz4: 1 or more, default 256; dedup: default 0, every string; dedup on a top-level key also stores every number, boolean, date, array or object value once)."},
                    "dictionary": {"type": "boolean", "description": "lz4 only: build a dictionary from the rule's values (default true; a dedup rule has none)."},
                    "replaces": {"type": "string", "description": "Rename: the rule this one replaces (dropped in the same commit)."},
                },
                "required": ["name", "label", "path"],
            }),
        ),
        tool(
            "drop_compression",
            "Drop a compression rule",
            Kind::Destructive,
            json!({
                "type": "object",
                "properties": {"name": {"type": "string", "description": "The rule's name (list_compressions)."}},
                "required": ["name"],
            }),
        ),
        tool(
            "recompress",
            "Recompress a rule",
            Kind::Write,
            json!({
                "type": "object",
                "properties": {"name": {"type": "string", "description": "The rule's name (list_compressions)."}},
                "required": ["name"],
            }),
        ),
        tool("list_indexes", "Property indexes", Kind::Read, no_args()),
        tool(
            "create_index",
            "Create a property index",
            Kind::Write,
            json!({
                "type": "object",
                "properties": {
                    "label": {"type": "string", "description": "The vertex or edge label; omit for every label."},
                    "key": {"type": "string", "description": "The top-level property name (not a jpath)."},
                    "element": {"type": "string", "enum": ["vertex", "edge"], "description": "Default vertex."},
                    "name": {"type": "string", "description": "The index's name (an identifier); default <label>_<key> (<key> label-less)."},
                    "type": {
                        "type": "string",
                        "enum": graphersal::catalog::PropertyIndexType::KNOWN_NAMES,
                        "description": "The index type; default memory-ordered (an ordered index kept in memory, rebuilt from the data on open; serves =, P.within and ranges).",
                    },
                    "transform": {
                        "enum": transform_choices(),
                        "description": "Optional: to_lower for has(key, __.to_lower().is(..)); each (list elements) for has(key, __.unfold().is(..)); [\"each\", \"to_lower\"] for has(key, __.unfold().to_lower().is(..)). Default: none.",
                    },
                    "unique": {"type": "boolean", "description": "true: no value twice (null exempt)."},
                },
                "required": ["key"],
            }),
        ),
        tool(
            "drop_index",
            "Drop a property index",
            Kind::Destructive,
            json!({
                "type": "object",
                "properties": {"name": {"type": "string", "description": "The index's name (list_indexes)."}},
                "required": ["name"],
            }),
        ),
        tool("get_schema", "Graph schema", Kind::Read, schema_read_args()),
        tool(
            "infer_schema",
            "Inferred schema",
            Kind::Read,
            schema_read_args(),
        ),
        tool(
            "validate_schema",
            "Check a schema",
            Kind::Read,
            json!({"type": "object", "properties": {"schema": schema_arg("The proposed schema"), "mode": mode_arg()}, "required": ["schema"]}),
        ),
        tool(
            "validate_schema_patch",
            "Check a schema patch",
            Kind::Read,
            json!({"type": "object", "properties": {"patch": {"type": "object", "description": "A JSON Merge Patch of the stored schema (a JSON string is accepted too)."}}, "required": ["patch"]}),
        ),
        tool(
            "diff_schema",
            "Diff a schema",
            Kind::Read,
            json!({"type": "object", "properties": {"schema": schema_arg("The proposed schema")}, "required": ["schema"]}),
        ),
        tool(
            "set_schema",
            "Replace the schema",
            Kind::Destructive,
            json!({"type": "object", "properties": {"schema": schema_arg("The new schema"), "mode": mode_arg(), "confirm_lock": confirm_lock_arg()}, "required": ["schema"]}),
        ),
        tool(
            "patch_schema",
            "Patch the schema",
            Kind::Write,
            json!({"type": "object", "properties": {"patch": {"type": "object", "description": "A JSON Merge Patch of the stored schema (a JSON string is accepted too)."}, "confirm_lock": confirm_lock_arg()}, "required": ["patch"]}),
        ),
        tool("statistics", "Statistics", Kind::Read, no_args()),
        tool(
            "workload_statistics",
            "Workload statistics",
            Kind::Read,
            json!({
                "type": "object",
                "properties": {
                    "max_rows": {"type": "integer", "minimum": 1, "description": "Keep this many rows per list (default 50); <list>_omitted counts the rest."},
                    "history": {"type": "string", "enum": ["minutes", "hours", "days"], "description": "Add every row's buckets of this tier: the last 60 minutes, 48 hours or 90 days."},
                },
            }),
        ),
        tool(
            "journal",
            "System journal",
            Kind::Read,
            json!({
                "type": "object",
                "properties": {
                    "after": {"type": "integer", "minimum": 0, "description": "Only records after this seq (the previous page's next_after)."},
                    "newest_first": {"type": "boolean", "description": "The newest matching records, newest first (the answer then has next_before)."},
                    "before": {"type": "integer", "minimum": 1, "description": "Only records before this seq (with newest_first: the previous page's next_before)."},
                    "level": {"type": "string", "enum": ["error", "warn", "notice", "info"], "description": "Only this level and the more severe ones."},
                    "since": {"type": "string", "description": "Only records at or after this time: RFC 3339 with a zone, or microseconds since the Unix epoch."},
                    "source": {"type": "string", "description": "Only this source and the ones below it (store matches store.backup)."},
                    "kind": {"type": "string", "description": "Only records of this kind (store.backup_done, auth.denied, ...)."},
                    "limit": {"type": "integer", "minimum": 1, "description": "At most this many records (default 100)."},
                },
            }),
        ),
        tool(
            "advise",
            "Storage advice",
            Kind::Read,
            json!({
                "type": "object",
                "properties": {
                    "everything": {"type": "boolean", "description": "Report every candidate, ignoring the default thresholds (for small graphs)."},
                },
            }),
        ),
        tool(
            "apply_recommendations",
            "Apply recommendations",
            Kind::Write,
            json!({
                "type": "object",
                "properties": {
                    "ids": {"type": "array", "items": {"type": "string"}, "minItems": 1, "description": "The recommendation ids from advise (recommendations[].id or groups[].ids)."},
                    "everything": {"type": "boolean", "description": "The same value advise was called with (the advice is recomputed with it)."},
                },
                "required": ["ids"],
            }),
        ),
        tool("list_jobs", "List scheduled jobs", Kind::Read, no_args()),
        tool(
            "memory_report",
            "Memory report",
            Kind::Read,
            json!({
                "type": "object",
                "properties": {
                    "limit": {"type": "integer", "minimum": 1, "description": "Keep the property rows and the label rows with the most bytes (default 50 each)."},
                    "max_rows": {"type": "integer", "minimum": 1, "description": "Keep the property rows with the most bytes (default: limit); rows_omitted counts the rest."},
                    "max_labels": {"type": "integer", "minimum": 1, "description": "Keep the label rows with the most bytes (default: limit); labels_omitted counts the rest."},
                    "max_elements": {"type": "integer", "minimum": 1, "description": "Sample so that about this many elements are read, at least one per label (default: all)."},
                    "sample_every": {"type": "integer", "minimum": 1, "description": "Read every k-th element only (default 1)."},
                },
            }),
        ),
        tool(
            "usage_statistics",
            "Usage statistics",
            Kind::Read,
            json!({
                "type": "object",
                "properties": {
                    "max_rows": {"type": "integer", "minimum": 1, "description": "Rows per list (tools, savedQueries, clients), most calls first (default 50)."},
                },
            }),
        ),
        tool("graph_info", "Graph info", Kind::Read, no_args()),
    ]
}

/// Whether `name` is a built-in tool, without building the catalog (the statistics ask it on
/// every call; [`all_tools`] builds every description and schema).
pub(crate) fn is_built_in(name: &str) -> bool {
    static NAMES: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
    NAMES
        .get_or_init(|| all_tools().iter().map(|t| t.name).collect())
        .contains(&name)
}

/// The tools offered: all of them, only the reading ones (and `query`, `gql`,
/// `run_saved_query`, which run under the read-only authorizer there) when `read_only`
/// (`--mcp-read-only`, an `--ai` file's `read_only`).
pub(crate) fn tools(read_only: bool) -> Vec<Tool> {
    all_tools()
        .into_iter()
        .filter(|t| !read_only || offered_read_only(t.name, t.kind))
        .map(|mut t| {
            t.description = description(t.name, read_only).to_string();
            t
        })
        .collect()
}

/// The tools a configured endpoint offers: [`tools`] of the mode, only those the
/// configuration offers, each with its description (the operator's override, else the
/// built-in one).
pub(crate) fn offered_tools(read_only: bool, config: &McpConfig) -> Vec<Tool> {
    tools(read_only)
        .into_iter()
        .filter(|t| config.offers(t.name))
        .map(|mut t| {
            if let Some(text) = config.override_of(t.name) {
                t.description = text.to_string();
            }
            t
        })
        .collect()
}

/// The `query` description of a read-only server.
const QUERY_TEXT_READ_ONLY: &str = "Run a Graphersal script: Gremlin in Rhai syntax (double-quoted \
strings, `__.` for child traversals, tokens such as P.gt(30), Order.desc, T.label); statements \
end with `;`, the last value is the result, and a traversal before it runs only with a terminal \
(.to_list(), .next(), .iterate()). Compact JSON, paged (offset, max_rows, max_items). Prefer the \
gql tool for plain reads; in a script GQL is g.gql(\"MATCH ...\", #{name: value}) and a saved \
query g.query(\"name\", #{..}). Every step: resource graphersal://dsl-reference. Example: \
g.v().has_label(\"person\").values(\"name\")";

/// The `query` description of a server that allows writes: [`QUERY_TEXT_READ_ONLY`] plus the
/// write steps and the transaction.
const QUERY_TEXT: &str = "Run a Graphersal script: Gremlin in Rhai syntax \
(double-quoted strings, `__.` for child traversals, tokens such as P.gt(30), Order.desc, \
T.label); statements end with `;`, the last value is the result, and a traversal before it runs \
only with a terminal (.to_list(), .next(), .iterate()). Compact JSON, paged (offset, max_rows, \
max_items). Prefer the gql tool for plain reads and writes; in a script GQL is \
g.gql(\"MATCH ...\", #{name: value}) and a saved query g.query(\"name\", #{..}). Writes, only \
when the user asked; the whole script is one transaction (a failure changes nothing): \
g.add_v(\"person\").property(\"name\", \"ada\"), g.v(\"id1\").add_e(\"knows\").to(\"id2\") \
(vertex ids), .property(k, v) sets, .properties(k).drop() removes a property, .drop() removes \
elements (vertices with their edges). Every step: resource graphersal://dsl-reference. Example: \
g.v().has_label(\"person\").values(\"name\")";

/// The `gql` description of a read-only server (the full support list is the resource
/// `graphersal://gql-reference`).
const GQL_TEXT_READ_ONLY: &str = "Run a GQL query (ISO graph query language; Cypher spellings \
accepted): the text as it is, strings in single quotes, $name values from params; one JSON \
object per row (elements as {id, label, properties}), paged (offset, max_rows). Ids: \
element_id(n) = 'x', not id(n); labels(n), type(r). Anchor a variable-length pattern -[:T*1..3]-> on a node found by \
a property (an unanchored one can exceed the memory limit). Every clause and function: resource \
graphersal://gql-reference. Example: MATCH (p:person)-[:knows]->(f) WHERE f.age > $min RETURN \
p.name AS who, f.name AS friend ORDER BY who, params {\"min\": 30}";

/// The `gql` description of a server that allows writes: [`GQL_TEXT_READ_ONLY`] plus what a
/// model must know to write correctly (the clauses, one transaction, DETACH, MERGE).
const GQL_TEXT: &str = "Run a GQL query (ISO graph query language; Cypher spellings \
accepted): the text as it is, strings in single quotes, $name values from params; one JSON \
object per row (elements as {id, label, properties}), paged (offset, max_rows). Ids: \
element_id(n) = 'x', not id(n); labels(n), type(r). Anchor a variable-length pattern -[:T*1..3]-> on a node found by \
a property (an unanchored one can exceed the memory limit). Writes, only when the user asked; \
each call is one transaction (a failure changes nothing; the answer names the net changes): \
INSERT (Cypher CREATE) (n:L {k: v}), an edge (one label) between matched nodes MATCH (a:L {k: \
1}), (b:L {k: 2}) INSERT (a)-[:T {k: v}]->(b), once per row (FOR x IN $list INSERT .. for many); \
SET n.k = v, n += {..} (merge), n = {..} (replace all), n:L (add a label); REMOVE n.k, n:L; MERGE \
(n:L {k: v}) [ON CREATE SET ..] [ON MATCH SET ..] finds or creates (idempotent; MERGE \
(a)-[:T]->(b) for an edge between matched nodes); DETACH DELETE n removes a node with its edges \
(DELETE fails on a node that has edges; DELETE r removes an edge); do not return a deleted \
element. Every clause and function: resource graphersal://gql-reference. Example: MATCH \
(p:person)-[:knows]->(f) WHERE f.age > $min RETURN p.name AS who, f.name AS friend ORDER BY \
who, params {\"min\": 30}";

/// The description of every built-in tool: one or two sentences and one example where it helps
/// (what an agent pays in context before its first call; the long material is in the input
/// schemas and the resources, the GQL support list in `graphersal://gql-reference`). `query`
/// and `gql` also name the writes when the server allows them (`read_only` false).
pub(crate) fn description(name: &str, read_only: bool) -> &'static str {
    match name {
        "query" if read_only => QUERY_TEXT_READ_ONLY,
        "query" => QUERY_TEXT,
        "gql" if read_only => GQL_TEXT_READ_ONLY,
        "gql" => GQL_TEXT,
        "list_saved_queries" => {
            "The saved queries stored in the graph (name, folder, description, parameters, \
             language gql or rhai, writes and effects). Check them before writing a new query."
        }
        "run_saved_query" => {
            "Run a saved query by name with named parameters (checked against their schemas; \
             defaults fill the rest), paged like the gql tool. Run one with writes: true only when the \
             user asked for that change. Example: {\"name\": \"older\", \"params\": \
             {\"min\": 30}}"
        }
        "list_compressions" => {
            "The compression rules of the graph (element, label, path, codec lz4 or dedup, \
             served, stats)."
        }
        "estimate_compression" => {
            "Measure candidate compression rules for one label and property path on the \
             current data, changing nothing (sizes, ratio, read speed, the best candidate). Use \
             it before the define_compression tool."
        }
        "define_compression" => {
            "Store or replace a compression rule: strings of one label at one property path \
             kept LZ4-compressed (large, rarely read text) or stored once per distinct value \
             (codec dedup); existing values are re-encoded at once."
        }
        "drop_compression" => {
            "Remove a compression rule; its values are decompressed (no data changes)."
        }
        "recompress" => {
            "Rebuild a compression rule's dictionary from the current values; no data changes."
        }
        "list_indexes" => {
            "The property indexes of the graph (element, label, key, type, transform, served, \
             stats)."
        }
        "create_index" => {
            "Create a property index on one label's top-level property, so has_label(label) \
             with has(key, ..) (equality, P.within, ranges) reads only the matching elements; \
             results never change. Example: {\"label\": \"person\", \"key\": \"name\"}"
        }
        "drop_index" => "Remove a property index (no data changes; queries scan again).",
        "get_schema" => {
            "A map of the graph (labels, counts, keys, types, edge from->to pairs, indexes); \
             labels: details. format \"json\": the stored schema."
        }
        "infer_schema" => "get_schema's map; format \"json\": the schema the data satisfies.",
        "validate_schema" => {
            "Check a proposed schema against the data without applying it: violations, the \
             diff from the stored schema, backward compatibility."
        }
        "validate_schema_patch" => {
            "validate_schema of the stored schema with a JSON Merge Patch applied; applies \
             nothing."
        }
        "diff_schema" => {
            "The changes from the stored schema to a proposed one, each marked backward \
             compatible or not."
        }
        "set_schema" => {
            "Store a schema, replacing the stored one; in mode open/closed the data is \
             validated first and nothing changes on a violation. Check it with the \
             validate_schema tool first. The whole database is LOCKED while a new vector rule \
             fills (confirm_lock)."
        }
        "patch_schema" => {
            "Apply a JSON Merge Patch to the stored schema, validated like set_schema (the \
             database is LOCKED while a vector rule fills). Example: {\"vertices\": {\"person\": \
             {\"schema\": {\"properties\": {\"email\": {\"type\": \"string\"}}}}}}"
        }
        "statistics" => {
            "Counts of the graph per label, its memory footprint, the query limits and the \
             memory of the process."
        }
        "workload_statistics" => {
            "Which queries ran and how the properties were used, per channel and time window \
             (a graph in a store only)."
        }
        "journal" => {
            "One page of the system journal: operational events (store opened, checkpoints, \
             backups, damage, denials), oldest first unless newest_first."
        }
        "advise" => {
            "The storage advisor's recommendations (indexes, compression), each with its \
             evidence, impact and the command applying it; changes nothing."
        }
        "apply_recommendations" => {
            "Apply the advisor's recommendations named by ids in one commit; only the ones the \
             user picked."
        }
        "list_jobs" => {
            "The scheduled jobs of the graph's store, their schedules and last runs (the user \
             defines them)."
        }
        "memory_report" => {
            "Where the memory goes per label and property (bytes, types, compression, indexes) \
             and per process component; changes nothing."
        }
        "usage_statistics" => {
            "How this MCP endpoint was used since the server started (memory only): calls, \
             errors and latency per tool and saved query, clients, sessions, refusals."
        }
        "graph_info" => {
            "What the server serves: the graph's name, counts, schema mode, source, a \
             read-only view the user opened, and answerLimitChars."
        }
        _ => "",
    }
}

/// The `transform` values `create_index` takes: every transform name this version knows, then
/// the chain it serves as the array of its names (`["each", "to_lower"]`).
fn transform_choices() -> Value {
    let mut choices: Vec<Value> = graphersal::catalog::PropertyIndexTransform::KNOWN_NAMES
        .iter()
        .map(|name| Value::from(*name))
        .collect();
    choices.push(json!(["each", "to_lower"]));
    Value::Array(choices)
}
