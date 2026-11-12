// Hash routing without a library: `#/` is the start page, `#/play` the workspace.
//
// With the dev server (`graphersal --server`) there is no start page: the server serves one
// graph, so every route is the workspace on that graph.
//
// A playground link (`#/play?graph=..&q=..&run=1`, `&lang=gql` for a GQL tab; js/link.js) loads its sample graph, or the
// graph it carries (`data`, with its `schema`: loaded like a GraphSON file with a schema file),
// puts its query into the editor and, with run=1, runs it; the address then goes back to `#/play`,
// so a reload does not apply the link again. With the dev server only the query is opened: the
// served graph is the user's own data, so a link neither replaces it nor runs anything on it.
//
// UI profiles (js/ui-profiles.js): `#/play?tab=<id>&k=v` shows the profile tab `id` with its
// parameters (none: the profile's start tab). The WebAssembly playground keeps its profile in
// the address (`profile=<name>`, built-in names only; every hash this module writes carries it),
// and reloads the page when the address asks for another one. The dev server's profile comes
// from `GET /api/info` (a `profile` parameter there is ignored).

import { state, loadGraph, engineReady, isServerMode, attachServerGraph, openScript, run, toast } from './store.js';
import { storage } from './util.js';
import { parseLink, decodeLinkData } from './link.js';
import { routeHash, tabRouteFromHash, requestedProfile } from './ui-profiles.js';

