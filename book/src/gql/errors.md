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
Help: Expected an expression at the marked position. Fix the query at the marked position: a GQL read query is MATCH <pattern> [WHERE <condition>] RETURN <items>, for example g.gql("MATCH (p:person) WHERE p.age > 30 RETURN p.name").
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
  expected:     `,`, WHERE, MATCH, RETURN
  did you mean: RETURN
  GQLSTATUS:    42001
Help: Did you mean RETURN? Expected `,`, WHERE, MATCH, RETURN at the marked position. ...
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

## GQLSTATUS codes

| Code | Kind | When |
|---|---|---|
| `42001` | syntax | the query does not parse: an unexpected token, a missing `RETURN`, an unclosed bracket |
| `42002` | semantic | an undefined variable, a variable used as a node and an edge, an edge variable bound twice in one `MATCH`, a duplicate column name, a missing parameter, an `ORDER BY` key that `RETURN DISTINCT` dropped |
| `0A000` | refused / not yet | a [refused construct](refused.md) or one [not supported yet](subset.md#not-supported-yet); also `gql()` after other steps |
| `54001` | limit | the query nests too deeply (more than 128 levels of parentheses, or an expression taller than 512) |
| `22G02` | data | a negative `OFFSET` / `LIMIT` (literal or parameter value) |
| `22G03` | data | a value of the wrong type: `OFFSET` / `LIMIT` not an integer, `'a' + 1`, `type()` of a vertex, a non-boolean condition |
| `22012` | data | division or `%` by zero |
| `22003` | data | a numeric result out of range (integer overflow) |

`42001` is fixed; the other codes are **provisional**: they follow the standard's classes and
may still be refined before the GQL support is complete. Match on the kind
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
