"""The system journal from Python: `Graph.journal` (the in-memory journal of a graph without a
store), `Store.journal` and the closed-store `Store.journal_dir`, with the filters of
`graphersal store journal` and its `--json` record keys."""

import datetime
import json
import os
import shutil
import subprocess

import pytest

import graphersal

ENVELOPE = {"seq", "origin", "origin_seq", "time", "level", "source", "kind", "message", "fields"}
PAGE = {"records", "next_after", "truncated", "more", "notes"}


def denied_writes(graph, count):
    for _ in range(count):
        with pytest.raises(graphersal.PermissionDeniedError):
            graph.execute(
                'g.addV("secret_label").next()', policy=graphersal.AccessPolicy.read_only()
            )


def test_a_graph_without_a_store_has_an_in_memory_journal():
    graph = graphersal.Graph()
    page = graph.journal()
    assert set(page) == PAGE
    assert page["records"] == [] and page["more"] is False and page["truncated"] is False
    denied_writes(graph, 1)
    page = graph.journal(kind="auth.denied")
    [record] = page["records"]
    assert ENVELOPE <= set(record)
    assert record["level"] == "warn" and record["source"] == "auth"
    assert record["fields"]["action"] == "Create"
    assert set(record["origin"]) == {"id", "name"}
    assert isinstance(record["time"], int) and isinstance(record["seq"], int)
    # Records hold no data values.
    assert "secret_label" not in json.dumps(record)


def test_filters_and_the_cursor():
    graph = graphersal.Graph()
    denied_writes(graph, 1)
    first = graph.journal()
    assert first["records"]
    cursor = first["next_after"]
    assert graph.journal(after=cursor)["records"] == []
    assert graph.journal(level="error")["records"] == []
    assert graph.journal(level="warn")["records"] == first["records"]
    assert graph.journal(source="auth")["records"] == first["records"]
    assert graph.journal(source="store")["records"] == []
    assert graph.journal(kind="store.opened")["records"] == []
    time = first["records"][0]["time"]
    assert graph.journal(since=time)["records"] == first["records"]
    assert graph.journal(since=time + 1)["records"] == []
    # `until` is exclusive: the records before it.
    assert graph.journal(until=time + 1)["records"] == first["records"]
    assert graph.journal(until=time)["records"] == []
    utc = datetime.datetime.fromtimestamp(time // 1_000_000, tz=datetime.timezone.utc)
    assert graph.journal(since=utc)["records"] == first["records"]
    assert graph.journal(since=utc.strftime("%Y-%m-%dT%H:%M:%SZ"))["records"] == first["records"]
    origin = first["records"][0]["origin"]["id"]
    assert graph.journal(origin=origin)["records"] == first["records"]
    assert graph.journal(origin="00000000-0000-0000-0000-000000000001")["records"] == []


def test_paging_with_limit_and_next_after():
    graph = graphersal.Graph()
    # Different principals' denials are distinct events; one per kind of write here.
    for script in ('g.addV("a").next()', 'g.V().drop().iterate()', 'g.addE("x").iterate()'):
        try:
            graph.execute(script, policy=graphersal.AccessPolicy.read_only())
        except graphersal.GraphersalError:
            pass
    everything = graph.journal()["records"]
    seen, after = [], 0
    while True:
        page = graph.journal(after=after, limit=1)
        seen += page["records"]
        after = page["next_after"]
        if not page["more"]:
            break
    assert seen == everything


def test_bad_filters_raise():
    graph = graphersal.Graph()
    with pytest.raises(ValueError):
        graph.journal(level="loud")
    with pytest.raises(ValueError):
        graph.journal(origin="not-a-uuid")
    with pytest.raises(ValueError):
        graph.journal(since=datetime.datetime(2026, 10, 31))  # naive
    with pytest.raises(ValueError):
        graph.journal(since="yesterday")
    with pytest.raises(TypeError):
        graph.journal(since=1.5)
    with pytest.raises(ValueError):
        graph.journal(limit=0)
    with pytest.raises(TypeError):
        graph.journal("warn")  # keyword-only


def test_reading_needs_read_on_the_journal():
    graph = graphersal.Graph()
    with pytest.raises(graphersal.PermissionDeniedError) as err:
        graph.journal(policy=graphersal.AccessPolicy.deny_all())
    assert "journal" in str(err.value)
    assert err.value.help and "read" in err.value.help
    assert graph.journal(policy=graphersal.AccessPolicy.read_only())["records"] == []
    allowed = graphersal.AccessPolicy.deny_all().allow("read", "journal")
    assert graph.journal(policy=allowed)["more"] is False


def test_a_store_journal_and_the_closed_store_form(tmp_path):
    path = str(tmp_path / "data")
    with graphersal.Store.create(path) as store:
        store.graph.execute('g.addV("person").next()')
        store.checkpoint("first")
        page = store.journal()
        kinds = [r["kind"] for r in page["records"]]
        assert "store.created" in kinds and "store.opened" in kinds, kinds
        assert store.graph.journal()["records"] == page["records"]
        assert all(r["source"].startswith("store") for r in store.journal(source="store")["records"])
        with pytest.raises(graphersal.PermissionDeniedError):
            store.journal(policy=graphersal.AccessPolicy.deny_all())
    closed = graphersal.Store.journal_dir(path)
    kinds = [r["kind"] for r in closed["records"]]
    assert "store.closed" in kinds, kinds
    assert set(closed["records"][0]) >= ENVELOPE
    assert graphersal.Store.journal_dir(path, kind="store.closed", limit=1)["records"][0][
        "kind"
    ] == "store.closed"
    with pytest.raises(graphersal.PermissionDeniedError):
        graphersal.Store.journal_dir(path, policy=graphersal.AccessPolicy.deny_all())
    with pytest.raises(graphersal.GraphersalError):
        graphersal.Store.journal_dir(str(tmp_path / "missing"))
    # A closed Store object raises like every other method.
    with pytest.raises(graphersal.GraphersalError):
        store.journal()


def test_a_single_file_store_journal(tmp_path):
    path = str(tmp_path / "one.gstore")
    with graphersal.Store.create_file(path) as store:
        assert store.journal(kind="store.opened")["records"]
    assert graphersal.Store.journal_dir(path, kind="store.closed")["records"]


def cli():
    """The `graphersal` binary of this workspace, when it was built."""
    root = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", ".."))
    for profile in ("debug", "release"):
        binary = os.path.join(root, "target", profile, "graphersal")
        if os.path.exists(binary):
            return binary
    return shutil.which("graphersal")


def test_records_have_the_keys_of_the_cli_json(tmp_path):
    binary = cli()
    if binary is None:
        pytest.skip("the graphersal binary is not built")
    path = str(tmp_path / "data")
    graphersal.Store.create(path).close()
    out = subprocess.run(
        [binary, "store", "journal", path, "--json"], capture_output=True, text=True, check=True
    ).stdout
    from_cli = [json.loads(line) for line in out.splitlines() if line.strip()]
    from_python = graphersal.Store.journal_dir(path)["records"]
    assert [sorted(r) for r in from_cli] == [sorted(r) for r in from_python]
    assert [r["seq"] for r in from_cli] == [r["seq"] for r in from_python]
