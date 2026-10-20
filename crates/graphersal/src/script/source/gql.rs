//! The GQL entry `g.gql(query[, #{params}])` (feature `gql`): records the query as
//! [`Step::Gql`]; `convert.rs` lowers it through [`crate::GraphTraversalSource::gql_with`].
//!
//! `gql()` starts the root traversal only. Called on `__` or after other steps it fails at the
//! call with the GQL error of the Rust entry (kind `NotYet`), so the script stops at the line
//! that wrote it; source steps (`with_sack`, `with_side_effect`, ...) may come before it.

use crate::error::{GqlErrorBuilder, GqlErrorKind, ScriptError, TraverserError};
use crate::script::*;

impl ScriptGraphTraversalSource {
    /// Runs a GQL read query (`MATCH ... RETURN ...`) at the start of a traversal; every result
    /// is one map per row (column name -> value), see [`crate::GraphTraversalSource::gql`]. Rhai
    /// binding of `gql()`, the overload without parameters.
    pub fn gql(&mut self, query: String) -> Result<Self, Box<rhai::EvalAltResult>> {
        self.gql_step(query, Vec::new())
    }

    /// Runs a GQL read query whose `$name` references read the values of `params`
    /// (`#{name: value, ..}`; `()` for none). Rhai binding of `gql()`, the overload with
    /// parameters.
    pub fn gql_with(
        ctx: NativeCallContext,
        this: &mut Self,
        query: String,
        params: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let what = "the query parameters as a map, for example #{name: \"marko\"}";
        let params =
            crate::script::saved_query::optional_map("gql", what, params)?.unwrap_or_default();
        let max = access::value_depth_of(&ctx);
        let mut values = Vec::with_capacity(params.len());
        for (name, value) in params {
            if value.is::<ScriptGraphTraversalSource>() || value.is::<rhai::FnPtr>() {
                return Err(crate::script::saved_query::fail(
                    ScriptError::ArgumentMismatch {
                        step_name: "gql".to_string(),
                        expected: format!("a value for the parameter `{name}` (not a traversal)"),
                        actual: type_name_of(&value),
                    },
                ));
            }
            let value = dynamic_to_element_property(value, max)?;
            values.push((CompactString::new(name.as_str()), value));
        }
        // A GQL write's creating steps carry the evaluation's `now()` (like `addV`).
        this.gql_step(query, values)
            .map(|source| source.stamped(&ctx))
    }

    fn gql_step(
        &mut self,
        query: String,
        params: Vec<(CompactString, ElementProperty)>,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        if self.anonymous {
            return Err(gql_not_at_start(
                &query,
                "inside __ (an anonymous traversal)",
            ));
        }
        if self
            .steps
            .iter()
            .any(|step| !matches!(step, Step::Source(_)))
        {
            return Err(gql_not_at_start(&query, "after other steps"));
        }
        Ok(self.add_step(Step::Gql(CompactString::new(query), params)))
    }
}

/// The error of `gql()` not at the start of the root traversal (`position` says where it was).
pub(crate) fn gql_not_at_start_error(query: &str, position: &str) -> TraverserError {
    GqlErrorBuilder::new(
        GqlErrorKind::NotYet,
        0..query.len(),
        format!("gql() starts a traversal in this version; it cannot run {position}"),
    )
    .alternative(crate::gql::lower::start_with_gql(query))
    .into_error(query)
}

fn gql_not_at_start(query: &str, position: &str) -> Box<rhai::EvalAltResult> {
    script_error(Box::new(gql_not_at_start_error(query, position)))
}
