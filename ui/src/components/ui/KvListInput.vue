<script lang="ts">
/** KV 编辑行：value 以字符串维护，提交时由调用方决定类型转换。 */
export interface KvEntry {
  key: string;
  value: string;
}
</script>

<script setup lang="ts">
import { Plus, Trash2 } from "lucide-vue-next";

import Button from "./Button.vue";

const props = defineProps<{
  modelValue: KvEntry[];
  keyPlaceholder?: string;
  valuePlaceholder?: string;
}>();

const emit = defineEmits<{ "update:modelValue": [KvEntry[]] }>();

const inputCls =
  "flex h-9 w-full rounded-md border border-input bg-transparent px-3 py-1 text-sm shadow-sm transition-colors placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-50";

function update(i: number, field: "key" | "value", v: string) {
  emit(
    "update:modelValue",
    props.modelValue.map((e, idx) => (idx === i ? { ...e, [field]: v } : e)),
  );
}

function add() {
  emit("update:modelValue", [...props.modelValue, { key: "", value: "" }]);
}

function removeAt(i: number) {
  emit(
    "update:modelValue",
    props.modelValue.filter((_, idx) => idx !== i),
  );
}
</script>

<template>
  <div class="space-y-2">
    <div v-if="modelValue.length === 0" class="text-xs text-muted-foreground">
      暂无字段，点击下方按钮添加。
    </div>
    <div v-for="(e, i) in modelValue" :key="i" class="flex items-center gap-2">
      <input
        :value="e.key"
        :placeholder="keyPlaceholder ?? '键'"
        :class="inputCls"
        @input="update(i, 'key', ($event.target as HTMLInputElement).value)"
      />
      <input
        :value="e.value"
        :placeholder="valuePlaceholder ?? '值'"
        :class="inputCls"
        @input="update(i, 'value', ($event.target as HTMLInputElement).value)"
      />
      <Button variant="ghost" size="icon" class="size-7 shrink-0" title="删除" @click="removeAt(i)">
        <Trash2 class="size-3.5 text-destructive" />
      </Button>
    </div>
    <Button variant="outline" size="sm" @click="add">
      <Plus class="size-3.5" /> 添加字段
    </Button>
  </div>
</template>
