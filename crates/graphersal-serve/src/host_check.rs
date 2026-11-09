//! Which `Host`/`Origin` headers a server answers (DNS rebinding and cross-site requests). The
//! host decides where it listens (the CLI's `--host`, `--port`); this module only checks the
//! headers of a request against that decision.
//!
//! - Loopback ([`Exposure::Loopback`]): `Host` must name this server exactly
//!   (`127.0.0.1:<port>`, `localhost:<port>`, `[::1]:<port>`, or the host name it was started
//!   with, with the port), and an `Origin` must be one of them.
//! - Network ([`Exposure::Network`], a non-loopback bind): the port of `Host` is not checked
//!   (a container's port mapping changes it), and its name must be an IP address, `localhost`
//!   or a `*.localhost` name, a single-label name without a dot (a machine name, a Compose
//!   service name), or the host name the server was started with. A fully qualified name other
//!   than that is refused: that is what a DNS rebinding attack sends. An `Origin` must be the
//!   request's own origin (`http://` + its `Host`): a page of another site is refused even when
//!   it names an IP address.
//! - Allowed host names (the CLI's `--allowed-host`, `GRAPHERSAL_ALLOWED_HOSTS`) are admitted in
//!   both exposures, on any port: a `Host` naming one of them is served, and an `Origin` of
//!   `http://` or `https://` one of them is accepted whatever the `Host` (a reverse proxy that
//!   ends TLS may forward the public name or one of its own). The operator vouches for them, so
//!   the DNS-rebinding guard does not apply to them.

use std::net::IpAddr;

use crate::http::{HttpError, Request};

/// Whether the server is reachable from other machines.
///
/// Closed by definition (not `#[non_exhaustive]`): a bind address is either a loopback address
/// or not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exposure {
    /// A loopback address: only this machine reaches it.
    Loopback,
    /// Any other address: reachable from the network, without authentication.
    Network,
}

/// The header rules of a bind decision, borrowed from the host's own record of it.
#[derive(Debug, Clone, Copy)]
pub struct HostRules<'a> {
    exposure: Exposure,
    name: Option<&'a str>,
    allowed: &'a [String],
}

impl<'a> HostRules<'a> {
    /// The rules of a server with `exposure`, started with the host name `name` (lower case;
    /// `None` when it was started with an IP address or the default), admitting the further
    /// names `allowed` (lower case, IPv6 in brackets).
    pub fn new(exposure: Exposure, name: Option<&'a str>, allowed: &'a [String]) -> Self {
        Self {
            exposure,
            name,
            allowed,
        }
    }

    /// Whether the name part of `host` (`name`, `name:port`, `[v6]:port`) is an allowed name.
    fn is_allowed_name(&self, host: &str) -> bool {
        if self.allowed.is_empty() {
            return false;
        }
        let name = host_name(host).trim_end_matches('.').to_ascii_lowercase();
        !name.is_empty() && self.allowed.contains(&name)
    }

    /// Whether a request naming `host` (a `Host` header, or the host part of an `Origin`) is
    /// served by this server on `port` (module docs).
    pub fn is_own_host(&self, host: &str, port: u16) -> bool {
        if self.is_allowed_name(host) {
            return true;
        }
        match self.exposure {
            Exposure::Loopback => {
                let port = port.to_string();
                ["127.0.0.1", "localhost", "[::1]"]
                    .into_iter()
                    .chain(self.name)
                    .any(|name| host.eq_ignore_ascii_case(&format!("{name}:{port}")))
            }
            Exposure::Network => {
                let name = host_name(host).to_ascii_lowercase();
                if name.is_empty() {
                    return false;
                }
                let bare = name
                    .strip_prefix('[')
                    .and_then(|n| n.strip_suffix(']'))
                    .unwrap_or(&name);
                bare.parse::<IpAddr>().is_ok()
                    || name == "localhost"
                    || name.ends_with(".localhost")
                    || !name.contains('.')
                    || self.name == Some(name.as_str())
            }
        }
    }

