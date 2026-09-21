<script setup lang="ts">
import { ArrowLeft, ChevronDown, ChevronRight, Pencil, Plus, RefreshCw, Search, Server, Trash2, X } from "lucide-vue-next";
import {
  computed,
  onActivated,
  onDeactivated,
  onMounted,
  onUnmounted,
  reactive,
  ref,
  watch,
} from "vue";

import Alert from "@/components/ui/Alert.vue";
import Badge from "@/components/ui/Badge.vue";
import Button from "@/components/ui/Button.vue";
import Checkbox from "@/components/ui/Checkbox.vue";
import EmptyState from "@/components/ui/EmptyState.vue";
import Input from "@/components/ui/Input.vue";
import Label from "@/components/ui/Label.vue";
import Pagination from "@/components/ui/Pagination.vue";
import Select from "@/components/ui/Select.vue";
import Switch from "@/components/ui/Switch.vue";
import { useConfirm } from "@/composables/useConfirm";
import { useToast } from "@/composables/useToast";
import {
  errMsg,
  modelApi,
  pluginApi,
  quotaApi,
  type ModelDef,
  type Offer,
  type PluginBinding,
  type PluginRecord,
  type Provider,
  type ProviderEndpoint,
  type QuotaKeyResult,
} from "@/lib/api";
import { useGatewayStore } from "@/stores/gateway";
import { useProviderStore } from "@/stores/provider";

const store = useProviderStore();
const gateway = useGatewayStore();
const { confirm } = useConfirm();
const toast = useToast();
/** 编辑器内错误（表单校验 / 保存失败） */
const error = ref<string | null>(null);
/** 列表操作错误（编辑器外展示） */
const listError = ref<string | null>(null);
const editing = ref(false);

const PROTOCOLS = ["anthropic", "openai-response", "openai-chat", "google-genai"];
const protocolOptions = PROTOCOLS.map((p) => ({ value: p, label: p }));

const pluginList = ref<PluginRecord[]>([]);
/** 只有 core 类插件参与请求链路的三态绑定；quota 插件由下方配额区单独绑定。 */
const hookPlugins = computed(() =>
  pluginList.value.filter((p) => (p.category ?? "core") === "core"),
);
const quotaPluginOptions = computed(() => [
  { value: "", label: "不绑定" },
  ...pluginList.value
    .filter((p) => p.category === "quota")
    .map((p) => ({ value: p.name, label: p.name })),
]);
/** 插件相对当前 provider 的三态：inherit=跟随全局 / on=启用 / off=禁用 */
type TriState = "inherit" | "on" | "off";
const pluginStates = reactive<Record<string, TriState>>({});
/** 打开编辑器时已存在的 provider 维度 binding（用于保存时 diff 删除） */
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
    quotaPluginRef: "",
    quotaIntervalSecs: 0,
    quotaEnabled: false,
    quotaConfig: {},
    createdAt: 0,
    updatedAt: 0,
  };
}

function endpointProtocols(p: Provider): string {
  return [...new Set(p.endpoints.map((e) => e.protocol))].join(" · ");
}

const form = reactive<Provider>(emptyProvider());

// ── 配额配置：按所选插件的 configSchema 渲染字段；无 schema = 无需配置 ──
const quotaTesting = ref(false);
const quotaTestResult = ref<QuotaKeyResult[] | null>(null);

interface QuotaSchemaField {
  name: string;
  type: string;
  label: string;
  help: string;
  secret: boolean;
  default: unknown;
}

const selectedQuotaPlugin = computed(() => pluginList.value.find((p) => p.name === form.quotaPluginRef));

