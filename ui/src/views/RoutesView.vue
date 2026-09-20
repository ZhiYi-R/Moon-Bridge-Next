<script setup lang="ts">
import { Check, GitBranch, Pencil, Plus, Trash2, X } from "lucide-vue-next";
import { computed, onActivated, onMounted, reactive, ref } from "vue";

import Alert from "@/components/ui/Alert.vue";
import Button from "@/components/ui/Button.vue";
import EmptyState from "@/components/ui/EmptyState.vue";
import Input from "@/components/ui/Input.vue";
import Select from "@/components/ui/Select.vue";
import { useConfirm } from "@/composables/useConfirm";
import { useToast } from "@/composables/useToast";
import { errMsg, modelApi, providerApi, routeApi, type ModelDef, type Provider, type Route } from "@/lib/api";

const { confirm } = useConfirm();
const toast = useToast();
const routes = ref<Route[]>([]);
const providers = ref<Provider[]>([]);
const models = ref<ModelDef[]>([]);
const loading = ref(false);
const error = ref<string | null>(null);

// ── 行内编辑：editAlias = 正在编辑的行别名，adding = 末尾新增虚拟行 ──
const editAlias = ref<string | null>(null);
const adding = ref(false);
const form = reactive<Route>({ alias: "", modelSlug: "", providerKey: "", extra: null });
const editingRow = computed(() => editAlias.value !== null || adding.value);

const providerOptions = computed(() => providers.value.map((p) => ({ value: p.key, label: p.key })));
const modelOptions = computed(() =>
  models.value.map((m) => ({ value: m.slug, label: m.displayName ? `${m.slug} · ${m.displayName}` : m.slug })),
);

async function load() {
  loading.value = true;
  try {
    [routes.value, providers.value, models.value] = await Promise.all([
      routeApi.list(),
      providerApi.list(),
      modelApi.list().catch(() => [] as ModelDef[]),
    ]);
    error.value = null;
  } catch (e) {
    error.value = errMsg(e);
  } finally {
    loading.value = false;
  }
}

function newRoute() {
  Object.assign(form, { alias: "", modelSlug: "", providerKey: "", extra: null });
  error.value = null;
  editAlias.value = null;
  adding.value = true;
}

function editRoute(r: Route) {
  Object.assign(form, { alias: r.alias, modelSlug: r.modelSlug, providerKey: r.providerKey, extra: r.extra });
  error.value = null;
  adding.value = false;
  editAlias.value = r.alias;
}

function cancelEdit() {
  editAlias.value = null;
  adding.value = false;
  error.value = null;
}

/** 行内快捷键：Enter 确认 / Esc 取消。Enter 不劫持按钮（✓✗/Select 自身用 Enter 点击）。 */
function onRowKeydown(e: KeyboardEvent, editing: boolean) {
  if (!editing) return;
  if (e.key === "Escape") cancelEdit();
  else if (e.key === "Enter" && (e.target as HTMLElement).tagName !== "BUTTON") void save();
}

async function save() {
  const alias = form.alias.trim();
  if (!alias) {
    error.value = "别名为必填项";
    return;
  }
  if (adding.value && routes.value.some((r) => r.alias === alias)) {
    error.value = `别名 “${alias}” 已存在`;
    return;
  }
  if (!form.modelSlug) {
    error.value = models.value.length === 0 ? "暂无模型定义，请先到「模型」页创建" : "请选择上游模型";
    return;
  }
  if (!form.providerKey) {
    error.value = providers.value.length === 0 ? "暂无上游服务，请先到「上游服务」页创建" : "请选择上游服务";
    return;
  }
  try {
    const rec: Route = { ...form, alias };
    await routeApi.save(rec);
    const wasNew = adding.value;
    cancelEdit();
    toast.success(wasNew ? `路由 “${alias}” 已创建` : `路由 “${alias}” 已更新`);
    // 服务端只写这一行：按别名原地增改，无需整表重拉
    routes.value = routes.value.some((r) => r.alias === alias)
      ? routes.value.map((r) => (r.alias === alias ? rec : r))
      : [...routes.value, rec];
  } catch (e) {
    error.value = errMsg(e);
  }
}

async function remove(alias: string) {
  if (!(await confirm({ title: "删除路由", message: `确认删除路由 “${alias}”？` }))) return;
  try {
    await routeApi.remove(alias);
    toast.success(`路由 “${alias}” 已删除`);
    // 删除只影响这一行：本地移除，无需重拉整表
    routes.value = routes.value.filter((r) => r.alias !== alias);
  } catch (e) {
    error.value = errMsg(e);
  }
}

onMounted(() => {
  void load();
});

