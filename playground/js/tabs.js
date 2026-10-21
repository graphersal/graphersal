// Query tabs: pure state logic, no Vue, DOM or engine (store.js wraps it in its reactive state,
// tests/tabs.test.mjs runs it under Node).
//
// A tab owns its script, its name, its run options and the outcome of its last run (result,
// profile, the scripts to re-run). The graph is shared by all tabs (one engine session), so a run
// that changes the graph marks the results of the other tabs as possibly stale. Only the
// definition of the tabs is persisted (id, name, script, example provenance, run options, the
// active tab); results are not (they can be large).
//
// A tab has a kind: 'query' (the default: the editor) or 'ai' (an AI Chat tab, js/ai-chat.js:
// its transcript and conversation id persist with it; it has no script and never takes an
// example). A query tab has a language: 'rhai' (the default) or 'gql' (its script is plain GQL
// and `params` the JSON text of its Parameters field; js/gql-tab.js). Both persist with the tab
// (a Rhai tab writes neither, so older playgrounds read the tabs as before).

import { createChat, serializeChat } from './ai-chat.js';
import { tabLanguage } from './gql-tab.js';

export const DEFAULT_SCRIPT = 'g.v().has_label("person").values("name")';
export const DEFAULT_RUN_OPTIONS = Object.freeze({ profile: true, memory: false });
export const MAX_TABS = 30;
export const MAX_NAME = 40;
/** The tab kinds. */
export const TAB_KINDS = Object.freeze(['query', 'ai']);

/** A fresh tab (runtime fields included). */
export function createTab(id, {
  name, script = '', example = null, runOptions, autoName = true, kind = 'query', chat = null, language = 'rhai', params = '',
} = {}) {
  const isChat = kind === 'ai';
  const lang = isChat ? 'rhai' : tabLanguage(language);
  return {
    id,
    kind: isChat ? 'ai' : 'query',
    language: lang, // 'rhai' | 'gql' (js/gql-tab.js)
    params: lang === 'gql' && typeof params === 'string' ? params : '', // a GQL tab's Parameters field (JSON text)
    name: name || `${isChat ? 'Chat' : 'Query'} ${id}`,
    autoName, // true until the user renames the tab: an example may then rename it
    script,
    // The example this tab's text came from: {title, script}. The tab still holds the example
    // "unmodified" while `script === example.script`; any edit makes it user text.
    example,
    runOptions: { ...DEFAULT_RUN_OPTIONS, ...(runOptions || {}) },
    result: null, // the last ExecuteResponse of this tab (with run_id)
    profile: null, // {metrics, run, script, at, error, terminal}: its last run that returned metrics
    lastScript: '', // the script of its last run (Results refresh)
    profileScript: '', // the script of its last profile (Profile refresh)
    stale: false, // another tab changed the graph after this tab's last result
    chat: isChat ? createChat(chat) : null, // an AI Chat tab's state (js/ai-chat.js)
  };
}

/** Whether an example may replace the tab's text without losing anything the user wrote. */
export function holdsNoUserText(tab) {
  if (tab.kind === 'ai') return false; // a chat is never replaced by an example
  if (!tab.script.trim()) return true;
  return !!tab.example && tab.script === tab.example.script;
}

function nextId(ts) {
  return ts.tabs.reduce((m, t) => Math.max(m, t.id), 0) + 1;
}

/** The smallest "Query N" ("Chat N" for an AI Chat tab) that no tab uses as its name. */
function freeName(ts, kind = 'query') {
  const base = kind === 'ai' ? 'Chat' : 'Query';
  const used = new Set(ts.tabs.map((t) => t.name));
  for (let n = 1; ; n++) if (!used.has(`${base} ${n}`)) return `${base} ${n}`;
}

/**
 * The tab state restored from storage. `saved` is what `serializeTabs` wrote (or anything else:
 * invalid input yields one default tab). `legacy` ({script, runOptions}) seeds the first tab when
 * nothing was saved yet (the single-editor playground stored those two keys).
 */
