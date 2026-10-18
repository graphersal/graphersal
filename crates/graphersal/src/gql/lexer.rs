//! Tokenizer of GQL query text: source text to [`Token`]s, each with a [`Span`].
//!
//! The lexer never panics on any input: it reads the source only through `str::get` and
//! `chars()`, so a byte offset that is not a character boundary yields no character instead of a
//! panic, and every failure is a [`LexError`] with the span of the offending text.
//!
//! # Lexical rules
//!
//! - **Whitespace** is any Unicode whitespace; line breaks are whitespace (multi-line queries).
//!   Lines are counted at `\n`; columns are 1-based and counted in characters, not bytes.
//! - **Comments:** `// ...` to the end of the line, `/* ... */` (not nested; unterminated is an
//!   error), and the GQL `-- ...` line comment under the rule below.
//! - **`--`: comment or edge.** ISO GQL reads `--` as a line comment, Cypher reads it as an edge in
//!   either direction (`(a)--(b)`, `(a)-->(b)`). `--` is read as two `-` tokens (an edge) when
//!   (a) the character right after it is `>` or `(` (`-->`, `--(`), or (b) the previous token is
//!   `)` on the same line (`(a)--(b)`, `(a) -- (b)`). Everywhere else it starts a line comment, so
//!   `-- note` on a line of its own, or after a value (`RETURN x -- the name`), is a comment. The
//!   rule never breaks an edge pattern; a `--` comment written right after a node pattern on the
//!   same line becomes an edge and fails to parse loudly (use `//` there), it never silently drops
//!   query text. Consequence: Cypher arithmetic `a--1` reads as `a` plus a comment (GQL wins).
//! - **Arrows are split, never `--`:** `->` and `<-` are tokens, the plain minus is `-`, and the
//!   abbreviated edges are sequences of them: `-->` is `-` `->`, `<--` is `<-` `-`, `--` is `-` `-`,
//!   `<-->` is `<-` `->`. The parser thus reads abbreviated and bracketed edges (`-[e]->`,
//!   `<-[e]-`, `-[e]-`) with one grammar, using span adjacency where it matters. In an expression
//!   the parser reads a `<-` token as `<` followed by a unary minus (`a<-1`).
//! - **`~` forms** (`~`, `~>`, `<~`, the GQL undirected edges) are tokens so the parser can refuse
//!   them by name.
//! - **Keywords are contextual.** A word that matches a [`Keyword`] (ASCII case-insensitive) is a
//!   [`TokenKind::Keyword`] token that keeps its text as written; whether it is reserved is decided
//!   by the parser per position. Where only a name can stand (after `.`, after `:` in a label
//!   expression, a map key, a function name before `(`) a keyword token is read as a name, so
//!   `n.order` or `labels(n)` keep working. Words such as `type`, `labels`, `count` are not
//!   keywords at all. A back-quoted identifier is never a keyword.
//! - **Identifiers** start with a Unicode letter or `_` and continue with letters, digits or `_`;
//!   they are case-sensitive. Back-quoted identifiers (`` `my name` ``) take any text, a doubled
//!   backtick is one backtick; an empty one is an error.
//! - **Parameters:** `$name`, `$0` or `` $`quoted name` ``.
//! - **Numbers:** decimal integers (`42`, `1_000`: `_` only between digits), `0x`/`0o`/`0b`
//!   integers, floats (`1.5`, `.5`, `1e3`, `1.5E-3`). An integer above 2^63 is an error; exactly
//!   2^63 is kept so the parser can fold `-9223372036854775808` (and refuses it without the minus).
//!   A float literal that overflows to infinity is an error. A leading zero (`007`, Cypher's
//!   deprecated octal) is an error, and so is a number glued to a letter (`1abc`, `1e`).
//!   `1..3` is `1` `..` `3`.
//! - **Strings** in `'...'` or `"..."`; they may span lines. Escapes: `\\`, `\'`, `\"`, `` \` ``,
//!   `\n`, `\t`, `\r`, `\b`, `\f`, `\uXXXX` (4 hex digits), `\UXXXXXXXX` (8 hex digits); the
//!   quote character doubled (`'it''s'`) is one quote. Any other escape, and a `\u` that is not a
//!   Unicode scalar value (e.g. a lone surrogate), is an error.
//! - **Punctuation and operators:** `( ) [ ] { } : , . .. ;`, `| & ! % * + - / =`, `<> !=`
//!   (both [`TokenKind::Ne`]), `< <= > >=`, `||`, `-> <-`, `~ ~> <~`.

use std::fmt;

/// A region of the source text: the byte range plus the 1-based line and column (in characters)
/// of its first character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Span {
    /// Byte offset of the first character.
    pub(crate) start: usize,
    /// Byte offset after the last character.
    pub(crate) end: usize,
    /// 1-based line of `start`.
    pub(crate) line: usize,
    /// 1-based column of `start`, counted in characters.
    pub(crate) column: usize,
}

impl Span {
    /// The span from the start of `self` to the end of `other` (an AST node built from several
    /// tokens).
    pub(crate) fn to(self, other: Span) -> Span {
        Span {
            end: other.end.max(self.start),
            ..self
        }
    }

    /// Whether `other` starts exactly where `self` ends (no whitespace or comment between).
    pub(crate) fn touches(self, other: Span) -> bool {
        self.end == other.start
    }
}

macro_rules! keywords {
    ($($variant:ident => $text:literal),* $(,)?) => {
        /// A keyword. Matched ASCII case-insensitively; contextual, not reserved (module docs).
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub(crate) enum Keyword {
            $(
                #[doc = concat!("`", $text, "`")]
                $variant,
            )*
        }

        impl Keyword {
            /// Every keyword (for "did you mean" suggestions).
            pub(crate) const ALL: &'static [Keyword] = &[$(Keyword::$variant),*];

            /// The canonical (upper-case) spelling.
            pub(crate) fn as_str(self) -> &'static str {
                match self {
                    $(Keyword::$variant => $text,)*
                }
            }
        }
    };
}

