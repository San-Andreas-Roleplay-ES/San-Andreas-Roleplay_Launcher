# CLAUDE.md — San Andreas Roleplay · SA:MP Launcher

Launcher oficial del servidor SAMP de San Andreas Roleplay. App de escritorio Windows (Tauri 2: frontend Vue en WebView2 + backend Rust) que verifica la instalación de GTA SA, instala SA:MP desde el CDN, sincroniza el caché de modelos del servidor archivo por archivo y lanza el juego. Los datos (manifest, métricas, eventos, lista del caché) vienen en vivo de la API; el CDN solo sirve las descargas pesadas.

---

## Skills del proyecto

Este repo incluye una colección de skills locales en [.agents/skills/](.agents/skills/) — guías de referencia (cada una con su propio `SKILL.md`) que Claude **debe consultar al iniciar tareas relacionadas** antes de generar código:

| Carpeta                                                                                | Cuándo usarla                                                              |
| -------------------------------------------------------------------------------------- | -------------------------------------------------------------------------- |
| [.agents/skills/vue/](.agents/skills/vue/)                                             | Cualquier trabajo con componentes Vue 3 (`<script setup>`, props, slots).  |
| [.agents/skills/vue-best-practices/](.agents/skills/vue-best-practices/)               | Patrones recomendados, organización, naming, antipatrones a evitar.        |
| [.agents/skills/vue-pinia-best-practices/](.agents/skills/vue-pinia-best-practices/)   | Diseño de stores Pinia, composición, persistencia, tipado.                 |
| [.agents/skills/vue-debug-guides/](.agents/skills/vue-debug-guides/)                   | Diagnóstico de bugs en Vue (reactividad, lifecycle, DevTools).             |
| [.agents/skills/tailwind-css-patterns/](.agents/skills/tailwind-css-patterns/)         | Patrones de Tailwind, composición de clases, variantes, arbitrary values.  |
| [.agents/skills/frontend-design/](.agents/skills/frontend-design/)                     | Decisiones de UX/UI, jerarquía visual, layouts, microinteracciones.        |
| [.agents/skills/accessibility/](.agents/skills/accessibility/)                         | A11y: roles ARIA, contraste, navegación por teclado, lectores de pantalla. |
| [.agents/skills/typescript-advanced-types/](.agents/skills/typescript-advanced-types/) | Generics, conditional types, mapped types, type narrowing avanzado.        |
| [.agents/skills/vite/](.agents/skills/vite/)                                           | Configuración de Vite, plugins, optimización de build, HMR.                |
| [.agents/skills/nodejs-backend-patterns/](.agents/skills/nodejs-backend-patterns/)     | (No aplica — el backend es Rust. Solo para scripts de Node en `scripts/`.) |
| [.agents/skills/nodejs-best-practices/](.agents/skills/nodejs-best-practices/)         | (No aplica — el backend es Rust. Solo para scripts de Node en `scripts/`.) |
| [.agents/skills/seo/](.agents/skills/seo/)                                             | (No aplica al launcher — una app de escritorio no se indexa.)              |

**Reglas de uso:**

1. **Antes de implementar** una feature/refactor que toque un dominio listado, **leer el `SKILL.md` correspondiente** y aplicar sus recomendaciones.
2. Si una skill tiene `references/` o `GENERATION.md`, consultarlos también si la tarea es no-trivial.
3. La decisión de cuál skill activar es del asistente — el usuario no necesita pedirlo. Cuando se use, mencionarlo brevemente al inicio del trabajo (_"Leyendo skill `vue-best-practices` antes de tocar este componente"_).
4. Si la tarea es trivial (cambio de copy, ajuste de color, typo), no hace falta consultar skill.

---

## Stack Tecnológico

