"""Vector rules (book page "Vector Rules"): the schema keyword ``from`` round trip, the
``VectorFillWarning`` issued before a fill, and every case that removes the vector."""

import warnings

import pytest

import graphersal

RULE = {
    "vertices": {
        "doc": {
            "schema": {
                "type": "object",
                "properties": {
                    "text": {"type": "string"},
                    "emb": {
                        "type": "array",
                        "format": "vector",
                        "embedder": "hash-ngram/1",
                        "from": "text",
                    },
                },
            }
        }
    }
}


def ruled():
    graph = graphersal.Graph()
    graph.set_schema(RULE)
    return graph


def vectors(graph):
    return graph.execute('g.v().values("emb").count().next()')


def test_the_rule_round_trips_and_declares_its_dimension():
    graph = ruled()
    emb = graph.get_schema()["vertices"]["doc"]["schema"]["properties"]["emb"]
    assert emb["from"] == "text"
    assert emb["embedder"] == "hash-ngram/1"
    assert emb["dimension"] == 256


def test_a_fill_is_warned_before_it_runs():
    graph = graphersal.Graph()
    graph.execute('g.add_v("doc").property("text", "graph databases").next()')
    assert graph.validate_schema(RULE)["fill"][0]["elements"] == 1
    with pytest.warns(graphersal.VectorFillWarning, match="LOCKED"):
        graph.set_schema(RULE)
    assert vectors(graph) == 1


def test_a_warning_turned_error_applies_nothing():
    graph = graphersal.Graph()
    graph.execute('g.add_v("doc").property("text", "graph databases").next()')
    with warnings.catch_warnings():
        warnings.simplefilter("error", graphersal.VectorFillWarning)
        with pytest.raises(graphersal.VectorFillWarning):
            graph.set_schema(RULE)
    assert graph.get_schema() is None
    assert vectors(graph) == 0


@pytest.mark.parametrize(
    "write",
    [
        'g.v().properties("text").drop().iterate()',
        'g.v().property("text", "").iterate()',
        'g.v().property("text", 42).iterate()',
        'g.v().property("text", " .,; ").iterate()',
    ],
)
def test_a_source_without_text_removes_the_vector(write):
    graph = ruled()
    graph.execute('g.add_v("doc").property("text", "graph databases").next()')
    assert vectors(graph) == 1
    graph.execute(write)
    assert vectors(graph) == 0


def test_writing_the_vector_directly_is_refused():
    graph = ruled()
    with pytest.raises(graphersal.GraphersalError) as raised:
        graph.execute('g.add_v("doc").property("emb", vector([1.0])).next()')
    assert "write text instead" in (raised.value.help or "")