/** 所选配额插件的实例配置字段（config_schema 声明）；无 schema 返回空数组。 */
const quotaSchemaFields = computed<QuotaSchemaField[]>(() => {
  const s = selectedQuotaPlugin.value?.configSchema;
  if (!s || typeof s !== "object" || Array.isArray(s)) return [];
  return Object.entries(s as Record<string, Record<string, unknown>>).map(([name, f]) => ({
    name,
    type: typeof f.type === "string" ? f.type : "string",
    label: typeof f.label === "string" ? f.label : name,
    help: typeof f.help === "string" ? f.help : "",
    secret: f.secret === true,
    default: f.default,
  }));
});

/** 校验并归一化 quotaConfig：number 字段转数字，剔除 schema 之外的键与空串。 */
function normalizeQuotaConfig(): boolean {
  const out: Record<string, unknown> = {};
  const src = form.quotaConfig && typeof form.quotaConfig === "object" ? form.quotaConfig : {};
  for (const f of quotaSchemaFields.value) {
    let v = (src as Record<string, unknown>)[f.name];
    if (v === undefined || v === null || v === "") continue;
    if (f.type === "number") {
      const n = Number(v);
      if (!Number.isFinite(n)) {
        error.value = `配额配置「${f.label}」需要是数字`;
        return false;
      }
      v = n;
    } else if (f.type === "boolean") {
      v = v === true || v === "true";
    } else {
      v = String(v);
    }
    out[f.name] = v;
  }
  form.quotaConfig = out;
  return true;
}

/** 换插件时把 quotaConfig 重置为该插件 schema 的默认值。 */
function applyQuotaDefaults() {
  const out: Record<string, unknown> = {};
  for (const f of quotaSchemaFields.value) if (f.default !== undefined) out[f.name] = f.default;
  form.quotaConfig = out;
}

// 表单装载期间 Object.assign 会改写 quotaPluginRef——此时绝不能重置 quotaConfig
// （会覆盖 Provider 已存的配置）。sync flush + 标志位精确拦截装载期触发。
let loadingForm = false;
watch(
  () => form.quotaPluginRef,
  () => {
    if (loadingForm) return;
    applyQuotaDefaults();
    quotaTestResult.value = null;
  },
  { flush: "sync" },
);

/** 用当前表单配置 dry-run 一次配额查询（不写入数据库）。 */
async function runQuotaTest() {
  if (!normalizeQuotaConfig()) return;
  quotaTesting.value = true;
  quotaTestResult.value = null;
  try {
    quotaTestResult.value = await quotaApi.test({ ...form, endpoints: form.endpoints.map((e) => ({ ...e })) });
  } catch (e) {
    error.value = errMsg(e);
  } finally {
    quotaTesting.value = false;
  }
}

function addEndpoint() {
  form.endpoints.push(emptyEndpoint());
}

function removeEndpoint(i: number) {
  form.endpoints.splice(i, 1);
  if (form.endpoints.length === 0) form.endpoints.push(emptyEndpoint());
}

// ── 端点 ↔ 模型绑定：写入报价的 endpointProtocol，保存时统一 diff ──
const allModels = ref<ModelDef[]>([]);
const providerOffers = ref<Offer[]>([]);
/** 打开编辑器时的报价快照，保存时对比出需要写入的绑定变更 */
const originalOffers = ref<Offer[]>([]);
/** 当前展开的协议绑定面板（绑定对象是协议组而非单个端点） */
const expandedProtocol = ref<string | null>(null);
const epModelQuery = ref("");
const epModelPage = ref(1);
/** 勾选面板每页行数：models.dev 全量导入后模型可达上万，避免一次渲染 */
const EP_PAGE_SIZE = 50;

const filteredEpModels = computed(() => {
  const q = epModelQuery.value.trim().toLowerCase();
  const list = q
    ? allModels.value.filter(
        (m) => m.slug.toLowerCase().includes(q) || (m.displayName ?? "").toLowerCase().includes(q),
      )
    : allModels.value;
  const pageCount = Math.max(1, Math.ceil(list.length / EP_PAGE_SIZE));
  return {
    total: list.length,
    pageCount,
    items: list.slice((epModelPage.value - 1) * EP_PAGE_SIZE, epModelPage.value * EP_PAGE_SIZE),
  };
});
watch([epModelQuery, expandedProtocol], () => {
  epModelPage.value = 1;
});
// 收起下拉只跟协议面板的展开态绑定：在搜索框里打字不能把下拉关掉
watch(expandedProtocol, () => {
  epDropdownOpen.value = false;
});
watch(
  () => filteredEpModels.value.pageCount,
  (c) => {
    if (epModelPage.value > c) epModelPage.value = c;
  },
);

