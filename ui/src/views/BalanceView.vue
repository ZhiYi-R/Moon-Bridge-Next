<script setup lang="ts">
import { ChevronDown, HelpCircle, Pencil, Plus, RefreshCw, Trash2, Wallet } from "lucide-vue-next";
import { computed, onActivated, onMounted, reactive, ref, watch } from "vue";

import BalanceScriptGuide from "@/components/balance/BalanceScriptGuide.vue";
import Alert from "@/components/ui/Alert.vue";
import Badge from "@/components/ui/Badge.vue";
import Button from "@/components/ui/Button.vue";
import CodeEditor from "@/components/ui/CodeEditor.vue";
import EmptyState from "@/components/ui/EmptyState.vue";
import Input from "@/components/ui/Input.vue";
import Label from "@/components/ui/Label.vue";
import Modal from "@/components/ui/Modal.vue";
import SegmentedControl from "@/components/ui/SegmentedControl.vue";
import Select from "@/components/ui/Select.vue";
import Switch from "@/components/ui/Switch.vue";
import Pagination from "@/components/ui/Pagination.vue";
import { useAutoPageSize } from "@/composables/useAutoPageSize";
import { useConfirm } from "@/composables/useConfirm";
import { useToast } from "@/composables/useToast";
import { errMsg, balanceApi, providerApi, usageApi, type BalanceCard, type BalanceCardView, type BalanceKeyResult, type BalancePayload, type BalanceQuota, type DisplayMode, type Provider, type ProviderCost } from "@/lib/api";
import { BALANCE_TEMPLATES, DEFAULT_BALANCE_SCRIPT, type BalanceTemplate } from "@/lib/balanceTemplates";
import { formatCost, formatTime } from "@/lib/utils";
import { useBalanceStore } from "@/stores/balance";

/** 新建卡片的默认内联脚本（模板库的「通用百分比」）。 */
const DEFAULT_SCRIPT = DEFAULT_BALANCE_SCRIPT;

/** 显示样式选项（卡片快捷切换与编辑表单同源）。 */
const DISPLAY_MODES: { value: DisplayMode; label: string }[] = [
  { value: "auto", label: "自动" },
  { value: "percent", label: "百分比" },
  { value: "amount", label: "金额" },
];

const store = useBalanceStore();
const { confirm } = useConfirm();
const toast = useToast();

const error = ref<string | null>(null);
const formError = ref<string | null>(null);
const editing = ref(false);
const isNew = ref(false);
const busy = ref(false);
const guideOpen = ref(false);

/** 卡片配额的显示状态：收起为文字，展开为环形图（默认收起）。 */
const chartOpen = ref<Record<string, boolean>>({});

function toggleChart(key: string) {
  chartOpen.value[key] = !chartOpen.value[key];
}

interface Form {
  key: string;
  providerKey: string;
  apiKey: string;
  baseUrl: string;
  displayMode: DisplayMode;
  intervalSecs: number;
  enabled: boolean;
  scriptRef: string;
  inlineScript: string;
  extraText: string;
}

const form = reactive<Form>({
  key: "",
  providerKey: "",
  apiKey: "",
  baseUrl: "",
  displayMode: "auto",
  intervalSecs: 900,
  enabled: true,
  scriptRef: "",
  inlineScript: DEFAULT_SCRIPT,
  extraText: "{}",
});

// ── 未保存守卫：进入编辑时拍快照，关闭弹窗时比对 ──
const snapshot = ref("");
const dirty = computed(() => JSON.stringify(form) !== snapshot.value);

function takeSnapshot() {
  snapshot.value = JSON.stringify(form);
}

async function guardClose(): Promise<boolean> {
  if (!dirty.value) return true;
  return await confirm({
    title: "关闭编辑",
    message: "有未保存的修改，确认丢弃？",
    confirmText: "丢弃修改",
  });
}

// ── 展示辅助 ──

function quotaLeft(q: BalanceQuota): number {
  if (typeof q.leftPercent === "number") return q.leftPercent;
  if (typeof q.usedPercent === "number") return 100 - q.usedPercent;
  return 0;
}

/** 卡片是否有百分比口径的配额数据（强制百分比模式据此决定「—」）。 */
function hasPercent(q: BalanceQuota): boolean {
  return typeof q.usedPercent === "number" || typeof q.leftPercent === "number";
}

/** 金额口径判定：强制金额模式也只对「有金额字段」的配额生效；
 *  纯百分比配额一律回退按百分比渲染（数据兜底优先于显示模式）。 */
function isAmountMode(mode: DisplayMode, q: BalanceQuota): boolean {
  if (mode === "percent") return false;
  return typeof q.usedAmount === "number" || typeof q.leftAmount === "number";
}

/** 金额文案「消耗 x{unit} · 余额 y{unit}」；used/left 都缺时返回 null。
 *  金额原样渲染（不取整，可能带小数）；单位缺省时只显示数字。 */
function amountLine(q: BalanceQuota): string | null {
  const unit = q.unit ?? "";
  const parts: string[] = [];
  if (typeof q.usedAmount === "number") parts.push(`消耗 ${q.usedAmount}${unit}`);
  if (typeof q.leftAmount === "number") parts.push(`余额 ${q.leftAmount}${unit}`);
  return parts.length > 0 ? parts.join(" · ") : null;
}

/** 悬停详情行：百分比口径补剩余（已用比在环心）；金额口径逐项列消耗与余额。 */
function quotaDetailLines(mode: DisplayMode, q: BalanceQuota): string[] {
  if (isAmountMode(mode, q)) return amountLine(q)?.split(" · ") ?? ["—"];
  return hasPercent(q) ? [`剩余 ${quotaLeft(q).toFixed(0)}%`] : ["—"];
}

