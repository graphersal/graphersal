//! The `P`/`TextP` predicate tokens (`ArgP`, `P.eq(..)`/`P::eq(..)`/`eq(..)`, ...).

use crate::script::*;

/// Scope marker that makes the vanilla dotted spelling `P.eq(...)`/`P.within(...)`/... resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScriptP;

/// Scope marker for the text-predicate spelling `TextP.startingWith(...)`/....
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScriptTextP;

/// `TextP.regex(pattern)` / `TextP.notRegex(pattern)`: compiles the expression once, here; an
/// invalid expression is a script error carrying the `ValueError::InvalidRegex` diagnostic.
pub(crate) fn script_text_regex(
    pattern: &str,
    negated: bool,
) -> Result<ArgP, Box<rhai::EvalAltResult>> {
    let regex = crate::value::TextRegex::new(pattern).map_err(script_error)?;
    Ok(if negated {
        ArgP::NotRegex(regex)
    } else {
        ArgP::Regex(regex)
    })
}

pub(crate) fn script_p_within(val: Dynamic) -> ArgP {
    if val.is::<ScriptP>() {
        ArgP::WithIn(Vec::new())
    } else if val.is_array() {
        ArgP::WithIn(val.cast::<rhai::Array>())
    } else {
        ArgP::WithIn(vec![val])
    }
}

pub(crate) fn script_p_without(val: Dynamic) -> ArgP {
    if val.is::<ScriptP>() {
        ArgP::WithOut(Vec::new())
    } else if val.is_array() {
        ArgP::WithOut(val.cast::<rhai::Array>())
    } else {
        ArgP::WithOut(vec![val])
    }
}

/// The predicate as the DSL writes it (`P.gt(1)`, `TextP.containing("a")`,
/// `P.gt(1).and(P.lt(3))`): the engine predicate's canonical rendering, the same text the
/// predicate has inside a step of `.profile()`.
impl crate::value::literal::TokenLiteral for ArgP {
    fn token_literal(&self) -> String {
        crate::value::literal::TokenLiteral::token_literal(&argp_to_p(self))
    }
}

/// A script-level predicate with Rhai operands, before conversion to `P`. Its script type name
/// is `Predicate` (`type_of(P.eq(1))`, diagnostics, function metadata).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ArgP {
    /// Equal to.
    Eq(Dynamic),
    /// Not equal to.
    Neq(Dynamic),
    /// Less than.
    Lt(Dynamic),
    /// Greater than.
    Gt(Dynamic),
    /// Less than or equal to.
    Lte(Dynamic),
    /// Greater than or equal to.
    Gte(Dynamic),
    /// Equal to one of the given values.
    WithIn(Vec<Dynamic>),
    /// Equal to none of the given values.
    WithOut(Vec<Dynamic>),
    /// Between two bounds, exclusive.
    Inside(Dynamic, Dynamic),
    /// Outside two bounds, exclusive.
    Outside(Dynamic, Dynamic),
    /// Between two bounds, inclusive of the lower bound.
    Between(Dynamic, Dynamic),
    /// A string starting with the given prefix.
    StartingWith(Dynamic),
    /// A string not starting with the given prefix.
    NotStartingWith(Dynamic),
    /// A string ending with the given suffix.
    EndingWith(Dynamic),
    /// A string not ending with the given suffix.
    NotEndingWith(Dynamic),
    /// A string containing the given substring.
    Containing(Dynamic),
    /// A string not containing the given substring.
    NotContaining(Dynamic),
    /// A string the regular expression matches somewhere (`TextP.regex`).
    Regex(crate::value::TextRegex),
    /// A string the regular expression matches nowhere (`TextP.notRegex`).
    NotRegex(crate::value::TextRegex),
    /// Tests the type a value has at runtime.
    TypeOf(GType),
    /// Both predicates hold (`P.gt(1).and(P.lt(5))`), evaluated left to right.
    And(Box<ArgP>, Box<ArgP>),
    /// At least one predicate holds (`P.lt(1).or(P.gt(5))`), evaluated left to right.
    Or(Box<ArgP>, Box<ArgP>),
    /// The predicate does not hold (`P.not(P.within(1, 2))`, `p.negate()`).
    Not(Box<ArgP>),
}

