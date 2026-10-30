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
//!
//! Beside these figures [`add_to_statistics`] adds the process memory bar ([`MemoryBar`]): the
//! process against its memory ceiling (`graphersal::exec::memory_room`) and the segments the
//! memory goes to, computed here once for every front end (playground, REPL, dev server):
//!
//! | Key | Meaning |
//! |---|---|
//! | `usedBytes`, `usedTracked` | the live heap (tracked) or the known segments' sum (`false`) |
//! | `limitBytes`, `usedPercent` | the ceiling and the share used; absent without a ceiling |
//! | `limitSource` | `configured`, `cgroup`, `physical`, `wasm`, `none` or `unknown` |
//! | `segments` | `[{name, bytes}]`: `structure`, `properties`, `compressed`, `indexes`, `models`, then `other` (tracked only) |
//! | `compression` | `{plainBytes, storedBytes}` when values are compressed or deduplicated |
//! | `estimateAboveHeap` | `true` when the graph's estimate exceeds the live heap (`other` clamped to 0) |
//! | `limitError` | instead of the bar's keys when the ceiling could not be read |

use serde_json::{Map, Value, json};

use crate::error::SessionError;
use crate::store::bytes_text;
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

/// Adds [`memory`] to a [`crate::Session::statistics`] answer as `process.memory`, together
/// with the ceiling bar ([`memory_bar`] over the answer's own `memory` and `process.models`, no
/// second engine call): `usedBytes`, `usedTracked`, `limitBytes?`, `limitSource`, `usedPercent?`,
/// `segments`, `compression?`, `estimateAboveHeap?` ([`MemoryBar::to_value`]). The hosts (the
/// WebAssembly wrapper, the dev server) call it; the session's own answer stays free of these
/// volatile figures so two reads of an unchanged session are equal.
///
/// Never fails: when the ceiling cannot be read the bar's keys are left out and the error text
/// is `process.memory.limitError`; the other members are kept.
pub fn add_to_statistics(statistics: &mut Value) {
    let models_bytes = models_bytes(&statistics["process"]["models"]);
    let bar = memory_bar(&statistics["memory"], models_bytes);
    if let Some(process) = statistics.get_mut("process").and_then(Value::as_object_mut) {
        let mut memory = memory();
        if let Some(out) = memory.as_object_mut() {
            match bar {
                Ok(bar) => {
                    if let Value::Object(keys) = bar.to_value() {
                        out.extend(keys);
                    }
                }
                Err(err) => {
                    out.insert("limitError".into(), json!(err.to_string()));
                }
            }
        }
        process.insert("memory".into(), memory);
    }
}

/// The sum of the `bytes` of `process.models` (`[{name, bytes, ..}]`); `0` without any.
pub fn models_bytes(models: &Value) -> u64 {
    models.as_array().map_or(0, |models| {
        models
            .iter()
            .filter_map(|model| model["bytes"].as_u64())
            .fold(0u64, u64::saturating_add)
    })
}

/// One segment of the process memory bar ([`MemoryBar`]): what it holds and its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct MemorySegment {
    /// `structure`, `properties`, `compressed`, `indexes`, `models` or `other`.
    pub name: &'static str,
    /// The segment's bytes.
    pub bytes: u64,
}

