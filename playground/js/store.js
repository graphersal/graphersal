// Application state and actions. Components read `state` and call the actions; only this module
// talks to the engine for graph loading and query execution. The query tabs' logic lives in
// tabs.js (pure, tested under Node); this module wraps it in the reactive state and persists it.

import { loadBuildInfo, versionLabel } from './build-info.js';
import { reactive, markRaw } from 'vue';
import { getEngine } from './engine/index.js';
import { DEFAULT_LIMITS } from './engine/engine.js';
import { StoppedError } from './engine/wasm-engine.js';
import { storage, debounce } from './util.js';
import { downloadBlob, savedGraphFile, schemaFile, isEmptySchema, catalogFile, isEmptyCatalog } from './export.js';
import * as tabs from './tabs.js';
import { createReadyGate } from './ready.js';
import { changedData } from './statistics.js';
import { createPoller, readChanges } from './change-feed.js';
import { createBurst, burstAnnouncement } from './change-burst.js';
import { LayoutJobs } from './graph/layout-jobs.js';
import { ChatController } from './ai-chat.js';
import { runText, mapErrorPosition } from './editor/run-selection.js';
import { taskStartText, taskEndText, snapshotFileName } from './store-model.js';
import { VALUE_MAX_CHARS } from './value-view.js';
import { MAX_SHARE_ELEMENTS, isUnchangedSample, shareLink, encodeLinkData } from './link.js';
import {
  GQL_TAB_TEMPLATE, gqlCallText, gqlErrorSpan, switchTabLanguage, withQueryParams, tabLanguage,
} from './gql-tab.js';
import { mapPosition } from './editor/run-selection.js';
import { keysBySchema } from './editor/gql-complete.js';
import { normalizeSizeBy } from './graph/size-by.js';

const HISTORY_MAX = 100;

export const DEFAULT_SETTINGS = Object.freeze({
  maxRows: DEFAULT_LIMITS.maxRows,
  maxItems: DEFAULT_LIMITS.maxItems,
  noLimit: false,
  spelling: 'snake', // step names in the profile and errors: 'snake' (has_label) or 'camel' (hasLabel)
  // The drawings fetched from the engine (whole graph, neighbourhood, sample): measured in GR4
  // (README "Graph drawing"): view + transfer + engine layout of 30 000 elements about 3 s in
  // the worker; larger graphs are reached through the Labels and Neighbourhood views.
  graphMaxNodes: 10000,
  graphMaxEdges: 20000,
  graphResultMax: 50000, // the result subgraph: vertices + edges (subgraph_max_elements; at most 200 000)
  graphTier: 'auto', // the Graph panel's tier switch: 'auto' | 'detailed' | 'fast' (js/graph/tiers.js)
  valueMaxChars: VALUE_MAX_CHARS.default, // element views collapse longer values (js/value-view.js); 0 = off
});

const media = typeof window !== 'undefined' && window.matchMedia ? window.matchMedia('(prefers-color-scheme: dark)') : null;

/**
 * The saved tabs, or one tab seeded from the single-editor playground's `script`/`runOptions`
 * keys (those are removed once the tabs are saved).
 */
function restoredTabs() {
  const ts = tabs.restoreTabs(storage.get('tabs', null), {
    script: storage.get('script', undefined),
    runOptions: storage.get('runOptions', undefined),
  });
  return { tabs: ts.tabs, activeId: ts.activeId };
}

export const state = reactive({
  ready: false,
  engine: null,
  engineInfo: null,
  // The header's version (js/build-info.js `versionLabel`): {text, title}, or null.
  versionLabel: null,
  engineMissing: false, // the WebAssembly build is not there: the page explains how to build it
  route: 'start',
  graph: null,
  graphVersion: 0,
  // Bumped (with graphVersion) when another graph REPLACES the shown one: a load, Stop's engine
  // restart, the dev server's graph on start, a read-only view, back to live, a rollback, an
  // attic restore, a database switch. The Schema panel infers again then (once, when shown);
  // after a plain change (graphVersion only) it shows "may be outdated" instead.
  graphEpoch: 0,
  dataVersion: 0, // bumped when the data may have changed: the Statistics tab reloads
  loadingGraph: false,
  loadError: null,
  theme: storage.get('theme', 'system'),
  systemDark: media ? media.matches : false,
  settings: { ...DEFAULT_SETTINGS, ...storage.get('settings', {}) },
  // [{script, ok, at, tab, language?, params?}], newest first, shared by all tabs (`language`
  // 'gql' with the Parameters text for a run of a GQL tab)
  history: storage.get('history', []),
  // Query tabs (tabs.js): `tabs` and `activeId`. Each tab owns its script, its run options (the
  // Query toolbar's Profile/Memory toggles), its last result and profile, and a stale flag.
  ...restoredTabs(),
  /** The active tab. */
  get tab() {
    return tabs.activeTab(this);
  },
  runCount: 0,
  running: false,
  runningTabId: null, // the tab whose run is in progress (its result goes there even after a switch)
  runningElapsedMs: 0, // live elapsed time of the running query, updated while running is true
  runAllActive: false,
  stopping: false,
  layoutsRunning: 0, // engine layouts of the drawings in progress (layoutJobs): Stop cancels them
  saving: false,
  // The store (the dev server's Store directory, or the browser's store in memory): the
  // read-only view of a past state served instead of the live graph ({commitSeq, label,
  // timeText, target}), or null.
  storeView: null,
  storeVersion: 0, // bumped when the store changed (rollback, restore): the Store menu reloads
  storeBusy: false, // a store action (view, rollback, ...) is in progress
  // The store action the Store menu shows as running (`runStoreTask`): {id, startedAt, now} in
  // milliseconds (`now` ticks while it runs), or null; one at a time.
  storeTask: null,
  storeAnnounce: '', // the Store menu's polite live region: "Verifying the store started." ...
  // The change feed's burst banner (js/change-burst.js `banner()`): {kind: 'burst'|'paused',
  // text}, or null in normal mode; `changeBurstAnnounce` is its polite live region's text.
  changeBurst: null,
  changeBurstAnnounce: '',
  // The saved queries of the graph (engine.listQueries), reloaded with the graph
  // (refreshSavedQueries); null before the first load.
  jobs: null, // the engine's jobs() answer: {timeZone, store, jobs}
  jobsError: null,
  savedQueries: null,
  savedQueriesError: null,
  // The compression rules of the graph with their stats (engine.listCompressions), reloaded with
  // the graph and when the Compression manager opens (refreshCompressions); null before.
  compressions: null,
  compressionsError: null,
  // The property indexes of the graph with their stats (engine.listIndexes), reloaded with the
  // graph and when the Property indexes manager opens (refreshIndexes); null before.
  indexes: null,
  indexesError: null,
  // The manager the Catalog dropdown opened: 'queries' | 'compression' | null (the Workspace
  // hosts the dialogs; openCatalog sets it, the dialog clears it when it closes).
  catalogDialog: null,
  optimizeN: 0, // bumped by every openOptimize: the Optimize panel opens
  catalogDialogN: 0, // bumped by every openCatalog, so opening the same manager again reopens it
  catalogDialogScript: null, // the tab text "Save as query…" hands to the saved-query editor
  focusPanel: null, // {panel, tab, n}: a request to reveal a panel/tab
  graphRequest: null, // {source, ids, n}: a request to the Graph panel (showNeighbourhood)
  jumpTo: null, // {line, column, n}: a request to move the editor cursor
  // The graph's vocabulary for the GQL completion (`gqlVocabulary`): {vertexLabels, edgeLabels,
  // keys} of the data version `version`; not reactive (markRaw), read by the completion source.
  gqlVocab: null,
  toasts: [],
});

export function isDark() {
  return state.theme === 'dark' || (state.theme === 'system' && state.systemDark);
}

