"""Python-facing tests for the `graphersal` extension module, run against a `maturin develop`
build (see the design spec's Testing section). Rust-side conversion/error-mapping unit tests live
in `src/convert.rs` and `src/error.rs`; these cover the surface Python code actually calls.
"""

import datetime
import json
import os
import uuid

import pytest

import graphersal


def test_empty_graph_execute_mutation_round_trips():
    graph = graphersal.Graph()
    graph.execute('g.addV("person").property("name", "Alice").next()')
    names = graph.query('g.V().values("name").to_list()')
    assert names == ["Alice"]


def test_iterate_runs_a_traversal_for_its_side_effects():
    # No per-traversal terminal API in Python: `iterate()` comes through the Rhai DSL.
    graph = graphersal.Graph()
    assert graph.execute('g.addV("person").property("name", "Ann").iterate()') is None
    assert graph.query('g.V().values("name").to_list()') == ["Ann"]


def test_tinkerpop_modern_query_with_element_map():
    graph = graphersal.Graph.tinkerpop_modern()
    rows = graph.query('g.V().hasLabel("person").order().by("name").elementMap().to_list()')
    assert [row["name"] for row in rows] == ["josh", "marko", "peter", "vadas"]
    assert rows[0]["label"] == "person"


def test_parameterized_query_round_trips_scalars():
    graph = graphersal.Graph()
    import uuid

    value = uuid.uuid4()
    result = graph.execute("params.x", params={"x": value})
    assert result == value
    assert graph.execute("params.n", params={"n": 3}) == 3
    assert graph.execute("params.f", params={"f": 1.5}) == 1.5
    assert graph.execute("params.s", params={"s": "hi"}) == "hi"
    assert graph.execute("params.b", params={"b": True}) is True
    assert graph.execute("params.z", params={"z": None}) is None


def test_error_case_raises_graphersal_error_with_help():
    graph = graphersal.Graph.tinkerpop_modern()
    with pytest.raises(graphersal.GraphersalError) as exc_info:
        graph.execute('UUID.fromString("not-a-uuid")')
    err = exc_info.value
    assert "Cast exception" in str(err)
    assert err.help is not None
    assert "canonical form" in err.help


def test_error_without_help_has_none_help():
    graph = graphersal.Graph()
    with pytest.raises(graphersal.GraphersalError) as exc_info:
        graph.execute("let x = ")
    assert exc_info.value.help is None


def test_list_param_is_rejected():
    graph = graphersal.Graph()
    with pytest.raises(graphersal.GraphersalError):
        graph.execute("params.xs", params={"xs": [1, 2, 3]})


def test_from_graphml_export_round_trip(tmp_path):
    path = os.fspath(tmp_path / "roundtrip.graphml")
    original = graphersal.Graph.tinkerpop_modern()
    original.export_graphml(path)

    loaded = graphersal.Graph.from_graphml(path)
    names = sorted(loaded.query('g.V().hasLabel("person").values("name").to_list()'))
    assert names == ["josh", "marko", "peter", "vadas"]


def test_from_graphson_export_round_trip(tmp_path):
    path = os.fspath(tmp_path / "roundtrip.json")
    original = graphersal.Graph.tinkerpop_modern()
    original.export_graphson(path)
    first = open(path, encoding="utf-8").readline()
    assert first.startswith('{"id":"1","label":"person",')

    loaded = graphersal.Graph.from_graphson(path)
    names = sorted(loaded.query('g.V().hasLabel("person").values("name").to_list()'))
    assert names == ["josh", "marko", "peter", "vadas"]
    # The script methods in both spellings, into a live graph.
    target = graphersal.Graph()
    message = target.query(f'g.importGraphson("{tmp_path.as_posix()}/roundtrip.json")')
    assert "imported 6 vertices and 6 edges" in message
    assert target.query("g.E().count().next()") == 6


def test_from_graphson_warns_about_skipped_dates(tmp_path):
    path = tmp_path / "dates.json"
    path.write_text(
        '{"id":"v","label":"x","properties":{"born":[{"value":{"@type":"gx:Year","@value":"2016"}}],'
        '"seen":[{"value":{"@type":"g:Date","@value":1}}],"name":[{"value":"n"}]}}\n'
    )
    with pytest.warns(UserWarning, match=r"skipped date/time values \(gx:Year") as record:
        graph = graphersal.Graph.from_graphson(os.fspath(path))
    assert not any("g:Date" in str(w.message) for w in record)
    assert graph.query('g.V().values("name").next()') == "n"
    assert graph.query('g.V().has("born").count().next()') == 0
    # A date/time type Graphersal holds is imported.
    assert graph.query('g.V().values("seen").next()') == datetime.datetime(
        1970, 1, 1, 0, 0, 0, 1000, tzinfo=datetime.timezone.utc
    )


