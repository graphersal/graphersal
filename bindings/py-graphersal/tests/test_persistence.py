"""Packed snapshots (`Graph.save` / `Graph.load`), the journal (`start_journal`, `mark`) and
`Graph.recover`."""

import uuid

import pytest

import graphersal

U1 = uuid.UUID("550e8400-e29b-41d4-a716-446655440000")


def names(graph):
    return sorted(graph.query('g.V().values("name").toList()'))


def test_save_and_load_keep_every_value_type_and_the_schema(tmp_path):
    graph = graphersal.Graph.tinkerpop_modern()
    graph.set_schema({"mode": "open"})
    graph.execute(
        'g.addV("doc").property("id_", u).property("tags", ["a", 1, 2.5]).'
        'property("meta", #{"k": [true, ()]}).next()',
        {"u": U1},
    )
    path = str(tmp_path / "g.gsnap")
    info = graph.save(path, name="test")
    assert info["vertex_count"] == 7
    loaded = graphersal.Graph.load(path)
    assert loaded.query('g.V().hasLabel("doc").values("id_").next()') == U1
    assert loaded.query('g.V().hasLabel("doc").values("tags").next()') == ["a", 1, 2.5]
    assert loaded.query('g.V().hasLabel("doc").values("meta").next()') == {"k": [True, None]}
    assert loaded.get_schema()["mode"] == "open"
    assert loaded.query("g.E().count().next()") == 6


def test_load_refuses_a_damaged_file_and_a_too_small_memory_limit(tmp_path):
    graph = graphersal.Graph.tinkerpop_modern()
    path = tmp_path / "g.gsnap"
    graph.save(str(path))
    # A snapshot over the process memory ceiling is refused before it is read.
    try:
        graphersal.set_memory_limit(1)
        with pytest.raises(graphersal.GraphersalError):
            graphersal.Graph.load(str(path))
    finally:
        graphersal.set_memory_limit("auto")
    data = bytearray(path.read_bytes())
    data[len(data) // 2] ^= 0x01
    path.write_bytes(bytes(data))
    with pytest.raises(graphersal.GraphersalError) as err:
        graphersal.Graph.load(str(path))
    assert "Corrupt" in str(err.value)
    assert err.value.help


def test_store_open_refuses_a_graph_over_the_ceiling(tmp_path):
    path = str(tmp_path / "s")
    store = graphersal.Store.create(path)
    store.graph.execute(
        'for i in 0..500 { g.addV("person").property("name", `p${i}`).iterate(); }'
    )
    store.checkpoint()
    store.close()
    # A graph that does not fit refuses the open (nothing opened, never maintenance mode).
    try:
        graphersal.set_memory_limit(1)
        with pytest.raises(graphersal.GraphersalError) as err:
            graphersal.Store.open(path)
        assert "store open" in str(err.value)
    finally:
        graphersal.set_memory_limit("auto")
    with graphersal.Store.open(path) as store:
        assert store.graph.query("g.V().count().next()") == 500


def test_journal_marks_and_recovery_to_each_target(tmp_path):
    snapshot = str(tmp_path / "base.gsnap")
    journal = str(tmp_path / "g.wal")
    graph = graphersal.Graph()
    graph.save(snapshot)
    graph.start_journal(journal, durability="os")
    graph.execute('g.addV("person").property("name", "ann").next()')
    mark = graph.mark("after-ann")
    assert mark["commit_seq"] == 1
    graph.execute('g.addV("person").property("name", "bob").next()')
    with pytest.raises(graphersal.GraphersalError):
        graph.mark("after-ann")

    latest, report = graphersal.Graph.recover(snapshot, [journal])
    assert names(latest) == ["ann", "bob"]
    assert report["commit_seq"] == 2
    assert report["marks"] == ["after-ann"]
    assert report["reached_end"]

    at_mark, report = graphersal.Graph.recover(snapshot, [journal], mark="after-ann")
    assert names(at_mark) == ["ann"]
    assert not report["reached_end"]
    at_one, _ = graphersal.Graph.recover(None, [journal], commit_seq=1)
    assert names(at_one) == ["ann"]
    with pytest.raises(ValueError):
        graphersal.Graph.recover(snapshot, [journal], commit_seq=1, mark="after-ann")
    with pytest.raises(graphersal.GraphersalError):
        graphersal.Graph.recover(snapshot, [journal], mark="nope")


def test_mark_needs_a_journal():
    with pytest.raises(graphersal.GraphersalError) as err:
        graphersal.Graph().mark("m")
    assert "journal" in str(err.value)


def test_durability_is_validated(tmp_path):
    graph = graphersal.Graph()
    with pytest.raises(ValueError):
        graph.start_journal(str(tmp_path / "a.wal"), durability="sometimes")
    graph.start_journal(str(tmp_path / "b.wal"), durability=0.5)
    graph.execute('g.addV("x").next()')
