# Property Index Rule

Rule name: `property_index` (on by default, sixth in the pipeline, right after
[Source Filter Pushdown](source_filter_pushdown.md)).

## What it rewrites

It records on a `v()`/`e()` start step which
[property index](../users_guide/property_indexes.md) the step reads instead of its label buckets,
and, for a condition the index answers [exactly](#exact-accesses), drops that condition's
re-check. `.profile()` shows the choice on the start step:

```rhai
v().has_label("person").has("title", "CEO")      →   v(labels: ["person"]).has("title", "CEO") [index: vertex person.title, exact]
v().has_label("person").has("age", P.gt(30))     →   v(labels: ["person"]) [index: vertex person.age]
                                                      has("age", P.gt(30))
e().has_label("knows").has("weight", P.lt(0.5))  →   e(labels: ["knows"]) [index: edge knows.weight]
                                                      has("weight", P.lt(0.5))
v().has_label("person")                          →   v(labels: ["person"]) [index: vertex person.name to_lower]
   .has("name", __.to_lower().is("ada"))              has("name", __.to_lower().is(P.eq("ada")))
v().has("name", "josh")                          →   v().has("name", "josh") [index: vertex name, any label, exact]
v().has("name", "josh").count()                  →   v().has("name", "josh").count() [index: vertex name, any label, exact] [count from index]
v().has("_owners", __.unfold().is("o1"))         →   v().has("_owners", __.unfold().is(P.eq("o1"))) [index: vertex _owners each, any label, exact]
v().has("_owners", __.unfold().is(P.gt("o1")))   →   v() [index: vertex _owners each, any label]
                                                      has("_owners", __.unfold().is(P.gt("o1")))
v().has_label("doc")                             →   v(labels: ["doc"]) [index: vertex doc.tags each to_lower]
   .has("tags", __.unfold().to_lower().is("x"))       has("tags", __.unfold().to_lower().is(P.eq("x")))
```

It looks at

- the `has(key, value)` conditions [Source Filter Pushdown](source_filter_pushdown.md) folded into
  the start step (scalar values), and
- the `has(key, P)` steps in the run of pure filters (`has`, `has_label`, `has_id`, `has_key`,
  `has_not`) right after it, with `P.eq`, `P.within`, `P.gt/gte/lt/lte`, `P.between`, `P.inside`
  or `P.outside`, and
- the `has(key, __.to_lower().is(P))` steps in that run (TinkerPop's `has(key, traversal)` with
  exactly that child, `P` as above), which only an index with the
  [transform](../users_guide/property_indexes.md#case-insensitive-search) `to_lower` on the key
  covers. Its slices are `P`'s constants as given (the child compares the lowered value with
  them); it is covered only while that index has skipped no value (`skipped` 0), because the
  child's `to_lower()` fails on a non-string value and the scan must report that error. A
  `null` constant is not covered (`to_lower()` passes a `null` through, the index holds none).
- the `has(key, __.unfold().is(P))` steps in that run, covered by an index with the transform
  [`each`](../users_guide/property_indexes.md#arrays-the-each-transform) (one entry per list
  element) whatever it skipped (a nested list, a map's entries or a vector that `unfold()`
  emits never equals or ranges with a scalar), and `has(key, __.unfold().to_lower().is(P))` by
  `["each", "to_lower"]` while it skipped nothing. Any other `has(key, traversal)` ends the run
  of pure filters.

A condition on the whole value (a folded `has(key, value)`, a `has(key, P)`) with no index of no
transform on the key is covered by an `each` index as a SUPERSET: lists holding the value are
candidates and the re-check of the whole value drops them; such an access is never exact.

A condition is covered when every label folded into the start step has an index on its key (of
the condition's transform: none for `has(key, ..)`, `to_lower` for the lowered form), or when a
[label-less index](../users_guide/property_indexes.md#label-less-indexes) on the key exists: a
start step without labels can only read a label-less index; one with labels compares the sum
of its per-label estimates with the label-less index's estimate, the lower wins (a tie keeps the
per-label indexes), and with the label-less index it checks each candidate's labels. Of the
covered conditions the rule picks the most selective by the indexes' statistics (an equality: about
`entries / distinct_values` elements per index and value; a range: half the entries). A
[unique index](../users_guide/property_indexes.md#unique-indexes) is read like any other index
(its statistics give an equality about one element; one that is not served, its data holding
repeated values, is never read).

## Exact accesses

An access is **exact** (user decision 2026-11-08) when the index answers the condition it reads
exactly, so no re-check could drop a candidate:

- every slice is a point (an equality, or each operand of `P.within`) of a string, boolean, uuid
  or temporal value: a number slice spans `int64` and `float64` (a superset), and `null` and
  ranges keep their re-check;
- the index has no transform (a condition on the whole value), or exactly `each` reading
  `has(key, __.unfold().is(..))` (an element is in the postings of each of its values once; the
  start step deduplicates several slices); not an `each` index read as a superset, and not
  `to_lower` or `["each", "to_lower"]` (a transform that can fail keeps its re-check);
- the query does not write at any level (no mutating step, in a child traversal neither: a write
  could change what the index holds while the plan reads it);
- the storage claims `StorageCapabilities::exact_property_index_lookups` (the built-in graph, its
  read-only views and the wrappers that forward it do; a custom storage's lookups otherwise
  count as supersets).

Then the re-check of that one condition is dropped: a folded `has(key, value)` is no longer
checked on the candidates (the start step still shows it), a `has` step leaves the plan and the
start step shows it after its folded conditions, and the index marker reads `, exact`. Every other
condition is still checked, and so are the labels of a label-less index's candidates under a
start with labels. When nothing else is left (one slice, no other folded condition, and a
label-less index under a start without labels or the index of the start's single label) the
access is also countable, and [Count Pushdown](count_pushdown.md) may fuse a following `count()`
into the start step, which the storage then answers from the index
(`GraphStorage::count_by_property_index`) without visiting the elements: `.profile()` shows
`[count from index]`. Should the storage not serve the index after all, the start step scans and
checks the answered condition itself, as the index keys it.

## Why

The start step then reads only the elements whose value lies in the slices of the index, instead
of every element of its labels. The conditions it does not answer exactly are not removed: the
start step still checks its other folded conditions on each candidate and their `has(key, P)`
steps stay in the plan, so the result equals the scan's (only the order of the elements may
differ). An exact condition is not checked twice: the index is maintained exactly on every write
and rollback, it is trusted, and a wrong entry would show in the result instead of being hidden
by a re-check (the randomized differential tests compare every exact shape with the scan).

## When it does not fire

- No index is declared (the rule then costs nothing), or one of the folded labels has none on the
  key and no label-less index covers it. Only indexes the storage serves count: an index of a
  [type](../users_guide/property_indexes.md#index-types) this version does not know (written by
  a newer version) is kept in the catalog but never read.
- The start step has no folded label (`v().has("title", "CEO")`) and no label-less index covers
  the key, has ids folded (`v("1").has_label(..)`: the ids are cheaper), or a fused
  `count()`/`group_count()`.
- The condition is not served: `P.neq`, `P.without`, text predicates, composites, a list or map
  value, a jpath key, `NaN`; a `has(key, traversal)` whose child is not exactly
  `to_lower().is(P)`, `unfold().is(P)` or `unfold().to_lower().is(P)`; a lowered form while its
  index skipped a non-string value.

`g.with("optimizer.disabled", ["property_index"])` turns it off for one query, for example to
compare the plan with the label scan.
