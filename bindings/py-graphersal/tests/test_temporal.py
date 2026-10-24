"""Dates and times: the five engine types convert to and from the `datetime` module (and
`graphersal.Interval`), to the microsecond."""

import datetime as dt
import zoneinfo

import pytest

import graphersal


@pytest.fixture
def graph():
    return graphersal.Graph()


def back(graph, value):
    """A value sent as a parameter and returned unchanged."""
    return graph.query("g.inject(x).next()", params={"x": value})


def text(graph, value):
    """The engine's canonical text of a value sent as a parameter."""
    return graph.query("g.inject(x).as_string().next()", params={"x": value})


def test_results_are_datetime_values(graph):
    at = graph.query('datetime("2023-08-02T09:30:00.123456+02:00")')
    assert at == dt.datetime(
        2023, 8, 2, 9, 30, 0, 123456, tzinfo=dt.timezone(dt.timedelta(hours=2))
    )
    assert at.utcoffset() == dt.timedelta(hours=2)
    assert graph.query('datetime("2023-08-02T09:30Z")').tzinfo == dt.timezone.utc
    local = graph.query('localdatetime("2023-08-02T09:30")')
    assert local == dt.datetime(2023, 8, 2, 9, 30) and local.tzinfo is None
    assert graph.query('date("2024-02-29")') == dt.date(2024, 2, 29)
    assert type(graph.query('date("2024-02-29")')) is dt.date
    time = graph.query('time("09:30:00.5")')
    assert time == dt.time(9, 30, 0, 500000) and time.tzinfo is None
    assert graph.query('duration("P1DT2H")') == dt.timedelta(days=1, hours=2)
    assert graph.query('duration("PT-1H")') == dt.timedelta(hours=-1)


def test_a_duration_with_months_is_an_interval(graph):
    interval = graph.query('duration("P1Y2M3DT4H")')
    assert isinstance(interval, graphersal.Interval)
    assert (interval.months, interval.days, interval.micros) == (14, 3, 4 * 3600 * 10**6)
    assert interval.to_iso() == "P1Y2M3DT4H"
    assert str(interval) == "P1Y2M3DT4H"
    assert repr(interval) == "Interval('P1Y2M3DT4H')"


def test_parameters_round_trip_to_the_microsecond(graph):
    values = [
        dt.datetime(2023, 8, 2, 9, 30, 0, 1, tzinfo=dt.timezone(dt.timedelta(hours=-7))),
        dt.datetime(1, 1, 1, tzinfo=dt.timezone.utc),
        dt.datetime(9999, 12, 31, 23, 59, 59, 999999, tzinfo=dt.timezone.utc),
        dt.datetime(2023, 8, 2, 9, 30, 0, 999999),
        dt.date(1970, 1, 1),
        dt.date(9999, 12, 31),
        dt.time(0, 0),
        dt.time(23, 59, 59, 999999),
        dt.timedelta(0),
        dt.timedelta(days=-3, microseconds=7),
        dt.timedelta(days=999_999_999),
        graphersal.Interval(1),
        graphersal.Interval(-14, 3, -5),
    ]
    for value in values:
        result = back(graph, value)
        assert result == value, value
        assert type(result) is type(value), value
        if isinstance(value, dt.datetime):
            assert result.utcoffset() == value.utcoffset(), value


def test_parameters_have_the_canonical_text(graph):
    berlin = dt.timezone(dt.timedelta(hours=2))
    assert text(graph, dt.datetime(2023, 8, 2, 9, 30, tzinfo=berlin)) == (
        "2023-08-02T09:30:00+02:00"
    )
    assert text(graph, dt.datetime(2023, 8, 2, 9, 30)) == "2023-08-02T09:30:00"
    assert text(graph, dt.date(2023, 8, 2)) == "2023-08-02"
    assert text(graph, dt.time(9, 30)) == "09:30:00"
    assert text(graph, dt.timedelta(hours=-1)) == "-PT1H"
    assert text(graph, dt.timedelta(days=1, hours=12)) == "P1DT12H"
    assert text(graph, graphersal.Interval("P1M")) == "P1M"
    assert graph.query("type_of(x)", params={"x": dt.datetime(2023, 1, 1)}) == "localdatetime"
    assert graph.query("type_of(x)", params={"x": dt.timedelta(1)}) == "duration"


def test_a_zoneinfo_datetime_keeps_its_offset_at_that_instant(graph):
    berlin = zoneinfo.ZoneInfo("Europe/Berlin")
    summer = back(graph, dt.datetime(2023, 8, 2, 9, 30, tzinfo=berlin))
    winter = back(graph, dt.datetime(2023, 1, 2, 9, 30, tzinfo=berlin))
    assert summer.utcoffset() == dt.timedelta(hours=2)
    assert winter.utcoffset() == dt.timedelta(hours=1)
    assert summer == dt.datetime(2023, 8, 2, 9, 30, tzinfo=berlin)


