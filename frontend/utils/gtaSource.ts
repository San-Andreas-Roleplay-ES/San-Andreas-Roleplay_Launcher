import type { GtaSource } from "@/types/gta"

const LABELS: Record<GtaSource, string> = {
  samp: "Usada por SA-MP",
  running: "Abierto ahora",
  "old-launcher": "Launcher anterior",
  rockstar: "Registro de Rockstar",
  steam: "Steam",
  uninstall: "Programas instalados",
  history: "Usada antes en esta PC",
  folder: "Carpeta habitual",
  dev: "Prueba (dev)",
}

/** Short label for where a GTA install was found. */
export function sourceLabel(source: GtaSource): string {
  return LABELS[source]
}
