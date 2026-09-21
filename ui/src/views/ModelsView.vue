<script setup lang="ts">
import { Boxes, Check, CircleDollarSign, CloudDownload, Pencil, Plus, Search, Trash2, X } from "lucide-vue-next";
import { computed, nextTick, onActivated, onMounted, onUnmounted, reactive, ref, watch, type Ref } from "vue";

import Alert from "@/components/ui/Alert.vue";
import Button from "@/components/ui/Button.vue";
import Checkbox from "@/components/ui/Checkbox.vue";
import EmptyState from "@/components/ui/EmptyState.vue";
import Input from "@/components/ui/Input.vue";
import Label from "@/components/ui/Label.vue";
import Modal from "@/components/ui/Modal.vue";
import Pagination from "@/components/ui/Pagination.vue";
import Select from "@/components/ui/Select.vue";
import StringListInput from "@/components/ui/StringListInput.vue";
import { useConfirm } from "@/composables/useConfirm";
import { useToast } from "@/composables/useToast";
import { useAutoPageSize } from "@/composables/useAutoPageSize";
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
function paginate<T>(list: T[], page: Ref<number>, pageSize: number): T[] {
  return list.slice((page.value - 1) * pageSize, page.value * pageSize);
}

const textareaClass =
  "flex w-full rounded-md border border-input bg-transparent px-3 py-2 font-mono text-xs shadow-sm focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring";
const providerOptions = computed(() => providers.value.map((p) => ({ value: p.key, label: p.key })));
const modelOptions = computed(() => models.value.map((m) => ({ value: m.slug, label: m.slug })));

// ───────────────────────── 模型定义 CRUD ─────────────────────────
const models = ref<ModelDef[]>([]);
const modelsLoading = ref(false);
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

// ── 未保存关闭确认：打开弹窗时拍快照，关闭时比对 ──
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

const someFilteredSelected = computed(
  () =>
    !allFilteredSelected.value &&
    filteredCatalog.value.some((m) => importSelected.value.has(catalogKey(m))),
);

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
/** 当前 tab："" = 全部（管理视图），否则为供应商 key。 */
const activeProvider = ref("");
// provider 列表变化时保证供应商 tab 指向存在的供应商
watch(
  providers,
  (ps) => {
    if (activeProvider.value !== "" && !ps.some((p) => p.key === activeProvider.value)) {
      activeProvider.value = "";
    }
  },
  { immediate: true },
);

// ── 标签条共享下划线：translateX+width 滑向激活 tab（同侧栏指示条思路）──
const tabBarRef = ref<HTMLElement | null>(null);
const tabInk = reactive({ x: 0, w: 0, on: false });

function updateTabInk() {
  const bar = tabBarRef.value;
  const active = bar?.querySelector<HTMLElement>("[data-active]");
  if (!bar || !active) {
    tabInk.on = false;
    return;
  }
  tabInk.x = active.offsetLeft;
  tabInk.w = active.offsetWidth;
  tabInk.on = true;
}

let tabBarObserver: ResizeObserver | null = null;
watch([activeProvider, providers], () => nextTick(updateTabInk));

// ── 切 tab 内容横向滑动：标签横向排列 → 内容沿标签方向横滚（同路由纵向滚动逻辑）──
// 旧内容不透明滑出盖住新内容，方向 = 新旧 tab 的索引差；scrollEl 本身不换挂载
const paneDir = ref(1);
const paneTransitioning = ref(false);
watch(activeProvider, (to, from) => {
  const order = ["", ...providers.value.map((p) => p.key)];
  const d = Math.sign(order.indexOf(to) - order.indexOf(from));
  if (d !== 0) paneDir.value = d;
});
onMounted(() => {
  updateTabInk();
  tabBarObserver = new ResizeObserver(updateTabInk);
  if (tabBarRef.value) tabBarObserver.observe(tabBarRef.value);
});
onUnmounted(() => tabBarObserver?.disconnect());
const offers = ref<Offer[]>([]);
const offersLoading = ref(false);
const offerBusy = ref(false);
/** 定价弹窗：五类 token 独立定价（单位 USD / 1M tokens），留空表示未单独定价。 */
const offerModal = ref(false);
const offerError = ref<string | null>(null);
const offerForm = reactive({
  modelSlug: "",
  /** 报价归属的上游；新建时由弹窗内 Select 选定，编辑时锁定。 */
  providerKey: "",
  inputPrice: "",
  outputPrice: "",
  cacheReadPrice: "",
  cacheWritePrice: "",
  reasoningPrice: "",
});
/** 新建模式：弹窗内显示上游服务选择器。 */
const offerAdding = ref(false);

/** pricing 中由表单显式管理的键：5 个扁平价 + 分层价目（tiers 权威 / context_over_200k 旧式镜像）。
 *  其余未知键原样带回，避免编辑报价时抹掉目录导入的扩展字段。 */
const PRICING_FORM_KEYS = ["input", "output", "cache_read", "cache_write", "reasoning", "tiers", "context_over_200k"];
const pricingExtras = ref<Record<string, unknown>>({});

interface TierForm {
  /** 阈值（K tokens）：input_tokens 超过该值后本档价键逐项覆盖基价。 */
  sizeK: string;
  input: string;
  output: string;
  cacheRead: string;
  cacheWrite: string;
  reasoning: string;
}
const offerTiers = ref<TierForm[]>([]);

