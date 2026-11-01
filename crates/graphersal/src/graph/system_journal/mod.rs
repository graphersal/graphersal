//! The system journal (book page "System Journal", format spec 4.7): a durable, append-only
//! stream of declared OPERATIONAL events a person reads later (backups, checkpoints, damage,
//! recovery at open, permission denials, clock steps), beside the WAL (data), the workload
//! statistics (numbers) and `tracing` (debug output).
//!
//! Every graph has one ([`crate::graph::Graph::system_journal`]): in memory (the newest
//! [`MEMORY_CAPACITY`] records), or the Store's, which keeps it in `journal/` segments. Records
//! are written by the engine only (declared kinds, `catalogue.rs`); hosts read them
//! ([`SystemJournal::query`], [`SystemJournal::subscribe`]) and forward them
//! ([`JournalSink`]). The journal decides nothing: a failed write never fails an operation.

pub(crate) mod catalogue;
pub(crate) mod collapse;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use crate::graph::CommitClock;
use crate::graph::clock::ClockObserver;
use crate::value::{ElementProperty, PropertyMap};
use catalogue::EventKind;

/// How many records a journal keeps in memory: all of an in-memory journal (older ones are
/// gone, [`JournalPage::truncated`]), the newest of a stored one (older ones are read from its
/// segments).
pub const MEMORY_CAPACITY: usize = 10_000;

/// The default page size of a [`JournalQuery`].
pub const DEFAULT_QUERY_LIMIT: usize = 1000;

/// The largest page a [`JournalQuery`] returns.
pub const MAX_QUERY_LIMIT: usize = 10_000;

/// The level of a record, most severe first: `Error < Warn < Notice < Info` in this order, so
/// "level at least warn" is `level <= Level::Warn` ([`Level::is_at_least`]).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    /// Something failed or is damaged (synced at once).
    Error,
    /// Something needs attention.
    Warn,
    /// A significant operation completed (backups, prunes, rollbacks).
    Notice,
    /// Routine operations (opens, closes, checkpoints).
    Info,
}

impl Level {
    /// Every level, most severe first.
    pub const ALL: [Level; 4] = [Level::Error, Level::Warn, Level::Notice, Level::Info];

    /// The name: `error`, `warn`, `notice`, `info`.
    pub fn name(self) -> &'static str {
        match self {
            Level::Error => "error",
            Level::Warn => "warn",
            Level::Notice => "notice",
            Level::Info => "info",
        }
    }

    /// The level named `name` (`warning` is `warn` too); `None` for another name.
    pub fn from_name(name: &str) -> Option<Level> {
        match name.to_ascii_lowercase().as_str() {
            "error" => Some(Level::Error),
            "warn" | "warning" => Some(Level::Warn),
            "notice" => Some(Level::Notice),
            "info" => Some(Level::Info),
            _ => None,
        }
    }

    /// Whether this level is `min` or more severe.
    pub fn is_at_least(self, min: Level) -> bool {
        self <= min
    }

    #[cfg_attr(not(feature = "persist"), allow(dead_code))]
    /// The byte stored in a record frame (0 to 3).
    pub(crate) fn code(self) -> u8 {
        match self {
            Level::Error => 0,
            Level::Warn => 1,
            Level::Notice => 2,
            Level::Info => 3,
        }
    }

    #[cfg_attr(not(feature = "persist"), allow(dead_code))]
    /// The level of a stored byte; a value of a newer version (above 3) reads as `info`, the
    /// least severe (format spec 4.7: kept and shown, never damage).
    pub(crate) fn from_code(code: u8) -> Level {
        match code {
            0 => Level::Error,
            1 => Level::Warn,
            2 => Level::Notice,
            _ => Level::Info,
        }
    }
}

impl std::fmt::Display for Level {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// The instance that wrote a record: a random id and a readable name (configured, else the host
/// name). A Store keeps its instance in the instance-local file `INSTANCE`, which no copy
/// carries: a restored backup on another machine is another instance.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InstanceId {
    /// The random id (a version-7 UUID).
    pub id: u128,
    /// The readable name.
    pub name: String,
}

impl InstanceId {
    /// An instance with `id` and `name`.
    pub fn new(id: u128, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
        }
    }

    /// A new random instance named `name`, else after the host (`HOSTNAME`, `COMPUTERNAME`,
    /// else `unknown`; `browser` in WebAssembly).
    pub(crate) fn generate(name: Option<&str>) -> Self {
        let name = match name {
            Some(name) if !name.is_empty() => name.to_string(),
            _ => default_instance_name(),
        };
        Self::new(crate::value::uuid::new_v7(), name)
    }
}

/// The instance name when none is configured.
pub(crate) fn default_instance_name() -> String {
    #[cfg(target_arch = "wasm32")]
    {
        "browser".to_string()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        ["HOSTNAME", "COMPUTERNAME"]
            .iter()
            .find_map(|key| {
                std::env::var(key)
                    .ok()
                    .filter(|name| !name.trim().is_empty())
            })
            .or_else(os_host_name)
            .map(|name| name.trim().chars().take(255).collect())
            .unwrap_or_else(|| "unknown".to_string())
    }
}

/// The operating system's host name (`uname`'s node name) when the environment exports none:
/// macOS shells and launchd services have no `HOSTNAME`, so a store's records named their
/// instance `unknown` there.
#[cfg(not(target_arch = "wasm32"))]
fn os_host_name() -> Option<String> {
    #[cfg(all(unix, feature = "persist"))]
    {
        let uname = rustix::system::uname();
        let name = uname.nodename().to_string_lossy().trim().to_string();
        (!name.is_empty()).then_some(name)
    }
    #[cfg(not(all(unix, feature = "persist")))]
    {
        None
    }
}

impl std::fmt::Display for InstanceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} ({})",
            self.name,
            crate::value::uuid::format_canonical(self.id)
        )
    }
}

