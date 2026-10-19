# GQL Conformance

This page lists which parts of ISO/IEC 39075 (GQL) `gql()` supports, in the style of the
conformance appendices that GQL implementations publish. It is **maintained by hand** together
with the code; when a construct lands, its row moves here in the same change.

GQL names its optional features with identifiers such as `G036` (graph pattern features),
`GQ..` (query features), `GE..` (expressions), `GF..` (functions), `GA..` (additional
behaviour), `GB..` (lexical), `GL..` (literals), `GH..` (other). This page cites only those
identifiers and short descriptive names of our own; it does not reproduce the standard. An
identifier marked *(provisional)* could not be checked against the standard text yet and may be
corrected.

Status: **supported**, **partial** (what is missing is named), **not yet** (planned), **no**
(refused for good; see [Refused Constructs](refused.md)).

## Core language

The mandatory core of a GQL read query, by construct.

| Construct | Status | Notes |
|---|---|---|
| `MATCH` with node and edge patterns, comma-separated patterns, several `MATCH` clauses | supported | trail (`DIFFERENT EDGES`) match mode per clause |
| `WHERE` after `MATCH`, inline `WHERE` and property maps in element patterns | supported | |
| `RETURN`, `RETURN *`, `RETURN DISTINCT`, `AS` | supported | `RETURN` is mandatory |
| `ORDER BY ... ASC / DESC` | supported | `NULL` last ascending, first descending |
| label expressions `&`, `\|`, `!`, `( )` | supported | |
| property reference `x.key` | supported | |
| comparison, `AND` / `OR` / `NOT`, `IS [NOT] NULL` | supported | three-valued logic |
| arithmetic `+ - * / %`, string concatenation `\|\|` | supported | |
| simple and searched `CASE`, `COALESCE` | supported | `NULLIF` not yet |
| `UPPER`, `LOWER`, `CHAR_LENGTH` / `CHARACTER_LENGTH` | supported | other string functions not yet |
| parameters `$name` | supported | values only, never text |
| aggregate functions `COUNT`, `SUM`, `AVG`, `MIN`, `MAX`, `COLLECT_LIST`, with `DISTINCT` | supported | implicit grouping by the items without an aggregate; Cypher `collect` alias |
| `OPTIONAL MATCH` | supported | also as the first clause |
| Cypher `WITH` | supported | not GQL; accepted with Cypher's meaning ([Cypher Aliases](aliases.md#the-cypher-clause-with)) |
| a query without `MATCH` (`RETURN 1`) | not yet | |
| data-modifying statements (`INSERT`, `SET`, `REMOVE`, `DELETE`) | no | `gql()` is read-only |
| catalog statements (`CREATE GRAPH`, `CREATE GRAPH TYPE`, ...), session and transaction commands | no | the host application manages graphs, schemas and transactions |
| value types | partial | the GQL types Graphersal has (integers, floats, strings, booleans, lists, records/maps, dates and times, `NULL`); path values; no byte strings |

## Optional features

