// The data model of the process memory bar (components/memory-bar.js), shown in the Profile
// panel's Statistics tab and in Optimize -> Memory: how full the process is against its memory
// ceiling and where the memory goes. Pure functions, no Vue, no DOM (tests/memory-bar.test.mjs).
//
// The figures are computed ONCE, in Rust (crates/graphersal-session `process::MemoryBar`, the
// same numbers the REPL's `/memory` prints): `statistics().process.memory` carries `usedBytes`,
// `usedTracked`, `limitBytes?`, `limitSource`, `usedPercent?`, `segments: [{name, bytes}]` and
// `compression?: {plainBytes, storedBytes}`. This model only renders them; it never recomputes a
// segment.
//
// The per-query peak of a PROFILE (`query_memory`, against `traversal.max_materialized_bytes`)
// is a different figure against a different limit: see `queryPeakModel`. The two limits are
// never mixed.

import { formatBytes } from './statistics.js';

/** Percentages from which the figure turns amber, and red above the second. */
export const AMBER_FROM = 70;
export const RED_ABOVE = 90;

/** What each segment holds (the legend's tooltips). */
export const SEGMENT_HINTS = {
  structure: 'vertex and edge slots, ids and labels (the Memory row\'s Structure)',
  properties: 'property values and their key lists, stored plain',
  compressed: 'compressed and deduplicated values with their dictionaries and shared tables',
  indexes: 'the id and label indexes plus the property indexes (the Memory row\'s Indexes and Property indexes)',
  models: 'embedding models held in memory',
  other: 'the rest of the process heap: query buffers, caches, the session, allocator overhead',
};

/** The words for a ceiling's source (`MemoryLimitSource`). */
export const SOURCE_TEXT = {
  configured: 'configured',
  cgroup: 'cgroup limit',
  physical: '80 % of RAM',
  wasm: 'WebAssembly default',
  none: 'memory.limit none',
  unknown: 'memory size unknown',
};

/**
 * The colour level of a percentage: `normal` below 70 %, `amber` from 70 to 90 %, `red` above;
 * `null` without a percentage.
 * @param {number|null|undefined} percent
 * @returns {'normal'|'amber'|'red'|null}
 */
export function memoryLevel(percent) {
  if (typeof percent !== 'number' || !Number.isFinite(percent)) return null;
  if (percent > RED_ABOVE) return 'red';
  if (percent >= AMBER_FROM) return 'amber';
  return 'normal';
}

/**
 * `47.06` -> `47 %`; below 1 one decimal (`0.4 %`) and below 0.1 `< 0.1 %` (the engine sends one
 * decimal), so a small process does not read `0 %`.
 */
export function percentText(percent) {
  if (typeof percent !== 'number' || !Number.isFinite(percent)) return '';
  if (percent < 0.1) return '< 0.1 %';
  if (percent < 1) return `${percent.toFixed(1)} %`;
  return `${Math.round(percent)} %`;
}

/**
 * The bar's model from a `statistics()` answer, or null when the answer has no bar
 * (`process.memory.segments` absent: an older engine, or `limitError`).
 * @param {Object|null} raw  The engine's `statistics()` answer.
 * @returns {null|{usedText:string, limitText:string|null, sourceText:string, percent:number|null,
 *   percentText:string, level:'normal'|'amber'|'red'|null,
 *   segments:Array<{name:string, bytes:number, text:string, share:number, colour:string, hint:string}>,
 *   label:string, savingText:string|null, notTracked:boolean, noCeiling:boolean,
 *   estimateAboveHeap:boolean, error:string|null}}
 */
export function memoryBarModel(raw) {
  const mem = raw && raw.process && raw.process.memory;
  if (!mem || !Array.isArray(mem.segments)) return null;
  const used = typeof mem.usedBytes === 'number' ? mem.usedBytes : 0;
  const limit = typeof mem.limitBytes === 'number' && mem.limitBytes > 0 ? mem.limitBytes : null;
  const percent = limit !== null && typeof mem.usedPercent === 'number' ? mem.usedPercent : null;
  const noCeiling = percent === null;
  const notTracked = mem.usedTracked === false;
  const known = mem.segments.reduce((sum, s) => sum + (s.bytes || 0), 0);
  // With a ceiling the bar is the ceiling (empty room at the right); without one, the used part.
  const whole = limit !== null ? limit : Math.max(known, used, 1);
  const segments = mem.segments.map((s) => ({
    name: s.name,
    bytes: s.bytes,
    text: formatBytes(s.bytes),
    share: Math.min(100, (s.bytes / whole) * 100),
    colour: `var(--mem-${s.name}, var(--text-faint))`,
    hint: SEGMENT_HINTS[s.name] || '',
  }));
  const source = mem.limitSource || 'unknown';
  const sourceText = SOURCE_TEXT[source] || source;
  let savingText = null;
  const c = mem.compression;
  if (c && typeof c.plainBytes === 'number' && typeof c.storedBytes === 'number') {
    if (c.plainBytes > c.storedBytes) {
      const saved = c.plainBytes - c.storedBytes;
      savingText = `compression saves ${formatBytes(saved)} (plain ${formatBytes(c.plainBytes)} → ${formatBytes(c.storedBytes)}, −${Math.round((saved / c.plainBytes) * 100)} %)`;
    } else {
      savingText = `compression saves nothing (plain ${formatBytes(c.plainBytes)} → ${formatBytes(c.storedBytes)})`;
    }
  }
  const usedText = `${formatBytes(used)}${notTracked ? ' known' : ''}`;
  const limitText = limit !== null ? formatBytes(limit) : null;
  const label = [
    noCeiling ? `Process memory ${usedText}, no ceiling` : `Process memory ${usedText} of ${limitText}, ${percentText(percent)}`,
    ...segments.map((s) => `${s.name} ${s.text}`),
  ].join('; ');
  return {
    usedText,
    limitText,
    sourceText,
    percent,
    percentText: percentText(percent),
    level: memoryLevel(percent),
    segments,
    label,
    savingText,
    notTracked,
    noCeiling,
    estimateAboveHeap: mem.estimateAboveHeap === true,
    error: mem.limitError || null,
  };
}

/**
 * The PROFILE's per-query peak (`TraversalMetrics.query_memory`: `{peak_bytes, limit_bytes,
 * limit, source}`, against the per-execution limit `traversal.max_materialized_bytes`, never the
 * process ceiling), or null when the profile has none.
 * @param {Object|null} metrics  TraversalMetrics.
 * @returns {null|{text:string, percent:number|null, level:'normal'|'amber'|'red'|null}}
 */
export function queryPeakModel(metrics) {
  const q = metrics && metrics.query_memory;
  if (!q || typeof q.peak_bytes !== 'number') return null;
  const limit = typeof q.limit_bytes === 'number' && q.limit_bytes > 0 ? q.limit_bytes : null;
  const percent = limit !== null ? (q.peak_bytes / limit) * 100 : null;
  const of = limit !== null ? ` of ${formatBytes(limit)} per query` : '';
  const why = [q.limit || 'traversal.max_materialized_bytes', q.source].filter(Boolean).join(', ');
  return {
    text: `Peak memory ${formatBytes(q.peak_bytes)}${of} (${why})`,
    percent,
    level: memoryLevel(percent),
  };
}