keywords! {
    // Stage 1a.
    Match => "MATCH", Where => "WHERE", Return => "RETURN", Distinct => "DISTINCT", As => "AS",
    Order => "ORDER", By => "BY", Asc => "ASC", Ascending => "ASCENDING", Desc => "DESC",
    Descending => "DESCENDING", Offset => "OFFSET", Skip => "SKIP", Limit => "LIMIT",
    And => "AND", Or => "OR", Xor => "XOR", Not => "NOT", Is => "IS", Null => "NULL",
    True => "TRUE", False => "FALSE", In => "IN", Starts => "STARTS", Ends => "ENDS",
    Contains => "CONTAINS", With => "WITH", Case => "CASE", When => "WHEN", Then => "THEN",
    Else => "ELSE", End => "END", Labeled => "LABELED",
    // Stage 1b.
    Optional => "OPTIONAL", Filter => "FILTER", Let => "LET", Nulls => "NULLS",
    First => "FIRST", Last => "LAST", Unknown => "UNKNOWN",
    // Recognised so the parser can refuse them by name.
    Insert => "INSERT", Create => "CREATE", Set => "SET", Remove => "REMOVE",
    Delete => "DELETE", Detach => "DETACH", Nodetach => "NODETACH", Merge => "MERGE",
    Call => "CALL", Yield => "YIELD", Union => "UNION", Except => "EXCEPT",
    Intersect => "INTERSECT", Next => "NEXT", For => "FOR", Unwind => "UNWIND",
    Exists => "EXISTS", Show => "SHOW", Use => "USE", Shortest => "SHORTEST", Cypher => "CYPHER",
}

impl Keyword {
    /// The keyword spelled `word` (ASCII case-insensitive), if any.
    pub(crate) fn from_word(word: &str) -> Option<Keyword> {
        Keyword::ALL
            .iter()
            .copied()
            .find(|k| k.as_str().eq_ignore_ascii_case(word))
    }
}

/// What a [`Token`] is.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TokenKind {
    /// A plain identifier; its name is the token text.
    Ident,
    /// A back-quoted identifier: the unescaped name.
    QuotedIdent(String),
    /// A keyword; the token text keeps the spelling as written.
    Keyword(Keyword),
    /// `$name`: the parameter name (unescaped when back-quoted).
    Parameter(String),
    /// An integer literal, at most 2^63 (the parser folds a preceding minus and refuses 2^63
    /// without it).
    Integer(u64),
    /// A float literal (finite).
    Float(f64),
    /// A string literal: the unescaped content.
    Str(String),
    /// `(`
    LParen,
    /// `)`
    RParen,
    /// `[`
    LBracket,
    /// `]`
    RBracket,
    /// `{`
    LBrace,
    /// `}`
    RBrace,
    /// `:`
    Colon,
    /// `,`
    Comma,
    /// `.`
    Dot,
    /// `..`
    DotDot,
    /// `;`
    Semicolon,
    /// `|`
    Pipe,
    /// `&`
    Ampersand,
    /// `!`
    Bang,
    /// `%`
    Percent,
    /// `*`
    Star,
    /// `+`
    Plus,
    /// `-`
    Minus,
    /// `/`
    Slash,
    /// `=`
    Eq,
    /// `<>` or `!=` (the token text tells which).
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `||`
    Concat,
    /// `->`
    RightArrow,
    /// `<-`
    LeftArrow,
    /// `~`
    Tilde,
    /// `~>`
    TildeRight,
    /// `<~`
    TildeLeft,
    /// End of input.
    Eof,
}

impl TokenKind {
    /// A short description for error messages ("expected ...").
    pub(crate) fn describe(&self) -> &'static str {
        match self {
            TokenKind::Ident | TokenKind::QuotedIdent(_) => "an identifier",
            TokenKind::Keyword(k) => k.as_str(),
            TokenKind::Parameter(_) => "a parameter",
            TokenKind::Integer(_) => "an integer",
            TokenKind::Float(_) => "a float",
            TokenKind::Str(_) => "a string",
            TokenKind::LParen => "`(`",
            TokenKind::RParen => "`)`",
            TokenKind::LBracket => "`[`",
            TokenKind::RBracket => "`]`",
            TokenKind::LBrace => "`{`",
            TokenKind::RBrace => "`}`",
            TokenKind::Colon => "`:`",
            TokenKind::Comma => "`,`",
            TokenKind::Dot => "`.`",
            TokenKind::DotDot => "`..`",
            TokenKind::Semicolon => "`;`",
            TokenKind::Pipe => "`|`",
            TokenKind::Ampersand => "`&`",
            TokenKind::Bang => "`!`",
            TokenKind::Percent => "`%`",
            TokenKind::Star => "`*`",
            TokenKind::Plus => "`+`",
            TokenKind::Minus => "`-`",
            TokenKind::Slash => "`/`",
            TokenKind::Eq => "`=`",
            TokenKind::Ne => "`<>`",
            TokenKind::Lt => "`<`",
            TokenKind::Le => "`<=`",
            TokenKind::Gt => "`>`",
            TokenKind::Ge => "`>=`",
            TokenKind::Concat => "`||`",
            TokenKind::RightArrow => "`->`",
            TokenKind::LeftArrow => "`<-`",
            TokenKind::Tilde => "`~`",
            TokenKind::TildeRight => "`~>`",
            TokenKind::TildeLeft => "`<~`",
            TokenKind::Eof => "end of input",
        }
    }
}

/// A token: its kind, span and raw source text.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Token<'a> {
    /// The kind (and payload).
    pub(crate) kind: TokenKind,
    /// Where it stands in the source.
    pub(crate) span: Span,
    /// The raw source text (quotes and escapes included).
    pub(crate) text: &'a str,
}

