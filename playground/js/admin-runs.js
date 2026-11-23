// The run administration dialog's model (pure): the rows of `GET /api/admin/runs` (every queued
// or running run of the served graph, whatever its client), shown only when `/api/info` names
// `adminRuns` (the host turned it on: `graphersal --server --admin-runs`, graphersal-serve
// `ContextBuilder::admin_runs`). Issue #32.

/** How often the open dialog reloads the list, in milliseconds. */
export const ADMIN_RUNS_REFRESH_MS = 2000;

/**
 * The run administration as `/api/info` announces it, or null when the host did not turn it on.
 * @param {Object|null} info  The raw `/api/info` answer.
 * @returns {null|{tokenRequired: boolean}}
 */
export function adminRunsOf(info) {
  const admin = info && info.adminRuns;
  if (!admin || typeof admin !== 'object' || Array.isArray(admin)) return null;
  return { tokenRequired: admin.tokenRequired === true };
}

/** Milliseconds as a short duration: `850 ms`, `12.4 s`, `3 min 5 s`, `2 h 1 min`. */
export function durationText(ms) {
  const n = Math.max(0, Number(ms) || 0);
  if (n < 1000) return `${Math.round(n)} ms`;
  const s = n / 1000;
  if (s < 60) return `${s.toFixed(1)} s`;
  const whole = Math.floor(s);
  const h = Math.floor(whole / 3600);
  const m = Math.floor((whole % 3600) / 60);
  if (h) return `${h} h ${m} min`;
  return `${m} min ${whole % 60} s`;
}

/** A client key as words: `page:abc` -> `page abc`, `mcp:s1` -> `MCP s1`, `ai:c` -> `AI Chat c`. */
export function clientText(client) {
  const text = String(client || '');
  const [kind, ...rest] = text.split(':');
  const id = rest.join(':');
  const name = { page: 'page', mcp: 'MCP', ai: 'AI Chat' }[kind];
  if (!name) return text;
  return id ? `${name} ${id}` : name;
}

/**
 * The dialog's rows from a list answer, or null without one.
 * @param {Object|null} answer  `GET /api/admin/runs`: `{runs: [...], textHeadChars}`.
 * @param {string} [ownClient]  This page's `X-Graphersal-Client` id: its own runs are marked.
 * @returns {null|{rows: Array<{id:number, client:string, own:boolean, kind:string, state:string,
 *   cancelled:boolean, started:string, duration:string, text:string, truncated:boolean,
 *   identity:string, mount:string}>, textHeadChars:number}}
 */
export function adminRunsModel(answer, ownClient = '') {
  if (!answer || typeof answer !== 'object' || !Array.isArray(answer.runs)) return null;
  const head = Number(answer.textHeadChars) || 0;
  const rows = answer.runs.map((run) => {
    const text = typeof run.text === 'string' ? run.text : '';
    return {
      id: run.id,
      client: clientText(run.client),
      own: !!ownClient && run.client === `page:${ownClient}`,
      kind: String(run.kind || 'other'),
      state: run.cancelled ? `${run.state || 'queued'}, cancelled` : String(run.state || 'queued'),
      cancelled: run.cancelled === true,
      started: run.startedAtMs ? new Date(run.startedAtMs).toLocaleTimeString() : '',
      duration: durationText(run.durationMs),
      text,
      // The server shows only the first `textHeadChars` characters.
      truncated: head > 0 && [...text].length >= head,
      identity: typeof run.identity === 'string' ? run.identity : '',
      mount: typeof run.mount === 'string' ? run.mount : '',
    };
  });
  return { rows, textHeadChars: head };
}
