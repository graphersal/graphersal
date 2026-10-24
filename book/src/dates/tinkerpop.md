# TinkerPop Compatibility

TinkerPop 3.8 has one date type, `datetime` (a Java `OffsetDateTime`), the literal `datetime(..)`,
the steps `asDate()`, `dateAdd()` and `dateDiff()`, the units `DT.second`, `DT.minute`, `DT.hour`
and `DT.day`, and the tokens `GType.DATETIME` and `GType.DURATION` (a Java `Duration`, which no
TinkerPop step produces). Graphersal implements all of it as TinkerPop specifies, with no
deviation:

- `datetime("..")` (also spelled `DateTime("..")`) reads ISO 8601 text, a text without an offset
  as UTC;
- `asDate()` converts text and epoch milliseconds, and fails with "Can't parse" for the
  same inputs as TinkerPop;
- `dateAdd(DT.unit, n)` keeps the offset and moves the instant, a day being 24 hours;
- `dateDiff(other)` is the difference in milliseconds, `null` being the epoch;
- `asNumber()` of a `datetime` is its epoch milliseconds;
- equality and ordering use the instant ("Dates are evaluated based on the numerical comparison of
  Unix Epoch time"), and dates sort after numbers and before strings.

Every date scenario of the vendored TinkerPop 3.8.2 suite passes (`DateTime`, `AsDate`,
`DateAdd`, `DateDiff`, the date cases of `TypeOf` and `AsNumber`), except two `Orderability`
scenarios that also hold a set literal (`{"x", "y", "z"}`), for which Graphersal has no value type
([TinkerPop Compliance](../for_developers/tinkerpop_compliance.md)).

## Extensions

Everything else on the pages of this part is a Graphersal extension, modelled on PostgreSQL:

- the types `date`, `localdatetime`, `time` and `offsettime` (`GType.DATE`, `LOCALDATETIME`,
  `TIME`, `OFFSETTIME`; `typeOf` names `LocalDate`, `LocalDateTime`, `LocalTime`, `OffsetTime`),
  and `duration` as a value with months, days and nanoseconds (PostgreSQL's `interval`, broader
  than Java's `Duration`), with their constructors;
- a zone name kept in a `datetime` written with one (`datetime("..[Europe/Berlin]")`, Java's
  `ZonedDateTime`, which TinkerPop's `datetime()` does not produce): its calendar arithmetic
  follows the zone; a `datetime` without a name behaves as TinkerPop specifies;
- the units `DT.nanosecond`, `DT.microsecond`, `DT.millisecond`, `DT.week` to `DT.millennium`
  and the parts
  `DT.day_of_week`, `DT.iso_day_of_week`, `DT.day_of_year`, `DT.iso_year`, `DT.epoch`,
  `DT.offset`;
- `date_add(duration)`, zone arguments, `date_diff(other, DT.unit)`, `age()`, `date_trunc()`,
  `date_part()`, `at_zone()`, `as_local()`, `as_duration()`, `as_date(GType.X)`;
- `now()`, `today()`, the getters and operators of script values;
- the schema formats and bounds, GraphSON's `java.time` types beyond `g:Date`, and the
  [Python mapping](python.md).

The extensions add behaviour where TinkerPop defines none; none of them changes what a TinkerPop
step returns. The one deliberate difference in the exchange formats is GraphSON's
`graphersal:Interval` for a `duration` that no `java.time` type can hold
([Storage and Exchange](storage.md#graphson)); all deviations are listed on
[TinkerPop Deviations](../for_developers/tinkerpop_deviations.md).
