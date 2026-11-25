<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, useTemplateRef, watch } from "vue"
import type { GtaCandidate } from "@/types/gta"
import { sourceLabel } from "@/utils/gtaSource"

const props = defineProps<{
  open: boolean
  candidates: GtaCandidate[]
}>()

const emit = defineEmits<{
  pick: [path: string]
  browse: []
  close: []
}>()

interface Badge {
  label: string
  tone: string
}

const rows = computed(() =>
  props.candidates.map((candidate) => {
    const badges: Badge[] = [
      { label: sourceLabel(candidate.source), tone: "border-white/15 text-white/60" },
    ]
    if (candidate.hasSamp) {
      badges.push({ label: "SA-MP instalado", tone: "border-emerald-400/40 text-emerald-300/90" })
    }
    if (candidate.current) {
      badges.push({ label: "En uso", tone: "border-amber-400/40 text-amber-300/90" })
    }
    return { ...candidate, badges }
  }),
)

const list = useTemplateRef<HTMLElement>("list")

// Focus the first install that can be picked, so Enter chooses it.
watch(
  () => props.open,
  async (open) => {
    if (!open) return
    await nextTick()
    list.value?.querySelector<HTMLButtonElement>("button:not([disabled])")?.focus()
  },
)

function onKeydown(event: KeyboardEvent): void {
  if (props.open && event.key === "Escape") {
    event.preventDefault()
    emit("close")
  }
}

onMounted(() => window.addEventListener("keydown", onKeydown))
onBeforeUnmount(() => window.removeEventListener("keydown", onKeydown))
</script>

<template>
  <!-- Teleported: the health card's backdrop-blur would trap a fixed overlay. -->
  <Teleport to="body">
    <Transition name="picker">
      <div
        v-if="open"
        class="fixed inset-0 z-50 flex items-center justify-center bg-black/60 px-6 backdrop-blur-sm"
        @click.self="emit('close')"
      >
        <div
          role="dialog"
          aria-modal="true"
          aria-labelledby="gta-picker-title"
          aria-describedby="gta-picker-message"
          class="picker-card flex max-h-[85vh] w-full max-w-lg flex-col overflow-hidden rounded-xl border border-white/10 bg-neutral-900/95 shadow-2xl ring-1 ring-amber-400/20 backdrop-blur-md"
        >
          <div class="flex items-start gap-3 px-5 pt-5">
            <i class="pi pi-folder-open mt-0.5 text-xl text-amber-300" aria-hidden="true" />
            <div class="min-w-0 flex-1">
              <h3 id="gta-picker-title" class="text-sm font-semibold text-white">
                ¿Qué GTA San Andreas quieres usar?
              </h3>
              <p id="gta-picker-message" class="mt-1 text-sm leading-relaxed text-white/70">
                Encontramos estas instalaciones en tu PC.
              </p>
            </div>
          </div>

          <ul ref="list" class="mt-4 flex min-h-0 flex-col gap-2 overflow-y-auto px-5">
            <li v-for="row in rows" :key="row.path">
              <button
                type="button"
                class="group flex w-full items-center gap-3 rounded-lg border border-white/10 bg-white/[0.03] px-3 py-2.5 text-left transition-colors duration-150 hover:border-amber-400/40 hover:bg-white/[0.06] focus-visible:border-amber-400/60 focus-visible:outline-none disabled:cursor-default disabled:opacity-60 disabled:hover:border-white/10 disabled:hover:bg-white/[0.03]"
                :disabled="row.current"
                :title="row.path"
                @click="emit('pick', row.path)"
              >
                <span class="min-w-0 flex-1">
                  <span class="block break-all font-mono text-xs leading-snug text-white/90">
                    {{ row.path }}
                  </span>
                  <span class="mt-1.5 flex flex-wrap gap-1.5">
                    <span
                      v-for="badge in row.badges"
                      :key="badge.label"
                      class="inline-flex items-center rounded-full border px-2 py-0.5 text-[10px] font-medium leading-none"
                      :class="badge.tone"
                    >
                      {{ badge.label }}
                    </span>
                  </span>
                </span>
                <i
                  v-if="!row.current"
                  class="pi pi-chevron-right flex-shrink-0 text-xs text-white/30 transition-colors duration-150 group-hover:text-amber-300"
                  aria-hidden="true"
                />
              </button>
            </li>
          </ul>

          <div
            class="mt-5 flex items-center justify-between gap-2 border-t border-white/5 bg-white/[0.02] px-5 py-3"
          >
            <button
              type="button"
              class="inline-flex h-8 items-center gap-1.5 rounded-md border border-white/15 px-3 text-xs font-medium text-white/80 transition-colors duration-150 hover:border-white/30 hover:bg-white/[0.04] hover:text-white"
              @click="emit('browse')"
            >
              <i class="pi pi-folder text-[11px]" aria-hidden="true" />
              <span>Elegir otra carpeta…</span>
            </button>
            <button
              type="button"
              class="inline-flex h-8 items-center rounded-md px-3 text-xs font-medium text-white/60 transition-colors duration-150 hover:bg-white/[0.04] hover:text-white"
              @click="emit('close')"
            >
              Cancelar
            </button>
          </div>
        </div>
      </div>
    </Transition>
  </Teleport>
</template>

<style scoped>
.picker-enter-active,
.picker-leave-active {
  transition: opacity 0.18s ease;
}
.picker-enter-from,
.picker-leave-to {
  opacity: 0;
}
.picker-enter-active .picker-card,
.picker-leave-active .picker-card {
  transition:
    transform 0.18s ease,
    opacity 0.18s ease;
}
.picker-enter-from .picker-card,
.picker-leave-to .picker-card {
  transform: scale(0.96) translateY(4px);
  opacity: 0;
}
</style>
