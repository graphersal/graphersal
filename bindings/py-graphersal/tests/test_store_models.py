"""Embedding models kept in a `graphersal.Store` (format spec 4.5): listed with their licences,
exported with or without them, loaded at open for the models the schema names; an explicit
embedder wins; a model the schema names but nobody provides is a warning."""

import hashlib
import json
import struct
import warnings

import pytest

import graphersal
from graphersal import Model2Vec

VOCAB = ["[UNK]", "graph", "vector", "model"]


def model_files():
    """The three files of a tiny Model2Vec model (dimension 3)."""
    tokenizer = {
        "version": "1.0", "truncation": None, "padding": None, "added_tokens": [],
        "normalizer": {"type": "Lowercase"}, "pre_tokenizer": {"type": "Whitespace"},
        "post_processor": None, "decoder": None,
        "model": {"type": "WordLevel", "vocab": {t: i for i, t in enumerate(VOCAB)}, "unk_token": "[UNK]"},
    }
    data = b"".join(struct.pack("<3f", 1.0 + i, 0.5, -0.25 * i) for i in range(len(VOCAB)))
    header = json.dumps({"embeddings": {"dtype": "F32", "shape": [len(VOCAB), 3], "data_offsets": [0, len(data)]}})
    header += " " * (-len(header) % 8)
    return {
        "config.json": b'{"normalize": true}',
        "tokenizer.json": json.dumps(tokenizer).encode(),
        "model.safetensors": struct.pack("<Q", len(header)) + header.encode() + data,
    }


def model_dir(tmp_path, name="model"):
    """The tiny model as a directory (no manifest: the repository is given at import)."""
    directory = tmp_path / name
    directory.mkdir()
    for file, data in model_files().items():
        (directory / file).write_bytes(data)
    (directory / "LICENSE").write_bytes(b"MIT License\n")
    return directory


def put_model(store, tmp_path, repo="acme/tiny"):
    """Imports the tiny model into the open `store` (one commit). Returns its identity."""
    report = store.import_model(str(model_dir(tmp_path)), repo=repo, license="mit")
    return report["model"]["identity"]


def schema_naming(identity):
    return (
        'g.set_schema(#{mode: "open", vertices: #{doc: #{schema: #{type: "object", properties: '
        '#{emb: #{type: "array", format: "vector", dimension: 3, embedder: "' + identity + '"}}}}}})'
    )


def test_the_models_of_a_store_are_listed_exported_and_loaded_at_open(tmp_path):
    path = tmp_path / "data"
    with graphersal.Store.create(str(path)) as store:
        identity = put_model(store, tmp_path)
        [model] = store.models()
        assert model["identity"] == identity and model["repo"] == "acme/tiny"
        assert model["license"] == "mit" and model["license_text"] == "file"
        assert model["used_by_schema"] is False and model["dimension"] == 3
        store.graph.execute(schema_naming(identity))
        assert store.models()[0]["needed_by"] == ["doc.emb"]
        store.checkpoint()
        with_models = store.export(str(tmp_path / "with.gsnap"))
        assert with_models["models"] == [identity]
        without = store.export(str(tmp_path / "without.gsnap"), without_models=True)
        assert without["models"] == []
        assert (tmp_path / "without.gsnap").stat().st_size < (tmp_path / "with.gsnap").stat().st_size
    with graphersal.Store.open(str(path)) as store:
        assert store.models()[0]["used_by_schema"] is True
        assert store.models()[0]["state"] == "loaded"
        found = store.graph.query("g.embedders().map(|e| e.model)")
        assert identity in found