| Capa               | Tecnología                                                                         |
| ------------------ | ---------------------------------------------------------------------------------- |
| Framework UI       | Vue 3 (`<script setup>`) + Composition API                                         |
| Lenguaje           | TypeScript 5 (frontend) · Rust stable, toolchain MSVC (backend)                    |
| Bundler            | Vite 5                                                                             |
| Runtime            | Tauri 2 — WebView2 (Chromium evergreen) en Windows                                 |
| Empaquetado        | `tauri build` → instalador NSIS per-user, x64                                      |
| Estado             | Pinia                                                                              |
| IPC                | `@tauri-apps/api` (`invoke` + `listen`), envuelto en `frontend/api/launcher.ts`    |
| HTTP               | Desde el backend (`net/`: API y descargas) — el frontend no hace requests directas |
| UI Components      | PrimeVue 4 (tema Aura)                                                             |
| Estilos            | Tailwind CSS 3                                                                     |
| Persistencia local | Store JSON del backend en `%APPDATA%\es.sarp.launcher\`                            |
| Auto-update        | `tauri-plugin-updater` → `latest.json` en Bunny CDN, firmado con minisign          |
| Diálogos / enlaces | `tauri-plugin-dialog` · `tauri-plugin-opener`                                      |
| Logs               | `tauri-plugin-log`                                                                 |
| DX                 | ESLint + Prettier + vue-tsc · `cargo fmt` + `cargo clippy` + `cargo test`          |
| Package manager    | yarn 1.x (frontend) · cargo (backend)                                              |

---

## Estructura de Directorios

```
launcher/
├── frontend/                # Frontend Vue (corre dentro de WebView2)
│   ├── api/                 # launcher.ts (bridge IPC tipado) + public.ts (métricas y eventos normalizados)
│   ├── assets/              # Assets importados desde código (graffiti, sonidos)
│   ├── components/          # Componentes Vue por feature (brand/, dialog/, home/)
│   ├── composables/         # useWindowState, useGameStatus, useServerPing, useUpdater, …
│   ├── layouts/             # ShellLayout (title bar + banner de update + footer)
│   ├── pages/               # Vistas full-screen (HomePage)
│   ├── stores/              # Pinia (healthCheck, dialog)
│   ├── types/               # Payloads del IPC (espejo de los structs serde de Rust)
│   ├── utils/               # Helpers puros (nativeFeel, randomTagline)
│   ├── App.vue
│   ├── main.ts              # Bootstrap Vue + Pinia + PrimeVue + guards de app nativa
│   ├── style.css            # Tailwind + reset global
│   └── env.d.ts             # Tipos de import.meta.env y __GIT_COMMIT__
├── backend/                 # Backend Rust (Tauri 2)
│   ├── src/
│   │   ├── main.rs, lib.rs  # Arranque y builder (plugins, estado, comandos)
│   │   ├── commands.rs      # Comandos expuestos al frontend (delgados, delegan en los módulos)
│   │   ├── config.rs, store.rs  # Config horneada en build y store persistente (JSON)
│   │   ├── app/             # window, updater, external, events
│   │   ├── game/            # paths, detect, health, launcher, mods, shortcut
│   │   ├── install/         # samp, gta, cache (+ cache/package: zip, cache/pace: paralelismo), verify, extract, progress, cancel, powershell, fs
│   │   └── net/             # api (API + X-Launcher-Key), http, manifest, query/ (consulta UDP a SA:MP)
│   ├── capabilities/        # Permisos que la ventana tiene sobre core y plugins
│   ├── icons/               # Íconos de la app, la ventana y el instalador
│   ├── build.rs             # Hornea VITE_API_URL / LAUNCHER_KEY / VITE_GAME_CACHE_FOLDER (y los DEV_* en debug)
│   ├── tauri.conf.json      # Ventana, bundle NSIS, updater (endpoint + pubkey)
│   ├── Cargo.toml
│   └── .cargo/config.toml   # target-dir → C:\tmp\sarp-target (fuera de OneDrive)
├── public/                  # Assets estáticos embebidos (NO van al CDN)
│   ├── logo.png
│   ├── logo-squared.png
│   ├── favicon.ico
│   └── background.png
├── scripts/reset.cjs        # `yarn reset`: borra el estado persistido del launcher
├── index.html               # Boot skeleton estático (se pinta antes de que cargue Vue)
├── .env / .env.example      # VITE_API_URL, LAUNCHER_KEY, VITE_GAME_SERVER_*, VITE_GAME_CACHE_FOLDER, DEV_*
├── vite.config.ts
├── tailwind.config.js + postcss.config.js
├── tsconfig.json + tsconfig.node.json
├── eslint.config.mjs
└── package.json             # Única fuente de la versión del launcher
```

---

## Decisiones de Producto

### Identidad

- **Nombre completo**: `San Andreas Roleplay - SA:MP Launcher` (window title, productName del instalador).
- **Nombre corto** (shortcuts, taskbar): `SARP SA:MP Launcher`.
- **Identifier** (Tauri): `es.sarp.launcher`. Define también las carpetas de datos (`%APPDATA%\es.sarp.launcher\`, `%LOCALAPPDATA%\es.sarp.launcher\`).
- **Idioma de la UI**: español. Código y comentarios: inglés.
- **Copyright**: `© 2023 - {currentYear}` (año dinámico, lo arranca en 2023).

### Versionado

- **La versión vive solo en `package.json:version`**. `tauri.conf.json` la toma de ahí (`"version": "../package.json"`), el backend la expone con `get_app_version` y el footer del `ShellLayout` la muestra junto al commit (`__GIT_COMMIT__`). Bumpear la versión solo requiere editar `package.json`.
- El usuario instala una vez. Las versiones siguientes llegan vía **`tauri-plugin-updater` desde Bunny CDN**: el launcher lee `https://sarp-public.b-cdn.net/launcher/latest.json` al arrancar, descarga el instalador en background (el `UpdaterBanner` muestra el progreso) y lo aplica cuando el usuario clickea _"Reiniciar para aplicar"_ (`updater_install`), que ejecuta el instalador y relanza el launcher. Si el usuario cierra el launcher sin clickear, el update se instala al salir (sin relanzar). Si la descarga o la firma fallan, el banner se oculta y se reintenta en el próximo arranque.
- **Updates firmados con minisign**. La clave pública está en `tauri.conf.json` (`plugins.updater.pubkey`); el updater rechaza cualquier instalador cuya firma no coincida. La clave privada **no está en el repo**: se pasa al compilar con `TAURI_SIGNING_PRIVATE_KEY` (ruta o contenido) y `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` (vacía). Si se pierde la clave privada, los launchers instalados no aceptan más updates y hay que reinstalar a mano.
- Un release publica en `/launcher/` del CDN: el `*-setup.exe`, su `.sig` y `latest.json` (`version`, `notes`, `pub_date`, `platforms["windows-x86_64"] = { signature, url }`).
- **No hay delta updates** (no existe blockmap): cada update descarga el instalador completo.

### Auth (v1)

