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
  Wallet,
} from "lucide-vue-next";
import { nextTick, onMounted, onUnmounted, ref, watch } from "vue";
import { RouterLink, useRoute } from "vue-router";

const nav = [
  { to: "/dashboard", title: "仪表盘", icon: Gauge },
  { to: "/providers", title: "上游服务", icon: Server },
  { to: "/models", title: "模型", icon: Boxes },
  { to: "/routes", title: "路由", icon: GitBranch },
  { to: "/plugins", title: "插件", icon: Puzzle },
  { to: "/usage", title: "用量", icon: BarChart3 },
  { to: "/quota", title: "额度", icon: Wallet },
  { to: "/traces", title: "调用追踪", icon: ScrollText },
  { to: "/settings", title: "设置", icon: Settings },
];

const STORAGE_KEY = "sidebar-collapsed";
const collapsed = ref(localStorage.getItem(STORAGE_KEY) === "1");

function toggle() {
  collapsed.value = !collapsed.value;
  localStorage.setItem(STORAGE_KEY, collapsed.value ? "1" : "0");
}

// ── 共享指示条：单元素 translateY 滑向激活项，方向与页面切换滚动一致 ──
const route = useRoute();
const navRef = ref<HTMLElement | null>(null);
const indicatorY = ref(0);
const indicatorVisible = ref(false);

const INDICATOR_H = 16; // 与 style.css .nav-indicator 的 height 一致

function updateIndicator() {
  const nav = navRef.value;
  const active = nav?.querySelector<HTMLElement>('a[aria-current="page"]');
  if (!nav || !active) {
    indicatorVisible.value = false;
    return;
  }
  indicatorY.value = active.offsetTop + (active.offsetHeight - INDICATOR_H) / 2;
  indicatorVisible.value = true;
}

// 指示条是 nav 的绝对定位子元素，跟随滚动坐标——nav 自身滚动不需要重算；
// 路由切换、侧栏收放（项高变化）、窗口尺寸变化时需要。
let navObserver: ResizeObserver | null = null;
watch(() => route.path, () => nextTick(updateIndicator));
watch(collapsed, () => nextTick(updateIndicator));
onMounted(() => {
  updateIndicator();
  navObserver = new ResizeObserver(updateIndicator);
  if (navRef.value) navObserver.observe(navRef.value);
});
onUnmounted(() => navObserver?.disconnect());
</script>

<template>
  <aside
    class="flex h-full shrink-0 flex-col border-r bg-card transition-[width] duration-150"
    :class="collapsed ? 'w-14' : 'w-60'"
  >
    <div
      class="flex h-14 shrink-0 items-center border-b"
      :class="collapsed ? 'justify-center px-2' : 'gap-2 px-4'"
    >
      <Moon class="size-5 shrink-0 text-primary" />
      <template v-if="!collapsed">
        <span class="text-sm font-semibold tracking-tight">Moon Bridge</span>
        <span class="rounded bg-primary px-1.5 py-0.5 text-[10px] font-semibold tracking-wide text-primary-foreground">
          Next
        </span>
      </template>
    </div>

    <nav ref="navRef" class="scrollbar-thin relative min-h-0 flex-1 space-y-1 overflow-y-auto p-2">
      <!-- 当前页共享指示条：滑动至激活项，随 nav 滚动坐标定位 -->
      <span
        class="nav-indicator"
        :style="{ transform: `translateY(${indicatorY}px)`, opacity: indicatorVisible ? 1 : 0 }"
      />
      <RouterLink
        v-for="item in nav"
        :key="item.to"
        :to="item.to"
        :title="item.title"
        class="relative flex items-center rounded-md text-sm text-muted-foreground transition-colors hover:bg-accent/60 hover:text-foreground"
        :class="collapsed ? 'justify-center p-2' : 'gap-3 px-3 py-2'"
        active-class="!bg-accent !text-accent-foreground font-medium"
      >
        <component :is="item.icon" class="size-4 shrink-0" />
        <span v-if="!collapsed" class="truncate">{{ item.title }}</span>
      </RouterLink>
    </nav>

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
