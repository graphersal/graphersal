# Arithmetic

Three steps compute with dates: [`date_add()`](../traversal/steps/date_add.md) moves a value,
[`date_diff()`](../traversal/steps/date_diff.md) measures the distance between two values as a
number, [`age()`](../traversal/steps/age.md) as a `duration`. All of them are checked: a result
outside the years -9999 to 9999 is an error, never a wrapped value. In a script the operators `+`
and `-` do the same ([script values](index.md#script-values)). In GQL, `+` with a duration and
`duration.between()` follow Neo4j's rules instead ([Dates in GQL](gql.md#arithmetic)).

## Units: the `DT` token

`DT.second`, `DT.minute`, `DT.hour` and `DT.day` are TinkerPop's (the bare names `second`,
`minute`, `hour` and `day` work too, as in the TinkerPop scenarios). Graphersal adds the smaller
and the calendar units, with PostgreSQL's field names:

| Unit | Kind | Adding it |
|---|---|---|
| `DT.nanosecond`, `DT.microsecond`, `DT.millisecond`, `DT.second`, `DT.minute`, `DT.hour` | a fixed length | moves the instant (or the clock) by that many units |
| `DT.day`, `DT.week` | calendar days | moves the wall-clock date; in a zone the time of day is kept across a daylight saving change |
| `DT.month`, `DT.quarter`, `DT.year`, `DT.decade`, `DT.century`, `DT.millennium` | calendar months (1, 3, 12, 120, 1200, 12000) | moves the month and clamps the day to the end of the target month |

`DT.day_of_week`, `DT.iso_day_of_week`, `DT.day_of_year`, `DT.iso_year`, `DT.epoch` and
`DT.offset` are parts, not units: only [`date_part()`](truncation_and_parts.md) takes them.

## `date_add()`

`date_add(DT.unit, n)` adds `n` units (a negative `n` subtracts); `date_add(duration)` adds a
`duration`. Both take an optional zone as the last argument ([Time Zones](time_zones.md)).

```rhai,graph=empty
g.inject(datetime("2023-08-02T12:00Z")).date_add(DT.hour, 36).next()   // datetime("2023-08-04T00:00:00Z")
g.inject(date("2023-08-02")).date_add(day, 3).next()   // date("2023-08-05")
g.inject(datetime("2023-08-02T12:00Z")).date_add(DT.millisecond, 1500).next()   // datetime("2023-08-02T12:00:01.500Z")
```

**Month ends.** Adding months keeps the day of the month where it exists and otherwise takes the
last day of the target month, as PostgreSQL and `java.time` do. Each step clamps on its own, so
two single months can end earlier than two months at once:

```rhai,graph=empty
g.inject(date("2024-01-31")).date_add(DT.month, 1).next()   // date("2024-02-29")
g.inject(date("2024-01-31")).date_add(DT.month, 2).next()   // date("2024-03-31")
g.inject(date("2024-01-31")).date_add(DT.month, 1).date_add(DT.month, 1).next()   // date("2024-03-29")
```

**A duration** is added in PostgreSQL's order: its months first (clamped), then its days, then
its time part.

```rhai,graph=empty
g.inject(datetime("2023-08-02T12:00Z")).date_add(duration("P1M1DT1H")).next()   // datetime("2023-09-03T13:00:00Z")
g.inject(date("2024-02-29")).date_add(duration("P1Y1D")).next()   // date("2025-03-01")
```

What each type accepts:

| Type | `DT` units | A duration |
|---|---|---|
| `datetime` | all units; a fixed length moves the instant and keeps the offset (TinkerPop: a day is 24 hours at a fixed offset), with a zone the result takes the zone's offset; a `datetime` with a zone name computes in its zone and keeps the name ([Time Zones](time_zones.md#zone-names-in-a-value)) | any |
| `localdatetime` | all units, on the wall clock | any |
| `date` | `day` and larger | without a time part |
| `time` | `hour` and smaller; the clock wraps around midnight (PostgreSQL `time + interval`) | without months or days |
| `offsettime` | as a `time`, on its local time; the offset stays | without months or days |
| `duration` | all units: adds to the matching field | any (field by field) |

```rhai,graph=empty
g.inject(time("23:00")).date_add(DT.hour, 2).next()   // time("01:00:00")
g.inject(date("2024-02-29")).date_add(DT.hour, 2).next()   // error: adding the unit hour is not defined for a date
```

## `date_diff()`

`date_diff(other)` is TinkerPop's `dateDiff()`: the incoming value minus `other` in
**milliseconds**, an `int64`, each operand's epoch milliseconds rounded down. `other` is a value or
a child traversal whose first result is the operand; `()` (null) stands for the epoch.

```rhai,graph=empty
g.inject(datetime("2023-08-02T12:00Z")).date_diff(datetime("2023-08-01T00:00Z")).next()   // 129600000
g.inject(datetime("2023-08-02T12:00Z")).date_diff(__.constant(datetime("2023-08-01T00:00Z"))).next()   // 129600000
g.inject(datetime("1970-01-01T00:00:01Z")).date_diff(()).next()   // 1000
```

Graphersal extends it to the other types (both operands of one type: a `date` counts from
midnight, a `time` within the day; `()` is 1970-01-01 or midnight) and adds
`date_diff(other, DT.unit[, zone])`: the difference in **whole units**, truncated toward zero.

- Units below a day count the elapsed time.
- Days and weeks count wall-clock days (a `datetime` on the wall clock of `other`'s offset, or of
  the zone).
- Months and larger count as `java.time`'s `ChronoUnit.between`: a month is complete when the
  day of the month (and the time) is reached again, so January 31 to February 28 is 0 months.

```rhai,graph=empty
g.inject(datetime("2023-08-02T12:00Z")).date_diff(datetime("2023-08-01T00:00Z"), DT.hour).next()   // 36
g.inject(time("10:00")).date_diff(time("09:00"), DT.minute).next()   // 60
g.inject(date("2024-02-28")).date_diff(date("2024-01-31"), DT.month).next()   // 0
g.inject(date("2024-03-31")).date_diff(date("2024-02-29"), DT.month).next()   // 1
```

## `age()`

`age(other[, zone])` is PostgreSQL's `age`: the difference as a `duration` of whole years and
months, days and a time part, all with the same sign. It is counted down from the incoming value
to `other` with PostgreSQL's borrowing rules, so swapping the operands negates the result. Both
operands have the same type; `()` is an error.

```rhai,graph=empty
g.inject(date("2001-04-10")).age(date("1957-06-13")).next()   // duration("P43Y9M27D")
g.inject(date("1957-06-13")).age(date("2001-04-10")).next()   // duration("-P43Y9M27D")
g.inject(datetime("2024-02-29T00:00Z")).age(datetime("2023-01-31T12:00Z")).next()   // duration("P1Y28DT12H")
```

To get a plain number of days, years or hours, use `date_diff(other, DT.unit)`; to get a
`duration` of elapsed time without calendar units, subtract the two values in a script
(`a - b`).