def test_an_aware_time_is_refused(graph):
    with pytest.raises(graphersal.GraphersalError, match="aware datetime.time"):
        back(graph, dt.time(9, 30, tzinfo=dt.timezone.utc))


def test_datetimes_compare_by_instant(graph):
    utc = dt.datetime(2023, 8, 2, 12, 0, tzinfo=dt.timezone.utc)
    plus2 = dt.datetime(2023, 8, 2, 14, 0, tzinfo=dt.timezone(dt.timedelta(hours=2)))
    assert graph.query("a == b", params={"a": utc, "b": plus2}) is True
    assert graph.query("g.inject(a, b).dedup().count().next()", params={"a": utc, "b": plus2}) == 1


def test_stored_properties_come_back_typed(graph):
    at = dt.datetime(2023, 8, 2, 9, 30, tzinfo=dt.timezone.utc)
    graph.execute(
        'g.add_v("event").property("at", at).property("on", on).property("took", took).iterate()',
        params={"at": at, "on": dt.date(2023, 8, 2), "took": graphersal.Interval("P1M")},
    )
    vertex = graph.query('g.V().has_label("event").next()')
    assert vertex["at"] == at
    assert vertex["on"] == dt.date(2023, 8, 2)
    assert vertex["took"] == graphersal.Interval(1)
    assert graph.query('g.V().has("at", P.gt(x)).count().next()', params={
        "x": dt.datetime(2023, 8, 2, 11, 0, tzinfo=dt.timezone(dt.timedelta(hours=2)))
    }) == 1
    by_map = graph.query('g.V().has_label("event").value_map("on").next()')
    assert by_map == {"on": [dt.date(2023, 8, 2)]}


def test_saved_query_arguments(graph):
    graph.define_query(
        {
            "name": "after",
            "params": {"t": {"schema": {"type": "string", "format": "date-time"}}},
            "body": 'fn after(t) { g.inject(t).date_add(DT.day, 1) }',
        }
    )
    at = dt.datetime(2023, 8, 2, 9, 30, tzinfo=dt.timezone(dt.timedelta(hours=2)))
    assert graph.call_query("after", {"t": at}) == [at + dt.timedelta(days=1)]


def test_interval_value_semantics():
    assert graphersal.Interval("P1M") == graphersal.Interval(1)
    assert graphersal.Interval("P1M") != graphersal.Interval(0, 30)
    assert hash(graphersal.Interval("P1Y")) == hash(graphersal.Interval(12))
    assert len({graphersal.Interval(1), graphersal.Interval("P1M")}) == 1
    interval = graphersal.Interval(1, 2, 3)
    with pytest.raises(AttributeError):
        interval.months = 5
    with pytest.raises(ValueError):
        graphersal.Interval("one month")
    with pytest.raises(ValueError):
        graphersal.Interval("P1M", 2)


def test_a_date_before_year_1_is_an_error_not_a_wrong_value(graph):
    with pytest.raises(graphersal.GraphersalError, match="years 1 to 9999"):
        graph.query('date("-0044-03-15")')
    assert graph.query('date("-0044-03-15").as_string()') == "-0044-03-15"


def test_readme_example():
    graph = graphersal.Graph()
    graph.execute(
        'g.add_v("event").property("at", at).property("took", took).iterate()',
        params={
            "at": dt.datetime(2023, 8, 2, 9, 30, tzinfo=dt.timezone(dt.timedelta(hours=2))),
            "took": dt.timedelta(minutes=90),
        },
    )
    assert graph.query('g.V().values("at").next()') == dt.datetime(
        2023, 8, 2, 9, 30, tzinfo=dt.timezone(dt.timedelta(seconds=7200))
    )
    assert (
        graph.query('g.V().values("at").date_add(DT.month, 1).as_string().next()')
        == "2023-09-02T09:30:00+02:00"
    )
    assert repr(graph.query('duration("P1Y2M")')) == "Interval('P1Y2M')"


def test_a_value_below_the_microsecond_is_refused_never_rounded(graph):
    for script in [
        'time("12:00:00.000000001")',
        'localdatetime("2024-01-01T00:00:00.123456789")',
        'datetime("2024-01-01T00:00:00.000000500Z")',
        'duration("PT0.000000001S")',
        'duration("P1MT0.000000001S")',
    ]:
        with pytest.raises(ValueError, match="below the microsecond"):
            graph.query(script)
    with pytest.raises(ValueError, match="below the microsecond"):
        graphersal.Interval("P1MT0.000000001S")
    # Whole microseconds written with nine digits convert as before.
    assert graph.query('time("12:00:00.000001000")') == dt.time(12, 0, 0, 1)
    # Truncated in the engine first, the value converts.
    assert graph.query(
        'g.inject(time("12:00:00.123456789")).date_trunc(DT.microsecond).next()'
    ) == dt.time(12, 0, 0, 123456)