// ── 行内编辑：tierEditIndex = -1 无编辑，= offerTiers.length 为末尾新增虚拟行；
//    baseEditing 控制基础价行（offerForm 直绑，取消时快照回滚）──
const tierEditIndex = ref(-1);
const baseEditing = ref(false);
let baseSnapshot: typeof offerForm | null = null;
const priceError = ref<string | null>(null);
const tierForm = reactive<TierForm>({
  sizeK: "",
  input: "",
  output: "",
  cacheRead: "",
  cacheWrite: "",
  reasoning: "",
});

function openTierAdd() {
  priceError.value = null;
  Object.assign(tierForm, { sizeK: "", input: "", output: "", cacheRead: "", cacheWrite: "", reasoning: "" });
  tierEditIndex.value = offerTiers.value.length;
}

function openTierEdit(i: number) {
  priceError.value = null;
  Object.assign(tierForm, offerTiers.value[i]);
  tierEditIndex.value = i;
}

function openBaseEdit() {
  priceError.value = null;
  baseSnapshot = { ...offerForm };
  baseEditing.value = true;
}

function cancelBaseEdit() {
  if (baseSnapshot) Object.assign(offerForm, baseSnapshot);
  baseSnapshot = null;
  baseEditing.value = false;
}

function confirmBaseEdit() {
  for (const v of [
    offerForm.inputPrice,
    offerForm.outputPrice,
    offerForm.cacheReadPrice,
    offerForm.cacheWritePrice,
    offerForm.reasoningPrice,
  ]) {
    if (v.trim() && Number.isNaN(Number(v))) {
      priceError.value = "定价须为数字";
      return;
    }
  }
  baseEditing.value = false;
}

function onBaseRowKeydown(e: KeyboardEvent) {
  if (!baseEditing.value) return;
  if (e.key === "Escape") {
    e.stopPropagation();
    cancelBaseEdit();
  } else if (e.key === "Enter") {
    confirmBaseEdit();
  }
}

/** 编辑态行内快捷键：Enter 确认 / Esc 取消（阻止冒泡到 Modal 的 Esc 关弹窗）。 */
function onTierRowKeydown(e: KeyboardEvent, editing: boolean) {
  if (!editing) return;
  if (e.key === "Escape") {
    e.stopPropagation();
    tierEditIndex.value = -1;
    priceError.value = null;
  } else if (e.key === "Enter") {
    confirmTierEdit();
  }
}

function confirmTierEdit() {
  const size = Number(tierForm.sizeK);
  if (!tierForm.sizeK.trim() || !Number.isFinite(size) || size <= 0) {
    priceError.value = "阈值须为正数（单位 K tokens）";
    return;
  }
  const t: TierForm = { sizeK: tierForm.sizeK.trim(), input: "", output: "", cacheRead: "", cacheWrite: "", reasoning: "" };
  for (const k of ["input", "output", "cacheRead", "cacheWrite", "reasoning"] as const) {
    const v = tierForm[k].trim();
    if (v && Number.isNaN(Number(v))) {
      priceError.value = "定价须为数字";
      return;
    }
    t[k] = v;
  }
  if (!t.input && !t.output && !t.cacheRead && !t.cacheWrite && !t.reasoning) {
    priceError.value = "至少填写一项价格";
    return;
  }
  if (tierEditIndex.value >= offerTiers.value.length) offerTiers.value.push(t);
  else offerTiers.value[tierEditIndex.value] = t;
  offerTiers.value.sort((a, b) => Number(a.sizeK) - Number(b.sizeK));
  tierEditIndex.value = -1;
}

const tierNum = (o: Record<string, unknown>, k: string) =>
  typeof o[k] === "number" ? String(o[k]) : "";

/** pricing JSON → 档位表单：tiers 权威优先；仅有 context_over_200k 旧式镜像时提为 200K 档。 */
function parseTiers(p: Record<string, unknown>): TierForm[] {
  const tierOf = (o: Record<string, unknown>): TierForm => ({
    sizeK: "",
    input: tierNum(o, "input"),
    output: tierNum(o, "output"),
    cacheRead: tierNum(o, "cache_read"),
    cacheWrite: tierNum(o, "cache_write"),
    reasoning: tierNum(o, "reasoning"),
  });
  const out: TierForm[] = [];
  if (Array.isArray(p.tiers)) {
    for (const t of p.tiers) {
      const o = t as Record<string, unknown>;
      const meta = o.tier as Record<string, unknown> | undefined;
      if (meta?.type !== "context" || typeof meta.size !== "number") continue;
      out.push({ ...tierOf(o), sizeK: String(meta.size / 1000) });
    }
  }
  const legacy = p.context_over_200k;
  if (out.length === 0 && typeof legacy === "object" && legacy !== null) {
    out.push({ ...tierOf(legacy as Record<string, unknown>), sizeK: "200" });
  }
  return out;
}