def test_from_graphson_raises_on_malformed_input(tmp_path):
    path = tmp_path / "broken.json"
    path.write_text('{"id":"a"}\n{"id":\n')
    with pytest.raises(graphersal.GraphersalError) as excinfo:
        graphersal.Graph.from_graphson(os.fspath(path))
    assert "line 2" in str(excinfo.value)
    assert excinfo.value.help


SCHEMA = {
    "mode": "open",
    "vertices": {
        "doc": {
            "schema": {
                "type": "object",
                "properties": {"tags": {"type": "array", "items": {"type": "string"}}},
            }
        }
    },
}


def test_graphml_carries_no_schema_set_it_first_for_typed_values(tmp_path):
    out = tmp_path / "out.graphml"
    original = graphersal.Graph()
    original.set_schema(SCHEMA)
    original.execute(
        'g.add_v("doc").property("tags", ["a", "b"]).next(); '
        f'g.export_graphml("{out.as_posix()}")'
    )
    assert "graphersal.schema" not in out.read_text()

    # Without the schema the JSON text stays a string; never guessed.
    plain = graphersal.Graph.from_graphml(os.fspath(out))
    assert plain.query('g.V().values("tags").next()') == '["a","b"]'

    typed = graphersal.Graph()
    typed.set_schema(json.dumps(SCHEMA))
    typed.execute(f'g.import_graphml("{out.as_posix()}")')
    assert typed.query('g.V().values("tags").next()') == ["a", "b"]


UUID_SCHEMA = {
    "vertices": {
        "item": {
            "schema": {
                "type": "object",
                "properties": {
                    "key": {"type": "string", "format": "uuid"},
                    "name": {"type": "string"},
                },
            }
        }
    },
}
UUID_TEXT = "6f1f0c1e-2d3b-4c5a-9e8f-0a1b2c3d4e5f"


def write_uuid_graphml(tmp_path):
    out = tmp_path / "items.graphml"
    original = graphersal.Graph()
    original.set_schema(UUID_SCHEMA)
    original.execute(
        f'g.add_v("item").property("key", UUID.fromString("{UUID_TEXT}"))'
        '.property("name", "a").next(); '
        f'g.export_graphml("{out.as_posix()}")'
    )
    return os.fspath(out)


def test_from_graphml_with_schema_restores_uuid(tmp_path):
    path = write_uuid_graphml(tmp_path)

    plain = graphersal.Graph.from_graphml(path)
    assert plain.query('g.V().values("key").next()') == UUID_TEXT  # lost: a string

    typed = graphersal.Graph.from_graphml(path, schema=UUID_SCHEMA)
    assert typed.query('g.V().values("key").next()') == uuid.UUID(UUID_TEXT)
    assert typed.get_schema()["mode"] == "none"
    # JSON text works too, and `mode` replaces the schema's own mode.
    opened = graphersal.Graph.from_graphml(path, schema=json.dumps(UUID_SCHEMA), mode="open")
    assert opened.get_schema()["mode"] == "open"
    assert opened.query('g.V().values("key").next()') == uuid.UUID(UUID_TEXT)


def test_from_graphml_closed_mode_violation_raises(tmp_path):
    path = write_uuid_graphml(tmp_path)
    other = {"vertices": {"thing": {"schema": {"type": "object"}}}}
    with pytest.raises(graphersal.GraphersalError, match="'item' is not declared"):
        graphersal.Graph.from_graphml(path, schema=other, mode="closed")
    # The same file loads into a closed graph whose schema declares it.
    closed = graphersal.Graph.from_graphml(path, schema=UUID_SCHEMA, mode="closed")
    assert closed.query("g.V().count().next()") == 1


