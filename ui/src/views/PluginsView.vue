<script setup lang="ts">
import { ArrowLeft, ChevronDown, Pencil, Plus, Power, RotateCw, Trash2, Upload } from "lucide-vue-next";
import { onMounted, reactive, ref } from "vue";

import Badge from "@/components/ui/Badge.vue";
import Button from "@/components/ui/Button.vue";
import Card from "@/components/ui/Card.vue";
import Input from "@/components/ui/Input.vue";
import Label from "@/components/ui/Label.vue";
import CodeEditor from "@/components/ui/CodeEditor.vue";
import Switch from "@/components/ui/Switch.vue";
import { useConfirm } from "@/composables/useConfirm";
import { errMsg, isTauriRuntime, pluginApi, type PluginImportOutcome, type PluginRecord } from "@/lib/api";
import { useGatewayStore } from "@/stores/gateway";

const gateway = useGatewayStore();
const { confirm } = useConfirm();

const CAPABILITIES = ["core", "raw_request", "raw_response", "raw_stream"] as const;
const SCOPES = ["global", "provider", "model", "route"] as const;

const DEFAULT_SCRIPT = `-- Moon Bridge Next 插件
-- 暴露全局 MB 表：既承载清单，也承载钩子；宿主 API 挂在全局 mb（小写）。
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

const plugins = ref<PluginRecord[]>([]);
const error = ref<string | null>(null);
const notice = ref<string | null>(null);
const needsRestart = ref(false);
const editing = ref(false);
const isNew = ref(false);
const busy = ref(false);
/** 编辑器内折叠状态：元信息默认收起，脚本默认展开。 */
const metaOpen = ref(false);
const scriptOpen = ref(true);

interface Form {
  name: string;
  scriptRef: string;
  enabled: boolean;
  configText: string;
  scopes: string[];
  capabilities: string[];
}

const form = reactive<Form>({
  name: "",
  scriptRef: "",
  enabled: true,
  configText: "{}",
  scopes: ["global"],
  capabilities: ["core"],
});
const script = ref(DEFAULT_SCRIPT);

async function load() {
  try {
    plugins.value = await pluginApi.list();
  } catch (e) {
    error.value = errMsg(e);
  }
}

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
    scopes: ["global"],
    capabilities: ["core"],
  });
  script.value = DEFAULT_SCRIPT;
  error.value = null;
  editing.value = true;
}

async function editPlugin(p: PluginRecord) {
  isNew.value = false;
  Object.assign(form, {
    name: p.name,
    scriptRef: p.scriptRef,
    enabled: p.enabled,
    configText: JSON.stringify(p.config ?? {}, null, 2),
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
  };
  busy.value = true;
  try {
    await pluginApi.save(record);
    await pluginApi.writeScript(name, script.value);
    editing.value = false;
    needsRestart.value = true;
    notice.value = `插件 “${name}” 已保存，重启网关后生效。`;
    await load();
  } catch (e) {
    error.value = errMsg(e);
  } finally {
    busy.value = false;
  }
}

async function toggleEnabled(p: PluginRecord) {
  error.value = null;
  try {
    await pluginApi.save({ ...p, enabled: !p.enabled });
    needsRestart.value = true;
    await load();
  } catch (e) {
    error.value = errMsg(e);
  }
}

async function remove(p: PluginRecord) {
  if (!(await confirm({ title: "删除插件", message: `确认删除插件 “${p.name}”？此操作不可撤销。` }))) return;
  error.value = null;
  try {
    await pluginApi.remove(p.name);
    needsRestart.value = true;
    await load();
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
    notice.value = "网关已重启，插件变更已生效。";
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
  if (parts.length) notice.value = `${parts.join("；")}。重启网关后生效。`;
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
</script>

<template>
  <div class="flex h-full min-h-0 flex-col">
    <div
      v-if="error"
      class="shrink-0 rounded-md border border-destructive/40 bg-destructive/10 px-4 py-2 text-sm text-destructive"
    >
      {{ error }}
    </div>
    <div
      v-if="notice && !error"
      class="flex shrink-0 items-center justify-between rounded-md border border-primary/40 bg-primary/10 px-4 py-2 text-sm"
    >
      <span>{{ notice }}</span>
      <button class="text-xs text-muted-foreground hover:text-foreground" @click="notice = null">
        关闭
      </button>
    </div>
    <div
      v-if="needsRestart"
      class="shrink-0 rounded-md border border-amber-500/40 bg-amber-500/10 px-4 py-2 text-sm text-amber-600 dark:text-amber-400"
    >
      有插件变更尚未生效——点击右上「重启网关」应用。
    </div>

    <!-- 编辑器模式：满页切换，不再弹窗 -->
    <Card v-if="editing" class="flex min-h-0 flex-1 flex-col overflow-hidden">
      <div class="flex shrink-0 items-center justify-between border-b px-5 py-3">
        <div class="flex items-center gap-2">
          <Button variant="ghost" size="icon" class="size-7" title="返回列表" @click="editing = false">
            <ArrowLeft class="size-4" />
          </Button>
          <h3 class="card-title">{{ isNew ? "新建插件" : "编辑插件" }}</h3>
        </div>
        <div class="flex items-center gap-3">
          <label class="flex items-center gap-1.5 text-xs text-muted-foreground">
            <Switch :checked="form.enabled" @update:checked="(v: boolean) => (form.enabled = v)" />
            启用
          </label>
          <Button variant="ghost" size="sm" @click="editing = false">取消</Button>
          <Button size="sm" :disabled="busy" @click="save">
            {{ busy ? "保存中…" : "保存" }}
          </Button>
        </div>
      </div>

      <!-- 元信息：可折叠，默认收起，把空间留给脚本 -->
      <button
        class="flex w-full shrink-0 items-center justify-between px-5 py-2.5"
        :class="!metaOpen && 'border-b'"
        @click="metaOpen = !metaOpen"
      >
        <span class="card-title">元信息</span>
        <ChevronDown
          class="size-4 text-muted-foreground transition-transform"
          :class="metaOpen ? '' : '-rotate-90'"
        />
      </button>
      <div
        v-show="metaOpen"
        class="grid gap-4 px-5 py-4 grid-cols-2 grid-rows-[auto_auto_1fr]"
        :class="scriptOpen ? 'shrink-0 border-b' : 'min-h-0 flex-1 overflow-y-auto'"
      >
        <div class="space-y-1.5">
          <Label for="pl-name">名称</Label>
          <Input id="pl-name" v-model="form.name" placeholder="如 my_plugin" :disabled="!isNew" />
        </div>
        <div class="space-y-1.5">
          <Label for="pl-ref">脚本引用</Label>
          <Input id="pl-ref" v-model="form.scriptRef" placeholder="留空则为 my_plugin.lua" />
        </div>
        <div class="space-y-1.5">
          <Label>能力</Label>
          <div class="flex flex-wrap gap-x-4 gap-y-2 pt-1.5">
            <label
              v-for="c in CAPABILITIES"
              :key="c"
              class="flex cursor-pointer items-center gap-1.5 text-sm"
            >
              <input
                type="checkbox"
                class="size-4 accent-primary"
                :checked="form.capabilities.includes(c)"
                @change="toggleArr(form.capabilities, c)"
              />
              <span class="font-mono text-xs">{{ c }}</span>
            </label>
          </div>
        </div>
        <div class="space-y-1.5">
          <Label>作用域</Label>
          <div class="flex flex-wrap gap-x-4 gap-y-2 pt-1.5">
            <label
              v-for="s in SCOPES"
              :key="s"
              class="flex cursor-pointer items-center gap-1.5 text-sm"
            >
              <input
                type="checkbox"
                class="size-4 accent-primary"
                :checked="form.scopes.includes(s)"
                @change="toggleArr(form.scopes, s)"
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

      <!-- 脚本：可折叠，默认展开并填满剩余高度 -->
      <button
        class="flex w-full shrink-0 items-center justify-between px-5 py-2.5"
        :class="scriptOpen && 'border-b'"
        @click="scriptOpen = !scriptOpen"
      >
        <span class="card-title">脚本</span>
        <ChevronDown
          class="size-4 text-muted-foreground transition-transform"
          :class="scriptOpen ? '' : '-rotate-90'"
        />
      </button>
      <div v-show="scriptOpen" class="flex min-h-0 flex-1 flex-col px-5 py-4">
        <CodeEditor v-model="script" height="100%" class="min-h-0 flex-1" />
      </div>
    </Card>

    <!-- 插件列表：满版单卡 -->
    <Card v-else class="flex min-h-0 flex-1 flex-col overflow-hidden">
      <div class="flex shrink-0 items-center justify-end gap-2 border-b px-5 py-3">
        <Button variant="outline" size="sm" :disabled="!needsRestart" @click="restart">
          <RotateCw class="size-4" /> 重启网关
        </Button>
        <Button variant="outline" size="sm" :disabled="busy" @click="importPlugins">
          <Upload class="size-4" /> 导入
        </Button>
        <Button size="sm" @click="newPlugin">
          <Plus class="size-4" /> 新建
        </Button>
        <input
          ref="fileInput"
          type="file"
          accept=".lua"
          multiple
          class="hidden"
          @change="onFilesPicked"
        />
      </div>
      <div class="scrollbar-thin min-h-0 flex-1 overflow-y-auto px-5 py-4">
        <div
          v-if="plugins.length === 0"
          class="rounded-md border border-dashed p-8 text-center text-sm text-muted-foreground"
        >
          暂无已注册插件，点击「新建」创建。示例见仓库
          <code class="font-mono">plugins/examples/</code>。
        </div>
        <ul v-else class="divide-y">
          <li v-for="p in plugins" :key="p.name" class="py-3 first:pt-0 last:pb-0">
            <div class="flex items-center justify-between gap-2">
              <div class="flex min-w-0 items-center gap-2">
                <span class="truncate font-medium">{{ p.name }}</span>
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
            <div class="mt-1.5 flex flex-wrap gap-x-2 text-xs text-muted-foreground">
              <span v-if="p.capabilities.length" class="font-mono">{{ p.capabilities.join(" · ") }}</span>
              <span v-else>未声明 capabilities</span>
              <span>·</span>
              <span class="font-mono">{{ p.scopes.join(" · ") }}</span>
            </div>
          </li>
        </ul>
      </div>
    </Card>
  </div>
</template>
