export interface GameStatus {
  running: boolean
  pid: number | null
}

export interface GameLaunchPayload {
  host: string
  port: number
}

export interface GameLaunchResult {
  ok: boolean
  error?: string
}
