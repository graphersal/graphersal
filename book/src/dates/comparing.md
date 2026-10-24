# Comparing and Sorting

Every comparison of dates goes through one set of rules: `==` in a script, the predicates
(`P.eq`, `P.gt`, `P.between`, `P.within`, ...), `dedup()`, `group()` and `group_count()` keys,
`order()`, `min()`/`max()` and the [schema bounds](schema.md).

## `datetime`: by its instant

Two `datetime`s are equal when they are the same instant, whatever their offsets
(`12:00Z` equals `14:00+02:00`). This is PostgreSQL's equality and TinkerPop's ("dates are
evaluated based on the numerical comparison of Unix Epoch time"). The offset is kept and shown,
but never compared:

```rhai,graph=empty
datetime("2023-08-02T12:00Z") == datetime("2023-08-02T14:00+02:00")   // true
g.inject(datetime("2023-08-02T12:00Z"), datetime("2023-08-02T14:00+02:00")).dedup().count().next()   // 1
g.inject(datetime("2023-08-02T14:00+02:00")).is(P.gt(datetime("2023-08-02T11:00Z"))).count().next()   // 1
```

A zone name is shown but never compared either: `2023-08-02T14:00+02:00[Europe/Berlin]` equals
`2023-08-02T12:00Z` (as Neo4j and the openCypher TCK compare instants).

```rhai,graph=empty
datetime("2023-08-02T14:00[Europe/Berlin]") == datetime("2023-08-02T12:00Z")   // true
```

Of two values with the same instant and different offsets the first one met is kept (`dedup()`,
a `group()` key); their order in a sort is unspecified. To compare the wall clocks instead, convert
first: `as_local()` gives the wall-clock `localdatetime` ([Time Zones](time_zones.md)).

## `offsettime`: by its UTC time of day

Two `offsettime`s are equal when they are the same time of day in UTC (`12:00+01:00` equals
`11:00Z`), and order by it, as a `datetime` compares by its instant (`java.time`
`OffsetTime.isEqual` / `isBefore`). The offset is kept and shown, never compared; the UTC time is
not wrapped around midnight (`01:00+02:00` is before `00:00Z`, the previous day's 23:00 in UTC).

```rhai,graph=empty
offsettime("12:00+01:00") == offsettime("11:00Z")   // true
offsettime("01:00+02:00") < offsettime("00:00Z")   // true
```

## `duration`: field-wise equal, ordered by length

Two `duration`s are equal when their months, days and nanoseconds are equal: `P1M` is not
`P30D`, and `P1D` is not `PT24H` (a day in a time zone may have 23 or 25 hours). Sorting
compares their length as PostgreSQL does for an `interval`, counting a month as 30 days and a day
as 24 hours; equal lengths are ordered by months, then days, then the time part:

```rhai,graph=empty
duration("P1M") == duration("P30D")   // false
g.inject(duration("P1M"), duration("P30D"), duration("PT719H")).order().to_list()   // [PT719H, P30D, P1M]
```

## The other types: by calendar and clock

A `date`, a `localdatetime` and a `time` compare by their calendar or clock value.

```rhai,graph=empty
g.inject(time("10:00")).is(P.between(time("09:00"), time("17:00"))).count().next()   // 1
g.inject(date("2023-08-02")).is(P.within(date("2023-08-02"), date("2023-08-03"))).count().next()   // 1
```

## Different types

Values of different types are never equal, and an ordering predicate between them does not match
(no error): a `date` is not a `datetime`, and a date is never equal to a string, as a UUID is not.
Convert one side first ([Conversions](conversions.md)).

```rhai,graph=empty
g.inject(date("2023-08-02")).is(P.eq("2023-08-02")).count().next()   // 0
g.inject(date("2023-08-02")).is(P.lt(datetime("2024-01-01T00:00Z"))).count().next()   // 0
g.inject(date("2023-08-02")).as_date().is(P.lt(datetime("2024-01-01T00:00Z"))).count().next()   // 1
```

`order()` of a mixed stream puts the dates after the numbers and before the strings (TinkerPop's
orderability places its date type there), in the order `date`, `localdatetime`, `datetime`,
`time`, `duration`, `offsettime`:

```rhai,graph=empty
g.inject("x", 1, duration("P1D"), time("01:00"), datetime("2023-08-02T00:00Z"), date("2023-08-02")).order().to_list()
```

## Property indexes

A [property index](../users_guide/property_indexes.md) on a key holding dates and times serves
`has()` with these same rules: an equality or `P.within` reads the values equal to the operand
(a `datetime` by its instant, an `offsettime` by its UTC time of day, a `duration` field by
field), a range (`P.gt`, `P.between`, ...)
reads the values of the operand's type in that range. The results are always the same as without
the index.

```rhai,graph=modern,script
g.create_index(#{label: "person", key: "joined"});
g.v().has("name", "marko").property("joined", datetime("2023-08-02T12:00Z")).iterate();
g.v().has_label("person").has("joined", datetime("2023-08-02T14:00+02:00")).count().next()   // 1
```

