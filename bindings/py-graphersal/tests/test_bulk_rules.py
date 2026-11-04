"""Bulk compression rules and several advisor recommendations at once (stage V1b):
``graph.define_compressions(rules)``, ``graph.advise()`` and ``graph.apply_recommendations(ids)``,
each one commit, all or nothing."""

import pytest

import graphersal

FILES = (
    'for i in 0..200 { g.add_v("file")'
    '.property("kind", if i % 2 == 0 { "doc" } else { "img" })'
    '.property("size", i % 3)'
    '.property("owner", "user-" + (i % 4)).iterate(); }'
)


def files_graph():
    graph = graphersal.Graph()
    graph.execute(FILES)
    return graph


def rule_names(graph):
    return sorted(rule["name"] for rule in graph.compressions())


def test_a_list_of_rules_is_stored_in_one_call_with_paths():
    graph = files_graph()
    kinds = sorted(graph.execute('g.v().values("kind").dedup().to_list()'))
    stored = graph.define_compressions(
        [
            {"label": "file", "paths": ["kind", "owner"], "codec": "dedup"},
            {"name": "sizes", "label": "file", "path": "size", "codec": "dedup"},
        ]
    )
    assert stored == 3
    assert rule_names(graph) == ["file_kind", "file_owner", "sizes"]
    assert all(rule["compressed_values"] == 200 for rule in graph.compressions())
    # Reads are plain.
    assert sorted(graph.execute('g.v().values("kind").dedup().to_list()')) == kinds


def test_a_refused_rule_stores_none_of_the_list():
    graph = files_graph()
    with pytest.raises(graphersal.GraphersalError) as excinfo:
        graph.define_compressions(
            [
                {"name": "ok", "label": "file", "path": "kind", "codec": "dedup"},
                {"name": "bad", "label": "file", "path": "size", "codec": "dedup", "dictionary": True},
            ]
        )
    assert "bad" in str(excinfo.value)
    assert rule_names(graph) == []
    with pytest.raises(graphersal.PermissionDeniedError):
        graph.define_compressions(
            [{"label": "file", "paths": ["kind"], "codec": "dedup"}],
            policy=graphersal.AccessPolicy.read_only(),
        )
    assert rule_names(graph) == []


def test_a_group_of_recommendations_is_applied_at_once():
    graph = files_graph()
    advice = graph.advise(everything=True)
    group = next(g for g in advice["groups"] if g["key"] == "add_compression:dedup")
    assert len(group["ids"]) >= 3
    assert group["title"].startswith(f"dedup: {len(group['ids'])} keys, total -")
    applied = graph.apply_recommendations(group["ids"], everything=True)
    assert [a["id"] for a in applied] == group["ids"]
    assert sum(a["memory_bytes"] for a in applied) == group["memory_bytes"]
    assert len(graph.compressions()) == len(group["ids"])
    after = graph.advise(everything=True)
    advised = {r["id"] for r in after["recommendations"]}
    assert not advised & set(group["ids"])


def test_applying_recommendations_is_all_or_nothing_and_asks_the_policy():
    graph = files_graph()
    ids = [g for g in graph.advise(everything=True)["groups"] if g["key"] == "add_compression:dedup"][0]["ids"]
    with pytest.raises(graphersal.GraphersalError) as excinfo:
        graph.apply_recommendations(ids + ["add_compression:vertex:file:nope"], everything=True)
    assert "nope" in str(excinfo.value)
    assert graph.compressions() == []
    with pytest.raises(graphersal.PermissionDeniedError):
        graph.apply_recommendations(ids, everything=True, policy=graphersal.AccessPolicy.read_only())
    with pytest.raises(graphersal.PermissionDeniedError):
        graph.advise(policy=graphersal.AccessPolicy.deny_all())
    assert graph.compressions() == []
