//! Rhai literal syntax for values inside the canonical rendering of a step (`.profile()` step
//! names, error locations, `help()` rewrites of the failing query).
//!
//! This is diagnostic text, never a data format: results, JSON and GraphML go through the
//! physical codec (`value/physical.rs`). It exists so that a rendered step reads like the DSL
//! call that builds it (`has("name", "josh")`, `inject(1, 2.5, ())`, `constant(#{a: [1]})`)
//! instead of a Rust `Debug` dump (rule 14).

use std::fmt::Write as _;

use crate::value::{ElementProperty, Number, PValue};

/// Characters of a string literal shown before it is cut (`"aaa…" (100000 chars)`): a rendered
/// step stays readable when an argument is a large value.
pub(crate) const MAX_LITERAL_CHARS: usize = 64;

/// Items of a list or map literal (and arguments of a variadic step) shown before it is cut
/// (`[1, 2, …] (10000 items)`).
pub(crate) const MAX_LITERAL_ITEMS: usize = 16;

/// The marker after a cut literal: ` (N chars)` / ` (N items)`.
fn cut_note(out: &mut String, total: usize, unit: &str) {
    let _ = write!(out, " ({total} {unit})");
}

/// A Rhai string literal: `"a\"b"`. Escapes the quote, the backslash and control characters,
/// so the literal parses back to the same string. A string longer than [`MAX_LITERAL_CHARS`]
/// is cut: `"aaaa…" (100000 chars)`.
pub(crate) fn rhai_string(text: &str) -> String {
    rhai_string_up_to(text, MAX_LITERAL_CHARS)
}

/// The most characters of a query a `help()` rewrite quotes whole as a runnable
/// `g.gql("..")` ([`rhai_query`]); a longer query gets a generic example instead.
#[cfg_attr(
    not(feature = "gql"),
    allow(dead_code, reason = "used by the gql() errors")
)]
pub(crate) const MAX_QUERY_EXAMPLE_CHARS: usize = 1000;

/// The Rhai string literal of a whole query (never cut, so the example runs), or `None` when
/// it is longer than [`MAX_QUERY_EXAMPLE_CHARS`].
#[cfg_attr(
    not(feature = "gql"),
    allow(dead_code, reason = "used by the gql() errors")
)]
pub(crate) fn rhai_query(text: &str) -> Option<String> {
    (text.chars().count() <= MAX_QUERY_EXAMPLE_CHARS)
        .then(|| rhai_string_up_to(text, MAX_QUERY_EXAMPLE_CHARS))
}