/// The process memory against its ceiling, split into segments a user can act on: built by
/// [`memory_bar`] only, shown by the playground (`process.memory`, [`Self::to_value`]), the
/// REPL's `/memory` ([`Self::text`]) and the dev server's start banner (from the JSON keys).
///
/// The segments come from the graph's `memory_usage` (the session's `memory` JSON):
///
/// | Segment | Bytes |
/// |---|---|
/// | `structure` | `structure_bytes + index_bytes` |
/// | `properties` | `property_bytes - compressed_stored_bytes - dedup_table_bytes` |
/// | `compressed` | `compressed_stored_bytes + compression_dictionary_bytes + dedup_table_bytes` (left out at 0) |
/// | `indexes` | `property_index_bytes` (left out at 0) |
/// | `models` | `model_bytes` when the graph reports it, else the loaded embedders (left out at 0) |
/// | `other` | tracked only: the live heap minus the known segments (0 when the estimate is above the heap) |
///
/// With a tracking allocator the used figure is the process's live heap; without one it is the
/// known part only (`used_tracked` false, no `other`).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct MemoryBar {
    /// The process's live heap (tracked), else the sum of the known segments.
    pub used_bytes: u64,
    /// Whether `used_bytes` is the tracked live heap of the whole process.
    pub used_tracked: bool,
    /// The process memory ceiling in bytes; `None` without a ceiling.
    pub limit_bytes: Option<u64>,
    /// Where the ceiling comes from (`MemoryLimitSource::as_str`: `configured`, `cgroup`,
    /// `physical`, `wasm`, `none`, `unknown`).
    pub limit_source: &'static str,
    /// `used_bytes` as a percentage of the ceiling (one decimal); `None` without a ceiling.
    pub used_percent: Option<f64>,
    /// The known segments, then `other` when tracked.
    pub segments: Vec<MemorySegment>,
    /// `(plain, stored)` bytes of the compressed and deduplicated values when there are any:
    /// their plain size and the `compressed` segment.
    pub compression: Option<(u64, u64)>,
    /// Tracked only: the graph's estimate is above the live heap, so `other` was clamped to 0.
    pub estimate_above_heap: bool,
}

/// The process memory bar now: the ceiling and the live heap from
/// [`graphersal::exec::memory_room`], the segments from `graph_memory` (the session's `memory`
/// JSON, [`crate::views::memory_usage`]; `null` when the storage reports none) and
/// `models_bytes` (the loaded embedders, used when `graph_memory` has no `model_bytes`).
/// Fails when the ceiling cannot be read ([`SessionError::Graph`]).
pub fn memory_bar(graph_memory: &Value, models_bytes: u64) -> Result<MemoryBar, SessionError> {
    let room = graphersal::exec::memory_room()?;
    Ok(bar_from(
        graph_memory,
        models_bytes,
        room.limit(),
        room.source().as_str(),
        room.live_bytes(),
    ))
}

/// [`memory_bar`] from given figures (the formulas of [`MemoryBar`]).
fn bar_from(
    graph_memory: &Value,
    models_bytes: u64,
    limit: Option<u64>,
    source: &'static str,
    live: Option<u64>,
) -> MemoryBar {
    let field = |key: &str| graph_memory[key].as_u64().unwrap_or(0);
    let mut segments = Vec::new();
    let mut compression = None;
    if graph_memory.is_object() {
        let stored = field("compressed_stored_bytes");
        let dedup = field("dedup_table_bytes");
        segments.push(MemorySegment {
            name: "structure",
            bytes: field("structure_bytes").saturating_add(field("index_bytes")),
        });
        segments.push(MemorySegment {
            name: "properties",
            bytes: field("property_bytes")
                .saturating_sub(stored)
                .saturating_sub(dedup),
        });
        let compressed = stored
            .saturating_add(field("compression_dictionary_bytes"))
            .saturating_add(dedup);
        if compressed > 0 {
            segments.push(MemorySegment {
                name: "compressed",
                bytes: compressed,
            });
        }
        if field("compressed_values") > 0 || field("deduped_values") > 0 {
            let plain =
                field("compressed_plain_bytes").saturating_add(field("deduped_plain_bytes"));
            compression = Some((plain, compressed));
        }
        let indexes = field("property_index_bytes");
        if indexes > 0 {
            segments.push(MemorySegment {
                name: "indexes",
                bytes: indexes,
            });
        }
    }
    let models = graph_memory["model_bytes"].as_u64().unwrap_or(models_bytes);
    if models > 0 {
        segments.push(MemorySegment {
            name: "models",
            bytes: models,
        });
    }
    let known = segments
        .iter()
        .fold(0u64, |sum, segment| sum.saturating_add(segment.bytes));
    let (used_bytes, used_tracked, estimate_above_heap) = match live {
        Some(live) if live > 0 => {
            segments.push(MemorySegment {
                name: "other",
                bytes: live.saturating_sub(known),
            });
            (live, true, known > live)
        }
        _ => (known, false, false),
    };
    let used_percent = limit
        .filter(|limit| *limit > 0)
        .map(|limit| (used_bytes as f64 / limit as f64 * 1000.0).round() / 10.0);
    MemoryBar {
        used_bytes,
        used_tracked,
        limit_bytes: limit,
        limit_source: source,
        used_percent,
        segments,
        compression,
        estimate_above_heap,
    }
}

