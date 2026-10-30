//! The process memory ceiling (`memory.limit`): ONE process-wide setting that every memory
//! check of the engine compares with ([`crate::traversal::memory_budget`]), on by default.
//!
//! There are two memory limits, never mixed: this ceiling, on what the whole PROCESS holds, and
//! the per-query `traversal.max_materialized_bytes`, on what one query materializes.
//!
//! The ceiling is set by the host, once, for the process ([`set_memory_limit`],
//! [`set_memory_headroom`]; the graphersal command line's `--memory-limit`, Python's
//! `graphersal.set_memory_limit`) and is not an execution option: a query's
//! `g.with("memory.limit", ..)`, an [`ExecutionPolicy`](crate::graph::ExecutionPolicy) default
//! or a host preset of that key fails with `TraverserError::InvalidOption` that points here.
//!
//! Resolution ([`memory_limit`] reports the result and its [`MemoryLimitSource`]):
//! 1. a configured byte value ([`MemoryLimit::Bytes`]), or no ceiling at all
//!    ([`MemoryLimit::Unlimited`], `none`);
//! 2. otherwise ([`MemoryLimit::Auto`], the default) the cgroup memory limit of the process minus
//!    the headroom ([`set_memory_headroom`], default
//!    [`default_headroom`](crate::traversal::memory_budget::default_headroom));
//! 3. else 80 % of the machine's physical memory (Linux `/proc/meminfo`, macOS `hw.memsize`,
//!    Windows `GlobalMemoryStatusEx`);
//! 4. on `wasm32` 3.5 GiB (linear memory ends at 4 GiB);
//! 5. on a platform where none of this can be read: no ceiling ([`MemoryLimitSource::Unknown`]).
//!
//! The cgroup limit and the physical memory are read once per process. The resolved value is
//! cached in one atomic: an execution reads it with one relaxed load.

use std::cell::Cell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::{TraverserError, TraverserResult};

/// The name of the process setting (the error's `limit`, `--memory-limit`, `memory_limit=`).
pub(crate) const MEMORY_LIMIT: &str = "memory.limit";

/// The name of the headroom `auto` keeps below a cgroup limit (`--memory-headroom`).
pub(crate) const MEMORY_HEADROOM: &str = "memory.headroom";

/// The share of the physical memory the default ceiling allows, in percent.
pub(crate) const PHYSICAL_MEMORY_PERCENT: u64 = 80;

/// The default ceiling on `wasm32`: 3.5 GiB, half a GiB below the 4 GiB end of a 32-bit linear
/// memory (browsers often grant less; the allocation then fails earlier, as before).
pub(crate) const WASM_DEFAULT_LIMIT: u64 = 3584 * 1024 * 1024;

/// The process memory ceiling as a host sets it: `auto` (the default), a byte count, or `none`.
///
/// Its text form (`Display`/`FromStr`) is what the front ends accept: `auto`, `none` (any letter
/// case) or a positive integer number of bytes.
///
/// ```
/// use graphersal::exec::MemoryLimit;
///
/// assert_eq!("auto".parse::<MemoryLimit>().unwrap(), MemoryLimit::Auto);
/// assert_eq!("none".parse::<MemoryLimit>().unwrap(), MemoryLimit::Unlimited);
/// assert_eq!("1048576".parse::<MemoryLimit>().unwrap(), MemoryLimit::Bytes(1 << 20));
/// assert_eq!(MemoryLimit::Bytes(1 << 20).to_string(), "1048576");
/// assert!("0".parse::<MemoryLimit>().is_err());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum MemoryLimit {
    /// The default: the cgroup limit minus the headroom, else 80 % of the physical memory, else
    /// 3.5 GiB on `wasm32`, else no ceiling (see [`memory_limit`]).
    #[default]
    Auto,
    /// An explicit ceiling in bytes (at least 1).
    Bytes(u64),
    /// No ceiling (`none`): the process may grow until the operating system stops it.
    Unlimited,
}

impl std::fmt::Display for MemoryLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MemoryLimit::Auto => f.write_str("auto"),
            MemoryLimit::Bytes(bytes) => write!(f, "{bytes}"),
            MemoryLimit::Unlimited => f.write_str("none"),
        }
    }
}

impl std::str::FromStr for MemoryLimit {
    type Err = Box<TraverserError>;

