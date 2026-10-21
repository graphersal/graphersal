# Time Zones

A time zone is never stored in a value. A `datetime` carries a fixed UTC offset (the one it was
written with); an operation that needs a zone takes it as an argument and gives back a `datetime`
with the zone's offset at that instant. This is the PostgreSQL model (`AT TIME ZONE`, a session
zone for the calendar arithmetic), with the zone passed explicitly instead of a session setting.

## Zone arguments

A zone argument is one of:

- an **IANA zone name**: `"Europe/Berlin"`, `"America/New_York"`, `"UTC"`;
- a **fixed offset**: `"+02:00"`, `"-0530"`, `"Z"`.

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

## Moving between a `datetime` and its wall clock

- `at_zone(zone)` on a `datetime`: the same instant, written with the zone's offset at it
  (PostgreSQL `AT TIME ZONE`, `java.time` `atZoneSameInstant`).
- `at_zone(zone)` on a `localdatetime`: the wall-clock time read in the zone, the `datetime` of
  that instant (`java.time` `atZone`).
- `as_local()` / `as_local(zone)`: the wall-clock `localdatetime` a `datetime` shows on its own
  offset or in the zone (`toLocalDateTime`).

```rhai,graph=empty
g.inject(datetime("2023-08-02T12:00Z")).at_zone("America/New_York").next()   // datetime("2023-08-02T08:00:00-04:00")
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
g.inject(localdatetime("2023-03-26T02:30")).at_zone("Europe/Berlin").next()   // datetime("2023-03-26T03:30:00+02:00")
g.inject(localdatetime("2023-10-29T02:30")).at_zone("Europe/Berlin").next()   // datetime("2023-10-29T02:30:00+02:00")
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
g.inject(datetime("2023-03-25T12:00+01:00")).date_add(DT.day, 1, "Europe/Berlin").next()   // datetime("2023-03-26T12:00:00+02:00")
g.inject(datetime("2023-03-25T12:00+01:00")).date_add(DT.hour, 24, "Europe/Berlin").next()   // datetime("2023-03-26T13:00:00+02:00")
```

The same holds for `date_diff()` in days and larger units, `age()`, `date_trunc()` and
`date_part()`: they read the wall clock of the zone. Without a zone, `date_diff()` and `age()`
read both operands on the wall clock of the second operand's offset:

```rhai,graph=empty
g.inject(datetime("2023-03-26T12:00+02:00")).date_diff(datetime("2023-03-25T12:00+01:00"), DT.day).next()   // 0 (23 hours)
g.inject(datetime("2023-03-26T12:00+02:00")).date_diff(datetime("2023-03-25T12:00+01:00"), DT.day, "Europe/Berlin").next()   // 1
g.inject(datetime("2023-08-02T12:00Z")).date_trunc(DT.day, "America/New_York").next()   // datetime("2023-08-02T00:00:00-04:00")
```

GQL has no zone argument: a zone name in its text (`datetime('..[Europe/Berlin]')`) or in a map
(`timezone: 'Europe/Berlin'`) resolves to the zone's offset when the value is built, and its
arithmetic then stays at that offset, unlike Neo4j, which keeps the name
([Dates in GQL](gql.md), [Semantics](../gql/semantics.md#where-dates-and-times-differ-from-neo4j)).
