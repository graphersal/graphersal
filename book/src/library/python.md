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

## Embedding models

<a id="embedders"></a>The step `embed()`, the function `embed(text)` and text queries
(`nearest("emb", "a question", 5)`) compute vectors with the built-in `hash-ngram/1` or with a
model kept in the graph's database ([Vectors: Embeddings](../users_guide/vectors.md#embeddings)).
A model serves a database only once it is imported into it:

```python
import graphersal

with graphersal.Store.create("kb") as store:
    # A repository downloaded before (`graphersal embed-model download minishlab/potion-base-8M`)
    # is found in the download cache; a model directory works as well.
    report = store.import_model("minishlab/potion-base-8M", default=True)
    report["identity"]      # 'model2vec/minishlab/potion-base-8M@<12 hex digits>'
    store.graph.embedders() # hash-ngram/1 and the store's model, the default first
```

`store.import_model(source, *, repo=None, license=None, replace=False, default=False)` takes a
repository name found in the download cache (`$GRAPHERSAL_EMBED_MODELS`, else
`$XDG_CACHE_HOME/graphersal/embed-models`, else `~/.cache/graphersal/embed-models`) or a
directory holding a Model2Vec model; `repo=`/`license=` fill what the directory does not say,
`replace=True` replaces another revision of the same repository (vector rules naming it are
re-embedded in the same commit), `default=True` makes it the default model. It returns the report
as a dict and releases the GIL while it runs. `graphersal.Store.import_model_into(path, source,
..)` does the same for a CLOSED store without loading its graph. The wheel has no download
client: fill the cache with the `graphersal` command line (`graphersal embed-model download
<repo>`), once for any number of databases.

A store opens with its models: the ones the schema needs are loaded at the open, the others at
the first query that uses them. `Graph.load("file.gsnap")` serves the models the file carries
(a `UserWarning` names what an earlier release left, such as vectors without a rule).
`graph.embedders()` lists the embedders of the graph by repository, with their `model` identity.

**Filling a property and searching by text.** A [vector rule](../users_guide/vector_rules.md)
(`"from"`) fills and refreshes the vectors by itself with `hash-ngram/1` or a model of the
store; the schema records the model, which is what a text query embeds the question with:

```python
store.graph.patch_schema({"vertices": {"chunk": {"schema": {"type": "object", "properties": {
    "emb": {"type": "array", "format": "vector", "embedder": "minishlab/potion-base-8M",
            "from": "text"}}}}}})
store.graph.execute("""
    for t in ["Cats need fresh water every day.", "Salt the pasta water generously."] {
        g.add_v("chunk").property("text", t).iterate();
    }
""")
store.graph.query('g.v().has_label("chunk").nearest("emb", question, 5).values("text").to_list()',
                  params={"question": "How often should I feed my cat?"})
```

**A model of your own** (sentence-transformers, a remote API) stays in your program: compute the
vectors there and write them as values (a 1-D `float32` numpy array or a `graphersal.Vector` in
`params`), and pass the question's vector to `nearest` ([Embeddings and Vector
Search](../getting_started/embeddings.md#with-real-embeddings-python)).

Changed in 0.5.0: `graphersal.Embedder`, `graphersal.Model2Vec`, the `embedders=`/
`default_embedder=` keywords of every constructor, loader and Store opener, and
`graph.register_embedder` are removed (a removed keyword raises Python's `TypeError`); the job
`embed_property` was removed as well (vector rules replace it).

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
