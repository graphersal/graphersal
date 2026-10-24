// Node tests of the Graph panel's toolbar model (js/graph/toolbar.js): which items collapse into
// the Display menu at which width, the Filter badge, and the placeholder switch.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  FILTER_ENABLED, COLLAPSIBLE, SHORTENED, TOOLBAR_GAP, DEFAULT_WIDTHS, DEFAULT_COMPACT_WIDTHS, toolbarFit, filterBadge, GRAPH_HELP, displayTitle,
} from '../js/graph/toolbar.js';

const W = DEFAULT_WIDTHS;
const sum = (ids) => ids.reduce((s, id) => s + W[id], 0) + TOOLBAR_GAP * (ids.length - 1);

test('a wide row keeps everything inline', () => {
  const r = toolbarFit({ available: 2000, show: { filter: true, clear: true } });
  assert.deepEqual(r.inline, ['layout', 'edit', 'filter', 'clear', 'fit', 'export', 'display']);
  assert.deepEqual(r.inMenu, []);
  assert.equal(r.compact, false);
});

test('without an active highlight or filter their items do not exist', () => {
  const r = toolbarFit({ available: 2000 });
  assert.deepEqual(r.inline, ['layout', 'edit', 'fit', 'export', 'display']);
});

test('items collapse in order: Layout, then Edit, then Filter, then compact labels', () => {
  const show = { filter: true, clear: true };
  const all = ['layout', 'edit', 'filter', 'clear', 'fit', 'export', 'display'];
  assert.equal(toolbarFit({ available: sum(all), show }).inMenu.length, 0, 'exactly fitting stays');
  assert.deepEqual(toolbarFit({ available: sum(all) - 1, show }).inMenu, ['layout']);
  const noLayout = all.filter((id) => id !== 'layout');
  assert.deepEqual(toolbarFit({ available: sum(noLayout) - 1, show }).inMenu, ['layout', 'edit']);
  const fixed = ['clear', 'fit', 'export', 'display'];
  const r = toolbarFit({ available: sum(fixed) + W.filter + TOOLBAR_GAP, show });
  assert.deepEqual(r.inMenu, ['layout', 'edit']);
  assert.equal(r.compact, false);
  const C = DEFAULT_COMPACT_WIDTHS;
  const tight = toolbarFit({ available: C.clear + C.fit + C.export + C.display + 3 * TOOLBAR_GAP + 5, show });
  assert.deepEqual(tight.inMenu, ['layout', 'edit', 'filter']);
  assert.deepEqual(tight.inline, fixed);
  assert.equal(tight.compact, true);
  assert.deepEqual(COLLAPSIBLE, ['layout', 'edit', 'filter']);
});

test('measured widths win over the defaults; bad values fall back', () => {
  const widths = { layout: 300, edit: 0, fit: NaN };
  const r = toolbarFit({ available: 300 + W.edit + W.fit + W.export + W.display + 4 * TOOLBAR_GAP, widths });
  assert.deepEqual(r.inMenu, []);
  assert.deepEqual(toolbarFit({ available: 400, widths }).inMenu, ['layout']);
});

test('every right-hand item is shortened when compact (so none is measured short)', () => {
  const tight = toolbarFit({ available: 10, show: { clear: true } });
  assert.equal(tight.compact, true);
  for (const id of tight.inline) assert.ok(SHORTENED.has(id), id);
  // A fit is stable: feeding the full widths again gives the same answer.
  assert.deepEqual(toolbarFit({ available: 10, show: { clear: true } }), tight);
});

test('compact: collapsed items come back (Filter, Edit, Layout) while they fit beside the short buttons', () => {
  const show = { filter: true, clear: true };
  const C = DEFAULT_COMPACT_WIDTHS;
  const short = C.clear + C.fit + C.export + C.display + 3 * TOOLBAR_GAP;
  const fixedFull = W.clear + W.fit + W.export + W.display + 3 * TOOLBAR_GAP;
  // Too narrow for the full buttons, room for Filter beside the short ones.
  const room = Math.min(fixedFull - 1, short + W.filter + TOOLBAR_GAP);
  const r = toolbarFit({ available: room, show });
  assert.equal(r.compact, true);
  assert.deepEqual(r.inline, ['filter', 'clear', 'fit', 'export', 'display']);
  assert.deepEqual(r.inMenu, ['layout', 'edit']);
  // Measured compact widths are used.
  const wide = toolbarFit({ available: room, show, compactWidths: { clear: 10, fit: 10, export: 10, display: 10 } });
  assert.deepEqual(wide.inMenu, ['layout']);
  assert.deepEqual(wide.inline, ['edit', 'filter', 'clear', 'fit', 'export', 'display']);
});

test('an item dropped early comes back when a wider one went after it', () => {
  // Filter is wider than Edit: with Edit + the fixed buttons fitting but not Edit + Filter, Edit
  // stays inline and only Filter (and Layout) move.
  const fixed = ['fit', 'export', 'display'];
  const r = toolbarFit({ available: sum(['edit', ...fixed]), show: { filter: true } });
  assert.equal(r.compact, false);
  assert.deepEqual(r.inline, ['edit', ...fixed]);
  assert.deepEqual(r.inMenu, ['layout', 'filter']);
});

test('an unknown width (not measured yet) keeps everything inline', () => {
  assert.deepEqual(toolbarFit({ available: undefined }).inMenu, []);
});

test('the Filter badge: count, off, nothing', () => {
  assert.equal(filterBadge({ count: 2, enabled: true }), '· 2');
  assert.equal(filterBadge({ count: 2, enabled: false }), '· off');
  assert.equal(filterBadge({ count: 0, enabled: false }), '');
  assert.equal(filterBadge(), '');
  assert.equal(filterBadge({ count: -1 }), '');
});

test('the Filter button is a placeholder for now; help and title texts', () => {
  assert.equal(FILTER_ENABLED, false);
  assert.ok(GRAPH_HELP.length >= 3 && GRAPH_HELP.every((s) => s.endsWith('.')));
  assert.match(displayTitle('Auto · Fast'), /Auto · Fast/);
});
