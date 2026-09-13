<script setup lang="ts">
import { Pencil, Plus, Trash2 } from "lucide-vue-next";
import { computed, onMounted, reactive, ref } from "vue";

import Button from "@/components/ui/Button.vue";
import Card from "@/components/ui/Card.vue";
import Input from "@/components/ui/Input.vue";
import Label from "@/components/ui/Label.vue";
import Modal from "@/components/ui/Modal.vue";
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
const editing = ref(false);
const isNew = ref(false);
const error = ref<string | null>(null);

const form = reactive<Route>({ alias: "", modelSlug: "", providerKey: "", extra: null });

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
  isNew.value = true;
  Object.assign(form, { alias: "", modelSlug: "", providerKey: "", extra: null });
  error.value = null;
  editing.value = true;
}

function editRoute(r: Route) {
  isNew.value = false;
  Object.assign(form, { alias: r.alias, modelSlug: r.modelSlug, providerKey: r.providerKey, extra: r.extra });
  error.value = null;
  editing.value = true;
}

async function save() {
  const alias = form.alias.trim();
  if (!alias) {
    error.value = "别名为必填项";
    return;
  }
  if (isNew.value && routes.value.some((r) => r.alias === alias)) {
    error.value = `别名 “${alias}” 已存在`;
    return;
  }
  if (!form.modelSlug) {
    error.value = "请选择上游模型";
    return;
  }
  if (!form.providerKey) {
    error.value = "请选择上游服务";
    return;
  }
  try {
    await routeApi.save({ ...form, alias });
    editing.value = false;
    toast.success(isNew.value ? `路由 “${alias}” 已创建` : `路由 “${alias}” 已更新`);
    await load();
  } catch (e) {
    error.value = errMsg(e);
  }
}

async function remove(alias: string) {
  if (!(await confirm({ title: "删除路由", message: `确认删除路由 “${alias}”？` }))) return;
  try {
    await routeApi.remove(alias);
    toast.success(`路由 “${alias}” 已删除`);
    await load();
  } catch (e) {
    error.value = errMsg(e);
  }
}

onMounted(load);
</script>

<template>
  <div class="flex h-full min-h-0 flex-col">
    <div
      v-if="error && !editing"
      class="shrink-0 rounded-md border border-destructive/40 bg-destructive/10 px-4 py-2 text-sm text-destructive"
    >
      {{ error }}
    </div>

    <!-- 新建/编辑路由弹窗 -->
    <Modal :open="editing" :title="isNew ? '新建路由' : '编辑路由'" width="max-w-md" @close="editing = false">
      <div v-if="error" class="mb-3 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive">
        {{ error }}
      </div>
      <div class="grid gap-4">
        <div class="space-y-1.5">
          <Label for="r-alias">别名</Label>
          <Input id="r-alias" v-model="form.alias" placeholder="如 moonbridge" :disabled="!isNew" />
          <p class="text-xs text-muted-foreground">客户端请求中的 model 字段值</p>
        </div>
        <div class="space-y-1.5">
          <Label>上游模型</Label>
          <Select v-model="form.modelSlug" :options="modelOptions" placeholder="选择模型…" searchable />
          <p v-if="models.length === 0" class="text-xs text-muted-foreground">
            暂无模型定义，请先到「模型」页创建。
          </p>
        </div>
        <div class="space-y-1.5">
          <Label>上游服务</Label>
          <Select v-model="form.providerKey" :options="providerOptions" placeholder="选择上游服务" />
          <p v-if="providers.length === 0" class="text-xs text-muted-foreground">
            暂无上游服务，请先到「上游服务」页创建。
          </p>
        </div>
      </div>
      <template #footer>
        <Button variant="ghost" size="sm" @click="editing = false">取消</Button>
        <Button size="sm" @click="save">保存</Button>
      </template>
    </Modal>

    <!-- 路由别名：满版单卡 -->
    <Card class="flex min-h-0 flex-1 flex-col overflow-hidden">
      <div class="flex shrink-0 items-center justify-end border-b px-5 py-3">
        <Button size="sm" @click="newRoute"><Plus class="size-4" /> 新建</Button>
      </div>
      <div class="scrollbar-thin min-h-0 flex-1 overflow-y-auto px-5 py-4">
        <div
          v-if="loading && routes.length === 0"
          class="rounded-md border border-dashed p-8 text-center text-sm text-muted-foreground"
        >
          加载中…
        </div>
        <div
          v-else-if="routes.length === 0"
          class="rounded-md border border-dashed p-8 text-center text-sm text-muted-foreground"
        >
          暂无路由别名。
        </div>
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
            <tr v-for="r in routes" :key="r.alias" class="border-b last:border-0">
              <td class="py-2 font-mono text-xs">{{ r.alias }}</td>
              <td class="py-2 font-mono text-xs">{{ r.modelSlug }}</td>
              <td class="py-2 font-mono text-xs text-muted-foreground">{{ r.providerKey }}</td>
              <td class="py-2">
                <div class="flex justify-center gap-0.5">
                  <Button variant="ghost" size="icon" class="size-7" title="编辑" @click="editRoute(r)">
                    <Pencil class="size-3.5" />
                  </Button>
                  <Button variant="ghost" size="icon" class="size-7" title="删除" @click="remove(r.alias)">
                    <Trash2 class="size-3.5 text-destructive" />
                  </Button>
                </div>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </Card>
  </div>
</template>
