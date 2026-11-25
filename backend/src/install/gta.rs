//! Installs the GTA: San Andreas rip into an (empty) folder.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::app::events::{self, GTA_INSTALL_PROGRESS};
use crate::game::shortcut::ensure_game_folder_shortcut;
use crate::install::cancel::Cancel;
use crate::install::extract;
use crate::install::fs::{
    can_write_to_dir, clear_temp_dir, ensure_temp_dir, remove_zone_identifier, sha256_file_async,
};
use crate::install::powershell::{self, Outcome, EXIT_UAC_CANCELLED};
use crate::install::progress::{Phase, Progress, Reporter};
use crate::net::http::{download_with_resume, Leftover, Limits, ResumeOptions};
use crate::Ctx;

const ZIP_FILENAME: &str = "GTA_RIP.zip";
const EXTRACT_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GtaInstallResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancelled: Option<bool>,
}

impl GtaInstallResult {
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(message.into()),
            cancelled: None,
        }
    }
}

fn reporter(app: &AppHandle, event: &'static str) -> Reporter {
    let app = app.clone();
    Arc::new(move |progress: Progress| events::emit(&app, event, progress))
}

/// Runs the blocking extractor off the async runtime.
async fn extract_direct(zip: PathBuf, target: PathBuf, report: Reporter) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || extract::extract(&zip, &target, &report))
        .await
        .map_err(|e| e.to_string())?
}

async fn extract_elevated(zip: &Path, target: &Path) -> Result<(), String> {
    let script =
        powershell::write_script("sarp-extract-", &powershell::extract_script(zip, target))
            .await
            .map_err(|e| e.to_string())?;
    let outcome = powershell::run(&powershell::run_script_elevated(&script), EXTRACT_TIMEOUT).await;
    let _ = tokio::fs::remove_file(&script).await;
    match outcome {
        Outcome::Exited(0) => Ok(()),
        Outcome::Exited(EXIT_UAC_CANCELLED) => {
            Err("Se canceló la solicitud de permisos de administrador.".into())
        }
        Outcome::Exited(_) => {
            Err("No se pudo extraer el juego con permisos de administrador.".into())
        }
        Outcome::TimedOut => Err("La extracción no respondió a tiempo.".into()),
        Outcome::Failed(error) => Err(error),
    }
}

pub async fn install(app: &AppHandle, target_dir: Option<String>) -> GtaInstallResult {
    let Some(target_dir) = target_dir else {
        return GtaInstallResult::error("Falta la carpeta destino.");
    };
    let report = reporter(app, GTA_INSTALL_PROGRESS);
    let target = PathBuf::from(&target_dir);
    match run(app, &target, &report).await {
        Ok(()) => {
            // Promote the folder so the next health check reads this install.
            app.state::<Ctx>()
                .store
                .update(|d| d.gta_path = Some(target_dir));
            report(Progress::new(Phase::Done, 100.0));
            GtaInstallResult {
                ok: true,
                error: None,
                cancelled: None,
            }
        }
        Err(error) => {
            log::warn!("GTA install failed: {error}");
            report(Progress::new(Phase::Error, 0.0).message(error.clone()));
            GtaInstallResult::error(error)
        }
    }
}

async fn run(app: &AppHandle, target: &Path, report: &Reporter) -> Result<(), String> {
    report(Progress::new(Phase::Preflight, 0.0).message("Preparando instalación..."));
    let ctx = app.state::<Ctx>();
    let manifest = ctx.manifest.ensure_saved(&ctx.api, &ctx.store).await?;
    let rip = &manifest.gta.rip;

    let temp = ensure_temp_dir(&ctx.store).map_err(|e| e.to_string())?;
    let zip = temp.join(ZIP_FILENAME);
    let opts = ResumeOptions {
        url: &rip.url,
        dest: &zip,
        expected_size: rip.size,
        leftover: Leftover::Sha256(&rip.sha256),
        limits: Limits::RESUMABLE,
        message: None,
    };
    // Not cancellable from the UI (yet): a token nobody fires.
    download_with_resume(&ctx.http, &opts, report, &Cancel::default()).await?;

    report(Progress::new(Phase::Verify, 100.0).message("Verificando archivo..."));
    let sha = sha256_file_async(zip.clone())
        .await
        .map_err(|e| e.to_string())?;
    if !sha.eq_ignore_ascii_case(&rip.sha256) {
        // Drop the bad zip but keep the folder so a retry can reuse it.
        let _ = tokio::fs::remove_file(&zip).await;
        return Err("El archivo descargado no coincide con el original.".into());
    }
    remove_zone_identifier(&zip);

    if can_write_to_dir(target) {
        report(Progress::new(Phase::Extract, 0.0).message("Extrayendo archivos..."));
        extract_direct(zip.clone(), target.to_path_buf(), report.clone()).await?;
    } else {
        report(
            Progress::new(Phase::Extract, 0.0)
                .message("Acepta el aviso de Windows para extraer en la carpeta protegida."),
        );
        extract_elevated(&zip, target).await?;
    }

    report(Progress::new(Phase::Shortcut, 100.0).message("Creando acceso directo..."));
    ensure_game_folder_shortcut(target).await;

    clear_temp_dir(&ctx.store);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn result_shape() {
        assert_eq!(
            serde_json::to_value(GtaInstallResult::error("Falta la carpeta destino.")).unwrap(),
            json!({ "ok": false, "error": "Falta la carpeta destino." })
        );
    }
}
