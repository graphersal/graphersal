//! The scheduled jobs of the session's active store (book page "Scheduled Jobs"): the host
//! ticks [`Session::tick_jobs`] (the dev server from a thread every few seconds, the playground
//! from a timer in its worker); the session keeps one [`Scheduler`] for the active store and
//! builds a new one when the active store changes. Without a store nothing runs. The
//! operations behind the playground's Catalog ▸ Jobs manager, the dev server's `/api/jobs*`
//! endpoints and the MCP tool `list_jobs`.
//!
//! JSON forms:
//!
//! - **List** ([`Session::jobs_view`]): `{timeZone, store, jobs: [entry]}`, the system jobs
//!   first, then the jobs of users (the catalog's, by name). An **entry**: `{name, kind:
//!   "system"|"user", schedule, enabled, nextAt, last: {startedAt, micros, error} | null,
//!   timeoutMs, memoryLimit, scheduled, reason, runs}` (times in microseconds since the Unix
//!   epoch; `scheduled` false with a `reason` when the job does not run: the graph is not in a
//!   store, this version does not run it, a system job has its name; `runs` the workload
//!   statistics' `{last_hour, last_two_days, last_ninety_days}` counts of its runs, `null`
//!   when there are none). A system job adds `setting: {schedule, enabled} | null` (the
//!   instance's changes, [`JobOverride`]); a job of a user adds `action, query, args, path,
//!   runOn, description, effects: [{action, element, label, text}], served`.
//! - **Define** ([`Session::define_job`]): a spec `{name, schedule, action: "query" |
//!   "checkpoint" | "backup", query?, args?, path?, enabled?, timeout_ms?, memory_limit?,
//!   run_on?, description?}` (the keys of `g.define_job`, which runs it with the definer's
//!   rights and stores the effects they allow); answers `{ok: true, job: entry}` or `{ok:
//!   false, errors: [{field, message, help?}]}` with `field` one of the keys or `general`.

use crate::error::SessionError;
use std::sync::{Arc, Weak};

use graphersal::catalog::{Definition, DefinitionKind, JobDefinition, JobTask};
use graphersal::jobs::{CronSchedule, JobInfo, JobKind, Scheduler};
use graphersal::workload::{JobOverride, JobRunRecord};
use serde_json::{Map, Value, json};

use crate::queries::FieldError;
use crate::session::Session;
use crate::storage::SessionStore;

/// The session's scheduler and the store it was built for.
#[derive(Default)]
pub struct SessionJobs {
    /// The IANA time zone the schedules run in (`None`: UTC).
    time_zone: Option<String>,
    /// The scheduler and the store it belongs to (a `Weak` keeps the allocation, so a new
    /// store never takes the old one's address and is mistaken for it).
    current: Option<(Weak<dyn SessionStore>, Scheduler)>,
    /// The catalog's job definitions the scheduler's user jobs were built from.
    user_jobs: Vec<graphersal::catalog::Definition>,
    /// The job definitions of the catalog the store does not schedule, with the reason
    /// ([`graphersal::persist::UserJobs::not_scheduled`]).
    not_scheduled: Vec<graphersal::persist::NotScheduledJob>,
}

impl Session {
    /// Sets the time zone the scheduled jobs run in (the instance's local zone, which the host
    /// knows: `TZ` or `/etc/localtime` natively, the browser's zone in the playground).
    pub fn set_time_zone(&mut self, name: &str) -> Result<(), SessionError> {
        Scheduler::new()
            .with_time_zone(name)
            .map_err(|err| err.to_string())?;
        self.jobs.time_zone = Some(name.to_string());
        // Rebuilt in the new zone at the next tick.
        self.jobs.current = None;
        Ok(())
    }