def test_from_graphson_with_schema(tmp_path):
    path = os.fspath(tmp_path / "modern.json")
    graphersal.Graph.tinkerpop_modern().export_graphson(path)
    schema = graphersal.Graph.tinkerpop_modern().infer_schema()
    loaded = graphersal.Graph.from_graphson(path, schema=schema, mode="closed")
    assert loaded.get_schema()["mode"] == "closed"
    assert loaded.query("g.V().count().next()") == 6
    with pytest.raises(graphersal.GraphersalError):
        graphersal.Graph.from_graphson(path, schema=UUID_SCHEMA, mode="closed")


def test_mode_without_schema_is_refused(tmp_path):
    path = write_uuid_graphml(tmp_path)
    with pytest.raises(ValueError, match="schema="):
        graphersal.Graph.from_graphml(path, mode="open")


def test_import_methods_into_a_live_graph(tmp_path):
    graphml = write_uuid_graphml(tmp_path)
    graphson = os.fspath(tmp_path / "modern.json")
    graphersal.Graph.tinkerpop_modern().export_graphson(graphson)

    graph = graphersal.Graph()
    graph.set_schema(UUID_SCHEMA)
    graph.import_graphml(graphml)
    assert graph.query('g.V().values("key").next()') == uuid.UUID(UUID_TEXT)
    plain = graphersal.Graph()
    plain.import_graphson(graphson)
    assert plain.query("g.E().count().next()") == 6


def test_import_into_a_live_graph_is_atomic(tmp_path):
    graph = graphersal.Graph.tinkerpop_modern()
    broken = tmp_path / "broken.json"
    # A valid first vertex, then a line that cannot be parsed: nothing of the file stays.
    broken.write_text('{"id":"new","label":"x","properties":{}}\n{"id":\n')
    with pytest.raises(graphersal.GraphersalError):
        graph.import_graphson(os.fspath(broken))
    assert graph.query("g.V().count().next()") == 6

    # A schema violation in the middle of a GraphML file rolls the whole import back.
    graphml = write_uuid_graphml(tmp_path)
    closed = graphersal.Graph.tinkerpop_modern()
    closed.set_schema(closed.infer_schema(), mode="closed")
    with pytest.raises(graphersal.GraphersalError, match="'item' is not declared"):
        closed.import_graphml(graphml)
    assert closed.query("g.V().count().next()") == 6
    assert closed.query('g.V().hasLabel("item").count().next()') == 0


def test_the_seven_schema_methods():
    graph = graphersal.Graph.tinkerpop_modern()
    assert graph.get_schema() is None
    inferred = graph.infer_schema()
    assert inferred["$schema"] == "https://graphersal.dev/schema/v1"
    assert inferred["mode"] == "none"
    assert inferred["vertices"]["person"]["schema"]["required"] == ["name", "age"]
    assert graph.get_schema() is None  # inference stores nothing

    closed = dict(inferred, mode="closed")
    check = graph.validate_schema(closed)
    assert check["violations"]["total_violations"] == 0
    assert check["compatible"] is False  # none -> closed is stricter
    graph.set_schema(closed)
    assert graph.get_schema() == closed

    patched = graph.patch_schema({"vertices": {"person": {"schema": {"properties": {"nick": {"type": "string"}}}}}})
    assert "nick" in patched["vertices"]["person"]["schema"]["properties"]
    # dicts keep the engine's key order
    assert list(patched["vertices"]["person"]["schema"]["properties"]) == ["name", "age", "nick"]
    diff = graph.diff_schema(closed)
    assert diff["changes"][0]["kind"] == "property_removed"
    preview = graph.validate_schema_patch('{"vertices": {"person": {"schema": {"required": ["name", "age", "nick"]}}}}')
    assert preview["violations"]["groups"][0]["kind"] == "missing_required"
    assert preview["violations"]["groups"][0]["example_field"] is None  # no array index
    assert graph.get_schema() == patched  # nothing applied

    with pytest.raises(graphersal.GraphersalError) as excinfo:
        graph.set_schema({"mdoe": "closed"})
    assert "unknown key 'mdoe'" in str(excinfo.value)
    assert excinfo.value.help
    with pytest.raises(graphersal.GraphersalError) as excinfo:
        graph.patch_schema({"vertices": {"person": {"schema": {"required": ["name", "age", "nick"]}}}})
    assert "is declared mandatory but is missing" in str(excinfo.value)
    assert "backfill" in (excinfo.value.help or "")
    graph.set_schema({})
    assert graph.get_schema() is None


