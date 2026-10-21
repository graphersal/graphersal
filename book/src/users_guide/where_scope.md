# where() Start and End Labels

This page lists where Graphersal's `where(traversal)` start and end labels differ from Apache
TinkerPop 3.8.2. Everything not listed follows `Where.feature`.

In `where(__.as("a").out().as("b"))` a leading `as("a")` is a *start label*: the child starts at
the object labelled `a` (a key of the current map value, a side-effect key, or a path label, in that
order, like `select()`). A trailing `as("b")` is an *end label*: the traverser is kept only when some
result of the child equals the object labelled `b`. The same holds for the children of `and()`,
`or()` and `not()` when that connective is the first step of the `where()` child. A start or end
label that resolves to nothing filters the traverser out, without an error.

## where(P) and by()

`where(P)` takes `by()` modulators, and a bare-string operand of any comparison (`eq`, `neq`, `lt`,
`lte`, `gt`, `gte`) is resolved as a step label first, then as a side-effect key, and only then
taken as a literal text:

```rhai,graph=modern
g.V("1").as("a").out().has("age").where(P.gt("a")).by("age").values("name")   // ["josh"]
```

The `by()` modulators form a ring, as in TinkerPop. The current object is projected through the
first `by()`; each label operand then takes the next one, from left to right in the textual order of
the predicate (`and`, `or` and `not` children included), cycling when there are fewer `by()` than
operands:

| Query | Current object | Operand `a` |
|---|---|---|
| `where(P.gt("a")).by("age")` | `age` | `age` |
| `where(P.gt("a")).by("age").by("weight")` | `age` | `weight` |
| `where(P.gt("a").and(P.lt("b"))).by("x").by("y").by("z")` | `x` | `y` for `a`, `z` for `b` |

A comparison without a label operand (`where(P.gt(30)).by("age")`) projects only the current
object. A `by()` that yields no value (a vertex without the property, a child traversal with no
result) drops the traverser, also under a `not()`. Without `by()`, two scalars compare by value
(`values("age").as("a")` followed by `where(P.gt("a"))` works), however each is held: a computed
`29` (`g.inject(29).as("x").V().values("age").where(P.eq("x"))`) equals a property read with
`values("age")`, and `1` equals `1.0`, as `P.eq(29)` and `is(29)` compare; the end label of
`where(__.values("age").as("x"))` and `where(P.within("side effect"))` compare the same way. Two
elements compare by identity for `eq`/`neq` and are incomparable (false) for an ordering; lists
and maps are equal when their contents are. `has(key, P.gt("x"))`, `is(P.gt("x"))`,
`all()` and `any()` keep comparing literal strings, even when a label `x` exists.

## Deviations

- **The end label is read on the incoming traverser.** TinkerPop looks it up on the child's output
  traverser. Both see the same path labels; they differ only when the child itself defines the
  label.
- **Several labels on a start or end step are rejected when the query is planned**, for both ends.
  TinkerPop rejects an end step with several labels only.
- **A connective after a start label is an ordinary step.** `where(__.as("a").and(...))` does not
  turn the `as()` calls inside the `and()` children into labels, as in TinkerPop.
- **Outside a `where()` nothing changes.** An `as()` at the start or end of the children of
  `not()`, `and()`, `or()` or `union()` labels the child's own path.
- **A start label that was never declared.** `where(__.as("a").out())` with no earlier `as("a")`
  filters every traverser out (TinkerPop's missing-key behaviour).
- **No flattening.** The optimizer rule `where_unnest` leaves a `where()` with a start or end label
  alone, because the label changes what the child runs on.
- **`where(P.gt("a"))` resolves the label `a`** for every comparison predicate, the ordering ones
  included, so it compares against the labelled object, never the text `"a"`.
- **Ring order of composite predicates follows the textual order.** `where(P.gt("a").and(P.lt("b")))`
  gives `a` the second and `b` the third `by()` whatever short-circuiting skips. How TinkerPop's
  connective handling orders its traversal ring was not verified, and no vendored scenario has a
  composite predicate with `by()`.
- **Not implemented:** a `by()` projection of the operands of `within`/`without`.

## where("a", P)

The two-argument form tests the object labelled `a` instead of the current object (TinkerPop's
`WherePredicateStep` with a start key). `a` is looked up like `select("a")`: a key of the current
map, then a side effect, then the path label (`Pop.last`); a traverser without it is filtered out.
The `by()` ring starts with that object.

```rhai,graph=modern
g.V("1").as("a").out("created").in("created").as("b").where("a", P.neq("b")).values("name")
// ["peter", "josh"]
g.V().as("a").out("created").in("created").as("b").where("a", P.gt("b")).by("age").
  select("a", "b").by("name")
// {"a": "josh", "b": "marko"}, {"a": "peter", "b": "josh"}, {"a": "peter", "b": "marko"}
```

Rust: `where_label_p("a", P::Neq("b"))`.

## filter() versus where()

`filter(traversal)` (Rust `filter_t`, also `__.filter`) keeps a traverser when the child traversal
yields at least one result, exactly like TinkerPop's `TraversalFilterStep`. It is not an exact alias
of `where(traversal)`:

| | `where(traversal)` | `filter(traversal)` |
|---|---|---|
| plain child | existence test | existence test (same result) |
| leading `as("a")` | start label: the child starts at the object labelled `a` | ordinary label of the child's own path |
| trailing `as("b")` | end label: some child result must equal `b` | ordinary label of the child's own path |
| predicate form | `where(P.gt("a"))` | not accepted |

```rhai
g.V().filter(__.as("a").out("knows").as("b")).values("name")   // ["marko"]
g.V().where(__.as("a").out("knows").as("b")).values("name")    // []: `a` is not labelled outside
```

Labels declared inside a `filter()` child are not visible to the outer traversal. The child is
optimized and path-analysed like the child of `where()`; a child that mutates the graph makes the
whole query a mutating one. There is no deviation from TinkerPop.
