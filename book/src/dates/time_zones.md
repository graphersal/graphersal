# Time Zones

A `datetime` carries the UTC offset it was written with. Written with an IANA zone name
(`datetime("2024-03-30T12:00[Europe/Berlin]")`, a GQL `timezone: 'Europe/Berlin'`, a Python
`zoneinfo.ZoneInfo`), it keeps the name as well, and its calendar arithmetic follows the zone
([Zone names in a value](#zone-names-in-a-value)). An operation can also take a zone as an
argument and give back a `datetime` with the zone's offset at that instant, carrying the zone's
name when the argument is an IANA name: the PostgreSQL model (`AT TIME ZONE`, a session zone for
the calendar arithmetic), with the zone passed explicitly instead of a session setting, and the
`java.time` model (`atZoneSameInstant` gives a `ZonedDateTime` that keeps its zone).

## Zone arguments

A zone argument is one of:

- an **IANA zone name**: `"Europe/Berlin"`, `"America/New_York"`, `"UTC"`; a `datetime` the
  step returns keeps the name (`+02:00[Europe/Berlin]`, `Z[UTC]`);
- a **fixed offset**: `"+02:00"`, `"-0530"`, `"Z"`; a `datetime` the step returns has that
  offset and no name.

```rhai,graph=empty
g.inject(datetime("2023-08-02T12:00Z")).at_zone("Europe/Berlin").next()   // datetime("2023-08-02T14:00:00+02:00[Europe/Berlin]")
g.inject(datetime("2023-08-02T12:00Z")).at_zone("+02:00").next()   // datetime("2023-08-02T14:00:00+02:00")
```

The IANA rules come from the time zone database **bundled** with Graphersal (the `jiff-tzdb`
crate). Every machine, and the browser playground, gives the same answer to the same query, and
a replayed journal computes what it computed when it was written; the database changes only with
a Graphersal release, not with an operating system update. An unknown name is an error, found
before any value is read:

```rhai,graph=empty
g.inject(datetime("2023-08-02T12:00Z")).at_zone("Mars/Olympus").next()   // error: unknown time zone
```

The steps that take a zone: [`at_zone(zone)`](../traversal/steps/at_zone.md),
[`as_local(zone)`](../traversal/steps/as_local.md),
[`date_add(.., zone)`](../traversal/steps/date_add.md),
[`date_diff(.., zone)`](../traversal/steps/date_diff.md), [`age(.., zone)`](../traversal/steps/age.md),
[`date_trunc(.., zone)`](../traversal/steps/date_trunc.md),
[`date_part(.., zone)`](../traversal/steps/date_part.md), and `today(zone)`. Only a `datetime` (and
the `localdatetime` of `at_zone`) takes one; the other types have no instant, and a zone argument
for them is an error.

## Zone names in a value

A text with a zone in brackets (RFC 9557, `java.time` `ZonedDateTime`) keeps the zone's name
beside the offset; the value prints it back:

```rhai,graph=empty
datetime("2024-03-30T12:00[Europe/Berlin]")   // datetime("2024-03-30T12:00:00+01:00[Europe/Berlin]")
datetime("2024-03-30T12:00[Europe/Berlin]").timezone   // "Europe/Berlin"
datetime("2024-03-30T12:00+01:00").timezone   // "+01:00"
```

Such a value computes in its zone wherever a zone argument could be given and none is: adding
days, weeks, months or years keeps the wall-clock time and takes the zone's offset at the new
day, while hours and smaller units (and the time part of a duration) move the instant. A
difference in days or larger units, `age()`, `date_trunc()` and `date_part()` read the wall clock
of the zone. The result keeps the name.

```rhai,graph=empty
datetime("2024-03-30T12:00[Europe/Berlin]") + duration("P1D")   // datetime("2024-03-31T12:00:00+02:00[Europe/Berlin]")
datetime("2024-03-30T12:00[Europe/Berlin]") + duration("PT24H")   // datetime("2024-03-31T13:00:00+02:00[Europe/Berlin]")
g.inject(datetime("2024-03-31T12:00[Europe/Berlin]")).date_trunc(DT.day).next()   // datetime("2024-03-31T00:00:00+01:00[Europe/Berlin]")
```

A zone argument wins over the value's own zone (`at_zone()`, `date_add(.., zone)`, ...): the
result carries the argument's name when it is an IANA zone and no name when it is a fixed
offset, whatever zone the input had. To drop a name and keep the instant, pass the offset:
`at_zone("+02:00")`. Equality and order ignore the name, as they ignore the offset
([Comparing and Sorting](comparing.md)). The name is the database's spelling (`europe/berlin`
reads as `Europe/Berlin`); a fixed offset in brackets (`[+01:00]`) is no name. The store,
GraphSON, GraphML (through a schema), the change set and Python keep the name
([Storage and Exchange](storage.md)).

## Moving between a `datetime` and its wall clock

- `at_zone(zone)` on a `datetime`: the same instant, written with the zone's offset at it
  (PostgreSQL `AT TIME ZONE`, `java.time` `atZoneSameInstant`).
- `at_zone(zone)` on a `localdatetime`: the wall-clock time read in the zone, the `datetime` of
  that instant (`java.time` `atZone`).
- `as_local()` / `as_local(zone)`: the wall-clock `localdatetime` a `datetime` shows on its own
  offset or in the zone (`toLocalDateTime`).

```rhai,graph=empty
g.inject(datetime("2023-08-02T12:00Z")).at_zone("America/New_York").next()   // datetime("2023-08-02T08:00:00-04:00[America/New_York]")
g.inject(localdatetime("2023-08-02T12:00")).at_zone("+02:00").next()   // datetime("2023-08-02T12:00:00+02:00")
g.inject(datetime("2023-08-02T12:00Z")).as_local("Asia/Tokyo").next()   // localdatetime("2023-08-02T21:00:00")
```

## Daylight saving: gaps and folds

Where a zone moves its clocks, a wall-clock time can be missing (a spring-forward **gap**) or
happen twice (a fall-back **fold**). Graphersal resolves both as PostgreSQL and `java.time` do
(the "compatible" rule), never with an error:

- a time in a **gap** moves forward by the length of the gap: 02:30 on 2023-03-26 in Berlin
  becomes 03:30+02:00;
- a time in a **fold** takes the **earlier** offset: 02:30 on 2023-10-29 in Berlin is
  02:30+02:00, the first of the two.

```rhai,graph=empty
g.inject(localdatetime("2023-03-26T02:30")).at_zone("Europe/Berlin").next()   // datetime("2023-03-26T03:30:00+02:00[Europe/Berlin]")
g.inject(localdatetime("2023-10-29T02:30")).at_zone("Europe/Berlin").next()   // datetime("2023-10-29T02:30:00+02:00[Europe/Berlin]")
```

The same rule applies to a text with only a zone (`datetime("2023-03-26T02:30[Europe/Berlin]")`)
and to every result a zone computes on the wall clock.

## Calendar arithmetic in a zone

Without a zone a `datetime` is computed on its own fixed offset: `date_add(DT.day, 1)` adds 24
hours, as TinkerPop specifies. With a zone, calendar units work on the zone's wall clock, so a
day stays a calendar day across a daylight saving change (23 or 25 hours long), while hours stay
hours:

```rhai,graph=empty
g.inject(datetime("2023-03-25T12:00+01:00")).date_add(DT.day, 1).next()   // datetime("2023-03-26T12:00:00+01:00")
g.inject(datetime("2023-03-25T12:00+01:00")).date_add(DT.day, 1, "Europe/Berlin").next()   // datetime("2023-03-26T12:00:00+02:00[Europe/Berlin]")
g.inject(datetime("2023-03-25T12:00+01:00")).date_add(DT.hour, 24, "Europe/Berlin").next()   // datetime("2023-03-26T13:00:00+02:00[Europe/Berlin]")
```

The same holds for `date_diff()` in days and larger units, `age()`, `date_trunc()` and
`date_part()`: they read the wall clock of the zone. Without a zone, `date_diff()` and `age()`
read both operands on the wall clock of the second operand's offset:

```rhai,graph=empty
g.inject(datetime("2023-03-26T12:00+02:00")).date_diff(datetime("2023-03-25T12:00+01:00"), DT.day).next()   // 0 (23 hours)
g.inject(datetime("2023-03-26T12:00+02:00")).date_diff(datetime("2023-03-25T12:00+01:00"), DT.day, "Europe/Berlin").next()   // 1
g.inject(datetime("2023-08-02T12:00Z")).date_trunc(DT.day, "America/New_York").next()   // datetime("2023-08-02T00:00:00-04:00[America/New_York]")
```

GQL has no zone argument of a step: a zone name in its text (`datetime('..[Europe/Berlin]')`),
in a map (`timezone: 'Europe/Berlin'`) or given to a clock (`datetime('Europe/Berlin')`) is kept
in the value, as Neo4j keeps it, and its arithmetic then follows the zone
([Dates in GQL](gql.md)).
