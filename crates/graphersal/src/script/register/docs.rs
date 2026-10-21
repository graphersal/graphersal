//! The documentation of every DSL function: one line of what it does and one runnable example,
//! attached by [`Registry::register_fn`](super::Registry::register_fn) to the function metadata
//! (`docComments`) of every overload and every spelling of a name, so the completion catalogue
//! of the playground and the dev server's `graphersal://dsl-reference` read it from the engine.
//!
//! A text is keyed by the *receiver* (the script type a method is called on: `traversal` for
//! `g.x()`/`__.x()`, `P` for `P.x()`, ..., `global` for a free function) and the
//! spelling-independent [`key`] of the name, so `has_label` and `hasLabel` share one entry. A
//! free function without an entry of its own takes the one of its `P`, `TextP`, `GraphSource` or
//! `By` twin (`gt(30)` is `P.gt(30)`), and `P`/`TextP` share their text predicates.
//!
//! Texts are written in the canonical snake_case DSL style ("Rendering and spelling" in
//! `CLAUDE.md`). `docs_tests.rs` checks that every function of the completion catalogue has a
//! text, that every text is used, and runs every example on the modern graph against its
//! documented result. The few functions that come from a `#[export_module]` plugin module carry
//! the same text as a `///` comment (the test compares them).

use std::collections::HashMap;

/// The documentation of one DSL function (all overloads, all spellings).
pub(crate) struct FnDoc {
    /// The receiver (see the module docs).
    pub(crate) receiver: &'static str,
    /// One spelling of the name; only its [`key`] matters.
    pub(crate) name: &'static str,
    /// One sentence: what the function does.
    pub(crate) summary: &'static str,
    /// A runnable example in the canonical snake_case DSL, evaluated on the modern graph.
    pub(crate) example: &'static str,
    /// The example's result as the script's `to_string()` renders it; empty when it is not
    /// shown (a table, a long text). `error: <text>` documents a failure whose message contains
    /// `<text>`; [`NOT_RUN`] marks an example that needs a file or a host setup.
    pub(crate) result: &'static str,
}

/// The [`FnDoc::result`] of an example that is not run by the test (it reads or writes files).
pub(crate) const NOT_RUN: &str = "\u{0}not run";

/// The spelling-independent key of a function name: lowercase, without underscores
/// (`hasLabel`, `has_label` -> `haslabel`; `in_` -> `in`; `toPlantUML` -> `toplantuml`).
pub(crate) fn key(name: &str) -> String {
    name.chars()
        .filter(|c| *c != '_')
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// The receiver of a function from the type name of its first parameter: the Rust type name at
/// registration (`&mut graphersal::script::source::ScriptGraphTraversalSource`) or the script
/// display name in the metadata (`&mut GraphTraversalSource`), both `traversal`; `global` for a
/// free function (no parameter, or a first parameter that is an argument).
pub(crate) fn receiver_of(first_param: Option<&str>) -> &'static str {
    let Some(ty) = first_param else {
        return "global";
    };
    let ty = ty
        .trim()
        .trim_start_matches("&mut ")
        .trim_start_matches('&');
    let base = ty.split('<').next().unwrap_or(ty);
    match base.rsplit("::").next().unwrap_or(base) {
        "ScriptGraphTraversalSource" | "GraphTraversalSource" => "traversal",
        "ScriptP" | "P" => "P",
        "ScriptTextP" | "TextP" => "TextP",
        "ScriptBy" | "By" => "By",
        "ScriptUuidTokens" | "UUIDTokens" => "UUID",
        "ScriptSetTokens" | "SetTokens" => "Set",
        "ScriptMap" | "Map" => "Map",
        "TraversalMetrics" => "TraversalMetrics",
        "ScriptExecution" | "Execution" => "Execution",
        "ScriptGraphSource" | "GraphSource" => "GraphSource",
        "ScriptCardinality" | "CardinalityTokens" => "Cardinality",
        "ScriptV" | "V" => "V",
        "ScriptCastPolicy" | "CastPolicy" => "CastPolicy",
        "ArgP" | "Predicate" => "Predicate",
        "ScriptUuid" | "Uuid" => "Uuid",
        "ScriptVector" | "Vector" => "Vector",
        "ScriptFake" | "Fake" => "Fake",
        "DateTime" | "datetime" | "Date" | "date" | "LocalDateTime" | "localdatetime" | "Time"
        | "time" | "Duration" | "duration" => "temporal",
        // `rhai::Map` is a `BTreeMap`; `map` is its display name.
        "BTreeMap" | "map" => "map",
        _ => "global",
    }
}

/// Names that never get a text: Rhai's printing hooks (on every custom type), operators,
/// property getters/setters and indexers.
pub(crate) fn is_undocumented(name: &str) -> bool {
    matches!(name, "to_debug" | "to_string")
        || name.starts_with("get$")
        || name.starts_with("set$")
        || name.starts_with("index$")
        || !name.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
}

