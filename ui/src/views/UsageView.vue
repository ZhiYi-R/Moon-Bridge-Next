<script setup lang="ts">
import { RefreshCw } from "lucide-vue-next";
import { computed, onMounted, ref, watch } from "vue";

import Badge from "@/components/ui/Badge.vue";
import Button from "@/components/ui/Button.vue";
import Card from "@/components/ui/Card.vue";
import { errMsg, modelApi, usageApi, type UsageRecord, type UsageSummary } from "@/lib/api";
import { formatLatency, formatTime, formatTokens } from "@/lib/utils";

const records = ref<UsageRecord[]>([]);
const summary = ref<UsageSummary | null>(null);
const error = ref<string | null>(null);
const loading = ref(false);

/** 模型 slug → 展示名映射（加载失败不影响用量展示，回退显示 slug）。 */
const modelNames = ref(new Map<string, string>());

function displayName(slug: string | null | undefined): string {
  if (!slug) return "—";
  return modelNames.value.get(slug) ?? slug;
}

async function load() {
  loading.value = true;
  error.value = null;
  try {
    const [recs, sum, models] = await Promise.all([
      usageApi.query({ limit: 500 }),
      usageApi.summary(),
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
  requests: number;
  input: number;
  output: number;
  cost: number;
  total: number;
  pct: number;
}

/** 按模型聚合 token/请求/成本，按总量降序。 */
function modelBuckets(): ModelBucket[] {
  const map = new Map<string, { requests: number; input: number; output: number; cost: number }>();
  for (const r of records.value) {
    const k = r.model ?? "—";
    const e = map.get(k) ?? { requests: 0, input: 0, output: 0, cost: 0 };
    e.requests += 1;
    e.input += r.inputTokens || 0;
    e.output += r.outputTokens || 0;
    e.cost += r.cost || 0;
    map.set(k, e);
  }
  const arr = [...map.entries()]
    .map(([model, v]) => ({ model, ...v, total: v.input + v.output }))
    .sort((a, b) => b.total - a.total);
  const max = Math.max(1, ...arr.map((a) => a.total).filter((n) => Number.isFinite(n)));
  return arr.map((a) => ({ ...a, pct: a.total > 0 ? (a.total / max) * 100 : 0 }));
}

const tBuckets = ref<TimeBucket[]>([]);
const mBuckets = ref<ModelBucket[]>([]);

function recompute() {
  tBuckets.value = timeBuckets();
  mBuckets.value = modelBuckets();
}

onMounted(async () => {
  await load();
  recompute();
});

async function refresh() {
  await load();
  recompute();
}

// ───────────────── 明细分页（页面本身不滚动，列表翻页） ───────────────
const PAGE_SIZE = 10;
const page = ref(0);
const pageCount = computed(() => Math.max(1, Math.ceil(records.value.length / PAGE_SIZE)));
const pagedRecords = computed(() =>
  records.value.slice(page.value * PAGE_SIZE, page.value * PAGE_SIZE + PAGE_SIZE),
);
watch(pageCount, (n) => {
  if (page.value >= n) page.value = n - 1;
});
</script>

<template>
  <div class="flex h-full min-h-0 flex-col gap-4">
    <div
      v-if="error"
      class="rounded-md border border-destructive/40 bg-destructive/10 px-4 py-2 text-sm text-destructive"
    >
      {{ error }}
    </div>

    <!-- 汇总摘要（紧凑单行，替代统计卡片堆叠） -->
    <div class="flex shrink-0 flex-wrap items-center gap-x-6 gap-y-1 text-sm text-muted-foreground">
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
        总成本
        <span class="font-semibold text-foreground tabular-nums">${{ (summary?.totalCost ?? 0).toFixed(2) }}</span>
      </span>
    </div>

    <!-- 图表行：时序 + 模型分布并排，控制纵向占用 -->
    <div class="grid shrink-0 gap-4 grid-cols-2">
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
        <div
          v-if="tBuckets.length === 0"
          class="rounded-md border border-dashed p-8 text-center text-sm text-muted-foreground"
        >
          暂无数据。启动网关并发起请求后将在此显示。
        </div>
        <div v-else class="flex h-28 items-end gap-1">
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

    <!-- 模型分布 -->
    <Card class="min-w-0 shrink-0">
      <div class="card-header">
        <h3 class="card-title">模型分布</h3>
      </div>
      <div class="card-content">
        <div
          v-if="mBuckets.length === 0"
          class="rounded-md border border-dashed p-8 text-center text-sm text-muted-foreground"
        >
          暂无数据。
        </div>
        <ul v-else class="space-y-3">
          <li v-for="m in mBuckets" :key="m.model">
            <div class="mb-1 flex items-center justify-between text-sm">
              <span class="text-xs" :title="m.model">{{ displayName(m.model) }}</span>
              <span class="text-xs text-muted-foreground">
                {{ m.requests }} 次 · 输入 {{ formatTokens(m.input) }} / 输出 {{ formatTokens(m.output) }}
                <template v-if="m.cost > 0"> · ${{ m.cost.toFixed(2) }}</template>
              </span>
            </div>
            <!-- 0/0（无 token）不渲染轨道：满宽空轨道看起来像满值进度条 -->
            <div v-if="m.total > 0" class="h-2.5 w-full overflow-hidden rounded-full bg-muted">
              <div
                class="h-full rounded-full bg-primary/80"
                :style="{ width: (Number.isFinite(m.pct) ? m.pct : 0) + '%' }"
              ></div>
            </div>
          </li>
        </ul>
      </div>
    </Card>
    </div>

    <!-- 明细表（客户端分页，内部滚动兜底，页面不出现整体滚动） -->
    <Card class="flex min-h-0 flex-1 flex-col overflow-hidden">
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
        <div
          v-if="records.length === 0"
          class="rounded-md border border-dashed p-8 text-center text-sm text-muted-foreground"
        >
          暂无用量记录。
        </div>
        <template v-else>
          <div class="scrollbar-thin min-h-[60px] flex-1 overflow-auto">
            <table class="w-full text-center text-sm">
              <thead class="thead-sticky">
                <tr class="border-b text-center text-muted-foreground">
                  <th class="py-2 font-medium">时间</th>
                  <th class="py-2 font-medium">模型</th>
                  <th class="py-2 font-medium">上游</th>
                  <th class="py-2 font-medium">输入</th>
                  <th class="py-2 font-medium">输出</th>
                  <th class="py-2 font-medium">成本</th>
                  <th class="py-2 font-medium">延迟</th>
                  <th class="py-2 font-medium">状态</th>
                </tr>
              </thead>
              <tbody>
                <tr v-for="r in pagedRecords" :key="r.id" class="border-b last:border-0">
                  <td class="whitespace-nowrap py-2 text-xs text-muted-foreground">
                    {{ formatTime(r.createdAt) }}
                  </td>
                  <td class="py-2" :title="r.model ?? ''">{{ displayName(r.model) }}</td>
                  <td class="py-2 text-xs text-muted-foreground" :title="r.upstreamModel ?? ''">
                    {{ displayName(r.upstreamModel) }}
                  </td>
                  <td class="py-2">{{ formatTokens(r.inputTokens) }}</td>
                  <td class="py-2">{{ formatTokens(r.outputTokens) }}</td>
                  <td class="py-2">${{ r.cost.toFixed(2) }}</td>
                  <td class="py-2 text-xs text-muted-foreground">{{ formatLatency(r.latencyMs) }}</td>
                  <td class="py-2">
                    <Badge :variant="r.status === 'ok' ? 'success' : 'destructive'">
                      {{ r.status ?? "?" }}
                    </Badge>
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
          <div class="flex shrink-0 items-center justify-between border-t pt-3">
            <span class="text-xs text-muted-foreground">共 {{ records.length }} 条</span>
            <div class="flex items-center gap-2">
              <span class="text-xs text-muted-foreground">第 {{ page + 1 }} / {{ pageCount }} 页</span>
              <Button
                variant="outline"
                size="sm"
                :disabled="page === 0"
                @click="page--"
              >
                上一页
              </Button>
              <Button
                variant="outline"
                size="sm"
                :disabled="page >= pageCount - 1"
                @click="page++"
              >
                下一页
              </Button>
            </div>
          </div>
        </template>
      </div>
    </Card>
  </div>
</template>
