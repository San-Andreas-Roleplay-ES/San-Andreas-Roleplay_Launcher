import { defineStore } from "pinia"
import { computed, shallowRef } from "vue"
import { launcher } from "@/api/launcher"
import type { RwGuardActionResult, RwGuardProgress, RwGuardStatus } from "@/types/rwguard"

/** The shield button that opens the RWGuard panel (focus returns to it on close). */
export const RWGUARD_BUTTON_ID = "rwguard-button"

function formatCount(n: number): string {
  return n.toLocaleString("es-ES")
}

/** Native RWGuard model validation state, live download/scan progress and
 *  persistent history overview. */
export const useRwGuardStore = defineStore("rwguard", () => {
  const status = shallowRef<RwGuardStatus | null>(null)
  const progress = shallowRef<RwGuardProgress | null>(null)
  const working = shallowRef(false)
  const notice = shallowRef<string | null>(null)
  const panelOpen = shallowRef(false)

  // Keep live validation counters and status in sync with backend events,
  // including concurrent validation happening during `install_cache`.
  launcher.onRwGuardProgress((next) => {
    progress.value = next
  })
  launcher.onRwGuardStatusChanged((next) => {
    status.value = next
    if (!next.scanning) {
      progress.value = null
    }
  })

  const scanning = computed(() => working.value || (progress.value?.scanning ?? false))
  const activeThreats = computed(() => status.value?.activeThreats ?? [])
  const blockedThreats = computed(() => status.value?.blockedThreats ?? [])
  const activeThreatCount = computed(() => activeThreats.value.length)
  const blockedThreatCount = computed(() => blockedThreats.value.length)
  const totalThreatCount = computed(() => activeThreatCount.value + blockedThreatCount.value)
  const attention = computed(() => totalThreatCount.value > 0)

  /** Compact live badge shown in the cache progress row while downloading or
   *  extracting models concurrently with RWGuard. */
  const liveBadge = computed<string | null>(() => {
    const p = progress.value
    if (!p) return null
    if (p.scanning && p.total > 0) {
      return `RWGuard · analizando ${formatCount(p.checked)}/${formatCount(p.total)}`
    }
    if (p.liveValidated > 0 || p.blocked > 0) {
      const base = `RWGuard · ${formatCount(p.liveValidated)} ${p.liveValidated === 1 ? "validado" : "validados"}`
      if (p.blocked > 0) {
        return `${base} · ${formatCount(p.blocked)} ${p.blocked === 1 ? "bloqueado" : "bloqueados"}`
      }
      return base
    }
    if (p.reused > 0) {
      return `RWGuard · ${formatCount(p.reused)} en historial`
    }
    return null
  })

  const liveBadgeWarning = computed(
    () => (progress.value?.blocked ?? 0) > 0 || (progress.value?.threats ?? 0) > 0,
  )

  /** One-line summary displayed under the "Caché de modelos" row. */
  const summaryLine = computed<string | undefined>(() => {
    const p = progress.value
    if (p?.scanning && p.total > 0) {
      return `RWGuard: analizando ${formatCount(p.checked)} de ${formatCount(p.total)} modelos nuevos…`
    }
    const s = status.value
    if (!s) return undefined

    if (s.activeThreats.length > 0) {
      const n = s.activeThreats.length
      return n === 1
        ? "RWGuard detectó 1 modelo inseguro en el caché. Revísalo en RWGuard."
        : `RWGuard detectó ${formatCount(n)} modelos inseguros en el caché. Revísalos en RWGuard.`
    }
    if (s.blockedThreats.length > 0) {
      const n = s.blockedThreats.length
      return n === 1
        ? `RWGuard: ${formatCount(s.validatedModels)} validados · 1 bloqueado al descargar.`
        : `RWGuard: ${formatCount(s.validatedModels)} validados · ${formatCount(n)} bloqueados al descargar.`
    }
    if (s.validatedModels === 0) {
      return "RWGuard activo · sin modelos en caché aún."
    }
    const newly = s.newlyScanned > 0 ? ` (${formatCount(s.newlyScanned)} nuevos)` : ""
    return `RWGuard: ${formatCount(s.validatedModels)} modelos validados${newly} · sin amenazas.`
  })

  async function refreshStatus(): Promise<void> {
    try {
      status.value = await launcher.rwguardStatus()
    } catch (err) {
      console.error("[rwguard] status failed", err)
    }
  }

  async function scan(options: { force?: boolean } = {}): Promise<RwGuardStatus | null> {
    if (working.value) return status.value
    working.value = true
    notice.value = null
    try {
      const result = await launcher.rwguardScan(options)
      status.value = result
      return result
    } catch (err) {
      notice.value = String(err)
      return null
    } finally {
      working.value = false
      progress.value = null
    }
  }

  async function heal(fileName?: string): Promise<RwGuardActionResult | null> {
    if (working.value) return null
    working.value = true
    notice.value = null
    try {
      const result = await launcher.rwguardHeal(fileName)
      status.value = result.status
      if (!result.ok && result.error) notice.value = result.error
      return result
    } catch (err) {
      notice.value = String(err)
      return null
    } finally {
      working.value = false
    }
  }

  async function deleteThreat(fileName?: string): Promise<RwGuardActionResult | null> {
    if (working.value) return null
    working.value = true
    notice.value = null
    try {
      const result = await launcher.rwguardDelete(fileName)
      status.value = result.status
      if (!result.ok && result.error) notice.value = result.error
      return result
    } catch (err) {
      notice.value = String(err)
      return null
    } finally {
      working.value = false
    }
  }

  function openPanel(): void {
    panelOpen.value = true
    notice.value = null
    if (!working.value) void refreshStatus()
  }

  function closePanel(): void {
    panelOpen.value = false
  }

  return {
    status,
    progress,
    working,
    scanning,
    notice,
    panelOpen,
    activeThreats,
    blockedThreats,
    activeThreatCount,
    blockedThreatCount,
    totalThreatCount,
    attention,
    liveBadge,
    liveBadgeWarning,
    summaryLine,
    refreshStatus,
    scan,
    heal,
    deleteThreat,
    openPanel,
    closePanel,
  }
})
