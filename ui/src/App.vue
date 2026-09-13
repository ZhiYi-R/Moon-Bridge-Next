<script setup lang="ts">
import { onMounted } from "vue";
import { RouterView, useRoute } from "vue-router";

import AppHeader from "@/components/layout/AppHeader.vue";
import AppSidebar from "@/components/layout/AppSidebar.vue";
import ConfirmHost from "@/components/ConfirmHost.vue";
import ToastHost from "@/components/ui/ToastHost.vue";
import { useGatewayStore } from "@/stores/gateway";

const gateway = useGatewayStore();
const route = useRoute();

// 应用挂载后开始轮询网关状态（自启动可能仍在进行，外部变更也要能感知）
onMounted(() => {
  gateway.refresh();
  gateway.startPolling();
});
</script>

<template>
  <div class="flex h-full overflow-hidden">
    <AppSidebar />
    <div class="flex min-w-0 flex-1 flex-col">
      <AppHeader />
      <!-- flush 路由（全屏主从面板）去内边距，内容直接打满 -->
      <main
        class="scrollbar-thin min-h-0 flex-1 overflow-y-auto"
        :class="route.meta.flush ? '' : 'p-6'"
      >
        <RouterView />
      </main>
    </div>
    <!-- 全局确认弹窗（替代 window.confirm，与应用主题一致） -->
    <ConfirmHost />
    <!-- 全局 toast 通知 -->
    <ToastHost />
  </div>
</template>
