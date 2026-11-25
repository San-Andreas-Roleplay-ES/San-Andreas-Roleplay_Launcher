export interface UpdaterAvailable {
  version: string
  releaseDate?: string
}

export interface UpdaterProgress {
  percent: number
  bytesPerSecond: number
  transferred: number
  total: number
}

export interface UpdaterDownloaded {
  version: string
  releaseDate?: string
}
