//! The scheduled jobs of the session's active store (book page "Scheduled Jobs"): the host
//! ticks [`Session::tick_jobs`] (the dev server from a thread every few seconds, the playground
//! from a timer in its worker); the session keeps one [`Scheduler`] for the active store and
//! builds a new one when the active store changes. Without a store there are no jobs.

use std::sync::Arc;

use graphersal::jobs::Scheduler;
use serde_json::{Value, json};

use crate::session::Session;
use crate::storage::SessionStore;

/// The session's scheduler and the store it was built for.
#[derive(Default)]
pub struct SessionJobs {
    /// The IANA time zone the schedules run in (`None`: UTC).
    time_zone: Option<String>,
    /// The scheduler and the address of the store it belongs to.
    current: Option<(usize, Scheduler)>,
}

/// The address of a store, to notice that the active store changed.
fn address(store: &Arc<dyn SessionStore>) -> usize {
    Arc::as_ptr(store) as *const () as usize
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
        let key = address(&store);
        if self.jobs.current.as_ref().map(|(k, _)| *k) != Some(key) {
            // The zone was checked when it was set; an unknown one falls back to UTC.
            let zoned = match &self.jobs.time_zone {
                Some(zone) => Scheduler::new().with_time_zone(zone).ok(),
                None => None,
            };
            let mut scheduler = zoned.unwrap_or_default().with_stats(store.workload_stats());
            for job in store.system_jobs() {
                let _ = scheduler.add(job);
            }
            self.jobs.current = Some((key, scheduler));
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
}
