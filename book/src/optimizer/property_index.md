# Property Index Rule

Rule name: `property_index` (on by default, sixth in the pipeline, right after
[Source Filter Pushdown](source_filter_pushdown.md)).

## What it rewrites

Nothing in the plan's shape: it records on a `v()`/`e()` start step which
[property index](../users_guide/property_indexes.md) the step reads instead of its label buckets.
`.profile()` shows the choice on the start step:

```rhai
v().has_label("person").has("title", "CEO")      →   v(labels: ["person"]).has("title", "CEO") [index: vertex person.title]
v().has_label("person").has("age", P.gt(30))     →   v(labels: ["person"]) [index: vertex person.age]
                                                      has("age", P.gt(30))
e().has_label("knows").has("weight", P.lt(0.5))  →   e(labels: ["knows"]) [index: edge knows.weight]
                                                      has("weight", P.lt(0.5))
v().has_label("person")                          →   v(labels: ["person"]) [index: vertex person.name to_lower]
   .has("name", __.to_lower().is("ada"))              has("name", __.to_lower().is(P.eq("ada")))
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
  child's `to_lower()` fails on a non-string value and the scan must report that error. Any
  other `has(key, traversal)` ends the run of pure filters.

A condition is covered when every label folded into the start step has an index on its key (of
the condition's transform: none for `has(key, ..)`, `to_lower` for the lowered form). Of the
covered conditions the rule picks the most selective by the indexes' statistics (an equality: about
`entries / distinct_values` elements per label and value; a range: half the entries).

## Why

The start step then reads only the elements whose value lies in the slices of the index, instead
of every element of its labels. The conditions are not removed: the start step still checks its
folded conditions on each candidate and the `has(key, P)` step stays in the plan, so the result
equals the scan's (only the order of the elements may differ).

## When it does not fire

- No index is declared (the rule then costs nothing), or one of the folded labels has none on the
  key. Only indexes the storage serves count: an index of a
  [type](../users_guide/property_indexes.md#index-types) this version does not know (written by
  a newer version) is kept in the catalog but never read.
- The start step has no folded label (`v().has("title", "CEO")`), has ids folded
  (`v("1").has_label(..)`: the ids are cheaper), or a fused `count()`/`group_count()`.
- The condition is not served: `P.neq`, `P.without`, text predicates, composites, a list or map
  value, a jpath key, `NaN`; a `has(key, traversal)` whose child is not exactly
  `to_lower().is(P)`; the lowered form while its index skipped a non-string value.

`g.with("optimizer.disabled", ["property_index"])` turns it off for one query, for example to
compare the plan with the label scan.
