//! Event kinds a HOST declares (issue #35, book page "System Journal", section "Host events"):
//! a server or a daemon embedding the engine records its own operational events beside the
//! engine's, in the same journal, through [`SystemJournal::record`].
//!
//! A declaration is process-wide ([`declare`]) and validated like the built-in catalogue: the
//! code and the source live under the host's own first segment (never one of the engine's), the
//! fields are named once, the collapse key names declared fields. Declaring the same kind again
//! with the same declaration is a no-op; a different declaration of a declared code is an
//! error. Recording an undeclared kind, an undeclared field or a record without a required
//! field is an error, never a silent write. Records keep the engine's layout: no format change.

use std::sync::Arc;

use super::catalogue::{self, Collapse, EventKind, FieldDecl, Form};
use super::{Draft, JournalError, Level, SystemJournal};
use crate::value::ElementProperty;

/// The longest code or source of a host kind, in bytes.
const MAX_NAME_BYTES: usize = 128;

/// The most fields a host kind declares.
const MAX_FIELDS: usize = 32;

/// The first segment no host code or source may start with besides the engine's own segments
/// (those of every built-in kind's code and source).
const RESERVED_SEGMENTS: &[&str] = &["graphersal"];

/// One field of a [`HostEventKind`].
#[derive(Debug, Clone, PartialEq, Eq)]
struct HostField {
    name: String,
    description: String,
    optional: bool,
}

/// An event kind a host declares ([`declare`]): its stable code, its source, its level, one
/// sentence describing it and its fields, like a built-in kind of the catalogue.
///
/// ```rust
/// use graphersal::system_journal::{HostEventKind, Level};
///
/// let kind = HostEventKind::new(
///     "myhost.job_failed",
///     "myhost.jobs",
///     Level::Error,
///     "A background job of the host failed.",
/// )
/// .field("job", "the job's name")
/// .optional_field("cause", "the error")
/// .collapse_by(["job"]);
/// assert_eq!(kind.code(), "myhost.job_failed");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostEventKind {
    code: String,
    source: String,
    level: Level,
    description: String,
    fields: Vec<HostField>,
    /// `None`: every occurrence is written; `Some(fields)`: repeats with equal values of these
    /// fields among the last five records are counted (empty: by the kind alone).
    collapse: Option<Vec<String>>,
}

