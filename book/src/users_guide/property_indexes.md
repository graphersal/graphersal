# Property Indexes

`g.v().has_label("person").has("title", "CEO")` reads every `person` vertex and compares its
`title`. On a large graph that dominates the query time even when only a few vertices match. A
**property index** orders the elements of one label by the value of one property, so the same
query reads only the matching vertices.

```rhai,graph=modern,script
// Declare: the `person` vertices, ordered by `age`.
g.create_index(#{label: "person", key: "age"});

// Served: equality, P.within and ranges on `age` after has_label("person").
g.v().has_label("person").has("age", 29).values("name").to_list();
g.v().has_label("person").has("age", P.between(30, 40)).values("name").to_list();

g.indexes()                                  // the indexes and what they hold
```

Indexes are **declared, never automatic**: nothing is indexed until you ask for it, and an index
costs memory and a little time on every write of its key (below).

## Declare, list, drop

| DSL | Rust (`GraphTraversalSource`) | Python (`Graph`) |
|---|---|---|
| `g.create_index(#{label, key, element?, name?, type?, transform?})` / `g.create_index(..)` | `create_index(name, PropertyIndexDefinition::new(kind, label, key))`, `.with_type(PropertyIndexType::MemoryOrdered)`, `.with_transform(Some(PropertyIndexTransform::ToLower))` | `graph.create_index(label, key, element="vertex", name=None, type="memory-ordered", transform=None)` |
| `g.indexes()` | `list_indexes()` | `graph.indexes()` |
| `g.drop_index(name)` | `drop_index(name)` | `graph.drop_index(name)` |

- `element` is `"vertex"` (the default) or `"edge"`; `label` is one vertex or edge label (it need
  not exist yet); `key` is one **top-level** property name (a plain name, not a jpath: nested
  values are not indexed).
- `name` defaults to `<label>_<key>`, then `_edge` for an edge index, then `_to_lower` for an
  index with the [transform](#case-insensitive-search) `to_lower` (`person_name_to_lower`,
  `knows_note_edge_to_lower`), with every character that is not a letter, a digit or `_`
  replaced by `_`, and `index_` in front when it would not start with a letter. It is an
  identifier like a saved query's name. The DSL, the session, the dev server, MCP and Python use
  this default; the playground's form shows it as the name's placeholder and follows the
  **Transform** choice until a name is typed.
- `type` is the [index type](#index-types), `"memory-ordered"` by default (the only type this
  version knows).
- `transform` is the [transform](#case-insensitive-search) of the index keys: absent (the
  default) for the values themselves, `"to_lower"` for case-insensitive search.
- One index per `(element, label, key, type, transform)`: a second one under another name fails
  with a *Conflicting property index* error that names the first. A plain index and a `to_lower`
  index on the same key coexist (their default names differ). Declaring the same name again
  replaces the index.