def test_an_explicit_embedder_wins_and_a_missing_model_refuses_the_open(tmp_path):
    path = tmp_path / "data"
    with graphersal.Store.create(str(path)) as store:
        identity = put_model(store, tmp_path)
        store.graph.execute(schema_naming(identity))
    own_dir = tmp_path / "own"
    own_dir.mkdir()
    for name, data in model_files().items():
        (own_dir / name).write_bytes(data)
    own = Model2Vec("own", str(own_dir), model="acme/tiny")
    assert own.model == identity
    with graphersal.Store.open(str(path), embedders=[own]) as store:
        names = store.graph.query(f'g.embedders().filter(|e| e.model == "{identity}").map(|e| e.name)')
        assert names == ["own"]
    # Without the model's blob: the store does not open, the error names the command.
    import shutil

    shutil.rmtree(path / "blobs")
    with pytest.raises(graphersal.GraphersalError) as raised:
        graphersal.Store.open(str(path))
    assert "embed-model download acme/tiny" in str(raised.value)


def test_upgrade_converts_models(tmp_path):
    """A store of the released 0.4.0 (golden fixture): its models/ become blobs with
    definitions; the report lists the vector properties without a rule and the models the
    schema lacks; the schema gets no source property (nothing is recomputed)."""
    import pathlib
    import shutil

    fixtures = pathlib.Path(__file__).resolve().parents[3] / "crates/graphersal/tests/data/v0.4.0"
    path = tmp_path / "store"
    shutil.copytree(fixtures / "store", path)
    report = graphersal.Store.upgrade(str(path), in_place=True)
    assert (report["from_version"], report["to_version"]) == (3, 4)
    assert report["migration_commit"] == 12
    converted = {m["identity"]: m for m in report["models_converted"]}
    assert set(converted) == {
        "model2vec/acme/tiny@70c61d0ed332",
        "model2vec/local/b6b362e7a651@b6b362e7a651",
    }
    renamed = converted["model2vec/local/b6b362e7a651@b6b362e7a651"]
    assert renamed["former_identity"] == "model2vec@b6b362e7a651"
    assert renamed["repo"] == "local/b6b362e7a651"
    assert report["models_missing"] == []
    vectors = {v["label"]: v for v in report["vectors_without_rule"]}
    assert set(vectors) == {"doc", "person", "note"}
    doc = vectors["doc"]
    assert (doc["element"], doc["property"], doc["model"], doc["elements"], doc["exact"]) == (
        "vertex", "emb", "acme/tiny", 3, True)
    assert '#{from: "<source property>"}' in doc["schema_patch"]
    assert "re-embeds every doc" in doc["note"]
    assert not (path / "models").exists()
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        with graphersal.Store.open(str(path)) as store:
            assert len(store.models()) == 2
            assert "\"from\"" not in str(store.graph.query("g.get_schema().to_json()"))

    missing = tmp_path / "missing"
    shutil.copytree(fixtures / "missing", missing)
    report = graphersal.Store.upgrade(str(missing), in_place=True)
    [needed] = report["models_missing"]
    assert needed["identity"] == "model2vec/acme/gone@0123456789ab"
    assert needed["needed_by"] == ["doc.emb"]
    assert needed["commands"][1] == f"graphersal embed-model import acme/gone --into {missing}"


def test_upgrade_lists_saved_queries_calling_a_removed_function(tmp_path):
    """A 0.4.0 store whose saved queries call embed_property (removed in 0.5.0): the report
    names each with the vector rule replacing the call; the query is not rewritten."""
    import pathlib
    import shutil

    fixtures = pathlib.Path(__file__).resolve().parents[3] / "crates/graphersal/tests/data/v0.4.0"
    path = tmp_path / "store"
    shutil.copytree(fixtures / "saved_queries", path)
    report = graphersal.Store.upgrade(str(path), in_place=True)
    removed = report["saved_queries_using_removed"]
    assert [r["query"] for r in removed] == ["fill", "fill_any"]
    fill = removed[0]
    assert (fill["function"], fill["removed_in"]) == ("embed_property", "0.5.0")
    assert 'from: "text"' in fill["schema_patch"]
    assert fill["schema_patch"] in fill["note"]
    assert removed[1]["schema_patch"] is None
    assert "vector rule" in removed[1]["replacement"]
