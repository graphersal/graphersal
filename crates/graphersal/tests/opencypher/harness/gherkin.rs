//! Hand-written parser for the Gherkin subset used by the vendored openCypher TCK.
//!
//! A `.feature` file becomes a typed [`Feature`]: one [`Scenario`] per `Scenario:` /
//! `Scenario Outline:` block, each expanded into its [`Case`]s (one for a plain scenario, one per
//! `Examples:` row for an outline). Every step is typed into a [`Step`]; a step text outside the
//! TCK vocabulary is a [`ParseError`] naming the file and line, never silently skipped.
//!
//! Decisions recorded here (copied from and adapted to the TinkerPop harness, not shared with it):
//! - Line endings: `\r\n` and `\n` are both accepted (nine vendored files use CRLF); a `\r` never
//!   reaches a step text, doc string or table cell. A leading byte-order mark is ignored.
//! - Comments: a line whose first non-blank character is `#` is a comment everywhere except
//!   inside a doc string (where it is content). `#` inside a scenario name is part of the name
//!   (`... #Example: <exampleName>`), as in Gherkin.
//! - Free text after `Feature:` (a description) is accepted until the first `Background:` or
//!   scenario; anywhere else an unknown line is an error.
//! - Doc strings (`"""`): the body loses as many leading whitespace characters as the opening
//!   delimiter is indented (Gherkin rule; tabs count as one character); lines are joined with
//!   `\n` without a trailing newline; trailing whitespace is kept.
//! - Table cells are trimmed and unescaped as Gherkin does: `\|` -> `|`, `\\` -> `\`, `\n` -> a
//!   newline; any other backslash sequence is kept verbatim (so `'\''` stays `'\''` for the value
//!   notation). A row of only `|` has zero cells (`test.doNothing() :: ()`).
//! - Scenario Outline: every `<name>` of an `Examples:` header is replaced by the row's
//!   (unescaped) cell in the scenario name, the step texts, the doc strings and every table cell.
//!   Steps are typed after the substitution; an unknown step still reports its own line.
//! - `Background:` steps are prepended to every case of the feature.
//! - Tags are kept verbatim with the leading `@`; a case's tags are the feature tags followed by
//!   the scenario's own and the `Examples:` block's, deduplicated in first-seen order.
//! - Ids: `<path>::<raw scenario name>` and, for an outline, `::<row>` (1-based over all of the
//!   outline's `Examples:` rows). The raw (unsubstituted) name keeps ids stable when example
//!   values change. A duplicate id is a parse error.
//! - The step keyword (`Given`/`When`/`Then`/`And`/`But`) is not checked against the phrase.
//! - Each case is validated for the runner: one graph step first, then setup (`having
//!   executed`, `parameters are`, `there exists a procedure`), exactly one `executing query`,
//!   its outcomes (result / error / side effects), then optional control-query pairs that hold
//!   a result step each.

use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};

/// One parsed `.feature` file.
#[derive(Debug, Clone, PartialEq)]
pub struct Feature {
    /// Path relative to the `features/` root, with `/` separators
    /// (e.g. `"clauses/match/Match1.feature"`).
    pub path: String,
    /// Text after `Feature:`.
    pub name: String,
    /// Tags on the lines preceding `Feature:`, including the leading `@`.
    pub tags: Vec<String>,
    /// `Background:` steps (already prepended to every case's `steps`).
    pub background: Vec<StepAt>,
    /// Scenarios in file order.
    pub scenarios: Vec<Scenario>,
}

/// One `Scenario:` or `Scenario Outline:` block.
#[derive(Debug, Clone, PartialEq)]
pub struct Scenario {
    /// The name as written (placeholders not substituted).
    pub raw_name: String,
    /// 1-based line of the `Scenario:` / `Scenario Outline:` line.
    pub line: usize,
    /// Whether this is a `Scenario Outline:`.
    pub outline: bool,
    /// The expanded test cases: one for a plain scenario, one per example row for an outline.
    pub cases: Vec<Case>,
}

/// One executable test case: a plain scenario or one row of an outline.
#[derive(Debug, Clone, PartialEq)]
pub struct Case {
    /// Stable id: `<path>::<raw name>` plus `::<row>` for an outline row.
    pub id: String,
    /// The scenario name with the placeholders substituted.
    pub name: String,
    /// 1-based line of the scenario.
    pub scenario_line: usize,
    /// The example row this case was expanded from, if any.
    pub example: Option<ExampleRow>,
    /// Effective tags (see the module doc).
    pub tags: Vec<String>,
    /// Background steps followed by the scenario's own, typed and validated.
    pub steps: Vec<StepAt>,
}

/// Where an outline case comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExampleRow {
    /// 1-based row number over all the outline's `Examples:` rows (the header not counted).
    pub row: usize,
    /// 1-based line of the row in the file.
    pub line: usize,
}

/// A step with its 1-based line.
#[derive(Debug, Clone, PartialEq)]
pub struct StepAt {
    /// 1-based line of the step keyword.
    pub line: usize,
    /// The typed step.
    pub step: Step,
}

/// One step of the TCK vocabulary found in the pinned tree (exact texts, keyword stripped).
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// `Given an empty graph`.
    EmptyGraph,
    /// `Given any graph`.
    AnyGraph,
    /// `Given the <name> graph`: a named graph under `graphs/<name>/<name>.cypher`.
    NamedGraph(String),
    /// `And having executed:` + doc string: a setup query (may repeat).
    HavingExecuted(String),
    /// `And parameters are:` + a headerless 2-column table `| name | value |` (raw value cells).
    Parameters(Vec<(String, String)>),
    /// `And there exists a procedure <signature>:` + table (procedure scenarios are out of scope).
    Procedure {
        /// The signature text, e.g. `test.my.proc(in :: INTEGER?) :: (out :: STRING?)`.
        signature: String,
        /// The procedure's rows (header = its input and output names; may have zero columns).
        table: Table,
    },
    /// `When executing query:` + doc string: the query under test.
    ExecutingQuery(String),
    /// `When executing control query:` + doc string: run after the main query on the same graph.
    ExecutingControlQuery(String),
    /// `Then the result should be ...:` + table.
    Result {
        /// Whether the row order matters.
        rows: RowOrder,
        /// Whether lists compare ignoring their element order.
        lists: ListOrder,
        /// Header = column names; cells in the TCK value notation.
        table: Table,
    },
    /// `Then the result should be empty`.
    ResultEmpty,
    /// `Then a(n) <type> should be raised at <phase>: <detail>`.
    Error {
        /// The error type.
        kind: ErrorType,
        /// When the error must arise.
        phase: ErrorPhase,
        /// The detail code as written (`*` = any detail); never compared with our messages.
        detail: String,
    },
    /// `And no side effects`.
    NoSideEffects,
    /// `And the side effects should be:` + a headerless 2-column table `| +nodes | 1 |`.
    SideEffects(Vec<(SideEffect, u64)>),
}

