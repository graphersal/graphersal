//! The fingerprint of a query for the query statistics: the optimized plan's canonical rendering
//! (the steps as `.profile()` names them, joined by `.`) with every literal replaced by `?`, so
//! `g.v().has("name", "ann")` and `g.v().has("name", "bob")` count as one query (as
//! `pg_stat_statements` normalizes constants). A list of literals becomes one `?`.

/// The longest fingerprint kept (a longer one is cut; its tail rarely tells queries apart).
pub(crate) const MAX_FINGERPRINT: usize = 400;

/// `text` with its string and number literals replaced by `?` and runs of `?, ?` collapsed.
pub(crate) fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(MAX_FINGERPRINT));
    let mut chars = text.chars().peekable();
    let mut previous_word = false;
    // The open brackets: a list of literals (`[`) folds into one `?`, call arguments (`(`) do
    // not.
    let mut open: Vec<char> = Vec::new();
    while let Some(c) = chars.next() {
        match c {
            '"' | '\'' => {
                // A string literal (with its escapes) becomes `?`.
                while let Some(d) = chars.next() {
                    if d == '\\' {
                        chars.next();
                    } else if d == c {
                        break;
                    }
                }
                push_literal(&mut out, open.last() == Some(&'['));
                previous_word = false;
            }
            c if c.is_ascii_digit() && !previous_word => {
                while chars
                    .peek()
                    .is_some_and(|d| d.is_ascii_alphanumeric() || *d == '.' || *d == '_')
                {
                    chars.next();
                }
                push_literal(&mut out, open.last() == Some(&'['));
                previous_word = false;
            }
            '-' if chars.peek().is_some_and(char::is_ascii_digit) && !previous_word => {
                // A negative number: the digits follow as one literal.
            }
            c => {
                match c {
                    '[' | '(' | '{' => open.push(c),
                    ']' | ')' | '}' => {
                        open.pop();
                    }
                    _ => {}
                }
                previous_word = c.is_alphanumeric() || c == '_';
                out.push(c);
            }
        }
        if out.len() >= MAX_FINGERPRINT {
            break;
        }
    }
    out
}

/// Appends `?`; inside a list (`in_list`) it folds into a preceding `?, ` (a list of literals is
/// one `?`).
fn push_literal(out: &mut String, in_list: bool) {
    if in_list && out.ends_with("?, ") {
        out.truncate(out.len() - 2);
    } else {
        out.push('?');
    }
}

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn literals_become_placeholders() {
        assert_eq!(normalize(r#"has("name", "ann")"#), "has(?, ?)");
        assert_eq!(
            normalize(r#"v(labels: ["person"]).has("age", P.gt(30))"#),
            "v(labels: [?]).has(?, P.gt(?))"
        );
        assert_eq!(normalize(r#"v(ids: ["1", "2", "3"])"#), "v(ids: [?])");
        assert_eq!(
            normalize("limit(10).range(-5, 2.5)"),
            "limit(?).range(?, ?)"
        );
        assert_eq!(normalize(r#"has("q", "a \"quoted\" text")"#), "has(?, ?)");
        // Digits inside names stay.
        assert_eq!(normalize("v2().as(\"a1\")"), "v2().as(?)");
        assert_eq!(normalize(&"x".repeat(1000)).len(), super::MAX_FINGERPRINT);
    }
}