    /// `auto`, `none` (any letter case, surrounding spaces ignored) or a positive integer number
    /// of bytes; anything else is `TraverserError::InvalidOption` of `memory.limit`.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let text = text.trim();
        if text.eq_ignore_ascii_case("auto") {
            return Ok(MemoryLimit::Auto);
        }
        if text.eq_ignore_ascii_case("none") {
            return Ok(MemoryLimit::Unlimited);
        }
        match text.parse::<u64>() {
            Ok(bytes) if bytes > 0 => Ok(MemoryLimit::Bytes(bytes)),
            _ => Err(invalid_limit(text)),
        }
    }
}

/// Where the process memory ceiling in effect comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MemoryLimitSource {
    /// The host set a byte value ([`MemoryLimit::Bytes`]).
    Configured,
    /// The cgroup memory limit of the process (a container) minus the headroom.
    Cgroup,
    /// 80 % of the machine's physical memory.
    PhysicalMemory,
    /// The `wasm32` default of 3.5 GiB.
    Wasm,
    /// The host turned the ceiling off ([`MemoryLimit::Unlimited`]).
    Unlimited,
    /// `auto` found neither a cgroup limit nor the physical memory (an unknown platform): no
    /// ceiling.
    Unknown,
}

impl MemoryLimitSource {
    /// A short stable name for displays and JSON: `configured`, `cgroup`, `physical`, `wasm`,
    /// `none` or `unknown`.
    pub fn as_str(&self) -> &'static str {
        match self {
            MemoryLimitSource::Configured => "configured",
            MemoryLimitSource::Cgroup => "cgroup",
            MemoryLimitSource::PhysicalMemory => "physical",
            MemoryLimitSource::Wasm => "wasm",
            MemoryLimitSource::Unlimited => "none",
            MemoryLimitSource::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for MemoryLimitSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The process memory ceiling in effect ([`memory_limit`]): the resolved byte value, where it
/// comes from, and the setting it was resolved from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ProcessMemoryLimit {
    bytes: Option<u64>,
    source: MemoryLimitSource,
    setting: MemoryLimit,
    headroom: Option<u64>,
}

impl ProcessMemoryLimit {
    /// The ceiling in bytes; `None`: no ceiling (`none`, or an unknown platform).
    pub fn bytes(&self) -> Option<u64> {
        self.bytes
    }

    /// Where the ceiling comes from.
    pub fn source(&self) -> MemoryLimitSource {
        self.source
    }

    /// The host's setting ([`MemoryLimit::Auto`] unless [`set_memory_limit`] was called).
    pub fn setting(&self) -> MemoryLimit {
        self.setting
    }

