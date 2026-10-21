//! The TCK value notation: the cells of result tables and the values of parameter tables.
//!
//! [`parse_value`] reads one cell (already unescaped by the Gherkin parser: `\|`, `\\` and `\n`
//! are resolved there, so a cell may hold a real newline) into a [`TckValue`]:
//!
//! - `null`, `true`, `false`;
//! - integers (`1`, `-9223372036854775808`) and floats (`1.0`, `-0.0`, `.5`, `1e-305`,
//!   `1.23456789e308`, `NaN`, `Inf` / `Infinity` with an optional sign). A number with a `.` or
//!   an exponent is a float, otherwise an integer: the two stay distinct (`1` != `1.0`);
//! - strings in single or double quotes with the Cypher escapes `\'`, `\"`, `\\`, `\n`, `\t`,
//!   `\r`, `\b`, `\f`, `\uXXXX`, `\UXXXXXXXX` (an unknown escape keeps the backslash);
//! - lists `[a, b]` and maps `{k: v, `odd key`: v}` (keys are identifiers or backtick-quoted);
//! - nodes `(:A:B {k: v})`, relationships `[:T {k: v}]` (no variables, no ids: elements are
//!   matched by labels / type and properties);
//! - paths `<(..)-[..]->(..)<-[..]-(..)>`, each hop in either direction.
//!
//! [`to_element_property`] converts a value to an `ElementProperty` for a query parameter;
//! nodes, relationships and paths are refused (the TCK passes only plain values).
//!
//! Recursion is bounded by [`MAX_DEPTH`]; a malformed cell is an `Err` with the position, never a
//! panic.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use graphersal::ElementProperty;
use graphersal::prelude::PropertyMap;

/// The deepest nesting [`parse_value`] accepts (the pinned tree nests at most a few levels).
pub const MAX_DEPTH: usize = 64;

/// A value of the TCK notation, or a result value materialized for comparison.
#[derive(Debug, Clone, PartialEq)]
pub enum TckValue {
    /// `null`.
    Null,
    /// `true` / `false`.
    Bool(bool),
    /// An integer (no `.`, no exponent).
    Int(i64),
    /// A float (`.` or exponent, `NaN`, `Inf`).
    Float(f64),
    /// A string.
    String(String),
    /// A list.
    List(Vec<TckValue>),
    /// A map with string keys.
    Map(BTreeMap<String, TckValue>),
    /// A node.
    Node(TckNode),
    /// A relationship.
    Relationship(TckRelationship),
    /// A path.
    Path(TckPath),
    /// A temporal result value, as its text in the TCK's form (`compare::temporal_text`: the
    /// `java.time` / Neo4j `toString`). Never produced by [`parse_value`]: the TCK writes
    /// temporals as strings, and an expected string matches it by exact text, so offsets are
    /// compared too.
    Temporal(String),
    /// A result value without a TCK notation (a UUID, vector, a temporal value the TCK form
    /// cannot write, or a map with non-string keys), kept as diagnostic text: it never equals an
    /// expected value.
    Opaque(String),
}

/// A node: its label set and properties; `id` only on materialized result values.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TckNode {
    /// The labels (a set: order is not significant).
    pub labels: BTreeSet<String>,
    /// The properties.
    pub properties: BTreeMap<String, TckValue>,
    /// The element id of a result node (path checks use it); `None` in the notation.
    pub id: Option<String>,
}

/// A relationship: its type and properties; `ends` only on materialized result values.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TckRelationship {
    /// The relationship type.
    pub rel_type: String,
    /// The properties.
    pub properties: BTreeMap<String, TckValue>,
    /// `(source id, target id)` of a result relationship; `None` in the notation.
    pub ends: Option<(String, String)>,
}

/// The direction of one path hop, as written from left to right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// `-[..]->`: the relationship points from the previous node to the next.
    Forward,
    /// `<-[..]-`: the relationship points from the next node to the previous.
    Backward,
}