/// Row order of a result step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RowOrder {
    /// `, in order`.
    Ordered,
    /// `, in any order` or the bare `(ignoring element order for lists)` form.
    Unordered,
}

/// List comparison of a result step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ListOrder {
    /// Lists compare element by element.
    Strict,
    /// `(ignoring element order for lists)`.
    IgnoreElementOrder,
}

/// A table with a header row.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Table {
    /// Header cells.
    pub header: Vec<String>,
    /// Data rows, each as wide as the header.
    pub rows: Vec<Vec<String>>,
}

/// The TCK error types (`Then a <type> should be raised ...`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ErrorType {
    /// `SyntaxError`.
    SyntaxError,
    /// `SemanticError`.
    SemanticError,
    /// `ParameterMissing`.
    ParameterMissing,
    /// `ConstraintVerificationFailed`.
    ConstraintVerificationFailed,
    /// `ConstraintValidationFailed`.
    ConstraintValidationFailed,
    /// `EntityNotFound`.
    EntityNotFound,
    /// `PropertyNotFound`.
    PropertyNotFound,
    /// `LabelNotFound`.
    LabelNotFound,
    /// `TypeError`.
    TypeError,
    /// `ArgumentError`.
    ArgumentError,
    /// `ArithmeticError`.
    ArithmeticError,
    /// `ProcedureError`.
    ProcedureError,
}

impl ErrorType {
    /// Every error type with its TCK spelling.
    pub const ALL: [(ErrorType, &'static str); 12] = [
        (ErrorType::SyntaxError, "SyntaxError"),
        (ErrorType::SemanticError, "SemanticError"),
        (ErrorType::ParameterMissing, "ParameterMissing"),
        (
            ErrorType::ConstraintVerificationFailed,
            "ConstraintVerificationFailed",
        ),
        (
            ErrorType::ConstraintValidationFailed,
            "ConstraintValidationFailed",
        ),
        (ErrorType::EntityNotFound, "EntityNotFound"),
        (ErrorType::PropertyNotFound, "PropertyNotFound"),
        (ErrorType::LabelNotFound, "LabelNotFound"),
        (ErrorType::TypeError, "TypeError"),
        (ErrorType::ArgumentError, "ArgumentError"),
        (ErrorType::ArithmeticError, "ArithmeticError"),
        (ErrorType::ProcedureError, "ProcedureError"),
    ];

    /// The TCK spelling.
    pub fn as_str(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(t, _)| *t == self)
            .map_or("?", |(_, s)| s)
    }

    fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().find(|(_, n)| *n == s).map(|(t, _)| *t)
    }
}

/// When an expected error must arise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ErrorPhase {
    /// `compile time`: before the query runs.
    CompileTime,
    /// `runtime`: while it runs.
    Runtime,
    /// `any time`: either.
    AnyTime,
}

/// The keys of `the side effects should be:`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SideEffect {
    /// `+nodes`.
    AddedNodes,
    /// `-nodes`.
    RemovedNodes,
    /// `+relationships`.
    AddedRelationships,
    /// `-relationships`.
    RemovedRelationships,
    /// `+labels`.
    AddedLabels,
    /// `-labels`.
    RemovedLabels,
    /// `+properties`.
    AddedProperties,
    /// `-properties`.
    RemovedProperties,
}

impl SideEffect {
    /// Every key with its TCK spelling.
    pub const ALL: [(SideEffect, &'static str); 8] = [
        (SideEffect::AddedNodes, "+nodes"),
        (SideEffect::RemovedNodes, "-nodes"),
        (SideEffect::AddedRelationships, "+relationships"),
        (SideEffect::RemovedRelationships, "-relationships"),
        (SideEffect::AddedLabels, "+labels"),
        (SideEffect::RemovedLabels, "-labels"),
        (SideEffect::AddedProperties, "+properties"),
        (SideEffect::RemovedProperties, "-properties"),
    ];

    /// The TCK spelling.
    pub fn as_str(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(k, _)| *k == self)
            .map_or("?", |(_, s)| s)
    }

    fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().find(|(_, n)| *n == s).map(|(k, _)| *k)
    }
}

impl Case {
    /// The query under test (`When executing query:`); validation guarantees exactly one.
    pub fn query(&self) -> &str {
        self.steps
            .iter()
            .find_map(|s| match &s.step {
                Step::ExecutingQuery(q) => Some(q.as_str()),
                _ => None,
            })
            .unwrap_or("")
    }

    /// Whether the case carries the upstream `@ignore` tag.
    pub fn is_ignored(&self) -> bool {
        self.tags.iter().any(|t| t == "@ignore")
    }
}

/// A parse failure with its location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// Feature path (relative to `features/`) or the file system path for I/O errors.
    pub file: String,
    /// 1-based line number, `0` when the error is not tied to a line.
    pub line: usize,
    /// What went wrong.
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.file, self.line, self.message)
    }
}

impl std::error::Error for ParseError {}

/// Loads and parses every `*.feature` file below `features_root`, in sorted path order.
pub fn load_all(features_root: &Path) -> Result<Vec<Feature>, ParseError> {
    let mut files = Vec::new();
    collect_feature_files(features_root, &mut files)?;
    let mut rel_paths: Vec<(String, PathBuf)> = files
        .into_iter()
        .map(|p| {
            let rel = p
                .strip_prefix(features_root)
                .unwrap_or(&p)
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            (rel, p)
        })
        .collect();
    rel_paths.sort();
    rel_paths
        .into_iter()
        .map(|(rel, full)| {
            let text = std::fs::read_to_string(&full).map_err(|e| ParseError {
                file: rel.clone(),
                line: 0,
                message: format!("cannot read file: {e}"),
            })?;
            parse_feature(&rel, &text)
        })
        .collect()
}

