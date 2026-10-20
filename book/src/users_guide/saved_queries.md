# Saved Queries

A *saved query* is a named, parameterized piece of DSL stored IN the database, next to the
schema: written once, called by name from any front end (the CLI, Python, the playground).

```rhai
g.query("older_than", #{age: 30}).values("name").toList()
// ["josh", "peter"]
```

- The body is the text of ONE Rhai function; its parameters are named and typed.
- A call returns what the function returns: a **traversal** can be continued by the caller (like a
  SQL view), a **value** (a number, a list, a map) is a value (like a stored procedure).
- A query **reads**, unless its body writes data: then it is saved as a query that **writes**,
  and a call may write exactly what the engine infers from the body (its *effects*), as one
  transaction. Nobody declares effects, and a query never writes the schema, the saved queries
  or files.
- Queries live in **folders** (an attribute, not part of the name) and carry an optional
  **description**.

Every example on this page was run with `graphersal` on the default modern graph; the line after
`//` is its real output. Each block starts from a fresh graph.

## Defining a query

```rhai
g.define_query(#{
    name: "older_than",
    folder: "reports/people",
    description: "Persons older than the given age.",
    params: #{
        age: #{schema: #{type: "integer", minimum: 0, maximum: 150,
                         description: "Age in years"},
               default: 30}
    },
    body: `fn older_than(age) {
        g.v().has_label("person").has("age", P.gt(age)).order().by("age")
    }`
})
g.query("older_than").values("name").toList()
// ["josh", "peter"]
```

`g.define_query(#{..})` (`g.defineQuery(..)`) stores a new query or replaces the one of that
name. The keys:

| Key | Required | Meaning |
|---|---|---|
| `name` | yes | the query's name, also its Rhai function name: `[A-Za-z_][A-Za-z0-9_]*`, 1 to 128 bytes, not a Rhai keyword; case-sensitive, unique per database |
| `body` | yes | the text of ONE Rhai function named like the query |
| `params` | when the function has parameters | parameter name -> its declaration (see [Parameters](#parameters)) |
| `folder` | no | a `/`-separated path such as `"reports/sales"`; `""` or `()` is the root |
| `description` | no | free text (at most 4 KiB); defaults to the function's doc comment |
| `meta` | no | free text, stored and never interpreted |

The definition is checked before anything is stored, and refused with an error naming the
problem:

- the body compiles and is exactly one function named like the query, with nothing beside it;
- every parameter of the function is declared in `params`, and `params` declares nothing else;
- the catalog rules: name, folder (at most 8 segments, 255 bytes), sizes (source at most 64 KiB),
  parameter names (not a DSL global such as `g`, `P` or `T`), every parameter schema, and every
  default against its schema.

Saved queries called by the body are not checked at definition: a missing one fails when it is
called.

### Several queries at once

`g.define_queries(folder, source[, params])` (`g.defineQueries(..)`) stores every function of a
source as a query of its own, named like the function, described by its doc comment (`///`
lines or a `/** .. */` block). `params` maps each function name to its parameter map. All of
them are stored in one unit: either every query is stored or none. It returns the names.

```rhai
g.define_queries("reports/sales", `
    /// Persons, oldest first.
    fn oldest(n) { g.v().has_label("person").order().by("age", Order.desc).limit(n) }

    /// Software names.
    fn software() { g.v().has_label("software").values("name") }
`, #{oldest: #{n: #{schema: #{type: "integer", minimum: 1}, default: 2}}})
// ["oldest", "software"]
g.query("oldest").values("name").toList()
// ["peter", "josh"]
g.get_query("oldest")["body"]
// fn oldest(n) { g.v().has_label("person").order().by("age", Order.desc).limit(n) }
```

Each query stores the text of its own function only.

## Parameters

A parameter is declared as `#{schema: <schema node>, default: <value>}`, or as a bare type name
(the short form): `"integer"`, `"number"`, `"string"`, `"boolean"`, `"array"`, `"object"`,
`"null"` or `"uuid"`.

- The **schema** uses the node format of the [graph schema](schema_enforcement.md): `type`,
  `enum`, `minimum`, `maximum`, `pattern`, `minLength`, `items`, ..., and the annotations
  `title`, `description`, `examples`.
- The **default** sits beside the schema, not inside it: a `default` keyword inside the
  parameter's schema is an annotation (a data schema's `default` applies only when a vertex or
  edge is created), while a parameter default IS substituted when the argument is left out. A
  parameter without a default is required. A `defaultFunction` (`uuidv4`, `now`, ...) anywhere
  in a parameter's schema is an error: it fills a property of a created element and means
  nothing for an argument.

Arguments are **named**: `g.query("x", #{limit: 5})`, or `g.query("x")` when every parameter has
a default. An argument is a data value (a string, a number, a boolean, a UUID, a date or time, a
list, a map; never a traversal or a closure), bound to the function's parameter like a
prepared-statement parameter, never spliced into text. It is checked against its schema before
the body runs; the only conversion is the schema's write coercion (an `int64` where a `float64` is
declared, a canonical UUID string where a `uuid` is declared, the ISO 8601 text of a declared
[date or time format](../dates/schema.md) such as `date-time`). `()` is a value (null), not an omitted
argument.

