//! Where the dev server listens (`--host`, `--port`) and which `Host`/`Origin` headers it
//! answers.
//!
//! Without `--host` the server binds the loopback (`127.0.0.1`, plus `[::1]` on the same port
//! when the system has IPv6), except inside a container ([`in_container`]: `/.dockerenv`,
//! Podman's `/run/.containerenv`, or a Kubernetes pod's `KUBERNETES_SERVICE_HOST`), where it binds `0.0.0.0` so `docker run -p 8080:8080 ...`
//! works without flags. `--host ADDR` binds the address given (an IP address or a host name,
//! resolved once; `localhost` is the loopback default).
//!
//! Header checks (DNS rebinding and cross-site requests):
//! - Loopback ([`Exposure::Loopback`]): `Host` must name this server exactly
//!   (`127.0.0.1:<port>`, `localhost:<port>`, `[::1]:<port>`, or the `--host` name with the
//!   port), and an `Origin` must be one of them, as before `--host` existed.
//! - Network ([`Exposure::Network`], a non-loopback bind): the port of `Host` is not checked
//!   (a container's port mapping changes it), and its name must be an IP address, `localhost`
//!   or a `*.localhost` name, a single-label name without a dot (a machine name, a Compose
//!   service name), or the `--host` name itself. A fully qualified name other than `--host` is
//!   refused: that is what a DNS rebinding attack sends. An `Origin` must be the request's own
//!   origin (`http://` + its `Host`): a page of another site is refused even when it names an
//!   IP address.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::path::Path;

/// The address the server binds without `--host` outside a container.
const LOOPBACK: Ipv4Addr = Ipv4Addr::LOCALHOST;

/// Whether this process runs inside a container: Docker creates `/.dockerenv`, Podman
/// `/run/.containerenv`.
pub(super) fn in_container() -> bool {
    container_detected(
        Path::new("/.dockerenv").exists(),
        Path::new("/run/.containerenv").exists(),
    ) || kubernetes_detected(std::env::var_os("KUBERNETES_SERVICE_HOST").as_deref())
}

/// A Kubernetes pod (containerd, CRI-O create neither container file): the kubelet sets
/// `KUBERNETES_SERVICE_HOST` in every container of a pod.
fn kubernetes_detected(service_host: Option<&std::ffi::OsStr>) -> bool {
    service_host.is_some_and(|host| !host.is_empty())
}

/// The container decision from the two probes (kept apart from the file system for tests).
fn container_detected(dockerenv: bool, containerenv: bool) -> bool {
    dockerenv || containerenv
}

/// The address the server binds when `--host` is not given.
fn default_host(in_container: bool) -> IpAddr {
    if in_container {
        IpAddr::V4(Ipv4Addr::UNSPECIFIED)
    } else {
        IpAddr::V4(LOOPBACK)
    }
}

/// Whether the server is reachable from other machines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Exposure {
    /// A loopback address: only this machine reaches it.
    Loopback,
    /// Any other address: reachable from the network, without authentication.
    Network,
}

/// Why the server binds the address it binds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Reason {
    /// No `--host`, not in a container.
    Default,
    /// No `--host`, in a container: `0.0.0.0`.
    Container,
    /// `--host`.
    Explicit,
}

/// The bind decision: the address, the exposure, and the name a browser opens it with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BindPlan {
    /// The address the main socket binds (its port is `--port`).
    pub(super) ip: IpAddr,
    /// Also bind `[::1]` on the same port (the loopback default, when available).
    pub(super) also_v6_loopback: bool,
    pub(super) exposure: Exposure,
    pub(super) reason: Reason,
    /// The `--host` value when it is a name (not an IP address): requests may name it.
    name: Option<String>,
}

/// Parses or resolves a `--host` value: an IP address (IPv6 with or without brackets) or a host
/// name; the error text is the CLI's usage error.
fn resolve_host(raw: &str) -> Result<IpAddr, String> {
    let host = raw.trim();
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if let Ok(ip) = bare.parse::<IpAddr>() {
        return Ok(ip);
    }
    let failed = |why: String| {
        format!(
            "Invalid --host value: {raw}: {why}. Expected an IP address (0.0.0.0 for every \
             interface, :: for every IPv6 interface, 127.0.0.1 for this machine only) or a host \
             name that resolves on this machine"
        )
    };
    if host.is_empty() || host.contains(['/', ' ', ':']) {
        return Err(failed("not an address or host name".into()));
    }
    let addrs: Vec<SocketAddr> = (host, 0)
        .to_socket_addrs()
        .map_err(|err| failed(err.to_string()))?
        .collect();
    // An IPv4 address first: the default loopback URL and most clients use IPv4.
    addrs
        .iter()
        .find(|a| a.is_ipv4())
        .or_else(|| addrs.first())
        .map(|a| a.ip())
        .ok_or_else(|| failed("it resolves to no address".into()))
}

