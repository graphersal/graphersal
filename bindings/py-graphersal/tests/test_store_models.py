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
