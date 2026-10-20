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
//!    - `type-error-at-evaluation` (user decision 2026-10-20): the TCK expects a `SyntaxError` or
//!      `TypeError` *at compile time* (detail `InvalidArgumentType`) for an operand whose kind is
//!      known from the query text (`123 AND true`, `WITH 123 AS x RETURN x.num`); Graphersal's GQL
//!      runs inside a Gremlin step, so a type error surfaces at evaluation time (`22G03` when the
//!      row reaches the expression). The ids were generated once from a run (every case that
//!      expected such an error and got `22G03` at runtime) and reviewed; the target's
//!      `deviation_cases_still_deviate` test checks that each still expects that and still gets it.
//!    - `w21-property-kinds` (user decision 4, design W21): a list of maps (and any map or mixed
//!      list) is a valid property value in Graphersal; Neo4j raises a `TypeError`.
//!
//!    D15 (`id()`) and W26 / W28 have no case in the pinned tree: `id()` does not occur, no case
//!    sets a label on a relationship, and no case returns a deleted element itself.
//!
//! An excluded case is not executed, gets the class `excluded` (detail [`DETAIL_PREFIX`] + its
//! code), is left out of every total and percentage and has no baseline entry (an entry for it
//! is stale). Merely unimplemented constructs (temporal types, `MERGE`, list indexing, ...) are
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
        code: "type-error-at-evaluation",
        reason: "the TCK expects a type error at compile time; GQL runs inside a Gremlin step: \
                 type errors surface at evaluation time (22G03 when a row reaches the expression)",
        docs: "gql/semantics.md#type-errors-surface-at-evaluation-time",
        deviation: true,
        ids: TYPE_ERROR_AT_EVALUATION,
    },
    Exclusion {
        code: "w21-property-kinds",
        reason: "a list of maps is a valid property value (maps and lists of any kinds are \
                 stored as given); Neo4j raises a TypeError",
        docs: "gql/semantics.md#writes",
        deviation: true,
        ids: &["clauses/set/Set1.feature::[10] Failing when setting a list of maps as a property"],
    },
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

