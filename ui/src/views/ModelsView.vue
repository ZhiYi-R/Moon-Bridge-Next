<script setup lang="ts">
import { CloudDownload, Pencil, Plus, Search, Trash2 } from "lucide-vue-next";
import { computed, onActivated, onMounted, reactive, ref, watch, type Ref } from "vue";

import Button from "@/components/ui/Button.vue";
import Card from "@/components/ui/Card.vue";
import Input from "@/components/ui/Input.vue";
import Label from "@/components/ui/Label.vue";
import Modal from "@/components/ui/Modal.vue";
import Pagination from "@/components/ui/Pagination.vue";
import Select from "@/components/ui/Select.vue";
import StringListInput from "@/components/ui/StringListInput.vue";
import { useConfirm } from "@/composables/useConfirm";
import { useToast } from "@/composables/useToast";
import { useAutoPageSize } from "@/composables/useAutoPageSize";
import { usePointerDrag } from "@/composables/usePointerDrag";
import {
  catalogApi,
  errMsg,
  modelApi,
  providerApi,
  type CatalogModel,
  type ModelDef,
  type Offer,
  type Provider,
} from "@/lib/api";
import { formatCtx } from "@/lib/utils";

const error = ref<string | null>(null);
const { confirm } = useConfirm();
const toast = useToast();
/** 分页切片：按页号取子集，并保证页号不越界。 */
function paginate<T>(list: T[], page: Ref<number>, pageSize: number): T[] {
  return list.slice((page.value - 1) * pageSize, page.value * pageSize);
}

const textareaClass =
  "flex w-full rounded-md border border-input bg-transparent px-3 py-2 font-mono text-xs shadow-sm focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring";
/** 节头内嵌小号下拉。 */
const providerOptions = computed(() => providers.value.map((p) => ({ value: p.key, label: p.key })));
const modelOptions = computed(() => models.value.map((m) => ({ value: m.slug, label: m.slug })));

// ───────────────────────── 模型定义 CRUD ─────────────────────────
const models = ref<ModelDef[]>([]);
const modelsLoading = ref(false);
const defScroll = ref<HTMLElement | null>(null);
const defPage = ref(1);
const { pageSize: defPageSize } = useAutoPageSize(defScroll, defPage);
const defPageCount = computed(() => Math.max(1, Math.ceil(models.value.length / defPageSize.value)));
const pagedModels = computed(() => paginate(models.value, defPage, defPageSize.value));
watch(defPageCount, (c) => {
  if (defPage.value > c) defPage.value = c;
});
const editing = ref(false);
const isNew = ref(false);
const busy = ref(false);

interface ModelForm {
  slug: string;
  displayName: string;
  contextWindow: string;
  maxOutputTokens: string;
  modalities: string[];
  reasoningLevels: string[];
  extraText: string;
}

/** 模态预置选项：text 为所有 LLM 天然支持，不作为勾选项（保存时始终隐含）；未知值动态追加。 */
const KNOWN_MODALITIES = ["pdf", "image", "audio", "video"] as const;

function toggleArr(arr: string[], v: string) {
  const i = arr.indexOf(v);
  if (i >= 0) arr.splice(i, 1);
  else arr.push(v);
}

/** 勾选项：预置项在前，已有数据中的未知值动态追加。 */
const modalityOptions = computed(() => [
  ...KNOWN_MODALITIES,
  ...form.modalities.filter((v) => !(KNOWN_MODALITIES as readonly string[]).includes(v)),
]);

const form = reactive<ModelForm>({
  slug: "",
  displayName: "",
  contextWindow: "",
  maxOutputTokens: "",
  modalities: [],
  reasoningLevels: [],
  extraText: "{}",
});

// ── 未保存关闭守卫：打开弹窗时拍快照，关闭时比对 ──
const formSnapshot = ref("");
const formDirty = computed(() => JSON.stringify(form) !== formSnapshot.value);

async function closeGuard(): Promise<boolean> {
  if (!formDirty.value) return true;
  return confirm({
    title: "关闭编辑",
    message: "有未保存的修改，确认丢弃并关闭？",
    confirmText: "丢弃修改",
  });
}

async function tryCloseEdit() {
  if (await closeGuard()) editing.value = false;
}

async function loadModels() {
  modelsLoading.value = true;
  try {
    models.value = await modelApi.list();
  } catch (e) {
    error.value = errMsg(e);
  } finally {
    modelsLoading.value = false;
  }
}

function newModel() {
  isNew.value = true;
  Object.assign(form, {
    slug: "",
    displayName: "",
    contextWindow: "",
    maxOutputTokens: "",
    modalities: [],
    reasoningLevels: [],
    extraText: "{}",
  });
  error.value = null;
  editing.value = true;
  formSnapshot.value = JSON.stringify(form);
}

