// Node tests of the Catalog dropdown and Compression manager models (js/compression.js): the
// dropdown's items and counts, the Store menu's catalog line, the rules table, the form's draft
// and spec, and the label suggestions.
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  CATALOG_KINDS, DEFAULT_MIN_BYTES, catalogItems, catalogTotal, catalogCountsText, ratioText,
  ruleRow, totals, newDraft, draftFromRule, draftToSpec, labelSuggestions,
} from '../js/compression.js';

const RULE = {
  name: 'customer_cv',
  element: 'vertex',
  label: 'customer',
  path: '$.details.career.cv',
  codec: 'lz4',
  minBytes: 512,
  dictionary: true,
  stats: { compressedValues: 3, plainBytes: 3000, storedBytes: 1200, dictionaryBytes: 2048 },
};

test('catalog items: one per kind with its count, from the graph info', () => {
  assert.deepEqual(CATALOG_KINDS.map((k) => k.id), ['queries', 'compression']);
  const items = catalogItems({ catalog: { query: 2, compression: 1, 'kind 77': 4 } });
  assert.deepEqual(items.map((i) => [i.id, i.count]), [['queries', 2], ['compression', 1]]);
  assert.equal(catalogTotal({ catalog: { query: 2, compression: 1, 'kind 77': 4 } }), 7);
  // An older info without `catalog`.
  assert.deepEqual(catalogItems({ savedQueries: 5, compressionRules: 2 }).map((i) => i.count), [5, 2]);
  assert.deepEqual(catalogItems(null).map((i) => i.count), [0, 0]);
});

test('the Store menu line names every kind', () => {
  assert.equal(catalogCountsText({ query: 2, compression: 1 }, 3), '2 saved queries, 1 compression rule');
  assert.equal(catalogCountsText({ query: 1, 'kind 77': 2 }, 3), '1 saved query, 2 kind 77 definitions');
  assert.equal(catalogCountsText({}, 0), 'empty');
  assert.equal(catalogCountsText(undefined, 4), '4 definitions');
});

test('rules table: sizes, ratio, saving and totals', () => {
  const row = ruleRow(RULE);
  assert.equal(row.target, 'vertex customer');
  assert.equal(row.path, '$.details.career.cv');
  assert.equal(row.threshold, '≥ 512 B');
  assert.equal(row.dictionary, '2 KiB');
  assert.equal(row.values, 3);
  assert.equal(row.sizes, '2.9 KiB → 1.2 KiB');
  assert.equal(row.ratio, '40 %');
  assert.equal(row.saved, '1.8 KiB');
  const empty = ruleRow({ ...RULE, element: 'edge', dictionary: true, stats: { compressedValues: 0, plainBytes: 0, storedBytes: 0, dictionaryBytes: 0 } });
  assert.equal(empty.target, 'edge customer');
  assert.equal(empty.dictionary, 'yes (none yet)');
  assert.equal(empty.sizes, '—');
  assert.equal(empty.ratio, '—');
  assert.equal(ruleRow({ ...RULE, dictionary: false }).dictionary, 'no');
  assert.equal(ratioText(null), '—');
  assert.deepEqual(totals([RULE, RULE]), { compressedValues: 6, plainBytes: 6000, storedBytes: 2400, dictionaryBytes: 4096 });
});

test('form: a new draft, an edit draft and the spec', () => {
  const draft = newDraft();
  assert.equal(draft.minBytesText, String(DEFAULT_MIN_BYTES));
  assert.equal(draft.dictionary, true);
  const edit = draftFromRule(RULE);
  assert.equal(edit.path, 'details.career.cv');
  assert.equal(edit.replaces, 'customer_cv');
  assert.equal(draftFromRule({ ...RULE, path: "$['a b'].c" }).path, "$['a b'].c");
  const { spec } = draftToSpec({ ...edit, name: ' renamed ', minBytesText: '1024', dictionary: false });
  assert.deepEqual(spec, {
    name: 'renamed', element: 'vertex', label: 'customer', path: 'details.career.cv', minBytes: 1024,
    dictionary: false, replaces: 'customer_cv',
  });
  assert.equal(draftToSpec({ ...draft, name: 'a', label: 'l', path: 'p', minBytesText: '' }).spec.minBytes, DEFAULT_MIN_BYTES);
});

