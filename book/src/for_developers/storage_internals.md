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

A vertex slot holds its id, a number for its label set, its property map and its slot
generation (96 bytes on a 64-bit target); an edge slot its id, a number for its label, its
property map, the generations of itself and its endpoints and the list links (112 bytes). The
labels live once per graph in a label table (`graph/labels.rs`): every distinct edge label and
every distinct ordered vertex label SET, so a vertex with labels `person, admin` costs 4 bytes in
its slot, not a list of strings, and `vertex_labels()` still returns a slice. A materialized
element (a vertex or edge value in a result) keeps its stored map's shape: the copy is the shared
key list and the values, no key is hashed.

The vectors grow by 1/8 of their capacity at a time once they hold more than 512 slots (below,
by doubling, as `Vec` does; `topology::growth_step`), so a graph built element by element carries
at most about 1/9 of its slots as spare capacity, and the first insert after a load adds 1/8 of
the slots instead of doubling them (the generations and the label buckets' entries follow the
same rule; the id index and the label buckets' hash tables are power-of-two tables and keep
their own spare room after a load). The growth stays geometric, so an insert is amortized O(1).
A snapshot load reserves the slots from the manifest's counts, and every load
(snapshot, Store, GraphSON, GraphML) ends with `shrink_to_fit`, which releases the spare capacity
(vacant slots stay: indices are stable). The memory report shows `live`, `tombstoned` (vacant) and
`spare` slots per kind.

## Property maps: `Indexed`, `Shaped` and `Coded`

`PropertyMap` is a newtype over a crate-internal `Repr` with three variants:

| Variant | Holds | Used for |
|---|---|---|
| `Indexed` | an `IndexMap<CompactString, ElementProperty>` (FNV) | every free-standing map: catalog payloads, change sets, results, schema values, every map a caller builds; a stored nested object whose key sequence was seen once or that sits deeper than eight levels |
| `Shaped` | `Arc<Shape>` (the ordered key list, shared) + `Vec<ElementProperty>` (this element's values, in key order) | every map a `TraversalGraph` stores per vertex or edge, and the objects nested in its values (see Nested maps below) |
| `Coded` | `Arc<Layout>` (shape, per position own value or code, the `dedup` rules' dictionaries, shared) + `Box<[ElementProperty]>` (own values) + `Box<[u32]>` (codes) | a stored map with values at the top-level keys of `dedup` rules ([Compressed Properties](../users_guide/compressed_properties.md#deduplicating-repeated-values)) |

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

A `Coded` row exists only under a `dedup` rule on a top-level key: the `Layout` (interned per
shape, coded positions and rules, in the compression rules' layout table) says for each position
whether the row holds the value itself or a 4-byte code into the rule's `ValueDictionary`
(`value/dictionary.rs`: append-only segments of `OnceLock` slots that never move, so a read gives
`&ElementProperty` without a lock; appends under a mutex; safe Rust only). A coded row never leaves
the graph: `Clone`, the owned iteration and every mutation through the generic methods turn it into
a plain `Shaped` row first; the graph's write path replaces an existing key in place (a new code,
the undo log keeping the old code with its dictionary) and runs every other write on the plain row
and codes it again (`CompressionRules::code_row`). A graph without `dedup` rules never builds one,
and the `Shaped` read path has no extra check. The own values and the codes are two boxes (no
`unsafe` single allocation, user decision 2026-11-04).

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

**Nested maps.** The objects inside a stored value (`ElementProperty::Object`, also inside arrays)
are shaped in the same table, down to eight container levels below the element's own values
(`NESTED_DEPTH`); deeper maps stay `Indexed`, so the recursive clone, comparison and drop of a
deeply nested value keep the frames of a plain `IndexMap` below that bound. A nested key sequence
gets a shape at its second sighting: a key sequence that already has a shape takes it at once, a new
one is remembered by its hash (`seen_once`, at most 65 536 hashes, cleared by `shrink_to_fit` and
so at the end of every load) and interned when it comes again. A map whose keys never repeat (a
JSON document keyed by ids) therefore stays `Indexed`, which is smaller than a shape of its own.
Nested maps are shaped by `adopt` (a map that is not one of this table's yet), by `insert_full`
(the value written), by `shaped_copy` (change sets) and after a jpath write of the property; they
keep their own one-entry memo, so they do not evict the element maps' last shape. They never take
the transition caches: a key-changing write inside a value (through the public `PropertyMap` API)
turns that nested map `Indexed`, which stays correct and is shaped again by the next write of the
property. Two `Shaped` maps of one shape compare their value vectors directly. A snapshot chunk
builds a nested map `Shaped` over a shape of its own from the second sighting of its key sequence
in the chunk (no `IndexMap`); the graph swaps in its own shape when it stores the value, by the same
second-sighting rule, and turns the map `Indexed` in the rare case the rule says the graph has not
seen the sequence before, so the stored form does not depend on how the chunk was decoded. On a
real code index graph (534k vertices, 2.3M edges) nested shapes save 91 MiB of property memory
(7.7 %) and the open takes about 1 % longer; the snapshot bytes are the same except the header's
memory estimate.

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
