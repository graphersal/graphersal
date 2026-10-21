//! The scope of the TCK run (design T12): the test cases excluded from it, as an explicit,
//! reviewed table of case ids grouped by reason ([`EXCLUSIONS`]).
//!
//! Ids only, never tags or content, so a case that a TCK upgrade adds is never excluded
//! silently: an upgrade that renames a case makes the table name an id that does not exist,
//! which the target's `scope_table_is_consistent` test fails on. The families:
//!
//! 1. **Vendor procedures** (`procedures`): the `clauses/call` scenarios, which register test
//!    procedures with `And there exists a procedure ...` and call them. Procedure calls are vendor
//!    extensions, not GQL, and refused for good (`book/src/gql/refused.md`).
//! 2. **Upstream `@ignore`** (`upstream-ignore`): the one scenario the TCK itself marks as not
//!    run.
//! 3. **Deliberate deviations decided by the user** (rule 18: documented on the item, on the
//!    book page of the feature and on the compliance page):
//!    - `w21-property-kinds` (user decision 4, design W21): a list of maps (and any map or mixed
//!      list) is a valid property value in Graphersal; Neo4j raises a `TypeError`.
//!    - `parameter-at-compile-time` (user decision 2026-10-21, 3f.P0 U8): a negative or
//!      non-integer `$parameter` of `SKIP` / `LIMIT` is refused when the query is compiled
//!      (`22G02` / `22G03` before any row), since parameters are bound then; the TCK expects the
//!      `SyntaxError` at runtime. The user sees the same thing: an error before any row. The
//!      guard test checks that each case still gets its error at compile time.
//!    - `zone-name-not-kept` (user decision 2026-10-21, 3g.P0 U3): a `datetime` keeps its UTC
//!      offset, not the zone name (`datetime({.., timezone: 'Europe/Stockholm'})` is
//!      `..+01:00`); the cases that print the name or depend on it (the name in the expected
//!      text, `x.timezone` of a named zone, a daylight-saving step in `duration.between`).
//!    - `year-range` (user decision 2026-10-21, 3g.P0 U10): temporal values hold the years
//!      -9999 to 9999 (22008 beyond); Neo4j's two cases with larger years.
//!
//!    Both are documented in `src/gql/functions/temporal/mod.rs` ("Deliberate deviations from
//!    Neo4j") and in `book/src/gql/semantics.md` ("Where dates and times differ from Neo4j").
//!    Sub-microsecond values (U1), Cypher's zoned `time()` (U2) and `X.realtime()` (U9) are no
//!    exclusions: those cases stay `missing` (0A000 "not yet" / refused).
//!
//!    The former family `type-error-at-evaluation` (131 cases expecting a compile-time
//!    `InvalidArgumentType`) passes since stage 3f (user decision U9, the static kind checks of
//!    `gql/eval/kinds.rs`) and is gone; the run-time remainder (kinds not certain from the query
//!    text) has no case in the pinned tree.
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
        code: "procedures",
        reason: "vendor procedures: the scenarios register test procedures and CALL them; \
                 procedure calls are vendor extensions, not GQL, and refused for good",
        docs: "gql/refused.md",
        deviation: false,
        ids: PROCEDURES,
    },
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
        code: "zone-name-not-kept",
        reason: "a datetime keeps its UTC offset, not the zone name: a zone name resolves to its \
                 offset at that time, so the value prints +01:00, not +01:00[Europe/Stockholm], \
                 and a later change of the date keeps the offset (no daylight saving step)",
        docs: "gql/semantics.md#where-dates-and-times-differ-from-neo4j",
        deviation: true,
        ids: ZONE_NAME_NOT_KEPT,
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

