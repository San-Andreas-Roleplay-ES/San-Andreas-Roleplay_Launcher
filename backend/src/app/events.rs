//! Event names pushed to the webview. They keep the old IPC channel names so
//! the frontend listens on the same strings.

use serde::Serialize;
use tauri::{AppHandle, Emitter};

pub const GAME_STATUS_CHANGED: &str = "game:status:changed";
pub const WINDOW_STATE_CHANGED: &str = "window:state:changed";
pub const SAMP_INSTALL_PROGRESS: &str = "samp:install:progress";
pub const GTA_INSTALL_PROGRESS: &str = "gta:install:progress";
pub const CACHE_INSTALL_PROGRESS: &str = "cache:install:progress";
pub const MODS_INSTALL_PROGRESS: &str = "mods:install:progress";
pub const RWGUARD_PROGRESS: &str = "rwguard:progress";
pub const RWGUARD_STATUS_CHANGED: &str = "rwguard:status:changed";
pub const UPDATER_AVAILABLE: &str = "updater:available";
pub const UPDATER_PROGRESS: &str = "updater:progress";
pub const UPDATER_DOWNLOADED: &str = "updater:downloaded";
pub const UPDATER_ERROR: &str = "updater:error";

/// Emits to every webview. Failures (e.g. the window is gone) are only logged.
pub fn emit<S: Serialize + Clone>(app: &AppHandle, event: &str, payload: S) {
    if let Err(error) = app.emit(event, payload) {
        log::warn!("failed to emit {event}: {error}");
    }
}
