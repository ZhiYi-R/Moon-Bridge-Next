<script setup lang="ts">
import { onMounted, onUnmounted, ref } from "vue";
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
      <!-- flush 路由（全屏主从面板）去内边距，内容直接打满 -->
      <main
        class="scrollbar-thin min-h-0 flex-1 overflow-y-auto"
        :class="route.meta.flush ? '' : 'p-6'"
      >
        <RouterView v-slot="{ Component }">
          <!-- 缓存全部视图实例：切回页面直接复用已渲染内容与滚动/筛选状态，不再整表重拉 -->
          <keep-alive>
            <component :is="Component" />
          </keep-alive>
        </RouterView>
      </main>
    </div>
  </div>
  <!-- 全局确认弹窗 / toast：登录遮罩期间也要可用 -->
  <ConfirmHost />
  <ToastHost />
</template>
