//! [`SessionError`]: the error of every session method that does not answer in-band JSON.
//!
//! The JSON wire methods ([`Session::execute`](crate::Session::execute) and the other methods
//! that answer `{"ok": false, "error": ..}`) keep their in-band errors; every other method that
//! touches the graph, a Store or a file returns `Result<_, SessionError>` (or the typed error of
//! the graph or Store it forwards), even where it cannot fail today, so a new failure is never a
//! breaking change. `Display` is the text the front ends show (the playground, the dev server,
//! the wasm build): the diagnostic of the failed operation, with its `Help:` line where the
//! engine gives one.

use graphersal::error::GraphError;
use graphersal::persist::PersistError;

/// Why a session operation failed. `#[non_exhaustive]`: new kinds are added without a breaking
/// change; match the ones a front end treats specially and show the rest with `Display`.
#[derive(Debug)]
#[non_exhaustive]
pub enum SessionError {
    /// The graph refused the operation: a view the current thread may not have
    /// ([`GraphError::GraphViewHeld`]), an operation the storage does not support, a unit
    /// error. `Display` is the error's message.
    Graph(GraphError),
    /// A Store operation failed. `Display` is the error's message.
    Persist(PersistError),
    /// The request cannot be served as asked: malformed JSON, an unknown name, kind or job, a
    /// value out of range. The text says what and how to fix it.
    Invalid(String),
    /// The engine operation failed: the text is its diagnostic (the message, and the `Help:`
    /// line where the engine gives one).
    Failed(String),
}

impl SessionError {
    /// A request the session cannot serve ([`SessionError::Invalid`]).
    pub fn invalid(text: impl Into<String>) -> Self {
        SessionError::Invalid(text.into())
    }

    /// A failed engine operation with its diagnostic ([`SessionError::Failed`]).
    pub fn failed(text: impl Into<String>) -> Self {
        SessionError::Failed(text.into())
    }
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::Graph(err) => write!(f, "{err}"),
            SessionError::Persist(err) => write!(f, "{err}"),
            SessionError::Invalid(text) | SessionError::Failed(text) => f.write_str(text),
        }
    }
}

impl std::error::Error for SessionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SessionError::Graph(err) => Some(err),
            SessionError::Persist(err) => Some(err),
            SessionError::Invalid(_) | SessionError::Failed(_) => None,
        }
    }
}

impl From<GraphError> for SessionError {
    fn from(err: GraphError) -> Self {
        SessionError::Graph(err)
    }
}

impl From<PersistError> for SessionError {
    fn from(err: PersistError) -> Self {
        SessionError::Persist(err)
    }
}

/// A diagnostic text of a failed operation ([`SessionError::Failed`]).
impl From<String> for SessionError {
    fn from(text: String) -> Self {
        SessionError::Failed(text)
    }
}

/// A diagnostic text of a failed operation ([`SessionError::Failed`]).
impl From<&str> for SessionError {
    fn from(text: &str) -> Self {
        SessionError::Failed(text.to_string())
    }
}

/// Serialized as its text ([`Display`](std::fmt::Display)): the JSON answers carry errors as
/// strings.
impl serde::Serialize for SessionError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

/// The text a front end shows ([`Display`](std::fmt::Display)), for the JSON answers that carry
/// errors as strings.
impl From<SessionError> for String {
    fn from(err: SessionError) -> Self {
        err.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_keeps_the_text_of_every_kind() {
        assert_eq!(SessionError::invalid("bad").to_string(), "bad");
        assert_eq!(SessionError::failed("worse").to_string(), "worse");
        let view = SessionError::from(GraphError::unsupported("stores"));
        assert_eq!(
            view.to_string(),
            GraphError::unsupported("stores").to_string()
        );
        assert!(std::error::Error::source(&view).is_some());
        assert_eq!(String::from(SessionError::from("x")), "x");
    }
}