    /// Whether an `Origin` header is allowed for a request whose `Host` is `host`.
    pub fn is_own_origin(&self, origin: &str, host: &str, port: u16) -> bool {
        // An allowed name: http or https (TLS ended at a reverse proxy), any port, any Host.
        let allowed_origin = origin
            .strip_prefix("https://")
            .or_else(|| origin.strip_prefix("http://"))
            .map(|h| h.trim_end_matches('/'))
            .is_some_and(|h| !h.contains('/') && self.is_allowed_name(h));
        if allowed_origin {
            return true;
        }
        let Some(origin_host) = origin
            .strip_prefix("http://")
            .map(|h| h.trim_end_matches('/'))
        else {
            return false;
        };
        match self.exposure {
            Exposure::Loopback => self.is_own_host(origin_host, port),
            // Same origin only: the page was served by this server under this very name.
            Exposure::Network => origin_host.eq_ignore_ascii_case(host),
        }
    }
}

/// The header checks of one listening server: its [`HostRules`], its port, and the URL a
/// browser on this machine opens it with (named in the answer to an unknown `Host`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostCheck {
    exposure: Exposure,
    name: Option<String>,
    allowed: Vec<String>,
    port: u16,
    url: String,
}

impl HostCheck {
    /// The checks of a server with `exposure` on `port`, opened at `url`
    /// (`http://127.0.0.1:<port>/`), without a host name or allowed names yet.
    pub fn new(exposure: Exposure, port: u16, url: &str) -> Self {
        Self {
            exposure,
            name: None,
            allowed: Vec::new(),
            port,
            url: url.to_string(),
        }
    }

    /// The checks with the host name the server was started with (lower case).
    pub fn with_name(mut self, name: Option<String>) -> Self {
        self.name = name;
        self
    }

    /// The checks with further allowed host names (lower case, IPv6 in brackets).
    pub fn with_allowed_hosts(mut self, allowed: Vec<String>) -> Self {
        self.allowed = allowed;
        self
    }

    /// The rules these checks apply.
    pub fn rules(&self) -> HostRules<'_> {
        HostRules::new(self.exposure, self.name.as_deref(), &self.allowed)
    }

    /// Whether a request naming `host` is served ([`HostRules::is_own_host`] on this port).
    pub fn is_own_host(&self, host: &str) -> bool {
        self.rules().is_own_host(host, self.port)
    }

    /// Whether `origin` is allowed for a request whose `Host` is `host`
    /// ([`HostRules::is_own_origin`] on this port).
    pub fn is_own_origin(&self, origin: &str, host: &str) -> bool {
        self.rules().is_own_origin(origin, host, self.port)
    }

    /// The checks of one request: `Ok` when its `Host` names this server and its `Origin`, if
    /// any, is allowed; else the `403` that refuses it (DNS rebinding, a cross-site page). A
    /// context runs it on every request before the host's [`crate::RequestFilter`].
    pub fn check(&self, request: &Request<'_>) -> Result<(), HttpError> {
        let host = match request.header("Host") {
            Some(host) if self.is_own_host(host) => host,
            _ => return Err(HttpError(403, self.unknown_host_message())),
        };
        if let Some(origin) = request.header("Origin")
            && !self.is_own_origin(origin, host)
        {
            return Err(HttpError(
                403,
                format!("cross-origin request from {origin} refused"),
            ));
        }
        Ok(())
    }

    /// The message of the `403` to a request whose `Host` is not this server.
    pub(crate) fn unknown_host_message(&self) -> String {
        match self.exposure {
            Exposure::Loopback => format!(
                "unknown Host: use {} (a public name behind a reverse proxy: start \
                 the server with --allowed-host <that name>)",
                self.url
            ),
            Exposure::Network => "unknown Host: open this server by its IP \
                address, localhost or a name without a dot, or start it with \
                --host <that name> (or --allowed-host <that name> behind a reverse \
                proxy)"
                .to_string(),
        }
    }
}

/// The name part of a `Host` value (`name`, `name:port`, `[v6]`, `[v6]:port`), brackets kept.
fn host_name(host: &str) -> &str {
    if host.starts_with('[') {
        return match host.find(']') {
            Some(end) => &host[..=end],
            None => "",
        };
    }
    match host.rsplit_once(':') {
        Some((name, port)) if port.chars().all(|c| c.is_ascii_digit()) => name,
        Some(_) => "",
        None => host,
    }
}