impl MemoryBar {
    /// The keys `add_to_statistics` merges into `process.memory` (camelCase like its other
    /// members): `usedBytes`, `usedTracked`, `limitBytes` (absent without a ceiling),
    /// `limitSource`, `usedPercent` (absent without a ceiling), `segments: [{name, bytes}]`,
    /// `compression: {plainBytes, storedBytes}` (when something is compressed or deduplicated)
    /// and `estimateAboveHeap: true` (only when set).
    pub fn to_value(&self) -> Value {
        let mut out = Map::new();
        out.insert("usedBytes".into(), json!(self.used_bytes));
        out.insert("usedTracked".into(), json!(self.used_tracked));
        if let Some(limit) = self.limit_bytes {
            out.insert("limitBytes".into(), json!(limit));
        }
        out.insert("limitSource".into(), json!(self.limit_source));
        if let Some(percent) = self.used_percent {
            out.insert("usedPercent".into(), json!(percent));
        }
        out.insert(
            "segments".into(),
            Value::Array(
                self.segments
                    .iter()
                    .map(|segment| json!({"name": segment.name, "bytes": segment.bytes}))
                    .collect(),
            ),
        );
        if let Some((plain, stored)) = self.compression {
            out.insert(
                "compression".into(),
                json!({"plainBytes": plain, "storedBytes": stored}),
            );
        }
        if self.estimate_above_heap {
            out.insert("estimateAboveHeap".into(), json!(true));
        }
        Value::Object(out)
    }

    /// The bar as plain ASCII text at most `width` columns wide (the REPL's `/memory`): a
    /// header (used of limit (source), the percentage at the right), the bar `[###...]`, the
    /// segments with their sizes and the compression saving. Without a tracking allocator the
    /// header says `known` and a last line says the process total is not tracked; without a
    /// ceiling there is no bar and no percentage.
    ///
    /// ```text
    /// Memory   1.6 GiB of 3.4 GiB (cgroup)                                        47 %
    ///          [###############################....................................]
    ///          structure 180.0 MiB  properties 420.0 MiB  compressed 95.0 MiB
    ///          indexes 60.0 MiB  models 506.0 MiB  other 340.0 MiB
    ///          compression saves 285.0 MiB (plain 380.0 MiB -> 95.0 MiB, -75 %)
    /// ```
    pub fn text(&self, width: usize) -> String {
        const HEAD: &str = "Memory   ";
        let indent = " ".repeat(HEAD.len());
        let width = width.max(HEAD.len() + 24);
        let used = bytes_text(self.used_bytes);
        let known = if self.used_tracked { "" } else { " known" };
        let mut lines = Vec::new();
        match (self.limit_bytes, self.used_percent) {
            (Some(limit), Some(percent)) => {
                let left = format!(
                    "{HEAD}{used}{known} of {} ({})",
                    bytes_text(limit),
                    self.limit_source
                );
                let right = format!("{percent:.0} %");
                let gap = width.saturating_sub(left.len() + right.len()).max(1);
                lines.push(format!("{left}{}{right}", " ".repeat(gap)));
                let inner = width - indent.len() - 2;
                let filled = ((percent / 100.0) * inner as f64)
                    .round()
                    .clamp(0.0, inner as f64) as usize;
                lines.push(format!(
                    "{indent}[{}{}]",
                    "#".repeat(filled),
                    ".".repeat(inner - filled)
                ));
            }
            _ => {
                let why = match self.limit_source {
                    "unknown" => "no ceiling: the machine's memory size is unknown".to_string(),
                    source => format!("no ceiling: memory.limit {source}"),
                };
                lines.push(format!("{HEAD}{used}{known} ({why})"));
            }
        }
        let mut line = String::new();
        for segment in &self.segments {
            let item = format!("{} {}", segment.name, bytes_text(segment.bytes));
            if !line.is_empty() && indent.len() + line.len() + 2 + item.len() > width {
                lines.push(format!("{indent}{line}"));
                line.clear();
            }
            if !line.is_empty() {
                line.push_str("  ");
            }
            line.push_str(&item);
        }
        if !line.is_empty() {
            lines.push(format!("{indent}{line}"));
        }
        if let Some((plain, stored)) = self.compression {
            if plain > stored {
                let saved = plain - stored;
                let share = (saved as f64 / plain as f64 * 100.0).round();
                lines.push(format!(
                    "{indent}compression saves {} (plain {} -> {}, -{share:.0} %)",
                    bytes_text(saved),
                    bytes_text(plain),
                    bytes_text(stored)
                ));
            } else {
                lines.push(format!(
                    "{indent}compression saves nothing (plain {} -> {})",
                    bytes_text(plain),
                    bytes_text(stored)
                ));
            }
        }
        if !self.used_tracked {
            let what = if self.used_percent.is_some() {
                "the percentage is of the known part: "
            } else {
                ""
            };
            lines.push(format!("{indent}{what}process total not tracked"));
        }
        lines.join("\n")
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

    const MIB: u64 = 1 << 20;

    /// A graph `memory` JSON with compression, dedup and a property index.
    fn graph_memory() -> Value {
        json!({
            "structure_bytes": 150 * MIB, "index_bytes": 30 * MIB,
            "property_bytes": 500 * MIB, "compressed_values": 1000,
            "compressed_plain_bytes": 300 * MIB, "compressed_stored_bytes": 60 * MIB,
            "compression_dictionary_bytes": 15 * MIB, "deduped_values": 10,
            "deduped_plain_bytes": 80 * MIB, "dedup_table_bytes": 20 * MIB,
            "property_index_bytes": 60 * MIB, "total_bytes": 755 * MIB,
        })
    }

    fn segment(bar: &MemoryBar, name: &str) -> Option<u64> {
        bar.segments
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.bytes)
    }