- **Sin auth de usuario**. Los datos salen de los endpoints `GET /api/public/launcher/{manifest,global-metrics,events,cache}` de la API (`VITE_API_URL`), que responden `401` sin el header `X-Launcher-Key`. La clave (`LAUNCHER_KEY`) va horneada en el binario del backend, así que solo identifica el tráfico del launcher, no protege nada; aun así el backend la manda **solo a la API** (cliente sin redirects), nunca al CDN ni a otro host, y no la escribe en logs ni mensajes. No lleva prefijo `VITE_`: nunca llega al bundle del frontend.
- A futuro: cookie `session` JWT (compartida con el UCP). Como el frontend no hace HTTP directo (todo pasa por comandos Rust), la sesión se gestionaría en el backend.

### Distribución y ubicación del launcher

- **El launcher se distribuye como instalador NSIS per-user** (`bundle.windows.nsis.installMode: "currentUser"`). No pide UAC, instala en `%LOCALAPPDATA%\San Andreas Roleplay - SA-MP Launcher\`, agrega shortcuts en el Start Menu y el Escritorio y una entrada en _Aplicaciones y características_. NSIS porque permite instalar sin admin y es el formato que `tauri-plugin-updater` sabe aplicar en Windows. Si falta el runtime de WebView2 (Windows 10 viejos), el instalador lo descarga.
- **Una sola instancia** (`tauri-plugin-single-instance`, registrado primero en el builder): abrir el launcher otra vez trae al frente la ventana abierta y el segundo proceso sale antes de arrancar nada (dos procesos harían dos sincronizaciones del caché y dos updaters).
- **La carpeta de GTA es un dato gestionado dentro de la app**, no algo que un wizard configura por adelantado. Mientras no haya una carpeta válida el backend no tiene carpeta de juego y los health checks reportan error con un botón _"Cambiar ubicación"_ visible.
- **Flujo de primera apertura (detección automática)**: sin carpeta guardada (o si la guardada ya no es una instalación válida), el launcher busca GTA SA en la PC (`game/detect.rs`, comando `detect_game_dir`), solo leyendo y sin pedir permisos: `HKCU\Software\SAMP` → `gta_sa_exe` (la que usa SA-MP), un `gta_sa.exe` abierto, el store del launcher anterior (`%APPDATA%\san-andreas-roleplay-launcher\config.json` → `gtaPath`), las claves de Rockstar, Steam (`libraryfolders.vdf`), las entradas de _Aplicaciones y características_, el historial de ejecutables de Windows (AppCompatFlags y MuiCache) y las carpetas habituales (`Program Files`, `Documentos\GTA San Andreas` y las carpetas "GTA…" junto a `GTA San Andreas User Files`). Nunca recorre discos enteros. Cada ruta se valida igual que una elegida a mano, se descartan las que no existen y se deduplican (sin distinguir mayúsculas).
  - Si la de SA-MP es válida, se adopta sola; si hay **una sola** instalación válida, también. Adoptar es lo mismo que elegirla a mano: se guarda en el store, se crea `SARP Launcher.lnk` dentro de la carpeta apuntando al `.exe` instalado y corren los health checks. La fila de GTA dice _"Detectada automáticamente en …"_ y _"Cambiar ubicación"_ sigue visible.
  - Si hay **varias** y ninguna es la de SA-MP, se abre un selector (una vez por sesión; después, el botón _"Elegir"_ de la fila) con cada carpeta, dónde se encontró y si tiene SA-MP; un clic la usa (`set_game_dir`). _"Elegir otra carpeta…"_ abre el selector nativo.
  - Si no hay ninguna, el flujo de siempre: _"Cambiar ubicación"_ (selector nativo) o _"Instalar"_ (descarga GTA del CDN en una carpeta vacía).
  - Con una carpeta válida guardada no se detecta nada al arrancar. _"Cambiar ubicación"_ ofrece las otras instalaciones encontradas (`find_game_dirs`) junto con el selector nativo; si no hay otras, abre directo el selector.
  - El shortcut es idempotente: cambiar de carpeta repointea el `.lnk` en la nueva, no borra el de la anterior.
- **En dev** (`yarn dev`), `DEV_GTA_PATH` del `.env` es la carpeta que propone el selector, `DEV_GTA_CANDIDATES` (si está) reemplaza todas las fuentes de la detección, la API es `DEV_API_URL` (por defecto `http://localhost:3333`), `DEV_CACHE_DIR` (si está) reemplaza la carpeta del caché, los shortcuts no se crean y el updater no corre (solo corre en builds release). Los `DEV_*` también se pueden pasar como variables de entorno al proceso; los builds release los ignoran.

### Funcionalidades v1

