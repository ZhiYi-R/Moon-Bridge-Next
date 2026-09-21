<script setup lang="ts">
import { ChevronDown, RefreshCw, Wallet } from "lucide-vue-next";
import { computed, onActivated, onMounted, ref, watch } from "vue";

import Alert from "@/components/ui/Alert.vue";
import Badge from "@/components/ui/Badge.vue";
import Button from "@/components/ui/Button.vue";
import EmptyState from "@/components/ui/EmptyState.vue";
import Pagination from "@/components/ui/Pagination.vue";
import { useAutoPageSize } from "@/composables/useAutoPageSize";
import { useToast } from "@/composables/useToast";
import { errMsg, usageApi, type ProviderCost, type ProviderQuotaView, type QuotaEntry, type QuotaPayload } from "@/lib/api";
import { formatCost, formatTime } from "@/lib/utils";
import { useQuotaStore } from "@/stores/quota";

const store = useQuotaStore();
const toast = useToast();

const error = ref<string | null>(null);

/** 配额的显示状态：收起为文字，展开为环形图（默认收起）。 */
const chartOpen = ref<Record<string, boolean>>({});

function toggleChart(key: string) {
  chartOpen.value[key] = !chartOpen.value[key];
}

// ── 展示辅助 ──

function quotaLeft(q: QuotaEntry): number {
  if (typeof q.leftPercent === "number") return q.leftPercent;
  if (typeof q.usedPercent === "number") return 100 - q.usedPercent;
  return 0;
}

/** 悬停详情行：percentage 补剩余（已用比在环心）；quota/counter 逐项列消耗与余额。 */
function quotaDetailLines(q: QuotaEntry): string[] {
  const unit = q.unit ?? "";
  if (q.type === "percentage") return [`剩余 ${quotaLeft(q).toFixed(0)}%`];
  const parts: string[] = [];
  if (typeof q.usedAmount === "number") parts.push(`消耗 ${q.usedAmount}${unit}`);
  if (q.type === "quota" && typeof q.leftAmount === "number") parts.push(`余额 ${q.leftAmount}${unit}`);
  return parts;
}

/** 悬停完整提示：详情行 + 重置时间，一行串起。 */
function quotaDetailText(q: QuotaEntry): string {
  const lines = quotaDetailLines(q);
  const reset = resetText(q);
  if (reset) lines.push(`重置 ${reset}`);
  return lines.join(" · ") || "—";
}

const RING_C = 2 * Math.PI * 15.5;

