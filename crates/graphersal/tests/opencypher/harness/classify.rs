//! Classifies a case run against the case's expectation (designs T9, T10; P0 G7, G8).
//!
//! # Classes
//!
//! Unsupported: `missing` (the query, or a control query, was refused as `Refused` / `NotYet`,
//! GQLSTATUS 0A000; the reason is the refusal's message, which names the construct, see
//! [`construct`]), `setup` (a named graph, `having executed` query or parameter value failed),
//! `excluded` (out of scope, `scope.rs`: not run, [`Outcome::excluded`]).
//! Wrong behaviour: `wrong_result`, `wrong_columns`, `wrong_side_effects`, `wrong_error` (an
//! error was expected and another one, or none, came), `unexpected_error` (an error of a
//! supported construct where a result was expected), `timeout` (the wall-clock guard or a limit
//! of the harness), `panic`.
//!
//! A refusal never passes a case that expects an error: it is `missing` there too, so the pass
//! count only rises through real support.
//!
//! # Errors (T9)
//!
//! `Then a <Type> should be raised at <phase>: <detail>` matches by type and phase only; the
//! detail is not compared, except that `ArgumentError` with detail `InvalidArgumentType` also
//! accepts 22G03 (P0 G7). The phase comes from where the error arose (`execute::Phase`); `any
//! time` accepts both. The type by family:
//!
//! | TCK type | compile time | runtime |
//! |---|---|---|
//! | `SyntaxError`, `SemanticError`, `ParameterMissing` | any GQL error (not 0A000) | any GQL error of class 22 |
//! | `TypeError` | 22G03, 22018 | 22G03, 22018 |
//! | `ArgumentError` | 22023, 22011, 22003, 2201E, 2201F, 22027 (+ 22G03 for `InvalidArgumentType`) | the same |
//! | `ArithmeticError` | 22003, 22012, 2201x | the same |
//! | `EntityNotFound` | never | an element read after its removal |
//! | `ConstraintVerificationFailed` | never | a GQL error of class G1 |
//! | `ProcedureError` (`ProcedureNotFound`: no saved query of that folder and name) | 42002 | never |
//! | the others (unused) | never | never |
//!
//! Errors imply no side effects (G8): a case that expects an error and gets a matching one is
//! `wrong_side_effects` when the state changed.

use super::compare::{self, ActualResult};
use super::execute::{ErrorRoot, MainRun, Phase, QueryError, QueryOutcome, RawOutcome};
use super::gherkin::{Case, ErrorPhase, ErrorType, Step};
use super::scope::{DETAIL_PREFIX, Exclusion};
use super::state::SideEffects;

/// The class of a case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Class {
    /// Every expectation holds.
    Pass,
    /// A construct of the query is refused or not yet supported.
    Missing,
    /// A setup step failed.
    Setup,
    /// Not run (out of scope).
    Excluded,
    /// The rows differ.
    WrongResult,
    /// The column names differ.
    WrongColumns,
    /// The side effects differ.
    WrongSideEffects,
    /// Another error than expected, or none.
    WrongError,
    /// An error where a result was expected.
    UnexpectedError,
    /// A time or resource budget fired.
    Timeout,
    /// A panic was caught.
    Panic,
}

impl Class {
    /// Every class, in report order.
    pub const ALL: [Class; 11] = [
        Class::Pass,
        Class::Missing,
        Class::Setup,
        Class::Excluded,
        Class::WrongResult,
        Class::WrongColumns,
        Class::WrongSideEffects,
        Class::WrongError,
        Class::UnexpectedError,
        Class::Timeout,
        Class::Panic,
    ];

