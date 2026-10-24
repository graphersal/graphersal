// The schema editor of the Schema panel: nested tables on one page (a table per label, a nested
// object as a table inside its row, arrays as `array of T`), edited in place. Every edit changes
// the DRAFT only (js/schema/draft.js); the review (what changes, check against data) and Apply
// go through js/schema/flow.js, which talks to the engine only through the seven schema methods
// (js/schema/api.js).

import { reactive, computed } from 'vue';
import { toast } from '../store.js';
import {
  BASE_TYPES, CONSTRAINTS, MODES, describe, getAt, typeText, constraintSummary, propertiesOf,
  nestedObjectPath, isRequired, setBaseType, setNullable, setRequired, setKeyword, addProperty,
  renameProperty, removeProperty, addLabel, renameLabel, removeLabel, addConnection, setConnection,
  removeConnection, isPlainObject, backfillScript, DEFAULT_FUNCTIONS, isTopLevelProperty, defaultSummary,
  VECTOR_ELEMENTS, VECTOR_METRICS, MAX_VECTOR_DIMENSION,
} from '../schema/draft.js';

/** Runs an edit of the draft; a refused edit (a duplicate name) becomes a toast. */
function edit(flow, fn) {
  try {
    return flow.ed.edit(fn);
  } catch (err) {
    toast(err && err.message ? err.message : String(err), 'error');
    return undefined;
  }
}

/** The delete button's icon (a trash can): unlike "×" it does not read as "close". */
const TRASH_ICON = '<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" aria-hidden="true"><path d="M2.5 4h11M6 4V2.5h4V4M4 4l.7 9.5h6.6L12 4M6.8 6.5v5M9.2 6.5v5"/></svg>';

const CONSTRAINT_LABELS = {
  minimum: 'minimum', maximum: 'maximum', exclusiveMinimum: 'exclusive min', exclusiveMaximum: 'exclusive max',
  minLength: 'min length', maxLength: 'max length', pattern: 'pattern', minItems: 'min items', maxItems: 'max items',
  formatMinimum: 'earliest', formatMaximum: 'latest', formatExclusiveMinimum: 'after', formatExclusiveMaximum: 'before',
};
/** Constraints written as text (a pattern, a date bound), not as a number. */
const TEXT_KEYWORDS = new Set(['pattern', 'formatMinimum', 'formatMaximum', 'formatExclusiveMinimum', 'formatExclusiveMaximum']);
/** An example bound of a date and time type (the field's placeholder). */
const BOUND_EXAMPLE = {
  datetime: '2000-01-01T00:00:00Z', localdatetime: '2000-01-01T00:00:00', date: '2000-01-01', time: '08:00:00',
  offsettime: '08:00:00+02:00', duration: 'PT1H',
};
const INTEGER_KEYWORDS = new Set(['minLength', 'maxLength', 'minItems', 'maxItems']);

/** The value of a JSON-typed form field: '' removes the keyword; invalid JSON is reported. */
function parseJsonField(text) {
  if (text.trim() === '') return { ok: true, value: undefined };
  try {
    return { ok: true, value: JSON.parse(text) };
  } catch (err) {
    return { ok: false };
  }
}

const jsonFieldText = (v) => (v === undefined ? '' : JSON.stringify(v));

/**
 * The detail form of one property: its constraints (on the node that carries the type) and its
 * annotations (on the property node). `default` is what a create stores for an absent top-level
 * property (engine-side; the form only edits the keyword).
 */
