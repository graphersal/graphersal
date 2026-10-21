// Playground links: a graph and a query in the address, so a page (the book) can open an example
// ready to run, and a user can share a query together with a small graph (a bug report).
//
//   https://play.graphersal.dev/#/play?graph=modern&q=g.v().count()&run=1
//   https://play.graphersal.dev/#/play?v=1&data=<graph>&schema=<schema>&q=..&run=1
//
// The parameters follow `#/play?`, in the fragment: the browser never sends them to the server,
// so a shared graph goes only where the user pastes the link. Values are percent-encoded
// (`encodeURIComponent`; `+` also reads as a space, as in a query string). Parameters:
//   graph   a sample graph: modern (the default), empty, large, tree
//   data    instead of `graph`: the graph itself, GraphSON 3.0 text compressed with deflate-raw
//           and written as base64url (`encodeLinkData`)
//   schema  with `data` only: the graph's schema (the schema format's JSON), encoded the same way
//   v       the format of `data` and `schema`; absent means 1, the only format so far
//   q       the script for the editor
//   lang    gql: `q` is a plain GQL query and opens in a GQL tab (absent: a Rhai script; any
//           other value is refused, so a later language is not run as Rhai)
//   params  with lang=gql: the JSON text of the tab's Parameters field (optional)
//   title   the tab name (optional)
//   run     1: run the script once it is in the editor (never with the dev server, whose graph is
//           the user's own data)
// Unknown parameters are ignored, so a later parameter (for example the catalog) is additive: a
// link without it keeps opening as before. A change of what `data` or `schema` mean needs a new
// `v`; a link with a newer `v` is refused with a reason.
// Pure functions, tested in tests/link.test.mjs; router.js applies a link, the share dialog
// (components/share-dialog.js) makes one.

/** The sample graphs a link may name (the playground's start page offers the same). */
export const LINK_GRAPHS = Object.freeze(['modern', 'empty', 'large', 'tree']);

/** The longest script a link may carry, in characters. */
export const MAX_LINK_SCRIPT = 100000;

/** The format of `data` and `schema` this playground writes and reads. */
export const LINK_FORMAT = 1;

/**
 * The longest link the share dialog makes, in characters (the whole address). A link must fit
 * into the text of an issue (GitHub takes 65,536 characters) next to the description, so only
 * small graphs fit: the graph that reproduces a problem, not the whole data set.
 */
export const MAX_SHARE_LINK = 60000;

/** The longest `data` or `schema` parameter a link may carry, in characters (opening a link). */
export const MAX_LINK_DATA = 1000000;

/** The largest text `data` or `schema` may decompress to, in bytes (a guard against bombs). */
export const MAX_LINK_DATA_BYTES = 16 * 1024 * 1024;

const MAX_TITLE = 40;
const BASE64URL = /^[A-Za-z0-9_-]*$/;

/**
 * The link in a location hash, or null when the hash carries none (`#/`, `#/play`, a hash without
 * `graph`, `data` or `q`). A link that names an unknown graph, is too long or malformed comes back
 * with `error` and nothing else to apply. `data`/`schema` stay encoded (`decodeLinkData`).
 * @param {string} hash  `window.location.hash`, with or without the leading `#`
 * @returns {null | {graph: string|null, data: string|null, schema: string|null, script: string|null, title: string, run: boolean, language?: 'gql', params?: string} | {error: string}}
 */
