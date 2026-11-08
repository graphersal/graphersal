"""Property indexes with the transform `each` (book page "Property Indexes", section "Arrays:
the each transform"): `Graph.create_index(.., transform="each")` indexes every element of a
list value, a chain is a list of names, and `has(key, __.unfold().is(..))` reads the index."""

import pytest

import graphersal


def test_an_each_index_and_a_chain_are_created_listed_and_served():
    graph = graphersal.Graph.tinkerpop_modern()
    graph.execute('g.v("1").property("tags", ["Rust", "go"]).iterate()')
    graph.execute('g.v("2").property("tags", ["go"]).iterate()')
    graph.create_index(None, "tags", transform="each")
    graph.create_index("person", "tags", transform=["each", "to_lower"])
    indexes = {index["name"]: index for index in graph.indexes()}
    assert indexes["tags_each"]["transform"] == "each"
    assert indexes["tags_each"]["entries"] == 3
    assert indexes["person_tags_each_to_lower"]["transform"] == ["each", "to_lower"]
    assert sorted(
        graph.execute('g.v().has("tags", __.unfold().is("go")).values("name").to_list()')
    ) == ["marko", "vadas"]
    profile = graph.execute('g.v().has("tags", __.unfold().is("go")).profile().to_string()')
    assert "[index: vertex tags each, any label]" in profile
    lowered = graph.execute(
        'g.v().has_label("person").has("tags", __.unfold().to_lower().is("rust")).profile().to_string()'
    )
    assert "[index: vertex person.tags each to_lower]" in lowered


def test_a_chain_in_the_wrong_order_is_refused():
    graph = graphersal.Graph.tinkerpop_modern()
    with pytest.raises(Exception) as exc_info:
        graph.create_index("person", "tags", transform=["to_lower", "each"])
    assert "is not one this version serves" in str(exc_info.value)
