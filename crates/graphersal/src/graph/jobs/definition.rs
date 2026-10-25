//! A user's scheduled job as a catalog definition ([`JobDefinition`], kind
//! [`DefinitionKind::JOB`]): when it runs (a cron schedule), what it does ([`JobTask`]: call a
//! saved query, write a checkpoint, take a backup), its limits, and the effects its definer was
//! allowed when it was defined (book page "Scheduled Jobs").
//!
//! The rights model (user decision): the definer's authorizer checks every effect of the job
//! when it is defined (the front end does that, like it infers a saved query's dialect); the
//! checked effects are stored with the definition, and every run executes under an authorizer
//! limited to exactly them. A newer version's action, `run_on` or effect form is kept as read,
//! written back unchanged and never run ([`JobDefinition::unserved_reason`]).

use std::path::Path;
use std::time::Duration;

use crate::error::DefinitionError;
use crate::graph::ElementKind;
use crate::graph::authorization::{AccessRequest, AccessRequestBuf, Action, Resource};
use crate::graph::definition::{
    Definition, DefinitionBody, DefinitionFlags, DefinitionKind, MAX_DESCRIPTION_BYTES,
    RESERVED_PAYLOAD_KEYS,
};
use crate::value::{ElementProperty, PropertyMap};

use super::{CronSchedule, JobLimits};

/// The longest schedule text, in bytes.
pub const MAX_SCHEDULE_BYTES: usize = 256;
/// The most effects a job definition stores.
pub const MAX_JOB_EFFECTS: usize = 256;
/// The longest backup path, in bytes.
pub const MAX_JOB_PATH_BYTES: usize = 4096;

/// What a job does when it runs.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub enum JobTask {
    /// Calls the saved query `name` with `args` (one unit, under the job's effects).
    Query {
        /// The saved query.
        name: String,
        /// Its arguments by parameter name.
        args: PropertyMap,
    },
    /// Writes a checkpoint of the store (a job on a graph without a store is not run).
    Checkpoint,
    /// Takes a backup of the store into the directory `path` (the first run a full backup, the
    /// next ones increments).
    Backup {
        /// The backup directory.
        path: String,
    },
    /// An action this version does not know, by name: kept, never run.
    Unknown(String),
}

impl JobTask {
    /// The action's name: `query`, `checkpoint`, `backup`, or the unknown name as read.
    pub fn name(&self) -> &str {
        match self {
            JobTask::Query { .. } => "query",
            JobTask::Checkpoint => "checkpoint",
            JobTask::Backup { .. } => "backup",
            JobTask::Unknown(name) => name,
        }
    }
}

/// Where a job runs in a replicated setup (planned): `any` node, only the `primary`, only a
/// `replica`. A single instance is the primary.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOn {
    /// Every node (the default).
    Any,
    /// Only the primary.
    Primary,
    /// Only a replica.
    Replica,
    /// A value this version does not know, as read: the job is kept, never run.
    Unknown(String),
}

impl RunOn {
    /// `any`, `primary`, `replica`, or the unknown value as read.
    pub fn as_str(&self) -> &str {
        match self {
            RunOn::Any => "any",
            RunOn::Primary => "primary",
            RunOn::Replica => "replica",
            RunOn::Unknown(name) => name,
        }
    }

    fn from_name(name: &str) -> RunOn {
        match name {
            "any" => RunOn::Any,
            "primary" => RunOn::Primary,
            "replica" => RunOn::Replica,
            other => RunOn::Unknown(other.to_string()),
        }
    }
}

