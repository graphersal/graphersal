// The Optimize panel's model (pure, tested under Node by tests/optimize.test.mjs): the storage
// advisor's recommendations, the workload statistics as tables with a sparkline per row and a
// detail chart, and the memory report as tables. The view is js/components/optimize.js.

import { formatBytes } from './statistics.js';

/** The windows of the workload statistics, each with the history tier its sparklines use. */
export const WINDOWS = Object.freeze([
  { id: 'hour', label: 'Last hour', field: 'last_hour', tier: 'minutes' },
  { id: 'two_days', label: 'Last 2 days', field: 'last_two_days', tier: 'hours' },
  { id: 'ninety_days', label: 'Last 90 days', field: 'last_ninety_days', tier: 'days' },
]);

/** The history tiers: bucket width in microseconds and the buckets kept. */
export const TIERS = Object.freeze({
  minutes: { width: 60e6, slots: 60, label: 'per minute, last 60 minutes' },
  hours: { width: 3600e6, slots: 48, label: 'per hour, last 48 hours' },
  days: { width: 86400e6, slots: 90, label: 'per day, last 90 days' },
});

/** The workload lists, in the panel's order. */
export const LISTS = Object.freeze([
  { id: 'queries', label: 'Queries', what: 'Query (normalized plan)' },
  { id: 'properties', label: 'Properties', what: 'Property use' },
  { id: 'locks', label: 'Locks', what: 'Lock' },
  { id: 'jobs', label: 'Jobs', what: 'Job' },
]);

const KIND_LABELS = Object.freeze({
  create_index: 'Index',
  drop_index: 'Index',
  add_compression: 'Memory',
  drop_compression: 'Speed',
});

/** A duration in microseconds for a cell: `850 µs`, `12.4 ms`, `3.20 s`. */
export function formatMicros(micros) {
  const n = Number(micros) || 0;
  if (n < 1000) return `${Math.round(n)} µs`;
  if (n < 1e6) return `${(n / 1000).toFixed(1)} ms`;
  return `${(n / 1e6).toFixed(2)} s`;
}

/** The effect of a recommendation on memory: `frees 1.2 MiB`, `takes about 40.0 KiB`. */
export function effectText(bytes, measured) {
  const n = Number(bytes) || 0;
  if (n === 0) return 'no memory change';
  const amount = formatBytes(Math.abs(n));
  if (n > 0) return `frees ${measured ? '' : 'about '}${amount}`;
  return `takes ${measured ? '' : 'about '}${amount}`;
}

/**
 * The share of the graph's memory a change of `bytes` is: `0.9 % of the graph`, `< 0.1 % of the
 * graph`; empty without a graph size or a change.
 */
export function shareText(bytes, graphBytes) {
  const n = Math.abs(Number(bytes) || 0);
  const total = Number(graphBytes) || 0;
  if (!n || total <= 0) return '';
  const percent = (n * 100) / total;
  if (percent < 0.1) return '< 0.1 % of the graph';
  return `${percent < 10 ? percent.toFixed(1) : Math.round(percent)} % of the graph`;
}

/** The rows of the recommendation list. */
export function recommendationRows(advice) {
  const list = (advice && advice.recommendations) || [];
  const graphBytes = advice && advice.graph_bytes;
  return list.map((r) => ({
    id: r.id,
    kind: r.kind,
    badge: KIND_LABELS[r.kind] || r.kind,
    title: r.title,
    evidence: r.evidence,
    cost: r.cost,
    effect: effectText(r.memory_bytes, r.measured),
    share: shareText(r.memory_bytes, graphBytes),
    frees: (Number(r.memory_bytes) || 0) > 0,
    impact: r.impact || null,
    confidence: r.confidence,
    reversible: !!r.reversible,
    command: r.command,
  }));
}

/**
 * The groups of the advice (one per kind, and for compression per codec), for "select all" in
 * one click: `[{key, title, ids, count}]`, in the advice's order. The engine answers them
 * (`advice.groups`); an older engine without them gets one group per kind computed here.
 */
