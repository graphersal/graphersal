//! The workload report as a value ([`WorkloadReport::to_property`]): the one form every front
//! end shows (the session's and the MCP tool's JSON, the REPL, Python's dict), so that they all
//! carry the same keys. Keys are snake_case; figures are integers (microseconds for times).

use super::{Counters, JobRunRecord, SeriesKind, SeriesTotals, WorkloadReport};
use crate::value::{ElementProperty, PropertyMap};

/// An integer figure (saturated at `i64::MAX`).
fn int(value: u64) -> ElementProperty {
    ElementProperty::from(i64::try_from(value).unwrap_or(i64::MAX))
}

fn text(value: &str) -> ElementProperty {
    ElementProperty::from(value)
}

fn object(map: PropertyMap) -> ElementProperty {
    ElementProperty::Object(Box::new(map))
}

fn counters(c: &Counters) -> ElementProperty {
    let mut map = PropertyMap::default();
    map.insert("count", int(c.count));
    map.insert("failed", int(c.failed));
    map.insert("items", int(c.items));
    map.insert("total_micros", int(c.total_micros));
    map.insert("max_micros", int(c.max_micros));
    object(map)
}

fn windows(map: &mut PropertyMap, series: &SeriesTotals) {
    map.insert("channel", text(series.key.channel.name()));
    map.insert("last_hour", counters(&series.last_hour));
    map.insert("last_two_days", counters(&series.last_two_days));
    map.insert("last_ninety_days", counters(&series.last_ninety_days));
}

fn last_run(run: Option<&JobRunRecord>) -> ElementProperty {
    let Some(run) = run else {
        return ElementProperty::Null;
    };
    let mut map = PropertyMap::default();
    map.insert("started_at", ElementProperty::from(run.started_at));
    map.insert("micros", int(run.micros));
    map.insert(
        "error",
        run.error.as_deref().map_or(ElementProperty::Null, text),
    );
    object(map)
}

impl WorkloadReport {
    /// The report as a map: `level`; `queries` (`query` = the fingerprint), most total time in
    /// the last 90 days first; `properties` (`element`, `label`, `key`, `access`; `null` element
    /// or label = not stated by the plan); `locks` (`lock`); `jobs` (`name`, `last_run`); every
    /// series row with its `channel` and the windows `last_hour` / `last_two_days` /
    /// `last_ninety_days` (`count`, `failed`, `items`, `total_micros`, `max_micros`).
    pub fn to_property(&self) -> ElementProperty {
        let mut series: Vec<&SeriesTotals> = self.series.iter().collect();
        series.sort_by(|a, b| {
            b.last_ninety_days
                .total_micros
                .cmp(&a.last_ninety_days.total_micros)
                .then_with(|| a.key.cmp(&b.key))
        });
        let (mut queries, mut properties, mut locks, mut jobs) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for row in series {
            let mut map = PropertyMap::default();
            match &row.key.kind {
                SeriesKind::Query(query) => {
                    map.insert("query", text(query));
                    windows(&mut map, row);
                    queries.push(object(map));
                }
                SeriesKind::Property(property) => {
                    map.insert(
                        "element",
                        property
                            .element
                            .map_or(ElementProperty::Null, |e| text(e.as_str())),
                    );
                    map.insert(
                        "label",
                        property
                            .label
                            .as_deref()
                            .map_or(ElementProperty::Null, text),
                    );
                    map.insert("key", text(&property.key));
                    map.insert("access", text(property.access.name()));
                    windows(&mut map, row);
                    properties.push(object(map));
                }
                SeriesKind::Lock(lock) => {
                    map.insert("lock", text(lock));
                    windows(&mut map, row);
                    locks.push(object(map));
                }
                SeriesKind::Job(name) => {
                    map.insert("name", text(name));
                    windows(&mut map, row);
                    let run = self
                        .jobs
                        .iter()
                        .find(|(job, _)| job == name.as_str())
                        .map(|(_, run)| run);
                    map.insert("last_run", last_run(run));
                    jobs.push(object(map));
                }
            }
        }
        let mut map = PropertyMap::default();
        map.insert("level", text(self.level.name()));
        map.insert("queries", ElementProperty::Array(queries));
        map.insert("properties", ElementProperty::Array(properties));
        map.insert("locks", ElementProperty::Array(locks));
        map.insert("jobs", ElementProperty::Array(jobs));
        object(map)
    }

    /// [`Self::to_property`] as JSON text.
    pub fn to_json(&self) -> String {
        crate::value::physical::to_json_text(&self.to_property()).unwrap_or_default()
    }
}
