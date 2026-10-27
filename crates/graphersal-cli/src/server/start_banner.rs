//! The block `--server` prints on stderr at start, after the warning box: one section per topic
//! (`── Store ──`, `── Graph ──`, `── Embedding models ──`, `── Server ──`), each a list of
//! `key: value` rows whose values start in one column across all sections; a long value wraps
//! at word boundaries under its value column, an explicit `\n` starts a new line there. stdout
//! keeps only the address line.

/// The width the block is laid out for (the warning box is as wide).
pub(super) const WIDTH: usize = 78;

/// One section of the block: a heading and its rows (key, value, whether the value may wrap).
pub(super) struct Section {
    title: String,
    rows: Vec<(String, String, bool)>,
}

impl Section {
    /// An empty section titled `title`.
    pub(super) fn new(title: &str) -> Self {
        Self {
            title: title.to_string(),
            rows: Vec::new(),
        }
    }

    /// Adds the row `key: value`.
    pub(super) fn row(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.rows.push((key.into(), value.into(), true));
    }

    /// Adds the row `key: value`, never wrapped, so a command or a path copies in one piece.
    pub(super) fn unwrapped(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.rows.push((key.into(), value.into(), false));
    }

    /// Adds every row of `rows`.
    pub(super) fn rows(&mut self, rows: impl IntoIterator<Item = (String, String)>) {
        self.rows
            .extend(rows.into_iter().map(|(key, value)| (key, value, true)));
    }

    /// Whether the section has no row (it is left out).
    pub(super) fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// The text of `sections` (empty ones left out), sections separated by a blank line.
pub(super) fn render(sections: &[Section]) -> String {
    let sections: Vec<&Section> = sections.iter().filter(|s| !s.is_empty()).collect();
    // "  key:" padded to the longest key of the whole block, then one space.
    let key_width = sections
        .iter()
        .flat_map(|s| s.rows.iter())
        .map(|(key, _, _)| key.chars().count() + 1)
        .max()
        .unwrap_or(0);
    let indent = 2 + key_width + 1;
    let mut out = Vec::new();
    for section in sections {
        if !out.is_empty() {
            out.push(String::new());
        }
        out.push(heading(&section.title));
        for (key, value, wraps) in &section.rows {
            let width = if *wraps {
                WIDTH.saturating_sub(indent).max(20)
            } else {
                usize::MAX
            };
            let mut lines = wrap(value, width).into_iter();
            let first = lines.next().unwrap_or_default();
            out.push(format!("  {:key_width$} {first}", format!("{key}:")));
            out.extend(lines.map(|line| format!("{:indent$}{line}", "")));
        }
    }
    out.join("\n")
}

/// `── Title ───...` as wide as [`WIDTH`].
fn heading(title: &str) -> String {
    let used = 3 + title.chars().count() + 1;
    format!("── {title} {}", "─".repeat(WIDTH.saturating_sub(used)))
}

/// The size units [`wrap`] keeps on the line of their number (`1.2 KiB`).
const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];

/// The lines of `text` at most `width` characters wide, broken at every `\n` and between the
/// items of a comma-separated list (`person 4, software 2`); an item longer than `width` is
/// broken at spaces, keeping a size unit with its number (a word longer than `width` keeps a
/// line of its own).
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut pieces: Vec<String> = Vec::new();
        let items: Vec<&str> = paragraph.split(", ").collect();
        for (i, item) in items.iter().enumerate() {
            let item = if i + 1 < items.len() {
                format!("{item},")
            } else {
                (*item).to_string()
            };
            if item.chars().count() <= width {
                pieces.push(item);
            } else {
                pieces.extend(words(&item));
            }
        }
        let mut line = String::new();
        for piece in pieces.iter().filter(|piece| !piece.is_empty()) {
            let needed =
                line.chars().count() + usize::from(!line.is_empty()) + piece.chars().count();
            if !line.is_empty() && needed > width {
                lines.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(piece);
        }
        lines.push(line);
    }
    lines
}

/// The words of `text`, a size unit joined to the number before it.
fn words(text: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    for word in text.split(' ').filter(|word| !word.is_empty()) {
        let unit = word.trim_end_matches([',', ';', ')']);
        match words.last_mut() {
            Some(last) if UNITS.contains(&unit) => {
                last.push(' ');
                last.push_str(word);
            }
            _ => words.push(word.to_string()),
        }
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_start_in_one_column_and_sections_are_separated() {
        let mut store = Section::new("Store");
        store.row("path", "test_kb");
        let mut server = Section::new("Server");
        server.row("listening", "http://127.0.0.1:9010/");
        let empty = Section::new("Embedding models");
        let text = render(&[store, empty, server]);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0].chars().count(), WIDTH);
        assert!(lines[0].starts_with("── Store ───"));
        assert_eq!(lines[1], "  path:      test_kb");
        assert_eq!(lines[2], "");
        assert!(lines[3].starts_with("── Server ───"));
        assert_eq!(lines[4], "  listening: http://127.0.0.1:9010/");
        assert!(!text.contains("Embedding models"), "{text}");
    }

    #[test]
    fn long_values_wrap_under_their_column() {
        let mut graph = Section::new("Graph");
        let labels: Vec<String> = (0..20).map(|i| format!("Label{i} 1,000,")).collect();
        graph.row("vertices", labels.join(" "));
        graph.row("memory", "graph 1 KiB\nprocess heap 2 MiB");
        graph.unwrapped("connect", format!("claude mcp add {}", "x".repeat(90)));
        let sizes: Vec<String> = (0..15).map(|i| format!("part{i} 1.2 KiB,")).collect();
        graph.row("sizes", sizes.join(" "));
        let text = render(&[graph]);
        let lines: Vec<&str> = text.lines().collect();
        let connect = lines.iter().find(|l| l.starts_with("  connect:")).unwrap();
        assert!(connect.ends_with(&"x".repeat(90)), "{text}");
        assert!(
            lines
                .iter()
                .filter(|line| !line.starts_with("  connect:"))
                .all(|line| line.chars().count() <= WIDTH),
            "{text}"
        );
        assert!(
            lines
                .iter()
                .all(|line| !line.trim_start().starts_with("KiB")),
            "{text}"
        );
        assert!(lines[1].starts_with("  vertices: Label0 1,000,"));
        // A label stays with its count.
        assert!(lines[2].starts_with("            Label"), "{text}");
        assert!(
            lines
                .iter()
                .all(|line| !line.trim_start().starts_with("1,000")),
            "{text}"
        );
        let memory = lines
            .iter()
            .position(|l| l.starts_with("  memory:"))
            .unwrap();
        assert_eq!(lines[memory], "  memory:   graph 1 KiB");
        assert_eq!(lines[memory + 1], "            process heap 2 MiB");
    }
}
