//! The spelling of the canonical step rendering: snake_case (the default, the house style of the
//! Rust API, `help()` and error texts) or Gremlin camelCase.
//!
//! Every step renders ONE canonical text that reads like the DSL (`has_label("person")`,
//! `where(__.out().has("x"))`, `order().by("age", Order.desc)`), always in snake_case. The
//! camelCase spelling is derived from that text by a crate-internal respelling: every identifier
//! directly followed by `(` outside a string literal is renamed to its camelCase name. The pairs
//! come from the registered DSL surface (`registered_step_names()`, where rule 2 registers each
//! TinkerPop step once under both spellings) when the `script` feature is on, so no second list
//! of names exists. A name the DSL registers in snake_case only (Graphersal's own extensions such
//! as `remove_property`, rule 2) keeps it, so the camelCase text still runs; a name the DSL does
//! not register at all (an optimizer-inserted `lazy_barrier()`) is converted by the same
//! snake -> camel rule the registry follows.
//!
//! The spelling is chosen per execution with `g.with("render.spelling", "camel")` (values
//! `"snake"`, `"camel"`); front ends set it as a session option (`--spelling camel`,
//! `/set spelling camel`). Only diagnostic text is affected: step names of `.profile()`, error
//! step locations and the `help()` rewrites of the failing query. Data output never is.

use std::borrow::Cow;

/// The spelling of rendered step names in diagnostics. See the module documentation.
///
/// `#[non_exhaustive]`: another spelling may be added later.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
#[non_exhaustive]
pub enum Spelling {
    /// snake_case step names (`has_label`, `group_count`), the default.
    #[default]
    Snake,
    /// Gremlin camelCase step names (`hasLabel`, `groupCount`).
    Camel,
}

impl Spelling {
    /// The option value naming this spelling: `"snake"` or `"camel"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Spelling::Snake => "snake",
            Spelling::Camel => "camel",
        }
    }

    /// Parses an option value (`"snake"`, `"camel"`, case-insensitive).
    pub fn parse(value: &str) -> Option<Self> {
        if value.eq_ignore_ascii_case("snake") {
            Some(Spelling::Snake)
        } else if value.eq_ignore_ascii_case("camel") {
            Some(Spelling::Camel)
        } else {
            None
        }
    }
}

