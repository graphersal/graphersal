# In-Memory Storage Internals

This page describes how `TraversalGraph`, the in-memory reference storage, lays out element
properties and the ID index, for contributors working on `crates/graphersal/src/graph/` and
`value/property_map.rs`. None of it is public API: the `GraphStorage` trait, `PropertyMap`'s
methods, `Debug` text, equality, serde and the persist format are the same whatever the internal
representation. A [custom storage](custom_storage.md) is not affected by anything here. What a
user sees of it (`memory_usage()`, `shape_bytes`, the advice about property order) is on
[Memory Inspection](../users_guide/memory_and_profiling.md#how-property-memory-works).

## Slots and adjacency

Vertices and edges live in two vectors of slots (`graph/topology.rs`, an own store that replaced
petgraph's `StableGraph` and keeps its behaviour). A slot index never changes while the element
lives: a removed element leaves its slot vacant, and the next insertion of that kind reuses the
most recently freed slot first, else appends. `g.V()` and `g.E()` walk the slots in order, which is
why their order is stable but not insertion order after removals.

Each edge slot holds its source and target and two links: the next edge of the source's outgoing
list and the next edge of the target's incoming list. A vertex slot holds the heads of its two
lists. A new edge goes to the front of both lists, so `out()`/`in()` see the newest edge first; a
self-loop is in both lists of its vertex. Removing an edge unlinks it from both lists; removing a
vertex removes its edges first.

The vectors grow by doubling, so a graph built element by element can carry up to half its slots
as spare capacity. A snapshot load reserves the slots from the manifest's counts, and every load
(snapshot, Store, GraphSON, GraphML) ends with `shrink_to_fit`, which releases the spare capacity
(vacant slots stay: indices are stable). The memory report shows `live`, `tombstoned` (vacant) and
`spare` slots per kind.

## Property maps: `Indexed` and `Shaped`

`PropertyMap` is a newtype over a crate-internal `Repr` with two variants:

| Variant | Holds | Used for |
|---|---|---|
| `Indexed` | an `IndexMap<CompactString, ElementProperty>` (FNV) | every free-standing map: nested objects, catalog payloads, change sets, results, schema values, every map a caller builds |
| `Shaped` | `Arc<Shape>` (the ordered key list, shared) + `Vec<ElementProperty>` (this element's values, in key order) | every map a `TraversalGraph` stores per vertex or edge |

An `IndexMap` slot costs a cached hash, the key and the value (64 bytes) plus its share of the hash
index and spare capacity, about 84 to 132 bytes per property. A `Shaped` slot is the value alone
(32 bytes); the keys and their lookup structure are paid once per distinct key sequence. Elements
with the same keys in the same order share one `Shape`, like the hidden classes of a JavaScript
engine. Insertion order is externally visible (`valueMap()`, `elementMap()`, `properties()`) and is
kept by the shape.

A `Shape` holds its keys, one `KeyPrint` per key (length plus the first and last eight bytes,
exact for keys up to 16 bytes), the hash of the key sequence and the id of the shape table that
interned it. A key lookup in a shape of up to 16 keys scans the prints, which avoids a `memcmp`
per candidate; a wider shape keeps an `IndexSet` of its keys. The values vector is exactly sized
when built or grown; a removed key keeps its slot as spare capacity, so a drop followed by an add,
or the rollback of either, reallocates nothing.

Both variants behave identically through the public API. A key-changing mutation of a `Shaped` map
outside the graph (a clone handed out to a caller who then inserts a new key) first converts it to
`Indexed`; a value-only mutation keeps it `Shaped`. Crate code never assumes a representation: it
uses the positional helpers (`insert_full`, `shift_remove_full`, `get_full`, `shift_insert`,
`insert_if_absent`, `indexed_mut`). The hot read paths match `Indexed` first and keep the `Shaped`
arm out of line, so free-standing maps pay nothing for the second variant.

## The shape table

Each `TraversalGraph` owns a `ShapeTable` (`graph/shape.rs`): a `hashbrown::HashTable` of the
interned non-empty shapes, keyed by the hash of the key sequence. Interning a key sequence that is
already known allocates nothing; the last shape interned is tried before the table.

**Transitions.** A key-adding write moves a map to the next shape through a cache
`(shape, position, key) -> shape`; a key-removing write through `(shape, position) -> shape`. An
added key goes to the end; the undo log puts a removed key back at its old position, which is why
the insertion transition carries a position. Both caches sit behind one-entry memos (a bulk write
repeats the same transition) and use a cheap word-mixing hasher over the shape address, the
position and the key.

**Choke points.** Every map enters the graph through `insert_vertex_raw` / `insert_edge_raw`
(`graph/undo.rs`), which `adopt` it: an `Indexed` map is converted (keys interned, values moved
into an exact vector), a `Shaped` map of another table gets this table's shape for the same keys,
and a map of this table is left alone. Since a shape carries its table's id, that last check is one
integer comparison. Loads, replay, undo restores, damage repair, checkpoint merges, imports and
`add_*` all go through these two functions. Every in-graph key-changing write goes through
`ShapeTable::insert_full`, `shift_remove_full` or `shift_insert`, reached through
`TraversalGraph::with_vertex_map` / `with_edge_map` (which hand the table and the element's map
out side by side). A write through a generic map helper (the jpath writes) is followed by `adopt`.
A map that missed adoption is still correct, it only does not share its keys.

**Sweep.** `ShapeTable::sweep` removes every shape that only the table still refers to (strong
count 1) and clears the transition caches and memos, which are keyed by shape address. It runs when
a new shape is interned and the table has doubled since the last sweep (at least 64 shapes), on
`shrink_to_fit`, and at the end of a load (`finish_load`, the GraphSON and GraphML readers). It
never runs under `&self`: reads never touch the table. A shape that anything still refers to (a
stored map, an element image in the undo log, a clone in a result) is never removed, so a shape
carrying this table's id is always in the table.

**Undo.** The undo log stores element images and old values with their positions, not handles or
shapes. Overwriting an existing key is a value-only change and restores in place. Rolling back an
added key removes it again; rolling back a dropped key puts it back at its old position through
`shift_insert`, so the map returns to the very shape it had (`restore_stored_property`). A dropped
element comes back through `insert_vertex_raw` / `insert_edge_raw` with its image, whose shape is
still this table's (the image kept it alive), so adoption is the one comparison.

**Snapshot decode.** A snapshot chunk has its own string table, and its elements repeat a few key
sequences. The decoder (`persist::codec::ChunkShapes`) builds each element map `Shaped` directly,
keyed by the sequence of key references, without building an `IndexMap` and without hashing a key
string. These chunk-local shapes belong to no table; the graph re-interns them on `adopt`, and a
one-entry memo (`last_foreign`) makes a run of elements with the same chunk shape cost one pointer
comparison each.

**Imports and change sets.** The GraphSON and GraphML readers do the same without a string table:
they collect each element's keys and values in order in a `MapBuilder` (`graph/shape.rs`; a
repeated key keeps its first position and its last value, as `PropertyMap::insert` does) and
intern the key sequence in an importer-own table, one per element kind, so the graph adopts the
maps through the same `last_foreign` memo. `TraversalGraph::apply_change_set` and every replay
path copy each added map straight into the graph's own table (`ShapeTable::shaped_copy`); other
storages receive ordinary maps. Coercion, validation, compression, property indexes and change
capture run inside `insert_vertex`/`insert_edge` as for any write.

**Memory accounting.** `MemoryStats::shape_bytes` is `ShapeTable::heap_bytes`: every interned
shape once (its `Arc` allocation, keys and prints, heap-allocated keys, a wide shape's index) plus
the table's and the caches' hash tables. A stored map adds only its value vector (by capacity) and
its values' heap to `property_bytes`; `shape_bytes` is included in `property_bytes`.

## The ID index

The ID index (`graph/index.rs`, `GraphIndex.nodes` / `edges`) maps an element id to its handle
without storing the id a second time. Each table is a `hashbrown::HashTable<(Handle, u32)>`: the
handle plus a cached 32-bit FNV-1a hash of the id, 12 bytes per bucket and one control byte. The
id itself lives once, in the element's weight in the graph.

- **Lookup** probes with the cached hash and confirms a candidate by reading the weight at the
  handle's slot: the slot must be occupied, carry the handle's generation, and hold the id asked
  for. Lookups therefore take the graph as an argument.
- **Removal** matches by handle and reads nothing from the graph.
- **Growth** re-places entries from the cached hash alone (the probe hash is derived from the
  32-bit value), so growing the table during a bulk load does no random reads into the graph.

The invariant that makes this sound, kept by the raw insert and remove helpers of `graph/undo.rs`
(the only writers of the ID index):

- every handle in the ID index points to a live slot whose weight carries the handle's generation
  and the id the entry was hashed from;
- an element's id never changes after insert.

An entry is inserted after its slot's generation is patched and removed by handle right after the
element left the graph. Cloning the index together with the graph keeps it valid, since a
topology clone keeps every slot. Nothing iterates the ID index; it serves point lookups only,
so it keeps no insertion order.

## Measured effect

On a test graph of 1 million vertices and 3 million edges with 36-character UUID ids:

| Change | One property per vertex | Six properties per vertex, two per edge |
|---|---|---|
| Before both | 1.61 GB live heap | 2.56 GB |
| ID index without the second id copy | 1.29 GB (-20 %) | 2.24 GB |
| Shared key lists (shapes) | 1.09 GB (-15 %) | 1.43 GB (-36 %) |

Building such a graph through the public `insert_vertex` / `insert_edge` became 5 to 12 % slower
(adopting a caller-built `Indexed` map costs one allocation and two frees per element); a lookup
followed by a property read, what `g.v(id).values(..)` does, became faster.
