# Layers

A **layer** is a writable graph over a frozen **base** graph. The base is loaded once and never
changes. The layer stores only its own difference from it: the elements it adds, and the base
elements it modifies or removes. To a query, a layer looks like an ordinary graph. Every step,
saved query, schema rule and transaction works on it the same way, and nothing tells it apart
from a graph that holds all of its data itself.

The typical use is a product graph shared by many projects. The product (the "box") is the
base. Each project gets a layer, and the same indexer scripts that built the product fill it
with the project's own repositories. Most of a project graph is the product itself, so a layer
costs about what the project adds or changes, not a copy of the product. Several layers can
share one base in the same process.

> **EXPERIMENTAL in 0.6.x.** Layers live in memory only: a layer cannot be saved yet (see
> [Limitations](#limitations)). How the storage works inside, its capabilities and memory
> figures are on the developer page [Layered Graphs](../for_developers/layered_graphs.md).

## Where an element comes from

Only some queries need to know whether an element is the product's or the project's. Three
functions answer that question, and only when they are called. Everything else reads the layer
as one graph.

| Function | Example | Answer |
|---|---|---|
| [`layer()`](../traversal/steps/layer.md) | `g.v().has_label("class").layer()` | the origin of each vertex or edge |
| [`has_layer(origins...)`](../traversal/steps/has_layer.md) | `g.v().has_layer("added", "modified")` | only the elements with one of these origins |
| [`g.layer_diff(#{..})`](../traversal/steps/layer_diff.md) | `g.layer_diff(#{kinds: "vertex"})` | the whole difference against the base, as one map |

An element has one of three origins:

- `"base"`: the element is the base's, unchanged. A base element written back to its base
  state, or written with a value equal to the base value, is `"base"` again.
- `"added"`: the layer created it.
- `"modified"`: a base element whose properties or labels the layer changed.

A removed base element cannot be traversed, so `layer()` never answers for it. It appears
only in `g.layer_diff()`, under `removed`.

**On a graph without layers** (every sample graph, file and Store) all three functions still
work: everything is `"base"` and the diff is empty. A query that uses them runs on both kinds
of graph.

```rhai,graph=modern
g.v().layer().dedup().to_list()   // ["base"]: the sample graph is no layer
g.v().has_layer("added", "modified").count().next()   // 0
g.e().has_layer("base").count().next()   // 6
g.v(1).out_e().group_count().by(__.layer()).next()   // #{"base": 3}
g.layer_diff()   // #{"added": [], "modified": [], "removed": [], "truncated": false}
g.layer_diff(#{kinds: "edge", labels: ["knows", "created"], limit: 10})["truncated"]   // false
g.v().has_layer("new").to_list()   // error: "new" is no origin (base, added or modified)
```

`has_layer()` without arguments keeps every vertex and edge; `has_layer([])` keeps none, like
`has_label()`. An unknown origin is an error before the query runs, even inside a child
traversal no traverser reaches. Both steps accept vertices and edges only: anything else (a
value, a property) is an error.

## `g.layer_diff(#{kinds, labels, limit})`

`g.layer_diff()` returns everything the layer changed. All options are optional:

| Option | Value | Default |
|---|---|---|
| `kinds` | `"vertex"`, `"edge"`, or a list of them | both kinds |
| `labels` | a label or a list of labels; an element matches when any label of its base label set or its layer label set is in the list. `[]` matches nothing | every label |
| `limit` | a positive number of entries, counted across the whole diff | no limit |

The answer is a map:

| Key | Value |
|---|---|
| `added` | a list of the elements the layer created: `{kind, id, labels, properties}`, an edge also with `out` and `in` (the endpoint ids) |
| `removed` | a list of the base elements the layer removed, in the same form, with their base labels and properties |
| `modified` | a list of the base elements the layer changed: `{kind, id, base_labels, labels, properties}` (an edge also with `out` and `in`), where `properties` maps each changed key to `{base, layer}`; a key the layer added has no `base`, a key it removed no `layer` |
| `truncated` | `true` when `limit` left out further entries |

An edge's `labels` is a list of one label. The diff lists vertices first (added, removed,
modified), then edges in the same order; added elements are listed in creation order, except
that an element created after a removal may fill the removed element's place. Values are the
logical values, the same ones a query reads. In the DSL the keys of each map come out sorted
(`added`, `modified`, `removed`, `truncated`), because the DSL's map type sorts them. The Rust
form ([`LayerDiff::to_property`](#rust)) and Python keep the order of the table.

A `g.layer_diff()` in a script sees the script's own earlier writes. It reads the delta only,
never every base element, so it is cheap on a large base with a small layer. It asks for
`Read` on `Data` (see [Permissions](permissions.md)).

## Trying a layer from the command line

`graphersal --layer` runs the session on an empty layer over the `--graph`: the loaded graph
becomes the frozen base, and every write goes to the layer.

```bash
graphersal --graph modern --layer \
  -e 'g.add_v("person").property("name", "ada").iterate()' \
  -e 'g.v().has("name", "marko").property("age", 30).iterate()' \
  -e 'g.v(6).drop().iterate()' \
  -e 'g.v().group_count().by(__.layer()).next()' \
  -e 'g.layer_diff(#{kinds: "vertex"})'
```

```text
"added": 1
"base": 4
"modified": 1
"added": [#{"id": "7", "kind": "vertex", "labels": ["person"], "properties": #{"name": "ada"}}]
"modified": [#{"base_labels": ["person"], "id": "1", "kind": "vertex", "labels": ["person"], "properties": #{"age": #{"base": 29, "layer": 30}}}]
"removed": [#{"id": "6", "kind": "vertex", "labels": ["person"], "properties": #{"age": 35, "name": "peter"}}]
"truncated": false
```

- The base can be any `--graph` source: a sample graph, a GraphSON or GraphML file, a packed
  snapshot (`.gsnap`), or a Store. A Store is opened read-only, without its lock, and nothing
  is written into it, so a layer can run over a store another process has open. `--schema`
  applies to the base as without `--layer`.
- The REPL (`graphersal --layer`) and the dev server (`graphersal --graph <src> --layer
  --server`) run on the layer too. The server's start banner has a `layer` row; the playground
  pages and the server's MCP endpoint query the layer. "Save to file" is refused: it would
  write the layer's graph over the base's file.
- The layer lives as long as the process: nothing is saved when it ends.
- `--layer` is refused with `--create-store` (a layer never writes its base, so it cannot
  create a store) and with `--read-only` (the layer is the part that takes the writes), with
  exit code 2. A base the layer cannot carry yet (see below) is refused with exit code 2 too.

## Rust

A layer is the storage `LayeredGraph` over an `Arc<TraversalGraph>`. Everything that runs on a
`Graph<S>` runs on it: the typed traversal API, the DSL, GQL and the session.

```rust
use std::sync::Arc;
use graphersal::prelude::*;
use graphersal::storage::{ElementKind, Graph, LayerDiffRequest, LayerOrigin, LayeredGraph};

let base = Arc::new(TraversalGraph::tinkerpop_modern());
let layer = Arc::new(Graph::new(LayeredGraph::over(Arc::clone(&base)).unwrap()));

// The DSL on the layer: the writes go to the layer, the base stays as it was.
let run = |script: &str| graphersal::script::eval_value(layer.clone(), script).unwrap();
run(r#"g.add_v("person").property("name", "ada").iterate()"#);
run(r#"g.v().has("name", "marko").property("age", 30).iterate()"#);
assert_eq!(run(r#"g.v().has_layer("added", "modified").count().next()"#).as_int(), Ok(2));
assert_eq!(run(r#"g.v().has("name", "ada").layer().next()"#).to_string(), "added");

// The typed API: origins and the diff.
let read = layer.read();
let g = GraphTraversalSource::new(&*read);
let diff = g.layer_diff(&LayerDiffRequest::new().kind(ElementKind::Vertex)).unwrap();
assert_eq!((diff.added.len(), diff.modified.len(), diff.removed.len()), (1, 1, 0));
assert_eq!(diff.modified[0].properties[0].name, "age");
assert!(diff.to_json().starts_with(r#"{"added":"#));
let marko = read.get_vertex_by_id("1").unwrap().unwrap();
assert_eq!(read.vertex_layer(marko).unwrap(), Some(LayerOrigin::Modified));
assert_eq!(TraversalGraph::vertex_count(&base), 6);
```

`LayerDiff::to_property()` is the map of the table above with its keys in that order, and
`LayerDiff::to_json()` its JSON text. A storage that has no layers answers `LayerOrigin::Base`
and an empty diff from the trait's defaults.

## Python

`graph.layer_diff(kinds=None, labels=None, limit=None, policy=None)` answers the diff as a
`dict` with the keys in the order of the table above. `layer()` and `has_layer()` are DSL steps
and run in `graph.query(...)` like every other step.

```python
import graphersal

graph = graphersal.Graph.tinkerpop_modern()
assert graph.query("g.v().layer().dedup().to_list()") == ["base"]
assert graph.layer_diff(kinds="vertex", limit=10) == {
    "added": [], "removed": [], "modified": [], "truncated": False,
}
```

A Python `Graph` cannot be a layer yet: it always holds a graph of its own, so from Python
every element is `"base"` and the diff is empty. The functions are there so that queries and
code written for layers already run.

## Limitations

- **In memory only.** A layer cannot be saved, opened from a Store or journaled yet; it lives as
  long as its process. Persistence of layers is planned.
- **One level.** The base is a plain graph; there is no layer over a layer. The base is frozen
  for good: when the product graph changes, project layers are rebuilt over the new base.
- **Refused for now**, with an error naming the feature ("... not supported on a layered graph
  yet"):
  - a base whose schema has [vector rules](vector_rules.md), and a schema with vector rules
    on a layer;
  - compression rule and embedding model definitions created on a layer.
- **Property indexes:** a layer serves the base's indexes for its own data, also after it
  changed elements: lookups and counts answer from the index, as on a plain graph. The
  base's unique indexes hold for the layer: a write that would give a second element a unique
  value fails with the same error a plain graph with the layer's data gives, and a value that
  an element of the base held before the layer dropped or changed it is free again. A unique
  index the base keeps disabled (it was stored over repeated values) stays disabled on the
  layer. A layer can create, drop and redefine indexes (`g.create_index(...)`,
  `g.drop_index(...)`) for itself: the base and the other layers keep theirs. A unique index
  is checked against the layer's data, as on a plain graph. Creating an index on a layer
  reads every element of its label in the base once and keeps its own copy of the index, which
  counts in the layer's `g.memory_usage()`.
- **Order:** unordered results list the same elements as a plain graph with the same data,
  in the same order, except after some removals (details on the
  [developer page](../for_developers/layered_graphs.md#limitations)). Use `order()` when the
  order matters.
- **Not in GQL yet.** GQL queries run on a layer like on any graph, but GQL has no function
  for an element's origin or the diff yet. Use the DSL for those.
- **No playground view of the difference yet.** The playground shows a layer like any graph;
  styling added, modified and removed elements is planned.
