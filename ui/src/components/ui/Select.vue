<script setup lang="ts">
import { Check, ChevronDown, Search } from "lucide-vue-next";
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from "vue";
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
    /** 大选项集时开启：浮层顶部显示搜索框，最多渲染 100 行，避免上万选项卡顿。 */
    searchable?: boolean;
  }>(),
  { placeholder: "", disabled: false, small: false, searchable: false },
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
const query = ref("");
const popoverRef = ref<HTMLElement | null>(null);
const searchRef = ref<HTMLInputElement | null>(null);
/** 浮层单次最多渲染的选项行数 */
const RENDER_LIMIT = 100;

const filteredOptions = computed(() => {
  if (!props.searchable) return props.options;
  const q = query.value.trim().toLowerCase();
  if (!q) return props.options;
  return props.options.filter((o) => o.label.toLowerCase().includes(q));
});
const visibleOptions = computed(() => filteredOptions.value.slice(0, RENDER_LIMIT));

watch(query, () => {
  active.value = 0;
});

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
  active.value = Math.max(0, visibleOptions.value.findIndex((o) => o.value === props.modelValue));
  open.value = true;
  if (props.searchable) void nextTick(() => searchRef.value?.focus());
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
    if (visibleOptions.value.length === 0) return;
    const d = e.key === "ArrowDown" ? 1 : -1;
    active.value = (active.value + d + visibleOptions.value.length) % visibleOptions.value.length;
  } else if (e.key === "Enter" || e.key === " ") {
    e.preventDefault();
    const opt = visibleOptions.value[active.value];
    if (opt) pick(opt.value);
  } else if (e.key === "Tab") {
    close();
  }
}

/** 搜索框内的键盘导航（高亮索引对应可见选项） */
function onSearchKeydown(e: KeyboardEvent) {
  if (e.key === "ArrowDown" || e.key === "ArrowUp") {
    e.preventDefault();
    if (visibleOptions.value.length === 0) return;
    const d = e.key === "ArrowDown" ? 1 : -1;
    active.value = (active.value + d + visibleOptions.value.length) % visibleOptions.value.length;
  } else if (e.key === "Enter") {
    e.preventDefault();
    const opt = visibleOptions.value[active.value];
    if (opt) pick(opt.value);
  } else if (e.key === "Escape") {
    e.preventDefault();
    close();
    buttonRef.value?.focus();
  }
}

function onDocClick(e: MouseEvent) {
  const t = e.target as Node;
  if (
    open.value &&
    rootRef.value &&
    !rootRef.value.contains(t) &&
    !popoverRef.value?.contains(t)
  )
    close();
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
      ref="popoverRef"
      class="fixed z-[60] max-h-60 overflow-y-auto rounded-md border bg-popover p-1 shadow-md scrollbar-thin"
      :style="floatStyle"
    >
      <div v-if="searchable" class="relative mb-1">
        <Search class="pointer-events-none absolute left-2 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground" />
        <input
          ref="searchRef"
          v-model="query"
          type="text"
          placeholder="搜索…"
          class="h-7 w-full rounded-sm border border-input bg-transparent pl-7 pr-2 text-xs placeholder:text-muted-foreground focus-visible:outline-none"
          @keydown="onSearchKeydown"
        />
      </div>
      <button
        v-for="(o, i) in visibleOptions"
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
      <p
        v-if="filteredOptions.length > visibleOptions.length"
        class="px-2 py-1.5 text-xs text-muted-foreground"
      >
        仅显示前 {{ visibleOptions.length }} 条（共 {{ filteredOptions.length }} 条），请输入关键词缩小范围。
      </p>
      <p v-else-if="visibleOptions.length === 0" class="px-2 py-1.5 text-xs text-muted-foreground">无匹配结果</p>
    </div>
  </Teleport>
</template>
