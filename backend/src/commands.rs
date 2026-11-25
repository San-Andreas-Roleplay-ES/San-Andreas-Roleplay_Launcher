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
use crate::game::mods::{self, DetectedMod};
use crate::game::paths::{self, game_dir, PickGameDirResult};
use crate::install::cache::{self, CacheInstallResult};
use crate::install::gta::{self, GtaInstallResult};
use crate::install::samp::{self, InstallResult};
use crate::install::verify::{self, SampVerificationResult};
use crate::net::manifest::{self, LauncherManifest};
use crate::net::query::{self, PingResult, ScoreboardSnapshot};
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

#[tauri::command]
pub async fn detect_gta_mods(app: AppHandle) -> Vec<DetectedMod> {
    mods::detect(game_dir(&app.state::<Ctx>().store).as_deref())
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
pub async fn fetch_scoreboard(ip: String, port: f64) -> Result<ScoreboardSnapshot, String> {
    query::scoreboard(&ip, port).await
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
