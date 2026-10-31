// When the drawing's container really changed size (components/graph-canvas.js). Pure: no DOM,
// tested under Node.
//
// The renderers size their canvases from the container in whole pixels, rounded, so a container of
// 758.5 px gets a 759 px canvas. Nothing may let that half pixel reach a scroll container (the CSS
// clips `.cy-host`), and this guard keeps sub-pixel jitter from re-sizing the renderer: only a
// change of the whole-pixel size counts. Without both, Windows' classic scrollbars (which take
// layout space) made the Graph panel flicker: the canvas overflowed by half a pixel, both
// scrollbars appeared, the container shrank, the canvas was resized smaller, the scrollbars went
// away, the container grew, and so on, about ten times a second while a notice was shown.

/** The whole-pixel size of a measured box (`{width, height}`, CSS pixels, maybe fractional). */
export function wholeSize(box) {
  const w = Number(box && box.width);
  const h = Number(box && box.height);
  return {
    width: Number.isFinite(w) && w > 0 ? Math.floor(w) : 0,
    height: Number.isFinite(h) && h > 0 ? Math.floor(h) : 0,
  };
}

/**
 * A guard remembering the last size the renderer was given. `changed(box)` answers whether the
 * whole-pixel size differs from it (and then remembers the new one); `reset()` forgets it, so the
 * next measurement counts (after the drawing was hidden or replaced).
 */
export function createResizeGuard() {
  let last = null;
  return {
    changed(box) {
      const next = wholeSize(box);
      if (last && last.width === next.width && last.height === next.height) return false;
      last = next;
      return true;
    },
    reset() {
      last = null;
    },
  };
}
