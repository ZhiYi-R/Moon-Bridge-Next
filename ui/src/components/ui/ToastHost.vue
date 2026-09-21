<script setup lang="ts">
import { AlertCircle, CheckCircle2, Info, X } from "lucide-vue-next";

import { useToast, type ToastType } from "@/composables/useToast";

const { toasts, dismiss } = useToast();

const icons: Record<ToastType, unknown> = {
  success: CheckCircle2,
  error: AlertCircle,
  info: Info,
};
const iconCls: Record<ToastType, string> = {
  success: "text-emerald-600 dark:text-emerald-400",
  error: "text-destructive",
  info: "text-muted-foreground",
};
</script>

<template>
  <Teleport to="body">
    <div class="pointer-events-none fixed bottom-4 right-4 z-[100] flex w-80 flex-col gap-2">
      <TransitionGroup name="toast">
        <div
          v-for="t in toasts"
          :key="t.id"
          class="glass pointer-events-auto flex items-start gap-2 rounded-md border px-3 py-2.5 shadow-lg"
          role="status"
        >
          <component :is="icons[t.type]" class="mt-0.5 size-4 shrink-0" :class="iconCls[t.type]" />
          <p class="min-w-0 flex-1 break-words text-sm leading-snug">{{ t.message }}</p>
          <button
            class="shrink-0 text-muted-foreground transition-colors hover:text-foreground"
            title="关闭"
            @click="dismiss(t.id)"
          >
            <X class="size-3.5" />
          </button>
        </div>
      </TransitionGroup>
    </div>
  </Teleport>
</template>

<style scoped>
.toast-enter-active,
.toast-leave-active {
  transition:
    opacity var(--dur-base) var(--ease-out),
    transform var(--dur-base) var(--ease-out);
}
.toast-leave-active {
  transition-duration: calc(var(--dur-base) * 0.75);
}
.toast-enter-from,
.toast-leave-to {
  opacity: 0;
  transform: translateY(8px);
}
.toast-move {
  transition: transform var(--dur-base) var(--ease-out);
}
</style>