#[export_module]
#[allow(non_snake_case, non_upper_case_globals)]
pub(crate) mod ArgPModule {
    use super::{ArgP, GType, script_p_within, script_p_without};
    use crate::value::literal::TokenLiteral;

    pub fn eq(val: Dynamic) -> ArgP {
        ArgP::Eq(val)
    }
    pub fn neq(val: Dynamic) -> ArgP {
        ArgP::Neq(val)
    }
    pub fn lt(val: Dynamic) -> ArgP {
        ArgP::Lt(val)
    }
    pub fn gt(val: Dynamic) -> ArgP {
        ArgP::Gt(val)
    }
    pub fn lte(val: Dynamic) -> ArgP {
        ArgP::Lte(val)
    }
    pub fn gte(val: Dynamic) -> ArgP {
        ArgP::Gte(val)
    }
    pub fn within(val: Dynamic) -> ArgP {
        script_p_within(val)
    }
    pub fn without(val: Dynamic) -> ArgP {
        script_p_without(val)
    }
    pub fn inside(val: Dynamic, val1: Dynamic) -> ArgP {
        ArgP::Inside(val, val1)
    }
    pub fn outside(val: Dynamic, val1: Dynamic) -> ArgP {
        ArgP::Outside(val, val1)
    }
    pub fn between(val: Dynamic, val1: Dynamic) -> ArgP {
        ArgP::Between(val, val1)
    }
    pub fn startingWith(val: Dynamic) -> ArgP {
        ArgP::StartingWith(val)
    }
    pub fn starting_with(val: Dynamic) -> ArgP {
        ArgP::StartingWith(val)
    }
    pub fn notStartingWith(val: Dynamic) -> ArgP {
        ArgP::NotStartingWith(val)
    }
    pub fn not_starting_with(val: Dynamic) -> ArgP {
        ArgP::NotStartingWith(val)
    }
    pub fn endingWith(val: Dynamic) -> ArgP {
        ArgP::EndingWith(val)
    }
    pub fn ending_with(val: Dynamic) -> ArgP {
        ArgP::EndingWith(val)
    }
    pub fn notEndingWith(val: Dynamic) -> ArgP {
        ArgP::NotEndingWith(val)
    }
    pub fn not_ending_with(val: Dynamic) -> ArgP {
        ArgP::NotEndingWith(val)
    }
    pub fn containing(val: Dynamic) -> ArgP {
        ArgP::Containing(val)
    }
    pub fn notContaining(val: Dynamic) -> ArgP {
        ArgP::NotContaining(val)
    }
    pub fn not_containing(val: Dynamic) -> ArgP {
        ArgP::NotContaining(val)
    }

    pub fn typeOf(val: GType) -> ArgP {
        ArgP::TypeOf(val)
    }

    /// Return the current variant of `ArgP`.
    #[rhai_fn(global, get = "enum_type", pure)]
    pub fn get_type(arg_p: &mut ArgP) -> String {
        match arg_p {
            ArgP::Eq(_) => "Eq",
            ArgP::Neq(_) => "Neq",
            ArgP::Lt(_) => "Lt",
            ArgP::Gt(_) => "Gt",
            ArgP::Lte(_) => "Lte",
            ArgP::Gte(_) => "Gte",
            ArgP::WithIn(_) => "WithIn",
            ArgP::WithOut(_) => "WithOut",
            ArgP::Inside(_, _) => "Inside",
            ArgP::Outside(_, _) => "Outside",
            ArgP::Between(_, _) => "Between",
            ArgP::StartingWith(_) => "StartingWith",
            ArgP::NotStartingWith(_) => "NotStartingWith",
            ArgP::EndingWith(_) => "EndingWith",
            ArgP::NotEndingWith(_) => "NotEndingWith",
            ArgP::Containing(_) => "Containing",
            ArgP::NotContaining(_) => "NotContaining",
            ArgP::Regex(_) => "Regex",
            ArgP::NotRegex(_) => "NotRegex",
            ArgP::TypeOf(_) => "TypeOf",
            ArgP::And(_, _) => "And",
            ArgP::Or(_, _) => "Or",
            ArgP::Not(_) => "Not",
        }
        .to_string()
    }

