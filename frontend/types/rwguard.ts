/** Native RWGuard RenderWare validation types (mirror of `backend/src/rwguard`). */

export type RwGuardVerdict = "safe" | "healed" | "threat" | "blocked"

export interface RwGuardThreat {
  fileName: string
  size: number
  reason: string
  /** `"cache"` = currently on disk; `"download"` = blocked before entering cache. */
  source: "cache" | "download"
  detectedAt: number
}

export interface RwGuardRecord {
  fileName: string
  size: number
  verdict: RwGuardVerdict
  reason?: string
  checkedAt: number
}

export interface RwGuardProgress {
  scanning: boolean
  total: number
  checked: number
  reused: number
  newlyScanned: number
  threats: number
  blocked: number
  /** Models validated concurrently while downloading or extracting the cache. */
  liveValidated: number
  message?: string
}

export interface RwGuardStatus {
  totalModels: number
  validatedModels: number
  reusedFromHistory: number
  newlyScanned: number
  healedModels: number
  activeThreats: RwGuardThreat[]
  blockedThreats: RwGuardThreat[]
  recentHistory: RwGuardRecord[]
  lastScanAt?: number
  scanning: boolean
}

export interface RwGuardActionResult {
  ok: boolean
  affected: number
  error?: string
  status: RwGuardStatus
}
