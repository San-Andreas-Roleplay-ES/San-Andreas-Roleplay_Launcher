// Wipes the launcher's persisted state (GTA folder, resumable GTA download,
// window bounds…) so the next start behaves like a fresh install.
const { existsSync, readdirSync, rmSync } = require("node:fs")
const { join } = require("node:path")
const os = require("node:os")

// Tauri stores app data under <roaming AppData>\<identifier> on Windows.
const IDENTIFIER = "es.sarp.launcher"

function appDataDir() {
  if (process.platform === "win32") {
    return process.env.APPDATA ?? join(os.homedir(), "AppData", "Roaming")
  }
  if (process.platform === "darwin") {
    return join(os.homedir(), "Library", "Application Support")
  }
  return process.env.XDG_DATA_HOME ?? join(os.homedir(), ".local", "share")
}

const appDir = join(appDataDir(), IDENTIFIER)

// Any top-level JSON file in the app dir is a store file; matching by extension
// keeps this script working whatever the backend names it.
const stores = existsSync(appDir)
  ? readdirSync(appDir, { withFileTypes: true })
      .filter((entry) => entry.isFile() && entry.name.toLowerCase().endsWith(".json"))
      .map((entry) => join(appDir, entry.name))
  : []

if (stores.length === 0) {
  console.log(`Nothing to reset — no store file in ${appDir}.`)
  process.exit(0)
}

for (const file of stores) {
  try {
    rmSync(file, { force: true })
    console.log(`Removed ${file}`)
  } catch (err) {
    if (err.code === "EBUSY" || err.code === "EPERM") {
      console.error(`\nCannot reset — the launcher is running.`)
      console.error(`Close the app (cierra la ventana) and try again.\n`)
      process.exit(1)
    }
    throw err
  }
}

console.log("Reset complete.")
