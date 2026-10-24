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

How far the Cypher compatibility forms agree with Neo4j's behaviour is measured against the
openCypher TCK: [TCK Compliance](tck_compliance.md).

Status: **supported**, **partial** (what is missing is named), **not yet** (planned), **no**
(refused for good; see [Refused Constructs](refused.md)).

## Core language

The mandatory core of a GQL query, by construct.

| Construct | Status | Notes |
|---|---|---|
| `MATCH` with node and edge patterns, comma-separated patterns, several `MATCH` clauses | supported | trail (`DIFFERENT EDGES`) match mode per clause |
| `WHERE` after `MATCH`, inline `WHERE` and property maps in element patterns | supported | |
| `RETURN`, `RETURN *`, `RETURN DISTINCT`, `AS` | supported | `RETURN` is mandatory unless the last clause writes; `FINISH` ends a query without a result |
| `ORDER BY ... ASC / DESC` | supported | `NULL` last ascending, first descending |
| label expressions `&`, `\|`, `!`, `( )` | supported | |
| property reference `x.key` | supported | |
| comparison, `AND` / `OR` / `NOT`, `IS [NOT] NULL` | supported | three-valued logic |
| arithmetic `+ - * / %`, string concatenation `\|\|` | supported | |
| simple and searched `CASE`, `COALESCE`, `NULLIF` | supported | |
| `UPPER`, `LOWER`, `CHAR_LENGTH` / `CHARACTER_LENGTH`, `LEFT`, `RIGHT`, `TRIM(s)` | supported | `TRIM` removes spaces by default; `NORMALIZE` not yet |
| `BYTE_LENGTH` / `OCTET_LENGTH` of a character string | supported | the number of UTF-8 bytes; Graphersal has no byte strings |
| list trim `TRIM(list, n)` | supported | drops the last `n` items |
| parameters `$name` | supported | values only, never text |
| aggregate functions `COUNT`, `SUM`, `AVG`, `MIN`, `MAX`, `COLLECT_LIST`, with `DISTINCT` | supported | implicit grouping by the items without an aggregate; Cypher `collect` alias |
| binary set functions `PERCENTILE_CONT`, `PERCENTILE_DISC` | supported | value first, then the percentile (0.0 to 1.0, `22003` outside); Cypher `percentileCont` / `percentileDisc` aliases (stage 3f) |
| `OPTIONAL MATCH` | supported | also as the first clause |
| `EXISTS` predicate: `EXISTS { MATCH ... }`, `EXISTS { <pattern> }`, `EXISTS ( ... )` *(provisional: whether the standard makes it core or an optional feature, and its identifier, are still to be checked)* | supported | anywhere an expression stands, nested too; TRUE or FALSE, never `NULL`; the body is a pattern, `MATCH` clauses, or a whole query ending in `RETURN` ([EXISTS](subset.md#exists-and-pattern-predicates)); Cypher pattern predicates as [aliases](aliases.md) |
| Cypher `WITH` | supported | not GQL; accepted with Cypher's meaning ([Cypher Aliases](aliases.md#the-cypher-clause-with)) |
| record constructor `{k: expr, ...}` / `RECORD {k: expr, ...}` and list value constructor `[a, b]` | supported | `RECORD` is contextual: a variable named `record` stays usable, back-quoted before a projection that starts with `key:` or is empty ([Expressions](subset.md#expressions)) |
| Cypher map projections `p {.k, .*, x, k: expr}`, list comprehensions `[x IN l WHERE c \| e]`, pattern comprehensions `[(a)-->(b) WHERE c \| e]` | supported | not GQL; accepted as compatibility forms with Cypher's meaning ([Cypher Aliases](aliases.md#map-projections-and-comprehensions)); a pattern comprehension is a subquery; `all()` / `any()` / `none()` / `single()` / `reduce()` likewise, three-valued as the openCypher TCK ([Cypher Aliases](aliases.md#list-predicates-and-reduce)) |
| Cypher `COUNT { }` and `COLLECT { }` subqueries | supported | not GQL; compatibility forms with Cypher's meaning ([Cypher Aliases](aliases.md#count-and-collect)); bodies as for `EXISTS`, `COLLECT` returns one item; one deviation below |
| a query without `MATCH` (`RETURN 1`) | supported | a query may start with `FOR`, `LET`, `WITH`, `FILTER` or `RETURN`; it starts from one empty row |
| data-modifying statements: `INSERT` | supported | nodes with label conjunctions and properties, directed edges with one label, once per row; Cypher `CREATE` as an [alias](aliases.md) ([Writing with GQL](writes.md)) |
| data-modifying statements: `SET`, `REMOVE` | supported | property items `n.k = v`, all properties `n = {..}`, label items `n:A` / `n IS A`; once per row, items left to right, `NULL` removes; Cypher's `+=` and `SET n = <expression>` as [compatibility forms](aliases.md#set-and-remove) ([Writing with GQL](writes.md#set)) |
| data-modifying statements: `DELETE` | supported | `DETACH DELETE` and `[NODETACH] DELETE` of nodes, edges, paths and lists of them, once per row; NODETACH checked per clause (`G1001`); reading a deleted element afterwards is an error ([Writing with GQL](writes.md#delete), [Semantics](semantics.md#delete)) |
| Cypher `MERGE` with `ON CREATE SET` / `ON MATCH SET` | supported | not GQL; a compatibility form with Cypher's meaning ([Cypher Aliases](aliases.md#merge)): one pattern, per row every match a row or the whole pattern created, rows see earlier rows' creations, `NULL` in the pattern `22004` ([Writing with GQL](writes.md#merge), [Semantics](semantics.md#merge)) |
| catalog statements (`CREATE GRAPH`, `CREATE GRAPH TYPE`, ...), session and transaction commands | no | the host application manages graphs, schemas and transactions |
| value types | partial | the GQL types Graphersal has (integers, floats, strings, booleans, lists, records/maps, dates and times, `NULL`); path values; no byte strings |

## Optional features

| ID | Feature | Status | Notes |
|---|---|---|---|
| G002 | different-edges match mode | partial | the semantics is the default and only mode; writing `DIFFERENT EDGES` is not accepted yet |
| G004 | path variables | supported | `p = (a)-->(b)`, `path_length(p)` (Cypher `length`); Cypher's `nodes(p)` / `relationships(p)` as [compatibility functions](aliases.md#compatibility-functions) |
| G005 | path search prefix in a path pattern | not yet | `ANY`, `ALL`, `SHORTEST` prefixes |
| G010 | explicit `WALK` keyword | not yet | |
| G011 | advanced path modes: `TRAIL` | not yet | trail is the semantics of every `MATCH`; the keyword is not accepted yet |
| G012 | advanced path modes: `SIMPLE` | not yet | |
| G013 | advanced path modes: `ACYCLIC` | not yet | |
| G016 | any path search | not yet | |
| G017 | all shortest path search | not yet | Gremlin `repeat()` |
| G018 | any shortest path search | not yet | Gremlin `repeat()` |
| G019 | counted shortest path search | not yet | |
| G035 | quantified paths *(provisional)* | partial | a parenthesized path pattern with `{m,n}`, `{n}`, `{m,}`, `{,n}`, `*`, `+` ([Quantified path patterns](subset.md#quantified-path-patterns)); body variables are group variables (lists), the outer nodes merge with the body's first and last node, trail per body edge; one chain per body, no quantifier, path variable, path mode or search prefix inside a body, a body variable is a new name (each refused with `0A000`) |
| G036 | quantified edges | supported | `{m,n}`, `{n}`, `{m,}`, `{,n}`, `*`, `+` on full and abbreviated edges; Cypher `[*m..n]` and its forms are aliases; trail per clause |
| G037 | questioned paths *(provisional)* | supported | `((..))?`: the body once or not at all; its variables are single elements, `NULL` when it did not match ([Semantics](semantics.md#quantified-path-patterns)) |
| G043 | complete full edge patterns | partial | pointing left, right, either direction (`-[e]-`, `<-[e]->`); the undirected forms (`~[e]~` and its variants) are refused, Graphersal edges are directed |
| G044 | basic abbreviated edge patterns | supported | `->`, `<-`, `-` (Cypher `-->`, `<--`, `--`) |
| G045 | complete abbreviated edge patterns | partial | `<->` supported; `~`, `<~`, `~>` refused |
| G050 | parenthesized path pattern: `WHERE` clause *(provisional)* | partial | `((x)-[e]->(y) WHERE <condition>)` with or without a quantifier, run in every iteration; it reads the body's variables, parameters and values (a condition reading an outer variable is not yet supported); a parenthesized path pattern without a quantifier matches once and keeps singletons |
| G074 | label expression: wildcard label | supported | `%` |
| G100 | `ELEMENT_ID` function | supported | the id as a string; Cypher `elementId` alias |
| G110 | `IS DIRECTED` predicate *(provisional)* | not yet | every edge is directed |
| G111 | `IS LABELED` predicate | supported | Cypher `x:A` alias |
| G112 | `IS SOURCE OF` / `IS DESTINATION OF` predicates *(provisional)* | supported | `n IS [NOT] SOURCE OF e`, `n IS [NOT] DESTINATION OF e`: the stored direction of the edge; Cypher's `startNode(e)` / `endNode(e)` as compatibility functions |
| G113 | `ALL_DIFFERENT` predicate *(provisional)* | not yet | |
| G114 | `SAME` predicate *(provisional)* | not yet | |
| G115 | `PROPERTY_EXISTS` predicate *(provisional)* | not yet | `x.key IS NOT NULL` tests a property |
| GA03 | explicit ordering of nulls | not yet | `NULLS FIRST` / `NULLS LAST` |
| GA04 | universal comparison *(provisional)* | partial | `=` / `<>` between values of different kinds are FALSE / TRUE, the orderings `NULL` (Neo4j's reading, see [Differences from the standard](#differences-from-the-standard)); there is no total order across kinds outside `ORDER BY` |
| GA05 | cast specification | partial | `CAST(x AS INTEGER \| FLOAT \| STRING \| BOOLEAN)` and their synonyms (`INT`, `BIGINT`, `DOUBLE`, `VARCHAR`, `BOOL`, ...); the other target types (`DATE`, `LIST<..>`, `DECIMAL`, ...) not yet; strict: unconvertible text is `22018` |
| GA07 | ordering by discarded binding variables *(provisional)* | supported | `ORDER BY` may read variables that `RETURN` does not return (not after `RETURN DISTINCT`) |
| GB02 | double minus sign comments | supported | `-- ...`, with the edge rule of [Writing GQL in Rhai Strings](rhai_strings.md#comments) |
| GB03 | double solidus comments | supported | `// ...` |
| GD01 | updatable graphs *(provisional)* | partial | `INSERT` (Cypher `CREATE`), `SET`, `REMOVE`, `DELETE`; Cypher's `MERGE` (no ISO counterpart) as a [compatibility form](aliases.md#merge) ([Writing with GQL](writes.md)); the Cypher forms pass every in-scope case of the openCypher TCK's write clauses ([TCK Compliance](tck_compliance.md)) |
| GD02 | graph label set changes *(provisional)* | supported | `SET n:A` / `SET n IS A` and `REMOVE n:A` add and remove node labels; an edge's single label is not changed by them ([Writing with GQL](writes.md#set)) |
| GD03 | `DELETE` statement: subquery support *(provisional)* | supported | a `DELETE` item may hold a subquery, as in `DETACH DELETE COLLECT { MATCH (s)<-[e]-() RETURN e }`; a `CALL { }` body may end with a `DELETE` ([Writing with GQL](writes.md#delete)) |
| GD04 | `DELETE` statement: simple expression support *(provisional)* | supported | a `DELETE` item is an expression: a variable, a path, a list (`collect(n)`), a map member, a function result; `NULL` is skipped, another value is `42002` / `22G03` ([Semantics](semantics.md#delete)) |
| GE07 | Boolean `XOR` | supported | |
| GF01 | enhanced numeric functions | supported | `ABS`, `FLOOR`, `CEIL` / `CEILING`, `SQRT`, `MOD`; an integer argument keeps its type |
| GF02 | trigonometric functions *(provisional)* | not yet | Gremlin `math()` after `gql()` |
| GF03 | logarithmic functions | supported | `EXP`, `LN`, `LOG10`, `LOG(base, x)` *(provisional)*, `POWER`; an argument outside the domain is an error (`2201E`, `2201F`), never `NaN` |
| GF04 | enhanced path functions | partial | `PATH_LENGTH`; `ELEMENTS(p)` *(provisional)* not yet (Cypher's `nodes(p)` / `relationships(p)` instead) |
| GF05 | multi-character trim functions | supported | `BTRIM`, `LTRIM`, `RTRIM` with an optional set of characters (default: the space) |
| GF06 | explicit `TRIM` function | supported | `TRIM([LEADING \| TRAILING \| BOTH] [c] FROM s)`; `c` is one character (`22027` otherwise), the space by default |
| GF12 | `CARDINALITY` function | supported | |
| GF13 | `SIZE` function | supported | also on strings as the Cypher alias of `CHAR_LENGTH` |
| GH02 | undirected edge patterns | no | Graphersal edges are directed; `-[e]-` matches either direction |
| GL01 | hexadecimal literals | supported | `0x1F` |
| GL02 | octal literals | supported | `0o17` |
| GL03 | binary literals | supported | `0b101` |
| GQ01 | `USE` graph clause | no | `gql()` runs on the graph of its traversal source |
| GQ02 | composite query: `OTHERWISE` *(provisional)* | not yet | |
| GQ03 | composite query: `UNION` | supported | `UNION`, `UNION DISTINCT`, `UNION ALL`; operands return the same column names in any order; one kind per chain; `ORDER BY` / `OFFSET` / `LIMIT` per operand ([UNION and NEXT](subset.md#union-and-next)) |
| GQ04 | composite query: `EXCEPT DISTINCT` *(provisional)* | not yet | |
| GQ06 | composite query: `INTERSECT DISTINCT` *(provisional)* | not yet | |
| GQ08 | `FILTER` statement | supported | `FILTER [WHERE] <condition>` |
| GQ09 | `LET` statement | supported | |
| GQ10 | `FOR` statement: list value support | supported | `FOR x IN <list>` (Cypher `UNWIND <list> AS x`), also as the first clause; a `NULL` list gives no row, a value that is not a list one row ([Semantics](semantics.md#for-null-and-values-that-are-not-lists)) |
| GQ11 | `FOR` statement: `WITH ORDINALITY` *(provisional)* | supported | the position from 1 |
| GQ24 | `FOR` statement: `WITH OFFSET` *(provisional)* | supported | the position from 0 |
| GQ12 | `ORDER BY` and page statement: `OFFSET` clause | partial | `OFFSET` / `SKIP` after `RETURN` and in `WITH`; the standalone statement form not yet |
| GQ13 | `ORDER BY` and page statement: `LIMIT` clause | partial | `LIMIT` after `RETURN` and in `WITH`; the standalone statement form not yet |
| GQ14 | complex expressions in sort keys *(provisional)* | supported | `ORDER BY element_id(e)`, `ORDER BY n.age * 2` |
| GQ15 | `GROUP BY` clause *(provisional)* | supported | after the `RETURN` items, `GROUP BY ()` included; the keys must be the items without an aggregate (grouping is implicit, so it is optional); a key may also repeat an item's expression (`GROUP BY p.name`), beyond ISO's variable references |
| GQ18 | scalar subqueries *(provisional)* | not yet | |
| GQ20 | advanced linear composition with `NEXT` | partial | `RETURN .. NEXT ..`, also after a `UNION` and inside subquery bodies; `NEXT YIELD` not yet |
| GQ21 | `OPTIONAL`: multiple `MATCH` statements *(provisional)* | not yet | one `OPTIONAL MATCH` per optional part is supported; the block form `OPTIONAL { MATCH ... MATCH ... }` is not |
| GP01 | inline procedure *(provisional)* | supported | `[OPTIONAL] CALL { <query> }`, run once per incoming row, the body ending in `RETURN` ([CALL subqueries](subset.md#call-subqueries)) or, as a unit subquery, in a write ([Writing with GQL](writes.md#once-per-row)); `IN TRANSACTIONS` refused |
| GP02 | inline procedure with implicit nested variable scope *(provisional)* | partial | `CALL { }` without a scope clause is accepted but imports **nothing** (Neo4j Cypher 25's reading; the ISO scoping is still to be checked); import with `CALL (*)` or Cypher's importing `WITH` |
| GP03 | inline procedure with explicit nested variable scope *(provisional)* | supported | `CALL (a, b) { }`, `CALL (*) { }`, `CALL () { }` |
| GP04 | named procedure calls *(provisional)* | partial | `[OPTIONAL] CALL folder.name(args) [YIELD ..]` calls a saved query written in GQL of the graph's catalog, once per row, standalone or inside a query ([Calling saved queries](subset.md#calling-saved-queries)); arguments are constants or parameters; a saved query in Rhai and a call inside `EXISTS` / `COUNT` / `COLLECT` not yet; vendor procedures (`CALL db.*`, `dbms.*`, `apoc.*`, `gds.*`) are refused |
| GV39 | temporal types: date, local datetime and local time *(provisional)* | partial | `date()`, `local_time()`, `local_datetime()` (Cypher `localtime()`, `localdatetime()`) from ISO 8601 text, a map of fields or another temporal value, components (`d.year`), arithmetic with durations, comparison ([Dates and times](subset.md#dates-and-times)); values hold nanoseconds; the literals `DATE '..'`, `TIME '..'` / `LOCAL TIME '..'`, `TIMESTAMP '..'` / `LOCAL DATETIME '..'` and the clocks `CURRENT_DATE`, `LOCAL_TIME`, `LOCAL_TIMESTAMP` ([Literals](subset.md#literals)); `CAST(x AS DATE)` not yet |
| GV40 | temporal types: zoned datetime and zoned time *(provisional)* | partial | `zoned_datetime()` (Cypher `datetime()`) keeps a UTC offset and the zone name it was built with, its arithmetic following the zone; `zoned_time()` (Cypher `time()`) keeps a time of day and its offset ([Dates and times](subset.md#dates-and-times)); the literals `TIME '10:00+01:00'`, `DATETIME '..+01:00'` (also `ZONED TIME '..'`, `ZONED DATETIME '..'`) and the clocks `CURRENT_TIME`, `CURRENT_TIMESTAMP` ([Literals](subset.md#literals)); `CAST(x AS ZONED TIME)` not yet |
| GV41 | temporal types: duration *(provisional)* | partial | `duration()` from text or a map, the literal `DURATION '..'`, `+` / `-` / `*` / `/`, components; ISO `DURATION_BETWEEN` not yet (the Cypher compatibility function `duration.between()` instead); `<` on two durations is `NULL` |

Every feature not listed here is not supported. Cypher spellings that GQL does not have, and the
openCypher functions accepted without a GQL counterpart (`toInteger`, `substring`, `split`,
`round`, `keys`, `nodes`, the temporal `date.truncate()`, `duration.between()`, ...), are on
[Cypher Aliases](aliases.md).

## Implementation-defined choices

Where the standard leaves a choice to the implementation, `gql()` makes this one (Neo4j's,
where it has one):

| Topic | Choice |
|---|---|
| `NULL` in `ORDER BY` | last ascending, first descending |
| order of values of different types | map, vertex, edge, list, date/time, UUID, string, boolean, number, vector, `NULL` |
| row order without `ORDER BY` | unspecified |
| item order of a pattern comprehension | unspecified: the order the matches are found, which depends on the plan |
| item order of `COLLECT { }` | the order of the body's rows: its `ORDER BY` when it has one, unspecified otherwise |
| `UNION` operands with the same column names in another order | accepted, matched by name; the result has the first operand's column order (provisional) |
| order of the rows of a `UNION` | unspecified, also when the operands are sorted: sort after `NEXT` |
| a `CALL` body that imports nothing | still run once per incoming row; its rows are not cached |
| the `RETURN` items of a projecting `COUNT { }` body | not evaluated: the rows are counted, an expression that would fail raises no error (a deviation, [Semantics](semantics.md#count-and-collect)) |
| a comprehension variable named like another variable of the query | an error (`42002`) for a name in scope or one a `LET` / `WITH` value in scope still reads (stricter than Neo4j only there); a name no longer in scope may be used again ([Semantics](semantics.md#map-projections-and-comprehensions)) |
| `DISTINCT`, grouping keys, `DISTINCT` in aggregates | GQL equality: `1` and `1.0` are one value, `NULL`s form one group |
| aggregates of no rows (no grouping keys) | one row: `count` 0, `sum` 0, `collect_list` `[]`, `avg` / `min` / `max` `NULL` (Neo4j's values, which the openCypher TCK confirms; the ISO text is still to be checked) |
| `min` / `max` over values of different kinds | the `ORDER BY` order, never an error |
| `sum` of integers | an integer; overflow is an error (`22003`) |
| unbounded quantifiers | no own limit: trail ends them; `repeat.max_loops` and `traversal.max_traversers` are errors, never a shortened result (quantified edges and quantified path patterns alike) |
| outer nodes next to a quantified path pattern | merged with the body's first node of the first iteration and last node of the last iteration (Neo4j Cypher 25's reading) |
| order of a group variable's list | the iterations in the order the text reads, also when the plan walks the pattern from its far end |
| `WITH ... LIMIT ... WHERE` | the `WHERE` filters after `ORDER BY` / `OFFSET` / `LIMIT` (Cypher's order; provisional) |
| name of an unaliased column | the expression as written (Neo4j's names, such as `count(*)`, which the openCypher TCK confirms; the ISO text is still to be checked) |
| element ids | strings, the same as Gremlin's `id()`; vertex and edge ids are separate spaces |
| integer division | truncates toward zero; `%` takes the sign of the dividend |
| temporal precision | nanoseconds |
| default time zone | UTC: a `datetime` without an offset or zone, and the clocks without a zone |
| the current date and time | one instant per query, taken when it starts; `X.realtime()` is refused |
| `ORDER BY` of durations | by length, a month counted as 30 days (`<` on two durations is `NULL`) |
| `CAST(temporal AS STRING)` | the engine's ISO 8601 text (`'12:00:00'`); Cypher's `toString()` writes Neo4j's (`'12:00'`) |
| integer overflow | an error (`22003`), never a wrap |
| text and numbers | no implicit conversion: `'a' + 1` is an error (`22G03`); convert with `CAST(x AS STRING)` |
| `CAST` of a float to `INTEGER`, of text with surrounding spaces | truncated toward zero; the spaces are skipped (provisional) |
| `CAST` to `STRING` of a float, a boolean, a list or a map | Java's float text (`'1.0'`, `'1.0E7'`), `'true'` / `'false'`, Neo4j's list and map text (provisional) |
| `CAST(x AS BOOLEAN)` of a number; of `'unknown'` | `22G03`; `NULL` (provisional) |
| default character of `TRIM`, `BTRIM`, `LTRIM`, `RTRIM` | the space U+0020 only (provisional) |
| `BTRIM` / `LTRIM` / `RTRIM` with characters | any character of the set is removed (Neo4j's semantics; provisional) |
| functions and `NULL` | a `NULL` argument gives `NULL`, before the kinds are checked |
| strings in functions | positions and lengths count characters (Unicode scalar values) |
| GQLSTATUS codes | `42001` for syntax errors; the others provisional ([Errors](errors.md)) |
| order of the writes of one `INSERT` | per row: its nodes in text order, then its edges in text order |
| property values of `INSERT` | `NULL` is not stored; maps and lists of any kinds are stored as given (a [difference from Neo4j](semantics.md#writes)) |

## Differences from the standard

Where ISO GQL and Neo4j disagree and the openCypher TCK measures the Neo4j behaviour, `gql()`
follows Neo4j in these places (user decision, stage 3f):

| Topic | ISO GQL | `gql()` |
|---|---|---|
| `=` / `<>` / `IN` / simple `CASE` between values of different kinds | not comparable | `=` FALSE, `<>` TRUE (`1 = '1'`, `[1] = 1`, `1 IN ['1']`); the orderings `<` .. `>=` stay `NULL` ([Semantics](semantics.md#comparisons-across-types)) |
| `<` `<=` `>` `>=` on two lists | not comparable | lexicographic, three-valued |
| a float divided by zero | `22012` | IEEE 754: `Infinity`, `-Infinity` or `NaN`; an integer divided by zero stays `22012` |
| `NaN` in a comparison | (no `NaN` arises: every division by zero is `22012`) | never equal (`NaN = NaN` FALSE), every ordering against a number FALSE |

The deliberate differences from Neo4j are listed on [Semantics](semantics.md#differences-from-neo4j).