/** 端点列表用到的协议集合（去重保序，模型绑定按协议组） */
const endpointProtocolList = computed(() => [...new Set(form.endpoints.map((e) => e.protocol))]);

/** 当前展开协议组下的已勾选模型集合（同协议端点共享） */
const checkedModels = computed(() => {
  return new Set(
    providerOffers.value.filter((o) => o.endpointProtocol === expandedProtocol.value).map((o) => o.modelSlug),
  );
});

function boundCount(protocol: string): number {
  return providerOffers.value.filter((o) => o.endpointProtocol === protocol).length;
}

const checkedModelSlugs = computed(() => [...checkedModels.value].sort((a, b) => a.localeCompare(b)));

/** 下拉浮层：默认收起，仅聚焦/点击搜索框时展开；多选不自动收起。 */
const epDropdownOpen = ref(false);

function closeEpDropdown() {
  epDropdownOpen.value = false;
}

function onEpDocPointerDown(e: MouseEvent) {
  if (!epDropdownOpen.value) return;
  const t = e.target as HTMLElement | null;
  if (!t?.closest("[data-ep-model-panel]")) closeEpDropdown();
}

/** 下拉期间拦截 Esc：捕获阶段先收起下拉，避免误触发外层的返回/丢弃确认。 */
function onEpKey(e: KeyboardEvent) {
  if (e.key !== "Escape" || !epDropdownOpen.value) return;
  closeEpDropdown();
  e.stopPropagation();
}

function offerFor(slug: string): Offer | undefined {
  return providerOffers.value.find((o) => o.modelSlug === slug);
}

/** 勾选 = 仅走该协议端点组（保留已有定价）；取消勾选 = 回退「全部端点」 */
function toggleEndpointModel(protocol: string, slug: string) {
  const cur = offerFor(slug);
  if (cur?.endpointProtocol === protocol) {
    providerOffers.value = providerOffers.value.map((o) =>
      o.modelSlug === slug ? { ...o, endpointProtocol: null } : o,
    );
  } else if (cur) {
    providerOffers.value = providerOffers.value.map((o) =>
      o.modelSlug === slug ? { ...o, endpointProtocol: protocol } : o,
    );
  } else {
    providerOffers.value = [
      ...providerOffers.value,
      { providerKey: form.key, modelSlug: slug, pricing: null, endpointProtocol: protocol },
    ];
  }
}

function toggleProtocolPanel(protocol: string) {
  if (expandedProtocol.value === protocol) {
    expandedProtocol.value = null;
  } else {
    expandedProtocol.value = protocol;
    epModelQuery.value = "";
  }
}

async function loadModelsOnce() {
  if (allModels.value.length > 0) return;
  allModels.value = await modelApi.list().catch(() => [] as ModelDef[]);
}

async function loadOfferBindings(providerKey: string) {
  if (!providerKey) {
    providerOffers.value = [];
    originalOffers.value = [];
    return;
  }
  const list = await modelApi.offerList(providerKey).catch(() => [] as Offer[]);
  providerOffers.value = list.map((o) => ({ ...o }));
  originalOffers.value = list.map((o) => ({ ...o }));
}

