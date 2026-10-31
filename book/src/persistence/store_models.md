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

An import is the ONLY way a model serves a database: the step `embed()`, the function
`embed(text)`, text queries, vector rules and `g.embedders()` use the built-in `hash-ngram/1`
and the models of the database, nothing else. (Graphersal 0.4.0 also served models given on the
command line, `--embedder`, or registered by a host; 0.5.0 removed that. A host with a model of
its own computes the vectors itself and writes them as values.)

## Importing a model

```text
graphersal embed-model download minishlab/potion-base-8M          # once, into the download cache
graphersal embed-model import minishlab/potion-base-8M --into data/ --default
graphersal embed-model import minishlab/potion-base-8M --into data/ --replace   # a new revision
```

The download fills the **download cache** (`$GRAPHERSAL_EMBED_MODELS`, else
`%LOCALAPPDATA%\graphersal\embed-models` on Windows, `$XDG_CACHE_HOME/graphersal/embed-models`,
else `~/.cache/graphersal/embed-models`; `--dir` for another one): one download, imported into
any number of databases without the network. The import takes a repository from the cache
first, else the argument as a model directory (`--repo`, `--license` fill what a directory does
not say). `--default` makes the model the default, `--replace` replaces another revision of the
same repository (below). The import writes the model's files (config, tokenizer, weights, model
card, licence files) as one blob and commits one definition.

From Python:

```python
store.import_model("minishlab/potion-base-8M", default=True)          # an open store
graphersal.Store.import_model_into("data", "minishlab/potion-base-8M")  # a closed store
```

`import_model(source, *, repo=None, license=None, replace=False, default=False)` returns the
report as a dict; the wheel has no download client, so the cache is filled by the command line.
In Rust (the cache lookup is `persist::ModelCache`: `ModelCache::new()?.source("minishlab/potion-base-8M")`
gives the `ModelSource`):

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
  the default if the old one was). [Vector rules](../users_guide/vector_rules.md) naming the old
  revision are switched to the new one and their properties re-embedded in the same commit;
  vectors of the old revision without a rule refuse the replace (`ModelInUse`).
- `ModelSource::Files(NewModel)` imports files given as streams (a host without a file system,
  such as the browser).
- A closed store: `Store::import_model_dir` (what the CLI does). See the next section.

## Changing the models of a closed store

`Store::import_model_dir`, `Store::set_default_model_dir` and `Store::remove_model_dir` (CLI
`embed-model import|download --into <dir>`, `store models default <dir> [<repo>|--none]`,
`store models remove <dir> <repo>`; Python `Store.import_model_into`) change the
catalog of a CLOSED store WITHOUT loading its graph: seconds on a store of any size. Each takes
the writer lock (a store open elsewhere is refused with `Locked`: make the change on the open
store, or close it), reads the catalog of the latest position from the newest snapshot's
manifest and the WAL after it (the data changes are read and checked, not applied), and appends
ONE ordinary commit to the WAL, synced (format spec 12.12). The commit is part of the history
like any other: the next open replays it, a rollback goes behind it, `changes_since` and
backups carry it, and it is recorded in the system journal (`catalog.closed_commit`, plus the
`model.*` record with `closed: true`). The same refusals apply as on an open store
(`ModelRevisionExists`, `ModelInUse`, `UnknownModel`, `InvalidModel`); a change that changes
nothing (the model is the default already, the same identity imported again) writes nothing.
The needed-model check of an open does not apply, so an import is also how a missing model
comes back. `ClosedCommitOptions::with_principal` records who made the change (the CLI records
`cli`).

A replace (`ImportModelOptions::with_replace`) stays such a closed commit when no schema
property uses the old revision. When [vector rules](../users_guide/vector_rules.md) name it, the replace re-embeds
their properties in the same commit, which needs the data: then, and only then,
`Store::import_model_dir` OPENS the store (its graph is loaded, the store is write-locked for
the time) and runs the open store's import; `ImportModelReport::reembedded` names the
re-embedded properties and the first warning says that the store was opened (the CLI prints it
as a `note:`). Vectors of the old revision without a rule (no `"from"`) refuse the replace with
`ModelInUse` on both paths.