def test_freeze_the_structure_with_a_mode():
    graph = graphersal.Graph.tinkerpop_modern()
    check = graph.validate_schema(graph.infer_schema(), "closed")
    assert check["violations"]["mode"] == "closed"
    assert check["violations"]["total_violations"] == 0
    graph.set_schema(graph.infer_schema(), mode="closed")
    assert graph.get_schema()["mode"] == "closed"
    with pytest.raises(graphersal.GraphersalError):
        graph.execute('g.addV("robot").next()')
    # The mode wins over the schema's own one.
    graph.set_schema(dict(graph.get_schema(), mode="closed"), mode="open")
    assert graph.get_schema()["mode"] == "open"
    with pytest.raises(graphersal.GraphersalError) as excinfo:
        graph.set_schema(graph.infer_schema(), mode="Closed")
    assert "unknown mode 'Closed'" in str(excinfo.value)
    assert graph.get_schema()["mode"] == "open"
    # Unlabeled elements: open freezes, closed names them.
    bare = graphersal.Graph()
    bare.execute('g.addV().next(); g.addV("a").next()')
    with pytest.raises(graphersal.GraphersalError) as excinfo:
        bare.set_schema(bare.infer_schema(), mode="closed")
    assert "1 vertex without a label" in str(excinfo.value)
    bare.set_schema(bare.infer_schema(), mode="open")


def test_execution_reports_changes():
    graph = graphersal.Graph.tinkerpop_modern()
    result = graph.execute('g.addV("x").execute()')
    assert result["mutated"] is True
    assert result["changes"] == {"data": True, "schema": False, "definitions": False}
    read = graph.execute("g.V().count().execute()")
    assert read["changes"] == {"data": False, "schema": False, "definitions": False}


def test_max_value_depth_is_configurable():
    graph = graphersal.Graph()
    deep = "let x = 1; for i in 0..200 { x = [x]; } g.inject(x).count().next()"
    with pytest.raises(graphersal.GraphersalError) as excinfo:
        graph.execute(deep)
    assert "script.max_value_depth" in str(excinfo.value)
    assert "max_value_depth=256" in (excinfo.value.help or "")
    graph.set_limits(max_value_depth=300)
    assert graph.execute(deep) == 1


def test_scripts_run_on_a_large_stack():
    # Rhai copies nested maps recursively; on an 8 MiB stack a debug build aborted at a few
    # thousand levels. The script worker's 64 MiB stack keeps this one alive.
    graph = graphersal.Graph()
    graph.set_limits(unlimited=True)
    assert graph.execute("let x = 1; for i in 0..5000 { x = #{a: x}; } 7") == 7


def test_infinite_loop_is_stopped_by_the_default_limits_and_limits_are_configurable():
    graph = graphersal.Graph()
    graph.set_limits(max_operations=50_000)
    with pytest.raises(graphersal.GraphersalError) as excinfo:
        graph.execute("let n = 0; loop { n += 1; }")
    assert "script.max_operations" in str(excinfo.value)
    assert "with_max_operations" in (excinfo.value.help or "")


def test_memory_budget_fails_an_oversized_traversal_and_rolls_back():
    assert graphersal.Graph.tinkerpop_modern().execute("g.v().count().next()") == 6
    # The extension installs the tracking allocator, so the budget compares the live Rust heap of
    # the extension (a few MiB here). 32 MiB leaves room for the modern graph, not for 200 new
    # vertices with a 256 KiB string each (50 MiB).
    graph = graphersal.Graph.tinkerpop_modern(memory_limit=32 << 20)
    assert graph.execute("g.v().count().next()") == 6
    with pytest.raises(graphersal.GraphersalError) as excinfo:
        graph.execute(
            'let s = "x"; for i in 0..18 { s += s; } '
            "let xs = []; for i in 0..200 { xs.push(i); } "
            'g.inject(xs).unfold().addV("tmp").property("text", s).toList()'
        )
    assert "memory.limit" in str(excinfo.value)
    assert "--memory-limit" in (excinfo.value.help or "")
    assert graph.execute("g.v().count().next()") == 6
    # "auto" without a cgroup limit is no budget; with one, this small query fits.
    auto = graphersal.Graph(memory_limit="auto", memory_headroom=0)
    assert auto.execute("g.v().count().next()") == 0
    for bad, error in [("lots", ValueError), (0, ValueError), (1.5, TypeError)]:
        with pytest.raises(error):
            graphersal.Graph(memory_limit=bad)
    with pytest.raises(ValueError):
        graphersal.Graph(memory_headroom=10)


