<script setup lang="ts">
import { nextTick, onBeforeUnmount, onMounted, reactive, ref, watch } from "vue";

/** 横向 tab 条：共享下划线 translateX+width 滑向激活项（.tab-ink 全局样式）。 */
const props = defineProps<{
  tabs: { key: string; label: string; count?: number; dimmed?: boolean }[];
  modelValue: string;
}>();
const emit = defineEmits<{ "update:modelValue": [string] }>();

const barRef = ref<HTMLElement | null>(null);
const ink = reactive({ x: 0, w: 0, on: false });

function updateInk() {
  const bar = barRef.value;
  const active = bar?.querySelector<HTMLElement>("[data-active]");
  if (!bar || !active) {
    ink.on = false;
    return;
  }
  ink.x = active.offsetLeft;
  ink.w = active.offsetWidth;
  ink.on = true;
}

let ro: ResizeObserver | null = null;
watch(() => [props.modelValue, props.tabs.length], () => nextTick(updateInk));
onMounted(() => {
  updateInk();
  ro = new ResizeObserver(updateInk);
  if (barRef.value) ro.observe(barRef.value);
});
onBeforeUnmount(() => ro?.disconnect());
</script>

<template>
  <div ref="barRef" class="relative flex shrink-0 border-b px-5">
    <span
      class="tab-ink"
      :style="{ transform: `translateX(${ink.x}px)`, width: `${ink.w}px`, opacity: ink.on ? 1 : 0 }"
    />
    <button
      v-for="t in tabs"
      :key="t.key"
      type="button"
      :data-active="t.key === modelValue || undefined"
      class="flex min-w-0 items-center gap-1.5 whitespace-nowrap px-3 py-2.5 text-sm transition-colors focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
      :class="[
        t.key === modelValue
          ? 'font-semibold text-foreground'
          : 'font-medium text-muted-foreground hover:text-foreground',
        t.dimmed ? 'opacity-50' : '',
      ]"
      @click="emit('update:modelValue', t.key)"
    >
      {{ t.label }}
      <span v-if="t.count !== undefined" class="text-xs tabular-nums text-muted-foreground/70">{{ t.count }}</span>
    </button>
  </div>
</template>
