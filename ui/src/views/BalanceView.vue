<script setup lang="ts">
import { Pencil, Plus, RefreshCw, Trash2 } from "lucide-vue-next";
import { computed, onActivated, onMounted, reactive, ref, watch } from "vue";

import BalanceScriptGuide from "@/components/balance/BalanceScriptGuide.vue";
import Badge from "@/components/ui/Badge.vue";
import Button from "@/components/ui/Button.vue";
import Card from "@/components/ui/Card.vue";
import CodeEditor from "@/components/ui/CodeEditor.vue";
import Input from "@/components/ui/Input.vue";
import Label from "@/components/ui/Label.vue";
import Modal from "@/components/ui/Modal.vue";
import Select from "@/components/ui/Select.vue";
import Switch from "@/components/ui/Switch.vue";
import { useConfirm } from "@/composables/useConfirm";
import { useToast } from "@/composables/useToast";
import { errMsg, balanceApi, providerApi, type BalanceCard, type BalanceCardView, type BalanceKeyResult, type BalanceQuota, type DisplayMode, type Provider } from "@/lib/api";
import { BALANCE_TEMPLATES, DEFAULT_BALANCE_SCRIPT, type BalanceTemplate } from "@/lib/balanceTemplates";
import { cn, formatTime } from "@/lib/utils";
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

function quotaUsed(q: BalanceQuota): number {
  if (typeof q.usedPercent === "number") return q.usedPercent;
  if (typeof q.leftPercent === "number") return 100 - q.leftPercent;
  return 0;
}

function quotaLeft(q: BalanceQuota): number {
  if (typeof q.leftPercent === "number") return q.leftPercent;
  if (typeof q.usedPercent === "number") return 100 - q.usedPercent;
  return 0;
}

/** 卡片是否有百分比口径的配额数据（强制百分比模式据此决定「—」）。 */
function hasPercent(q: BalanceQuota): boolean {
  return typeof q.usedPercent === "number" || typeof q.leftPercent === "number";
}