function routeFromHash() {
  const h = window.location.hash.replace(/^#/, '');
  return h.startsWith('/play') ? 'play' : 'start';
}

/** The profile name the hashes of this page carry: the WebAssembly playground's, else none. */
function hashProfile() {
  return isServerMode() ? null : state.ui.name;
}

/** The hash of `route` ('play' | 'start') under the page's profile (`tab`: a profile tab). */
export function hashOf(route, tab = null, params = {}) {
  return routeHash({ route, profile: hashProfile(), tab, params });
}

export function navigate(route) {
  const hash = hashOf(route);
  if (window.location.hash !== hash) window.location.hash = hash;
  else applyRoute();
}

/**
 * Shows the profile tab `tab` with `params` (the profile API's `navigate`): the address changes,
 * the hash change applies it. Throws for a tab the profile does not have.
 */
export function navigateTab(tab, params = {}) {
  if (!state.ui.tab(tab)) {
    throw new Error(`The profile "${state.ui.name}" has no tab "${tab}" (its tabs: ${state.ui.tabs.map((t) => t.id).join(', ')}).`);
  }
  const hash = hashOf('play', tab, params);
  if (window.location.hash !== hash) window.location.hash = hash;
  else applyRoute();
}

/**
 * Takes the profile tab of the address (`#/play?tab=..`) into the state; an unknown tab shows
 * the start tab and says so. Returns the canonical hash of the route shown.
 */
function applyTabRoute() {
  const route = tabRouteFromHash(window.location.hash);
  let tab = route.tab;
  let params = route.params;
  if (tab !== null && !state.ui.tab(tab)) {
    toast(`The profile "${state.ui.name}" has no tab "${tab.slice(0, 40)}": it shows "${state.ui.startTab}".`, 'error', 8000);
    tab = null;
    params = {};
  }
  state.profileTab = tab || state.ui.startTab;
  state.profileParams = params;
  return hashOf('play', tab, params);
}

/** A link put its query into the editor: show the workspace tab (when it is not the start tab). */
function showWorkspaceTab() {
  const ws = state.ui.workspaceTab;
  if (!ws || state.profileTab === ws) return;
  state.profileTab = ws;
  state.profileParams = {};
  history.replaceState(null, '', hashOf('play', ws));
}

const SAMPLE_NAMES = { modern: 'Modern (TinkerPop)', empty: 'empty', large: 'large generated', tree: 'file tree' };

/** Puts a link's query into the editor (a new tab when the current one holds the user's text). */
function openLinkScript(link) {
  if (link.script === null) return null;
  return openScript({ title: link.title || 'Linked query', script: link.script, language: link.language, params: link.params });
}

/** The load request of a link that carries its graph: a GraphSON file and an optional schema file. */
async function linkedGraphRequest(link) {
  const text = await decodeLinkData(link.data);
  const schema = link.schema !== null ? await decodeLinkData(link.schema) : null;
  const request = { kind: 'file', file: new File([text], 'linked-graph.json', { type: 'application/json' }) };
  if (schema !== null) request.schemaFile = new File([schema], 'linked-schema.json', { type: 'application/json' });
  return request;
}

/** Applies a link in the browser playground. Returns true when the workspace is shown. */
async function applyLink(link) {
  let request = { kind: link.graph };
  if (link.data !== null) {
    try {
      request = await linkedGraphRequest(link);
    } catch (err) {
      toast(err && err.message ? err.message : String(err), 'error', 9000);
      return false;
    }
  }
  const what = link.data !== null ? 'the graph it carries' : `the ${SAMPLE_NAMES[link.graph]} sample graph`;
  // A graph is open in this page (the link was pasted into it): replacing it loses its changes.
  if (state.graph) {
    const ok = window.confirm(`This link loads ${what} and replaces the graph open now. Continue?`);
    if (!ok) {
      openLinkScript(link);
      state.route = 'play';
      toast('The current graph stays; the linked query is in the editor (not run).', 'info');
      return true;
    }
  }
  if (!(await loadGraph(request))) {
    if (link.data !== null) toast(`The graph in the link could not be loaded: ${state.loadError}`, 'error', 9000);
    return false;
  }
  state.route = 'play';
  const tab = openLinkScript(link);
  if (tab && link.run) await run(link.script, tab.id);
  return true;
}

export async function applyRoute() {
  // A hash change can come before the engine exists (the WebAssembly module still loads).
  await engineReady();
  if (!state.engine) return;
  // The WebAssembly playground: the address asks for another profile than the one shown (or
  // than the one that could not be used): start again with it.
  if (!isServerMode() && requestedProfile(window.location.hash) !== state.profileRequested) {
    window.location.reload();
    return;
  }
  if (state.profileError) return;
  const link = parseLink(window.location.hash);
  if (link) history.replaceState(null, '', hashOf('play'));
  if (link && link.error) toast(link.error, 'error', 8000);
  if (isServerMode()) {
    // A profile without the workspace never reads the served graph (its tabs call saved queries).
    if (!state.graph && state.ui.hasWorkspace) await attachServerGraph();
    const canonical = applyTabRoute();
    if (window.location.hash !== canonical) history.replaceState(null, '', canonical);
    state.route = 'play';
    if (link && !link.error && state.ui.hasWorkspace && openLinkScript(link)) {
      showWorkspaceTab();
      const graphNote = link.data !== null ? ' The graph in the link was not loaded either; open the link in the public playground for it.' : '';
      toast(`The linked query is in the editor. It was not run: the dev server serves your own graph.${graphNote}`, 'info', 8000);
    }
    return;
  }
  if (link && !link.error && (await applyLink(link))) {
    applyTabRoute();
    if (link.script !== null) showWorkspaceTab();
    return;
  }
  const route = routeFromHash();
  if (route === 'play') applyTabRoute();
  if (route === 'play' && !state.graph) {
    // A reload on #/play: bring back the last sample graph (files are not stored; save the graph
    // with "Save graph" and load the file again).
    const last = storage.get('lastGraph', null);
    if (['modern', 'empty', 'large', 'tree'].includes(last) && (await loadGraph({ kind: last }))) {
      state.route = 'play';
      return;
    }
    if (window.location.hash !== hashOf('start')) window.location.hash = hashOf('start');
    state.route = 'start';
    return;
  }
  state.route = route;
}
