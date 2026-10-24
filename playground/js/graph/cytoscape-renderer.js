// The DETAILED renderer: Cytoscape.js (global `cytoscape`, classic script, Canvas 2D) behind the
// renderer interface of js/graph/renderer.js. Every style, vertex and edge labels, curved edges,
// arrows, the built-in layouts, the schema drawing, PNG export.
//
// Size adaptation (graph-data.js `drawPlan`, measured in GR1): larger drawings get straight
// edges, labels without outline drawn only when readable, and a texture instead of the edges
// while the view moves; above the confirmation size haystack edges without arrows. Colours come
// from the page's label palette (js/graph/palette.js): every node has the class of its slot, the
// style colours the slots in the current theme.
//
// Elements are added lazily: `setData` keeps the data, the first `setPositions` adds them AT
// their positions in one batch (no layout pass, one paint), the first `setLayout` adds them and
// runs the layout.
//
// Edit mode (`setEditMode`, js/graph/edit-mode.js): vertices cannot be dragged; a drag from a
// vertex draws a rubber band (an SVG line over the canvas, never an element of the graph) to the
// pointer and, released on a vertex, reports the pair (`onConnect`). Panning on empty space and
// zooming stay as they are; a double click reports its position in drawing coordinates.

import { wheelZoomRatio } from './wheel-zoom.js';
import { toCyElements, drawPlan, vertexCaption, edgeCaption } from '../graph-data.js';
import { PALETTE_SIZE, slotColor } from './palette.js';
import { TIER_CAPABILITIES } from './renderer.js';
import { ConnectGesture } from './edit-mode.js';
import { SIZE_RANGE, sizeOf } from './size-by.js';

const FIT_PADDING = 36;

/** The built-in layouts of this renderer. */
const LAYOUTS = TIER_CAPABILITIES.detailed.layouts;

