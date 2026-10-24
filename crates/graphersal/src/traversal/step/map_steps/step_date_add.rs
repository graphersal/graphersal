//! `date_add()` (TinkerPop `dateAdd()`): adds units of time or a duration to a date or time.

use crate::graph::GraphStorage;
use crate::traversal::step::map_steps::temporal::{
    ZoneArg, check_zone, computed, incoming_temporal, replaced, temporal_error, zone_of,
};
use crate::value::dt::DT;
use crate::value::temporal::{Duration, Temporal, arith};
use crate::*;

/// What `date_add()` adds.
#[derive(Clone, Copy)]
enum Amount {
    /// `n` units (`date_add(DT.hour, 2)`; a negative `n` subtracts).
    Units(DT, i64),
    /// A duration: its months, then its days, then its time part (PostgreSQL).
    Duration(Duration),
}

/// `date_add(DT, n)`, `date_add(DT, n, zone)`, `date_add(duration)`, `date_add(duration, zone)`.
///
/// TinkerPop's `dateAdd(DT.second|minute|hour|day, n)` on a `datetime` keeps the offset and moves
/// the instant (a day is 24 hours at a fixed offset). Graphersal adds the other units of [`DT`]
/// (calendar units on the wall clock, clamping the day to the month end), the other date and
/// time types, durations, and a zone in which a `datetime`'s wall clock is shifted (a day stays
/// a calendar day across a daylight saving change; the result carries the zone's offset). The
/// arithmetic: `value/temporal/arith.rs`.
#[derive(Clone)]
struct StepDateAdd {
    amount: Amount,
    zone: Option<ZoneArg>,
}

impl fmt::Display for StepDateAdd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("date_add(")?;
        match self.amount {
            Amount::Units(unit, n) => write!(f, "{}, {n}", unit.token_literal())?,
            Amount::Duration(duration) => f.write_str(&crate::value::literal::rhai_temporal(
                &Temporal::Duration(duration),
            ))?,
        }
        if let Some(zone) = &self.zone {
            write!(f, ", {}", zone.literal())?;
        }
        f.write_str(")")
    }
}

use crate::value::literal::TokenLiteral;

fn step<'g, S: GraphStorage + 'g>(amount: Amount, zone: Option<&str>) -> GraphStep<'g, S> {
    GraphStep::Map(
        "date_add",
        Box::new(StepDateAdd {
            amount,
            zone: zone.map(ZoneArg::parse),
        }),
    )
}

macro_rules! date_add_builders {
    () => {
        /// `date_add(DT, n)` (TinkerPop `dateAdd()`): adds `n` units (a negative `n`
        /// subtracts). Units below a day move the instant of a `datetime` (its offset kept);
        /// `DT::Day`/`DT::Week` add calendar days and `DT::Month` and larger calendar months
        /// (the day clamped to the month end). Book part "Dates and Times".
        pub fn date_add(self, unit: DT, n: i64) -> Self {
            self.push_step(step(Amount::Units(unit, n), None))
        }

        /// `date_add(DT, n, zone)`: like [`date_add`](Self::date_add), on the wall clock of a
        /// `datetime` in `zone` (an IANA name such as `"Europe/Berlin"` or an offset such as
        /// `"+02:00"`): a day stays a calendar day across a daylight saving change, and the
        /// result carries the zone's offset. An unknown zone fails before execution.
        pub fn date_add_in(self, unit: DT, n: i64, zone: &str) -> Self {
            self.push_step(step(Amount::Units(unit, n), Some(zone)))
        }

        /// `date_add(duration)`: adds a duration: its months, then its days, then its
        /// time part (PostgreSQL `value + interval`).
        pub fn date_add_duration(self, duration: Duration) -> Self {
            self.push_step(step(Amount::Duration(duration), None))
        }

        /// `date_add(duration, zone)`: [`date_add_duration`](Self::date_add_duration) on the
        /// wall clock of a `datetime` in `zone`.
        pub fn date_add_duration_in(self, duration: Duration, zone: &str) -> Self {
            self.push_step(step(Amount::Duration(duration), Some(zone)))
        }
    };
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    date_add_builders!();
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    date_add_builders!();
}

impl __ {
    /// Anonymous `date_add(DT, n)`; see [`GraphTraversalSource::date_add`].
    pub fn date_add<'g, S: GraphStorage + 'g>(unit: DT, n: i64) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().date_add(unit, n)
    }

    /// Anonymous `date_add(DT, n, zone)`; see [`GraphTraversalSource::date_add_in`].
    pub fn date_add_in<'g, S: GraphStorage + 'g>(
        unit: DT,
        n: i64,
        zone: &str,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().date_add_in(unit, n, zone)
    }

    /// Anonymous `date_add(duration)`; see [`GraphTraversalSource::date_add_duration`].
    pub fn date_add_duration<'g, S: GraphStorage + 'g>(
        duration: Duration,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().date_add_duration(duration)
    }

    /// Anonymous `date_add(duration, zone)`; see
    /// [`GraphTraversalSource::date_add_duration_in`].
    pub fn date_add_duration_in<'g, S: GraphStorage + 'g>(
        duration: Duration,
        zone: &str,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().date_add_duration_in(duration, zone)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepDateAdd {
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        let value = incoming_temporal(context, &source.value, "date_add", "a date or time")?;
        let zone = zone_of(&self.zone, "date_add")?;
        let result = match self.amount {
            Amount::Units(unit, n) => arith::add(value, unit, n, zone),
            Amount::Duration(duration) => arith::add_duration(value, duration, zone),
        }
        .map_err(|error| temporal_error("date_add", error))?;
        Ok(replaced(source, computed(result)))
    }

    fn may_filter(&self) -> bool {
        true
    }

    fn plan_check(&self) -> TraverserResult<()> {
        check_zone(&self.zone, "date_add")
    }

    fn clone_box(&self) -> Box<dyn GraphStepMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}
