<script setup lang="ts">
import { Pencil, Plus, Trash2 } from "lucide-vue-next";
import { onMounted, reactive, ref } from "vue";

import Badge from "@/components/ui/Badge.vue";
import Button from "@/components/ui/Button.vue";
import Card from "@/components/ui/Card.vue";
import Input from "@/components/ui/Input.vue";
import Label from "@/components/ui/Label.vue";
import Modal from "@/components/ui/Modal.vue";
import Select from "@/components/ui/Select.vue";
import { useConfirm } from "@/composables/useConfirm";
import {
  errMsg,
  pluginApi,
  type PluginBinding,
  type PluginRecord,
  type Provider,
  type ProviderEndpoint,
} from "@/lib/api";
import { useGatewayStore } from "@/stores/gateway";
import { useProviderStore } from "@/stores/provider";

const store = useProviderStore();
const gateway = useGatewayStore();
const { confirm } = useConfirm();
/** 弹窗内错误（表单校验 / 保存失败） */
const error = ref<string | null>(null);
/** 列表操作错误（弹窗外展示） */
const listError = ref<string | null>(null);
const editing = ref(false);

const PROTOCOLS = ["anthropic", "openai-response", "openai-chat", "google-genai"];
const protocolOptions = PROTOCOLS.map((p) => ({ value: p, label: p }));

const pluginList = ref<PluginRecord[]>([]);
/** 插件相对当前 provider 的三态：inherit=跟随全局 / on=启用 / off=禁用 */
type TriState = "inherit" | "on" | "off";
const pluginStates = reactive<Record<string, TriState>>({});
/** 打开弹窗时已存在的 provider 维度 binding（用于保存时 diff 删除） */
const originalBindings = ref<Record<string, PluginBinding | undefined>>({});

const TRI_OPTIONS = [
  { value: "inherit", label: "跟随全局" },
  { value: "on", label: "启用" },
  { value: "off", label: "禁用" },
];

function emptyEndpoint(): ProviderEndpoint {
  return { protocol: "anthropic", baseUrl: "", apiKey: "" };
}

function emptyProvider(): Provider {
  return {
    key: "",
    endpoints: [emptyEndpoint()],
    version: null,
    userAgent: null,
    webSearch: null,
    extra: {},
    enabled: true,
    createdAt: 0,
    updatedAt: 0,
  };
}

/** 端点协议集合（去重，用于表格展示） */
function endpointProtocols(p: Provider): string {
  return [...new Set(p.endpoints.map((e) => e.protocol))].join(" · ");
}

const form = reactive<Provider>(emptyProvider());

function addEndpoint() {
  form.endpoints.push(emptyEndpoint());
}

function removeEndpoint(i: number) {
  form.endpoints.splice(i, 1);
  if (form.endpoints.length === 0) form.endpoints.push(emptyEndpoint());
}

async function loadPluginStates(providerKey: string) {
  const list = await pluginApi.list().catch(() => [] as PluginRecord[]);
  pluginList.value = list;
  resetPluginStates();
  const bindings = await pluginApi.bindingListByScope("provider").catch(() => []);
  for (const b of bindings) {
    if (b.scopeKey !== providerKey) continue;
    pluginStates[b.pluginName] = b.enabled ? "on" : "off";
    originalBindings.value[b.pluginName] = b;
  }
}

/** 初始化三态表：全部默认「跟随全局」 */
function resetPluginStates() {
  for (const name of Object.keys(pluginStates)) delete pluginStates[name];
  originalBindings.value = {};
  for (const p of pluginList.value) pluginStates[p.name] = "inherit";
}

function newProvider() {
  Object.assign(form, emptyProvider());
  error.value = null;
  editing.value = true;
  void loadPluginStates("");
}

/** 关闭弹窗并清除表单错误 */
function closeModal() {
  editing.value = false;
  error.value = null;
}

async function editProvider(p: Provider) {
  // JSON 深拷贝隔离编辑态（structuredClone 无法克隆 Vue 响应式 Proxy）
  Object.assign(form, JSON.parse(JSON.stringify(p)));
  error.value = null;
  editing.value = true;
  await loadPluginStates(p.key);
}

async function save() {
  if (!form.key.trim()) {
    error.value = "唯一标识为必填项";
    return;
  }
  if (form.endpoints.every((e) => !e.baseUrl.trim())) {
    error.value = "至少需要一个 Base URL";
    return;
  }
  try {
    await store.save({ ...form });
    // 插件三态 diff：非 inherit 落 binding，inherit 删除已有 binding
    let bindingsChanged = false;
    for (const p of pluginList.value) {
      const state = pluginStates[p.name] ?? "inherit";
      const had = p.name in originalBindings.value;
      if (state === "inherit") {
        if (had) {
          await pluginApi.bindingRemove(p.name, "provider", form.key);
          bindingsChanged = true;
        }
      } else {
        await pluginApi.bindingSave({
          pluginName: p.name,
          scope: "provider",
          scopeKey: form.key,
          enabled: state === "on",
          config: {},
        });
        bindingsChanged = true;
      }
    }
    // binding 在网关启动时装配进门控表，变更后需重启生效
    if (bindingsChanged) await gateway.restart();
    closeModal();
  } catch (e) {
    error.value = errMsg(e);
  }
}

async function remove(key: string) {
  if (!(await confirm({ title: "删除上游服务", message: `确认删除上游服务 “${key}”？` }))) return;
  try {
    await store.remove(key);
  } catch (e) {
    listError.value = errMsg(e);
  }
}

onMounted(() => {
  void store.load();
  void pluginApi
    .list()
    .then((list) => {
      pluginList.value = list;
      resetPluginStates();
    })
    .catch(() => {});
});
</script>

