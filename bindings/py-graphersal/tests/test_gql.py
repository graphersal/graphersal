"""GQL read queries (`g.gql()`, feature `gql`) through the Python binding: the wheel is built
with the feature, so a GQL query runs from `Graph.query` like any traversal."""

import pytest

import graphersal


def test_gql_match_returns_rows_as_dicts():
    graph = graphersal.Graph.tinkerpop_modern()
    rows = graph.query(
        'g.gql("MATCH (p:person)-[:created]->(s:software) WHERE p.age > 30 '
        'RETURN p.name AS who, s.name AS what ORDER BY who, what").to_list()'
    )
    assert rows == [
        {"who": "josh", "what": "lop"},
        {"who": "josh", "what": "ripple"},
        {"who": "peter", "what": "lop"},
    ]


def test_gql_with_parameters_and_aggregation():
    graph = graphersal.Graph.tinkerpop_modern()
    rows = graph.query(
        'g.gql("MATCH (p:person) WHERE p.age > $min RETURN count(*) AS n", '
        '#{min: 30}).to_list()'
    )
    assert rows == [{"n": 2}]


def test_gql_syntax_error_raises_with_help():
    graph = graphersal.Graph.tinkerpop_modern()
    with pytest.raises(graphersal.GraphersalError) as exc_info:
        graph.query('g.gql("MATCH (p) RETRUN p").to_list()')
    assert "RETRUN" in str(exc_info.value)


def test_gql_refused_query_changes_nothing():
    graph = graphersal.Graph.tinkerpop_modern()
    with pytest.raises(graphersal.GraphersalError):
        graph.query(
            'g.gql("CALL { CREATE (n:person {name: \'x\'}) } IN TRANSACTIONS").to_list()'
        )
    assert graph.query('g.V().has("name", "x").count().next()') == 0


def test_gql_create_writes():
    graph = graphersal.Graph.tinkerpop_modern()
    rows = graph.query(
        'g.gql("CREATE (n:person {name: \'x\'}) RETURN n.name AS name").to_list()'
    )
    assert rows == [{"name": "x"}]
    assert graph.query('g.V().has("name", "x").count().next()') == 1
