# Embedding Models in a Store

Vectors made by a real embedding model are only useful with that model: a text query
(`nearest("emb", "a question", 5)`) embeds its text with the model the schema recorded for the
property (`model2vec/<repo>@<12 hex digits>`, see
[Which model produced a property](../users_guide/vectors.md#which-model-produced-a-property)).
A Store can keep the models it needs **inside it**, beside the graph, so the store opens with
its model wherever it goes: on another machine, from a backup, after a fork.

## Putting a model into a store

A model enters a store only by an explicit command, never on its own:

```bash
graphersal store create data/
graphersal embed-model download minishlab/potion-base-8M --into data/
graphersal embed-model import ./my-model --into data/ --repo acme/my-model --license mit
```

`download --into` fetches the model from Hugging Face (as `embed-model download` does, plus its
model card, its licence files and its metadata) and puts it into the store instead of the models
directory. `import` puts a local Model2Vec directory (a download, or any directory with
`config.json`, `tokenizer.json` and `model.safetensors`) into the store; `--repo` names its
repository when the directory has no manifest (the repository is part of the model identity),
`--license` its licence. Both load the model once as a check. The store must be closed (a
server holding it open keeps its writer lock).

In a program: `Store::add_model(NewModel::new().with_file(..).with_repo(..).with_license(..))`
on an open store, `Store::add_model_dir` on a closed one.

## Opening a store loads its models

When a store is opened (`graphersal --graph data/`, the dev server, Python `Store.open`), every
model the store keeps is registered as an embedder (under its repository name,
`minishlab/potion-base-8M`; a model **the schema names** under the identity the schema records),
so text queries and `embed_property()` work without `--embedder`:

```bash
graphersal --graph data/ -e 'g.embed_property(#{label: "doc", from: "text", to: "emb", embedder: "minishlab/potion-base-8M"})'
graphersal --graph data/ -e 'g.v().has_label("doc").nearest("emb", "connected data", 3).values("text").to_list()'
```
 An embedder you register yourself for the same model wins (`--embedder
potion=...`, Python `embedders=[...]`): the stored copy is not loaded then. A model the schema
names that neither you nor the store provides is a note on stderr (Python: a `UserWarning`)
naming the command that adds it. The REPL and a one-shot run print one note per model, with its
licence; the dev server lists the models (name, repository, licence, where each comes from) and
these notes in the Embedding models section of its start banner instead
([Dev Server](dev_server.md)):

```text
note: loaded the embedding model model2vec/minishlab/potion-base-8M@fac4fa854e68 from the store data/ as minishlab/potion-base-8M (licence MIT)
note: the schema names the embedding model model2vec/acme/gone@0123456789ab (doc.emb), which is neither registered nor stored in data/: add it with `graphersal embed-model download acme/gone --into data/`, or pass it with --embedder NAME=<model_dir>
```

A program asks for the same with `Store::stored_embedders(&registered)` (features `persist` and
`model2vec`), which returns the registry to use, what it loaded and the notes. Models of other
kinds than Model2Vec are not kept in a store yet. The WebAssembly playground keeps the models of a
store (in its copies and downloads) but cannot run them.

## Listing and removing

```bash
graphersal store models data/
graphersal store models remove data/ model2vec/minishlab/potion-base-8M@fac4fa854e68
```

```text
model2vec/minishlab/potion-base-8M@fac4fa854e68  29.5 MiB  licence mit (standard text, written by Graphersal)  used by the schema
```

`graphersal store info data/` shows the same block; the Store menu of the playground lists them
with their licences, and Python has `store.models()`. A removal is the only way a model leaves a
store; the schema keeps its identity (a later open notes the missing model).

## The licence travels with the model

A model's licence is the model's own. When a model enters a store, its licence comes with it:

- the **model card** (`README.md`), which states the licence and the attribution;
- the repository's **licence and notice files** (`LICENSE*`, `NOTICE*`) when it has them;
- the **licence id** from the repository's metadata (`cardData.license`, an SPDX id such as
  `mit`), else `unknown`, the models it was made from (`base_model`) with their licences, and
  whether the repository is **gated**;
- when the repository has no licence file (none of the Model2Vec models from MinishLab has one:
  their licence is only in the card), the **standard text** of an MIT or Apache-2.0 licence is
  written beside the model as `LICENSE`, marked as a template (`license_text: template`) and
  saying where it comes from;
- when a **base model has another licence** than the model's own (a tokenizer may come from
  it: `minishlab/M2V_multilingual_output` is MIT, its tokenizer comes from the Apache-2.0
  `sentence-transformers/LaBSE`), the standard text of that licence (MIT or Apache-2.0) as
  `LICENSE-BASE-<licence>`, e.g. `LICENSE-BASE-apache-2.0`, naming the base models it covers.

`download` and `import` warn when the licence is unknown or non-commercial, or the repository is
gated: the terms you accepted may restrict passing the model on.

**A store handed on redistributes its models.** A backup, a ZIP backup, a `.gsnap` export, a
fork or a copied store carries every model with its card and licence files, and whoever passes
it on must follow the models' licences. The copies you hand on can leave the models out:

```bash
graphersal store export data/ graph.gsnap --without-models
graphersal store backup data/ data.zip --zip --without-models
```

The reports of `store export`, `store backup` and a fork list the models (and licences) a copy
carries. The playground's Store menu offers each snapshot download with the models and without
them. The receiver of a copy without models adds them again by the identities its schema records.

## What carries the models

| Operation | Models |
|---|---|
| checkpoint, prune, compact, rollback, attic | untouched |
| `store verify` | every file checked against its SHA-256; a damaged model is a problem (exit 1) but never makes an open store read-only |
| backup (full and incremental) | carried, checked in the store and in the copy; a damaged model stops the backup |
| ZIP backup | carried unless `--without-models` |
| `.gsnap` export | carried unless `--without-models`; `store create --from x.gsnap` (and loading it in the playground) puts them back into a store |
| fork, attic fork, convert, upgrade, restore | carried |
| repair | the intact models carried, damaged ones listed as not carried |

## Older Graphersal versions

Models do not change the store format version. Graphersal 0.3.0 opens a store with models and
ignores (and keeps) them, loads a `.gsnap` with models and ignores them, but **refuses a ZIP
backup with models** as written by a newer version: hand it one made with `--without-models`
(details in [format spec 4.5](format.md#45-embedding-models)).
