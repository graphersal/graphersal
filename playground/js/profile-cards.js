// Profile cards (EXPERIMENTAL, like the profile API): how `api.card(label, row, options)`
// (js/profile-api.js) shows one row by the profile's card template for `label`
// (js/ui-profiles.js `cards: {<label>: template}`). The view is components/profile-card.js;
// this file is the pure model (no DOM, no Vue; tests/profile-cards.test.mjs).
//
// A ROW is what a saved query returns: an element (`{id, label, properties: {...}}`), a map
// (`value_map()`, `project(..)`, a JSON object) or a single value.
//
// A FIELD PATH names a value of the row: `name`, `author.name`, `tags.0` (dot-separated keys; a
// number indexes a list). On an element the path is looked up in its `properties` first, then on
// the element itself (`id`, `label`). A one-item list (how `value_map()` returns every property)
// is shown as its item. A FIELD REF is a path or a list of paths: the first one present wins
// (`["title", "slug"]`). A value is MISSING when it is absent, null, "" or an empty list; a
// missing value renders nothing (no "undefined", no empty line).
//
// The TEMPLATE (every key optional; an unknown key is an error, so a typo is never ignored):
//   title     field ref: the card's heading. Without it (or when it is missing) the heading is
//             the label, muted, so every card has one.
//   subtitle  a part or a list of parts, one line joined with " · " (missing parts left out).
//   badges    a list of parts shown as small pills after the title. A badge shows when its
//             `when` (default: its own field) is present and truthy (not false, not 0); its text
//             is `text`, else the field's value (a `true` value shows the field's name).
//   body      field ref of a Markdown text: the full card renders it with the playground's safe
//             Markdown renderer (never HTML); the compact card shows its first `excerpt`
//             characters as plain text.
//   excerpt   the characters of the compact card's body excerpt (0..2000, default 160; 0: none).
//   fields    a list of parts shown as a key/value list in the full card (long values collapse
//             through the setting `valueMaxChars`, js/value-view.js). Left out: every field of
//             the row that the title, subtitle, badges and body do not show; [] shows none.
//   actions   a list of default buttons {id, text, kind?, when?, unless?, title?}; buttons show
//             only when the module passes `onAction` (an option of `api.card`), which may also
//             replace them with its own `actions`.
// A PART is a field path, or an object:
//   {field: ref, title?, text?, prefix?, format?, tone?, when?, unless?}
//     title   the key column's text in `fields` (default: the path's last key)
//     text    a fixed text instead of the value (badges, subtitle)
//     prefix  text before the value ("age " + 29)
//     format  'text' (default: lists joined with ", ", maps as JSON), 'date' (YYYY-MM-DD),
//             'datetime' (YYYY-MM-DD HH:MM, UTC), 'count' (a list's length), 'markdown' (fields
//             only: rendered by the safe renderer). A date is an ISO text or a number of
//             milliseconds since 1970 (the template says so; values are never sniffed).
//     tone    badges: 'neutral' (default), 'accent', 'ok', 'warn', 'danger', or a map from the
//             value to a tone ({"correction": "warn"}); other values neutral.
//     when    field ref: show the part only when it is present and truthy.
//     unless  field ref: show the part only when it is missing or falsy.
//
// Example (graphersal-memory's memory card):
//   memory: {title: ["title", "slug"], subtitle: ["kind", "scope", "source",
//            {field: "updated", format: "date"}],
//            badges: [{field: "pinned", text: "pinned", tone: "accent"},
//                     {field: "superseded_by", text: "replaced", tone: "warn"},
//                     {field: "conflicts", text: "conflict", tone: "danger"}],
//            body: "body", fields: [{field: "why", format: "markdown"}, "links"]}

import { cutText } from './value-view.js';
import { formatValue } from './util.js';
import { parseMarkdown, inlineText } from './markdown.js';

