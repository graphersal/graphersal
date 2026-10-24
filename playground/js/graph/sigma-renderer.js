// The FAST renderer: sigma.js 3 + graphology on WebGL behind the renderer interface of
// js/graph/renderer.js (capabilities: TIER_CAPABILITIES.fast). Vertices coloured from the page's
// label palette, vertex labels by zoom level (sigma's label grid: a label is drawn once its vertex
// is large enough on screen), hover highlights the vertex's neighbours and shows the labels of its
// edges, click = inspector, double click = expand the selection (graph-canvas.js), straight edges
// with arrow heads, positions from the engine's force layout (or the cheap Circle / Grid here).
// No edge labels otherwise, no curves or loops, no Tree/Concentric, no schema drawing.
//
// Loading: the scripts come from cdnjs as their UMD builds with Subresource Integrity, injected
// on first use (`loadFastLibraries`), so a page that never draws a large graph loads nothing.
// (Their ES module builds import the CommonJS `events` package by bare name, which a browser
// cannot resolve without a bundler; jsDelivr's converted `+esm` files would work but cannot be
// pinned with SRI.)
//
// Elements are added lazily like in the Detailed renderer: `setData` keeps the data, the first
// `setPositions` / `setLayout` builds the graphology graph AT its positions and hands it to sigma
// in one piece (`setGraph`, one indexation, one paint).

import { vertexCaption, edgeCaption, vertexCount, edgeCount, adjacencyOf, circlePositions, gridPositions } from '../graph-data.js';
import { PALETTE_SIZE, slotColor } from './palette.js';
import { TIER_CAPABILITIES } from './renderer.js';
import { detectWebGL } from './tiers.js';
import { SIZE_RANGE, sizeOf } from './size-by.js';

/** The pinned scripts (cdnjs, exact versions, SRI hashes from cdnjs). */
export const FAST_LIBRARIES = Object.freeze([
  Object.freeze({
    global: 'graphology',
    src: 'https://cdnjs.cloudflare.com/ajax/libs/graphology/0.26.0/graphology.umd.min.js',
    integrity: 'sha512-Hqa5FKQ53pYDWaRnytoNvRT3JXRac7dcH+kB3RUCX69CGNrnz5LE76Mp0z186qDv0LBWrwx5QipEoenZB5CE4w==',
  }),
  Object.freeze({
    global: 'Sigma',
    src: 'https://cdnjs.cloudflare.com/ajax/libs/sigma.js/3.0.3/sigma.min.js',
    integrity: 'sha512-Pl31Mn3QNmO8wHTaB9fdt/0YGpGtHHqcvT8SuwSK4/E1VLgkiPoluigM5BxlC3irGAzfLjSv9/L1m9dyVEl4Bw==',
  }),
]);

/** Edges are clickable (sigma's edge picking, an extra render pass) up to this many edges. */
export const FAST_EDGE_EVENTS_MAX = 20000;

/** sigma's graph y axis points up, the engine's and Cytoscape's down: positions are mirrored. */
const Y_SIGN = -1;

function loadScript({ src, integrity }) {
  return new Promise((resolve, reject) => {
    const el = document.createElement('script');
    el.src = src;
    el.integrity = integrity;
    el.crossOrigin = 'anonymous';
    el.referrerPolicy = 'no-referrer';
    el.onload = () => resolve();
    el.onerror = () => {
      el.remove();
      reject(new Error(`could not load ${src} (offline, or blocked)`));
    };
    document.head.appendChild(el);
  });
}

let loading = null;

/**
 * Loads sigma.js and graphology once (later calls share the promise; a failure may be retried).
 * Rejects without WebGL.
 * @returns {Promise<{Graph:Function, Sigma:Function}>}
 */
export function loadFastLibraries() {
  if (!loading) {
    loading = (async () => {
      if (!detectWebGL()) throw new Error('this browser has no WebGL');
      for (const lib of FAST_LIBRARIES) if (!window[lib.global]) await loadScript(lib);
      const sigma = window.Sigma;
      const Sigma = sigma && (sigma.Sigma || sigma);
      if (!window.graphology || typeof Sigma !== 'function') throw new Error('sigma.js did not load');
      return { Graph: window.graphology.MultiDirectedGraph, Sigma };
    })();
    loading.catch(() => { loading = null; });
  }
  return loading;
}

