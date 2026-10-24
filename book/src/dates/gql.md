# Dates in GQL

[`gql()`](../gql/index.md) has openCypher's temporal functions (`date()`, `datetime()`,
`duration()`, `x.year`, `date.truncate()`, `duration.between()`, ...) with Neo4j's meaning. They
build and read the same five values as the Gremlin steps of this part: a date a GQL query returns
is a `date` value, and a date a Gremlin step stored reads as a date in GQL. The full list of
forms is on [The Supported Subset](../gql/subset.md#dates-and-times), the differences from Neo4j
on [Semantics](../gql/semantics.md#where-dates-and-times-differ-from-neo4j).

The Gremlin steps keep their PostgreSQL-style rules; where a GQL function means something else,
the GQL function changes, never the step. This page sets each GQL form beside the Gremlin step or
Rhai literal that does the same, and names the differences.

```rhai,graph=empty
g.gql("RETURN date('2024-03-15') AS d").select("d").next() == date("2024-03-15")   // true
g.gql("RETURN $d + duration('P1M') AS d", #{d: date("2024-01-31")}).to_list()   // #{"d": date("2024-02-29")}
```

```rhai,graph=empty,script
g.add_v("event").property("at", date("2024-03-15")).iterate();
g.gql("MATCH (e:event) RETURN e.at.year AS y, e.at + duration('P1M') AS next").to_list()   // #{"next": date("2024-04-15"), "y": 2024}
```

## Building values

| GQL | Gremlin / Rhai | Difference |
|---|---|---|
| `date('2015-07-21')`, `localtime('21:40')`, `localdatetime('..')`, `datetime('..')`, `duration('P1D')` | the literals `date("2015-07-21")`, `time("21:40")`, `localdatetime("..")`, `datetime("..")`, `duration("P1D")` | GQL also reads Neo4j's forms: week, ordinal and quarter dates (`2015-W30-2`, `2015-202`, `2015-Q3-21`), basic forms (`20150721T2140`), a zone in brackets (`[Europe/London]`), fractions on any duration unit (`P1.5Y`); digits below the microsecond are `0A000` in GQL and cut in Rhai |
| `date({year: 2015, week: 30, dayOfWeek: 2})` and the other maps | none: build the text, or `date_add()` from a start | |
| `date(x)`, `localdatetime(x)`, `datetime(x)` of another temporal value | [`as_date(GType.DATE)`](conversions.md#as_dategtypex-to-another-date-type), `as_date(GType.LOCALDATETIME)`, `as_date(GType.DATETIME)` | the same projections (the date of a `datetime` at its own offset, a `localdatetime` read at UTC); GQL's `localdatetime(date)` and `datetime(date)` are errors as in Neo4j (`localdatetime({date: d})` is midnight), `as_date(GType.LOCALDATETIME)` of a date is midnight |
| `datetime({.., timezone: 'Europe/Berlin'})`, `datetime({datetime: x, timezone: z})` | [`at_zone(z)`](time_zones.md#moving-between-a-datetime-and-its-wall-clock) | both resolve a zone name to its offset at that instant |
| `datetime.fromepochmillis(ms)`, `datetime({epochMillis: ms})`, `datetime({epochSeconds: s})` | [`as_date()`](conversions.md) of an `int64` (epoch milliseconds) | |
| `date()`, `datetime()`, `X.statement()`, `X.transaction()` | [`today()`, `now()`](now_and_today.md) | both are one instant per run; a query run from a script reads the script's instant, so `datetime()` in `g.gql(..)` equals the script's `now()` (a host's pinned clock too); run alone, a query takes it when it starts |

```rhai,graph=empty
g.gql("RETURN date(datetime('2015-07-21T23:40:32+02:00')) AS d").to_list()   // #{"d": date("2015-07-21")}
g.inject(datetime("2015-07-21T23:40:32+02:00")).as_date(GType.DATE).next()   // date("2015-07-21")
```

## Reading components

| GQL | Gremlin / Rhai | Difference |
|---|---|---|
| `x.year`, `x.month`, `x.day`, `x.hour`, `x.minute`, `x.second` | [`date_part(DT.year)`](truncation_and_parts.md#date_part), ..., or the getters `d.year` of a script value | the same |
| `x.quarter`, `x.week`, `x.weekYear`, `x.ordinalDay` | `date_part(DT.quarter)`, `DT.week`, `DT.iso_year`, `DT.day_of_year` | the same (ISO 8601 weeks) |
| `x.dayOfWeek` (1 = Monday) | `date_part(DT.iso_day_of_week)` | `DT.day_of_week` is PostgreSQL's 0 = Sunday |
| `x.millisecond`, `x.microsecond`, `x.nanosecond` | `date_part(DT.millisecond)`, `DT.microsecond`, `DT.nanosecond` | the same: the fraction of the second |
| `x.epochSeconds`, `x.epochMillis` | `date_part(DT.epoch)` (seconds with the fraction), the getter `epoch_millis` | |
| `x.offset`, `x.timezone` (`'+01:00'`), `x.offsetSeconds` | `date_part(DT.offset)` (seconds) | GQL writes the offset as text |
| `dur.months`, `dur.years`, `dur.days`, `dur.hours`, ... (totals) | `date_part(DT.month)` of a duration is the remainder field (PostgreSQL: `P1Y` has month 0); the getters `months`, `days`, `micros` | GQL's names are Neo4j's totals (`duration('P1Y').months` is 12); its remainders are `monthsOfYear`, `minutesOfHour`, ... |

```rhai,graph=empty
g.gql("RETURN date('2015-07-21').dayOfWeek AS a, duration('P1Y').months AS b").to_list()   // #{"a": 2, "b": 12}
g.inject(date("2015-07-21")).date_part(DT.iso_day_of_week).next()   // 2
g.inject(duration("P1Y")).date_part(DT.month).next()   // 0
```

## Truncation

| GQL | Gremlin | Difference |
|---|---|---|
| `date.truncate('month', d)`, `localdatetime.truncate(..)`, `datetime.truncate(..)`, `localtime.truncate(..)` | [`date_trunc(DT.month)`](truncation_and_parts.md#date_trunc) | the same for `year` down to `microsecond` and `week` |
| `X.truncate('millennium' \| 'century', d)` | `date_trunc(DT.millennium)`, `DT.century` | GQL follows Neo4j (2017 gives 2000, 1984 gives 1900), Gremlin PostgreSQL (2001, 1901) |
| `X.truncate('weekYear', d)`, a map of overrides (`{dayOfWeek: 2}`) | none | |
| `datetime.truncate(unit, d, {timezone: z})` | `date_trunc(unit, z)` | GQL keeps the wall clock and reads it in `z`; Gremlin truncates on the wall clock of `z` |

```rhai,graph=empty
g.gql("RETURN date.truncate('millennium', date('2017-10-11')) AS d").to_list()   // #{"d": date("2000-01-01")}
g.inject(date("2017-10-11")).date_trunc(DT.millennium).next()   // date("2001-01-01")
```

## Arithmetic

| GQL | Gremlin / Rhai | Difference |
|---|---|---|
| `x + dur`, `x - dur` | [`date_add(dur)`](arithmetic.md#date_add), the Rhai operators `x + dur` | the same order (months clamped, days, time); but in GQL a `date` takes the whole days of a time part (`date + PT33H` adds a day) and a `localtime` ignores months and days, where `date_add` is an error |
| `dur + dur`, `dur * n`, `dur / n` | `date_add(dur)` on a duration (field by field) | GQL scales with the fraction cascade (`P1M * 0.5` is `P15DT5H14M33S`) |
| `duration.between(a, b)` | [`age()`](arithmetic.md#age) (the operands swapped: `age` counts from the incoming value back to `other`) | GQL counts whole months, then days, then time, each part signed (Neo4j); `age` borrows as PostgreSQL does; GQL takes two different types, `age` one type |
| `duration.inMonths(a, b)`, `duration.inDays(a, b)` | [`date_diff(other, DT.month)`](arithmetic.md#date_diff), `DT.day` (a number, the incoming value minus `other`) | GQL gives a duration (`P30Y8M`), `date_diff` an integer (368) |
| `duration.inSeconds(a, b)` | `date_diff(other)` (the milliseconds, an integer) | GQL gives a duration of seconds only (`PT24H`) |

```rhai,graph=empty
g.gql("RETURN date('1997-09-11') + duration('P29DT33H') AS d").to_list()   // #{"d": date("1997-10-11")}
g.inject(date("1997-09-11")).date_add(duration("P29DT33H")).next()   // error: adding a duration with a time part is not defined for a date
g.gql("RETURN duration.between(date('1984-10-11'), date('2015-06-24')) AS d").to_list()   // #{"d": duration("P30Y8M13D")}
g.inject(date("2015-06-24")).age(date("1984-10-11")).next()   // duration("P30Y8M13D")
g.inject(date("2015-06-24")).date_diff(date("1984-10-11"), DT.month).next()   // 368
```

## Comparing and text

| GQL | Gremlin | Difference |
|---|---|---|
| `a = b`, `a < b` on one type | `is(P.eq(..))`, `is(P.lt(..))` ([Comparing and Sorting](comparing.md)) | the same; two `datetime`s by their instant |
| `a < b` on two durations | `is(P.lt(..))` orders by length | GQL gives `NULL`; `ORDER BY` and `order()` sort by length |
| `a = b`, `a < b` on two different temporal types | | GQL: `=` is FALSE, `<` is `NULL`, as for any two kinds |
| `toString(x)` | [`as_string()`](conversions.md#to-numbers-and-text) | GQL writes Neo4j's text (`'12:00'`), `as_string()` the canonical ISO text (`"12:00:00"`), as GQL's `CAST(x AS STRING)` does |

```rhai,graph=empty
g.gql("RETURN toString(localtime('12:00')) AS a, CAST(localtime('12:00') AS STRING) AS b").to_list()   // #{"a": "12:00", "b": "12:00:00"}
g.inject(time("12:00")).as_string().next()   // "12:00:00"
```

## Not in GQL yet

Cypher's zoned `time()` (a time of day with an offset; neither side has the type) and named
zones kept in a value: [Semantics](../gql/semantics.md#where-dates-and-times-differ-from-neo4j).