    /// Return the inner value.
    #[rhai_fn(global, get = "value", pure)]
    pub fn get_value(arg_p: &mut ArgP) -> Dynamic {
        match arg_p {
            ArgP::Eq(val)
            | ArgP::Neq(val)
            | ArgP::Lt(val)
            | ArgP::Gt(val)
            | ArgP::Lte(val)
            | ArgP::Gte(val)
            | ArgP::StartingWith(val)
            | ArgP::NotStartingWith(val)
            | ArgP::EndingWith(val)
            | ArgP::NotEndingWith(val)
            | ArgP::Containing(val)
            | ArgP::NotContaining(val) => val.clone(),

            ArgP::Inside(a_l, a_r) | ArgP::Outside(a_l, a_r) | ArgP::Between(a_l, a_r) => {
                Dynamic::from((a_l.clone(), a_r.clone()))
            }

            ArgP::WithIn(val) | ArgP::WithOut(val) => Dynamic::from_array(val.clone()),

            ArgP::TypeOf(target) => Dynamic::from(target.clone()),

            ArgP::Regex(regex) | ArgP::NotRegex(regex) => Dynamic::from(regex.as_str().to_string()),

            ArgP::And(a, b) | ArgP::Or(a, b) => {
                Dynamic::from((Dynamic::from((**a).clone()), Dynamic::from((**b).clone())))
            }

            ArgP::Not(inner) => Dynamic::from((**inner).clone()),
        }
    }

    // The `///` text is the DSL doc (`script/register/docs_tests.rs` checks it against `docs.rs`).

    /// The complement of the predicate (see the book page Predicates for the deviation from TinkerPop).
    ///
    /// Example: `g.v().has("age", P.gt(30).negate()).values("name").to_list()` returns `["marko", "vadas"]`
    #[rhai_fn(global, pure)]
    pub fn negate(arg_p: &mut ArgP) -> ArgP {
        ArgP::Not(Box::new(arg_p.clone()))
    }

    /// Printing
    #[rhai_fn(global, name = "to_string", name = "to_debug", pure)]
    pub fn to_string(arg_p: &mut ArgP) -> String {
        arg_p.token_literal()
    }
}

/// The PascalCase twins of `ArgPModule` (`P::Eq`-style), registered only in the
/// static module, never as bare globals.
#[export_module]
#[allow(non_snake_case)]
pub(crate) mod ArgPPascalModule {
    use super::{ArgP, GType, script_p_within, script_p_without};

    pub fn Eq(val: Dynamic) -> ArgP {
        ArgP::Eq(val)
    }
    pub fn Neq(val: Dynamic) -> ArgP {
        ArgP::Neq(val)
    }
    pub fn Lt(val: Dynamic) -> ArgP {
        ArgP::Lt(val)
    }
    pub fn Gt(val: Dynamic) -> ArgP {
        ArgP::Gt(val)
    }
    pub fn Lte(val: Dynamic) -> ArgP {
        ArgP::Lte(val)
    }
    pub fn Gte(val: Dynamic) -> ArgP {
        ArgP::Gte(val)
    }
    pub fn WithIn(val: Dynamic) -> ArgP {
        script_p_within(val)
    }
    pub fn WithOut(val: Dynamic) -> ArgP {
        script_p_without(val)
    }
    pub fn Inside(val: Dynamic, val1: Dynamic) -> ArgP {
        ArgP::Inside(val, val1)
    }
    pub fn Outside(val: Dynamic, val1: Dynamic) -> ArgP {
        ArgP::Outside(val, val1)
    }
    pub fn Between(val: Dynamic, val1: Dynamic) -> ArgP {
        ArgP::Between(val, val1)
    }
    pub fn StartingWith(val: Dynamic) -> ArgP {
        ArgP::StartingWith(val)
    }
    pub fn NotStartingWith(val: Dynamic) -> ArgP {
        ArgP::NotStartingWith(val)
    }
    pub fn EndingWith(val: Dynamic) -> ArgP {
        ArgP::EndingWith(val)
    }
    pub fn NotEndingWith(val: Dynamic) -> ArgP {
        ArgP::NotEndingWith(val)
    }
    pub fn Containing(val: Dynamic) -> ArgP {
        ArgP::Containing(val)
    }
    pub fn NotContaining(val: Dynamic) -> ArgP {
        ArgP::NotContaining(val)
    }
    pub fn TypeOf(val: GType) -> ArgP {
        ArgP::TypeOf(val)
    }
}