- `g.indexes()` lists each index with its `type`, its `transform` (`()` without one), `served`
  (whether it is served, with a `reason` when it is not: see
  [unknown types](#an-index-type-from-a-newer-version) and [other storages](#other-storages)) and
  its current `entries` (indexed elements), `distinct_values`, `skipped` (elements whose non-null
  value the index does not hold) and `bytes` (an estimate of its memory).

```rhai,graph=modern,script
g.create_index(#{element: "edge", label: "knows", key: "weight"});
g.e().has_label("knows").has("weight", P.gt(0.6)).count().next()
```

## Index types

Every index has a **type** (the key `type`): how it keeps its entries, which values it holds and
which conditions it serves. This version knows one type, and it is the default:

| Type | What it is | Serves |
|---|---|---|
| `memory-ordered` | an ordered index kept in memory; its entries are derived from the data and rebuilt whenever the graph is loaded ([below](#persistence)) | equality, `P.within` and the ranges ([below](#what-is-served)) |

Which value types an index holds (Rust: `PropertyIndexType::holds` for the type,
`PropertyIndexTransform::accepts` for the transform, both combined by
`PropertyIndexDefinition::holds`, the one place the engine and the front ends take it from):

| Value type | `memory-ordered` | `memory-ordered` with `to_lower` |
|---|---|---|
| `null`, `boolean` | yes | no |
| `int64`, `float64` | yes (ranged by their value as `float64`) | no |
| `string` (also a [compressed](compressed_properties.md) one, by its text) | yes | yes, by its lowered text |
| `uuid` | yes | no |
| `date`, `localdatetime`, `datetime`, `time`, `duration` | yes | no |
| `array` (a list), `object` (a map) | no | no |
| `vector` | no ([vectors](vectors.md) are searched with `nearest()` and `similarity()`) | no |

A value the index does not hold is skipped: the element is not in the index, and a condition on
such a value is answered by a scan, so results never change.

```rhai,graph=modern
g.create_index(#{label: "person", key: "age", type: "memory-ordered"})   // the default, spelled out
g.create_index(#{label: "person", key: "age", type: "hash"})   // refused: not a type this version knows
```

### Checked against the schema when created

When the graph's stored [schema](schema_enforcement.md) declares the key at the top level of the
index's label (a vertex label for a vertex index, an edge label for an edge index) and **none** of
the declared types, `null` aside, can be held by the index (for the type a vector, a list, a map,
or an `anyOf` of only such; with the transform `to_lower` every type but `string`), creating the
index is refused, with a message that names the label, the
key, the declared type and the index type:

```rhai,graph=empty
// refused: the schema declares `embedding` as a vector
{
  g.set_schema(#{mode: "open", vertices: #{doc: #{schema: #{type: "object", properties: #{
    embedding: #{type: "array", format: "vector", element: "f32"}}}}}});
  g.create_index(#{label: "doc", key: "embedding"})
}
```

```text
Error: Cannot create a property index on vertex 'doc' key 'embedding': the schema declares
'embedding' as vector<f32>, and an index of type 'memory-ordered' holds none of its values
Help: Vectors are not index keys: search them with nearest() or similarity() (an exact search,
book page "Vectors"), for example g.v().has_label("doc").nearest("embedding", query, 10) for the
10 closest; a vector index type is planned. ...
```

With `to_lower` the message says `an index of type 'memory-ordered' with the transform
'to_lower' holds none of its values`, and the help suggests a plain index or a string property.
For a list or a map the help suggests indexing a scalar top-level property instead (an index key
is one top-level name; nested fields are not indexed). A declaration with at least one type the
index holds (`anyOf` of a list and a string, `["string", "null"]`), a key the schema does not
declare, a label it does not declare, and a graph without a schema are all accepted. The schema's
mode does not matter (a declaration in mode `none` counts too). The playground's form shows the
refusal under its **Key** field.

The check runs when an index is **created** (the DSL, `GraphTraversalSource::create_index`,
Python, the playground, the dev server and MCP), never when a definition is stored as it is:
`set_definition`, `apply_change_set`, a replay or recovery, a catalog file or a snapshot load
keep what they carry. A **later schema change never drops an index**: an index whose key the
schema later declares as a vector stays and simply holds nothing of it.

### An index type from a newer version

A store (or a catalog file, a change set) written by a newer Graphersal may hold an index of a
type this version does not know. Such a definition is **never damage**: it is kept in the catalog
exactly as read and written back unchanged (its payload byte for byte through the journal,
checkpoints, backups, forks and snapshots), but it is **not served**: no entries are built, the
optimizer never reads it, and `g.indexes()` (and the playground, the dev server and MCP's
`list_indexes`) shows it with `served: false` and a `reason` such as *index type 'x' is not
known to this version*. It can be dropped like any index. Creating or redefining an index of an
unknown type is refused with an error whose help lists the known types. Indexes of different
types may cover the same `(element, label, key)`. A **transform** this version does not know is
handled the same way: kept byte for byte, built and served never, listed with the reason *index
transform 'x' is not known to this version*, refused when created (the field `transform`). So is
an **element kind** other than `vertex` and `edge` (since 0.2.1; `g.indexes()` shows the name as
read, the reason *element kind 'x' is not known to this version*, refused when created with the
field `element`).

## Case-insensitive search

An index can carry a **transform** (the key `transform`): its keys are then the transformed
values, and it serves a condition that applies the same transform before comparing. This version
knows one transform, `to_lower`, named after the step [`to_lower()`](../traversal/steps/to_lower.md):
the index key is the lowered text of a **string** value, computed by exactly the function the
step uses (Rust's `str::to_lowercase`: Unicode, no locale), so the index and the scan never
disagree. The query form is TinkerPop's `has(key, traversal)`: `has("name", __.to_lower().is(x))`
keeps the elements whose lowered `name` is `x`.

```rhai,graph=modern,script
g.v("1").property("name", "MARKO").iterate();
g.create_index(#{label: "person", key: "name", transform: "to_lower"});   // person_name_to_lower

// Served: the lowered value against the constants, equality, P.within and the ranges.
g.v().has_label("person").has("name", __.to_lower().is("marko")).values("name").to_list();
g.v().has_label("person").has("name", __.to_lower().is(P.within("josh", "vadas"))).count().next();
g.v().has_label("person").has("name", __.to_lower().is(P.gte("p"))).values("name").to_list();

// The constant is compared AS GIVEN with the lowered value: write it in lower case.
g.v().has_label("person").has("name", __.to_lower().is("Marko")).count().next()   // 0
```

What is served and what is not:

- Served: `v()`/`e()` with a folded `has_label(..)` (every label with a `to_lower` index on the
  key), followed in the run of pure filters by `has(key, __.to_lower().is(P))` with `P` a value,
  `P.eq`, `P.within` or a range (`P.gt/gte/lt/lte/between/inside/outside`). `.profile()` shows
  `[index: vertex person.name to_lower]` on the start step; the `has` step stays in the plan and
  re-checks every candidate.
- Not served (a scan, the same result): any other child (`__.is("x")` without `to_lower()`,
  `__.to_upper()`, `__.to_lower(Scope.local)`, more steps after `is(..)`, `P.neq`, text
  predicates), `where(__.values("name").to_lower().is(..))`, a jpath key, a source without
  `has_label`. The constant is never lowered for you: `is("Marko")` matches nothing.
- Only strings are in the index; `null` and a missing key are not. Because `to_lower()` fails on
  any other value (an `int64` name, a list), such a value must make the query fail as the scan
  does: while the index has **skipped** a non-null, non-string value (`g.indexes()` shows
  `skipped`), queries do not read it and scan, with the step's error. Remove or convert those
  values and the index is served again.
- A plain index and a `to_lower` index on the same key coexist: `has("name", "Marko")` reads the
  plain one, `has("name", __.to_lower().is("marko"))` the lowered one. A `to_lower` index never
  serves a plain `has("name", "marko")`.
- Creating one on a key the schema declares as a non-string type only (an `integer`) is refused
  with a message naming the transform; a union with `string` is fine
  ([below](#checked-against-the-schema-when-created)).
- An unknown transform (written by a newer version) is kept byte for byte and listed as not served
  ([below](#an-index-type-from-a-newer-version)).

## What is served

A `v()`/`e()` start step reads an index when

- it is the **start** of the traversal (or of a child traversal), the optimizer folded a
  `has_label(..)` into it, and **every** folded label has an index on the key: the index of
  `has_label("person")`, or of `has_label("person", "employee")` when both labels have one;
- and a `has()` on the indexed key follows directly (only `has()`, `has_label()`, `has_id()`,
  `has_key()`, `has_not()` in between):

| Condition | Served |
|---|---|
| `has(key, value)` | equality |
| `has(key, P.eq(v))`, `has(key, P.within(a, b, ..))` | one slice per value |
| `has(key, P.gt/gte/lt/lte(v))` | one range |
| `has(key, P.between(a, b))`, `P.inside(a, b)` | one range |
| `has(key, P.outside(a, b))` | two ranges |
| `P.neq`, `P.without`, text predicates, `P.typeOf`, `and`/`or`/`not` composites | not served (scan) |
| a value that is a list or a map (`has("tags", ["a"])`) | not served (scan) |
| `v().has("title", "CEO")` without a label, a jpath key | not served (scan) |

With several served conditions the optimizer picks the **most selective** index from the index
statistics (an equality reads about `entries / distinct_values` elements of each label, a range
about half the entries).

**Results are the same as without the index**, always: the index returns a superset of the
matching elements (numbers are ranged by their value as `float64`, so `29` and `29.0` are both
read), and the start step and the `has()` step still check every condition on each candidate. The
comparison is exactly that of `has()`: `has("age", 29)` matches an `int64` 29 only,
`has("age", P.eq(29.0))` and the ranges compare numbers across `int64` and `float64`, a string never
equals a uuid, `null` equals only `null`. Dates and times are index keys with the same rules
([Comparing and Sorting](../dates/comparing.md)): each of `date`, `localdatetime`, `datetime`,
`time` and `duration` is its own kind (a `date` never equals a `datetime` or a string, and a range
reads only values of its bounds' type), a `datetime` is keyed by its instant (`12:00Z` and
`14:00+02:00` are one key), and a `duration` is keyed by all its fields (`P1M` and `P30D` are two
keys) and ranged by its length, a month as 30 days, ties broken by the fields: equality,
`P.within` and every range on durations are served. Only the **order** of the elements may differ (the index
reads them by value): use `order()` when the order matters.

A string operand of `has(key, P)` is always a value, also inside a child traversal and after an
`as()` of the same spelling (only `where(P.eq("a"))` names a path label), so `P.eq("a")` is served
wherever the start step is.

## See it in `.profile()`

The start step names the index it reads, and the rule `property_index` is listed:

```console
$ graphersal -e 'g.create_index(#{label: "person", key: "age"})' \
             -e 'g.v().has_label("person").has("age", P.gt(30)).values("name").profile()'
Traversal Metrics
Step                                                         Call      In     Out       Time    % Dur
=====================================================================================================
v(labels: ["person"]) [index: vertex person.age]                1       0       2   55.666µs     7.53
has("age", P.gt(30))                                            1       2       2   42.625µs     5.77
values("name")                                                  1       2       2    1.834µs     0.25
                                                      TOTAL:             execute:  738.958µs    13.55
=====================================================================================================
Optimizer rules applied: source_filter_pushdown, property_index
```

An equality folded into the start step shows as
`v(labels: ["person"]).has("title", "CEO") [index: vertex person.title]`. To compare with the scan,
turn the rule off for one query: `g.with("optimizer.disabled", ["property_index"])` (see
[Property Index Rule](../optimizer/property_index.md)).

## Kept exact

An index is maintained on every write: adding and dropping elements (a vertex drop takes its edges
out of the edge indexes), writing and removing the key (also through a jpath write that changes
it), adding and removing a vertex label (a vertex with several labels is in the index of each of
them), changing an edge label. A rolled-back unit restores the index with the data; `replay`,
`apply_change_set` and a store's recovery rebuild it exactly. A string kept
[compressed](compressed_properties.md) is indexed by its text (the index holds the plain text, so
indexing a long compressed field costs the memory compression saved).

## Persistence

An index is a catalog definition (kind `property_index`, like [saved queries](saved_queries.md)
and compression rules), so its **declaration** travels with the graph: the journal (WAL), snapshots,
backups, forks, rollbacks and the catalog file. Its **entries are never stored**: they are rebuilt
from the data whenever a graph is loaded, by a store open, a read-only view (also of a past state),
a backup open, a recovery, a snapshot load. A read-only view is therefore always consistent with
the data it shows. GraphML and GraphSON files carry no catalog, so no index.

## Memory

Each indexed element costs one entry: its value (strings longer than 24 bytes on the heap) plus
about 50 bytes of ordered-set bookkeeping. `g.memory_usage()` reports the indexes as
`property_index_bytes` (part of `total_bytes`, so the [memory budget](resource_limits.md) sees
them), `g.indexes()` per index. Building an index reads every element of its label once.

## Other storages

Indexes are a capability of the storage (`StorageCapabilities::property_indexes`); the built-in
graph has it. On a [custom storage](../for_developers/custom_storage.md) without it,
`create_index` fails with *unsupported: property indexes*; an index definition can still be stored
in its catalog (a catalog file loaded into it, say) and is listed with `served: false`, but no
query reads it. A storage that serves indexes implements `GraphStorage::property_indexes` and the
lookups `vertices_by_property_index`/`edges_by_property_index`, and for indexes with a transform
`vertices_by_transformed_property_index`/`edges_by_transformed_property_index` (reporting
`PropertyIndexStats::skipped`, which the optimizer reads before it uses such an index).

## In the playground, the dev server and MCP

- **Playground** (WebAssembly and over the dev server): **Catalog ▾ ▸ Property indexes** lists the
  indexes with their type, transform, entries, distinct values, elements per value (what an
  equality reads) and memory (an index that is not served says why), creates one from a form
  (element, label, key, type, transform: none or `to_lower` with a hint on the query form,
  optional name; the default name is shown as you type and follows the transform; an engine
  refusal shows under its field) and drops one after a confirmation. The **Statistics** tab of the Profile panel
  shows *Property indexes* with their memory when the graph has some. Indexes travel with the
  catalog file (**Catalog ▾ ▸ Save catalog**).
- **Dev server**: `GET /api/indexes`, `POST /api/indexes/create` `{label, key, element?, name?, type?, transform?}`,
  `POST /api/indexes/drop` `{name}` (see [the dev server](../persistence/dev_server.md)); a
  create or drop is a commit recorded in the change feed, so open browsers reload. The start
  block on stderr names the index memory.
- **MCP**: the tools `list_indexes` (also with `--mcp-read-only`), `create_index` (with an
  optional `type` and `transform`, enums of the known ones) and `drop_index` (not offered read-only, where
  `query` refuses `g.create_index` too).

## Limits

- Not indexed: nested values (jpath keys), lists, maps and vectors as values (see
  [index types](#index-types)), labels other than the one declared (an index on `person` does not
  serve `v().has("title", ..)` without `has_label`).
- One index type (`memory-ordered`): persisted, hash, vector (approximate nearest neighbour) and
  full-text types are planned and will be additional values of `type`.
- One transform (`to_lower`); it serves only the exact child `__.to_lower().is(P)`. A `to_lower`
  index does not serve a plain `has(key, value)`, and `TextP` predicates are not served.
- No unique indexes (constraints) in this version.
- `order().by(key)` does not read the index.