def test_memory_profile_reports_tracked_bytes():
    # The extension installs `TrackingAllocator`: exact allocator figures, not estimates.
    graph = graphersal.Graph.tinkerpop_modern()
    profile = graph.execute("g.v().out().group().by(T.label).profile_with(ProfileType.Memory)")
    memory = [step["memory"] for step in profile["metrics"]]
    assert memory and all(m["source"] == "tracked" for m in memory)
    assert sum(m["alloc_count"] for m in memory) > 0
    # A bare profile collects no memory data at all.
    plain = graph.execute("g.v().out().profile()")
    assert all(step.get("memory") is None for step in plain["metrics"])


def test_saved_queries_define_list_call_and_drop():
    graph = graphersal.Graph.tinkerpop_modern()
    graph.define_query(
        {
            "name": "older_than",
            "folder": "people",
            "description": "People older than an age.",
            "params": {"age": {"schema": {"type": "integer", "minimum": 0}, "default": 30}},
            "body": 'fn older_than(age) { g.v().has_label("person").has("age", P.gt(age)).values("name") }',
        }
    )
    listed = graph.queries("people")
    assert [q["name"] for q in listed] == ["older_than"]
    assert listed[0]["status"] == "ok"
    assert listed[0]["params"][0]["default"] == 30
    assert graph.queries("other") == []
    assert graph.get_query("older_than")["body"].startswith("fn older_than")
    assert graph.get_query("none") is None
    assert sorted(graph.call_query("older_than", {"age": 29})) == ["josh", "peter"]
    assert sorted(graph.call_query("older_than")) == ["josh", "peter"]
    with pytest.raises(graphersal.GraphersalError) as excinfo:
        graph.call_query("older_than", {"age": "x"})
    assert "age" in str(excinfo.value)
    with pytest.raises(graphersal.GraphersalError):
        graph.define_query({"name": "bad", "body": "fn other() { 1 }"})
    assert graph.drop_query("older_than") is True
    assert graph.drop_query("older_than") is False


def test_a_saved_query_that_writes_lists_its_effects_and_writes_once():
    graph = graphersal.Graph.tinkerpop_modern()
    graph.define_query(
        {
            "name": "add_person",
            "params": {"name": "string"},
            "body": 'fn add_person(name) { g.add_v("person").property("name", name).next() }',
        }
    )
    entry = graph.get_query("add_person")
    assert entry["dialect"] == "graphersal-rhai/2"
    assert entry["writes"] is True
    assert [e["text"] for e in entry["effects"]] == [
        'Create Data(vertex "person")',
        'Update Data(vertex "person")',
    ]
    graph.call_query("add_person", {"name": "ann"})
    assert graph.execute('g.v().has("name", "ann").count().next()') == 1
    # Under a read-only policy the call is denied before it writes.
    with pytest.raises(graphersal.GraphersalError) as excinfo:
        graph.call_query("add_person", {"name": "bob"}, policy=graphersal.AccessPolicy.read_only())
    assert "Permission denied" in str(excinfo.value)
    assert graph.execute('g.v().has("name", "bob").count().next()') == 0


def test_compression_rules_define_list_recompress_and_drop():
    graph = graphersal.Graph()
    cv = "Senior engineer, led the platform team in Rust. " * 20
    graph.execute('g.add_v("customer").property("cv", text).to_list()', {"text": cv})
    graph.define_compression("cv", "customer", "cv", min_bytes=64)
    rules = graph.compressions()
    assert [r["name"] for r in rules] == ["cv"]
    assert rules[0]["path"] == "$.cv"
    assert rules[0]["element"] == "vertex"
    assert rules[0]["compressed_values"] == 1
    assert rules[0]["stored_bytes"] < rules[0]["plain_bytes"]
    # Reads are plain.
    assert graph.execute('g.v().values("cv").to_list()') == [cv]
    graph.recompress("cv")
    graph.define_compression("notes", "bought", "notes", element="edge", dictionary=False)
    assert len(graph.compressions()) == 2
    with pytest.raises(graphersal.GraphersalError):
        graph.define_compression("bad", "customer", "list[0]")
    with pytest.raises(graphersal.GraphersalError):
        graph.recompress("nope")
    assert graph.drop_compression("cv") is True
    assert graph.drop_compression("cv") is False
    assert graph.execute('g.v().values("cv").to_list()') == [cv]