// Opened by init() once the engine exists (or is known to be missing, or failed to start).
const engineGate = createReadyGate();

/**
 * Resolves once init() has created the engine (or found it missing). Every engine-dependent
 * action awaits it, so an early call (a hash navigation while the WebAssembly module loads) waits
 * instead of calling a null engine.
 */
export function engineReady() {
  return engineGate.ready;
}

/** Whether the engine is the CLI's dev server (HTTP) rather than the WebAssembly worker. */
export function isServerMode() {
  return !!(state.engineInfo && state.engineInfo.server);
}

export async function init() {
  if (media) {
    const update = () => { state.systemDark = media.matches; };
    if (media.addEventListener) media.addEventListener('change', update);
    else if (media.addListener) media.addListener(update);
  }
  applyTheme();
  try {
    const { engine, missing } = await getEngine();
    state.engineMissing = missing;
    if (engine) {
      state.engine = markRaw(engine);
      state.engineInfo = await engine.info();
      state.storeView = state.engineInfo.view || null;
      const build = state.engineInfo.server ? null : await loadBuildInfo();
      state.versionLabel = versionLabel(state.engineInfo.version, build);
    }
  } finally {
    engineGate.open();
  }
  state.ready = true;
}

export function setTheme(theme) {
  state.theme = theme;
  storage.set('theme', theme);
  applyTheme();
}

export function cycleTheme() {
  const order = ['system', 'light', 'dark'];
  setTheme(order[(order.indexOf(state.theme) + 1) % order.length]);
}

function applyTheme() {
  const root = document.documentElement;
  if (state.theme === 'system') delete root.dataset.theme;
  else root.dataset.theme = state.theme;
}

export function saveSettings(settings) {
  state.settings = { ...state.settings, ...settings };
  storage.set('settings', state.settings);
}

// ------------------------------------------------------------------ query tabs

/** Saves the tab definitions (never the results). */
export function persistTabs() {
  storage.set('tabs', tabs.serializeTabs(state));
  storage.remove('script');
  storage.remove('runOptions');
}
const persistTabsSoon = debounce(persistTabs, 300);

const tabLimitToast = () => toast(`At most ${tabs.MAX_TABS} tabs; close one first.`, 'info');

/** The editor changed the text of a tab. */
export function saveScript(script, tabId = state.activeId) {
  tabs.setScript(state, tabId, script);
  persistTabsSoon();
}

export function switchTab(id) {
  tabs.activateTab(state, id);
  persistTabs();
}

/** Alt+Shift+1..9: the n-th tab (1-based); 9 is always the last tab, as in browsers. */
export function switchTabNumber(n) {
  tabs.activateIndex(state, n === 9 ? -1 : n - 1);
  persistTabs();
}

export function switchTabRelative(delta) {
  tabs.activateRelative(state, delta);
  persistTabs();
}

export function newTab(opts = {}) {
  const tab = tabs.addTab(state, opts);
  if (!tab) tabLimitToast();
  persistTabs();
  return tab;
}

export function duplicateTab(id = state.activeId) {
  if (!tabs.duplicateTab(state, id)) tabLimitToast();
  persistTabs();
}

export function renameTab(id, name) {
  if (tabs.renameTab(state, id, name)) persistTabs();
}

export function moveTab(id, toIndex) {
  tabs.moveTab(state, id, toIndex);
  persistTabs();
}

/**
 * Whether closing the tab would lose text the user wrote: not empty, not an unmodified example,
 * and not in the query history (every run is).
 */
export function wouldLoseText(tab) {
  if (tabs.holdsNoUserText(tab)) return false;
  const text = tab.script.trim();
  return !state.history.some((h) => h.script === text);
}

/** Whether closing the tab would lose an AI Chat transcript. */
function wouldLoseChat(tab) {
  return tab.kind === 'ai' && !!tab.chat && tab.chat.items.length > 0;
}

/**
 * Closes a tab (asks first when its text or chat transcript would be lost). The last tab is
 * emptied instead. A chat tab's poll loop ends and the conversation this page created is deleted.
 */
export function closeTab(id = state.activeId) {
  const tab = tabs.findTab(state, id);
  if (!tab) return;
  if (tab.id === state.runningTabId) {
    toast('This tab is running a query; stop it first.', 'info');
    return;
  }
  if (wouldLoseText(tab) && !window.confirm(`Close "${tab.name}"? Its text was never run, so it is not in the history.`)) return;
  if (wouldLoseChat(tab) && !window.confirm(`Close "${tab.name}"? Its chat transcript is deleted with the tab.`)) return;
  disposeChat(tab.id);
  tabs.closeTab(state, id);
  persistTabs();
}

export function closeOtherTabs(id = state.activeId) {
  const others = state.tabs.filter((t) => t.id !== id);
  if (!others.length) return;
  if (others.some((t) => t.id === state.runningTabId)) {
    toast('Another tab is running a query; stop it first.', 'info');
    return;
  }
  const losing = others.filter((t) => wouldLoseText(t) || wouldLoseChat(t)).length;
  if (losing && !window.confirm(`Close ${others.length} other tab${others.length === 1 ? '' : 's'}? ` +
      `${losing} of them hold${losing === 1 ? 's' : ''} text that was never run or a chat transcript.`)) return;
  for (const t of others) disposeChat(t.id);
  tabs.closeOtherTabs(state, id);
  persistTabs();
}

/**
 * Puts a script into the editor without overwriting user text: the active tab is reused when it
 * is empty or still holds an unmodified example (or history entry), otherwise a new tab opens.
 * `title` names a tab the user did not rename. Returns the tab, or null at the tab limit.
 */
export function openScript({ title = '', script, language = 'rhai', params = '' }) {
  const { tab, reused } = tabs.openExample(state, { title, script, language, params });
  if (!tab) tabLimitToast();
  else if (!reused) toast(`Opened in a new tab, "${tab.name}": the current tab holds your own text.`, 'info', 3500);
  persistTabs();
  return tab;
}

/** A new GQL query tab (the "+" menu's "GQL query"), with a starting query. */
export function newGqlTab() {
  return newTab({ language: 'gql', script: GQL_TAB_TEMPLATE });
}

/** The Parameters field of a GQL tab changed. */
export function saveParams(params, tabId = state.activeId) {
  tabs.setParams(state, tabId, params);
  persistTabsSoon();
}

/**
 * The editor's Rhai / GQL switch: the tab's text is rewritten like the saved-query editor's switch
 * (js/gql-tab.js `switchTabLanguage`). Returns false (with a toast) when it cannot switch: GQL ->
 * Rhai with a Parameters field that is not a JSON map.
 */
export function setTabLanguage(language, tabId = state.activeId) {
  const tab = tabs.findTab(state, tabId);
  if (!tab || tab.kind === 'ai' || tab.language === tabLanguage(language)) return false;
  const next = switchTabLanguage({ script: tab.script, params: tab.params }, language);
  if (next.error) {
    toast(`${next.error}. Fix the Parameters field to switch to Rhai.`, 'error', 7000);
    return false;
  }
  tabs.setLanguage(state, tab.id, language, next);
  persistTabs();
  return true;
}

/** "+ Add $parameters": an entry for every `$name` the GQL tab's query reads that its field lacks. */
export function addQueryParams(tabId = state.activeId) {
  const tab = tabs.findTab(state, tabId);
  if (!tab || tab.language !== 'gql') return;
  const next = withQueryParams(tab.params, tab.script);
  if (next.error) {
    toast(`${next.error}.`, 'error', 7000);
    return;
  }
  if (!next.added.length) {
    toast('Every $parameter of the query is in the Parameters field already.', 'info', 3500);
    return;
  }
  saveParams(next.text, tab.id);
}

/** A Query toolbar toggle (Profile, Memory) of the active tab. */
export function setRunOption(key, value) {
  const tab = state.tab;
  tab.runOptions = { ...tab.runOptions, [key]: value };
  persistTabs();
}