/** 写入绑定 diff：仅保存 protocol 有变化的报价；新建 provider 时 form.key 已在前一步写入数据库 */
async function saveOfferChanges() {
  const orig = new Map(originalOffers.value.map((o) => [o.modelSlug, o]));
  for (const o of providerOffers.value) {
    const prev = orig.get(o.modelSlug);
    if (!prev || prev.endpointProtocol !== o.endpointProtocol) {
      await modelApi.offerSave({ ...o, providerKey: form.key });
    }
  }
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

/** 初始化三态表：全部默认「跟随全局」（仅 core 类插件参与）。 */
function resetPluginStates() {
  for (const name of Object.keys(pluginStates)) delete pluginStates[name];
  originalBindings.value = {};
  for (const p of hookPlugins.value) pluginStates[p.name] = "inherit";
}

// ── 未保存关闭确认：编辑器打开并加载完绑定后拍快照，离开时比对 ──
const formSnapshot = ref("");

function takeSnapshot() {
  formSnapshot.value = JSON.stringify({ f: form, o: providerOffers.value, p: pluginStates });
}

const dirty = computed(
  () =>
    JSON.stringify({ f: form, o: providerOffers.value, p: pluginStates }) !== formSnapshot.value,
);

async function closeGuard(): Promise<boolean> {
  if (!dirty.value) return true;
  return confirm({
    title: "关闭编辑",
    message: "有未保存的修改，确认丢弃并关闭？",
    confirmText: "丢弃修改",
  });
}

async function tryClose() {
  if (await closeGuard()) closeEditor();
}

async function newProvider() {
  loadingForm = true;
  Object.assign(form, emptyProvider());
  loadingForm = false;
  quotaTestResult.value = null;
  error.value = null;
  editing.value = true;
  expandedProtocol.value = null;
  providerOffers.value = [];
  originalOffers.value = [];
  void loadModelsOnce();
  await loadPluginStates("");
  takeSnapshot();
}

function closeEditor() {
  editing.value = false;
  error.value = null;
}

async function editProvider(p: Provider) {
  // JSON 深拷贝隔离编辑态（structuredClone 无法克隆 Vue 响应式 Proxy）
  loadingForm = true;
  Object.assign(form, JSON.parse(JSON.stringify(p)));
  loadingForm = false;
  // 补齐 schema 默认值的缺省键（存量值优先）
  for (const f of quotaSchemaFields.value) {
    const cfg = form.quotaConfig as Record<string, unknown>;
    if (f.default !== undefined && cfg[f.name] === undefined) cfg[f.name] = f.default;
  }
  quotaTestResult.value = null;
  error.value = null;
  editing.value = true;
  expandedProtocol.value = null;
  void loadModelsOnce();
  await loadOfferBindings(p.key);
  await loadPluginStates(p.key);
  takeSnapshot();
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
  if (!normalizeQuotaConfig()) return;
  try {
    // 服务端会回填 createdAt/updatedAt 并归一化端点顺序，本地拼不出最终记录：回退整表重拉
    await store.save({ ...form });
    // 插件三态 diff：非 inherit 落 binding，inherit 删除已有 binding（仅 core 类）
    let bindingsChanged = false;
    for (const p of hookPlugins.value) {
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
    // binding 在网关启动时装配进绑定表，变更后需重启生效
    if (bindingsChanged) await gateway.restart();
    await saveOfferChanges();
    toast.success(`上游服务 “${form.key}” 已保存`);
    closeEditor();
  } catch (e) {
    error.value = errMsg(e);
  }
}

async function remove(key: string) {
  if (!(await confirm({ title: "删除上游服务", message: `确认删除上游服务 “${key}”？` }))) return;
  try {
    await store.remove(key);
    toast.success(`上游服务 “${key}” 已删除`);
  } catch (e) {
    listError.value = errMsg(e);
  }
}

onMounted(() => {
  document.addEventListener("pointerdown", onEpDocPointerDown);
  document.addEventListener("keydown", onEpKey, true);
  void store.load();
  void loadPluginList();
});

// keep-alive 下切回本页：第二次起静默重拉；编辑器打开时不动，避免重置三态表
let activated = false;
onActivated(() => {
  document.addEventListener("pointerdown", onEpDocPointerDown);
  document.addEventListener("keydown", onEpKey, true);
  if (activated && !editing.value) {
    void store.load();
    void loadPluginList();
  }
  activated = true;
});

// 视图被缓存后不再卸载：模型下拉的外部点击 / Esc 监听改在停用时移除
onDeactivated(() => {
  document.removeEventListener("pointerdown", onEpDocPointerDown);
  document.removeEventListener("keydown", onEpKey, true);
});

onUnmounted(() => {
  document.removeEventListener("pointerdown", onEpDocPointerDown);
  document.removeEventListener("keydown", onEpKey, true);
});

function loadPluginList() {
  return pluginApi
    .list()
    .then((list) => {
      pluginList.value = list;
      resetPluginStates();
    })
    .catch(() => {});
}
</script>

<template>
  <div class="flex h-full min-h-0 flex-col">
    <Alert v-if="listError" class="shrink-0">{{ listError }}</Alert>

    <!-- 编辑视图：满页切换（与插件编辑器同一惯例），列表↔编辑器淡出/淡入 -->
    <Transition name="editor" mode="out-in">
    <div v-if="editing" key="editor" class="flex min-h-0 flex-1 flex-col overflow-hidden">
      <div class="flex shrink-0 items-center justify-between border-b px-5 py-3">
        <div class="flex items-center gap-2">
          <Button variant="ghost" size="icon" class="size-7" title="返回列表" @click="tryClose">
            <ArrowLeft class="size-4" />
          </Button>
          <h3 class="card-title">{{ form.createdAt ? "编辑上游服务" : "新建上游服务" }}</h3>
          <span v-if="form.key" class="truncate font-mono text-xs text-muted-foreground">
            {{ form.key }}
          </span>
        </div>
        <div class="flex items-center gap-3">
          <label class="flex items-center gap-1.5 text-xs text-muted-foreground">
            <Switch :checked="form.enabled" @update:checked="(v: boolean) => (form.enabled = v)" />
            启用
          </label>
          <Button variant="ghost" size="sm" @click="tryClose">取消</Button>
          <Button size="sm" @click="save">保存</Button>
        </div>
      </div>

      <div class="scrollbar-thin min-h-0 flex-1 overflow-y-auto">
        <div class="mx-auto w-full max-w-3xl px-5 py-4">
          <Alert v-if="error" class="mb-4 px-3">{{ error }}</Alert>

          <div class="grid gap-4 grid-cols-[repeat(auto-fit,minmax(14rem,1fr))]">
            <div class="space-y-1.5">
              <Label for="p-key">唯一标识</Label>
              <Input id="p-key" v-model="form.key" placeholder="如 deepseek" :disabled="!!form.createdAt" />
            </div>
            <div class="space-y-1.5">
              <Label for="p-version">协议版本头</Label>
              <Input id="p-version" v-model="form.version" placeholder="2023-06-01" />
            </div>
          </div>

          <!-- 端点：行序即故障转移顺序 -->
          <div class="mt-6 border-t pt-4">
            <div class="card-title">端点</div>
            <div class="mt-3 grid grid-cols-[8.5rem_minmax(0,1fr)_minmax(0,1fr)_2rem] items-center gap-2 text-xs text-muted-foreground">
              <span>协议</span>
              <span>Base URL</span>
              <span>API Key</span>
              <span />
            </div>
            <div
              v-for="(ep, i) in form.endpoints"
              :key="i"
              class="mt-2 grid grid-cols-[8.5rem_minmax(0,1fr)_minmax(0,1fr)_2rem] items-center gap-2"
            >
              <Select v-model="ep.protocol" :options="protocolOptions" />
              <Input v-model="ep.baseUrl" placeholder="https://api.anthropic.com" />
              <Input v-model="ep.apiKey" type="password" placeholder="sk-..." />
              <Button variant="ghost" size="icon" class="size-8" @click="removeEndpoint(i)">
                <Trash2 class="size-3.5 text-destructive" />
              </Button>
            </div>
            <!-- 尾行：添加端点（与列表页新建入口同一惯例） -->
            <button
              type="button"
              class="mt-2 flex w-full items-center gap-1.5 rounded-sm py-1.5 text-xs text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
              @click="addEndpoint"
            >
              <Plus class="size-3.5" /> 添加端点
            </button>

            <!-- 模型绑定：绑定对象是协议组而非端点行——每个协议一条展开器 -->
            <div class="mt-4 border-t border-dashed pt-3">
              <div class="text-xs font-medium">模型绑定</div>
              <div class="mt-2 space-y-1">
                <div v-for="proto in endpointProtocolList" :key="proto">
                  <button
                    type="button"
                    class="flex w-full items-center gap-1.5 py-1 text-xs transition-colors hover:text-foreground"
                    @click="toggleProtocolPanel(proto)"
                  >
                    <ChevronDown v-if="expandedProtocol === proto" class="size-3.5 shrink-0 text-muted-foreground" />
                    <ChevronRight v-else class="size-3.5 shrink-0 text-muted-foreground" />
                    <span class="font-mono">{{ proto }}</span>
                    <span class="text-muted-foreground">
                      {{ boundCount(proto) ? `已绑定 ${boundCount(proto)} 个模型` : "未绑定" }}
                    </span>
                  </button>
                  <div
                    v-if="expandedProtocol === proto"
                    data-ep-model-panel
                    class="mb-1 ml-5 space-y-3 pb-1"
                  >
                    <!-- 已选模型：chip 上的 × 复用勾选开关（取消该模型的协议绑定） -->
                    <div class="space-y-1.5">
                      <div v-if="checkedModelSlugs.length === 0" class="text-xs text-muted-foreground">
                        未选择模型
                      </div>
                      <div v-else class="flex flex-wrap gap-1.5">
                        <span
                          v-for="slug in checkedModelSlugs"
                          :key="slug"
                          class="inline-flex items-center gap-1 rounded-md border bg-muted/40 py-0.5 pl-2 pr-1 font-mono text-xs"
                        >
                          {{ slug }}
                          <button
                            type="button"
                            class="text-muted-foreground transition-colors hover:text-destructive"
                            title="移除"
                            @click="toggleEndpointModel(proto, slug)"
                          >
                            <X class="size-3" />
                          </button>
                        </span>
                      </div>
                    </div>

                    <!-- 候选列表默认收起：聚焦/点击搜索框才展开 -->
                    <div v-if="allModels.length > 0" class="relative">
                      <Search class="pointer-events-none absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
                      <Input
                        v-model="epModelQuery"
                        placeholder="搜索模型…"
                        class="pl-8"
                        @focus="epDropdownOpen = true"
                        @click="epDropdownOpen = true"
                      />
                    </div>
                    <div
                      v-if="allModels.length === 0"
                      class="rounded-md border border-dashed p-4 text-center text-xs text-muted-foreground"
                    >
                      暂无模型定义，请先到「模型」页创建。
                    </div>
                    <div
                      v-else-if="epDropdownOpen"
                      class="scrollbar-thin max-h-48 space-y-1 overflow-y-auto rounded-md border bg-popover p-2 shadow-md"
                    >
                      <label
                        v-for="m in filteredEpModels.items"
                        :key="m.slug"
                        class="flex cursor-pointer items-center gap-2 py-0.5 text-sm"
                      >
                        <Checkbox
                          :checked="checkedModels.has(m.slug)"
                          @update:checked="toggleEndpointModel(proto, m.slug)"
                        />
                        <span class="font-mono text-xs">{{ m.slug }}</span>
                        <span v-if="m.displayName" class="text-xs text-muted-foreground">{{ m.displayName }}</span>
                      </label>
                      <div
                        v-if="filteredEpModels.total > EP_PAGE_SIZE"
                        class="border-t pt-2"
                      >
                        <Pagination
                          v-model:page="epModelPage"
                          :page-count="filteredEpModels.pageCount"
                          :total="filteredEpModels.total"
                        />
                      </div>
                    </div>
                  </div>
                </div>
              </div>
            </div>
          </div>

          <!-- 配额查询绑定：脚本在配额插件里，这里只选插件与间隔；配置按插件 config_schema -->
          <div class="mt-6 border-t pt-4">
            <div class="card-title">配额查询</div>
            <div class="mt-3 grid gap-4 grid-cols-[repeat(auto-fit,minmax(14rem,1fr))]">
              <div class="space-y-1.5">
                <Label for="p-quota-plugin">配额插件</Label>
                <Select
                  id="p-quota-plugin"
                  v-model="form.quotaPluginRef"
                  :options="quotaPluginOptions"
                  placeholder="不绑定"
                />
              </div>
              <div class="space-y-1.5">
                <Label for="p-quota-interval">查询间隔（秒）</Label>
                <Input id="p-quota-interval" v-model="form.quotaIntervalSecs" type="number" min="0" />
              </div>
              <div class="flex items-center gap-2 pt-6">
                <Checkbox id="p-quota-enabled" v-model:checked="form.quotaEnabled" />
                <Label for="p-quota-enabled">启用定时查询</Label>
              </div>
            </div>
            <template v-if="form.quotaPluginRef">
              <!-- 实例配置：由插件 MB.config_schema 驱动；无 schema = 无需配置 -->
              <div v-if="quotaSchemaFields.length" class="mt-3 grid gap-4 grid-cols-[repeat(auto-fit,minmax(14rem,1fr))]">
                <div v-for="f in quotaSchemaFields" :key="f.name" class="space-y-1.5">
                  <Label :for="'p-quota-cfg-' + f.name">{{ f.label }}</Label>
                  <Input
                    :id="'p-quota-cfg-' + f.name"
                    v-model="(form.quotaConfig as Record<string, string | number>)[f.name]"
                    :type="f.secret ? 'password' : f.type === 'number' ? 'number' : 'text'"
                    :placeholder="f.default !== undefined ? String(f.default) : ''"
                    autocomplete="off"
                  />
                  <p v-if="f.help" class="text-xs text-muted-foreground">{{ f.help }}</p>
                </div>
              </div>
              <p v-else class="mt-3 text-xs text-muted-foreground">该插件未声明实例配置项。</p>
              <p v-if="quotaSchemaFields.length" class="mt-1.5 text-xs text-muted-foreground">
                实例配置整体加密写入数据库。
              </p>

              <!-- 测试查询：dry-run 当前表单配置，不写入数据库 -->
              <div class="mt-3 flex items-center gap-2">
                <Button
                  variant="outline"
                  size="sm"
                  :disabled="quotaTesting"
                  title="试跑当前表单配置，不写入数据库"
                  @click="runQuotaTest"
                >
                  <RefreshCw class="size-3.5" :class="quotaTesting ? 'animate-spin' : ''" />
                  {{ quotaTesting ? "查询中…" : "测试查询" }}
                </Button>
              </div>
              <div v-if="quotaTestResult" class="mt-2 space-y-1.5">
                <div v-for="r in quotaTestResult" :key="r.keyIndex" class="flex items-center gap-2 text-xs">
                  <Badge v-if="r.keyLabel" variant="secondary" class="max-w-[10rem] truncate font-mono">
                    {{ r.keyLabel }}
                  </Badge>
                  <Badge :variant="r.status === 'ok' ? 'success' : 'destructive'">
                    {{ r.status === "ok" ? "成功" : "失败" }}
                  </Badge>
                  <span v-if="r.status === 'error'" class="text-destructive">{{ r.error || r.payload?.message }}</span>
                  <span v-else-if="r.payload?.summary" class="text-muted-foreground">{{ r.payload.summary }}</span>
                  <span v-else-if="r.payload?.quotas?.length" class="text-muted-foreground">
                    {{ r.payload.quotas.map((q) => q.label).join("、") }}
                  </span>
                </div>
              </div>
            </template>
          </div>

          <!-- 插件三态开关（仅请求链路插件；配额插件在上方单独绑定） -->
          <div v-if="hookPlugins.length > 0" class="mt-6 border-t pt-4">
            <div class="card-title">插件</div>
            <div class="mt-3 space-y-1.5">
              <div
                v-for="p in hookPlugins"
                :key="p.name"
                class="flex items-center justify-between gap-4"
              >
                <span class="min-w-0 truncate font-mono text-xs text-muted-foreground">
                  {{ p.name }}
                  <span v-if="!p.enabled" class="text-amber-600 dark:text-amber-400">已全局停用</span>
                </span>
                <div class="w-32 shrink-0">
                  <Select v-model="pluginStates[p.name]" :options="TRI_OPTIONS" small />
                </div>
              </div>
            </div>
          </div>
        </div>
      </div>
    </div>

    <!-- 上游服务：满版面板（直接铺进 main，不套卡片外壳） -->
    <div v-else key="list" class="flex min-h-0 flex-1 flex-col overflow-hidden">
      <div class="scrollbar-thin min-h-0 flex-1 overflow-y-auto px-5 py-4">
        <EmptyState v-if="store.loading && store.providers.length === 0">加载中…</EmptyState>
        <EmptyState v-else-if="store.providers.length === 0" :icon="Server">
          暂无上游服务。
          <template #action>
            <Button size="sm" @click="newProvider"><Plus class="size-4" /> 新建上游服务</Button>
          </template>
        </EmptyState>
        <table v-else class="w-full text-sm">
          <thead class="thead-sticky">
            <tr class="border-b text-left text-muted-foreground">
              <th class="py-2 font-medium">Key</th>
              <th class="py-2 font-medium">协议</th>
              <th class="py-2 font-medium">端点</th>
              <th class="py-2 text-center font-medium">状态</th>
              <th class="w-20 py-2 text-center font-medium">操作</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="p in store.providers" :key="p.key" class="border-b transition-colors last:border-0 hover:bg-accent/40">
              <td class="py-2 font-mono text-xs">{{ p.key }}</td>
              <td class="py-2 font-mono text-xs text-muted-foreground">{{ endpointProtocols(p) }}</td>
              <td class="max-w-[280px] truncate py-2 text-muted-foreground" :title="p.endpoints[0]?.baseUrl">
                {{ p.endpoints[0]?.baseUrl }}
                <span v-if="p.endpoints.length > 1" class="ml-1 font-mono text-xs">×{{ p.endpoints.length }}</span>
              </td>
              <td class="py-2 text-center">
                <Badge :variant="p.enabled ? 'success' : 'secondary'">
                  {{ p.enabled ? "启用" : "停用" }}
                </Badge>
              </td>
              <td class="py-2">
                <div class="flex justify-center gap-0.5">
                  <Button variant="ghost" size="icon" class="size-7" title="编辑" @click="editProvider(p)">
                    <Pencil class="size-3.5" />
                  </Button>
                  <Button variant="ghost" size="icon" class="size-7" title="删除" @click="remove(p.key)">
                    <Trash2 class="size-3.5 text-destructive" />
                  </Button>
                </div>
              </td>
            </tr>
            <!-- 尾行：新建入口（与路由页/模型页同一惯例） -->
            <tr class="last:border-0">
              <td colspan="5" class="py-1">
                <button
                  type="button"
                  class="flex w-full items-center gap-1.5 rounded-sm py-1.5 text-xs text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
                  @click="newProvider"
                >
                  <Plus class="size-3.5" /> 新建上游服务
                </button>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </div>
    </Transition>
  </div>
</template>