/// One record of the system journal: a fixed envelope plus typed fields. Records hold no data
/// values, query text or secrets: names, counts, durations and ids only.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct JournalRecord {
    /// The local sequence number: monotonic, never reused (the cursor of a reader).
    pub seq: u64,
    /// The instance that created the record.
    pub origin: InstanceId,
    /// Its sequence number there: `(origin, origin_seq)` identifies a record everywhere.
    pub origin_seq: u64,
    /// When, microseconds since the Unix epoch (UTC), from the instance clock: monotonic per
    /// origin, never before a commit it is about.
    pub time: i64,
    /// The level.
    pub level: Level,
    /// Where it comes from, a dotted hierarchy (`store.backup`).
    pub source: String,
    /// The stable event code (`store.backup_done`); an unknown kind (a newer version's) is
    /// shown by its message and fields.
    pub kind: String,
    /// The rendered English text.
    pub message: String,
    /// The operation it belongs to (a job run, one backup).
    pub op: Option<u64>,
    /// The parent operation.
    pub parent: Option<u64>,
    /// The commit in the WAL it relates to.
    pub commit: Option<u64>,
    /// Who: the channel the work came through (`cli`, `mcp`, `playground`, `python`, `host`).
    pub principal: Option<String>,
    /// The typed fields (names, counts, durations, ids), in declaration order.
    pub fields: Vec<(String, ElementProperty)>,
}

impl JournalRecord {
    /// The value of field `name`.
    pub fn field(&self, name: &str) -> Option<&ElementProperty> {
        self.fields
            .iter()
            .find(|(field, _)| field == name)
            .map(|(_, value)| value)
    }

    /// The record as a map (the form every front end shows): `seq`, `origin` (`{id, name}`),
    /// `origin_seq`, `time` (µs), `level`, `source`, `kind`, `message`, `op`, `parent`,
    /// `commit` and `principal` when set, `fields`.
    pub fn to_property(&self) -> ElementProperty {
        let mut map = PropertyMap::new();
        let int = collapse::int;
        map.insert("seq", int(self.seq));
        let origin: PropertyMap = [
            (
                "id",
                ElementProperty::from(
                    crate::value::uuid::format_canonical(self.origin.id).to_string(),
                ),
            ),
            ("name", ElementProperty::from(self.origin.name.as_str())),
        ]
        .into_iter()
        .collect();
        map.insert("origin", ElementProperty::Object(Box::new(origin)));
        map.insert("origin_seq", int(self.origin_seq));
        map.insert("time", ElementProperty::from(self.time));
        map.insert("level", ElementProperty::from(self.level.name()));
        map.insert("source", ElementProperty::from(self.source.as_str()));
        map.insert("kind", ElementProperty::from(self.kind.as_str()));
        map.insert("message", ElementProperty::from(self.message.as_str()));
        if let Some(op) = self.op {
            map.insert("op", int(op));
        }
        if let Some(parent) = self.parent {
            map.insert("parent", int(parent));
        }
        if let Some(commit) = self.commit {
            map.insert("commit", int(commit));
        }
        if let Some(principal) = &self.principal {
            map.insert("principal", ElementProperty::from(principal.as_str()));
        }
        let fields: PropertyMap = self
            .fields
            .iter()
            .map(|(name, value)| (name.as_str(), value.clone()))
            .collect();
        map.insert("fields", ElementProperty::Object(Box::new(fields)));
        ElementProperty::Object(Box::new(map))
    }

    /// [`Self::to_property`] as compact JSON text.
    pub fn to_json(&self) -> String {
        crate::value::physical::to_json_text(&self.to_property()).unwrap_or_default()
    }

    /// The record as text: one header line with the time (UTC), level, kind, message and the
    /// one-line fields in parentheses. A field whose value spans several lines (a
    /// `recommendation` in the note layout: a headline, then each command on its own line
    /// under a `# comment`) follows on lines of its own, indented by two spaces: `name:` and
    /// the value's first line, then its other lines as they are, indented by two more spaces.
    /// The header line is always the first line, so a reader splitting records by line finds
    /// one record per unindented line.
    pub fn to_line(&self) -> String {
        let time = jiff::Timestamp::from_microsecond(self.time)
            .map(|t| t.strftime("%Y-%m-%dT%H:%M:%S%.6fZ").to_string())
            .unwrap_or_else(|_| self.time.to_string());
        let mut line = format!(
            "{} #{} {:<6} {} {}",
            time,
            self.seq,
            self.level.name(),
            self.kind,
            self.message
        );
        let (inline, below): (Vec<_>, Vec<_>) = self
            .fields
            .iter()
            .map(|(name, value)| (name, field_text(value)))
            .partition(|(_, text)| !text.contains('\n'));
        if !inline.is_empty() {
            let inline: Vec<String> = inline
                .into_iter()
                .map(|(name, text)| format!("{name}={text}"))
                .collect();
            line.push_str(" (");
            line.push_str(&inline.join(", "));
            line.push(')');
        }
        for (name, text) in below {
            let mut lines = text.lines();
            line.push_str(&format!("\n  {name}: {}", lines.next().unwrap_or_default()));
            for rest in lines {
                line.push_str(&format!("\n  {rest}"));
            }
        }
        line
    }
}

/// The display text of one field value.
fn field_text(value: &ElementProperty) -> String {
    crate::value::physical::to_display_text(value.logical().as_ref()).to_string()
}

