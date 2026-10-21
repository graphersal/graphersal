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
Help: Expected an expression at the marked position. Fix the query at the marked position: a GQL read query is MATCH <pattern> [WHERE <condition>], then any further MATCH, OPTIONAL MATCH, WITH, LET or FILTER clauses, then RETURN <items>, for example g.gql("MATCH (p:person) WHERE p.age > 30 RETURN p.name") or g.gql("MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) WITH p, count(s) AS n RETURN p.name, n"). CALL (p) { .. RETURN .. } runs a query for each row, UNION combines the records of two queries and NEXT continues after a RETURN, as in g.gql("MATCH (p:person) RETURN p NEXT MATCH (p)-[:created]->(s) RETURN p.name, s.name"). FOR x IN <list> (Cypher UNWIND <list> AS x) gives one row per item of a list, and a query may also start with FOR, LET, WITH, FILTER or RETURN, as in g.gql("FOR n IN ['marko', 'josh'] MATCH (p:person {name: n}) RETURN n, p.age").
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
  expected:     `,`, WHERE, MATCH, OPTIONAL MATCH, WITH, LET, FILTER, FOR, UNWIND, CALL, RETURN
  did you mean: RETURN
  GQLSTATUS:    42001
Help: Did you mean RETURN? Expected `,`, WHERE, MATCH, OPTIONAL MATCH, WITH, LET, FILTER, FOR, UNWIND, CALL, ... at the marked position. ...
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