export const SchemaNodeDetails = {
  props: { flow: { type: Object, required: true }, path: { type: Array, required: true } },
  setup(props) {
    const bad = reactive({}); // keyword -> true while its JSON text does not parse
    const node = () => getAt(props.flow.ed.working, props.path) || {};
    const d = () => describe(node());
    const valuePath = () => [...props.path, ...d().value];
    const value = () => getAt(props.flow.ed.working, valuePath()) || {};

    function setConstraint(keyword, text) {
      let v;
      if (text === '') v = undefined;
      else if (TEXT_KEYWORDS.has(keyword)) v = text.trim();
      else {
        v = Number(text);
        if (!Number.isFinite(v) || (INTEGER_KEYWORDS.has(keyword) && (!Number.isInteger(v) || v < 0))) {
          bad[keyword] = true;
          return;
        }
      }
      bad[keyword] = false;
      edit(props.flow, (doc) => setKeyword(doc, valuePath(), keyword, v));
    }
    function setJson(keyword, text, onValue) {
      const parsed = parseJsonField(text);
      bad[keyword] = !parsed.ok;
      if (!parsed.ok) return;
      if (keyword === 'enum' && parsed.value !== undefined && !Array.isArray(parsed.value)) {
        bad[keyword] = true;
        return;
      }
      edit(props.flow, (doc) => setKeyword(doc, onValue ? valuePath() : props.path, keyword, parsed.value));
    }
    function setText(keyword, text) {
      edit(props.flow, (doc) => setKeyword(doc, props.path, keyword, text === '' ? undefined : text));
    }
    // A vector's element kind, dimension and default metric (on the value node, like the type).
    function setVector(keyword, text) {
      let v = text === '' ? undefined : text;
      if (keyword === 'dimension' && v !== undefined) {
        v = Number(text);
        if (!Number.isInteger(v) || v < 1 || v > MAX_VECTOR_DIMENSION) {
          bad.dimension = true;
          return;
        }
      }
      if (keyword === 'embedder' && v !== undefined && !/^[A-Za-z0-9._/:@?=-]{1,128}$/.test(v)) {
        bad.embedder = true;
        return;
      }
      if (keyword === 'element' && v === undefined) v = 'f32';
      bad[keyword] = false;
      edit(props.flow, (doc) => setKeyword(doc, valuePath(), keyword, v));
    }
    // `default` and `defaultFunction` exclude each other (the engine refuses both on one node).
    function setDefaultFunction(name) {
      edit(props.flow, (doc) => {
        setKeyword(doc, props.path, 'defaultFunction', name === '' ? undefined : name);
        if (name !== '') setKeyword(doc, props.path, 'default', undefined);
      });
    }
    function setDeprecated(on) {
      edit(props.flow, (doc) => setKeyword(doc, props.path, 'deprecated', on ? true : undefined));
    }
    function setAdditional(choice) {
      const v = choice === 'default' ? undefined : choice === 'true';
      edit(props.flow, (doc) => setKeyword(doc, valuePath(), 'additionalProperties', v));
    }
    const additional = () => {
      const v = value().additionalProperties;
      return v === true ? 'true' : v === false ? 'false' : 'default';
    };
    return {
      bad, node, d, value, CONSTRAINTS, CONSTRAINT_LABELS, TEXT_KEYWORDS, BOUND_EXAMPLE, jsonFieldText, additional,
      setConstraint, setJson, setText, setDeprecated, setAdditional, setDefaultFunction, setVector,
      VECTOR_ELEMENTS, VECTOR_METRICS, MAX_VECTOR_DIMENSION,
      functions: () => DEFAULT_FUNCTIONS[d().base] || [],
      topLevel: () => isTopLevelProperty(props.path),
    };
  },
  template: `
  <div class="schema-details">
    <fieldset v-if="d().base === 'vector'">
      <legend>Vector</legend>
      <label><span>element</span>
        <select :value="value().element ?? 'f32'" @change="setVector('element', $event.target.value)">
          <option v-for="e in VECTOR_ELEMENTS" :key="e" :value="e">{{ e }}</option>
        </select></label>
      <label><span>dimension</span>
        <input type="number" min="1" :max="MAX_VECTOR_DIMENSION" step="1" :class="{ invalid: bad.dimension }"
               :value="value().dimension ?? ''" placeholder="any" @change="setVector('dimension', $event.target.value)"></label>
      <label><span>metric</span>
        <select :value="value().metric ?? ''" @change="setVector('metric', $event.target.value)">
          <option value="">none (cosine)</option>
          <option v-for="m in VECTOR_METRICS" :key="m" :value="m">{{ m }}</option>
        </select></label>
      <label v-if="topLevel()"><span>embedder</span>
        <input type="text" :class="{ invalid: bad.embedder }" :value="value().embedder ?? ''" placeholder="none"
               spellcheck="false" @change="setVector('embedder', $event.target.value.trim())"></label>
      <p class="hint wide">element: how components are stored: f32 exact, f16 half precision, i8 one byte per component plus a scale,
        bit one bit per component (the sign). A list of numbers or a vector of another element written to the property is converted
        to it (quantized). dimension: the number of components every value must have (empty: any). metric: the default of
        <code>similarity</code> and <code>nearest</code> on this property; <code>hamming</code> needs bit vectors.
        embedder (a label's top-level property only): the model identity of the embedder that produced the vectors, as
        <code>g.embedders()</code> lists it under <code>model</code> (<code>hash-ngram/1</code>); <code>embed_property</code> fills it in,
        and a text query (<code>nearest("emb", "graph indexes", 5)</code>) embeds its text with that model.</p>
    </fieldset>
    <fieldset v-if="CONSTRAINTS[d().base] && CONSTRAINTS[d().base].length">
      <legend>Constraints</legend>
      <label v-for="k in CONSTRAINTS[d().base]" :key="k">
        <span>{{ CONSTRAINT_LABELS[k] }}</span>
        <input :type="TEXT_KEYWORDS.has(k) ? 'text' : 'number'" :class="{ invalid: bad[k], mono: TEXT_KEYWORDS.has(k) && k !== 'pattern' }" :value="value()[k] ?? ''"
               :placeholder="k === 'pattern' ? '^[A-Z].*$' : BOUND_EXAMPLE[d().base] || ''" @change="setConstraint(k, $event.target.value)">
      </label>
      <p v-if="d().base === 'string'" class="hint">A pattern is not anchored: write <code>^…$</code> to match the whole value.</p>
      <p v-if="BOUND_EXAMPLE[d().base]" class="hint">Bounds are values of the type, written as their ISO 8601 text and compared as values (a datetime by its instant); the engine checks them when the schema is applied.</p>
    </fieldset>
    <fieldset>
      <legend>Values</legend>
      <label><span>enum (JSON array)</span>
        <input type="text" :class="{ invalid: bad.enum }" :value="jsonFieldText(value().enum)" placeholder='["a", "b"]'
               @change="setJson('enum', $event.target.value, true)"></label>
      <label><span>const (JSON)</span>
        <input type="text" :class="{ invalid: bad.const }" :value="jsonFieldText(value().const)"
               @change="setJson('const', $event.target.value, true)"></label>
      <label v-if="d().base === 'object'"><span>additional properties</span>
        <select :value="additional()" @change="setAdditional($event.target.value)">
          <option value="default">mode default (open: allowed, closed: forbidden)</option>
          <option value="true">allowed</option>
          <option value="false">forbidden</option>
        </select></label>
    </fieldset>
    <fieldset>
      <legend>Annotations (no effect on validation)</legend>
      <label><span>title</span><input type="text" :value="node().title ?? ''" @change="setText('title', $event.target.value)"></label>
      <label class="wide"><span>description</span><textarea rows="2" :value="node().description ?? ''" @change="setText('description', $event.target.value)"></textarea></label>
      <label><span>default (JSON)</span>
        <input type="text" :class="{ invalid: bad.default }" :value="jsonFieldText(node().default)"
               :disabled="!!node().defaultFunction" :title="node().defaultFunction ? 'Clear the default function to set a fixed default' : ''"
               @change="setJson('default', $event.target.value, false)"></label>
      <label v-if="topLevel() && (functions().length || node().defaultFunction)"><span>default function</span>
        <select :value="node().defaultFunction ?? ''" @change="setDefaultFunction($event.target.value)">
          <option value="">none</option>
          <option v-for="f in functions()" :key="f" :value="f">{{ f }}</option>
          <option v-if="node().defaultFunction && !functions().includes(node().defaultFunction)" :value="node().defaultFunction">{{ node().defaultFunction }} (does not fit the type)</option>
        </select></label>
      <p class="hint wide">On a top-level property in mode open or closed: stored when a vertex or edge is created without it
        (also satisfies required). Never on read, never for existing data (the backfill script fills those); null is never stored.
        A default function computes the value instead: <code>uuidv4</code>/<code>uuidv7</code> a new uuid per element,
        <code>now</code> (datetime) and <code>today</code> (date) the start of the unit, the same for every element it creates.</p>
      <label><span>examples (JSON array)</span>
        <input type="text" :class="{ invalid: bad.examples }" :value="jsonFieldText(node().examples)"
               @change="setJson('examples', $event.target.value, false)"></label>
      <label><span>$comment</span><input type="text" :value="node().$comment ?? ''" @change="setText('$comment', $event.target.value)"></label>
      <label class="check"><input type="checkbox" :checked="node().deprecated === true" @change="setDeprecated($event.target.checked)"> deprecated</label>
    </fieldset>
  </div>
  `,
};