/// Cases that expect `SyntaxError` / `TypeError` at compile time with detail
/// `InvalidArgumentType` where Graphersal raises `22G03` at runtime (generated once from a run,
/// reviewed; see the module doc).
const TYPE_ERROR_AT_EVALUATION: &[&str] = &[
    "expressions/boolean/Boolean1.feature::[8] Fail on conjunction of at least one non-booleans::1",
    "expressions/boolean/Boolean1.feature::[8] Fail on conjunction of at least one non-booleans::11",
    "expressions/boolean/Boolean1.feature::[8] Fail on conjunction of at least one non-booleans::13",
    "expressions/boolean/Boolean1.feature::[8] Fail on conjunction of at least one non-booleans::14",
    "expressions/boolean/Boolean1.feature::[8] Fail on conjunction of at least one non-booleans::15",
    "expressions/boolean/Boolean1.feature::[8] Fail on conjunction of at least one non-booleans::16",
    "expressions/boolean/Boolean1.feature::[8] Fail on conjunction of at least one non-booleans::19",
    "expressions/boolean/Boolean1.feature::[8] Fail on conjunction of at least one non-booleans::2",
    "expressions/boolean/Boolean1.feature::[8] Fail on conjunction of at least one non-booleans::20",
    "expressions/boolean/Boolean1.feature::[8] Fail on conjunction of at least one non-booleans::21",
    "expressions/boolean/Boolean1.feature::[8] Fail on conjunction of at least one non-booleans::22",
    "expressions/boolean/Boolean1.feature::[8] Fail on conjunction of at least one non-booleans::23",
    "expressions/boolean/Boolean1.feature::[8] Fail on conjunction of at least one non-booleans::3",
    "expressions/boolean/Boolean1.feature::[8] Fail on conjunction of at least one non-booleans::4",
    "expressions/boolean/Boolean1.feature::[8] Fail on conjunction of at least one non-booleans::5",
    "expressions/boolean/Boolean1.feature::[8] Fail on conjunction of at least one non-booleans::6",
    "expressions/boolean/Boolean1.feature::[8] Fail on conjunction of at least one non-booleans::7",
    "expressions/boolean/Boolean1.feature::[8] Fail on conjunction of at least one non-booleans::8",
    "expressions/boolean/Boolean1.feature::[8] Fail on conjunction of at least one non-booleans::9",
    "expressions/boolean/Boolean2.feature::[8] Fail on disjunction of at least one non-booleans::1",
    "expressions/boolean/Boolean2.feature::[8] Fail on disjunction of at least one non-booleans::10",
    "expressions/boolean/Boolean2.feature::[8] Fail on disjunction of at least one non-booleans::12",
    "expressions/boolean/Boolean2.feature::[8] Fail on disjunction of at least one non-booleans::13",
    "expressions/boolean/Boolean2.feature::[8] Fail on disjunction of at least one non-booleans::16",
    "expressions/boolean/Boolean2.feature::[8] Fail on disjunction of at least one non-booleans::17",
    "expressions/boolean/Boolean2.feature::[8] Fail on disjunction of at least one non-booleans::18",
    "expressions/boolean/Boolean2.feature::[8] Fail on disjunction of at least one non-booleans::19",
    "expressions/boolean/Boolean2.feature::[8] Fail on disjunction of at least one non-booleans::2",
    "expressions/boolean/Boolean2.feature::[8] Fail on disjunction of at least one non-booleans::20",
    "expressions/boolean/Boolean2.feature::[8] Fail on disjunction of at least one non-booleans::21",
    "expressions/boolean/Boolean2.feature::[8] Fail on disjunction of at least one non-booleans::22",
    "expressions/boolean/Boolean2.feature::[8] Fail on disjunction of at least one non-booleans::23",
    "expressions/boolean/Boolean2.feature::[8] Fail on disjunction of at least one non-booleans::3",
    "expressions/boolean/Boolean2.feature::[8] Fail on disjunction of at least one non-booleans::4",
    "expressions/boolean/Boolean2.feature::[8] Fail on disjunction of at least one non-booleans::5",
    "expressions/boolean/Boolean2.feature::[8] Fail on disjunction of at least one non-booleans::6",
    "expressions/boolean/Boolean2.feature::[8] Fail on disjunction of at least one non-booleans::7",
    "expressions/boolean/Boolean2.feature::[8] Fail on disjunction of at least one non-booleans::8",
    "expressions/boolean/Boolean2.feature::[8] Fail on disjunction of at least one non-booleans::9",
    "expressions/boolean/Boolean3.feature::[8] Fail on exclusive disjunction of at least one non-booleans::1",
    "expressions/boolean/Boolean3.feature::[8] Fail on exclusive disjunction of at least one non-booleans::10",
    "expressions/boolean/Boolean3.feature::[8] Fail on exclusive disjunction of at least one non-booleans::11",
    "expressions/boolean/Boolean3.feature::[8] Fail on exclusive disjunction of at least one non-booleans::12",
    "expressions/boolean/Boolean3.feature::[8] Fail on exclusive disjunction of at least one non-booleans::13",
    "expressions/boolean/Boolean3.feature::[8] Fail on exclusive disjunction of at least one non-booleans::14",
    "expressions/boolean/Boolean3.feature::[8] Fail on exclusive disjunction of at least one non-booleans::15",
    "expressions/boolean/Boolean3.feature::[8] Fail on exclusive disjunction of at least one non-booleans::16",
    "expressions/boolean/Boolean3.feature::[8] Fail on exclusive disjunction of at least one non-booleans::17",
    "expressions/boolean/Boolean3.feature::[8] Fail on exclusive disjunction of at least one non-booleans::18",
    "expressions/boolean/Boolean3.feature::[8] Fail on exclusive disjunction of at least one non-booleans::19",
    "expressions/boolean/Boolean3.feature::[8] Fail on exclusive disjunction of at least one non-booleans::2",
    "expressions/boolean/Boolean3.feature::[8] Fail on exclusive disjunction of at least one non-booleans::20",
    "expressions/boolean/Boolean3.feature::[8] Fail on exclusive disjunction of at least one non-booleans::21",
    "expressions/boolean/Boolean3.feature::[8] Fail on exclusive disjunction of at least one non-booleans::22",
    "expressions/boolean/Boolean3.feature::[8] Fail on exclusive disjunction of at least one non-booleans::23",
    "expressions/boolean/Boolean3.feature::[8] Fail on exclusive disjunction of at least one non-booleans::3",
    "expressions/boolean/Boolean3.feature::[8] Fail on exclusive disjunction of at least one non-booleans::4",
    "expressions/boolean/Boolean3.feature::[8] Fail on exclusive disjunction of at least one non-booleans::5",
    "expressions/boolean/Boolean3.feature::[8] Fail on exclusive disjunction of at least one non-booleans::6",
    "expressions/boolean/Boolean3.feature::[8] Fail on exclusive disjunction of at least one non-booleans::7",
    "expressions/boolean/Boolean3.feature::[8] Fail on exclusive disjunction of at least one non-booleans::8",
    "expressions/boolean/Boolean3.feature::[8] Fail on exclusive disjunction of at least one non-booleans::9",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::1",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::10",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::11",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::12",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::13",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::14",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::15",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::16",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::17",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::18",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::19",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::2",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::20",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::21",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::22",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::23",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::24",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::25",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::26",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::28",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::3",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::31",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::32",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::33",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::34",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::37",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::38",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::39",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::4",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::42",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::43",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::44",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::46",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::47",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::48",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::49",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::5",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::6",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::7",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::8",
    "expressions/boolean/Boolean4.feature::[4] Fail when using NOT on a non-boolean literal::9",
    "expressions/graph/Graph3.feature::[8] `labels()` failing on a path",
    "expressions/graph/Graph6.feature::[9] Fail when performing property access on a non-graph element::1",
    "expressions/graph/Graph6.feature::[9] Fail when performing property access on a non-graph element::2",
    "expressions/graph/Graph6.feature::[9] Fail when performing property access on a non-graph element::3",
    "expressions/graph/Graph6.feature::[9] Fail when performing property access on a non-graph element::4",
    "expressions/graph/Graph6.feature::[9] Fail when performing property access on a non-graph element::5",
    "expressions/graph/Graph6.feature::[9] Fail when performing property access on a non-graph element::6",
    "expressions/graph/Graph9.feature::[5] `properties()` failing on an integer literal",
    "expressions/graph/Graph9.feature::[6] `properties()` failing on a string literal",
    "expressions/graph/Graph9.feature::[7] `properties()` failing on a list of booleans",
    "expressions/map/Map1.feature::[6] Fail when performing property access on a non-map::1",
    "expressions/map/Map1.feature::[6] Fail when performing property access on a non-map::2",
    "expressions/map/Map1.feature::[6] Fail when performing property access on a non-map::3",
    "expressions/map/Map1.feature::[6] Fail when performing property access on a non-map::4",
    "expressions/map/Map1.feature::[6] Fail when performing property access on a non-map::5",
    "expressions/map/Map1.feature::[6] Fail when performing property access on a non-map::6",
    "expressions/quantifier/Quantifier1.feature::[15] Fail none quantifier on type mismatch between list elements and predicate::1",
    "expressions/quantifier/Quantifier1.feature::[15] Fail none quantifier on type mismatch between list elements and predicate::2",
    "expressions/quantifier/Quantifier1.feature::[15] Fail none quantifier on type mismatch between list elements and predicate::3",
    "expressions/quantifier/Quantifier2.feature::[16] Fail single quantifier on type mismatch between list elements and predicate::1",
    "expressions/quantifier/Quantifier2.feature::[16] Fail single quantifier on type mismatch between list elements and predicate::2",
    "expressions/quantifier/Quantifier2.feature::[16] Fail single quantifier on type mismatch between list elements and predicate::3",
    "expressions/quantifier/Quantifier3.feature::[15] Fail any quantifier on type mismatch between list elements and predicate::1",
    "expressions/quantifier/Quantifier3.feature::[15] Fail any quantifier on type mismatch between list elements and predicate::2",
    "expressions/quantifier/Quantifier3.feature::[15] Fail any quantifier on type mismatch between list elements and predicate::3",
    "expressions/quantifier/Quantifier4.feature::[15] Fail all quantifier on type mismatch between list elements and predicate::1",
    "expressions/quantifier/Quantifier4.feature::[15] Fail all quantifier on type mismatch between list elements and predicate::2",
    "expressions/quantifier/Quantifier4.feature::[15] Fail all quantifier on type mismatch between list elements and predicate::3",
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
        assert_eq!(TYPE_ERROR_AT_EVALUATION.len(), 131);
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
