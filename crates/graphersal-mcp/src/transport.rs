//! The transport boundary: one HTTP request to the endpoint ([`McpRequest`]) and its answer
//! ([`McpResponse`]), independent of the HTTP library the host uses.

use std::io::Read;

/// Largest request body the endpoint reads (the dev server's limit for every body).
pub(crate) const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// One HTTP request to the MCP endpoint: its method, its headers and its body. The host's HTTP
/// server builds it ([`McpRequest::new`], [`McpRequest::header`]) and answers with the
/// [`McpResponse`] of [`crate::Mcp::handle`]. The body is read only when the request needs it
/// (a `POST` that passed the header checks), at most 16 MiB of it.
pub struct McpRequest<'a> {
    method: String,
    headers: Vec<(String, String)>,
    body: Box<dyn Read + 'a>,
}

impl<'a> McpRequest<'a> {
    /// A request of the HTTP `method` (`POST`, `DELETE`, ...) with `body` and no headers yet.
    pub fn new(method: &str, body: impl Read + 'a) -> Self {
        Self {
            method: method.to_string(),
            headers: Vec::new(),
            body: Box::new(body),
        }
    }

    /// Adds the header `name: value` (names compare ASCII case-insensitively; the first one of
    /// a name counts).
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    /// The HTTP method.
    pub(crate) fn method(&self) -> &str {
        &self.method
    }

    /// The value of the first header `name`.
    pub(crate) fn header_value(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(field, _)| field.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// Reads the body (at most [`MAX_BODY_BYTES`]) of a `Content-Type: application/json`
    /// request: `Err` is the HTTP status and the message.
    pub(crate) fn text_body(&mut self) -> Result<String, (u16, String)> {
        let json_type = self
            .header_value("Content-Type")
            .map(|t| {
                t.trim_start()
                    .to_ascii_lowercase()
                    .starts_with("application/json")
            })
            .unwrap_or(false);
        if !json_type {
            return Err((
                415,
                "send the body as Content-Type: application/json".into(),
            ));
        }
        let declared = self
            .header_value("Content-Length")
            .and_then(|len| len.trim().parse::<usize>().ok());
        if declared.is_some_and(|len| len > MAX_BODY_BYTES) {
            return Err((
                413,
                format!("the body is larger than {MAX_BODY_BYTES} bytes"),
            ));
        }
        let mut bytes = Vec::new();
        (&mut self.body)
            .take(MAX_BODY_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|err| (400, format!("cannot read the body: {err}")))?;
        if bytes.len() > MAX_BODY_BYTES {
            return Err((
                413,
                format!("the body is larger than {MAX_BODY_BYTES} bytes"),
            ));
        }
        String::from_utf8(bytes).map_err(|_| (400, "the body is not UTF-8".to_string()))
    }
}

/// The answer to an [`McpRequest`]: an HTTP status, headers in order, and a body (JSON, or
/// empty).
#[derive(Debug, Clone)]
pub struct McpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl McpResponse {
    /// An answer of `status` with `body` and no headers.
    pub(crate) fn new(status: u16, body: Vec<u8>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body,
        }
    }

    /// Adds the header `name: value`.
    pub(crate) fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    /// The HTTP status.
    pub fn status(&self) -> u16 {
        self.status
    }

    /// The headers, in the order the host should send them.
    pub fn headers(&self) -> &[(String, String)] {
        &self.headers
    }

    /// The value of the first header `name` (ASCII case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(field, _)| field.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The body.
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// The status, the headers and the body.
    pub fn into_parts(self) -> (u16, Vec<(String, String)>, Vec<u8>) {
        (self.status, self.headers, self.body)
    }
}