/** 悬停完整提示：详情行 + 重置时间，一行串起。 */
function quotaDetailText(mode: DisplayMode, q: BalanceQuota): string {
  const lines = quotaDetailLines(mode, q);
  const reset = resetText(q);
  if (reset) lines.push(`重置 ${reset}`);
  return lines.join(" · ");
}

const RING_C = 2 * Math.PI * 15.5;

/** 环形图比例：缺该口径所需字段时返回 null（不渲染环形图）。 */
function quotaRingPercent(mode: DisplayMode, q: BalanceQuota): number | null {
  const clamp = (v: number) => Math.min(100, Math.max(0, v));
  if (isAmountMode(mode, q)) {
    if (typeof q.usedAmount !== "number" || typeof q.leftAmount !== "number") return null;
    const total = q.usedAmount + q.leftAmount;
    return total > 0 ? clamp((q.usedAmount / total) * 100) : null;
  }
  if (typeof q.usedPercent === "number") return clamp(q.usedPercent);
  if (typeof q.leftPercent === "number") return clamp(100 - q.leftPercent);
  return null;
}

const DISPLAY_MODE_LABEL: Record<DisplayMode, string> = { auto: "自动", percent: "百分比", amount: "金额" };

/** 上次查询的短格式：当天只给时分秒，跨天补月/日；完整时间在 title。 */
function shortTime(sec: number): string {
  if (!sec) return "—";
  const d = new Date(sec * 1000);
  const pad = (n: number) => String(n).padStart(2, "0");
  const hm = `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
  const today = new Date();
  return d.toDateString() === today.toDateString() ? hm : `${d.getMonth() + 1}/${d.getDate()} ${hm}`;
}

/** 重置时间文案：纯数字串视为 unix 秒（脚本可以直接给数字，沙箱里没有 os 库，
 *  没法自己格式化时间）转为本地时间；其余字符串原样展示。 */
function resetText(q: BalanceQuota): string {
  const v = q.resetAt;
  if (v === null || v === undefined || v === "") return "";
  return /^\d{9,11}$/.test(v) ? formatTime(Number(v)) : v;
}

/** 值压成一行给人看：字符串原样，数组逐项顿号分隔，对象展开成「键：值」分号连接。 */
function scalarText(v: unknown): string {
  if (typeof v === "string") return v;
  if (typeof v === "number" || typeof v === "boolean") return String(v);
  if (Array.isArray(v)) return v.map(scalarText).join("、");
  if (typeof v === "object" && v !== null) {
    return Object.entries(v as Record<string, unknown>)
      .map(([k, val]) => `${k}：${scalarText(val)}`)
      .join("；");
  }
  return String(v);
}

/** 查询结果里除配额（画成进度条/环形图）、失败原因和说明（逐 key 重复的样板文本）外的字段，按「字段名：值」逐行给出。 */
function payloadLines(payload: Record<string, unknown>): { label: string; value: string }[] {
  const lines: { label: string; value: string }[] = [];
  for (const [k, v] of Object.entries(payload)) {
    if (v === null || v === undefined || k === "quotas" || k === "message" || k === "summary") continue;
    lines.push({ label: k, value: scalarText(v) });
  }
  return lines;
}

/** 无界计数：只有 usedAmount，无剩余量/百分比/重置窗口——是计数器不是配额，列表不展示。 */
function isBareCounter(q: BalanceQuota): boolean {
  return (
    typeof q.usedAmount === "number" &&
    q.leftAmount == null &&
    q.usedPercent == null &&
    q.leftPercent == null &&
    (q.resetAt == null || q.resetAt === "")
  );
}

/** 表格实际展示的配额行：剔除无界计数。 */
function visibleQuotas(payload: BalancePayload | null | undefined): BalanceQuota[] {
  return (payload?.quotas ?? []).filter((q) => !isBareCounter(q));
}

/** 配额的一行文字：百分比、金额、重置时间合并成一句话；缩略场景传 withReset=false（重置时间仍在 title）。 */
function quotaText(q: BalanceQuota, withReset = true): string {
  const parts: string[] = [];
  if (typeof q.usedPercent === "number") parts.push(`已用 ${q.usedPercent.toFixed(0)}%`);
  else if (typeof q.leftPercent === "number") parts.push(`剩余 ${quotaLeft(q).toFixed(0)}%`);
  if (typeof q.usedAmount === "number") parts.push(`消耗 ${q.usedAmount}${q.unit ?? ""}`);
  if (typeof q.leftAmount === "number") parts.push(`余额 ${q.leftAmount}${q.unit ?? ""}`);
  if (withReset) {
    const reset = resetText(q);
    if (reset) parts.push(`重置 ${reset}`);
  }
  return parts.join(" · ") || "—";
}

/** 卡片分组键：上游服务 key 优先，旧卡遗留的展示名兜底，再落「未分组」并排最后；
 *  其余组名按中文排序，组内沿用后端口径（position 升序，再 createdAt）。 */
const groups = computed(() => {
  const map = new Map<string, { label: string; ungrouped: boolean; cards: BalanceCardView[] }>();
  for (const c of store.cards) {
    const label = (c.providerKey ?? "").trim() || (c.providerLabel ?? "").trim();
    const g = map.get(label) ?? { label: label || "未分组", ungrouped: !label, cards: [] };
    g.cards.push(c);
    map.set(label, g);
  }
  return [...map.values()]
    .map((g) => ({
      ...g,
      cards: [...g.cards].sort((a, b) => a.position - b.position || a.createdAt - b.createdAt),
    }))
    .sort((a, b) => {
      if (a.ungrouped !== b.ungrouped) return a.ungrouped ? 1 : -1;
      return a.label.localeCompare(b.label, "zh");
    });
});

/** 拍平的卡片序列：沿用分组排序（未分组垫底、组名中文序、组内按 position），网格不再按组分行。 */
const flatCards = computed(() => groups.value.flatMap((g) => g.cards));

// ── 上游服务候选：卡片引用 provider.key，表单下拉即取其列表 ──

const providers = ref<Provider[]>([]);

async function ensureProviders() {
  if (providers.value.length > 0) return;
  providers.value = await providerApi.list().catch(() => [] as Provider[]);
}

const providerOptions = computed(() => [
  { value: "", label: "不引用上游服务" },
  ...providers.value.map((p) => ({ value: p.key, label: p.key })),
]);

function manualKeys(value: string): string[] {
  return [...new Set(value.split(/\r?\n/).map((key) => key.trim()).filter(Boolean))];
}

const formManualKeys = computed(() => manualKeys(form.apiKey));

/** 表单当前选中的上游服务（未选中时为 undefined）。 */
const selectedProvider = computed(() =>
  providers.value.find((p) => p.key === form.providerKey),
);

/** 单个 provider 的端点与去重后非空 key 数（仅用于表单提示，不复刻回退规则）。 */
function providerSummary(p: Provider): string {
  const keys = new Set(p.endpoints.map((e) => e.apiKey).filter((k) => k));
  return `${p.endpoints.length} 个端点 · ${keys.size} 个 key（去重后）`;
}

const selectedProviderSummary = computed(() =>
  selectedProvider.value ? providerSummary(selectedProvider.value) : "",
);

/** 卡片 key 徽标的悬停补充：Key 来源，不展示密钥原文。 */
function cardBindingText(c: BalanceCardView): string {
  const count = manualKeys(c.apiKey).length;
  const target = count > 0
    ? `${c.providerKey ? `${c.providerKey} · ` : ""}手动 Key（${count} 个）`
    : c.providerKey || "未配置 Key";
  const url = (c.baseUrl ?? "").trim();
  return url ? `${target} · ${url}` : target;
}

/** 组行 key 数：已查询按结果数，否则按手动 key 数兜底。 */
function keyCount(c: BalanceCardView): number {
  return c.results.length || manualKeys(c.apiKey).length;
}

/** 卡片最近一次查询时刻（跨 key 取 max）。 */
function cardLastQuery(c: BalanceCardView): number {
  let m = 0;
  for (const r of c.results) if (r.queriedAt > m) m = r.queriedAt;
  return m;
}

// ── 分页：按卡片组整组分页，容量随可视高度自适应（估算行高留余量防溢出） ──
const page = ref(1);
const tableScroll = ref<HTMLElement | null>(null);
const { availHeight } = useAutoPageSize(tableScroll, page);

/** 卡片块的估算高度（px）：组行 + 各 key 行；配额/附加字段按行高累加，展开态按环形图行高。 */
function estCardHeight(c: BalanceCardView): number {
  const GROUP = 32;
  const EMPTY = 37;
  const KEY_BASE = 25;
  const QUOTA_LINE = 19;
  const RING = 96;
  let h = GROUP;
  if (c.results.length === 0) return h + EMPTY;
  for (const r of c.results) {
    if (chartOpen.value[c.key]) {
      h += KEY_BASE + (visibleQuotas(r.payload).length ? RING : QUOTA_LINE);
      continue;
    }
    const lines = visibleQuotas(r.payload).length + payloadLines(r.payload ?? {}).length;
    h += KEY_BASE + Math.max(1, lines) * QUOTA_LINE;
  }
  return h;
}

/** 每页容纳的卡片：累计估算高度不超实测可用高度；单卡超高时独占一页（内部滚动兜底）。 */
const pages = computed<BalanceCardView[][]>(() => {
  const avail = availHeight.value * 0.94;
  const out: BalanceCardView[][] = [];
  let cur: BalanceCardView[] = [];
  let h = 0;
  for (const c of flatCards.value) {
    const ch = estCardHeight(c);
    if (cur.length && h + ch > avail) {
      out.push(cur);
      cur = [];
      h = 0;
    }
    cur.push(c);
    h += ch;
  }
  if (cur.length) out.push(cur);
  return out;
});
const pageCount = computed(() => Math.max(1, pages.value.length));
const pagedCards = computed(() => pages.value[page.value - 1] ?? []);
const totalKeys = computed(() => flatCards.value.reduce((n, c) => n + Math.max(1, c.results.length), 0));
watch(pageCount, (n) => {
  if (page.value > n) page.value = n;
});

// ── 本地等值额度：按配额窗口汇总 usage_records 中该 provider 的实际消耗 ──
const localCosts = ref(new Map<number, Map<string, ProviderCost>>());
const costBaseSec = ref(0);

/** 配额窗口起点（unix 秒）：时长从标签推断（N 小时 / 周 / 月 / 日），末端优先取
 *  resetAt，缺省用当前时刻；起点对齐到分钟以合并各卡近似窗口。无法推断返回 null。 */
function quotaSince(q: BalanceQuota, nowSec: number): number | null {
  const label = q.label ?? "";
  let secs = 0;
  const hours = /(\d+(?:\.\d+)?)\s*小时/.exec(label);
  if (hours) secs = Number(hours[1]) * 3600;
  else if (/周|星期|week/i.test(label)) secs = 7 * 86400;
  else if (/月|month/i.test(label)) secs = 30 * 86400;
  else if (/[日天]|daily/i.test(label)) secs = 86400;
  if (!secs) return null;
  return Math.floor(((parseResetSec(q.resetAt) ?? nowSec) - secs) / 60) * 60;
}

/** resetAt 可能是 unix 秒数字串或可读时间串，两种都试解析。 */
function parseResetSec(v: string | null | undefined): number | null {
  if (!v) return null;
  if (/^\d{9,11}$/.test(v)) return Number(v);
  const t = Date.parse(v);
  return Number.isNaN(t) ? null : Math.floor(t / 1000);
}

/** 收集全部配额的窗口起点，按 provider 拉取本地消耗（去重后每个窗口一次查询）。 */
async function loadLocalCosts() {
  const now = Math.floor(Date.now() / 1000);
  costBaseSec.value = now;
  const sinces = new Set<number>();
  for (const c of flatCards.value)
    for (const r of c.results)
      for (const q of r.payload?.quotas ?? []) {
        const s = quotaSince(q, now);
        if (s !== null) sinces.add(s);
      }
  const map = new Map<number, Map<string, ProviderCost>>();
  await Promise.all(
    [...sinces].map(async (s) => {
      const rows = await usageApi.costByProvider(s).catch(() => [] as ProviderCost[]);
      map.set(s, new Map(rows.map((r) => [r.providerKey, r])));
    }),
  );
  localCosts.value = map;
}

/** 该配额窗口内此 provider 的本地消耗；配额无窗口或卡片无 provider 时返回 null。 */
function localStat(c: BalanceCardView, q: BalanceQuota): ProviderCost | null {
  if (!c.providerKey) return null;
  const s = quotaSince(q, costBaseSec.value);
  if (s === null) return null;
  return localCosts.value.get(s)?.get(c.providerKey) ?? { providerKey: c.providerKey, cost: 0, requests: 0 };
}

/** 本地消耗文案：零值压成 $0，避免 $0.0000 这类四位小数噪音。 */
function localStatText(c: BalanceCardView, q: BalanceQuota): string | null {
  const s = localStat(c, q);
  if (!s) return null;
  return `本地 ${s.cost > 0 ? formatCost(s.cost) : "$0"}`;
}

// 查询结果刷新（queriedAt 变化）后重拉本地统计；卡片列表加载完成也会触发。
watch(
  () => flatCards.value.map((c) => c.results.map((r) => r.queriedAt).join(",")).join("|"),
  () => void loadLocalCosts(),
);

// ── 加载与查询 ──

onMounted(() => store.list());
onActivated(() => store.list());

async function refreshAll() {
  error.value = null;
  try {
    await store.refreshAll();
    toast.success("已刷新全部余额卡片");
  } catch (e) {
    error.value = errMsg(e);
  }
}

async function refreshOne(key: string) {
  error.value = null;
  try {
    await store.refreshOne(key);
  } catch (e) {
    error.value = errMsg(e);
  }
}

// ── 编辑 ──

function newCard() {
  invalidatePreview();
  isNew.value = true;
  Object.assign(form, {
    key: "",
    providerKey: "",
    apiKey: "",
    baseUrl: "",
    displayMode: "auto",
    intervalSecs: 900,
    enabled: true,
    scriptRef: "",
    inlineScript: DEFAULT_SCRIPT,
    extraText: "{}",
  });
  error.value = null;
  formError.value = null;
  testResult.value = null;
  templateId.value = "";
  editing.value = true;
  void ensureProviders();
  takeSnapshot();
}

function editCard(key: string) {
  const c = store.cards.find((x) => x.key === key);
  if (!c) return;
  invalidatePreview();
  isNew.value = false;
  // scriptRef 为 .lua 路径时走文件引用；存脚本原文时填入内联编辑器
  const isPath = c.scriptRef.trim().toLowerCase().endsWith(".lua");
  Object.assign(form, {
    key: c.key,
    providerKey: c.providerKey ?? "",
    apiKey: c.apiKey,
    baseUrl: c.baseUrl,
    displayMode: c.displayMode,
    intervalSecs: c.intervalSecs,
    enabled: c.enabled,
    scriptRef: isPath ? c.scriptRef : "",
    inlineScript: isPath ? "" : c.scriptRef,
    extraText: JSON.stringify(c.extra ?? {}, null, 2),
  });
  error.value = null;
  formError.value = null;
  testResult.value = null;
  templateId.value = "";
  editing.value = true;
  void ensureProviders();
  takeSnapshot();
}

async function closeModal() {
  if (await guardClose()) editing.value = false;
}

// ── 模板填充：切换到内联脚本并填入建议默认值 ──
const templateId = ref("");

const templateOptions = BALANCE_TEMPLATES.map((t) => ({ value: t.id, label: t.label }));

const selectedTemplate = computed<BalanceTemplate | undefined>(() =>
  BALANCE_TEMPLATES.find((t) => t.id === templateId.value),
);

/** 应用模板：切换到内联来源，并填入建议表单值（未携带的字段不动）。 */
function applyTemplate(id: string) {
  templateId.value = id;
  const t = selectedTemplate.value;
  if (!t) return;
  form.scriptRef = "";
  form.inlineScript = t.script;
  if (t.baseUrl !== undefined) form.baseUrl = t.baseUrl;
  if (t.extraText !== undefined) form.extraText = t.extraText;
  if (t.intervalSecs !== undefined) form.intervalSecs = t.intervalSecs;
}

// ── 测试拉取（dry-run 预览）：用当前表单内容逐 key 试跑一次，不保存、不影响线上结果 ──
const testing = ref(false);
const testResult = ref<BalanceKeyResult[] | null>(null);
const testErrors = computed(() => (testResult.value ?? [])
  .filter((result) => result.status === "error")
  .map((result) => {
    const message = result.error || result.payload?.message || "查询失败";
    return result.keyLabel ? `${result.keyLabel}：${message}` : message;
  }));
const modalErrors = computed(() => formError.value ? [formError.value] : testErrors.value);

/** 本地校验失败时直接构造一个 error 结果进预览面板，不发请求。 */
function testLocalError(message: string) {
  testResult.value = [
    { keyIndex: 0, keyLabel: "", status: "error", payload: null, error: message, queriedAt: 0 },
  ];
}

let previewRequestId = 0;

function invalidatePreview() {
  previewRequestId += 1;
  testResult.value = null;
  testing.value = false;
}

watch([() => JSON.stringify(form), editing], invalidatePreview, { flush: "sync" });

async function runTest() {
  invalidatePreview();
  if (!editing.value) return;
  const requestId = previewRequestId;
  const input = { ...form };
  const inputSnapshot = JSON.stringify(input);
  const isCurrent = () => editing.value
    && requestId === previewRequestId
    && JSON.stringify(form) === inputSnapshot;
  formError.value = null;
  const scriptRef = input.scriptRef.trim() || input.inlineScript;
  if (!scriptRef.trim()) return testLocalError("请先填写脚本路径或内联脚本");
  const keys = manualKeys(input.apiKey);
  if (!input.providerKey.trim() && keys.length === 0) {
    return testLocalError("请填写手动 API Key 或选择上游服务");
  }
  let extra: unknown;
  try {
    extra = JSON.parse(input.extraText || "{}");
  } catch {
    return testLocalError("额外参数不是合法 JSON");
  }
  const existing = store.cards.find((c) => c.key === input.key.trim());
  testing.value = true;
  try {
    const result = await balanceApi.test({
      key: input.key.trim() || "preview",
      providerKey: input.providerKey.trim() || null,
      displayMode: input.displayMode,
      apiKey: keys.join("\n"),
      baseUrl: input.baseUrl.trim(),
      providerLabel: "",
      scriptRef,
      intervalSecs: Number(input.intervalSecs) || 0,
      enabled: input.enabled,
      extra,
      position: existing?.position ?? 0,
      createdAt: existing?.createdAt ?? 0,
      updatedAt: 0,
    });
    if (isCurrent()) testResult.value = result;
  } catch (e) {
    if (isCurrent()) testLocalError(errMsg(e));
  } finally {
    if (isCurrent()) testing.value = false;
  }
}


async function save(andQuery: boolean) {
  formError.value = null;
  testResult.value = null;
  const key = form.key.trim();
  if (!key) {
    formError.value = "卡片名（key）不能为空";
    return;
  }
  if (!form.providerKey.trim() && formManualKeys.value.length === 0) {
    formError.value = "请填写手动 API Key 或选择上游服务";
    return;
  }
  if (!form.scriptRef.trim() && !form.inlineScript.trim()) {
    formError.value = "请填写脚本路径或内联脚本";
    return;
  }
  let extra: unknown;
  try {
    extra = JSON.parse(form.extraText || "{}");
  } catch {
    formError.value = "额外参数不是合法 JSON";
    return;
  }
  const existing = store.cards.find((c) => c.key === key);
  const card: BalanceCard = {
    key,
    providerKey: form.providerKey.trim() || null,
    displayMode: form.displayMode,
    apiKey: formManualKeys.value.join("\n"),
    baseUrl: form.baseUrl.trim(),
    // 显示名字段已并入「上游服务」选择：恒为空串，界面按 providerKey 展示与分组
    providerLabel: "",
    // 内联脚本与插件记录同一约定：scriptRef 直接存脚本原文
    scriptRef: form.scriptRef.trim() || form.inlineScript,
    intervalSecs: Number(form.intervalSecs) || 0,
    enabled: form.enabled,
    extra,
    position: existing?.position ?? store.cards.length,
    createdAt: existing?.createdAt ?? Math.floor(Date.now() / 1000),
    updatedAt: Math.floor(Date.now() / 1000),
  };
  busy.value = true;
  try {
    await store.save(card);
    editing.value = false;
    toast.success(isNew.value ? `余额卡片 “${key}” 已创建` : `余额卡片 “${key}” 已更新`);
    if (andQuery) await refreshOne(key);
  } catch (e) {
    formError.value = errMsg(e);
  } finally {
    busy.value = false;
  }
}

async function remove(key: string) {
  if (!(await confirm({ title: "删除余额卡片", message: `确认删除卡片 “${key}”？` }))) return;
  error.value = null;
  try {
    await store.remove(key);
    toast.success(`余额卡片 “${key}” 已删除`);
  } catch (e) {
    error.value = errMsg(e);
  }
}
</script>

<template>
  <div class="flex h-full min-h-0 flex-col gap-4">
    <Alert v-if="error || store.error" class="shrink-0">
      <p>{{ error || store.error }}</p>
      <Button v-if="store.error" class="mt-2" variant="outline" size="sm" :disabled="store.loading" @click="error = null; store.list()">
        {{ store.loading ? "加载中…" : "重试加载" }}
      </Button>
    </Alert>

    <!-- 页头操作 -->
    <div class="flex shrink-0 items-center justify-end gap-2">
      <Button variant="ghost" size="icon" class="size-8" title="脚本编写指南" @click="guideOpen = true">
        <HelpCircle class="size-4" />
      </Button>
      <Button variant="outline" size="sm" :disabled="store.refreshingAll" @click="refreshAll">
        <RefreshCw class="size-4" :class="store.refreshingAll ? 'animate-spin' : ''" />
        一键刷新
      </Button>
      <Button size="sm" @click="newCard"><Plus class="size-4" /> 新增卡片</Button>
    </div>

    <EmptyState v-if="store.loading && store.cards.length === 0">加载中…</EmptyState>

    <!-- 空态：引导 + 示例脚本 -->
    <EmptyState
      v-else-if="store.loaded && !store.error && store.cards.length === 0"
      :icon="Wallet"
    >
      <p>还没有余额卡片。手动填写 API Key 或引用上游服务，用 Lua 脚本查询额度并按间隔自动刷新。</p>
      <div class="mt-4">
        <Button size="sm" @click="newCard">
          <Plus class="size-4" /> 使用示例脚本新建
        </Button>
      </div>
      <BalanceScriptGuide class="mt-4" />
    </EmptyState>

    <!-- 余额表：卡片为组行，key 为数据行；配额内联细进度条，组行展开切环形图 -->
    <div v-else-if="store.cards.length > 0" class="flex min-h-0 flex-1 flex-col">
      <section ref="tableScroll" class="scrollbar-thin min-h-0 flex-1 overflow-auto">
      <table class="w-full text-center text-sm">
        <thead class="thead-sticky">
          <tr class="border-b text-muted-foreground">
            <th class="py-2 font-medium">Key</th>
            <th class="py-2 font-medium">配额</th>
            <th class="py-2 font-medium">状态</th>
            <th class="py-2 font-medium">上次查询</th>
            <th class="py-2 font-medium">操作</th>
          </tr>
        </thead>
        <tbody>
          <template v-for="c in pagedCards" :key="c.key">
            <!-- 卡片组行：元信息与卡级操作对齐到列位 -->
            <tr class="border-b bg-muted/30">
              <td class="py-1.5">
                <div class="flex min-w-0 items-center justify-center gap-1.5">
                  <button
                    type="button"
                    class="shrink-0 rounded-sm p-0.5 text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
                    :title="chartOpen[c.key] ? '收起环形图' : '展开环形图'"
                    @click="toggleChart(c.key)"
                  >
                    <ChevronDown
                      class="size-3.5 transition-transform"
                      :class="chartOpen[c.key] ? '' : '-rotate-90'"
                    />
                  </button>
                  <span class="font-mono text-xs font-medium" :title="`${c.key} · ${cardBindingText(c)}`">{{ c.key }}</span>
                  <span class="truncate text-xs text-muted-foreground">{{ cardBindingText(c) }}</span>
                </div>
              </td>
              <td class="py-1.5 text-xs text-muted-foreground">
                {{ keyCount(c) ? `${keyCount(c)} 个 key · ` : "" }}{{ DISPLAY_MODE_LABEL[c.displayMode] }}
              </td>
              <td class="py-1.5"><Badge v-if="!c.enabled" variant="secondary">停用</Badge></td>
              <td class="py-1.5 text-xs tabular-nums text-muted-foreground">{{ shortTime(cardLastQuery(c)) }}</td>
              <td class="py-1.5">
                <div class="flex items-center justify-center gap-0.5">
                  <Button
                    variant="ghost"
                    size="icon"
                    class="size-7"
                    title="查询整卡"
                    :disabled="store.refreshingAll || store.refreshingKeys.has(c.key)"
                    @click="refreshOne(c.key)"
                  >
                    <RefreshCw class="size-3.5" :class="store.refreshingKeys.has(c.key) ? 'animate-spin' : ''" />
                  </Button>
                  <Button variant="ghost" size="icon" class="size-7" title="编辑卡片" @click="editCard(c.key)">
                    <Pencil class="size-3.5" />
                  </Button>
                  <Button variant="ghost" size="icon" class="size-7" title="删除卡片" @click="remove(c.key)">
                    <Trash2 class="size-3.5 text-destructive" />
                  </Button>
                </div>
              </td>
            </tr>
            <!-- 未查询占位行 -->
            <tr v-if="c.results.length === 0" class="border-b">
              <td class="py-2 font-mono text-xs text-muted-foreground">—</td>
              <td class="py-2 text-xs text-muted-foreground">尚未查询</td>
              <td></td>
              <td></td>
              <td></td>
            </tr>
            <!-- key 数据行 -->
            <tr v-for="result in c.results" :key="result.keyIndex" class="border-b transition-colors hover:bg-accent/40">
              <td class="py-2 font-mono text-xs" :title="`API Key ${result.keyLabel}（掩码）`">{{ result.keyLabel || `Key ${result.keyIndex + 1}` }}</td>
              <td class="py-2">
                <!-- 收起：配额行内细进度条 + 文本；展开：环形图 -->
                <div v-if="!chartOpen[c.key]" class="flex flex-col items-center gap-0.5">
                  <div class="grid grid-cols-[4rem_3.5rem_auto_5.5rem] items-center gap-x-1.5 gap-y-0.5 text-xs">
                    <template v-for="(q, i) in visibleQuotas(result.payload)" :key="`q${i}`">
                      <span class="truncate text-right text-muted-foreground" :title="`${q.label}：${quotaDetailText(c.displayMode, q)}`">{{ q.label }}</span>
                      <span
                        v-if="quotaRingPercent(c.displayMode, q) !== null"
                        class="inline-block h-1.5 w-14 overflow-hidden rounded-full bg-muted"
                        :title="`${q.label}：${quotaDetailText(c.displayMode, q)}`"
                      >
                        <span class="block h-full bg-primary" :style="{ width: quotaRingPercent(c.displayMode, q) + '%' }" />
                      </span>
                      <span v-else />
                      <span class="tabular-nums" :title="`${q.label}：${quotaDetailText(c.displayMode, q)}`">{{ quotaText(q, false) }}</span>
                      <span
                        v-if="localStat(c, q)"
                        class="truncate text-right text-muted-foreground tabular-nums"
                        :title="`本地统计：跟随配额窗口的实际消耗 · ${localStat(c, q)!.requests} 次请求`"
                      >{{ localStatText(c, q) }}</span>
                      <span v-else />
                    </template>
                    <template v-for="(line, i) in payloadLines(result.payload ?? {})" :key="`p${i}`">
                      <span class="truncate text-right text-muted-foreground">{{ line.label }}</span>
                      <span class="col-span-3 tabular-nums">{{ line.value }}</span>
                    </template>
                  </div>
                  <div v-if="result.status === 'error'" class="text-xs text-destructive">{{ result.error || result.payload?.message || "查询失败" }}</div>
                  <div
                    v-if="result.status === 'ok' && !visibleQuotas(result.payload).length && !payloadLines(result.payload ?? {}).length"
                    class="text-xs text-muted-foreground"
                  >—</div>
                </div>
                <div v-else-if="visibleQuotas(result.payload).length" class="flex flex-wrap items-start justify-center gap-x-3 gap-y-2">
                  <template v-for="(q, i) in visibleQuotas(result.payload)" :key="i">
                    <div
                      v-if="quotaRingPercent(c.displayMode, q) !== null"
                      class="flex w-20 shrink-0 flex-col items-center gap-1 text-center"
                      :title="`${q.label}：${quotaDetailText(c.displayMode, q)}`"
                    >
                      <div class="relative size-11 shrink-0">
                        <svg viewBox="0 0 36 36" class="size-full -rotate-90">
                          <circle cx="18" cy="18" r="15.5" fill="none" stroke-width="3" class="stroke-muted" />
                          <circle
                            cx="18" cy="18" r="15.5" fill="none" stroke-width="3" stroke-linecap="round"
                            class="stroke-primary transition-[stroke-dashoffset] duration-500"
                            :stroke-dasharray="RING_C"
                            :stroke-dashoffset="RING_C * (1 - (quotaRingPercent(c.displayMode, q) ?? 0) / 100)"
                          />
                        </svg>
                        <div class="absolute inset-0 flex items-center justify-center text-[10px] font-semibold tabular-nums">
                          {{ (quotaRingPercent(c.displayMode, q) ?? 0).toFixed(0) }}%
                        </div>
                      </div>
                      <div class="w-full truncate text-[11px]">{{ q.label }}</div>
                      <div v-if="localStat(c, q)" class="w-full truncate text-[10px] text-muted-foreground tabular-nums">
                        {{ localStatText(c, q) }}
                      </div>
                    </div>
                    <div
                      v-else
                      class="flex min-h-16 w-20 shrink-0 items-center justify-center text-center"
                      :title="`${q.label}：${quotaDetailText(c.displayMode, q)}`"
                    >
                      <div class="w-full truncate text-[11px] text-muted-foreground">{{ q.label }}</div>
                    </div>
                  </template>
                </div>
                <div v-else class="text-xs text-muted-foreground">—</div>
              </td>
              <td class="py-2">
                <Badge :variant="result.status === 'ok' ? 'success' : 'destructive'">{{ result.status === "ok" ? "正常" : "异常" }}</Badge>
              </td>
              <td class="py-2 text-xs tabular-nums text-muted-foreground" :title="formatTime(result.queriedAt)">{{ shortTime(result.queriedAt) }}</td>
              <td class="py-2" />
            </tr>
          </template>
        </tbody>
      </table>
      </section>
      <div v-if="pageCount > 1" class="shrink-0 border-t pt-3">
        <Pagination v-model:page="page" :page-count="pageCount" :total="totalKeys" />
      </div>
    </div>

    <!-- 新增 / 编辑弹窗：单列纵向布局，只上下滚动 -->
    <Modal
      :open="editing"
      :title="isNew ? '新增余额卡片' : '编辑余额卡片'"
      width="max-w-3xl"
      :guard="guardClose"
      @close="editing = false"
    >
      <template v-if="modalErrors.length" #notice>
        <Alert
          aria-live="assertive"
          class="scrollbar-thin mx-5 mt-3 max-h-28 overflow-y-auto"
        >
          <p v-for="(message, index) in modalErrors" :key="index" class="whitespace-pre-wrap break-words">{{ message }}</p>
        </Alert>
      </template>
      <div class="grid gap-4">
        <div class="space-y-1.5 rounded-md border border-dashed p-3">
          <Label>应用模板（覆盖脚本并切换到内联来源）</Label>
          <Select
            :model-value="templateId"
            :options="templateOptions"
            placeholder="选择常用服务的查询模板，也可直接手写"
            @update:model-value="applyTemplate"
          />
          <p v-if="selectedTemplate" class="text-xs leading-relaxed text-muted-foreground">
            {{ selectedTemplate.description }}
          </p>
        </div>

        <div class="space-y-1.5">
          <Label for="bc-key">卡片名</Label>
          <Input
            id="bc-key"
            v-model="form.key"
            placeholder="如 anthropic-official"
            :disabled="!isNew"
          />
        </div>

        <div class="space-y-1.5">
          <Label>上游服务（可选）</Label>
          <Select v-model="form.providerKey" :options="providerOptions" placeholder="请选择上游服务" />
          <p v-if="selectedProviderSummary && formManualKeys.length === 0" class="text-xs text-muted-foreground">
            {{ selectedProviderSummary }}
          </p>
          <p v-else-if="providers.length === 0" class="text-xs text-muted-foreground">
            还没有上游服务，可直接填写下方 API Key。
          </p>
        </div>

        <div class="space-y-1.5">
          <Label for="bc-api-keys">手动 API Key（可选，一行一个）</Label>
          <textarea
            id="bc-api-keys"
            v-model="form.apiKey"
            rows="3"
            autocomplete="off"
            autocapitalize="off"
            :spellcheck="false"
            placeholder="填写一个 Key，或换行输入 Key 列表"
            class="flex w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          ></textarea>
          <p v-if="formManualKeys.length > 0" class="text-xs text-muted-foreground">
            当前使用 {{ formManualKeys.length }} 个手动 Key；清空后恢复上游服务引用。
          </p>
        </div>

        <div class="space-y-1.5">
          <Label for="bc-url">查询 URL（可选）</Label>
          <Input id="bc-url" v-model="form.baseUrl" placeholder="如 https://api.example.com" />
        </div>

        <div class="space-y-1.5">
          <Label>显示样式</Label>
          <SegmentedControl
            :model-value="form.displayMode"
            :options="DISPLAY_MODES"
            fill
            @update:model-value="(v) => (form.displayMode = v as DisplayMode)"
          />
        </div>

        <div class="space-y-1.5">
          <Label for="bc-interval">查询间隔（秒）</Label>
          <Input id="bc-interval" v-model="form.intervalSecs" type="number" min="0" />
        </div>

        <div class="space-y-1.5">
          <Label>启用</Label>
          <div class="pt-1">
            <Switch :checked="form.enabled" @update:checked="(v: boolean) => (form.enabled = v)" />
          </div>
        </div>

        <div class="space-y-1.5">
          <Label for="bc-script">脚本文件路径（可选）</Label>
          <Input id="bc-script" v-model="form.scriptRef" placeholder="如 balance_openai.lua" />
        </div>

        <BalanceScriptGuide />

        <div v-if="!form.scriptRef.trim()" class="flex min-h-0 flex-col space-y-1.5">
          <Label>内联脚本（必填）</Label>
          <CodeEditor v-model="form.inlineScript" height="14rem" />
        </div>
        <div class="flex min-h-0 flex-col space-y-1.5">
          <Label>额外参数（JSON）</Label>
          <CodeEditor v-model="form.extraText" lang="json" height="8rem" />
        </div>

        <!-- 测试拉取：用当前表单内容 dry-run 一次，不写库、不影响线上结果 -->
        <div class="space-y-2 rounded-md border p-3">
          <div class="flex items-center gap-2">
            <Button variant="outline" size="sm" :disabled="testing" @click="runTest">
              <RefreshCw class="size-3.5" :class="testing ? 'animate-spin' : ''" />
              {{ testing ? "查询中…" : "测试拉取" }}
            </Button>
            <span class="text-xs text-muted-foreground">
              用当前表单内容试跑一次，不保存、不影响线上结果
            </span>
          </div>
          <div v-if="testResult" class="space-y-3 border-t pt-2">
            <div
              v-for="r in testResult"
              :key="r.keyIndex"
              class="space-y-1.5"
            >
              <div class="flex items-center gap-2">
                <Badge
                  v-if="r.keyLabel"
                  variant="secondary"
                  class="max-w-[10rem] truncate font-mono"
                  :title="`API Key ${r.keyLabel}（掩码）`"
                >
                  {{ r.keyLabel }}
                </Badge>
                <Badge :variant="r.status === 'ok' ? 'success' : 'destructive'">
                  {{ r.status === "ok" ? "成功" : "失败" }}
                </Badge>
                <span v-if="r.queriedAt" class="text-xs text-muted-foreground">
                  {{ formatTime(r.queriedAt) }}
                </span>
              </div>
              <div v-if="r.payload?.quotas?.length" class="space-y-0.5">
                <div v-for="(q, i) in r.payload.quotas" :key="i" class="text-xs">
                  {{ q.label }}：{{ quotaText(q) }}
                </div>
              </div>
              <div v-if="r.payload && payloadLines(r.payload).length" class="space-y-0.5">
                <div v-for="(line, i) in payloadLines(r.payload)" :key="i" class="text-xs">
                  <span class="text-muted-foreground">{{ line.label }}</span>
                  {{ line.value }}
                </div>
              </div>
            </div>
          </div>
        </div>
      </div>
      <template #footer>
        <Button variant="ghost" size="sm" @click="closeModal">取消</Button>
        <Button variant="outline" size="sm" :disabled="busy" @click="save(true)">
          保存并立即查询
        </Button>
        <Button size="sm" :disabled="busy" @click="save(false)">
          {{ busy ? "保存中…" : "保存" }}
        </Button>
      </template>
    </Modal>

    <!-- 脚本编写指南：页头问号按钮弹出 -->
    <Modal :open="guideOpen" title="脚本编写指南" @close="guideOpen = false">
      <BalanceScriptGuide bare :open="true" />
    </Modal>
  </div>
</template>
