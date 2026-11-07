// Export helpers: file names, downloads, the files of the Save menu, and SVG -> PNG rasterisation.

/** `graphersal-<kind>-YYYYMMDD-HHMMSS.<ext>` in local time (`d`, default now). */
export function exportFileName(kind, ext, d = new Date()) {
  const p = (n) => String(n).padStart(2, '0');
  const stamp = `${d.getFullYear()}${p(d.getMonth() + 1)}${p(d.getDate())}-${p(d.getHours())}${p(d.getMinutes())}${p(d.getSeconds())}`;
  return `graphersal-${kind}-${stamp}.${ext}`;
}

/**
 * The ONE file of a "Save" menu entry for the graph data (`engine.saveGraph()` result): GraphSON
 * `graphersal-graph-<stamp>.json` when `saved.format` is 'graphson', GraphML
 * `graphersal-graph-<stamp>.graphml` for 'graphml', a packed snapshot
 * `graphersal-graph-<stamp>.gsnap` (`bytes` instead of `text`, carries the schema) for
 * 'snapshot'. GraphSON and GraphML carry no schema: it is saved by its own menu entry
 * ([`schemaFile`]). One click downloads one file, because browsers block or drop the second of
 * two downloads started by one click.
 * @param {{format?: 'graphson'|'graphml'|'snapshot', text?: string, bytes?: Uint8Array}} saved
 * @returns {{name: string, type: string, text?: string, bytes?: Uint8Array}}
 */
export function savedGraphFile(saved, d = new Date()) {
  if (saved.format === 'snapshot') {
    const kind = saved.withoutModels ? 'graph-without-models' : 'graph';
    return { name: exportFileName(kind, 'gsnap', d), type: 'application/octet-stream', bytes: saved.bytes };
  }
  return saved.format === 'graphson'
    ? { name: exportFileName('graph', 'json', d), type: 'application/json', text: saved.text }
    : { name: exportFileName('graph', 'graphml', d), type: 'application/xml', text: saved.text };
}

/**
 * Whether a stored schema (as `engine.getSchema()` returns it) is no schema: `null`, or an
 * envelope without labels and without a mode other than `none` (`set_schema({})` clears).
 */
export function isEmptySchema(schema) {
  if (!schema || typeof schema !== 'object') return true;
  const labels = Object.keys(schema.vertices || {}).length + Object.keys(schema.edges || {}).length;
  return labels === 0 && (!schema.mode || schema.mode === 'none');
}

/** The schema file `graphersal-schema-<stamp>.json` (pretty JSON) of a stored schema object. */
export function schemaFile(schema, d = new Date()) {
  return { name: exportFileName('schema', 'json', d), type: 'application/json', text: JSON.stringify(schema, null, 2) + '\n' };
}

/**
 * The catalog file `graphersal-catalog-<stamp>.json`: the catalog's JSON exchange form
 * (`{"definitions": [..]}`: saved queries, compression rules and every other kind) exactly as
 * the engine wrote it. Load it back with "Catalog ▸ Load catalog".
 */
export function catalogFile(text, d = new Date()) {
  return { name: exportFileName('catalog', 'json', d), type: 'application/json', text };
}

/** Whether a catalog file's text holds no definitions. */
export function isEmptyCatalog(text) {
  try {
    const catalog = JSON.parse(text);
    return !catalog || !Array.isArray(catalog.definitions) || catalog.definitions.length === 0;
  } catch (err) {
    return true;
  }
}

/** Saves a Blob as a file through a temporary link. */
export function downloadBlob(blob, fileName) {
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = fileName;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 10000);
}

/**
 * Makes an SVG string self-contained for export: explicit width/height from the viewBox, the
 * namespaces, the page's font inlined, an optional opaque background, and any `<foreignObject>`
 * (HTML labels, which taint a canvas in some browsers) replaced by plain SVG text.
 * @returns {{text: string, width: number, height: number}}
 */
