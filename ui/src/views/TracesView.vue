<script setup lang="ts">
import { RefreshCw, ScrollText, Trash2 } from "lucide-vue-next";
import { computed, nextTick, onActivated, onDeactivated, onMounted, onUnmounted, ref, watch } from "vue";

import Alert from "@/components/ui/Alert.vue";
import Badge from "@/components/ui/Badge.vue";
import Button from "@/components/ui/Button.vue";
import CodeEditor from "@/components/ui/CodeEditor.vue";
import EmptyState from "@/components/ui/EmptyState.vue";
import Input from "@/components/ui/Input.vue";
import Pagination from "@/components/ui/Pagination.vue";
import { appApi, errMsg, traceApi, type TraceDetail, type TraceEntry } from "@/lib/api";
import { formatBytes, formatLatency, formatTimeMs, formatTokens } from "@/lib/utils";
import { useAutoPageSize } from "@/composables/useAutoPageSize";
import { useConfirm } from "@/composables/useConfirm";
import { usePointerDrag } from "@/composables/usePointerDrag";
import { useToast } from "@/composables/useToast";
import { useWheelPaging } from "@/composables/useWheelPaging";

const { confirm } = useConfirm();
const toast = useToast();
const entries = ref<TraceEntry[]>([]);
const error = ref<string | null>(null);
const loading = ref(false);
const filter = ref("");
const selected = ref<TraceEntry | null>(null);
const detail = ref<TraceDetail | null>(null);
const detailLoading = ref(false);
const traceDir = ref("");

/** TPS：输出 token / 生成秒；生成时间 = 总延迟 − TTFT，非流式或无 TTFT 为 null。 */
const tpsText = computed(() => {
  const d = detail.value;
  if (!d?.ttftMs || d.latencyMs <= d.ttftMs) return "—";
  const genSec = (d.latencyMs - d.ttftMs) / 1000;
  return `${(d.usage.outputTokens / genSec).toFixed(1)} tok/s`;
});

/** 超过该体积的报文不走 CodeMirror 高亮（大 JSON 语法解析成本高），回退纯文本 <pre>。 */
const HIGHLIGHT_LIMIT = 256 * 1024;
const encoder = new TextEncoder();

/** 报文区数据：旧 trace 无响应快照且 error 存有响应体时，回退展示到「上游响应」。
 *  pretty 结果与行数在此一次性算好，避免模板里对 MB 级报文重复 stringify/split。 */
const sections = computed(() => {
  const d = detail.value;
  if (!d) return [];
  const upstreamResponse =
    d.upstreamResponse ?? (d.error ? { traceError: d.error } : null);
  return [
    { title: "客户端请求", body: d.clientRequest },
    { title: "上游请求", body: d.upstreamRequest },
    { title: "上游响应", body: upstreamResponse },
    { title: "客户端响应", body: d.clientResponse },
  ].map((s) => {
    const text = pretty(s.body);
    return {
      ...s,
      text,
      bytes: encoder.encode(text).length,
      big: text.length > HIGHLIGHT_LIMIT,
      lines: text.split("\n").length,
      /** null 报文：记录被关闭、旧版流式 trace 未聚合，或该段本无内容。 */
      empty: s.body === null || s.body === undefined,
    };
  });
});

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

// ── 列表分页（页大小随可视高度自适应）──
const listScroll = ref<HTMLElement | null>(null);
const page = ref(1);
const { pageSize } = useAutoPageSize(listScroll, page, "li");
const pageCount = computed(() => Math.max(1, Math.ceil(filtered.value.length / pageSize.value)));
const paged = computed(() =>
  filtered.value.slice((page.value - 1) * pageSize.value, page.value * pageSize.value),
);
watch(pageCount, (n) => {
  if (page.value > n) page.value = n;
});
watch(filter, () => {
  page.value = 1;
});
useWheelPaging(listScroll, {
  canPrev: () => page.value > 1,
  canNext: () => page.value < pageCount.value,
  prev: () => page.value--,
  next: () => page.value++,
});