/** The keys of a card template. */
export const CARD_TEMPLATE_KEYS = Object.freeze(['title', 'subtitle', 'badges', 'body', 'excerpt', 'fields', 'actions']);
/** The keys of a part object. */
export const CARD_PART_KEYS = Object.freeze(['field', 'title', 'text', 'prefix', 'format', 'tone', 'when', 'unless']);
/** The keys of an action. */
export const CARD_ACTION_KEYS = Object.freeze(['id', 'text', 'kind', 'when', 'unless', 'title', 'disabled']);
/** The formats of a part. */
export const CARD_FORMATS = Object.freeze(['text', 'date', 'datetime', 'count', 'markdown']);
/** The tones of a badge (CSS: `.profile-card-badge.tone-<tone>`, colours from the theme tokens). */
export const BADGE_TONES = Object.freeze(['neutral', 'accent', 'ok', 'warn', 'danger']);
/** The kinds of an action button. */
export const ACTION_KINDS = Object.freeze(['default', 'primary', 'danger']);
/** The variants of a card: `full` (an opened card) and `compact` (a list row). */
export const CARD_VARIANTS = Object.freeze(['full', 'compact']);
/** The option keys of `api.card(label, row, options)`. */
export const CARD_OPTION_KEYS = Object.freeze(['variant', 'selected', 'onOpen', 'actions', 'onAction']);
/** Limits of the template. */
export const CARD_LIMITS = Object.freeze({ excerptDefault: 160, excerptMax: 2000, parts: 50, actions: 12, path: 200, text: 200 });

const ID = /^[A-Za-z][A-Za-z0-9_-]{0,39}$/;
const isPlainObject = (v) => v !== null && typeof v === 'object' && !Array.isArray(v);
const quote = (v) => JSON.stringify(String(v).slice(0, 60));

/** Whether `path` is a valid field path (dot-separated, no empty key). */
export function isFieldPath(path) {
  return typeof path === 'string' && path.length > 0 && path.length <= CARD_LIMITS.path
    && path.split('.').every((key) => key.length > 0);
}

function refProblems(ref, at, problems) {
  if (typeof ref === 'string') {
    if (!isFieldPath(ref)) problems.push(`${at} must be a field path ("name", "author.name"; no empty key)`);
  } else if (Array.isArray(ref) && ref.length > 0 && ref.length <= 10) {
    if (!ref.every(isFieldPath)) problems.push(`${at} must be a list of field paths`);
  } else {
    problems.push(`${at} must be a field path or a list of 1 to 10 field paths`);
  }
}

function shortText(value, at, problems) {
  if (typeof value !== 'string' || value.length > CARD_LIMITS.text) problems.push(`${at} must be a text of at most ${CARD_LIMITS.text} characters`);
}

function partProblems(part, at, problems, { badge = false, field = false } = {}) {
  if (typeof part === 'string') {
    refProblems(part, at, problems);
    return;
  }
  if (!isPlainObject(part)) {
    problems.push(`${at} must be a field path or an object {field, ...}`);
    return;
  }
  for (const key of Object.keys(part)) {
    if (!CARD_PART_KEYS.includes(key)) problems.push(`${at} has the unknown key ${quote(key)} (the keys are ${CARD_PART_KEYS.join(', ')})`);
  }
  if (part.field === undefined) {
    // A badge may be a fixed text shown by `when`/`unless` alone.
    if (!(badge && part.text !== undefined && (part.when !== undefined || part.unless !== undefined))) {
      problems.push(`${at}.field is required (a badge without a field needs "text" and "when" or "unless")`);
    }
  } else {
    refProblems(part.field, `${at}.field`, problems);
  }
  for (const key of ['title', 'text', 'prefix']) if (part[key] !== undefined) shortText(part[key], `${at}.${key}`, problems);
  if (part.format !== undefined) {
    if (!CARD_FORMATS.includes(part.format)) problems.push(`${at}.format must be one of ${CARD_FORMATS.join(', ')}`);
    else if (part.format === 'markdown' && !field) problems.push(`${at}.format "markdown" is only for "fields"`);
  }
  if (part.tone !== undefined) {
    if (!badge) problems.push(`${at}.tone is only for "badges"`);
    else if (typeof part.tone === 'string') {
      if (!BADGE_TONES.includes(part.tone)) problems.push(`${at}.tone must be one of ${BADGE_TONES.join(', ')} or a map from values to tones`);
    } else if (!isPlainObject(part.tone) || !Object.values(part.tone).every((t) => BADGE_TONES.includes(t))) {
      problems.push(`${at}.tone must be one of ${BADGE_TONES.join(', ')} or a map from values to tones`);
    }
  }
  if (part.when !== undefined) refProblems(part.when, `${at}.when`, problems);
  if (part.unless !== undefined) refProblems(part.unless, `${at}.unless`, problems);
}

