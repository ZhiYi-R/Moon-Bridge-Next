import { defineStore } from "pinia";
import { computed, ref } from "vue";

import { useToast } from "@/composables/useToast";
import { errMsg, gatewayApi, isWebRuntime, type GatewayStatus } from "@/lib/api";

/** 轮询间隔：足够敏感地反映启停/崩溃，IPC 开销可忽略。 */
const POLL_MS = 4000;

/** 网关运行状态 store：启停控制 + 状态轮询。 */
export const useGatewayStore = defineStore("gateway", () => {
  const toast = useToast();
  const status = ref<GatewayStatus | null>(null);
  const loading = ref(false);
  const error = ref<string | null>(null);

  /** web 模式：进程由外部托管，启停在 UI 隐藏；轮询失败只表示「重连中」。 */
  const canControlProcess = !isWebRuntime;
  /** 最近一次状态查询失败（web 模式下服务重启期间的正常现象）。 */
  const disconnected = ref(false);

  const running = computed(() => status.value?.running ?? false);
  const addr = computed(() => status.value?.addr ?? "");

  async function refresh() {
    try {
      status.value = await gatewayApi.status();
      error.value = status.value?.error ?? null;
      disconnected.value = false;
    } catch (e) {
      if (isWebRuntime) {
        // 服务重启期间请求短暂失败属正常：保留上次状态并显示「重连中」，不刷错误横幅
        disconnected.value = true;
        return;
      }
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
      if (isWebRuntime) {
        await gatewayApi.restart();
        toast.success("正在等待在途请求结束，然后重新启动网关");
        return;
      }
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
    disconnected,
    canControlProcess,
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