// ── 行 = 模型 ⨝ 当前 tab 供应商的报价；孤儿报价只在其所属供应商的 tab 出现 ──
interface ModelRow {
  slug: string;
  def: ModelDef | null;
  offer: Offer | null;
}
const offersBySlug = computed(() => {
  const m = new Map<string, Offer[]>();
  for (const o of offers.value) {
    const l = m.get(o.modelSlug) ?? [];
    l.push(o);
    m.set(o.modelSlug, l);
  }
  for (const l of m.values()) l.sort((a, b) => a.providerKey.localeCompare(b.providerKey));
  return m;
});
const orphanSlugs = computed(() =>
  [...offersBySlug.value.keys()].filter((slug) => !existingSlugs.value.has(slug)),
);
const offerCountOf = (slug: string) => offersBySlug.value.get(slug)?.length ?? 0;
const rows = computed<ModelRow[]>(() => {
  // 全部 tab：所有模型定义 + 孤儿报价，价目区合并显示覆盖的供应商数
  if (activeProvider.value === "") {
    return [
      ...models.value.map((m) => ({ slug: m.slug, def: m as ModelDef | null, offer: null })),
      ...orphanSlugs.value.map((slug) => ({ slug, def: null, offer: null })),
    ];
  }
  // 供应商 tab：只展示该供应商提供报价的模型
  const forActive = (slug: string) =>
    offersBySlug.value.get(slug)?.find((o) => o.providerKey === activeProvider.value) ?? null;
  return [
    ...models.value
      .map((m) => ({ slug: m.slug, def: m as ModelDef | null, offer: forActive(m.slug) }))
      .filter((r) => r.offer !== null),
    ...orphanSlugs.value
      .map((slug): ModelRow => ({ slug, def: null, offer: forActive(slug) }))
      .filter((r) => r.offer !== null),
  ];
});
const offerCountByProvider = computed(() => {
  const m = new Map<string, number>();
  for (const o of offers.value) m.set(o.providerKey, (m.get(o.providerKey) ?? 0) + 1);
  return m;
});
const allEmpty = computed(() => models.value.length === 0 && offers.value.length === 0);

const scrollEl = ref<HTMLElement | null>(null);
const page = ref(1);
const { pageSize } = useAutoPageSize(scrollEl, page);
const pageCount = computed(() => Math.max(1, Math.ceil(rows.value.length / pageSize.value)));
const pagedRows = computed(() => paginate(rows.value, page, pageSize.value));
watch(pageCount, (c) => {
  if (page.value > c) page.value = c;
});

const offerExists = computed(() =>
  offers.value.some((o) => o.providerKey === offerForm.providerKey && o.modelSlug === offerForm.modelSlug),
);

/** 弹窗标题：显示名优先（slug 回退，孤儿报价无定义），@ 上游点明归属；新建未选模型时只给动作名。 */
const offerTitle = computed(() => {
  if (!offerForm.modelSlug) return "添加报价";
  const name = models.value.find((m) => m.slug === offerForm.modelSlug)?.displayName ?? offerForm.modelSlug;
  return offerForm.providerKey ? `${name} @ ${offerForm.providerKey}` : name;
});

/** 打开弹窗前复位行内编辑态，防止上次未确认的编辑脏状态带进新会话。 */
function resetOfferEditState() {
  tierEditIndex.value = -1;
  baseEditing.value = false;
  baseSnapshot = null;
  priceError.value = null;
}

function openAddOffer() {
  offerError.value = null;
  resetOfferEditState();
  pricingExtras.value = {};
  Object.assign(offerForm, {
    modelSlug: "",
    providerKey: activeProvider.value || providers.value[0]?.key || "",
    inputPrice: "",
    outputPrice: "",
    cacheReadPrice: "",
    cacheWritePrice: "",
    reasoningPrice: "",
  });
  offerTiers.value = [];
  offerAdding.value = true;
  offerModal.value = true;
}

