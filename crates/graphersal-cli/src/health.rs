//! `graphersal health`: the probe of a running dev server for images without curl or wget
//! (a Docker `HEALTHCHECK`, a Kubernetes `exec` probe). One HTTP/1.1 `GET /healthz` (or
//! `/readyz` with `--ready`) over `std::net`; exit 0 when it answers 200 within the timeout,
//! 1 otherwise (the reason on stderr), 2 for an invalid invocation. Nothing on stdout.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use crate::cli::{DEFAULT_PORT, EXIT_OK, EXIT_QUERY_ERROR, EXIT_USAGE_ERROR};

/// The usage of `graphersal health`.
pub const HEALTH_HELP: &str = "\
graphersal health - probe a running dev server (graphersal --server)

USAGE:
    graphersal health [--ready] [--host ADDR] [--port N] [--timeout MS]

Sends GET /healthz (liveness: the process answers HTTP) or, with --ready, GET /readyz
(readiness: the graph is loaded and the server is not stopping) and exits 0 when the answer
is 200 within the timeout, 1 otherwise (the reason on stderr). Prints nothing on stdout.
For images without curl or wget: HEALTHCHECK CMD [\"graphersal\", \"health\"].

OPTIONS:
        --ready          Probe /readyz instead of /healthz.
        --host <ADDR>    Address of the server; default 127.0.0.1.
        --port <N>       Port of the server; default 8080.
        --timeout <MS>   Time for connecting and answering, in milliseconds; default 2000.
    -h, --help           Print this help.
";

/// Default time a probe may take.
const DEFAULT_TIMEOUT_MS: u64 = 2000;

/// Largest answer the probe reads (the endpoints answer a few bytes of JSON).
const MAX_ANSWER_BYTES: u64 = 8192;

/// Runs `graphersal health <args>`; returns the exit code.
pub fn run(args: &[String]) -> i32 {
    let mut ready = false;
    let mut host = "127.0.0.1".to_string();
    let mut port = DEFAULT_PORT;
    let mut timeout_ms = DEFAULT_TIMEOUT_MS;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) if name.starts_with("--") => (name, Some(value.to_string())),
            _ => (arg, None),
        };
        let value = |i: &mut usize| -> Option<String> {
            if inline.is_some() {
                return inline.clone();
            }
            *i += 1;
            args.get(*i).cloned()
        };
        match name {
            "-h" | "--help" | "help" => {
                out!("{HEALTH_HELP}");
                return EXIT_OK;
            }
            "--ready" if inline.is_none() => ready = true,
            "--host" => match value(&mut i) {
                Some(v) if !v.is_empty() => host = v,
                _ => return usage("--host needs an address"),
            },
            "--port" => match value(&mut i).and_then(|v| v.parse::<u16>().ok()) {
                Some(v) if v > 0 => port = v,
                _ => return usage("--port needs a port number 1..65535"),
            },
            "--timeout" => match value(&mut i).and_then(|v| v.parse::<u64>().ok()) {
                Some(v) if v > 0 => timeout_ms = v,
                _ => return usage("--timeout needs a number of milliseconds >= 1"),
            },
            _ => return usage(&format!("unknown argument: {arg}")),
        }
        i += 1;
    }
    let path = if ready { "/readyz" } else { "/healthz" };
    match probe(&host, port, path, Duration::from_millis(timeout_ms)) {
        Ok(()) => EXIT_OK,
        Err(reason) => {
            errln!("unhealthy: {reason}");
            EXIT_QUERY_ERROR
        }
    }
}

fn usage(message: &str) -> i32 {
    errln!("Error: {message}\n\nRun `graphersal health --help` for usage.");
    EXIT_USAGE_ERROR
}

/// One `GET path` to `host:port`: `Ok` for a 200 answer within `timeout`, else the reason.
fn probe(host: &str, port: u16, path: &str, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    let target = if host.contains(':') {
        format!(
            "[{}]:{port}",
            host.trim_start_matches('[').trim_end_matches(']')
        )
    } else {
        format!("{host}:{port}")
    };
    let addresses: Vec<_> = (host.trim_start_matches('[').trim_end_matches(']'), port)
        .to_socket_addrs()
        .map_err(|err| format!("cannot resolve {target}: {err}"))?
        .collect();
    let mut last = format!("cannot resolve {target}");
    let mut stream = None;
    for address in addresses {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            last = format!("no answer from {target} within {} ms", timeout.as_millis());
            break;
        }
        match TcpStream::connect_timeout(&address, left) {
            Ok(connected) => {
                stream = Some(connected);
                break;
            }
            Err(err) => last = format!("nothing answers at {target} ({err})"),
        }
    }
    let mut stream = stream.ok_or(last)?;
    let left = deadline.saturating_duration_since(Instant::now());
    let left = left.max(Duration::from_millis(1));
    let timed_out = |err: std::io::Error| match err.kind() {
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => {
            format!("no answer from {target} within {} ms", timeout.as_millis())
        }
        _ => format!("the connection to {target} failed ({err})"),
    };
    stream
        .set_write_timeout(Some(left))
        .and_then(|()| stream.set_read_timeout(Some(left)))
        .map_err(timed_out)?;
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {target}\r\nUser-Agent: graphersal-health\r\n\
         Connection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).map_err(timed_out)?;
    let mut answer = Vec::new();
    stream
        .take(MAX_ANSWER_BYTES)
        .read_to_end(&mut answer)
        .map_err(timed_out)?;
    let text = String::from_utf8_lossy(&answer);
    let status_line = text.lines().next().unwrap_or_default();
    let status = status_line.split_whitespace().nth(1).unwrap_or_default();
    if status == "200" {
        return Ok(());
    }
    if status.is_empty() {
        return Err(format!("{target} gave no HTTP answer to GET {path}"));
    }
    let body = text
        .split_once("\r\n\r\n")
        .map_or("", |(_, body)| body.trim());
    Err(format!("GET {path} on {target} answered {status} {body}")
        .trim_end()
        .to_string())
}
