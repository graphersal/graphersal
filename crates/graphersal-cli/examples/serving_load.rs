//! Load test of the read-only serving mode (`graphersal --server --read-only --mcp`). Not part
//! of `cargo test`; run it in release against a running server:
//!
//! ```text
//! cargo build --release -p graphersal-cli --examples
//! target/release/graphersal --graph large-kb.gsnap --read-only --server --port 9500 \
//!     --mcp --mcp-read-only &
//! target/release/examples/serving_load --port 9500 --idle 1000 --active 200 --tabs 5 \
//!     --seconds 60 --pid <server pid>
//! ```
//!
//! It opens `--idle` MCP sessions that `initialize` and then keep their keep-alive connection
//! open without a request, then runs `--active` MCP clients (each its own session and
//! connection) issuing a mix of `run_saved_query`, `gql` and `query` calls for `--seconds`,
//! and `--tabs` playground pages polling `/api/changes` and running a query now and then. A
//! 503 is counted and retried after its `Retry-After`. With `--pid` it samples the server's
//! RSS, thread count and CPU time (`ps`). Prints one JSON summary on stdout.
//!
//! The calls assume a graph with the saved queries `load.by_level(level)` and
//! `load.children(id)` over vertices with the properties `id` and `level` (the `large`
//! sample exported to a `.gsnap` with those two queries defined: see the stage report).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

struct Options {
    port: u16,
    idle: usize,
    active: usize,
    tabs: usize,
    seconds: u64,
    pid: Option<u32>,
}

fn options() -> Options {
    let mut options = Options {
        port: 9500,
        idle: 1000,
        active: 200,
        tabs: 5,
        seconds: 60,
        pid: None,
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i + 1 < args.len() {
        let value = &args[i + 1];
        match args[i].as_str() {
            "--port" => options.port = value.parse().unwrap_or(options.port),
            "--idle" => options.idle = value.parse().unwrap_or(options.idle),
            "--active" => options.active = value.parse().unwrap_or(options.active),
            "--tabs" => options.tabs = value.parse().unwrap_or(options.tabs),
            "--seconds" => options.seconds = value.parse().unwrap_or(options.seconds),
            "--pid" => options.pid = value.parse().ok(),
            other => eprintln!("unknown option {other}"),
        }
        i += 2;
    }
    options
}

/// One keep-alive HTTP/1.1 connection.
struct Connection {
    port: u16,
    reader: BufReader<TcpStream>,
}

/// An answer: status, the `Retry-After` header, body.
struct Answer {
    status: u16,
    retry_after: Option<u64>,
    session: Option<String>,
    body: String,
}

impl Connection {
    fn open(port: u16) -> std::io::Result<Self> {
        let stream = TcpStream::connect(("127.0.0.1", port))?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(Duration::from_secs(120)))?;
        Ok(Self {
            port,
            reader: BufReader::new(stream),
        })
    }

    fn send(
        &mut self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> std::io::Result<Answer> {
        let mut head = format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Length: {}\r\n",
            self.port,
            body.len()
        );
        for (name, value) in headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str("\r\n");
        let stream = self.reader.get_mut();
        stream.write_all(head.as_bytes())?;
        stream.write_all(body.as_bytes())?;
        let mut line = String::new();
        self.reader.read_line(&mut line)?;
        let status = line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let (mut length, mut chunked, mut retry_after, mut session) = (0, false, None, None);
        loop {
            line.clear();
            self.reader.read_line(&mut line)?;
            let header = line.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some((name, value)) = header.split_once(':') {
                let value = value.trim();
                match name.to_ascii_lowercase().as_str() {
                    "content-length" => length = value.parse().unwrap_or(0),
                    "transfer-encoding" => chunked = value.eq_ignore_ascii_case("chunked"),
                    "retry-after" => retry_after = value.parse().ok(),
                    "mcp-session-id" => session = Some(value.to_string()),
                    _ => {}
                }
            }
        }
        let mut body = Vec::new();
        if chunked {
            loop {
                line.clear();
                self.reader.read_line(&mut line)?;
                let size = usize::from_str_radix(line.trim(), 16).unwrap_or(0);
                if size == 0 {
                    line.clear();
                    self.reader.read_line(&mut line)?;
                    break;
                }
                let start = body.len();
                body.resize(start + size, 0);
                self.reader.read_exact(&mut body[start..])?;
                line.clear();
                self.reader.read_line(&mut line)?;
            }
        } else {
            body.resize(length, 0);
            self.reader.read_exact(&mut body)?;
        }
        Ok(Answer {
            status,
            retry_after,
            session,
            body: String::from_utf8_lossy(&body).into_owned(),
        })
    }
}