/// A scheduled job of a user (catalog kind [`DefinitionKind::JOB`]): runs [`action`](Self::action)
/// on [`schedule`](Self::schedule) (5-field cron, the instance's local time zone) while
/// [`enabled`](Self::enabled), within [`limits`](Self::limits), under the stored
/// [`effects`](Self::effects).
///
/// ```
/// use graphersal::catalog::{Definition, JobTask, JobDefinition};
///
/// let job = JobDefinition::new("0 3 * * *", JobTask::Checkpoint)
///     .with_description("nightly checkpoint");
/// let definition = Definition::job("nightly", job);
/// assert!(definition.validate().is_ok());
/// assert_eq!(definition.as_job().unwrap().action().name(), "checkpoint");
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct JobDefinition {
    schedule: String,
    action: JobTask,
    enabled: bool,
    limits: JobLimits,
    run_on: RunOn,
    effects: Vec<AccessRequestBuf>,
    description: String,
    extra: PropertyMap,
    /// The payload exactly as read, kept when this version cannot run the job (an unknown
    /// action, `run_on` or effect form): written back unchanged.
    original: Option<PropertyMap>,
    /// Why this version does not run it (set together with `original`).
    unserved: Option<String>,
}

impl JobDefinition {
    /// A job running `action` on `schedule`, enabled, without limits, effects or description.
    pub fn new(schedule: impl Into<String>, action: JobTask) -> Self {
        JobDefinition {
            schedule: schedule.into(),
            action,
            enabled: true,
            limits: JobLimits::default(),
            run_on: RunOn::Any,
            effects: Vec::new(),
            description: String::new(),
            extra: PropertyMap::default(),
            original: None,
            unserved: None,
        }
    }

    /// The same job switched on or off.
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// The same job with `limits` per run.
    pub fn with_limits(mut self, limits: JobLimits) -> Self {
        self.limits = limits;
        self
    }

    /// The same job with `run_on`.
    pub fn with_run_on(mut self, run_on: RunOn) -> Self {
        self.run_on = run_on;
        self
    }

    /// The same job with the effects its definer was allowed (checked by the front end when
    /// the job is defined).
    pub fn with_effects(mut self, effects: Vec<AccessRequestBuf>) -> Self {
        self.effects = effects;
        self
    }

    /// The same job with a description.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// The schedule text (5-field cron).
    pub fn schedule(&self) -> &str {
        &self.schedule
    }

    /// What the job does.
    pub fn action(&self) -> &JobTask {
        &self.action
    }

    /// Whether it runs on its schedule.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Its limits per run.
    pub fn limits(&self) -> JobLimits {
        self.limits
    }

    /// Where it runs in a replicated setup.
    pub fn run_on(&self) -> &RunOn {
        &self.run_on
    }

    /// The effects its definer was allowed; a run may do exactly these (and read).
    pub fn effects(&self) -> &[AccessRequestBuf] {
        &self.effects
    }

    /// Its description (empty when none).
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Payload keys this version does not know, kept and written back.
    pub fn extra(&self) -> &PropertyMap {
        &self.extra
    }

    /// Why this version does not run the job (an action, `run_on` value or effect form a newer
    /// version wrote), `None` when it does.
    pub fn unserved_reason(&self) -> Option<String> {
        self.unserved.clone()
    }

    /// The payload property map (format spec 6.3), in this key order: `schedule`, `action`,
    /// `query` and `args` (action `query`), `path` (action `backup`), `enabled`, `timeout_ms`
    /// and `memory_limit` (when set), `run_on` (unless `any`), `description` (unless empty),
    /// `effects` (a list of `{action, resource, ..}` maps), then the unknown keys. A job this
    /// version does not run gives its payload back unchanged.
    pub fn to_payload(&self) -> PropertyMap {
        if let Some(original) = &self.original {
            return original.clone();
        }
        let mut map = PropertyMap::default();
        map.insert("schedule", self.schedule.as_str().into());
        map.insert("action", self.action.name().into());
        match &self.action {
            JobTask::Query { name, args } => {
                map.insert("query", name.as_str().into());
                map.insert("args", ElementProperty::Object(Box::new(args.clone())));
            }
            JobTask::Backup { path } => {
                map.insert("path", path.as_str().into());
            }
            JobTask::Checkpoint | JobTask::Unknown(_) => {}
        }
        map.insert("enabled", ElementProperty::from(self.enabled));
        if let Some(timeout) = self.limits.timeout {
            map.insert("timeout_ms", int(timeout.as_millis() as u64));
        }
        if let Some(bytes) = self.limits.memory_limit {
            map.insert("memory_limit", int(bytes));
        }
        if self.run_on != RunOn::Any {
            map.insert("run_on", self.run_on.as_str().into());
        }
        if !self.description.is_empty() {
            map.insert("description", self.description.as_str().into());
        }
        map.insert(
            "effects",
            ElementProperty::Array(self.effects.iter().map(effect_value).collect()),
        );
        for (key, value) in &self.extra {
            map.insert_if_absent(key, || value.clone());
        }
        map
    }

