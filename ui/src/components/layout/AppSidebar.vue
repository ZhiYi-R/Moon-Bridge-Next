<script setup lang="ts">
import {
  BarChart3,
  Boxes,
  Gauge,
  GitBranch,
  Moon,
  PanelLeftClose,
  PanelLeftOpen,
  Puzzle,
  ScrollText,
  Server,
  Settings,
} from "lucide-vue-next";
import { ref } from "vue";
import { RouterLink } from "vue-router";

const nav = [
  { to: "/dashboard", title: "仪表盘", icon: Gauge },
  { to: "/providers", title: "上游服务", icon: Server },
  { to: "/models", title: "模型", icon: Boxes },
  { to: "/routes", title: "路由", icon: GitBranch },
  { to: "/plugins", title: "插件", icon: Puzzle },
  { to: "/usage", title: "用量", icon: BarChart3 },
  { to: "/traces", title: "调用追踪", icon: ScrollText },
  { to: "/settings", title: "设置", icon: Settings },
];

const STORAGE_KEY = "sidebar-collapsed";
const collapsed = ref(localStorage.getItem(STORAGE_KEY) === "1");

function toggle() {
  collapsed.value = !collapsed.value;
  localStorage.setItem(STORAGE_KEY, collapsed.value ? "1" : "0");
}
</script>

<template>
  <aside
    class="flex h-full shrink-0 flex-col border-r bg-card transition-[width] duration-150"
    :class="collapsed ? 'w-14' : 'w-60'"
  >
    <!-- 品牌区 -->
    <div
      class="flex h-14 shrink-0 items-center border-b"
      :class="collapsed ? 'justify-center px-2' : 'gap-2 px-4'"
    >
      <Moon class="size-5 shrink-0 text-primary" />
      <template v-if="!collapsed">
        <span class="text-sm font-semibold tracking-tight">Moon Bridge</span>
        <span class="rounded bg-accent px-1.5 py-0.5 text-[10px] font-medium text-accent-foreground">
          Next
        </span>
      </template>
    </div>

    <!-- 导航 -->
    <nav class="scrollbar-thin min-h-0 flex-1 space-y-1 overflow-y-auto p-2">
      <RouterLink
        v-for="item in nav"
        :key="item.to"
        :to="item.to"
        :title="item.title"
        class="flex items-center rounded-md text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-accent-foreground"
        :class="collapsed ? 'justify-center p-2' : 'gap-3 px-3 py-2'"
        active-class="!bg-accent !text-accent-foreground font-medium"
      >
        <component :is="item.icon" class="size-4 shrink-0" />
        <span v-if="!collapsed" class="truncate">{{ item.title }}</span>
      </RouterLink>
    </nav>

    <!-- 收起 / 展开 -->
    <div class="shrink-0 border-t p-2">
      <button
        class="flex w-full items-center rounded-md text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-accent-foreground"
        :class="collapsed ? 'justify-center p-2' : 'gap-3 px-3 py-2'"
        :title="collapsed ? '展开侧栏' : '收起侧栏'"
        @click="toggle"
      >
        <PanelLeftOpen v-if="collapsed" class="size-4 shrink-0" />
        <PanelLeftClose v-else class="size-4 shrink-0" />
        <span v-if="!collapsed">收起侧栏</span>
      </button>
    </div>
  </aside>
</template>
