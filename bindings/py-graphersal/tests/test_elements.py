"""A vertex or an edge a query returns is a `graphersal.Vertex` / `graphersal.Edge` (like
gremlin-python's): its own id and label are kept apart from its properties, so a property named
`id` or `label` never hides them (issue #2)."""

import pytest

import graphersal


@pytest.fixture
def graph():
    return graphersal.Graph.tinkerpop_modern()


def test_a_vertex_keeps_id_label_and_properties_apart(graph):
    v = graph.query('g.V("1").next()')
    assert isinstance(v, graphersal.Vertex)
    assert repr(v) == "v[1]"
    assert v.id == "1"
    assert v.label == "person"
    assert v.labels == ["person"]
    assert v.properties == {"name": "marko", "age": 29}
    assert v["name"] == "marko"
    with pytest.raises(KeyError):
        v["label"]


def test_a_property_named_id_or_label_never_hides_the_vertex_own(graph):
    v = graph.query('g.addV("person").property("label", "CUSTOM").property("id", "X").next()')
    assert v.label == "person"
    assert v.id != "X"
    assert v["label"] == "CUSTOM"
    assert v["id"] == "X"
    assert v.properties == {"label": "CUSTOM", "id": "X"}


def test_an_edge_has_its_endpoints(graph):
    e = graph.query('g.E("0").next()')
    assert isinstance(e, graphersal.Edge)
    assert repr(e) == "e[0][1-knows->2]"
    assert (e.id, e.label) == ("0", "knows")
    assert e["weight"] == 0.5
    assert e.properties == {"weight": 0.5}
    assert isinstance(e.out_v, graphersal.Vertex)
    assert (e.out_v.id, e.out_v.label, e.out_v.properties) == ("1", "person", {})
    assert e.in_v.id == "2"


def test_edge_properties_may_hold_nested_objects(graph):
    e = graph.query(
        'g.V("1").addE("knows").to(__.V("2")).property("meta", #{since: 2020, tags: ["a"]}).next()'
    )
    assert e["meta"] == {"since": 2020, "tags": ["a"]}


def test_elements_compare_and_hash_by_id(graph):
    a = graph.query('g.V("1").next()')
    b = graph.query('g.V().has("name", "marko").next()')
    assert a == b
    assert len({a, b}) == 1
    assert a != graph.query('g.V("2").next()')
    assert graph.query('g.E("0").next()') == graph.query('g.E("0").next()')


def test_elements_pass_back_as_parameters(graph):
    v = graph.query('g.V("1").next()')
    assert graph.query('g.V(v).values("name").toList()', params={"v": v}) == ["marko"]
    e = graph.query('g.E("0").next()')
    assert graph.query('g.inject(e).inV().values("name").toList()', params={"e": e}) == ["vadas"]
    assert graph.query('g.V(e.out_v).values("name").toList()', params={"e": e}) == ["marko"]


def test_vertices_pass_as_add_e_endpoints(graph):
    # TinkerPop's from(Vertex) / to(Vertex): a Vertex parameter is that vertex, by its id.
    a = graph.query('g.V("1").next()')
    b = graph.query('g.V("2").next()')
    e = graph.query('g.addE("likes").from(a).to(b).next()', params={"a": a, "b": b})
    assert (e.out_v.id, e.in_v.id) == ("1", "2")
    e = graph.query('g.V("3").addE("likes").to(b).next()', params={"b": b})
    assert (e.out_v.id, e.in_v.id) == ("3", "2")
    # An edge's endpoint is a vertex too.
    e = graph.query('g.addE("back").from(e.in_v).to(e.out_v).next()', params={"e": e})
    assert (e.out_v.id, e.in_v.id) == ("2", "3")
    # An edge is not an endpoint.
    with pytest.raises(graphersal.GraphersalError):
        graph.query('g.V("1").addE("x").to(e).next()', params={"e": e})


def test_lists_and_paths_hold_elements(graph):
    path = graph.query('g.V("1").outE("knows").inV().has("name", "vadas").path().next()')
    assert [type(item).__name__ for item in path] == ["Vertex", "Edge", "Vertex"]
    assert [item.id for item in path] == ["1", "0", "2"]
