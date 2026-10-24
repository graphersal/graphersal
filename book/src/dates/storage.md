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
tag per type, 9 to 13, defined in [the format specification](../persistence/format.md#3-value-encoding):
the exact payload, the range of each type, and the rule that a `datetime` keeps its offset while
equality uses the instant. These tags came with **file format version 2**: a build that knows them
writes version 2 and reads versions 1 and 2, so a `.gsnap` written by 0.1.x still loads, while an
older build refuses a version 2 file instead of misreading it. See
[Format Versions and Upgrade](../persistence/store_versions.md) for a Store written by 0.1.x.

The tags hold microseconds. A value with a part below the microsecond
(`time("12:00:00.000000001")`) is refused when it is written: the transaction or snapshot that
holds it fails with an error naming the value, and nothing is stored (never a cut value).
Truncate it with `date_trunc(DT.microsecond)` to store it. A `duration` whose time part exceeds
a 64-bit count of microseconds (about 292,000 years; the type reaches a 64-bit count of seconds)
is refused in the same way.

File format version 2 has no tag for an `offsettime` and no field for a zone name either. Such a
value is refused in the same way when it is written (the unit fails, nothing is stored, the zone
name is never dropped silently): store a `datetime` without the name
(`datetime("2024-03-31T12:00:00+02:00")`), or the time and its offset apart. The next file format
version adds both.

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
knows it, otherwise the id is dropped and reported), `gx:LocalDate`, `gx:LocalDateTime`,
`gx:LocalTime` and `gx:OffsetTime` the types without a date or a zone and the `offsettime`,
`gx:Duration` and `gx:Period` a `duration`. The export writes the tags of the change set table above, which TinkerGraph's
`g.io(file).read()` loads, with one exception: a `duration` with both months or days **and** a time
part has no `java.time` type, so it is written as `graphersal:Interval`, which only Graphersal
reads (a [deliberate deviation](../for_developers/tinkerpop_deviations.md)). Up to nine
fraction digits are kept, and the `java.time` types Graphersal cannot hold (`gx:Year`,
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
