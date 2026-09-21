<script setup lang="ts">
import { BarChart3, RefreshCw } from "lucide-vue-next";
import { computed, onActivated, onMounted, ref, watch } from "vue";

import Alert from "@/components/ui/Alert.vue";
import Badge from "@/components/ui/Badge.vue";
import Button from "@/components/ui/Button.vue";
import Card from "@/components/ui/Card.vue";
import EmptyState from "@/components/ui/EmptyState.vue";
import Pagination from "@/components/ui/Pagination.vue";
import DateRangeFilter, { type DateRange } from "@/components/ui/DateRangeFilter.vue";
import { useAutoPageSize } from "@/composables/useAutoPageSize";
import { errMsg, modelApi, usageApi, type UsageRecord, type UsageSummary } from "@/lib/api";
import { formatCost, formatLatency, formatTime, formatTokens } from "@/lib/utils";

const records = ref<UsageRecord[]>([]);
const summary = ref<UsageSummary | null>(null);
const error = ref<string | null>(null);
const loading = ref(false);

/** 时间范围筛选：null = 不限；变化时按 since/until 重查后端（汇总/图表/明细全部跟随）。 */
const range = ref<DateRange | null>(null);

/** 模型 slug → 展示名映射（加载失败不影响用量展示，回退显示 slug）。 */
const modelNames = ref(new Map<string, string>());

function displayName(slug: string | null | undefined): string {
  if (!slug) return "—";
  return modelNames.value.get(slug) ?? slug;
}

/** silent=true 用于 keep-alive 切回时的后台重拉：不点亮 refresh 图标，避免每次进页面都闪一次 loading。 */
async function load(silent = false) {
  loading.value = !silent;
  error.value = null;
  try {
    const [recs, sum, models] = await Promise.all([
      usageApi.query({ limit: 500, since: range.value?.from, until: range.value?.to }),
      usageApi.summary({ since: range.value?.from, until: range.value?.to }),
      modelApi.list().catch(() => []),
    ]);
    records.value = recs;
    summary.value = sum;
    modelNames.value = new Map(
      models.filter((m) => m.displayName).map((m) => [m.slug, m.displayName as string]),
    );
  } catch (e) {
    error.value = errMsg(e);
  } finally {
    loading.value = false;
  }
}

interface TimeBucket {
  label: string;
  input: number;
  output: number;
  cacheRead: number;
  cacheWrite: number;
  reasoning: number;
  total: number;
  requests: number;
  /** 各系列高度百分比，顺序与 SERIES 一致（堆叠自上而下）。 */
  pcts: number[];
}

/** 时序图系列（堆叠自上而下），颜色为完整类名字面量以便 Tailwind 扫描。 */
const SERIES = [
  { label: "缓存读", bar: "bg-emerald-500/60 group-hover:bg-emerald-500/85", dot: "bg-emerald-500" },
  { label: "缓存写", bar: "bg-amber-500/60 group-hover:bg-amber-500/85", dot: "bg-amber-500" },
  { label: "输出", bar: "bg-cyan-400/60 group-hover:bg-cyan-400/85", dot: "bg-cyan-400" },
  { label: "推理", bar: "bg-fuchsia-500/60 group-hover:bg-fuchsia-500/85", dot: "bg-fuchsia-500" },
  { label: "输入", bar: "bg-violet-500/60 group-hover:bg-violet-500/85", dot: "bg-violet-500" },
] as const;

