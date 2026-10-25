# Writing Queries

This part describes the Gremlin DSL in detail, topic by topic. The DSL is the text form of a
query: what the command line, the playground, Python, saved queries and AI agents run, and what
`script::eval_value` evaluates in Rust. It is Gremlin embedded in [Rhai](https://rhai.rs).

## The shape of a script

A script is a sequence of Rhai statements. `g` is the graph, `__` starts an anonymous traversal,
and the token classes (`P`, `T`, `Order`, `Scope`, `Column`, `Pop`, ...) are predefined:

```rhai,graph=modern,script
let min_age = 30;
let names = g.v().has_label("person").has("age", P.gt(min_age)).values("name").to_list();
names.len()                                        // 2
```

- **Terminals run a traversal.** `to_list()`, `next()` and `iterate()` run it and return native
  values; a traversal that is the *last* value of a script is displayed instead. A traversal in
  the middle of a script without a terminal is only built, not run.
- **Two spellings for TinkerPop's steps.** Every TinkerPop step and function is available in
  snake_case (`has_label`, `out_e`) and in Gremlin camelCase (`hasLabel`, `outE`), with the
  same overloads. Graphersal's own extensions (`create_index`, `memory_report`,
  `remove_property`, `glob_path`, `define_query`, the `Fake` methods, ...) are snake_case only
  (since 0.3.0); a camelCase call of one fails with a help that names the snake_case name.
- **Strings in double quotes.** Single quotes are Rhai character literals; maps are written
  `#{key: value}`.
- **Anonymous traversals start with `__.`**: `where(__.out("knows"))`, `repeat(__.both())`.
- **Every traversal is atomic.** A failing traversal leaves nothing it wrote
  ([Transactions](../users_guide/transactions.md)).

An error names the failing step with its position in the plan, and most errors come with a help
text that suggests a fix built from the failing query.

## Topics

| Page | What it covers |
|---|---|
| [Displaying Results](../users_guide/displaying_results.md) | how results are shown, the row and item limits |
| [Predicates](../users_guide/predicates.md) | `P.*` and `TextP.*`, comparison and resolution rules |
| [Id Arguments](../users_guide/id_arguments.md) | what `v()`, `e()` and `has_id()` accept |
| [Type Conversion](../users_guide/type_conversion.md) | `as_string()`, `as_bool()`, `as_number()`, `length()` |
| [Math Expressions](../users_guide/math.md) | the `math()` step |
| [Map Keys and Values](../users_guide/map_keys_and_values.md), [Non-String Keys](../users_guide/value_keyed_maps.md) | maps in the stream |
| [Local Scope](../users_guide/local_scope.md) | steps applied inside a collection |
| [Path Keys (jpath)](../users_guide/jpath_keys.md) | reading and writing nested values |
| [UUID Values](../users_guide/uuid_values.md), [Multi-Label Vertices](../users_guide/multi_label_vertices.md), [Property Elements](../users_guide/property_elements.md) | values and elements beyond strings and numbers |

The following chapters cover [traversal patterns](traversal_patterns.md),
[changing the graph](changing_the_graph.md), schemas, saved queries and profiling; every single
step is in the [Step Reference](../traversal/step_reference.md).
