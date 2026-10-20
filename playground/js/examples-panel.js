// The model of the Examples panel (components/examples-panel.js): the group tree built from
// js/examples.json, the plain-text search over titles and scripts with highlight pieces and a
// one-line script snippet, the flat order the keyboard walks, and the remembered panel state
// (collapsed groups, the last opened example, the scroll position). Pure functions, no DOM:
// tested by playground/tests/examples-panel.test.mjs.
//
// Data shape: an example has `group` and optionally `subgroup` (the GQL group is split into
// subgroups; the Rust tests and the MCP examples resource keep reading `group` alone).

/** The id of an example: unique since titles are unique within a group (no group name holds `::`). */
export function exampleId(ex) {
  return `${ex.group}::${ex.title}`;
}

/** The collapse key of a group (`"GQL"`) or a subgroup (`"GQL / Basics"`). */
export function sectionKey(group, subgroup) {
  return subgroup ? `${group} / ${subgroup}` : group;
}

/**
 * The examples as a tree in file order (groups and subgroups by first appearance):
 * `[{ key, title, items, subgroups: [{ key, title, items }] }]`. A group's own items (without a
 * subgroup) come before its subgroups.
 */
export function groupExamples(examples) {
  const groups = [];
  const byName = new Map();
  for (const ex of examples) {
    let g = byName.get(ex.group);
    if (!g) {
      g = { key: sectionKey(ex.group), title: ex.group, items: [], subgroups: [], byName: new Map() };
      byName.set(ex.group, g);
      groups.push(g);
    }
    if (!ex.subgroup) {
      g.items.push(ex);
      continue;
    }
    let s = g.byName.get(ex.subgroup);
    if (!s) {
      s = { key: sectionKey(ex.group, ex.subgroup), title: ex.subgroup, items: [] };
      g.byName.set(ex.subgroup, s);
      g.subgroups.push(s);
    }
    s.items.push(ex);
  }
  return groups.map(({ key, title, items, subgroups }) => ({ key, title, items, subgroups }));
}

/** The number of examples in a group, its subgroups included. */
export function groupSize(group) {
  return group.items.length + group.subgroups.reduce((n, s) => n + s.items.length, 0);
}

/** Collapses every run of whitespace (newlines too) into one space. */
function oneLine(s) {
  return s.replace(/\s+/g, ' ');
}

/**
 * Where `query` occurs in `text`, case-insensitive, as plain text (never a pattern): the index
 * of the first occurrence and its length in `text`, or null. An empty query matches nothing.
 */
export function findMatch(text, query) {
  if (!query) return null;
  const hay = text.toLowerCase();
  const needle = query.toLowerCase();
  // toLowerCase keeps the length of almost all text; where it does not (a few non-ASCII
  // letters), the indexes would not map back, so fall back to an exact-case search.
  if (hay.length !== text.length || needle.length !== query.length) {
    const at = text.indexOf(query);
    return at < 0 ? null : { index: at, length: query.length };
  }
  const at = hay.indexOf(needle);
  return at < 0 ? null : { index: at, length: query.length };
}

/** `text` split around a match: `{ before, match, after }`. */
export function splitAt(text, m) {
  return {
    before: text.slice(0, m.index),
    match: text.slice(m.index, m.index + m.length),
    after: text.slice(m.index + m.length),
  };
}

/**
 * A one-line snippet of `text` around the match `m`: up to `radius` characters on each side,
 * whitespace collapsed, `…` where text was cut. Returns `{ before, match, after }`.
 */
export function snippet(text, m, radius = 36) {
  const from = Math.max(0, m.index - radius);
  const to = Math.min(text.length, m.index + m.length + radius);
  let before = oneLine(text.slice(from, m.index));
  const match = oneLine(text.slice(m.index, m.index + m.length));
  let after = oneLine(text.slice(m.index + m.length, to));
  if (from === 0) before = before.trimStart();
  else before = `…${before}`;
  if (to === text.length) after = after.trimEnd();
  else after = `${after}…`;
  return { before, match, after };
}