/** 按时间分桶（跨度 ≤48h 用小时，否则用天），取最近至多 24 桶，堆叠五类 token。 */
function timeBuckets(): TimeBucket[] {
  const recs = records.value
    .filter((r) => r.createdAt > 0)
    .slice()
    .sort((a, b) => a.createdAt - b.createdAt);
  if (recs.length === 0) return [];
  const span = recs[recs.length - 1].createdAt - recs[0].createdAt;
  const byHour = span <= 48 * 3600;
  const size = byHour ? 3600 : 86400;
  const map = new Map<
    number,
    { input: number; output: number; cacheRead: number; cacheWrite: number; reasoning: number; requests: number }
  >();
  for (const r of recs) {
    const key = Math.floor(r.createdAt / size) * size;
    const e =
      map.get(key) ?? { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, reasoning: 0, requests: 0 };
    // 字段缺失时回退 0，避免 undefined 污染聚合与后续除法（NaN 会把柱状图画成满宽）
    e.input += r.inputTokens || 0;
    e.output += r.outputTokens || 0;
    e.cacheRead += r.cacheReadTokens || 0;
    e.cacheWrite += r.cacheWriteTokens || 0;
    e.reasoning += r.reasoningTokens || 0;
    e.requests += 1;
    map.set(key, e);
  }
  const buckets = [...map.entries()]
    .map(([t, v]) => ({ t, ...v, total: v.input + v.output + v.cacheRead + v.cacheWrite + v.reasoning }))
    .sort((a, b) => a.t - b.t)
    .slice(-24);
  const max = Math.max(1, ...buckets.map((b) => b.total).filter((n) => Number.isFinite(n)));
  return buckets.map((b) => ({
    label: byHour
      ? `${new Date(b.t * 1000).getHours()}:00`
      : new Date(b.t * 1000).toLocaleDateString(undefined, { month: "numeric", day: "numeric" }),
    input: b.input,
    output: b.output,
    cacheRead: b.cacheRead,
    cacheWrite: b.cacheWrite,
    reasoning: b.reasoning,
    total: b.total,
    requests: b.requests,
    pcts: [b.cacheRead, b.cacheWrite, b.output, b.reasoning, b.input].map((v) => (v / max) * 100),
  }));
}

interface ModelBucket {
  model: string;
  /** 聚合行的成员 slug（仅「其他」行有值，用于 tooltip）。 */
  members?: string[];
  requests: number;
  input: number;
  output: number;
  cacheRead: number;
  cost: number;
  total: number;
  /** 缓存命中率 = 缓存读 / 输入（input 为 0 时 null）；与仪表盘口径一致。 */
  hitRate: number | null;
  pct: number;
}

/** 分布行数上限：超出聚合为「其他」行，避免卡片被拉长挤压下方明细表。 */
const MAX_MODEL_ROWS = 3;
const OTHER_MODEL = "__other__";

/** 按模型聚合 token/请求/成本，按总量降序；Top 6 之外并入「其他」。 */
function modelBuckets(): ModelBucket[] {
  const map = new Map<string, { requests: number; input: number; output: number; cacheRead: number; cost: number }>();
  for (const r of records.value) {
    const k = r.model ?? "—";
    const e = map.get(k) ?? { requests: 0, input: 0, output: 0, cacheRead: 0, cost: 0 };
    e.requests += 1;
    e.input += r.inputTokens || 0;
    e.output += r.outputTokens || 0;
    e.cacheRead += r.cacheReadTokens || 0;
    e.cost += r.cost || 0;
    map.set(k, e);
  }
  const arr = [...map.entries()]
    .map(([model, v]) => ({ model, ...v, total: v.input + v.output }))
    .sort((a, b) => b.total - a.total);

  const rows: Omit<ModelBucket, "hitRate" | "pct">[] = arr.slice(0, MAX_MODEL_ROWS);
  const rest = arr.slice(MAX_MODEL_ROWS);
  if (rest.length > 0) {
    const agg = rest.reduce(
      (a, b) => ({
        requests: a.requests + b.requests,
        input: a.input + b.input,
        output: a.output + b.output,
        cacheRead: a.cacheRead + b.cacheRead,
        cost: a.cost + b.cost,
        total: a.total + b.total,
      }),
      { requests: 0, input: 0, output: 0, cacheRead: 0, cost: 0, total: 0 },
    );
    rows.push({ model: OTHER_MODEL, members: rest.map((r) => r.model), ...agg });
  }

  const max = Math.max(1, ...rows.map((a) => a.total).filter((n) => Number.isFinite(n)));
  return rows.map((a) => ({
    ...a,
    hitRate: a.input > 0 ? a.cacheRead / a.input : null,
    pct: a.total > 0 ? (a.total / max) * 100 : 0,
  }));
}

const tBuckets = computed(timeBuckets);
const mBuckets = computed(modelBuckets);

onMounted(() => {
  void load();
});

// keep-alive 下切回本页：第二次起静默重拉，不闪 loading、不清空现有列表
let activated = false;
onActivated(() => {
  if (activated) void load(true);
  activated = true;
});

async function refresh() {
  await load();
}