```rhai
g.query("older_than", #{age: 200})
// Error: Invalid argument 'age' for saved query 'older_than': Constraint violation on 'older_than.age': value <= 150 (actual: 200)
g.query("older_than", #{years: 30})
// Error: Saved query 'older_than' has no parameter 'years'
```

## Calling a query

`g.query(name)` / `g.query(name, #{..})` calls the query at the **start** of a traversal, on `g`;
it works anywhere a script can use `g` (loops, functions, the body of another saved query). It is
not a step: `g.v().query(..)` and `__.query(..)` fail with an error that says so (calling a query
on incoming traversers is a later feature).

A traversal result is continued like any other traversal, and the optimizer sees the whole plan:
the caller's filters are pushed into the body's steps.

```rhai
g.define_query(#{name: "everything", body: "fn everything() { g.v() }"})
g.query("everything").has_label("person").count().profile()
// Step                                    Call      In     Out  ...
// v(labels: ["person"]).count()              1       0       4  ...
// Optimizer rules applied: source_filter_pushdown, count_pushdown
```

A value result is returned as it is:

```rhai
g.define_query(#{name: "stats", body: `fn stats() {
    #{people: g.v().has_label("person").count().next(), software: g.v().has_label("software").count().next()}
}`})
g.query("stats")
// #{"people": 4, "software": 2}
```

A call does not recompile an unchanged body: a compiled body is cached per thread, keyed by
the stored text, so a redefined query (or one restored by a rollback) always runs its current
body, and the plan is made anew on every call (new optimizations apply to old queries).

Saved queries may call each other up to **16 levels** deep; a deeper call (usually a query that
calls itself without an end) fails with `Saved query calls nested deeper than 16 levels`.

## Queries that read and queries that write

