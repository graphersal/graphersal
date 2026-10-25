//! The workload report as text (`Display`, the REPL's `/workload`): one block per kind of
//! series, one line per series with its last-90-days figures, most total time first.

use std::fmt;

use super::{SeriesKind, SeriesTotals, WorkloadReport};

/// A block of the text: its title and what a series of it shows (`None`: not in the block).
type Block = (&'static str, fn(&SeriesKind) -> Option<String>);

/// The figures of one series: `count failed items mean max channel what`.
fn line(f: &mut fmt::Formatter<'_>, row: &SeriesTotals, what: &str) -> fmt::Result {
    let c = &row.last_ninety_days;
    let mean = c.total_micros.checked_div(c.count).unwrap_or(0);
    writeln!(
        f,
        "  {:>8} {:>6} {:>10} {:>10} {:>10}  {:<10} {what}",
        c.count,
        c.failed,
        c.items,
        mean,
        c.max_micros,
        row.key.channel.name(),
    )
}

impl fmt::Display for WorkloadReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "Workload statistics (level {}, last 90 days)",
            self.level.name()
        )?;
        let mut series: Vec<&SeriesTotals> = self.series.iter().collect();
        series.sort_by(|a, b| {
            b.last_ninety_days
                .total_micros
                .cmp(&a.last_ninety_days.total_micros)
                .then_with(|| a.key.cmp(&b.key))
        });
        let blocks: [Block; 4] = [
            ("Queries", |kind| match kind {
                SeriesKind::Query(query) => Some(query.to_string()),
                _ => None,
            }),
            ("Properties", |kind| match kind {
                SeriesKind::Property(p) => Some(format!(
                    "{} {} {} {}",
                    p.element.map_or("*", |e| e.as_str()),
                    p.label.as_deref().unwrap_or("*"),
                    p.key,
                    p.access.name()
                )),
                _ => None,
            }),
            ("Locks", |kind| match kind {
                SeriesKind::Lock(lock) => Some(lock.to_string()),
                _ => None,
            }),
            ("Jobs", |kind| match kind {
                SeriesKind::Job(name) => Some(name.to_string()),
                _ => None,
            }),
        ];
        let mut empty = true;
        for (title, what) in blocks {
            let rows: Vec<(&SeriesTotals, String)> = series
                .iter()
                .filter_map(|row| what(&row.key.kind).map(|w| (*row, w)))
                .collect();
            if rows.is_empty() {
                continue;
            }
            empty = false;
            writeln!(f)?;
            writeln!(f, "{title}")?;
            writeln!(
                f,
                "  {:>8} {:>6} {:>10} {:>10} {:>10}  {:<10} what",
                "count", "failed", "items", "mean µs", "max µs", "channel"
            )?;
            for (row, what) in rows {
                line(f, row, &what)?;
            }
        }
        if empty {
            writeln!(f, "Nothing counted yet.")?;
        }
        Ok(())
    }
}
