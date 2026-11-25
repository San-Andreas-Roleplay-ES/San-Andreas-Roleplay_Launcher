//! Health check shown on the home screen: GTA folder and SA-MP (local and
//! instant), and the server cache (needs the API's list, so it is its own
//! command and never holds up the other rows).

use std::path::Path;

use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::game::paths::{find_missing, game_dir, REQUIRED_DIRS, REQUIRED_FILES};
use crate::install::cache::{self, cache_root, CacheList};
use crate::Ctx;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GtaHealth {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Every required item is missing: an empty (or fake) install. The UI
    /// offers "Instalar" in that case.
    pub missing_all: bool,
    /// The folder in use, when it is a valid install.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SampHealth {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Never blocks playing: SA-MP downloads whatever model it lacks from the
/// game server. `needs_install` / `needs_update` only say a sync would help.
/// `ok: false` with neither means "could not verify" (no list, empty list,
/// unreadable folder): nothing gets synced or deleted.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheHealth {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// None of the listed files is in the folder: download from scratch.
    pub needs_install: bool,
    /// Some files missing (or with another size), or stale ones to delete.
    pub needs_update: bool,
    /// Files in the list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_file_count: Option<u64>,
    /// Listed files already in the folder with the right size.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual_file_count: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HealthCheckPayload {
    pub gta: GtaHealth,
    pub samp: SampHealth,
}

const NO_LIST: &str = "No se pudo verificar el caché. Revisa tu conexión a Internet.";
const EMPTY_LIST: &str =
    "No se pudo verificar el caché por ahora. El juego descargará lo que falte al conectarte.";
const UNREADABLE: &str = "No se pudo leer la carpeta del caché.";
const NOT_INSTALLED: &str =
    "El caché del servidor no está instalado; el juego descargará los modelos al entrar.";

/// GTA check for a folder (`None` = no valid folder picked).
pub fn check_gta(stored: Option<&Path>) -> GtaHealth {
    let fail = |missing_all, detail: String| GtaHealth {
        ok: false,
        detail: Some(detail),
        missing_all,
        path: None,
    };
    let Some(dir) = stored else {
        return fail(
            true,
            "Aún no has elegido la carpeta de GTA San Andreas.".into(),
        );
    };
    let missing = find_missing(Some(dir)).names();
    match missing.len() {
        0 => GtaHealth {
            ok: true,
            detail: None,
            missing_all: false,
            path: Some(dir.to_string_lossy().into_owned()),
        },
        n if n == REQUIRED_FILES.len() + REQUIRED_DIRS.len() => {
            fail(true, "No está instalado en esta carpeta.".into())
        }
        1 => fail(
            false,
            format!("No encontramos {} en la carpeta.", missing[0]),
        ),
        _ => fail(
            false,
            format!("Faltan en la carpeta: {}.", missing.join(", ")),
        ),
    }
}

pub fn check_samp(game_dir: Option<&Path>) -> SampHealth {
    let Some(dir) = game_dir else {
        return SampHealth {
            ok: false,
            detail: Some("Elige primero la carpeta de GTA San Andreas.".into()),
        };
    };
    if !dir.join("samp.exe").exists() {
        return SampHealth {
            ok: false,
            detail: Some("Falta samp.exe. Vuelve a abrir el launcher para descargarlo.".into()),
        };
    }
    SampHealth {
        ok: true,
        detail: None,
    }
}

fn unverified(detail: &str) -> CacheHealth {
    CacheHealth {
        ok: false,
        detail: Some(detail.into()),
        needs_install: false,
        needs_update: false,
        expected_file_count: None,
        actual_file_count: None,
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        one.to_string()
    } else {
        many.replace("{n}", &n.to_string())
    }
}

