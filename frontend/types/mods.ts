import type { GtaInstallProgress } from "@/types/gta"

/** What a mod looks like in the game folder (`status::State` in Rust). */
export type ModState =
  /** Not in the folder. */
  | "notInstalled"
  | "installed"
  /** Installed by the launcher; the catalog has a newer version. */
  | "outdated"
  /** Installed by the launcher; a file is missing or was changed. */
  | "damaged"
  /** In place by other means ("ya instalado por tu cuenta"): left alone. */
  | "foreign"
  /** Can't be installed: launcher too old or a dependency unavailable. */
  | "blocked"

/** What the automatic flow would do with it. */
export type ModAction = "none" | "install" | "update" | "repair" | "switch" | "remove"

/** Where the catalog in use came from. `missing`: none usable, nothing installs. */
export type CatalogState = "live" | "saved" | "empty" | "missing"

export interface CatalogInfo {
  state: CatalogState
  /** For the user when something is off. Never blocks playing. */
  detail?: string
  serial?: number
}

export interface ModVariant {
  id: string
  /** "Calidad estándar", "Alta definición"; empty for a single variant. */
  label: string
  version: string
  /** Download size. */
  bytes: number
}

export interface InstalledMod {
  variant: string
  version: string
  /** Found in place with matching files rather than installed by the launcher. */
  adopted: boolean
}

export interface ModStatus {
  id: string
  name: string
  description: string
  category: string
  required: boolean
  /** Wanted on (always, for a required mod). */
  enabled: boolean
  /** The variant wanted. */
  variant: string
  variants: ModVariant[]
  state: ModState
  installed?: InstalledMod
  action: ModAction
  /** Why it is blocked. */
  detail?: string
  /** The last attempt this session failed. */
  error?: string
  installing: boolean
  /** Required, its install failed and it is not in place: Play waits. */
  blocking: boolean
  /** Other installs that clash with it (offered to disable). */
  conflicts: string[]
  /** Those it already renamed out of the way (back when it is removed). */
  disabledConflicts: string[]
  /** Names of the enabled mods that need it: it can't be turned off. */
  requiredBy: string[]
  /** Names of the mods it needs. */
  requires: string[]
}

/** `check_mods`. Never rejects. */
export interface ModsHealth {
  catalog: CatalogInfo
  /** There is a valid game folder to check against. */
  ready: boolean
  mods: ModStatus[]
  /** The automatic flow has work to do (`launcher.syncMods()`). */
  pending: boolean
  busy: boolean
  /** A required mod failed to install: Play waits for it. */
  blocking: boolean
}

export interface ModFailure {
  id: string
  name: string
  error: string
}

export interface ModsSyncResult {
  ok: boolean
  /** The request was refused or could not run at all. */
  error?: string
  failed: ModFailure[]
  cancelled?: boolean
}

/** `mods:install:progress`: for one mod (`id`) or for the whole run. */
export interface ModsProgress extends GtaInstallProgress {
  id?: string
}
