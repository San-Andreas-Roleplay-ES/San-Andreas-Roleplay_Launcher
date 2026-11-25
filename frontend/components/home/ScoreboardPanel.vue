<script setup lang="ts">
import { computed, ref } from "vue"
import { useScoreboard } from "@/composables/useScoreboard"

const props = defineProps<{ host: string; port: number }>()
const {
  players,
  totalPlayers,
  maxPlayers,
  queriedAt,
  loading,
  error,
  hasSnapshot,
  throttled,
  reload,
} = useScoreboard(props.host, props.port)

type SortKey = "score" | "ping"
const sortKey = ref<SortKey>("score")
const sorted = computed(() =>
  [...players.value].sort((a, b) =>
    sortKey.value === "score" ? b.score - a.score : a.ping - b.ping,
  ),
)
const numberFormat = new Intl.NumberFormat("es-AR")
const formatNumber = (n: number) => numberFormat.format(n)
const pingClass = (ping: number) =>
  ping === 0
    ? "text-white/40"
    : ping < 80
      ? "text-emerald-300"
      : ping < 150
        ? "text-amber-300"
        : "text-red-300"
const updatedText = () => {
  if (queriedAt.value === null) return ""
  const s = Math.max(0, Math.floor((Date.now() - queriedAt.value) / 1000))
  return s < 2 ? "ahora" : `hace ${s} s`
}
const toggleSort = (key: SortKey) => (sortKey.value = key)
</script>

<template>
  <section
    class="relative w-full overflow-hidden rounded-xl border border-white/10 bg-white/[0.03] px-5 py-5 backdrop-blur-sm"
  >
    <header class="mb-3 flex items-center justify-between gap-3">
      <div class="flex min-w-0 items-center gap-2">
        <h2 class="text-xs font-medium uppercase tracking-widest text-white/50">Scoreboard</h2>
        <span class="text-xs text-white/35">
          {{ totalPlayers ?? "—" }}/{{ maxPlayers ?? "—" }} en línea
        </span>
      </div>
      <button
        type="button"
        class="flex h-6 w-6 flex-shrink-0 items-center justify-center rounded-md text-white/40 transition-colors duration-150 hover:bg-white/[0.06] hover:text-white active:scale-95 disabled:opacity-40"
        title="Actualizar jugadores"
        :disabled="loading"
        @click="reload()"
      >
        <i class="pi pi-refresh text-xs" />
      </button>
    </header>

    <div
      v-if="loading && !hasSnapshot"
      class="py-3 text-xs italic text-white/40"
      aria-live="polite"
    >
      Consultando jugadores...
    </div>

    <div v-else-if="!hasSnapshot" class="py-3 text-xs text-amber-200/70" aria-live="polite">
      Lista no disponible — {{ error ?? "sin respuesta del servidor" }}
    </div>

    <div v-else-if="players.length === 0" class="py-3 text-xs text-white/45" aria-live="polite">
      {{
        throttled
          ? "El servidor no publica la lista con 100 o más jugadores conectados."
          : "No hay jugadores conectados."
      }}
    </div>

    <template v-else>
      <div class="grid grid-cols-[1.75rem_1fr_4rem_4.5rem] gap-2 border-b border-white/5 pb-1.5">
        <span class="text-[10px] uppercase tracking-wider text-white/30">#</span>
        <span class="text-[10px] uppercase tracking-wider text-white/30">Jugador</span>
        <button
          type="button"
          class="text-right text-[10px] uppercase tracking-wider transition-colors hover:text-white/70"
          :class="sortKey === 'score' ? 'text-amber-200/80' : 'text-white/30'"
          title="Ordenar por score"
          @click="toggleSort('score')"
        >
          Score
        </button>
        <button
          type="button"
          class="text-right text-[10px] uppercase tracking-wider transition-colors hover:text-white/70"
          :class="sortKey === 'ping' ? 'text-amber-200/80' : 'text-white/30'"
          title="Ordenar por ping"
          @click="toggleSort('ping')"
        >
          Ping
        </button>
      </div>

      <div class="max-h-52 overflow-y-auto overscroll-contain pr-1">
        <div
          v-for="(player, index) in sorted"
          :key="player.id"
          class="grid grid-cols-[1.75rem_1fr_4rem_4.5rem] items-center gap-2 border-b border-white/[0.04] py-2 last:border-0"
        >
          <span class="font-mono text-[11px] text-white/30">{{ index + 1 }}</span>
          <span class="truncate text-xs text-white/85" :title="player.name">{{ player.name }}</span>
          <span class="text-right font-mono text-xs text-white/70">
            {{ formatNumber(player.score) }}
          </span>
          <span class="text-right font-mono text-xs" :class="pingClass(player.ping)">
            {{ player.ping === 0 ? "—" : `${player.ping} ms` }}
          </span>
        </div>
      </div>

      <footer class="mt-3 flex items-center justify-end gap-1.5 text-[10px] text-white/35">
        <span class="flex items-center gap-1.5">
          <span v-if="error" class="text-amber-200/60">Reconectando</span>
          <span v-if="error && queriedAt !== null">·</span>
          <span v-if="queriedAt !== null">Actualizado {{ updatedText() }}</span>
        </span>
      </footer>
    </template>
  </section>
</template>
