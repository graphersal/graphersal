// How the drawings show where an element of a LAYER comes from (pure: no DOM; tested in
// tests/origin-style.test.mjs). A layered graph's drawings carry the columns `vertexOrigin` and
// `edgeOrigin` (parallel to `vertexIds` / `edgeIds`, values 'base' | 'added' | 'modified', null
// for a stale handle; crates/graphersal-session `CompactGraph`); a plain graph's drawings have
// neither, and nothing here changes how they are drawn.
//
// Colour is never the only cue (the colours are a colour-blind-safe pair, Okabe-Ito blue and
// reddish purple, light and dark variants; CSS tokens `--origin-added` / `--origin-modified`):
// - Detailed (Cytoscape): an added vertex has a SOLID outline, a modified one a DASHED outline; an
//   added edge is drawn solid and wider in the added colour, a modified edge DASHED in the
//   modified colour. Base elements are drawn exactly as before.
// - Fast (sigma.js, WebGL: no dashes, no borders in its node program): a ring drawn on the canvas
//   layer (solid for added, dashed for modified) around up to `FAST_RING_MAX` vertices; edges get
//   the colour and a width of their own (added wider than modified), and the edge's hover label
//   names its origin.
// Removed elements are never drawn (the diff panel lists them).

/** The origins that are styled (base elements are drawn as on a plain graph). */
export const STYLED_ORIGINS = Object.freeze(['added', 'modified']);

/** Every origin value a drawing column may hold (besides null). */
export const ORIGINS = Object.freeze(['base', 'added', 'modified']);

/** The colours when the CSS tokens cannot be read (tests, an old stylesheet). */
export const ORIGIN_COLORS = Object.freeze({
  light: Object.freeze({ added: '#0072b2', modified: '#b5508a' }),
  dark: Object.freeze({ added: '#56b4e9', modified: '#e69ac4' }),
});

/** The words for an origin (legend, inspector, edge hover label). */
export const ORIGIN_TEXT = Object.freeze({
  added: 'added in this layer',
  modified: 'changed in this layer',
  base: 'from the base',
});

/** The second cue of each origin, as the legend names it. */
export const ORIGIN_CUE = Object.freeze({
  added: 'solid outline',
  modified: 'dashed outline',
  base: 'no outline',
});

/** The Fast tier draws a ring around at most this many changed vertices (canvas work per frame). */
export const FAST_RING_MAX = 10000;

/** The dash of a modified element's outline (Detailed: Cytoscape's `dashed`; Fast: this pattern). */
export const MODIFIED_DASH = Object.freeze([3, 2]);

/** Whether a drawing carries origin columns (only a layer's drawings do). */
export function hasOrigins(view) {
  return !!(view && (Array.isArray(view.vertexOrigin) || Array.isArray(view.edgeOrigin)));
}

/**
 * The origin of vertex (`group` 'node') or edge (`group` 'edge') `index` of `view`: 'base',
 * 'added', 'modified', or null (a plain graph, a stale handle, an element a client-side edit
 * appended without one).
 */
export function originAt(view, group, index) {
  if (!view) return null;
  const column = group === 'edge' ? view.edgeOrigin : view.vertexOrigin;
  if (!column || index < 0 || index >= column.length) return null;
  const value = column[index];
  return ORIGINS.includes(value) ? value : null;
}

/** The origin colours of a theme: the CSS tokens' values (`read(name)`) over the fallbacks. */
export function originColors(dark, read = null) {
  const fallback = dark ? ORIGIN_COLORS.dark : ORIGIN_COLORS.light;
  const pick = (name, def) => {
    const v = read ? read(name) : '';
    return v || def;
  };
  return { added: pick('--origin-added', fallback.added), modified: pick('--origin-modified', fallback.modified) };
}

// ---------------------------------------------------------------- Detailed tier (Cytoscape)

/** The Cytoscape class of an origin ('' for base and null: no rule applies). */
export function cyOriginClass(origin) {
  return STYLED_ORIGINS.includes(origin) ? `origin-${origin}` : '';
}

/**
 * The Cytoscape style rules of the origin classes (after the label and profile styles, before
 * the highlight and selection rules, which still win). `arrows`: the size plan draws arrow heads.
 */
export function cyOriginRules(colors, { arrows = true } = {}) {
  return [
    { selector: 'node.origin-added', style: { 'border-width': 3, 'border-color': colors.added, 'border-style': 'solid' } },
    { selector: 'node.origin-modified', style: { 'border-width': 3, 'border-color': colors.modified, 'border-style': 'dashed' } },
    {
      selector: 'edge.origin-added',
      style: { 'line-color': colors.added, 'target-arrow-color': colors.added, 'line-style': 'solid', width: 2.6, ...(arrows ? {} : { 'target-arrow-shape': 'none' }) },
    },
    {
      selector: 'edge.origin-modified',
      style: { 'line-color': colors.modified, 'target-arrow-color': colors.modified, 'line-style': 'dashed', width: 2, ...(arrows ? {} : { 'target-arrow-shape': 'none' }) },
    },
  ];
}

// ---------------------------------------------------------------- Fast tier (sigma.js)

/**
 * The sigma node attributes of an origin: `{origin, ringColor, ringDash, highlighted: true}` (the
 * renderer's hover layer draws the ring of a highlighted vertex), or null for base / null /
 * `ringed` false (more changed vertices than `FAST_RING_MAX`).
 */
export function sigmaNodeOrigin(origin, colors, ringed = true) {
  if (!ringed || !STYLED_ORIGINS.includes(origin)) return null;
  return {
    origin,
    ringColor: colors[origin],
    ringDash: origin === 'modified' ? [...MODIFIED_DASH] : [],
    highlighted: true,
  };
}

/** The sigma edge attributes of an origin: `{origin, color, size}`, or null for base / null. */
export function sigmaEdgeOrigin(origin, colors) {
  if (!STYLED_ORIGINS.includes(origin)) return null;
  return { origin, color: colors[origin], size: origin === 'added' ? 2.2 : 1.4 };
}

/** Whether the Fast tier rings the changed vertices of `view` (at most `FAST_RING_MAX`). */
export function fastRings(view, max = FAST_RING_MAX) {
  const column = view && view.vertexOrigin;
  if (!column) return false;
  let changed = 0;
  for (const o of column) if (o === 'added' || o === 'modified') changed++;
  return changed <= max;
}

/** An edge's hover label with its origin: `knows · added` (base and null: the caption alone). */
export function originCaption(caption, origin) {
  return STYLED_ORIGINS.includes(origin) ? `${caption} · ${origin}` : caption;
}

// ---------------------------------------------------------------- legend

/**
 * The legend entries of the origins: `[{origin, text, cue}]` for added, modified and base, only
 * when the graph is a layer (`layer`: `info.layer`) and the drawing carries origins; else `[]`.
 */
export function originLegend(view, layer) {
  if (!layer || !hasOrigins(view)) return [];
  return ORIGINS.slice(1).concat('base').map((origin) => ({ origin, text: ORIGIN_TEXT[origin], cue: ORIGIN_CUE[origin] }));
}
