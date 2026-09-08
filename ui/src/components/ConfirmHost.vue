<script setup lang="ts">
import Button from "@/components/ui/Button.vue";
import Modal from "@/components/ui/Modal.vue";
import { useConfirm } from "@/composables/useConfirm";

const { state, settle } = useConfirm();
</script>

<template>
  <Modal :open="state.open" :title="state.title" width="max-w-md" plain @close="settle(false)">
    <p class="text-sm leading-relaxed">{{ state.message }}</p>
    <template #footer>
      <Button variant="ghost" size="sm" @click="settle(false)">取消</Button>
      <Button
        :variant="state.danger ? 'outline' : 'default'"
        size="sm"
        :class="state.danger && 'border-destructive/50 text-destructive hover:bg-destructive/10 hover:text-destructive'"
        @click="settle(true)"
      >
        {{ state.confirmText }}
      </Button>
    </template>
  </Modal>
</template>
