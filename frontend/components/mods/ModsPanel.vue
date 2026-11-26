<script setup lang="ts">
import { computed, useTemplateRef } from "vue"
import ModRow from "./ModRow.vue"
import { useFocusTrap } from "@/composables/useFocusTrap"
import { useHealthCheckStore } from "@/stores/healthCheck"
import { MODS_BUTTON_ID, useModsStore } from "@/stores/mods"
import type { ModStatus } from "@/types/mods"
import { categoryLabel } from "@/utils/modCategory"

const props = defineProps<{ open: boolean }>()
const emit = defineEmits<{ close: [] }>()

const mods = useModsStore()
const health = useHealthCheckStore()

const card = useTemplateRef<HTMLElement>("card")
useFocusTrap(
  card,
  () => props.open,
  () => emit("close"),
  // The row (and its button) is re-rendered after a run.
  () => document.getElementById(MODS_BUTTON_ID),
)

interface Group {
  category: string
  label: string
  mods: ModStatus[]
}

/** Optional mods by category, alphabetically; one group needs no heading. */
const groups = computed<Group[]>(() => {
  const byCategory = new Map<string, ModStatus[]>()
  for (const mod of mods.optional) {
    const list = byCategory.get(mod.category) ?? []
    list.push(mod)
    byCategory.set(mod.category, list)
  }
  return [...byCategory.entries()]
    .map(([category, list]) => ({ category, label: categoryLabel(category), mods: list }))
    .sort((a, b) => a.label.localeCompare(b.label, "es"))
})

const disabled = computed(() => mods.working || health.gameRunning)

/** One note above the list, the most useful first. */
const banner = computed<{ text: string; tone: string; icon: string } | null>(() => {
  if (health.gameRunning) {
    return {
      text: "Cierra el juego para cambiar los mods.",
      tone: "border-sky-400/30 bg-sky-400/[0.06] text-sky-100/90",
      icon: "pi-info-circle text-sky-300",
    }
  }
  const detail = mods.health?.catalog.detail
  if (detail) {
    return {
      text: detail,
      tone: "border-amber-400/30 bg-amber-400/[0.06] text-amber-100/90",
      icon: "pi-exclamation-triangle text-amber-300",
    }
  }
  if (mods.health && !mods.health.ready) {
    return {
      text: "Elige la carpeta de GTA San Andreas para instalar mods. Puedes dejarlos elegidos.",
      tone: "border-white/10 bg-white/[0.03] text-white/70",
      icon: "pi-folder-open text-white/50",
    }
  }
  return null
})

/** Read out while a run goes on. */
const status = computed(() => (mods.working ? (mods.progress?.message ?? "Instalando mods…") : ""))

function working(mod: ModStatus): boolean {
  return mods.working && (mod.installing || mods.runIds.includes(mod.id))
}
</script>