function editOffer(o: Offer) {
  offerError.value = null;
  resetOfferEditState();
  const p = (o.pricing ?? {}) as Record<string, unknown>;
  pricingExtras.value = Object.fromEntries(
    Object.entries(p).filter(([k]) => !PRICING_FORM_KEYS.includes(k)),
  );
  offerTiers.value = parseTiers(p);
  const num = (k: string) => tierNum(p, k);
  offerForm.modelSlug = o.modelSlug;
  offerForm.providerKey = o.providerKey;
  offerAdding.value = false;
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
  if (!offerForm.providerKey || !offerForm.modelSlug) {
    offerError.value = "请选择模型与上游服务";
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
  // 档位在二级弹窗内已校验（阈值正数、价格数字、至少一项价），此处直接序列化
  const tiers: Record<string, unknown>[] = [];
  for (const t of offerTiers.value) {
    const obj: Record<string, unknown> = { tier: { type: "context", size: Math.round(Number(t.sizeK) * 1000) } };
    for (const [key, v] of [
      ["input", t.input],
      ["output", t.output],
      ["cache_read", t.cacheRead],
      ["cache_write", t.cacheWrite],
      ["reasoning", t.reasoning],
    ] as const) {
      if (v) obj[key] = Number(v);
    }
    tiers.push(obj);
  }
  if (tiers.length > 0) {
    pricing = { ...(pricing ?? {}), tiers };
    // 旧式单档镜像：存在 200K 档时同步 context_over_200k，兼容旧读取路径
    const t200 = tiers.find((t) => (t.tier as { size: number }).size === 200_000);
    if (t200) {
      const { tier: _omit, ...rest } = t200;
      pricing.context_over_200k = rest;
    }
  }
  offerBusy.value = true;
  try {
    const offer: Offer = {
      providerKey: offerForm.providerKey,
      modelSlug: offerForm.modelSlug,
      pricing,
      // 端点绑定已从弹窗移除：编辑定价时保留存量值（按 provider+model 精确匹配），新建报价默认全部端点
      endpointProtocol:
        offers.value.find(
          (o) => o.providerKey === offerForm.providerKey && o.modelSlug === offerForm.modelSlug,
        )?.endpointProtocol ?? null,
    };
    await modelApi.offerSave(offer);
    offerModal.value = false;
    toast.success(`报价 “${offerForm.modelSlug}” 已保存`);
    // 保存只影响这一行：按 provider+slug 原地增改，避免整表重拉
    offers.value = offers.value.some(
      (o) => o.providerKey === offer.providerKey && o.modelSlug === offer.modelSlug,
    )
      ? offers.value.map((o) =>
          o.providerKey === offer.providerKey && o.modelSlug === offer.modelSlug ? offer : o,
        )
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
    await loadOffers();
  } catch (e) {
    error.value = errMsg(e);
  }
}

async function loadOffers() {
  if (providers.value.length === 0) {
    offers.value = [];
    return;
  }
  offersLoading.value = true;
  try {
    const lists = await Promise.all(providers.value.map((p) => modelApi.offerList(p.key)));
    offers.value = lists.flat();
  } catch (e) {
    error.value = errMsg(e);
  } finally {
    offersLoading.value = false;
  }
}

async function removeOffer(providerKey: string, modelSlug: string): Promise<boolean> {
  if (!(await confirm({ title: "删除报价", message: `确认删除 ${providerKey} 对 “${modelSlug}” 的报价？` }))) return false;
  try {
    await modelApi.offerRemove(providerKey, modelSlug);
    toast.success(`报价 “${modelSlug}” 已删除`);
    offers.value = offers.value.filter(
      (o) => !(o.providerKey === providerKey && o.modelSlug === modelSlug),
    );
    return true;
  } catch (e) {
    error.value = errMsg(e);
    return false;
  }
}

async function removeOfferFromModal() {
  if (await removeOffer(offerForm.providerKey, offerForm.modelSlug)) offerModal.value = false;
}

function price(p: unknown, key: string): string {
  const v = (p as Record<string, unknown> | null)?.[key];
  return typeof v === "number" ? String(v) : "—";
}

/** 长上下文分段计价：pricing 含 tiers/context_over_200k 时返回阈值明细（hover 提示用），无则 null。 */
function tierHint(p: unknown): string | null {
  const o = p as Record<string, unknown> | null;
  if (!o) return null;
  const tiers = Array.isArray(o.tiers) ? o.tiers : [];
  const sizes = tiers
    .map((t) => (t as Record<string, unknown>)?.tier as Record<string, unknown> | undefined)
    .filter((m) => m?.type === "context" && typeof m.size === "number")
    .map((m) => m?.size as number);
  if (o.context_over_200k != null) sizes.push(200_000);
  const uniq = [...new Set(sizes)].sort((a, b) => a - b);
  if (uniq.length === 0) return null;
  return `输入超过阈值后按档价计费：>${uniq.map((s) => s / 1000).join("K / >")}K`;
}

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
    <Alert v-if="error" class="shrink-0">{{ error }}</Alert>

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
        </div>
        <div class="space-y-1.5">
          <Label>模态</Label>
          <div class="flex flex-wrap gap-x-4 gap-y-2 pt-1.5">
            <label
              v-for="m in modalityOptions"
              :key="m"
              class="flex cursor-pointer items-center gap-1.5 text-sm"
            >
              <Checkbox
                :checked="form.modalities.includes(m)"
                @update:checked="toggleArr(form.modalities, m)"
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

    <Modal
      :open="importModal"
      title="从 models.dev 导入模型"
      width="max-w-3xl"
      @close="importModal = false"
    >
      <Alert v-if="importError" class="mb-3 px-3">{{ importError }}</Alert>

      <div class="mb-3 flex items-center gap-2">
        <div class="relative flex-1">
          <Search class="pointer-events-none absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
          <Input v-model="importQuery" placeholder="搜索模型 / provider…" class="pl-8" :disabled="importLoading" />
        </div>
        <Button variant="outline" size="sm" :disabled="importLoading" @click="refreshCatalog">
          {{ importLoading ? "拉取中…" : "刷新" }}
        </Button>
      </div>

      <EmptyState v-if="importLoading">正在从 models.dev 拉取模型库…</EmptyState>
      <EmptyState v-else-if="catalog.length === 0" :icon="Boxes">
        未获取到模型。点「刷新」重试，或检查网络。
      </EmptyState>
      <template v-else>
        <div class="mb-2 flex items-center justify-between text-xs text-muted-foreground">
          <label class="flex cursor-pointer items-center gap-1.5">
            <Checkbox
              :checked="allFilteredSelected"
              :indeterminate="someFilteredSelected"
              @update:checked="toggleSelectAllFiltered"
            />
            <span>全选当前结果（{{ filteredCatalog.length }}）</span>
          </label>
          <span>已选 {{ importSelected.size }}</span>
        </div>
        <div ref="importScroll" class="scrollbar-thin max-h-[52vh] overflow-y-auto rounded-md border">
          <table class="w-full text-center text-sm">
            <thead class="thead-sticky">
              <tr class="border-b text-muted-foreground">
                <th class="w-8 py-2"></th>
                <th class="py-2 font-medium">模型</th>
                <th class="py-2 font-medium">Provider</th>
                <th class="py-2 font-medium">上下文</th>
                <th class="py-2 font-medium">输出上限</th>
                <th class="py-2 font-medium">输入/输出</th>
                <th class="py-2 font-medium">状态</th>
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
                  <Checkbox
                    :checked="importSelected.has(catalogKey(m))"
                    @click.stop
                    @update:checked="toggleSelect(m)"
                  />
                </td>
                <td class="py-1.5">
                  <div class="font-mono text-xs">{{ m.id }}</div>
                  <div v-if="m.name" class="text-xs text-muted-foreground">{{ m.name }}</div>
                </td>
                <td class="py-1.5 text-xs text-muted-foreground">{{ m.providerName }}</td>
                <td class="py-1.5 tabular-nums text-muted-foreground">{{ formatCtx(m.contextWindow) }}</td>
                <td class="py-1.5 tabular-nums text-muted-foreground">{{ formatCtx(m.maxOutputTokens) }}</td>
                <td class="py-1.5 tabular-nums text-muted-foreground">{{ catalogPrice(m) }}</td>
                <td class="py-1.5 text-center">
                  <span v-if="existingSlugs.has(m.id)" class="text-xs text-muted-foreground">已存在</span>
                  <span v-else class="text-xs text-emerald-600 dark:text-emerald-500">新增</span>
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

    <!-- 模型 + 报价：满版单表；报价列归组表头所选上游服务 -->
    <div class="flex min-h-0 flex-1 flex-col overflow-hidden">
      <!-- Provider tab：报价列归属于当前选中的上游服务；下划线为共享滑动墨线 -->
      <div v-if="providers.length > 0" ref="tabBarRef" class="relative flex shrink-0 border-b px-5">
        <span
          class="tab-ink"
          :style="{ transform: `translateX(${tabInk.x}px)`, width: `${tabInk.w}px`, opacity: tabInk.on ? 1 : 0 }"
        />
        <button
          type="button"
          :data-active="activeProvider === '' || undefined"
          class="flex min-w-0 items-center gap-1.5 whitespace-nowrap px-3 py-2.5 text-sm transition-colors focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
          :class="
            activeProvider === ''
              ? 'font-semibold text-foreground'
              : 'font-medium text-muted-foreground hover:text-foreground'
          "
          @click="activeProvider = ''"
        >
          全部
        </button>
        <button
          v-for="p in providers"
          :key="p.key"
          type="button"
          :data-active="p.key === activeProvider || undefined"
          class="flex min-w-0 items-center gap-1.5 whitespace-nowrap px-3 py-2.5 text-sm transition-colors focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
          :class="[
            p.key === activeProvider
              ? 'font-semibold text-foreground'
              : 'font-medium text-muted-foreground hover:text-foreground',
            !p.enabled ? 'opacity-50' : '',
          ]"
          :title="p.enabled ? p.key : `${p.key}（已停用）`"
          @click="activeProvider = p.key"
        >
          <span class="truncate font-mono">{{ p.key }}</span>
          <span v-if="offerCountByProvider.get(p.key)" class="shrink-0 text-muted-foreground/60">{{
            offerCountByProvider.get(p.key)
          }}</span>
        </button>
      </div>
      <div
        ref="scrollEl"
        class="pane-stack scrollbar-thin min-h-0 flex-1 overflow-auto px-5 py-4"
        :class="[{ 'pane-transitioning': paneTransitioning }, paneDir > 0 ? 'dir-fwd' : 'dir-back']"
      >
        <Transition
          name="pane"
          @before-leave="paneTransitioning = true"
          @after-leave="paneTransitioning = false"
          @leave-cancelled="paneTransitioning = false"
        >
          <div :key="activeProvider" class="pane-item">
        <EmptyState v-if="modelsLoading && allEmpty">加载中…</EmptyState>
        <EmptyState v-else-if="allEmpty" :icon="Boxes">
          暂无模型定义。
          <template #action>
            <Button size="sm" @click="newModel"><Plus class="size-4" /> 新建模型</Button>
            <Button variant="outline" size="sm" @click="openImport">
              <CloudDownload class="size-4" /> 从 models.dev 导入
            </Button>
          </template>
        </EmptyState>
        <table
          v-else
          class="w-full text-center text-sm [&_td]:px-2 [&_th]:px-2 [&_td:first-child]:pl-0 [&_td:last-child]:pr-0 [&_th:first-child]:pl-0 [&_th:last-child]:pr-0"
        >
          <thead class="thead-sticky">
            <tr class="border-b text-muted-foreground">
              <th class="py-2 font-medium">模型</th>
              <th class="py-2 font-medium">上下文</th>
              <th class="py-2 font-medium">输出上限</th>
              <template v-if="activeProvider !== ''">
                <th class="py-2 font-medium">输入</th>
                <th class="py-2 font-medium">输出</th>
                <th class="py-2 font-medium">缓存读</th>
                <th class="py-2 font-medium">缓存写</th>
                <th class="py-2 font-medium">推理</th>
              </template>
              <th v-else class="py-2 font-medium">供应商报价</th>
              <th class="py-2 font-medium">操作</th>
            </tr>
          </thead>
          <tbody>
            <!-- 一行 = 一个模型；供应商 tab 的价格列即该供应商对该模型的报价 -->
            <tr
              v-for="r in pagedRows"
              :key="r.slug"
              class="border-b transition-colors last:border-0 hover:bg-accent/40"
            >
              <td class="py-2">
                <div class="text-sm font-medium">
                  {{ r.def?.displayName ?? r.slug }}
                  <span
                    v-if="r.def === null"
                    class="ml-1 rounded bg-muted px-1 py-px font-sans text-[10px] text-muted-foreground"
                    title="该报价对应的模型定义已删除，只剩此报价记录"
                    >无定义</span
                  >
                </div>
                <div class="mt-0.5 flex items-center justify-center font-mono text-xs text-muted-foreground">
                  <span class="truncate" :title="r.slug">{{ r.slug }}</span>
                  <span
                    v-if="r.offer?.endpointProtocol"
                    class="shrink-0 text-muted-foreground/60"
                    :title="`绑定端点协议：${r.offer.endpointProtocol}`"
                    >·{{ r.offer.endpointProtocol }}</span
                  >
                  <span
                    v-if="r.offer && tierHint(r.offer.pricing)"
                    class="shrink-0 whitespace-nowrap rounded bg-muted px-1 py-px font-sans text-[10px]"
                    :title="tierHint(r.offer.pricing) ?? undefined"
                    >分段计价</span
                  >
                </div>
              </td>
              <td class="py-2 tabular-nums text-muted-foreground">
                {{ formatCtx(r.def?.contextWindow) }}
              </td>
              <td class="py-2 tabular-nums text-muted-foreground">
                {{ formatCtx(r.def?.maxOutputTokens) }}
              </td>
              <!-- 供应商 tab：五列价目；全部 tab：报价覆盖列 -->
              <template v-if="activeProvider !== ''">
                <td class="py-2 tabular-nums">{{ price(r.offer?.pricing, "input") }}</td>
                <td class="py-2 tabular-nums">{{ price(r.offer?.pricing, "output") }}</td>
                <td class="py-2 tabular-nums">{{ price(r.offer?.pricing, "cache_read") }}</td>
                <td class="py-2 tabular-nums">{{ price(r.offer?.pricing, "cache_write") }}</td>
                <td class="py-2 tabular-nums">{{ price(r.offer?.pricing, "reasoning") }}</td>
              </template>
              <td v-else class="py-2 tabular-nums text-xs text-muted-foreground">
                {{ offerCountOf(r.slug) > 0 ? `${offerCountOf(r.slug)} 家` : "—" }}
              </td>
              <td class="py-2">
                <div class="flex items-center justify-center gap-0.5">
                  <template v-if="r.offer">
                    <Button
                      variant="ghost"
                      size="icon"
                      class="size-7"
                      title="编辑定价"
                      @click="editOffer(r.offer)"
                    >
                      <CircleDollarSign class="size-3.5" />
                    </Button>
                    <Button
                      variant="ghost"
                      size="icon"
                      class="size-7"
                      title="删除报价"
                      @click="removeOffer(r.offer.providerKey, r.offer.modelSlug)"
                    >
                      <Trash2 class="size-3.5 text-destructive" />
                    </Button>
                    <span v-if="r.def" class="mx-0.5 h-3.5 w-px bg-border" />
                  </template>
                  <template v-if="r.def">
                    <Button
                      variant="ghost"
                      size="icon"
                      class="size-7"
                      title="编辑模型"
                      @click="editModel(r.def!)"
                    >
                      <Pencil class="size-3.5" />
                    </Button>
                    <Button
                      variant="ghost"
                      size="icon"
                      class="size-7"
                      title="删除模型"
                      @click="removeModel(r.slug)"
                    >
                      <Trash2 class="size-3.5 text-destructive" />
                    </Button>
                  </template>
                </div>
              </td>
            </tr>
            <tr v-if="pagedRows.length === 0 && activeProvider !== ''">
              <td colspan="9" class="py-8 text-center text-xs text-muted-foreground">
                该供应商暂无报价
              </td>
            </tr>
            <tr class="last:border-0">
              <td :colspan="activeProvider === '' ? 5 : 9" class="py-1">
                <div class="flex items-center justify-end gap-4">
                  <button
                    v-if="activeProvider !== ''"
                    type="button"
                    class="flex items-center gap-1.5 rounded-sm py-1.5 text-xs text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
                    @click="openAddOffer"
                  >
                    <CircleDollarSign class="size-3.5" /> 添加报价
                  </button>
                  <button
                    type="button"
                    class="flex items-center gap-1.5 rounded-sm py-1.5 text-xs text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
                    @click="openImport"
                  >
                    <CloudDownload class="size-3.5" /> 从 models.dev 导入
                  </button>
                  <button
                    type="button"
                    class="flex items-center gap-1.5 rounded-sm py-1.5 text-xs text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
                    @click="newModel"
                  >
                    <Plus class="size-3.5" /> 新建模型
                  </button>
                </div>
              </td>
            </tr>
          </tbody>
        </table>
          </div>
        </Transition>
      </div>
      <div v-if="rows.length > pageSize" class="shrink-0 border-t px-5 py-2">
        <Pagination v-model:page="page" :page-count="pageCount" :total="rows.length" />
      </div>
    </div>

    <!-- 报价定价弹窗：五类 token 独立定价 -->
    <Modal
      :open="offerModal"
      :title="offerTitle"
      width="max-w-lg"
      @close="offerModal = false"
    >
      <Alert v-if="offerError" class="mb-4 px-3">{{ offerError }}</Alert>
      <!-- 新建时选定模型与供应商（供应商默认当前 tab）；编辑时两者已在标题中锁定 -->
      <div v-if="offerAdding" class="mb-4 grid grid-cols-2 gap-3">
        <div class="flex items-center gap-2">
          <Label class="shrink-0">模型</Label>
          <Select v-model="offerForm.modelSlug" :options="modelOptions" placeholder="选择模型" small searchable class="min-w-0 flex-1" />
        </div>
        <div class="flex items-center gap-2">
          <Label class="shrink-0">上游服务</Label>
          <Select v-model="offerForm.providerKey" :options="providerOptions" placeholder="选择上游服务" small class="min-w-0 flex-1" />
        </div>
      </div>
      <!-- 价目表：基础价是阶梯最底档（任意输入生效），其上各行按 input tokens 阈值逐档覆盖 -->
      <table class="w-full text-center text-sm">
        <thead>
          <tr class="border-b text-xs text-muted-foreground">
            <th class="w-24 py-1.5 font-medium">上下文长度</th>
            <th class="py-1.5 font-medium">输入</th>
            <th class="py-1.5 font-medium">输出</th>
            <th class="py-1.5 font-medium">缓存读</th>
            <th class="py-1.5 font-medium">缓存写</th>
            <th class="py-1.5 font-medium">推理</th>
            <th class="w-16">
              <div class="flex justify-center">
                <Button
                  variant="ghost"
                  size="icon"
                  class="size-7"
                  title="添加档位"
                  :disabled="tierEditIndex >= 0 || baseEditing"
                  @click="openTierAdd()"
                >
                  <Plus class="size-3.5" />
                </Button>
              </div>
            </th>
          </tr>
        </thead>
        <tbody>
          <tr class="border-b last:border-0" @keydown="onBaseRowKeydown">
            <td class="py-1.5 text-muted-foreground">基础</td>
            <template v-if="baseEditing">
              <td class="py-1">
                <Input v-model="offerForm.inputPrice" class="h-7 px-2 text-center tabular-nums" placeholder="0" inputmode="decimal" autofocus />
              </td>
              <td class="py-1">
                <Input v-model="offerForm.outputPrice" class="h-7 px-2 text-center tabular-nums" placeholder="0" inputmode="decimal" />
              </td>
              <td class="py-1">
                <Input v-model="offerForm.cacheReadPrice" class="h-7 px-2 text-center tabular-nums" :placeholder="offerForm.inputPrice || '0'" inputmode="decimal" />
              </td>
              <td class="py-1">
                <Input v-model="offerForm.cacheWritePrice" class="h-7 px-2 text-center tabular-nums" :placeholder="offerForm.inputPrice || '0'" inputmode="decimal" />
              </td>
              <td class="py-1">
                <Input v-model="offerForm.reasoningPrice" class="h-7 px-2 text-center tabular-nums" :placeholder="offerForm.outputPrice || '0'" inputmode="decimal" />
              </td>
              <td class="py-1">
                <div class="flex justify-center gap-0.5">
                  <Button variant="ghost" size="icon" class="size-7" title="确认" @click="confirmBaseEdit">
                    <Check class="size-3.5" />
                  </Button>
                  <Button variant="ghost" size="icon" class="size-7" title="取消" @click="cancelBaseEdit">
                    <X class="size-3.5" />
                  </Button>
                </div>
              </td>
            </template>
            <template v-else>
              <td class="py-1.5 text-center tabular-nums">{{ offerForm.inputPrice || "—" }}</td>
              <td class="py-1.5 text-center tabular-nums">{{ offerForm.outputPrice || "—" }}</td>
              <td class="py-1.5 text-center tabular-nums">{{ offerForm.cacheReadPrice || "—" }}</td>
              <td class="py-1.5 text-center tabular-nums">{{ offerForm.cacheWritePrice || "—" }}</td>
              <td class="py-1.5 text-center tabular-nums">{{ offerForm.reasoningPrice || "—" }}</td>
              <td class="py-1">
                <div class="flex justify-center gap-0.5">
                  <Button
                    variant="ghost"
                    size="icon"
                    class="size-7"
                    title="编辑基础价"
                    :disabled="tierEditIndex >= 0"
                    @click="openBaseEdit"
                  >
                    <Pencil class="size-3.5" />
                  </Button>
                </div>
              </td>
            </template>
          </tr>
          <tr
            v-for="(t, i) in offerTiers"
            :key="i"
            class="border-b last:border-0"
            @keydown="onTierRowKeydown($event, tierEditIndex === i)"
          >
            <template v-if="tierEditIndex === i">
              <td class="py-1">
                <Input v-model="tierForm.sizeK" class="h-7 w-16 px-2 text-center tabular-nums" placeholder="200" inputmode="decimal" />
              </td>
              <td class="py-1"><Input v-model="tierForm.input" class="h-7 px-2 text-center tabular-nums" :placeholder="offerForm.inputPrice || '0'" inputmode="decimal" /></td>
              <td class="py-1"><Input v-model="tierForm.output" class="h-7 px-2 text-center tabular-nums" :placeholder="offerForm.outputPrice || '0'" inputmode="decimal" /></td>
              <td class="py-1"><Input v-model="tierForm.cacheRead" class="h-7 px-2 text-center tabular-nums" :placeholder="offerForm.cacheReadPrice || tierForm.input || offerForm.inputPrice || '0'" inputmode="decimal" /></td>
              <td class="py-1"><Input v-model="tierForm.cacheWrite" class="h-7 px-2 text-center tabular-nums" :placeholder="offerForm.cacheWritePrice || tierForm.input || offerForm.inputPrice || '0'" inputmode="decimal" /></td>
              <td class="py-1"><Input v-model="tierForm.reasoning" class="h-7 px-2 text-center tabular-nums" :placeholder="offerForm.reasoningPrice || tierForm.output || offerForm.outputPrice || '0'" inputmode="decimal" /></td>
              <td class="py-1">
                <div class="flex justify-center gap-0.5">
                  <Button variant="ghost" size="icon" class="size-7" title="确认" @click="confirmTierEdit">
                    <Check class="size-3.5" />
                  </Button>
                  <Button variant="ghost" size="icon" class="size-7" title="取消" @click="tierEditIndex = -1">
                    <X class="size-3.5" />
                  </Button>
                </div>
              </td>
            </template>
            <template v-else>
              <td class="py-1.5 tabular-nums">&gt; {{ t.sizeK }}K</td>
              <td class="py-1.5 text-center tabular-nums">{{ t.input || "—" }}</td>
              <td class="py-1.5 text-center tabular-nums">{{ t.output || "—" }}</td>
              <td class="py-1.5 text-center tabular-nums">{{ t.cacheRead || "—" }}</td>
              <td class="py-1.5 text-center tabular-nums">{{ t.cacheWrite || "—" }}</td>
              <td class="py-1.5 text-center tabular-nums">{{ t.reasoning || "—" }}</td>
              <td class="py-1">
                <div class="flex justify-center gap-0.5">
                  <Button variant="ghost" size="icon" class="size-7" title="编辑档位" :disabled="tierEditIndex >= 0 || baseEditing" @click="openTierEdit(i)">
                    <Pencil class="size-3.5" />
                  </Button>
                  <Button variant="ghost" size="icon" class="size-7" title="删除档位" :disabled="tierEditIndex >= 0 || baseEditing" @click="offerTiers.splice(i, 1)">
                    <Trash2 class="size-3.5" />
                  </Button>
                </div>
              </td>
            </template>
          </tr>
          <tr
            v-if="tierEditIndex === offerTiers.length"
            class="border-b last:border-0"
            @keydown="onTierRowKeydown($event, true)"
          >
            <td class="py-1">
              <Input v-model="tierForm.sizeK" class="h-7 w-16 px-2 text-center tabular-nums" placeholder="200" inputmode="decimal" autofocus />
            </td>
            <td class="py-1"><Input v-model="tierForm.input" class="h-7 px-2 text-center tabular-nums" :placeholder="offerForm.inputPrice || '0'" inputmode="decimal" /></td>
            <td class="py-1"><Input v-model="tierForm.output" class="h-7 px-2 text-center tabular-nums" :placeholder="offerForm.outputPrice || '0'" inputmode="decimal" /></td>
            <td class="py-1"><Input v-model="tierForm.cacheRead" class="h-7 px-2 text-center tabular-nums" :placeholder="offerForm.cacheReadPrice || tierForm.input || offerForm.inputPrice || '0'" inputmode="decimal" /></td>
            <td class="py-1"><Input v-model="tierForm.cacheWrite" class="h-7 px-2 text-center tabular-nums" :placeholder="offerForm.cacheWritePrice || tierForm.input || offerForm.inputPrice || '0'" inputmode="decimal" /></td>
            <td class="py-1"><Input v-model="tierForm.reasoning" class="h-7 px-2 text-center tabular-nums" :placeholder="offerForm.reasoningPrice || tierForm.output || offerForm.outputPrice || '0'" inputmode="decimal" /></td>
            <td class="py-1">
              <div class="flex justify-center gap-0.5">
                <Button variant="ghost" size="icon" class="size-7" title="确认" @click="confirmTierEdit">
                  <Check class="size-3.5" />
                </Button>
                <Button variant="ghost" size="icon" class="size-7" title="取消" @click="tierEditIndex = -1">
                  <X class="size-3.5" />
                </Button>
              </div>
            </td>
          </tr>
        </tbody>
      </table>
      <p v-if="priceError" class="mt-2 text-xs text-destructive">{{ priceError }}</p>
      <template #footer>
        <Button
          v-if="offerExists"
          variant="destructive"
          size="sm"
          class="mr-auto"
          @click="removeOfferFromModal"
        >
          删除报价
        </Button>
        <Button variant="ghost" size="sm" @click="offerModal = false">取消</Button>
        <Button size="sm" :disabled="offerBusy || tierEditIndex >= 0" @click="confirmOffer">{{ offerBusy ? "保存中…" : "保存" }}</Button>
      </template>
    </Modal>
  </div>