function cssVar(name, fallback) {
  const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return v || fallback;
}

/** `a` blended over `b` with weight `t` (both #rrggbb): an opaque faded colour. */
function mix(a, b, t) {
  const p = (hex) => {
    const h = hex.replace('#', '');
    const full = h.length === 3 ? h.split('').map((c) => c + c).join('') : h.slice(0, 6);
    const n = parseInt(full, 16);
    return Number.isFinite(n) ? [(n >> 16) & 255, (n >> 8) & 255, n & 255] : [128, 128, 128];
  };
  const x = p(a);
  const y = p(b);
  return '#' + x.map((c, k) => Math.round(c * t + y[k] * (1 - t)).toString(16).padStart(2, '0')).join('');
}

/** The vertex radius (px at zoom 1) for a drawing of `n` vertices. */
function nodeSizeFor(n) {
  if (n <= 500) return 7;
  if (n <= 5000) return 5;
  if (n <= 20000) return 3.5;
  return 2.5;
}

export class SigmaRenderer {
  /**
   * @param {HTMLElement} container
   * @param {{dark?:boolean, libs:{Graph:Function, Sigma:Function}}} options
   */
  constructor(container, options = {}) {
    if (!options.libs) throw new Error('sigma.js is not loaded');
    this.capabilities = TIER_CAPABILITIES.fast;
    this.libs = options.libs;
    this.container = container;
    this.dark = !!options.dark;
    this.data = null;
    this.options = {};
    this.added = false;
    this.handlers = { click: null, dblclick: null, move: null };
    this.selNodes = null; // Uint8Array mask of highlighted vertices (others faded), or null
    this.selEdges = null;
    this.selection = null;
    this.hoverNodes = new Set();
    this.hoverEdges = new Set();
    this.readTheme();
    this.graph = new this.libs.Graph({ allowSelfLoops: true });
    this.sigma = new this.libs.Sigma(this.graph, container, {
      allowInvalidContainer: true, // a hidden panel has no size yet
      defaultEdgeType: 'arrow',
      renderEdgeLabels: true, // only hovered edges get a label (edgeReducer)
      enableEdgeEvents: false,
      labelFont: this.theme.font,
      labelSize: 11,
      labelColor: { color: this.theme.text },
      edgeLabelColor: { color: this.theme.text },
      edgeLabelFont: this.theme.font,
      edgeLabelSize: 10,
      labelRenderedSizeThreshold: 6,
      labelDensity: 0.7,
      minCameraRatio: 0.005,
      maxCameraRatio: 4,
      stagePadding: 24,
      defaultDrawNodeLabel: (ctx, data, settings) => this.drawLabel(ctx, data, settings),
      defaultDrawNodeHover: (ctx, data, settings) => this.drawHover(ctx, data, settings),
      nodeReducer: (key, attrs) => this.reduceNode(attrs),
      edgeReducer: (key, attrs) => this.reduceEdge(attrs),
    });
    const s = this.sigma;
    s.on('clickNode', ({ node }) => this.emit('click', { group: 'node', index: Number(node) }));
    s.on('clickEdge', ({ edge }) => this.emit('click', { group: 'edge', index: Number(edge) }));
    s.on('clickStage', () => this.emit('click', null));
    s.on('doubleClickNode', (e) => {
      e.preventSigmaDefault(); // no zoom: double click expands the selection
      this.emit('dblclick', { group: 'node', index: Number(e.node) });
    });
    s.on('doubleClickEdge', (e) => {
      e.preventSigmaDefault();
      this.emit('dblclick', { group: 'edge', index: Number(e.edge) });
    });
    s.on('doubleClickStage', (e) => {
      e.preventSigmaDefault(); // the panel fits instead
      this.emit('dblclick', null);
    });
    s.on('enterNode', ({ node }) => {
      container.style.cursor = 'pointer';
      this.hover(Number(node));
    });
    s.on('leaveNode', () => {
      container.style.cursor = '';
      this.hover(null);
    });
    const moved = () => { if (this.handlers.move) this.handlers.move(); };
    s.getMouseCaptor().on('wheel', moved);
    s.getMouseCaptor().on('mousemovebody', (e) => { if (e.original && e.original.buttons & 1) moved(); });
    s.getTouchCaptor().on('touchmove', moved);
  }

  emit(name, target) {
    if (this.handlers[name]) this.handlers[name](target);
  }

