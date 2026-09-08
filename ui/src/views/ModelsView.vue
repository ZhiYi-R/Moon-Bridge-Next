<script setup lang="ts">
import { Pencil, Plus, Trash2 } from "lucide-vue-next";
import { computed, onMounted, reactive, ref } from "vue";

import Button from "@/components/ui/Button.vue";
import Card from "@/components/ui/Card.vue";
import Input from "@/components/ui/Input.vue";
import Label from "@/components/ui/Label.vue";
import Modal from "@/components/ui/Modal.vue";
import Select from "@/components/ui/Select.vue";
import StringListInput from "@/components/ui/StringListInput.vue";
import { useConfirm } from "@/composables/useConfirm";
import type { KvEntry } from "@/components/ui/KvListInput.vue";
import {
  errMsg,
  modelApi,
  providerApi,
  type ModelDef,
  type Offer,
  type Provider,
} from "@/lib/api";
import { formatCtx } from "@/lib/utils";

const error = ref<string | null>(null);
const { confirm } = useConfirm();
const textareaClass =
  "flex w-full rounded-md border border-input bg-transparent px-3 py-2 font-mono text-xs shadow-sm focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring";
/** 节头内嵌小号下拉。 */
const providerOptions = computed(() => providers.value.map((p) => ({ value: p.key, label: p.key })));
const modelOptions = computed(() => models.value.map((m) => ({ value: m.slug, label: m.slug })));

// ───────────────────────── 模型定义 CRUD ─────────────────────────
const models = ref<ModelDef[]>([]);
const editing = ref(false);
const isNew = ref(false);
const busy = ref(false);

interface ModelForm {
  slug: string;
  displayName: string;
  contextWindow: string;
  modalities: string[];
  reasoningLevels: string[];
  /** 默认定价：五类已知 token 单价（USD/1M），留空表示不单独定价。 */
  pricing: { input: string; output: string; cacheRead: string; cacheWrite: string; reasoning: string };
  /** 定价中的未知键（保留原值，保存时合并回写）。 */
  pricingExtra: KvEntry[];
  extraText: string;
}

const emptyPricing = () => ({ input: "", output: "", cacheRead: "", cacheWrite: "", reasoning: "" });

/** 模态预置选项：text 为所有 LLM 天然支持，不作为勾选项（保存时始终隐含）；未知值动态追加。 */
const KNOWN_MODALITIES = ["pdf", "image", "audio", "video"] as const;

function toggleArr(arr: string[], v: string) {
  const i = arr.indexOf(v);
  if (i >= 0) arr.splice(i, 1);
  else arr.push(v);
}

/** 勾选项：预置项在前，已有数据中的未知值动态追加。 */
const modalityOptions = computed(() => [
  ...KNOWN_MODALITIES,
  ...form.modalities.filter((v) => !(KNOWN_MODALITIES as readonly string[]).includes(v)),
]);

const form = reactive<ModelForm>({
  slug: "",
  displayName: "",
  contextWindow: "",
  modalities: [],
  reasoningLevels: [],
  pricing: emptyPricing(),
  pricingExtra: [],
  extraText: "{}",
});

/** 已知结构的对象 ↔ KV 编辑行互转。 */
function objectToKv(v: unknown): KvEntry[] {
  if (v === null || typeof v !== "object" || Array.isArray(v)) return [];
  return Object.entries(v).map(([key, val]) => ({ key, value: String(val) }));
}

function kvToObject(entries: KvEntry[]): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const { key, value } of entries) {
    const k = key.trim();
    if (!k) continue;
    const t = value.trim();
    out[k] = t !== "" && !Number.isNaN(Number(t)) ? Number(t) : t;
  }
  return out;
}

async function loadModels() {
  try {
    models.value = await modelApi.list();
  } catch (e) {
    error.value = errMsg(e);
  }
}

function newModel() {
  isNew.value = true;
  Object.assign(form, {
    slug: "",
    displayName: "",
    contextWindow: "",
    modalities: [],
    reasoningLevels: [],
    pricing: emptyPricing(),
    pricingExtra: [],
    extraText: "{}",
  });
  error.value = null;
  editing.value = true;
}

