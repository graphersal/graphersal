# Embeddings and Vector Search

An **embedding** is a list of numbers that a machine-learning model computes for a piece of data:
a sentence, a document, an image, a product. The model places similar things close together, so
"how do I feed my cat?" and "Feeding your cat" get vectors that point in almost the same
direction, while "Rust for beginners" points elsewhere. Searching by embedding finds what is
*meant*, not only what is spelled the same. Graphersal stores embeddings as a
[vector](../users_guide/vectors.md) value, searches them exactly with `nearest()` and
`similarity()`, and combines the hits with everything a graph does well: filters, edges, paths.

**Graphersal never calls a model on its own.** A semantic model runs in your host program (Python,
a server, an MCP tool), which writes the result as an ordinary property value, or registers the
model as an *embedder* that the `embed` step calls. Built in is one small embedder,
`hash-ngram/1`: no model, lexical similarity only (typos and word forms, not meaning), enough for
fuzzy lookup and for trying the steps; the [reference page](../users_guide/vectors.md#embeddings)
has the details and the reasoning. The `graphersal` command line registers a real semantic model
for you: a downloaded [Model2Vec](#real-models-model2vec) model, run locally.

This page is the practical tour. Every detail (element kinds, metrics, schema rules, formats) is
on the reference page [Vectors](../users_guide/vectors.md).

## Try it in the playground, without a model

Real embeddings have hundreds of dimensions. To see how the steps behave you can make up
**toy embeddings** with three dimensions whose meaning you pick yourself:

| Axis | Meaning |
|---|---|
| 1 | animals |
| 2 | food |
| 3 | technology |

"Training a puppy" is mostly animals: `[0.8, 0.1, 0.2]`. "Robot chefs" is food and technology:
`[0.1, 0.6, 0.8]`. A question about pets becomes `[1.0, 0.0, 0.0]`.

Every block below is a complete script that starts on the empty graph, so each one runs on its
own: the button in the margin opens it in the playground and runs it. The first lines are the
same in most blocks: a schema that declares `emb` as a 3-dimensional f32 vector, and seven
documents.

### The document graph and a first search

The schema declares the property `emb` with the format `vector`. In mode `open` a **list of
numbers written to it becomes a vector**, so the host can send plain lists (or JSON arrays).
`nearest(key, q, k)` then keeps the `k` documents closest to the query vector `q`, best first:

```rhai,graph=empty,script
// Toy embeddings with three axes: [animals, food, technology].
g.set_schema(#{mode: "open", vertices: #{doc: #{schema: #{type: "object", properties: #{
  emb: #{type: "array", format: "vector", element: "f32", dimension: 3}}}}}});
for d in [
  ["Cats and dogs",        2024, [0.9, 0.1, 0.0]],
  ["Training a puppy",     2026, [0.8, 0.1, 0.2]],
  ["Feeding your cat",     2025, [0.6, 0.7, 0.0]],
  ["Pasta at home",        2025, [0.0, 0.9, 0.1]],
  ["Robot chefs",          2026, [0.1, 0.6, 0.8]],
  ["Rust for beginners",   2024, [0.0, 0.0, 1.0]],
  ["GPS collars for dogs", 2026, [0.7, 0.0, 0.6]],
] {
  g.add_v("doc").property("title", d[0]).property("year", d[1]).property("emb", d[2]).iterate();
}

// The list was stored as a vector: vector([0.9, 0.1, 0.0])
g.v().has("title", "Cats and dogs").values("emb").next();

// The three documents closest to "animals":
// ["Cats and dogs", "Training a puppy", "GPS collars for dogs"]
g.v().has_label("doc").nearest("emb", [1.0, 0.0, 0.0], 3).values("title").to_list()
```

`nearest()` is an exact search: it scores every document it receives (cosine similarity unless a
metric is named or the schema declares one) and keeps the best `k`. The dimension is checked: a
query with 2 or 4 numbers against these 3-dimensional vectors is an error, never truncated.

### Ranking with scores

To see *how* close each document is, score it with `similarity()` and order by the score.
`project()` builds one row per document; anonymous steps take the `__.` prefix and the order
token is written out (`Order.desc`):

```rhai,graph=empty,script
g.set_schema(#{mode: "open", vertices: #{doc: #{schema: #{type: "object", properties: #{
  emb: #{type: "array", format: "vector", element: "f32", dimension: 3}}}}}});
for d in [
  ["Cats and dogs",        [0.9, 0.1, 0.0]],
  ["Training a puppy",     [0.8, 0.1, 0.2]],
  ["Feeding your cat",     [0.6, 0.7, 0.0]],
  ["Pasta at home",        [0.0, 0.9, 0.1]],
  ["Robot chefs",          [0.1, 0.6, 0.8]],
  ["Rust for beginners",   [0.0, 0.0, 1.0]],
  ["GPS collars for dogs", [0.7, 0.0, 0.6]],
] {
  g.add_v("doc").property("title", d[0]).property("emb", d[1]).iterate();
}

// A question about "animals and technology":
let q = vector([0.5, 0.0, 0.5]);
g.v().has_label("doc")
 .project("title", "score")
   .by("title")
   .by(__.similarity("emb", q))
 .order().by(__.select("score"), Order.desc)
 .limit(4)
 .to_list()
// GPS collars for dogs 0.997, Training a puppy 0.851,
// Rust for beginners 0.707, Cats and dogs 0.703
```

`nearest("emb", q, 4)` gives the same four documents in the same order without the scores; to
have both, search first and project after it:
`nearest("emb", q, 4).project("title", "score").by("title").by(__.similarity("emb", q))`.

### Filtered search

Put ordinary filters before `nearest()`: it then ranks only what passes them. Here only the
documents from 2025 on:

```rhai,graph=empty,script
g.set_schema(#{mode: "open", vertices: #{doc: #{schema: #{type: "object", properties: #{
  emb: #{type: "array", format: "vector", element: "f32", dimension: 3}}}}}});
for d in [
  ["Cats and dogs",        2024, [0.9, 0.1, 0.0]],
  ["Training a puppy",     2026, [0.8, 0.1, 0.2]],
  ["Feeding your cat",     2025, [0.6, 0.7, 0.0]],
  ["Pasta at home",        2025, [0.0, 0.9, 0.1]],
  ["Robot chefs",          2026, [0.1, 0.6, 0.8]],
  ["Rust for beginners",   2024, [0.0, 0.0, 1.0]],
  ["GPS collars for dogs", 2026, [0.7, 0.0, 0.6]],
] {
  g.add_v("doc").property("title", d[0]).property("year", d[1]).property("emb", d[2]).iterate();
}

// "Cats and dogs" (2024) is left out:
// ["Training a puppy", "GPS collars for dogs", "Feeding your cat"]
g.v().has_label("doc")
 .has("year", P.gte(2025))
 .nearest("emb", [1.0, 0.0, 0.0], 3)
 .values("title").to_list()
```

A filter is exact: unlike an approximate vector index, a filtered search never returns fewer
than `k` hits when `k` documents pass the filter. A `where(__.out(..)...)` filters by the graph
around each document the same way.

### GraphRAG: a vector hit, then the graph

Retrieval-augmented generation (RAG) gives a language model the passages that answer a question.
**GraphRAG** starts the same way, with the documents nearest to the question, and then follows
the edges: who wrote them, what they are about, which other documents share the topic. Here each
document has an author (`wrote`) and a topic (`about`). This block has no schema, so the lists
are wrapped in `vector(..)`:

```rhai,graph=empty,script
// Documents with toy embeddings [animals, food, technology], their authors and topics.
for d in [
  ["Cats and dogs",      "Ada",  "pets",        [0.9, 0.1, 0.0]],
  ["Feeding your cat",   "Ada",  "pets",        [0.6, 0.7, 0.0]],
  ["Pasta at home",      "Ben",  "cooking",     [0.0, 0.9, 0.1]],
  ["Robot chefs",        "Cleo", "cooking",     [0.1, 0.6, 0.8]],
  ["Rust for beginners", "Cleo", "programming", [0.0, 0.0, 1.0]],
] {
  g.merge_v(#{"T.label": "person", name: d[1]}).as("a")
   .merge_v(#{"T.label": "topic", name: d[2]}).as("t")
   .add_v("doc").property("title", d[0]).property("emb", vector(d[3])).as("d")
   .add_e("about").to("t")
   .add_e("wrote").from("a").to("d")
   .iterate();
}

// "What should I cook for my cat?" ~ [0.5, 0.8, 0.0]: the two best hits with their context.
// [#{"author": "Ada", "title": "Feeding your cat", "topic": "pets"},
//  #{"author": "Ben", "title": "Pasta at home", "topic": "cooking"}]
g.v().has_label("doc").nearest("emb", [0.5, 0.8, 0.0], 2)
 .project("title", "author", "topic")
   .by("title")
   .by(__.in("wrote").values("name"))
   .by(__.out("about").values("name"))
 .to_list()
```

One more hop finds documents that a vector search alone would rank low, but that share the best
hit's topic:

```rhai,graph=empty,script
for d in [
  ["Cats and dogs",      "pets",        [0.9, 0.1, 0.0]],
  ["Feeding your cat",   "pets",        [0.6, 0.7, 0.0]],
  ["Pasta at home",      "cooking",     [0.0, 0.9, 0.1]],
  ["Rust for beginners", "programming", [0.0, 0.0, 1.0]],
] {
  g.merge_v(#{"T.label": "topic", name: d[1]}).as("t")
   .add_v("doc").property("title", d[0]).property("emb", vector(d[2]))
   .add_e("about").to("t")
   .iterate();
}

// The best hit for [0.5, 0.8, 0.0] is "Feeding your cat"; its topic "pets" brings in:
// ["Cats and dogs"]
g.v().has_label("doc").nearest("emb", [0.5, 0.8, 0.0], 1).as("hit")
 .out("about").in("about").where(P.neq("hit"))
 .values("title").to_list()
```

The host then turns the rows into the prompt for its language model. In Python this is a few
lines ([below](#graphrag-from-python)).

### GraphRAG with text: `embed_property()` and a text query

The same flow without making up any vector: `embed_property()` embeds every document's title
with an embedder (here the built-in `hash-ngram/1`, which compares spelling, not meaning; a host
registers a semantic model the same way) and records in the schema which model produced the
vectors. A query then passes the question as **text**: `nearest()` embeds it once, with that
same model, so the question and the documents are always comparable. Both need a schema
attached (mode `open` or `closed`):

```rhai,graph=empty,script
g.set_schema(#{mode: "open"});
for d in [
  ["Cats and dogs",      "Ada",  "pets"],
  ["Feeding your cat",   "Ada",  "pets"],
  ["Pasta at home",      "Ben",  "cooking"],
  ["Cooking for robots", "Cleo", "cooking"],
  ["Rust for beginners", "Cleo", "programming"],
] {
  g.merge_v(#{"T.label": "person", name: d[1]}).as("a")
   .merge_v(#{"T.label": "topic", name: d[2]}).as("t")
   .add_v("doc").property("title", d[0]).as("d")
   .add_e("about").to("t")
   .add_e("wrote").from("a").to("d")
   .iterate();
}
// Embeds the 5 titles into "emb" and declares {"format": "vector", "embedder": "hash-ngram/1", ..}.
g.embed_property(#{label: "doc", from: "title", to: "emb"});

// [#{"author": "Ada", "title": "Feeding your cat", "topic": "pets"},
//  #{"author": "Ada", "title": "Cats and dogs", "topic": "pets"}]
g.v().has_label("doc").nearest("emb", "feeding cats", 2)
 .project("title", "author", "topic")
   .by("title")
   .by(__.in("wrote").values("name"))
   .by(__.out("about").values("name"))
 .to_list()
```

The job works page by page and never holds the graph's lock while the embedder runs, so a slow
model does not block other queries; a second run embeds only documents added since
([Embedding a property](../users_guide/vectors.md#embedding-a-property-embed_property)). One more
hop works as before:

```rhai,graph=empty,script
g.set_schema(#{mode: "open"});
for d in [["Pasta at home", "cooking"], ["Cooking for robots", "cooking"], ["Cats and dogs", "pets"]] {
  g.merge_v(#{"T.label": "topic", name: d[1]}).as("t")
   .add_v("doc").property("title", d[0])
   .add_e("about").to("t")
   .iterate();
}
g.embed_property(#{label: "doc", from: "title", to: "emb"});
// The best hit for "cooking pasta" shares its topic with: ["Pasta at home"]
g.v().has_label("doc").nearest("emb", "cooking pasta", 1).as("hit")
 .out("about").in("about").where(P.neq("hit"))
 .values("title").to_list()
```

### More like this

The query vector can come from the graph: take a document's own embedding, label it with
`as("q")` and search with `__.select("q")`, leaving the document itself out:

```rhai,graph=empty,script
g.set_schema(#{mode: "open", vertices: #{doc: #{schema: #{type: "object", properties: #{
  emb: #{type: "array", format: "vector", element: "f32", dimension: 3}}}}}});
for d in [
  ["Cats and dogs",        [0.9, 0.1, 0.0]],
  ["Training a puppy",     [0.8, 0.1, 0.2]],
  ["Feeding your cat",     [0.6, 0.7, 0.0]],
  ["Pasta at home",        [0.0, 0.9, 0.1]],
  ["Robot chefs",          [0.1, 0.6, 0.8]],
  ["Rust for beginners",   [0.0, 0.0, 1.0]],
  ["GPS collars for dogs", [0.7, 0.0, 0.6]],
] {
  g.add_v("doc").property("title", d[0]).property("emb", d[1]).iterate();
}

// Like "Feeding your cat" (animals and food): ["Pasta at home", "Cats and dogs"]
g.v().has("title", "Feeding your cat").as("me").values("emb").as("q")
 .v().has_label("doc").where(P.neq("me"))
 .nearest("emb", __.select("q"), 2)
 .values("title").to_list()
```

### Recommendations with vector_mean()

`vector_mean()` averages vectors, component by component. The mean of the documents a user
liked is a "taste" vector; search with it among the documents the user has not liked yet:

```rhai,graph=empty,script
g.set_schema(#{mode: "open", vertices: #{doc: #{schema: #{type: "object", properties: #{
  emb: #{type: "array", format: "vector", element: "f32", dimension: 3}}}}}});
for d in [
  ["Cats and dogs",        [0.9, 0.1, 0.0]],
  ["Training a puppy",     [0.8, 0.1, 0.2]],
  ["Feeding your cat",     [0.6, 0.7, 0.0]],
  ["Pasta at home",        [0.0, 0.9, 0.1]],
  ["Robot chefs",          [0.1, 0.6, 0.8]],
  ["Rust for beginners",   [0.0, 0.0, 1.0]],
  ["GPS collars for dogs", [0.7, 0.0, 0.6]],
] {
  g.add_v("doc").property("title", d[0]).property("emb", d[1]).iterate();
}
g.add_v("user").property("name", "sam").iterate();
for title in ["Cats and dogs", "Training a puppy"] {
  g.v().has("user", "name", "sam").as("u")
   .v().has("doc", "title", title).add_e("liked").from("u").iterate();
}

// Sam's taste: vector([0.85, 0.1, 0.1])
g.v().has("name", "sam").out("liked").values("emb").vector_mean().next();

// Recommended: ["GPS collars for dogs", "Feeding your cat"]
g.v().has("name", "sam").out("liked").values("emb").vector_mean().as("taste")
 .v().has_label("doc")
 .where(__.in("liked").has("name", "sam").count().is(0))
 .nearest("emb", __.select("taste"), 2)
 .values("title").to_list()
```

### Quantization: smaller vectors

An f32 vector takes 4 bytes per component: 1.5 KiB for a 384-dimensional embedding. Declared with
`element: "i8"` it takes 1 byte per component (plus 4 bytes for a scale), with almost the same
search results (measured on the [reference page](../users_guide/vectors.md#quantization-memory-against-accuracy)).
**The schema's element wins on write**: a list or an f32 vector written to an `i8` property is
quantized as it is stored, so the host keeps sending what its model gives it:

```rhai,graph=empty,script
g.set_schema(#{mode: "open", vertices: #{doc: #{schema: #{type: "object", properties: #{
  emb: #{type: "array", format: "vector", element: "i8", dimension: 3}}}}}});
for d in [
  ["Cats and dogs",      [0.9, 0.1, 0.0]],
  ["Pasta at home",      [0.0, 0.9, 0.1]],
  ["Rust for beginners", [0.0, 0.0, 1.0]],
] {
  g.add_v("doc").property("title", d[0]).property("emb", d[1]).iterate();
}

// Stored as i8: vector([0.9, 0.0992126, 0.0], "i8"); an f32 query searches it.
g.v().has("title", "Cats and dogs").values("emb").next();
g.v().has_label("doc").nearest("emb", [1.0, 0.2, 0.0], 1).values("title").next()   // "Cats and dogs"
```

The same rule works against you when you **quantize a property that is already declared as
f32**: `set_schema`/`patch_schema` to `element: "i8"` is refused while the stored vectors are
still f32 (the schema would not fit the data), and a rewrite with `quantize("i8")` under the f32
declaration is turned straight back into an f32 vector. Switch the checks off for the migration,
rewrite the values, then declare the new element:

```rhai,graph=empty,script
g.set_schema(#{mode: "open", vertices: #{doc: #{schema: #{type: "object", properties: #{
  emb: #{type: "array", format: "vector", element: "f32", dimension: 3}}}}}});
g.add_v("doc").property("title", "Cats and dogs").property("emb", [0.9, 0.1, 0.0]).iterate();
g.add_v("doc").property("title", "Pasta at home").property("emb", [0.0, 0.9, 0.1]).iterate();

// 1. Mode "none": no checks, no conversion on write.
g.patch_schema(#{mode: "none"});
// 2. Rewrite every vector quantized.
g.v().has_label("doc").property("emb", __.values("emb").quantize("i8")).iterate();
// 3. Declare i8 and switch the checks on again: the data fits now.
g.patch_schema(#{mode: "open", vertices: #{doc: #{schema: #{properties: #{emb: #{element: "i8"}}}}}});

g.v().has_label("doc").values("emb").to_list().map(|v| v.element)   // ["i8", "i8"]
```

The playground runs a whole script as one unit, so a failure on the way leaves the graph as it
was; from Python pass `atomic=True` to `execute()` for the same.

## Real models: Model2Vec

The `graphersal` command line (and the dev server it starts) can run a real, semantic text model
itself: a [Model2Vec](https://github.com/MinishLab/model2vec) *static* embedding model. Such a
model keeps one vector per token and averages them for a text, so it needs no GPU and no neural
network at run time: a small model like `minishlab/potion-base-8M` (256 dimensions, about 30 MB)
embeds thousands of sentences per second on a laptop CPU, and it captures meaning, not only
spelling ("car" and "automobile" come out close). It is less precise than a large transformer
model, and much faster.

**1. Download a model once.** This is the only step that uses the network:

```bash
graphersal embed-model download minishlab/potion-base-8M
```

It fetches `config.json`, `tokenizer.json` and `model.safetensors` from Hugging Face into the
models directory (`$GRAPHERSAL_EMBED_MODELS`, else `~/.cache/graphersal/embed-models`, on
Windows `%LOCALAPPDATA%\graphersal\embed-models`; `--dir` overrides it), checks the SHA-256
Hugging Face states for the large file and the git blob id of the small ones, and writes a
manifest `graphersal-embed-model.json` (repository, revision, the commit it resolved to, the
files with their sizes and SHA-256). A failed or interrupted download leaves no model behind.
`--revision <branch|tag|commit>` pins a version; `HF_TOKEN` is sent for gated or private
repositories. `graphersal embed-model list` shows what is there:

```text
REPO                           SIZE  LICENCE  IDENTITY
minishlab/potion-base-8M   29.5 MiB  mit      model2vec/minishlab/potion-base-8M@fac4fa854e68
```

**2. Register it under a name.** `--embedder NAME=REPO` (a downloaded repository) or
`--embedder NAME=PATH` (any directory holding a Model2Vec model) works with every mode: a
one-shot query, the REPL, `--server` and its MCP tools, a Store. `--default-embedder NAME` makes
it the embedder `embed()` uses without `#{embedder: ..}`:

```bash
graphersal --embedder potion=minishlab/potion-base-8M \
  -e 'g.add_v("doc").property("text", "a graph database stores vertices and edges")
        .add_v("doc").property("text", "banana bread recipe with walnuts")
        .add_v("doc").property("text", "traversal queries over a network of nodes").iterate();
      g.set_schema(g.infer_schema(), SchemaMode.open);
      g.embed_property(#{label: "doc", from: "text", to: "emb", embedder: "potion"});
      g.v().has_label("doc").nearest("emb", "how do I query connected data?", 2)
        .values("text").to_list()'
```

`embedder: "potion"` names the embedder registered by `--embedder potion=..`; without it the
default runs (`--default-embedder NAME`, else `hash-ngram/1`). The text query needs no embedder:
it uses the model the schema recorded for `doc.emb`.

```text
"traversal queries over a network of nodes"
"a graph database stores vertices and edges"
```

The model loads once when the command starts (a model that does not load stops it with exit
code 2 and the reason); the built-in `hash-ngram/1` stays registered beside it. `g.embedders()`
lists both, `GET /api/info` of the dev server too.

**What leaves the machine.** Only the explicit `embed-model download`: it connects to
`https://huggingface.co` (or the mirror in `$GRAPHERSAL_HF_ENDPOINT`). Loading and running a
model is local: no text is sent anywhere.

**Model identity.** Each embedder reports a model identity (`g.embedders()`, field `model`):
`model2vec/<repo>@<12 hex digits>`, the start of a SHA-256 over the model's three files (without
a manifest `model2vec@<12 hex digits>`). It names the model, never a path: a copied or moved
model directory keeps it, a changed file gets another one. Vectors are comparable only between
embedders of the same identity, so embed documents and questions with the same model.
`embed_property()` writes the identity into the schema itself. To declare a vector property by
hand, copy the full identity, hash included, from `graphersal embed-model list` or
`g.embedders()`: a schema naming a model without its hash matches no embedder (the error shows
the full identity of the one you tried), so a changed model file can never quietly mix its
vectors with the old ones.

**Licences.** A model's licence is the model's own, set by whoever published it (see its
Hugging Face page); Graphersal downloads and runs it but does not license it. `potion-base-8M`
is MIT licensed. A download keeps the model card (`README.md`), the repository's licence files
and the licence id beside the model, and warns about an unknown or non-commercial licence or a
gated repository; `embed-model list` shows the licences.

**A model inside a store.** `graphersal embed-model download minishlab/potion-base-8M --into
data/` puts the model into the Store `data/` instead: whenever the store is opened, the models
its schema names are loaded without `--embedder`, and every copy of the store carries them with
their licences (see [Embedding Models in a Store](../persistence/store_models.md)).

The Rust library has the same embedder as `graphersal::vector::Model2Vec` (feature
`model2vec`, native targets; it loads from a local directory only), see
[Vectors](../users_guide/vectors.md#embeddings).

## With real embeddings (Python)

With a real model the steps stay the same; only the vectors get longer. This example uses
[sentence-transformers](https://www.sbert.net/) and the small model `all-MiniLM-L6-v2`, which
gives 384 numbers per text and runs on a laptop CPU (`pip install graphersal
sentence-transformers`):

```python
import graphersal
from sentence_transformers import SentenceTransformer

model = SentenceTransformer("all-MiniLM-L6-v2")      # 384 dimensions

graph = graphersal.Graph()
graph.set_schema({
    "mode": "open",
    "vertices": {
        "chunk": {"schema": {"type": "object", "properties": {
            "text": {"type": "string"},
            "emb": {"type": "array", "format": "vector", "element": "f32",
                    "dimension": 384, "metric": "cosine"},
        }}},
    },
})

# The host's data: documents cut into chunks, plus the topics each document mentions.
docs = [
    {"title": "Cat care", "topics": ["pets", "health"], "chunks": [
        "Cats need fresh water every day.",
        "Most adult cats do well on two meals a day.",
    ]},
    {"title": "Weeknight pasta", "topics": ["cooking"], "chunks": [
        "Salt the pasta water generously.",
    ]},
]

for doc in docs:
    graph.execute('g.add_v("doc").property("title", title).iterate()', params={"title": doc["title"]})
    for topic in doc["topics"]:
        graph.execute(
            'g.merge_v(#{"T.label": "topic", name: topic}).as("t")'
            '.v().has("doc", "title", title).add_e("mentions").to("t").iterate()',
            params={"title": doc["title"], "topic": topic},
        )
    embs = model.encode(doc["chunks"])               # numpy float32, shape (n, 384)
    for text, emb in zip(doc["chunks"], embs):
        graph.execute(
            'g.v().has("doc", "title", title).as("d")'
            '.add_v("chunk").property("text", text).property("emb", emb)'
            '.add_e("part_of").to("d").iterate()',
            params={"title": doc["title"], "text": text, "emb": emb},
        )
```

A one-dimensional `float32` numpy array passed in `params` arrives in the script as an f32
vector (so does a `graphersal.Vector(values)`, built from a list or a buffer); the extension
reads it through `memoryview` and does not depend on numpy. A `list` in `params` is not
supported: wrap it in `graphersal.Vector(..)`.

The search embeds the question with the **same model** and passes it as a parameter:

```python
question = "How often should I feed my cat?"
q = model.encode(question)

hits = graph.query("""
    g.v().has_label("chunk").nearest("emb", q, 3)
     .project("text", "score")
       .by("text")
       .by(__.similarity("emb", q))
     .to_list()
""", params={"q": q})
# a list of dicts, best first: [{'text': '...', 'score': ...}, ...]
```

### GraphRAG from Python

The context for the language model is the hits plus what the graph knows about them, in one
query:

```python
rows = graph.query("""
    g.v().has_label("chunk").nearest("emb", q, 3)
     .project("text", "doc", "topics")
       .by("text")
       .by(__.out("part_of").values("title"))
       .by(__.out("part_of").out("mentions").values("name").fold())
     .to_list()
""", params={"q": q})

context = "\n".join(f"[{r['doc']}; topics: {', '.join(r['topics'])}] {r['text']}" for r in rows)
prompt = f"Answer from this context only.\n\n{context}\n\nQuestion: {question}"
# send `prompt` to the language model of your choice
```

To let an AI agent (an MCP client or the playground's AI Chat on the
[dev server](server_mcp_ai.md)) run the search itself, store it as a saved query whose parameter
is the vector; the agent still needs the question's embedding from a tool of the host:

```python
graph.define_query({
    "name": "semantic_search",
    "params": {
        "q": {"schema": {"type": "array", "format": "vector", "dimension": 384}},
        "k": {"schema": {"type": "integer"}, "default": 5},
    },
    "body": 'fn semantic_search(q, k) { g.v().has_label("chunk").nearest("emb", q, k).values("text") }',
})
graph.call_query("semantic_search", {"q": graphersal.Vector(q), "k": 2})
```

When the host registers its model as an embedder and the documents were embedded with
`embed_property()`, the schema knows the model, and the parameter can be the question itself:
`nearest("emb", question, k)` with `params: {"question": "string"}` embeds it with that model, so
the agent needs no embedding tool at all. From Python that is
`graphersal.Embedder("minilm", model.encode, dimension=384, model="sentence-transformers/all-MiniLM-L6-v2")`
in `Graph(embedders=[..])`, then `graph.embed_property(label="chunk", source="text",
target="emb")` ([Python Quick Start: Embedders](../library/python.md#embedders)).

**Any model works.** OpenAI, Cohere, Ollama or a model of your own: an embedding is just a list
of numbers. Keep the schema's `dimension` equal to the model's, embed documents and questions
with the same model, and pick the metric the model is meant for (most text models: `cosine`).

### Sizing and performance

`nearest()` scores every element it receives, with no index to build or keep up to date, so its
time grows with the number of vectors times the dimension. The
[measured numbers](../users_guide/vectors.md#performance) (10 000 vectors of 384 dimensions)
show that this is fast enough for most in-memory graphs; a filter before the search scores
fewer, and an `i8` property takes a quarter of the memory. There is no approximate (ANN) index
yet ([Current Limitations](../users_guide/current_limitations.md)).

## Where to go next

- [Vectors](../users_guide/vectors.md): the reference: element kinds, metrics, `similarity()`,
  `nearest()`, `vector_mean()`, quantization, formats.
- [Schema Enforcement](../users_guide/schema_enforcement.md): declaring properties, modes and
  migrations.
- [Python Quick Start](../library/python.md): the Python API.
- [Saved Queries](../users_guide/saved_queries.md): searches that hosts, MCP clients and the AI
  Chat call by name.
- The playground's **Examples ▾** menu has a group "Embeddings (vectors)" with these queries.
