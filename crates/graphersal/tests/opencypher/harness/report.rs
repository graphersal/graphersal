//! Reports of a TCK run: the short summary printed on stderr and the Markdown fix backlog
//! written to `target/opencypher-report.md` (design T11).
//!
//! Modelled on the TinkerPop harness's `report.rs` (W42: copied ideas, no shared code). Both are
//! pure functions of the run's outcomes (sorted by case id) and the baseline gate, so they are
//! deterministic for a deterministic run. The book page's generated blocks (`page.rs`) are
//! rendered by the `*_markdown` functions here, so the page and the report cannot disagree.
//!
//! Wording rule (licence Attribution Notice): Graphersal's GQL is *measured against the
//! openCypher TCK*; it is never called "Cypher" or "openCypher".

use std::collections::BTreeMap;
use std::fmt::Write as _;

use super::baseline::GateResult;
use super::classify::{self, Class, Outcome};
use super::scope;

/// Upstream TCK release the vendored features come from.
pub const TCK_VERSION: &str = "2024.3";

/// The sub-directory reported separately in the headline (it is about a quarter of the tree and
/// would hide every other movement, P0 G14).
pub const TEMPORAL_DIR: &str = "expressions/temporal";

/// One test case of a run.
#[derive(Debug, Clone, Copy)]
pub struct Row<'a> {
    /// Case id (`<path>::<raw name>[::<row>]`).
    pub id: &'a str,
    /// Its classified outcome.
    pub outcome: &'a Outcome,
}

impl Row<'_> {
    /// Feature file path (`clauses/match/Match1.feature`).
    fn file(&self) -> &str {
        self.id.split("::").next().unwrap_or(self.id)
    }

    /// Sub-directory (`clauses/match`): the first two path components.
    fn dir(&self) -> &str {
        let file = self.file();
        match file.match_indices('/').nth(1) {
            Some((at, _)) => &file[..at],
            None => file,
        }
    }

    fn class(&self) -> Class {
        self.outcome.class
    }
}

/// Classes of the `Unsupported` group: a construct is refused or not yet supported.
const UNSUPPORTED_CLASSES: [Class; 2] = [Class::Missing, Class::Setup];

/// Classes of the `Wrong behaviour` group: a supported construct differs from the TCK or fails.
const WRONG_CLASSES: [Class; 7] = [
    Class::WrongResult,
    Class::WrongColumns,
    Class::WrongSideEffects,
    Class::WrongError,
    Class::UnexpectedError,
    Class::Timeout,
    Class::Panic,
];

/// Per-class counts of a group of cases.
#[derive(Debug, Clone, Default)]
struct Tally {
    total: usize,
    by_class: BTreeMap<Class, usize>,
}

impl Tally {
    fn add(&mut self, class: Class) {
        self.total += 1;
        *self.by_class.entry(class).or_default() += 1;
    }

    fn get(&self, class: Class) -> usize {
        self.by_class.get(&class).copied().unwrap_or(0)
    }

    fn passed(&self) -> usize {
        self.get(Class::Pass)
    }

    fn unsupported(&self) -> usize {
        UNSUPPORTED_CLASSES.iter().map(|c| self.get(*c)).sum()
    }

    /// The number to drive to 0 (W43).
    fn wrong(&self) -> usize {
        WRONG_CLASSES.iter().map(|c| self.get(*c)).sum()
    }
}

/// Percentage `part / whole` (0 for an empty whole).
pub fn pct(part: usize, whole: usize) -> f64 {
    if whole == 0 {
        0.0
    } else {
        100.0 * part as f64 / whole as f64
    }
}

fn tally<'a>(rows: impl IntoIterator<Item = &'a Row<'a>>) -> Tally {
    let mut t = Tally::default();
    for row in rows {
        t.add(row.class());
    }
    t
}

