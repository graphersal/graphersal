"""Layer visibility from Python (book page "Layered Graphs"): `Graph.layer_diff` and the DSL
steps `layer()` / `has_layer()`. A Python `Graph` is a plain graph (no layer can be created from
Python yet), so every element is "base" and the diff is empty; the keys and the argument checks
are the same as on a layer."""

import pytest

import graphersal
from graphersal import AccessPolicy


def modern():
    return graphersal.Graph.tinkerpop_modern()


def test_a_plain_graph_has_an_empty_diff_with_the_ordered_keys():
    diff = modern().layer_diff()
    assert diff == {"added": [], "removed": [], "modified": [], "truncated": False}
    # The order of LayerDiff::to_property, not the sorted order of a DSL map.
    assert list(diff) == ["added", "removed", "modified", "truncated"]


def test_the_options_are_accepted_in_every_form():
    graph = modern()
    empty = {"added": [], "removed": [], "modified": [], "truncated": False}
    assert graph.layer_diff(kinds="vertex") == empty
    assert graph.layer_diff(kinds=["vertex", "edge"], labels="person", limit=3) == empty
    assert graph.layer_diff(kinds=None, labels=["person", "software"], limit=None) == empty
    assert graph.layer_diff(labels=[]) == empty


@pytest.mark.parametrize(
    ("kwargs", "error", "text"),
    [
        ({"kinds": "node"}, ValueError, "unknown kind"),
        ({"kinds": []}, ValueError, "at least one kind"),
        ({"kinds": 1}, TypeError, "'kinds' is a str or a list of str"),
        ({"labels": ["person", 2]}, TypeError, "'labels' is a str or a list of str"),
        ({"limit": 0}, ValueError, "'limit' is a positive int"),
        ({"limit": -2}, ValueError, "'limit' is a positive int"),
        ({"limit": "3"}, TypeError, "'limit' is a positive int"),
        ({"limit": True}, TypeError, "'limit' is a positive int"),
    ],
)
def test_bad_options_raise(kwargs, error, text):
    with pytest.raises(error, match=text):
        modern().layer_diff(**kwargs)


def test_the_policy_is_asked_for_read_on_data():
    graph = modern()
    assert graph.layer_diff(policy=AccessPolicy.read_only())["truncated"] is False
    with pytest.raises(graphersal.PermissionDeniedError):
        graph.layer_diff(policy=AccessPolicy())


def test_the_dsl_steps_and_the_dsl_diff_run_from_python():
    graph = modern()
    assert graph.query("g.v().layer().dedup().to_list()") == ["base"]
    assert graph.query('g.v().has_layer("base").count().next()') == 6
    assert graph.query('g.e().has_layer("added", "modified").count().next()') == 0
    graph.execute('g.add_v("person").property("name", "ada").iterate()')
    # A write to a plain graph is not a layer: still "base", still nothing in the diff.
    assert graph.query('g.v().has("name", "ada").layer().next()') == "base"
    assert graph.query("g.layer_diff()") == {
        "added": [],
        "modified": [],
        "removed": [],
        "truncated": False,
    }
    assert graph.layer_diff()["added"] == []
    with pytest.raises(graphersal.GraphersalError, match="layer"):
        graph.query('g.v().has_layer("new").count().next()')


def test_the_memory_report_scope_delta_reports_a_plain_graph_whole():
    graph = modern()
    full = graph.memory_report()
    delta = graph.memory_report(scope="delta")
    # A plain graph owns all it shows: the delta scope reports every row, nothing is shared.
    assert (full["scope"], delta["scope"]) == ("all", "delta")
    assert delta["properties"] == full["properties"]
    assert delta["property_bytes"] == full["property_bytes"]
    assert delta["shared_bytes"] == 0 == full["shared_bytes"]
    assert delta["structure"]["totals"]["shared_bytes"] == 0
    assert graph.memory_report(scope="all")["scope"] == "all"
    with pytest.raises(graphersal.GraphersalError, match="scope"):
        graph.memory_report(scope="base")