const MCP_HEADERS: [(&str, &str); 2] = [
    ("Content-Type", "application/json"),
    ("Accept", "application/json, text/event-stream"),
];

/// `initialize` + `notifications/initialized` on `connection`; the session id.
fn initialize(connection: &mut Connection, name: &str) -> std::io::Result<String> {
    let body = json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {},
        "clientInfo": {"name": name, "version": "1"}}})
    .to_string();
    let answer = connection.send("POST", "/mcp", &MCP_HEADERS, &body)?;
    let session = answer.session.ok_or_else(|| {
        std::io::Error::other(format!(
            "no session id (status {}): {}",
            answer.status, answer.body
        ))
    })?;
    let mut headers = MCP_HEADERS.to_vec();
    headers.push(("Mcp-Session-Id", &session));
    headers.push(("MCP-Protocol-Version", "2025-06-18"));
    connection.send(
        "POST",
        "/mcp",
        &headers,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    )?;
    Ok(session)
}

/// Latencies (microseconds) and counts of one kind of call.
#[derive(Default)]
struct Kind {
    latencies: Vec<u64>,
    errors: u64,
    busy: u64,
}

#[derive(Default)]
struct Stats {
    kinds: std::collections::BTreeMap<String, Kind>,
}

impl Stats {
    fn record(&mut self, kind: &str, micros: u64, status: u16, tool_error: bool) {
        let entry = self.kinds.entry(kind.to_string()).or_default();
        match status {
            200 | 202 if !tool_error => entry.latencies.push(micros),
            503 => entry.busy += 1,
            _ => entry.errors += 1,
        }
    }
}

