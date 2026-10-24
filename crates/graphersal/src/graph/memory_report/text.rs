//! The memory report as plain text ([`MemoryReport`]'s `Display`): a summary for a terminal
//! (the REPL's `/memory`), the largest labels and properties first. Every figure of it is in
//! [`MemoryReport::to_property`] too; this form only picks and rounds.

use std::fmt;

use super::{MemoryReport, PropertyMemory};

/// How many label and property lines the text shows.
const TEXT_ROWS: usize = 20;

/// `bytes` in B, KiB, MiB or GiB with one decimal.
fn human(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// The value types of a row, most first: `string 98%, null 2%`.
fn types(row: &PropertyMemory) -> String {
    let total: u64 = row.value_types.iter().map(|t| t.count).sum();
    row.value_types
        .iter()
        .take(3)
        .map(|t| {
            if t.count == total {
                t.name.clone()
            } else {
                format!("{} {}%", t.name, t.count * 100 / total.max(1))
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

impl fmt::Display for MemoryReport {
    /// A terminal summary: totals, the 20 largest labels and properties, indexes, compression
    /// rules and the structure.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let elements: u64 = self.labels.iter().map(|l| l.elements).sum();
        write!(f, "Memory report at commit {}: ", self.commit_seq)?;
        match self.estimated_total_bytes() {
            Some(total) => write!(f, "{} estimated", human(total))?,
            None => write!(f, "no structure reported by the storage")?,
        }
        write!(f, ", properties {}", human(self.property_bytes()))?;
        if let Some(live) = self.tracked_live_bytes {
            write!(f, ", process heap {}", human(live))?;
        }
        writeln!(f)?;
        if self.sampled {
            writeln!(
                f,
                "Sampled: {} of about {} elements read (every {}th per label), figures \
                 extrapolated",
                self.visited_elements, elements, self.sample_every
            )?;
        }
        writeln!(f, "Took {:.1} ms", self.elapsed_micros as f64 / 1000.0)?;

        writeln!(
            f,
            "\nLabels ({}{}): elements, slots, properties",
            self.labels.len(),
            if self.labels_omitted > 0 {
                format!(" shown, {} more", self.labels_omitted)
            } else {
                String::new()
            }
        )?;
        let mut labels: Vec<_> = self.labels.iter().collect();
        labels.sort_by_key(|l| std::cmp::Reverse(l.property_bytes + l.slot_bytes.unwrap_or(0)));
        for label in labels.iter().take(TEXT_ROWS) {
            write!(
                f,
                "  {:<6} {:<24} {:>10}  {:>10}  {:>10}",
                label.element.as_str(),
                label.label.as_deref().unwrap_or("-"),
                label.elements,
                label.slot_bytes.map_or("-".to_string(), human),
                human(label.property_bytes)
            )?;
            if let Some(degree) = label.degree
                && degree.max > 0
            {
                write!(f, "  degree max {} p95 {}", degree.max, degree.p95)?;
            }
            writeln!(f)?;
        }
        if labels.len() > TEXT_ROWS {
            writeln!(f, "  ... {} more", labels.len() - TEXT_ROWS)?;
        }

        writeln!(
            f,
            "\nProperties ({}{}): elements/label, bytes, max value, types, distinct",
            self.properties.len(),
            if self.rows_omitted > 0 {
                format!(" shown, {} more", self.rows_omitted)
            } else {
                String::new()
            }
        )?;
        for row in self.properties.iter().take(TEXT_ROWS) {
            write!(
                f,
                "  {:<40} {:>8}/{:<8} {:>10}  max {:>9}  {}",
                row.target.to_string(),
                row.elements,
                row.label_elements,
                human(row.bytes()),
                human(row.max_value_bytes),
                types(row)
            )?;
            if let Some(strings) = &row.strings {
                write!(
                    f,
                    "  ~{}{} distinct",
                    strings.distinct_estimate,
                    if strings.distinct_reliable {
                        ""
                    } else {
                        " (sample)"
                    }
                )?;
            }
            if row.compressed_values + row.deduped_values > 0 {
                write!(
                    f,
                    "  compressed {} deduped {}",
                    row.compressed_values, row.deduped_values
                )?;
            }
            writeln!(f)?;
        }
        if self.properties.len() > TEXT_ROWS {
            writeln!(f, "  ... {} more", self.properties.len() - TEXT_ROWS)?;
        }

        if !self.indexes.is_empty() {
            writeln!(f, "\nProperty indexes: entries, bytes")?;
            for index in &self.indexes {
                writeln!(
                    f,
                    "  {:<24} {} {}.{}  {:>10}  {:>10}",
                    index.name,
                    index.element.as_str(),
                    index.label,
                    index.key,
                    index.stats.entries,
                    human(index.stats.bytes)
                )?;
            }
        }
        if !self.compression_rules.is_empty() {
            writeln!(
                f,
                "\nCompression rules: values, plain -> stored (+ dictionary)"
            )?;
            for rule in &self.compression_rules {
                write!(f, "  {:<24} {} {}", rule.name, rule.codec, rule.target)?;
                if let Some(stats) = &rule.stats {
                    write!(
                        f,
                        "  {}  {} -> {} (+ {})",
                        stats.compressed_values,
                        human(stats.plain_bytes),
                        human(stats.stored_bytes),
                        human(stats.dictionary_bytes)
                    )?;
                }
                writeln!(f)?;
            }
        }
        if let Some(structure) = &self.structure {
            let totals = &structure.totals;
            writeln!(f, "\nStructure")?;
            for (kind, slots) in [
                ("vertex", &structure.vertex_slots),
                ("edge", &structure.edge_slots),
            ] {
                writeln!(
                    f,
                    "  {kind} slots: {} live, {} tombstoned, {} spare, {} each = {}",
                    slots.live,
                    slots.tombstoned,
                    slots.spare,
                    human(slots.slot_bytes),
                    human(slots.bytes())
                )?;
            }
            writeln!(
                f,
                "  id index {}, label index {}, property indexes {}",
                human(structure.id_index_bytes),
                human(structure.label_index_bytes),
                human(totals.property_index_bytes)
            )?;
            writeln!(
                f,
                "  shapes {} ({} met by the walk), {}; dedup tables {}; dictionaries {}",
                structure.shape_table_shapes,
                self.shapes.shapes,
                human(totals.shape_bytes),
                human(totals.dedup_table_bytes),
                human(totals.compression_dictionary_bytes)
            )?;
            if let Some(unattributed) = self.unattributed_bytes() {
                writeln!(
                    f,
                    "  process heap minus estimate: {}{}",
                    if unattributed < 0 { "-" } else { "" },
                    human(unattributed.unsigned_abs())
                )?;
            }
        }
        Ok(())
    }
}