    /// The headroom the host set for a cgroup limit ([`set_memory_headroom`]); `None`: the
    /// default (a tenth of the cgroup limit, at least 64 MiB, at most half).
    pub fn headroom(&self) -> Option<u64> {
        self.headroom
    }
}

/// The host's settings.
#[derive(Debug, Clone, Copy)]
struct Settings {
    limit: MemoryLimit,
    headroom: Option<u64>,
}

static SETTINGS: Mutex<Settings> = Mutex::new(Settings {
    limit: MemoryLimit::Auto,
    headroom: None,
});

/// [`RESOLVED`] before the first resolution and after a setter changed the settings.
const UNRESOLVED: u64 = 0;
/// [`RESOLVED`] when there is no ceiling.
const NO_CEILING: u64 = u64::MAX;

/// The resolved ceiling: [`UNRESOLVED`], [`NO_CEILING`] or the byte value.
static RESOLVED: AtomicU64 = AtomicU64::new(UNRESOLVED);

/// Locks the settings; a poisoned lock (a panic while it was held, which nothing here does) is
/// taken over, as the settings are plain values.
fn settings() -> std::sync::MutexGuard<'static, Settings> {
    SETTINGS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Sets the process memory ceiling for every graph and execution of this process, from now on
/// (an execution that already runs keeps the ceiling it started with). The default, without a
/// call, is [`MemoryLimit::Auto`]; [`MemoryLimit::Unlimited`] turns it off.
///
/// `Err`: `TraverserError::InvalidOption` of `memory.limit` for `Bytes(0)`.
///
/// ```standalone_crate
/// use graphersal::exec::{self, MemoryLimit, MemoryLimitSource};
///
/// // A process-wide setting: this example runs in a process of its own.
/// exec::set_memory_limit(MemoryLimit::Bytes(8 << 30)).unwrap();
/// let limit = exec::memory_limit().unwrap();
/// assert_eq!(limit.bytes(), Some(8 << 30));
/// assert_eq!(limit.source(), MemoryLimitSource::Configured);
/// exec::set_memory_limit(MemoryLimit::Auto).unwrap();
/// ```
pub fn set_memory_limit(limit: MemoryLimit) -> TraverserResult<()> {
    if limit == MemoryLimit::Bytes(0) {
        return Err(invalid_limit("0"));
    }
    let mut settings = settings();
    settings.limit = limit;
    RESOLVED.store(UNRESOLVED, Ordering::Relaxed);
    Ok(())
}

/// Sets the headroom [`MemoryLimit::Auto`] keeps below a cgroup memory limit, in bytes (`None`:
/// the default, a tenth of the limit, at least 64 MiB and at most half of it). It has no effect
/// on an explicit ceiling or on the physical-memory default (whose 20 % are the headroom).
/// Returns `Result` so a check can be added later; it does not fail today.
pub fn set_memory_headroom(bytes: Option<u64>) -> TraverserResult<()> {
    let mut settings = settings();
    settings.headroom = bytes;
    RESOLVED.store(UNRESOLVED, Ordering::Relaxed);
    Ok(())
}

/// The process memory ceiling in effect: its byte value and where it comes from (module
/// documentation of [`crate::exec`] and the book page "Resource Limits"). Returns `Result` so a
/// platform query that can fail can be added later; it does not fail today.
pub fn memory_limit() -> TraverserResult<ProcessMemoryLimit> {
    let settings = *settings();
    let (bytes, source) = resolve(
        settings.limit,
        settings.headroom,
        crate::traversal::memory_budget::cgroup_limit(),
        physical_memory(),
        cfg!(target_arch = "wasm32"),
    );
    Ok(ProcessMemoryLimit {
        bytes,
        source,
        setting: settings.limit,
        headroom: settings.headroom,
    })
}

/// The ceiling `setting` resolves to with `headroom` (for a cgroup limit), the process's
/// `cgroup` limit, its `physical` memory and whether it runs on `wasm32`. Pure: the tests feed
/// it every source.
pub(crate) fn resolve(
    setting: MemoryLimit,
    headroom: Option<u64>,
    cgroup: Option<u64>,
    physical: Option<u64>,
    wasm: bool,
) -> (Option<u64>, MemoryLimitSource) {
    match setting {
        MemoryLimit::Bytes(bytes) => (Some(bytes.max(1)), MemoryLimitSource::Configured),
        MemoryLimit::Unlimited => (None, MemoryLimitSource::Unlimited),
        MemoryLimit::Auto => {
            if wasm {
                return (Some(WASM_DEFAULT_LIMIT), MemoryLimitSource::Wasm);
            }
            if let Some(limit) = crate::traversal::memory_budget::auto_budget(cgroup, headroom) {
                return (Some(limit), MemoryLimitSource::Cgroup);
            }
            match physical {
                Some(total) if total > 0 => (
                    Some(percent_of(total, PHYSICAL_MEMORY_PERCENT)),
                    MemoryLimitSource::PhysicalMemory,
                ),
                _ => (None, MemoryLimitSource::Unknown),
            }
        }
    }
}

/// `percent` % of `total`, at least 1, without overflow.
fn percent_of(total: u64, percent: u64) -> u64 {
    u64::try_from(u128::from(total) * u128::from(percent) / 100)
        .unwrap_or(u64::MAX)
        .max(1)
}

/// The process ceiling in bytes (`None`: none), one relaxed load once resolved.
#[inline]
fn process_limit() -> Option<u64> {
    let mut value = RESOLVED.load(Ordering::Relaxed);
    if value == UNRESOLVED {
        value = resolve_and_cache();
    }
    (value != NO_CEILING).then_some(value)
}

/// Resolves the settings and caches the result (under the settings lock, so a setter running at
/// the same time cannot be overwritten by a stale value).
#[cold]
fn resolve_and_cache() -> u64 {
    let settings = settings();
    let (bytes, _) = resolve(
        settings.limit,
        settings.headroom,
        crate::traversal::memory_budget::cgroup_limit(),
        physical_memory(),
        cfg!(target_arch = "wasm32"),
    );
    // `NO_CEILING` and `UNRESOLVED` are never a real ceiling: `u64::MAX` bytes is no ceiling.
    let value = bytes.map_or(NO_CEILING, |bytes| bytes.clamp(1, NO_CEILING));
    RESOLVED.store(value, Ordering::Relaxed);
    value
}

thread_local! {
    /// A lower ceiling for the executions on this thread ([`memory_limit_scope`]).
    static SCOPED: Cell<Option<u64>> = const { Cell::new(None) };
}

/// Runs `run` with the memory ceiling of this thread's executions lowered to at most `bytes`
/// (the process ceiling still applies when it is lower). Restored afterwards, also on a panic.
///
/// Test support for the crate's own suites (fuzzers and conformance runners that bound each case
/// while other tests run in the same process); not part of the supported API: the ceiling a host
/// sets is the process-wide [`set_memory_limit`].
#[doc(hidden)]
pub fn memory_limit_scope<R>(bytes: u64, run: impl FnOnce() -> R) -> R {
    struct Restore(Option<u64>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let previous = self.0;
            let _ = SCOPED.try_with(|scoped| scoped.set(previous));
        }
    }
    let previous = SCOPED
        .try_with(|scoped| scoped.replace(Some(bytes.max(1))))
        .ok()
        .flatten();
    let _restore = Restore(previous);
    run()
}

