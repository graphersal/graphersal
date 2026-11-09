//! The resources (`resources/list`, `resources/read`): the schema, the statistics, the DSL
//! reference built from the engine, the playground's examples and the saved queries.

use std::sync::OnceLock;

use serde_json::Value;

use crate::answer::compact;
use crate::host::Ctx;
use crate::tools::saved_queries;

pub(crate) struct Resource {
    pub(crate) uri: &'static str,
    pub(crate) name: &'static str,
    pub(crate) title: &'static str,
    pub(crate) description: &'static str,
    pub(crate) mime: &'static str,
}

pub(crate) const RESOURCES: [Resource; 7] = [
    Resource {
        uri: "graphersal://schema",
        name: "schema",
        title: "Stored schema",
        description: "The stored schema JSON, or null: get_schema with format \"json\".",
        mime: "application/json",
    },
    Resource {
        uri: "graphersal://schema-summary",
        name: "schema-summary",
        title: "Schema summary",
        description: "The full schema summary text with the schema's descriptions (get_schema with descriptions: true).",
        mime: "text/plain",
    },
    Resource {
        uri: "graphersal://statistics",
        name: "statistics",
        title: "Statistics",
        description: "Counts per label, memory footprint and query limits of the graph.",
        mime: "application/json",
    },
    Resource {
        uri: "graphersal://dsl-reference",
        name: "dsl-reference",
        title: "DSL reference",
        description: "Every step and function of the query language with its signatures, a \
                      one-line description and a runnable example with its result, generated \
                      from the engine (about 55 KB).",
        mime: "text/markdown",
    },
    Resource {
        uri: "graphersal://examples",
        name: "examples",
        title: "Examples",
        description: "The playground's runnable example queries (about 8 KB).",
        mime: "text/markdown",
    },
    Resource {
        uri: "graphersal://saved-queries",
        name: "saved-queries",
        title: "Saved queries",
        description: "The saved queries stored in the graph (name, folder, description, \
                      parameters, language rhai or gql, status), as list_saved_queries returns \
                      them: check them before writing a new query.",
        mime: "application/json",
    },
    // Listed only when the configuration names it (`resources`), but always readable: the
    // short `gql` description points to it.
    Resource {
        uri: "graphersal://gql-reference",
        name: "gql-reference",
        title: "GQL reference",
        description: "What the gql tool supports: clauses, patterns, expressions, functions, \
                      what is not supported yet, and examples (the full gql tool description, \
                      about 12 KB).",
        mime: "text/markdown",
    },
];

/// The text of the resource `uri`; `read_only` leaves the saved queries that write out of
/// `graphersal://saved-queries`.
pub(crate) fn read_resource(
    context: &Ctx<'_>,
    uri: &str,
    read_only: bool,
) -> Result<String, String> {
    match uri {
        "graphersal://schema" => Ok(context.call(|s| s.get_schema_text())??),
        "graphersal://schema-summary" => {
            Ok(context.call(|s| s.schema_summary_text(&Value::Null))??)
        }
        "graphersal://gql-reference" => Ok(gql_reference_text()),
        "graphersal://saved-queries" => Ok(compact(&Value::Array(saved_queries(
            context, None, None, read_only,
        )?))),
        "graphersal://statistics" => {
            Ok(compact(&context.host.add_process_statistics(
                context.call(|s| s.statistics_with_process())??,
            )))
        }
        "graphersal://dsl-reference" => {
            static REFERENCE: OnceLock<String> = OnceLock::new();
            if let Some(text) = REFERENCE.get() {
                return Ok(text.clone());
            }
            let completions =
                context.call(|s| graphersal_session::views::completions(s.engine()))?;
            Ok(REFERENCE
                .get_or_init(|| dsl_reference(&completions))
                .clone())
        }
        "graphersal://examples" => {
            let bytes = context
                .host
                .examples_json()
                .ok_or_else(|| "the UI has no js/examples.json".to_string())?;
            let examples: Value = serde_json::from_slice(&bytes)
                .map_err(|err| format!("js/examples.json is not JSON: {err}"))?;
            Ok(examples_text(&examples))
        }
        other => Err(format!("resource not found: {other}")),
    }
}

