// Graph panel: the VIEW SELECTOR (js/graph/selection.js) and the drawing. Sources: the whole
// graph when it fits the limits (else its labels), the label meta-graph (double click on a label
// or a label pair draws a sample), the neighbourhood of a vertex fetched from the engine (double
// click expands a vertex from the engine, the drawing keeps its positions; a double click in the
// query result's drawing does the same and continues as a neighbourhood), and the last query
// result. Data drawings use the Detailed tier (Cytoscape) or the Fast tier (sigma.js, WebGL),
// chosen by size with hysteresis or by the user's switch (js/graph/tiers.js); the label
// meta-graph is drawn like the schema, always Detailed. In Fast a picked vertex's neighbourhood
// within the drawing can be opened in Detailed ("drill down").
//
// Edit mode (the toolbar's switch, js/graph/edit-mode.js): the mouse adds vertices and edges,
// deletes and edits them (components/graph-canvas.js). It is off for the label overview and while
// the graph is read-only (a past state's view, a backup, maintenance mode: the engine's reason in
// the tooltip); the Fast tier edits and deletes but adds nothing (`controlStates().addElements`).
// After a change the panel patches the drawing it shows (`applyEdit`): the whole graph and a
// neighbourhood are then read again from the engine (the neighbourhood keeps the new vertex), the
// query result's drawing and a drill-down stay patched.

import { ref, computed, watch, onMounted, markRaw, toRaw } from 'vue';
import { state, saveSettings } from '../store.js';
import { PanelFrame, PanelTabs, FitButton, ExportMenu } from './panel-frame.js';
import { exportFileName, downloadBlob } from '../export.js';
import { GraphCanvas, LAYOUTS } from './graph-canvas.js';
import { storage } from '../util.js';
import {
  vertexCount, edgeCount, elementCount, idsOf, capToElements, neighbourhood, subView, vertexCaption,
} from '../graph-data.js';
import { TIER_CAPABILITIES } from '../graph/renderer.js';
import {
  TIER_TUNING, TIER_CHOICES, chooseTier, controlStates, tierBanner, detectWebGL, normalizeChoice,
} from '../graph/tiers.js';
import {
  VIEW_SOURCES, SELECTION_TUNING, DIRECTIONS, normalizeSource, normalizeDirection, normalizeDepth, wholeGraphPlan, metaGraphDrawing, metaNotice, sampleRequestFor,
  neighbourhoodRequest, expandRequest, carryPositions, selectionNotice, resultNotice,
} from '../graph/selection.js';
import { editModeState, applyEdit } from '../graph/edit-mode.js';
import { resultHighlight, createDismissals } from '../graph/highlight.js';

const LAYOUT_IDS = LAYOUTS.map((l) => l.id);
const HOP_CHOICES = Array.from({ length: SELECTION_TUNING.maxHops }, (_, i) => i + 1);
const fmt = (n) => n.toLocaleString('en-US');
// The dismissed result highlights, kept outside the component: they outlive a re-mounted panel.
const dismissals = createDismissals();
const dismissVersion = ref(0);
const message = (err) => (err && err.message ? err.message : String(err));