/// `name=value, ...` of `fields` (display text).
pub(crate) fn render_fields(fields: &[(String, ElementProperty)]) -> String {
    fields
        .iter()
        .map(|(name, value)| format!("{name}={}", field_text(value)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A question to the journal: the records after a cursor that match every filter set, oldest
/// first; or, with [`Self::newest_first`], the NEWEST matching records (below the cursor
/// [`Self::before`] when set), newest first: a reader shows the end of a long journal at once
/// and pages backwards from [`JournalPage::next_before`], while [`JournalPage::next_after`]
/// follows what comes next.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalQuery {
    /// Only records with `seq` greater than this (the cursor; 0: from the start).
    pub after: u64,
    /// Only records with `seq` less than this (the cursor of a newest-first reader paging
    /// backwards; `None`: up to the newest).
    pub before: Option<u64>,
    /// The newest matching records, newest first, instead of the oldest after the cursor
    /// (stored segments are read from the end, one at a time).
    pub newest_first: bool,
    /// Only records at or after this time (µs).
    pub since: Option<i64>,
    /// Only records before this time (µs).
    pub until: Option<i64>,
    /// Only records at this level or more severe.
    pub min_level: Option<Level>,
    /// Only records whose source is this one or below it (`store` matches `store.backup`).
    pub source_prefix: Option<String>,
    /// Only records of this kind.
    pub kind: Option<String>,
    /// Only records of this operation.
    pub op: Option<u64>,
    /// Only records created by this instance id.
    pub origin: Option<u128>,
    /// The most records returned (1 to [`MAX_QUERY_LIMIT`]; default
    /// [`DEFAULT_QUERY_LIMIT`]).
    pub limit: usize,
}

impl Default for JournalQuery {
    fn default() -> Self {
        Self {
            after: 0,
            before: None,
            newest_first: false,
            since: None,
            until: None,
            min_level: None,
            source_prefix: None,
            kind: None,
            op: None,
            origin: None,
            limit: DEFAULT_QUERY_LIMIT,
        }
    }
}

impl JournalQuery {
    /// Every record from the start, [`DEFAULT_QUERY_LIMIT`] at a time.
    pub fn new() -> Self {
        Self::default()
    }

    /// Only records after `seq` (the cursor of [`JournalPage::next_after`]).
    pub fn after(mut self, seq: u64) -> Self {
        self.after = seq;
        self
    }

    /// Only records with `seq` before `seq` (the cursor of [`JournalPage::next_before`]).
    pub fn before(mut self, seq: u64) -> Self {
        self.before = Some(seq);
        self
    }

    /// The newest matching records, newest first ([`JournalQuery`]).
    pub fn newest_first(mut self) -> Self {
        self.newest_first = true;
        self
    }

    /// Only records at or after `time` (µs since the Unix epoch).
    pub fn since(mut self, time: i64) -> Self {
        self.since = Some(time);
        self
    }

    /// Only records before `time` (µs since the Unix epoch).
    pub fn until(mut self, time: i64) -> Self {
        self.until = Some(time);
        self
    }

    /// Only records at `level` or more severe.
    pub fn min_level(mut self, level: Level) -> Self {
        self.min_level = Some(level);
        self
    }

    /// Only records whose source is `prefix` or below it.
    pub fn source_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.source_prefix = Some(prefix.into());
        self
    }

    /// Only records of `kind`.
    pub fn kind(mut self, kind: impl Into<String>) -> Self {
        self.kind = Some(kind.into());
        self
    }

    /// Only records of operation `op`.
    pub fn op(mut self, op: u64) -> Self {
        self.op = Some(op);
        self
    }

    /// Only records created by instance `id`.
    pub fn origin(mut self, id: u128) -> Self {
        self.origin = Some(id);
        self
    }

    /// At most `limit` records (clamped to 1 to [`MAX_QUERY_LIMIT`]).
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    /// The page size in force.
    pub(crate) fn page_size(&self) -> usize {
        self.limit.clamp(1, MAX_QUERY_LIMIT)
    }

    /// The exclusive upper `seq` bound (`u64::MAX`: none).
    pub(crate) fn upper(&self) -> u64 {
        self.before.unwrap_or(u64::MAX)
    }

    /// Whether `record` passes every filter but the cursor.
    pub(crate) fn matches(&self, record: &JournalRecord) -> bool {
        self.since.is_none_or(|since| record.time >= since)
            && self.until.is_none_or(|until| record.time < until)
            && self
                .min_level
                .is_none_or(|level| record.level.is_at_least(level))
            && self.source_prefix.as_deref().is_none_or(|prefix| {
                prefix.is_empty()
                    || record.source == prefix
                    || record
                        .source
                        .strip_prefix(prefix)
                        .is_some_and(|rest| rest.starts_with('.'))
            })
            && self.kind.as_deref().is_none_or(|kind| record.kind == kind)
            && self.op.is_none_or(|op| record.op == Some(op))
            && self.origin.is_none_or(|origin| record.origin.id == origin)
    }
}

/// Fills a newest-first page from `records` (newest first, at most `limit + 1`): `more` when
/// there is one beyond the limit, the cursors from the newest covered `end` and the oldest
/// record kept.
pub(crate) fn finish_newest(
    page: &mut JournalPage,
    mut records: Vec<JournalRecord>,
    limit: usize,
    query: &JournalQuery,
    end: u64,
) {
    if records.len() > limit {
        records.truncate(limit);
        page.more = true;
    }
    page.next_after = end.min(query.upper().saturating_sub(1)).max(query.after);
    page.next_before = Some(match (page.more, records.last()) {
        (true, Some(oldest)) => oldest.seq,
        _ => query.after.saturating_add(1),
    });
    page.records = records;
}

/// One page of records.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Default)]
pub struct JournalPage {
    /// The records, in `seq` order (newest first for a [`JournalQuery::newest_first`] query).
    pub records: Vec<JournalRecord>,
    /// The cursor of the next page ([`JournalQuery::after`]): the last record looked at (of a
    /// newest-first page: the newest record covered, so a reader goes on with what comes
    /// next).
    pub next_after: u64,
    /// Of a newest-first page: the cursor of the next OLDER page ([`JournalQuery::before`]),
    /// the oldest record looked at; `None` for an oldest-first page.
    pub next_before: Option<u64>,
    /// Older records the query asked for are gone (an in-memory journal keeps the newest
    /// [`MEMORY_CAPACITY`]).
    pub truncated: bool,
    /// More matching records follow: ask again with [`Self::next_after`] (of a newest-first
    /// page: OLDER matching records exist, ask with [`Self::next_before`]).
    pub more: bool,
    /// Problems met while reading stored segments (damaged frames skipped, unreadable
    /// segments): never an error, the journal decides nothing.
    pub notes: Vec<String>,
}

impl JournalPage {
    /// The page as a map: `records` (each [`JournalRecord::to_property`]), `next_after`,
    /// `next_before` (a newest-first page only), `truncated`, `more`, `notes`.
    pub fn to_property(&self) -> ElementProperty {
        let mut map = PropertyMap::new();
        map.insert(
            "records",
            ElementProperty::Array(self.records.iter().map(|r| r.to_property()).collect()),
        );
        map.insert("next_after", collapse::int(self.next_after));
        if let Some(before) = self.next_before {
            map.insert("next_before", collapse::int(before));
        }
        map.insert("truncated", ElementProperty::from(self.truncated));
        map.insert("more", ElementProperty::from(self.more));
        map.insert(
            "notes",
            ElementProperty::Array(
                self.notes
                    .iter()
                    .map(|note| ElementProperty::from(note.as_str()))
                    .collect(),
            ),
        );
        ElementProperty::Object(Box::new(map))
    }

    /// [`Self::to_property`] as compact JSON text.
    pub fn to_json(&self) -> String {
        crate::value::physical::to_json_text(&self.to_property()).unwrap_or_default()
    }
}

/// A host's receiver of journal records (its logging, a SIEM): called with every WRITTEN
/// record (the collapsed stream), after the journal's lock is released, in `seq` order. A
/// panicking sink is caught and counted ([`SystemJournal::sink_failures`]); it must not block
/// for long (the emitting operation waits for it).
pub trait JournalSink: Send + Sync {
    /// Receives one record.
    fn record(&self, record: &JournalRecord);

    /// A name for diagnostics.
    fn name(&self) -> &str;
}

/// The handle of a registered [`JournalSink`] ([`SystemJournal::remove_sink`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SinkId(u64);

/// Why the journal could not answer.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JournalError {
    /// The journal is not available here.
    #[error("the system journal is not available: {reason}")]
    #[non_exhaustive]
    Unavailable {
        /// Why.
        reason: String,
    },
    /// Reading or writing its files failed.
    #[error("the system journal could not {context}: {reason}")]
    #[non_exhaustive]
    Io {
        /// What was being done.
        context: String,
        /// The I/O error.
        reason: String,
    },
    /// Its files are damaged where they had to be read.
    #[error("the system journal is damaged at {location}: {reason}")]
    #[non_exhaustive]
    Damaged {
        /// The file.
        location: String,
        /// What is wrong.
        reason: String,
    },
    /// The process memory ceiling refused reading a segment whole.
    #[error("the system journal could not be read under the memory ceiling: {reason}")]
    #[non_exhaustive]
    MemoryLimit {
        /// The refusal.
        reason: String,
    },
    /// The operation is not supported here.
    #[error("the system journal does not support {operation}")]
    #[non_exhaustive]
    Unsupported {
        /// The operation.
        operation: String,
    },
}

