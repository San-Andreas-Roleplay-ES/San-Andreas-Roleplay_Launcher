<script setup lang="ts">
import { computed, ref, useTemplateRef } from "vue"
import { useFocusTrap } from "@/composables/useFocusTrap"
import { useHealthCheckStore } from "@/stores/healthCheck"
import { RWGUARD_BUTTON_ID, useRwGuardStore } from "@/stores/rwguard"
import type { RwGuardThreat, RwGuardVerdict } from "@/types/rwguard"
import { formatBytes } from "@/utils/formatBytes"

const props = defineProps<{ open: boolean }>()
const emit = defineEmits<{ close: [] }>()

const rwguard = useRwGuardStore()
const health = useHealthCheckStore()

const card = useTemplateRef<HTMLElement>("card")
useFocusTrap(
  card,
  () => props.open,
  () => emit("close"),
  () => document.getElementById(RWGUARD_BUTTON_ID),
)

const query = ref("")

const busy = computed(() => rwguard.scanning || health.cacheInstalling)
const actionsDisabled = computed(() => busy.value || health.gameRunning)

const allThreats = computed<RwGuardThreat[]>(() => [
  ...rwguard.activeThreats,
  ...rwguard.blockedThreats,
])

const filteredHistory = computed(() => {
  const list = rwguard.status?.recentHistory ?? []
  const term = query.value.trim().toLowerCase()
  if (!term) return list
  return list.filter(
    (item) =>
      item.fileName.toLowerCase().includes(term) ||
      (item.reason?.toLowerCase().includes(term) ?? false),
  )
})

const scanPercent = computed(() => {
  const p = rwguard.progress
  if (!p || p.total <= 0) return 0
  return Math.min(100, Math.round((p.checked / p.total) * 100))
})

const banner = computed<{ text: string; tone: string; icon: string } | null>(() => {
  if (health.gameRunning) {
    return {
      text: "Cierra GTA San Andreas para reparar o eliminar modelos del caché.",
      tone: "border-sky-400/30 bg-sky-400/[0.06] text-sky-100/90",
      icon: "pi-info-circle text-sky-300",
    }
  }
  if (health.cacheInstalling) {
    return {
      text: "Sincronizando caché: RWGuard está validando cada modelo nuevo en simultáneo.",
      tone: "border-emerald-400/30 bg-emerald-400/[0.06] text-emerald-100/90",
      icon: "pi-shield text-emerald-300",
    }
  }
  if (rwguard.activeThreatCount > 0) {
    return {
      text: "Se encontraron modelos con estructuras RenderWare inseguras. Puedes repararlos o eliminarlos.",
      tone: "border-rose-400/30 bg-rose-400/[0.08] text-rose-100/90",
      icon: "pi-exclamation-triangle text-rose-300",
    }
  }
  if (rwguard.blockedThreatCount > 0) {
    return {
      text: "RWGuard bloqueó modelos inseguros antes de guardarlos en tu carpeta de caché.",
      tone: "border-amber-400/30 bg-amber-400/[0.06] text-amber-100/90",
      icon: "pi-shield text-amber-300",
    }
  }
  return null
})

function formatCount(n: number): string {
  return n.toLocaleString("es-ES")
}

function formatTime(unixSecs: number | undefined): string {
  if (!unixSecs) return "—"
  try {
    return new Intl.DateTimeFormat("es-ES", {
      day: "2-digit",
      month: "2-digit",
      hour: "2-digit",
      minute: "2-digit",
    }).format(new Date(unixSecs * 1000))
  } catch {
    return "—"
  }
}

function verdictMeta(verdict: RwGuardVerdict): { label: string; cls: string } {
  switch (verdict) {
    case "safe":
      return {
        label: "Seguro",
        cls: "border-emerald-400/30 bg-emerald-400/10 text-emerald-300",
      }
    case "healed":
      return {
        label: "Reparado",
        cls: "border-sky-400/30 bg-sky-400/10 text-sky-300",
      }
    case "blocked":
      return {
        label: "Bloqueado",
        cls: "border-amber-400/30 bg-amber-400/10 text-amber-300",
      }
    case "threat":
      return {
        label: "Amenaza",
        cls: "border-rose-400/30 bg-rose-400/10 text-rose-300",
      }
  }
}

function handleHealAll(): void {
  void rwguard.heal()
}

function handleHealOne(fileName: string): void {
  void rwguard.heal(fileName)
}

