<script setup lang="ts">
import type { ModVariant } from "@/types/mods"
import { formatBytes } from "@/utils/formatBytes"

// A group of toggle buttons rather than a radio group on purpose: choosing a
// variant downloads and installs it, so arrow keys must not pick one by
// just moving across.
defineProps<{
  variants: ModVariant[]
  selected: string
  /** Accessible name of the group, e.g. "Variante de Radar". */
  label: string
  disabled?: boolean
}>()

const emit = defineEmits<{ select: [id: string] }>()
</script>

<template>
  <div
    role="group"
    :aria-label="label"
    class="inline-flex max-w-full flex-wrap gap-0.5 rounded-md border border-white/10 bg-black/20 p-0.5"
  >
    <button
      v-for="variant in variants"
      :key="variant.id"
      type="button"
      :aria-pressed="variant.id === selected"
      :disabled="disabled"
      :title="`${variant.label || variant.id} · ${formatBytes(variant.bytes)}`"
      class="inline-flex h-7 items-center rounded px-2.5 text-xs font-medium transition-colors duration-150 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-400/70 disabled:cursor-not-allowed disabled:opacity-60"
      :class="
        variant.id === selected
          ? 'bg-amber-500/90 text-black'
          : 'text-white/70 hover:bg-white/[0.06] hover:text-white'
      "
      @click="variant.id !== selected && emit('select', variant.id)"
    >
      {{ variant.label || variant.id }}
    </button>
  </div>
</template>
