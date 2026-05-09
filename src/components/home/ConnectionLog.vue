<script setup lang="ts">
import { ref, watch, nextTick } from "vue"
import type { LogEntry } from "@/composables/useConnectionLog"

defineProps<{
  entries: readonly LogEntry[]
}>()

const open = ref(false)
const scrollRef = ref<HTMLElement | null>(null)

// auto-scroll cuando llegan entradas nuevas
watch(
  () => scrollRef.value?.scrollHeight,
  async () => {
    await nextTick()
    if (scrollRef.value) {
      scrollRef.value.scrollTop = scrollRef.value.scrollHeight
    }
  },
)

const levelColor: Record<LogEntry["level"], string> = {
  info: "text-white/50",
  ok: "text-emerald-400/80",
  error: "text-rose-400/80",
}

const levelDot: Record<LogEntry["level"], string> = {
  info: "bg-white/25",
  ok: "bg-emerald-400/60",
  error: "bg-rose-400/60",
}
</script>

<template>
  <section class="w-full">
    <!-- boton para abrir/cerrar -->
    <button
      type="button"
      class="group flex w-full items-center gap-2 rounded-lg px-3 py-2 text-left transition-colors duration-150 hover:bg-white/[0.04]"
      @click="open = !open"
    >
      <i
        class="pi pi-chevron-right text-[10px] text-white/30 transition-transform duration-200"
        :class="{ 'rotate-90': open }"
      />
      <span class="text-[11px] font-medium uppercase tracking-widest text-white/40">
        Registro de conexión
      </span>
      <span
        v-if="entries.length > 0"
        class="ml-auto text-[10px] tabular-nums text-white/25"
      >
        {{ entries.length }}
      </span>
    </button>

    <!-- panel colapsable -->
    <Transition name="log-panel">
      <div
        v-if="open"
        class="mt-1 overflow-hidden rounded-xl border border-white/[0.06] bg-white/[0.02]"
      >
        <div
          ref="scrollRef"
          class="max-h-40 overflow-y-auto px-3 py-2 font-mono text-[11px] leading-relaxed"
        >
          <!-- vacio -->
          <p
            v-if="entries.length === 0"
            class="py-3 text-center text-[11px] text-white/25"
          >
            Sin actividad todavia.
          </p>

          <!-- entradas -->
          <div
            v-for="entry in entries"
            :key="entry.id"
            class="flex items-start gap-2 py-[3px]"
          >
            <span class="mt-[5px] flex-shrink-0">
              <span
                class="inline-block h-[5px] w-[5px] rounded-full"
                :class="levelDot[entry.level]"
              />
            </span>
            <span class="flex-shrink-0 tabular-nums text-white/20">
              {{ entry.time }}
            </span>
            <span :class="levelColor[entry.level]">
              {{ entry.text }}
            </span>
          </div>
        </div>
      </div>
    </Transition>
  </section>
</template>

<style scoped>
.log-panel-enter-active,
.log-panel-leave-active {
  transition:
    opacity 0.2s ease,
    max-height 0.25s ease,
    margin-top 0.2s ease;
  overflow: hidden;
}

.log-panel-enter-from,
.log-panel-leave-to {
  opacity: 0;
  max-height: 0;
  margin-top: 0;
}

.log-panel-enter-to,
.log-panel-leave-from {
  max-height: 200px;
}

/* scrollbar */
div::-webkit-scrollbar {
  width: 3px;
}
div::-webkit-scrollbar-track {
  background: transparent;
}
div::-webkit-scrollbar-thumb {
  background: rgba(255, 255, 255, 0.08);
  border-radius: 3px;
}
</style>