//! Workload statistics (book page "Workload Statistics"): what the queries of an instance did,
//! kept in time buckets per series, on the instance (not versioned with the data, never in the
//! WAL), persisted best effort by a Store in its sidecar file.
//!
//! A series is a key ([`SeriesKey`]: what is counted and through which [`Channel`]) with three
//! tiers of buckets, like a round-robin database: one bucket per minute for the last 60
//! minutes, one per hour for the last 48 hours, one per day for the last 90 days (a Store
//! persists all three). Every sample adds to the current bucket of each tier, so a
//! tier needs no roll-up; old buckets are dropped when a sample or a flush passes them. Series
//! are sparse: a bucket exists only once something was counted in it.
//!
//! The scheduler's job history (`jobs.md`) is a block of the same statistics.

#[cfg(feature = "persist")]
pub(crate) mod codec;
pub(crate) mod fingerprint;
mod property;
pub(crate) mod scope;
mod text;

pub use scope::{current_channel, scope, set_thread_default};

use std::collections::HashMap;
use std::collections::VecDeque;

use compact_str::CompactString;

/// How much of the workload statistics an instance keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum WorkloadLevel {
    /// Nothing is recorded (one branch per sample).
    Off,
    /// Recorded in memory only: gone with the process.
    Memory,
    /// Recorded in memory and, by a Store, written to its sidecar file (periodically and at
    /// close, every tier) and read back at open. The default.
    #[default]
    Persisted,
}

impl WorkloadLevel {
    /// The level's name: `off`, `memory`, `persisted`.
    pub fn name(self) -> &'static str {
        match self {
            WorkloadLevel::Off => "off",
            WorkloadLevel::Memory => "memory",
            WorkloadLevel::Persisted => "persisted",
        }
    }

    /// The level named `name` (`off`, `memory`, `persisted`).
    pub fn from_name(name: &str) -> Option<WorkloadLevel> {
        match name {
            "off" => Some(WorkloadLevel::Off),
            "memory" => Some(WorkloadLevel::Memory),
            "persisted" => Some(WorkloadLevel::Persisted),
            _ => None,
        }
    }
}

/// Through which front end a counted operation came in: the playground, an AI agent over MCP,
/// the CLI, Python, an embedding host, or the scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
#[non_exhaustive]
pub enum Channel {
    /// An embedding host (the default of the Rust API).
    #[default]
    Host,
    /// The browser playground (or the dev server's page).
    Playground,
    /// An AI agent through the MCP endpoint (or the AI Chat).
    Mcp,
    /// The command line (one-shot runs and the REPL).
    Cli,
    /// The Python binding.
    Python,
    /// A scheduled job.
    Job,
}

impl Channel {
    /// Every channel, in order.
    pub const ALL: [Channel; 6] = [
        Channel::Host,
        Channel::Playground,
        Channel::Mcp,
        Channel::Cli,
        Channel::Python,
        Channel::Job,
    ];

    /// The channel's name: `host`, `playground`, `mcp`, `cli`, `python`, `job`.
    pub fn name(self) -> &'static str {
        match self {
            Channel::Host => "host",
            Channel::Playground => "playground",
            Channel::Mcp => "mcp",
            Channel::Cli => "cli",
            Channel::Python => "python",
            Channel::Job => "job",
        }
    }

    /// The channel named `name`.
    pub fn from_name(name: &str) -> Option<Channel> {
        Channel::ALL
            .into_iter()
            .find(|channel| channel.name() == name)
    }

    fn code(self) -> u8 {
        match self {
            Channel::Host => 0,
            Channel::Playground => 1,
            Channel::Mcp => 2,
            Channel::Cli => 3,
            Channel::Python => 4,
            Channel::Job => 5,
        }
    }

    fn from_code(code: u8) -> Option<Channel> {
        Channel::ALL
            .into_iter()
            .find(|channel| channel.code() == code)
    }
}

