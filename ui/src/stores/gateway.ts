import { defineStore } from "pinia";
import { computed, ref } from "vue";

import { useToast } from "@/composables/useToast";
import { errMsg, gatewayApi, type GatewayStatus } from "@/lib/api";

/** 轮询间隔：足够敏感地反映启停/崩溃，IPC 开销可忽略。 */
const POLL_MS = 4000;

/** 网关运行状态 store：启停控制 + 状态轮询。 */
export const useGatewayStore = defineStore("gateway", () => {
  const toast = useToast();
  const status = ref<GatewayStatus | null>(null);
  const loading = ref(false);
  const error = ref<string | null>(null);

  const running = computed(() => status.value?.running ?? false);
  const addr = computed(() => status.value?.addr ?? "");

  async function refresh() {
    try {
      status.value = await gatewayApi.status();
      error.value = status.value?.error ?? null;
    } catch (e) {
      error.value = errMsg(e);
    }
  }

  async function start() {
    loading.value = true;
    try {
      status.value = await gatewayApi.start();
      error.value = status.value?.error ?? null;
    } catch (e) {
      error.value = errMsg(e);
      toast.error(`启动网关失败：${error.value}`);
    } finally {
      loading.value = false;
    }
  }

  async function stop() {
    loading.value = true;
    try {
      status.value = await gatewayApi.stop();
    } catch (e) {
      error.value = errMsg(e);
      toast.error(`停止网关失败：${error.value}`);
    } finally {
      loading.value = false;
    }
  }

  async function restart() {
    loading.value = true;
    try {
      status.value = await gatewayApi.restart();
      error.value = status.value?.error ?? null;
    } catch (e) {
      error.value = errMsg(e);
      toast.error(`重启网关失败：${error.value}`);
    } finally {
      loading.value = false;
    }
  }

  // ── 状态轮询：托盘操作、自启动、网关崩溃等外部变化可反映到顶栏 ──
  let timer: ReturnType<typeof setInterval> | null = null;

  function startPolling() {
    if (timer) return;
    timer = setInterval(refresh, POLL_MS);
  }

  function stopPolling() {
    if (timer) clearInterval(timer);
    timer = null;
  }

  return {
    status,
    loading,
    error,
    running,
    addr,
    refresh,
    start,
    stop,
    restart,
    startPolling,
    stopPolling,
  };
});