impl JournalError {
    /// What to do about it.
    pub fn help(&self) -> &'static str {
        match self {
            JournalError::Unavailable { .. } => {
                "Open the store (or the graph) the journal belongs to; a closed store's journal \
                 is read with Store::journal_dir or `graphersal store journal <store>`."
            }
            JournalError::Io { .. } => {
                "Check that the store's directory is readable (permissions, disk). The journal \
                 decides nothing: the store itself is not affected."
            }
            JournalError::Damaged { .. } => {
                "Damaged journal records are skipped; `graphersal store verify` lists them as \
                 notes. The store's data is not affected."
            }
            JournalError::MemoryLimit { .. } => {
                "The process memory ceiling refused reading a journal segment whole: free memory \
                 or raise the ceiling (book page \"Resource Limits\"), then ask again."
            }
            JournalError::Unsupported { .. } => {
                "This journal cannot do that here (an in-memory journal has no files)."
            }
        }
    }

    pub(crate) fn io(context: impl Into<String>, reason: impl std::fmt::Display) -> Self {
        JournalError::Io {
            context: context.into(),
            reason: reason.to_string(),
        }
    }
}

/// A reader that follows the journal: every record after its cursor, then new ones as they are
/// written ([`SystemJournal::subscribe`]).
pub struct JournalSubscription {
    journal: Arc<SystemJournal>,
    after: u64,
}

impl std::fmt::Debug for JournalSubscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JournalSubscription")
            .field("after", &self.after)
            .finish_non_exhaustive()
    }
}

impl JournalSubscription {
    /// The cursor: the last record returned.
    pub fn after(&self) -> u64 {
        self.after
    }

    /// The records written since the last call, without waiting (at most
    /// [`MAX_QUERY_LIMIT`]; call again for more).
    pub fn poll(&mut self) -> Result<Vec<JournalRecord>, JournalError> {
        let page = self
            .journal
            .query(&JournalQuery::new().after(self.after).limit(MAX_QUERY_LIMIT))?;
        self.after = page.next_after;
        Ok(page.records)
    }

    /// Like [`Self::poll`], but waits up to `timeout` for a record when there is none yet
    /// (an empty answer: none came). In WebAssembly it does not wait (no threads).
    pub fn wait(
        &mut self,
        timeout: std::time::Duration,
    ) -> Result<Vec<JournalRecord>, JournalError> {
        let records = self.poll()?;
        if !records.is_empty() {
            return Ok(records);
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut state = self.journal.state.lock();
            if state.next_seq <= self.after.saturating_add(1) {
                self.journal.changed.wait_for(&mut state, timeout);
            }
        }
        #[cfg(target_arch = "wasm32")]
        let _ = timeout;
        self.poll()
    }
}

/// A record about to be written (the collapser's unit).
#[derive(Clone)]
pub(crate) struct Draft {
    pub(crate) kind: &'static EventKind,
    pub(crate) level: Level,
    pub(crate) message: String,
    pub(crate) op: Option<u64>,
    pub(crate) parent: Option<u64>,
    pub(crate) commit: Option<u64>,
    pub(crate) principal: Option<String>,
    pub(crate) fields: Vec<(String, ElementProperty)>,
    pub(crate) time: i64,
}

