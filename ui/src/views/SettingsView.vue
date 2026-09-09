<script setup lang="ts">
import { ChevronDown } from "lucide-vue-next";
import { onMounted, reactive, ref } from "vue";

import Button from "@/components/ui/Button.vue";
import Card from "@/components/ui/Card.vue";
import Input from "@/components/ui/Input.vue";
import Label from "@/components/ui/Label.vue";
import Select from "@/components/ui/Select.vue";
import { appApi, errMsg, type AppConfig, type AppInfo } from "@/lib/api";
import { useGatewayStore } from "@/stores/gateway";

const gateway = useGatewayStore();
const info = ref<AppInfo | null>(null);
const error = ref<string | null>(null);
const saved = ref(false);

const form = reactive<AppConfig>({
  gateway: {
    addr: "127.0.0.1:38440",
    authToken: null,
    egressProxy: null,
    maxBodyBytes: 8388608,
    requestTimeoutSecs: 300,
    traceDir: null,
    traceRecordBodies: true,
    traceRetention: 500,
  },
  logLevel: "info",
  autoStart: true,
});

const LOG_LEVELS = ["trace", "debug", "info", "warn", "error"];
const logOptions = LOG_LEVELS.map((l) => ({ value: l, label: l }));

/** 请求体大小：数值 + 单位下拉（内部换算为字节）。 */
const bodyUnitOptions = [
  { value: "1", label: "字节" },
  { value: "1024", label: "KB" },
  { value: "1048576", label: "MB" },
  { value: "1073741824", label: "GB" },
];
const bodyValue = ref("8");
const bodyUnit = ref("1048576");

/** 字节数 → 「数值 + 最大可整除单位」。 */
function bytesToUnit(b: number): { v: string; unit: string } {
  for (const u of [1073741824, 1048576, 1024]) {
    if (b >= u && b % u === 0) return { v: String(b / u), unit: String(u) };
  }
  return { v: String(b), unit: "1" };
}

/** 分区折叠状态，默认收起。 */
const gwOpen = ref(false);
const infoOpen = ref(false);

onMounted(async () => {
  try {
    const c = await appApi.getConfig();
    form.gateway = { ...c.gateway };
    form.logLevel = c.logLevel;
    form.autoStart = c.autoStart;
    const bu = bytesToUnit(c.gateway.maxBodyBytes);
    bodyValue.value = bu.v;
    bodyUnit.value = bu.unit;
    info.value = await appApi.info();
  } catch (e) {
    error.value = errMsg(e);
  }
});

function payload(): AppConfig {
  const n = Number(bodyValue.value);
  return {
    gateway: {
      ...form.gateway,
      maxBodyBytes: Number.isNaN(n) ? 0 : Math.round(n * Number(bodyUnit.value)),
      requestTimeoutSecs: Number(form.gateway.requestTimeoutSecs),
      traceRetention: Math.max(0, Math.floor(Number(form.gateway.traceRetention) || 0)),
    },
    logLevel: form.logLevel,
    autoStart: form.autoStart,
  };
}

async function save(restart = false) {
  try {
    await appApi.setConfig(payload());
    saved.value = true;
    setTimeout(() => (saved.value = false), 2000);
    if (restart) await gateway.restart();
    error.value = null;
  } catch (e) {
    error.value = errMsg(e);
  }
}
</script>