/// How a query used a property: the kind of access a step makes (the property statistics; the
/// storage advisor weighs them: a key filtered by a scan is an index candidate, a key only
/// written is cold).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum AccessKind {
    /// A filter evaluated element by element (`has(key, ..)`, `has_not(key)`).
    FilterScan,
    /// A filter served by a property index (`v().has(key, ..)` reading an index).
    FilterIndex,
    /// The value read into a result (`values`, `value_map`, `element_map`, `project().by(key)`).
    Read,
    /// The value used to sort or group (`order().by(key)`, `group().by(key)`, `dedup().by(key)`).
    OrderGroup,
    /// A vector compared (`similarity`, `nearest`).
    Similarity,
    /// The value written (`property(key, ..)`, a created element's properties).
    Write,
    /// The value removed (`remove_property`).
    Remove,
}

impl AccessKind {
    /// Every kind, in order.
    pub const ALL: [AccessKind; 7] = [
        AccessKind::FilterScan,
        AccessKind::FilterIndex,
        AccessKind::Read,
        AccessKind::OrderGroup,
        AccessKind::Similarity,
        AccessKind::Write,
        AccessKind::Remove,
    ];

    /// `filter_scan`, `filter_index`, `read`, `order_group`, `similarity`, `write`, `remove`.
    pub fn name(self) -> &'static str {
        match self {
            AccessKind::FilterScan => "filter_scan",
            AccessKind::FilterIndex => "filter_index",
            AccessKind::Read => "read",
            AccessKind::OrderGroup => "order_group",
            AccessKind::Similarity => "similarity",
            AccessKind::Write => "write",
            AccessKind::Remove => "remove",
        }
    }

    pub(crate) fn code(self) -> u8 {
        match self {
            AccessKind::FilterScan => 0,
            AccessKind::FilterIndex => 1,
            AccessKind::Read => 2,
            AccessKind::OrderGroup => 3,
            AccessKind::Similarity => 4,
            AccessKind::Write => 5,
            AccessKind::Remove => 6,
        }
    }

    pub(crate) fn from_code(code: u8) -> Option<AccessKind> {
        AccessKind::ALL.into_iter().find(|kind| kind.code() == code)
    }
}

/// Which property a property series counts: the element kind and label as the plan states them
/// (`None`: unknown, every one) and the top-level key (`*`: every key, a step reading the whole
/// map such as `element_map()`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub struct PropertyUse {
    /// The element kind, when the plan knows it.
    pub element: Option<crate::graph::ElementKind>,
    /// The label, when the plan knows it.
    pub label: Option<CompactString>,
    /// The top-level key (`*`: every key).
    pub key: CompactString,
    /// How it was used.
    pub access: AccessKind,
}

/// What a series counts. Later stages add the property accesses and the query statistics;
/// a sidecar written by a newer version with kinds this one does not know is read without them.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum SeriesKind {
    /// The runs of one scheduled job (its name).
    Job(CompactString),
    /// The graph's lock: `write.wait` (asking for the write lock until holding it),
    /// `write.hold` (holding it), `read.wait` (a reader waiting for a writer).
    Lock(CompactString),
    /// One query (the query statistics): its normalized fingerprint (the optimized plan with
    /// literals replaced by `?`), or `saved:<name>` for a saved query call as a whole.
    Query(CompactString),
    /// One property used one way (the property statistics): executions that used it and the
    /// traversers that reached the step using it.
    Property(PropertyUse),
}

/// The key of a series: what is counted and the channel it came through.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub struct SeriesKey {
    /// What is counted.
    pub kind: SeriesKind,
    /// Where it came from.
    pub channel: Channel,
}

impl SeriesKey {
    /// The key of `kind` through `channel`.
    pub fn new(kind: SeriesKind, channel: Channel) -> Self {
        SeriesKey { kind, channel }
    }
}

/// One counted occurrence: how many items it touched and how long it took.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct Sample {
    /// Items (traversers, elements, values) the occurrence touched.
    pub items: u64,
    /// Its duration in microseconds.
    pub micros: u64,
    /// Whether it failed.
    pub failed: bool,
}