/**
 * The property table of one object node (`path` points at the node that has `properties`), with
 * its nested tables, detail forms and the "+" row. Recursive.
 */
export const SchemaObjectTable = {
  name: 'SchemaObjectTable',
  components: { SchemaNodeDetails },
  props: {
    flow: { type: Object, required: true },
    path: { type: Array, required: true },
    depth: { type: Number, default: 0 },
  },
  setup(props) {
    const open = reactive({}); // property -> detail form open
    const expanded = reactive({}); // property -> nested table expanded (deep objects start collapsed)
    const draftName = reactive({ value: '' });
    const doc = () => props.flow.ed.working;
    const props_ = () => propertiesOf(getAt(doc(), props.path) || {});
    const propPath = (key) => [...props.path, 'properties', key];
    const nested = (key) => nestedObjectPath(doc(), propPath(key));
    const isExpanded = (key) => (key in expanded ? expanded[key] : props.depth < 1);

    function add() {
      const name = draftName.value.trim();
      if (!name) return;
      if (edit(props.flow, (d) => addProperty(d, props.path, name)) !== undefined) draftName.value = '';
    }
    function rename(key, to) {
      const n = edit(props.flow, (d) => renameProperty(d, props.path, key, to));
      if (n && n !== key) {
        open[n] = open[key];
        expanded[n] = expanded[key];
      }
    }
    return {
      open, expanded, draftName, props_, propPath, nested, isExpanded, add, rename, TRASH_ICON,
      BASE_TYPES,
      info: (key) => describe(props_()[key]),
      type: (key) => typeText(props_()[key], { collapsed: true }),
      itemsBase: (key) => describe(getAt(doc(), [...propPath(key), ...describe(props_()[key]).value, 'items']) || {}).base,
      summary: (key) => {
        const n = props_()[key];
        const dflt = isTopLevelProperty(propPath(key)) ? defaultSummary(n) : null;
        return dflt ? [...constraintSummary(n), dflt] : constraintSummary(n);
      },
      required: (key) => isRequired(doc(), props.path, key),
      fieldCount: (key) => Object.keys(propertiesOf(getAt(doc(), nested(key)) || {})).length,
      setType: (key, base) => edit(props.flow, (d) => setBaseType(d, propPath(key), base)),
      setItems: (key, base) => edit(props.flow, (d) => setBaseType(d, [...propPath(key), ...describe(getAt(d, propPath(key))).value, 'items'], base)),
      setNull: (key, on) => edit(props.flow, (d) => setNullable(d, propPath(key), on)),
      setReq: (key, on) => edit(props.flow, (d) => setRequired(d, props.path, key, on)),
      // A property's whole declaration goes with it (type, constraints, annotations, nested fields).
      remove: (key) => {
        if (!window.confirm(`Delete property "${key}" from the schema draft? Its type, constraints and annotations are removed with it.`)) return;
        edit(props.flow, (d) => removeProperty(d, props.path, key));
      },
      // A click on the row outside its fields and buttons opens or closes the detail form.
      rowClick: (key, event) => {
        if (event.target.closest('input, select, textarea, button, a, label')) return;
        open[key] = !open[key];
      },
    };
  },
  template: `
  <table class="schema-table">
    <thead><tr>
      <th>Property</th><th>Type</th>
      <th title="Required: every element of the label must have this key. Says nothing about its value: with Null checked as well the key must be there but its value may be null.">Req.</th>
      <th title="Nullable: the value may be null (the type includes null). Says nothing about whether the key must be there: that is Req. An absent key and a null value are different: property(k, null) stores the key with null.">Null</th>
      <th>Constraints</th><th class="actions"><span class="sr-only">Actions</span></th>
    </tr></thead>
    <tbody>
      <template v-for="(node, key) in props_()" :key="key">
        <tr class="prop-row" :class="{ open: open[key] }" :title="open[key] ? 'Click to close the details' : ''" @click="rowClick(key, $event)">
          <td><input type="text" class="cell-input name" :value="key" :aria-label="'Name of property ' + key" spellcheck="false"
                     @change="rename(key, $event.target.value)" @keydown.enter="$event.target.blur()"></td>
          <td class="type">
            <select :value="info(key).base" :aria-label="'Type of ' + key" @change="setType(key, $event.target.value)">
              <option v-if="info(key).base === 'union'" value="union" disabled>{{ type(key) }} (edit as JSON)</option>
              <option v-for="t in BASE_TYPES" :key="t" :value="t">{{ t }}</option>
            </select>
            <template v-if="info(key).base === 'array'">
              <span class="faint"> of </span>
              <select :value="itemsBase(key)" :aria-label="'Item type of ' + key" @change="setItems(key, $event.target.value)">
                <option v-if="itemsBase(key) === 'union'" value="union" disabled>union (edit as JSON)</option>
                <option v-for="t in BASE_TYPES" :key="t" :value="t">{{ t }}</option>
              </select>
            </template>
          </td>
          <td class="check"><input type="checkbox" :checked="required(key)" :aria-label="key + ' is required'" @change="setReq(key, $event.target.checked)"></td>
          <td class="check"><input type="checkbox" :checked="info(key).nullable" :disabled="['any', 'null', 'union'].includes(info(key).base)"
                                   :aria-label="key + ' admits null'" @change="setNull(key, $event.target.checked)"></td>
          <td class="constraints">
            <button class="link-btn" :aria-expanded="String(!!open[key])" :title="'Constraints and annotations of ' + key" @click="open[key] = !open[key]">
              <template v-if="summary(key).length"><code v-for="c in summary(key)" :key="c" class="schema-chip">{{ c }}</code></template>
              <span v-else class="faint">add…</span>
              <span v-if="node && (node.description || node.default !== undefined)" class="faint" title="Has annotations"> ✎</span>
              <span class="faint chevron" aria-hidden="true">{{ open[key] ? ' ▴' : ' ▾' }}</span>
            </button>
          </td>
          <td class="actions">
            <button class="icon-btn danger-hover" :title="'Delete property ' + key" :aria-label="'Delete property ' + key" @click="remove(key)" v-html="TRASH_ICON"></button>
          </td>
        </tr>
        <tr v-if="open[key]" class="details-row"><td colspan="6">
          <schema-node-details :flow="flow" :path="propPath(key)"></schema-node-details>
          <div class="details-close"><button class="btn small" @click="open[key] = false">▴ Close</button></div>
        </td></tr>
        <tr v-if="nested(key)" class="nested-row"><td colspan="6">
          <button class="link-btn nested-toggle" :aria-expanded="String(isExpanded(key))" @click="expanded[key] = !isExpanded(key)">
            {{ isExpanded(key) ? '▾' : '▸' }} {{ info(key).base === 'array' ? key + '[ ]' : key }}: object {{ '{' }}{{ fieldCount(key) }} field{{ fieldCount(key) === 1 ? '' : 's' }}{{ '}' }}
          </button>
          <schema-object-table v-if="isExpanded(key)" :flow="flow" :path="nested(key)" :depth="depth + 1"></schema-object-table>
        </td></tr>
      </template>
      <tr class="add-row"><td colspan="6">
        <form @submit.prevent="add">
          <input type="text" class="cell-input" v-model="draftName.value" placeholder="+ property" aria-label="New property name" spellcheck="false">
          <button class="btn small" type="submit" :disabled="!draftName.value.trim()">Add</button>
        </form>
      </td></tr>
    </tbody>
  </table>
  `,
};