/// What the engine reports (crate-internal: hosts do not write records): a message plus the
/// envelope's optional parts and the kind's declared fields.
pub(crate) struct Event {
    message: String,
    level: Option<Level>,
    op: Option<u64>,
    parent: Option<u64>,
    commit: Option<u64>,
    principal: Option<String>,
    fields: Vec<(&'static str, ElementProperty)>,
}

impl Event {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            level: None,
            op: None,
            parent: None,
            commit: None,
            principal: None,
            fields: Vec::new(),
        }
    }

    #[cfg_attr(not(feature = "persist"), allow(dead_code))]
    /// Overrides the kind's level.
    pub(crate) fn level(mut self, level: Level) -> Self {
        self.level = Some(level);
        self
    }

    #[allow(dead_code)] // used by the packages that run operations with several records
    pub(crate) fn op(mut self, op: u64) -> Self {
        self.op = Some(op);
        self
    }

    #[allow(dead_code)]
    pub(crate) fn parent(mut self, parent: u64) -> Self {
        self.parent = Some(parent);
        self
    }

    #[cfg_attr(not(feature = "persist"), allow(dead_code))]
    pub(crate) fn commit(mut self, commit: u64) -> Self {
        self.commit = Some(commit);
        self
    }

    pub(crate) fn principal(mut self, principal: impl Into<String>) -> Self {
        self.principal = Some(principal.into());
        self
    }

    /// A declared field.
    pub(crate) fn field(mut self, name: &'static str, value: impl Into<ElementProperty>) -> Self {
        self.fields.push((name, value.into()));
        self
    }

    /// A declared count/position field.
    pub(crate) fn count(self, name: &'static str, value: u64) -> Self {
        self.field(name, collapse::int(value))
    }

    fn into_draft(self, kind: &'static EventKind, time: i64) -> Draft {
        debug_assert!(
            catalogue::EVENT_KINDS
                .iter()
                .any(|declared| std::ptr::eq(*declared, kind)),
            "undeclared journal event kind {}",
            kind.code
        );
        debug_assert!(
            self.fields
                .iter()
                .all(|(name, _)| kind.fields.iter().any(|field| field.name == *name)),
            "undeclared field of journal event {}",
            kind.code
        );
        Draft {
            kind,
            level: self.level.unwrap_or(kind.level),
            message: self.message,
            op: self.op,
            parent: self.parent,
            commit: self.commit,
            // Who: the channel the work came through when the event names nobody (the dev
            // server's requests run as `playground` or `mcp`, the CLI as `cli`, Python as
            // `python`, anything else as `host`).
            principal: self
                .principal
                .or_else(|| Some(crate::graph::workload::current_channel().name().to_string())),
            fields: self
                .fields
                .into_iter()
                .map(|(name, value)| (name.to_string(), value))
                .collect(),
            time,
        }
    }
}

/// A failed write of journal records.
pub(crate) struct WriteFailure {
    pub(crate) reason: String,
    /// How many records it lost.
    pub(crate) lost: u64,
}

/// Where a stored journal's records go (feature `persist`: the Store's segment writer).
pub(crate) trait JournalWriter: Send {
    /// Appends one record; `urgent` (an `error` record) makes it (and every record before it)
    /// durable at once, otherwise it is written in a batch later ([`Self::sync`]).
    fn append(&mut self, record: &JournalRecord, urgent: bool) -> Result<(), WriteFailure>;

    /// Writes and makes durable everything appended.
    fn sync(&mut self) -> Result<(), WriteFailure>;

    /// When the oldest record appended but not written yet was appended (`None`: nothing is
    /// waiting): the age the Store's journal flusher writes the batch at.
    fn pending_since(&self) -> Option<web_time::Instant> {
        None
    }
}

/// Reads a stored journal's records older than the memory holds.
pub(crate) trait PersistedReader: Send + Sync {
    /// The records with `after < seq < before` that match `query`, at most `limit`, plus
    /// notes about damage met.
    fn read(
        &self,
        after: u64,
        before: u64,
        query: &JournalQuery,
        limit: usize,
    ) -> Result<(Vec<JournalRecord>, Vec<String>), JournalError>;

    /// The NEWEST records with `after < seq < before` that match `query`, newest first, at
    /// most `limit`, plus notes about damage met: segments are read from the newest, one at a
    /// time, and reading stops once `limit` records are found.
    fn read_newest(
        &self,
        after: u64,
        before: u64,
        query: &JournalQuery,
        limit: usize,
    ) -> Result<(Vec<JournalRecord>, Vec<String>), JournalError>;
}

struct State {
    next_seq: u64,
    last_time: i64,
    ring: VecDeque<JournalRecord>,
    /// The smallest seq the ring answers for: older records are read from the segments, or
    /// gone.
    floor: u64,
    collapse: collapse::Collapser,
    writer: Option<Box<dyn JournalWriter>>,
    /// Records lost since the last successful write (reported by `journal.records_lost`).
    unreported_lost: u64,
    /// Written records not yet handed to the sinks and the `tracing` mirror.
    deliver: VecDeque<JournalRecord>,
}

/// The system journal of a graph or a Store (see the module documentation). `Send + Sync`,
/// shared through an `Arc`; every method that touches the shared journal returns a `Result`.
pub struct SystemJournal {
    instance: InstanceId,
    clock: CommitClock,
    capacity: usize,
    state: parking_lot::Mutex<State>,
    changed: parking_lot::Condvar,
    /// Serializes the hand-over to sinks (reentrant: a sink whose work reads the clock may emit).
    delivery: parking_lot::ReentrantMutex<()>,
    sinks: parking_lot::Mutex<Vec<(SinkId, Arc<dyn JournalSink>)>>,
    next_sink: AtomicU64,
    lost: AtomicU64,
    sink_failures: AtomicU64,
    reader: Option<Arc<dyn PersistedReader>>,
}

impl std::fmt::Debug for SystemJournal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SystemJournal")
            .field("instance", &self.instance)
            .field("lost", &self.lost_records())
            .finish_non_exhaustive()
    }
}

/// Reports a step back of the instance clock to the journal (the `ClockObserver` of P2).
struct StepBack(Weak<SystemJournal>);

impl ClockObserver for StepBack {
    fn stepped_back(&self, system: i64, floor: i64) {
        match self.0.upgrade() {
            Some(journal) => journal.emit_at(
                &catalogue::CLOCK_STEP_BACK,
                Event::new(format!(
                    "the system clock stepped back {} ms behind the instance clock",
                    (floor - system) / 1000
                ))
                .field("system_us", system)
                .field("floor_us", floor)
                .field("behind_ms", (floor - system) / 1000),
                floor,
            ),
            None => tracing::warn!(
                system_us = system,
                floor_us = floor,
                "the system clock stepped back behind the instance clock"
            ),
        }
    }
}

impl SystemJournal {
    /// An in-memory journal timed by `clock` (a graph without a store).
    pub(crate) fn in_memory(clock: CommitClock) -> Arc<Self> {
        Self::build(InstanceId::generate(None), clock, 1, Vec::new(), None, None)
    }

