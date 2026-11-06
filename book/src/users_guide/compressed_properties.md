# Compressed Properties

Large text values that queries rarely read (a CV, a product description, a document body) can take
most of a graph's memory. A **compression rule** names where such values live (an element kind, a
label and a property path) and the graph keeps the strings there **LZ4-compressed in memory**. A
traversal that reads one gets the **plain** string, decompressed on the fly; the graph never holds a
decompressed copy. Nothing else changes: queries, results, change sets, exports and files see plain
strings only.

```rhai
// Strings of `customer` vertices at `details.career.cv` (and their `bio`) are kept compressed.
g.define_compression(#{name: "customer_cv", label: "customer", path: "details.career.cv"});
g.define_compression(#{name: "customer_bio", label: "customer", path: "bio", min_bytes: 512});
// An edge rule.
g.define_compression(#{name: "order_notes", element: "edge", label: "bought", path: "notes"});

g.v().has_label("customer").values(jpath("details.career.cv")).limit(1)   // plain text
g.compressions()                                                         // the rules and their stats
g.memory_usage()                                                         // compressed_values, ...
```

Long values that **repeat** across many elements (file paths, URLs, addresses, category or status
texts) are a different case: a rule with `codec: "dedup"`
[stores each distinct value once](#deduplicating-repeated-values) and lets every element point to
it.

Not sure a rule pays off, or which `min_bytes` to take? [Estimate it first](#estimating-the-gain):
`g.estimate_compression(#{label: "customer", path: "bio"})` measures the candidate rules on the
current data without changing anything.

## When it pays off

- Long strings (hundreds of bytes and more) that are written once and read seldom: CVs,
  descriptions, notes, logs, document bodies. Typical text compresses to 40 to 55 % of its size
  (measured below); highly repetitive text much further.
- Not for short strings (names, codes): a value is compressed only when it is at least `min_bytes`
  long (default 256) **and** its compressed form is at most 90 % of the plain size; everything else
  stays plain.

## The cost model

- **A read decompresses.** `values()`, `value_map()`, `element_map()`, `project().by()`,
  `order().by()`, a jpath read, a result: each decompresses the value it reads (about 1.5 to 3 GB/s,
  below). A read that never touches the field costs nothing.
- **A filter on such a field decompresses every candidate.** `has("bio", TextP.containing("Rust"))`
  reads the text of every vertex it tests. Filter on other properties first. A
  [property index](property_indexes.md) on the field serves equality and ranges without reading
  the candidates, but it holds the plain text (the memory compression saved).
- **A write compresses** a string at a rule path (with the rule's dictionary). Only the touched
  top-level property is looked at; with no rule for the element's labels a write costs one lookup.
- **The disk stays plain.** Snapshots, the write-ahead log, GraphSON and GraphML hold the plain
  text: the persistence layer compresses whole chunks and large WAL records already, compressing
  single values twice would only cost CPU. A load compresses the rule paths again in one pass
  (in parallel threads where the platform has them; `dedup` keys are coded while the elements
  are read, see "Deduplicating repeated values").

## Rules

A rule is a definition of kind `compression` in the graph's catalog, next to the saved queries. It
has:

| Field | Meaning | Default |
|---|---|---|
| `name` | an identifier, unique among the rules | required |
| `element` | `"vertex"` or `"edge"` | `"vertex"` |
| `label` | the label the rule applies to (it need not exist yet) | required |
| `path` | a property name or a jpath of names: `"cv"`, `"details.career.cv"`, `jpath("$['a b'].c")`; no index (`[0]`, `[-1]`): arrays are not compressed in this version | required |
| `codec` | `"lz4"` (compress each value) or `"dedup"` ([store each distinct value once](#deduplicating-repeated-values)) | `"lz4"` |
| `min_bytes` | strings shorter than this (UTF-8 bytes) stay plain; 1 to 16 MiB (`lz4`), 0 to 16 MiB (`dedup`, strings only: a dedup rule on a top-level key codes every other type) | 256 (`lz4`), 0 (`dedup`) |
| `dictionary` | build a dictionary from the rule's values (`lz4` only; `true` with `dedup` is refused) | `true` (`lz4`), `false` (`dedup`) |

- **Any number of rules**: several paths on one label, the same path on different labels, vertex
  and edge labels. One rule per element kind, label and path: a second one is refused
  (`DefinitionError::Conflict`, its help names `drop_compression`).
- **Several labels of one vertex**: every label's rules apply; when two labels have a rule for
  the same path, the first label in label order wins (the same rule as schema coercion).
- A **label change** (`add_vertex_label`/`remove_vertex_label`, `set_edge_label`) compresses what a new label's rules
  cover and decompresses what no rule covers any more.
- An `lz4` rule compresses only **strings**: a number, an object or an array at its path stays
  as it is. A `dedup` rule on a top-level key takes every value type (below).

### The explicit pass

Defining a rule, redefining it with other parameters, dropping it and loading the catalog run the
**pass** over the rule's label in the same commit: every value at the old and the new path is
re-encoded (compressed when it qualifies, decompressed when it no longer does). `g.recompress(name)`
runs it again on demand and rebuilds the dictionary, for example after much data was added to a
rule that was defined on an empty label. The pass changes no data: a commit hook sees only the
`SetDefinition` of the rule, and a `recompress` alone commits nothing.

### Dictionaries

With `dictionary: true` the pass builds a dictionary from a sample of the rule's current values (in
id order, up to 1 KiB of each value, up to 64 KiB, the LZ4 window) and compresses with it, which
helps short and similar texts most. A dictionary is in memory only: a load builds it again. A value
written before a dictionary exists (a rule on an empty label) is compressed without one until the
next `recompress` or load. Every compressed value carries its dictionary, so it stays readable when
the rule changes.

## Deduplicating repeated values

A rule with `codec: "dedup"` keeps **one copy of every distinct value** at its path, and every
element whose value is that one keeps only a reference to it. Reads stay plain and cost nothing
extra (the value is read in place, nothing is decompressed); query results, change sets, exports
and files never change, and the files keep their format (plain values; a load shares them
again).

```rhai
// Every `symbol` vertex's `path`, `kind` and `visibility` (a few hundred distinct values over
// 100,000 symbols).
g.define_compression(#{name: "symbol_path", label: "symbol", path: "path", codec: "dedup"});
g.define_compression(#{name: "symbol_kind", label: "symbol", path: "kind", codec: "dedup"});
g.compressions()[0]          // compressed_values, distinct_values, ...
```

**On a top-level property** (a path of one name, the common case) a dedup rule takes **every
value type**: strings of any length, numbers, booleans, dates and times, UUIDs, arrays, objects,
vectors and null. The rule keeps a **dictionary** of the distinct values; the element's row
holds a 4-byte **code** instead of the 32-byte value slot, and the value's heap (a long string's
text, an array's elements, an object's entries) exists once, in the dictionary. Two values share
an entry only when they are **identical**: the same type (the int `1` and the float `1.0` are
two values), floats by their bits, a `datetime` with its offset and zone (one instant at two
offsets is two values), arrays element by element and objects entry by entry in their key order.
`min_bytes` (default 0) applies to strings only: a shorter string stays in the row; every other
type is coded whatever its size.

**Under a nested path** (`loc.file`) a dedup rule shares **strings** only, and only those longer
than 24 bytes and at least `min_bytes` (a string of at most 24 bytes is stored inline in its slot
and owns no heap to share): the value inside the object keeps its slot and points to the one copy
of its text in the rule's table.

**When it pays**: values repeated across many elements: file paths, URLs, kinds, states, flags,
categories, dates, small lists of roles or line numbers. Measured on a code knowledge graph: the
`path` of its symbols took 1.17 MB of heap for 816 distinct values, about 40 KB deduplicated; on a
larger one (2.8 million elements) the advisor's dedup rules on 130 keys saved about 30 % of the
property memory. Each coded value saves 28 bytes of its slot plus its heap; each distinct value
costs its entry (52 bytes) and its heap once.

**When it does not**: **mostly distinct values** (ids, hashes, descriptions, CVs): every
distinct value costs its entry, so a key whose values rarely repeat saves little or costs memory.
The estimate warns with `high_cardinality`; for long unique text use `lz4`. The
[advisor](optimize.md) recommends a dedup rule only where at most one value in eight is distinct
and it saves at least 1 MiB.

**The cost**: a write of a coded key looks the value up in the dictionary (a hash of the value).
Measured at query level on a real graph: overwriting a deduplicated key costs about 5-9 % more
than a plain write; **adding a key that is new to an element** whose row is coded, and a
**jpath write into a deduplicated value**, about 30 % more. Reads stay plain and are mostly
faster (a filter on a coded key compares codes). A load codes the rule keys while it reads the
elements: a snapshot holds its rules before its elements, so each row is coded as it enters the
graph, straight into the dictionaries, and no plain copy of a coded value is kept until the end
of the load. Opening a store with rules is then about as fast as opening it without them
(measured on a real graph of 2.8 million elements with 74 dedup rules and two lz4 rules,
`--release`, the embedding model loaded: 4.9-5.1 s with the rules; earlier versions took
5.2-5.3 s without the 74 dedup rules and 6.6-7.0 s with them).

The rule's numbers: `compressed_values` counts the values the rule holds (coded or shared, one
per element), `distinct_values` the distinct values among them, `plain_bytes` and `stored_bytes`
what they take without and with the rule. On a top-level key `plain_bytes` is each value's slot
(32 bytes) plus its heap and `stored_bytes` each value's code (4 bytes) plus every distinct value
once (its dictionary entry, 52 bytes, plus its heap); under a nested path `plain_bytes` is the text
of each value and `stored_bytes` every distinct text once plus 32 bytes for it (a reference costs
nothing beyond the slot a plain string occupies too). `saved_bytes` of an estimate is
`plain_bytes - stored_bytes`.

A dictionary only grows while the graph runs; a value no element holds any more (overwritten,
its element dropped) is removed when the dictionary is **rebuilt**: at the end of a transaction
once it holds max(256, twice its entries at the last rebuild, an eighth of the label's elements)
entries, after a rolled-back transaction that added values, on `g.recompress(name)`, on
`g.shrink_to_fit()` and on a load. A nested path's table is swept the same way when it has doubled
since its last sweep. `g.memory_usage()` reports `deduped_values`, `deduped_plain_bytes` and
`dedup_table_bytes` (the dictionaries, the coded rows' layouts and the nested paths' tables, every
value once, part of `property_bytes`).

`lz4` and `dedup` are one rule per element kind, label and path: redefining the rule with the other
codec re-encodes its values in the same commit.

> **Warning (stores written by this version):** a dedup rule with `min_bytes` 0 (the default since this version) cannot be
> read by Graphersal 0.5.2 and older: they report a store holding one as **damaged**. Upgrade
> the engine before opening such a store; this version reads every older store and its rules
> (`min_bytes` 25) as they were written. Rules defined by 0.2.1 to 0.5.2 code their non-string
> values too once this version loads them; strings shorter than their `min_bytes` stay in the
> row.
>
> **Warning (0.2.0):** Graphersal 0.2.0 does not know the codec `dedup` and reads a store that
> holds a `dedup` rule as **damaged** from the commit that defined it on (maintenance mode; its
> repair would drop those commits). Do not open such a store with 0.2.0, and never run
> `graphersal store repair` from 0.2.0 on it; use 0.2.1 or later.

## Several rules at once

Defining many rules one by one walks the elements of their labels once per rule and commits
once per rule. A **list of ordinary rules** is stored in **one unit**: one commit (one WAL
record), and **one walk** over the elements of all their labels re-encodes them for every rule
together. Measured on a real graph (2.8 million elements, the advisor's 74 dedup rules, 11.9
million values coded, `--release`): one by one 8.6 s and 74 commits (13,932 bytes of WAL), as
one list 1.5 s and one commit (2,210 bytes of WAL), with the same values coded and the same
memory after a reopen. There is no wildcard or template rule:
the list holds ordinary rules, each stored, listed, dropped and persisted like one defined
alone. It is all or nothing: a refused rule (an invalid one, a conflict with a stored rule or
with another rule of the list) fails the whole list with its error, which names the rule, and
nothing is stored. Undoing such a unit (a rolled-back outer transaction) re-encodes in one walk
as well.

```rhai,graph=modern
g.define_compressions([#{label: "person", paths: ["name", "age"], codec: "dedup"}, #{name: "software_lang", label: "software", path: "lang", codec: "dedup"}])
```

Each item is `define_compression`'s map, or the short form `#{label, paths: [..], element?,
codec?, min_bytes?, dictionary?}`: one rule per path, each named `<label>_<path>` (the path's
names joined by `_`, `_edge` added for an edge rule, characters a name may not hold replaced by
`_`), the advisor's naming. A short form has no `name`. The call answers the number of rules
stored and asks for `Create` (`Update` for a name already stored) on each rule before anything
changes.

The [advisor](../persistence/optimize.md#applying-several-at-once) groups its recommendations the
same way (`dedup: 74 keys, total -400.1 MiB`) and applies a picked group in one commit too.

## A rule from a newer version

A store (or a catalog file, a change set) written by a newer Graphersal may hold a rule whose
`codec` or `element` this version does not know, or whose `path` it cannot parse. Since 0.2.1 such
a rule is **never damage**: it is kept in the catalog exactly as read and written back unchanged
(its payload byte for byte through the journal, checkpoints, backups, forks and snapshots), but it
is **not applied**: its values stay plain in memory, as they are on disk anyway. `g.compressions()`
(and the playground, the dev server and MCP's `list_compressions`, Python's `compressions()`) shows
it with the codec, element and path as read, `served: false` and a `reason` such as *codec 'zstd'
is not known to this version*; `g.recompress(name)` refuses it with that reason. It can be dropped
like any rule. A rule of a known codec on the same element kind, label and path conflicts with it
(so the newer version never finds two rules on one path). Creating a rule of an unknown codec is
refused by every front end (they take only the known codec names).

## Estimating the gain

Before defining a rule, an **estimate** measures what rules for one element kind, label and path
would hold over the current values, **without changing the graph** (no commit, no catalog change,
no value re-encoded; it only reads):

```rhai,graph=modern
g.estimate_compression(#{label: "person", path: "name"})   // short names: nothing qualifies
```

```rhai
let e = g.estimate_compression(#{label: "customer", path: "bio"});
e.candidates[e.best]          // the candidate that saves the most
// Only some candidates: each key given narrows the set.
g.estimate_compression(#{label: "customer", path: "details.career.cv", min_bytes: 512});
g.estimate_compression(#{element: "edge", label: "bought", path: "notes", dictionary: false});
// Deduplication, alone or side by side with lz4.
g.estimate_compression(#{label: "symbol", path: "path", codec: "dedup"});
g.estimate_compression(#{label: "symbol", path: "path", codec: ["lz4", "dedup"]});
```

Without `codec`, `min_bytes` and `dictionary` the **candidates** are `lz4` without and with a
dictionary at `min_bytes` 64, 128, 256, 512 and 1024, plus the parameters of the rule that covers
the path today (if any, a dedup rule too). `codec: "dedup"` measures dedup at `min_bytes` 0, 25,
64, 128 and 256 (on a top-level key counting every value type, with the dictionary's exact
identity); a list of codecs measures each codec's candidates. The answer:

| Key | Meaning |
|---|---|
| `elements`, `values_found`, `string_values`, `string_bytes` | the elements carrying the label, how many have a value at the path, how many of those are strings, and their size |
| `histogram` | the strings by size (`min_bytes`, `max_bytes`, `values`, `bytes`; buckets from 64 B to 16 KiB), to choose `min_bytes` |
| `candidates` | per candidate: `codec`, `min_bytes`, `dictionary`, `compressed_values` (the strings it would compress or share), `plain_bytes`, `stored_bytes`, `dictionary_bytes`, `distinct_values` (dedup: the distinct texts among them), `saved_bytes` (= plain - stored - dictionary; negative: the rule costs memory), `ratio`, `current` (the parameters of today's rule), `encode_bytes_per_sec` and `decode_bytes_per_sec` (the speed measured on this data: a read of a compressed value pays the decode; `()` for dedup, whose reads cost nothing extra) |
| `best` | the index of the candidate that saves the most, `()` when none saves anything |
| `current` | today's rule on that path (name, parameters and what it holds now), the baseline |
| `warnings` | `unknown_label` (no element has the label: a typo?), `no_values` (nothing at the path), `not_strings` (values a rule never compresses), `nothing_qualifies`, `no_saving`, `high_cardinality` (dedup would share values but saves nothing: they are mostly distinct) |
| `sampled`, `measured_values` | whether the candidates were measured on a sample (below) |

**The estimate is the rule.** The trial dictionary is built by the same code as a defined rule's
(the values in id order, up to 1 KiB of each, up to 64 KiB) and a value counts exactly when the
rule's write path would compress it, so the numbers of a candidate are what `g.compressions()`
reports after defining that rule on the same data. The byte counts are those of the rule's stats:
the plain and the compressed size of the values and the dictionary, not the per-value bookkeeping.

**Bounded.** The estimate compresses at most 100,000 strings and 16 MiB of plain text per
dictionary setting. Above either cap it compresses a deterministic sample (every k-th string in id
order), says `sampled: true`, and extrapolates each candidate from the exact count and size of the
strings that reach its threshold; the counts and the histogram stay exact, and so do the dedup
candidates (they need no trial compression: every string is counted). A running estimate stops
when the host cancels (the dev server's Stop, a `CancelToken`).

It reads the data of that label, so it asks for `Read` on `Data` of that element kind and label,
never for a catalog write: `AccessPolicy::read_only()`, the MCP read-only mode and a saved query may
estimate. A storage without compressed properties answers `Unsupported`.

In the playground, the rule form of **Catalog ▾ ▸ Compression** has a **Codec** choice (`lz4` or
`dedup`; the dictionary is lz4 only) and an **Estimate** button once the label and the path are
filled: a table of the lz4 and dedup candidates (with the distinct values of each dedup candidate)
with a saving bar each, the best one highlighted, the note "the graph was not changed", and **Use**
copying a candidate's codec, minimum size and dictionary into the form.

## API

| | Define | List | Drop | Recompress | Estimate |
|---|---|---|---|---|---|
| Rhai | `g.define_compression(#{..})`, `g.define_compressions([#{..}, ..])` | `g.compressions()` | `g.drop_compression(name)` | `g.recompress(name)` | `g.estimate_compression(#{label, path, element?, codec?, min_bytes?, dictionary?})` |
| Rust (`GraphTraversalSource`) | `define_compression(name, CompressionDefinition)`, `define_compressions([(name, rule), ..])`, `set_definition(Definition::compression(..))`, `set_definitions(vec![..])` | `list_definitions(Some(DefinitionKind::COMPRESSION))`, `compression_stats(name)` | `remove_definition(DefinitionKind::COMPRESSION, name)` | `recompress(name)` | `estimate_compression(&CompressionEstimateRequest)` |
| Python (`Graph`) | `define_compression(name, label, path, element="vertex", min_bytes=None, dictionary=None, codec=None)`, `define_compressions([{..}, ..])` | `compressions()` | `drop_compression(name)` | `recompress(name)` | `estimate_compression(label, path, element="vertex", min_bytes=None, dictionary=None, codec=None)` |
| Playground | **Catalog ▾ ▸ Compression**: the rules with their stats, a form with field errors and Estimate | | | | |
| Dev server | `POST /api/compression/define` | `GET /api/compression` | `POST /api/compression/drop` | `POST /api/compression/recompress` | `POST /api/compression/estimate` |
| MCP | `define_compression` | `list_compressions` | `drop_compression` | `recompress` | `estimate_compression` (also read-only) |

`g.compressions()` lists each rule with `compressed_values`, `plain_bytes`, `stored_bytes`,
`dictionary_bytes` and `distinct_values` (dedup). `g.memory_usage()` (and `MemoryStats`) adds
`compressed_values`, `compressed_plain_bytes`, `compressed_stored_bytes` and
`compression_dictionary_bytes` (lz4), and `deduped_values`, `deduped_plain_bytes` and
`dedup_table_bytes` (dedup); `property_bytes` counts the compressed (stored) size and every shared
text once.

```rust
use graphersal::prelude::*;
use graphersal::{catalog::CompressionDefinition, storage::ElementKind};

let mut graph = TraversalGraph::new();
let rule = CompressionDefinition::new(ElementKind::Vertex, "customer", "details.career.cv")
    .unwrap()
    .with_min_bytes(512)
    .with_dictionary(false);
graph.traversal_mut().define_compression("customer_cv", rule).unwrap();
```

Several rules in one unit, one commit and one walk (a storage gets the list through
`GraphStorage::set_definitions`, whose default stores them one by one):

```rust
use graphersal::prelude::*;
use graphersal::catalog::{Codec, CompressionDefinition};
use graphersal::storage::ElementKind;

let mut graph = TraversalGraph::tinkerpop_modern();
let dedup = |key: &str| {
    let rule = CompressionDefinition::new(ElementKind::Vertex, "person", key).unwrap();
    (format!("person_{key}"), rule.with_codec(Codec::Dedup))
};
graph.traversal_mut().define_compressions([dedup("name"), dedup("age")]).unwrap();
```

The estimate in Rust (`graphersal::catalog`: `CompressionEstimateRequest`, `CompressionCandidate`,
`CompressionEstimate`); a storage implements it as `GraphStorage::estimate_compression`:

```rust
use graphersal::prelude::*;
use graphersal::catalog::{CompressionCandidate, CompressionDefinition, CompressionEstimateRequest};
use graphersal::storage::ElementKind;

let mut graph = TraversalGraph::new();
let request = CompressionEstimateRequest::new(ElementKind::Vertex, "customer", "bio")
    .unwrap()
    .with_candidates([CompressionCandidate::lz4(256, true), CompressionCandidate::lz4(512, false)]);
let estimate = graph.traversal().estimate_compression(&request).unwrap();
if let Some(best) = estimate.best {
    let rule = CompressionDefinition::new(ElementKind::Vertex, "customer", "bio").unwrap();
    let rule = estimate.candidates[best].candidate.apply_to(rule);
    graph.traversal_mut().define_compression("customer_bio", rule).unwrap();
}
```

**Authorization**: a rule is the resource `Definition { kind: compression, name }`: define asks
`Create` (`Update` when it replaces one; a list asks it for each rule before anything changes), drop `Delete`, recompress `Update`, listing `Read`. No data
request is made (the logical data does not change). `AccessPolicy::read_only()`, the MCP read-only
mode and a saved query (which never writes the catalog) cannot change rules. An estimate asks
`Read` on the `Data` of its element kind and label.

**The catalog file**: rules travel with the saved queries in one catalog file
(`{"definitions": [..]}`, the playground's **Catalog ▾ ▸ Save catalog**, the dev server's
`GET /api/catalog/export`); loading it stores the rules and compresses their values at once. A
snapshot and a Store carry the catalog, so a rule survives every store operation (WAL replay,
checkpoint, fork, backup, restore, repair, rollback); the files hold plain values, and the load
compresses again. A version that does not know the kind keeps the rule unchanged and only loses the
compression (the definition is not critical). A version that knows the kind but not the codec
(0.2.0 reading a `dedup` rule written by 0.2.1), or a threshold it refuses (0.5.2 and older
reading a `dedup` rule with `min_bytes` 0), reports the definition as damage; drop dedup rules
before opening a store with an older version.

In the playground, **Catalog ▾ ▸ Compression** lists the rules with their statistics (here 400
customer biographies, about 1 KiB each) and defines, edits, recompresses and drops them:

![The compression manager: a rule with its values, plain and stored size, ratio and the bytes saved](../images/playground/compression-manager.png)

## Measured

On a generated CV-like corpus of 5,000 texts of 2 to 20 KB (55 MB: section headers, sentences
from a 200-word vocabulary, company names, years and numbers), Apple M-series, `--release`
(`compressed_property_tests::measure_ratio_and_throughput`, an `#[ignore]`d test):

| | Stored / plain | Dictionary | Define pass (compress) | Read (decompress + materialize) |
|---|---:|---:|---:|---:|
| LZ4 | 0.52 | none | 451 MB/s | 2,741 MB/s |
| LZ4 with dictionary | 0.42 | 64 KiB | 328 MB/s | 1,626 MB/s |

The compression figures include a round-trip check of every value (a compressed value is verified
once when it is made, so reading it back cannot fail). A highly repetitive corpus (a dozen phrases
recombined) compresses to 0.07 (0.05 with a dictionary).
The figures come from a source build, not from the musl-linked prebuilt Linux binary
([Performance of the prebuilt Linux binaries](../getting_started/installation.md#performance-of-the-prebuilt-linux-binaries)).
