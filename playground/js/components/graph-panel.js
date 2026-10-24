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
//
// Filter (js/graph/filter-model.js, popover components/graph-filter.js): the Filter button opens
// the popover under itself at the panel's left edge (never over the inspector on the right), its
// switch turns the filter off and on without losing it. The filter state is kept per graph in
// localStorage; the applied filter goes to the engine for the whole graph and the
// neighbourhood (`graphView(limits, filter)`, `neighbourhood(request, filter)`), and is applied by
// label on the client to a query result and a label sample (they carry no properties); the label
// overview draws hidden labels faded. Edits apply after 300 ms, on a graph above
// `FILTER_TUNING.manualApplyAbove` elements only on Apply; one filtered request at a time, the
// newest wins, and an engine error is shown at the condition it names while the drawing keeps the
// last good result.
//
// Vertex size (the Display menu, js/graph/size-by.js, `graph.sizeBy`): Off, Degree, In, Out. Only
// while it is not Off are the degree columns requested (`degrees: true`, `subgraph_degrees` in
// store.js `run`); the canvas gets a scale per vertex (`nodeScale`) that each renderer maps into
// its own size range.
//
// Toolbar (js/graph/toolbar.js): the row holds [Filter] [Layout] [Edit] ... [Clear highlight] [Fit]
// [Export] [⚙ ▾]; the Display menu (⚙) holds the drawing tier, the ID and label switches, the
// highlight switch and the mouse help. The row is measured (a ResizeObserver): when it is narrow,
// Layout, Edit and Filter move into the Display menu and the right-hand buttons shrink to icons.

import { ref, computed, watch, onMounted, onBeforeUnmount, nextTick, markRaw, toRaw } from 'vue';
import { state, saveSettings, newTab, reveal, isDark } from '../store.js';
import { labelPalette, labelSwatch, paletteLabels } from '../graph/palette.js';
import { PanelFrame, PanelTabs, FitButton, ExportMenu } from './panel-frame.js';
import { DropMenu } from './drop-menu.js';
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
  neighbourhoodRequest, expandRequest, redrawRequest, neighbourhoodIds, carryPositions, selectionNotice, resultNotice,
} from '../graph/selection.js';
import { editModeState, applyEdit } from '../graph/edit-mode.js';
import { resultHighlight, createDismissals } from '../graph/highlight.js';
import { FILTER_ENABLED, TOOLBAR_GAP, SHORTENED, toolbarFit, filterBadge, GRAPH_HELP, displayTitle } from '../graph/toolbar.js';
import {
  FILTER_TUNING, emptyFilter, normalizeFilter, filterStorageKey, buildFilter, activeSpec, errorTarget, applyMode,
  createApplyQueue, labelVisible, labelFilterView, filteredText, keyTypesOf,
} from '../graph/filter-model.js';
import { statisticsModel } from '../statistics.js';
import { GraphFilterDrawer } from './graph-filter.js';
import { SIZE_BY_CHOICES, normalizeSizeBy, nodeScaleFor } from '../graph/size-by.js';

const LAYOUT_IDS = LAYOUTS.map((l) => l.id);
const HOP_CHOICES = Array.from({ length: SELECTION_TUNING.maxHops }, (_, i) => i + 1);
const fmt = (n) => n.toLocaleString('en-US');
// The dismissed result highlights, kept outside the component: they outlive a re-mounted panel.
const dismissals = createDismissals();
const dismissVersion = ref(0);
const message = (err) => (err && err.message ? err.message : String(err));

