<script setup lang="ts">
import { X } from "lucide-vue-next";
import { onMounted, onUnmounted } from "vue";

const props = defineProps<{
  open: boolean;
  title: string;
  /** 内容区最大宽度类，默认 max-w-2xl。 */
  width?: string;
  /** 简洁模式：去掉 header/footer 分割线，适合小型确认弹窗。 */
  plain?: boolean;
  /** 层级类，默认 z-50；需要盖在其它弹窗之上时（如表单守卫确认框）传更高值。 */
  zClass?: string;
  /** 关闭守卫：遮罩点击 / Esc / X 触发，resolve false 则拦截关闭（用于有未保存修改的表单）。 */
  guard?: () => boolean | Promise<boolean>;
}>();

const emit = defineEmits<{ close: [] }>();

/** 所有关闭入口统一走守卫。 */
async function requestClose() {
  if (props.guard && !(await props.guard())) return;
  emit("close");
}

function onKey(e: KeyboardEvent) {
  if (e.key === "Escape" && props.open) void requestClose();
}

onMounted(() => document.addEventListener("keydown", onKey));
onUnmounted(() => document.removeEventListener("keydown", onKey));
</script>

<template>
  <Teleport to="body">
    <Transition name="modal">
      <div v-if="open" class="fixed inset-0 flex items-center justify-center p-4" :class="props.zClass ?? 'z-50'">
        <!-- 遮罩 -->
        <div class="modal-overlay absolute inset-0 bg-black/60" @click="requestClose" />
        <!-- 面板 -->
        <div
          class="modal-panel relative z-10 flex max-h-[85vh] w-full flex-col rounded-md border bg-card shadow-xl"
          :class="width ?? 'max-w-2xl'"
        >
          <div class="flex shrink-0 items-center justify-between px-5 py-3" :class="!plain && 'border-b'">
            <h3 class="card-title">{{ title }}</h3>
            <button
              class="text-muted-foreground transition-colors hover:text-foreground"
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
            :class="!plain && 'border-t'"
          >
            <slot name="footer" />
          </div>
        </div>
      </div>
    </Transition>
  </Teleport>
</template>

<style scoped>
.modal-enter-active,
.modal-leave-active {
  transition: opacity 0.16s ease;
}
.modal-enter-from,
.modal-leave-to {
  opacity: 0;
}
.modal-enter-active .modal-panel {
  transition:
    opacity 0.16s ease,
    transform 0.16s ease;
}
.modal-leave-active .modal-panel {
  transition:
    opacity 0.12s ease,
    transform 0.12s ease;
}
.modal-enter-from .modal-panel {
  opacity: 0;
  transform: translateY(10px) scale(0.98);
}
.modal-leave-to .modal-panel {
  opacity: 0;
  transform: translateY(6px) scale(0.98);
}
</style>