    #[test]
    fn segments_follow_the_agreed_formulas() {
        let bar = bar_from(
            &graph_memory(),
            506 * MIB,
            Some(4096 * MIB),
            "cgroup",
            Some(1600 * MIB),
        );
        assert_eq!(segment(&bar, "structure"), Some(180 * MIB));
        assert_eq!(segment(&bar, "properties"), Some(420 * MIB));
        assert_eq!(segment(&bar, "compressed"), Some(95 * MIB));
        assert_eq!(segment(&bar, "indexes"), Some(60 * MIB));
        assert_eq!(segment(&bar, "models"), Some(506 * MIB));
        // Known = total_bytes + models.
        let known: u64 = bar.segments[..5].iter().map(|s| s.bytes).sum();
        assert_eq!(known, 755 * MIB + 506 * MIB);
        assert_eq!(segment(&bar, "other"), Some(1600 * MIB - known));
        assert_eq!(bar.segments.last().unwrap().name, "other");
        assert!(bar.used_tracked && !bar.estimate_above_heap);
        assert_eq!(bar.used_bytes, 1600 * MIB);
        assert_eq!(bar.used_percent, Some(39.1));
        assert_eq!(bar.compression, Some((380 * MIB, 95 * MIB)));
        let value = bar.to_value();
        assert_eq!(value["usedBytes"], json!(1600 * MIB));
        assert_eq!(value["limitBytes"], json!(4096 * MIB));
        assert_eq!(value["limitSource"], "cgroup");
        assert_eq!(value["usedPercent"], json!(39.1));
        assert_eq!(value["usedTracked"], json!(true));
        assert_eq!(
            value["segments"][0],
            json!({"name": "structure", "bytes": 180 * MIB})
        );
        assert_eq!(
            value["compression"],
            json!({"plainBytes": 380 * MIB, "storedBytes": 95 * MIB})
        );
        assert!(value.get("estimateAboveHeap").is_none());
        // The estimate above the heap: other clamps at 0 and the flag is set.
        let bar = bar_from(&graph_memory(), 0, Some(4096 * MIB), "cgroup", Some(MIB));
        assert_eq!(segment(&bar, "other"), Some(0));
        assert!(bar.estimate_above_heap);
        assert_eq!(bar.to_value()["estimateAboveHeap"], json!(true));
        // Nothing compressed, no index: those segments and the saving are left out.
        let plain = json!({"structure_bytes": 10, "index_bytes": 5, "property_bytes": 20});
        let bar = bar_from(&plain, 0, Some(1000), "configured", None);
        let names: Vec<&str> = bar.segments.iter().map(|s| s.name).collect();
        assert_eq!(names, ["structure", "properties"]);
        assert_eq!(bar.compression, None);
    }

