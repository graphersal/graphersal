"""`graphersal.AccessPolicy` and the `policy=` keyword: requests denied and allowed by action
and resource kind, the file root, the allow-all default and the error classes.
"""

import json
import os

import pytest

import graphersal
from graphersal import AccessPolicy

SCHEMA = {
    "mode": "open",
    "vertices": {"tagged": {"schema": {"type": "object", "properties": {"n": {"type": "integer"}}}}},
}


def script_path(path):
    # Script string literals: forward slashes, no escapes needed.
    return os.fspath(path).replace("\\", "/")


def assert_denied(graph, script, policy, request):
    with pytest.raises(graphersal.PermissionDeniedError) as exc_info:
        graph.execute(script, policy=policy)
    err = exc_info.value
    assert isinstance(err, graphersal.GraphersalError)
    assert "Permission denied" in str(err)
    assert f"({request}" in str(err)
    assert err.help is not None
    return err


# --- the AccessPolicy value -----------------------------------------------------------------


def test_presets_and_builders():
    assert AccessPolicy.deny_all().rules == frozenset()
    assert AccessPolicy.read_only().rules == {("read", "data"), ("read", "schema"), ("read", "definition"), ("execute", "definition"), ("execute", "embedder"), ("read", "journal")}
    assert len(AccessPolicy.allow_all().rules) == 6 * 8
    p = AccessPolicy.read_only().allow("create", "data").allow("*", "option").deny("*", "option")
    assert p.rules == {("read", "data"), ("read", "schema"), ("read", "definition"), ("execute", "definition"), ("execute", "embedder"), ("read", "journal"), ("create", "data")}
    assert p == AccessPolicy(
        ("read", "data"), ("read", "schema"), ("read", "definition"), ("execute", "definition"), ("execute", "embedder"), ("read", "journal"), ("create", "data")
    )
    assert p.allows("create", "data") and not p.allows("read", "file")
    assert not p.allows("*", "data")
    assert AccessPolicy.allow_all().deny("*", "file").allows("*", "data")
    assert p.file_root is None


def test_policies_are_immutable_hashable_and_repr_round_trips(tmp_path):
    base = AccessPolicy.read_only()
    widened = base.allow("update", "data")
    assert base.rules == {("read", "data"), ("read", "schema"), ("read", "definition"), ("execute", "definition"), ("execute", "embedder"), ("read", "journal")}  # unchanged
    same = AccessPolicy(("read", "data"), ("read", "schema"), ("read", "definition"), ("execute", "definition"), ("execute", "embedder"), ("read", "journal"))
    assert len({same, AccessPolicy.read_only(), widened}) == 2
    rooted = AccessPolicy(("read", "*"), ("create", "file"), file_root=tmp_path)
    assert rooted.file_root == os.fspath(tmp_path)
    assert eval(repr(rooted), {"AccessPolicy": AccessPolicy}) == rooted
    assert rooted.with_file_root(None).file_root is None
    with pytest.raises(AttributeError):
        rooted.file_root = None


def test_unknown_names_raise_value_error():
    with pytest.raises(ValueError, match="'file'"):
        AccessPolicy(("read", "files"))
    with pytest.raises(ValueError, match="'create'"):
        AccessPolicy.read_only().allow("write", "data")
    with pytest.raises(ValueError):
        AccessPolicy.read_only().allows(1, "data")
    with pytest.raises(ValueError):
        AccessPolicy("read")


# --- requests, denied and allowed -----------------------------------------------------------


def test_read():
    graph = graphersal.Graph.tinkerpop_modern()
    assert_denied(graph, "g.V().count().next()", AccessPolicy.deny_all(), "Read Data(vertex)")
    assert graph.query("g.V().count().next()", policy=AccessPolicy.read_only()) == 6
    # A script can still compute with its own values without any rule.
    assert graph.execute("1 + 2", policy=AccessPolicy.deny_all()) == 3


def test_create_update_delete():
    graph = graphersal.Graph.tinkerpop_modern()
    read = AccessPolicy.read_only()
    assert_denied(graph, 'g.addV("x").next()', read, 'Create Data(vertex "x")')
    assert_denied(graph, 'g.V().property("k", 1).toList()', read, "Update Data")
    assert_denied(graph, "g.V().drop().toList()", read, "Delete Data")
    assert graph.query("g.V().count().next()") == 6  # nothing ran
    graph.execute('g.addV("x").next()', policy=read.allow("create", "data"))
    assert graph.query("g.V().count().next()") == 7


