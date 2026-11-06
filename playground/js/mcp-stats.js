// The MCP usage panel's model (pure): the rows of `GET /api/mcp/stats` (the dev server's MCP
// endpoint counters, kept in the server's memory only since it started).

/** Milliseconds as text: `0.42 ms`, `12.3 ms`, `1.25 s`. */
export function msText(ms) {
  const n = Number(ms) || 0;
  if (n >= 1000) return `${(n / 1000).toFixed(2)} s`;
  if (n >= 10) return `${n.toFixed(1)} ms`;
  return `${n.toFixed(2)} ms`;
}

/** Seconds as `3 h 12 min`, `4 min 2 s`, `9 s`. */
export function uptimeText(seconds) {
  const s = Math.max(0, Math.floor(Number(seconds) || 0));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  if (h) return `${h} h ${m} min`;
  if (m) return `${m} min ${s % 60} s`;
  return `${s} s`;
}

/** The histogram as `<1 ms: 3 · <10 ms: 5 · ≥10 s: 1` (empty buckets left out). */
export function histogramText(histogram, bucketsMs) {
  const bounds = bucketsMs || [];
  const label = (ms) => (ms >= 1000 ? `${ms / 1000} s` : `${ms} ms`);
  return (histogram || [])
    .map((count, i) => {
      if (!count) return null;
      const name = i < bounds.length ? `<${label(bounds[i])}` : `≥${label(bounds[bounds.length - 1] || 0)}`;
      return `${name}: ${count}`;
    })
    .filter(Boolean)
    .join(' · ');
}

function callRows(list, bucketsMs) {
  return (list || []).map((r) => ({
    name: r.name,
    calls: r.calls,
    errors: r.errors,
    mean: msText(r.meanMs),
    max: msText(r.maxMs),
    chars: r.answerChars,
    histogram: histogramText(r.histogram, bucketsMs),
  }));
}

/**
 * The panel's sections from a stats answer, or null without one.
 * @param {Object|null} stats  `GET /api/mcp/stats`.
 * @returns {null|{summary: Array<{name:string, text:string}>, refusals: Array<{name:string, count:number}>,
 *   tools: Array, savedQueries: Array, clients: Array, methods: Array<{name:string, count:number}>}}
 */
export function mcpStatsModel(stats) {
  if (!stats || typeof stats !== 'object') return null;
  const requests = stats.requests || {};
  const sessions = stats.sessions || {};
  const summary = [
    { name: 'Since', text: `${stats.startedAtMs ? new Date(stats.startedAtMs).toLocaleString() : '?'} (${uptimeText(stats.uptimeS)})` },
    { name: 'Requests', text: `${requests.total || 0}${requests.busy503 ? ` (${requests.busy503} answered 503 busy)` : ''}` },
    { name: 'Sessions', text: `${sessions.initialized || 0} initialized, about ${sessions.activeLast5Min || 0} active in the last 5 min` },
  ];
  if (stats.clientsKept >= stats.clientsMax && stats.clientsMax) {
    summary.push({ name: 'Clients', text: `the ${stats.clientsMax} most recent client names are kept` });
  }
  const refusals = Object.entries(stats.refusals || {})
    .filter(([, n]) => n > 0)
    .map(([name, count]) => ({ name, count }));
  const methods = Object.entries(stats.methods || {})
    .filter(([, n]) => n > 0)
    .map(([name, count]) => ({ name, count }));
  return {
    summary,
    refusals,
    methods,
    tools: callRows(stats.tools, stats.latencyBucketsMs),
    savedQueries: callRows(stats.savedQueries, stats.latencyBucketsMs),
    clients: (stats.clients || []).map((c) => ({
      name: c.version ? `${c.name} ${c.version}` : c.name,
      initialized: c.initialized,
      requests: c.requests,
      toolCalls: c.toolCalls,
    })),
  };
}