    #[test]
    fn untracked_shows_only_the_known_part() {
        let bar = bar_from(&graph_memory(), 0, Some(2000 * MIB), "physical", None);
        assert!(!bar.used_tracked);
        assert_eq!(bar.used_bytes, 755 * MIB);
        assert_eq!(segment(&bar, "other"), None);
        assert_eq!(bar.used_percent, Some(37.8));
        let text = bar.text(80);
        assert!(
            text.starts_with("Memory   755.0 MiB known of 2.0 GiB (physical)"),
            "{text}"
        );
        assert!(
            text.ends_with("the percentage is of the known part: process total not tracked"),
            "{text}"
        );
        assert_eq!(bar.to_value()["usedTracked"], json!(false));
    }

    #[test]
    fn no_ceiling_has_no_percentage() {
        let bar = bar_from(&graph_memory(), 0, None, "none", Some(900 * MIB));
        assert_eq!(bar.used_percent, None);
        let value = bar.to_value();
        assert!(value.get("limitBytes").is_none() && value.get("usedPercent").is_none());
        assert_eq!(value["limitSource"], "none");
        let text = bar.text(80);
        assert!(
            text.starts_with("Memory   900.0 MiB (no ceiling: memory.limit none)"),
            "{text}"
        );
        assert!(!text.contains('['), "{text}");
        let unknown = bar_from(&Value::Null, 0, None, "unknown", None);
        assert!(unknown.segments.is_empty());
        assert!(unknown.text(80).contains("memory size is unknown"));
    }

    #[test]
    fn models_segment_reads_model_bytes_first() {
        let mut memory = graph_memory();
        memory["model_bytes"] = json!(7 * MIB);
        let bar = bar_from(&memory, 506 * MIB, None, "none", None);
        assert_eq!(segment(&bar, "models"), Some(7 * MIB));
        let bar = bar_from(&graph_memory(), 506 * MIB, None, "none", None);
        assert_eq!(segment(&bar, "models"), Some(506 * MIB));
        let bar = bar_from(&graph_memory(), 0, None, "none", None);
        assert_eq!(segment(&bar, "models"), None);
        assert_eq!(
            models_bytes(&json!([{"name": "a", "bytes": 3}, {"name": "b", "bytes": 4}])),
            7
        );
        assert_eq!(models_bytes(&Value::Null), 0);
    }

    #[test]
    fn text_bar_has_the_agreed_shape() {
        let bar = bar_from(
            &graph_memory(),
            506 * MIB,
            Some(3400 * MIB),
            "cgroup",
            Some(1600 * MIB),
        );
        let text = bar.text(80);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines.iter().all(|l| l.len() <= 80), "{text}");
        assert!(
            lines[0].starts_with("Memory   1.6 GiB of 3.3 GiB (cgroup)"),
            "{text}"
        );
        assert!(
            lines[0].ends_with(" 47 %") && lines[0].len() == 80,
            "{text}"
        );
        assert!(
            lines[1].starts_with("         [#") && lines[1].ends_with(".]"),
            "{text}"
        );
        assert_eq!(lines[1].len(), 80);
        let hashes = lines[1].matches('#').count();
        assert_eq!(hashes, (0.471 * 69.0f64).round() as usize, "{text}");
        assert!(lines[2].starts_with("         structure 180.0 MiB  properties 420.0 MiB"));
        assert!(text.contains("models 506.0 MiB"), "{text}");
        assert!(text.contains("other "), "{text}");
        assert_eq!(
            *lines.last().unwrap(),
            "         compression saves 285.0 MiB (plain 380.0 MiB -> 95.0 MiB, -75 %)"
        );
        assert!(!text.contains("not tracked"));
    }

    #[test]
    fn statistics_without_a_ceiling_error_get_the_bar_keys() {
        let mut statistics = json!({"memory": graph_memory(), "process": {"models": []}});
        add_to_statistics(&mut statistics);
        let memory = &statistics["process"]["memory"];
        assert!(memory["limitSource"].is_string(), "{memory}");
        assert!(memory["segments"].is_array(), "{memory}");
        assert!(memory["usedBytes"].is_u64(), "{memory}");
        assert!(memory.get("limitError").is_none(), "{memory}");
    }
}
