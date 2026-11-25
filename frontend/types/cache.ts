import type { GtaInstallProgress } from "@/types/gta"

/** Same payload as the GTA install (`Progress` in Rust). The cache sync goes
 *  preflight → [download of the package → extract] → download of the files
 *  still missing → done | error; `message` says which step it is in Spanish
 *  ("Descargando paquete del caché...", "Extrayendo 120 de 11400 archivos...",
 *  "Descargando 3 de 260 archivos restantes..."). */
export type CacheInstallProgress = GtaInstallProgress

/** How a sync attempt ended. Nothing here blocks playing: SA-MP downloads the
 *  models it lacks from the game server. */
export interface CacheInstallResult {
  /** Every listed file is in place. */
  ok: boolean
  /** The sync could not run or stopped as a whole (no list, no disk space,
   *  game open...). Per-file failures are counted in `failed` instead. */
  error?: string
  /** Listed files that could not be downloaded (or were never tried). */
  failed: number
  /** The user stopped it (`launcher.cancelCache()`). What it got stays; the
   *  next run continues from there. */
  cancelled?: boolean
}

/** Cache row of the health check (`check_cache`). Never blocks playing.
 *  `ok: false` with neither `needsInstall` nor `needsUpdate` means it could
 *  not be verified (API unreachable, nothing published yet). */
export interface CacheHealth {
  ok: boolean
  detail?: string
  /** None of the listed files is in the folder. */
  needsInstall: boolean
  /** Some files missing or stale. */
  needsUpdate: boolean
  /** Files in the server's list. */
  expectedFileCount?: number
  /** Listed files already in the folder. */
  actualFileCount?: number
}
