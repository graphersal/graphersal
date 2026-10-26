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


def put_model(store_dir, repo="acme/tiny"):
    """Writes the model into `store_dir/models/` as `embed-model download --into` would: the
    files, a licence file, the manifest with sizes and SHA-256. Returns the identity."""
    files = model_files()
    files["LICENSE"] = b"MIT License\n"
    sha = {name: hashlib.sha256(data).hexdigest() for name, data in files.items()}
    text = "model2vec-identity/1\n" + "".join(
        f"{role} {len(files[name])} {sha[name]}\n"
        for role, name in [("config", "config.json"), ("tokenizer", "tokenizer.json"), ("model", "model.safetensors")]
    )
    short = hashlib.sha256(text.encode()).hexdigest()[:12]
    directory = store_dir / "models" / f"model2vec-{short}"
    directory.mkdir(parents=True)
    roles = {"config.json": "config", "tokenizer.json": "tokenizer", "model.safetensors": "model", "LICENSE": "license"}
    for name, data in files.items():
        (directory / name).write_bytes(data)
    manifest = {
        "format": 2, "family": "model2vec", "identity": f"model2vec/{repo}@{short}", "repo": repo,
        "license": "mit", "license_text": "file", "base_models": [], "gated": False,
        "files": [{"name": n, "role": roles[n], "size": len(d), "sha256": sha[n]} for n, d in files.items()],
    }
    (directory / "graphersal-embed-model.json").write_text(json.dumps(manifest))
    return f"model2vec/{repo}@{short}"


def schema_naming(identity):
    return (
        'g.set_schema(#{mode: "open", vertices: #{doc: #{schema: #{type: "object", properties: '
        '#{emb: #{type: "array", format: "vector", dimension: 3, embedder: "' + identity + '"}}}}}})'
    )


def test_the_models_of_a_store_are_listed_exported_and_loaded_at_open(tmp_path):
    path = tmp_path / "data"
    graphersal.Store.create(str(path)).close()
    identity = put_model(path)
    with graphersal.Store.open(str(path)) as store:
        [model] = store.models()
        assert model["identity"] == identity
        assert model["license"] == "mit" and model["license_text"] == "file"
        assert model["used_by_schema"] is False
        assert {f["name"] for f in model["files"]} == {"config.json", "tokenizer.json", "model.safetensors", "LICENSE"}
        store.graph.execute(schema_naming(identity))
        store.checkpoint()
        with_models = store.export(str(tmp_path / "with.gsnap"))
        assert with_models["models"] == [identity]
        without = store.export(str(tmp_path / "without.gsnap"), without_models=True)
        assert without["models"] == []
        assert (tmp_path / "without.gsnap").stat().st_size < (tmp_path / "with.gsnap").stat().st_size
    with graphersal.Store.open(str(path)) as store:
        assert store.models()[0]["used_by_schema"] is True
        found = store.graph.query("g.embedders().map(|e| e.model)")
        assert identity in found


def test_an_explicit_embedder_wins_and_a_missing_model_warns(tmp_path):
    path = tmp_path / "data"
    graphersal.Store.create(str(path)).close()
    identity = put_model(path)
    with graphersal.Store.open(str(path)) as store:
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
    # Without the model in the store: a warning naming the command that adds it.
    import shutil

    shutil.rmtree(path / "models")
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        store = graphersal.Store.open(str(path))
        store.close()
    texts = [str(w.message) for w in caught]
    assert any("embed-model download acme/tiny --into" in t for t in texts), texts
