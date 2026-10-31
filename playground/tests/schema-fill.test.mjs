// Node tests of the schema editor's vector-fill confirmation (js/schema/flow.js, book page
// "Vector Rules"): a planned fill above the threshold stops Apply with the dialog's warning
// until "Apply and lock"; "Cancel" applies nothing; below the threshold Apply goes on and keeps
// the warning as a plain note. The warning is the engine's text, only marked up.
// Run: node --test playground/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { applyMergePatch } from '../js/schema/draft.js';
import { SchemaFlow, fillHtml } from '../js/schema/flow.js';

const WARNING = 'Applying this schema fills vectors: 20,000 elements of doc.emb from doc.text with the model ' +
  'hash-ngram/1 (about 2 min).\nThe whole database is LOCKED while this runs: every write and every read waits ' +
  'until it ends.\nRun it at a quiet time. If it fails, nothing is written.';

function fillApi({ needsConfirmation }) {
  const calls = [];
  let current = { mode: 'none', vertices: { doc: { schema: { type: 'object', properties: { text: { type: 'string' } } } } } };
  const report = { mode: 'none', elements_checked: 1, violating_elements: 0, total_violations: 0, omitted_violations: 0, groups: [] };
  const fill = [{ label: 'doc', to: 'emb', elements: 20000, needs_confirmation: needsConfirmation }];
  const answer = () => ({ violations: report, diff: [{ kind: 'vector_rule_added', compatible: true }], compatible: true, fill, fill_warning: WARNING });
  return {
    calls,
    get: async () => { calls.push('get'); return current; },
    set: async (s) => { calls.push('set'); current = s; },
    patch: async (p) => { calls.push('patch'); current = applyMergePatch(current, p); return current; },
    validate: async () => { calls.push('validate'); return answer(); },
    validatePatch: async () => { calls.push('validatePatch'); return answer(); },
    infer: async () => { calls.push('infer'); return current; },
    diff: async () => { calls.push('diff'); return { changes: [], compatible: true }; },
  };
}

const addRule = (d) => {
  d.vertices.doc.schema.properties.emb = { type: 'array', format: 'vector', embedder: 'hash-ngram/1', from: 'text' };
};

test('fill: above the threshold Apply asks; Cancel applies nothing; Apply and lock applies', async () => {
  const api = fillApi({ needsConfirmation: true });
  const flow = new SchemaFlow(api);
  await flow.load();
  flow.ed.edit(addRule);
  assert.equal(await flow.apply(), false);
  assert.equal(flow.pendingFill.warning, WARNING);
  assert.ok(!api.calls.includes('patch'));
  flow.cancelFill();
  assert.equal(flow.pendingFill, null);
  assert.ok(!api.calls.includes('patch'));
  assert.equal(await flow.apply(), false, 'asks again');
  assert.equal(await flow.applyAndLock(), true);
  assert.ok(api.calls.includes('patch'));
  assert.equal(flow.pendingFill, null);
  assert.equal(flow.ed.stored.vertices.doc.schema.properties.emb.from, 'text');
});

test('fill: an edit after the question needs a new one', async () => {
  const api = fillApi({ needsConfirmation: true });
  const flow = new SchemaFlow(api);
  await flow.load();
  flow.ed.edit(addRule);
  assert.equal(await flow.apply(), false);
  flow.ed.edit((d) => { d.meta = { v: 2 }; });
  assert.equal(await flow.applyAndLock(), false);
  assert.ok(!api.calls.includes('patch'));
});

test('fill: below the threshold Apply goes on with a plain note', async () => {
  const api = fillApi({ needsConfirmation: false });
  const flow = new SchemaFlow(api);
  await flow.load();
  flow.ed.edit(addRule);
  assert.equal(await flow.apply(), true);
  assert.equal(flow.pendingFill, null);
  assert.equal(flow.fillNote, WARNING);
});

test('fill: the warning is marked up, never rewritten', () => {
  const html = fillHtml(WARNING);
  assert.ok(html.includes('<b>20,000 elements</b>'), html);
  assert.ok(html.includes('(<b>about 2 min</b>)'), html);
  assert.ok(html.includes('<br>The whole database is LOCKED'), html);
  assert.equal(fillHtml('<x> 1 element (time depends on the model)'),
    '&lt;x&gt; <b>1 element</b> (<b>time depends on the model</b>)');
});