/// [`rhai_string`] cut after `max` characters.
fn rhai_string_up_to(text: &str, max: usize) -> String {
    let total = text.chars().count();
    let cut = total > max;
    let mut out = String::with_capacity(text.len().min(4 * max) + 2);
    out.push('"');
    for c in text.chars().take(max) {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    if cut {
        out.push('…');
    }
    out.push('"');
    if cut {
        cut_note(&mut out, total, "chars");
    }
    out
}

/// Comma-separated Rhai string literals: `"a", "b"`; cut after [`MAX_LITERAL_ITEMS`] items.
pub(crate) fn rhai_strings<T: AsRef<str>>(items: &[T]) -> String {
    join_cut(
        items.iter().map(|item| rhai_string(item.as_ref())),
        items.len(),
    )
}

/// `parts` joined with `, `; after [`MAX_LITERAL_ITEMS`] of `total` parts: `, …` and the count.
fn join_cut(parts: impl Iterator<Item = String>, total: usize) -> String {
    let mut out = parts.take(MAX_LITERAL_ITEMS).collect::<Vec<_>>().join(", ");
    if total > MAX_LITERAL_ITEMS {
        out.push_str(", …");
        cut_note(&mut out, total, "items");
    }
    out
}

/// A Rhai array literal of strings: `["a", "b"]`.
pub(crate) fn rhai_string_array<T: AsRef<str>>(items: &[T]) -> String {
    format!("[{}]", rhai_strings(items))
}

/// A Rhai number literal: an integer as is, a float always with a decimal point or an exponent
/// (`2.0`, `0.5`, `1e300`), so it reads back as a float.
pub(crate) fn rhai_number(number: &Number) -> String {
    match (number.as_i64(), number.as_f64()) {
        (Some(int), _) => int.to_string(),
        (None, Some(float)) => rhai_float(float),
        (None, None) => String::new(),
    }
}

fn rhai_float(float: f64) -> String {
    if !float.is_finite() {
        // No Rhai literal exists; the name is the clearest diagnostic text.
        return if float.is_nan() {
            "NaN".to_string()
        } else if float > 0.0 {
            "inf".to_string()
        } else {
            "-inf".to_string()
        };
    }
    let abs = float.abs();
    let text = if abs != 0.0 && !(1e-6..1e16).contains(&abs) {
        format!("{float:e}")
    } else {
        format!("{float}")
    };
    if text.contains(['.', 'e']) {
        text
    } else {
        format!("{text}.0")
    }
}

/// A Rhai object-map key: bare when it is an identifier, quoted otherwise.
pub(crate) fn rhai_key(key: &str) -> String {
    let mut chars = key.chars();
    let identifier = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if identifier {
        key.to_string()
    } else {
        rhai_string(key)
    }
}

/// The Rhai literal of a value: `()` for null, `true`, `1`, `2.5`, `"x"`,
/// `UUID.from_string("...")`, `[..]`, `#{k: v}`. A vertex or edge (a stream value, never a
/// literal) renders as `v[id]`/`e[id]`, the way the rest of the diagnostics name an element; a
/// map with non-string keys renders its keys as literals too (`#{29: 1}`), which reads clearly
/// although Rhai object maps only take string keys.
pub(crate) fn rhai_literal(value: &ElementProperty) -> String {
    let mut out = String::new();
    write_literal(&mut out, value);
    out
}

/// Comma-separated Rhai literals: `1, "a", ()`; cut after [`MAX_LITERAL_ITEMS`] values.
pub(crate) fn rhai_literals(values: &[ElementProperty]) -> String {
    join_cut(values.iter().map(rhai_literal), values.len())
}

fn write_literal(out: &mut String, value: &ElementProperty) {
    match value {
        ElementProperty::Null => out.push_str("()"),
        ElementProperty::Boolean(b) => out.push_str(if *b { "true" } else { "false" }),
        ElementProperty::Number(n) => out.push_str(&rhai_number(n)),
        ElementProperty::String(s) => out.push_str(&rhai_string(s)),
        ElementProperty::Compressed(text) => text.with_text(|s| out.push_str(&rhai_string(s))),
        ElementProperty::Deduped(text) => out.push_str(&rhai_string(text.as_str())),
        ElementProperty::Uuid(bits) => {
            let _ = write!(
                out,
                "UUID.from_string({})",
                rhai_string(&crate::value::uuid::format_canonical(*bits))
            );
        }
        ElementProperty::Temporal(value) => out.push_str(&rhai_temporal(value)),
        // `vector([0.1, 0.2], "i8")`, cut after the first components like a list.
        ElementProperty::Vector(value) => out.push_str(&value.literal(Some(MAX_LITERAL_ITEMS))),
        ElementProperty::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().take(MAX_LITERAL_ITEMS).enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_literal(out, item);
            }
            close_cut(out, ']', items.len());
        }
        ElementProperty::Object(map) => {
            out.push_str("#{");
            for (i, (key, item)) in map.iter().take(MAX_LITERAL_ITEMS).enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&rhai_key(key));
                out.push_str(": ");
                write_literal(out, item);
            }
            close_cut(out, '}', map.len());
        }
        ElementProperty::Map(map) => {
            out.push_str("#{");
            for (i, (key, item)) in map.iter().take(MAX_LITERAL_ITEMS).enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                match key {
                    ElementProperty::String(key) => out.push_str(&rhai_key(key)),
                    other => write_literal(out, other),
                }
                out.push_str(": ");
                write_literal(out, item);
            }
            close_cut(out, '}', map.len());
        }
        ElementProperty::Vertex(vertex) => {
            out.push_str("v[");
            out.push_str(vertex.id());
            out.push(']');
        }
        ElementProperty::Edge(edge) => {
            out.push_str("e[");
            out.push_str(edge.id());
            out.push(']');
        }
    }
}