/// A path: a start node and hops of (relationship, direction, next node).
#[derive(Debug, Clone, PartialEq)]
pub struct TckPath {
    /// The first node.
    pub start: TckNode,
    /// The hops in order.
    pub hops: Vec<(TckRelationship, Direction, TckNode)>,
}

/// Parses one TCK cell / parameter value. The whole text must be one value (surrounding
/// whitespace allowed).
pub fn parse_value(text: &str) -> Result<TckValue, String> {
    let mut parser = Parser {
        text,
        chars: text.char_indices().collect(),
        pos: 0,
    };
    let value = parser.value(0)?;
    parser.skip_ws();
    if parser.pos < parser.chars.len() {
        return Err(parser.error("trailing text after the value"));
    }
    Ok(value)
}

/// Converts a parameter value to an `ElementProperty`: lists become arrays, maps objects;
/// nodes, relationships, paths (and temporal or opaque values) are an error.
pub fn to_element_property(value: &TckValue) -> Result<ElementProperty, String> {
    Ok(match value {
        TckValue::Null => ElementProperty::Null,
        TckValue::Bool(b) => ElementProperty::from(*b),
        TckValue::Int(i) => ElementProperty::from(*i),
        TckValue::Float(f) => ElementProperty::from(*f),
        TckValue::String(s) => ElementProperty::from(s.as_str()),
        TckValue::List(items) => ElementProperty::Array(
            items
                .iter()
                .map(to_element_property)
                .collect::<Result<_, _>>()?,
        ),
        TckValue::Map(map) => {
            let mut object = PropertyMap::default();
            for (key, value) in map {
                object.insert(key.as_str(), to_element_property(value)?);
            }
            ElementProperty::Object(Box::new(object))
        }
        TckValue::Node(_) | TckValue::Relationship(_) | TckValue::Path(_) => {
            return Err(format!(
                "a graph element cannot be a parameter value: {value}"
            ));
        }
        TckValue::Temporal(text) | TckValue::Opaque(text) => {
            return Err(format!("not a parameter value: {text}"));
        }
    })
}

