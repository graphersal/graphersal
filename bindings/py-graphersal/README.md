# graphersal

An in-memory graph database and traversal engine for Python, queried with Gremlin, the query
language of Apache TinkerPop. The engine is [Graphersal](https://graphersal.dev), written in Rust
and embedded into the Python process: no server, no JVM.

**[Book](https://graphersal.dev)** · **[Python Quick Start](https://graphersal.dev/library/python.html)** ·
**[Playground](https://play.graphersal.dev)** (try the queries in the browser) ·
**[Repository](https://github.com/graphersal/graphersal)**

You write Gremlin as script text (the same DSL as the playground and the command line tool) and
run it against a `Graph`; results come back as native Python values
(`list`/`dict`/`int`/`float`/`str`/`bool`/`None`/`uuid.UUID`, the `datetime` types for dates
and times, and `graphersal.Vertex` / `graphersal.Edge` for vertices and edges). Graph files are GraphSON 3.0 (the
primary exchange format for Gremlin users) and GraphML; graphs can also be kept on disk with
durable commits (the Store).

**Vector search for GraphRAG.** Store the embeddings your model computes (a list or a `numpy`
float32 array becomes a `graphersal.Vector`; `f32`, or quantized to `f16`, `i8`, `bit`), find the
passages nearest to a question with `nearest()` and follow the graph from them in the same query:
the context for your language model in one call. The guide, with a sentence-transformers example:
[Embeddings and Vector Search](https://graphersal.dev/getting_started/embeddings.html).

## Installing

```bash
pip install graphersal
```

Wheels exist for Linux (x86_64, aarch64), macOS (Apple silicon) and Windows (x86_64). One
wheel per platform covers Python 3.10 and every newer version (the stable ABI, `abi3`). On another
platform pip builds the package from source, which needs a Rust toolchain (1.88 or newer).

## Quick start

```python
import graphersal

graph = graphersal.Graph()
graph.execute('g.addV("person").property("name", "Alice").next()')
names = graph.query('g.V().values("name").to_list()')
print(names)  # ['Alice']
```

Script text follows this project's Gremlin/Rhai syntax: string literals use **double quotes**
(single quotes are Rhai character literals), and both snake_case (`has_label`) and Gremlin
camelCase (`hasLabel`) step spellings work. Inside a script a traversal only runs when it ends
in a terminal — `.next()`, `.to_list()` or `.iterate()` (runs for the side effects, returns
`None`) — including mutations: a statement `g.addV("person");` in the middle of a script builds a
traversal object and does nothing; add `.iterate()`. A traversal
the script *returns* without a terminal (also inside a returned list or map) is run for you, as
if it ended in `.to_list()`: `graph.execute('g.V().values("name")')` returns the list of names.

`execute()` and `query()` are exactly the same method under two names; `query` is offered for
read-oriented call sites (it does not restrict the script by itself, see
[Access policy](#access-policy)). Both take the keywords `atomic=` ([Units of
atomicity](#units-of-atomicity)) and `policy=` ([Access policy](#access-policy)); type stubs
(`graphersal.pyi`) ship with the package.

The DSL terminals `.profile()` and `.execute()` come back as dicts with snake_case keys
(durations as integer nanoseconds, `duration_ns`); see the book page "Profiling and execute()":

```python
metrics = graph.execute('g.V().out().profile()')
metrics["metrics"][0]["counts"]["traverser_count"]

run = graph.execute('g.V().values("name").sum().execute(#{profile: true})')
run["error"]["step_id"]   # "2": the failing step, numbered like metrics[..]["id"]
run["profile"]            # the profile up to the failing step
```

A DSL `.execute()` never raises: its error is part of the dict. A failing `.profile()` raises
`GraphersalError` like any other terminal.

## Creating a graph

```python
import graphersal

# An empty graph.
graph = graphersal.Graph()

# The standard TinkerPop "modern" toy graph (6 vertices, 6 edges), useful for trying things out.
graph = graphersal.Graph.tinkerpop_modern()
```

## Loading and exporting GraphSON and GraphML

GraphSON 3.0 is what TinkerPop's `g.io("graph.json").write()` produces (one vertex per line):

```python
import graphersal

graph = graphersal.Graph.from_graphson("path/to/graph.json")
graph.export_graphson("path/to/out.json")   # TinkerGraph loads it with g.io(path).read()
```

GraphSON values carry their types (`uuid`, dates and times, lists and maps come back as such).
Data Graphersal cannot hold (types without a Graphersal twin) is skipped, never converted, and
named in one `UserWarning`. Multi-valued properties become lists, meta-properties go into the
`_meta` property; see the book's GraphSON page for every mapping.

GraphML:

```python
graph = graphersal.Graph.from_graphml("path/to/graph.xml")

# ... query or mutate ...

graph.export_graphml("path/to/out.graphml")
```

GraphML carries no schema. Without one, values come back with their GraphML types: a UUID, a list
or a map is a string. To restore declared types (`uuid`, `array`, `object`), pass the schema to the
load: it is set on the new, empty graph first, then the file is imported into it as one unit (in
`open`/`closed` mode a violation fails the load with the usual report):

```python
typed = graphersal.Graph.from_graphml(
    "path/to/out.graphml",
    schema=open("path/to/schema.json").read(),   # a dict or JSON text
    mode="closed",                               # optional: replaces the schema's own mode
)
```

`Graph.from_graphson` takes the same `schema=`/`mode=` keywords. To add a file to a graph you
already have, import it into the live graph; each import is one unit (a file that fails to parse
or violates the schema leaves the graph unchanged):

```python
graph.import_graphml("path/to/more.graphml")    # declared types restored from graph's schema
graph.import_graphson("path/to/more.json")      # skipped data: one UserWarning
```

## Saving, journal and recovery (packed snapshots)

Graphersal's own binary format keeps everything (every value type, the schema, the graph's
commit position) in one file, and a journal makes every committed change durable before it is
final (book chapter "Keeping a Graph on Disk", page "Python"):

```python
graph.save("g.gsnap")                                     # {"commit_seq", "vertex_count", "edge_count"}
graph = graphersal.Graph.load("g.gsnap")                  # no schema re-validation, checksummed

graph.start_journal("g.wal")                              # durability="every_commit" | "os" | seconds
graph.execute('g.addV("person").property("name", "ann").next()')
graph.mark("after-ann")                                   # a named point in the journal

graph, report = graphersal.Graph.recover("g.gsnap", ["g.wal"])            # to the end
graph, report = graphersal.Graph.recover("g.gsnap", ["g.wal"], mark="after-ann")
```

`Graph.recover` also takes `commit_seq=` or `time=` (microseconds since the Unix epoch). An
interrupted last write is ignored (`report["torn_tail"]`); damage raises `GraphersalError` with
its location. The recovered graph has no journal: `save` and `start_journal` again to continue.

## Keeping a graph on disk: the Store

A Store is one directory (or one `.gstore` file) holding one graph: every commit is durable before
it returns, and the store keeps marks, snapshots, point-in-time forks and verified backups (book
page "Persistence: Python"):

```python
with graphersal.Store.create("data") as store:            # or Store.open("data"); "x.gstore": one file
    store.graph.execute('g.addV("person").property("name", "ann").next()')   # durable when it returns
    store.mark("after-ann")
    store.checkpoint("nightly")
    store.fork("branch", mark="after-ann")                 # a new store from that state
    store.backup("bk")                                     # full, then incremental
```

Every graph has a system journal of operational events (store opened or closed, checkpoints,
backups, damage, permissions denied; book page "System Journal"). `graph.journal(...)`,
`store.journal(...)` and, for a closed store, `Store.journal_dir(path, ...)` answer one page
`{"records", "next_after", "truncated", "more", "notes"}`, records oldest first with the keys of
`graphersal store journal --json`; filters `after` (the cursor), `since`/`until`, `level`,
`source`, `kind`, `op`, `origin`, `limit`, and `policy=` (needs `("read", "journal")`):

```python
page = store.journal(level="warn")
newer = store.journal(after=page["next_after"])            # only what came since
graphersal.Store.journal_dir("data", kind="store.closed")  # no lock, nothing written
```

## Schemas

A schema is a `dict` (or JSON text) in the library's schema format: an envelope with `mode`
(`"none"`, `"open"`, `"closed"`), `vertices`, `edges` and a free `meta`, around standard JSON
Schema 2020-12 label schemas, so a Pydantic `Model.model_json_schema()` can be used as a label's
schema (drop `dict[str, T]` fields first: there is no map-of-T type; the `date-time`, `date`,
`time` and `duration` formats are supported, and a stored `date-time` needs a UTC offset). The
full reference is the book page "Schemas".

A vector property with `"from": "<text property>"` beside its `"embedder"` is a vector rule
(book page "Vector Rules"): every write of the text embeds it into the vector in the same unit.
`set_schema`/`patch_schema` issue `graphersal.VectorFillWarning` (a `UserWarning`) with the
preview's text before a change that fills existing elements, because the whole graph is locked
while that runs; `validate_schema(..)["fill"]` lists the planned fills.

```python
graph = graphersal.Graph.tinkerpop_modern()
graph.set_schema(graph.infer_schema(), mode="closed")  # freeze the structure the graph has
schema = graph.infer_schema()          # what the data satisfies; stores nothing
schema["mode"] = "closed"
report = graph.validate_schema(schema) # dry run: {"violations", "diff", "compatible"}
graph.set_schema(schema)               # validates the data first; on violations nothing changes
graph.patch_schema({"vertices": {"person": {"schema": {"properties": {"email": {"type": "string"}}}}}})
graph.diff_schema(schema)              # {"changes": [...], "compatible": bool}
graph.get_schema()                     # the stored schema, or None
graph.set_schema({})                   # removes it
```

| Method | Returns |
|---|---|
| `get_schema()` | the stored schema, or `None` (never inferred) |
| `set_schema(schema, mode=None)` | `None`; replaces the stored schema (`mode`: `"none"`/`"open"`/`"closed"` replaces the schema's own mode) |
| `patch_schema(patch)` | the new schema (JSON Merge Patch, RFC 7396) |
| `validate_schema(schema, mode=None)`, `validate_schema_patch(patch)` | `{"violations": {...}, "diff": [...], "compatible": bool}`; applies nothing |
| `infer_schema()` | the schema the data satisfies (mode `"none"`) |
| `diff_schema(schema)` | `{"changes": [...], "compatible": bool}` from the stored schema |

Every method takes `policy=` like `execute()` (read methods need `("read", "schema")`, `set`/`patch`
need `("update", "schema")`). A violation raises `GraphersalError` with the aggregated report and a
`help`. A schema `default` (or `defaultFunction`: `uuidv4`, `uuidv7`, `now`, `today`) of a top-level property is
stored when a vertex or edge is created without it (mode `open`/`closed`), never substituted on
read and never filled into existing elements. To make a key required that existing elements lack, backfill first in one
atomic script (patch to optional, write the values, patch to required):

```python
graph.execute('''
    g.patch_schema(#{vertices: #{person: #{schema: #{properties: #{email: #{type: "string"}}}}}});
    g.V().hasLabel("person").property("email", "unknown").toList();
    g.patch_schema(#{vertices: #{person: #{schema: #{required: ["name", "age", "email"]}}}});
''', atomic=True)
```

The schema methods are also in the script DSL (`g.get_schema()`, `g.set_schema(..)`, ...), and a
run reports what it changed: `execute()` of a script ending in `.execute()` returns
`changes: {"data": bool, "schema": bool, "definitions": bool}`.

## Saved queries

Named, parameterized queries stored in the graph (book page "Saved Queries"); a query whose
body writes data is saved as one that writes, and a call changes only what its `effects` name, as
one transaction:

```python
graph.define_query({
    "name": "older_than",
    "folder": "people",
    "params": {"age": {"schema": {"type": "integer", "minimum": 0}, "default": 30}},
    "body": 'fn older_than(age) { g.v().has_label("person").has("age", P.gt(age)).values("name") }',
})
graph.call_query("older_than", {"age": 29})   # ["josh", "peter"]; graph.query stays execute's alias
graph.queries("people")                        # [{"name", "folder", "description", "params", "status", "writes", "effects", ...}]
graph.get_query("older_than")["body"]
graph.drop_query("older_than")                 # True
```

Each wraps the DSL call (`g.define_query`, `g.query`, `g.queries`, `g.get_query`,
`g.drop_query`) and takes `policy=` like `execute()`.

## Creating vertices and edges

```python
import graphersal

graph = graphersal.Graph()

# addE()'s from()/to() take step labels (set with as(...)), not nested traversals.
graph.execute(
    'g.addV("person").property("name", "Alice").property("age", 30).as("a")'
    '.addV("person").property("name", "Bob").as("b")'
    '.addE("knows").from("a").to("b").next()'
)
```

## Query examples

```python
import graphersal

graph = graphersal.Graph.tinkerpop_modern()

# A plain list of scalar values.
names = graph.query('g.V().hasLabel("person").values("name").to_list()')
# ['marko', 'vadas', 'josh', 'peter']

# elementMap() turns each vertex/edge into a dict.
people = graph.query('g.V().hasLabel("person").elementMap().to_list()')
# [{'id': '1', 'label': 'person', 'name': 'marko', 'age': 29}, ...]

# A vertex or an edge comes back as a Vertex / Edge object, like gremlin-python's: its own id
# and label, and its properties apart (a property named "label" is v["label"], never v.label).
marko = graph.query('g.V("1").next()')
# v[1]; marko.id == '1', marko.label == 'person', marko["name"] == 'marko',
# marko.properties == {'name': 'marko', 'age': 29}
knows = graph.query('g.E("0").next()')
# e[0][1-knows->2]; knows.out_v == marko, knows.in_v.id == '2', knows["weight"] == 0.5

# next() returns a single value (or None if the traversal yields nothing).
count = graph.query('g.V().hasLabel("person").count().next()')
# 4

# A one-hop traversal.
who_marko_knows = graph.query(
    'g.V().has("name", "marko").out("knows").values("name").to_list()'
)
# ['josh', 'vadas']
```

## Parameterized queries

Pass a `params` dict of `int`, `float`, `str`, `bool`, `None`, `uuid.UUID` or date and time
values (see [Dates and times](#dates-and-times)), or a `Vertex`/`Edge` a query returned (it stands for its element: `g.V(v)`, `inject(v)`). Each one is
bound as both a script variable of the same name and an entry of the `params` map, so user input
never needs to be spliced into the script string:

```python
import graphersal

graph = graphersal.Graph.tinkerpop_modern()

names = graph.query(
    'g.V().hasLabel("person").has("age", params.min_age).values("name").to_list()',
    params={"min_age": 32},
)
# ['josh']
```

`list`/`dict` values in `params` are not supported and raise `GraphersalError`, as does an `int`
outside the int64 range (it is never widened to a float). A key that is not a `str` raises
`TypeError`.

## Dates and times

The engine's six date and time types (book part "Dates and Times") convert both ways. Python's
`datetime` counts microseconds, the engine nanoseconds: a value with digits below the
microsecond comes back as a `pandas.Timestamp` / `pandas.Timedelta` (subclasses of `datetime` /
`timedelta` that hold nanoseconds) when the optional extra is installed
(`pip install graphersal[pandas]`), else it raises `ImportError` naming the extra. Nothing is
ever rounded, and the type depends on the value only.

| Graphersal | Python | With digits below the microsecond |
|---|---|---|
| `datetime` (an instant with its UTC offset) | aware `datetime.datetime`, `tzinfo=datetime.timezone(timedelta(seconds=offset))` | aware `pandas.Timestamp`, the same `tzinfo` |
| `datetime` with a zone name (`..+02:00[Europe/Berlin]`) | aware `datetime.datetime`, `tzinfo=zoneinfo.ZoneInfo(name)` | aware `pandas.Timestamp` in the `ZoneInfo` |
| `localdatetime` (a wall-clock time) | naive `datetime.datetime` | naive `pandas.Timestamp` |
| `date` | `datetime.date` | - |
| `time` (a time of day) | naive `datetime.time` | `ValueError` (no Python type holds a time of day in nanoseconds) |
| `offsettime` (a time of day with its UTC offset) | `datetime.time` with `tzinfo=datetime.timezone(..)` | `ValueError` |
| `duration` without months | `datetime.timedelta` | `pandas.Timedelta` (an `Interval` beyond an int64 of nanoseconds, about 292 years) |
| `duration` with months (`P1M`) | `graphersal.Interval` | `graphersal.Interval` |

A `pandas.Timestamp` holds nanoseconds only from 1677-09-21 to 2262-04-11: such a value outside
those years raises (read it with `as_string()`, or `date_trunc(DT.microsecond)` it first).

```python
import datetime as dt
import graphersal

graph = graphersal.Graph()
graph.execute(
    'g.add_v("event").property("at", at).property("took", took).iterate()',
    params={
        "at": dt.datetime(2023, 8, 2, 9, 30, tzinfo=dt.timezone(dt.timedelta(hours=2))),
        "took": dt.timedelta(minutes=90),
    },
)
graph.query('g.V().values("at").next()')
# datetime.datetime(2023, 8, 2, 9, 30, tzinfo=datetime.timezone(datetime.timedelta(seconds=7200)))
graph.query('g.V().values("at").date_add(DT.month, 1).as_string().next()')
# '2023-09-02T09:30:00+02:00'
graph.query('duration("P1Y2M")')
# Interval('P1Y2M')
```

- An aware `datetime` is stored with the UTC offset it has at that instant; with a
  `zoneinfo.ZoneInfo` the zone name is kept too (a name the bundled database does not know is
  refused), and it comes back as a `ZoneInfo` datetime; any other `tzinfo` comes back as a
  `datetime.timezone`. An offset with a fraction of a second is refused. Returning a zoned value
  needs the zone in Python's own database (`tzdata` on systems without one), else it raises.
- An aware `datetime.time` with a fixed offset (`datetime.timezone`) is an `offsettime`; one whose
  `tzinfo` is a `ZoneInfo` is refused: a zone's offset needs a date. Pass a fixed offset or an
  aware `datetime`.
- A `timedelta` becomes a duration of whole days and the rest, both with the sign of the whole:
  `timedelta(hours=-1)` is `-PT1H` (Python itself writes it as `days=-1, seconds=82800`); a
  duration comes back as the equal `timedelta`, or as an `Interval` when it is longer than a
  `timedelta` holds (999999999 days; a duration's time part reaches an int64 of seconds).
- `graphersal.Interval(months, days=0, micros=0)` or `graphersal.Interval("P1Y2M3DT4H")` (up to
  nine fraction digits) holds a duration with months (a month has no fixed length, so
  `timedelta` cannot). It is immutable, equal when all three fields are equal (`P1M` is not
  `P30D`, as in the engine), and has `months`, `days`, `micros` (exact, also beyond an int64;
  `ValueError` for a time part with digits below the microsecond), `nanos` and `to_iso()`.
- `pandas.Timestamp` and `pandas.Timedelta` parameters keep their nanoseconds; a `pytz` zone
  keeps its name like a `ZoneInfo`; `pandas.NaT` is refused (pass `None`).
- Python's `datetime` holds the years 1 to 9999; a Graphersal date before year 1 raises
  `GraphersalError` when it is returned (read it as text with `as_string()`).
- Saved queries (`call_query`) take the same values; they travel as their ISO 8601 text, and the
  parameter's schema (`#{type: "string", format: "date-time"}`, ...) reads them back.

## Vectors

A vector property (an embedding) is a `graphersal.Vector` both ways. Build one from a list of
numbers or from any one-dimensional `float32`/`float64` buffer, such as a `numpy` array (read
through `memoryview`: the extension does not depend on `numpy`). Embeddings are computed outside
the engine, by your model; the engine stores, quantizes and compares them.

```python
import numpy as np
from graphersal import Graph, Vector

g = Graph()
emb = np.array([0.12, -0.5, 0.33], dtype=np.float32)      # model.encode(text) in practice
g.execute('g.add_v("doc").property("title", t).property("emb", e).iterate()',
          params={"t": "Graph databases 101", "e": Vector(emb)})

v = g.query('g.v().values("emb").next()')                # graphersal.Vector
v.element, v.dimension, v.tolist()                       # ('f32', 3, [0.12, -0.5, 0.33])
small = Vector(emb, element="i8")                        # quantized: 1 byte per component
back = np.array(v.tolist(), dtype=np.float32)
```

- `Vector(values, element="f32")`; the elements are `"f32"`, `"f16"`, `"i8"` and `"bit"`.
  `v.quantize("i8")` converts, `quantize("f32")` dequantizes. A NaN, an infinity, an empty list
  or more than 65536 components raise `ValueError`.
- A vector is immutable, hashable and equal when element, dimension and components are equal.
- A schema property `{"type": "array", "format": "vector", "element": "i8", "dimension": 384}`
  turns a written list (or a vector of another element) into the declared vector in mode
  `open` or `closed`; saved queries (`call_query`) take a `Vector` argument the same way.
- A parameter may also be a one-dimensional `float32`/`float64` numpy array: it arrives as an
  f32 vector, so a query embedding goes straight into an exact search:

```python
q = model.encode("how do graph indexes work?")           # np.ndarray, float32
g.query('g.v().has_label("doc").nearest("emb", q, 10).values("title").to_list()', params={"q": q})
g.query('g.v().has_label("doc").similarity("emb", q).to_list()', params={"q": Vector(q)})
```

## Embedding models

An embedder turns text into vectors inside the engine: the step `embed()`, the function
`embed(text)`, text queries (`nearest("emb", "a question", 5)`) and vector rules (`"from"` in
the schema) use the built-in `hash-ngram/1` (hashed words and character n-grams, no model file)
or an embedding model kept IN THE DATABASE (book page "Embedding Models in a Store"). A model
serves a database only once it is imported into it: the store keeps the model's files as one
blob with its licence, every copy of the store carries it, and every program that opens the
store gets the same model.

```python
# Download once into the cache (the command line; the wheel has no HTTP client):
#   graphersal embed-model download minishlab/potion-base-8M
with graphersal.Store.open("data") as store:
    report = store.import_model("minishlab/potion-base-8M", default=True)  # from the cache
    print(report["model"]["identity"], report["model"]["license"])
    store.graph.query('embed("a question").dimension')                   # 256
    store.graph.embedders()        # hash-ngram/1 and the store's models, the default first
```

`store.import_model(source, *, repo=None, license=None, replace=False, default=False)` takes a
repository name found in the download cache (`$GRAPHERSAL_EMBED_MODELS`, else the user cache
`~/.cache/graphersal/embed-models`; one download feeds any number of databases without the
network) or a model directory (`config.json`, `tokenizer.json`, `model.safetensors`; `repo=`
names its repository when it has no download manifest). `replace=True` replaces another revision
of the repository (vector rules naming the old one are re-embedded in the same commit:
`report["reembedded"]`), `default=True` makes it the model `embed` uses without a name
(`store.set_default_model(repo)` later). `Store.import_model_into(path, source, ...)` imports
into a CLOSED store without loading its graph. `store.models()` lists the models with their
licences and states; an open of a store that holds models but none of them the default is a
`UserWarning`.

A `.gsnap` exported with its models (`store.export(path)`) serves them too when loaded without
a store (`Graph.load(path)`; a model reads its blob from the file when it first loads, so keep
the file in place); a graph that has neither has
`hash-ngram/1` only. What a file of an earlier release left that this version does not keep
current (vectors without a vector rule, a model the schema names that the file lacks, saved
queries calling a removed function) is one `UserWarning` per note.

**Searching by text.** The schema says which model made a property's vectors; a vector rule
fills and refreshes them by itself:

```python
graph.patch_schema({"vertices": {"chunk": {"schema": {"type": "object", "properties": {
    "text": {"type": "string"},
    "emb": {"type": "array", "format": "vector", "embedder": "minishlab/potion-base-8M",
            "from": "text"}}}}}})
graph.query('g.v().has_label("chunk").nearest("emb", question, 5).values("text").to_list()',
            params={"question": "How often should I feed my cat?"})
```

**Vectors of a model outside the database** (an API, a model this version does not run): compute
them in Python and write them as values (`property("emb", v)` with a `graphersal.Vector`, a list
or a float buffer); search with a vector instead of a text
(`nearest("emb", v, 5)`).

**Migrating from 0.4.** 0.5.0 removed every external embedder: `graphersal.Embedder`,
`graphersal.Model2Vec`, the `embedders=` and `default_embedder=` keywords of every `Graph`
constructor, loader and `Store` opener (passing one raises Python's own `TypeError`) and
`graph.register_embedder`. Import the model into the store instead (above); the job
`embed_property` was replaced by vector rules.

**Threads of Model2Vec.** The tokenizer (`tokenizers`, through `rayon`) starts a thread pool of
one thread per CPU core on the first batch it encodes. `TOKENIZERS_PARALLELISM=false` in the
environment (set before the first embedding) keeps it on the calling thread, for example in a
program that forks worker processes after embedding or that limits its CPU use.

## Error handling

Every script/engine failure — parse errors, a failed cast, a broken GraphML file — raises
`graphersal.GraphersalError` or one of its subclasses. When the underlying error carries
actionable guidance, it is available as `.help` (`None` otherwise):

```python
import graphersal

graph = graphersal.Graph.tinkerpop_modern()

try:
    graph.execute('UUID.fromString("not-a-uuid")')
except graphersal.GraphersalError as err:
    print(err)        # Error: Cast exception: expected type [uuid], got type 'string' ...
    print(err.help)    # A UUID is written in canonical form: 8-4-4-4-12 lowercase hex digits, ...
```

The failures a program handles on purpose have their own subclasses (all of them are
`GraphersalError`s and keep `.help`):

| Exception | Raised when |
|---|---|
| `PermissionDeniedError` | the script makes a request (an action on a resource) its `policy` does not allow, or uses a file outside the file root; the denied traversal did not run |
| `ReturnedClosureError` | a script run as one unit (`atomic=True`, `dry_run`) returns a closure; everything it changed was rolled back |
| `CommitRejectedError` | a commit hook vetoed the commit (hooks can only be registered from Rust); everything was rolled back |

```python
try:
    graph.execute('g.addV("x").next()', policy=graphersal.AccessPolicy.read_only())
except graphersal.PermissionDeniedError as err:
    print(err)        # Error: Permission denied for 'add_v' (Create Data(vertex "x")): ...
```

## Units of atomicity

A failing script never leaves a half-applied traversal behind. By default every traversal (every
terminal such as `.next()` or `.to_list()`) is its own unit: a failing traversal rolls back only
itself, and the statements before it stay — the TinkerPop behaviour:

```python
graph = graphersal.Graph.tinkerpop_modern()
script = 'g.addV("x").next(); g.V().values("name").sum().next()'   # sum() over strings fails
try:
    graph.execute(script)
except graphersal.GraphersalError:
    pass
graph.query('g.V().count().next()')   # 7: the first statement stayed, the second one left nothing
```

`atomic=True` makes the whole script one unit: when it fails, everything it changed is rolled
back; when it succeeds, all of it stays. While it runs, no other reader or writer of the graph
does. Inside it, each traversal is a savepoint, so a script that catches a failure itself
(`try { ... } catch (err) { }`) goes on with only that traversal undone.

```python
try:
    graph.execute(script, atomic=True)
except graphersal.GraphersalError:
    pass
graph.query('g.V().count().next()')   # still 7: nothing of the failing script stayed
```

A script run with `atomic=True` may not return a closure (`ReturnedClosureError`): return data.
A traversal it returns without a terminal runs after the commit, as its own unit.
Auto-generated ids are never reused, also not after a rollback.

## Dry run: preview the changes of a script

`graph.dry_run(script, params=None, *, policy=None)` runs the script as one unit and then
always rolls it back. It returns a `graphersal.DryRunResult`, an immutable object with two
attributes: `.result`, what `execute` would have returned, and `.changes`, the would-be changes as
plain Python data. The graph is left exactly as it was.

```python
graph = graphersal.Graph.tinkerpop_modern()
run = graph.dry_run(
    'g.V().has("name", "marko").property("age", 30).next();'
    'g.V().has("name", "lop").drop().to_list();'
    'g.V().count().next()'
)
run                         # DryRunResult(result=5, changes=<5 mutations>)
run.result                  # 5
changes = run.changes
changes["commit_seq"]       # 0: never committed
changes["committed_at"]     # 0: never committed (a commit's time: microseconds since the epoch)
changes["node_sequence"]    # 6: the vertex auto-id sequence after the run (ids are never reused)
[m["op"] for m in changes["mutations"]]
# ['set_property', 'drop_edge', 'drop_edge', 'drop_edge', 'drop_vertex']
changes["mutations"][0]
# {'op': 'set_property', 'element': {'kind': 'vertex', 'id': '1'}, 'key': 'age', 'before': 29, 'after': 30}
graph.query('g.V().count().next()')   # 6: untouched
```

The change set is the serialized form of the engine's `ChangeSet` (book page "Transactions"),
JSON-compatible as it is (`json.dumps(changes)` works):

- `commit_seq` and `committed_at` are `0` (never committed); `node_sequence` and `edge_sequence`
  are the auto-id sequences after the run: auto ids a dry run handed out are not handed out again.
- `mutations` lists the changes in execution order, each a dict with an `"op"` key:
  `add_vertex`, `drop_vertex`, `add_edge`, `drop_edge`, `set_property`, `remove_property`,
  `add_label`, `remove_label`, `set_schema`, with the values before and after. Dropping a vertex
  lists the drops of its incident edges first.
- Values JSON cannot hold carry a type tag instead of being guessed from their content: a
  `uuid.UUID` is `{"@type": "g:UUID", "@value": "550e8400-e29b-41d4-a716-446655440000"}`. A
  user map that itself has an `"@type"` key is wrapped as
  `{"@type": "graphersal:Object", "@value": {...}}`.

A failing script raises like `execute` and returns no change set; a returned closure raises
`ReturnedClosureError`. No commit hook runs. A traversal the script returns without a terminal
runs inside the dry run, so its result reflects the previewed changes. The change set is data only:
Python cannot register commit hooks (callbacks on every commit) yet.

## Access policy

`policy=` limits what a script may do. Every step and file operation of a script makes a request,
an **action** on a **resource**; a `graphersal.AccessPolicy` allows or denies them by kind, plus
an optional file root:

| Resource | Requested by |
|---|---|
| `"data"` | `read`: every step that reads the graph (`V()`, `out()`, `values()`, `count()`, ...); `create`: `addV`, `addE`, `mergeV`, `mergeE`, importing GraphML; `update`: `property`, `removeProperty`, `mergeV`/`mergeE`; `delete`: `drop` |
| `"schema"` | `read`: `get_schema`, `infer_schema`, `diff_schema`, `validate_schema(_patch)` (methods and DSL); `update`: `set_schema`, `patch_schema` |
| `"file"` | `read`: `import_graphml`, `GraphSource::file`; `create`: `export_graphml`; `execute`: Rhai `import` |
| `"option"` | `update`: administrative `g.with()` options (`evaluationTimeout`, `repeat.max_loops`, optimizer switches, ...) |
| `"custom"` | never requested by the engine (for servers embedding the Rust library) |

```python
from graphersal import Graph, AccessPolicy

graph = Graph.tinkerpop_modern()

graph.query('g.V().count().next()', policy=AccessPolicy.read_only())          # 6
graph.execute('g.addV("x").next()', policy=AccessPolicy.read_only())          # PermissionDeniedError

AccessPolicy.deny_all()                                   # nothing: the script can only compute
AccessPolicy.allow_all()                                  # everything
AccessPolicy(("read", "data"), ("create", "data"))        # exactly these (action, resource) pairs
AccessPolicy.read_only().allow("*", "data")               # a new value; policies are immutable
AccessPolicy.allow_all().deny("*", "file").deny("update", "option")

# File access limited to one directory: relative paths resolve under it, paths that leave it
# ("..", absolute paths elsewhere, symbolic links pointing out) are denied.
uploads = AccessPolicy.read_only().allow("*", "file").with_file_root("/srv/graphs")
graph.execute('g.export_graphml("snapshot.graphml")', policy=uploads)   # /srv/graphs/snapshot.graphml
```

A denied script fails with `PermissionDeniedError` before the denied traversal runs; the message
names the step and the request (`Permission denied for 'add_v' (Create Data(vertex "x"))`). A file
root does not allow file access by itself.

**The default allows everything**: without `policy=`, a script may do everything, like the Rust
library's own default. A Python program is a trusted local host — it already has the graph
object, file access and `Graph.export_graphml`. Whenever a script comes from somewhere else (a
user, a request, an LLM), pass an explicit policy, e.g. `AccessPolicy.read_only()`, together with
resource limits (`graph.set_limits(...)`).

## Resource limits

A new `Graph` runs scripts under the library's SAFE limits: 10 million script operations, 16 MiB
strings, 100 000 list items and map entries, and 128 levels of nested lists and maps in one value.
`graph.set_limits(...)` changes them for every later script of that graph; each keyword overrides
one limit, `0` meaning unlimited, and `unlimited=True` starts from no limits:

```python
graph.set_limits(max_operations=1_000_000, max_string_size=1 << 20)
graph.set_limits(unlimited=True, max_value_depth=256)
graph.set_limits(max_materialized_bytes=256 << 20)   # the per-query memory limit
graph.limits()   # {"max_operations": 10000000, ..., "max_materialized_bytes": 268435456}
```

`max_value_depth` is never unlimited (`0` means its default, 128): deeper values are read
recursively and would overflow the native stack. Scripts run on a worker thread with a 64 MiB
stack (one per calling Python thread), so the scripting engine's own recursion over nested values
has room. A limit hit raises `GraphersalError` whose `help` names the limit to raise.
`max_materialized_bytes` is the default of the per-query memory limit
`traversal.max_materialized_bytes` (the memory one query holds at once, live bytes: memory a
step frees is given back; without it the library default, 1 GiB) for every traversal of the
graph's scripts. Since 0.5.0 the limit counts what a query holds at the same time, no longer the
size of one value: a profile reports the query's peak (`query_memory`).
Traversal limits (`evaluationTimeout`, `traversal.max_materialized_bytes`,
`traversal.max_string_bytes`, ...) are also set per query with `g.with(key, value)`, which
overrides the default; see the book's "Resource Limits" page.

The process has a memory ceiling, ON by default: every traversal, import or write that would
take the memory in use over it raises `GraphersalError` and is rolled back, instead of the process
being killed for lack of memory. It is ONE setting for the whole Python process (every graph),
by default `"auto"`: the cgroup memory limit of the process (a Linux container) minus a headroom
(default a tenth of the limit, at least 64 MiB), else 80 % of the physical memory. Set a number of
bytes, `"auto"`, or `"none"` to turn it off; `graphersal.memory_limit()` reports the value in
effect and where it comes from:

```python
graphersal.set_memory_limit(2 << 30)
graphersal.set_memory_limit("auto", headroom=512 << 20)
graphersal.memory_limit()   # {'bytes': ..., 'source': 'configured', 'setting': ..., 'headroom': ...}
```

`Graph.load` and every `Store` open also refuse a graph whose size estimate exceeds the ceiling,
before reading it (nothing is opened or changed; the error names the minimum to raise the ceiling
to), and stop a load or recovery that grows past it while reading. GraphSON/GraphML loads into a
new graph and checkpoints are checked too.

The extension installs Graphersal's tracking allocator as its own global allocator, so the ceiling
compares the exact live heap of the extension's Rust code (every graph and query of the process,
read with one atomic load, no walk of the graph). That count does not include Python objects: the
ceiling must leave room for the Python side (the interpreter, your own data, the converted
results). The same allocator makes Memory profiles exact:
`g.v().out().profile_with(ProfileType.Memory)` reports `"source": "tracked"` bytes per step
(a bare `profile()` collects no memory data). The cost is a few counter increments per
allocation of the extension.

`graph.memory_report()` shows where a graph's own memory goes (per label and property, plus the
slots: `report["structure"]["vertex_slots"]["spare"]`). A graph built by write queries keeps the
spare capacity its growing vectors reserved past the data (up to about half of the slots); a
loaded graph has none. After a bulk load by queries, release it once with
`graph.shrink_to_fit()`; it changes no data and commits nothing (the vacant slots of removed
elements stay):

```python
graph.execute('g.gql("UNWIND range(0, 199999) AS i CREATE (:P {id: i})").iterate()')
graph.shrink_to_fit()   # {'measured': True, 'before_bytes': ..., 'after_bytes': ..., 'released_bytes': ...}
```

## GraphRAG example

[`examples/graphrag.py`](https://github.com/graphersal/graphersal/blob/main/bindings/py-graphersal/examples/graphrag.py) ingests a small knowledge graph of text entities and
relations, extracts the 2-hop neighbourhood of a question entity with
`repeat(__.out()).times(2)` and prints one JSON line per path (`entity`, `type`, `text`, `path`,
`hops`), ready to paste into an LLM prompt:

```bash
python examples/graphrag.py "Marie Curie"
```

## Concurrency

`execute()`/`query()`/`dry_run()` release the GIL for the duration of the script, so a
long-running query on one `Graph` does not stall other Python threads (e.g. a thread-pool server,
or `asyncio` driving this through `run_in_executor`). The underlying graph still serializes
concurrent mutations from multiple threads; an `atomic=True` script and a dry run hold the graph's
write lock for the whole script.

## What's out of scope (for now)

- Commit hooks from Python (a Python callback on every commit); change sets are available only
  as data through `dry_run`.
- Positional parameters (`$1`, `$2`, ...).
- `list`/`dict` values in `params`.

- Graph file formats other than GraphSON and GraphML (and Graphersal's own snapshots).

## Building from source

For development in [the repository](https://github.com/graphersal/graphersal).

You need a Rust toolchain and Python 3.10+ (the binding targets `abi3-py310`). The macOS system
`python3` is 3.9 and too old; use e.g. Homebrew's `python3.12`. Keep the virtualenv outside the
repository or at `bindings/py-graphersal/.venv` (ignored by version control). One-time setup, from
the repository root:

```bash
/opt/homebrew/bin/python3.12 -m venv bindings/py-graphersal/.venv
bindings/py-graphersal/.venv/bin/pip install maturin pytest
```

Build the extension into that virtualenv as `graphersal` (repeat after every Rust change in the
binding or the engine), then run the tests:

```bash
cd bindings/py-graphersal
VIRTUAL_ENV=$PWD/.venv .venv/bin/maturin develop      # add --release for an optimized build
.venv/bin/python -m pytest tests/                     # Python suite, including examples/
cd ../..
PYO3_PYTHON=$PWD/bindings/py-graphersal/.venv/bin/python cargo test -p py-graphersal   # Rust unit tests
```

`PYO3_PYTHON` must name a Python >= 3.10 for every Cargo command that builds this crate
(`cargo test -p py-graphersal`, workspace-wide `cargo check`/`cargo clippy`); otherwise exclude it
with `--exclude py-graphersal`. `import graphersal` is then available in the virtualenv
(`source bindings/py-graphersal/.venv/bin/activate`).

`tests/test_temporal_pandas.py` covers both sides of the optional `pandas` extra: run the suite
once in a virtualenv without pandas and once in one with it (`pip install pandas`); each test
that needs the other environment is skipped.

### A wheel to install elsewhere

```bash
cd bindings/py-graphersal
maturin build --release        # writes a wheel to target/wheels/
pip install ../../target/wheels/graphersal-*.whl
```

The wheel targets the stable ABI (`abi3-py310`), so one wheel per platform covers Python 3.10 and
newer — no per-Python-version rebuild is needed.

## License

Apache License, Version 2.0. Graphersal is compatible with, and inspired by, Apache TinkerPop
Gremlin. It is an independent project: it is not part of Apache TinkerPop and is not endorsed by
the Apache Software Foundation.
