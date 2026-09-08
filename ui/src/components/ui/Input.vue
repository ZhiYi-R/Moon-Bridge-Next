<script setup lang="ts">
import type { HTMLAttributes } from "vue";

import { cn } from "@/lib/utils";

defineOptions({ inheritAttrs: false });

interface Props {
  class?: HTMLAttributes["class"];
  modelValue?: string | number | null;
}

const props = defineProps<Props>();
const emit = defineEmits<{ "update:modelValue": [value: string] }>();

function onInput(e: Event) {
  emit("update:modelValue", (e.target as HTMLInputElement).value);
}
</script>

<template>
  <input
    v-bind="$attrs"
    :value="props.modelValue ?? ''"
    :class="
      cn(
        'flex h-9 w-full rounded-md border border-input bg-transparent px-3 py-1 text-sm shadow-sm transition-colors file:border-0 file:bg-transparent file:text-sm file:font-medium placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-50',
        props.class,
      )
    "
    @input="onInput"
  />
</template>