/// The ceiling an execution or unit on this thread runs under, in bytes (`None`: none): the
/// process ceiling, lowered by a running job's memory limit and a test scope.
#[inline]
pub(crate) fn effective_limit() -> Option<u64> {
    let mut limit = process_limit();
    let mut lower = |cap: Option<u64>| {
        if let Some(cap) = cap {
            limit = Some(limit.map_or(cap, |limit| limit.min(cap)));
        }
    };
    lower(SCOPED.try_with(Cell::get).ok().flatten());
    lower(crate::graph::jobs::limits::current().and_then(|job| job.memory_limit));
    limit
}

/// The error of an invalid `memory.limit` value `text`.
fn invalid_limit(text: &str) -> Box<TraverserError> {
    Box::new(TraverserError::InvalidOption {
        key: MEMORY_LIMIT.to_string(),
        reason: format!(
            "expected a positive integer number of bytes, \"auto\" or \"none\", got \"{text}\""
        ),
    })
}

/// The `MemTotal` of a `/proc/meminfo` text in bytes (the file gives KiB: `MemTotal:  16303428
/// kB`). `None` when the line is missing or malformed.
#[cfg_attr(not(any(test, target_os = "linux")), allow(dead_code))]
pub(crate) fn parse_meminfo_total(text: &str) -> Option<u64> {
    let line = text.lines().find(|line| line.starts_with("MemTotal:"))?;
    let mut fields = line["MemTotal:".len()..].split_whitespace();
    let value = fields.next()?.parse::<u64>().ok()?;
    match fields.next() {
        Some(unit) if unit.eq_ignore_ascii_case("kB") => value.checked_mul(1024),
        None => Some(value),
        Some(_) => None,
    }
    .filter(|bytes| *bytes > 0)
}

/// The physical memory read from the `meminfo` file at `path` (normally `/proc/meminfo`); a
/// parameter so tests can point it at a fake file.
#[cfg(not(target_arch = "wasm32"))]
#[cfg_attr(not(any(test, target_os = "linux")), allow(dead_code))]
pub(crate) fn physical_memory_at(path: &std::path::Path) -> Option<u64> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| parse_meminfo_total(&text))
}

/// The machine's physical memory in bytes, read once per process; `None` on `wasm32` and where
/// it cannot be read.
pub(crate) fn physical_memory() -> Option<u64> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        static TOTAL: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
        *TOTAL.get_or_init(read_physical_memory)
    }
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
}

#[cfg(target_os = "linux")]
fn read_physical_memory() -> Option<u64> {
    physical_memory_at(std::path::Path::new("/proc/meminfo"))
}

