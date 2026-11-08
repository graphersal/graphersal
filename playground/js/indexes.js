// The model of the Catalog's Property indexes manager: what the engine's `listIndexes()`
// returns (crates/graphersal-session `indexes.rs`: `[{name, element, label, key, type, transform,
// served, reason, stats: {entries, distinctValues, skipped, bytes} | null}]`) turned into rows to
// show, the form's draft and its spec, the index types and transforms the form offers, and the
// Catalog dropdown's items
// with this kind added. Pure functions, no Vue, no DOM (tested under Node:
// tests/indexes.test.mjs). Book page "Property Indexes".

import { formatBytes } from './statistics.js';
import { catalogItems } from './compression.js';

/**
 * The catalog kind of an index (the engine's `DefinitionKind::PROPERTY_INDEX` name); a
 * label-less index is kept under `extKind` (`DefinitionKind::PROPERTY_INDEX_EXT`).
 */
export const INDEX_KIND = Object.freeze({
  kind: 'property_index',
  extKind: 'property_index_ext',
  id: 'indexes',
  title: 'Property indexes',
  hint: 'Indexes that let v()/e() with has_label() and has() read only the matching elements',
});

/**
 * The items of the Catalog dropdown: the kinds of js/compression.js's `catalogItems` and the
 * property indexes, each with its count from the graph info's `catalog` (`{property_index: 1}`;
 * the session's info also has `propertyIndexes`).
 * @param {object|null} info the graph info
 * @returns {Array<{id:string, title:string, hint:string, count:number}>}
 */
export function catalogMenuItems(info) {
  const counts = (info && info.catalog) || {};
  let count = counts[INDEX_KIND.kind];
  if (count !== undefined || counts[INDEX_KIND.extKind] !== undefined) {
    count = (Number(count) || 0) + (Number(counts[INDEX_KIND.extKind]) || 0);
  } else {
    count = info && info.propertyIndexes;
  }
  return [
    ...catalogItems(info),
    { id: INDEX_KIND.id, title: INDEX_KIND.title, hint: INDEX_KIND.hint, count: Number(count) || 0 },
  ];
}

/**
 * The name the engine gives an index without one (`PropertyIndexDefinition::default_name`):
 * `<label>_<key>` (`<key>` for a label-less index, `label` null or empty; `<label>_<key>_edge`
 * for an edge index), then `_<transform>` for an index with
 * a transform (`person_name_to_lower`), every character that is not an ASCII letter, digit or `_`
 * replaced by `_`, prefixed with `index_` when it would not start with a letter. The form shows
 * it as the name's placeholder, so it follows the Transform select while no name is typed.
 */
export function defaultIndexName(element, label, key, transform) {
  let name = (label ? `${label}_${key}` : `${key}`).replace(/[^A-Za-z0-9_]/gu, '_');
  if (element === 'edge') name += '_edge';
  const t = typeof transform === 'string' ? transform.trim() : '';
  if (t) name += `_${t.replace(/[^A-Za-z0-9_]/gu, '_')}`;
  if (!/^[A-Za-z]/.test(name)) name = `index_${name}`;
  return name;
}

/**
 * One row of the indexes table.
 * @param {object} index an entry of `listIndexes()`
 */
export function indexRow(index) {
  const s = index.stats;
  const served = index.served !== false && !!s;
  return {
    name: index.name,
    // A label-less index (`label: null`) covers every element of its kind.
    target: `${index.element || 'vertex'} ${index.label == null ? 'any label' : index.label}`,
    key: index.key,
    served,
    entries: served ? s.entries : '—',
    distinct: served ? s.distinctValues : '—',
    // Elements per value: what an equality reads (the optimizer's estimate).
    perValue: served && s.distinctValues ? (s.entries / s.distinctValues).toFixed(1).replace(/\.0$/u, '') : '—',
    bytes: served ? formatBytes(s.bytes) : '—',
  };
}

/** The totals of all served indexes: entries and bytes. */
export function indexTotals(indexes) {
  const t = { entries: 0, bytes: 0, notServed: 0 };
  for (const i of indexes || []) {
    if (i.served === false || !i.stats) {
      t.notServed += 1;
      continue;
    }
    t.entries += i.stats.entries || 0;
    t.bytes += i.stats.bytes || 0;
  }
  return t;
}

/** The index type the engine uses when a spec names none (`PropertyIndexType::MemoryOrdered`). */
export const DEFAULT_INDEX_TYPE = 'memory-ordered';

