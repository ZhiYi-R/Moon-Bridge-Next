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
}>();

const emit = defineEmits<{ close: [] }>();

function onKey(e: KeyboardEvent) {
  if (e.key === "Escape" && props.open) emit("close");
}

onMounted(() => document.addEventListener("keydown", onKey));
onUnmounted(() => document.removeEventListener("keydown", onKey));
</script>

<template>
  <Teleport to="body">
    <div v-if="open" class="fixed inset-0 z-50 flex items-center justify-center p-4">
      <!-- 遮罩 -->
      <div class="absolute inset-0 bg-black/60" @click="emit('close')" />
      <!-- 面板 -->
      <div
        class="relative z-10 flex max-h-[85vh] w-full flex-col rounded-md border bg-card shadow-xl"
        :class="width ?? 'max-w-2xl'"
      >
        <div class="flex shrink-0 items-center justify-between px-5 py-3" :class="!plain && 'border-b'">
          <h3 class="card-title">{{ title }}</h3>
          <button
            class="text-muted-foreground transition-colors hover:text-foreground"
            title="关闭"
            @click="emit('close')"
          >
            <X class="size-4" />
          </button>
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
  </Teleport>
</template>
