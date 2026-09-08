<script setup lang="ts">
import { Plus, X } from "lucide-vue-next";
import { ref } from "vue";

import Button from "./Button.vue";

const props = defineProps<{
  modelValue: string[];
  placeholder?: string;
  addLabel?: string;
}>();

const emit = defineEmits<{ "update:modelValue": [string[]] }>();
const draft = ref("");

const inputCls =
  "flex h-9 w-full rounded-md border border-input bg-transparent px-3 py-1 text-sm shadow-sm transition-colors placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-50";

function add() {
  const v = draft.value.trim();
  if (v && !props.modelValue.includes(v)) {
    emit("update:modelValue", [...props.modelValue, v]);
  }
  draft.value = "";
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
    <div v-if="modelValue.length > 0" class="flex flex-wrap gap-1.5">
      <span
        v-for="(tag, i) in modelValue"
        :key="tag"
        class="inline-flex items-center gap-1 rounded-md border px-2 py-0.5 text-xs"
      >
        {{ tag }}
        <button
          type="button"
          class="text-muted-foreground transition-colors hover:text-foreground"
          title="移除"
          @click="removeAt(i)"
        >
          <X class="size-3" />
        </button>
      </span>
    </div>
    <div class="flex gap-2">
      <input
        v-model="draft"
        :placeholder="placeholder ?? '输入后回车添加'"
        :class="inputCls"
        @keydown.enter.prevent="add"
      />
      <Button type="button" variant="outline" size="sm" class="shrink-0" @click="add">
        <Plus class="size-3.5" /> {{ addLabel ?? "添加" }}
      </Button>
    </div>
  </div>
</template>