impl Sample {
    /// A sample of `items` items taking `micros` microseconds.
    pub fn new(items: u64, micros: u64) -> Self {
        Sample {
            items,
            micros,
            failed: false,
        }
    }

    /// The same sample, failed.
    pub fn failed(mut self) -> Self {
        self.failed = true;
        self
    }
}

/// The figures of one bucket (or a sum of buckets).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct Counters {
    /// Occurrences.
    pub count: u64,
    /// Failed occurrences.
    pub failed: u64,
    /// Items touched.
    pub items: u64,
    /// Total duration, in microseconds.
    pub total_micros: u64,
    /// The longest single occurrence, in microseconds.
    pub max_micros: u64,
}

impl Counters {
    fn add(&mut self, sample: &Sample) {
        self.count += 1;
        self.failed += u64::from(sample.failed);
        self.items = self.items.saturating_add(sample.items);
        self.total_micros = self.total_micros.saturating_add(sample.micros);
        self.max_micros = self.max_micros.max(sample.micros);
    }

    fn merge(&mut self, other: &Counters) {
        self.count = self.count.saturating_add(other.count);
        self.failed = self.failed.saturating_add(other.failed);
        self.items = self.items.saturating_add(other.items);
        self.total_micros = self.total_micros.saturating_add(other.total_micros);
        self.max_micros = self.max_micros.max(other.max_micros);
    }
}

/// The tiers: bucket width in microseconds and how many buckets are kept.
const MINUTE: i64 = 60_000_000;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;
const TIERS: [(i64, usize); 3] = [(MINUTE, 60), (HOUR, 48), (DAY, 90)];

/// The buckets of one tier, oldest first: `(bucket start / width, counters)`.
#[derive(Debug, Clone, Default)]
struct Tier {
    buckets: VecDeque<(i64, Counters)>,
}

impl Tier {
    fn add(&mut self, index: i64, keep: usize, sample: &Sample) {
        match self.buckets.back_mut() {
            Some((last, counters)) if *last == index => counters.add(sample),
            Some((last, _)) if *last > index => {
                // A late sample (a clock step back): counted in its own bucket if it is kept.
                if let Some((_, counters)) = self.buckets.iter_mut().find(|(i, _)| *i == index) {
                    counters.add(sample);
                }
            }
            _ => {
                let mut counters = Counters::default();
                counters.add(sample);
                self.buckets.push_back((index, counters));
            }
        }
        self.expire(index, keep);
    }

    /// Drops the buckets older than the `keep` newest ones ending at `index`.
    fn expire(&mut self, index: i64, keep: usize) {
        while let Some((first, _)) = self.buckets.front() {
            if *first <= index - keep as i64 {
                self.buckets.pop_front();
            } else {
                break;
            }
        }
    }

    fn sum_since(&self, index: i64) -> Counters {
        let mut out = Counters::default();
        for (i, counters) in &self.buckets {
            if *i >= index {
                out.merge(counters);
            }
        }
        out
    }
}

/// One series: its three tiers.
#[derive(Debug, Clone, Default)]
struct Series {
    tiers: [Tier; 3],
}

/// The last run of a scheduled job (the job block of the statistics).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct JobRunRecord {
    /// When it started (microseconds since the Unix epoch).
    pub started_at: i64,
    /// How long it took, in microseconds.
    pub micros: u64,
    /// Its error, `None` when it succeeded.
    pub error: Option<String>,
}

/// The settings an instance keeps for one of its SYSTEM jobs (book page "Scheduled Jobs"): a
/// schedule and a switch replacing the built-in ones (`None` keeps the built-in one). Settings,
/// not statistics: kept at every level, written to the sidecar at level `persisted`, never
/// deleted by [`WorkloadStats::clear`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct JobOverride {
    /// The schedule (cron text) replacing the built-in one.
    pub schedule: Option<String>,
    /// The switch replacing the built-in one.
    pub enabled: Option<bool>,
}