    /// A journal continuing at `next_seq`, holding the `primed` newest stored records in memory,
    /// writing through `writer` (none: read-only, new records only in memory) and reading older
    /// records through `reader`.
    pub(crate) fn build(
        instance: InstanceId,
        clock: CommitClock,
        next_seq: u64,
        primed: Vec<JournalRecord>,
        writer: Option<Box<dyn JournalWriter>>,
        reader: Option<Arc<dyn PersistedReader>>,
    ) -> Arc<Self> {
        let capacity = MEMORY_CAPACITY;
        let mut ring: VecDeque<JournalRecord> = primed.into_iter().collect();
        while ring.len() > capacity {
            ring.pop_front();
        }
        let last_time = ring.back().map_or(0, |record| record.time);
        let next_seq = next_seq
            .max(ring.back().map_or(1, |record| record.seq + 1))
            .max(1);
        let floor = ring.front().map_or(next_seq, |record| record.seq);
        let journal = Arc::new(SystemJournal {
            instance,
            clock: clock.clone(),
            capacity,
            state: parking_lot::Mutex::new(State {
                next_seq,
                last_time,
                ring,
                floor,
                collapse: collapse::Collapser::default(),
                writer,
                unreported_lost: 0,
                deliver: VecDeque::new(),
            }),
            changed: parking_lot::Condvar::new(),
            delivery: parking_lot::ReentrantMutex::new(()),
            sinks: parking_lot::Mutex::new(Vec::new()),
            next_sink: AtomicU64::new(1),
            lost: AtomicU64::new(0),
            sink_failures: AtomicU64::new(0),
            reader,
        });
        // The instance clock reports a step back here from now on (the first observer stays).
        clock.raise(last_time);
        clock.set_observer(Box::new(StepBack(Arc::downgrade(&journal))));
        journal
    }

    /// The records after `query.after` (and before `query.before`) that match its filters, at
    /// most `query.limit`: the oldest of them, or with [`JournalQuery::newest_first`] the
    /// newest, newest first.
    pub fn query(&self, query: &JournalQuery) -> Result<JournalPage, JournalError> {
        if query.newest_first {
            return self.query_newest(query);
        }
        let limit = query.page_size();
        let upper = query.upper();
        let (floor, end, ring_part, ring_more) = {
            let state = self.state.lock();
            let mut part = Vec::new();
            let mut more = false;
            for record in state
                .ring
                .iter()
                .filter(|record| record.seq > query.after && record.seq < upper)
            {
                if query.matches(record) {
                    if part.len() == limit {
                        more = true;
                        break;
                    }
                    part.push(record.clone());
                }
            }
            (state.floor, state.next_seq.saturating_sub(1), part, more)
        };
        let mut page = JournalPage::default();
        if query.after.saturating_add(1) < floor {
            match &self.reader {
                Some(reader) => {
                    let (records, notes) =
                        reader.read(query.after, floor.min(upper), query, limit + 1)?;
                    page.records = records;
                    page.notes = notes;
                }
                None => page.truncated = true,
            }
        }
        page.records.extend(ring_part);
        page.more = ring_more;
        if page.records.len() > limit {
            page.records.truncate(limit);
            page.more = true;
        }
        page.next_after = if page.more {
            page.records.last().map_or(query.after, |record| record.seq)
        } else {
            end.min(upper.saturating_sub(1)).max(query.after)
        };
        Ok(page)
    }

    /// [`Self::query`] of a newest-first query: the ring from its end, then (when the page is
    /// not full and older records are asked for) the stored segments from the newest.
    fn query_newest(&self, query: &JournalQuery) -> Result<JournalPage, JournalError> {
        let limit = query.page_size();
        let upper = query.upper();
        let (floor, end, mut records) = {
            let state = self.state.lock();
            let mut part = Vec::new();
            for record in state.ring.iter().rev() {
                if record.seq <= query.after || part.len() > limit {
                    break;
                }
                if record.seq < upper && query.matches(record) {
                    part.push(record.clone());
                }
            }
            (state.floor, state.next_seq.saturating_sub(1), part)
        };
        let mut page = JournalPage::default();
        let older_from = floor.min(upper);
        if records.len() <= limit && query.after.saturating_add(1) < older_from {
            match &self.reader {
                Some(reader) => {
                    let wanted = limit + 1 - records.len();
                    let (older, notes) =
                        reader.read_newest(query.after, older_from, query, wanted)?;
                    records.extend(older);
                    page.notes = notes;
                }
                None => page.truncated = true,
            }
        }
        finish_newest(&mut page, records, limit, query, end);
        Ok(page)
    }

    /// A reader of every record after `after`, then of new ones as they come.
    pub fn subscribe(self: &Arc<Self>, after: u64) -> Result<JournalSubscription, JournalError> {
        Ok(JournalSubscription {
            journal: Arc::clone(self),
            after,
        })
    }

    /// Registers `sink`: it receives every record written from now on.
    pub fn add_sink(&self, sink: Box<dyn JournalSink>) -> Result<SinkId, JournalError> {
        let id = SinkId(self.next_sink.fetch_add(1, Ordering::Relaxed));
        self.sinks.lock().push((id, Arc::from(sink)));
        Ok(id)
    }

    /// Unregisters a sink; `Ok(())` also when it is not registered (any more).
    pub fn remove_sink(&self, id: SinkId) -> Result<(), JournalError> {
        self.sinks.lock().retain(|(sink, _)| *sink != id);
        Ok(())
    }

    /// The current state of operations (a view, nothing stored): the last record of each
    /// operation (`op`) among the records in memory that has no `_done`/`_failed` record yet.
    pub fn open_operations(&self) -> Result<Vec<JournalRecord>, JournalError> {
        let state = self.state.lock();
        let mut open: Vec<JournalRecord> = Vec::new();
        for record in &state.ring {
            let Some(op) = record.op else { continue };
            open.retain(|last| last.op != Some(op));
            let finished = record.kind.ends_with("_done")
                || record.kind.ends_with("_failed")
                || record.kind.ends_with(".end");
            if !finished {
                open.push(record.clone());
            }
        }
        Ok(open)
    }

