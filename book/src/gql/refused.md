# Refused Constructs

Some constructs `gql()` refuses for good, not just for now. A refused construct fails before the
traversal runs, with GQLSTATUS `0A000`, a caret under it and a help line naming what to use
instead. (Constructs that are only *not supported yet* are listed on
[The Supported Subset](subset.md#not-supported-yet).) `GROUP BY` is neither: it is accepted after
the `RETURN` items, and grouping is implicit anyway; after `WITH` (no ISO counterpart) it is a
syntax error whose help shows the query without it ([Aggregation](subset.md#aggregation)).

| Construct | Why | Instead |
|---|---|---|
| undirected edges `~[e]~`, `<~[e]~`, `~[e]~>`, `~`, `<~`, `~>` | Graphersal edges are always directed; there is no undirected edge to match | `-[e]-`, an edge in either direction |
| catalog commands `CREATE INDEX`, `CREATE CONSTRAINT`, `CREATE GRAPH`, `CREATE DATABASE` | not part of a query: indexes, schemas and graphs are the host's | `g.create_index(..)`, `g.set_schema(..)`, opening another graph |
| `LOAD CSV` | a vendor extension, not GQL | the host's import (GraphSON, GraphML), or the rows as a list parameter: `FOR row IN $rows INSERT (..)` |
| vendor procedures: `CALL db.labels()`, `CALL apoc.*`, `YIELD` | vendor extensions, not GQL (an inline `CALL { }` subquery is [supported](subset.md#call-subqueries)) | the Gremlin steps that do the same, such as `g.v().label().dedup()` |
| `CALL { ... } IN TRANSACTIONS` | commits the writes of a subquery in batches; a `gql()` query is one unit, which commits at once or not at all | the same `CALL` without `IN TRANSACTIONS` |
| `SHOW ...` | a vendor command, not GQL | the catalog steps (`indexes()`, `queries()`, `get_schema()`, `statistics()`) |
| vendor path search `*BFS`, `*WSHORTEST`, `*ALLSHORTEST` | vendor extensions, not GQL | Gremlin `repeat()` |
| `id(n)` | Cypher's `id()` is an integer; Graphersal ids are strings, and returning a different type under the same name would mislead | `element_id(n)`, the id as a string |
| `exists(n.p)`, the Cypher 4 property test | `EXISTS` is GQL's subquery predicate and takes a graph pattern; reading it as a property test as well would give one keyword two meanings | `n.p IS NOT NULL`; `EXISTS { (n)-[:T]->() }` tests a pattern ([EXISTS](subset.md#exists-and-pattern-predicates)) |
| `USE graph` | `gql()` runs on the graph of its traversal source | open the other graph and run there |
| non-deterministic functions `rand()`, `randomUUID()` | every `gql()` function is pure, so a `LET` or `WITH` name may stand for its expression and the plan stays deterministic; a random value would differ between its uses | Gremlin after `gql()`: `sample(n)` or `coin(p)` to pick rows at random |
| vendor and namespaced functions `apoc.*`, `db.*`, `gds.*`, `text.*`, ... | vendor extensions, not GQL; the accepted Cypher functions are a fixed list ([compatibility functions](aliases.md#compatibility-functions)) | a `gql()` function, or Gremlin steps after `gql()` |

```rhai,graph=modern
g.gql("MATCH (a)~[e]~(b) RETURN a")   // refused: undirected edges (`~`) are not supported
g.gql("CALL db.labels() YIELD label RETURN label")   // refused: procedure calls are vendor extensions
g.gql("MATCH (p:person) CALL (p) { MATCH (p)-[:created]->(s) RETURN s.name AS sw } IN TRANSACTIONS RETURN sw")   // refused: CALL { } IN TRANSACTIONS commits writes in batches
g.gql("MATCH (p:person) CALL (p) { MATCH (p)-[:created]->(s) RETURN s.name AS sw } RETURN sw ORDER BY sw").to_list()   // lop, lop, lop, ripple
g.gql("SHOW INDEXES")   // refused: SHOW commands are vendor extensions
g.gql("CREATE INDEX FOR (n:person) ON (n.name)")   // refused: CREATE INDEX is a catalog command
g.gql("LOAD CSV FROM 'people.csv' AS row RETURN row")   // refused: LOAD CSV is a vendor extension
g.gql("MATCH (n) RETURN id(n)")   // refused: use element_id(n)
g.gql("MATCH (a)-[:knows*BFS]->(b) RETURN b")   // refused: *BFS is a vendor path search extension
g.gql("USE g MATCH (n) RETURN n")   // refused: USE is not supported
g.gql("MATCH (n:person) WHERE exists(n.age) RETURN n.name AS name")   // refused: exists(value) is the Cypher 4 property test
g.gql("MATCH (n:person) WHERE n.age IS NOT NULL RETURN n.name AS name ORDER BY name").to_list()   // josh, marko, peter, vadas
g.gql("MATCH (n) RETURN rand() AS r")   // refused: rand() is a non-deterministic function
g.gql("MATCH (n) RETURN apoc.text.join(['a', 'b'], ',') AS r")   // refused: apoc.text.join() is a vendor function
g.gql("MATCH (n:person) RETURN n.name AS name ORDER BY name").sample(1).count().next()   // 1
g.gql("MATCH (n) RETURN element_id(n) AS id ORDER BY id LIMIT 2").to_list()   // #{"id": "1"}, #{"id": "2"}
```

Writes are supported (`INSERT`, `SET`, `REMOVE`: [Writing with GQL](writes.md)); `DELETE` and
`MERGE` are not supported *yet* and are listed there. The full message of a refused construct reads:

```text
Error: GQL unsupported construct at line 1, column 1: CREATE INDEX is a catalog command, not part of a gql() query (step #1 'gql:error("0A000")' refused the query before the traversal ran)
  at #1: v().gql:error("0A000")
             ^^^^^^^^^^^^^^^^^^
  query:     CREATE INDEX FOR (n:person) ON (n.name)
             ^^^^^^^^^^^^
  GQLSTATUS: 0A000
Help: create a property index with g.create_index(#{label: "person", key: "age"}); every query that can use it then does. gql() does not support this construct; write this part of the query as Gremlin steps.
```
