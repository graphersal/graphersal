// The models of the Catalog dropdown and its Compression manager: what the engine's
// `listCompressions()` returns (crates/graphersal-session `compression.rs`: `[{name, element,
// label, path, codec, minBytes, dictionary, stats: {compressedValues, plainBytes, storedBytes,
// dictionaryBytes, distinctValues}}]`; codec "lz4" or "dedup") turned into rows to show, the form's draft and its spec, the form's
// estimate (`estimateCompression()`: the candidates measured on the current data, the best one,
// "Use"), and the items of the Catalog dropdown with their counts. Pure functions, no Vue, no DOM
// (tested under Node: tests/compression.test.mjs).

import { formatBytes } from './statistics.js';

/** The default threshold of a rule (the engine's `DEFAULT_MIN_BYTES`). */
export const DEFAULT_MIN_BYTES = 256;
/** The default threshold of a dedup rule (the engine's `DEFAULT_DEDUP_MIN_BYTES`). */
export const DEFAULT_DEDUP_MIN_BYTES = 0;
/** The codecs the form offers: lz4 compresses each value, dedup stores each distinct value once. */
export const CODECS = Object.freeze(['lz4', 'dedup']);
/** The codecs the form's Estimate measures (the engine's estimate spec takes a list). */
export const ESTIMATE_CODECS = Object.freeze(['lz4', 'dedup']);

/** The default threshold of a codec. */
export function defaultMinBytes(codec) {
  return codec === 'dedup' ? DEFAULT_DEDUP_MIN_BYTES : DEFAULT_MIN_BYTES;
}
/** The largest threshold (the engine's `MAX_MIN_BYTES`, the default `traversal.max_string_bytes`). */
export const MAX_MIN_BYTES = 16 * 1024 * 1024;

/**
 * The kinds of the catalog the Catalog dropdown lists, in its order, with what each manager
 * opens. The property indexes come after them: js/indexes.js `catalogMenuItems` appends their
 * item (and the dropdown uses that list).
 */
export const CATALOG_KINDS = Object.freeze([
  Object.freeze({ kind: 'query', id: 'queries', title: 'Saved queries', hint: 'Named, parameterized, read-only queries stored in the graph' }),
  Object.freeze({ kind: 'compression', id: 'compression', title: 'Compression', hint: 'Rules that keep large strings compressed in memory' }),
]);

/**
 * The items of the Catalog dropdown: one per kind with its count, from the graph info's
 * `catalog` (`{query: 2, compression: 1}`; older answers have only `savedQueries`).
 * @param {object|null} info the graph info
 * @returns {Array<{id:string, title:string, hint:string, count:number}>}
 */
export function catalogItems(info) {
  const counts = (info && info.catalog) || {};
  return CATALOG_KINDS.map((k) => {
    let count = counts[k.kind];
    if (count === undefined && k.kind === 'query') count = info && info.savedQueries;
    if (count === undefined && k.kind === 'compression') count = info && info.compressionRules;
    return { id: k.id, title: k.title, hint: k.hint, count: Number(count) || 0 };
  });
}

/** How many definitions of every kind the catalog holds (unknown kinds included). */
export function catalogTotal(info) {
  const counts = (info && info.catalog) || {};
  return Object.values(counts).reduce((sum, n) => sum + (Number(n) || 0), 0);
}

/**
 * The text of the Store menu's catalog line from `StoreInfo`'s `definitionsByKind`
 * (`{query: 2, compression: 1}`): `"2 saved queries, 1 compression rule"`.
 */
export function catalogCountsText(byKind, total) {
  const names = {
    query: ['saved query', 'saved queries'],
    compression: ['compression rule', 'compression rules'],
    property_index: ['property index', 'property indexes'],
    job: ['scheduled job', 'scheduled jobs'],
  };
  const order = CATALOG_KINDS.map((k) => k.kind);
  const rank = (kind) => (order.includes(kind) ? order.indexOf(kind) : order.length);
  const parts = Object.entries(byKind || {})
    .filter(([, n]) => Number(n) > 0)
    .sort(([a], [b]) => rank(a) - rank(b) || a.localeCompare(b))
    .map(([kind, n]) => {
      const [one, many] = names[kind] || [`${kind} definition`, `${kind} definitions`];
      return `${n} ${Number(n) === 1 ? one : many}`;
    });
  if (parts.length) return parts.join(', ');
  return total ? `${total} definition${total === 1 ? '' : 's'}` : 'empty';
}

