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
  /** One word at most; none for "installed". */
  label: string
  icon: string
  tone: string
  /** The longer story, on hover. */
  title: string
}

const selected = computed(
  () => props.mod.variants.find((v) => v.id === props.mod.variant) ?? props.mod.variants[0],
)

const chip = computed<Chip | null>(() => {
  const m = props.mod
  const version = m.installed ? `v${m.installed.version}` : ""
  if (props.working) {
    return { label: "Instalando", icon: "pi-spinner pi-spin", tone: "text-amber-300", title: "" }
  }
  if (m.error) {
    return { label: "Error", icon: "pi-times-circle", tone: "text-rose-300", title: m.error }
  }
  switch (m.state) {
    case "installed":
      return {
        label: "",
        icon: "pi-check-circle",
        tone: "text-emerald-400",
        title: `Instalado ${version}`,
      }
    case "outdated":
      return {
        label: "Actualización",
        icon: "pi-arrow-circle-up",
        tone: "text-amber-300",
        title: `Hay una versión nueva (tienes ${version}).`,
      }
    case "damaged":
      return {
        label: "Dañado",
        icon: "pi-exclamation-circle",
        tone: "text-rose-300",
        title: "Le faltan archivos: se va a reparar.",
      }
    case "foreign":
      return {
        label: "Tuyo",
        icon: "pi-user",
        tone: "text-sky-300",
        title: "Instalado por tu cuenta: el launcher no lo toca.",
      }
    case "blocked":
      return {
        label: "Bloqueado",
        icon: "pi-lock",
        tone: "text-white/50",
        title: m.detail ?? "No disponible.",
      }
    case "notInstalled":
      return m.enabled
        ? {
            label: "Pendiente",
            icon: "pi-clock",
            tone: "text-amber-200/80",
            title: "Se va a instalar.",
          }
        : null
  }
  return null
})

/** Version, size and what it needs: secondary, on the name's tooltip. */
const details = computed(() => {
  const parts: string[] = []
  if (selected.value) parts.push(`v${selected.value.version}`, formatBytes(selected.value.bytes))
  if (props.mod.requires.length > 0) parts.push(`Necesita ${props.mod.requires.join(", ")}`)
  return parts.join(" · ")
})

const locked = computed(() => props.mod.required || props.mod.requiredBy.length > 0)

const switchHint = computed(() => {
  if (props.mod.required) return "Esencial"
  if (props.mod.requiredBy.length > 0) return `Lo necesita ${props.mod.requiredBy.join(", ")}`
  if (props.mod.state === "blocked" && !props.mod.enabled) return props.mod.detail
  return props.mod.enabled ? "Desactivar" : "Activar"
})

const switchDisabled = computed(
  () => props.disabled || (props.mod.state === "blocked" && !props.mod.enabled),
)

const percent = computed(() => Math.min(100, Math.max(0, props.progress?.percent ?? 0)))
const measured = computed(
  () => props.progress?.phase === "download" && (props.progress.bytesTotal ?? 0) > 0,
)

/** The folder's own name: `modloader/radar_hq` → `radar_hq`. */
const clash = computed(() => props.mod.conflicts.map((p) => p.split("/").pop()).join(", "))
const clashTitle = computed(
  () =>
    `${props.mod.conflicts.map((p) => p.replace(/\//g, "\\")).join(", ")}: se renombra con un punto delante (Modloader lo ignora) y vuelve si quitas este mod.`,
)

const btn =
  "inline-flex h-6 flex-shrink-0 items-center rounded-md px-2 text-[11px] font-bold transition-colors duration-150 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-400/70 disabled:cursor-not-allowed disabled:opacity-60"
</script>

<template>
  <li
    class="rounded-lg border bg-white/[0.03] px-3.5 py-2.5 transition-colors duration-150"
    :class="mod.blocking ? 'border-rose-400/30' : 'border-white/10'"
    :aria-busy="working"
  >
    <div class="flex items-center gap-3">
      <div class="min-w-0 flex-1">
        <div class="flex min-w-0 items-center gap-2">
          <h4 class="truncate text-sm font-medium leading-tight text-white/90" :title="details">
            {{ mod.name }}
          </h4>
          <span
            v-if="chip"
            class="inline-flex flex-shrink-0 items-center gap-1 text-[11px] font-medium leading-none"
            :class="chip.tone"
            :title="chip.title || undefined"
          >
            <i class="pi text-[11px]" :class="chip.icon" aria-hidden="true" />
            <span v-if="chip.label">{{ chip.label }}</span>
            <span v-else class="sr-only">{{ chip.title }}</span>
          </span>
        </div>
        <p
          v-if="mod.description"
          class="mt-0.5 truncate text-xs leading-snug text-white/50"
          :title="mod.description"
        >
          {{ mod.description }}
        </p>
      </div>
      <button
        v-if="mod.state === 'foreign' && !working"
        type="button"
        :class="[btn, 'bg-white/10 text-white/90 hover:bg-white/15']"
        :disabled="disabled"
        title="Instala la versión oficial; la tuya queda respaldada."
        @click="emit('recommended')"
      >
        Usar oficial
      </button>
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
      class="mt-2"
      :variants="mod.variants"
      :selected="mod.variant"
      :label="`Variante de ${mod.name}`"
      :disabled="disabled || mod.state === 'blocked'"
      @select="emit('variant', $event)"
    />

    <div
      v-if="working"
      class="relative mt-2 h-1 overflow-hidden rounded-full bg-white/10"
      role="progressbar"
      :aria-label="progress?.message ?? `Instalando ${mod.name}`"
      :aria-valuenow="measured ? Math.floor(percent) : undefined"
      aria-valuemin="0"
      aria-valuemax="100"
    >
      <div
        v-if="measured"
        class="h-full rounded-full bg-amber-500 transition-[width] duration-150"
        :style="{ width: `${percent}%` }"
      />
      <div v-else class="mod-bar-indeterminate" />
    </div>

    <!-- At most one line each, a short verb to act. -->
    <div v-if="mod.error && !working" class="mt-2 flex items-center gap-2">
      <p class="min-w-0 flex-1 truncate text-xs text-rose-200/90" :title="mod.error">
        {{ mod.error }}
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

    <div v-if="mod.conflicts.length > 0 && !working" class="mt-2 flex items-center gap-2">
      <p class="min-w-0 flex-1 truncate text-xs text-amber-100/80" :title="clashTitle">
        Choca con {{ clash }}
      </p>
      <button
        type="button"
        :class="[btn, 'bg-amber-500/90 text-black hover:bg-amber-400']"
        :disabled="disabled"
        :title="clashTitle"
        @click="emit('conflicts')"
      >
        Desactivar
      </button>
    </div>
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
