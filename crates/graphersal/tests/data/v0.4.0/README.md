# Golden fixtures of Graphersal 0.4.0

Stores written by the RELEASED Graphersal 0.4.0 binary (store format version 3; embedding
models kept as files under `models/<id>/` with the manifest `graphersal-embed-model.json`), for
the migration to store format version 4 (`tests/all/store_upgrade_tests.rs`, module `v040`):
each model becomes a blob with a catalog definition. Every build must keep upgrading them
(book: Persistence > Store format versions). The tests copy them first: these files never
change.

The model is a TINY synthetic Model2Vec model (a word-level tokenizer of four tokens, an f32
matrix of 4 x 3, the same files as `tests/all/model_fixture.rs` writes), imported twice: as
`acme/tiny` (identity `model2vec/acme/tiny@70c61d0ed332`) and, with other weights, WITHOUT a
repository name (identity `model2vec@b6b362e7a651`).

| Fixture | What it holds |
|---|---|
| `store/` | A directory store: both models; vertices `doc` (3 with a `text`, embedded into `emb` with `acme/tiny`; one with only a `title`), `person` (2, `name` embedded with `hash-ngram/1`), `note` (1, `body` embedded with the model without a repository); the schema (mode `open`) records each `embedder`, never a source property; commit 11 in the merged checkpoint `base`, the mark `last` (11). |
| `backup/` | A full backup directory of `store/` (marker at commit 11), with the models. |
| `backup.zip` | A ZIP backup of `store/`, with the models. |
| `snapshot.gsnap` | A packed snapshot of `store/` with the model rows (segment kind 3). |
| `single.gstore` | A single-file store: `acme/tiny` and the `doc`/`person` vectors, all in the WAL (no checkpoint: commits 1 to 10). |
| `missing/` | A store whose schema names `model2vec/acme/gone@0123456789ab` (`doc.emb`), a model it does not keep (0.4.0 served such models through `--embedder`). |

They were produced by `make.sh` with the binary built from the tag `v0.4.0` (in a separate
checkout: `git worktree add <dir> v0.4.0`, `cargo build -p graphersal-cli` there; needs
`python3` for the model files):

```sh
crates/graphersal/tests/data/v0.4.0/make.sh <dir>/target/debug/graphersal
```

The script lists every command. The lock files (`LOCK`, `BACKUP`) are removed afterwards; they
hold no data. Regenerating changes ids and times; the identities, counts and commits above are
pinned by the tests.