    /// The instance that writes this journal.
    pub fn instance(&self) -> InstanceId {
        self.instance.clone()
    }

    /// How many records could not be written to the store's segments (they were kept in memory
    /// only); 0 for an in-memory journal.
    pub fn lost_records(&self) -> u64 {
        self.lost.load(Ordering::Acquire)
    }

    /// How many times a sink panicked (each was caught).
    pub fn sink_failures(&self) -> u64 {
        self.sink_failures.load(Ordering::Acquire)
    }

    /// Whether the records are kept in a store's files (`false`: in memory only).
    pub fn is_stored(&self) -> bool {
        self.state.lock().writer.is_some() || self.reader.is_some()
    }

    /// Closes every open group of repeats (one `journal.repeated` record each) and makes every
    /// written record durable. A Store does this at checkpoints and at close.
    pub fn flush(&self) -> Result<(), JournalError> {
        let time = self.clock.now();
        let result = {
            let mut state = self.state.lock();
            self.close_groups(&mut state, time);
            match state.writer.as_mut().map(|writer| writer.sync()) {
                Some(Err(failure)) => {
                    self.note_lost(&mut state, &failure);
                    Err(JournalError::io("write its records", failure.reason))
                }
                _ => Ok(()),
            }
        };
        self.changed.notify_all();
        self.deliver();
        result
    }

    #[cfg_attr(not(feature = "persist"), allow(dead_code))]
    /// Makes the records written so far durable (the batched ones), without closing groups.
    pub(crate) fn sync(&self) {
        let mut state = self.state.lock();
        if let Some(Err(failure)) = state.writer.as_mut().map(|writer| writer.sync()) {
            self.note_lost(&mut state, &failure);
        }
    }

    #[cfg_attr(not(feature = "persist"), allow(dead_code))]
    /// Stops writing to the files at once, writing nothing more (a store frozen by damage):
    /// the records not written yet stay in memory only.
    pub(crate) fn drop_writer(&self) {
        self.state.lock().writer = None;
        self.changed.notify_all();
    }

    #[cfg_attr(
        any(not(feature = "persist"), target_arch = "wasm32"),
        allow(dead_code)
    )]
    /// Whether records are written to a store's files (the journal flusher stops without).
    pub(crate) fn has_writer(&self) -> bool {
        self.state.lock().writer.is_some()
    }

    #[cfg_attr(not(feature = "persist"), allow(dead_code))]
    /// When the oldest batched record (appended, not written yet) was appended; `None` when
    /// nothing waits.
    pub(crate) fn batched_since(&self) -> Option<web_time::Instant> {
        self.state
            .lock()
            .writer
            .as_ref()
            .and_then(|writer| writer.pending_since())
    }

    #[cfg_attr(
        any(not(feature = "persist"), target_arch = "wasm32"),
        allow(dead_code)
    )]
    /// Waits up to `timeout` for a change (a record, a flush, the writer dropped), returning at
    /// once when a record is batched already; then [`Self::batched_since`].
    pub(crate) fn wait_batched(&self, timeout: std::time::Duration) -> Option<web_time::Instant> {
        let mut state = self.state.lock();
        let waiting = |state: &State| {
            state
                .writer
                .as_ref()
                .and_then(|writer| writer.pending_since())
        };
        if waiting(&state).is_none() && state.writer.is_some() {
            let _ = self.changed.wait_for(&mut state, timeout);
        }
        waiting(&state)
    }

    #[cfg_attr(
        any(not(feature = "persist"), target_arch = "wasm32"),
        allow(dead_code)
    )]
    /// Waits up to `timeout` for any change of the journal.
    pub(crate) fn wait_changed(&self, timeout: std::time::Duration) {
        let mut state = self.state.lock();
        let _ = self.changed.wait_for(&mut state, timeout);
    }

    #[cfg_attr(
        any(not(feature = "persist"), target_arch = "wasm32"),
        allow(dead_code)
    )]
    /// Wakes every waiter (the journal flusher at a store's close).
    pub(crate) fn wake(&self) {
        self.changed.notify_all();
    }

    /// Counts the records a failed write lost (reported by the next `journal.records_lost`).
    fn note_lost(&self, state: &mut State, failure: &WriteFailure) {
        self.lost.fetch_add(failure.lost, Ordering::AcqRel);
        state.unreported_lost += failure.lost;
        tracing::warn!(
            reason = %failure.reason,
            lost = failure.lost,
            "system journal records could not be written; they are kept in memory only"
        );
    }

    #[cfg_attr(not(feature = "persist"), allow(dead_code))]
    /// Closes the groups, syncs and stops writing to the files (the Store closes); later
    /// records stay in memory.
    pub(crate) fn detach_writer(&self) {
        if let Err(err) = self.flush() {
            tracing::warn!(%err, "the system journal was not synced at close");
        }
        self.state.lock().writer = None;
        self.changed.notify_all();
    }

    /// Writes an event of `kind`, timed by the instance clock (collapsed per its declaration;
    /// never fails: a failed write is a `tracing` warning and a lost record).
    pub(crate) fn emit(&self, kind: &'static EventKind, event: Event) {
        let time = self.clock.now();
        self.emit_at(kind, event, time);
    }

    /// [`Self::emit`] of a kind that is never collapsed, returning the record's seq (the `op`
    /// of the records that end the operation it starts).
    pub(crate) fn emit_op(&self, kind: &'static EventKind, event: Event) -> u64 {
        let time = self.clock.now();
        let draft = event.into_draft(kind, time);
        let mut seq = 0;
        {
            let mut state = self.state.lock();
            let state = &mut *state;
            let mut collapser = std::mem::take(&mut state.collapse);
            collapser.admit(draft, &mut |draft| {
                seq = self.write_locked(state, draft);
                seq
            });
            state.collapse = collapser;
        }
        self.changed.notify_all();
        self.deliver();
        seq
    }

    /// [`Self::emit`] at `time` (never earlier than the last record).
    pub(crate) fn emit_at(&self, kind: &'static EventKind, event: Event, time: i64) {
        let draft = event.into_draft(kind, time);
        {
            let mut state = self.state.lock();
            let state = &mut *state;
            let mut collapser = std::mem::take(&mut state.collapse);
            collapser.admit(draft, &mut |draft| self.write_locked(state, draft));
            state.collapse = collapser;
        }
        self.changed.notify_all();
        self.deliver();
    }

    fn close_groups(&self, state: &mut State, time: i64) {
        let mut collapser = std::mem::take(&mut state.collapse);
        collapser.close_all(time, &mut |draft| self.write_locked(state, draft));
        state.collapse = collapser;
    }

    /// Assigns the seq, writes the record (and a pending `journal.records_lost` after a failed
    /// write), keeps it in memory and queues it for the sinks. Returns its seq.
    fn write_locked(&self, state: &mut State, draft: Draft) -> u64 {
        let record = self.record_of(state, draft);
        let seq = record.seq;
        let urgent = record.level == Level::Error;
        let mut lost_note = None;
        if let Some(writer) = state.writer.as_mut() {
            match writer.append(&record, urgent) {
                Ok(()) if state.unreported_lost > 0 => {
                    lost_note = Some(std::mem::take(&mut state.unreported_lost));
                }
                Ok(()) => {}
                Err(failure) => self.note_lost(state, &failure),
            }
        }
        self.keep(state, record);
        if let Some(count) = lost_note {
            let draft = Event::new(format!(
                "{count} system journal record(s) could not be written to the store"
            ))
            .count("count", count)
            .into_draft(&catalogue::RECORDS_LOST, state.last_time);
            let lost = self.record_of(state, draft);
            if let Some(writer) = state.writer.as_mut()
                && let Err(failure) = writer.append(&lost, false)
            {
                // The count is reported again by the next successful write.
                state.unreported_lost += count;
                self.note_lost(state, &failure);
            }
            self.keep(state, lost);
        }
        seq
    }

    fn record_of(&self, state: &mut State, draft: Draft) -> JournalRecord {
        let seq = state.next_seq;
        state.next_seq += 1;
        let time = draft.time.max(state.last_time);
        state.last_time = time;
        JournalRecord {
            seq,
            origin: self.instance.clone(),
            origin_seq: seq,
            time,
            level: draft.level,
            source: draft.kind.source.to_string(),
            kind: draft.kind.code.to_string(),
            message: draft.message,
            op: draft.op,
            parent: draft.parent,
            commit: draft.commit,
            principal: draft.principal,
            fields: draft.fields,
        }
    }

    fn keep(&self, state: &mut State, record: JournalRecord) {
        state.deliver.push_back(record.clone());
        state.ring.push_back(record);
        while state.ring.len() > self.capacity {
            state.ring.pop_front();
        }
        state.floor = state
            .ring
            .front()
            .map_or(state.next_seq, |record| record.seq);
    }

    /// Hands the queued records to the `tracing` mirror and the sinks, in `seq` order.
    fn deliver(&self) {
        let _delivery = self.delivery.lock();
        loop {
            let batch: Vec<JournalRecord> = self.state.lock().deliver.drain(..).collect();
            if batch.is_empty() {
                return;
            }
            let sinks: Vec<Arc<dyn JournalSink>> = self
                .sinks
                .lock()
                .iter()
                .map(|(_, sink)| Arc::clone(sink))
                .collect();
            for record in &batch {
                mirror(record);
                for sink in &sinks {
                    let delivered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        sink.record(record)
                    }));
                    if delivered.is_err() {
                        self.sink_failures.fetch_add(1, Ordering::AcqRel);
                        tracing::warn!(
                            sink = sink.name(),
                            "a system journal sink panicked; the record was not delivered to it"
                        );
                    }
                }
            }
        }
    }
}

