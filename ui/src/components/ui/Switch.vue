<script setup lang="ts">
import { cn } from "@/lib/utils";

interface Props {
  checked?: boolean;
  disabled?: boolean;
}

const props = withDefaults(defineProps<Props>(), { checked: false, disabled: false });
const emit = defineEmits<{ "update:checked": [boolean] }>();
</script>

<template>
  <button
    type="button"
    role="switch"
    :aria-checked="props.checked"
    :disabled="props.disabled"
    :class="
      cn(
        'inline-flex h-5 w-9 shrink-0 items-center rounded-full border border-transparent transition-colors',
        props.checked ? 'bg-primary' : 'bg-input',
        props.disabled && 'cursor-not-allowed opacity-50',
      )
    "
    @click="emit('update:checked', !props.checked)"
  >
    <span
      :class="
        cn(
          'pointer-events-none block size-4 rounded-full bg-background shadow-sm transition-transform',
          props.checked ? 'translate-x-4' : 'translate-x-0',
        )
      "
    />
  </button>
</template>
