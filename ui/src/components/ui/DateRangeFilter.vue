<script setup lang="ts">
import { Calendar, Check, ChevronDown, ChevronLeft, ChevronRight } from "lucide-vue-next";
import { computed, onMounted, onUnmounted, ref, type HTMLAttributes } from "vue";
import { cn } from "@/lib/utils";

/** 时间范围（秒）：两端均含（后端 until 是 created_at <= ?）。 */
export interface DateRange {
  from: number;
  to: number;
}

const props = withDefaults(
  defineProps<{
    modelValue: DateRange | null;
    class?: HTMLAttributes["class"];
  }>(),
  {},
);

const emit = defineEmits<{ "update:modelValue": [DateRange | null] }>();

interface Preset {
  key: string;
  label: string;
  /** 返回 [from, to) 秒；null = 不限。 */
  resolve: () => DateRange | null;
}

const DAY = 86400;
const nowSec = () => Date.now() / 1000;
/** 本地某日 00:00 的秒级时间戳。 */
const dayStart = (d: Date) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime() / 1000;

const PRESETS: Preset[] = [
  { key: "today", label: "今日", resolve: () => ({ from: dayStart(new Date()), to: nowSec() }) },
  { key: "24h", label: "24小时", resolve: () => ({ from: nowSec() - DAY, to: nowSec() }) },
  {
    key: "week",
    label: "本周",
    resolve: () => {
      const d = new Date();
      const monday = new Date(d.getFullYear(), d.getMonth(), d.getDate() - ((d.getDay() + 6) % 7));
      return { from: dayStart(monday), to: nowSec() };
    },
  },
  { key: "7d", label: "7天", resolve: () => ({ from: nowSec() - 7 * DAY, to: nowSec() }) },
  {
    key: "month",
    label: "本月",
    resolve: () => {
      const d = new Date();
      return { from: dayStart(new Date(d.getFullYear(), d.getMonth(), 1)), to: nowSec() };
    },
  },
  { key: "all", label: "所有", resolve: () => null },
  { key: "custom", label: "自定义", resolve: () => null },
];

const open = ref(false);
const rootRef = ref<HTMLElement | null>(null);
const buttonRef = ref<HTMLButtonElement | null>(null);
const popoverRef = ref<HTMLElement | null>(null);
const floatStyle = ref<{ top: string; right: string; bottom?: string }>({ top: "0", right: "0" });

const preset = ref("all");
/** 日历面板是否展开（选「自定义」时展开，选其它项应用并关闭）。 */
const customMode = ref(false);
/** 日历暂存的起止日（日初秒）；从已应用的 range 恢复。 */
const selStart = ref<number | null>(null);
const selEnd = ref<number | null>(null);
/** 悬停预览日（仅选了起点未选终点时预览区间）。 */
const hoverDay = ref<number | null>(null);
/** 日历当前展示的月份。 */
const viewY = ref(0);
const viewM = ref(0);

const WEEKDAYS = ["一", "二", "三", "四", "五", "六", "日"];

/** 当月日历格：前导 null 对齐周一，后跟 1..N 日（日初秒）。 */
const cells = computed(() => {
  const first = new Date(viewY.value, viewM.value, 1);
  const lead = (first.getDay() + 6) % 7;
  const n = new Date(viewY.value, viewM.value + 1, 0).getDate();
  const out: (number | null)[] = Array(lead).fill(null);
  for (let d = 1; d <= n; d++) out.push(dayStart(new Date(viewY.value, viewM.value, d)));
  return out;
});

const viewTitle = computed(() => `${viewY.value}年${viewM.value + 1}月`);

function prevMonth() {
  const d = new Date(viewY.value, viewM.value - 1, 1);
  viewY.value = d.getFullYear();
  viewM.value = d.getMonth();
}
function nextMonth() {
  const d = new Date(viewY.value, viewM.value + 1, 1);
  viewY.value = d.getFullYear();
  viewM.value = d.getMonth();
}

