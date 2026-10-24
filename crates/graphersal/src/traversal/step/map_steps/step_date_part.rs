//! `date_part()`: one field of a date or time (PostgreSQL `extract` / `date_part`).

use crate::graph::GraphStorage;
use crate::traversal::step::map_steps::temporal::{
    ZoneArg, check_zone, incoming_temporal, replaced, temporal_error, zone_of,
};
use crate::value::dt::DT;
use crate::value::literal::TokenLiteral;
use crate::value::temporal::arith;
use crate::*;

/// `date_part(DT)`, `date_part(DT, zone)` (a Graphersal extension, PostgreSQL `extract`): the
/// field as an `int64` (`DT.year`, `DT.month`, `DT.day`, `DT.hour`, ..., `DT.day_of_week` 0 =
/// Sunday, `DT.iso_day_of_week` 1 = Monday, `DT.day_of_year`, `DT.week` (ISO), `DT.iso_year`,
/// `DT.quarter`, `DT.decade`, `DT.century`, `DT.millennium`, `DT.offset` in seconds), except
/// `DT.epoch`, a `float64` of seconds. `second`, `millisecond`, `microsecond` and `nanosecond` are
/// the plain fields (0-59, 0-999, 0-999999, 0-999999999), not PostgreSQL's seconds with a
/// fraction. A `datetime` is read on its own wall clock, or in `zone`; a `duration` gives
/// PostgreSQL's `interval` fields.
#[derive(Clone)]
struct StepDatePart {
    part: DT,
    zone: Option<ZoneArg>,
}

impl fmt::Display for StepDatePart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "date_part({}", self.part.token_literal())?;
        if let Some(zone) = &self.zone {
            write!(f, ", {}", zone.literal())?;
        }
        f.write_str(")")
    }
}

fn step<'g, S: GraphStorage + 'g>(part: DT, zone: Option<&str>) -> GraphStep<'g, S> {
    GraphStep::Map(
        "date_part",
        Box::new(StepDatePart {
            part,
            zone: zone.map(ZoneArg::parse),
        }),
    )
}

macro_rules! date_part_builders {
    () => {
        /// `date_part(DT)`: one field of a date or time (PostgreSQL `extract`), an `int64`
        /// (`DT::Epoch`: a `float64` of seconds). Book part "Dates and Times".
        pub fn date_part(self, part: DT) -> Self {
            self.push_step(step(part, None))
        }

        /// `date_part(DT, zone)`: the field of a `datetime` read on its wall clock in `zone`.
        pub fn date_part_in(self, part: DT, zone: &str) -> Self {
            self.push_step(step(part, Some(zone)))
        }
    };
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    date_part_builders!();
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    date_part_builders!();
}

impl __ {
    /// Anonymous `date_part(DT)`; see [`GraphTraversalSource::date_part`].
    pub fn date_part<'g, S: GraphStorage + 'g>(part: DT) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().date_part(part)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepDatePart {
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        let value = incoming_temporal(context, &source.value, "date_part", "a date or time")?;
        let zone = zone_of(&self.zone, "date_part")?;
        let result = arith::part(value, self.part, zone)
            .map_err(|error| temporal_error("date_part", error))?;
        Ok(replaced(
            source,
            TraverserValue::Computed(ElementProperty::Number(result), None),
        ))
    }

    fn may_filter(&self) -> bool {
        true
    }

    fn plan_check(&self) -> TraverserResult<()> {
        check_zone(&self.zone, "date_part")
    }

    fn clone_box(&self) -> Box<dyn GraphStepMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}
