<script setup lang="ts">
import { ArrowLeft, ChevronDown, Pencil, Plus, Power, Puzzle, RotateCw, Trash2, Upload } from "lucide-vue-next";
import { computed, onActivated, onMounted, reactive, ref, watch } from "vue";

import Alert from "@/components/ui/Alert.vue";
import Badge from "@/components/ui/Badge.vue";
import Button from "@/components/ui/Button.vue";
import Checkbox from "@/components/ui/Checkbox.vue";
import EmptyState from "@/components/ui/EmptyState.vue";
import Input from "@/components/ui/Input.vue";
import Label from "@/components/ui/Label.vue";
import CodeEditor from "@/components/ui/CodeEditor.vue";
import Modal from "@/components/ui/Modal.vue";
import Select from "@/components/ui/Select.vue";
import Switch from "@/components/ui/Switch.vue";
import TabBar from "@/components/ui/TabBar.vue";
import { useConfirm } from "@/composables/useConfirm";
import { useToast } from "@/composables/useToast";
import { errMsg, isTauriRuntime, isWebRuntime, pluginApi, type PluginImportOutcome, type PluginRecord } from "@/lib/api";
import { importPluginFiles } from "@/lib/web";
import { useGatewayStore } from "@/stores/gateway";

const gateway = useGatewayStore();
const { confirm } = useConfirm();
const toast = useToast();

const CAPABILITIES = ["core", "raw_request", "raw_response", "raw_stream"] as const;
const SCOPES = ["global", "provider", "model", "route"] as const;

const DEFAULT_SCRIPT = `-- Moon Bridge Next 插件
-- 暴露全局 MB 表：既承载清单，也承载钩子；程序提供的 API 挂载在全局 mb（小写）。
MB = {
  version = "0.1.0",
  scopes = { "global" },
  capabilities = { "core" },
}

-- ctx: {request_id, session_id, model_alias, client_protocol, upstream_protocol, provider, stream}
-- req: CoreRequest（就地修改即生效，亦可 return 新 table）
function MB.on_request(ctx, req)
  mb.log.info(string.format("[on_request] model=%s", req.model or "-"))
  return req
end
`;

/** 配额插件的默认脚本骨架：契约是 MB.query(ctx)，配额必须带 type 判别。 */
const QUOTA_DEFAULT_SCRIPT = `-- Moon Bridge Next 配额查询插件
-- 配额引擎对 Provider 的每个端点 Key 各调一次 MB.query：
--   ctx = { name, key, keys = {key}, base_url（该端点地址）, provider, extra（配额配置） }
-- 返回 { status = "ok", quotas = {...}, summary = "..." }；quotas 每项必须带 type：
--   percentage: used_percent / left_percent（0-100，可附 period_secs / reset_at）
--   quota:      unit + used_amount / left_amount
--   counter:    unit + used_amount（无界计数，看板不展示）
MB = {
  category = "quota",
}

function MB.query(ctx)
  local r = mb.http.request({
    method = "GET",
    url = ctx.base_url .. "/quota",
    headers = { { "Authorization", "Bearer " .. ctx.key } },
  })
  if r.status ~= 200 then
    return { status = "error", message = "上游 " .. r.status }
  end
  return {
    status = "ok",
    quotas = { { type = "percentage", label = "额度", left_percent = r.body.left } },
  }
end
`;

const plugins = ref<PluginRecord[]>([]);
const error = ref<string | null>(null);
const needsRestart = ref(false);
const editing = ref(false);
const isNew = ref(false);
const busy = ref(false);
// 元信息/脚本为互斥手风琴：同时至多一个展开（也可全收起），切换时 flex-grow 过渡
const metaOpen = ref(false);
const scriptOpen = ref(true);
function toggleMeta() {
  metaOpen.value = !metaOpen.value;
  if (metaOpen.value) scriptOpen.value = false;
}
function toggleScript() {
  scriptOpen.value = !scriptOpen.value;
  if (scriptOpen.value) metaOpen.value = false;
}

interface Form {
  name: string;
  scriptRef: string;
  enabled: boolean;
  configText: string;
  category: string;
  scopes: string[];
  capabilities: string[];
}

