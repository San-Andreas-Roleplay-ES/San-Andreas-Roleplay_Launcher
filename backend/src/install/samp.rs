//! SA-MP install / repair.
//!
//! No SA-MP files at all → run the official NSIS installer silently (gets the
//! full SAMP folder, registry keys...). Anything else → download only the
//! files that fail verification and copy them in.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::app::events::{self, SAMP_INSTALL_PROGRESS};
use crate::game::launcher::find_game_process;
use crate::game::paths::game_dir;
use crate::install::extract::safe_relative_path;
use crate::install::fs::{
    can_write_to_dir, new_temp_dir, remove_zone_identifier, sanitize_filename,
};
use crate::install::powershell::{
    self, Outcome, EXIT_LAUNCH_FAILED, EXIT_NO_PROCESS, EXIT_UAC_CANCELLED,
};
use crate::install::progress::percent;
use crate::install::verify::{verify, FileVerification, Reason};
use crate::net::http::download_verified;
use crate::net::manifest::{LauncherManifest, ManifestFileEntry};
use crate::Ctx;

const INSTALL_TIMEOUT: Duration = Duration::from_secs(120);
const NO_GAME_DIR: &str = "Elige primero la carpeta de GTA San Andreas.";
const UAC_CANCELLED: &str = "Se canceló la solicitud de permisos de administrador.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum InstallPhase {
    Preflight,
    Download,
    Install,
    #[allow(dead_code)]
    Verify,
    Done,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InstallProgress {
    pub phase: InstallPhase,
    pub percent: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstallResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

struct Reporter<'a>(&'a AppHandle);

impl Reporter<'_> {
    fn send(&self, phase: InstallPhase, percent: f64, message: Option<&str>) {
        events::emit(
            self.0,
            SAMP_INSTALL_PROGRESS,
            InstallProgress {
                phase,
                percent,
                message: message.map(String::from),
            },
        );
    }
}

/// Appends `/D=<game dir>` (NSIS target folder; must be last and unquoted)
/// unless the manifest already provides one.
pub fn installer_args(silent_args: &[String], game_dir: &Path) -> Vec<String> {
    let mut args = silent_args.to_vec();
    if !args.iter().any(|a| a.starts_with("/D=")) {
        args.push(format!("/D={}", game_dir.display()));
    }
    args
}

pub fn requires_elevation(app: &AppHandle) -> bool {
    // No folder yet → we can't know. The install attempt fails with a useful
    // error before any UAC prompt would appear.
    match game_dir(&app.state::<Ctx>().store) {
        Some(dir) => !can_write_to_dir(&dir),
        None => false,
    }
}

pub async fn install(app: &AppHandle) -> InstallResult {
    let report = Reporter(app);
    let mut temp: Option<PathBuf> = None;
    let result = run(app, &report, &mut temp).await;
    if let Some(dir) = temp {
        let _ = tokio::fs::remove_dir_all(dir).await;
    }
    match result {
        Ok(()) => {
            report.send(InstallPhase::Done, 100.0, None);
            InstallResult {
                ok: true,
                error: None,
            }
        }
        Err(error) => {
            log::warn!("SA-MP install failed: {error}");
            report.send(InstallPhase::Error, 0.0, Some(&error));
            InstallResult {
                ok: false,
                error: Some(error),
            }
        }
    }
}