impl BindPlan {
    /// The plan for `--host` (`None`: the default) on a machine that is (or is not) a container.
    pub(super) fn new(host: Option<&str>, in_container: bool) -> Result<Self, String> {
        let Some(raw) = host else {
            let ip = default_host(in_container);
            let reason = if in_container {
                Reason::Container
            } else {
                Reason::Default
            };
            return Ok(Self::for_ip(ip, reason, None));
        };
        if raw.trim().eq_ignore_ascii_case("localhost") {
            return Ok(Self::for_ip(IpAddr::V4(LOOPBACK), Reason::Explicit, None));
        }
        let ip = resolve_host(raw)?;
        let trimmed = raw.trim();
        let name = trimmed
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
            .is_err()
            .then(|| trimmed.to_ascii_lowercase());
        Ok(Self::for_ip(ip, Reason::Explicit, name))
    }

    fn for_ip(ip: IpAddr, reason: Reason, name: Option<String>) -> Self {
        let exposure = if ip.is_loopback() {
            Exposure::Loopback
        } else {
            Exposure::Network
        };
        Self {
            ip,
            also_v6_loopback: ip == IpAddr::V4(LOOPBACK) && name.is_none(),
            exposure,
            reason,
            name,
        }
    }

    /// Whether the bind address is a wildcard (`0.0.0.0`, `::`): every interface.
    pub(super) fn is_wildcard(&self) -> bool {
        self.ip.is_unspecified()
    }

    /// The host part of the URL a browser on this machine opens: the loopback for a wildcard,
    /// else the `--host` name or address.
    pub(super) fn url_host(&self) -> String {
        match (self.ip, &self.name) {
            (_, Some(name)) => name.clone(),
            (IpAddr::V4(ip), None) if ip.is_unspecified() => LOOPBACK.to_string(),
            (IpAddr::V6(ip), None) if ip.is_unspecified() => format!("[{}]", Ipv6Addr::LOCALHOST),
            (IpAddr::V4(ip), None) => ip.to_string(),
            (IpAddr::V6(ip), None) => format!("[{ip}]"),
        }
    }

    /// The URL of the UI on `port`.
    pub(super) fn url(&self, port: u16) -> String {
        format!("http://{}:{port}/", self.url_host())
    }

    /// The bound address as `ip:port` (IPv6 in brackets).
    pub(super) fn address(&self, port: u16) -> String {
        SocketAddr::new(self.ip, port).to_string()
    }

    /// The one-line stderr notice of a server reachable from the network, or `None`.
    pub(super) fn network_notice(&self, port: u16) -> Option<String> {
        if self.exposure == Exposure::Loopback {
            return None;
        }
        let why = match self.reason {
            Reason::Container => " (container detected: the default --host is 0.0.0.0)",
            Reason::Default | Reason::Explicit => "",
        };
        Some(format!(
            "warning: the dev server listens on {}{why} and is reachable from the network: it has \
             NO authentication, anyone who reaches this port can read and change the graph (and \
             use the AI Chat); use --host 127.0.0.1 for this machine only",
            self.address(port)
        ))
    }

    /// Whether a request naming `host` (a `Host` header, or the host part of an `Origin`) is
    /// served by this server on `port` (module docs).
    pub(super) fn is_own_host(&self, host: &str, port: u16) -> bool {
        match self.exposure {
            Exposure::Loopback => {
                let port = port.to_string();
                ["127.0.0.1", "localhost", "[::1]"]
                    .into_iter()
                    .chain(self.name.as_deref())
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
                    || self.name.as_deref() == Some(name.as_str())
            }
        }
    }

    /// Whether an `Origin` header is allowed for a request whose `Host` is `host`.
    pub(super) fn is_own_origin(&self, origin: &str, host: &str, port: u16) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kubernetes_pod_counts_as_a_container() {
        assert!(!kubernetes_detected(None));
        assert!(!kubernetes_detected(Some(std::ffi::OsStr::new(""))));
        assert!(kubernetes_detected(Some(std::ffi::OsStr::new("10.96.0.1"))));
    }

