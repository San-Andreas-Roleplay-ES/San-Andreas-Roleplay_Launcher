<script setup lang="ts">
import { computed } from "vue"

const props = defineProps<{
  checked: boolean
  /** Accessible name, e.g. "Activar Radar". */
  label: string
  /** Can't change (an essential mod, or one another mod needs). Stays
   *  focusable so its `hint` can be read. */
  locked?: boolean
  /** Temporarily unavailable (an install running, the game open). */
  disabled?: boolean
  hint?: string
}>()

const emit = defineEmits<{ change: [checked: boolean] }>()

const inert = computed(() => props.locked || props.disabled)

function toggle(): void {
  if (!inert.value) emit("change", !props.checked)
}
</script>

<template>
  <button
    type="button"
    role="switch"
    :aria-checked="checked"
    :aria-label="label"
    :aria-disabled="inert"
    :title="hint"
    class="group relative inline-flex h-5 w-9 flex-shrink-0 items-center rounded-full border transition-colors duration-150 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-400/70 focus-visible:ring-offset-2 focus-visible:ring-offset-neutral-900"
    :class="[
      checked ? 'border-amber-400/60 bg-amber-500/80' : 'border-white/15 bg-white/10',
      inert ? 'cursor-not-allowed opacity-60' : 'cursor-pointer hover:border-amber-300/70',
    ]"
    @click="toggle"
  >
    <span
      class="inline-flex h-3.5 w-3.5 items-center justify-center rounded-full bg-white shadow transition-transform duration-150"
      :class="checked ? 'translate-x-[18px]' : 'translate-x-[2px]'"
      aria-hidden="true"
    >
      <i v-if="locked" class="pi pi-lock text-[8px] text-neutral-700" />
    </span>
  </button>
</template>