function partsProblems(list, at, problems, opts) {
  if (!Array.isArray(list) || list.length > CARD_LIMITS.parts) {
    problems.push(`${at} must be a list of at most ${CARD_LIMITS.parts} parts`);
    return;
  }
  list.forEach((part, i) => partProblems(part, `${at}[${i}]`, problems, opts));
}

/** The problems of a list of actions (the template's or `api.card`'s `actions`), each naming `at`. */
export function actionProblems(actions, at) {
  const problems = [];
  if (!Array.isArray(actions) || actions.length > CARD_LIMITS.actions) {
    return [`${at} must be a list of at most ${CARD_LIMITS.actions} actions`];
  }
  const ids = new Set();
  actions.forEach((action, i) => {
    const where = `${at}[${i}]`;
    if (!isPlainObject(action)) {
      problems.push(`${where} must be an object {id, text, kind?, when?, unless?, title?}`);
      return;
    }
    for (const key of Object.keys(action)) {
      if (!CARD_ACTION_KEYS.includes(key)) problems.push(`${where} has the unknown key ${quote(key)} (the keys are ${CARD_ACTION_KEYS.join(', ')})`);
    }
    if (typeof action.id !== 'string' || !ID.test(action.id)) problems.push(`${where}.id must be letters, digits, "_" and "-", starting with a letter`);
    else if (ids.has(action.id)) problems.push(`${where}.id ${quote(action.id)} is used twice`);
    else ids.add(action.id);
    if (typeof action.text !== 'string' || !action.text.trim() || action.text.length > 40) problems.push(`${where}.text must be a text of 1 to 40 characters`);
    if (action.kind !== undefined && !ACTION_KINDS.includes(action.kind)) problems.push(`${where}.kind must be one of ${ACTION_KINDS.join(', ')}`);
    if (action.title !== undefined) shortText(action.title, `${where}.title`, problems);
    if (action.disabled !== undefined && typeof action.disabled !== 'boolean') problems.push(`${where}.disabled must be true or false`);
    if (action.when !== undefined) refProblems(action.when, `${where}.when`, problems);
    if (action.unless !== undefined) refProblems(action.unless, `${where}.unless`, problems);
  });
  return problems;
}

/**
 * The problems of one card template, each naming where (`cards.<label>.<key>`); [] when valid.
 * @param {*} template
 * @param {string} at  e.g. `"cards.person"`.
 */
export function cardTemplateProblems(template, at) {
  const problems = [];
  if (!isPlainObject(template)) return [`${at} must be an object (a card template)`];
  for (const key of Object.keys(template)) {
    if (!CARD_TEMPLATE_KEYS.includes(key)) problems.push(`${at} has the unknown key ${quote(key)} (the keys are ${CARD_TEMPLATE_KEYS.join(', ')})`);
  }
  if (template.title !== undefined) refProblems(template.title, `${at}.title`, problems);
  if (template.subtitle !== undefined) {
    if (Array.isArray(template.subtitle)) partsProblems(template.subtitle, `${at}.subtitle`, problems, {});
    else partProblems(template.subtitle, `${at}.subtitle`, problems, {});
  }
  if (template.badges !== undefined) partsProblems(template.badges, `${at}.badges`, problems, { badge: true });
  if (template.body !== undefined) refProblems(template.body, `${at}.body`, problems);
  if (template.excerpt !== undefined && !(Number.isInteger(template.excerpt) && template.excerpt >= 0 && template.excerpt <= CARD_LIMITS.excerptMax)) {
    problems.push(`${at}.excerpt must be a whole number of characters from 0 to ${CARD_LIMITS.excerptMax}`);
  }
  if (template.fields !== undefined) partsProblems(template.fields, `${at}.fields`, problems, { field: true });
  if (template.actions !== undefined) problems.push(...actionProblems(template.actions, `${at}.actions`));
  return problems;
}

/**
 * The problems of a profile's `cards` ({<label>: template}); [] when valid.
 * @param {*} cards
 */
