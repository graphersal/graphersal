// The graph view of a UI profile (EXPERIMENTAL): what `api.graph(data, options)` renders
// (js/profile-api.js). It draws the module's `{nodes, edges}` with the Graph panel's drawing code
// (the renderer interface js/graph/renderer.js: Cytoscape in the Detailed tier, sigma.js on WebGL
// in the Fast tier for large views, chosen by js/graph/tiers.js; the layouts of
// js/graph/controller.js), styled per label by the profile's `graph` key and per element by the
// module (js/profile-graph.js, the pure model). It never calls a drawing library itself.
//
// Gestures: click = select (highlight with its neighbours, `onSelect`, the side card), double
// click on a node = `onExpand`, double click on empty space = fit, Esc on the focused drawing =
// clear the selection. Toolbar: Fit, zoom out/in, the layout and Relayout (Stop while the
// engine's force layout runs). A view that changes (an expansion merged in, new styles) keeps
// the positions of the vertices it had and places the new ones next to their neighbours
// (`carryPositions`): no new layout, the view (zoom, pan) stays.
//
// Layout engine: the engine's force layout (Barnes-Hut in the worker or on the dev server)
// takes over above the `cose` cap like in the Graph panel; a server in allow-list mode answers no
// layout request, so there `cose` runs up to NO_ENGINE_COSE_MAX vertices and larger views fall
// back to a grid with a note.

import { h, ref, computed, watch, onMounted, onBeforeUnmount, markRaw } from 'vue';
import { state, isDark } from '../store.js';
import { debounce } from '../util.js';
import { rendererFactory } from '../graph/renderer.js';
import { GraphController } from '../graph/controller.js';
import { createResizeGuard } from '../graph/resize-guard.js';
import { chooseTier, detectWebGL } from '../graph/tiers.js';
import { labelPalette, slotColor } from '../graph/palette.js';
import { isClearKey } from '../graph/highlight.js';
import { DRAW_TUNING } from '../graph-data.js';
import {
  checkGraphData, styledView, legendEntries, highlightIds, selectionOf, carryPositions, GRAPH_LAYOUTS,
} from '../profile-graph.js';

/** Without an engine layout (allow-list mode), `cose` lays out at most this many vertices. */
export const NO_ENGINE_COSE_MAX = 1500;

/** The layout menu's names. */
const LAYOUT_NAMES = Object.freeze({ cose: 'Force', circle: 'Circle', concentric: 'Concentric', breadthfirst: 'Tree', grid: 'Grid' });

const message = (err) => (err && err.message ? err.message : String(err));