def test_schema(tmp_path):
    graph = graphersal.Graph.tinkerpop_modern()
    text = json.dumps(SCHEMA)
    set_schema = f"g.set_schema(`{text}`)"
    files = AccessPolicy.read_only().allow("read", "file")
    assert_denied(graph, set_schema, files, "Update Schema")
    graph.execute(set_schema, policy=AccessPolicy.read_only().allow("update", "schema"))
    # The Graph methods take the same policy.
    with pytest.raises(graphersal.PermissionDeniedError):
        graph.set_schema(SCHEMA, policy=AccessPolicy.read_only())
    assert graph.get_schema(policy=AccessPolicy.read_only())["mode"] == "open"
    with pytest.raises(graphersal.PermissionDeniedError):
        graph.infer_schema(policy=AccessPolicy.deny_all())


def test_file(tmp_path):
    graph = graphersal.Graph.tinkerpop_modern()
    out = tmp_path / "out.graphml"
    export = f'g.export_graphml("{script_path(out)}")'
    assert_denied(graph, export, AccessPolicy.read_only(), "Create File(")
    assert not out.exists()
    graph.execute(export, policy=AccessPolicy.read_only().allow("*", "file"))
    assert out.exists()


def test_option():
    graph = graphersal.Graph.tinkerpop_modern()
    script = 'g.with("evaluationTimeout", 10000).V().count().next()'
    assert_denied(
        graph, script, AccessPolicy.read_only(), 'Update Option("evaluationTimeout")'
    )
    assert graph.query(script, policy=AccessPolicy.read_only().allow("update", "option")) == 6


def test_default_allows_everything(tmp_path):
    graph = graphersal.Graph.tinkerpop_modern()
    out = tmp_path / "default.graphml"
    graph.execute(f'g.export_graphml("{script_path(out)}")')
    graph.execute('g.with("evaluationTimeout", 10000).addV("x").next()')
    assert out.exists()
    assert graph.query("g.V().count().next()") == 7


# --- the file root --------------------------------------------------------------------------


def test_file_root_resolves_relative_paths_and_denies_paths_outside(tmp_path):
    root = tmp_path / "root"
    root.mkdir()
    graph = graphersal.Graph.tinkerpop_modern()
    rooted = AccessPolicy.read_only().allow("*", "file").with_file_root(root)

    graph.execute('g.export_graphml("inside.graphml")', policy=rooted)
    assert (root / "inside.graphml").exists()

    err = assert_denied(graph, 'g.export_graphml("../outside.graphml")', rooted, "Create File(")
    assert "outside the file root" in str(err)
    outside = tmp_path / "elsewhere.graphml"
    assert_denied(graph, f'g.export_graphml("{script_path(outside)}")', rooted, "Create File(")
    assert not (tmp_path / "outside.graphml").exists()
    assert not outside.exists()


def test_file_root_does_not_allow_files(tmp_path):
    graph = graphersal.Graph.tinkerpop_modern()
    rooted = AccessPolicy.read_only().with_file_root(tmp_path)
    assert_denied(graph, 'g.export_graphml("x.graphml")', rooted, "Create File(")


# --- error classes --------------------------------------------------------------------------


def test_error_class_hierarchy():
    for cls in (
        graphersal.PermissionDeniedError,
        graphersal.CommitRejectedError,
        graphersal.ReturnedClosureError,
    ):
        assert issubclass(cls, graphersal.GraphersalError)
        assert cls.__module__ == "graphersal"
        assert cls.__doc__
    assert graphersal.CommitRejectedError.help is None  # inherited class-level default


def test_atomic_denial_rolls_back_the_script():
    graph = graphersal.Graph.tinkerpop_modern()
    with pytest.raises(graphersal.PermissionDeniedError):
        graph.execute(
            'g.addV("x").next(); g.export_graphml("nope.graphml")',
            atomic=True,
            policy=AccessPolicy.read_only().allow("create", "data"),
        )
    assert graph.query("g.V().count().next()") == 6