/// Compares `cache_dir` with the API's list (`None`: it could not be
/// fetched). Read-only. Blocking: scans the folder.
pub fn check_cache(cache_dir: &Path, list: Option<&CacheList>) -> CacheHealth {
    let Some(list) = list else {
        return unverified(NO_LIST);
    };
    if list.files.is_empty() {
        // Nothing published yet: we can't tell what belongs in the folder.
        return unverified(EMPTY_LIST);
    }
    let scan = match cache::scan(cache_dir) {
        Ok(scan) => scan,
        Err(error) => {
            log::warn!("scan {}: {error}", cache_dir.display());
            return unverified(UNREADABLE);
        }
    };
    let plan = cache::plan(list, &scan.files);
    let expected = list.files.len();
    let missing = plan.missing.len();
    let (ok, needs_install, needs_update, detail) = if missing == 0 && plan.stale.is_empty() {
        (true, false, false, None)
    } else if plan.present == 0 {
        (false, true, false, Some(NOT_INSTALLED.to_string()))
    } else if missing > 0 {
        (false, false, true, Some(cache::missing_message(missing)))
    } else {
        let stale = plan.stale.len();
        let detail = plural(
            stale,
            "Hay 1 archivo viejo en el caché.",
            "Hay {n} archivos viejos en el caché.",
        );
        (false, false, true, Some(detail))
    };
    CacheHealth {
        ok,
        detail,
        needs_install,
        needs_update,
        expected_file_count: Some(expected as u64),
        actual_file_count: Some(plan.present as u64),
    }
}

pub fn run(app: &AppHandle) -> HealthCheckPayload {
    let ctx = app.state::<Ctx>();
    // Same as before: only a still-valid folder counts, so an install that
    // broke since it was picked reads as "not chosen" (missingAll → the UI
    // offers to install / pick again).
    let dir = game_dir(&ctx.store);
    HealthCheckPayload {
        gta: check_gta(dir.as_deref()),
        samp: check_samp(dir.as_deref()),
    }
}

