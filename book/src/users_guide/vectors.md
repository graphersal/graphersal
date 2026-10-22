# Vectors

> New to embeddings? [Embeddings and Vector Search](../getting_started/embeddings.md) is the
> task-oriented tour: semantic search, filtered search, GraphRAG and recommendations, in the
> playground with toy vectors and in Python with a real model. This page is the reference.

A **vector** is a fixed-length list of numbers stored as one value: an embedding of a text, an
image or a node, as a language or vision model computes it. A property holds a `vector`, not a
list that happens to contain numbers, and the engine stores it compactly, converts it between
element kinds and compares it as one value. Vectors are a Graphersal extension: TinkerPop has no
vector type.

**The engine never calls a model on its own.** A query that reached out to an embedding service
by itself would depend on the network, could not be rolled back (a paid call stays paid) and would
not run in the browser playground. The host either computes embeddings itself (in Python, in an
MCP tool, in a server) and writes them as ordinary property values, or registers *embedders* that
the `embed` step calls ([Embeddings](#embeddings) below); one small embedder, `hash-ngram/1`, is
built in.

```rhai,graph=empty
type_of(vector([0.12, -0.5, 0.33]))   // "Vector"
vector([0.12, -0.5, 0.33]).dimension   // 3
vector([0.12, -0.5, 0.33], "i8").element   // "i8"
g.inject(vector([0.5, 1.0])).is(P.typeOf(GType.VECTOR)).count().next()   // 1
```

## Element kinds and quantization

A vector has 1 to 65 536 components, all of one **element kind**:

| Element | A component | Bytes per component | 768 components |
|---|---|---|---|
| `f32` (default) | an IEEE single-precision float | 4 | 3 KiB |
| `f16` | an IEEE half-precision float | 2 | 1.5 KiB |
| `i8` | an 8-bit code times one f32 scale per vector (`value = code * scale`) | 1 | 768 B |
| `bit` | one sign bit: `1.0` for a set bit, `-1.0` otherwise | 1/8 | 96 B |

A list of 768 numbers as a plain `array` takes about 24 KiB; the same vector as `f32` 3 KiB.

**Quantization** converts a vector to another element kind, to save memory at a small cost in
precision:

- **f32 to f16** rounds every component to the nearest half value (ties to even). A component
  beyond the half range (65 504) is an error, never an infinity.
- **f32 to i8** is symmetric scalar quantization: `scale = max |x| / 127`, `code = round(x /
  scale)` in -127..=127. A zero vector has scale 0.
- **f32 to bit** keeps the sign: a component `> 0` sets its bit (binary quantization).
- Every other conversion goes through the f32 values, so `quantize("f32")` dequantizes.

```rhai,graph=empty
vector([0.5, -1.0, 0.25]).quantize("bit").to_list()   // [1.0, -1.0, 1.0]
vector([0.5, -1.0, 0.25]).quantize("f16").to_list()   // [0.5, -1.0, 0.25]
g.inject(vector([0.5, -1.0, 0.25])).quantize("i8").next().element   // "i8"
vector([1e10], "f16")   // fails: the component is beyond the range of an f16 vector
```

All four kinds are part of the value type and of every file format from the start, so storing a
property quantized is a choice of the data, never a format change.

## Building vectors

- `vector(list)` builds an f32 vector of a list of numbers; `vector(list, kind)` quantizes it to
  `"f16"`, `"i8"` or `"bit"`.
- The `as_vector()` step converts a list of numbers in a traversal (a stored list, a `fold()`);
  a vector passes, `null` stays `null`.
- A schema that declares the property as a vector converts a written list on write (below).

A vector never holds NaN or an infinity (it would break ordering and comparison); `-0.0` is kept
and equals `0.0`. An empty list, a component that is not a number and more than 65 536
components are errors:

```rhai,graph=empty
g.inject([0.5, 1]).as_vector().next()   // vector([0.5, 1.0])
g.inject(1, 2, 3).fold().as_vector().next().dimension   // 3
vector([])   // fails: a vector has 1 to 65536 components
vector([1, "x"])   // fails: component 1 is not a number
```

A vector prints as its DSL literal, which evaluates back to the same vector (an `i8` or `bit`
vector shows its dequantized values and its kind): `vector([0.5, 1.0])`, `vector([0.503937,
-1.0], "i8")`. `to_string()` and the step names in `.profile()` cut it after 16 components, like
a list. In a script a vector has the getters `element` and `dimension` and the methods
`quantize(kind)` and `to_list()` (the values as floats).

## Writing, filtering and ordering

A vector is written like any other value, and `has()` compares it by value: the same element
kind, the same dimension and equal components. A list is never equal to a vector, and an `f16`
vector never equals an `f32` one, even with the same values:

```rhai,graph=empty,script
g.add_v("doc").property("title", "Graphs").property("emb", vector([0.1, 0.9])).iterate();
g.add_v("doc").property("title", "Vectors").property("emb", vector([0.8, 0.2])).iterate();
g.v().has("emb", vector([0.1, 0.9])).values("title").next();   // "Graphs"
g.v().has("emb", [0.1, 0.9]).count().next();   // 0
g.v().has_label("doc").order().by("emb").values("title").to_list()   // ["Graphs", "Vectors"]
```

Vectors order (a Graphersal extension) after the dates and before strings; among themselves by
element kind (`f32`, `f16`, `i8`, `bit`), then by dimension, then component by component. A
vector is one value in the local steps (`count(Scope.local)` is 1, `unfold()` passes it), and
`min()`/`max()` refuse it.

A [property index](property_indexes.md) never holds vectors: `has(key, vector)` on an indexed
key reads the elements by a scan, with the same result.

**Quantize an existing property** with one write per element, then declare the new element in the
schema (`g.patch_schema(..)`, below). This works as shown while no schema declares the property;
when the schema already declares it as `f32` (mode open or closed), a write is converted back to
`f32` and the new element is refused while f32 data remains: the migration recipe for that case
(mode `none` for the rewrite, one unit) is in
[Embeddings and Vector Search](../getting_started/embeddings.md#quantization-smaller-vectors).

```rhai,graph=empty,script
g.add_v("doc").property("emb", vector([0.5, -1.0, 0.25])).iterate();
g.v().has_label("doc").has("emb").property("emb", __.values("emb").quantize("i8")).iterate();
g.v().values("emb").next().element   // "i8"
```

## Vectors in a schema

A schema declares a vector property as an `array` with the custom format `vector`:

```json
{"type": "array", "format": "vector", "element": "i8", "dimension": 768, "metric": "cosine"}
```

- `element` (default `f32`) and `dimension` (optional: any dimension) are checked exactly: a
  stored vector of another kind or length does not conform. `metric` (`cosine`, `dot`, `l2`,
  `hamming`) is an annotation: the metric the vectors are meant to be compared with, which
  `similarity()` and `nearest()` use when the query names none ([Metrics](#metrics)). It never
  changes or checks any data.
- `embedder` (on a label's top-level property only) records the model identity of the
  embedder that produced the vectors (`"hash-ngram/1"`): `embed_property()` fills it in and the
  text queries read it ([Which model produced a property](#which-model-produced-a-property)). A
  plain write does not check it.
- `items` is not allowed next to the format (the components are numbers of the element kind).
- Another JSON Schema validator ignores the unknown format and keywords and sees an array of
  numbers, which is what a vector exports as. The schema visualizer's per-label JSON Schema
  (`visualize("jsonschema")`) spells out `items` and `minItems`/`maxItems` for it.

In mode `open` or `closed`, a **list of numbers written to the property becomes a vector of the
declared element**, and a vector of another element is quantized to it: a property is stored
quantized from the start, whatever the host sends. The dimension is never changed:

```rhai,graph=empty,script
g.set_schema(#{mode: "open", vertices: #{doc: #{schema: #{type: "object", properties: #{
  emb: #{type: "array", format: "vector", element: "i8", dimension: 3}}}}}});
g.add_v("doc").property("emb", [0.5, -1.0, 0.25]).iterate();
g.add_v("doc").property("emb", vector([1.0, 2.0, 3.0])).iterate();
g.v().values("emb").to_list().map(|v| v.element)   // ["i8", "i8"]
```

On that graph, `g.add_v("doc").property("emb", [1.0, 2.0])` fails with "Type mismatch on vertex
'doc.emb': expected vector<i8, 3>, got vector<i8, 2>", and its help names the conversion
(`values("emb").as_vector().quantize("i8")`).

`infer_schema()` reports a vector as such, with its dimension when every vector of that element
at the position has the same one. `diff_schema()` counts a change of `element` or `dimension`
(and a list turned into a vector, or back) as incompatible, since the stored data no longer
conforms; dropping `dimension` (any dimension) is compatible. An `embedder` change is the entry
`embedder_changed`: declaring one where there was none is compatible, changing it is not (the
stored vectors and a text query embedded by the new model are no longer comparable), and removing
it is not either (a text query on the property no longer finds its model). `infer_schema()`
never reports an `embedder`: nothing in the data says which model made a vector. A saved query takes a vector
parameter the same way: `params: #{q: #{schema: #{type: "array", format: "vector", element:
"f32", dimension: 768}}}`; a list argument is converted to the declared vector.

## Similarity

`similarity()` scores a vector against a query vector `q` and yields a `float64`. It is a map
step in two forms:

| Form | Scores |
|---|---|
| `similarity(key, q[, metric])` | the vector property `key` (a name or a `jpath(..)`) of the incoming element (or map) |
| `similarity(q[, metric])` | the incoming value itself, a vector (`values("emb").similarity(q)`, a computed vector) |

`q` is a vector, a list of numbers (made an f32 vector when the step is built), or an anonymous
traversal run on each incoming traverser whose first result is the query (`__.select("q")`).
The key form drops an element without the property or whose value is not a vector; the value
form drops `null` and fails on any other value that is not a vector.

```rhai,graph=empty,script
g.add_v("doc").property("title", "Graphs").property("emb", vector([0.6, 0.8])).iterate();
g.add_v("doc").property("title", "Vectors").property("emb", vector([1.0, 0.0])).iterate();
g.v().has_label("doc").similarity("emb", vector([1.0, 0.0])).to_list();   // [0.6000000095367428, 1.0]
g.v().has_label("doc").values("emb").similarity([0.0, 0.0], "l2").to_list();   // [1.000000023841858, 1.0]
g.v().has_label("doc")
 .project("title", "score").by("title").by(__.similarity("emb", vector([1.0, 0.0])))
 .to_list()
```

### Metrics

| Metric | Score | Closer is | Vectors |
|---|---|---|---|
| `cosine` (default) | the dot product over the product of the norms, -1 to 1; `0.0` when either vector is zero (never NaN) | higher | any kind except bit against non-bit |
| `dot` | the dot product | higher | as cosine |
| `l2` | the Euclidean distance | smaller | as cosine |
| `hamming` | the number of differing bits | smaller | bit vectors only |

The metric is the step's argument if it names one; otherwise the `metric` annotation the stored
schema declares for the property (a top-level name on a vertex or edge, by the element's labels
like a write; for the value form, the property a `values("emb")` reference points to), and
`cosine` without one. A computed vector, a jpath key and a map have no declaration: cosine.

Scores are exact: every component is read from the stored kind (an f16 or i8 vector dequantized,
bits counted) and summed in f64, in a fixed order, so the same vectors always give the same score.
Vectors of different element kinds compare through their values: an `i8` property can be searched
with an `f32` query. A bit vector compares only with another bit vector (for which the dot product,
cosine and distance follow exactly from the Hamming distance, the components being ±1). Two vectors
of different dimensions are an error, never truncated:

```rhai,graph=empty
g.inject(vector([1.0, 0.0])).similarity(vector([1.0, 0.0, 0.0])).next()   // fails: vectors of dimensions 2 and 3 cannot be compared
g.inject(vector([1.0, -1.0], "bit")).similarity(vector([1.0, -1.0])).next()   // fails: a bit vector compares only with another bit vector
g.inject(vector([1.0, -1.0], "bit")).similarity(vector([1.0, 1.0], "bit"), "hamming").next()   // 1.0
```

## Exact search: nearest()

`nearest(key, q, k[, #{metric: ".."}])` keeps the `k` incoming elements whose vector property
`key` is closest to `q`, **best first** (the highest cosine or dot, the smallest l2 or hamming).
It is an exact search: every incoming element is scored, with a bounded heap of `k` (O(n log k),
never a full sort).

- Elements without the property, or whose value is not a vector, are skipped.
- Equal scores keep the input order, the earlier element first: the answer is deterministic, the
  same in a live graph, a view at a commit and a fork.
- `q` is a vector, a list of numbers or a traversal. A traversal runs for every scored element and
  must give the same vector each time (one query per search, "more like this" below); a query
  that depends on the element fails and names `similarity()`, which scores each element against
  its own vector.
- The metric follows the same rule as for `similarity()`. Without a metric argument, elements whose
  labels declare different metrics fail: one search ranks with one metric.
- Bulk counts as in `limit()`: a traverser that stands for n equal ones is n results, cut at `k`.
- `.profile()` shows the step as written, `nearest("emb", vector([..]), 10, #{metric: "l2"})`;
  the deadline (`evaluationTimeout`) and a cancel stop it while it scores.

```rhai,graph=empty,script
g.add_v("doc").property("title", "Graph databases 101").property("emb", vector([0.9, 0.1, 0.0])).iterate();
g.add_v("doc").property("title", "Vector search").property("emb", vector([0.1, 0.9, 0.1])).iterate();
g.add_v("doc").property("title", "Indexes").property("emb", vector([0.6, 0.6, 0.2], "i8")).iterate();
g.add_v("doc").property("title", "Draft").iterate();
g.v().has_label("doc").nearest("emb", vector([1.0, 0.2, 0.0]), 2).values("title").to_list();   // ["Graph databases 101", "Indexes"]
g.v().has_label("doc").nearest("emb", [0.0, 1.0, 0.0], 1, #{metric: "l2"}).values("title").next()   // "Vector search"
```

`nearest()` is the same as ordering by the score and keeping `k`, which is how to get the scores
along with the elements:

```rhai,graph=empty,script
g.add_v("doc").property("title", "a").property("emb", vector([1.0, 0.0])).iterate();
g.add_v("doc").property("title", "b").property("emb", vector([0.6, 0.8])).iterate();
let q = vector([1.0, 0.1]);
g.v().has_label("doc").nearest("emb", q, 2)
 .project("title", "score").by("title").by(__.similarity("emb", q))
 .to_list()
```

### Filtered search

Filter first, then search: `has_label()` and `has()` before `nearest()` reduce what it scores (a
`has_label()` folds into `v()` as usual, a property index serves a `has()`), and a `where()` can
filter by the graph around each element:

```rhai,graph=empty,script
g.add_v("team").property("name", "engineering").as("eng")
 .add_v("doc").property("title", "Design").property("lang", "en").property("year", 2026)
   .property("emb", vector([0.9, 0.1])).add_e("owned_by").to("eng")
 .add_v("doc").property("title", "Entwurf").property("lang", "de").property("year", 2026)
   .property("emb", vector([0.9, 0.1])).add_e("owned_by").to("eng")
 .iterate();
g.v().has_label("doc")
 .has("lang", "en").has("year", P.gte(2026))
 .where(__.out("owned_by").has("name", "engineering"))
 .nearest("emb", vector([1.0, 0.0]), 5)
 .values("title").to_list()   // ["Design"]
```

### GraphRAG: a vector hit, then the graph

Retrieval-augmented generation over a graph finds the chunks nearest to the question's embedding
and expands along the edges to build the context:

```rhai,graph=empty,script
g.add_v("doc").property("title", "Graphersal").as("d")
 .add_v("entity").property("name", "Rhai").as("e")
 .add_v("chunk").property("text", "Graphersal runs Gremlin in Rhai.").property("emb", vector([0.9, 0.2]))
   .add_e("part_of").to("d")
 .add_e("mentions").from("d").to("e")
 .iterate();
g.v().has_label("chunk").nearest("emb", vector([1.0, 0.1]), 5).as("hit")
 .out("part_of").as("doc")
 .out("mentions").has_label("entity").as("ent")
 .select("hit", "doc", "ent").by("text").by("title").by("name")
 .to_list()
```

The embedding of the question comes from the host (the model is never called by the engine), as
a query parameter or the argument of a saved query: `params: #{q: #{schema: #{type: "array",
format: "vector", dimension: 384}}}` with the body `g.v().has_label("chunk").nearest("emb", q,
k)`. MCP clients and the AI Chat call such a query with `run_saved_query`.

### More like this

The query vector can come from the graph itself: label it, then search with `__.select(..)`:

```rhai,graph=empty,script
g.add_v("doc").property("id", "d42").property("emb", vector([1.0, 0.0])).iterate();
g.add_v("doc").property("id", "d7").property("emb", vector([0.9, 0.1])).iterate();
g.add_v("doc").property("id", "d9").property("emb", vector([0.0, 1.0])).iterate();
g.v().has("id", "d42").as("me").values("emb").as("q")
 .v().has_label("doc").where(P.neq("me"))
 .nearest("emb", __.select("q"), 2)
 .values("id").to_list()   // ["d7", "d9"]
```

### Recommendations with vector_mean()

`vector_mean()` is a reducing barrier (like `mean()`): the component-wise mean of the incoming
vectors, as an f32 vector. An incoming list of vectors (`fold()`) counts each of its vectors,
`null` is skipped, quantized vectors count with their values, an empty stream gives no result,
and vectors of different dimensions fail. `local(__.unfold().vector_mean())` averages each list
on its own. The mean of what a user liked is a taste vector to search with:

```rhai,graph=empty,script
g.add_v("user").property("name", "u7").as("u")
 .add_v("article").property("title", "Graphs").property("emb", vector([1.0, 0.0])).as("a")
 .add_v("article").property("title", "Trees").property("emb", vector([0.8, 0.4])).as("b")
 .add_v("article").property("title", "Cooking").property("emb", vector([0.0, 1.0]))
 .add_v("article").property("title", "Forests").property("emb", vector([0.9, 0.3]))
 .add_e("liked").from("u").to("a").add_e("liked").from("u").to("b").add_e("read").from("u").to("a")
 .add_e("read").from("u").to("b")
 .iterate();
g.v().has("name", "u7").out("liked").values("emb").fold().vector_mean().as("taste")
 .v().has_label("article")
 .where(__.in("read").has("name", "u7").count().is(0))
 .nearest("emb", __.select("taste"), 1)
 .values("title").next()   // "Forests"
```

### Performance

The search is an exact scan: its time grows with the number of scored elements times the
dimension, and it needs no index, so it costs nothing to keep up to date and answers the same at
every commit, mark and snapshot. Measured with the bench `nearest_10k_x_384` (`cargo bench -p
graphersal --bench graph --features io,script,display,serde -- nearest_10k_x_384`): 10 000
vertices with a random 384-dimensional embedding, `v().has_label("doc").nearest("emb", q, 10)`,
on an Apple M3:

| Stored element | Metric | Median | Per vector |
|---|---|---|---|
| `f32` | cosine | 1.3 ms | 130 ns |
| `f16` | cosine | 2.3 ms | 230 ns |
| `i8` | cosine | 1.8 ms | 180 ns |
| `bit` | hamming | 0.21 ms | 21 ns |

So about 13 ms per 100 000 f32 vectors of 384 components: enough for most in-memory graphs, and a
filter before the search (above) scores fewer. There is no approximate (ANN) index yet (see
[Current Limitations](current_limitations.md)). These figures come from a source build, not from
the musl-linked prebuilt Linux binary
([Performance of the prebuilt Linux binaries](../getting_started/installation.md#performance-of-the-prebuilt-linux-binaries)).

### Quantization: memory against accuracy

A quantized property takes less memory and gives approximately the same answers. Measured by the
test `quantized_search_recall_against_f32` on 2 000 random 128-dimensional vectors (20 queries,
recall@10 against the f32 search with cosine); random vectors are the hardest case, real
embeddings usually keep more:

| Stored as | Bytes per 384 components | recall@10 |
|---|---|---|
| `f32` | 1 536 | 1.00 (exact) |
| `f16` | 768 | 1.00 |
| `i8` | 384 (+ 4 for the scale) | 0.99 |
| `bit` (hamming) | 48 | 0.30 |
| `bit` top 100, re-ranked by `f32` | 48 + 1 536 | 0.74 |

Bit vectors are a coarse, very fast prefilter: keep both properties and re-rank the prefilter's
hits exactly, `nearest("emb_bit", q.quantize("bit"), 100, #{metric: "hamming"})` followed by
`nearest("emb", q, 10)`.

## Embeddings

An **embedder** turns text into vectors. The step `embed()` embeds the incoming string,
`embed(key)` the string property `key` (a name or a `jpath(..)`) of the incoming element, and the
function `embed(text)` makes a query vector before any traversal runs. Each takes an option map
naming the embedder, `#{embedder: "name"}`; without it the host's default runs. `g.embedders()`
lists what is registered:

```rhai,graph=modern
g.embedders()[0].name   // "hash-ngram/1"
embed("graph").dimension   // 256
g.v().has_label("person").embed("name").count().next()   // 4
g.v().embed("name", #{embedder: "hash-ngram/1"}).count().next()   // 6
g.v().values("age").embed().next()   // error: embed() takes a string, not int64
g.v().embed("name", #{embedder: "nope"}).next()   // error: there is no embedder "nope"
```

Rules of the step:

- an element without the property is dropped (like `values(key)`); a value that is not a string
  fails: nothing is converted implicitly, write `as_string()` first;
- an empty text, or one with no word at all, fails (`has(key, P.neq(""))` leaves them out);
- the step hands the embedder batches (up to the embedder's `max_batch` traversers per call),
  each traverser once whatever its bulk, and keeps the order;
- `.profile()` names the embedder that ran: `embed("name", embedder: "hash-ngram/1")`;
- writing the vectors (`property("emb", __.embed("text"))`) happens inside the traversal's unit
  under the graph's write lock, which suits a fast local embedder such as the built-in one;
- a property declared `{"type": "array", "format": "vector", "dimension": 384}` refuses a vector
  of another dimension or element kind, as for any write ([Vectors in a schema](#vectors-in-a-schema)).

A fuzzy name search on the modern graph:

```rhai,graph=modern,script
g.v().has_label("person").property("name_emb", __.embed("name")).iterate();
g.v().has_label("person").nearest("name_emb", embed("Markus"), 2).values("name").to_list()   // ["marko", "vadas"]
```

What `g.embedders()` returns for each embedder:

| Key | |
|---|---|
| `name` | the registry name, what `#{embedder: ..}` takes |
| `model` | the stable identity of what computes the vectors (`hash-ngram/1`, `acme/minilm@3f2a9c41b7de`), independent of the host's name for it and of any file path: two embedders of one model give comparable vectors |
| `dimension`, `element` | the shape of every vector it returns |
| `metric` | the metric it recommends for comparing them |
| `max_batch` | the most texts per call |
| `deterministic` | whether the same text always gives the same bits |
| `local` | whether the texts stay in the process |
| `default` | whether it runs without an `embedder` option |

Running an embedder asks `Execute` on the resource `Embedder("name")`, for every embedder including
the built-in one ([Permissions](permissions.md)); `AccessPolicy::read_only()` allows it, and a
saved query may embed. A computed vector is stored as a plain value: replay, recovery, change sets
and loads never call an embedder, so an embedder that is not deterministic is safe to use.

### Embedding a property: `embed_property()`

`g.embed_property(#{label, from, to})` embeds the string property `from` (a name or a
`jpath(..)`) of every element with `label` and stores the vector in its top-level property `to`.
It is a job, not a traversal step, and returns a report:

```rhai,graph=modern,script
g.set_schema(g.infer_schema(), SchemaMode.open);
let report = g.embed_property(#{label: "person", from: "name", to: "emb"});
[report.embedded, report.units, report.schema_changed]   // [4, 1, true]
```

| Option | Default | |
|---|---|---|
| `label` | (required) | the label of the elements |
| `from` | (required) | the text property, a name or a `jpath(..)` |
| `to` | (required) | the vector property, a top-level name |
| `element` | `"vertex"` | `"edge"` embeds edges |
| `embedder` | the host's default | the registry name of the embedder |
| `only_missing` | `true` | skip elements that have `to` already; `false` embeds every element again |
| `batch` | the embedder's `max_batch` | elements per page (one embedder call and one unit each) |

The report counts `embedded`, `skipped_no_text` (no such property, `null`, or a text with
nothing to embed), `not_text` (a value that is not a string: nothing is converted),
`skipped_changed` and `skipped_removed` (see below), the committed `units`, whether the job
declared the property in the schema (`schema_changed`), `elapsed_ms`, and the `embedder` and
`model` that ran.

**How it runs.** The job works in pages so that a slow embedder (a model in another process, a
network service) never blocks the graph:

1. it collects the ids of the elements to embed (a short read lock);
2. per page it reads their texts (a short read lock), calls the embedder **without any lock of
   the graph**, so other queries and writers go on meanwhile, and then writes the page's vectors
   in one unit under the write lock;
3. before writing an element it reads its text again: a text another writer changed since step 2
   is skipped (`skipped_changed`), as is an element removed meanwhile (`skipped_removed`), so a
   stale vector is never stored.

Each page is atomic; the job as a whole is not. When it fails (the embedder fails, a schema
refuses a write, the run is cancelled), the pages before stay committed, the failing page is
rolled back, and the error stops the script; since `only_missing` is the default, running the job
again continues where it stopped. A text the embedder finds nothing in (an empty one, punctuation
only) is skipped and counted, never a failure.

Inside an open unit (an explicit `transaction`, and every script of the playground, which runs
as one unit) the job releases nothing: it runs every phase under the unit's lock, its pages are
savepoints of that unit, and it commits or rolls back with it. The CLI runs it two-phase.

The job asks `Read` and `Update` on the label's data, `Execute` on the embedder and, when it
declares the property, `Update` on `Schema` ([Permissions](permissions.md)); a saved query
cannot run it (it may write data only, never the schema). From Rust it is `Graph::embed_property(&EmbedJob, &Embedders,
&CancelToken, &mut progress)` (two-phase, a progress callback after every page, a partial
report in the error) or `EmbedJob::run_in(&mut storage, ..)` inside a transaction you hold.

### Which model produced a property

Vectors of different models cannot be compared: a cosine between a `hash-ngram/1` vector and one
of a semantic model means nothing. The schema therefore records, per label and property, which
model produced the vectors: the keyword `embedder` of a vector declaration holds the embedder's
**model identity** (`model` in `g.embedders()`, never a registry name or a file path):

```json
{"type": "array", "format": "vector", "element": "f32", "dimension": 256, "metric": "cosine", "embedder": "hash-ngram/1"}
```

`embed_property` needs an **attached schema** (mode `open` or `closed`) and keeps it right; it
never creates or attaches one itself (`g.set_schema(g.infer_schema(), SchemaMode.open)` attaches
the structure a graph has):

- `to` undeclared for the label: the job declares it (`type`, `format: vector`, `element`,
  `dimension`, `metric` from the embedder, `embedder` its model), in the unit of the first page,
  so the declaration and the first vectors commit together (and travel together through the
  journal, a rollback, a fork);
- declared as a vector of the embedder's dimension (or of any dimension) without `embedder`:
  the job adds `embedder`;
- declared with the same model: nothing to change;
- declared with **another model**: refused ("the schema declares "doc"."emb" as vectors of the
  model ..."): embed into another property, or change the schema deliberately and embed again
  with `only_missing: false`;
- declared as something else (not a vector, or another dimension): refused with the schema's
  own type mismatch, before the embedder runs;
- declared with **another element kind** (`i8`, `f16`, `bit`) than the embedder returns: fine,
  each vector is quantized to the declared element on write, exactly as a schema write of a
  vector of another element is, so an f32 model can fill an `i8` property (a quarter of the
  memory) directly.

Different labels and properties may use different models: `doc.emb` from one, `doc.title_emb`
or `person.emb` from another. A plain write of a vector (`property("emb", vector)`) still checks
only the element kind and the dimension: the provenance is kept by `embed_property` and used by
the text queries, not checked on every write.

### Text queries

With the provenance recorded, `nearest()` and `similarity()` take the query as **text**: the text
is embedded once per execution, with the registered embedder whose model the schema declares for
the property, so the query vector always comes from the model of the stored ones:

```rhai,graph=modern,script
g.set_schema(g.infer_schema(), SchemaMode.open);
g.embed_property(#{label: "person", from: "name", to: "emb"});
g.v().has_label("person").nearest("emb", "Markus", 2).values("name").to_list()   // ["marko", "vadas"]
```

- The model is read per element from the attached schema (the element's labels, the property);
  every element the query meets must declare the same model, else the query fails naming both
  (filter by label first: `has_label("doc")`).
- No schema attached, or no `embedder` declared for the property: the query fails, and its help
  shows how to attach a schema or run `embed_property`; passing a vector, `nearest("emb",
  embed("Markus"), 2)`, keeps working without any schema.
- No registered embedder of the declared model (`Embedders::by_model`): the query fails, and its
  help shows how a host registers one.
- The key form only: `similarity("emb", "text")` and `nearest("emb", "text", k)`, with a
  top-level property name.
- On a property declared `i8` or `f16` the embedded (f32) query is compared with the stored
  vectors as a vector query is; on a `bit` property, which compares only with bit vectors, the
  query is quantized to bits first, with the quantizer of a write (the same as
  `nearest("emb", g.inject(embed("text")).quantize("bit").next(), k)`).
- It asks `Execute` on the embedder it resolves (on each one, when the schema declares several
  models for the property on different labels), and `.profile()` names it:
  `nearest("emb", "Markus", 2, embedder: "hash-ngram/1")`.

### The built-in `hash-ngram/1`

`hash-ngram/1` needs no model file and no dependency and gives the same bits on every target (the
CLI, Python, the dev server, the browser playground). It lowercases the text, splits it into words
(runs of letters and digits), and hashes each word and its character 3-, 4- and 5-grams (of the
word framed as `<word>`) into 256 signed buckets, then scales the result to length 1 (f32
components, metric cosine).

It measures **lexical** similarity: typos, word forms and shared stems come out close, in any
script ("Graph" and "grahp", "Москва" and "Москве"), meaning does not ("car" and "automobile"
are unrelated). Uses: fuzzy lookup, entity matching, deduplication, and a baseline before a
semantic model.

The algorithm is frozen: the output of `hash-ngram/1` never changes (a different algorithm would
be a new name, `hash-ngram/2`), and tests pin its bits. Two caveats: there is no Unicode
normalization (a precomposed "é" and "e" followed by a combining accent are different words), and
lowercasing and the letter/digit classes follow the Unicode version of the Rust toolchain the
binary was built with.

Its time grows with the length of the text. Measured with the bench `embed_hash_ngram_1000`
(`cargo bench -p graphersal --bench graph --features io,script,display,serde --
embed_hash_ngram_1000`, 1000 texts per call, Apple M-series, source build): about 0.84 µs per
short title (5 words) and about 18.5 µs per paragraph of about 1 KB, so embedding a property of
100 000 short texts takes well under a second.

### Embedders for hosts

A host plugs in its own embedder (a model loaded from a file, an HTTP client, a binding
language's function) by implementing the Rust trait `graphersal::vector::Embedder`: `info()`
returns its `EmbedderInfo` (name, model identity, dimension, element kind, metric, batch size,
determinism, locality; built at runtime, so a dimension learnt from a model file is fine), and
`embed(texts, ctx)` returns one vector per text, calling `ctx.check()` between pieces of work so a
cancelled or timed-out query stops. A constructor that cannot load its model returns
`EmbedError::unavailable(..)`. The host registers it in an `Embedders` registry and hands that to
its traversals:

```rust
use std::sync::Arc;
use graphersal::prelude::*;
use graphersal::vector::{Embedders, HashNgram};

// Any `Embedder`; here the built-in algorithm with 64 components under the host's own name.
let small = HashNgram::with_dimension("acme/small", 64).unwrap();
let embedders = Arc::new(
    Embedders::builder()
        .with(Arc::new(small))
        .unwrap()
        .default_name("acme/small")
        .unwrap()
        .build(),
);
// The Rust API.
let graph = GraphSource::tinkerpop_modern();
let n = graph
    .read()
    .traversal()
    .with_embedders(embedders.clone())
    .v(None::<()>)
    .embed("name", None)
    .count()
    .next()
    .unwrap();
assert_eq!(n.and_then(|n| n.as_i64()), Some(6));
// A script host: the engine (embed(text), GraphSource::x()) and the scope (g, _g, __).
let mut engine = graphersal::script::engine();
graphersal::script::set_engine_embedders(&mut engine, embedders.clone());
let mut scope = graphersal::script::graph_scope(Arc::new(graph));
graphersal::script::set_scope_embedders(&mut scope, embedders);
```

The script entry points that build their own engine and scope (`eval_value_with_limits`,
`eval_value_atomic`, `eval_value_dry_run`, ...) take the registry from `Embedders::scope`:
`embedders.scope(|| eval_value_atomic(graph, script, params, &limits, authorizer))` gives every
engine, scope and traversal created inside the closure on that thread the host's registry (an
explicit `with_embedders`/`set_*_embedders` wins). The Python binding runs its scripts this way.

The registry always keeps `hash-ngram/1`; `hash-ngram` and every `hash-ngram/*` name are
reserved, a name is unique and made of letters, digits and `. _ - / :` (such as
`minishlab/potion-base-8M`), and `Embedders::by_model` finds an embedder by its model identity.
`graphersal-session` takes the registry in `SessionPresets::with_embedders`, which the dev server
(its MCP `query` tool and the AI Chat's tools included) and the WebAssembly playground run under.

### Model2Vec models (feature `model2vec`)

`graphersal::vector::Model2Vec` is a ready-made embedder for
[Model2Vec](https://github.com/MinishLab/model2vec) static embedding models (one vector per
token, averaged over the text; semantic, fast on a CPU), behind the feature `model2vec` of the
`graphersal` crate (off by default, native targets only; the `graphersal` command line has it on:
[Real models: Model2Vec](../getting_started/embeddings.md#real-models-model2vec)). It loads from a
**local directory only** and never touches the network:

```rust,ignore
use std::sync::Arc;
use graphersal::vector::{Embedder, Embedders, Model2Vec};

let model = Model2Vec::options()
    .with_max_tokens(256) // a longer text is cut (default 512)
    .load("potion", "models/minishlab/potion-base-8M")?;
assert_eq!(model.info().dimension, 256);
let embedders = Embedders::builder().with(Arc::new(model))?.default_name("potion")?.build();
# Ok::<(), graphersal::vector::EmbedError>(())
```

- **Layouts**: `config.json`, `tokenizer.json` and `model.safetensors` in the directory (the
  Model2Vec layout), or `config_sentence_transformers.json` beside them or beside a
  `0_StaticEmbedding` directory holding them (the sentence-transformers layout).
- **Info**: the model's dimension, `f32`, metric cosine, 256 texts per call, `local`, and
  `deterministic: false`: one build gives the same bits for the same text, but they depend on the
  versions of `model2vec-rs` and `tokenizers` it was built with. Vectors are L2-normalized when the
  model's config says so (`with_normalize` overrides it).
- **Model identity**: `model2vec/<repo>@<12 hex digits>`, where `<repo>` comes from the manifest
  `graphersal-embed-model.json` that `graphersal embed-model download` writes or from
  `Model2VecOptions::with_repo("minishlab/potion-base-8M")` for a directory the host downloaded
  itself (the same identity as the download of the same files; a repository differing from the
  manifest's refuses the load), else `model2vec@<12 hex digits>`. The digits start a SHA-256 over the three files
  (`Model2Vec::model_identity(dir)` computes it without loading the model); no path is part of it,
  so a copied or moved directory keeps its identity. A normalization other than the config's adds
  `?normalize=false`.
- **Load failures** are `EmbedError::Unavailable` with the reason (a missing file, a file that
  does not parse, a tokenizer whose vocabulary does not fit the embedding matrix) and the hint to
  download a model. The checks at load make every later lookup safe; a tokenizer failure while
  embedding is `EmbedError::Failed`, never a panic. A text with no known token (an empty text, or
  only unknown words) fails like an empty text for `hash-ngram/1`.

## Storage and exchange

| Format | A vector |
|---|---|
| Graphersal's binary format (snapshots, the Store, the journal) | its own value tag: the element kind, the dimension, for `i8` the scale, then the packed components ([format spec 3](../persistence/format.md#3-value-encoding)) |
| Serialized `ChangeSet` | exact, tagged: `{"@type": "graphersal:Vector", "@value": {"element": "f32", "data": [..]}}` ([Transactions](transactions.md)) |
| JSON results, `to_json()` | the list of its values (an `i8` or `bit` vector dequantized) |
| GraphSON export | a `g:List` of `g:Float`, which TinkerGraph loads |
| GraphSON import | a list stays a list; it becomes a vector where the target schema declares one (mode `open`/`closed`); the exact `graphersal:Vector` tag is read too ([GraphSON](graphson.md)) |
| GraphML | the JSON text of the list; a vector again where the target graph's schema declares one (any mode) |

Exporting an `i8` or `bit` vector as its values loses nothing that matters: a declared `i8`
property quantizes them back to the same codes. `memory_usage()` counts a vector's components
exactly (plus a small header), and the memory budget of a query estimates them the same way.

## From Python

A vector property is a `graphersal.Vector` in Python, built from a list of numbers or from any
one-dimensional `float32`/`float64` buffer such as a `numpy` array (the extension does not depend
on `numpy`):

```python
import numpy as np
from graphersal import Graph, Vector

g = Graph()
emb = np.array([0.12, -0.5, 0.33], dtype=np.float32)      # model.encode(text) in practice
g.execute('g.add_v("doc").property("emb", e).iterate()', params={"e": Vector(emb)})
v = g.query('g.v().values("emb").next()')                # graphersal.Vector
v.element, v.dimension, v.tolist()                       # ('f32', 3, [0.12, -0.5, 0.33])
Vector(emb, element="i8")                                # quantized, 1 byte per component

q = np.array([0.1, -0.4, 0.3], dtype=np.float32)          # the question's embedding
g.query('g.v().nearest("emb", q, 10).to_list()', params={"q": q})
```

A one-dimensional `float32`/`float64` numpy array passed as a parameter arrives as an f32
vector, like `Vector(array)`. See the Python binding's README for the details.

Python registers its own embedders: `graphersal.Embedder(name, fn, dimension=..)` wraps any
Python model (`fn(texts)` returns the rows, or one 2-D numpy array), `graphersal.Model2Vec(name,
path, model="minishlab/potion-base-8M")` loads a Model2Vec model from a directory you downloaded
(the same model identity as the command line's download), and `Graph(embedders=[..])`,
`graph.register_embedder`, `graph.embed_property(label=.., source=.., target=..)` and text
queries work as described above ([Python Quick Start: Embedders](../library/python.md#embedders)).

## In the playground and the CLI

The result table shows a vector as `vector[768 f32]` with an expandable preview of its first
values; the JSON view holds all of them. The property editor edits a vector as a JSON list of
numbers (a declared vector property offers the type `vector`). The CLI's tables show the kind and
the first values (`vector[768 f32] [0.012, -0.5, 0.33, 0.1, …]`), and a final vector prints as its
literal.
