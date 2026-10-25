// The model of the Catalog's Scheduled jobs manager: what the engine's `jobs()` returns
// (crates/graphersal-session `jobs.rs`: `{timeZone, store, jobs: [entry]}`) turned into rows to
// show, the form's draft and its spec, and the schedule presets. Pure functions, no Vue, no DOM
// (tested under Node: tests/jobs.test.mjs). Book page "Scheduled Jobs".

/** The catalog kind of a job of a user (the engine's `DefinitionKind::JOB` name). */
export const JOB_KIND = Object.freeze({
  kind: 'job',
  id: 'jobs',
  title: 'Scheduled jobs',
  hint: 'Jobs that run on a cron schedule in the store: a saved query, a checkpoint or a backup, and the system jobs',
});

/** The actions a job of a user can run. */
export const JOB_ACTIONS = Object.freeze([
  { name: 'query', label: 'Run a saved query', hint: 'Calls a saved query with fixed arguments, as one transaction' },
  { name: 'checkpoint', label: 'Write a checkpoint', hint: 'Writes a checkpoint of the store' },
  { name: 'backup', label: 'Back up the store', hint: 'Backs the store up into a directory (only with the dev server: the browser has no files)' },
]);

/** Schedules to pick from (5-field cron). */
export const SCHEDULE_PRESETS = Object.freeze([
  { cron: '*/5 * * * *', label: 'Every 5 minutes' },
  { cron: '0 * * * *', label: 'Every hour' },
  { cron: '0 3 * * *', label: 'Daily at 03:00' },
  { cron: '0 3 * * 0', label: 'Weekly, Sunday 03:00' },
  { cron: '0 3 1 * *', label: 'Monthly, the 1st at 03:00' },
]);

/**
 * The Catalog dropdown's item for the jobs. With the engine's `jobs()` answer its count is
 * `active/all` over every job, system or not (active: switched on and running); before it is
 * loaded, the catalog's jobs of users from the graph info.
 */
export function jobsMenuItem(info, view) {
  const item = { id: JOB_KIND.id, title: JOB_KIND.title, hint: JOB_KIND.hint };
  const jobs = view && view.jobs;
  if (Array.isArray(jobs)) {
    const active = jobs.filter((j) => j.enabled && j.scheduled !== false).length;
    return { ...item, count: `${active}/${jobs.length}`, hint: `${item.hint} (${active} active of ${jobs.length})` };
  }
  const counts = (info && info.catalog) || {};
  return { ...item, count: Number(counts[JOB_KIND.kind]) || 0 };
}

/** A schedule in words when it is a preset, else the cron text itself. */
export function scheduleText(cron) {
  const preset = SCHEDULE_PRESETS.find((p) => p.cron === cron);
  if (preset) return preset.label;
  if (cron === '* * * * *') return 'Every minute';
  return cron || '';
}

/** A time in microseconds since the Unix epoch, relative to `now`: `in 4 min`, `2 h ago`. */
export function relativeTime(micros, now) {
  if (micros == null) return '—';
  const s = Math.round((micros - now) / 1e6);
  const abs = Math.abs(s);
  let text;
  if (abs < 60) text = `${abs} s`;
  else if (abs < 3600) text = `${Math.round(abs / 60)} min`;
  else if (abs < 86400 * 2) text = `${Math.round(abs / 3600)} h`;
  else text = `${Math.round(abs / 86400)} days`;
  return s >= 0 ? `in ${text}` : `${text} ago`;
}

/** What a job does, in words. */
export function actionText(job) {
  switch (job.action) {
    case 'query': {
      const args = job.args && Object.keys(job.args).length
        ? `(${Object.entries(job.args).map(([k, v]) => `${k}: ${JSON.stringify(v)}`).join(', ')})`
        : '()';
      return `query ${job.query}${args}`;
    }
    case 'checkpoint': return 'checkpoint';
    case 'backup': return `backup into ${job.path}`;
    case undefined: return job.name === 'stats_flush' ? 'writes the workload statistics' : job.name === 'compact' ? 'releases spare memory capacity' : 'system job';
    default: return job.action;
  }
}

