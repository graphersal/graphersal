//! The HTTP boundary without an HTTP library: a host converts each request of its own server
//! (`tiny_http` in the CLI's dev server) into a [`Request`], hands it to
//! [`crate::handle`], and writes the [`Response`] back. Like `graphersal-mcp`'s
//! `McpRequest`/`McpResponse`, these types carry only what the API reads and answers.

use std::fmt;
use std::io::Read;

use serde_json::Value;

/// Largest request body the server reads (scripts, schemas, patches).
pub const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// The method of a request.
///
/// `#[non_exhaustive]`: methods may be named by their own variant later; every other method is
/// [`Method::Other`] with its name as the client sent it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Method {
    /// `GET`.
    Get,
    /// `HEAD`.
    Head,
    /// `POST`.
    Post,
    /// `PUT`.
    Put,
    /// `DELETE`.
    Delete,
    /// `OPTIONS`.
    Options,
    /// `PATCH`.
    Patch,
    /// Any other method, by its name.
    Other(String),
}

impl Method {
    /// The method of the name a request line carries (`GET`, `POST`, ...). Method names are
    /// case-sensitive (RFC 9110): `get` is [`Method::Other`].
    pub fn parse(name: &str) -> Self {
        match name {
            "GET" => Self::Get,
            "HEAD" => Self::Head,
            "POST" => Self::Post,
            "PUT" => Self::Put,
            "DELETE" => Self::Delete,
            "OPTIONS" => Self::Options,
            "PATCH" => Self::Patch,
            _ => Self::Other(name.to_string()),
        }
    }

    /// The method's name (`GET`, `POST`, ...).
    pub fn as_str(&self) -> &str {
        match self {
            Self::Get => "GET",
            Self::Head => "HEAD",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
            Self::Options => "OPTIONS",
            Self::Patch => "PATCH",
            Self::Other(name) => name,
        }
    }
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One HTTP request: method, request target (path and query), headers, the declared body
/// length and the body.
pub struct Request<'a> {
    method: Method,
    url: String,
    headers: Vec<(String, String)>,
    body_length: Option<usize>,
    body: Box<dyn Read + 'a>,
    identity: Option<String>,
}

impl<'a> Request<'a> {
    /// A request of `method` for the request target `url` (`/api/info?x=1`) with `body`, no
    /// headers and no declared body length yet.
    pub fn new(method: Method, url: &str, body: impl Read + 'a) -> Self {
        Self {
            method,
            url: url.to_string(),
            headers: Vec::new(),
            body_length: None,
            body: Box::new(body),
            identity: None,
        }
    }

    /// The request with one more header.
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    /// The request with the body length its `Content-Length` declares (`None`: not declared).
    pub fn with_body_length(mut self, length: Option<usize>) -> Self {
        self.body_length = length;
        self
    }

    /// The method.
    pub fn method(&self) -> &Method {
        &self.method
    }

    /// The request target as sent (path and query).
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The value of the first header `name` (ASCII case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(field, _)| field.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The headers in the order they were added.
    pub fn headers(&self) -> &[(String, String)] {
        &self.headers
    }

    /// The declared body length, if any.
    pub fn body_length(&self) -> Option<usize> {
        self.body_length
    }

    /// Attaches `identity` to the request: who sent it, as a request filter decided
    /// ([`crate::RequestFilter`]; an opaque text of the host, e.g. a user or token name). The
    /// extra routes read it here, a run observer from [`crate::RunInfo::identity`]. The core
    /// itself never reads it.
    pub fn set_identity(&mut self, identity: impl Into<String>) {
        self.identity = Some(identity.into());
    }

    /// The identity a request filter attached ([`Self::set_identity`]), if any.
    pub fn identity(&self) -> Option<&str> {
        self.identity.as_deref()
    }

    /// The body.
    pub fn body(&mut self) -> &mut (dyn Read + 'a) {
        &mut *self.body
    }

    /// Reads the body (at most [`MAX_BODY_BYTES`]) of a `Content-Type: application/json`
    /// request.
    pub fn text_body(&mut self) -> Result<String, HttpError> {
        let json_type = self
            .header("Content-Type")
            .map(|t| {
                t.trim_start()
                    .to_ascii_lowercase()
                    .starts_with("application/json")
            })
            .unwrap_or(false);
        if !json_type {
            return Err(HttpError(
                415,
                "send the body as Content-Type: application/json".into(),
            ));
        }
        if self.body_length.is_some_and(|len| len > MAX_BODY_BYTES) {
            return Err(HttpError(
                413,
                format!("the body is larger than {MAX_BODY_BYTES} bytes"),
            ));
        }
        let mut bytes = Vec::new();
        self.body
            .as_mut()
            .take(MAX_BODY_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|err| HttpError::bad_request(format!("cannot read the body: {err}")))?;
        if bytes.len() > MAX_BODY_BYTES {
            return Err(HttpError(
                413,
                format!("the body is larger than {MAX_BODY_BYTES} bytes"),
            ));
        }
        String::from_utf8(bytes).map_err(|_| HttpError::bad_request("the body is not UTF-8"))
    }

    /// [`Self::text_body`] parsed as a JSON object.
    pub fn json_body(&mut self) -> Result<Value, HttpError> {
        let text = self.text_body()?;
        let body: Value = serde_json::from_str(&text)
            .map_err(|err| HttpError::bad_request(format!("malformed JSON body: {err}")))?;
        if !body.is_object() {
            return Err(HttpError::bad_request("the body must be a JSON object"));
        }
        Ok(body)
    }
}

/// The answer to a [`Request`]: status, headers in order, body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Response {
    /// An answer of `status` with `body` and no headers.
    pub fn new(status: u16, body: Vec<u8>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body,
        }
    }

    /// A JSON text answer (`Content-Type: application/json; charset=utf-8`, `Cache-Control:
    /// no-store`, `X-Content-Type-Options: nosniff`).
    pub fn text(status: u16, text: String) -> Self {
        Self::new(status, text.into_bytes())
            .with_header("Content-Type", "application/json; charset=utf-8")
            .with_header("Cache-Control", "no-store")
            .with_header("X-Content-Type-Options", "nosniff")
    }

    /// [`Self::text`] of a JSON value.
    pub fn json(status: u16, body: &Value) -> Self {
        Self::text(status, body.to_string())
    }

    /// The answer with one more header.
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    /// The status code.
    pub fn status(&self) -> u16 {
        self.status
    }

    /// The headers in order.
    pub fn headers(&self) -> &[(String, String)] {
        &self.headers
    }

    /// The body.
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// Status, headers and body, to write the answer.
    pub fn into_parts(self) -> (u16, Vec<(String, String)>, Vec<u8>) {
        (self.status, self.headers, self.body)
    }
}

/// A failed request: status and message, answered as `{"error": message}` (a `503` also with
/// `Retry-After: 1`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpError(pub(crate) u16, pub(crate) String);

impl HttpError {
    /// A failure of `status` with `message`.
    pub fn new(status: u16, message: impl Into<String>) -> Self {
        Self(status, message.into())
    }

    /// A `400` with `message`.
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self(400, message.into())
    }

    /// The status code.
    pub fn status(&self) -> u16 {
        self.0
    }

    /// The message.
    pub fn message(&self) -> &str {
        &self.1
    }
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.0, self.1)
    }
}

impl std::error::Error for HttpError {}
