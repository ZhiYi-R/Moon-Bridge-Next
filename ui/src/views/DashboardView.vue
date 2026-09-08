<script setup lang="ts">
import { AlertCircle, RefreshCw } from "lucide-vue-next";
import { onMounted, ref } from "vue";
import { useRouter } from "vue-router";

import Badge from "@/components/ui/Badge.vue";
import Button from "@/components/ui/Button.vue";
import Card from "@/components/ui/Card.vue";
import { errMsg, providerApi, usageApi, type Provider, type UsageRecord, type UsageSummary } from "@/lib/api";
import { formatTokens } from "@/lib/utils";
import { useGatewayStore } from "@/stores/gateway";

const gateway = useGatewayStore();
const router = useRouter();

const providers = ref<Provider[]>([]);
const summary = ref<UsageSummary | null>(null);
const loadError = ref<string | null>(null);

// ───────────────── 性能时序：平均 TTFT / 平均 TPS（逐小时分桶） ─────────────────

const records = ref<UsageRecord[]>([]);
const perfLoading = ref(false);

interface PerfBucket {
  label: string;
  /** 该小时平均 TTFT（ms）；无流式样本为 null */
  ttft: number | null;
  /** 该小时加权 TPS（输出 token/生成秒）；无样本为 null */
  tps: number | null;
  /** 流式样本数 */
  samples: number;
  /** 归一化柱高（相对本序列最大值的百分比） */
  ttftPct: number;
  tpsPct: number;
}

/** 从记录计算 TTFT/TPS 时序（最近 ≤24 个小时桶）与整体均值。 */
function perfStats(): { buckets: PerfBucket[]; avgTtft: number | null; avgTps: number | null } {
  const recs = records.value.filter((r) => r.ttftMs != null && r.ttftMs > 0);
  if (recs.length === 0) return { buckets: [], avgTtft: null, avgTps: null };

  const size = 3600;
  const map = new Map<number, { ttftSum: number; n: number; out: number; genSec: number }>();
  let ttftSum = 0;
  let outSum = 0;
  let genSum = 0;
  for (const r of recs) {
    const key = Math.floor(r.createdAt / size) * size;
    const genSec = (r.latencyMs - (r.ttftMs ?? 0)) / 1000;
    const e = map.get(key) ?? { ttftSum: 0, n: 0, out: 0, genSec: 0 };
    e.ttftSum += r.ttftMs!;
    e.n += 1;
    if (genSec > 0) {
      e.out += r.outputTokens;
      e.genSec += genSec;
    }
    map.set(key, e);
    ttftSum += r.ttftMs!;
    outSum += r.outputTokens;
    if (genSec > 0) genSum += genSec;
  }

  const buckets = [...map.entries()]
    .sort((a, b) => a[0] - b[0])
    .slice(-24)
    .map(([t, v]) => ({
      t,
      label: `${new Date(t * 1000).getHours()}:00`,
      ttft: v.n > 0 ? v.ttftSum / v.n : null,
      tps: v.genSec > 0 ? v.out / v.genSec : null,
      samples: v.n,
    }));
  const maxTtft = Math.max(1, ...buckets.map((b) => b.ttft ?? 0));
  const maxTps = Math.max(1, ...buckets.map((b) => b.tps ?? 0));

  return {
    buckets: buckets.map((b) => ({
      ...b,
      ttftPct: ((b.ttft ?? 0) / maxTtft) * 100,
      tpsPct: ((b.tps ?? 0) / maxTps) * 100,
    })),
    avgTtft: ttftSum / recs.length,
    avgTps: genSum > 0 ? outSum / genSum : null,
  };
}

const perf = ref<{ buckets: PerfBucket[]; avgTtft: number | null; avgTps: number | null }>({
  buckets: [],
  avgTtft: null,
  avgTps: null,
});

const TTFT_BAR = "bg-sky-500/60 group-hover:bg-sky-500/85";
const TPS_BAR = "bg-emerald-500/60 group-hover:bg-emerald-500/85";

async function loadPerf() {
  perfLoading.value = true;
  try {
    records.value = await usageApi.query({ limit: 1000 });
    perf.value = perfStats();
  } catch {
    // 性能图数据加载失败不影响页面其余部分
  } finally {
    perfLoading.value = false;
  }
}

async function loadAll() {
  await gateway.refresh();
  try {
    const [p, s] = await Promise.all([providerApi.list(), usageApi.summary()]);
    providers.value = p;
    summary.value = s;
    loadError.value = null;
  } catch (e) {
    loadError.value = errMsg(e);
  }
  await loadPerf();
}

onMounted(loadAll);
</script>