def test_property_indexes_create_list_serve_and_drop():
    graph = graphersal.Graph.tinkerpop_modern()
    graph.create_index("person", "age")
    graph.create_index("knows", "weight", element="edge", name="weights")
    indexes = {index["name"]: index for index in graph.indexes()}
    assert set(indexes) == {"person_age", "weights"}
    assert indexes["person_age"]["entries"] == 4
    assert indexes["person_age"]["served"] is True
    assert indexes["weights"]["element"] == "edge"
    names = graph.execute('g.v().has_label("person").has("age", P.gt(30)).values("name")')
    assert sorted(names) == ["josh", "peter"]
    profile = graph.execute('g.v().has_label("person").has("age", P.gt(30)).profile().to_string()')
    assert "[index: vertex person.age]" in profile
    assert graph.drop_index("weights") is True
    assert graph.drop_index("weights") is False
    assert [index["name"] for index in graph.indexes()] == ["person_age"]


def test_compression_estimate_changes_nothing_and_matches_the_rule():
    graph = graphersal.Graph()
    cv = "Senior engineer, led the platform team in Rust. " * 20
    graph.execute('g.add_v("customer").property("cv", text).to_list()', {"text": cv})
    estimate = graph.estimate_compression("customer", "cv")
    assert estimate["values_found"] == 1
    assert len(estimate["candidates"]) == 10
    assert estimate["current"] is None
    best = estimate["candidates"][estimate["best"]]
    assert best["saved_bytes"] > 0
    assert graph.compressions() == []
    one = graph.estimate_compression("customer", "cv", min_bytes=64, dictionary=False)
    assert len(one["candidates"]) == 1
    graph.define_compression("cv", "customer", "cv", min_bytes=64, dictionary=False)
    assert graph.compressions()[0]["stored_bytes"] == one["candidates"][0]["stored_bytes"]
    assert graph.estimate_compression("customer", "cv")["current"]["name"] == "cv"
    unknown = graph.estimate_compression("custmer", "cv")
    assert unknown["warnings"][0]["code"] == "unknown_label"


def test_the_memory_report_shows_where_the_memory_goes():
    graph = graphersal.Graph()
    for i in range(30):
        graph.execute(
            'g.add_v("doc").property("title", t).property("body", b).to_list()',
            {"t": f"t{i}", "b": "lorem ipsum " * (10 + i)},
        )
    report = graph.memory_report()
    body = next(r for r in report["properties"] if r["path"] == "$.body")
    assert (body["element"], body["label"], body["elements"]) == ("vertex", "doc", 30)
    assert body["value_types"] == [{"type": "string", "count": 30}]
    assert body["strings"]["inline_values"] == 0
    assert body["bytes"] == body["slot_bytes"] + body["heap_bytes"]
    assert report["properties"][0]["path"] == "$.body"  # the most bytes first
    assert report["structure"]["totals"]["property_bytes"] == report["property_bytes"]
    assert report["sampled"] is False
    small = graph.memory_report(max_rows=1, max_elements=10)
    assert len(small["properties"]) == 1 and small["rows_omitted"] == 1
    assert small["sampled"] is True


def test_a_dedup_rule_shares_repeated_values_and_the_estimate_matches():
    graph = graphersal.Graph()
    for i in range(12):
        graph.execute(
            'g.add_v("symbol").property("path", p).to_list()',
            {"p": f"crates/graphersal/src/graph/file_{i % 3}.rs"},
        )
    estimate = graph.estimate_compression("symbol", "path", codec=["lz4", "dedup"])
    best = estimate["candidates"][estimate["best"]]
    assert best["codec"] == "dedup"
    assert best["distinct_values"] == 3
    graph.define_compression("path", "symbol", "path", codec="dedup", min_bytes=best["min_bytes"])
    rule = graph.compressions()[0]
    assert rule["codec"] == "dedup"
    assert rule["dictionary"] is False
    assert rule["distinct_values"] == 3
    assert rule["stored_bytes"] == best["stored_bytes"]
    assert sorted(set(graph.execute('g.v().values("path").to_list()'))) == [
        f"crates/graphersal/src/graph/file_{k}.rs" for k in range(3)
    ]
    with pytest.raises(graphersal.GraphersalError):
        graph.define_compression("bad", "symbol", "doc", codec="dedup", dictionary=True)
