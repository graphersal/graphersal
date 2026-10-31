// The System journal panel's model (Catalog ▸ System journal…; view
// js/components/journal.js). Pure functions over the engine's journal pages, tested under Node
// (tests/journal.test.mjs).
//
// A page is `Session::journal`'s answer (`GET /api/journal`, wasm `journal(spec)`):
// `{records, next_after, truncated, more, notes}`, records oldest first, each
// `{seq, origin: {id, name}, origin_seq, time (µs), level, source, kind, message, fields,
// op?, parent?, commit?, principal?}` (book "System Journal"). The engine reads forward from a
// cursor (`after`); the panel keeps the newest `JOURNAL_KEEP` records it has read, NEWEST
// FIRST, and renders at most `shown` of them (`JOURNAL_PAGE_ROWS` more per "Show more"), so a
// long journal never becomes an unbounded table.

/** Records asked per request (the engine's default page). */
export const JOURNAL_REQUEST_LIMIT = 1000;
/** Requests per refresh at most; the next refresh goes on from the cursor. */
export const JOURNAL_MAX_REQUESTS = 20;
/** Records kept in the panel (the newest). */
export const JOURNAL_KEEP = 2000;
/** Rows rendered at first and added per "Show more". */
export const JOURNAL_PAGE_ROWS = 300;

/** The levels, most severe first, with the filter's labels (`level` = that one and above). */
export const JOURNAL_LEVELS = [
  { name: '', label: 'Every level' },
  { name: 'info', label: 'info and above' },
  { name: 'notice', label: 'notice and above' },
  { name: 'warn', label: 'warn and above' },
  { name: 'error', label: 'error only' },
];

/** An empty view: nothing read yet. */
export function emptyJournal() {
  return { records: [], after: 0, truncated: false, dropped: 0, notes: [], more: false };
}

/**
 * The request object for the engine (`{after, level?, source?, limit}`): empty filters are
 * left out; the source is a prefix (`store` matches `store.backup`).
 */
export function journalSpec(filters, after, limit = JOURNAL_REQUEST_LIMIT) {
  const spec = { after: Math.max(0, Number(after) || 0), limit };
  const level = filters && filters.level ? String(filters.level).trim() : '';
  const source = filters && filters.source ? String(filters.source).trim() : '';
  if (level) spec.level = level;
  if (source) spec.source = source;
  return spec;
}

/**
 * The view after one more page: its records put in front (newest first, a record already held
 * is not added twice), the cursor moved, at most `keep` records (the oldest dropped and
 * counted in `dropped`). `truncated` (older records gone from an in-memory journal) and the
 * notes (damaged stored records skipped) are kept once seen.
 */
export function addJournalPage(view, page, keep = JOURNAL_KEEP) {
  const base = view || emptyJournal();
  const records = (page && Array.isArray(page.records)) ? page.records : [];
  const held = base.records.length ? base.records[0].seq : 0;
  const fresh = records.filter((r) => r && typeof r.seq === 'number' && r.seq > held).reverse();
  let all = fresh.concat(base.records);
  let dropped = base.dropped || 0;
  if (all.length > keep) {
    dropped += all.length - keep;
    all = all.slice(0, keep);
  }
  const notes = base.notes.slice();
  for (const note of (page && page.notes) || []) if (!notes.includes(note)) notes.push(note);
  const next = page && typeof page.next_after === 'number' ? page.next_after : base.after;
  return {
    records: all,
    after: Math.max(base.after, next),
    truncated: base.truncated || !!(page && page.truncated),
    dropped,
    notes,
    more: !!(page && page.more),
  };
}

/**
 * Reads the journal forward from the view's cursor: `fetchPage(spec)` is the engine call;
 * stops when no more follow or after `maxRequests` requests (the next refresh goes on).
 * Returns the new view.
 */
export async function readJournal(view, filters, fetchPage, maxRequests = JOURNAL_MAX_REQUESTS) {
  let current = view || emptyJournal();
  for (let i = 0; i < maxRequests; i++) {
    const page = await fetchPage(journalSpec(filters, current.after));
    current = addJournalPage(current, page);
    if (!current.more) break;
  }
  return current;
}

const pad = (n, width = 2) => String(n).padStart(width, '0');