// ------------------------------------------------------------------ engine

/**
 * Loads a graph through the engine. Returns true on success. Every tab's result is cleared (it
 * referred to the old graph). A graph file starts without the previous graph's schema; its
 * optional `schemaFile` is set first and the data imported against it.
 * @param {{kind:string, file?:File, schemaFile?:File}} request
 */
export async function loadGraph(request) {
  state.loadingGraph = true;
  state.loadError = null;
  try {
    await engineReady();
    if (!state.engine) throw new Error('The engine is not available');
    const info = await state.engine.loadGraph(request);
    state.graph = info;
    state.graphEpoch++;
    state.graphVersion++;
    state.dataVersion++;
    // In the browser a loaded graph is a new store in memory (commit 0 = this state).
    state.storeView = null;
    state.storeVersion++;
    tabs.clearResults(state);
    if (request.kind !== 'file') storage.set('lastGraph', request.kind);
    else storage.remove('lastGraph');
    if (info.warnings && info.warnings.length) toast(info.warnings.join(' '), 'info');
    return true;
  } catch (err) {
    state.loadError = err && err.message ? err.message : String(err);
    return false;
  } finally {
    state.loadingGraph = false;
  }
}

/**
 * Runs a tab's script (default: the active tab's) through `engine.execute` and records it in the
 * history with the tab's name. A traversal without a terminal returns results and (with the
 * tab's Profile toggle) metrics in one run. The result goes to the tab that started the run, also
 * when another tab is active by the time it ends; the tab's profile follows every run that
 * returns metrics. A run that changed the graph marks the other tabs' results as possibly stale.
 * Resolves to the response, or null when nothing ran.
 *
 * `selection` (`{lines, offset}`, from `runText` of js/editor/run-selection.js) says the script is
 * the editor's selection: the result carries it (`res.selection`, shown as "selection (N lines)")
 * and an error position is moved by its `offset` into the full editor text. Without it the script
 * is the whole tab, and the position is moved past the leading blank lines the engine never sees.
 */
export async function run(script, tabId = state.activeId, { selection = null } = {}) {
  const tab = tabs.findTab(state, tabId);
  if (!tab) return null;
  const source = script ?? tab.script;
  const text = source.trim();
  const offset = selection ? selection.offset : runText(source).offset;
  if (!text || state.running || !state.engine) return null;
  // A GQL tab runs `g.gql("<text>", #{params})` (js/gql-tab.js); its error positions are those
  // of the GQL text, never of that wrapper.
  const gql = tab.language === 'gql';
  let engineScript = text;
  if (gql) {
    const call = gqlCallText(text, tab.params);
    if (call.error) {
      tabs.recordRun(state, tab.id, {
        result: markRaw(failedResult({ message: call.error, help: 'The Parameters field under the editor holds a JSON map, for example {"min": 30, "names": ["josh", "vadas"]}; $min and $names read its values.' }, selection, ++state.runCount)),
        profile: null,
        script: text,
        mutated: false,
      });
      reveal('results');
      return tab.result;
    }
    engineScript = call.script;
  }
  state.running = true;
  state.runningTabId = tab.id;
  state.runningElapsedMs = 0;
  const startedAt = typeof performance !== 'undefined' && performance.now ? performance.now() : Date.now();
  const timer = setInterval(() => {
    const now = typeof performance !== 'undefined' && performance.now ? performance.now() : Date.now();
    state.runningElapsedMs = now - startedAt;
  }, 100);
  try {
    const { maxRows, maxItems, noLimit, spelling, graphResultMax } = state.settings;
    const { profile, memory } = tab.runOptions;
    const res = await state.engine.execute(engineScript, {
      profile,
      profile_types: memory ? ['memory'] : [],
      max_rows: maxRows,
      max_items: maxItems,
      no_limit: noLimit,
      spelling: spelling === 'camel' ? 'camel' : 'snake',
      subgraph_max_elements: graphResultMax,
      // The Graph panel sizes vertices by degree (Display menu, js/graph/size-by.js): only then.
      subgraph_degrees: normalizeSizeBy(storage.get('graph.sizeBy', 'off')) !== 'off',
    });
    const runId = ++state.runCount;
    res.run_id = runId;
    const ex = res.execution;
    if (ex.error) ex.error = gql ? gqlError(ex.error, offset) : mapErrorPosition(ex.error, offset);
    if (selection) res.selection = { lines: selection.lines, offset: selection.offset };
    tabs.recordRun(state, tab.id, {
      result: markRaw(res),
      profile: ex.profile
        ? markRaw({
          metrics: ex.profile, run: runId, script: text, at: Date.now(), error: ex.error, terminal: res.terminal,
          selection: res.selection || null,
        })
        : null,
      script: text,
      mutated: res.mutated,
    });
    if (tab.id === state.activeId && !state.runAllActive) {
      if (res.terminal === 'profile') reveal('profile');
      else reveal('results', res.ok ? res.preferred_view : null);
    }
    if (res.graph) state.graph = res.graph;
    // A run that changed the graph (data or schema: `res.changes`) redraws the Graph and
    // Schema panels.
    if (res.mutated) state.graphVersion++;
    if (changedData(res)) state.dataVersion++;
    addHistory(text, res.ok, tab.name, gql ? { language: 'gql', params: tab.params } : null);
    return res;
  } catch (err) {
    const stopped = err instanceof StoppedError;
    tab.lastScript = text;
    tab.stale = false;
    tab.result = markRaw({
      ok: false,
      terminal: 'unknown',
      mutated: false,
      stopped,
      execution: {
        results: [], profile: null,
        error: stopped
          ? { message: 'Stopped. The engine was restarted and the graph restored from the last loaded or saved state.' }
          : { message: `The engine failed: ${err && err.message ? err.message : err}` },
      },
      display: null,
      subgraph: null,
      elapsed_ms: 0,
      run_id: ++state.runCount,
      ...(selection ? { selection: { lines: selection.lines, offset: selection.offset } } : {}),
    });
    return tab.result;
  } finally {
    clearInterval(timer);
    state.running = false;
    state.runningTabId = null;
    state.runningElapsedMs = 0;
  }
}

/**
 * The error of a GQL tab's run with the editor position of its span: `position` (the Go to line
 * button) and `span` `{line, column, width, gqlstatus, offset}` for the underline, both in the
 * editor's text (moved by `offset`, where the text that ran starts). An error without a GQL span
 * loses the position of the `g.gql(..)` wrapper it was reported for.
 */
function gqlError(error, offset) {
  const out = { ...error };
  delete out.position;
  const span = gqlErrorSpan(error);
  if (span) {
    out.position = mapPosition({ line: span.line, column: span.column }, offset);
    out.span = { ...span, offset };
  }
  return out;
}

/** A failed result built in the page (no engine call): an error shown in Results. */
function failedResult(error, selection, runId) {
  return {
    ok: false,
    terminal: 'unknown',
    mutated: false,
    execution: { results: [], profile: null, error },
    display: null,
    subgraph: null,
    elapsed_ms: 0,
    run_id: runId,
    ...(selection ? { selection: { lines: selection.lines, offset: selection.offset } } : {}),
  };
}

/**
 * "Run all tabs": runs every tab that has a script once, left to right, each into its own tab. A
 * stopped run ends the series. When a later tab changes the graph, the earlier tabs' results
 * become possibly stale, exactly as with single runs.
 */