/** `0.42` -> `42 %` (the stored share of the plain size); `—` without values. */
export function ratioText(stats) {
  if (!stats || !stats.plainBytes) return '—';
  return `${Math.round((100 * stats.storedBytes) / stats.plainBytes)} %`;
}

/**
 * One row of the rules table: the rule and its stats as text.
 * @param {object} rule an entry of `listCompressions()`
 */
export function ruleRow(rule) {
  const s = rule.stats || {};
  const dedup = rule.codec === 'dedup';
  return {
    name: rule.name,
    // A rule the engine does not apply (a codec, element kind or path of a newer version, or a
    // storage without compressed properties): `reason` says why.
    served: rule.served !== false,
    reason: rule.served === false ? (rule.reason || '') : '',
    codec: rule.codec || 'lz4',
    target: `${rule.element || 'vertex'} ${rule.label}`,
    path: rule.path,
    threshold: `≥ ${formatBytes(rule.minBytes)}`,
    dictionary: dedup ? '—' : (rule.dictionary ? (s.dictionaryBytes ? formatBytes(s.dictionaryBytes) : 'yes (none yet)') : 'no'),
    // A dedup rule: how many distinct texts its shared values hold.
    distinct: dedup ? String(s.distinctValues || 0) : '—',
    values: s.compressedValues || 0,
    sizes: s.compressedValues ? `${formatBytes(s.plainBytes)} → ${formatBytes(s.storedBytes)}` : '—',
    ratio: ratioText(s),
    saved: s.plainBytes > s.storedBytes ? formatBytes(s.plainBytes - s.storedBytes) : '—',
  };
}

/** The bytes a rule saves: plain minus stored (0 without stats or when it saves nothing). */
export function ruleSaved(rule) {
  const s = (rule && rule.stats) || {};
  return Math.max(0, (s.plainBytes || 0) - (s.storedBytes || 0));
}

/** What the rules table sorts by (column -> value of a rule); text columns start ascending. */
const SORT_VALUES = Object.freeze({
  name: (rule) => rule.name || '',
  codec: (rule) => rule.codec || 'lz4',
  values: (rule) => (rule.stats && rule.stats.compressedValues) || 0,
  plain: (rule) => (rule.stats && rule.stats.plainBytes) || 0,
  ratio: (rule) => (rule.stats && rule.stats.plainBytes ? rule.stats.storedBytes / rule.stats.plainBytes : 1),
  saved: ruleSaved,
});

/** Whether a first click on the rules table's column `key` sorts it descending. */
export function ruleSortStartsDescending(key) {
  return key === 'values' || key === 'plain' || key === 'saved';
}

/** The rules sorted by column `key`; ties by name, always ascending, so the order is stable. */
export function sortRules(rules, key = 'saved', desc = true) {
  const value = SORT_VALUES[key] || SORT_VALUES.saved;
  const sign = desc ? -1 : 1;
  return [...(rules || [])].sort((a, b) => {
    const x = value(a);
    const y = value(b);
    const order = typeof x === 'string' ? x.localeCompare(y) : x - y;
    return (order ? sign * order : 0) || String(a.name).localeCompare(String(b.name));
  });
}

/** The ways to group the rules table: `Group by` choices. */
export const RULE_GROUPINGS = Object.freeze([
  { key: 'codec', text: 'Codec' },
  { key: 'label', text: 'Label' },
  { key: 'none', text: 'Nothing' },
]);

/** A group with more rules than this starts collapsed (its totals say what it holds). */
export const COLLAPSE_ABOVE = 12;

/**
 * The rules as groups: `[{key, title, rules, names, count, saved}]`, the group that saves the
 * most first, its rules sorted by `sort` (see `sortRules`). `by` is a `RULE_GROUPINGS` key;
 * `none` gives one group without a title.
 */
export function groupRules(rules, by = 'codec', sort = 'saved', desc = true) {
  const keyOf = (rule) => {
    if (by === 'codec') return rule.codec || 'lz4';
    if (by === 'label') return `${rule.element || 'vertex'} ${rule.label}`;
    return '';
  };
  const groups = new Map();
  for (const rule of sortRules(rules, sort, desc)) {
    const key = keyOf(rule);
    if (!groups.has(key)) groups.set(key, { key, title: key, rules: [], names: [], count: 0, saved: 0 });
    const group = groups.get(key);
    group.rules.push(rule);
    group.names.push(rule.name);
    group.count += 1;
    group.saved += ruleSaved(rule);
  }
  return [...groups.values()].sort((a, b) => b.saved - a.saved || a.key.localeCompare(b.key));
}