    /// Stable lower-case name (for reports and the baseline).
    pub fn name(self) -> &'static str {
        match self {
            Class::Pass => "pass",
            Class::Missing => "missing",
            Class::Setup => "setup",
            Class::Excluded => "excluded",
            Class::WrongResult => "wrong_result",
            Class::WrongColumns => "wrong_columns",
            Class::WrongSideEffects => "wrong_side_effects",
            Class::WrongError => "wrong_error",
            Class::UnexpectedError => "unexpected_error",
            Class::Timeout => "timeout",
            Class::Panic => "panic",
        }
    }

    /// Whether the class counts as wrong behaviour of a supported construct.
    pub fn is_wrong(self) -> bool {
        match self {
            Class::Pass | Class::Missing | Class::Setup | Class::Excluded => false,
            Class::WrongResult
            | Class::WrongColumns
            | Class::WrongSideEffects
            | Class::WrongError
            | Class::UnexpectedError
            | Class::Timeout
            | Class::Panic => true,
        }
    }
}

/// The classification of one case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The class.
    pub class: Class,
    /// One-line reason: the refused construct, the setup failure, the first difference, ...
    pub detail: String,
}

impl Outcome {
    fn new(class: Class, detail: impl Into<String>) -> Self {
        Self {
            class,
            detail: detail.into(),
        }
    }

    /// The outcome of a case out of scope (`scope.rs`): class `excluded`, detail
    /// `excluded:<code>`.
    pub fn excluded(exclusion: &Exclusion) -> Self {
        Self::new(
            Class::Excluded,
            format!("{DETAIL_PREFIX}{}", exclusion.code),
        )
    }
}

/// The refused or "not yet" construct a `missing` / `setup` detail names, with its family:
/// `not yet: datetime()`, `refused: rand()`. The context prefix (`control query: `,
/// `having executed: `), the position and the rest of the message are dropped, so the name is
/// stable across message edits and ranks the backlog (P0 G11: the namespaced temporal functions
/// keep their function name). A detail without a refusal is returned on one line, unchanged.
pub fn construct(detail: &str) -> String {
    const MARKERS: [(&str, &str); 2] = [
        ("GQL construct not supported yet", "not yet"),
        ("GQL unsupported construct", "refused"),
    ];
    let Some((at, marker, family)) = MARKERS
        .iter()
        .find_map(|(m, f)| detail.find(m).map(|at| (at, *m, *f)))
    else {
        return one_line(detail);
    };
    let rest = &detail[at + marker.len()..];
    // Skip ` at line N, column M`, then `: `.
    let rest = rest.split_once(": ").map_or(rest, |(_, r)| r);
    let end = [
        " (a ",
        " is not yet supported",
        " is not supported",
        ": ",
        " (step #",
    ]
    .iter()
    .filter_map(|cut| rest.find(cut))
    .min()
    .unwrap_or(rest.len());
    format!("{family}: {}", rest[..end].trim())
}

/// The context prefix of a detail before its GQL message (`having executed: `), or "".
pub fn context_prefix(detail: &str) -> &str {
    detail.find("GQL ").map_or("", |at| &detail[..at])
}

/// What a case expects of its query.
enum Expectation<'a> {
    /// A result step (`Result` or `ResultEmpty`).
    Result(&'a Step),
    /// An error.
    Error {
        kind: ErrorType,
        phase: ErrorPhase,
        detail: &'a str,
    },
}

/// The expectation of the query under test, its side effects (`None`: not stated) and the
/// result step of every control query, in order.
fn expectations(case: &Case) -> (Option<Expectation<'_>>, Option<SideEffects>, Vec<&Step>) {
    let mut main = None;
    let mut side_effects = None;
    let mut controls = Vec::new();
    let mut in_control = false;
    for s in &case.steps {
        match &s.step {
            Step::ExecutingControlQuery(_) => in_control = true,
            step @ (Step::Result { .. } | Step::ResultEmpty) => {
                if in_control {
                    controls.push(step);
                } else if main.is_none() {
                    main = Some(Expectation::Result(step));
                }
            }
            Step::Error {
                kind,
                phase,
                detail,
            } => {
                main = Some(Expectation::Error {
                    kind: *kind,
                    phase: *phase,
                    detail,
                });
            }
            Step::NoSideEffects => side_effects = Some(SideEffects::default()),
            Step::SideEffects(table) => side_effects = Some(SideEffects::expected(table)),
            _ => {}
        }
    }
    (main, side_effects, controls)
}

