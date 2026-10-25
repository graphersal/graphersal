//! The Rhai engine: `engine()`/`engine_with_limits()` (the shared DSL registrations of
//! `register/`, then the per-engine limits, expression depths and keyword token rewriting) and
//! `registered_step_names()`.

use super::*;

/// Maximum expression nesting depth of a script (Rhai counts every call of a method chain and every
/// nested call). 128 is twice Rhai's release default and four times its debug default, enough for
/// the longest TinkerPop feature queries. It is not higher because the parser recurses: on a 2 MiB
/// thread stack (the default of spawned threads) a debug build overflowed the stack at 160/80,
/// while 128/64 rejects a too-long chain cleanly. Identical in every build profile.
const MAX_EXPR_DEPTH: usize = 128;

/// Maximum expression depth inside a script-defined function body; half of [`MAX_EXPR_DEPTH`].
const MAX_FUNCTION_EXPR_DEPTH: usize = 64;

/// Builds the Rhai engine with every DSL type/step registered. Shared by [`eval_value`] and its
/// parameterized variants. Cheap (microseconds): the registrations and Rhai's standard library
/// are built once per thread and attached to each new engine by pointer; only the first engine
/// of a thread pays for building them (milliseconds).
///
/// The engine carries the SAFE [`ScriptLimits::default`] limits (operations, string, array, map
/// size, recursion depth), so a script cannot hang or exhaust the process, and allows
/// everything ([`AllowAll`](crate::graph::AllowAll)): `import` loads modules and the graphs a
/// script builds itself can be read and written. Use [`engine_with_limits`] to choose both, for
/// example an [`AccessPolicy`](crate::graph::AccessPolicy) for scripts the host did not write.
pub fn engine() -> Engine {
    engine_with_limits(&ScriptLimits::default(), Arc::new(crate::graph::AllowAll))
}

/// [`engine`] with explicit [`ScriptLimits`] and [`Authorizer`](crate::graph::Authorizer).
/// Every limit hit surfaces as an
/// `EvalAltResult` that [`resource_limit_from_rhai`] maps to
/// [`TraverserError::ResourceLimitExceeded`](crate::TraverserError::ResourceLimitExceeded).
///
/// The authorizer governs what the engine itself provides: Rhai's `import "module"` (`Execute`
/// on the module's `File`, resolved by [`Authorizer::resolve_file`](crate::graph::Authorizer::resolve_file),
/// e.g. under a file root) and the traversal sources a script creates (`GraphSource::empty()`,
/// `GraphSource::file(path)`, `schema.to_graph()`). The session's own `g` gets its authorizer
/// from [`graph_scope_with_options`]: pass the same one to both.
pub fn engine_with_limits(
    limits: &ScriptLimits,
    authorizer: Arc<dyn crate::graph::Authorizer>,
) -> Engine {
    engine_with_options(limits, authorizer, &[])
}

/// [`engine_with_limits`] with the host's preset traversal `options` for the traversal sources
/// the engine itself provides: `GraphSource::empty()`, `GraphSource::tinkerpop_modern()`,
/// `GraphSource::file(path)`, `schema.to_graph()`, `subgraph(..)` results. Pass the same options
/// as to [`graph_scope_with_options`] (which covers `g`, `_g` and `__`), so a script cannot escape
/// a preset such as `evaluationTimeout` by creating a source of its own:
///
/// ```rust
/// # use std::sync::Arc;
/// # use graphersal::prelude::*;
/// # use graphersal::auth::{AllowAll, Authorizer};
/// let authorizer: Arc<dyn Authorizer> = Arc::new(AllowAll);
/// let options = [("evaluationTimeout", ElementProperty::from(1))];
/// let engine =
///     graphersal::script::engine_with_options(&Default::default(), authorizer.clone(), &options);
/// let mut scope = graphersal::script::graph_scope_with_options(
///     Arc::new(GraphSource::tinkerpop_modern()),
///     &options,
///     authorizer,
/// );
/// let timed_out = engine.eval_with_scope::<graphersal::script::rhai::Dynamic>(
///     &mut scope,
///     "GraphSource::empty().inject(1).repeat(__.group()).times(60).count().next()",
/// );
/// assert!(timed_out.unwrap_err().to_string().contains("evaluationTimeout"));
/// ```
pub fn engine_with_options(
    limits: &ScriptLimits,
    authorizer: Arc<dyn crate::graph::Authorizer>,
    options: &[(&str, ElementProperty)],
) -> Engine {
    let mut engine = register::new_engine();
    access::install(
        &mut engine,
        authorizer,
        options,
        limits.effective_max_value_depth(),
    );
    limits.apply(&mut engine);
    // Rhai's default depth is 64 in release but 32 in debug builds; fix it so a query compiles
    // the same in every profile. See `MAX_EXPR_DEPTH`.
    engine.set_max_expr_depths(MAX_EXPR_DEPTH, MAX_FUNCTION_EXPR_DEPTH);

    // `in`, `as` and `until` are Gremlin step names as well as Rhai keywords. They are only
    // rewritten into identifiers in method position (directly after a `.`), so that
    // `for item in list`, `import "m" as m` and `do { .. } until x` keep their language meaning
    // while `.in("knows")`, `.as("a")` and `.until(__.has(..))` stay available as steps.
    let after_period = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    #[allow(deprecated)]
    engine.on_parse_token(move |token, _pos, _state| {
        let is_method_position = after_period.swap(
            matches!(token, Token::Period),
            std::sync::atomic::Ordering::Relaxed,
        );

        match token {
            Token::In if is_method_position => Token::Identifier(Box::new("in".into())),
            Token::As if is_method_position => Token::Identifier(Box::new("as".into())),
            Token::Until if is_method_position => Token::Identifier(Box::new("until".into())),
            // `default` is a word Rhai reserves for the future; it is the key of a saved-query
            // parameter default (`#{schema: .., default: 20}`), so it is an identifier here.
            Token::Reserved(s) => match s.as_str() {
                "is" | "where" | "with" | "__" | "default" => Token::Identifier(s),
                _ => Token::Reserved(s),
            },
            Token::LexError(err) => {
                if let LexError::MalformedIdentifier(ref s) = *err
                    && s == "__"
                {
                    return Token::Identifier(Box::new(s.as_str().into()));
                }
                Token::LexError(err)
            }
            _ => token,
        }
    });

    engine
}

/// Every step/method name registered with the Rhai engine, sorted and deduplicated. Used to keep
/// documentation/tests in sync with the DSL's actual registered surface.
pub fn registered_step_names() -> Vec<String> {
    let engine = engine();
    let mut names = std::collections::BTreeSet::new();

    for sig in engine.gen_fn_signatures(false) {
        if let Some(pos) = sig.find('(') {
            let fn_name = sig[..pos].trim();
            // Filter out operators and internal functions
            if !fn_name.is_empty()
                && !fn_name.contains(' ')
                && fn_name.chars().all(|c| c.is_alphanumeric() || c == '_')
            {
                names.insert(fn_name.to_string());
            }
        }
    }

    names.into_iter().collect()
}
