<script setup lang="ts">
import { onMounted, onUnmounted, ref, watch } from "vue";
import { RouterView, useRoute } from "vue-router";

import AppHeader from "@/components/layout/AppHeader.vue";
import AppSidebar from "@/components/layout/AppSidebar.vue";
import LoginCard from "@/components/layout/LoginCard.vue";
import ConfirmHost from "@/components/ConfirmHost.vue";
import ToastHost from "@/components/ui/ToastHost.vue";
import { isWebRuntime } from "@/lib/api";
import { UNAUTHORIZED_EVENT, getToken } from "@/lib/web";
import { useGatewayStore } from "@/stores/gateway";

const gateway = useGatewayStore();
const route = useRoute();

// 侧栏导航顺序（与路由定义一致）：视图切换沿纵向滚动——目标在下方则新视图自底部滚入，
// 旧视图向上滚出，方向与用户在标签列表里的移动方向一致。
const ROUTE_ORDER = ["/dashboard", "/providers", "/models", "/routes", "/plugins", "/usage", "/quota", "/traces", "/settings"];
const viewDir = ref(1); // 1 = 向下，-1 = 向上
watch(
  () => route.path,
  (to, from) => {
    const ti = ROUTE_ORDER.indexOf(to);
    const fi = ROUTE_ORDER.indexOf(from);
    if (ti >= 0 && fi >= 0 && ti !== fi) viewDir.value = ti > fi ? 1 : -1;
  },
);

// 过渡期间裁剪 main 的可视溢出：旧视图滑出屏外的部分不露出、也不撑出滚动条
const transitioning = ref(false);

/** web 模式缺令牌 / 令牌失效时只渲染登录卡片，避免无令牌请求刷屏。 */
const needsLogin = ref(isWebRuntime && !getToken());

function onUnauthorized() {
  if (!isWebRuntime) return;
  needsLogin.value = true;
  // 登录态失效后停止轮询：避免无令牌请求反复触发 401
  gateway.stopPolling();
}

async function onAuthenticated() {
  needsLogin.value = false;
  await gateway.refresh();
  gateway.startPolling();
}

// 应用挂载后开始轮询网关状态（自启动可能仍在进行，外部变更也要能感知）
onMounted(() => {
  window.addEventListener(UNAUTHORIZED_EVENT, onUnauthorized);
  if (needsLogin.value) return;
  gateway.refresh();
  gateway.startPolling();
});

onUnmounted(() => {
  window.removeEventListener(UNAUTHORIZED_EVENT, onUnauthorized);
});
</script>

<template>
  <!-- web 管理模式：令牌校验通过前不渲染主界面 -->
  <LoginCard v-if="needsLogin" @authenticated="onAuthenticated" />
  <div v-else class="flex h-full overflow-hidden">
    <AppSidebar />
    <div class="flex min-w-0 flex-1 flex-col">
      <AppHeader />
      <!-- flush 路由（全屏主从面板）去内边距，内容直接打满；bg-card 由 main 承载 -->
      <main
        class="route-stack scrollbar-thin min-h-0 flex-1 overflow-y-auto"
        :class="[route.meta.flush ? 'bg-card text-card-foreground' : 'p-6', { transitioning }, viewDir > 0 ? 'dir-down' : 'dir-up']"
      >
        <RouterView v-slot="{ Component }">
          <!-- 缓存全部视图实例：切回页面直接复用已渲染内容与滚动/筛选状态，不再整表重拉 -->
          <Transition
            name="view"
            @before-leave="transitioning = true"
            @after-leave="transitioning = false"
            @leave-cancelled="transitioning = false"
          >
            <keep-alive>
              <component :is="Component" />
            </keep-alive>
          </Transition>
        </RouterView>
      </main>
    </div>
  </div>
  <!-- 全局确认弹窗 / toast：登录遮罩期间也要可用 -->
  <ConfirmHost />
  <ToastHost />
</template>

<style>
/* 视图栈：main 用 grid 让所有子视图占同一格。两方向都只向上侧越界——
   滚动容器的可滚动区不增长，过渡全程无滚动条闪现（dir-down 旧视图向上滑出
   揭开新视图；dir-up 新视图自上方滑入盖住旧视图）。纯位移无叠影。
   非 scoped——Transition 类加在 keep-alive 的子组件根上。 */
.route-stack {
  display: grid;
}
.route-stack > * {
  grid-area: 1 / 1;
  min-width: 0;
  min-height: 0;
}
.route-stack.dir-down .view-leave-active {
  position: relative;
  z-index: 1;
  pointer-events: none;
  animation: view-out-up var(--dur-med) var(--ease-in) both;
}
.route-stack.dir-up .view-enter-active {
  position: relative;
  z-index: 1;
  pointer-events: none;
  animation: view-in-down var(--dur-med) var(--ease-out) both;
}
/* dir-up 的旧视图在下静止：仅撑住动画时长等待新视图盖完（滚动条/时序需要） */
.route-stack.dir-up .view-leave-active {
  animation: view-stay var(--dur-med) linear both;
}
.route-stack.dir-down > .view-leave-active {
  background-color: hsl(var(--background));
}
.route-stack.dir-down.bg-card > .view-leave-active {
  background-color: hsl(var(--card));
}
.route-stack.dir-up > .view-enter-active {
  background-color: hsl(var(--background));
}
.route-stack.dir-up.bg-card > .view-enter-active {
  background-color: hsl(var(--card));
}
@keyframes view-out-up {
  to {
    transform: translateY(-100%);
  }
}
@keyframes view-in-down {
  from {
    transform: translateY(-100%);
  }
}
@keyframes view-stay {
  to {
    transform: translateY(0.01px);
  }
}
/* 过渡期间两个视图的 sticky 表头都压平：离场/进场视图的表头随各自表身一起
   滑走或被揭开——吸附钉在视口顶口会与表身脱节。 */
.route-stack.transitioning .thead-sticky {
  position: static;
}
</style>
