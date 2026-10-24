//! `at_zone()`: places a date-time in a time zone.

use crate::graph::GraphStorage;
use crate::traversal::step::map_steps::temporal::{
    ZoneArg, computed, describe, incoming_temporal, input_error, replaced, temporal_error,
};
use crate::value::temporal::Temporal;
use crate::*;

/// `at_zone(zone)` (a Graphersal extension; PostgreSQL `AT TIME ZONE`, `java.time`
/// `atZoneSameInstant`/`atZone`): a `datetime` keeps its instant and takes the zone's offset at
/// it; a `localdatetime` is read as a wall clock in the zone and becomes the `datetime` of that
/// instant (a time the zone skips moves forward by the gap, a repeated one takes the earlier
/// offset). `zone` is an IANA name (`"Europe/Berlin"`, kept in the result:
/// `+02:00[Europe/Berlin]`) or a UTC offset (`"+02:00"`, the offset only).
#[derive(Clone)]
struct StepAtZone {
    zone: ZoneArg,
}

impl fmt::Display for StepAtZone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "at_zone({})", self.zone.literal())
    }
}

fn step<'g, S: GraphStorage + 'g>(zone: &str) -> GraphStep<'g, S> {
    GraphStep::Map(
        "at_zone",
        Box::new(StepAtZone {
            zone: ZoneArg::parse(zone),
        }),
    )
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `at_zone(zone)`: a `datetime` at the same instant with the zone's offset, or a
    /// `localdatetime` read as a wall clock in the zone; an IANA zone's name is kept in the
    /// result, a fixed offset gives none. An unknown zone fails before execution. Book part
    /// "Dates and Times".
    pub fn at_zone(self, zone: &str) -> Self {
        self.push_step(step(zone))
    }
}

impl __ {
    /// Anonymous `at_zone(zone)`.
    pub fn at_zone<'g, S: GraphStorage + 'g>(zone: &str) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().at_zone(zone)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `at_zone(zone)`; see [`GraphTraversalSource::at_zone`].
    pub fn at_zone(self, zone: &str) -> Self {
        self.push_step(step(zone))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepAtZone {
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        const EXPECTED: &str = "a datetime or a localdatetime";
        let value = incoming_temporal(context, &source.value, "at_zone", EXPECTED)?;
        let zone = self.zone.get("at_zone")?;
        let result = match value {
            Temporal::DateTime(v) => zone.at_named(v),
            Temporal::LocalDateTime(v) => zone.resolve_named(v),
            other => {
                return Err(input_error(
                    "at_zone",
                    "incoming value",
                    EXPECTED,
                    describe(&ElementProperty::Temporal(other)),
                ));
            }
        }
        .map_err(|error| temporal_error("at_zone", error))?;
        Ok(replaced(source, computed(Temporal::DateTime(result))))
    }

    fn may_filter(&self) -> bool {
        true
    }

    fn plan_check(&self) -> TraverserResult<()> {
        self.zone.check("at_zone")
    }

    fn clone_box(&self) -> Box<dyn GraphStepMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}