struct Parser<'a> {
    text: &'a str,
    chars: Vec<(usize, char)>,
    pos: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).map(|(_, c)| *c)
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).map(|(_, c)| *c)
    }

    fn byte_pos(&self) -> usize {
        self.chars
            .get(self.pos)
            .map_or(self.text.len(), |(b, _)| *b)
    }

    fn error(&self, message: &str) -> String {
        format!("{message} at byte {} of {:?}", self.byte_pos(), self.text)
    }

    fn skip_ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.pos += 1;
        }
    }

    fn eat(&mut self, c: char) -> bool {
        self.skip_ws();
        if self.peek() == Some(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, c: char) -> Result<(), String> {
        if self.eat(c) {
            Ok(())
        } else {
            Err(self.error(&format!("expected `{c}`")))
        }
    }

    /// Whether the text at the cursor starts with `word` followed by a non-identifier char.
    fn at_word(&self, word: &str) -> bool {
        let n = word.chars().count();
        word.chars()
            .enumerate()
            .all(|(i, c)| self.peek_at(i) == Some(c))
            && !self.peek_at(n).is_some_and(is_ident_char)
    }

    fn value(&mut self, depth: usize) -> Result<TckValue, String> {
        if depth > MAX_DEPTH {
            return Err(self.error("nesting too deep"));
        }
        self.skip_ws();
        match self.peek() {
            None => Err(self.error("expected a value")),
            Some('\'' | '"') => self.string().map(TckValue::String),
            Some('[') => {
                // `[:` starts a relationship, anything else a list.
                let save = self.pos;
                self.pos += 1;
                self.skip_ws();
                let is_rel = self.peek() == Some(':');
                self.pos = save;
                if is_rel {
                    self.relationship(depth).map(TckValue::Relationship)
                } else {
                    self.list(depth)
                }
            }
            Some('{') => self.map(depth).map(TckValue::Map),
            Some('(') => self.node(depth).map(TckValue::Node),
            Some('<') => self.path(depth).map(TckValue::Path),
            Some(c) if c == '-' || c == '+' || c == '.' || c.is_ascii_digit() => self.number(),
            Some(_) => {
                for (word, value) in [
                    ("null", TckValue::Null),
                    ("true", TckValue::Bool(true)),
                    ("false", TckValue::Bool(false)),
                    ("NaN", TckValue::Float(f64::NAN)),
                    ("Infinity", TckValue::Float(f64::INFINITY)),
                    ("Inf", TckValue::Float(f64::INFINITY)),
                ] {
                    if self.at_word(word) {
                        self.pos += word.chars().count();
                        return Ok(value);
                    }
                }
                Err(self.error("unknown value"))
            }
        }
    }

    fn number(&mut self) -> Result<TckValue, String> {
        let start = self.pos;
        let mut text = String::new();
        if let Some(sign @ ('-' | '+')) = self.peek() {
            text.push(sign);
            self.pos += 1;
        }
        for word in ["Infinity", "Inf"] {
            if self.at_word(word) {
                self.pos += word.chars().count();
                let inf = if text == "-" {
                    f64::NEG_INFINITY
                } else {
                    f64::INFINITY
                };
                return Ok(TckValue::Float(inf));
            }
        }
        if self.at_word("NaN") {
            self.pos += 3;
            return Ok(TckValue::Float(f64::NAN));
        }
        let mut float = false;
        let mut digits = 0;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                digits += 1;
            } else if c == '.' && !float && self.peek_at(1).is_some_and(|d| d.is_ascii_digit()) {
                float = true;
            } else if (c == 'e' || c == 'E') && digits > 0 {
                float = true;
                text.push(c);
                self.pos += 1;
                if let Some(sign @ ('-' | '+')) = self.peek() {
                    text.push(sign);
                    self.pos += 1;
                }
                continue;
            } else {
                break;
            }
            text.push(c);
            self.pos += 1;
        }
        if digits == 0 || self.peek().is_some_and(is_ident_char) {
            self.pos = start;
            return Err(self.error("malformed number"));
        }
        if float {
            text.parse::<f64>()
                .map(TckValue::Float)
                .map_err(|e| format!("float {text:?}: {e}"))
        } else {
            text.parse::<i64>()
                .map(TckValue::Int)
                .map_err(|e| format!("integer {text:?}: {e}"))
        }
    }

    fn string(&mut self) -> Result<String, String> {
        let quote = self.peek().ok_or_else(|| self.error("expected a string"))?;
        self.pos += 1;
        let mut out = String::new();
        loop {
            let c = self
                .peek()
                .ok_or_else(|| self.error("unterminated string"))?;
            self.pos += 1;
            if c == quote {
                return Ok(out);
            }
            if c != '\\' {
                out.push(c);
                continue;
            }
            let e = self
                .peek()
                .ok_or_else(|| self.error("unterminated escape"))?;
            self.pos += 1;
            match e {
                '\'' | '"' | '\\' => out.push(e),
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'b' => out.push('\u{8}'),
                'f' => out.push('\u{c}'),
                'u' | 'U' => {
                    let n = if e == 'u' { 4 } else { 8 };
                    let hex: String = (0..n).filter_map(|i| self.peek_at(i)).collect();
                    let code = (hex.chars().count() == n)
                        .then(|| u32::from_str_radix(&hex, 16).ok())
                        .flatten()
                        .and_then(char::from_u32)
                        .ok_or_else(|| self.error("bad unicode escape"))?;
                    self.pos += n;
                    out.push(code);
                }
                other => {
                    out.push('\\');
                    out.push(other);
                }
            }
        }
    }

    fn list(&mut self, depth: usize) -> Result<TckValue, String> {
        self.expect('[')?;
        let mut items = Vec::new();
        if self.eat(']') {
            return Ok(TckValue::List(items));
        }
        loop {
            items.push(self.value(depth + 1)?);
            if self.eat(']') {
                return Ok(TckValue::List(items));
            }
            self.expect(',')?;
        }
    }

    /// A name: an identifier or a backtick-quoted name (a doubled backtick is one backtick).
    fn name(&mut self) -> Result<String, String> {
        self.skip_ws();
        if self.peek() == Some('`') {
            self.pos += 1;
            let mut out = String::new();
            loop {
                match self.peek() {
                    None => return Err(self.error("unterminated backtick name")),
                    Some('`') if self.peek_at(1) == Some('`') => {
                        out.push('`');
                        self.pos += 2;
                    }
                    Some('`') => {
                        self.pos += 1;
                        return Ok(out);
                    }
                    Some(c) => {
                        out.push(c);
                        self.pos += 1;
                    }
                }
            }
        }
        let mut out = String::new();
        while let Some(c) = self.peek().filter(|c| is_ident_char(*c)) {
            out.push(c);
            self.pos += 1;
        }
        if out.is_empty() {
            return Err(self.error("expected a name"));
        }
        Ok(out)
    }

    fn map(&mut self, depth: usize) -> Result<BTreeMap<String, TckValue>, String> {
        self.expect('{')?;
        let mut map = BTreeMap::new();
        if self.eat('}') {
            return Ok(map);
        }
        loop {
            let key = self.name()?;
            self.expect(':')?;
            let value = self.value(depth + 1)?;
            if map.insert(key.clone(), value).is_some() {
                return Err(self.error(&format!("duplicate map key `{key}`")));
            }
            if self.eat('}') {
                return Ok(map);
            }
            self.expect(',')?;
        }
    }

    /// Optional `{..}` properties of an element.
    fn properties(&mut self, depth: usize) -> Result<BTreeMap<String, TckValue>, String> {
        self.skip_ws();
        if self.peek() == Some('{') {
            self.map(depth + 1)
        } else {
            Ok(BTreeMap::new())
        }
    }

    fn node(&mut self, depth: usize) -> Result<TckNode, String> {
        self.expect('(')?;
        let mut labels = BTreeSet::new();
        while self.eat(':') {
            labels.insert(self.name()?);
        }
        let properties = self.properties(depth)?;
        self.expect(')')?;
        Ok(TckNode {
            labels,
            properties,
            id: None,
        })
    }

    fn relationship(&mut self, depth: usize) -> Result<TckRelationship, String> {
        self.expect('[')?;
        self.expect(':')?;
        let rel_type = self.name()?;
        let properties = self.properties(depth)?;
        self.expect(']')?;
        Ok(TckRelationship {
            rel_type,
            properties,
            ends: None,
        })
    }

    fn path(&mut self, depth: usize) -> Result<TckPath, String> {
        self.expect('<')?;
        let start = self.node(depth + 1)?;
        let mut hops = Vec::new();
        loop {
            self.skip_ws();
            if self.eat('>') {
                return Ok(TckPath { start, hops });
            }
            let direction = if self.eat('<') {
                self.expect('-')?;
                Direction::Backward
            } else {
                self.expect('-')?;
                Direction::Forward
            };
            let rel = self.relationship(depth + 1)?;
            self.expect('-')?;
            if direction == Direction::Forward {
                self.expect('>')?;
            }
            let next = self.node(depth + 1)?;
            hops.push((rel, direction, next));
        }
    }
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Renders a name as written in the notation: bare when it is an identifier, else backticked.
fn write_name(f: &mut fmt::Formatter<'_>, name: &str) -> fmt::Result {
    let bare = !name.is_empty()
        && name.chars().all(is_ident_char)
        && !name.starts_with(|c: char| c.is_ascii_digit());
    if bare {
        f.write_str(name)
    } else {
        write!(f, "`{}`", name.replace('`', "``"))
    }
}