export async function runAllTabs() {
  if (state.running || !state.engine) return;
  state.runAllActive = true;
  let ran = 0;
  let failed = 0;
  let stopped = false;
  try {
    for (const tab of [...state.tabs]) {
      if (!state.tabs.includes(tab) || !tab.script.trim()) continue;
      const res = await run(tab.script, tab.id);
      if (!res) continue;
      ran++;
      if (res.stopped) {
        stopped = true;
        break;
      }
      if (!res.ok) failed++;
    }
  } finally {
    state.runAllActive = false;
  }
  reveal('results');
  if (stopped) toast(`Run all stopped after ${ran} tab${ran === 1 ? '' : 's'}.`, 'info');
  else toast(`Ran ${ran} tab${ran === 1 ? '' : 's'}${failed ? `; ${failed} failed` : ''}.`, failed ? 'error' : 'info');
}

/**
 * The engine layouts of the drawings in progress (js/graph/layout-jobs.js): graph-canvas.js
 * registers each one; `stopRun` cancels them.
 */
export const layoutJobs = new LayoutJobs((count) => { state.layoutsRunning = count; });

/**
 * Stops what runs. Engine layouts of the drawings are cancelled (each drawing keeps the positions
 * reached so far). A running query: the engine restarts (its worker is terminated) and restores the
 * graph from the last loaded sample or file, or the last "Save graph". Changes since then are
 * lost, so the other tabs' results become possibly stale. When only layouts run, nothing restarts.
 */
export async function stopRun() {
  if (state.stopping || !state.engine) return;
  const layouts = layoutJobs.cancelAll();
  if (!state.running) {
    if (layouts) toast(`Stopped the layout${layouts === 1 ? '' : 's'}: the drawing shows the positions reached so far.`, 'info');
    return;
  }
  state.stopping = true;
  try {
    const stopped = await state.engine.stop();
    if ('cancelled' in stopped) {
      // The dev server cancels the query; it rolls back and the graph stays as it was.
      toast(stopped.cancelled ? 'Stopped: the query was cancelled on the server and rolled back.' : 'Nothing was running on the server.', 'info', 7000);
      return;
    }
    const { restored } = stopped;
    state.graph = await state.engine.graphInfo();
    state.graphEpoch++;
    state.graphVersion++;
    state.dataVersion++;
    // The new worker starts a new store in memory from the restore point.
    state.storeView = null;
    state.storeVersion++;
    tabs.markOthersStale(state, state.runningTabId);
    toast(restored ? `Stopped. The engine restarted with "${restored}" as last loaded or saved; the store in memory starts over from it (its history, marks and forks are gone).` : 'Stopped. The engine restarted.', 'info', 9000);
  } catch (err) {
    toast(`The engine could not be restarted: ${err && err.message ? err.message : err}`, 'error', 9000);
  } finally {
    state.stopping = false;
  }
}

/**
 * Save ▸ Data: downloads the graph data as GraphSON (`format` 'graphson', the default:
 * `graphersal-graph-YYYYMMDD-HHMMSS.json`) or GraphML (`.graphml`), ONE file: neither format
 * carries the schema, which has its own entry ([`saveSchema`]); browsers drop a second download
 * of one click. Loading the data with the schema as its schema file (Load from file) restores both; the saved
 * state is also what Stop restores. `format` 'snapshot' downloads one packed snapshot
 * (`.gsnap`): lossless, with the schema, loaded back from the start page in one step.
 * @param {'graphson'|'graphml'|'snapshot'} [format]
 */
export async function saveGraph(format = 'graphson') {
  if (!state.engine || !state.graph || state.saving || state.running) return;
  state.saving = true;
  try {
    const saved = await state.engine.saveGraph(format);
    const file = savedGraphFile(saved);
    downloadBlob(new Blob([file.bytes || file.text], { type: file.type }), file.name);
    if (saved.format === 'snapshot') {
      toast('Saved a snapshot (.gsnap: data, types and schema); load it from the start page.', 'info', 8000);
      return;
    }
    const name = saved.format === 'graphml' ? 'GraphML' : 'GraphSON';
    const what = saved.schema
      ? `Saved the graph data (${name}). It carries no schema: save that with Save ▸ Schema; to load both back, choose both in Load from file (graph file + schema file).`
      : `Saved as ${name}; load it from the start page.`;
    const warnings = saved.warnings || [];
    if (warnings.length) toast(`${what} Caveat: ${warnings.join(' ')}`, 'info', 12000);
    else toast(what, 'info', 8000);
  } catch (err) {
    toast(`Save failed: ${err && err.message ? err.message : err}`, 'error', 9000);
  } finally {
    state.saving = false;
  }
}

let shareDialogOpener = null;

/** The workspace registers how to open the share dialog (components/share-dialog.js). */
export function registerShareDialog(open) {
  shareDialogOpener = open;
}

/** Opens the share dialog: the top bar's Share button and the tab menu's "Share as link…". */
export function openShareDialog() {
  // Over the dev server there is nothing to share: a link opens the public playground, which
  // has neither the server's graph nor its store.
  if (isServerMode()) return;
  if (shareDialogOpener) shareDialogOpener();
}

/**
 * The link of the share dialog (components/share-dialog.js) for the active tab: its query and the
 * graph, as a sample name when the graph is a sample that has not changed, otherwise the graph's
 * GraphSON and schema compressed into the link (js/link.js). Saved queries and compression rules
 * are not included. Not offered over the dev server (openShareDialog).
 * Resolves to `{url, chars, fits, sample, vertexCount, edgeCount, schema, tooMany}`.
 * @param {{run?: boolean}} [options]
 */
export async function makeShareLink({ run = true } = {}) {
  const tab = state.tab;
  const graph = state.graph;
  if (!state.engine || !graph || !tab) throw new Error('No graph is open.');
  const script = tab.kind === 'ai' ? '' : tab.script.trim();
  const base = window.location.href;
  const title = tab.name;
  const counts = { vertexCount: graph.vertexCount, edgeCount: graph.edgeCount };
  let store = null;
  if (typeof state.engine.storeInfo === 'function') {
    try {
      store = await state.engine.storeInfo();
    } catch (err) {
      store = null;
    }
  }
  const gql = tab.kind !== 'ai' && tab.language === 'gql' ? { language: 'gql', params: tab.params } : {};
  if (isUnchangedSample(graph, store, !!state.storeView)) {
    const link = shareLink(base, { graph: graph.kind, script, title, run: run && script !== '', ...gql });
    return { ...link, ...counts, sample: graph.kind, schema: false, tooMany: false };
  }
  if (graph.vertexCount + graph.edgeCount > MAX_SHARE_ELEMENTS) {
    return { url: '', chars: 0, fits: false, ...counts, sample: null, schema: false, tooMany: true };
  }
  const saved = await state.engine.saveGraph('graphson');
  const data = await encodeLinkData(saved.text);
  const schema = saved.schema ? await encodeLinkData(saved.schema) : null;
  const link = shareLink(base, { data, schema, script, title, run: run && script !== '', ...gql });
  return { ...link, ...counts, sample: null, schema: !!saved.schema, tooMany: false };
}

/** Save ▸ Schema: downloads the stored schema as its own JSON file (or says there is none). */
export async function saveSchema() {
  if (!state.engine || !state.graph || state.saving || state.running) return;
  state.saving = true;
  try {
    const schema = await state.engine.getSchema();
    if (isEmptySchema(schema)) {
      toast('The graph has no schema to save (define one in the Schema panel, or infer it).', 'info', 7000);
      return;
    }
    const file = schemaFile(schema);
    downloadBlob(new Blob([file.text], { type: file.type }), file.name);
    toast('Saved the schema (JSON); load it back as the schema file of Load from file, together with the graph data.', 'info', 7000);
  } catch (err) {
    toast(`Save failed: ${err && err.message ? err.message : err}`, 'error', 9000);
  } finally {
    state.saving = false;
  }
}

/**
 * "Save to file" (dev server only): writes the graph back to the file the server was started
 * with (`--graph <file>`); explicit, never automatic.
 */