1. **Verificar GTA SA** en la carpeta elegida (o instalarlo desde el RIP del CDN si no está).
2. **Verificar e instalar SA:MP**: compara los archivos contra el manifest (tamaño + sha256) y, si faltan o están dañados, ejecuta el instalador oficial (pide elevación si la carpeta lo exige). Si la API no responde se verifica contra el último manifest guardado; sin ninguno, con `samp.exe` en la carpeta queda en "no se pudo verificar" (aviso que **no bloquea Jugar**). Solo un SA:MP que falta o está dañado bloquea Jugar.
3. **Sincronizar el caché del servidor** en `Documents\GTA San Andreas User Files\SAMP\cache\<VITE_GAME_CACHE_FOLDER>\`: si al jugador le falta mucho baja primero el zip del caché (reanudable), después solo los archivos que siguen faltando, y borra los viejos; se puede omitir para jugar ya (ver [Caché del servidor](#caché-del-servidor)).
4. **Lanzar el juego**: el backend valida IP (IPv4) y puerto igual que el ping, ejecuta `samp.exe IP:PUERTO` y vigila `gta_sa.exe` para reflejar el estado _"Conectado"_. Jugar depende de GTA y SA:MP; el caché solo lo bloquea **mientras se sincroniza** (el backend también se niega a lanzar en ese momento).
5. **Estado del servidor** (query SA:MP por UDP: ping y jugadores), **métricas** y **eventos** desde la API.
6. **Auto-actualizar el launcher** cuando haya nueva versión publicada.

**Fuera de alcance v1**: auth, edición de `samp.cfg`, perfil del usuario, chat, Socket.IO, onboarding (no hace falta — el instalador ya hace ese rol).

### CDN y assets

- **CDN**: `https://sarp-public.b-cdn.net` (Bunny). Solo descargas: archivos e instalador de SA:MP, RIP de GTA, archivos del caché (`/cache-files/<VITE_GAME_CACHE_FOLDER>/`), updates del launcher (`/launcher/`).
- **API** (`VITE_API_URL`, en vivo): manifest, métricas, eventos y la lista del caché. Cambia sin recompilar.
- **Regla de oro**: _"¿Lo necesito para mostrar la primera pantalla sin internet?"_ → sí, va en `public/`. No, va al CDN (o a la API si es un dato).
- **`public/`** (embebido en el `.exe`): logo, favicon, background, íconos UI, fuentes propias.
- **Versionado**: lo que cambia de contenido bajo el mismo nombre lleva `?v=N` en su URL. Los archivos del caché no lo necesitan: su nombre es el CRC de su contenido.

### Manifest

- `GET {API}/api/public/launcher/manifest` → envelope `{ message, httpCode, source, payload }` con `payload = { schemaVersion: 1, generatedAt, samp: { files, installer }, gta: { rip } }`: URLs (del CDN), tamaños y sha256 de los archivos de SA:MP, su instalador y el RIP de GTA. Ya no trae sección `cache` (si una versión vieja la trae, se ignora).
- La API lo arma en vivo desde el bucket (la primera vez tras reiniciarla tarda ~6 s, por eso el timeout es de 25 s). El backend lo cachea en memoria con reintentos 1 s / 2 s / 4 s (`fetch_manifest`; `force: true` lo vuelve a pedir) y guarda el último bueno en el store (`manifest`). Si la API no responde (caída, reiniciando, 401) usa el guardado: con uno guardado hace un solo intento rápido (10 s) en vez de los reintentos, así Jugar no espera a la API. Subir una versión nueva de un archivo de SA:MP al bucket basta (la API recalcula tamaño y sha256), **sin republicar el launcher**; el RIP de GTA está fijo en la API.
- Un manifest con `schemaVersion` mayor al soportado se rechaza: obliga a actualizar el launcher.

### Caché del servidor

- SA-MP guarda cada modelo descargado como `0x<CRC32>.dff|txd` (CRC-32 IEEE del contenido, hex en mayúsculas sin ceros a la izquierda, extensión en minúsculas: `^0x[0-9A-F]{1,8}\.(dff|txd)$`). La API publica los mismos archivos con los mismos nombres en el CDN.
- `GET {API}/api/public/launcher/cache` → `{ baseUrl, zipUploadedAt, count, totalBytes, files: [[nombre, bytes], ...], zip: { url, bytes } | null }`. Cada archivo está en `${baseUrl}/${nombre}`. `zip` es el caché entero en un solo zip del CDN (`url` con `?v=<etag>`: cada subida nueva cambia la URL); viene `null` si `zipUploadedAt` es `null`, y una API vieja sin el campo cuenta como `null`. El zip trae casi todos los archivos de la lista (los skins custom se publican solo sueltos) y puede traer alguno que la lista ya no tiene. Un `zip` inválido se ignora (no invalida la lista). El backend guarda la lista en memoria 60 s, así el health check y la sincronización que dispara comparten un solo pedido. El timeout de la lista (20 s) es para que llegue la respuesta; el cuerpo (cientos de KB) puede tardar lo que haga falta mientras siga llegando.
- **Health check** (`check_cache`): escanea la carpeta (un nivel, sin seguir links). Falta un archivo si no está o tiene otro tamaño. `ok` si no falta nada ni hay nada para borrar; `needsInstall` si no hay ninguno; `needsUpdate` si no. Sin lista (API caída) o con la lista vacía (todavía no se publicó nada) responde "no se pudo verificar": un aviso que no dispara nada. **El caché nunca bloquea Jugar salvo mientras se sincroniza**: lo que falte lo baja el juego al conectarse.
- **Sincronización** (`install_cache`, progreso por `cache:install:progress`): arranca sola cuando GTA y SA:MP están bien y el juego no está abierto (el backend también se niega si `gta_sa.exe` corre). Escanea, borra lo viejo (ver abajo) y elige el camino según cuánto le falta al jugador (`route`, función pura con tests):
  - **Paquete (zip) primero** si la lista tiene `zip` y lo que falta pesa más que lo que queda por bajar del zip (su tamaño menos lo que ya está bajado de una vez anterior), y además entran en el disco el zip y los archivos sueltos (mismo disco: la suma más 256 MB de margen; discos distintos: cada uno lo suyo). Si no entra, va archivo por archivo, que necesita menos espacio. Un jugador nuevo (1,9 GB que faltan contra un zip de 850 MB) va por el zip; uno al día al que le faltan unos skins, archivo por archivo sin tocar el zip.
  - El zip se baja **reanudable** a `%TEMP%\sarp-cache-zip\cache-<hash>.zip` (nunca dentro de la carpeta del caché: el juego no debe ver un zip a medio bajar y Documentos suele estar en OneDrive). El nombre sale del hash de URL + tamaño: una subida nueva (otro `?v=`) descarta el parcial viejo. Con `Range: bytes=N-` sigue desde lo que ya hay, también entre reinicios del launcher; si el servidor ignora el Range (200) empieza de cero limpio; si el tamaño no cuadra (416, `Content-Range`/`Content-Length` distintos) borra el parcial y empieza de cero una vez. Sin redirects, https en release y nunca con `X-Launcher-Key`. El barrido de temporales del arranque no lo toca; lo borra recién si nadie lo escribió en 7 días.
  - **Extracción** (en un hilo bloqueante, con progreso _"Extrayendo N de M archivos..."_): solo entradas cuyo nombre base (sin carpetas: da igual si el zip trae una carpeta envoltorio o rutas raras) es uno de los archivos que faltan de la lista, con su tamaño y su CRC; todo lo demás se ignora (otros nombres, archivos que ya están, archivos que la lista no conoce). Cada una se escribe a un `*.sarp-part`, se verifica el CRC mientras se descomprime (sin buffers grandes) y se renombra; las que no coinciden se descartan. Al terminar se borra el zip.
  - **Después, archivo por archivo** lo que siga faltando (los skins, lo que el zip no tenía, lo que falló), desde `baseUrl`: cada uno a un temporal `*.sarp-part` en la misma carpeta, aceptado solo si tamaño y CRC coinciden con el nombre, y renombrado. Si el zip no se pudo bajar del todo, está corrupto o no se puede abrir, se cae a este camino en la misma corrida con todos los que faltan: nunca se queda peor que antes.
  - **Paralelismo adaptativo** (`pace.rs`, función pura con tests): empieza con 12 archivos a la vez; cada timeout o atasco baja un nivel (12 → 6 → 3 → 1) y una racha larga de éxitos sube uno (cada bajada duplica la racha que hace falta para volver a subir). Cuando la línea se ahoga fallan todos a la vez: solo los que empezaron en el nivel actual cuentan, así una ráfaga cuesta un nivel y no cuatro. Los reintentos (3 intentos por archivo) vuelven a la cola y esperan su turno como los demás. Se corta por "conexión caída" cuando fallan seguidos el doble de archivos que el nivel actual, mínimo 8 (24 a 12 en paralelo, como antes; 8 de a uno).
  - **Timeouts sin tope total**: ninguna descarga tiene un límite para la transferencia completa (una línea lenta que sigue mandando datos nunca se corta). Lo que vence es la espera: 30 s para la respuesta y 30 s sin recibir nada (60 s en el zip). El zip reintenta con espera creciente (1 s → 30 s) mientras cada intento llegue más lejos que el anterior; se rinde después de 5 intentos seguidos sin avanzar. Lo mismo vale para el RIP de GTA y para los archivos e instalador de SA:MP (3 intentos).
  - Antes de bajar archivos sueltos revisa el espacio libre. Volver a correrla solo baja lo que sigue faltando (y retoma el zip).