fn tally_by<'a, F>(rows: &'a [Row<'a>], key: F) -> BTreeMap<&'a str, Tally>
where
    F: Fn(&'a Row<'a>) -> &'a str,
{
    let mut map: BTreeMap<&str, Tally> = BTreeMap::new();
    for row in rows {
        map.entry(key(row)).or_default().add(row.class());
    }
    map
}

/// Groups rows by a key, ranked by group size (then key).
fn ranked<'a, F>(rows: &[&'a Row<'a>], key: F) -> Vec<(String, Vec<&'a Row<'a>>)>
where
    F: Fn(&Row<'a>) -> String,
{
    let mut groups: BTreeMap<String, Vec<&Row<'a>>> = BTreeMap::new();
    for row in rows {
        groups.entry(key(row)).or_default().push(row);
    }
    let mut groups: Vec<(String, Vec<&Row<'a>>)> = groups.into_iter().collect();
    groups.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(&b.0)));
    groups
}

/// Splits the rows into in-scope and excluded ones.
pub fn split_scope<'a>(rows: &[Row<'a>]) -> (Vec<Row<'a>>, Vec<Row<'a>>) {
    rows.iter().partition(|r| r.class() != Class::Excluded)
}

/// Excluded cases per exclusion code, in [`scope::EXCLUSIONS`] order (codes with no case
/// omitted).
fn excluded_counts<'a>(excluded: &[Row<'a>]) -> Vec<(&'static scope::Exclusion, usize)> {
    scope::EXCLUSIONS
        .iter()
        .map(|e| {
            let n = excluded
                .iter()
                .filter(|r| scope::code_of_detail(&r.outcome.detail) == Some(e.code))
                .count();
            (e, n)
        })
        .filter(|(_, n)| *n > 0)
        .collect()
}