/** silent=true 用于 keep-alive 切回时的后台重拉：不点亮 refresh 图标，避免每次进页面都闪一次 loading。 */
async function load(silent = false) {
  loading.value = !silent;
  error.value = null;
  try {
    const [list, info] = await Promise.all([
      traceApi.list(500),
      // 目录信息仅作展示，失败不影响列表
      appApi.info().catch(() => null),
    ]);
    entries.value = list;
    if (info) traceDir.value = info.traceDir;
  } catch (e) {
    error.value = errMsg(e);
  } finally {
    loading.value = false;
  }
}

async function open(entry: TraceEntry, scroll = false) {
  selected.value = entry;
  detail.value = null;
  detailLoading.value = true;
  if (scroll) {
    // 选中项不在当前页时先把页号跟过去，渲染后再 scrollIntoView
    const i = filtered.value.findIndex((x) => x.relPath === entry.relPath);
    if (i >= 0) page.value = Math.floor(i / pageSize.value) + 1;
    await nextTick();
    listScroll.value
      ?.querySelector('[data-selected="true"]')
      ?.scrollIntoView({ block: "nearest" });
  }
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
    toast.success("trace 已删除");
    if (selected.value?.relPath === entry.relPath) {
      selected.value = null;
      detail.value = null;
    }
    // 删除只影响这一行：本地移除，无需重拉整表
    entries.value = entries.value.filter((x) => x.relPath !== entry.relPath);
  } catch (e) {
    error.value = errMsg(e);
  }
}

function pretty(v: unknown): string {
  if (v === null || v === undefined) return "—";
  // 字符串原样输出（SSE 报文等），不再套 JSON 引号转义
  if (typeof v === "string") return v;
  try {
    return JSON.stringify(v, null, 2);
  } catch {
    return String(v);
  }
}

/** 报文编辑器高度：按行数自适应，限制在 [6rem, 18rem]，小 body 不留大片空白。 */
function editorHeight(lines: number): string {
  const px = Math.min(288, Math.max(96, lines * 17 + 24));
  return `${px}px`;
}

// ── 左栏拖拽调宽（240–560px，持久化） ──
const LIST_WIDTH_KEY = "traces-list-width";
const listWidth = ref(
  Math.min(560, Math.max(240, Number(localStorage.getItem(LIST_WIDTH_KEY)) || 360)),
);
const listEl = ref<HTMLElement | null>(null);

let startW = listWidth.value;
const startResize = usePointerDrag(
  (dx) => {
    listWidth.value = Math.min(560, Math.max(240, startW + dx));
  },
  {
    onStart: () => {
      startW = listWidth.value;
    },
    onEnd: () => localStorage.setItem(LIST_WIDTH_KEY, String(listWidth.value)),
  },
);

// ── 键盘导航：↑/↓ 在过滤后的列表中移动选中（输入框/编辑器内不劫持） ──
function onKey(e: KeyboardEvent) {
  if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
  const t = e.target as HTMLElement | null;
  if (t?.closest("input, textarea, select, [contenteditable], .cm-editor")) return;
  const list = filtered.value;
  if (list.length === 0) return;
  e.preventDefault();
  const i = list.findIndex((x) => x.relPath === selected.value?.relPath);
  const next =
    i < 0 ? 0 : e.key === "ArrowDown" ? Math.min(list.length - 1, i + 1) : Math.max(0, i - 1);
  void open(list[next], true);
}

onMounted(async () => {
  document.addEventListener("keydown", onKey);
  await load();
});

// keep-alive 下切回本页：恢复键盘导航，第二次起静默重拉，选中项与详情保持不动
let activated = false;
onActivated(() => {
  document.addEventListener("keydown", onKey);
  if (activated) void load(true);
  activated = true;
});

// 视图被缓存后不再卸载：键盘导航监听改在停用时移除，避免离开本页仍劫持上下键
onDeactivated(() => document.removeEventListener("keydown", onKey));

