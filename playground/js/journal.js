// The System journal panel's model (Catalog ▸ System journal…; view
// js/components/journal.js). Pure functions over the engine's journal pages, tested under Node
// (tests/journal.test.mjs).
//
// A page is `Session::journal`'s answer (`GET /api/journal`, wasm `journal(spec)`):
// `{records, next_after, next_before?, truncated, more, notes}`, each record `{seq, origin:
// {id, name}, origin_seq, time (µs), level, source, kind, message, fields, op?, parent?,
// commit?, principal?}` (book "System Journal"). The panel opens on the NEWEST page
// (`newest_first`, records newest first, read from the end of a stored journal), follows new
// records forward from `next_after` (`after`, oldest first), and "Show more" first renders
// what it holds, then pages backwards from `next_before` (`before`). It keeps at most
// `JOURNAL_KEEP` records from following (the oldest are dropped, and fetched again by Show more)
// and renders at most `shown` rows, so a long journal never becomes an unbounded table.

/** Records asked per forward request (the engine's default page). */
export const JOURNAL_REQUEST_LIMIT = 1000;
/** Forward requests per refresh at most; the next refresh goes on from the cursor. */
export const JOURNAL_MAX_REQUESTS = 20;
/** Records kept by following (the newest). */
export const JOURNAL_KEEP = 2000;
/** Rows rendered at first and added per "Show more"; also the size of a backwards page. */
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
  return {
    records: [], after: 0, before: null, older: false, truncated: false, notes: [], more: false,
  };
}

/** The filters as request keys (empty ones left out; the source is a prefix). */
function filterKeys(spec, filters) {
  const level = filters && filters.level ? String(filters.level).trim() : '';
  const source = filters && filters.source ? String(filters.source).trim() : '';
  if (level) spec.level = level;
  if (source) spec.source = source;
  return spec;
}

/** The forward request (`{after, limit, level?, source?}`): what came after the cursor. */
export function journalSpec(filters, after, limit = JOURNAL_REQUEST_LIMIT) {
  return filterKeys({ after: Math.max(0, Number(after) || 0), limit }, filters);
}

/**
 * The newest-first request (`{newest_first: true, limit, before?, level?, source?}`): the
 * newest records, or with `before` the next older page.
 */
export function newestSpec(filters, before = null, limit = JOURNAL_PAGE_ROWS) {
  const spec = { newest_first: true, limit };
  if (before != null) spec.before = Math.max(0, Number(before) || 0);
  return filterKeys(spec, filters);
}

/** Notes of `page` added to `notes` once. */
function mergeNotes(notes, page) {
  const out = notes.slice();
  for (const note of (page && page.notes) || []) if (!out.includes(note)) out.push(note);
  return out;
}

/** The view of the first, newest-first page (its records already newest first). */
export function startJournal(page) {
  const base = emptyJournal();
  const records = ((page && page.records) || []).filter((r) => r && typeof r.seq === 'number');
  return {
    ...base,
    records,
    after: page && typeof page.next_after === 'number' ? page.next_after : 0,
    before: page && typeof page.next_before === 'number' ? page.next_before : null,
    older: !!(page && page.more),
    truncated: !!(page && page.truncated),
    notes: mergeNotes([], page),
  };
}

/**
 * The view after one more FORWARD page (oldest first): its new records put in front (newest
 * first, a record already held is not added twice), the cursor moved, at most `keep` records
 * (the oldest dropped: they become older records again, `before` moves up). `truncated`
 * (older records gone from an in-memory journal) and the notes are kept once seen.
 */
export function addJournalPage(view, page, keep = JOURNAL_KEEP) {
  const base = view || emptyJournal();
  const records = (page && Array.isArray(page.records)) ? page.records : [];
  const held = base.records.length ? base.records[0].seq : base.after;
  const fresh = records.filter((r) => r && typeof r.seq === 'number' && r.seq > held).reverse();
  let all = fresh.concat(base.records);
  let { before, older } = base;
  if (all.length > keep) {
    all = all.slice(0, keep);
    before = all[all.length - 1].seq;
    older = true;
  }
  const next = page && typeof page.next_after === 'number' ? page.next_after : base.after;
  return {
    ...base,
    records: all,
    after: Math.max(base.after, next),
    before,
    older,
    truncated: base.truncated || !!(page && page.truncated),
    notes: mergeNotes(base.notes, page),
    more: !!(page && page.more),
  };
}

/** The view after one OLDER page (newest first): its records appended below the held ones. */
export function addOlderPage(view, page) {
  const base = view || emptyJournal();
  const oldest = base.records.length ? base.records[base.records.length - 1].seq : Infinity;
  const records = ((page && page.records) || [])
    .filter((r) => r && typeof r.seq === 'number' && r.seq < oldest);
  return {
    ...base,
    records: base.records.concat(records),
    before: page && typeof page.next_before === 'number' ? page.next_before : base.before,
    older: !!(page && page.more),
    truncated: base.truncated || !!(page && page.truncated),
    notes: mergeNotes(base.notes, page),
  };
}

/** Opens the journal: the newest page (`fetchPage(spec)` is the engine call). */
export async function openJournal(filters, fetchPage, limit = JOURNAL_PAGE_ROWS) {
  return startJournal(await fetchPage(newestSpec(filters, null, limit)));
}

/**
 * Reads the journal forward from the view's cursor: stops when no more follow or after
 * `maxRequests` requests (the next refresh goes on). Returns the new view.
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

/**
 * "Show more": `{view, shown}` with `JOURNAL_PAGE_ROWS` more rows: the held records first, an
 * older page from the engine (`before`) when they are all shown and older ones exist.
 */
export async function showMoreJournal(view, shown, filters, fetchPage) {
  const target = shown + JOURNAL_PAGE_ROWS;
  let current = view;
  if (current.records.length < target && current.older && current.before != null) {
    const page = await fetchPage(newestSpec(filters, current.before, target - current.records.length));
    current = addOlderPage(current, page);
  }
  return { view: current, shown: target };
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

/**
 * The fields as the CLI shows them (`JournalRecord::to_line`): the one-line ones as
 * `name=value, ...`; a value of several lines (a `recommendation`: a headline, then each command
 * on its own line under a `# comment`) on lines of its own below them, `name: ` before its first
 * line, every line indented by two spaces (the panel keeps the line breaks: `white-space:
 * pre-wrap`).
 */
export function fieldsText(fields) {
  if (!fields || typeof fields !== 'object') return '';
  const inline = [];
  const below = [];
  for (const [k, v] of Object.entries(fields)) {
    const text = valueText(v);
    if (text.includes('\n')) {
      const [first, ...rest] = text.split('\n');
      below.push([`  ${k}: ${first}`, ...rest.map((line) => `  ${line}`)].join('\n'));
    } else {
      inline.push(`${k}=${text}`);
    }
  }
  return [inline.join(', '), ...below].filter((part) => part !== '').join('\n');
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

/**
 * The line under the table: how many held rows are not shown, whether older records exist in
 * the journal, what is gone; `more` = "Show more" has something to show.
 */
export function journalFooter(view, shown) {
  const total = (view && view.records && view.records.length) || 0;
  const hidden = Math.max(0, total - shown);
  const older = !!(view && view.older);
  const parts = [];
  if (hidden) parts.push(`${hidden} more loaded`);
  if (older) parts.push('older records in the journal');
  if (view && view.truncated) parts.push('the oldest records are gone from the in-memory journal');
  return { hidden, more: hidden > 0 || older, text: parts.join(' · ') };
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
