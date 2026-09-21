<script lang="ts">
// 叠放弹窗（如编辑弹窗上的「丢弃修改」确认框）只允许最上层接管 Esc/Tab，否则下层会把焦点抢回去。
// 模块级栈：open 时入栈，关闭/卸载时出栈。
const modalStack: number[] = [];
let modalSeq = 0;
</script>

<script setup lang="ts">
import { X } from "lucide-vue-next";
import { nextTick, onMounted, onUnmounted, ref, watch } from "vue";

const props = defineProps<{
  open: boolean;
  title: string;
  /** 内容区最大宽度类，默认 max-w-2xl。 */
  width?: string;
  /** 层级类，默认 z-50；需要盖在其它弹窗之上时（如表单确认框）传更高值。 */
  zClass?: string;
  /** 关闭前确认：遮罩点击 / Esc / X 均触发，resolve false 则拦截关闭（用于有未保存修改的表单）。 */
  guard?: () => boolean | Promise<boolean>;
}>();

const emit = defineEmits<{ close: [] }>();

/** 所有关闭入口统一走 guard。 */
async function requestClose() {
  if (props.guard && !(await props.guard())) return;
  emit("close");
}

// ── 焦点管理：打开时焦点进入面板，关闭时还原到触发元素 ──
const modalId = ++modalSeq;
const panelRef = ref<HTMLElement | null>(null);
let prevActive: HTMLElement | null = null;

const FOCUSABLE =
  'a[href], button:not([disabled]), input:not([disabled]), textarea:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex="-1"])';

/** 是否当前最上层弹窗（叠放场景下只有它能响应 Esc/Tab）。 */
const isTop = () => modalStack[modalStack.length - 1] === modalId;

function leaveStack() {
  const i = modalStack.lastIndexOf(modalId);
  if (i >= 0) modalStack.splice(i, 1);
}

watch(
  () => props.open,
  async (v) => {
    if (v) {
      leaveStack();
      modalStack.push(modalId);
      prevActive = document.activeElement as HTMLElement | null;
      await nextTick();
      // 聚焦面板本身（tabindex=-1，无可见焦点环），Tab 自然流入第一个控件
      panelRef.value?.focus();
    } else {
      leaveStack();
      prevActive?.focus?.();
      prevActive = null;
    }
  },
);

/** focus trap：Tab 在面板内循环；teleport 到 body 的 Select 浮层不算脱管（它自己处理 Tab 收起）。 */
function trapTab(e: KeyboardEvent) {
  const panel = panelRef.value;
  if (!panel) return;
  const items = [...panel.querySelectorAll<HTMLElement>(FOCUSABLE)].filter(
    (el) => el.getClientRects().length > 0,
  );
  if (items.length === 0) {
    e.preventDefault();
    return;
  }
  const active = document.activeElement;
  const escape = e.shiftKey
    ? active === items[0] || !panel.contains(active)
    : active === items[items.length - 1] || !panel.contains(active);
  if (escape) {
    e.preventDefault();
    (e.shiftKey ? items[items.length - 1] : items[0]).focus();
  }
}

function onKey(e: KeyboardEvent) {
  if (!props.open || !isTop()) return;
  if (e.key === "Escape") void requestClose();
  else if (e.key === "Tab") trapTab(e);
}

onMounted(() => document.addEventListener("keydown", onKey));
onUnmounted(() => {
  document.removeEventListener("keydown", onKey);
  leaveStack();
});
</script>

<template>
  <Teleport to="body">
    <Transition name="modal" :duration="{ enter: 200, leave: 150 }">
      <div v-if="open" class="fixed inset-0 flex items-center justify-center p-4" :class="props.zClass ?? 'z-50'">
        <div class="modal-overlay absolute inset-0 bg-black/60" @click="requestClose" />
        <div
          ref="panelRef"
          role="dialog"
          aria-modal="true"
          :aria-label="title"
          tabindex="-1"
          class="modal-panel glass-panel relative z-10 flex max-h-[85vh] w-full flex-col rounded-md border shadow-xl outline-none"
          :class="width ?? 'max-w-2xl'"
        >
          <div class="flex shrink-0 items-center justify-between px-5 py-3">
            <h3 class="card-title">{{ title }}</h3>
            <button
              class="flex size-7 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
              title="关闭"
              @click="requestClose"
            >
              <X class="size-4" />
            </button>
          </div>
          <div v-if="$slots.notice" class="shrink-0">
            <slot name="notice" />
          </div>
          <div class="scrollbar-thin min-h-0 flex-1 overflow-y-auto p-5">
            <slot />
          </div>
          <div
            v-if="$slots.footer"
            class="flex shrink-0 justify-end gap-2 px-5 py-3"
          >
            <slot name="footer" />
          </div>
        </div>
      </div>
    </Transition>
  </Teleport>
</template>

<style scoped>
/* WebKit 下 backdrop-filter 在 opacity<1 的元素/祖先内被整体跳过——
   带模糊的遮罩与面板全程保持不透明：遮罩只渐变 background-color，
   面板只动 transform。元素移除时机由 Transition 的 :duration 决定（根无过渡）。 */
.modal-overlay {
  transition: background-color var(--dur-base) var(--ease-out);
}
.modal-leave-active .modal-overlay {
  transition-duration: calc(var(--dur-base) * 0.75);
  transition-timing-function: var(--ease-in);
}
.modal-enter-from .modal-overlay,
.modal-leave-to .modal-overlay {
  background-color: transparent;
}
.modal-enter-active .modal-panel {
  transition: transform var(--dur-base) var(--ease-out);
}
.modal-leave-active .modal-panel {
  transition: transform calc(var(--dur-base) * 0.75) var(--ease-in);
}
.modal-enter-from .modal-panel {
  transform: translateY(10px) scale(0.98);
}
.modal-leave-to .modal-panel {
  transform: translateY(6px) scale(0.98);
}
</style>