/// `3846 in-scope test cases; 133 excluded (upstream-ignore 1, ...)`.
fn scope_line(in_scope: usize, excluded: &[Row<'_>]) -> String {
    let parts: Vec<String> = excluded_counts(excluded)
        .iter()
        .map(|(e, n)| format!("{} {n}", e.code))
        .collect();
    format!(
        "{in_scope} in-scope test cases; {} excluded ({})",
        excluded.len(),
        parts.join(", ")
    )
}

/// A Markdown code span that survives backticks in the text.
fn code(text: &str) -> String {
    if text.contains('`') {
        format!("`` {text} ``")
    } else {
        format!("`{text}`")
    }
}

/// `missing 1372, setup 90` for the classes of one group.
fn group_classes(t: &Tally, classes: &[Class]) -> String {
    classes
        .iter()
        .map(|c| format!("{} {}", c.name(), t.get(*c)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The headline table and the class line (body of the report's "Headline" section and of the
/// book's `headline` block). `rows` are the in-scope rows.
fn headline_block(rows: &[Row<'_>]) -> String {
    let all = tally(rows);
    let total = all.total;
    let mut out = String::new();
    let _ = writeln!(out, "| | Test cases | Share |\n|---|---:|---:|");
    let _ = writeln!(
        out,
        "| **Passed** | **{}** / {total} | **{:.1} %** |",
        all.passed(),
        pct(all.passed(), total)
    );
    let _ = writeln!(
        out,
        "| Unsupported (not yet or refused) | {} / {total} | {:.1} % |",
        all.unsupported(),
        pct(all.unsupported(), total)
    );
    let _ = writeln!(
        out,
        "| **Wrong behaviour of supported constructs** | **{}** / {total} | **{:.1} %** |",
        all.wrong(),
        pct(all.wrong(), total)
    );
    let rest: Vec<&Row<'_>> = rows.iter().filter(|r| r.dir() != TEMPORAL_DIR).collect();
    let rest = tally(rest);
    let _ = writeln!(
        out,
        "| Passed without `{TEMPORAL_DIR}` | {} / {} | {:.1} % |\n",
        rest.passed(),
        rest.total,
        pct(rest.passed(), rest.total)
    );
    let _ = writeln!(
        out,
        "Unsupported = {}. Wrong behaviour = {}.",
        group_classes(&all, &UNSUPPORTED_CLASSES),
        group_classes(&all, &WRONG_CLASSES)
    );
    out
}

/// The per-sub-directory table of the in-scope rows.
fn directories_table(rows: &[Row<'_>]) -> String {
    let cells = |t: &Tally| {
        [t.unsupported(), t.wrong()]
            .iter()
            .map(|n| match n {
                0 => " · |".to_string(),
                n => format!(" {n} |"),
            })
            .collect::<String>()
    };
    let mut out = String::new();
    let _ = writeln!(
        out,
        "| Directory | Pass % | Passed | Total | Unsupported | Wrong |"
    );
    let _ = writeln!(out, "|---|---:|---:|---:|---:|---:|");
    for (name, t) in tally_by(rows, |r| r.dir()) {
        let _ = writeln!(
            out,
            "| {} | {:.1} % | {} | {} |{}",
            code(name),
            pct(t.passed(), t.total),
            t.passed(),
            t.total,
            cells(&t)
        );
    }
    let total = tally(rows);
    let _ = writeln!(
        out,
        "| **all** | **{:.1} %** | **{}** | **{}** |{}",
        pct(total.passed(), total.total),
        total.passed(),
        total.total,
        cells(&total)
    );
    out
}

/// The exclusion table: code, reason, docs, count. `link_prefix` turns the docs path (relative
/// to `book/src/`) into a link from the page (`../`); `None` prints the path as code.
fn excluded_table(excluded: &[Row<'_>], link_prefix: Option<&str>) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "| Code | Why | Docs | Test cases |\n|---|---|---|---:|"
    );
    let counts = excluded_counts(excluded);
    for (e, n) in &counts {
        let docs = match link_prefix {
            Some(prefix) => format!("[{}]({prefix}{})", e.docs, e.docs),
            None => code(&format!("book/src/{}", e.docs)),
        };
        let kind = if e.deviation {
            " (deliberate deviation)"
        } else {
            ""
        };
        let _ = writeln!(
            out,
            "| {} | {}{kind} | {docs} | {n} |",
            code(e.code),
            e.reason.replace('|', "\\|")
        );
    }
    let _ = writeln!(out, "| **all** | | | **{}** |", excluded.len());
    out
}

/// Markdown of the book block `scope`: the scope in one line.
pub fn scope_markdown(rows: &[Row<'_>]) -> String {
    let (in_scope, excluded) = split_scope(rows);
    format!("{}.\n", scope_line(in_scope.len(), &excluded))
}

/// Markdown of the book block `headline`.
pub fn headline_markdown(rows: &[Row<'_>]) -> String {
    let (in_scope, _) = split_scope(rows);
    headline_block(&in_scope)
}

/// Markdown of the book block `directories`.
pub fn directories_markdown(rows: &[Row<'_>]) -> String {
    let (in_scope, _) = split_scope(rows);
    directories_table(&in_scope)
}

/// Markdown of the book block `excluded` (links relative to a page in `book/src/gql/`).
pub fn excluded_markdown(rows: &[Row<'_>]) -> String {
    let (_, excluded) = split_scope(rows);
    excluded_table(&excluded, Some("../"))
}

/// The short summary printed on stderr after every run.
pub fn summary(rows: &[Row<'_>], report_path: &str) -> String {
    let (in_scope, excluded) = split_scope(rows);
    let all = tally(&in_scope);
    let total = all.total;
    let rule = "=".repeat(70);
    let mut out = String::new();
    let _ = writeln!(out, "{rule}");
    let _ = writeln!(
        out,
        "Graphersal GQL measured against the openCypher TCK {TCK_VERSION}"
    );
    let _ = writeln!(out, "{rule}");
    let _ = writeln!(
        out,
        "Passed:          {:>5} / {total}  ({:>5.2}%)",
        all.passed(),
        pct(all.passed(), total)
    );
    let _ = writeln!(
        out,
        "Unsupported:     {:>5} / {total}  ({:>5.2}%) [{}]",
        all.unsupported(),
        pct(all.unsupported(), total),
        group_classes(&all, &UNSUPPORTED_CLASSES)
    );
    let _ = writeln!(
        out,
        "Wrong behaviour: {:>5} / {total}  ({:>5.2}%) [{}]",
        all.wrong(),
        pct(all.wrong(), total),
        group_classes(&all, &WRONG_CLASSES)
    );
    let _ = writeln!(out, "Scope: {}", scope_line(total, &excluded));
    let _ = writeln!(out, "Full report (fix backlog): {report_path}");
    let _ = writeln!(out, "{rule}");
    out
}

fn id_list(out: &mut String, rows: &[&Row<'_>], with_detail: bool) {
    for row in rows {
        if with_detail {
            let _ = writeln!(out, "- {}: {}", code(row.id), code(&row.outcome.detail));
        } else {
            let _ = writeln!(out, "- {}", code(row.id));
        }
    }
}

/// A ranked table (`# | key | cases`) followed by every group's ids, collapsed.
fn ranked_section(out: &mut String, title: &str, groups: &[(String, Vec<&Row<'_>>)]) {
    let _ = writeln!(out, "| # | {title} | Test cases |\n|---:|---|---:|");
    for (i, (key, rs)) in groups.iter().enumerate() {
        let _ = writeln!(out, "| {} | {} | {} |", i + 1, code(key), rs.len());
    }
    out.push('\n');
    for (key, rs) in groups {
        let _ = writeln!(
            out,
            "<details><summary>{} — {}</summary>\n",
            code(key),
            rs.len()
        );
        id_list(out, rs, false);
        let _ = writeln!(out, "\n</details>\n");
    }
}

/// The full Markdown report.
///
/// `gate` is the comparison with the baseline as it was before this run (`None` when there was
/// no baseline); `baseline_note` says how the baseline was used (checked or regenerated).
pub fn markdown(rows: &[Row<'_>], gate: Option<&GateResult>, baseline_note: &str) -> String {
    let (in_scope, excluded) = split_scope(rows);
    let rows = &in_scope[..];
    let mut out = String::new();
    let _ = writeln!(out, "# openCypher TCK report\n");
    let _ = writeln!(
        out,
        "Graphersal's GQL measured against the openCypher TCK {TCK_VERSION} ({}). Generated by\n\
         `cargo test --package graphersal --features script,io,display,serde,persist,gql --test opencypher_tck`.\n\
         This file is the fix backlog: wrong behaviour of supported constructs is driven to 0,\n\
         the ranked missing constructs are the feature backlog.\n",
        scope_line(rows.len(), &excluded)
    );

    let urgent: Vec<&Row<'_>> = rows
        .iter()
        .filter(|r| matches!(r.class(), Class::Panic | Class::Timeout))
        .collect();
    if !urgent.is_empty() {
        let _ = writeln!(out, "## Panics and timeouts (top priority)\n");
        let _ = writeln!(
            out,
            "A panic violates CLAUDE.md rule 1 (no panics in step or script paths).\n"
        );
        for row in &urgent {
            let _ = writeln!(
                out,
                "- **{}** {}: {}",
                row.class().name(),
                code(row.id),
                code(&row.outcome.detail)
            );
        }
        out.push('\n');
    }

    let _ = writeln!(out, "## Headline\n");
    out.push_str(&headline_block(rows));
    let _ = writeln!(out, "\nBaseline: {baseline_note}\n");

    let _ = writeln!(out, "## By directory\n");
    out.push_str(&directories_table(rows));
    out.push('\n');

    let _ = writeln!(out, "## Excluded from the scope\n");
    let _ = writeln!(
        out,
        "Not run, in no total and no percentage, no baseline entry (`harness/scope.rs`).\n"
    );
    out.push_str(&excluded_table(&excluded, None));
    out.push('\n');
    for (e, n) in excluded_counts(&excluded) {
        let ids: Vec<&Row<'_>> = excluded
            .iter()
            .filter(|r| scope::code_of_detail(&r.outcome.detail) == Some(e.code))
            .collect();
        let _ = writeln!(out, "<details><summary>{} — {n}</summary>\n", code(e.code));
        id_list(&mut out, &ids, false);
        let _ = writeln!(out, "\n</details>\n");
    }

    let of_class =
        |class: Class| -> Vec<&Row<'_>> { rows.iter().filter(|r| r.class() == class).collect() };

    let wrong: Vec<&Row<'_>> = rows.iter().filter(|r| r.class().is_wrong()).collect();
    let _ = writeln!(
        out,
        "## Wrong behaviour of supported constructs — {} test cases\n",
        wrong.len()
    );
    let _ = writeln!(
        out,
        "The full list, by class. Each one is fixed in the engine, or excluded with a user \
         decision and a reason (`harness/scope.rs`, documented as a difference from Neo4j).\n"
    );
    for class in WRONG_CLASSES {
        let rs = of_class(class);
        if rs.is_empty() {
            continue;
        }
        let _ = writeln!(out, "### {} — {}\n", class.name(), rs.len());
        id_list(&mut out, &rs, true);
        out.push('\n');
    }

    let missing = of_class(Class::Missing);
    let _ = writeln!(
        out,
        "## Missing constructs — {} test cases\n",
        missing.len()
    );
    let _ = writeln!(
        out,
        "Refused (`refused:`) or not yet supported (`not yet:`) constructs, ranked by how many \
         test cases each blocks.\n"
    );
    ranked_section(
        &mut out,
        "Construct",
        &ranked(&missing, |r| classify::construct(&r.outcome.detail)),
    );

    let setup = of_class(Class::Setup);
    let _ = writeln!(out, "## Setup failures — {} test cases\n", setup.len());
    let _ = writeln!(
        out,
        "A named graph, `having executed` query or parameter value that does not run, by reason.\n"
    );
    ranked_section(
        &mut out,
        "Reason",
        &ranked(&setup, |r| {
            format!(
                "{}{}",
                classify::context_prefix(&r.outcome.detail),
                classify::construct(&r.outcome.detail)
            )
        }),
    );

    if let Some(g) = gate
        && !g.class_changed.is_empty()
    {
        let _ = writeln!(
            out,
            "## Baseline class changes — {}\n",
            g.class_changed.len()
        );
        let _ = writeln!(
            out,
            "Still failing, under another class than the baseline records (not a failure on \
             the default storage).\n"
        );
        for c in &g.class_changed {
            let _ = writeln!(
                out,
                "- {}: {} -> {}: {}",
                code(&c.id),
                c.recorded,
                c.current,
                code(&c.detail)
            );
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(class: Class, detail: &str) -> Outcome {
        Outcome {
            class,
            detail: detail.to_string(),
        }
    }

    #[test]
    fn rows_group_by_directory_and_scope() {
        let pass = outcome(Class::Pass, "");
        let missing = outcome(
            Class::Missing,
            "GQL construct not supported yet at line 1, column 1: MERGE is not yet supported",
        );
        let wrong = outcome(Class::WrongResult, "rows differ");
        let excluded = outcome(Class::Excluded, "excluded:upstream-ignore");
        let rows = [
            Row {
                id: "clauses/match/Match1.feature::[1] a",
                outcome: &pass,
            },
            Row {
                id: "clauses/merge/Merge1.feature::[1] b",
                outcome: &missing,
            },
            Row {
                id: "expressions/temporal/Temporal1.feature::[1] c::2",
                outcome: &wrong,
            },
            Row {
                id: "clauses/call/Call1.feature::[1] d",
                outcome: &excluded,
            },
        ];
        assert_eq!(rows[2].dir(), "expressions/temporal");
        assert_eq!(
            scope_markdown(&rows),
            "3 in-scope test cases; 1 excluded (upstream-ignore 1).\n"
        );
        let headline = headline_markdown(&rows);
        assert!(
            headline.contains("| **Passed** | **1** / 3 | **33.3 %** |"),
            "{headline}"
        );
        assert!(headline.contains("Passed without `expressions/temporal` | 1 / 2 |"));
        assert!(
            headline.contains("Wrong behaviour = wrong_result 1,"),
            "{headline}"
        );
        let dirs = directories_markdown(&rows);
        assert!(
            dirs.contains("| `clauses/merge` | 0.0 % | 0 | 1 | 1 | · |"),
            "{dirs}"
        );
        assert!(!dirs.contains("clauses/call"));
        let report = markdown(&rows, None, "none");
        assert!(report.contains("| 1 | `not yet: MERGE` | 1 |"), "{report}");
        assert!(report.contains("### wrong_result — 1"), "{report}");
        assert!(!report.to_lowercase().contains("cypher implementation"));
        let summary = summary(&rows, "x.md");
        assert!(summary.contains("Wrong behaviour:"), "{summary}");
    }
}