export function cardsProblems(cards) {
  if (!isPlainObject(cards)) return ['"cards" must be an object'];
  const problems = [];
  for (const [label, template] of Object.entries(cards)) {
    if (!label || label.length > 100) problems.push(`"cards" has a label of ${label.length} characters (1 to 100)`);
    problems.push(...cardTemplateProblems(template, `"cards.${label.slice(0, 60)}"`));
  }
  return problems;
}

/** Whether a row is an element as the engine sends it (`{id, label, properties: {...}}`). */
export function isElementRow(row) {
  return isPlainObject(row) && isPlainObject(row.properties) && typeof row.label === 'string';
}

/** `value` without the one-item list `value_map()` wraps every property in. */
function unwrap(value) {
  return Array.isArray(value) && value.length === 1 ? value[0] : value;
}

/** Whether a (unwrapped) value counts as missing: absent, null, "" or an empty list. */
export function isMissing(value) {
  return value === undefined || value === null || value === '' || (Array.isArray(value) && value.length === 0);
}

function walk(value, keys) {
  let at = value;
  for (const key of keys) {
    at = unwrap(at);
    if (Array.isArray(at) && /^\d+$/.test(key)) at = at[Number(key)];
    else if (isPlainObject(at) && Object.prototype.hasOwnProperty.call(at, key)) at = at[key];
    else return undefined;
  }
  return unwrap(at);
}

/**
 * The value at a field path of `row` (undefined when absent). An element's path is looked up in
 * its `properties` first, then on the element itself; one-item lists are unwrapped.
 * @param {*} row
 * @param {string} path
 */
export function fieldValue(row, path) {
  if (!isFieldPath(path)) return undefined;
  const keys = path.split('.');
  if (isElementRow(row)) {
    const inProperties = walk(row.properties, keys);
    if (inProperties !== undefined) return inProperties;
  }
  return walk(row, keys);
}

/**
 * The first present value of a field ref: `{value, path}`, or null when every path is missing.
 * @param {*} row
 * @param {string|string[]} ref
 */
export function resolveRef(row, ref) {
  const paths = typeof ref === 'string' ? [ref] : Array.isArray(ref) ? ref : [];
  for (const path of paths) {
    const value = fieldValue(row, path);
    if (!isMissing(value)) return { value, path };
  }
  return null;
}

const truthy = (value) => !isMissing(value) && value !== false && value !== 0;
const pad = (n) => String(n).padStart(2, '0');

/** The text of a typed cell (`{"@type", "@value"}`) or the value itself. */
function plainValue(value) {
  if (isPlainObject(value) && typeof value['@type'] === 'string' && value['@value'] !== undefined && typeof value['@value'] !== 'object') return value['@value'];
  return value;
}

function dateText(value, withTime) {
  const v = plainValue(value);
  if (typeof v === 'string') {
    const m = /^(\d{4}-\d{2}-\d{2})(?:[T ](\d{2}:\d{2}))?/.exec(v);
    if (m) return withTime && m[2] ? `${m[1]} ${m[2]}` : m[1];
    return v;
  }
  if (typeof v === 'number' && Number.isFinite(v)) {
    const d = new Date(v);
    if (Number.isNaN(d.getTime())) return String(v);
    const day = `${d.getUTCFullYear()}-${pad(d.getUTCMonth() + 1)}-${pad(d.getUTCDate())}`;
    return withTime ? `${day} ${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())}` : day;
  }
  return formatValue(v);
}

/**
 * One value as one line of text by a part's `format` (never "undefined": a missing value is '').
 * @param {*} value
 * @param {string} [format]
 */
export function formatCardValue(value, format = 'text') {
  if (isMissing(value)) return '';
  switch (format) {
    case 'date': return dateText(value, false);
    case 'datetime': return dateText(value, true);
    case 'count':
      if (Array.isArray(value)) return String(value.length);
      if (isPlainObject(value)) return String(Object.keys(value).length);
      return formatValue(plainValue(value));
    case 'markdown':
    case 'text':
    default: {
      const v = plainValue(value);
      if (Array.isArray(v) && !v.some((x) => x !== null && typeof x === 'object')) return v.map((x) => formatValue(x)).join(', ');
      return formatValue(v);
    }
  }
}

const asPart = (part) => (typeof part === 'string' || Array.isArray(part) ? { field: part } : part);
const lastKey = (path) => path.split('.').pop();