test('form: local field errors before the engine is asked', () => {
  const { errors } = draftToSpec({ ...newDraft(), minBytesText: '0' });
  assert.deepEqual(errors.map((e) => e.field), ['name', 'label', 'path', 'minBytes']);
  for (const bad of ['-5', '1.5', 'many', String(16 * 1024 * 1024 + 1)]) {
    const res = draftToSpec({ ...newDraft(), name: 'a', label: 'l', path: 'p', minBytesText: bad });
    assert.deepEqual(res.errors.map((e) => e.field), ['minBytes'], bad);
  }
});

test('label suggestions come from the statistics of the chosen element kind', () => {
  const stats = {
    statistics: {
      vertex_labels: [{ label: 'person', count: 4 }, { label: 'customer', count: 2 }, { label: 'person', count: 1 }],
      edge_labels: [{ label: 'knows', count: 2 }],
    },
  };
  assert.deepEqual(labelSuggestions(stats, 'vertex'), ['customer', 'person']);
  assert.deepEqual(labelSuggestions(stats, 'edge'), ['knows']);
  assert.deepEqual(labelSuggestions(null, 'vertex'), []);
});

// ------------------------------------------------------------------ the estimate (issue #12)

import {
  canEstimate, draftToEstimateSpec, rateText, candidateName, estimateRows, estimateSummary,
  histogramRows, applyCandidate,
} from '../js/compression.js';

const ESTIMATE = {
  element: 'vertex',
  label: 'customer',
  path: '$.cv',
  elements: 3,
  valuesFound: 3,
  stringValues: 2,
  stringBytes: 6000,
  sampled: false,
  measuredValues: 2,
  histogram: [
    { minBytes: 0, maxBytes: 64, values: 0, bytes: 0 },
    { minBytes: 1024, maxBytes: 4096, values: 2, bytes: 6000 },
    { minBytes: 16384, maxBytes: null, values: 0, bytes: 0 },
  ],
  candidates: [
    { codec: 'lz4', minBytes: 64, dictionary: false, compressedValues: 2, plainBytes: 6000, storedBytes: 3000, dictionaryBytes: 0, savedBytes: 3000, ratio: 0.5, current: false, encodeBytesPerSec: 3e8, decodeBytesPerSec: 2 * 1024 * 1024 },
    { codec: 'lz4', minBytes: 64, dictionary: true, compressedValues: 2, plainBytes: 6000, storedBytes: 1000, dictionaryBytes: 2000, savedBytes: 3000, ratio: 0.17, current: true, encodeBytesPerSec: null, decodeBytesPerSec: null },
    { codec: 'lz4', minBytes: 4096, dictionary: true, compressedValues: 0, plainBytes: 0, storedBytes: 0, dictionaryBytes: 4000, savedBytes: -4000, ratio: null, current: false },
  ],
  best: 0,
  current: { name: 'cv', codec: 'lz4', minBytes: 64, dictionary: true, stats: { compressedValues: 2, plainBytes: 6000, storedBytes: 1000, dictionaryBytes: 2000 } },
  warnings: [{ code: 'not_strings', message: '1 value(s) at $.cv are not strings; a rule compresses strings only' }],
  elapsedMs: 1.5,
  graphChanged: false,
};

test('estimate: the form asks once label and path are filled, without its threshold', () => {
  assert.equal(canEstimate(newDraft()), false);
  assert.equal(canEstimate({ ...newDraft(), label: 'customer', path: ' ' }), false);
  assert.equal(canEstimate({ ...newDraft(), label: 'customer', path: 'cv' }), true);
  assert.deepEqual(draftToEstimateSpec({ ...newDraft(), element: 'edge', label: ' bought ', path: 'notes', minBytesText: '9' }), {
    spec: { element: 'edge', label: 'bought', path: 'notes' },
  });
  assert.deepEqual(draftToEstimateSpec(newDraft()).errors.map((e) => e.field), ['label', 'path']);
});