- **"Omitir"** (`cancel_cache`): mientras sincroniza, la fila del caché muestra _"Omitir"_. El backend corta en el próximo chunk (cancelación cooperativa): los archivos en curso se descartan sin dejar `*.sarp-part`, lo ya bajado o extraído queda, y el zip a medias queda para retomarlo. `install_cache` devuelve `{ ok: false, failed: N, cancelled: true }`, la fila pasa al aviso ámbar con _"Continuar"_ y Jugar se habilita. En esa sesión no vuelve a arrancar sola (ni con el botón de actualizar): solo con _"Continuar"_ o en el próximo arranque.
- **Si la sincronización no termina bien** (archivos que fallan, corte de conexión, falta de espacio): `install_cache` devuelve `{ ok: false, failed: N }` (o `error` si no pudo correr) y la fila queda en aviso ámbar _"Faltan N archivos del caché; el juego los descargará al entrar"_ con _"Reintentar"_; Jugar queda habilitado y no hay diálogo de error. En la misma sesión no se reintenta sola: solo con _"Reintentar"_, con el botón de actualizar o en el próximo arranque.
- **Progreso** (`cache:install:progress`, cada 150 ms como mucho; mismo payload que la instalación de GTA): _"Descargando paquete del caché..."_ con bytes, velocidad y lo ya bajado de antes (si se cortó: _"Se cortó la conexión. Reintentando..."_), _"Extrayendo N de M archivos..."_, _"Descargando N de M archivos restantes..."_ (o _"Descargando N de M archivos..."_ sin zip) y `done`.
- **Borrado de archivos viejos** (la carpeta es de este servidor): se borran los archivos con nombre de caché que no están en la lista. Salvaguardas: nunca se toca nada que no cumpla la regex ni se entra en subcarpetas o links; nunca se borra si la lista está vacía, si `zipUploadedAt` es `null`, si la lista trajo entradas inválidas o si no se pudo obtener; se conservan los modificados después de `zipUploadedAt` (los bajó el juego después del último zip y la lista todavía no los conoce); y no se borra nada si lo viejo es más de la mitad de los archivos de la carpeta y más de 100 (una lista incompleta nunca vacía el caché de un jugador; queda en el log). Los temporales de una sincronización cortada se limpian.
- Solo descarga de la `baseUrl` y del `zip.url` que dio la API (https obligatorio en release, sin redirects) y rechaza nombres que no cumplan la regex.

---

## Variables de Entorno

En el frontend, leer siempre desde `import.meta.env.VITE_*` (Vite solo expone el prefijo `VITE_`).

