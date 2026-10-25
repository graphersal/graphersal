# GraphSON

GraphSON 3.0 is TinkerPop's JSON graph format and Graphersal's primary exchange format for Gremlin
users: TinkerGraph, Gremlin Server and JanusGraph write it with `g.io("graph.json").write()`, and
TinkerGraph reads Graphersal's export with `g.io("graph.json").read()`. GraphML stays available as
the second format. Both need the `io` feature, and neither carries the schema (see
[Schema](#schema)).

## Reading and writing

| Front end | Import | Export |
|---|---|---|
| Rust, new graph | `GraphSource::from_graphson(path)`, `from_graphson_reader(reader)`, `GraphSource::from_file(path)` for `.json`/`.jsonl`/`.graphson`; `GraphSONImport::new().load(reader)` / `load_file(path)` also return the [report](#the-import-report) | `graph.export_graphson(path)`, `export_graphson_writer(writer)` (`GraphExport`), `GraphSONExport::new().export_writer(&storage, writer)` |
| Rust, live graph | `graph.import_graphson(path)` / `import_graphson_reader(reader)` (`GraphImport`): one [unit](transactions.md), returns the report | |
| Rhai | `g.import_graphson(path)`: returns the report as text; `GraphSource::file("graph.json")` | `g.export_graphson(path)` |
| CLI | `graphersal --graph graph.json` (also `.jsonl`, `.graphson`); skipped data is listed in a `note:` on stderr | `-e 'g.export_graphson("out.json")'` |
| Python | `Graph.from_graphson(path, schema=None, mode=None)` (a `schema` is set on the new graph first, then the file is imported as one unit); `graph.import_graphson(path)` into a live graph, one unit; skipped data is one `UserWarning` | `graph.export_graphson(path)` |
| Playground | **Load from file** with a `.json`/`.jsonl`/`.graphson` file; skipped data is a notice | **Save ▾ › Data as GraphSON** (also Data as GraphML, Schema) |

The file operations ask the authorizer like GraphML's: `Read` on the `File` and `Create` on `Data`
for an import, `Read` on `Data` and `Create` on the `File` for an export (see
[Permissions](permissions.md)).

```rust,ignore
use graphersal::io::{GraphExport, GraphImport, GraphSONImport};

let graph = GraphSource::from_graphson("tinkerpop-modern.json")?;
graph.export_graphson("copy.json")?;

let (graph, report) = GraphSONImport::new().load_file("export-from-janusgraph.json")?;
if report.has_skips() {
    eprintln!("{report}");
}
```

## Accepted input

- **Line form**: one vertex object per line, TinkerPop's default and the only form `io()` reads.
  Blank lines, a UTF-8 byte order mark and `\r\n` line ends are accepted.
- **Wrapped form**: one document `{"vertices": [...]}` (TinkerPop's `wrapAdjacencyList`), compact
  or pretty-printed. Other top-level keys are ignored and reported.
- **Versions**: GraphSON 3.0 typed (the default), 3.0 `types=false`, 2.0 typed and untyped 1.0 and
  2.0 are read by one decoder: an `{"@type": .., "@value": ..}` object is a typed value, anything
  else is plain JSON. Typed GraphSON 1.0 (`@class`) and the TinkerPop 2 (Blueprints) format are
  rejected with `IOError::UnsupportedGraphSON`.
- A vertex object is read from `id`, `label`, `properties`, `outE` and `inE`. Other keys (Cosmos
  DB's `type`, `_partition`) are ignored and reported. A vertex-property entry needs only `value`
  (its `id` is not kept); an edge entry needs `inV`/`outV`.

## Type mapping

| GraphSON | Graphersal | Export writes |
|---|---|---|
| string, boolean, `null` | `string`, `boolean`, `null` | the same |
| `g:Int32`, `g:Int64`, `gx:Int16`, `gx:Byte`, `gx:BigInteger` (within `int64`), a plain integer | `int64` | `g:Int64` |
| `g:Double`, `g:Float`, a plain decimal; the payloads `"NaN"`, `"Infinity"`, `"-Infinity"` | `float64` | `g:Double` (non-finite values as those strings) |
| `g:UUID` | `uuid` | `g:UUID` |
| `g:List`, `g:Set`, a plain array | `array` | `g:List` |
| `g:Map` with string keys, a plain object | `object` | `g:Map` |
| `gx:Char` | `string` | a string |
| `g:Date`, `g:Timestamp` (epoch milliseconds) | `datetime` in UTC | `gx:OffsetDateTime` |
| `gx:Instant`, `gx:OffsetDateTime` | `datetime` with the offset of the text (a text without an offset is malformed and skipped, never read as UTC) | `gx:OffsetDateTime` |
| `gx:ZonedDateTime` (`2007-12-03T10:15:30+01:00[Europe/Paris]`) | `datetime` with the offset and the zone id of the text; a zone id the bundled database does not know (`GMT+02:00`), or whose offset at that instant is not the text's (`2024-01-01T10:00+05:00[Europe/Berlin]`), is dropped and reported, and the text's offset is kept | `gx:ZonedDateTime` for a `datetime` with a zone name, else `gx:OffsetDateTime` |
| `gx:LocalDate`, `gx:LocalDateTime`, `gx:LocalTime`, `gx:OffsetTime` (`10:15:30+01:00`) | `date`, `localdatetime`, `time`, `offsettime` | the same tags |
| `gx:Duration` (a fixed length; `P2DT3H` reads a day as 24 hours; `java.time`'s signed fields `PT-1H-30M` too) | `duration` of a time part only, up to the `java.time.Duration` range (a 64-bit count of seconds plus nanoseconds) | `gx:Duration` for a duration of a time part only (`PT26H`); a negative one as `java.time` writes it, each field signed (`PT-1H-30M`, `PT-0.5S`) |
| `gx:Period` (`P1Y2M3D`, weeks as 7 days) | `duration` of months and days | `gx:Period` for a duration of months and days only |
| `graphersal:Interval` (Graphersal's own tag) | `duration` | `graphersal:Interval` for a duration with both months or days and a time part (`P1MT2H`): no `java.time` type holds it, so TinkerGraph cannot load such a value |
| `g:List` of numbers, read as a list; a vector where the target graph's schema declares one (mode `open` or `closed`, coerced on write); `graphersal:Vector` (Graphersal's own exact tag, as a serialized `ChangeSet` writes it) | `vector` (book page [Vectors](vectors.md)) | `g:List` of `g:Float`, which TinkerGraph loads; an `i8` or `bit` vector as its dequantized values |
| `gx:Year`, `gx:YearMonth`, `gx:MonthDay`, `gx:ZoneOffset`, `g:Calendar`, `g:TimeZone` | **skipped** | |
| `gx:BigDecimal`, `g:Class`, the enum tokens, `g:Vertex`/`g:Edge`/`g:Path`/... as a value, vendor types (`janusgraph:Geoshape`) | **skipped** | |

Nothing is guessed: a string stays a string whatever it looks like (a date-like text without a
type tag, as in GraphSON 1.0 or `types=false`, stays a `string`; declare the property in the
schema to store it as a date), and a value whose type Graphersal cannot hold is **skipped**,
never converted to a string or a number. The `java.time` types map to the date and time types
by their tag (both the `gx:` and the `g:` prefix are read; the types are described in
[Dates and Times](../dates/index.md)); their text is read as `java.time`
writes it (seconds may be missing, up to nine fraction digits), and kept to the nanosecond,
Graphersal's resolution. A date outside the years
-9999 to 9999, or text that is not of the tag's type, is skipped as malformed. The export writes
each value's canonical text, which `java.time`'s parsers read, with all nine fraction digits
where the value has them; a negative `gx:Duration` is written in `java.time`'s own form,
because `Duration.parse` sums the fields before it applies a leading sign and so cannot read
`-PT2562047788015215H30M8S`, the canonical text of the shortest duration. TinkerGraph's
`g.io(file).read()` loads every value (except `graphersal:Interval`). A skipped value inside a container skips the whole
property value. A value
nested deeper than 128 levels, a `g:Map` with an odd number of items or a non-string key, and a
payload that does not fit its type (`{"@type": "g:Int32", "@value": "7"}`) are skipped as
malformed.

A number outside `int64`/`float64` (a plain integer above `i64::MAX`, `1e999`) makes its line fail
to parse; the import stops with `IOError::GraphSONParse`.

## Structure mapping

| GraphSON | Graphersal |
|---|---|
| element id of any type | a string: `g:Int32 1` becomes `"1"`, a `g:UUID` its canonical text, JanusGraph's `{"relationId": "x"}` becomes `"x"`, any other id its JSON text |
| vertex without an `id` | the graph assigns one (reported) |
| `label` `"A::B"` | the label set `[A, B]` ([Multi-Label Vertices](multi_label_vertices.md)) |
| `properties.k` with one entry | the entry's value |
| `properties.k` with several entries (list cardinality) | one `array` of the values (reported) |
| meta-properties of an entry | the reserved object property `_meta`: `_meta.k = {..}` for one entry, an array of objects aligned with the values for several (reported) |
| `outE` and `inE` | edges. `outE` is the source of truth; an `inE` copy with the same id is matched with it (a disagreement is reported, the `outE` copy wins); an edge found only in `inE` (a file written with `Direction.IN`) is added. Without an id, an `inE` entry is taken only when its tail vertex has no `outE` |
| an edge whose other endpoint is not in the file | connected to the target graph's vertex of that id if one exists, else skipped (reported) |
| a vertex id used twice | the first vertex wins, the second and its edges are skipped (reported) |

Export does the reverse: every vertex is one line with `id`, `label`, `inE`, `outE` and
`properties`; ids are JSON strings; a label set is joined with `"::"`; vertex-property ids are a
file-wide `g:Int64` sequence. `_meta` is written back as meta-properties when it has exactly the
shape an import produces (an object per key, or an array aligned with an array value of two or
more items: then the array becomes that many entries); any other `_meta` is written as an ordinary
property, so a round trip never loses it.

A graph exported by Graphersal and imported again is the same graph: ids, label sets, every
property with its exact logical type (`int64`, `float64` including `NaN` and infinities, `uuid`,
nested `array`/`object`, every date and time type with a `datetime`'s offset), edge ids, labels
and properties. Only the iteration order of edges may differ, because GraphSON groups them by vertex and label. Two Graphersal elements have no
TinkerPop counterpart and come back changed: a vertex without labels is written with TinkerPop's
default label `vertex`, an edge without a label with `edge`, and both are read back with that
label.

## The import report

`ImportReport` (`graphersal::io`) holds the counts of imported vertices and edges and one
`ImportNote` per kind of event, with a count and up to five sample locations
(`line 3: vertex "1", property "born"`; `vertices[2]: ...` in the wrapped form). `ImportNoteKind`
says what happened; its `Skipped*` kinds lost data (`is_skip()`), the others only changed the
representation:

| Kind | Meaning |
|---|---|
| `SkippedDateTime { type_tag }` | a date/time value of a type Graphersal cannot hold (`gx:Year`, `gx:MonthDay`, ...) |
| `SkippedUnsupportedType { type_tag }` | a value of a type Graphersal cannot hold |
| `SkippedMalformedValue` | a value whose payload does not fit its type, or nested too deeply |
| `SkippedMalformedEntry` | a vertex-property or edge entry without the expected shape |
| `SkippedDuplicateVertex` | a vertex whose id came earlier in the file |
| `SkippedDanglingEdge` | an edge to a vertex that does not exist |
| `SkippedDuplicateEdge` | an edge whose id a different edge of the file already has |
| `SkippedReservedMeta` | a `_meta` property of a vertex that also has meta-properties |
| `MultiValuedToArray`, `MetaPropertiesToMeta`, `LabelSplit`, `VertexWithoutId`, `EdgeCopiesDisagree`, `IgnoredKey { key }` | representation changes |
| `ZoneDropped { type_tag }` | a `gx:ZonedDateTime` imported as a `datetime` with the offset of its text; its zone id is not a zone of the bundled database, or that zone's offset at that instant is not the text's, so the name is not kept (the offset is never recomputed from it) |
| `SubMicrosecondsCut { type_tag }` | deprecated since 0.3.0 and never reported: values keep nanoseconds (kept so code that matches it compiles) |

Its `Display` is the text the CLI, Rhai and Python show: a first line with the counts and one line
per note.

## Limits and errors

One line (line form) or the whole document (wrapped form) is read only up to
`GraphSONImport::max_text_bytes` (512 MiB natively, 64 MiB on wasm32); a longer one fails with
`IOError::InputTooLarge` before it is held in memory (`GraphSONImport::new().with_max_text_bytes(n)`
changes it). Use the line form for large graphs: it is read line by line, and only the edges are
buffered until every vertex is known. JSON nesting is bounded, so no input can exhaust the stack.

Fatal errors stop the import, and on a live graph roll it back as a whole: invalid JSON or a line
that is not an object (`IOError::GraphSONParse` with the line), an unsupported variant
(`IOError::UnsupportedGraphSON`), an oversized line, and every graph error, such as a schema
violation or a vertex id that already exists in the target graph (`IOError::Graph`).

## Schema

GraphSON carries no schema. Typed values keep their own types, so a GraphSON file needs no schema
to come back exactly. When the target graph stores a schema, the import goes through it like any
other write: `open`/`closed` coerce declared fields (an untyped canonical UUID string becomes a
declared `string` with `format: uuid`) and reject what violates it, which fails the import. Save
the schema separately (`g.get_schema()`, its JSON in the [schema format](schema_enforcement.md))
and set it on the target with `g.set_schema(..)` before importing. The front ends do both steps
in that order for you: `graphersal --graph graph.json --schema schema.json [--schema-mode closed]`,
Python `Graph.from_graphson(path, schema=.., mode=..)` and the playground's **Load from file** with
its optional schema file.

## Loading an export into TinkerGraph

Element ids are strings in Graphersal, the same model as Amazon Neptune and Azure Cosmos DB, and
the export writes them as JSON strings (`"id":"1"`). A TinkerGraph opened with its default id
manager keeps them as `String`s, so it answers `g.V("1")`, and `g.V(1)` finds nothing. When the ids
are numeric, open the TinkerGraph with the `LONG` id managers: they convert every id they read (and
every id a query passes, `1`, `1L` or `"1"`) to a `Long`, so `g.V(1)` works as on TinkerPop's own
sample graphs:

```groovy
import org.apache.commons.configuration2.BaseConfiguration
import org.apache.tinkerpop.gremlin.tinkergraph.structure.TinkerGraph

conf = new BaseConfiguration()
conf.setProperty("gremlin.tinkergraph.vertexIdManager", "LONG")
conf.setProperty("gremlin.tinkergraph.edgeIdManager", "LONG")
// keeps every value of an array that came from a multi-property (see below)
conf.setProperty("gremlin.tinkergraph.defaultVertexPropertyCardinality", "list")
graph = TinkerGraph.open(conf)
g = graph.traversal()
g.io("modern.json").read().iterate()
g.V(1).out("knows").values("name")   // ==> vadas, josh
```

The same keys go into a Gremlin Server's TinkerGraph `.properties` file
(`gremlin.tinkergraph.vertexIdManager=LONG`, ...). The `LONG` managers need ids that parse as
numbers: a graph with ids such as `"V_0_0"` fails to load with them (`Expected an id that is
convertible to class java.lang.Long`); load it with the default managers and query it by its
string ids.

Verified with TinkerGraph 3.8.2 (Gremlin Console, `g.io(file).read()`) on the exports of `modern`,
`large` (111 110 vertices, 111 100 edges) and a graph with every value type:

| What | Default id managers | `LONG` id managers |
|---|---|---|
| vertex and edge counts | exact | exact (`large`: fails, its ids are not numbers) |
| id type | `String` | `Long` |
| `g.V(1)` / `g.V(1L)` | nothing | the vertex |
| `g.V("1")` | the vertex | the vertex |
| values | `int64` -> `Long`, `float64` -> `Double` (also `NaN`), `uuid` -> `UUID`, `array` -> `List`, `object` -> `Map`, `string`, `boolean` | the same |
| `_meta` | meta-properties on the vertex properties | the same |
| multi-label vertex | one label `"A::B"` | the same |

TinkerGraph's default vertex-property cardinality is `single`: an array exported as several
entries with meta-properties (a former multi-property, `_meta` aligned with it) keeps only its last
entry unless the graph is opened with `defaultVertexPropertyCardinality` `list`. With `list`, the
file TinkerGraph writes back (`g.io(file).write()`) imports into Graphersal as the same graph:
values with their types, the array, `_meta`, and the label set from `"A::B"`.

## Deviations from TinkerPop

Listed in [TinkerPop Deviations](../for_developers/tinkerpop_deviations.md#graphson) as well:

- **Ids are strings.** Imported numeric ids become strings, and export writes string ids, so a
  TinkerGraph that loads the export answers `g.V("1")`, not `g.V(1)`, unless it is opened with
  the `LONG` id managers ([recipe](#loading-an-export-into-tinkergraph)).
- **Multi- and meta-properties** are folded into an array and the `_meta` property, because
  Graphersal stores one value per key; export restores them only from `_meta`, otherwise an array
  is one vertex property holding a `g:List`.
- **Multi-labels** are written as `"A::B"`, which TinkerGraph treats as one opaque label
  (Neptune's convention, see [Multi-Label Vertices](multi_label_vertices.md)).
- **Unlabeled elements** are written with the default labels `vertex`/`edge` and read back with
  them.
- **Skipped values**: the types listed above as skipped (`gx:Year`, `gx:BigDecimal`, ...) are
  skipped and reported where TinkerPop reads them.
- **Dates**: a `gx:ZonedDateTime` whose zone id the bundled database does not know keeps its
  offset but not its zone id (reported). One whose offset is not its zone's offset at that
  instant (written with another time zone database) keeps the offset of the text and drops the
  name (reported), where `java.time` keeps the instant and the zone and recomputes the offset
  (user decision: the stored offset is kept as written, never recomputed from a zone name, as in
  the store). A duration with both months or days and a time part is exported as
  `graphersal:Interval`, which TinkerPop cannot read.
- **Lenient structure**: an edge to a missing vertex is skipped (TinkerPop's `readGraph` fails), a
  duplicate vertex id keeps the first vertex, and vertex-property ids are not kept.
- **Widening**: `g:Int32` and `g:Float` come back as `g:Int64` and `g:Double` on export.
