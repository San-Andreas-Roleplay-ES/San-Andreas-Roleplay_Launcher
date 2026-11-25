<div align="center">
  <a href="https://sarp.es/" target="_blank">
    <img style="width: 200px;" src="https://i.imgur.com/EFK15nk.png" alt="San Andreas Roleplay"/>
  </a>
</div>

![Discord Shield](https://discord.com/api/guilds/1118327104908570654/widget.png?style=shield)
[![License: MIT](https://img.shields.io/badge/License-MIT%20License-yellow.svg)](https://opensource.org/licenses/MIT)

Launcher oficial del servidor SAMP de [San Andreas Roleplay](https://sarp.es). Verifica la instalación de GTA SA, instala SA:MP, mantiene al día el caché de modelos del servidor (con el paquete completo si falta mucho, y si no solo los archivos que faltan; pensado también para conexiones malas) y lanza el juego en el servidor oficial.

## Para jugadores

Descargá la última versión desde [Releases](https://github.com/San-Andreas-Roleplay-ES/San-Andreas-Roleplay_Launcher/releases/latest). A partir de la primera instalación, el launcher se actualiza solo cuando hay una nueva versión disponible.

## Para desarrolladores

App de escritorio [Tauri 2](https://tauri.app): frontend Vue 3 + Vite en `frontend/`, backend Rust en `backend/`.

### Requisitos

- **Node** + **yarn 1.x**.
- **Rust stable** con toolchain MSVC (`rustup default stable-x86_64-pc-windows-msvc`).
- **Visual Studio Build Tools** con la carga _"Desarrollo para el escritorio con C++"_ (compilador MSVC + Windows SDK).
- **WebView2 Runtime** (ya viene con Windows 11).

> El repo vive en OneDrive, así que `backend/.cargo/config.toml` manda los artefactos de cargo a `C:\tmp\sarp-target` para no sincronizarlos.

Copiá `.env.example` a `.env` y completá `LAUNCHER_KEY` (la misma que la API). En dev el launcher habla con la API local (`DEV_API_URL`, por defecto `http://localhost:3333`); para probar el caché sin tocar la carpeta real, definí `DEV_CACHE_DIR`, y para probar la detección automática de GTA sin que adopte una instalación real, `DEV_GTA_CANDIDATES` (carpetas separadas por `;`; un `;` solo = "no se encontró nada").

```bash
yarn install
yarn dev      # tauri dev: Vite + backend Rust en modo debug
yarn polish   # format + lint + typecheck (frontend)

cd backend
cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test   # backend
cargo test -- --ignored   # contrato contra la API local + red
```

Documentación técnica completa (stack, estructura, convenciones, decisiones de producto) en [CLAUDE.md](CLAUDE.md).

## Release

Las actualizaciones se distribuyen con `tauri-plugin-updater`, que lee `https://sarp-public.b-cdn.net/launcher/latest.json`. El launcher revisa al arrancar, descarga el instalador nuevo en background y lo aplica cuando el usuario clickea _"Reiniciar para aplicar"_ (o al cerrar el launcher, sin relanzarlo). No hay delta updates: cada update descarga el instalador completo.

Mientras `latest.json` no exista en el CDN el launcher funciona igual (no muestra ningún error), pero cada arranque deja en el log una línea `ERROR` del plugin (`update endpoint did not respond with a successful status code`). Publicar `latest.json` ya con la primera versión Tauri (1.1.0) evita ese ruido.

Los updates van **firmados con minisign**. La clave pública está en [backend/tauri.conf.json](backend/tauri.conf.json) (`plugins.updater.pubkey`); la clave privada se guarda **fuera del repo** y nunca se commitea. Si se pierde, los launchers instalados dejan de aceptar updates.

### Requisitos de la API

El launcher lee el manifest, las métricas, los eventos y la lista del caché de `VITE_API_URL` (`https://api.gta-rol.com`), en `/api/public/launcher/*`, con el header `X-Launcher-Key`. Antes de publicar un launcher que dependa de esos endpoints, la API tiene que tenerlos en producción con la misma `LAUNCHER_KEY` que el `.env` del build:

```bash
curl -s -o /dev/null -w "%{http_code}\n" -H "X-Launcher-Key: <clave>" https://api.gta-rol.com/api/public/launcher/manifest   # 200
curl -s -o /dev/null -w "%{http_code}\n" https://api.gta-rol.com/api/public/launcher/manifest                                # 401
```

Si la API no responde (caída, reiniciando, o responde 401/404), Jugar sigue habilitado:

- **SA:MP** se verifica contra el último manifest bueno, que el launcher guarda en su store cada vez que lo obtiene. En una instalación que nunca lo obtuvo, si `samp.exe` está en la carpeta la fila queda en "no se pudo verificar" (aviso ámbar, no bloquea); si falta, sí bloquea, porque tampoco se puede instalar sin la API.
- **El caché** queda en "no se pudo verificar" (no bloquea Jugar, no descarga ni borra nada). Lo mismo mientras la lista del caché esté vacía (antes de la primera publicación de archivos al bucket). El caché solo bloquea Jugar mientras se sincroniza, y el jugador puede cortarlo con _"Omitir"_; si algunos archivos no se pueden bajar, el juego los descarga al entrar.

La lista del caché puede traer `zip: { url, bytes }` (el caché entero en un zip del CDN). Un launcher al que le falta más de lo que pesa el zip lo baja primero (reanudable, también entre reinicios) y después baja sueltos solo los archivos que el zip no tenía; con `zip: null`, o con una API que todavía no manda el campo, sincroniza archivo por archivo como siempre. Detalles en [CLAUDE.md](CLAUDE.md#caché-del-servidor).

### Pasos para publicar una nueva versión

1. **Bump de versión** en [package.json](package.json) (`"version": "1.1.1"`). Es la única fuente de verdad — `tauri.conf.json` la toma de ahí y el footer del launcher la muestra.

2. **Revisar el `.env`**: `VITE_API_URL=https://api.gta-rol.com` y `LAUNCHER_KEY` igual a la de la API de producción. Se hornean en el binario: cambiarlos después exige otro release.

3. **Build local** con la clave de firma en el entorno:

   ```powershell
   $env:TAURI_SIGNING_PRIVATE_KEY = "C:\ruta\fuera\del\repo\sarp-launcher.key"  # ruta o contenido
   $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = ""
   yarn build
   ```

   Produce en `C:\tmp\sarp-target\release\bundle\nsis\`:
   - `<nombre>_<version>_x64-setup.exe` — el instalador NSIS per-user.
   - `<nombre>_<version>_x64-setup.exe.sig` — la firma minisign del instalador.

4. **Armar `latest.json`**:

   ```json
   {
     "version": "1.1.1",
     "notes": "Correcciones y mejoras.",
     "pub_date": "2026-01-31T12:00:00Z",
     "platforms": {
       "windows-x86_64": {
         "signature": "<contenido completo del .sig>",
         "url": "https://sarp-public.b-cdn.net/launcher/<nombre>_1.1.1_x64-setup.exe"
       }
     }
   }
   ```

   `signature` es el **contenido** del `.sig` (no su URL). Si el nombre del instalador tiene espacios, codificarlos en la `url` (`%20`).

5. **Subir los 3 archivos** (`*-setup.exe`, `*.sig`, `latest.json`) a Bunny CDN bajo el path `/launcher/`. Pueden subirse por el panel web o por la Storage API (`PUT https://storage.bunnycdn.com/<storage-zone>/launcher/<file>` con header `AccessKey: <storage-zone-password>`). Subir `latest.json` **al final**, cuando el instalador ya está disponible, y purgar su caché en Bunny.

6. **Verificar accesibilidad**:

   ```bash
   curl -I https://sarp-public.b-cdn.net/launcher/latest.json
   curl -I https://sarp-public.b-cdn.net/launcher/<nombre>_<version>_x64-setup.exe
   ```

   Ambos deben responder `200 OK`.

7. **Probar el update** desde la versión anterior instalada: abrir el launcher, esperar unos segundos, debería aparecer el banner _"Descargando actualización"_ → _"Reiniciar para aplicar"_. Logs en `%LOCALAPPDATA%\es.sarp.launcher\logs\`.

### Code signing

El `.exe` no está firmado con Authenticode (la firma minisign solo la verifica el updater). Los usuarios verán el SmartScreen _"Windows protected your PC"_ → _"More info"_ → _"Run anyway"_ la primera vez.

## Licencia

MIT — ver [LICENSE](LICENSE).