export function restoreTabs(saved, legacy = {}) {
  const ts = { tabs: [], activeId: 0 };
  const list = saved && Array.isArray(saved.tabs) ? saved.tabs.slice(0, MAX_TABS) : [];
  for (const raw of list) {
    if (!raw || typeof raw !== 'object' || typeof raw.script !== 'string') continue;
    const id = Number.isInteger(raw.id) && raw.id > 0 && !ts.tabs.some((t) => t.id === raw.id) ? raw.id : nextId(ts);
    const example = raw.example && typeof raw.example.script === 'string'
      ? { title: String(raw.example.title || ''), script: raw.example.script } : null;
    ts.tabs.push(createTab(id, {
      name: typeof raw.name === 'string' && raw.name.trim() ? raw.name.trim().slice(0, MAX_NAME) : undefined,
      script: raw.script,
      example,
      runOptions: raw.runOptions && typeof raw.runOptions === 'object' ? raw.runOptions : undefined,
      autoName: raw.autoName !== false,
      kind: raw.kind === 'ai' ? 'ai' : 'query',
      chat: raw.kind === 'ai' ? raw.chat : null,
      language: raw.language,
      params: raw.params,
    }));
  }
  if (!ts.tabs.length) {
    const script = typeof legacy.script === 'string' ? legacy.script : DEFAULT_SCRIPT;
    ts.tabs.push(createTab(1, { name: 'Query 1', script, runOptions: legacy.runOptions }));
  }
  ts.activeId = saved && ts.tabs.some((t) => t.id === saved.active) ? saved.active : ts.tabs[0].id;
  return ts;
}

/** What is persisted: the tab definitions and the active tab, never results (a chat's transcript is kept). */
export function serializeTabs(ts) {
  return {
    tabs: ts.tabs.map((t) => {
      const out = {
        id: t.id, name: t.name, autoName: t.autoName, script: t.script, example: t.example, runOptions: t.runOptions,
      };
      if (t.kind === 'ai') {
        out.kind = 'ai';
        out.chat = serializeChat(t.chat);
      } else if (t.language === 'gql') {
        out.language = 'gql';
        out.params = t.params;
      }
      return out;
    }),
    active: ts.activeId,
  };
}

export function activeTab(ts) {
  return ts.tabs.find((t) => t.id === ts.activeId) || ts.tabs[0];
}

export function findTab(ts, id) {
  return ts.tabs.find((t) => t.id === id) || null;
}

/**
 * Adds a tab after the active one and activates it (`opts.kind: 'ai'` for an AI Chat tab). New
 * tabs inherit the active tab's run options. Returns the new tab, or null at MAX_TABS.
 */
export function addTab(ts, opts = {}) {
  if (ts.tabs.length >= MAX_TABS) return null;
  const current = activeTab(ts);
  const tab = createTab(nextId(ts), {
    runOptions: current ? current.runOptions : undefined,
    ...opts,
    name: opts.name || freeName(ts, opts.kind),
  });
  const at = current ? ts.tabs.indexOf(current) + 1 : ts.tabs.length;
  ts.tabs.splice(at, 0, tab);
  ts.activeId = tab.id;
  return tab;
}

/**
 * A copy of a tab's script, name and options (not its result) next to it, activated. A chat
 * tab's copy keeps the transcript as history, without the conversation (its next Send starts one).
 */
export function duplicateTab(ts, id) {
  const src = findTab(ts, id);
  if (!src) return null;
  ts.activeId = src.id;
  const base = src.name.replace(/ \(copy(?: \d+)?\)$/, '');
  const used = new Set(ts.tabs.map((t) => t.name));
  let name = `${base} (copy)`;
  for (let n = 2; used.has(name); n++) name = `${base} (copy ${n})`;
  return addTab(ts, {
    name: name.slice(0, MAX_NAME), autoName: false, script: src.script, example: src.example, runOptions: src.runOptions,
    kind: src.kind,
    language: src.language,
    params: src.params,
    chat: src.kind === 'ai' ? { items: serializeChat(src.chat).items } : null,
  });
}

/**
 * Closes a tab; the last tab is never closed (it is emptied instead: an empty query tab, also
 * for a chat tab). The neighbour to the right
 * (or left) becomes active when the active tab closes. Returns the tab that was removed.
 */
export function closeTab(ts, id) {
  const i = ts.tabs.findIndex((t) => t.id === id);
  if (i < 0) return null;
  if (ts.tabs.length === 1) {
    const only = ts.tabs[0];
    const copy = { ...only };
    const wasChat = only.kind === 'ai';
    Object.assign(only, createTab(only.id, {
      name: wasChat ? 'Query 1' : only.name, autoName: wasChat || only.autoName, runOptions: only.runOptions,
    }));
    return copy;
  }
  const [removed] = ts.tabs.splice(i, 1);
  if (ts.activeId === id) ts.activeId = ts.tabs[Math.min(i, ts.tabs.length - 1)].id;
  return removed;
}

/** Closes every tab except `id`. Returns the removed tabs. */
export function closeOtherTabs(ts, id) {
  const keep = findTab(ts, id);
  if (!keep) return [];
  const removed = ts.tabs.filter((t) => t !== keep);
  ts.tabs.splice(0, ts.tabs.length, keep);
  ts.activeId = keep.id;
  return removed;
}