/**
 * The index types this version knows (`PropertyIndexType::KNOWN_NAMES`), for the form's Type
 * select: the name sent as `type` and the tooltip text.
 */
export const INDEX_TYPES = Object.freeze([
  Object.freeze({
    name: DEFAULT_INDEX_TYPE,
    description: 'An ordered index kept in memory and rebuilt from the data when the graph is opened; '
      + 'it serves =, P.within and ranges on scalar values (null, booleans, numbers, strings, uuids, '
      + 'dates and times). Lists, maps and vectors are not indexed.',
  }),
]);

/**
 * The index transforms the form's Transform select offers (`PropertyIndexTransform::KNOWN_NAMES`
 * after "none"): the name sent as `transform` ('' sends none), the option's text and its hint.
 */
export const INDEX_TRANSFORMS = Object.freeze([
  Object.freeze({
    name: '',
    label: 'none',
    description: 'The index holds the values themselves.',
  }),
  Object.freeze({
    name: 'to_lower',
    label: 'to_lower',
    description: 'Indexes the lowered text of string values (as to_lower() computes it) for a '
      + 'case-insensitive search. Queries must be written has(key, __.to_lower().is(..)), with the '
      + 'constant in lower case (it is compared as given); P.within and ranges are served too. '
      + 'Other values are not indexed; while the key holds a non-string value queries scan.',
  }),
]);

/** The transform of an entry of `listIndexes()`, '' for none. */
export function indexTransformOf(index) {
  return (index && typeof index.transform === 'string' && index.transform) || '';
}

/** The type of an entry of `listIndexes()` (an entry without one is the default type). */
export function indexTypeOf(index) {
  return (index && typeof index.type === 'string' && index.type) || DEFAULT_INDEX_TYPE;
}

/** Whether this version knows an index type name. */
export function isKnownIndexType(name) {
  return INDEX_TYPES.some((t) => t.name === name);
}

/**
 * Why an index is not served, '' when it is: the engine's `reason` (a type a newer version
 * wrote, a storage without indexes), else the storage text the table showed before.
 */
export function indexNotServedReason(index) {
  if (!index) return '';
  const served = index.served !== false && !!index.stats;
  if (served) return '';
  if (typeof index.reason === 'string' && index.reason) return index.reason;
  return 'This storage keeps the definition but serves no index: queries scan';
}

/** A new index's draft (the form); the form's "any label" box adds `anyLabel: true`. */
export function newIndexDraft() {
  return { name: '', element: 'vertex', label: '', key: '' };
}

/** A new index's draft with the Type select at the default type. */
export function newTypedIndexDraft() {
  return { ...newIndexDraft(), type: DEFAULT_INDEX_TYPE };
}

/** A new index's draft with the Type select at the default type and the Transform at none. */
export function newTransformIndexDraft() {
  return { ...newTypedIndexDraft(), transform: '' };
}

/**
 * The spec `createIndex` takes (`{label, key, element, name?, type?, transform?}`; `type` and
 * `transform` only when the draft has one; `label: null` for a label-less index, the draft's
 * `anyLabel`), or the field errors the form shows without asking the engine (an empty label
 * or key, a key that looks like a jpath). Whether the type fits the schema's declaration of the
 * key is the engine's check (its error comes back for the field `key`).
 * @returns {{spec: object} | {errors: Array<{field:string, message:string}>}}
 */
export function indexDraftToSpec(draft) {
  const errors = [];
  const anyLabel = draft.anyLabel === true;
  const label = anyLabel ? '' : String(draft.label || '').trim();
  const key = String(draft.key || '').trim();
  const name = String(draft.name || '').trim();
  if (!label && !anyLabel) errors.push({ field: 'label', message: 'Name the vertex or edge label the index covers, for example person.' });
  if (!key) {
    errors.push({ field: 'key', message: 'Name the top-level property the index orders by, for example age.' });
  } else if (/^\$[.[]/u.test(key)) {
    errors.push({ field: 'key', message: 'A top-level property name, not a jpath: nested values are not indexed.' });
  }
  if (errors.length) return { errors };
  const spec = { element: draft.element === 'edge' ? 'edge' : 'vertex', label: anyLabel ? null : label, key };
  if (name) spec.name = name;
  const type = typeof draft.type === 'string' ? draft.type.trim() : '';
  if (type) spec.type = type;
  const transform = typeof draft.transform === 'string' ? draft.transform.trim() : '';
  if (transform) spec.transform = transform;
  return { spec };
}