// keep-alive 下切回本页：第二次起静默重拉，不闪 loading
let activated = false;
onActivated(() => {
  if (activated) void load();
  activated = true;
});
</script>

<template>
  <div class="flex h-full min-h-0 flex-col">
    <Alert v-if="error" class="shrink-0">{{ error }}</Alert>

    <!-- 路由别名：满版面板 -->
    <div class="flex min-h-0 flex-1 flex-col overflow-hidden">
      <div class="scrollbar-thin min-h-0 flex-1 overflow-y-auto px-5 py-4">
        <EmptyState v-if="loading && routes.length === 0">加载中…</EmptyState>
        <EmptyState v-else-if="routes.length === 0 && !adding" :icon="GitBranch">
          暂无路由别名。
          <template #action>
            <Button size="sm" @click="newRoute"><Plus class="size-4" /> 新建路由</Button>
          </template>
        </EmptyState>
        <table v-else class="w-full text-sm">
          <thead class="thead-sticky">
            <tr class="border-b text-left text-muted-foreground">
              <th class="py-2 font-medium">别名</th>
              <th class="py-2 font-medium">上游模型</th>
              <th class="py-2 font-medium">上游服务</th>
              <th class="w-16 py-2 text-center font-medium">操作</th>
            </tr>
          </thead>
          <tbody>
            <tr
              v-for="r in routes"
              :key="r.alias"
              class="border-b transition-colors last:border-0 hover:bg-accent/40"
              @keydown="onRowKeydown($event, editAlias === r.alias)"
            >
              <template v-if="editAlias === r.alias">
                <!-- 别名是主键：编辑态保持只读 -->
                <td class="py-1 font-mono text-xs">{{ r.alias }}</td>
                <td class="py-1 pr-2">
                  <Select v-model="form.modelSlug" :options="modelOptions" placeholder="选择模型…" small searchable />
                </td>
                <td class="py-1 pr-2">
                  <Select v-model="form.providerKey" :options="providerOptions" placeholder="选择上游服务" small />
                </td>
                <td class="py-1">
                  <div class="flex justify-center gap-0.5">
                    <Button variant="ghost" size="icon" class="size-7" title="确认" @click="save">
                      <Check class="size-3.5" />
                    </Button>
                    <Button variant="ghost" size="icon" class="size-7" title="取消" @click="cancelEdit">
                      <X class="size-3.5" />
                    </Button>
                  </div>
                </td>
              </template>
              <template v-else>
                <td class="py-2 font-mono text-xs">{{ r.alias }}</td>
                <td class="py-2 font-mono text-xs">{{ r.modelSlug }}</td>
                <td class="py-2 font-mono text-xs text-muted-foreground">{{ r.providerKey }}</td>
                <td class="py-2">
                  <div class="flex justify-center gap-0.5">
                    <Button variant="ghost" size="icon" class="size-7" title="编辑" :disabled="editingRow" @click="editRoute(r)">
                      <Pencil class="size-3.5" />
                    </Button>
                    <Button variant="ghost" size="icon" class="size-7" title="删除" :disabled="editingRow" @click="remove(r.alias)">
                      <Trash2 class="size-3.5 text-destructive" />
                    </Button>
                  </div>
                </td>
              </template>
            </tr>
            <!-- 尾行：常态是「新建路由」入口，点击原位展开成编辑行 -->
            <tr v-if="!adding" class="last:border-0">
              <td colspan="4" class="py-1">
                <button
                  type="button"
                  class="flex w-full items-center gap-1.5 rounded-sm py-1.5 text-xs text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring disabled:pointer-events-none disabled:opacity-50"
                  :disabled="editingRow"
                  @click="newRoute"
                >
                  <Plus class="size-3.5" /> 新建路由
                </button>
              </td>
            </tr>
            <!-- 新增虚拟行 -->
            <tr v-else class="border-b last:border-0" @keydown="onRowKeydown($event, true)">
              <td class="py-1 pr-2">
                <Input v-model="form.alias" class="h-7 px-2 font-mono text-xs" placeholder="别名" autofocus />
              </td>
              <td class="py-1 pr-2">
                <Select v-model="form.modelSlug" :options="modelOptions" placeholder="选择模型…" small searchable />
              </td>
              <td class="py-1 pr-2">
                <Select v-model="form.providerKey" :options="providerOptions" placeholder="选择上游服务" small />
              </td>
              <td class="py-1">
                <div class="flex justify-center gap-0.5">
                  <Button variant="ghost" size="icon" class="size-7" title="确认" @click="save">
                    <Check class="size-3.5" />
                  </Button>
                  <Button variant="ghost" size="icon" class="size-7" title="取消" @click="cancelEdit">
                    <X class="size-3.5" />
                  </Button>
                </div>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </div>
  </div>
</template>
