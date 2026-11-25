import { onMounted, onUnmounted, ref } from "vue"
import { launcher } from "@/api/launcher"
import type { Unsubscribe } from "@/api/launcher"

export type UpdaterPhase = "idle" | "downloading" | "ready"

export function useUpdater() {
  const phase = ref<UpdaterPhase>("idle")
  const version = ref<string | null>(null)
  const percent = ref<number>(0)

  const unsubs: Unsubscribe[] = []

  onMounted(() => {
    unsubs.push(
      launcher.onUpdaterAvailable((info) => {
        version.value = info.version
        percent.value = 0
        // Skip "downloading" if a progress event hasn't kicked in yet — small
        // updates over a fast connection can finish before any progress fires.
        if (phase.value !== "ready") phase.value = "downloading"
      }),
    )
    unsubs.push(
      launcher.onUpdaterProgress((p) => {
        percent.value = Math.min(100, Math.max(0, Math.round(p.percent)))
        if (phase.value !== "ready") phase.value = "downloading"
      }),
    )
    unsubs.push(
      launcher.onUpdaterDownloaded((info) => {
        version.value = info.version
        percent.value = 100
        phase.value = "ready"
      }),
    )
    unsubs.push(
      // Errors are never shown — a transient network hiccup should not pop a
      // scary banner; full traces live in the backend log. But a failed
      // download (e.g. bad signature) or install must not leave the banner
      // stuck on "Descargando…" or on a button that no longer does anything:
      // hide it, the next launch checks again.
      launcher.onUpdaterError(() => {
        phase.value = "idle"
        version.value = null
        percent.value = 0
      }),
    )
  })

  onUnmounted(() => {
    unsubs.forEach((u) => u())
    unsubs.length = 0
  })

  async function applyUpdate() {
    if (phase.value !== "ready") return
    await launcher.quitAndInstall()
  }

  return { phase, version, percent, applyUpdate }
}
