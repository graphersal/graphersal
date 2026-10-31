# Embedding Models in a Store

A store keeps the embedding models its vectors were made with. A model is **data of the
database**: ONE blob (its files as a deterministic tar, see [Blobs](store_blobs.md)) referenced
by a catalog definition of kind `model` that follows the history like every other definition.
Every position of the store (a commit, a mark, a snapshot, a rollback, a view of the past) has
exactly the models, and the default model, of that moment; every copy of the store carries them.
The normative description is the [format specification](format.md), sections 4.5 and 6.3.

A model is named by its **repository** (`minishlab/potion-base-8M`): that is the name `embed`
and text queries use. Inside, the schema records the model's **identity**
(`model2vec/<repo>@<12 hex>`, a hash of its files), so the vectors stay tied to the exact model
that made them.

## Importing a model

```text
graphersal embed-model download minishlab/potion-base-8M
graphersal embed-model import minishlab/potion-base-8M --into data/
```

The import writes the model's files (config, tokenizer, weights, model card, licence files) as
one blob and commits one definition. In Rust:

```rust,ignore
use graphersal::persist::{ImportModelOptions, ModelSource, Store, StoreOptions};

let store = Store::open("data", StoreOptions::new())?;
let report = store.import_model(
    ModelSource::Dir("models/minishlab/potion-base-8M".into()),
    ImportModelOptions::new().with_default(true),
)?;
println!("{} ({}), commit {:?}", report.model.repo, report.model.identity, report.commit_seq);
```

- The same model again commits nothing (`already_present`).
- Another revision of a repository the store holds is refused (`ModelRevisionExists`) unless
  `with_replace(true)`: then the new revision replaces the old one in ONE commit (and becomes
  the default if the old one was). While vectors of the old revision are in the data, a replace
  needs vector rules, which a later release adds.
- `ModelSource::Files(NewModel)` imports files given as streams (a host without a file system,
  such as the browser).
- A closed store: `Store::import_model_dir` (what the CLI does); it opens the store without the
  needed-model check below, so it is also how a missing model comes back.

## The default model

`Store::set_default_model(Some(repo))` makes a model the one `embed` uses without a model name
(one commit; `None` clears it). At most one model is the default. Without one, `embed` uses the
built-in `hash-ngram/1`, and the CLI notes that the store holds models but none is the default.

## Removing a model

`Store::remove_model(repo)` (CLI `graphersal store models remove <dir> <repo>`) commits the
removal of the definition. It is refused while the current schema needs the model
(`ModelInUse`: re-embed or drop the properties first). Earlier positions keep the model; its
blob is removed by the [prune](store_prune.md) that drops the last retained history referencing
it.

## Loading and memory

A model the CURRENT schema needs (a vector property's `embedder` names it) is loaded when the
store opens; every other model is loaded at the first query that embeds with it. A model is
loaded once per process, shared by every position that uses the same blob, and unloaded when no
position uses it any more. Loaded models count in `g.memory_usage()` as `model_bytes` and fall
under the process [memory ceiling](../users_guide/resource_limits.md): before a load the ceiling
is checked with the size the definition records, so nothing is read when it does not fit.

The store does **not open** (in any mode: writable, `--read-only`, a backup, a view of a past
position) when a model its schema needs is not in its catalog at that position, its blob is
missing or damaged (`ModelUnavailable`; the message names the commands that bring it back), or
it does not fit under the ceiling (`ModelDoesNotFit`, naming the smallest ceiling that fits). A
model nothing needs that is damaged is a journal record (`model.unavailable`); the store works.
A build that cannot run models (the browser playground, a library build without the feature
`model2vec`) opens a store whose schema needs one read-only, without text queries.

## Listing

`Store::models()` (CLI `graphersal store models <dir>`, `store info`) lists the models of the
current position: repository, identity, revision, dimension, licence, size, the memory it takes
loaded, whether it is the default, its state (`loaded`, `not_loaded`, `damaged`, `missing`,
`unsupported`) and the schema properties that need it. `Store::verify` reads every model's blob
and checks it is the tar of its identity.

## The licence travels with the model

The licence the repository states, its licence file (or, without one, the standard MIT or
Apache-2.0 text, marked as a template) and the licences of its base models are part of the
model's tar. `graphersal store info <dir>` names every model with its licence: who hands a store
or a copy of it on redistributes its models.

## What carries the models

Every copy: full and incremental backups, the ZIP backup, fork, attic fork, repair, conversion
and upgrade, a packed snapshot (`.gsnap`; `--without-models` leaves the blobs out, the
definitions stay and the models are then missing in the copy). Prune, checkpoint and compact
keep the blobs the retained history references.

## Older Graphersal versions

Graphersal 0.4.0 kept models as `models/<model2vec-12hex>/` directories outside the history.
This version carries such directories through its copies; the migration into blobs and
definitions comes with the store upgrade of the 0.5.0 release.
