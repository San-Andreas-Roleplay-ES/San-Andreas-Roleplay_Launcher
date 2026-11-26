//! Tauri commands: the IPC surface the frontend `invoke`s. Thin wrappers;
//! results are returned as-is (errors encoded as `{ ok: false, error }`),
//! except the `fetch_*` data commands, which reject with a message.

use serde_json::Value;
use tauri::{AppHandle, Manager, WebviewWindow};

use crate::app::window::{self, WindowState};
use crate::app::{external, updater};
use crate::game::detect::{self, Candidate, Detection};
use crate::game::health::{self, CacheHealth, HealthCheckPayload};
use crate::game::launcher::{self, GameLaunchResult, GameStatus};
use crate::game::paths::{self, game_dir, PickGameDirResult};
use crate::install::cache::{self, CacheInstallResult};
use crate::install::gta::{self, GtaInstallResult};
use crate::install::samp::{self, InstallResult};
use crate::install::verify::{self, SampVerificationResult};
use crate::mods::{self, ModsHealth, ModsSyncResult, Request};
use crate::net::manifest::{self, LauncherManifest};
use crate::net::query::{self, PingResult};
use crate::rwguard::{self, RwGuardActionResult, RwGuardStatus};
use crate::Ctx;

// ---- game ----

#[tauri::command]
pub async fn health_check(app: AppHandle) -> HealthCheckPayload {
    health::run(&app)
}

#[tauri::command]
pub async fn check_cache(app: AppHandle) -> CacheHealth {
    health::cache(&app).await
}

#[tauri::command]
pub async fn launch_game(app: AppHandle, host: String, port: u16) -> GameLaunchResult {
    launcher::launch(&app, &host, port)
}

#[tauri::command]
pub fn get_game_status(app: AppHandle) -> GameStatus {
    app.state::<Ctx>().game.status()
}

#[tauri::command]
pub async fn pick_game_dir(app: AppHandle) -> PickGameDirResult {
    paths::pick_game_dir(&app).await
}

/// Startup: without a valid folder, finds the installs on this PC (read-only)
/// and adopts the one SA-MP uses, or the only one found.
#[tauri::command]
pub async fn detect_game_dir(app: AppHandle) -> Detection {
    detect::detect(&app).await
}

/// The installs found on this PC, for "Cambiar ubicación". Read-only.
#[tauri::command]
pub async fn find_game_dirs(app: AppHandle) -> Vec<Candidate> {
    detect::list(&app).await
}

/// Uses one of the detected installs (validated like a picked folder).
#[tauri::command]
pub async fn set_game_dir(app: AppHandle, path: String) -> PickGameDirResult {
    paths::set_game_dir(&app, &path).await
}

#[tauri::command]
pub async fn pick_empty_install_dir(app: AppHandle) -> PickGameDirResult {
    paths::pick_empty_install_dir(&app).await
}

// ---- install ----

#[tauri::command]
pub async fn verify_samp(app: AppHandle) -> SampVerificationResult {
    let ctx = app.state::<Ctx>();
    let Some(dir) = game_dir(&ctx.store) else {
        return SampVerificationResult::failed(verify::NO_GAME_DIR);
    };
    // The live manifest, or the last saved one while the API is down.
    match ctx.manifest.ensure_saved(&ctx.api, &ctx.store).await {
        Ok(manifest) => verify::verify(Some(&dir), &manifest).await,
        Err(error) => {
            log::warn!("SA-MP not verified, no manifest: {error}");
            verify::unverified(Some(&dir))
        }
    }
}

#[tauri::command]
pub async fn install_samp(app: AppHandle) -> InstallResult {
    samp::install(&app).await
}

#[tauri::command]
pub async fn samp_install_requires_elevation(app: AppHandle) -> bool {
    samp::requires_elevation(&app)
}

#[tauri::command]
pub async fn install_gta(app: AppHandle, target_dir: Option<String>) -> GtaInstallResult {
    gta::install(&app, target_dir).await
}

#[tauri::command]
pub async fn install_cache(app: AppHandle) -> CacheInstallResult {
    cache::install(&app).await
}

/// Stops the running cache sync ("Omitir"). `false` when none was running.
/// `install_cache` then resolves with `cancelled: true`.
#[tauri::command]
pub fn cancel_cache(app: AppHandle) -> bool {
    cache::cancel(&app)
}

// ---- rwguard ----

/// Current RWGuard protection summary, active/blocked threats and recent
/// validation history for the server cache folder.
#[tauri::command]
pub fn rwguard_status(app: AppHandle) -> RwGuardStatus {
    rwguard::status(&app)
}

/// Scans the server cache folder with RWGuard. Unchanged models already in
/// persistent history are skipped unless `force` is `true`.
#[tauri::command]
pub async fn rwguard_scan(app: AppHandle, force: Option<bool>) -> RwGuardStatus {
    rwguard::scan_cache(&app, force.unwrap_or(false)).await
}

