//! Scheduled jobs of users in the DSL (book page "Scheduled Jobs"): plain calls on the source,
//! not steps, snake_case only (Graphersal extensions): `g.define_job(#{..})`, `g.jobs()`,
//! `g.drop_job(name)`.
//!
//! Defining a job checks the definer's rights for everything the job will do and stores those
//! effects with it (the rights model of the jobs: a run may do exactly them): `Execute` on the
//! saved query a query job calls and every data write the query may make, `Create` on the
//! directory a backup job writes.

use super::saved_query::{fail, graph_fail, optional_text};
use super::*;
use crate::error::ScriptError;
use crate::graph::definition::{Definition, DefinitionKind};
use crate::graph::jobs::{JobDefinition, JobLimits, JobTask, RunOn};
use crate::graph::{AccessRequest, AccessRequestBuf, Action, Resource};

/// The keys `define_job` accepts.
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

/// One effect as the map `g.jobs()` shows: `{action, resource, element?, label?, path?, kind?,
/// name?}`.
fn effect_entry(effect: &AccessRequestBuf) -> Dynamic {
    let request = effect.as_request();
    let mut map = rhai::Map::new();
    map.insert(
        "action".into(),
        request.action.kind().name().to_string().into(),
    );
    match request.resource {
        Resource::Data { element, label } => {
            map.insert("resource".into(), "data".to_string().into());
            map.insert(
                "element".into(),
                element.map_or(Dynamic::UNIT, |e| e.as_str().to_string().into()),
            );
            map.insert(
                "label".into(),
                label.map_or(Dynamic::UNIT, |l| l.to_string().into()),
            );
        }
        Resource::File { path } => {
            map.insert("resource".into(), "file".to_string().into());
            map.insert("path".into(), path.to_string_lossy().into_owned().into());
        }
        Resource::Definition { kind, name } => {
            map.insert("resource".into(), "definition".to_string().into());
            map.insert(
                "kind".into(),
                kind.map_or(Dynamic::UNIT, |k| k.to_string().into()),
            );
            map.insert(
                "name".into(),
                name.map_or(Dynamic::UNIT, |n| n.to_string().into()),
            );
        }
        _ => {
            map.insert("resource".into(), "other".to_string().into());
        }
    }
    Dynamic::from_map(map)
}

/// One job as a map: name, schedule, action, query/args (a query job), path (a backup job),
/// enabled, timeout_ms, memory_limit (`()` when unset), run_on, description, effects, served
/// (with `reason` when this version does not run it).
fn job_entry(definition: &Definition) -> Option<rhai::Map> {
    let job = definition.as_job()?;
    let mut map = rhai::Map::new();
    let text = |value: &str| Dynamic::from(value.to_string());
    map.insert("name".into(), text(definition.name()));
    map.insert("schedule".into(), text(job.schedule()));
    map.insert("action".into(), text(job.action().name()));
    match job.action() {
        JobTask::Query { name, args } => {
            map.insert("query".into(), text(name));
            let mut out = rhai::Map::new();
            for (key, value) in args {
                out.insert(
                    key.as_str().into(),
                    element_property_to_dynamic(value.clone()),
                );
            }
            map.insert("args".into(), Dynamic::from_map(out));
        }
        JobTask::Backup { path } => {
            map.insert("path".into(), text(path));
        }
        _ => {}
    }
    map.insert("enabled".into(), Dynamic::from_bool(job.enabled()));
    let int = |n: u64| Dynamic::from_int(i64::try_from(n).unwrap_or(i64::MAX));
    let limits = job.limits();
    map.insert(
        "timeout_ms".into(),
        limits
            .timeout
            .map_or(Dynamic::UNIT, |t| int(t.as_millis() as u64)),
    );
    map.insert(
        "memory_limit".into(),
        limits.memory_limit.map_or(Dynamic::UNIT, int),
    );
    map.insert("run_on".into(), text(job.run_on().as_str()));
    map.insert("description".into(), text(job.description()));
    map.insert(
        "effects".into(),
        Dynamic::from_array(job.effects().iter().map(effect_entry).collect()),
    );
    let reason = job.unserved_reason();
    map.insert("served".into(), Dynamic::from_bool(reason.is_none()));
    if let Some(reason) = reason {
        map.insert("reason".into(), reason.into());
    }
    Some(map)
}