function editModel(m: ModelDef) {
  isNew.value = false;
  Object.assign(form, {
    slug: m.slug,
    displayName: m.displayName ?? "",
    contextWindow: m.contextWindow != null ? String(m.contextWindow) : "",
    maxOutputTokens: m.maxOutputTokens != null ? String(m.maxOutputTokens) : "",
    modalities: Array.isArray(m.modalities)
      ? (m.modalities as string[]).filter((v) => v !== "text")
      : [],
    reasoningLevels: Array.isArray(m.reasoningLevels) ? (m.reasoningLevels as string[]) : [],
    extraText: m.extra == null ? "{}" : JSON.stringify(m.extra, null, 2),
  });
  error.value = null;
  editing.value = true;
  formSnapshot.value = JSON.stringify(form);
}

async function saveModel() {
  error.value = null;
  const slug = form.slug.trim();
  if (!slug) {
    error.value = "模型 slug 不能为空";
    return;
  }
  let extra: unknown;
  try {
    extra = form.extraText.trim() ? JSON.parse(form.extraText) : {};
  } catch {
    error.value = "扩展 extra 不是合法 JSON";
    return;
  }
  let contextWindow: number | null = null;
  if (form.contextWindow.trim()) {
    contextWindow = Number(form.contextWindow);
    if (Number.isNaN(contextWindow)) {
      error.value = "上下文窗口须为数字";
      return;
    }
  }
  let maxOutputTokens: number | null = null;
  if (form.maxOutputTokens.trim()) {
    maxOutputTokens = Number(form.maxOutputTokens);
    if (Number.isNaN(maxOutputTokens)) {
      error.value = "输出上限须为数字";
      return;
    }
  }
  const rec: ModelDef = {
    slug,
    displayName: form.displayName.trim() || null,
    contextWindow,
    maxOutputTokens,
    modalities: form.modalities.length ? ["text", ...form.modalities] : null,
    reasoningLevels: form.reasoningLevels.length ? form.reasoningLevels : null,
    extra,
  };
  busy.value = true;
  try {
    await modelApi.save(rec);
    editing.value = false;
    toast.success(`模型 “${slug}” 已保存`);
    models.value = models.value.some((m) => m.slug === slug)
      ? models.value.map((m) => (m.slug === slug ? rec : m))
      : [...models.value, rec];
  } catch (e) {
    error.value = errMsg(e);
  } finally {
    busy.value = false;
  }
}

async function removeModel(slug: string) {
  if (!(await confirm({ title: "删除模型", message: `确认删除模型 “${slug}”？相关 offer 不会自动删除。` }))) return;
  try {
    await modelApi.remove(slug);
    toast.success(`模型 “${slug}” 已删除`);
    models.value = models.value.filter((m) => m.slug !== slug);
  } catch (e) {
    error.value = errMsg(e);
  }
}

// ───────────────────────── 从 models.dev 导入 ─────────────────────────
const importModal = ref(false);
const importLoading = ref(false);
const importBusy = ref(false);
const importError = ref<string | null>(null);
const catalog = ref<CatalogModel[]>([]);
const catalogLoaded = ref(false);
const importQuery = ref("");
/** 勾选集合，键为 `providerKey::id`（models.dev 允许不同 provider 有同名模型）。 */
const importSelected = ref<Set<string>>(new Set());

const catalogKey = (m: CatalogModel) => `${m.providerKey}::${m.id}`;
/** 已存在于本地的 slug 集合，用于在列表里标注「已导入」。 */
const existingSlugs = computed(() => new Set(models.value.map((m) => m.slug)));

/** 搜索过滤：匹配模型 id、显示名、provider 名（大小写不敏感）。 */
const filteredCatalog = computed(() => {
  const q = importQuery.value.trim().toLowerCase();
  if (!q) return catalog.value;
  return catalog.value.filter(
    (m) =>
      m.id.toLowerCase().includes(q) ||
      (m.name ?? "").toLowerCase().includes(q) ||
      m.providerName.toLowerCase().includes(q),
  );
});

// 导入目录同样分页：models.dev 全量上万条，一次渲染会卡顿；页大小按弹窗内可视高度自动计算
const importScroll = ref<HTMLElement | null>(null);
const importPage = ref(1);
const { pageSize: importPageSize } = useAutoPageSize(importScroll, importPage);
const importPageCount = computed(() => Math.max(1, Math.ceil(filteredCatalog.value.length / importPageSize.value)));
const pagedCatalog = computed(() => paginate(filteredCatalog.value, importPage, importPageSize.value));
watch(importQuery, () => {
  importPage.value = 1;
});

