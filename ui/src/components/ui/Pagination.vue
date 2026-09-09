<script setup lang="ts">
import { ChevronLeft, ChevronRight } from "lucide-vue-next";

import Button from "./Button.vue";

defineProps<{ page: number; pageCount: number; total: number }>();
const emit = defineEmits<{ (e: "update:page", v: number): void }>();
</script>

<template>
  <div class="flex items-center justify-between gap-2 text-xs text-muted-foreground">
    <span>共 {{ total }} 条 · 第 {{ page }} / {{ pageCount }} 页</span>
    <div class="flex items-center gap-1">
      <Button
        variant="outline"
        size="icon"
        class="size-7"
        :disabled="page <= 1"
        @click="emit('update:page', page - 1)"
      >
        <ChevronLeft class="size-3.5" />
      </Button>
      <Button
        variant="outline"
        size="icon"
        class="size-7"
        :disabled="page >= pageCount"
        @click="emit('update:page', page + 1)"
      >
        <ChevronRight class="size-3.5" />
      </Button>
    </div>
  </div>
</template>
