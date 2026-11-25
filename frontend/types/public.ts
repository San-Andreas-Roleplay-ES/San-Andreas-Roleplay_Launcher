/** Counters from the API's `global-metrics` endpoint (`onlineAccountCounter`, ...). */
export interface GlobalMetrics {
  [key: string]: unknown
}

/** An event exactly as the API's `events` endpoint sends it. */
export interface RawServerEvent {
  id?: string | number | null
  title?: string | null
  description?: string | null
  image_url?: string | null
  event_date?: string | null
}