fn collect_feature_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), ParseError> {
    let io_err = |e: std::io::Error| ParseError {
        file: dir.display().to_string(),
        line: 0,
        message: format!("cannot read directory: {e}"),
    };
    for entry in std::fs::read_dir(dir).map_err(io_err)? {
        let path = entry.map_err(io_err)?.path();
        if path.is_dir() {
            collect_feature_files(&path, out)?;
        } else if path.extension().is_some_and(|ext| ext == "feature") {
            out.push(path);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Raw (untyped) model: what the file says before placeholder substitution.

#[derive(Debug, Clone)]
struct RawRow {
    line: usize,
    cells: Vec<String>,
}

#[derive(Debug, Clone)]
struct RawStep {
    line: usize,
    /// The step text after the keyword, trimmed.
    text: String,
    doc: Option<String>,
    table: Option<Vec<RawRow>>,
}

struct RawScenario {
    name: String,
    line: usize,
    outline: bool,
    tags: Vec<String>,
    steps: Vec<RawStep>,
    /// One entry per `Examples:` block: its tags, header and rows.
    examples: Vec<(Vec<String>, RawRow, Vec<RawRow>)>,
}

struct Parser<'a> {
    path: &'a str,
    lines: Vec<&'a str>,
    /// Index of the next line to read (0-based).
    pos: usize,
}

impl<'a> Parser<'a> {
    fn err(&self, line: usize, message: impl Into<String>) -> ParseError {
        ParseError {
            file: self.path.to_string(),
            line,
            message: message.into(),
        }
    }

    fn skip_comments_and_blanks(&mut self) {
        while self.lines.get(self.pos).is_some_and(|l| {
            let t = l.trim();
            t.is_empty() || t.starts_with('#')
        }) {
            self.pos += 1;
        }
    }

    /// The trimmed next significant line (comments and blanks skipped, position moved past them).
    fn peek_significant(&mut self) -> Option<&'a str> {
        self.skip_comments_and_blanks();
        self.lines.get(self.pos).map(|l| l.trim())
    }

    /// Reads a doc string whose opening `"""` is the current line.
    fn doc_string(&mut self, step_line: usize) -> Result<String, ParseError> {
        let open = self.lines.get(self.pos).copied().unwrap_or("");
        let indent = open.chars().take_while(|c| c.is_whitespace()).count();
        if open.trim() != "\"\"\"" {
            return Err(self.err(
                self.pos + 1,
                "a doc string delimiter must be `\"\"\"` alone",
            ));
        }
        self.pos += 1;
        let mut body: Vec<String> = Vec::new();
        loop {
            let Some(line) = self.lines.get(self.pos) else {
                return Err(self.err(step_line, "unterminated `\"\"\"` doc string"));
            };
            self.pos += 1;
            if line.trim() == "\"\"\"" {
                break;
            }
            let mut rest = *line;
            for _ in 0..indent {
                let mut chars = rest.chars();
                match chars.next() {
                    Some(c) if c.is_whitespace() => rest = chars.as_str(),
                    _ => break,
                }
            }
            body.push(rest.to_string());
        }
        Ok(body.join("\n"))
    }

    /// Reads the consecutive `| ... |` rows starting at the current line (comment lines between
    /// rows are skipped). All rows must have the same width.
    fn table(&mut self) -> Result<Vec<RawRow>, ParseError> {
        let mut rows: Vec<RawRow> = Vec::new();
        loop {
            match self.peek_significant() {
                Some(t) if t.starts_with('|') => {
                    let line = self.pos + 1;
                    self.pos += 1;
                    let cells = split_row(t).map_err(|m| self.err(line, m))?;
                    if let Some(first) = rows.first()
                        && first.cells.len() != cells.len()
                    {
                        return Err(self.err(
                            line,
                            format!(
                                "table row has {} cells, the first row has {}",
                                cells.len(),
                                first.cells.len()
                            ),
                        ));
                    }
                    rows.push(RawRow { line, cells });
                }
                _ => return Ok(rows),
            }
        }
    }
}

/// Splits a trimmed `| a | b |` row into unescaped, trimmed cells.
fn split_row(t: &str) -> Result<Vec<String>, String> {
    let Some(mut rest) = t.strip_prefix('|') else {
        return Err("a table row must start with `|`".into());
    };
    let mut cells = Vec::new();
    loop {
        if rest.trim().is_empty() {
            return Ok(cells);
        }
        let mut cell = String::new();
        let mut chars = rest.char_indices();
        let mut end = None;
        while let Some((i, c)) = chars.next() {
            match c {
                '\\' => match chars.next() {
                    Some((_, '|')) => cell.push('|'),
                    Some((_, '\\')) => cell.push('\\'),
                    Some((_, 'n')) => cell.push('\n'),
                    Some((_, other)) => {
                        cell.push('\\');
                        cell.push(other);
                    }
                    None => cell.push('\\'),
                },
                '|' => {
                    end = Some(i);
                    break;
                }
                other => cell.push(other),
            }
        }
        let Some(end) = end else {
            return Err("a table row must end with `|`".into());
        };
        cells.push(cell.trim().to_string());
        rest = &rest[end + 1..];
    }
}

/// Parses one `.feature` file. `path` is the file path relative to `features/`.
pub fn parse_feature(path: &str, text: &str) -> Result<Feature, ParseError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut p = Parser {
        path,
        lines: text.lines().collect(),
        pos: 0,
    };
    let mut pending_tags: Vec<String> = Vec::new();
    let mut feature: Option<(String, Vec<String>, usize)> = None;
    let mut background: Option<Vec<RawStep>> = None;
    let mut raw: Vec<RawScenario> = Vec::new();
    // Where steps go: `None` = no block open yet (feature description allowed), `Some(None)` =
    // background, `Some(Some(i))` = scenario `i`.
    let mut target: Option<Option<usize>> = None;

    while p.pos < p.lines.len() {
        let line_no = p.pos + 1;
        let t = p.lines[p.pos].trim();
        p.pos += 1;
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if t.starts_with('@') {
            for tag in t.split_whitespace() {
                if !tag.starts_with('@') || tag.len() == 1 {
                    return Err(p.err(line_no, format!("malformed tag `{tag}`")));
                }
                pending_tags.push(tag.to_string());
            }
            continue;
        }
        if let Some(name) = t.strip_prefix("Feature:") {
            if feature.is_some() {
                return Err(p.err(line_no, "second `Feature:` in one file"));
            }
            feature = Some((
                name.trim().to_string(),
                std::mem::take(&mut pending_tags),
                line_no,
            ));
            continue;
        }
        if feature.is_none() {
            return Err(p.err(line_no, format!("unexpected line before `Feature:`: `{t}`")));
        }
        let scenario_header = t
            .strip_prefix("Scenario Outline:")
            .map(|n| (n, true))
            .or_else(|| t.strip_prefix("Scenario:").map(|n| (n, false)));
        if let Some((name, outline)) = scenario_header {
            raw.push(RawScenario {
                name: name.trim().to_string(),
                line: line_no,
                outline,
                tags: std::mem::take(&mut pending_tags),
                steps: Vec::new(),
                examples: Vec::new(),
            });
            target = Some(Some(raw.len() - 1));
            continue;
        }
        if t.starts_with("Background:") {
            if background.is_some() {
                return Err(p.err(line_no, "second `Background:` in one file"));
            }
            if !raw.is_empty() {
                return Err(p.err(line_no, "`Background:` after a scenario"));
            }
            if !pending_tags.is_empty() {
                return Err(p.err(line_no, "tags on `Background:`"));
            }
            background = Some(Vec::new());
            target = Some(None);
            continue;
        }
        if t.starts_with("Examples:") {
            let Some(Some(i)) = target else {
                return Err(p.err(line_no, "`Examples:` outside a scenario"));
            };
            if !raw[i].outline {
                return Err(p.err(line_no, "`Examples:` in a plain `Scenario:`"));
            }
            let tags = std::mem::take(&mut pending_tags);
            let mut rows = p.table()?;
            if rows.is_empty() {
                return Err(p.err(line_no, "`Examples:` without a table"));
            }
            let header = rows.remove(0);
            raw[i].examples.push((tags, header, rows));
            continue;
        }
        if !pending_tags.is_empty() {
            return Err(p.err(
                line_no,
                "tags must precede `Feature:`, a scenario or `Examples:`",
            ));
        }
        let keyword_text = ["Given ", "When ", "Then ", "And ", "But "]
            .iter()
            .find_map(|kw| t.strip_prefix(kw));
        let Some(step_text) = keyword_text else {
            if target.is_none() {
                // Free-text feature description.
                continue;
            }
            return Err(p.err(line_no, format!("expected a step keyword: `{t}`")));
        };
        let Some(dest) = target else {
            return Err(p.err(line_no, "step outside a scenario or background"));
        };
        if let Some(Some(i)) = target
            && !raw[i].examples.is_empty()
        {
            return Err(p.err(line_no, "step after `Examples:`"));
        }
        let mut step = RawStep {
            line: line_no,
            text: step_text.trim().to_string(),
            doc: None,
            table: None,
        };
        match p.peek_significant() {
            Some("\"\"\"") => step.doc = Some(p.doc_string(line_no)?),
            Some(next) if next.starts_with('|') => step.table = Some(p.table()?),
            _ => {}
        }
        match dest {
            None => background.get_or_insert_with(Vec::new).push(step),
            Some(i) => raw[i].steps.push(step),
        }
    }

    let Some((name, tags, _)) = feature else {
        return Err(p.err(0, "no `Feature:` line"));
    };
    if !pending_tags.is_empty() {
        return Err(p.err(p.lines.len(), "dangling tags at end of file"));
    }

    let no_subst: (&[String], &[String]) = (&[], &[]);
    let background_steps: Vec<StepAt> = background
        .unwrap_or_default()
        .iter()
        .map(|s| type_step(path, s, no_subst))
        .collect::<Result<_, _>>()?;

    let mut ids: HashSet<String> = HashSet::new();
    let mut scenarios = Vec::with_capacity(raw.len());
    for r in raw {
        let base_tags = merge_tags(&tags, &r.tags);
        let base_id = format!("{path}::{}", r.name);
        let mut cases = Vec::new();
        if r.outline {
            if r.examples.is_empty() {
                return Err(p.err(
                    r.line,
                    format!("scenario outline `{}` has no `Examples:`", r.name),
                ));
            }
            let mut row_no = 0;
            for (ex_tags, header, rows) in &r.examples {
                let case_tags = merge_tags(&base_tags, ex_tags);
                for row in rows {
                    row_no += 1;
                    let subst = (header.cells.as_slice(), row.cells.as_slice());
                    let case = build_case(
                        path,
                        &r,
                        format!("{base_id}::{row_no}"),
                        substitute(&r.name, subst),
                        Some(ExampleRow {
                            row: row_no,
                            line: row.line,
                        }),
                        case_tags.clone(),
                        &background_steps,
                        subst,
                    )?;
                    cases.push(case);
                }
            }
            if cases.is_empty() {
                return Err(p.err(
                    r.line,
                    format!("scenario outline `{}` has no example rows", r.name),
                ));
            }
        } else {
            let case = build_case(
                path,
                &r,
                base_id.clone(),
                r.name.clone(),
                None,
                base_tags,
                &background_steps,
                no_subst,
            )?;
            cases.push(case);
        }
        for c in &cases {
            if !ids.insert(c.id.clone()) {
                return Err(p.err(r.line, format!("duplicate case id `{}`", c.id)));
            }
        }
        scenarios.push(Scenario {
            raw_name: r.name,
            line: r.line,
            outline: r.outline,
            cases,
        });
    }

    Ok(Feature {
        path: path.to_string(),
        name,
        tags,
        background: background_steps,
        scenarios,
    })
}