test('estimate: rows with saving bars, the best and the current candidate', () => {
  const rows = estimateRows(ESTIMATE);
  assert.equal(rows.length, 3);
  assert.equal(rows[0].name, 'lz4');
  assert.equal(rows[1].name, 'lz4 + dictionary');
  assert.equal(candidateName({ dictionary: true }), 'lz4 + dictionary');
  assert.equal(rows[0].threshold, '≥ 64 B');
  assert.equal(rows[0].sizes, '5.9 KiB → 2.9 KiB');
  assert.equal(rows[0].ratio, '50 %');
  assert.equal(rows[0].saved, '2.9 KiB');
  assert.equal(rows[0].share, 1);
  assert.equal(rows[0].best, true);
  assert.equal(rows[1].best, false);
  assert.equal(rows[1].current, true);
  assert.equal(rows[1].dictionary, '2 KiB');
  assert.equal(rows[0].dictionary, '—');
  assert.equal(rows[0].reads, '2 MiB/s');
  assert.equal(rows[1].reads, '—');
  assert.equal(rows[2].saved, 'costs 3.9 KiB');
  assert.equal(rows[2].share, 0);
  assert.equal(rows[2].sizes, '—');
  assert.equal(rateText(0), '—');
  assert.equal(rateText(undefined), '—');
  assert.deepEqual(estimateRows(null), []);
  // Nothing saves: no bar at all.
  const none = estimateRows({ ...ESTIMATE, best: null, candidates: [ESTIMATE.candidates[2]] });
  assert.equal(none[0].share, 0);
  assert.equal(none[0].best, false);
});

test('estimate: the summary says what was found, that nothing changed, and the warnings', () => {
  const s = estimateSummary(ESTIMATE);
  assert.equal(s.found, '3 vertices labelled "customer", 3 values at $.cv, 2 strings (5.9 KiB).');
  assert.match(estimateSummary({ ...ESTIMATE, element: 'edge', elements: 1 }).found, /^1 edge labelled/);
  assert.equal(s.notes[0], 'Estimated on the current data; the graph was not changed.');
  assert.match(s.notes[1], /Today the rule "cv" \(≥ 64 B, dictionary on\) holds 2 values/);
  assert.deepEqual(s.warnings, ['1 value(s) at $.cv are not strings; a rule compresses strings only']);
  const sampled = estimateSummary({ ...ESTIMATE, sampled: true, measuredValues: 1000, current: null, warnings: [] });
  assert.match(sampled.notes[1], /sample of 1000 strings/);
  assert.equal(sampled.notes.length, 2);
  assert.deepEqual(estimateSummary(null), { found: '', notes: [], warnings: [] });
  assert.deepEqual(histogramRows(ESTIMATE), [{ range: '1 KiB – 4 KiB', values: 2, bytes: '5.9 KiB' }]);
  assert.deepEqual(histogramRows({ histogram: [{ minBytes: 16384, maxBytes: null, values: 1, bytes: 20000 }] }), [
    { range: '≥ 16 KiB', values: 1, bytes: '19.5 KiB' },
  ]);
});

test('estimate: Use copies codec, threshold and dictionary into the form and the spec', () => {
  const draft = { ...newDraft(), name: 'cv', label: 'customer', path: 'cv' };
  const used = applyCandidate(draft, ESTIMATE.candidates[1]);
  assert.equal(used.minBytesText, '64');
  assert.equal(used.dictionary, true);
  assert.equal(used.codec, 'lz4');
  assert.equal(used.label, 'customer');
  assert.deepEqual(draftToSpec(used).spec, {
    name: 'cv', element: 'vertex', label: 'customer', path: 'cv', minBytes: 64, dictionary: true, codec: 'lz4',
  });
  assert.equal(applyCandidate(draft, { minBytes: 512, dictionary: false }).dictionary, false);
});