/**
 * A record's time (µs since the Unix epoch) as `YYYY-MM-DD HH:MM:SS.mmm`, in the browser's
 * zone (`utc` true: in UTC, for tests); `''` for a missing time.
 */
export function journalTime(micros, utc = false) {
  if (typeof micros !== 'number' || !Number.isFinite(micros)) return '';
  const d = new Date(Math.floor(micros / 1000));
  if (Number.isNaN(d.getTime())) return String(micros);
  const get = (local, universal) => (utc ? d[universal]() : d[local]());
  return `${get('getFullYear', 'getUTCFullYear')}-${pad(get('getMonth', 'getUTCMonth') + 1)}-${pad(get('getDate', 'getUTCDate'))} `
    + `${pad(get('getHours', 'getUTCHours'))}:${pad(get('getMinutes', 'getUTCMinutes'))}:${pad(get('getSeconds', 'getUTCSeconds'))}`
    + `.${pad(get('getMilliseconds', 'getUTCMilliseconds'), 3)}`;
}

/** The ISO 8601 UTC form of a record's time, to the microsecond (the row's tooltip). */
export function journalIsoTime(micros) {
  if (typeof micros !== 'number' || !Number.isFinite(micros)) return '';
  const d = new Date(Math.floor(micros / 1000));
  if (Number.isNaN(d.getTime())) return String(micros);
  const sub = ((micros % 1_000_000) + 1_000_000) % 1_000_000;
  return `${d.toISOString().slice(0, 19)}.${pad(sub, 6)}Z`;
}

/** A field value as short text: strings plain, everything else as JSON. */
function valueText(value) {
  if (typeof value === 'string') return value;
  try {
    return JSON.stringify(value);
  } catch (err) {
    return String(value);
  }
}

/** The fields as `name=value, ...` (the CLI's line form). */
export function fieldsText(fields) {
  if (!fields || typeof fields !== 'object') return '';
  return Object.entries(fields).map(([k, v]) => `${k}=${valueText(v)}`).join(', ');
}

/** The badge class of a level (`badge danger` for errors, ...). */
export function levelClass(level) {
  switch (level) {
    case 'error': return 'badge danger';
    case 'warn': return 'badge warn';
    case 'notice': return 'badge accent';
    default: return 'badge';
  }
}

/** The rows to render: the first `shown` records of the view (newest first). */
export function journalRows(view, shown = JOURNAL_PAGE_ROWS, utc = false) {
  const records = (view && view.records) || [];
  return records.slice(0, Math.max(0, shown)).map((r) => {
    const extra = [];
    if (r.principal) extra.push(`by ${r.principal}`);
    if (r.commit != null) extra.push(`commit ${r.commit}`);
    if (r.op != null) extra.push(`op ${r.op}`);
    return {
      seq: r.seq,
      time: journalTime(r.time, utc),
      iso: journalIsoTime(r.time),
      level: r.level || '',
      levelClass: levelClass(r.level),
      source: r.source || '',
      kind: r.kind || '',
      message: r.message || '',
      fields: fieldsText(r.fields),
      extra: extra.join(' · '),
      origin: r.origin && r.origin.name ? `${r.origin.name} #${r.origin_seq}` : '',
    };
  });
}

/** The line under the table: how many rows are not shown, and what is gone. */
export function journalFooter(view, shown) {
  const total = (view && view.records && view.records.length) || 0;
  const hidden = Math.max(0, total - shown);
  const parts = [];
  if (hidden) parts.push(`${hidden} more`);
  if (view && view.dropped) parts.push(`${view.dropped} older records not kept here`);
  if (view && view.truncated) parts.push('the oldest records are gone from the in-memory journal');
  return { hidden, text: parts.join(' · ') };
}

/**
 * The sources to suggest in the filter: every source the view holds and each of its dotted
 * prefixes (`store.backup` suggests `store` too), sorted.
 */
export function sourceChoices(view) {
  const set = new Set();
  for (const r of (view && view.records) || []) {
    const parts = String(r.source || '').split('.').filter(Boolean);
    for (let i = 1; i <= parts.length; i++) set.add(parts.slice(0, i).join('.'));
  }
  return [...set].sort();
}
