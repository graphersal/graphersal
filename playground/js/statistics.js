// The data model of the Statistics tab of the Profile panel: what the engine's `statistics()`
// returns (`{statistics, memory, limits: {presets, defaults}}`, crates/graphersal-session
// `Session::statistics`) turned into rows to show. Pure functions, no Vue, no DOM (tested under
// Node: tests/statistics.test.mjs).
//
// Two sources side by side, on purpose: the graph's data counts (`g.statistics()`, what the
// optimizer reads) and its memory footprint, and the limits a query runs under (the host's
// presets, and the engine's defaults for the rest). Limits are not part of `GraphStatistics`.

/** Option keys whose value is a byte count. */
const BYTE_KEYS = new Set([
  'traversal.max_materialized_bytes',
  'traversal.max_string_bytes',
  'memory.limit',
  'memory.headroom',
]);

/** Script limit keys whose value is a byte count. */
const SCRIPT_BYTE_KEYS = new Set(['max_string_size']);

/** `1536` -> `1.5 KiB`; plain bytes below 1 KiB. */
export function formatBytes(n) {
  if (typeof n !== 'number' || !Number.isFinite(n)) return String(n);
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  let value = n;
  let unit = 0;
  while (Math.abs(value) >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }
  if (unit === 0) return `${n} B`;
  const text = value >= 100 ? value.toFixed(0) : value.toFixed(1).replace(/\.0$/, '');
  return `${text} ${units[unit]}`;
}

/** `10000000` -> `10,000,000`. */
export function formatCount(n) {
  if (typeof n !== 'number' || !Number.isFinite(n)) return String(n);
  return Math.trunc(n).toString().replace(/\B(?=(\d{3})+(?!\d))/g, ',');
}

/** `counted(1, 'vertex', 'vertices')` -> `1 vertex`; `counted(2, ...)` -> `2 vertices`. */
export function counted(n, one, many) {
  return `${formatCount(n)} ${n === 1 ? one : many}`;
}

/** One limit value as text: bytes for byte keys, `ms` for timeouts, counts otherwise. */
export function formatLimit(key, value) {
  if (value === null || value === undefined) return 'none';
  if (typeof value === 'number') {
    if (BYTE_KEYS.has(key) || SCRIPT_BYTE_KEYS.has(key)) return formatBytes(value);
    if (key === 'evaluationTimeout' || key === 'scriptTimeoutMs') return `${formatCount(value)} ms`;
    return formatCount(value);
  }
  return typeof value === 'string' ? value : JSON.stringify(value);
}

/**
 * The rows of the "Process memory" section (`statistics().process`: the loaded embedding models
 * with their sizes, the whole process (`memory`: RSS, live heap, WebAssembly memory) and, on the dev server with --ai, the AI Chat history), or null when there is
 * nothing to show.
 * @param {Object|undefined} process
 * @returns {null|Array<{name:string, text:string}>}
 */
export function processMemoryRows(process) {
  if (!process) return null;
  const rows = [];
  const mem = process.memory;
  if (mem) {
    if (typeof mem.rssBytes === 'number') {
      const peak = typeof mem.peakRssBytes === 'number' ? ` (peak ${formatBytes(mem.peakRssBytes)})` : '';
      rows.push({ name: 'Process memory (RSS)', text: `${formatBytes(mem.rssBytes)}${peak}` });
    }
    if (typeof mem.liveHeapBytes === 'number') rows.push({ name: 'Live heap', text: formatBytes(mem.liveHeapBytes) });
    if (typeof mem.wasmMemoryBytes === 'number') rows.push({ name: 'WebAssembly memory', text: formatBytes(mem.wasmMemoryBytes) });
  }
  for (const m of process.models || []) {
    rows.push({ name: `Model ${m.name}`, text: `${formatBytes(m.bytes)} (${m.dimension} dimensions${m.default ? ', default' : ''})` });
  }
  const chat = process.aiChat;
  if (chat) {
    const n = chat.conversations;
    const raw = chat.historyUncompressedBytes > chat.historyBytes ? `, ${formatBytes(chat.historyUncompressedBytes)} uncompressed` : '';
    rows.push({ name: 'AI Chat history', text: `${formatBytes(chat.historyBytes)} (${n} ${n === 1 ? 'conversation' : 'conversations'}${raw})` });
  }
  return rows.length ? rows : null;
}

