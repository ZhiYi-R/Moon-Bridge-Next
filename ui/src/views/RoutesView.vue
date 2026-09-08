<script setup lang="ts">
import { Plus, Trash2 } from "lucide-vue-next";
import { computed, onMounted, reactive, ref } from "vue";

import Button from "@/components/ui/Button.vue";
import Card from "@/components/ui/Card.vue";
import Input from "@/components/ui/Input.vue";
import Label from "@/components/ui/Label.vue";
import Modal from "@/components/ui/Modal.vue";
import Select from "@/components/ui/Select.vue";
import { useConfirm } from "@/composables/useConfirm";
import { errMsg, providerApi, routeApi, type Provider, type Route } from "@/lib/api";

const { confirm } = useConfirm();
const routes = ref<Route[]>([]);
const providers = ref<Provider[]>([]);
const editing = ref(false);
const error = ref<string | null>(null);

const form = reactive<Route>({ alias: "", modelSlug: "", providerKey: "", extra: null });

const providerOptions = computed(() => providers.value.map((p) => ({ value: p.key, label: p.key })));

async function load() {
  try {
    routes.value = await routeApi.list();
    providers.value = await providerApi.list();
  } catch (e) {
    error.value = errMsg(e);
  }
}

function newRoute() {
  Object.assign(form, { alias: "", modelSlug: "", providerKey: "", extra: null });
  error.value = null;
  editing.value = true;
}

async function save() {
  try {
    await routeApi.save({ ...form });
    editing.value = false;
    await load();
  } catch (e) {
    error.value = errMsg(e);
  }
}

async function remove(alias: string) {
  if (!(await confirm({ title: "删除路由", message: `确认删除路由 “${alias}”？` }))) return;
  try {
    await routeApi.remove(alias);
    await load();
  } catch (e) {
    error.value = errMsg(e);
  }
}

onMounted(load);
</script>

<template>
  <div class="flex h-full min-h-0 flex-col">
    <div
      v-if="error"
      class="shrink-0 rounded-md border border-destructive/40 bg-destructive/10 px-4 py-2 text-sm text-destructive"
    >
      {{ error }}
    </div>

    <!-- 新建路由弹窗 -->
    <Modal :open="editing" title="新建路由" @close="editing = false">
      <div class="grid gap-4">
        <div class="space-y-1.5">
          <Label for="r-alias">别名</Label>
          <Input id="r-alias" v-model="form.alias" placeholder="如 moonbridge" />
        </div>
        <div class="space-y-1.5">
          <Label for="r-model">上游模型</Label>
          <Input id="r-model" v-model="form.modelSlug" placeholder="如 claude-sonnet-4" />
        </div>
        <div class="space-y-1.5">
          <Label>上游服务</Label>
          <Select v-model="form.providerKey" :options="providerOptions" placeholder="选择上游服务" />
        </div>
      </div>
      <template #footer>
        <Button variant="ghost" size="sm" @click="editing = false">取消</Button>
        <Button size="sm" @click="save">保存</Button>
      </template>
    </Modal>

    <!-- 路由别名：满版单卡 -->
    <Card class="flex min-h-0 flex-1 flex-col overflow-hidden">
      <div class="flex shrink-0 items-center justify-end border-b px-5 py-3">
        <Button size="sm" @click="newRoute"><Plus class="size-4" /> 新建</Button>
      </div>
      <div class="scrollbar-thin min-h-0 flex-1 overflow-y-auto px-5 py-4">
        <div
          v-if="routes.length === 0"
          class="rounded-md border border-dashed p-8 text-center text-sm text-muted-foreground"
        >
          暂无路由别名。
        </div>
        <table v-else class="w-full text-sm">
          <thead>
            <tr class="border-b text-left text-muted-foreground">
              <th class="pb-2 font-medium">别名</th>
              <th class="pb-2 font-medium">上游模型</th>
              <th class="pb-2 font-medium">上游服务</th>
              <th class="pb-2 text-right font-medium">操作</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="r in routes" :key="r.alias" class="border-b last:border-0">
              <td class="py-2 font-mono text-xs">{{ r.alias }}</td>
              <td class="py-2">{{ r.modelSlug }}</td>
              <td class="py-2">{{ r.providerKey }}</td>
              <td class="py-2">
                <div class="flex justify-end">
                  <Button variant="ghost" size="icon" class="size-7" @click="remove(r.alias)">
                    <Trash2 class="size-3.5 text-destructive" />
                  </Button>
                </div>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </Card>
  </div>
</template>
