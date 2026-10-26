# Predicates: Comparison and Resolution

This page lists where Graphersal's predicates (`P.eq`, `P.neq`, `P.lt`, `P.lte`, `P.gt`, `P.gte`,
`P.within`, `P.without`, `P.inside`, `P.outside`, `P.between`), their composition (`and`, `or`,
`not`, `negate`) and the id order of elements differ from Apache TinkerPop 3.8.2 or settle what
TinkerPop leaves open. The [Deviations](#deviations) table at the end collects them.

## One comparison core

All of these predicates compare through one rule set (TinkerPop's `semantics/Comparability`).

- Numbers compare by value across `int64` and `float64`; `NaN` is incomparable with everything,
  itself included.
- Strings compare with strings, booleans with booleans (`false < true`), UUIDs with UUIDs,
  `null` only with `null`.
- A string is never read as a number, a date or a UUID.
- **Incomparable means false, never an error.** Every test except `neq` is false for an
  incomparable pair. A vertex, edge, path or collection on the stream against a number is false
  as well; for example `g.V().where(P.eq(111))` is an empty result, not a cast error.

```rhai
g.V().where(P.eq(111)).count().next()          // 0, no error
g.V().has("age", P.gt("x")).count().next()     // 0, a number never compares with a string
```

**Operands are single values**: a string, a number, a boolean, a date or time, a UUID, a vector
or `()` (the items of `P.within([..])` / `P.without([..])` each). A list, a map or a traversal as
an operand is an `ArgumentMismatch` naming the predicate when the step is built, never compared
as its text. Comparing a list or map *inside* a predicate is not supported; to match a list or
map property as a whole, pass the value itself, as in `has("tags", ["a", "b"])`. `P.within()` /
`P.without()` without arguments are the empty set.

```rhai,graph=modern
g.V().has("name", P.gt(["a"])).count().next()          // error: Argument mismatch for step 'P.gt'
g.V().has("name", P.within(["a", "b"])).count().next()  // 0: the list holds the values
g.V().has("name", P.without()).count().next()           // 6
```

## Text predicates match strings only

`TextP.startingWith`, `endingWith`, `containing` and their negations (also spelled
`P.startingWith(..)`) test a string. Any other value on the stream (a number, a UUID, a list from
`fold()`, a map, a path, a vertex or an edge) is not one, so the predicate is false and its
negation true. This is never an error, matching TinkerPop, where `TextP` only matches a `String`:

```rhai,graph=modern
g.V().values("age").is(TextP.startingWith("2")).count().next()   // 0
g.V().fold().is(TextP.startingWith("a")).count().next()           // 0
g.V().fold().is(TextP.notStartingWith("a")).count().next()        // 1
```

A property handle (`values("name")`) and a property element (`properties("name")`) are tested by
their stored value.

## Regular expressions

`TextP.regex(pattern)` holds for a string in which `pattern` matches somewhere; `TextP.notRegex(pattern)`
is its complement (also spelled `not_regex`, and bare `regex(..)`/`notRegex(..)`). Like the other text
predicates, a value that is not a string makes `regex` false and `notRegex` true. The match is
unanchored, as Java's `Matcher.find()` is; write `^...$` to match the whole string.

```rhai,graph=modern
g.V().has("name", TextP.regex("^[jp]")).values("name").to_list()   // josh, peter
g.V().has("name", TextP.regex("(?i)^MAR")).values("name").to_list() // marko
```

**Deviation from TinkerPop:** Graphersal compiles the expression with the Rust
[`regex`](https://docs.rs/regex/latest/regex/#syntax) crate and uses that crate's dialect, not
Java's `java.util.regex`. Look at the crate's syntax page for what an expression may contain.
Matching takes linear time in the input. The expression is compiled once, when the predicate is
built: an invalid one is a script error (`ValueError::InvalidRegex`) before the traversal runs. In
Rust the predicate is `P::regex("^mar")?` / `P::not_regex(..)?`.

## Resolution of a string operand

- **`where(P)`:** a bare string operand of `eq/neq/lt/lte/gt/gte/within/without` is resolved in
  this order: a **path label** (`as("a")`), then a **side effect** (`aggregate`, `store`,
  `withSideEffect`), then the **literal** string. A side effect holding a collection compares as
  a whole value for `eq`/`neq` and is false for the ordering predicates. An unknown name stays a
  literal.
- **`has(key, P)`, `is(P)`, `hasValue(P)`, `all(P)`, `any(P)`, `none(P)`:** operands are always
  literal values, as in TinkerPop; they never resolve to a label or side effect, so a name that
  exists as a step label or side-effect key cannot change the meaning of a property filter:
  `g.V().as("vadas").out("knows").has("name", P.eq("vadas"))` finds vadas.
- **`eq`/`neq` with a string on a vertex** compares the vertex's id (`is(P.eq("1"))`, kept from
  the original implementation); under `where(P)` a step label of that name comes first.
- **Changed in 0.2.2:** before, a value filter compared a string naming a step label with the
  labelled element (so the first query below returned nothing). TinkerPop compares the literal;
  only `where(P)` resolves labels.

```rhai,graph=modern
g.v(1).as("vadas").out("knows").has("name", P.eq("vadas")).values("name").to_list()   // ["vadas"]: a value filter compares the string
g.v(1).as("a").out("created").in("created").where(P.eq("a")).values("name").to_list()   // ["marko"]: where() reads the label a
```

## hasId(P), hasLabel(P), hasKey(P)

`hasId(P.neq("1"))` and its siblings evaluate the predicate instead of stringifying it. A lone
`P.eq(x)`/`P.within(xs)` is rewritten to the literal step so source-filter pushdown still applies.

- **Operands are always literal.** No step label or side effect is consulted, so a label named
  like an id cannot shadow it.
- **Label and key sets.** A vertex carries a set of labels, a map a set of keys. A *positive*
  predicate (`eq`, `within`, `lt`, ...) holds if **any** label or key satisfies it; a *negated*
  one (`neq`, `without`, `notStartingWith`, ...) holds only if **all** do. For a single-label
  vertex this is TinkerPop's behaviour.
- A predicate mixed with other arguments (`hasId(P.eq("1"), "2")`) is an `ArgumentMismatch`.
- **Ids are strings.** An order predicate (`P.gt`, `P.gte`, `P.lt`, `P.lte`, `P.between`,
  `P.inside`, `P.outside`) with a number operand in `hasId(P)` / `has(T.id, P)` is an error
  before the query runs, not a filter that silently matches nothing; the help shows the string
  form. Compare with a string instead, `hasId(P.gt("3"))`, and mind the string order: `"10"`
  sorts before `"9"`. Equality forms (`hasId(3)`, `hasId(P.eq(3))`, `hasId(P.neq(3))`,
  `hasId(P.within(1, 2))`, `hasId(P.without(1, 2))`, also nested as in
  `hasId(P.eq(1).or(P.eq(2)))`) take numbers and compare their literal text, in the DSL and the
  Rust API (`has_id_p(P::Neq(3))`): `3` matches the id `"3"`, a float `3.0` the id `"3.0"`.

```rhai,graph=modern
g.V().hasId(P.neq("1")).count().next()                     // 5
g.V().hasId(P.gt("3")).count().next()                      // 3 ("4", "5", "6")
g.V().hasId(P.without(1, 2)).count().next()                // 4
g.V().hasLabel(P.within("person", "x")).count().next()     // 4
```

## Element order

- **TinkerPop:** vertices and edges order by id; its ids are typically numbers, so `9` sorts
  before `10`.
- **Graphersal:** ids are stored as strings and compared as strings (a string is never parsed
  into a number), so `"10"` sorts before `"9"`. Elements with equal or missing ids fall back to a
  structural comparison.

```rhai,graph=modern
g.V().order().by(T.id).id().toList()   // "1", "2", ... ; with ids up to 10: "1", "10", "2", ...
```

## Composing predicates: and, or, not, negate

```rhai,graph=modern
g.V().has("age", P.gt(18).and(P.lt(30)).or(P.gt(35))).values("name").toList()   // marko, vadas
g.V().has("name", TextP.containing("o").and(P.lt("m"))).values("name").toList() // lop, josh
g.V().hasLabel(P.not(P.eq("person"))).count().next()                            // 2
g.V("1").out().aggregate("a").out().where(P.not(P.within("a"))).values("name").toList()
```

- `p.and(q)` and `p.or(q)` are binary and left-associative: `a.and(b).or(c)` means
  `(a and b) or c`, `a.or(b).and(c)` means `(a or b) and c`. Evaluation is left to right and
  **short-circuits**, as TinkerPop's `AndP`/`OrP` do: `P.eq(1).or(q)` never evaluates `q` for a
  value equal to 1, so a runtime error on the right is not raised when the left side decides.
- `P.not(p)` (also `P::not(p)` and a bare `not(p)`) and `p.negate()` are the complement of the
  result for the same stream value. `TextP` predicates compose with `P` ones.
- The logic is **two-valued**. An incomparable pair is plain `false` (see above), so
  `P.lt(NaN)` is the "error" column of the `Comparability` scenarios and reduces to `false`;
  there is no third value.
- Over a label or key set (`hasLabel(P)`, `hasKey(P)`) a leaf keeps the rule above (positive:
  any, negated: all); a composite combines the **set results**: `and` is `&&`, `or` is `||`,
  `not` is `!` of the child's set result.
- In `where(P)` each child resolves a string operand on its own (step label, then side effect,
  then literal), and the path analysis records the union of the labels of the children
  (`P.eq("a").or(P.eq("d"))` records `a` and `d`).
- A composite is never pushed into an index or a source filter (`.profile()` shows
  `has("age", P.gt(27).and(P.lt(33)))` with no optimizer rule named); the results are identical with
  `g.with("optimizer.disabled", [...])`.
- A non-predicate operand (`P.gt(1).and(__.out())`, `P.not("x")`) is an `ArgumentMismatch`
  whose help names the predicate and the traversal combinators (`__.and(..)`, `__.or(..)`).
- Not part of this feature: the infix steps `g.V().has(..).and().has(..)` (TinkerPop's
  `ConnectiveStrategy`) and the two-argument `where("a", P)` with `by()` modulators.

## Type predicates

`P.typeOf(GType.X)` keeps a value of the given type (`GType.STRING`, `GType.LONG`, `GType.NUMBER`,
`GType.LIST`, `GType.VERTEX`, ...). TinkerPop 3.8 also accepts a type *name*, and so does
Graphersal, for a fixed list of names:

```rhai,graph=modern
g.V().values("name").is(P.typeOf("String")).count().next()   // 6
g.V().values("age").is(typeOf("Long")).count().next()         // 4
g.V().values("age").is(P.typeOf("Byte"))                      // script error: unknown type name
```

| Name | Token |
|---|---|
| `String` | `GType.STRING` |
| `Integer`, `Long` | `GType.LONG` (every integer is an `int64`) |
| `Double`, `Float` | `GType.DOUBLE` (every float is a `float64`) |
| `OffsetDateTime` | `GType.DATETIME` (TinkerPop's date type; see [Dates and Times](../dates/index.md)) |
| `Duration` | `GType.DURATION` |
| `LocalDate`, `LocalDateTime`, `LocalTime` | `GType.DATE`, `GType.LOCALDATETIME`, `GType.TIME` (Graphersal's other date and time types, by their `java.time` names) |
| `Boolean`, `UUID`, `Number`, `List`, `Map`, `Vertex`, `Edge`, `Path`, `Graph` | the token of the same name |

Both forms take the same values in both spellings (`P.typeOf(GType.LONG)`, `P::typeOf(GType::LONG)`,
`typeOf("Long")` select the same ages). The names are Java's simple class names, as TinkerPop
registers them, not the token names: `"LONG"` is not a type name, in TinkerPop either.
The names are case-sensitive. Any other name is a script error (`ArgumentMismatch` listing the
accepted names), as TinkerPop rejects a name it has not registered. In Rust, the same lookup is
`GType::from_type_name`.

## Deviations

| Area | TinkerPop behaviour | Graphersal behaviour | Why | Scenarios affected |
|---|---|---|---|---|
| `P.typeOf(name)` | any name in TinkerPop's registered-type cache, custom types included | a fixed list of names (above) | Graphersal has no type registry; one width per number kind | none (the suite uses `"String"` and an unregistered name) |
| `negate()` / `P.not(p)` | `Compare.negate()` flips the operator (`gt` becomes `lte`), so the result differs from the complement for incomparable operands | The plain complement of the result: `P.not(P.gt(1))` is `true` for `"foo"` and `NaN` | One rule for every predicate (text, `typeOf`, composites); operator flipping would need a per-variant inverse | None (every vendored scenario negates a comparable number) |
| Error value in `and`/`or` | Comparability scenarios call a `NaN` comparison an "error" column | There is no error value: incomparable is `false`, the logic is two-valued | The expected results reduce to boolean `and`/`or` | 18 `Comparability` |
| Numeric widths in the test surface | byte, short, BigInteger, BigDecimal are distinct types | Only `int64`/`float64` exist; the harness maps `1b`/`1s`/`1n` to an integer and `1m` to a float (documented in `tests/tinkerpop/README.md`) | Widths cannot be told apart; equality is by value | 8 `Equality`, `Sum`, `AsNumber`, `Inject` with width literals |
| Composites and indexes | n/a | A composite predicate is not index-pushable | Only a single comparison can be folded into a range | none |
| Incomparable pairs | Some comparisons raise | `false` for every test but `neq`, never an error | See "One comparison core" | `Comparability`, `where(111)` on a vertex |
| Element id order | Numeric ids order numerically | Ids compare as strings, `"10"` sorts before `"9"`; an order predicate on a number in `hasId(P)`/`has(T.id, P)` (`P.gt(3)`) is an error naming the string form `P.gt("3")` | A string is never parsed into a number; a number comparison against a string id could never hold | `order().by(T.id)` on ids with different digit counts |
| `null` | Property `null` handling depends on the `@AllowNullPropertyValues` flavour | `null` is a real stored value | Graphersal has a `null` type | `AddVertex`/`AddEdge`/`MergeEdge` null-property scenarios |
| Property cardinality | `list`/`set` properties | One value per property key | By design | `@MultiProperties` scenarios |