impl HostEventKind {
    /// A kind with `code` (`myhost.job_failed`: two or more dotted segments of lowercase ASCII
    /// letters, digits and `_`, each starting with a letter), `source` (`myhost.jobs`: one or
    /// more such segments), its `level` and a one-sentence `description`. The first segment of
    /// the code and of the source must not be one of the engine's (`store`, `journal`, `auth`,
    /// ...) nor `graphersal`: pick one naming the host.
    pub fn new(
        code: impl Into<String>,
        source: impl Into<String>,
        level: Level,
        description: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            source: source.into(),
            level,
            description: description.into(),
            fields: Vec::new(),
            collapse: None,
        }
    }

    /// A field every record of the kind holds.
    pub fn field(mut self, name: impl Into<String>, description: impl Into<String>) -> Self {
        self.fields.push(HostField {
            name: name.into(),
            description: description.into(),
            optional: false,
        });
        self
    }

    /// A field a record may leave out.
    pub fn optional_field(
        mut self,
        name: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        self.fields.push(HostField {
            name: name.into(),
            description: description.into(),
            optional: true,
        });
        self
    }

    /// Collapses repeats: a record whose kind and these fields' values equal one of the last
    /// five written records is counted instead of written, and a `journal.repeated` record
    /// closes the group (book "Repeats and states"). No fields: by the kind alone. Without this
    /// call every occurrence is written.
    pub fn collapse_by<I, S>(mut self, fields: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.collapse = Some(fields.into_iter().map(Into::into).collect());
        self
    }

    /// The stable code.
    pub fn code(&self) -> &str {
        &self.code
    }

    /// The source.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The level.
    pub fn level(&self) -> Level {
        self.level
    }

    /// The description.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// The declared field names with whether each is optional, in declaration order.
    pub fn fields(&self) -> impl Iterator<Item = (&str, bool)> {
        self.fields
            .iter()
            .map(|field| (field.name.as_str(), field.optional))
    }

    /// The collapse fields (`None`: never collapsed; empty: by the kind alone).
    pub fn collapse_fields(&self) -> Option<&[String]> {
        self.collapse.as_deref()
    }

    fn invalid(&self, reason: impl Into<String>) -> JournalError {
        JournalError::InvalidDeclaration {
            kind: self.code.clone(),
            reason: reason.into(),
        }
    }

    /// Checks the declaration on its own (names, fields, collapse key, reserved segments).
    fn validate(&self) -> Result<(), JournalError> {
        check_dotted(&self.code, 2).map_err(|reason| self.invalid(format!("code: {reason}")))?;
        check_dotted(&self.source, 1)
            .map_err(|reason| self.invalid(format!("source: {reason}")))?;
        for (what, name) in [("code", &self.code), ("source", &self.source)] {
            let first = first_segment(name);
            if is_reserved(first) {
                return Err(self.invalid(format!(
                    "the {what} starts with `{first}`, a segment of the engine's own events: \
                     use a first segment naming the host"
                )));
            }
        }
        if self.description.trim().is_empty() {
            return Err(self.invalid("the description is empty"));
        }
        if self.fields.len() > MAX_FIELDS {
            return Err(self.invalid(format!(
                "{} fields, at most {MAX_FIELDS}",
                self.fields.len()
            )));
        }
        for (index, field) in self.fields.iter().enumerate() {
            if !is_identifier(&field.name) {
                return Err(self.invalid(format!(
                    "field `{}`: a name is lowercase ASCII letters, digits and `_`, starting \
                     with a letter",
                    field.name
                )));
            }
            if self.fields[..index]
                .iter()
                .any(|other| other.name == field.name)
            {
                return Err(self.invalid(format!("field `{}` is declared twice", field.name)));
            }
        }
        if let Some(collapse) = &self.collapse {
            for (index, name) in collapse.iter().enumerate() {
                if !self.fields.iter().any(|field| &field.name == name) {
                    return Err(self.invalid(format!(
                        "the collapse key names `{name}`, which is not a declared field"
                    )));
                }
                if collapse[..index].contains(name) {
                    return Err(self.invalid(format!("the collapse key names `{name}` twice")));
                }
            }
        }
        Ok(())
    }

    /// The declaration as the catalogue's `'static` form (leaked once per declared code: the
    /// registry keeps it for the life of the process).
    fn leak(&self) -> &'static EventKind {
        fn text(value: &str) -> &'static str {
            Box::leak(value.to_owned().into_boxed_str())
        }
        let fields: Vec<FieldDecl> = self
            .fields
            .iter()
            .map(|field| FieldDecl {
                name: text(&field.name),
                description: text(&field.description),
                optional: field.optional,
            })
            .collect();
        let collapse = match &self.collapse {
            None => Collapse::Never,
            Some(names) => {
                let names: Vec<&'static str> = names.iter().map(|name| text(name)).collect();
                Collapse::By(Box::leak(names.into_boxed_slice()))
            }
        };
        Box::leak(Box::new(EventKind {
            code: text(&self.code),
            source: text(&self.source),
            level: self.level,
            description: text(&self.description),
            fields: Box::leak(fields.into_boxed_slice()),
            form: Form::Event(collapse),
            emitted_later_by: None,
        }))
    }
}

