//! The `CastPolicy` token of the coercion steps (`CastPolicy.Skip`/`CastPolicy::Skip`).

use crate::script::*;

/// Scope marker that makes the vanilla dotted spelling `CastPolicy.Skip` resolve. See
/// [`ScriptBy`]'s doc comment for the shared rationale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScriptCastPolicy;

#[export_module]
#[allow(non_snake_case, non_upper_case_globals)]
pub(crate) mod CastPolicyModule {
    use super::{CastPolicy, value_arg};
    use crate::value::literal::TokenLiteral;

    /// Fail the traversal on a value that does not convert.
    pub const Error: CastPolicy = CastPolicy::Error;
    /// Drop a value that does not convert from the stream.
    pub const Skip: CastPolicy = CastPolicy::Skip;

    /// Replace a value that does not convert with a fallback.
    #[rhai_fn(return_raw)]
    pub fn Default(
        ctx: NativeCallContext,
        value: Dynamic,
    ) -> Result<CastPolicy, Box<rhai::EvalAltResult>> {
        Ok(CastPolicy::Default(value_arg(
            &ctx,
            "CastPolicy.Default",
            value,
        )?))
    }

    /// Printing
    #[rhai_fn(global, name = "to_string", name = "to_debug", pure)]
    pub fn to_string(policy: &mut CastPolicy) -> String {
        policy.token_literal()
    }
}
