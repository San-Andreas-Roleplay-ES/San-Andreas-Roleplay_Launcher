import { computed, onMounted, onUnmounted, ref } from "vue"
import { launcher } from "@/api/launcher"
import type { ScoreboardEntry } from "@/types/scoreboard"

export const SCOREBOARD_REFRESH_MS = 6_000

export function useScoreboard(host: string, port: number) {
  const players = ref<ScoreboardEntry[]>([])
  const totalPlayers = ref<number | null>(null)
  const maxPlayers = ref<number | null>(null)
  const queriedAt = ref<number | null>(null)
  const loading = ref(true)
  const error = ref<string | null>(null)

  let timer: ReturnType<typeof setInterval> | null = null
  let inFlight: Promise<void> | null = null

  function load() {
    if (inFlight) return inFlight
    inFlight = (async () => {
      try {
        const snapshot = await launcher.fetchScoreboard(host, port)
        players.value = snapshot.players
        totalPlayers.value = snapshot.totalPlayers
        maxPlayers.value = snapshot.maxPlayers
        queriedAt.value = snapshot.queriedAt
        error.value = null
      } catch (err) {
        error.value =
          typeof err === "string" ? err : err instanceof Error ? err.message : "consulta fallida"
      } finally {
        loading.value = false
      }
    })().finally(() => {
      inFlight = null
    })
    return inFlight
  }

  const hasSnapshot = computed(() => queriedAt.value !== null)
  const throttled = computed(
    () => totalPlayers.value !== null && totalPlayers.value >= 100 && players.value.length === 0,
  )

  onMounted(() => {
    void load()
    timer = setInterval(() => void load(), SCOREBOARD_REFRESH_MS)
  })

  onUnmounted(() => {
    if (timer) clearInterval(timer)
  })

  return {
    players,
    totalPlayers,
    maxPlayers,
    queriedAt,
    loading,
    error,
    hasSnapshot,
    throttled,
    reload: load,
  }
}