/// The host kinds declared in this process, in declaration order.
static DECLARED: parking_lot::Mutex<Vec<(&'static EventKind, Arc<HostEventKind>)>> =
    parking_lot::Mutex::new(Vec::new());

/// Declares a host event kind for this process (every journal of every graph and store in it
/// records it from now on). Declaring an identical kind again is a no-op, so a host may declare
/// its kinds at every start of a component.
///
/// # Errors
///
/// [`JournalError::InvalidDeclaration`] when the declaration is malformed (see
/// [`HostEventKind::new`]), names a code of the engine's catalogue, or differs from an earlier
/// declaration of the same code.
pub fn declare(kind: HostEventKind) -> Result<(), JournalError> {
    kind.validate()?;
    if catalogue::kind(&kind.code).is_some() {
        return Err(kind.invalid("the engine declares this code"));
    }
    let mut declared = DECLARED.lock();
    if let Some((_, earlier)) = declared
        .iter()
        .find(|(static_kind, _)| static_kind.code == kind.code)
    {
        return if **earlier == kind {
            Ok(())
        } else {
            Err(kind.invalid("this code is already declared differently in this process"))
        };
    }
    let static_kind = kind.leak();
    declared.push((static_kind, Arc::new(kind)));
    Ok(())
}

/// The host kinds declared in this process, in declaration order (for a host's own catalogue
/// or a reader that renders their fields).
pub fn declared_kinds() -> Vec<HostEventKind> {
    DECLARED
        .lock()
        .iter()
        .map(|(_, kind)| HostEventKind::clone(kind))
        .collect()
}

/// The declared host kind with `code`: its catalogue form and its declaration.
fn lookup(code: &str) -> Option<(&'static EventKind, Arc<HostEventKind>)> {
    DECLARED
        .lock()
        .iter()
        .find(|(kind, _)| kind.code == code)
        .map(|(kind, declaration)| (*kind, Arc::clone(declaration)))
}

/// Whether `kind` is a declared host kind (the debug check of `Event::into_draft`).
pub(crate) fn is_declared(kind: &'static EventKind) -> bool {
    DECLARED
        .lock()
        .iter()
        .any(|(declared, _)| std::ptr::eq(*declared, kind))
}

/// One event a host records ([`SystemJournal::record`]): a declared kind, the rendered English
/// message, the kind's fields and the envelope's optional parts.
///
/// Like every record it holds names, counts, durations and ids: never data values, query text
/// or secrets.
#[derive(Debug, Clone, PartialEq)]
pub struct HostEvent {
    kind: String,
    message: String,
    level: Option<Level>,
    op: Option<u64>,
    parent: Option<u64>,
    commit: Option<u64>,
    principal: Option<String>,
    fields: Vec<(String, ElementProperty)>,
}

impl HostEvent {
    /// An event of the declared kind `kind` with its rendered `message`.
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
            level: None,
            op: None,
            parent: None,
            commit: None,
            principal: None,
            fields: Vec::new(),
        }
    }

    /// A declared field's value.
    pub fn field(mut self, name: impl Into<String>, value: impl Into<ElementProperty>) -> Self {
        self.fields.push((name.into(), value.into()));
        self
    }

    /// Overrides the kind's declared level for this record.
    pub fn level(mut self, level: Level) -> Self {
        self.level = Some(level);
        self
    }

    /// The operation the record belongs to: the `seq` of the record that started it (the
    /// value [`SystemJournal::record`] returned for it).
    pub fn op(mut self, op: u64) -> Self {
        self.op = Some(op);
        self
    }

    /// The parent operation.
    pub fn parent(mut self, parent: u64) -> Self {
        self.parent = Some(parent);
        self
    }

    /// The commit in the WAL the event relates to.
    pub fn commit(mut self, commit: u64) -> Self {
        self.commit = Some(commit);
        self
    }

    /// Who: the channel or subject the work came through (default: the thread's workload
    /// channel, `host` unless a front end set one).
    pub fn principal(mut self, principal: impl Into<String>) -> Self {
        self.principal = Some(principal.into());
        self
    }

    /// The kind's code.
    pub fn kind(&self) -> &str {
        &self.kind
    }

    fn invalid(&self, reason: impl Into<String>) -> JournalError {
        JournalError::InvalidEvent {
            kind: self.kind.clone(),
            reason: reason.into(),
        }
    }

    /// The checked draft: a declared kind, declared fields once each, every required field,
    /// the fields in declaration order.
    fn into_draft(self, time: i64) -> Result<Draft, JournalError> {
        let Some((kind, declaration)) = lookup(&self.kind) else {
            return Err(if catalogue::kind(&self.kind).is_some() {
                self.invalid("the engine's own kinds are written by the engine only")
            } else {
                JournalError::Undeclared {
                    kind: self.kind.clone(),
                }
            });
        };
        for (index, (name, _)) in self.fields.iter().enumerate() {
            if !declaration.fields.iter().any(|field| &field.name == name) {
                return Err(self.invalid(format!("`{name}` is not a declared field")));
            }
            if self.fields[..index].iter().any(|(other, _)| other == name) {
                return Err(self.invalid(format!("field `{name}` is given twice")));
            }
        }
        if let Some(missing) = declaration.fields.iter().find(|field| {
            !field.optional && !self.fields.iter().any(|(name, _)| *name == field.name)
        }) {
            return Err(self.invalid(format!("the required field `{}` is missing", missing.name)));
        }
        let mut fields = self.fields;
        fields.sort_by_key(|(name, _)| {
            declaration
                .fields
                .iter()
                .position(|field| &field.name == name)
        });
        Ok(Draft {
            kind,
            level: self.level.unwrap_or(kind.level),
            message: self.message,
            op: self.op,
            parent: self.parent,
            commit: self.commit,
            principal: self
                .principal
                .or_else(|| Some(crate::graph::workload::current_channel().name().to_string())),
            fields,
            time,
        })
    }
}

