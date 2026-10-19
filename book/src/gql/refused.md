# Refused Constructs

Some constructs `gql()` refuses for good, not just for now. A refused construct fails before the
traversal runs, with GQLSTATUS `0A000`, a caret under it and a help line naming what to use
instead. (Constructs that are only *not supported yet* are listed on
[The Supported Subset](subset.md#not-supported-yet).) `GROUP BY` is neither: it is accepted after
the `RETURN` items, and grouping is implicit anyway; after `WITH` (no ISO counterpart) it is a
syntax error whose help shows the query without it ([Aggregation](subset.md#aggregation)).

| Construct | Why | Instead |
|---|---|---|
| writes: `INSERT`, `CREATE`, `SET`, `REMOVE`, `DELETE`, `DETACH DELETE`, `MERGE` | `gql()` is read-only in this version; writes are a later phase | the Gremlin mutation steps `add_v()`, `add_e()`, `property()`, `drop()`, `merge_v()` ([Changing the Graph](../dsl/changing_the_graph.md)) |
| undirected edges `~[e]~`, `<~[e]~`, `~[e]~>`, `~`, `<~`, `~>` | Graphersal edges are always directed; there is no undirected edge to match | `-[e]-`, an edge in either direction |
| vendor procedures: `CALL db.labels()`, `CALL apoc.*`, `YIELD` | vendor extensions, not GQL | the Gremlin steps that do the same, such as `g.v().label().dedup()` |
| `SHOW ...` | a vendor command, not GQL | the catalog steps (`indexes()`, `queries()`, `get_schema()`, `statistics()`) |
| vendor path search `*BFS`, `*WSHORTEST`, `*ALLSHORTEST` | vendor extensions, not GQL | Gremlin `repeat()` |
| `id(n)` | Cypher's `id()` is an integer; Graphersal ids are strings, and returning a different type under the same name would mislead | `element_id(n)`, the id as a string |
| `USE graph` | `gql()` runs on the graph of its traversal source | open the other graph and run there |
| non-deterministic functions `rand()`, `randomUUID()` | every `gql()` function is pure, so a `LET` or `WITH` name may stand for its expression and the plan stays deterministic; a random value would differ between its uses | Gremlin after `gql()`: `sample(n)` or `coin(p)` to pick rows at random |
| vendor and namespaced functions `apoc.*`, `db.*`, `gds.*`, `text.*`, ... | vendor extensions, not GQL; the accepted Cypher functions are a fixed list ([compatibility functions](aliases.md#compatibility-functions)) | a `gql()` function, or Gremlin steps after `gql()` |

```rhai,graph=modern
g.gql("CREATE (n:person {name: 'ann'}) RETURN n")   // refused: CREATE writes to the graph; gql() is read-only
g.gql("MATCH (n) DETACH DELETE n RETURN n")   // refused: DETACH DELETE writes to the graph
g.gql("MATCH (a)~[e]~(b) RETURN a")   // refused: undirected edges (`~`) are not supported
g.gql("CALL db.labels() YIELD label RETURN label")   // refused: procedure calls are vendor extensions
g.gql("SHOW INDEXES")   // refused: SHOW commands are vendor extensions
g.gql("MATCH (n) RETURN id(n)")   // refused: use element_id(n)
g.gql("MATCH (a)-[:knows*BFS]->(b) RETURN b")   // refused: *BFS is a vendor path search extension
g.gql("USE g MATCH (n) RETURN n")   // refused: USE is not supported
g.gql("MATCH (n) RETURN rand() AS r")   // refused: rand() is a non-deterministic function
g.gql("MATCH (n) RETURN apoc.text.join(['a', 'b'], ',') AS r")   // refused: apoc.text.join() is a vendor function
g.gql("MATCH (n:person) RETURN n.name AS name ORDER BY name").sample(1).count().next()   // 1
g.gql("MATCH (n) RETURN element_id(n) AS id ORDER BY id LIMIT 2").to_list()   // #{"id": "1"}, #{"id": "2"}
```

A write is done in Gremlin, and the result read back with GQL:

```rhai,graph=modern,script
g.add_v("person").property("name", "ann").property("age", 41).iterate();
g.gql("MATCH (p:person) WHERE p.age > 35 RETURN p.name AS name ORDER BY name").to_list()   // #{"name": "ann"}
```

The full refusal message reads:

```text
Error: GQL unsupported construct at line 1, column 1: CREATE writes to the graph; gql() is read-only (step #1 'gql:error("0A000")' refused the query before the traversal ran)
  at #1: v().gql:error("0A000")
             ^^^^^^^^^^^^^^^^^^
  query:     CREATE (n:person {name: 'ann'}) RETURN n
             ^^^^^^
  GQLSTATUS: 0A000
Help: gql() is read-only; use the Gremlin mutation steps (add_v(), add_e(), property(), drop(), merge_v()). gql() runs read queries (MATCH ... RETURN ...); write the rest of the query as Gremlin steps.
```
