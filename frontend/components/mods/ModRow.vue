<script setup lang="ts">
import { computed } from "vue"
import ModSwitch from "./ModSwitch.vue"
import VariantPicker from "./VariantPicker.vue"
import type { ModsProgress, ModStatus } from "@/types/mods"
import { formatBytes } from "@/utils/formatBytes"

const props = defineProps<{
  mod: ModStatus
  /** Its progress while a run works on it. */
  progress?: ModsProgress
  /** In the run going on now. */
  working: boolean
  /** Controls unavailable: a run going on, or the game open. */
  disabled: boolean
}>()

const emit = defineEmits<{
  toggle: [enabled: boolean]
  variant: [id: string]
  retry: []
  recommended: []
  conflicts: []
}>()

interface Chip {
  label: string
  tone: string
}

const chip = computed<Chip>(() => {
  const m = props.mod
  if (props.working) return { label: "Instalando…", tone: "border-amber-400/40 text-amber-300" }
  if (m.error) return { label: "Error", tone: "border-rose-400/40 text-rose-300" }
  switch (m.state) {
    case "installed":
      return { label: "Instalado", tone: "border-emerald-400/40 text-emerald-300" }
    case "outdated":
      return { label: "Actualización pendiente", tone: "border-amber-400/40 text-amber-300" }
    case "damaged":
      return { label: "Dañado", tone: "border-rose-400/40 text-rose-300" }
    case "foreign":
      return { label: "Instalado por tu cuenta", tone: "border-sky-400/40 text-sky-300" }
    case "blocked":
      return { label: "No disponible", tone: "border-white/15 text-white/50" }
    case "notInstalled":
      return m.enabled
        ? { label: "Pendiente", tone: "border-amber-400/30 text-amber-200/80" }
        : { label: "Desactivado", tone: "border-white/15 text-white/50" }
  }
  return { label: "", tone: "" }
})

const selected = computed(
  () => props.mod.variants.find((v) => v.id === props.mod.variant) ?? props.mod.variants[0],
)

/** "12 MB · v2.0 (tienes v1.9)" */
const meta = computed(() => {
  const parts: string[] = []
  if (selected.value) parts.push(formatBytes(selected.value.bytes))
  const installed = props.mod.installed
  if (installed && selected.value && installed.version !== selected.value.version) {
    parts.push(`v${selected.value.version} (tienes v${installed.version})`)
  } else if (selected.value) {
    parts.push(`v${selected.value.version}`)
  }
  if (props.mod.requires.length > 0) parts.push(`Necesita ${props.mod.requires.join(", ")}`)
  return parts.join(" · ")
})

const locked = computed(() => props.mod.required || props.mod.requiredBy.length > 0)

const switchHint = computed(() => {
  if (props.mod.required) return "Esencial: el launcher lo mantiene instalado."
  if (props.mod.requiredBy.length > 0) return `Lo necesita ${props.mod.requiredBy.join(", ")}.`
  if (props.mod.state === "blocked" && !props.mod.enabled) return props.mod.detail
  return props.mod.enabled ? `Desactivar ${props.mod.name}` : `Activar ${props.mod.name}`
})

const switchDisabled = computed(
  () => props.disabled || (props.mod.state === "blocked" && !props.mod.enabled),
)

const percent = computed(() => Math.min(100, Math.max(0, props.progress?.percent ?? 0)))
const measured = computed(
  () => props.progress?.phase === "download" && (props.progress.bytesTotal ?? 0) > 0,
)

