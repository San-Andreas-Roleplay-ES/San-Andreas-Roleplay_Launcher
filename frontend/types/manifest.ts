export interface ManifestFileEntry {
  path: string
  url: string
  sha256: string
  size: number
}

export interface ManifestInstallerEntry {
  filename: string
  url: string
  sha256: string
  size: number
  silentArgs: string[]
}

export interface ManifestGtaRipEntry {
  filename: string
  url: string
  sha256: string
  size: number
}

export interface LauncherManifest {
  schemaVersion: number
  generatedAt: string
  samp: {
    files: ManifestFileEntry[]
    installer: ManifestInstallerEntry
  }
  gta: {
    rip: ManifestGtaRipEntry
  }
}
