import { defineStore } from "pinia"
import { computed, shallowRef } from "vue"
import { launcher } from "@/api/launcher"
import type { ModsHealth, ModsProgress, ModsSyncResult, ModStatus } from "@/types/mods"

/** The cube button that opens the panel (focus goes back to it on close). */
export const MODS_BUTTON_ID = "mods-button"

/** Mods from the signed catalog: their state in the game folder, the runs
 *  that install or remove them, and the Mods panel. */
export const useModsStore = defineStore("mods", () => {
  const health = shallowRef<ModsHealth | null>(null)
  const checking = shallowRef(false)
  /** A run (the automatic flow or a panel action) is writing the folder. */
  const working = shallowRef(false)
  /** Mods the current run is about. */
  const runIds = shallowRef<string[]>([])
  /** Latest progress of the run, and per mod. */
  const progress = shallowRef<ModsProgress | null>(null)
  const progressById = shallowRef<Record<string, ModsProgress>>({})
  const cancelling = shallowRef(false)
  /** Why the last panel action was refused or failed as a whole. */
  const notice = shallowRef<string | null>(null)
  const lastRun = shallowRef<ModsSyncResult | null>(null)
  const panelOpen = shallowRef(false)

  const mods = computed<ModStatus[]>(() => health.value?.mods ?? [])
  const required = computed(() => mods.value.filter((m) => m.required))
  const optional = computed(() => mods.value.filter((m) => !m.required))
  const blocking = computed(() => health.value?.blocking ?? false)
  const failed = computed(() => mods.value.filter((m) => m.error))

  /** Something the user may want to look at: the cube button's dot. */
  const attention = computed(
    () =>
      working.value ||
      Boolean(health.value?.catalog.detail) ||
      mods.value.some(
        (m) =>
          m.error ||
          m.conflicts.length > 0 ||
          (m.action !== "none" && m.state !== "notInstalled") ||
          (m.enabled && m.state === "notInstalled"),
      ),
  )

  /** "Omitir" only while the run touches optional mods alone: skipping a
   *  required one would leave Play waiting for it. */
  const cancellable = computed(
    () =>
      working.value &&
      runIds.value.length > 0 &&
      runIds.value.every((id) => mods.value.find((m) => m.id === id)?.required === false),
  )

  async function check(options: { force?: boolean } = {}): Promise<void> {
    checking.value = true
    try {
      health.value = await launcher.checkMods(options)
    } catch (err) {
      console.error("[mods] check failed", err)
    } finally {
      checking.value = false
    }
  }

  /** The automatic flow may start: GTA and SA:MP ready, the game closed. */
  function shouldSync(ready: boolean): boolean {
    return ready && !working.value && (health.value?.pending ?? false)
  }

  async function perform(
    ids: string[],
    action: () => Promise<ModsSyncResult>,
  ): Promise<ModsSyncResult | null> {
    if (working.value) return null
    working.value = true
    runIds.value = ids
    notice.value = null
    progress.value = null
    progressById.value = {}
    const detach = launcher.onModsProgress((next) => {
      progress.value = next
      if (next.id) progressById.value = { ...progressById.value, [next.id]: next }
    })
    try {
      const result = await action()
      lastRun.value = result
      if (result.error) notice.value = result.error
      return result
    } catch (err) {
      notice.value = String(err)
      return null
    } finally {
      detach()
      await check()
      working.value = false
      cancelling.value = false
      runIds.value = []
      progress.value = null
      progressById.value = {}
    }
  }

  /** What the automatic flow will touch. */
  function pendingIds(): string[] {
    return mods.value.filter((m) => m.action !== "none" && !m.error).map((m) => m.id)
  }

  function sync(options: { retry?: boolean } = {}) {
    const ids = options.retry
      ? mods.value.filter((m) => m.action !== "none").map((m) => m.id)
      : pendingIds()
    return perform(ids, () => launcher.syncMods(options))
  }

  function setEnabled(id: string, enabled: boolean) {
    return perform([id], () => launcher.setModEnabled(id, enabled))
  }

  function setVariant(id: string, variant: string) {
    return perform([id], () => launcher.setModVariant(id, variant))
  }

  function installRecommended(id: string) {
    return perform([id], () => launcher.installRecommendedMod(id))
  }

  function disableConflicts(id: string) {
    return perform([id], () => launcher.disableModConflicts(id))
  }

  async function cancel(): Promise<void> {
    if (!cancellable.value || cancelling.value) return
    cancelling.value = true
    try {
      if (!(await launcher.cancelMods())) cancelling.value = false
    } catch {
      cancelling.value = false
    }
  }

  function openPanel(): void {
    panelOpen.value = true
    notice.value = null
    // Fresh statuses for the panel; the catalog stays cached for a minute.
    if (!working.value) void check()
  }

  function closePanel(): void {
    panelOpen.value = false
  }

  return {
    health,
    checking,
    working,
    runIds,
    progress,
    progressById,
    cancelling,
    notice,
    lastRun,
    panelOpen,
    mods,
    required,
    optional,
    blocking,
    failed,
    attention,
    cancellable,
    check,
    shouldSync,
    sync,
    setEnabled,
    setVariant,
    installRecommended,
    disableConflicts,
    cancel,
    openPanel,
    closePanel,
  }
})
