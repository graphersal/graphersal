# Writing with GQL

`gql()` writes too. This version supports **`INSERT`** (Cypher `CREATE` is its
[alias](aliases.md)), which creates nodes and edges, and **`SET`** and **`REMOVE`**, which change
the properties and labels of existing ones, and **`DELETE`** / **`DETACH DELETE`**, which remove
them; each runs once for each row that reaches it. Cypher's **`MERGE`**, which finds a pattern
or creates it when it is missing, is accepted as a [compatibility form](aliases.md#merge)
([MERGE](#merge)).

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
existing node and may not carry labels or properties (`42002`; change an existing node with
[`SET`](#set), as in `MATCH (p:person) SET p:admin`). An edge without a direction, an edge with
a label expression (`:r|s`), a node label expression other than a conjunction, a path variable
(`p = ...`) and `WHERE` or a quantifier inside the pattern are errors before the run; the help
shows the accepted form.

```rhai,graph=modern
g.gql("INSERT (a:x)-[:r]-(b:y)")   // error: INSERT needs a direction
g.gql("MATCH (p:person) INSERT (p:admin)")   // error: the variable `p` is already bound
```

## SET

`SET` takes one or more comma-separated items and applies them to the elements of each row that
reaches it, left to right, so a later item sees the earlier ones:

| Item | Effect |
|---|---|
| `SET n.k = expr` (also `SET (n).k = expr`) | sets one property of the node or edge `n` |
| `SET n = {k: expr, ...}` | replaces every property: the keys missing from the map are removed |
| `SET n += {k: expr, ...}` | merges the map in (a Cypher [compatibility form](aliases.md#set-and-remove)): other keys stay |
| `SET n:A` / `SET n IS A` | adds the label `A` to the node `n`; `SET n:A:B` and `SET n IS A & B` add both |

```rhai,graph=modern
g.gql("MATCH (p:person {name: 'marko'}) SET p.age = p.age + 1, p.checked = true RETURN p.age AS age, p.checked AS checked").to_list()   // #{"age": 30, "checked": true}
g.gql("MATCH (p:person {name: 'marko'}) SET p:admin:staff RETURN labels(p) AS labels").to_list()   // #{"labels": ["person", "admin", "staff"]}
g.gql("MATCH (p:person {name: 'marko'}) SET p += {city: 'Rome'} RETURN properties(p) AS props").to_list()   // #{"props": #{"age": 29, "city": "Rome", "name": "marko"}}
g.gql("MATCH (p:person {name: 'marko'}) SET p = {name: 'marko', city: 'Rome'} RETURN properties(p) AS props").to_list()   // #{"props": #{"city": "Rome", "name": "marko"}}
g.gql("MATCH (a {name: 'marko'})-[e:knows]->(b) SET e.checked = true RETURN b.name AS b, e.checked AS checked ORDER BY b").to_list()   // josh true, vadas true
```

**`NULL` removes.** GQL stores no `NULL`, so `SET n.k = NULL` removes the property, and a `NULL`
value in the map of `SET n = {..}` or `SET n += {..}` removes its key. A `NULL` **target** (a
variable an `OPTIONAL MATCH` left `NULL`) is skipped: the item does nothing for that row. A
`NULL` map on the right of `=` or `+=` is an error (`22004`): write `SET n = {}` to remove every
property, or `coalesce($map, {})` for a parameter that may be `NULL`.

```rhai,graph=modern
g.gql("MATCH (p:person {name: 'marko'}) SET p.age = NULL RETURN keys(p) AS keys").to_list()   // #{"keys": ["name"]}
g.gql("MATCH (p:person {name: 'marko'}) SET p += {age: NULL, city: 'Rome'} RETURN properties(p) AS props").to_list()   // #{"props": #{"city": "Rome", "name": "marko"}}
g.gql("OPTIONAL MATCH (p:person {name: 'nobody'}) SET p.seen = true RETURN p").to_list()   // #{"p": ()}
g.gql("MATCH (p:person {name: 'marko'}) SET p = $props", #{props: ()})   // error: the right side of SET p = .. is NULL, not a map
```

**Values** follow the rules of `INSERT`: literals, `$parameters`, functions and the row's
variables; lists and maps are stored as they are; a node, an edge or a path cannot be a property
value (`22G03`). Every value of a map is checked before the item writes anything. On the right of
`=` and `+=` a node or an edge stands for its properties (`SET n = m` copies them, `SET n = n`
changes nothing); any other value that is not a map is an error (`22G03`). An aggregate cannot be
a value: aggregate in a `WITH` first.

```rhai,graph=modern
g.gql("MATCH (p:person {name: 'marko'}), (q:person {name: 'josh'}) SET p = q RETURN properties(p) AS props").to_list()   // #{"props": #{"age": 32, "name": "josh"}}
g.gql("MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) WITH p, count(s) AS n SET p.projects = n RETURN p.name AS name, p.projects AS n ORDER BY name").to_list()
// josh 2, marko 1, peter 1, vadas 0
g.gql("MATCH (p:person {name: 'marko'}) SET p.self = p")   // error: a property cannot hold a graph element
```

**Labels.** A label item changes a node's label set: adding a label it has, like removing one it
lacks, is no change. An edge has exactly one label, its type, so a label item on an edge is an
error: `42002` before the run when the variable is an edge of a pattern, `22G03` when a row
holds an edge (Gremlin's `set_label()` changes an edge's label). The item names labels joined by
`:` or `&`; `|`, `!` and `%` are syntax errors.

```rhai,graph=modern
g.gql("MATCH (a)-[e:knows]->(b) SET e:friend")   // error: an edge has exactly one label
```

**Targets.** The target of an item is a variable of the row: `n.k`, `(n).k` or `n`. A deeper
target (`n.address.city = ..`, `head(list).k = ..`) is not supported yet (`0A000`); name the
element first, as in `WITH head(list) AS x SET x.k = ..`. The variable must be bound before the
`SET` (`42002`).

## REMOVE

`REMOVE` takes comma-separated items like `SET`: `REMOVE n.k` removes a property of a node or an
edge, `REMOVE n:A` (also `REMOVE n IS A`, `REMOVE n:A:B`) removes labels from a node, and the
last label may go too. Removing what is not there changes nothing, and a `NULL` target is
skipped.

```rhai,graph=modern
g.gql("MATCH (p:person {name: 'marko'}) REMOVE p.age RETURN keys(p) AS keys").to_list()   // #{"keys": ["name"]}
g.gql("MATCH (s:software {name: 'ripple'}) SET s:product REMOVE s:software RETURN labels(s) AS labels").to_list()   // #{"labels": ["product"]}
g.gql("MATCH (p:person {name: 'marko'}) REMOVE p.nothing, p:nothing RETURN labels(p) AS labels, keys(p) AS keys").to_list()   // #{"keys": ["name", "age"], "labels": ["person"]}
```

The same in Gremlin:

```rhai,graph=modern
g.v().has("name", "marko").property("age", 30).add_label("admin").values("age").next()   // 30
g.v().has("name", "marko").remove_property("age").properties().key().to_list()   // "name"
g.v().has("name", "ripple").add_label("product").drop_label("software").label().next()   // "product"
```

## DELETE

`DELETE` takes one or more comma-separated expressions and removes the nodes and edges they
give, for every row that reaches it. **`DETACH DELETE`** removes a node together with all its
edges. A plain `DELETE` (GQL's default, also written **`NODETACH DELETE`**) removes only nodes
without edges: a node that still has an edge fails the query (`G1001`) and nothing is removed.
Edges the same clause deletes do not count, so `DELETE e, n` removes a node and the edge that
held it in one clause.

```rhai,graph=modern,script
g.gql("MATCH (p:person {name: 'marko'}) DETACH DELETE p").iterate();
[g.v().count().next(), g.e().count().next()]   // [5, 3]
```

```rhai,graph=modern
g.gql("MATCH (a {name: 'marko'})-[e:knows]->(b {name: 'vadas'}) DELETE e RETURN b.name AS still_here").to_list()   // #{"still_here": "vadas"}
g.gql("MATCH (s:software {name: 'ripple'})<-[e]-() DELETE e, s RETURN count(*) AS n").to_list()   // #{"n": 1}
g.gql("MATCH (p:person {name: 'marko'}) DELETE p")   // error: `p` is a node that still has edges (G1001)
```

The NODETACH check runs for the **whole clause** before it removes anything: the help of the
error names the `DETACH DELETE` that works, built from the failing query.

**Targets** are expressions: a node or an edge variable, a **path** (`MATCH p = .. DETACH DELETE
p` removes its nodes and edges), a **list** of them (also nested, such as `collect(n)`), or an
expression that gives one (a map member, `head(list)`). A `NULL` target (an `OPTIONAL MATCH`
that found nothing) is skipped, and an element already removed (by an earlier row or clause) is
not removed twice. A target that gives another value fails the query: before the run when the
text shows it (`DELETE 1 + 1`, `42002`), on the row otherwise (`22G03`). A label test is not a
target: `DELETE n:L` is a syntax error whose help names `REMOVE n:L`, which removes the label and
keeps the node.

```rhai,graph=modern,script
g.gql("MATCH p = (:person {name: 'marko'})-[:knows]->(:person {name: 'vadas'}) DETACH DELETE p").iterate();
[g.v().count().next(), g.e().count().next()]   // [4, 3]
```

```rhai,graph=modern
g.gql("OPTIONAL MATCH (p:person {name: 'nobody'}) DELETE p RETURN p").to_list()   // #{"p": ()}
g.gql("MATCH (p:person) WITH collect(p) AS people DETACH DELETE people RETURN size(people) AS n").to_list()   // #{"n": 4}
g.gql("MATCH (n:person) DELETE n:person")   // error: a label test is not an element; REMOVE n:person removes the label
```

**After a `DELETE`.** A deleted element is gone for the rest of the query: reading its
properties, labels, `element_id()` or endpoints is an **error**, never a silent `NULL`, and so
is returning it (`RETURN n`, also with `LIMIT 0` after it, and `RETURN p` of a path holding it).
Read what you need **before** the `DELETE` and carry it in a `WITH`; `count(n)` and `n IS NULL`
still work, and `type(e)` of a deleted edge still gives its label (as in the openCypher tests).
A walk from a deleted node (`MATCH (n)-->(m)` after deleting `n`) finds nothing.

```rhai,graph=modern
g.gql("MATCH (p:person {name: 'vadas'}) WITH p, properties(p) AS props DETACH DELETE p RETURN props").to_list()   // #{"props": #{"age": 27, "name": "vadas"}}
g.gql("MATCH (p:person {name: 'vadas'}) DETACH DELETE p RETURN count(p) AS deleted").to_list()   // #{"deleted": 1}
g.gql("MATCH (a)-[e:created]->(s:software {name: 'lop'}) DELETE e RETURN type(e) AS t, a.name AS who ORDER BY who").to_list()   // three rows, t "created"
g.gql("MATCH (p:person {name: 'vadas'}) DETACH DELETE p RETURN p")   // error: the node was deleted earlier in this query
g.gql("MATCH (p:person {name: 'vadas'}) DETACH DELETE p RETURN p.name AS name")   // error: the node was deleted earlier in this query
```

Returning a deleted node is an error in `gql()` where Neo4j returns a value of it; the reasons
are on [Semantics](semantics.md#delete).

A query that fails after its `DELETE` changed nothing ([Units](#one-query-is-one-unit)): the
removed elements come back with their ids, labels, properties and edges. In Rust they are new
elements: a `VertexId` / `EdgeId` handle taken before the failed run is stale and must be looked
up again by id ([What a rollback restores](../users_guide/transactions.md#what-a-rollback-restores)),
exactly as after a failing Gremlin `drop()`. A Rhai vertex or edge value finds the element by its
id and stays usable.

The same in Gremlin:

```rhai,graph=modern
g.v().has("name", "marko").drop().count().next()   // 0
g.v().has("name", "marko").out_e("knows").where(__.in_v().has("name", "vadas")).drop().iterate()
```

`drop()` of a vertex always removes its edges (it is `DETACH DELETE`); the NODETACH check has no
Gremlin twin.

## MERGE

`MERGE` is Cypher's "find or create": for each row it looks for its **one pattern**, and every
match becomes a row; when nothing matches, it **creates the whole pattern** and the row goes on
with the new elements. ISO GQL has no `MERGE` statement; `gql()` accepts it as a
[compatibility form](aliases.md#merge) with Cypher's meaning, so a query written for Neo4j runs
as it is. The second `MERGE` of the first block finds the city the first one created:

```rhai,graph=modern,script
g.gql("MERGE (c:city {name: 'Berlin'}) RETURN c.name AS name").to_list();   // #{"name": "Berlin"}: created
g.gql("MERGE (c:city {name: 'Berlin'}) RETURN c.name AS name").to_list();   // #{"name": "Berlin"}: found
g.v().has_label("city").count().next()   // 1
```

```rhai,graph=modern
g.gql("MERGE (p:person {name: 'marko'}) RETURN p.age AS age").to_list()   // #{"age": 29}: found, nothing created
g.gql("MERGE p = (:person {name: 'marko'})-[:knows]->(f:person) RETURN f.name AS friend, path_length(p) AS hops ORDER BY friend").to_list()   // josh and vadas: two matches, two rows
```

**The pattern.** One path pattern (a second one after `,` is a syntax error: write one `MERGE`
per pattern). Its nodes have the labels written (`:A:B` or `:A&B`; a found node carries all of
them and may have more) and a property map compared with GQL `=`; its edges have exactly one
label and no quantifier, and the pattern has no `WHERE`. A path variable (`MERGE p = ..`) gives
the found or the created path.

**A variable bound before** stands for its element: write it alone, as `(a)`, and `MERGE`
reuses it in both cases, so an edge between matched nodes is created only when it is missing.
Labels or properties on a bound node (`MERGE (a:admin)`), a bound edge variable, and a pattern
that is only a bound node (`MATCH (a) MERGE (a)`) are refused before the run (`42002`); change a
bound node with `SET`. An unbound variable is new: the `MERGE` finds or creates its element.

```rhai,graph=modern,script
g.gql("MATCH (a:person {name: 'marko'}), (b:person {name: 'peter'}) MERGE (a)-[e:knows]->(b) RETURN b.name AS to").to_list();   // #{"to": "peter"}: created
g.gql("MATCH (a:person {name: 'marko'}), (b:person {name: 'peter'}) MERGE (a)-[e:knows]->(b) RETURN b.name AS to").to_list();   // found
g.e().has_label("knows").count().next()   // 3: marko's two edges and the new one
```

```rhai,graph=modern
g.gql("MATCH (a:person) MERGE (a)")   // error: a bound variable alone has nothing to find or create
g.gql("MATCH (a:person) MERGE (a:admin)")   // error: a bound node carries no new labels in MERGE; SET a:admin does
g.gql("MERGE (a:person {name: 'x'}), (b:person {name: 'y'})")   // error: MERGE takes exactly one pattern
```

**Either direction.** An edge without a direction, `-[:T]-`, matches an edge in either
direction; when it is created, it points from left to right, from the node written first:

```rhai,graph=modern
g.gql("MATCH (a:person {name: 'josh'}), (b:person {name: 'marko'}) MERGE (a)-[e:knows]-(b) RETURN startNode(e).name AS from").to_list()   // #{"from": "marko"}: the stored edge marko -> josh
g.gql("MATCH (a:person {name: 'vadas'}), (b:person {name: 'josh'}) MERGE (a)-[e:knows]-(b) RETURN startNode(e).name AS from").to_list()   // #{"from": "vadas"}: created left to right
```

**Per row, and each row sees the rows before it.** `MERGE` runs once for each row, in order,
and a row finds what an earlier row created: a repeated item is created once, so `MERGE` is
idempotent within a query as across queries. The whole pattern is matched or created: `MATCH
(p:person) MERGE (p)-[:in]->(g:group {name: 'x'})` creates a group for every person without
such an edge, because the pattern with the edge does not match for them; to share one group,
`MERGE` the group first and the edge second:

```rhai,graph=modern
g.gql("FOR n IN ['Rome', 'Rome', 'Oslo'] MERGE (c:city {name: n}) RETURN count(*) AS rows, count(DISTINCT c) AS cities").to_list()   // #{"cities": 2, "rows": 3}
g.gql("MATCH (p:person) WHERE p.age > 30 MERGE (g:group {name: 'over30'}) MERGE (p)-[:in]->(g) RETURN count(DISTINCT g) AS groups").to_list()   // #{"groups": 1}
```

**ON CREATE SET and ON MATCH SET.** After the pattern, `ON CREATE SET <items>` changes the
elements of a row that the `MERGE` created, and `ON MATCH SET <items>` those of a row it found.
The items are the items of [`SET`](#set) and may read every variable of the row (the pattern's
and the earlier ones). Both may be written several times and in any order; they apply in the
order written, each in its own case.

```rhai,graph=modern,script
g.gql("FOR name IN ['marko', 'ann'] MERGE (p:person {name: name}) ON CREATE SET p.visits = 1 ON MATCH SET p.visits = coalesce(p.visits, 0) + 1").iterate();
g.gql("FOR name IN ['marko', 'ann'] MERGE (p:person {name: name}) ON CREATE SET p.visits = 1 ON MATCH SET p.visits = coalesce(p.visits, 0) + 1 RETURN p.name AS name, p.visits AS visits ORDER BY name").to_list()   // ann 2, marko 2
```

```rhai,graph=modern
g.gql("MERGE (p:person {name: 'marko'}) ON CREATE SET p.new = true ON MATCH SET p.new = false RETURN p.new AS new").to_list()   // #{"new": false}
g.gql("MERGE (p:person {name: 'marko'}) ON MATCH SET p.seen = 1 ON MATCH SET p.seen = p.seen + 1 RETURN p.seen AS seen").to_list()   // #{"seen": 2}
```

**NULL in the pattern.** `MERGE` cannot look for or create a `NULL` property value: a property
of the pattern that is `NULL` on a row (a missing parameter, an `OPTIONAL MATCH` that found
nothing) fails the query with `22004` when that row runs, and nothing of the query is kept.
Keep such rows away with `FILTER x IS NOT NULL`, or give a default with `coalesce()`:

```rhai,graph=modern
g.gql("FOR x IN [1, NULL] MERGE (n:num {v: x})")   // error: MERGE cannot match or create `{v: NULL}` (22004)
g.gql("FOR x IN [1, NULL] FILTER x IS NOT NULL MERGE (n:num {v: x}) RETURN count(*) AS n").to_list()   // #{"n": 1}
```

**A `MERGE` that only finds changes nothing.** It commits no change: a commit hook and the
journal see nothing, and the dev server's answer reports no change summary and no commit. An
`ON MATCH SET` that runs is a change, as a `SET` is.

The same in Gremlin: [`merge_v()`](../traversal/steps/merge_v.md) finds or creates a vertex,
[`merge_e()`](../traversal/steps/merge_e.md) an edge between given vertices, and the
`fold().coalesce(__.unfold(), __.add_v(..))` idiom does it by hand. Their `onCreate` option map
replaces the creation map, while `ON CREATE SET` changes the created element afterwards.

```rhai,graph=modern
g.merge_v(#{"T.label": "city", name: "Berlin"}).values("name").to_list()   // "Berlin"
g.v().has_label("city").has("name", "Berlin").fold().coalesce(__.unfold(), __.add_v("city").property("name", "Berlin")).values("name").to_list()   // "Berlin"
```

The same in plain GQL, without the compatibility form: a `CALL` body that inserts only when
nothing exists yet, then a `MATCH` that reads the result:

```rhai,graph=modern
g.gql("CALL { FILTER NOT EXISTS { MATCH (:city {name: 'Berlin'}) } INSERT (:city {name: 'Berlin'}) } MATCH (c:city {name: 'Berlin'}) RETURN count(c) AS cities").to_list()   // #{"cities": 1}
```

## Once per row

A write clause runs once for each row that reaches it, so the clauses before it decide how many
elements an `INSERT` creates and how often a `SET` applies (`SET n.c = n.c + 1` over two rows of
the same node adds 2). A `MATCH` gives one row per match, `FOR` (Cypher `UNWIND`) one per list
item, and a variable of the row connects the new elements to what it matched:

```rhai,graph=modern,script
g.gql("MATCH (p:person {name: 'marko'}) INSERT (p)-[:likes]->(f:fruit {name: 'apple'}) RETURN f.name AS fruit").to_list();   // #{"fruit": "apple"}
g.gql("FOR n IN $names INSERT (:person {name: n, born: $year})", #{names: ["ann", "bob"], year: 1990}).iterate();
g.gql("MATCH (p:person) WHERE p.born = 1990 RETURN p.name AS name ORDER BY name").to_list()   // #{"name": "ann"}, #{"name": "bob"}
```

A write clause may stand after `MATCH`, `OPTIONAL MATCH`, `FOR`, `WITH`, `LET`, `FILTER` and
another write clause, and further clauses may follow it: a `MATCH` after the `INSERT` sees the new
elements, and a new node can be connected to every row a later `MATCH` finds:

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
write (`42001`), and a write in an operand of `UNION` is not supported yet.

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

A value a row carried before a `SET` keeps the value it had: `WITH p.age AS old SET p.age = 0`
still returns the old age in `old`, while `p.age` reads the new one.

```rhai,graph=modern
g.gql("MATCH (p:person {name: 'marko'}) WITH p, p.age AS old SET p.age = 0 RETURN old, p.age AS now").to_list()   // #{"now": 0, "old": 29}
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

A `SET` that fails on one row leaves the rows before it unchanged too:

```rhai,graph=modern,script
let set = g.gql("MATCH (p:person) SET p.score = 100 / (p.age - 27)").execute();   // fails on vadas (27)
g.v().has("score").count().next()   // 0
```

Commit hooks see the query's `ChangeSet` and may veto it, a dry run shows its changes without
keeping them, and a Store's journal records it like any Gremlin write
([Change capture](../users_guide/transactions.md#change-capture-changeset)).

## Schema

An `INSERT` creates each element in one storage call with all its properties, so a schema sees
the whole element ([Schemas](../users_guide/schema_enforcement.md)): defaults on create are
filled in, a `required` property is satisfied by the map, write-time coercion applies, and a
closed schema refuses undeclared labels and properties. `SET` and `REMOVE` are checked like the
Gremlin steps that do the same: a value of the wrong type, removing a `required` property (also
with `SET n.k = NULL` or a `SET n = {..}` without it) and a label the schema does not allow fail
the query. A violation fails the query (and changes nothing), with a help line written for
GQL:

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

A `MERGE` that creates creates like an `INSERT` of its pattern: the schema's defaults are filled
in and the pattern's map must satisfy `required`. Its `ON CREATE SET` items run afterwards, as a
second write in the same query, so a `required` property given only there is missing when the
element is created and fails the query; put it into the pattern's map instead (where it is also
part of what `MERGE` looks for):

```rhai,graph=empty,script
g.set_schema(#{mode: "open", vertices: #{person: #{schema: #{type: "object", properties: #{
    name: #{type: "string"}, active: #{type: "boolean", default: true}}, required: ["name"]}}}});
g.gql("MERGE (p:person {name: 'ann'}) RETURN p.active AS active").to_list()   // #{"active": true}
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
the reads of its `MATCH` clauses. `SET` and `REMOVE` ask `Update`, as `property()`,
`remove_property()`, `add_label()` and `drop_label()` do: on `Data(vertex)` for a node (any
label, since a node's label set is not fixed by the pattern), on `Data(edge "label")` for an
edge whose pattern names its labels, and on any element when the kind is not known before the
run (an item of a `FOR` list). It asks nothing for the properties: they are part of creating
the element. `DELETE` asks `Delete`, as `drop()` does: on `Data(vertex)` for a node, on
`Data(edge "label")` for an edge whose pattern names its labels, and on any element for a path,
a list or a value whose kind is known only at run time; `DETACH` asks nothing more (as `drop()`
of a vertex, which removes its edges too). `MERGE` asks what its two halves need, whether a
row will find or create: `Read` for the search (as a `MATCH` of its pattern), `Create` per label
of every element the pattern can create (as the `INSERT` of it), and, only when it has
`ON CREATE SET` / `ON MATCH SET` items, their `Update`s (as `SET`). The step's own `Create` and
`Update` requests are checked first, then the `Read`s of its search; a policy that denies
`Create` refuses a `MERGE` even where every row would only find. `merge_v()` and `merge_e()`
ask `Read` on what they search first, then `Create` and `Update` on any vertex or edge (no
labels, and `Update` even without `on_create` / `on_match` options). (Gremlin's `add_v(..).property(..)` asks `Create` and `Update`, because the check
runs before the optimizer folds the property into the creation; a policy that denies only
`Update` allows the `INSERT` and refuses its Gremlin twin.) A read-only host
(`AccessPolicy::read_only()`, the MCP server's `--mcp-read-only`, a read-only view of a past
state) refuses a GQL write before it runs.

## Saved queries and the DSL

A saved query may be written in GQL itself ([Saved queries in
GQL](../users_guide/saved_queries.md#saved-queries-in-gql)): the body is the plain query text,
its `$parameters` are the declared ones, and an `INSERT`, `SET`, `REMOVE`, `DELETE` or `MERGE` makes it
a query that writes, whose `effects` name what its plan creates, changes and deletes. A call
writes in one unit and returns the rows:

```rhai,graph=modern,script
g.define_query(#{name: "hire", language: "gql", params: #{name: "string", company: "string"},
    body: "MATCH (c:software {name: $company}) INSERT (p:person {name: $name})-[:created]->(c) RETURN p.name AS name"});
g.get_query("hire")["effects"].map(|e| e.text);   // ["Create Data(vertex \"person\")", "Create Data(edge \"created\")"]
g.query("hire", #{name: "ann", company: "lop"}).select("name").to_list()   // "ann"
```

```rhai,graph=modern,script
g.define_query(#{name: "birthday", language: "gql", params: #{name: "string"},
    body: "MATCH (p:person {name: $name}) SET p.age = p.age + 1 RETURN p.age AS age"});
g.get_query("birthday")["effects"].map(|e| e.text);   // ["Update Data(vertex)"]
g.query("birthday", #{name: "vadas"}).select("age").to_list()   // 28
```

```rhai,graph=modern,script
g.define_query(#{name: "unlink", language: "gql", params: #{}, body: "MATCH ()-[e:knows]->() DELETE e"});
g.get_query("unlink")["effects"].map(|e| e.text)   // ["Delete Data(edge \"knows\")"]
```

```rhai,graph=modern,script
g.define_query(#{name: "visit", language: "gql", params: #{name: "string"},
    body: "MERGE (p:person {name: $name}) ON MATCH SET p.visits = coalesce(p.visits, 0) + 1 RETURN p.name AS name"});
g.get_query("visit")["effects"].map(|e| e.text)   // ["Create Data(vertex \"person\")", "Update Data(vertex)"]
```

A [saved query](../users_guide/saved_queries.md) in Rhai whose body runs `g.gql("INSERT ...")`
with a literal query text is a writing query too: its `effects` name what it creates, changes
and deletes. A computed query text claims every write; pass values as `$parameters` instead.

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
element of the pattern, in text order ([Plans and Performance](plans.md#writes)); a `SET` clause
to one `gql:set` step and a `REMOVE` clause to one `gql:remove` step with its items, which pass
the row on unchanged. A `DELETE` clause lowers to one `gql:delete` step: it collects the targets
of all its rows first (the NODETACH check sees the whole clause), removes the edges, then the
nodes, and passes the rows on unchanged. A `MERGE` lowers to one `gql:merge` step with two
children: the search (the plan of a `MATCH` of the pattern, starting from a bound node or using
a [property index](plans.md) where one serves it) and the creation (the `gql:insert_node` /
`gql:insert_edge` chain of the pattern); `.profile()` shows both under the step. Because the plan writes, its traversers are never merged into one with a
higher bulk: each row creates its own elements and applies its own changes.

```rhai,graph=modern
g.gql("MATCH (p:person {name: 'marko'}) SET p.age = 1 RETURN p").profile()
g.gql("MERGE (c:city {name: 'Rome'}) RETURN c").profile()
```

## Not yet

| Construct | Until then |
|---|---|
| `FOREACH (x IN list \| ...)` | `FOR x IN list INSERT ...` |
| a write in a `UNION` operand, a path variable in `INSERT` | run the write on its own; name the nodes and edges |
| a deeper `SET` / `REMOVE` target (`n.a.b`, `head(l).k`), Cypher's dynamic `n[key]` | name the element with `WITH .. AS x` and write the item on `x`; for a nested value Gremlin `property()` with a [jpath key](../users_guide/jpath_keys.md) |

```rhai,graph=modern
g.gql("MATCH (p:person {name: 'marko'}) SET p.address.city = 'Rome'")   // error: `p.address.city` as a SET target is not yet supported
```
