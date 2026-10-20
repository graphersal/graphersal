//! Classifies a case run against the case's expectation (designs T9, T10; P0 G7, G8).
//!
//! # Classes
//!
//! Unsupported: `missing` (the query, or a control query, was refused as `Refused` / `NotYet`,
//! GQLSTATUS 0A000; the reason is the refusal's message, which names the construct), `setup` (a
//! named graph, `having executed` query or parameter value failed), `excluded` (not run).
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
//! | the others (procedures, unused) | never | never |
//!
//! Errors imply no side effects (G8): a case that expects an error and gets a matching one is
//! `wrong_side_effects` when the state changed.

use super::execute::{ErrorRoot, MainRun, Phase, QueryError, QueryOutcome, RawOutcome};
use super::gherkin::{Case, ErrorPhase, ErrorType, Step};
use super::stand_in::{self, ResultCheck};
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
    /// TEMPORARY (until 3b.T3's comparison): a pass whose rows were not compared.
    pub result_unchecked: bool,
}

impl Outcome {
    fn new(class: Class, detail: impl Into<String>) -> Self {
        Self {
            class,
            detail: detail.into(),
            result_unchecked: false,
        }
    }
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
        RawOutcome::NotRun(reason) => Outcome::new(Class::Excluded, reason.clone()),
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
    let mut outcome = match (&expectation, &run.outcome) {
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
                    rows.len()
                ),
            );
        }
        (Expectation::Result(step), QueryOutcome::Rows(rows)) => {
            match stand_in::check_result(step, rows) {
                ResultCheck::Match => Outcome::new(Class::Pass, ""),
                ResultCheck::Unchecked => Outcome {
                    result_unchecked: true,
                    ..Outcome::new(Class::Pass, "")
                },
                ResultCheck::WrongColumns(d) => return Outcome::new(Class::WrongColumns, d),
                ResultCheck::WrongResult(d) => return Outcome::new(Class::WrongResult, d),
            }
        }
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
            QueryOutcome::Rows(rows) => match stand_in::check_result(step, rows) {
                ResultCheck::Match => {}
                ResultCheck::Unchecked => outcome.result_unchecked = true,
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
        ErrorType::ConstraintValidationFailed
        | ErrorType::PropertyNotFound
        | ErrorType::LabelNotFound
        | ErrorType::ProcedureError => false,
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
}
