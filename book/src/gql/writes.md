# Writing with GQL

`gql()` writes too. This version supports **`INSERT`** (Cypher `CREATE` is its
[alias](aliases.md)): it creates nodes and edges, once for each row that reaches it. `SET`,
`REMOVE`, `DELETE` and `MERGE` come in later versions; until then they fail before the run with
GQLSTATUS `0A000` and a help line naming the Gremlin step that does the same
([Not yet](#not-yet)).

A writing query is one unit, like every traversal: it commits when it succeeds, and a query that
fails, on any row, changes nothing ([Units](#one-query-is-one-unit)).

## INSERT

`INSERT` takes one or more comma-separated patterns. A node pattern creates a node with the
labels written (none, one, or several joined by `&`; Cypher's `:A:B` is the alias) and the
properties of its map. An edge pattern creates an edge between the nodes beside it: it needs a
direction (`-[:T]->` or `<-[:T]-`) and exactly one label.

```rhai,graph=modern,script
g.gql("INSERT (:city {name: 'Berlin', population: 3.7})").iterate();
g.gql("INSERT (a:person {name: 'ann'})-[:knows {since: 2020}]->(b:person&admin {name: 'bob'})").iterate();
g.gql("MATCH (a {name: 'ann'})-[e:knows]->(b) RETURN e.since AS since, labels(b) AS labels").to_list()   // #{"labels": ["person", "admin"], "since": 2020}
```

The same in Gremlin:

```rhai,graph=modern
g.add_v("person").property("name", "ann").as("a").add_v(["person", "admin"]).property("name", "bob").add_e("knows").from("a").property("since", 2020).count().next()   // 1
```

A query may end with the `INSERT`: it then returns **no rows**. GQL's `FINISH` ends a query
without a result too. `RETURN` after an `INSERT` reads the new elements, and `RETURN *` includes
the variables the `INSERT` named:

```rhai,graph=modern
g.gql("INSERT (c:city {name: 'Berlin'}) RETURN c.name AS name").to_list()   // #{"name": "Berlin"}
g.gql("CREATE (c:city:capital {name: 'Rome'}) RETURN labels(c) AS l").to_list()   // #{"l": ["city", "capital"]}
g.gql("INSERT (:city {name: 'Paris'})").to_list()   // no rows
g.gql("INSERT (:city {name: 'Paris'}) FINISH").to_list()   // no rows
```

**Property values** are expressions: literals, `$parameters`, functions and the variables of the
row. A property whose value is `NULL` is **not stored** (GQL has no stored `NULL`; a Gremlin
`property(k, ())` stores one). Lists and maps are stored as they are, also lists of mixed kinds
(a [difference from Neo4j](semantics.md#writes)); a node, an edge or a path cannot be a property
value (`22G03`).

```rhai,graph=modern
g.gql("INSERT (n:x {k: NULL, tags: ['a', 1], address: {city: 'Rome'}}) RETURN keys(n) AS keys, n.address.city AS city").to_list()   // #{"city": "Rome", "keys": ["tags", "address"]}
```

**Pattern rules.** A variable bound before the `INSERT` is written alone, `(p)`: it connects the
existing node and may not carry labels or properties (`42002`; change an existing node with the
Gremlin steps `add_label()` and `property()` for now). An edge without a direction, an edge with
a label expression (`:r|s`), a node label expression other than a conjunction, a path variable
(`p = ...`) and `WHERE` or a quantifier inside the pattern are errors before the run; the help
shows the accepted form.

```rhai,graph=modern
g.gql("INSERT (a:x)-[:r]-(b:y)")   // error: INSERT needs a direction
g.gql("MATCH (p:person) INSERT (p:admin)")   // error: the variable `p` is already bound
```

## Once per row

An `INSERT` runs once for each row that reaches it, so the clauses before it decide how many
elements it creates. A `MATCH` gives one row per match, `FOR` (Cypher `UNWIND`) one per list
item, and a variable of the row connects the new elements to what it matched:

```rhai,graph=modern,script
g.gql("MATCH (p:person {name: 'marko'}) INSERT (p)-[:likes]->(f:fruit {name: 'apple'}) RETURN f.name AS fruit").to_list();   // #{"fruit": "apple"}
g.gql("FOR n IN $names INSERT (:person {name: n, born: $year})", #{names: ["ann", "bob"], year: 1990}).iterate();
g.gql("MATCH (p:person) WHERE p.born = 1990 RETURN p.name AS name ORDER BY name").to_list()   // #{"name": "ann"}, #{"name": "bob"}
```

It may stand after `MATCH`, `OPTIONAL MATCH`, `FOR`, `WITH`, `LET` and `FILTER`, and further
clauses may follow it: a `MATCH` after the `INSERT` sees the new elements, and a new node can be
connected to every row a later `MATCH` finds:

```rhai,graph=modern
g.gql("INSERT (c:city {name: 'Berlin'}) MATCH (p:person) INSERT (p)-[:lives_in]->(c) RETURN count(*) AS n").to_list()   // #{"n": 4}
g.gql("MATCH (p:person) WITH p, p.age + 1 AS later INSERT (p)-[:turns]->(:age {years: later}) RETURN p.name AS name, later ORDER BY name").to_list()
// #{"later": 33, "name": "josh"}, #{"later": 30, "name": "marko"}, #{"later": 36, "name": "peter"}, #{"later": 28, "name": "vadas"}
```

A node an `OPTIONAL MATCH` left `NULL` cannot be an edge's endpoint: the query fails (`22004`)
and changes nothing. Keep only the bound rows with `FILTER x IS NOT NULL` first:

```rhai,graph=modern
g.gql("OPTIONAL MATCH (p:person {name: 'nobody'}) INSERT (p)-[:knows]->(:person)")   // error: the source node `p` of the inserted `:knows` edge is NULL
g.gql("MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) FILTER s IS NOT NULL INSERT (p)-[:made]->(s) RETURN count(*) AS n").to_list()   // #{"n": 4}
```

**Inside `CALL`.** A `CALL` body may end with its write instead of `RETURN` (a unit subquery): it
runs once per incoming row and passes the row on unchanged, also when the body found nothing:

```rhai,graph=modern
g.gql("MATCH (p:person) CALL (p) { INSERT (p)-[:likes]->(:fruit {name: p.name}) } RETURN count(*) AS c").to_list()   // #{"c": 4}
```

A subquery expression (`EXISTS { }`, `COUNT { }`, `COLLECT { }`) and a comprehension cannot
write (`42001`), and an `INSERT` in an operand of `UNION` is not supported yet.

## What a row sees

Each clause sees every write of the clauses before it, for all rows; within one clause the rows
are processed in order, and each row sees the writes of the rows before it. A query never sees
its own writes while it is still finding the rows of an earlier clause: `MATCH (n) INSERT (:X)`
creates exactly one node per node that existed before, and `MATCH (a)-[e]->(b) INSERT
(a)-[:copy]->(b)` copies each existing edge once, never the copies.

```rhai,graph=modern,script
g.gql("MATCH (n) INSERT (:X)").iterate();
g.v().has_label("X").count().next()   // 6
```

```rhai,graph=modern
g.gql("FOR i IN range(1, 3) INSERT (:n {i: i}) WITH count(*) AS made MATCH (m:n) RETURN made, count(m) AS seen").to_list()   // #{"made": 3, "seen": 3}
```

The details, and how this follows from the executor, are on
[Semantics](semantics.md#writes).

## One query is one unit

A `gql()` query is one traversal and so one unit ([Transactions](../users_guide/transactions.md)):
when any row fails (a data error on the last row, a schema violation, a permission, a resource
limit, cancellation), nothing the query wrote stays. Here the second item divides by zero, and
the node of the first item is rolled back with it (`execute()` keeps the error instead of
raising it, so the script goes on):

```rhai,graph=modern,script
let run = g.gql("FOR x IN [1, 0] INSERT (:n {v: 1 / x})").execute();   // fails: division by zero
g.v().has_label("n").count().next()   // 0
```

Commit hooks see the query's `ChangeSet` and may veto it, a dry run shows its changes without
keeping them, and a Store's journal records it like any Gremlin write
([Change capture](../users_guide/transactions.md#change-capture-changeset)).

## Schema

An `INSERT` creates each element in one storage call with all its properties, so a schema sees
the whole element ([Schemas](../users_guide/schema_enforcement.md)): defaults on create are
filled in, a `required` property is satisfied by the map, write-time coercion applies, and a
closed schema refuses undeclared labels and properties. A violation fails the query (and
changes nothing), with a help line written for GQL:

```rhai,graph=empty,script
g.set_schema(#{mode: "open", vertices: #{person: #{schema: #{type: "object", properties: #{
    name: #{type: "string"}, age: #{type: "integer"}, active: #{type: "boolean", default: true}},
    required: ["name"]}}}});
g.gql("INSERT (:person {name: 'alice'})").iterate();
g.gql("MATCH (p:person) RETURN p.name AS name, p.active AS active").to_list()   // #{"active": true, "name": "alice"}
```

```text
Caused by: Missing required property 'name' on vertex 'person'
Help: Give 'name' a value in the property map of the INSERT, as in g.gql("INSERT (:person {name: 'value'})"); a NULL value (such as a property the matched element lacks) is not stored and so is missing too: give it a fallback, as in {name: coalesce(n.name, 'unknown')}. Or remove it from the label's "required" in the schema, or give it a "default" (or "defaultFunction") in the schema: it is stored when an element is created without the property.
```

A value of the wrong type is not converted, and GQL never infers a type from text. Convert it where
the `INSERT` writes it:

```text
Caused by: Type mismatch on vertex 'person.age': expected int64, got string
Help: Convert the value to the type 'int64' where the INSERT writes it, with toInteger(), as in g.gql("INSERT (:person {age: toInteger($v)})", #{v: "29"}). toInteger() gives NULL for a value it cannot convert, and a NULL value is not stored; CAST(x AS ..) fails the query for it instead.
```

## Permissions

The [permission](../users_guide/permissions.md) check sees the lowered plan before anything
runs: an `INSERT` asks `Create` on `Data(vertex "label")` per label of each new node and `Create`
on `Data(edge "label")` per new edge, exactly as `add_v(labels)` and `add_e(label)` do, plus
the reads of its `MATCH` clauses. It asks nothing for the properties: they are part of creating
the element. (Gremlin's `add_v(..).property(..)` asks `Create` and `Update`, because the check
runs before the optimizer folds the property into the creation; a policy that denies only
`Update` allows the `INSERT` and refuses its Gremlin twin.) A read-only host
(`AccessPolicy::read_only()`, the MCP server's `--mcp-read-only`, a read-only view of a past
state) refuses a GQL write before it runs.

## Saved queries and the DSL

A saved query may be written in GQL itself ([Saved queries in
GQL](../users_guide/saved_queries.md#saved-queries-in-gql)): the body is the plain query text,
its `$parameters` are the declared ones, and an `INSERT` makes it a query that writes, whose
`effects` name the labels its plan creates. A call writes in one unit and returns the rows:

```rhai,graph=modern,script
g.define_query(#{name: "hire", language: "gql", params: #{name: "string", company: "string"},
    body: "MATCH (c:software {name: $company}) INSERT (p:person {name: $name})-[:created]->(c) RETURN p.name AS name"});
g.get_query("hire")["effects"].map(|e| e.text);   // ["Create Data(vertex \"person\")", "Create Data(edge \"created\")"]
g.query("hire", #{name: "ann", company: "lop"}).select("name").to_list()   // "ann"
```

A [saved query](../users_guide/saved_queries.md) in Rhai whose body runs `g.gql("INSERT ...")`
with a literal query text is a writing query too: its `effects` name the labels it creates. A
computed query text claims every write; pass values as `$parameters` instead.

In a script, a writing `gql()` takes the write lock like a Gremlin write (`is_mutating()` is
true), and the playground asks before running it again. Gremlin steps may follow:

```rhai,graph=modern
g.gql("INSERT (c:city {name: 'Berlin'}) RETURN c").select("c").values("name").to_list()   // "Berlin"
```

## In Rust

Run a writing query on the writing source, `traversal_mut()`, under the write lock. The read
source refuses it with the error a Gremlin write gets there:

```rust,ignore
use graphersal::prelude::*;

let graph = Graph::new(TraversalGraph::tinkerpop_modern());
graph
    .write()
    .traversal_mut()
    .gql("MATCH (p:person {name: 'marko'}) INSERT (p)-[:likes]->(:fruit {name: 'apple'})")
    .iterate()?;

// The read source cannot write:
let error = graph.read().traversal().gql("INSERT (:x)").iterate().unwrap_err();
assert_eq!(
    error.root_cause().to_string(),
    "Invalid element state: Cannot mutate immutable graph reference"
);
```

## The plan

An `INSERT` lowers to the internal steps `gql:insert_node` and `gql:insert_edge`, one per
element of the pattern, in text order ([Plans and Performance](plans.md#writes)). Because the
plan writes, its traversers are never merged into one with a higher bulk: each row creates its
own elements.

## Not yet

| Construct | Until then |
|---|---|
| `SET n.k = v`, `SET n:A`, `SET n += {..}` | Gremlin `property()`, `add_label()` |
| `REMOVE n.k`, `REMOVE n:A` | Gremlin `remove_property()`, `drop_label()` |
| `DELETE n`, `DETACH DELETE n`, `NODETACH DELETE n` | Gremlin `drop()` |
| `MERGE` | Gremlin `merge_v()`, `merge_e()` |
| `FOREACH (x IN list \| ...)` | `FOR x IN list INSERT ...` |
| `INSERT` in a `UNION` operand, a path variable in `INSERT` | run the write on its own; name the nodes and edges |

```rhai,graph=modern
g.gql("MATCH (p:person) SET p.checked = true")   // error: SET is not yet supported in this version
g.gql("MATCH (n) DETACH DELETE n")   // error: DETACH DELETE is not yet supported in this version
g.gql("MERGE (n:person {name: 'ann'})")   // error: MERGE is not yet supported in this version
g.gql("MATCH (p:person) RETURN p").select("p").property("checked", true).count().next()   // 4
```