export function parseLink(hash) {
  const h = String(hash || '').replace(/^#/, '');
  const at = h.indexOf('?');
  if (!h.startsWith('/play') || at < 0) return null;
  let params;
  try {
    params = new URLSearchParams(h.slice(at + 1));
  } catch (e) {
    return { error: 'The link could not be read.' };
  }
  if (!params.has('graph') && !params.has('q') && !params.has('data')) return null;
  if (params.has('v')) {
    const v = params.get('v');
    if (!/^[1-9][0-9]{0,3}$/.test(v)) return { error: `The link's format "${v.slice(0, 10)}" is not a version number.` };
    if (Number(v) > LINK_FORMAT) {
      return { error: `The link was made by a newer playground (link format ${v}, this page reads ${LINK_FORMAT}). Reload the page to get the latest playground.` };
    }
  }
  let graph = null;
  let data = null;
  let schema = null;
  if (params.has('data')) {
    if (params.has('graph')) return { error: 'The link names a sample graph and carries a graph: it can only do one.' };
    data = params.get('data');
    schema = params.has('schema') ? params.get('schema') : null;
    for (const [name, value] of [['graph', data], ['schema', schema]]) {
      if (value === null) continue;
      if (value.length > MAX_LINK_DATA) return { error: `The link's ${name} is longer than ${MAX_LINK_DATA} characters.` };
      if (!BASE64URL.test(value)) return { error: `The link's ${name} is damaged (it was perhaps cut off or changed when it was copied).` };
    }
    if (data === '') return { error: 'The link\'s graph is empty (it was perhaps cut off when it was copied).' };
  } else {
    if (params.has('schema')) return { error: 'The link carries a schema without a graph.' };
    graph = params.get('graph') || 'modern';
    if (!LINK_GRAPHS.includes(graph)) {
      return { error: `The link names an unknown graph "${graph.slice(0, 40)}"; the samples are ${LINK_GRAPHS.join(', ')}.` };
    }
  }
  const script = params.has('q') ? params.get('q') : null;
  if (script !== null && script.length > MAX_LINK_SCRIPT) {
    return { error: `The link's query is longer than ${MAX_LINK_SCRIPT} characters.` };
  }
  const title = (params.get('title') || '').trim().slice(0, MAX_TITLE);
  let language = 'rhai';
  if (params.has('lang')) {
    const lang = params.get('lang');
    if (lang !== 'gql' && lang !== 'rhai') return { error: `The link's query language "${lang.slice(0, 20)}" is unknown; this playground reads rhai and gql.` };
    language = lang;
  }
  const gqlParams = language === 'gql' && params.has('params') ? params.get('params') : '';
  if (gqlParams.length > MAX_LINK_SCRIPT) return { error: `The link's parameters are longer than ${MAX_LINK_SCRIPT} characters.` };
  const link = { graph, data, schema, script, title, run: params.get('run') === '1' && script !== null && script.trim() !== '' };
  // A Rhai link keeps the shape it always had; a GQL link adds its language and parameters.
  return language === 'gql' ? { ...link, language, params: gqlParams } : link;
}

/**
 * The location hash of a link (the inverse of `parseLink`). With `data` (from `encodeLinkData`)
 * the link carries the graph itself and `graph` is not written.
 * `language` 'gql' writes `lang=gql` (and `params` when the Parameters field is not blank).
 * @param {{graph?: string, data?: string|null, schema?: string|null, script?: string, title?: string, run?: boolean, language?: string, params?: string}} link
 */
export function linkHash({ graph = 'modern', data = null, schema = null, script, title, run = false, language = 'rhai', params = '' }) {
  const parts = [];
  if (data !== null && data !== undefined) {
    parts.push(`v=${LINK_FORMAT}`, `data=${data}`);
    if (schema !== null && schema !== undefined) parts.push(`schema=${schema}`);
  } else {
    parts.push(`graph=${encodeURIComponent(graph)}`);
  }
  if (script !== undefined && script !== null) parts.push(`q=${encodeURIComponent(script)}`);
  if (language === 'gql') {
    parts.push('lang=gql');
    if (params && params.trim()) parts.push(`params=${encodeURIComponent(params)}`);
  }
  if (title) parts.push(`title=${encodeURIComponent(title)}`);
  if (run) parts.push('run=1');
  return `#/play?${parts.join('&')}`;
}

/**
 * Text as a link parameter: UTF-8, compressed with deflate-raw (`CompressionStream`, built into
 * browsers and Node), written as base64url (no padding; safe in a URL without escaping).
 * @param {string} text
 * @returns {Promise<string>}
 */
export async function encodeLinkData(text) {
  const input = new Blob([new TextEncoder().encode(String(text))]).stream();
  const compressed = new Uint8Array(await new Response(input.pipeThrough(new CompressionStream('deflate-raw'))).arrayBuffer());
  let binary = '';
  for (let i = 0; i < compressed.length; i += 0x8000) {
    binary += String.fromCharCode.apply(null, compressed.subarray(i, i + 0x8000));
  }
  return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

/**
 * The text of a link parameter (the inverse of `encodeLinkData`). Rejects with a readable reason
 * when the parameter is damaged or decompresses to more than `maxBytes`.
 * @param {string} encoded
 * @param {number} [maxBytes]
 * @returns {Promise<string>}
 */
export async function decodeLinkData(encoded, maxBytes = MAX_LINK_DATA_BYTES) {
  const damaged = () => new Error('The graph in the link is damaged (it was perhaps cut off or changed when it was copied).');
  let bytes;
  try {
    const binary = atob(String(encoded).replace(/-/g, '+').replace(/_/g, '/'));
    bytes = Uint8Array.from(binary, (c) => c.charCodeAt(0));
  } catch (e) {
    throw damaged();
  }
  const chunks = [];
  let total = 0;
  try {
    const reader = new Blob([bytes]).stream().pipeThrough(new DecompressionStream('deflate-raw')).getReader();
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      total += value.length;
      if (total > maxBytes) {
        await reader.cancel().catch(() => {});
        throw new Error(`The graph in the link is larger than ${Math.floor(maxBytes / (1024 * 1024))} MiB.`);
      }
      chunks.push(value);
    }
  } catch (e) {
    if (e && /larger than/.test(e.message)) throw e;
    throw damaged();
  }
  const all = new Uint8Array(total);
  let at = 0;
  for (const c of chunks) {
    all.set(c, at);
    at += c.length;
  }
  try {
    return new TextDecoder('utf-8', { fatal: true }).decode(all);
  } catch (e) {
    throw damaged();
  }
}

/** The public playground: a link made on the dev server points here (a local address is no use to others). */
export const PUBLIC_PLAYGROUND = 'https://play.graphersal.dev/';

/** More vertices + edges than this never fit into a link: the share dialog does not even try. */
export const MAX_SHARE_ELEMENTS = 20000;

/**
 * Whether a link can name the graph as a sample instead of carrying it: a sample that has not
 * changed since it was loaded (commit 0 of the playground's store in memory, no past view open).
 * @param {{kind?: string}|null} graph  the graph info
 * @param {{commitSeq?: number}|null} store  the store info (null when the engine has none)
 * @param {boolean} inView  a read-only view of a past state is open
 */
export function isUnchangedSample(graph, store, inView) {
  return !!graph && LINK_GRAPHS.includes(graph.kind) && !!store && store.commitSeq === 0 && !inView;
}

/**
 * The shared link: the address of `base` (the page without its hash) with the link's hash, its
 * length and whether it fits `MAX_SHARE_LINK`.
 * @param {string} base
 * @param {Parameters<typeof linkHash>[0]} link
 * @returns {{url: string, chars: number, fits: boolean}}
 */
export function shareLink(base, link) {
  const url = String(base).replace(/#.*$/, '') + linkHash(link);
  return { url, chars: url.length, fits: url.length <= MAX_SHARE_LINK };
}
