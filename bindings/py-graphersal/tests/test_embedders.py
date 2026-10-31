"""Embedders from Python (book page "Vectors", section "Embeddings"): `graphersal.Embedder` over a
Python callable (rows as lists, 1-D buffers or one 2-D numpy array; wrong answers; exceptions),
the per-graph registry (`Graph(embedders=..)`, `register_embedder`, `embedders()`, a Store's
graph), vectors written by a query and declared in the schema, text queries, the removed job
`embed_property`, the GIL rule (no
deadlock between a sleeping embedder and a writer thread), and `graphersal.Model2Vec` on a tiny
synthetic model written by the test (its identity equal to the command line's for the same files
with `model=`)."""

import json
import struct
import subprocess
import sys
import threading
import time

import pytest

import graphersal
from graphersal import Embedder, Graph, Model2Vec, Vector

try:
    import numpy
except ImportError:  # numpy is optional: the binding reads buffers through memoryview
    numpy = None

needs_numpy = pytest.mark.skipif(numpy is None, reason="numpy is not installed")


def letters(texts):
    """A 3-dimensional toy embedding: counts of a, e and o (plus 1, never the zero vector)."""
    return [[t.count("a") + 1.0, t.count("e") + 1.0, t.count("o") + 1.0] for t in texts]


def toy(name="toy", **kwargs):
    return Embedder(name, letters, dimension=3, **kwargs)


def docs_graph(**kwargs):
    g = Graph(**kwargs)
    g.execute(
        """
        for t in ["a graph database", "vector search", "hello world", "banana"] {
            g.add_v("doc").property("text", t).iterate();
        }
        """
    )
    return g


def attach_schema(g):
    g.execute("g.set_schema(g.infer_schema(), SchemaMode.open)")


# ---------------------------------------------------------------------------------------------
# The callable embedder
# ---------------------------------------------------------------------------------------------


def test_an_embedder_describes_itself():
    e = Embedder("acme/letters", letters, dimension=3, metric="dot", max_batch=8, deterministic=True)
    assert (e.name, e.model, e.dimension, e.element, e.metric) == ("acme/letters", "acme/letters", 3, "f32", "dot")
    assert (e.max_batch, e.deterministic, e.local) == (8, True, True)
    named = toy(model="acme/letters-v2", local=False)
    assert named.model == "acme/letters-v2" and named.local is False
    assert "acme/letters" in repr(e)
    v = e.embed(["banana", "tree"])
    assert [x.tolist() for x in v] == [[4.0, 1.0, 1.0], [1.0, 3.0, 1.0]]


def test_invalid_embedders_are_refused_at_construction():
    with pytest.raises(ValueError, match="reserved"):
        Embedder("hash-ngram/2", letters, dimension=3)
    with pytest.raises(ValueError, match="dimension"):
        Embedder("x", letters, dimension=0)
    with pytest.raises(ValueError, match="max_batch"):
        Embedder("x", letters, dimension=3, max_batch=0)
    with pytest.raises(ValueError, match="element"):
        Embedder("x", letters, dimension=3, element="f64")
    with pytest.raises(ValueError, match="metric"):
        Embedder("x", letters, dimension=3, metric="manhattan")
    with pytest.raises(TypeError, match="callable"):
        Embedder("x", "not callable", dimension=3)


def test_rows_may_be_lists_tuples_buffers_or_vectors():
    import array

    def mixed(texts):
        return [[1.0, 2.0], (3, 4), array.array("f", [5.0, 6.0]), Vector([7.0, 8.0])][: len(texts)]

    e = Embedder("mixed", mixed, dimension=2)
    assert [v.tolist() for v in e.embed(["a", "b", "c", "d"])] == [[1.0, 2.0], [3.0, 4.0], [5.0, 6.0], [7.0, 8.0]]
    # The element quantizes the floats.
    q = Embedder("q", lambda texts: [[1.0, -1.0]] * len(texts), dimension=2, element="bit")
    assert q.embed(["x"])[0].element == "bit"


