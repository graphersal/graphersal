//! The scheduled jobs of the session's active store (book page "Scheduled Jobs"): the host
//! ticks [`Session::tick_jobs`] (the dev server from a thread every few seconds, the playground
//! from a timer in its worker); the session keeps one [`Scheduler`] for the active store and
//! builds a new one when the active store changes. Without a store there are no jobs.

use std::sync::{Arc, Weak};

use graphersal::jobs::Scheduler;
use serde_json::{Value, json};

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
}

impl Session {
    /// Sets the time zone the scheduled jobs run in (the instance's local zone, which the host
    /// knows: `TZ` or `/etc/localtime` natively, the browser's zone in the playground).
    pub fn set_time_zone(&mut self, name: &str) -> Result<(), String> {
        Scheduler::new()
            .with_time_zone(name)
            .map_err(|err| err.to_string())?;
        self.jobs.time_zone = Some(name.to_string());
        // Rebuilt in the new zone at the next tick.
        self.jobs.current = None;
        Ok(())
    }

    /// The scheduler of the active store, built when the active store changed.
    fn scheduler(&mut self) -> Option<&mut Scheduler> {
        let Some(store) = self.active_store() else {
            self.jobs.current = None;
            return None;
        };
        let same = self
            .jobs
            .current
            .as_ref()
            .is_some_and(|(known, _)| std::ptr::addr_eq(known.as_ptr(), Arc::as_ptr(&store)));
        if !same {
            // The zone was checked when it was set; an unknown one falls back to UTC.
            let zoned = match &self.jobs.time_zone {
                Some(zone) => Scheduler::new().with_time_zone(zone).ok(),
                None => None,
            };
            let mut scheduler = zoned.unwrap_or_default().with_stats(store.workload_stats());
            for job in store.system_jobs() {
                let _ = scheduler.add(job);
            }
            self.jobs.current = Some((Arc::downgrade(&store), scheduler));
            self.jobs.user_jobs.clear();
        }
        // The user jobs follow the catalog: rebuilt when its job definitions changed (a define,
        // a drop, a rollback or a catalog load).
        let definitions = self
            .graph()
            .definitions(Some(graphersal::catalog::DefinitionKind::JOB));
        let (_, scheduler) = self.jobs.current.as_mut()?;
        if definitions != self.jobs.user_jobs {
            scheduler.sync_user_jobs(store.user_jobs());
            self.jobs.user_jobs = definitions;
        }
        self.jobs.current.as_mut().map(|(_, scheduler)| scheduler)
    }

    /// Runs the due jobs of the active store at `now` (microseconds since the Unix epoch);
    /// returns the names of the jobs that ran.
    pub fn tick_jobs(&mut self, now: i64) -> Vec<String> {
        self.scheduler()
            .map_or_else(Vec::new, |scheduler| scheduler.tick(now))
    }

    /// The jobs of the active store: `{timeZone, jobs: [{name, kind, schedule, enabled,
    /// nextAt, last: {startedAt, micros, error} | null}]}` (times in microseconds since the
    /// Unix epoch); `jobs` is empty without a store.
    pub fn jobs_view(&mut self) -> Value {
        let zone = self
            .jobs
            .time_zone
            .clone()
            .unwrap_or_else(|| "UTC".to_string());
        let jobs: Vec<Value> = self
            .scheduler()
            .map(|scheduler| {
                scheduler
                    .jobs()
                    .into_iter()
                    .map(|job| {
                        json!({
                            "name": job.name,
                            "kind": job.kind.name(),
                            "schedule": job.schedule,
                            "enabled": job.enabled,
                            "nextAt": job.next_at,
                            "last": job.last.map(|run| json!({
                                "startedAt": run.started_at,
                                "micros": run.micros,
                                "error": run.error,
                            })),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        json!({ "timeZone": zone, "jobs": jobs })
    }
}

impl Session {
    /// The workload statistics of the served graph
    /// ([`graphersal::workload::WorkloadReport::to_property`] as JSON: `{level, queries,
    /// properties, locks, jobs}`); a graph without statistics (not in a store) answers level
    /// `off` with empty lists. Reading it is not counted.
    pub fn workload_report(&self) -> Value {
        self.workload_report_in(None)
    }

    /// [`Self::workload_report`] with every row's `history` of the tier `tier` (`minutes`,
    /// `hours` or `days`: its buckets, oldest first, the data of a chart) and `history_tier`;
    /// `Err` names the tiers for another `tier`.
    pub fn workload_report_with_history(&self, tier: &str) -> Result<Value, String> {
        let tier = graphersal::workload::HistoryTier::from_name(tier)
            .ok_or_else(|| format!("unknown history tier '{tier}': use minutes, hours or days"))?;
        Ok(self.workload_report_in(Some(tier)))
    }

    fn workload_report_in(&self, history: Option<graphersal::workload::HistoryTier>) -> Value {
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
        serde_json::from_str(&report.to_json()).unwrap_or(Value::Null)
    }
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
        assert_eq!(s.jobs_view()["jobs"], json!([]));
        assert!(s.tick_jobs(T0).is_empty());
        s.set_memory_stores(true);
        s.load_sample("modern").unwrap();
        s.set_time_zone("Europe/Berlin").unwrap();
        assert!(s.set_time_zone("Nowhere/Land").is_err());
        let view = s.jobs_view();
        assert_eq!(view["timeZone"], "Europe/Berlin");
        assert_eq!(view["jobs"][0]["name"], "stats_flush");
        assert_eq!(view["jobs"][0]["kind"], "system");
        assert_eq!(view["jobs"][1]["enabled"], false);
        s.tick_jobs(T0);
        assert_eq!(s.tick_jobs(T0 + MINUTE), ["stats_flush"]);
        let view = s.jobs_view();
        assert!(view["jobs"][0]["last"]["error"].is_null(), "{view}");
        // Another active store gets its own scheduler (planned anew).
        s.load_sample("modern").unwrap();
        assert!(s.jobs_view()["jobs"][0]["last"].is_null());
    }

    #[test]
    fn only_the_users_queries_count_under_the_hosts_channel() {
        use graphersal::workload::{Channel, scope};
        let mut s = Session::new();
        assert_eq!(s.workload_report()["level"], "off");
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
        let report = s.workload_report();
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
        assert!(s.workload_report()["queries"][0].get("history").is_none());
        assert!(
            s.workload_report_with_history("weeks")
                .unwrap_err()
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
        let view = s.jobs_view();
        let cp = view["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|j| j["name"] == "cp")
            .cloned()
            .unwrap_or_default();
        assert_eq!(cp["kind"], "user", "{view}");
        s.tick_jobs(T0);
        assert!(s.tick_jobs(T0 + MINUTE).contains(&"cp".to_string()));
        // Dropped: gone from the scheduler at the next look.
        s.execute(r#"g.drop_job("cp")"#, &crate::RunOptions::default());
        let view = s.jobs_view();
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
        assert_eq!(s.statistics()["memory"]["workload_bytes"], 0);
        s.set_memory_stores(true);
        s.load_sample("modern").unwrap();
        let bytes = s.statistics()["memory"]["workload_bytes"].as_u64().unwrap();
        assert!(bytes > 0, "{bytes}");
    }
}