impl Token<'_> {
    /// The name this token spells where a name can stand: a plain or back-quoted identifier, or
    /// a keyword as written (keywords are contextual).
    pub(crate) fn name(&self) -> Option<&str> {
        match &self.kind {
            TokenKind::Ident | TokenKind::Keyword(_) => Some(self.text),
            TokenKind::QuotedIdent(name) => Some(name),
            TokenKind::Parameter(_)
            | TokenKind::Integer(_)
            | TokenKind::Float(_)
            | TokenKind::Str(_)
            | TokenKind::LParen
            | TokenKind::RParen
            | TokenKind::LBracket
            | TokenKind::RBracket
            | TokenKind::LBrace
            | TokenKind::RBrace
            | TokenKind::Colon
            | TokenKind::Comma
            | TokenKind::Dot
            | TokenKind::DotDot
            | TokenKind::Semicolon
            | TokenKind::Pipe
            | TokenKind::Ampersand
            | TokenKind::Bang
            | TokenKind::Percent
            | TokenKind::Star
            | TokenKind::Plus
            | TokenKind::Minus
            | TokenKind::Slash
            | TokenKind::Eq
            | TokenKind::Ne
            | TokenKind::Lt
            | TokenKind::Le
            | TokenKind::Gt
            | TokenKind::Ge
            | TokenKind::Concat
            | TokenKind::RightArrow
            | TokenKind::LeftArrow
            | TokenKind::Tilde
            | TokenKind::TildeRight
            | TokenKind::TildeLeft
            | TokenKind::Eof => None,
        }
    }

    /// The keyword this token is, if any.
    pub(crate) fn keyword(&self) -> Option<Keyword> {
        match self.kind {
            TokenKind::Keyword(k) => Some(k),
            _ => None,
        }
    }
}

/// What went wrong while tokenizing.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum LexErrorKind {
    /// A character that starts no token.
    UnexpectedChar(char),
    /// A string literal without its closing quote (the span starts at the opening quote).
    UnterminatedString,
    /// A back-quoted identifier without its closing backtick.
    UnterminatedQuotedIdent,
    /// ``` `` ```: a back-quoted identifier with no name.
    EmptyQuotedIdent,
    /// `/*` without `*/`.
    UnterminatedComment,
    /// An escape sequence that is not one of the documented ones (the span covers it).
    InvalidEscape(String),
    /// A malformed number literal (glued to a letter, missing digits).
    InvalidNumber(String),
    /// A decimal integer with a leading zero (`007`).
    LeadingZero(String),
    /// An integer literal above 2^63.
    IntegerOutOfRange(String),
    /// A float literal that overflows to infinity.
    FloatOutOfRange(String),
    /// `$` not followed by a parameter name.
    MissingParameterName,
}

/// A tokenizer error: what and where.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LexError {
    /// What went wrong.
    pub(crate) kind: LexErrorKind,
    /// The offending text.
    pub(crate) span: Span,
}

impl LexError {
    /// The message, without position (the caller shows the span).
    pub(crate) fn message(&self) -> String {
        match &self.kind {
            LexErrorKind::UnexpectedChar(c) => format!("unexpected character `{c}`"),
            LexErrorKind::UnterminatedString => "unterminated string literal".into(),
            LexErrorKind::UnterminatedQuotedIdent => {
                "unterminated back-quoted identifier (missing closing `)".into()
            }
            LexErrorKind::EmptyQuotedIdent => "empty back-quoted identifier".into(),
            LexErrorKind::UnterminatedComment => "unterminated comment (missing `*/`)".into(),
            LexErrorKind::InvalidEscape(text) => format!(
                "invalid escape sequence `{text}` (valid: \\\\ \\' \\\" \\` \\n \\t \\r \\b \\f \
                 \\uXXXX \\UXXXXXXXX)"
            ),
            LexErrorKind::InvalidNumber(text) => format!("invalid number literal `{text}`"),
            LexErrorKind::LeadingZero(text) => format!(
                "integer literal `{text}` has a leading zero (write 0o... for an octal number)"
            ),
            LexErrorKind::IntegerOutOfRange(text) => format!(
                "integer literal `{text}` is out of range (the largest integer is {})",
                i64::MAX
            ),
            LexErrorKind::FloatOutOfRange(text) => {
                format!("float literal `{text}` is out of range")
            }
            LexErrorKind::MissingParameterName => "`$` must be followed by a parameter name".into(),
        }
    }
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} at line {}, column {}",
            self.message(),
            self.span.line,
            self.span.column
        )
    }
}

/// Splits `src` into tokens, ending with one [`TokenKind::Eof`].
pub(crate) fn tokenize(src: &str) -> Result<Vec<Token<'_>>, LexError> {
    Lexer {
        src,
        pos: 0,
        line: 1,
        column: 1,
        out: Vec::new(),
    }
    .run()
}

/// A position: byte offset, line, column.
#[derive(Clone, Copy)]
struct Mark {
    pos: usize,
    line: usize,
    column: usize,
}

struct Lexer<'a> {
    src: &'a str,
    pos: usize,
    line: usize,
    column: usize,
    out: Vec<Token<'a>>,
}