const form = reactive<Form>({
  name: "",
  scriptRef: "",
  enabled: true,
  configText: "{}",
  category: "core",
  scopes: ["global"],
  capabilities: ["core"],
});
const script = ref(DEFAULT_SCRIPT);

// 切换类别时，脚本仍是某一套默认骨架则换成对应骨架；用户改过则不动。
watch(
  () => form.category,
  (category) => {
    if (script.value === DEFAULT_SCRIPT && category === "quota") script.value = QUOTA_DEFAULT_SCRIPT;
    else if (script.value === QUOTA_DEFAULT_SCRIPT && category === "core") script.value = DEFAULT_SCRIPT;
  },
);

// ── 编辑器未保存确认：进入编辑时拍快照，返回/取消时比对 ──
const editorSnapshot = ref("");
const editorDirty = computed(
  () => JSON.stringify({ f: form, s: script.value }) !== editorSnapshot.value,
);

function takeEditorSnapshot() {
  editorSnapshot.value = JSON.stringify({ f: form, s: script.value });
}

async function tryCloseEditor() {
  if (
    !editorDirty.value ||
    (await confirm({
      title: "关闭编辑",
      message: "有未保存的修改，确认丢弃并返回列表？",
      confirmText: "丢弃修改",
    }))
  ) {
    editing.value = false;
  }
}

async function load() {
  try {
    plugins.value = await pluginApi.list();
  } catch (e) {
    error.value = errMsg(e);
  }
}

/** 类别 tab：core（请求链路）与 quota（配额查询）是两类运维对象；
 *  「全部」为平铺视图。未知类别追加为额外 tab。 */
const CATEGORY_LABELS: Record<string, string> = { core: "请求链路", quota: "配额查询" };
const activeCategory = ref("");
const categoryTabs = computed(() => {
  const cats = [...new Set(plugins.value.map((p) => p.category ?? "core"))].sort(
    (a, b) => (a === "core" ? 0 : a === "quota" ? 1 : 2) - (b === "core" ? 0 : b === "quota" ? 1 : 2) || a.localeCompare(b),
  );
  return [
    { key: "", label: "全部", count: plugins.value.length },
    ...cats.map((c) => ({
      key: c,
      label: CATEGORY_LABELS[c] ?? c,
      count: plugins.value.filter((p) => (p.category ?? "core") === c).length,
    })),
  ];
});
const tabbedPlugins = computed(() =>
  activeCategory.value === ""
    ? plugins.value
    : plugins.value.filter((p) => (p.category ?? "core") === activeCategory.value),
);

function toggleArr(arr: string[], v: string) {
  const i = arr.indexOf(v);
  if (i >= 0) arr.splice(i, 1);
  else arr.push(v);
}

function newPlugin() {
  isNew.value = true;
  Object.assign(form, {
    name: "",
    scriptRef: "",
    enabled: true,
    configText: "{}",
    category: "core",
    scopes: ["global"],
    capabilities: ["core"],
  });
  script.value = DEFAULT_SCRIPT;
  error.value = null;
  editing.value = true;
  takeEditorSnapshot();
}

async function editPlugin(p: PluginRecord) {
  isNew.value = false;
  Object.assign(form, {
    name: p.name,
    scriptRef: p.scriptRef,
    enabled: p.enabled,
    configText: JSON.stringify(p.config ?? {}, null, 2),
    category: p.category ?? "core",
    scopes: [...p.scopes],
    capabilities: [...p.capabilities],
  });
  error.value = null;
  editing.value = true;
  try {
    script.value = await pluginApi.readScript(p.name);
  } catch (e) {
    error.value = errMsg(e);
    script.value = "";
  } finally {
    takeEditorSnapshot();
  }
}

