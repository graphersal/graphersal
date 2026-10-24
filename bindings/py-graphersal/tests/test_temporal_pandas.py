"""Dates and times with digits below the microsecond (temporal v2 stage 4): a `datetime` or
`localdatetime` becomes a `pandas.Timestamp`, a `duration` without months a `pandas.Timedelta`
(both subclasses of the `datetime` types, holding nanoseconds) when the extra
`graphersal[pandas]` is installed, else an `ImportError` names the extra. The type depends on
the value only. Run the suite once with and once without pandas installed."""

import datetime as dt
import importlib.util
import zoneinfo

import pytest

import graphersal

HAS_PANDAS = importlib.util.find_spec("pandas") is not None
needs_pandas = pytest.mark.skipif(not HAS_PANDAS, reason="needs pandas (graphersal[pandas])")
without_pandas = pytest.mark.skipif(HAS_PANDAS, reason="pandas is installed")

#: Scripts whose value has digits below the microsecond and a pandas type.
PANDAS_KINDS = [
    'localdatetime("2024-01-01T00:00:00.123456789")',
    'datetime("2024-01-01T00:00:00.000000500Z")',
    'datetime("2024-10-27T02:30:00.000000001+01:00[Europe/Berlin]")',
    'duration("PT0.000000001S")',
]


@pytest.fixture
def graph():
    return graphersal.Graph()


def text(graph, value):
    """The engine's canonical text of a value sent as a parameter."""
    return graph.query("g.inject(x).as_string().next()", params={"x": value})


# ---------------------------------------------------------------------------------------------
# With or without pandas
# ---------------------------------------------------------------------------------------------


@without_pandas
@pytest.mark.parametrize("script", PANDAS_KINDS)
def test_without_pandas_the_error_names_the_extra(graph, script):
    with pytest.raises(ImportError, match=r"pip install graphersal\[pandas\]"):
        graph.query(script)


def test_a_time_of_day_with_nanoseconds_has_no_python_type(graph):
    for script in ['time("23:59:59.999999999")', 'offsettime("00:00:00.000000001-18:00")']:
        with pytest.raises(ValueError, match="no Python time type"):
            graph.query(script)


def test_an_interval_holds_nanoseconds(graph):
    # A duration with months, or longer than a pandas.Timedelta, is an Interval with or
    # without pandas.
    value = graph.query('duration("P1MT0.000000001S")')
    assert isinstance(value, graphersal.Interval)
    assert (value.months, value.days, value.nanos) == (1, 0, 1)
    with pytest.raises(ValueError, match="read nanos"):
        value.micros
    assert value == graphersal.Interval("P1MT0.000000001S")
    assert text(graph, value) == "P1MT0.000000001S"
    longest = graph.query('duration("PT9223372036854775807.999999999S")')
    assert isinstance(longest, graphersal.Interval)
    assert longest.nanos == 9223372036854775807 * 10**9 + 999999999
    assert text(graph, longest) == "PT2562047788015215H30M7.999999999S"
    # One nanosecond beyond an int64 of nanoseconds (about 292 years).
    beyond = graph.query('duration("PT9223372036.854775808S")')
    assert isinstance(beyond, graphersal.Interval)
    assert beyond.nanos == 9223372036854775808


def test_nat_is_refused_as_a_parameter(graph):
    pd = pytest.importorskip("pandas")
    with pytest.raises(graphersal.GraphersalError, match="NaT"):
        text(graph, pd.NaT)


# ---------------------------------------------------------------------------------------------
# With pandas
# ---------------------------------------------------------------------------------------------


@needs_pandas
def test_a_localdatetime_is_a_naive_timestamp(graph):
    import pandas as pd

    value = graph.query('localdatetime("2262-04-11T23:47:16.854775807")')
    assert isinstance(value, pd.Timestamp) and isinstance(value, dt.datetime)
    assert value.tzinfo is None
    assert value == pd.Timestamp("2262-04-11T23:47:16.854775807")
    assert value.nanosecond == 807
    assert text(graph, value) == "2262-04-11T23:47:16.854775807"