impl JobOverride {
    /// No override: the built-in schedule and switch.
    pub fn new() -> Self {
        Self::default()
    }

    /// The same override with `schedule` (cron text).
    pub fn with_schedule(mut self, schedule: impl Into<String>) -> Self {
        self.schedule = Some(schedule.into());
        self
    }

    /// The same override with the switch `enabled`.
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = Some(enabled);
        self
    }

    /// Whether it overrides nothing.
    pub fn is_empty(&self) -> bool {
        self.schedule.is_none() && self.enabled.is_none()
    }
}

/// What the instance keeps about one storage-advisor recommendation (book page "Optimize"), by
/// its id: a dismissal (until a time, `i64::MAX` = for good) and the record of applying it (the
/// baseline its outcome is measured against). Settings of the instance like [`JobOverride`]:
/// kept at every level, written to the sidecar at level `persisted`, never deleted by
/// [`WorkloadStats::clear`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct AdviceMark {
    /// Not recommended before this time (microseconds since the Unix epoch).
    pub dismissed_until: Option<i64>,
    /// When it was applied, and the figures before.
    pub applied: Option<AppliedAdvice>,
}

impl AdviceMark {
    /// Whether it says nothing.
    pub fn is_empty(&self) -> bool {
        self.dismissed_until.is_none() && self.applied.is_none()
    }
}

/// The record of an applied recommendation: when, what it was and the memory figures before.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct AppliedAdvice {
    /// When it was applied (microseconds since the Unix epoch).
    pub at: i64,
    /// Its kind (`create_index`, ...).
    pub kind: String,
    /// Its title.
    pub title: String,
    /// The memory it promised to free (negative: to take), in bytes.
    pub promised_bytes: i64,
    /// The graph's property bytes plus property index bytes before it was applied.
    pub bytes_before: u64,
}

impl AppliedAdvice {
    /// The record of `kind` / `title` applied at `at`, which promised `promised_bytes` when the
    /// graph held `bytes_before`.
    pub fn new(
        at: i64,
        kind: impl Into<String>,
        title: impl Into<String>,
        promised_bytes: i64,
        bytes_before: u64,
    ) -> Self {
        Self {
            at,
            kind: kind.into(),
            title: title.into(),
            promised_bytes,
            bytes_before,
        }
    }
}

/// The most query series kept; a new query beyond it evicts the one run least in the last 90
/// days (the query statistics stay bounded whatever the workload).
pub const MAX_QUERY_SERIES: usize = 500;

#[derive(Debug, Default)]
struct Inner {
    series: HashMap<SeriesKey, Series>,
    jobs: HashMap<CompactString, JobRunRecord>,
    /// The system jobs' settings ([`JobOverride`]), by job name.
    overrides: HashMap<CompactString, JobOverride>,
    /// The storage advisor's marks ([`AdviceMark`]), by recommendation id.
    advice: HashMap<String, AdviceMark>,
    /// Something changed since the last flush.
    dirty: bool,
}

/// The workload statistics of one instance (module docs). Shared (`Arc`) between the front
/// ends, the graph's lock and the scheduler; every method takes `&self` (one mutex, held for a
/// few map operations per sample).
#[derive(Debug, Default)]
pub struct WorkloadStats {
    level: parking_lot::RwLock<WorkloadLevel>,
    inner: parking_lot::Mutex<Inner>,
}

/// The totals of one series over the three windows.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SeriesTotals {
    /// The series.
    pub key: SeriesKey,
    /// The last 60 minutes.
    pub last_hour: Counters,
    /// The last 48 hours.
    pub last_two_days: Counters,
    /// The last 90 days.
    pub last_ninety_days: Counters,
    /// The buckets of the tier the report was asked for ([`WorkloadStats::report_with_history`]),
    /// oldest first, only those with something counted; empty in a plain [`WorkloadStats::report`].
    pub history: Vec<Bucket>,
}

