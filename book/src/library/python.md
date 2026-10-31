# Python Quick Start

The Python package `graphersal` embeds the engine into the Python process. Queries are Gremlin
text, the same DSL as everywhere else, and results come back as plain Python values (`list`,
`dict`, `int`, `float`, `str`, `bool`, `None`, `uuid.UUID`, and the `datetime` types for dates and
times, see [Dates and Times: From Python](../dates/python.md)); a vertex or an edge comes back as a
`graphersal.Vertex` / `graphersal.Edge` (see [Vertices and Edges as Values](../users_guide/vertex_and_edge_values.md#in-python)).

## Install

Build the extension from the repository (Python 3.10 or newer, one wheel covers every later
version):

```bash
cd bindings/py-graphersal
python3.12 -m venv .venv && .venv/bin/pip install maturin
VIRTUAL_ENV=$PWD/.venv .venv/bin/maturin develop --release     # installs `graphersal` into .venv
```

`maturin build --release` writes a wheel to install elsewhere; details are in
`bindings/py-graphersal/README.md`.

## Query

```python
import graphersal

graph = graphersal.Graph.tinkerpop_modern()      # or graphersal.Graph() for an empty graph

graph.query('g.V().has("name", "marko").out("knows").values("name")')
# ['josh', 'vadas']

graph.query('g.V().hasLabel("person").group().by("name").by("age").next()')
# {'josh': [32], 'marko': [29], 'peter': [35], 'vadas': [27]}
```

A traversal the script returns is run for you; inside a longer script, end each traversal with a
terminal (`.next()`, `.to_list()`, `.iterate()`).

## Parameters

Pass values as parameters instead of pasting them into the text; they arrive as script variables:

```python
graph.query('g.V().has_label("person").has("age", P.gt(min_age)).values("name")',
            params={"min_age": 30})
# ['josh', 'peter']
```

## Write

Every query is one unit: when it fails, nothing it wrote stays, and the error is a
`graphersal.GraphersalError` with the diagnostic and its help text.

```python
graph.execute('g.addV("person").property("name", "ann").property("age", 41).iterate()')

try:
    graph.execute('g.V().has("name", "ann").property("age", 42).fail("stop").iterate()')
except graphersal.GraphersalError as err:
    print(err)                                   # the failing step and how to fix it

graph.query('g.V().has("name", "ann").values("age").next()')
# 41: the failed query changed nothing
```

## Files and persistence

```python
graph.export_graphson("people.json")             # GraphSON 3.0; TinkerGraph reads it
graph = graphersal.Graph.from_graphson("people.json")

with graphersal.Store.create("data") as store:   # a graph on disk, every commit durable
    store.graph.execute('g.addV("person").property("name", "bob").iterate()')
```

## Embedders

An embedder turns text into vectors inside the engine: the step `embed()`, the function
`embed(text)`, the job `embed_property` and text queries (`nearest("emb", "a question", 5)`)
call the embedders a graph has registered ([Vectors: Embeddings](../users_guide/vectors.md#embeddings)).
The built-in `hash-ngram/1` is always there; Python adds its own two ways.

**Any Python model** becomes an embedder through `graphersal.Embedder(name, fn, *, dimension,
..)`. `fn` gets a `list[str]` (at most `max_batch` texts) and returns one row per text: a list of
rows or one 2-D `float32`/`float64` numpy array of shape `(len(texts), dimension)` (read through
`memoryview`; the extension does not depend on numpy). With
[sentence-transformers](https://www.sbert.net/):

```python
import graphersal
from sentence_transformers import SentenceTransformer

st = SentenceTransformer("all-MiniLM-L6-v2")
minilm = graphersal.Embedder(
    "minilm",
    lambda texts: st.encode(texts),          # numpy float32, shape (len(texts), 384)
    dimension=384,
    model="sentence-transformers/all-MiniLM-L6-v2",
    max_batch=64,
)
graph = graphersal.Graph(embedders=[minilm], default_embedder="minilm")
```

`model` (default: the name) is the **model identity** the schema records for the vectors the
embedder produces, and what a text query looks up later: keep it stable across runs and hosts,
and change it whenever the model (or its version) changes. Wrong answers (another number of rows
or of values) and exceptions of `fn` (also `KeyboardInterrupt`) fail the query with
`GraphersalError`, the exception's type and message in the text.

**Model2Vec models** load from a local directory through `graphersal.Model2Vec(name, path, *,
model=None, max_tokens=512, normalize=None, max_batch=256)` (semantic, fast on a CPU, no
PyTorch). The library never downloads: fetch the files however you like, for example with
`huggingface_hub`, and pass the model's name together with the directory:

```python
from huggingface_hub import snapshot_download

path = snapshot_download("minishlab/potion-base-8M")
potion = graphersal.Model2Vec("potion", path, model="minishlab/potion-base-8M")
potion.model         # 'model2vec/minishlab/potion-base-8M@<12 hex digits>'
graph.register_embedder(potion, default=True)
```

The identity `model2vec/<model>@<12 hex>` hashes the three model files, so it is the same as a
`graphersal embed-model download minishlab/potion-base-8M` of the same files: a graph embedded
with the command line finds its model in Python and the other way round. Without `model=` the
directory's `graphersal-embed-model.json` (written by that download) names the model, else the
identity is `model2vec@<12 hex>`; a `model=` that differs from that file raises. Loading
releases the GIL.

**The registry.** Every graph constructor and loader (`Graph()`, `Graph.tinkerpop_modern`,
`from_graphml`, `from_graphson`, `load`, `recover`) and every `Store` opener take
`embedders=[..]` and `default_embedder=`; `graph.register_embedder(e, default=False)` adds one
later (a running execution keeps the embedders it started with); a duplicate or reserved name
(`hash-ngram`, `hash-ngram/*`) raises `ValueError`. `graph.embedders()` lists them, the default
first, with their `model`. Scripts see the registry in `execute`, `query`, `dry_run` and
`atomic=True` runs.

**Embedding a property and searching by text.** `embed_property` needs a schema attached (mode
`open` or `closed`): it declares the target property as vectors of the embedder's model, which
is what a text query embeds the question with:

```python
graph.execute("""
    for t in ["Cats need fresh water every day.", "Salt the pasta water generously."] {
        g.add_v("chunk").property("text", t).iterate();
    }
""")
graph.set_schema(graph.infer_schema(), "open")
report = graph.embed_property(label="chunk", source="text", target="emb")
# {'embedded': 2, 'units': 1, 'schema_changed': True, 'embedder': 'minilm',
#  'model': 'sentence-transformers/all-MiniLM-L6-v2', ...}

graph.query('g.v().has_label("chunk").nearest("emb", question, 5).values("text").to_list()',
            params={"question": "How often should I feed my cat?"})
```

The job reads the texts under a short read lock, runs the embedder without any lock of the graph
(other threads keep reading and writing) and writes each page of `batch` elements (default the
embedder's `max_batch`) as one unit; the GIL is released for the whole job. A second run embeds
only the elements still missing the property (`only_missing=False` re-embeds all). A failure
raises `graphersal.EmbedPropertyError`, whose `report` holds the counts up to it (earlier pages
stay); Ctrl+C cancels the job between pages and the `KeyboardInterrupt` carries the `report` too.

**Threads and the GIL.** A Python embedder runs while a traversal holds the graph's lock (the
`embed` step) and takes the GIL for the call. No method of the binding waits for the graph's
lock while it holds the GIL, so a sleeping embedder and other Python threads writing to the
same graph never wait for each other forever.

**Reference cycles.** A graph keeps its embedders (and so their Python functions) alive, and
Python's garbage collector cannot see that reference, which is held by the engine. An embedder
whose function captures its own graph (`Embedder("e", lambda t: use(graph, t), ..)`) therefore
forms a cycle that is never collected: that graph and its data stay in memory until the process
ends. Let the function capture the model only, not the graph; this matters only for a program
that creates and drops many graphs.

**Threads of Model2Vec.** The tokenizer (`tokenizers`, through `rayon`) starts a thread pool of
one thread per CPU core on the first batch it encodes. `TOKENIZERS_PARALLELISM=false` in the
environment (set before the first embedding) keeps it on the calling thread, for example in a
program that forks worker processes after embedding or that limits its CPU use.

## Vector rules

A [vector rule](../users_guide/vector_rules.md) in the schema keeps a vector property embedded
from a text property. `graph.set_schema(..)` and `graph.patch_schema(..)` issue
`graphersal.VectorFillWarning` (a `UserWarning`) with the preview's text BEFORE a change that
fills vectors, because the whole graph is locked while the fill runs; `graph.validate_schema(..)
["fill"]` lists the planned fills. Turn the warning into an error to stop such a change:

```python
import warnings
import graphersal

with warnings.catch_warnings():
    warnings.simplefilter("error", graphersal.VectorFillWarning)
    graph.set_schema(schema)   # raises before anything is applied when it would fill vectors
```

## Where to go from here

- `bindings/py-graphersal/README.md`: every method, schemas, access policies, limits;
- [Python](../persistence/python.md): snapshots, the journal and the Store;
- [Embeddings and Vector Search](../getting_started/embeddings.md) and
  [Vectors](../users_guide/vectors.md): vector search and embedders;
- [Writing Queries](../dsl/writing_queries.md): the DSL.