/// 2^63, the largest integer magnitude a literal may spell (`-9223372036854775808`).
const MAX_MAGNITUDE: u64 = 1 << 63;

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl<'a> Lexer<'a> {
    fn run(mut self) -> Result<Vec<Token<'a>>, LexError> {
        loop {
            self.skip_trivia()?;
            let mark = self.mark();
            let Some(c) = self.peek() else {
                self.push(TokenKind::Eof, mark);
                return Ok(self.out);
            };
            let kind = self.token(c, mark)?;
            self.push(kind, mark);
        }
    }

    fn peek(&self) -> Option<char> {
        self.peek_nth(0)
    }

    fn peek_nth(&self, n: usize) -> Option<char> {
        self.src.get(self.pos..)?.chars().nth(n)
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos = self.pos.saturating_add(c.len_utf8());
        if c == '\n' {
            self.line = self.line.saturating_add(1);
            self.column = 1;
        } else {
            self.column = self.column.saturating_add(1);
        }
        Some(c)
    }

    /// Consumes `c` when it is the next character.
    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn mark(&self) -> Mark {
        Mark {
            pos: self.pos,
            line: self.line,
            column: self.column,
        }
    }

    fn span_from(&self, mark: Mark) -> Span {
        Span {
            start: mark.pos,
            end: self.pos,
            line: mark.line,
            column: mark.column,
        }
    }

    fn text_from(&self, mark: Mark) -> &'a str {
        self.src.get(mark.pos..self.pos).unwrap_or("")
    }

    fn error(&self, kind: LexErrorKind, mark: Mark) -> LexError {
        LexError {
            kind,
            span: self.span_from(mark),
        }
    }

    fn push(&mut self, kind: TokenKind, mark: Mark) {
        let span = self.span_from(mark);
        let text = self.text_from(mark);
        self.out.push(Token { kind, span, text });
    }

    /// Skips whitespace and comments.
    fn skip_trivia(&mut self) -> Result<(), LexError> {
        loop {
            match (self.peek(), self.peek_nth(1)) {
                (Some(c), _) if c.is_whitespace() => {
                    self.bump();
                }
                (Some('/'), Some('/')) => self.skip_line(),
                (Some('-'), Some('-')) if self.dash_dash_is_comment() => self.skip_line(),
                (Some('/'), Some('*')) => {
                    let mark = self.mark();
                    self.bump();
                    self.bump();
                    loop {
                        match self.bump() {
                            Some('*') if self.peek() == Some('/') => {
                                self.bump();
                                break;
                            }
                            Some(_) => {}
                            None => {
                                return Err(self.error(LexErrorKind::UnterminatedComment, mark));
                            }
                        }
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    fn skip_line(&mut self) {
        while let Some(c) = self.peek() {
            if c == '\n' {
                return;
            }
            self.bump();
        }
    }

    /// The `--` rule of the module docs: at `--`, whether it starts a line comment.
    fn dash_dash_is_comment(&self) -> bool {
        if matches!(self.peek_nth(2), Some('>' | '(')) {
            return false;
        }
        let after_node = self
            .out
            .last()
            .is_some_and(|t| t.kind == TokenKind::RParen && t.span.line == self.line);
        !after_node
    }

    /// Scans the token starting with `c` at `mark`.
    fn token(&mut self, c: char, mark: Mark) -> Result<TokenKind, LexError> {
        if c.is_ascii_digit() || (c == '.' && self.peek_nth(1).is_some_and(|d| d.is_ascii_digit()))
        {
            return self.number(mark);
        }
        if is_ident_start(c) {
            while self.peek().is_some_and(is_ident_continue) {
                self.bump();
            }
            let text = self.text_from(mark);
            return Ok(match Keyword::from_word(text) {
                Some(k) => TokenKind::Keyword(k),
                None => TokenKind::Ident,
            });
        }
        self.bump();
        let kind = match c {
            '\'' | '"' => TokenKind::Str(self.string(c, mark)?),
            '`' => TokenKind::QuotedIdent(self.quoted_ident(mark)?),
            '$' => TokenKind::Parameter(self.parameter(mark)?),
            '(' => TokenKind::LParen,
            ')' => TokenKind::RParen,
            '[' => TokenKind::LBracket,
            ']' => TokenKind::RBracket,
            '{' => TokenKind::LBrace,
            '}' => TokenKind::RBrace,
            ':' => TokenKind::Colon,
            ',' => TokenKind::Comma,
            ';' => TokenKind::Semicolon,
            '&' => TokenKind::Ampersand,
            '%' => TokenKind::Percent,
            '*' => TokenKind::Star,
            '+' => TokenKind::Plus,
            '/' => TokenKind::Slash,
            '=' => TokenKind::Eq,
            '.' if self.eat('.') => TokenKind::DotDot,
            '.' => TokenKind::Dot,
            '|' if self.eat('|') => TokenKind::Concat,
            '|' => TokenKind::Pipe,
            '!' if self.eat('=') => TokenKind::Ne,
            '!' => TokenKind::Bang,
            '-' if self.eat('>') => TokenKind::RightArrow,
            '-' => TokenKind::Minus,
            '~' if self.eat('>') => TokenKind::TildeRight,
            '~' => TokenKind::Tilde,
            '>' if self.eat('=') => TokenKind::Ge,
            '>' => TokenKind::Gt,
            '<' => match self.peek() {
                Some('=') => {
                    self.bump();
                    TokenKind::Le
                }
                Some('>') => {
                    self.bump();
                    TokenKind::Ne
                }
                Some('-') => {
                    self.bump();
                    TokenKind::LeftArrow
                }
                Some('~') => {
                    self.bump();
                    TokenKind::TildeLeft
                }
                _ => TokenKind::Lt,
            },
            other => return Err(self.error(LexErrorKind::UnexpectedChar(other), mark)),
        };
        Ok(kind)
    }

    /// Consumes digits of `radix`, with `_` allowed between two digits.
    fn digits(&mut self, radix: u32) {
        while let Some(c) = self.peek() {
            if c.is_digit(radix) {
                self.bump();
            } else if c == '_' && self.peek_nth(1).is_some_and(|d| d.is_digit(radix)) {
                self.bump();
            } else {
                return;
            }
        }
    }

    /// A number literal starting at `mark` (a digit, or `.` before a digit).
    fn number(&mut self, mark: Mark) -> Result<TokenKind, LexError> {
        let radix = match (self.peek(), self.peek_nth(1)) {
            (Some('0'), Some('x' | 'X')) => 16,
            (Some('0'), Some('o' | 'O')) => 8,
            (Some('0'), Some('b' | 'B')) => 2,
            _ => 10,
        };
        if radix != 10 {
            self.bump();
            self.bump();
            let digits_mark = self.mark();
            self.digits(radix);
            let digits: String = self.text_from(digits_mark).replace('_', "");
            self.reject_glued(mark)?;
            if digits.is_empty() {
                let text = self.text_from(mark).to_string();
                return Err(self.error(LexErrorKind::InvalidNumber(text), mark));
            }
            return self.integer(&digits, radix, mark);
        }
        let mut is_float = false;
        self.digits(10);
        if self.peek() == Some('.') && self.peek_nth(1).is_some_and(|d| d.is_ascii_digit()) {
            is_float = true;
            self.bump();
            self.digits(10);
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            let digit_at = if matches!(self.peek_nth(1), Some('+' | '-')) {
                2
            } else {
                1
            };
            if self.peek_nth(digit_at).is_some_and(|d| d.is_ascii_digit()) {
                is_float = true;
                for _ in 0..digit_at {
                    self.bump();
                }
                self.digits(10);
            }
        }
        self.reject_glued(mark)?;
        let text = self.text_from(mark);
        let clean = text.replace('_', "");
        if is_float {
            return match clean.parse::<f64>() {
                Ok(v) if v.is_finite() => Ok(TokenKind::Float(v)),
                Ok(_) => Err(self.error(LexErrorKind::FloatOutOfRange(text.into()), mark)),
                Err(_) => Err(self.error(LexErrorKind::InvalidNumber(text.into()), mark)),
            };
        }
        if clean.len() > 1 && clean.starts_with('0') {
            return Err(self.error(LexErrorKind::LeadingZero(text.into()), mark));
        }
        self.integer(&clean, 10, mark)
    }

    /// A number directly followed by an identifier character is one malformed literal.
    fn reject_glued(&mut self, mark: Mark) -> Result<(), LexError> {
        if !self.peek().is_some_and(is_ident_continue) {
            return Ok(());
        }
        while self.peek().is_some_and(is_ident_continue) {
            self.bump();
        }
        let text = self.text_from(mark).to_string();
        Err(self.error(LexErrorKind::InvalidNumber(text), mark))
    }

    fn integer(&self, digits: &str, radix: u32, mark: Mark) -> Result<TokenKind, LexError> {
        let text = self.text_from(mark);
        match u64::from_str_radix(digits, radix) {
            Ok(v) if v <= MAX_MAGNITUDE => Ok(TokenKind::Integer(v)),
            Ok(_) => Err(self.error(LexErrorKind::IntegerOutOfRange(text.into()), mark)),
            // Only digits of `radix` reach here, so the one failure left is overflow.
            Err(_) => Err(self.error(LexErrorKind::IntegerOutOfRange(text.into()), mark)),
        }
    }

    /// The rest of a string after its opening `quote` (already consumed).
    fn string(&mut self, quote: char, mark: Mark) -> Result<String, LexError> {
        let mut content = String::new();
        loop {
            let at = self.mark();
            match self.bump() {
                None => return Err(self.error(LexErrorKind::UnterminatedString, mark)),
                Some(c) if c == quote => {
                    if self.eat(quote) {
                        content.push(quote);
                    } else {
                        return Ok(content);
                    }
                }
                Some('\\') => content.push(self.escape(at)?),
                Some(c) => content.push(c),
            }
        }
    }

    /// An escape sequence after its `\` (consumed, at `at`).
    fn escape(&mut self, at: Mark) -> Result<char, LexError> {
        let invalid = |lexer: &Self| {
            let text = lexer.text_from(at).to_string();
            lexer.error(LexErrorKind::InvalidEscape(text), at)
        };
        let Some(c) = self.bump() else {
            return Err(invalid(self));
        };
        let simple = match c {
            '\\' => Some('\\'),
            '\'' => Some('\''),
            '"' => Some('"'),
            '`' => Some('`'),
            'n' => Some('\n'),
            't' => Some('\t'),
            'r' => Some('\r'),
            'b' => Some('\u{8}'),
            'f' => Some('\u{c}'),
            _ => None,
        };
        if let Some(e) = simple {
            return Ok(e);
        }
        let width = match c {
            'u' => 4,
            'U' => 8,
            _ => return Err(invalid(self)),
        };
        let mut value: u32 = 0;
        for _ in 0..width {
            match self.peek().and_then(|d| d.to_digit(16)) {
                Some(d) => {
                    self.bump();
                    value = value.saturating_mul(16).saturating_add(d);
                }
                None => return Err(invalid(self)),
            }
        }
        char::from_u32(value).ok_or_else(|| invalid(self))
    }

    /// The rest of a back-quoted identifier after its opening backtick.
    fn quoted_ident(&mut self, mark: Mark) -> Result<String, LexError> {
        let mut name = String::new();
        loop {
            match self.bump() {
                None => return Err(self.error(LexErrorKind::UnterminatedQuotedIdent, mark)),
                Some('`') => {
                    if self.eat('`') {
                        name.push('`');
                    } else if name.is_empty() {
                        return Err(self.error(LexErrorKind::EmptyQuotedIdent, mark));
                    } else {
                        return Ok(name);
                    }
                }
                Some(c) => name.push(c),
            }
        }
    }

    /// The name after `$` (consumed).
    fn parameter(&mut self, mark: Mark) -> Result<String, LexError> {
        if self.eat('`') {
            return self.quoted_ident(mark);
        }
        let name_mark = self.mark();
        while self.peek().is_some_and(is_ident_continue) {
            self.bump();
        }
        let name = self.text_from(name_mark);
        if name.is_empty() {
            return Err(self.error(LexErrorKind::MissingParameterName, mark));
        }
        Ok(name.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<TokenKind> {
        match tokenize(src) {
            Ok(tokens) => tokens.into_iter().map(|t| t.kind).collect(),
            Err(e) => panic!("{src:?}: {e}"),
        }
    }

    fn err(src: &str) -> LexError {
        match tokenize(src) {
            Ok(tokens) => panic!("{src:?} tokenized: {tokens:?}"),
            Err(e) => e,
        }
    }

    use TokenKind as K;

    #[test]
    fn simple_query_tokens_and_spans() {
        let src = "MATCH (a:person)\n  WHERE a.age >= 30\nRETURN a.name AS n";
        let tokens = tokenize(src).unwrap();
        let shown: Vec<(&str, usize, usize)> = tokens
            .iter()
            .map(|t| (t.text, t.span.line, t.span.column))
            .collect();
        assert_eq!(
            shown,
            vec![
                ("MATCH", 1, 1),
                ("(", 1, 7),
                ("a", 1, 8),
                (":", 1, 9),
                ("person", 1, 10),
                (")", 1, 16),
                ("WHERE", 2, 3),
                ("a", 2, 9),
                (".", 2, 10),
                ("age", 2, 11),
                (">=", 2, 15),
                ("30", 2, 18),
                ("RETURN", 3, 1),
                ("a", 3, 8),
                (".", 3, 9),
                ("name", 3, 10),
                ("AS", 3, 15),
                ("n", 3, 18),
                ("", 3, 19),
            ]
        );
        assert_eq!(tokens[0].kind, K::Keyword(Keyword::Match));
        assert_eq!(tokens[11].kind, K::Integer(30));
        assert_eq!(tokens.last().map(|t| &t.kind), Some(&K::Eof));
        for t in &tokens {
            assert_eq!(src.get(t.span.start..t.span.end), Some(t.text));
        }
    }

    #[test]
    fn columns_count_characters_not_bytes() {
        let tokens = tokenize("RETURN 'äöü', x").unwrap();
        assert_eq!(tokens[1].kind, K::Str("äöü".into()));
        assert_eq!((tokens[2].span.column, tokens[2].span.start), (13, 15));
        assert_eq!((tokens[3].span.column, tokens[3].span.start), (15, 17));
    }

    #[test]
    fn keywords_are_case_insensitive_and_keep_their_text() {
        for word in ["match", "MATCH", "Match", "mAtCh"] {
            let tokens = tokenize(word).unwrap();
            assert_eq!(tokens[0].keyword(), Some(Keyword::Match));
            assert_eq!(tokens[0].name(), Some(word));
        }
        assert_eq!(Keyword::from_word("starts"), Some(Keyword::Starts));
        assert_eq!(Keyword::from_word("labels"), None);
        assert_eq!(Keyword::from_word("type"), None);
        assert_eq!(Keyword::from_word("MATCHX"), None);
        assert_eq!(kinds("type")[0], K::Ident);
        // Every keyword spells itself back.
        for k in Keyword::ALL {
            assert_eq!(Keyword::from_word(&k.as_str().to_lowercase()), Some(*k));
            assert_eq!(K::Keyword(*k).describe(), k.as_str());
        }
    }

    #[test]
    fn identifiers_and_back_quoted_identifiers() {
        let tokens = tokenize("_a1 Ärger `my name` `a``b` `MATCH`").unwrap();
        assert_eq!(tokens[0].kind, K::Ident);
        assert_eq!(tokens[1].name(), Some("Ärger"));
        assert_eq!(tokens[2].kind, K::QuotedIdent("my name".into()));
        assert_eq!(tokens[3].name(), Some("a`b"));
        assert_eq!(tokens[3].text, "`a``b`");
        assert_eq!(tokens[4].kind, K::QuotedIdent("MATCH".into()));
        assert_eq!(tokens[4].keyword(), None);
        assert_eq!(tokens[5].name(), None);
        let e = err("RETURN `abc");
        assert_eq!(e.kind, LexErrorKind::UnterminatedQuotedIdent);
        assert_eq!((e.span.start, e.span.column), (7, 8));
        assert_eq!(err("RETURN ``").kind, LexErrorKind::EmptyQuotedIdent);
    }

    #[test]
    fn parameters() {
        assert_eq!(
            kinds("$name $0 $`a b`"),
            vec![
                K::Parameter("name".into()),
                K::Parameter("0".into()),
                K::Parameter("a b".into()),
                K::Eof
            ]
        );
        let e = err("WHERE a = $ ");
        assert_eq!(e.kind, LexErrorKind::MissingParameterName);
        assert_eq!(e.span.column, 11);
    }

    #[test]
    fn string_escapes() {
        let cases = [
            (r#"'plain'"#, "plain"),
            (r#""dq""#, "dq"),
            (r#"'it''s'"#, "it's"),
            (r#""say ""hi""""#, "say \"hi\""),
            (r#"'a\'b\"c\\d\`e'"#, "a'b\"c\\d`e"),
            (r#"'\n\t\r\b\f'"#, "\n\t\r\u{8}\u{c}"),
            (r#"'ä\U0001F600'"#, "ä😀"),
            ("'two\nlines'", "two\nlines"),
            ("'-- not a comment // either'", "-- not a comment // either"),
        ];
        for (src, want) in cases {
            assert_eq!(kinds(src), vec![K::Str(want.into()), K::Eof], "{src}");
        }
        let e = err(r"RETURN 'a\qb'");
        assert_eq!(e.kind, LexErrorKind::InvalidEscape(r"\q".into()));
        assert_eq!((e.span.start, e.span.end), (9, 11));
        assert!(matches!(
            err(r"'\u12'").kind,
            LexErrorKind::InvalidEscape(_)
        ));
        assert!(matches!(
            err(r"'\uD800'").kind,
            LexErrorKind::InvalidEscape(_)
        ));
        assert!(matches!(
            err(r"'\UFFFFFFFF'").kind,
            LexErrorKind::InvalidEscape(_)
        ));
        assert!(matches!(err("'\\").kind, LexErrorKind::InvalidEscape(_)));
    }

    #[test]
    fn unterminated_string_points_at_the_opening_quote() {
        let e = err("MATCH (a)\nWHERE a.name = 'bob\nRETURN a");
        assert_eq!(e.kind, LexErrorKind::UnterminatedString);
        assert_eq!((e.span.line, e.span.column, e.span.start), (2, 16, 25));
        assert!(e.to_string().contains("line 2, column 16"));
        assert_eq!(err("\"abc").kind, LexErrorKind::UnterminatedString);
    }

    #[test]
    fn numbers() {
        assert_eq!(
            kinds("0 42 1_000 0x1F 0o17 0b101 1.5 .5 1e3 2.5E-2 1E+2"),
            vec![
                K::Integer(0),
                K::Integer(42),
                K::Integer(1000),
                K::Integer(31),
                K::Integer(15),
                K::Integer(5),
                K::Float(1.5),
                K::Float(0.5),
                K::Float(1000.0),
                K::Float(0.025),
                K::Float(100.0),
                K::Eof,
            ]
        );
        assert_eq!(
            kinds("1..3"),
            vec![K::Integer(1), K::DotDot, K::Integer(3), K::Eof]
        );
        assert_eq!(kinds("1.x"), vec![K::Integer(1), K::Dot, K::Ident, K::Eof]);
        // `.5x` is a float glued to a letter.
        assert_eq!(err("n.5x").kind, LexErrorKind::InvalidNumber(".5x".into()));
    }

    #[test]
    fn number_errors() {
        let max = "9223372036854775807";
        assert_eq!(kinds(max)[0], K::Integer(i64::MAX as u64));
        assert_eq!(kinds("9223372036854775808")[0], K::Integer(1 << 63));
        let e = err("RETURN 9223372036854775809");
        assert_eq!(
            e.kind,
            LexErrorKind::IntegerOutOfRange("9223372036854775809".into())
        );
        assert_eq!((e.span.start, e.span.end, e.span.column), (7, 26, 8));
        assert!(matches!(
            err("99999999999999999999999999").kind,
            LexErrorKind::IntegerOutOfRange(_)
        ));
        assert!(matches!(
            err("0xFFFFFFFFFFFFFFFFFF").kind,
            LexErrorKind::IntegerOutOfRange(_)
        ));
        assert!(matches!(
            err("1e999").kind,
            LexErrorKind::FloatOutOfRange(_)
        ));
        assert_eq!(err("007").kind, LexErrorKind::LeadingZero("007".into()));
        assert_eq!(err("1abc").kind, LexErrorKind::InvalidNumber("1abc".into()));
        assert_eq!(err("1e").kind, LexErrorKind::InvalidNumber("1e".into()));
        assert_eq!(err("1_").kind, LexErrorKind::InvalidNumber("1_".into()));
        assert_eq!(err("0x").kind, LexErrorKind::InvalidNumber("0x".into()));
        assert_eq!(err("0b12").kind, LexErrorKind::InvalidNumber("0b12".into()));
    }

    #[test]
    fn comments_of_all_kinds() {
        let src = "MATCH // line\n(a) /* block\n spanning */ RETURN a -- dash\n-- own line\n";
        assert_eq!(
            kinds(src),
            vec![
                K::Keyword(Keyword::Match),
                K::LParen,
                K::Ident,
                K::RParen,
                K::Keyword(Keyword::Return),
                K::Ident,
                K::Eof
            ]
        );
        let tokens = tokenize(src).unwrap();
        assert_eq!((tokens[4].span.line, tokens[4].span.column), (3, 14));
        let e = err("RETURN 1 /* open\n comment");
        assert_eq!(e.kind, LexErrorKind::UnterminatedComment);
        assert_eq!((e.span.start, e.span.column, e.span.end), (9, 10, 25));
        assert_eq!(kinds("/**/1/*/ */"), vec![K::Integer(1), K::Eof]);
        assert_eq!(kinds("// only"), vec![K::Eof]);
        assert_eq!(kinds("--"), vec![K::Eof]);
    }

    #[test]
    fn dash_dash_rule() {
        use K::{LParen as L, Minus as M, RParen as R, RightArrow as RA};
        let node = |v: &mut Vec<TokenKind>| v.extend([L, K::Ident, R]);
        // `(a)--(b)` and `(a) -- (b)`: an edge.
        for src in [
            "(a)--(b)",
            "(a) -- (b)",
            "(a)\t--\t(b)",
            "(a) /* c */ -- (b)",
        ] {
            let mut want = Vec::new();
            node(&mut want);
            want.extend([M, M]);
            node(&mut want);
            want.push(K::Eof);
            assert_eq!(kinds(src), want, "{src}");
        }
        // `-->` in every spacing.
        for src in ["(a)-->(b)", "(a) --> (b)", "(a)\n-->(b)"] {
            let mut want = Vec::new();
            node(&mut want);
            want.extend([M, RA]);
            node(&mut want);
            want.push(K::Eof);
            assert_eq!(kinds(src), want, "{src}");
        }
        // `--(` after a line break.
        assert_eq!(
            kinds("(a)\n--(b)"),
            vec![L, K::Ident, R, M, M, L, K::Ident, R, K::Eof]
        );
        // Comments: own line, after a value, after a keyword.
        assert_eq!(
            kinds("MATCH (a)\n-- the people\nRETURN a"),
            vec![
                K::Keyword(Keyword::Match),
                L,
                K::Ident,
                R,
                K::Keyword(Keyword::Return),
                K::Ident,
                K::Eof
            ]
        );
        assert_eq!(kinds("RETURN x -- note"), kinds("RETURN x"));
        assert_eq!(kinds("RETURN a--1"), kinds("RETURN a"));
        assert_eq!(kinds("MATCH -- (a)"), kinds("MATCH"));
        // After `)` on the same line: an edge, even when meant as a comment (fails loudly later).
        assert_eq!(
            kinds("(a) -- note"),
            vec![L, K::Ident, R, M, M, K::Ident, K::Eof]
        );
        // Single minus stays arithmetic.
        assert_eq!(kinds("a - -1"), vec![K::Ident, M, M, K::Integer(1), K::Eof]);
    }

    #[test]
    fn arrows_and_edge_forms() {
        use K::{LBracket as LB, LeftArrow as LA, Minus as M, RBracket as RB, RightArrow as RA};
        let edge = |src: &str| -> Vec<TokenKind> {
            kinds(src)
                .into_iter()
                .filter(|k| !matches!(k, K::LParen | K::RParen | K::Ident | K::Eof))
                .collect()
        };
        assert_eq!(edge("(a)-[e]->(b)"), vec![M, LB, RB, RA]);
        assert_eq!(edge("(a)<-[e]-(b)"), vec![LA, LB, RB, M]);
        assert_eq!(edge("(a)-[e]-(b)"), vec![M, LB, RB, M]);
        assert_eq!(edge("(a)-[e:T]->(b)"), vec![M, LB, K::Colon, RB, RA]);
        assert_eq!(edge("(a)-->(b)"), vec![M, RA]);
        assert_eq!(edge("(a)<--(b)"), vec![LA, M]);
        assert_eq!(edge("(a)<-->(b)"), vec![LA, RA]);
        assert_eq!(edge("(a)->(b)"), vec![RA]);
        assert_eq!(edge("(a)<-(b)"), vec![LA]);
        assert_eq!(edge("(a)~[e]~(b)"), vec![K::Tilde, LB, RB, K::Tilde]);
        assert_eq!(
            edge("(a)<~[e]~>(b)"),
            vec![K::TildeLeft, LB, RB, K::TildeRight]
        );
        // Adjacency is visible through the spans.
        let tokens = tokenize("(a)-->(b) - ->").unwrap();
        assert!(tokens[3].span.touches(tokens[4].span));
        assert!(!tokens[8].span.touches(tokens[9].span));
        assert_eq!(tokens[3].span.to(tokens[4].span).end, 6);
    }

    #[test]
    fn operators_and_punctuation() {
        assert_eq!(
            kinds("{ } [ ] : , . .. ; | & ! % * + / = <> != < <= > >= || -"),
            vec![
                K::LBrace,
                K::RBrace,
                K::LBracket,
                K::RBracket,
                K::Colon,
                K::Comma,
                K::Dot,
                K::DotDot,
                K::Semicolon,
                K::Pipe,
                K::Ampersand,
                K::Bang,
                K::Percent,
                K::Star,
                K::Plus,
                K::Slash,
                K::Eq,
                K::Ne,
                K::Ne,
                K::Lt,
                K::Le,
                K::Gt,
                K::Ge,
                K::Concat,
                K::Minus,
                K::Eof,
            ]
        );
        let tokens = tokenize("a<>b!=c").unwrap();
        assert_eq!((tokens[1].text, tokens[3].text), ("<>", "!="));
        let e = err("RETURN 1 ^ 2");
        assert_eq!(e.kind, LexErrorKind::UnexpectedChar('^'));
        assert_eq!((e.span.start, e.span.end), (9, 10));
        assert!(e.message().contains('^'));
        assert_eq!(err("€").span.end, 3);
        for kind in kinds("{ } [ ] : , . .. ; | & ! % * + / = <> < <= > >= || - -> <- ~ ~> <~") {
            assert!(!kind.describe().is_empty());
        }
    }

    #[test]
    fn every_error_has_a_message() {
        for src in [
            "^",
            "'a",
            "`a",
            "``",
            "/*",
            r"'\q'",
            "1x",
            "007",
            "99999999999999999999",
            "1e999",
            "$",
        ] {
            let e = err(src);
            assert!(!e.message().is_empty(), "{src}");
            assert!(src.get(e.span.start..e.span.end).is_some(), "{src}");
        }
    }

    /// Line and column of byte offset `at`, computed independently of the lexer.
    fn line_column(src: &str, at: usize) -> (usize, usize) {
        let before = src.get(..at).unwrap_or("");
        let line = before.matches('\n').count() + 1;
        let line_start = before.rfind('\n').map_or(0, |i| i + 1);
        let column = before.get(line_start..).unwrap_or("").chars().count() + 1;
        (line, column)
    }

    /// Checks the span invariants of a lexer result.
    fn check(src: &str) {
        match tokenize(src) {
            Ok(tokens) => {
                assert_eq!(tokens.last().map(|t| &t.kind), Some(&K::Eof), "{src:?}");
                let mut prev_end = 0;
                for t in &tokens {
                    assert!(
                        t.span.start >= prev_end && t.span.end >= t.span.start,
                        "{src:?}"
                    );
                    assert_eq!(src.get(t.span.start..t.span.end), Some(t.text), "{src:?}");
                    assert_eq!(
                        (t.span.line, t.span.column),
                        line_column(src, t.span.start),
                        "{src:?}"
                    );
                    prev_end = t.span.end;
                }
            }
            Err(e) => {
                assert!(src.get(e.span.start..e.span.end).is_some(), "{src:?}");
                assert_eq!(
                    (e.span.line, e.span.column),
                    line_column(src, e.span.start),
                    "{src:?}"
                );
                assert!(!e.message().is_empty());
            }
        }
    }

    /// A deterministic xorshift generator (no dependency).
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n.max(1) as u64) as usize
        }
    }

    #[test]
    fn never_panics_on_mutated_or_random_input() {
        const SEEDS: &[&str] = &[
            "MATCH (a:person {name: 'marko'})-[e:knows]->(b) WHERE b.age > 30 RETURN a, b.name",
            "MATCH (a)--(b), (b)<-[:created]-(c:A&!B|%) RETURN DISTINCT * ORDER BY a.x DESC",
            "MATCH (n WHERE n.v IN [1, 2.5e3, 0x1F, $p, `q``x`]) RETURN CASE n.v WHEN 1 THEN \
             'a\\n' ELSE \"b\" END AS c /* c */ OFFSET 1 LIMIT 2 -- end",
            "RETURN 'äöü' || \"€\" // x\n, -9223372036854775808, 1..2, ~> <~ ~",
        ];
        const ALPHABET: &[char] = &[
            '(', ')', '[', ']', '{', '}', '-', '>', '<', '~', '/', '*', '\'', '"', '`', '\\', '$',
            '.', '0', '9', 'e', 'x', '_', ' ', '\n', '\t', 'ä', '€', '😀', '|', '&', '!', '=', ':',
            ',', 'u', 'U',
        ];
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let mut runs = 0;
        for seed in SEEDS {
            check(seed);
            for _ in 0..150 {
                let mut chars: Vec<char> = seed.chars().collect();
                for _ in 0..1 + rng.below(4) {
                    let at = rng.below(chars.len() + 1);
                    let c = ALPHABET[rng.below(ALPHABET.len())];
                    match rng.below(3) {
                        0 if at < chars.len() => {
                            chars.remove(at);
                        }
                        1 if at < chars.len() => chars[at] = c,
                        _ => chars.insert(at, c),
                    }
                }
                let mutated: String = chars.into_iter().collect();
                check(&mutated);
                // Every prefix (cut at a char boundary) as well, for unterminated forms.
                let cut = rng.below(mutated.len() + 1);
                if let Some(prefix) = mutated.get(..cut) {
                    check(prefix);
                }
                runs += 1;
            }
        }
        for _ in 0..300 {
            let len = rng.below(40);
            let random: String = (0..len)
                .map(|_| ALPHABET[rng.below(ALPHABET.len())])
                .collect();
            check(&random);
            runs += 1;
        }
        assert_eq!(runs, 900);
    }
}