/** One label: name (renamable), remove, connections (edges), additional properties, its property table. */
const SchemaLabel = {
  components: { SchemaObjectTable },
  props: {
    flow: { type: Object, required: true },
    kind: { type: String, required: true }, // 'vertex' | 'edge'
    name: { type: String, required: true },
  },
  setup(props) {
    const section = () => (props.kind === 'edge' ? 'edges' : 'vertices');
    const entry = () => getAt(props.flow.ed.working, [section(), props.name]) || {};
    const schemaPath = () => [section(), props.name, 'schema'];
    const vertexLabels = () => Object.keys(props.flow.ed.working.vertices || {});
    // A connection menu lists at most LABELS_SHOWN labels (a graph with 100 000 labels would
    // render 100 000 options per menu); the current value is always offered.
    const labelOptions = () => vertexLabels().slice(0, LABELS_SHOWN);
    const additional = () => {
      const s = entry().schema;
      const v = isPlainObject(s) ? s.additionalProperties : undefined;
      return v === true ? 'true' : v === false ? 'false' : 'default';
    };
    return {
      entry, schemaPath, vertexLabels, labelOptions, additional,
      connections: () => (Array.isArray(entry().connections) ? entry().connections : []),
      rename: (to) => edit(props.flow, (d) => renameLabel(d, props.kind, props.name, to)),
      remove: () => {
        if (!window.confirm(`Delete ${props.kind} label "${props.name}" from the schema draft? Its properties${props.kind === 'edge' ? ' and connections' : ''} are removed with it.`)) return;
        edit(props.flow, (d) => removeLabel(d, props.kind, props.name));
      },
      setAdditional: (choice) => edit(props.flow, (d) => {
        const s = getAt(d, schemaPath());
        if (!isPlainObject(s)) d[section()][props.name].schema = { type: 'object', properties: {} };
        setKeyword(d, schemaPath(), 'additionalProperties', choice === 'default' ? undefined : choice === 'true');
      }),
      addConn: () => edit(props.flow, (d) => addConnection(d, props.name)),
      setConn: (i, side, v) => edit(props.flow, (d) => setConnection(d, props.name, i, side, v)),
      removeConn: (i) => edit(props.flow, (d) => removeConnection(d, props.name, i)),
      TRASH_ICON,
    };
  },
  template: `
  <section class="schema-label" :aria-label="kind + ' label ' + name">
    <div class="schema-label-head">
      <span class="badge" :class="kind === 'edge' ? 'warn' : 'accent'">{{ kind }}</span>
      <input type="text" class="cell-input label-name" :value="name" :aria-label="'Name of ' + kind + ' label ' + name" spellcheck="false"
             @change="rename($event.target.value)" @keydown.enter="$event.target.blur()">
      <label class="faint" title="Whether properties the label does not declare are allowed">extra properties
        <select :value="additional()" @change="setAdditional($event.target.value)" :aria-label="'Additional properties of ' + name">
          <option value="default">mode default</option>
          <option value="true">allowed</option>
          <option value="false">forbidden</option>
        </select>
      </label>
      <span class="spacer"></span>
      <button class="icon-btn danger-hover" :title="'Delete ' + kind + ' label ' + name" :aria-label="'Delete ' + kind + ' label ' + name" @click="remove" v-html="TRASH_ICON"></button>
    </div>
    <div v-if="kind === 'edge'" class="schema-connections">
      <span class="faint">connections</span>
      <span v-if="!connections().length" class="faint">(none: every pair of labels)</span>
      <span v-for="(c, i) in connections()" :key="i" class="schema-connection">
        <select :value="c.from" :aria-label="'From label of connection ' + (i + 1)" @change="setConn(i, 'from', $event.target.value)">
          <option value="*">* (any)</option>
          <option v-if="c.from !== '*' && !labelOptions().includes(c.from)" :value="c.from">{{ c.from }}{{ vertexLabels().includes(c.from) ? '' : ' (undeclared)' }}</option>
          <option v-for="l in labelOptions()" :key="l" :value="l">{{ l }}</option>
        </select>
        <span aria-hidden="true">→</span>
        <select :value="c.to" :aria-label="'To label of connection ' + (i + 1)" @change="setConn(i, 'to', $event.target.value)">
          <option value="*">* (any)</option>
          <option v-if="c.to !== '*' && !labelOptions().includes(c.to)" :value="c.to">{{ c.to }}{{ vertexLabels().includes(c.to) ? '' : ' (undeclared)' }}</option>
          <option v-for="l in labelOptions()" :key="l" :value="l">{{ l }}</option>
        </select>
        <button class="icon-btn danger-hover" :aria-label="'Delete connection ' + (i + 1)" title="Delete this connection" @click="removeConn(i)" v-html="TRASH_ICON"></button>
      </span>
      <button class="btn small" @click="addConn">+ connection</button>
    </div>
    <div class="schema-label-body">
      <schema-object-table :flow="flow" :path="schemaPath()" :depth="0"></schema-object-table>
    </div>
  </section>
  `,
};

