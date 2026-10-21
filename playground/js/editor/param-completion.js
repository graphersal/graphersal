// The declared parameters of a saved query as completion items of its body (the saved-query
// editor, components/saved-queries.js). A Rhai body is a function body: its parameters are the
// function's arguments, offered as names above the DSL's global items. A GQL body reads them as
// `$name`: they go to the GQL completion's `$` list (js/editor/gql-completion.js `getParams`).
// The list is read on every request, so it follows the parameter table live. No CodeMirror
// import: the sources return plain completion results (tests/param-completion.test.mjs).

/** The type text of an editor parameter (js/saved-queries.js `newParam`): `integer`, `array of string`. */
export function paramTypeText(param) {
  if (!param) return '';
  if (param.type === 'array') return `array of ${param.itemsType || 'string'}`;
  return param.type || 'string';
}

/** The named parameters of the editor's table: `[{name, detail}]`, blank and repeated names left out. */
export function declaredParams(params) {
  const out = [];
  for (const p of params || []) {
    const name = String((p && p.name) || '').trim();
    if (!name || out.some((o) => o.name === name)) continue;
    out.push({ name, detail: paramTypeText(p) });
  }
  return out;
}

/**
 * Where a Rhai name is being typed at the end of `before` (the text up to the cursor):
 * `{from, prefix}` (`from` an offset into `before`), or null after `.` / `::` (a step or token
 * value comes there), inside a string or a comment.
 */
export function rhaiNameContext(before) {
  const text = String(before ?? '');
  const line = text.slice(text.lastIndexOf('\n') + 1);
  let quotes = 0;
  for (let i = 0; i < line.length; i++) {
    if (line[i] === '"' && line[i - 1] !== '\\') quotes += 1;
    if (quotes % 2 === 0 && line.startsWith('//', i)) return null;
  }
  if (quotes % 2 === 1) return null;
  const m = /[A-Za-z_][A-Za-z0-9_]*$/.exec(text);
  const prefix = m ? m[0] : '';
  const from = text.length - prefix.length;
  if (/(\.|::)\s*$/.test(text.slice(0, from))) return null;
  if (/[0-9]$/.test(text.slice(0, from)) && prefix) return null;
  return { from, prefix };
}

/**
 * The completion source of a Rhai body's parameters: `getParams()` gives the editor's table
 * (read on each request). Ranked above the DSL's globals (boost 5); opens on a typed prefix or
 * Ctrl+Space.
 */
export function rhaiParamSource(getParams) {
  return (context) => {
    const before = context.state.sliceDoc(0, context.pos);
    const at = rhaiNameContext(before);
    if (!at || (!at.prefix && !context.explicit)) return null;
    const params = declaredParams(getParams());
    if (!params.length) return null;
    return {
      from: at.from,
      options: params.map((p) => ({ label: p.name, type: 'variable', detail: `parameter · ${p.detail}`, boost: 5 })),
      validFor: /^[A-Za-z0-9_]*$/,
    };
  };
}
