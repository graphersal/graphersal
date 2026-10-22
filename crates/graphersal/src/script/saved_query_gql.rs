//! Saved queries written in GQL (feature `gql`; book `users_guide/saved_queries.md`, "Saved
//! queries in GQL"): `g.define_query(#{name, language: "gql", body: "<GQL text>", params})`.
//!
//! The body is one GQL query, stored as it is written, in the dialect
//! [`GQL_QUERY_DIALECT`](crate::graph::definition::GQL_QUERY_DIALECT) when its plan writes
//! nothing and [`GQL_WRITING_QUERY_DIALECT`](crate::graph::definition::GQL_WRITING_QUERY_DIALECT)
//! otherwise. Every save and every first call of a definition on a thread checks it here:
//! the query parses, binds and lowers (on an empty scratch graph with placeholder parameter
//! values, [`crate::gql::lower::saved_query_plan`]), every `$name` it reads is declared in
//! `params` and every declared parameter is read. Its effects are the data writes of the plan,
//! exact per label, so the dialect, the `writes` / `effects` of `g.queries()` and the call's
//! authorization come from the same mapping the authorization check of the traversal uses.
//! A body that calls saved queries (`CALL folder.name(..)`, stage 3h) lists them as its calls,
//! so their effects are added transitively like those of a Rhai body's `g.query(..)`; when the
//! query is saved its calls are checked against the graph's catalog too (3h.P0 D12).
//!
//! A call runs no Rhai: `saved_query.rs` builds the traversal `g.gql(body, params)` on the
//! caller's graph under the call's access (read-only, or the query's effects inside one unit)
//! and returns it like `g.gql()` returns it, so Gremlin steps may continue it.

use crate::error::{ScriptError, TraverserError};
use crate::gql::procedure::Procedures;
use crate::graph::definition::{DefinitionKind, QueryDefinition};
use crate::graph::{AccessRequestBuf, AnyGraph};

/// A checked GQL body.
pub(super) struct GqlBody {
    /// The parameters the query reads, each once, in the order of their first `$name`.
    pub(super) parameters: Vec<String>,
    /// The data writes of its plan (empty for a query that only reads), without those of the
    /// saved queries it calls.
    pub(super) writes: Vec<AccessRequestBuf>,
    /// The saved queries it calls (`CALL folder.name(..)`, stage 3h), each once: their effects
    /// are its effects too ([`super::effects::transitive`]).
    pub(super) calls: Vec<String>,
}

/// The catalog of a script's graph, where a GQL body's `CALL`s are looked up (stage 3h).
pub(super) struct GraphCatalog<'a>(pub(super) &'a dyn AnyGraph);

impl Procedures for GraphCatalog<'_> {
    fn saved_query(&self, name: &str) -> Option<QueryDefinition> {
        self.0
            .definition(DefinitionKind::QUERY, name)
            .and_then(|d| d.as_query().cloned())
    }
}

/// Why a GQL body cannot be a saved query.
pub(super) enum GqlBodyError {
    /// The query does not parse, bind or lower (the GQL error with its GQLSTATUS and caret).
    Gql(Box<TraverserError>),
    /// Its parameters do not match the declared ones.
    Parameters(ScriptError),
}

impl GqlBodyError {
    /// The reason as one line: the `status: "error"` text of `g.queries()` and the reason of
    /// [`ScriptError::InvalidQueryBody`] at a call.
    pub(super) fn reason(&self) -> String {
        match self {
            GqlBodyError::Gql(error) => format!("it is not a valid GQL query: {error}"),
            GqlBodyError::Parameters(ScriptError::UndeclaredQueryParameter {
                parameter, ..
            }) => format!("it reads the parameter ${parameter}, which its params do not declare"),
            GqlBodyError::Parameters(ScriptError::UnusedQueryParameter { parameter, .. }) => {
                format!("it declares the parameter '{parameter}', which its GQL query never reads")
            }
            GqlBodyError::Parameters(other) => other.to_string(),
        }
    }
}

/// Checks the GQL body `source` of the saved query `name` whose declared parameters are
/// `declared` (module docs); with `catalog` (when it is saved) its calls of saved queries are
/// checked against it too.
pub(super) fn check(
    name: &str,
    source: &str,
    declared: &[&str],
    catalog: Option<&dyn Procedures>,
) -> Result<GqlBody, GqlBodyError> {
    let plan = crate::gql::lower::saved_query_plan(source, catalog)
        .map_err(|error| GqlBodyError::Gql(Box::new(error)))?;
    let (parameters, writes, calls) = (plan.parameters, plan.writes, plan.calls);
    if let Some(parameter) = parameters.iter().find(|p| !declared.contains(&p.as_str())) {
        return Err(GqlBodyError::Parameters(
            ScriptError::UndeclaredQueryParameter {
                query: name.to_string(),
                parameter: parameter.clone(),
                declared: declared.iter().map(ToString::to_string).collect(),
            },
        ));
    }
    if let Some(parameter) = declared
        .iter()
        .find(|d| !parameters.iter().any(|p| p == *d))
    {
        return Err(GqlBodyError::Parameters(
            ScriptError::UnusedQueryParameter {
                query: name.to_string(),
                parameter: parameter.to_string(),
            },
        ));
    }
    Ok(GqlBody {
        parameters,
        writes,
        calls,
    })
}