// ───────────────── 明细分页（页大小随可视高度自适应，避免滚动+翻页双溢出） ───────────────
const page = ref(1);
const detailScroll = ref<HTMLElement | null>(null);
const { pageSize } = useAutoPageSize(detailScroll, page);
const pageCount = computed(() => Math.max(1, Math.ceil(records.value.length / pageSize.value)));
const pagedRecords = computed(() =>
  records.value.slice((page.value - 1) * pageSize.value, page.value * pageSize.value),
);
watch(pageCount, (n) => {
  if (page.value > n) page.value = n;
});
watch(range, () => {
  page.value = 1;
  void load(true);
});
</script>

<template>
  <div class="flex h-full min-h-0 flex-col gap-4">
    <Alert v-if="error" class="shrink-0">{{ error }}</Alert>

    <!-- 汇总摘要（紧凑单行，替代统计卡片堆叠）；右侧为时间范围筛选 -->
    <div class="flex shrink-0 items-center justify-between gap-4">
      <div class="flex min-w-0 flex-wrap items-center gap-x-6 gap-y-1 text-sm text-muted-foreground">
        <span>
          请求
          <span class="font-semibold text-foreground tabular-nums">{{ formatTokens(summary?.requests ?? 0) }}</span>
        </span>
        <span>
          输入 token
          <span class="font-semibold text-foreground tabular-nums">{{ formatTokens(summary?.inputTokens ?? 0) }}</span>
        </span>
        <span>
          输出 token
          <span class="font-semibold text-foreground tabular-nums">{{ formatTokens(summary?.outputTokens ?? 0) }}</span>
        </span>
        <span>
          缓存读
          <span class="font-semibold text-foreground tabular-nums">{{ formatTokens(summary?.cacheReadTokens ?? 0) }}</span>
        </span>
        <span>
          推理
          <span class="font-semibold text-foreground tabular-nums">{{ formatTokens(summary?.reasoningTokens ?? 0) }}</span>
        </span>
        <span>
          总成本
          <span class="font-semibold text-foreground tabular-nums">{{ formatCost(summary?.totalCost ?? 0) }}</span>
        </span>
      </div>
      <DateRangeFilter v-model="range" class="shrink-0" />
    </div>

    <!-- 图表行：时序 + 模型分布并排；auto-fit 窄窗口自动堆叠 -->
    <div class="grid shrink-0 gap-4 grid-cols-[repeat(auto-fit,minmax(20rem,1fr))]">
    <Card class="min-w-0 shrink-0">
      <div class="card-header">
        <h3 class="card-title">Token 消耗</h3>
        <div class="flex flex-wrap items-center gap-3 text-xs text-muted-foreground">
          <span
            v-for="s in SERIES"
            :key="s.label"
            class="flex items-center gap-1"
          >
            <i class="size-2.5 rounded-sm" :class="s.dot" />{{ s.label }}
          </span>
        </div>
      </div>
      <div class="card-content">
        <EmptyState v-if="tBuckets.length === 0" :icon="BarChart3">
          暂无数据。启动网关并发起请求后将在此显示。
        </EmptyState>
        <div v-else class="flex h-28 items-end gap-1 border-b">
          <div
            v-for="(b, i) in tBuckets"
            :key="i"
            class="group flex h-full flex-1 flex-col justify-end"
            :title="`${b.label} · ${b.requests} 次 · 输入 ${formatTokens(b.input)} / 输出 ${formatTokens(b.output)} / 推理 ${formatTokens(b.reasoning)} / 缓存读 ${formatTokens(b.cacheRead)} / 缓存写 ${formatTokens(b.cacheWrite)}`"
          >
            <div class="flex w-full flex-col justify-end overflow-hidden rounded-t-sm" style="height: 100%">
              <div
                v-for="(s, si) in SERIES"
                :key="s.label"
                class="w-full transition-all"
                :class="s.bar"
                :style="{ height: (b.pcts[si] || 0) + '%' }"
              ></div>
            </div>
          </div>
        </div>
      </div>
    </Card>

    <Card class="min-w-0 shrink-0">
      <div class="card-header">
        <h3 class="card-title">模型分布</h3>
      </div>
      <div class="card-content">
        <EmptyState v-if="mBuckets.length === 0" :icon="BarChart3">暂无数据。</EmptyState>
        <ul v-else class="space-y-3">
          <li v-for="m in mBuckets" :key="m.model">
            <div class="mb-1 flex items-center justify-between text-sm">
              <span
                class="text-xs"
                :class="m.model === OTHER_MODEL && 'text-muted-foreground'"
                :title="m.members?.join('、') ?? m.model"
              >
                {{ m.model === OTHER_MODEL ? `其他 ${m.members?.length ?? 0} 个模型` : displayName(m.model) }}
              </span>
              <span class="text-xs text-muted-foreground">
                {{ formatTokens(m.total) }} · {{ formatCost(m.cost) }} · 命中率
                {{ m.hitRate != null ? (m.hitRate * 100).toFixed(1) + "%" : "—" }}
              </span>
            </div>
            <!-- 0/0（无 token）不渲染轨道：满宽空轨道看起来像满值进度条 -->
            <div v-if="m.total > 0" class="h-2.5 w-full overflow-hidden rounded-full bg-muted">
              <div
                class="h-full rounded-full"
                :class="m.model === OTHER_MODEL ? 'bg-muted-foreground/50' : 'bg-primary/80'"
                :style="{ width: (Number.isFinite(m.pct) ? m.pct : 0) + '%' }"
              ></div>
            </div>
          </li>
        </ul>
      </div>
    </Card>
    </div>

    <!-- 明细表（客户端分页，页大小随高度自适应；min-h 保证至少数行，极端矮窗口由页面滚动接管） -->
    <Card class="flex min-h-[16rem] flex-1 flex-col overflow-hidden">
      <div class="card-header shrink-0 flex-row items-center justify-between space-y-0">
        <h3 class="card-title">最近记录</h3>
        <Button
          variant="ghost"
          size="icon"
          class="size-7"
          :disabled="loading"
          title="刷新"
          @click="refresh"
        >
          <RefreshCw class="size-3.5" :class="loading ? 'animate-spin' : ''" />
        </Button>
      </div>
      <div class="card-content flex min-h-0 flex-1 flex-col">
        <EmptyState v-if="loading && records.length === 0">加载中…</EmptyState>
        <EmptyState v-else-if="records.length === 0">{{ range ? "该时间范围内暂无记录。" : "暂无用量记录。" }}</EmptyState>
        <template v-else>
          <div ref="detailScroll" class="scrollbar-thin min-h-[60px] flex-1 overflow-auto">
            <table class="w-full text-center text-sm">
              <thead class="thead-sticky">
                <tr class="border-b text-muted-foreground">
                  <th class="py-2 font-medium">时间</th>
                  <th class="py-2 font-medium">模型 / 上游</th>
                  <th class="py-2 font-medium">输入</th>
                  <th class="py-2 font-medium">输出</th>
                  <th class="py-2 font-medium">成本</th>
                  <th class="py-2 font-medium">延迟</th>
                  <th class="py-2 font-medium">状态</th>
                </tr>
              </thead>
              <tbody>
                <tr v-for="r in pagedRecords" :key="r.id" class="border-b transition-colors last:border-0 hover:bg-accent/40">
                  <td class="whitespace-nowrap py-2 text-xs text-muted-foreground">
                    {{ formatTime(r.createdAt) }}
                  </td>
                  <td
                    class="py-2"
                    :title="[r.model, r.providerKey, r.upstreamModel].filter(Boolean).join(' · ')"
                  >
                    <div class="font-medium">{{ displayName(r.model) }}</div>
                    <div class="mt-0.5 font-mono text-xs text-muted-foreground">{{ r.providerKey ?? "—" }}</div>
                  </td>
                  <td class="py-2 tabular-nums">{{ formatTokens(r.inputTokens) }}</td>
                  <td class="py-2 tabular-nums">{{ formatTokens(r.outputTokens) }}</td>
                  <td class="py-2 tabular-nums">{{ formatCost(r.cost) }}</td>
                  <td class="py-2 text-xs tabular-nums text-muted-foreground">{{ formatLatency(r.latencyMs) }}</td>
                  <td class="py-2">
                    <Badge :variant="r.status === 'ok' ? 'success' : 'destructive'">
                      {{ r.status ?? "?" }}
                    </Badge>
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
          <div v-if="records.length > pageSize" class="shrink-0 border-t pt-3">
            <Pagination v-model:page="page" :page-count="pageCount" :total="records.length" />
          </div>
        </template>
      </div>
    </Card>
  </div>
</template>