/// One bucket of a series' history: the figures of one minute, hour or day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Bucket {
    /// Its start, in microseconds since the Unix epoch (UTC).
    pub start: i64,
    /// What was counted in it.
    pub counters: Counters,
}

/// A tier of the history: one bucket per minute (the last 60), hour (the last 48) or day (the
/// last 90).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum HistoryTier {
    /// One bucket per minute, the last 60 minutes.
    Minutes,
    /// One bucket per hour, the last 48 hours.
    Hours,
    /// One bucket per day, the last 90 days.
    Days,
}

impl HistoryTier {
    /// `minutes`, `hours` or `days`.
    pub fn name(self) -> &'static str {
        match self {
            HistoryTier::Minutes => "minutes",
            HistoryTier::Hours => "hours",
            HistoryTier::Days => "days",
        }
    }

    /// The tier with this [name](Self::name).
    pub fn from_name(name: &str) -> Option<HistoryTier> {
        match name {
            "minutes" => Some(HistoryTier::Minutes),
            "hours" => Some(HistoryTier::Hours),
            "days" => Some(HistoryTier::Days),
            _ => None,
        }
    }

    fn index(self) -> usize {
        match self {
            HistoryTier::Minutes => 0,
            HistoryTier::Hours => 1,
            HistoryTier::Days => 2,
        }
    }
}

/// A snapshot of the statistics ([`WorkloadStats::report`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct WorkloadReport {
    /// The level the statistics are kept at.
    pub level: WorkloadLevel,
    /// Every series with its totals, by key.
    pub series: Vec<SeriesTotals>,
    /// The last run of every job that ran, by name.
    pub jobs: Vec<(String, JobRunRecord)>,
    /// The tier of every series' [`SeriesTotals::history`], `None` without history.
    pub history: Option<HistoryTier>,
}

impl Inner {
    /// Evicts the least used query series when [`MAX_QUERY_SERIES`] are kept.
    fn make_room_for_query(&mut self) {
        let queries = self
            .series
            .keys()
            .filter(|key| matches!(key.kind, SeriesKind::Query(_)))
            .count();
        if queries < MAX_QUERY_SERIES {
            return;
        }
        let least = self
            .series
            .iter()
            .filter(|(key, _)| matches!(key.kind, SeriesKind::Query(_)))
            .min_by_key(|(_, series)| series.tiers[2].sum_since(i64::MIN).count)
            .map(|(key, _)| key.clone());
        if let Some(key) = least {
            self.series.remove(&key);
        }
    }
}

impl WorkloadStats {
    /// Empty statistics at `level`.
    pub fn new(level: WorkloadLevel) -> Self {
        WorkloadStats {
            level: parking_lot::RwLock::new(level),
            inner: parking_lot::Mutex::new(Inner::default()),
        }
    }