/** Whether `group` is collapsed: the user's choice (`choices`: key -> boolean), else by its size. */
export function groupCollapsed(group, choices) {
  const chosen = choices && choices[group.key];
  return chosen === undefined ? group.count > COLLAPSE_ABOVE : chosen;
}

/** `all`, `some` or `none` of `names` are in `selected`. */
export function selectionState(names, selected) {
  const chosen = new Set(selected || []);
  const count = (names || []).filter((name) => chosen.has(name)).length;
  if (!count) return 'none';
  return count === names.length ? 'all' : 'some';
}

/** `selected` with `names` ticked, or unticked when all of them already are. */
export function toggleNames(selected, names) {
  const chosen = new Set(selected || []);
  const add = selectionState(names, selected) !== 'all';
  for (const name of names || []) {
    if (add) chosen.add(name);
    else chosen.delete(name);
  }
  return [...chosen];
}

/** `selected` without the names no rule of `rules` has any more. */
export function pruneRuleSelection(rules, selected) {
  const names = new Set((rules || []).map((rule) => rule.name));
  return (selected || []).filter((name) => names.has(name));
}

/**
 * The selection for "Drop selected": `{count, saved, text, confirm}`. `saved` is what the rules
 * save now, so what dropping them gives back to the values; `confirm` is the question asked first.
 */
export function dropSelection(rules, selected) {
  const chosen = new Set(selected || []);
  const picked = (rules || []).filter((rule) => chosen.has(rule.name));
  const saved = picked.reduce((sum, rule) => sum + ruleSaved(rule), 0);
  const count = picked.length;
  const these = count === 1 ? 'this compression rule' : `these ${count} compression rules`;
  const titles = picked.slice(0, 5).map((rule) => `• ${rule.name}`);
  if (count > 5) titles.push(`• … and ${count - 5} more`);
  return {
    count,
    saved,
    text: count ? `${count} selected · saves ${formatBytes(saved)} now` : 'nothing selected',
    confirm: count
      ? `Drop ${these} in one commit? Their values are stored in full again, so the memory grows by about ${formatBytes(saved)}. No data changes.\n\n${titles.join('\n')}\n\nAll or nothing: if it fails, every rule stays.`
      : '',
  };
}

/** The totals of all rules: values, plain, stored and dictionary bytes. */
export function totals(rules) {
  const t = { compressedValues: 0, plainBytes: 0, storedBytes: 0, dictionaryBytes: 0 };
  for (const r of rules || []) {
    const s = r.stats || {};
    t.compressedValues += s.compressedValues || 0;
    t.plainBytes += s.plainBytes || 0;
    t.storedBytes += s.storedBytes || 0;
    t.dictionaryBytes += s.dictionaryBytes || 0;
  }
  return t;
}

/** A new rule's draft (the form). */
export function newDraft() {
  return {
    name: '',
    element: 'vertex',
    label: '',
    path: '',
    minBytesText: String(DEFAULT_MIN_BYTES),
    dictionary: true,
    replaces: null,
  };
}

/**
 * The draft of a stored rule (Edit): its path without the leading `$.` when it is plain names.
 * A dedup rule keeps its codec in the draft (an lz4 rule leaves it out: the engine's default).
 */
export function draftFromRule(rule) {
  const path = String(rule.path || '');
  const plain = /^\$\.[A-Za-z_][A-Za-z0-9_]*(\.[A-Za-z_][A-Za-z0-9_]*)*$/.test(path);
  const draft = {
    name: rule.name,
    element: rule.element === 'edge' ? 'edge' : 'vertex',
    label: rule.label,
    path: plain ? path.slice(2) : path,
    minBytesText: String(rule.minBytes ?? defaultMinBytes(rule.codec)),
    dictionary: rule.codec === 'dedup' ? false : rule.dictionary !== false,
    replaces: rule.name,
  };
  if (rule.codec && rule.codec !== 'lz4') draft.codec = rule.codec;
  return draft;
}

/**
 * The draft with another codec chosen in the form: dedup has no dictionary, and a threshold
 * still at the former codec's default moves to the new codec's default.
 */
export function withCodec(draft, codec) {
  const from = draft.codec || 'lz4';
  const next = { ...draft, codec };
  const text = String(draft.minBytesText ?? '').trim();
  if (!text || text === String(defaultMinBytes(from))) next.minBytesText = String(defaultMinBytes(codec));
  if (codec === 'dedup') next.dictionary = false;
  else if (from === 'dedup') next.dictionary = true;
  return next;
}

/**
 * The spec `defineCompression` takes, or the field errors the form shows without asking the
 * engine (empty fields, a threshold that is not a whole number in range).
 * @returns {{spec: object} | {errors: Array<{field:string, message:string}>}}
 */
