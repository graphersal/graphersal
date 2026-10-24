# Truncation and Parts

[`date_trunc(DT.unit)`](../traversal/steps/date_trunc.md) is PostgreSQL's `date_trunc`, and
[`date_part(DT.unit)`](../traversal/steps/date_part.md) is its `extract`. Both are Graphersal
extensions; TinkerPop has neither. GQL's `X.truncate()` and `x.year` do the same with Neo4j's
names, but truncate millennia and centuries as Neo4j does ([Dates in GQL](gql.md#truncation)). A `datetime` is read on its own wall clock, or on the wall
clock of a zone given as the last argument ([Time Zones](time_zones.md)).

## `date_trunc()`

The fields below the unit become their first value: the start of the hour, the day, the month.
A truncated `datetime` keeps its offset (or, with a zone, takes the zone's offset at the new
instant, and the zone's name when it is an IANA zone).

```rhai,graph=empty
g.inject(datetime("2023-08-17T15:47:12.5+02:00")).date_trunc(DT.hour).next()   // datetime("2023-08-17T15:00:00+02:00")
g.inject(datetime("2023-08-17T15:47:12.5+02:00")).date_trunc(DT.week).next()   // datetime("2023-08-14T00:00:00+02:00")
g.inject(datetime("2023-08-17T15:47:12.5+02:00")).date_trunc(DT.day, "UTC").next()   // datetime("2023-08-17T00:00:00Z")
g.inject(date("2023-08-17")).date_trunc(DT.quarter).next()   // date("2023-07-01")
g.inject(duration("P1Y2M3DT4H5M")).date_trunc(DT.day).next()   // duration("P1Y2M3D")
```

| Unit | Truncates to |
|---|---|
| `nanosecond` ... `hour` | the start of that unit of the clock (`nanosecond` keeps the value) |
| `day` | midnight |
| `week` | Monday, midnight (ISO 8601 weeks) |
| `month`, `quarter`, `year` | the first day of the month, of the quarter (January, April, July, October), of the year |
| `decade` | the first day of a year divisible by 10 (2020-01-01) |
| `century`, `millennium` | the first day of a year ending in `01`/`001`: centuries and millennia start at year 1, as PostgreSQL counts them (2001-01-01 starts the 21st century) |

A `date` takes `day` and larger, a `time` and an `offsettime` `hour` and smaller (the offset
stays), a `duration` keeps the fields at
and above the unit (`month` and larger keep whole multiples of the unit's months and drop the days
and the time part).

## `date_part()`

One field as a number: an `int64`, except `DT.epoch`, a `float64` of seconds.

| Part | Value |
|---|---|
| `DT.year`, `DT.month` (1-12), `DT.day` (1-31), `DT.hour`, `DT.minute`, `DT.second` (0-59) | the field |
| `DT.millisecond` (0-999), `DT.microsecond` (0-999999), `DT.nanosecond` (0-999999999) | the fraction of the second in that unit; unlike PostgreSQL, not the seconds with the fraction |
| `DT.quarter` (1-4) | the quarter |
| `DT.week` (1-53) | the ISO 8601 week number |
| `DT.iso_year` | the ISO 8601 week-numbering year (2023-01-01 is in week 52 of 2022) |
| `DT.day_of_week` | 0 = Sunday to 6 = Saturday (PostgreSQL `dow`) |
| `DT.iso_day_of_week` | 1 = Monday to 7 = Sunday (PostgreSQL `isodow`) |
| `DT.day_of_year` (1-366) | the day of the year |
| `DT.decade`, `DT.century`, `DT.millennium` | as PostgreSQL counts them (the 21st century is 2001-2100) |
| `DT.epoch` | seconds since 1970-01-01T00:00:00Z (a `localdatetime` read as UTC); for a `duration` its length with a year of 365.25 days and a month of 30 days |
| `DT.offset` | the UTC offset of a `datetime` or an `offsettime` in seconds |

```rhai,graph=empty
g.inject(date("2023-08-17")).date_part(DT.day_of_week).next()   // 4
g.inject(datetime("2023-01-01T15:47:12.5+02:00")).date_part(DT.week).next()   // 52
g.inject(datetime("2023-01-01T15:47:12.5+02:00")).date_part(DT.iso_year).next()   // 2022
g.inject(datetime("2023-08-17T15:47:12.5+02:00")).date_part(DT.epoch).next()   // 1692280032.5
g.inject(datetime("2023-08-17T15:47:12.5+02:00")).date_part(DT.offset).next()   // 7200
g.inject(datetime("2023-08-02T02:00Z")).date_part(DT.day, "America/New_York").next()   // 1
```

A `date` has no clock parts (`hour` and smaller), a `time` only those (and `epoch`, the seconds
since midnight). A `duration` gives PostgreSQL's `interval` fields: `year` and `month` from its
months (14 months are 1 year and 2 months), `day` its days, `hour` and smaller from its time part;
it has no `week`, day-of parts, `iso_year` or `offset`:

```rhai,graph=empty
g.inject(duration("P1Y2M3DT4H5M6.5S")).date_part(DT.month).next()   // 2
g.inject(date("2023-08-17")).date_part(DT.hour).next()   // error: the part hour is not defined for a date
```
