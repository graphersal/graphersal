"""Unique property indexes (book page "Property Indexes", section "Unique indexes"):
`Graph.create_index(.., unique=True)` refuses writes that repeat a value, is refused over data
that already repeats one and with `each`, and `indexes()` lists `unique`."""

import pytest

import graphersal


def test_a_unique_index_refuses_a_repeated_value_and_is_listed():
    graph = graphersal.Graph.tinkerpop_modern()
    graph.create_index("person", "name", unique=True)
    (index,) = graph.indexes()
    assert index["unique"] is True
    assert index["served"] is True
    with pytest.raises(Exception) as exc_info:
        graph.execute('g.add_v("person").property("name", "marko").iterate()')
    assert "The unique index 'person_name' already holds" in str(exc_info.value)
    assert graph.execute('g.v().has("name", "marko").count().next()') == 1
    graph.create_index("software", "lang", unique=False)
    plain = {i["name"]: i for i in graph.indexes()}["software_lang"]
    assert plain["unique"] is False


def test_a_unique_index_over_repeated_values_or_with_each_is_refused():
    graph = graphersal.Graph.tinkerpop_modern()
    graph.execute('g.add_v("person").property("name", "marko").iterate()')
    with pytest.raises(Exception) as exc_info:
        graph.create_index("person", "name", unique=True)
    assert "Cannot create the unique index 'person_name'" in str(exc_info.value)
    assert graph.indexes() == []
    with pytest.raises(Exception) as exc_info:
        graph.create_index(None, "tags", transform="each", unique=True)
    assert "cannot use the transform each" in str(exc_info.value)