```bash
# API de los builds release (https obligatorio)
VITE_API_URL=https://api.gta-rol.com

# Solo dev: la API local
DEV_API_URL=http://localhost:3333

# Clave del header X-Launcher-Key (la misma que LAUNCHER_KEY de la API)
LAUNCHER_KEY=...

# Servidor de juego
VITE_GAME_SERVER_IP=135.148.128.72
VITE_GAME_SERVER_PORT=7777
VITE_GAME_SERVER_LOCATION=Miami

# Subcarpeta del caché del servidor (también la usa el backend)
VITE_GAME_CACHE_FOLDER=135.148.128.72.7777

# Solo dev: carpeta que propone el selector de GTA
DEV_GTA_PATH=C:\Program Files (x86)\Rockstar Games\GTA San Andreas

# Solo dev: carpeta que reemplaza a la del caché (para probar sin tocar la real)
# DEV_CACHE_DIR=C:\tmp\sarp-cache

# Solo dev: carpetas (separadas por ;) que reemplazan TODAS las fuentes de la
# detección de GTA; un ";" solo = "no se encontró nada"
# DEV_GTA_CANDIDATES=C:\tmp\gta\A;C:\tmp\gta\B
```

- `VITE_API_URL`, `LAUNCHER_KEY` y `VITE_GAME_CACHE_FOLDER` los lee **`backend/build.rs` en tiempo de compilación** (son obligatorios: el build falla con un mensaje claro si falta alguno, o si `VITE_API_URL` no es https) y quedan horneados en el binario Rust. Los `DEV_*` solo se hornean en builds debug.
- El frontend solo usa `VITE_GAME_SERVER_*` (y nunca habla con la API ni con el CDN: todo pasa por comandos del backend).
- **Importante**: el `.env` se _bundlea_ dentro del `.exe` en build (en el JS del frontend y en el binario del backend). Cualquier cambio requiere republicar el launcher. Por eso lo dinámico (archivos, versiones) vive en la API, no en `.env`.

---

## Reglas Globales de UI

Aplicadas en [frontend/style.css](frontend/style.css) y [frontend/utils/nativeFeel.ts](frontend/utils/nativeFeel.ts). Valen para toda la app:

- **Look de app nativa, no de página web**: `user-select: none` global, `cursor: default`.
- **Sin comportamiento de navegador**: `nativeFeel.ts` (llamado desde `frontend/main.ts`) bloquea el menú contextual, recargar (F5 / Ctrl+R / Ctrl+Shift+R, solo en build de producción), imprimir, buscar, ver código fuente, caret browsing (F7) y el zoom (Ctrl +/−/0 y Ctrl+rueda).
- **Imágenes no arrastrables**: `<img>` con `pointer-events: none` y `-webkit-user-drag: none`.
- **Excepción de selección de texto**: `<input>`, `<textarea>`, `[contenteditable]` mantienen `user-select: text`.
- **Drag region**: la ventana es frameless (`decorations: false`); la title bar propia ([TitleBar.vue](frontend/components/brand/TitleBar.vue)) mueve la ventana con el atributo `data-tauri-drag-region`.
  - El atributo **solo aplica al elemento que recibe el mousedown, no a sus hijos**: ponerlo en cada hijo no interactivo que deba arrastrar (contenedores, textos). Las `<img>` ya son click-through por `pointer-events: none`.
  - Botones y links van sin el atributo y no necesitan nada más.
  - El doble click sobre la región maximiza/restaura de forma nativa — no agregar un handler propio (togglearía dos veces).
  - Requiere los permisos `core:window:allow-start-dragging` y `core:window:allow-internal-toggle-maximize` en `backend/capabilities/`.
- **Tema**: oscuro por defecto (gaming/SAMP). Tailwind con `darkMode: "class"`, clase `.dark` ya aplicada por el sistema PrimeVue Aura.
- **Tipografía**: `system-ui, -apple-system, "Segoe UI", sans-serif` (stack nativo Windows). No cargar fuentes web — el launcher debe arrancar offline.

---

## Iconografía

| Contexto                   | Archivo                                                                  |
| -------------------------- | ------------------------------------------------------------------------ |
| Ventana, taskbar y `.exe`  | `backend/icons/icon.ico` (listado en `bundle.icon` de `tauri.conf.json`) |
| Instalador NSIS            | `backend/icons/icon.ico` (`bundle.windows.nsis.installerIcon`)           |
| Favicon del frontend       | `public/favicon.ico`                                                     |
| Logo en UI / boot skeleton | `public/logo.png`, `public/logo-squared.png`                             |

El `.ico` debe ser multi-resolución (16/24/32/48/64/128/256). `yarn tauri icon <png>` regenera el set de `backend/icons/` desde un PNG cuadrado grande.

---

## Configuración de Tauri

[backend/tauri.conf.json](backend/tauri.conf.json):