  readTheme() {
    this.theme = {
      text: cssVar('--graph-text', '#1c2330'),
      edge: cssVar('--graph-edge', '#9aa3b2'),
      selected: cssVar('--graph-selected', '#e0791f'),
      surface: cssVar('--surface', this.dark ? '#181d25' : '#ffffff'),
      font: cssVar('--font-ui', 'system-ui, sans-serif'),
    };
    this.slotColors = [];
    this.slotFaded = [];
    for (let s = 0; s <= PALETTE_SIZE; s++) {
      const c = slotColor(s, this.dark);
      this.slotColors.push(c);
      this.slotFaded.push(mix(c, this.theme.surface, 0.22));
    }
    this.edgeFaded = mix(this.theme.edge, this.theme.surface, 0.3);
  }

  /** A vertex label: the caption next to the vertex, outlined in the surface colour. */
  drawLabel(ctx, data, settings) {
    if (!data.label) return;
    const size = settings.labelSize;
    ctx.font = `${size}px ${settings.labelFont}`;
    ctx.lineWidth = 3;
    ctx.strokeStyle = this.theme.surface;
    ctx.fillStyle = this.theme.text;
    const x = data.x + data.size + 3;
    const y = data.y + size / 3;
    ctx.strokeText(data.label, x, y);
    ctx.fillText(data.label, x, y);
  }

  /** A hovered or highlighted vertex: a ring in the selection colour and its label on the surface. */
  drawHover(ctx, data, settings) {
    ctx.beginPath();
    ctx.arc(data.x, data.y, data.size + 2.5, 0, Math.PI * 2);
    ctx.lineWidth = 2;
    ctx.strokeStyle = this.theme.selected;
    ctx.stroke();
    if (!data.label) return;
    const size = settings.labelSize;
    ctx.font = `600 ${size}px ${settings.labelFont}`;
    const w = ctx.measureText(data.label).width;
    const x = data.x + data.size + 4;
    const y = data.y + size / 3;
    ctx.fillStyle = this.theme.surface;
    ctx.fillRect(x - 2, y - size + 1, w + 4, size + 4);
    ctx.fillStyle = this.theme.text;
    ctx.fillText(data.label, x, y);
  }

  reduceNode(attrs) {
    const i = attrs.i;
    if (this.selNodes && !this.selNodes[i]) return { ...attrs, color: this.slotFaded[attrs.slot], label: null };
    if (this.hoverNodes.has(i)) return { ...attrs, highlighted: true };
    return attrs;
  }

  reduceEdge(attrs) {
    const j = attrs.j;
    if (this.hoverEdges.has(j)) {
      return { ...attrs, color: this.theme.selected, label: attrs.caption, forceLabel: true, size: 1.6 };
    }
    if (this.selEdges && !this.selEdges[j]) return { ...attrs, color: this.edgeFaded, label: null };
    if (this.selEdges) return { ...attrs, color: this.theme.selected, label: null };
    return { ...attrs, label: null };
  }

  /** Hover: the vertex's neighbours highlighted, its edges coloured with their labels. */
  hover(index) {
    const before = { nodes: [...this.hoverNodes], edges: [...this.hoverEdges] };
    this.hoverNodes = new Set();
    this.hoverEdges = new Set();
    if (index !== null && this.data) {
      const { offsets, neighbours, edges } = adjacencyOf(this.data);
      this.hoverNodes.add(index);
      for (let k = offsets[index]; k < offsets[index + 1]; k++) {
        this.hoverNodes.add(neighbours[k]);
        this.hoverEdges.add(edges[k]);
      }
    }
    const nodes = new Set([...before.nodes, ...this.hoverNodes]);
    const edges = new Set([...before.edges, ...this.hoverEdges]);
    if (!nodes.size && !edges.size) return;
    try {
      // Repaints only these elements (cheap at any size).
      this.sigma.refresh({
        partialGraph: { nodes: [...nodes].map(String), edges: [...edges].map(String) },
        skipIndexation: true,
      });
    } catch (err) {
      // A full refresh is pending (its indices are not built yet): it repaints everything anyway.
      this.sigma.scheduleRefresh();
    }
  }

  counts() {
    return [vertexCount(this.data), edgeCount(this.data)];
  }

