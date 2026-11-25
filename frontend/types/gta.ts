export type GtaInstallPhase =
  | "preflight"
  | "download"
  | "verify"
  | "extract"
  | "shortcut"
  | "done"
  | "error"

export interface GtaInstallProgress {
  phase: GtaInstallPhase
  percent: number
  bytesDone?: number
  bytesTotal?: number
  speedBps?: number
  message?: string
}

export interface GtaInstallResult {
  ok: boolean
  error?: string
  cancelled?: boolean
}

export type ModId = "modloader" | "asi-loader"

export interface DetectedMod {
  id: ModId
  label: string
}

/** Where an install was found (`detect::Source` in Rust), best first. */
export type GtaSource =
  | "samp"
  | "running"
  | "old-launcher"
  | "rockstar"
  | "steam"
  | "uninstall"
  | "history"
  | "folder"
  | "dev"

/** A valid GTA San Andreas install found on this PC. */
export interface GtaCandidate {
  path: string
  source: GtaSource
  hasSamp: boolean
  /** The folder the launcher uses now. */
  current: boolean
}

export interface GtaDetection {
  /** Adopted without asking (the one SA-MP uses, or the only one found). */
  adopted?: GtaCandidate
  /** Every valid install found, best first. Empty when the launcher already
   *  had a valid folder. */
  candidates: GtaCandidate[]
}

export interface PickGameDirResult {
  ok: boolean
  path?: string
  error?: string
  cancelled?: boolean
}