/// Closes a container literal of `total` items, of which [`MAX_LITERAL_ITEMS`] were written.
fn close_cut(out: &mut String, close: char, total: usize) {
    if total > MAX_LITERAL_ITEMS {
        out.push_str(", …");
        out.push(close);
        cut_note(out, total, "items");
    } else {
        out.push(close);
    }
}

/// A property key as the DSL writes it: `"name"` or `jpath("$.a.b")`.
pub(crate) fn rhai_prop_key(key: &crate::value::PropKey) -> String {
    match key {
        crate::value::PropKey::Name(name) => rhai_string(name),
        crate::value::PropKey::Path(path) => rhai_jpath(path),
    }
}

/// A JSON path as the DSL writes it: `jpath("$.a.b")`.
pub(crate) fn rhai_jpath(path: &impl std::fmt::Display) -> String {
    format!("jpath({})", rhai_string(&path.to_string()))
}

/// The Rhai literal of a predicate operand.
pub(crate) fn rhai_pvalue(value: &PValue) -> String {
    match value {
        PValue::String(s) => rhai_string(s),
        PValue::Number(n) => rhai_number(n),
        PValue::Boolean(b) => b.to_string(),
        PValue::Uuid(bits) => format!(
            "UUID.from_string({})",
            rhai_string(&crate::value::uuid::format_canonical(*bits))
        ),
        PValue::Temporal(value) => rhai_temporal(value),
        PValue::Vector(value) => value.literal(Some(MAX_LITERAL_ITEMS)),
        PValue::Null => "()".to_string(),
    }
}

/// The Rhai literal of a temporal value: its constructor and canonical text
/// (`datetime("2023-08-02T00:00:00Z")`, `date("2023-08-02")`, `localdatetime(..)`, `time(..)`,
/// `duration("P1D")`).
pub(crate) fn rhai_temporal(value: &crate::value::temporal::Temporal) -> String {
    format!(
        "{}({})",
        value.kind().name(),
        rhai_string(&value.to_string())
    )
}

/// The canonical DSL text of a token value: the Rhai literal that evaluates to it (`Scope.local`,
/// `Operator.sum`, `GType.LONG`, `P.gt(1)`), in the canonical dotted spelling.
///
/// It is what a script sees for a token: the session visualizer's rendering of a final value
/// (also nested in a list or map), `to_string()`/`to_debug()` and string interpolation `${..}`
/// (`script/tokens/*` register it). Never `Debug`, never a Rust type path (rule 14).
#[cfg_attr(not(feature = "script"), allow(dead_code))] // only the DSL renders tokens
pub(crate) trait TokenLiteral {
    /// The token's DSL literal.
    fn token_literal(&self) -> String;
}

impl TokenLiteral for crate::value::Scope {
    fn token_literal(&self) -> String {
        format!("Scope.{self}")
    }
}

impl TokenLiteral for crate::value::Column {
    fn token_literal(&self) -> String {
        format!("Column.{self}")
    }
}

impl TokenLiteral for crate::value::GType {
    fn token_literal(&self) -> String {
        self.token()
    }
}

impl TokenLiteral for crate::traversal::Order {
    fn token_literal(&self) -> String {
        self.to_string()
    }
}

impl TokenLiteral for crate::value::Pop {
    fn token_literal(&self) -> String {
        format!("Pop.{}", self.name())
    }
}

impl TokenLiteral for crate::value::dt::DT {
    fn token_literal(&self) -> String {
        format!("DT.{}", self.name())
    }
}

impl TokenLiteral for crate::value::Operator {
    fn token_literal(&self) -> String {
        format!("Operator.{}", self.name())
    }
}

impl TokenLiteral for crate::value::Barrier {
    fn token_literal(&self) -> String {
        format!("Barrier.{self}")
    }
}

impl TokenLiteral for crate::value::WithOptions {
    fn token_literal(&self) -> String {
        format!("WithOptions.{}", self.name())
    }
}

impl TokenLiteral for crate::value::Direction {
    fn token_literal(&self) -> String {
        format!("Direction.{self}")
    }
}

impl TokenLiteral for crate::value::Merge {
    fn token_literal(&self) -> String {
        format!("Merge.{self}")
    }
}