<template>
  <div class="flex h-full min-h-0 flex-col">
    <div
      v-if="listError"
      class="shrink-0 rounded-md border border-destructive/40 bg-destructive/10 px-4 py-2 text-sm text-destructive"
    >
      {{ listError }}
    </div>

    <!-- 编辑弹窗 -->
    <Modal :open="editing" :title="form.createdAt ? '编辑上游服务' : '新建上游服务'" @close="closeModal">
      <div
        v-if="error"
        class="mb-3 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
      >
        {{ error }}
      </div>
      <div class="grid gap-4 md:grid-cols-2">
        <div class="space-y-1.5">
          <Label for="p-key">唯一标识</Label>
          <Input id="p-key" v-model="form.key" placeholder="如 deepseek" :disabled="!!form.createdAt" />
        </div>
        <div class="space-y-1.5">
          <Label for="p-version">协议版本头（可选）</Label>
          <Input id="p-version" v-model="form.version" placeholder="2023-06-01" />
        </div>
        <div class="flex items-center gap-2 md:col-span-2">
          <input id="p-enabled" v-model="form.enabled" type="checkbox" class="size-4 accent-primary" />
          <Label for="p-enabled">启用</Label>
        </div>
      </div>

      <!-- 端点列表（协议绑定在端点上，按序故障转移） -->
      <div class="mt-4 space-y-2">
        <div class="flex items-center justify-between">
          <Label>端点</Label>
          <span class="text-xs text-muted-foreground">按序故障转移；Key 留空沿用上一非空 Key</span>
        </div>
        <!-- 列头 -->
        <div class="grid grid-cols-[10rem_1fr_1fr_2rem] items-center gap-2 text-xs text-muted-foreground">
          <span>协议</span>
          <span>Base URL</span>
          <span>API Key</span>
          <span />
        </div>
        <div
          v-for="(ep, i) in form.endpoints"
          :key="i"
          class="grid grid-cols-[10rem_1fr_1fr_2rem] items-center gap-2"
        >
          <Select v-model="ep.protocol" :options="protocolOptions" />
          <Input v-model="ep.baseUrl" placeholder="https://api.anthropic.com" />
          <Input v-model="ep.apiKey" type="password" placeholder="sk-..." />
          <Button variant="ghost" size="icon" class="size-8" @click="removeEndpoint(i)">
            <Trash2 class="size-3.5 text-destructive" />
          </Button>
        </div>
        <Button variant="outline" size="sm" @click="addEndpoint">
          <Plus class="size-4" /> 添加端点
        </Button>
      </div>

      <!-- 插件三态开关 -->
      <div v-if="pluginList.length > 0" class="mt-4 border-t pt-4">
        <div class="flex items-center justify-between">
          <Label>插件</Label>
          <span class="text-xs text-muted-foreground">变更保存后自动重启网关生效</span>
        </div>
        <div class="mt-2 space-y-1.5">
          <div
            v-for="p in pluginList"
            :key="p.name"
            class="flex items-center justify-between gap-4"
          >
            <span class="min-w-0 truncate font-mono text-xs text-muted-foreground">
              {{ p.name }}
              <span v-if="!p.enabled" class="text-warning">（全局停用）</span>
            </span>
            <div class="w-32 shrink-0">
              <Select v-model="pluginStates[p.name]" :options="TRI_OPTIONS" small />
            </div>
          </div>
        </div>
      </div>

      <template #footer>
        <Button variant="ghost" size="sm" @click="closeModal">取消</Button>
        <Button size="sm" @click="save">保存</Button>
      </template>
    </Modal>

    <!-- 上游服务：满版单卡 -->
    <Card class="flex min-h-0 flex-1 flex-col overflow-hidden">
      <div class="flex shrink-0 items-center justify-end border-b px-5 py-3">
        <Button size="sm" @click="newProvider">
          <Plus class="size-4" /> 新建
        </Button>
      </div>
      <div class="scrollbar-thin min-h-0 flex-1 overflow-y-auto px-5 py-4">
        <div
          v-if="store.providers.length === 0"
          class="rounded-md border border-dashed p-8 text-center text-sm text-muted-foreground"
        >
          暂无上游服务，点击「新建」添加。
        </div>
        <table v-else class="w-full text-sm">
          <thead>
            <tr class="border-b text-left text-muted-foreground">
              <th class="pb-2 font-medium">Key</th>
              <th class="pb-2 font-medium">协议</th>
              <th class="pb-2 font-medium">端点</th>
              <th class="pb-2 font-medium">状态</th>
              <th class="pb-2 text-right font-medium">操作</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="p in store.providers" :key="p.key" class="border-b last:border-0">
              <td class="py-2 font-mono text-xs">{{ p.key }}</td>
              <td class="py-2 font-mono text-xs text-muted-foreground">{{ endpointProtocols(p) }}</td>
              <td class="max-w-[280px] truncate py-2 text-muted-foreground">
                {{ p.endpoints[0]?.baseUrl }}
                <span v-if="p.endpoints.length > 1" class="ml-1 font-mono text-xs">×{{ p.endpoints.length }}</span>
              </td>
              <td class="py-2">
                <Badge :variant="p.enabled ? 'success' : 'secondary'">
                  {{ p.enabled ? "启用" : "停用" }}
                </Badge>
              </td>
              <td class="py-2">
                <div class="flex justify-end gap-0.5">
                  <Button variant="ghost" size="icon" class="size-7" @click="editProvider(p)">
                    <Pencil class="size-3.5" />
                  </Button>
                  <Button variant="ghost" size="icon" class="size-7" @click="remove(p.key)">
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
