<script setup lang="ts">
import type { HTMLAttributes } from "vue";

import { cn } from "@/lib/utils";

export interface SegmentOption {
  value: string;
  label: string;
}

/** 分段选择器：默认紧凑内联（卡片工具区），fill=true 时撑满容器（表单场景）。 */
const props = withDefaults(
  defineProps<{
    modelValue: string;
    options: SegmentOption[];
    fill?: boolean;
    /** 每项按钮的 title 前缀，如「显示样式」→「显示样式：自动」 */
    titlePrefix?: string;
    class?: HTMLAttributes["class"];
  }>(),
  { fill: false },
);

const emit = defineEmits<{ "update:modelValue": [string] }>();
</script>

<template>
  <div :class="cn('flex items-center rounded-md border p-0.5', props.class)" role="radiogroup">
    <button
      v-for="o in props.options"
      :key="o.value"
      type="button"
      role="radio"
      :aria-checked="o.value === props.modelValue"
      :title="props.titlePrefix ? `${props.titlePrefix}：${o.label}` : undefined"
      :class="
        cn(
          'rounded transition-colors',
          props.fill ? 'flex-1 px-2 py-1 text-xs' : 'px-1.5 py-0.5 text-[11px]',
          o.value === props.modelValue
            ? 'bg-accent text-accent-foreground'
            : 'text-muted-foreground hover:bg-accent/60',
        )
      "
      @click="emit('update:modelValue', o.value)"
    >
      {{ o.label }}
    </button>
  </div>
</template>
