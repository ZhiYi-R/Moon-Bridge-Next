<script setup lang="ts">
import { RefreshCw, Trash2 } from "lucide-vue-next";
import { computed, onMounted, ref } from "vue";

import Badge from "@/components/ui/Badge.vue";
import Button from "@/components/ui/Button.vue";
import Card from "@/components/ui/Card.vue";
import Input from "@/components/ui/Input.vue";
import { appApi, errMsg, traceApi, type TraceDetail, type TraceEntry } from "@/lib/api";
import { formatBytes, formatLatency, formatTimeMs, formatTokens } from "@/lib/utils";
import { useConfirm } from "@/composables/useConfirm";

const { confirm } = useConfirm();
const entries = ref<TraceEntry[]>([]);
const error = ref<string | null>(null);
const loading = ref(false);
const filter = ref("");
const selected = ref<TraceEntry | null>(null);
const detail = ref<TraceDetail | null>(null);
const detailLoading = ref(false);
const traceDir = ref("");

const filtered = computed(() => {
  const q = filter.value.trim().toLowerCase();
  if (!q) return entries.value;
  return entries.value.filter(
    (e) =>
      e.session.toLowerCase().includes(q) ||
      e.model.toLowerCase().includes(q) ||
      e.fileName.toLowerCase().includes(q),
  );
});

async function load() {
  loading.value = true;
  error.value = null;
  try {
    entries.value = await traceApi.list(500);
  } catch (e) {
    error.value = errMsg(e);
  } finally {
    loading.value = false;
  }
}

async function open(entry: TraceEntry) {
  selected.value = entry;
  detail.value = null;
  detailLoading.value = true;
  try {
    detail.value = await traceApi.read(entry.relPath);
  } catch (e) {
    error.value = errMsg(e);
  } finally {
    detailLoading.value = false;
  }
}

async function remove(entry: TraceEntry) {
  if (!(await confirm({ title: "删除 trace", message: `确认删除 trace “${entry.relPath}”？` }))) return;
  try {
    await traceApi.remove(entry.relPath);
    if (selected.value?.relPath === entry.relPath) {
      selected.value = null;
      detail.value = null;
    }
    await load();
  } catch (e) {
    error.value = errMsg(e);
  }
}

function pretty(v: unknown): string {
  if (v === null || v === undefined) return "—";
  try {
    return JSON.stringify(v, null, 2);
  } catch {
    return String(v);
  }
}

onMounted(async () => {
  await load();
  try {
    traceDir.value = (await appApi.info()).traceDir;
  } catch {
    // 忽略：仅展示用途
  }
});
</script>

