# now() and today()

`now()` is the current time as a `datetime` in UTC; `today()` is its date, and `today(zone)` the
date in a zone (an IANA name or an offset).

```rhai,graph=empty
type_of(now())   // "datetime"
now() == now()   // true
type_of(today("Europe/Berlin"))   // "date"
g.add_v("event").property("at", now()).values("at").count().next()   // 1
```

## The start of the evaluation

Like PostgreSQL's `now()` (`CURRENT_TIMESTAMP`), `now()` is the time the **script evaluation
started**, the same value for every call in it: a script that stamps a thousand elements stamps
them all with one instant, and `now() == now()` holds. In the playground a whole script is one
unit, so `now()` is the start of that unit. A [saved query](../users_guide/saved_queries.md) called
from a script sees its caller's `now()`.

`now()` and `today()` are script functions, evaluated before the traversal runs: the traversal
gets the resulting value as a constant (`inject(now())`, `property("at", now())`,
`has("at", P.lt(now()))`). The journal of a [Store](../persistence/store.md) and a change set
record that value, never the call, so a replay or a recovery writes the same instant, not the time
of the replay.

`today()` is the date of `now()` in UTC, PostgreSQL's `current_date` in a UTC session. For the
date in another zone pass the zone, or convert the instant yourself:

```rhai,graph=empty
today("UTC") == today()   // true
g.inject(now()).as_local("America/Los_Angeles").as_date(GType.DATE).next() == today("America/Los_Angeles")   // true
```

## Pinning the clock (hosts)

A host that needs a deterministic clock (tests, a replay of a recorded session) pins it on its
engine with `graphersal::script::pin_now`; every evaluation on that engine then returns the
pinned instant, until it is unpinned with `None`:

```rust,ignore
use graphersal::script::{engine_with_options, pin_now};
use graphersal::temporal::DateTime;

let mut engine = engine_with_options(&limits, authorizer, &presets);
pin_now(&mut engine, Some("2023-08-02T23:30:00-07:00".parse::<DateTime>()?));
// now() is 2023-08-02T23:30:00-07:00, today() is 2023-08-03 (the UTC date)
pin_now(&mut engine, None); // the system clock again
```

The system clock is read through `web_time`, so `now()` works the same in the WebAssembly
playground.

## Clock precision

`now()`, `today()`, the GQL clocks (`datetime()`, `date()`, `X.statement()`, ...) and the time a
schema's `defaultFunction: "now"` stores all come from the system clock, taken once (at the
start of the evaluation, or of the outermost open unit). The time is kept in nanoseconds, with
the digits the platform clock gives and never invented ones (the digits below are zero):

| Platform | Digits of the system clock |
|---|---|
| Linux | nanoseconds |
| macOS | microseconds (`CLOCK_REALTIME`) |
| Windows | 100 nanoseconds |
| WebAssembly (the browser playground, Node.js) | milliseconds (`Date.now()`) |

A pinned clock (`pin_now`, `GraphTraversalSource::with_now`) keeps every digit of the pinned
value. The binary store format of this release holds microseconds: where the clock has digits
below the microsecond (Linux, Windows), a `now()` stored in a [Store](../persistence/store.md)
is refused like any such value; store `g.inject(now()).date_trunc(DT.microsecond)` instead
([Storage and Exchange](storage.md)).