function handleDeleteAll(): void {
  void rwguard.deleteThreat()
}

function handleDeleteOne(fileName: string): void {
  void rwguard.deleteThreat(fileName)
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
          aria-labelledby="rwguard-title"
          aria-describedby="rwguard-subtitle"
          tabindex="-1"
          class="panel-card flex max-h-[85vh] w-full max-w-xl flex-col overflow-hidden rounded-xl border border-white/10 bg-neutral-900/95 shadow-2xl ring-1 backdrop-blur-md focus:outline-none"
          :class="rwguard.attention ? 'ring-rose-400/25' : 'ring-emerald-400/20'"
        >
          <header class="flex items-start gap-3 px-5 pt-5">
            <i
              class="pi pi-shield mt-0.5 text-xl"
              :class="rwguard.attention ? 'text-amber-300' : 'text-emerald-400'"
              aria-hidden="true"
            />
            <div class="min-w-0 flex-1">
              <div class="flex items-center gap-2">
                <h3 id="rwguard-title" class="text-sm font-semibold text-white">
                  RWGuard · Protección nativa
                </h3>
                <span
                  class="rounded-full border px-2 py-0.5 text-[10px] font-medium uppercase tracking-wider"
                  :class="
                    rwguard.attention
                      ? 'border-amber-400/30 bg-amber-400/10 text-amber-300'
                      : 'border-emerald-400/30 bg-emerald-400/10 text-emerald-300'
                  "
                >
                  {{ rwguard.attention ? "Atención" : "Activo" }}
                </span>
              </div>
              <p id="rwguard-subtitle" class="mt-1 text-xs leading-relaxed text-white/65">
                Analiza todos los modelos RenderWare (<code>.dff</code> y <code>.txd</code>) del
                caché y valida cada descarga nueva en simultáneo. El historial evita re-analizar
                archivos que no cambiaron.
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

          <!-- Summary KPI strip -->
          <div class="mx-5 mt-4 grid grid-cols-4 gap-2">
            <div class="rounded-lg border border-white/10 bg-white/[0.03] px-3 py-2">
              <div class="text-[10px] uppercase tracking-wider text-white/45">Validados</div>
              <div class="mt-0.5 font-mono text-base font-bold tabular-nums text-emerald-300">
                {{ formatCount(rwguard.status?.validatedModels ?? 0) }}
              </div>
            </div>
            <div class="rounded-lg border border-white/10 bg-white/[0.03] px-3 py-2">
              <div class="text-[10px] uppercase tracking-wider text-white/45">Nuevos</div>
              <div class="mt-0.5 font-mono text-base font-bold tabular-nums text-white/90">
                {{ formatCount(rwguard.status?.newlyScanned ?? 0) }}
              </div>
            </div>
            <div class="rounded-lg border border-white/10 bg-white/[0.03] px-3 py-2">
              <div class="text-[10px] uppercase tracking-wider text-white/45">Historial</div>
              <div class="mt-0.5 font-mono text-base font-bold tabular-nums text-white/75">
                {{ formatCount(rwguard.status?.reusedFromHistory ?? 0) }}
              </div>
            </div>
            <div class="rounded-lg border border-white/10 bg-white/[0.03] px-3 py-2">
              <div class="text-[10px] uppercase tracking-wider text-white/45">Amenazas</div>
              <div
                class="mt-0.5 font-mono text-base font-bold tabular-nums"
                :class="rwguard.totalThreatCount > 0 ? 'text-rose-400' : 'text-white/50'"
              >
                {{ formatCount(rwguard.totalThreatCount) }}
              </div>
            </div>
          </div>

          <!-- Active scan progress bar -->
          <div
            v-if="rwguard.progress?.scanning"
            class="mx-5 mt-3 rounded-lg border border-emerald-400/20 bg-emerald-400/[0.05] px-3 py-2"
          >
            <div class="flex items-center justify-between gap-2 text-xs text-emerald-200">
              <span class="truncate">
                {{ rwguard.progress.message ?? "Analizando modelos del caché..." }}
              </span>
              <span class="font-mono text-[11px] tabular-nums">{{ scanPercent }}%</span>
            </div>
            <div class="mt-1.5 h-1.5 w-full overflow-hidden rounded-full bg-white/10">
              <div
                class="h-full rounded-full bg-emerald-400 transition-[width] duration-150"
                :style="{ width: `${scanPercent}%` }"
              />
            </div>
          </div>

          <div
            v-if="banner"
            class="mx-5 mt-3 flex items-start gap-2 rounded-lg border px-3 py-2 text-xs leading-snug"
            :class="banner.tone"
          >
            <i class="pi mt-0.5 text-xs" :class="banner.icon" aria-hidden="true" />
            <span>{{ banner.text }}</span>
          </div>

          <p
            v-if="rwguard.notice"
            role="alert"
            class="mx-5 mt-3 rounded-lg border border-rose-400/30 bg-rose-400/[0.06] px-3 py-2 text-xs leading-snug text-rose-100/90"
          >
            {{ rwguard.notice }}
          </p>

          <div
            class="mt-4 min-h-0 flex-1 overflow-y-auto px-5 pb-1 [scrollbar-color:rgba(255,255,255,0.15)_transparent] [scrollbar-width:thin]"
          >
            <!-- Threats section -->
            <section v-if="allThreats.length > 0" class="mb-4" aria-labelledby="rwguard-threats">
              <div class="mb-2 flex items-center justify-between gap-2">
                <h4
                  id="rwguard-threats"
                  class="text-[11px] font-medium uppercase tracking-widest text-rose-300/80"
                >
                  Amenazas detectadas ({{ allThreats.length }})
                </h4>
                <div class="flex items-center gap-1.5">
                  <button
                    v-if="rwguard.activeThreatCount > 0"
                    type="button"
                    class="inline-flex h-6 items-center gap-1 rounded-md bg-amber-500/90 px-2 text-[11px] font-bold text-black transition-colors hover:bg-amber-400 disabled:cursor-not-allowed disabled:opacity-50"
                    :disabled="actionsDisabled"
                    @click="handleHealAll"
                  >
                    Reparar todo
                  </button>
                  <button
                    type="button"
                    class="inline-flex h-6 items-center gap-1 rounded-md bg-rose-500/20 px-2 text-[11px] font-semibold text-rose-200 transition-colors hover:bg-rose-500/30 disabled:cursor-not-allowed disabled:opacity-50"
                    :disabled="actionsDisabled"
                    @click="handleDeleteAll"
                  >
                    {{ rwguard.activeThreatCount > 0 ? "Eliminar todo" : "Limpiar avisos" }}
                  </button>
                </div>
              </div>
              <ul class="flex flex-col gap-1.5">
                <li
                  v-for="threat in allThreats"
                  :key="`${threat.source}-${threat.fileName}`"
                  class="flex items-center justify-between gap-3 rounded-lg border border-rose-400/20 bg-rose-500/[0.06] px-3 py-2"
                >
                  <div class="min-w-0 flex-1">
                    <div class="flex items-center gap-2">
                      <span class="truncate font-mono text-xs font-medium text-white/95">
                        {{ threat.fileName }}
                      </span>
                      <span
                        class="flex-shrink-0 rounded border px-1.5 py-0.5 text-[10px] font-medium"
                        :class="
                          threat.source === 'download'
                            ? 'border-amber-400/30 bg-amber-400/10 text-amber-300'
                            : 'border-rose-400/30 bg-rose-400/10 text-rose-300'
                        "
                      >
                        {{ threat.source === "download" ? "Bloqueado" : "En caché" }}
                      </span>
                    </div>
                    <div class="mt-0.5 text-[11px] text-rose-200/75">
                      {{ threat.reason }} · {{ formatBytes(threat.size) }}
                    </div>
                  </div>
                  <div class="flex flex-shrink-0 items-center gap-1.5">
                    <button
                      v-if="threat.source === 'cache'"
                      type="button"
                      class="inline-flex h-6 items-center rounded-md bg-white/10 px-2 text-[11px] font-semibold text-white/90 transition-colors hover:bg-white/15 disabled:cursor-not-allowed disabled:opacity-50"
                      :disabled="actionsDisabled"
                      @click="handleHealOne(threat.fileName)"
                    >
                      Reparar
                    </button>
                    <button
                      type="button"
                      class="inline-flex h-6 items-center rounded-md bg-rose-500/20 px-2 text-[11px] font-semibold text-rose-200 transition-colors hover:bg-rose-500/30 disabled:cursor-not-allowed disabled:opacity-50"
                      :disabled="actionsDisabled"
                      @click="handleDeleteOne(threat.fileName)"
                    >
                      {{ threat.source === "cache" ? "Eliminar" : "Quitar" }}
                    </button>
                  </div>
                </li>
              </ul>
            </section>

            <!-- Recent validation history -->
            <section aria-labelledby="rwguard-history">
              <div class="mb-2 flex items-center justify-between gap-2">
                <h4
                  id="rwguard-history"
                  class="text-[11px] font-medium uppercase tracking-widest text-white/45"
                >
                  Historial reciente de modelos
                </h4>
                <input
                  v-if="(rwguard.status?.recentHistory.length ?? 0) > 0"
                  v-model="query"
                  type="text"
                  placeholder="Filtrar archivo…"
                  class="h-6 w-40 rounded-md border border-white/10 bg-white/[0.04] px-2 text-[11px] text-white/85 placeholder:text-white/35 focus:border-emerald-400/50 focus:outline-none"
                />
              </div>

              <div
                v-if="filteredHistory.length === 0"
                class="rounded-lg border border-white/10 bg-white/[0.03] px-4 py-6 text-center text-xs text-white/55"
              >
                {{
                  rwguard.scanning
                    ? "Validando modelos del caché…"
                    : query.trim()
                      ? "Ningún archivo coincide con ese filtro."
                      : "Todavía no hay modelos registrados en el historial."
                }}
              </div>

              <ul
                v-else
                class="divide-y divide-white/5 rounded-lg border border-white/10 bg-white/[0.02]"
              >
                <li
                  v-for="item in filteredHistory"
                  :key="`${item.verdict}-${item.fileName}`"
                  class="flex items-center justify-between gap-3 px-3 py-1.5 text-xs"
                >
                  <div class="min-w-0 flex-1">
                    <span class="block truncate font-mono text-[11px] text-white/85">
                      {{ item.fileName }}
                    </span>
                    <span v-if="item.reason" class="block truncate text-[10px] text-rose-300/80">
                      {{ item.reason }}
                    </span>
                  </div>
                  <span class="flex-shrink-0 font-mono text-[10px] tabular-nums text-white/40">
                    {{ formatBytes(item.size) }}
                  </span>
                  <span class="flex-shrink-0 font-mono text-[10px] tabular-nums text-white/35">
                    {{ formatTime(item.checkedAt) }}
                  </span>
                  <span
                    class="flex-shrink-0 rounded border px-1.5 py-0.5 text-[10px] font-medium"
                    :class="verdictMeta(item.verdict).cls"
                  >
                    {{ verdictMeta(item.verdict).label }}
                  </span>
                </li>
              </ul>
            </section>
          </div>

          <footer
            class="mt-4 flex flex-wrap items-center justify-between gap-2 border-t border-white/5 bg-white/[0.02] px-5 py-3"
          >
            <span class="text-[11px] leading-snug text-white/40">
              Última revisión: {{ formatTime(rwguard.status?.lastScanAt) }}
            </span>
            <div class="flex items-center gap-2">
              <button
                type="button"
                class="inline-flex h-8 items-center gap-1.5 rounded-md border border-white/15 bg-transparent px-2.5 text-xs font-medium text-white/75 transition-colors hover:border-white/30 hover:bg-white/[0.04] hover:text-white disabled:cursor-not-allowed disabled:opacity-45"
                title="Analiza únicamente los modelos nuevos o modificados"
                :disabled="busy"
                @click="rwguard.scan({ force: false })"
              >
                <i class="pi pi-search text-[11px]" aria-hidden="true" />
                <span>Analizar nuevos</span>
              </button>
              <button
                type="button"
                class="inline-flex h-8 items-center gap-1.5 rounded-md border border-white/15 bg-transparent px-2.5 text-xs font-medium text-white/75 transition-colors hover:border-white/30 hover:bg-white/[0.04] hover:text-white disabled:cursor-not-allowed disabled:opacity-45"
                title="Ignora el historial y re-valida todos los modelos del caché"
                :disabled="busy"
                @click="rwguard.scan({ force: true })"
              >
                <i class="pi pi-refresh text-[11px]" aria-hidden="true" />
                <span>Verificar todo</span>
              </button>
              <button
                type="button"
                data-autofocus
                class="inline-flex h-8 min-w-[76px] items-center justify-center rounded-md bg-white/10 px-3 text-xs font-bold text-white/90 transition-colors hover:bg-white/15 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-400/70"
                @click="emit('close')"
              >
                Cerrar
              </button>
            </div>
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
