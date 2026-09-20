<script setup lang="ts">
import { AlertCircle, TriangleAlert } from "lucide-vue-next";
import type { Component, HTMLAttributes } from "vue";

import { cn } from "@/lib/utils";

export type AlertVariant = "destructive" | "warning";

const props = withDefaults(
  defineProps<{ variant?: AlertVariant; class?: HTMLAttributes["class"] }>(),
  { variant: "destructive" },
);

const styles: Record<AlertVariant, string> = {
  destructive: "border-destructive/40 bg-destructive/10 text-destructive",
  warning: "border-amber-500/40 bg-amber-500/10 text-amber-600 dark:text-amber-400",
};
const icons: Record<AlertVariant, Component> = {
  destructive: AlertCircle,
  warning: TriangleAlert,
};
</script>

<template>
  <div
    role="alert"
    :class="
      cn('flex items-start gap-2 rounded-md border px-4 py-2 text-sm', styles[props.variant], props.class)
    "
  >
    <component :is="icons[props.variant]" class="mt-0.5 size-4 shrink-0" />
    <div class="min-w-0 flex-1"><slot /></div>
  </div>
</template>