const fmtDay = (sec: number) => {
  const d = new Date(sec * 1000);
  return `${d.getMonth() + 1}/${d.getDate()}`;
};

const buttonLabel = computed(() => {
  if (preset.value === "custom" && selStart.value && selEnd.value)
    return `${fmtDay(selStart.value)} – ${fmtDay(selEnd.value)}`;
  return PRESETS.find((p) => p.key === preset.value)?.label ?? "所有";
});

function toggle() {
  open.value ? close() : show();
}

function show() {
  const el = buttonRef.value;
  if (!el) return;
  const r = el.getBoundingClientRect();
  const below = window.innerHeight - r.bottom;
  // 日历展开约需 21rem 高；放不下时向上展开
  const openUp = below < 340 && r.top > 340;
  floatStyle.value = {
    // 右对齐锚定：日历展开向左生长不会越过视口右缘
    right: `${window.innerWidth - r.right}px`,
    top: openUp ? "" : `${r.bottom + 4}px`,
    bottom: openUp ? `${window.innerHeight - r.top + 4}px` : "",
  };
  customMode.value = preset.value === "custom";
  open.value = true;
}

function close() {
  open.value = false;
}

function pick(p: Preset) {
  if (p.key === "custom") {
    preset.value = "custom";
    customMode.value = true;
    // 恢复上次应用的自定义范围，否则默认本月视图
    const base = selStart.value ? new Date(selStart.value * 1000) : new Date();
    viewY.value = base.getFullYear();
    viewM.value = base.getMonth();
    return;
  }
  preset.value = p.key;
  customMode.value = false;
  emit("update:modelValue", p.resolve());
  close();
  buttonRef.value?.focus();
}

function pickDay(daySec: number) {
  if (selStart.value === null || selEnd.value !== null) {
    selStart.value = daySec;
    selEnd.value = null;
  } else if (daySec < selStart.value) {
    selStart.value = daySec;
  } else {
    selEnd.value = daySec;
  }
}

/** 区间高亮：已定 [start,end]；未定终点时用悬停日预览。 */
function inRange(daySec: number) {
  if (selStart.value === null) return false;
  const end = selEnd.value ?? hoverDay.value;
  if (end === null) return false;
  const lo = Math.min(selStart.value, end);
  const hi = Math.max(selStart.value, end);
  return daySec > lo && daySec < hi;
}

function isEndpoint(daySec: number) {
  return daySec === selStart.value || daySec === selEnd.value;
}

const canApply = computed(() => selStart.value !== null && selEnd.value !== null);

