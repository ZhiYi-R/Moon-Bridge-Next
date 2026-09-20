import { defineStore } from "pinia";
import { ref } from "vue";

import { balanceApi, errMsg, type BalanceCard, type BalanceCardView } from "@/lib/api";

/** 余额卡片 store：列表 + 单卡/全量查询，写操作按返回值原地更新，不整表重拉。 */
export const useBalanceStore = defineStore("balance", () => {
  const cards = ref<BalanceCardView[]>([]);
  const loading = ref(false);
  const error = ref<string | null>(null);
  /** 正在查询中的卡片 key（刷新图标按此旋转）。 */
  const refreshingKeys = ref(new Set<string>());
  const refreshingAll = ref(false);
  /** 是否已成功加载过一次：路由缓存复用时用于静默刷新。 */
  const loaded = ref(false);

  /** 用新视图替换同 key 卡片；不存在则按 position 插入。 */
  function upsert(view: BalanceCardView) {
    const idx = cards.value.findIndex((c) => c.key === view.key);
    if (idx >= 0) cards.value[idx] = view;
    else cards.value.push(view);
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
        cards.value = await balanceApi.list();
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

  /** 保存卡片：后端返回 null，故以本地副本原地更新（保留已有查询结果）。 */
  async function save(card: BalanceCard) {
    await balanceApi.save(card);
    const view: BalanceCardView = {
      ...card,
      results: cards.value.find((c) => c.key === card.key)?.results ?? [],
    };
    upsert(view);
    error.value = null;
  }

  async function remove(key: string) {
    await balanceApi.remove(key);
    cards.value = cards.value.filter((c) => c.key !== key);
    error.value = null;
  }

  /** 刷新整张卡片（手动刷新只允许到卡粒度，key 行不提供单刷入口）。 */
  async function refreshOne(key: string) {
    markRefreshing(key, true);
    try {
      upsert(await balanceApi.refresh(key));
      error.value = null;
    } catch (e) {
      error.value = errMsg(e);
      throw e;
    } finally {
      markRefreshing(key, false);
    }
  }

  async function refreshAll() {
    refreshingAll.value = true;
    try {
      const views = await balanceApi.refreshAll();
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
    cards,
    loading,
    error,
    refreshingKeys,
    refreshingAll,
    loaded,
    list,
    save,
    remove,
    refreshOne,
    refreshAll,
  };
});