export async function saveToSource() {
  if (!state.engine || !state.engine.saveToSource || state.saving || state.running) return;
  state.saving = true;
  try {
    const saved = await state.engine.saveToSource();
    const warnings = saved.warnings || [];
    const what = saved.schemaPath
      ? `Saved to ${saved.path} and the schema to ${saved.schemaPath}.`
      : `Saved to ${saved.path}.`;
    if (warnings.length) toast(`${what} Caveat: ${warnings.join(' ')}`, 'info', 12000);
    else toast(what, 'info', 6000);
    // The stored schema could not be saved (GraphML/GraphSON without --schema FILE).
    if (saved.warning) toast(`Schema not saved: ${saved.warning}.`, 'error', 15000);
  } catch (err) {
    toast(`Save failed: ${err && err.message ? err.message : err}`, 'error', 9000);
  } finally {
    state.saving = false;
  }
}

/**
 * The graph the dev server serves: read once on start (there is no graph choice in that mode).
 */
export async function attachServerGraph() {
  await engineReady();
  if (!state.engine) return false;
  try {
    // The change feed's position BEFORE the graph is read: a change between the two is seen.
    await baselineChangeFeed();
    state.graph = await state.engine.graphInfo();
    state.graphEpoch++;
    state.graphVersion++;
    state.dataVersion++;
    startChangeFeed();
    return true;
  } catch (err) {
    state.loadError = err && err.message ? err.message : String(err);
    return false;
  }
}

// ------------------------------------------------------------------ change feed (dev server)

let feedPoller = null;
let knownVersion = null;
// The change feed's burst state machine (js/change-burst.js): throttles reloads while the graph
// changes fast elsewhere.
const burst = createBurst();
let lastCommitSeq = 0;
// Reloads never overlap: the poller awaits applyChanges, Refresh now waits for this chain.
let reloadChain = Promise.resolve();

/** Reads the feed's position once (the baseline); a server without the feed is ignored. */
async function baselineChangeFeed() {
  if (!state.engine || typeof state.engine.changes !== 'function') return;
  try {
    const first = readChanges(await state.engine.changes(), null, state.engine.clientId);
    knownVersion = first.version;
    lastCommitSeq = first.commitSeq;
  } catch (err) {
    /* the first poll sets it */
  }
}

/**
 * Polls the dev server's change feed (js/change-feed.js) while the page is visible. A change
 * this page did not make (another tab, an AI agent over MCP) reloads the graph info, the Graph,
 * Schema and Statistics panels and the Store menu, follows a view opened or closed elsewhere,
 * marks every tab's result as possibly stale and says "changed: commit N". While the graph changes
 * fast elsewhere, reloads are throttled (js/change-burst.js; banner `state.changeBurst`). The MCP
 * badge follows the feed's `mcp` (the connected agent's name).
 */
export function startChangeFeed() {
  if (feedPoller || !state.engine || typeof state.engine.changes !== 'function') return;
  const visible = () => typeof document === 'undefined' || document.visibilityState !== 'hidden';
  feedPoller = createPoller({
    fetchChanges: () => state.engine.changes(),
    isVisible: visible,
    onAnswer: applyChanges,
  });
  feedPoller.start();
  if (typeof document !== 'undefined') {
    document.addEventListener('visibilitychange', () => { if (visible()) feedPoller.kick(); });
  }
}

async function applyChanges(answer) {
  if (state.engineInfo && JSON.stringify(state.engineInfo.mcp || null) !== JSON.stringify(answer.mcp || null)) {
    state.engineInfo = { ...state.engineInfo, mcp: answer.mcp || null };
  }
  // Damage found on disk while the store is open (a backup, a checkpoint, verify): the banner.
  if (state.engineInfo && JSON.stringify(state.engineInfo.damageFound || null) !== JSON.stringify(answer.damageFound || null)) {
    state.engineInfo = { ...state.engineInfo, damageFound: answer.damageFound || null };
    state.storeVersion++;
  }
  const seen = readChanges(answer, knownVersion, state.engine.clientId);
  knownVersion = seen.version;
  // How many commits the answer covers (the burst banner's count; at least 1 when external).
  const commits = seen.external ? Math.max(1, seen.commitSeq - lastCommitSeq) : 0;
  lastCommitSeq = seen.commitSeq;
  const view = answer.view || null;
  // A view opened or closed elsewhere replaces the shown graph: always followed at once.
  const replaced = seen.external && JSON.stringify(state.storeView || null) !== JSON.stringify(view);
  const decision = burst.observe(seen.external, commits);
  if (replaced || decision === 'refresh') {
    const quiet = burst.mode === 'normal' && !burst.paused;
    await reloadFromFeed(view, replaced);
    // In burst mode the banner replaces the toast per change.
    if (quiet) toast(`changed: commit ${seen.commitSeq}`, 'info', 4000);
  }
  showBurst();
}

/** Reloads the graph info, the panels and the Store menu after changes made elsewhere. */
function reloadFromFeed(view, replaced) {
  const run = async () => {
    const token = burst.beginRefresh();
    if (view !== undefined) {
      if (state.engine.serverInfo) state.engine.serverInfo = { ...state.engine.serverInfo, view };
      state.storeView = view;
    }
    state.graph = await state.engine.graphInfo();
    if (replaced) state.graphEpoch++;
    state.graphVersion++;
    state.dataVersion++;
    state.storeVersion++;
    tabs.markOthersStale(state, null);
    burst.refreshed(token);
  };
  const next = reloadChain.then(run, run);
  reloadChain = next.catch(() => {});
  return next;
}

/** Mirrors the burst banner into the state; announces it when its kind changes. */
function showBurst() {
  const banner = burst.banner();
  const kind = banner ? banner.kind : null;
  const was = state.changeBurst ? state.changeBurst.kind : null;
  if (kind !== was) state.changeBurstAnnounce = burstAnnouncement(banner);
  if (JSON.stringify(banner) !== JSON.stringify(state.changeBurst)) state.changeBurst = banner;
}

/** The burst banner's Refresh now: reloads at once (and restarts the 30 s interval). */
export async function refreshFromFeedNow() {
  if (!state.engine) return;
  try {
    await reloadFromFeed(undefined, false);
  } catch (err) {
    toast(`Refresh failed: ${err && err.message ? err.message : err}`, 'error', 7000);
  }
  showBurst();
}

/** The burst banner's Pause auto refresh: no automatic reload until Resume. */
export function pauseFeedRefresh() {
  burst.pause();
  showBurst();
}

/** The burst banner's Resume: automatic reloads again (one now when it is due). */
export async function resumeFeedRefresh() {
  if (burst.resume() === 'refresh') await refreshFromFeedNow();
  else showBurst();
}

// ------------------------------------------------------------------ AI Chat tabs

/**
 * Where the AI Chat works: 'on' (the dev server runs with `--ai`), 'off' (a dev server without
 * it: the tab says how to start it), 'wasm' (the WebAssembly playground: read-only, decision 6),
 * or 'loading' before the engine answered.
 */
export function aiChatMode() {
  const info = state.engineInfo;
  if (!info) return 'loading';
  if (!info.server) return 'wasm';
  return info.ai ? 'on' : 'off';
}

// One controller per chat tab (js/ai-chat.js): its conversation, poll loop, Stop and errors.
const chatControllers = new Map();
let chatVisibilityHooked = false;

function pageVisible() {
  return typeof document === 'undefined' || document.visibilityState !== 'hidden';
}

