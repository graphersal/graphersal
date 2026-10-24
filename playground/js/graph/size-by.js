// Vertex size by degree (pure: no DOM; tested under Node by tests/graph-size.test.mjs).
//
// The Display menu's "Vertex size" (`graph.sizeBy` in js/util.js `storage`): Off, Degree (in +
// out), In-degree, Out-degree. While it is not Off the panel asks the engine for the degree
// columns (`vertexInDegree`/`vertexOutDegree`: degrees in the WHOLE graph) and hands each renderer
// a SCALE per vertex in [0, 1]:
//
//   t = log1p(d) / log1p(dMax)          (dMax: the largest degree among the drawn vertices)
//
// and each renderer maps it to its own size range: `size = min + (max - min) * t`
// (`SIZE_RANGE`). A drawing without degree columns (a result run before the option was switched
// on, a drawing patched by edit mode) uses the degrees within the drawing instead.

/** The choices of the Display menu's "Vertex size" row. */
export const SIZE_BY_CHOICES = Object.freeze([
  Object.freeze({ id: 'off', label: 'Off', title: 'Every vertex has the same size' }),
  Object.freeze({ id: 'degree', label: 'Degree', title: 'Size by the number of edges (in + out) in the whole graph, log scale' }),
  Object.freeze({ id: 'in', label: 'In', title: 'Size by the number of incoming edges in the whole graph, log scale' }),
  Object.freeze({ id: 'out', label: 'Out', title: 'Size by the number of outgoing edges in the whole graph, log scale' }),
]);

/** A stored choice, or 'off'. */
export function normalizeSizeBy(v) {
  return SIZE_BY_CHOICES.some((c) => c.id === v) ? v : 'off';
}

/**
 * The size range per renderer: Cytoscape (Detailed) in px at zoom 1 (its fixed size is 24, the
 * caption sits under the node, so the range stays readable at both ends); sigma (Fast) as factors
 * of its size for the drawing's vertex count (sigma-renderer.js `nodeSizeFor`: 7 px up to 500
 * vertices down to 2.5 px above 20 000).
 */
export const SIZE_RANGE = Object.freeze({
  detailed: Object.freeze({ min: 12, max: 40 }),
  fast: Object.freeze({ minFactor: 0.5, maxFactor: 1.6 }),
});

/**
 * The degree of every vertex of compact view `view` under `mode` ('degree' | 'in' | 'out'), as a
 * Uint32Array: from the degree columns when the view has them for every vertex, else counted
 * over the drawn edges.
 */
export function degreesOf(view, mode) {
  const n = view ? view.vertexIds.length : 0;
  const out = new Uint32Array(n);
  if (!n) return out;
  const inCol = view.vertexInDegree;
  const outCol = view.vertexOutDegree;
  if (inCol && outCol && inCol.length === n && outCol.length === n) {
    for (let i = 0; i < n; i++) out[i] = mode === 'in' ? inCol[i] : mode === 'out' ? outCol[i] : inCol[i] + outCol[i];
    return out;
  }
  const m = view.edgeIds.length;
  for (let j = 0; j < m; j++) {
    if (mode !== 'in') out[view.edgeSource[j]]++;
    if (mode !== 'out') out[view.edgeTarget[j]]++;
  }
  return out;
}

/**
 * The scale in [0, 1] of each degree: `log1p(d) / log1p(dMax)` (all 0 when every degree is 0).
 * @param {ArrayLike<number>} degrees
 * @returns {Float32Array}
 */
export function degreeScale(degrees) {
  const n = degrees ? degrees.length : 0;
  const out = new Float32Array(n);
  let max = 0;
  for (let i = 0; i < n; i++) if (degrees[i] > max) max = degrees[i];
  if (!(max > 0)) return out;
  const denom = Math.log1p(max);
  for (let i = 0; i < n; i++) out[i] = degrees[i] > 0 ? Math.log1p(degrees[i]) / denom : 0;
  return out;
}

/** The size of scale `t` in `[min, max]` (`t` clamped to [0, 1]; a non-number is 0). */
export function sizeOf(t, min, max) {
  const x = Number.isFinite(t) ? Math.min(1, Math.max(0, t)) : 0;
  return min + (max - min) * x;
}

/**
 * The scales the panel hands to the canvas (`nodeScale`): null when `mode` is 'off' or `view` is
 * not a data drawing, else `degreeScale(degreesOf(view, mode))`.
 */
export function nodeScaleFor(view, mode) {
  if (!view || !Array.isArray(view.vertexIds) || normalizeSizeBy(mode) === 'off') return null;
  return degreeScale(degreesOf(view, mode));
}