export function recommendationGroups(advice) {
  const recs = (advice && advice.recommendations) || [];
  if (advice && Array.isArray(advice.groups)) {
    return advice.groups.map((g) => ({ key: g.key, title: g.title, ids: [...(g.ids || [])], count: (g.ids || []).length }));
  }
  const groups = [];
  for (const r of recs) {
    let group = groups.find((g) => g.key === r.kind);
    if (!group) {
      group = { key: r.kind, title: '', ids: [], count: 0, bytes: 0 };
      groups.push(group);
    }
    group.ids.push(r.id);
    group.count += 1;
    group.bytes += Number(r.memory_bytes) || 0;
  }
  return groups.map((g) => ({
    key: g.key,
    title: `${g.key.replace(/_/g, ' ')}: ${g.count} ${g.count === 1 ? 'key' : 'keys'}, total ${g.bytes > 0 ? '-' : '+'}${formatBytes(Math.abs(g.bytes))}`,
    ids: g.ids,
    count: g.count,
  }));
}

/**
 * The selection after the advice changed: only the ids still advised, in the advice's order
 * (nothing is ever selected by default: a human picks).
 */
export function pruneSelection(advice, selected) {
  const chosen = new Set(selected || []);
  return ((advice && advice.recommendations) || []).map((r) => r.id).filter((id) => chosen.has(id));
}

/** Whether every id of `group` is selected (`false` for an empty group). */
export function groupSelected(group, selected) {
  const chosen = new Set(selected || []);
  return !!group && group.ids.length > 0 && group.ids.every((id) => chosen.has(id));
}

/**
 * The selection with the ids of `group` added (when not all of them were selected) or removed
 * (when all were), kept in the advice's order.
 */
export function toggleGroup(advice, selected, group) {
  const chosen = new Set(selected || []);
  if (groupSelected(group, selected)) for (const id of group.ids) chosen.delete(id);
  else for (const id of group.ids) chosen.add(id);
  return pruneSelection(advice, [...chosen]);
}

/** The selection with `id` added or removed, kept in the advice's order. */
export function toggleRecommendation(advice, selected, id) {
  const chosen = new Set(selected || []);
  if (chosen.has(id)) chosen.delete(id);
  else chosen.add(id);
  return pruneSelection(advice, [...chosen]);
}

/**
 * What the selection does, for the "Apply selected" bar and its confirmation:
 * `{count, bytes, text, confirm}`; `text` is `"3 selected · frees 12.0 MiB"`, `confirm` the
 * question asked before applying (one commit, all or nothing, each reversible).
 */
export function selectionSummary(advice, selected) {
  const chosen = new Set(selected || []);
  const recs = ((advice && advice.recommendations) || []).filter((r) => chosen.has(r.id));
  const bytes = recs.reduce((sum, r) => sum + (Number(r.memory_bytes) || 0), 0);
  const count = recs.length;
  const effect = effectText(bytes, recs.every((r) => r.measured));
  const text = count ? `${count} selected · ${effect}` : 'nothing selected';
  const titles = recs.slice(0, 5).map((r) => `• ${r.title}`);
  if (count > 5) titles.push(`• … and ${count - 5} more`);
  const confirm = count
    ? `Apply ${count === 1 ? 'this recommendation' : `these ${count} recommendations`} in one commit (${effect})?\n\n${titles.join('\n')}\n\nAll or nothing: if one is refused, none is applied. Each can be undone later (drop the rule or the index).`
    : '';
  return { count, bytes, text, confirm };
}

/** Whether applied recommendations of these kinds changed compression rules (a pass per open). */
export function changesCompressionRules(kinds) {
  return (kinds || []).some((kind) => kind === 'add_compression' || kind === 'drop_compression');
}

/**
 * Whether to recommend a snapshot after compression rules changed at commit `rulesCommit`: while
 * no snapshot holds that commit, every open of the store replays the rule changes and re-encodes
 * their labels (book page "Optimize"). `store` is the Store menu's summary, null without a store.
 */