async function openImport() {
  importError.value = null;
  importQuery.value = "";
  importSelected.value = new Set();
  importModal.value = true;
  // 首次打开才拉取；后续复用（models.dev 数据大，避免重复拉取）
  if (catalogLoaded.value) return;
  importLoading.value = true;
  try {
    catalog.value = await catalogApi.fetch();
    catalogLoaded.value = true;
  } catch (e) {
    importError.value = errMsg(e);
  } finally {
    importLoading.value = false;
  }
}

async function refreshCatalog() {
  importError.value = null;
  importLoading.value = true;
  try {
    catalog.value = await catalogApi.fetch();
    catalogLoaded.value = true;
  } catch (e) {
    importError.value = errMsg(e);
  } finally {
    importLoading.value = false;
  }
}

function toggleSelect(m: CatalogModel) {
  const key = catalogKey(m);
  const next = new Set(importSelected.value);
  if (next.has(key)) next.delete(key);
  else next.add(key);
  importSelected.value = next;
}

/** 全选/清空当前过滤结果。 */
function toggleSelectAllFiltered() {
  const keys = filteredCatalog.value.map(catalogKey);
  const allSelected = keys.length > 0 && keys.every((k) => importSelected.value.has(k));
  const next = new Set(importSelected.value);
  if (allSelected) keys.forEach((k) => next.delete(k));
  else keys.forEach((k) => next.add(k));
  importSelected.value = next;
}

const allFilteredSelected = computed(() => {
  const keys = filteredCatalog.value.map(catalogKey);
  return keys.length > 0 && keys.every((k) => importSelected.value.has(k));
});

async function confirmImport() {
  const chosen = catalog.value.filter((m) => importSelected.value.has(catalogKey(m)));
  if (chosen.length === 0) {
    importError.value = "请至少勾选一个模型";
    return;
  }
  importError.value = null;
  importBusy.value = true;
  try {
    const n = await catalogApi.import(chosen);
    importModal.value = false;
    toast.success(`已导入 ${n} 个模型`);
    // 导入同时更新模型定义与各 provider 报价，服务端改动面超出行级：回退整表重拉
    await loadModels();
  } catch (e) {
    importError.value = errMsg(e);
  } finally {
    importBusy.value = false;
  }
}

/** 定价紧凑展示：input/output（USD/1M）。 */
function catalogPrice(m: CatalogModel): string {
  const i = m.pricing.input;
  const o = m.pricing.output;
  if (i == null && o == null) return "—";
  return `$${i ?? "?"} / $${o ?? "?"}`;
}

// ───────────────────────── Offer 管理（provider 维度）─────────────────────────
const providers = ref<Provider[]>([]);
const selectedProvider = ref("");
const offers = ref<Offer[]>([]);
const offersLoading = ref(false);
const offerScroll = ref<HTMLElement | null>(null);
const offerPage = ref(1);
const { pageSize: offerPageSize } = useAutoPageSize(offerScroll, offerPage);
const offerPageCount = computed(() => Math.max(1, Math.ceil(offers.value.length / offerPageSize.value)));
const pagedOffers = computed(() => paginate(offers.value, offerPage, offerPageSize.value));
watch(offerPageCount, (c) => {
  if (offerPage.value > c) offerPage.value = c;
});
const offerBusy = ref(false);
/** 定价弹窗：五类 token 独立定价（单位 USD / 1M tokens），留空表示未单独定价。 */
const offerModal = ref(false);
const offerError = ref<string | null>(null);
const offerForm = reactive({
  modelSlug: "",
  endpointProtocol: "",
  inputPrice: "",
  outputPrice: "",
  cacheReadPrice: "",
  cacheWritePrice: "",
  reasoningPrice: "",
});

/** 定价表单 5 个扁平键之外的 pricing 字段（tiers/context_over_200k 等长上下文
 *  分层价目）。表单不展示这些字段，但保存时必须原样带回，否则编辑一次报价
 *  就把目录导入的分层数据抹掉。 */
const PRICING_FORM_KEYS = ["input", "output", "cache_read", "cache_write", "reasoning"];
const pricingExtras = ref<Record<string, unknown>>({});

/** 当前所选 provider 的端点协议去重列表（供 offer 绑定端点下拉）。 */
const endpointProtocolOptions = computed(() => {
  const p = providers.value.find((x) => x.key === selectedProvider.value);
  const protos = Array.from(new Set((p?.endpoints ?? []).map((e) => e.protocol)));
  return [
    { value: "", label: "全部端点（按序故障转移）" },
    ...protos.map((pr) => ({ value: pr, label: pr })),
  ];
});