export const GraphPanel = {
  components: { PanelFrame, PanelTabs, GraphCanvas, FitButton, ExportMenu, DropMenu, GraphFilterDrawer },
  setup() {
    const source = ref(normalizeSource(storage.get('graph.source', 'graph')));
    const layout = ref(storage.get('graph.layout', 'cose'));
    const edgeLabels = ref(storage.get('graph.edgeLabels', true));
    const showIds = ref(storage.get('graph.showIds', true));
    const edgeIds = ref(storage.get('graph.edgeIds', false));
    const highlightResult = ref(true);
    const sizeBy = ref(normalizeSizeBy(storage.get('graph.sizeBy', 'off')));
    /** `request` asking for the degree columns too, while the vertex size uses them. */
    const deg = (request) => (sizeBy.value !== 'off' ? { ...request, degrees: true } : request);
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

    // ------------------------------------------------------------------ filter (state)
    // `fstate`: the filter state as edited (filter-model.js), persisted per graph; `applied`: the
    // GraphFilter the drawings use now (null: the whole graph), `appliedBuilt` the build it came
    // from (to place an engine error at its condition).
    const filterKey = computed(() => filterStorageKey(state.graph));
    const fstate = ref(normalizeFilter(storage.get(filterKey.value, null)));
    const vocab = ref({ vertexLabels: [], edgeLabels: [], types: { vertex: {}, edge: {} }, labelsFor: null, typesFor: null });
    const vocabLoading = ref(null); // 'labels' | 'keys' | null
    const vocabError = ref(null);
    const built = computed(() => buildFilter(fstate.value, {
      vertexLabels: vocab.value.vertexLabels.map((l) => l.label),
      edgeLabels: vocab.value.edgeLabels.map((l) => l.label),
      types: vocab.value.types,
      othersVisible: true, // hidden labels by name: labels beyond the statistics' list stay visible
    }));
    const wanted = computed(() => activeSpec(fstate.value, built.value));
    const applied = ref(null);
    let appliedBuilt = null;
    const filterError = ref(null); // {message, target: {field, label, index}|null}
    // A stored filter with restrictions is applied once the labels and key types are read (the
    // badge and the drawer need them; the spec itself names only hidden labels and conditions):
    // drawings wait until `filterSettled`.
    const needsLabels = (m) => !!m && m.enabled && (m.hideIsolated || (m.vertices.off.length + m.edges.off.length) > 0
      || Object.keys(m.vertices.conds).length + Object.keys(m.edges.conds).length > 0);
    const filterSettled = ref(!needsLabels(fstate.value));
    const filterSpec = () => (applied.value ? toRaw(applied.value) : null);

    // ------------------------------------------------------------------ whole graph
    const sliceRequested = ref(false); // the user asked for the first N vertices of a large graph
    const plan = computed(() => {
      if (!state.graph) return { mode: 'whole', notice: null };
      return wholeGraphPlan({
        vertexCount: state.graph.vertexCount,
        edgeCount: state.graph.edgeCount,
        ...limits.value,
        // A filter draws what passes it, the engine cutting at the limits: never the overview.
        sliceRequested: sliceRequested.value || !!applied.value,
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

    // One whole-graph request at a time, the newest wins (filter-model.js `createApplyQueue`).
    const viewQueue = createApplyQueue(
      ({ limits: l, filter: f }) => state.engine.graphView(l, f),
      {
        onResult(v) {
          view.value = markRaw(v);
          filterError.value = null;
          loading.value = false;
        },
        onError(err, { filter: f, built: b }) {
          loading.value = false;
          if (f && isFilterError(err)) {
            filterError.value = { message: message(err), target: errorTarget(message(err), b) };
            return; // the drawing keeps the last good result
          }
          error.value = message(err);
          view.value = null;
        },
      },
    );
    function loadView() {
      if (source.value === 'graph') drill.value = null;
      if (!state.engine || !state.graph || plan.value.mode === 'overview') {
        view.value = null;
        return;
      }
      if (wholeHeld.value) {
        view.value = null; // not even fetched until the user agrees
        return;
      }
      if (!filterSettled.value) return; // the stored filter's labels are read first
      loading.value = true;
      error.value = null;
      viewQueue.push({ limits: deg({ ...limits.value }), filter: filterSpec(), built: appliedBuilt });
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
    // The label overview: labels the applied filter hides are drawn faded (an edge pair when its
    // label or one of its ends is hidden).
    const metaDrawing = computed(() => {
      if (!meta.value) return null;
      const d = metaGraphDrawing(meta.value);
      const f = applied.value;
      if (f) {
        const m = meta.value;
        const hidden = d.nodes.map((n) => n.pick.kind === 'label' && !labelVisible(f, 'vertices', m.labels[n.pick.index].label));
        d.nodes.forEach((n, i) => { if (hidden[i]) n.faded = true; });
        d.edges.forEach((e, j) => {
          const pair = m.edges[j];
          if (!labelVisible(f, 'edges', pair.label) || hidden[pair.from] || hidden[pair.to]) e.faded = true;
        });
      }
      return markRaw(d);
    });

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

    let selectSeq = 0;
    async function select(fetch, next) {
      const seq = ++selectSeq;
      const f = filterSpec();
      const b = appliedBuilt;
      nbBusy.value = true;
      error.value = null;
      try {
        const v = await fetch();
        if (seq !== selectSeq) return false; // a newer selection superseded this one
        filterError.value = null;
        const { positions, ...rest } = next;
        const xy = positions ? positions(v) : null;
        nbPositions.value = xy ? markRaw(xy) : null;
        nb.value = { ...rest, view: markRaw(v) };
        if (next.spec) storage.set('graph.selection', next.spec);
        return true;
      } catch (err) {
        if (seq !== selectSeq) return false;
        if (f && isFilterError(err)) filterError.value = { message: message(err), target: errorTarget(message(err), b) };
        else error.value = message(err);
        return false;
      } finally {
        if (seq === selectSeq) nbBusy.value = false;
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
      return select(() => state.engine.neighbourhood(deg(neighbourhoodRequest(ids, hops, limits.value, direction)), filterSpec()), {
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
      select(() => state.engine.labelSample(deg(s.request)), {
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
      select(() => state.engine.neighbourhood(deg(expandRequest(cur.view, index, limits.value, nbDirection.value)), filterSpec()), {
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
      const ok = await select(() => state.engine.neighbourhood(deg(expandRequest(view, index, limits.value, direction)), filterSpec()), {
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
      const keep = neighbourhoodIds(cur.view);
      if (!keep.length) return;
      select(() => state.engine.neighbourhood(deg({ ids: [], keep, hops: 0, ...limits.value }), filterSpec()), {
        ...cur,
        positions: (next) => carryPositions(cur.view, before, next),
      });
    }

    /** Restores the last neighbourhood or sample after a reload. */
    function restoreSelection() {
      const spec = storage.get('graph.selection', null);
      if (!spec || nb.value || !state.engine || !state.graph || !filterSettled.value) return;
      if (spec.kind === 'neighbourhood' && Array.isArray(spec.ids)) {
        const hops = normalizeDepth(spec.hops);
        const direction = normalizeDirection(spec.direction);
        select(() => state.engine.neighbourhood(deg(neighbourhoodRequest(spec.ids, hops, limits.value, direction)), filterSpec()), {
          hops, direction, expanded: 0, spec,
        });
      } else if (spec.kind === 'sample' && spec.request) {
        select(() => state.engine.labelSample(deg({ ...spec.request, ...limits.value, maxNodes: Math.min(SELECTION_TUNING.sampleNodes, limits.value.maxNodes) })), {
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
    watch(sizeBy, (v, before) => {
      storage.set('graph.sizeBy', v);
      // Switched on: read the drawings again with the degree columns (a query result keeps the
      // degrees within the drawing until it runs again). Switched between modes or off: nothing to read.
      if (before !== 'off' || v === 'off') return;
      // The same vertices come back: keep their positions (unless a layout is still running).
      if (cyRef.value && !(layoutState.value && layoutState.value.running)) cyRef.value.keepPositions();
      loadView();
      const cur = nb.value;
      if (cur && cur.spec && cur.spec.kind === 'neighbourhood') refreshNeighbourhood();
      else if (cur && cur.spec && cur.spec.kind === 'sample') {
        select(() => state.engine.labelSample(deg(cur.spec.request)), { ...cur });
      }
    });

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
    // A query result and a label sample carry no properties: the filter applies by label there.
    const filteredResult = computed(() => (applied.value && resultView.value ? markRaw(labelFilterView(resultView.value, applied.value)) : resultView.value));
    const nbView = computed(() => {
      const cur = nb.value;
      if (!cur) return null;
      return cur.sampleTitle && applied.value ? markRaw(labelFilterView(cur.view, applied.value)) : cur.view;
    });
    const base = computed(() => {
      if (overview.value) return { data: metaDrawing.value, kind: 'schema' };
      if (source.value === 'result') return { data: filteredResult.value, kind: 'data' };
      if (source.value === 'neighbourhood') return { data: nbView.value, kind: 'data' };
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
    /** Vertex size by degree: a scale per vertex of the data drawing (null: Off, or the overview). */
    const nodeScale = computed(() => (kind.value === 'data' && elements.value ? nodeScaleFor(elements.value, sizeBy.value) : null));
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
        if (p.mode === 'slice' && applied.value && !sliceRequested.value) {
          // Filtered: the engine draws the first vertices that pass, up to the limit.
          const v = view.value;
          if (!v || !v.truncated) return null;
          return { text: `${fmt(v.matchedNodes ?? 0)} vertices and ${fmt(v.matchedEdges ?? 0)} edges pass the filter, more than the limits of ${fmt(limits.value.maxNodes)} vertices and ${fmt(limits.value.maxEdges)} edges: the first ones are drawn. Narrow the filter or use the Neighbourhood view.`, links: [{ id: 'raise', label: 'Double the limits' }] };
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
        await select(() => state.engine.neighbourhood(deg({ ids: [], keep: neighbourhoodIds(nb.value.view), hops: 0, ...limits.value }), filterSpec()), { ...nb.value });
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

    // ------------------------------------------------------------------ toolbar
    // The row's fit (js/graph/toolbar.js): which items stay inline, which the Display menu shows.
    const toolbarEl = ref(null);
    const itemWidths = {}; // natural widths of the items, kept while an item is collapsed
    const compactWidths = {}; // widths of the shortened items (measured while compact)
    const fitState = ref({ inline: [], inMenu: [], compact: false });
    const inMenu = (id) => fitState.value.inMenu.includes(id);
    let refits = 0; // re-fits in a row without a new outside cause (a guard against flip-flopping)
    function measureToolbar(again = false) {
      refits = again ? refits + 1 : 0;
      if (refits > 4) return;
      const row = toolbarEl.value;
      if (!row || !row.isConnected) return;
      const cs = getComputedStyle(row);
      // One gap more: the flex gap between the left and the right group.
      const available = row.clientWidth - parseFloat(cs.paddingLeft) - parseFloat(cs.paddingRight) - TOOLBAR_GAP;
      if (available <= 0) return; // hidden (a collapsed panel)
      for (const el of row.querySelectorAll('[data-tb]')) {
        const id = el.dataset.tb;
        // Shortened items are not measured: their natural width is the full one.
        const into = fitState.value.compact && SHORTENED.has(id) ? compactWidths : itemWidths;
        const w = el.getBoundingClientRect().width;
        if (w > 0) into[id] = Math.ceil(w);
      }
      const next = toolbarFit({ available, widths: itemWidths, compactWidths, show: { filter: FILTER_ENABLED, clear: !!highlight.value } });
      const cur = fitState.value;
      if (next.compact !== cur.compact || next.inMenu.join() !== cur.inMenu.join() || next.inline.join() !== cur.inline.join()) {
        fitState.value = next;
        nextTick(() => measureToolbar(true)); // an item shown again is measured anew
      }
    }
    let toolbarObserver = null;
    onMounted(() => {
      if (typeof ResizeObserver === 'function' && toolbarEl.value) {
        toolbarObserver = new ResizeObserver(() => measureToolbar());
        toolbarObserver.observe(toolbarEl.value);
      }
      measureToolbar();
    });
    onBeforeUnmount(() => { if (toolbarObserver) toolbarObserver.disconnect(); });
    watch(() => [!!highlight.value, layout.value, exportBusy.value], () => nextTick(measureToolbar), { flush: 'post' });
    const displayTitleText = computed(() => {
      const drawn = drill.value || isMeta.value ? 'Detailed' : (tier.value === 'fast' ? 'Fast' : 'Detailed');
      const choice = TIER_CHOICES.find((c) => c.id === tierChoice.value);
      return displayTitle(tierChoice.value === 'auto' ? `Auto · ${drawn}` : (choice ? choice.label : drawn));
    });

    // ------------------------------------------------------------------ filter (drawer, applying)
    const filterOpen = ref(false);
    // The popover's top: just below the toolbar, under the Filter button (measured when it opens;
    // also when Filter sits in the Display menu, the popover opens at the left under the toolbar).
    const drawerTop = ref(38);
    const drawerStyle = computed(() => ({ top: drawerTop.value + 'px', maxHeight: `calc(100% - ${drawerTop.value + 8}px)` }));
    /** The Filter button's state: `count` = active restrictions, `enabled`, `open` (the drawer). */
    const filter = computed(() => ({ count: built.value.count, enabled: fstate.value.enabled, open: filterOpen.value }));
    const graphSize = computed(() => (state.graph ? state.graph.vertexCount + state.graph.edgeCount : 0));
    const filterMode = computed(() => applyMode(graphSize.value));
    const specText = (f) => (f ? JSON.stringify(f) : '');
    // The popover's colour swatch per vertex label (palette.js `labelSwatch`): the colour the
    // drawings give the label. The current drawing's labels are assigned first, through the same
    // `paletteLabels` order graph-canvas.js uses, so the swatch never waits for the canvas; a label
    // never drawn in this page has no slot yet and shows a neutral outline (its slot depends on the
    // order labels are first drawn). Recomputed with the drawing, the labels and the theme.
    const vertexSwatches = computed(() => {
      const dark = isDark();
      if (elements.value) labelPalette.assign(paletteLabels(elements.value, kind.value));
      const out = {};
      for (const l of vocab.value.vertexLabels) out[l.label] = labelSwatch(labelPalette, l.label, dark);
      return out;
    });
    const filterDirty = computed(() => specText(wanted.value) !== specText(applied.value));
    const filterBusy = computed(() => !!applied.value && (loading.value || nbBusy.value));

    /** Reads the graph's labels (statistics) and, with `keys`, the property keys and types (schema). */
    let vocabSeq = 0;
    async function loadVocab({ keys = false, force = false } = {}) {
      if (!state.engine || !state.graph) return;
      const version = `${state.graphEpoch}:${state.graphVersion}:${state.dataVersion}`;
      const keysVersion = `${state.graphEpoch}:${state.dataVersion}`;
      const needLabels = force || vocab.value.labelsFor !== version;
      const needKeys = keys && (force || vocab.value.typesFor !== keysVersion);
      if (!needLabels && !needKeys) return;
      const seq = ++vocabSeq;
      vocabError.value = null;
      try {
        if (needLabels) {
          vocabLoading.value = 'labels';
          const counts = statisticsModel(await state.engine.statistics()).counts;
          if (seq !== vocabSeq) return;
          vocab.value = {
            ...vocab.value,
            vertexLabels: counts ? counts.vertexLabels.map((l) => ({ label: l.label, count: l.count })) : [],
            edgeLabels: counts ? counts.edgeLabels.map((l) => ({ label: l.label, count: l.count })) : [],
            // More labels than statistics() lists: the filter cannot name the others.
            vertexLabelTotal: counts ? counts.vertexLabelTotal : 0,
            edgeLabelTotal: counts ? counts.edgeLabelTotal : 0,
            labelsFor: version,
          };
        }
        if (needKeys) {
          // The stored schema when it declares labels, else the inferred one (walks the graph:
          // only when the drawer opens, never on every change).
          vocabLoading.value = 'keys';
          const stored = await state.engine.getSchema().catch(() => null);
          const declares = !!stored && ['vertices', 'edges'].some((k) => stored[k] && Object.keys(stored[k]).length);
          const schema = declares ? stored : await state.engine.inferSchema();
          if (seq !== vocabSeq) return;
          vocab.value = { ...vocab.value, types: keyTypesOf(schema), typesFor: keysVersion };
        }
      } catch (err) {
        if (seq === vocabSeq) vocabError.value = `The filter could not read the graph's labels or keys: ${message(err)}`;
      } finally {
        if (seq === vocabSeq) vocabLoading.value = null;
      }
    }

    /** Whether an engine rejection is about the filter (its messages start with "filter"). */
    function isFilterError(err) {
      return /\bfilter\b/.test(message(err));
    }

    /** Makes the edited filter the applied one and redraws what depends on it. */
    function applyFilter() {
      clearTimeout(applyTimer);
      const next = wanted.value;
      if (specText(next) === specText(applied.value)) return;
      applied.value = next ? markRaw(JSON.parse(JSON.stringify(next))) : null;
      appliedBuilt = built.value;
      filterError.value = null;
      redrawFiltered();
    }
    function redrawFiltered() {
      loadView(); // the whole graph's view, also while another view is shown (it is kept)
      const cur = nb.value;
      if (!cur) {
        if (source.value === 'neighbourhood') restoreSelection();
        return;
      }
      if (cur.sampleTitle || !cur.spec || cur.spec.kind !== 'neighbourhood') return; // a sample: by label, on the client
      // The same vertices before any filter (the centre with every expansion the user drilled into:
      // `unfiltered.vertexIds` of a filtered answer, selection.js `redrawRequest`), filtered anew,
      // keeping the positions of the vertices that stay.
      const before = source.value === 'neighbourhood' && !drill.value && cyRef.value ? cyRef.value.positions() : null;
      select(() => state.engine.neighbourhood(deg(redrawRequest(cur.view, cur.spec.ids, limits.value)), filterSpec()), {
        ...cur,
        positions: (next) => carryPositions(cur.view, before, next),
      });
    }
    let applyTimer = null;
    watch(() => specText(wanted.value), () => {
      if (!filterSettled.value) return;
      if (filterMode.value === 'auto') {
        clearTimeout(applyTimer);
        applyTimer = setTimeout(applyFilter, FILTER_TUNING.debounceMs);
      }
    });
    watch(fstate, (m) => storage.set(filterKey.value, m), { deep: true });
    watch(filterKey, (k) => {
      clearTimeout(applyTimer);
      fstate.value = normalizeFilter(storage.get(k, null));
      applied.value = null;
      appliedBuilt = null;
      filterError.value = null;
      filterSettled.value = !needsLabels(fstate.value);
      if (!filterSettled.value) settleFilter();
    });
    /** A stored filter with restrictions: read the labels, apply it, then draw. */
    async function settleFilter() {
      await loadVocab();
      filterSettled.value = true;
      clearTimeout(applyTimer);
      const next = wanted.value;
      applied.value = next ? markRaw(JSON.parse(JSON.stringify(next))) : null;
      appliedBuilt = built.value;
      loadAll();
    }
    onMounted(() => { if (!filterSettled.value) settleFilter(); });
    // New labels may appear with a change of the data: re-read them while the drawer is open or a
    // filter is applied (statistics only; the keys are read again when the drawer opens).
    watch(() => [state.graphVersion, state.dataVersion, state.graphEpoch], () => {
      if (filterOpen.value || applied.value) loadVocab();
    });

    function openFilter() {
      filterOpen.value = !filterOpen.value;
      if (filterOpen.value && toolbarEl.value) drawerTop.value = toolbarEl.value.offsetTop + toolbarEl.value.offsetHeight + 4;
      if (filterOpen.value) loadVocab({ keys: true });
      else nextTick(focusFilterButton);
    }
    function closeFilter() {
      filterOpen.value = false;
      nextTick(focusFilterButton);
    }
    function focusFilterButton() {
      const el = toolbarEl.value && toolbarEl.value.querySelector(inMenu('filter') ? '.display-btn' : '.filter-btn .split-main');
      if (el) el.focus({ preventScroll: true });
    }
    /** On/off without losing the filter: applies at once, also on a large graph. */
    function toggleFilter() {
      fstate.value = { ...fstate.value, enabled: !fstate.value.enabled };
      if (fstate.value.enabled && !vocab.value.labelsFor) {
        loadVocab().then(applyFilter);
        return;
      }
      applyFilter();
    }
    /** The strip's Clear: switches the filter off (it is kept). */
    function clearFilter() {
      if (!fstate.value.enabled) return;
      fstate.value = { ...fstate.value, enabled: false };
      applyFilter();
    }
    /** The drawer's Reset: deletes every condition and shows every label. */
    function resetFilter() {
      fstate.value = emptyFilter();
      applyFilter();
    }
    function onFilterUpdate(next) {
      fstate.value = next;
    }
    async function openFilterQuery() {
      if (!state.engine) return;
      try {
        const text = await state.engine.graphFilterQuery(built.value.spec || {});
        const tab = newTab({ script: `// The vertices and edges the Graph panel's filter shows (before any drawing limit).\n${text}`, name: 'Graph filter' });
        if (tab) reveal('editor');
      } catch (err) {
        filterError.value = { message: message(err), target: errorTarget(message(err), built.value) };
      }
    }

    /** The "Filtered: X of Y" strip above the drawing (an applied filter only). */
    const filteredStrip = computed(() => {
      if (!applied.value || drill.value || held.value) return null;
      if (overview.value) {
        const f = applied.value;
        const hidden = vocab.value.vertexLabels.filter((l) => !labelVisible(f, 'vertices', l.label)).length
          + vocab.value.edgeLabels.filter((l) => !labelVisible(f, 'edges', l.label)).length;
        return `Filtered: ${hidden} label${hidden === 1 ? '' : 's'} hidden, drawn faded in the overview`;
      }
      if (source.value === 'result') {
        const r = resultView.value;
        const f = filteredResult.value;
        if (!r || !f) return null;
        if (f === r) return `Filtered by label: all ${fmt(vertexCount(r))} vertices and ${fmt(edgeCount(r))} edges of the query result pass`;
        return filteredText(f, 'result', { vertices: vertexCount(r), edges: edgeCount(r) });
      }
      if (source.value === 'neighbourhood') {
        const cur = nb.value;
        if (!cur) return null;
        if (cur.sampleTitle) {
          const f = nbView.value;
          if (f === cur.view) return 'Filtered by label: everything in this sample passes';
          return filteredText(f, 'sample', { vertices: vertexCount(cur.view), edges: edgeCount(cur.view) });
        }
        return filteredText(cur.view, 'neighbourhood');
      }
      return view.value ? filteredText(view.value, 'graph') : null;
    });

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
      toolbarEl, fitState, inMenu, displayTitleText, GRAPH_HELP, FILTER_ENABLED, filter, filterBadge, openFilter, toggleFilter,
      sizeBy, SIZE_BY_CHOICES, nodeScale,
      fstate, vocab, vocabLoading, vocabError, built, filterError, filterMode, filterDirty, filterBusy, filterOpen,
      closeFilter, clearFilter, resetFilter, drawerStyle, vertexSwatches, onFilterUpdate, applyFilter, openFilterQuery, filteredStrip,
    };
  },
  template: `
  <panel-frame title="Graph" refresh-title="Reload the drawing from the engine and run the layout again" @refresh="refresh">
    <template #tabs><panel-tabs :tabs="SOURCES" v-model="source"></panel-tabs></template>
    <div class="graph-wrap">
      <div ref="toolbarEl" class="panel-toolbar graph-toolbar">
        <div class="toolbar-main">
          <!-- The Filter slot (FILTER_ENABLED in js/graph/toolbar.js): a split button; its main part
               opens the filter drawer, its right part switches the filter on and off. -->
          <span v-if="FILTER_ENABLED && !inMenu('filter')" data-tb="filter" class="split-btn filter-btn" :class="{ off: !filter.enabled }">
            <button type="button" class="btn small split-main" :class="{ active: filter.open }" :aria-expanded="String(filter.open)"
                    title="Filter what the drawing shows" @click="openFilter">⚲ Filter<span v-if="filterBadge(filter)" class="filter-badge"> {{ filterBadge(filter) }}</span></button>
            <button type="button" class="btn small split-toggle" role="switch" :aria-checked="String(filter.enabled)"
                    :aria-label="filter.enabled ? 'Switch the filter off' : 'Switch the filter on'" :title="filter.enabled ? 'Switch the filter off' : 'Switch the filter on'"
                    @click="toggleFilter"><span class="switch-track" aria-hidden="true"><span class="switch-knob"></span></span></button>
          </span>
          <label v-if="!inMenu('layout')" data-tb="layout">Layout
            <select v-model="layout">
              <option v-for="l in LAYOUTS" :key="l.id" :value="l.id" :disabled="controls.layouts[l.id].disabled" :title="controls.layouts[l.id].reason || ''">{{ l.label }}</option>
            </select>
          </label>
          <span v-if="!inMenu('edit')" data-tb="edit" :title="editTitle" class="edit-toggle-wrap"><button type="button" class="btn small edit-toggle" :class="{ active: edit.active }"
                  :aria-pressed="String(editOn && !edit.disabled)" :disabled="edit.disabled" @click="editOn = !editOn"
                  :aria-label="'Edit mode' + (edit.disabled ? ' (' + edit.reason + ')' : '')">✎ Edit</button></span>
        </div>
        <div class="toolbar-end">
          <span v-if="highlight" data-tb="clear" class="tb-item"><button type="button" class="btn small clear-highlight" @click="clearHighlight"
                  aria-label="Clear highlight"
                  title="Show the graph normally again (also Esc on the drawing). The next query result highlights again.">✕ {{ fitState.compact ? 'Clear' : 'Clear highlight' }}</button></span>
          <span data-tb="fit" class="tb-item"><fit-button :compact="fitState.compact" @fit="fit"></fit-button></span>
          <span data-tb="export" class="tb-item"><export-menu :formats="EXPORT_FORMATS" :busy="exportBusy" :compact="fitState.compact" @export="onExport"></export-menu></span>
          <span data-tb="display" class="tb-item"><drop-menu button-class="btn small display-btn" :title="displayTitleText" aria-label="Display settings" menu-style="width: 300px">
            <template #trigger>
              <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true">
                <circle cx="8" cy="8" r="2.2"/><path d="M8 1.5v2M8 12.5v2M1.5 8h2M12.5 8h2M3.4 3.4l1.4 1.4M11.2 11.2l1.4 1.4M3.4 12.6l1.4-1.4M11.2 4.8l1.4-1.4"/>
              </svg>
              {{ fitState.compact ? '▾' : 'Display ▾' }}
            </template>
            <template v-if="fitState.inMenu.length">
              <div class="menu-title">Toolbar</div>
              <button v-if="FILTER_ENABLED && inMenu('filter')" type="button" class="menu-item" @click="openFilter">⚲ Filter<span v-if="filterBadge(filter)" class="filter-badge"> {{ filterBadge(filter) }}</span></button>
              <label v-if="FILTER_ENABLED && inMenu('filter') && filter.count" class="menu-item menu-indent" title="Switch the filter off and on (it is kept)">
                <input type="checkbox" :checked="filter.enabled" @change="toggleFilter"> Filter on</label>
              <label v-if="inMenu('layout')" class="menu-item menu-field">Layout
                <select v-model="layout">
                  <option v-for="l in LAYOUTS" :key="l.id" :value="l.id" :disabled="controls.layouts[l.id].disabled" :title="controls.layouts[l.id].reason || ''">{{ l.label }}</option>
                </select>
              </label>
              <div v-if="inMenu('edit')" :title="editTitle"><button type="button" class="menu-item edit-toggle" :class="{ active: edit.active }"
                      :aria-pressed="String(editOn && !edit.disabled)" :disabled="edit.disabled" @click="editOn = !editOn"
                      :aria-label="'Edit mode' + (edit.disabled ? ' (' + edit.reason + ')' : '')">✎ Edit mode{{ editOn && !edit.disabled ? ' (on)' : '' }}</button>
                <div v-if="edit.disabled && edit.reason" class="menu-note">Unavailable: {{ edit.reason }}</div>
              </div>
              <div class="menu-sep"></div>
            </template>
            <div class="menu-title">Drawing tier</div>
            <div class="menu-row">
              <span class="segmented" role="group" aria-label="Drawing tier">
                <button v-for="c in TIER_CHOICES" :key="c.id" type="button" class="seg-btn" :class="{ active: tierChoice === c.id }"
                        :aria-pressed="String(tierChoice === c.id)" @click="setTier(c.id)"
                        :disabled="isMeta || (c.id === 'fast' && (!webgl || !!fastError))"
                        :title="isMeta ? 'The label overview is always drawn in the Detailed tier' : (c.id === 'fast' && (!webgl || fastError) ? 'The Fast tier is not available: ' + (fastError || 'this browser has no WebGL') : c.title)">
                  {{ c.label }}<template v-if="c.id === 'auto' && tierChoice === 'auto'"> · {{ drill || isMeta ? 'Detailed' : (tier === 'fast' ? 'Fast' : 'Detailed') }}</template>
                </button>
              </span>
            </div>
            <div v-if="isMeta" class="menu-note">The label overview is always drawn in the Detailed tier.</div>
            <div v-else-if="!webgl || fastError" class="menu-note">The Fast tier is not available: {{ fastError || 'this browser has no WebGL' }}.</div>
            <div class="menu-sep"></div>
            <div class="menu-title">Show</div>
            <label class="menu-item" title="Show each vertex id on its node"><input type="checkbox" v-model="showIds" :disabled="isMeta"> Vertex IDs</label>
            <label class="menu-item" :title="controls.edgeLabels.reason || 'Draw each edge with its label'"><input type="checkbox" v-model="edgeLabels" :disabled="controls.edgeLabels.disabled"> Edge labels</label>
            <label class="menu-item menu-indent" :title="controls.edgeIds.reason || 'Show each edge id next to its label'"><input type="checkbox" v-model="edgeIds" :disabled="!edgeLabels || controls.edgeIds.disabled || isMeta"> Edge IDs</label>
            <label v-if="source === 'graph' && !drill && !isMeta" class="menu-item" :title="subgraph ? 'Fade everything the last result does not refer to' : 'Run a query that returns elements'">
              <input type="checkbox" v-model="highlightResult" :disabled="!subgraph"> Highlight result
            </label>
            <div class="menu-sep"></div>
            <div class="menu-title">Vertex size</div>
            <div class="menu-row">
              <span class="segmented" role="group" aria-label="Vertex size">
                <button v-for="c in SIZE_BY_CHOICES" :key="c.id" type="button" class="seg-btn" :class="{ active: sizeBy === c.id }"
                        :aria-pressed="String(sizeBy === c.id)" :disabled="isMeta && c.id !== 'off'"
                        :title="isMeta ? 'The label overview sizes a label by its vertex count' : c.title" @click="sizeBy = c.id">{{ c.label }}</button>
              </span>
            </div>
            <div class="menu-sep"></div>
            <div class="menu-title">Mouse</div>
            <ul class="menu-help">
              <li v-for="h in GRAPH_HELP" :key="h">{{ h }}</li>
            </ul>
          </drop-menu></span>
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
      <div v-if="filteredStrip" class="notice info filtered-strip" role="status">{{ filteredStrip }} ·
        <a href="#" @click.prevent="clearFilter" title="Switch the filter off (it is kept: the Filter switch turns it on again)">Clear</a>
        <template v-if="!filterOpen"> · <a href="#" @click.prevent="openFilter">Edit</a></template>
      </div>
      <div v-if="filterError && !filterOpen" class="notice" role="alert">The filter was not applied: {{ filterError.message }}
        <a href="#" @click.prevent="openFilter">Edit the filter</a></div>
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
      <graph-canvas ref="cyRef" :elements="elements" :kind="kind" :tier="tier" :layout="layout" :edge-labels="edgeLabels" :show-ids="showIds" :edge-ids="edgeIds" :node-scale="nodeScale" :highlight="highlight" :empty-text="emptyText"
                    :expand-from-engine="expandFromEngine" :initial-positions="initialPositions" :edit-mode="edit.active" @edited="onEdited"
                    @layout-note="layoutNote = $event" @layout-state="layoutState = $event" @tier-failed="onTierFailed" @pick="pick = $event"
                    @activate="openSample" @expand="expand" @clear-highlight="clearHighlight">
        <template #inspector-actions="{ element }">
          <div v-if="element && element.kind === 'vertex' && element.id !== undefined" class="inspector-actions">
            <a href="#" @click.prevent="showNeighbourhood(element.id)" title="Draw this vertex and its neighbours, fetched from the engine (the Neighbourhood view)">Neighbourhood</a>
          </div>
        </template>
      </graph-canvas>
      <graph-filter-drawer v-if="filterOpen" :style="drawerStyle" :model="fstate" :vertex-labels="vocab.vertexLabels" :edge-labels="vocab.edgeLabels"
                           :vertex-swatches="vertexSwatches" :vertex-label-total="vocab.vertexLabelTotal || 0" :edge-label-total="vocab.edgeLabelTotal || 0"
                           :types="vocab.types" :built="built" :engine-error="filterError" :mode="filterMode" :dirty="filterDirty"
                           :busy="filterBusy" :loading="vocabLoading" :load-error="vocabError" :source="overview ? 'labels' : source"
                           @update="onFilterUpdate" @close="closeFilter" @apply="applyFilter" @toggle="toggleFilter"
                           @reset="resetFilter" @open-query="openFilterQuery"></graph-filter-drawer>
    </div>
  </panel-frame>
  `,
};