test('dedup: the form switches codec defaults, the spec has no dictionary, rows show distinct values', async () => {
  const m = await import('../js/compression.js');
  const draft = { ...m.newDraft(), name: 'p', label: 'symbol', path: 'path' };
  const dedup = m.withCodec(draft, 'dedup');
  assert.equal(dedup.codec, 'dedup');
  assert.equal(dedup.minBytesText, String(m.DEFAULT_DEDUP_MIN_BYTES));
  assert.equal(dedup.dictionary, false);
  assert.deepEqual(m.draftToSpec({ ...dedup, dictionary: true }).spec, {
    name: 'p', element: 'vertex', label: 'symbol', path: 'path', minBytes: 0, dictionary: false, codec: 'dedup',
  });
  assert.equal(m.draftToSpec({ ...dedup, minBytesText: '' }).spec.minBytes, 0);
  // Back to lz4: its defaults return; a threshold the user typed stays.
  const back = m.withCodec(dedup, 'lz4');
  assert.equal(back.minBytesText, String(m.DEFAULT_MIN_BYTES));
  assert.equal(back.dictionary, true);
  assert.equal(m.withCodec({ ...draft, minBytesText: '40' }, 'dedup').minBytesText, '40');
  // A stored dedup rule: its draft keeps the codec, its row shows distinct values.
  const rule = {
    name: 'p', element: 'vertex', label: 'symbol', path: '$.path', codec: 'dedup', minBytes: 25, dictionary: false,
    stats: { compressedValues: 12, plainBytes: 600, storedBytes: 240, dictionaryBytes: 0, distinctValues: 3 },
  };
  assert.equal(m.draftFromRule(rule).codec, 'dedup');
  assert.equal(m.draftFromRule(rule).dictionary, false);
  const row = m.ruleRow(rule);
  assert.equal(row.codec, 'dedup');
  assert.equal(row.distinct, '3');
  assert.equal(row.dictionary, '—');
  assert.equal(m.ruleRow({ ...rule, codec: 'lz4' }).distinct, '—');
  // Estimate rows: a dedup candidate reads as plain and has a distinct count.
  const rows = m.estimateRows({
    best: 0,
    candidates: [{ codec: 'dedup', minBytes: 25, dictionary: false, compressedValues: 12, plainBytes: 600, storedBytes: 240, dictionaryBytes: 0, distinctValues: 3, savedBytes: 360 }],
  });
  assert.equal(rows[0].name, 'dedup');
  assert.equal(rows[0].distinct, '3');
  assert.equal(rows[0].reads, 'as plain');
  assert.deepEqual([...m.ESTIMATE_CODECS], ['lz4', 'dedup']);
  const summary = m.estimateSummary({ element: 'vertex', label: 'symbol', path: '$.path', current: { name: 'p', codec: 'dedup', minBytes: 25, stats: rule.stats } });
  assert.match(summary.notes[1], /Today the dedup rule "p" \(≥ 25 B\) shares 12 values \(3 distinct\)/);
});

test('a rule of a newer version: listed, not applied, with its reason', () => {
  const rule = {
    name: 'body', element: 'vertex', label: 'doc', path: '$.body', codec: 'zstd', minBytes: 256,
    dictionary: true, served: false,
    reason: "codec 'zstd' is not known to this version (a newer Graphersal wrote it)", stats: null,
  };
  const row = ruleRow(rule);
  assert.equal(row.served, false);
  assert.equal(row.codec, 'zstd');
  assert.match(row.reason, /zstd/u);
  assert.equal(row.values, 0);
  assert.equal(ruleRow(RULE).served, true);
  assert.equal(ruleRow({ ...RULE, element: 'property' }).target, 'property customer');
});

test('estimate view: candidates that would hold nothing are left out and named in a note', async () => {
  const m = await import('../js/compression.js');
  const c = (codec, minBytes, dictionary, compressedValues, savedBytes) => ({
    codec, minBytes, dictionary, compressedValues, plainBytes: compressedValues * 40,
    storedBytes: compressedValues * 4, dictionaryBytes: dictionary ? 65536 : 0, distinctValues: 3, savedBytes,
  });
  const estimate = {
    best: 4,
    candidates: [c('lz4', 64, false, 0, 0), c('lz4', 64, true, 0, -65536), c('lz4', 128, false, 0, 0),
      c('dedup', 64, false, 0, 0), c('dedup', 25, false, 12, 360)],
  };
  const view = m.estimateView(estimate);
  assert.deepEqual(view.rows.map((r) => r.name + ' ' + r.threshold), ['dedup ≥ 25 B']);
  assert.equal(view.empty, 'Not shown: lz4 from ≥ 64 B, dedup from ≥ 64 B (with and without a dictionary): '
    + 'no string at the path is that long, so they would hold nothing.');
  // Nothing left out: no note.
  assert.equal(m.estimateView({ best: 0, candidates: [c('lz4', 64, false, 5, 100)] }).empty, '');
});