    /// The memory these statistics hold, in bytes (an estimate from the capacities of their
    /// tables and buckets): shown as `workload_bytes` beside a graph's
    /// [`MemoryStats`](crate::graph::MemoryStats), never part of its `total_bytes`.
    pub fn memory_bytes(&self) -> u64 {
        use std::mem::size_of;
        let inner = self.inner.lock();
        let heap = |text: &CompactString| {
            if text.is_heap_allocated() {
                text.capacity() as u64
            } else {
                0
            }
        };
        let mut bytes = size_of::<WorkloadStats>() as u64;
        // The series table: one slot per capacity (key, series, control byte).
        bytes += inner.series.capacity() as u64
            * (size_of::<SeriesKey>() + size_of::<Series>() + 1) as u64;
        for (key, series) in &inner.series {
            bytes += match &key.kind {
                SeriesKind::Job(name) | SeriesKind::Lock(name) | SeriesKind::Query(name) => {
                    heap(name)
                }
                SeriesKind::Property(property) => {
                    heap(&property.key) + property.label.as_ref().map_or(0, heap)
                }
            };
            for tier in &series.tiers {
                bytes += (tier.buckets.capacity() * size_of::<(i64, Counters)>()) as u64;
            }
        }
        bytes += inner.jobs.capacity() as u64
            * (size_of::<CompactString>() + size_of::<JobRunRecord>() + 1) as u64;
        for (name, run) in &inner.jobs {
            bytes += heap(name) + run.error.as_ref().map_or(0, |e| e.capacity() as u64);
        }
        bytes += inner.overrides.capacity() as u64
            * (size_of::<CompactString>() + size_of::<JobOverride>() + 1) as u64;
        for (name, setting) in &inner.overrides {
            bytes += heap(name) + setting.schedule.as_ref().map_or(0, |s| s.capacity() as u64);
        }
        bytes += inner.advice.capacity() as u64
            * (size_of::<String>() + size_of::<AdviceMark>() + 1) as u64;
        for (id, mark) in &inner.advice {
            bytes += id.capacity() as u64
                + mark.applied.as_ref().map_or(0, |a| {
                    (size_of::<AppliedAdvice>() + a.kind.capacity() + a.title.capacity()) as u64
                });
        }
        bytes
    }

    /// The level the statistics are kept at.
    pub fn level(&self) -> WorkloadLevel {
        *self.level.read()
    }

    /// Changes the level. Switching off keeps what was collected (only [`Self::clear`] deletes).
    pub fn set_level(&self, level: WorkloadLevel) {
        *self.level.write() = level;
    }

    /// Counts `sample` in series `key` at `now` (microseconds since the Unix epoch). Nothing
    /// happens at level `off`.
    pub fn record(&self, key: SeriesKey, now: i64, sample: Sample) {
        if self.level() == WorkloadLevel::Off {
            return;
        }
        let mut inner = self.inner.lock();
        if matches!(key.kind, SeriesKind::Query(_)) && !inner.series.contains_key(&key) {
            inner.make_room_for_query();
        }
        let series = inner.series.entry(key).or_default();
        for (tier, (width, keep)) in series.tiers.iter_mut().zip(TIERS) {
            tier.add(now.div_euclid(width), keep, &sample);
        }
        inner.dirty = true;
    }

    /// Records the run of job `name` (its last run, and a sample of series [`SeriesKind::Job`]
    /// through [`Channel::Job`]).
    pub fn record_job_run(&self, name: &str, run: JobRunRecord) {
        if self.level() == WorkloadLevel::Off {
            return;
        }
        let sample = Sample {
            items: 0,
            micros: run.micros,
            failed: run.error.is_some(),
        };
        let key = SeriesKey::new(SeriesKind::Job(name.into()), Channel::Job);
        let end = run.started_at.saturating_add(run.micros as i64);
        self.record(key, end, sample);
        let mut inner = self.inner.lock();
        inner.jobs.insert(name.into(), run);
        inner.dirty = true;
    }

    /// The settings of the system job `name`, `None` when it has none.
    pub fn job_override(&self, name: &str) -> Option<JobOverride> {
        self.inner.lock().overrides.get(name).cloned()
    }

    /// Every system job's settings, by name.
    pub fn job_overrides(&self) -> Vec<(String, JobOverride)> {
        let mut all: Vec<(String, JobOverride)> = self
            .inner
            .lock()
            .overrides
            .iter()
            .map(|(name, setting)| (name.to_string(), setting.clone()))
            .collect();
        all.sort_by(|a, b| a.0.cmp(&b.0));
        all
    }

    /// Gives the system job `name` the settings `setting` (an empty one removes them: the
    /// built-in schedule and switch again). The host applies them to its scheduler; a Store
    /// applies them to the system jobs it hands out.
    pub fn set_job_override(&self, name: &str, setting: JobOverride) {
        let mut inner = self.inner.lock();
        if setting.is_empty() {
            inner.overrides.remove(name);
        } else {
            inner.overrides.insert(name.into(), setting);
        }
        inner.dirty = true;
    }