/** The controller of a chat tab (created on first use), or null for another tab or mode. */
export function chatController(tabId) {
  const tab = tabs.findTab(state, tabId);
  if (!tab || tab.kind !== 'ai' || !state.engine || aiChatMode() !== 'on') return null;
  let c = chatControllers.get(tabId);
  if (c && c.chat !== tab.chat) {
    c.dispose();
    c = null;
  }
  if (!c) {
    const engine = state.engine;
    c = new ChatController(tab.chat, {
      api: {
        create: () => engine.aiCreateConversation(),
        send: (id, text) => engine.aiSend(id, text),
        events: (id, after) => engine.aiEvents(id, after),
        stop: (id) => engine.aiStop(id),
        remove: (id) => engine.aiDelete(id),
      },
      onChange: persistTabsSoon,
      isVisible: pageVisible,
    });
    chatControllers.set(tabId, c);
    if (!chatVisibilityHooked && typeof document !== 'undefined') {
      chatVisibilityHooked = true;
      // A hidden page pauses every chat's poll loop (like the change feed); visible again: poll now.
      document.addEventListener('visibilitychange', () => {
        if (pageVisible()) for (const ctl of chatControllers.values()) ctl.kick();
      });
    }
  }
  return c;
}

/** Ends a chat tab's poll loop (the tab closes); a conversation this page created is deleted. */
function disposeChat(tabId) {
  const c = chatControllers.get(tabId);
  if (c) c.dispose();
  chatControllers.delete(tabId);
}

/** A new AI Chat tab after the active one. */
export function newChatTab() {
  return newTab({ kind: 'ai' });
}

/** The chat input's Send (a `/command` runs locally). */
export function chatSubmit(tabId, text) {
  const c = chatController(tabId);
  return c ? c.submit(text) : Promise.resolve(false);
}

/** The chat's Stop: ends this tab's turn only. */
export function chatStop(tabId) {
  const c = chatController(tabId);
  return c ? c.stop() : Promise.resolve(false);
}

/** The unsent text of a chat tab (persisted with the tab). */
export function setChatDraft(tabId, text) {
  const tab = tabs.findTab(state, tabId);
  if (!tab || tab.kind !== 'ai') return;
  tab.chat.draft = text;
  persistTabsSoon();
}

// ------------------------------------------------------------------ store (dev server or in memory)

/**
 * The server now serves another graph (a read-only view, the live graph again, the state after
 * a rollback or an attic restore): the Graph, Schema and Statistics panels reload, every tab's
 * result may be stale, and the Store menu reloads.
 */
function storeGraphSwapped(graph, view) {
  if (graph) state.graph = graph;
  state.storeView = view || null;
  state.graphEpoch++;
  state.graphVersion++;
  state.dataVersion++;
  state.storeVersion++;
  tabs.markOthersStale(state, null);
}

/** Runs a store action once at a time; rejects with the server's message. */
async function storeAction(fn) {
  if (!state.engine || state.storeBusy) return null;
  state.storeBusy = true;
  try {
    return await fn();
  } finally {
    state.storeBusy = false;
  }
}

/**
 * Runs the Store menu's action `id` (a key of `STORE_TASKS` in js/store-model.js) as THE running
 * store action: `state.storeTask` holds it with a ticking clock while `fn` runs (the menu item,
 * the Store button and the dialogs show it), the live region announces its start and end, and
 * every other store action is refused meanwhile (returns `{refused: true}` without calling `fn`;
 * also while a query runs or `storeAction` holds the store). `key` (optional) names the item among
 * several of the same action (an attic entry's id). Returns `{value}` or rethrows.
 */
export async function runStoreTask(id, fn, key = null) {
  if (state.storeTask || state.storeBusy || state.running) return { refused: true };
  const startedAt = Date.now();
  state.storeTask = { id, key, startedAt, now: startedAt };
  state.storeAnnounce = taskStartText(id);
  const tick = setInterval(() => {
    if (state.storeTask) state.storeTask.now = Date.now();
  }, 250);
  let ok = false;
  try {
    const value = await fn();
    ok = true;
    return { value };
  } finally {
    clearInterval(tick);
    state.storeTask = null;
    state.storeAnnounce = taskEndText(id, Date.now() - startedAt, ok);
  }
}

/** "Open read-only here": the server serves the state at `target` (js/store-model.js) read-only. */
export function openStoreView(target) {
  return storeAction(async () => {
    const res = await state.engine.openView(target);
    storeGraphSwapped(res.graph, res.view);
    return res;
  });
}

/** "Back to the live graph" from a read-only view. */
export function backToLiveGraph() {
  return storeAction(async () => {
    const res = await state.engine.closeView();
    storeGraphSwapped(res.graph, null);
    return res;
  });
}

/**
 * "Roll back here": the server cancels the running query and rolls the store back in place;
 * the live graph is the state at `target` afterwards.
 */
export function rollbackStore(target, deleteHistory = false) {
  return storeAction(async () => {
    const res = await state.engine.rollback(target, deleteHistory);
    storeGraphSwapped(res.graph, null);
    return res;
  });
}

/** "Switch to" a database of the session (the browser's store in memory and its forks). */
export function switchDatabase(id) {
  return storeAction(async () => {
    const res = await state.engine.switchDatabase(id);
    storeGraphSwapped(res.graph, null);
    return res;
  });
}

/** "Close" a database that is not in use (its memory is freed). */
export function closeDatabase(id) {
  return storeAction(async () => {
    const res = await state.engine.closeDatabase(id);
    state.storeVersion++;
    return res;
  });
}

/**
 * Downloads the stored snapshot at `commit` (the browser's store in memory) as `.gsnap`, with the
 * store's embedding models unless `withModels` is false.
 */
export async function downloadStoredSnapshot(commit, withModels = true) {
  if (!state.engine || !state.engine.snapshotBytes) return;
  try {
    const bytes = await state.engine.snapshotBytes(commit, withModels);
    downloadBlob(new Blob([bytes], { type: 'application/octet-stream' }), snapshotFileName(commit, withModels));
  } catch (err) {
    toast(`Download failed: ${err && err.message ? err.message : err}`, 'error', 9000);
  }
}

/** "Restore" of an attic entry: undoes its rollback. */
export function restoreAtticEntry(id) {
  return storeAction(async () => {
    const res = await state.engine.atticRestore(id);
    storeGraphSwapped(res.graph, null);
    return res;
  });
}

/**
 * "Load schema": applies a schema JSON file to the CURRENT graph (checked against its data). A
 * graph loaded from a file afterwards does not keep it: to load a graph file with its schema
 * (typed values back), give the schema file in Load from file. Returns true on success.
 * @param {File} file
 */
export async function loadSchema(file) {
  if (!state.engine || !file || state.loadingGraph || state.running) return false;
  state.loadingGraph = true;
  try {
    state.graph = await state.engine.loadSchema(file);
    state.graphVersion++;
    toast(`Schema applied to the current graph (mode ${state.graph.schemaMode}).`, 'info', 6000);
    return true;
  } catch (err) {
    toast(`Could not load the schema: ${err && err.message ? err.message : err}`, 'error', 12000);
    return false;
  } finally {
    state.loadingGraph = false;
  }
}

/**
 * The schema editor applied a schema (through the engine's schema methods): the graph info
 * (schema mode) is re-read, the Graph and Schema panels reload, and every tab's result may be
 * stale (a schema change is a change of the graph).
 */
export async function schemaChanged() {
  if (!state.engine) return;
  state.graph = await state.engine.graphInfo();
  state.graphVersion++;
  tabs.markOthersStale(state, null);
}

/**
 * The property editor (Graph panel) saved an element: the write was one commit, so the graph
 * info is re-read, the Graph, Schema and Statistics panels reload, and every tab's result may
 * be stale, as after a query that changed the graph.
 */
export async function elementEdited() {
  if (!state.engine) return;
  try {
    state.graph = await state.engine.graphInfo();
  } catch (err) {
    /* the counts in the info are cosmetic */
  }
  state.graphVersion++;
  state.dataVersion++;
  state.storeVersion++;
  tabs.markOthersStale(state, null);
}

// ------------------------------------------------------------------ saved queries

let queriesLoad = 0;