@needs_numpy
@pytest.mark.parametrize("dtype", ["float32", "float64"])
def test_one_2d_numpy_array_is_the_whole_answer(dtype):
    def matrix(texts):
        return numpy.asarray(letters(texts), dtype=dtype)

    e = Embedder("np", matrix, dimension=3)
    assert [v.tolist() for v in e.embed(["banana", "tree"])] == [[4.0, 1.0, 1.0], [1.0, 3.0, 1.0]]
    # A list of 1-D numpy rows works too, and a non-contiguous (transposed) array is read in order.
    rows = Embedder("np-rows", lambda texts: [numpy.asarray(r, dtype=dtype) for r in letters(texts)], dimension=3)
    assert rows.embed(["banana"])[0].tolist() == [4.0, 1.0, 1.0]
    transposed = Embedder(
        "np-t", lambda texts: numpy.ascontiguousarray(numpy.asarray(letters(texts), dtype=dtype).T).T, dimension=3
    )
    assert transposed.embed(["banana", "tree"])[1].tolist() == [1.0, 3.0, 1.0]


@needs_numpy
def test_wrong_numpy_shapes_and_types_are_bad_output():
    for answer, match in [
        (lambda texts: numpy.zeros((len(texts) + 1, 3), dtype="float32"), "shape"),
        (lambda texts: numpy.zeros((len(texts), 2), dtype="float32"), "shape"),
        (lambda texts: numpy.ones(3, dtype="float32"), "1-D"),
        (lambda texts: numpy.ones((len(texts), 3), dtype="int32"), "float32 or float64"),
    ]:
        with pytest.raises(graphersal.GraphersalError, match="wrong answer") as info:
            Embedder("bad", answer, dimension=3).embed(["a"])
        assert match in str(info.value)


def test_wrong_answers_are_bad_output():
    cases = [
        (lambda texts: letters(texts)[:-1], "rows for"),
        (lambda texts: [[1.0, 2.0]] * len(texts), "dimension is 3"),
        (lambda texts: "nope", "return a list of rows"),
        (lambda texts: [["x", 1.0, 2.0]] * len(texts), "row 0"),
        (lambda texts: [[float("nan"), 1.0, 2.0]] * len(texts), "row 0"),
        (lambda texts: None, "NoneType"),
    ]
    g = docs_graph()
    for answer, match in cases:
        e = Embedder("bad", answer, dimension=3)
        with pytest.raises(graphersal.GraphersalError, match="wrong answer") as info:
            e.embed(["a", "b"])
        assert match in str(info.value)
        # Inside a query too, with the engine's help.
        g2 = docs_graph(embedders=[e])
        with pytest.raises(graphersal.GraphersalError, match="wrong answer") as info:
            g2.query('g.v().embed("text", #{embedder: "bad"}).to_list()')
        assert info.value.help and "contract" in info.value.help
    assert g.query("g.v().count().next()") == 4


@pytest.mark.parametrize("exception", [RuntimeError("model offline"), KeyboardInterrupt("stop"), ValueError()])
def test_exceptions_become_embedder_failures(exception):
    def failing(texts):
        raise exception

    g = docs_graph(embedders=[Embedder("failing", failing, dimension=3)], default_embedder="failing")
    with pytest.raises(graphersal.GraphersalError, match="the embedder failed") as info:
        g.query('g.v().embed("text").to_list()')
    message = str(info.value)
    assert type(exception).__name__ in message
    if str(exception):
        assert str(exception) in message
    # The atomic script rolled back; the graph is usable.
    with pytest.raises(graphersal.GraphersalError, match="failed"):
        g.execute('g.add_v("x").iterate(); g.v().has_label("doc").embed("text").iterate()', atomic=True)
    assert g.query("g.v().count().next()") == 4


# ---------------------------------------------------------------------------------------------
# The registry
# ---------------------------------------------------------------------------------------------


def test_the_registry_reaches_scripts_atomic_runs_and_dry_runs():
    g = docs_graph(embedders=[toy()], default_embedder="toy")
    assert g.query('embed("banana").dimension') == 3
    assert g.query('g.v().embed("text").next().dimension') == 3
    assert g.execute('g.v().embed("text").next().dimension', atomic=True) == 3
    assert g.dry_run('g.v().embed("text").next().dimension').result == 3
    # The built-in is still there and can be named.
    assert g.query('embed("banana", #{embedder: "hash-ngram/1"}).dimension') == 256


def test_embedders_lists_the_registry_default_first():
    g = Graph(embedders=[toy("a"), toy("b", model="acme/b@1")], default_embedder="b")
    listed = g.embedders()
    assert [e["name"] for e in listed] == ["b", "hash-ngram/1", "a"]
    assert listed[0] == {
        "name": "b",
        "model": "acme/b@1",
        "dimension": 3,
        "element": "f32",
        "metric": "cosine",
        "max_batch": 64,
        "deterministic": False,
        "local": True,
        "default": True,
    }
    assert listed[1]["model"] == "hash-ngram/1" and listed[1]["default"] is False
    assert [e["name"] for e in Graph().embedders()] == ["hash-ngram/1"]