fn write_map(f: &mut fmt::Formatter<'_>, map: &BTreeMap<String, TckValue>) -> fmt::Result {
    f.write_str("{")?;
    for (i, (key, value)) in map.iter().enumerate() {
        if i > 0 {
            f.write_str(", ")?;
        }
        write_name(f, key)?;
        write!(f, ": {value}")?;
    }
    f.write_str("}")
}

impl fmt::Display for TckNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("(")?;
        for label in &self.labels {
            f.write_str(":")?;
            write_name(f, label)?;
        }
        if !self.properties.is_empty() {
            if !self.labels.is_empty() {
                f.write_str(" ")?;
            }
            write_map(f, &self.properties)?;
        }
        f.write_str(")")
    }
}

impl fmt::Display for TckRelationship {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[:")?;
        write_name(f, &self.rel_type)?;
        if !self.properties.is_empty() {
            f.write_str(" ")?;
            write_map(f, &self.properties)?;
        }
        f.write_str("]")
    }
}

impl fmt::Display for TckValue {
    /// The notation text (diagnostics only; floats print with a `.` or exponent).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TckValue::Null => f.write_str("null"),
            TckValue::Bool(b) => write!(f, "{b}"),
            TckValue::Int(i) => write!(f, "{i}"),
            TckValue::Float(x) if x.is_nan() => f.write_str("NaN"),
            TckValue::Float(x) if x.is_infinite() => {
                f.write_str(if *x > 0.0 { "Inf" } else { "-Inf" })
            }
            TckValue::Float(x) => write!(f, "{x:?}"),
            TckValue::String(s) => {
                f.write_str("'")?;
                for c in s.chars() {
                    match c {
                        '\'' => f.write_str("\\'")?,
                        '\\' => f.write_str("\\\\")?,
                        '\n' => f.write_str("\\n")?,
                        c => write!(f, "{c}")?,
                    }
                }
                f.write_str("'")
            }
            TckValue::List(items) => {
                f.write_str("[")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{item}")?;
                }
                f.write_str("]")
            }
            TckValue::Map(map) => write_map(f, map),
            TckValue::Node(node) => write!(f, "{node}"),
            TckValue::Relationship(rel) => write!(f, "{rel}"),
            TckValue::Path(path) => {
                write!(f, "<{}", path.start)?;
                for (rel, direction, node) in &path.hops {
                    match direction {
                        Direction::Forward => write!(f, "-{rel}->{node}")?,
                        Direction::Backward => write!(f, "<-{rel}-{node}")?,
                    }
                }
                f.write_str(">")
            }
            TckValue::Temporal(text) => write!(f, "'{text}'"),
            TckValue::Opaque(text) => write!(f, "<opaque {text}>"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::super::gherkin::{self, Step};
    use super::*;

    fn p(text: &str) -> TckValue {
        parse_value(text).unwrap_or_else(|e| panic!("{text}: {e}"))
    }

    fn map(entries: &[(&str, TckValue)]) -> BTreeMap<String, TckValue> {
        entries
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    fn node(labels: &[&str], props: &[(&str, TckValue)]) -> TckNode {
        TckNode {
            labels: labels.iter().map(|l| (*l).to_string()).collect(),
            properties: map(props),
            id: None,
        }
    }

    fn rel(t: &str, props: &[(&str, TckValue)]) -> TckRelationship {
        TckRelationship {
            rel_type: t.to_string(),
            properties: map(props),
            ends: None,
        }
    }

    #[test]
    fn scalars() {
        assert_eq!(p("null"), TckValue::Null);
        assert_eq!(p(" true "), TckValue::Bool(true));
        assert_eq!(p("false"), TckValue::Bool(false));
        assert_eq!(p("0"), TckValue::Int(0));
        assert_eq!(p("-12"), TckValue::Int(-12));
        assert_eq!(p("9223372036854775807"), TckValue::Int(i64::MAX));
        assert_eq!(p("-9223372036854775808"), TckValue::Int(i64::MIN));
        assert!(parse_value("9223372036854775808").is_err());
    }

    #[test]
    fn floats_stay_floats() {
        assert_eq!(p("1.0"), TckValue::Float(1.0));
        assert_ne!(p("1.0"), TckValue::Int(1));
        assert_eq!(p(".5"), TckValue::Float(0.5));
        assert_eq!(p("1e-305"), TckValue::Float(1e-305));
        assert_eq!(p("-1e-305"), TckValue::Float(-1e-305));
        assert_eq!(p("1.23456789e308"), TckValue::Float(1.23456789e308));
        assert_eq!(p("1E3"), TckValue::Float(1000.0));
        assert_eq!(p("2.5e+2"), TckValue::Float(250.0));
        match p("-0.0") {
            TckValue::Float(z) => assert!(z == 0.0 && z.is_sign_negative()),
            other => panic!("{other:?}"),
        }
        assert!(matches!(p("NaN"), TckValue::Float(x) if x.is_nan()));
        assert!(matches!(p("-NaN"), TckValue::Float(x) if x.is_nan()));
        assert_eq!(p("Inf"), TckValue::Float(f64::INFINITY));
        assert_eq!(p("Infinity"), TckValue::Float(f64::INFINITY));
        assert_eq!(p("-Inf"), TckValue::Float(f64::NEG_INFINITY));
        assert_eq!(p("-Infinity"), TckValue::Float(f64::NEG_INFINITY));
        assert!(parse_value("1.").is_err());
        assert!(parse_value("1x").is_err());
        assert!(parse_value("-").is_err());
    }

    #[test]
    fn strings_with_escapes() {
        assert_eq!(p("''"), TckValue::String(String::new()));
        assert_eq!(p("'a b'"), TckValue::String("a b".into()));
        assert_eq!(p(r"'\''"), TckValue::String("'".into()));
        assert_eq!(p(r#""say \"hi\"""#), TckValue::String("say \"hi\"".into()));
        // Literals6 [5] after the Gherkin unescape.
        assert_eq!(
            p(r#"'a\\bcn5t\'"\\//\\"\''"#),
            TckValue::String(r#"a\bcn5t'"\//\"'"#.into())
        );
        assert_eq!(p(r"'\nFoo\n'"), TckValue::String("\nFoo\n".into()));
        // A real newline from the Gherkin `\n` escape stays one.
        assert_eq!(p("'\nFoo\n'"), TckValue::String("\nFoo\n".into()));
        assert_eq!(p(r"'\t\r'"), TckValue::String("\t\r".into()));
        assert_eq!(p(r"'é\U0001F600'"), TckValue::String("é😀".into()));
        assert_eq!(p("'🧐🍌❖⋙⚐'"), TckValue::String("🧐🍌❖⋙⚐".into()));
        assert!(parse_value("'open").is_err());
        assert!(parse_value(r"'\u12'").is_err());
    }

    #[test]
    fn nested_lists_and_maps() {
        assert_eq!(p("[]"), TckValue::List(vec![]));
        assert_eq!(p("{}"), TckValue::Map(BTreeMap::new()));
        assert_eq!(
            p("[1, [2.0, [null]], {a: ['x'], b: {c: true}}]"),
            TckValue::List(vec![
                TckValue::Int(1),
                TckValue::List(vec![
                    TckValue::Float(2.0),
                    TckValue::List(vec![TckValue::Null])
                ]),
                TckValue::Map(map(&[
                    ("a", TckValue::List(vec![TckValue::String("x".into())])),
                    ("b", TckValue::Map(map(&[("c", TckValue::Bool(true))]))),
                ])),
            ])
        );
        assert_eq!(
            p("{``: null, `a``b`: 1, null: 2, NULL: 3}"),
            TckValue::Map(map(&[
                ("", TckValue::Null),
                ("a`b", TckValue::Int(1)),
                ("null", TckValue::Int(2)),
                ("NULL", TckValue::Int(3)),
            ]))
        );
        assert!(parse_value("[1, 2").is_err());
        assert!(parse_value("{a: 1, a: 2}").is_err());
        assert!(parse_value("[1] 2").is_err());
        let deep = format!("{}{}", "[".repeat(MAX_DEPTH + 2), "]".repeat(MAX_DEPTH + 2));
        assert!(parse_value(&deep).is_err());
    }

    #[test]
    fn nodes_and_relationships() {
        assert_eq!(p("()"), TckValue::Node(node(&[], &[])));
        assert_eq!(
            p("(:B:A {name: 'x', num: 1})"),
            TckValue::Node(node(
                &["A", "B"],
                &[
                    ("name", TckValue::String("x".into())),
                    ("num", TckValue::Int(1)),
                ]
            ))
        );
        assert_eq!(
            p("({a: 1})"),
            TckValue::Node(node(&[], &[("a", TckValue::Int(1))]))
        );
        assert_eq!(p("[:T]"), TckValue::Relationship(rel("T", &[])));
        assert_eq!(
            p("[ :KNOWS {num: 1.5}]"),
            TckValue::Relationship(rel("KNOWS", &[("num", TckValue::Float(1.5))]))
        );
        assert!(parse_value("[:]").is_err());
        assert!(parse_value("(:A").is_err());
    }

    #[test]
    fn paths_in_both_directions() {
        assert_eq!(
            p("<()>"),
            TckValue::Path(TckPath {
                start: node(&[], &[]),
                hops: vec![]
            })
        );
        let mixed = p("<(:Label1)<-[:T1]-(:Label2)-[:T2 {k: 'v'}]->(:Label3)>");
        assert_eq!(
            mixed,
            TckValue::Path(TckPath {
                start: node(&["Label1"], &[]),
                hops: vec![
                    (rel("T1", &[]), Direction::Backward, node(&["Label2"], &[])),
                    (
                        rel("T2", &[("k", TckValue::String("v".into()))]),
                        Direction::Forward,
                        node(&["Label3"], &[])
                    ),
                ],
            })
        );
        // Display round-trips.
        assert_eq!(p(&mixed.to_string()), mixed);
        assert!(parse_value("<(:A)-[:T]-(:B)>").is_err());
        assert!(parse_value("<(:A)<-[:T]->(:B)>").is_err());
        assert!(parse_value("<(:A)-[:T]->>").is_err());
    }

    #[test]
    fn parameter_conversion() {
        let value = p("{name: 'Alice', age: 38, address: {city: 'London'}, xs: [1.5, null]}");
        let ElementProperty::Object(object) = to_element_property(&value).unwrap() else {
            panic!("not an object")
        };
        assert_eq!(object.get("name"), Some(&ElementProperty::from("Alice")));
        assert_eq!(object.get("age"), Some(&ElementProperty::from(38i64)));
        assert!(matches!(
            object.get("address"),
            Some(ElementProperty::Object(_))
        ));
        assert_eq!(
            object.get("xs"),
            Some(&ElementProperty::Array(vec![
                ElementProperty::from(1.5),
                ElementProperty::Null
            ]))
        );
        assert!(to_element_property(&p("(:A)")).is_err());
        assert!(to_element_property(&p("[:T]")).is_err());
        assert!(to_element_property(&p("<()>")).is_err());
        assert!(to_element_property(&p("[(:A)]")).is_err());
    }

    /// Every result cell and parameter value of the vendored tree parses.
    #[test]
    fn every_cell_of_the_tree_parses() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/opencypher/features");
        let features = gherkin::load_all(&root).unwrap_or_else(|e| panic!("{e}"));
        let mut failures = Vec::new();
        let mut cells = 0usize;
        let mut params = 0usize;
        for feature in &features {
            for case in feature.scenarios.iter().flat_map(|s| &s.cases) {
                for step in &case.steps {
                    match &step.step {
                        Step::Result { table, .. } => {
                            for cell in table.rows.iter().flatten() {
                                cells += 1;
                                if let Err(e) = parse_value(cell) {
                                    failures.push(format!("{}:{}: {e}", case.id, step.line));
                                }
                            }
                        }
                        Step::Parameters(pairs) => {
                            for (_, value) in pairs {
                                params += 1;
                                let parsed = parse_value(value)
                                    .and_then(|v| to_element_property(&v).map(|_| ()));
                                if let Err(e) = parsed {
                                    failures.push(format!("{}:{}: {e}", case.id, step.line));
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        assert!(
            cells > 3000 && params > 40,
            "{cells} cells, {params} params"
        );
        assert!(
            failures.is_empty(),
            "{} of {cells} cells / {params} params fail:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }
}