function editModel(m: ModelDef) {
  isNew.value = false;
  const p = (m.pricing ?? {}) as Record<string, unknown>;
  const num = (k: string) => (typeof p[k] === "number" ? String(p[k]) : "");
  const known = new Set(["input", "output", "cache_read", "cache_write", "reasoning"]);
  Object.assign(form, {
    slug: m.slug,
    displayName: m.displayName ?? "",
    contextWindow: m.contextWindow != null ? String(m.contextWindow) : "",
    modalities: Array.isArray(m.modalities)
      ? (m.modalities as string[]).filter((v) => v !== "text")
      : [],
    reasoningLevels: Array.isArray(m.reasoningLevels) ? (m.reasoningLevels as string[]) : [],
    pricing: {
      input: num("input"),
      output: num("output"),
      cacheRead: num("cache_read"),
      cacheWrite: num("cache_write"),
      reasoning: num("reasoning"),
    },
    pricingExtra: objectToKv(p).filter((e) => !known.has(e.key)),
    extraText: m.extra == null ? "{}" : JSON.stringify(m.extra, null, 2),
  });
  error.value = null;
  editing.value = true;
}

async function saveModel() {
  error.value = null;
  const slug = form.slug.trim();
  if (!slug) {
    error.value = "模型 slug 不能为空";
    return;
  }
  let extra: unknown;
  try {
    extra = form.extraText.trim() ? JSON.parse(form.extraText) : {};
  } catch {
    error.value = "扩展 extra 不是合法 JSON";
    return;
  }
  let contextWindow: number | null = null;
  if (form.contextWindow.trim()) {
    contextWindow = Number(form.contextWindow);
    if (Number.isNaN(contextWindow)) {
      error.value = "上下文窗口须为数字";
      return;
    }
  }
  // 默认定价：五个已知键 + 未知键合并回写
  const pricingObj: Record<string, unknown> = {};
  const knownPrices: Array<[string, string]> = [
    ["input", form.pricing.input],
    ["output", form.pricing.output],
    ["cache_read", form.pricing.cacheRead],
    ["cache_write", form.pricing.cacheWrite],
    ["reasoning", form.pricing.reasoning],
  ];
  for (const [key, raw] of knownPrices) {
    const t = raw.trim();
    if (!t) continue;
    const n = Number(t);
    if (Number.isNaN(n)) {
      error.value = "默认定价须为数字";
      return;
    }
    pricingObj[key] = n;
  }
  Object.assign(pricingObj, kvToObject(form.pricingExtra));
  const rec: ModelDef = {
    slug,
    displayName: form.displayName.trim() || null,
    contextWindow,
    modalities: form.modalities.length ? ["text", ...form.modalities] : null,
    reasoningLevels: form.reasoningLevels.length ? form.reasoningLevels : null,
    pricing: Object.keys(pricingObj).length ? pricingObj : null,
    extra,
  };
  busy.value = true;
  try {
    await modelApi.save(rec);
    editing.value = false;
    await loadModels();
  } catch (e) {
    error.value = errMsg(e);
  } finally {
    busy.value = false;
  }
}

async function removeModel(slug: string) {
  if (!(await confirm({ title: "删除模型", message: `确认删除模型 “${slug}”？相关 offer 不会自动删除。` }))) return;
  try {
    await modelApi.remove(slug);
    await loadModels();
  } catch (e) {
    error.value = errMsg(e);
  }
}

// ───────────────────────── Offer 管理（provider 维度）─────────────────────────
const providers = ref<Provider[]>([]);
const selectedProvider = ref("");
const offers = ref<Offer[]>([]);
const offerBusy = ref(false);
/** 定价弹窗：五类 token 独立定价（单位 USD / 1M tokens），留空表示未单独定价。 */
const offerModal = ref(false);
const offerError = ref<string | null>(null);
const offerForm = reactive({
  modelSlug: "",
  inputPrice: "",
  outputPrice: "",
  cacheReadPrice: "",
  cacheWritePrice: "",
  reasoningPrice: "",
});

function openAddOffer() {
  offerError.value = null;
  Object.assign(offerForm, {
    inputPrice: "",
    outputPrice: "",
    cacheReadPrice: "",
    cacheWritePrice: "",
    reasoningPrice: "",
  });
  offerModal.value = true;
}

function editOffer(o: Offer) {
  offerError.value = null;
  const p = (o.pricing ?? {}) as Record<string, unknown>;
  const num = (k: string) => (typeof p[k] === "number" ? String(p[k]) : "");
  offerForm.modelSlug = o.modelSlug;
  Object.assign(offerForm, {
    inputPrice: num("input"),
    outputPrice: num("output"),
    cacheReadPrice: num("cache_read"),
    cacheWritePrice: num("cache_write"),
    reasoningPrice: num("reasoning"),
  });
  offerModal.value = true;
}

