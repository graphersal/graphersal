// A plain table for {columns, rows}: sticky header, row numbers, typed cell styling. A long text or
// a long list of numbers is collapsed with an expand button (components/value-text.js); the
// engine's vector marker keeps its own disclosure (the JSON view has every value).

import { formatValue, isNonFiniteCell, isVectorCell, vectorPreview, vectorSummary } from '../util.js';
import { ValueText } from './value-text.js';

export const DataTable = {
  components: { ValueText },
  props: {
    table: { type: Object, required: true },
    rowNumbers: { type: Boolean, default: true },
    /**
     * An action offered on some rows (`{label, title, applies(row, columns), run(row, columns)}`):
     * a small button next to the row number.
     */
    rowAction: { type: Object, default: null },
  },
  setup() {
    function cellClass(v) {
      if (v === null || v === undefined) return 'cell-null';
      if (isVectorCell(v)) return 'cell-vector';
      // NaN / Infinity / -Infinity (the engine's typed double) is a number.
      if (isNonFiniteCell(v)) return 'num';
      if (typeof v === 'number') return 'num';
      if (typeof v === 'boolean') return 'cell-bool';
      if (typeof v === 'object') return 'cell-json';
      return '';
    }
    function cellText(v) {
      if (v === undefined) return '';
      return formatValue(v);
    }
    return { cellClass, cellText, isVectorCell, vectorPreview, vectorSummary };
  },
  template: `
  <div class="data-table-wrap">
    <table class="data-table" v-if="table.columns.length">
      <thead>
        <tr>
          <th v-if="rowNumbers" aria-label="Row"></th>
          <th v-for="c in table.columns" :key="c">{{ c }}</th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="(row, i) in table.rows" :key="i">
          <td v-if="rowNumbers" class="rownum">{{ i + 1 }}<button v-if="rowAction && rowAction.applies(row, table.columns)" type="button"
              class="row-action" :title="rowAction.title" :aria-label="rowAction.title" @click="rowAction.run(row, table.columns)">{{ rowAction.label }}</button></td>
          <td v-for="(v, j) in row" :key="j" :class="cellClass(v)" :title="cellText(v)">
            <details v-if="isVectorCell(v)"><summary>{{ vectorSummary(v) }}</summary><span class="mono">{{ vectorPreview(v) }}</span></details>
            <value-text v-else-if="v !== undefined" :value="v" :name="table.columns[j] || 'value'"></value-text>
          </td>
        </tr>
      </tbody>
    </table>
    <div v-else class="placeholder">No rows.</div>
  </div>
  `,
};