A damaged store refuses the change: open it (it opens in maintenance mode) and
[repair](store_damage.md) it first. The commit reads the WAL after the newest snapshot; on a
store with a very long WAL tail run `graphersal store checkpoint <dir>` first.

## The default model

`Store::set_default_model(Some(repo))` makes a model the one `embed` uses without a model name
(one commit; `None` clears it; on a closed store `Store::set_default_model_dir`, CLI
`graphersal store models default <dir> <repo>` or `--none`, or `embed-model import --default`).
At most one model is the default. Without one, `embed` uses the built-in `hash-ngram/1`, and the
front ends (CLI, dev server banner, playground, Python) note that the store holds models but none
is the default.

## Removing a model

`Store::remove_model(repo)` (CLI `graphersal store models remove <dir> <repo>`, on a closed
store `Store::remove_model_dir`) commits the removal of the definition. It is refused while the current schema needs the model
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

## A `.gsnap` without a store

A packed snapshot exported from a store (`graphersal store export`, the playground's "Save
graph") carries the blobs of its models. Loaded WITHOUT a store (`graphersal --graph
file.gsnap`, `GraphSource::from_file`, Python `Graph.load`; in Rust
`persist::read_snapshot_file`, which also returns a `SnapshotLoadReport`) the graph serves those
models: `embed()`, `embed(text)`, text queries, vector rules and `g.embedders()` use them as a
store's graph does. The load only notes where each model's blob lies in the file; a model is
loaded at its first use by reading its blob from the file (checked by length, CRC-32C and
SHA-256), so no copy of a blob stays in memory, only the loaded model. The file must therefore
stay as it was while the graph serves its models: a file changed, truncated or removed after the
load makes a model that was not loaded yet unavailable, with an error naming the file (a model
loaded before keeps running). A graph with no packed models has the built-in `hash-ngram/1` only.
Changes stay in memory: the file is never written.

- A `.gsnap` of Graphersal 0.4.0 holds its models as model rows: they are converted as the
  store upgrade converts them (one blob and definition each; a model without a repository
  becomes `local/<12 hex>` and the schema's `embedder` naming it is rewritten in memory), the
  file stays as it is. The load reads the rows once to convert them (briefly holding a model's
  files in memory) and keeps only how to build each model's tar; the tar is built again from the
  rows when the model loads. `graphersal store create <dir> --from file.gsnap` keeps them
  converted (the documented route for a 0.4.0 file).
- A `.gsnap` exported `--without-models` keeps the definitions but not the blobs: the models
  are listed as `missing`, and a query that needs one fails with the import command. In a store
  made from it, `graphersal embed-model import <repo> --into <store>` (or `Store::import_model`)
  of the same model writes its blob back.
- A 0.4.0 `.gsnap` whose schema names a model the file does not carry (0.4.0 served it through
  `--embedder`) loads, but text queries and vector rules on those properties fail until the
  graph is in a store holding the model: `graphersal store create <dir> --from file.gsnap`
  makes the store (and says so with a `warning:`), the import puts the model in, then the store
  opens.
- The CLI prints what the load found as `note:` lines on stderr (`SnapshotLoadReport::notes`,
  the same text in Python as a `UserWarning`, in Rust from the report; `GraphSource::from_file`
  logs them as `tracing` warnings): converted models, models stored but none the default, models
  the schema names that the file lacks, vector properties without a rule, saved queries calling
  a removed function. Their schema patches run in a store (`graphersal store create <dir> --from
  file.gsnap`, then `graphersal --graph <dir> -e '...'`): a schema change of the loaded file
  stays in memory.

## Older Graphersal versions

Graphersal 0.4.0 kept models as `models/<model2vec-12hex>/` directories outside the history,
in stores of store format version 3. This version opens such a store only after `graphersal
store upgrade` (in place or into a copy), which turns each model into a blob and defines all of
them in ONE commit (principal `upgrade`); the history before that commit has no models. No
default model is set. A model kept without a repository name becomes `local/<12 hex>` (import it
again with `--repo` to give it its name). A ZIP backup of 0.4.0 is migrated as it is restored, a
`.gsnap` with model rows when a store is created from it. The upgrade also lists the models the
schema needs that the store lacks, and every vector property with a model but no source property
(`from`): see [0.4.0 -> 0.5.0](store_versions.md#040---050-models-become-blobs).
