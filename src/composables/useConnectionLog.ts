import { ref, readonly } from "vue"

export interface LogEntry {
  id: number
  time: string
  text: string
  level: "info" | "ok" | "error"
}

let nextId = 0

const entries = ref<LogEntry[]>([])
const MAX_ENTRIES = 50

function now(): string {
  const d = new Date()
  const h = String(d.getHours()).padStart(2, "0")
  const m = String(d.getMinutes()).padStart(2, "0")
  const s = String(d.getSeconds()).padStart(2, "0")
  return `${h}:${m}:${s}`
}

export function useConnectionLog() {
  function push(text: string, level: LogEntry["level"] = "info") {
    entries.value.push({ id: nextId++, time: now(), text, level })
    if (entries.value.length > MAX_ENTRIES) {
      entries.value = entries.value.slice(-MAX_ENTRIES)
    }
  }

  function clear() {
    entries.value = []
  }

  return {
    entries: readonly(entries),
    push,
    clear,
  }
}