export const GraphPanel = {
  components: { PanelFrame, PanelTabs, GraphCanvas, FitButton, ExportMenu },
  setup() {
    const source = ref(normalizeSource(storage.get('graph.source', 'graph')));
    const layout = ref(storage.get('graph.layout', 'cose'));
    const edgeLabels = ref(storage.get('graph.edgeLabels', true));
    const showIds = ref(storage.get('graph.showIds', true));
    const edgeIds = ref(storage.get('graph.edgeIds', false));
    const highlightResult = ref(true);
    const view = ref(null); // the whole graph (or its first-N slice)
    const loading = ref(false);
    const error = ref(null);
    const cyRef = ref(null);
    const layoutNote = ref(null); // why the drawing uses another layout than the chosen one
    const layoutState = ref(null); // the engine layout in progress: {running, progress, vertexCount, ...}
    const limits = computed(() => ({ maxNodes: state.settings.graphMaxNodes, maxEdges: state.settings.graphMaxEdges }));

    // Tier: the user's switch (settings), WebGL, and the last automatic choice (hysteresis).
    const tierChoice = computed(() => normalizeChoice(state.settings.graphTier));
    const webgl = detectWebGL();
    const fastError = ref(null); // the Fast renderer failed to start (offline, blocked)
    const lastTier = ref(null);
    const tierInput = (size, choice = tierChoice.value) => ({ size, choice, previous: lastTier.value, webgl, fastError: fastError.value });

    // Drill down: a neighbourhood of the Fast drawing, drawn in Detailed. `pick` is the vertex the
    // canvas reports (click or double-click selection).
    const pick = ref(null);
    const drill = ref(null); // {view, title, hops, from}

    // ------------------------------------------------------------------ whole graph
    const sliceRequested = ref(false); // the user asked for the first N vertices of a large graph
    const plan = computed(() => {
      if (!state.graph) return { mode: 'whole', notice: null };
      return wholeGraphPlan({
        vertexCount: state.graph.vertexCount,
        edgeCount: state.graph.edgeCount,
        ...limits.value,
        sliceRequested: sliceRequested.value,
      });
    });
    const overview = computed(() => source.value === 'labels' || (source.value === 'graph' && plan.value.mode === 'overview'));

    // Size guard: a drawing whose tier decision asks for confirmation waits for an explicit "Draw
    // it". `allowed` is the largest size agreed to in this page view; it is not persisted, so a
    // reload never starts a large drawing by itself.
    const allowed = ref(0);
    const expectedWhole = computed(() => {
      if (!state.graph) return 0;
      return Math.min(state.graph.vertexCount, limits.value.maxNodes) + Math.min(state.graph.edgeCount, limits.value.maxEdges);
    });
    const wholeHeld = computed(() => {
      const d = chooseTier(tierInput(expectedWhole.value));
      return !!d.confirm && d.confirm.size > allowed.value;
    });

    async function loadView() {
      if (source.value === 'graph') drill.value = null;
      if (!state.engine || !state.graph || plan.value.mode === 'overview') {
        view.value = null;
        return;
      }
      if (wholeHeld.value) {
        view.value = null; // not even fetched until the user agrees
        return;
      }
      loading.value = true;
      error.value = null;
      try {
        view.value = markRaw(await state.engine.graphView(limits.value));
      } catch (err) {
        error.value = message(err);
        view.value = null;
      } finally {
        loading.value = false;
      }
    }

    // ------------------------------------------------------------------ labels (meta-graph)
    const meta = ref(null);
    const metaFor = ref(-1); // the graphVersion `meta` was fetched for
    async function loadMeta(force = false) {
      if (!state.engine || !state.graph || !overview.value) return;
      if (!force && meta.value && metaFor.value === state.graphVersion) return;
      const version = state.graphVersion;
      loading.value = true;
      error.value = null;
      try {
        const m = await state.engine.metaGraph();
        if (version !== state.graphVersion) return;
        meta.value = markRaw(m);
        metaFor.value = version;
      } catch (err) {
        error.value = `The label overview failed: ${message(err)}`;
      } finally {
        loading.value = false;
      }
    }
    const metaDrawing = computed(() => (meta.value ? markRaw(metaGraphDrawing(meta.value)) : null));

    // ------------------------------------------------------------------ neighbourhood / sample
    // `nb`: {view, title?, hops, expanded, sampleTitle?, from?, spec}; `spec` re-creates it after
    // a reload ({kind: 'neighbourhood', ids, hops} | {kind: 'sample', request, title}).
    const nb = ref(null);
    const nbPositions = ref(null); // positions to draw the next neighbourhood view at (an expansion)
    const nbInput = ref(storage.get('graph.neighbourhood.id', ''));
    const nbHops = ref(normalizeDepth(storage.get('graph.neighbourhood.hops', SELECTION_TUNING.defaultHops)));
    const nbDirection = ref(normalizeDirection(storage.get('graph.neighbourhood.direction', 'both')));
    const nbBusy = ref(false);
    // Changing the depth or the direction redraws from the same centre.
    watch([nbHops, nbDirection], ([hops, direction]) => {
      storage.set('graph.neighbourhood.hops', hops);
      storage.set('graph.neighbourhood.direction', direction);
      const spec = nb.value && nb.value.spec;
      if (spec && spec.kind === 'neighbourhood') showNeighbourhood(spec.ids[0], hops);
    });

    async function select(fetch, next) {
      nbBusy.value = true;
      error.value = null;
      try {
        const v = await fetch();
        const { positions, ...rest } = next;
        const xy = positions ? positions(v) : null;
        nbPositions.value = xy ? markRaw(xy) : null;
        nb.value = { ...rest, view: markRaw(v) };
        if (next.spec) storage.set('graph.selection', next.spec);
        return true;
      } catch (err) {
        error.value = message(err);
        return false;
      } finally {
        nbBusy.value = false;
      }
    }

    /** Draws the neighbourhood of vertex `id` within `hops` (from the engine). */
    function showNeighbourhood(id, hops = normalizeDepth(nbHops.value)) {
      const ids = [String(id).trim()];
      if (!ids[0] || !state.engine) return Promise.resolve(false);
      drill.value = null;
      nbInput.value = ids[0];
      storage.set('graph.neighbourhood.id', ids[0]);
      source.value = 'neighbourhood';
      const direction = nbDirection.value;
      return select(() => state.engine.neighbourhood(neighbourhoodRequest(ids, hops, limits.value, direction)), {
        hops, direction, expanded: 0, spec: { kind: 'neighbourhood', ids, hops, direction },
      });
    }

    /** Draws a sample of the meta-graph element `pick` (a double click in the Labels view). */
    function openSample(target) {
      const drawing = metaDrawing.value;
      if (!drawing || !target) return;
      const el = target.group === 'node' ? drawing.nodes[target.index] : drawing.edges[target.index];
      const s = el && sampleRequestFor(meta.value, el.pick, limits.value);
      if (!s) return;
      const from = source.value;
      source.value = 'neighbourhood';
      select(() => state.engine.labelSample(s.request), {
        hops: 0, expanded: 0, sampleTitle: s.title, from, spec: { kind: 'sample', request: s.request, title: s.title, from },
      });
    }

    /**
     * Expands vertex `index` by one hop, from the engine: in the neighbourhood drawing, and in the
     * query result's drawing, which then continues as a neighbourhood (the result's vertices kept in
     * place) so further double clicks keep expanding. A result above the vertex limit is not expanded
     * (the panel says so).
     */
    function expand({ index }) {
      if (nbBusy.value) return;
      if (source.value === 'result') {
        expandResult(index);
        return;
      }
      const cur = nb.value;
      if (!cur) return;
      const anchor = cur.view.vertexIds[index];
      const before = cyRef.value ? cyRef.value.positions() : null;
      select(() => state.engine.neighbourhood(expandRequest(cur.view, index, limits.value, nbDirection.value)), {
        ...cur,
        expanded: cur.expanded + 1,
        positions: (next) => carryPositions(cur.view, before, next, anchor),
      });
    }

    async function expandResult(index) {
      const view = subgraph.value;
      if (!view || !state.engine) return;
      if (vertexCount(view) > limits.value.maxNodes) {
        error.value = `The query result has more vertices than the drawing limit (${limits.value.maxNodes}): open a vertex's neighbourhood instead (the inspector's Neighbourhood link) or raise the limit in Settings.`;
        return;
      }
      const anchor = view.vertexIds[index];
      const before = cyRef.value ? cyRef.value.positions() : null;
      const direction = nbDirection.value;
      const ok = await select(() => state.engine.neighbourhood(expandRequest(view, index, limits.value, direction)), {
        hops: 1, direction, expanded: 1, title: 'the query result, expanded', from: 'result',
        spec: { kind: 'neighbourhood', ids: [String(anchor)], hops: 1, direction },
        positions: (next) => carryPositions(view, before, next, anchor),
      });
      // Switch only after the expanded view is there: the source watcher would otherwise restore
      // the previous neighbourhood first.
      if (ok) source.value = 'neighbourhood';
    }

    /** Re-reads the neighbourhood drawing after a graph change: the same vertices, current edges. */
    function refreshNeighbourhood() {
      const cur = nb.value;
      if (!cur || !state.engine) return;
      const before = cyRef.value ? cyRef.value.positions() : null;
      const keep = Array.from(cur.view.vertexIds, String);
      if (!keep.length) return;
      select(() => state.engine.neighbourhood({ ids: [], keep, hops: 0, ...limits.value }), {
        ...cur,
        positions: (next) => carryPositions(cur.view, before, next),
      });
    }

    /** Restores the last neighbourhood or sample after a reload. */
    function restoreSelection() {
      const spec = storage.get('graph.selection', null);
      if (!spec || nb.value || !state.engine || !state.graph) return;
      if (spec.kind === 'neighbourhood' && Array.isArray(spec.ids)) {
        const hops = normalizeDepth(spec.hops);
        const direction = normalizeDirection(spec.direction);
        select(() => state.engine.neighbourhood(neighbourhoodRequest(spec.ids, hops, limits.value, direction)), {
          hops, direction, expanded: 0, spec,
        });
      } else if (spec.kind === 'sample' && spec.request) {
        select(() => state.engine.labelSample({ ...spec.request, ...limits.value, maxNodes: Math.min(SELECTION_TUNING.sampleNodes, limits.value.maxNodes) }), {
          hops: 0, expanded: 0, sampleTitle: spec.title, from: spec.from, spec,
        });
      }
    }

    function backFromSample() {
      source.value = nb.value && nb.value.from === 'graph' ? 'graph' : 'labels';
    }

    // ------------------------------------------------------------------ loading and watching
    function loadAll() {
      loadView();
      loadMeta();
      if (source.value === 'neighbourhood') {
        if (nb.value) refreshNeighbourhood();
        else restoreSelection();
      }
    }
    onMounted(loadAll);
    // Separate sources (not one getter returning a new array, which would fire on every
    // saveSettings, e.g. the tier switch, and fetch and lay out the same view again).
    watch([() => state.graphVersion, () => state.settings.graphMaxNodes, () => state.settings.graphMaxEdges], () => {
      loadView();
      loadMeta(true);
      if (source.value === 'neighbourhood' && nb.value) refreshNeighbourhood();
    });
    watch(() => state.graph && state.graph.name, () => { nb.value = null; sliceRequested.value = false; });
    watch(() => plan.value.mode, () => loadView());
    watch(overview, (o) => { if (o) loadMeta(); });
    watch(source, (v) => {
      storage.set('graph.source', v);
      drill.value = null;
      pick.value = null;
      if (v === 'neighbourhood' && !nb.value) restoreSelection();
    });
    watch(() => state.graphRequest && state.graphRequest.n, () => {
      const r = state.graphRequest;
      if (r && r.source === 'neighbourhood' && r.ids && r.ids.length) showNeighbourhood(r.ids[0]);
    });
    watch(layout, (v) => storage.set('graph.layout', v));
    watch(edgeLabels, (v) => storage.set('graph.edgeLabels', v));
    watch(showIds, (v) => storage.set('graph.showIds', v));
    watch(edgeIds, (v) => storage.set('graph.edgeIds', v));

    const subgraph = computed(() => (state.tab.result ? state.tab.result.subgraph || null : null));
    watch(subgraph, () => { if (source.value === 'result') drill.value = null; });
    // The query result's drawing after changes made in edit mode: {base, view}, valid while the
    // result is `base`.
    const resultEdited = ref(null);
    const resultView = computed(() => {
      const r = resultEdited.value;
      return r && r.base === subgraph.value ? r.view : subgraph.value;
    });

    // ------------------------------------------------------------------ what is drawn
    /** `{data, kind}` of the source before the tier decision (data null: nothing yet). */
    const base = computed(() => {
      if (overview.value) return { data: metaDrawing.value, kind: 'schema' };
      if (source.value === 'result') return { data: resultView.value, kind: 'data' };
      if (source.value === 'neighbourhood') return { data: nb.value ? nb.value.view : null, kind: 'data' };
      return { data: view.value, kind: 'data' };
    });
    const isMeta = computed(() => base.value.kind === 'schema' && !drill.value);
    const baseSize = computed(() => {
      if (isMeta.value) return 0;
      if (source.value === 'graph' && !view.value) return expectedWhole.value;
      return elementCount(base.value.data);
    });

    /** The tier decision of what is drawn (a drill-down and the meta-graph are Detailed). */
    const decision = computed(() => {
      if (drill.value) return chooseTier({ size: elementCount(drill.value.view), choice: 'detailed', webgl: true });
      if (isMeta.value) return chooseTier({ size: 0, kind: 'schema' });
      return chooseTier(tierInput(baseSize.value));
    });
    watch(decision, (d) => { if (!drill.value && d.why !== 'no-fast' && d.why !== 'schema') lastTier.value = d.tier; }, { immediate: true });
    const tier = computed(() => decision.value.tier);
    const caps = computed(() => TIER_CAPABILITIES[tier.value]);
    const controls = computed(() => controlStates(caps.value, LAYOUT_IDS));

    /** The drawing waiting for confirmation: {vertices, edges, size, reason}, or null. */
    const held = computed(() => {
      const d = decision.value;
      if (!d.confirm || d.confirm.size <= allowed.value) return null;
      if (drill.value) {
        return { vertices: vertexCount(drill.value.view), edges: edgeCount(drill.value.view), size: d.confirm.size, reason: d.confirm.reason };
      }
      const data = base.value.data;
      if (data) return { vertices: vertexCount(data), edges: edgeCount(data), size: d.confirm.size, reason: d.confirm.reason };
      const g = state.graph;
      if (!g || source.value !== 'graph') return null;
      return {
        vertices: Math.min(g.vertexCount, limits.value.maxNodes),
        edges: Math.min(g.edgeCount, limits.value.maxEdges),
        size: d.confirm.size,
        reason: d.confirm.reason,
      };
    });
    function drawAnyway() {
      if (!held.value) return;
      allowed.value = held.value.size;
      if (source.value === 'graph' && !drill.value && !view.value) loadView();
    }

    const kind = computed(() => (drill.value ? 'data' : base.value.kind));
    const elements = computed(() => {
      if (held.value) return null;
      if (drill.value) return drill.value.view;
      const els = base.value.data;
      if (kind.value === 'schema') return els;
      const cap = decision.value.cap;
      return cap && els ? markRaw(capToElements(els, cap)) : els;
    });
    /** Positions for the next drawing (an expanded neighbourhood keeps its vertices in place). */
    const initialPositions = computed(() => (source.value === 'neighbourhood' && !drill.value ? nbPositions.value : null));
    const expandFromEngine = computed(() => (source.value === 'neighbourhood' || source.value === 'result') && !drill.value);

    // The result highlight (js/graph/highlight.js): "Clear highlight" / Esc dismiss it for this
    // result only; a later result, or another tab's, highlights again.
    const highlight = computed(() => {
      void dismissVersion.value; // re-read after a dismissal
      return resultHighlight({
        source: source.value, overview: overview.value, drill: !!drill.value, enabled: highlightResult.value,
        subgraph: subgraph.value, dismissed: (s) => dismissals.isDismissed(toRaw(s)), idsOf,
      });
    });
    /** Leaves the highlight: the drawing is shown normally again (and the double-click selection cleared). */
    function clearHighlight() {
      if (cyRef.value) cyRef.value.clearFocus();
      if (subgraph.value && dismissals.dismiss(toRaw(subgraph.value))) dismissVersion.value = dismissals.version;
    }

    // ------------------------------------------------------------------ notices
    /** `{text, links: [{id, label}]}` of what is drawn and how to see more. */
    const notice = computed(() => {
      if (drill.value) return null;
      if (source.value === 'graph') {
        const p = plan.value;
        if (p.mode === 'overview') {
          return { text: p.notice, links: [{ id: 'slice', label: `Draw the first ${fmt(limits.value.maxNodes)} vertices anyway` }, { id: 'raise', label: 'Double the limits' }] };
        }
        if (p.mode === 'slice') return { text: p.notice, links: [{ id: 'overview', label: 'Back to the labels' }, { id: 'raise', label: 'Double the limits' }] };
        return null;
      }
      if (source.value === 'labels') return meta.value ? { text: metaNotice(meta.value), links: [] } : null;
      if (source.value === 'neighbourhood') {
        const cur = nb.value;
        if (!cur) return null;
        const links = cur.sampleTitle ? [{ id: 'back', label: cur.from === 'graph' ? 'Back to the whole graph' : 'Back to the labels' }] : [];
        return { text: selectionNotice(cur.view, {
          hops: cur.hops, direction: cur.direction, expanded: cur.expanded, sampleTitle: cur.sampleTitle, title: cur.title, ...limits.value,
        }), links };
      }
      if (source.value === 'result') {
        const text = resultNotice(subgraph.value, state.settings.graphResultMax);
        return text ? { text, links: [] } : null;
      }
      return null;
    });
    /** Overview notice in the whole-graph source: the meta-graph's own notice under it. */
    const metaDetail = computed(() => (source.value === 'graph' && overview.value && meta.value ? metaNotice(meta.value) : null));
    function onNoticeLink(id) {
      if (id === 'slice') sliceRequested.value = true;
      else if (id === 'overview') sliceRequested.value = false;
      else if (id === 'raise') raiseLimit();
      else if (id === 'back') backFromSample();
    }

    /** The tier banner: what Fast lacks, a forced tier's warning, or the fallback notice. */
    const tierNotes = computed(() => {
      if (drill.value || held.value || !elements.value || isMeta.value) return [];
      const d = decision.value;
      const out = [];
      if (d.notice) out.push(d.notice);
      if (d.warning) out.push(d.warning);
      const banner = tierBanner(caps.value, { why: d.why, size: elementCount(elements.value) });
      if (banner) out.push(banner);
      return out;
    });

    function setTier(choice) {
      allowed.value = 0;
      saveSettings({ graphTier: normalizeChoice(choice) });
    }
    function onTierFailed(reason) {
      fastError.value = reason || 'unknown error';
    }

    /** The drill-down offer: a picked vertex of the Fast drawing. */
    const drillOffer = computed(() => {
      const p = pick.value;
      const els = elements.value;
      if (!p || drill.value || tier.value !== 'fast' || !els || kind.value !== 'data' || p.index >= vertexCount(els)) return null;
      const text = p.count
        ? ` and its neighbours (${p.count} vertices within ${p.hops} hop${p.hops === 1 ? '' : 's'}) selected. `
        : ' selected. ';
      return { caption: vertexCaption(els, p.index, false), hops: p.hops, count: p.count, text };
    });
    function openInDetailed() {
      const p = pick.value;
      const els = elements.value;
      if (!p || !els) return;
      const indices = (cyRef.value && cyRef.value.focusIndices()) || neighbourhood(els, [p.index], p.hops, TIER_TUNING.drillMaxNodes);
      drill.value = {
        view: markRaw(subView(els, indices)),
        title: vertexCaption(els, p.index, false),
        hops: p.hops,
        from: { result: 'the query result', neighbourhood: 'the neighbourhood' }[source.value] || 'the whole drawing',
      };
      pick.value = null;
    }
    function closeDrill() {
      drill.value = null;
    }

    const emptyText = computed(() => {
      if (held.value) return 'Not drawn yet: see the note above.';
      if (loading.value || nbBusy.value) return 'Loading…';
      if (overview.value) return meta.value ? 'The graph is empty.' : 'Loading the labels…';
      if (source.value === 'result') {
        return state.tab.result ? 'The last result refers to no vertices or edges. Try g.v(1).out_e().in_v().path() or g.v().has_label("person").' : 'Run a query that returns vertices, edges or paths.';
      }
      if (source.value === 'neighbourhood') {
        return 'Enter a vertex id above, click "Neighbourhood" in a vertex\'s properties, or double-click a label in the Labels view.';
      }
      return 'The graph is empty. Add vertices with g.add_v("person").property("name", "ann").';
    });

    const exportBusy = ref(false);
    const exportError = ref(null);
    const EXPORT_FORMATS = [
      { id: 'png', label: 'PNG (whole drawing)' },
      { id: 'png-view', label: 'PNG (visible area)' },
    ];
    async function onExport(id, { transparent }) {
      exportError.value = null;
      exportBusy.value = true;
      try {
        const blob = await cyRef.value.exportPng({ full: id === 'png', transparent });
        downloadBlob(blob, exportFileName('graph', 'png'));
      } catch (err) {
        exportError.value = `Export failed: ${message(err)}`;
      } finally {
        exportBusy.value = false;
      }
    }

    /** Header refresh: re-fetch from the engine and lay it out again. */
    async function refresh() {
      if (overview.value) await loadMeta(true);
      else if (source.value === 'graph') await loadView();
      else if (source.value === 'neighbourhood' && nb.value) {
        nbPositions.value = null;
        await select(() => state.engine.neighbourhood({ ids: [], keep: Array.from(nb.value.view.vertexIds, String), hops: 0, ...limits.value }), { ...nb.value });
      }
      if (cyRef.value) cyRef.value.relayout();
    }

    // ------------------------------------------------------------------ edit mode
    const editOn = ref(false);
    const readOnlyReason = ref(null); // why the graph cannot be written (the engine's answer)
    let readOnlySeq = 0;
    async function loadReadOnly() {
      const seq = ++readOnlySeq;
      if (!state.engine || !state.graph) return;
      if (typeof state.engine.labelChoices !== 'function') {
        readOnlyReason.value = 'this engine cannot edit the graph (reload the page)';
        return;
      }
      try {
        const choices = await state.engine.labelChoices({ kind: 'vertex' });
        if (seq === readOnlySeq) readOnlyReason.value = choices.readOnly || null;
      } catch (err) {
        if (seq === readOnlySeq) readOnlyReason.value = null; // the engine checks every write anyway
      }
    }
    onMounted(loadReadOnly);
    watch([() => state.graphVersion, () => state.storeVersion, () => state.graph && state.graph.name], loadReadOnly);
    const edit = computed(() => editModeState({
      caps: caps.value, kind: kind.value, readOnly: readOnlyReason.value, running: state.running, held: !!held.value, on: editOn.value,
    }));
    const editTitle = computed(() => edit.value.reason || (editOn.value
      ? 'Leave edit mode'
      : 'Edit mode: double-click empty space to add a vertex, drag from a vertex to another to add an edge, double-click an element to edit it, Delete removes the selected one'));

    /** A change made in edit mode: the drawing shown is patched in place (the canvas then reloads every panel). */
    function onEdited(change) {
      if (drill.value) {
        drill.value = { ...drill.value, view: markRaw(applyEdit(drill.value.view, change)) };
        return;
      }
      if (source.value === 'result') {
        resultEdited.value = { base: subgraph.value, view: markRaw(applyEdit(resultView.value, change)) };
        return;
      }
      if (source.value === 'neighbourhood' && nb.value) {
        nbPositions.value = null;
        nb.value = { ...nb.value, view: markRaw(applyEdit(nb.value.view, change)) };
        return;
      }
      if (source.value === 'graph' && view.value) view.value = markRaw(applyEdit(view.value, change));
    }

    function raiseLimit() {
      saveSettings({ graphMaxNodes: state.settings.graphMaxNodes * 2, graphMaxEdges: state.settings.graphMaxEdges * 2 });
    }

    return {
      state, SOURCES: VIEW_SOURCES, LAYOUTS, TIER_CHOICES, HOP_CHOICES, source, layout, edgeLabels, showIds, edgeIds, highlightResult,
      elements, kind, highlight, clearHighlight, notice, metaDetail, onNoticeLink, emptyText, isMeta,
      error, cyRef, subgraph, held, drawAnyway, layoutNote, layoutState, fmt,
      tier, tierChoice, setTier, tierNotes, controls, webgl, fastError, onTierFailed,
      pick, drill, drillOffer, openInDetailed, closeDrill,
      nb, nbInput, nbHops, nbDirection, DIRECTIONS, nbBusy, showNeighbourhood, openSample, expand, initialPositions, expandFromEngine,
      stopLayout: () => cyRef.value && cyRef.value.cancelLayout(),
      fit: () => cyRef.value && cyRef.value.fit(),
      refresh, EXPORT_FORMATS, onExport, exportBusy, exportError,
      editOn, edit, editTitle, onEdited,
    };
  },
  template: `
  <panel-frame title="Graph" refresh-title="Reload the drawing from the engine and run the layout again" @refresh="refresh">
    <template #tabs><panel-tabs :tabs="SOURCES" v-model="source"></panel-tabs></template>
    <div class="graph-wrap">
      <div class="panel-toolbar">
        <div class="toolbar-main">
        <span class="segmented" role="group" aria-label="Drawing tier">
          <button v-for="c in TIER_CHOICES" :key="c.id" type="button" class="seg-btn" :class="{ active: tierChoice === c.id }"
                  :aria-pressed="String(tierChoice === c.id)" @click="setTier(c.id)"
                  :disabled="isMeta || (c.id === 'fast' && (!webgl || !!fastError))"
                  :title="isMeta ? 'The label overview is always drawn in the Detailed tier' : (c.id === 'fast' && (!webgl || fastError) ? 'The Fast tier is not available: ' + (fastError || 'this browser has no WebGL') : c.title)">
            {{ c.label }}<template v-if="c.id === 'auto' && tierChoice === 'auto'"> · {{ drill || isMeta ? 'Detailed' : (tier === 'fast' ? 'Fast' : 'Detailed') }}</template>
          </button>
        </span>
        <span :title="editTitle" class="edit-toggle-wrap"><button type="button" class="btn small edit-toggle" :class="{ active: edit.active }"
                :aria-pressed="String(editOn && !edit.disabled)" :disabled="edit.disabled" @click="editOn = !editOn"
                :aria-label="'Edit mode' + (edit.disabled ? ' (' + edit.reason + ')' : '')">✎ Edit mode</button></span>
        <label>Layout
          <select v-model="layout">
            <option v-for="l in LAYOUTS" :key="l.id" :value="l.id" :disabled="controls.layouts[l.id].disabled" :title="controls.layouts[l.id].reason || ''">{{ l.label }}</option>
          </select>
        </label>
        <label title="Show each vertex id on its node"><input type="checkbox" v-model="showIds" :disabled="isMeta"> Show IDs</label>
        <label :title="controls.edgeLabels.reason || 'Draw each edge with its label'"><input type="checkbox" v-model="edgeLabels" :disabled="controls.edgeLabels.disabled"> edge labels</label>
        <label :title="controls.edgeIds.reason || 'Show each edge id next to its label'"><input type="checkbox" v-model="edgeIds" :disabled="!edgeLabels || controls.edgeIds.disabled || isMeta"> Edge IDs</label>
        <label v-if="source === 'graph' && !drill && !isMeta" :title="subgraph ? 'Fade everything the last result does not refer to' : 'Run a query that returns elements'">
          <input type="checkbox" v-model="highlightResult" :disabled="!subgraph"> highlight result
        </label>
        </div>
        <div class="toolbar-end">
          <button v-if="highlight" type="button" class="btn small clear-highlight" @click="clearHighlight"
                  title="Show the graph normally again (also Esc on the drawing). The next query result highlights again.">✕ Clear highlight</button>
          <span class="info-tip" tabindex="0" role="note" aria-label="Click an element for its properties. Double-click a vertex to select its neighbours (again: one hop more); in the Neighbourhood view it fetches them from the engine; in the Labels view double-click a label or an edge to draw a sample. Pinch or wheel to zoom, drag to pan, double-click empty space to fit."
                title="Click an element for its properties. Double-click a vertex to select its neighbours (again: one hop more); in the Neighbourhood view it fetches them from the engine; in the Labels view double-click a label or an edge to draw a sample. Pinch or wheel to zoom, drag to pan, double-click empty space to fit.">ⓘ</span>
          <fit-button @fit="fit"></fit-button>
          <export-menu :formats="EXPORT_FORMATS" :busy="exportBusy" @export="onExport"></export-menu>
        </div>
      </div>
      <form v-if="source === 'neighbourhood' && !drill" class="panel-toolbar nb-form" @submit.prevent="showNeighbourhood(nbInput)">
        <div class="toolbar-main">
          <label>Vertex id <input type="text" v-model="nbInput" size="12" placeholder="e.g. 1" aria-label="Vertex id"></label>
          <label title="1 = the direct neighbours, 2 = their neighbours too, ... (changing it redraws from the same vertex)">Depth
            <select v-model.number="nbHops" aria-label="Depth"><option v-for="h in HOP_CHOICES" :key="h" :value="h">{{ h }}</option></select></label>
          <span class="segmented" role="group" aria-label="Direction">
            <button v-for="d in DIRECTIONS" :key="d.id" type="button" class="seg-btn" :class="{ active: nbDirection === d.id }"
                    :aria-pressed="String(nbDirection === d.id)" :title="d.title" @click="nbDirection = d.id">{{ d.label }}</button>
          </span>
          <button type="submit" class="btn small" :disabled="!String(nbInput).trim() || nbBusy">Draw</button>
          <span v-if="nbBusy" class="faint">Fetching from the engine…</span>
        </div>
      </form>
      <div v-if="drill" class="notice info" role="status">
        Neighbourhood of <b>{{ drill.title }}</b> ({{ drill.view.vertexIds.length }} vertices, {{ drill.view.edgeIds.length }} edges
        within {{ drill.hops }} hop{{ drill.hops === 1 ? '' : 's' }}) in the Detailed tier.
        <a href="#" @click.prevent="closeDrill">Back to {{ drill.from }}</a>
      </div>
      <div v-if="notice && !held" class="notice info view-notice" role="status">{{ notice.text }}
        <template v-for="(l, i) in notice.links" :key="l.id"><template v-if="i"> · </template><a href="#" @click.prevent="onNoticeLink(l.id)">{{ l.label }}</a></template>
      </div>
      <div v-if="metaDetail" class="notice faint" role="status">{{ metaDetail }}</div>
      <div v-if="held" class="notice" role="status">
        This drawing has {{ fmt(held.vertices) }} vertices and {{ fmt(held.edges) }} edges. {{ held.reason }}
        <a href="#" @click.prevent="drawAnyway">Draw it</a>
        <template v-if="tierChoice === 'detailed' && webgl && !fastError"> · or <a href="#" @click.prevent="setTier('auto')">let the size choose the tier</a></template>
        <template v-else-if="source === 'graph'"> · or lower the limits in the settings</template>
        <template v-else> · or draw a smaller selection</template>.
        If the page freezes, open <a href="reset/">reset/</a>.
      </div>
      <div v-for="t in tierNotes" :key="t" class="notice faint tier-note" role="status">{{ t }}
        <a v-if="tier === 'fast'" href="#" @click.prevent="setTier('detailed')" title="Draw this in the Detailed tier (Cytoscape)">Switch to Detailed</a>
      </div>
      <div v-if="drillOffer" class="notice info" role="status">
        <b>{{ drillOffer.caption }}</b>{{ drillOffer.text }}<a href="#" @click.prevent="openInDetailed">Open {{ drillOffer.count ? 'the selection' : 'its neighbourhood' }} in Detailed</a><span
          class="faint"> · double-click a vertex to {{ expandFromEngine ? 'expand it from the engine' : 'select its neighbours, again for one hop more' }}</span>
      </div>
      <div v-if="edit.hint" class="notice info edit-hint" role="status">{{ edit.hint }}
        <template v-if="!edit.canAdd"> <a href="#" @click.prevent="setTier('detailed')" :title="controls.addElements.reason">Switch to Detailed to add</a> ·</template>
        <a href="#" @click.prevent="editOn = false">Leave edit mode</a>
      </div>
      <div v-if="error" class="notice" role="alert">{{ error }}</div>
      <div v-if="layoutNote && !held" class="notice faint" role="status">{{ layoutNote }}</div>
      <div v-if="exportError" class="notice" role="alert">{{ exportError }} <a href="#" @click.prevent="exportError = null">Dismiss</a></div>
      <div v-if="layoutState && layoutState.running && !held" class="notice" role="status">
        <progress :value="layoutState.progress" max="1" style="vertical-align: middle; width: 8em"></progress>
        Laying out {{ layoutState.vertexCount }} vertices in the engine…
        {{ Math.round(layoutState.progress * 100) }} %
        <a href="#" @click.prevent="stopLayout" title="Stop the layout and draw the positions reached so far">Stop</a>
      </div>
      <graph-canvas ref="cyRef" :elements="elements" :kind="kind" :tier="tier" :layout="layout" :edge-labels="edgeLabels" :show-ids="showIds" :edge-ids="edgeIds" :highlight="highlight" :empty-text="emptyText"
                    :expand-from-engine="expandFromEngine" :initial-positions="initialPositions" :edit-mode="edit.active" @edited="onEdited"
                    @layout-note="layoutNote = $event" @layout-state="layoutState = $event" @tier-failed="onTierFailed" @pick="pick = $event"
                    @activate="openSample" @expand="expand" @clear-highlight="clearHighlight">
        <template #inspector-actions="{ element }">
          <div v-if="element && element.kind === 'vertex' && element.id !== undefined" class="inspector-actions">
            <a href="#" @click.prevent="showNeighbourhood(element.id)" title="Draw this vertex and its neighbours, fetched from the engine (the Neighbourhood view)">Neighbourhood</a>
          </div>
        </template>
      </graph-canvas>
    </div>
  </panel-frame>
  `,
};
