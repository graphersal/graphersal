//! The built-in tool catalog: every tool's name, title, kind, description and input schema,
//! and the read-only selection.

use serde_json::{Value, json};

use crate::answer::with_paging;
use crate::config::{Descriptions, McpConfig, offered_read_only};
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

pub(crate) const QUERY_DESCRIPTION: &str = "Run a Graphersal script against the shared graph and return \
its results as compact JSON, paged: at most max_rows top-level results (default 100) after the \
first offset ones (default 0), max_items items of every nested list or map (default 100), and \
only whole results within the server's answer limit (graph_info: answerLimitChars); a note says \
what was cut and names the next page's offset (pass offset to read on).

GQL (the ISO graph query language, which also accepts Cypher spellings) is supported and is the \
preferred language for reading and writing data. To run a GQL query directly, with no script \
around it, call the `gql` tool and pass the query text as it is (MATCH (p:person) RETURN p.name \
AS name). Inside this tool a GQL query is a script statement: g.gql(\"MATCH (p:person) RETURN \
p.name AS name\", #{param: value}). This tool is for Gremlin, for GQL combined with other \
statements, and for everything GQL does not cover.

Before writing a new query, check list_saved_queries: a saved query the user stored for this \
task is run with run_saved_query (or inside a script as g.query(\"name\", #{param: value})).

The language is Gremlin (Apache TinkerPop) in Rhai syntax: `g` is the traversal source, steps \
chain with `.`, strings use double quotes, lists are `[..]`, maps `#{k: v}`. Both spellings of \
every TinkerPop step work: snake_case (has_label, value_map, out_e, in_) and Gremlin camelCase \
(hasLabel, valueMap, outE, in); Graphersal's own functions (create_index, memory_report, \
define_query, ...) are snake_case only. Anonymous child traversals need an explicit `__.` (`__.out(\"knows\")`, never \
a bare `out()`), tokens are explicit too: `Order.desc`, `T.id`, `T.label`, `P.gt(30)`, \
`P.within([\"a\", \"b\"])`, `Column.keys`, `Scope.local`. A traversal without a terminal is \
executed and all its results returned; terminals such as `.to_list()`, `.next()`, `.count()` \
work as well. A script may have several statements (`let x = ...;`); the last value is the \
result.

Examples:
  g.v().has_label(\"person\").values(\"name\")
  g.V().has(\"person\", \"name\", \"marko\").out(\"knows\").valueMap(true)
  g.v().group_count().by(T.label)
  g.v().has(\"age\", P.gt(30)).order().by(\"age\", Order.desc).limit(5).element_map()
  g.v().has_label(\"person\").project(\"name\", \"friends\").by(\"name\").by(__.out(\"knows\").count())
  g.add_v(\"person\").property(\"name\", \"ada\").property(\"age\", 36)
  g.v().has(\"name\", \"ada\").drop()
  let f = Fake(42); for i in 0..10 { let n = f.full_name(); g.add_v(\"person\").property(\"name\", n).property(\"email\", f.email(n)).property(\"age\", f.int(18, 90)).to_list(); }

The whole script is ONE transaction: it commits only when it succeeds; a failing script changes \
nothing. The answer says whether it changed the graph and the new commit number. An error comes \
back as the engine's diagnostic with a `Help:` line naming the fix: read it before retrying. \
`profile: true` adds the per-step metrics of the optimized plan. The resource \
graphersal://dsl-reference lists every step; graphersal://examples has more examples. Test \
data: `Fake(seed)` (same seed, same values; `f.fork(\"name\")` for independent streams).";

/// The schema format in one line, for the tool descriptions.
pub(crate) const SCHEMA_SHAPE: &str = "{\"mode\": \"open\", \"vertices\": {\"person\": {\"schema\": \
{\"type\": \"object\", \"properties\": {\"name\": {\"type\": \"string\"}, \"age\": {\"type\": \
\"integer\"}}, \"required\": [\"name\"]}}}, \"edges\": {\"knows\": {\"connections\": [{\"from\": \
\"person\", \"to\": \"person\"}], \"schema\": {\"type\": \"object\", \"properties\": {\"weight\": \
{\"type\": \"number\"}}}}}} (each label's properties are a JSON Schema object under \"schema\"; \
mode none/open/closed)";

/// An example JSON Merge Patch of a stored schema, for the tool descriptions.
pub(crate) const PATCH_EXAMPLE: &str = "{\"vertices\": {\"person\": {\"schema\": {\"properties\": \
{\"email\": {\"type\": \"string\"}}}}}} adds an optional property; null deletes a key, e.g. \
{\"vertices\": {\"person\": {\"schema\": {\"properties\": {\"email\": null}}}}}";

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

pub(crate) fn no_args() -> Value {
    json!({"type": "object", "properties": {}})
}

pub(crate) const GQL_DESCRIPTION: &str = "Run a GQL (ISO graph query language, Cypher-like) query against \
the shared graph and return its rows as JSON: one JSON object per row, its keys the RETURN \
columns in the order written (compact JSON, paged like query's: at most max_rows rows, default \
100, after the first offset ones; a note says what was cut and names the next page's offset). It runs the script g.gql(query, params). A query may write \
(INSERT, Cypher CREATE; SET; REMOVE; DELETE; MERGE): it is ONE transaction, a failing query changes nothing, and the answer \
says whether it changed the graph (with the net vertex and edge count changes) and the new \
commit number; run writes only when the user asked for that change (a read-only server refuses \
them). No Rhai escaping is needed: pass the GQL text as it is. GQL support is experimental (its \
subset and answers may still change between versions); the query tool with Gremlin stays the \
complete interface.

Supported: MATCH with comma-separated patterns of node patterns \
(v:Label {key: value}) and edge patterns -[e:LABEL]->, <-[e]-, -[e]- (either direction); label \
expressions with | & ! and %; WHERE (comparisons, AND/OR/NOT/XOR, IS [NOT] NULL, IN, STARTS \
WITH, ENDS WITH, CONTAINS); OPTIONAL MATCH (NULL columns when nothing matches); WITH (Cypher \
pipeline: project, aggregate, DISTINCT, ORDER BY, SKIP, LIMIT, WHERE, then more MATCH clauses); \
LET x = expr and FILTER expr; FOR x IN list [WITH ORDINALITY i | WITH OFFSET i] (Cypher \
UNWIND list AS x): one row per item, e.g. FOR id IN $ids MATCH (n) WHERE element_id(n) = id \
(a NULL list gives no row, a non-list value one row; an equality on element_id or on an \
indexed property after FOR is a direct lookup, not a scan); a query may start with MATCH, \
OPTIONAL MATCH, FOR, UNWIND, LET, WITH, FILTER or RETURN (RETURN 1 + 2 AS x runs on one empty \
row); EXISTS subqueries EXISTS { MATCH <pattern> [WHERE <condition>] } and EXISTS { <pattern> } \
and Cypher pattern predicates (a pattern where a condition stands, naming only variables bound \
before it), TRUE or FALSE (never NULL), anywhere an expression stands, each row kept once (a \
semi-join, unlike a MATCH of the same pattern), e.g. who has no project: MATCH (p:person) WHERE \
NOT EXISTS { (p)-[:created]->() } RETURN p.name AS name ORDER BY name, or MATCH (p:person) \
WHERE NOT (p)-[:created]->() ...; map projections p {.name, .age, .*, f, total: expr} \
(Cypher; NULL for a NULL p, while the GQL record {name: p.name} gives {name: NULL}), list \
comprehensions [x IN list WHERE x > 1 | x * 2] (WHERE and | each optional) and pattern \
comprehensions [(p)-[:knows]->(f) WHERE f.age > 30 | f.name] (per row the list of the \
pattern's matches, [] when none, in no defined order), anywhere an expression stands and \
nested: use them for JSON-shaped answers, one row per entity with its related data inside, e.g. \
MATCH (p:person) RETURN p {.name, friends: [(p)-[:knows]->(f) | f.name]} AS person ORDER BY \
person.name; list predicates all(x IN list WHERE cond), any, none, single (NULL when a NULL \
item could decide) and reduce(acc = 0, x IN list | acc + x); COUNT { <pattern> } (the number \
of matches per row, never NULL) and COLLECT { MATCH ... RETURN x } (the list of its one RETURN \
column per row); the body of EXISTS { }, COUNT { } and COLLECT { } may be a whole query with \
WITH, ORDER BY, LIMIT and RETURN; composite queries q1 UNION q2 (duplicate rows removed; UNION \
ALL keeps them; every operand returns the same column names, in any order; ORDER BY and LIMIT \
belong to their operand) and q1 NEXT q2 (the RETURN columns of q1 are the rows of q2, e.g. \
... UNION ... NEXT RETURN name ORDER BY name LIMIT 3); CALL (p) { MATCH ... RETURN ... } runs \
its body once per incoming row and joins the row with each of the body's rows (a row with none \
is dropped; OPTIONAL CALL keeps it with NULL columns; Cypher CALL { WITH p ... } imports p \
too); the body's aggregates, ORDER BY and LIMIT apply per row, so CALL gives top k per group, \
e.g. each person's heaviest software: MATCH (p:person) CALL (p) { MATCH (p)-[e:created]->(s) \
RETURN s.name AS top ORDER BY e.weight DESC LIMIT 1 } RETURN p.name AS who, top ORDER BY who; \
aggregates count(x), count(*), sum, avg, min, max, collect_list \
(Cypher alias collect), each with DISTINCT, grouped implicitly by the other columns (an \
optional GROUP BY after the RETURN items must list exactly those columns); quantified edges -[:knows]->{1,3}, {n}, {m,}, * (0 or more), + (1 or more) and \
Cypher -[e*1..3]-> (a group variable e is the list of edges); quantified path patterns \
(a) ((x)-[:knows]->(y) WHERE x.age < y.age){1,3} (b) repeat a whole chain with the same \
quantifiers (? = 0 or 1): the outer node before the group is its first node, the one after it \
its last, a variable inside is the list of its nodes or edges, one per iteration (with ? or no \
quantifier a single value, NULL when ? matched nothing), the body WHERE reads only body \
variables, every edge at most once per match, e.g. people one or two co-author hops apart \
(sharing software): MATCH (a:person) ((x)-[:created]->(s)<-[:created]-(y)){1,2} (b) RETURN DISTINCT a.name, \
b.name; path variables p = (a)-->(b) \
returned as a path value, path_length(p) (Cypher alias length), nodes(p) and \
relationships(p) (lists of the path's vertices and edges); CASE, coalesce, nullif; scalar \
functions: conversions CAST(x AS INTEGER|FLOAT|STRING|BOOLEAN) (strict: bad text fails the query \
with GQLSTATUS 22018) vs Cypher toInteger, toFloat, toBoolean, toString (bad text gives NULL) \
and their OrNull forms (NULL for any other kind too); strings upper/toUpper, lower/toLower, \
substring (0-based), left, right, split, replace, reverse, trim, btrim, ltrim, rtrim, \
char_length, size, || concatenation; numbers abs, ceil, floor, round(x[, precision[, mode]]), \
sign, sqrt, exp, ln, log, log10, power, mod; lists head, last, tail, range, size, isEmpty; \
elements keys, properties, labels, type, element_id, startNode(e) and endNode(e) (the edge's \
stored direction), a IS [NOT] SOURCE OF e / DESTINATION OF e; dates and times with \
Cypher's temporal functions: date('2015-07-21'), localtime('12:31'), localdatetime(..), \
datetime('2015-07-21T21:40+01:00') (ISO names local_time, local_datetime, zoned_datetime) from \
ISO 8601 text or a map such as date({year: 2015, week: 30, dayOfWeek: 2}) or datetime({year: \
1984, month: 10, day: 11, hour: 12, timezone: 'Europe/Berlin'}) (no argument: the time the query \
started), duration('P1M2DT3H') or duration({days: 14, hours: 16}); components x.year, x.month, \
x.week, x.dayOfWeek, x.hour, x.epochMillis, dur.days; arithmetic d + duration('P1M'), dur * 2; \
date.truncate('month', d) (also localtime / localdatetime / datetime.truncate); \
duration.between(a, b), duration.inDays(a, b); time('10:00+01:00') (ISO zoned_time) a time \
of day with its offset; the ISO literals DATE '2015-07-21', TIME '10:00+01:00', TIMESTAMP \
'2015-07-21T21:40', DURATION 'P1D' and the clocks CURRENT_DATE, CURRENT_TIMESTAMP, \
LOCAL_TIMESTAMP; values hold nanoseconds and a datetime keeps its UTC offset and the zone name \
it was built with; RETURN (mandatory; expressions \
with AS aliases, DISTINCT, RETURN *); ORDER BY (ASC/DESC; NULLs last ascending), OFFSET (or \
SKIP) and LIMIT; $name parameters read from params (values, never spliced into the text); \
writes: INSERT (Cypher alias CREATE) of node patterns (n:Label&Other {key: expr}) and directed \
edge patterns (a)-[:LABEL {key: expr}]->(b) (exactly one edge label; a bound variable stands \
for an existing node), once per incoming row, e.g. MATCH (p:person {name: 'marko'}) INSERT \
(p)-[:likes]->(f:fruit {name: 'apple'}) RETURN f.name AS fruit; a NULL property value stores \
nothing; SET changes the elements of each incoming row, its items left to right: SET n.k = expr \
(one property), SET n = {k: expr} (replaces every property), SET n += {k: expr} (Cypher: merges \
the map in), SET n:Label (adds labels to a node; an edge has exactly one label, change it with \
Gremlin set_label()), e.g. MATCH (p:person {name: 'marko'}) SET p.age = p.age + 1, p:admin \
RETURN p.age AS age; REMOVE n.k and REMOVE n:Label remove a property or a label (nothing \
happens when it is absent); SET n.k = NULL removes the property too, and a NULL target (from \
OPTIONAL MATCH) is skipped; DETACH DELETE n removes a node with all its edges, e.g. MATCH \
(p:person {name: 'marko'}) DETACH DELETE p; DELETE x (also written NODETACH DELETE) removes \
nodes, edges, paths and lists of them, but a node that still has edges fails the query (G1001, \
nothing removed) unless the same clause deletes those edges, e.g. MATCH (a)-[e:knows]->(b) \
DELETE e or MATCH (n)-[r]-() DELETE r, n; a NULL target is skipped; after DELETE, reading or \
returning the deleted element is an error (only count(n), n IS NULL and type(r) still work): \
read what you need first, e.g. MATCH (p:person {name: 'vadas'}) WITH p, properties(p) AS props \
DETACH DELETE p RETURN props; MERGE <pattern> (Cypher compatibility form) finds or creates one \
pattern per row: every match is a row, and when nothing matches the whole pattern is created, \
reusing the nodes bound before (written alone, as (a)); each row sees what earlier rows created, \
so it is idempotent and FOR creates a repeated item once, e.g. create-if-missing: MERGE \
(c:city {name: 'Berlin'}) RETURN c.name AS name (run twice, one city), an edge between matched \
nodes: MATCH (a:person {name: 'marko'}), (b:person {name: 'peter'}) MERGE (a)-[e:knows]->(b) \
RETURN e; ON CREATE SET and ON MATCH SET items (SET items, in written order) change what was \
created or matched, e.g. FOR n IN $names MERGE (p:person {name: n}) ON CREATE SET p.visits = 1 \
ON MATCH SET p.visits = coalesce(p.visits, 0) + 1 RETURN p.name AS name, p.visits AS visits; \
an edge without a direction -[:T]- matches either direction and is created from left to right; \
a NULL property value in the pattern fails the query (22004); \
saved queries as procedures: CALL folder.name(args) YIELD col [AS x], .. [WHERE cond] runs the \
saved query `name` (written in GQL, see list_saved_queries) of the folder `folder` (CALL name() \
for the root folder) once per incoming row, its arguments constants or $params in the order its \
body first reads its parameters (trailing ones left out take their defaults); YIELD names the \
RETURN columns read after it, e.g. CALL people.adults(30) YIELD name RETURN name ORDER BY name; \
a call alone is the whole query and returns every column (YIELD * the same); a saved query \
without RETURN keeps each row once; its writes are part of the query's one transaction; \
each clause sees every write of the clauses before it; a query may \
end with its write clause or FINISH (no rows), or RETURN what it wrote. \
String literals use single quotes. A missing property is NULL. Edges in one match are distinct \
(trail semantics), also inside quantified edges and path patterns. Vertices and edges in a row come back as {id, \
label, properties}. Rows come in no defined order unless the query has ORDER BY: add one when \
the order matters. Not yet supported (refused with a GQLSTATUS error and a Help line): \
a quantifier nested inside a quantified path pattern, EXCEPT and INTERSECT (filter the first query's \
rows with a pattern predicate instead, e.g. WHERE NOT (p)-[:created]->()), SHORTEST, CALL { } IN TRANSACTIONS, vendor \
procedures (CALL db.labels(), apoc.*), a CALL of a saved query written in Rhai (run it with \
run_saved_query) or with an argument that reads the row, non-deterministic functions (rand, randomUUID, date.realtime()), trigonometry, \
and the write \
FOREACH (write FOR x IN list INSERT .. instead); use the query tool with Gremlin for the rest, e.g. g.gql(\"MATCH (p:person) RETURN p\").select(\"p\").out(\"created\").

Examples: MATCH (p:person)-[:knows]->(f:person) WHERE f.age > $min RETURN p.name AS who, \
f.name AS friend ORDER BY friend LIMIT 10 with params {\"min\": 30}; MATCH (p:person) \
OPTIONAL MATCH (p)-[:created]->(s) RETURN p.name AS who, count(s) AS projects ORDER BY \
projects DESC, who; FOR name IN $names INSERT (:tag {name: name}) with params {\"names\": \
[\"a\", \"b\"]}; MATCH (p:person {name: $name}) SET p.age = p.age + 1 RETURN p.age AS age with \
params {\"name\": \"vadas\"}; more in the group \
\"GQL\" of the resource graphersal://examples (scripts g.gql(\"...\")).";

pub(crate) fn all_tools() -> Vec<Tool> {
    let tool = |name, title, kind, description: &str, input| Tool {
        name,
        title,
        kind,
        description: description.to_string(),
        input,
    };
    vec![
        tool(
            "query",
            "Run a query",
            Kind::Destructive,
            QUERY_DESCRIPTION,
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
            GQL_DESCRIPTION,
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
            "The saved queries stored in the graph: named, parameterized queries the user wrote \
             for recurring questions and tasks. Each has a name, a folder, a description, its \
             parameters (JSON Schema, default, required, description), a language (rhai: a \
             Gremlin script function; gql: one GQL query whose $name parameters are the \
             declared ones), a status (error: it cannot run) and writes (true: it changes data, \
             effects lists what; run it only when the user asked for that change). Check them \
             before writing a new query, and run one with run_saved_query.",
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
            "Runs a saved query by name with named parameters, exactly like the script \
             g.query(name, #{..}): the parameters are checked against their declared schemas \
             (missing required, wrong type, violated constraint: an error with a Help line), \
             defaults fill the omitted ones, and the answer is paged like query's (offset, \
             max_rows, max_items). A query \
             listed with language: \"gql\" runs the same way (its parameters become the $name \
             values of its GQL text) and answers rows like the gql tool; inside a GQL query \
             the gql tool calls it as CALL folder.name(args) YIELD col (the arguments in the \
             order its text first reads them). A query \
             listed with writes: true changes data (only what its effects name, as one \
             transaction): run it only when the user asked for that change.",
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
            "The compression rules of the graph: strings of one vertex or edge label at one \
             property path that are kept LZ4-compressed in memory (codec lz4) or stored once \
             per distinct value (codec dedup; on a top-level property every value type, each \
             element keeping a 4-byte code); reads are always plain. Each has name, element, \
             label, path, codec, minBytes, dictionary, served (false: the rule is kept but \
             not applied, values stay plain; reason says why, such as a codec of a newer \
             version) and its current stats (compressedValues, plainBytes, storedBytes, \
             dictionaryBytes, distinctValues: the distinct values of a dedup rule).",
            no_args(),
        ),
        tool(
            "estimate_compression",
            "Estimate a compression rule",
            Kind::Read,
            "Measures what compression rules for one vertex or edge label and property path \
             would hold over the current values, WITHOUT changing the graph: lz4 with and \
             without a dictionary at minBytes 64, 128, 256, 512 and 1024 (or only the codec, \
             minBytes and dictionary given; codec [\"lz4\", \"dedup\"] also measures dedup at \
             0, 25, 64, 128 and 256; on a top-level property dedup counts every value type), \
             plus the rule that covers the path today. Answers \
             valuesFound, stringValues, a size histogram, per candidate compressedValues, \
             plainBytes, storedBytes, dictionaryBytes, distinctValues (dedup), savedBytes, ratio \
             and the read (decode) speed, the best candidate's index and warnings (unknown \
             label, no value at the path, values that are not strings, high_cardinality: \
             dedup saves nothing). Run it before define_compression to choose the codec, \
             minBytes and dictionary, and to catch a typo in the label or path.",
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
            "Stores (or replaces) a compression rule: strings of `element`s labelled `label` at \
             `path` (a jpath of names such as \"bio\" or \"details.career.cv\"; no index) are \
             kept compressed in memory when they are at least minBytes long and compress to at \
             most 90 %, or (codec dedup) stored once per distinct value: on a top-level property \
             every value (strings of at least minBytes, default 0) is kept in the rule's \
             dictionary and the element holds a 4-byte code instead of the 32-byte value; under \
             a nested path strings of at least minBytes and more than 24 bytes are shared. The \
             label's existing values are re-encoded at once. lz4 is worth it for large text that \
             queries rarely read (CVs, descriptions, document bodies; a read or a has() \
             decompresses it); dedup for values repeated across many elements (paths, kinds, \
             flags, dates, small lists; reads cost nothing extra, writes look the value up). \
             One rule per element, label and path. advise recommends dedup where at most one \
             value in eight is distinct and it saves at least 1 MiB.",
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
            "Removes a compression rule; its values are decompressed (no data changes).",
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
            "Rebuilds a compression rule's dictionary from the current values and re-encodes \
             them (after much data was added); changes no data.",
            json!({
                "type": "object",
                "properties": {"name": {"type": "string", "description": "The rule's name (list_compressions)."}},
                "required": ["name"],
            }),
        ),
        tool(
            "list_indexes",
            "Property indexes",
            Kind::Read,
            "The property indexes of the graph: the vertices or edges of one label (label null: \
             of every label) ordered by one top-level property, so that v()/e() with \
             has_label(label) and has(key, ..) (equality, P.within, ranges) read only the \
             matching elements. Each has name, element, label, key, type (memory-ordered), transform (null, to_lower, each \
             or [\"each\",\"to_lower\"]), unique, served (false: the definition is kept but no \
             query reads it; reason says why, such as a storage without indexes or a type or \
             transform this version does not know) and its current stats (entries, \
             distinctValues, skipped, bytes; a to_lower index is read only while skipped, the \
             count of non-string values at the key, is 0).",
            no_args(),
        ),
        tool(
            "create_index",
            "Create a property index",
            Kind::Write,
            "Declares a property index on the `element`s labelled `label`, ordered by their \
             top-level property `key`, and builds it from the data. Without `label`: every \
             vertex (edge), also read by has(key, ..) without has_label. Queries that start with g.v().has_label(label) (g.e() for edges) and \
             filter has(key, value), has(key, P.within(..)) or a range \
             (P.gt/gte/lt/lte/between/inside/outside) then read only the matching elements; \
             results stay the same, only their order may differ. It costs \
             memory (see bytes in list_indexes) and a little time on every write of the key. One \
             index per element, label, key, type and transform; `name` defaults to \
             <label>_<key> (<key> without a label, <label>_<key>_edge for edges, then \
             _<transform>), `type` to memory-ordered (an ordered index in \
             memory holding scalar values: null, booleans, numbers, strings, uuids, dates and \
             times). `transform` \"to_lower\" indexes the lowered text of string values for a \
             case-insensitive search written has(key, __.to_lower().is(\"alice\")) (also \
             P.within and ranges; the constants compared as given); \"each\" the elements of a \
             list, for has(key, __.unfold().is(\"x\")). A key the schema declares as a vector, \
             an object, a list (without each) is refused, with to_lower any non-string key.",
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
            "Removes a property index (no data changes; queries scan again).",
            json!({
                "type": "object",
                "properties": {"name": {"type": "string", "description": "The index's name (list_indexes)."}},
                "required": ["name"],
            }),
        ),
        tool(
            "get_schema",
            "Stored schema",
            Kind::Read,
            "The schema stored in the graph (Graphersal's schema format, JSON), or null when \
             the graph stores none (then infer_schema shows what the data has).",
            no_args(),
        ),
        tool(
            "infer_schema",
            "Inferred schema",
            Kind::Read,
            "The schema the data satisfies now (labels, properties, types, edge topology; \
             mode none). Stores nothing; a good start for set_schema.",
            no_args(),
        ),
        tool(
            "validate_schema",
            "Check a schema",
            Kind::Read,
            "Checks a proposed schema against the data without applying it: violations by \
             rule with sample element ids, the diff from the stored schema, and whether it is \
             backward compatible.",
            json!({"type": "object", "properties": {"schema": schema_arg("The proposed schema"), "mode": mode_arg()}, "required": ["schema"]}),
        ),
        tool(
            "validate_schema_patch",
            "Check a schema patch",
            Kind::Read,
            &format!(
                "validate_schema of the stored schema with a JSON Merge Patch (RFC 7396) \
                 applied: {PATCH_EXAMPLE}. Applies nothing."
            ),
            json!({"type": "object", "properties": {"patch": {"type": "object", "description": "A JSON Merge Patch of the stored schema (a JSON string is accepted too)."}}, "required": ["patch"]}),
        ),
        tool(
            "diff_schema",
            "Diff a schema",
            Kind::Read,
            "The changes from the stored schema to a proposed one, each marked backward \
             compatible or not.",
            json!({"type": "object", "properties": {"schema": schema_arg("The proposed schema")}, "required": ["schema"]}),
        ),
        tool(
            "set_schema",
            "Replace the schema",
            Kind::Destructive,
            "Stores a schema (replacing the stored one). In mode open/closed the existing data \
             is validated first: on violations nothing changes and the error lists them. Run \
             validate_schema first. A vector rule (\"from\" on a vector property) added or \
             changed fills its vectors in the same unit while the WHOLE database is LOCKED \
             (every read and write waits); a large fill is refused with the warning text until \
             it is repeated with confirm_lock: true.",
            json!({"type": "object", "properties": {"schema": schema_arg("The new schema"), "mode": mode_arg(), "confirm_lock": confirm_lock_arg()}, "required": ["schema"]}),
        ),
        tool(
            "patch_schema",
            "Patch the schema",
            Kind::Write,
            &format!(
                "Applies a JSON Merge Patch (RFC 7396) to the stored schema and stores the \
                 result (validated against the data like set_schema); returns the new schema. \
                 Example: {PATCH_EXAMPLE}. Without a stored schema, set_schema first (e.g. \
                 the infer_schema result with mode open). A vector rule added or changed fills \
                 its vectors while the WHOLE database is LOCKED; a large fill needs \
                 confirm_lock: true."
            ),
            json!({"type": "object", "properties": {"patch": {"type": "object", "description": "A JSON Merge Patch of the stored schema (a JSON string is accepted too)."}, "confirm_lock": confirm_lock_arg()}, "required": ["patch"]}),
        ),
        tool(
            "statistics",
            "Statistics",
            Kind::Read,
            "Counts of the graph (vertices and edges in total and per label), its memory \
             footprint, the limits every query runs under, and the memory the process holds beside the \
             graph (`process`: loaded models, the AI Chat history, and `process.memory` with the \
             resident set size where the OS reports one and the live heap).",
            no_args(),
        ),
        tool(
            "workload_statistics",
            "Workload statistics",
            Kind::Read,
            "What the workload of this graph did, counted by the engine (only for a graph in a \
             store; level `off` otherwise): `queries` by normalized plan (literals as `?`), \
             `properties` (element, label, key, access: filter_scan, filter_index, read, \
             order_group, similarity, write, remove), `locks` (write.wait, write.hold, \
             read.wait) and scheduled `jobs`, each per channel (playground, mcp, cli, python, \
             job, host) with the windows last_hour, last_two_days and last_ninety_days (count, \
             failed, items, total_micros, max_micros). The page's own drawing and panels are not \
             counted. Rows sorted by total time over 90 days; max_rows per list (default 50). \
             history adds every row's buckets of that tier (its course over time: start in \
             microseconds since the Unix epoch, count, failed, items, total_micros, max_micros).",
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
            "The system journal of this graph: what the instance DID, one record per operational \
             event (store opened or closed, checkpoints, backups, prunes, damage found, \
             rollbacks, permission denials, clock steps), oldest first. Each record: seq (the \
             cursor), time (microseconds since the Unix epoch, UTC), level (error, warn, notice, \
             info), source, kind, message, fields. No data values or query text. Page with \
             after = the answer's next_after; more says that more records follow. newest_first: \
             true answers the NEWEST records first (the recent events at once); go further back \
             with before = the answer's next_before.",
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
            "Recommendations of the storage advisor, WITHOUT changing anything: create a \
             property index for a key filtered often by a full scan, drop an index no query \
             used, compress large strings or store repeated values once (dedup: at most one \
             value in eight distinct, saving at least 1 MiB; savings measured on the data), stop \
             compressing a key read often. Each has evidence, cost, memory_bytes (freed; \
             negative = taken), impact (what it is worth: low, medium, high; for memory its \
             share of the graph), confidence (how much data it rests on) and the exact command \
             applying it (create_index, drop_index, define_compression, drop_compression: \
             apply one only when the user asked for it). The list is ordered by impact. groups \
             sums the recommendations per kind and codec (\"dedup: 74 keys, total -400.1 \
             MiB\") with their ids, for apply_recommendations. Cost of dedup: on a deduped key \
             adding a key new to an element and jpath writes into the value are about 30 % \
             slower, plain overwrites about 5-9 %, reads mostly faster. everything: true lists \
             every candidate (small graphs).",
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
            "Applies the storage advisor's recommendations named by `ids` (the ids advise \
             answered, such as the ids of one of its groups) in ONE commit: the advice is \
             computed again with the same `everything`, then every index and compression rule \
             they name is created or dropped together, and all new compression rules re-encode \
             the data in one pass. All or nothing: an id no longer advised or a refused change \
             (named in the error) applies nothing. Each change is reversible (drop_index, \
             drop_compression). Apply only the recommendations the user picked: the user \
             decides, never apply on your own.",
            json!({
                "type": "object",
                "properties": {
                    "ids": {"type": "array", "items": {"type": "string"}, "minItems": 1, "description": "The recommendation ids from advise (recommendations[].id or groups[].ids)."},
                    "everything": {"type": "boolean", "description": "The same value advise was called with (the advice is recomputed with it)."},
                },
                "required": ["ids"],
            }),
        ),
        tool(
            "list_jobs",
            "List scheduled jobs",
            Kind::Read,
            "The scheduled jobs of this graph's store: the system jobs (stats_flush, compact), \
             then the jobs of users from the catalog: name, kind, schedule (5-field cron in \
             timeZone), enabled, nextAt and the last run (microseconds since the Unix epoch, \
             error), the run counts of the last hour, two days and 90 days, scheduled (with a \
             reason when it does not run); a user job also its action (query with args, \
             checkpoint, backup with path) and effects (what each run may write). Jobs are \
             defined and changed by the user in the playground.",
            no_args(),
        ),
        tool(
            "memory_report",
            "Memory report",
            Kind::Read,
            "Where the graph's memory goes, WITHOUT changing it: one row per (element, label, \
             top-level property) with elements having the key, value types, bytes (slots + \
             heap), a size histogram (non-empty buckets by heap bytes), max value bytes, string \
             figures (inline values, plain bytes, distinct_estimate by HyperLogLog), vectors by \
             element kind and dimension, compressed and deduplicated values, the rules and \
             indexes on it; one row per label (elements, id and label bytes, slot bytes, \
             property bytes, degree max/p95 for vertices); shapes; each property index and \
             compression rule with its bytes; the structure (slots live/tombstoned/spare, id \
             and label index bytes, totals). Facts, not advice. max_elements samples a large \
             graph (figures extrapolated per label, distinct counts then unreliable); limit \
             keeps the property rows and the label rows with the most bytes (default 50), \
             max_rows and max_labels set one list each; the rows left out are counted with \
             their bytes (rows_omitted, rows_omitted_bytes, labels_omitted, \
             labels_omitted_elements, labels_omitted_bytes), the totals cover everything. \
             components says where the whole process memory goes (the graph, embedding \
             models, the script engine, the store, the dev server's MCP and AI Chat state, \
             each with bytes and exact or estimated); unattributed_bytes is the tracked heap \
             no component explains.",
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
            "How this MCP endpoint has been used since the server process started (kept in \
             memory only, lost on a restart, never written): requests per JSON-RPC method, \
             tool calls per tool and per saved query (calls, errors, totalMs, meanMs, maxMs, a \
             latency histogram over the buckets latencyBucketsMs (each count below its bound, \
             the last one above the largest), answerChars), requests and tool calls per client \
             name (clientInfo of initialize, at most clientsMax kept), sessions initialized \
             and active in about the last 5 minutes (an estimate), and refusals by kind \
             (queueFull, queueTimeout, clientLimit, tooManyConnections, stopping, \
             unknownSession, unauthorized, invalidRequest). Lists are sorted by calls; \
             max_rows keeps that many rows per list (default 50), <list>_omitted counts the \
             rest. Offered only when the server's configuration names it.",
            json!({
                "type": "object",
                "properties": {
                    "max_rows": {"type": "integer", "minimum": 1, "description": "Rows per list (tools, savedQueries, clients), most calls first (default 50)."},
                },
            }),
        ),
        tool(
            "graph_info",
            "Graph info",
            Kind::Read,
            "What the server serves: the graph's name, vertex and edge counts, schema mode, \
             source file or Store directory, the read-only view of a past state if the user \
             opened one in the browser (view, and a note: writes are refused while it is \
             open), damage found on the store's disk while it is open (damageFound: writes are \
             refused when it says frozen), whether MCP is read-only, and answerLimitChars \
             (the largest tool answer in characters; longer results are paged with offset).",
            no_args(),
        ),
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
        .collect()
}

/// The tools a configured endpoint offers: [`tools`] of the mode, only those the
/// configuration offers, each with its description (the operator's override, else the short
/// one with `descriptions: short`, else the full one).
pub(crate) fn offered_tools(read_only: bool, config: &McpConfig) -> Vec<Tool> {
    tools(read_only)
        .into_iter()
        .filter(|t| config.offers(t.name))
        .map(|mut t| {
            if let Some(text) = config.override_of(t.name) {
                t.description = text.to_string();
            } else if config.descriptions == Descriptions::Short {
                t.description = short_description(t.name, read_only).to_string();
            }
            t
        })
        .collect()
}

/// The short `query` description of a read-only server.
const QUERY_SHORT: &str = "Run a Graphersal script: Gremlin in Rhai syntax (double-quoted \
strings, `__.` for child traversals, tokens such as P.gt(30), Order.desc, T.label); statements \
end with `;`, the last value is the result, and a traversal before it runs only with a terminal \
(.to_list(), .next(), .iterate()). Compact JSON, paged (offset, max_rows, max_items). Prefer the \
gql tool for plain reads; in a script GQL is g.gql(\"MATCH ...\", #{name: value}) and a saved \
query g.query(\"name\", #{..}). Every step: resource graphersal://dsl-reference. Example: \
g.v().has_label(\"person\").values(\"name\")";

/// The short `query` description of a server that allows writes: [`QUERY_SHORT`] plus the
/// write steps and the transaction.
const QUERY_SHORT_WRITABLE: &str = "Run a Graphersal script: Gremlin in Rhai syntax \
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

/// The short `gql` description of a read-only server (the full support list is the resource
/// `graphersal://gql-reference`).
const GQL_SHORT: &str = "Run a GQL query (ISO graph query language; Cypher spellings \
accepted): the text as it is, strings in single quotes, $name values from params; one JSON \
object per row (elements as {id, label, properties}), paged (offset, max_rows). Ids: \
element_id(n) = 'x', not id(n). Anchor a variable-length pattern -[:T*1..3]-> on a node found by \
a property (an unanchored one can exceed the memory limit). Every clause and function: resource \
graphersal://gql-reference. Example: MATCH (p:person)-[:knows]->(f) WHERE f.age > $min RETURN \
p.name AS who, f.name AS friend ORDER BY who, params {\"min\": 30}";

/// The short `gql` description of a server that allows writes: [`GQL_SHORT`] plus what a
/// model must know to write correctly (the clauses, one transaction, DETACH, MERGE).
const GQL_SHORT_WRITABLE: &str = "Run a GQL query (ISO graph query language; Cypher spellings \
accepted): the text as it is, strings in single quotes, $name values from params; one JSON \
object per row (elements as {id, label, properties}), paged (offset, max_rows). Ids: \
element_id(n) = 'x', not id(n). Anchor a variable-length pattern -[:T*1..3]-> on a node found by \
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

/// The hand-written short description of every built-in tool (`descriptions: short`): one or
/// two sentences and one example where it helps; `query` and `gql` also name the writes when
/// the server allows them (`read_only` false). The input schemas stay the same.
pub(crate) fn short_description(name: &str, read_only: bool) -> &'static str {
    match name {
        "query" if read_only => QUERY_SHORT,
        "query" => QUERY_SHORT_WRITABLE,
        "gql" if read_only => GQL_SHORT,
        "gql" => GQL_SHORT_WRITABLE,
        "list_saved_queries" => {
            "The saved queries stored in the graph (name, folder, description, parameters, \
             language, writes and effects). Check them before writing a new query."
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
            "The schema stored in the graph (JSON), or null (then the infer_schema tool shows \
             what the data has)."
        }
        "infer_schema" => {
            "The schema the data satisfies now (labels, properties, types, edge topology); \
             stores nothing."
        }
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
             validate_schema tool first."
        }
        "patch_schema" => {
            "Apply a JSON Merge Patch to the stored schema, validated like set_schema. Example: \
             {\"vertices\": {\"person\": {\"schema\": {\"properties\": {\"email\": \
             {\"type\": \"string\"}}}}}}"
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
