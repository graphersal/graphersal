//! TEMPORARY stand-ins for the value notation and the result comparison (task 3b.T3).
//!
//! The runner calls exactly two seams, both defined here until 3b.T3 lands:
//! - [`parameter_value`]: one cell of an `And parameters are:` table to the value passed to
//!   `gql_with`. 3b.T3 replaces the body with its notation parser
//!   (`values::parse_value(&str) -> Result<TckValue, String>`) plus its conversion of a
//!   `TckValue` to an `ElementProperty`.
//! - [`check_result`]: the rows of a query against a `Then the result should be ...` step.
//!   3b.T3 replaces the body with `compare.rs`; until then every result is
//!   [`ResultCheck::Unchecked`] ("the query ran"), which the classification counts as a pass
//!   flagged `result_unchecked`.
//!
//! Once both are replaced, this file and [`ResultCheck::Unchecked`] are removed.

use graphersal::ElementProperty;
use graphersal::storage::PropertyMap;

use super::gherkin::Step;

/// What the comparison of a result step found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResultCheck {
    /// The rows match the expected table.
    Match,
    /// TEMPORARY (removed with this file): the rows were not compared.
    Unchecked,
    /// The column names differ, with a one-line description.
    WrongColumns(String),
    /// The values differ, with a one-line description of the first difference.
    WrongResult(String),
}

/// The value of one parameter cell (the TCK value notation, raw as the parser keeps it).
pub fn parameter_value(raw: &str) -> Result<ElementProperty, String> {
    temporary_parameter_value(raw)
}

/// Compares `rows` (each a map column -> value, as `to_list()` returns them) with `expected`,
/// a [`Step::Result`] or [`Step::ResultEmpty`].
pub fn check_result(expected: &Step, rows: &[ElementProperty]) -> ResultCheck {
    match expected {
        // The one comparison that needs no notation: an empty result.
        Step::ResultEmpty if rows.is_empty() => ResultCheck::Match,
        Step::ResultEmpty => {
            ResultCheck::WrongResult(format!("expected no rows, got {} row(s)", rows.len()))
        }
        _ => ResultCheck::Unchecked,
    }
}

/// A minimal reading of the notation for parameter values only: `null`, booleans, integers,
/// floats, single-quoted strings (`\'` and `\\` escapes), lists and maps of those. Anything else
/// is an error naming the text (the case becomes class `setup`).
fn temporary_parameter_value(raw: &str) -> Result<ElementProperty, String> {
    let mut p = MiniParser {
        text: raw.as_bytes(),
        at: 0,
        depth: 0,
    };
    let value = p.value()?;
    p.blank();
    if p.at != p.text.len() {
        return Err(format!("unexpected text after the value in `{raw}`"));
    }
    Ok(value)
}

struct MiniParser<'a> {
    text: &'a [u8],
    at: usize,
    depth: usize,
}

impl MiniParser<'_> {
    fn blank(&mut self) {
        while self.at < self.text.len() && self.text[self.at].is_ascii_whitespace() {
            self.at += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.text.get(self.at).copied()
    }

    fn eat(&mut self, c: u8) -> bool {
        self.blank();
        if self.peek() == Some(c) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn value(&mut self) -> Result<ElementProperty, String> {
        self.depth += 1;
        if self.depth > 64 {
            return Err("parameter value nested too deeply".to_string());
        }
        self.blank();
        let v = match self.peek() {
            Some(b'[') => self.list(),
            Some(b'{') => self.map(),
            Some(b'\'') => self.string().map(ElementProperty::from),
            Some(_) => self.atom(),
            None => Err("empty parameter value".to_string()),
        };
        self.depth -= 1;
        v
    }

    fn list(&mut self) -> Result<ElementProperty, String> {
        self.at += 1;
        let mut items = Vec::new();
        if self.eat(b']') {
            return Ok(ElementProperty::Array(items));
        }
        loop {
            items.push(self.value()?);
            if self.eat(b']') {
                return Ok(ElementProperty::Array(items));
            }
            if !self.eat(b',') {
                return Err("expected `,` or `]` in a list".to_string());
            }
        }
    }

    fn map(&mut self) -> Result<ElementProperty, String> {
        self.at += 1;
        let mut map = PropertyMap::new();
        if self.eat(b'}') {
            return Ok(ElementProperty::Object(Box::new(map)));
        }
        loop {
            self.blank();
            let start = self.at;
            while self
                .peek()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_')
            {
                self.at += 1;
            }
            let key = std::str::from_utf8(&self.text[start..self.at])
                .map_err(|_| "map key is not UTF-8".to_string())?
                .to_string();
            if key.is_empty() || !self.eat(b':') {
                return Err("expected `key:` in a map".to_string());
            }
            let value = self.value()?;
            map.insert(key, value);
            if self.eat(b'}') {
                return Ok(ElementProperty::Object(Box::new(map)));
            }
            if !self.eat(b',') {
                return Err("expected `,` or `}` in a map".to_string());
            }
        }
    }

    fn string(&mut self) -> Result<String, String> {
        self.at += 1;
        let mut out = Vec::new();
        while let Some(c) = self.peek() {
            self.at += 1;
            match c {
                b'\'' => {
                    return String::from_utf8(out).map_err(|_| "string is not UTF-8".to_string());
                }
                b'\\' => match self.peek() {
                    Some(e @ (b'\'' | b'\\')) => {
                        self.at += 1;
                        out.push(e);
                    }
                    _ => return Err("unsupported escape in a string".to_string()),
                },
                _ => out.push(c),
            }
        }
        Err("unterminated string".to_string())
    }

    fn atom(&mut self) -> Result<ElementProperty, String> {
        let start = self.at;
        while self
            .peek()
            .is_some_and(|c| !matches!(c, b',' | b']' | b'}') && !c.is_ascii_whitespace())
        {
            self.at += 1;
        }
        let word = std::str::from_utf8(&self.text[start..self.at])
            .map_err(|_| "value is not UTF-8".to_string())?;
        match word {
            "null" => Ok(ElementProperty::Null),
            "true" => Ok(ElementProperty::from(true)),
            "false" => Ok(ElementProperty::from(false)),
            _ => {
                if let Ok(i) = word.parse::<i64>() {
                    Ok(ElementProperty::from(i))
                } else if word.contains(['.', 'e', 'E'])
                    && let Ok(f) = word.parse::<f64>()
                {
                    Ok(ElementProperty::from(f))
                } else {
                    Err(format!(
                        "temporary parameter conversion does not read `{word}`"
                    ))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_temporary_conversion_reads_scalars_lists_and_maps() {
        assert_eq!(parameter_value("1"), Ok(ElementProperty::from(1i64)));
        assert_eq!(parameter_value("-2.5"), Ok(ElementProperty::from(-2.5)));
        assert_eq!(parameter_value("'a\\'b'"), Ok(ElementProperty::from("a'b")));
        assert_eq!(parameter_value("null"), Ok(ElementProperty::Null));
        let list = parameter_value("[1, {name: 'x'}, []]").unwrap();
        let ElementProperty::Array(items) = list else {
            panic!("not a list")
        };
        assert_eq!(items.len(), 3);
        assert!(parameter_value("(:A)").is_err());
        assert!(parameter_value("[1").is_err());
    }
}
