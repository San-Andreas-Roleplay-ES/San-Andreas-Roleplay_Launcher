/// <reference types="vite/client" />

declare module "*.vue" {
  import type { DefineComponent } from "vue"
  const component: DefineComponent<object, object, unknown>
  export default component
}

// VITE_API_URL is only read by backend/build.rs: the webview never talks to
// the API (or the CDN) itself, everything goes through backend commands.
interface ImportMetaEnv {
  readonly VITE_GAME_SERVER_IP: string
  readonly VITE_GAME_SERVER_PORT: string
  readonly VITE_GAME_SERVER_LOCATION?: string
}

interface ImportMeta {
  readonly env: ImportMetaEnv
}

/** Short git commit hash baked into the bundle by Vite at build time.
 *  Surfaced in the footer ("v1.0.0 (a3f2b1c)") so support can identify
 *  which build a user is running. */
declare const __GIT_COMMIT__: string
