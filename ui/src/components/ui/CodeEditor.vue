<script setup lang="ts">
import { json } from "@codemirror/lang-json";
import { lua } from "@codemirror/legacy-modes/mode/lua";
import { StreamLanguage } from "@codemirror/language";
import { Compartment, EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { oneDark } from "@codemirror/theme-one-dark";
import { basicSetup } from "codemirror";
import { onMounted, onUnmounted, ref, watch } from "vue";

import { isDark } from "@/lib/theme";

const props = withDefaults(
  defineProps<{
    modelValue: string;
    /** 编辑器高度（CSS 值）；"100%" 表示由外部 flex 布局决定。 */
    height?: string;
    readonly?: boolean;
    /** 语言：默认 lua。 */
    lang?: "lua" | "json";
  }>(),
  { height: "20rem", readonly: false, lang: "lua" },
);

const emit = defineEmits<{ "update:modelValue": [string] }>();

const host = ref<HTMLElement | null>(null);
let view: EditorView | null = null;

/** 浅色主题：透明底接入卡片背景，语法高亮走 basicSetup 内置的 defaultHighlightStyle 回退。 */
const lightTheme = EditorView.theme(
  {
    "&": { backgroundColor: "transparent", color: "hsl(var(--foreground))" },
    ".cm-gutters": {
      backgroundColor: "transparent",
      color: "hsl(var(--muted-foreground))",
      border: "none",
    },
    ".cm-activeLine, .cm-activeLineGutter": {
      backgroundColor: "hsl(var(--muted) / 0.6)",
    },
    "&.cm-focused .cm-selectionBackground, & .cm-selectionBackground": {
      backgroundColor: "hsl(var(--accent)) !important",
    },
  },
  { dark: false },
);

/** 主题用 Compartment 隔离：App 主题切换时只重配这一格，不重建编辑器。 */
const colorTheme = new Compartment();

onMounted(() => {
  view = new EditorView({
    doc: props.modelValue,
    parent: host.value!,
    extensions: [
      basicSetup,
      props.lang === "json" ? json() : StreamLanguage.define(lua),
      colorTheme.of(isDark.value ? oneDark : lightTheme),
      EditorView.theme({
        // 不用百分比高度：外层是 flex 容器，编辑器用 flex:1 填满——grid/flex 混合
        // 布局下百分比高度可能无法解析，导致编辑器退化到内容高度。
        "&": { flex: 1, minWidth: 0, fontSize: "12px" },
        ".cm-scroller": {
          fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
          lineHeight: "1.7",
        },
      }),
      EditorState.readOnly.of(props.readonly),
      EditorView.updateListener.of((u) => {
        if (u.docChanged) emit("update:modelValue", u.state.doc.toString());
      }),
    ],
  });
});

// 应用主题切换 → 只重配色主题一格，文档/选区不动
watch(isDark, (d) => {
  view?.dispatch({ effects: colorTheme.reconfigure(d ? oneDark : lightTheme) });
});

// 外部值变化时同步进编辑器（避免光标跳动，仅在内容确实不同时分派）
watch(
  () => props.modelValue,
  (v) => {
    if (view && v !== view.state.doc.toString()) {
      view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: v } });
    }
  },
);

onUnmounted(() => view?.destroy());
</script>

<template>
  <div
    ref="host"
    class="flex overflow-hidden rounded-md border border-input"
    :style="height === '100%' ? undefined : { height }"
  />
</template>
