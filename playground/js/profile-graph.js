// The graph view of a UI profile (EXPERIMENTAL, like the profile API): the pure model behind
// `api.graph(data, options)` (js/profile-api.js) and components/profile-graph.js. No DOM, no Vue;
// tested in tests/profile-graph.test.mjs.
//
// A profile module hands the view its own data, typically built from saved queries:
//
//   {nodes: [{id, label, caption?, data?, classes?}], edges: [{id, source, target, label, data?, classes?}]}
//
// `id` a non-empty text or a number (kept as text; unique per kind), `label` a text (the style
// and the card template are chosen by it), `caption` the text under the vertex (else the style's
// `caption` field of `data`, else the label), `data` the element's fields (what its card shows,
// what `select`/`expand` hand back), `classes` names of style classes (below). An edge's `source`
// and `target` are ids of nodes of the same data. `checkGraphData` names every problem.
//
// The profile's optional key `graph` (checked by `graphSpecProblems`, called by
// js/ui-profiles.js `validateProfile`) styles the view per label:
//
//   graph: {
//     nodes:   {<label> | "*": {color, shape, size, opacity, border: {width, color, style}, caption}},
//     edges:   {<label> | "*": {color, style, arrow, width, opacity, caption}},
//     classes: {<name>: {any of the node and edge keys}},
//     legend:  [{text, node: {node keys}} | {text, edge: {edge keys}}],
//     legendLabels: true | false | "nodes" | "edges"
//   }
//
// `color` '#rgb' / '#rrggbb' or a CSS colour token of the page ('--accent': follows the theme);
// a node without one takes the label's colour of the page's palette (js/graph/palette.js, the
// Graph panel's colours). `shape` one of NODE_SHAPES; `size` NODE_SIZE px; `opacity` 0.05..1;
// `border.style` and an edge's `style` one of LINE_STYLES; `arrow` true/false (default true);
// `width` EDGE_WIDTH px; `caption` a field path of `data` (js/profile-cards.js paths). "*" applies
// to every label, the label's own entry on top of it. `legendLabels` says which labels drawn the
// legend lists by itself before the `legend` entries (default true: node and edge labels; a
// profile that colours by classes lists its own entries and keeps only "edges", say).
//
// A style is resolved in this order, later wins: the defaults, `"*"`, the label's entry, each
// class of the element (`classes`, then the classes `styleOf` returns; a class is the profile's
// `graph.classes` entry, else a built-in one of BUILTIN_CLASSES: `faded`, `bold`, `dashed`;
// an unknown name changes nothing), then what the module's `styleOf(element, kind)` returns
// besides `classes` (the same keys). So a module fades a superseded memory with
// `classes: ['faded']` on the node, or computes it in `styleOf`.

import { fieldValue, isFieldPath } from './profile-cards.js';

/** The node shapes (the Detailed tier draws them; the Fast tier draws every node as a circle). */
export const NODE_SHAPES = Object.freeze(['ellipse', 'round-rectangle', 'rectangle', 'diamond', 'triangle', 'hexagon', 'star']);

/** Border and edge line styles (the Fast tier draws every line solid). */
export const LINE_STYLES = Object.freeze(['solid', 'dashed', 'dotted']);

/** The keys of a node style. */
export const NODE_STYLE_KEYS = Object.freeze(['color', 'shape', 'size', 'opacity', 'border', 'caption']);

/** The keys of a node style's `border`. */
export const BORDER_KEYS = Object.freeze(['width', 'color', 'style']);

/** The keys of an edge style. */
export const EDGE_STYLE_KEYS = Object.freeze(['color', 'style', 'arrow', 'width', 'opacity', 'caption']);

/** The keys of the profile's `graph`. */
export const GRAPH_SPEC_KEYS = Object.freeze(['nodes', 'edges', 'classes', 'legend', 'legendLabels']);

/** The values of the spec's `legendLabels`: which labels drawn the legend lists by itself. */
export const LEGEND_LABELS = Object.freeze([true, false, 'nodes', 'edges']);

/** The keys of an element of the data. */
export const NODE_KEYS = Object.freeze(['id', 'label', 'caption', 'data', 'classes']);
export const EDGE_KEYS = Object.freeze(['id', 'source', 'target', 'label', 'caption', 'data', 'classes']);

/** Ranges of the number keys. */
export const NODE_SIZE = Object.freeze({ min: 6, max: 96, default: 24 });
export const EDGE_WIDTH = Object.freeze({ min: 0.5, max: 12, default: 1.6 });
export const BORDER_WIDTH = Object.freeze({ min: 0, max: 12 });
export const OPACITY = Object.freeze({ min: 0.05, max: 1 });

/** Limits of the data and the spec. */
export const GRAPH_LIMITS = Object.freeze({ nodes: 20000, edges: 100000, classes: 40, legend: 24, text: 200, problems: 12 });

/**
 * The classes every view knows (a profile's `graph.classes` entry of the same name replaces one):
 * `faded` (a vertex or edge in the background), `bold` (a thick border, a wide edge), `dashed`
 * (a dashed border, a dashed edge).
 */