async fn run(
    app: &AppHandle,
    report: &Reporter<'_>,
    temp: &mut Option<PathBuf>,
) -> Result<(), String> {
    let ctx = app.state::<Ctx>();
    let Some(dir) = game_dir(&ctx.store) else {
        return Err(NO_GAME_DIR.into());
    };
    report.send(
        InstallPhase::Preflight,
        0.0,
        Some("Verificando estado del juego..."),
    );
    if find_game_process().is_some() {
        return Err("Cierra GTA San Andreas antes de continuar.".into());
    }

    let manifest = ctx.manifest.ensure_saved(&ctx.api, &ctx.store).await?;
    report.send(
        InstallPhase::Preflight,
        0.0,
        Some("Comprobando archivos..."),
    );
    let verification = verify(Some(&dir), &manifest).await;
    if let Some(error) = verification.error {
        return Err(error);
    }
    let failed: Vec<&FileVerification> = verification.files.iter().filter(|f| !f.ok).collect();
    let all_missing = !failed.is_empty()
        && failed.len() == verification.files.len()
        && failed.iter().all(|f| f.reason == Some(Reason::Missing));

    if all_missing {
        full_install(app, report, &manifest, &dir, temp).await
    } else if !failed.is_empty() {
        repair(app, report, &manifest, &failed, &dir, temp).await
    } else {
        Ok(()) // already healthy
    }
}

async fn full_install(
    app: &AppHandle,
    report: &Reporter<'_>,
    manifest: &LauncherManifest,
    dir: &Path,
    temp: &mut Option<PathBuf>,
) -> Result<(), String> {
    let installer = &manifest.samp.installer;
    report.send(
        InstallPhase::Download,
        0.0,
        Some("Descargando instalador..."),
    );
    // Random folder, original file name inside: UAC then shows a recognisable
    // name (sa-mp-0.3.DL-R1-2-MP-install.exe) instead of a random one.
    let staging = new_temp_dir().map_err(|e| e.to_string())?;
    *temp = Some(staging.clone());
    let exe = staging.join(sanitize_filename(&installer.filename));

    let http = &app.state::<Ctx>().http;
    let mut throttle = Throttle::default();
    download_verified(
        http,
        &installer.url,
        &exe,
        installer.size,
        &installer.sha256,
        |received| {
            if throttle.ready(received == installer.size) {
                report.send(
                    InstallPhase::Download,
                    percent(received, installer.size),
                    None,
                );
            }
        },
    )
    .await?;
    remove_zone_identifier(&exe);

    let args = installer_args(&installer.silent_args, dir);
    let elevate = !can_write_to_dir(dir);
    if elevate {
        report.send(
            InstallPhase::Install,
            0.0,
            Some("Acepta el aviso de Windows para instalar en la carpeta protegida."),
        );
    } else {
        report.send(InstallPhase::Install, 0.0, Some("Instalando SA:MP..."));
    }
    let command = powershell::start_process(&exe.to_string_lossy(), &args, Some(dir), elevate);
    match powershell::run(&command, INSTALL_TIMEOUT).await {
        Outcome::Exited(0) => Ok(()),
        Outcome::Exited(EXIT_UAC_CANCELLED) => Err(UAC_CANCELLED.into()),
        Outcome::Exited(EXIT_LAUNCH_FAILED | EXIT_NO_PROCESS) => Err(if elevate {
            "No se pudo iniciar el instalador con permisos de administrador.".into()
        } else {
            "No se pudo iniciar el instalador.".into()
        }),
        Outcome::Exited(code) => Err(format!("El instalador terminó con código {code}.")),
        Outcome::TimedOut => Err("El instalador no respondió a tiempo.".into()),
        Outcome::Failed(error) => Err(error),
    }
}

