//! The scope of the TCK run (design T12): the test cases excluded from it, as an explicit,
//! reviewed table of case ids grouped by reason ([`EXCLUSIONS`]).
//!
//! Ids only, never tags or content, so a case that a TCK upgrade adds is never excluded
//! silently: an upgrade that renames a case makes the table name an id that does not exist,
//! which the target's `scope_table_is_consistent` test fails on. The families:
//!
//! 1. **Upstream `@ignore`** (`upstream-ignore`): the one scenario the TCK itself marks as not
//!    run.
//! 2. **Deliberate deviations decided by the user** (rule 18: documented on the item, on the
//!    book page of the feature and on the compliance page):
//!    - `w21-property-kinds` (user decision 4, design W21): a list of maps (and any map or mixed
//!      list) is a valid property value in Graphersal; Neo4j raises a `TypeError`.
//!    - `parameter-at-compile-time` (user decision 2026-10-21, 3f.P0 U8): a negative or
//!      non-integer `$parameter` of `SKIP` / `LIMIT` is refused when the query is compiled
//!      (`22G02` / `22G03` before any row), since parameters are bound then; the TCK expects the
//!      `SyntaxError` at runtime. The user sees the same thing: an error before any row. The
//!      guard test checks that each case still gets its error at compile time.
//!    - `year-range` (user decision 2026-10-21, 3g.P0 U10): temporal values hold the years
//!      -9999 to 9999 (22008 beyond); Neo4j's two cases with larger years.
//!
//!    `year-range` is documented in `src/gql/functions/temporal/mod.rs` ("Deliberate deviations
//!    from Neo4j") and in `book/src/gql/semantics.md` ("Where dates and times differ from
//!    Neo4j"). `X.realtime()` (U9) is no exclusion: those cases stay `missing` (refused).
//!
//!    The former family `zone-name-not-kept` (3g.P0 U3, 101 cases) is gone since temporal v2: a
//!    `datetime` keeps its zone name and computes in its zone, Cypher's `time()` builds a time
//!    with an offset (U2), and values hold nanoseconds (U1).
//!
//!    The former family `type-error-at-evaluation` (131 cases expecting a compile-time
//!    `InvalidArgumentType`) passes since stage 3f (user decision U9, the static kind checks of
//!    `gql/eval/kinds.rs`) and is gone; the run-time remainder (kinds not certain from the query
//!    text) has no case in the pinned tree.
//!
//!    The former family `procedures` (the `clauses/call` scenarios) is gone since stage 3h: the
//!    harness registers each test procedure as a saved GQL query (`procedure.rs`), which GQL's
//!    `CALL folder.name(..)` calls.
//!
//!    D15 (`id()`) and W26 / W28 have no case in the pinned tree: `id()` does not occur, no case
//!    sets a label on a relationship, and no case returns a deleted element itself.
//!
//! An excluded case is not executed, gets the class `excluded` (detail [`DETAIL_PREFIX`] + its
//! code), is left out of every total and percentage and has no baseline entry (an entry for it
//! is stale). Merely unimplemented constructs (temporal functions, ...) are
//! never excluded: they stay `missing`, the backlog.

/// Prefix of the outcome detail of an excluded case; the exclusion code follows it.
pub const DETAIL_PREFIX: &str = "excluded:";

/// One family of excluded cases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exclusion {
    /// Stable code (report, page, outcome detail).
    pub code: &'static str,
    /// Why these cases are out of scope (one sentence).
    pub reason: &'static str,
    /// Where the reason is documented, relative to `book/src/` (`page.md#anchor`).
    pub docs: &'static str,
    /// Whether this is a deliberate deviation (the cases run in the guard test and must not
    /// pass), as opposed to cases that cannot run at all.
    pub deviation: bool,
    /// The excluded case ids (`<path>::<raw name>[::<row>]`).
    pub ids: &'static [&'static str],
}

