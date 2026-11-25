//! Self-update from the CDN (`latest.json`, minisign-verified by the plugin).
//!
//! Release builds check once at startup and download in the background,
//! reporting through `updater:*` events. "Reiniciar para aplicar" calls
//! [`install_now`] (installer runs with a progress bar and relaunches the
//! app). If the user just closes the launcher with an update downloaded,
//! [`apply_on_exit`] runs the installer without relaunching.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};
use time::format_description::well_known::Rfc3339;

use crate::app::events::{
    self, UPDATER_AVAILABLE, UPDATER_DOWNLOADED, UPDATER_ERROR, UPDATER_PROGRESS,
};
use crate::app::window;
use crate::config::IS_DEV;
use crate::Ctx;

const PROGRESS_THROTTLE: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdaterInfo {
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_date: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdaterProgress {
    pub percent: f64,
    pub bytes_per_second: u64,
    pub transferred: u64,
    pub total: u64,
}

#[derive(Default)]
pub struct UpdaterState {
    started: AtomicBool,
    pending: Mutex<Option<(Update, Vec<u8>)>>,
}

impl UpdaterState {
    fn take(&self) -> Option<(Update, Vec<u8>)> {
        self.pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
    }
}

fn info(update: &Update) -> UpdaterInfo {
    UpdaterInfo {
        version: update.version.clone(),
        release_date: update.date.and_then(|d| d.format(&Rfc3339).ok()),
    }
}

/// Starts the background check once (release builds only).
pub fn start(app: &AppHandle) {
    if IS_DEV {
        return;
    }
    if app
        .state::<Ctx>()
        .updater
        .started
        .swap(true, Ordering::SeqCst)
    {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(error) = check_and_download(&app).await {
            // The full story is in the log; the UI decides whether to show it.
            log::warn!("updater: {error}");
            events::emit(&app, UPDATER_ERROR, error);
        }
    });
}

async fn check_and_download(app: &AppHandle) -> Result<(), String> {
    let save_app = app.clone();
    let updater = app
        .updater_builder()
        // The Windows installer path ends in process::exit, skipping our
        // close handlers: save the window placement first.
        .on_before_exit(move || window::save_now(&save_app))
        .build()
        .map_err(|e| e.to_string())?;
    let Some(update) = updater.check().await.map_err(|e| e.to_string())? else {
        log::info!("updater: up to date");
        return Ok(());
    };
    log::info!("updater: {} available, downloading", update.version);
    events::emit(app, UPDATER_AVAILABLE, info(&update));

    let started = Instant::now();
    let mut transferred = 0u64;
    let mut last_emit: Option<Instant> = None;
    let bytes = update
        .download(
            |chunk, total| {
                transferred += chunk as u64;
                let total = total.unwrap_or(0);
                let now = Instant::now();
                let done = total > 0 && transferred >= total;
                if !done && last_emit.is_some_and(|t| now.duration_since(t) < PROGRESS_THROTTLE) {
                    return;
                }
                last_emit = Some(now);
                let seconds = started.elapsed().as_secs_f64().max(0.001);
                events::emit(
                    app,
                    UPDATER_PROGRESS,
                    UpdaterProgress {
                        percent: crate::install::progress::percent(transferred, total),
                        bytes_per_second: (transferred as f64 / seconds).round() as u64,
                        transferred,
                        total,
                    },
                );
            },
            || {},
        )
        .await
        .map_err(|e| e.to_string())?;

    log::info!("updater: {} downloaded", update.version);
    let downloaded = info(&update);
    *app.state::<Ctx>()
        .updater
        .pending
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = Some((update, bytes));
    events::emit(app, UPDATER_DOWNLOADED, downloaded);
    Ok(())
}

/// "Reiniciar para aplicar": runs the installer (progress bar, no prompts)
/// and relaunches the new version. On Windows this does not return.
pub fn install_now(app: &AppHandle) {
    let Some((update, bytes)) = app.state::<Ctx>().updater.take() else {
        log::warn!("updater: install requested but nothing is downloaded");
        return;
    };
    // The window placement is saved by the `on_before_exit` hook.
    match update.install(&bytes) {
        Ok(()) => app.restart(),
        Err(error) => {
            log::error!("updater: install failed: {error}");
            events::emit(app, UPDATER_ERROR, error.to_string());
        }
    }
}

/// On a normal close with an update waiting: install it without relaunching.
pub fn apply_on_exit(app: &AppHandle) {
    let Some((update, bytes)) = app.try_state::<Ctx>().and_then(|ctx| ctx.updater.take()) else {
        return;
    };
    log::info!("updater: applying {} on exit", update.version);
    if let Err(error) = update.restart_after_install(false).install(&bytes) {
        log::error!("updater: install on exit failed: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn payload_shapes() {
        assert_eq!(
            serde_json::to_value(UpdaterInfo {
                version: "1.1.0".into(),
                release_date: None
            })
            .unwrap(),
            json!({ "version": "1.1.0" })
        );
        assert_eq!(
            serde_json::to_value(UpdaterProgress {
                percent: 50.0,
                bytes_per_second: 1000,
                transferred: 5,
                total: 10
            })
            .unwrap(),
            json!({ "percent": 50.0, "bytesPerSecond": 1000, "transferred": 5, "total": 10 })
        );
    }

    #[test]
    fn release_date_is_rfc3339() {
        let date = time::OffsetDateTime::UNIX_EPOCH + time::Duration::days(1);
        assert_eq!(date.format(&Rfc3339).unwrap(), "1970-01-02T00:00:00Z");
    }
}