/// Classifies one case's run.
pub fn classify(case: &Case, run: &RawOutcome) -> Outcome {
    match run {
        RawOutcome::Setup(reason) => Outcome::new(Class::Setup, reason.clone()),
        RawOutcome::Timeout => Outcome::new(
            Class::Timeout,
            format!(
                "wall-clock limit of {} s",
                super::execute::WALL_CLOCK_LIMIT.as_secs()
            ),
        ),
        RawOutcome::Panic(message) => Outcome::new(Class::Panic, message.clone()),
        RawOutcome::Ran(main) => classify_run(case, main),
    }
}

fn classify_run(case: &Case, run: &MainRun) -> Outcome {
    let (expectation, side_effects, controls) = expectations(case);
    let Some(expectation) = expectation else {
        return Outcome::new(Class::Panic, "harness: the case states no outcome");
    };
    let outcome = match (&expectation, &run.outcome) {
        (_, QueryOutcome::Error(e)) if e.is_unsupported() => {
            return Outcome::new(Class::Missing, e.message.clone());
        }
        (_, QueryOutcome::Error(e)) if e.is_limit() => {
            return Outcome::new(Class::Timeout, e.message.clone());
        }
        (Expectation::Result(_), QueryOutcome::Error(e)) => {
            return Outcome::new(Class::UnexpectedError, e.message.clone());
        }
        (
            Expectation::Error {
                kind,
                phase,
                detail,
            },
            QueryOutcome::Error(e),
        ) => {
            if !error_matches(*kind, *phase, detail, e) {
                return Outcome::new(
                    Class::WrongError,
                    format!(
                        "expected {} at {}, got {} at {}: {}",
                        kind.as_str(),
                        phase_name(*phase),
                        e.gqlstatus().unwrap_or("a non-GQL error"),
                        match e.phase {
                            Phase::Compile => "compile time",
                            Phase::Runtime => "runtime",
                        },
                        e.message
                    ),
                );
            }
            // G8: an error implies no side effects.
            if !run.side_effects.is_empty() {
                return Outcome::new(
                    Class::WrongSideEffects,
                    format!(
                        "the failed query changed the graph: {}",
                        run.side_effects.render()
                    ),
                );
            }
            Outcome::new(Class::Pass, "")
        }
        (Expectation::Error { kind, phase, .. }, QueryOutcome::Rows(rows)) => {
            return Outcome::new(
                Class::WrongError,
                format!(
                    "expected {} at {}, the query succeeded with {} row(s)",
                    kind.as_str(),
                    phase_name(*phase),
                    rows.as_ref().map_or(0, |r| r.rows.len())
                ),
            );
        }
        (Expectation::Result(step), QueryOutcome::Rows(rows)) => match check_result(step, rows) {
            ResultCheck::Match => Outcome::new(Class::Pass, ""),
            ResultCheck::WrongColumns(d) => return Outcome::new(Class::WrongColumns, d),
            ResultCheck::WrongResult(d) => return Outcome::new(Class::WrongResult, d),
        },
    };

    if let Some(expected) = &side_effects {
        let differences = run.side_effects.differences(expected);
        if !differences.is_empty() {
            return Outcome::new(Class::WrongSideEffects, differences.join("; "));
        }
    }

    for (i, step) in controls.iter().enumerate() {
        let Some(control) = run.controls.get(i) else {
            return Outcome::new(Class::Panic, "harness: a control query did not run");
        };
        match control {
            QueryOutcome::Error(e) if e.is_unsupported() => {
                return Outcome::new(Class::Missing, format!("control query: {}", e.message));
            }
            QueryOutcome::Error(e) if e.is_limit() => {
                return Outcome::new(Class::Timeout, format!("control query: {}", e.message));
            }
            QueryOutcome::Error(e) => {
                return Outcome::new(
                    Class::UnexpectedError,
                    format!("control query: {}", e.message),
                );
            }
            QueryOutcome::Rows(rows) => match check_result(step, rows) {
                ResultCheck::Match => {}
                ResultCheck::WrongColumns(d) => {
                    return Outcome::new(Class::WrongColumns, format!("control query: {d}"));
                }
                ResultCheck::WrongResult(d) => {
                    return Outcome::new(Class::WrongResult, format!("control query: {d}"));
                }
            },
        }
    }
    outcome
}