    /// Reads a payload map (the inverse of [`Self::to_payload`]). `schedule` and `action` are
    /// required; a missing `enabled` is true, missing `effects` none; a known key of the wrong
    /// type is an error; unknown keys are kept. An unknown action, `run_on` value or effect
    /// form is never an error: the job is kept with its payload and not run. Not validated.
    pub fn from_payload(payload: &PropertyMap) -> Result<JobDefinition, DefinitionError> {
        Self::read_fields(payload, &[])
    }

    pub(crate) fn read_fields(
        map: &PropertyMap,
        skip: &[&str],
    ) -> Result<JobDefinition, DefinitionError> {
        let malformed = |key: &str, reason: &str| DefinitionError::Malformed {
            reason: format!("/{key}: {reason}"),
        };
        let text = |key: &str| -> Result<Option<String>, DefinitionError> {
            match map.get(key) {
                None => Ok(None),
                Some(value) => value
                    .text()
                    .map(|text| Some(text.into_owned()))
                    .ok_or_else(|| malformed(key, "must be a string")),
            }
        };
        let number = |key: &str| -> Result<Option<u64>, DefinitionError> {
            match map.get(key) {
                None | Some(ElementProperty::Null) => Ok(None),
                Some(value) => value
                    .as_i64()
                    .and_then(|n| u64::try_from(n).ok())
                    .map(Some)
                    .ok_or_else(|| malformed(key, "must be a non-negative integer")),
            }
        };
        let schedule = text("schedule")?.ok_or_else(|| DefinitionError::Malformed {
            reason: "'schedule' is missing".to_string(),
        })?;
        let action_name = text("action")?.ok_or_else(|| DefinitionError::Malformed {
            reason: "'action' is missing".to_string(),
        })?;
        let mut unserved = None;
        let action = match action_name.as_str() {
            "query" => {
                let name = text("query")?.ok_or_else(|| DefinitionError::Malformed {
                    reason: "'query' is missing (the saved query a query job calls)".to_string(),
                })?;
                let args = match map.get("args") {
                    None | Some(ElementProperty::Null) => PropertyMap::default(),
                    Some(ElementProperty::Object(args)) => {
                        let mut logical = PropertyMap::default();
                        for (key, value) in args.iter() {
                            logical.insert(key.clone(), value.to_logical());
                        }
                        logical
                    }
                    Some(_) => return Err(malformed("args", "must be an object")),
                };
                JobTask::Query { name, args }
            }
            "checkpoint" => JobTask::Checkpoint,
            "backup" => {
                let path = text("path")?.ok_or_else(|| DefinitionError::Malformed {
                    reason: "'path' is missing (the directory a backup job writes)".to_string(),
                })?;
                JobTask::Backup { path }
            }
            other => {
                unserved = Some(format!(
                    "the action '{other}' is not known to this version"
                ));
                JobTask::Unknown(other.to_string())
            }
        };
        let enabled = match map.get("enabled") {
            None => true,
            Some(ElementProperty::Boolean(enabled)) => *enabled,
            Some(_) => return Err(malformed("enabled", "must be true or false")),
        };
        let mut limits = JobLimits::default();
        if let Some(ms) = number("timeout_ms")? {
            limits = limits.with_timeout(Duration::from_millis(ms));
        }
        if let Some(bytes) = number("memory_limit")? {
            limits = limits.with_memory_limit(bytes);
        }
        let run_on = match text("run_on")? {
            None => RunOn::Any,
            Some(name) => RunOn::from_name(&name),
        };
        if let RunOn::Unknown(name) = &run_on {
            unserved.get_or_insert_with(|| {
                format!("the run_on value '{name}' is not known to this version")
            });
        }
        let description = text("description")?.unwrap_or_default();
        let effects = match map.get("effects") {
            None | Some(ElementProperty::Null) => Vec::new(),
            Some(ElementProperty::Array(items)) => {
                let mut effects = Vec::with_capacity(items.len().min(MAX_JOB_EFFECTS));
                for (index, item) in items.iter().enumerate() {
                    match effect_from_value(item) {
                        Ok(effect) => effects.push(effect),
                        Err(reason) => {
                            unserved.get_or_insert_with(|| {
                                format!("effect {index}: {reason} (not known to this version)")
                            });
                        }
                    }
                }
                effects
            }
            Some(_) => return Err(malformed("effects", "must be a list")),
        };
        let mut job = JobDefinition::new(schedule, action)
            .with_enabled(enabled)
            .with_limits(limits)
            .with_run_on(run_on)
            .with_effects(effects)
            .with_description(description);
        const KNOWN: [&str; 11] = [
            "schedule",
            "action",
            "query",
            "args",
            "path",
            "enabled",
            "timeout_ms",
            "memory_limit",
            "run_on",
            "description",
            "effects",
        ];
        for (key, value) in map {
            if !KNOWN.contains(&key.as_str()) && !skip.contains(&key.as_str()) {
                job.extra.insert(key.clone(), value.to_logical());
            }
        }
        if unserved.is_some() {
            let mut original = PropertyMap::default();
            for (key, value) in map {
                if !skip.contains(&key.as_str()) {
                    original.insert(key.clone(), value.to_logical());
                }
            }
            job.original = Some(original);
            job.unserved = unserved;
        }
        Ok(job)
    }