/**
 * Most labels per kind the Tables view renders at once (and the most a connection menu lists):
 * a schema inferred from a graph with one label per vertex has 100 000 labels, which would block
 * the page for minutes. "Show more" renders the next ones; the JSON tab has all of them.
 */
export const LABELS_SHOWN = 300;

/** The Tables view: settings (mode, meta), then the vertex and edge labels. */
export const SchemaTables = {
  components: { SchemaLabel },
  props: { flow: { type: Object, required: true } },
  setup(props) {
    const newLabel = reactive({ vertex: '', edge: '' });
    const shown = reactive({ vertex: LABELS_SHOWN, edge: LABELS_SHOWN });
    const meta = reactive({ text: null, error: null });
    const working = () => props.flow.ed.working;
    // The label names once per change of the working schema (the template reads them several
    // times per render; a schema can have 100 000 labels).
    const labelNames = computed(() => ({
      vertex: Object.keys(working().vertices || {}),
      edge: Object.keys(working().edges || {}),
    }));
    function add(kind) {
      const name = newLabel[kind].trim();
      if (!name) return;
      if (edit(props.flow, (d) => addLabel(d, kind, name)) !== undefined) newLabel[kind] = '';
    }
    const metaText = () => (meta.text !== null ? meta.text : working().meta === undefined ? '' : JSON.stringify(working().meta, null, 2));
    function setMeta(text) {
      meta.text = text;
      if (text.trim() === '') {
        meta.error = null;
        edit(props.flow, (d) => { delete d.meta; });
        meta.text = null;
        return;
      }
      let v;
      try {
        v = JSON.parse(text);
      } catch (err) {
        meta.error = `Not valid JSON: ${err.message}`;
        return;
      }
      if (!isPlainObject(v)) {
        meta.error = 'meta is a JSON object';
        return;
      }
      meta.error = null;
      edit(props.flow, (d) => { d.meta = v; });
      meta.text = null;
    }
    return {
      newLabel, add, meta, metaText, setMeta, MODES, shown, LABELS_SHOWN,
      labels: (kind) => labelNames.value[kind],
    };
  },
  template: `
  <div class="schema-doc">
    <details class="schema-settings">
      <summary>meta <span class="faint">(free-form JSON the engine stores and never interprets)</span></summary>
      <textarea class="code-input" rows="4" spellcheck="false" :class="{ invalid: meta.error }" :value="metaText()"
                aria-label="meta as JSON" placeholder='{"version": "1.0"}' @change="setMeta($event.target.value)"></textarea>
      <div v-if="meta.error" class="field-error" role="alert">{{ meta.error }} (the draft keeps the last valid meta)</div>
    </details>
    <template v-for="kind in ['vertex', 'edge']" :key="kind">
      <div class="schema-section-head">
        <h3>{{ kind === 'vertex' ? 'Vertex labels' : 'Edge labels' }} <span class="faint">({{ labels(kind).length }})</span></h3>
        <form @submit.prevent="add(kind)">
          <input type="text" v-model="newLabel[kind]" :placeholder="'+ ' + kind + ' label'" :aria-label="'New ' + kind + ' label'" spellcheck="false">
          <button class="btn small" type="submit" :disabled="!newLabel[kind].trim()">Add</button>
        </form>
      </div>
      <p v-if="!labels(kind).length" class="faint schema-empty">No {{ kind }} labels declared.</p>
      <schema-label v-for="name in labels(kind).slice(0, shown[kind])" :key="kind + ':' + name" :flow="flow" :kind="kind" :name="name"></schema-label>
      <p v-if="labels(kind).length > shown[kind]" class="faint schema-empty">
        Showing {{ shown[kind] }} of {{ labels(kind).length }} {{ kind }} labels.
        <a href="#" @click.prevent="shown[kind] += LABELS_SHOWN">Show {{ Math.min(LABELS_SHOWN, labels(kind).length - shown[kind]) }} more</a>
        (the JSON tab has all of them).
      </p>
    </template>
  </div>
  `,
};