/**
 * Searches the examples for `query` (trimmed; plain, case-insensitive substring) in the title
 * and the script. Returns a Map from example id to `{ title, snippet }`: `title` the title's
 * highlight pieces (or null when only the script matches), `snippet` the script's one-line
 * snippet pieces (or null when the script does not match). Null for an empty query.
 */
export function searchExamples(examples, query) {
  const q = (query || '').trim();
  if (!q) return null;
  const hits = new Map();
  for (const ex of examples) {
    const t = findMatch(ex.title, q);
    const s = findMatch(ex.script, q);
    if (!t && !s) continue;
    hits.set(exampleId(ex), { title: t ? splitAt(ex.title, t) : null, snippet: s ? snippet(ex.script, s) : null });
  }
  return hits;
}

/**
 * The tree as shown: without a search, every group with its collapsed flag; with search hits,
 * only the groups and subgroups holding hits, all expanded, each with only its hits.
 * Each section gets `collapsed` and `count` (the examples it shows, or would show).
 */
export function visibleTree(tree, collapsed, hits) {
  const isCollapsed = (key) => !hits && collapsed.has(key);
  const keep = (items) => (hits ? items.filter((ex) => hits.has(exampleId(ex))) : items);
  const out = [];
  for (const g of tree) {
    const items = keep(g.items);
    const subgroups = g.subgroups
      .map((s) => ({ key: s.key, title: s.title, items: keep(s.items), collapsed: isCollapsed(s.key) }))
      .filter((s) => s.items.length);
    for (const s of subgroups) s.count = s.items.length;
    const count = items.length + subgroups.reduce((n, s) => n + s.count, 0);
    if (!count) continue;
    out.push({ key: g.key, title: g.title, items, subgroups, collapsed: isCollapsed(g.key), count });
  }
  return out;
}

/** The examples the keyboard walks (Up/Down): the shown tree, collapsed sections skipped. */
export function flatVisible(shown) {
  const list = [];
  for (const g of shown) {
    if (g.collapsed) continue;
    list.push(...g.items);
    for (const s of g.subgroups) if (!s.collapsed) list.push(...s.items);
  }
  return list;
}

/**
 * The next example for the arrow keys: `delta` +1 or -1 from `currentId` in `list` (ids).
 * From nothing (`currentId` null), Down starts at `startId`'s successor when it is shown (the
 * example after the last opened one), else at the first; Up from the first returns null (back to
 * the search field).
 */
export function stepSelection(list, currentId, delta, startId = null) {
  if (!list.length) return null;
  if (currentId === null) {
    if (delta < 0) return null;
    const at = startId === null ? -1 : list.indexOf(startId);
    return list[at >= 0 ? Math.min(at + 1, list.length - 1) : 0];
  }
  const at = list.indexOf(currentId);
  if (at < 0) return list[0];
  const next = at + delta;
  if (next < 0) return null;
  return list[Math.min(next, list.length - 1)];
}

const STATE_KEY = 'examples.panel';

/**
 * The remembered panel state over a storage with `get(key, fallback)` / `set(key, value)`
 * (js/util.js `storage`, which never throws). A broken stored value falls back to the defaults.
 */
export function loadPanelState(store) {
  let raw = null;
  try {
    raw = store.get(STATE_KEY, null);
  } catch (e) {
    raw = null;
  }
  const ok = raw && typeof raw === 'object';
  const collapsed = ok && Array.isArray(raw.collapsed) ? raw.collapsed.filter((k) => typeof k === 'string') : [];
  return {
    collapsed: new Set(collapsed),
    lastId: ok && typeof raw.lastId === 'string' ? raw.lastId : null,
    scrollTop: ok && Number.isFinite(raw.scrollTop) && raw.scrollTop > 0 ? raw.scrollTop : 0,
  };
}

/** Stores the panel state (see `loadPanelState`). */
export function savePanelState(store, s) {
  try {
    store.set(STATE_KEY, { collapsed: [...s.collapsed], lastId: s.lastId, scrollTop: Math.round(s.scrollTop || 0) });
  } catch (e) {
    /* storage unavailable: the state lasts for this page view */
  }
}
