//! `date_trunc()`: truncates a date or time to a unit (PostgreSQL `date_trunc`).

use crate::graph::GraphStorage;
use crate::traversal::step::map_steps::temporal::{
    ZoneArg, check_zone, computed, incoming_temporal, replaced, temporal_error, zone_of,
};
use crate::value::dt::DT;
use crate::value::literal::TokenLiteral;
use crate::value::temporal::arith;
use crate::*;

/// `date_trunc(DT)`, `date_trunc(DT, zone)` (a Graphersal extension, PostgreSQL `date_trunc`):
/// the fields below the unit become their first value (`DT.week` starts on Monday; centuries and
/// millennia start at year 1). A `datetime` is truncated on its own wall clock, or in `zone` and
/// then resolved in it; a `duration` keeps the fields at and above the unit.
#[derive(Clone)]
struct StepDateTrunc {
    unit: DT,
    zone: Option<ZoneArg>,
}

impl fmt::Display for StepDateTrunc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "date_trunc({}", self.unit.token_literal())?;
        if let Some(zone) = &self.zone {
            write!(f, ", {}", zone.literal())?;
        }
        f.write_str(")")
    }
}

fn step<'g, S: GraphStorage + 'g>(unit: DT, zone: Option<&str>) -> GraphStep<'g, S> {
    GraphStep::Map(
        "date_trunc",
        Box::new(StepDateTrunc {
            unit,
            zone: zone.map(ZoneArg::parse),
        }),
    )
}

macro_rules! date_trunc_builders {
    () => {
        /// `date_trunc(DT)`: truncates a date or time to `unit` (PostgreSQL `date_trunc`;
        /// `DT::Week` starts on Monday). Book part "Dates and Times".
        pub fn date_trunc(self, unit: DT) -> Self {
            self.push_step(step(unit, None))
        }

        /// `date_trunc(DT, zone)`: truncates a `datetime` on its wall clock in `zone`; the
        /// result carries the zone's offset and, for an IANA zone, its name.
        pub fn date_trunc_in(self, unit: DT, zone: &str) -> Self {
            self.push_step(step(unit, Some(zone)))
        }
    };
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    date_trunc_builders!();
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    date_trunc_builders!();
}

impl __ {
    /// Anonymous `date_trunc(DT)`; see [`GraphTraversalSource::date_trunc`].
    pub fn date_trunc<'g, S: GraphStorage + 'g>(unit: DT) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().date_trunc(unit)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepDateTrunc {
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        let value = incoming_temporal(context, &source.value, "date_trunc", "a date or time")?;
        let zone = zone_of(&self.zone, "date_trunc")?;
        let result = arith::trunc(value, self.unit, zone)
            .map_err(|error| temporal_error("date_trunc", error))?;
        Ok(replaced(source, computed(result)))
    }

    fn may_filter(&self) -> bool {
        true
    }

    fn plan_check(&self) -> TraverserResult<()> {
        check_zone(&self.zone, "date_trunc")
    }

    fn clone_box(&self) -> Box<dyn GraphStepMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}