impl Drop for SystemJournal {
    fn drop(&mut self) {
        // Best effort: the open groups are closed and the batched records synced.
        let time = self.clock.floor();
        let state = self.state.get_mut();
        let mut collapser = std::mem::take(&mut state.collapse);
        let mut pending = Vec::new();
        collapser.close_all(time, &mut |draft| {
            pending.push(draft);
            0
        });
        for draft in pending {
            let seq = state.next_seq;
            state.next_seq += 1;
            let record = JournalRecord {
                seq,
                origin: self.instance.clone(),
                origin_seq: seq,
                time: draft.time.max(state.last_time),
                level: draft.level,
                source: draft.kind.source.to_string(),
                kind: draft.kind.code.to_string(),
                message: draft.message,
                op: None,
                parent: None,
                commit: None,
                principal: None,
                fields: draft.fields,
            };
            mirror(&record);
            if let Some(writer) = state.writer.as_mut() {
                let _ = writer.append(&record, false);
            }
        }
        if let Some(writer) = state.writer.as_mut()
            && let Err(failure) = writer.sync()
        {
            tracing::warn!(
                reason = %failure.reason,
                "the system journal was not written when it was dropped"
            );
        }
    }
}

/// The `tracing` level a journal level is mirrored at.
pub(crate) fn tracing_level(level: Level) -> tracing::Level {
    match level {
        Level::Error => tracing::Level::ERROR,
        Level::Warn => tracing::Level::WARN,
        Level::Notice | Level::Info => tracing::Level::INFO,
    }
}

/// Mirrors a written record to `tracing` (target `graphersal::journal`) at the matching level
/// (`notice` and `info` as `info`).
pub(crate) fn mirror(record: &JournalRecord) {
    let fields = render_fields(&record.fields);
    macro_rules! mirror_at {
        ($macro:ident) => {
            tracing::$macro!(
                target: "graphersal::journal",
                seq = record.seq,
                kind = %record.kind,
                source = %record.source,
                fields = %fields,
                "{}",
                record.message
            )
        };
    }
    let level = tracing_level(record.level);
    #[cfg(test)]
    MIRRORED.with(|mirrored| mirrored.borrow_mut().push((record.seq, level)));
    if level == tracing::Level::ERROR {
        mirror_at!(error);
    } else if level == tracing::Level::WARN {
        mirror_at!(warn);
    } else {
        mirror_at!(info);
    }
}

#[cfg(test)]
thread_local! {
    /// What [`mirror`] handed to `tracing` on this thread, `(seq, level)` in order: the unit
    /// tests check the mirror here, independent of `tracing`'s process-wide callsite cache.
    static MIRRORED: std::cell::RefCell<Vec<(u64, tracing::Level)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
mod tests;
