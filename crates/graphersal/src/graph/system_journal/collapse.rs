//! Collapsing repeats and states before a record is written (book page "System Journal",
//! "Repeats and states"): the journal is append-only, so a repeat is counted in memory and
//! closed by one `journal.repeated` record, never by rewriting a record.

use std::collections::VecDeque;

use super::catalogue::{self, Collapse, EventKind, Form};
use super::{Draft, Level};
use crate::value::ElementProperty;

/// How many written records a new one is compared with.
pub(crate) const RECENT: usize = 5;

/// After this long (µs) a group of repeats is closed and a continuing problem is written again;
/// a state gets its reminder: one hour.
pub(crate) const COLLAPSE_WINDOW_US: i64 = 3_600_000_000;

/// A group of repeats of one written record.
#[derive(Clone, Copy)]
struct Group {
    count: u64,
    first: i64,
    last: i64,
}

/// One of the last written records.
struct Recent {
    /// The collapse key (`None`: never collapsed).
    key: Option<String>,
    origin_seq: u64,
    code: &'static str,
    level: Level,
    time: i64,
    group: Option<Group>,
}

/// A state that holds.
struct ActiveState {
    key: String,
    kind: &'static EventKind,
    level: Level,
    since: i64,
    last_reminder: i64,
    /// Repeats since the last record about it.
    count: u64,
    fields: Vec<(String, ElementProperty)>,
}

/// The collapsing state of one journal (under its lock).
#[derive(Default)]
pub(crate) struct Collapser {
    recent: VecDeque<Recent>,
    states: Vec<ActiveState>,
}

/// The collapse key: kind, source and the named fields' values (never time or seq).
fn key_of(draft: &Draft, fields: &[&str]) -> String {
    let mut key = format!("{}\u{0}{}", draft.kind.code, draft.kind.source);
    for name in fields {
        key.push('\u{0}');
        key.push_str(name);
        key.push('=');
        if let Some((_, value)) = draft.fields.iter().find(|(field, _)| field == name) {
            key.push_str(&crate::value::physical::to_json_text(value).unwrap_or_default());
        }
    }
    key
}

impl Collapser {
    /// Decides about `draft`: written through `write` (which returns the record's seq), or
    /// counted. May write `journal.repeated` / `journal.state_reminder` records too.
    pub(crate) fn admit(&mut self, draft: Draft, write: &mut dyn FnMut(Draft) -> u64) {
        self.close_expired(draft.time, write);
        match &draft.kind.form {
            Form::Event(Collapse::Never) => self.push_written(draft, None, write),
            Form::Event(Collapse::By(fields)) => {
                let key = key_of(&draft, fields);
                if let Some(recent) = self
                    .recent
                    .iter_mut()
                    .find(|recent| recent.key.as_deref() == Some(key.as_str()))
                {
                    let group = recent.group.get_or_insert(Group {
                        count: 0,
                        first: draft.time,
                        last: draft.time,
                    });
                    group.count += 1;
                    group.last = draft.time;
                    return;
                }
                self.push_written(draft, Some(key), write);
            }
            Form::State { key: fields, .. } => {
                let key = key_of(&draft, fields);
                if let Some(state) = self.states.iter_mut().find(|state| state.key == key) {
                    state.count += 1;
                    state.fields = draft.fields.clone();
                    if draft.time - state.last_reminder >= COLLAPSE_WINDOW_US {
                        state.last_reminder = draft.time;
                        let mut fields = vec![
                            ("state".to_string(), state.kind.code.into()),
                            ("since_us".to_string(), state.since.into()),
                            ("count".to_string(), int(state.count)),
                        ];
                        fields.extend(state.fields.iter().cloned());
                        state.count = 0;
                        let reminder = Draft {
                            kind: &catalogue::STATE_REMINDER,
                            level: state.level,
                            message: format!("{} still holds", state.kind.code),
                            op: None,
                            parent: None,
                            commit: None,
                            principal: None,
                            fields,
                            time: draft.time,
                        };
                        self.push_written(reminder, None, write);
                    }
                    return;
                }
                self.states.push(ActiveState {
                    key,
                    kind: draft.kind,
                    level: draft.level,
                    since: draft.time,
                    last_reminder: draft.time,
                    count: 0,
                    fields: draft.fields.clone(),
                });
                self.push_written(draft, None, write);
            }
            Form::Cleared { state } => {
                if let Some(state_kind) = catalogue::kind(state)
                    && let Form::State { key: fields, .. } = &state_kind.form
                {
                    let mut probe = draft.clone();
                    probe.kind = state_kind;
                    let key = key_of(&probe, fields);
                    self.states.retain(|active| active.key != key);
                }
                self.push_written(draft, None, write);
            }
        }
    }

