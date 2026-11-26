//! Launching SA-MP and tracking whether `gta_sa.exe` is running.

use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Manager};

use crate::app::events::{self, GAME_STATUS_CHANGED};
use crate::game::paths::game_dir;
use crate::net::query;
use crate::Ctx;

const TARGET_PROCESS: &str = "gta_sa.exe";
const POLL_INTERVAL: Duration = Duration::from_millis(2500);
/// After a launch, how long `gta_sa.exe` may take to show up before we give up.
const SPAWN_GRACE: Duration = Duration::from_secs(30);

const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct GameStatus {
    pub running: bool,
    pub pid: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GameLaunchResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl GameLaunchResult {
    fn error(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(message.into()),
        }
    }
}

/// PID of the first process whose image name matches (case-insensitive).
pub fn find_process(image: &str) -> Option<u32> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut found = None;
        let mut more = Process32FirstW(snapshot, &mut entry).is_ok();
        while more {
            let len = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
            if name.eq_ignore_ascii_case(image) {
                found = Some(entry.th32ProcessID);
                break;
            }
            more = Process32NextW(snapshot, &mut entry).is_ok();
        }
        let _ = CloseHandle(snapshot);
        found
    }
}

pub fn find_game_process() -> Option<u32> {
    find_process(TARGET_PROCESS)
}

#[derive(Default)]
struct WatchState {
    pid: Option<u32>,
    poller: Option<JoinHandle<()>>,
}

/// Running-game state plus the background poller.
#[derive(Default)]
pub struct GameWatch(Mutex<WatchState>);

impl GameWatch {
    fn lock(&self) -> std::sync::MutexGuard<'_, WatchState> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn status(&self) -> GameStatus {
        let pid = self.lock().pid;
        GameStatus {
            running: pid.is_some(),
            pid,
        }
    }
}

fn set_active_pid(app: &AppHandle, pid: Option<u32>) {
    let ctx = app.state::<Ctx>();
    {
        let mut state = ctx.game.lock();
        if state.pid == pid {
            return;
        }
        state.pid = pid;
    }
    events::emit(app, GAME_STATUS_CHANGED, ctx.game.status());
}

/// (Re)starts the poller. `launched_at` is set right after a launch: until
/// the game is first seen, its absence is not treated as "exited" for up to
/// `SPAWN_GRACE` (samp.exe needs a moment to start gta_sa.exe).
fn start_polling(app: &AppHandle, launched_at: Option<Instant>) {
    let handle = app.clone();
    let task = tauri::async_runtime::spawn(async move {
        let mut seen = launched_at.is_none();
        loop {
            tokio::time::sleep(POLL_INTERVAL).await;
            match find_game_process() {
                Some(pid) => {
                    seen = true;
                    set_active_pid(&handle, Some(pid));
                }
                None => {
                    let in_grace = !seen && launched_at.is_some_and(|t| t.elapsed() < SPAWN_GRACE);
                    if !in_grace {
                        set_active_pid(&handle, None);
                        break;
                    }
                }
            }
        }
    });
    let previous = app.state::<Ctx>().game.lock().poller.replace(task);
    if let Some(previous) = previous {
        previous.abort();
    }
}

pub fn launch(app: &AppHandle, host: &str, port: u16) -> GameLaunchResult {
    // Same rules as the server ping: a dotted IPv4 and a real port, so
    // nothing but `a.b.c.d:port` ever reaches samp.exe's command line.
    let Some(server) = query::endpoint(host, f64::from(port)) else {
        return GameLaunchResult::error("invalid host");
    };
    let ctx = app.state::<Ctx>();
    if ctx.game.status().running {
        return GameLaunchResult::error("already running");
    }
    // The game reads and writes the cache folder: never both at once.
    if ctx.cache.is_syncing() {
        return GameLaunchResult::error("Espera a que termine de actualizarse el caché.");
    }
    // Mod files are being written, or a required one could not be installed.
    if ctx.mods.is_busy() {
        return GameLaunchResult::error("Instalando mods…");
    }
    if ctx.mods.is_blocking() {
        return GameLaunchResult::error("Falta un mod esencial.");
    }
    let Some(dir) = game_dir(&ctx.store) else {
        return GameLaunchResult::error("Elige primero la carpeta de GTA San Andreas");
    };
    let samp = dir.join("samp.exe");
    if !samp.exists() {
        return GameLaunchResult::error("samp.exe no está instalado");
    }

    let spawned = Command::new(&samp)
        .arg(&server)
        .current_dir(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .spawn();
    if let Err(error) = spawned {
        log::error!("failed to start {}: {error}", samp.display());
        return GameLaunchResult::error(error.to_string());
    }
    // Dropping the Child neither waits for nor kills samp.exe.

    start_polling(app, Some(Instant::now()));
    GameLaunchResult {
        ok: true,
        error: None,
    }
}

/// At startup: pick up a game that was already running.
pub fn detect_running(app: &AppHandle) {
    if let Some(pid) = find_game_process() {
        set_active_pid(app, Some(pid));
        start_polling(app, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn finds_our_own_process() {
        let exe = std::env::current_exe().unwrap();
        // Upper-cased on purpose: matching is case-insensitive.
        let name = exe.file_name().unwrap().to_string_lossy().to_uppercase();
        assert!(find_process(&name).is_some());
        assert_eq!(find_process("definitely-not-running-sarp.exe"), None);
    }

    #[test]
    fn payload_shapes() {
        assert_eq!(
            serde_json::to_value(GameStatus {
                running: false,
                pid: None
            })
            .unwrap(),
            json!({ "running": false, "pid": null })
        );
        assert_eq!(
            serde_json::to_value(GameStatus {
                running: true,
                pid: Some(42)
            })
            .unwrap(),
            json!({ "running": true, "pid": 42 })
        );
        assert_eq!(
            serde_json::to_value(GameLaunchResult::error("already running")).unwrap(),
            json!({ "ok": false, "error": "already running" })
        );
        assert_eq!(
            serde_json::to_value(GameLaunchResult {
                ok: true,
                error: None
            })
            .unwrap(),
            json!({ "ok": true })
        );
    }
}
