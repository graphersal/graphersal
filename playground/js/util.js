// Small helpers shared by the UI components.

const PREFIX = 'graphersal.playground.';

/**
 * localStorage access that never throws: storage can be disabled, full, or unavailable
 * (private windows, blocked site data). Values are JSON.
 */
export const storage = {
  get(key, fallback) {
    try {
      const raw = window.localStorage.getItem(PREFIX + key);
      return raw === null ? fallback : JSON.parse(raw);
    } catch (e) {
      return fallback;
    }
  },
  set(key, value) {
    try {
      window.localStorage.setItem(PREFIX + key, JSON.stringify(value));
    } catch (e) {
      /* storage unavailable: the setting lasts for this page view only */
    }
  },
  remove(key) {
    try {
      window.localStorage.removeItem(PREFIX + key);
    } catch (e) {
      /* ignore */
    }
  },
};

export function debounce(fn, ms) {
  let timer = null;
  return (...args) => {
    clearTimeout(timer);
    timer = setTimeout(() => fn(...args), ms);
  };
}

export const isMac = typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
export const modKey = isMac ? '⌘' : 'Ctrl';

/** Readable rendering of an arbitrary cell value. */
export function formatValue(v) {
  if (v === null || v === undefined) return 'null';
  if (isVectorCell(v)) return `${vectorSummary(v)} ${vectorPreview(v)}`;
  if (isNonFiniteCell(v)) return v['@value'];
  if (typeof v === 'object') return jsonText(v);
  return String(v);
}

/** The payloads of a non-finite float: JSON has no number for them. */
const NON_FINITE = Object.freeze({ NaN: NaN, Infinity: Infinity, '-Infinity': -Infinity });

/**
 * Whether a value is a non-finite float: the engine sends `NaN`, `Infinity` and `-Infinity` as
 * GraphSON's typed double `{"@type": "g:Double", "@value": "NaN"}` (crates/graphersal-session
 * `display.rs` `non_finite_marker`), so it is not mistaken for the string "NaN".
 */
export function isNonFiniteCell(v) {
  return v !== null && typeof v === 'object' && !Array.isArray(v) && v['@type'] === 'g:Double'
    && Object.keys(v).length === 2 && Object.prototype.hasOwnProperty.call(NON_FINITE, v['@value']);
}

/** The number of a non-finite float cell (`NaN`, `Infinity`, `-Infinity`). */
export function nonFiniteNumber(v) {
  return NON_FINITE[v['@value']];
}

/**
 * Compact JSON of a nested value, with every non-finite float shown bare as `NaN`, `Infinity` or
 * `-Infinity` (`[1.5, NaN]`), the way the result table shows a number.
 */
export function jsonText(v) {
  const marks = [];
  const text = JSON.stringify(v, (key, item) => {
    if (!isNonFiniteCell(item)) return item;
    marks.push(item['@value']);
    return `\u0000${marks.length - 1}\u0000`;
  });
  if (!marks.length) return text;
  return text.replace(/"\\u0000(\d+)\\u0000"/g, (_, i) => marks[Number(i)]);
}

/**
 * Whether a result cell is a vector: the engine puts `{"@type": "graphersal:Vector", element,
 * dimension, preview}` into a table instead of its values (crates/graphersal-session
 * `display.rs` `table_cell`; the JSON view keeps the values).
 */
export function isVectorCell(v) {
  return v !== null && typeof v === 'object' && !Array.isArray(v) && v['@type'] === 'graphersal:Vector';
}

/** `vector[768 f32]` of a vector cell. */
export function vectorSummary(v) {
  return `vector[${v.dimension} ${v.element}]`;
}

/** The first values of a vector cell: `[0.1, -0.2, …]`. */
export function vectorPreview(v) {
  const shown = Array.isArray(v.preview) ? v.preview : [];
  const more = v.dimension > shown.length ? ', …' : '';
  return `[${shown.join(', ')}${more}]`;
}

export function formatMs(ms) {
  if (ms === undefined || ms === null) return '';
  if (ms < 1) return `${(ms * 1000).toFixed(0)} µs`;
  if (ms < 1000) return `${ms.toFixed(1)} ms`;
  return `${(ms / 1000).toFixed(2)} s`;
}

/** Reads a CSS custom property of the document (for canvas-based libraries). */
export function cssVar(name) {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

/** Nanoseconds in the units of the engine's profile table. */
export function formatDuration(ns) {
  if (ns === undefined || ns === null) return '';
  if (ns < 1000) return `${Math.round(ns)}ns`;
  if (ns < 1e6) return `${(ns / 1e3).toFixed(3)}µs`;
  if (ns < 1e9) return `${(ns / 1e6).toFixed(3)}ms`;
  return `${(ns / 1e9).toFixed(3)}s`;
}

/**
 * A graph name split for display: `{dir, base}`, where `dir` is everything up to and including
 * the last `/` or `\` (empty for a name that is not a path) and `base` the rest. The header
 * shortens `dir` from the left so the file or directory name itself stays visible.
 * @param {string} name
 * @returns {{dir: string, base: string}}
 */
export function splitGraphName(name) {
  const text = String(name ?? '');
  const trimmed = text.replace(/[\\/]+$/, '');
  const cut = Math.max(trimmed.lastIndexOf('/'), trimmed.lastIndexOf('\\'));
  if (cut < 0) return { dir: '', base: text };
  return { dir: text.slice(0, cut + 1), base: text.slice(cut + 1) };
}

export function formatBytes(b) {
  if (b === undefined || b === null) return '';
  const abs = Math.abs(b);
  if (abs < 1024) return `${b}B`;
  if (abs < 1024 * 1024) return `${(b / 1024).toFixed(2)}KB`;
  return `${(b / (1024 * 1024)).toFixed(2)}MB`;
}

/** One Markdown table cell: compact JSON for nested values, `|` escaped, newlines as <br>. */
function markdownCell(v) {
  let text;
  if (v === undefined) text = '';
  else if (v === null) text = 'null';
  else if (isNonFiniteCell(v)) text = v['@value'];
  else if (typeof v === 'object') text = jsonText(v);
  else text = String(v);
  return text.replace(/\\/g, '\\\\').replace(/\|/g, '\\|').replace(/\r?\n/g, '<br>');
}

/**
 * A GitHub-flavoured Markdown table of {columns, rows}. `note` (e.g. a truncation notice) is
 * appended as an italic line after the table.
 */
export function toMarkdownTable(table, note) {
  if (!table || !table.columns || !table.columns.length) return note ? `_${note}_\n` : '';
  const line = (cells) => `| ${cells.map(markdownCell).join(' | ')} |`;
  const out = [line(table.columns), `| ${table.columns.map(() => '---').join(' | ')} |`];
  for (const row of table.rows) out.push(line(row));
  if (note) out.push('', `_${markdownCell(note)}_`);
  return out.join('\n') + '\n';
}

/**
 * Copies text to the clipboard: `navigator.clipboard.writeText`, else (an insecure origin, an
 * older browser) a hidden textarea and `execCommand('copy')`. Resolves to whether it worked.
 */
export async function copyText(text) {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch (e) {
    /* try the fallback */
  }
  try {
    const area = document.createElement('textarea');
    area.value = text;
    area.setAttribute('readonly', '');
    area.style.position = 'fixed';
    area.style.top = '0';
    area.style.opacity = '0';
    document.body.appendChild(area);
    area.select();
    const ok = document.execCommand('copy');
    area.remove();
    return !!ok;
  } catch (e) {
    return false;
  }
}