<template>
  <div class="space-y-4">
    <div
      v-if="error"
      class="rounded-md border border-destructive/40 bg-destructive/10 px-4 py-2 text-sm text-destructive"
    >
      {{ error }}
    </div>

    <Card>
      <button
        class="flex w-full items-center justify-between px-5 py-3"
        @click="gwOpen = !gwOpen"
      >
        <h3 class="card-title">网关</h3>
        <ChevronDown
          class="size-4 text-muted-foreground transition-transform"
          :class="gwOpen ? '' : '-rotate-90'"
        />
      </button>
      <div v-show="gwOpen">
        <div class="card-content grid gap-4 border-t grid-cols-2">
          <div class="space-y-1.5">
            <Label for="s-addr">监听地址</Label>
            <Input id="s-addr" v-model="form.gateway.addr" placeholder="127.0.0.1:38440" />
          </div>
          <div class="space-y-1.5">
            <Label>日志级别</Label>
            <Select v-model="form.logLevel" :options="logOptions" />
          </div>
          <div class="space-y-1.5">
            <Label for="s-token">Bearer Token</Label>
            <Input
              id="s-token"
              v-model="form.gateway.authToken"
              type="password"
              placeholder="留空则不鉴权"
            />
          </div>
          <div class="space-y-1.5">
            <Label for="s-proxy">出站代理</Label>
            <Input id="s-proxy" v-model="form.gateway.egressProxy" placeholder="http://127.0.0.1:7890" />
          </div>
          <div class="space-y-1.5">
            <Label for="s-body">请求体大小上限</Label>
            <div class="flex gap-2">
              <Input id="s-body" v-model="bodyValue" inputmode="numeric" class="flex-1" />
              <div class="w-28 shrink-0">
                <Select v-model="bodyUnit" :options="bodyUnitOptions" />
              </div>
            </div>
          </div>
          <div class="space-y-1.5">
            <Label for="s-timeout">上游超时</Label>
            <Input id="s-timeout" v-model="form.gateway.requestTimeoutSecs" type="number" placeholder="60" />
          </div>
          <div class="col-span-2 space-y-1.5">
            <Label for="s-retention">trace 保留条数</Label>
            <Input
              id="s-retention"
              v-model="form.gateway.traceRetention"
              type="number"
              min="0"
              placeholder="500"
            />
          </div>
          <div class="col-span-2 flex flex-col gap-2.5 border-t pt-4">
            <div class="flex items-center gap-2">
              <input
                id="s-bodies"
                v-model="form.gateway.traceRecordBodies"
                type="checkbox"
                class="size-4 accent-primary"
              />
              <Label for="s-bodies">trace 记录请求/响应体</Label>
            </div>
            <div class="flex items-center gap-2">
              <input id="s-auto" v-model="form.autoStart" type="checkbox" class="size-4 accent-primary" />
              <Label for="s-auto">应用启动时自动开启网关</Label>
            </div>
          </div>
        </div>
        <div class="flex shrink-0 justify-end gap-2 border-t px-5 py-3">
          <span v-if="saved" class="mr-auto text-sm text-emerald-400">已保存</span>
          <Button variant="outline" size="sm" @click="save(false)">保存</Button>
          <Button size="sm" @click="save(true)">保存并重启网关</Button>
        </div>
      </div>
    </Card>

    <Card>
      <button
        class="flex w-full items-center justify-between px-5 py-3"
        @click="infoOpen = !infoOpen"
      >
        <h3 class="card-title">应用信息</h3>
        <ChevronDown
          class="size-4 text-muted-foreground transition-transform"
          :class="infoOpen ? '' : '-rotate-90'"
        />
      </button>
      <div v-show="infoOpen">
        <div class="card-content space-y-1.5 border-t text-sm">
        <div class="flex justify-between gap-4">
          <span class="text-muted-foreground">版本</span>
          <span class="font-mono">{{ info?.version ?? "—" }}</span>
        </div>
        <div class="flex justify-between gap-4">
          <span class="text-muted-foreground">数据库</span>
          <span class="font-mono text-xs break-all">{{ info?.dbPath ?? "—" }}</span>
        </div>
        <div class="flex justify-between gap-4">
          <span class="text-muted-foreground">配置文件</span>
          <span class="font-mono text-xs break-all">{{ info?.configPath ?? "—" }}</span>
        </div>
        <div class="flex justify-between gap-4">
          <span class="text-muted-foreground">插件目录</span>
          <span class="font-mono text-xs break-all">{{ info?.pluginsDir ?? "—" }}</span>
        </div>
        </div>
      </div>
    </Card>
  </div>
</template>