/// The cache row. Fetches the list (shared with the sync for a minute).
pub async fn cache(app: &AppHandle) -> CacheHealth {
    let dir = match cache_root(app) {
        Ok(dir) => dir,
        Err(error) => return unverified(&error),
    };
    let ctx = app.state::<Ctx>();
    let list = ctx
        .cache
        .list(&ctx.api)
        .await
        .inspect_err(|error| log::warn!("cache list: {error}"))
        .ok();
    tokio::task::spawn_blocking(move || check_cache(&dir, list.as_deref()))
        .await
        .unwrap_or_else(|_| unverified(UNREADABLE))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::paths::tests::fake_install;
    use crate::install::cache::tests::{entry, list_of};
    use serde_json::json;
    use std::fs;
    use std::time::{Duration, SystemTime};

    #[test]
    fn gta_messages() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            check_gta(None),
            GtaHealth {
                ok: false,
                detail: Some("Aún no has elegido la carpeta de GTA San Andreas.".into()),
                missing_all: true,
                path: None,
            }
        );
        let empty = check_gta(Some(dir.path()));
        assert!(empty.missing_all);
        assert_eq!(
            empty.detail.as_deref(),
            Some("No está instalado en esta carpeta.")
        );

        fake_install(dir.path());
        assert_eq!(
            check_gta(Some(dir.path())),
            GtaHealth {
                ok: true,
                detail: None,
                missing_all: false,
                path: Some(dir.path().to_string_lossy().into_owned()),
            }
        );

        fs::remove_file(dir.path().join("gta_sa.exe")).unwrap();
        assert_eq!(
            check_gta(Some(dir.path())).detail.as_deref(),
            Some("No encontramos gta_sa.exe en la carpeta.")
        );
        fs::remove_dir(dir.path().join("anim")).unwrap();
        let two = check_gta(Some(dir.path()));
        assert_eq!(
            two.detail.as_deref(),
            Some("Faltan en la carpeta: gta_sa.exe, anim.")
        );
        assert!(!two.missing_all);
        assert_eq!(two.path, None, "only a valid folder is reported");
    }

    #[test]
    fn samp_messages() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            check_samp(None).detail.as_deref(),
            Some("Elige primero la carpeta de GTA San Andreas.")
        );
        assert_eq!(
            check_samp(Some(dir.path())).detail.as_deref(),
            Some("Falta samp.exe. Vuelve a abrir el launcher para descargarlo.")
        );
        fs::write(dir.path().join("samp.exe"), b"").unwrap();
        assert!(check_samp(Some(dir.path())).ok);
    }

    #[test]
    fn cache_could_not_be_verified() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("0xDEAD.dff"), b"x").unwrap();
        let offline = check_cache(dir.path(), None);
        assert_eq!(offline, unverified(NO_LIST));
        let empty = check_cache(dir.path(), Some(&list_of(vec![], Some(SystemTime::now()))));
        assert_eq!(empty, unverified(EMPTY_LIST));
        // Read-only: nothing was touched.
        assert!(dir.path().join("0xDEAD.dff").exists());
    }

    #[test]
    fn cache_states() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("cache");
        let zip = SystemTime::now() - Duration::from_secs(3600);
        let list = list_of(
            vec![
                entry("0xA.dff", 1),
                entry("0xB.txd", 2),
                entry("0xC.dff", 3),
            ],
            Some(zip),
        );

        // Folder absent (and later empty): install.
        let absent = check_cache(&dir, Some(&list));
        assert!(absent.needs_install && !absent.ok);
        assert_eq!(
            absent.detail.as_deref(),
            Some("El caché del servidor no está instalado; el juego descargará los modelos al entrar.")
        );
        assert_eq!(
            (absent.expected_file_count, absent.actual_file_count),
            (Some(3), Some(0))
        );
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("unrelated.txt"), b"x").unwrap();
        assert!(check_cache(&dir, Some(&list)).needs_install);

        // Partly there, one with the wrong size: update.
        fs::write(dir.join("0xA.dff"), b"a").unwrap();
        fs::write(dir.join("0xB.txd"), b"b").unwrap();
        let partial = check_cache(&dir, Some(&list));
        assert!(partial.needs_update && !partial.needs_install);
        assert_eq!(
            partial.detail.as_deref(),
            Some("Faltan 2 archivos del caché; el juego los descargará al entrar.")
        );
        assert_eq!(partial.actual_file_count, Some(1));

        fs::write(dir.join("0xB.txd"), b"bb").unwrap();
        let one = check_cache(&dir, Some(&list));
        assert_eq!(
            one.detail.as_deref(),
            Some("Falta 1 archivo del caché; el juego lo descargará al entrar.")
        );

        // Complete.
        fs::write(dir.join("0xC.dff"), b"ccc").unwrap();
        let ok = check_cache(&dir, Some(&list));
        assert!(ok.ok && !ok.needs_update && ok.detail.is_none());
        assert_eq!(
            (ok.expected_file_count, ok.actual_file_count),
            (Some(3), Some(3))
        );

        // A game download newer than the zip does not count against it...
        let fresh = dir.join("0xF00D.dff");
        fs::write(&fresh, b"new").unwrap();
        assert!(check_cache(&dir, Some(&list)).ok);
        // ...an old stale file does.
        let old = dir.join("0xDEAD.dff");
        fs::write(&old, b"old").unwrap();
        fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(zip - Duration::from_secs(60))
            .unwrap();
        let stale = check_cache(&dir, Some(&list));
        assert!(stale.needs_update);
        assert_eq!(
            stale.detail.as_deref(),
            Some("Hay 1 archivo viejo en el caché.")
        );
        assert!(old.exists(), "the health check never deletes");
    }

    #[test]
    fn payload_shapes() {
        let payload = HealthCheckPayload {
            gta: GtaHealth {
                ok: true,
                detail: None,
                missing_all: false,
                path: Some(r"D:\GTA".into()),
            },
            samp: SampHealth {
                ok: false,
                detail: Some("x".into()),
            },
        };
        assert_eq!(
            serde_json::to_value(payload).unwrap(),
            json!({
                "gta": { "ok": true, "missingAll": false, "path": "D:\\GTA" },
                "samp": { "ok": false, "detail": "x" }
            })
        );
        assert_eq!(
            serde_json::to_value(unverified("y")).unwrap(),
            json!({ "ok": false, "detail": "y", "needsInstall": false, "needsUpdate": false })
        );
        let counted = CacheHealth {
            ok: true,
            detail: None,
            needs_install: false,
            needs_update: false,
            expected_file_count: Some(11414),
            actual_file_count: Some(11414),
        };
        assert_eq!(
            serde_json::to_value(counted).unwrap(),
            json!({
                "ok": true, "needsInstall": false, "needsUpdate": false,
                "expectedFileCount": 11414, "actualFileCount": 11414
            })
        );
    }
}
