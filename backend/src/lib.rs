//! San Andreas Roleplay - SA:MP Launcher (Tauri backend).

mod app;
mod commands;
mod config;
mod game;
mod install;
mod net;
mod store;

use tauri::{AppHandle, Manager, RunEvent};
use tauri_plugin_log::{RotationStrategy, Target, TargetKind, TimezoneStrategy};

use crate::app::{updater, window};
use crate::config::IS_DEV;
use crate::game::launcher;
use crate::install::cache;
use crate::net::{api, http, manifest};

/// Everything the commands share.
pub struct Ctx {
    /// SA-MP / GTA downloads from the CDN URLs in the manifest.
    pub http: reqwest::Client,
    /// Our API (manifest, metrics, events, cache list) and the cache files.
    pub api: api::Api,
    pub store: store::Store,
    pub manifest: manifest::ManifestCache,
    pub cache: cache::CacheState,
    pub game: launcher::GameWatch,
    pub window: window::WindowPersist,
    pub updater: updater::UpdaterState,
}

fn log_plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    let mut builder = tauri_plugin_log::Builder::new()
        .clear_targets()
        .target(Target::new(TargetKind::LogDir {
            file_name: Some("launcher".into()),
        }))
        .level(if IS_DEV {
            log::LevelFilter::Debug
        } else {
            log::LevelFilter::Info
        })
        // Transport crates are chatty at debug level.
        .level_for("hyper", log::LevelFilter::Warn)
        .level_for("hyper_util", log::LevelFilter::Warn)
        .level_for("reqwest", log::LevelFilter::Warn)
        .level_for("rustls", log::LevelFilter::Warn)
        .level_for("h2", log::LevelFilter::Warn)
        .max_file_size(2_000_000)
        .rotation_strategy(RotationStrategy::KeepOne)
        .timezone_strategy(TimezoneStrategy::UseLocal);
    if IS_DEV {
        builder = builder.target(Target::new(TargetKind::Stdout));
    }
    builder.build()
}

fn setup(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("panic: {info}");
        previous(info);
    }));

    let version = app.package_info().version.to_string();
    log::info!("SARP launcher {version} starting");
    let store_path = app.path().app_config_dir()?.join(store::FILE_NAME);
    let api_url = config::api_url();
    log::info!("API at {api_url}");
    app.manage(Ctx {
        http: http::client(&version),
        api: api::Api::new(http::strict_client(&version), api_url),
        store: store::Store::open(store_path),
        manifest: manifest::ManifestCache::default(),
        cache: cache::CacheState::default(),
        game: launcher::GameWatch::default(),
        window: window::WindowPersist::default(),
        updater: updater::UpdaterState::default(),
    });

    window::create(app)?;
    launcher::detect_running(app);

    // Fire-and-forget: never hold up the window for filesystem cleanup.
    let gta_temp = app.state::<Ctx>().store.get().gta_install_temp_dir;
    let app_name = app.package_info().name.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let temp = std::env::temp_dir();
        // Resumable downloads: the GTA rip's folder, and the cache package's,
        // which has its own (longer) age limit.
        let package = cache::package::dir(&temp);
        let mut keep = vec![package.as_path()];
        if let Some(gta) = gta_temp.as_deref() {
            keep.push(std::path::Path::new(gta));
        }
        install::fs::sweep_temp(&temp, &keep, &app_name);
        cache::package::sweep(&temp);
    });
    Ok(())
}

pub fn run() {
    // reqwest is built without a bundled crypto provider; use ring (already
    // pulled in by the updater plugin) for every TLS client in the process.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let app = tauri::Builder::default()
        // Must be the first plugin. A second launch hands over to the running
        // launcher (its window comes to the front) and exits before anything
        // else starts: two processes would run two cache syncs (each one
        // deleting the other's temp files) and two updaters.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            window::focus(app)
        }))
        .plugin(log_plugin())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            commands::health_check,
            commands::check_cache,
            commands::launch_game,
            commands::get_game_status,
            commands::ping_server,
            commands::fetch_manifest,
            commands::fetch_global_metrics,
            commands::fetch_events,
            commands::verify_samp,
            commands::install_samp,
            commands::samp_install_requires_elevation,
            commands::pick_game_dir,
            commands::detect_game_dir,
            commands::find_game_dirs,
            commands::set_game_dir,
            commands::pick_empty_install_dir,
            commands::detect_gta_mods,
            commands::install_gta,
            commands::install_cache,
            commands::cancel_cache,
            commands::window_minimize,
            commands::window_close,
            commands::window_toggle_maximize,
            commands::window_toggle_fullscreen,
            commands::get_window_state,
            commands::open_external,
            commands::get_app_version,
            commands::updater_install,
        ])
        .setup(|app| setup(app.handle()))
        .build(tauri::generate_context!());

    let app = match app {
        Ok(app) => app,
        Err(error) => {
            log::error!("failed to start: {error}");
            if IS_DEV {
                eprintln!("failed to start: {error}");
            }
            std::process::exit(1);
        }
    };

    app.run(|handle, event| {
        if let RunEvent::Exit = event {
            updater::apply_on_exit(handle);
        }
    });
}