export function draftToSpec(draft) {
  const errors = [];
  const name = String(draft.name || '').trim();
  const label = String(draft.label || '').trim();
  const path = String(draft.path || '').trim();
  if (!name) errors.push({ field: 'name', message: 'A rule needs a name (an identifier such as customer_cv).' });
  if (!label) errors.push({ field: 'label', message: 'Name the vertex or edge label the rule applies to.' });
  if (!path) errors.push({ field: 'path', message: 'Name the property path, for example cv or details.career.cv.' });
  const minText = String(draft.minBytesText ?? '').trim();
  let minBytes = defaultMinBytes(draft.codec);
  if (minText) {
    minBytes = Number(minText);
    const least = draft.codec === 'dedup' ? 0 : 1;
    if (!Number.isInteger(minBytes) || minBytes < least || minBytes > MAX_MIN_BYTES) {
      errors.push({ field: 'minBytes', message: `A whole number of bytes from ${least} to ${MAX_MIN_BYTES}.` });
    }
  }
  if (errors.length) return { errors };
  const spec = {
    name,
    element: draft.element === 'edge' ? 'edge' : 'vertex',
    label,
    path,
    minBytes,
    // A dedup rule has no dictionary.
    dictionary: draft.codec === 'dedup' ? false : !!draft.dictionary,
  };
  // A codec appears only once "Use" of an estimate chose one (the engine's default is lz4).
  if (draft.codec) spec.codec = draft.codec;
  if (draft.replaces) spec.replaces = draft.replaces;
  return { spec };
}

// ------------------------------------------------------------------ the estimate

/** Whether the form can ask for an estimate: the label and the path are filled. */
export function canEstimate(draft) {
  return !!(draft && String(draft.label || '').trim() && String(draft.path || '').trim());
}

/**
 * The spec `estimateCompression` takes for the form's element, label and path (every
 * candidate is measured, so the threshold and dictionary of the form are not sent), or the
 * field errors of empty fields.
 * @returns {{spec: object} | {errors: Array<{field:string, message:string}>}}
 */
export function draftToEstimateSpec(draft) {
  const errors = [];
  const label = String((draft && draft.label) || '').trim();
  const path = String((draft && draft.path) || '').trim();
  if (!label) errors.push({ field: 'label', message: 'Name the vertex or edge label to estimate.' });
  if (!path) errors.push({ field: 'path', message: 'Name the property path to estimate, for example cv.' });
  if (errors.length) return { errors };
  return { spec: { element: draft.element === 'edge' ? 'edge' : 'vertex', label, path } };
}

/** `123456789` bytes per second -> `118 MiB/s`; `—` without a measurement. */
export function rateText(bytesPerSec) {
  if (typeof bytesPerSec !== 'number' || !Number.isFinite(bytesPerSec) || bytesPerSec <= 0) return '—';
  return `${formatBytes(Math.round(bytesPerSec))}/s`;
}

/** A candidate's name: `lz4`, `lz4 + dictionary`, `dedup`. */
export function candidateName(candidate) {
  return `${candidate.codec || 'lz4'}${candidate.dictionary ? ' + dictionary' : ''}`;
}

/**
 * The rows of the estimate table (an engine answer's `estimate`: crates/graphersal-session
 * `compression.rs`): one per candidate with its numbers as text, the share of the largest
 * saving for the bar (0..1; 0 for a candidate that saves nothing), and whether it is the best
 * one or the current rule's parameters.
 */
export function estimateRows(estimate) {
  const candidates = (estimate && estimate.candidates) || [];
  const most = Math.max(0, ...candidates.map((c) => c.savedBytes || 0));
  return candidates.map((c, index) => ({
    index,
    candidate: c,
    name: candidateName(c),
    threshold: `≥ ${formatBytes(c.minBytes)}`,
    values: c.compressedValues || 0,
    sizes: c.compressedValues ? `${formatBytes(c.plainBytes)} → ${formatBytes(c.storedBytes)}` : '—',
    dictionary: c.dictionary ? (c.dictionaryBytes ? formatBytes(c.dictionaryBytes) : 'none (too little text)') : '—',
    // A dedup candidate: the distinct texts among the values it would share.
    distinct: c.codec === 'dedup' ? String(c.distinctValues || 0) : '—',
    saved: c.savedBytes > 0 ? formatBytes(c.savedBytes) : (c.savedBytes < 0 ? `costs ${formatBytes(-c.savedBytes)}` : '—'),
    share: most > 0 && c.savedBytes > 0 ? c.savedBytes / most : 0,
    ratio: ratioText(c),
    // A shared value reads like a plain one.
    reads: c.codec === 'dedup' ? 'as plain' : rateText(c.decodeBytesPerSec),
    best: estimate.best === index,
    current: !!c.current,
  }));
}

