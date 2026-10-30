// The process memory bar (`MemoryBar`, prop `stats` = a `statistics()` answer): the process
// against its memory ceiling, one coloured segment per place the memory goes, a legend with every
// segment's size (a tiny segment stays readable there) and the compression saving. ONE component
// for both places that show memory: the Profile panel's Statistics tab and Optimize -> Memory.
// The model is js/memory-bar.js; the figures come from Rust (`process.memory`).

import { computed } from 'vue';
import { memoryBarModel } from '../memory-bar.js';

export const MemoryBar = {
  props: { stats: { type: Object, default: null } },
  setup(props) {
    const model = computed(() => memoryBarModel(props.stats));
    return { model };
  },
  template: `
  <div v-if="model" class="mem-bar">
    <div class="mem-bar-head">
      <span>Process memory <b>{{ model.usedText }}</b>
        <template v-if="!model.noCeiling"> of <b>{{ model.limitText }}</b></template>
        <span class="muted"> ({{ model.noCeiling ? 'no ceiling: ' + model.sourceText : model.sourceText }})</span></span>
      <span v-if="!model.noCeiling" :class="['mem-bar-pct', 'mem-' + model.level]"
        :title="model.notTracked ? 'Of the known part: the process total is not tracked' : 'Of the process memory ceiling (memory.limit)'">{{ model.percentText }}</span>
    </div>
    <div class="mem-bar-track" role="img" :aria-label="model.label">
      <span v-for="s in model.segments" :key="s.name" class="mem-bar-seg"
        :style="{ width: s.share + '%', minWidth: s.bytes > 0 ? '2px' : '0', background: s.colour }" :title="s.name + ' ' + s.text"></span>
    </div>
    <ul class="mem-bar-legend">
      <li v-for="s in model.segments" :key="s.name" :title="s.hint">
        <span class="mem-bar-swatch" :style="{ background: s.colour }"></span>{{ s.name }} <b>{{ s.text }}</b>
      </li>
    </ul>
    <p v-if="model.savingText" class="muted mem-bar-note">{{ model.savingText }}</p>
    <p v-if="model.notTracked" class="muted mem-bar-note">Process total not tracked: only the known segments are shown<template v-if="!model.noCeiling">, the percentage is of the known part</template>.</p>
    <p v-if="model.estimateAboveHeap" class="muted mem-bar-note">The graph's estimate is above the live heap; "other" is shown as 0.</p>
  </div>
  <p v-else-if="stats && stats.process && stats.process.memory && stats.process.memory.limitError" class="muted">Process memory ceiling unavailable: {{ stats.process.memory.limitError }}</p>
  `,
};
