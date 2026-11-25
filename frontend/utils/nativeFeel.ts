// WebView2 is a full browser: without these guards the launcher would answer to
// right-click menus, reloads, zoom, print, find-in-page, view-source and caret
// browsing — none of which make sense in a desktop app.

/** Blocked regardless of modifiers. F3 is find-next, F7 toggles caret browsing. */
const BLOCKED_KEYS = new Set(["F3", "F7"])

/** Blocked when Ctrl is held: print, find, find-next, view-source and zoom. */
const BLOCKED_CTRL_KEYS = new Set(["p", "f", "g", "u", "+", "=", "-", "_", "0"])

/** Numpad zoom keys report `key` as "+"/"-" but are matched by `code` to be safe. */
const BLOCKED_CTRL_CODES = new Set(["NumpadAdd", "NumpadSubtract", "Numpad0"])

/** Reload stays available in dev, where a manual refresh is handy. */
function isReload(event: KeyboardEvent): boolean {
  return event.key === "F5" || (event.ctrlKey && event.key.toLowerCase() === "r")
}

function isBlocked(event: KeyboardEvent): boolean {
  if (BLOCKED_KEYS.has(event.key)) return true
  if (import.meta.env.PROD && isReload(event)) return true
  if (!event.ctrlKey) return false
  return BLOCKED_CTRL_KEYS.has(event.key.toLowerCase()) || BLOCKED_CTRL_CODES.has(event.code)
}

/** Installs the global guards. Call once, before mounting the app. */
export function applyNativeFeel(): void {
  window.addEventListener("contextmenu", (event) => event.preventDefault(), { capture: true })

  window.addEventListener(
    "keydown",
    (event) => {
      if (isBlocked(event)) event.preventDefault()
    },
    { capture: true },
  )

  // Ctrl+wheel (and touchpad pinch, which arrives as Ctrl+wheel) zooms the page.
  window.addEventListener(
    "wheel",
    (event) => {
      if (event.ctrlKey) event.preventDefault()
    },
    { capture: true, passive: false },
  )
}