/// The receivers whose entries document a function of `receiver`, in lookup order.
fn lookup_order(receiver: &'static str) -> &'static [&'static str] {
    match receiver {
        "global" => &["global", "P", "TextP", "GraphSource", "By"],
        "P" => &["P", "TextP"],
        "TextP" => &["TextP", "P"],
        "traversal" => &["traversal"],
        "By" => &["By"],
        "UUID" => &["UUID"],
        "Set" => &["Set"],
        "Map" => &["Map"],
        "TraversalMetrics" => &["TraversalMetrics"],
        "Execution" => &["Execution"],
        "GraphSource" => &["GraphSource"],
        "Cardinality" => &["Cardinality"],
        "V" => &["V"],
        "CastPolicy" => &["CastPolicy"],
        "Predicate" => &["Predicate"],
        "Uuid" => &["Uuid"],
        "Vector" => &["Vector"],
        "Fake" => &["Fake"],
        "temporal" => &["temporal"],
        "map" => &["map"],
        _ => &[],
    }
}

/// The entry that documents `name` called on `receiver` (see the module docs).
pub(crate) fn lookup(receiver: &'static str, name: &str) -> Option<&'static FnDoc> {
    let key = key(name);
    lookup_order(receiver).iter().find_map(|receiver| {
        DOCS.iter()
            .find(|doc| doc.receiver == *receiver && self::key(doc.name) == key)
    })
}

/// The doc comment lines of `doc`, in the form Rhai's metadata carries (`/// ...`): the summary,
/// an empty line and the example with its result.
pub(crate) fn comment_lines(doc: &FnDoc) -> Vec<String> {
    let mut example = format!("/// Example: `{}`", doc.example);
    if let Some(error) = doc.result.strip_prefix("error: ") {
        example.push_str(&format!(" fails with `{error}`"));
    } else if !doc.result.is_empty() && doc.result != NOT_RUN {
        example.push_str(&format!(" returns `{}`", doc.result));
    }
    vec![format!("/// {}", doc.summary), "///".to_string(), example]
}

/// The doc comment lines of every documented `(receiver, key)`, built once per registry.
pub(crate) struct DocIndex(HashMap<(&'static str, String), Vec<String>>);

impl DocIndex {
    pub(crate) fn new() -> Self {
        Self(HashMap::new())
    }

    /// The doc comment lines of `name` called on `receiver`; empty when undocumented.
    pub(crate) fn get(&mut self, receiver: &'static str, name: &str) -> Vec<String> {
        if is_undocumented(name) {
            return Vec::new();
        }
        self.0
            .entry((receiver, key(name)))
            .or_insert_with(|| {
                lookup(receiver, name)
                    .map(comment_lines)
                    .unwrap_or_default()
            })
            .clone()
    }
}

/// Builds the [`DOCS`] table from `receiver name: summary, example => result;` entries.
macro_rules! docs {
    ($($(#[$meta:meta])* $receiver:literal $name:literal: $summary:literal, $example:literal => $result:expr;)*) => {
        &[$($(#[$meta])* FnDoc {
            receiver: $receiver,
            name: $name,
            summary: $summary,
            example: $example,
            result: $result,
        }),*]
    };
}

/// Every documented function, grouped by receiver: one entry per function, whatever its
/// spellings and overloads.
pub(crate) const DOCS: &[FnDoc] = docs! {
    "traversal" "add_e":
        "Adds an edge with the given label; its endpoints come from from() and to() (see the book page addE() Endpoints).",
        "g.v(1).add_e(\"likes\").to(\"3\").label().next()" => "likes";
    "traversal" "add_label":
        "Adds a label to the label set of each incoming vertex and passes it on (Graphersal extension; edges use set_label()).",
        "g.v(1).add_label(\"employee\").labels().next()" => "[\"person\", \"employee\"]";
    "traversal" "add_v":
        "Adds a vertex with the given label (or the label a child traversal yields); chain property() to set its properties.",
        "g.add_v(\"person\").property(\"name\", \"ada\").values(\"name\").next()" => "ada";
    "traversal" "aggregate":
        "Collects every incoming value into the named list side effect before passing them on (a barrier); read it with cap() or select().",
        "g.v().has_label(\"person\").values(\"age\").aggregate(\"ages\").cap(\"ages\").next()" => "[29, 27, 32, 35]";
    "traversal" "all":
        "Keeps a list whose every element matches the predicate (an empty list matches).",
        "g.inject([1, 2, 3]).all(P.gt(0)).next()" => "[1, 2, 3]";
    "traversal" "and":
        "Keeps the traverser when every child traversal yields a result.",
        "g.v().and(__.out_e(\"knows\"), __.out_e(\"created\")).values(\"name\").to_list()" => "[\"marko\"]";
    "traversal" "any":
        "Keeps a list where at least one element matches the predicate.",
        "g.inject([1, 5, 3]).any(P.gt(4)).next()" => "[1, 5, 3]";
    "traversal" "as":
        "Labels the current step so select(), where() and path() can refer back to it.",
        "g.v(1).as(\"a\").out(\"knows\").as(\"b\").select(\"a\", \"b\").by(\"name\").next()" => "#{\"a\": \"marko\", \"b\": \"josh\"}";
    "traversal" "as_bool":
        "Converts the value to a boolean: true/false strings in any case, numbers (0 is false); anything else is an error unless a CastPolicy says otherwise.",
        "g.inject(\"true\", \"FALSE\", 0).as_bool().to_list()" => "[true, false, false]";
    "traversal" "as_date":
        "Text or epoch milliseconds to a datetime (TinkerPop asDate()), or to a GType.",
        "g.inject(0).as_date().next().year" => "1970";
    "traversal" "as_duration":
        "Converts ISO 8601 text to a duration.",
        "g.inject(\"P1D\").as_duration().next().days" => "1";
    "traversal" "as_vector":
        "Converts a list of numbers to an f32 vector; a vector passes (see the book page Vectors).",
        "g.inject([0.5, 1]).as_vector().next()" => "vector([0.5, 1.0])";
    "traversal" "quantize":
        "Converts a vector to the element kind f32, f16, i8 or bit; quantize(\"f32\") dequantizes.",
        "g.inject(vector([0.5, -1.0])).quantize(\"i8\").next().element" => "i8";
    "traversal" "similarity":
        "Score of a vector property (or the incoming vector) against q: cosine (default), dot, l2 or hamming; a text q is embedded by the model the schema declares for the property.",
        "g.inject(vector([3.0, 4.0])).similarity([0, 0], \"l2\").next()" => "5.0";
    "traversal" "nearest":
        "Exact top-k: the k inputs whose vector property is closest to q, best first; a text q is embedded by the model the schema declares for the property.",
        "g.inject(#{n: 1, emb: vector([1.0, 0.0])}, #{n: 2, emb: vector([0.0, 1.0])}).nearest(\"emb\", vector([0.0, 1.0]), 1).select(\"n\").next()" => "2";
    "traversal" "vector_mean":
        "The component-wise mean of the incoming vectors (or lists of vectors).",
        "g.inject(vector([1.0, 0.0]), vector([0.0, 1.0])).vector_mean().next()" => "vector([0.5, 0.5])";
    "traversal" "embed":
        "Embeds the incoming string, or the string property key, as a vector; #{embedder: name} picks the embedder (default hash-ngram/1).",
        "g.v().has(\"name\", \"marko\").embed(\"name\").next().dimension" => "256";
    "traversal" "embedders":
        "Lists the registered embedders (name, model, dimension, element, metric, ...), the default first.",
        "g.embedders()[0].name" => "hash-ngram/1";
    "traversal" "embed_property":
        "Embeds the property from of every element with label into the vector property to, page by page, with the embedder of that registry name (#{embedder: name}, else the default; g.embedders() lists them), and records its model in the attached schema; other keys: element, only_missing, batch; returns the report.",
        "{ g.set_schema(g.infer_schema(), SchemaMode.open); g.embed_property(#{label: \"person\", from: \"name\", to: \"emb\", embedder: \"hash-ngram/1\"}).model }" => "hash-ngram/1";
    "traversal" "date_add":
        "Adds n DT units or a duration, optionally in a zone (TinkerPop dateAdd()).",
        "g.inject(date(\"2024-01-31\")).date_add(DT.month, 1).next().day" => "29";
    "traversal" "date_diff":
        "Difference in milliseconds (TinkerPop dateDiff()) or whole DT units.",
        "g.inject(time(\"10:00\")).date_diff(time(\"09:00\"), DT.hour).next()" => "1";
    "traversal" "age":
        "The difference as a duration (PostgreSQL age).",
        "g.inject(date(\"2001-04-10\")).age(date(\"1957-06-13\")).next().months" => "525";
    "traversal" "date_trunc":
        "Truncates to a DT unit (PostgreSQL date_trunc).",
        "g.inject(date(\"2023-08-17\")).date_trunc(DT.month).next().day" => "1";
    "traversal" "date_part":
        "One field as a number (PostgreSQL extract).",
        "g.inject(date(\"2023-08-17\")).date_part(DT.day_of_week).next()" => "4";
    "traversal" "at_zone":
        "Places a date-time in a time zone.",
        "g.inject(localdatetime(\"2023-08-02T12:00\")).at_zone(\"+02:00\").next().offset" => "7200";
    "traversal" "as_local":
        "A datetime's wall clock.",
        "g.inject(datetime(\"2023-08-02T12:00Z\")).as_local().next().hour" => "12";
    "traversal" "as_number":
        "Converts the value to a number, optionally of the given GType (GType.LONG truncates toward zero, see the book page Type Conversion and length()).",
        "g.inject(\"42\", 3.9).as_number(GType.LONG).to_list()" => "[42, 3]";
    "traversal" "as_string":
        "Converts each value to its string text (Scope.local: each element of a list); a list itself is a cast error.",
        "g.v(1).values(\"age\").as_string().to_list()" => "[\"29\"]";
    "traversal" "barrier":
        "Collects the whole stream before passing it on; equal traversers merge into one with a bulk (see the book page Bulk and Barriers).",
        "g.v().out().barrier().count().next()" => "6";
    "traversal" "both":
        "Moves to the adjacent vertices over incoming and outgoing edges, optionally only edges with the given labels.",
        "g.v(4).both(\"knows\").values(\"name\").to_list()" => "[\"marko\"]";
    "traversal" "both_e":
        "Moves to the incident edges in both directions, optionally only edges with the given labels.",
        "g.v(4).both_e().label().to_list()" => "[\"knows\", \"created\", \"created\"]";
    "traversal" "both_v":
        "Moves from an edge to both of its endpoint vertices (out vertex first).",
        "g.e(\"0\").both_v().values(\"name\").to_list()" => "[\"marko\", \"vadas\"]";
    "traversal" "branch":
        "Routes each traverser by the value its child traversal yields to the matching option().",
        "g.v(1, 2).branch(__.values(\"age\")).option(29, __.values(\"name\")).option(Pick.none, __.constant(\"other\")).to_list()" => "[\"marko\", \"other\"]";
    "traversal" "by":
        "Modulates the step before it: the key, token or child traversal a value is read with, and for order() the direction; by(\"label\") reads a property named label, by(T.label) the label (see the book page Properties Named id or label).",
        "g.v().has_label(\"person\").order().by(\"age\", Order.desc).values(\"name\").to_list()" => "[\"peter\", \"josh\", \"marko\", \"vadas\"]";
    "traversal" "cap":
        "Emits the value of the named side effect(s) (one map for several keys) after the stream is exhausted.",
        "g.v().group_count(\"c\").by(T.label).cap(\"c\").next()" => "#{\"person\": 4, \"software\": 2}";
    "traversal" "cast":
        "Converts the value to the given GType (number, string, boolean, uuid, list, map), optionally with a CastPolicy for values that do not convert.",
        "g.inject(\"1\", \"2\").cast(GType.LONG).sum().next()" => "3";
    "traversal" "choose":
        "If-then-else: runs the true or false child traversal depending on a predicate or child traversal; with one argument it picks an option() by value.",
        "g.v(1, 4).choose(__.values(\"age\").is(P.gt(30)), __.constant(\"old\"), __.constant(\"young\")).to_list()" => "[\"young\", \"old\"]";
    "traversal" "coalesce":
        "Runs the child traversals in order and emits the results of the first one that yields anything.",
        "g.v(1, 3).coalesce(__.values(\"age\"), __.values(\"lang\")).to_list()" => "[29, \"java\"]";
    "traversal" "coin":
        "Keeps each traverser with the given probability (0.0 to 1.0); seed it with g.with(\"random.seed\", n).",
        "g.v().coin(1.0).count().next()" => "6";
    "traversal" "combine":
        "Appends the given list (or the list a child traversal yields) to the incoming list, duplicates kept.",
        "g.inject([1, 2]).combine([2, 3]).next()" => "[1, 2, 2, 3]";
    "traversal" "compressions":
        "Lists the compression rules as maps: name, element, label, path, codec, min_bytes, dictionary, served (with reason when not applied) and the current compressed_values, plain_bytes, stored_bytes, dictionary_bytes.",
        "{ g.define_compression(#{name: \"bio\", label: \"person\", path: \"details.bio\"}); g.compressions().map(|r| r.path) }" => "[\"$.details.bio\"]";
    "traversal" "concat":
        "Appends strings (or the strings child traversals yield) to the incoming string.",
        "g.v(1).values(\"name\").concat(\"-\", \"x\").next()" => "marko-x";
    "traversal" "conjoin":
        "Joins the elements of the incoming list into one string with the given delimiter (see the book page Step - conjoin).",
        "g.inject([\"a\", \"b\", \"c\"]).conjoin(\"-\").next()" => "a-b-c";
    "traversal" "constant":
        "Replaces every incoming value with the given constant.",
        "g.v().has_label(\"software\").constant(\"app\").to_list()" => "[\"app\", \"app\"]";
    "traversal" "count":
        "Counts the traversers (bulk included); with Scope.local counts the elements of each incoming collection.",
        "g.v().has_label(\"person\").count().next()" => "4";
    "traversal" "create_index":
        "Declares a property index #{label, key, element, name, type, transform}: a v() or e() with that label and a has() on that key then reads the index instead of every element of the label; type defaults to \"memory-ordered\", transform \"to_lower\" indexes lowered strings for has(key, __.to_lower().is(..)) (see the book page Property Indexes).",
        "{ g.create_index(#{label: \"person\", key: \"age\"}); g.v().has_label(\"person\").has(\"age\", P.gt(30)).values(\"name\").to_list() }" => "[\"josh\", \"peter\"]";
    "traversal" "cyclic_path":
        "Keeps only traversers whose path visits some element twice.",
        "g.v(1).out().in().cyclic_path().count().next()" => "3";
    "traversal" "dedup":
        "Removes duplicates, optionally by a by() key or by the values of path labels; with Scope.local inside each collection.",
        "g.v().out().dedup().values(\"name\").to_list()" => "[\"josh\", \"lop\", \"vadas\", \"ripple\"]";
    "traversal" "define_compression":
        "Stores (or replaces) a compression rule #{name, label, path, element, codec, min_bytes, dictionary}: strings at that path are kept compressed in memory (codec \"lz4\") or stored once per distinct value (codec \"dedup\") and read plain (see the book page Compressed Properties).",
        "{ g.define_compression(#{name: \"n\", label: \"person\", path: \"name\"}); g.compressions().len() }" => "1";
    "traversal" "define_queries":
        "Stores every function of a Rhai source as a saved query in a folder, described by its doc comment; params maps function name -> its parameter map (see the book page Saved Queries).",
        "{ g.define_queries(\"reports\", \"/** Persons. */ fn people() { g.v().has_label(\\\"person\\\") }\"); g.query(\"people\").count().next() }" => "4";
    "traversal" "define_query":
        "Stores (or replaces) a saved query: #{name, body, language, folder, description, params}, the body being one Rhai function named like the query, or with language: \"gql\" one GQL query reading the declared $params; a body that writes data saves a query that writes (see the book page Saved Queries).",
        "{ g.define_query(#{name: \"older\", params: #{min: #{schema: #{type: \"integer\"}, default: 30}}, body: \"fn older(min) { g.v().has(\\\"age\\\", P.gt(min)) }\"}); g.query(\"older\").count().next() }" => "2";
    "traversal" "describe_query":
        "Sets the description of a saved query (\"\" or () removes it).",
        "{ g.define_query(#{name: \"n\", body: \"fn n() { 1 }\"}); g.describe_query(\"n\", \"One.\"); g.get_query(\"n\")[\"description\"] }" => "One.";
    "traversal" "diff_schema":
        "Compares the stored schema with the given schema map: {changes, compatible}.",
        "g.diff_schema(g.infer_schema())[\"compatible\"]" => "true";
    "traversal" "difference":
        "Keeps the elements of the incoming list that are not in the given list (set difference).",
        "g.inject([1, 2, 3]).difference([2]).next()" => "[1, 3]";
    "traversal" "discard":
        "Drops every traverser: the traversal yields nothing (side effects still run).",
        "g.v().side_effect(__.aggregate(\"x\")).discard().count().next()" => "0";
    "traversal" "disjunct":
        "Returns the elements that are in exactly one of the incoming and the given list (symmetric difference).",
        "g.inject([1, 2, 3]).disjunct([3, 4]).next()" => "[1, 2, 4]";
    "traversal" "drop":
        "Removes the incoming vertices, edges or properties from the graph (a vertex takes its edges with it) and yields nothing (see the book page Dropping Elements).",
        "{ g.v(1).out_e(\"knows\").drop().to_list(); g.e().count().next() }" => "4";
    "traversal" "drop_compression":
        "Removes a compression rule and decompresses its values; false when there is none.",
        "{ g.define_compression(#{name: \"bio\", label: \"person\", path: \"bio\"}); g.drop_compression(\"bio\") }" => "true";
    "traversal" "drop_index":
        "Drops a property index; false when there is none.",
        "{ g.create_index(#{label: \"person\", key: \"age\"}); g.drop_index(\"person_age\") }" => "true";
    "traversal" "drop_label":
        "Removes a label from the label set of each incoming vertex and passes it on; the last one leaves it unlabeled (Graphersal extension).",
        "g.v(1).drop_label(\"person\").labels().next()" => "[]";
    "traversal" "drop_query":
        "Removes a saved query; false when there is none.",
        "{ g.define_query(#{name: \"n\", body: \"fn n() { 1 }\"}); g.drop_query(\"n\") }" => "true";
    "traversal" "e":
        "Starts at the edges of the graph, all or the ones with the given ids.",
        "g.e().has_label(\"created\").count().next()" => "4";
    "traversal" "element":
        "Moves from a property to the vertex or edge that owns it.",
        "g.v(1).properties(\"name\").element().values(\"age\").next()" => "29";
    "traversal" "element_map":
        "Turns each vertex or edge into a map of the T.id and T.label tokens (an edge also Direction.OUT and Direction.IN) and the given (or all) properties, written \"id\", \"label\", \"OUT\", \"IN\" outside the engine where a property of the same name wins (see the book page Properties Named id or label).",
        "g.v(1).element_map(\"name\").next()" => "#{\"id\": \"1\", \"label\": \"person\", \"name\": \"marko\"}";
    "traversal" "emit":
        "Within repeat(): also emits the traversers of intermediate iterations (all, or those that match the child traversal or predicate).",
        "g.v(1).repeat(__.out()).times(2).emit().values(\"name\").to_list()" => "[\"josh\", \"lop\", \"vadas\", \"ripple\", \"lop\"]";
    "traversal" "estimate_compression":
        "Measures what compression rules for a label and path would save on the current values (lz4 with and without a dictionary at several min_bytes, or the given ones; codec: \"dedup\" or [\"lz4\", \"dedup\"] measures deduplication too), without changing the graph.",
        "{ let e = g.estimate_compression(#{label: \"person\", path: \"name\"}); [e.values_found, e.candidates.len(), e.warnings[0].code] }" => "[4, 10, \"nothing_qualifies\"]";
    "traversal" "execute":
        "Runs the traversal and returns an Execution with results, error and optionally the profile; it never throws (see the book page Profiling and execute()).",
        "g.v().count().execute().results" => "[6]";
    "traversal" "export_graphml":
        "Writes the whole graph to a GraphML file at the given path (needs file access).",
        "g.export_graphml(\"modern.graphml\")" => NOT_RUN;
    "traversal" "export_graphson":
        "Writes the whole graph to a GraphSON file at the given path (needs file access).",
        "g.export_graphson(\"modern.json\")" => NOT_RUN;
    "traversal" "export_snapshot":
        "Writes the whole graph to a packed snapshot file (.gsnap) at the given path (needs file access).",
        "g.export_snapshot(\"modern.gsnap\")" => NOT_RUN;
    "traversal" "fail":
        "Stops the traversal with an error carrying the given message when a traverser reaches it.",
        "g.v(1).fail(\"stop here\").to_list()" => "error: stop here";
    "traversal" "filter":
        "Keeps the traverser when the child traversal yields a result (or the predicate matches).",
        "g.v().filter(__.out_e(\"created\")).values(\"name\").to_list()" => "[\"marko\", \"josh\", \"peter\"]";
    "traversal" "flat_map":
        "Replaces each traverser with all results of the child traversal.",
        "g.v(1).flat_map(__.out(\"knows\")).values(\"name\").to_list()" => "[\"josh\", \"vadas\"]";
    "traversal" "fold":
        "Collects the whole stream into one list; with a seed and an Operator it reduces the stream instead.",
        "g.v().has_label(\"person\").values(\"age\").fold(0, Operator.sum).next()" => "123";
    "traversal" "format":
        "Builds a string from a template, filling %{key} with the element's properties or map values (see the book page Step - format).",
        "g.v(1).format(\"%{name} is %{age}\").next()" => "marko is 29";
    "traversal" "from":
        "Sets the out vertex of the edge add_e() creates: a vertex id, a path label or a child traversal; for path() it starts the window at a label.",
        "g.add_e(\"knows\").from(\"2\").to(\"4\").out_v().values(\"name\").next()" => "vadas";
    "traversal" "get_schema":
        "Returns the graph's stored schema as a map (unit when none is stored).",
        "g.get_schema() == ()" => "true";
    "traversal" "get_query":
        "Returns a saved query as a map (name, folder, description, language, dialect, params, status, writes, effects, denied_writes, body, meta), unit when there is none.",
        "{ g.define_query(#{name: \"add\", body: \"fn add() { g.add_v(\\\"person\\\").next() }\"}); g.get_query(\"add\")[\"effects\"].map(|e| e.text) }" => "[\"Create Data(vertex \\\"person\\\")\"]";
    "traversal" "glob_path":
        "Walks the tree below each vertex along a glob pattern of edge names, like a file path (Graphersal extension, see the book page Step - glob_path).",
        "GraphSource::file_tree().v().has(\"name\", \"/\").glob_path(\"*\").values(\"name\").to_list()" => "[]";
    #[cfg(feature = "gql")]
    "traversal" "gql":
        "Runs a GQL query (experimental: MATCH, OPTIONAL MATCH, WHERE, LET, FILTER, WITH, RETURN with aggregates, ORDER BY, OFFSET, LIMIT, quantified edges and path patterns, path variables, and the writes INSERT / CREATE, SET, REMOVE, [DETACH] DELETE and MERGE; $name reads a value of the optional parameter map) at the start of a traversal; each row is a map from column name to value, and Gremlin steps may follow.",
        "g.gql(\"MATCH (p:person) WHERE p.age > $min RETURN p.name AS name ORDER BY name\", #{min: 30}).to_list()" => "[#{\"name\": \"josh\"}, #{\"name\": \"peter\"}]";
    "traversal" "group":
        "Groups the stream into a map: key by the first by(), values by the second by() (default: the elements); with a key it is a side effect.",
        "g.v().group().by(T.label).by(\"name\").next()" => "#{\"person\": [\"marko\", \"vadas\", \"josh\", \"peter\"], \"software\": [\"lop\", \"ripple\"]}";
    "traversal" "group_count":
        "Counts the stream per key (the first by(), default the element) into a map; with a key it is a side effect.",
        "g.v().group_count().by(T.label).next()" => "#{\"person\": 4, \"software\": 2}";
    "traversal" "has":
        "Keeps elements that have the property (and, with a value or predicate, whose value matches, a string operand being a value, never a path label; with a traversal, for whose value the traversal yields a result, like filter(__.values(key)...)); with T.id or T.label first it tests the id or label; the 3-argument form also tests the label.",
        "{ [g.v().has(\"person\", \"age\", P.gt(30)).count().next(), g.v().has(\"name\", __.to_lower().is(\"josh\")).count().next(), g.v().has(T.label, __.is(\"software\")).count().next()] }" => "[2, 1, 2]";
    "traversal" "has_id":
        "Keeps elements whose id is one of the given ids (ids are strings, see the book page Id Arguments).",
        "g.v().has_id(1, 2).values(\"name\").to_list()" => "[\"marko\", \"vadas\"]";
    "traversal" "has_key":
        "Keeps properties whose key is one of the given keys.",
        "g.v(1).properties().has_key(\"age\").value().to_list()" => "[29]";
    "traversal" "has_label":
        "Keeps elements whose label is one of the given labels (or matches a predicate); without arguments it keeps the elements that have a label, so not(__.has_label()) selects the unlabeled ones (Graphersal extension).",
        "{ g.add_v().to_list(); [g.v().has_label(\"software\").count().next(), g.v().not(__.has_label()).count().next()] }" => "[2, 1]";
    "traversal" "has_not":
        "Keeps elements that do not have the given property.",
        "g.v().has_not(\"age\").values(\"name\").to_list()" => "[\"lop\", \"ripple\"]";
    "traversal" "has_p":
        "Graphersal form of has(key, predicate) (a bare value means P.eq); a string operand is always a value, never a path label (only where() reads labels; before 0.2.2 a string naming a label compared with the labelled element).",
        "g.v(1).as(\"vadas\").out(\"knows\").has_p(\"name\", P.eq(\"vadas\")).values(\"name\").to_list()" => "[\"vadas\"]";
    "traversal" "has_value":
        "Keeps properties whose value is one of the given values (or matches a predicate, whose string operand is a value, never a path label).",
        "g.v(1).properties().has_value(\"marko\").key().to_list()" => "[\"name\"]";
    "traversal" "has_value_any":
        "Graphersal alias of has_value(v1, v2, ...).",
        "g.v().properties(\"name\").has_value_any(\"marko\", \"josh\").value().to_list()" => "[\"marko\", \"josh\"]";
    "traversal" "has_value_p":
        "Graphersal form of has_value(predicate); a string operand is always a value, never a path label (only where() reads labels; before 0.2.2 a string naming a label compared with the labelled element).",
        "g.v(1).as(\"vadas\").out(\"knows\").properties(\"name\").has_value_p(P.eq(\"vadas\")).value().to_list()" => "[\"vadas\"]";
    "traversal" "id":
        "Yields the id of each vertex or edge (a string), never a property named id.",
        "g.v().has(\"name\", \"josh\").id().to_list()" => "[\"4\"]";
    "traversal" "identity":
        "Passes every traverser through unchanged.",
        "g.v().identity().count().next()" => "6";
    "traversal" "import_graphml":
        "Imports a GraphML file into the graph as one unit (needs file access).",
        "g.import_graphml(\"modern.graphml\")" => NOT_RUN;
    "traversal" "import_graphson":
        "Imports a GraphSON file into the graph as one unit (needs file access).",
        "g.import_graphson(\"modern.json\")" => NOT_RUN;
    "traversal" "in":
        "Moves to the adjacent vertices over incoming edges, optionally only edges with the given labels.",
        "g.v(3).in(\"created\").values(\"name\").to_list()" => "[\"peter\", \"josh\", \"marko\"]";
    "traversal" "in_e":
        "Moves to the incoming edges, optionally only edges with the given labels.",
        "g.v(3).in_e(\"created\").count().next()" => "3";
    "traversal" "in_v":
        "Moves from an edge to its in (head) vertex.",
        "g.e(\"0\").in_v().values(\"name\").next()" => "vadas";
    "traversal" "index":
        "Pairs every element of the incoming list with its position ([value, index]).",
        "g.inject([\"a\", \"b\"]).index().next()" => "[[\"a\", 0], [\"b\", 1]]";
    "traversal" "indexes":
        "Lists the property indexes as maps: name, element, label, key, type, transform, served (with a reason when it is not) and the current entries, distinct_values, skipped and bytes.",
        "{ g.create_index(#{label: \"person\", key: \"age\"}); g.indexes().map(|i| i.entries) }" => "[4]";
    "traversal" "infer_schema":
        "Infers a schema map from the data: on g from the whole graph, after steps from the elements the traversal yields.",
        "g.infer_schema()[\"vertices\"].keys()" => "[\"person\", \"software\"]";
    "traversal" "inject":
        "Inserts the given values into the stream (on g it starts a traversal with them).",
        "g.inject(1, 2, 3).sum().next()" => "6";
    "traversal" "intersect":
        "Keeps the elements of the incoming list that are also in the given list.",
        "g.inject([1, 2, 3]).intersect([2, 3, 4]).next()" => "[2, 3]";
    "traversal" "is":
        "Keeps values equal to the given value or matching the predicate; a string operand is always a value, never a path label (only where() reads labels; before 0.2.2 a string naming a label compared with the labelled element).",
        "g.v(1).as(\"vadas\").out(\"knows\").values(\"name\").is(P.eq(\"vadas\")).to_list()" => "[\"vadas\"]";
    "traversal" "iterate":
        "Runs the traversal for its side effects and returns unit; the results are discarded, never materialized.",
        "{ g.add_v(\"x\").property(\"n\", 1).iterate(); g.v().has_label(\"x\").count().next() }" => "1";
    "traversal" "json_path":
        "Reads a nested value from each element with a JSONPath (Graphersal extension, see the book page Path keys (jpath)).",
        "g.inject(#{a: #{b: [1, 2]}}).json_path(\"a.b[1]\").next()" => "2";
    "traversal" "key":
        "Yields the key of each property.",
        "g.v(1).properties().key().to_list()" => "[\"name\", \"age\"]";
    "traversal" "l_trim":
        "Removes leading whitespace (Unicode White_Space) from each string.",
        "g.inject(\"  a \").l_trim().next()" => "a ";
    "traversal" "label":
        "Yields the label of each vertex or edge, never a property named label.",
        "g.v(1).out_e().label().to_list()" => "[\"knows\", \"created\", \"knows\"]";
    "traversal" "labels":
        "Yields the full label set of each vertex as a list (Graphersal multi-label vertices).",
        "g.v(1).labels().next()" => "[\"person\"]";
    "traversal" "length":
        "Yields the length of each string in characters (Unicode scalar values, see the book page Type Conversion and length()).",
        "g.v(1).values(\"name\").length().next()" => "5";
    "traversal" "limit":
        "Passes on only the first n traversers; with Scope.local the first n elements of each collection.",
        "g.v().values(\"name\").limit(2).to_list()" => "[\"marko\", \"vadas\"]";
    "traversal" "local":
        "Runs the child traversal separately for each traverser (per-object scope).",
        "g.v().has_label(\"person\").local(__.out_e().limit(1)).label().to_list()" => "[\"knows\", \"created\", \"created\"]";
    "traversal" "loops":
        "Within repeat(): yields the current iteration count (of the named loop).",
        "g.v(1).repeat(__.out()).until(__.loops().is(2)).values(\"name\").to_list()" => "[\"ripple\", \"lop\"]";
    "traversal" "map":
        "Replaces each traverser with the first result of the child traversal.",
        "g.v(1).map(__.out().count()).next()" => "3";
    "traversal" "mark":
        "Records a named mark in the graph's journal for point-in-time recovery; needs a journal (see the book page Persistence).",
        "g.mark(\"before-import\")" => "error: no journal is registered";
    "traversal" "math":
        "Evaluates an arithmetic expression; _ is the incoming value and other names are path labels or by() values (see the book page Math Expressions).",
        "g.v(1).values(\"age\").math(\"_ * 2\").next()" => "58.0";
    "traversal" "max":
        "Yields the largest value of the stream (Scope.local: of each collection; see the book page Local Scope).",
        "g.v().values(\"age\").max().next()" => "35";
    "traversal" "mean":
        "Yields the arithmetic mean of the stream (Scope.local: of each collection).",
        "g.v().values(\"age\").mean().next()" => "30.75";
    "traversal" "median":
        "Yields the median of the numbers in the stream.",
        "g.v().values(\"age\").median().next()" => "30.5";
    "traversal" "memory_usage":
        "Returns the graph's own memory footprint in bytes (structure, indexes, properties).",
        "g.memory_usage().total_bytes > 0" => "true";
    "traversal" "merge":
        "Merges the given list (union without duplicates) or map (keys of the argument win) into the incoming one.",
        "g.inject([1, 2]).merge([2, 3]).next()" => "[1, 2, 3]";
    "traversal" "merge_e":
        "Upsert of an edge: finds the edge matching the map (T.label, Direction.OUT/IN ids, properties) or creates it (see the book page Upserts).",
        "g.merge_e(#{\"T.label\": \"knows\", \"Direction.OUT\": \"1\", \"Direction.IN\": \"2\"}).id().next()" => "0";
    "traversal" "merge_v":
        "Upsert of a vertex: finds the vertices matching the map or creates one; option(Merge.onCreate/onMatch, ..) adds properties (see the book page Upserts).",
        "g.merge_v(#{\"T.label\": \"person\", name: \"marko\"}).values(\"age\").next()" => "29";
    "traversal" "min":
        "Yields the smallest value of the stream (Scope.local: of each collection; see the book page Local Scope).",
        "g.v().values(\"age\").min().next()" => "27";
    "traversal" "move_query":
        "Moves a saved query into a folder (\"\" or () for the root); callers name it without the folder.",
        "{ g.define_query(#{name: \"n\", body: \"fn n() { 1 }\"}); g.move_query(\"n\", \"archive\"); g.get_query(\"n\")[\"folder\"] }" => "archive";
    "traversal" "next":
        "Runs the traversal and returns its first result (unit when there is none).",
        "g.v().values(\"name\").next()" => "marko";
    "traversal" "none":
        "Keeps a list where no element matches the predicate.",
        "g.inject([1, 2, 3]).none(P.gt(5)).next()" => "[1, 2, 3]";
    "traversal" "not":
        "Keeps the traverser when the child traversal yields nothing.",
        "g.v().not(__.out_e()).values(\"name\").to_list()" => "[\"vadas\", \"lop\", \"ripple\"]";
    "traversal" "option":
        "Adds a branch to choose()/branch() for a key (a value, Pick.none, Pick.any), or the onCreate/onMatch map of merge_v()/merge_e().",
        "g.v(1, 2).choose(__.values(\"age\")).option(29, __.constant(\"a\")).option(Pick.none, __.constant(\"b\")).to_list()" => "[\"a\", \"b\"]";
    "traversal" "optional":
        "Emits the results of the child traversal, or the traverser itself when it yields nothing.",
        "g.v(2).optional(__.out()).values(\"name\").to_list()" => "[\"vadas\"]";
    "traversal" "or":
        "Keeps the traverser when at least one child traversal yields a result.",
        "g.v().or(__.has(\"age\", P.gt(33)), __.has(\"lang\")).values(\"name\").to_list()" => "[\"lop\", \"ripple\", \"peter\"]";
    "traversal" "order":
        "Sorts the stream (Scope.local: each collection) by the by() modulators, ascending by default.",
        "g.v().values(\"name\").order().to_list()" => "[\"josh\", \"lop\", \"marko\", \"peter\", \"ripple\", \"vadas\"]";
    "traversal" "other_v":
        "Moves from an edge to the vertex at the other end than the one the traverser came from.",
        "g.v(1).out_e(\"knows\").other_v().values(\"name\").to_list()" => "[\"josh\", \"vadas\"]";
    "traversal" "out":
        "Moves to the adjacent vertices over outgoing edges, optionally only edges with the given labels.",
        "g.v(1).out(\"knows\").values(\"name\").to_list()" => "[\"josh\", \"vadas\"]";
    "traversal" "out_e":
        "Moves to the outgoing edges, optionally only edges with the given labels.",
        "g.v(1).out_e().count().next()" => "3";
    "traversal" "out_v":
        "Moves from an edge to its out (tail) vertex.",
        "g.e(\"0\").out_v().values(\"name\").next()" => "marko";
    "traversal" "patch_schema":
        "Applies a JSON Merge Patch to the stored schema (checking the data first) and returns the new schema.",
        "g.patch_schema(#{vertices: #{robot: #{schema: #{type: \"object\"}}}})[\"vertices\"].keys()" => "[\"robot\"]";
    "traversal" "path":
        "Yields the path of each traverser (every element it visited), modulated by by() round-robin.",
        "g.v(1).out(\"knows\").path().by(\"name\").to_list()" => "[[\"marko\", \"josh\"], [\"marko\", \"vadas\"]]";
    "traversal" "product":
        "Returns the cartesian product of the incoming list and the given list (list of pairs).",
        "g.inject([1, 2]).product([\"a\"]).next()" => "[[1, \"a\"], [2, \"a\"]]";
    "traversal" "profile":
        "Runs the traversal and returns its metrics per step of the optimized plan (time, traverser counts; ProfileType.Memory adds memory; see the book page Profiling and execute()).",
        "g.v().count().profile().to_map()[\"optimizer_rules_applied\"]" => "[\"count_pushdown\"]";
    "traversal" "profile_with":
        "Rust-API spelling of profile(ProfileType).",
        "g.v().count().profile_with(ProfileType.Memory).to_table().contains(\"Mem\")" => "true";
    "traversal" "project":
        "Turns each traverser into a map with the given keys, one by() per key in order.",
        "g.v(1).project(\"name\", \"degree\").by(\"name\").by(__.both().count()).next()" => "#{\"degree\": 3, \"name\": \"marko\"}";
    "traversal" "properties":
        "Yields the property elements of vertices/edges (all, or the given keys; a property named id or label is one of them, never the element's id or label).",
        "g.v(1).properties(\"age\").value().next()" => "29";
    "traversal" "property":
        "Sets a property on the incoming element (Cardinality.single only); a null value is stored as null, use remove_property() to remove; a key id or label makes an ordinary property, never the element's id or label (see the book page Properties Named id or label).",
        "g.v(1).property(\"age\", 30).values(\"age\").next()" => "30";
    "traversal" "property_json":
        "Sets every key of the given map (or JSON object) as a property of the incoming vertex (Graphersal extension); a key id or label makes an ordinary property, never the element's id or label.",
        "g.v(1).property_json(#{age: 30, tags: [\"a\", \"b\"]}).values(\"tags\").next()" => "[\"a\", \"b\"]";
    "traversal" "queries":
        "Lists the saved queries (of a folder and its subfolders) as maps: name, folder, description, language, dialect, params, status, writes and the effects a call may write.",
        "{ g.define_query(#{name: \"n\", folder: \"a/b\", body: \"fn n() { 1 }\"}); g.queries(\"a\").map(|q| q.name) }" => "[\"n\"]";
    "traversal" "query":
        "Calls a saved query by name with named arguments (start of a traversal only); a query that writes writes in the call, and a traversal result can be continued, read-only (see the book page Saved Queries).",
        "{ g.define_query(#{name: \"people\", body: \"fn people() { g.v().has_label(\\\"person\\\") }\"}); g.query(\"people\").values(\"name\").limit(2).to_list() }" => "[\"marko\", \"vadas\"]";
    "traversal" "r_trim":
        "Removes trailing whitespace (Unicode White_Space) from each string.",
        "g.inject(\" a  \").r_trim().next()" => " a";
    "traversal" "range":
        "Passes on the traversers from position low (inclusive) to high (exclusive); with Scope.local inside each collection.",
        "g.v().values(\"name\").range(1, 3).to_list()" => "[\"vadas\", \"lop\"]";
    "traversal" "recompress":
        "Rebuilds a compression rule's dictionary from the current values and re-encodes them; changes no data.",
        "{ g.define_compression(#{name: \"bio\", label: \"person\", path: \"bio\"}); g.recompress(\"bio\"); g.compressions()[0].compressed_values }" => "0";
    "traversal" "remove_property":
        "Removes the given properties (none = all) from vertices and edges and passes them on (Graphersal extension, see the book page Removing Properties).",
        "g.v(1).remove_property(\"age\").values(\"age\").to_list()" => "[]";
    "traversal" "repeat":
        "Runs the child traversal in a loop, controlled by times(), until() and emit(); a name makes loops(name) readable.",
        "g.v(1).repeat(__.out()).times(2).values(\"name\").to_list()" => "[\"ripple\", \"lop\"]";
    "traversal" "replace":
        "Replaces every occurrence of a substring in each string.",
        "g.v(1).values(\"name\").replace(\"ar\", \"AR\").next()" => "mARko";
    "traversal" "reverse":
        "Reverses each string or list.",
        "g.v(1).values(\"name\").reverse().next()" => "okram";
    "traversal" "sack":
        "Without arguments yields the traverser's sack value; with an Operator, by() feeds a value into the sack (see the book page Sack and Operators).",
        "g.with_sack(1).v(1).out().sack(Operator.sum).by(__.constant(2)).sack().to_list()" => "[3, 3, 3]";
    "traversal" "sample":
        "Passes on n traversers drawn at random (weighted by by(), see the book page Step - sample); with Scope.local samples inside each collection.",
        "g.v().sample(2).count().next()" => "2";
    "traversal" "select":
        "Selects path labels, map keys or side effects (one label: the value, several: a map; an undeclared label is an error, see the book page Repeated Labels: select with Pop); select(T.id)/select(T.label) read the token entries of an element_map() map, select(\"label\") a property named label.",
        "g.v(1).as(\"a\").out(\"created\").as(\"b\").select(\"a\", \"b\").by(\"name\").next()" => "#{\"a\": \"marko\", \"b\": \"lop\"}";
    "traversal" "set_label":
        "Changes the label of each incoming edge and passes it on (Graphersal extension; vertices use add_label()/drop_label()).",
        "g.e(0).set_label(\"met\").label().next()" => "met";
    "traversal" "set_schema":
        "Stores a schema map on the graph, optionally with a SchemaMode; open/closed modes check the existing data first.",
        "{ g.set_schema(g.infer_schema()); g.get_schema()[\"mode\"] }" => "none";
    "traversal" "set_visualizer":
        "Sets how results of g are displayed in this session (a V token such as V.Table or V.Json).",
        "{ g.set_visualizer(V.Json); g.v(1).values(\"name\").to_list() }" => "[\"marko\"]";
    "traversal" "side_effect":
        "Runs the child traversal for its side effects and passes each traverser on unchanged.",
        "g.v(1).side_effect(__.out().aggregate(\"x\")).cap(\"x\").next().len()" => "3";
    "traversal" "simple_path":
        "Keeps only traversers whose path never repeats an element.",
        "g.v(1).both().both().simple_path().count().next()" => "4";
    "traversal" "skip":
        "Skips the first n traversers (Scope.local: elements of each collection).",
        "g.v().values(\"name\").skip(4).to_list()" => "[\"ripple\", \"peter\"]";
    "traversal" "split":
        "Splits each string at a separator into a list.",
        "g.inject(\"a,b,c\").split(\",\").next()" => "[\"a\", \"b\", \"c\"]";
    "traversal" "statistics":
        "Returns the graph's counts: vertices and edges in total and per label.",
        "g.statistics()[\"vertex_labels\"]" => "#{\"person\": 4, \"software\": 2}";
    "traversal" "store":
        "Like aggregate(), but adds each value lazily, without a barrier.",
        "g.v().has_label(\"software\").values(\"name\").store(\"s\").cap(\"s\").next()" => "[\"lop\", \"ripple\"]";
    "traversal" "subgraph":
        "Collects the incoming edges with both endpoints into a graph side effect; cap() it or use to_graph() (see the book page Set Side Effects).",
        "g.v(1).out_e(\"knows\").subgraph(\"sg\").cap(\"sg\").next().v().count().next()" => "3";
    "traversal" "substring":
        "Yields the part of each string from start (to end, exclusive); negative positions count from the end.",
        "g.v(1).values(\"name\").substring(1, 3).next()" => "ar";
    "traversal" "sum":
        "Yields the sum of the numbers in the stream (Scope.local: of each collection).",
        "g.v().values(\"age\").sum().next()" => "123";
    "traversal" "tail":
        "Passes on only the last n traversers (default 1); with Scope.local the last elements of each collection.",
        "g.v().values(\"name\").tail(2).to_list()" => "[\"ripple\", \"peter\"]";
    "traversal" "times":
        "Within repeat(): runs the loop body exactly n times.",
        "g.v(1).repeat(__.out()).times(2).values(\"name\").to_list()" => "[\"ripple\", \"lop\"]";
    "traversal" "to":
        "Sets the in vertex of the edge add_e() creates (vertex id, path label or child traversal); with a Direction it moves like out()/in()/both().",
        "g.v(1).to(Direction.OUT, \"knows\").values(\"name\").to_list()" => "[\"josh\", \"vadas\"]";
    "traversal" "to_e":
        "Moves to the incident edges in the given Direction, optionally only edges with the given labels.",
        "g.v(1).to_e(Direction.OUT, \"knows\").count().next()" => "2";
    "traversal" "to_graph":
        "Runs the traversal and builds a new graph from the edges it yields, both endpoints included (Graphersal terminal).",
        "g.v(1).out_e().to_graph().v().count().next()" => "4";
    "traversal" "to_json":
        "Runs the traversal and renders its results as JSON text.",
        "g.v(1).values(\"name\").to_json()" => "";
    "traversal" "to_json_schema":
        "Runs the traversal and renders the JSON Schema of its results (a schema map as JSON Schema; other results as JSON per line).",
        "g.v(1).values(\"name\").to_json_schema()" => "\"marko\"";
    "traversal" "to_list":
        "Runs the traversal and returns all results as a list.",
        "g.v().has_label(\"software\").values(\"name\").to_list()" => "[\"lop\", \"ripple\"]";
    "traversal" "to_lower":
        "Converts each string to lower case.",
        "g.inject(\"ABC\").to_lower().next()" => "abc";
    "traversal" "to_markdown":
        "Runs the traversal and renders its results as a Markdown table.",
        "g.v(1).values(\"name\").to_markdown()" => "";
    "traversal" "to_mermaid":
        "Runs the traversal and renders its results in Mermaid format (a schema map as a diagram; other results as JSON per line).",
        "g.v(1).values(\"name\").to_mermaid()" => "\"marko\"";
    "traversal" "to_plantuml":
        "Runs the traversal and renders its results in PlantUML format (a schema map as a diagram; other results as JSON per line).",
        "g.v(1).values(\"name\").to_plantuml()" => "\"marko\"";
    "traversal" "to_table":
        "Runs the traversal and renders its results as a text table.",
        "g.v(1).values(\"name\").to_table()" => "";
    "traversal" "to_tree":
        "Runs the traversal and renders its results as an indented tree.",
        "g.v(1).out().to_tree()" => "";
    "traversal" "to_upper":
        "Converts each string to upper case.",
        "g.v(1).values(\"name\").to_upper().next()" => "MARKO";
    "traversal" "to_v":
        "Moves from an edge to its vertex in the given Direction (Direction.OUT: out vertex).",
        "g.e(\"0\").to_v(Direction.IN).values(\"name\").next()" => "vadas";
    "traversal" "tree":
        "Builds a tree map of the paths the traversers took (modulated by by()); with a key it is a side effect (see the book page tree(\"key\") as a Side Effect).",
        "g.v(1).out(\"knows\").tree().by(\"name\").next()" => "#{\"marko\": #{\"josh\": #{}, \"vadas\": #{}}}";
    "traversal" "trim":
        "Removes leading and trailing whitespace (Unicode White_Space, see the book page Type Conversion and length()) from each string.",
        "g.inject(\"  a  \").trim().next()" => "a";
    "traversal" "unfold":
        "Splits a list (or map, into entries) into its elements, one traverser each.",
        "g.inject([1, 2, 3]).unfold().sum().next()" => "6";
    "traversal" "union":
        "Runs every child traversal on the incoming traversers and merges their results (see the book page Branch Children and Global State).",
        "g.v(1).union(__.out(\"knows\"), __.out(\"created\")).values(\"name\").to_list()" => "[\"josh\", \"vadas\", \"lop\"]";
    "traversal" "until":
        "Within repeat(): stops the loop for a traverser when the child traversal yields a result (or the predicate matches).",
        "g.v(1).repeat(__.out()).until(__.has_label(\"software\")).values(\"name\").to_list()" => "[\"lop\", \"ripple\", \"lop\"]";
    "traversal" "v":
        "Starts at the vertices of the graph (all, or the ones with the given ids); after steps it starts a new scan per traverser.",
        "g.v(1, 2).values(\"name\").to_list()" => "[\"marko\", \"vadas\"]";
    "traversal" "validate_schema":
        "Dry run of set_schema(): checks the data against a schema map (optionally with a SchemaMode) and returns {violations, diff, compatible}.",
        "g.validate_schema(g.infer_schema())[\"compatible\"]" => "true";
    "traversal" "validate_schema_patch":
        "Dry run of patch_schema(): checks the patched schema against the data and returns {violations, diff, compatible}.",
        "g.validate_schema_patch(#{vertices: #{robot: #{schema: #{type: \"object\"}}}})[\"compatible\"]" => "true";
    "traversal" "value":
        "Yields the value of each property.",
        "g.v(1).properties(\"name\").value().next()" => "marko";
    "traversal" "value_map":
        "Turns each element into a map of its properties (each value in a list); value_map(true) adds the T.id and T.label tokens, written \"id\" and \"label\" outside the engine where a property of the same name wins.",
        "g.v(1).value_map(\"name\").next()" => "#{\"name\": [\"marko\"]}";
    "traversal" "values":
        "Yields the values of the given (or all) properties; a jpath() key reads a nested value; values(\"label\") reads a property named label, never the element's label.",
        "g.v(1).values(\"name\", \"age\").to_list()" => "[\"marko\", 29]";
    "traversal" "visualize":
        "Runs the traversal and renders its results in the given V format (default: the session's).",
        "g.v(1).values(\"name\").visualize(V.Json)" => "";
    "traversal" "where":
        "Keeps the traverser when the child traversal yields a result, or when a labelled object matches a predicate on another label; where(P.eq(\"a\")) is the step whose string operand names a path label (see the book page where() Start and End Labels).",
        "g.v(1).as(\"a\").out().where(__.in(\"created\").as(\"a\")).values(\"name\").to_list()" => "[\"lop\"]";
    "traversal" "where_p":
        "Graphersal form of where(predicate), modulated by by(); a string operand names a path label first (has() and is() never read labels).",
        "g.v(1).as(\"a\").out(\"created\").in(\"created\").where_p(P.neq(\"a\")).values(\"name\").to_list()" => "[\"peter\", \"josh\"]";
    "traversal" "where_t":
        "Graphersal form of where(traversal).",
        "g.v().where_t(__.out(\"created\")).values(\"name\").to_list()" => "[\"marko\", \"josh\", \"peter\"]";
    "traversal" "with":
        "On g sets an execution option (g.with(\"key\", value), see the book page Execution Options Reference); after a step a WithOptions token configures it.",
        "g.with(\"random.seed\", 7).v().count().next()" => "6";
    "traversal" "with_bulk":
        "g.with_bulk(false) makes every merge keep bulk 1, so equal traversers collapse at barrier().",
        "g.with_bulk(false).v().out().barrier().count().next()" => "4";
    "traversal" "with_path":
        "Records full paths for this traversal (TinkerPop's withPath(); path recording is otherwise derived from the query).",
        "g.with_path().v(1).out().path().count().next()" => "3";
    "traversal" "with_sack":
        "Gives every traverser an initial sack value, optionally with a merge Operator for when traversers merge.",
        "g.with_sack(0).v(1).sack().next()" => "0";
    "traversal" "with_side_effect":
        "Starts the traversal with a named side effect holding the given value (optionally with a reducing Operator).",
        "g.with_side_effect(\"x\", [1]).v(1).values(\"age\").aggregate(\"x\").cap(\"x\").next()" => "[1, 29]";
    "P" "eq":
        "Equal to the value.",
        "g.v().has(\"age\", P.eq(29)).values(\"name\").to_list()" => "[\"marko\"]";
    "P" "neq":
        "Not equal to the value.",
        "g.v().values(\"age\").is(P.neq(29)).to_list()" => "[27, 32, 35]";
    "P" "lt":
        "Less than the value.",
        "g.v().values(\"age\").is(P.lt(29)).to_list()" => "[27]";
    "P" "lte":
        "Less than or equal to the value.",
        "g.v().values(\"age\").is(P.lte(29)).to_list()" => "[29, 27]";
    "P" "gt":
        "Greater than the value.",
        "g.v().values(\"age\").is(P.gt(30)).to_list()" => "[32, 35]";
    "P" "gte":
        "Greater than or equal to the value.",
        "g.v().values(\"age\").is(P.gte(32)).to_list()" => "[32, 35]";
    "P" "inside":
        "Strictly between the two values (both excluded).",
        "g.v().values(\"age\").is(P.inside(27, 32)).to_list()" => "[29]";
    "P" "outside":
        "Below the first or above the second value.",
        "g.v().values(\"age\").is(P.outside(27, 32)).to_list()" => "[35]";
    "P" "between":
        "At least the first and less than the second value.",
        "g.v().values(\"age\").is(P.between(27, 30)).to_list()" => "[29, 27]";
    "P" "within":
        "Equal to one of the values (given as arguments or one list).",
        "g.v().values(\"age\").is(P.within(27, 35)).to_list()" => "[27, 35]";
    "P" "without":
        "Equal to none of the values (given as arguments or one list).",
        "g.v().values(\"age\").is(P.without(27, 35)).to_list()" => "[29, 32]";
    "P" "not":
        "The complement of the given predicate (see the book page Predicates for the deviation from TinkerPop).",
        "g.v().values(\"age\").is(P.not(P.gt(30))).to_list()" => "[29, 27]";
    "P" "typeOf":
        "The value is of the given GType or type name (String, Integer, Double, UUID, ...).",
        "g.v().has(\"age\", P.typeOf(GType.LONG)).count().next()" => "4";
    "TextP" "containing":
        "The string contains the given text.",
        "g.v().values(\"name\").is(TextP.containing(\"ar\")).to_list()" => "[\"marko\"]";
    "TextP" "not_containing":
        "The string does not contain the given text.",
        "g.v().values(\"name\").is(TextP.not_containing(\"o\")).to_list()" => "[\"vadas\", \"ripple\", \"peter\"]";
    "TextP" "starting_with":
        "The string starts with the given text.",
        "g.v().values(\"name\").is(TextP.starting_with(\"m\")).to_list()" => "[\"marko\"]";
    "TextP" "not_starting_with":
        "The string does not start with the given text.",
        "g.v().values(\"name\").is(TextP.not_starting_with(\"r\")).to_list()" => "[\"marko\", \"vadas\", \"lop\", \"josh\", \"peter\"]";
    "TextP" "ending_with":
        "The string ends with the given text.",
        "g.v().values(\"name\").is(TextP.ending_with(\"o\")).to_list()" => "[\"marko\"]";
    "TextP" "not_ending_with":
        "The string does not end with the given text.",
        "g.v().values(\"name\").is(TextP.not_ending_with(\"s\")).to_list()" => "[\"marko\", \"lop\", \"josh\", \"ripple\", \"peter\"]";
    "TextP" "regex":
        "The string matches the regular expression (Rust regex dialect, see the book page Predicates).",
        "g.v().values(\"name\").is(TextP.regex(\"^[mj]\")).to_list()" => "[\"marko\", \"josh\"]";
    "TextP" "not_regex":
        "The string does not match the regular expression.",
        "g.v().values(\"name\").is(TextP.not_regex(\"^[mjp]\")).to_list()" => "[\"vadas\", \"lop\", \"ripple\"]";
    "Predicate" "and":
        "Combines two predicates: both must match.",
        "g.v().has(\"age\", P.gt(30).and(P.lt(35))).values(\"name\").to_list()" => "[\"josh\"]";
    "Predicate" "or":
        "Combines two predicates: at least one must match.",
        "g.v().has(\"age\", P.lt(28).or(P.gt(33))).values(\"name\").to_list()" => "[\"vadas\", \"peter\"]";
    "Predicate" "negate":
        "The complement of the predicate (see the book page Predicates for the deviation from TinkerPop).",
        "g.v().has(\"age\", P.gt(30).negate()).values(\"name\").to_list()" => "[\"marko\", \"vadas\"]";
    "global" "jpath":
        "Builds a JSONPath property key for nested values, usable wherever a property name is (see the book page Path keys (jpath)).",
        "g.inject(#{a: #{b: [5, 6]}}).values(jpath(\"a.b[1]\")).to_list()" => "[6]";
    "global" "datetime":
        "A datetime from ISO 8601 text.",
        "datetime(\"2023-08-02T09:30Z\").hour" => "9";
    "global" "date":
        "A date from ISO 8601 text.",
        "date(\"2024-02-29\").day" => "29";
    "global" "localdatetime":
        "A date and time without a zone.",
        "localdatetime(\"2023-08-02T09:30\").hour" => "9";
    "global" "time":
        "A time of day.",
        "time(\"09:30:15\").second" => "15";
    "global" "duration":
        "A duration (PostgreSQL interval).",
        "duration(\"P1Y2M\").months" => "14";
    "global" "now":
        "Script start time, the same for every call (PostgreSQL now()).",
        "now() == now()" => "true";
    "global" "today":
        "The date of now() in UTC or in the given zone.",
        "type_of(today())" => "date";
    "global" "UUID":
        "Returns a new random UUID value.",
        "type_of(UUID())" => "Uuid";
    "UUID" "from_string":
        "Parses a UUID value from its canonical text.",
        "UUID.from_string(\"123e4567-e89b-12d3-a456-426614174000\").as_string()" => "123e4567-e89b-12d3-a456-426614174000";
    "global" "vector":
        "A vector value (an embedding) from a list of numbers: f32, or quantized to f16, i8 or bit.",
        "vector([0.5, -1.0], \"bit\").to_list()" => "[1.0, -1.0]";
    "global" "embed":
        "A vector for a text from an embedder (default hash-ngram/1), computed before any traversal runs.",
        "embed(\"Marko\") == embed(\"marko\")" => "true";
    "Vector" "quantize":
        "The vector in another element kind (f32, f16, i8, bit).",
        "vector([0.5, -1.0]).quantize(\"i8\").dimension" => "2";
    "Vector" "to_list":
        "The values of the vector as a list of floats (dequantized).",
        "vector([0.5, 2]).to_list()" => "[0.5, 2.0]";
    "Uuid" "as_string":
        "The canonical text of the UUID value.",
        "UUID.from_string(\"123e4567-e89b-12d3-a456-426614174000\").as_string().len()" => "36";
    "temporal" "as_string":
        "The canonical ISO 8601 text of a date or time value.",
        "datetime(\"2023-08-02T09:30+02:00\").as_string()" => "2023-08-02T09:30:00+02:00";
    "global" "Fake":
        "A seeded generator of English test data and random numbers: the same seed and calls give the same values; without a seed it picks one (see the book page Generating test data).",
        "Fake(1).full_name()" => "Victoria Vargas";
    "Fake" "seed":
        "The seed the generator (or the root of a fork) was created with; pass it to Fake(seed) to reproduce a run.",
        "Fake(1).seed()" => "1";
    "Fake" "fork":
        "An independent generator for a named stream of the same seed, unaffected by what was drawn before.",
        "Fake(1).fork(\"people\").first_name()" => "Mason";
    "Fake" "int":
        "A random integer from lo to hi, both included.",
        "Fake(1).int(1, 6)" => "4";
    "Fake" "float":
        "A random float from 0.0 (included) to 1.0 (excluded), or from lo to hi.",
        "Fake(1).float(0, 100)" => "56.65615751722809";
    "Fake" "chance":
        "True with the given probability (0.0 to 1.0).",
        "Fake(1).chance(0.5)" => "false";
    "Fake" "pick":
        "A random element of a non-empty array.",
        "Fake(1).pick([\"red\", \"green\", \"blue\"])" => "green";
    "Fake" "shuffle":
        "A copy of the array in random order.",
        "Fake(1).shuffle([1, 2, 3, 4])" => "[1, 2, 4, 3]";
    "Fake" "sample":
        "The given number of distinct elements of the array, in random order.",
        "Fake(1).sample([1, 2, 3, 4], 2)" => "[3, 4]";
    "Fake" "uuid":
        "A random version 4 UUID value drawn from the seed.",
        "Fake(1).uuid().as_string()" => "910a2dec-8902-4cc1-beeb-8da1658eec67";
    "Fake" "first_name":
        "A random English given name.",
        "Fake(1).first_name()" => "Victoria";
    "Fake" "last_name":
        "A random English surname.",
        "Fake(1).last_name()" => "Vasquez";
    "Fake" "full_name":
        "A random given name and surname.",
        "Fake(1).full_name()" => "Victoria Vargas";
    "Fake" "username":
        "A random user name: a lowercase given name, an initial and a number.",
        "Fake(1).username()" => "victoriav971";
    "Fake" "email":
        "A random e-mail address at a reserved example domain (example.com, .org, .net); with a name, one made from that name.",
        "Fake(1).email(\"Ada Lovelace\")" => "ada.lovelace57@example.net";
    "Fake" "phone":
        "A random US phone number in the fictional 555-01xx range.",
        "Fake(1).phone()" => "+1-648-555-0174";
    "Fake" "job_title":
        "A random job title such as Senior Data Analyst.",
        "Fake(1).job_title()" => "Principal Infrastructure Officer";
    "Fake" "street_name":
        "A random street name.",
        "Fake(1).street_name()" => "Lakeview Circle";
    "Fake" "street_address":
        "A random house number and street name.",
        "Fake(1).street_address()" => "5666 Broad Trail";
    "Fake" "city":
        "A random US city name.",
        "Fake(1).city()" => "Irvine";
    "Fake" "state":
        "A random US state name.",
        "Fake(1).state()" => "New Hampshire";
    "Fake" "state_abbr":
        "A random US state postal abbreviation.",
        "Fake(1).state_abbr()" => "NH";
    "Fake" "zip_code":
        "A random five-digit US ZIP code.",
        "Fake(1).zip_code()" => "57062";
    "Fake" "country":
        "A random country name.",
        "Fake(1).country()" => "South Africa";
    "Fake" "address":
        "A random one-line US address whose city and state belong together.",
        "Fake(1).address()" => "5666 Broad Trail, Bakersfield, CA 44960";
    "Fake" "company":
        "A random company name.",
        "Fake(1).company()" => "Vargas & Ryan";
    "Fake" "domain_name":
        "A random domain name.",
        "Fake(1).domain_name()" => "vasquezsalt.info";
    "Fake" "word":
        "A random English word.",
        "Fake(1).word()" => "onyx";
    "Fake" "sentence":
        "A random sentence of 4 to 12 words.",
        "Fake(1).sentence()" => "Salt winter linen linen shadow swift mountain glacier snow.";
    "Fake" "lorem":
        "Lorem ipsum text of a random length from lo to hi bytes, in paragraphs, ending with a period.",
        "Fake(1).lorem(100, 200).len()" => "157";
    "Fake" "lorem_sentence":
        "A random Lorem ipsum sentence of 6 to 14 words.",
        "Fake(1).lorem_sentence()" => "Vel non non accumsan libero est consequat ac excepteur montes sunt.";
    "Fake" "lorem_paragraph":
        "A random Lorem ipsum paragraph of 3 to 7 sentences.",
        "Fake(1).lorem_paragraph().len()" => "300";
    "By" "Property":
        "A by() argument that reads the named property (the same as by(\"name\")).",
        "g.v().has_label(\"person\").order().by(By.Property(\"age\")).values(\"name\").to_list()" => "[\"vadas\", \"marko\", \"josh\", \"peter\"]";
    "By" "JsonPath":
        "A by() argument that reads a nested value by a JSONPath.",
        "g.inject(#{a: #{b: 2}}, #{a: #{b: 1}}).order().by(By.JsonPath(\"a.b\")).to_list()" => "[#{\"a\": #{\"b\": 1}}, #{\"a\": #{\"b\": 2}}]";
    "By" "Traversal":
        "A by() argument that runs a child traversal (the same as by(__.x())).",
        "g.v().has_label(\"person\").order().by(By.Traversal(__.out_e().count())).values(\"name\").to_list()" => "[\"vadas\", \"peter\", \"josh\", \"marko\"]";
    "Cardinality" "single":
        "A property value tagged with Cardinality.single, for merge_v()/merge_e() option maps.",
        "g.merge_v(#{\"T.id\": \"1\"}).option(Merge.onMatch, #{age: Cardinality.single(30)}).values(\"age\").next()" => "30";
    "Cardinality" "list":
        "A property value tagged with Cardinality.list; writing it fails (one value per key, see the book page Upserts).",
        "type_of(Cardinality.list(5))" => "CardinalityValue";
    "Cardinality" "set":
        "A property value tagged with Cardinality.set; writing it fails (one value per key, see the book page Upserts).",
        "type_of(Cardinality.set(5))" => "CardinalityValue";
    "CastPolicy" "Default":
        "A cast policy that replaces a value that does not convert with the given fallback.",
        "g.inject(\"x\", \"1\").as_number(GType.LONG, CastPolicy.Default(0)).to_list()" => "[0, 1]";
    "Set" "of":
        "A set value (duplicates dropped) of the given values, e.g. to seed a set side effect.",
        "g.with_side_effect(\"s\", Set.of()).v().out().values(\"name\").aggregate(\"s\").cap(\"s\").next()" => "[\"josh\", \"lop\", \"vadas\", \"ripple\"]";
    "Set" "of_list":
        "A set value (duplicates dropped) of the elements of the given list.",
        "g.with_side_effect(\"s\", Set.of_list([\"lop\"])).v().out().values(\"name\").aggregate(\"s\").cap(\"s\").next()" => "[\"lop\", \"josh\", \"vadas\", \"ripple\"]";
    "GraphSource" "empty":
        "Creates a new empty graph, private to the script, and returns its traversal source.",
        "GraphSource::empty().v().count().next()" => "0";
    "GraphSource" "tinkerpop_modern":
        "Creates a private copy of TinkerPop's modern sample graph (6 vertices, 6 edges).",
        "GraphSource::tinkerpop_modern().e().count().next()" => "6";
    "GraphSource" "file_tree":
        "Creates a private sample graph of a file tree (directories, files, symlinks), as used by glob_path().",
        "GraphSource::file_tree().v().count().next()" => "37";
    "GraphSource" "file":
        "Loads a graph file (.xml/.graphml, GraphSON .json) into a new private graph (needs file access).",
        "GraphSource::file(\"modern.graphml\").v().count().next()" => NOT_RUN;
    "V" "Auto":
        "The default display format: chosen by the result's shape.",
        "g.v(1).values(\"name\").visualize(V.Auto)" => "";
    "V" "Table":
        "Display format: a text table.",
        "g.v(1).values(\"name\").visualize(V.Table)" => "";
    "V" "Json":
        "Display format: JSON text.",
        "g.v(1).values(\"name\").visualize(V.Json)" => "";
    "V" "JsonSchema":
        "Display format: the JSON Schema of the results (a schema map as JSON Schema; other results as JSON per line).",
        "g.v(1).values(\"name\").visualize(V.JsonSchema)" => "\"marko\"";
    "V" "Markdown":
        "Display format: a Markdown table.",
        "g.v(1).values(\"name\").visualize(V.Markdown)" => "";
    "V" "Mermaid":
        "Display format: Mermaid (a schema map as a diagram; other results as JSON per line).",
        "g.v(1).values(\"name\").visualize(V.Mermaid)" => "\"marko\"";
    "V" "PlantUML":
        "Display format: PlantUML (a schema map as a diagram; other results as JSON per line).",
        "g.v(1).values(\"name\").visualize(V.PlantUML)" => "\"marko\"";
    "V" "Tree":
        "Display format: an indented tree.",
        "g.v(1).out().visualize(V.Tree)" => "";
    "Execution" "to_json":
        "The execution (results, error, profile) as JSON text.",
        "g.v().count().execute().to_json()" => "{\"results\":[6],\"error\":null,\"profile\":null}";
    "Execution" "to_map":
        "The execution as a map: results, error, profile, mutated, changes.",
        "g.v().count().execute().to_map()[\"results\"]" => "[6]";
    "TraversalMetrics" "to_json":
        "The profile as JSON text.",
        "g.v().count().profile().to_json().contains(\"count_pushdown\")" => "true";
    "TraversalMetrics" "to_map":
        "The profile as a map: duration_ns, metrics per step, optimizer_rules_applied.",
        "g.v().count().profile().to_map()[\"optimizer_rules_applied\"]" => "[\"count_pushdown\"]";
    "TraversalMetrics" "to_table":
        "The profile as the text table that profile() shows.",
        "g.v().count().profile().to_table().contains(\"v().count()\")" => "true";
    "Map" "keys":
        "The keys of a map with non-string keys (group(), group_count() results).",
        "g.v().has_label(\"person\").group_count().by(\"age\").next().keys()" => "[29, 27, 32, 35]";
    "Map" "values":
        "The values of a map with non-string keys.",
        "g.v().has_label(\"person\").group_count().by(\"age\").next().values()" => "[1, 1, 1, 1]";
    "Map" "len":
        "The number of entries of a map with non-string keys.",
        "g.v().group().by(\"age\").next().len()" => "4";
    "Map" "is_empty":
        "Whether a map with non-string keys has no entries.",
        "g.v().group().by(\"age\").next().is_empty()" => "false";
    "Map" "contains":
        "Whether a map with non-string keys has the given key.",
        "g.v().group().by(\"age\").next().contains(29)" => "true";
    "Map" "contains_key":
        "Whether a map with non-string keys has the given key (alias of contains()).",
        "g.v().group().by(\"age\").next().contains_key(30)" => "false";
    "Map" "to_map":
        "Converts the map to a plain Rhai object map; fails when a key is not a string.",
        "g.v().group_count().by(\"age\").next().to_map()" => "error: non-string key 29";
    "map" "to_graph":
        "Builds the meta-graph of a schema map: one vertex per label, one edge per declared connection (private to the script).",
        "g.infer_schema().to_graph().v().count().next()" => "2";
    "map" "subgraph":
        "The meta-graph of one label of a schema map (private to the script).",
        "g.infer_schema().subgraph(\"person\").v().count().next()" => "1";
    "map" "subgraph_labels":
        "The meta-graph of the given labels of a schema map (private to the script).",
        "g.infer_schema().subgraph_labels([\"person\", \"software\"]).v().count().next()" => "2";
};