    /// The scheduler of the active store, built when the active store changed (`None`: no
    /// store). Fails when the catalog or the store's jobs cannot be read, the time zone set
    /// before no longer resolves, or a system job is refused by the scheduler.
    fn scheduler(&mut self) -> Result<Option<&mut Scheduler>, SessionError> {
        let Some(store) = self.active_store() else {
            self.jobs.current = None;
            return Ok(None);
        };
        let same = self
            .jobs
            .current
            .as_ref()
            .is_some_and(|(known, _)| std::ptr::addr_eq(known.as_ptr(), Arc::as_ptr(&store)));
        if !same {
            // The zone was checked when it was set ([`Self::set_time_zone`]): one that no
            // longer resolves is an error, never a silent UTC.
            let scheduler = match &self.jobs.time_zone {
                Some(zone) => Scheduler::new()
                    .with_time_zone(zone)
                    .map_err(|err| SessionError::failed(err.to_string()))?,
                None => Scheduler::new(),
            };
            let mut scheduler = scheduler.with_stats(store.workload_stats());
            for job in store.system_jobs() {
                scheduler
                    .add(job)
                    .map_err(|err| SessionError::failed(err.to_string()))?;
            }
            self.jobs.current = Some((Arc::downgrade(&store), scheduler));
            self.jobs.user_jobs.clear();
            self.jobs.not_scheduled.clear();
        }
        // The user jobs follow the catalog: rebuilt when its job definitions changed (a define,
        // a drop, a rollback or a catalog load).
        let definitions = self
            .graph()
            .definitions(Some(graphersal::catalog::DefinitionKind::JOB))?;
        let Some((_, scheduler)) = self.jobs.current.as_mut() else {
            return Ok(None);
        };
        if definitions != self.jobs.user_jobs {
            let found = store.user_jobs()?;
            scheduler.sync_user_jobs(found.jobs);
            self.jobs.not_scheduled = found.not_scheduled;
            self.jobs.user_jobs = definitions;
        }
        Ok(self.jobs.current.as_mut().map(|(_, scheduler)| scheduler))
    }

    /// Runs the due jobs of the active store at `now` (microseconds since the Unix epoch);
    /// returns the names of the jobs that ran. Fails when the scheduler cannot be built
    /// (the catalog or the store's jobs cannot be read).
    pub fn tick_jobs(&mut self, now: i64) -> Result<Vec<String>, SessionError> {
        Ok(self
            .scheduler()?
            .map_or_else(Vec::new, |scheduler| scheduler.tick(now)))
    }

    /// The jobs of the served graph (module docs, "List"): the active store's system jobs, then
    /// the catalog's jobs of users by name. Fails when the catalog, the store's jobs or the
    /// workload statistics cannot be read.
    pub fn jobs_view(&mut self) -> Result<Value, SessionError> {
        let zone = self
            .jobs
            .time_zone
            .clone()
            .unwrap_or_else(|| "UTC".to_string());
        let store = self.active_store();
        let stats = store.as_ref().map(|store| store.workload_stats());
        let scheduled: Vec<JobInfo> = self
            .scheduler()?
            .map(|scheduler| scheduler.jobs())
            .unwrap_or_default();
        let runs = self.job_runs()?;
        let mut jobs: Vec<Value> = scheduled
            .iter()
            .filter(|job| job.kind == JobKind::System)
            .map(|job| {
                let mut entry = scheduler_entry(job);
                let setting = stats
                    .as_ref()
                    .and_then(|stats| stats.job_override(&job.name));
                entry["setting"] = setting.map_or(
                    Value::Null,
                    |setting| json!({"schedule": setting.schedule, "enabled": setting.enabled}),
                );
                entry["scheduled"] = json!(true);
                entry["reason"] = Value::Null;
                entry
            })
            .collect();
        let definitions = self.graph().definitions(Some(DefinitionKind::JOB))?;
        for definition in &definitions {
            let Some(job) = definition.as_job() else {
                continue;
            };
            let running = scheduled
                .iter()
                .find(|info| info.kind == JobKind::User && info.name == definition.name());
            let mut entry = definition_entry(definition.name(), job);
            let reason = match (running, job.unserved_reason()) {
                (Some(_), _) => None,
                (None, Some(reason)) => Some(reason),
                (None, None) if store.is_none() => Some(NO_STORE.to_string()),
                (None, None) => Some(
                    self.jobs
                        .not_scheduled
                        .iter()
                        .find(|skipped| skipped.name == definition.name())
                        .map_or_else(
                            || NAMED_LIKE_SYSTEM.to_string(),
                            |skipped| skipped.reason.clone(),
                        ),
                ),
            };
            entry["scheduled"] = json!(running.is_some());
            entry["reason"] = json!(reason);
            entry["nextAt"] = json!(running.and_then(|info| info.next_at));
            entry["last"] = running
                .and_then(|info| info.last.as_ref())
                .map_or(Value::Null, run_json);
            jobs.push(entry);
        }
        for entry in &mut jobs {
            let name = entry["name"].as_str().unwrap_or_default().to_string();
            entry["runs"] = runs.get(&name).cloned().unwrap_or(Value::Null);
        }
        Ok(json!({ "timeZone": zone, "store": store.is_some(), "jobs": jobs }))
    }

    /// The job `name` as its list entry (module docs), `null` when there is none. Fails like
    /// [`Self::jobs_view`].
    pub fn job_entry(&mut self, name: &str) -> Result<Value, SessionError> {
        Ok(self.jobs_view()?["jobs"]
            .as_array()
            .and_then(|jobs| jobs.iter().find(|job| job["name"] == name))
            .cloned()
            .unwrap_or(Value::Null))
    }

