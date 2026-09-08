import { defineStore } from "pinia";
import { computed, ref } from "vue";

import { errMsg, gatewayApi, type GatewayStatus } from "@/lib/api";

/** 网关运行状态 store：启停控制 + 状态轮询。 */
export const useGatewayStore = defineStore("gateway", () => {
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
    } finally {
      loading.value = false;
    }
  }

  return { status, loading, error, running, addr, refresh, start, stop, restart };
});
