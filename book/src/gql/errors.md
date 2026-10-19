# Errors

A GQL error is a `TraverserError::Gql` (feature `gql`) with a kind, a message, the position
(line and column inside the query), a caret under the offending text, the expected tokens, a
"did you mean" suggestion where one fits, and a **GQLSTATUS** code, the five-character status
code of ISO GQL (derived from SQL's SQLSTATE). Like every Graphersal error it has a `Help:` line
that names the fix.

## Errors before the run

A query that does not parse or bind fails **before the traversal runs**, even when no traverser
would ever reach the step, as [`math()`](../users_guide/math.md#errors) does with an equation.
On the empty graph, where nothing matches, the query is still checked:

```rhai,graph=empty
g.gql("MATCH (n:person) RETURN n.name +")   // error: unexpected end of input
```

```text
Error: GQL syntax error at line 1, column 33: unexpected end of input (step #1 'gql:error("42001")' refused the query before the traversal ran)
  at #1: v().gql:error("42001")
             ^^^^^^^^^^^^^^^^^^
  query:     MATCH (n:person) RETURN n.name +
                                             ^
  expected:  an expression
  GQLSTATUS: 42001
Help: Expected an expression at the marked position. Fix the query at the marked position: a GQL read query is MATCH <pattern> [WHERE <condition>], then any further MATCH, OPTIONAL MATCH, WITH, LET or FILTER clauses, then RETURN <items>, for example g.gql("MATCH (p:person) WHERE p.age > 30 RETURN p.name") or g.gql("MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) WITH p, count(s) AS n RETURN p.name, n").
```

The plan of a query that failed this way is the marker step `gql:error("<GQLSTATUS>")`, which
raises the error before anything runs; the `query:` line, not the plan, shows the position. In a multi-line query the error names the line and
shows only that line:

```rhai,graph=modern
g.gql(`MATCH (n:person)
       WHERE n.age >
       RETURN n`)   // error: unexpected keyword RETURN at line 3
```

```text
Error: GQL syntax error at line 3, column 8: unexpected keyword RETURN (step #1 'gql:error("42001")' refused the query before the traversal ran)
  at #1: v().gql:error("42001")
             ^^^^^^^^^^^^^^^^^^
  line 3:           RETURN n
                    ^^^^^^
  expected:  an expression
  GQLSTATUS: 42001
```

Misspelt keywords, functions and variables get a suggestion:

```rhai,graph=modern
g.gql("MATCH (n:person) RETRUN n")   // error: unexpected `RETRUN`, did you mean RETURN
g.gql("MATCH (n:person) RETURN m.name")   // error: the variable `m` is not defined, did you mean n
g.gql("MATCH (n:person) WHERE n.age > $min RETURN n.name AS name")   // error: the parameter `$min` has no value
```

```text
Error: GQL syntax error at line 1, column 18: unexpected `RETRUN` (step #1 'gql:error("42001")' refused the query before the traversal ran)
  at #1: v().gql:error("42001")
             ^^^^^^^^^^^^^^^^^^
  query:        MATCH (n:person) RETRUN n
                                 ^^^^^^
  expected:     `,`, WHERE, MATCH, OPTIONAL MATCH, WITH, LET, FILTER, RETURN
  did you mean: RETURN
  GQLSTATUS:    42001
Help: Did you mean RETURN? Expected `,`, WHERE, MATCH, OPTIONAL MATCH, WITH, LET, FILTER, RETURN at the marked position. ...
```

## Errors while the query runs

A valid query can still fail on a row of the data: a division by zero, an overflow, an operand
of the wrong kind. These fail the traversal when the row reaches the expression (kind `Data`,
GQLSTATUS class `22`), with the caret under the expression and the step that ran it:

```rhai,graph=modern
g.gql("MATCH (n:person) RETURN n.age / 0 AS x")   // error: division by zero
g.gql("MATCH (n:person) RETURN n.name + 1 AS x")   // error: + joins two strings or adds two numbers, not a string and a number
g.gql("MATCH (n:person) RETURN type(n) AS x")   // error: type() takes an edge, not a node
```

```text
Error: Step #1 'gql:project("x")' execution failed
  at #1: v().has_label("person").gql:project("x")
                                 ^^^^^^^^^^^^^^^^
  plan:  v(labels: ["person"]).gql:project("x")
                               ^^^^^^^^^^^^^^^^
Caused by: GQL data exception at line 1, column 25: division by zero
  query:     MATCH (n:person) RETURN n.age / 0 AS x
                                     ^^^^^^^^^
  GQLSTATUS: 22012
Help: A row of the data made the marked expression fail. Keep such rows away from it with a WHERE condition or a CASE expression, ...
```

Comparisons never fail: comparing values of different kinds, or with `NULL`, is `NULL`
([Semantics](semantics.md#comparisons-across-types)).

`+` of a string and a number names the explicit conversions in its help:

```text
Help: Convert the number or boolean to text first, with toString(x) or CAST(x AS STRING), and join text with ||, as in g.gql("MATCH (p:person) RETURN p.name || ' is ' || toString(p.age) AS s"); or compute with numbers only, converting text with toInteger(x) or toFloat(x). ...
```

### Function errors

A function fails on an argument it cannot take, with a GQLSTATUS that says why and a help line
that leads with the fix for that function, then the general advice above:

```rhai,graph=modern
g.gql("MATCH (n:person) RETURN CAST(n.name AS INTEGER) AS x")   // error: invalid character value for cast (22018)
g.gql("MATCH (n:person) RETURN substring(n.name, -1) AS x")   // error: substring() takes a start of 0 or more (22011)
g.gql("MATCH (n:person) RETURN TRIM(BOTH 'ab' FROM n.name) AS x")   // error: TRIM(.. FROM ..) removes exactly one character (22027)
g.gql("MATCH (n:person) RETURN ln(n.age - 29) AS x")   // error: ln() of a number <= 0 (2201E)
g.gql("MATCH (n:person) RETURN sqrt(27 - n.age) AS x")   // error: sqrt() of a negative number (2201F)
g.gql("MATCH (n:person) RETURN range(1, n.age, 0) AS x")   // error: range() step 0 (22023)
g.gql("MATCH (n:person) RETURN toFloat(n.age > 30) AS x")   // error: toFloat() takes a number or a string, not a boolean (22G03)
```

```text
Caused by: GQL data exception at line 1, column 30: invalid character value for cast: 'marko' is not an INTEGER (expected a decimal number, surrounding spaces allowed)
  query:     MATCH (n:person) RETURN CAST(n.name AS INTEGER) AS x
                                          ^^^^^^
  GQLSTATUS: 22018
Help: CAST is strict: text that is not a value of the target type fails the query. The Cypher conversions are lenient: toInteger(x), toFloat(x) and toBoolean(x) give NULL for such text, and toIntegerOrNull(x), toFloatOrNull(x) and toBooleanOrNull(x) give NULL for a value of any other kind too, as in g.gql("MATCH (p:person) RETURN p.name, toInteger(p.name) AS n"). A row of the data made the marked expression fail. ...
```

A function `gql()` knows but does not support gets a message with the reason and the
alternative instead of "unknown function", and a misspelt name a suggestion:

```rhai,graph=modern
g.gql("MATCH (n:person) RETURN sin(n.age) AS x")   // error: sin() (a trigonometric function) is not yet supported
g.gql("MATCH (n:person) RETURN rand() AS x")   // refused: rand() is not supported: a non-deterministic function
g.gql("MATCH (n:person) RETURN n.age ^ 2 AS x")   // error: the exponentiation operator `^` is not yet supported
g.gql("MATCH (n:person) RETURN toInt(n.age) AS x")   // error: unknown function toInt(), did you mean toInteger()
```

```text
Error: GQL construct not supported yet at line 1, column 25: sin() (a trigonometric function) is not yet supported in this version (step #1 'gql:error("0A000")' refused the query before the traversal ran)
  ...
  GQLSTATUS: 0A000
Help: Gremlin math() after gql(), for example g.gql("MATCH (n:person) RETURN n.age AS a").select("a").math("sin(_)"). This construct comes in a later version of gql(); until then, write the query without it or as Gremlin steps.
```

## Scope, grouping and limits

The help of a semantic error explains the rule behind it. A variable that a `WITH` did not carry
on, a value read outside an aggregate that is not a grouping key, and a `GROUP BY` that leaves out
a grouping key each name the fix:

```rhai,graph=modern
g.gql("MATCH (p:person) WITH p.name AS name RETURN p.age")   // error: the variable `p` is not in scope: the WITH before does not carry it
g.gql("MATCH (p:person) RETURN p.name AS n, count(p.age) + p.age AS x")   // error: `p` is read outside an aggregate and is not a grouping key
g.gql("MATCH (p:person) RETURN p.name AS n, p.age AS a, count(*) GROUP BY n")   // error: the RETURN item `a` has no aggregate, so it is a grouping key, and GROUP BY does not list it
```

```text
Help: add `p` to the WITH items, as in WITH ..., p. WITH passes on only its columns: a variable it does not list is out of scope after it, so list every variable a later clause reads, ...
Help: add `p` to the RETURN items (it then becomes a grouping key), or read it inside an aggregate, as in collect(p). A RETURN or WITH with an aggregate groups the rows by its other items (the grouping keys): ...
Help: add it to GROUP BY (GROUP BY ..., a), or remove GROUP BY: grouping is implicit, the items without an aggregate are the keys. ...
```

An unbounded quantifier that reaches `repeat.max_loops` or `traversal.max_traversers` fails with
the ordinary limit error (`RepeatLimitExceeded`, `ResourceLimitExceeded`; no GQLSTATUS), never with
fewer rows. Its help is written for GQL: it names the quantified edge, an upper bound to give it
and the `g.with(..).gql(..)` that raises the limit ([Semantics](semantics.md#quantified-edges)):

```rhai,graph=modern
g.with("traversal.max_traversers", 3).gql("MATCH (a)-[:knows|created]->{1,}(b) RETURN b.name AS b")   // error: traversal.max_traversers exceeded
```

```text
Caused by: Resource limit 'traversal.max_traversers' exceeded (limit 3, reached 6)
Help: The gql() query kept more traversers (rows in the making) alive between two steps than 'traversal.max_traversers' allows (3). Each path a quantified edge (-[:knows|created]->{1,}) matches is a traverser of its own (trail keeps them apart, so they do not merge), ... give it a lower upper bound, as in -[:knows|created]->{1,5}. ...
```

A function whose result can be larger than its arguments (`range()`, `split()`, `replace()`,
`toString()` of a list or map, `||`) checks the size of its result before it builds it, against
`traversal.max_materialized_bytes` and the memory budget, like `collect_list()`. A result that is
too large fails with the ordinary limit error (`ResourceLimitExceeded`, no GQLSTATUS: the option
and its lock are the engine's), never with a shortened value; inside `gql()` its help names the
growing functions, a bound for them and the `g.with(..).gql(..)` that raises the limit:

```rhai,graph=modern
g.gql("MATCH (n:person) RETURN range(0, 9223372036854775807) AS r")   // error: Resource limit 'traversal.max_materialized_bytes' exceeded
g.with("traversal.max_materialized_bytes", 100).gql("MATCH (n:person) RETURN split(n.name, '') AS s")   // error: Resource limit 'traversal.max_materialized_bytes' exceeded
```

```text
Caused by: Resource limit 'traversal.max_materialized_bytes' exceeded (limit 1073741824, reached 18446744073709551615)
Help: The gql() query built a value larger than 'traversal.max_materialized_bytes' allows (1073741824 bytes); ... A function whose result can be larger than its arguments (range(), split(), replace(), || and toString() of a list or map) and the list of collect_list() are checked before they are built, so the query stops instead of running out of memory; the result is never cut short. Build smaller values: bound range() and replace() by the data, as in g.gql("MATCH (p:person) RETURN range(1, p.age / 10) AS r"), ... or raise the limit for the query, g.with("traversal.max_materialized_bytes", 2147483648).gql(..), ...
```

## GQLSTATUS codes

| Code | Kind | When |
|---|---|---|
| `42001` | syntax | the query does not parse: an unexpected token, a missing `RETURN`, an unclosed bracket, a quantifier with `m > n`, `GROUP BY` after `WITH` (grouping is implicit) or after `ORDER BY`; an unknown function or `CAST` type name (with a suggestion), a function called with the wrong number of arguments, the SQL form `SUBSTRING(s FROM a FOR b)` |
| `42002` | semantic | an undefined variable, a variable used as a node and an edge, an edge variable bound twice in one `MATCH`, a duplicate column name, a missing parameter, an `ORDER BY` key that `RETURN DISTINCT` dropped; a variable a `WITH` did not carry on, a `LET` name bound again; a variable read outside an aggregate that is not a grouping key, an aggregate in `WHERE` / `FILTER` / `LET` or inside another aggregate, `ORDER BY` after an aggregation reading something other than a column; a `GROUP BY` key that is not an item without an aggregate, or such an item missing from `GROUP BY`; a path or group variable bound twice |
| `0A000` | refused / not yet | a [refused construct](refused.md) or one [not supported yet](subset.md#not-supported-yet) (also a function: `sin()`, `date()`, `rand()`, `elements()`, `^`, `CAST` to `DATE`); also `gql()` after other steps |
| `54001` | limit | the query nests too deeply (more than 128 levels of parentheses, or an expression taller than 512) |
| `54001` | limit | a MATCH clause holds more than 64 edge patterns (over all its comma-separated paths); split it into several MATCH clauses that share a variable, or use Gremlin `repeat()` |
| `22G02` | data | a negative `OFFSET` / `LIMIT` (literal or parameter value) |
| `22G03` | data | a value of the wrong type: `OFFSET` / `LIMIT` not an integer, `'a' + 1`, `type()` of a vertex, a non-boolean condition, `sum()` / `avg()` of a non-number; a function argument of the wrong kind (`toFloat(true)`, a float where an integer is required, `toString()` of a vertex, edge or path, `trim(s, x)` with a string `s`) |
| `22012` | data | division or `%` by zero, also `mod(a, 0)` |
| `22003` | data | a numeric result out of range (integer overflow, also of `sum()`, `abs()`, `exp()`, `power()`); a conversion out of range (`toInteger(1.0e19)`, `CAST('1e400' AS FLOAT)`) |
| `22018` | data | `CAST` of text that is no value of the target type (`CAST('abc' AS INTEGER)`) |
| `22011` | data | a negative position or length in `substring()`, `left()`, `right()` |
| `22027` | data | a `TRIM(c FROM s)` character `c` that is not exactly one character |
| `2201E` | data | the logarithm of a number `<= 0`, or to a base `<= 0` or `1` (`ln`, `log`, `log10`) |
| `2201F` | data | `sqrt()` of a negative number, `power()` of 0 to a negative power or of a negative number to a non-integer power |
| `22023` | data | an argument outside the function's values: `range()` step 0, a negative `round()` precision or an unknown rounding mode, a negative `replace()` limit, a negative `n` in the list `trim(list, n)` |

`42001` is fixed; the other codes are **provisional**: they follow the standard's classes and
may still be refined before the GQL support is complete. The function codes (`22018`, `22011`,
`22027`, `2201E`, `2201F`, `22023`) are the ISO / SQL-heritage subclasses of class `22`; Neo4j
reports the same failures with its own vendor subclasses (`22N38`, `22N03`, ...). Match on the kind
(`GqlErrorKind`) in code, and treat the code as diagnostic information.

```rhai,graph=modern
g.gql("MATCH (n:person) RETURN n.name AS name ORDER BY name LIMIT $l", #{l: -1})   // error: LIMIT must be a non-negative integer (22G02)
g.v().gql("MATCH (n) RETURN n")   // error: gql() starts a traversal in this version (0A000)
```

## In Rust

```rust,ignore
use graphersal::error::GqlErrorKind;
use graphersal::prelude::*;

let graph = TraversalGraph::tinkerpop_modern();
let error = graph.traversal().gql("MATCH (n:person) RETRUN n").to_list().unwrap_err();
if let TraverserError::Gql { kind, gqlstatus, line, column, .. } = error.root_cause() {
    assert_eq!(*kind, GqlErrorKind::Syntax);
    assert_eq!(gqlstatus, "42001");
    assert_eq!((*line, *column), (1, 18));
}
```

The `DiagnosticError` trait gives the help text (`help()`) and the labelled lines (`details()`:
`query`, `expected`, `did you mean`, `GQLSTATUS`) shown above.
