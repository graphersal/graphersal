//! Dates and times: the six temporal value types a property or a traversal value can hold.
//!
//! For query writers and integrators that build or read temporal values in Rust: a
//! [`Temporal`] is stored as [`ElementProperty::Temporal`](crate::ElementProperty::Temporal) and
//! travels in a traversal as that property value
//! ([`TraverserValue::Computed`](crate::TraverserValue::Computed)). The guide is the book part
//! "Dates and Times".
//!
//! | Type | Holds | Canonical text |
//! |---|---|---|
//! | [`Date`] | a calendar day | `2023-08-02` |
//! | [`LocalDateTime`] | a wall-clock date and time, no zone | `2023-08-02T09:30:00` |
//! | [`DateTime`] | an instant plus the UTC offset it was written with, and its IANA zone name when written with one | `2023-08-02T09:30:00+02:00`, `2023-08-02T09:30:00+02:00[Europe/Berlin]` |
//! | [`Time`] | a time of day, no zone | `09:30:00` |
//! | [`OffsetTime`] | a time of day plus a UTC offset | `09:30:00+02:00` |
//! | [`Duration`] | months, days and nanoseconds (PostgreSQL `interval`) | `P1M2DT3H` |
//!
//! Every type with a time has a resolution of one nanosecond; the calendar types cover the years
//! -9999 to 9999 of the proleptic Gregorian calendar. A [`DateTime`] compares by its instant only
//! (`12:00:00Z` equals `14:00:00+02:00`, as TinkerPop and PostgreSQL compare dates; a zone name
//! is never compared either), an [`OffsetTime`] by its UTC time of day; a
//! [`Duration`] compares field by field (`P1M` is not `P30D`) and orders by its length with a
//! month counted as 30 days. A string is never turned into a temporal value implicitly: values
//! come from these types, a schema declaration or an explicit conversion.
//!
//! [`DT`] is the unit and part token of the date steps.
//!
//! ```rust
//! use graphersal::temporal::{DateTime, Temporal};
//!
//! let a: DateTime = "2023-08-02T12:00:00Z".parse().unwrap();
//! let b: DateTime = "2023-08-02T14:00:00+02:00".parse().unwrap();
//! assert_eq!(a, b);
//! assert_eq!(b.to_string(), "2023-08-02T14:00:00+02:00");
//! assert_eq!(Temporal::from(a).kind().name(), "datetime");
//! let berlin: DateTime = "2023-08-02T14:00[Europe/Berlin]".parse().unwrap();
//! assert_eq!(berlin.zone_name(), Some("Europe/Berlin"));
//! assert_eq!(berlin, a);
//! ```

pub use crate::value::dt::DT;
pub use crate::value::temporal::{
    Date, DateTime, Duration, LocalDateTime, OffsetTime, Temporal, TemporalError, TemporalKind,
    TemporalResult, Time,
};
