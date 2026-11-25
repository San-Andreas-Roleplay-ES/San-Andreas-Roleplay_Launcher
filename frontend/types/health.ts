export interface HealthCheckPayload {
  /** `path`: the folder in use, when it is a valid install. */
  gta: { ok: boolean; detail?: string; missingAll: boolean; path?: string }
  samp: { ok: boolean; detail?: string }
}
