// The Graph panel's toolbar model (pure: no DOM, no Vue; tested under Node by
// tests/graph-toolbar.test.mjs).
//
// The row keeps the frequent actions: [Filter] [Layout] [Edit] ... [Clear highlight] [Fit] [Export]
// [Display]. The settings (drawing tier, IDs, edge labels, highlight, vertex size, help) live in
// the Display menu. When the panel is narrow, `toolbarFit` moves Layout, then Edit, then Filter
// into the Display menu, and as a last step shortens the labels of the right-hand buttons, so the
// row never wraps or overflows. The panel measures the row and the natural width of each item (a
// ResizeObserver) and asks `toolbarFit` which items stay inline.

/**
 * The Filter button: its main part opens the filter drawer (components/graph-filter.js), its
 * switch turns the filter off and on without losing it (`graph-panel.js` `openFilter` /
 * `toggleFilter`). While false the button is not rendered; its slot is the first item of the row
 * (`data-tb="filter"`).
 */
export const FILTER_ENABLED = true;

/** Left-hand items that can move into the Display menu, in the order they move. */
export const COLLAPSIBLE = Object.freeze(['layout', 'edit', 'filter']);

/**
 * Items whose label `compact` shortens. While compact the panel measures them into
 * `compactWidths`, never into `widths`: their natural width must stay the full one, or the fit
 * would flip back and forth.
 */
export const SHORTENED = Object.freeze(new Set(['clear', 'fit', 'export', 'display']));

/** Widths (px) of the shortened items until measured. */
export const DEFAULT_COMPACT_WIDTHS = Object.freeze({ clear: 62, fit: 32, export: 44, display: 44 });

/** Gap between toolbar items in px (`.graph-toolbar` in css/app.css). */
export const TOOLBAR_GAP = 6;

/**
 * Natural widths (px) used until an item was measured once (the panel measures every shown item;
 * a collapsed one keeps its last measured width).
 */
export const DEFAULT_WIDTHS = Object.freeze({
  filter: 110, layout: 130, edit: 66, clear: 118, fit: 52, export: 84, display: 84,
});

/**
 * Which items stay in the row. `available`: the row's inner width (px); `widths`: natural item
 * widths by id (missing ones from `DEFAULT_WIDTHS`); `show`: which items exist now
 * (`{filter, clear}`; Layout, Edit, Fit, Export and Display always exist).
 * Returns `{inline: [...ids], inMenu: [...ids], compact}`: `inMenu` are the collapsible items the
 * Display menu shows instead; `compact` shortens the `SHORTENED` items. Afterwards every dropped
 * item that still fits comes back, last dropped first (when compact, measured beside the
 * shortened buttons: `compactWidths`, missing ones from `DEFAULT_COMPACT_WIDTHS`).
 */
export function toolbarFit({ available, widths = {}, compactWidths = {}, show = {} }) {
  const pick = (table, fallback, id) => (Number.isFinite(table[id]) && table[id] > 0 ? table[id] : fallback[id]);
  const w = (id) => pick(widths, DEFAULT_WIDTHS, id);
  const cw = (id) => (SHORTENED.has(id) ? pick(compactWidths, DEFAULT_COMPACT_WIDTHS, id) : w(id));
  const fixed = ['clear', 'fit', 'export', 'display'].filter((id) => id !== 'clear' || show.clear);
  const left = COLLAPSIBLE.filter((id) => id !== 'filter' || show.filter);
  const sumOf = (ids, width) => ids.reduce((s, id) => s + width(id), 0) + TOOLBAR_GAP * Math.max(0, ids.length - 1);
  const need = (ids) => sumOf(ids, w);
  const room = Number.isFinite(available) ? available : Infinity;
  // Drop collapsible items in COLLAPSIBLE order until the rest fits; compact when even the fixed
  // items alone do not fit.
  const inMenu = [];
  let inline = [...left];
  let compact = true;
  for (let i = 0; i <= COLLAPSIBLE.length; i++) {
    if (need([...inline, ...fixed]) <= room) {
      compact = false;
      break;
    }
    const next = COLLAPSIBLE[i];
    if (next && inline.includes(next)) {
      inline = inline.filter((id) => id !== next);
      inMenu.push(next);
    }
  }
  // Bring dropped items back, last dropped first, wherever one still fits (dropping Edit may have
  // left room for it once Filter went too; compact buttons leave room as well).
  const width = compact ? cw : w;
  for (const id of [...inMenu].reverse()) {
    const ids = left.filter((x) => x === id || inline.includes(x));
    if (sumOf([...ids, ...fixed], width) <= room) inline = ids;
  }
  return { inline: [...inline, ...fixed], inMenu: inMenu.filter((id) => !inline.includes(id)), compact };
}

/**
 * The Filter button's badge: "· 2" (active conditions), "· off" (the filter is switched off), ""
 * (no conditions). `count`: the number of active conditions, `enabled`: the button's toggle.
 */
export function filterBadge({ count = 0, enabled = true } = {}) {
  const n = Number.isFinite(count) && count > 0 ? Math.floor(count) : 0;
  if (!n) return '';
  return enabled ? `· ${n}` : '· off';
}

/** The interaction help shown at the bottom of the Display menu (was the toolbar's ⓘ tip). */
export const GRAPH_HELP = Object.freeze([
  'Click an element for its properties.',
  'Double-click a vertex to select its neighbours (again: one hop more); in the Neighbourhood view it fetches them from the engine.',
  'In the Labels view double-click a label or an edge to draw a sample.',
  'Pinch or wheel to zoom, drag to pan, double-click empty space to fit.',
]);

/** The Display button's tooltip: what it holds and the tier in use (`tierLabel`, e.g. "Auto · Fast"). */
export function displayTitle(tierLabel) {
  return `Display: drawing tier (${tierLabel}), vertex and edge IDs, edge labels, highlight, vertex size, help`;
}