    #[test]
    fn the_container_probes_decide_the_default_host() {
        assert!(!container_detected(false, false));
        assert!(container_detected(true, false));
        assert!(container_detected(false, true));
        assert!(container_detected(true, true));
        let normal = BindPlan::new(None, false).unwrap();
        assert_eq!(normal.ip, IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(normal.exposure, Exposure::Loopback);
        assert_eq!(normal.reason, Reason::Default);
        assert!(normal.also_v6_loopback);
        assert_eq!(normal.network_notice(8080), None);
        assert_eq!(normal.url(8080), "http://127.0.0.1:8080/");
        let container = BindPlan::new(None, true).unwrap();
        assert_eq!(container.ip, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        assert_eq!(container.exposure, Exposure::Network);
        assert_eq!(container.reason, Reason::Container);
        assert!(!container.also_v6_loopback);
        assert_eq!(container.url(8080), "http://127.0.0.1:8080/");
        let notice = container.network_notice(8080).unwrap();
        assert!(notice.contains("0.0.0.0:8080"), "{notice}");
        assert!(notice.contains("container detected"), "{notice}");
        assert!(notice.contains("NO authentication"), "{notice}");
        // An explicit --host wins over the container default.
        let explicit = BindPlan::new(Some("127.0.0.1"), true).unwrap();
        assert_eq!(explicit.exposure, Exposure::Loopback);
        assert_eq!(explicit.network_notice(8080), None);
    }

    #[test]
    fn explicit_hosts_and_their_urls() {
        let all = BindPlan::new(Some("0.0.0.0"), false).unwrap();
        assert!(all.is_wildcard());
        assert_eq!(all.url(9000), "http://127.0.0.1:9000/");
        assert!(!all.network_notice(9000).unwrap().contains("container"));
        let v6 = BindPlan::new(Some("::"), false).unwrap();
        assert!(v6.is_wildcard());
        assert_eq!(v6.url(9000), "http://[::1]:9000/");
        assert_eq!(v6.address(9000), "[::]:9000");
        let lan = BindPlan::new(Some("192.168.1.5"), false).unwrap();
        assert_eq!(lan.exposure, Exposure::Network);
        assert_eq!(lan.url(80), "http://192.168.1.5:80/");
        let bracketed = BindPlan::new(Some("[::1]"), false).unwrap();
        assert_eq!(bracketed.exposure, Exposure::Loopback);
        assert_eq!(bracketed.url(80), "http://[::1]:80/");
        let localhost = BindPlan::new(Some("LocalHost"), false).unwrap();
        assert_eq!(localhost.ip, IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert!(localhost.also_v6_loopback);
        for bad in ["nonsense..", "", "a b", "http://x", "1.2.3.4:80"] {
            let err = BindPlan::new(Some(bad), false).unwrap_err();
            assert!(err.starts_with("Invalid --host value"), "{bad}: {err}");
        }
    }

    #[test]
    fn loopback_accepts_only_its_own_names_and_port() {
        let plan = BindPlan::new(None, false).unwrap();
        for ok in [
            "127.0.0.1:8080",
            "localhost:8080",
            "LOCALHOST:8080",
            "[::1]:8080",
        ] {
            assert!(plan.is_own_host(ok, 8080), "{ok}");
        }
        for bad in [
            "127.0.0.1:9000",
            "192.168.1.5:8080",
            "evil.example:8080",
            "graphersal:8080",
            "127.0.0.1",
        ] {
            assert!(!plan.is_own_host(bad, 8080), "{bad}");
        }
        assert!(plan.is_own_origin("http://localhost:8080", "127.0.0.1:8080", 8080));
        assert!(!plan.is_own_origin("http://evil.example", "127.0.0.1:8080", 8080));
    }

    #[test]
    fn network_accepts_addresses_and_single_label_names_on_any_port() {
        let plan = BindPlan::new(Some("0.0.0.0"), false).unwrap();
        for ok in [
            "127.0.0.1:8080",
            "localhost:9000",
            "192.168.1.5:8080",
            "10.0.0.2",
            "[fe80::1]:8080",
            "[::1]",
            "graphersal:8080",
            "app.localhost:8080",
        ] {
            assert!(plan.is_own_host(ok, 8080), "{ok}");
        }
        for bad in [
            "evil.example:8080",
            "rebind.attacker.com",
            "",
            ":8080",
            "[::1",
            "host:port",
        ] {
            assert!(!plan.is_own_host(bad, 8080), "{bad}");
        }
        // A fully qualified --host name is accepted (and only that one).
        let named = BindPlan {
            name: Some("graph.example.org".into()),
            ..plan.clone()
        };
        assert!(named.is_own_host("Graph.Example.org:8080", 8080));
        assert!(!named.is_own_host("other.example.org:8080", 8080));
        // Origin: the request's own origin only.
        assert!(plan.is_own_origin("http://192.168.1.5:8080", "192.168.1.5:8080", 8080));
        assert!(!plan.is_own_origin("http://192.168.1.9:8080", "192.168.1.5:8080", 8080));
        assert!(!plan.is_own_origin("https://192.168.1.5:8080", "192.168.1.5:8080", 8080));
        assert!(!plan.is_own_origin("null", "192.168.1.5:8080", 8080));
    }
}