- `identifier: "es.sarp.launcher"`, `version: "../package.json"`.
- `build`: `beforeDevCommand: "yarn dev:web"` + `devUrl: "http://localhost:5173"` (puerto fijo, `strictPort` en Vite) y `beforeBuildCommand: "yarn build:web"` + `frontendDist: "../dist"`.
- Ventana frameless (`decorations: false`); tamaño por defecto y mínimo definidos en `app.windows`.
- DevTools solo existen en builds debug (`yarn dev`): F12 / Ctrl+Shift+I. En release no se compilan.
- **Permisos**: los `#[tauri::command]` propios están disponibles para la ventana; las APIs core y los plugins (eventos, drag de ventana, updater, dialog, opener…) solo funcionan si `backend/capabilities/` los habilita.
- **Target de cargo fuera del repo**: `backend/.cargo/config.toml` redirige `target-dir` a `C:\tmp\sarp-target`, porque el repo vive en OneDrive y sincronizar gigas de artefactos de compilación lo rompe. Los instaladores quedan en `C:\tmp\sarp-target\release\bundle\nsis\`.

---

## Scripts

```bash
yarn dev          # tauri dev: Vite + backend Rust en modo debug
yarn dev:web      # solo Vite (UI en el navegador; el IPC no funciona fuera de Tauri)
yarn build        # tauri build: build:web + backend release + instalador NSIS firmado
yarn build:web    # vue-tsc + vite build → dist/
yarn build:dir    # tauri build --no-bundle (binario release sin instalador, testing rápido)
yarn tauri        # CLI de Tauri (ej: yarn tauri icon, yarn tauri info)
yarn typecheck    # vue-tsc --noEmit
yarn lint         # eslint --fix
yarn lint:check   # eslint en modo verificación (CI, sin --fix, --max-warnings=0)
yarn format       # prettier --write
yarn format:check # prettier --check (CI)
yarn polish       # format + lint + typecheck (todo en uno)
yarn reset        # borra el store del launcher (%APPDATA%\es.sarp.launcher\*.json)
```

Backend (desde `backend/`):

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
cargo test -- --ignored   # contrato contra la API en DEV_API_URL + red (CDN, query al server)
```

---

## Reglas de Calidad de Código

### Regla obligatoria al final de cada cambio

**Después de cualquier modificación de código del frontend (componente, composable, store, config), correr siempre `yarn polish` antes de cerrar el cambio.**

`yarn polish` ejecuta en orden:

1. `prettier --write` — formato consistente.
2. `eslint --fix` — autocorrige + falla en errores.
3. `vue-tsc --noEmit` — type-check estricto (incluye `.vue`).

**Después de cualquier modificación en `backend/`**, correr en esa carpeta `cargo fmt`, `cargo clippy --all-targets -- -D warnings` y `cargo test`.

Si alguno falla, **el cambio no está terminado**. Hay que arreglar lo que reporta antes de avanzar al próximo punto.

### Configuración estricta

- **Prettier** ([.prettierrc](.prettierrc)): `semi: false`, `singleQuote: false`, `printWidth: 100`, `trailingComma: "all"`, `arrowParens: "always"`, `endOfLine: "lf"`.
- **ESLint flat config** ([eslint.config.mjs](eslint.config.mjs)) — ESLint 9 con flat config, no `.eslintrc`.
  - `eqeqeq: "error"` (siempre `===`).
  - `prefer-const`, `no-var` como `error`.
  - `no-console: "warn"` (permite `warn`, `error`, `info`).
  - `@typescript-eslint/no-unused-vars` como `warn`, ignora variables con prefijo `_`.
  - `vue/html-self-closing: "always"` para componentes y void elements.
- **TypeScript**: `strict: true`. `vue-tsc` valida `.vue`. El frontend no tiene tipos de Node (`types: ["vite/client"]`): todo acceso al sistema pasa por el backend.
- **Rust**: `rustfmt` por defecto, `clippy` sin warnings.

### Naming — corto, claro, sin redundancia

**Regla**: el nombre dice **qué** es, no **cómo** funciona ni todos los detalles. Si necesita una frase, está mal. Si lo entiende un dev nuevo en 1 segundo, está bien.

| ✅ Sí                | ❌ No                                                     |
| -------------------- | --------------------------------------------------------- |
| `useProgressBar.ts`  | `useAsymptoticProgress.ts`                                |
| `LoadingPage.vue`    | `InitialLoadingScreenWithProgress.vue`                    |
| `ProgressBar.vue`    | `LinearProgressBarWithGlow.vue`                           |
| `LogoMark.vue`       | `AnimatedLogoWithShimmerEffect.vue`                       |
| `paths.rs`           | `game_installation_path_helpers.rs`                       |
| `downloader.rs`      | `asset_downloader_with_progress_tracking.rs`              |
| `formatBytes(n)`     | `convertNumberOfBytesToHumanReadableString(n)`            |
| `getUser(id)`        | `retrieveUserFromDatabaseById(id)`                        |
| `extract(zip, dest)` | `extractZipArchiveContentsToDestinationFolder(zip, dest)` |

**Pautas concretas:**

- **Componentes Vue**: `PascalCase`, sustantivos cortos (`LogoMark`, no `LauncherBrandLogoMark`).
- **Composables**: `useNombre` — el nombre es el **qué** (`useProgressBar`), no el **cómo** (`useAsymptoticProgressAnimation`).
- **Funciones**: verbos cortos. `download()` mejor que `performAssetDownload()`. `extract()` mejor que `extractZipFileContents()`.
- **Variables**: el contexto ya da la mitad del significado. Dentro de `function downloadFile() { ... }` la variable se llama `progress`, no `downloadProgressPercentage`.
- **Detalles del cómo van adentro** del archivo (en JSDoc / comentario corto si hace falta), no en el nombre.
- **Si el nombre se repite con la carpeta, simplificar**: `composables/useProgressBar.ts` → función `useProgressBar`. No `composables/progressBar/useProgressBarComposable.ts`.
- **Excepción**: si hay dos cosas similares en el mismo scope, sí distinguilas (`getUserById` vs `getUserByEmail`). Pero si solo hay una, `getUser` alcanza.
- **Rust**: mismas reglas con las convenciones del lenguaje (`snake_case` para módulos, funciones y comandos; `PascalCase` para tipos).

### Convenciones de imports

- Usar siempre el alias `@/` para `frontend/*`.
- Imports de tipos: `import type { ... }` cuando solo se usan como tipo.

