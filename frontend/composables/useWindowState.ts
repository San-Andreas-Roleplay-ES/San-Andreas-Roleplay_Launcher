import { onMounted, onUnmounted, ref } from "vue"
import { launcher } from "@/api/launcher"
import type { Unsubscribe } from "@/api/launcher"

/** Mirrors the native window's maximise/fullscreen state.
 *  Used by the title bar to swap maximise/restore icons and to hide itself
 *  when the user enters fullscreen. */
export function useWindowState() {
  const isMaximized = ref(false)
  const isFullscreen = ref(false)

  let detach: Unsubscribe | null = null
  let disposed = false

  onMounted(async () => {
    const initial = await launcher.getWindowState()
    // The title bar unmounts on fullscreen; don't subscribe for a dead instance.
    if (disposed) return
    isMaximized.value = initial.isMaximized
    isFullscreen.value = initial.isFullscreen
    detach = launcher.onWindowStateChange((state) => {
      isMaximized.value = state.isMaximized
      isFullscreen.value = state.isFullscreen
    })
  })

  onUnmounted(() => {
    disposed = true
    detach?.()
  })

  return {
    isMaximized,
    isFullscreen,
    toggleMaximize: () => void launcher.toggleMaximize(),
    toggleFullscreen: () => void launcher.toggleFullscreen(),
  }
}