/** Reloads the saved queries of the graph (the Catalog manager and the `g.query("` completion). */
export async function refreshSavedQueries() {
  if (!state.engine || typeof state.engine.listQueries !== 'function' || !state.graph) return;
  const load = ++queriesLoad;
  try {
    const list = await state.engine.listQueries();
    if (load === queriesLoad) {
      state.savedQueries = markRaw(list || []);
      state.savedQueriesError = null;
    }
  } catch (err) {
    if (load === queriesLoad) state.savedQueriesError = err && err.message ? err.message : String(err);
  }
}

/** The catalog changed through the page: the list and the graph info (its count) reload. */
async function savedQueriesChanged() {
  await refreshSavedQueries();
  try {
    state.graph = await state.engine.graphInfo();
  } catch (err) {
    /* the count in the info is cosmetic */
  }
}

/**
 * The editor's Save: defines (or replaces, or renames) a saved query from a spec
 * (js/saved-queries.js `draftToSpec`). Resolves to the engine's answer: `{ok, query}` or
 * `{ok: false, errors}` (field errors, shown in the editor).
 */
export async function defineSavedQuery(spec) {
  if (!state.engine) return { ok: false, errors: [{ field: 'general', message: 'The engine is not available.' }] };
  const res = await state.engine.defineQuery(spec);
  if (res && res.ok) await savedQueriesChanged();
  return res;
}

/** Drops a saved query (the caller asked the user first). */
export async function dropSavedQuery(name) {
  const res = await state.engine.dropQuery(name);
  await savedQueriesChanged();
  return res;
}

/**
 * The preview of a call of a saved query that writes (the run form asks before it runs one):
 * `{ok, changes}` or `{ok: false, error}`; the call runs as a dry run and is rolled back.
 */
export async function previewSavedQuery(name, args) {
  if (!state.engine) return { ok: false, error: { message: 'The engine is not available.' } };
  return state.engine.previewQuery(name, args);
}

/**
 * The run form's Run: writes `g.query("name", #{..})` into a tab (the active one when it is
 * empty or holds an unmodified example, else a new one named after the query) and runs it
 * there, so the call is visible, editable and in the history.
 */
export async function runSavedQuery(name, args) {
  if (!state.engine) return null;
  const script = await state.engine.queryCallText(name, args);
  const tab = openScript({ title: name, script });
  if (!tab) return null;
  if (state.running) {
    toast('A query is running: run the new tab when it has finished.', 'info');
    reveal('editor');
    return null;
  }
  return run(script, tab.id);
}

/**
 * Catalog ▸ Save catalog: downloads the whole catalog (saved queries, compression rules and
 * every other kind) as its JSON file `graphersal-catalog-<stamp>.json` (or says it is empty).
 */
export async function saveCatalogFile() {
  if (!state.engine || !state.graph || state.saving || state.running) return;
  state.saving = true;
  try {
    const text = await state.engine.saveCatalog();
    if (isEmptyCatalog(text)) {
      toast('The catalog is empty: no saved queries, compression rules or property indexes to save.', 'info', 7000);
      return;
    }
    const file = catalogFile(text);
    downloadBlob(new Blob([file.text], { type: file.type }), file.name);
    toast('Saved the catalog (saved queries, compression rules and property indexes, JSON); load it back with Catalog ▸ Load catalog.', 'info', 7000);
  } catch (err) {
    toast(`Save failed: ${err && err.message ? err.message : err}`, 'error', 9000);
  } finally {
    state.saving = false;
  }
}

/**
 * Catalog ▸ Load catalog: stores the definitions of a catalog file in the current graph (one
 * commit; one of the same kind and name is replaced; a compression rule re-encodes its values).
 * A graph loaded from a GraphSON or GraphML file afterwards keeps them. Returns true on success.
 * @param {File} file
 */
export async function loadCatalogFile(file) {
  if (!state.engine || !file || state.loadingGraph || state.running) return false;
  state.loadingGraph = true;
  try {
    const res = await state.engine.loadCatalog(file);
    if (res && res.graph) state.graph = res.graph;
    await refreshSavedQueries();
    await refreshCompressions();
    await refreshIndexes();
    toast(`Loaded ${res.loaded} catalog definition${res.loaded === 1 ? '' : 's'}.`, 'info', 6000);
    return true;
  } catch (err) {
    toast(`Could not load the catalog: ${err && err.message ? err.message : err}`, 'error', 12000);
    return false;
  } finally {
    state.loadingGraph = false;
  }
}

/** Opens a manager of the Catalog dropdown: 'queries', 'compression' or 'indexes'. */
/** Opens the Optimize panel (js/components/optimize.js). */
export function openOptimize() {
  state.optimizeN++;
}

/**
 * A recommendation of the Optimize panel was applied (an index or a compression rule created
 * or dropped): the catalog lists and the graph info (its counts) reload.
 */
export async function optimizeApplied() {
  await Promise.all([refreshIndexes(), refreshCompressions()]);
  try {
    state.graph = await state.engine.graphInfo();
  } catch (err) {
    /* the counts in the info are cosmetic */
  }
}

export function openCatalog(id) {
  state.catalogDialog = id;
  state.catalogDialogN++;
}

/**
 * The query tabs' "Save as query…": the saved-query editor on the active tab's text (a tab
 * holding one `g.gql(..)` call becomes a GQL query, js/saved-queries.js `draftFromScript`).
 */
export function saveTabAsQuery() {
  const tab = state.tab;
  if (!tab || tab.kind === 'ai') return;
  // A GQL tab hands its query and parameters directly (saved as `language: "gql"`).
  state.catalogDialogScript = tab.language === 'gql'
    ? { language: 'gql', query: tab.script, params: tab.params }
    : tab.script;
  openCatalog('save-as-query');
}

// ------------------------------------------------------------------ compression rules

let compressionsLoad = 0;

/** Reloads the compression rules with their stats (the Compression manager). */
export async function refreshCompressions() {
  if (!state.engine || typeof state.engine.listCompressions !== 'function' || !state.graph) return;
  const load = ++compressionsLoad;
  try {
    const list = await state.engine.listCompressions();
    if (load === compressionsLoad) {
      state.compressions = markRaw(list || []);
      state.compressionsError = null;
    }
  } catch (err) {
    if (load === compressionsLoad) state.compressionsError = err && err.message ? err.message : String(err);
  }
}

/** A rule changed through the page: the list and the graph info (its counts) reload. */
async function compressionsChanged() {
  await refreshCompressions();
  try {
    state.graph = await state.engine.graphInfo();
  } catch (err) {
    /* the counts in the info are cosmetic */
  }
}

/**
 * The Compression form's Save: defines (or replaces, or renames) a rule from a spec
 * (js/compression.js `draftToSpec`). Resolves to the engine's answer: `{ok, rule}` or
 * `{ok: false, errors}` (field errors, shown in the form).
 */
export async function defineCompressionRule(spec) {
  if (!state.engine) return { ok: false, errors: [{ field: 'general', message: 'The engine is not available.' }] };
  const res = await state.engine.defineCompression(spec);
  if (res && res.ok) await compressionsChanged();
  return res;
}

/** Drops a compression rule (the caller asked the user first); its values become plain. */
export async function dropCompressionRule(name) {
  const res = await state.engine.dropCompression(name);
  await compressionsChanged();
  return res;
}

/**
 * The Compression form's Estimate: what rules for the form's element, label and path would hold
 * over the current data (js/compression.js `draftToEstimateSpec`); changes nothing. Resolves to
 * the engine's answer: `{ok, estimate}` or `{ok: false, errors}`.
 */
export async function estimateCompressionRule(spec) {
  if (!state.engine || typeof state.engine.estimateCompression !== 'function') {
    return { ok: false, errors: [{ field: 'general', message: 'This engine cannot estimate compression rules.' }] };
  }
  return state.engine.estimateCompression(spec);
}

