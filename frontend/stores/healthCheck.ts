import { defineStore } from "pinia"
import { computed, ref, shallowRef, watch } from "vue"
import { launcher } from "@/api/launcher"
import { useServerPing } from "@/composables/useServerPing"
import { useDialogStore } from "@/stores/dialog"
import { useModsStore } from "@/stores/mods"
import type { CacheHealth, CacheInstallProgress, CacheInstallResult } from "@/types/cache"
import type { GtaCandidate, GtaInstallProgress } from "@/types/gta"
import type { InstallProgress, SampVerificationResult } from "@/types/samp"

export type GtaActionType = "install" | null

export type SampActionType = "install" | "repair" | null

export type CacheActionType = "install" | "update" | null

export type CheckState = "checking" | "ok" | "warning" | "error"

export interface HealthEntry {
  id: "gta" | "samp" | "mods" | "cache" | "server"
  label: string
  state: CheckState
  detail?: string
  meta?: string
}

const INITIAL: HealthEntry[] = [
  { id: "gta", label: "GTA San Andreas", state: "checking" },
  { id: "samp", label: "SA:MP", state: "checking" },
  { id: "mods", label: "Mods", state: "checking" },
  { id: "cache", label: "Caché de modelos", state: "checking" },
  { id: "server", label: "Ping con el servidor", state: "checking" },
]

function describeSamp(result: SampVerificationResult): string | undefined {
  // The backend's errors are user-facing ("Elige primero la carpeta...",
  // "Falta samp.exe y no se pudo contactar...", "No se pudo leer...").
  if (result.error) return result.error
  const failed = result.files.filter((f) => !f.ok)
  if (failed.length === 0) return undefined

  const missing = failed.filter((f) => f.reason === "missing")
  const corrupted = failed.filter(
    (f) => f.reason === "size-mismatch" || f.reason === "hash-mismatch",
  )

  if (missing.length > 0 && corrupted.length === 0) {
    return missing.length === failed.length && failed.length === result.files.length
      ? "No está instalado en esta carpeta."
      : `Falta ${joinFileList(missing.map((f) => f.path))}.`
  }

  if (corrupted.length > 0 && missing.length === 0) {
    return `${joinFileList(corrupted.map((f) => f.path))} ${corrupted.length === 1 ? "está dañado" : "están dañados"}.`
  }

  return "Tu instalación de SA:MP está incompleta o dañada."
}

function joinFileList(files: string[]): string {
  if (files.length === 0) return ""
  if (files.length === 1) return files[0]
  if (files.length === 2) return `${files[0]} y ${files[1]}`
  return `${files.slice(0, -1).join(", ")} y ${files[files.length - 1]}`
}

function deriveSampAction(result: SampVerificationResult | null): SampActionType {
  if (!result || result.ok || result.error) return null
  const failed = result.files.filter((f) => !f.ok)
  if (failed.length === 0) return null
  // "Install" only applies when nothing exists yet (virgin state — every file
  // is missing). Any partial failure — a single missing file, a corrupted one,
  // or a mix — is a repair on top of an existing installation.
  const allMissing =
    failed.length === result.files.length && failed.every((f) => f.reason === "missing")
  return allMissing ? "install" : "repair"
}

function samePath(a: string, b: string): boolean {
  const norm = (p: string) => p.replace(/[\\/]+$/, "").toLowerCase()
  return norm(a) === norm(b)
}