function applyCustom() {
  if (!canApply.value) return;
  // 结束日取 23:59:59（含）：until 语义是 <=，给次日 0 点会把次日凌晨记录漏进来
  emit("update:modelValue", { from: selStart.value!, to: selEnd.value! + DAY - 1 });
  close();
  buttonRef.value?.focus();
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

function onWinScroll(e: Event) {
  if (popoverRef.value?.contains(e.target as Node)) return;
  close();
}

onMounted(() => {
  document.addEventListener("click", onDocClick, true);
  window.addEventListener("resize", close);
  window.addEventListener("scroll", onWinScroll, true);
});
onUnmounted(() => {
  document.removeEventListener("click", onDocClick, true);
  window.removeEventListener("resize", close);
  window.removeEventListener("scroll", onWinScroll, true);
});
</script>

<template>
  <div ref="rootRef" :class="cn('relative min-w-0', props.class)">
    <button
      ref="buttonRef"
      type="button"
      aria-haspopup="dialog"
      :aria-expanded="open"
      :title="buttonLabel"
      class="flex h-7 items-center gap-1.5 rounded-md border border-input bg-transparent px-2 text-xs shadow-sm transition-colors focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
      @click="toggle"
      @keydown.escape="open && close()"
    >
      <Calendar class="size-3.5 shrink-0 text-muted-foreground" />
      <span class="whitespace-nowrap">{{ buttonLabel }}</span>
      <ChevronDown class="size-3.5 shrink-0 text-muted-foreground" :class="open && 'rotate-180'" />
    </button>
  </div>

  <Teleport to="body">
    <Transition name="pop">
      <div
        v-if="open"
        ref="popoverRef"
        role="dialog"
        aria-label="时间范围筛选"
        class="glass fixed z-[60] flex max-h-[22rem] overflow-hidden rounded-md border shadow-md"
        :style="floatStyle"
        @keydown.escape="close()"
      >
      <!-- 预设列 -->
      <div class="scrollbar-thin w-28 shrink-0 overflow-y-auto p-1">
        <button
          v-for="p in PRESETS"
          :key="p.key"
          type="button"
          :class="
            cn(
              'flex w-full items-center justify-between gap-2 rounded-sm px-2 py-1.5 text-left text-sm transition-colors hover:bg-accent',
              p.key === preset && 'bg-accent text-accent-foreground',
            )
          "
          @click="pick(p)"
        >
          <span>{{ p.label }}</span>
          <Check v-if="p.key === preset" class="size-3.5 shrink-0" />
        </button>
      </div>

      <!-- 自定义日历（选「自定义」时展开，面板不关闭） -->
      <div v-if="customMode" class="w-60 border-l p-3">
        <div class="mb-2 flex items-center justify-between">
          <button
            type="button"
            class="flex size-6 items-center justify-center rounded-sm text-muted-foreground transition-colors hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
            title="上个月"
            @click="prevMonth"
          >
            <ChevronLeft class="size-4" />
          </button>
          <span class="text-xs font-medium tabular-nums">{{ viewTitle }}</span>
          <button
            type="button"
            class="flex size-6 items-center justify-center rounded-sm text-muted-foreground transition-colors hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
            title="下个月"
            @click="nextMonth"
          >
            <ChevronRight class="size-4" />
          </button>
        </div>
        <div class="grid grid-cols-7 gap-0.5 text-center">
          <span
            v-for="w in WEEKDAYS"
            :key="w"
            class="flex h-6 items-center justify-center text-[10px] text-muted-foreground"
            >{{ w }}</span
          >
          <template v-for="(cell, i) in cells" :key="i">
            <span v-if="cell === null" class="h-7" />
            <button
              v-else
              type="button"
              :class="
                cn(
                  'flex h-7 items-center justify-center rounded-sm text-xs tabular-nums transition-colors hover:bg-accent focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring',
                  inRange(cell) && 'bg-accent',
                  isEndpoint(cell) && 'bg-primary text-primary-foreground hover:bg-primary',
                )
              "
              @click="pickDay(cell)"
              @mouseenter="hoverDay = cell"
            >
              {{ new Date(cell * 1000).getDate() }}
            </button>
          </template>
        </div>
        <div class="mt-2 flex items-center justify-between gap-2 border-t pt-2">
          <span class="text-xs text-muted-foreground tabular-nums">
            <template v-if="selStart">{{ fmtDay(selStart) }}<template v-if="selEnd"> – {{ fmtDay(selEnd) }}</template><template v-else> – …</template></template>
            <template v-else>选择起止日期</template>
          </span>
          <button
            type="button"
            :disabled="!canApply"
            class="h-7 rounded-md bg-primary px-3 text-xs font-medium text-primary-foreground transition-colors hover:bg-primary/90 focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring disabled:pointer-events-none disabled:opacity-50"
            @click="applyCustom"
          >
            确定
          </button>
        </div>
      </div>
      </div>
    </Transition>
  </Teleport>
</template>

<style scoped>
.pop-enter-active,
.pop-leave-active {
  transition:
    opacity var(--dur-micro) var(--ease-out),
    transform var(--dur-micro) var(--ease-out);
  transform-origin: top center;
}
.pop-leave-active {
  transition-duration: calc(var(--dur-micro) * 0.75);
}
.pop-enter-from,
.pop-leave-to {
  opacity: 0;
  transform: translateY(-4px) scale(0.97);
}
</style>
