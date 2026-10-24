//! The memory the WHOLE process holds (`process.memory` of [`crate::Session::statistics`]),
//! collected by the platform: what the operating system reports for the process (resident set
//! size), the engine's own live heap and, in the browser, the WebAssembly linear memory.
//!
//! Every member is optional and left out when it is unknown; collecting never fails and never
//! panics (a missing `/proc`, a missing `ps` or unparsable output simply omits the member).
//!
//! | Platform | `rssBytes` | `peakRssBytes` | other |
//! |---|---|---|---|
//! | Linux | `VmRSS` of `/proc/self/status` | `VmHWM` (the OS's own high-water mark) | |
//! | macOS | `ps -o rss= -p <pid>` | highest RSS this process has seen in a report | |
//! | Windows, other native | omitted | omitted | |
//! | wasm32 | omitted | omitted | `wasmMemoryBytes`: the linear memory size |
//!
//! `liveHeapBytes` is `graphersal::alloc_tracking::global_live_bytes()`: the bytes the Rust
//! allocator currently holds, present only when the binary installed the `TrackingAllocator`
//! (the CLI, the WebAssembly module and the Python binding do). The library cannot tell
//! "not installed" from "nothing allocated", and a running process always holds some bytes, so
//! `0` means not installed and the member is omitted.

use serde_json::{Map, Value, json};
#[cfg(any(target_os = "macos", test))]
use std::sync::atomic::{AtomicU64, Ordering};

/// The highest resident set size any report of this process has seen (macOS has no OS-side
/// high-water mark, so the peak is "the highest value seen by a report").
#[cfg(any(target_os = "macos", test))]
static PEAK_SEEN: AtomicU64 = AtomicU64::new(0);

/// Parses a `/proc/<pid>/status` text into `(VmRSS, VmHWM)` in bytes (the file gives KiB as
/// `VmRSS:\t  412344 kB`). A missing or malformed line is `None`.
pub fn parse_proc_status(text: &str) -> (Option<u64>, Option<u64>) {
    fn field(text: &str, key: &str) -> Option<u64> {
        let line = text.lines().find_map(|l| l.strip_prefix(key))?;
        let rest = line.strip_prefix(':')?.trim();
        let mut parts = rest.split_whitespace();
        let number: u64 = parts.next()?.parse().ok()?;
        match parts.next() {
            Some(unit) if unit.eq_ignore_ascii_case("kb") => number.checked_mul(1024),
            None => Some(number),
            Some(_) => None,
        }
    }
    (field(text, "VmRSS"), field(text, "VmHWM"))
}

/// Parses the output of `ps -o rss= -p <pid>` (KiB, surrounding spaces and a newline) into
/// bytes. Anything else (empty, text, a negative number) is `None`.
pub fn parse_ps_rss_kib(text: &str) -> Option<u64> {
    let kib: u64 = text.trim().parse().ok()?;
    kib.checked_mul(1024)
}

#[cfg(target_os = "linux")]
fn os_rss() -> (Option<u64>, Option<u64>) {
    match std::fs::read_to_string("/proc/self/status") {
        Ok(text) => parse_proc_status(&text),
        Err(_) => (None, None),
    }
}

#[cfg(target_os = "macos")]
fn os_rss() -> (Option<u64>, Option<u64>) {
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    let rss = match output {
        Ok(out) if out.status.success() => std::str::from_utf8(&out.stdout)
            .ok()
            .and_then(parse_ps_rss_kib),
        _ => None,
    };
    let peak = rss.map(|r| PEAK_SEEN.fetch_max(r, Ordering::Relaxed).max(r));
    (rss, peak)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn os_rss() -> (Option<u64>, Option<u64>) {
    (None, None)
}

/// The size of the WebAssembly linear memory in bytes.
#[cfg(target_arch = "wasm32")]
fn wasm_memory_bytes() -> Option<u64> {
    Some(core::arch::wasm32::memory_size::<0>() as u64 * 65536)
}

#[cfg(not(target_arch = "wasm32"))]
fn wasm_memory_bytes() -> Option<u64> {
    None
}

/// `process.memory`: `{liveHeapBytes?, rssBytes?, peakRssBytes?, wasmMemoryBytes?, source}`.
/// `source` names where the figures come from (`proc`, `ps`, `wasm`, `allocator` or `none`);
/// on macOS the peak is the highest value seen by a report of this process.
pub fn memory() -> Value {
    let mut out = Map::new();
    let live = graphersal::alloc_tracking::global_live_bytes();
    if live > 0 {
        out.insert("liveHeapBytes".into(), json!(live));
    }
    let (rss, peak) = os_rss();
    if let Some(rss) = rss {
        out.insert("rssBytes".into(), json!(rss));
    }
    if let Some(peak) = peak {
        out.insert("peakRssBytes".into(), json!(peak));
    }
    if let Some(wasm) = wasm_memory_bytes() {
        out.insert("wasmMemoryBytes".into(), json!(wasm));
    }
    let source = if cfg!(target_os = "linux") && rss.is_some() {
        "proc"
    } else if cfg!(target_os = "macos") && rss.is_some() {
        "ps"
    } else if cfg!(target_arch = "wasm32") {
        "wasm"
    } else if live > 0 {
        "allocator"
    } else {
        "none"
    };
    out.insert("source".into(), json!(source));
    Value::Object(out)
}

/// Adds [`memory`] to a [`crate::Session::statistics`] answer as `process.memory`. The hosts
/// (the WebAssembly wrapper, the dev server) call it; the session's own answer stays free of
/// these volatile figures so two reads of an unchanged session are equal.
pub fn add_to_statistics(statistics: &mut Value) {
    if let Some(process) = statistics.get_mut("process").and_then(Value::as_object_mut) {
        process.insert("memory".into(), memory());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_status_is_parsed() {
        let text = "Name:\tgraphersal\nVmPeak:\t  900000 kB\nVmHWM:\t  640000 kB\nVmRSS:\t  412344 kB\nThreads:\t4\n";
        assert_eq!(
            parse_proc_status(text),
            (Some(412344 * 1024), Some(640000 * 1024))
        );
        assert_eq!(parse_proc_status("VmRSS:\t12 kB\n"), (Some(12288), None));
        assert_eq!(
            parse_proc_status("VmRSS:\tlots kB\nVmHWM: 5 MB\n"),
            (None, None)
        );
        assert_eq!(parse_proc_status(""), (None, None));
    }

    #[test]
    fn ps_output_is_parsed() {
        assert_eq!(parse_ps_rss_kib("  412344\n"), Some(412344 * 1024));
        assert_eq!(parse_ps_rss_kib("7"), Some(7168));
        assert_eq!(parse_ps_rss_kib(""), None);
        assert_eq!(parse_ps_rss_kib("  \n"), None);
        assert_eq!(parse_ps_rss_kib("RSS 12"), None);
        assert_eq!(parse_ps_rss_kib("-5"), None);
        assert_eq!(parse_ps_rss_kib("99999999999999999999999999"), None);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn the_real_collection_reports_a_plausible_rss() {
        let m = memory();
        let rss = m["rssBytes"].as_u64().expect("rssBytes");
        assert!(rss > 1 << 20 && rss < 1 << 40, "{m}");
        assert!(m["peakRssBytes"].as_u64().unwrap() >= rss, "{m}");
        assert!(m["source"] == "proc" || m["source"] == "ps", "{m}");
        let _ = PEAK_SEEN.load(Ordering::Relaxed);
    }
}