#[cfg(target_os = "macos")]
fn read_physical_memory() -> Option<u64> {
    let mut bytes: u64 = 0;
    let mut size = std::mem::size_of::<u64>();
    // SAFETY: the name is a NUL-terminated C string; `bytes` is a writable u64 and `size` holds
    // its size, as `sysctlbyname` requires for "hw.memsize" (a 64-bit integer); no new value is
    // set (null pointer, length 0).
    let status = unsafe {
        libc::sysctlbyname(
            c"hw.memsize".as_ptr(),
            (&raw mut bytes).cast::<libc::c_void>(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (status == 0 && size == std::mem::size_of::<u64>() && bytes > 0).then_some(bytes)
}

#[cfg(windows)]
fn read_physical_memory() -> Option<u64> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    // SAFETY: `MEMORYSTATUSEX` is plain data (all-zero is valid); `dwLength` is set to its size
    // before the call, as `GlobalMemoryStatusEx` requires.
    let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    // SAFETY: a valid, initialized `MEMORYSTATUSEX` the call writes into.
    let ok = unsafe { GlobalMemoryStatusEx(&mut status) };
    (ok != 0 && status.ullTotalPhys > 0).then_some(status.ullTotalPhys)
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    windows,
    target_arch = "wasm32"
)))]
fn read_physical_memory() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;
    const MIB: u64 = 1 << 20;

    #[test]
    fn resolution_follows_the_sources_in_order() {
        use MemoryLimitSource as S;
        // Configured and none win over everything.
        assert_eq!(
            resolve(MemoryLimit::Bytes(5), None, Some(GIB), Some(GIB), true),
            (Some(5), S::Configured)
        );
        assert_eq!(
            resolve(MemoryLimit::Unlimited, None, Some(GIB), Some(GIB), false),
            (None, S::Unlimited)
        );
        // wasm32: 3.5 GiB.
        assert_eq!(
            resolve(MemoryLimit::Auto, None, None, None, true),
            (Some(3584 * MIB), S::Wasm)
        );
        // A cgroup limit minus the default or the set headroom.
        assert_eq!(
            resolve(
                MemoryLimit::Auto,
                None,
                Some(512 * MIB),
                Some(64 * GIB),
                false
            ),
            (Some(448 * MIB), S::Cgroup)
        );
        assert_eq!(
            resolve(
                MemoryLimit::Auto,
                Some(12 * MIB),
                Some(512 * MIB),
                None,
                false
            ),
            (Some(500 * MIB), S::Cgroup)
        );
        // No cgroup limit: 80 % of the physical memory (the headroom does not apply).
        assert_eq!(
            resolve(MemoryLimit::Auto, Some(MIB), None, Some(10 * GIB), false),
            (Some(8 * GIB), S::PhysicalMemory)
        );
        // Nothing known: no ceiling.
        assert_eq!(
            resolve(MemoryLimit::Auto, None, None, None, false),
            (None, S::Unknown)
        );
        assert_eq!(
            resolve(MemoryLimit::Auto, None, None, Some(0), false),
            (None, S::Unknown)
        );
    }

    #[test]
    fn meminfo_parses_the_total() {
        let text = "MemTotal:       16303428 kB\nMemFree:          123 kB\n";
        assert_eq!(parse_meminfo_total(text), Some(16_303_428 * 1024));
        assert_eq!(parse_meminfo_total("MemFree: 1 kB\n"), None);
        assert_eq!(parse_meminfo_total("MemTotal: lots kB\n"), None);
        assert_eq!(parse_meminfo_total("MemTotal: 0 kB\n"), None);
        assert_eq!(parse_meminfo_total("MemTotal: 5 MB\n"), None);
        assert_eq!(parse_meminfo_total(""), None);
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn meminfo_is_read_from_a_fake_file() {
        let dir = std::env::temp_dir().join(format!("graphersal-meminfo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("meminfo");
        assert_eq!(physical_memory_at(&path), None, "no file");
        std::fs::write(&path, "MemTotal:        2048 kB\n").unwrap();
        assert_eq!(physical_memory_at(&path), Some(2 * MIB));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    fn the_physical_memory_of_this_machine_is_known() {
        let total = physical_memory().expect("physical memory");
        assert!(total >= 256 * MIB, "{total}");
    }

    #[test]
    fn text_forms_round_trip() {
        for limit in [
            MemoryLimit::Auto,
            MemoryLimit::Unlimited,
            MemoryLimit::Bytes(42),
        ] {
            assert_eq!(limit.to_string().parse::<MemoryLimit>().unwrap(), limit);
        }
        assert_eq!(" AUTO ".parse::<MemoryLimit>().unwrap(), MemoryLimit::Auto);
        assert_eq!(
            "None".parse::<MemoryLimit>().unwrap(),
            MemoryLimit::Unlimited
        );
        for bad in ["", "0", "-1", "lots", "1.5"] {
            let err = bad.parse::<MemoryLimit>().unwrap_err();
            assert!(
                matches!(&*err, TraverserError::InvalidOption { key, .. } if key == MEMORY_LIMIT),
                "{bad}: {err}"
            );
        }
    }

    #[test]
    fn a_thread_scope_lowers_the_ceiling_and_is_restored() {
        let outside = effective_limit();
        memory_limit_scope(1000, || {
            assert_eq!(effective_limit(), Some(1000));
            memory_limit_scope(5000, || assert_eq!(effective_limit(), Some(5000)));
            assert_eq!(effective_limit(), Some(1000));
        });
        assert_eq!(effective_limit(), outside);
    }
}