/**
 * The estimate table as shown: the rows of [`estimateRows`] without the candidates that would
 * hold no value (every string at the path is shorter than their minimum size), unless one is the
 * best or the current rule, and a note naming what was left out (`''` when nothing was).
 */
export function estimateView(estimate) {
  const all = estimateRows(estimate);
  const rows = all.filter((r) => r.values > 0 || r.best || r.current);
  const smallest = new Map(); // codec -> the smallest left-out minimum size
  for (const r of all) {
    if (rows.includes(r)) continue;
    const codec = r.candidate.codec || 'lz4';
    smallest.set(codec, Math.min(smallest.get(codec) ?? Infinity, r.candidate.minBytes));
  }
  const parts = [...smallest].map(([codec, min]) => `${codec} from ≥ ${formatBytes(min)}`);
  const empty = parts.length
    ? `Not shown: ${parts.join(', ')} (with and without a dictionary): no string at the path is that long, so they would hold nothing.`
    : '';
  return { rows, empty };
}

/**
 * The lines above the estimate table: what was found at the path, whether it was sampled, the
 * current rule as the baseline, and the engine's warnings.
 */
export function estimateSummary(estimate) {
  if (!estimate) return { found: '', notes: [], warnings: [] };
  const element = estimate.element === 'edge' ? 'edge' : 'vertex';
  const n = (count, one, many) => `${count} ${count === 1 ? one : many}`;
  const found = `${n(estimate.elements || 0, element, element === 'edge' ? 'edges' : 'vertices')} labelled "${estimate.label}", `
    + `${n(estimate.valuesFound || 0, 'value', 'values')} at ${estimate.path}, `
    + `${n(estimate.stringValues || 0, 'string', 'strings')} (${formatBytes(estimate.stringBytes || 0)}).`;
  const notes = ['Estimated on the current data; the graph was not changed.'];
  if (estimate.sampled) {
    notes.push(`Measured on a sample of ${estimate.measuredValues} strings and extrapolated (the counts and sizes are exact).`);
  }
  const cur = estimate.current;
  if (cur && cur.codec === 'dedup') {
    const s = cur.stats || {};
    notes.push(`Today the dedup rule "${cur.name}" (≥ ${formatBytes(cur.minBytes)}) shares ${s.compressedValues || 0} values `
      + `(${s.distinctValues || 0} distinct), ${formatBytes(s.plainBytes || 0)} → ${formatBytes(s.storedBytes || 0)}.`);
  } else if (cur) {
    const s = cur.stats || {};
    notes.push(`Today the rule "${cur.name}" (≥ ${formatBytes(cur.minBytes)}, dictionary ${cur.dictionary ? 'on' : 'off'}) `
      + `holds ${s.compressedValues || 0} values, ${formatBytes(s.plainBytes || 0)} → ${formatBytes(s.storedBytes || 0)}.`);
  }
  const warnings = (estimate.warnings || []).map((w) => w.message || w.code);
  return { found, notes, warnings };
}

/**
 * The size histogram as rows: `0 B – 64 B: 3 values (168 B)`; empty buckets left out.
 */
export function histogramRows(estimate) {
  return ((estimate && estimate.histogram) || [])
    .filter((b) => b.values > 0)
    .map((b) => ({
      range: b.maxBytes == null ? `≥ ${formatBytes(b.minBytes)}` : `${formatBytes(b.minBytes)} – ${formatBytes(b.maxBytes)}`,
      values: b.values,
      bytes: formatBytes(b.bytes),
    }));
}

/** "Use": the form's draft with a candidate's codec, threshold and dictionary. */
export function applyCandidate(draft, candidate) {
  return {
    ...draft,
    codec: candidate.codec || 'lz4',
    minBytesText: String(candidate.minBytes),
    dictionary: !!candidate.dictionary,
  };
}

/**
 * The label suggestions of the form: the graph's labels of the chosen element kind, from the
 * engine's `statistics()` answer (`{statistics: {vertex_labels: [{label}], edge_labels:
 * [{label}]}}`), sorted, without duplicates.
 */
export function labelSuggestions(statistics, element) {
  const s = statistics && statistics.statistics;
  if (!s) return [];
  const rows = element === 'edge' ? s.edge_labels : s.vertex_labels;
  return [...new Set((rows || []).map((r) => r.label).filter((l) => typeof l === 'string' && l))].sort();
}