impl SystemJournal {
    /// Records a host's event of a declared kind ([`declare`]), timed by the instance clock and
    /// collapsed per its declaration, like the engine's own records: kept in memory, written to
    /// the store's journal (an `error` record at once, others batched), mirrored to `tracing`
    /// and handed to the sinks. Returns the record's `seq` (the `op` of later records of the
    /// same operation), or `None` when it was counted as a repeat. A failed write of the stored
    /// journal is not an error here: like every journal write it is a lost record
    /// ([`SystemJournal::lost_records`]).
    ///
    /// # Errors
    ///
    /// [`JournalError::Undeclared`] for a kind nobody declared; [`JournalError::InvalidEvent`]
    /// for an engine kind, an undeclared or repeated field, or a missing required field.
    /// Nothing is recorded then.
    pub fn record(&self, event: HostEvent) -> Result<Option<u64>, JournalError> {
        let time = self.clock.now();
        let draft = event.into_draft(time)?;
        Ok(self.admit(draft))
    }
}

/// Whether `segment` is the first segment of a built-in kind's code or source, or reserved.
fn is_reserved(segment: &str) -> bool {
    RESERVED_SEGMENTS.contains(&segment)
        || catalogue::EVENT_KINDS.iter().any(|kind| {
            first_segment(kind.code) == segment || first_segment(kind.source) == segment
        })
}

fn first_segment(name: &str) -> &str {
    name.split('.').next().unwrap_or(name)
}

/// A name of lowercase ASCII letters, digits and `_`, starting with a letter.
fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('a'..='z'))
        && chars.all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_'))
}

/// At least `min` dotted identifier segments, at most [`MAX_NAME_BYTES`].
fn check_dotted(name: &str, min: usize) -> Result<(), String> {
    if name.len() > MAX_NAME_BYTES {
        return Err(format!("`{name}` is longer than {MAX_NAME_BYTES} bytes"));
    }
    let segments: Vec<&str> = name.split('.').collect();
    if segments.len() < min {
        return Err(format!(
            "`{name}` needs at least {min} dotted segments (`prefix.name`)"
        ));
    }
    if let Some(bad) = segments.iter().find(|segment| !is_identifier(segment)) {
        return Err(format!(
            "segment `{bad}` of `{name}`: lowercase ASCII letters, digits and `_`, starting \
             with a letter"
        ));
    }
    Ok(())
}
