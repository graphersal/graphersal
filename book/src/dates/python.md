# From Python

The Python binding converts the six types to and from the standard `datetime` module, both as
results and as `params=`. Python's types count microseconds, the engine nanoseconds: a value
with digits below the microsecond becomes a [pandas](https://pandas.pydata.org/) type, which
holds nanoseconds and subclasses the standard type, when the optional extra is installed
(`pip install graphersal[pandas]`); without pandas it raises `ImportError` naming the extra.
Nothing is ever rounded.

| Graphersal | Python | With digits below the microsecond |
|---|---|---|
| `datetime` | aware `datetime.datetime` with `tzinfo=datetime.timezone(timedelta(seconds=offset))` | aware `pandas.Timestamp`, the same `tzinfo` |
| `datetime` with a zone name | aware `datetime.datetime` with `tzinfo=zoneinfo.ZoneInfo(name)` | aware `pandas.Timestamp` in the `ZoneInfo` |
| `localdatetime` | naive `datetime.datetime` | naive `pandas.Timestamp` |
| `date` | `datetime.date` | - |
| `time` | naive `datetime.time` | `ValueError` |
| `offsettime` | `datetime.time` with `tzinfo=datetime.timezone(timedelta(seconds=offset))` | `ValueError` |
| `duration` without months | `datetime.timedelta` (an `Interval` beyond 999999999 days) | `pandas.Timedelta` (an `Interval` beyond an int64 of nanoseconds, about 292 years) |
| `duration` with months | `graphersal.Interval` | `graphersal.Interval` |

Why pandas: `pandas.Timestamp` and `pandas.Timedelta` are subclasses of `datetime.datetime` and
`datetime.timedelta`, so code written for the standard types keeps working; they hold
nanoseconds, fixed offsets and `ZoneInfo` zones, and pandas is the most widely installed library
that has them. [`whenever`](https://whenever.readthedocs.io/) holds nanoseconds too (also a time
of day) but its types are not the standard ones, and `numpy.datetime64` has neither offsets,
zones nor a time of day. Which type comes back depends on the value only, never on what is
installed. Limits:

- A time of day (`time`, `offsettime`) with digits below the microsecond raises `ValueError`:
  no common Python type holds one (pandas has no time of day; a `pandas.Timedelta` since
  midnight would come back as a duration). Truncate it with `date_trunc(DT.microsecond)` or read
  it with `as_string()`.
- A `pandas.Timestamp` holds nanoseconds only from 1677-09-21 to 2262-04-11: a `datetime` or
  `localdatetime` with digits below the microsecond outside these years raises.

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

- An aware `datetime` is passed with the UTC offset it has at that instant; with a
  `zoneinfo.ZoneInfo` its zone name is kept too (a name the bundled database does not know is an
  error), and it comes back as a `ZoneInfo` datetime (with the right `fold` in a repeated hour).
  Any other `tzinfo` gives its offset only. An offset with a fraction of a second is refused. A
  Python installation without the zone (no `tzdata` on some systems) raises when such a value is
  returned, rather than dropping the name.
- An aware `datetime.time` with a fixed offset (`datetime.timezone`) is an `offsettime`; one whose
  `tzinfo` has no fixed offset (a `ZoneInfo`) is refused: a zone's offset needs a date.
- A `timedelta` becomes whole days and the rest, both with the sign of the whole
  (`timedelta(hours=-1)` is `-PT1H`).
- `graphersal.Interval` is immutable, equal when its three fields are equal (`Interval("P1M") !=
  Interval(0, 30)`, as in the engine), has `months`, `days`, `micros` and `nanos` (Python `int`s,
  exact over the whole range of a duration; `micros` raises `ValueError` for a time part with
  digits below the microsecond) and `to_iso()`, and is built from the fields or from ISO 8601
  text with up to nine fraction digits (`Interval("P1Y2M3DT4.000000001H")`).
- A `pandas.Timestamp` or `pandas.Timedelta` parameter keeps its nanoseconds; a `pandas.Timestamp`
  in a `pytz` zone keeps the zone name as a `ZoneInfo` one does. `pandas.NaT` is refused (pass
  `None`).
- Python's `datetime` holds the years 1 to 9999; a date before year 1 cannot be returned as one
  (read it with `as_string()`).

The binding's README has the details and more examples.