async fn repair(
    app: &AppHandle,
    report: &Reporter<'_>,
    manifest: &LauncherManifest,
    failed: &[&FileVerification],
    dir: &Path,
    temp: &mut Option<PathBuf>,
) -> Result<(), String> {
    let staging = new_temp_dir().map_err(|e| e.to_string())?;
    *temp = Some(staging.clone());
    report.send(InstallPhase::Download, 0.0, Some("Descargando archivos..."));

    let targets: Vec<&ManifestFileEntry> = failed
        .iter()
        .filter_map(|f| manifest.samp.files.iter().find(|m| m.path == f.path))
        .collect();
    let total: u64 = targets.iter().map(|t| t.size).sum();
    let http = &app.state::<Ctx>().http;
    let mut prior = 0u64;
    let mut pairs = Vec::with_capacity(targets.len());
    for file in targets {
        // Manifest paths are relative to the game folder; never let one point
        // outside it.
        let rel = safe_relative_path(&file.path)
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| format!("Ruta no permitida en el manifest: {}", file.path))?;
        let staged = staging.join(sanitize_filename(&file.path));
        let mut throttle = Throttle::default();
        download_verified(
            http,
            &file.url,
            &staged,
            file.size,
            &file.sha256,
            |received| {
                if throttle.ready(received == file.size) {
                    report.send(
                        InstallPhase::Download,
                        percent(prior + received, total),
                        None,
                    );
                }
            },
        )
        .await?;
        remove_zone_identifier(&staged);
        prior += file.size;
        pairs.push((staged, dir.join(rel)));
    }

    if can_write_to_dir(dir) {
        report.send(InstallPhase::Install, 0.0, Some("Reparando archivos..."));
        for (src, dst) in &pairs {
            tokio::fs::copy(src, dst).await.map_err(|e| e.to_string())?;
        }
        return Ok(());
    }

    report.send(
        InstallPhase::Install,
        0.0,
        Some("Acepta el aviso de Windows para reparar archivos en la carpeta protegida."),
    );
    let script = powershell::write_script("sarp-copy-", &powershell::copy_script(&pairs))
        .await
        .map_err(|e| e.to_string())?;
    let outcome = powershell::run(&powershell::run_script_elevated(&script), INSTALL_TIMEOUT).await;
    let _ = tokio::fs::remove_file(&script).await;
    match outcome {
        Outcome::Exited(0) => Ok(()),
        Outcome::Exited(EXIT_UAC_CANCELLED) => Err(UAC_CANCELLED.into()),
        Outcome::Exited(_) => {
            Err("No se pudieron copiar los archivos con permisos de administrador.".into())
        }
        Outcome::TimedOut => Err("La copia no respondió a tiempo.".into()),
        Outcome::Failed(error) => Err(error),
    }
}

/// Limits progress events to one per 100 ms (plus the final one).
#[derive(Default)]
struct Throttle(Option<std::time::Instant>);

impl Throttle {
    fn ready(&mut self, force: bool) -> bool {
        let now = std::time::Instant::now();
        if force
            || self
                .0
                .is_none_or(|t| now.duration_since(t) >= Duration::from_millis(100))
        {
            self.0 = Some(now);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn appends_the_install_dir_last() {
        let dir = Path::new(r"C:\Program Files (x86)\Rockstar Games\GTA San Andreas");
        assert_eq!(
            installer_args(&["/S".into()], dir),
            vec![
                "/S".to_string(),
                r"/D=C:\Program Files (x86)\Rockstar Games\GTA San Andreas".into()
            ]
        );
        assert_eq!(installer_args(&[], dir).len(), 1);
        // A manifest that already sets the folder wins.
        assert_eq!(
            installer_args(&["/S".into(), r"/D=D:\GTA".into()], dir),
            vec!["/S".to_string(), r"/D=D:\GTA".into()]
        );
    }

    #[test]
    fn progress_and_result_shapes() {
        let progress = InstallProgress {
            phase: InstallPhase::Install,
            percent: 0.0,
            message: Some("Instalando SA:MP...".into()),
        };
        assert_eq!(
            serde_json::to_value(progress).unwrap(),
            json!({ "phase": "install", "percent": 0.0, "message": "Instalando SA:MP..." })
        );
        let bare = InstallProgress {
            phase: InstallPhase::Download,
            percent: 50.0,
            message: None,
        };
        assert_eq!(
            serde_json::to_value(bare).unwrap(),
            json!({ "phase": "download", "percent": 50.0 })
        );
        assert_eq!(
            serde_json::to_value(InstallResult {
                ok: true,
                error: None
            })
            .unwrap(),
            json!({ "ok": true })
        );
    }

    #[test]
    fn throttle_lets_the_final_event_through() {
        let mut throttle = Throttle::default();
        assert!(throttle.ready(false));
        assert!(!throttle.ready(false));
        assert!(throttle.ready(true));
    }
}