function cssVar(name) {
  if (typeof document === 'undefined') return '';
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

/** A legend swatch: the node's shape, colour, border and opacity, or the edge's line. */
function swatch(entry) {
  const s = entry.style;
  const dash = (style, w) => (style === 'dashed' ? `${3 * w + 1} ${2 * w + 1}` : style === 'dotted' ? `${w} ${1.6 * w}` : null);
  if (entry.kind === 'edge') {
    const w = Math.min(4, Math.max(1, s.width));
    return h('svg', { class: 'profile-graph-swatch edge', viewBox: '0 0 28 12', width: 28, height: 12, 'aria-hidden': 'true' }, [
      h('line', { x1: 1, y1: 6, x2: s.arrow ? 21 : 27, y2: 6, stroke: s.color, 'stroke-width': w, 'stroke-dasharray': dash(s.lineStyle, w), opacity: s.opacity }),
      s.arrow ? h('path', { d: 'M20 2 L27 6 L20 10 Z', fill: s.color, opacity: s.opacity }) : null,
    ]);
  }
  const bw = Math.min(3, s.borderWidth);
  const stroke = bw > 0 ? { stroke: s.borderColor, 'stroke-width': bw, 'stroke-dasharray': dash(s.borderStyle, Math.max(1, bw)) } : {};
  const attrs = { fill: s.color, opacity: s.opacity, ...stroke };
  const c = 8;
  const r = 6.2 - bw / 2;
  let shape;
  switch (s.shape) {
    case 'rectangle': shape = h('rect', { x: c - r, y: c - r, width: 2 * r, height: 2 * r, ...attrs }); break;
    case 'round-rectangle': shape = h('rect', { x: c - r, y: c - r, width: 2 * r, height: 2 * r, rx: 3, ...attrs }); break;
    case 'diamond': shape = h('polygon', { points: `${c},${c - r} ${c + r},${c} ${c},${c + r} ${c - r},${c}`, ...attrs }); break;
    case 'triangle': shape = h('polygon', { points: `${c},${c - r} ${c + r},${c + r} ${c - r},${c + r}`, ...attrs }); break;
    case 'hexagon': {
      const pts = [0, 1, 2, 3, 4, 5].map((k) => `${c + r * Math.cos((Math.PI / 3) * k)},${c + r * Math.sin((Math.PI / 3) * k)}`);
      shape = h('polygon', { points: pts.join(' '), ...attrs });
      break;
    }
    case 'star': {
      const pts = [];
      for (let k = 0; k < 10; k++) {
        const rr = k % 2 ? r * 0.45 : r;
        const a = (Math.PI / 5) * k - Math.PI / 2;
        pts.push(`${c + rr * Math.cos(a)},${c + rr * Math.sin(a)}`);
      }
      shape = h('polygon', { points: pts.join(' '), ...attrs });
      break;
    }
    default: shape = h('circle', { cx: c, cy: c, r, ...attrs });
  }
  return h('svg', { class: 'profile-graph-swatch', viewBox: '0 0 16 16', width: 16, height: 16, 'aria-hidden': 'true' }, [shape]);
}

export const ProfileGraph = {
  name: 'ProfileGraph',
  props: {
    data: { default: null },
    options: { type: Object, required: true }, // js/profile-graph.js `checkGraphOptions`
    spec: { type: Object, default: () => ({}) }, // the profile's `graph` key
    renderCard: { type: Function, default: null }, // (label, row, options) => VNode (api.card)
  },
  setup(props) {
    const host = ref(null);
    const failed = ref(null);
    const selection = ref(null); // {kind, id} | null
    const layoutState = ref(null);
    const note = ref(null);
    const layout = ref(props.options.layout);
    const expanding = ref(false);
    const tier = ref('detailed');
    const tierNote = ref(null);
    const darkTick = ref(0); // bumped when the theme changed (the colours are read again)
    let renderer = null;
    let controller = null;
    let fastError = null;
    let shownSig = null;
    let shownIds = null;
    let lastExpanded = null;
    let observer = null;
    let queue = Promise.resolve();
    let userMoved = false;
    const webgl = typeof document !== 'undefined' ? detectWebGL() : false;
    const engine = () => (state.engineInfo && state.engineInfo.apiMode === 'allow-list' ? null : state.engine);

    /** The normalized data, or the error that says why it cannot be drawn. */
    const checked = computed(() => {
      try {
        return { graph: checkGraphData(props.data || { nodes: [], edges: [] }), error: null };
      } catch (err) {
        return { graph: { nodes: [], edges: [] }, error: err };
      }
    });

    const ctx = () => {
      void darkTick.value;
      const dark = isDark();
      return {
        styleOf: props.options.styleOf,
        labelColor: (label) => slotColor(labelPalette.slot(label), dark),
        token: (name) => cssVar(`--${name}`),
        borderColor: cssVar('--graph-text') || '#1c2330',
        edgeColor: cssVar('--graph-edge') || '#9aa3b2',
        neutralColor: cssVar('--text-faint') || '#8a93a3',
      };
    };

    /** The styled view of the current data (or the error of a module's `styleOf`). */
    const view = computed(() => {
      const graph = checked.value.graph;
      try {
        return { view: styledView(graph, props.spec || {}, ctx()), error: null };
      } catch (err) {
        return { view: styledView({ nodes: [], edges: [] }, {}, ctx()), error: err };
      }
    });

    const legend = computed(() => {
      if (props.options.legend === false) return [];
      try {
        return legendEntries(checked.value.graph, props.spec || {}, Array.isArray(props.options.legend) ? props.options.legend : [], ctx());
      } catch (err) {
        return [];
      }
    });

    const isHidden = () => !host.value || !host.value.clientWidth || !host.value.clientHeight;

    const signature = (v) => JSON.stringify([v.vertexIds, v.edgeIds, Array.from(v.edgeSource), Array.from(v.edgeTarget), v.nodeStyle, v.edgeStyle, v.edgeName]);

    function applySelection() {
      if (!renderer) return;
      const graph = checked.value.graph;
      if (selection.value && !selectionOf(graph, selection.value)) selection.value = null;
      renderer.select(highlightIds(graph, selection.value, props.options.highlight));
    }

    function choose(id, kind) {
      const next = id === null ? null : { kind, id };
      selection.value = next;
      applySelection();
      if (props.options.onSelect) props.options.onSelect(selectionOf(checked.value.graph, next));
    }

    async function expand(sel) {
      if (!sel || sel.kind !== 'node' || !props.options.onExpand) return;
      lastExpanded = sel.id;
      expanding.value = true;
      try {
        await props.options.onExpand(sel);
      } finally {
        expanding.value = false;
      }
    }

    function wire(r) {
      r.onClick((target) => {
        const v = view.value.view;
        if (!target) choose(null);
        else choose(target.group === 'node' ? v.vertexIds[target.index] : v.edgeIds[target.index], target.group);
      });
      r.onDoubleClick((target) => {
        const v = view.value.view;
        if (!target) {
          userMoved = false;
          r.fit();
          return;
        }
        if (target.group !== 'node') return;
        const id = v.vertexIds[target.index];
        if (!selection.value || selection.value.id !== id) choose(id, 'node');
        expand(selectionOf(checked.value.graph, { kind: 'node', id }));
      });
      r.onUserMove(() => { userMoved = true; });
    }

    async function makeRenderer(want) {
      let make;
      try {
        make = await rendererFactory(want);
      } catch (err) {
        if (want !== 'fast') throw err;
        fastError = message(err);
        want = 'detailed';
        make = await rendererFactory(want);
      }
      return { want, make };
    }

    function wantedTier(size) {
      const choice = chooseTier({ size, choice: 'auto', previous: tier.value, webgl, fastError });
      tierNote.value = choice.tier === 'fast'
        ? 'Large view: drawn by the Fast tier (WebGL): every node is a circle, every line solid.'
        : choice.notice;
      return choice.tier;
    }

    async function sync() {
      if (!controller || !host.value) return;
      const v = view.value.view;
      const sig = signature(v);
      if (sig === shownSig) {
        applySelection();
        return;
      }
      const old = renderer.getPositions();
      const want = wantedTier(v.vertexIds.length + v.edgeIds.length);
      if (want !== tier.value) {
        const made = await makeRenderer(want);
        if (!host.value) return;
        tier.value = made.want;
        await controller.replaceRenderer(() => {
          renderer = markRaw(made.make(host.value, { dark: isDark() }));
          wire(renderer);
          return renderer;
        }, { redraw: false });
      }
      const positions = shownIds && old ? carryPositions(shownIds, old, checked.value.graph, { anchor: lastExpanded, spacing: 90 }) : null;
      shownSig = sig;
      shownIds = v.vertexIds.slice();
      if (!positions) userMoved = false;
      const shown = controller.show(v, {
        kind: 'data', layout: layout.value, edgeLabels: true, palette: labelPalette, positions, keepView: !!positions,
      });
      applySelection();
      await shown;
    }

    const schedule = () => {
      queue = queue.then(sync, sync).catch((err) => { failed.value = message(err); });
      return queue;
    };

    onMounted(async () => {
      const v = view.value.view;
      let made;
      try {
        made = await makeRenderer(wantedTier(v.vertexIds.length + v.edgeIds.length));
      } catch (err) {
        failed.value = `The drawing could not start: ${message(err)}`;
        return;
      }
      if (!host.value) return;
      tier.value = made.want;
      try {
        renderer = markRaw(made.make(host.value, { dark: isDark() }));
      } catch (err) {
        failed.value = `The drawing could not start: ${message(err)}`;
        return;
      }
      wire(renderer);
      const eng = engine();
      controller = markRaw(new GraphController({
        renderer,
        engine: eng,
        isHidden,
        onState: (s) => {
          layoutState.value = s.layout;
          note.value = s.note;
        },
        tuning: eng ? DRAW_TUNING : { ...DRAW_TUNING, coseMaxNodes: NO_ENGINE_COSE_MAX },
      }));
      // The controller draws through the renderer it holds (a tier swap replaces it).
      const current = () => controller.renderer;
      let wasHidden = isHidden();
      const guard = createResizeGuard();
      if (!wasHidden) guard.changed(host.value.getBoundingClientRect());
      const onResize = debounce(() => {
        if (!controller || !host.value) return;
        if (isHidden()) {
          wasHidden = true;
          guard.reset();
          return;
        }
        if (!guard.changed(host.value.getBoundingClientRect()) && !wasHidden && controller.pending === null) return;
        current().resize();
        if (controller.pending !== null) controller.resume();
        else if (wasHidden || !userMoved) current().fit();
        wasHidden = false;
      }, 80);
      observer = new ResizeObserver(onResize);
      observer.observe(host.value);
      schedule();
    });

    onBeforeUnmount(() => {
      if (observer) observer.disconnect();
      if (controller) controller.destroy();
      controller = null;
      renderer = null;
    });

    watch(() => [props.data, props.options, props.spec], schedule);
    watch(() => (props.options.selected ? `${props.options.selected.kind}:${props.options.selected.id}` : ''), (key) => {
      const sel = props.options.selected;
      if (!key) {
        if (selection.value) selection.value = null;
      } else if (!selection.value || selection.value.kind !== sel.kind || selection.value.id !== sel.id) {
        selection.value = { kind: sel.kind, id: sel.id };
      }
      applySelection();
    }, { immediate: true });
    watch(() => [state.theme, state.systemDark], () => {
      // Wait for the theme attribute to apply before reading the CSS tokens.
      requestAnimationFrame(() => {
        if (!controller) return;
        controller.renderer.setTheme(isDark());
        darkTick.value++;
        shownSig = null;
        schedule();
      });
    });

    const zoom = (factor) => {
      const r = controller && controller.renderer;
      if (r && typeof r.zoomBy === 'function') {
        userMoved = true;
        r.zoomBy(factor);
      }
    };
    const fit = () => {
      if (!controller) return;
      userMoved = false;
      controller.renderer.fit();
    };
    const relayout = () => {
      if (!controller) return;
      userMoved = false;
      controller.applyLayout(layout.value);
    };
    const onKey = (e) => {
      if (isClearKey(e) && selection.value) {
        e.preventDefault();
        choose(null);
      }
    };

    function toolbar() {
      const v = view.value.view;
      const running = layoutState.value && layoutState.value.running;
      const zoomable = !!(controller && typeof controller.renderer.zoomBy === 'function');
      return h('div', { class: 'profile-graph-toolbar', role: 'toolbar', 'aria-label': 'Graph view' }, [
        h('button', { type: 'button', class: 'btn small', onClick: fit, title: 'Fit the drawing into the view (or double click on empty space)' }, 'Fit'),
        h('button', { type: 'button', class: 'btn small', onClick: () => zoom(1 / 1.3), disabled: !zoomable, 'aria-label': 'Zoom out', title: 'Zoom out' }, '−'),
        h('button', { type: 'button', class: 'btn small', onClick: () => zoom(1.3), disabled: !zoomable, 'aria-label': 'Zoom in', title: 'Zoom in' }, '+'),
        h('label', { class: 'profile-graph-layout' }, [
          h('span', { class: 'sr-only' }, 'Layout'),
          h('select', {
            value: layout.value,
            'aria-label': 'Layout',
            onChange: (e) => { layout.value = e.target.value; relayout(); },
          }, GRAPH_LAYOUTS.map((id) => h('option', { value: id }, LAYOUT_NAMES[id] || id))),
        ]),
        running
          ? h('button', { type: 'button', class: 'btn small', onClick: () => controller && controller.cancelLayout() },
            `Stop layout (${Math.round(100 * (layoutState.value.progress || 0))} %)`)
          : h('button', { type: 'button', class: 'btn small', onClick: relayout, title: 'Lay the drawing out again' }, 'Relayout'),
        h('span', { class: 'spacer' }),
        h('span', { class: 'profile-graph-counts muted' }, `${v.vertexIds.length} nodes · ${v.edgeIds.length} edges`),
      ]);
    }

    function legendRow() {
      if (!legend.value.length) return null;
      return h('ul', { class: 'profile-graph-legend', 'aria-label': 'Legend' }, legend.value.map((entry, k) => h('li', { key: k, class: entry.extra ? 'extra' : '' }, [
        swatch(entry), h('span', null, entry.text),
      ])));
    }

    function sideCard() {
      const card = props.options.card;
      if (!card || !selection.value || !props.renderCard) return null;
      const sel = selectionOf(checked.value.graph, selection.value);
      if (!sel) return null;
      const opts = { variant: 'full' };
      if (typeof card === 'object') {
        const actions = typeof card.actions === 'function' ? card.actions(sel) : card.actions;
        if (Array.isArray(actions)) opts.actions = actions;
        if (card.onAction) opts.onAction = (id) => card.onAction(id, sel);
      }
      const option = typeof card === 'object' && card.expand !== undefined ? card.expand : 'Expand';
      const expandText = typeof option === 'function' ? option(sel) : option;
      const canExpand = sel.kind === 'node' && props.options.onExpand && typeof expandText === 'string' && expandText.trim() !== '';
      return h('aside', { class: 'profile-graph-side', 'aria-label': `Selected ${sel.kind}` }, [
        h('div', { class: 'profile-graph-side-head' }, [
          h('span', { class: 'badge' }, sel.label || sel.kind),
          h('span', { class: 'spacer' }),
          canExpand
            ? h('button', { type: 'button', class: 'btn small', disabled: expanding.value, onClick: () => expand(sel), title: 'Add its neighbours to the drawing (or double click the node)' },
              expanding.value ? 'Working…' : expandText)
            : null,
          h('button', { type: 'button', class: 'icon-btn', 'aria-label': 'Close', onClick: () => choose(null) }, '×'),
        ]),
        props.renderCard(sel.label, sel.data, opts),
      ]);
    }

    return () => {
      const dataError = checked.value.error || view.value.error;
      const empty = !dataError && !checked.value.graph.nodes.length;
      return h('div', { class: 'profile-graph' }, [
        toolbar(),
        dataError ? h('div', { class: 'profile-graph-error', role: 'alert' }, message(dataError)) : null,
        h('div', { class: 'profile-graph-body' }, [
          h('div', { class: 'profile-graph-stage', style: { height: props.options.height } }, [
            h('div', {
              class: 'cy-canvas',
              ref: host,
              role: 'img',
              tabindex: 0,
              'aria-label': 'Graph drawing (click selects, double click on a node expands it, Esc clears the selection)',
              onKeydown: onKey,
            }),
            failed.value ? h('div', { class: 'graph-empty' }, failed.value) : empty ? h('div', { class: 'graph-empty' }, props.options.emptyText) : null,
          ]),
          sideCard(),
        ]),
        (note.value || tierNote.value) ? h('p', { class: 'profile-graph-note muted', role: 'status' }, [note.value, tierNote.value].filter(Boolean).join(' ')) : null,
        legendRow(),
      ]);
    };
  },
};