<template>
  <div class="space-y-4">
    <div
      v-if="loadError"
      class="flex items-center gap-2 rounded-md border border-destructive/40 bg-destructive/10 px-4 py-3 text-sm text-destructive"
    >
      <AlertCircle class="size-4" />
      {{ loadError }}
    </div>

    <!-- 网关错误（启动/停止控制已收敛在顶栏开关） -->
    <div
      v-if="gateway.error"
      class="rounded-md border border-destructive/40 bg-destructive/10 px-4 py-2 text-sm text-destructive"
    >
      {{ gateway.error }}
    </div>

    <!-- 用量统计 -->
    <div class="grid gap-4 md:grid-cols-3">
      <Card>
        <div class="card-header pb-2">
          <span class="card-description">总请求</span>
        </div>
        <div class="card-content">
          <div class="text-2xl font-semibold tabular-nums">{{ summary?.requests ?? 0 }}</div>
        </div>
      </Card>
      <Card>
        <div class="card-header pb-2">
          <span class="card-description">总 Tokens</span>
        </div>
        <div class="card-content">
          <div class="text-2xl font-semibold tabular-nums">
            {{ formatTokens((summary?.inputTokens ?? 0) + (summary?.outputTokens ?? 0)) }}
          </div>
          <p class="mt-1 text-xs text-muted-foreground">
            输入 {{ formatTokens(summary?.inputTokens ?? 0) }} · 输出
            {{ formatTokens(summary?.outputTokens ?? 0) }}
          </p>
        </div>
      </Card>
      <Card>
        <div class="card-header pb-2">
          <span class="card-description">总成本</span>
        </div>
        <div class="card-content">
          <div class="text-2xl font-semibold tabular-nums">${{ (summary?.totalCost ?? 0).toFixed(4) }}</div>
        </div>
      </Card>
    </div>

    <!-- 性能时序：平均 TTFT / 平均 TPS（流式请求，逐小时） -->
    <div class="grid gap-4 lg:grid-cols-2">
      <Card>
        <div class="card-header flex-row items-center justify-between space-y-0">
          <div class="flex items-center gap-2">
            <h3 class="card-title">平均 TTFT</h3>
            <span v-if="perf.avgTtft != null" class="text-sm font-semibold text-foreground tabular-nums">
              {{ Math.round(perf.avgTtft) }} ms
            </span>
          </div>
          <Button
            variant="ghost"
            size="icon"
            class="size-7"
            :disabled="perfLoading"
            title="刷新"
            @click="loadPerf"
          >
            <RefreshCw class="size-3.5" :class="perfLoading ? 'animate-spin' : ''" />
          </Button>
        </div>
        <div class="card-content">
          <div
            v-if="perf.buckets.length === 0"
            class="rounded-md border border-dashed p-6 text-center text-sm text-muted-foreground"
          >
            暂无流式请求数据。
          </div>
          <div v-else class="flex h-20 items-end gap-1">
            <div
              v-for="b in perf.buckets"
              :key="b.label"
              class="group flex h-full flex-1 items-end"
              :title="`${b.label} · ${b.samples} 次流式 · 平均 ${b.ttft != null ? Math.round(b.ttft) + ' ms' : '—'}`"
            >
              <div
                class="w-full rounded-t-sm transition-all"
                :class="TTFT_BAR"
                :style="{ height: b.ttftPct + '%' }"
              />
            </div>
          </div>
        </div>
      </Card>

      <Card>
        <div class="card-header flex-row items-center justify-between space-y-0">
          <div class="flex items-center gap-2">
            <h3 class="card-title">平均 TPS</h3>
            <span v-if="perf.avgTps != null" class="text-sm font-semibold text-foreground tabular-nums">
              {{ perf.avgTps.toFixed(1) }} tok/s
            </span>
          </div>
        </div>
        <div class="card-content">
          <div
            v-if="perf.buckets.length === 0"
            class="rounded-md border border-dashed p-6 text-center text-sm text-muted-foreground"
          >
            暂无流式请求数据。
          </div>
          <div v-else class="flex h-20 items-end gap-1">
            <div
              v-for="b in perf.buckets"
              :key="b.label"
              class="group flex h-full flex-1 items-end"
              :title="`${b.label} · ${b.samples} 次流式 · 平均 ${b.tps != null ? b.tps.toFixed(1) + ' tok/s' : '—'}`"
            >
              <div
                class="w-full rounded-t-sm transition-all"
                :class="TPS_BAR"
                :style="{ height: b.tpsPct + '%' }"
              />
            </div>
          </div>
        </div>
      </Card>
    </div>

    <!-- Providers 概览 -->
    <Card>
      <div class="card-header flex-row items-center justify-between space-y-0">
        <div>
          <h2 class="card-title">上游服务</h2>
          <p class="card-description">已配置 {{ providers.length }} 个上游服务商</p>
        </div>
        <Button size="sm" variant="outline" @click="router.push('/providers')">管理</Button>
      </div>
      <div class="card-content">
        <div
          v-if="providers.length === 0"
          class="rounded-md border border-dashed p-6 text-center text-sm text-muted-foreground"
        >
          尚未配置上游服务。前往「上游服务」页添加。
        </div>
        <ul v-else class="divide-y divide-border">
          <li v-for="p in providers" :key="p.key" class="flex items-center justify-between py-2.5">
            <div class="flex items-center gap-3">
              <span class="font-medium">{{ p.key }}</span>
              <Badge variant="outline" class="font-mono text-[10px]">
                {{ [...new Set(p.endpoints.map((e) => e.protocol))].join(" · ") }}
              </Badge>
            </div>
            <div class="flex items-center gap-3">
              <span class="max-w-[240px] truncate text-xs text-muted-foreground">
                {{ p.endpoints[0]?.baseUrl }}<template v-if="p.endpoints.length > 1"> ×{{ p.endpoints.length }}</template>
              </span>
              <Badge :variant="p.enabled ? 'success' : 'secondary'">
                {{ p.enabled ? "启用" : "停用" }}
              </Badge>
            </div>
          </li>
        </ul>
      </div>
    </Card>
  </div>
</template>