export const useHealthCheckStore = defineStore("healthCheck", () => {
  const entries = ref<HealthEntry[]>(INITIAL.map((e) => ({ ...e })))
  const running = ref(false)
  const installing = ref(false)
  const installProgress = ref<InstallProgress | null>(null)
  const lastSampVerification = ref<SampVerificationResult | null>(null)
  const requiresElevation = ref<boolean>(false)
  const gtaMissingAll = ref<boolean>(false)
  const gtaInstalling = ref<boolean>(false)
  const gtaInstallProgress = ref<GtaInstallProgress | null>(null)
  // The GTA folder in use (a valid install), from the last check.
  const gtaPath = shallowRef<string | null>(null)
  // Installs found on this PC: what the chooser lists.
  const gtaChoices = shallowRef<GtaCandidate[]>([])
  // Adopted automatically this session: the GTA row says so.
  const gtaDetected = shallowRef<GtaCandidate | null>(null)
  const gtaPickerOpen = shallowRef(false)
  // The chooser opens by itself once per session at most.
  let gtaPickerShown = false
  const cacheInstalling = ref<boolean>(false)
  const cacheInstallProgress = ref<CacheInstallProgress | null>(null)
  const cacheActionType = ref<CacheActionType>(null)
  // "Omitir" was clicked: the backend is stopping the sync.
  const cacheCancelling = shallowRef(false)
  // How the last sync attempt this session ended. After a failed one the sync
  // is not retried by itself: only by "Reintentar", a manual refresh or the
  // next launch. After a skipped one ("Omitir"), only by "Continuar" or the
  // next launch.
  const cacheSync = shallowRef<CacheInstallResult | null>(null)
  // Mirror of the OS-level "gta_sa.exe is alive" status. While the game is
  // running we suppress every automatic health re-check and every cache
  // re-download, so we don't fight the game for disk and bandwidth.
  const gameRunning = ref<boolean>(false)
  const serverPing = useServerPing()
  const dialog = useDialogStore()
  const mods = useModsStore()

  // Bootstrap the game-status mirror and keep it in sync with the OS. `run`
  // waits for it, so the first check never mistakes a running game for none.
  const gameStatusReady = launcher
    .getGameStatus()
    .then((status) => {
      gameRunning.value = status.running
    })
    .catch(() => {})
  launcher.onGameStatusChanged((status) => {
    const wasRunning = gameRunning.value
    gameRunning.value = status.running
    // When the user closes the game, the cache may have changed upstream
    // and the launcher couldn't know — re-run the checks so the next thing
    // they see reflects reality (and any auto-update can finally fire).
    if (wasRunning && !status.running) void run()
  })

  function stateOf(id: HealthEntry["id"]): CheckState | undefined {
    return entries.value.find((e) => e.id === id)?.state
  }

  // SA:MP that could not be verified (no manifest) but has samp.exe is a
  // warning that still lets the user play.
  const gtaReady = computed(() => stateOf("gta") === "ok")
  const sampReady = computed(() => stateOf("samp") === "ok" || stateOf("samp") === "warning")

  // Playing needs GTA and SA:MP. The cache only holds it back while a sync
  // writes the folder (or the check that may start one is still running):
  // whatever it could not download, the game fetches when it connects. Mods
  // hold it back while they are written, and when a required one could not
  // be installed (the row says why, with "Reintentar").
  const allOk = computed(
    () =>
      !cacheInstalling.value &&
      !mods.working &&
      !mods.blocking &&
      gtaReady.value &&
      sampReady.value &&
      stateOf("mods") !== "checking" &&
      stateOf("cache") !== "checking",
  )

  /** Several installs found, none in use yet: the GTA row asks to choose. */
  const gtaChoicePending = computed(() => gtaPath.value === null && gtaChoices.value.length > 1)

  /** "Reintentar" on the cache row: files are missing and nothing prevents a
   *  sync right now. */
  const cacheRetry = computed(
    () =>
      cacheActionType.value !== null &&
      gtaReady.value &&
      sampReady.value &&
      !gameRunning.value &&
      !cacheInstalling.value,
  )

  const sampActionType = computed<SampActionType>(() =>
    deriveSampAction(lastSampVerification.value),
  )

  function update(id: HealthEntry["id"], patch: Partial<HealthEntry>) {
    const target = entries.value.find((e) => e.id === id)
    if (!target) return
    Object.assign(target, patch)
  }

  const gtaActionType = computed<GtaActionType>(() => (gtaMissingAll.value ? "install" : null))

  function reset() {
    entries.value = INITIAL.map((e) => ({ ...e }))
    gtaMissingAll.value = false
    cacheActionType.value = null
  }

  /** The Mods row, from the mods store. */
  function describeMods(): Partial<HealthEntry> {
    const health = mods.health
    const clear = { detail: undefined, meta: undefined }
    if (!health) {
      return mods.checking
        ? { ...clear, state: "checking" }
        : { ...clear, state: "warning", detail: "No se pudieron comprobar los mods." }
    }
    if (health.catalog.state === "missing") {
      return { ...clear, state: "warning", detail: health.catalog.detail }
    }
    if (health.mods.length === 0) {
      return { ...clear, state: "ok", meta: "No hay mods disponibles por ahora." }
    }
    const failed = health.mods.find((m) => m.blocking) ?? health.mods.find((m) => m.error)
    if (failed) {
      return {
        ...clear,
        state: failed.blocking ? "error" : "warning",
        detail: `${failed.name}: ${failed.error}`,
      }
    }
    if (health.pending && !(gtaReady.value && sampReady.value)) {
      return { ...clear, state: "warning", detail: "Esperando a que GTA y SA:MP estén listos." }
    }
    if (health.pending && gameRunning.value) {
      return { ...clear, state: "warning", detail: "Se instalarán al cerrar el juego." }
    }
    const conflict = health.mods.find((m) => m.conflicts.length > 0)
    if (conflict) {
      return {
        ...clear,
        state: "warning",
        detail: `${conflict.name} puede chocar con otra instalación tuya. Revísalo en Mods.`,
      }
    }
    if (health.catalog.detail) {
      return { ...clear, state: "warning", detail: health.catalog.detail }
    }
    const active = health.mods
      .filter((m) => m.state === "installed" || m.state === "outdated" || m.state === "foreign")
      .map((m) => m.name)
    return {
      ...clear,
      state: "ok",
      meta: active.length > 0 ? `Activos: ${joinFileList(active)}.` : "Ninguno activo.",
    }
  }

  watch(
    () => [mods.health, mods.checking, gtaReady.value, sampReady.value, gameRunning.value],
    () => {
      // A re-check while a row already shows results keeps them until the
      // new ones land.
      if (mods.checking && mods.health && stateOf("mods") !== "checking") return
      update("mods", describeMods())
    },
  )

  /** The Mods row once the catalog check (started earlier) lands. */
  async function checkMods(pending: Promise<void>): Promise<void> {
    await pending
    update("mods", describeMods())
  }

  /** The automatic flows, mods first (required ones, defaults, updates),
   *  then the cache, once GTA and SA:MP are ready and the game is closed.
   *  Started without awaiting, so the rest of the checks (and the page)
   *  don't wait for the downloads. */
  function startInstalls(): void {
    if (mods.shouldSync(gtaReady.value && sampReady.value && !gameRunning.value)) {
      void mods.sync().then(() => {
        if (shouldSync()) void installCache()
      })
      return
    }
    if (shouldSync()) void installCache()
  }

  /** "Reintentar" on the Mods row. */
  async function retryMods(): Promise<void> {
    if (gameRunning.value) return
    await mods.sync({ retry: true })
  }

  function syncServerEntry() {
    const result = serverPing.result.value
    if (!result) {
      update("server", { state: "checking", detail: undefined, meta: undefined })
      return
    }
    if (!result.alive) {
      update("server", {
        state: "error",
        detail: "El servidor no responde.",
        meta: undefined,
      })
      return
    }
    if (!result.info || result.ms === null) {
      update("server", {
        state: "error",
        detail: "Respuesta incompleta del servidor.",
        meta: undefined,
      })
      return
    }
    // Format: "Miami · 42 ms · 18/200 jugadores". Location is baked at
    // build time from VITE_GAME_SERVER_LOCATION — the server doesn't move
    // often enough to warrant geo lookups at runtime.
    const location = import.meta.env.VITE_GAME_SERVER_LOCATION ?? ""
    const stats = `${result.ms} ms · ${result.info.players}/${result.info.maxPlayers} jugadores`
    update("server", {
      state: "ok",
      detail: undefined,
      meta: location ? `${location} · ${stats}` : stats,
    })
  }

  watch(() => serverPing.result.value, syncServerEntry, { immediate: true })

  /** Without a valid folder the backend looks for installs and adopts the
   *  one SA-MP uses (or the only one). Several: the chooser opens, once. */
  async function detectGta(): Promise<void> {
    try {
      const detection = await launcher.detectGameDir()
      gtaChoices.value = detection.candidates
      if (detection.adopted) {
        gtaDetected.value = detection.adopted
      } else if (detection.candidates.length > 1 && !gtaPickerShown) {
        gtaPickerShown = true
        gtaPickerOpen.value = true
      }
    } catch {
      // Detection is a convenience: the manual flow still works.
    }
  }

  async function checkGta(): Promise<void> {
    const health = await launcher.healthCheck()
    gtaPath.value = health.gta.path ?? null
    gtaMissingAll.value = health.gta.missingAll
    if (health.gta.ok) {
      const path = health.gta.path
      const detected = gtaDetected.value
      update("gta", {
        state: "ok",
        detail: undefined,
        meta:
          path && detected && samePath(path, detected.path)
            ? `Detectada automáticamente en ${path}`
            : undefined,
      })
      return
    }
    if (gtaChoicePending.value) {
      update("gta", {
        state: "warning",
        detail: `Encontramos ${gtaChoices.value.length} instalaciones de GTA San Andreas. Elige cuál usar.`,
      })
      return
    }
    update("gta", { state: "error", detail: health.gta.detail })
  }

  async function checkSamp(): Promise<void> {
    // The backend checks against the live manifest, or the last saved one
    // when the API can't be reached.
    const result = await launcher.verifySamp()
    lastSampVerification.value = result
    if (result.ok) {
      update("samp", { state: "ok", detail: undefined })
      return
    }
    if (result.unverified) {
      // No manifest to check against, but samp.exe is there: playable.
      update("samp", { state: "warning", detail: result.error })
      return
    }
    update("samp", {
      state: "error",
      detail: describeSamp(result) ?? "Tu instalación de SA:MP necesita ser reparada.",
    })
  }

  /** `pending`: a check already in flight (`run` starts it early so the API
   *  round-trip overlaps the GTA / SA:MP checks). */
  async function checkCache(pending: Promise<CacheHealth> = launcher.checkCache()): Promise<void> {
    const cache = await pending
    cacheActionType.value = cache.ok
      ? null
      : cache.needsInstall
        ? "install"
        : cache.needsUpdate
          ? "update"
          : null

    if (cache.ok) {
      update("cache", { state: "ok", detail: undefined })
      return
    }
    // Never an error: missing models are downloaded by the game when it
    // connects. "Could not verify" (API unreachable, nothing published yet)
    // syncs nothing; missing files sync once GTA and SA:MP are ready.
    const waiting = cacheActionType.value !== null && !(gtaReady.value && sampReady.value)
    update("cache", {
      state: "warning",
      detail: waiting ? "Esperando a que GTA y SA:MP estén listos." : cache.detail,
    })
  }

  /** The automatic sync: files missing, GTA and SA:MP ready, the game closed,
   *  and no failed attempt yet this session. */
  function shouldSync(): boolean {
    return cacheRetry.value && cacheSync.value?.ok !== false
  }

  async function checkElevation(): Promise<void> {
    try {
      requiresElevation.value = await launcher.sampInstallRequiresElevation()
    } catch {
      requiresElevation.value = false
    }
  }

  /** `force`: the user asked for it (refresh button). Runs even while the game
   *  is open, and lets a failed cache sync try again. */
  async function run(options: { force?: boolean } = {}): Promise<void> {
    await gameStatusReady
    // While the game is running every check (sha256-of-samp files, recursive
    // dir reads, manifest fetch) competes with the game for I/O and CPU.
    // Skip silently — the next on-game-close event re-runs us. The user can
    // still force a re-check via the refresh button.
    if (gameRunning.value && !options.force) return
    // A refresh retries a failed sync, but not one the user skipped.
    if (options.force && !cacheSync.value?.cancelled) cacheSync.value = null

    running.value = true
    reset()
    // The cache check (an API call plus a folder scan) starts right away;
    // detection runs first among the local checks, as it may set the folder
    // they (and the mods check) look at.
    const cache = launcher.checkCache()
    await detectGta()
    const modsCheck = mods.check({ force: options.force })
    await Promise.all([checkGta(), checkSamp(), checkElevation()])
    // GTA → SA:MP → mods → cache: required mods install before the cache
    // sync starts, and Play waits for them.
    await checkMods(modsCheck)
    await checkCache(cache)
    // Straight away, before any other await: there is no moment where Play
    // looks enabled and then an install starts under it.
    startInstalls()
    await serverPing.ping()
    syncServerEntry()
    running.value = false
  }

  function describeInstallProgress(progress: InstallProgress): string {
    switch (progress.phase) {
      case "preflight":
        return progress.message ?? "Preparando instalación..."
      case "download":
        return `Descargando... ${Math.floor(progress.percent)}%`
      case "install":
        return progress.message ?? "Instalando SA:MP..."
      case "verify":
        return "Verificando..."
      case "done":
        return "Instalación completada."
      case "error":
        return progress.message ?? "La instalación falló."
    }
  }

  async function installSamp(): Promise<void> {
    if (installing.value) return
    installing.value = true
    installProgress.value = { phase: "preflight", percent: 0 }

    const detach = launcher.onSampInstallProgress((progress) => {
      installProgress.value = progress
      update("samp", {
        state: progress.phase === "error" ? "error" : "checking",
        detail: describeInstallProgress(progress),
      })
    })

    try {
      const result = await launcher.installSamp()
      if (!result.ok) {
        update("samp", {
          state: "error",
          detail: result.error ?? "La instalación falló.",
        })
        return
      }
      await run()
    } finally {
      detach()
      installing.value = false
      installProgress.value = null
    }
  }

  async function pickGameDir(): Promise<{ ok: boolean; cancelled?: boolean; error?: string }> {
    const result = await launcher.pickGameDir()
    if (result.ok) {
      gtaDetected.value = null
      await run()
      return { ok: true }
    }
    if (result.cancelled) return { ok: false, cancelled: true }
    if (result.error) {
      void dialog.error("Carpeta inválida", result.error)
    }
    return { ok: false, error: result.error }
  }

  /** One of the detected installs, from the chooser. */
  async function chooseGameDir(path: string): Promise<void> {
    gtaPickerOpen.value = false
    const result = await launcher.setGameDir(path)
    if (!result.ok) {
      void dialog.error("Carpeta inválida", result.error ?? "No se pudo usar esa carpeta.")
      return
    }
    gtaDetected.value = null
    await run()
  }

  /** "Cambiar ubicación": the other installs found on this PC, if any;
   *  otherwise straight to the folder picker. */
  async function changeGameDir(): Promise<void> {
    try {
      gtaChoices.value = await launcher.findGameDirs()
    } catch {
      gtaChoices.value = []
    }
    if (gtaChoices.value.some((c) => !c.current)) {
      gtaPickerOpen.value = true
      return
    }
    await pickGameDir()
  }

  /** "Elegir otra carpeta…" in the chooser. */
  async function browseGameDir(): Promise<void> {
    gtaPickerOpen.value = false
    await pickGameDir()
  }

  function openGtaPicker(): void {
    gtaPickerOpen.value = true
  }

  function closeGtaPicker(): void {
    gtaPickerOpen.value = false
  }

  async function installCache(): Promise<void> {
    if (cacheInstalling.value) return
    cacheInstalling.value = true
    cacheInstallProgress.value = { phase: "preflight", percent: 0 }

    const detach = launcher.onCacheInstallProgress((progress) => {
      cacheInstallProgress.value = progress
    })

    try {
      const result = await launcher.installCache()
      detach()
      cacheSync.value = result
      // No error dialog: whatever is still missing, the game downloads. The
      // row turns into a warning with "Reintentar" once re-counted.
      cacheInstallProgress.value = {
        phase: "preflight",
        percent: 0,
        message: "Comprobando archivos del caché...",
      }
      await checkCache()
      // A sync that could not run at all says why (no disk space, game open).
      if (result.error && cacheActionType.value !== null && gtaReady.value && sampReady.value) {
        update("cache", { detail: result.error })
      }
    } finally {
      detach()
      cacheInstalling.value = false
      cacheCancelling.value = false
      cacheInstallProgress.value = null
    }
  }

  /** "Omitir": stops the running sync so the user can play now. What it got
   *  stays; the row turns into the usual warning with "Continuar", and it
   *  does not start again by itself this session. */
  async function cancelCache(): Promise<void> {
    if (!cacheInstalling.value || cacheCancelling.value) return
    cacheCancelling.value = true
    try {
      const stopping = await launcher.cancelCache()
      // Nothing was running (it just finished): `installCache` resets it.
      if (!stopping) cacheCancelling.value = false
    } catch {
      cacheCancelling.value = false
    }
  }

  async function installGta(): Promise<{ ok: boolean; cancelled?: boolean; error?: string }> {
    if (gtaInstalling.value) return { ok: false, error: "Ya hay una instalación en curso." }

    const pick = await launcher.pickEmptyInstallDir()
    if (!pick.ok || !pick.path) {
      if (pick.cancelled) return { ok: false, cancelled: true }
      const error = pick.error ?? "No se pudo seleccionar la carpeta."
      void dialog.error("Carpeta no válida", error)
      return { ok: false, error }
    }

    gtaInstalling.value = true
    gtaInstallProgress.value = { phase: "preflight", percent: 0 }

    const detach = launcher.onGtaInstallProgress((progress) => {
      gtaInstallProgress.value = progress
    })

    try {
      const result = await launcher.installGta(pick.path)
      if (!result.ok) {
        const error = result.error ?? "La instalación falló."
        void dialog.error("Error al instalar", error)
        return { ok: false, error }
      }
      gtaDetected.value = null
      await run()
      void dialog.success("Listo", "GTA San Andreas se instaló correctamente.")
      return { ok: true }
    } finally {
      detach()
      gtaInstalling.value = false
      gtaInstallProgress.value = null
    }
  }

  return {
    entries,
    allOk,
    running,
    installing,
    installProgress,
    lastSampVerification,
    sampActionType,
    requiresElevation,
    gtaMissingAll,
    gtaActionType,
    gtaInstalling,
    gtaInstallProgress,
    gtaPath,
    gtaChoices,
    gtaDetected,
    gtaPickerOpen,
    gtaChoicePending,
    cacheInstalling,
    cacheInstallProgress,
    cacheActionType,
    cacheCancelling,
    cacheSync,
    cacheRetry,
    gameRunning,
    run,
    retryMods,
    installSamp,
    installCache,
    cancelCache,
    pickGameDir,
    chooseGameDir,
    changeGameDir,
    browseGameDir,
    openGtaPicker,
    closeGtaPicker,
    installGta,
  }
})
