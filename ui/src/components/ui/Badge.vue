<script setup lang="ts">
import type { HTMLAttributes } from "vue";
import { cva, type VariantProps } from "class-variance-authority";

import { cn } from "@/lib/utils";

const badgeVariants = cva(
  "inline-flex items-center rounded-md border px-2 py-0.5 text-xs font-medium transition-colors focus:outline-none",
  {
    variants: {
      variant: {
        default: "border-transparent bg-primary text-primary-foreground",
        secondary: "border-transparent bg-secondary text-secondary-foreground",
        /* 语义彩色克制使用：低透明度背景 + 降低明度的前景 */
        destructive: "border-transparent bg-destructive/10 text-destructive",
        outline: "text-foreground",
        success: "border-transparent bg-emerald-500/10 text-emerald-500",
        warning: "border-transparent bg-amber-500/10 text-amber-500",
      },
    },
    defaultVariants: { variant: "default" },
  },
);

type BadgeVariantProps = VariantProps<typeof badgeVariants>;

interface Props {
  variant?: BadgeVariantProps["variant"];
  class?: HTMLAttributes["class"];
}

const props = withDefaults(defineProps<Props>(), { variant: "default" });
</script>

<template>
  <span :class="cn(badgeVariants({ variant: props.variant }), props.class)">
    <slot />
  </span>
</template>
