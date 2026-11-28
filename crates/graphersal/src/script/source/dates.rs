//! Builder methods of the date steps (`as_date`, `as_duration`, `date_add`, `date_diff`, `age`,
//! `date_trunc`, `date_part`, `at_zone`, `as_local`; book part "Dates and Times").

use crate::script::*;
use crate::value::dt::DT;
use crate::value::temporal::Duration;

/// The operand of `date_diff()` / `age()`: a traversal becomes a child traversal, anything else a
/// constant (a date or time, or `null`; another value is rejected before execution).
fn operand(
    ctx: &NativeCallContext,
    step: &str,
    arg: Dynamic,
) -> Result<ScriptListArg, Box<rhai::EvalAltResult>> {
    if arg.is::<ScriptGraphTraversalSource>() {
        Ok(ScriptListArg::Traversal(try_dynamic_to_gts(arg)?))
    } else {
        Ok(ScriptListArg::Value(value_arg(ctx, step, arg)?))
    }
}

fn zone(text: &str) -> Option<CompactString> {
    Some(CompactString::new(text))
}

impl ScriptGraphTraversalSource {
    /// Converts a string (ISO 8601; no offset is UTC) or epoch milliseconds to a datetime
    /// (TinkerPop asDate()). Rhai binding of `as_date()`, the overload without arguments.
    pub fn as_date(&mut self) -> Self {
        self.add_step(Step::AsDate(None))
    }

    /// Converts to the date or time type of the GType. Rhai binding of `as_date()`, the overload
    /// with a target `GType`.
    pub fn as_date_t(&mut self, target: GType) -> Self {
        self.add_step(Step::AsDate(Some(target)))
    }

    /// Converts ISO 8601 duration text to a duration. Rhai binding of `as_duration()`.
    pub fn as_duration(&mut self) -> Self {
        self.add_step(Step::AsDuration)
    }

    /// Adds n units of time (TinkerPop dateAdd()). Rhai binding of `date_add(DT, n)`.
    pub fn date_add(&mut self, unit: DT, n: i64) -> Self {
        self.add_step(Step::DateAdd(unit, n, None))
    }

    /// Adds n units of time on the wall clock of a zone. Rhai binding of `date_add(DT, n, zone)`.
    pub fn date_add_in(&mut self, unit: DT, n: i64, zone_name: &str) -> Self {
        self.add_step(Step::DateAdd(unit, n, zone(zone_name)))
    }

    /// Adds a duration. Rhai binding of `date_add(duration)`.
    pub fn date_add_duration(&mut self, duration: Duration) -> Self {
        self.add_step(Step::DateAddDuration(duration, None))
    }

    /// Adds a duration on the wall clock of a zone. Rhai binding of `date_add(duration, zone)`.
    pub fn date_add_duration_in(&mut self, duration: Duration, zone_name: &str) -> Self {
        self.add_step(Step::DateAddDuration(duration, zone(zone_name)))
    }

    /// The difference in milliseconds (TinkerPop dateDiff()). Rhai binding of
    /// `date_diff(value | traversal)`.
    pub fn date_diff(
        ctx: NativeCallContext,
        this: &mut Self,
        other: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let other = operand(&ctx, "date_diff", other)?;
        Ok(this.add_step(Step::DateDiff(other, None, None)))
    }

    /// The difference in whole units. Rhai binding of `date_diff(value | traversal, DT)`.
    pub fn date_diff_unit(
        ctx: NativeCallContext,
        this: &mut Self,
        other: Dynamic,
        unit: DT,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let other = operand(&ctx, "date_diff", other)?;
        Ok(this.add_step(Step::DateDiff(other, Some(unit), None)))
    }

    /// The difference in whole units on the wall clock of a zone. Rhai binding of
    /// `date_diff(value | traversal, DT, zone)`.
    pub fn date_diff_unit_in(
        ctx: NativeCallContext,
        this: &mut Self,
        other: Dynamic,
        unit: DT,
        zone_name: &str,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let other = operand(&ctx, "date_diff", other)?;
        Ok(this.add_step(Step::DateDiff(other, Some(unit), zone(zone_name))))
    }

    /// The difference as a duration (PostgreSQL age). Rhai binding of `age(value | traversal)`.
    pub fn age(
        ctx: NativeCallContext,
        this: &mut Self,
        other: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let other = operand(&ctx, "age", other)?;
        Ok(this.add_step(Step::Age(other, None)))
    }

    /// The difference as a duration on the wall clock of a zone. Rhai binding of
    /// `age(value | traversal, zone)`.
    pub fn age_in(
        ctx: NativeCallContext,
        this: &mut Self,
        other: Dynamic,
        zone_name: &str,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let other = operand(&ctx, "age", other)?;
        Ok(this.add_step(Step::Age(other, zone(zone_name))))
    }

    /// Truncates to a unit (PostgreSQL date_trunc). Rhai binding of `date_trunc(DT)`.
    pub fn date_trunc(&mut self, unit: DT) -> Self {
        self.add_step(Step::DateTrunc(unit, None))
    }

    /// Truncates to a unit on the wall clock of a zone. Rhai binding of `date_trunc(DT, zone)`.
    pub fn date_trunc_in(&mut self, unit: DT, zone_name: &str) -> Self {
        self.add_step(Step::DateTrunc(unit, zone(zone_name)))
    }

    /// One field (PostgreSQL extract). Rhai binding of `date_part(DT)`.
    pub fn date_part(&mut self, part: DT) -> Self {
        self.add_step(Step::DatePart(part, None))
    }

    /// One field on the wall clock of a zone. Rhai binding of `date_part(DT, zone)`.
    pub fn date_part_in(&mut self, part: DT, zone_name: &str) -> Self {
        self.add_step(Step::DatePart(part, zone(zone_name)))
    }

    /// Places a datetime or a localdatetime in a zone. Rhai binding of `at_zone(zone)`.
    pub fn at_zone(&mut self, zone_name: &str) -> Self {
        self.add_step(Step::AtZone(CompactString::new(zone_name)))
    }

    /// The wall clock of a datetime. Rhai binding of `as_local()`.
    pub fn as_local(&mut self) -> Self {
        self.add_step(Step::AsLocal(None))
    }

    /// The wall clock of a datetime in a zone. Rhai binding of `as_local(zone)`.
    pub fn as_local_in(&mut self, zone_name: &str) -> Self {
        self.add_step(Step::AsLocal(zone(zone_name)))
    }
}
