# Blobs

A **blob** is an immutable file of a Store, named by the SHA-256 of its content
(`blobs/<2 hex>/<64 hex>`). A catalog definition references it by a `BlobRef` (facade
`graphersal::blob`): the hash, the content's length and its CRC-32C. Blobs hold large binary
content beside the graph (the files of embedding models, later user `BLOB` values) without
passing it through the write-ahead log. The byte-level rules are the
[format specification, section 4.6](format.md#46-blobs).

```rust
use graphersal::blob::{BlobHashAlgo, BlobRef};

let blob = BlobRef::new(BlobHashAlgo::Sha256, &[0xab; 32], 1024, 7).unwrap();
assert_eq!(blob.relative_path(), format!("ab/{}", "ab".repeat(32)));
```

## Writing and reading

`Store::write_blob(reader)` streams the content into a temporary file while it computes the
SHA-256, the CRC-32C and the length (constant memory), syncs it and renames it to its name. The
same content written twice is stored once. The returned `PendingBlob` protects the new blob
from every collection until you drop it: put its `BlobRef` into a catalog definition's payload,
commit, then drop the handle. A blob that no commit references is an orphan the next prune
removes.

`Store::read_blob(&blob)` returns the content: the length is checked first, the process memory
ceiling before anything is allocated (operation `"blob read"`), the CRC-32C after reading. The
SHA-256 is computed only when content is written or arrives from outside and by `verify`, never
on a read.

In this version a `BlobRef` lives only in catalog payloads: a graph refuses it as an element
property value (`GraphError::InvalidPropertyValue`, kind `"blob reference"`).

## How long a blob stays

A blob stays exactly as long as some retained history references it: a snapshot's blob table, a
commit in the WAL (the derived `ledger` file lists every commit's reference changes), or the
history an attic entry keeps. `prune`, removing an attic entry and a rollback with `delete`
remove every other blob (`PruneReport::blobs_removed`, the journal record `blob.collected`).
`Store::info()` lists the blobs by name and length (`StoreInfo::blobs`: count, bytes, how many no
history references); `graphersal store info` prints a `blobs` line when there are any.

## Copies

Every copy carries the blobs its history references, checked by length and CRC-32C on both
sides: full and incremental backups, ZIP backups (entries `blobs/<2 hex>/<hash>`, checked by
their hash again on restore), backups from memory, forks, attic forks, repairs, conversions and
packed snapshots (`.gsnap`, as rows of segment kind 4). `BlobCopy::Exclude` (`ModelCopy` is its
0.4 name) leaves the models and blobs out of a ZIP backup or a packed snapshot meant to be
shared; the copy's catalog still references them, so the receiver has them missing.

## Damage

A missing or damaged blob is **not** damage of the store: the damage policy does not apply,
nothing turns read-only, and only what needs the blob's bytes fails
(`StoreFailure::BlobUnavailable` or `BlobDamaged`). The system journal gets one `blob.damaged`
error per blob and failure, with a recommendation; `graphersal store verify` lists every missing
or damaged blob. Any copy with the same hash is the same bytes, so two tools bring a store back
to a healthy state:

- **Restore the blobs** from any copy that still has them: a backup directory (full or with its
  increments), a ZIP backup, a packed snapshot (`.gsnap`) or another store (a fork, a repaired
  copy, a `.gstore`). The store must be closed (the writer lock):

  ```text
  graphersal store restore-blobs <dir> --from <backup_dir|file.zip|file.gsnap|other_store> [--json]
  ```

  In Rust `Store::restore_blobs_from(dir, BlobSource::store(..) | BlobSource::zip(..) |
  BlobSource::packed(..))`. Every blob the store's retained history references is checked
  (length, CRC-32C, hash); each missing or damaged one is looked up in the source by its
  algorithm and hash (a blob of an unknown algorithm by its name), streamed into
  `blobs/tmp-*`, checked the same way, synced and renamed into place. A healthy blob is never
  touched. The report (`RestoreBlobsReport`) lists what was restored, what the source does not
  hold and what it holds damaged too; the command exits 1 unless every referenced blob is intact
  afterwards. Each restored blob is a `blob.repaired` record (`how` = `restored`, `source`).
- **Leave them out** when no copy exists: `graphersal store repair <dir> --to <new_dir>
  --drop-damaged-blobs` (`RepairOptions::with_drop_damaged_blobs`) builds a repaired store
  without every catalog definition that references a missing or damaged blob (and without the
  blob): `RepairReport::blobs_dropped` and `definitions_dropped`, one `blob.dropped` record per
  blob in the new store's journal. It is refused, before anything is written, while the schema
  needs such a definition (a model a vector property's `embedder` names); the error names the
  properties: restore the blob first, or change the schema. Without the flag a repair carries the
  definitions and reports their blobs as not carried.

By hand, copying the file `blobs/<2 hex>/<hash>` from another copy works too, and writing the
content again (`Store::write_blob`) replaces a damaged blob.
