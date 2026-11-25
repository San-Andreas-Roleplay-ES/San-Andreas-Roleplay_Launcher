import { onMounted, onUnmounted, ref } from "vue"
import { launcher } from "@/api/launcher"
import type { Unsubscribe } from "@/api/launcher"
import type { GameLaunchResult } from "@/types/game"

export type GamePhase = "idle" | "launching" | "in-game"

const LAUNCH_MESSAGES: Array<{ atMs: number; text: string }> = [
  { atMs: 0, text: "Iniciando..." },
  { atMs: 600, text: "Cargando..." },
  { atMs: 1500, text: "Conectando..." },
]

const LAUNCH_TIMEOUT_MS = 12_000

export function useGameStatus() {
  const phase = ref<GamePhase>("idle")
  const launchMessage = ref<string>(LAUNCH_MESSAGES[0].text)

  let messageTimers: Array<ReturnType<typeof setTimeout>> = []
  let launchTimeoutTimer: ReturnType<typeof setTimeout> | null = null
  let unsubscribe: Unsubscribe | null = null

  function clearTimers() {
    messageTimers.forEach((t) => clearTimeout(t))
    messageTimers = []
    if (launchTimeoutTimer) {
      clearTimeout(launchTimeoutTimer)
      launchTimeoutTimer = null
    }
  }

  function scheduleLaunchMessages() {
    clearTimers()
    launchMessage.value = LAUNCH_MESSAGES[0].text
    for (const step of LAUNCH_MESSAGES.slice(1)) {
      const timer = setTimeout(() => {
        if (phase.value === "launching") launchMessage.value = step.text
      }, step.atMs)
      messageTimers.push(timer)
    }
    launchTimeoutTimer = setTimeout(() => {
      if (phase.value === "launching") {
        phase.value = "idle"
        clearTimers()
      }
    }, LAUNCH_TIMEOUT_MS)
  }

  async function launch(host: string, port: number): Promise<GameLaunchResult | null> {
    if (phase.value !== "idle") return null
    phase.value = "launching"
    scheduleLaunchMessages()

    let result: GameLaunchResult
    try {
      result = await launcher.launchGame({ host, port })
    } catch (err) {
      result = {
        ok: false,
        error: typeof err === "string" ? err : "No se pudo iniciar el juego",
      }
    }
    if (!result.ok) {
      phase.value = "idle"
      clearTimers()
    }
    return result
  }

  onMounted(async () => {
    const initial = await launcher.getGameStatus()
    phase.value = initial.running ? "in-game" : "idle"

    unsubscribe = launcher.onGameStatusChanged((status) => {
      if (status.running) {
        phase.value = "in-game"
        clearTimers()
      } else {
        phase.value = "idle"
        clearTimers()
      }
    })
  })

  onUnmounted(() => {
    clearTimers()
    if (unsubscribe) unsubscribe()
  })

  return { phase, launchMessage, launch }
}