/**
 * The rows of the Statistics tab.
 * @param {Object|null} raw  The engine's `statistics()` answer.
 * @returns {{counts: null|{vertices:number, edges:number, vertexLabels:Array, edgeLabels:Array,
 *   vertexLabelTotal:number, edgeLabelTotal:number, truncated:boolean},
 *   memory: null|Array<{name:string, text:string, hint?:string}>,
 *   process: null|Array<{name:string, text:string}>,
 *   limits: Array<{key:string, text:string, source:'preset'|'default'|'script'}>,
 *   noPresets: boolean}}
 */
export function statisticsModel(raw) {
  const r = raw || {};
  const s = r.statistics;
  const counts = s
    ? {
        vertices: s.vertex_count,
        edges: s.edge_count,
        vertexLabels: (s.vertex_labels || []).map((l) => ({
          label: l.label,
          count: l.count,
          primary: l.primary,
        })),
        edgeLabels: (s.edge_labels || []).map((l) => ({ label: l.label, count: l.count })),
        vertexLabelTotal: s.vertex_label_total ?? (s.vertex_labels || []).length,
        edgeLabelTotal: s.edge_label_total ?? (s.edge_labels || []).length,
        truncated: !!s.truncated,
      }
    : null;
  const m = r.memory;
  const memory = m
    ? [
        { name: 'Structure', text: formatBytes(m.structure_bytes) },
        { name: 'Indexes', text: formatBytes(m.index_bytes) },
        { name: 'Properties', text: formatBytes(m.property_bytes) },
        // The shared key lists ("shapes"): part of Properties, never added to the total again.
        ...(m.shape_bytes
          ? [
              {
                name: 'of which key lists (shapes)',
                text: formatBytes(m.shape_bytes),
                hint: 'Every distinct property key sequence is stored once per graph and shared by the elements with exactly these keys in this order; each element keeps only its values. Part of Properties.',
              },
            ]
          : []),
        // Compression rules (Catalog ▸ Compression): only when something is compressed.
        ...(m.compressed_values || m.compression_dictionary_bytes
          ? [
              {
                name: 'Compressed strings',
                text: `${m.compressed_values} (${formatBytes(m.compressed_plain_bytes)} → ${formatBytes(m.compressed_stored_bytes)})`,
              },
              { name: 'Compression dictionaries', text: formatBytes(m.compression_dictionary_bytes) },
            ]
          : []),
        // Dedup rules: the shared values and their tables (part of Properties).
        ...(m.deduped_values
          ? [
              {
                name: 'Shared strings (dedup)',
                text: `${m.deduped_values} (${formatBytes(m.deduped_plain_bytes)} → ${formatBytes(m.dedup_table_bytes)})`,
              },
            ]
          : []),
        // Property indexes (Catalog ▸ Property indexes): only when the graph has some.
        ...(m.property_index_bytes ? [{ name: 'Property indexes', text: formatBytes(m.property_index_bytes) }] : []),
        { name: 'Total', text: formatBytes(m.total_bytes) },
        // The workload statistics of a graph in a store: held beside the graph, not in Total.
        ...(m.workload_bytes
          ? [
              {
                name: 'Workload statistics',
                text: formatBytes(m.workload_bytes),
                hint: 'The counts of the queries, property uses, lock times and jobs of this store (minutes, hours, days), kept in memory beside the graph and written to the store\'s WORKLOAD file. Not part of Total.',
              },
            ]
          : []),
      ]
    : null;
  const processRows = processMemoryRows(r.process);
  const presets = (r.limits && r.limits.presets) || {};
  const defaults = (r.limits && r.limits.defaults) || {};
  const limits = [];
  for (const [key, value] of Object.entries(presets.options || {})) {
    limits.push({ key, text: formatLimit(key, value), source: 'preset' });
  }
  if (presets.scriptTimeoutMs !== null && presets.scriptTimeoutMs !== undefined) {
    limits.push({ key: 'script timeout', text: formatLimit('scriptTimeoutMs', presets.scriptTimeoutMs), source: 'preset' });
  }
  for (const [key, value] of Object.entries(presets.scriptLimits || {})) {
    limits.push({ key: `script.${key}`, text: formatLimit(key, value), source: 'script' });
  }
  for (const [key, value] of Object.entries(defaults)) {
    limits.push({ key, text: formatLimit(key, value), source: 'default' });
  }
  const noPresets = !limits.some((l) => l.source !== 'default');
  return { counts, memory, process: processRows, limits, noPresets };
}

/** Whether a run's response changed the data, so the counts and memory are out of date. */
export function changedData(res) {
  return !!(res && res.changes && res.changes.data);
}
