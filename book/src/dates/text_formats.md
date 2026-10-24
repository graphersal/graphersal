# Text Formats

Dates are read and written as ISO 8601 text (RFC 3339 for a `datetime`, with the RFC 9557 zone
suffix). No locale formats, no two-digit years, no guessing: a text is read as a date only where a
date is asked for (a constructor, a conversion step, a declared schema format, a typed value in a
file).

## Canonical output

Every value has one text, the one tables, JSON, GraphML, GraphSON and `as_string()` write. It is
the text `java.time` writes, so other tools read it back:

| Type | Canonical text |
|---|---|
| `date` | `2023-08-02`; a year before 1 or after 9999 with its sign: `-0044-03-15` |
| `time` | `09:30:00`; a fraction only when it is not zero, in groups of three digits (milliseconds, microseconds or nanoseconds): `09:30:00.500`, `09:30:00.123456`, `09:30:00.123456789` |
| `localdatetime` | `2023-08-02T09:30:00` (the date, `T`, the time) |
| `datetime` | `2023-08-02T09:30:00+02:00`; `Z` for a zero offset; an offset with seconds as `+00:19:32` |
| `duration` | `P1Y2M3DT4H5M6S`: years and months from the months, days, then the time part; a zero part is left out, a zero duration is `PT0S`; a sign before `P` when every part is negative (`-P1M2D`), else a sign per part (`P1M-2D`) |

```rhai,graph=empty
time("09:30:00.1").as_string()   // "09:30:00.100"
datetime("2023-08-02 09:30+00:00").as_string()   // "2023-08-02T09:30:00Z"
duration("PT90M").as_string()   // "PT1H30M"
duration("P2W").as_string()   // "P14D"
```

A `duration` keeps its three fields as they are: `PT36H` stays `PT36H` (not `P1DT12H`), and
weeks are read as 7 days. Years and months are one field, months: `P1Y2M` is 14 months.

## What is read

| Type | Accepted text |
|---|---|
| `date` | `YYYY-MM-DD`; a year before 1 with its sign (`-0044-03-15`) |
| `time` | `HH:MM`, `HH:MM:SS`, `HH:MM:SS.f` with 1 to 9 fraction digits, kept to the nanosecond |
| `localdatetime` | a date, optionally followed by `T` (or `t`, or a space) and a time; a date alone is midnight |
| `datetime` | a local date-time, then an offset (`Z`, `+02:00`, `+0200`, `+02`, `+00:19:32`) and/or a zone in brackets (`[Europe/Berlin]`) |
| `duration` | `[+-]P[nY][nM][nW][nD][T[nH][nM][n[.f]S]]`, a sign per number allowed (`P1M-2D`), letters in either case |

```rhai,graph=empty
datetime("2023-08-02T09:30+0200")   // datetime("2023-08-02T09:30:00+02:00")
datetime("2023-08-02T09:30[Europe/Berlin]")   // datetime("2023-08-02T09:30:00+02:00")
datetime("2023-08-02T09:30:00.123456789Z")   // datetime("2023-08-02T09:30:00.123456Z")
date("10000-01-01")   // error: expected a four-digit year
duration("1 day")   // error: not an ISO 8601 duration
```

A `datetime` with only a zone is resolved in it (a wall-clock time the zone skips moves forward,
one it repeats takes the earlier offset, see [Time Zones](time_zones.md)); with an offset and a
zone the offset must be the zone's at that instant.

## Strict and lenient: a `datetime` without an offset

TinkerPop reads a date-time text without an offset as UTC (`asDate("2023-08-02")` is midnight
UTC). Graphersal does that only where a query **asks** for a conversion:

- **lenient** (no offset = UTC, a date alone = midnight UTC): the `datetime()` / `DateTime()`
  constructor and the conversion steps `as_date()` and `as_date(GType.DATETIME)`;
- **strict** (an offset or a zone is required): every place a text is **stored** as a `datetime`,
  namely a property declared `"format": "date-time"` in the [schema](schema.md), the
  `formatMinimum`/`formatMaximum` bounds, a typed `gx:OffsetDateTime` in GraphSON or a change set,
  a saved query argument declared as a date-time, and the playground's property editor.

```rhai,graph=empty
datetime("2023-08-02T09:30")   // datetime("2023-08-02T09:30:00Z")
g.inject("2023-08-02").as_date().next()   // datetime("2023-08-02T00:00:00Z")
```

A stored instant without its offset would be a guess about where it was written; the strict
rule turns that guess into an error that names the expected form. The other types have no
offset, so they read the same everywhere.