/** One row of the jobs table. */
export function jobRow(job, now) {
  const last = job.last || null;
  const runs = job.runs && job.runs.last_two_days;
  return {
    name: job.name,
    system: job.kind === 'system',
    action: actionText(job),
    schedule: job.schedule,
    scheduleText: scheduleText(job.schedule),
    enabled: !!job.enabled,
    next: job.enabled && job.nextAt != null ? relativeTime(job.nextAt, now) : '—',
    last: last ? relativeTime(last.startedAt, now) : 'never',
    lastError: last && last.error ? last.error : null,
    lastMs: last ? Math.round((last.micros || 0) / 1000) : null,
    runs: runs ? runs.count : 0,
    failed: runs ? runs.failed : 0,
    scheduled: job.scheduled !== false,
    reason: job.reason || null,
    changed: !!(job.setting && (job.setting.schedule != null || job.setting.enabled != null)),
    description: job.description || '',
    effects: (job.effects || []).map((e) => e.text),
  };
}

/** The rows of every job: the system jobs first, then the jobs of users (the engine's order). */
export function jobRows(view, now = Date.now() * 1000) {
  return ((view && view.jobs) || []).map((job) => jobRow(job, now));
}

/** A new form draft. */
export function newJobDraft() {
  return {
    name: '', action: 'query', query: '', argsText: '{}', path: '', schedule: '0 3 * * *',
    enabled: true, timeoutText: '', description: '', replaces: null,
  };
}

/** The form draft of an existing job of a user (editing it). */
export function draftFromJob(job) {
  return {
    name: job.name,
    action: job.action,
    query: job.query || '',
    argsText: JSON.stringify(job.args || {}, null, 2),
    path: job.path || '',
    schedule: job.schedule,
    enabled: !!job.enabled,
    timeoutText: job.timeoutMs ? String(job.timeoutMs) : '',
    description: job.description || '',
    replaces: job.name,
  };
}

/**
 * The spec of a draft for `defineJob`, or the local errors (`{errors: [{field, message}]}`)
 * the form shows before asking the engine: the arguments must be a JSON object, the timeout a
 * positive whole number of milliseconds.
 */
export function jobDraftToSpec(draft) {
  const errors = [];
  const spec = {
    name: draft.name.trim(),
    schedule: draft.schedule.trim(),
    action: draft.action,
    enabled: !!draft.enabled,
  };
  if (draft.action === 'query') {
    spec.query = draft.query.trim();
    const text = draft.argsText.trim();
    if (text) {
      try {
        const args = JSON.parse(text);
        if (!args || typeof args !== 'object' || Array.isArray(args)) throw new Error('not an object');
        spec.args = args;
      } catch {
        errors.push({ field: 'args', message: 'The arguments are a JSON object, for example {"label": "event"}.' });
      }
    }
  }
  if (draft.action === 'backup') spec.path = draft.path.trim();
  const timeout = draft.timeoutText.trim();
  if (timeout) {
    if (!/^[1-9][0-9]*$/.test(timeout)) errors.push({ field: 'timeout_ms', message: 'A whole number of milliseconds, for example 30000.' });
    else spec.timeout_ms = Number(timeout);
  }
  if (draft.description.trim()) spec.description = draft.description.trim();
  return errors.length ? { errors } : { spec };
}

/** The fields the form shows; an error of another field is shown as a general one. */
const FORM_FIELDS = new Set(['name', 'action', 'query', 'args', 'path', 'schedule', 'timeout_ms', 'description', 'general']);

/** Field errors as the form shows them: unknown fields become `general`. */
export function formErrors(errors) {
  return (errors || []).map((e) => (FORM_FIELDS.has(e.field) ? e : { ...e, field: 'general', message: `${e.field}: ${e.message}` }));
}

/** The saved queries a query job can call: `[{name, params: [name]}]` by name. */
export function queryChoices(savedQueries) {
  return (savedQueries || [])
    .map((q) => ({ name: q.name, params: (q.params || []).map((p) => p.name) }))
    .sort((a, b) => a.name.localeCompare(b.name));
}

/** An arguments skeleton for a query's parameters: `{"label": null}`. */
export function argsSkeleton(params) {
  const out = {};
  for (const p of params || []) out[p] = null;
  return JSON.stringify(out, null, 2);
}