const KIND_TEXT = (k) => String(k || '').replace(/_/g, ' ');

/**
 * The review of the draft: the diff (What changes), the check against the data (violations by
 * rule; a click opens a query with the offending elements), the backfill hint, errors.
 */
export const SchemaReview = {
  props: { flow: { type: Object, required: true } },
  emits: ['show-violation', 'backfill'],
  setup(props) {
    const canBackfill = () => !!(props.flow.freshCheck && props.flow.ed.draft &&
      backfillScript(props.flow.ed.draft, props.flow.freshCheck.violations.groups));
    const where = (c) => [c.element, c.label, c.path].filter((x) => x !== null && x !== undefined && x !== '').join(' ');
    const beforeAfter = (c) => (c.before !== null && c.before !== undefined) || (c.after !== null && c.after !== undefined)
      ? `${c.before ?? '—'} → ${c.after ?? '—'}` : '';
    const groupWhere = (g) => [g.element, g.label, g.field].filter((x) => x).join(' ');
    return { KIND_TEXT, where, beforeAfter, groupWhere, canBackfill };
  },
  template: `
  <div class="schema-review" aria-live="polite">
    <div v-if="flow.applyError" class="schema-error" role="alert"><b>Apply failed.</b><pre>{{ flow.applyError }}</pre></div>
    <div v-if="flow.freezeError" class="schema-error" role="alert"><b>Freeze failed: nothing changed.</b><pre>{{ flow.freezeError }}</pre></div>
    <div v-if="flow.freshDiffError" class="schema-error" role="alert"><b>The draft is not a valid schema.</b><pre>{{ flow.freshDiffError }}</pre></div>
    <div v-if="flow.freshCheckError" class="schema-error" role="alert"><b>The check failed.</b><pre>{{ flow.freshCheckError }}</pre></div>
    <div v-if="flow.ed.jsonError" class="schema-error" role="alert">The JSON tab holds invalid JSON ({{ flow.ed.jsonError }}).
      The draft is its last valid state; fix the text, or edit the tables (an edit rewrites the text).</div>
    <div v-if="flow.ed.draft && !flow.ed.dirty" class="schema-note">The draft equals the stored schema: nothing to apply.</div>
    <div v-if="flow.freshDiff" class="schema-block">
      <div class="schema-block-head">
        <b>What changes</b>
        <span v-if="!flow.freshDiff.changes.length" class="faint">nothing</span>
        <span v-else-if="flow.freshDiff.compatible" class="badge ok">backward compatible</span>
        <span v-else class="badge danger">not backward compatible</span>
      </div>
      <ul class="schema-list">
        <li v-for="(c, i) in flow.freshDiff.changes" :key="i">
          <span class="badge" :class="c.compatible ? 'ok' : 'danger'" :title="c.compatible ? 'Backward compatible: data and clients written against the old schema keep working (it only adds or relaxes; Check against data finds existing data that conflicts)' : 'Incompatible: it removes a declaration or tightens a rule, so clients written against the old schema may fail'">{{ c.compatible ? 'ok' : '!' }}</span>
          <span class="kind">{{ KIND_TEXT(c.kind) }}</span>
          <code v-if="where(c)">{{ where(c) }}</code>
          <span v-if="beforeAfter(c)" class="faint">{{ beforeAfter(c) }}</span>
        </li>
      </ul>
    </div>
    <div v-if="flow.freshCheck" class="schema-block">
      <div class="schema-block-head">
        <b>Check against data</b>
        <span v-if="!flow.freshCheck.violations.total_violations" class="badge ok">no violations in {{ flow.freshCheck.violations.elements_checked }} elements</span>
        <span v-else class="badge danger">{{ flow.freshCheck.violations.total_violations }} violations on {{ flow.freshCheck.violations.violating_elements }} of {{ flow.freshCheck.violations.elements_checked }} elements</span>
      </div>
      <ul v-if="flow.freshCheck.violations.groups.length" class="schema-list">
        <li v-for="(g, i) in flow.freshCheck.violations.groups" :key="i">
          <button class="link-btn" :title="'Open a query that shows these elements (new tab)'" @click="$emit('show-violation', g)">
            <span class="badge danger">{{ g.count }}</span>
            <span class="kind">{{ KIND_TEXT(g.kind) }}</span>
            <code>{{ groupWhere(g) }}</code>
            <span v-if="g.example_field" class="faint">first at {{ g.example_field }}</span>
            <span v-if="g.detail" class="faint">{{ g.detail }}</span>
            <span class="faint">▸ show</span>
          </button>
        </li>
      </ul>
      <p v-if="flow.freshCheck.violations.omitted_violations" class="faint">{{ flow.freshCheck.violations.omitted_violations }} further violations are only counted.</p>
      <div v-if="flow.blocked" class="schema-hint">
        <b>Apply is blocked</b> while the data violates the draft. Fix or migrate the data, or relax the draft (make a key
        optional, admit null, declare the label or property, a less strict mode). To make a key required on existing
        data: <b>backfill first, then require it, in one run</b>.
        <button v-if="canBackfill()" class="btn small" @click="$emit('backfill')" title="A script that sets the draft with mode none, fills the missing keys, then sets the draft (one run: all or nothing)">Open a backfill script</button>
      </div>
    </div>
  </div>
  `,
};
