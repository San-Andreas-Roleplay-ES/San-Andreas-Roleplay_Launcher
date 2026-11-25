import { launcher } from "@/api/launcher"
import type { GlobalMetrics, RawServerEvent } from "@/types/public"

export type { GlobalMetrics }

export interface ServerEvent {
  id: string | number
  title: string
  description?: string | null
  image?: string | null
  url?: string | null
  startsAt?: string | null
  endsAt?: string | null
}

function normalizeEvent(raw: RawServerEvent, index: number): ServerEvent {
  return {
    id: raw.id ?? `event-${index}`,
    title: raw.title ?? "Evento",
    description: raw.description ?? null,
    image: raw.image_url ?? null,
    url: null,
    startsAt: raw.event_date ?? null,
    endsAt: null,
  }
}

/** Live from the API, through the backend (which adds the launcher key). */
export async function fetchGlobalMetrics(): Promise<GlobalMetrics> {
  return launcher.fetchGlobalMetrics()
}

export async function fetchEvents(): Promise<ServerEvent[]> {
  const raw = await launcher.fetchEvents()
  return raw.map(normalizeEvent)
}
