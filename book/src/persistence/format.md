# Storage Format Specification (store version 3, file version 3)

This page is the normative specification of Graphersal's storage format, store format version 3
with file format version 3 (15): the packed snapshot (`.gsnap`), the write-ahead log
(journal), the Store directory and its files, and the single-file container (`.gstore`). It is
written so that a third party can read (and write) these files without Graphersal's code: a
backup tool, a converter, a change-data-capture connector that tails the WAL.

The reference implementation is the `persist` module of the `graphersal` crate. Where this text
and the implementation disagree, it is a bug in one of them; please report it.

**Contents**

1. [Scope, conformance and terms](#1-scope-conformance-and-terms)
2. [Conventions](#2-conventions)
3. [Value encoding](#3-value-encoding)
4. [The store: layout and names](#4-the-store-layout-and-names)
5. [The `GRAPH` file](#5-the-graph-file)
6. [The snapshot manifest](#6-the-snapshot-manifest)
7. [Segment files and chunks](#7-segment-files-and-chunks)
8. [The packed snapshot](#8-the-packed-snapshot)
9. [The write-ahead log](#9-the-write-ahead-log)
10. [The `marks` file](#10-the-marks-file)
11. [The `INTENT` and `ATTIC` files](#11-the-intent-and-attic-files)
12. [Operations](#12-operations)
13. [Damage handling](#13-damage-handling)
14. [The single-file container](#14-the-single-file-container)
15. [Versioning](#15-versioning)
16. [Extensibility](#16-extensibility)
17. [Conformance notes](#17-conformance-notes)

## 1. Scope, conformance and terms

The key words MUST, MUST NOT, SHOULD, SHOULD NOT and MAY are to be read as described in RFC 2119.
Byte layouts, numeric constants, magic values, names and the rules that tell a valid file from a
damaged one are normative. Default sizes (chunk, segment, thresholds) are informative unless a
file records them; a reader MUST NOT assume a default where the file states the value.

**Terms**

| Term | Meaning |
|---|---|
| graph | a property graph: vertices (an id, a set of labels, properties) and edges (an id, at most one label, an out and an in vertex, properties), plus an optional schema and a catalog of definitions |
| definition | a named entry of the graph's catalog, stored beside the schema: a saved query or a compression rule today, other kinds later (6.3) |
| commit | one committed unit of change (a transaction); it has a commit sequence number and a time |
| `commit_seq` | the commit sequence number: `u64`, the first commit of a graph is 1, every later commit is the previous one + 1; 0 is the position before any commit |
| position | a `commit_seq`: the state after that commit |
| `graph_id` | a UUID (version 7 when created by the reference implementation) naming a *lineage*: a history of commits. A fork, an in-place rollback and a repair start a new lineage |
| lineage chain | the list of lineages a store descends from, with the commit at which each child branched off |
| snapshot | the whole graph at one position: a manifest (with the catalog of definitions), a schema and segment files |
| chunk | a self-contained, optionally compressed block of elements inside a segment |
| WAL, journal | the write-ahead log: a sequence of records, one per commit and one per mark |
| mark | a name for a position, recorded in the WAL |
| target | a position to recover to: a `commit_seq`, a time, or a mark name |
| store | the set of files of ONE graph, kept below a root (a directory, a single container file, or memory) |
| backup | a store whose `GRAPH` file carries a backup marker (12.6) |
| attic | the part of a store that keeps history an in-place rollback moved aside |
| donor | an intact older copy of damaged data (13) |
| torn tail | the incomplete end of a file left by an interrupted write; distinguished from damage by the rules of 9.6 and 14.4 |

## 2. Conventions

- **Fixed-width integers** are little-endian: `u8`, `u16`, `u32`, `u64` unsigned, `i64` two's
  complement.
- **`uvar`** is an unsigned LEB128 varint (7 bits per byte, least significant group first, high
  bit = more bytes follow). A `uvar` longer than 10 bytes, or one whose value overflows `u64`, is
  invalid. Writers MUST write the shortest encoding.
- **`svar`** is a signed integer as zigzag (`(n << 1) ^ (n >> 63)`) followed by `uvar`.
- **`string`** is `uvar` byte length followed by that many bytes of UTF-8. Invalid UTF-8 is
  invalid.
- **`opt_string`** is `uvar 0` for "none", else `uvar (length + 1)` followed by the UTF-8 bytes.
- **Time** is an `i64` of microseconds since the Unix epoch, UTC. Commit times of one lineage are
  monotonic: a commit's time is `max(now, time of the previous commit)`.
- **UUID** values and every `graph_id` are 16 bytes in RFC 9562 network order (big-endian), so
  that byte order equals the order of the canonical text.
- **Element ids** are strings. "Sorted by id" means the bytewise order of the UTF-8 encoding.
- **Checksums** are CRC-32C (Castagnoli, polynomial `0x1EDC6F41`, reflected, initial value and
  final XOR `0xFFFFFFFF`), stored as `u32`. "The CRC of bytes `a..b`" covers exactly those bytes.
- **Reserved** bytes and fields MUST be written as zero. A reader MUST ignore reserved bytes
  unless a section says otherwise.
- **Bounds.** Every length or count read from a file MUST be checked against the bytes that
  remain before anything is allocated for it; a count that the remaining bytes cannot hold is
  invalid. An uncompressed chunk or WAL body is at most 1 GiB. A declared uncompressed size of
  an LZ4 block above `255 * stored length + 64` is invalid.

### 2.1 Compression codecs

| Codec byte | Codec | Stored bytes |
|---:|---|---|
| 0 | none | the raw bytes; the stored length MUST equal the uncompressed length |
| 1 | LZ4 | one LZ4 *block* (no frame, no size prefix) |
| 2 | zstd | one zstd frame |

Writers compress a block only when it is at least 4096 bytes long and compression makes it
smaller; otherwise they store it with codec 0. Other codec bytes are invalid. A reader that does
not implement codec 2 MUST report such a block as unsupported, never as damage.

## 3. Value encoding

One encoding is shared by segment chunks and WAL records. A value starts with a tag byte:

| Tag | Value | Payload |
|---:|---|---|
| 0 | null | none |
| 1 | `false` | none |
| 2 | `true` | none |
| 3 | `int64` | `svar` |
| 4 | `float64` | 8 bytes, IEEE 754 binary64, little-endian; NaN payloads are preserved |
| 5 | `string` | `string` |
| 6 | `uuid` | 16 bytes, big-endian |
| 7 | array | `uvar n`, then `n` values |
| 8 | object | `uvar n`, then `n` pairs of key and value; a key is a string-table reference (`uvar`) inside a segment chunk and an inline `string` inside a WAL record |
| 9 | `date` | `svar days`: days since 1970-01-01 (file format version 2) |
| 10 | `localdatetime` | `svar micros`: microseconds since 1970-01-01T00:00:00, wall clock, no zone (file format version 2; read only since version 3) |
| 11 | `datetime` | `svar micros` since 1970-01-01T00:00:00Z (the instant), then `svar offset`: the UTC offset in seconds (file format version 2; read only since version 3) |
| 12 | `time` | `uvar micros`: microseconds since midnight (file format version 2; read only since version 3) |
| 13 | `duration` | `svar months`, `svar days`, `svar micros`, each with its own sign (file format version 2; read only since version 3) |
| 14 | `vector` | `u8 element` (0 `f32`, 1 `f16`, 2 `i8`, 3 `bit`), `uvar dimension`, then for `i8` the scale (4 bytes, IEEE 754 binary32, little-endian), then the packed components: `f32` 4 bytes each (binary32, little-endian), `f16` 2 bytes each (binary16, little-endian), `i8` 1 byte each (two's complement), `bit` `ceil(dimension / 8)` bytes in all, the first component in the most significant bit of the first byte (file format version 2) |
| 15 | `localdatetime` | `svar seconds` since 1970-01-01T00:00:00 rounded down (toward minus infinity), then `uvar nanos`: the nanoseconds of that second, `0..10^9`; wall clock, no zone (file format version 3) |
| 16 | `datetime` | `svar seconds` since 1970-01-01T00:00:00Z rounded down and `uvar nanos` as in tag 15 (the instant), then `svar offset`: the UTC offset in seconds (file format version 3) |
| 17 | `datetime` with a zone name | the fields of tag 16, then the IANA zone name (`Europe/Berlin`) as a key: a string-table reference (`uvar`) inside a segment chunk, an inline `string` inside a WAL record (file format version 3) |
| 18 | `time` | `uvar nanos`: nanoseconds since midnight (file format version 3) |
| 19 | `offsettime` | `uvar nanos`: nanoseconds since midnight (the local time of day), then `svar offset`: the UTC offset in seconds (file format version 3) |
| 20 | `duration` | `svar months`, `svar days`, then the time part as `svar seconds` rounded down and `uvar nanos` of that second (`0..10^9`); each part with its own sign (the time part's sign is that of `seconds * 10^9 + nanos`) (file format version 3) |
| 21..254 | reserved | invalid in versions 1 to 3 |
| 255 | absent | only where a field explicitly allows it (the *before* value of a SetProperty mutation, 9.3) |

- The order of object entries (and of property maps, 7.3) is the insertion order and MUST be
  preserved. A repeated key in one object or property map is invalid.
- **Temporal values** use the proleptic Gregorian calendar. Tags 9 to 13 (file format version
  2) have a resolution of one microsecond; tags 15 to 20 (version 3) one nanosecond. A writer
  of version 3 writes ONE form per kind: a `date` as tag 9, a `localdatetime` as tag 15, a
  `datetime` as tag 16 (tag 17 when it keeps a zone name), a `time` as tag 18, an `offsettime`
  as tag 19, a `duration` as tag 20; every engine value has an encoding, so nothing is refused.
  A reader of version 3 reads tags 9 to 13 as well (a version 2 segment's chunks copied into
  a version 3 snapshot keep them). The `svar seconds` + `uvar nanos` pairs hold nanoseconds
  over the years -9999 to 9999 (about ±3.2 * 10^20 ns, more than an `i64`) and a duration's
  whole time part. Their ranges are normative; a value outside its range is invalid:

  | Tag | Valid payload |
  |---:|---|
  | 9 | `days` within the years -9999 to 9999: -4371587 (`-9999-01-01`) to 2932896 (`9999-12-31`) |
  | 10 | `micros` within the same days (`-9999-01-01T00:00:00` to `9999-12-31T23:59:59.999999`) |
  | 11 | `offset` within ±64800 (±18:00); both the instant and the local time (`micros + offset * 10^6`) within the range of tag 10 |
  | 12 | `micros` in `0..86400000000` |
  | 13 | `months` and `days` within `i32`, `micros` within `i64` |
  | 15 | `nanos` below 10^9; `seconds * 10^9 + nanos` within the days of tag 9 (`-9999-01-01T00:00:00` to `9999-12-31T23:59:59.999999999`) |
  | 16, 17 | `nanos` below 10^9; `offset` within ±64800; both the instant and the local time within the range of tag 15 |
  | 18 | `nanos` in `0..86400000000000` |
  | 19 | `nanos` as in tag 18; `offset` within ±64800 |
  | 20 | `months` and `days` within `i32`; `nanos` below 10^9 (`seconds` is an `svar`, so the time part spans `-9223372036854775808` s to `9223372036854775807.999999999` s, the range of `java.time.Duration`) |

  A `datetime` keeps the offset it was written with; equality and order of two `datetime`s use
  the instant only (the offset and the zone name are not compared). The zone name of tag 17 is
  stored as text, never as an index into a build's own table; the offset is stored as written
  and is not recomputed from the name. A name the reader's time zone database does not know (a
  newer database's) is content of a newer writer behind a valid checksum: unsupported content
  (16 rule 9), never damage; the reference reader refuses the value and the operation, as it
  does with every unsupported content; ASCII case is ignored when the name is looked up. An
  `offsettime` orders by its UTC time of day (the local time minus the offset, not wrapped). A
  `duration` is not normalized: `P1M`, `P30D` and `PT720H` are three different values.
- **Vector values** (tag 14): `dimension` is 1 to 65536; a component of an `f32` or `f16`
  vector is finite (no NaN, no infinity; `-0.0` is valid and equals `0.0`); the `i8` scale is
  finite and `>= 0` (`-0.0` is written as `0.0`); the value of an `i8` component is
  `code * scale`; a `bit` component is `1.0` for a set bit and `-1.0` otherwise, and the padding
  bits of the last byte are 0. An unknown element byte, a dimension out of range and any other
  violation are invalid (damage): every element kind of version 2 is defined here. A reader
  checks the payload length (`dimension` times the component size) against the remaining bytes
  before it allocates.
- Tags 9 to 14 are **invalid in a file of format version 1**, tags 15 to 20 **in a file of
  format version 1 or 2** (15): a reader MUST report such a value as damage, never decode it. A reader takes the version from the header of the file the
  value is in: a segment chunk from its segment's header (7.1, also when the chunk is read
  through the chunk index without framing the whole segment), a WAL record from its WAL
  segment's header (9.1), a definition from the manifest (6.1) or WAL segment it is stored in.
- Readers MUST bound the nesting depth of arrays and objects (the reference reader defaults to
  128 levels and is configurable); writers MUST NOT write more than 4096 levels.

A **property map** is `uvar count` followed by `count` pairs of key and value, with keys encoded
as object keys are in the same context.

## 4. The store: layout and names

### 4.1 Layout

A store is a set of named files below a root:

```text
GRAPH                         identity and state (5); written with its copy GRAPH.copy
GRAPH.copy
LOCK                          an empty file; the writer's operating-system lock (12.10)
BACKUP                        an empty file; the backup pin (12.10)
INTENT                        present only while a multi-file operation runs (11.1)
marks                         the marks, derived from the WAL (10)
WORKLOAD                      the workload statistics sidecar (4.4); optional, instance data
snapshots/
    00000000000000052817/     one snapshot; the name is its commit_seq, 20 decimal digits
        manifest              6
        schema.json           the schema, canonical compact JSON
        v-000000.seg          vertex segments (7), six decimal digits per kind, from 0
        e-000000.seg          edge segments
    00000000000000052000/     a holder: the directory of a pruned snapshot without its manifest,
        e-000002.seg          keeping only files newer snapshots reference (6.2, 12.4)
    tmp-<32 hex digits>/      a snapshot being written (never listed, removed on open)
wal/
    00000000000000052818.wal  a WAL segment; the name is the first commit_seq it may hold (9)
attic/
    20261007T182205Z-00000000000000052001/   one rolled-back history (11.6, 12.5)
```

`LOCK` and `BACKUP` exist only for directories on a file system; other backends provide the same
locks without files (4.3). A snapshot directory holds exactly the files its manifest lists. A
directory under `snapshots/` named by a commit but without a `manifest` file is a **holder**: it
is no snapshot (it is not listed, loaded or counted) and only keeps segment files that newer
snapshots reference (6.2); a prune removes it when no snapshot uses them any more (12.4).

### 4.2 Names

Every name a store file records (segment paths in a manifest, paths in `INTENT`, names in
`ATTIC`) is **relative to the store root**, `/`-separated, and contains no empty, `.` or `..`
part, no `\`, no `:` and no NUL. No store file holds an absolute path, an operating-system path,
a device, or the target of a symbolic link. A store is therefore relocatable: moved or copied as
a whole, it opens unchanged. A reader MUST refuse a recorded name that breaks these rules.

### 4.3 Backends

The format does not depend on where the named files live. The reference implementation provides
three backends behind one directory abstraction:

- a **directory** on a file system (names map to files below the root; parts of the tree MAY be
  symbolic links to other file systems, which are never resolved or recorded);
- a **single container file** (14);
- **memory** (no durability; locks are in-process flags).

A backend MUST provide: reading a file or a byte range of it; appending to a file (an append MAY
tear on a crash); replacing a whole file atomically (`GRAPH`, `GRAPH.copy`, `marks` rewrites,
`INTENT`, `ATTIC`); an atomic rename; removal of a file or a directory tree; a durability barrier
(`fsync` of a file and of a directory); an exclusive writer lock; and a shared/exclusive backup
pin. Files are only ever appended to, cut back, or replaced whole; lengths are logical bytes.

On a file system, a move between two parts of the tree that live on different file systems is
done as: copy into `tmp-xdev-<name>` in the target directory, sync, rename there, sync the
directory, remove the source, sync its directory. Leftover `tmp-xdev-*` names MUST be ignored by
every listing.

### 4.4 The workload statistics sidecar

`WORKLOAD` holds the instance's workload statistics (book page "Workload Statistics"): data of
the running instance, not of the graph. It is never in the WAL, never in a snapshot, and never
copied by a backup, a ZIP backup, a fork, a repair or a conversion; a restored or copied store
starts without it. A writable store replaces it whole (`write_atomic`) periodically and at close.

```text
magic "GRWLSTAT" | version u32 = 1 | series count u32
per series: byte length u32 | kind u8 (1 job, 2 lock, 3 query) | name (u32 length + UTF-8)
            | channel u8 (0 host, 1 playground, 2 mcp, 3 cli, 4 python, 5 job)
            | tier count u8 | per tier: tier u8 (0 minutes, 1 hours, 2 days) | bucket count u16
            | per bucket: index i64 (the bucket's start in minutes/hours/days since the Unix
              epoch, UTC) | count u64 | failed u64 | items u64 | total microseconds u64
              | max microseconds u64
job count u32 | per job: name | started_at i64 (microseconds) | microseconds u64
            | error flag u8 [+ error text]
CRC-32C u32 of everything before it
```

All integers little-endian. A reader MUST treat the file as optional: a missing file, a wrong
magic, length or checksum, or a version it does not know means "no statistics" (it starts anew
and replaces the file at its next write), never damage, never a reason to refuse the store. A
series of a kind or channel it does not know is skipped by its byte length, a tier it does not
know by its bucket count; a reader keeps at most 60 minute, 48 hour and 90 day buckets. The magic is none of
a segment's or a WAL segment's, so the version guard (15) ignores the file.

## 5. The `GRAPH` file

### 5.1 Layout

A fixed part of 256 bytes, followed by the lineage chain:

| Offset | Size | Field |
|---:|---:|---|
| 0 | 8 | magic `GRSLGRPH` |
| 8 | 4 | store format version (`u32`, 3; see 15) |
| 12 | 4 | state: 0 = closed cleanly, 1 = open (a writer has it, or it was not closed cleanly); another value in an intact file (both checksums valid) is a newer writer's: the store is refused as unsupported, never damage (16 rule 9) |
| 16 | 16 | `graph_id` of the current lineage |
| 32 | 8 | created at (time) |
| 40 | 4 | lineage entry count `n` (at least 1) |
| 44 | 4 | chunk target size in bytes, fixed when the store is created |
| 48 | 4 | flags (5.2) |
| 52 | 8 | WAL cut: segment (its first `commit_seq`); zero unless flag bit 1 |
| 60 | 8 | WAL cut: byte offset in that segment; zero unless flag bit 1 |
| 68 | 8 | backup marker: the last commit the backup holds; zero unless flag bit 2 |
| 76 | 8 | backup marker: taken at (time); zero unless flag bit 2 |
| 84 | 8 | `wal_head`: the first `commit_seq` (the name) of the newest WAL segment; zero unless flag bit 3 |
| 92 | 8 | `closed_at`: the last commit at a clean close; zero unless flag bit 4 |
| 100 | 16 | `store_id`: the store's identity, random, never zero (5.5) |
| 116 | 136 | reserved, zero |
| 252 | 4 | CRC-32C of bytes 0..252 |
| 256 | 32 × n | lineage entries, newest first: `graph_id` (16), branched at `commit_seq` (`u64`), at time (`i64`) |
| 256 + 32n | 4 | CRC-32C of the lineage entries |

The file's length MUST be exactly `256 + 32n + 4`. The first lineage entry's `graph_id` MUST
equal the one at offset 16. The root entry (the last) has `branched at` 0. A `store_id` of zero
is invalid (the file is damaged).

### 5.2 Flags

| Bit | Meaning |
|---:|---|
| 0 | reserved for parity data; MUST be 0 in store format versions 2 and 3 |
| 1 | a WAL cut is recorded (offsets 52, 60; 12.2) |
| 2 | the store is a backup (offsets 68, 76; 12.6) |
| 3 | `wal_head` is recorded (offset 84; 9.8) |
| 4 | `closed_at` is recorded (offset 92; 9.8) |
| 5 | the damage policy is `continue` (13.3); clear: `maintenance`, the default |
| 6..31 | reserved; a writer MUST preserve unknown bits it read |

Bit 5 is a **creation parameter** like the chunk target size (offset 44): it is set when the store
is created and never changed; every writer of `GRAPH` keeps it, and every copy of the store
(fork, repair, conversion, backup, restore, a backup from memory) carries it. A reader that does
not know it treats the store as `maintenance`, the safe default. A repair that finds both `GRAPH`
copies unusable cannot read it and writes the default.

A `GRAPH` file without bits 3 and 4 (written before they existed) means "unknown": the checks
of 9.8 are skipped.

### 5.3 Two copies

`GRAPH` is written as two files, `GRAPH.copy` first and then `GRAPH`, each through a temporary
name (`<name>.tmp`), `fsync` and an atomic rename; then the directory is synced. A reader uses:
both valid and equal, either; both valid and different, the one with the longer lineage chain,
on a tie `GRAPH.copy` (it was written first, so it is the newer); one valid, that one. A
read-write open that found the copies different or one of them invalid rewrites both (and
reports it). Both invalid is damage (13).

### 5.4 The lineage chain

The chain records every lineage the store descends from. A fork's chain is
`[new id, branched at the target commit, time]` followed by the parent's chain; an in-place
rollback adds such an entry in the same store; a repair starts a new lineage whose chain
continues the damaged store's. An ancestor lineage is valid only up to the `branched at` of its
child entry: a WAL record of an ancestor beyond that commit does not belong to this store's
history. A file (a WAL segment, a snapshot manifest) whose `graph_id` is not on the chain MUST
NOT be replayed.

### 5.5 The store identity

`store_id` (offset 100; new in store format version 2) identifies the STORE, where `graph_id`
identifies a lineage of its data. It is a creation parameter: a create writes a new random one
(128 bits, like a lineage id), and every writer of `GRAPH` keeps it. A fork, an attic fork (12.5), a repair (13.4) and a conversion
to another backend are new stores with a new `store_id` (their lineage chain still names the
parent); an in-place rollback, an attic restore, a compaction, a restored backup (12.6, 12.8)
and an upgrade (15.1) keep it: a restore IS the store, an upgrade is the same store in a new version. Every backup (full, increment, ZIP, from memory) records its
source's `store_id`, and an increment requires it to be equal (12.7).

## 6. The snapshot manifest

`manifest = fixed part (4096) | variable part | CRC-32C of the variable part (u32) | copy of the fixed part (4096)`.

### 6.1 Fixed part

| Offset | Size | Field |
|---:|---:|---|
| 0 | 8 | magic `GRSLMANI` |
| 8 | 4 | format version |
| 12 | 4 | flags (bit 0: incremental, the segment table references files of older snapshots; other bits reserved) |
| 16 | 16 | `graph_id` |
| 32 | 16 | parent `graph_id` (zero if none) |
| 48 | 8 | parent `commit_seq` |
| 56 | 8 | `commit_seq`: every commit up to and including it is contained |
| 64 | 8 | created at (time) |
| 72 | 8 | last commit at: the time of the commit at `commit_seq` |
| 80 | 8 | `node_sequence`: the vertex auto-id sequence |
| 88 | 8 | `edge_sequence`: the edge auto-id sequence |
| 96 | 8 | vertex count |
| 104 | 8 | edge count |
| 112 | 8 | vertex property count |
| 120 | 8 | edge property count |
| 128 | 8 | estimated in-memory bytes of the loaded graph (lets a loader refuse a snapshot that does not fit a memory budget before reading it) |
| 136 | 8 | `schema.json` length |
| 144 | 4 | `schema.json` CRC-32C |
| 148 | 4 | segment count |
| 152 | 8 | offset of the variable part: 4096 |
| 160 | 8 | length of the variable part: from offset 4096 up to, not including, its CRC |
| 168 | 2 | name length (bytes, at most 255) |
| 170 | 255 | name: UTF-8, `name length` bytes followed by zeros (optional, human-readable) |
| 425 | 3 | reserved, zero |
| 428 | 4 | chunk target size in bytes |
| 432 | 3660 | reserved, zero |
| 4092 | 4 | CRC-32C of bytes 0..4092 |

Listing the snapshots of a store reads only the first 4096 bytes of each manifest. A reader uses
the head copy and falls back to the tail copy when the head's CRC fails.

### 6.2 Variable part

In this order:

1. **meta**: `string` (empty = none). Free-form text the format never interprets.
2. **Segment table**: `segment count` rows of `kind u8` (1 vertex, 2 edge, other kinds 7.5;
   0 is invalid) | `path string` | `first id string` | `last id string` | `element count u64` |
   `byte length u64` | `CRC-32C u32` of the whole segment file. Vertex rows precede edge rows,
   rows of other kinds follow the edge rows; vertex and edge rows of one kind are ordered by id
   range and their ranges never overlap. A path is `v-NNNNNN.seg` or `e-NNNNNN.seg` in the
   snapshot's own directory, or, in an incremental snapshot (flag bit 0), a **reference**: a
   name relative to `snapshots/` such as `00000000000000001000/v-000003.seg`, exactly two parts,
   a 20-digit snapshot name and a file name (see *References* below).
3. **Definitions**: the definitions table (6.3).
4. **Chunk index**: `uvar` row count, then rows of `segment number u32` (its position in the
   segment table; it MUST name a vertex or edge row) | `chunk offset u64` | `first id string` |
   `last id string` | `chunk CRC u32` (the value of 7.2), grouped by segment in table order.
   With it the id range of a damaged chunk is known even when the segment's own footer is
   damaged.

`schema.json` holds the stored schema as canonical compact JSON (an empty schema of mode `none`
when the graph stores none); its length and CRC MUST match the manifest.

**References (incremental snapshots).** A reference names a vertex or edge segment file of an
OLDER snapshot of the same store, unchanged; its row (kind, ids, count, length, CRC) and its chunk
index rows are those of the referenced file. Rules:

- A manifest with a reference MUST have flag bit 0 set; a reference in a manifest without it, a
  reference that is not `<20 digits>/<file name>`, and a row of another kind than 1 or 2 with a
  `/` are invalid.
- Every segment a manifest lists, a referenced one too, carries the manifest's `graph_id` in its
  header (7.1): a snapshot of a new lineage (after an in-place rollback) references no file of
  an older lineage.
- A reference is resolved in the directory that holds the snapshot: `snapshots/` of the store,
  or of an attic entry (11.2) for a snapshot moved there; a referenced file not found next to a
  snapshot of an attic entry is resolved in the store's own `snapshots/`.
- The referenced file may lie in a snapshot directory or in a holder (4.1). Reading, verifying
  and loading an incremental snapshot check the referenced files exactly like its own.
- A writer MUST NOT reference a file that the store's newest snapshot uses (the donor rule of
  12.3): a new snapshot references only *twins* of the newest snapshot's files, held by the
  snapshot before it.
- A packed snapshot (8) is self-contained and has no references: writing one from an incremental
  snapshot names each segment `v-NNNNNN.seg`/`e-NNNNNN.seg` by its position per kind and clears
  flag bit 0 (the manifest is re-encoded; the segment sections are the files' bytes).

### 6.3 The definitions table

The graph's catalog of definitions: named, typed entries stored in the database beside the
schema (saved queries and compression rules today). One self-describing table serves every kind,
present and future,
so that a new kind needs no new layout and no new version (16).

```text
definitions = uvar count | definition*
definition  = kind u8 | flags u8 | name string | length uvar | payload (length bytes)
flags       = bit 0: critical (a reader that does not know `kind` MUST NOT write the store, 16);
              bits 1-7 reserved: written as zero, preserved as read
payload     = property map (3) with inline keys (as in a WAL record)
```

- The rows are ordered by `kind`, then by `name` bytewise, strictly increasing: a kind and name
  appear at most once. A table out of this order is invalid.
- `length` MUST NOT exceed the bytes that remain; the payload is exactly `length` bytes.
- The payload of every kind, known or not, is a property map. Its keys MUST NOT be `kind`,
  `name` or `flags`: these are the envelope keys of the exchange form of a definition, a JSON
  object that puts the payload keys beside them. A writer never writes such a key; a reader
  that decodes the payload treats one as invalid.
- A reader that knows the kind decodes the payload. A payload that is not a valid property map
  (truncated or invalid bytes, trailing bytes, a repeated or reserved key) is damage (13; a
  repair takes the catalog from a donor, 13.4). A valid map is NEVER damage (16 rule 9): keys
  the reader does not know are ignored and kept (16 rule 1); an unknown VALUE of a known key it
  can represent (a compression `codec`, `element` or `path`, an index `element`, `type` or
  `transform`, a saved query's parameter schema) keeps the definition, not served, its payload
  written back exactly as read; a map it cannot read as its kind at all (a missing or mistyped
  known key) keeps the definition as opaque bytes like an unknown kind (16 rules 2 and 3).
- A reader that does not know the kind keeps the definition as opaque bytes (kind, flags, name,
  payload) and writes it back unchanged (16 rules 2 and 3).
- A load does not validate a definition beyond this (as it does not validate data against the
  schema, 12.1); a saved query whose source no longer compiles is loaded and fails when called.

| Kind | Definition |
|---:|---|
| 0 | invalid |
| 1 | saved query |
| 2 | property index |
| 3 | compression rule |
| 4..127 | unassigned (assigned only by this format) |
| 128..255 | host kinds: reserved for applications built on the library, never assigned by this format (16 rule 8) |

A **saved query** (kind 1) is one Rhai function (or one GQL query) stored as text and called by
name; its name is
an identifier (`[A-Za-z_][A-Za-z0-9_]*`, at most 128 bytes). Its payload keys, written in this
order, followed by the keys the writer does not know in the order it read them:

| Key | Value |
|---|---|
| `folder` | string: a `/`-separated folder path; absent = the root |
| `description` | string; absent = none |
| `dialect` | string: the language and content version of `source`: `"graphersal-rhai/1"` (a Rhai function that reads), `"graphersal-rhai/2"` (a Rhai function that may write), `"graphersal-gql/1"` (a GQL query that reads), `"graphersal-gql/2"` (a GQL query that writes); a reader that does not run a dialect keeps the query unchanged and lists it as not runnable |
| `source` | string: the Rhai function text, or the GQL query text |
| `params` | string: canonical compact JSON of an object, parameter name -> `{"schema": <schema node>, "default": <value>}` (`default` absent when the parameter has none), in declaration order |
| `meta` | string, free, never interpreted; absent = none |

`source` is required; every other key is optional. The text is stored, never a compiled plan:
every call plans it anew. A `params` value the reader cannot read (a schema keyword or a
parameter form of a newer writer) keeps the query: it is listed with the reason, written back
exactly as read and refused when called (since 0.2.1; the exchange form a user imports stays
strict).

A **compression rule** (kind 3, not critical) keeps the strings of one element kind and label at
one property path compressed in memory (book page "Compressed Properties"). Its name is an
identifier as for a saved query. The data in snapshots and the WAL is ALWAYS plain: the rule
describes only the in-memory representation, so a reader that does not know the kind loses
nothing but the compression, and a load compresses the rule paths again. Its payload keys, in this
order, followed by the unknown keys:

| Key | Value |
|---|---|
| `element` | string: `"vertex"` or `"edge"` |
| `label` | string: the label |
| `path` | string: canonical jpath text of object keys only (`$.details.career.cv`) |
| `codec` | string: `"lz4"` (each value compressed) or `"dedup"` (each distinct value held once in memory; since 0.2.1) |
| `min_bytes` | int64: strings shorter than this (UTF-8 bytes) stay plain, 1 to 16777216 |
| `dictionary` | boolean: whether the load builds a dictionary from the rule's values (`lz4` only; `true` with `"dedup"` is invalid) |

`label` and `path` are required; `element` defaults to `"vertex"`, `codec` to `"lz4"`,
`min_bytes` to 256 (`lz4`) or 25 (`dedup`), `dictionary` to `true` (`lz4`) or `false` (`dedup`);
writers always write `min_bytes` and `dictionary`. A `codec` or an `element` the reader does not
know, or a `path` it cannot parse (a newer writer's), is NOT damage and needs no new format
version (16 rules 1 and 9): the reader keeps the definition, writes its payload back exactly as
read (every key, in the order read), does not apply it (the values stay plain in memory as on
disk) and lists it as not applied with the reason; a front end never creates such a rule. The
codec changes no stored data, which stays plain. Two rules with the same `element`, `label` and
`path` (compared as read) are invalid (a writer refuses the second).

> **0.2.0** decodes an unknown `codec` as an invalid payload, that is as damage: it reads a store
> holding a `dedup` rule (0.2.1) as damaged from that commit on and proposes a repair that would
> drop commits. Open such a store with 0.2.1 or later only; 0.2.1 and later follow the rule above.

A **property index** (kind 2, not critical) orders the elements of one element kind and label by
the value of one top-level property (book page "Property Indexes"). Its name is an identifier as
for a saved query. Only the declaration is stored: snapshots and the WAL hold NO index entries, a
load builds them from the data, so a reader that does not know the kind loses nothing but the
speed-up. Its payload keys, in this order, followed by the unknown keys:

| Key | Value |
|---|---|
| `element` | string: `"vertex"` or `"edge"` |
| `label` | string: the label |
| `key` | string: the top-level property name (not a jpath) |
| `type` | string: the index type, `"memory-ordered"` (an ordered index in memory whose entries a load builds from the data) |
| `transform` | string, optional: the transform of the index keys, `"to_lower"` (the lowered text of string values, as the step `to_lower()` computes it; other values are not in the index); absent for an index of the values themselves |

`label` and `key` are required; `element` defaults to `"vertex"`, `type` to `"memory-ordered"`
(a payload written before the key existed has none), `transform` to none. A writer of this
version always writes `type` and writes `transform` only for an index with a transform. Two
indexes with the same `element`, `label`, `key`, `type` and `transform` are invalid (a writer
refuses the second); indexes of different types or transforms may cover the same key. A `type`,
a `transform` or (since 0.2.1) an `element` the reader does not know (a newer writer's) is NOT
damage and needs no new format version (16 rules 1 and 9): the reader keeps the definition,
writes its payload back exactly as read (every key, in the order read), builds no entries for it
and serves no query from it. A store written before this kind was decoded never holds it (kind 2
was reserved), so no format version changes.

## 7. Segment files and chunks

`segment = header (64) | chunk* | footer`. A segment holds elements of one kind, in strictly
increasing id order across its chunks.

### 7.1 Header

| Offset | Size | Field |
|---:|---:|---|
| 0 | 8 | magic `GRSLSEGM` |
| 8 | 4 | format version |
| 12 | 1 | kind: 1 vertex, 2 edge |
| 13 | 1 | default codec (informative) |
| 14 | 2 | zero |
| 16 | 16 | `graph_id` |
| 32 | 4 | chunk count |
| 36 | 8 | footer offset |
| 44 | 16 | reserved, zero |
| 60 | 4 | CRC-32C of bytes 0..60 |

### 7.2 Chunks

`chunk = uncompressed length u32 | stored length u32 | codec u8 | element count u32 | chunk CRC u32 | stored bytes`.

The chunk CRC covers the first 13 header bytes (both lengths, the codec, the element count)
followed by the stored bytes, so a damaged length or codec is detected too. Chunks start right
after the header and end exactly at the footer offset. The uncompressed payload (2.1) is the
chunk payload (7.3), which its elements fill exactly. A chunk's uncompressed size is close to
the store's chunk target size (default 1 MiB); the chunk is the unit of repair (13.4).

### 7.3 Chunk payload

```text
payload    = string table | element*            (element count elements)
string tbl = uvar n | string*n                  labels and property keys of this chunk
vertex     = id string | uvar label count | uvar label ref * | property map
edge       = id string | label uvar (0 = none, else ref + 1) | out id string | in id string | property map
```

A chunk is self-contained: its own string table holds every label and property key it uses
(string *values* stay inline), so any chunk decodes alone. References are indexes into that
table; a reference outside it is invalid. A vertex's labels are a set (a repeated label is
invalid); its first label is its primary label. In a property map and in object values inside a
chunk, keys are table references. An edge with an empty property map and one without properties
are the same (count 0).

### 7.4 Footer

`footer = (chunk offset u64 | first id string) per chunk | CRC-32C u32 of the footer bytes before it`.

The footer allows a binary search by id without reading every chunk.

### 7.5 Files of other segment kinds

Large data of a future feature (an index, statistics) lives in files of its own, not in the
manifest (16 rule 5): a row of the segment table with another kind than 1 and 2 names such a
file. Its path is a plain file name in the snapshot's own directory (4.2), its byte length and
CRC-32C are those of the whole file; `first id`, `last id` and `element count` belong to the
kind (a reader that does not know it ignores them). The file's content belongs to the kind; no
chunk index row names it.

- Kinds **3..127** are **ignorable**: a reader that does not know the kind checks the file's
  length and CRC like any segment (a mismatch is damage), copies it wherever the snapshot is
  copied file by file (backup, import and export of the packed form, 8) and otherwise ignores
  it. Such a file holds data derived from the snapshot; a writer that does not know the kind
  does not carry it into a new snapshot (a checkpoint, fork or repair), and a writer that knows
  it rebuilds it.
- Kinds **128..255** are **critical**: a store whose base snapshot has such a row opens
  read-only for a reader that does not know the kind (16 rule 3).

## 8. The packed snapshot

A packed snapshot (`.gsnap`) is a whole snapshot in one stream:

```text
packed  = magic "GRSLPACK" | format version u32 | section count u32 | section* | trailer
section = kind u8 | length u64 | bytes
trailer = CRC-32C u32 of every byte before it, from the magic on
```

| Section kind | Content |
|---:|---|
| 1 | the manifest (6) |
| 2 | `schema.json` |
| 3 | a vertex segment (7) |
| 4 | an edge segment |
| 5 | a named extra file: `name string` followed by the file's bytes; the file of a segment-table row of another kind (7.5), `name` = the row's path |
| 0, 6..255 | invalid |

The sections appear in this order: the manifest, `schema.json`, the vertex segments, the edge
segments, the named extra files, each in the manifest's table order; the section count is 2 + the
manifest's segment count, and each section's length MUST equal what the manifest records for it
(for kind 5: the length of `name` as a `string` plus the row's byte length; the file's bytes MUST
match the row's length and CRC). A new file type therefore needs no new section kind. The manifest and
`schema.json` sections are at most 256 MiB each. The sections are byte-identical to the files of
a store's snapshot directory, so a store imports and exports packed snapshots by copying, and a
directory snapshot can be read as the packed stream it is equivalent to. A reader of a
non-seekable stream checks each section's CRC as it streams and the trailer at the end; nothing
follows the trailer.

**Writing without seeking (informative).** The manifest precedes the segments, every section
starts with its length, and a segment file starts with its chunk count and footer offset (7.1),
so a writer must know every segment's length, CRC and chunk index before it writes the first
segment byte. A writer that neither seeks nor holds the encoded snapshot can plan first: encode
every chunk once and keep only its size, CRC and first and last id; write the header, the
manifest and `schema.json`; then encode each chunk again from the same elements and write it.
Chunks are self-contained (7.3), so with a deterministic compressor the second encoding yields
the planned bytes; the reference writer checks every chunk's CRC against its plan and fails on
a difference (the graph must not change in between). A segment's CRC covers its header, which
is known only after the chunks; it is the CRC-32C combination of the header's CRC and the CRC
of the rest. A store writes a snapshot directory the same way, each segment file as soon as its
plan is complete and the manifest last, holding one chunk at a time.

## 9. The write-ahead log

The WAL is a sequence of segment files. Over a plain stream (a journal outside a store) the same
bytes are one segment that is never rotated.

### 9.1 Segment header

| Offset | Size | Field |
|---:|---:|---|
| 0 | 8 | magic `GRSLWALS` |
| 8 | 4 | format version |
| 12 | 16 | `graph_id` of the lineage that writes the segment |
| 28 | 8 | first `commit_seq` the segment may contain |
| 36 | 8 | created at (time) |
| 44 | 16 | reserved, zero |
| 60 | 4 | CRC-32C of bytes 0..60 |

In a store the file name is the first `commit_seq` as 20 decimal digits; a header whose first
`commit_seq` differs from its file name is damage (9.8).

### 9.2 Record frame

A record is a 25-byte frame followed by its payload:

| Offset | Size | Field |
|---:|---:|---|
| 0 | 4 | sync marker `0x47525357` (bytes `57 53 52 47`) |
| 4 | 4 | payload length |
| 8 | 8 | `commit_seq`: a Commit's own number; for a Mark or Checkpoint, the commit it refers to |
| 16 | 1 | record type |
| 17 | 4 | header CRC-32C of bytes 0..17 |
| 21 | 4 | body CRC-32C of the payload |
| 25 | length | payload |

| Type | Record |
|---:|---|
| 1 | Commit (9.3) |
| 2 | Mark (9.4) |
| 3 | Checkpoint (9.5) |
| 0, 4..255 | reserved: an unknown type is invalid, never skipped |

The sync marker and `commit_seq` lie outside the possibly compressed payload, so a reader can
resynchronise after a damaged record by searching for the next position that holds the sync
marker, a valid header CRC and a valid body CRC. The separate header CRC makes a damaged length
detectable, so it is never taken for an interrupted write (9.6). A record is never split between
segments.

### 9.3 Commit payload

```text
commit   = commit_seq u64 | time i64 | node_sequence u64 | edge_sequence u64 | codec u8 | body'
body'    = body                                  when codec = 0
         | uncompressed length u32 | stored body  when codec = 1 or 2
body     = principal opt_string | attributes property map | uvar mutation count | mutation*
```

`commit_seq` MUST equal the frame's. The auto-id sequences are written with every commit so
that an id handed out before a crash, a drop or a rollback is never handed out again after a
recovery. `body` is at most 1 GiB (the bound of section 2): a writer
refuses a larger commit (`PersistError::CommitTooLarge`, the unit rolls back) instead of
splitting it over records. A commit with a mutation count of 0 is valid: a *sequence-only commit*, a unit that
changed nothing but raised an auto-id sequence (applying a change set whose mutations were all
compacted away); replaying it only raises the sequences (and advances the commit position). The
attributes are free metadata of the commit (keys inline). Every string is inline.

| Op | Mutation | Fields |
|---:|---|---|
| 1 | AddVertex | id `string` \| `uvar` label count \| label `string`* \| property map |
| 2 | DropVertex | id \| before: labels (as in AddVertex) \| property map |
| 3 | AddEdge | id \| label `opt_string` \| out id \| in id \| property map |
| 4 | DropEdge | id \| before: label `opt_string` \| out id \| in id \| property map |
| 5 | SetProperty | element kind `u8` (1 vertex, 2 edge) \| id \| key `string` \| before value (tag 255 = absent) \| after value |
| 6 | RemoveProperty | element kind \| id \| key \| before value |
| 7 | AddLabel | vertex id \| label |
| 8 | RemoveLabel | vertex id \| label |
| 9 | SetSchema | before schema `string` \| after schema `string` (canonical JSON) |
| 10 | SetDefinition | kind `u8` \| name `string` \| before `opt(definition)` \| after `opt(definition)` |
| 11 | SetEdgeLabel | edge id \| before label `opt_string` \| after label `string` |

`opt(definition)` is `u8 0` for none, or `u8 1` followed by a definition encoded as in the
definitions table (6.3); other presence bytes are invalid. Op 10 serves every definition kind,
so a new kind needs no new op: a define has no before, a removal no after, a change (replace,
move, describe) both; at least one is present, and each present image MUST carry the op's kind
and name. A definition the reader cannot decode follows 6.3: an unknown kind is kept opaque
(and replayed as such), a payload of a known kind that is not a valid property map is damage,
and a valid map is never damage (kept, not served, or kept opaque; 16 rule 9). A writer MUST NOT write an
after image that is a critical definition of a kind it does not know (16 rule 3).

Mutations are in execution order. Before images are always present: a reader can reconstruct
the state before each commit, and CDC consumers get before and after values. The edges a vertex
drop removes appear as DropEdge mutations before the DropVertex. A write to a nested path inside
a property is a SetProperty of the top-level key.

### 9.4 Mark payload

`mark = commit_seq u64 | time i64 | name string`.

A mark names the position right after commit `commit_seq` (recovering to it replays up to and
including that commit). Its time is `max(now, time of the last commit)`. Mark names are unique
along a store's whole history (across lineages); a duplicate MUST be refused when it is set.

### 9.5 Checkpoint payload

`checkpoint = commit_seq u64 | time i64`: informational, "a snapshot at `commit_seq` exists".
Readers accept and skip it. Commits newer than `commit_seq` MAY precede it.

### 9.6 Torn tail and damage

At the end of the **last** segment (of a recovery's last journal), the following are a torn
tail, the remains of an interrupted write:

1. fewer than 25 bytes that are a prefix of the sync marker (the first up to four bytes match)
   or all zero;
2. a frame with a valid header CRC whose payload runs past the end of the file;
3. zero bytes only, from a record boundary to the end (space the file system allocated that the
   interrupted write never filled).

A full frame header with a bad header CRC, a full record with a bad body CRC, and any other bytes
at the end are **damage**, wherever they are, also in the last record. Anything that is not a
valid record before the end of a segment that is not the last one is damage. A torn tail is cut
off by the next read-write open (12.2), never by a reader; damage is never cut (13).

### 9.7 WAL segments in a store

- Segments are named by the first `commit_seq` they may contain. Rotation is lazy: before a
  record is written, if the current segment exceeds the segment size (default 16 MiB) or a
  checkpoint asked for a new segment, the current file is synced, and a new segment named by the
  next commit sequence number is created with its header, synced, and its directory synced. A
  Mark or Checkpoint record may therefore be the first record of a segment named
  `commit_seq + 1` of the commit it refers to. Only the last segment can end in a torn record.
- The **replay set** of a snapshot at position `S` is the last segment whose name is
  `<= S + 1` and every later one. Commit records `<= S` are skipped; every later commit record
  MUST be exactly the previous + 1 (else the history has a gap: damage).
- After an in-place rollback or a fork, the new lineage writes a new segment (its header carries
  the new `graph_id`).
- A record whose write or sync failed is cut off again (the file truncated to the record's start
  and synced) before the writer refuses further commits. When that cut fails, the valid end is
  recorded in `GRAPH` (flag bit 1); readers MUST read only up to it, and the next read-write open
  truncates the segment there and removes later segments.

### 9.8 The newest segment and a clean close

A cut of the last WAL record is indistinguishable from an interrupted write and is treated as a
torn tail. A segment that is missing, emptied or replaced by another segment's bytes must never
be: it would silently open the store at an earlier commit. Without a metadata write per commit,
`GRAPH` therefore records:

- **`wal_head`** (flag bit 3, offset 84): the name of the newest WAL segment, written whenever a
  segment is started, after the segment's header was written and synced and its directory synced
  (rotation, the first segment of an open, the segment `target + 1` of a rollback together with
  its lineage entry), and after an attic restore (the newest restored segment); a recorded WAL
  cut lowers it to the cut segment. A failed write leaves the older value.
- **`closed_at`** (flag bit 4, offset 92): the last commit at a clean close (state 0).

At every read-write open (before the torn-tail rules), in the damage scan and in verification it
is **damage** when: a segment's header names another first commit than its file name; the
segment `wal_head` is missing, or it or an older segment is shorter than its 64-byte header; or
the store was closed cleanly at `closed_at` and the snapshot plus the WAL end at an earlier
commit. A last segment **newer** than `wal_head` and shorter than its header is a crash while it
was being started: it is removed as a torn tail.

### 9.9 Write ordering and durability

The writer of a journal MUST:

1. assign the commit's sequence number (checked against overflow) before writing its record;
2. run every other commit check (hooks that may veto) before writing, so that nothing fallible
   follows a written record and the WAL never holds a commit that did not happen;
3. write the record, then make it durable according to the configured durability: a sync after
   every commit (the default), at most once per interval, or left to the operating system;
4. treat a failed write or sync as a vetoed commit (the unit rolls back) and refuse every later
   commit and mark until the WAL is reopened, because the state of the file is unknown.

The **durable end** is the offset after the last record whose sync completed. Records after it
may still be vetoed and MUST NOT be copied by a backup (12.6).

## 10. The `marks` file

A derived list of the store's marks: append-only frames
`length u32 | CRC-32C u32 of the payload | Mark payload (9.4)`, appended and synced after the
Mark record is durable in the WAL. The WAL is the source of truth: a missing or damaged `marks`
file is rebuilt from all WAL segments on open, and marks in the replayed WAL that the file lacks
are appended. Listing marks reads only this file and lists only the marks a target can reach: those
at or after the oldest snapshot of the store (or backup) listed. The file is not rewritten by a
prune (12.4): a backup copies it and may still reach older marks with older snapshots of its own.
A mark that is not listed keeps its name taken; resolving it as a target fails.

## 11. The `INTENT` and `ATTIC` files

### 11.1 `INTENT`

A multi-file operation writes `INTENT` (compact JSON, replaced atomically and synced) before its
first file change and removes it after its last one. Every step is idempotent; a read-write open
that finds `INTENT` completes (or, for a backup increment, rolls back) the operation before
anything else. An unknown `op` MUST fail the open, as unsupported content of a newer writer
(16 rule 9), never as damage. Every path in it follows 4.2.

```json
{"op": "<operation>", "files": ["<store-relative names>"], "started_at": <micros>, ...}
```

Further parameters are string values. Operations of store format version 3 (the same as version 2):

| `op` | Parameters and steps |
|---|---|
| `prune` | `files`: the snapshot directories and WAL segments to remove. On open: each listed name is removed (missing is fine), the directories synced, `INTENT` removed |
| `rollback` | `files`: snapshot directories and WAL segments to move (or delete); `target`, `last_commit`, `base_snapshot` (newest snapshot at or before the target), `mode` (`move` \| `delete`), `old_graph_id`, `new_graph_id` (32 hex digits), `time`, `attic` (`attic/<YYYYMMDDTHHMMSSZ>-<target+1, 20 digits>`), `reason`; and either `split` + `split_offset` (the segment holding the first commit after the target is cut there; its tail becomes `<attic>/wal/<target+1>.wal` with the old lineage's header) or `kept` (hex; when that segment is itself named `target+1`, it moves whole, and the records before the cut, marks pointing at the target, open the new lineage's segment). The tail of a split segment keeps the header lineage of the segment it is cut from (an ANCESTOR's when the target lies before the current lineage's branch point), not necessarily `old_graph_id`. `files` names every directory under `snapshots/` after the target, holders (4.1) included. For the existing attic entries (11.2) whose history before their own WAL reaches past the target, three more parameters, each a list of lines separated by `\n`: `copies` (`<source> <destination>`: the store's WAL segments, or the split segment meaning its tail as the attic gets it, that hold the commits from `target + 1` up to such an entry's prefix end, copied into `<entry>/wal/` under their names), `rebase` (the ids of those entries) and `drop` (the snapshot directories of entries one of whose snapshots references a file of a directory this rollback moves). Steps: attic directories; the `copies` (each written whole under its final name, skipped when present; all of them exist before any source changes); the `drop` directories removed; each `rebase` entry's `ATTIC` rewritten with `base_snapshot` = this rollback's `base_snapshot`; tail and cut; move or delete files; `ATTIC`; `marks` filtered to `<= target`; `GRAPH` with the new lineage entry `[new id, branched at target, time]`; the new lineage's segment `<target+1>.wal` (written whole, its header created at `time`, then the kept records; one found shorter is written again); holders no snapshot of the store or of an attic entry references any more are removed (12.4); `INTENT` removed |
| `attic_restore` | Allowed only while the store is at the entry's target on the rollback's lineage and its new segment holds nothing beyond the kept records. `files`: the entry's directories under `snapshots/` (holders too) and its WAL segments named after its target (its prefix copies, 11.2, stay behind: the store holds that history again); refused when one of them exists in the store. Steps: remove the new lineage's segment and a snapshot of the new lineage at the target (a checkpoint of the closed store after the rollback writes one without a WAL record), move the entry's files back, drop the lineage entry from `GRAPH`, rebuild `marks` from the WAL, remove the entry |
| `backup_increment` | written in the **backup's** root (12.7): `files` (new snapshot directories and WAL segments), `graph_before` (hex of the backup's `GRAPH`), `taken_at`, optionally `tail` + `tail_len` (the segment appended to and its length before) and `replaced` |

Stray `*.tmp` files in the store root and `snapshots/tmp-*` directories are removed on open.

### 11.2 `ATTIC` and attic entries

An attic entry is a directory `attic/<YYYYMMDDTHHMMSSZ>-<first moved commit, 20 digits>/` with
the moved `snapshots/` and `wal/` and an `ATTIC` file (the name is unique: when it is taken, by
an entry or an entry being removed, the time part is advanced by one second until it is free;
the `ATTIC` file records the exact time): compact JSON with the string values
`graph_id` (the old lineage), `new_graph_id`, `target`, `last_commit`, `time`, `reason`,
`base_snapshot` and `kept_bytes`. An entry being removed is first renamed
`attic/removing-<id>` (removed at the next open).

**An entry is self-sufficient given the store** (it never needs another entry). Its history is
its base snapshot (`base_snapshot`, a snapshot of the store's own `snapshots/`), then the store's
WAL from it up to the *prefix end*, then the entry's own WAL; or, when the entry holds
snapshots, its newest one and its own WAL after it. The prefix end is the commit before the
entry's first WAL segment, at most its `target`. Segments of the entry named at or before its
`target` are its **prefix**: copies of the store's history that a later rollback to an earlier
commit (`rebase`, 11.1) gave it before moving that history into its own entry; the same rollback
points `base_snapshot` at its own base and drops the entry's snapshots when they reference files
it moves (they are derived: the entry's WAL rebuilds every state they held). The store's history
up to an entry's prefix end therefore stays in the store: rollbacks keep everything at or before
their target, restores add only history after theirs, and prune keeps every entry's base and the
WAL after it (12.4). The entry's segment and snapshot listings treat a missing `snapshots/` or
`wal/` as empty.

## 12. Operations

### 12.1 Load and recovery

A target is `Latest`, a `commit_seq`, a time (the last commit whose time is at or before it), or
a mark name. Loading a target: take the newest snapshot at or before the target (for `Latest`,
the newest), then replay its replay set (9.7) along the lineage chain (5.4) up to the target.

- A load checks `GRAPH` and the lineage, may check the manifest's estimated in-memory bytes
  against a budget, loads the vertex segments, then the edge segments (an edge's endpoints are
  resolved by id), rebuilds every index and statistic, and restores the `graph_id`, the
  position, the last commit time and the auto-id sequences.
- A snapshot load does **not** validate the data against the stored schema: the data was valid
  when it was committed, and integrity comes from the checksums. It restores the catalog of
  definitions from the manifest (6.3).
- WAL replay applies mutations without running commit hooks or schema validation; op 10 stores
  or removes a definition.
- A read-write open whose loaded state holds a critical definition of a kind the reader does not
  know, or whose base snapshot has a file of a critical segment kind it does not know (7.5),
  opens the store **read-only** (16 rule 3): every commit, mark, checkpoint, prune, rollback
  and compaction is refused, and nothing is written after the load (no torn-tail cut, no
  `marks` rebuild, no change of `GRAPH`). The steps of 12.2 before the load (completing an
  `INTENT`, removing leftovers, applying a recorded WAL cut) MAY have run; they do not change the
  graph. Reading, verification, export, fork and backups work; a fork or backup keeps the
  definition and is read-only for that reader too.
- A read-write open whose snapshot `schema.json` or a schema image of op 9 the reader cannot
  parse (a newer writer's keyword or mode; its checksum is valid) loads the data WITHOUT the
  schema and opens the store **read-only** in the same way (16 rule 9). As the loaded state
  lacks the schema, every store operation that would write a state derived from the files (a
  checkpoint, a fork, a repair) is refused as unsupported; a file-level backup and
  the export of a stored snapshot copy the bytes as they are. A snapshot written from the
  loaded graph in memory (`g.export_snapshot`) has no schema.
- Recovery to a target before the end yields a read-only past state. A writable state from it
  is either a fork (a new store, 12.5) or an in-place rollback.
- Without a snapshot (a journal over a stream), the first journal MUST start at commit 1.

### 12.2 Open and close

A read-write open: take the writer lock (12.10); complete an `INTENT`; remove leftovers
(`snapshots/tmp-*`, `*.tmp`); apply a recorded WAL cut; run the checks of 9.8; load (12.1, mode
`Latest`); cut a torn tail of the last segment; rebuild `marks` if needed; set `GRAPH` state 1.
A clean close: sync, then write `GRAPH` with state 0 and `closed_at`. A store whose state is 1
at open was not closed cleanly; that is not damage (the WAL replay recovers it).

### 12.3 Checkpoints

A checkpoint writes a snapshot of the current state: encoded consistently at one position
(the reference implementation streams it chunk by chunk, see 8, or merges it, below), into
`snapshots/tmp-<32 hex>/`, every file synced, the directory synced, **verified** (manifest CRCs,
`schema.json`, every segment's length and CRC, every chunk CRC and the chunk index), renamed to
its 20-digit name, `snapshots/` synced; then a Checkpoint record is appended and a new WAL
segment is requested. The manifest carries the whole catalog of definitions, every definition
of an unknown kind byte for byte as read (6.3). Only renamed snapshots exist for listing, loading
and retention. A
checkpoint at a position that already has a snapshot returns that snapshot. The chunk target
size is the store's (`GRAPH` offset 44) for its whole life.

**Merge checkpoint.** A writer MAY build the new snapshot from the newest snapshot (the *base*)
and the WAL after it instead of from a loaded graph (the reference implementation does, without
the graph's lock, unless the WAL after the base exceeds a configured bound). Such a writer:

- reads the WAL only up to its durable end (9.9), from the base's replay set (9.7) along the
  lineage chain (5.4), with every header and body CRC, commit continuity and every lineage rule
  of a load; the new snapshot's position is the last commit read and its `graph_id` the lineage
  the replay ends in (an empty segment of a new lineage counts);
- applies the commits as a load replays them (12.1) to the base's state of every element they
  name, and MUST check every mutation's *before* image against that state: a mismatch (the WAL
  does not continue the base) is damage;
- reads every base chunk it uses with its CRC and chunk index row, every base segment it
  rewrites in full (header, chunks, footer, file CRC), and verifies the new snapshot, referenced
  files included, before the rename;
- carries the schema, the catalog (unknown non-critical definitions byte for byte), the
  auto-id sequences, the position and the last commit time exactly as a load followed by a
  checkpoint would; drops files of an ignorable unknown segment kind (7.5); refuses a base or a
  state with something critical it does not know (16 rule 3);
- rewrites every segment that holds an element a commit names (an element whose id falls
  between two segments' ranges goes to the later one, beyond the last range to the last one);
  the rewritten elements are in strictly increasing id order across the new segments;
- keeps every other segment of the base byte for byte under the **donor rule**: the new
  snapshot uses no file the base uses, so the base stays an independent copy of every id range,
  the donor of 13.4. Such a segment is referenced (6.2) when the snapshot before the base (the
  newest snapshot older than the base, of the same lineage) has a *twin*: a row with the same
  kind, first and last id, element count, byte length and file CRC, the same chunk index rows,
  naming another file than the base's (after resolving both, 6.2); the reference names that
  file. Otherwise the segment's bytes are copied into a new file of the new snapshot (read with
  the file CRC checked). So an unchanged range alternates between two files from checkpoint to
  checkpoint, and only the first checkpoint after the range was rewritten copies it. A writer
  that encodes a loaded graph (8) writes only new files and meets the rule too;
- on damage, writes nothing visible: the temporary directory is removed, no file of the store
  changes.

A closed store MAY be checkpointed the same way by a process that takes the writer lock (12.10),
completes an `INTENT` and removes leftovers (12.2); it ignores a torn tail of the last segment,
honours a recorded WAL cut, applies the checks of 9.8, and appends no Checkpoint record.

### 12.4 Prune and retention

`prune(up_to)` keeps every snapshot from `min(newest snapshot <= up_to, second-newest snapshot)`
on, plus the base snapshot of every attic entry; it verifies each kept snapshot first, then
removes the older snapshots and the WAL segments before the replay set of the oldest kept one,
under an `INTENT` (`prune`). The last two verified snapshots and the WAL between them always stay
(the donors of 13.4). Nothing is ever removed implicitly. Prune is refused while a backup holds
the pin (12.10). Marks before the oldest kept snapshot become unreachable (10); a prune reports
them, from the `marks` file only.

Files that a kept snapshot or a snapshot of an attic entry references (6.2) stay: of a removed
snapshot directory that holds one, only the other files are removed, the `manifest` first (the
directory is a holder from then on, 4.1); a holder older than the oldest kept snapshot that no
such snapshot uses any more is removed whole. The `INTENT`'s `files` then name single files of
such a directory. Under the donor rule (12.3) the last two snapshots share no file, so each is
the other's donor. A rollback that deleted the history using a holder's files, and the removal
of an attic entry whose snapshots used them, remove the files no snapshot of the store or of an
attic entry references any more (and the holder when it is left empty); a file nobody
references is never referenced again (a checkpoint references only files of a listed snapshot),
so this needs no `INTENT`.

### 12.5 Fork, in-place rollback, attic

- **Fork**: load the target (12.1) and write it as a new store: one snapshot (with the catalog,
  6.3), a new `graph_id`, the manifest's parent fields set, the chain `[new id, branched at
  target, time]` + the source's chain. Marks are not copied.
- **In-place rollback** (`move` or `delete`), under `INTENT` `rollback` (11.1): everything after
  the target (later snapshots, the WAL after it; the segment holding the target is split) moves
  into an attic entry or is deleted; the store continues as a new lineage in the same root.
- **Attic restore** (11.1) undoes a rollback while nothing was committed or marked since; an
  attic fork writes the entry's history as a new store. Prune never removes an attic entry's base
  snapshot except together with the entry.
- **Every attic entry stays whole** after any sequence of rollbacks, restores, removals and
  prunes (11.2): a rollback to a commit before an older entry's history copies what that entry
  needs into it first (11.1 `copies`, `rebase`, `drop`). Entries never depend on each other:
  removing, restoring or forking one never breaks another. **Restore order (normative):** an
  entry MAY be restored only while `GRAPH`'s current `graph_id` equals the entry's
  `new_graph_id` (the lineage its rollback started) and nothing was committed or marked since;
  after several rollbacks the entries therefore restore newest first, each restore bringing back
  the lineage the next older one needs. A writer MUST refuse an out-of-order restore without
  changing any file, and SHOULD name the newer entry to restore first. Any entry MAY be forked at
  any time.
- **A rollback or restore that fails** once its `INTENT` is written leaves the files ahead of the
  graph in memory: the writer refuses every further change and its close writes no `GRAPH`; the
  next open completes the operation.

### 12.6 Backups

A backup is a file-level copy that takes **no graph lock**: snapshots are immutable once renamed
and the WAL is append-only. A full backup copies `GRAPH` (rewritten: state 0, no WAL cut, the
backup marker, `wal_head` and `closed_at` of the backup's own files) to `GRAPH` and `GRAPH.copy`;
the `marks` file as read before the copy ends were fixed (every mark in it is in the copied WAL);
the latest snapshot (verified before and after the copy) with the files it references (6.2,
copied under their names, in holders when their snapshot is not copied) and its replay set, the
segment of the durable end cut at the durable end, later segments left out. A copy by another process, which has
no durable end, copies the last segment up to its last complete record (a torn tail, 9.6, is left
out; damage before it stops the backup), never past a recorded WAL cut.
`LOCK`, `BACKUP` and the attic are not copied. During the copy the source's `BACKUP` pin is held
shared.

**Verification.** A backup (full, increment, ZIP) reads every checksum of what it copies in the
SOURCE before it writes anything, and again in the COPY after writing it:

- in the source: both `GRAPH` copies (one unusable copy is a warning, both are damage; the backup
  writes two fresh ones); the `marks` file (derived, 10: damage is a warning and the backup's is
  rebuilt from the WAL it copies); every snapshot it copies with everything 12.3 verifies
  (manifest CRCs, `schema.json`, every segment and chunk, the chunk index, referenced files) and
  both copies of the manifest's fixed part (one damaged copy is a warning: the backup gets the
  file with the intact copy in both places, byte for byte what the writer wrote); every WAL
  record of every segment it copies, header and body CRC, each segment's header against its name
  (9.8) and the lineage chain, the segment set of 9.8, and commit continuity (every commit the
  previous + 1; the first commit after the snapshot, or after the backup's position, its
  successor);
- in the copy: `GRAPH`, `GRAPH.copy` and `marks` byte for byte as written, every snapshot as
  12.3 verifies it (both fixed copies strictly), every WAL segment read again with the same end
  and the same commits as the source's; a ZIP archive is read back entry by entry (12.8).

Damage in the source stops the backup before anything is written; damage of the copy removes a
new backup and rolls an increment back (for an increment, the backup's old `GRAPH` is written
first, so the rollback never takes a final `GRAPH` that did not read back for a completed one).
When the copy does not read back, the source's checks run again: a source that fails them now is
the source's damage. The report lists each problem with its file, offset and reason, as
verification does (12.9), and names the side; the remedy for damage in the source is a repair of
the SOURCE (13.4, never of the backup), or, while the source is open in a process whose graph is
intact, a backup from memory (12.11).

**Marker.** Flag bit 2 marks a backup; offset 68 holds the last commit it holds and offset 76 when
it (its last increment) was taken. `graph_id`, `store_id` and the lineage chain are the source's. A marked
store never opens read-write; read-only loads, verification, listing, export, fork, prune of the
backup and backups *of* it work. Restoring a backup in place takes the writer lock, rolls back an
interrupted increment, clears bit 2 and the marker fields and writes `GRAPH` clean: the root is
then the live store with the same `graph_id` and `store_id`.

### 12.7 Incremental backups

A backup into a root that holds a marked backup is an **increment**; into an empty or missing
root, a full backup; anything else is refused (a store that is not a backup is never written to).

- **Relation.** The backup's chain MUST equal the tail of the source's chain that starts at the
  entry with the backup's `graph_id`. With that entry at position k > 0, the backup may hold
  commits only up to the smallest `branched at` of the entries 0..k-1 (a rollback of a later
  lineage to before its own start branches from an ancestor at an earlier commit); a backup
  holding more (the source was rolled back behind the backup's position) is refused; a full
  backup into a new directory follows the new history, and the old backup stays as the archive of
  the abandoned one. Unrelated stores are refused.
- **Identity.** The backup's `store_id` (5.5) MUST equal the source's. A fork, repair or
  conversion of the backed-up store shares its lineage chain up to the copy but is a different
  store: its increment is refused, and a full backup into a new directory is needed.
- **Position.** The backup's position is the larger of its newest snapshot and the last commit of
  its WAL; its last segment L MUST end with a complete record.
- **WAL.** If the source has a segment named L with an equal 64-byte header: the source's copy
  end MUST be at least L's length, and the 25-byte frame of L's last record MUST equal the
  source's bytes at that offset (prefix check); the bytes from L's length to the copy end are
  appended. Equal name, different header: allowed only when L holds no commit record and the
  source branched from the backup's lineage (a rollback to exactly the backup's position); the
  source's segment then replaces L. The source has no segment L (pruned): its next segment MUST
  be named at most position + 1, else the gap is refused and a full backup is needed. Every
  source segment named after L, up to the durable end, is copied whole.
- **Snapshots.** Every source snapshot newer than the backup's newest is copied, verified before
  and after. A referenced file (6.2) the backup lacks (its snapshot was written and pruned since
  the last increment) is copied from the source, under its name, before the snapshots that use
  it, and listed in the `INTENT`'s `files`; one the source lacks too is refused.
- **Steps**, with the backup's writer lock held and the source's pin shared: complete or roll
  back an earlier interrupted increment; write `INTENT` `backup_increment`; when the lineage
  changed, write `GRAPH` with the source's chain and the OLD marker; copy each snapshot through
  `snapshots/tmp-<hex>/` (synced, verified, renamed); append the tail and sync, re-reading every
  record of the segment; copy a replacing segment and each new segment through `<name>.wal.tmp`
  (synced, re-read, renamed; a replaced L is first renamed `<name>.wal.replaced`); check that the
  copied commits continue the backup's WAL without gap or overlap; write `marks`; write `GRAPH`
  with the source's identity, clean, no WAL cut, the new marker; remove the `.replaced` file;
  remove `INTENT`. The backup is a valid store after every step.
- **Interrupted increment** (found at the next backup, restore or prune of the backup): when the
  marker's time equals the intent's `taken_at`, the increment is complete (remove `.replaced` and
  `INTENT`); otherwise it is rolled back: the listed files and every `snapshots/tmp-*` and
  `wal/*.tmp` removed, a `.replaced` segment renamed back, the tail cut to `tail_len`, `GRAPH`
  rewritten from `graph_before`, `marks` rebuilt from the WAL, `INTENT` removed.
- Nothing pins the source's WAL for a backup: a prune of the source can make the next increment
  impossible (a full backup is then needed).

### 12.8 ZIP backups

A full backup MAY be written as one ZIP archive: stored entries (method 0), UTF-8 names (the
store's names, 4.2), DOS time 1980-01-01 00:00, the CRC-32 (IEEE, as ZIP requires) in the local
header, zip64 extended fields always (sizes in the local header, sizes and offset in the central
header), then the zip64 end record, its locator and the end record. The archive's `GRAPH` carries
the backup marker; unpacking it is the explicit restore and clears the marker. A reader MUST
accept only the store's names and MUST refuse compressed, encrypted or streamed (data
descriptor) entries. A well-formed name outside the layout it knows (plain segments, at most
four) is a newer writer's file: the restore is refused as unsupported, never as damage (16
rule 9); any other name is damage. The reference writer reads the archive back after writing it (12.6): every
entry's CRC-32 and every checksum of the store files in it, which arrive in an order a one-pass
reader can check (`GRAPH`, `GRAPH.copy`, `marks`, then each snapshot's `manifest` before its
other files, then the WAL segments).

### 12.9 Verification (scrub)

Verification reads every checksum: both `GRAPH` copies, every manifest (both fixed copies), every
`schema.json`, every segment and chunk (and the chunk index), every WAL record; it checks id order
and non-overlapping ranges, WAL continuity (`commit_seq` + 1 per Commit, no gaps) along the
lineage chain, the checks of 9.8, every attic entry (its history is rebuilt as 11.2 describes,
and each of its snapshots verified), and the `marks` file. A holder (4.1) with files that no
snapshot of the store or of an attic entry references is a problem (a lost manifest, or files a
prune left). A segment file the newest snapshot shares with the previous one (a break of the
donor rule, 12.3) is a **warning**, not damage: the range has no independent donor; the next
checkpoint finds no twin for it and copies it. It is meant to be run regularly: it finds damage while donors
still exist.

### 12.10 Locks and pins

- **Writer lock**: at most one writer per store. On a directory it is an exclusive operating
  system advisory lock (`flock` / `LockFileEx`) on `LOCK`, held while the store is open
  read-write; in a container file it is the lock on the file itself (14.5). Read-only loads,
  verification, listing, export, fork and out-of-process backups take none.
- **Backup pin**: a backup holds the `BACKUP` lock shared while it copies (waiting while a prune
  holds it); prune takes it exclusively and without blocking, and is refused while a backup runs.

### 12.11 Backups from memory

A writer that holds a store open and has found damage in its files (13.3) MAY write a backup of
its graph in memory instead of copying files: the graph was verified when it was loaded and
changed only by commits since. It reads and writes NOTHING in the store's directory. The backup is
an ordinary marked backup (12.6):

- `snapshots/<seq>/`: one snapshot of the graph at its last commit `seq`, written through
  `snapshots/tmp-<hex>/`, synced, verified (12.3) and renamed;
- `wal/<seq + 1>.wal`: an empty WAL segment of the graph's lineage (its header only), synced;
- `GRAPH` and `GRAPH.copy`: the store's identity, lineage chain and creation parameters (5.2)
  with state 0, no WAL cut, the backup marker (`seq`, the time), `wal_head` `seq + 1`,
  `closed_at` `seq`; read back. No `marks` file: like a fork, the backup holds no WAL before its
  snapshot, so the marks would not be targets.

Into a missing or empty root it is a full backup. Into a marked backup of the same store (the
relation of 12.7, and its position at most `seq`) it is an increment under an `INTENT`
`backup_increment` whose `files` are the new snapshot directory and segment: the snapshot is newer
than everything the backup holds, the commits between the backup's position and `seq` are not in
it (no target between them). A later file-level increment of such a backup finds no segment of
the source it continues and is refused: a full backup is needed.

## 13. Damage handling

### 13.1 Redundancy

- `GRAPH` and `GRAPH.copy` (5.3); the manifest's fixed part at its head and its tail (6).
- The manifest's chunk index (6.2) carries the id range and CRC of every chunk.
- WAL frames carry a sync marker and `commit_seq` outside the payload (9.2).
- Retention keeps the last two verified snapshots and the WAL between them (12.4), and the donor
  rule (12.3) makes them independent: the newest snapshot shares no segment file with the
  previous one, so every id range of the newest has a donor in a distinct file (the previous
  snapshot's chunks plus the WAL between the two), and every range of the previous one has a
  later snapshot covering it. A file the newest snapshot references is a twin in the directory
  (or holder) of an older snapshot; its damage is repaired from the previous snapshot like
  damage of the newest's own files.
- Flag bit 0 of `GRAPH` is reserved for parity data (not in store format version 3).

### 13.2 The damage scan

Chunks are checked one by one against the manifest's chunk index (a damaged segment header or
footer with intact chunks is framing damage; the data is intact). WAL records are read with
resynchronisation (9.2). Torn tails follow 9.6, the segment set follows 9.8. The commits a
damaged WAL region lost are those between the valid commits around it; if none are missing, the
region held a Mark or Checkpoint record. Damage to one copy of redundant metadata (one `GRAPH`
copy, one manifest fixed part of the latest snapshot) is a warning, not damage.

### 13.3 Maintenance mode

A read-write open whose load fails with damage (a checksum, a gap, a lineage mismatch, a missing
file, both `GRAPH` copies invalid, container damage, 14.4) does not fail and does not repair: the
store opens **read-only** with a damage report that lists the damaged files, chunks and records,
the id ranges and commits affected, and for each its **donor**: an older snapshot whose chunks
cover the id range plus the WAL up to the damaged one; a later snapshot that covers a damaged
WAL record; or the other copy. The readable state is built from donors. Nothing is written in
maintenance mode (no state change in `GRAPH`, no torn-tail cut).

**Damage found while a store is open.** Damage a writer finds in its files while it holds the
store open (a backup, 12.6; a checkpoint's merge, 12.3; a verification, 12.9) follows the store's
**damage policy** (5.2 bit 5), fixed when the store was created:

- `maintenance` (the default): the store turns read-only for the rest of the time it is open,
  with the damage as the reason: every commit, mark, checkpoint, prune, rollback and compaction
  is refused, and nothing more is written (a close syncs the WAL and does not rewrite `GRAPH`, so
  the next open sees an unclean close, which is no damage). Reading and a backup from memory
  (12.11) go on.
- `continue`: the store keeps accepting commits; the damage stays reported for as long as it is
  open.

The policy does not apply to damage found when the store is OPENED: that is always maintenance
mode, as above. Problems of attic entries alone (history moved aside, 11.2) are reported by
verification but do not count as damage of the store's own history.

### 13.4 Repair

Repair is explicit and never in place; it writes a new, verified store elsewhere:

- The base is the newest snapshot with a readable manifest; its schema comes from `schema.json`,
  else an older snapshot's plus the WAL's schema changes. Its catalog of definitions comes from
  its manifest (covered by the variable part's CRC); when a definition there does not decode,
  from an older snapshot's catalog plus the WAL's op 10 changes up to the base, else it is lost
  (reported). The repaired snapshot carries the catalog, unknown definitions byte for byte.
- A damaged chunk is rebuilt from the newest older snapshot whose chunks overlapping its id range
  are all intact, provided the WAL between the two is complete: that range's elements are decoded
  and every WAL mutation naming an element of the range is replayed. Edges of intact chunks whose
  endpoint is lost are lost elements.
- The WAL after the base is replayed mutation by mutation: a damaged record is skipped when a
  later snapshot covers it; otherwise replay continues after the gap, and every element whose
  current state does not match a later mutation's *before* image (or whose mutation fails) is
  reported as **diverged** (the later value is kept), never guessed.
- The result is a new lineage whose chain continues the damaged store's, with one snapshot named
  `repaired`. The report lists what was repaired, the lost commits, id ranges and elements, and
  the diverged elements. The damaged files are never changed.

## 14. The single-file container

A whole store in ONE file (extension `.gstore` by convention; the content decides): a
log-structured container behind the backend abstraction of 4.3. The store's own files, names and
byte formats are exactly those of a directory; the container only records which bytes a name
holds. Nothing inside the file is ever overwritten, except the two header slots.

### 14.1 Layout

```text
0      preamble (16): magic "GRSLFILE" | container version u32 = 1 | flags u32 = 0;
       zeros up to 4096; written once at creation
4096   header slot 0 (64), zeros up to 8192
8192   header slot 1 (64), zeros up to 12288
12288  records, appended one after the other to the end of the file
```

**Header slot** (64 bytes): `0 magic "GRSLHEAD" | 8 generation u64 | 16 table offset u64 (the
table record's frame) | 24 table record length u64 (the whole frame) | 32 table record sequence
number u64 | 40 zero (20) | 60 CRC-32C of bytes 0..60`. An all-zero slot is unused. Generation
`g` lives in slot `g mod 2`; a new table is published by writing the OLDER slot with `g + 1`.

**Record frame**:

| Offset | Size | Field |
|---:|---:|---|
| 0 | 4 | sync marker `GSFR` |
| 4 | 1 | record type |
| 5 | 3 | zero |
| 8 | 8 | sequence number: +1 per record within one file, the first record is 1 |
| 16 | 8 | durable end: the file offset up to which every byte was synced when this record was written (at most the record's own offset) |
| 24 | 4 | fields length F (at most 16 KiB) |
| 28 | 8 | payload length P |
| 36 | F | fields |
| 36 + F | 4 | header CRC-32C of bytes 0..36+F |
| 40 + F | 4 | payload CRC-32C |
| 44 + F | P | payload |

**Record types** (fields: strings are `string`, numbers `uvar`; names follow 4.2 and are never
empty):

| Type | Record | Fields | Payload |
|---:|---|---|---|
| 1 | append | name, logical offset | the bytes to append there (a missing file is created; a longer file is first cut to the offset) |
| 2 | create | name | none: an empty file, replacing one |
| 3 | put | name | the file's whole new content (an atomic replacement) |
| 4 | truncate | name, length | none |
| 5 | mkdir | name | none (parents are implied) |
| 6 | rename | from, to | none: a file replaces a file; a directory moves with its subtree; onto an existing directory it is the completed cross-device move of 4.3 (only the source goes) |
| 7 | remove | name | none: a file or a whole subtree |
| 8 | table | none | every name (14.1, below) |

**Table payload**: `uvar` directory count, the directory names (`string`); `uvar` file count,
per file `name string | length uvar | extent count uvar | extents (container offset uvar,
length uvar)`. An extent at
offset 0 is a hole (zeros: bytes a damaged record lost); every other extent lies in
`12288..table offset`; the extents of a file add up to its length.

### 14.2 Writing

- Every backend operation appends records: an atomic replace is one put record + sync; an append
  is one append record + sync; a truncate one truncate record + sync; name changes (mkdir, create,
  rename, remove) one record each, durable at the next sync. Bytes written through an open file
  handle are buffered (at most 1 MiB) and become one append record when the handle is flushed,
  synced, cut, full or dropped.
- A new table is appended when the records since the last one reach
  `max(16 MiB, 16 × the last table record's length)`, when the writer lock is released (a close),
  and after a damaged or missing table was found: table record, sync, the older header slot with
  `generation + 1`, sync. A torn table write leaves the previous header and table valid.
- A new file is written under `<name>.create-<hex>.tmp` (preamble, an empty table as record 1,
  header slot 1 with generation 1), synced, hard-linked to the name only when nothing is there
  (a rename on file systems without links), and the directory synced.

### 14.3 Opening

The header slots are read; the valid ones are tried newest generation first: the table record a
slot points at MUST be a type-8 frame with that sequence number and length, an intact payload CRC
and a valid table. The first one that works is the base; the records after it are replayed. A
damaged slot or table is a warning: the older generation and the records after it give the same
names, because every change is a record. Without any usable table every record from offset 12288
is replayed (a warning).

### 14.4 Torn tail and damage

Replaying from the base, at a position `p`:

- fewer than 44 bytes, or a header whose CRC is valid but whose payload runs past the end: the
  **torn tail** of an interrupted write;
- an invalid frame header (marker, type, non-zero padding, fields length, header CRC, a durable
  end above `p`, or a sequence number below the expected one): the next position with a valid
  frame header is searched. None: if every byte from `p` on is zero, a torn tail; otherwise
  damage. Found at `q`: if the last valid frame of the file names a durable end above `p`, the
  bytes `p..q` had been made durable and are **damage** (replay continues at `q`; a sequence gap
  is reported as lost records); otherwise nothing after `p` was ever made durable: a torn tail at
  `p`;
- an intact header with a payload CRC mismatch: an append whose bytes were made durable (a later
  frame names a durable end above `p`) is applied as it is and reported as damage; the **last**
  append of the file is applied as it is without a report (the store's own checksums judge it, as
  on a directory: zeros at the end of a WAL segment are a WAL torn tail, other bytes WAL damage);
  a put or another record made durable is skipped and reported as damage (a put keeps the old
  content); a table made durable is skipped with a warning (redundant); anything not made durable
  is a torn tail at `p`;
- a valid record whose sequence number is above the expected one: damage (lost records);
- an append beyond a file's end (a lost earlier append) leaves a hole and is damage.

A torn tail is cut (truncate + sync) at the next write, never by a reader. Container damage
opens the store in maintenance mode (13.3); the container then refuses every write. Damage of
both header slots or both tables alone loses nothing.

### 14.5 Locks, processes, relocation

- The writer lock is the operating system's lock on the container file itself, held on its own
  handle; after taking it, the path MUST still name the locked file (a compaction may have
  replaced it), else it is taken again. A process without the writer lock changes the file only
  under that lock, taken for the one change.
- A process that does not hold the writer lock compares the file's identity and length before each
  operation and replays what another process appended (or reloads a replaced file). A backup pin
  freezes the pinning process's view: every byte it references stays where it is until a
  compaction, and a replaced file stays readable through the open handle.
- The file holds no path; it may be moved or copied while no process has it open. Temporary files
  of an interrupted create or compaction (`<name>.create-*.tmp`, `<name>.compact-*.tmp`) are
  removed when the writer lock is taken.

### 14.6 Compaction

A removal frees space only inside the file. Compaction copies the live state into
`<name>.compact-<hex>.tmp` next to the file: the preamble, one mkdir record per directory, per
file one create record (empty) or append records of at most 64 MiB, the table, header slot 1
with generation 1; synced; the writer lock is moved to the new file; the new file is renamed over
the old one and the directory synced. It needs free disk space for the live data meanwhile and
is refused on a damaged container.

**Space accounting**: total = the file's size; live = what a compaction would write, from the
in-memory table; garbage = total - live (old tables, replaced and removed files, the framing of
many small appends).

## 15. Versioning

- Every file starts with a magic and a format version. There are three version axes: the
  **store format version** in `GRAPH` (offset 8, 5.1), the **file format version** in the
  manifests, segments, WAL segments and packed snapshots, and the single-file **container
  version** (14.1). This specification is store format version 3, file format version 3 and
  container version 1.
- Version history of the store format: 1, the first version; 2 (Graphersal 0.1.0) added the
  store identity `store_id` at `GRAPH` offset 100 (5.5), which version 1 left zero; 3
  (Graphersal 0.2.0) has the layout of version 2 and marks a store written with file format
  version 2. The bump exists for older readers: a reader of store format version 2 knows only
  file format version 1 and would take the manifests and WAL segments of file format version 2
  for damage (an unknown version word is damage there, 13), open the store in maintenance mode
  and offer a repair that loses data; with version 3 in `GRAPH` it refuses the store as newer
  before it reads any other file.
- Version history of the file format: 1 (Graphersal 0.1.x); 2 (Graphersal 0.2.0) added the
  temporal value tags 9 to 13 and the vector value tag 14 (3); 3 (Graphersal 0.3.0) added the
  nanosecond temporal value tags 15 to 20 (with `offsettime` and the zone name of a
  `datetime`) and writes every temporal kind but `date` with them. Nothing else differs, so a
  reader of version 3 reads version 1 and 2 files as they are: a packed snapshot written by
  0.1.x or 0.2.x loads, and a store created from it keeps its older manifest and segment files
  byte for byte (8, 12.1). Every file carries its own version: a snapshot of file format version
  3 MAY reference (6.2) or a packed snapshot MAY contain segment files of versions 1 and 2, and
  a manifest of an older version MAY be rewritten as version 3 (its content is valid in both).
  A file of version 3 is not readable by a reader of version 2: Graphersal 0.2.x refuses a
  store, backup or packed snapshot holding one as written by a newer version (unsupported
  version, below), never as damage, because it checks every file header first (it does so since
  0.1.1). That is why file format version 3 needs no new store format version, unlike version 2
  (whose readers, 0.1.0, took an unknown file version for damage). The container is still at
  version 1.
- A writer writes only the current version.
- A file with a newer version than the reader knows MUST be refused (unsupported version),
  never guessed at. A reader of file format version 3 MUST accept versions 1, 2 and 3 in every
  file header (manifest 6.1, segment 7.1, packed snapshot 8, WAL segment 9.1).
- **A version from the future is not damage, and a store holding one MUST NOT be worked on.**
  A file whose magic matches and whose header checksum is valid (the `GRAPH` fixed part, 5.1; a
  manifest fixed-part copy, 6.1; a segment or WAL segment header, 7.1, 9.1) but whose version
  word is above the reader's is "written by a newer version". The reader MUST refuse the whole
  operation with an unsupported-version error and MUST NOT classify it as damage (13). A store,
  backup or single-file store holding even one such file MUST NOT be worked on in any way: no
  write, no lock, no maintenance mode, no salvage or repair, no upgrade (15.1), no checkpoint,
  no backup into or out of it, so a reader can never corrupt what a newer writer wrote. A
  single-file container (14.1, whose preamble has no checksum) is newer when its magic matches
  and its container version is above the reader's. A header whose checksum fails stays damage.
  Every operation on an existing store, backup or attic checks, before it takes a lock, reads
  data or writes: the container version, then both `GRAPH` copies (either one newer refuses the
  store, whatever the other holds), then the headers of every manifest, snapshot segment and WAL
  segment of the tree (snapshots, holders, `tmp-*` directories and attic entries); a header it
  cannot read is left to the operation's own checks. A restored ZIP backup that is refused this
  way is removed again. (The reference implementation does so since 0.1.1.)
- **Format versions move only with a release that needs them.** A patch release (x.y.Z) MUST
  NOT change any format version (store, file or container). A format version changes only with
  a release whose new features need it; a release that changes no format reads and writes the
  stores of the release before it as they are, with no upgrade.
- A `GRAPH` of an OLDER store format version (its magic and fixed-part checksum valid, the
  version word below the current one) MUST be classified by its version word before any field
  that version did not have is checked: it is neither damage (13) nor "not a store". Every
  operation refuses it with "older format" (open, read-only open, info, verify, backup, restore
  of a backup directory, fork, conversion), except the upgrade (15.1), the repair (which reads a
  store of a released older version as upgraded and writes the repaired store in the current
  version) and the restore of a ZIP backup (which upgrades what it unpacks).
- **Every release reads every released version.** A reader of store format version 3 MUST read
  the files of store format version 2 (Graphersal 0.1.0) and of file format version 1, and a
  later version MUST keep reading them; a store of a released older version is upgraded without
  the build that wrote it (15.1). Store format version 1 was never released: a store of it is
  migrated through a packed snapshot exported by the build that wrote it, from which the current
  build creates a new store (only the graph state, schema and catalog move; marks, the attic and
  backups do not).
- New value tags, record types, mutation ops, `INTENT` operations and flag bits require a new
  version, except flag bits that a reader may ignore without misreading the store (a writer
  preserves unknown bits it read).

### 15.1 Upgrading a store

Which store format versions an implementation upgrades, and how, is an explicit table; an
upgrade not in it is refused before anything is written. The reference implementation's table
(generated from its code):

<!-- upgrades:begin -->
| From store format version | Written by | To | Upgrade |
|---|---|---|---|
| 1 | never released (no store id) | 3 | unsupported (packed snapshot) |
| 2 | Graphersal 0.1.x | 3 | in place or into a copy (version word) |
| 3 | Graphersal 0.2.x and later | 3 | nothing to do |
<!-- upgrades:end -->

"Version word" means only the version word of `GRAPH` and `GRAPH.copy` (and their checksums)
changes, which is safe in place; "into a copy only" means the upgrade writes a new store and an
in-place upgrade is refused; "unsupported" means the migration through a packed snapshot
(below). A store holding a file of a newer version is never upgraded (15).

An upgrade converts a store of store format version 2 (a store, a backup directory, either
backend) to version 3. Version 3 has the layout of version 2 and every file of version 2 is a
valid file of version 3 (its own header names its file format version, 1, which a version 3
reader reads), so an upgrade:

1. Takes the writer lock of the store and refuses it while held (an older writer has it open).
2. Verifies the store as version 3 would read it (12.6's checks: both `GRAPH` copies, every
   snapshot, every WAL record and the segment set of 9.8, the attic) WITHOUT writing: the
   `GRAPH` copies are read with the version word replaced by 3. Any problem refuses the upgrade
   (repair it instead, 13).
3. Rewrites `GRAPH.copy`, then `GRAPH` (each atomically, 5.3) with version 3 and every other field
   unchanged (lineage, identity, flags, the backup marker, `wal_head`, `closed_at`, `wal_cut`).
   No other file changes, so no `INTENT` is needed: after a crash between the two writes the
   copies differ by the version word only and a version 3 reader uses the valid copy (5.3);
   repeating the upgrade rewrites both. A version 2 reader refuses either state as newer.

An upgrade into a NEW location copies every file of the store (as a conversion does: name by name, without the
locks and temporary files) after step 2, writes a marker `upgrade-in-progress.tmp` into the
target first, verifies the copy with its new `GRAPH` held in memory, then writes `GRAPH.copy`,
`GRAPH` and removes the marker: until `GRAPH` is written the target is not a store, and a target
holding the marker is an interrupted upgrade that the next upgrade into it clears. Unlike a
conversion, the copy keeps the store id: it is the same store in the new version.

After an upgrade the files of file format version 1 stay where they are: snapshots, WAL segments
and the attic keep their bytes, so the history is exactly what was written. A writer MUST NOT
append to a WAL segment of an older file format version: when the newest segment of the
lineage is older, the next record goes into a new segment of the current version (9.8:
`wal_head` names it); a segment of an older version that holds no record (it starts at the next
commit) is replaced whole by one of the current version under the same name.

A new FILE format version alone needs no upgrade and no table row: a store of store format
version 3 written by 0.2.x (files of version 2) is opened and continued as it is by a reader of
file format version 3; its version 2 files stay, the files written from then on are of version
3 (a new WAL segment, new snapshots), and from then on 0.2.x refuses the store as newer (15).
- New definition kinds (6.3), new payload keys of a known kind, new segment kinds (7.5) and
  named extra files (8) do **not** change the version: the rules of 16 tell every reader how to
  treat what it does not know.

## 16. Extensibility

The format is meant to grow without migrations. These rules bind every reader and writer of
store format version 3 and file format version 3:

1. **Length before payload, payload as a map.** Every definition carries its length before its
   payload (6.3), so a reader skips an unknown kind by its length; a payload is a property map,
   so a reader ignores unknown keys of a known kind. A new field is a new key, never a new
   layout. Payload keys are never `kind`, `name` or `flags` (6.3).
2. **Unknown, not critical: kept.** A definition of an unknown kind without the critical flag
   is kept as opaque bytes (kind, flags, name, payload) and written back unchanged at the next
   checkpoint and in every copy (fork, backup, ZIP, repair), so an older reader never drops a
   newer definition. Unknown keys of a known kind are kept the same way (their values, written
   after the known keys in the order read).
3. **Unknown and critical: read-only.** A critical definition of an unknown kind, or a file of
   a critical unknown segment kind (7.5), must stay consistent with the data (an index, for
   example); a writer that does not maintain it would corrupt it. A store holding one opens
   read-only for that reader (12.1), and a writer never stores such a definition itself (9.3).
4. **One WAL op for every kind.** Op 10 (9.3) carries every definition change; a new kind needs
   no new op and no new version. Its replay follows rules 2 and 3.
5. **Large data in files of its own.** The manifest is read whole and bounded at 256 MiB: a
   feature with large data stores a definition in the catalog plus files in the snapshot
   directory, referenced from the segment table by a new segment kind (7.5) with the same
   critical/ignorable rule; packed snapshots carry such a file as a named extra file (section
   kind 5, 8).
6. **Two version axes.** The format version (the byte structure; with rules 1-5 it rarely
   moves) and content versions such as a saved query's `dialect`: a DSL evolves by compiling the
   stored text, never by re-encoding files.
7. **Upgrades without the older build.** Every release reads every released store and file
   format version (15), so a store of a released older version is upgraded by the current build
   itself (15.1); a store format change keeps that possible. A packed snapshot of an older file
   format version loads as it is.
8. **Host kinds.** Definition kinds 128-255 (6.3) belong to hosts, the applications built on a
   library that implements this format; the format never assigns them, so a host entity never
   collides with a future library kind. To the library a host kind is an unknown kind: kept as
   opaque bytes under rules 2 and 3 (a critical one opens the store read-only), never damage. A
   host SHOULD encode its payload as the property map of 6.3 (the reference implementation
   offers that codec publicly), so the payload stays inspectable and extensible by key.
9. **An unknown value of a known key is not damage.** Damage is what a checksum, a length or
   the byte structure (value tags, record types, mutation ops, guarded by the format version,
   15) says is wrong. Content behind a valid checksum that the reader cannot interpret was
   written by a newer writer, and a reader MUST NOT report it as damage or offer a repair that
   would drop it (a repair by a reader that cannot read it loses it):
   - in a definition payload (6.3): kept and not served (an unknown value it can represent), or
     kept opaque (a payload it cannot read as its kind at all); a critical one opens the store
     read-only (rule 3);
   - in the graph schema (`schema.json`, 6.1; a schema image of op 9, 9.3): the schema changes
     what writes are allowed, so it is never ignored. A reader opens the store READ-ONLY (the
     data without the schema; nothing is written, not even a torn-tail cut) and refuses every
     operation that would write a state derived from it (a checkpoint, a fork, a repair) as
     unsupported; a backup copies its bytes as they are; verify reports it as a note, not a
     problem;
   - an `INTENT` operation (11.1) or a `GRAPH` state value (5.1) the reader does not know, and a
     well-formed entry outside its layout in a backup archive (12.8): refused as unsupported,
     never damage.

## 17. Conformance notes

**Readers**

- Check every CRC before using the bytes it covers; check every count and length against the
  remaining input before allocating (2).
- Never treat damage as a torn tail: apply 9.6 and 14.4 exactly. Never skip an unknown record
  type, value tag or mutation op.
- Keep what you do not know (16): an unknown definition byte for byte, unknown payload keys of a
  known kind, an unknown value of a known key (not served, written back as read), an ignorable
  file of an unknown segment kind in every file-level copy; open a store with something critical
  you do not know, or a graph schema you cannot parse, read-only. Content behind a valid
  checksum that you cannot interpret is never damage (16 rule 9).
- Replay only files whose `graph_id` is on the lineage chain, each ancestor only up to its branch
  point (5.4), and require commit continuity (9.7).
- A read-only reader of a store MUST NOT write anything into it, take the writer lock, or cut a
  torn tail. It SHOULD take the shared backup pin while it copies files, so that a prune cannot
  remove them meanwhile.
- A reader tailing a live WAL (CDC) reads only complete records with valid CRCs and treats an
  incomplete record at the end as "not yet written". Records before the writer's durable end are
  final; a record after it may still be cut off (9.7).
- Read every file with the version in its own header (15): accept files of format versions 1
  to 3 in one store or packed snapshot, reject a temporal value tag in a version 1 file and a
  nanosecond temporal value tag (15 to 20) in a version 1 or 2 file, and reject a temporal value
  outside its range (3) as damage; a zone name you do not know is unsupported content (3).

**Writers**

- Write every new file with the current file format version (3) and `GRAPH` with store format
  version 3; never append a record to a WAL segment of an older file format version (start a new
  segment, 15.1), and never write a nanosecond temporal tag into a file of version 1 or 2.
- Follow the write ordering of 9.9: a WAL never holds a commit that did not happen, and a caller
  never gets "ok" for a commit that is not durable under the configured durability.
- Make files visible only when complete: a temporary name, sync, an atomic rename, a sync of the
  directory. Verify a snapshot before it is renamed (12.3).
- Write `GRAPH` as specified in 5.3, `wal_head` as in 9.8, and every multi-file change under an
  `INTENT` with idempotent steps (11.1).
- Never remove WAL or snapshots implicitly: only an explicit prune (12.4), a rollback (12.5) or
  removal of an attic entry removes history.
- Record only store-relative names (4.2).
