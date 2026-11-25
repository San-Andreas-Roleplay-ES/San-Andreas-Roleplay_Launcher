import { invoke } from "@tauri-apps/api/core"
import { listen } from "@tauri-apps/api/event"
import type { UnlistenFn } from "@tauri-apps/api/event"
import type { CacheHealth, CacheInstallProgress, CacheInstallResult } from "@/types/cache"
import type { GameLaunchPayload, GameLaunchResult, GameStatus } from "@/types/game"
import type {
  DetectedMod,
  GtaCandidate,
  GtaDetection,
  GtaInstallProgress,
  GtaInstallResult,
  PickGameDirResult,
} from "@/types/gta"
import type { HealthCheckPayload } from "@/types/health"
import type { LauncherManifest } from "@/types/manifest"
import type { GlobalMetrics, RawServerEvent } from "@/types/public"
import type { InstallProgress, InstallResult, SampVerificationResult } from "@/types/samp"
import type { PingResult } from "@/types/server"
import type { UpdaterAvailable, UpdaterDownloaded, UpdaterProgress } from "@/types/updater"
import type { WindowState } from "@/types/window"

export type Unsubscribe = () => void

/** Subscribes to a backend event and hands back a synchronous unsubscribe.
 *  Tauri's `listen` registers asynchronously, so an unsubscribe issued before
 *  the registration lands is remembered and applied as soon as it does. */
function subscribe<T>(event: string, callback: (payload: T) => void): Unsubscribe {
  let unlisten: UnlistenFn | null = null
  let active = true

  listen<T>(event, ({ payload }) => {
    if (active) callback(payload)
  })
    .then((fn) => {
      if (active) unlisten = fn
      else fn()
    })
    .catch((err: unknown) => console.error(`[launcher] could not listen to ${event}`, err))

  return () => {
    if (!active) return
    active = false
    unlisten?.()
    unlisten = null
  }
}

/** Typed wrapper around the Rust commands and events of the backend. */
export const launcher = {
  /** GTA folder and SA:MP (local only, instant). */
  healthCheck: () => invoke<HealthCheckPayload>("health_check"),

  /** Cache row: compares the folder with the server's list. Never rejects. */
  checkCache: () => invoke<CacheHealth>("check_cache"),

  launchGame: ({ host, port }: GameLaunchPayload) =>
    invoke<GameLaunchResult>("launch_game", { host, port }),

  getGameStatus: () => invoke<GameStatus>("get_game_status"),

  onGameStatusChanged: (callback: (status: GameStatus) => void) =>
    subscribe("game:status:changed", callback),

  /** Rejects with a string when the API cannot be reached. */
  fetchGlobalMetrics: () => invoke<GlobalMetrics>("fetch_global_metrics"),

  /** Rejects with a string when the API cannot be reached. */
  fetchEvents: () => invoke<RawServerEvent[]>("fetch_events"),

  pingServer: (ip: string, port: number) => invoke<PingResult>("ping_server", { ip, port }),

  /** Rejects with a string when the manifest cannot be fetched. */
  fetchManifest: (options: { force?: boolean } = {}) =>
    invoke<LauncherManifest>("fetch_manifest", { force: options.force ?? false }),

  verifySamp: () => invoke<SampVerificationResult>("verify_samp"),

  installSamp: () => invoke<InstallResult>("install_samp"),

  sampInstallRequiresElevation: () => invoke<boolean>("samp_install_requires_elevation"),

  onSampInstallProgress: (callback: (progress: InstallProgress) => void) =>
    subscribe("samp:install:progress", callback),

  pickGameDir: () => invoke<PickGameDirResult>("pick_game_dir"),

  /** Without a valid folder: looks for GTA installs (read-only) and adopts the
   *  one SA-MP uses, or the only one found. With one: does nothing. */
  detectGameDir: () => invoke<GtaDetection>("detect_game_dir"),

  /** The GTA installs found on this PC (read-only). */
  findGameDirs: () => invoke<GtaCandidate[]>("find_game_dirs"),

  /** Uses one of the detected installs. */
  setGameDir: (path: string) => invoke<PickGameDirResult>("set_game_dir", { path }),

  pickEmptyInstallDir: () => invoke<PickGameDirResult>("pick_empty_install_dir"),

  detectGtaMods: () => invoke<DetectedMod[]>("detect_gta_mods"),

  installGta: (targetDir: string) => invoke<GtaInstallResult>("install_gta", { targetDir }),

  onGtaInstallProgress: (callback: (progress: GtaInstallProgress) => void) =>
    subscribe("gta:install:progress", callback),

  /** Syncs the cache folder with the server's list: the package (zip) first
   *  when the folder is far behind, then file by file. */
  installCache: () => invoke<CacheInstallResult>("install_cache"),

  /** Stops the running sync; its `installCache()` then resolves with
   *  `cancelled: true`. `false` when none was running. */
  cancelCache: () => invoke<boolean>("cancel_cache"),

  onCacheInstallProgress: (callback: (progress: CacheInstallProgress) => void) =>
    subscribe("cache:install:progress", callback),

  minimize: () => invoke<void>("window_minimize"),

  close: () => invoke<void>("window_close"),

  toggleMaximize: () => invoke<void>("window_toggle_maximize"),

  toggleFullscreen: () => invoke<void>("window_toggle_fullscreen"),

  getWindowState: () => invoke<WindowState>("get_window_state"),

  onWindowStateChange: (callback: (state: WindowState) => void) =>
    subscribe("window:state:changed", callback),

  openExternal: (url: string) => invoke<void>("open_external", { url }),

  getAppVersion: () => invoke<string>("get_app_version"),

  onUpdaterAvailable: (callback: (info: UpdaterAvailable) => void) =>
    subscribe("updater:available", callback),

  onUpdaterProgress: (callback: (progress: UpdaterProgress) => void) =>
    subscribe("updater:progress", callback),

  onUpdaterDownloaded: (callback: (info: UpdaterDownloaded) => void) =>
    subscribe("updater:downloaded", callback),

  onUpdaterError: (callback: (message: string) => void) => subscribe("updater:error", callback),

  /** Runs the downloaded installer; the backend exits and relaunches the app. */
  quitAndInstall: () => invoke<void>("updater_install"),
}
