//! The generated blocks of the book page `book/src/gql/tck_compliance.md`.
//!
//! Copied from the TinkerPop harness's `page.rs` and adapted (W42: copy, never share). The
//! machine-derivable numbers of the page live between marker comments
//! (`<!-- opencypher:begin NAME -->` ... `<!-- opencypher:end NAME -->`, invisible in mdBook).
//! Their text is rendered by the report renderers, so a block shows exactly what
//! `target/opencypher-report.md` shows for the same run. A normal run compares the blocks with
//! the page and fails on drift; `OPENCYPHER_UPDATE_BASELINE=1` rewrites them in place.
//! Everything outside the markers is hand-written and never touched.

use std::path::{Path, PathBuf};

use super::report::{self, Row};

/// Path of the page, relative to the `graphersal` crate directory.
const PAGE_RELATIVE: &str = "../../book/src/gql/tck_compliance.md";

/// Absolute path of the page (via `CARGO_MANIFEST_DIR`, so the cwd does not matter).
pub fn page_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(PAGE_RELATIVE)
}

/// The opening marker line of a block.
pub fn begin_marker(name: &str) -> String {
    format!("<!-- opencypher:begin {name} -->")
}

/// The closing marker line of a block.
pub fn end_marker(name: &str) -> String {
    format!("<!-- opencypher:end {name} -->")
}

/// Renders every generated block of the page, in page order: `(name, markdown)`.
pub fn render_blocks(rows: &[Row<'_>]) -> Vec<(String, String)> {
    vec![
        ("scope".to_string(), report::scope_markdown(rows)),
        ("headline".to_string(), report::headline_markdown(rows)),
        (
            "directories".to_string(),
            report::directories_markdown(rows),
        ),
        ("excluded".to_string(), report::excluded_markdown(rows)),
    ]
}

/// Trailing whitespace of every line and trailing blank lines are the only differences ignored.
fn normalize(text: &str) -> String {
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    lines.join("\n").trim_end().to_string()
}

/// A block found on the page: its name and the byte range of its content (between the marker lines).
struct Found {
    name: String,
    start: usize,
    end: usize,
}

/// Finds every marker pair; a malformed page (unclosed, nested, mismatched, duplicated) is an error.
fn scan(page: &str) -> Result<Vec<Found>, String> {
    let mut found: Vec<Found> = Vec::new();
    let mut open: Option<(String, usize)> = None;
    let mut offset = 0;
    for line in page.split_inclusive('\n') {
        let trimmed = line.trim();
        let next = offset + line.len();
        if let Some(name) = trimmed
            .strip_prefix("<!-- opencypher:begin ")
            .and_then(|r| r.strip_suffix(" -->"))
        {
            if let Some((o, _)) = &open {
                return Err(format!("block `{name}` begins inside the open block `{o}`"));
            }
            open = Some((name.trim().to_string(), next));
        } else if let Some(name) = trimmed
            .strip_prefix("<!-- opencypher:end ")
            .and_then(|r| r.strip_suffix(" -->"))
        {
            let name = name.trim();
            match open.take() {
                Some((o, start)) if o == name => {
                    if found.iter().any(|f| f.name == o) {
                        return Err(format!("block `{o}` appears twice"));
                    }
                    found.push(Found {
                        name: o,
                        start,
                        end: offset,
                    });
                }
                Some((o, _)) => {
                    return Err(format!(
                        "`{}` closes `{name}` but `{o}` is open",
                        end_marker(name)
                    ));
                }
                None => return Err(format!("`{}` has no begin marker", end_marker(name))),
            }
        }
        offset = next;
    }
    match open {
        Some((o, _)) => Err(format!(
            "block `{o}` is never closed (`{}`)",
            end_marker(&o)
        )),
        None => Ok(found),
    }
}

/// A rendered block: its name and its generated content.
type Block = (String, String);

/// Locates every rendered block on the page and rejects page blocks nobody renders.
fn locate<'p>(page: &str, blocks: &'p [Block]) -> Result<Vec<(&'p Block, Found)>, String> {
    let mut found = scan(page)?;
    let mut located = Vec::new();
    for block in blocks {
        let Some(i) = found.iter().position(|f| f.name == block.0) else {
            return Err(format!(
                "the page has no block `{}`: add `{}` and `{}` where it belongs",
                block.0,
                begin_marker(&block.0),
                end_marker(&block.0)
            ));
        };
        located.push((block, found.remove(i)));
    }
    if let Some(extra) = found.first() {
        return Err(format!(
            "the page has a block `{}` that the harness does not render (stale marker?)",
            extra.name
        ));
    }
    Ok(located)
}

/// The differing lines of one block, as a short readable listing (first lines only).
fn line_diff(on_page: &str, expected: &str) -> String {
    let (a, b): (Vec<&str>, Vec<&str>) = (on_page.lines().collect(), expected.lines().collect());
    let mut out = String::new();
    let mut shown = 0;
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied(), b.get(i).copied());
        if x == y {
            continue;
        }
        if shown == 6 {
            out.push_str("    ...\n");
            break;
        }
        shown += 1;
        let _ = std::fmt::Write::write_fmt(
            &mut out,
            format_args!(
                "    line {}:\n      page:     {}\n      expected: {}\n",
                i + 1,
                x.unwrap_or("(missing)"),
                y.unwrap_or("(missing)")
            ),
        );
    }
    out
}