---

## Convenciones del Proyecto

### Estilos: Tailwind primero, siempre

**Regla obligatoria**: usamos Tailwind para todo. **Nunca** se abre un `<style>` o un nuevo `.css` si la tarea se puede resolver con clases utility. Antes de pensar "necesito CSS custom", consultar la skill [.agents/skills/tailwind-css-patterns/](.agents/skills/tailwind-css-patterns/) — casi siempre Tailwind ya tiene la solución.

**Por qué**: consistencia visual entre componentes, sin estilos huérfanos, sin nombres de clase que envejecen, scope automático, y el bundle final es óptimo (PurgeCSS).

**Cuándo SÍ se prefiere CSS custom (`<style scoped>`)** — excepciones legítimas:

- **Animaciones**: `@keyframes`, transiciones complejas, secuencias multi-step. CSS custom es **mejor** que Tailwind para esto — más expresivo, más controlable, mejor performance que `transition-*` para casos no triviales.
- `mask-image`, filtros complejos, propiedades que Tailwind no expresa bien sin arbitrary values gigantes.
- Reglas globales (`html`, `body`, selectores universales) → solo en [frontend/style.css](frontend/style.css).

**Para todo lo demás, antes de abrir un `<style>`, agotar estas opciones**:

1. ¿Hay una clase utility de Tailwind directa? → usarla.
2. ¿Hay un arbitrary value razonable? (`bg-[#1e293b]`, `mt-[42px]`) → usar.
3. ¿Hay una variante de Tailwind que aplique? (`hover:`, `dark:`, `motion-safe:`, `[&_img]:`) → usar.
4. ¿Lo resuelve un plugin de Tailwind ya instalado? → usar.
5. **Solo si todo lo anterior falla** (y no es una animación), abrir `<style scoped>` con la mínima cantidad de CSS posible.

**Nunca** crear nuevos archivos `.css` por componente / feature. El único `.css` del proyecto es [frontend/style.css](frontend/style.css) (Tailwind + reglas globales), y permanece pequeño.

### IPC frontend ↔ backend

- **Comandos**: funciones Rust con `#[tauri::command]` (nombre `snake_case`, ej. `install_cache`), registradas en el `invoke_handler` de `backend/src/lib.rs`.
- **Bridge tipado**: el frontend **nunca** llama `invoke`/`listen` sueltos — todo pasa por el objeto `launcher` de [frontend/api/launcher.ts](frontend/api/launcher.ts) (métodos `camelCase`, ej. `launcher.installCache()`). Los argumentos van con claves `camelCase` (`{ targetDir }`); Tauri los mapea a los parámetros `snake_case` de Rust.
- **Eventos** backend → frontend: el backend emite `dominio:accion` (ej. `cache:install:progress`) y el frontend se suscribe con `launcher.onXxx(callback)`, que devuelve un unsubscribe **síncrono** (llamarlo siempre en `onUnmounted` o en el `finally` del flujo).
- **Tipos**: los payloads viven en [frontend/types/](frontend/types/) y deben coincidir con los structs de Rust (`#[serde(rename_all = "camelCase")]`). Si cambia uno, cambiar el otro en el mismo commit.
- **Errores**: un comando que falla rechaza la promesa con el mensaje del `Err` de Rust (string). Los flujos que el usuario debe ver devuelven `{ ok, error? }` en vez de rechazar.

### Otras convenciones

- **Path alias**: `@/*` → `frontend/*` (configurado en `tsconfig.json` y `vite.config.ts`).
- **Componentes Vue**: `<script setup lang="ts">` siempre.
- **Páginas full-screen** van en `frontend/pages/`. Componentes reutilizables en `frontend/components/<feature>/`.
- **Lógica reutilizable** → composables en `frontend/composables/`.
- **Estado global** → Pinia stores en `frontend/stores/`.
- **Datos remotos** → módulos en `frontend/api/`, siempre vía comandos del backend (`launcher.fetchGlobalMetrics()`, `launcher.fetchEvents()`, `launcher.checkCache()`...). La CSP (`connect-src`) no permite ningún host externo.

---

## Archivos Clave

| Archivo                                                      | Propósito                                            |
| ------------------------------------------------------------ | ---------------------------------------------------- |
| [backend/src/lib.rs](backend/src/lib.rs)                     | Builder de Tauri: plugins, estado, comandos, eventos |
| [backend/tauri.conf.json](backend/tauri.conf.json)           | Ventana, bundle NSIS, updater (endpoint + pubkey)    |
| [backend/capabilities/](backend/capabilities/)               | Permisos de la ventana sobre core y plugins          |
| [backend/build.rs](backend/build.rs)                         | Variables del `.env` horneadas en el binario         |
| [backend/src/net/api.rs](backend/src/net/api.rs)             | Cliente de la API (envelope, `X-Launcher-Key`)       |
| [backend/src/install/cache.rs](backend/src/install/cache.rs) | Sincronización del caché (zip + archivo por archivo) |
| [frontend/api/launcher.ts](frontend/api/launcher.ts)         | Bridge IPC tipado (comandos + eventos)               |
| [frontend/types/](frontend/types/)                           | Payloads compartidos con Rust                        |
| [vite.config.ts](vite.config.ts)                             | Plugin Vue, alias, dev server para Tauri             |
| [frontend/main.ts](frontend/main.ts)                         | Bootstrap Vue + Pinia + PrimeVue + nativeFeel        |
| [frontend/style.css](frontend/style.css)                     | Tailwind + reglas globales                           |