export function prepareSvg(svgText, { background = null, fontFamily = null } = {}) {
  const doc = new DOMParser().parseFromString(svgText, 'image/svg+xml');
  const err = doc.getElementsByTagName('parsererror')[0];
  if (err) throw new Error('the diagram is not valid SVG');
  const svg = doc.documentElement;
  const NS = 'http://www.w3.org/2000/svg';
  svg.setAttribute('xmlns', NS);
  svg.setAttribute('xmlns:xlink', 'http://www.w3.org/1999/xlink');
  const vb = (svg.getAttribute('viewBox') || '').split(/[\s,]+/).map(Number);
  let [vx, vy, width, height] = vb.length === 4 && vb.every(Number.isFinite) ? vb : [0, 0, 0, 0];
  if (!width || !height) {
    width = parseFloat(svg.getAttribute('width')) || 800;
    height = parseFloat(svg.getAttribute('height')) || 600;
    vx = 0;
    vy = 0;
    svg.setAttribute('viewBox', `0 0 ${width} ${height}`);
  }
  svg.setAttribute('width', String(width));
  svg.setAttribute('height', String(height));
  svg.removeAttribute('style'); // Mermaid's max-width
  if (fontFamily) svg.setAttribute('font-family', fontFamily);

  for (const fo of Array.from(svg.getElementsByTagNameNS(NS, 'foreignObject'))) {
    const w = parseFloat(fo.getAttribute('width')) || 0;
    const h = parseFloat(fo.getAttribute('height')) || 0;
    const text = doc.createElementNS(NS, 'text');
    text.setAttribute('x', String((parseFloat(fo.getAttribute('x')) || 0) + w / 2));
    text.setAttribute('y', String((parseFloat(fo.getAttribute('y')) || 0) + h / 2));
    text.setAttribute('text-anchor', 'middle');
    text.setAttribute('dominant-baseline', 'central');
    text.setAttribute('font-size', '14');
    text.textContent = fo.textContent.trim();
    fo.replaceWith(text);
  }

  if (background) {
    const rect = doc.createElementNS(NS, 'rect');
    rect.setAttribute('x', String(vx));
    rect.setAttribute('y', String(vy));
    rect.setAttribute('width', String(width));
    rect.setAttribute('height', String(height));
    rect.setAttribute('fill', background);
    svg.insertBefore(rect, svg.firstChild);
  }
  return { text: new XMLSerializer().serializeToString(svg), width, height };
}

/** Rasterises a prepared SVG at `scale` into a PNG Blob. Rejects with a readable message. */
export function svgToPngBlob(svgText, width, height, scale = 2) {
  return new Promise((resolve, reject) => {
    const url = URL.createObjectURL(new Blob([svgText], { type: 'image/svg+xml;charset=utf-8' }));
    const img = new Image();
    img.onload = () => {
      try {
        const canvas = document.createElement('canvas');
        canvas.width = Math.max(1, Math.round(width * scale));
        canvas.height = Math.max(1, Math.round(height * scale));
        const ctx = canvas.getContext('2d');
        ctx.scale(scale, scale);
        ctx.drawImage(img, 0, 0, width, height);
        canvas.toBlob((blob) => {
          URL.revokeObjectURL(url);
          if (blob) resolve(blob);
          else reject(new Error('the browser could not encode the PNG (drawing too large?)'));
        }, 'image/png');
      } catch (e) {
        URL.revokeObjectURL(url);
        reject(new Error(e && e.name === 'SecurityError'
          ? 'the browser refused to export the drawing (tainted canvas); export as SVG instead'
          : `PNG export failed: ${e && e.message ? e.message : e}`));
      }
    };
    img.onerror = () => {
      URL.revokeObjectURL(url);
      reject(new Error('the browser could not load the SVG for rasterisation; export as SVG instead'));
    };
    img.src = url;
  });
}