export function checkpointDue(store, rulesCommit) {
  if (!store || rulesCommit == null) return false;
  const newest = Math.max(0, ...(store.snapshots || []).map((s) => Number(s.commitSeq) || 0));
  return newest < rulesCommit;
}

/** The days a "Snooze" dismisses a recommendation for. */
export const SNOOZE_DAYS = 7;

/** A day count or a date for "dismissed until": `for good`, `for 3 more days`, `until tomorrow`. */
function untilText(until, now) {
  if (until == null) return 'for good';
  const days = Math.ceil((until - now) / TIERS.days.width);
  if (days <= 1) return 'until tomorrow';
  return `for ${days} more days`;
}

/** The dismissed recommendations: `[{id, title, until}]`. */
export function dismissedRows(advice, now = Date.now() * 1000) {
  return ((advice && advice.dismissed) || []).map((d) => ({ id: d.id, title: d.title, until: untilText(d.until, now) }));
}

/** The outcomes of applied recommendations: `[{id, title, when, promised, measured, lookups}]`. */
export function appliedRows(advice, now = Date.now() * 1000) {
  return ((advice && advice.applied) || []).map((a) => {
    const days = Math.floor((now - a.at) / TIERS.days.width);
    return {
      id: a.id,
      title: a.title,
      when: days <= 0 ? 'today' : days === 1 ? 'yesterday' : `${days} days ago`,
      promised: effectText(a.promised_bytes, true),
      measured: effectText(a.measured_bytes, true),
      lookups: a.index_lookups == null ? null : `${a.index_lookups} indexed lookup${a.index_lookups === 1 ? '' : 's'} since`,
    };
  });
}

/** One line about the advice: how many, how much memory in all, what it rests on. */
export function adviceSummary(advice, now = Date.now() * 1000) {
  if (!advice) return '';
  const recs = advice.recommendations || [];
  const freed = recs.reduce((sum, r) => sum + Math.max(0, Number(r.memory_bytes) || 0), 0);
  const parts = [];
  parts.push(recs.length === 1 ? '1 recommendation' : `${recs.length} recommendations`);
  if (freed > 0) parts.push(`up to ${formatBytes(freed)} of memory to free`);
  if (advice.workload_level === 'off') {
    parts.push('no workload statistics (the graph is not in a store): only the data was looked at');
  } else if (advice.observed_since != null) {
    const days = Math.max(0, Math.floor((now - advice.observed_since) / TIERS.days.width));
    parts.push(days === 0 ? 'workload observed today' : `workload observed for ${days} day${days === 1 ? '' : 's'}`);
  }
  return parts.join(' · ');
}

/** The channels present in a workload report, sorted. */
export function channels(report) {
  const set = new Set();
  for (const list of LISTS) for (const row of (report && report[list.id]) || []) if (row.channel) set.add(row.channel);
  return [...set].sort();
}

/** What a row is about, as one text. */
function rowName(listId, row) {
  switch (listId) {
    case 'queries': return row.query;
    case 'properties': {
      const target = row.label ? `${row.label}.${row.key}` : `${row.element || 'any'} *.${row.key}`;
      return `${target} · ${row.access}`;
    }
    case 'locks': return row.lock;
    case 'jobs': return row.name;
    default: return '';
  }
}

/**
 * The buckets of a row's history laid on the tier's slots, oldest first, ending at the slot of
 * `now` (microseconds): `[{start, count, failed, mean, max}]` with zeros for empty slots.
 */
export function bucketSeries(history, tier, now) {
  const { width, slots } = TIERS[tier] || TIERS.minutes;
  const last = Math.floor(now / width);
  const first = last - slots + 1;
  const out = [];
  for (let i = first; i <= last; i++) out.push({ start: i * width, count: 0, failed: 0, mean: 0, max: 0 });
  for (const b of history || []) {
    const index = Math.floor(b.start / width) - first;
    if (index < 0 || index >= out.length) continue;
    const slot = out[index];
    slot.count = b.count || 0;
    slot.failed = b.failed || 0;
    slot.mean = b.count ? (b.total_micros || 0) / b.count : 0;
    slot.max = b.max_micros || 0;
  }
  return out;
}

