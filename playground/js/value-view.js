// How a property value is shown in the element views (the graph drawing's inspector, the result
// table's cells): short values as they are, long ones COLLAPSED to a summary with an expand
// button (components/value-text.js). Pure: tested under Node (tests/value-view.test.mjs).
//
// - A vector is collapsed to `vector[384] [0.12, -0.03, 0.5, …]`. The inspector receives a
//   vector as a plain list of numbers (the session's element JSON), so every list of at least
//   `VALUE_VIEW_TUNING.vectorMin` numbers is shown this way (also inside a one-item list, as
//   `value_map()` returns it: `[vector[384] [..]]`).
// - A text (a string, or the JSON of a list or a map) longer than the setting
//   `valueMaxChars` (default 200 characters) or with more than `VALUE_VIEW_TUNING.maxLines` lines
//   is collapsed to its start and "…".
// - `valueMaxChars: 0` turns collapsing off: every value is shown in full.
// The expanded text, and what a copy button copies, is always the full value.

import { formatValue } from './util.js';

/** Fixed tuning of the collapsed view (the character limit is the setting `valueMaxChars`). */
export const VALUE_VIEW_TUNING = Object.freeze({
  /** Lists of at least this many numbers are shown as a vector. */
  vectorMin: 16,
  /** Components a collapsed vector shows. */
  vectorPreview: 4,
  /** Significant digits of a component in the collapsed vector. */
  vectorDigits: 3,
  /** Texts with more lines than this are collapsed (to this many lines). */
  maxLines: 3,
});

/** The setting `valueMaxChars`: its default and its range (0 = no collapsing). */
export const VALUE_MAX_CHARS = Object.freeze({ default: 200, min: 20, max: 100000 });

/**
 * Why `value` is not a valid `valueMaxChars` (null: it is): a whole number, 0 (show every value
 * in full) or between `VALUE_MAX_CHARS.min` and `.max`.
 * @returns {string|null}
 */
export function maxCharsError(value) {
  const n = typeof value === 'number' || (typeof value === 'string' && value.trim() !== '') ? Number(value) : NaN;
  if (!Number.isInteger(n) || n < 0) return 'Collapse values above: a whole number of characters (0 shows every value in full).';
  if (n !== 0 && (n < VALUE_MAX_CHARS.min || n > VALUE_MAX_CHARS.max)) {
    return `Collapse values above: 0 (off) or ${VALUE_MAX_CHARS.min} to ${VALUE_MAX_CHARS.max.toLocaleString('en-US')} characters.`;
  }
  return null;
}

/** `value` as a valid `valueMaxChars` (an invalid one, e.g. from old storage, is the default). */
export function normalizeMaxChars(value) {
  return maxCharsError(value) === null ? Number(value) : VALUE_MAX_CHARS.default;
}

/** Whether `v` is a list of at least `vectorMin` finite numbers (how a vector arrives). */
export function isNumericList(v) {
  if (!Array.isArray(v) || v.length < VALUE_VIEW_TUNING.vectorMin) return false;
  for (const x of v) if (typeof x !== 'number' || !Number.isFinite(x)) return false;
  return true;
}

/** A vector component, short: `-0.169` of `-0.16903085`, `0` of `0`. */
export function shortNumber(x) {
  if (Number.isInteger(x)) return String(x);
  return String(Number(x.toPrecision(VALUE_VIEW_TUNING.vectorDigits)));
}

/** `vector[384] [0.12, -0.03, 0.5, 1, …]` of a numeric list. */
export function vectorText(list) {
  const shown = list.slice(0, VALUE_VIEW_TUNING.vectorPreview).map(shortNumber);
  const more = list.length > shown.length ? ', …' : '';
  return `vector[${list.length}] [${shown.join(', ')}${more}]`;
}

/** The full text of a value: a numeric list with ", " between its components, else `formatValue`. */
export function fullText(v) {
  if (Array.isArray(v) && v.every((x) => typeof x === 'number')) return `[${v.join(', ')}]`;
  return formatValue(v);
}

/** `text` cut to `maxChars` characters and `maxLines` lines, with "…" (unchanged when it fits). */
export function cutText(text, maxChars, maxLines = VALUE_VIEW_TUNING.maxLines) {
  let end = Math.min(text.length, maxChars);
  let line = 0;
  for (let i = 0; i < end; i++) {
    if (text[i] === '\n' && ++line === maxLines) {
      end = i;
      break;
    }
  }
  if (end >= text.length) return text;
  // Do not split a surrogate pair.
  const code = text.charCodeAt(end - 1);
  if (code >= 0xd800 && code <= 0xdbff) end--;
  return `${text.slice(0, end).trimEnd()}…`;
}

/** Number of lines of `text`. */
function lineCount(text) {
  let n = 1;
  for (let i = 0; i < text.length; i++) if (text[i] === '\n') n++;
  return n;
}

/**
 * How to show `value`: `{full, short, kind}`. `full` is the whole value as text; `short` is the
 * collapsed text, or null when the value is shown in full (it is short, or collapsing is off);
 * `kind` is 'vector' (a numeric list) or 'text'.
 * @param {*} value
 * @param {{maxChars?:number}} [options]  `maxChars`: the setting `valueMaxChars` (0 = off).
 * @returns {{full:string, short:string|null, kind:'vector'|'text'}}
 */
export function valueView(value, { maxChars = VALUE_MAX_CHARS.default } = {}) {
  const limit = normalizeMaxChars(maxChars);
  const full = fullText(value);
  if (isNumericList(value)) return { full, short: limit === 0 ? null : vectorText(value), kind: 'vector' };
  // A vector inside a one-item list (a `value_map()` row): `[vector[384] [..]]`.
  if (Array.isArray(value) && value.length === 1 && isNumericList(value[0])) {
    return { full: `[${fullText(value[0])}]`, short: limit === 0 ? null : `[${vectorText(value[0])}]`, kind: 'vector' };
  }
  if (limit === 0 || (full.length <= limit && lineCount(full) <= VALUE_VIEW_TUNING.maxLines)) return { full, short: null, kind: 'text' };
  return { full, short: cutText(full, limit), kind: 'text' };
}
