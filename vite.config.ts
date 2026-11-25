import { defineConfig } from "vite"
import vue from "@vitejs/plugin-vue"
import { execSync } from "node:child_process"
import { resolve } from "node:path"

// Resolve the short git commit hash at build time so the footer can show e.g.
// "v1.0.0 (a3f2b1c)". Falls back to "unknown" outside a git checkout (CI
// without history, sources extracted from a tarball, etc.) so the build never
// breaks because of this.
function resolveGitCommit(): string {
  try {
    return execSync("git rev-parse --short HEAD", { stdio: ["ignore", "pipe", "ignore"] })
      .toString()
      .trim()
  } catch {
    return "unknown"
  }
}

// Set by the Tauri CLI when it drives Vite through `tauri dev` / `tauri build`.
const tauriDebug = Boolean(process.env.TAURI_ENV_DEBUG)

export default defineConfig({
  plugins: [vue()],
  resolve: {
    alias: {
      "@": resolve(__dirname, "frontend"),
    },
  },
  define: {
    __GIT_COMMIT__: JSON.stringify(resolveGitCommit()),
  },
  // Keep the Rust compiler output visible: Vite would otherwise wipe the terminal.
  clearScreen: false,
  server: {
    // Tauri loads the dev server from a fixed URL (`devUrl` in tauri.conf.json).
    port: 5173,
    strictPort: true,
    watch: {
      ignored: ["**/backend/**"],
    },
  },
  envPrefix: "VITE_",
  build: {
    // WebView2 is an evergreen Chromium.
    target: "chrome105",
    minify: tauriDebug ? false : "esbuild",
    sourcemap: tauriDebug,
  },
})
