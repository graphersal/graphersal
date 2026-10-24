# Conversions

A value becomes a date only through an explicit step; nothing is converted by guessing
(a string that looks like a date stays a string).

## `as_date()`: TinkerPop's `asDate()`

[`as_date()`](../traversal/steps/as_date.md) converts to a `datetime` exactly as TinkerPop's
`asDate()` does:

| Incoming | Result |
|---|---|
| a `datetime` | unchanged |
| a string | read leniently as ISO 8601 / RFC 3339: without an offset it is UTC, a date alone is midnight UTC ([Text Formats](text_formats.md#strict-and-lenient-a-datetime-without-an-offset)) |
| an `int64` | milliseconds since the epoch, in UTC |
| a `date`, a `localdatetime` | the `datetime` at UTC (Graphersal: TinkerPop has no such types) |
| anything else (a `float64`, `()`, a list, a map, a `time`, a `duration`, an element) | an error starting with "Can't parse", as in TinkerPop |

```rhai,graph=empty
g.inject("2023-08-02T09:30:00+02:00", 1690000000000, date("2023-08-02")).as_date().to_list()   // [2023-08-02T09:30:00+02:00, 2023-07-22T04:26:40Z, 2023-08-02T00:00:00Z]
g.inject(1.5).as_date().next()   // error: Can't parse float64 1.5 as a datetime
```

## `as_date(GType.X)`: to another date type

`as_date(GType.DATETIME | DATE | LOCALDATETIME | TIME | OFFSETTIME)` converts to that type (a
Graphersal extension, spelled like `as_number(GType.LONG)`):

| Incoming → | `DATETIME` | `DATE` | `LOCALDATETIME` | `TIME` | `OFFSETTIME` |
|---|---|---|---|---|---|
| string | lenient, as `as_date()` | strict: a date text | strict: a local date-time text | strict: a time text | strict: a time text with its offset |
| `datetime` | unchanged | its wall-clock date (at its own offset) | its wall clock | its wall-clock time | its wall-clock time and offset |
| `localdatetime` | at UTC | its date | unchanged | its time | its time at UTC |
| `date` | midnight UTC | unchanged | midnight | an error | an error |
| `time` | an error | an error | an error | unchanged | at UTC |
| `offsettime` | an error | an error | an error | its local time | unchanged |
| `int64` | epoch milliseconds, UTC | an error | an error | an error | an error |

```rhai,graph=empty
g.inject(datetime("2023-08-02T09:30+02:00")).as_date(GType.OFFSETTIME).next()   // offsettime("09:30:00+02:00")
g.inject(offsettime("09:30+02:00")).as_date(GType.TIME).next()   // time("09:30:00")
```

`GType.DURATION` is not a target here: use `as_duration()`. To read a `datetime`'s wall clock in
another zone, use `as_local(zone)` first ([Time Zones](time_zones.md)).

```rhai,graph=empty
g.inject(datetime("2023-08-02T23:30-07:00")).as_date(GType.DATE).next()   // date("2023-08-02")
g.inject(datetime("2023-08-02T23:30-07:00")).as_local("UTC").as_date(GType.DATE).next()   // date("2023-08-03")
g.inject("2023-08-02").as_date(GType.DATE).next()   // date("2023-08-02")
g.inject("2023-08-02T09:30").as_date(GType.DATE).next()   // error: unexpected text after the value
```

## `as_duration()`

[`as_duration()`](../traversal/steps/as_duration.md) reads an ISO 8601 duration text; a
`duration` passes unchanged, anything else fails with "Can't parse".

```rhai,graph=empty
g.inject("PT90M").as_duration().next()   // duration("PT1H30M")
```

## To numbers and text

- `as_number()` of a `datetime` is its epoch milliseconds (TinkerPop), rounded down; the other
  date types are not numbers (use `date_part(DT.epoch)` or `date_diff()`).
- `as_string()` of any date is its canonical text; in a script, `x.as_string()` does the same.
- `cast(GType.DATETIME)` (and the other date tokens) parses a string or, for `DATETIME`, reads an
  `int64` as epoch milliseconds, like `as_date(GType.X)`.

```rhai,graph=empty
g.inject(datetime("2023-08-02T00:00Z")).as_number().next()   // 1690934400000
g.inject(datetime("2023-08-02T09:30+02:00")).as_string().next()   // "2023-08-02T09:30:00+02:00"
g.inject(date("2023-08-02")).as_number().next()   // error: Cast exception
```

## Storing text as a date

A property written as a string stays a string. To store dates, declare the property in the
[schema](schema.md) (`"format": "date-time"`): in `open` and `closed` mode a string written to it
is converted on write. Or convert in the query: `property("at", __.constant(text).as_date())`, or
write a date value (`property("on", date("2023-08-02"))`).