Whether a query may write is decided when it is SAVED, from its body: the engine walks the body's
Rhai syntax tree and infers its **effects**, the data writes its steps can make. A query with no
effects is saved in the dialect `graphersal-rhai/1` and only reads; a query with effects is saved
in `graphersal-rhai/2` and may write exactly those. Every save (`define_query`, `define_queries`,
a replacement, the playground's editor) sets the dialect again, also back to `/1` when the body
no longer writes; moving or describing a query does not.

```rhai
g.define_query(#{name: "add_person", params: #{name: "string", age: "integer"},
                 body: "fn add_person(name, age) { g.add_v(\"person\").property(\"name\", name).property(\"age\", age).next() }"})
g.get_query("add_person")["dialect"]
// graphersal-rhai/2
g.get_query("add_person")["effects"].map(|e| e.text)
// "Create Data(vertex \"person\")"
// "Update Data(vertex \"person\")"
g.query("add_person", #{name: "ann", age: 41})
// v[7]
```

An effect is a request as an [authorizer](permissions.md) sees it: an action (`Create`, `Update`,
`Delete`) on `Data`, with the element kind and label the chain states before the writing step.
The inference follows the same rules as the authorization check of a plan, so the run-time
check can hold every write against it:

| Body | Inferred |
|---|---|
| `add_v("person")`, `add_e("knows")` | `Create` on that label |
| `add_v(label)` (a variable) | `Create` on any vertex |
| `g.e().has_label("knows").drop()`, `out_e("knows").property(..)` | `Delete` / `Update` on `knows` edges |
| `g.v().has_label("person").property(..)` | `Update` on any vertex: a vertex has a label SET, so `has_label("person")` says nothing about its other labels |
| a chain from a variable or `__` (`t.drop()`, `side_effect(__.property(..))`) | `Delete` / `Update` on any data |
| `merge_v(..)` / `merge_e(..)` | `Create` and `Update` on the kind |
| `g.query("other")` | the effects of `other`, when it writes |
| `g.gql("MATCH (p:person) INSERT (p)-[:owns]->(:pet)")` | the writes of the lowered GQL query: `Create` on `pet` vertices and `owns` edges |
| `g.gql(text)` with a computed text (a variable, a concatenation) | `Create`, `Update` and `Delete` on any data: pass values as `$parameters` to keep a GQL read read-only |
| a method of the same name on something else (`m.drop()` on a map) | counted as a write |

The inference may claim more than a body writes, never less. What it cannot see, it refuses:

- a body that writes the **schema, the catalog or files** (`set_schema`, `define_query`,
  `create_index`, `export_graphml`, `import_graphson`, `mark`, ...) is refused when it is saved:
  a saved query writes data only;
- a body that **writes and hides what it calls** (`Fn(..)`, `call`, `curry`, `eval`, `import`, or
  `g.query(name)` with a computed name) is refused when it is saved. Without writes such a body is
  saved as a read-only query, and whatever it reaches stays read-only.

```rhai
g.define_query(#{name: "q", params: #{n: "string"}, body: "fn q(n) { g.query(n); g.add_v(\"x\").next() }"})
// Error: Invalid body of saved query 'q': it writes data (Create Data(vertex "x")) and uses g.query(name) with a computed name, which hides what it calls, so its effects cannot be inferred; ...
```

Effects are never stored. They are inferred again when a query is saved, listed or called, so a
newer version of Graphersal may infer more or less for the same text; a call always uses the
effects inferred now.

### A call of a query that writes

- **Invoker rights.** The whole effect set is asked of the caller's authorizer before the body
  runs (`Permission denied for 'query("name")' (Delete Data(edge "knows"))`), so a denial comes
  before the first write. The body then runs under the caller's rights restricted to the
  effects: a write the effects do not cover is denied (it would be a bug of the inference, and
  the message says so).
- **One unit.** The body runs as one transaction: a failing body rolls back everything it wrote,
  and the commit's [`ChangeSet`](transactions.md) carries the query's name and arguments as
  metadata (`saved_query`, `saved_query_args`). Inside a running unit (the playground runs a
  whole script as one) the call is a savepoint: a later failure of the script rolls it back
  too. A dry run (`eval_value_dry_run`) of a call previews its changes; nothing stays.
- **Nesting.** A query that writes may call queries that read and queries that write; their
  effects are part of its own. A read-only query that calls one that writes stays read-only:
  that call's writes are denied.
- **The result is read-only.** What the query returns is used after the call, outside its
  unit, so it can be continued only by reading; a returned traversal that writes runs INSIDE the
  call (next section).

```rhai
g.define_query(#{name: "unfriend", params: #{name: "string"},
                 body: "fn unfriend(name) { g.v().has(\"name\", name).out_e(\"knows\").drop().iterate(); g.v().has(\"name\", name).out(\"knows\").count().next() }"})
g.get_query("unfriend")["effects"].map(|e| e.text)
// "Delete Data(edge \"knows\")"
g.query("unfriend", #{name: "marko"})
// 0
```

### What a query that writes returns

A query that writes may end with the writing traversal itself, without a terminal. The call runs
that traversal inside its unit, as if it ended in `toList()`, and returns a **read-only
traversal over what it wrote** (`inject(results)` on the caller's graph). The caller continues it
like the result of a read-only query:

```rhai
g.define_query(#{name: "add_person", params: #{p_name: "string"},
                 body: "fn add_person(p_name) { g.add_v(\"person\").property(\"name\", p_name) }"})
g.query("add_person", #{p_name: "Bill"}).count().next()
// 1
```

Each call writes once, whatever the caller does with the result; the result only reads what
was written:

```rhai
g.query("add_person", #{p_name: "Bill"}).values("name").toList()
// ["Bill"]
g.query("add_person", #{p_name: "Bill"}).id().toList()
// ["7"]
g.v().has("name", "Bill").count().next()
// 1
```

Without a step after it, the result is shown like any traversal (in the playground and the CLI:
the written elements as a table). The written elements are the stored ones, so the
continuation walks the graph from them:

```rhai
g.define_query(#{name: "befriend", params: #{a: "string", b: "string"},
                 body: "fn befriend(a, b) { g.v().has(\"name\", a).add_e(\"knows\").to(__.v().has(\"name\", b)) }"})
g.query("befriend", #{a: "vadas", b: "peter"}).in_v().values("name").toList()
// ["peter"]
```

The continuation itself may not write: the call's effects end with the call.

```rhai
g.query("add_person", #{p_name: "Bill"}).property("age", 30).iterate()
// Error: ... Permission denied for 'property' (Update Data): a saved query is read-only: ...
```

Write the change into the query, or write in a traversal of your own after the call. The rules in
detail:

- **Only the returned traversal is run.** A traversal the body ends with a terminal
  (`next()`, `toList()`, `iterate()`) has already run; its value is returned as a value
  (`fn add(n) { g.add_v("person").property("name", n).next() }` returns the vertex `v[7]`, which
  is no traversal: `g.query("add", ..).count()` fails with `Function not found: count`). Return
  the traversal, or `g.v(written)` after `let written = ...toList();`, to let callers continue.
- **Also inside a list or a map**: `fn pair() { [g.add_v("a"), g.add_v("b")] }` writes both and
  returns two read-only traversals.
- **In the call's unit**: a returned traversal that fails rolls back everything the body wrote,
  and a dry run of the call previews its writes too.
- **The results are kept in memory** until the caller reads them, like a `toList()`: a query
  that writes millions of elements should end the writing traversal with `iterate()` and return
  a count or nothing instead.
- **An anonymous writing traversal** (`__.add_v("x")`) is no query of the graph: returning one
  fails the call (`returned a traversal with a writing step`).

### Read-only queries

A query of the dialect `graphersal-rhai/1` never writes. The call runs under a read-only
restriction on top of the host's authorizer: creating, changing or deleting data, the schema,
saved queries or files is denied with `PermissionDenied` (`a saved query is read-only`) before
anything runs, and nested queries inherit it. A traversal ANY query returns carries the
restriction too:

```rhai
g.define_query(#{name: "people", body: "fn people() { g.v().has_label(\"person\") }"})
g.query("people").property("seen", true).toList()
// Error: ... Permission denied for 'property' (Update Data(vertex)): a saved query is read-only: ...
```

Write in a traversal of your own after the call:

```rhai
let ids = g.query("people").id().toList();
g.v(ids).property("seen", true).toList();
g.v().has("seen").count().next()
// 4
```

- A graph the query builds itself (`GraphSource::empty()`, `__`) stays writable; it is not the
  database.
- Terminals inside the body (`next()`, `toList()`) are allowed: a value-returning query needs
  them.
- Nothing built from a query result is mutating (`is_mutating()` is false); whether a script that
  calls a query that writes changed the graph is what its run reports (`mutated`).
- **Stored before queries could write.** Opening a database never changes a dialect: a
  `graphersal-rhai/1` query whose body has writing steps (saved by 0.2.0, or through the Rust API
  or a catalog file, which keep the dialect as given) stays read-only and its writes are denied;
  `g.queries()` lists them as `denied_writes`. Saving the query again makes it one that writes.

### Unavailable queries and older versions

A query that cannot run is listed with `status: "error"` and the reason, and a call fails with
`Invalid body of saved query`: its body does not compile (after an upgrade of the language, or
stored through the Rust API), it is a `graphersal-rhai/2` query whose effects cannot be inferred,
or its dialect is one this version does not know. Such a query never blocks opening a database.

A database written by this version keeps every read-only query in `graphersal-rhai/1`, so
Graphersal 0.2.0 runs them; 0.2.0 lists a query that writes (`graphersal-rhai/2`) as not runnable
("its dialect is 'graphersal-rhai/2', this version runs 'graphersal-rhai/1'").

## Errors name the query

An error inside a body keeps its diagnostic and gains one line per call, innermost first:

```rhai
g.define_query(#{name: "broken", body: "fn broken() { g.v().values(\"name\").as_number(GType.LONG).toList() }"})
g.query("broken")
// Error: Step #2 'as_number(GType.LONG)' execution failed
//   at #2: v().values("name").as_number(GType.LONG)
// ...
// Caused by: Cast exception: expected type [int64], got type 'string'
//   value:  "marko"
//   origin: vertex id="1" label="person" property="name"
//   in saved query: "broken" (line 1, position 58 of its body)
// Help: ...
```

A missing query, a body that no longer compiles and a call at the wrong position fail with
errors naming the query and a `Help:` line with the fix.

## Managing queries

| Call | Result |
|---|---|
| `g.queries()` | every saved query as a list of maps, by name |
| `g.queries("reports")` | the queries in `reports` and the folders below it (`reports/sales`, not `reportsx`) |
| `g.get_query(name)` / `getQuery` | one query as a map, `()` when there is none |
| `g.drop_query(name)` / `dropQuery` | removes it; `false` when there was none |
| `g.move_query(name, folder)` / `moveQuery` | moves it (`""` or `()`: the root) |
| `g.describe_query(name, text)` / `describeQuery` | sets the description (`""` or `()` removes it) |

An entry of `queries()` has `name`, `folder` and `description` (`()` when absent), `params` (a
list in signature order: `#{name, schema, default?, required}`), `status`: `"ok"`, or `"error"`
with `error`, the reason the query cannot run (see
[Unavailable queries](#unavailable-queries-and-older-versions)), `writes` (a call may write),
`effects` (what it may write) and `denied_writes` (the writes of a read-only query's body that a
call denies), each effect `#{action, element, label, text}` (`element` / `label` `()` when open).
`get_query` adds `body`, `dialect` and `meta`.

```rhai
g.define_query(#{name: "older_than", folder: "reports/people", params: #{age: #{schema: #{type: "integer"}, default: 30}},
                 body: "fn older_than(age) { g.v().has(\"age\", P.gt(age)) }"})
g.queries("reports")
// [#{"denied_writes": [], "description": (), "effects": [], "folder": "reports/people", "name": "older_than", "params": [#{"default": 30, "name": "age", "required": false, "schema": #{"type": "integer"}}], "status": "ok", "writes": false}]
```

Moving a query or renaming a folder never breaks a caller: a call uses the name only. Dropping a
query that others call is allowed (dependencies are not tracked); the callers fail when they call
it.

Every change of the catalog (define, replace, move, describe, drop) is a database change like a
schema change: one unit of its own, or a savepoint inside a running unit (the playground runs a
script as one unit, so a script that fails leaves no definition behind), rolled back with it,
visible to commit hooks as a `SetDefinition` mutation (see [Transactions](transactions.md)).

## Permissions

Calling a query asks `Execute` on `Definition(query "name")` before the body runs; a query that
writes then asks its whole effect set; the body's own reads and writes are then asked as usual.
Listing asks `Read`, defining `Create` (`Update` to replace, move or describe), dropping
`Delete`. `AccessPolicy::read_only()` allows calling and listing, so under it a query that writes
is denied before it writes anything. See [Permissions](permissions.md).

## Limits

| What | Limit |
|---|---|
| name | 1 to 128 bytes, `[A-Za-z_][A-Za-z0-9_]*`, not a Rhai keyword |
| folder | at most 255 bytes and 8 segments |
| description | at most 4 KiB |
| body (source) | at most 64 KiB |
| parameters | at most 64 per query |
| definitions | at most 10 000 per database |
| call depth | 16 |

## Persistence

The catalog is part of the database: a [Store](../persistence/overview.md) keeps it in its
snapshots and journal, losslessly (every catalog change is a durable commit; recovery, rollback,
forks and backups carry it), and a packed snapshot (`.gsnap`) carries it too. A query is stored as
its source text, never as a compiled plan: every call plans anew. GraphML and GraphSON carry no
saved queries, like they carry no schema. The byte layout is in the
[format spec](../persistence/format.md) (sections 6.3, 9.3 and 16).

## Front ends

- **Playground** (WebAssembly and the dev server alike): the **Catalog ▾ ▸ Saved queries** manager
  lists the queries by folder with a badge each (**R/O** reads, **R/W** writes, its tooltip lists
  the effects; **R/O ⚠** read-only with writing steps that a call denies; **unavailable** with the
  reason), creates and edits them in an editor with a parameter list and the body in the code
  editor, deletes them, and runs one from a form built from its parameter schemas, which writes
  the `g.query(..)` call into a query tab. Running an **R/W** query shows a preview of what the
  call would change first (a dry run, rolled back) and asks. Saved queries travel in the catalog file with the other
  definitions (**Catalog ▾ ▸ Save catalog**, **Load catalog**), like the schema. See
  [Web Playground](web_playground.md#saved-queries).
- **MCP** (the [dev server](../persistence/dev_server.md#what-the-agent-gets) with `--mcp`): an AI
  agent lists them with `list_saved_queries`, runs one with `run_saved_query`, reads
  `graphersal://saved-queries`, and is told to look for a saved query before writing a new one
  and to run one with `writes: true` only when the user asked for that change;
  `--mcp-query-tools` also offers each query as a tool of its own (`readOnlyHint` /
  `destructiveHint` from its effects). With `--mcp-read-only` the queries that write are neither
  listed nor run. The **AI Chat** runs a query that writes only after the user confirmed it in the
  chat.
- **Python**: `graph.queries(folder)`, `graph.get_query(name)`, `graph.define_query(spec)`,
  `graph.drop_query(name)` and `graph.call_query(name, params)` (`graph.query` stays the alias of
  `execute`).
- **CLI**: the DSL calls (`graphersal -e 'g.queries()'`); `graphersal store info <dir>` prints how
  many definitions a Store's catalog holds.

In the playground, **Catalog ▾ ▸ Saved queries** lists the queries by folder:

![The saved queries manager: queries by folder, with Run, Edit and Delete](../images/playground/saved-queries-manager.png)

**Edit…** opens the editor: name, folder, description, every parameter with its type, default and
constraints, and the body in the code editor:

![The saved query editor: parameters with type, default and minimum, and the body](../images/playground/saved-queries-editor.png)

**Run…** builds a form from the parameter schemas; **Run** writes the `g.query(..)` call into a
query tab and runs it there:

![The run form of a saved query with one integer parameter](../images/playground/saved-queries-run.png)

## In Rust

The catalog is reachable without the DSL: `GraphTraversalSource::list_definitions`,
`get_definition`, `define_query(name, QueryDefinition)`, `remove_definition`, `move_query`,
`describe_query` (module `graphersal::catalog`). The Rust API stores a body without
compiling it (the core has no Rhai) and keeps the dialect as given (`QUERY_DIALECT` by default,
`WRITING_QUERY_DIALECT` for a query that writes); the DSL compiles it on definition and on every
call. A front end that builds definitions itself runs them through
`graphersal::script::with_inferred_dialect` (the body checked like `define_query` checks it, the
dialect set from the effects) and lists them with `graphersal::script::saved_query_effects`.

## Host-defined catalog kinds

Saved queries are one kind of catalog definition; [property indexes](property_indexes.md) and
compression rules are others. An application
built on the library (a server, say) can keep its own entities in the same catalog, so they are
stored, journaled, rolled back, forked and backed up with the graph: definition kinds 128 to 255
are reserved for hosts and never assigned by the library (`DefinitionKind::host(n)` refuses a
kind below 128; `DefinitionKind::HOST_FIRST`/`HOST_LAST`, `is_host()`). Kind 2 is the library's
property index.

```rust,ignore
use graphersal::catalog::{decode_payload, encode_payload};
use graphersal::catalog::{Definition, DefinitionFlags, DefinitionKind};

let kind = DefinitionKind::host(200).unwrap();
let bytes = encode_payload(&payload)?; // a property map, the codec of the library's own kinds
graph.set_definition(Definition::opaque(kind, DefinitionFlags::NONE, "person_name", bytes))?;
let back = decode_payload(graph.definition(kind, "person_name").unwrap().opaque_payload().unwrap())?;
```

The library keeps a host definition as opaque bytes: it checks only the name (1 to 1024 bytes)
and the payload size, writes the bytes back unchanged everywhere, and never reports them as
damage. `encode_payload`/`decode_payload` (feature `persist`) are optional, but they keep the
payload a property map like every library kind (decoding is safe on hostile input). The
*critical* flag means what it means for any kind the library does not decode: a store holding
a critical host definition opens read-only, and a Store refuses to write one. The byte rules are in the
[format spec](../persistence/format.md) (sections 6.3 and 16).