/** Reads a CSS custom property of the document. */
function cssVar(name) {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

function buildStyle(kind, plan, dark) {
  const text = cssVar('--graph-text') || '#222';
  const outline = cssVar('--graph-text-outline') || '#fff';
  const edge = cssVar('--graph-edge') || '#999';
  const selected = cssVar('--graph-selected') || '#e0791f';
  const muted = cssVar('--text-muted') || '#666';
  const schema = kind === 'schema';
  const outlineWidth = plan.outline ? 2 : 0;
  const slots = [];
  for (let s = 0; s <= PALETTE_SIZE; s++) slots.push({ selector: `node.s${s}`, style: { 'background-color': slotColor(s, dark) } });
  return [
    {
      selector: 'node',
      style: {
        'background-color': slotColor(PALETTE_SIZE, dark),
        label: 'data(caption)',
        color: text,
        'font-size': schema ? 12 : 10,
        'font-weight': schema ? 600 : 400,
        'text-outline-color': outline,
        'text-outline-width': outlineWidth,
        'text-valign': schema ? 'center' : 'bottom',
        'text-margin-y': schema ? 0 : 3,
        'text-wrap': schema ? 'wrap' : 'none',
        'min-zoomed-font-size': schema ? 0 : plan.minZoomedFontSize,
        width: schema ? 'data(w)' : 24,
        height: schema ? 'data(h)' : 24,
        shape: schema ? 'round-rectangle' : 'ellipse',
        'background-opacity': schema ? 0.9 : 1,
        'border-width': 0,
      },
    },
    ...slots,
    // Vertex size by degree (`options.nodeScale`, js/graph/size-by.js): a node with a `size`.
    ...(schema ? [] : [{ selector: 'node[size]', style: { width: 'data(size)', height: 'data(size)' } }]),
    {
      selector: 'edge',
      style: {
        width: plan.edgeWidth,
        'curve-style': plan.curve,
        'line-color': edge,
        'target-arrow-color': edge,
        'target-arrow-shape': plan.arrows ? 'triangle' : 'none',
        'arrow-scale': 0.9,
        label: 'data(caption)',
        'font-size': schema ? 11 : 9,
        'min-zoomed-font-size': schema ? 0 : plan.minZoomedFontSize,
        color: muted,
        'text-rotation': 'autorotate',
        'text-outline-color': outline,
        'text-outline-width': outlineWidth,
        'loop-direction': '-45deg',
        'loop-sweep': '-60deg',
      },
    },
    { selector: 'edge.no-label', style: { label: '' } },
    { selector: '.faded', style: { opacity: 0.22 } },
    // A label the Graph panel's filter hides, in the label overview (a schema drawing).
    { selector: '.filtered-out', style: { opacity: 0.25 } },
    { selector: 'node.hl', style: { 'border-width': 3, 'border-color': selected } },
    { selector: 'edge.hl', style: { 'line-color': selected, 'target-arrow-color': selected, width: 2.6 } },
    { selector: 'node:selected', style: { 'border-width': 3, 'border-color': selected, 'overlay-opacity': 0 } },
    { selector: 'edge:selected', style: { 'line-color': selected, 'target-arrow-color': selected, width: 3, 'overlay-opacity': 0 } },
    { selector: 'node.connect-source, node.connect-target', style: { 'border-width': 3, 'border-color': selected, 'border-style': 'dashed', 'overlay-opacity': 0 } },
  ];
}

/** Cytoscape element definitions of the schema view's `{nodes, edges}`. */
function schemaToCy(els, slotOf, positions) {
  const nodes = els.nodes.map((n, index) => {
    const caption = `${n.label}\n${n.count === undefined ? '' : n.count.toLocaleString('en-US')}`;
    const longest = Math.max(...caption.split('\n').map((l) => l.length));
    const scale = n.scale || 1; // the meta-graph sizes a label by its vertex count
    const def = {
      group: 'nodes',
      classes: 's' + slotOf(n.label) + (n.faded ? ' filtered-out' : ''),
      data: { id: 'n' + n.id, index, caption, w: Math.max(70 * scale, longest * 8 + 24), h: 46 * scale },
    };
    if (positions) def.position = { x: positions[2 * index], y: positions[2 * index + 1] };
    return def;
  });
  const ids = new Set(els.nodes.map((n) => n.id));
  const edges = [];
  els.edges.forEach((e, index) => {
    if (!ids.has(e.source) || !ids.has(e.target)) return;
    edges.push({
      group: 'edges',
      classes: e.faded ? 'filtered-out' : '',
      data: {
        id: 'e' + e.id,
        index,
        source: 'n' + e.source,
        target: 'n' + e.target,
        caption: e.count !== undefined ? `${e.label} (${e.count.toLocaleString('en-US')})` : e.label,
      },
    });
  });
  return [...nodes, ...edges];
}

export class CytoscapeRenderer {
  /**
   * @param {HTMLElement} container
   * @param {{dark?:boolean}} [options]
   */
  constructor(container, options = {}) {
    if (typeof window.cytoscape !== 'function') throw new Error('Cytoscape.js did not load (offline?)');
    this.capabilities = TIER_CAPABILITIES.detailed;
    this.container = container;
    this.dark = !!options.dark;
    this.kind = 'data';
    this.data = null;
    this.options = {};
    this.added = false; // the data's elements are in the graph
    this.plan = drawPlan(0, 0);
    this.handlers = { click: null, dblclick: null, move: null, connect: null };
    this.editMode = false;
    this.gesture = new ConnectGesture();
    this.band = null; // the rubber band's SVG, while a connect drag runs
    // Our wheel zoom runs before Cytoscape's (a capturing listener registered first on the same
    // element) and stops it: js/graph/wheel-zoom.js says why.
    this.wheelMemo = {};
    this.wheelTimer = null;
    this.onWheel = (e) => this.wheelZoom(e);
    container.addEventListener('wheel', this.onWheel, { capture: true, passive: false });
    this.cy = window.cytoscape({
      container,
      style: buildStyle(this.kind, this.plan, this.dark),
      minZoom: 0.02,
      maxZoom: 4,
      boxSelectionEnabled: false,
    });
    const target = (evt) => ({ group: evt.target.isNode() ? 'node' : 'edge', index: evt.target.data('index') });
    this.cy.on('tap', 'node, edge', (evt) => { if (this.handlers.click) this.handlers.click(target(evt)); });
    this.cy.on('tap', (evt) => { if (evt.target === this.cy && this.handlers.click) this.handlers.click(null); });
    this.cy.on('dbltap', (evt) => {
      if (!this.handlers.dblclick) return;
      const point = evt.position ? { x: evt.position.x, y: evt.position.y } : null;
      this.handlers.dblclick(evt.target === this.cy ? null : target(evt), point);
    });
    this.cy.on('tapstart', 'node', (evt) => this.connectStart(evt));
    this.cy.on('tapdrag', (evt) => this.connectMove(evt));
    this.cy.on('tapend', (evt) => this.connectEnd(evt));
    this.cy.on('scrollzoom pinchzoom dragpan', () => { if (this.handlers.move) this.handlers.move(); });
  }

  /** One wheel event: zoom around the pointer by `wheelZoomRatio`, Cytoscape's handler skipped. */
  wheelZoom(e) {
    const cy = this.cy;
    if (!cy || !cy.zoomingEnabled() || !cy.userZoomingEnabled()) return;
    e.preventDefault();
    e.stopImmediatePropagation();
    const ratio = wheelZoomRatio(e, this.wheelMemo, performance.now());
    if (ratio === null) return;
    const rect = this.container.getBoundingClientRect();
    // While the view moves, the renderer draws the cheap way (a texture, edges hidden on large
    // drawings), as during its own wheel zoom; the full drawing comes back 150 ms after the last.
    const r = cy.renderer();
    if (r && r.data) {
      r.data.wheelZooming = true;
      clearTimeout(this.wheelTimer);
      this.wheelTimer = setTimeout(() => {
        if (!this.cy) return;
        r.data.wheelZooming = false;
        r.redrawHint('eles', true);
        r.redraw();
      }, 150);
    }
    cy.zoom({ level: cy.zoom() * ratio, renderedPosition: { x: e.clientX - rect.left, y: e.clientY - rect.top } });
    cy.emit('scrollzoom');
  }

  isHidden() {
    return !this.container || !this.container.clientWidth || !this.container.clientHeight;
  }

  counts() {
    const d = this.data;
    if (!d) return [0, 0];
    return this.kind === 'schema' ? [d.nodes.length, d.edges.length] : [d.vertexIds.length, d.edgeIds.length];
  }

  /** The size-dependent plan: style and the renderer's viewport options. */
  applyPlan() {
    const [nodes, edges] = this.counts();
    this.plan = drawPlan(nodes, edges);
    this.cy.style(buildStyle(this.kind, this.plan, this.dark));
    // Read on every frame; Cytoscape takes them only at init, so they are set on its (canvas)
    // renderer directly.
    const r = this.cy.renderer();
    if (r) {
      r.hideEdgesOnViewport = this.plan.hideEdgesOnViewport;
      r.textureOnViewport = this.plan.textureOnViewport;
    }
  }

  setData(data, options = {}) {
    this.data = data;
    this.kind = options.kind || 'data';
    this.options = {
      showIds: !!options.showIds, edgeIds: !!options.edgeIds, edgeLabels: options.edgeLabels !== false, nodeScale: options.nodeScale || null,
    };
    this.palette = options.palette;
    this.added = false;
    this.cy.batch(() => {
      this.cy.elements().remove();
      this.applyPlan();
    });
  }

  /** Adds the data's elements (at `positions`, when given). */
  addElements(positions) {
    const data = this.data;
    const slotOf = (label) => (this.palette ? this.palette.slot(label) : PALETTE_SIZE);
    this.cy.batch(() => {
      this.cy.elements().remove();
      if (data) {
        const defs = this.kind === 'schema'
          ? schemaToCy(data, slotOf, positions)
          : toCyElements(data, { showIds: this.options.showIds, edgeIds: this.options.edgeIds, slotOf, positions });
        this.cy.add(defs);
        this.cy.edges().toggleClass('no-label', !this.options.edgeLabels);
        this.applySizes();
      }
    });
    this.added = true;
    if (this.selection) this.select(this.selection);
  }

  setPositions(xy) {
    if (!this.added) {
      this.addElements(xy && xy.length ? xy : null);
      return;
    }
    this.cy.batch(() => {
      this.cy.nodes().forEach((node) => {
        const i = node.data('index');
        if (2 * i + 1 < xy.length) node.position({ x: xy[2 * i], y: xy[2 * i + 1] });
      });
    });
  }

  getPositions() {
    if (!this.added) return null;
    const nodes = this.cy.nodes();
    const out = new Float32Array(2 * nodes.length);
    nodes.forEach((node) => {
      const i = node.data('index');
      const p = node.position();
      out[2 * i] = p.x;
      out[2 * i + 1] = p.y;
    });
    return out;
  }

  setLayout(name, options = {}) {
    if (!this.added) this.addElements(null);
    if (!this.cy.elements().length) return Promise.resolve();
    this.cy.resize();
    const schema = this.kind === 'schema';
    const opts = { name: LAYOUTS.includes(name) ? name : 'grid', animate: false, fit: false, padding: FIT_PADDING };
    if (opts.name === 'cose') Object.assign(opts, { nodeRepulsion: () => 9000, idealEdgeLength: () => (schema ? 140 : 70), numIter: options.iterations || 1000, randomize: true });
    if (opts.name === 'breadthfirst') Object.assign(opts, { directed: true, spacingFactor: 1.1 });
    if (opts.name === 'concentric') Object.assign(opts, { minNodeSpacing: 20 });
    if (opts.name === 'grid') Object.assign(opts, { avoidOverlapPadding: 30 });
    return new Promise((resolve) => {
      const layout = this.cy.layout(opts);
      layout.one('layoutstop', () => resolve());
      layout.run();
    });
  }

  /**
   * Vertex sizes from `options.nodeScale` (a scale in [0, 1] per vertex, js/graph/size-by.js):
   * `SIZE_RANGE.detailed`; without scales (or scales of another drawing) the fixed size.
   */
  applySizes() {
    if (this.kind === 'schema' || !this.data) return;
    const scale = this.options.nodeScale;
    const ok = scale && scale.length === this.data.vertexIds.length;
    const { min, max } = SIZE_RANGE.detailed;
    this.cy.nodes().forEach((n) => {
      if (ok) n.data('size', Math.round(sizeOf(scale[n.data('index')], min, max) * 10) / 10);
      else if (n.data('size') !== undefined) n.removeData('size');
    });
  }

  setOptions(options) {
    Object.assign(this.options, options);
    if (!this.added || !this.data) return;
    if ('nodeScale' in options) this.cy.batch(() => this.applySizes());
    if ('edgeLabels' in options) this.cy.edges().toggleClass('no-label', !this.options.edgeLabels);
    if (this.kind === 'schema' || !('showIds' in options || 'edgeIds' in options)) return;
    const view = this.data;
    this.cy.batch(() => {
      this.cy.nodes().forEach((n) => { n.data('caption', vertexCaption(view, n.data('index'), this.options.showIds)); });
      this.cy.edges().forEach((e) => { e.data('caption', edgeCaption(view, e.data('index'), this.options.edgeIds)); });
    });
  }

  /**
   * Fits the drawing with padding; the zoom is capped so a handful of nodes is not blown up.
   * Skipped while the canvas has no size (the panel fits once it has one).
   */
  fit() {
    if (this.isHidden()) return;
    this.cy.resize();
    if (!this.cy.elements().length) return;
    this.cy.fit(undefined, FIT_PADDING);
    const cap = this.kind === 'schema' ? 1.4 : 1.6;
    if (this.cy.zoom() > cap) {
      this.cy.zoom(cap);
      this.cy.center();
    }
  }

  resize() {
    this.cy.resize();
  }

  select(ids) {
    this.selection = ids;
    if (!this.added) return;
    this.cy.batch(() => {
      this.cy.elements().removeClass('faded hl');
      if (!ids || (!ids.nodes.length && !ids.edges.length)) return;
      const keep = this.cy.collection();
      ids.nodes.forEach((id) => { keep.merge(this.cy.getElementById('n' + id)); });
      ids.edges.forEach((id) => { keep.merge(this.cy.getElementById('e' + id)); });
      this.cy.elements().not(keep).addClass('faded');
      keep.addClass('hl');
    });
  }

  onClick(handler) { this.handlers.click = handler; }

  onDoubleClick(handler) { this.handlers.dblclick = handler; }

  onUserMove(handler) { this.handlers.move = handler; }

  onConnect(handler) { this.handlers.connect = handler; }

  setEditMode(on) {
    this.editMode = !!on;
    if (!this.cy) return;
    this.cy.autoungrabify(this.editMode);
    this.container.classList.toggle('graph-editing', this.editMode);
    if (!this.editMode) this.connectAbort();
  }

  /** The vertex under the drawing position `p` (the topmost), or null. */
  nodeAt(p) {
    if (!p) return null;
    let hit = null;
    this.cy.nodes().forEach((n) => {
      const bb = n.boundingBox({ includeLabels: false, includeOverlays: false });
      if (p.x >= bb.x1 && p.x <= bb.x2 && p.y >= bb.y1 && p.y <= bb.y2) hit = n;
    });
    return hit;
  }

  connectStart(evt) {
    if (!this.editMode || !this.handlers.connect || this.kind !== 'data') return;
    const node = evt.target;
    this.gesture.start(node.data('index'), evt.renderedPosition || { x: 0, y: 0 });
    this.cy.userPanningEnabled(false); // this drag draws a rubber band, it does not pan
  }

  connectMove(evt) {
    if (!this.gesture.active) return;
    const over = this.nodeAt(evt.position);
    const band = this.gesture.move(evt.renderedPosition || { x: 0, y: 0 }, over ? over.data('index') : null);
    if (!band) return;
    const source = this.cy.nodes().filter((n) => n.data('index') === band.from);
    const from = source.length ? source[0].renderedPosition() : band.to;
    this.drawBand(from, band.to);
    this.cy.nodes('.connect-target').removeClass('connect-target');
    source.addClass('connect-source');
    if (band.target !== null && over) over.addClass('connect-target');
  }

  connectEnd(evt) {
    if (!this.gesture.active) return;
    const over = this.nodeAt(evt.position);
    const result = this.gesture.end(over ? over.data('index') : null);
    this.connectAbort();
    if (result.type === 'connect' && this.handlers.connect) {
      this.handlers.connect({ group: 'node', index: result.from }, { group: 'node', index: result.to });
    }
  }

  /** Ends a connect drag without connecting: removes the band, pans again. */
  connectAbort() {
    this.gesture.reset();
    if (this.band) {
      this.band.remove();
      this.band = null;
    }
    if (this.cy) {
      this.cy.userPanningEnabled(true);
      this.cy.nodes('.connect-source, .connect-target').removeClass('connect-source connect-target');
    }
  }

  /** The rubber band from `a` to `b` (rendered pixels of the container). */
  drawBand(a, b) {
    if (!this.band) {
      const ns = 'http://www.w3.org/2000/svg';
      const svg = document.createElementNS(ns, 'svg');
      svg.setAttribute('class', 'graph-band');
      svg.setAttribute('aria-hidden', 'true');
      const line = document.createElementNS(ns, 'line');
      svg.appendChild(line);
      this.container.appendChild(svg);
      this.band = svg;
    }
    const line = this.band.firstChild;
    line.setAttribute('x1', a.x);
    line.setAttribute('y1', a.y);
    line.setAttribute('x2', b.x);
    line.setAttribute('y2', b.y);
  }

  /**
   * The drawing as a PNG Blob at 2x: the whole drawing (`full`) or the visible area, on the
   * theme's background or transparent. Labels and id captions are exported as shown.
   */
  async exportPng({ full = true, transparent = false } = {}) {
    if (!this.cy.elements().length) throw new Error('there is nothing to export');
    const blob = await this.cy.png({
      output: 'blob-promise',
      full,
      scale: 2,
      maxWidth: 8192,
      maxHeight: 8192,
      bg: transparent ? undefined : (cssVar('--surface') || '#ffffff'),
    });
    if (!blob || !blob.size) throw new Error('the browser returned an empty image (drawing too large?)');
    return blob;
  }

  setTheme(dark) {
    this.dark = !!dark;
    this.cy.style(buildStyle(this.kind, this.plan, this.dark));
  }

  destroy() {
    this.container.removeEventListener('wheel', this.onWheel, { capture: true });
    clearTimeout(this.wheelTimer);
    this.connectAbort();
    this.container.classList.remove('graph-editing');
    this.cy.destroy();
    this.cy = null;
  }
}
