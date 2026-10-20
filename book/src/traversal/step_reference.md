# Step Reference

Every step of the DSL has a page of its own, listed in alphabetical order under this chapter in
the sidebar and by category [below](#steps-by-category). This page is the short guide to what
every query shares: where it starts, how it runs, how much it returns and how it is shown.

## `g` and `__`

`g` is the graph you query; a query starts with a start step on it, such as `v()` (vertices),
`e()` (edges) or `inject(..)` (values). `__` starts an anonymous traversal: the child of a step
such as `where`, `repeat`, `union` or `by`, which runs once for every element that reaches it.

```rhai,graph=modern
g.v().has_label("person").where(__.out("created")).values("name")   // marko, josh, peter
```

Every step has a snake_case name and its Gremlin camelCase twin (`has_label` and `hasLabel`,
`out_e` and `outE`); they are the same function. Strings are written in double quotes, maps as
`#{key: value}`, lists as `[1, 2]`. Tokens are written with their class: `P.gt(30)`,
`Order.desc`, `T.label`, `Scope.local`, `Column.keys` (see [Predicates](functions/predicates.md) and
[Functions, Tokens and Values](functions/tokens_and_values.md)).

## Running a query

A query runs when it reaches a **terminal**. A query without one, at the end of a script, is run
and shown (a table in the command line and the playground).

| Terminal | Returns |
|---|---|
| [`to_list()`](steps/to_list.md) | all results, as a list |
| [`next()`](steps/next.md) | the first result |
| [`iterate()`](steps/iterate.md) | nothing: runs the query for what it writes |
| [`to_graph()`](steps/to_graph.md) | the edges reached and their vertices, as a graph |
| [`profile()`](steps/profile.md) | the optimized plan with timings and counts |
| [`execute()`](steps/execute.md) | results, error and profile together, never throws |

```rhai,graph=modern
g.v().values("name").next()                                   // marko
g.v().has("name", "marko").property("age", 30).iterate()      // a write, nothing returned
```

Every query is one unit: when a step fails, nothing the query wrote stays
([Transactions](../users_guide/transactions.md)).

## How many results

The data a query returns is never cut. To ask for less, use the steps that select a part:
[`limit(n)`](steps/limit.md), [`range(low, high)`](steps/range.md), [`tail(n)`](steps/tail.md),
[`skip(n)`](steps/skip.md), [`dedup()`](steps/dedup.md), [`sample(n)`](steps/sample.md).

```rhai,graph=modern
g.v().has_label("person").values("name").limit(2)         // marko, vadas
g.v().has_label("person").values("name").range(1, 3)      // vadas, josh
```

What is **shown** is bounded: a displayed result has at most 100 rows and 100 items per nested
list or map, with a notice when more exist. One query changes it with an argument of its
visualizer or a `g.with()` option; the command line has `--max-rows`, `--max-items` and
`--no-limit` ([Displaying Results](../users_guide/displaying_results.md)).

```rhai,graph=modern
g.v().to_table(#{max_rows: 2})                     // two rows and a notice that more exist
g.with("render.max_rows", 2).v().values("name")    // the same for one query
```

## Showing results

| Visualizer | Shows |
|---|---|
| [`to_table()`](steps/to_table.md) | a table (the default) |
| [`to_markdown()`](steps/to_markdown.md) | a Markdown table |
| [`to_json()`](steps/to_json.md) | JSON |
| [`to_tree()`](steps/to_tree.md) | a tree of nested values |
| [`to_mermaid()`](steps/to_mermaid.md), [`to_plantuml()`](steps/to_plantuml.md), [`to_json_schema()`](steps/to_json_schema.md) | a diagram or a JSON Schema of a schema, for example of `infer_schema()` |
| [`visualize(V.Json)`](steps/visualize.md) | any of them by token: `V.Table`, `V.Markdown`, `V.Json`, `V.Tree`, `V.Mermaid`, ... |

```rhai,graph=modern
g.v().has_label("person").value_map("name", "age").to_markdown()
g.v().infer_schema().to_mermaid()
```

## Options of one query

`g.with(key, value)` sets an option for the query that follows it: a time limit, the loop limit,
the display limits, the spelling of the profile, which optimizer rules run. Every key is in the
[Execution Options Reference](../users_guide/execution_options_reference.md); a host can lock them
([Running Queries Safely](../library/safety.md)).

```rhai,graph=modern
g.with("evaluationTimeout", 1000).v().count()        // stops after one second
g.with("repeat.max_loops", 10).v("1").repeat(__.out()).until(__.has("name", "ripple")).values("name")
```

## Help inside the tools

The text of every step page is also where you write queries:

- in the REPL: `/help <name>` (`/help has_label`, `/help hasLabel`), `/help steps`, `/help tokens`;
- in the [web playground](../users_guide/web_playground.md): the completion of the query editor;
- over MCP: the `graphersal://dsl-reference` resource of the [dev server](../persistence/dev_server.md).

Steps follow the Apache TinkerPop
[reference documentation](https://tinkerpop.apache.org/docs/3.8.2/reference/#graph-traversal-steps)
unless a page says otherwise; the differences are collected in
[TinkerPop Deviations](../for_developers/tinkerpop_deviations.md).

## Steps by category

<!-- steps:begin -->
### Start

[`v`](steps/v.md) · [`e`](steps/e.md) · [`inject`](steps/inject.md) · [`gql`](steps/gql.md)

### Walking the graph

[`out`](steps/out.md) · [`in`](steps/in.md) · [`both`](steps/both.md) · [`out_e`](steps/out_e.md) · [`in_e`](steps/in_e.md) · [`both_e`](steps/both_e.md) · [`out_v`](steps/out_v.md) · [`in_v`](steps/in_v.md) · [`both_v`](steps/both_v.md) · [`other_v`](steps/other_v.md) · [`to_e`](steps/to_e.md) · [`to_v`](steps/to_v.md) · [`glob_path`](steps/glob_path.md)

### Filters

[`has`](steps/has.md) · [`has_id`](steps/has_id.md) · [`has_key`](steps/has_key.md) · [`has_label`](steps/has_label.md) · [`has_not`](steps/has_not.md) · [`has_p`](steps/has_p.md) · [`has_value`](steps/has_value.md) · [`has_value_any`](steps/has_value_any.md) · [`has_value_p`](steps/has_value_p.md) · [`is`](steps/is.md) · [`where`](steps/where.md) · [`where_p`](steps/where_p.md) · [`where_t`](steps/where_t.md) · [`filter`](steps/filter.md) · [`and`](steps/and.md) · [`or`](steps/or.md) · [`not`](steps/not.md) · [`dedup`](steps/dedup.md) · [`limit`](steps/limit.md) · [`range`](steps/range.md) · [`skip`](steps/skip.md) · [`tail`](steps/tail.md) · [`coin`](steps/coin.md) · [`sample`](steps/sample.md) · [`simple_path`](steps/simple_path.md) · [`cyclic_path`](steps/cyclic_path.md) · [`none`](steps/none.md) · [`discard`](steps/discard.md) · [`all`](steps/all.md) · [`any`](steps/any.md)

### Values and properties

[`values`](steps/values.md) · [`value_map`](steps/value_map.md) · [`element_map`](steps/element_map.md) · [`properties`](steps/properties.md) · [`id`](steps/id.md) · [`label`](steps/label.md) · [`labels`](steps/labels.md) · [`key`](steps/key.md) · [`value`](steps/value.md) · [`constant`](steps/constant.md) · [`element`](steps/element.md) · [`json_path`](steps/json_path.md) · [`index`](steps/index.md) · [`identity`](steps/identity.md)

### Strings and type conversion

[`to_lower`](steps/to_lower.md) · [`to_upper`](steps/to_upper.md) · [`trim`](steps/trim.md) · [`l_trim`](steps/l_trim.md) · [`r_trim`](steps/r_trim.md) · [`substring`](steps/substring.md) · [`replace`](steps/replace.md) · [`concat`](steps/concat.md) · [`format`](steps/format.md) · [`split`](steps/split.md) · [`length`](steps/length.md) · [`as_string`](steps/as_string.md) · [`as_bool`](steps/as_bool.md) · [`as_number`](steps/as_number.md) · [`cast`](steps/cast.md)

### Dates and times

[`as_date`](steps/as_date.md) · [`as_duration`](steps/as_duration.md) · [`date_add`](steps/date_add.md) · [`date_diff`](steps/date_diff.md) · [`age`](steps/age.md) · [`date_trunc`](steps/date_trunc.md) · [`date_part`](steps/date_part.md) · [`at_zone`](steps/at_zone.md) · [`as_local`](steps/as_local.md)

### Vectors

[`as_vector`](steps/as_vector.md) · [`quantize`](steps/quantize.md) · [`similarity`](steps/similarity.md) · [`nearest`](steps/nearest.md) · [`vector_mean`](steps/vector_mean.md) · [`embed`](steps/embed.md) · [`embedders`](steps/embedders.md) · [`embed_property`](steps/embed_property.md)

### Lists and ordering

[`fold`](steps/fold.md) · [`unfold`](steps/unfold.md) · [`combine`](steps/combine.md) · [`conjoin`](steps/conjoin.md) · [`difference`](steps/difference.md) · [`disjunct`](steps/disjunct.md) · [`intersect`](steps/intersect.md) · [`merge`](steps/merge.md) · [`product`](steps/product.md) · [`reverse`](steps/reverse.md) · [`order`](steps/order.md)

### Aggregation

[`count`](steps/count.md) · [`sum`](steps/sum.md) · [`min`](steps/min.md) · [`max`](steps/max.md) · [`mean`](steps/mean.md) · [`median`](steps/median.md) · [`group`](steps/group.md) · [`group_count`](steps/group_count.md) · [`tree`](steps/tree.md) · [`aggregate`](steps/aggregate.md) · [`store`](steps/store.md) · [`cap`](steps/cap.md) · [`barrier`](steps/barrier.md)

### Labels and paths

[`as`](steps/as.md) · [`select`](steps/select.md) · [`path`](steps/path.md) · [`project`](steps/project.md) · [`math`](steps/math.md)

### Branches and loops

[`union`](steps/union.md) · [`choose`](steps/choose.md) · [`branch`](steps/branch.md) · [`option`](steps/option.md) · [`coalesce`](steps/coalesce.md) · [`optional`](steps/optional.md) · [`repeat`](steps/repeat.md) · [`times`](steps/times.md) · [`until`](steps/until.md) · [`emit`](steps/emit.md) · [`loops`](steps/loops.md) · [`local`](steps/local.md) · [`map`](steps/map.md) · [`flat_map`](steps/flat_map.md)

### Modulators and options

[`by`](steps/by.md) · [`from`](steps/from.md) · [`to`](steps/to.md) · [`with`](steps/with.md) · [`with_path`](steps/with_path.md) · [`with_bulk`](steps/with_bulk.md)

### Sack and side effects

[`sack`](steps/sack.md) · [`with_sack`](steps/with_sack.md) · [`with_side_effect`](steps/with_side_effect.md) · [`side_effect`](steps/side_effect.md) · [`subgraph`](steps/subgraph.md)

### Changing the graph

[`add_v`](steps/add_v.md) · [`add_e`](steps/add_e.md) · [`property`](steps/property.md) · [`property_json`](steps/property_json.md) · [`drop`](steps/drop.md) · [`remove_property`](steps/remove_property.md) · [`add_label`](steps/add_label.md) · [`drop_label`](steps/drop_label.md) · [`set_label`](steps/set_label.md) · [`merge_v`](steps/merge_v.md) · [`merge_e`](steps/merge_e.md) · [`fail`](steps/fail.md)

### Running and showing results

[`to_list`](steps/to_list.md) · [`next`](steps/next.md) · [`iterate`](steps/iterate.md) · [`execute`](steps/execute.md) · [`profile`](steps/profile.md) · [`profile_with`](steps/profile_with.md) · [`to_graph`](steps/to_graph.md) · [`to_table`](steps/to_table.md) · [`to_json`](steps/to_json.md) · [`to_markdown`](steps/to_markdown.md) · [`to_tree`](steps/to_tree.md) · [`to_mermaid`](steps/to_mermaid.md) · [`to_plantuml`](steps/to_plantuml.md) · [`to_json_schema`](steps/to_json_schema.md) · [`visualize`](steps/visualize.md) · [`set_visualizer`](steps/set_visualizer.md)

### Schema

[`get_schema`](steps/get_schema.md) · [`set_schema`](steps/set_schema.md) · [`patch_schema`](steps/patch_schema.md) · [`validate_schema`](steps/validate_schema.md) · [`validate_schema_patch`](steps/validate_schema_patch.md) · [`infer_schema`](steps/infer_schema.md) · [`diff_schema`](steps/diff_schema.md)

### Saved queries

[`define_query`](steps/define_query.md) · [`define_queries`](steps/define_queries.md) · [`query`](steps/query.md) · [`queries`](steps/queries.md) · [`get_query`](steps/get_query.md) · [`drop_query`](steps/drop_query.md) · [`move_query`](steps/move_query.md) · [`describe_query`](steps/describe_query.md)

### Property indexes

[`create_index`](steps/create_index.md) · [`drop_index`](steps/drop_index.md) · [`indexes`](steps/indexes.md)

### Compressed properties

[`define_compression`](steps/define_compression.md) · [`drop_compression`](steps/drop_compression.md) · [`compressions`](steps/compressions.md) · [`recompress`](steps/recompress.md) · [`estimate_compression`](steps/estimate_compression.md)

### Graph information and files

[`statistics`](steps/statistics.md) · [`memory_usage`](steps/memory_usage.md) · [`mark`](steps/mark.md) · [`import_graphml`](steps/import_graphml.md) · [`import_graphson`](steps/import_graphson.md) · [`export_graphml`](steps/export_graphml.md) · [`export_graphson`](steps/export_graphson.md) · [`export_snapshot`](steps/export_snapshot.md)

### Predicates, tokens and test data

[Predicates](functions/predicates.md) · [Functions, Tokens and Values](functions/tokens_and_values.md) · [Test Data Functions](functions/test_data.md)

<!-- steps:end -->