onUnmounted(() => document.removeEventListener("keydown", onKey));
</script>

<template>
  <div class="flex h-full min-h-0 flex-col gap-4">
    <Alert v-if="error" class="shrink-0">{{ error }}</Alert>

    <!-- 主从面板：左列表（可拖宽） / 右详情，高度填满视口 -->
    <div class="flex min-h-0 flex-1 overflow-hidden">
      <section ref="listEl" class="flex min-h-0 shrink-0 flex-col" :style="{ width: listWidth + 'px' }">
        <div class="flex shrink-0 items-center gap-2 border-b p-3" :title="traceDir || undefined">
          <Input v-model="filter" placeholder="按会话 / 模型 / 文件名过滤…(↑↓ 切换)" class="flex-1" />
          <Button
            variant="ghost"
            size="icon"
            class="size-8 shrink-0"
            :disabled="loading"
            title="刷新"
            @click="load()"
          >
            <RefreshCw class="size-4" :class="loading && 'animate-spin'" />
          </Button>
        </div>
        <div ref="listScroll" class="scrollbar-thin min-h-0 flex-1 overflow-y-auto p-3">
          <EmptyState v-if="loading && filtered.length === 0" class="p-6">加载中…</EmptyState>
          <EmptyState v-else-if="filtered.length === 0" :icon="ScrollText" class="p-6">
            暂无 trace。启动网关并发起请求后将在此显示。
          </EmptyState>
          <ul v-else class="space-y-1.5">
            <li
              v-for="e in paged"
              :key="e.relPath"
              :data-selected="selected?.relPath === e.relPath"
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
                  class="shrink-0 text-muted-foreground opacity-0 transition-opacity hover:text-destructive focus:opacity-100 group-hover:opacity-100"
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
        <div v-if="pageCount > 1" class="shrink-0 border-t px-3 py-2">
          <Pagination v-model:page="page" :page-count="pageCount" :total="filtered.length" />
        </div>
      </section>

      <div
        class="w-1 shrink-0 cursor-col-resize border-l transition-colors hover:bg-primary/40 active:bg-primary/60"
        title="拖拽调整列表宽度"
        @pointerdown="startResize"
      />

      <section class="flex min-h-0 min-w-0 flex-1 flex-col">
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
                <dt class="text-muted-foreground">TTFT</dt>
                <dd class="font-mono">
                  {{ detail.ttftMs != null ? formatLatency(detail.ttftMs) : "—" }}
                </dd>
                <dt class="text-muted-foreground">TPS</dt>
                <dd class="font-mono">{{ tpsText }}</dd>
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

              <!-- 各阶段报文：小报文只读高亮，大报文回退纯文本（快得多） -->
              <section v-for="sec in sections" :key="sec.title">
                <h4 class="mb-1 flex items-baseline justify-between text-xs font-semibold text-muted-foreground">
                  {{ sec.title }}
                  <span v-if="!sec.empty" class="font-normal">
                    {{ formatBytes(sec.bytes) }}
                    <template v-if="sec.big">· 过大，已关闭高亮</template>
                  </span>
                </h4>
                <div
                  v-if="sec.empty"
                  class="rounded-md border border-dashed px-3 py-2.5 text-xs text-muted-foreground"
                >
                  {{ detail.stream ? "未记录（旧版 trace / 记录已关闭 / 流未产出内容）" : "未记录" }}
                </div>
                <pre
                  v-else-if="sec.big"
                  class="scrollbar-thin max-h-96 overflow-auto whitespace-pre rounded-md bg-muted p-3 font-mono text-[11px] leading-relaxed"
                  >{{ sec.text }}</pre
                >
                <CodeEditor
                  v-else
                  :model-value="sec.text"
                  lang="json"
                  readonly
                  :height="editorHeight(sec.lines)"
                />
              </section>
            </div>
          </div>
        </template>
      </section>
    </div>
  </div>
</template>
