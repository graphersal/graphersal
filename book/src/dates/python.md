# From Python

The Python binding converts the five types to and from the standard `datetime` module, both as
results and as `params=`, to the microsecond (the resolution of Python's types). A value with a
part below the microsecond (`time("12:00:00.000000001")`) raises `ValueError` instead of being
rounded; truncate it first with `date_trunc(DT.microsecond)`:

| Graphersal | Python |
|---|---|
| `datetime` | aware `datetime.datetime` with `tzinfo=datetime.timezone(timedelta(seconds=offset))` |
| `localdatetime` | naive `datetime.datetime` |
| `date` | `datetime.date` |
| `time` | naive `datetime.time` |
| `duration` without months | `datetime.timedelta` |
| `duration` with months | `graphersal.Interval(months, days, micros)` |

```python
import datetime as dt
import graphersal

graph = graphersal.Graph()
at = dt.datetime(2023, 8, 2, 9, 30, tzinfo=dt.timezone(dt.timedelta(hours=2)))
graph.query("g.inject(x).date_add(DT.day, 1).next()", params={"x": at})
# datetime.datetime(2023, 8, 3, 9, 30, tzinfo=datetime.timezone(datetime.timedelta(seconds=7200)))
graph.query('duration("P1Y2M")')
# Interval('P1Y2M')
```

- An aware `datetime` with any `tzinfo` (a `zoneinfo.ZoneInfo` too) is passed with the UTC offset
  it has at that instant; the zone name is not kept. An offset with a fraction of a second is
  refused.
- An aware `datetime.time` is refused: a time of day has no instant without a date.
- A `timedelta` becomes whole days and the rest, both with the sign of the whole
  (`timedelta(hours=-1)` is `-PT1H`).
- `graphersal.Interval` is immutable, equal when its three fields are equal (`Interval("P1M") !=
  Interval(0, 30)`, as in the engine), has `months`, `days`, `micros` and `to_iso()`, and is
  built from the fields or from ISO 8601 text (`Interval("P1Y2M3DT4H")`).
- Python's `datetime` holds the years 1 to 9999; a date before year 1 cannot be returned as one
  (read it with `as_string()`).

The binding's README has the details and more examples.