/// The temporal cases whose expected value keeps a zone name (`[Europe/Stockholm]`) or depends
/// on it (3g.P0 U3: a zone resolves to its offset).
const ZONE_NAME_NOT_KEPT: &[&str] = &[
    "expressions/temporal/Temporal1.feature::[10] Should construct date time with named time zone::10",
    "expressions/temporal/Temporal1.feature::[10] Should construct date time with named time zone::11",
    "expressions/temporal/Temporal1.feature::[10] Should construct date time with named time zone::12",
    "expressions/temporal/Temporal1.feature::[10] Should construct date time with named time zone::13",
    "expressions/temporal/Temporal1.feature::[10] Should construct date time with named time zone::14",
    "expressions/temporal/Temporal1.feature::[10] Should construct date time with named time zone::16",
    "expressions/temporal/Temporal1.feature::[10] Should construct date time with named time zone::17",
    "expressions/temporal/Temporal1.feature::[10] Should construct date time with named time zone::18",
    "expressions/temporal/Temporal1.feature::[10] Should construct date time with named time zone::19",
    "expressions/temporal/Temporal1.feature::[10] Should construct date time with named time zone::2",
    "expressions/temporal/Temporal1.feature::[10] Should construct date time with named time zone::20",
    "expressions/temporal/Temporal1.feature::[10] Should construct date time with named time zone::21",
    "expressions/temporal/Temporal1.feature::[10] Should construct date time with named time zone::23",
    "expressions/temporal/Temporal1.feature::[10] Should construct date time with named time zone::24",
    "expressions/temporal/Temporal1.feature::[10] Should construct date time with named time zone::25",
    "expressions/temporal/Temporal1.feature::[10] Should construct date time with named time zone::26",
    "expressions/temporal/Temporal1.feature::[10] Should construct date time with named time zone::27",
    "expressions/temporal/Temporal1.feature::[10] Should construct date time with named time zone::28",
    "expressions/temporal/Temporal1.feature::[10] Should construct date time with named time zone::29",
    "expressions/temporal/Temporal1.feature::[10] Should construct date time with named time zone::3",
    "expressions/temporal/Temporal1.feature::[10] Should construct date time with named time zone::4",
    "expressions/temporal/Temporal1.feature::[10] Should construct date time with named time zone::5",
    "expressions/temporal/Temporal1.feature::[10] Should construct date time with named time zone::6",
    "expressions/temporal/Temporal1.feature::[10] Should construct date time with named time zone::7",
    "expressions/temporal/Temporal1.feature::[10] Should construct date time with named time zone::9",
    "expressions/temporal/Temporal10.feature::[8] Should handle durations at daylight saving time day::1",
    "expressions/temporal/Temporal10.feature::[8] Should handle durations at daylight saving time day::2",
    "expressions/temporal/Temporal10.feature::[8] Should handle durations at daylight saving time day::3",
    "expressions/temporal/Temporal10.feature::[8] Should handle durations at daylight saving time day::4",
    "expressions/temporal/Temporal10.feature::[8] Should handle durations at daylight saving time day::5",
    "expressions/temporal/Temporal10.feature::[8] Should handle durations at daylight saving time day::6",
    "expressions/temporal/Temporal2.feature::[6] Should parse date time with named time zone from string::1",
    "expressions/temporal/Temporal2.feature::[6] Should parse date time with named time zone from string::2",
    "expressions/temporal/Temporal2.feature::[6] Should parse date time with named time zone from string::3",
    "expressions/temporal/Temporal2.feature::[6] Should parse date time with named time zone from string::4",
    "expressions/temporal/Temporal2.feature::[6] Should parse date time with named time zone from string::5",
    "expressions/temporal/Temporal3.feature::[10] Should select date and time into date time::12",
    "expressions/temporal/Temporal3.feature::[10] Should select date and time into date time::13",
    "expressions/temporal/Temporal3.feature::[10] Should select date and time into date time::15",
    "expressions/temporal/Temporal3.feature::[10] Should select date and time into date time::16",
    "expressions/temporal/Temporal3.feature::[10] Should select date and time into date time::28",
    "expressions/temporal/Temporal3.feature::[10] Should select date and time into date time::29",
    "expressions/temporal/Temporal3.feature::[10] Should select date and time into date time::31",
    "expressions/temporal/Temporal3.feature::[10] Should select date and time into date time::32",
    "expressions/temporal/Temporal3.feature::[10] Should select date and time into date time::44",
    "expressions/temporal/Temporal3.feature::[10] Should select date and time into date time::45",
    "expressions/temporal/Temporal3.feature::[10] Should select date and time into date time::47",
    "expressions/temporal/Temporal3.feature::[10] Should select date and time into date time::48",
    "expressions/temporal/Temporal3.feature::[11] Should datetime into date time::10",
    "expressions/temporal/Temporal3.feature::[11] Should datetime into date time::5",
    "expressions/temporal/Temporal3.feature::[11] Should datetime into date time::6",
    "expressions/temporal/Temporal3.feature::[11] Should datetime into date time::7",
    "expressions/temporal/Temporal3.feature::[11] Should datetime into date time::9",
    "expressions/temporal/Temporal3.feature::[8] Should select date into date time::12",
    "expressions/temporal/Temporal3.feature::[8] Should select date into date time::4",
    "expressions/temporal/Temporal3.feature::[8] Should select date into date time::8",
    "expressions/temporal/Temporal3.feature::[9] Should select time into date time::12",
    "expressions/temporal/Temporal3.feature::[9] Should select time into date time::13",
    "expressions/temporal/Temporal3.feature::[9] Should select time into date time::15",
    "expressions/temporal/Temporal3.feature::[9] Should select time into date time::16",
    "expressions/temporal/Temporal9.feature::[2] Should truncate datetime::12",
    "expressions/temporal/Temporal9.feature::[2] Should truncate datetime::2",
    "expressions/temporal/Temporal9.feature::[2] Should truncate datetime::20",
    "expressions/temporal/Temporal9.feature::[2] Should truncate datetime::29",
    "expressions/temporal/Temporal9.feature::[2] Should truncate datetime::38",
    "expressions/temporal/Temporal9.feature::[2] Should truncate datetime::47",
    "expressions/temporal/Temporal9.feature::[2] Should truncate datetime::56",
    "expressions/temporal/Temporal9.feature::[2] Should truncate datetime::65",
    "expressions/temporal/Temporal9.feature::[2] Should truncate datetime::74",
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

/// The `clauses/call` cases (all of them use vendor procedures).
const PROCEDURES: &[&str] = &[
    "clauses/call/Call1.feature::[10] In-query call to procedure should fail if too many explicit argument are given",
    "clauses/call/Call1.feature::[11] Standalone call to procedure should fail if implicit argument is missing",
    "clauses/call/Call1.feature::[12] In-query call to procedure that has outputs fails if no outputs are yielded",
    "clauses/call/Call1.feature::[13] Standalone call to unknown procedure should fail",
    "clauses/call/Call1.feature::[14] In-query call to unknown procedure should fail",
    "clauses/call/Call1.feature::[15] In-query procedure call should fail if shadowing an already bound variable",
    "clauses/call/Call1.feature::[16] In-query procedure call should fail if one of the argument expressions uses an aggregation function",
    "clauses/call/Call1.feature::[1] Standalone call to procedure that takes no arguments and yields no results",
    "clauses/call/Call1.feature::[2] Standalone call to procedure that takes no arguments and yields no results, called with implicit arguments",
    "clauses/call/Call1.feature::[3] In-query call to procedure that takes no arguments and yields no results",
    "clauses/call/Call1.feature::[4] In-query call to procedure that takes no arguments and yields no results and consumes no rows",
    "clauses/call/Call1.feature::[5] Standalone call to STRING procedure that takes no arguments",
    "clauses/call/Call1.feature::[6] In-query call to STRING procedure that takes no arguments",
    "clauses/call/Call1.feature::[7] Standalone call to procedure should fail if explicit argument is missing",
    "clauses/call/Call1.feature::[8] In-query call to procedure should fail if explicit argument is missing",
    "clauses/call/Call1.feature::[9] Standalone call to procedure should fail if too many explicit argument are given",
    "clauses/call/Call2.feature::[1] In-query call to procedure with explicit arguments",
    "clauses/call/Call2.feature::[2] Standalone call to procedure with explicit arguments",
    "clauses/call/Call2.feature::[3] Standalone call to procedure with implicit arguments",
    "clauses/call/Call2.feature::[4] In-query call to procedure that takes arguments fails when trying to pass them implicitly",
    "clauses/call/Call2.feature::[5] Standalone call to procedure should fail if input type is wrong",
    "clauses/call/Call2.feature::[6] In-query call to procedure should fail if input type is wrong",
    "clauses/call/Call3.feature::[1] Standalone call to procedure with argument of type NUMBER accepts value of type INTEGER",
    "clauses/call/Call3.feature::[2] In-query call to procedure with argument of type NUMBER accepts value of type INTEGER",
    "clauses/call/Call3.feature::[3] Standalone call to procedure with argument of type NUMBER accepts value of type FLOAT",
    "clauses/call/Call3.feature::[4] In-query call to procedure with argument of type NUMBER accepts value of type FLOAT",
    "clauses/call/Call3.feature::[5] Standalone call to procedure with argument of type FLOAT accepts value of type INTEGER",
    "clauses/call/Call3.feature::[6] In-query call to procedure with argument of type FLOAT accepts value of type INTEGER",
    "clauses/call/Call4.feature::[1] Standalone call to procedure with null argument",
    "clauses/call/Call4.feature::[2] In-query call to procedure with null argument",
    "clauses/call/Call5.feature::[1] Explicit procedure result projection",
    "clauses/call/Call5.feature::[2] Explicit procedure result projection with RETURN *",
    "clauses/call/Call5.feature::[3] The order of yield items is irrelevant::1",
    "clauses/call/Call5.feature::[3] The order of yield items is irrelevant::2",
    "clauses/call/Call5.feature::[4] Rename outputs to unbound variable names::1",
    "clauses/call/Call5.feature::[4] Rename outputs to unbound variable names::10",
    "clauses/call/Call5.feature::[4] Rename outputs to unbound variable names::11",
    "clauses/call/Call5.feature::[4] Rename outputs to unbound variable names::2",
    "clauses/call/Call5.feature::[4] Rename outputs to unbound variable names::3",
    "clauses/call/Call5.feature::[4] Rename outputs to unbound variable names::4",
    "clauses/call/Call5.feature::[4] Rename outputs to unbound variable names::5",
    "clauses/call/Call5.feature::[4] Rename outputs to unbound variable names::6",
    "clauses/call/Call5.feature::[4] Rename outputs to unbound variable names::7",
    "clauses/call/Call5.feature::[4] Rename outputs to unbound variable names::8",
    "clauses/call/Call5.feature::[4] Rename outputs to unbound variable names::9",
    "clauses/call/Call5.feature::[5] Fail on renaming to an already bound variable name",
    "clauses/call/Call5.feature::[6] Fail on renaming all outputs to the same variable name",
    "clauses/call/Call5.feature::[7] Fail on in-query call to procedure with YIELD *",
    "clauses/call/Call5.feature::[8] Allow standalone call to procedure with YIELD *",
    "clauses/call/Call6.feature::[1] Calling the same STRING procedure twice using the same outputs in each call",
    "clauses/call/Call6.feature::[2] Project procedure results between query scopes with WITH clause",
    "clauses/call/Call6.feature::[3] Project procedure results between query scopes with WITH clause and rename the projection",
];

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
        assert_eq!(PROCEDURES.len(), 52);
    }

    #[test]
    fn lookups() {
        let id = "expressions/graph/Graph5.feature::[2] Single-labels expression on relationships";
        assert_eq!(exclusion(id).map(|e| e.code), Some("upstream-ignore"));
        assert_eq!(exclusion("nope"), None);
        assert_eq!(by_code("procedures").map(|e| e.ids.len()), Some(52));
        assert_eq!(code_of_detail("excluded:procedures"), Some("procedures"));
        assert_eq!(code_of_detail("procedures"), None);
    }
}