/// Repairs one flagged model (or all active threats when `file_name` is
/// omitted) using RWGuard's native RenderWare truncation.
#[tauri::command]
pub async fn rwguard_heal(app: AppHandle, file_name: Option<String>) -> RwGuardActionResult {
    rwguard::heal(&app, file_name).await
}

/// Deletes one flagged model (or all active threats) from the cache folder and
/// clears matching blocked-download notices.
#[tauri::command]
pub async fn rwguard_delete(app: AppHandle, file_name: Option<String>) -> RwGuardActionResult {
    rwguard::delete_threats(&app, file_name).await
}

// ---- mods ----

/// The Mods row and panel: the catalog's mods and their state in the game
/// folder. `force`: fetch the catalog again. Never rejects.
#[tauri::command]
pub async fn check_mods(app: AppHandle, force: Option<bool>) -> ModsHealth {
    mods::check(&app, force.unwrap_or(false)).await
}

/// The automatic flow: required mods, defaults, updates, repairs. `retry`:
/// also the mods that failed this session.
#[tauri::command]
pub async fn sync_mods(app: AppHandle, retry: Option<bool>) -> ModsSyncResult {
    let retry = retry.unwrap_or(false);
    mods::run(&app, Request::Auto { retry }).await
}

#[tauri::command]
pub async fn set_mod_enabled(app: AppHandle, id: String, enabled: bool) -> ModsSyncResult {
    mods::run(&app, Request::Enable { id, enabled }).await
}

#[tauri::command]
pub async fn set_mod_variant(app: AppHandle, id: String, variant: String) -> ModsSyncResult {
    mods::run(&app, Request::Variant { id, variant }).await
}

/// "Instalar versión recomendada" over an install made by other means.
#[tauri::command]
pub async fn install_recommended_mod(app: AppHandle, id: String) -> ModsSyncResult {
    mods::run(&app, Request::Recommended { id }).await
}

/// Renames the mod's `replaces` out of the way (reversible).
#[tauri::command]
pub async fn disable_mod_conflicts(app: AppHandle, id: String) -> ModsSyncResult {
    mods::run(&app, Request::Conflicts { id }).await
}

/// Stops the running mods run. `false` when none was running.
#[tauri::command]
pub fn cancel_mods(app: AppHandle) -> bool {
    mods::cancel(&app)
}

// ---- net ----

#[tauri::command]
pub async fn fetch_global_metrics(app: AppHandle) -> Result<Value, String> {
    app.state::<Ctx>().api.global_metrics().await
}

#[tauri::command]
pub async fn fetch_events(app: AppHandle) -> Result<Value, String> {
    app.state::<Ctx>().api.events().await
}

/// Args stay untyped so bad input gets the old `invalid args` result instead
/// of a rejected invoke.
#[tauri::command]
pub async fn ping_server(ip: Option<Value>, port: Option<Value>) -> PingResult {
    match (
        ip.as_ref().and_then(Value::as_str),
        port.as_ref().and_then(Value::as_f64),
    ) {
        (Some(ip), Some(port)) => query::ping(ip, port).await,
        _ => PingResult::failed("invalid args"),
    }
}

#[tauri::command]
pub async fn fetch_manifest(
    app: AppHandle,
    force: Option<bool>,
) -> Result<LauncherManifest, String> {
    let ctx = app.state::<Ctx>();
    if force.unwrap_or(false) {
        let fresh = ctx.manifest.refresh(&ctx.api).await;
        if let Ok(manifest) = &fresh {
            manifest::save(&ctx.store, manifest);
        }
        fresh
    } else {
        ctx.manifest.ensure_saved(&ctx.api, &ctx.store).await
    }
}

// ---- window / app ----

#[tauri::command]
pub fn window_minimize(window: WebviewWindow) {
    let _ = window.minimize();
}

#[tauri::command]
pub fn window_close(window: WebviewWindow) {
    let _ = window.close();
}

#[tauri::command]
pub fn window_toggle_maximize(app: AppHandle, window: WebviewWindow) {
    window::toggle_maximize(&app, &window);
}

#[tauri::command]
pub fn window_toggle_fullscreen(app: AppHandle, window: WebviewWindow) {
    window::toggle_fullscreen(&app, &window);
}

#[tauri::command]
pub fn get_window_state(window: WebviewWindow) -> WindowState {
    window::state(&window)
}

#[tauri::command]
pub fn open_external(app: AppHandle, url: String) {
    external::open(&app, &url);
}

#[tauri::command]
pub fn get_app_version(app: AppHandle) -> String {
    app.package_info().version.to_string()
}

#[tauri::command]
pub fn updater_install(app: AppHandle) {
    updater::install_now(&app);
}
