import { ref, onMounted, onUnmounted } from "vue"
import { fetchWoozieAlerts, type WoozieAlert } from "@/api/public"

const POLL_INTERVAL = 20_000 // cada 20 segundos
const SEEN_KEY = "woozie_seen_ids"

// los iconos de woozie por tipo de alerta
const typeLabel: Record<WoozieAlert["type"], string> = {
  bonus: "Triple XP, monedas... Woozie tiene algo para ti.",
  event: "Woozie te avisa de un evento.",
  maintenance: "¡Hey! Hay un mantenimiento ahora mismo. ¡Será mejor que te guardes en tu casa!.",
  announcement: "¡Hey! ",
}

export function useWoozieAlerts() {
  const current = ref<WoozieAlert | null>(null)
  const history = ref<WoozieAlert[]>([])
  let timer: ReturnType<typeof setInterval> | null = null

  // ids que ya mostramos, para no repetir notificaciones
  function getSeenIds(): Set<number> {
    try {
      const raw = localStorage.getItem(SEEN_KEY)
      if (!raw) return new Set()
      return new Set(JSON.parse(raw))
    } catch {
      return new Set()
    }
  }

  function markSeen(id: number) {
    const seen = getSeenIds()
    seen.add(id)
    // guardamos solo los ultimos 100 para que no crezca infinito
    const arr = [...seen].slice(-100)
    localStorage.setItem(SEEN_KEY, JSON.stringify(arr))
  }

  function showNativeNotification(alert: WoozieAlert) {
    try {
      if (!("Notification" in window)) return
      if (Notification.permission === "denied") return

      if (Notification.permission === "default") {
        Notification.requestPermission()
        return
      }

      new Notification("Mensaje de Woozie", {
        body: alert.message,
        silent: false,
      })
    } catch {
      // si falla la notificacion nativa no pasa nada
    }
  }

  async function poll() {
    try {
      const alerts = await fetchWoozieAlerts()
      if (alerts.length === 0) return

      const seen = getSeenIds()

      for (const alert of alerts) {
        if (seen.has(alert.id)) continue

        // nueva alerta
        current.value = alert
        history.value.unshift(alert)
        markSeen(alert.id)
        showNativeNotification(alert)
      }

      // mantener solo las ultimas 20 en el historial
      if (history.value.length > 20) {
        history.value = history.value.slice(0, 20)
      }
    } catch {
      // si falla el fetch no rompemos nada
    }
  }

  function dismiss() {
    current.value = null
  }

  onMounted(() => {
    poll()
    timer = setInterval(poll, POLL_INTERVAL)
  })

  onUnmounted(() => {
    if (timer) clearInterval(timer)
  })

  return {
    current,
    history,
    dismiss,
    poll,
  }
}