impl ScriptGraphTraversalSource {
    /// Asks the session's authorizer for `action` on the job `name` (`None`: every job).
    fn require_job(
        &self,
        operation: &str,
        action: Action<'_>,
        name: Option<&str>,
    ) -> Result<(), Box<EvalAltResult>> {
        self.require(
            operation,
            AccessRequest::new(
                action,
                Resource::definition(Some(DefinitionKind::JOB), name),
            ),
        )
    }

    fn job_graph(&self, operation: &str) -> Result<&Arc<dyn AnyGraph>, Box<EvalAltResult>> {
        self.g.as_ref().ok_or_else(|| {
            Box::new(EvalAltResult::ErrorRuntime(
                Dynamic::from(format!("{operation}: no graph associated with this source")),
                rhai::Position::NONE,
            ))
        })
    }

    /// `g.define_job(#{name, schedule, action, query?, args?, path?, enabled?, timeout_ms?,
    /// memory_limit?, run_on?, description?})`: defines (or replaces) a scheduled job: on
    /// `schedule` (5-field cron, the instance's local time zone) it runs `action`: `"query"`
    /// (calls the saved query `query` with `args`), `"checkpoint"` (writes a checkpoint of the
    /// store) or `"backup"` (backs the store up into the directory `path`). The definer's rights
    /// are checked now for everything the job does (`Execute` on the query and every data write
    /// it may make, `Create` on the backup directory) and stored with it: a run may do exactly
    /// that. Asks for `Create` on the job (`Update` when it replaces one).
    pub fn define_job(
        ctx: NativeCallContext,
        this: &mut Self,
        spec: Dynamic,
    ) -> Result<(), Box<EvalAltResult>> {
        let operation = "define_job";
        let actual = type_name_of(&spec);
        let mismatch = |expected: String, actual: String| {
            fail(ScriptError::ArgumentMismatch {
                step_name: operation.to_string(),
                expected,
                actual,
            })
        };
        let Some(mut spec) = spec.try_cast::<rhai::Map>() else {
            return Err(mismatch(
                "a map #{name, schedule, action, query?, args?, path?, ...}".into(),
                actual,
            ));
        };
        if let Some(unknown) = spec.keys().find(|key| !KEYS.contains(&key.as_str())) {
            return Err(mismatch(
                format!("only the keys {}", KEYS.join(", ")),
                format!("the key '{unknown}'"),
            ));
        }
        let mut take = |key: &str| spec.remove(key).unwrap_or(Dynamic::UNIT);
        let required = |key: &str, what: &str, value: Option<String>| {
            value.ok_or_else(|| mismatch(format!("the key {key} ({what})"), format!("no {key}")))
        };
        let name = required(
            "name",
            "the job's name",
            optional_text(operation, "name", take("name"))?,
        )?;
        let schedule = required(
            "schedule",
            "a 5-field cron schedule such as \"0 3 * * *\"",
            optional_text(operation, "schedule", take("schedule"))?,
        )?;
        let action = required(
            "action",
            "\"query\", \"checkpoint\" or \"backup\"",
            optional_text(operation, "action", take("action"))?,
        )?;
        let query = optional_text(operation, "query", take("query"))?;
        let args = take("args");
        let path = optional_text(operation, "path", take("path"))?;
        let flag = |value: Dynamic, key: &str| -> Result<Option<bool>, Box<EvalAltResult>> {
            if value.is_unit() {
                return Ok(None);
            }
            value
                .as_bool()
                .map(Some)
                .map_err(|actual| mismatch(format!("{key} true or false"), actual.to_string()))
        };
        let number = |value: Dynamic, key: &str| -> Result<Option<u64>, Box<EvalAltResult>> {
            if value.is_unit() {
                return Ok(None);
            }
            value
                .as_int()
                .ok()
                .and_then(|n| u64::try_from(n).ok())
                .map(Some)
                .ok_or_else(|| mismatch(format!("{key} a positive integer"), type_name_of(&value)))
        };
        let enabled = flag(take("enabled"), "enabled")?.unwrap_or(true);
        let timeout_ms = number(take("timeout_ms"), "timeout_ms")?;
        let memory_limit = number(take("memory_limit"), "memory_limit")?;
        let run_on = match optional_text(operation, "run_on", take("run_on"))?.as_deref() {
            None | Some("any") => RunOn::Any,
            Some("primary") => RunOn::Primary,
            Some("replica") => RunOn::Replica,
            Some(other) => {
                return Err(mismatch(
                    "run_on \"any\", \"primary\" or \"replica\"".into(),
                    format!("\"{other}\""),
                ));
            }
        };
        let description =
            optional_text(operation, "description", take("description"))?.unwrap_or_default();

        let graph = this.job_graph(operation)?;
        let exists = graph
            .definition(DefinitionKind::JOB, &name)
            .map_err(graph_fail)?
            .is_some();
        this.require_job(
            operation,
            if exists {
                Action::Update
            } else {
                Action::Create
            },
            Some(&name),
        )?;

        // What the job does, and the definer's rights for all of it.
        let mut effects: Vec<AccessRequestBuf> = Vec::new();
        let task = match action.as_str() {
            "query" => {
                let query_name = required("query", "the saved query a query job calls", query)?;
                let execute = AccessRequest::new(
                    Action::Execute,
                    Resource::definition(Some(DefinitionKind::QUERY), Some(&query_name)),
                );
                this.require(operation, execute)?;
                effects.push(execute.to_owned_request());
                let definition = graph
                    .definition(DefinitionKind::QUERY, &query_name)
                    .map_err(graph_fail)?
                    .ok_or_else(|| {
                        graph_fail(crate::error::GraphError::from(
                            crate::error::DefinitionError::NotFound {
                                kind: DefinitionKind::QUERY.to_string(),
                                name: query_name.clone(),
                            },
                        ))
                    })?;
                let invalid = |reason: String| {
                    fail(ScriptError::InvalidQueryBody {
                        query: query_name.clone(),
                        reason,
                    })
                };
                let stored = definition
                    .as_query()
                    .ok_or_else(|| {
                        invalid("it is kept opaque (this version could not decode it)".into())
                    })?
                    .clone();
                let catalog = super::saved_query::StoredQueries::new(&**graph);
                let writes = super::saved_query_effects(
                    ctx.engine(),
                    &|name| catalog.lookup(name),
                    &query_name,
                    &stored,
                );
                catalog.finish()?;
                let writes = writes.map_err(invalid)?;
                for effect in writes.effects {
                    this.require(operation, effect.as_request())?;
                    effects.push(effect);
                }
                // The arguments: known parameters with values their schemas accept.
                let mut checked = crate::value::PropertyMap::default();
                if !args.is_unit() {
                    let actual = type_name_of(&args);
                    let Some(args) = args.try_cast::<rhai::Map>() else {
                        return Err(mismatch("args a map of parameter values".into(), actual));
                    };
                    for (key, value) in args {
                        let Some(param) = stored.param(key.as_str()) else {
                            return Err(fail(ScriptError::UnknownQueryArgument {
                                query: query_name.clone(),
                                argument: key.to_string(),
                                parameters: stored
                                    .params()
                                    .iter()
                                    .map(|(p, _)| p.clone())
                                    .collect(),
                            }));
                        };
                        let value = super::dynamic::value_arg(&ctx, value)?;
                        let value = param
                            .check_value(&query_name, key.as_str(), value)
                            .map_err(|err| {
                                fail(ScriptError::InvalidQueryArgument {
                                    query: query_name.clone(),
                                    parameter: key.to_string(),
                                    reason: err.to_string(),
                                })
                            })?;
                        checked.insert(key.as_str(), value);
                    }
                }
                JobTask::Query {
                    name: query_name,
                    args: checked,
                }
            }
            "checkpoint" => JobTask::Checkpoint,
            "backup" => {
                let path = required("path", "the directory the backups go to", path)?;
                let resolved = this.require_file(operation, Action::Create, &path)?;
                effects.push(
                    AccessRequest::new(Action::Create, Resource::File { path: &resolved })
                        .to_owned_request(),
                );
                JobTask::Backup { path }
            }
            other => {
                return Err(mismatch(
                    "action \"query\", \"checkpoint\" or \"backup\"".into(),
                    format!("\"{other}\""),
                ));
            }
        };
        let mut limits = JobLimits::new();
        if let Some(ms) = timeout_ms {
            limits = limits.with_timeout(std::time::Duration::from_millis(ms));
        }
        if let Some(bytes) = memory_limit {
            limits = limits.with_memory_limit(bytes);
        }
        let job = JobDefinition::new(schedule, task)
            .with_enabled(enabled)
            .with_limits(limits)
            .with_run_on(run_on)
            .with_effects(effects)
            .with_description(description);
        let definition = Definition::job(name.as_str(), job);
        definition
            .validate()
            .map_err(|err| graph_fail(crate::error::GraphError::from(err)))?;
        graph
            .set_definitions(vec![definition])
            .map_err(script_error)
    }

