# Packed Snapshots

A packed snapshot is the whole graph in one file or stream, conventionally named `*.gsnap`. It
works over any `std::io::Write` / `Read`, so it runs in WebAssembly and on any stream a host has
(a file, a socket, an upload). There is no journal and no directory: the application decides
when to save. For durability between saves, add a [journal](journal_and_recovery.md) or use
[the Store](store.md).

```rust
use graphersal::prelude::*;
use graphersal::persist::{self, ReadOptions, WriteOptions};
use graphersal::storage::PersistentStorage; // graph_id()

let graph = TraversalGraph::tinkerpop_modern();
let mut bytes = Vec::new();
let info = persist::write_snapshot(&graph, &mut bytes, &WriteOptions::new().with_name("nightly"))
    .unwrap();
assert_eq!(info.vertex_count, 6);

let header = persist::read_snapshot_info(bytes.as_slice()).unwrap();   // the manifest only
assert_eq!(header.name, "nightly");

let loaded = persist::read_snapshot(bytes.as_slice(), &ReadOptions::new()).unwrap();
assert_eq!(loaded.vertex_count(), 6);
assert_eq!(loaded.graph_id(), graph.graph_id());
```

`graph.write_snapshot(w)` and `TraversalGraph::read_snapshot(r)` are the same with default
options.

Your own storage (EXPERIMENTAL): `persist::write_snapshot` takes any storage that implements
`graphersal::storage::PersistentStorage` (for a shared graph pass `&*graph.read()`), and
`persist::read_snapshot_into(reader, &options, MyStorage::default())` loads a snapshot into an
empty one through the trait's load sink. The same graph gives the same bytes in every storage.
A shared `Graph<MyStorage>` built with `Graph::new_persistent` can also be written by the DSL's
`g.export_snapshot(path)`; built with plain `Graph::new` it cannot (`Unsupported`), except a
`Graph<TraversalGraph>`, which is written either way. `Recovery::new_in(MyStorage::default())`
recovers a snapshot plus journals into it.

## What a snapshot holds

- Every vertex (id, label set, properties in insertion order) and edge (id, optional label,
  endpoints, properties); every value type losslessly (`uuid`, nested arrays and objects, NaN
  payloads).
- The stored schema (its mode and declarations).
- The catalog of definitions (saved queries, ...). A definition of a kind this version does not
  know (written by a newer Graphersal) is kept byte for byte and written back unchanged
  ([format spec 6.3, 16](format.md#16-extensibility)).
- The graph's identity and position: the lineage id ([`graph_id`](journal_and_recovery.md#lineage)),
  the commit position (`last_commit_seq`), the time of the last commit, and the auto-id
  sequences, so the next automatic id never repeats one handed out before the snapshot.
- Counts and an estimate of the in-memory size (for a memory check before loading), a name
  and free-form `meta` text (`WriteOptions::with_name`, `with_meta`).

Indexes and statistics are not stored: they are rebuilt during the load.

## Writing

- Writing holds the graph's read access for the whole write, so the snapshot is consistent; its
  position is `graph.last_commit_seq()`.
- Elements are written sorted by id, in compressed, self-contained chunks (1 MiB uncompressed by
  default, `with_chunk_bytes`) grouped into segments (256 MiB by default, `with_segment_bytes`).
  Chunks are LZ4 (`with_codec(Codec::Zstd)` with the `persist-zstd` feature); a chunk below 4 KiB,
  or one compression does not shrink, is stored as is.
- The writer streams: the manifest at the start lists every segment's size and checksum, so a
  first pass encodes every chunk to plan them and keeps only those figures; the second pass
  encodes each chunk again and writes it straight to the writer. Memory: the sorted id index
  (about 24 bytes per element) and one chunk, never the encoded snapshot; the writer need not
  seek (a pipe, a socket, a download). The price is encoding the graph twice.
- A store imports a packed snapshot (`Store::create_from_packed`, `graphersal store create
  --from x.gsnap`) by copying it section by section into its snapshot directory, and exports one
  (`export_packed`) by streaming the files: neither holds the snapshot in memory.

## Reading

- Reading loads vertices, then edges, rebuilds indexes and statistics, and restores the schema,
  the lineage id, the position, the commit time and the auto-id sequences.
- The schema is **not** re-validated: the data was valid when it was committed. Integrity comes
  from CRC-32C checksums on every header, chunk and section. Damage is a
  `PersistError::Corrupt` naming the section and the byte offset, never a panic or a silently
  wrong graph.
- `ReadOptions::with_memory_limit(bytes)` refuses a snapshot whose recorded size estimate does
  not fit, before anything is loaded (`PersistError::MemoryBudget`);
  `with_max_value_depth(n)` bounds value nesting (default 128, like `traversal.max_value_depth`).
- `persist::read_snapshot_info(r)` reads only the manifest: name, meta, identity, position,
  counts. It is how `graphersal store list` lists snapshots without loading them.
- A non-seekable stream is fine: the sections come in the order they are needed.

## From the DSL, the CLI, Python and the playground

```bash
graphersal -e 'g.export_snapshot("m.gsnap")'          # Exported a snapshot to m.gsnap (6 vertices, 6 edges, commit 0)
graphersal --graph m.gsnap -e 'g.v().count().next()'  # 6
graphersal store export data/ data.gsnap              # a Store's latest snapshot as a .gsnap
graphersal store create data2/ --from data.gsnap      # a new Store from a .gsnap
```

- DSL: `g.export_snapshot(path)` (or `g.export_snapshot(..)`) writes one; `GraphSource::file("m.gsnap")`
  loads one. Both need file access ([Permissions](../users_guide/permissions.md)).
- Python: `graph.save(path)` and `graphersal.Graph.load(path)` ([Python](python.md)).
- Playground: **Save as snapshot** on the start page, and a `.gsnap` file loads like any graph
  file; the Store menu downloads any stored snapshot as `.gsnap`.
- Dev server: `GET /api/export?format=gsnap` downloads the current state.

The byte layout is section 8 of the [format specification](format.md#8-the-packed-snapshot): a
packed file holds exactly the files of a Store's snapshot directory, so the Store imports and
exports `.gsnap` files by copying (`Store::create_from_packed`, `store.export_packed`).