async function save() {
  error.value = null;
  const name = form.name.trim();
  if (!name) {
    error.value = "插件名不能为空";
    return;
  }
  let config: unknown;
  try {
    config = JSON.parse(form.configText || "{}");
  } catch {
    error.value = "配置不是合法 JSON";
    return;
  }
  const scriptRef = form.scriptRef.trim() || `${name}.lua`;
  const record: PluginRecord = {
    name,
    source: "lua",
    scriptRef,
    enabled: form.enabled,
    config,
    scopes: [...form.scopes],
    capabilities: [...form.capabilities],
    category: form.category,
  };
  busy.value = true;
  try {
    await pluginApi.save(record);
    await pluginApi.writeScript(name, script.value);
    editing.value = false;
    needsRestart.value = true;
    toast.success(`插件 “${name}” 已保存，重启网关后生效`);
    // 保存记录即本地已构造的这份数据：原地增改，无需重拉整表
    plugins.value = plugins.value.some((x) => x.name === name)
      ? plugins.value.map((x) => (x.name === name ? record : x))
      : [...plugins.value, record];
  } catch (e) {
    handleSaveError(e);
  } finally {
    busy.value = false;
  }
}

/** 启用条件：`MB.requires` 未满足时后端返回该前缀的结构化错误，弹提示框而非横幅。 */
const REQUIREMENTS_PREFIX = "REQUIREMENTS";
const reqError = ref<string[] | null>(null);

function handleSaveError(e: unknown) {
  const msg = errMsg(e);
  if (msg.startsWith(REQUIREMENTS_PREFIX)) {
    reqError.value = msg.slice(REQUIREMENTS_PREFIX.length).split("\n").filter(Boolean);
  } else {
    error.value = msg;
  }
}

async function toggleEnabled(p: PluginRecord) {
  error.value = null;
  try {
    await pluginApi.save({ ...p, enabled: !p.enabled });
    needsRestart.value = true;
    // 本地已知的新状态即最终状态：原地替换，避免整表重拉
    plugins.value = plugins.value.map((x) => (x.name === p.name ? { ...x, enabled: !p.enabled } : x));
  } catch (e) {
    handleSaveError(e);
  }
}

async function remove(p: PluginRecord) {
  if (!(await confirm({ title: "删除插件", message: `确认删除插件 “${p.name}”？此操作不可撤销。` }))) return;
  error.value = null;
  try {
    await pluginApi.remove(p.name);
    needsRestart.value = true;
    // 删除只影响这一行：本地移除，无需重拉整表
    plugins.value = plugins.value.filter((x) => x.name !== p.name);
  } catch (e) {
    error.value = errMsg(e);
  }
}

async function restart() {
  error.value = null;
  await gateway.restart();
  if (gateway.error) {
    error.value = gateway.error;
  } else {
    needsRestart.value = false;
    toast.success("网关已重启，插件变更已生效");
  }
}

// ── 导入插件：Tauri 走原生文件对话框（磁盘路径）；浏览器模式用文件选择器读内容 ──
const fileInput = ref<HTMLInputElement | null>(null);

function reportImport(outcomes: PluginImportOutcome[]) {
  const imported = outcomes.filter((o) => o.status === "imported");
  const skipped = outcomes.filter((o) => o.status === "skipped");
  const failed = outcomes.filter((o) => o.status === "error");
  if (imported.length > 0) needsRestart.value = true;
  if (failed.length > 0) {
    const ok = [
      imported.length ? `成功导入 ${imported.length} 个` : "",
      skipped.length ? `跳过 ${skipped.length} 个同名插件` : "",
    ]
      .filter(Boolean)
      .join("，");
    error.value = [ok ? `${ok}；` : "", ...failed.map((o) => `${o.name}：${o.message ?? "导入失败"}`)].join("");
    return;
  }
  const parts = [
    imported.length ? `成功导入 ${imported.map((o) => o.name).join("、")}` : "",
    skipped.length ? `跳过 ${skipped.length} 个同名插件` : "",
  ].filter(Boolean);
  if (parts.length) toast.success(`${parts.join("；")}。重启网关后生效`);
}

async function importPlugins() {
  error.value = null;
  if (isTauriRuntime) {
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const picked = await open({
        multiple: true,
        filters: [{ name: "Lua 插件", extensions: ["lua"] }],
      });
      if (!picked) return;
      const paths = Array.isArray(picked) ? picked : [picked];
      busy.value = true;
      try {
        reportImport(await pluginApi.import(paths));
        await load();
      } finally {
        busy.value = false;
      }
    } catch (e) {
      error.value = errMsg(e);
    }
  } else {
    fileInput.value?.click();
  }
}

