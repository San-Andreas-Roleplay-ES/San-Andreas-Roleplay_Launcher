import { nextTick, onBeforeUnmount, onMounted, watch } from "vue"
import type { Ref } from "vue"

const FOCUSABLE = [
  "button:not([disabled])",
  "[href]",
  "input:not([disabled])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  '[tabindex]:not([tabindex="-1"])',
].join(", ")

/** Modal keyboard behaviour while `active()`: focus moves into `container`
 *  when it opens (to a `[data-autofocus]` element if there is one), Tab and
 *  Shift+Tab cycle inside it, Escape calls `onEscape`, and focus goes back to
 *  where it was when it closes, or to `fallback()` when that element was
 *  re-rendered meanwhile. */
export function useFocusTrap(
  container: Readonly<Ref<HTMLElement | null>>,
  active: () => boolean,
  onEscape: () => void,
  fallback?: () => HTMLElement | null,
): void {
  let previous: HTMLElement | null = null

  function focusables(): HTMLElement[] {
    const root = container.value
    if (!root) return []
    return Array.from(root.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
      (el) => el.offsetParent !== null || el === document.activeElement,
    )
  }

  function onKeydown(event: KeyboardEvent): void {
    if (!active()) return
    if (event.key === "Escape") {
      event.preventDefault()
      onEscape()
      return
    }
    if (event.key !== "Tab") return
    const items = focusables()
    if (items.length === 0) {
      event.preventDefault()
      return
    }
    const first = items[0]
    const last = items[items.length - 1]
    const current = document.activeElement
    const inside = container.value?.contains(current) ?? false
    if (event.shiftKey && (current === first || !inside)) {
      event.preventDefault()
      last.focus()
    } else if (!event.shiftKey && (current === last || !inside)) {
      event.preventDefault()
      first.focus()
    }
  }

  watch(active, async (open) => {
    if (open) {
      previous = document.activeElement instanceof HTMLElement ? document.activeElement : null
      await nextTick()
      const target =
        container.value?.querySelector<HTMLElement>("[data-autofocus]") ??
        focusables()[0] ??
        container.value
      target?.focus()
      return
    }
    const kept = previous?.isConnected ? previous : null
    previous = null
    await nextTick()
    if (kept) {
      kept.focus()
      return
    }
    // The opener may still be re-rendering (a run just ended): give it a
    // few frames to come back.
    for (let frame = 0; frame < 30; frame++) {
      const target = fallback?.()
      if (target) {
        target.focus()
        return
      }
      await new Promise((resolve) => requestAnimationFrame(resolve))
    }
  })

  onMounted(() => window.addEventListener("keydown", onKeydown))
  onBeforeUnmount(() => window.removeEventListener("keydown", onKeydown))
}
