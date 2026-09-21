import { defineStore } from "pinia";
import { ref } from "vue";

import { errMsg, quotaApi, type ProviderQuotaView } from "@/lib/api";

/** 配额查询 store：Provider 视图列表 + 单 Provider/全量刷新，写操作按返回值原地更新。 */
export const useQuotaStore = defineStore("quota", () => {
  const views = ref<ProviderQuotaView[]>([]);
  const loading = ref(false);
  const error = ref<string | null>(null);
  /** 正在查询中的 provider key（刷新图标按此旋转）。 */
  const refreshingKeys = ref(new Set<string>());
  const refreshingAll = ref(false);
  /** 是否已成功加载过一次：路由缓存复用时用于静默刷新。 */
  const loaded = ref(false);

  /** 用新视图替换同 key Provider；不存在则追加。 */
  function upsert(view: ProviderQuotaView) {
    const idx = views.value.findIndex((v) => v.providerKey === view.providerKey);
    if (idx >= 0) views.value[idx] = view;
    else views.value.push(view);
  }

  function markRefreshing(key: string, on: boolean) {
    const next = new Set(refreshingKeys.value);
    if (on) next.add(key);
    else next.delete(key);
    refreshingKeys.value = next;
  }

  let listRequest: Promise<void> | null = null;

  function list(): Promise<void> {
    if (listRequest) return listRequest;
    loading.value = true;
    listRequest = (async () => {
      try {
        views.value = await quotaApi.list();
        loaded.value = true;
        error.value = null;
      } catch (e) {
        error.value = errMsg(e);
      } finally {
        loading.value = false;
        listRequest = null;
      }
    })();
    return listRequest;
  }

  /** 刷新一个 Provider 的配额（逐端点全量重跑）。 */
  async function refreshOne(providerKey: string) {
    markRefreshing(providerKey, true);
    try {
      upsert(await quotaApi.refresh(providerKey));
      error.value = null;
    } catch (e) {
      error.value = errMsg(e);
      throw e;
    } finally {
      markRefreshing(providerKey, false);
    }
  }

  async function refreshAll() {
    refreshingAll.value = true;
    try {
      const views = await quotaApi.refreshAll();
      for (const v of views) upsert(v);
      error.value = null;
    } catch (e) {
      error.value = errMsg(e);
      throw e;
    } finally {
      refreshingAll.value = false;
    }
  }

  return {
    views,
    loading,
    error,
    refreshingKeys,
    refreshingAll,
    loaded,
    list,
    refreshOne,
    refreshAll,
  };
});