/// What the comparison of a result step found.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ResultCheck {
    /// The rows match the expected table.
    Match,
    /// The column names differ.
    WrongColumns(String),
    /// The rows differ (or could not be materialized).
    WrongResult(String),
}

/// Compares the materialized rows of a query with a [`Step::Result`] / [`Step::ResultEmpty`].
/// The column names are checked here first (when there is a row: an empty result carries
/// none), so a column difference is `wrong_columns`, then `compare::compare_result`.
fn check_result(expected: &Step, actual: &Result<ActualResult, String>) -> ResultCheck {
    let actual = match actual {
        Ok(actual) => actual,
        Err(e) => return ResultCheck::WrongResult(format!("rows not comparable: {e}")),
    };
    match expected {
        Step::ResultEmpty if actual.rows.is_empty() => ResultCheck::Match,
        Step::ResultEmpty => ResultCheck::WrongResult(format!(
            "expected no rows, got {} row(s)",
            actual.rows.len()
        )),
        Step::Result { rows, lists, table } => {
            if !actual.rows.is_empty() && actual.columns != table.header {
                return ResultCheck::WrongColumns(format!(
                    "columns differ: expected {:?}, actual {:?}",
                    table.header, actual.columns
                ));
            }
            match compare::compare_result(table, *rows, *lists, actual) {
                Ok(()) => ResultCheck::Match,
                Err(e) => ResultCheck::WrongResult(one_line(&e)),
            }
        }
        _ => ResultCheck::WrongResult("harness: not a result step".to_string()),
    }
}

/// `text` on one line (newlines as ` | `), cut to 300 characters.
fn one_line(text: &str) -> String {
    let joined = text.lines().map(str::trim).collect::<Vec<_>>().join(" | ");
    if joined.chars().count() > 300 {
        let mut cut: String = joined.chars().take(300).collect();
        cut.push('…');
        cut
    } else {
        joined
    }
}

fn phase_name(phase: ErrorPhase) -> &'static str {
    match phase {
        ErrorPhase::CompileTime => "compile time",
        ErrorPhase::Runtime => "runtime",
        ErrorPhase::AnyTime => "any time",
    }
}