  setData(data, options = {}) {
    if (options.kind === 'schema') throw new Error('the Fast tier does not draw the schema graph');
    this.data = data;
    this.options = { showIds: !!options.showIds, edgeIds: !!options.edgeIds, nodeScale: options.nodeScale || null };
    this.palette = options.palette;
    this.added = false;
    this.hoverNodes = new Set();
    this.hoverEdges = new Set();
    this.idIndex = null;
    this.selNodes = null;
    this.selEdges = null;
    this.graph = new this.libs.Graph({ allowSelfLoops: true });
    this.sigma.setGraph(this.graph);
  }

  /** Builds the graphology graph of the data at `xy` and hands it to sigma in one piece. */
  addElements(xy) {
    const view = this.data;
    const graph = new this.libs.Graph({ allowSelfLoops: true });
    if (view) {
      const [n, m] = this.counts();
      const sizeAt = this.sizer(n);
      const slots = view.labels.map((l) => (this.palette ? this.palette.slot(l) : PALETTE_SIZE));
      for (let i = 0; i < n; i++) {
        const slot = slots[view.vertexLabel[i]];
        graph.addNode(String(i), {
          i,
          slot,
          x: xy && 2 * i + 1 < xy.length ? xy[2 * i] : 0,
          y: xy && 2 * i + 1 < xy.length ? Y_SIGN * xy[2 * i + 1] : 0,
          size: sizeAt(i),
          color: this.slotColors[slot],
          label: vertexCaption(view, i, this.options.showIds),
        });
      }
      for (let j = 0; j < m; j++) {
        graph.addDirectedEdgeWithKey(String(j), String(view.edgeSource[j]), String(view.edgeTarget[j]), {
          j,
          size: 1,
          color: this.theme.edge,
          caption: edgeCaption(view, j, false),
        });
      }
      this.sigma.setSetting('enableEdgeEvents', m <= FAST_EDGE_EVENTS_MAX);
    }
    this.graph = graph;
    this.sigma.setGraph(graph);
    this.added = true;
    if (this.selection) this.select(this.selection);
  }

  setPositions(xy) {
    if (!this.added) {
      this.addElements(xy && xy.length ? xy : null);
      return;
    }
    this.graph.updateEachNodeAttributes((key, attrs) => {
      const i = attrs.i;
      if (2 * i + 1 < xy.length) {
        attrs.x = xy[2 * i];
        attrs.y = Y_SIGN * xy[2 * i + 1];
      }
      return attrs;
    }, { attributes: ['x', 'y'] });
  }

  getPositions() {
    if (!this.added) return null;
    const out = new Float32Array(2 * this.graph.order);
    this.graph.forEachNode((key, attrs) => {
      out[2 * attrs.i] = attrs.x;
      out[2 * attrs.i + 1] = Y_SIGN * attrs.y;
    });
    return out;
  }

  /** The cheap built-in layouts (`capabilities.layouts`): Circle and Grid, computed here. */
  async setLayout(name) {
    const n = vertexCount(this.data);
    const xy = name === 'circle' ? circlePositions(n) : gridPositions(n);
    this.setPositions(xy);
  }

  /**
   * The size of vertex `i` of a drawing of `n` vertices: `nodeSizeFor(n)`, or with
   * `options.nodeScale` (js/graph/size-by.js) between `SIZE_RANGE.fast` factors of it.
   */
  sizer(n) {
    const base = nodeSizeFor(n);
    const scale = this.options.nodeScale;
    if (!scale || scale.length !== n) return () => base;
    const min = base * SIZE_RANGE.fast.minFactor;
    const max = base * SIZE_RANGE.fast.maxFactor;
    return (i) => sizeOf(scale[i], min, max);
  }

  setOptions(options) {
    Object.assign(this.options, options);
    if (this.added && this.data && 'nodeScale' in options) {
      const sizeAt = this.sizer(vertexCount(this.data));
      this.graph.updateEachNodeAttributes((key, attrs) => {
        attrs.size = sizeAt(attrs.i);
        return attrs;
      }, { attributes: ['size'] });
    }
    if (!this.added || !this.data || !('showIds' in options)) return;
    const view = this.data;
    this.graph.updateEachNodeAttributes((key, attrs) => {
      attrs.label = vertexCaption(view, attrs.i, this.options.showIds);
      return attrs;
    }, { attributes: ['label'] });
  }

  isHidden() {
    return !this.container || !this.container.clientWidth || !this.container.clientHeight;
  }