def test_registration_rules():
    with pytest.raises(ValueError, match="already registered"):
        Graph(embedders=[toy(), toy()])
    with pytest.raises(ValueError, match="not registered"):
        Graph(default_embedder="missing")
    g = Graph()
    g.register_embedder(toy())
    assert [e["name"] for e in g.embedders()] == ["hash-ngram/1", "toy"]
    with pytest.raises(ValueError, match="already registered"):
        g.register_embedder(toy())
    g.register_embedder(toy("second"), default=True)
    assert g.embedders()[0]["name"] == "second"
    # The earlier registrations and their order stay.
    assert [e["name"] for e in g.embedders()] == ["second", "hash-ngram/1", "toy"]


def test_every_constructor_and_loader_takes_embedders(tmp_path):
    def check(graph):
        assert graph.query('embed("x").dimension') == 3

    kw = {"embedders": [toy()], "default_embedder": "toy"}
    check(Graph(**kw))
    check(Graph.tinkerpop_modern(**kw))
    src = Graph.tinkerpop_modern()
    src.export_graphml(str(tmp_path / "g.xml"))
    src.export_graphson(str(tmp_path / "g.json"))
    src.save(str(tmp_path / "g.gsnap"))
    check(Graph.from_graphml(str(tmp_path / "g.xml"), **kw))
    check(Graph.from_graphson(str(tmp_path / "g.json"), **kw))
    check(Graph.load(str(tmp_path / "g.gsnap"), **kw))
    recovered, _ = Graph.recover(str(tmp_path / "g.gsnap"), [], **kw)
    check(recovered)


def test_a_store_graph_keeps_its_registry(tmp_path):
    store = graphersal.Store.create(str(tmp_path / "s"), embedders=[toy()], default_embedder="toy")
    assert store.graph.query('embed("x").dimension') == 3
    # A registration through one `store.graph` reaches the next one.
    store.graph.register_embedder(toy("later"))
    assert "later" in [e["name"] for e in store.graph.embedders()]
    store.close()
    with graphersal.Store.open(str(tmp_path / "s"), embedders=[toy()]) as reopened:
        assert "toy" in [e["name"] for e in reopened.graph.embedders()]


# ---------------------------------------------------------------------------------------------
# Declared vectors and text queries
# ---------------------------------------------------------------------------------------------


def embed_and_declare(g, model):
    """Writes `emb` of every doc with the default embedder and declares its `model`."""
    g.execute('g.v().has_label("doc").property("emb", __.embed("text")).iterate()')
    attach_schema(g)
    g.patch_schema({"vertices": {"doc": {"schema": {"properties": {"emb": {"embedder": model}}}}}})


def test_declared_vectors_answer_text_queries():
    g = docs_graph(embedders=[toy(model="acme/letters@1")], default_embedder="toy")
    embed_and_declare(g, "acme/letters@1")
    emb = g.get_schema()["vertices"]["doc"]["schema"]["properties"]["emb"]
    assert emb["embedder"] == "acme/letters@1"
    # A text query embeds with the declared model.
    top = g.query('g.v().nearest("emb", "banana", 1).values("text").to_list()')
    assert top == ["banana"]
    scores = g.query('g.v().has("text", "banana").similarity("emb", "banana").to_list()')
    assert scores == [pytest.approx(1.0)]


def test_embed_property_was_removed():
    assert not hasattr(Graph, "embed_property")
    assert not hasattr(graphersal, "EmbedPropertyError")
    g = docs_graph()
    attach_schema(g)
    with pytest.raises(graphersal.GraphersalError, match="embed_property was removed") as info:
        g.execute('g.embed_property(#{label: "doc", from: "text", to: "emb"})')
    assert "Vector Rules" in str(info.value) + str(info.value.help)


# ---------------------------------------------------------------------------------------------
# The GIL rule
# ---------------------------------------------------------------------------------------------


def run_with_timeout(target, seconds=30):
    errors = []

    def wrapped():
        try:
            target()
        except BaseException as err:  # noqa: BLE001 - reported to the test thread
            errors.append(err)

    thread = threading.Thread(target=wrapped, daemon=True)
    thread.start()
    thread.join(seconds)
    assert not thread.is_alive(), "deadlock: the thread did not finish"
    if errors:
        raise errors[0]


