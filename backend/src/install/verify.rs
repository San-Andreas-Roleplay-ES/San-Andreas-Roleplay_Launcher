//! Compares the SA-MP files in the game folder with the manifest.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::install::fs::sha256_file_async;
use crate::net::manifest::{LauncherManifest, ManifestFileEntry};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reason {
    Missing,
    SizeMismatch,
    HashMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Expected {
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Actual {
    pub exists: bool,
    pub size: Option<u64>,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileVerification {
    pub path: String,
    pub expected: Expected,
    pub actual: Actual,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<Reason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SampVerificationResult {
    pub ok: bool,
    pub files: Vec<FileVerification>,
    /// User-facing (Spanish).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// No manifest to check against (API unreachable, none saved yet), but
    /// samp.exe is in the folder: playable, the UI only warns.
    pub unverified: bool,
}

pub const NO_GAME_DIR: &str = "Elige primero la carpeta de GTA: San Andreas.";

impl SampVerificationResult {
    pub fn failed(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            files: Vec::new(),
            error: Some(error.into()),
            unverified: false,
        }
    }
}

/// Without any manifest: samp.exe in the folder is taken as a playable SA-MP
/// that could not be checked; without it, SA-MP is missing (and can't be
/// installed until the server answers).
pub fn unverified(game_dir: Option<&Path>) -> SampVerificationResult {
    let Some(dir) = game_dir else {
        return SampVerificationResult::failed(NO_GAME_DIR);
    };
    if dir.join("samp.exe").is_file() {
        SampVerificationResult {
            unverified: true,
            ..SampVerificationResult::failed(
                "No se pudo verificar SA:MP porque no hay conexión con el servidor. Puedes jugar igual.",
            )
        }
    } else {
        SampVerificationResult::failed(
            "Falta samp.exe y no se pudo contactar al servidor para instalarlo. Revisa tu conexión a Internet.",
        )
    }
}

async fn verify_file(
    game_dir: &Path,
    entry: &ManifestFileEntry,
) -> Result<FileVerification, String> {
    let abs: PathBuf = game_dir.join(&entry.path);
    let mut result = FileVerification {
        path: entry.path.clone(),
        expected: Expected {
            sha256: entry.sha256.clone(),
            size: entry.size,
        },
        actual: Actual {
            exists: false,
            size: None,
            sha256: None,
        },
        ok: false,
        reason: None,
    };
    let Ok(meta) = tokio::fs::metadata(&abs).await else {
        result.reason = Some(Reason::Missing);
        return Ok(result);
    };
    result.actual.exists = true;
    result.actual.size = Some(meta.len());
    if meta.len() != entry.size {
        result.reason = Some(Reason::SizeMismatch);
        return Ok(result);
    }
    let sha = sha256_file_async(abs).await.map_err(|error| {
        log::warn!("could not hash {}: {error}", entry.path);
        format!("No se pudo leer {} en la carpeta del juego.", entry.path)
    })?;
    let matches = sha.eq_ignore_ascii_case(&entry.sha256);
    result.actual.sha256 = Some(sha);
    if matches {
        result.ok = true;
    } else {
        result.reason = Some(Reason::HashMismatch);
    }
    Ok(result)
}

pub async fn verify(
    game_dir: Option<&Path>,
    manifest: &LauncherManifest,
) -> SampVerificationResult {
    let Some(dir) = game_dir else {
        return SampVerificationResult::failed(NO_GAME_DIR);
    };
    let mut files = Vec::with_capacity(manifest.samp.files.len());
    for entry in &manifest.samp.files {
        match verify_file(dir, entry).await {
            Ok(file) => files.push(file),
            Err(error) => return SampVerificationResult::failed(error),
        }
    }
    SampVerificationResult {
        ok: files.iter().all(|f| f.ok),
        files,
        error: None,
        unverified: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::manifest::{parse, tests::sample};
    use serde_json::json;
    use std::fs;

    fn manifest_with(files: serde_json::Value) -> LauncherManifest {
        let mut value = sample();
        value["samp"]["files"] = files;
        parse(value).unwrap()
    }

    #[tokio::test]
    async fn reports_each_reason() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("ok.bin"), b"abc").unwrap();
        fs::write(dir.path().join("short.bin"), b"ab").unwrap();
        fs::write(dir.path().join("tampered.bin"), b"abd").unwrap();
        let abc_sha = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let entry = |path: &str| json!({ "path": path, "url": "https://x/y", "sha256": abc_sha, "size": 3 });
        let manifest = manifest_with(json!([
            entry("ok.bin"),
            entry("missing.bin"),
            entry("short.bin"),
            entry("tampered.bin")
        ]));

        let result = verify(Some(dir.path()), &manifest).await;
        assert!(!result.ok);
        let reasons: Vec<_> = result.files.iter().map(|f| f.reason).collect();
        assert_eq!(
            reasons,
            vec![
                None,
                Some(Reason::Missing),
                Some(Reason::SizeMismatch),
                Some(Reason::HashMismatch)
            ]
        );

        let json = serde_json::to_value(&result.files).unwrap();
        assert_eq!(
            json[0],
            json!({
                "path": "ok.bin",
                "expected": { "sha256": abc_sha, "size": 3 },
                "actual": { "exists": true, "size": 3, "sha256": abc_sha },
                "ok": true
            })
        );
        assert_eq!(
            json[1],
            json!({
                "path": "missing.bin",
                "expected": { "sha256": abc_sha, "size": 3 },
                "actual": { "exists": false, "size": null, "sha256": null },
                "ok": false,
                "reason": "missing"
            })
        );
        assert_eq!(json[2]["reason"], "size-mismatch");
        assert_eq!(json[3]["reason"], "hash-mismatch");
    }

    #[tokio::test]
    async fn needs_a_game_dir() {
        let result = verify(None, &parse(sample()).unwrap()).await;
        assert_eq!(
            serde_json::to_value(result).unwrap(),
            json!({ "ok": false, "files": [], "error": "Elige primero la carpeta de GTA: San Andreas.", "unverified": false })
        );
    }

    #[test]
    fn without_a_manifest_samp_exe_is_enough_to_play() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            unverified(None),
            SampVerificationResult::failed(NO_GAME_DIR)
        );
        // Missing: blocks (and says why it can't be installed now).
        let missing = unverified(Some(dir.path()));
        assert!(!missing.ok && !missing.unverified);
        assert_eq!(
            missing.error.as_deref(),
            Some("Falta samp.exe y no se pudo contactar al servidor para instalarlo. Revisa tu conexión a Internet.")
        );
        // Present: a warning only.
        fs::write(dir.path().join("samp.exe"), b"MZ").unwrap();
        assert_eq!(
            serde_json::to_value(unverified(Some(dir.path()))).unwrap(),
            json!({
                "ok": false, "files": [], "unverified": true,
                "error": "No se pudo verificar SA:MP porque no hay conexión con el servidor. Puedes jugar igual."
            })
        );
        // A folder named samp.exe is not samp.exe.
        let other = tempfile::tempdir().unwrap();
        fs::create_dir(other.path().join("samp.exe")).unwrap();
        assert!(!unverified(Some(other.path())).unverified);
    }
}