/** 自动模式识别任一金额字段，包括零余额。 */
function isAmountMode(mode: DisplayMode, q: BalanceQuota): boolean {
  if (mode === "amount") return true;
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

/** 百分比文案「已用 x% · 剩余 y%」；两个百分比都缺时返回 null。 */
function percentLine(q: BalanceQuota): string | null {
  return hasPercent(q)
    ? `${quotaUsed(q).toFixed(0)}% · 剩余 ${quotaLeft(q).toFixed(0)}%`
    : null;
}

/** 配额行文案：缺该口径所需字段时显示「—」。 */
function quotaLine(mode: DisplayMode, q: BalanceQuota): string {
  return (isAmountMode(mode, q) ? amountLine(q) : percentLine(q)) ?? "—";
}

/** 进度条比例：缺该口径所需字段时返回 null（不渲染进度条）。 */
function quotaBarPercent(mode: DisplayMode, q: BalanceQuota): number | null {
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

/** 间隔文案：0 表示关闭定时。 */
function intervalText(secs: number): string {
  if (!secs) return "定时关闭";
  if (secs < 3600) return `每 ${Math.round(secs / 60)} 分钟`;
  return `每 ${(secs / 3600).toFixed(secs % 3600 === 0 ? 0 : 1)} 小时`;
}

/** 重置时间文案：纯数字串视为 unix 秒（契约允许脚本给数字，沙箱无 os 库无法自行
 *  格式化）转为本地时间；其余字符串原样展示。 */
function resetText(q: BalanceQuota): string {
  const v = q.resetAt;
  if (v === null || v === undefined || v === "") return "";
  return /^\d{9,11}$/.test(v) ? formatTime(Number(v)) : v;
}

/** 无 quotas 时的回退展示：载荷去掉空字段后的 pretty JSON。 */
function payloadJson(payload: Record<string, unknown>): string {
  const rest: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(payload)) {
    if (v === null || v === undefined) continue;
    rest[k] = v;
  }
  return JSON.stringify(rest, null, 2);
}

/** 载荷除空字段外是否还有内容（决定要不要折叠展示 JSON）。 */
function hasPayloadBody(payload: Record<string, unknown>): boolean {
  return Object.values(payload).some((v) => v !== null && v !== undefined);
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

/** 卡片副标题只展示 Key 来源，不展示密钥原文。 */
function cardBindingText(c: BalanceCardView): string {
  const count = manualKeys(c.apiKey).length;
  const target = count > 0
    ? `${c.providerKey ? `${c.providerKey} · ` : ""}手动 Key（${count} 个）`
    : c.providerKey || "未配置 Key";
  const url = (c.baseUrl ?? "").trim();
  return url ? `${target} · ${url}` : target;
}

/** 单 key 刷新的旋转标记（与 store 的粒度口径一致）。 */
function refreshMark(c: BalanceCardView, r: BalanceKeyResult): string {
  return `${c.key}#${r.keyIndex}`;
}

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

async function refreshOne(key: string, keyIndex?: number) {
  error.value = null;
  try {
    await store.refreshOne(key, keyIndex);
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

/** 卡片上的快捷切换：乐观改显示样式并落库，失败回滚（不加 toast）。 */
async function setDisplayMode(c: BalanceCardView, mode: DisplayMode) {
  if (c.displayMode === mode) return;
  const prev = c.displayMode;
  c.displayMode = mode;
  error.value = null;
  try {
    await store.save(toCardInput(c));
  } catch (e) {
    c.displayMode = prev;
    error.value = errMsg(e);
  }
}

/** 视图 → 保存入参：只带卡片配置字段，查询结果不进 payload。 */
function toCardInput(c: BalanceCardView): BalanceCard {
  return {
    key: c.key,
    providerKey: c.providerKey,
    displayMode: c.displayMode,
    apiKey: c.apiKey,
    baseUrl: c.baseUrl,
    providerLabel: c.providerLabel,
    scriptRef: c.scriptRef,
    intervalSecs: c.intervalSecs,
    enabled: c.enabled,
    extra: c.extra,
    position: c.position,
    createdAt: c.createdAt,
    updatedAt: c.updatedAt,
  };
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
    <div
      v-if="error || store.error"
      role="alert"
      class="shrink-0 rounded-md border border-destructive/40 bg-destructive/10 px-4 py-2 text-sm text-destructive"
    >
      <p>{{ error || store.error }}</p>
      <Button v-if="store.error" class="mt-2" variant="outline" size="sm" :disabled="store.loading" @click="error = null; store.list()">
        {{ store.loading ? "加载中…" : "重试加载" }}
      </Button>
    </div>

    <!-- 页头操作 -->
    <div class="flex shrink-0 items-center justify-end gap-2">
      <Button variant="outline" size="sm" :disabled="store.refreshingAll" @click="refreshAll">
        <RefreshCw class="size-4" :class="store.refreshingAll ? 'animate-spin' : ''" />
        一键刷新
      </Button>
      <Button size="sm" @click="newCard"><Plus class="size-4" /> 新增卡片</Button>
    </div>

    <div
      v-if="store.loading && store.cards.length === 0"
      class="rounded-md border border-dashed p-8 text-center text-sm text-muted-foreground"
    >
      加载中…
    </div>

    <!-- 空态：引导 + 示例脚本 -->
    <div
      v-else-if="store.loaded && !store.error && store.cards.length === 0"
      class="rounded-md border border-dashed p-8 text-center text-sm text-muted-foreground"
    >
      <p>还没有余额卡片。手动填写 API Key 或引用上游服务，用 Lua 脚本查询额度并按间隔自动刷新。</p>
      <Button class="mt-4" size="sm" @click="newCard">
        <Plus class="size-4" /> 使用示例脚本新建
      </Button>
      <BalanceScriptGuide class="mt-4" />
    </div>

    <!-- 卡片：按上游服务 key 分组（旧卡遗留展示名兜底） -->
    <section v-else-if="store.cards.length > 0" class="space-y-5">
      <BalanceScriptGuide />
      <div v-for="g in groups" :key="g.label">
        <div class="mb-2 flex items-baseline gap-2">
          <h3 class="text-sm font-medium">{{ g.label }}</h3>
          <span class="text-xs text-muted-foreground">
            · {{ g.cards.length }} 张卡片
          </span>
        </div>
        <div class="grid gap-4 grid-cols-[repeat(auto-fill,minmax(21rem,1fr))]">
          <template v-for="c in g.cards" :key="c.key">
            <Card class="flex min-w-0 flex-col">
              <div class="card-header space-y-2">
                <div class="flex items-center justify-between gap-2">
                  <Badge variant="outline" class="max-w-[12rem] truncate font-mono" :title="c.key">{{ c.key }}</Badge>
                  <div class="flex items-center gap-1">
                    <Button variant="ghost" size="icon" class="size-7" title="编辑卡片" @click="editCard(c.key)">
                      <Pencil class="size-3.5" />
                    </Button>
                    <Button variant="ghost" size="icon" class="size-7" title="删除卡片" @click="remove(c.key)">
                      <Trash2 class="size-3.5 text-destructive" />
                    </Button>
                  </div>
                </div>
                <div class="truncate text-xs text-muted-foreground" :title="cardBindingText(c)">{{ cardBindingText(c) }}</div>
                <div class="flex items-center justify-between gap-2">
                  <div class="flex items-center rounded-md border p-0.5">
                    <button
                      v-for="m in DISPLAY_MODES"
                      :key="m.value"
                      type="button"
                      :title="`显示样式：${m.label}`"
                      :class="cn('rounded px-1.5 py-0.5 text-[11px] transition-colors', c.displayMode === m.value ? 'bg-accent text-accent-foreground' : 'text-muted-foreground hover:bg-accent/60')"
                      @click="setDisplayMode(c, m.value)"
                    >{{ m.label }}</button>
                  </div>
                  <span class="text-[11px] text-muted-foreground">{{ intervalText(c.intervalSecs) }}</span>
                  <Badge v-if="!c.enabled" variant="secondary">停用</Badge>
                </div>
              </div>
              <div class="card-content divide-y">
                <div v-if="c.results.length === 0" class="flex items-center justify-between py-3">
                  <span class="text-xs text-muted-foreground">尚未查询</span>
                  <Button variant="ghost" size="icon" class="size-7" title="查询卡片" :disabled="store.refreshingAll || store.refreshingKeys.has(c.key)" @click="refreshOne(c.key)">
                    <RefreshCw class="size-3.5" :class="store.refreshingKeys.has(c.key) ? 'animate-spin' : ''" />
                  </Button>
                </div>
                <div v-for="result in c.results" :key="result.keyIndex" class="space-y-2 py-3">
                  <div class="flex items-center justify-between gap-2">
                    <Badge variant="secondary" class="max-w-[10rem] truncate font-mono" :title="`API Key ${result.keyLabel}（掩码）`">{{ result.keyLabel || `Key ${result.keyIndex + 1}` }}</Badge>
                    <div class="flex items-center gap-2">
                      <Badge :variant="result.status === 'ok' ? 'success' : 'destructive'">{{ result.status === "ok" ? "正常" : "异常" }}</Badge>
                      <Button
                        variant="ghost"
                        size="icon"
                        class="size-7"
                        title="立即查询该 key"
                        :disabled="store.refreshingAll || store.refreshingKeys.has(c.key) || store.refreshingKeys.has(refreshMark(c, result))"
                        @click="refreshOne(c.key, result.keyIndex)"
                      >
                        <RefreshCw class="size-3.5" :class="store.refreshingKeys.has(refreshMark(c, result)) ? 'animate-spin' : ''" />
                      </Button>
                    </div>
                  </div>
                  <div v-if="result.payload?.quotas?.length" class="space-y-2 pt-1">
                    <div v-for="(q, i) in result.payload.quotas" :key="i">
                      <div class="flex items-center justify-between text-xs">
                        <span class="truncate">{{ q.label }}</span>
                        <span class="shrink-0 text-muted-foreground tabular-nums">{{ quotaLine(c.displayMode, q) }}</span>
                      </div>
                      <div v-if="quotaBarPercent(c.displayMode, q) !== null" class="mt-1 h-2 w-full overflow-hidden rounded-full bg-muted">
                        <div class="h-full rounded-full bg-primary" :style="{ width: (quotaBarPercent(c.displayMode, q) ?? 0) + '%' }"></div>
                      </div>
                      <div v-if="resetText(q)" class="mt-0.5 text-[11px] text-muted-foreground">重置 {{ resetText(q) }}</div>
                    </div>
                  </div>
                  <details v-else-if="result.payload && hasPayloadBody(result.payload)" class="pt-1">
                    <summary class="cursor-pointer text-xs text-muted-foreground">查看返回内容</summary>
                    <pre class="scrollbar-thin mt-1 max-h-40 overflow-auto rounded border bg-muted/40 p-2 font-mono text-[11px]">{{ payloadJson(result.payload) }}</pre>
                  </details>
                  <div v-if="result.status === 'error'" class="text-xs text-destructive">{{ result.error || result.payload?.message || "查询失败" }}</div>
                  <div v-if="result.payload?.summary" class="text-xs text-muted-foreground">{{ result.payload.summary }}</div>
                  <div class="text-[11px] text-muted-foreground">上次查询 {{ formatTime(result.queriedAt) }}</div>
                </div>
              </div>
            </Card>
          </template>
        </div>
      </div>
    </section>

    <!-- 新增 / 编辑弹窗：单列纵向布局，只上下滚动 -->
    <Modal
      :open="editing"
      :title="isNew ? '新增余额卡片' : '编辑余额卡片'"
      width="max-w-3xl"
      :guard="guardClose"
      @close="editing = false"
    >
      <template v-if="modalErrors.length" #notice>
        <div
          role="alert"
          aria-live="assertive"
          class="scrollbar-thin mx-5 mt-3 max-h-28 overflow-y-auto rounded-md border border-destructive/40 bg-destructive/10 px-4 py-2 text-sm text-destructive"
        >
          <p v-for="(message, index) in modalErrors" :key="index" class="whitespace-pre-wrap break-words">{{ message }}</p>
        </div>
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
          <p class="text-xs text-muted-foreground">卡片的唯一标识，同时作为脚本 ctx.name</p>
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
          <p class="text-xs text-muted-foreground">
            手动 Key 为空时，使用该服务端点的 Key（去重保序，留空继承前一个非空 Key）；否则仅用于分组，不引用服务的 Key。
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
          <p class="text-xs text-muted-foreground">
            手动填写后只使用这些 Key，不混入上游服务的 Key；忽略空行和重复值，每个 Key 单独查询。
          </p>
          <p v-if="formManualKeys.length > 0" class="text-xs text-muted-foreground">
            当前使用 {{ formManualKeys.length }} 个手动 Key；清空后恢复上游服务引用。
          </p>
        </div>

        <div class="space-y-1.5">
          <Label for="bc-url">查询 URL（可选）</Label>
          <Input id="bc-url" v-model="form.baseUrl" placeholder="如 https://api.example.com" />
          <p class="text-xs text-muted-foreground">
            配额接口的基准地址，脚本里用 ctx.base_url 读取；留空则脚本需自己处理 URL
          </p>
        </div>

        <div class="space-y-1.5">
          <Label>显示样式</Label>
          <div class="flex items-center rounded-md border p-0.5">
            <button
              v-for="m in DISPLAY_MODES"
              :key="m.value"
              type="button"
              :class="
                cn(
                  'flex-1 rounded px-2 py-1 text-xs transition-colors',
                  form.displayMode === m.value
                    ? 'bg-accent text-accent-foreground'
                    : 'text-muted-foreground hover:bg-accent/60',
                )
              "
              @click="form.displayMode = m.value"
            >
              {{ m.label }}
            </button>
          </div>
          <p class="text-xs text-muted-foreground">自动 = 按脚本返回的字段判断；强制口径缺字段时显示「—」</p>
        </div>

        <div class="space-y-1.5">
          <Label for="bc-interval">查询间隔（秒）</Label>
          <Input id="bc-interval" v-model="form.intervalSecs" type="number" min="0" />
          <p class="text-xs text-muted-foreground">0 = 关闭定时；小于 60 秒会按 60 秒计</p>
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
          <p class="text-xs text-muted-foreground">
            当前来源：{{ form.scriptRef.trim() ? "文件引用（插件目录内的 .lua 文件）" : "内联脚本" }}。
            清空路径后必须填写内联脚本，不会自动套用默认脚本。
          </p>
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
              <pre
                v-if="r.payload"
                class="scrollbar-thin max-h-48 overflow-auto rounded border bg-muted/40 p-2 font-mono text-[11px]"
              >{{ JSON.stringify(r.payload, null, 2) }}</pre>
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
  </div>
</template>