/** The "Max time ≥" choices of the workload tables, in microseconds (0: every row). */
export const MIN_TIMES = Object.freeze([
  { micros: 0, label: 'Any time' },
  { micros: 1e3, label: 'Max ≥ 1 ms' },
  { micros: 1e4, label: 'Max ≥ 10 ms' },
  { micros: 1e5, label: 'Max ≥ 100 ms' },
  { micros: 1e6, label: 'Max ≥ 1 s' },
]);

/** The columns a workload table sorts by; numbers start descending, the channel ascending. */
export const SORTS = Object.freeze(['channel', 'count', 'failed', 'mean', 'max', 'total']);

/** The first direction of a column: `true` for descending (numbers), `false` for text. */
export function sortsDescending(column) {
  return column !== 'channel';
}

/**
 * The rows of one workload list in a window, for a channel (`all`: every channel):
 * `{key, name, channel, count, failed, items, total, mean, max, series}`. `options.minMicros`
 * keeps only rows whose longest run took at least that long; `options.sort` (one of
 * [`SORTS`], default `total`) orders them in direction `options.desc` (default: the column's
 * first direction, [`sortsDescending`]); ties by total time, then by name.
 */
export function workloadRows(report, listId, windowId, channel = 'all', now = Date.now() * 1000, options = {}) {
  const minMicros = options.minMicros || 0;
  const sort = SORTS.includes(options.sort) ? options.sort : 'total';
  const desc = options.desc === undefined ? sortsDescending(sort) : !!options.desc;
  const window = WINDOWS.find((w) => w.id === windowId) || WINDOWS[0];
  const rows = [];
  for (const row of (report && report[listId]) || []) {
    if (channel !== 'all' && row.channel !== channel) continue;
    const c = row[window.field] || {};
    if (!c.count) continue;
    if ((c.max_micros || 0) < minMicros) continue;
    const name = rowName(listId, row);
    rows.push({
      key: `${listId}|${name}|${row.channel}`,
      name,
      channel: row.channel,
      count: c.count,
      failed: c.failed || 0,
      items: c.items || 0,
      total: c.total_micros || 0,
      mean: c.count ? (c.total_micros || 0) / c.count : 0,
      max: c.max_micros || 0,
      series: row.history ? bucketSeries(row.history, window.tier, now) : null,
      lastRun: row.last_run || null,
    });
  }
  const compare = sort === 'channel'
    ? (a, b) => String(a.channel).localeCompare(String(b.channel))
    : (a, b) => a[sort] - b[sort];
  rows.sort((a, b) => (desc ? compare(b, a) : compare(a, b)) || b.total - a.total || a.name.localeCompare(b.name));
  return rows;
}

/** An SVG path through `values` scaled into `width` x `height` (empty without values). */
export function sparkPath(values, width, height) {
  if (!values || !values.length) return '';
  const max = Math.max(...values, 0);
  const step = values.length > 1 ? width / (values.length - 1) : 0;
  return values
    .map((v, i) => {
      const x = (i * step).toFixed(1);
      const y = (max > 0 ? height - (v / max) * height : height).toFixed(1);
      return `${i ? 'L' : 'M'}${x} ${y}`;
    })
    .join(' ');
}

/**
 * The geometry of a detail chart over `series` (bucketSeries): one bar per bucket for the runs
 * (failed part on top), lines for the mean and the max time on their own scale:
 * `{bars: [{x, w, h, failedH, title}], mean, max, countMax, timeMax}`.
 */
