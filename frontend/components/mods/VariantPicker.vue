<script setup lang="ts">
import { computed, shallowRef } from "vue"
import type { ModVariant } from "@/types/mods"
import { formatBytes } from "@/utils/formatBytes"

// Toggle buttons rather than a radio group on purpose: choosing a variant
// downloads and installs it, so arrow keys must not pick one by moving.
// With previews for every variant the options ARE the images, large enough to
// compare side by side (and zoomed in on hover or keyboard focus); if one
// fails to load, plain text buttons take over.
const props = defineProps<{
  variants: ModVariant[]
  selected: string
  /** Accessible name of the group, e.g. "Variante de Radar". */
  label: string
  disabled?: boolean
}>()

const emit = defineEmits<{ select: [id: string] }>()

const broken = shallowRef(false)
const visual = computed(() => !broken.value && props.variants.every((v) => Boolean(v.preview)))

function pick(id: string): void {
  if (id !== props.selected) emit("select", id)
}

function hint(variant: ModVariant): string {
  return `${variant.label || variant.id} · v${variant.version} · ${formatBytes(variant.bytes)}`
}
</script>

<template>
  <div
    v-if="visual"
    role="group"
    :aria-label="label"
    class="grid grid-cols-[repeat(auto-fill,minmax(9rem,13rem))] gap-2"
  >
    <button
      v-for="variant in variants"
      :key="variant.id"
      type="button"
      :aria-pressed="variant.id === selected"
      :disabled="disabled"
      :title="hint(variant)"
      class="group relative w-full overflow-hidden rounded-lg border text-left transition-colors duration-150 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-400/70 disabled:cursor-not-allowed disabled:opacity-60"
      :class="
        variant.id === selected
          ? 'border-amber-400/80 bg-amber-400/10'
          : 'border-white/10 bg-black/20 hover:border-white/30'
      "
      @click="pick(variant.id)"
    >
      <span class="block aspect-square w-full overflow-hidden bg-black/40">
        <img
          :src="variant.preview"
          alt=""
          loading="lazy"
          decoding="async"
          referrerpolicy="no-referrer"
          class="h-full w-full object-cover transition-transform duration-300 group-hover:scale-150 group-focus-visible:scale-150"
          @error="broken = true"
        />
      </span>
      <span
        class="block truncate px-1.5 py-1 text-center text-[11px] font-medium leading-tight"
        :class="variant.id === selected ? 'text-amber-200' : 'text-white/70'"
      >
        {{ variant.label || variant.id }}
      </span>
      <span
        v-if="variant.id === selected"
        class="absolute right-1 top-1 inline-flex h-4 w-4 items-center justify-center rounded-full bg-amber-500 text-black shadow"
        aria-hidden="true"
      >
        <i class="pi pi-check text-[8px]" />
      </span>
    </button>
  </div>

  <div
    v-else
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
      :title="hint(variant)"
      class="inline-flex h-7 items-center rounded px-2.5 text-xs font-medium transition-colors duration-150 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-400/70 disabled:cursor-not-allowed disabled:opacity-60"
      :class="
        variant.id === selected
          ? 'bg-amber-500/90 text-black'
          : 'text-white/70 hover:bg-white/[0.06] hover:text-white'
      "
      @click="pick(variant.id)"
    >
      {{ variant.label || variant.id }}
    </button>
  </div>
</template>