    /// Writes `draft` and keeps it among the last records; a record leaving them closes its
    /// group.
    fn push_written(
        &mut self,
        draft: Draft,
        key: Option<String>,
        write: &mut dyn FnMut(Draft) -> u64,
    ) {
        let code = draft.kind.code;
        let level = draft.level;
        let time = draft.time;
        let origin_seq = write(draft);
        self.recent.push_back(Recent {
            key,
            origin_seq,
            code,
            level,
            time,
            group: None,
        });
        while self.recent.len() > RECENT {
            let Some(left) = self.recent.pop_front() else {
                break;
            };
            if let Some(group) = left.group.filter(|group| group.count > 0) {
                let repeated = repeated(&left, group, time);
                // A `repeated` record is never collapsed; it may push another one out.
                let repeated_time = repeated.time;
                let repeated_level = repeated.level;
                let seq = write(repeated);
                self.recent.push_back(Recent {
                    key: None,
                    origin_seq: seq,
                    code: catalogue::REPEATED.code,
                    level: repeated_level,
                    time: repeated_time,
                    group: None,
                });
            }
        }
    }

    /// Closes the groups whose original is older than the window at `now`.
    fn close_expired(&mut self, now: i64, write: &mut dyn FnMut(Draft) -> u64) {
        let mut closing = Vec::new();
        for recent in &mut self.recent {
            if now - recent.time >= COLLAPSE_WINDOW_US {
                // A continuing problem is written again after the window.
                recent.key = None;
                if let Some(group) = recent.group.take().filter(|group| group.count > 0) {
                    closing.push(repeated(recent, group, now));
                }
            }
        }
        for draft in closing {
            self.push_written(draft, None, write);
        }
    }

    /// Closes every open group (close, shutdown, an explicit flush).
    pub(crate) fn close_all(&mut self, now: i64, write: &mut dyn FnMut(Draft) -> u64) {
        let closing: Vec<Draft> = self
            .recent
            .iter_mut()
            .filter_map(|recent| {
                let group = recent.group.take().filter(|group| group.count > 0)?;
                Some(repeated(recent, group, now))
            })
            .collect();
        for draft in closing {
            self.push_written(draft, None, write);
        }
    }
}

/// A u64 as a stored integer (saturating).
pub(crate) fn int(value: u64) -> ElementProperty {
    ElementProperty::from(i64::try_from(value).unwrap_or(i64::MAX))
}

/// The `journal.repeated` record closing `group` of `original`.
fn repeated(original: &Recent, group: Group, now: i64) -> Draft {
    Draft {
        kind: &catalogue::REPEATED,
        level: original.level,
        message: format!(
            "{} occurred {} more time(s) after record {}",
            original.code, group.count, original.origin_seq
        ),
        op: None,
        parent: None,
        commit: None,
        principal: None,
        fields: vec![
            ("of_origin_seq".into(), int(original.origin_seq)),
            ("of_kind".into(), original.code.into()),
            ("count".into(), int(group.count)),
            ("first_us".into(), group.first.into()),
            ("last_us".into(), group.last.into()),
        ],
        time: now,
    }
}