    /// The run counts of every job from the workload statistics: name -> `{last_hour,
    /// last_two_days, last_ninety_days}` (each `{count, failed, total_micros, max_micros, ..}`).
    fn job_runs(&self) -> Result<std::collections::HashMap<String, Value>, SessionError> {
        let report = self.workload_report()?;
        Ok(report["jobs"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| {
                        let name = row["name"].as_str()?.to_string();
                        Some((
                            name,
                            json!({
                                "last_hour": row["last_hour"],
                                "last_two_days": row["last_two_days"],
                                "last_ninety_days": row["last_ninety_days"],
                            }),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Defines (or replaces) a job of a user from a spec (module docs, "Define") with the
    /// playground's rights ([`crate::playground_authorizer`]: no files, so no backup jobs).
    /// Answers `{ok: true, job: <entry>}` or `{ok: false, errors: [{field, message, help?}]}`.
    pub fn define_job(&mut self, spec: &Value) -> Value {
        self.define_job_with_authorizer(spec, crate::playground_authorizer())
    }

    /// [`Self::define_job`] with the definer's rights `authorizer`: the dev server, whose user
    /// owns the machine's files (its Store menu writes backups anywhere), allows files.
    pub fn define_job_with_authorizer(
        &mut self,
        spec: &Value,
        authorizer: Arc<dyn graphersal::auth::Authorizer>,
    ) -> Value {
        let queries = match self.graph().definitions(Some(DefinitionKind::QUERY)) {
            Ok(queries) => queries,
            Err(err) => return field_errors(&[FieldError::new("general", err.to_string())]),
        };
        let script = match job_call_text(spec, |name| {
            queries.iter().any(|definition| definition.name() == name)
        }) {
            Ok(script) => script,
            Err(errors) => return field_errors(&errors),
        };
        let any: Arc<dyn graphersal::storage::AnyGraph> = self.graph.clone();
        let limits = self.presets().limits.clone();
        match graphersal::script::eval_value_with_limits(
            any,
            &script,
            Default::default(),
            &limits,
            authorizer,
        ) {
            Ok(_) => {
                let name = spec["name"].as_str().unwrap_or_default().trim().to_string();
                match self.job_entry(&name) {
                    Ok(job) => json!({"ok": true, "job": job}),
                    Err(err) => field_errors(&[FieldError::new("general", err.to_string())]),
                }
            }
            Err(err) => {
                let text = graphersal::script::map_eval_error(err, &limits).to_string();
                field_errors(&[define_error(&text)])
            }
        }
    }

    /// Drops the job of a user `name`: `true`, or `false` when there was none.
    pub fn drop_job(&mut self, name: &str) -> Result<bool, SessionError> {
        self.graph.drop_definition(DefinitionKind::JOB, name)
    }

    /// Switches job `name` on or off: a job of a user in its catalog definition (one commit;
    /// its stored effects stay), a system job in the instance's settings (kept in the store's
    /// workload sidecar). Answers the job's entry.
    pub fn set_job_enabled(&mut self, name: &str, enabled: bool) -> Result<Value, SessionError> {
        if let Some(definition) = self.graph().definition(DefinitionKind::JOB, name)? {
            let job = definition
                .as_job()
                .ok_or_else(|| format!("the job '{name}' is kept opaque and cannot be changed"))?
                .clone()
                .with_enabled(enabled);
            self.graph
                .set_definitions(vec![Definition::job(name, job)])
                .map_err(|err| err.to_string())?;
            return self.job_entry(name);
        }
        self.change_system_job(name, |setting| setting.with_enabled(enabled))
    }

    /// Gives the SYSTEM job `name` the schedule `schedule` (5-field cron), or its built-in one
    /// again with `None`; kept in the instance's settings. A job of a user changes its
    /// schedule by being defined again ([`Self::define_job`]). Answers the job's entry.
    pub fn set_job_schedule(
        &mut self,
        name: &str,
        schedule: Option<&str>,
    ) -> Result<Value, SessionError> {
        if self
            .graph()
            .definition(DefinitionKind::JOB, name)?
            .is_some()
        {
            return Err(format!(
                "'{name}' is a job of a user: define it again with the new schedule"
            )
            .into());
        }
        let schedule = schedule.map(str::trim).filter(|text| !text.is_empty());
        if let Some(text) = schedule {
            text.parse::<CronSchedule>()
                .map_err(|err| err.to_string())?;
        }
        self.change_system_job(name, |mut setting| {
            setting.schedule = schedule.map(str::to_string);
            setting
        })
    }

    /// Changes the settings of the active store's system job `name` with `change`, then
    /// applies them to the scheduler.
    fn change_system_job(
        &mut self,
        name: &str,
        change: impl FnOnce(JobOverride) -> JobOverride,
    ) -> Result<Value, SessionError> {
        let store = self.active_store().ok_or_else(|| NO_STORE.to_string())?;
        if !store.system_jobs().iter().any(|job| job.name() == name) {
            return Err(format!("no job named '{name}'").into());
        }
        let stats = store.workload_stats();
        let setting = change(stats.job_override(name).unwrap_or_default());
        stats.set_job_override(name, setting);
        // The store hands out its system jobs with the settings applied: the scheduler takes
        // the job's new schedule and switch from there.
        let job = store
            .system_jobs()
            .into_iter()
            .find(|job| job.name() == name);
        if let (Some(job), Some(scheduler)) = (job, self.scheduler()?) {
            scheduler
                .set_schedule(name, job.schedule().clone())
                .and_then(|()| scheduler.set_enabled(name, job.enabled()))
                .map_err(|err| err.to_string())?;
        }
        self.job_entry(name)
    }

    /// Runs job `name` now, whatever its schedule (its planned time stays). Answers the job's
    /// entry, whose `last` is this run.
    pub fn run_job_now(&mut self, name: &str) -> Result<Value, SessionError> {
        let now = now_micros();
        let scheduler = self
            .scheduler()?
            .ok_or_else(|| SessionError::invalid(NO_STORE))?;
        scheduler
            .run_now(name, now)
            .map_err(|err| err.to_string())?;
        self.job_entry(name)
    }

    /// The run history of job `name` in tier `tier` (`minutes`, `hours`, `days`): `{tier,
    /// buckets: [{start, count, failed, total_micros, max_micros, items}]}`, oldest first (the
    /// data of a chart; empty when it never ran in that window).
    pub fn job_history(&self, name: &str, tier: &str) -> Result<Value, SessionError> {
        let report = self.workload_report_with_history(tier)?;
        let buckets = report["jobs"]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["name"] == name))
            .map_or(json!([]), |row| row["history"].clone());
        Ok(json!({"tier": tier, "buckets": buckets}))
    }
}

/// Why a job of the catalog does not run: the graph is not in a store.
const NO_STORE: &str = "the graph is not in a store: scheduled jobs run only in a store";

/// Why a job of the catalog does not run: a system job has its name.
const NAMED_LIKE_SYSTEM: &str = "a system job has this name: rename the job";

fn now_micros() -> i64 {
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as i64)
}

impl Session {
    /// The workload statistics of the served graph
    /// ([`graphersal::workload::WorkloadReport::to_property`] as JSON: `{level, queries,
    /// properties, locks, jobs}`); a graph without statistics (not in a store) answers level
    /// `off` with empty lists. Reading it is not counted. Fails when the report does not read
    /// back as JSON (never expected: the report writes its own JSON).
    pub fn workload_report(&self) -> Result<Value, SessionError> {
        self.workload_report_in(None)
    }

    /// [`Self::workload_report`] with every row's `history` of the tier `tier` (`minutes`,
    /// `hours` or `days`: its buckets, oldest first, the data of a chart) and `history_tier`;
    /// `Err` names the tiers for another `tier`.
    pub fn workload_report_with_history(&self, tier: &str) -> Result<Value, SessionError> {
        let tier = graphersal::workload::HistoryTier::from_name(tier).ok_or_else(|| {
            SessionError::invalid(format!(
                "unknown history tier '{tier}': use minutes, hours or days"
            ))
        })?;
        self.workload_report_in(Some(tier))
    }

    fn workload_report_in(
        &self,
        history: Option<graphersal::workload::HistoryTier>,
    ) -> Result<Value, SessionError> {
        let now = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .map_or(0, |d| d.as_micros() as i64);
        let stats = self.graph().workload_stats().unwrap_or_else(|| {
            Arc::new(graphersal::workload::WorkloadStats::new(
                graphersal::workload::WorkloadLevel::Off,
            ))
        });
        let report = match history {
            Some(tier) => stats.report_with_history(now, tier),
            None => stats.report(now),
        };
        serde_json::from_str(&report.to_json())
            .map_err(|err| SessionError::failed(format!("the workload report is not JSON: {err}")))
    }
}

/// The keys a spec may have (the keys of `g.define_job`).
const KEYS: [&str; 11] = [
    "name",
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
];

/// A run as JSON: `{startedAt, micros, error}`.
fn run_json(run: &JobRunRecord) -> Value {
    json!({"startedAt": run.started_at, "micros": run.micros, "error": run.error})
}

/// The common part of a scheduled job's entry.
fn scheduler_entry(job: &JobInfo) -> Value {
    json!({
        "name": job.name,
        "kind": job.kind.name(),
        "schedule": job.schedule,
        "enabled": job.enabled,
        "nextAt": job.next_at,
        "last": job.last.as_ref().map_or(Value::Null, run_json),
        "timeoutMs": job.limits.timeout.map(|t| t.as_millis() as u64),
        "memoryLimit": job.limits.memory_limit,
    })
}

/// The entry of a job of a user from its catalog definition.
fn definition_entry(name: &str, job: &JobDefinition) -> Value {
    let limits = job.limits();
    let mut entry = json!({
        "name": name,
        "kind": "user",
        "schedule": job.schedule(),
        "enabled": job.enabled(),
        "action": job.action().name(),
        "query": Value::Null,
        "args": Value::Null,
        "path": Value::Null,
        "timeoutMs": limits.timeout.map(|t| t.as_millis() as u64),
        "memoryLimit": limits.memory_limit,
        "runOn": job.run_on().as_str(),
        "description": job.description(),
        "effects": job.effects().iter().map(crate::queries::effect_json).collect::<Vec<_>>(),
        "served": job.unserved_reason().is_none(),
    });
    match job.action() {
        JobTask::Query { name, args } => {
            entry["query"] = json!(name);
            entry["args"] = Value::Object(
                args.iter()
                    .map(|(key, value)| (key.to_string(), crate::queries::value_json(value)))
                    .collect(),
            );
        }
        JobTask::Backup { path } => entry["path"] = json!(path),
        _ => {}
    }
    entry
}

fn field_errors(errors: &[FieldError]) -> Value {
    json!({"ok": false, "errors": errors.iter().map(FieldError::to_value).collect::<Vec<_>>()})
}

/// The field an error of the engine's `define_job` belongs to (the spec's own checks found
/// none): a refused file is the backup `path`, an argument the `args`, the rest `general`;
/// the `Help:` line of the diagnostic becomes the help.
fn define_error(text: &str) -> FieldError {
    let (message, help) = match text.split_once("\nHelp:") {
        Some((message, help)) => (message.trim(), Some(help.trim().to_string())),
        None => (text.trim(), None),
    };
    let field = if message.contains("File(") || message.contains("file") {
        "path"
    } else if message.contains("argument") || message.contains("parameter") {
        "args"
    } else if message.contains("saved query") {
        "query"
    } else {
        "general"
    };
    let mut error = FieldError::new(field, message);
    error.help = help;
    error
}

/// The `g.define_job(#{..})` call of a spec (module docs, "Define"), every wrong field reported
/// at once; `query_exists` says whether the catalog holds a saved query of a name.
fn job_call_text(
    spec: &Value,
    query_exists: impl Fn(&str) -> bool,
) -> Result<String, Vec<FieldError>> {
    let Some(object) = spec.as_object() else {
        return Err(vec![FieldError::new(
            "general",
            "the job is a JSON object {name, schedule, action, query?, args?, path?, ...}",
        )]);
    };
    if let Some(unknown) = object.keys().find(|key| !KEYS.contains(&key.as_str())) {
        return Err(vec![FieldError::new(
            "general",
            format!("unknown key '{unknown}' (the keys are {})", KEYS.join(", ")),
        )]);
    }
    let mut errors = Vec::new();
    let mut text = |key: &str| match object.get(key) {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) if text.trim().is_empty() => None,
        Some(Value::String(text)) => Some(text.trim().to_string()),
        Some(_) => {
            errors.push(FieldError::new(key, format!("{key} must be a string")));
            None
        }
    };
    let name = text("name");
    let schedule = text("schedule");
    let action = text("action");
    let query = text("query");
    let path = text("path");
    let run_on = text("run_on");
    let description = text("description");
    match &name {
        None => errors.push(FieldError::new(
            "name",
            "name the job, for example nightly_purge",
        )),
        Some(name) => {
            let probe = Definition::job(
                name.as_str(),
                JobDefinition::new("* * * * *", JobTask::Checkpoint),
            );
            if let Err(err) = probe.validate() {
                errors.push(FieldError::of(&err));
            }
        }
    }
    match &schedule {
        None => errors.push(FieldError::new(
            "schedule",
            "give a 5-field cron schedule, for example 0 3 * * * (daily at 03:00)",
        )),
        Some(text) => {
            if let Err(err) = text.parse::<CronSchedule>() {
                errors.push(FieldError::new("schedule", err.to_string()));
            }
        }
    }
    match action.as_deref() {
        Some("query") => match &query {
            None => errors.push(FieldError::new(
                "query",
                "choose the saved query the job calls",
            )),
            Some(name) if !query_exists(name) => errors.push(FieldError::new(
                "query",
                format!("there is no saved query '{name}'"),
            )),
            Some(_) => {}
        },
        Some("checkpoint") => {}
        Some("backup") => {
            if path.is_none() {
                errors.push(FieldError::new(
                    "path",
                    "name the directory the backups go to",
                ));
            }
        }
        Some(other) => errors.push(FieldError::new(
            "action",
            format!("'{other}' is not query, checkpoint or backup"),
        )),
        None => errors.push(FieldError::new(
            "action",
            "choose query, checkpoint or backup",
        )),
    }
    if !matches!(
        object.get("args"),
        None | Some(Value::Null) | Some(Value::Object(_))
    ) {
        errors.push(FieldError::new(
            "args",
            "the arguments are an object: parameter -> value",
        ));
    }
    if !matches!(
        object.get("enabled"),
        None | Some(Value::Null) | Some(Value::Bool(_))
    ) {
        errors.push(FieldError::new("enabled", "enabled is true or false"));
    }
    for key in ["timeout_ms", "memory_limit"] {
        match object.get(key) {
            None | Some(Value::Null) => {}
            Some(Value::Number(n)) if n.as_u64().is_some_and(|n| n > 0) => {}
            Some(_) => errors.push(FieldError::new(key, format!("{key} is a positive integer"))),
        }
    }
    if let Some(value) = &run_on
        && !matches!(value.as_str(), "any" | "primary" | "replica")
    {
        errors.push(FieldError::new(
            "run_on",
            format!("'{value}' is not any, primary or replica"),
        ));
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    // The spec as the DSL's map, with the trimmed texts and only the keys that are set.
    let mut map = Map::new();
    let texts = [
        ("name", name),
        ("schedule", schedule),
        ("action", action.clone()),
        ("run_on", run_on),
        ("description", description),
    ];
    for (key, value) in texts {
        if let Some(value) = value {
            map.insert(key.to_string(), json!(value));
        }
    }
    match action.as_deref() {
        Some("query") => {
            map.insert("query".to_string(), json!(query));
            if let Some(args @ Value::Object(_)) = object.get("args") {
                map.insert("args".to_string(), args.clone());
            }
        }
        Some("backup") => {
            map.insert("path".to_string(), json!(path));
        }
        _ => {}
    }
    for key in ["enabled", "timeout_ms", "memory_limit"] {
        if let Some(value) = object.get(key).filter(|value| !value.is_null()) {
            map.insert(key.to_string(), value.clone());
        }
    }
    let mut out = String::from("g.define_job(");
    crate::queries::rhai_literal(&Value::Object(map), 0, &mut out)
        .map_err(|message| vec![FieldError::new("args", message)])?;
    out.push(')');
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_800_000_000_000_000;
    const MINUTE: i64 = 60_000_000;

    #[test]
    fn the_active_store_s_system_jobs_tick() {
        let mut s = Session::new();
        // Without a store there are no jobs.
        assert_eq!(s.jobs_view().unwrap()["jobs"], json!([]));
        assert!(s.tick_jobs(T0).unwrap().is_empty());
        s.set_memory_stores(true);
        s.load_sample("modern").unwrap();
        s.set_time_zone("Europe/Berlin").unwrap();
        assert!(s.set_time_zone("Nowhere/Land").is_err());
        let view = s.jobs_view().unwrap();
        assert_eq!(view["timeZone"], "Europe/Berlin");
        assert_eq!(view["jobs"][0]["name"], "stats_flush");
        assert_eq!(view["jobs"][0]["kind"], "system");
        assert_eq!(view["jobs"][1]["enabled"], false);
        s.tick_jobs(T0).unwrap();
        assert_eq!(s.tick_jobs(T0 + MINUTE).unwrap(), ["stats_flush"]);
        let view = s.jobs_view().unwrap();
        assert!(view["jobs"][0]["last"]["error"].is_null(), "{view}");
        // Another active store gets its own scheduler (planned anew).
        s.load_sample("modern").unwrap();
        assert!(s.jobs_view().unwrap()["jobs"][0]["last"].is_null());
    }

    #[test]
    fn only_the_users_queries_count_under_the_hosts_channel() {
        use graphersal::workload::{Channel, scope};
        let mut s = Session::new();
        assert_eq!(s.workload_report().unwrap()["level"], "off");
        s.set_memory_stores(true);
        s.load_sample("modern").unwrap();
        scope(Channel::Mcp, true, || {
            // The page's own work: technical, not counted.
            s.graph_view(r#"{"maxNodes": 10, "maxEdges": 10}"#).unwrap();
            let answer = s.execute(
                r#"g.v().has_label("person").values("name").to_list()"#,
                &crate::RunOptions::default(),
            );
            assert!(answer.contains("marko"), "{answer}");
        });
        let report = s.workload_report().unwrap();
        let queries = report["queries"].as_array().unwrap();
        assert_eq!(queries.len(), 1, "{report}");
        assert_eq!(queries[0]["channel"], "mcp");
        assert_eq!(queries[0]["last_hour"]["count"], 1);
        assert_eq!(queries[0]["last_hour"]["items"], 4);
        let names = report["properties"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["key"] == "name")
            .cloned()
            .unwrap_or_default();
        assert_eq!(names["label"], "person", "{report}");
        assert_eq!(names["access"], "read");
    }

    #[test]
    fn the_report_with_history_has_every_rows_buckets() {
        let mut s = Session::new();
        s.set_memory_stores(true);
        s.load_sample("modern").unwrap();
        let answer = s.execute("g.v().count().to_list()", &crate::RunOptions::default());
        assert!(answer.contains('6'), "{answer}");
        // The minute bucket of the run: the data of a chart.
        let report = s.workload_report_with_history("minutes").unwrap();
        assert_eq!(report["history_tier"], "minutes");
        let history = report["queries"][0]["history"].as_array().unwrap();
        assert_eq!(history.len(), 1, "{report}");
        assert_eq!(history[0]["count"], 1);
        assert!(history[0]["start"].as_i64().unwrap() > 0);
        assert!(
            s.workload_report().unwrap()["queries"][0]
                .get("history")
                .is_none()
        );
        assert!(
            s.workload_report_with_history("weeks")
                .unwrap_err()
                .to_string()
                .contains("minutes")
        );
    }

    #[test]
    fn a_job_of_the_catalog_is_listed_and_runs_on_its_schedule() {
        let mut s = Session::new();
        s.set_memory_stores(true);
        s.load_sample("modern").unwrap();
        let answer = s.execute(
            r#"g.define_job(#{name: "cp", schedule: "* * * * *", action: "checkpoint"})"#,
            &crate::RunOptions::default(),
        );
        assert!(
            answer.contains("\"ok\": true") || answer.contains("\"ok\":true"),
            "{answer}"
        );
        let view = s.jobs_view().unwrap();
        let cp = view["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|j| j["name"] == "cp")
            .cloned()
            .unwrap_or_default();
        assert_eq!(cp["kind"], "user", "{view}");
        s.tick_jobs(T0).unwrap();
        assert!(
            s.tick_jobs(T0 + MINUTE)
                .unwrap()
                .contains(&"cp".to_string())
        );
        // Dropped: gone from the scheduler at the next look.
        s.execute(r#"g.drop_job("cp")"#, &crate::RunOptions::default());
        let view = s.jobs_view().unwrap();
        assert!(
            !view["jobs"]
                .as_array()
                .unwrap()
                .iter()
                .any(|j| j["name"] == "cp"),
            "{view}"
        );
    }

    #[test]
    fn the_statistics_memory_is_in_the_sessions_statistics() {
        let mut s = Session::new();
        assert_eq!(s.statistics().unwrap()["memory"]["workload_bytes"], 0);
        s.set_memory_stores(true);
        s.load_sample("modern").unwrap();
        let bytes = s.statistics().unwrap()["memory"]["workload_bytes"]
            .as_u64()
            .unwrap();
        assert!(bytes > 0, "{bytes}");
    }

    fn store_session() -> Session {
        let mut s = Session::new();
        s.set_memory_stores(true);
        s.load_sample("modern").unwrap();
        let answer = s.execute(
            r#"g.define_query(#{name: "purge", params: #{label: "string"},
                body: "fn purge(label) { g.v().has_label(label).drop() }"})"#,
            &crate::RunOptions::default(),
        );
        assert!(answer.contains("true"), "{answer}");
        s
    }

    fn fields(answer: &Value) -> Vec<String> {
        answer["errors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["field"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn a_job_is_defined_listed_switched_run_and_dropped() {
        let mut s = store_session();
        let answer = s.define_job(&json!({
            "name": "nightly_purge", "schedule": "0 3 * * *", "action": "query",
            "query": "purge", "args": {"label": "software"}, "timeout_ms": 30000,
            "description": "drops the software"
        }));
        assert_eq!(answer["ok"], true, "{answer}");
        let job = &answer["job"];
        assert_eq!(job["kind"], "user");
        assert_eq!(job["query"], "purge");
        assert_eq!(job["args"]["label"], "software");
        assert_eq!(job["timeoutMs"], 30000);
        assert_eq!(job["scheduled"], true);
        assert!(job["reason"].is_null());
        let effects: Vec<&str> = job["effects"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["action"].as_str().unwrap())
            .collect();
        assert_eq!(effects, ["Execute", "Delete"]);
        // The list: system jobs first, then this one.
        let view = s.jobs_view().unwrap();
        assert_eq!(view["store"], true);
        let names: Vec<&str> = view["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|j| j["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["stats_flush", "compact", "nightly_purge"]);
        // Switched off: the catalog definition changes, its effects stay.
        let off = s.set_job_enabled("nightly_purge", false).unwrap();
        assert_eq!(off["enabled"], false);
        assert_eq!(off["effects"].as_array().unwrap().len(), 2);
        assert!(off["nextAt"].is_null());
        // Run now (also when switched off): the software is gone, the run recorded.
        let ran = s.run_job_now("nightly_purge").unwrap();
        assert!(ran["last"]["error"].is_null(), "{ran}");
        assert_eq!(ran["runs"]["last_hour"]["count"], 1, "{ran}");
        let count = s.execute(
            r#"g.v().has_label("software").count().next()"#,
            &crate::RunOptions::default(),
        );
        assert!(count.contains("\"0\"") || count.contains(": 0"), "{count}");
        let history = s.job_history("nightly_purge", "minutes").unwrap();
        assert_eq!(history["buckets"].as_array().unwrap().len(), 1, "{history}");
        assert!(
            s.run_job_now("nowhere")
                .unwrap_err()
                .to_string()
                .contains("nowhere")
        );
        assert!(s.drop_job("nightly_purge").unwrap());
        assert!(!s.drop_job("nightly_purge").unwrap());
        assert!(s.job_entry("nightly_purge").unwrap().is_null());
    }

    #[test]
    fn a_wrong_spec_names_every_wrong_field() {
        let mut s = store_session();
        let answer = s.define_job(&json!({
            "name": "1bad", "schedule": "61 * * * *", "action": "query", "query": "nowhere",
            "timeout_ms": -1
        }));
        assert_eq!(answer["ok"], false);
        assert_eq!(
            fields(&answer),
            ["name", "schedule", "query", "timeout_ms"],
            "{answer}"
        );
        let answer =
            s.define_job(&json!({"name": "b", "schedule": "* * * * *", "action": "backup"}));
        assert_eq!(fields(&answer), ["path"]);
        let answer =
            s.define_job(&json!({"name": "c", "schedule": "* * * * *", "action": "compact"}));
        assert_eq!(fields(&answer), ["action"]);
        let answer = s.define_job(&json!({"name": "c", "every": 1}));
        assert_eq!(fields(&answer), ["general"]);
        // The engine's own checks: an unknown argument of the query.
        let answer = s.define_job(&json!({
            "name": "d", "schedule": "* * * * *", "action": "query", "query": "purge",
            "args": {"lbl": "x"}
        }));
        assert_eq!(fields(&answer), ["args"], "{answer}");
        // The playground may not write files, so it may not schedule backups.
        let answer = s.define_job(&json!({
            "name": "b", "schedule": "* * * * *", "action": "backup", "path": "/tmp/backups"
        }));
        assert_eq!(fields(&answer), ["path"], "{answer}");
        assert_eq!(s.jobs_view().unwrap()["jobs"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn a_system_jobs_schedule_and_switch_are_kept_by_the_store() {
        let mut s = store_session();
        let entry = s.set_job_enabled("compact", true).unwrap();
        assert_eq!(entry["enabled"], true);
        assert_eq!(entry["setting"]["enabled"], true);
        let entry = s.set_job_schedule("compact", Some("30 4 * * *")).unwrap();
        assert_eq!(entry["schedule"], "30 4 * * *");
        assert!(s.set_job_schedule("compact", Some("99 * * * *")).is_err());
        assert!(
            s.set_job_schedule("nowhere", None)
                .unwrap_err()
                .to_string()
                .contains("nowhere")
        );
        // The store hands its system jobs out with the setting applied.
        let store = s.active_store().unwrap();
        let compact = store
            .system_jobs()
            .into_iter()
            .find(|job| job.name() == "compact")
            .unwrap();
        assert!(compact.enabled());
        assert_eq!(compact.schedule().to_string(), "30 4 * * *");
        // Back to the built-in schedule.
        let entry = s.set_job_schedule("compact", None).unwrap();
        assert_eq!(entry["schedule"], "0 3 * * *");
        assert_eq!(entry["setting"]["schedule"], Value::Null);
        // A job of a user changes its schedule by being defined again.
        s.define_job(&json!({"name": "cp", "schedule": "* * * * *", "action": "checkpoint"}));
        assert!(
            s.set_job_schedule("cp", Some("0 * * * *"))
                .unwrap_err()
                .to_string()
                .contains("define")
        );
    }
}