impl std::fmt::Display for Spelling {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `has_label` -> `hasLabel`: the snake -> camel rule every registered name pair follows (the
/// script surface test checks it).
fn camel_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut upper = false;
    for c in name.chars() {
        if c == '_' {
            upper = true;
        } else if upper {
            out.push(c.to_ascii_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// The snake_case step names of the registered DSL surface, each with its camelCase twin when
/// that is registered too (`None`: registered in snake_case only). Built once, on the first
/// camelCase rendering.
#[cfg(feature = "script")]
fn registered_pairs() -> &'static std::collections::HashMap<String, Option<String>> {
    static PAIRS: std::sync::OnceLock<std::collections::HashMap<String, Option<String>>> =
        std::sync::OnceLock::new();
    PAIRS.get_or_init(|| {
        let names: std::collections::HashSet<String> =
            crate::script::registered_step_names().into_iter().collect();
        names
            .iter()
            .filter(|name| is_snake_step_name(name))
            .map(|name| {
                let camel = camel_case(name);
                (name.clone(), names.contains(&camel).then_some(camel))
            })
            .collect()
    })
}

/// A lowercase identifier with an inner underscore (`has_label`, not `__`, `_g` or `in_`).
fn is_snake_step_name(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_lowercase())
        && name.contains('_')
        && !name.ends_with('_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The camelCase spelling of the snake_case step name `snake`: the registered pair when the DSL
/// registers one, the name itself when the DSL registers it in snake_case only, otherwise the
/// same snake -> camel rule.
pub(crate) fn camel_name(snake: &str) -> Cow<'_, str> {
    if !is_snake_step_name(snake) {
        return Cow::Borrowed(snake);
    }
    #[cfg(feature = "script")]
    if let Some(pair) = registered_pairs().get(snake) {
        return match pair {
            Some(camel) => Cow::Owned(camel.clone()),
            None => Cow::Borrowed(snake),
        };
    }
    Cow::Owned(camel_case(snake))
}

/// `text` (canonical snake_case rendering, or prose quoting it) in `spelling`: with
/// [`Spelling::Camel`], every identifier directly followed by `(` outside a string literal is
/// renamed through [`camel_name`]. Idempotent; [`Spelling::Snake`] returns `text` unchanged.
pub(crate) fn respell(text: &str, spelling: Spelling) -> Cow<'_, str> {
    respell_keeping(text, spelling, |_| false)
}

/// [`respell`], except that a called identifier for which `keep` holds stays as written: the
/// help of a GQL error keeps the GQL function names (`element_id(p)`) and respells the Gremlin
/// steps.
pub(crate) fn respell_keeping(
    text: &str,
    spelling: Spelling,
    keep: impl Fn(&str) -> bool,
) -> Cow<'_, str> {
    if spelling == Spelling::Snake || !text.contains('_') {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    let mut escaped = false;
    let mut chars = text.char_indices().peekable();
    let mut prev: Option<char> = None;
    while let Some((start, c)) = chars.next() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            prev = Some(c);
            continue;
        }
        if c == '"' {
            in_string = true;
            out.push(c);
            prev = Some(c);
            continue;
        }
        let starts_identifier = (c.is_ascii_alphabetic() || c == '_')
            && !prev.is_some_and(|p| p.is_ascii_alphanumeric() || p == '_');
        if !starts_identifier {
            out.push(c);
            prev = Some(c);
            continue;
        }
        let mut end = start + c.len_utf8();
        while let Some(&(index, next)) = chars.peek() {
            if next.is_ascii_alphanumeric() || next == '_' {
                end = index + next.len_utf8();
                chars.next();
            } else {
                break;
            }
        }
        let identifier = &text[start..end];
        let called = chars.peek().is_some_and(|&(_, next)| next == '(');
        if called && !keep(identifier) {
            out.push_str(&camel_name(identifier));
        } else {
            out.push_str(identifier);
        }
        prev = identifier.chars().last();
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "script")]
    #[test]
    fn respell_renames_calls_outside_strings_only() {
        assert_eq!(
            respell(
                r#"v().has_label("has_label(").group_count().by(T.label).out_e()"#,
                Spelling::Camel
            ),
            r#"v().hasLabel("has_label(").groupCount().by(T.label).outE()"#
        );
        assert_eq!(
            respell(r#"values("a\"b_c(").to_list()"#, Spelling::Camel),
            r#"values("a\"b_c(").toList()"#
        );
        // keys of an object map literal and the math variable are no calls; a Graphersal
        // extension (snake_case only) keeps its name
        assert_eq!(
            respell(
                r#"glob_path("a", #{max_depth: 2}).math("_ + 1").to_list()"#,
                Spelling::Camel
            ),
            r#"glob_path("a", #{max_depth: 2}).math("_ + 1").toList()"#
        );
    }

    #[test]
    fn respell_keeping_leaves_the_kept_calls_alone() {
        assert_eq!(
            respell_keeping(
                "use element_id(p) or add_v(), char_length(x)",
                Spelling::Camel,
                |name| name == "element_id" || name == "char_length"
            ),
            "use element_id(p) or addV(), char_length(x)"
        );
        assert_eq!(
            respell_keeping("add_v()", Spelling::Snake, |_| false),
            "add_v()"
        );
    }

    #[test]
    fn respell_is_idempotent_and_snake_is_identity() {
        let once = respell("v().side_effect(__.drop()).lazy_barrier()", Spelling::Camel);
        assert_eq!(once, "v().sideEffect(__.drop()).lazyBarrier()");
        assert_eq!(respell(&once, Spelling::Camel), once);
        assert_eq!(
            respell("v().has_label(\"x\")", Spelling::Snake),
            "v().has_label(\"x\")"
        );
    }

    #[test]
    fn spelling_parses_option_values() {
        assert_eq!(Spelling::parse("camel"), Some(Spelling::Camel));
        assert_eq!(Spelling::parse("Snake"), Some(Spelling::Snake));
        assert_eq!(Spelling::parse("kebab"), None);
    }

    #[cfg(feature = "script")]
    #[test]
    fn camel_names_come_from_the_registry() {
        let pairs = registered_pairs();
        let camel = |snake: &str| pairs.get(snake).cloned().flatten();
        assert_eq!(camel("has_label").as_deref(), Some("hasLabel"));
        assert_eq!(camel("from_string").as_deref(), Some("fromString"));
        assert!(!pairs.contains_key("in_"));
        // A Graphersal extension is registered in snake_case only and keeps it.
        assert_eq!(pairs.get("remove_property"), Some(&None));
        assert_eq!(camel_name("remove_property"), "remove_property");
        assert_eq!(camel_name("lazy_barrier"), "lazyBarrier");
    }
}