/// Whether `actual` satisfies `Then a <kind> should be raised at <phase>: <detail>` (the table
/// in the module doc).
pub fn error_matches(
    kind: ErrorType,
    phase: ErrorPhase,
    detail: &str,
    actual: &QueryError,
) -> bool {
    let phase_ok = match phase {
        ErrorPhase::AnyTime => true,
        ErrorPhase::CompileTime => actual.phase == Phase::Compile,
        ErrorPhase::Runtime => actual.phase == Phase::Runtime,
    };
    if !phase_ok || actual.is_unsupported() {
        return false;
    }
    let status = actual.gqlstatus().unwrap_or("");
    let in_class = |class: &str| status.starts_with(class);
    let one_of = |codes: &[&str]| codes.contains(&status);
    const ARGUMENT: &[&str] = &["22023", "22011", "22003", "2201E", "2201F", "22027"];
    match kind {
        ErrorType::SyntaxError | ErrorType::SemanticError | ErrorType::ParameterMissing => {
            match actual.phase {
                Phase::Compile => matches!(actual.root, ErrorRoot::Gql { .. }),
                Phase::Runtime => in_class("22"),
            }
        }
        ErrorType::TypeError => one_of(&["22G03", "22018"]),
        ErrorType::ArgumentError => {
            one_of(ARGUMENT) || (detail == "InvalidArgumentType" && status == "22G03")
        }
        ErrorType::ArithmeticError => one_of(&["22003", "22012"]) || in_class("2201"),
        ErrorType::EntityNotFound => actual.root == ErrorRoot::ElementRemoved,
        ErrorType::ConstraintVerificationFailed => in_class("G1"),
        // A `CALL` is resolved against the catalog when the query is compiled (3h.P0).
        ErrorType::ProcedureError => actual.phase == Phase::Compile && status == "42002",
        ErrorType::ConstraintValidationFailed
        | ErrorType::PropertyNotFound
        | ErrorType::LabelNotFound => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use graphersal::error::GqlErrorKind;

    fn gql(phase: Phase, kind: GqlErrorKind, status: &str) -> QueryError {
        QueryError {
            phase,
            root: ErrorRoot::Gql {
                kind,
                gqlstatus: status.to_string(),
            },
            message: String::new(),
        }
    }

    #[test]
    fn construct_names_are_extracted() {
        let d = "GQL construct not supported yet at line 1, column 8: datetime() (a date or time \
                 function) is not yet supported in this version (step #1 'gql:error(\"0A000\")' x)";
        assert_eq!(construct(d), "not yet: datetime()");
        let d = "having executed: GQL unsupported construct at line 2, column 3: \
                 datetime.truncate(): namespaced functions are not supported (`.truncate` ...)";
        assert_eq!(construct(d), "refused: datetime.truncate()");
        assert_eq!(context_prefix(d), "having executed: ");
        let d = "GQL unsupported construct at line 1, column 8: rand() is not supported: x";
        assert_eq!(construct(d), "refused: rand()");
        let d = "control query: GQL construct not supported yet at line 1, column 1: MERGE is not \
                 yet supported in this version";
        assert_eq!(construct(d), "not yet: MERGE");
        let d = "GQL construct not supported yet at line 1, column 1: list indexing and slicing \
                 (x[0], x[1..2]) is not yet supported in this version";
        assert_eq!(
            construct(d),
            "not yet: list indexing and slicing (x[0], x[1..2])"
        );
        assert_eq!(construct("other\nerror"), "other | error");
        assert_eq!(context_prefix("other"), "");
    }

    fn case(outcome: &str, control: bool) -> Case {
        let control = if control {
            "    When executing control query:\n      \"\"\"\n      RETURN 1 AS y\n      \"\"\"\n    \
             Then the result should be, in any order:\n      | y |\n      | 1 |\n"
        } else {
            ""
        };
        let text = format!(
            "Feature: F\n\n  Scenario: s\n    Given an empty graph\n    When executing query:\n      \
             \"\"\"\n      RETURN 1 AS x\n      \"\"\"\n{outcome}{control}"
        );
        let mut feature = super::super::gherkin::parse_feature("x/T.feature", &text)
            .unwrap_or_else(|e| panic!("{e}"));
        feature.scenarios.remove(0).cases.remove(0)
    }

    fn ran(outcome: QueryOutcome, controls: Vec<QueryOutcome>) -> RawOutcome {
        RawOutcome::Ran(MainRun {
            outcome,
            side_effects: SideEffects::default(),
            controls,
        })
    }

    /// A "not yet" raised while the query runs (a construct found only at runtime) is
    /// `missing`, as one raised at compile time, whatever the case expects, and in a control
    /// query too.
    #[test]
    fn a_runtime_not_yet_is_missing() {
        let result = "    Then the result should be, in any order:\n      | x |\n      | 1 |\n";
        let error = "    Then a TypeError should be raised at runtime: InvalidArgumentType\n";
        let mut not_yet = gql(Phase::Runtime, GqlErrorKind::NotYet, "0A000");
        not_yet.message = "GQL construct not supported yet at line 1, column 8: \
                           toIntegerList() (a list conversion function) is not yet supported \
                           in this version"
            .to_string();
        for expected in [result, error] {
            let outcome = classify(
                &case(expected, false),
                &ran(QueryOutcome::Error(not_yet.clone()), vec![]),
            );
            assert_eq!(outcome.class, Class::Missing, "{outcome:?}");
            assert_eq!(construct(&outcome.detail), "not yet: toIntegerList()");
        }
        let refused = gql(Phase::Runtime, GqlErrorKind::Refused, "0A000");
        let outcome = classify(
            &case(result, false),
            &ran(QueryOutcome::Error(refused), vec![]),
        );
        assert_eq!(outcome.class, Class::Missing);
        // In a control query.
        let rows = QueryOutcome::Rows(Ok(ActualResult {
            columns: vec!["x".into()],
            rows: vec![vec![super::super::values::TckValue::Int(1)]],
        }));
        let outcome = classify(
            &case(result, true),
            &ran(rows, vec![QueryOutcome::Error(not_yet.clone())]),
        );
        assert_eq!(outcome.class, Class::Missing, "{outcome:?}");
        assert!(outcome.detail.starts_with("control query: "));
        // Any other runtime data error stays wrong behaviour.
        let data = gql(Phase::Runtime, GqlErrorKind::Data, "22007");
        let outcome = classify(
            &case(result, false),
            &ran(QueryOutcome::Error(data), vec![]),
        );
        assert_eq!(outcome.class, Class::UnexpectedError);
    }

    #[test]
    fn errors_match_by_type_and_phase() {
        let syntax = gql(Phase::Compile, GqlErrorKind::Syntax, "42001");
        assert!(error_matches(
            ErrorType::SyntaxError,
            ErrorPhase::CompileTime,
            "x",
            &syntax
        ));
        assert!(error_matches(
            ErrorType::SyntaxError,
            ErrorPhase::AnyTime,
            "x",
            &syntax
        ));
        assert!(!error_matches(
            ErrorType::SyntaxError,
            ErrorPhase::Runtime,
            "x",
            &syntax
        ));
        assert!(!error_matches(
            ErrorType::TypeError,
            ErrorPhase::CompileTime,
            "x",
            &syntax
        ));

        let wrong_kind = gql(Phase::Runtime, GqlErrorKind::Data, "22G03");
        assert!(error_matches(
            ErrorType::TypeError,
            ErrorPhase::Runtime,
            "x",
            &wrong_kind
        ));
        assert!(!error_matches(
            ErrorType::TypeError,
            ErrorPhase::CompileTime,
            "x",
            &wrong_kind
        ));
        assert!(error_matches(
            ErrorType::ArgumentError,
            ErrorPhase::Runtime,
            "InvalidArgumentType",
            &wrong_kind
        ));
        assert!(!error_matches(
            ErrorType::ArgumentError,
            ErrorPhase::Runtime,
            "NumberOutOfRange",
            &wrong_kind
        ));
        // SKIP / LIMIT parameters: SyntaxError at runtime = any runtime 22xxx.
        assert!(error_matches(
            ErrorType::SyntaxError,
            ErrorPhase::Runtime,
            "x",
            &wrong_kind
        ));

        let refused = gql(Phase::Compile, GqlErrorKind::NotYet, "0A000");
        for (kind, _) in ErrorType::ALL {
            assert!(!error_matches(kind, ErrorPhase::AnyTime, "x", &refused));
        }
    }

    #[test]
    fn a_procedure_error_is_a_compile_time_42002() {
        let not_found = gql(Phase::Compile, GqlErrorKind::Semantic, "42002");
        for phase in [ErrorPhase::CompileTime, ErrorPhase::AnyTime] {
            assert!(error_matches(
                ErrorType::ProcedureError,
                phase,
                "ProcedureNotFound",
                &not_found
            ));
        }
        assert!(!error_matches(
            ErrorType::ProcedureError,
            ErrorPhase::Runtime,
            "ProcedureNotFound",
            &not_found
        ));
        let syntax = gql(Phase::Compile, GqlErrorKind::Syntax, "42001");
        assert!(!error_matches(
            ErrorType::ProcedureError,
            ErrorPhase::CompileTime,
            "ProcedureNotFound",
            &syntax
        ));
        let at_runtime = gql(Phase::Runtime, GqlErrorKind::Semantic, "42002");
        assert!(!error_matches(
            ErrorType::ProcedureError,
            ErrorPhase::AnyTime,
            "ProcedureNotFound",
            &at_runtime
        ));
    }
}
