# Release 0.7.1

A patch release of the 0.7 line. It changes no format; stores, snapshots and journals of 0.7.0
are read and written exactly as before. Its API change is additive.

## Added

- **A store conversion can leave out the blobs it cannot find.**
  `ConvertOptions::with_skip_missing_blobs()` makes `Store::convert_with` complete when a blob
  the store references (an embedding model's) is missing from the source store and from the fill
  source: the copy is made without it, never with a placeholder, and the new field
  `ConvertReport::blobs_missing` names each one. The copy is a store whose blob was lost, which
  is not damage: its model is listed as missing, an open is refused while the schema needs the
  model, and `Store::restore_blobs_from` fills the blob later. Everything else is still verified;
  the default stays strict. See [Blobs](../persistence/store_blobs.md#damage).

## Upgrading from 0.7.0

A drop-in replacement: no format change, no upgrade step.