function openAddOffer() {
  offerError.value = null;
  pricingExtras.value = {};
  Object.assign(offerForm, {
    endpointProtocol: "",
    inputPrice: "",
    outputPrice: "",
    cacheReadPrice: "",
    cacheWritePrice: "",
    reasoningPrice: "",
  });
  offerModal.value = true;
}

function editOffer(o: Offer) {
  offerError.value = null;
  const p = (o.pricing ?? {}) as Record<string, unknown>;
  pricingExtras.value = Object.fromEntries(
    Object.entries(p).filter(([k]) => !PRICING_FORM_KEYS.includes(k)),
  );
  const num = (k: string) => (typeof p[k] === "number" ? String(p[k]) : "");
  offerForm.modelSlug = o.modelSlug;
  offerForm.endpointProtocol = o.endpointProtocol ?? "";
  Object.assign(offerForm, {
    inputPrice: num("input"),
    outputPrice: num("output"),
    cacheReadPrice: num("cache_read"),
    cacheWritePrice: num("cache_write"),
    reasoningPrice: num("reasoning"),
  });
  offerModal.value = true;
}

async function confirmOffer() {
  offerError.value = null;
  if (!selectedProvider.value || !offerForm.modelSlug) {
    offerError.value = "请选择 provider 与模型 slug";
    return;
  }
  const fields: Array<[string, string]> = [
    ["input", offerForm.inputPrice],
    ["output", offerForm.outputPrice],
    ["cache_read", offerForm.cacheReadPrice],
    ["cache_write", offerForm.cacheWritePrice],
    ["reasoning", offerForm.reasoningPrice],
  ];
  let pricing: Record<string, unknown> | null = null;
  for (const [key, raw] of fields) {
    const t = raw.trim();
    if (t === "") continue;
    const n = Number(t);
    if (Number.isNaN(n)) {
      offerError.value = "定价须为数字";
      return;
    }
    (pricing ??= {})[key] = n;
  }
  if (Object.keys(pricingExtras.value).length > 0) {
    pricing = { ...pricingExtras.value, ...(pricing ?? {}) };
  }
  offerBusy.value = true;
  try {
    const offer: Offer = {
      providerKey: selectedProvider.value,
      modelSlug: offerForm.modelSlug,
      pricing,
      endpointProtocol: offerForm.endpointProtocol || null,
    };
    await modelApi.offerSave(offer);
    offerModal.value = false;
    toast.success(`报价 “${offerForm.modelSlug}” 已保存`);
    // 保存只影响这一行：按 slug 原地增改，避免整表重拉
    offers.value = offers.value.some((o) => o.modelSlug === offer.modelSlug)
      ? offers.value.map((o) => (o.modelSlug === offer.modelSlug ? offer : o))
      : [...offers.value, offer];
  } catch (e) {
    offerError.value = errMsg(e);
  } finally {
    offerBusy.value = false;
  }
}

async function loadProviders() {
  try {
    providers.value = await providerApi.list();
    if (!selectedProvider.value && providers.value.length > 0) {
      selectedProvider.value = providers.value[0].key;
      await loadOffers();
    }
  } catch (e) {
    error.value = errMsg(e);
  }
}

async function loadOffers() {
  if (!selectedProvider.value) {
    offers.value = [];
    return;
  }
  offersLoading.value = true;
  try {
    offers.value = await modelApi.offerList(selectedProvider.value);
  } catch (e) {
    error.value = errMsg(e);
  } finally {
    offersLoading.value = false;
  }
}

async function onProviderChange() {
  offerPage.value = 1;
  await loadOffers();
}

async function removeOffer(modelSlug: string) {
  if (!(await confirm({ title: "删除报价", message: `确认删除 ${selectedProvider.value} 对 “${modelSlug}” 的报价？` }))) return;
  try {
    await modelApi.offerRemove(selectedProvider.value, modelSlug);
    toast.success(`报价 “${modelSlug}” 已删除`);
    offers.value = offers.value.filter((o) => o.modelSlug !== modelSlug);
  } catch (e) {
    error.value = errMsg(e);
  }
}

/** 读取 offer 定价中的单项（无该定价显示 —）。 */
function price(p: unknown, key: string): string {
  const v = (p as Record<string, unknown> | null)?.[key];
  return typeof v === "number" ? String(v) : "—";
}

/** 长上下文分层价目提示：pricing 含 tiers/context_over_200k 时返回如 ">200K"。 */
function tierHint(p: unknown): string | null {
  const o = p as Record<string, unknown> | null;
  if (!o) return null;
  const tiers = Array.isArray(o.tiers) ? o.tiers : [];
  const sizes = tiers
    .map((t) => (t as Record<string, unknown>)?.tier as Record<string, unknown> | undefined)
    .filter((m) => m?.type === "context" && typeof m.size === "number")
    .map((m) => m?.size as number);
  if (o.context_over_200k != null) sizes.push(200_000);
  if (sizes.length === 0) return null;
  return `>${Math.min(...sizes) / 1000}K 加价`;
}

