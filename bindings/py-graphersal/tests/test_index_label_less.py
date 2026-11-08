"""Label-less property indexes (book page "Property Indexes", section "Label-less indexes"):
`Graph.create_index(None, key)` indexes every element of the kind, `Graph.indexes()` lists its
`label` as `None`, and `has(key, ..)` without a label reads it."""

import graphersal


def test_a_label_less_index_is_created_listed_served_and_dropped():
    graph = graphersal.Graph.tinkerpop_modern()
    graph.create_index(None, "name")
    graph.create_index(label=None, key="weight", element="edge")
    indexes = {index["name"]: index for index in graph.indexes()}
    assert indexes["name"]["label"] is None
    assert indexes["name"]["entries"] == 6
    assert indexes["weight_edge"]["label"] is None
    assert graph.execute('g.v().has("name", "josh").values("age").to_list()') == [32]
    profile = graph.execute('g.v().has("name", "josh").profile().to_string()')
    assert "[index: vertex name, any label]" in profile
    assert graph.drop_index("name") is True
    assert graph.drop_index("name") is False
