<script setup lang="ts">
import { RefreshCw, Wallet } from "lucide-vue-next";
import { computed, onActivated, onBeforeUnmount, onMounted, ref, watch } from "vue";

import Alert from "@/components/ui/Alert.vue";
import Badge from "@/components/ui/Badge.vue";
import Button from "@/components/ui/Button.vue";
import EmptyState from "@/components/ui/EmptyState.vue";
import Pagination from "@/components/ui/Pagination.vue";
import Select from "@/components/ui/Select.vue";
import { useToast } from "@/composables/useToast";
import { errMsg, usageApi, type ProviderCost, type ProviderQuotaView, type QuotaEntry, type QuotaPayload } from "@/lib/api";
import { formatCost, formatTime } from "@/lib/utils";
import { useQuotaStore } from "@/stores/quota";

const store = useQuotaStore();
const toast = useToast();

const error = ref<string | null>(null);

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

/** 悬停完整提示：详情、重置时间、本地统计逐行折行展示。 */
function quotaDetailText(v: ProviderQuotaView, q: QuotaEntry): string {
  const lines = quotaDetailLines(q);
  const reset = resetText(q);
  if (reset) lines.push(`重置 ${reset}`);
  const s = localStat(v, q);
  if (s) lines.push(`本地统计 ${s.cost > 0 ? formatCost(s.cost) : "$0"} · ${s.requests} 次请求`);
  return lines.join("\n") || "—";
}