async function onFilesPicked(e: Event) {
  const input = e.target as HTMLInputElement;
  const files = Array.from(input.files ?? []);
  input.value = ""; // 允许重复选择同一文件
  if (files.length === 0) return;
  busy.value = true;
  const outcomes: PluginImportOutcome[] = [];
  try {
    if (isWebRuntime) {
      // web 模式：无磁盘路径，改由后端按文件内容导入（同名跳过、结果形状一致）
      const payload = await Promise.all(
        files.map(async (f) => ({ name: f.name, content: await f.text() })),
      );
      outcomes.push(...(await importPluginFiles(payload)));
      reportImport(outcomes);
      await load();
      return;
    }
    for (const file of files) {
      const name = file.name.replace(/\.lua$/i, "");
      const path = file.name;
      try {
        if (await pluginApi.get(name)) {
          outcomes.push({ path, name, status: "skipped", message: "同名插件已存在" });
          continue;
        }
        const content = await file.text();
        await pluginApi.save({
          name,
          source: "lua",
          scriptRef: `${name}.lua`,
          enabled: true,
          config: null,
          scopes: ["global"],
          capabilities: ["core"],
        });
        await pluginApi.writeScript(name, content);
        outcomes.push({ path, name, status: "imported", message: null });
      } catch (err) {
        outcomes.push({ path, name, status: "error", message: errMsg(err) });
      }
    }
    reportImport(outcomes);
    await load();
  } finally {
    busy.value = false;
  }
}

onMounted(() => {
  load();
  gateway.refresh();
});

// keep-alive 下切回本页：编辑器打开时不动（避免覆盖未保存草稿），否则静默重拉
let activated = false;
onActivated(() => {
  if (activated && !editing.value) void load();
  activated = true;
});
</script>