<template>
  <div class="flex h-full min-h-0 flex-col gap-4">
    <div
      v-if="error"
      class="shrink-0 rounded-md border border-destructive/40 bg-destructive/10 px-4 py-2 text-sm text-destructive"
    >
      {{ error }}
    </div>

    <!-- 主从一体卡：左列表 / 右详情，中缝分齐，高度填满视口 -->
    <Card class="grid min-h-0 flex-1 overflow-hidden lg:grid-cols-[360px_1fr] lg:divide-x lg:divide-border">
      <!-- 列表 -->
      <section class="flex min-h-0 flex-col">
        <div class="flex shrink-0 items-center gap-2 border-b p-3">
          <Input v-model="filter" placeholder="按会话 / 模型 / 文件名过滤…" class="flex-1" />
          <Button
            variant="ghost"
            size="icon"
            class="size-8 shrink-0"
            :disabled="loading"
            title="刷新"
            @click="load"
          >
            <RefreshCw class="size-4" :class="loading && 'animate-spin'" />
          </Button>
        </div>
        <div class="scrollbar-thin min-h-0 flex-1 overflow-y-auto p-3">
          <div
            v-if="filtered.length === 0"
            class="rounded-md border border-dashed p-8 text-center text-sm text-muted-foreground"
          >
            暂无 trace。启动网关并发起请求后将在此显示。
          </div>
          <ul v-else class="space-y-1.5">
            <li
              v-for="e in filtered"
              :key="e.relPath"
              class="group cursor-pointer rounded-lg border p-2.5 transition-colors hover:bg-accent"
              :class="selected?.relPath === e.relPath && 'border-primary bg-accent'"
              @click="open(e)"
            >
              <div class="flex items-center justify-between gap-2">
                <span class="truncate font-mono text-xs font-medium">{{ e.model }}</span>
                <span class="shrink-0 text-[10px] text-muted-foreground">
                  {{ formatBytes(e.size) }}
                </span>
              </div>
              <div class="mt-0.5 flex items-center justify-between gap-2">
                <span class="truncate text-[11px] text-muted-foreground">
                  {{ e.session }}
                </span>
                <button
                  class="shrink-0 text-muted-foreground opacity-0 transition-opacity hover:text-destructive group-hover:opacity-100"
                  title="删除"
                  @click.stop="remove(e)"
                >
                  <Trash2 class="size-3.5" />
                </button>
              </div>
              <div class="mt-0.5 text-[10px] text-muted-foreground">
                {{ formatTimeMs(e.modifiedAt) }}
              </div>
            </li>
          </ul>
        </div>
        <div
          v-if="traceDir"
          class="shrink-0 truncate border-t px-3 py-2 font-mono text-[10px] text-muted-foreground"
          :title="traceDir"
        >
          {{ traceDir }}
        </div>
      </section>

      <!-- 详情 -->
      <section class="flex min-h-0 flex-col">
        <div v-if="!selected" class="flex h-full items-center justify-center">
          <p class="text-sm text-muted-foreground">从左侧选择一条 trace 查看详情。</p>
        </div>
        <template v-else>
          <div class="flex shrink-0 flex-wrap items-center gap-2 border-b p-4">
            <h3 class="card-title truncate font-mono text-sm">{{ selected.relPath }}</h3>
            <Badge v-if="detail" :variant="detail.status === 'ok' ? 'success' : 'destructive'">
              {{ detail.status }}
            </Badge>
            <Badge v-if="detail?.stream" variant="outline">stream</Badge>
          </div>
          <div class="scrollbar-thin min-h-0 flex-1 overflow-y-auto p-5">
            <div v-if="detailLoading" class="py-10 text-center text-sm text-muted-foreground">
              加载中…
            </div>
            <div v-else-if="detail" class="space-y-4">
              <!-- 元信息：对齐的定义列表 -->
              <dl class="grid grid-cols-[80px_1fr] items-baseline gap-x-3 gap-y-1.5 text-xs">
                <dt class="text-muted-foreground">请求 ID</dt>
                <dd class="break-all font-mono">{{ detail.requestId }}</dd>
                <dt class="text-muted-foreground">会话</dt>
                <dd class="break-all font-mono">{{ detail.sessionId ?? "—" }}</dd>
                <dt class="text-muted-foreground">入口协议</dt>
                <dd class="font-mono">{{ detail.clientProtocol }}</dd>
                <dt class="text-muted-foreground">上游协议</dt>
                <dd class="font-mono">{{ detail.upstreamProtocol }}</dd>
                <dt class="text-muted-foreground">上游服务</dt>
                <dd class="break-all font-mono">{{ detail.providerKey }}</dd>
                <dt class="text-muted-foreground">模型</dt>
                <dd class="break-all font-mono">{{ detail.modelAlias }} → {{ detail.upstreamModel }}</dd>
                <dt class="text-muted-foreground">延迟</dt>
                <dd class="font-mono">{{ formatLatency(detail.latencyMs) }}</dd>
                <dt class="text-muted-foreground">用量</dt>
                <dd class="flex flex-wrap gap-x-1.5 font-mono">
                  <span>输入 {{ formatTokens(detail.usage.inputTokens) }}</span>
                  <span>输出 {{ formatTokens(detail.usage.outputTokens) }}</span>
                  <span v-if="detail.usage.reasoningTokens">推理 {{ formatTokens(detail.usage.reasoningTokens) }}</span>
                  <span v-if="detail.usage.cacheReadTokens">缓存读 {{ formatTokens(detail.usage.cacheReadTokens) }}</span>
                  <span v-if="detail.usage.cacheWriteTokens">缓存写 {{ formatTokens(detail.usage.cacheWriteTokens) }}</span>
                </dd>
                <dt class="text-muted-foreground">时间</dt>
                <dd class="font-mono">{{ formatTimeMs(detail.createdAt) }}</dd>
              </dl>

              <div
                v-if="detail.error"
                class="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-xs text-destructive"
              >
                {{ detail.error }}
              </div>

              <!-- 各阶段报文 -->
              <section v-for="sec in [
                { title: '客户端请求', body: detail.clientRequest },
                { title: '上游请求', body: detail.upstreamRequest },
                { title: '上游响应', body: detail.upstreamResponse },
                { title: '客户端响应', body: detail.clientResponse },
              ]" :key="sec.title">
                <h4 class="mb-1 text-xs font-semibold text-muted-foreground">{{ sec.title }}</h4>
                <pre class="scrollbar-thin max-h-72 overflow-auto rounded-md bg-muted p-3 font-mono text-[11px] leading-relaxed">{{ pretty(sec.body) }}</pre>
              </section>
            </div>
          </div>
        </template>
      </section>
    </Card>
  </div>
</template>
