<script setup lang="ts">
import { json } from "@codemirror/lang-json";
import { lua } from "@codemirror/legacy-modes/mode/lua";
import { StreamLanguage } from "@codemirror/language";
import { EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { oneDark } from "@codemirror/theme-one-dark";
import { basicSetup } from "codemirror";
import { onMounted, onUnmounted, ref, watch } from "vue";

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

onMounted(() => {
  view = new EditorView({
    doc: props.modelValue,
    parent: host.value!,
    extensions: [
      basicSetup,
      props.lang === "json" ? json() : StreamLanguage.define(lua),
      oneDark,
      EditorView.theme({
        // 不用百分比高度：宿主为 flex 容器，编辑器用 flex:1 填满——grid/flex 混合
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