    /// Every storage-advisor mark, by recommendation id.
    pub fn advice_marks(&self) -> Vec<(String, AdviceMark)> {
        let mut all: Vec<(String, AdviceMark)> = self
            .inner
            .lock()
            .advice
            .iter()
            .map(|(id, mark)| (id.clone(), mark.clone()))
            .collect();
        all.sort_by(|a, b| a.0.cmp(&b.0));
        all
    }

    /// Changes the mark of recommendation `id` with `change` (an empty result removes it).
    pub fn mark_advice(&self, id: &str, change: impl FnOnce(&mut AdviceMark)) {
        let mut inner = self.inner.lock();
        let mut mark = inner.advice.remove(id).unwrap_or_default();
        change(&mut mark);
        if !mark.is_empty() {
            inner.advice.insert(id.to_string(), mark);
        }
        inner.dirty = true;
    }

    /// The totals of every series at `now` and the last job runs.
    pub fn report(&self, now: i64) -> WorkloadReport {
        self.report_in(now, None)
    }

    /// [`Self::report`] with every series' buckets of `tier` within its window (the data of a
    /// chart: runs, failures, mean and max time per minute, hour or day).
    pub fn report_with_history(&self, now: i64, tier: HistoryTier) -> WorkloadReport {
        self.report_in(now, Some(tier))
    }

    fn report_in(&self, now: i64, history: Option<HistoryTier>) -> WorkloadReport {
        let inner = self.inner.lock();
        let mut series: Vec<SeriesTotals> = inner
            .series
            .iter()
            .map(|(key, series)| {
                let since = |tier: usize, windows: i64| {
                    let (width, _) = TIERS[tier];
                    series.tiers[tier].sum_since(now.div_euclid(width) - windows + 1)
                };
                let history = history.map_or_else(Vec::new, |tier| {
                    let (width, keep) = TIERS[tier.index()];
                    let first = now.div_euclid(width) - keep as i64 + 1;
                    series.tiers[tier.index()]
                        .buckets
                        .iter()
                        .filter(|(index, _)| *index >= first)
                        .map(|(index, counters)| Bucket {
                            start: index.saturating_mul(width),
                            counters: *counters,
                        })
                        .collect()
                });
                SeriesTotals {
                    key: key.clone(),
                    last_hour: since(0, 60),
                    last_two_days: since(1, 48),
                    last_ninety_days: since(2, 90),
                    history,
                }
            })
            .collect();
        series.sort_by(|a, b| a.key.cmp(&b.key));
        let mut jobs: Vec<(String, JobRunRecord)> = inner
            .jobs
            .iter()
            .map(|(name, run)| (name.to_string(), run.clone()))
            .collect();
        jobs.sort_by(|a, b| a.0.cmp(&b.0));
        WorkloadReport {
            level: self.level(),
            series,
            jobs,
            history,
        }
    }

    /// Deletes series: all of them (`None`), or those `keep` returns `false` for.
    pub fn clear(&self, keep: Option<&dyn Fn(&SeriesKey) -> bool>) {
        let mut inner = self.inner.lock();
        match keep {
            None => {
                inner.series.clear();
                inner.jobs.clear();
            }
            Some(keep) => inner.series.retain(|key, _| keep(key)),
        }
        inner.dirty = true;
    }

    /// Deletes every bucket that ends before `before` (microseconds since the Unix epoch), in
    /// every tier; series left empty go.
    pub fn clear_before(&self, before: i64) {
        let mut inner = self.inner.lock();
        for series in inner.series.values_mut() {
            for (tier, (width, _)) in series.tiers.iter_mut().zip(TIERS) {
                tier.buckets
                    .retain(|(index, _)| (index + 1) * width > before);
            }
        }
        inner
            .series
            .retain(|_, series| series.tiers.iter().any(|tier| !tier.buckets.is_empty()));
        inner.dirty = true;
    }

