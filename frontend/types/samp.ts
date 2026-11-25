export type FileVerificationReason = "missing" | "size-mismatch" | "hash-mismatch"

export interface FileVerification {
  path: string
  expected: { sha256: string; size: number }
  actual: { exists: boolean; size: number | null; sha256: string | null }
  ok: boolean
  reason?: FileVerificationReason
}

export interface SampVerificationResult {
  ok: boolean
  files: FileVerification[]
  /** User-facing (Spanish). */
  error?: string
  /** No manifest to check against (API unreachable and none saved yet), but
   *  samp.exe is in the folder: playable, only a warning. */
  unverified: boolean
}

export type InstallPhase = "preflight" | "download" | "install" | "verify" | "done" | "error"

export interface InstallProgress {
  phase: InstallPhase
  percent: number
  message?: string
}

export interface InstallResult {
  ok: boolean
  error?: string
}