export function chartGeometry(series, width, height, tier) {
  const n = (series && series.length) || 0;
  const countMax = Math.max(0, ...(series || []).map((b) => b.count));
  const timeMax = Math.max(0, ...(series || []).map((b) => b.max));
  const w = n ? width / n : 0;
  const bars = (series || []).map((b, i) => {
    const h = countMax ? (b.count / countMax) * height : 0;
    const failedH = countMax ? (b.failed / countMax) * height : 0;
    return {
      x: +(i * w).toFixed(2),
      w: +Math.max(1, w - 1).toFixed(2),
      h: +h.toFixed(2),
      failedH: +failedH.toFixed(2),
      title: `${bucketLabel(b.start, tier)}: ${b.count} runs${b.failed ? `, ${b.failed} failed` : ''}${b.count ? `, mean ${formatMicros(b.mean)}, max ${formatMicros(b.max)}` : ''}`,
    };
  });
  const line = (field) => {
    if (!n || !timeMax) return '';
    return series
      .map((b, i) => `${i ? 'L' : 'M'}${(i * w + w / 2).toFixed(1)} ${(height - (b[field] / timeMax) * height).toFixed(1)}`)
      .join(' ');
  };
  return { bars, mean: line('mean'), max: line('max'), countMax, timeMax };
}

/** A bucket's start for a tooltip: `14:05`, `Mon 14:00`, `2026-10-25`. */
export function bucketLabel(start, tier) {
  const d = new Date(start / 1000);
  const pad = (n) => String(n).padStart(2, '0');
  if (tier === 'days') return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
  const time = `${pad(d.getHours())}:${pad(d.getMinutes())}`;
  if (tier === 'hours') return `${d.toLocaleDateString(undefined, { weekday: 'short' })} ${time}`;
  return time;
}

/**
 * The most rows the Memory tab shows per list (properties, labels), the engine's default limit
 * (`MemoryReportRequest::DEFAULT_LIMIT`). The engine already answers at most this many; the cut
 * here also guards a longer answer (an older server), so a graph with a hundred thousand labels
 * never renders a hundred thousand rows.
 */
export const MEMORY_LIST_LIMIT = 500;

/** The memory report spec the Memory tab asks for. */
export function memoryReportSpec() {
  return { limit: MEMORY_LIST_LIMIT };
}

/** The property rows of a memory report, the most bytes first, at most `MEMORY_LIST_LIMIT`. */
export function memoryRows(report) {
  const rows = ((report && report.properties) || []).map((p) => {
    const path = String(p.path || '').replace(/^\$\./, '');
    const types = (p.value_types || []).map((t) => t.type).join(', ');
    const badges = [];
    if ((p.rules || []).length) badges.push(`compressed (${p.rules.join(', ')})`);
    if ((p.indexes || []).length) badges.push(`indexed (${p.indexes.join(', ')})`);
    if ((p.vectors || []).length) badges.push('vectors');
    const strings = p.strings || null;
    return {
      key: `${p.element}|${p.label}|${p.path}`,
      target: `${p.label == null ? '(no label)' : p.label}.${path}`,
      element: p.element,
      elements: p.elements || 0,
      bytes: p.bytes || 0,
      types,
      distinct: strings ? strings.distinct_estimate : null,
      maxValue: p.max_value_bytes || 0,
      badges,
    };
  });
  rows.sort((a, b) => b.bytes - a.bytes || a.target.localeCompare(b.target));
  return rows.slice(0, MEMORY_LIST_LIMIT);
}

/** The label rows of a memory report, the most bytes first, at most `MEMORY_LIST_LIMIT`. */
export function labelRows(report) {
  const rows = ((report && report.labels) || []).map((l) => ({
    key: `${l.element}|${l.label}`,
    label: l.label == null ? '(no label)' : l.label,
    element: l.element,
    elements: l.elements || 0,
    slots: l.slot_bytes || 0,
    properties: l.property_bytes || 0,
    values: l.values || 0,
    maxDegree: l.degree ? l.degree.max : null,
  }));
  rows.sort((a, b) => b.properties + b.slots - (a.properties + a.slots) || a.label.localeCompare(b.label));
  return rows.slice(0, MEMORY_LIST_LIMIT);
}