function shown(part, row) {
  if (part.when !== undefined) {
    const w = resolveRef(row, part.when);
    if (!w || !truthy(w.value)) return false;
  }
  if (part.unless !== undefined) {
    const u = resolveRef(row, part.unless);
    if (u && truthy(u.value)) return false;
  }
  return true;
}

/** The text of a subtitle part, or '' when it renders nothing. */
function partText(part, row) {
  if (!shown(part, row)) return '';
  if (part.field === undefined) return part.text || '';
  const hit = resolveRef(row, part.field);
  if (!hit) return '';
  const text = part.text !== undefined ? part.text : formatCardValue(hit.value, part.format);
  return text ? `${part.prefix || ''}${text}` : '';
}

function badgeOf(part, row) {
  const hit = part.field === undefined ? null : resolveRef(row, part.field);
  if (part.when === undefined && part.field !== undefined && (!hit || !truthy(hit.value))) return null;
  if (!shown(part, row)) return null;
  let text;
  if (part.text !== undefined) text = part.text;
  else if (!hit) return null;
  else if (hit.value === true) text = lastKey(hit.path);
  else text = formatCardValue(hit.value, part.format);
  if (!text) return null;
  let tone = 'neutral';
  if (typeof part.tone === 'string') tone = part.tone;
  else if (isPlainObject(part.tone) && hit && !isPlainObject(hit.value) && !Array.isArray(hit.value)) {
    const key = String(plainValue(hit.value));
    if (Object.prototype.hasOwnProperty.call(part.tone, key)) tone = part.tone[key];
  }
  return { text: `${part.prefix || ''}${text}`, tone };
}

/** The plain text of a Markdown text (for the compact card's excerpt), on one line. */
export function markdownPlainText(text) {
  const out = [];
  const visit = (blocks) => {
    for (const b of blocks || []) {
      if (b.type === 'paragraph' || b.type === 'heading') out.push(inlineText(b.children));
      else if (b.type === 'quote') visit(b.children);
      else if (b.type === 'list') b.items.forEach((item) => visit(item));
      else if (b.type === 'code') out.push(b.text);
      else if (b.type === 'table') out.push(b.header.map((c) => inlineText(c.children)).join(' '));
    }
  };
  visit(parseMarkdown(String(text)));
  return out.join(' ').replace(/\s+/g, ' ').trim();
}

/** The top-level keys a field ref reads (to leave them out of the default `fields`). */
function refKeys(ref, into) {
  if (ref === undefined) return;
  for (const path of typeof ref === 'string' ? [ref] : ref) into.add(path.split('.')[0]);
}

/** The default `fields`: every field of the row the other parts do not show. */
function defaultFields(row, template) {
  if (row === null || typeof row !== 'object' || Array.isArray(row)) return isMissing(row) ? [] : [{ key: 'value', title: 'value', value: row, format: 'text' }];
  const used = new Set();
  refKeys(template.title, used);
  refKeys(template.body, used);
  const sub = template.subtitle === undefined ? [] : Array.isArray(template.subtitle) ? template.subtitle : [template.subtitle];
  for (const part of [...sub, ...(template.badges || [])]) refKeys(asPart(part).field, used);
  const source = isElementRow(row) ? row.properties : row;
  return Object.entries(source)
    .filter(([key]) => !used.has(key) && !(source === row && key === 'properties'))
    .map(([key, value]) => ({ key, title: key, value: unwrap(value), format: 'text' }))
    .filter((f) => !isMissing(f.value));
}

/**
 * Checks the options of `api.card(label, row, options)` and returns them normalized: `{variant,
 * selected, onOpen, actions, onAction}` (`actions` null: the template's). Throws a TypeError
 * naming every problem.
 * @param {*} options
 */
