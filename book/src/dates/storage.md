# Storage and Exchange

A date keeps its type wherever it goes: in memory, in a snapshot or a Store, in a change set and
in a GraphSON file. GraphML has no date type and needs the schema.

## In memory

A date is stored inline in the property value, 20 bytes at most, without a heap allocation: a
`date` is a day number, a `time` and a `localdatetime` nanoseconds, a `datetime` nanoseconds
since the epoch plus its offset in seconds and, when it has one, a two-byte index of its zone
name in the bundled time zone database, an `offsettime` nanoseconds of the day plus its offset,
a `duration` its months, days and nanoseconds.
Equality and ordering compare these numbers ([Comparing and Sorting](comparing.md)).

## Snapshots, journals and the Store

The binary format (packed snapshots `.gsnap`, Store segments, the write-ahead log) has a value
tag per type, defined in [the format specification](../persistence/format.md#3-value-encoding):
the exact payload, the range of each type, and the rule that a `datetime` keeps its offset while
equality uses the instant. Every value is stored exactly as it is in memory: nanoseconds over the
whole range of years, an `offsettime` with its offset, a `datetime` with its zone name (as text,
`Europe/Berlin`, never as a number of this build), a `duration` over its whole range (a 64-bit
count of seconds plus the nanoseconds of one second). Nothing is refused and nothing is cut.

These nanosecond tags came with **file format version 3** (Graphersal 0.3.0); version 2 (0.2.x)
had microsecond tags only and could not store a value with digits below the microsecond, an
`offsettime` or a zone name. A build that knows version 3 writes version 3 and reads versions 1
to 3, so a `.gsnap` or a Store written by 0.1.x or 0.2.x still loads; a Store written by 0.2.x
is continued as it is, with no upgrade. Graphersal 0.2.x refuses a version 3 file as written by
a newer version instead of misreading it. A zone name that this build's time zone database does
not know (a file written by a newer Graphersal with a newer database) is refused as content of
a newer version, never taken for damage. See
[Format Versions and Upgrade](../persistence/store_versions.md).

## Change sets

A serialized [`ChangeSet`](../users_guide/transactions.md#serialization) (feature `serde`) writes a
date as a per-value type tag in TinkerPop's GraphSON style, so a replay restores the type without a
schema:

| Type | Tag |
|---|---|
| `datetime` | `{"@type": "gx:OffsetDateTime", "@value": "2024-05-01T09:30:00+02:00"}` |
| `datetime` with a zone name | `{"@type": "gx:ZonedDateTime", "@value": "2024-05-01T09:30:00+02:00[Europe/Berlin]"}` |
| `date`, `localdatetime`, `time`, `offsettime` | `gx:LocalDate`, `gx:LocalDateTime`, `gx:LocalTime`, `gx:OffsetTime` |
| `duration` of a time part only | `gx:Duration` (`PT26H`) |
| `duration` of months and days only | `gx:Period` (`P1Y2M3D`) |
| any other `duration` | `graphersal:Interval` (`P1MT2H`) |

Reading is strict: the text must be a value of the tag's type, and a `gx:OffsetDateTime` must carry
its offset.

## GraphSON

GraphSON 3.0 reads and writes the `java.time` types: `g:Date`/`g:Timestamp` (epoch milliseconds)
become a `datetime` in UTC, `gx:OffsetDateTime`, `gx:Instant` and `gx:ZonedDateTime` a `datetime`
with the offset of the text (a `gx:ZonedDateTime` keeps its zone id when the bundled database
knows it and the text's offset is that zone's offset at that instant; otherwise the id is
dropped and reported, and the offset of the text is kept: the instant is never recomputed from
the name, as in the store), `gx:LocalDate`, `gx:LocalDateTime`,
`gx:LocalTime` and `gx:OffsetTime` the types without a date or a zone and the `offsettime`,
`gx:Duration` and `gx:Period` a `duration`. The export writes the tags of the change set table above, which TinkerGraph's
`g.io(file).read()` loads, with one exception: a `duration` with both months or days **and** a time
part has no `java.time` type, so it is written as `graphersal:Interval`, which only Graphersal
reads (a [deliberate deviation](../for_developers/tinkerpop_deviations.md)), and a negative
`gx:Duration` is written as `java.time` writes it, each field signed (`PT-1H-30M`), because
`java.time` cannot read `-PT2562047788015215H30M8S`, the canonical text of the shortest duration.
Up to nine fraction digits are kept in both directions, and the `java.time` types Graphersal cannot hold (`gx:Year`,
`gx:YearMonth`, `gx:MonthDay`, `gx:ZoneOffset`) are skipped and reported. The
full mapping is on the [GraphSON](../users_guide/graphson.md#type-mapping) page.

A date-like string without a type tag (GraphSON 1.0, `types=false`) stays a string; declare the
property in the schema to store it as a date.

## GraphML

GraphML has no date type: a date is written as its canonical text under a `string` key. It is read
back as a date only when the **target graph's schema** declares the property with a date
`format`: set the schema on the empty graph first, then import (the playground's "Load from file"
takes the schema file next to the graph file; the CLI's `--schema`). Without a schema the text
stays a string. See [UUID Values](../users_guide/uuid_values.md), which describes the same rule for
UUIDs, and the [Schemas](../users_guide/schema_enforcement.md) page.

## JSON, tables and the playground

Tables, JSON results, the CLI's visualizers, MCP and the dev server write the canonical text
(`"2023-08-02T09:30:00+02:00"`); a script value prints as its literal
(`datetime("2023-08-02T09:30:00+02:00")`). The playground's property editor keeps the type: it
shows and edits a date property as one of the date types ([Text Formats](text_formats.md)).