#: The deadlock scenario, run in a child process: a deadlock there holds the child's GIL, so only
#: a parent process can see it (as a timeout) instead of hanging itself.
DEADLOCK_SCENARIO = r"""
import sys, threading, time
from graphersal import Embedder, Graph

out = sys.argv[1]
started = threading.Event()

def letters(texts):
    return [[t.count("a") + 1.0, t.count("e") + 1.0, t.count("o") + 1.0] for t in texts]

def slow(texts):
    started.set()
    time.sleep(0.2)  # releases the GIL; the traversal holds the graph's lock meanwhile
    return letters(texts)

g = Graph(embedders=[Embedder("slow", slow, dimension=3, max_batch=1)], default_embedder="slow")
g.execute('for t in ["a graph", "vector", "hello", "banana"] { g.add_v("doc").property("text", t).iterate(); }')
g.execute("g.set_schema(g.infer_schema(), SchemaMode.open)")
results = {}

def writer():
    started.wait(10)
    for i in range(5):
        g.execute('g.add_v("note").property("n", i).iterate()', params={"i": i})

def reader():
    started.wait(10)
    for i in range(5):
        g.export_graphson(f"{out}/g{i}.json")
        g.export_graphml(f"{out}/g{i}.xml")
        g.save(f"{out}/g{i}.gsnap")
        assert g.get_schema() is not None and g.infer_schema()
        assert g.embedders()[0]["name"] == "slow"

def query():
    results["query"] = len(g.query('g.v().has_label("doc").embed("text").to_list()'))

started.clear()
threads = [threading.Thread(target=t) for t in (query, writer, reader)]
for t in threads:
    t.start()
for t in threads:
    t.join()
assert results == {"query": 4}, results
assert g.query('g.v().has_label("note").count().next()') == 5
print("ok")
"""


def test_a_sleeping_embedder_and_a_writer_thread_do_not_deadlock(tmp_path):
    """A traversal holds the graph's lock while its Python embedder runs (and needs the GIL); a
    writer thread queues for the write lock, and a third thread reads (exports, saves, schema
    methods): a read queued behind the writer must not hold the GIL, or the embedder could never
    finish. (Before the exports released the GIL this scenario hung.)"""
    try:
        done = subprocess.run(
            [sys.executable, "-c", DEADLOCK_SCENARIO, str(tmp_path)],
            capture_output=True,
            text=True,
            timeout=120,
        )
    except subprocess.TimeoutExpired:
        pytest.fail("deadlock: the scenario did not finish within 120 s")
    assert done.returncode == 0 and done.stdout.strip() == "ok", done.stderr


def test_register_embedder_during_a_running_execution():
    started = threading.Event()
    release = threading.Event()

    def blocking(texts):
        started.set()
        release.wait(10)
        return letters(texts)

    g = docs_graph(embedders=[Embedder("blocking", blocking, dimension=3)], default_embedder="blocking")
    out = {}

    def query():
        out["names"] = g.query("g.embedders().map(|e| e.name)")
        out["dims"] = g.query('g.v().embed("text").to_list().map(|v| v.dimension)')

    thread = threading.Thread(target=query)
    thread.start()
    assert started.wait(10)
    # The running execution keeps its registry; the registration does not wait for it.
    g.register_embedder(toy("late"))
    assert "late" in [e["name"] for e in g.embedders()]
    release.set()
    run_with_timeout(thread.join)
    assert "late" not in out["names"]
    assert out["dims"] == [3, 3, 3, 3]


# ---------------------------------------------------------------------------------------------
# Model2Vec on a tiny synthetic model
# ---------------------------------------------------------------------------------------------

VOCAB = ["[UNK]", "graph", "traversal", "vector", "marko", "josh", "lop", "ripple"]
ROWS = [
    [9.0, 9.0, 9.0, 9.0],
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.5],
    [0.5, 0.5, 0.0, 0.0],
    [0.0, 0.5, 0.5, 0.0],
    [0.25, 0.0, 0.0, 1.0],
    [0.0, 0.0, 0.75, 0.75],
]


