// The Graph panel's RESULT HIGHLIGHT (pure: tested under Node, tests/highlight.test.mjs). In the
// whole-graph drawing the elements the active tab's last query result refers to are highlighted
// and everything else is faded (the renderer's `select(ids)`, js/graph/renderer.js). The toolbar's
// "Clear highlight" button and Esc on the drawing DISMISS it: the drawing is shown normally again.
//
// A dismissal belongs to one result (the result's subgraph object): a later query result
// highlights again, and so does another tab's result when the user switches tabs; switching back
// to the tab whose highlight was cleared keeps it cleared. The "highlight result" checkbox is the
// lasting switch (it stays off for every result until turned on again).

/**
 * The ids to highlight (`{nodes, edges}`), or null for a normal drawing.
 * @param {{source:string, overview:boolean, drill:boolean, enabled:boolean, subgraph:Object|null,
 *          dismissed:(subgraph:Object) => boolean, idsOf:(subgraph:Object) => Object}} input
 *   `source`: the panel's view ('graph' highlights); `overview`: the label overview is drawn;
 *   `drill`: a drill-down is drawn; `enabled`: the "highlight result" checkbox; `subgraph`: the
 *   active result's subgraph; `dismissed`: whether its highlight was cleared; `idsOf`: its ids.
 */
export function resultHighlight({ source, overview, drill, enabled, subgraph, dismissed, idsOf }) {
  if (drill || source !== 'graph' || overview || !enabled || !subgraph) return null;
  if (dismissed(subgraph)) return null;
  return idsOf(subgraph);
}

/**
 * The dismissed results: `dismiss(subgraph)`, `isDismissed(subgraph)`. Weak, so a result that is
 * gone (its tab closed, a new run) is forgotten. `version` counts the dismissals (a reactive
 * caller re-reads on change).
 */
export function createDismissals() {
  const set = new WeakSet();
  const api = {
    version: 0,
    dismiss(subgraph) {
      if (!subgraph || typeof subgraph !== 'object' || set.has(subgraph)) return false;
      set.add(subgraph);
      api.version++;
      return true;
    },
    isDismissed(subgraph) {
      return !!subgraph && typeof subgraph === 'object' && set.has(subgraph);
    },
  };
  return api;
}

/**
 * Whether a key press clears the highlight: Esc without modifiers, not in a field or a dialog.
 * @param {{key:string, altKey?:boolean, ctrlKey?:boolean, metaKey?:boolean, shiftKey?:boolean, inField?:boolean}} event
 */
export function isClearKey(event) {
  if (!event || event.key !== 'Escape') return false;
  if (event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return false;
  return !event.inField;
}