| ID | Feature | Status | Notes |
|---|---|---|---|
| G002 | different-edges match mode | partial | the semantics is the default and only mode; writing `DIFFERENT EDGES` is not accepted yet |
| G004 | path variables | supported | `p = (a)-->(b)`, `path_length(p)` (Cypher `length`); `nodes()` / `relationships()` not yet |
| G005 | path search prefix in a path pattern | not yet | `ANY`, `ALL`, `SHORTEST` prefixes |
| G010 | explicit `WALK` keyword | not yet | |
| G011 | advanced path modes: `TRAIL` | not yet | trail is the semantics of every `MATCH`; the keyword is not accepted yet |
| G012 | advanced path modes: `SIMPLE` | not yet | |
| G013 | advanced path modes: `ACYCLIC` | not yet | |
| G016 | any path search | not yet | |
| G017 | all shortest path search | not yet | Gremlin `repeat()` |
| G018 | any shortest path search | not yet | Gremlin `repeat()` |
| G019 | counted shortest path search | not yet | |
| G035 | quantified paths | not yet | a parenthesized path with a quantifier, `((a)-->(b)){1,3}`; quantify a single edge instead |
| G036 | quantified edges | supported | `{m,n}`, `{n}`, `{m,}`, `{,n}`, `*`, `+` on full and abbreviated edges; Cypher `[*m..n]` and its forms are aliases; trail per clause |
| G037 | questioned paths | not yet | |
| G043 | complete full edge patterns | partial | pointing left, right, either direction (`-[e]-`, `<-[e]->`); the undirected forms (`~[e]~` and its variants) are refused, Graphersal edges are directed |
| G044 | basic abbreviated edge patterns | supported | `->`, `<-`, `-` (Cypher `-->`, `<--`, `--`) |
| G045 | complete abbreviated edge patterns | partial | `<->` supported; `~`, `<~`, `~>` refused |
| G050 | parenthesized path pattern: `WHERE` clause *(provisional)* | not yet | parenthesized path patterns are not supported yet |
| G074 | label expression: wildcard label | supported | `%` |
| G100 | `ELEMENT_ID` function | supported | the id as a string; Cypher `elementId` alias |
| G110 | `IS DIRECTED` predicate *(provisional)* | not yet | every edge is directed |
| G111 | `IS LABELED` predicate | supported | Cypher `x:A` alias |
| G112 | `IS SOURCE OF` / `IS DESTINATION OF` predicates *(provisional)* | not yet | |
| G113 | `ALL_DIFFERENT` predicate *(provisional)* | not yet | |
| G114 | `SAME` predicate *(provisional)* | not yet | |
| G115 | `PROPERTY_EXISTS` predicate *(provisional)* | not yet | `x.key IS NOT NULL` tests a property |
| GA03 | explicit ordering of nulls | not yet | `NULLS FIRST` / `NULLS LAST` |
| GA04 | universal comparison *(provisional)* | no | comparing values of different kinds is `NULL` |
| GA07 | ordering by discarded binding variables *(provisional)* | supported | `ORDER BY` may read variables that `RETURN` does not return (not after `RETURN DISTINCT`) |
| GB02 | double minus sign comments | supported | `-- ...`, with the edge rule of [Writing GQL in Rhai Strings](rhai_strings.md#comments) |
| GB03 | double solidus comments | supported | `// ...` |
| GE07 | Boolean `XOR` | supported | |
| GF12 | `CARDINALITY` function | supported | |
| GF13 | `SIZE` function | supported | also on strings as the Cypher alias of `CHAR_LENGTH` |
| GH02 | undirected edge patterns | no | Graphersal edges are directed; `-[e]-` matches either direction |
| GL01 | hexadecimal literals | supported | `0x1F` |
| GL02 | octal literals | supported | `0o17` |
| GL03 | binary literals | supported | `0b101` |
| GQ01 | `USE` graph clause | no | `gql()` runs on the graph of its traversal source |
| GQ02 | composite query: `OTHERWISE` *(provisional)* | not yet | |
| GQ03 | composite query: `UNION` | not yet | Gremlin `union()` |
| GQ04 | composite query: `EXCEPT DISTINCT` *(provisional)* | not yet | |
| GQ06 | composite query: `INTERSECT DISTINCT` *(provisional)* | not yet | |
| GQ08 | `FILTER` statement | supported | `FILTER [WHERE] <condition>` |
| GQ09 | `LET` statement | supported | |
| GQ10 | `FOR` statement: list value support | not yet | Gremlin `unfold()` |
| GQ12 | `ORDER BY` and page statement: `OFFSET` clause | partial | `OFFSET` / `SKIP` after `RETURN` and in `WITH`; the standalone statement form not yet |
| GQ13 | `ORDER BY` and page statement: `LIMIT` clause | partial | `LIMIT` after `RETURN` and in `WITH`; the standalone statement form not yet |
| GQ14 | complex expressions in sort keys *(provisional)* | supported | `ORDER BY element_id(e)`, `ORDER BY n.age * 2` |
| GQ15 | `GROUP BY` clause *(provisional)* | supported | after the `RETURN` items, `GROUP BY ()` included; the keys must be the items without an aggregate (grouping is implicit, so it is optional); a key may also repeat an item's expression (`GROUP BY p.name`), beyond ISO's variable references |
| GQ18 | scalar subqueries *(provisional)* | not yet | |
| GQ20 | advanced linear composition with `NEXT` | not yet | chain Gremlin steps after `gql()` |
| GQ21 | `OPTIONAL`: multiple `MATCH` statements *(provisional)* | not yet | one `OPTIONAL MATCH` per optional part is supported; the block form `OPTIONAL { MATCH ... MATCH ... }` is not |
| GP01 | inline procedure *(provisional)* | not yet | `CALL { ... }` |
| GP04 | named procedure calls *(provisional)* | no | vendor procedures (`CALL db.*`, `apoc.*`) are refused |

Every feature not listed here is not supported. Cypher spellings that GQL does not have are on
[Cypher Aliases](aliases.md).

## Implementation-defined choices

Where the standard leaves a choice to the implementation, `gql()` makes this one (Neo4j's,
where it has one):

| Topic | Choice |
|---|---|
| `NULL` in `ORDER BY` | last ascending, first descending |
| order of values of different types | map, vertex, edge, list, date/time, UUID, string, boolean, number, vector, `NULL` |
| row order without `ORDER BY` | unspecified |
| `DISTINCT`, grouping keys, `DISTINCT` in aggregates | GQL equality: `1` and `1.0` are one value, `NULL`s form one group |
| aggregates of no rows (no grouping keys) | one row: `count` 0, `sum` 0, `collect_list` `[]`, `avg` / `min` / `max` `NULL` (Neo4j's values; provisional) |
| `min` / `max` over values of different kinds | the `ORDER BY` order, never an error |
| `sum` of integers | an integer; overflow is an error (`22003`) |
| unbounded quantifiers | no own limit: trail ends them; `repeat.max_loops` and `traversal.max_traversers` are errors, never a shortened result |
| `WITH ... LIMIT ... WHERE` | the `WHERE` filters after `ORDER BY` / `OFFSET` / `LIMIT` (Cypher's order; provisional) |
| name of an unaliased column | the expression as written (provisional) |
| element ids | strings, the same as Gremlin's `id()`; vertex and edge ids are separate spaces |
| integer division | truncates toward zero; `%` takes the sign of the dividend |
| integer overflow | an error (`22003`), never a wrap |
| text and numbers | no implicit conversion: `'a' + 1` is an error (`22G03`) |
| GQLSTATUS codes | `42001` for syntax errors; the others provisional ([Errors](errors.md)) |
