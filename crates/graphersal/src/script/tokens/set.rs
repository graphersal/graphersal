//! The `Set.of(..)` side-effect seed literal (`ScriptSet`).

use crate::script::*;

/// A set literal for `withSideEffect(name, Set.of(..))`: the values, duplicates collapsed when the
/// side effect is created. Only a side-effect seed: a set read back from a traversal is a plain
/// array (`cap()` of a set side effect yields an array without duplicates).
///
/// Deviations from TinkerPop (book page "Set Side Effects"): there is no set *value* (`toSet()`,
/// `GType.SET` and `{..}` literals outside `withSideEffect` do not exist), members keep
/// first-seen order, and `Set.of(..)` accepts plain values only.
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptSet(pub(crate) Vec<ElementProperty>);

/// Converts the members of a `Set.of(..)` call; a value that is not a plain value is an error.
pub(crate) fn script_set_from(items: Vec<Dynamic>) -> Result<ScriptSet, Box<rhai::EvalAltResult>> {
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let plain = item.is_string()
            || item.is_float()
            || item.is_int()
            || item.is_bool()
            || item.is_unit()
            || item.is::<ScriptUuid>();
        if !plain {
            return Err(Box::new(rhai::EvalAltResult::ErrorRuntime(
                Dynamic::from(crate::error::format_diagnostic(
                    &crate::error::ScriptError::ArgumentMismatch {
                        step_name: "Set.of".to_string(),
                        expected: "plain values (strings, numbers, booleans, UUIDs, null)"
                            .to_string(),
                        actual: crate::script::dynamic::type_name_of(&item),
                    },
                )),
                rhai::Position::NONE,
            )));
        }
        // A plain value has no nesting: the depth bound cannot be reached.
        out.push(dynamic_to_element_property(
            "Set.of",
            item,
            crate::value::depth::DEFAULT_MAX_VALUE_DEPTH,
        )?);
    }
    Ok(ScriptSet(out))
}

/// Scope marker that makes the Java-style spelling `Set.of(..)` / `Set.ofList([..])` resolve;
/// `Set::of(..)` / `Set::of_list([..])` come from a static module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScriptSetTokens;