/** 进度条比例（已用%）：percentage 取 percent；quota 由 used/(used+left) 算；缺数据返回 null（不画条）。 */
function quotaUsedPercent(q: QuotaEntry): number | null {
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

/** 查询结果里除配额（画成环形图）、失败原因和说明外的字段，按「字段名：值」逐行给出。 */
function payloadLines(payload: Record<string, unknown>): { label: string; value: string }[] {
  const lines: { label: string; value: string }[] = [];
  for (const [k, v] of Object.entries(payload)) {
    if (v === null || v === undefined || k === "quotas" || k === "message" || k === "summary") continue;
    lines.push({ label: k, value: scalarText(v) });
  }
  return lines;
}

/** 展示的配额项：counter 是无界计数（如请求数），不是配额，不展示。 */
function visibleQuotas(payload: QuotaPayload | null | undefined): QuotaEntry[] {
  return (payload?.quotas ?? []).filter((q) => q.type !== "counter");
}

/** 无环配额的兜底文字：percentage 给百分比，quota/counter 给金额与余额。 */
function quotaText(q: QuotaEntry): string {
  const parts: string[] = [];
  if (q.type === "percentage") {
    if (typeof q.usedPercent === "number") parts.push(`已用 ${q.usedPercent.toFixed(0)}%`);
    else parts.push(`剩余 ${quotaLeft(q).toFixed(0)}%`);
  } else {
    if (typeof q.usedAmount === "number") parts.push(`消耗 ${q.usedAmount}${q.unit ?? ""}`);
    if (q.type === "quota" && typeof q.leftAmount === "number") parts.push(`余额 ${q.leftAmount}${q.unit ?? ""}`);
  }
  return parts.join(" · ") || "—";
}

// ── Provider 过滤与卡片列表 ──

/** 视图列表按 provider key 中文排序稳定展示。 */
const views = computed(() =>
  [...store.views].sort((a, b) => a.providerKey.localeCompare(b.providerKey, "zh")),
);

/** Provider 过滤：默认全部；选中时只看该 Provider 的 key 卡。 */
const providerFilter = ref("all");
const providerOptions = computed(() => [
  { value: "all", label: "全部 Provider" },
  ...views.value.map((v) => ({ value: v.providerKey, label: v.providerKey })),
]);
const filteredViews = computed(() =>
  providerFilter.value === "all"
    ? views.value
    : views.value.filter((v) => v.providerKey === providerFilter.value),
);
/** 选中 Provider 已不在列表里（被删除）时回退全部。 */
watch(views, () => {
  if (providerFilter.value !== "all" && !views.value.some((v) => v.providerKey === providerFilter.value)) {
    providerFilter.value = "all";
  }
});

// ── 自适应分页：容器宽高测行列，行高取行内最高卡片的估计值，累计超高即翻页 ──

interface CardItem {
  v: ProviderQuotaView;
  result: ProviderQuotaView["results"][number] | null;
}

/** 扁平卡片流：有结果的每 key 一卡，无结果的 Provider 一张占位卡。 */
const cards = computed<CardItem[]>(() =>
  filteredViews.value.flatMap((v): CardItem[] =>
    v.results.length ? v.results.map((result): CardItem => ({ v, result })) : [{ v, result: null }],
  ),
);

/** 网格容器实测内容区宽高（高度由 flex 布局决定，不随卡片数增长）。 */
const gridWrap = ref<HTMLElement | null>(null);
const gridSize = ref({ w: 0, h: 0 });
let gridRO: ResizeObserver | null = null;
watch(
  gridWrap,
  (el) => {
    gridRO?.disconnect();
    if (!el) return;
    const measure = () => {
      const cs = getComputedStyle(el);
      gridSize.value = {
        w: el.clientWidth - parseFloat(cs.paddingLeft) - parseFloat(cs.paddingRight),
        h: el.clientHeight - parseFloat(cs.paddingTop) - parseFloat(cs.paddingBottom),
      };
    };
    gridRO = new ResizeObserver(measure);
    gridRO.observe(el);
    measure();
  },
  { immediate: true },
);
onBeforeUnmount(() => gridRO?.disconnect());

const CARD_W = 208; // minmax(13rem, 1fr) 的下限
const GAP = 8; // gap-2

/** 列数：卡片等宽，按 minmax 下限估算能放几列。 */
const cols = computed(() => Math.max(1, Math.floor((gridSize.value.w + GAP) / (CARD_W + GAP))));

/** 卡片估算高度（px）：头部两行 + 配额行（标签文本+细条约 24px/项）+ 附加字段/错误行。 */
function estCardHeight(item: CardItem): number {
  if (!item.result) return 88;
  const r = item.result;
  const quotas = Math.max(1, visibleQuotas(r.payload).length);
  const extra = payloadLines(r.payload ?? {}).length + (r.status === "error" ? 1 : 0);
  return 20 + 18 + 19 + 8 + quotas * 24 + extra * 18 + 8;
}

/** 分页：按网格行打包（一行 cols 张卡，行高取行内最高估计值），累计超高翻页。 */
const page = ref(1);
const pages = computed<CardItem[][]>(() => {
  const avail = gridSize.value.h - 4;
  const c = cols.value;
  const items = cards.value;
  const out: CardItem[][] = [];
  let cur: CardItem[] = [];
  let h = 0;
  for (let i = 0; i < items.length; i += c) {
    const row = items.slice(i, i + c);
    const rh = Math.max(...row.map(estCardHeight)) + GAP;
    if (cur.length && h + rh > avail) {
      out.push(cur);
      cur = [];
      h = 0;
    }
    cur.push(...row);
    h += rh;
  }
  if (cur.length || !out.length) out.push(cur);
  return out;
});
const pageCount = computed(() => pages.value.length);
const pagedCards = computed(() => pages.value[page.value - 1] ?? []);
watch(pageCount, (n) => {
  if (page.value > n) page.value = n;
});

/** 选中 Provider 的绑定信息行（插件 · 间隔 · key 数）。 */
const filteredMeta = computed(() => {
  if (providerFilter.value === "all") return null;
  const v = views.value.find((x) => x.providerKey === providerFilter.value);
  if (!v) return null;
  const interval = v.quotaIntervalSecs > 0 ? `每 ${v.quotaIntervalSecs} 秒` : "仅手动";
  return `${v.quotaPluginRef} · ${interval} · ${v.keyCount} 个 key`;
});

/** 上下文刷新：全部 → 刷新全部配额；选中 → 只刷该 Provider。 */
async function refresh() {
  error.value = null;
  try {
    if (providerFilter.value === "all") {
      await store.refreshAll();
      toast.success("已刷新全部配额查询");
    } else {
      await store.refreshOne(providerFilter.value);
    }
  } catch (e) {
    error.value = errMsg(e);
  }
}
const refreshing = computed(() =>
  providerFilter.value === "all" ? store.refreshingAll : store.refreshingKeys.has(providerFilter.value),
);

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

// 查询结果刷新（queriedAt 变化）后重拉本地统计；视图列表加载完成也会触发。
watch(
  () => views.value.map((v) => v.results.map((r) => r.queriedAt).join(",")).join("|"),
  () => void loadLocalCosts(),
);

// ── 加载与查询 ──

onMounted(() => store.list());
onActivated(() => store.list());
</script>

<template>
  <div class="flex h-full min-h-0 flex-col gap-4">
    <Alert v-if="error || store.error" class="shrink-0">
      <p>{{ error || store.error }}</p>
      <Button v-if="store.error" class="mt-2" variant="outline" size="sm" :disabled="store.loading" @click="error = null; store.list()">
        {{ store.loading ? "加载中…" : "重试加载" }}
      </Button>
    </Alert>

    <!-- 页头操作：Provider 过滤 + 上下文刷新（全部→全刷，选中→单刷） -->
    <div class="flex shrink-0 items-center justify-end gap-2">
      <Select v-model="providerFilter" :options="providerOptions" small class="w-44" />
      <Button variant="outline" size="sm" :disabled="refreshing" :title="providerFilter === 'all' ? '刷新全部配额' : '刷新该 Provider 配额'" @click="refresh">
        <RefreshCw class="size-4" :class="refreshing ? 'animate-spin' : ''" />
        {{ providerFilter === "all" ? "一键刷新" : "刷新" }}
      </Button>
    </div>
    <p v-if="filteredMeta" class="-mt-2 shrink-0 text-right text-xs text-muted-foreground">{{ filteredMeta }}</p>

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

    <!-- 各端点 Key 的信息卡网格；「全部」模式下卡片带 provider 名 -->
    <div v-else-if="filteredViews.length > 0" class="flex min-h-0 flex-1 flex-col">
      <section ref="gridWrap" class="min-h-0 flex-1 overflow-hidden">
        <div class="grid grid-cols-[repeat(auto-fill,minmax(13rem,1fr))] gap-2 pb-1">
        <template v-for="item in pagedCards" :key="item.result ? `${item.v.providerKey}:${item.result.keyIndex}` : `${item.v.providerKey}:empty`">
          <!-- 未查询占位卡 -->
          <div
            v-if="!item.result"
            class="rounded-md border border-dashed p-3 text-xs text-muted-foreground"
          >
            <div class="flex items-center justify-between gap-2">
              <span v-if="providerFilter === 'all'" class="truncate font-medium text-foreground">{{ item.v.providerKey }}</span>
              <span v-else class="font-mono">—</span>
              <Badge v-if="!item.v.quotaEnabled" variant="secondary">停用</Badge>
            </div>
            <p class="mt-2">尚未查询</p>
          </div>
          <!-- key 卡：掩码 key + 状态 + 配额环图；明细与本地统计进悬停 -->
          <div
            v-else
            class="rounded-md border p-2.5"
          >
            <div class="flex items-center justify-between gap-2">
              <span class="truncate font-mono text-xs" :title="`API Key ${item.result.keyLabel}（掩码）`">
                {{ item.result.keyLabel || `Key ${item.result.keyIndex + 1}` }}
              </span>
              <Badge :variant="item.result.status === 'ok' ? 'success' : 'destructive'" class="shrink-0">
                {{ item.result.status === "ok" ? "正常" : "异常" }}
              </Badge>
            </div>
            <div class="mt-0.5 flex items-center justify-between gap-2 text-[11px] text-muted-foreground">
              <span v-if="providerFilter === 'all'" class="truncate">{{ item.v.providerKey }}</span>
              <span v-else />
              <span class="shrink-0 tabular-nums" :title="formatTime(item.result.queriedAt)">{{ shortTime(item.result.queriedAt) }}</span>
            </div>
            <!-- 配额：纵向交错堆叠「标签+数值」文本行与全宽细进度条 -->
            <div class="mt-2 space-y-1.5">
              <div
                v-for="(q, i) in visibleQuotas(item.result.payload)"
                :key="i"
                :title="`${q.label}：${quotaDetailText(item.v, q)}`"
              >
                <div class="flex items-baseline justify-between gap-2 text-[11px]">
                  <span class="truncate text-muted-foreground">{{ q.label }}</span>
                  <span class="shrink-0 tabular-nums">{{ quotaText(q) }}</span>
                </div>
                <div v-if="quotaUsedPercent(q) !== null" class="mt-0.5 h-1 w-full overflow-hidden rounded-full bg-muted">
                  <div class="h-full bg-primary transition-[width] duration-500" :style="{ width: quotaUsedPercent(q) + '%' }" />
                </div>
              </div>
              <p
                v-if="item.result.status === 'ok' && !visibleQuotas(item.result.payload).length && !payloadLines(item.result.payload ?? {}).length"
                class="text-xs text-muted-foreground"
              >—</p>
            </div>
            <div v-if="item.result.status === 'error'" class="mt-1.5 text-xs text-destructive">{{ item.result.error || item.result.payload?.message || "查询失败" }}</div>
            <div v-for="(line, i) in payloadLines(item.result.payload ?? {})" :key="i" class="mt-1 flex items-baseline gap-1.5 text-[11px]">
              <span class="shrink-0 text-muted-foreground">{{ line.label }}</span>
              <span class="min-w-0 truncate tabular-nums">{{ line.value }}</span>
            </div>
          </div>
        </template>
        </div>
      </section>
      <div v-if="pageCount > 1" class="shrink-0 border-t pt-3">
        <Pagination v-model:page="page" :page-count="pageCount" :total="cards.length" />
      </div>
    </div>
  </div>
</template>
