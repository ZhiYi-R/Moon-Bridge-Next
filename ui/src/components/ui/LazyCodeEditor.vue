<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from "vue";

import CodeEditor from "./CodeEditor.vue";

/** 惰性挂载的 CodeEditor：进入视口前只渲染等高占位块。
 *  一页多个只读编辑器（如 trace 各阶段报文）时，CodeMirror 初始化
 *  摊到滚动命中处，打开页面不再为屏外内容付全价。 */
const props = withDefaults(
  defineProps<{
    modelValue: string;
    height?: string;
    readonly?: boolean;
    lang?: "lua" | "json";
    /** 距视口边缘多少 px 提前挂载。 */
    margin?: number;
  }>(),
  { height: "20rem", readonly: false, lang: "lua", margin: 300 },
);

const emit = defineEmits<{ "update:modelValue": [string] }>();

const host = ref<HTMLElement | null>(null);
const shown = ref(false);
let io: IntersectionObserver | null = null;

onMounted(() => {
  io = new IntersectionObserver(
    (entries) => {
      if (!entries.some((e) => e.isIntersecting)) return;
      shown.value = true;
      io?.disconnect();
      io = null;
    },
    { rootMargin: `${props.margin}px` },
  );
  if (host.value) io.observe(host.value);
});

onBeforeUnmount(() => io?.disconnect());
</script>

<template>
  <div ref="host">
    <CodeEditor
      v-if="shown"
      :model-value="modelValue"
      :height="height"
      :readonly="readonly"
      :lang="lang"
      @update:model-value="emit('update:modelValue', $event)"
    />
    <div
      v-else
      class="rounded-md border border-input bg-muted/30"
      :style="{ height }"
    />
  </div>
</template>
