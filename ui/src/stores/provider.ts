import { defineStore } from "pinia";
import { ref } from "vue";

import { errMsg, providerApi, type Provider } from "@/lib/api";

/** Provider 列表 store：加载 / 保存 / 删除。 */
export const useProviderStore = defineStore("provider", () => {
  const providers = ref<Provider[]>([]);
  const loading = ref(false);
  const error = ref<string | null>(null);

  async function load() {
    loading.value = true;
    try {
      providers.value = await providerApi.list();
      error.value = null;
    } catch (e) {
      error.value = errMsg(e);
    } finally {
      loading.value = false;
    }
  }

  async function save(p: Provider) {
    await providerApi.save(p);
    await load();
  }

  async function remove(key: string) {
    await providerApi.remove(key);
    await load();
  }

  return { providers, loading, error, load, save, remove };
});
