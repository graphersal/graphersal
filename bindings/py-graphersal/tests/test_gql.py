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


def test_a_saved_query_written_in_gql_is_defined_listed_and_called():
    graph = graphersal.Graph.tinkerpop_modern()
    graph.define_query(
        {
            "name": "older",
            "language": "gql",
            "params": {"min": {"schema": {"type": "integer"}, "default": 30}},
            "body": "MATCH (p:person) WHERE p.age > $min\nRETURN p.name AS name ORDER BY name",
        }
    )
    entry = graph.get_query("older")
    assert entry["language"] == "gql"
    assert entry["dialect"] == "graphersal-gql/1"
    assert entry["body"].startswith("MATCH (p:person)")
    assert [(q["name"], q["language"]) for q in graph.queries()] == [("older", "gql")]
    assert graph.call_query("older", {"min": 29}) == [{"name": "josh"}, {"name": "peter"}]
    assert graph.call_query("older") == [{"name": "josh"}, {"name": "peter"}]
    # Gremlin steps may continue the call.
    assert graph.execute('g.query("older", #{min: 0}).count().next()') == 4

    # One that writes: the writing dialect, its effects, denied under a read-only policy.
    graph.define_query(
        {
            "name": "add_person",
            "language": "gql",
            "params": {"name": "string"},
            "body": "INSERT (:person {name: $name})",
        }
    )
    entry = graph.get_query("add_person")
    assert entry["dialect"] == "graphersal-gql/2"
    assert [e["text"] for e in entry["effects"]] == ['Create Data(vertex "person")']
    graph.call_query("add_person", {"name": "ann"})
    assert graph.execute('g.v().has("name", "ann").count().next()') == 1
    with pytest.raises(graphersal.GraphersalError) as excinfo:
        graph.call_query("add_person", {"name": "bob"}, policy=graphersal.AccessPolicy.read_only())
    assert "Permission denied" in str(excinfo.value)
    assert graph.execute('g.v().has("name", "bob").count().next()') == 0

    # A $name the params do not declare is refused when it is saved.
    with pytest.raises(graphersal.GraphersalError) as excinfo:
        graph.define_query(
            {"name": "bad", "language": "gql", "body": "MATCH (p) WHERE p.age > $min RETURN p"}
        )
    assert "$min" in str(excinfo.value)
    assert graph.get_query("bad") is None
