// The text visualizer of TraversalMetrics for the Profile panel's Text tab, rendered client-side
// from the pure metrics data in the layout of the CLI's "Traversal Metrics" table (the engine's
// own rendering is the Raw tab of a `profile()` result). Optional sections are rendered only when
// present: `path_recording` as the ` [path: ..]` suffix, `loops` as an indented line.

import { formatDuration, formatBytes } from './util.js';

function anyMemory(list) {
  return list.some((m) => m.memory || anyMemory(m.nested || []));
}

/**
 * @param {object} p           TraversalMetrics.
 * @param {object} [error]     The execution's error; its `step_id` / `step_path` mark the failure.
 */
export function profileText(p, error = null) {
  if (!p) return '';
  const failedId = error && error.step_id;
  const onPath = new Set(error && Array.isArray(error.step_path) ? error.step_path.map((s) => s.id) : []);
  const mem = anyMemory(p.metrics);
  const rows = [];
  const walk = (list, depth) => list.forEach((m) => { rows.push([m, depth]); walk(m.nested || [], depth + 1); });
  walk(p.metrics, 0);
  const stepText = (m, depth) => {
    const mark = m.id === failedId ? '  <-- failed' : onPath.has(m.id) ? '  <' : '';
    return '  '.repeat(depth) + m.name + (m.path_recording ? ` [path: ${m.path_recording}]` : '') + mark;
  };
  // As wide as the longest step text (between 56 and 160), like the engine's table.
  const width = Math.min(160, rows.reduce((w, [m, depth]) => Math.max(w, stepText(m, depth).length), 56));
  const head = ['Step'.padEnd(width), 'Call'.padStart(8), 'In'.padStart(8), 'Out'.padStart(8), 'Time'.padStart(11)];
  if (mem) head.push('Mem'.padStart(10));
  head.push('% Dur'.padStart(9));
  const line = head.join('');
  const sep = '='.repeat(line.length);
  const out = ['Traversal Metrics', line, sep];
  for (const [m, depth] of rows) {
    const name = stepText(m, depth);
    const cells = [name.length > width ? name.slice(0, width - 1) + '…' : name.padEnd(width), String(m.calls).padStart(8),
      String(m.count_in).padStart(8), String(m.counts.traverser_count).padStart(8), formatDuration(m.duration_ns).padStart(11)];
    if (mem) cells.push((m.memory ? formatBytes(m.memory.bytes_materialized) : '').padStart(10));
    cells.push(Number(m.percent_duration).toFixed(2).padStart(9));
    out.push(cells.join(''));
    if (m.loops) {
      out.push(`${'  '.repeat(depth + 1)}loops: total ${m.loops.total_loops}, max depth ${m.loops.max_depth}, avg ${Number(m.loops.avg_loops_per_traverser).toFixed(2)} per traverser`);
    }
  }
  out.push(`${'TOTAL: duration:'.padStart(width + 24)}${formatDuration(p.duration_ns).padStart(11)}`);
  out.push(sep);
  if (p.optimizer_rules_applied && p.optimizer_rules_applied.length) {
    out.push(`Optimizer rules applied: ${p.optimizer_rules_applied.join(', ')}`);
  }
  if (error && error.message) out.push(`Failed${failedId !== undefined ? ` at step ${failedId}` : ''}: ${error.message}`);
  return out.join('\n');
}