@needs_pandas
def test_a_datetime_is_an_aware_timestamp_with_its_offset_or_zone(graph):
    import pandas as pd

    value = graph.query('datetime("2024-01-01T10:00:00.000000500+05:45")')
    assert isinstance(value, pd.Timestamp)
    assert value.utcoffset() == dt.timedelta(hours=5, minutes=45)
    assert value.nanosecond == 500
    assert text(graph, value) == "2024-01-01T10:00:00.000000500+05:45"

    # The second 02:30 of the autumn change: the fold and the zone name are kept.
    berlin = zoneinfo.ZoneInfo("Europe/Berlin")
    zoned = graph.query('datetime("2024-10-27T02:30:00.000000001+01:00[Europe/Berlin]")')
    assert isinstance(zoned, pd.Timestamp)
    assert zoned.tzinfo == berlin
    assert zoned.fold == 1 and zoned.utcoffset() == dt.timedelta(hours=1)
    assert zoned.nanosecond == 1
    assert text(graph, zoned) == "2024-10-27T02:30:00.000000001+01:00[Europe/Berlin]"


@needs_pandas
def test_a_duration_without_months_is_a_timedelta_of_nanoseconds(graph):
    import pandas as pd

    for script, nanos in [
        ('duration("PT0.000000001S")', 1),
        ('duration("-PT1.000000001S")', -1_000_000_001),
        ('duration("P2DT0.000000001S")', 2 * 86_400 * 10**9 + 1),
        ('duration("-P2DT0.000000001S")', -(2 * 86_400 * 10**9 + 1)),
        ('duration("PT9223372036.854775807S")', 9223372036854775807),
    ]:
        value = graph.query(script)
        assert isinstance(value, pd.Timedelta) and isinstance(value, dt.timedelta), script
        assert value.value == nanos, script
        # A timedelta comes in as whole days and the rest (test_temporal.py), and back the same.
        again = graph.query("g.inject(x).next()", params={"x": value})
        assert isinstance(again, pd.Timedelta) and again.value == nanos, script


@needs_pandas
def test_pandas_parameters_keep_their_nanoseconds(graph):
    import pandas as pd

    assert text(graph, pd.Timestamp("2024-01-01T00:00:00.000000001")) == (
        "2024-01-01T00:00:00.000000001"
    )
    assert text(graph, pd.Timestamp("2024-01-01T00:00:00.000000001", tz="UTC")) == (
        "2024-01-01T00:00:00.000000001Z"
    )
    assert text(
        graph, pd.Timestamp("2024-07-01T10:00:00.123456789", tz=zoneinfo.ZoneInfo("Europe/Berlin"))
    ) == ("2024-07-01T10:00:00.123456789+02:00[Europe/Berlin]")
    assert text(graph, pd.Timedelta(-1, unit="ns")) == "-PT0.000000001S"
    assert text(graph, pd.Timedelta(days=-3, nanoseconds=7)) == "-P2DT23H59M59.999999993S"
    # Whole microseconds stay the standard library's types on the way back.
    value = graph.query("g.inject(x).next()", params={"x": pd.Timestamp("2024-01-01T00:00:01")})
    assert type(value) is dt.datetime


@needs_pandas
def test_a_pytz_zone_keeps_its_name(graph):
    pd = pytest.importorskip("pandas")
    pytz = pytest.importorskip("pytz")
    value = pd.Timestamp("2024-07-01T10:00:00.000000001").tz_localize(pytz.timezone("Europe/Berlin"))
    assert text(graph, value) == "2024-07-01T10:00:00.000000001+02:00[Europe/Berlin]"


@needs_pandas
def test_outside_the_nanosecond_years_of_pandas_the_value_raises(graph):
    with pytest.raises(graphersal.GraphersalError, match="1677-09-21 to 2262-04-11"):
        graph.query('localdatetime("1500-01-01T00:00:00.000000001")')
    with pytest.raises(graphersal.GraphersalError, match="1677-09-21 to 2262-04-11"):
        graph.query('datetime("9999-12-31T23:59:59.999999999Z")')
