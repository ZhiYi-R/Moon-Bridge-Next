<script setup lang="ts">
import { Minus, Square, X } from "lucide-vue-next";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { computed } from "vue";
import { useRoute } from "vue-router";

import Badge from "@/components/ui/Badge.vue";
import Switch from "@/components/ui/Switch.vue";
import { isTauriRuntime, usingMock } from "@/lib/api";
import { useGatewayStore } from "@/stores/gateway";

const route = useRoute();
const gateway = useGatewayStore();

const title = computed(() => (route.meta.title as string) ?? "");

// 自绘标题栏：仅在 Tauri 壳内渲染窗口控制按钮（浏览器开发模式隐藏）。
const appWindow = isTauriRuntime ? getCurrentWindow() : null;
const minimize = () => appWindow?.minimize();
const toggleMaximize = () => appWindow?.toggleMaximize();
const close = () => appWindow?.close();

const controlCls =
  "flex h-14 w-11 items-center justify-center text-muted-foreground transition-colors hover:bg-accent hover:text-foreground";
</script>

<template>
  <!-- 自绘标题栏：data-tauri-drag-region 使空白区域可拖拽窗口（双击切换最大化） -->
  <header
    data-tauri-drag-region
    class="flex h-14 shrink-0 select-none items-center justify-between border-b px-6"
  >
    <h1 data-tauri-drag-region class="text-lg font-semibold tracking-tight">{{ title }}</h1>
    <div class="flex items-center gap-2">
      <Badge
        v-if="usingMock"
        variant="warning"
        class="h-7 font-mono text-xs"
        title="后端不可用（纯浏览器开发模式），展示内存 mock 数据"
      >
        MOCK 数据
      </Badge>
      <Badge variant="outline" class="h-7 gap-1.5 font-mono text-xs">
        <span
          class="size-1.5 rounded-full"
          :class="gateway.running ? 'bg-emerald-500' : 'bg-muted-foreground/40'"
        />
        {{ gateway.running ? "运行中" : "已停止" }} · {{ gateway.addr || "—" }}
      </Badge>
      <!-- 网关开关：开启=运行中 -->
      <Switch
        :checked="gateway.running"
        :disabled="gateway.loading"
        :title="gateway.running ? '停止网关' : '启动网关'"
        @update:checked="(v) => (v ? gateway.start() : gateway.stop())"
      />

      <!-- 窗口控制（仅 Tauri 壳内） -->
      <div v-if="appWindow" class="-mr-6 ml-2 flex items-center self-stretch">
        <button :class="controlCls" title="最小化" @click="minimize">
          <Minus class="size-4" />
        </button>
        <button :class="controlCls" title="最大化 / 还原" @click="toggleMaximize">
          <Square class="size-3.5" />
        </button>
        <button
          :class="controlCls"
          class="hover:bg-destructive hover:text-white"
          title="关闭"
          @click="close"
        >
          <X class="size-4" />
        </button>
      </div>
    </div>
  </header>
</template>
