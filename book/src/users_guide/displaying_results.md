# Displaying Results

A traversal's result is either **data** or **display**:

- **Data** is what `to_list()`, `next()`, a script variable, and the Rust data API return. It is
  never cut: `let xs = g.v().to_list(); xs.len()` counts every vertex. `iterate()` runs a
  traversal for its side effects (`g.add_v("x").property("a", 1).iterate()`) and returns nothing:
  its results are discarded, never materialized, so nothing is shown.
- **Display** is every rendering of results into text: a traversal left without a terminal (the
  session visualizer), a list or map that is a script's final value, and the explicit visualizers
  `to_table()`, `to_markdown()`, `to_json()`, `to_tree()`, `to_mermaid()`, `to_plantuml()`,
  `to_json_schema()` and `visualize(fmt)`.

Display can be bounded by two limits:

| Limit | What it bounds |
|---|---|
| `max_rows` | top-level results: table rows, traversers, list items |
| `max_items` | items of every nested collection inside a row, at any depth (a `fold()` list, the keys of one `group()` row) |

## Where the limits come from

- **The library is unbounded by default.** A Rust caller of `visualize_with` and a script engine
  built without render options get everything.
- **Front ends set a session default.** `graphersal` and the web playground show at most 100 rows and
  100 nested items, for every kind of display, including an explicit `to_table()`: the DSL cannot
  write a string to a file, so in a front end such a call is for viewing.
- **A single call overrides the session default** with an options map.

### Per-call options (DSL)

```rhai,graph=modern
g.v().to_table(#{max_rows: 20})             // other limits for this call
g.v().to_json(#{max_items: 5})
g.v().visualize(V.Markdown, #{no_limit: true})  // no limit at all for this call
g.with("render.max_rows", 10).v()           // session default for one query
```

`max_rows: 0` / `max_items: 0` is an error: `no_limit: true` is the one way to say "unlimited".
`no_limit: true` together with `max_rows` or `max_items` is an error too. `no_limit: false` keeps the
session default. Both spellings work: `max_rows`/`maxRows`, `max_items`/`maxItems`,
`no_limit`/`noLimit`.

### Session default (embedding the DSL)

A host passes its limits as scope options, and can change them later (a REPL `/set`):

```rust
use std::sync::Arc;
use graphersal::prelude::*;
use graphersal::auth::AllowAll;
use graphersal::exec::RenderOptions;
use graphersal::script::{graph_scope_with_options, render_scope_options, set_scope_render_options};

let limits = RenderOptions::new().with_max_rows(100).with_max_items(100);
let graph = Arc::new(GraphSource::tinkerpop_modern());
let mut scope =
    graph_scope_with_options(graph, &render_scope_options(&limits), Arc::new(AllowAll));
// ... later: remove the limits for the rest of the session.
set_scope_render_options(&mut scope, &RenderOptions::unlimited());
```

`render_value` / `try_render_value` take the host's `RenderOptions` for final lists and maps and
return a `RenderedOutput`.

### Rust API

```rust
use graphersal::prelude::*;
use graphersal::exec::RenderOptions;

let graph = TraversalGraph::tinkerpop_modern();
let rendered = graph
    .traversal()
    .v(None::<()>)
    .visualize_with(VisualizeFormat::Table, &RenderOptions::new().with_max_rows(2))
    .unwrap();
println!("{}", rendered.text);
if let Some(notice) = rendered.notice() {
    eprintln!("{notice}");
}
```

`visualize(fmt)`, `to_table()`, `to_json()`, ... are always unbounded and return a `String`;
`visualize_with`, `to_table_with` and `to_markdown_with` take `RenderOptions` and return a
`RenderedOutput`. `RenderOptions::unlimited()` / `.with_no_limit()` remove both limits.

## How a cut is shown

The truncation is **never part of the result text**. `RenderedOutput::truncation` reports it
(`rows_shown`, `more_rows`, `items_cut`, `max_items`; there is no total count, because counting would
cost a full execution), and `Truncation::notice()` builds the one notice text every front end
prints:

```text
… showing the first 100 rows; more exist. Use .limit(n), #{no_limit: true}, /set no-limit (--no-limit), or to_list() for the data.
… nested collections were cut to 100 items. Use #{max_items: n}, /set max-items <n> (--max-items <n>), or /set no-limit (--no-limit).
```

- Tables, markdown and trees mark a nested cut inside the cell with `…`, for example
  `[1, 2, 3, …]`, or with a `└─ …` line in a tree. A row with more keys than `max_items` shows
  `max_items` columns and a `…` column.
- **Maps with typed keys in JSON**: a map with a non-string key (`groupCount().by("age")`) is written in
  the GraphSON shape `{"@type": "g:Map", "@value": [29, 1, 27, 1]}`; a string-keyed map is a plain object.
  Tables and trees print such keys as text (`29`, `v[1]`); see [Maps With Non-String Keys](value_keyed_maps.md).
- **Non-finite floats in JSON**: JSON has no number for `NaN`, `Infinity` or `-Infinity`, so a
  result holding one (`math("0 / 0")`, `0.0 / 0.0`, a stored `NaN` property) writes it in GraphSON's
  typed form, `{"@type": "g:Double", "@value": "NaN"}` (payloads `"NaN"`, `"Infinity"`,
  `"-Infinity"`), at any depth, the same form a [GraphSON export](graphson.md) writes. The tag keeps
  it apart from the string `"NaN"`, which stays a plain string. This holds for `to_json()`,
  `--format json`, `execute().to_json()`, the playground's and the dev server's answers and the
  MCP `query` tool; tables and trees print the number itself (`NaN`, `inf`, `-inf`), and the
  playground shows `NaN`, `Infinity`, `-Infinity` in its Table view and inspector. The tag is
  written, not read: plain JSON read back (serde of a value, GraphML's embedded JSON) does not turn
  it into a float; GraphSON import does. The serde `Serialize` of a value itself stays strict and
  refuses a non-finite float, and GraphML export refuses one (with a help naming the fix).
- **JSON stays valid JSON**: an array of the first `max_rows` results, with nested collections cut
  and no marker inside, since a sentinel element would break the data's types. The only signal is
  the notice. The JSON-per-line fallback formats (`jsonschema`, `mermaid`, `plantuml` on
  traversal results) follow the same rules.
- A schema (`g.get_schema()`, `g.infer_schema()`) and a `profile()` result are never bounded.

## Graph values

A graph as a script's final value (`g.e(0).to_graph()`, `g.e().subgraph("sg").cap("sg").next()`,
`GraphSource::empty()`, `g` itself) is shown as one result `{"vertices": [..], "edges": [..]}`,
each element materialized like a `g.v()`/`g.e()` result: exactly what `cap("sg")` shows for a graph
inside a traversal. A table prints the elements in their short form (`[v[1], v[2]]`), JSON with
all their properties:

```rhai
graphersal> g.e(0).to_graph()
╭────────────────────┬──────────────╮
│ edges              │ vertices     │
├────────────────────┼──────────────┤
│ [e[0][1-knows->2]] │ [v[1], v[2]] │
╰────────────────────┴──────────────╯
```

`max_items` bounds both lists, and only `max_items + 1` vertices and edges are read, so even `g` on
a large graph shows its first elements and the cut notice instead of materializing everything.
This is display only: `next()` and `to_graph()` still return the graph itself, a source you
traverse (`sub.v().count().next()`). The web playground shows the same result (and draws it in
the Graph panel).

## A single list-valued result

A traversal that yields ONE list-valued traverser (`g.v().values("age").fold()`, `cap("a")` of an
`aggregate("a")`) is a list of **one** element in every terminal: `to_list().len()` is `1` and
`to_json()` is `[[29, 27, 32, 35]]`. Nothing is unwrapped. The only difference you may see is in the
`graphersal` printer: it prints a returned script array one element per line, so the list of one list prints as
`[29, 27, 32, 35]` (the single element), while the flat list `[29, 27, 32, 35]` prints as four lines. Use
`to_json()` or `.len()` to tell them apart.