async function confirmOffer() {
  offerError.value = null;
  if (!selectedProvider.value || !offerForm.modelSlug) {
    offerError.value = "请选择 provider 与模型 slug";
    return;
  }
  const fields: Array<[string, string]> = [
    ["input", offerForm.inputPrice],
    ["output", offerForm.outputPrice],
    ["cache_read", offerForm.cacheReadPrice],
    ["cache_write", offerForm.cacheWritePrice],
    ["reasoning", offerForm.reasoningPrice],
  ];
  let pricing: Record<string, number> | null = null;
  for (const [key, raw] of fields) {
    const t = raw.trim();
    if (t === "") continue;
    const n = Number(t);
    if (Number.isNaN(n)) {
      offerError.value = "定价须为数字";
      return;
    }
    (pricing ??= {})[key] = n;
  }
  offerBusy.value = true;
  try {
    await modelApi.offerSave({
      providerKey: selectedProvider.value,
      modelSlug: offerForm.modelSlug,
      pricing,
    });
    offerModal.value = false;
    await loadOffers();
  } catch (e) {
    offerError.value = errMsg(e);
  } finally {
    offerBusy.value = false;
  }
}

async function loadProviders() {
  try {
    providers.value = await providerApi.list();
    if (!selectedProvider.value && providers.value.length > 0) {
      selectedProvider.value = providers.value[0].key;
      await loadOffers();
    }
  } catch (e) {
    error.value = errMsg(e);
  }
}

async function loadOffers() {
  if (!selectedProvider.value) {
    offers.value = [];
    return;
  }
  try {
    offers.value = await modelApi.offerList(selectedProvider.value);
  } catch (e) {
    error.value = errMsg(e);
  }
}

async function onProviderChange() {
  await loadOffers();
}

async function removeOffer(modelSlug: string) {
  if (!(await confirm({ title: "删除报价", message: `确认删除 ${selectedProvider.value} 对 “${modelSlug}” 的报价？` }))) return;
  try {
    await modelApi.offerRemove(selectedProvider.value, modelSlug);
    await loadOffers();
  } catch (e) {
    error.value = errMsg(e);
  }
}

/** 读取 offer 定价中的单项（无该定价显示 —）。 */
function price(p: unknown, key: string): string {
  const v = (p as Record<string, unknown> | null)?.[key];
  return typeof v === "number" ? String(v) : "—";
}