export function activateTab(ts, id) {
  if (findTab(ts, id)) ts.activeId = id;
}

/** Activates the n-th tab (0-based); n = -1 is the last one. */
export function activateIndex(ts, n) {
  const tab = n === -1 ? ts.tabs[ts.tabs.length - 1] : ts.tabs[n];
  if (tab) ts.activeId = tab.id;
}

/** Activates the previous (-1) or next (+1) tab, wrapping around. */
export function activateRelative(ts, delta) {
  const i = ts.tabs.indexOf(activeTab(ts));
  const n = ts.tabs.length;
  ts.activeId = ts.tabs[(((i + delta) % n) + n) % n].id;
}

/** Renames a tab; a blank name keeps the old one. Returns whether it changed. */
export function renameTab(ts, id, name) {
  const tab = findTab(ts, id);
  const clean = String(name || '').replace(/\s+/g, ' ').trim().slice(0, MAX_NAME);
  if (!tab || !clean || clean === tab.name) return false;
  tab.name = clean;
  tab.autoName = false;
  return true;
}

/** Moves a tab to another position (drag and drop). */
export function moveTab(ts, id, toIndex) {
  const i = ts.tabs.findIndex((t) => t.id === id);
  if (i < 0) return;
  const [tab] = ts.tabs.splice(i, 1);
  ts.tabs.splice(Math.max(0, Math.min(toIndex, ts.tabs.length)), 0, tab);
}

/** The editor changed a tab's text. */
export function setScript(ts, id, script) {
  const tab = findTab(ts, id);
  if (tab) tab.script = script;
}

/** The Parameters field of a GQL tab changed. */
export function setParams(ts, id, params) {
  const tab = findTab(ts, id);
  if (tab && tab.language === 'gql') tab.params = String(params ?? '');
}

/**
 * Switches a query tab's language with its rewritten text and parameters (`switchTabLanguage`
 * of js/gql-tab.js gives them). The tab keeps its example provenance only while its text is
 * still the example's.
 */
export function setLanguage(ts, id, language, { script, params = '' }) {
  const tab = findTab(ts, id);
  if (!tab || tab.kind === 'ai') return;
  tab.language = tabLanguage(language);
  tab.script = script;
  tab.params = tab.language === 'gql' ? params : '';
  if (tab.example && tab.example.script !== script) tab.example = null;
}

/**
 * Opens an example: in the active tab when it is empty or still holds an unmodified example,
 * otherwise in a new tab, so user text is never overwritten. A tab that the user did not rename
 * takes the example's title as its name. `ex.language` 'gql' opens it as a GQL tab with
 * `ex.params` (the JSON text of its Parameters field); a reused tab takes the example's language.
 * Returns `{tab, reused}` (tab is null at MAX_TABS).
 */
export function openExample(ts, ex) {
  const current = activeTab(ts);
  const language = tabLanguage(ex.language);
  const params = language === 'gql' && typeof ex.params === 'string' ? ex.params : '';
  const example = { title: String(ex.title || ''), script: ex.script };
  const name = example.title ? example.title.slice(0, MAX_NAME) : undefined;
  if (current && current.kind !== 'ai' && holdsNoUserText(current)) {
    current.script = ex.script;
    current.example = example;
    current.language = language;
    current.params = params;
    if (current.autoName && name) current.name = name;
    return { tab: current, reused: true };
  }
  const tab = addTab(ts, { script: ex.script, example, name, autoName: true, language, params });
  return { tab, reused: false };
}

/**
 * Records the outcome of a run in its tab. A run that changed the graph marks every other tab
 * that shows a result as possibly stale; the tab's own result is fresh again.
 */
export function recordRun(ts, id, { result, profile, script, mutated }) {
  const tab = findTab(ts, id);
  if (!tab) return;
  tab.result = result;
  tab.lastScript = script;
  tab.stale = false;
  if (profile) {
    tab.profile = profile;
    tab.profileScript = script;
  }
  if (mutated) markOthersStale(ts, id);
}

/** Marks the results of every tab except `id` (null: all tabs) as possibly stale. */
export function markOthersStale(ts, id) {
  for (const t of ts.tabs) if (t.id !== id && (t.result || t.profile)) t.stale = true;
}

/** A new graph was loaded: no tab's result refers to it. */
export function clearResults(ts) {
  for (const t of ts.tabs) {
    t.result = null;
    t.profile = null;
    t.stale = false;
  }
}