    /// Writes the exchange-form fields (the payload keys) into `map`.
    pub(crate) fn write_fields(&self, map: &mut PropertyMap) {
        for (key, value) in self.to_payload() {
            map.insert(key, value);
        }
    }

    /// The rules of [`Definition::validate`] for a job named `name`: the schedule parses (an
    /// unknown action's job only needs a schedule text of a valid size), a query job names a
    /// query, a backup job a path, a timeout is not zero, sizes are bounded, no reserved key.
    pub(crate) fn validate(&self, name: &str) -> Result<(), DefinitionError> {
        let field = |field: &str, reason: String| DefinitionError::InvalidField {
            name: name.to_string(),
            field: field.to_string(),
            reason,
        };
        if self.schedule.len() > MAX_SCHEDULE_BYTES {
            return Err(field(
                "schedule",
                format!("it is longer than {MAX_SCHEDULE_BYTES} bytes"),
            ));
        }
        if self.unserved.is_none()
            && let Err(err) = self.schedule.parse::<CronSchedule>()
        {
            return Err(field("schedule", err.to_string()));
        }
        match &self.action {
            JobTask::Query { name: query, .. } if query.trim().is_empty() => {
                return Err(field(
                    "query",
                    "it is empty; name the saved query the job calls".to_string(),
                ));
            }
            JobTask::Backup { path } if path.trim().is_empty() => {
                return Err(field(
                    "path",
                    "it is empty; name the directory the backups go to".to_string(),
                ));
            }
            JobTask::Backup { path } if path.len() > MAX_JOB_PATH_BYTES => {
                return Err(field(
                    "path",
                    format!("it is longer than {MAX_JOB_PATH_BYTES} bytes"),
                ));
            }
            JobTask::Query { .. }
            | JobTask::Checkpoint
            | JobTask::Backup { .. }
            | JobTask::Unknown(_) => {}
        }
        if self.limits.timeout == Some(Duration::ZERO) {
            return Err(field(
                "timeout_ms",
                "it is 0; leave it out for no timeout".to_string(),
            ));
        }
        if self.limits.memory_limit == Some(0) {
            return Err(field(
                "memory_limit",
                "it is 0; leave it out for no memory limit".to_string(),
            ));
        }
        if self.effects.len() > MAX_JOB_EFFECTS {
            return Err(field(
                "effects",
                format!("there are more than {MAX_JOB_EFFECTS}"),
            ));
        }
        if self.description.len() > MAX_DESCRIPTION_BYTES {
            return Err(field(
                "description",
                format!("it is longer than {MAX_DESCRIPTION_BYTES} bytes"),
            ));
        }
        if let Some(key) = self
            .extra
            .keys()
            .find(|key| RESERVED_PAYLOAD_KEYS.contains(&key.as_str()))
        {
            return Err(field(
                "payload",
                format!("the payload key '{key}' is reserved"),
            ));
        }
        Ok(())
    }
}

