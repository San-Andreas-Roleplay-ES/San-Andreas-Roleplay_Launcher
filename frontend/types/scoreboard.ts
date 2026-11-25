export interface ScoreboardEntry {
  id: number
  name: string
  score: number
  ping: number
}

export interface ScoreboardSnapshot {
  players: ScoreboardEntry[]
  totalPlayers: number
  maxPlayers: number
  queriedAt: number
}
