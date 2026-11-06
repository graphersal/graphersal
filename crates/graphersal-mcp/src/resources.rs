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

pub(crate) const RESOURCES: [Resource; 5] = [
    Resource {
        uri: "graphersal://schema",
        name: "schema",
        title: "Stored schema",
        description: "The schema stored in the graph (graphersal-schema-v2 JSON), or null.",
        mime: "application/json",
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
        "graphersal://saved-queries" => Ok(compact(&Value::Array(saved_queries(
            context, None, None, read_only,
        )?))),
        "graphersal://statistics" => {
            Ok(compact(&context.host.add_process_statistics(
                context.call(|s| s.statistics())??,
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
