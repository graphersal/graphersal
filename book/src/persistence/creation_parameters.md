# Creation Parameters

Some parameters of a store are chosen **when it is created** and never change afterwards: they are
written into the store itself (its `GRAPH` file, its container), every copy of the store carries
them, and no API, command or option changes them. Everything else is an **open option**: given to
every open, it may differ from one open to the next and is stored nowhere.

`graphersal store info <dir>` lists the creation parameters under "fixed at creation"; `Store::info`
/ `Store::inspect` return them (`StoreInfo::store_id`, `chunk_bytes`, `damage_policy`, `created_at`,
`format_version`, `space` for a single file), the dev server's `GET /api/store` and the Store menu
show them.

```text
$ graphersal store create data/ --from modern --on-damage continue
Created the store data/: graph 01a11cfc-..., commit 0, 1 snapshot(s); damage policy continue (fixed for its life).
$ graphersal store info data/
...
fixed at creation (never changed):
  store id      01a11cfd-...
  created       2026-10-08T19:27:26Z
  chunk size    1048576 bytes
  on damage     continue (damage found while open is reported, commits continue)
  backend       directory (format version 3)
```

## The parameters fixed at creation

| Parameter | Default | Set with | Stored in | Carried by |
|---|---|---|---|---|
| Damage policy: what an open store does when damage is found on disk while it is open (below) | `maintenance` | Rust `StoreOptions::with_damage_policy` on `Store::create`, `create_with`, `create_from_packed`, `create_file`; CLI `graphersal store create <dir> --on-damage maintenance\|continue`, `graphersal --graph <dir> --create-store --on-damage <p>` (also with `--server`); Python `Store.create(path, on_damage="continue")`, `Store.create_file(...)` | `GRAPH` flag bit 5 ([format spec](format.md) 5.2) | every write of `GRAPH`; fork, attic fork, repair, convert, compact, rollback, every backup (full, increment, ZIP, from memory), restore, upgrade (copy and in place) |
| Chunk target size: the uncompressed size of a snapshot chunk, the unit of repair | 1 MiB | Rust `StoreOptions::with_chunk_bytes`; CLI `store create --chunk-size <bytes>` | `GRAPH` offset 44 (5.1); each manifest | fork, attic fork, repair, convert, backups, restore, upgrade |
| Store id: the store's identity, which an incremental backup requires to be equal | random (128 bits) | (made by the create) | `GRAPH` offset 100 ([format spec](format.md) 5.5) | every write of `GRAPH`; rollback, attic restore, compact, every backup (full, increment, ZIP, from memory), restore (a restored backup IS the store), upgrade (copy and in place: the upgraded store IS the store in the new format). RENEWED (a new store) by fork, attic fork, repair and convert |
| Creation time | the time of the create | (the clock) | `GRAPH` offset 32 | convert, backups, restore, rollback, upgrade (a fork or repair is a new store with its own time) |
| Backend: a directory, one container file (`.gstore`), memory | by the path (`store_dir_at`): an existing file or a new `.gstore` path is a single file | the path; CLI `--single-file`; Rust `SingleFileDir`, `MemDir`; Python `Store.create_file` | the location itself; a single file's preamble holds its container version (14.1) | compact (same file), upgrade in place; `store convert` and `store upgrade <old> <new>` make a NEW store, in any backend |
| Store format version | the build's (3) | (the build) | `GRAPH` offset 8 (the file format version, 2, is in every other file header; a store created from a 0.1.x packed snapshot keeps that snapshot's version 1 files) | everything; a newer version is refused; an older released one (2, Graphersal 0.1.x) is refused until `graphersal store upgrade` converts it, the only change of this parameter, which carries every other one ([Format Versions](store_versions.md#upgrading-a-store-of-an-older-version)); the pre-release version 1 moves through a [packed snapshot](../users_guide/command_cheat_sheet.md#migrate-a-store-from-an-older-format) |

The **graph id** is not a parameter: it identifies the current lineage. It is set when the store is
created, an in-place rollback starts a new one in the same store, and a fork or repair has its own
(its lineage chain names the parent). The **store id** identifies the store itself: a fork,
repair or conversion is another store (a new id), so it never continues the original's
incremental backups ([Backups](store_backups.md#incremental-backups)).

### Damage policy

Damage can appear on disk while a store is open. A backup (it reads every checksum of what it
copies), a checkpoint (its merge reads the last snapshot and the WAL after it) and `verify` find
it; the damage policy decides what happens then:

- **`maintenance`** (the default): the open store turns **read-only** with the damage as the
  reason. Commits, marks, checkpoints, prune, rollback and compaction are refused
  (`PersistError::DamageFound`); queries go on; a backup from memory saves the intact graph; the
  store writes nothing more to the damaged disk.
- **`continue`**: the store keeps accepting commits; the damage stays reported (store info, the
  Store menu, a warning in the log) for as long as it is open.

It does not apply to damage found when the store is **opened**: a store that cannot be loaded
intact always opens in maintenance mode. See [Damage, Maintenance and
Repair](store_damage.md#damage-found-while-the-store-is-open).

Why fixed: the policy is a decision about the data (may it change while its files are known to be
damaged?), not about one process. A fork, a repair or a backup is the same data, so it keeps the
decision; an open never overrides it. An open given another policy ignores it
(`StoreOptions::with_damage_policy` is used only when a store is created), and the CLI refuses
`--on-damage` for an existing store with another policy instead of ignoring it silently. A store
whose `GRAPH` files are both lost cannot tell its policy: a repair of it gets the default.

The storage type of the graph (`Store<TraversalGraph>` or a host's `Store<S>`,
[Your Own Storage](store_own_storage.md)) is NOT a creation parameter: the files do not depend on
it, so any storage type opens any store.

## Open options (per open, stored nowhere)

| Option | Default | Rust (`StoreOptions`) |
|---|---|---|
| Durability: fsync per commit, at most once per interval, or left to the OS | per commit | `with_durability` |
| WAL segment size | 16 MiB | `with_wal_segment_bytes` |
| Automatic checkpoint after this much WAL | 256 MiB (4 MiB in the browser) | `with_checkpoint_after_bytes` |
| Merge checkpoint bound | 1 GiB (64 MiB in the browser) | `with_checkpoint_merge_bytes` |
| Snapshot segment target | 64 MiB | `with_snapshot_segment_bytes` |
| Compression codec of chunks and large WAL records | LZ4 | `with_codec` |
| Checkpoint on close | off | `with_checkpoint_on_close` |
| Read options (memory limit, value depth) | defaults | `with_read_options` |
| Compaction policy and automatic compaction after prune | defaults, off | `with_compaction_policy`, `with_auto_compact` |
| Workload statistics level (`off`, `memory`, `persisted`: the sidecar `WORKLOAD`) | `persisted` | `with_workload_level` |

A new creation parameter is added to the first table (and to `store info`); see `CLAUDE.md`.