impl TokenLiteral for crate::value::Cardinality {
    fn token_literal(&self) -> String {
        format!("Cardinality.{self}")
    }
}

impl TokenLiteral for crate::graph::schema::SchemaMode {
    fn token_literal(&self) -> String {
        format!("SchemaMode.{}", self.as_str())
    }
}

impl TokenLiteral for crate::value::CastPolicy {
    fn token_literal(&self) -> String {
        match self {
            crate::value::CastPolicy::Error => "CastPolicy.Error".to_string(),
            crate::value::CastPolicy::Skip => "CastPolicy.Skip".to_string(),
            crate::value::CastPolicy::Default(value) => {
                format!("CastPolicy.Default({})", rhai_literal(value))
            }
        }
    }
}

/// Every flag joined with ` | ` (`ProfileType.Memory`); no flag, which no DSL expression
/// yields, is the Rust API's `ProfileType.none()`.
impl TokenLiteral for crate::traversal::context::ProfileType {
    fn token_literal(&self) -> String {
        use crate::traversal::context::ProfileType;
        let flags: Vec<&str> = [(ProfileType::Memory, "ProfileType.Memory")]
            .into_iter()
            .filter(|(flag, _)| self.contains(*flag))
            .map(|(_, name)| name)
            .collect();
        if flags.is_empty() {
            "ProfileType.none()".to_string()
        } else {
            flags.join(" | ")
        }
    }
}

impl TokenLiteral for crate::graph::schema::VisualizeFormat {
    fn token_literal(&self) -> String {
        use crate::graph::schema::VisualizeFormat;
        match self {
            VisualizeFormat::Auto => "V.Auto",
            VisualizeFormat::Table => "V.Table",
            VisualizeFormat::Markdown => "V.Markdown",
            VisualizeFormat::Json => "V.Json",
            VisualizeFormat::JsonSchema => "V.JsonSchema",
            VisualizeFormat::Mermaid => "V.Mermaid",
            VisualizeFormat::PlantUML => "V.PlantUML",
            VisualizeFormat::Tree => "V.Tree",
        }
        .to_string()
    }
}

impl TokenLiteral for crate::value::P<PValue> {
    fn token_literal(&self) -> String {
        crate::value::predicate::render_predicate(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_escape_quotes_backslashes_and_controls() {
        assert_eq!(rhai_string("a\"b\\c\nd"), r#""a\"b\\c\nd""#);
        assert_eq!(rhai_string("\u{1}"), r#""\u0001""#);
    }

    #[test]
    fn long_strings_and_lists_are_cut_with_their_length() {
        let long = "x".repeat(100);
        assert_eq!(
            rhai_string(&long),
            format!("\"{}…\" (100 chars)", "x".repeat(MAX_LITERAL_CHARS))
        );
        assert_eq!(
            rhai_string(&"y".repeat(64)),
            format!("\"{}\"", "y".repeat(64))
        );
        let list = ElementProperty::Array((0..20i64).map(ElementProperty::from).collect());
        assert_eq!(
            rhai_literal(&list),
            "[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, …] (20 items)"
        );
        let args: Vec<ElementProperty> = (0..17i64).map(ElementProperty::from).collect();
        assert!(rhai_literals(&args).ends_with("15, … (17 items)"));
    }

    #[test]
    fn floats_keep_a_decimal_point() {
        assert_eq!(rhai_number(&Number::from_f64(2.0)), "2.0");
        assert_eq!(rhai_number(&Number::from_f64(0.5)), "0.5");
        assert_eq!(rhai_number(&Number::from_f64(1e300)), "1e300");
        assert_eq!(rhai_number(&Number::from_i64(-3)), "-3");
    }

    #[test]
    fn containers_use_rhai_syntax() {
        let mut map = crate::value::PropertyMap::default();
        map.insert("a b", ElementProperty::Null);
        map.insert("k", ElementProperty::Array(vec![1i64.into(), "x".into()]));
        assert_eq!(
            rhai_literal(&ElementProperty::Object(Box::new(map))),
            r#"#{"a b": (), k: [1, "x"]}"#
        );
    }
}