export const BUILTIN_CLASSES = Object.freeze({
  faded: Object.freeze({ opacity: 0.3 }),
  bold: Object.freeze({ border: Object.freeze({ width: 4 }), width: 3.2 }),
  dashed: Object.freeze({ border: Object.freeze({ width: 2, style: 'dashed' }), style: 'dashed' }),
});

const isPlainObject = (v) => v !== null && typeof v === 'object' && !Array.isArray(v);
const quote = (v) => JSON.stringify(String(v).slice(0, 60));
const COLOR = /^(#[0-9a-fA-F]{3}|#[0-9a-fA-F]{6}|--[a-z][a-z0-9-]*)$/;
const CLASS_NAME = /^[a-zA-Z][a-zA-Z0-9_-]{0,39}$/;

/** Whether `v` is a colour of a style: '#rgb', '#rrggbb' or a CSS token name ('--accent'). */
export function isStyleColor(v) {
  return typeof v === 'string' && COLOR.test(v);
}

const inRange = (v, r) => typeof v === 'number' && Number.isFinite(v) && v >= r.min && v <= r.max;

/**
 * The problems of one style fragment: `kind` 'node', 'edge' or 'class' (the keys of both).
 * @param {*} style
 * @param {'node'|'edge'|'class'} kind
 * @param {string} at  Where it is, for the messages.
 * @returns {string[]}
 */
export function styleProblems(style, kind, at) {
  if (!isPlainObject(style)) return [`"${at}" must be an object`];
  const problems = [];
  const keys = kind === 'node' ? NODE_STYLE_KEYS : kind === 'edge' ? EDGE_STYLE_KEYS : [...new Set([...NODE_STYLE_KEYS, ...EDGE_STYLE_KEYS])];
  for (const [key, v] of Object.entries(style)) {
    if (!keys.includes(key)) {
      problems.push(`"${at}" has the unknown key ${quote(key)} (the keys are ${keys.join(', ')})`);
      continue;
    }
    const where = `"${at}.${key}"`;
    const sub = (k) => `"${at}.${key}.${k}"`;
    switch (key) {
      case 'color':
        if (!isStyleColor(v)) problems.push(`${where} must be "#rgb", "#rrggbb" or a CSS token like "--accent"`);
        break;
      case 'shape':
        if (!NODE_SHAPES.includes(v)) problems.push(`${where} must be one of ${NODE_SHAPES.join(', ')}`);
        break;
      case 'size':
        if (!inRange(v, NODE_SIZE)) problems.push(`${where} must be a number from ${NODE_SIZE.min} to ${NODE_SIZE.max}`);
        break;
      case 'opacity':
        if (!inRange(v, OPACITY)) problems.push(`${where} must be a number from ${OPACITY.min} to ${OPACITY.max}`);
        break;
      case 'width':
        if (!inRange(v, EDGE_WIDTH)) problems.push(`${where} must be a number from ${EDGE_WIDTH.min} to ${EDGE_WIDTH.max}`);
        break;
      case 'style':
        if (!LINE_STYLES.includes(v)) problems.push(`${where} must be one of ${LINE_STYLES.join(', ')}`);
        break;
      case 'arrow':
        if (typeof v !== 'boolean') problems.push(`${where} must be true or false`);
        break;
      case 'caption':
        if (!isFieldPath(v)) problems.push(`${where} must be a field path like "name" or "data.title"`);
        break;
      case 'border':
        if (!isPlainObject(v)) {
          problems.push(`${where} must be an object {width, color, style}`);
          break;
        }
        for (const [bk, bv] of Object.entries(v)) {
          if (!BORDER_KEYS.includes(bk)) problems.push(`${where} has the unknown key ${quote(bk)} (the keys are ${BORDER_KEYS.join(', ')})`);
          else if (bk === 'width' && !inRange(bv, BORDER_WIDTH)) problems.push(`${sub('width')} must be a number from ${BORDER_WIDTH.min} to ${BORDER_WIDTH.max}`);
          else if (bk === 'color' && !isStyleColor(bv)) problems.push(`${sub('color')} must be "#rgb", "#rrggbb" or a CSS token like "--accent"`);
          else if (bk === 'style' && !LINE_STYLES.includes(bv)) problems.push(`${sub('style')} must be one of ${LINE_STYLES.join(', ')}`);
        }
        break;
      default:
        break;
    }
  }
  return problems;
}

/**
 * The problems of a profile's `graph` key (empty: valid). Called by js/ui-profiles.js.
 * @param {*} spec
 * @returns {string[]}
 */
export function graphSpecProblems(spec) {
  if (!isPlainObject(spec)) return ['"graph" must be an object'];
  const problems = [];
  for (const key of Object.keys(spec)) {
    if (!GRAPH_SPEC_KEYS.includes(key)) problems.push(`"graph" has the unknown key ${quote(key)} (the keys are ${GRAPH_SPEC_KEYS.join(', ')})`);
  }
  for (const kind of ['nodes', 'edges']) {
    if (spec[kind] === undefined) continue;
    if (!isPlainObject(spec[kind])) {
      problems.push(`"graph.${kind}" must be an object of styles by label`);
      continue;
    }
    for (const [label, style] of Object.entries(spec[kind])) {
      if (!label || label.length > 100) problems.push(`"graph.${kind}" has a label of ${label.length} characters (1 to 100)`);
      problems.push(...styleProblems(style, kind === 'nodes' ? 'node' : 'edge', `graph.${kind}.${label.slice(0, 60)}`));
    }
  }
  if (spec.classes !== undefined) {
    if (!isPlainObject(spec.classes)) {
      problems.push('"graph.classes" must be an object of styles by class name');
    } else {
      const names = Object.keys(spec.classes);
      if (names.length > GRAPH_LIMITS.classes) problems.push(`"graph.classes" has ${names.length} classes (at most ${GRAPH_LIMITS.classes})`);
      for (const name of names) {
        if (!CLASS_NAME.test(name)) problems.push(`"graph.classes" has the invalid class name ${quote(name)} (a letter, then letters, digits, "_" or "-")`);
        problems.push(...styleProblems(spec.classes[name], 'class', `graph.classes.${name.slice(0, 40)}`));
      }
    }
  }
  if (spec.legend !== undefined) problems.push(...legendProblems(spec.legend, 'graph.legend'));
  if (spec.legendLabels !== undefined && !LEGEND_LABELS.includes(spec.legendLabels)) {
    problems.push('"graph.legendLabels" must be true, false, "nodes" or "edges"');
  }
  return problems;
}

/** The problems of legend entries (`graph.legend`, the `legend` option). */
export function legendProblems(legend, at) {
  if (!Array.isArray(legend)) return [`"${at}" must be a list of {text, node} or {text, edge} entries`];
  const problems = [];
  if (legend.length > GRAPH_LIMITS.legend) problems.push(`"${at}" has ${legend.length} entries (at most ${GRAPH_LIMITS.legend})`);
  legend.forEach((entry, i) => {
    const path = `${at}[${i}]`;
    const where = `"${path}"`;
    if (!isPlainObject(entry)) {
      problems.push(`${where} must be an object {text, node} or {text, edge}`);
      return;
    }
    for (const key of Object.keys(entry)) {
      if (!['text', 'node', 'edge'].includes(key)) problems.push(`${where} has the unknown key ${quote(key)} (the keys are text, node, edge)`);
    }
    if (typeof entry.text !== 'string' || !entry.text.trim() || entry.text.length > GRAPH_LIMITS.text) {
      problems.push(`"${path}.text" must be a text of 1 to ${GRAPH_LIMITS.text} characters`);
    }
    if ((entry.node === undefined) === (entry.edge === undefined)) {
      problems.push(`${where} must have exactly one of "node" and "edge"`);
    } else if (entry.node !== undefined) {
      problems.push(...styleProblems(entry.node, 'node', `${path}.node`));
    } else {
      problems.push(...styleProblems(entry.edge, 'edge', `${path}.edge`));
    }
  });
  return problems;
}

/** Data a graph view cannot draw: `problems` lists the reasons (at most GRAPH_LIMITS.problems). */
export class GraphDataError extends Error {
  constructor(problems) {
    const shown = problems.slice(0, GRAPH_LIMITS.problems);
    const more = problems.length > shown.length ? `; and ${problems.length - shown.length} more` : '';
    super(`api.graph: the graph data is invalid: ${shown.join('; ')}${more}.`);
    this.name = 'GraphDataError';
    this.problems = problems;
  }
}

const isId = (v) => (typeof v === 'string' && v.length > 0 && v.length <= 400) || (typeof v === 'number' && Number.isFinite(v));

function classesProblems(classes, at) {
  if (classes === undefined) return [];
  if (!Array.isArray(classes) || classes.some((c) => typeof c !== 'string' || !CLASS_NAME.test(c))) {
    return [`${at}.classes must be a list of class names (a letter, then letters, digits, "_" or "-")`];
  }
  return [];
}

/**
 * Checks the data of a graph view and returns a normalized copy: ids as text, `label` '' when
 * absent, `data` {} when absent, `classes` [] when absent; nodes and edges keep their order.
 * Throws a GraphDataError naming every problem (unknown keys, missing or duplicate ids, an edge
 * whose end is not a node of the data, ...).
 * @param {*} input
 * @param {{knownNodes?: ?Set<string>}} [options]  `knownNodes`: ids of nodes outside `input` that
 *   an edge may end at (`mergeGraph`: the base's nodes).
 * @returns {{nodes: Array<{id:string, label:string, caption:?string, data:Object, classes:string[]}>,
 *            edges: Array<{id:string, source:string, target:string, label:string, caption:?string, data:Object, classes:string[]}>}}
 */
export function checkGraphData(input, { knownNodes = null } = {}) {
  if (!isPlainObject(input)) throw new GraphDataError(['it must be an object {nodes, edges}']);
  const problems = [];
  for (const key of Object.keys(input)) {
    if (key !== 'nodes' && key !== 'edges') problems.push(`unknown key ${quote(key)} (the keys are nodes, edges)`);
  }
  const rawNodes = input.nodes === undefined ? [] : input.nodes;
  const rawEdges = input.edges === undefined ? [] : input.edges;
  if (!Array.isArray(rawNodes)) problems.push('"nodes" must be a list');
  if (!Array.isArray(rawEdges)) problems.push('"edges" must be a list');
  if (problems.length) throw new GraphDataError(problems);
  if (rawNodes.length > GRAPH_LIMITS.nodes) problems.push(`${rawNodes.length} nodes (at most ${GRAPH_LIMITS.nodes})`);
  if (rawEdges.length > GRAPH_LIMITS.edges) problems.push(`${rawEdges.length} edges (at most ${GRAPH_LIMITS.edges})`);
  if (problems.length) throw new GraphDataError(problems);

  const check = (raw, i, kind) => {
    const at = `${kind}[${i}]`;
    if (!isPlainObject(raw)) {
      problems.push(`${at} must be an object`);
      return null;
    }
    const keys = kind === 'nodes' ? NODE_KEYS : EDGE_KEYS;
    for (const key of Object.keys(raw)) {
      if (!keys.includes(key)) problems.push(`${at} has the unknown key ${quote(key)} (the keys are ${keys.join(', ')})`);
    }
    if (!isId(raw.id)) problems.push(`${at}.id must be a non-empty text or a number`);
    if (raw.label !== undefined && typeof raw.label !== 'string') problems.push(`${at}.label must be a text`);
    if (raw.caption !== undefined && raw.caption !== null && typeof raw.caption !== 'string' && typeof raw.caption !== 'number') {
      problems.push(`${at}.caption must be a text`);
    }
    if (raw.data !== undefined && raw.data !== null && !isPlainObject(raw.data)) problems.push(`${at}.data must be an object`);
    problems.push(...classesProblems(raw.classes, at));
    const out = {
      id: isId(raw.id) ? String(raw.id) : '',
      label: typeof raw.label === 'string' ? raw.label : '',
      caption: raw.caption === undefined || raw.caption === null ? null : String(raw.caption),
      data: isPlainObject(raw.data) ? raw.data : {},
      classes: Array.isArray(raw.classes) ? raw.classes.filter((c) => typeof c === 'string') : [],
    };
    return out;
  };

  const nodes = [];
  const nodeIds = new Set();
  rawNodes.forEach((raw, i) => {
    const node = check(raw, i, 'nodes');
    if (!node) return;
    if (node.id && nodeIds.has(node.id)) problems.push(`nodes[${i}].id ${quote(node.id)} is used twice`);
    nodeIds.add(node.id);
    nodes.push(node);
  });
  const edges = [];
  const edgeIds = new Set();
  rawEdges.forEach((raw, i) => {
    const edge = check(raw, i, 'edges');
    if (!edge) return;
    if (edge.id && edgeIds.has(edge.id)) problems.push(`edges[${i}].id ${quote(edge.id)} is used twice`);
    edgeIds.add(edge.id);
    for (const end of ['source', 'target']) {
      if (!isId(raw[end])) problems.push(`edges[${i}].${end} must be the id of a node`);
      else if (!nodeIds.has(String(raw[end])) && !(knownNodes && knownNodes.has(String(raw[end])))) {
        problems.push(`edges[${i}].${end} ${quote(raw[end])} is not a node of the data`);
      }
    }
    edge.source = isId(raw.source) ? String(raw.source) : '';
    edge.target = isId(raw.target) ? String(raw.target) : '';
    edges.push(edge);
  });
  if (problems.length) throw new GraphDataError(problems);
  return { nodes, edges };
}

/**
 * The union of two graph datas (both as `checkGraphData` takes them; an edge of `addition` may
 * end at a node of `base`): every node and edge of `base`, then those of `addition` whose ids
 * are new. An element in both keeps its place and
 * takes `addition`'s content (the fresher read). `added` names the new ids, so a module can say
 * "nothing new" and the view can place the new vertices next to the one expanded.
 * @param {Object} base
 * @param {Object} addition
 * @returns {{graph: {nodes: Array, edges: Array}, added: {nodes: string[], edges: string[]}}}
 */
export function mergeGraph(base, addition) {
  const a = checkGraphData(base || { nodes: [], edges: [] });
  // An edge of the addition may end at a node of the base (a neighbour already drawn).
  const b = checkGraphData(addition || { nodes: [], edges: [] }, { knownNodes: new Set(a.nodes.map((n) => n.id)) });
  const merge = (left, right) => {
    const index = new Map(left.map((el, i) => [el.id, i]));
    const out = left.slice();
    const added = [];
    for (const el of right) {
      if (index.has(el.id)) {
        out[index.get(el.id)] = el;
      } else {
        index.set(el.id, out.length);
        out.push(el);
        added.push(el.id);
      }
    }
    return { out, added };
  };
  const nodes = merge(a.nodes, b.nodes);
  const edges = merge(a.edges, b.edges);
  return { graph: { nodes: nodes.out, edges: edges.out }, added: { nodes: nodes.added, edges: edges.added } };
}

/** A deep merge of style fragments (`border` merged by key). */
function layer(into, fragment) {
  if (!fragment) return into;
  for (const [key, v] of Object.entries(fragment)) {
    if (key === 'classes') continue;
    if (key === 'border' && isPlainObject(v)) into.border = { ...(into.border || {}), ...v };
    else into[key] = v;
  }
  return into;
}

function classStyle(spec, name) {
  const own = spec && isPlainObject(spec.classes) && Object.prototype.hasOwnProperty.call(spec.classes, name) ? spec.classes[name] : null;
  if (own) return own;
  return Object.prototype.hasOwnProperty.call(BUILTIN_CLASSES, name) ? BUILTIN_CLASSES[name] : null;
}

/**
 * The module's `styleOf(element, kind)` answer for one element: `{classes, style}`. A non-object
 * answer (null, undefined) is no change; an invalid one throws a TypeError naming the problems.
 */
function moduleStyle(styleOf, element, kind) {
  if (typeof styleOf !== 'function') return { classes: [], style: null };
  const answer = styleOf(element, kind);
  if (answer === undefined || answer === null || answer === false) return { classes: [], style: null };
  if (!isPlainObject(answer)) throw new TypeError(`api.graph: styleOf must return an object or null (got ${typeof answer} for ${kind} ${quote(element.id)}).`);
  const { classes, ...style } = answer;
  const problems = [...classesProblems(classes, 'styleOf()'), ...styleProblems(style, kind, `styleOf(${kind} ${String(element.id).slice(0, 60)})`)];
  if (problems.length) throw new TypeError(`api.graph: ${problems.join('; ')}.`);
  return { classes: classes || [], style };
}

/**
 * The fragments that apply to an element, in order: `"*"`, its label's entry, its classes, the
 * module's `styleOf` answer.
 */
function fragmentsOf(element, kind, spec, styleOf) {
  const byLabel = spec && isPlainObject(spec[kind === 'node' ? 'nodes' : 'edges']) ? spec[kind === 'node' ? 'nodes' : 'edges'] : {};
  const own = moduleStyle(styleOf, element, kind);
  const out = [];
  if (Object.prototype.hasOwnProperty.call(byLabel, '*')) out.push(byLabel['*']);
  if (element.label !== '*' && Object.prototype.hasOwnProperty.call(byLabel, element.label)) out.push(byLabel[element.label]);
  for (const name of [...(element.classes || []), ...own.classes]) {
    const style = classStyle(spec, name);
    if (style) out.push(style);
  }
  if (own.style) out.push(own.style);
  return out;
}

/**
 * The colour a style names, as a drawing takes it: '#…' as it is, a token through `token(name)`
 * ('' when unknown: `fallback`).
 */
export function colorOf(value, token, fallback) {
  if (typeof value !== 'string' || !value) return fallback;
  if (value.startsWith('--')) {
    const v = token ? String(token(value.slice(2)) || '').trim() : '';
    return v || fallback;
  }
  return value;
}

/**
 * The resolved style of a node: every key concrete, ready for a renderer.
 * @param {{id:string, label:string, caption:?string, data:Object, classes:string[]}} node  normalized
 * @param {Object} spec  The profile's `graph` (may be empty).
 * @param {{styleOf?:Function, labelColor:(label:string)=>string, token?:(name:string)=>string,
 *          borderColor?:string}} ctx
 * @returns {{color:string, shape:string, size:number, opacity:number, borderWidth:number,
 *            borderColor:string, borderStyle:string, caption:string}}
 */
export function resolveNodeStyle(node, spec, ctx) {
  const s = {};
  for (const f of fragmentsOf(node, 'node', spec, ctx.styleOf)) layer(s, f);
  const border = s.border || {};
  let caption = node.caption;
  if (caption === null && s.caption) {
    const v = fieldValue(node.data, s.caption);
    if (v !== undefined && v !== null && v !== '' && typeof v !== 'object') caption = String(v);
  }
  if (caption === null) caption = node.label || node.id;
  const fallbackBorder = ctx.borderColor || '#1c2330';
  return {
    color: colorOf(s.color, ctx.token, ctx.labelColor(node.label)),
    shape: s.shape || 'ellipse',
    size: s.size || NODE_SIZE.default,
    opacity: s.opacity === undefined ? 1 : s.opacity,
    borderWidth: border.width === undefined ? 0 : border.width,
    borderColor: colorOf(border.color, ctx.token, fallbackBorder),
    borderStyle: border.style || 'solid',
    caption: caption.length > GRAPH_LIMITS.text ? `${caption.slice(0, GRAPH_LIMITS.text - 1)}…` : caption,
  };
}

/**
 * The resolved style of an edge.
 * @param {{id:string, label:string, caption:?string, data:Object, classes:string[]}} edge  normalized
 * @param {Object} spec
 * @param {{styleOf?:Function, token?:(name:string)=>string, edgeColor?:string}} ctx
 * @returns {{color:string, lineStyle:string, arrow:boolean, width:number, opacity:number, caption:string}}
 */
export function resolveEdgeStyle(edge, spec, ctx) {
  const s = {};
  for (const f of fragmentsOf(edge, 'edge', spec, ctx.styleOf)) layer(s, f);
  let caption = edge.caption;
  if (caption === null && s.caption) {
    const v = fieldValue(edge.data, s.caption);
    if (v !== undefined && v !== null && v !== '' && typeof v !== 'object') caption = String(v);
  }
  if (caption === null) caption = edge.label;
  return {
    color: colorOf(s.color, ctx.token, ctx.edgeColor || '#9aa3b2'),
    lineStyle: s.style || 'solid',
    arrow: s.arrow === undefined ? true : s.arrow,
    width: s.width || EDGE_WIDTH.default,
    opacity: s.opacity === undefined ? 1 : s.opacity,
    caption: caption.length > GRAPH_LIMITS.text ? `${caption.slice(0, GRAPH_LIMITS.text - 1)}…` : caption,
  };
}

/**
 * The styled view of normalized graph data: a compact graph view (js/graph-data.js columns, so
 * the renderers, the layouts and the selection work on it as on the Graph panel's views) plus the
 * resolved style per element (`nodeStyle[i]`, `edgeStyle[j]`), the edge captions (`edgeName`)
 * and the data itself (`graph`, for the events and the card).
 * @param {{nodes:Array, edges:Array}} graph  from `checkGraphData`
 * @param {Object} spec
 * @param {Object} ctx  as `resolveNodeStyle` / `resolveEdgeStyle`
 */
export function styledView(graph, spec, ctx) {
  const labels = [];
  const labelIndex = new Map();
  const labelOf = (label) => {
    if (!labelIndex.has(label)) {
      labelIndex.set(label, labels.length);
      labels.push(label);
    }
    return labelIndex.get(label);
  };
  const nodeIndex = new Map(graph.nodes.map((n, i) => [n.id, i]));
  const nodeStyle = graph.nodes.map((n) => resolveNodeStyle(n, spec, ctx));
  const edgeStyle = graph.edges.map((e) => resolveEdgeStyle(e, spec, ctx));
  return {
    vertexIds: graph.nodes.map((n) => n.id),
    vertexLabel: Uint32Array.from(graph.nodes, (n) => labelOf(n.label)),
    vertexName: nodeStyle.map((s) => s.caption),
    edgeIds: graph.edges.map((e) => e.id),
    edgeSource: Uint32Array.from(graph.edges, (e) => nodeIndex.get(e.source)),
    edgeTarget: Uint32Array.from(graph.edges, (e) => nodeIndex.get(e.target)),
    edgeLabel: Uint32Array.from(graph.edges, (e) => labelOf(e.label)),
    edgeName: edgeStyle.map((s) => s.caption),
    labels,
    truncated: false,
    totalNodes: graph.nodes.length,
    totalEdges: graph.edges.length,
    nodeStyle,
    edgeStyle,
    graph,
  };
}

/**
 * The legend: one entry per node label and per edge label of the data (first-use order, with
 * the style a plain element of that label gets; the spec's `legendLabels` keeps only the node
 * or edge labels, or none), then the profile's `graph.legend` entries, then the `extra` ones
 * (the `legend` option). Each entry: `{kind: 'node'|'edge', text, style}` with a
 * resolved style.
 */
export function legendEntries(graph, spec, extra, ctx) {
  const out = [];
  const seen = { node: new Set(), edge: new Set() };
  const plainCtx = { ...ctx, styleOf: null };
  const which = spec && spec.legendLabels !== undefined ? spec.legendLabels : true;
  const listNodes = which === true || which === 'nodes';
  const listEdges = which === true || which === 'edges';
  for (const n of listNodes ? graph.nodes : []) {
    if (seen.node.has(n.label)) continue;
    seen.node.add(n.label);
    const style = resolveNodeStyle({ id: n.label, label: n.label, caption: n.label || '(no label)', data: {}, classes: [] }, spec, plainCtx);
    out.push({ kind: 'node', text: n.label || '(no label)', style });
  }
  for (const e of listEdges ? graph.edges : []) {
    if (seen.edge.has(e.label)) continue;
    seen.edge.add(e.label);
    const style = resolveEdgeStyle({ id: e.label, label: e.label, caption: e.label || '(no label)', data: {}, classes: [] }, spec, plainCtx);
    out.push({ kind: 'edge', text: e.label || '(no label)', style });
  }
  const entries = [...((spec && Array.isArray(spec.legend)) ? spec.legend : []), ...(Array.isArray(extra) ? extra : [])];
  // An entry without a colour shows the neutral one (it explains a border, a dash, a fade).
  const entryCtx = { ...plainCtx, labelColor: () => ctx.neutralColor || '#8b94a3' };
  for (const entry of entries) {
    const kind = entry.node !== undefined ? 'node' : 'edge';
    const style = kind === 'node'
      ? resolveNodeStyle({ id: 'legend', label: '', caption: entry.text, data: {}, classes: [] }, { classes: spec && spec.classes, nodes: { '*': entry.node } }, entryCtx)
      : resolveEdgeStyle({ id: 'legend', label: '', caption: entry.text, data: {}, classes: [] }, { classes: spec && spec.classes, edges: { '*': entry.edge } }, entryCtx);
    out.push({ kind, text: entry.text, style, extra: true });
  }
  return out;
}

/** The option keys of `api.graph(data, options)`. */
export const GRAPH_OPTION_KEYS = Object.freeze([
  'styleOf', 'onSelect', 'onExpand', 'selected', 'card', 'legend', 'layout', 'height', 'highlight', 'emptyText',
]);

/** The layouts a graph view offers (the Detailed tier's built-ins; js/graph-data.js `layoutFor`). */
export const GRAPH_LAYOUTS = Object.freeze(['cose', 'circle', 'concentric', 'breadthfirst', 'grid']);

/** The highlight modes of a selection. */
export const HIGHLIGHT_MODES = Object.freeze(['neighbours', 'element', 'none']);

/**
 * Checks the options of `api.graph` and returns them normalized; throws a TypeError naming every
 * problem (an unknown option is a problem, so a typo is never ignored).
 *   styleOf    (element, kind) => {classes?, ...style} | null: the per-element style (above)
 *   onSelect   (selection) => void: a click chose an element ({kind, id, label, data, element})
 *              or empty space (null)
 *   onExpand   (node) => void: a double click on a node (or the side card's Expand button)
 *   selected   {kind: 'node'|'edge', id} | null: the selection set from outside (e.g. a tab
 *              parameter); a click still changes it
 *   card       false (default: no side card) | true | {actions, onAction, expand}: the side card
 *              of the selected element through `api.card` (js/profile-cards.js; `actions` a list
 *              or (element) => list; `expand` a button text that calls onExpand, default "Expand"
 *              when onExpand is given, false for none, or (selection) => text | false per node)
 *   legend     true (default) | false | [{text, node} | {text, edge}]: extra legend entries
 *   layout     one of GRAPH_LAYOUTS (default 'cose')
 *   height     the drawing's height (CSS length, default '520px')
 *   highlight  one of HIGHLIGHT_MODES (default 'neighbours': the selection with its neighbours)
 *   emptyText  the text shown for a graph without nodes
 */
export function checkGraphOptions(options) {
  if (options === undefined || options === null) options = {};
  if (!isPlainObject(options)) throw new TypeError('api.graph: the options must be an object.');
  const problems = [];
  for (const key of Object.keys(options)) {
    if (!GRAPH_OPTION_KEYS.includes(key)) problems.push(`unknown option ${quote(key)} (the options are ${GRAPH_OPTION_KEYS.join(', ')})`);
  }
  for (const key of ['styleOf', 'onSelect', 'onExpand']) {
    if (options[key] !== undefined && typeof options[key] !== 'function') problems.push(`"${key}" must be a function`);
  }
  const sel = options.selected;
  if (sel !== undefined && sel !== null && !(isPlainObject(sel) && (sel.kind === 'node' || sel.kind === 'edge') && isId(sel.id))) {
    problems.push('"selected" must be null or {kind: "node"|"edge", id}');
  }
  const card = options.card;
  if (card !== undefined && typeof card !== 'boolean') {
    if (!isPlainObject(card)) {
      problems.push('"card" must be true, false or {actions, onAction, expand}');
    } else {
      for (const key of Object.keys(card)) {
        if (!['actions', 'onAction', 'expand'].includes(key)) problems.push(`"card" has the unknown key ${quote(key)} (the keys are actions, onAction, expand)`);
      }
      if (card.actions !== undefined && !Array.isArray(card.actions) && typeof card.actions !== 'function') problems.push('"card.actions" must be a list or a function (element) => list');
      if (card.onAction !== undefined && typeof card.onAction !== 'function') problems.push('"card.onAction" must be a function');
      if (card.expand !== undefined && card.expand !== false && typeof card.expand !== 'function' && (typeof card.expand !== 'string' || !card.expand.trim())) {
        problems.push('"card.expand" must be a button text, false or a function (selection) => text | false');
      }
    }
  }
  if (options.legend !== undefined && typeof options.legend !== 'boolean') problems.push(...legendProblems(options.legend, 'legend'));
  if (options.layout !== undefined && !GRAPH_LAYOUTS.includes(options.layout)) problems.push(`"layout" must be one of ${GRAPH_LAYOUTS.join(', ')}`);
  if (options.height !== undefined && (typeof options.height !== 'string' || !/^\d+(\.\d+)?(px|vh|em|rem|%)$/.test(options.height))) {
    problems.push('"height" must be a CSS length like "520px" or "70vh"');
  }
  if (options.highlight !== undefined && !HIGHLIGHT_MODES.includes(options.highlight)) problems.push(`"highlight" must be one of ${HIGHLIGHT_MODES.join(', ')}`);
  if (options.emptyText !== undefined && typeof options.emptyText !== 'string') problems.push('"emptyText" must be a text');
  if (problems.length) throw new TypeError(`api.graph: ${problems.join('; ')}.`);
  return Object.freeze({
    styleOf: options.styleOf || null,
    onSelect: options.onSelect || null,
    onExpand: options.onExpand || null,
    selected: sel ? { kind: sel.kind, id: String(sel.id) } : null,
    card: card === undefined ? false : card,
    legend: options.legend === undefined ? true : options.legend,
    layout: options.layout || 'cose',
    height: options.height || '520px',
    highlight: options.highlight || 'neighbours',
    emptyText: options.emptyText === undefined ? 'Nothing to show.' : options.emptyText,
  });
}

/**
 * What a selection highlights (`renderer.select` ids): the element alone, or a node with its
 * edges and neighbours / an edge with its two ends ('neighbours'); null for none.
 * @param {{nodes:Array, edges:Array}} graph  normalized
 * @param {?{kind:string, id:string}} selection
 * @param {string} mode  HIGHLIGHT_MODES
 * @returns {?{nodes:string[], edges:string[]}}
 */
export function highlightIds(graph, selection, mode = 'neighbours') {
  if (!selection || mode === 'none') return null;
  if (selection.kind === 'node') {
    if (!graph.nodes.some((n) => n.id === selection.id)) return null;
    if (mode === 'element') return { nodes: [selection.id], edges: [] };
    const nodes = new Set([selection.id]);
    const edges = [];
    for (const e of graph.edges) {
      if (e.source === selection.id || e.target === selection.id) {
        edges.push(e.id);
        nodes.add(e.source);
        nodes.add(e.target);
      }
    }
    return { nodes: [...nodes], edges };
  }
  const edge = graph.edges.find((e) => e.id === selection.id);
  if (!edge) return null;
  return { nodes: mode === 'element' ? [] : [...new Set([edge.source, edge.target])], edges: [edge.id] };
}

/**
 * What the events hand a module: `{kind, id, label, data, element}` of the selected element
 * (`element` the normalized node or edge), or null when it is not in the data.
 */
export function selectionOf(graph, selection) {
  if (!selection) return null;
  const list = selection.kind === 'node' ? graph.nodes : graph.edges;
  const element = list.find((el) => el.id === selection.id);
  if (!element) return null;
  return { kind: selection.kind, id: element.id, label: element.label, data: element.data, element };
}

/**
 * Positions for a view that changed (an expansion merged in, a reload): every vertex that was
 * drawn before keeps its position; a new vertex goes next to its drawn neighbours (their middle,
 * spread on a ring), else around `anchor` (the vertex just expanded), else around the middle of
 * the drawing. Returns null when no vertex of `ids` was drawn before (a new drawing: lay it out).
 * Deterministic (no randomness), so tests can pin it.
 * @param {string[]} oldIds   The vertex ids of the previous drawing, in its order.
 * @param {ArrayLike<number>|null} oldXY  Their positions (`x, y` per vertex).
 * @param {{nodes:Array, edges:Array}} graph  The new normalized data.
 * @param {{anchor?:?string, spacing?:number}} [options]
 * @returns {Float32Array|null}
 */
export function carryPositions(oldIds, oldXY, graph, { anchor = null, spacing = 70 } = {}) {
  if (!oldIds || !oldXY || oldXY.length < 2 * oldIds.length) return null;
  const old = new Map();
  oldIds.forEach((id, i) => old.set(String(id), [oldXY[2 * i], oldXY[2 * i + 1]]));
  const n = graph.nodes.length;
  const out = new Float32Array(2 * n);
  const placed = new Map();
  graph.nodes.forEach((node, i) => {
    const p = old.get(node.id);
    if (p) {
      out[2 * i] = p[0];
      out[2 * i + 1] = p[1];
      placed.set(node.id, p);
    }
  });
  if (!placed.size) return null;
  if (placed.size === n) return out;
  let cx = 0;
  let cy = 0;
  for (const [x, y] of placed.values()) {
    cx += x;
    cy += y;
  }
  cx /= placed.size;
  cy /= placed.size;
  const neighbours = new Map();
  for (const e of graph.edges) {
    if (!neighbours.has(e.source)) neighbours.set(e.source, []);
    if (!neighbours.has(e.target)) neighbours.set(e.target, []);
    neighbours.get(e.source).push(e.target);
    neighbours.get(e.target).push(e.source);
  }
  // New vertices around the same centre share a ring, spaced by angle.
  const rings = new Map();
  const pending = [];
  graph.nodes.forEach((node, i) => {
    if (placed.has(node.id)) return;
    const near = (neighbours.get(node.id) || []).map((id) => placed.get(id)).filter(Boolean);
    let centre;
    if (near.length) {
      centre = [near.reduce((s, p) => s + p[0], 0) / near.length, near.reduce((s, p) => s + p[1], 0) / near.length];
    } else if (anchor && placed.has(String(anchor))) {
      centre = placed.get(String(anchor));
    } else {
      centre = [cx, cy];
    }
    const key = `${centre[0]},${centre[1]}`;
    if (!rings.has(key)) rings.set(key, { centre, members: [] });
    rings.get(key).members.push(i);
    pending.push(i);
  });
  for (const { centre, members } of rings.values()) {
    const count = members.length;
    const radius = Math.max(spacing, (count * spacing * 0.6) / (2 * Math.PI));
    members.forEach((i, k) => {
      const angle = (2 * Math.PI * k) / count - Math.PI / 2;
      out[2 * i] = centre[0] + radius * Math.cos(angle);
      out[2 * i + 1] = centre[1] + radius * Math.sin(angle);
    });
  }
  return out;
}