fn percentile(sorted: &[u64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let index = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[index] as f64 / 1000.0
}

/// The calls of an active client, in turn.
fn call_body(n: u64, id: u64) -> (&'static str, Value) {
    let level = (n % 5).to_string();
    let (kind, params) = match n % 4 {
        0 => (
            "run_saved_query",
            json!({"name": "run_saved_query", "arguments": {
                "name": "load.by_level", "params": {"level": level}}}),
        ),
        1 => (
            "run_saved_query",
            json!({"name": "run_saved_query", "arguments": {
                "name": "load.children", "params": {"id": format!("V_2_{}", n % 100)}}}),
        ),
        2 => (
            "gql",
            json!({"name": "gql", "arguments": {
                "query": format!("MATCH (v) WHERE v.level = '{level}' RETURN count(v) AS n")}}),
        ),
        _ => (
            "query",
            json!({"name": "query", "arguments": {
                "script": format!("g.v().has(\"level\", \"{level}\").limit(50).values(\"id\").fold()")}}),
        ),
    };
    (
        kind,
        json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": params}),
    )
}

/// One `ps` sample of the server: RSS (KiB), threads, CPU seconds so far.
fn sample(pid: u32) -> Option<(u64, u64, f64)> {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=,time=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut fields = text.split_whitespace();
    let rss = fields.next()?.parse().ok()?;
    let time = fields.next()?;
    // [[hh:]mm:]ss.cc
    let cpu = time
        .split(':')
        .fold(0.0, |acc, part| acc * 60.0 + part.parse::<f64>().unwrap_or(0.0));
    let threads = std::process::Command::new("ps")
        .args(["-M", "-p", &pid.to_string()])
        .output()
        .ok()
        .map(|out| String::from_utf8_lossy(&out.stdout).lines().count() as u64 - 1)
        .unwrap_or(0);
    Some((rss, threads, cpu))
}

fn main() {
    let options = options();
    let port = options.port;
    let stop = Arc::new(AtomicBool::new(false));
    let stats = Arc::new(Mutex::new(Stats::default()));
    let failures = Arc::new(AtomicU64::new(0));

    // Background sessions: initialize, then hold the connection idle.
    let started = Instant::now();
    let mut idle = Vec::new();
    let opened = Arc::new(AtomicU64::new(0));
    for i in 0..options.idle {
        let stop = stop.clone();
        let opened = opened.clone();
        let failures = failures.clone();
        let thread = std::thread::Builder::new()
            .stack_size(128 * 1024)
            .spawn(move || {
                let connected = Connection::open(port)
                    .and_then(|mut c| initialize(&mut c, &format!("idle-{}", i % 50)).map(|_| c));
                match connected {
                    Ok(connection) => {
                        opened.fetch_add(1, Ordering::SeqCst);
                        while !stop.load(Ordering::SeqCst) {
                            std::thread::sleep(Duration::from_millis(200));
                        }
                        drop(connection);
                    }
                    Err(err) => {
                        failures.fetch_add(1, Ordering::SeqCst);
                        eprintln!("idle {i}: {err}");
                    }
                }
            });
        match thread {
            Ok(thread) => idle.push(thread),
            Err(err) => eprintln!("idle thread {i}: {err}"),
        }
    }
    while opened.load(Ordering::SeqCst) + failures.load(Ordering::SeqCst)
        < options.idle as u64
    {
        std::thread::sleep(Duration::from_millis(50));
    }
    let idle_setup = started.elapsed();
    let before = options.pid.and_then(sample);

    // Active clients and pages.
    let deadline = Instant::now() + Duration::from_secs(options.seconds);
    let mut workers = Vec::new();
    for client in 0..options.active {
        let stats = stats.clone();
        let failures = failures.clone();
        workers.push(std::thread::spawn(move || {
            let Ok(mut connection) = Connection::open(port) else {
                failures.fetch_add(1, Ordering::SeqCst);
                return;
            };
            let session = match initialize(&mut connection, "active") {
                Ok(session) => session,
                Err(err) => {
                    eprintln!("active {client}: {err}");
                    failures.fetch_add(1, Ordering::SeqCst);
                    return;
                }
            };
            let mut headers = MCP_HEADERS.to_vec();
            headers.push(("Mcp-Session-Id", &session));
            headers.push(("MCP-Protocol-Version", "2025-06-18"));
            let mut n = client as u64;
            while Instant::now() < deadline {
                n += 1;
                let (kind, body) = call_body(n, n);
                let sent = Instant::now();
                match connection.send("POST", "/mcp", &headers, &body.to_string()) {
                    Ok(answer) => {
                        let tool_error = answer.status == 200
                            && serde_json::from_str::<Value>(&answer.body)
                                .map(|v| v["result"]["isError"] == true || v["error"].is_object())
                                .unwrap_or(true);
                        if tool_error && n % 997 == 0 {
                            eprintln!("tool error: {}", &answer.body[..answer.body.len().min(300)]);
                        }
                        stats.lock().unwrap().record(
                            kind,
                            sent.elapsed().as_micros() as u64,
                            answer.status,
                            tool_error,
                        );
                        if answer.status == 503 {
                            std::thread::sleep(Duration::from_secs(
                                answer.retry_after.unwrap_or(1),
                            ));
                        }
                    }
                    Err(err) => {
                        failures.fetch_add(1, Ordering::SeqCst);
                        eprintln!("active {client}: {err}");
                        match Connection::open(port) {
                            Ok(c) => connection = c,
                            Err(_) => return,
                        }
                    }
                }
            }
        }));
    }
    for tab in 0..options.tabs {
        let stats = stats.clone();
        let failures = failures.clone();
        workers.push(std::thread::spawn(move || {
            let Ok(mut connection) = Connection::open(port) else {
                failures.fetch_add(1, Ordering::SeqCst);
                return;
            };
            let client = format!("tab{tab}");
            let headers = [
                ("Content-Type", "application/json"),
                ("X-Graphersal-Client", client.as_str()),
            ];
            let mut n = 0u64;
            while Instant::now() < deadline {
                n += 1;
                let sent = Instant::now();
                let (kind, answer) = if n % 2 == 0 {
                    let body = json!({"script": format!(
                        "g.v().has(\"level\", \"{}\").limit(20).values(\"id\")", n % 5)})
                    .to_string();
                    ("page execute", connection.send("POST", "/api/execute", &headers, &body))
                } else {
                    ("page changes", connection.send("GET", "/api/changes", &headers, ""))
                };
                match answer {
                    Ok(answer) => {
                        let error = kind == "page execute"
                            && !answer.body.starts_with("{\"ok\":true");
                        stats.lock().unwrap().record(
                            kind,
                            sent.elapsed().as_micros() as u64,
                            answer.status,
                            error,
                        );
                    }
                    Err(_) => {
                        failures.fetch_add(1, Ordering::SeqCst);
                        match Connection::open(port) {
                            Ok(c) => connection = c,
                            Err(_) => return,
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }));
    }
    // Server samples while the load runs.
    let mut samples = Vec::new();
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_secs(5));
        if let Some(sample) = options.pid.and_then(sample) {
            samples.push(sample);
        }
    }
    for worker in workers {
        let _ = worker.join();
    }
    let after = options.pid.and_then(sample);
    stop.store(true, Ordering::SeqCst);
    for thread in idle {
        let _ = thread.join();
    }

    let stats = stats.lock().unwrap();
    let mut kinds = serde_json::Map::new();
    let mut total = 0usize;
    for (name, kind) in &stats.kinds {
        let mut sorted = kind.latencies.clone();
        sorted.sort_unstable();
        total += sorted.len();
        kinds.insert(
            name.clone(),
            json!({
                "ok": sorted.len(), "errors": kind.errors, "busy503": kind.busy,
                "p50ms": percentile(&sorted, 0.50), "p95ms": percentile(&sorted, 0.95),
                "p99ms": percentile(&sorted, 0.99), "maxms": percentile(&sorted, 1.0),
            }),
        );
    }
    let mut mcp_sorted: Vec<u64> = stats
        .kinds
        .iter()
        .filter(|(name, _)| !name.starts_with("page"))
        .flat_map(|(_, kind)| kind.latencies.iter().copied())
        .collect();
    mcp_sorted.sort_unstable();
    let server = match (before, after) {
        (Some(before), Some(after)) => {
            let peak_rss = samples.iter().map(|s| s.0).max().unwrap_or(after.0);
            let peak_threads = samples.iter().map(|s| s.1).max().unwrap_or(after.1);
            json!({
                "rssIdleMiB": before.0 / 1024, "rssPeakMiB": peak_rss / 1024,
                "threadsIdle": before.1, "threadsPeak": peak_threads,
                "cpuPercent": ((after.2 - before.2) / options.seconds as f64 * 100.0).round(),
            })
        }
        _ => Value::Null,
    };
    let summary = json!({
        "idleSessions": opened.load(Ordering::SeqCst),
        "idleSetupS": idle_setup.as_secs_f64(),
        "active": options.active, "tabs": options.tabs, "seconds": options.seconds,
        "connectionFailures": failures.load(Ordering::SeqCst),
        "okCalls": total,
        "throughputPerS": (total as f64 / options.seconds as f64).round(),
        "mcp": {
            "ok": mcp_sorted.len(),
            "p50ms": percentile(&mcp_sorted, 0.50), "p95ms": percentile(&mcp_sorted, 0.95),
            "p99ms": percentile(&mcp_sorted, 0.99),
        },
        "kinds": kinds,
        "server": server,
    });
    println!("{summary}");
}