/// Every exclusion, in report order.
pub const EXCLUSIONS: &[Exclusion] = &[
    Exclusion {
        code: "upstream-ignore",
        reason: "marked @ignore by the TCK itself (\"does not work in Cypher\")",
        docs: "gql/tck_compliance.md#out-of-scope",
        deviation: false,
        ids: &["expressions/graph/Graph5.feature::[2] Single-labels expression on relationships"],
    },
    Exclusion {
        code: "w21-property-kinds",
        reason: "a list of maps is a valid property value (maps and lists of any kinds are \
                 stored as given); Neo4j raises a TypeError",
        docs: "gql/semantics.md#writes",
        deviation: true,
        ids: &["clauses/set/Set1.feature::[10] Failing when setting a list of maps as a property"],
    },
    Exclusion {
        code: "parameter-at-compile-time",
        reason: "parameters are bound when the query is compiled: a negative or non-integer \
                 SKIP / LIMIT parameter is refused then (22G02 / 22G03), not at runtime",
        docs: "gql/semantics.md#parameters-are-bound-when-the-query-is-compiled",
        deviation: true,
        ids: PARAMETER_AT_COMPILE_TIME,
    },
    Exclusion {
        code: "year-range",
        reason: "temporal values hold the years -9999 to 9999; a year beyond them is a data \
                 error (22008), Neo4j reads years up to 999999999",
        docs: "gql/semantics.md#where-dates-and-times-differ-from-neo4j",
        deviation: true,
        ids: YEAR_RANGE,
    },
];

/// The temporal cases with years beyond -9999..9999 (3g.P0 U10).
const YEAR_RANGE: &[&str] = &[
    "expressions/temporal/Temporal10.feature::[10] Should handle large durations in seconds",
    "expressions/temporal/Temporal10.feature::[9] Should handle large durations",
];

/// The `SKIP` / `LIMIT` cases with an invalid parameter (3f.P0 U8).
const PARAMETER_AT_COMPILE_TIME: &[&str] = &[
    "clauses/return-skip-limit/ReturnSkipLimit1.feature::[6] Negative parameter for SKIP should fail",
    "clauses/return-skip-limit/ReturnSkipLimit1.feature::[8] Floating point parameter for SKIP should fail",
    "clauses/return-skip-limit/ReturnSkipLimit2.feature::[10] Negative parameter for LIMIT should fail",
    "clauses/return-skip-limit/ReturnSkipLimit2.feature::[11] Negative parameter for LIMIT with ORDER BY should fail",
    "clauses/return-skip-limit/ReturnSkipLimit2.feature::[14] Floating point parameter for LIMIT should fail",
    "clauses/return-skip-limit/ReturnSkipLimit2.feature::[15] Floating point parameter for LIMIT with ORDER BY should fail",
];

/// The exclusion that covers the case `id`, if any.
pub fn exclusion(id: &str) -> Option<&'static Exclusion> {
    EXCLUSIONS.iter().find(|e| e.ids.contains(&id))
}

/// The exclusion with this code.
pub fn by_code(code: &str) -> Option<&'static Exclusion> {
    EXCLUSIONS.iter().find(|e| e.code == code)
}

/// The exclusion code of an outcome detail ([`DETAIL_PREFIX`] + code).
pub fn code_of_detail(detail: &str) -> Option<&str> {
    detail.strip_prefix(DETAIL_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn codes_and_ids_are_unique() {
        let mut codes = HashSet::new();
        let mut ids = HashSet::new();
        for e in EXCLUSIONS {
            assert!(codes.insert(e.code), "duplicate code {}", e.code);
            assert!(!e.ids.is_empty(), "{} lists no id", e.code);
            for id in e.ids {
                assert!(ids.insert(*id), "{id} is listed twice");
            }
        }
    }

    #[test]
    fn lookups() {
        let id = "expressions/graph/Graph5.feature::[2] Single-labels expression on relationships";
        assert_eq!(exclusion(id).map(|e| e.code), Some("upstream-ignore"));
        assert_eq!(exclusion("nope"), None);
        assert_eq!(by_code("year-range").map(|e| e.ids.len()), Some(2));
        assert_eq!(code_of_detail("excluded:year-range"), Some("year-range"));
        assert_eq!(code_of_detail("procedures"), None);
    }
}