/**
 * What a list of the Memory tab leaves out (`kind` `properties` or `labels`): the rows the engine
 * left out (`rows_omitted`/`labels_omitted` with their bytes) plus any the tab cut itself.
 * `{count, bytes, elements, text}`, `null` when nothing is left out; `text` is
 * `"39,500 more properties not shown (10.2 MiB)"`.
 */
export function memoryOmitted(report, kind, format = (n) => `${n} B`) {
  if (!report) return null;
  const labels = kind === 'labels';
  const list = (labels ? report.labels : report.properties) || [];
  const cut = list.length > MEMORY_LIST_LIMIT ? list.slice(MEMORY_LIST_LIMIT) : [];
  const rowBytes = labels ? (l) => (l.property_bytes || 0) + (l.slot_bytes || 0) : (p) => p.bytes || 0;
  let count = (labels ? report.labels_omitted : report.rows_omitted) || 0;
  let bytes = (labels ? report.labels_omitted_bytes : report.rows_omitted_bytes) || 0;
  let elements = labels ? report.labels_omitted_elements || 0 : 0;
  if (cut.length) {
    // The tab's own cut takes the smallest rows after its sort.
    const sorted = [...list].sort((a, b) => rowBytes(b) - rowBytes(a));
    for (const row of sorted.slice(MEMORY_LIST_LIMIT)) {
      count += 1;
      bytes += rowBytes(row);
      if (labels) elements += row.elements || 0;
    }
  }
  if (!count) return null;
  const noun = labels ? (count === 1 ? 'label' : 'labels') : (count === 1 ? 'property' : 'properties');
  const what = labels && elements ? `${elements.toLocaleString('en-US')} element${elements === 1 ? '' : 's'}, ` : '';
  return {
    count,
    bytes,
    elements,
    text: `${count.toLocaleString('en-US')} more ${noun} not shown (${what}${format(bytes)})`,
  };
}

/**
 * Where the whole process memory goes (`report.components`, book page "Memory Usage"): one row
 * per component, most bytes first, then the remainder no component explains
 * (`unattributed_bytes`, absent without a tracking allocator). `[]` for an older engine.
 * @returns {Array<{key:string, name:string, detail:string, bytes:number, exact:boolean, share:number|null}>}
 */
export function memoryComponents(report) {
  if (!report || !Array.isArray(report.components)) return [];
  const tracked = report.tracked_live_bytes || 0;
  const share = (bytes) => (tracked > 0 ? bytes / tracked : null);
  const rows = report.components
    .map((c, i) => ({
      key: `${c.name}:${c.detail || ''}:${i}`,
      name: c.name,
      detail: c.detail || '',
      bytes: c.bytes || 0,
      exact: !!c.exact,
      share: share(c.bytes || 0),
    }))
    .sort((a, b) => b.bytes - a.bytes);
  if (typeof report.unattributed_bytes === 'number') {
    rows.push({
      key: 'unattributed',
      name: 'unattributed',
      detail: 'the tracked heap no component explains',
      bytes: report.unattributed_bytes,
      exact: false,
      share: share(report.unattributed_bytes),
    });
  }
  return rows;
}

/** The memory totals of a report: `[{label, bytes}]` (structure, indexes, properties, ...). */
export function memoryTotals(report) {
  const totals = report && report.structure && report.structure.totals;
  if (!totals) return report ? [{ label: 'Properties', bytes: report.property_bytes || 0 }] : [];
  return [
    { label: 'Total', bytes: totals.total_bytes || 0 },
    { label: 'Structure', bytes: totals.structure_bytes || 0 },
    { label: 'Indexes (id, labels)', bytes: totals.index_bytes || 0 },
    { label: 'Properties', bytes: totals.property_bytes || 0 },
    { label: 'Property indexes', bytes: totals.property_index_bytes || 0 },
    { label: 'Compression dictionaries', bytes: totals.compression_dictionary_bytes || 0 },
  ];
}