/// Compares the blocks with the page. `Ok(None)` means the page is current; `Ok(Some(message))`
/// describes the drift; `Err` means the markers are missing or malformed.
pub fn check(page: &str, blocks: &[(String, String)]) -> Result<Option<String>, String> {
    let mut drifted = String::new();
    for ((name, expected), f) in locate(page, blocks)? {
        let current = &page[f.start..f.end];
        if normalize(current) != normalize(expected) {
            drifted.push_str(&format!(
                "  block `{name}` differs:\n{}",
                line_diff(&normalize(current), &normalize(expected))
            ));
        }
    }
    Ok((!drifted.is_empty()).then_some(drifted))
}

/// Returns the page with every block's content replaced by the rendered text; only the text
/// between the marker lines changes.
pub fn rewrite(page: &str, blocks: &[(String, String)]) -> Result<String, String> {
    let mut located = locate(page, blocks)?;
    located.sort_by_key(|(_, f)| f.start);
    let mut out = String::with_capacity(page.len());
    let mut at = 0;
    for ((_, text), f) in located {
        out.push_str(&page[at..f.start]);
        out.push_str(text.trim_end());
        out.push('\n');
        at = f.end;
    }
    out.push_str(&page[at..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::classify::{Class, Outcome};

    fn blocks(v: &str) -> Vec<(String, String)> {
        vec![("a".to_string(), v.to_string())]
    }

    fn page(body: &str) -> String {
        format!(
            "prose 1\n{}\n{body}{}\nprose 2\n",
            begin_marker("a"),
            end_marker("a")
        )
    }

    #[test]
    fn current_page_has_no_drift_and_trailing_whitespace_is_ignored() {
        assert_eq!(check(&page("x 1\n"), &blocks("x 1\n")), Ok(None));
        assert_eq!(check(&page("x 1  \n\n\n"), &blocks("x 1")), Ok(None));
    }

    #[test]
    fn drift_is_reported_with_the_differing_line() {
        let msg = check(&page("x 1\ny 2\n"), &blocks("x 1\ny 3\n"));
        let msg = msg.unwrap().unwrap();
        assert!(msg.contains("block `a` differs"), "{msg}");
        assert!(
            msg.contains("page:     y 2") && msg.contains("expected: y 3"),
            "{msg}"
        );
        // Leading whitespace is not ignored.
        assert!(check(&page(" x\n"), &blocks("x\n")).unwrap().is_some());
    }

    #[test]
    fn rewrite_touches_only_the_block() {
        let old = page("x 1\n");
        let new = rewrite(&old, &blocks("x 2\nz\n")).unwrap();
        assert_eq!(new, page("x 2\nz\n"));
        assert_eq!(check(&new, &blocks("x 2\nz\n")), Ok(None));
        assert_eq!(
            rewrite(&new, &blocks("x 2\nz\n")).unwrap(),
            new,
            "idempotent"
        );
    }

    #[test]
    fn missing_or_malformed_markers_are_clear_errors() {
        let e = check("no markers\n", &blocks("x")).unwrap_err();
        assert!(
            e.contains("no block `a`") && e.contains("opencypher:begin a"),
            "{e}"
        );
        let unclosed = format!("{}\nx\n", begin_marker("a"));
        assert!(
            check(&unclosed, &blocks("x"))
                .unwrap_err()
                .contains("never closed")
        );
        let stray = format!("{}\n", end_marker("a"));
        assert!(
            check(&stray, &blocks("x"))
                .unwrap_err()
                .contains("no begin marker")
        );
        let extra = format!(
            "{}{}\n{}\n",
            page("x\n"),
            begin_marker("b"),
            end_marker("b")
        );
        assert!(
            check(&extra, &blocks("x"))
                .unwrap_err()
                .contains("does not render")
        );
        let twice = format!("{}{}", page("x\n"), page("x\n"));
        assert!(check(&twice, &blocks("x")).unwrap_err().contains("twice"));
        assert!(rewrite("none", &blocks("x")).is_err());
    }

    #[test]
    fn rendered_blocks_equal_the_report_text() {
        let pass = Outcome {
            class: Class::Pass,
            detail: String::new(),
        };
        let call = Outcome {
            class: Class::Excluded,
            detail: "excluded:upstream-ignore".to_string(),
        };
        let rows = [
            Row {
                id: "clauses/match/Match1.feature::[1] a",
                outcome: &pass,
            },
            Row {
                id: "clauses/call/Call1.feature::[1] b",
                outcome: &call,
            },
        ];
        let rendered = render_blocks(&rows);
        let report = report::markdown(&rows, None, "none");
        for (name, text) in &rendered {
            // The excluded table links its docs on the page and prints paths in the report.
            if name == "excluded" {
                assert!(
                    text.contains(
                        "[gql/tck_compliance.md#out-of-scope](../gql/tck_compliance.md#out-of-scope)"
                    ),
                    "{text}"
                );
                assert!(
                    report.contains("`book/src/gql/tck_compliance.md#out-of-scope`"),
                    "{report}"
                );
                continue;
            }
            let text = text.trim_end().trim_end_matches('.');
            assert!(
                report.contains(text),
                "block {name} not in the report:\n{text}"
            );
        }
        assert_eq!(rendered, render_blocks(&rows));
    }
}
