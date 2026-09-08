<script setup lang="ts">
import { Check, ChevronDown } from "lucide-vue-next";
import { computed, onMounted, onUnmounted, ref } from "vue";
import { cn } from "@/lib/utils";

export interface SelectOption {
  value: string;
  label: string;
}

const props = withDefaults(
  defineProps<{
    modelValue: string;
    options: SelectOption[];
    placeholder?: string;
    disabled?: boolean;
    /** 紧凑样式（用于节头内嵌）。 */
    small?: boolean;
  }>(),
  { placeholder: "", disabled: false, small: false },
);

const emit = defineEmits<{ "update:modelValue": [string] }>();

const open = ref(false);
const rootRef = ref<HTMLElement | null>(null);
const buttonRef = ref<HTMLButtonElement | null>(null);
/** 下拉浮层的 fixed 定位样式（打开时计算，随按钮宽度对齐）。 */
const floatStyle = ref<{ top: string; left: string; width: string; bottom?: string }>({
  top: "0",
  left: "0",
  width: "0",
});
/** 键盘高亮索引。 */
const active = ref(0);

const selected = computed(() => props.options.find((o) => o.value === props.modelValue));
const display = computed(() => selected.value?.label ?? "");

function toggle() {
  if (props.disabled) return;
  open.value ? close() : show();
}

function show() {
  const el = buttonRef.value;
  if (!el) return;
  const r = el.getBoundingClientRect();
  const below = window.innerHeight - r.bottom;
  // 下方放不下时向上展开
  const openUp = below < 180 && r.top > 180;
  floatStyle.value = {
    left: `${r.left}px`,
    width: `${r.width}px`,
    top: openUp ? "" : `${r.bottom + 4}px`,
    bottom: openUp ? `${window.innerHeight - r.top + 4}px` : "",
  };
  active.value = Math.max(0, props.options.findIndex((o) => o.value === props.modelValue));
  open.value = true;
}

function close() {
  open.value = false;
}

function pick(v: string) {
  emit("update:modelValue", v);
  close();
  buttonRef.value?.focus();
}

function onKeydown(e: KeyboardEvent) {
  if (!open.value) {
    if (["Enter", " ", "ArrowDown", "ArrowUp"].includes(e.key)) {
      e.preventDefault();
      show();
    }
    return;
  }
  if (e.key === "Escape") {
    e.preventDefault();
    close();
  } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
    e.preventDefault();
    const d = e.key === "ArrowDown" ? 1 : -1;
    active.value = (active.value + d + props.options.length) % props.options.length;
  } else if (e.key === "Enter" || e.key === " ") {
    e.preventDefault();
    const opt = props.options[active.value];
    if (opt) pick(opt.value);
  } else if (e.key === "Tab") {
    close();
  }
}

function onDocClick(e: MouseEvent) {
  if (open.value && rootRef.value && !rootRef.value.contains(e.target as Node)) close();
}

onMounted(() => {
  document.addEventListener("click", onDocClick, true);
  window.addEventListener("resize", close);
  window.addEventListener("scroll", close, true);
});
onUnmounted(() => {
  document.removeEventListener("click", onDocClick, true);
  window.removeEventListener("resize", close);
  window.removeEventListener("scroll", close, true);
});
</script>

<template>
  <div ref="rootRef" class="relative">
    <button
      ref="buttonRef"
      type="button"
      role="combobox"
      :aria-expanded="open"
      :disabled="disabled"
      :title="display || placeholder"
      :class="
        cn(
          'flex items-center justify-between gap-2 rounded-md border border-input bg-transparent text-sm shadow-sm transition-colors focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-50',
          small ? 'h-7 px-2 text-xs' : 'h-9 w-full px-3',
          !small && 'w-full',
        )
      "
      @click="toggle"
      @keydown="onKeydown"
    >
      <span class="min-w-0 truncate" :class="display ? '' : 'text-muted-foreground'">{{ display || placeholder || "请选择" }}</span>
      <ChevronDown class="size-4 shrink-0 text-muted-foreground" :class="open && 'rotate-180'" />
    </button>
  </div>

  <Teleport to="body">
    <div
      v-if="open"
      class="fixed z-[60] max-h-60 overflow-y-auto rounded-md border bg-popover p-1 shadow-md scrollbar-thin"
      :style="floatStyle"
    >
      <button
        v-for="(o, i) in options"
        :key="o.value"
        type="button"
        :class="
          cn(
            'flex w-full items-center justify-between gap-2 rounded-sm px-2 py-1.5 text-left text-sm',
            i === active && 'bg-accent text-accent-foreground',
          )
        "
        @click="pick(o.value)"
        @mouseenter="active = i"
      >
        <span class="truncate">{{ o.label }}</span>
        <Check v-if="o.value === modelValue" class="size-3.5 shrink-0" />
      </button>
    </div>
  </Teleport>
</template>