onMounted(async () => {
  await Promise.all([loadModels(), loadProviders()]);
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

    <!-- 编辑弹窗 -->
    <Modal
      :open="editing"
      :title="isNew ? '新建模型' : '编辑模型'"
      @close="editing = false"
    >
      <div class="grid gap-4 md:grid-cols-2">
        <div class="space-y-1.5">
          <Label for="m-slug">唯一标识</Label>
          <Input id="m-slug" v-model="form.slug" placeholder="如 claude-sonnet-4" :disabled="!isNew" />
        </div>
        <div class="space-y-1.5">
          <Label for="m-name">显示名</Label>
          <Input id="m-name" v-model="form.displayName" placeholder="Claude Sonnet 4" />
        </div>
        <div class="space-y-1.5 md:col-span-2">
          <Label for="m-ctx">上下文窗口（token，可选）</Label>
          <Input id="m-ctx" v-model="form.contextWindow" placeholder="200000" inputmode="numeric" />
        </div>
        <div class="space-y-1.5">
          <Label>模态</Label>
          <div class="flex flex-wrap gap-x-4 gap-y-2 pt-1.5">
            <label
              v-for="m in modalityOptions"
              :key="m"
              class="flex cursor-pointer items-center gap-1.5 text-sm"
            >
              <input
                type="checkbox"
                class="size-4 accent-primary"
                :checked="form.modalities.includes(m)"
                @change="toggleArr(form.modalities, m)"
              />
              <span class="font-mono text-xs">{{ m }}</span>
            </label>
          </div>
        </div>
        <div class="space-y-1.5">
          <Label>推理档位（各家支持的档位名不同，逐个添加）</Label>
          <StringListInput v-model="form.reasoningLevels" placeholder="如 high 后回车添加" />
        </div>
        <div class="space-y-1.5 md:col-span-2">
          <Label>默认定价（USD / 1M tokens，可选）</Label>
          <div class="grid grid-cols-2 gap-3 sm:grid-cols-5">
            <div class="space-y-1">
              <span class="text-xs text-muted-foreground">输入</span>
              <Input v-model="form.pricing.input" placeholder="3" inputmode="decimal" />
            </div>
            <div class="space-y-1">
              <span class="text-xs text-muted-foreground">输出</span>
              <Input v-model="form.pricing.output" placeholder="15" inputmode="decimal" />
            </div>
            <div class="space-y-1">
              <span class="text-xs text-muted-foreground">缓存读</span>
              <Input v-model="form.pricing.cacheRead" placeholder="0.3" inputmode="decimal" />
            </div>
            <div class="space-y-1">
              <span class="text-xs text-muted-foreground">缓存写</span>
              <Input v-model="form.pricing.cacheWrite" placeholder="3.75" inputmode="decimal" />
            </div>
            <div class="space-y-1">
              <span class="text-xs text-muted-foreground">推理</span>
              <Input v-model="form.pricing.reasoning" placeholder="5" inputmode="decimal" />
            </div>
          </div>
        </div>
        <div class="space-y-1.5 md:col-span-2">
          <Label for="m-extra">扩展字段（自由 JSON）</Label>
          <textarea id="m-extra" v-model="form.extraText" rows="3" spellcheck="false" :class="textareaClass"></textarea>
        </div>
      </div>
      <template #footer>
        <Button variant="ghost" size="sm" @click="editing = false">取消</Button>
        <Button size="sm" :disabled="busy" @click="saveModel">{{ busy ? "保存中…" : "保存" }}</Button>
      </template>
    </Modal>

    <!-- 模型定义 + 模型报价：单卡双节，满版填满视口 -->
    <Card class="flex min-h-0 flex-1 flex-col overflow-hidden">
      <!-- 节：模型定义 -->
      <section class="shrink-0">
        <div class="flex items-center justify-between border-b px-5 py-3">
          <h3 class="card-title">模型定义</h3>
          <Button size="sm" @click="newModel">
            <Plus class="size-4" /> 新建模型
          </Button>
        </div>
        <div class="px-5 py-4">
          <div
            v-if="models.length === 0"
            class="rounded-md border border-dashed p-8 text-center text-sm text-muted-foreground"
          >
            暂无模型定义，点击「新建模型」添加。
          </div>
          <table v-else class="w-full text-sm">
            <thead>
              <tr class="border-b text-left text-muted-foreground">
                <th class="pb-2 font-medium">标识</th>
                <th class="pb-2 font-medium">显示名</th>
                <th class="pb-2 text-right font-medium">上下文窗口</th>
                <th class="pb-2 text-right font-medium">操作</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="m in models" :key="m.slug" class="border-b last:border-0">
                <td class="py-2 font-mono text-xs">{{ m.slug }}</td>
                <td class="py-2">{{ m.displayName ?? "—" }}</td>
                <td class="py-2 text-right tabular-nums text-muted-foreground">
                  {{ formatCtx(m.contextWindow) }}
                </td>
                <td class="py-2">
                  <div class="flex justify-end gap-0.5">
                    <Button variant="ghost" size="icon" class="size-7" @click="editModel(m)">
                      <Pencil class="size-3.5" />
                    </Button>
                    <Button variant="ghost" size="icon" class="size-7" @click="removeModel(m.slug)">
                      <Trash2 class="size-3.5 text-destructive" />
                    </Button>
                  </div>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
      </section>

      <!-- 节：模型报价 -->
      <section class="flex min-h-0 flex-1 flex-col border-t">
        <div class="flex shrink-0 flex-wrap items-center justify-between gap-2 border-b px-5 py-3">
          <h3 class="card-title">模型报价</h3>
          <div class="flex items-center gap-2 text-xs text-muted-foreground">
            <span>上游服务</span>
            <Select v-model="selectedProvider" :options="providerOptions" small @update:model-value="onProviderChange" />
          </div>
        </div>
        <div class="scrollbar-thin min-h-0 flex-1 space-y-4 overflow-y-auto px-5 py-4">
          <div
            v-if="!selectedProvider"
            class="rounded-md border border-dashed p-6 text-center text-sm text-muted-foreground"
          >
            请先在「上游服务」页创建 provider。
          </div>
          <template v-else>
            <!-- 新增 offer：选模型 → 弹窗内填五类定价 -->
            <div class="flex items-end gap-3">
              <div class="flex-1 space-y-1.5">
                <Label>模型</Label>
                <Select v-model="offerForm.modelSlug" :options="modelOptions" placeholder="选择模型…" />
              </div>
              <Button size="sm" class="h-9" :disabled="!offerForm.modelSlug" @click="openAddOffer">
                <Plus class="size-4" /> 添加报价
              </Button>
            </div>

            <!-- 现有 offer -->
            <div
              v-if="offers.length === 0"
              class="rounded-md border border-dashed p-6 text-center text-sm text-muted-foreground"
            >
              该 provider 暂无报价。
            </div>
            <table v-else class="w-full text-sm">
              <thead>
                <tr class="border-b text-left text-muted-foreground">
                  <th class="pb-2 font-medium">模型</th>
                  <th class="pb-2 text-right font-medium">输入</th>
                  <th class="pb-2 text-right font-medium">输出</th>
                  <th class="pb-2 text-right font-medium">缓存读</th>
                  <th class="pb-2 text-right font-medium">缓存写</th>
                  <th class="pb-2 text-right font-medium">推理</th>
                  <th class="pb-2 text-right font-medium">操作</th>
                </tr>
              </thead>
              <tbody>
                <tr v-for="o in offers" :key="o.modelSlug" class="border-b last:border-0">
                  <td class="py-2 font-mono text-xs">{{ o.modelSlug }}</td>
                  <td class="py-2 text-right tabular-nums">{{ price(o.pricing, "input") }}</td>
                  <td class="py-2 text-right tabular-nums">{{ price(o.pricing, "output") }}</td>
                  <td class="py-2 text-right tabular-nums">{{ price(o.pricing, "cache_read") }}</td>
                  <td class="py-2 text-right tabular-nums">{{ price(o.pricing, "cache_write") }}</td>
                  <td class="py-2 text-right tabular-nums">{{ price(o.pricing, "reasoning") }}</td>
                  <td class="py-2">
                    <div class="flex justify-end gap-0.5">
                      <Button variant="ghost" size="icon" class="size-7" title="编辑定价" @click="editOffer(o)">
                        <Pencil class="size-3.5" />
                      </Button>
                      <Button variant="ghost" size="icon" class="size-7" @click="removeOffer(o.modelSlug)">
                        <Trash2 class="size-3.5 text-destructive" />
                      </Button>
                    </div>
                  </td>
                </tr>
              </tbody>
            </table>
          </template>
        </div>
      </section>
    </Card>

    <!-- 报价定价弹窗：五类 token 独立定价 -->
    <Modal
      :open="offerModal"
      :title="'报价定价'"
      width="max-w-lg"
      @close="offerModal = false"
    >
      <div v-if="offerError" class="mb-4 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive">
        {{ offerError }}
      </div>
      <p class="mb-4 text-xs text-muted-foreground">单位：USD / 1M tokens；留空表示该项不单独定价。</p>
      <div class="grid gap-4 sm:grid-cols-2">
        <div class="space-y-1.5">
          <Label for="p-input">输入</Label>
          <Input id="p-input" v-model="offerForm.inputPrice" placeholder="3" inputmode="decimal" />
        </div>
        <div class="space-y-1.5">
          <Label for="p-output">输出</Label>
          <Input id="p-output" v-model="offerForm.outputPrice" placeholder="15" inputmode="decimal" />
        </div>
        <div class="space-y-1.5">
          <Label for="p-cr">缓存读</Label>
          <Input id="p-cr" v-model="offerForm.cacheReadPrice" placeholder="0.3" inputmode="decimal" />
        </div>
        <div class="space-y-1.5">
          <Label for="p-cw">缓存写</Label>
          <Input id="p-cw" v-model="offerForm.cacheWritePrice" placeholder="3.75" inputmode="decimal" />
        </div>
        <div class="space-y-1.5">
          <Label for="p-r">推理</Label>
          <Input id="p-r" v-model="offerForm.reasoningPrice" placeholder="5" inputmode="decimal" />
        </div>
      </div>
      <template #footer>
        <Button variant="ghost" size="sm" @click="offerModal = false">取消</Button>
        <Button size="sm" :disabled="offerBusy" @click="confirmOffer">{{ offerBusy ? "保存中…" : "保存" }}</Button>
      </template>
    </Modal>
  </div>
</template>