/// The DSL reference: syntax rules, then one line per function of every receiver.
pub(crate) fn dsl_reference(completions: &Value) -> String {
    let mut out = String::from(
        "# Graphersal DSL reference\n\n\
         Gremlin (Apache TinkerPop) in Rhai syntax. `g` is the traversal source; `__.` starts an \
         anonymous child traversal, always explicit (`__.out(\"knows\")`). TinkerPop's \
         names have a snake_case and a camelCase spelling (`has_label`, `hasLabel`), \
         Graphersal's own functions snake_case only: the steps list their aliases in brackets, \
         other sections only irregular ones. Tokens: `T.id`, \
         `T.label`, `P.eq/neq/lt/lte/gt/gte/inside/outside/between/within/without(..)`, \
         `TextP.containing/startingWith/endingWith(..)`, `Order.asc/desc/shuffle`, \
         `Column.keys/values`, `Scope.local/global`, `Pop.first/last/all/mixed`, \
         `Cardinality.single/list/set`, `Direction.OUT/IN/BOTH`, `GType.LONG/DOUBLE/STRING/..`, \
         `Operator.sum/..`, `DT.second/minute/hour/day/week/month/year/..`. Literals: strings in \
         double quotes, lists `[1, 2]`, maps `#{name: \"x\"}`, `jpath(\"a.b[0]\")` for a nested \
         property key, `datetime(\"2023-08-02T09:30Z\")` and the other date constructors. A \
         traversal without a terminal returns all its results; terminals: `to_list()`, \
         `next()`, `iterate()` (side effects only, returns nothing), `to_graph()`, `profile()`, \
         `execute()` and the visualizers (`to_table()`, `to_json()`, ...). Below: overloads by \
         argument types (`(any…)`: up to 10), an example on the modern graph (a final \
         `to_list()` left out), `→` its result.\n",
    );
    // receiver -> (lines, entries written as another one: (`P.x()`, `gt`) or ("", `a` = `b`))
    #[allow(clippy::type_complexity)]
    let mut sections: Vec<(String, Vec<String>, Vec<(String, String)>)> = Vec::new();
    let mut first_with_doc: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for entry in completions.as_array().into_iter().flatten() {
        let receiver = entry["receiver"].as_str().unwrap_or("other").to_string();
        let name = entry["name"].as_str().unwrap_or("");
        let signatures: Vec<&str> = entry["signatures"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let aliases: Vec<&str> = entry["aliases"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let doc = compact_doc(&doc_line(entry["doc"].as_str().unwrap_or("")));
        let section = match sections.iter().position(|(r, ..)| *r == receiver) {
            Some(at) => at,
            None => {
                sections.push((receiver.clone(), Vec::new(), Vec::new()));
                sections.len() - 1
            }
        };
        // A text shared by several entries (`gt(30)` is `P.gt(30)`, `P.containing` is
        // `TextP.containing`) is written once; the later entries are listed as the first.
        if let Some(first) = first_with_doc.get(&doc) {
            let irregular = aliases.iter().any(|alias| *alias != camel_case(name));
            let names: Vec<&str> = std::iter::once(name).chain(aliases).collect();
            // `gt` = `P.gt()`: grouped under `P.x()`; another name is written out.
            let same_name = first
                .rsplit_once('.')
                .filter(|(_, called)| *called == format!("{name}()") && !irregular);
            sections[section].2.push(match same_name {
                Some((class, _)) => (format!("`{class}.x()`"), name.to_string()),
                None => (
                    String::new(),
                    format!("`{}` = `{first}`", names.join("`/`")),
                ),
            });
            continue;
        }
        let mut line = format!("- {name}{}", compact_signatures(&signatures));
        // Outside the steps a regular camelCase twin (`domain_name` -> `domainName`) goes
        // without saying (the header says so).
        let listed: Vec<&str> = aliases
            .iter()
            .copied()
            .filter(|alias| receiver == "traversal" || *alias != camel_case(name))
            .collect();
        if !listed.is_empty() {
            line.push_str(&format!(" [{}]", listed.join(", ")));
        }
        match first_with_doc.get(&doc) {
            Some(_) => {}
            None if !doc.is_empty() => {
                line.push_str(&format!(": {doc}"));
                let label = match receiver.as_str() {
                    "traversal" | "global" => format!("{name}()"),
                    other => format!("{other}.{name}()"),
                };
                first_with_doc.insert(doc, label);
            }
            None => {}
        }
        sections[section].1.push(line);
    }
    sections.sort_by_key(|(receiver, ..)| receiver != "traversal");
    for (receiver, lines, same) in sections {
        let title = match receiver.as_str() {
            "traversal" => "Steps and source methods (g.x(), __.x(), traversal.x())".to_string(),
            "global" => "Free functions (jpath(..), bare predicates such as gt(30))".to_string(),
            "GraphSource" => "Private graphs (GraphSource::x())".to_string(),
            "Predicate" => "Predicate values (p.x())".to_string(),
            "Uuid" => "UUID values (u.x())".to_string(),
            "Fake" => "Test data generator (let f = Fake(seed); f.x())".to_string(),
            "Map" => "Maps with non-string keys (m.x())".to_string(),
            "map" => "Schema maps (schema.x())".to_string(),
            "Execution" => "execute() results (r.x())".to_string(),
            "TraversalMetrics" => "profile() results (m.x())".to_string(),
            "temporal" => "Date and time values (d.x())".to_string(),
            other => format!("{other}.x()"),
        };
        out.push_str(&format!("\n## {title}\n\n"));
        if !lines.is_empty() {
            out.push_str(&format!("{}\n", lines.join("\n")));
        }
        let mut groups: Vec<(&str, Vec<&str>)> = Vec::new();
        for (class, text) in &same {
            match groups.iter_mut().find(|(c, _)| c == class) {
                Some((_, texts)) => texts.push(text),
                None => groups.push((class, vec![text])),
            }
        }
        for (class, texts) in groups {
            match class {
                "" => out.push_str(&format!("\nThe same functions: {}.\n", texts.join(", "))),
                class => out.push_str(&format!(
                    "\nThe same as {class} under the same name: {}.\n",
                    texts.join(", ")
                )),
            }
        }
    }
    out
}

/// The overloads of one function in the reference's compact form: argument types only (an
/// argument name is kept where the engine has a real one), a type list once, `|` between the
/// overloads, and a variadic function's overloads of any arguments (`(any)`, `(any, any)`,
/// `(array)`, `(arg1, …, arg10)`) as `(any…)`.
pub(crate) fn compact_signatures(signatures: &[&str]) -> String {
    let placeholder = |name: &str| {
        name == "_"
            || name
                .strip_prefix("arg")
                .is_some_and(|n| n.parse::<u32>().is_ok())
    };
    let mut seen: Vec<Vec<&str>> = Vec::new();
    let mut out: Vec<String> = Vec::new();
    let mut variadic = false;
    for signature in signatures {
        let inner = signature.strip_prefix('(').unwrap_or(signature);
        let inner = inner.strip_suffix(')').unwrap_or(inner);
        let params: Vec<(&str, &str)> = match inner.strip_prefix("arg1, …, arg") {
            Some("10") => {
                variadic = true;
                continue;
            }
            Some(max) => vec![("_", "any"); max.parse().unwrap_or(1)],
            None => inner
                .split(", ")
                .filter(|p| !p.is_empty())
                .map(|p| p.split_once(": ").unwrap_or((p, "any")))
                .collect(),
        };
        let types: Vec<&str> = params.iter().map(|(_, ty)| *ty).collect();
        if seen.contains(&types) {
            continue;
        }
        seen.push(types);
        let text: Vec<String> = params
            .iter()
            .map(|(name, ty)| {
                if placeholder(name) {
                    (*ty).to_string()
                } else {
                    format!("{name}: {ty}")
                }
            })
            .collect();
        out.push(format!("({})", text.join(", ")));
    }
    if variadic {
        // `(any…)` stands for the overloads of one to ten arguments of any type.
        out.retain(|s| {
            s == "()"
                || !s[1..s.len() - 1]
                    .split(", ")
                    .all(|ty| ty == "any" || ty == "array")
        });
        out.push("(any…)".to_string());
    }
    out.join("|")
}

/// The regular camelCase spelling of a snake_case name (`domain_name` -> `domainName`).
pub(crate) fn camel_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut upper = false;
    for c in name.chars() {
        if c == '_' {
            upper = true;
        } else if upper {
            out.push(c.to_ascii_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// A [`doc_line`] made shorter for the reference: `→` between an example and its result (a
/// final `to_list()` left out: a traversal without a terminal returns all its results), and
/// without the pointers to book pages, which a client of the reference cannot open.
pub(crate) fn compact_doc(line: &str) -> String {
    const POINTER: &str = "see the book page";
    const SUMMARY_CHARS: usize = 150;
    let mut text = line
        .replace(".to_list()` returns `", "` → `")
        .replace("` returns `", "` → `");
    let (summary, example) = match text.find(" Example: `") {
        Some(at) => text.split_at(at),
        None => (text.as_str(), ""),
    };
    if summary.chars().count() > SUMMARY_CHARS {
        let cut: String = summary.chars().take(SUMMARY_CHARS - 1).collect();
        text = format!("{}…{example}", cut.trim_end_matches('…').trim_end());
    }
    while let Some(at) = text.find(POINTER) {
        let before = &text[..at];
        // `(see the book page X)`: the whole parenthesis; `..., see the book page X`: up to the
        // parenthesis or the sentence it ends.
        let own_parenthesis = before.ends_with('(');
        let start = if own_parenthesis {
            before.trim_end_matches('(').trim_end().len()
        } else {
            let kept = before.trim_end();
            let kept = kept
                .strip_suffix(',')
                .or_else(|| kept.strip_suffix(';'))
                .unwrap_or(kept);
            kept.len()
        };
        let tail = at + POINTER.len();
        let mut depth = 0usize;
        let mut end = text.len();
        for (i, c) in text[tail..].char_indices() {
            let here = tail + i;
            match c {
                '(' => depth += 1,
                ')' if depth > 0 => depth -= 1,
                ')' => {
                    end = if own_parenthesis { here + 1 } else { here };
                    break;
                }
                '.' if depth == 0 && text[here + 1..].starts_with(' ') => {
                    end = here;
                    break;
                }
                // A summary cut short (`doc_line`) ends the pointer too.
                '…' => {
                    end = here;
                    break;
                }
                _ => {}
            }
        }
        text.replace_range(start..end, "");
    }
    text
}

/// A function's doc text (the engine's doc table: one sentence, an empty line, `Example: ...`)
/// on one line, each part at most 240 characters.
pub(crate) fn doc_line(doc: &str) -> String {
    let flat = |text: &str| {
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if text.chars().count() > 240 {
            format!("{}…", text.chars().take(239).collect::<String>())
        } else {
            text
        }
    };
    let (summary, example) = doc.split_once("\n\n").unwrap_or((doc, ""));
    let mut line = flat(summary);
    let example = flat(example);
    if !example.is_empty() {
        line.push(' ');
        line.push_str(&example);
    }
    line
}

/// The playground's examples as Markdown, grouped like the start page.
pub(crate) fn examples_text(examples: &Value) -> String {
    let mut out = String::from(
        "# Graphersal examples\n\nRunnable with the `query` tool. Each group names the sample \
         graph it was written for; the server serves one graph, so adapt labels and keys to it \
         (get_schema / statistics).\n",
    );
    let mut group = String::new();
    for example in examples.as_array().into_iter().flatten() {
        let this = example["group"].as_str().unwrap_or("Examples");
        if this != group {
            group = this.to_string();
            let graph = example["graph"].as_str().unwrap_or("");
            out.push_str(&format!("\n## {group} (graph: {graph})\n"));
        }
        let script = example["script"].as_str().unwrap_or("");
        // A GQL example (`lang: "gql"`, a plain GQL query with its `params`) as the call the
        // `query` tool runs, `g.gql("..", #{..})`.
        let script = match example["lang"].as_str() {
            Some("gql") => graphersal_session::queries::gql_call_text(script, &example["params"])
                .unwrap_or_else(|_| script.to_string()),
            _ => script.to_string(),
        };
        out.push_str(&format!(
            "\n{}:\n```\n{}\n```\n",
            example["title"].as_str().unwrap_or(""),
            script
        ));
    }
    out
}

/// The text of the resource `graphersal://gql-reference`.
pub fn gql_reference_text() -> String {
    format!("# GQL reference\n\n{GQL_REFERENCE}\n")
}

/// The GQL reference (the resource `graphersal://gql-reference`, which the `gql` tool's
/// description points to; [`crate::gql_reference_text`] for an agent without resources): the
/// supported subset, the write semantics and examples.
pub(crate) const GQL_REFERENCE: &str = "Run a GQL (ISO graph query language, Cypher-like) query against \
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