// ── 上下分区拖拽比例（持久化；高度变化经 useAutoPageSize 自动换算页号） ──
const SPLIT_KEY = "models-split-pct";
const splitEl = ref<HTMLElement | null>(null);
const topPct = ref(
  Math.min(0.75, Math.max(0.25, Number(localStorage.getItem(SPLIT_KEY)) || 0.55)),
);
let startPct = topPct.value;
const startSplit = usePointerDrag(
  (_dx, dy) => {
    const h = splitEl.value?.clientHeight ?? 0;
    if (h > 0) topPct.value = Math.min(0.75, Math.max(0.25, startPct + dy / h));
  },
  {
    onStart: () => {
      startPct = topPct.value;
    },
    onEnd: () => localStorage.setItem(SPLIT_KEY, topPct.value.toFixed(4)),
  },
);

// 首次挂载：模型定义与 provider 列表互相独立，一次并行拉取
onMounted(async () => {
  await Promise.all([loadModels(), loadProviders()]);
});

// keep-alive 下切回本页：第二次起并行静默重拉（已有数据不闪 loading）
let activated = false;
onActivated(() => {
  if (activated) void Promise.all([loadModels(), loadProviders(), loadOffers()]);
  activated = true;
});
</script>

<template>
  <div class="flex h-full min-h-0 flex-col">
    <div
      v-if="error"
      class="shrink-0 rounded-md border border-destructive/40 bg-destructive/10 px-4 py-2 text-sm text-destructive"
    >
      {{ error }}
    </div>

    <!-- 编辑弹窗 -->
    <Modal
      :open="editing"
      :title="isNew ? '新建模型' : '编辑模型'"
      :guard="closeGuard"
      @close="editing = false"
    >
      <div class="grid gap-4 grid-cols-[repeat(auto-fit,minmax(14rem,1fr))]">
        <div class="space-y-1.5">
          <Label for="m-slug">唯一标识</Label>
          <Input id="m-slug" v-model="form.slug" placeholder="如 claude-sonnet-4" :disabled="!isNew" />
        </div>
        <div class="space-y-1.5">
          <Label for="m-name">显示名</Label>
          <Input id="m-name" v-model="form.displayName" placeholder="Claude Sonnet 4" />
        </div>
        <div class="space-y-1.5">
          <Label for="m-ctx">上下文窗口</Label>
          <Input id="m-ctx" v-model="form.contextWindow" placeholder="200000" inputmode="numeric" />
        </div>
        <div class="space-y-1.5">
          <Label for="m-maxout">输出上限</Label>
          <Input id="m-maxout" v-model="form.maxOutputTokens" placeholder="64000" inputmode="numeric" />
          <p class="text-xs text-muted-foreground">Anthropic 类上游在客户端未设上限时以此兜底</p>
        </div>
        <div class="space-y-1.5">
          <Label>模态</Label>
          <div class="flex flex-wrap gap-x-4 gap-y-2 pt-1.5">
            <label
              v-for="m in modalityOptions"
              :key="m"
              class="flex cursor-pointer items-center gap-1.5 text-sm"
            >
              <input
                type="checkbox"
                class="size-4 accent-primary"
                :checked="form.modalities.includes(m)"
                @change="toggleArr(form.modalities, m)"
              />
              <span class="font-mono text-xs">{{ m }}</span>
            </label>
          </div>
        </div>
        <div class="space-y-1.5">
          <Label>推理档位</Label>
          <StringListInput v-model="form.reasoningLevels" placeholder="如 high 后回车添加" />
        </div>
        <div class="space-y-1.5 col-span-full">
          <Label for="m-extra">扩展字段</Label>
          <textarea id="m-extra" v-model="form.extraText" rows="3" spellcheck="false" :class="textareaClass"></textarea>
        </div>
      </div>
      <template #footer>
        <Button variant="ghost" size="sm" @click="tryCloseEdit">取消</Button>
        <Button size="sm" :disabled="busy" @click="saveModel">{{ busy ? "保存中…" : "保存" }}</Button>
      </template>
    </Modal>

    <!-- 从 models.dev 导入弹窗 -->
    <Modal
      :open="importModal"
      title="从 models.dev 导入模型"
      width="max-w-3xl"
      @close="importModal = false"
    >
      <div v-if="importError" class="mb-3 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive">
        {{ importError }}
      </div>

      <!-- 搜索 + 操作条 -->
      <div class="mb-3 flex items-center gap-2">
        <div class="relative flex-1">
          <Search class="pointer-events-none absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
          <Input v-model="importQuery" placeholder="搜索模型 / provider…" class="pl-8" :disabled="importLoading" />
        </div>
        <Button variant="outline" size="sm" :disabled="importLoading" @click="refreshCatalog">
          {{ importLoading ? "拉取中…" : "刷新" }}
        </Button>
      </div>

      <div v-if="importLoading" class="rounded-md border border-dashed p-8 text-center text-sm text-muted-foreground">
        正在从 models.dev 拉取模型库…
      </div>
      <div
        v-else-if="catalog.length === 0"
        class="rounded-md border border-dashed p-8 text-center text-sm text-muted-foreground"
      >
        未获取到模型。点「刷新」重试，或检查网络。
      </div>
      <template v-else>
        <div class="mb-2 flex items-center justify-between text-xs text-muted-foreground">
          <label class="flex cursor-pointer items-center gap-1.5">
            <input
              type="checkbox"
              class="size-4 accent-primary"
              :checked="allFilteredSelected"
              @change="toggleSelectAllFiltered"
            />
            <span>全选当前结果（{{ filteredCatalog.length }}）</span>
          </label>
          <span>已选 {{ importSelected.size }}</span>
        </div>
        <div ref="importScroll" class="scrollbar-thin max-h-[52vh] overflow-y-auto rounded-md border">
          <table class="w-full text-sm">
            <thead class="thead-sticky">
              <tr class="border-b text-left text-muted-foreground">
                <th class="w-8 py-2"></th>
                <th class="py-2 font-medium">模型</th>
                <th class="py-2 font-medium">Provider</th>
                <th class="py-2 text-right font-medium">上下文</th>
                <th class="py-2 text-right font-medium">输出上限</th>
                <th class="py-2 text-right font-medium">输入/输出</th>
                <th class="py-2 text-center font-medium">状态</th>
              </tr>
            </thead>
            <tbody>
              <tr
                v-for="m in pagedCatalog"
                :key="catalogKey(m)"
                class="cursor-pointer border-b last:border-0 hover:bg-accent/40"
                @click="toggleSelect(m)"
              >
                <td class="py-1.5 text-center">
                  <input
                    type="checkbox"
                    class="size-4 accent-primary"
                    :checked="importSelected.has(catalogKey(m))"
                    @click.stop="toggleSelect(m)"
                  />
                </td>
                <td class="py-1.5">
                  <div class="font-mono text-xs">{{ m.id }}</div>
                  <div v-if="m.name" class="text-xs text-muted-foreground">{{ m.name }}</div>
                </td>
                <td class="py-1.5 text-xs text-muted-foreground">{{ m.providerName }}</td>
                <td class="py-1.5 text-right tabular-nums text-muted-foreground">{{ formatCtx(m.contextWindow) }}</td>
                <td class="py-1.5 text-right tabular-nums text-muted-foreground">{{ formatCtx(m.maxOutputTokens) }}</td>
                <td class="py-1.5 text-right tabular-nums text-muted-foreground">{{ catalogPrice(m) }}</td>
                <td class="py-1.5 text-center">
                  <span v-if="existingSlugs.has(m.id)" class="text-xs text-muted-foreground">已存在</span>
                  <span v-else class="text-xs text-emerald-600">新增</span>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
        <div v-if="filteredCatalog.length > importPageSize" class="mt-2">
          <Pagination v-model:page="importPage" :page-count="importPageCount" :total="filteredCatalog.length" />
        </div>
        <p class="mt-2 text-xs text-muted-foreground">
          导入将写入模型定义；定价写入对应上游服务的报价（已有报价保留现价）。同名模型会被覆盖更新。
        </p>
      </template>

      <template #footer>
        <Button variant="ghost" size="sm" @click="importModal = false">取消</Button>
        <Button size="sm" :disabled="importBusy || importSelected.size === 0" @click="confirmImport">
          {{ importBusy ? "导入中…" : `导入所选（${importSelected.size}）` }}
        </Button>
      </template>
    </Modal>

    <!-- 模型定义 + 模型报价：单卡双节，满版填满视口；分隔条可拖拽调比例 -->
    <Card class="flex min-h-0 flex-1 flex-col overflow-hidden">
      <div ref="splitEl" class="flex min-h-0 flex-1 flex-col">
      <!-- 节：模型定义（拖拽分栏，内部滚动） -->
      <section class="flex min-h-0 flex-col" :style="{ flex: `0 0 ${topPct * 100}%` }">
        <div class="flex items-center justify-between border-b px-5 py-3">
          <h3 class="card-title">模型定义</h3>
          <div class="flex items-center gap-2">
            <Button variant="outline" size="sm" @click="openImport">
              <CloudDownload class="size-4" /> 从 models.dev 导入
            </Button>
            <Button size="sm" @click="newModel">
              <Plus class="size-4" /> 新建模型
            </Button>
          </div>
        </div>
        <div ref="defScroll" class="scrollbar-thin min-h-0 flex-1 overflow-y-auto px-5 py-4">
          <div
            v-if="modelsLoading && models.length === 0"
            class="rounded-md border border-dashed p-8 text-center text-sm text-muted-foreground"
          >
            加载中…
          </div>
          <div
            v-else-if="models.length === 0"
            class="rounded-md border border-dashed p-8 text-center text-sm text-muted-foreground"
          >
            暂无模型定义，点击「新建模型」添加。
          </div>
          <table v-else class="w-full text-sm">
            <thead class="thead-sticky">
              <tr class="border-b text-left text-muted-foreground">
                <th class="py-2 font-medium">标识</th>
                <th class="py-2 font-medium">显示名</th>
                <th class="py-2 text-right font-medium">上下文窗口</th>
                <th class="py-2 text-right font-medium">输出上限</th>
                <th class="w-20 py-2 text-center font-medium">操作</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="m in pagedModels" :key="m.slug" class="border-b last:border-0">
                <td class="py-2 font-mono text-xs">{{ m.slug }}</td>
                <td class="py-2">{{ m.displayName ?? "—" }}</td>
                <td class="py-2 text-right tabular-nums text-muted-foreground">
                  {{ formatCtx(m.contextWindow) }}
                </td>
                <td class="py-2 text-right tabular-nums text-muted-foreground">
                  {{ formatCtx(m.maxOutputTokens) }}
                </td>
                <td class="py-2">
                  <div class="flex justify-center gap-0.5">
                    <Button variant="ghost" size="icon" class="size-7" title="编辑" @click="editModel(m)">
                      <Pencil class="size-3.5" />
                    </Button>
                    <Button variant="ghost" size="icon" class="size-7" title="删除" @click="removeModel(m.slug)">
                      <Trash2 class="size-3.5 text-destructive" />
                    </Button>
                  </div>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
        <div v-if="models.length > defPageSize" class="shrink-0 border-t px-5 py-2">
          <Pagination v-model:page="defPage" :page-count="defPageCount" :total="models.length" />
        </div>
      </section>

      <!-- 拖拽分栏把手 -->
      <div
        class="h-1 shrink-0 cursor-row-resize bg-border transition-colors hover:bg-primary/50 active:bg-primary/60"
        title="拖拽调整分区高度"
        @pointerdown="startSplit"
      />

      <!-- 节：模型报价（剩余高度，内部滚动） -->
      <section class="flex min-h-0 flex-1 flex-col">
        <div class="flex shrink-0 flex-wrap items-center justify-between gap-2 border-b px-5 py-3">
          <h3 class="card-title">模型报价</h3>
          <div class="flex items-center gap-2 text-xs text-muted-foreground">
            <span>上游服务</span>
            <Select v-model="selectedProvider" :options="providerOptions" small @update:model-value="onProviderChange" />
          </div>
        </div>
        <div ref="offerScroll" class="scrollbar-thin min-h-0 flex-1 space-y-4 overflow-y-auto px-5 py-4">
          <div
            v-if="!selectedProvider"
            class="rounded-md border border-dashed p-6 text-center text-sm text-muted-foreground"
          >
            请先在「上游服务」页创建 provider。
          </div>
          <template v-else>
            <!-- 新增 offer：选模型 → 弹窗内填五类定价 -->
            <div class="flex items-end gap-3">
              <div class="flex-1 space-y-1.5">
                <Label>模型</Label>
                <Select v-model="offerForm.modelSlug" :options="modelOptions" placeholder="选择模型…" searchable />
              </div>
              <Button size="sm" class="h-9" :disabled="!offerForm.modelSlug" @click="openAddOffer">
                <Plus class="size-4" /> 添加报价
              </Button>
            </div>

            <!-- 现有 offer -->
            <div
              v-if="offersLoading && offers.length === 0"
              class="rounded-md border border-dashed p-6 text-center text-sm text-muted-foreground"
            >
              加载中…
            </div>
            <div
              v-else-if="offers.length === 0"
              class="rounded-md border border-dashed p-6 text-center text-sm text-muted-foreground"
            >
              该 provider 暂无报价。
            </div>
            <table v-else class="w-full text-sm">
              <thead class="thead-sticky">
                <tr class="border-b text-left text-muted-foreground">
                  <th class="py-2 font-medium">模型</th>
                  <th class="py-2 font-medium">绑定端点</th>
                  <th class="py-2 text-right font-medium">输入</th>
                  <th class="py-2 text-right font-medium">输出</th>
                  <th class="py-2 text-right font-medium">缓存读</th>
                  <th class="py-2 text-right font-medium">缓存写</th>
                  <th class="py-2 text-right font-medium">推理</th>
                  <th class="w-20 py-2 text-center font-medium">操作</th>
                </tr>
              </thead>
              <tbody>
                <tr v-for="o in pagedOffers" :key="o.modelSlug" class="border-b last:border-0">
                  <td class="py-2 font-mono text-xs">
                    {{ o.modelSlug }}
                    <span
                      v-if="tierHint(o.pricing)"
                      class="ml-1 rounded bg-muted px-1 py-px font-sans text-[10px] text-muted-foreground"
                      :title="'长上下文分层计价：输入超过阈值后按档位价计费'"
                      >{{ tierHint(o.pricing) }}</span
                    >
                  </td>
                  <td class="py-2 font-mono text-xs text-muted-foreground">{{ o.endpointProtocol || "全部" }}</td>
                  <td class="py-2 text-right tabular-nums">{{ price(o.pricing, "input") }}</td>
                  <td class="py-2 text-right tabular-nums">{{ price(o.pricing, "output") }}</td>
                  <td class="py-2 text-right tabular-nums">{{ price(o.pricing, "cache_read") }}</td>
                  <td class="py-2 text-right tabular-nums">{{ price(o.pricing, "cache_write") }}</td>
                  <td class="py-2 text-right tabular-nums">{{ price(o.pricing, "reasoning") }}</td>
                  <td class="py-2">
                    <div class="flex justify-center gap-0.5">
                      <Button variant="ghost" size="icon" class="size-7" title="编辑定价" @click="editOffer(o)">
                        <Pencil class="size-3.5" />
                      </Button>
                      <Button variant="ghost" size="icon" class="size-7" @click="removeOffer(o.modelSlug)">
                        <Trash2 class="size-3.5 text-destructive" />
                      </Button>
                    </div>
                  </td>
                </tr>
              </tbody>
            </table>
          </template>
        </div>
        <div v-if="offers.length > offerPageSize" class="shrink-0 border-t px-5 py-2">
          <Pagination v-model:page="offerPage" :page-count="offerPageCount" :total="offers.length" />
        </div>
      </section>
      </div>
    </Card>

    <!-- 报价定价弹窗：五类 token 独立定价 -->
    <Modal
      :open="offerModal"
      :title="'报价定价'"
      width="max-w-lg"
      @close="offerModal = false"
    >
      <div v-if="offerError" class="mb-4 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive">
        {{ offerError }}
      </div>
      <div class="mb-4 space-y-1.5">
        <Label>绑定端点</Label>
        <Select v-model="offerForm.endpointProtocol" :options="endpointProtocolOptions" />
        <p class="text-xs text-muted-foreground">
          选择该模型走 provider 的哪个协议端点。选「全部端点」则按端点顺序故障转移（旧行为）。
        </p>
      </div>
      <p class="mb-4 text-xs text-muted-foreground">单位：USD / 1M tokens；留空表示该项不单独定价。</p>
      <div class="grid gap-4 grid-cols-[repeat(auto-fit,minmax(10rem,1fr))]">
        <div class="space-y-1.5">
          <Label for="p-input">输入</Label>
          <Input id="p-input" v-model="offerForm.inputPrice" placeholder="3" inputmode="decimal" />
        </div>
        <div class="space-y-1.5">
          <Label for="p-output">输出</Label>
          <Input id="p-output" v-model="offerForm.outputPrice" placeholder="15" inputmode="decimal" />
        </div>
        <div class="space-y-1.5">
          <Label for="p-cr">缓存读</Label>
          <Input id="p-cr" v-model="offerForm.cacheReadPrice" placeholder="0.3" inputmode="decimal" />
        </div>
        <div class="space-y-1.5">
          <Label for="p-cw">缓存写</Label>
          <Input id="p-cw" v-model="offerForm.cacheWritePrice" placeholder="3.75" inputmode="decimal" />
        </div>
        <div class="space-y-1.5">
          <Label for="p-r">推理</Label>
          <Input id="p-r" v-model="offerForm.reasoningPrice" placeholder="5" inputmode="decimal" />
        </div>
      </div>
      <template #footer>
        <Button variant="ghost" size="sm" @click="offerModal = false">取消</Button>
        <Button size="sm" :disabled="offerBusy" @click="confirmOffer">{{ offerBusy ? "保存中…" : "保存" }}</Button>
      </template>
    </Modal>
  </div>
</template>
