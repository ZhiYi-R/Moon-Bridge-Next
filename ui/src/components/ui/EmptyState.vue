<script setup lang="ts">
import type { Component, HTMLAttributes } from "vue";

import { cn } from "@/lib/utils";

/** 虚线空态框：可选图标 + 正文 + action 槽（CTA 按钮）。 */
const props = defineProps<{
  icon?: Component;
  class?: HTMLAttributes["class"];
}>();
</script>

<template>
  <div
    :class="
      cn('rounded-md border border-dashed p-8 text-center text-sm text-muted-foreground', props.class)
    "
  >
    <component :is="props.icon" v-if="props.icon" class="mx-auto mb-3 size-6 opacity-50" />
    <slot />
    <div v-if="$slots.action" class="mt-4 flex items-center justify-center gap-2">
      <slot name="action" />
    </div>
  </div>
</template>
