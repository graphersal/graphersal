# Count Pushdown Rule

Rule name: `count_pushdown` (on by default, eighth in the pipeline).

## What it rewrites

A `count()` directly behind a leading `v()` or `e()` is fused into the start step, which then
computes the number without producing a traverser per element:

```rhai
v().count()                        →   v().count()            (one fused step)
e().has_label("knows").count()     →   e(labels: ["knows"]).count()
v("1", "2", "99").count()          →   v("1", "2", "99").count()
```

How the fused step counts:

| Start step | Source of the number |
|---|---|
| `v()` / `e()` without ids or labels | the graph's element count, no iteration at all |
| with labels (`v(labels: ...)`) | the label index; a vertex carrying several of the labels counts once |
| with ids (`v("1", "2")`) | one lookup per id; ids that do not exist (or, with labels, whose element carries none of them) are not counted |
| with one folded `has(key, value)` and an exact [index access](property_index.md) | the entries of the index slice (`GraphStorage::count_by_property_index`), no element visited; `.profile()` shows `[count from index]` |

The `count()` does not have to be the last step: `v().count().is(6)` fuses as well, and the
following steps run on the single count value.

## Why

`v().count()` on an unfiltered graph becomes a constant-time read instead of a scan.

## When it does not fire

- Any step between the start step and `count()` that was not folded into the start step:
  `v().out().count()`, `v().has("age", P.gt(30)).count()`, `v().as("a").count()`, a second
  `has_label`.
- A start step with folded `has(key, value)` conditions, unless the
  [Property Index Rule](property_index.md) marked its index access exact (see the table).
- The traversal does not start with `v()`/`e()`.
- `count(Scope.local)`: it counts the elements of a collection, not the stream.
- The start step was already fused with a count per label by
  [Group Count Pushdown](group_count_pushdown.md).

## Example

On the `large` graph (111110 vertices):

```console
$ graphersal --graph large -e 'g.v().count().profile()'
Traversal Metrics
Step                                                         Call      In     Out       Time    % Dur
=====================================================================================================
v().count()                                                     1       0  111110    3.125µs     4.45
                                                      TOTAL:             execute:   70.208µs     4.45
=====================================================================================================
Optimizer rules applied: count_pushdown

$ graphersal --graph large -e 'g.with("optimizer.disabled", ["count_pushdown"]).v().count().profile()'
Traversal Metrics
Step                                                         Call      In     Out       Time    % Dur
=====================================================================================================
v()                                                             1       0  111110    1.557ms    98.25
count()                                                         1  111110       1        0ns     0.00
                                                      TOTAL:             execute:    1.585ms    98.25
=====================================================================================================
```

The fused step's `Out` column shows the counted number (111110), not the one traverser that carries
it.

On the modern graph, combined with [Source Filter Pushdown](source_filter_pushdown.md):

```console
$ graphersal -e 'g.e().has_label("knows").count().profile()'
Traversal Metrics
Step                                                         Call      In     Out       Time    % Dur
=====================================================================================================
e(labels: ["knows"]).count()                                    1       0       2    1.667µs     5.31
                                                      TOTAL:             execute:   31.417µs     5.31
=====================================================================================================
Optimizer rules applied: source_filter_pushdown, count_pushdown
```

A count that is not directly behind the start step stays a separate step:

```console
$ graphersal -e 'g.v().has("age", P.gt(30)).count().profile()'
Traversal Metrics
Step                                                         Call      In     Out       Time    % Dur
=====================================================================================================
v()                                                             1       0       6    1.875µs     4.97
has("age", P.gt(30))                                            1       6       2    5.375µs    14.25
count()                                                         1       2       1        0ns     0.00
                                                      TOTAL:             execute:   37.708µs    19.23
=====================================================================================================
```

## Results

Unchanged: the fused step returns the same number as the scan and the count (`4` for
`g.v().has_label("person").count()`, with and without the rule).

In a child traversal that receives several traversers at once, every incoming traverser stands for
its own pass over the graph, so the fused count is multiplied by the number of incoming traversers,
as the unfused plan counts them: `g.v().has_label("software").union(__.v().count())` returns `12`
(2 x 6) either way.

## Interaction with other rules

Runs after [Source Filter Pushdown](source_filter_pushdown.md) and [Has ID Pushdown](has_id_pushdown.md),
whose folded labels and ids it keeps, and after [Group Count Pushdown](group_count_pushdown.md),
which it leaves alone. [Where Unnest](where_unnest.md) and [Filter Reorder](filter_reorder.md)
indirectly widen its reach: `v().where(__.has_label("person")).count()` ends up as
`v(labels: ["person"]).count()`.