#[export_module]
#[allow(non_snake_case, non_upper_case_globals)]
pub(crate) mod TextPModule {
    use super::{ArgP, script_text_regex};

    #[rhai_fn(return_raw)]
    pub fn regex(pattern: &str) -> Result<ArgP, Box<rhai::EvalAltResult>> {
        script_text_regex(pattern, false)
    }
    #[rhai_fn(return_raw)]
    pub fn notRegex(pattern: &str) -> Result<ArgP, Box<rhai::EvalAltResult>> {
        script_text_regex(pattern, true)
    }
    #[rhai_fn(return_raw)]
    pub fn not_regex(pattern: &str) -> Result<ArgP, Box<rhai::EvalAltResult>> {
        script_text_regex(pattern, true)
    }

    pub fn startingWith(val: Dynamic) -> ArgP {
        ArgP::StartingWith(val)
    }
    pub fn starting_with(val: Dynamic) -> ArgP {
        ArgP::StartingWith(val)
    }
    pub fn notStartingWith(val: Dynamic) -> ArgP {
        ArgP::NotStartingWith(val)
    }
    pub fn not_starting_with(val: Dynamic) -> ArgP {
        ArgP::NotStartingWith(val)
    }
    pub fn endingWith(val: Dynamic) -> ArgP {
        ArgP::EndingWith(val)
    }
    pub fn ending_with(val: Dynamic) -> ArgP {
        ArgP::EndingWith(val)
    }
    pub fn notEndingWith(val: Dynamic) -> ArgP {
        ArgP::NotEndingWith(val)
    }
    pub fn not_ending_with(val: Dynamic) -> ArgP {
        ArgP::NotEndingWith(val)
    }
    pub fn containing(val: Dynamic) -> ArgP {
        ArgP::Containing(val)
    }
    pub fn notContaining(val: Dynamic) -> ArgP {
        ArgP::NotContaining(val)
    }
    pub fn not_containing(val: Dynamic) -> ArgP {
        ArgP::NotContaining(val)
    }
}

/// The PascalCase twins of `TextPModule` (`TextP::Eq`-style), registered only in the
/// static module, never as bare globals.
#[export_module]
#[allow(non_snake_case)]
pub(crate) mod TextPPascalModule {
    use super::ArgP;

    pub fn StartingWith(val: Dynamic) -> ArgP {
        ArgP::StartingWith(val)
    }
    pub fn NotStartingWith(val: Dynamic) -> ArgP {
        ArgP::NotStartingWith(val)
    }
    pub fn EndingWith(val: Dynamic) -> ArgP {
        ArgP::EndingWith(val)
    }
    pub fn NotEndingWith(val: Dynamic) -> ArgP {
        ArgP::NotEndingWith(val)
    }
    pub fn Containing(val: Dynamic) -> ArgP {
        ArgP::Containing(val)
    }
    pub fn NotContaining(val: Dynamic) -> ArgP {
        ArgP::NotContaining(val)
    }
}