    /// Whether something changed since the last [`Self::take_dirty`] (and resets it).
    #[cfg_attr(not(feature = "persist"), allow(dead_code))]
    pub(crate) fn take_dirty(&self) -> bool {
        std::mem::take(&mut self.inner.lock().dirty)
    }

    /// Marks the statistics changed (a write that failed is retried by the next flush).
    #[cfg_attr(not(feature = "persist"), allow(dead_code))]
    pub(crate) fn mark_dirty(&self) {
        self.inner.lock().dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(name: &str) -> SeriesKey {
        SeriesKey::new(SeriesKind::Job(name.into()), Channel::Job)
    }

    #[test]
    fn samples_fill_every_tier_and_old_buckets_expire() {
        let stats = WorkloadStats::new(WorkloadLevel::Memory);
        let t0 = 1_800_000_000_000_000i64;
        stats.record(job("a"), t0, Sample::new(3, 100));
        stats.record(job("a"), t0 + 10_000_000, Sample::new(2, 50).failed());
        let report = stats.report(t0 + 20_000_000);
        let a = &report.series[0];
        assert_eq!(a.last_hour.count, 2);
        assert_eq!(a.last_hour.failed, 1);
        assert_eq!(a.last_hour.items, 5);
        assert_eq!(a.last_hour.total_micros, 150);
        assert_eq!(a.last_hour.max_micros, 100);
        assert_eq!(a.last_ninety_days, a.last_hour);
        // Two hours later: the minute tier no longer has them, the hour tier does.
        let later = t0 + 2 * HOUR;
        stats.record(job("a"), later, Sample::new(1, 1));
        let report = stats.report(later);
        assert_eq!(report.series[0].last_hour.count, 1);
        assert_eq!(report.series[0].last_two_days.count, 3);
        // 100 days later only the new sample's day is left.
        let much_later = t0 + 100 * DAY;
        stats.record(job("a"), much_later, Sample::new(1, 1));
        assert_eq!(stats.report(much_later).series[0].last_ninety_days.count, 1);
    }

    #[test]
    fn off_records_nothing_and_clear_deletes() {
        let stats = WorkloadStats::new(WorkloadLevel::Off);
        stats.record(job("a"), 0, Sample::new(1, 1));
        assert!(stats.report(0).series.is_empty());
        stats.set_level(WorkloadLevel::Memory);
        stats.record(job("a"), 0, Sample::new(1, 1));
        stats.record(job("b"), 0, Sample::new(1, 1));
        stats.clear(Some(&|key: &SeriesKey| key == &job("a")));
        assert_eq!(stats.report(0).series.len(), 1);
        stats.clear_before(DAY * 2);
        assert!(stats.report(0).series.is_empty());
        stats.record_job_run(
            "flush",
            JobRunRecord {
                started_at: 0,
                micros: 5,
                error: None,
            },
        );
        assert_eq!(stats.report(0).jobs.len(), 1);
        stats.clear(None);
        assert!(stats.report(0).jobs.is_empty());
    }

    #[test]
    fn query_series_stay_bounded() {
        let stats = WorkloadStats::new(WorkloadLevel::Memory);
        let query =
            |i: usize| SeriesKey::new(SeriesKind::Query(format!("q{i}").into()), Channel::Cli);
        // q0 runs often, the others once.
        for _ in 0..5 {
            stats.record(query(0), 0, Sample::new(1, 1));
        }
        for i in 1..=MAX_QUERY_SERIES + 10 {
            stats.record(query(i), 0, Sample::new(1, 1));
        }
        let report = stats.report(0);
        assert_eq!(report.series.len(), MAX_QUERY_SERIES);
        assert!(
            report.series.iter().any(|s| s.key == query(0)),
            "the frequent one stays"
        );
        // Other kinds are never evicted for queries.
        stats.record(job("j"), 0, Sample::new(1, 1));
        assert_eq!(stats.report(0).series.len(), MAX_QUERY_SERIES + 1);
    }
}