<template>
  <div class="flex h-full min-h-0 flex-col">
    <Alert v-if="error" class="shrink-0">{{ error }}</Alert>
    <Alert v-if="needsRestart" variant="warning" class="shrink-0">
      有插件变更尚未生效——点击列表底部「重启网关」应用。
    </Alert>

    <!-- 编辑器模式：满页切换，不再弹窗；列表↔编辑器进出用淡出/淡入衔接 -->
    <Transition name="editor" mode="out-in">
    <div v-if="editing" key="editor" class="flex min-h-0 flex-1 flex-col overflow-hidden">
      <div class="flex shrink-0 items-center justify-between border-b px-5 py-3">
        <div class="flex items-center gap-2">
          <Button variant="ghost" size="icon" class="size-7" title="返回列表" @click="tryCloseEditor">
            <ArrowLeft class="size-4" />
          </Button>
          <h3 class="card-title">{{ isNew ? "新建插件" : "编辑插件" }}</h3>
        </div>
        <div class="flex items-center gap-3">
          <label class="flex items-center gap-1.5 text-xs text-muted-foreground">
            <Switch :checked="form.enabled" @update:checked="(v: boolean) => (form.enabled = v)" />
            启用
          </label>
          <Button variant="ghost" size="sm" @click="tryCloseEditor">取消</Button>
          <Button size="sm" :disabled="busy" @click="save">
            {{ busy ? "保存中…" : "保存" }}
          </Button>
        </div>
      </div>

      <!-- 元信息：可折叠，默认收起，把空间留给脚本 -->
      <button
        class="flex w-full shrink-0 items-center justify-between px-5 py-2.5"
        :class="!metaOpen && 'border-b'"
        @click="toggleMeta"
      >
        <span class="card-title">元信息</span>
        <ChevronDown
          class="size-4 text-muted-foreground transition-transform"
          :class="metaOpen ? '' : '-rotate-90'"
        />
      </button>
      <div class="collapse-body" :class="metaOpen && 'collapse-open border-b'">
      <div class="grid h-full gap-4 overflow-y-auto px-5 py-4 grid-cols-2 grid-rows-[auto_auto_1fr]">
        <div class="space-y-1.5">
          <Label for="pl-name">名称</Label>
          <Input id="pl-name" v-model="form.name" placeholder="如 my_plugin" :disabled="!isNew" />
        </div>
        <div class="space-y-1.5">
          <Label for="pl-ref">脚本引用</Label>
          <Input id="pl-ref" v-model="form.scriptRef" placeholder="留空则为 my_plugin.lua" />
        </div>
        <div class="space-y-1.5">
          <Label for="pl-category">类别</Label>
          <Select
            id="pl-category"
            v-model="form.category"
            :options="[
              { value: 'core', label: 'core · 请求链路' },
              { value: 'quota', label: 'quota · 配额查询' },
            ]"
          />
        </div>
        <div v-if="form.category === 'core'" class="space-y-1.5">
          <Label>能力</Label>
          <div class="flex flex-wrap gap-x-4 gap-y-2 pt-1.5">
            <label
              v-for="c in CAPABILITIES"
              :key="c"
              class="flex cursor-pointer items-center gap-1.5 text-sm"
            >
              <Checkbox
                :checked="form.capabilities.includes(c)"
                @update:checked="toggleArr(form.capabilities, c)"
              />
              <span class="font-mono text-xs">{{ c }}</span>
            </label>
          </div>
        </div>
        <div v-if="form.category === 'core'" class="space-y-1.5">
          <Label>作用域</Label>
          <div class="flex flex-wrap gap-x-4 gap-y-2 pt-1.5">
            <label
              v-for="s in SCOPES"
              :key="s"
              class="flex cursor-pointer items-center gap-1.5 text-sm"
            >
              <Checkbox
                :checked="form.scopes.includes(s)"
                @update:checked="toggleArr(form.scopes, s)"
              />
              <span class="font-mono text-xs">{{ s }}</span>
            </label>
          </div>
        </div>
        <div class="flex min-h-0 flex-col space-y-1.5 col-span-2">
          <Label>配置</Label>
          <CodeEditor v-model="form.configText" lang="json" height="100%" class="min-h-32 flex-1" />
        </div>
      </div>
      </div>

      <!-- 脚本：可折叠，默认展开并填满剩余高度 -->
      <button
        class="flex w-full shrink-0 items-center justify-between px-5 py-2.5"
        :class="scriptOpen && 'border-b'"
        @click="toggleScript"
      >
        <span class="card-title">脚本</span>
        <ChevronDown
          class="size-4 text-muted-foreground transition-transform"
          :class="scriptOpen ? '' : '-rotate-90'"
        />
      </button>
      <div class="collapse-body" :class="scriptOpen && 'collapse-open'">
        <div class="flex h-full flex-col px-5 py-4">
          <CodeEditor v-model="script" height="100%" class="min-h-0 flex-1" />
        </div>
      </div>
    </div>

    <div v-else key="list" class="flex min-h-0 flex-1 flex-col overflow-hidden">
      <input ref="fileInput" type="file" accept=".lua" multiple class="hidden" @change="onFilesPicked" />
      <TabBar v-if="plugins.length > 0" v-model="activeCategory" :tabs="categoryTabs" />
      <div class="scrollbar-thin min-h-0 flex-1 overflow-y-auto px-5 py-4">
        <EmptyState v-if="plugins.length === 0" :icon="Puzzle">
          <p>
            暂无已注册插件。示例见仓库
            <code class="font-mono">plugins/examples/</code>。
          </p>
          <template #action>
            <Button size="sm" @click="newPlugin"><Plus class="size-4" /> 新建插件</Button>
            <Button variant="outline" size="sm" :disabled="busy" @click="importPlugins">
              <Upload class="size-4" /> 导入
            </Button>
          </template>
        </EmptyState>
        <EmptyState v-else-if="tabbedPlugins.length === 0" class="p-6">
          该类别下暂无插件。
        </EmptyState>
        <ul v-else class="divide-y">
          <li v-for="p in tabbedPlugins" :key="p.name" class="py-3 first:pt-0">
            <div class="flex items-center justify-between gap-2">
              <div class="flex min-w-0 items-center gap-2">
                <span class="truncate font-medium">{{ p.name }}</span>
                <Badge v-if="p.category && p.category !== 'core'" variant="secondary" class="shrink-0">{{ p.category }}</Badge>
                <span class="shrink-0 font-mono text-xs text-muted-foreground">{{ p.source }}</span>
                <Badge :variant="p.enabled ? 'success' : 'secondary'" class="shrink-0">
                  {{ p.enabled ? "启用" : "停用" }}
                </Badge>
              </div>
              <div class="flex shrink-0 items-center gap-0.5">
                <Button variant="ghost" size="icon" class="size-7" :title="p.enabled ? '停用' : '启用'" @click="toggleEnabled(p)">
                  <Power class="size-3.5" />
                </Button>
                <Button variant="ghost" size="icon" class="size-7" title="编辑" @click="editPlugin(p)">
                  <Pencil class="size-3.5" />
                </Button>
                <Button variant="ghost" size="icon" class="size-7" title="删除" @click="remove(p)">
                  <Trash2 class="size-3.5 text-destructive" />
                </Button>
              </div>
            </div>
            <div class="mt-1 truncate font-mono text-xs text-muted-foreground" :title="p.scriptRef">
              {{ p.scriptRef }}
            </div>
            <!-- 能力/作用域只对 core 链路插件有意义；quota 插件不展示 -->
            <div v-if="(p.category ?? 'core') === 'core'" class="mt-1.5 flex flex-wrap gap-x-2 text-xs text-muted-foreground">
              <span v-if="p.capabilities.length" class="font-mono">{{ p.capabilities.join(" · ") }}</span>
              <span v-else>未声明 capabilities</span>
              <span>·</span>
              <span class="font-mono">{{ p.scopes.join(" · ") }}</span>
            </div>
          </li>
          <!-- 尾行：页级与条目操作（与模型页尾行同一惯例） -->
          <li class="py-1">
            <div class="flex items-center justify-end gap-4">
              <button
                type="button"
                class="flex items-center gap-1.5 rounded-sm py-1.5 text-xs text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring disabled:pointer-events-none disabled:opacity-50"
                :disabled="!needsRestart"
                @click="restart"
              >
                <RotateCw class="size-3.5" /> 重启网关
              </button>
              <button
                type="button"
                class="flex items-center gap-1.5 rounded-sm py-1.5 text-xs text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring disabled:pointer-events-none disabled:opacity-50"
                :disabled="busy"
                @click="importPlugins"
              >
                <Upload class="size-3.5" /> 导入
              </button>
              <button
                type="button"
                class="flex items-center gap-1.5 rounded-sm py-1.5 text-xs text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
                @click="newPlugin"
              >
                <Plus class="size-3.5" /> 新建插件
              </button>
            </div>
          </li>
        </ul>
      </div>
    </div>
    </Transition>

    <!-- 启用条件：插件 MB.requires 声明的设置未满足时弹框提示，不自动修改 -->
    <Modal :open="reqError !== null" title="无法启用插件" width="max-w-md" @close="reqError = null">
      <p class="text-sm text-muted-foreground">以下网关设置未满足插件要求，请在「设置」中调整后重试：</p>
      <ul class="mt-3 space-y-1.5 text-sm">
        <li v-for="r in reqError" :key="r" class="flex gap-2">
          <span class="shrink-0 text-destructive">•</span>
          <span>{{ r }}</span>
        </li>
      </ul>
      <template #footer>
        <Button variant="outline" size="sm" @click="reqError = null">知道了</Button>
      </template>
    </Modal>
  </div>
</template>

<style scoped>
/* 手风琴折叠区：flex-grow 可插值——互斥切换时一区收一区放同步进行，
   比 height/max-height 方案平滑且无硬编码数值。 */
.collapse-body {
  flex: 0 1 0%;
  min-height: 0;
  overflow: hidden;
  transition: flex-grow var(--dur-med) var(--ease-out);
}
.collapse-body.collapse-open {
  flex-grow: 1;
}
</style>