/** Folders as Windows shows them: `modloader\radar_hq`. */
const paths = (list: string[]) => list.map((p) => p.replace(/\//g, "\\")).join(", ")

const btn =
  "inline-flex h-7 flex-shrink-0 items-center rounded-md px-2.5 text-xs font-bold transition-colors duration-150 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-400/70 disabled:cursor-not-allowed disabled:opacity-60"
</script>

<template>
  <li
    class="rounded-lg border bg-white/[0.03] px-4 py-3 transition-colors duration-150"
    :class="mod.blocking ? 'border-rose-400/30' : 'border-white/10'"
    :aria-busy="working"
  >
    <div class="flex items-start gap-3">
      <div class="min-w-0 flex-1">
        <div class="flex flex-wrap items-center gap-x-2 gap-y-1">
          <h4 class="text-sm font-medium leading-tight text-white/90">{{ mod.name }}</h4>
          <span
            class="inline-flex items-center rounded-full border px-2 py-0.5 text-[10px] font-medium leading-none"
            :class="chip.tone"
          >
            {{ chip.label }}
          </span>
        </div>
        <p v-if="mod.description" class="mt-1 text-xs leading-snug text-white/55">
          {{ mod.description }}
        </p>
        <p class="mt-1 text-[11px] leading-snug text-white/40">{{ meta }}</p>
      </div>
      <ModSwitch
        :checked="mod.enabled"
        :label="mod.required ? `${mod.name}: esencial` : `Activar ${mod.name}`"
        :locked="locked"
        :disabled="switchDisabled"
        :hint="switchHint"
        @change="emit('toggle', $event)"
      />
    </div>

    <VariantPicker
      v-if="mod.variants.length > 1"
      class="mt-2.5"
      :variants="mod.variants"
      :selected="mod.variant"
      :label="`Variante de ${mod.name}`"
      :disabled="disabled || mod.state === 'blocked'"
      @select="emit('variant', $event)"
    />

    <div
      v-if="working"
      class="mt-2.5 flex items-center gap-3"
      role="progressbar"
      :aria-label="progress?.message ?? `Instalando ${mod.name}`"
      :aria-valuenow="measured ? Math.floor(percent) : undefined"
      aria-valuemin="0"
      aria-valuemax="100"
    >
      <div class="relative h-1.5 flex-1 overflow-hidden rounded-full bg-white/10">
        <div
          v-if="measured"
          class="h-full rounded-full bg-amber-500 transition-[width] duration-150"
          :style="{ width: `${percent}%` }"
        />
        <div v-else class="mod-bar-indeterminate" />
      </div>
      <span class="flex-shrink-0 text-[11px] text-white/55">
        {{ progress?.message ?? "Preparando…" }}
      </span>
    </div>

    <!-- What needs a decision, one note each. -->
    <div v-if="mod.error && !working" class="mt-2.5 flex items-start gap-2">
      <i class="pi pi-times-circle mt-0.5 text-xs text-rose-300" aria-hidden="true" />
      <p class="min-w-0 flex-1 text-xs leading-snug text-rose-200/90">
        {{ mod.error }}
        <span v-if="mod.blocking" class="text-rose-200/70"
          >Hasta instalarlo no se puede jugar.</span
        >
      </p>
      <button
        type="button"
        :class="[
          btn,
          mod.blocking
            ? 'bg-amber-500/90 text-black hover:bg-amber-400'
            : 'bg-white/10 text-white/90 hover:bg-white/15',
        ]"
        :disabled="disabled"
        @click="emit('retry')"
      >
        Reintentar
      </button>
    </div>

    <div v-if="mod.state === 'foreign' && !working" class="mt-2.5 flex items-start gap-2">
      <i class="pi pi-info-circle mt-0.5 text-xs text-sky-300" aria-hidden="true" />
      <p class="min-w-0 flex-1 text-xs leading-snug text-white/60">
        Ya está instalado por tu cuenta, así que el launcher no lo toca.
      </p>
      <button
        type="button"
        :class="[btn, 'bg-white/10 text-white/90 hover:bg-white/15']"
        :disabled="disabled"
        title="Instala la versión del servidor; lo que reemplace queda guardado para volver atrás."
        @click="emit('recommended')"
      >
        Instalar versión recomendada
      </button>
    </div>

    <div v-if="mod.conflicts.length > 0 && !working" class="mt-2.5 flex items-start gap-2">
      <i class="pi pi-exclamation-triangle mt-0.5 text-xs text-amber-300" aria-hidden="true" />
      <p class="min-w-0 flex-1 text-xs leading-snug text-white/60">
        También tienes {{ paths(mod.conflicts) }}, que puede chocar con este mod.
      </p>
      <button
        type="button"
        :class="[btn, 'bg-amber-500/90 text-black hover:bg-amber-400']"
        :disabled="disabled"
        title="Se renombra con un punto delante (Modloader lo ignora). Vuelve si quitas este mod."
        @click="emit('conflicts')"
      >
        {{ mod.conflicts.length === 1 ? "Desactivar el otro" : "Desactivar los otros" }}
      </button>
    </div>

    <p
      v-if="mod.disabledConflicts.length > 0 && !working"
      class="mt-2 text-[11px] leading-snug text-white/40"
    >
      Desactivado: {{ paths(mod.disabledConflicts) }}. Vuelve si quitas este mod.
    </p>
    <p
      v-if="mod.state === 'blocked' && mod.detail"
      class="mt-2 text-[11px] leading-snug text-white/45"
    >
      {{ mod.detail }}
    </p>
  </li>
</template>

<style scoped>
.mod-bar-indeterminate {
  position: absolute;
  inset: 0;
  width: 40%;
  border-radius: 9999px;
  background: linear-gradient(
    90deg,
    rgba(245, 158, 11, 0) 0%,
    rgba(245, 158, 11, 0.85) 50%,
    rgba(245, 158, 11, 0) 100%
  );
  animation: mod-bar-slide 1.4s ease-in-out infinite;
  will-change: transform;
}

@keyframes mod-bar-slide {
  0% {
    transform: translateX(-100%);
  }
  100% {
    transform: translateX(350%);
  }
}
</style>
