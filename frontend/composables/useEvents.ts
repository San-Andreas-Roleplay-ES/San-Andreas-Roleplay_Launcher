import { ref } from "vue"
import { fetchEvents, type ServerEvent } from "@/api/public"

export function useEvents() {
  const events = ref<ServerEvent[]>([])
  const loading = ref(false)
  const error = ref<string | null>(null)

  async function load() {
    loading.value = true
    error.value = null
    try {
      events.value = await fetchEvents()
    } catch {
      events.value = []
      error.value = "No se pudieron cargar los eventos."
    } finally {
      loading.value = false
    }
  }

  return { events, loading, error, load }
}