def write_model(directory):
    """The tiny model of `crates/graphersal/tests/all/model2vec_tests.rs`: a word-level tokenizer,
    an 8 x 4 f32 embedding matrix, a config that normalizes."""
    directory.mkdir(parents=True, exist_ok=True)
    (directory / "config.json").write_text('{"model_type": "model2vec", "normalize": true}')
    tokenizer = {
        "version": "1.0",
        "truncation": None,
        "padding": None,
        "added_tokens": [],
        "normalizer": {"type": "Lowercase"},
        "pre_tokenizer": {"type": "Whitespace"},
        "post_processor": None,
        "decoder": None,
        "model": {"type": "WordLevel", "vocab": {t: i for i, t in enumerate(VOCAB)}, "unk_token": "[UNK]"},
    }
    (directory / "tokenizer.json").write_text(json.dumps(tokenizer))
    data = b"".join(struct.pack("<4f", *row) for row in ROWS)
    header = json.dumps({"embeddings": {"dtype": "F32", "shape": [8, 4], "data_offsets": [0, len(data)]}})
    header += " " * (-len(header) % 8)
    (directory / "model.safetensors").write_bytes(struct.pack("<Q", len(header)) + header.encode() + data)
    return directory


def test_model2vec_loads_and_embeds(tmp_path):
    m = Model2Vec("tiny", str(write_model(tmp_path / "m")))
    assert isinstance(m, Embedder)
    assert (m.name, m.dimension, m.element, m.max_batch) == ("tiny", 4, "f32", 256)
    assert m.model.startswith("model2vec@") and len(m.model) == len("model2vec@") + 12
    v = m.embed(["graph"])[0].tolist()
    assert v == pytest.approx([1.0, 0.0, 0.0, 0.0])
    g = Graph.tinkerpop_modern(embedders=[m], default_embedder="tiny")
    assert g.query('g.v().has("name", "marko").embed("name").next().dimension') == 4
    small = Model2Vec("tiny2", str(tmp_path / "m"), max_batch=2, max_tokens=1, normalize=False)
    assert small.max_batch == 2 and small.model.endswith("?normalize=false")


def test_model2vec_identity_equals_the_cli_download_for_the_same_files(tmp_path):
    bare = write_model(tmp_path / "bare")
    named = Model2Vec("tiny", str(bare), model="acme/tiny-model")
    digest = Model2Vec("tiny", str(bare)).model.removeprefix("model2vec@")
    assert named.model == f"model2vec/acme/tiny-model@{digest}"
    # What `graphersal embed-model download acme/tiny-model` leaves: the files and the manifest.
    downloaded = write_model(tmp_path / "downloaded")
    (downloaded / "graphersal-embed-model.json").write_text(
        json.dumps({"format": 1, "repo": "acme/tiny-model", "revision": "main", "files": []})
    )
    assert Model2Vec("tiny", str(downloaded)).model == named.model
    assert Model2Vec("tiny", str(downloaded), model="acme/tiny-model").model == named.model
    with pytest.raises(graphersal.GraphersalError, match="differs") as info:
        Model2Vec("tiny", str(downloaded), model="acme/other")
    assert info.value.help
    with pytest.raises(graphersal.GraphersalError, match="repository name"):
        Model2Vec("tiny", str(bare), model="not a repo")


def test_model2vec_failures(tmp_path):
    with pytest.raises(graphersal.GraphersalError, match="does not exist"):
        Model2Vec("tiny", str(tmp_path / "missing"))
    with pytest.raises(ValueError, match="reserved"):
        Model2Vec("hash-ngram", str(write_model(tmp_path / "m")))


def test_model2vec_vectors_and_text_query(tmp_path):
    m = Model2Vec("tiny", str(write_model(tmp_path / "m")), model="acme/tiny-model")
    g = Graph.tinkerpop_modern(embedders=[m])
    # vadas and peter are not in the tiny vocabulary: only marko and josh are embedded.
    g.execute(
        'g.v().has("name", within("marko", "josh"))'
        '.property("emb", __.embed("name", #{embedder: "tiny"})).iterate()'
    )
    attach_schema(g)
    g.patch_schema({"vertices": {"person": {"schema": {"properties": {"emb": {"embedder": m.model}}}}}})
    assert g.query('g.v().has("emb").count().next()') == 2
    assert g.get_schema()["vertices"]["person"]["schema"]["properties"]["emb"]["embedder"] == m.model
    # The text query finds the model by the schema's identity, not by the registry name or default.
    assert g.query('g.v().has_label("person").nearest("emb", "marko", 1).values("name").to_list()') == ["marko"]
    # A graph loaded with the same files under another name finds them too.
    other = Model2Vec("renamed", str(tmp_path / "m"), model="acme/tiny-model")
    path = tmp_path / "g.gsnap"
    g.save(str(path))
    loaded = Graph.load(str(path), embedders=[other])
    assert loaded.query('g.v().has_label("person").nearest("emb", "josh", 1).values("name").to_list()') == ["josh"]