## Tokens and anonymous traversals

A DSL token shows as the DSL text that builds it, wherever a script prints it: as the final value
(also inside a list or map), through `to_string()` and in string interpolation `${..}`. The text
is the canonical dotted spelling, and evaluating it gives the same token back:

| Script | Shows |
|---|---|
| `Scope::local` | `Scope.local` |
| `Order::Desc` | `Order.desc` |
| `keys`, `first` | `Column.keys`, `Pop.first` |
| `T.id`, `By.Count` | `T.id`, `By.Count` |
| `GType.INT` | `GType.LONG` (the same type) |
| `Operator.sum`, `Barrier.normSack` | `Operator.sum`, `Barrier.normSack` |
| `P.gt(1).and(P.lt(3))` | `P.gt(1).and(P.lt(3))` |
| `TextP.containing("a")` | `TextP.containing("a")` |
| `Cardinality.single(5)`, `CastPolicy.Default("x")` | the same text |
| `__.out().has("name", "x")` | `__.out().has("name", "x")` |
| `Scope`, `P` (a class itself) | `Scope`, `P` |

An anonymous traversal (`__...`) is a template for a child position, not a query of your graph, so
a script that returns one shows its text instead of running it; the steps read like `.profile()`
names them. The session's step-name spelling (`--spelling camel`, `render.spelling`) applies to a
displayed result (`__.outE()`, `TextP.startingWith("a")`); `to_string()` and `${..}` always give
the canonical snake_case text. A UUID is a value, not a token: it shows its canonical text.

## Early stop

With `max_rows = n`, the visualizer appends an internal `limit(n + 1)` to the pipeline before it
is optimized and executed, so only `n + 1` results are produced and materialized. The extra one
only tells whether more exist. Barriers before it (`group()`, `order()`, ...) still see the whole
stream. In the executed plan it is the step `limit(n + 1) [display]`.

An **error location never shows it**: the `at #N:` line with its caret, the `plan:` line,
[`step_location()`](profiling_and_execute.md) and the query a help text rewrites show the
traversal as you wrote it, so `g.inject(1).math("value + 1")` fails with

```text
Error: Step #1 'math("value + 1")' execution failed
  at #1: inject(1).math("value + 1")
                   ^^^^^^^^^^^^^^^^^
```

and not with a `.limit(101) [display]` you never wrote. The display step is appended last, so
leaving it out changes no step number. It is shown only when the failure is that step itself.

No limit is appended, and the traversal runs completely with only the output cut, when the
pipeline mutates the graph (so `g.v().property("x", 1)` still updates every vertex) or already ends
on a single-result step such as `count()`, `fold()` or `group()`.

## `graphersal`

- Defaults: 100 rows, 100 nested items, the same in the REPL, `-e` and `--in`.
- Flags: `--max-rows <N>`, `--max-items <N>` (N ≥ 1), and `--no-limit`. `--no-limit` together with
  either of the others is an invalid invocation (exit code 2).
- REPL: `/set max-rows <N>`, `/set max-items <N>`, `/set no-limit`. Setting a limit after
  `/set no-limit` turns limits back on.
- In `-e` / `--in` the notice goes to **stderr**, so stdout carries only the result (and
  `--format json` stays parseable) and the exit code stays `0`. The REPL prints it after the output.

```bash
graphersal --graph large -e 'g.v()'                         # 100 rows, notice on stderr
graphersal --graph large --no-limit -e 'g.v()' > all.txt    # everything
graphersal --graph large --format json -e 'g.v()' | jq length   # 100
```

## Web playground

The [web playground](web_playground.md) uses the same defaults (changeable in its settings). It
runs a traversal without a terminal as `execute()` and keeps all results as data; its Table, JSON and
Raw views show them within the limits (Raw is the text the CLI prints), and a cut result shows the
same notice above them. `g.with("render.max_rows", n)` and `to_table(#{max_rows: n})` override the
limits for one query there too.