</template>

<style scoped>
/* 切 tab：内容区 grid 叠放。两方向都只向左侧越界——可滚动区不增长，
   无横向滚动条闪现（dir-fwd 向右选 tab：旧内容向左滑出揭开新内容；
   dir-back：新内容自左滑入盖住旧内容）。与路由纵向滑动同构。 */
.pane-stack {
  display: grid;
}
.pane-stack > * {
  grid-area: 1 / 1;
  min-width: 0;
  min-height: 0;
}
.pane-stack.dir-fwd .pane-leave-active {
  position: relative;
  z-index: 1;
  pointer-events: none;
  animation: pane-out-left var(--dur-base) var(--ease-in) both;
}
.pane-stack.dir-back .pane-enter-active {
  position: relative;
  z-index: 1;
  pointer-events: none;
  animation: pane-in-right var(--dur-base) var(--ease-out) both;
}
.pane-stack.dir-back .pane-leave-active {
  animation: pane-stay var(--dur-base) linear both;
}
.pane-stack.dir-fwd > .pane-leave-active {
  background-color: hsl(var(--background));
}
.pane-stack.dir-back > .pane-enter-active {
  background-color: hsl(var(--background));
}
@keyframes pane-out-left {
  to {
    transform: translateX(-100%);
  }
}
@keyframes pane-in-right {
  from {
    transform: translateX(-100%);
  }
}
@keyframes pane-stay {
  to {
    transform: translateX(0.01px);
  }
}
/* 过渡期间表头压平，跟表身一起滑走/被揭开（同路由切换的处理） */
.pane-transitioning .thead-sticky {
  position: static;
}
</style>