Comparisons never fail: `=` of values of different kinds is FALSE (`<>` TRUE), an ordering
`<` of them is `NULL`, and any comparison with `NULL` is `NULL`
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
g.gql("MATCH (n:person) RETURN toInt(n.age) AS x")   // error: unknown function toInt(), did you mean toInteger()
```

```text
Error: GQL construct not supported yet at line 1, column 25: sin() (a trigonometric function) is not yet supported in this version (step #1 'gql:error("0A000")' refused the query before the traversal ran)
  ...
  GQLSTATUS: 0A000
Help: Gremlin math() after gql(), for example g.gql("MATCH (n:person) RETURN n.age AS a").select("a").math("sin(_)"). This construct comes in a later version of gql(); until then, write the query without it or as Gremlin steps.
```

### Dates and times

The [temporal functions](subset.md#dates-and-times) fail with these codes; the help line shows
the accepted forms with a runnable example:

| Code | When |
|---|---|
| `22007` | text that is no value of the type (`date('nonsense')`, `date('2015-13-01')`, an offset in `localtime('12:00+01:00')`, an offset that is not the zone's in `'..+02:00[Europe/London]'`); a map with an unknown key or with two groups of date keys; an unknown time zone |
| `22008` | a field out of range (`month: 13`, `day: 30` in February, `hour: 25`) or a value outside the years -9999 to 9999, also as the result of arithmetic (`date('9999-12-31') + duration('P1D')`) |
| `22G03` | a component the value's type does not have (`date(..).hour`; the message lists the components it has); a constructor argument of the wrong kind (`date(42)`, `localdatetime(date(..))`); arithmetic without a meaning (`date + date`, `date * 2`); a truncation or difference of a value without the part it needs (`date.truncate('day', localtime(..))`, `duration.inDays()` of two local times) |
| `22023` | a truncation unit the type does not have (`date.truncate('hour', ..)`) or an unknown unit |
| `22012` | a duration divided by zero |
| `0A000` | a part below the microsecond (`'12:00:00.0000001'`, `nanosecond: 5`), Cypher's zoned `time()` and ISO `zoned_time()` (not yet); `X.realtime()` (refused) |

```rhai,graph=empty
g.gql("RETURN date('nonsense') AS d")   // error: cannot parse "nonsense" as a date (22007)
g.gql("RETURN date({year: 2015, mnth: 3}) AS d")   // error: the map has an unknown field mnth (22007)
g.gql("RETURN datetime({year: 2015, timezone: 'Mars/Olympus'}) AS d")   // error: unknown time zone (22007)
g.gql("RETURN date({year: 2015, month: 13, day: 1}) AS d")   // error: the field month cannot be 13 (22008)
g.gql("RETURN date('9999-12-31') + duration('P1D') AS d")   // error: the result is outside the supported range (22008)
g.gql("RETURN date('2015-01-01').foo AS x")   // error: a date has no component foo; its components are year, quarter, ... (22G03)
g.gql("RETURN date('2015-01-01') * 2 AS x")   // error: * with a temporal value takes a duration and a number (22G03)
g.gql("RETURN date.truncate('fortnight', date('2017-10-11')) AS d")   // error: unknown unit 'fortnight' (22023)
g.gql("RETURN duration('P1D') / 0 AS d")   // error: division by zero (22012)
g.gql("RETURN localtime('12:00:00.0000001') AS t")   // error: a part below the microsecond is not yet supported (0A000)
```

```text
Caused by: GQL data exception at line 1, column 13: date(): the field month cannot be 13
  query:     RETURN date({year: 2015, month: 13, day: 1}) AS d
                         ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
  GQLSTATUS: 22008
Help: Temporal fields stay within their ranges (month 1 to 12, day within its month, week within its week-numbering year, hour 0 to 23, minute and second 0 to 59, millisecond 0 to 999) and every value within the years -9999 to 9999, as in g.gql("RETURN date({year: 1984, month: 10, day: 11}) AS d, localtime({hour: 23, minute: 59, second: 59, millisecond: 999}) AS t"). A row of the data made the marked expression fail. ...
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
Help: add `p` to the WITH items, as in WITH ..., p. WITH passes on only its columns: a variable it does not list is out of scope after it (only the WHERE of a WITH without an aggregate still reads it), so list every variable a later clause reads, ...
Help: add `p` to the RETURN items (it then becomes a grouping key), or read it inside an aggregate, as in collect(p). A RETURN or WITH with an aggregate groups the rows by its other items (the grouping keys): ...
Help: add it to GROUP BY (GROUP BY ..., a), or remove GROUP BY: grouping is implicit, the items without an aggregate are the keys. ...
```

A `FOR` (or `UNWIND`) defines new names; reusing a name in scope, giving the item and its
position one name, or using the position as a node is an error whose help says what `FOR`
defines:

```rhai,graph=modern
g.gql("MATCH (p:person) FOR p IN [1] RETURN p")   // error: the variable `p` is already defined; FOR defines a new variable
g.gql("FOR x IN [3, 1, 2] WITH OFFSET x RETURN x")   // error: `x` names both the item and its position
```

```text
Help: choose another name, as in FOR p2 IN <list>. FOR x IN <list> (Cypher UNWIND <list> AS x) gives one row per item of the list, which it reads on the row before the FOR; the item x and, with WITH ORDINALITY i (from 1) or WITH OFFSET i (from 0), its position i are two new variables with names of their own; ...
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
Help: The gql() query built a value larger than 'traversal.max_materialized_bytes' allows (1073741824 bytes); ... A function whose result can be larger than its arguments (range(), split(), replace(), || and CAST(x AS STRING) of a list or map) and the list of collect_list() are checked before they are built, so the query stops instead of running out of memory; the result is never cut short. Build smaller values: bound range() and replace() by the data, as in g.gql("MATCH (p:person) RETURN range(1, p.age / 10) AS r"), ... or raise the limit for the query, g.with("traversal.max_materialized_bytes", 2147483648).gql(..), ...
```

## Subqueries

The errors of an `EXISTS` subquery or a pattern predicate share one help text that explains
subqueries: TRUE or FALSE, never `NULL`; the body reads the variables bound before it, and the
variables it binds stay inside it; a pattern predicate binds no new variable. Before it, each
names its own fix:

```rhai,graph=modern
g.gql("MATCH (p:person) WHERE (p)-[:knows]->(q) RETURN p.name AS name")   // error: the pattern predicate introduces the new variable `q`
g.gql("MATCH (p:person) RETURN p.name AS name, (p)-[:knows]->() AS k")   // error: the pattern stands where a value is expected (42001)
g.gql("MATCH (p:person) WHERE EXISTS { MATCH (p)-[:created]->(s) } RETURN s.name AS s")   // error: the variable `s` is not in scope: it is bound inside a subquery
g.gql("MATCH (a:person) WHERE EXISTS { MATCH (a)-->() WHERE b.age > 1 MATCH (b) } RETURN a")   // error: `b` is bound by a later MATCH clause of the subquery
```

```text
Help: write it as EXISTS { MATCH (p)-[:knows]->(q) }, whose MATCH may bind new variables, or leave `q` out of the pattern. A subquery, EXISTS { MATCH <pattern> [WHERE <condition>] }, EXISTS { <pattern> } or a Cypher pattern predicate such as (p)-[:created]->(), is TRUE when its body has a match and FALSE otherwise, never NULL; its body reads the variables bound before it, and the variables it binds itself stay inside it. A pattern predicate binds no new variable: name new elements in an EXISTS { MATCH .. }, as in g.gql("MATCH (p:person) WHERE NOT EXISTS { MATCH (p)-[:created]->(s) WHERE s.lang = 'java' } RETURN p.name"), and bind an element in the MATCH clause of the query to read it after the subquery, as in g.gql("MATCH (p:person)-[:created]->(s) RETURN p.name, s.name").
Help: write EXISTS { (p)-[:knows]->() } for whether it has a match, COUNT { (p)-[:knows]->() } for the number of matches, or a pattern comprehension such as [(n)-->(m) | m] for the list of what it matches, as in g.gql("MATCH (p:person) RETURN p.name, COUNT { (p)-[:knows]->() } AS friends, [(p)-[:created]->(s) | s.name] AS made"). A subquery, ...
Help: bind `s` in the MATCH clause of the query to use it here, or test it inside the subquery. A subquery, ...
Help: move the condition to the WHERE of that later MATCH clause inside the subquery. A subquery, ...
```

A query holds at most 64 subqueries (`EXISTS`, pattern predicates, pattern comprehensions,
`COUNT { }` and `COLLECT { }`, nested ones included) and runs at most 1024 subquery bodies, where
a `LET` or `WITH` variable holding a subquery counts once for every place that reads it. A
`CALL { }` body counts toward neither; `CALL` bodies and whole-query subquery bodies nest at most
16 levels ([UNION, NEXT and CALL](#union-next-and-call)). Either limit is `54001` before the run,
and its help names both bounds and the forms that need fewer subqueries:

```text
Error: GQL limit exceeded at line 1, column 785: the query holds more than 64 subqueries (EXISTS and pattern predicates), pattern comprehensions included, as are COUNT { } and COLLECT { } ...
Help: join the patterns into fewer subqueries (one EXISTS { MATCH .. } may hold several MATCH clauses and patterns), or add the patterns to the MATCH clause of the query; a pattern comprehension is a subquery too: collect the matches of several patterns with OPTIONAL MATCH and collect_list() instead. The query is larger than gql() accepts: one query holds at most 64 subqueries (EXISTS, pattern predicates, pattern comprehensions, COUNT { } and COLLECT { }, nested ones included) and runs at most 1024 subquery bodies, where a LET or WITH variable holding a subquery counts once for every place that reads it; a CALL { } body counts toward neither (CALL bodies nest at most 16 levels). One EXISTS may hold several MATCH clauses and patterns, as in g.gql("MATCH (p:person) WHERE EXISTS { MATCH (p)-[:knows]->(f) MATCH (f)-[:created]->(s) } RETURN p.name"), and one CALL may compute what several COUNT { } and COLLECT { } over the same pattern do, as in g.gql("MATCH (p:person) CALL (p) { OPTIONAL MATCH (p)-[:created]->(s) RETURN count(s) AS n, collect_list(s.name) AS names } RETURN p.name, n, names").
```

A quantified edge inside a subquery that reaches `repeat.max_loops` or
`traversal.max_traversers` fails like one outside, and the help says where it stands and why a
subquery may loop long:

```rhai,graph=modern
g.with("repeat.max_loops", 1).gql("MATCH (p:person) WHERE EXISTS { (p)-[:knows|created]->{1,}(x) } RETURN p.name AS n")   // error: repeat() exceeded the loop limit of 1 iterations
```

```text
Help: The loop of the quantified edge -[:knows|created]->{1,} inside an EXISTS subquery (or pattern predicate) of the gql() query ran 1 iterations, the limit of repeat.max_loops. ... A subquery stops at its first match, so its loop runs this long on a row that has no match, or only a late one. Give the quantifier an upper bound, as in -[:knows|created]->{1,5}, ...
```

`COUNT { }` and `COLLECT { }` follow the same rules (a variable bound inside one is named as
bound inside that `COUNT { }` or `COLLECT { }`, and the help explains the two forms); a
`COLLECT { }` body must end in a `RETURN` of exactly one column. Cypher 4's `exists(n.p)` is [refused](refused.md):

```rhai,graph=modern
g.gql("MATCH (p:person) RETURN COLLECT { MATCH (p)-[:knows]->(f) RETURN f.name, f.age } AS l")   // error: COLLECT { } needs a body that ends in a RETURN of exactly one column
g.gql("MATCH (p:person) RETURN COUNT { MATCH (p)-[:knows]->(f) } AS n, f.name AS x")   // error: the variable `f` is not in scope: it is bound inside a COUNT { } subquery
g.gql("MATCH (p:person) RETURN exists(p.age) AS a")   // refused: exists(value), the Cypher 4 property test, is not supported
```

```text
Help: return the one value to collect, as in MATCH (p:person) RETURN COLLECT { MATCH (p)-[:created]->(s) RETURN s.name } AS names. A subquery, ...
Help: bind `f` in the MATCH clause of the query to use it here, or test it inside the COUNT { } body. COUNT { <body> } is the number of matches of its body (or of the records its RETURN gives), and COLLECT { <body> RETURN <expression> } the list of the values it returns; the body reads the variables bound before it, and the variables it binds itself stay inside it, ...
Help: test a property with n.p IS NOT NULL, here p.age IS NOT NULL; EXISTS tests a graph pattern, as in MATCH (p:person) WHERE EXISTS { (p)-[:created]->() } RETURN p.name. ...
```

### Map projections and comprehensions

A pattern comprehension is a subquery, so its errors follow the rules above with their own help
text: the variables it binds stay inside it (read them in its `|` part), and its `|` part holds
no aggregate (aggregate the list, `size([..])`, or use `OPTIONAL MATCH` and an aggregate). A
list-comprehension variable must be a new name, and its `WHERE` and `|` parts hold no aggregate
either. Each help names the form that works:

```rhai,graph=modern
g.gql("MATCH (p:person) RETURN p.name AS n, [(p)-[:knows]->(f) | f.name] AS l ORDER BY f.name")   // error: `f` is bound inside a pattern comprehension
g.gql("MATCH (p:person) RETURN [(p)-->(f) | count(f)] AS l")   // error: count() cannot be used inside the | part of a pattern comprehension
g.gql("MATCH (p:person) RETURN [p IN [1, 2] | p * 2] AS l")   // error: the comprehension variable `p` shadows the variable `p` in scope
g.gql("MATCH (p:person) RETURN [x IN [1, 2] | count(x)] AS l")   // error: count() cannot be used inside the WHERE or | part of a list comprehension
g.gql("MATCH (p:person) RETURN [x IN [1, 2] WHERE x:A|B | x] AS a")   // error: unexpected `|`
```

```text
Help: bind `f` in the MATCH clause of the query to use it here, or read it in the | part of the comprehension, as in [(a)-->(f) | f.name]. A pattern comprehension, [(a)-[:knows]->(f) WHERE <condition> | <expression>], is the list of its | part evaluated for every match of its pattern (the empty list when nothing matches); its pattern reads the variables bound before it, and the variables it binds itself stay inside it. ...
Help: aggregate the list the comprehension builds, as in MATCH (p) RETURN size([(p)-->(f) | f.age]) AS n, or aggregate in a WITH after an OPTIONAL MATCH, as in MATCH (p) OPTIONAL MATCH (p)-->(f) RETURN p, count(f.age) AS n. ...
Help: give the comprehension its own variable, as in [p2 IN list WHERE p2 > 0 | p2]. ...
Help: aggregate in a WITH first and use its column in the comprehension, as in MATCH (p)-->(f) WITH p, count(f.age) AS n RETURN [x IN [1, 2, 3] WHERE x < n]. ...
Help: a `|` in a comprehension ends its list or its WHERE condition; write a label alternative there in parentheses, as in [x IN l WHERE x:(A|B) | x.name]. ...
```

A map projection of a value that is not an element or a map, and a list comprehension over a
value that is not a list, are data errors (`22G03`) of the row; so is `all()`, `any()`,
`none()`, `single()` or `reduce()` over a value that is not a list, and the message names the
function. Their variables are new names like a comprehension's (`42002`), and an aggregate
inside their condition or `|` part is `42002` with a `WITH` example of the same form. A
list-comprehension variable read inside an `EXISTS` or a pattern comprehension of its own
`WHERE` or `|` part is not supported yet (`0A000`), the variables of the list predicates and
`reduce()` neither:

```rhai,graph=modern
g.gql("WITH 1 AS m RETURN m {.a} AS x")   // error: a map projection reads a node, an edge or a map, not a number
g.gql("MATCH (p:person) RETURN [x IN [p] WHERE (x)-->() | x.name] AS l")   // error: reading the comprehension variable `x` inside a subquery is not yet supported
g.gql("RETURN any(x IN 5 WHERE x > 1) AS a")   // error: any() iterates over a list, not a number (22G03)
g.gql("MATCH (p:person) RETURN reduce(p = 0, x IN [1] | p + x) AS a")   // error: the accumulator `p` of reduce() shadows the variable `p` in scope
```

```text
Help: test `x` outside the subquery, as in [x IN list WHERE x > 0 AND EXISTS { MATCH (n)-->() }]. ...
Help: give reduce() an accumulator of its own, as in reduce(p2 = 0, item IN list | p2 + item). ... The same holds for the variable of all(), any(), none() and single() and for both variables of reduce(), as in g.gql("MATCH (p:person) RETURN p.name, all(a IN [p.age] WHERE a > 30) AS old, reduce(s = 0, a IN [p.age, 1] | s + a) AS total").
```

A resource limit reached inside a pattern comprehension names it. A quantified edge in its
pattern loops until the row's last match is found (unlike `EXISTS`, which stops at its first),
and its list is checked against `traversal.max_materialized_bytes` and the memory budget before
it is built, like `collect_list()`:

```rhai,graph=modern
g.with("repeat.max_loops", 1).gql("MATCH (p:person) RETURN p.name AS n, [(p)-[:knows|created]->{1,}(x) | x.name] AS r")   // error: repeat() exceeded the loop limit of 1 iterations
g.with("traversal.max_materialized_bytes", 100).gql("MATCH (p:person) RETURN [(p)-[:created]->(s) | s.name] AS r")   // error: Resource limit 'traversal.max_materialized_bytes' exceeded
```

```text
Help: The loop of the quantified edge -[:knows|created]->{1,} inside a pattern comprehension [(a)-->(b) | ..] of the gql() query ran 1 iterations, the limit of repeat.max_loops. ... A pattern comprehension collects every match of its pattern for each row, so its loop runs until the row's last match is found. Give the quantifier an upper bound, as in -[:knows|created]->{1,5}, ...
Help: ... A function whose result can be larger than its arguments (range(), split(), replace(), || and CAST(x AS STRING) of a list or map) and the list of collect_list() are checked before they are built, as is the list of a pattern comprehension [(a)-->(b) | b.name], so the query stops instead of running out of memory; the result is never cut short. ... count the values instead of collecting them, ... or, for the matches of a pattern, g.gql("MATCH (p:person) OPTIONAL MATCH (p)-->(f) RETURN p.name, count(f) AS n"), ...
```

## UNION, NEXT and CALL

The errors of a composite query share one help text that explains `UNION` and `NEXT`: the
operands return the same column names in any order (the result keeps the first operand's order),
`UNION` removes duplicates and `UNION ALL` keeps them, one chain uses one kind, each operand has
its own variables, `ORDER BY` and `LIMIT`, and `NEXT` passes on only the `RETURN` columns. The
column mismatch names both column sets:

```rhai,graph=modern
g.gql("RETURN 1 AS x UNION RETURN 2 AS y")   // error: the operands of UNION must return the same column names
g.gql("RETURN 1 AS x UNION RETURN 2 AS x UNION ALL RETURN 3 AS x")   // error: UNION ALL is mixed with UNION in one query
g.gql("MATCH (p:person) RETURN p.name AS name NEXT RETURN p.age")   // error: the variable `p` is not in scope: the RETURN before NEXT does not return it
g.gql("MATCH (p:person) RETURN p MATCH (p)-[:created]->(s) RETURN s")   // error: MATCH after RETURN: RETURN ends the query
```

```text
Error: GQL semantic error at line 1, column 21: the operands of UNION must return the same column names: the first returns `x`, this one returns `y` (`x` is missing) ...
Help: name the columns alike with AS in every operand (their order may differ), as in MATCH (p:person) RETURN p.name AS name UNION MATCH (s:software) RETURN s.name AS name. UNION combines the records of queries that return the same column names, in any order (the result keeps the order of the first query): UNION (or UNION DISTINCT) removes duplicate records, UNION ALL keeps them, and one chain of UNIONs uses one kind. Each query of a UNION has its own variables, ORDER BY, OFFSET and LIMIT. NEXT passes the records of the statement before it on by their column names (only those), so it sorts, filters or aggregates a UNION as a whole, as in g.gql("MATCH (p:person) RETURN p.name AS name UNION MATCH (s:software) RETURN s.name AS name NEXT RETURN name ORDER BY name LIMIT 3").
Help: write the same kind between every operand, or combine the parts with NEXT, as in RETURN 1 AS x UNION ALL RETURN 1 AS x NEXT RETURN x UNION RETURN 2 AS x. UNION combines ...
Help: add `p` to the RETURN items before NEXT, as in RETURN ..., p NEXT ... UNION combines ...
Help: put every MATCH before the RETURN, or continue with NEXT, which passes the columns of the RETURN on to the next statement, as in MATCH (p:person) RETURN p NEXT MATCH (p)-[:created]->(s) RETURN p.name, s.name, or chain Gremlin steps after gql(). ...
```

The errors of a `CALL` share a help text on its scope: the body reads only the variables its
scope clause names (`CALL (*)` all, `CALL { }` none, Cypher's `CALL { WITH a .. }` those of its
first `WITH`), returns new names, aggregates per row, and drops a row without records unless the
`CALL` is `OPTIONAL`. Cypher's importing `WITH` lists plain variables only, as in Neo4j: a first
`WITH` with a `WHERE`, an alias, an expression, `DISTINCT`, `ORDER BY`, `OFFSET` or `LIMIT` is
no importing `WITH`, so the body imports nothing and the error says so. A body without `RETURN` (a unit subquery, which only a write could use) is
not supported, and `IN TRANSACTIONS` is [refused](refused.md):

```rhai,graph=modern
g.gql("MATCH (p:person) CALL (p) { MATCH (p)-[:knows]->(f) RETURN f AS p } RETURN p")   // error: the CALL body returns `p`, which is already defined outside it
g.gql("MATCH (p:person) CALL { WITH p AS q MATCH (q)-->(s) RETURN s.name AS sw } RETURN sw")   // error: the variable `p` is not imported into the CALL body: its first WITH is not an importing WITH
g.gql("MATCH (p:person) CALL { WITH p WHERE p.age > 30 MATCH (p)-->(s) RETURN s.name AS sw } RETURN sw")   // error: the variable `p` is not imported into the CALL body: its first WITH is not an importing WITH
g.gql("MATCH (p:person) CALL (p) { MATCH (p)-[:created]->(s) } RETURN p")   // error: a CALL { } body without RETURN (a unit subquery) only makes sense for writes, and this body writes nothing
g.gql("MATCH (p:person) CALL (p) { MATCH (p)-->(s) RETURN s.name AS sw } IN TRANSACTIONS RETURN sw")   // refused: CALL { } IN TRANSACTIONS commits writes in batches
```

```text
Help: rename the column inside the body, as in RETURN ... AS p2. A CALL subquery, CALL (a, b) { <query ending in RETURN, or in a write (INSERT, SET, REMOVE, DELETE, MERGE / FINISH)> }, runs its query once for each incoming row: the body reads only the variables its scope clause names (CALL (*) imports all of them, CALL { } none, and Cypher's CALL { WITH a .. } those of its first WITH), and passes on only the columns of its RETURN, which must be new names; its aggregates, ORDER BY and LIMIT apply to each row's records, as in g.gql("MATCH (p:person) CALL (p) { MATCH (p)-[:created]->(s) RETURN count(s) AS made } RETURN p.name, made"). A row whose body returns no record is dropped; OPTIONAL CALL keeps it with NULL columns.
Help: import `p` with a variable scope clause, as in CALL (p) { MATCH ... WHERE ... }, or in Cypher with a WITH of plain variables first and the rest of the body after it, as in CALL { WITH p MATCH ... WHERE ... }. A CALL subquery, ...
Help: end the body with RETURN and the columns the query reads after it, for example MATCH (p:person) CALL (p) { MATCH (p)-[:created]->(s) RETURN count(s) AS made } RETURN p.name, made. A CALL subquery, ...
Help: remove IN TRANSACTIONS: the whole query commits at once, and a query that fails changes nothing. A CALL subquery, ...
```

`CALL` bodies and whole-query subquery bodies (`EXISTS`, `COUNT` or `COLLECT` with `WITH`,
`RETURN`, ...) nest at most 16 levels (`54001`); the help shows how to un-nest them. A resource
limit reached inside a `CALL` body names it: the body runs for each incoming row and keeps every
record, so a quantified edge in it loops until the row's last match:

```rhai,graph=modern
g.with("repeat.max_loops", 1).gql("MATCH (p:person) CALL (p) { MATCH (p)-[:knows|created]->{1,}(x) RETURN count(x) AS n } RETURN p.name AS who, n")   // error: repeat() exceeded the loop limit of 1 iterations
```

```text
Error: GQL limit exceeded at line 1, column 113: the query nests CALL { } and subquery bodies deeper than 16 levels ...
Help: un-nest the bodies: a CALL body may follow another in the same query, as in CALL { .. RETURN a } CALL (a) { .. RETURN b }, and NEXT passes the result of one query to the next. The query is larger than gql() accepts.

Help: The loop of the quantified edge -[:knows|created]->{1,} inside a CALL { } subquery of the gql() query ran 1 iterations, the limit of repeat.max_loops. ... A CALL { } subquery runs its body for each incoming row and keeps every record it returns, so its loop runs until the row's last match is found. Give the quantifier an upper bound, as in -[:knows|created]->{1,5}, ...
```

A limit inside an operand of a `UNION` or a block after `NEXT` at the top level keeps the plain
text of a top-level clause; inside a subquery the place named is the innermost subquery
(`CALL`, `COUNT`, `COLLECT`, `EXISTS` or a pattern comprehension).

## Quantified path patterns

The forms a [quantified path pattern](subset.md#quantified-path-patterns) does not take yet are
refused before the run with `0A000`; each help names the rewrite. A body variable is a new name
(`42002`), and outside a quantified body it is a list, so a property of it is an error whose help
offers the body's `WHERE` or a list comprehension:

```rhai,graph=modern
g.gql("MATCH (a) ((x)-[:knows]->{1,2}(y)){1,3} (b) RETURN b")   // error: a quantified edge inside a parenthesized path pattern is not yet supported
g.gql("MATCH (a) ((x)-->(y) WHERE y.age > a.age){1,3} (b) RETURN b")   // error: a condition inside a parenthesized path pattern that reads a variable declared outside it is not yet supported
g.gql("MATCH (a) ((a)-->(y)){1,2} (b) RETURN b")   // error: `a` is already bound; a variable of a quantified path pattern must be a new name
g.gql("MATCH (a) ((x)-->(y)){1,3} (b) RETURN x.name")   // error: `x` is a group variable of the quantified path pattern
```

```text
Help: write the edge out as often as needed inside the parentheses, as in ((x)-->()-->(y)){1,3}, or quantify the edge outside them, as in MATCH (a)-[:knows]->{1,3}(b) RETURN b. ...
Help: compare the body's variables with each other, with values or with parameters inside it, as in ((x)-[e]->(y) WHERE e.weight > $w){1,3}, and test the outer variable in the WHERE of the MATCH clause. ...
Help: give the element inside its own variable: the node before a parenthesized path pattern is the first node of its first iteration and the node after it the last node of its last iteration, as in (a) ((x)-->(y)){1,3} (b). ...
Help: test each element inside the parenthesized path pattern, with WHERE x.name ... before its closing parenthesis, or read the list, as in [v IN x | v.name]. ...
```

An unbounded quantified path pattern that reaches `repeat.max_loops` or
`traversal.max_traversers` fails like a quantified edge (`RepeatLimitExceeded`,
`ResourceLimitExceeded`, no GQLSTATUS), never with fewer rows, and its help names **the pattern
as the plan walks it**: read back from the lowered `repeat()` body, so it shows each node's
labels but not its variable (a node without a label is `()`), each edge with its labels in the
direction of the walk (the plan may walk the pattern from its far end), and a body of one edge
with nothing on its nodes as the quantified edge it equals (`-[:knows]->{1,}`):

```rhai,graph=modern
g.with("repeat.max_loops", 1).gql("MATCH (a {name: 'marko'}) ((x:person)-[:knows]->(m)-[:created]->(y))+ (b) RETURN b.name AS b")   // error: repeat() exceeded the loop limit of 1 iterations
```

```text
Help: The loop of the quantified path pattern ((:person)-[:knows]->()-[:created]->()){1,} of the gql() query ran 1 iterations, the limit of repeat.max_loops. A quantifier without an upper bound ({m,}, *, + or Cypher [*]) repeats the pattern until every path has used all the edges it can reach (trail), ... Give the quantifier an upper bound, as in ((:person)-[:knows]->()-[:created]->()){1,5}, ... Or raise the limit for the query, g.with("repeat.max_loops", 2).gql(..), ...
```

A `MATCH` clause holds at most 64 edge patterns; an edge in a quantified body counts once,
however often the body repeats, so the help of that limit names a quantified path pattern as one
of the fixes:

```text
Help: split the pattern into several MATCH clauses that share a variable, for example MATCH (a)-->(b)-->(c) MATCH (c)-->(d)-->(e) RETURN e (edge uniqueness then holds within each clause only), repeat a chain as a quantified path pattern, whose body edges count once however often it repeats, for example MATCH (a) (()-->()){65} (z) RETURN z, or walk a long path with Gremlin repeat(), for example g.v().repeat(__.out()).times(100). The query is larger than gql() accepts.
```

## INSERT

An `INSERT` pattern that cannot create what it says fails before the run; its help shows the
accepted form and ends with one shared paragraph on `INSERT`:

```rhai,graph=modern
g.gql("INSERT (a:x)-[:r]-(b:y)")   // error: INSERT needs a direction; write -[e:T]-> or <-[e:T]-
g.gql("INSERT (:a)-[:r|s]->(:b)")   // error: an edge in INSERT has exactly one label
g.gql("MATCH (p:person) INSERT (p:admin)")   // error: the variable `p` is already bound
g.gql("INSERT (:x {a: 1, a: 2})")   // error: the key `a` appears twice in this map
g.gql("MATCH (p:person) INSERT (:x {n: count(p)})")   // error: the aggregate function count() cannot be used in INSERT
g.gql("MATCH (p:person) WHERE EXISTS { INSERT (:a) } RETURN p")   // error: INSERT inside EXISTS { }: a subquery expression or a comprehension cannot write
```

```text
Error: GQL syntax error at line 1, column 13: INSERT needs a direction; write -[e:T]-> or <-[e:T]- (step #1 'gql:error("42001")' refused the query before the traversal ran)
  at #1: v().gql:error("42001")
             ^^^^^^^^^^^^^^^^^^
  query:     INSERT (a:x)-[:r]-(b:y)
                         ^^^^^^
  GQLSTATUS: 42001
Help: give the new edge its direction, from the node before it (-[:r]->) or to it (<-[:r]-); for example g.gql("INSERT (a:person {name: 'ann'})-[:knows]->(b:person:admin)"). INSERT (Cypher CREATE) creates its pattern once for each row: a new node with the labels written, joined by & or :, and a property map; a new edge with a direction and exactly one label; and a variable bound before, written alone as (p), connects the existing node, as in g.gql("MATCH (p:person {name: 'marko'}) INSERT (p)-[:likes]->(f:fruit&food {name: 'apple'}) RETURN f.name"). A query may end with the INSERT (it then returns no rows), and FOR gives it one row per item, as in g.gql("FOR n IN ['ann', 'bob'] INSERT (:person {name: n})"). To create a pattern only where it is missing, MERGE finds it or creates it, as in g.gql("FOR n IN ['ann', 'marko'] MERGE (:person {name: n})").
```

While the query runs, a row whose endpoint is `NULL` (`22004`) or no node (`22G03`), or whose
property value is a node, an edge or a path (`22G03`), fails the query, and it changes nothing:

```rhai,graph=modern
g.gql("OPTIONAL MATCH (p:person {name: 'nobody'}) INSERT (p)-[:knows]->(:person)")   // error: the source node `p` of the inserted `:knows` edge is NULL
g.gql("MATCH (p:person) INSERT (:x {v: p})")   // error: the value of property `v` is a node
```

```text
Caused by: GQL data exception at line 1, column 51: the source node `p` of the inserted `:knows` edge is NULL
  query:     …person {name: 'nobody'}) INSERT (p)-[:knows]->(:person)
                                              ^^^
  GQLSTATUS: 22004
Help: keep only the rows where it is bound before the INSERT: FILTER p IS NOT NULL. A row of the data gave the marked endpoint of the new edge no node: an edge connects two nodes, so a variable that OPTIONAL MATCH may leave NULL is filtered first, as in g.gql("MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) FILTER s IS NOT NULL INSERT (p)-[:made]->(s)"), and a value that is no node is turned into one by a MATCH, as in g.gql("FOR n IN ['lop'] MATCH (s:software {name: n}) INSERT (:review {stars: 5})-[:about]->(s)").
```

A schema violation of an `INSERT` is the schema's error, with a help line written for GQL: a
missing `required` property names the `INSERT` map and `coalesce()`, a type mismatch the
conversion function (`toInteger()`, ...) with a `$parameter` example
([Writing with GQL](writes.md#schema)). A bound node written with labels or properties
(`MATCH (p:person) INSERT (p:admin)`) gets the `SET` that changes it, built from the query, as in
`g.gql("MATCH (p:person) SET p:admin")`, beside the Gremlin steps. The write clause that is not supported yet
(`FOREACH`) is `0A000` with a help line naming the GQL form:

```rhai,graph=modern
g.gql("MATCH (p:person) FOREACH (x IN [1] | SET p.k = x)")   // error: FOREACH is not yet supported in this version
```

```text
Help: write FOR x IN <list> followed by the write clause, for example FOR name IN ['ann', 'bob'] INSERT (:person {name: name}). DELETE takes a list itself, as in MATCH p = (:person)-[:knows]->() DETACH DELETE nodes(p). This construct comes in a later version of gql(); until then, write the query without it or as Gremlin steps.
```

## SET and REMOVE

A `SET` / `REMOVE` item of the wrong form fails before the run: a label item joined by `|`, `!` or
`%` (`42001`), a label item on a variable known to be an edge, an aggregate in a value, or a
target that is undefined, a `LET` / `WITH` value, a path, a group variable or a `FOR` position
(`42002`); a deeper target such as `p.a.b` or `head(l).k` is not supported yet (`0A000`). Each
help names the accepted form and ends with one shared paragraph on `SET` / `REMOVE`:

```rhai,graph=modern
g.gql("MATCH (p:person) SET p:A|B")   // error: `|` (a label alternative) cannot stand in SET
g.gql("MATCH (a)-[e:knows]->(b) SET e:met")   // error: `e` is an edge: an edge has exactly one label
g.gql("MATCH (p) SET p.k = count(*)")   // error: the aggregate function count() cannot be used in SET
g.gql("MATCH (p) SET p.a.b = 1")   // error: `p.a.b` as a SET target is not yet supported in this version
```

```text
Error: GQL syntax error at line 1, column 24: `|` (a label alternative) cannot stand in SET: a label item names the labels, joined by `:` or `&` (step #1 'gql:error("42001")' refused the query before the traversal ran)
  at #1: v().gql:error("42001")
             ^^^^^^^^^^^^^^^^^^
  query:     MATCH (p:person) SET p:A|B
                                    ^^^
  GQLSTATUS: 42001
Help: write the labels, as in SET p:A:B. SET changes the nodes and edges of each row, item by item from left to right: n.k = <value> sets a property (a NULL value removes it), n = {map} replaces every property, n += {map} merges the map into them, and n:A:B adds labels to a node (an edge has exactly one label); REMOVE n.k removes a property and REMOVE n:A a label, as in g.gql("MATCH (p:person {name: 'marko'}) SET p.age = p.age + 1, p:employee REMOVE p.nick RETURN p.age"). A variable that OPTIONAL MATCH leaves NULL is skipped, and a query may end with the SET or REMOVE (it then returns no rows), as in g.gql("MATCH (s:software) SET s += {open: true}").
Help: change the type of the edge with the Gremlin step set_label(), as in g.gql("MATCH (a)-[e:knows]->(b) RETURN e").select("e").set_label("met"). In a MATCH clause a node variable names one node ...
```

While the query runs, a `NULL` right side of `n = ..` or `n += ..` (`22004`), a right side that
is no map, node or edge, a node, edge or path as a property value, a target that holds no element,
and a label item on a value that turns out to be an edge (`22G03`) fail the query, and it changes
nothing. The help rewrites the failing query where it can:

```rhai,graph=modern
g.gql("MATCH (p:person) SET p = NULL")   // error: the right side of SET p = .. is NULL, not a map
g.gql("MATCH (p:person) SET p.best = p")   // error: the value of property `best` is a node
g.gql("MATCH (p:person) SET p = 1")   // error: the right side of SET p = .. is a single value
g.gql("FOR x IN [1] SET x.k = 1")   // error: SET changes a node or an edge, but `x` holds a value of type 'int64'
```

```text
Caused by: GQL data exception at line 1, column 26: the right side of SET p = .. is NULL, not a map
  query:     MATCH (p:person) SET p = NULL
                                      ^^^^
  GQLSTATUS: 22004
Help: SET p = {} removes every property; to keep them when the map is NULL, give it the element's own properties as a default with coalesce($p, properties(p)), as in g.gql("MATCH (p:person) SET p = coalesce(NULL, properties(p))"). A row of the data gave the marked SET or REMOVE item a value it cannot write; keep such rows away from it with FILTER or WHERE, as in g.gql("MATCH (p:person) FILTER p.age IS NOT NULL SET p.next = p.age + 1"). SET changes the nodes and edges of each row, ...
Help: store a value of it instead, such as its id, as in g.gql("MATCH (p:person) SET p.best = element_id(p)"), or its properties with properties(..). A row of the data gave the marked SET or REMOVE item a value it cannot write; ...
```

A schema violation of a `SET` / `REMOVE` gets a help line written for GQL too: a type mismatch
names the conversion function in a `SET` of the label and key, a removed `required` property
(`REMOVE`, a `NULL` value, `SET n = {map}` without the key) the map that keeps it and `+=`, and
a label whose `required` property the node lacks the `SET` that writes the key before the label.

## DELETE

A `DELETE` target that cannot hold an element fails before the run: a label test such as
`n:person` or `n IS person` (`42001`: it is a boolean, and `DELETE` removes whole elements), a
literal, a computed value, a map or a `FOR` position (`42002`). `DROP n`, the Gremlin step
written as a clause, is a syntax error naming `DETACH DELETE`, and `REMOVE n` without an item
names it too. Each help rewrites the failing query and ends with one shared paragraph on
`DELETE`:

```rhai,graph=modern
g.gql("MATCH (n:person {name: 'vadas'}) DELETE n:person")   // error: `n:person` is a label test (a boolean), not an element
g.gql("MATCH (n:person) DELETE 1 + 1")   // error: `1 + 1` is a computed value, not a node, an edge, a path or a list of them
g.gql("MATCH (p:person) DROP p")   // error: GQL has no DROP clause: DELETE removes nodes and edges
```

```text
Error: GQL syntax error at line 1, column 41: `n:person` is a label test (a boolean), not an element; DELETE removes whole nodes and edges (step #1 'gql:error("42001")' refused the query before the traversal ran)
  at #1: v().gql:error("42001")
             ^^^^^^^^^^^^^^^^^^
  query:     MATCH (n:person {name: 'vadas'}) DELETE n:person
                                                     ^^^^^^^^
  GQLSTATUS: 42001
Help: delete the element itself with DELETE n, as in g.gql("MATCH (n:person {name: 'vadas'}) DELETE n"), or remove the label and keep the element with REMOVE n:person, as in g.gql("MATCH (n:person {name: 'vadas'}) REMOVE n:person"). DELETE removes, for each row, the nodes and edges its targets give: a node or edge variable, a path (all its nodes and edges) or a list of them; a NULL target is skipped, and an element given twice is removed once. A node that still has edges is removed only together with them: DETACH DELETE removes its edges too, as in g.gql("MATCH (s:software {name: 'lop'}) DETACH DELETE s"). The rest of the query can no longer read what it deleted, so read what it needs first, as in g.gql("MATCH (p:person {name: 'vadas'}) WITH p, p.name AS name DETACH DELETE p RETURN name"); a query may end with the DELETE (it then returns no rows).
Help: name the element to remove by its variable, as in g.gql("MATCH (n:person) DETACH DELETE n"). DELETE removes, for each row, ...
Help: write DETACH DELETE (the GQL form of the Gremlin step drop()), as in g.gql("MATCH (p:person) DETACH DELETE p"). DELETE removes, for each row, ...
```

While the query runs, a plain (or `NODETACH`) `DELETE` of a node that still has an edge the same
clause does not delete fails with `G1001`; the help names `DETACH DELETE` in the failing query
and the form that deletes the edges in the same clause. A target that turns out to hold no
element (a `WITH` value, a property) is `22G03`. Either failure changes nothing:

```rhai,graph=modern
g.gql("MATCH (n:person {name: 'marko'}) DELETE n")   // error: `n` is a node that still has edges
g.gql("MATCH (n:person) WITH 1 AS x DELETE x")   // error: DELETE removes nodes, edges and paths, but `1` gives a number on this row
```

```text
Caused by: GQL data exception at line 1, column 41: `n` is a node that still has edges, and DELETE removes only nodes without edges (besides the edges deleted in the same clause)
  query:     MATCH (n:person {name: 'marko'}) DELETE n
                                                     ^
  GQLSTATUS: G1001
Help: remove the node together with its edges with DETACH DELETE, as in g.gql("MATCH (n:person {name: 'marko'}) DETACH DELETE n"), or delete its edges in the same clause, as in g.gql("MATCH (s:software {name: 'ripple'})<-[r]-() DELETE r, s").
Help: give DELETE a node, an edge, a path or a list of them, and keep the rows where the target holds something else away from it with FILTER or WHERE, as in g.gql("MATCH (n:person {name: 'marko'}) DETACH DELETE n"). DELETE removes, for each row, ...
```

After a `DELETE`, a later part of the query that reads a deleted element (a property,
`labels()`, `element_id()`, a `SET`, `REMOVE` or `INSERT` on it) or returns it fails, and the
query changes nothing. The error is the storage's (`ElementRemoved`), with a help line written for
GQL: read what the query needs before the `DELETE`, for example with `properties(n)`.
`count(n)`, `n IS NULL` and `type(r)` of a deleted edge still work, and a pattern from a deleted
node matches nothing:

```rhai,graph=modern
g.gql("MATCH (n:software) DETACH DELETE n RETURN n.name")   // error: the node was deleted earlier in this query
g.gql("MATCH ()-[r:created]->() DELETE r RETURN r.weight")   // error: the edge was deleted earlier in this query
```

```text
Caused by: The vertex was removed earlier in this query; its id, label and value are no longer available
Help: The node was deleted earlier in this query (by DELETE or DETACH DELETE, or together with a node a DETACH DELETE removed), but a later part of the query still reads it: a deleted node has no properties, labels or id any more and cannot be returned or changed, and a pattern from it matches nothing; only count(n) and n IS NULL still work. Read what you need before the DELETE and keep that instead, for example g.gql("MATCH (n:person {name: 'marko'}) WITH n, properties(n) AS p DETACH DELETE n RETURN p"), or run the DELETE as a separate query after reading.
Help: The edge was deleted earlier in this query (...); only count(r), r IS NULL and type(r) still work. Read what you need before the DELETE and keep that instead, for example g.gql("MATCH (:person)-[r:knows]->() WITH r, properties(r) AS p DELETE r RETURN p"), or for a node g.gql("MATCH (n:person {name: 'marko'}) WITH n, properties(n) AS p DETACH DELETE n RETURN p"), or run the DELETE as a separate query after reading.
```

## MERGE

A `MERGE` pattern that cannot be matched or created as written fails before the run: a second
pattern after `,`, an edge without its one label or with a label expression, a quantifier
(`42001`); a node bound before written with labels or properties, a bound edge variable, and a
pattern that is only a node bound before (`MATCH (a) MERGE (a)`: nothing to find or create)
(`42002`). Each help rewrites the failing query where it can and ends with one shared paragraph
on `MERGE`:

```rhai,graph=modern
g.gql("MERGE (a:x), (b:y)")   // error: MERGE takes exactly one pattern
g.gql("MATCH (a) MERGE (a)")   // error: MERGE (a) with `a` bound earlier has nothing to find or create
g.gql("MATCH (a:person) MERGE (a:admin)-[:r]->(:b)")   // error: the variable `a` is already bound
g.gql("MERGE (a:x)-[:r|s]->(b)")   // error: an edge in MERGE has exactly one label
```

```text
Error: GQL syntax error at line 1, column 12: MERGE takes exactly one pattern: a second pattern after `,` cannot stand here (step #1 'gql:error("42001")' refused the query before the traversal ran)
  at #1: v().gql:error("42001")
             ^^^^^^^^^^^^^^^^^^
  query:     MERGE (a:x), (b:y)
                        ^
  GQLSTATUS: 42001
Help: write one MERGE for each pattern, as in g.gql("MERGE (a:x) MERGE (b:y)"), or connect the patterns with an edge into one. MERGE looks for its whole pattern for each row: every match is a row, and when nothing matches the whole pattern is created, reusing the nodes bound before. Its one pattern has nodes with the labels written, joined by & or :, and a property map, edges with exactly one label (an edge without a direction matches either direction and is created from left to right), and a node bound before written alone, as (p); ON CREATE SET and ON MATCH SET items change what was created or matched, as in g.gql("MERGE (p:person {name: 'ann'}) ON CREATE SET p.new = true ON MATCH SET p.new = false RETURN p.new"). Each row sees what the rows before it created, so FOR creates a repeated item once, as in g.gql("FOR n IN ['ann', 'ann'] MERGE (:person {name: n})").
Help: to find or create a connection, give the pattern an edge, as in g.gql("MATCH (a) MERGE (a)-[:T]->(:Label)"); to find or create another node, give it its own name, as in g.gql("MATCH (a) MERGE (a2:Label)"). MERGE looks for its whole pattern for each row: ...
Help: write (a) to connect the node, or give a new node its own name, as in MERGE (a)-[:T]->(a2:Label {k: 1}); to change the bound node itself use SET on it, as in g.gql("MATCH (a:person) SET a:admin"), or the Gremlin steps add_label() and property() on it, as in g.gql("MATCH (a) RETURN a").select("a").add_label("admin"). MERGE looks for its whole pattern for each row: ...
Help: give the new edge one label, as in -[:r]->; for example g.gql("MERGE (a:person {name: 'ann'})-[:knows]->(b:person:admin)"). MERGE looks for its whole pattern for each row: ...
```

While the query runs, a property value of the pattern that is `NULL` (`22004`: MERGE can neither
match nor create `{k: NULL}`, checked for each row before the match), a node bound before that is
`NULL` (`22004`) or no node (`22G03`), and a node, edge or path as a property value (`22G03`) fail
the query, and it changes nothing, also what earlier rows created. The help names the `FILTER`
before the `MERGE` or the `coalesce()` default in the failing query:

```rhai,graph=modern
g.gql("FOR x IN [1, NULL] MERGE (:n {k: x})")   // error: MERGE cannot match or create `{k: NULL}`
g.gql("MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) MERGE (p)-[:made]->(s)")   // error: the destination node `s` of the merged `:made` edge is NULL
g.gql("MERGE (p:person {name: NULL})")   // error: MERGE cannot match or create `{name: NULL}`
```

```text
Caused by: GQL data exception at line 1, column 34: MERGE cannot match or create `{k: NULL}`: a property value is NULL
  query:     FOR x IN [1, NULL] MERGE (:n {k: x})
                                              ^
  GQLSTATUS: 22004
Help: keep only the rows where it has a value with FILTER x IS NOT NULL before the MERGE, or give it a default with coalesce(), as in g.gql("FOR x IN [1, NULL] MERGE (:n {k: coalesce(x, 0)})"). A row of the data gave the marked part of the MERGE pattern a value it can neither match nor create: a property value that is NULL or a graph element, or a node bound before that is NULL or no node. A variable that OPTIONAL MATCH may leave NULL is filtered first, and a value that is no node is turned into one by a MATCH, as in g.gql("FOR n IN ['lop'] MATCH (s:software {name: n}) MERGE (:review {stars: 5})-[:about]->(s)"). MERGE looks for its whole pattern for each row: ...
Help: keep only the rows where it is bound before the MERGE: FILTER s IS NOT NULL, as in g.gql("MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) FILTER s IS NOT NULL MERGE (p)-[:made]->(s)"). A row of the data gave the marked part of the MERGE pattern a value it can neither match nor create: ...
Help: MERGE matches and creates only property values that are not NULL: leave `name` out of the pattern, and find the elements that lack it with a MATCH, as in g.gql("MATCH (n) FILTER n.name IS NULL RETURN n"). A row of the data gave the marked part of the MERGE pattern a value it can neither match nor create: ...
```

A schema violation of a `MERGE` gets a help line written for GQL too. One raised by the create
names the `MERGE` pattern and keeps its node variable: a type mismatch the conversion function, as
in `g.gql("MERGE (p:person {w: toFloat($v)})", #{v: "1.5"})`, and a missing `required` property
the pattern's property map, because the `ON CREATE SET` items run after the create and cannot
give it (or a `"default"` in the schema, to match on other keys only). One raised by an
`ON CREATE SET` / `ON MATCH SET` item gets the `SET` fix in `MERGE` form, as in
`g.gql("MERGE (n:person) ON MATCH SET n.w = toFloat($v)", #{v: "1.5"})`.

## GQLSTATUS codes

| Code | Kind | When |
|---|---|---|
| `42001` | syntax | the query does not parse: an unexpected token, a missing `RETURN`, an unclosed bracket, a quantifier with `m > n`, `GROUP BY` after `WITH` (grouping is implicit) or after `ORDER BY`; an unknown function or `CAST` type name (with a suggestion), a function called with the wrong number of arguments, the SQL form `SUBSTRING(s FROM a FOR b)`; `WITH ORDINALITY` / `WITH OFFSET` after `UNWIND`; an `INSERT` edge without a direction or with a label expression, an `INSERT` node label other than a conjunction, `WHERE` or a quantifier in an `INSERT` pattern, a `SET` / `REMOVE` label item joined by `|`, `!` or `%`, a `DELETE` target written as a label test (`n:L`, `n IS L`), `DROP n`, a write inside `EXISTS` / `COUNT` / `COLLECT` or a comprehension, a write clause after `RETURN`; a second `MERGE` pattern after `,`, a `MERGE` edge without its one label or with a label expression, a quantifier in a `MERGE` pattern; a pattern predicate where a value is expected (a `RETURN` / `WITH` item, a function argument, a `LET` or `SET` value: write `EXISTS { }`, `COUNT { }` or a pattern comprehension) |
| `42002` | semantic | an undefined variable, a variable used as a node and an edge, an edge variable bound twice in one `MATCH`, a duplicate column name, a missing parameter, an `ORDER BY` key that `RETURN DISTINCT` dropped; a variable a `WITH` did not carry on, a `LET` name bound again; a variable read outside an aggregate that is not a grouping key, an aggregate in `WHERE` / `FILTER` / `LET` or inside another aggregate, `ORDER BY` after an aggregation reading something other than a column; a `GROUP BY` key that is not an item without an aggregate, or such an item missing from `GROUP BY`; a path or group variable bound twice; a variable of a quantified path pattern that is not a new name, or a property of one read outside its body; a `FOR` / `UNWIND` name already in scope, one name for the item and its position, the position used as a node or edge, an aggregate in the `FOR` list; a new variable in a pattern predicate, a variable bound inside a subquery (`EXISTS`, a pattern predicate, `COUNT { }`, `COLLECT { }`) read outside it, a condition in a subquery reading a variable of a later `MATCH` clause of the body, a subquery in an aggregating item reading a variable outside the aggregates that is not a grouping key; a variable bound inside a pattern comprehension read outside it, a comprehension variable named like another variable of the query, an aggregate inside a comprehension's `WHERE` or `\|` part; a `COLLECT { }` body that does not return exactly one column; `UNION` operands with different column names, `UNION` mixed with `UNION ALL` in one chain, a variable read after `NEXT` that the `RETURN` before it does not return; a `CALL` body returning a name already in scope, reading a variable it does not import (also through a first `WITH` that is no importing `WITH`), importing one twice; in `INSERT`, a bound variable with labels or properties, a bound edge variable, a duplicate key in a property map, an aggregate in a property value; in `SET` / `REMOVE`, an undefined target, a target that is a `LET` / `WITH` value, a path, a group variable or a `FOR` position, a label item on an edge variable, an aggregate in a value; in `DELETE`, a target known to hold no element (a literal, a computed value, a map, a `FOR` position), an aggregate in a target; in `MERGE`, a bound node with labels or properties, a bound edge variable, a pattern that is only a node bound before |
| `0A000` | refused / not yet | a [refused construct](refused.md) or one [not supported yet](subset.md#not-supported-yet) (also a function: `sin()`, `rand()`, `elements()`, `CAST` to `DATE`; Cypher's zoned `time()`, a temporal value below the microsecond, `X.realtime()`; `EXCEPT`, `INTERSECT`, `OTHERWISE`; a `CALL` body without `RETURN` that writes nothing, `CALL { } IN TRANSACTIONS`; Cypher 4's `exists(n.p)`; a `SET` / `REMOVE` target deeper than a variable (`p.a.b`, `head(l).k`), the write clause `FOREACH`, a subquery in a `MERGE` property value, an `INSERT` in a `UNION` operand or with a path variable; `LOAD CSV`, `CREATE INDEX` / `CONSTRAINT` / `GRAPH`; a list-comprehension or list-predicate variable read inside a subquery; inside a parenthesized path pattern: a nested group or quantified edge, a path variable, several patterns, no edge, a variable written twice or bound outside, a condition reading an outer variable, a subquery); also `gql()` after other steps |
| `54001` | limit | the query nests too deeply (more than 128 levels of parentheses, or an expression taller than 512) |
| `54001` | limit | more than 64 subqueries (`EXISTS`, pattern predicates, pattern comprehensions, `COUNT { }` and `COLLECT { }`, nested ones included), or more than 1024 subquery bodies to run (a `LET` / `WITH` variable holding a subquery counts once per reader); `CALL` bodies and whole-query subquery bodies nested more than 16 levels |
| `54001` | limit | a MATCH clause holds more than 64 edge patterns (over all its comma-separated paths; a quantified body's edges count once); split it into several MATCH clauses that share a variable, repeat a chain as a quantified path pattern, or use Gremlin `repeat()` |
| `22004` | data | an endpoint of an `INSERT` edge is `NULL` (from an `OPTIONAL MATCH`); the right side of `SET n = ..` / `SET n += ..` is `NULL`; a property value of a `MERGE` pattern or a node bound before it is `NULL` |
| `22G02` | data | a negative `OFFSET` / `LIMIT` (literal or parameter value) |
| `22G03` | data | a value of the wrong type, before the run when the kind is certain from the query text ([static kind checks](semantics.md#static-kind-checks): `false AND 123`, `1 IN true`, `type(n)` of a node variable, `p.name` of a path, `WITH 123 AS x RETURN x.num`), else when a row reaches it: `OFFSET` / `LIMIT` not an integer, `'a' + 1`, `type()` of a vertex, a non-boolean condition, `IN` with a right operand that is no list, `sum()` / `avg()` of a non-number; a map projection of a value that is no element or map, a list comprehension over a value that is no list; a temporal component the type lacks (`date(..).hour`), temporal arithmetic without a meaning (`date + date`); a function argument of the wrong kind (`toFloat(true)`, a float where an integer is required, `toString()` of a list, map, vertex, edge or path, `trim(s, x)` with a string `s`); an `INSERT` endpoint that is no node, a node, edge or path as an `INSERT` or `SET` property value; a `SET` / `REMOVE` target that holds no node or edge, a label item on an edge at run time, a right side of `SET n = ..` / `+=` that is no map, node or edge; a `DELETE` target that holds no node, edge or path; a node bound before a `MERGE` that is no node, a node, edge or path as a `MERGE` property value |
| `G1001` | data | a `DELETE` / `NODETACH DELETE` of a node that still has an edge the same clause does not delete (use `DETACH DELETE`) |
| `22012` | data | integer division or `%` by zero (a float one gives `Infinity` or `NaN`), also `mod(a, 0)`; a duration divided by zero |
| `22007` | data | temporal text that is no value of the type, a temporal map with an unknown key or conflicting date keys, an unknown time zone ([Dates and times](#dates-and-times)) |
| `22008` | data | a temporal field out of range, a temporal value or result outside the years -9999 to 9999 |
| `22003` | data | a numeric result out of range (integer overflow, also of `sum()`, `abs()`, `exp()`, `power()`); a conversion out of range (`toInteger(1.0e19)`, `CAST('1e400' AS FLOAT)`) |
| `22018` | data | `CAST` of text that is no value of the target type (`CAST('abc' AS INTEGER)`) |
| `22011` | data | a negative position or length in `substring()`, `left()`, `right()` |
| `22027` | data | a `TRIM(c FROM s)` character `c` that is not exactly one character |
| `2201E` | data | the logarithm of a number `<= 0`, or to a base `<= 0` or `1` (`ln`, `log`, `log10`) |
| `2201F` | data | `sqrt()` of a negative number, `power()` of 0 to a negative power or of a negative number to a non-integer power |
| `22023` | data | an argument outside the function's values: a truncation unit the type lacks or an unknown one (`date.truncate('hour', ..)`), `range()` step 0, a negative `round()` precision or an unknown rounding mode, a negative `replace()` limit, a negative `n` in the list `trim(list, n)` |

`42001` is fixed; the other codes are **provisional**: they follow the standard's classes and
may still be refined before the GQL support is complete. The function codes (`22018`, `22011`,
`22027`, `2201E`, `2201F`, `22023`, `22007`, `22008`) are the ISO / SQL-heritage subclasses of class `22`; Neo4j
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
