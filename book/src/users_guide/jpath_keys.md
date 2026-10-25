# Path keys (jpath)

Wherever a read step takes a property name, it also accepts `jpath("...")`: a path into a nested
property value (an object or an array). A plain string is always a literal property name:
`"a.b"` names a property called `a.b`, `jpath("a.b")` reads key `b` inside the property `a`.
Strings are never sniffed.

```rhai
g.v().values(jpath("meta.tags[0]"))
g.v().has(jpath("meta.k"), P.gt(1))
g.v().order().by(jpath("meta.k"))
g.v().valueMap(jpath("meta.k"), "name")      // key of the entry: "$.meta.k"
g.v().properties(jpath("meta.k")).key()      // "$.meta.k"
```

## Grammar

RFC 9535 *singular queries* only: member names and integer indices, so a path always reaches at
most one value. `$` is optional (`a.b[1].c` and `$.a.b[1].c` are the same). Bracket names
`['a.b']` / `["a.b"]` hold a name that contains a dot, a quote or a bracket, with the escapes `\'`,
`\"`, `\\` (and the RFC control escapes and `\uXXXX`). Negative indices count from the end: `[-1]`
is the last element. Not supported (a parse error naming the position): `..` recursive descent,
wildcards `*`, filters `[?(..)]`, slices `[a:b]`, unions `[a,b]`. A path needs at least one
segment: `$` alone and the empty string are errors. A path has at most 256 segments, and an index is at most
9007199254740991 (2^53 - 1, the interoperable JSON integer range) from either end; beyond that it
is a parse error naming the position.

Deliberate leniencies compared to the RFC, kept so that old `json_path("meta.tags[0]")` strings
keep working: the dot notation accepts any non-structural character in a name (`user-name`; the
canonical form then renders it as `['user-name']`), a leading `.` and a leading `[` without `$` are
accepted, and whitespace inside brackets is allowed. Empty segments (`a..b`, `a.`, `a.[0]`) are
rejected. The canonical text (`$.a.b[1].c`, bracket form only for names that need it) is what shows
up as a result key and in `.profile()`; parsing the canonical text gives the same path back.

A plain string is never parsed: `values("a.b")` reads the property named `a.b`. Write
`jpath("a.b")` for the path. In the Rust API, any `&str`/`String` converts to a name key
(`PropKey::Name`) and a `JsonPath` to a path key (`PropKey::Path`).

## Missing paths

A path that does not exist (missing key, index out of range, a scalar on the way) is an absent
property, never an error: `values` yields nothing, `has` is false, `hasNot` is true, `by()` is
unproductive.

## `properties(jpath(...))`

A path has no stored property to point at, so `properties(jpath(..))` yields a *path property
element*: a materialized pair of the canonical path text and the value the path ends on. It
behaves like a property element otherwise:

| Step | Result |
|---|---|
| `key()` | canonical path text, e.g. `$.meta.k` |
| `value()` | the leaf value |
| `label()`, `hasLabel(..)`, `hasKey(..)` | the path text (like a property's key) |
| `hasValue(..)`, `is(P)`, `where(P)` | compare the leaf |
| `id()` | error: property elements have no id in Graphersal |
| `as_string()` | `p[$.meta.k->1]` |
| `dedup()` | compares path text and leaf |
| terminal (`toList()`, display, JSON) | the leaf value |
| `out()`, `has(..)`, `drop()`, ... | error (not a vertex or an edge) |

Only a vertex or an edge is a valid input. This is a Graphersal extension: a plain name never
produces this element and behaves exactly as in TinkerPop.

## Name-taking steps and what they accept

Converted (accept a path): `values`, `properties`, `has` (all forms with a key), `hasNot`,
`valueMap`, `elementMap`, and `by(jpath(..))` everywhere a property `by()` is read (order, group,
groupCount, dedup, path, tree, select, project, where, math, aggregate, store, sack).

Not applicable, on purpose:

- `hasKey` / `hasKey(P)` filter the *key* of a property element or the keys of a map; there is
  nothing nested to descend into. Pass names; to filter a path property element use its path text
  (`properties(jpath("a.b")).hasKey("$.a.b")`).
- `glob_path().by(..)`: the match key is the stored value of one property, a `jpath` there is
  rejected with a message that names `by("name")`.
- `project(names)`, `select(labels)`, `as(..)`, `dedup("label")`, `math("a + b")`, `cap`,
  `aggregate("x")`, `store("x")`, `loops(name)`: these take step labels or result names, not
  property names (only their `by()` was relevant).
- `has_label`, `has_id`, `hasValue`, `out(..)`, `in(..)`, ...: labels, ids, values.
- `merge_v` / `merge_e` match maps: literal property names.
- `property(map)`, `property(T.id|T.label, v)`, `property_json(..)`, `mergeV`/`mergeE` maps: the
  keys of a map are literal property names (a map key `"a.b"` is the property `a.b`), `property_json`
  merges whole top-level properties. Use `property(jpath(..), v)` for nested writes.
- `remove_property(jpath(..))` is covered below; `drop()` on a path property element is an error (see below).

## Writing: `property(jpath(..), value)`

The first segment is the property name on the vertex or edge, the rest descends into its value.
`value` is anything a plain `property(name, value)` accepts (a constant, a nested map or list, or a
traversal whose first result is used; an empty traversal result writes nothing). Vertices and edges
are still rejected as values.

| Situation | Result |
|---|---|
| a key segment, the key is missing (also the whole top-level property) | created as an object |
| a key segment, the key exists | replaced (leaf or whole subtree) |
| `[n]`, `0 <= n < len` | replaces the element |
| `[len]` as the **last** segment | appends |
| `[n]`, `n > len`, or `[len]` followed by more segments | error, arrays are never padded with `null` |
| `[-k]`, `1 <= k <= len` | replaces counting from the end; error when out of range or the array is empty |
| an index segment where the array does not exist | error: only key segments create missing containers |
| key on an array / scalar / `null` / non-string-keyed map | error, never an overwrite |
| index on an object / scalar / `null` | error |
| first segment is an index (`jpath("[0]")`) | error: the first segment names the property |
| `property(Cardinality.x, jpath(..), v)` | error in v1 (Graphersal stores one value per property) |

```rhai
g.V("a").property(jpath("meta.k.z"), 1)          // creates meta, k, z as objects
g.V("a").property(jpath("meta.tags[0]"), "x")    // replace; error when meta.tags does not exist
g.V("a").property(jpath("meta.tags[-1]"), "z")   // replace the last element
```

All faults surface as `GraphError::InvalidPropertyPath` with a help text, and every fault is
detected before anything changes, so a refused write leaves the property as it was. The failing
traversal is then rolled back as a whole, so writes made by earlier steps (or earlier traversers) of
the same traversal are undone too, exactly as for a plain `property` (see
[Transactions](transactions.md)). A single-segment path is the plain property. The write mutates the stored
value in place (no clone of the top-level property) when the graph has no schema; with a schema
the leaf is coerced at its declared nested type and the whole resulting top-level property is
validated (see [Schemas and nested paths](#schemas-and-nested-paths)). A
[property index](property_indexes.md) on the top-level property is updated with it (an index holds
top-level scalar values only).

A write counts one nesting level per path segment below the property, plus the depth of the
written value: `property(jpath("a.b.c"), [1])` leaves a value three levels deep. The result must
stay within the execution option `traversal.max_value_depth` (default 128), so a stored value is
never too deep to read back (see [Resource Limits](resource_limits.md#value-depth)).

## Removing: `remove_property(jpath(..))`

`remove_property(key)` (and the array form `remove_property([k1, k2])`,
which may mix names and paths) removes the value at a path and lets the element continue, so
`values(..)` can follow. The first segment is the property name, the rest descends into its value.

| Situation | Result |
|---|---|
| the last segment is an object key that exists | the key is removed (the parent object stays, even when it becomes empty) |
| the last segment is `[n]` / `[-k]` in range | the element is removed **with shift** (`Vec::remove`); `[-1]` is the last one |
| a missing key, a missing top-level property, an out-of-range index | **no-op**, not an error |
| a type mismatch on the way (key step on an array, index step on an object, descent through a scalar or `null`, a non-string-keyed map) | **no-op**: for removal a mismatch means "the path does not exist" |
| first segment is an index (`jpath("[0]")`) | no-op (it names no property) |
| a one-segment path (`jpath("age")`) | the same as plain `remove_property("age")` |
| a plain string, `remove_property("a.b")` | still a literal key: removes the property named `a.b` |

Removal is deliberately more lenient than writing (where a mismatch is an error): removing
something that is not there reaches the desired end state, whereas writing through a wrong
container type would have to overwrite data. `property(k, null)` still stores a `null`;
`remove_property` is the explicit removal.

```rhai
g.V("a").remove_property(jpath("meta.k"))          // removes the key k below meta
g.V("a").remove_property(jpath("meta.tags[0]"))    // removes the first element, the rest shifts down
g.V("a").remove_property([jpath("meta.k"), "age"]) // paths and names mix
```

With no schema the removal mutates the stored value in place (no clone of the top-level property).
With a schema active the whole resulting top-level property is validated as described in
[Schemas and nested paths](#schemas-and-nested-paths): a removal that would violate the declaration
(a required nested key, `min_items`, ...) is refused and leaves the property as it was; a path that
does not exist never reaches the schema layer. A removal of a whole property (a name or a
one-segment path) is the plain removal and is not checked against the schema, like `drop()`.
A property index on the top-level property is updated with it.

`properties(jpath(..)).drop()` stays an error: a path property element is a materialized leaf and
holds no reference to the vertex or edge it was read from, so `drop()` cannot know what to remove
(its help text names `remove_property(jpath(..))`). Use `remove_property` on the element instead.

## Schemas and nested paths

A schema describes nested content (`properties` of objects, `items` of arrays, `anyOf`
members), and every write and removal below a stored property honours it. The three modes:

| Mode | Declared nested field | Undeclared nested key |
|---|---|---|
| `none` | unchecked, nothing is coerced | allowed |
| `open` | coerced and type/constraint checked | allowed (unless the object says `"additionalProperties": false`) |
| `closed` | coerced and type/constraint checked | **rejected** (unless the object says `"additionalProperties": true`), the error names the full path (`$.meta.extra`) |

- **Leaf coercion.** `property(jpath("meta.w"), 3)` coerces the written leaf at the type the schema
  declares at that location (walking `properties` by key segments and `items` by index segments):
  an `int64` becomes a `float64` where `"number"` is declared, a canonical UUID string becomes a
  `uuid` where `"format": "uuid"` is declared. Nothing else converts, no type is guessed from content, and
  reads never coerce. An undeclared location is not coerced.
- **Validation of the result.** The complete resulting top-level property is then validated:
  nested types, nested constraints (`minimum`, `maximum`, `minLength`, `maxLength`, `pattern`,
  `minItems`, `maxItems`, `enum`, `const`), and the nested `required` keys (they must be present
  in every present object, in `open` and `closed` alike, because an object missing a required key
  does not conform to its declared type). Siblings that were already stored are validated but not
  re-coerced.
- **Unions.** A value is valid when one `anyOf` member (or one type of a type array) accepts it.
  When a write path runs through members that declare the location with different types, the
  member the written value already conforms to exactly wins; with none exact the write is refused
  with a type mismatch listing the candidates (`union<int64|string>`). Ambiguity is never resolved
  by guessing a coercion.
- **Whole objects.** `property("meta", #{..})` and `property_json(..)` run the same nested
  validation (a plain object value is checked at any depth). Without a schema nothing changes.
- **Removal.** `remove_property(jpath(..))` that leaves the property violating the schema (a
  required nested key removed, an array below `min_items`) is refused with the schema error; a
  removal of something that is not there is a no-op and never errors.
- **Errors.** The schema errors keep their variants (`TypeMismatch`, `UndeclaredProperty`,
  `MissingRequiredProperty`, `CoercionFailed`, `ConstraintViolation`) and carry the canonical path
  (`$.a.b[1].c`) as the property name; their help texts read the location with `jpath(..)`.
- **Inference and patches.** An inferred schema (`infer_schema`) and a schema changed with
  `patch_schema` describe nested types the same way (`properties`/`items` at any depth), so nested
  writes are enforced against them.

```rhai
// schema: P.meta = {"type": "object", "properties": {"k": {"type": "integer"},
//                     "w": {"type": "number"}}, "required": ["k"]}
g.V("a").property(jpath("meta.w"), 3)          // stored as 3.0
g.V("a").property(jpath("meta.k"), "x")        // error: Type mismatch on vertex 'P.$.meta.k'
g.V("a").property(jpath("meta.extra"), 1)      // Closed: error, Open: stored
g.V("a").remove_property(jpath("meta.k"))      // error: Missing required property '$.meta.k'
```

## `json_path()` vs `jpath`

`json_path("a.b[1]")` is a standalone step: the path is a string argument and it navigates
whatever the stream carries (a vertex or edge property, or a materialized map). A `jpath` key is
the same grammar used as the name of the property inside the step that reads it (`values`, `has`,
`by`, ...). Both resolve a single target, share one parser, and a missing target produces
nothing. `json_path()` is unchanged and does not take a `jpath(..)` object.

## Errors

| Error | When | Fix |
|---|---|---|
| `InvalidJsonPath` (script error at `jpath(..)`) | the text is not a singular query: `a..b`, `a[*]`, `$..x`, an unclosed bracket, `$`, `""` | the help lists the grammar |
| `InvalidPropertyPath` + `FirstSegmentNotName` | write path starts with an index, `jpath("[0]")` | start with the property name |
| `InvalidPropertyPath` + `KeyOnNonObject` / `IndexOnNonArray` | a write meets a value of the wrong kind | write a whole value at a shorter path first |
| `InvalidPropertyPath` + `IndexOutOfRange` / `MissingArray` | `[n]` beyond `[len]`, or an index where the array does not exist | `[len]` appends, create the array first |
| `InvalidPropertyPath` + `CardinalityWithPath` | `property(Cardinality.x, jpath(..), v)` | drop the cardinality |
| `ArgumentMismatch` | a key that is neither a string nor a `jpath(..)` (`property(5, 1)`) | pass a string or `jpath(..)` |
| `InvalidModulator` | `glob_path().by(jpath(..))` | `by("name")` |
| schema errors (`TypeMismatch`, `UndeclaredProperty`, `MissingRequiredProperty`, `CoercionFailed`, `ConstraintViolation`) | a nested write or removal violates the declared schema | the property name in the error is the canonical path |
| cast error `got type 'pathProperty'` | `drop()` / navigation on `properties(jpath(..))` | `remove_property(jpath(..))` |

Reads never fail on a bad path target (see [Missing paths](#missing-paths)); only the text of the
path itself can be invalid. Every error above carries a `help()` with a runnable example.

## For storage implementers

A `GraphStorage` outside the crate sees a parsed path as `JsonPath::segments()`: a slice of
`JsonPathSegment`s (`Key(name)` for a dot or bracket name, `Index(i)` for an index, negative
counting from the end; the leading `$` is not a segment; the enum is `#[non_exhaustive]`). It
does not need to walk them itself: `graphersal::storage` has the helpers the trait defaults and
`TraversalGraph` use, `path_property_name`, `write_path` (on a copy of the top-level property)
and `write_path_in` (in place), `remove_path` / `remove_path_in`, with exactly the
`InvalidPropertyPath` faults of the table above, and `check_property_value` for the written
value. A path is built with `"meta.tags[-1]".parse::<JsonPath>()`.

## Performance

- A plain name builds exactly the step it always built (identical `.profile()` text, same
  optimizer behaviour). A path key builds a separate step with the same name (`has`, `values`, ...)
  and is never fused or pushed into an index: `has(jpath(..)).count()` shows `has(jpath("$.a.b"), v)`
  then `count()`, with no `count_only`. A [property index](property_indexes.md) orders
  top-level values only, so it never serves a jpath key.
- Reads descend into the stored property by reference and clone only the leaf, never the whole
  top-level object or array, per traverser. A one-segment path on a vertex keeps the lazy property
  handle.
- Writes and removals mutate the stored value in place when the graph has no schema. With a schema
  active they work on a copy of the top-level property so the complete result can be validated.
- `.profile()` shows the optimized plan with the canonical path text, `[path: ..]` annotations are
  unrelated (those are traverser paths).

## Groovy to Rhai

| Gremlin / earlier Graphersal | Rhai DSL |
|---|---|
| `values('a.b')` (a literal key) | `values("a.b")` is still the literal name; a path is `values(jpath("a.b"))` |
| `has('k', 1)` | `has("k", 1)`; nested: `has(jpath("meta.k"), 1)` |
| `valueMap('a')` | `valueMap("a")` / `valueMap(jpath("meta.k"))` (key `$.meta.k`) |
| `by('age')` | `by("age")` / `by(jpath("meta.k"))` |
| `property('a', v)` | `property("a", v)` / `property(jpath("meta.tags[0]"), v)` |
| `properties('a').drop()` | `remove_property("a")` / `remove_property(jpath("meta.k"))` |
| `.json_path("$.a.b")` | unchanged, a string argument, no `jpath(..)` object |

The camelCase spellings (`hasNot`, `valueMap`, `elementMap`, `removeProperty`) and their snake_case
twins (`has_not`, `value_map`, `element_map`, `remove_property`) both take a `jpath(..)`. Rust:
`g.values(jpath)`/`g.has(key, v)`/`g.property(key, v)` take `impl Into<PropKey>` (so `"a"`,
`String` and a parsed `JsonPath` all work).