/// An integer figure (saturated at `i64::MAX`).
fn int(value: u64) -> ElementProperty {
    ElementProperty::from(i64::try_from(value).unwrap_or(i64::MAX))
}

fn action_name(action: &Action<'_>) -> Option<&'static str> {
    match action {
        Action::Read => Some("read"),
        Action::Create => Some("create"),
        Action::Update => Some("update"),
        Action::Delete => Some("delete"),
        Action::Execute => Some("execute"),
        Action::Custom(_) => None,
    }
}

/// An effect as a payload value: `{action, resource: "data", element?, label?}`, `{action,
/// resource: "file", path}`, `{action, resource: "definition", kind?, name?}`. Other resources
/// (a job never stores them) are written as `{action, resource: "other"}`.
fn effect_value(effect: &AccessRequestBuf) -> ElementProperty {
    let request = effect.as_request();
    let mut map = PropertyMap::default();
    map.insert(
        "action",
        action_name(&request.action).unwrap_or("custom").into(),
    );
    match request.resource {
        Resource::Data { element, label } => {
            map.insert("resource", "data".into());
            if let Some(element) = element {
                map.insert("element", element.as_str().into());
            }
            if let Some(label) = label {
                map.insert("label", label.into());
            }
        }
        Resource::File { path } => {
            map.insert("resource", "file".into());
            map.insert("path", path.to_string_lossy().as_ref().into());
        }
        Resource::Definition { kind, name } => {
            map.insert("resource", "definition".into());
            if let Some(kind) = kind {
                map.insert("kind", int(u64::from(kind.code())));
            }
            if let Some(name) = name {
                map.insert("name", name.into());
            }
        }
        Resource::Schema
        | Resource::Option { .. }
        | Resource::Embedder { .. }
        | Resource::Custom { .. } => {
            map.insert("resource", "other".into());
        }
    }
    ElementProperty::Object(Box::new(map))
}

/// The inverse of [`effect_value`]; `Err` names what this version does not know.
fn effect_from_value(value: &ElementProperty) -> Result<AccessRequestBuf, String> {
    let map = value
        .as_object()
        .ok_or_else(|| "an effect must be an object".to_string())?;
    let text = |key: &str| map.get(key).and_then(|v| v.as_str()).map(str::to_string);
    let action = match text("action").as_deref() {
        Some("read") => Action::Read,
        Some("create") => Action::Create,
        Some("update") => Action::Update,
        Some("delete") => Action::Delete,
        Some("execute") => Action::Execute,
        other => return Err(format!("the action {other:?}")),
    };
    let request = match text("resource").as_deref() {
        Some("data") => {
            let element = match text("element").as_deref() {
                None => None,
                Some("vertex") => Some(ElementKind::Vertex),
                Some("edge") => Some(ElementKind::Edge),
                Some(other) => return Err(format!("the element kind '{other}'")),
            };
            let label = text("label");
            AccessRequest::new(
                action,
                Resource::Data {
                    element,
                    label: label.as_deref(),
                },
            )
            .to_owned_request()
        }
        Some("file") => {
            let path = text("path").ok_or_else(|| "a file effect without 'path'".to_string())?;
            AccessRequest::new(
                action,
                Resource::File {
                    path: Path::new(&path),
                },
            )
            .to_owned_request()
        }
        Some("definition") => {
            let kind = match map.get("kind") {
                None => None,
                Some(value) => Some(
                    value
                        .as_i64()
                        .and_then(|code| u8::try_from(code).ok())
                        .map(DefinitionKind::from_code)
                        .ok_or_else(|| "a definition kind is a number 0 to 255".to_string())?,
                ),
            };
            let name = text("name");
            AccessRequest::new(
                action,
                Resource::Definition {
                    kind,
                    name: name.as_deref(),
                },
            )
            .to_owned_request()
        }
        other => return Err(format!("the resource {other:?}")),
    };
    Ok(request)
}

impl Definition {
    /// A scheduled job named `name` (kind [`DefinitionKind::JOB`], not critical: a reader that
    /// does not know the kind keeps it and does not run it).
    pub fn job(name: impl Into<String>, job: JobDefinition) -> Self {
        Definition::from_parts(
            DefinitionKind::JOB,
            DefinitionFlags::NONE,
            name.into(),
            DefinitionBody::Job(job),
        )
    }

    /// The job, when this is a decoded one.
    pub fn as_job(&self) -> Option<&JobDefinition> {
        match self.body() {
            DefinitionBody::Job(job) => Some(job),
            DefinitionBody::Query(_)
            | DefinitionBody::Compression(_)
            | DefinitionBody::PropertyIndex(_)
            | DefinitionBody::Opaque(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query_job() -> JobDefinition {
        let mut args = PropertyMap::default();
        args.insert("limit", ElementProperty::from(10i64));
        JobDefinition::new(
            "*/5 * * * *",
            JobTask::Query {
                name: "cleanup".to_string(),
                args,
            },
        )
        .with_limits(
            JobLimits::new()
                .with_timeout(Duration::from_secs(30))
                .with_memory_limit(1 << 20),
        )
        .with_description("drops stale rows")
        .with_effects(vec![
            AccessRequest::new(
                Action::Delete,
                Resource::Data {
                    element: Some(ElementKind::Vertex),
                    label: Some("event"),
                },
            )
            .to_owned_request(),
        ])
    }

    #[test]
    fn payloads_round_trip() {
        for job in [
            query_job(),
            JobDefinition::new("0 3 * * *", JobTask::Checkpoint).with_enabled(false),
            JobDefinition::new(
                "0 4 * * 0",
                JobTask::Backup {
                    path: "/backups/kb".to_string(),
                },
            )
            .with_run_on(RunOn::Primary),
        ] {
            let payload = job.to_payload();
            let back = JobDefinition::from_payload(&payload).unwrap();
            assert_eq!(back, job);
            assert!(Definition::job("j", back).validate().is_ok());
        }
    }

    #[test]
    fn a_newer_action_or_effect_is_kept_byte_for_byte_and_not_run() {
        let mut payload = query_job().to_payload();
        payload.insert("action", "compact_vectors".into());
        payload.insert("future_key", ElementProperty::from(1i64));
        let job = JobDefinition::from_payload(&payload).unwrap();
        assert!(job.unserved_reason().unwrap().contains("compact_vectors"));
        assert_eq!(job.to_payload(), payload);

        let mut payload = query_job().to_payload();
        let mut effect = PropertyMap::default();
        effect.insert("action", "create".into());
        effect.insert("resource", "tenant".into());
        payload.insert(
            "effects",
            ElementProperty::Array(vec![ElementProperty::Object(Box::new(effect))]),
        );
        let job = JobDefinition::from_payload(&payload).unwrap();
        assert!(job.unserved_reason().unwrap().contains("effect 0"));
        assert_eq!(job.to_payload(), payload);
    }

    #[test]
    fn validation_names_the_field() {
        let field_of = |job: JobDefinition| match Definition::job("j", job).validate() {
            Err(DefinitionError::InvalidField { field, .. }) => field,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            field_of(JobDefinition::new("61 * * * *", JobTask::Checkpoint)),
            "schedule"
        );
        assert_eq!(
            field_of(JobDefinition::new(
                "* * * * *",
                JobTask::Backup {
                    path: " ".to_string()
                }
            )),
            "path"
        );
        assert_eq!(
            field_of(
                JobDefinition::new("* * * * *", JobTask::Checkpoint)
                    .with_limits(JobLimits::new().with_timeout(Duration::ZERO))
            ),
            "timeout_ms"
        );
        assert!(matches!(
            Definition::job("not a name", JobDefinition::new("* * * * *", JobTask::Checkpoint))
                .validate(),
            Err(DefinitionError::InvalidName { .. })
        ));
    }
}