  /** Fits the whole drawing (sigma scales the graph's extent to the view at camera ratio 1). */
  fit() {
    if (this.isHidden()) return;
    this.sigma.resize();
    this.sigma.getCamera().setState({ x: 0.5, y: 0.5, ratio: 1, angle: 0 });
  }

  resize() {
    this.sigma.resize();
    this.sigma.refresh();
  }

  select(ids) {
    this.selection = ids;
    if (!this.added) return;
    if (!ids || (!ids.nodes.length && !ids.edges.length)) {
      this.selNodes = null;
      this.selEdges = null;
    } else {
      const view = this.data;
      if (!this.idIndex) {
        const nodes = new Map();
        view.vertexIds.forEach((id, i) => nodes.set(String(id), i));
        const edges = new Map();
        view.edgeIds.forEach((id, j) => edges.set(String(id), j));
        this.idIndex = { nodes, edges };
      }
      this.selNodes = new Uint8Array(vertexCount(view));
      this.selEdges = new Uint8Array(edgeCount(view));
      for (const id of ids.nodes) {
        const i = this.idIndex.nodes.get(String(id));
        if (i !== undefined) this.selNodes[i] = 1;
      }
      for (const id of ids.edges) {
        const j = this.idIndex.edges.get(String(id));
        if (j !== undefined) this.selEdges[j] = 1;
      }
    }
    this.sigma.refresh();
  }

  onClick(handler) { this.handlers.click = handler; }

  onDoubleClick(handler) { this.handlers.dblclick = handler; }

  onUserMove(handler) { this.handlers.move = handler; }

  /**
   * Edit mode changes nothing here: this tier adds no elements with the mouse
   * (`capabilities.addElements` is false); properties are edited and elements deleted through
   * the inspector, which a click opens in every mode.
   */
  setEditMode(on) { this.editMode = !!on; }

  /** Never called: no connect gesture in this tier. */
  onConnect(handler) { this.handlers.connect = handler; }

  /**
   * The drawing as a PNG Blob, at the canvas' size and the screen's pixel ratio: the whole drawing
   * (`full`: the camera is reset for the capture and put back) or the visible area, on the theme's
   * background or transparent. sigma renders synchronously, so its WebGL layers are read in the
   * same task, before the browser clears them.
   */
  async exportPng({ full = true, transparent = false } = {}) {
    if (!this.added || !this.graph.order) throw new Error('there is nothing to export');
    if (this.isHidden()) throw new Error('the drawing is not visible');
    const camera = this.sigma.getCamera();
    const saved = camera.getState();
    if (full) camera.setState({ x: 0.5, y: 0.5, ratio: 1, angle: 0 });
    this.sigma.refresh();
    const layers = Object.entries(this.sigma.getCanvases()).filter(([id]) => id !== 'mouse').map(([, c]) => c);
    const width = layers[0].width;
    const height = layers[0].height;
    const out = document.createElement('canvas');
    out.width = width;
    out.height = height;
    const ctx = out.getContext('2d');
    if (!transparent) {
      ctx.fillStyle = this.theme.surface;
      ctx.fillRect(0, 0, width, height);
    }
    for (const layer of layers) ctx.drawImage(layer, 0, 0, width, height);
    if (full) {
      camera.setState(saved);
      this.sigma.refresh();
    }
    const blob = await new Promise((resolve) => out.toBlob(resolve, 'image/png'));
    if (!blob || !blob.size) throw new Error('the browser returned an empty image');
    return blob;
  }

  setTheme(dark) {
    this.dark = !!dark;
    this.readTheme();
    if (this.added) {
      this.graph.updateEachNodeAttributes((key, attrs) => {
        attrs.color = this.slotColors[attrs.slot];
        return attrs;
      }, { attributes: ['color'] });
      this.graph.updateEachEdgeAttributes((key, attrs) => {
        attrs.color = this.theme.edge;
        return attrs;
      }, { attributes: ['color'] });
    }
    // Last: a setting change schedules a full refresh, which drops sigma's program indices at
    // once, and the attribute updates above repaint through those indices.
    this.sigma.setSettings({ labelColor: { color: this.theme.text }, edgeLabelColor: { color: this.theme.text } });
  }

  destroy() {
    this.container.style.cursor = '';
    this.sigma.kill();
    this.sigma = null;
    this.graph = null;
  }
}
