<script setup lang="ts">
import { Check, Minus } from "lucide-vue-next";
import { ref, watchEffect, type HTMLAttributes } from "vue";

import { cn } from "@/lib/utils";

defineOptions({ inheritAttrs: false });

/**
 * 自绘外观的原生 checkbox：保留 label for/键盘交互/表单语义，外观对齐设计 token。
 * 半选态（indeterminate）是 DOM 属性而非 attribute，watchEffect 负责写回元素。
 */
const props = withDefaults(
  defineProps<{
    checked?: boolean;
    indeterminate?: boolean;
    disabled?: boolean;
    class?: HTMLAttributes["class"];
  }>(),
  { checked: false, indeterminate: false, disabled: false },
);
const emit = defineEmits<{ "update:checked": [boolean] }>();

const el = ref<HTMLInputElement | null>(null);
watchEffect(() => {
  if (el.value) el.value.indeterminate = props.indeterminate;
});
</script>

<template>
  <span class="relative inline-flex size-4 shrink-0 items-center">
    <input
      ref="el"
      v-bind="$attrs"
      type="checkbox"
      :checked="props.checked"
      :disabled="props.disabled"
      :class="
        cn(
          'peer size-4 appearance-none rounded-sm border border-input bg-transparent transition-colors checked:border-primary checked:bg-primary indeterminate:border-primary indeterminate:bg-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-1 focus-visible:ring-offset-background disabled:cursor-not-allowed disabled:opacity-50',
          props.class,
        )
      "
      @change="emit('update:checked', ($event.target as HTMLInputElement).checked)"
    />
    <Check
      v-if="!props.indeterminate"
      class="pointer-events-none absolute inset-0 m-auto size-3 text-primary-foreground opacity-0 transition-opacity peer-checked:opacity-100"
    />
    <Minus v-else class="pointer-events-none absolute inset-0 m-auto size-3 text-primary-foreground" />
  </span>
</template>