/** Rebuilds a rule's dictionary and re-encodes its values; resolves to the rule's entry. */
export async function recompressRule(name) {
  const res = await state.engine.recompress(name);
  await compressionsChanged();
  return res;
}

// ------------------------------------------------------------------ property indexes

let indexesLoad = 0;

/** Reloads the property indexes with their stats (the Property indexes manager). */
export async function refreshIndexes() {
  if (!state.engine || typeof state.engine.listIndexes !== 'function' || !state.graph) return;
  const load = ++indexesLoad;
  try {
    const list = await state.engine.listIndexes();
    if (load === indexesLoad) {
      state.indexes = markRaw(list || []);
      state.indexesError = null;
    }
  } catch (err) {
    if (load === indexesLoad) state.indexesError = err && err.message ? err.message : String(err);
  }
}

/** An index changed through the page: the list and the graph info (its counts) reload. */
async function indexesChanged() {
  await refreshIndexes();
  try {
    state.graph = await state.engine.graphInfo();
  } catch (err) {
    /* the counts in the info are cosmetic */
  }
}

/**
 * The Property indexes form's Create: creates an index from a spec (js/indexes.js
 * `indexDraftToSpec`). Resolves to the engine's answer: `{ok, index}` or `{ok: false, errors}`
 * (field errors, shown in the form).
 */
export async function createPropertyIndex(spec) {
  if (!state.engine) return { ok: false, errors: [{ field: 'general', message: 'The engine is not available.' }] };
  const res = await state.engine.createIndex(spec);
  if (res && res.ok) await indexesChanged();
  return res;
}

/** Drops a property index (the caller asked the user first); no data changes. */
export async function dropPropertyIndex(name) {
  const res = await state.engine.dropIndex(name);
  await indexesChanged();
  return res;
}

// Scheduled jobs (the Catalog's Scheduled jobs manager, js/jobs.js).

let jobsLoad = 0;

/** Reloads the scheduled jobs (`state.jobs`). */
export async function refreshJobs() {
  if (!state.engine || typeof state.engine.jobs !== 'function' || !state.graph) return;
  const load = ++jobsLoad;
  try {
    const view = await state.engine.jobs();
    if (load === jobsLoad) {
      state.jobs = markRaw(view || null);
      state.jobsError = null;
    }
  } catch (err) {
    if (load === jobsLoad) state.jobsError = err && err.message ? err.message : String(err);
  }
}

/** A job changed through the page: the list and the graph info (its counts) reload. */
async function jobsChanged() {
  await refreshJobs();
  try {
    state.graph = await state.engine.graphInfo();
  } catch (err) {
    /* the counts in the info are cosmetic */
  }
}

/** Defines or replaces a job of a user: `{ok, job}` or `{ok: false, errors}`. */
export async function defineUserJob(spec) {
  if (!state.engine) return { ok: false, errors: [{ field: 'general', message: 'The engine is not available.' }] };
  const res = await state.engine.defineJob(spec);
  if (res && res.ok) await jobsChanged();
  return res;
}

/** Drops a job of a user (the caller asked the user first). */
export async function dropUserJob(name) {
  const res = await state.engine.dropJob(name);
  await jobsChanged();
  return res;
}

/** Switches a job on or off. */
export async function switchJob(name, enabled) {
  const res = await state.engine.setJobEnabled(name, enabled);
  await jobsChanged();
  return res;
}

/** Gives a system job a schedule (`null`: its built-in one). */
export async function scheduleSystemJob(name, schedule) {
  const res = await state.engine.setJobSchedule(name, schedule);
  await refreshJobs();
  return res;
}

/**
 * Runs a job now: its entry. The run may have changed the data (a query job) or the store (a
 * checkpoint): the graph info reloads and the Graph, Schema and Statistics panels redraw, as
 * after a query of the page (the change feed does not reload for the page's own commit).
 */
export async function runJobNow(name) {
  const res = await state.engine.runJobNow(name);
  await jobsChanged();
  state.graphVersion++;
  state.dataVersion++;
  return res;
}

/**
 * Opens `script` in a NEW tab named `title` and, with `execute`, runs it there (the schema
 * editor's "show the offending elements"). Returns the tab, or null at the tab limit.
 */
export function openQueryTab({ title, script, execute = false }) {
  const tab = newTab({ name: String(title || '').slice(0, tabs.MAX_NAME), autoName: false, script });
  if (!tab) return null;
  if (execute) {
    if (state.running) toast('A query is running: run the new tab when it has finished.', 'info');
    else run(script, tab.id);
  } else {
    reveal('editor');
  }
  return tab;
}

let vocabLoading = null;

/**
 * The graph's vocabulary for the GQL completion (js/editor/gql-completion.js): `{vertexLabels,
 * edgeLabels, keys}` (labels from `statistics()`, property keys by label from the stored schema
 * or, without one, the inferred schema). It returns what is loaded (possibly of an older version)
 * and starts loading the current version in the background; before the first load it returns
 * the promise of that load.
 */
export function gqlVocabulary() {
  const version = `${state.graphEpoch}:${state.graphVersion}:${state.dataVersion}`;
  const have = state.gqlVocab;
  if ((!have || have.version !== version) && !vocabLoading && state.engine && state.graph) {
    vocabLoading = loadGqlVocabulary(version).catch(() => {}).finally(() => { vocabLoading = null; });
  }
  if (!have && vocabLoading) return vocabLoading.then(() => state.gqlVocab);
  return have;
}

async function loadGqlVocabulary(version) {
  const engine = state.engine;
  const [stats, stored] = await Promise.all([
    engine.statistics().catch(() => null),
    engine.getSchema().catch(() => null),
  ]);
  const declares = (schema) => !!schema && ['vertices', 'edges'].some((k) => schema[k] && Object.keys(schema[k]).length);
  const schema = declares(stored) ? stored : await engine.inferSchema().catch(() => null);
  const s = stats && stats.statistics;
  const labels = (rows) => [...new Set((rows || []).map((r) => r.label).filter((l) => typeof l === 'string' && l))].sort();
  const keys = keysBySchema(schema);
  state.gqlVocab = markRaw({
    version,
    vertexLabels: [...new Set([...labels(s && s.vertex_labels), ...Object.keys(keys.vertex)])].sort(),
    edgeLabels: [...new Set([...labels(s && s.edge_labels), ...Object.keys(keys.edge)])].sort(),
    keys,
  });
}

function addHistory(script, ok, tab, gql = null) {
  const history = state.history.filter((h) => h.script !== script);
  history.unshift({ script, ok, at: Date.now(), tab, ...(gql || {}) });
  state.history = history.slice(0, HISTORY_MAX);
  storage.set('history', state.history);
}

export function clearHistory() {
  state.history = [];
  storage.set('history', []);
}

export function reveal(panel, tab = null) {
  state.focusPanel = { panel, tab, n: (state.focusPanel ? state.focusPanel.n : 0) + 1 };
}

/** Shows the neighbourhood of vertex `id` in the Graph panel (and reveals it). */
export function showNeighbourhood(id) {
  state.graphRequest = { source: 'neighbourhood', ids: [String(id)], n: (state.graphRequest ? state.graphRequest.n : 0) + 1 };
  reveal('graph');
}

export function jumpTo(position) {
  if (!position) return;
  state.jumpTo = { ...position, n: (state.jumpTo ? state.jumpTo.n : 0) + 1 };
}

let toastId = 0;
/**
 * Shows a toast for `ms` milliseconds; `action` (optional, `{label, run}`) adds a button to it,
 * e.g. "Details" that opens the Store menu.
 */
export function toast(message, kind = 'info', ms = 5000, action = null) {
  const id = ++toastId;
  state.toasts.push({ id, message, kind, action: action ? markRaw(action) : null });
  setTimeout(() => {
    state.toasts = state.toasts.filter((t) => t.id !== id);
  }, ms);
}
