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
Help: The gql() query built a value larger than 'traversal.max_materialized_bytes' allows (1073741824 bytes); ... A function whose result can be larger than its arguments (range(), split(), replace(), || and toString() of a list or map) and the list of collect_list() are checked before they are built, so the query stops instead of running out of memory; the result is never cut short. Build smaller values: bound range() and replace() by the data, as in g.gql("MATCH (p:person) RETURN range(1, p.age / 10) AS r"), ... or raise the limit for the query, g.with("traversal.max_materialized_bytes", 2147483648).gql(..), ...
```

## Subqueries

The errors of an `EXISTS` subquery or a pattern predicate share one help text that explains
subqueries: TRUE or FALSE, never `NULL`; the body reads the variables bound before it, and the
variables it binds stay inside it; a pattern predicate binds no new variable. Before it, each
names its own fix:

```rhai,graph=modern
g.gql("MATCH (p:person) RETURN p.name AS name, (p)-[:knows]->(q) AS k")   // error: the pattern predicate introduces the new variable `q`
g.gql("MATCH (p:person) WHERE EXISTS { MATCH (p)-[:created]->(s) } RETURN s.name AS s")   // error: the variable `s` is not in scope: it is bound inside a subquery
g.gql("MATCH (a:person) WHERE EXISTS { MATCH (a)-->() WHERE b.age > 1 MATCH (b) } RETURN a")   // error: `b` is bound by a later MATCH clause of the subquery
```

```text
Help: write it as EXISTS { MATCH (p)-[:knows]->(q) }, whose MATCH may bind new variables, or leave `q` out of the pattern. A subquery, EXISTS { MATCH <pattern> [WHERE <condition>] }, EXISTS { <pattern> } or a Cypher pattern predicate such as (p)-[:created]->(), is TRUE when its body has a match and FALSE otherwise, never NULL; its body reads the variables bound before it, and the variables it binds itself stay inside it. A pattern predicate binds no new variable: name new elements in an EXISTS { MATCH .. }, as in g.gql("MATCH (p:person) WHERE NOT EXISTS { MATCH (p)-[:created]->(s) WHERE s.lang = 'java' } RETURN p.name"), and bind an element in the MATCH clause of the query to read it after the subquery, as in g.gql("MATCH (p:person)-[:created]->(s) RETURN p.name, s.name").
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
Help: ... A function whose result can be larger than its arguments (range(), split(), replace(), || and toString() of a list or map) and the list of collect_list() are checked before they are built, as is the list of a pattern comprehension [(a)-->(b) | b.name], so the query stops instead of running out of memory; the result is never cut short. ... count the values instead of collecting them, ... or, for the matches of a pattern, g.gql("MATCH (p:person) OPTIONAL MATCH (p)-->(f) RETURN p.name, count(f) AS n"), ...
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
g.gql("MATCH (p:person) CALL (p) { MATCH (p)-[:created]->(s) } RETURN p")   // error: a CALL { } body without RETURN (a unit subquery) only makes sense for writes
g.gql("MATCH (p:person) CALL (p) { MATCH (p)-->(s) RETURN s.name AS sw } IN TRANSACTIONS RETURN sw")   // refused: CALL { } IN TRANSACTIONS commits writes in batches
```

```text
Help: rename the column inside the body, as in RETURN ... AS p2. A CALL subquery, CALL (a, b) { <query ending in RETURN> }, runs its query once for each incoming row: the body reads only the variables its scope clause names (CALL (*) imports all of them, CALL { } none, and Cypher's CALL { WITH a .. } those of its first WITH), and passes on only the columns of its RETURN, which must be new names; its aggregates, ORDER BY and LIMIT apply to each row's records, as in g.gql("MATCH (p:person) CALL (p) { MATCH (p)-[:created]->(s) RETURN count(s) AS made } RETURN p.name, made"). A row whose body returns no record is dropped; OPTIONAL CALL keeps it with NULL columns.
Help: import `p` with a variable scope clause, as in CALL (p) { MATCH ... WHERE ... }, or in Cypher with a WITH of plain variables first and the rest of the body after it, as in CALL { WITH p MATCH ... WHERE ... }. A CALL subquery, ...
Help: end the body with RETURN and the columns the query reads after it, for example MATCH (p:person) CALL (p) { MATCH (p)-[:created]->(s) RETURN count(s) AS made } RETURN p.name, made. A CALL subquery, ...
Help: remove IN TRANSACTIONS: a read-only subquery needs no batches. A CALL subquery, ...
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

## GQLSTATUS codes

| Code | Kind | When |
|---|---|---|
| `42001` | syntax | the query does not parse: an unexpected token, a missing `RETURN`, an unclosed bracket, a quantifier with `m > n`, `GROUP BY` after `WITH` (grouping is implicit) or after `ORDER BY`; an unknown function or `CAST` type name (with a suggestion), a function called with the wrong number of arguments, the SQL form `SUBSTRING(s FROM a FOR b)`; `WITH ORDINALITY` / `WITH OFFSET` after `UNWIND` |
| `42002` | semantic | an undefined variable, a variable used as a node and an edge, an edge variable bound twice in one `MATCH`, a duplicate column name, a missing parameter, an `ORDER BY` key that `RETURN DISTINCT` dropped; a variable a `WITH` did not carry on, a `LET` name bound again; a variable read outside an aggregate that is not a grouping key, an aggregate in `WHERE` / `FILTER` / `LET` or inside another aggregate, `ORDER BY` after an aggregation reading something other than a column; a `GROUP BY` key that is not an item without an aggregate, or such an item missing from `GROUP BY`; a path or group variable bound twice; a variable of a quantified path pattern that is not a new name, or a property of one read outside its body; a `FOR` / `UNWIND` name already in scope, one name for the item and its position, the position used as a node or edge, an aggregate in the `FOR` list; a new variable in a pattern predicate, a variable bound inside a subquery (`EXISTS`, a pattern predicate, `COUNT { }`, `COLLECT { }`) read outside it, a condition in a subquery reading a variable of a later `MATCH` clause of the body, a subquery in an aggregating item reading a variable outside the aggregates that is not a grouping key; a variable bound inside a pattern comprehension read outside it, a comprehension variable named like another variable of the query, an aggregate inside a comprehension's `WHERE` or `\|` part; a `COLLECT { }` body that does not return exactly one column; `UNION` operands with different column names, `UNION` mixed with `UNION ALL` in one chain, a variable read after `NEXT` that the `RETURN` before it does not return; a `CALL` body returning a name already in scope, reading a variable it does not import (also through a first `WITH` that is no importing `WITH`), importing one twice |
| `0A000` | refused / not yet | a [refused construct](refused.md) or one [not supported yet](subset.md#not-supported-yet) (also a function: `sin()`, `date()`, `rand()`, `elements()`, `^`, `CAST` to `DATE`; `EXCEPT`, `INTERSECT`, `OTHERWISE`; a `CALL` body without `RETURN`, `CALL { } IN TRANSACTIONS`; Cypher 4's `exists(n.p)`; a list-comprehension or list-predicate variable read inside a subquery; inside a parenthesized path pattern: a nested group or quantified edge, a path variable, several patterns, no edge, a variable written twice or bound outside, a condition reading an outer variable, a subquery); also `gql()` after other steps |
| `54001` | limit | the query nests too deeply (more than 128 levels of parentheses, or an expression taller than 512) |
| `54001` | limit | more than 64 subqueries (`EXISTS`, pattern predicates, pattern comprehensions, `COUNT { }` and `COLLECT { }`, nested ones included), or more than 1024 subquery bodies to run (a `LET` / `WITH` variable holding a subquery counts once per reader); `CALL` bodies and whole-query subquery bodies nested more than 16 levels |
| `54001` | limit | a MATCH clause holds more than 64 edge patterns (over all its comma-separated paths; a quantified body's edges count once); split it into several MATCH clauses that share a variable, repeat a chain as a quantified path pattern, or use Gremlin `repeat()` |
| `22G02` | data | a negative `OFFSET` / `LIMIT` (literal or parameter value) |
| `22G03` | data | a value of the wrong type: `OFFSET` / `LIMIT` not an integer, `'a' + 1`, `type()` of a vertex, a non-boolean condition, `sum()` / `avg()` of a non-number; a map projection of a value that is no element or map, a list comprehension over a value that is no list; a function argument of the wrong kind (`toFloat(true)`, a float where an integer is required, `toString()` of a vertex, edge or path, `trim(s, x)` with a string `s`) |
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
