# Dates and Times

Graphersal has five date and time types. They are values of their own, like numbers and
[UUIDs](../users_guide/uuid_values.md): a property holds a `date`, not a string that looks like
one, and a query compares, sorts and computes with it. The model follows PostgreSQL and the SQL
standard; the part TinkerPop defines (`datetime()`, `asDate()`, `dateAdd()`, `dateDiff()`,
`DT.second` to `DT.day`) behaves exactly as TinkerPop 3.8 specifies it
([TinkerPop compatibility](tinkerpop.md)).

## The five types

| Graphersal (`type_of`, `GType`) | What it is | Literal | PostgreSQL | SQL standard | Python | TinkerPop / `java.time` |
|---|---|---|---|---|---|---|
| `datetime` (`GType.DATETIME`) | an instant with the UTC offset it was written with | `datetime("2023-08-02T09:30:00+02:00")` | `timestamptz` (keeps no offset) | `TIMESTAMP WITH TIME ZONE` | aware `datetime.datetime` | `datetime`, `OffsetDateTime` |
| `localdatetime` (`GType.LOCALDATETIME`) | a wall-clock date and time, no zone | `localdatetime("2023-08-02T09:30")` | `timestamp` | `TIMESTAMP` | naive `datetime.datetime` | `LocalDateTime` |
| `date` (`GType.DATE`) | a calendar day | `date("2023-08-02")` | `date` | `DATE` | `datetime.date` | `LocalDate` |
| `time` (`GType.TIME`) | a time of day, no zone | `time("09:30:15.5")` | `time` | `TIME` | naive `datetime.time` | `LocalTime` |
| `duration` (`GType.DURATION`) | months, days and nanoseconds, each with its own sign | `duration("P1Y2M3DT4H")` | `interval` | `INTERVAL` | `datetime.timedelta`, or `graphersal.Interval` with months | `Duration`, `Period` |

```rhai,graph=empty
type_of(datetime("2023-08-02T09:30:00+02:00"))   // "datetime"
date("2024-02-29").day   // 29
duration("P1Y2M3DT4H").months   // 14
g.inject(datetime("2023-08-02T09:30Z")).is(P.typeOf(GType.DATETIME)).count().next()   // 1
```

- **Resolution:** one nanosecond for every type with a time, as in `java.time` and TinkerPop
  (PostgreSQL and Python stop at the microsecond). TinkerPop's millisecond views (`as_number()`
  of a `datetime`, `date_diff()`) round down to milliseconds. Python's `datetime` holds
  microseconds: a value with a part below them does not convert ([From Python](python.md)),
  and the binary store format of this release refuses it too ([Storage and
  Exchange](storage.md)).
- **Range:** the years -9999 to 9999 in the proleptic Gregorian calendar (`date("-0044-03-15")`
  is year 44 BC counted astronomically, year 0 = 1 BC). A value outside the range, or a result
  that would leave it, is an error, never a wrapped value.
- **Offsets:** a `datetime` keeps its offset (±18:00, whole seconds) and shows it, unlike
  PostgreSQL's `timestamptz`, which keeps only the instant. A time zone is never stored in a
  value: operations take one ([Time Zones](time_zones.md)).
- **A `duration` is not normalized:** `P1M`, `P30D` and `PT720H` are three different values. A
  month has no fixed length and a day is not always 24 hours in a time zone, so the three fields
  stay apart, as in PostgreSQL's `interval`.
- **No guessing:** a string stays a string whatever it looks like. A date comes from a
  constructor, a conversion step ([Conversions](conversions.md)), a schema that declares the
  property ([Schema](schema.md)), a typed value in a file ([Storage and Exchange](storage.md)) or
  a Python `datetime` ([From Python](python.md)).

## Script values

In a script each type is a value of its own Rhai type with getters and operators:

| Type | Getters |
|---|---|
| `datetime` | `year`, `month`, `day`, `hour`, `minute`, `second`, `microsecond`, `nanosecond` (on its own wall clock), `offset` (seconds), `epoch_millis` |
| `localdatetime` | `year`, `month`, `day`, `hour`, `minute`, `second`, `microsecond`, `nanosecond` |
| `date` | `year`, `month`, `day` |
| `time` | `hour`, `minute`, `second`, `microsecond`, `nanosecond` |
| `duration` | `months`, `days`, `micros` (the time part in whole microseconds), `nanos` (in nanoseconds) |

`microsecond` is the whole microseconds of the second (0-999999), `nanosecond` all its
nanoseconds (0-999999999): `time("12:00:00.123456789")` has `microsecond` 123456 and
`nanosecond` 123456789.

```rhai,graph=empty
let d = datetime("2023-08-02T09:30:15.25+02:00"); [d.hour, d.offset, d.microsecond]   // [9, 7200, 250000]
datetime("2023-08-02T12:00Z") + duration("P1M")   // datetime("2023-09-02T12:00:00Z")
date("2023-08-02") - date("2023-01-01")   // duration("P213D")
time("10:00") - time("09:30")   // duration("PT30M")
datetime("2023-08-02T09:30+02:00").as_string()   // "2023-08-02T09:30:00+02:00"
```

`+` and `-` with a `duration` work as [`date_add()`](arithmetic.md) does; the difference of two
values of one type is a `duration` (two `datetime`s or `localdatetime`s give days of 24 hours and
a time part, two `date`s days, two `time`s the elapsed time). `as_string()` is the canonical text,
`to_string()` the DSL literal. A value printed or shown as a script value is its literal
(`date("2023-08-02")`); tables, JSON and the playground show the canonical text.

## The pages of this part

- [Comparing and Sorting](comparing.md): equality by instant, field-wise durations, the order of
  types.
- [Text Formats](text_formats.md): the canonical output, what is read, strict and lenient input.
- [Time Zones](time_zones.md): the bundled IANA database, offsets, daylight saving.
- [Arithmetic](arithmetic.md): `date_add()`, `date_diff()`, `age()`.
- [Truncation and Parts](truncation_and_parts.md): `date_trunc()`, `date_part()`.
- [Conversions](conversions.md): `as_date()`, `as_duration()`, `as_number()`, `as_string()`.
- [now() and today()](now_and_today.md): the evaluation's start time and how a host pins it.
- [Dates in a Schema](schema.md): the date `format`s, bounds and coercion.
- [Storage and Exchange](storage.md): the binary format, GraphSON, GraphML, change sets.
- [From Python](python.md): the `datetime` mapping.
- [TinkerPop Compatibility](tinkerpop.md): what is TinkerPop and what is an extension.
- [Dates in GQL](gql.md): the openCypher temporal functions of `gql()` (`date()`, `x.year`,
  `date.truncate()`, `duration.between()`) beside the steps of this part.

The steps have generated reference pages: [as_date](../traversal/steps/as_date.md),
[as_duration](../traversal/steps/as_duration.md), [date_add](../traversal/steps/date_add.md),
[date_diff](../traversal/steps/date_diff.md), [age](../traversal/steps/age.md),
[date_trunc](../traversal/steps/date_trunc.md), [date_part](../traversal/steps/date_part.md),
[at_zone](../traversal/steps/at_zone.md) and [as_local](../traversal/steps/as_local.md); the
constructors, `now()`, `today()` and `DT` are on
[Functions, Tokens and Values](../traversal/functions/tokens_and_values.md).