<template>
  <!-- Teleported: the health card's backdrop-blur would trap a fixed overlay. -->
  <Teleport to="body">
    <Transition name="panel">
      <div
        v-if="open"
        class="fixed inset-0 z-50 flex items-center justify-center bg-black/60 px-6 backdrop-blur-sm"
        @click.self="emit('close')"
      >
        <div
          ref="card"
          role="dialog"
          aria-modal="true"
          aria-labelledby="mods-title"
          aria-describedby="mods-subtitle"
          tabindex="-1"
          class="panel-card flex max-h-[85vh] w-full max-w-xl flex-col overflow-hidden rounded-xl border border-white/10 bg-neutral-900/95 shadow-2xl ring-1 ring-amber-400/20 backdrop-blur-md focus:outline-none"
        >
          <header class="flex items-start gap-3 px-5 pt-5">
            <i class="pi pi-box mt-0.5 text-xl text-amber-300" aria-hidden="true" />
            <div class="min-w-0 flex-1">
              <h3 id="mods-title" class="text-sm font-semibold text-white">Mods</h3>
              <p id="mods-subtitle" class="mt-1 text-sm leading-relaxed text-white/70">
                Los esenciales los instala el launcher. Los opcionales, como prefieras: los cambios
                se aplican al momento.
              </p>
            </div>
            <button
              type="button"
              class="-mr-1 -mt-1 inline-flex h-8 w-8 flex-shrink-0 items-center justify-center rounded-md text-white/50 transition-colors duration-150 hover:bg-white/[0.06] hover:text-white focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-400/70"
              aria-label="Cerrar"
              @click="emit('close')"
            >
              <i class="pi pi-times text-sm" aria-hidden="true" />
            </button>
          </header>

          <div
            v-if="banner"
            class="mx-5 mt-4 flex items-start gap-2 rounded-lg border px-3 py-2 text-xs leading-snug"
            :class="banner.tone"
          >
            <i class="pi mt-0.5 text-xs" :class="banner.icon" aria-hidden="true" />
            <span>{{ banner.text }}</span>
          </div>
          <p
            v-if="mods.notice"
            role="alert"
            class="mx-5 mt-3 rounded-lg border border-rose-400/30 bg-rose-400/[0.06] px-3 py-2 text-xs leading-snug text-rose-100/90"
          >
            {{ mods.notice }}
          </p>
          <p class="sr-only" aria-live="polite">{{ status }}</p>

          <div
            class="mt-4 min-h-0 flex-1 overflow-y-auto px-5 pb-1 [scrollbar-color:rgba(255,255,255,0.15)_transparent] [scrollbar-width:thin]"
          >
            <p
              v-if="mods.mods.length === 0"
              class="rounded-lg border border-white/10 bg-white/[0.03] px-4 py-6 text-center text-sm text-white/55"
            >
              {{ mods.checking ? "Buscando mods…" : "No hay mods disponibles por ahora." }}
            </p>

            <section v-if="mods.required.length > 0" aria-labelledby="mods-essential">
              <h4
                id="mods-essential"
                class="mb-2 text-[11px] font-medium uppercase tracking-widest text-white/45"
              >
                Esenciales
              </h4>
              <ul class="flex flex-col gap-2">
                <ModRow
                  v-for="mod in mods.required"
                  :key="mod.id"
                  :mod="mod"
                  :progress="mods.progressById[mod.id]"
                  :working="working(mod)"
                  :disabled="disabled"
                  @retry="mods.setEnabled(mod.id, true)"
                  @variant="mods.setVariant(mod.id, $event)"
                  @recommended="mods.installRecommended(mod.id)"
                  @conflicts="mods.disableConflicts(mod.id)"
                />
              </ul>
            </section>

            <section v-if="mods.optional.length > 0" class="mt-5" aria-labelledby="mods-optional">
              <h4
                id="mods-optional"
                class="mb-2 text-[11px] font-medium uppercase tracking-widest text-white/45"
              >
                Opcionales
              </h4>
              <div v-for="group in groups" :key="group.category" class="mb-3 last:mb-0">
                <h5
                  v-if="groups.length > 1"
                  class="mb-1.5 text-[11px] font-medium text-amber-200/70"
                >
                  {{ group.label }}
                </h5>
                <ul class="flex flex-col gap-2">
                  <ModRow
                    v-for="mod in group.mods"
                    :key="mod.id"
                    :mod="mod"
                    :progress="mods.progressById[mod.id]"
                    :working="working(mod)"
                    :disabled="disabled"
                    @toggle="mods.setEnabled(mod.id, $event)"
                    @retry="mods.setEnabled(mod.id, true)"
                    @variant="mods.setVariant(mod.id, $event)"
                    @recommended="mods.installRecommended(mod.id)"
                    @conflicts="mods.disableConflicts(mod.id)"
                  />
                </ul>
              </div>
            </section>
          </div>

          <footer
            class="mt-4 flex items-center justify-between gap-3 border-t border-white/5 bg-white/[0.02] px-5 py-3"
          >
            <span class="text-[11px] leading-snug text-white/40">
              Lo que el launcher reemplaza queda guardado y vuelve si quitas el mod.
            </span>
            <button
              type="button"
              data-autofocus
              class="inline-flex h-8 min-w-[80px] items-center justify-center rounded-md bg-white/10 px-3 text-xs font-bold text-white/90 transition-colors duration-150 hover:bg-white/15 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-400/70"
              @click="emit('close')"
            >
              Cerrar
            </button>
          </footer>
        </div>
      </div>
    </Transition>
  </Teleport>
</template>

<style scoped>
.panel-enter-active,
.panel-leave-active {
  transition: opacity 0.18s ease;
}
.panel-enter-from,
.panel-leave-to {
  opacity: 0;
}
.panel-enter-active .panel-card,
.panel-leave-active .panel-card {
  transition:
    transform 0.18s ease,
    opacity 0.18s ease;
}
.panel-enter-from .panel-card,
.panel-leave-to .panel-card {
  transform: scale(0.96) translateY(4px);
  opacity: 0;
}
</style>