export function checkCardOptions(options) {
  if (options === undefined || options === null) options = {};
  if (!isPlainObject(options)) throw new TypeError('api.card: the options must be an object.');
  const problems = [];
  for (const key of Object.keys(options)) {
    if (!CARD_OPTION_KEYS.includes(key)) problems.push(`unknown option ${quote(key)} (the options are ${CARD_OPTION_KEYS.join(', ')})`);
  }
  const variant = options.variant === undefined ? 'full' : options.variant;
  if (!CARD_VARIANTS.includes(variant)) problems.push(`"variant" must be one of ${CARD_VARIANTS.join(', ')}`);
  if (options.selected !== undefined && typeof options.selected !== 'boolean') problems.push('"selected" must be true or false');
  for (const key of ['onOpen', 'onAction']) {
    if (options[key] !== undefined && typeof options[key] !== 'function') problems.push(`"${key}" must be a function`);
  }
  if (options.actions !== undefined) problems.push(...actionProblems(options.actions, '"actions"'));
  if (problems.length) throw new TypeError(`api.card: ${problems.join('; ')}.`);
  return {
    variant,
    selected: options.selected === true,
    onOpen: options.onOpen || null,
    actions: options.actions === undefined ? null : options.actions,
    onAction: options.onAction || null,
  };
}

/**
 * What a card shows (nothing of it is HTML; the view renders text nodes, `body` and markdown
 * fields through the safe Markdown renderer):
 *   {label, variant, selected, openable,
 *    heading: {text, fromLabel},     the title, or the label when the title is missing
 *    subtitle: string,               '' when nothing to show
 *    badges: [{text, tone}],
 *    body: string|null,              the Markdown text (full variant)
 *    excerpt: string|null,           its plain-text start (compact variant)
 *    fields: [{key, title, value, format}]   (full variant; `value` raw, for value-view)
 *    actions: [{id, text, kind, title, disabled}]}   (only with `onAction`)
 * @param {string} label
 * @param {*} row
 * @param {Object|null} template  The profile's template for `label` (null: none, the default card).
 * @param {Object} [options]  From `checkCardOptions`.
 */
export function cardModel(label, row, template, options = {}) {
  const t = isPlainObject(template) ? template : {};
  const variant = options.variant === 'compact' ? 'compact' : 'full';
  const titleHit = t.title === undefined ? null : resolveRef(row, t.title);
  const titleText = titleHit ? formatCardValue(titleHit.value) : '';
  const heading = titleText ? { text: titleText, fromLabel: false } : { text: String(label || ''), fromLabel: true };

  const subParts = t.subtitle === undefined ? [] : Array.isArray(t.subtitle) ? t.subtitle : [t.subtitle];
  const subtitle = subParts.map((p) => partText(asPart(p), row)).filter(Boolean).join(' · ');
  const badges = (t.badges || []).map((p) => badgeOf(asPart(p), row)).filter(Boolean);

  const bodyHit = t.body === undefined ? null : resolveRef(row, t.body);
  const bodyText = bodyHit ? (typeof bodyHit.value === 'string' ? bodyHit.value : formatCardValue(bodyHit.value)) : '';
  let body = null;
  let excerpt = null;
  if (bodyText) {
    if (variant === 'full') body = bodyText;
    else {
      const chars = t.excerpt === undefined ? CARD_LIMITS.excerptDefault : t.excerpt;
      const plain = chars > 0 ? markdownPlainText(bodyText) : '';
      excerpt = plain ? cutText(plain, chars, 1) : null;
    }
  }

  let fields = [];
  if (variant === 'full') {
    if (t.fields === undefined) fields = defaultFields(row, t);
    else {
      for (const p of t.fields) {
        const part = asPart(p);
        if (!shown(part, row)) continue;
        const hit = resolveRef(row, part.field);
        if (!hit) continue;
        fields.push({
          key: hit.path,
          title: part.title !== undefined ? part.title : lastKey(hit.path),
          value: part.format && part.format !== 'text' && part.format !== 'markdown' ? formatCardValue(hit.value, part.format) : hit.value,
          format: part.format || 'text',
        });
      }
    }
  }

  let actions = [];
  if (typeof options.onAction === 'function') {
    const list = options.actions || t.actions || [];
    actions = list.filter((a) => shown(a, row)).map((a) => ({
      id: a.id,
      text: a.text,
      kind: a.kind || 'default',
      title: a.title || '',
      disabled: a.disabled === true,
    }));
  }

  return {
    label: String(label || ''),
    variant,
    selected: options.selected === true,
    openable: typeof options.onOpen === 'function',
    heading,
    subtitle,
    badges,
    body,
    excerpt,
    fields,
    actions,
  };
}