    /// `g.jobs()`: every scheduled job of a user as a map (name, schedule, action, query/args or
    /// path, enabled, timeout_ms, memory_limit, run_on, description, effects, served, `reason`
    /// when this version does not run it), by name. Asks for `Read` on the jobs.
    pub fn jobs(&mut self) -> Result<rhai::Array, Box<EvalAltResult>> {
        self.require_job("jobs", Action::Read, None)?;
        Ok(self
            .job_graph("jobs")?
            .definitions(Some(DefinitionKind::JOB))
            .map_err(graph_fail)?
            .iter()
            .filter_map(job_entry)
            .map(Dynamic::from_map)
            .collect())
    }

    /// `g.drop_job(name)`: removes the job; `false` when there is none. Asks for `Delete` on
    /// it.
    pub fn drop_job(&mut self, name: &str) -> Result<bool, Box<EvalAltResult>> {
        self.require_job("drop_job", Action::Delete, Some(name))?;
        self.job_graph("drop_job")?
            .remove_definition(DefinitionKind::JOB, name)
            .map_err(script_error)
    }
}

/// One run of a `query` job (`Store::user_jobs`): calls the saved query `query` with `args` as
/// one unit carrying `{job: <job>}` in its commit metadata, under an authorizer limited to the
/// job's stored `effects`; a traversal the query returns runs inside the unit.
pub(crate) fn run_query_job(
    graph: Arc<dyn AnyGraph>,
    job: &str,
    query: &str,
    args: &crate::value::PropertyMap,
    effects: Vec<AccessRequestBuf>,
) -> Result<(), String> {
    let mut params = std::collections::HashMap::new();
    params.insert("job_query".to_string(), ElementProperty::from(query));
    params.insert(
        "job_args".to_string(),
        ElementProperty::Object(Box::new(args.clone())),
    );
    let authorizer = Arc::new(crate::graph::jobs::JobAuthorizer::new(job, effects));
    let metadata = crate::graph::CommitMetadata::new().with_attribute("job", job);
    super::eval::eval_job_unit(
        graph,
        "g.query(job_query, job_args)",
        params,
        &ScriptLimits::default(),
        authorizer,
        metadata,
    )
    .map_err(|err| err.to_string())
}