fn merge_tags(base: &[String], extra: &[String]) -> Vec<String> {
    let mut out = base.to_vec();
    for t in extra {
        if !out.contains(t) {
            out.push(t.clone());
        }
    }
    out
}

/// Replaces every `<header>` by the row's cell.
fn substitute(s: &str, (header, row): (&[String], &[String])) -> String {
    let mut out = s.to_string();
    for (h, v) in header.iter().zip(row) {
        let placeholder = format!("<{h}>");
        if out.contains(&placeholder) {
            out = out.replace(&placeholder, v);
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn build_case(
    path: &str,
    r: &RawScenario,
    id: String,
    name: String,
    example: Option<ExampleRow>,
    tags: Vec<String>,
    background: &[StepAt],
    subst: (&[String], &[String]),
) -> Result<Case, ParseError> {
    let mut steps = background.to_vec();
    for s in &r.steps {
        steps.push(type_step(path, s, subst)?);
    }
    let case = Case {
        id,
        name,
        scenario_line: r.line,
        example,
        tags,
        steps,
    };
    validate(path, &case)?;
    Ok(case)
}

fn err_at(path: &str, line: usize, message: impl Into<String>) -> ParseError {
    ParseError {
        file: path.to_string(),
        line,
        message: message.into(),
    }
}

/// Types one raw step after placeholder substitution.
fn type_step(
    path: &str,
    raw: &RawStep,
    subst: (&[String], &[String]),
) -> Result<StepAt, ParseError> {
    let line = raw.line;
    let err = |m: String| err_at(path, line, m);
    let text = substitute(&raw.text, subst);
    let doc = raw.doc.as_deref().map(|d| substitute(d, subst));
    let table: Option<Vec<Vec<String>>> = raw.table.as_ref().map(|rows| {
        rows.iter()
            .map(|r| r.cells.iter().map(|c| substitute(c, subst)).collect())
            .collect()
    });

    #[derive(PartialEq)]
    enum Arg {
        None,
        Doc,
        Table,
    }
    let (step, arg) = match text.as_str() {
        "an empty graph" => (Step::EmptyGraph, Arg::None),
        "any graph" => (Step::AnyGraph, Arg::None),
        "having executed:" => (Step::HavingExecuted(String::new()), Arg::Doc),
        "parameters are:" => (Step::Parameters(Vec::new()), Arg::Table),
        "executing query:" => (Step::ExecutingQuery(String::new()), Arg::Doc),
        "executing control query:" => (Step::ExecutingControlQuery(String::new()), Arg::Doc),
        "the result should be, in any order:" => (
            result_step(RowOrder::Unordered, ListOrder::Strict),
            Arg::Table,
        ),
        "the result should be, in order:" => (
            result_step(RowOrder::Ordered, ListOrder::Strict),
            Arg::Table,
        ),
        "the result should be (ignoring element order for lists):" => (
            result_step(RowOrder::Unordered, ListOrder::IgnoreElementOrder),
            Arg::Table,
        ),
        "the result should be, in order (ignoring element order for lists):" => (
            result_step(RowOrder::Ordered, ListOrder::IgnoreElementOrder),
            Arg::Table,
        ),
        "the result should be empty" => (Step::ResultEmpty, Arg::None),
        "no side effects" => (Step::NoSideEffects, Arg::None),
        "the side effects should be:" => (Step::SideEffects(Vec::new()), Arg::Table),
        _ => {
            if let Some(name) = text
                .strip_prefix("the ")
                .and_then(|r| r.strip_suffix(" graph"))
                .filter(|n| {
                    !n.is_empty()
                        && n.chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                })
            {
                (Step::NamedGraph(name.to_string()), Arg::None)
            } else if let Some(sig) = text
                .strip_prefix("there exists a procedure ")
                .and_then(|r| r.strip_suffix(':'))
            {
                let signature = sig.trim().to_string();
                if signature.is_empty() {
                    return Err(err("empty procedure signature".into()));
                }
                (
                    Step::Procedure {
                        signature,
                        table: Table::default(),
                    },
                    Arg::Table,
                )
            } else if let Some(rest) = text
                .strip_prefix("a ")
                .or_else(|| text.strip_prefix("an "))
                .filter(|r| r.contains(" should be raised at "))
            {
                (parse_error_step(rest).map_err(err)?, Arg::None)
            } else {
                return Err(err(format!("unknown step text: `{text}`")));
            }
        }
    };

    let step = match (arg, doc, table) {
        (Arg::None, None, None) => step,
        (Arg::Doc, Some(d), None) => match step {
            Step::HavingExecuted(_) => Step::HavingExecuted(d),
            Step::ExecutingQuery(_) => Step::ExecutingQuery(d),
            Step::ExecutingControlQuery(_) => Step::ExecutingControlQuery(d),
            other => other,
        },
        (Arg::Table, None, Some(rows)) => fill_table(step, rows).map_err(err)?,
        (Arg::None, ..) => return Err(err(format!("step `{text}` takes no argument"))),
        (Arg::Doc, ..) => return Err(err(format!("step `{text}` needs a `\"\"\"` doc string"))),
        (Arg::Table, ..) => return Err(err(format!("step `{text}` needs a table"))),
    };
    Ok(StepAt { line, step })
}

fn result_step(rows: RowOrder, lists: ListOrder) -> Step {
    Step::Result {
        rows,
        lists,
        table: Table::default(),
    }
}

/// `<Type> should be raised at <phase>: <detail>` (the article already stripped).
fn parse_error_step(rest: &str) -> Result<Step, String> {
    let (ty, rest) = rest
        .split_once(" should be raised at ")
        .ok_or_else(|| format!("malformed error step `{rest}`"))?;
    let kind = ErrorType::parse(ty).ok_or_else(|| format!("unknown error type `{ty}`"))?;
    let (phase, detail) = rest
        .split_once(':')
        .ok_or_else(|| format!("error step without `: <detail>`: `{rest}`"))?;
    let phase = match phase {
        "compile time" => ErrorPhase::CompileTime,
        "runtime" => ErrorPhase::Runtime,
        "any time" => ErrorPhase::AnyTime,
        other => return Err(format!("unknown error phase `{other}`")),
    };
    let detail = detail.trim();
    if detail.is_empty() || detail.contains(char::is_whitespace) {
        return Err(format!("malformed error detail `{detail}`"));
    }
    Ok(Step::Error {
        kind,
        phase,
        detail: detail.to_string(),
    })
}

/// Puts the table rows into a step that takes a table.
fn fill_table(step: Step, mut rows: Vec<Vec<String>>) -> Result<Step, String> {
    let headed = |mut rows: Vec<Vec<String>>| -> Result<Table, String> {
        if rows.is_empty() {
            return Err("table without a header row".into());
        }
        let header = rows.remove(0);
        Ok(Table { header, rows })
    };
    let pairs = |rows: Vec<Vec<String>>| -> Result<Vec<(String, String)>, String> {
        rows.into_iter()
            .map(|r| match <[String; 2]>::try_from(r) {
                Ok([a, b]) => Ok((a, b)),
                Err(r) => Err(format!(
                    "expected a 2-column table row, got {} cells",
                    r.len()
                )),
            })
            .collect()
    };
    Ok(match step {
        Step::Parameters(_) => {
            let params = pairs(std::mem::take(&mut rows))?;
            let mut seen = HashSet::new();
            for (name, _) in &params {
                if name.is_empty() || !seen.insert(name.clone()) {
                    return Err(format!("empty or repeated parameter name `{name}`"));
                }
            }
            Step::Parameters(params)
        }
        Step::SideEffects(_) => {
            let mut out = Vec::new();
            for (key, value) in pairs(rows)? {
                let k = SideEffect::parse(&key)
                    .ok_or_else(|| format!("unknown side effect `{key}`"))?;
                let v: u64 = value
                    .parse()
                    .map_err(|_| format!("side effect `{key}` needs a count, got `{value}`"))?;
                if out.iter().any(|(seen, _)| *seen == k) {
                    return Err(format!("side effect `{key}` given twice"));
                }
                out.push((k, v));
            }
            Step::SideEffects(out)
        }
        Step::Result { rows: o, lists, .. } => Step::Result {
            rows: o,
            lists,
            table: headed(rows)?,
        },
        Step::Procedure { signature, .. } => Step::Procedure {
            signature,
            table: headed(rows)?,
        },
        other => other,
    })
}

/// Checks the step order the runner relies on (see the module doc).
fn validate(path: &str, case: &Case) -> Result<(), ParseError> {
    #[derive(Clone, Copy, PartialEq, PartialOrd)]
    enum Phase {
        Start,
        Setup,
        Outcome,
        Control,
    }
    let mut phase = Phase::Start;
    let mut queries = 0;
    let mut outcome_seen = false;
    let mut control_has_result = true;
    for s in &case.steps {
        let bad = |m: &str| err_at(path, s.line, format!("{m} (case `{}`)", case.id));
        match &s.step {
            Step::EmptyGraph | Step::AnyGraph | Step::NamedGraph(_) => {
                if phase != Phase::Start {
                    return Err(bad("the graph step must be the first step, once"));
                }
                phase = Phase::Setup;
            }
            Step::HavingExecuted(_) | Step::Parameters(_) | Step::Procedure { .. } => {
                if phase != Phase::Setup {
                    return Err(bad(
                        "a setup step must follow the graph step and precede the query",
                    ));
                }
            }
            Step::ExecutingQuery(_) => {
                if phase != Phase::Setup {
                    return Err(bad(
                        "`executing query` must follow the graph and setup steps",
                    ));
                }
                queries += 1;
                phase = Phase::Outcome;
            }
            Step::ExecutingControlQuery(_) => {
                if phase < Phase::Outcome || !outcome_seen || !control_has_result {
                    return Err(bad(
                        "a control query must follow the query's outcome and the previous control query's result",
                    ));
                }
                phase = Phase::Control;
                control_has_result = false;
            }
            Step::Result { .. } | Step::ResultEmpty => match phase {
                Phase::Outcome => outcome_seen = true,
                Phase::Control if !control_has_result => control_has_result = true,
                _ => return Err(bad("a result step must follow a query")),
            },
            Step::Error { .. } | Step::NoSideEffects | Step::SideEffects(_) => {
                if phase != Phase::Outcome {
                    return Err(bad(
                        "an error or side-effect step must follow the main query",
                    ));
                }
                outcome_seen = true;
            }
        }
    }
    let at = |m: &str| {
        err_at(
            path,
            case.scenario_line,
            format!("{m} (case `{}`)", case.id),
        )
    };
    if phase == Phase::Start {
        return Err(at("no graph step"));
    }
    if queries != 1 {
        return Err(at("expected exactly one `executing query` step"));
    }
    if !outcome_seen {
        return Err(at("the query has no outcome step"));
    }
    if !control_has_result {
        return Err(at("the control query has no result step"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Feature {
        match parse_feature("x/T.feature", text) {
            Ok(f) => f,
            Err(e) => panic!("parse failed: {e}"),
        }
    }

    fn parse_err(text: &str) -> ParseError {
        match parse_feature("x/T.feature", text) {
            Ok(f) => panic!("expected an error, parsed {f:?}"),
            Err(e) => e,
        }
    }

    /// Parses a one-scenario feature and returns its only case.
    fn one(body: &str) -> Case {
        let text = format!("Feature: F\n\n  Scenario: s\n{body}");
        let mut f = parse(&text);
        assert_eq!(f.scenarios.len(), 1);
        assert_eq!(f.scenarios[0].cases.len(), 1);
        f.scenarios.remove(0).cases.remove(0)
    }

    const Q: &str = "    Given any graph\n    When executing query:\n      \"\"\"\n      RETURN 1 AS x\n      \"\"\"\n";

    fn steps(c: &Case) -> Vec<Step> {
        c.steps.iter().map(|s| s.step.clone()).collect()
    }

    fn table(header: &[&str], rows: &[&[&str]]) -> Table {
        Table {
            header: header.iter().map(|s| s.to_string()).collect(),
            rows: rows
                .iter()
                .map(|r| r.iter().map(|s| s.to_string()).collect())
                .collect(),
        }
    }

    #[test]
    fn graph_steps() {
        for (text, step) in [
            ("an empty graph", Step::EmptyGraph),
            ("any graph", Step::AnyGraph),
            (
                "the binary-tree-1 graph",
                Step::NamedGraph("binary-tree-1".into()),
            ),
        ] {
            let c = one(&format!(
                "    Given {text}\n    When executing query:\n      \"\"\"\n      RETURN 1\n      \"\"\"\n    Then the result should be empty\n"
            ));
            assert_eq!(c.steps[0].step, step);
            assert_eq!(c.steps[0].line, 4);
        }
    }

    #[test]
    fn query_and_setup_doc_strings() {
        let c = one(
            "    Given an empty graph\n    And having executed:\n      \"\"\"\n      CREATE (:A)\n        CREATE (:B)\n      \"\"\"\n    And having executed:\n      \"\"\"\n      CREATE ()\n      \"\"\"\n    When executing query:\n      \"\"\"\n      MATCH (n)\n      RETURN n\n      \"\"\"\n    Then the result should be empty\n    And no side effects\n",
        );
        assert_eq!(
            steps(&c),
            vec![
                Step::EmptyGraph,
                Step::HavingExecuted("CREATE (:A)\n  CREATE (:B)".into()),
                Step::HavingExecuted("CREATE ()".into()),
                Step::ExecutingQuery("MATCH (n)\nRETURN n".into()),
                Step::ResultEmpty,
                Step::NoSideEffects,
            ]
        );
        assert_eq!(c.query(), "MATCH (n)\nRETURN n");
    }

    #[test]
    fn doc_string_with_tab_indent_and_comment_before_it() {
        let c = one(
            "    Given any graph\n    When executing query:\n    # a comment\n\t\"\"\"\n\tRETURN 1\n\t\"\"\"\n    Then the result should be empty\n",
        );
        assert_eq!(c.query(), "RETURN 1");
    }

    #[test]
    fn result_step_forms() {
        for (text, rows, lists) in [
            (
                "the result should be, in any order:",
                RowOrder::Unordered,
                ListOrder::Strict,
            ),
            (
                "the result should be, in order:",
                RowOrder::Ordered,
                ListOrder::Strict,
            ),
            (
                "the result should be (ignoring element order for lists):",
                RowOrder::Unordered,
                ListOrder::IgnoreElementOrder,
            ),
            (
                "the result should be, in order (ignoring element order for lists):",
                RowOrder::Ordered,
                ListOrder::IgnoreElementOrder,
            ),
        ] {
            let c = one(&format!(
                "{Q}    Then {text}\n      | x | 1 + 2 |\n      | 1 | [1, 'a\\|b'] |\n      | (:A {{n: 1}}) | <(:A)-[:T]->()> |\n"
            ));
            assert_eq!(
                c.steps[2].step,
                Step::Result {
                    rows,
                    lists,
                    table: table(
                        &["x", "1 + 2"],
                        &[&["1", "[1, 'a|b']"], &["(:A {n: 1})", "<(:A)-[:T]->()>"]]
                    ),
                },
                "{text}"
            );
        }
    }

    #[test]
    fn table_cells_are_unescaped_like_gherkin() {
        let c = one(&format!(
            "{Q}    Then the result should be, in any order:\n      | a | b |\n      | 'x\\|y' | 'a\\\\\\\\b\\'' |\n      | '\\nFoo\\n' | '\\t' |\n"
        ));
        let Step::Result { table, .. } = &c.steps[2].step else {
            panic!("not a result step")
        };
        assert_eq!(table.rows[0], vec!["'x|y'", r"'a\\b\''"]);
        assert_eq!(table.rows[1], vec!["'\nFoo\n'", r"'\t'"]);
    }

    #[test]
    fn ragged_table_is_an_error() {
        let e = parse_err(&format!(
            "Feature: F\n  Scenario: s\n{Q}    Then the result should be, in any order:\n      | a | b |\n      | 1 |\n"
        ));
        assert_eq!(e.line, 10);
        assert!(e.message.contains("cells"), "{}", e.message);
    }

    #[test]
    fn headerless_parameter_table() {
        let c = one(
            "    Given any graph\n    And parameters are:\n      | 1     | [{a: 1}] |\n      | _skip | 'x'      |\n    When executing query:\n      \"\"\"\n      RETURN $1\n      \"\"\"\n    Then the result should be empty\n",
        );
        assert_eq!(
            c.steps[1].step,
            Step::Parameters(vec![
                ("1".into(), "[{a: 1}]".into()),
                ("_skip".into(), "'x'".into()),
            ])
        );
    }

    #[test]
    fn side_effects_table() {
        let all = SideEffect::ALL
            .iter()
            .enumerate()
            .map(|(i, (_, k))| format!("      | {k} | {i} |\n"))
            .collect::<String>();
        let c = one(&format!(
            "{Q}    Then the result should be empty\n    And the side effects should be:\n{all}"
        ));
        assert_eq!(
            c.steps[3].step,
            Step::SideEffects(
                SideEffect::ALL
                    .iter()
                    .enumerate()
                    .map(|(i, (k, _))| (*k, i as u64))
                    .collect()
            )
        );
        let e = parse_err(&format!(
            "Feature: F\n  Scenario: s\n{Q}    Then the result should be empty\n    And the side effects should be:\n      | +edges | 1 |\n"
        ));
        assert!(e.message.contains("unknown side effect"), "{}", e.message);
        let e = parse_err(&format!(
            "Feature: F\n  Scenario: s\n{Q}    Then the result should be empty\n    And the side effects should be:\n      | +nodes | x |\n"
        ));
        assert!(e.message.contains("needs a count"), "{}", e.message);
    }

    #[test]
    fn error_steps() {
        for (text, kind, phase, detail) in [
            (
                "a SyntaxError should be raised at compile time: UndefinedVariable",
                ErrorType::SyntaxError,
                ErrorPhase::CompileTime,
                "UndefinedVariable",
            ),
            (
                "a TypeError should be raised at any time: *",
                ErrorType::TypeError,
                ErrorPhase::AnyTime,
                "*",
            ),
            (
                "an ArgumentError should be raised at runtime: NumberOutOfRange",
                ErrorType::ArgumentError,
                ErrorPhase::Runtime,
                "NumberOutOfRange",
            ),
            (
                "a ConstraintVerificationFailed should be raised at runtime: DeleteConnectedNode",
                ErrorType::ConstraintVerificationFailed,
                ErrorPhase::Runtime,
                "DeleteConnectedNode",
            ),
        ] {
            let c = one(&format!("{Q}    Then {text}\n"));
            assert_eq!(
                c.steps[2].step,
                Step::Error {
                    kind,
                    phase,
                    detail: detail.into()
                }
            );
        }
        for (ty, _) in ErrorType::ALL {
            let c = one(&format!(
                "{Q}    Then a {ty:?} should be raised at runtime: X\n"
            ));
            assert!(matches!(c.steps[2].step, Step::Error { kind, .. } if kind == ty));
        }
        for bad in [
            "a FooError should be raised at runtime: X",
            "a TypeError should be raised at lunch time: X",
            "a TypeError should be raised at runtime",
        ] {
            let e = parse_err(&format!("Feature: F\n  Scenario: s\n{Q}    Then {bad}\n"));
            assert_eq!(e.line, 8, "{bad}");
        }
    }

    #[test]
    fn procedure_step_with_empty_table() {
        let c = one(
            "    Given an empty graph\n    And there exists a procedure test.doNothing() :: ():\n      |\n    And there exists a procedure test.my.proc(in :: INTEGER?) :: (a :: INTEGER?, b :: INTEGER?) :\n      | in | a | b |\n      | null | 1 | 2 |\n    When executing query:\n      \"\"\"\n      CALL test.doNothing()\n      \"\"\"\n    Then the result should be empty\n",
        );
        assert_eq!(
            c.steps[1].step,
            Step::Procedure {
                signature: "test.doNothing() :: ()".into(),
                table: Table {
                    header: vec![],
                    rows: vec![]
                },
            }
        );
        assert_eq!(
            c.steps[2].step,
            Step::Procedure {
                signature: "test.my.proc(in :: INTEGER?) :: (a :: INTEGER?, b :: INTEGER?)".into(),
                table: table(&["in", "a", "b"], &[&["null", "1", "2"]]),
            }
        );
    }

    #[test]
    fn control_query_pair() {
        let c = one(&format!(
            "{Q}    Then the result should be empty\n    And the side effects should be:\n      | +nodes | 1 |\n    When executing control query:\n      \"\"\"\n      MATCH (n) RETURN n\n      \"\"\"\n    Then the result should be, in any order:\n      | n |\n      | () |\n"
        ));
        assert_eq!(
            c.steps[4].step,
            Step::ExecutingControlQuery("MATCH (n) RETURN n".into())
        );
        assert!(matches!(c.steps[5].step, Step::Result { .. }));
        // A control query without its result step, and an error step after it, are refused.
        let e = parse_err(&format!(
            "Feature: F\n  Scenario: s\n{Q}    Then the result should be empty\n    When executing control query:\n      \"\"\"\n      RETURN 1\n      \"\"\"\n"
        ));
        assert!(e.message.contains("control query"), "{}", e.message);
        let e = parse_err(&format!(
            "Feature: F\n  Scenario: s\n{Q}    Then the result should be empty\n    When executing control query:\n      \"\"\"\n      RETURN 1\n      \"\"\"\n    Then a TypeError should be raised at runtime: X\n"
        ));
        assert_eq!(e.line, 13);
    }

    #[test]
    fn step_order_is_validated() {
        // Setup after the query.
        let e = parse_err(&format!(
            "Feature: F\n  Scenario: s\n{Q}    And having executed:\n      \"\"\"\n      CREATE ()\n      \"\"\"\n    Then the result should be empty\n"
        ));
        assert_eq!(e.line, 8);
        // Two graph steps.
        let e = parse_err(&format!(
            "Feature: F\n  Scenario: s\n    Given any graph\n{Q}    Then the result should be empty\n"
        ));
        assert_eq!(e.line, 4);
        // No outcome.
        let e = parse_err(&format!("Feature: F\n  Scenario: s\n{Q}"));
        assert!(e.message.contains("no outcome"), "{}", e.message);
        // No query.
        let e = parse_err("Feature: F\n  Scenario: s\n    Given any graph\n");
        assert!(e.message.contains("executing query"), "{}", e.message);
    }

    #[test]
    fn missing_or_extra_step_argument_is_an_error() {
        let e = parse_err(
            "Feature: F\n  Scenario: s\n    Given any graph\n    When executing query:\n    Then the result should be empty\n",
        );
        assert_eq!(e.line, 4);
        assert!(e.message.contains("doc string"), "{}", e.message);
        let e = parse_err(&format!(
            "Feature: F\n  Scenario: s\n{Q}    Then the result should be empty\n      | a |\n"
        ));
        assert!(e.message.contains("takes no argument"), "{}", e.message);
    }

    #[test]
    fn unknown_step_is_an_error_with_file_and_line() {
        let e = parse_err(&format!(
            "Feature: F\n\n  Scenario: s\n{Q}    Then the result should be purple\n"
        ));
        assert_eq!(e.file, "x/T.feature");
        assert_eq!(e.line, 9);
        assert!(e.message.contains("unknown step text"), "{}", e.message);
        assert!(e.to_string().starts_with("x/T.feature:9: "));
        // The inline form of the upstream README is not in the vocabulary.
        let e = parse_err(
            "Feature: F\n  Scenario: s\n    Given any graph\n    When executing query: RETURN 1\n    Then the result should be empty\n",
        );
        assert_eq!(e.line, 4);
    }

    #[test]
    fn crlf_line_endings() {
        let text = format!(
            "#encoding: utf-8\r\n\r\nFeature: F\r\n\r\n  Scenario: s\r\n{}    Then the result should be, in order:\r\n      | x |\r\n      | 1 |\r\n",
            Q.replace('\n', "\r\n")
        );
        let f = parse(&text);
        let c = &f.scenarios[0].cases[0];
        assert_eq!(c.query(), "RETURN 1 AS x");
        assert_eq!(
            c.steps[2].step,
            Step::Result {
                rows: RowOrder::Ordered,
                lists: ListOrder::Strict,
                table: table(&["x"], &[&["1"]]),
            }
        );
        assert!(!format!("{f:?}").contains('\r'));
    }

    #[test]
    fn background_is_prepended_to_every_case() {
        let f = parse(
            "Feature: F\n  # comment\n  Background:\n    Given an empty graph\n    And having executed:\n      \"\"\"\n      CREATE (:A)\n      \"\"\"\n\n  Scenario: s1\n    When executing query:\n      \"\"\"\n      RETURN 1\n      \"\"\"\n    Then the result should be empty\n\n  Scenario: s2\n    When executing query:\n      \"\"\"\n      RETURN 2\n      \"\"\"\n    Then the result should be empty\n",
        );
        assert_eq!(f.background.len(), 2);
        for (s, q) in f.scenarios.iter().zip(["RETURN 1", "RETURN 2"]) {
            let c = &s.cases[0];
            assert_eq!(c.steps[0].step, Step::EmptyGraph);
            assert_eq!(c.steps[0].line, 4);
            assert_eq!(c.steps[1].step, Step::HavingExecuted("CREATE (:A)".into()));
            assert_eq!(c.query(), q);
        }
    }

    #[test]
    fn outline_expansion_substitutes_everywhere() {
        let f = parse(
            "Feature: F\n\n  Scenario Outline: [1] Example #Example: <name>\n    Given any graph\n    And parameters are:\n      | p | <value> |\n    When executing query:\n      \"\"\"\n      RETURN <expr> AS x, $p AS p\n      \"\"\"\n    Then the result should be, in any order:\n      | x        | p       |\n      | <result> | <value> |\n    And the side effects should be:\n      | +nodes | <n> |\n\n    Examples:\n      | name | expr  | result | value | n |\n      | one  | 1     | 1      | 'a'   | 0 |\n      | two  | 1 + 1 | 2      | [1]   | 3 |\n",
        );
        let s = &f.scenarios[0];
        assert!(s.outline);
        assert_eq!(s.raw_name, "[1] Example #Example: <name>");
        assert_eq!(s.cases.len(), 2);
        let c = &s.cases[1];
        assert_eq!(c.id, "x/T.feature::[1] Example #Example: <name>::2");
        assert_eq!(c.name, "[1] Example #Example: two");
        assert_eq!(c.example, Some(ExampleRow { row: 2, line: 20 }));
        assert_eq!(
            steps(c),
            vec![
                Step::AnyGraph,
                Step::Parameters(vec![("p".into(), "[1]".into())]),
                Step::ExecutingQuery("RETURN 1 + 1 AS x, $p AS p".into()),
                Step::Result {
                    rows: RowOrder::Unordered,
                    lists: ListOrder::Strict,
                    table: table(&["x", "p"], &[&["2", "[1]"]]),
                },
                Step::SideEffects(vec![(SideEffect::AddedNodes, 3)]),
            ]
        );
        assert_eq!(
            s.cases[0].id,
            "x/T.feature::[1] Example #Example: <name>::1"
        );
    }

    #[test]
    fn outline_placeholder_in_step_text_is_typed_after_substitution() {
        let f = parse(
            "Feature: F\n  Scenario Outline: o\n    Given any graph\n    When executing query:\n      \"\"\"\n      RETURN 1\n      \"\"\"\n    Then a <type> should be raised at runtime: X\n    Examples:\n      | type |\n      | TypeError |\n",
        );
        assert!(matches!(
            f.scenarios[0].cases[0].steps[2].step,
            Step::Error {
                kind: ErrorType::TypeError,
                ..
            }
        ));
        let e = parse_err(
            "Feature: F\n  Scenario Outline: o\n    Given any graph\n    When executing query:\n      \"\"\"\n      RETURN 1\n      \"\"\"\n    Then the result should be <how>\n    Examples:\n      | how |\n      | empty |\n      | purple |\n",
        );
        assert_eq!(e.line, 8);
    }

    #[test]
    fn outline_without_examples_is_an_error() {
        let e = parse_err(&format!(
            "Feature: F\n  Scenario Outline: o\n{Q}    Then the result should be empty\n"
        ));
        assert_eq!(e.line, 2);
        let e = parse_err(&format!(
            "Feature: F\n  Scenario: s\n{Q}    Then the result should be empty\n    Examples:\n      | a |\n      | 1 |\n"
        ));
        assert!(e.message.contains("plain"), "{}", e.message);
    }

    #[test]
    fn tags_and_ignore() {
        let f = parse(&format!(
            "@F\nFeature: F\n  This is a description.\n\n  @ignore @skipStyleCheck\n  Scenario: s1\n{Q}    Then the result should be empty\n\n  @skipGrammarCheck\n  Scenario: s2\n{Q}    Then the result should be empty\n"
        ));
        assert_eq!(f.tags, vec!["@F"]);
        let c1 = &f.scenarios[0].cases[0];
        assert_eq!(c1.tags, vec!["@F", "@ignore", "@skipStyleCheck"]);
        assert!(c1.is_ignored());
        let c2 = &f.scenarios[1].cases[0];
        assert_eq!(c2.tags, vec!["@F", "@skipGrammarCheck"]);
        assert!(!c2.is_ignored());
        assert_eq!(c1.id, "x/T.feature::s1");
    }

    #[test]
    fn duplicate_ids_are_an_error() {
        let e = parse_err(&format!(
            "Feature: F\n  Scenario: s\n{Q}    Then the result should be empty\n  Scenario: s\n{Q}    Then the result should be empty\n"
        ));
        assert!(e.message.contains("duplicate"), "{}", e.message);
    }
}
