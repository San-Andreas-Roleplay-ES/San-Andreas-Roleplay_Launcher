const LABELS: Record<string, string> = {
  essential: "Esenciales",
  hud: "Interfaz",
  interface: "Interfaz",
  graphics: "Gráficos",
  visual: "Gráficos",
  vehicles: "Vehículos",
  audio: "Sonido",
  sound: "Sonido",
  gameplay: "Jugabilidad",
  maps: "Mapas",
  weapons: "Armas",
  skins: "Personajes",
  other: "Otros",
}

/** Spanish heading for a catalog category. New ones (the catalog grows
 *  without a launcher release) show their own name, capitalised. */
export function categoryLabel(category: string): string {
  const known = LABELS[category]
  if (known) return known
  const words = category.replace(/[-_]+/g, " ").trim()
  return words ? words.charAt(0).toUpperCase() + words.slice(1) : LABELS.other
}