/** 环形图比例：percentage 取 percent；quota 由 used/(used+left) 算；缺数据返回 null（不渲染）。 */
function quotaRingPercent(q: QuotaEntry): number | null {
  const clamp = (v: number) => Math.min(100, Math.max(0, v));
  if (q.type === "percentage") {
    if (typeof q.usedPercent === "number") return clamp(q.usedPercent);
    if (typeof q.leftPercent === "number") return clamp(100 - q.leftPercent);
    return null;
  }
  if (q.type !== "quota") return null;
  if (typeof q.usedAmount !== "number" || typeof q.leftAmount !== "number") return null;
  const total = q.usedAmount + q.leftAmount;
  return total > 0 ? clamp((q.usedAmount / total) * 100) : null;
}

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
function resetText(q: QuotaEntry): string {
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

/** 表格实际展示的配额行：counter 是无界计数（如请求数），不是配额，不展示。 */
function visibleQuotas(payload: QuotaPayload | null | undefined): QuotaEntry[] {
  return (payload?.quotas ?? []).filter((q) => q.type !== "counter");
}

/** 配额的一行文字：percentage 给百分比，quota/counter 给金额与余额，再合重置时间；
 *  缩略场景传 withReset=false（重置时间仍在 title）。 */
function quotaText(q: QuotaEntry, withReset = true): string {
  const parts: string[] = [];
  if (q.type === "percentage") {
    if (typeof q.usedPercent === "number") parts.push(`已用 ${q.usedPercent.toFixed(0)}%`);
    else parts.push(`剩余 ${quotaLeft(q).toFixed(0)}%`);
  } else {
    if (typeof q.usedAmount === "number") parts.push(`消耗 ${q.usedAmount}${q.unit ?? ""}`);
    if (q.type === "quota" && typeof q.leftAmount === "number") parts.push(`余额 ${q.leftAmount}${q.unit ?? ""}`);
  }
  if (withReset) {
    const reset = resetText(q);
    if (reset) parts.push(`重置 ${reset}`);
  }
  return parts.join(" · ") || "—";
}

// ── 列表：每个绑定配额插件的 Provider 一个组，端点 key 为数据行 ──

/** 视图列表按 provider key 中文排序稳定展示。 */
const views = computed(() =>
  [...store.views].sort((a, b) => a.providerKey.localeCompare(b.providerKey, "zh")),
);

/** Provider 最近一次查询时刻（跨端点取 max）。 */
function lastQuery(v: ProviderQuotaView): number {
  let m = 0;
  for (const r of v.results) if (r.queriedAt > m) m = r.queriedAt;
  return m;
}

/** 组行悬停补充：绑定插件与查询间隔。 */
function bindingText(v: ProviderQuotaView): string {
  const interval = v.quotaIntervalSecs > 0 ? `每 ${v.quotaIntervalSecs} 秒` : "仅手动";
  return `${v.quotaPluginRef} · ${interval}`;
}

// ── 分页：按 Provider 组整组分页，容量随可视高度自适应（估算行高留余量防溢出） ──
const page = ref(1);
const tableScroll = ref<HTMLElement | null>(null);
const { availHeight } = useAutoPageSize(tableScroll, page);

/** Provider 块的估算高度（px）：组行 + 各端点行；配额/附加字段按行高累加，展开态按环形图行高。 */
function estViewHeight(v: ProviderQuotaView): number {
  const GROUP = 32;
  const EMPTY = 37;
  const KEY_BASE = 25;
  const QUOTA_LINE = 19;
  const RING = 96;
  let h = GROUP;
  if (v.results.length === 0) return h + EMPTY;
  for (const r of v.results) {
    if (chartOpen.value[v.providerKey]) {
      h += KEY_BASE + (visibleQuotas(r.payload).length ? RING : QUOTA_LINE);
      continue;
    }
    const lines = visibleQuotas(r.payload).length + payloadLines(r.payload ?? {}).length;
    h += KEY_BASE + Math.max(1, lines) * QUOTA_LINE;
  }
  return h;
}

/** 每页容纳的 Provider：累计估算高度不超实测可用高度；单组超高时独占一页（内部滚动兜底）。 */
const pages = computed<ProviderQuotaView[][]>(() => {
  const avail = availHeight.value * 0.94;
  const out: ProviderQuotaView[][] = [];
  let cur: ProviderQuotaView[] = [];
  let h = 0;
  for (const v of views.value) {
    const vh = estViewHeight(v);
    if (cur.length && h + vh > avail) {
      out.push(cur);
      cur = [];
      h = 0;
    }
    cur.push(v);
    h += vh;
  }
  if (cur.length) out.push(cur);
  return out;
});
const pageCount = computed(() => Math.max(1, pages.value.length));
const pagedViews = computed(() => pages.value[page.value - 1] ?? []);
const totalKeys = computed(() => views.value.reduce((n, v) => n + Math.max(1, v.results.length), 0));
watch(pageCount, (n) => {
  if (page.value > n) page.value = n;
});

// ── 本地等值额度：按配额窗口汇总 usage_records 中该 provider 的实际消耗 ──
const localCosts = ref(new Map<number, Map<string, ProviderCost>>());
const costBaseSec = ref(0);

/** 配额窗口起点（unix 秒）：时长取脚本声明的 `periodSecs`，末端优先取 resetAt、
 *  缺省用当前时刻；起点对齐到分钟以合并各 Provider 近似窗口。无窗口的配额返回 null。 */
function quotaSince(q: QuotaEntry, nowSec: number): number | null {
  if (typeof q.periodSecs !== "number" || q.periodSecs <= 0) return null;
  return Math.floor(((parseResetSec(q.resetAt) ?? nowSec) - q.periodSecs) / 60) * 60;
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
  for (const v of views.value)
    for (const r of v.results)
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

/** 该配额窗口内此 provider 的本地消耗；配额无窗口时返回 null。 */
function localStat(v: ProviderQuotaView, q: QuotaEntry): ProviderCost | null {
  const s = quotaSince(q, costBaseSec.value);
  if (s === null) return null;
  return localCosts.value.get(s)?.get(v.providerKey) ?? { providerKey: v.providerKey, cost: 0, requests: 0 };
}

/** 本地消耗文案：零值压成 $0，避免 $0.0000 这类四位小数噪音。 */
function localStatText(v: ProviderQuotaView, q: QuotaEntry): string | null {
  const s = localStat(v, q);
  if (!s) return null;
  return `本地 ${s.cost > 0 ? formatCost(s.cost) : "$0"}`;
}

// 查询结果刷新（queriedAt 变化）后重拉本地统计；视图列表加载完成也会触发。
watch(
  () => views.value.map((v) => v.results.map((r) => r.queriedAt).join(",")).join("|"),
  () => void loadLocalCosts(),
);

// ── 加载与查询 ──

onMounted(() => store.list());
onActivated(() => store.list());

async function refreshAll() {
  error.value = null;
  try {
    await store.refreshAll();
    toast.success("已刷新全部配额查询");
  } catch (e) {
    error.value = errMsg(e);
  }
}

async function refreshOne(providerKey: string) {
  error.value = null;
  try {
    await store.refreshOne(providerKey);
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
      <Button variant="outline" size="sm" :disabled="store.refreshingAll" @click="refreshAll">
        <RefreshCw class="size-4" :class="store.refreshingAll ? 'animate-spin' : ''" />
        一键刷新
      </Button>
    </div>

    <EmptyState v-if="store.loading && store.views.length === 0">加载中…</EmptyState>

    <!-- 空态：绑定入口在上游服务编辑页 -->
    <EmptyState
      v-else-if="store.loaded && !store.error && store.views.length === 0"
      :icon="Wallet"
    >
      <p>还没有绑定配额查询的上游服务。在「上游服务」编辑页为 Provider 绑定配额插件后，这里会展示各端点 Key 的额度。</p>
      <div class="mt-4">
        <RouterLink to="/providers">
          <Button size="sm">前往上游服务</Button>
        </RouterLink>
      </div>
    </EmptyState>

    <!-- 配额表：Provider 为组行，端点 key 为数据行；配额内联细进度条，组行展开切环形图 -->
    <div v-else-if="store.views.length > 0" class="flex min-h-0 flex-1 flex-col">
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
          <template v-for="v in pagedViews" :key="v.providerKey">
            <!-- Provider 组行：元信息与组级操作对齐到列位 -->
            <tr class="border-b bg-muted/30">
              <td class="py-1.5">
                <div class="flex min-w-0 items-center justify-center gap-1.5">
                  <button
                    type="button"
                    class="shrink-0 rounded-sm p-0.5 text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
                    :title="chartOpen[v.providerKey] ? '收起环形图' : '展开环形图'"
                    @click="toggleChart(v.providerKey)"
                  >
                    <ChevronDown
                      class="size-3.5 transition-transform"
                      :class="chartOpen[v.providerKey] ? '' : '-rotate-90'"
                    />
                  </button>
                  <span class="text-xs font-medium" :title="`${v.providerKey} · ${bindingText(v)}`">{{ v.providerKey }}</span>
                  <span class="truncate text-xs text-muted-foreground">{{ bindingText(v) }}</span>
                </div>
              </td>
              <td class="py-1.5 text-xs text-muted-foreground">
                {{ v.keyCount ? `${v.keyCount} 个 key` : "—" }}
              </td>
              <td class="py-1.5"><Badge v-if="!v.quotaEnabled" variant="secondary">停用</Badge></td>
              <td class="py-1.5 text-xs tabular-nums text-muted-foreground">{{ shortTime(lastQuery(v)) }}</td>
              <td class="py-1.5">
                <div class="flex items-center justify-center gap-0.5">
                  <Button
                    variant="ghost"
                    size="icon"
                    class="size-7"
                    title="刷新该 Provider 配额"
                    :disabled="store.refreshingAll || store.refreshingKeys.has(v.providerKey)"
                    @click="refreshOne(v.providerKey)"
                  >
                    <RefreshCw class="size-3.5" :class="store.refreshingKeys.has(v.providerKey) ? 'animate-spin' : ''" />
                  </Button>
                </div>
              </td>
            </tr>
            <!-- 未查询占位行 -->
            <tr v-if="v.results.length === 0" class="border-b">
              <td class="py-2 font-mono text-xs text-muted-foreground">—</td>
              <td class="py-2 text-xs text-muted-foreground">尚未查询</td>
              <td></td>
              <td></td>
              <td></td>
            </tr>
            <!-- 端点 key 数据行 -->
            <tr v-for="result in v.results" :key="result.keyIndex" class="border-b transition-colors hover:bg-accent/40">
              <td class="py-2 font-mono text-xs" :title="`API Key ${result.keyLabel}（掩码）`">{{ result.keyLabel || `Key ${result.keyIndex + 1}` }}</td>
              <td class="py-2">
                <!-- 收起：配额行内细进度条 + 文本；展开：环形图 -->
                <div v-if="!chartOpen[v.providerKey]" class="flex flex-col items-center gap-0.5">
                  <div class="grid grid-cols-[4rem_3.5rem_auto_5.5rem] items-center gap-x-1.5 gap-y-0.5 text-xs">
                    <template v-for="(q, i) in visibleQuotas(result.payload)" :key="`q${i}`">
                      <span class="truncate text-right text-muted-foreground" :title="`${q.label}：${quotaDetailText(q)}`">{{ q.label }}</span>
                      <span
                        v-if="quotaRingPercent(q) !== null"
                        class="inline-block h-1.5 w-14 overflow-hidden rounded-full bg-muted"
                        :title="`${q.label}：${quotaDetailText(q)}`"
                      >
                        <span class="block h-full bg-primary" :style="{ width: quotaRingPercent(q) + '%' }" />
                      </span>
                      <span v-else />
                      <span class="tabular-nums" :title="`${q.label}：${quotaDetailText(q)}`">{{ quotaText(q, false) }}</span>
                      <span
                        v-if="localStat(v, q)"
                        class="truncate text-right text-muted-foreground tabular-nums"
                        :title="`本地统计：跟随配额窗口的实际消耗 · ${localStat(v, q)!.requests} 次请求`"
                      >{{ localStatText(v, q) }}</span>
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
                      v-if="quotaRingPercent(q) !== null"
                      class="flex w-20 shrink-0 flex-col items-center gap-1 text-center"
                      :title="`${q.label}：${quotaDetailText(q)}`"
                    >
                      <div class="relative size-11 shrink-0">
                        <svg viewBox="0 0 36 36" class="size-full -rotate-90">
                          <circle cx="18" cy="18" r="15.5" fill="none" stroke-width="3" class="stroke-muted" />
                          <circle
                            cx="18" cy="18" r="15.5" fill="none" stroke-width="3" stroke-linecap="round"
                            class="stroke-primary transition-[stroke-dashoffset] duration-500"
                            :stroke-dasharray="RING_C"
                            :stroke-dashoffset="RING_C * (1 - (quotaRingPercent(q) ?? 0) / 100)"
                          />
                        </svg>
                        <div class="absolute inset-0 flex items-center justify-center text-[10px] font-semibold tabular-nums">
                          {{ (quotaRingPercent(q) ?? 0).toFixed(0) }}%
                        </div>
                      </div>
                      <div class="w-full truncate text-[11px]">{{ q.label }}</div>
                      <div v-if="localStat(v, q)" class="w-full truncate text-[10px] text-muted-foreground tabular-nums">
                        {{ localStatText(v, q) }}
                      </div>
                    </div>
                    <div
                      v-else
                      class="flex min-h-16 w-20 shrink-0 items-center justify-center text-center"
                      :title="`${q.label}：${quotaDetailText(q)}`"
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
  </div>
</template>
