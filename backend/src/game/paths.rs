//! The GTA: San Andreas folder: validation, the user-picked override, and the
//! folder pickers.

use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::DialogExt;

use crate::config::{dev_gta_path, IS_DEV};
use crate::game::shortcut::ensure_game_folder_shortcut;
use crate::install::fs::is_effectively_empty;
use crate::store::Store;
use crate::Ctx;

const TYPICAL_GTA_PATH: &str = r"C:\Program Files (x86)\Rockstar Games\GTA San Andreas";

/// Files that must all exist for a folder to count as a GTA SA install.
/// The vorbis DLLs ship with the original 1.0 release and SA-MP needs them.
pub const REQUIRED_FILES: [&str; 4] = [
    "gta_sa.exe",
    "vorbis.dll",
    "vorbisFile.dll",
    "vorbisHooked.dll",
];
/// Folders present in any vanilla install.
pub const REQUIRED_DIRS: [&str; 4] = ["anim", "audio", "data", "models"];

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Missing {
    pub files: Vec<&'static str>,
    pub dirs: Vec<&'static str>,
}

impl Missing {
    /// Files first, then folders — the order used in user-facing messages.
    pub fn names(&self) -> Vec<&'static str> {
        self.files.iter().chain(&self.dirs).copied().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty() && self.dirs.is_empty()
    }
}

pub fn find_missing(path: Option<&Path>) -> Missing {
    let Some(path) = path.filter(|p| p.exists()) else {
        return Missing {
            files: REQUIRED_FILES.to_vec(),
            dirs: REQUIRED_DIRS.to_vec(),
        };
    };
    Missing {
        files: REQUIRED_FILES
            .into_iter()
            .filter(|name| !path.join(name).is_file())
            .collect(),
        dirs: REQUIRED_DIRS
            .into_iter()
            .filter(|name| !path.join(name).is_dir())
            .collect(),
    }
}

/// The folder the user picked, as long as it is still a valid install.
pub fn game_dir(store: &Store) -> Option<PathBuf> {
    let path = PathBuf::from(store.get().gta_path?);
    find_missing(Some(&path)).is_empty().then_some(path)
}

/// Starting folder for the pickers when the user has never chosen one.
fn suggested_default(app: &AppHandle) -> PathBuf {
    if IS_DEV {
        return PathBuf::from(dev_gta_path().unwrap_or_else(|| TYPICAL_GTA_PATH.into()));
    }
    let typical = PathBuf::from(TYPICAL_GTA_PATH);
    if typical.exists() {
        return typical;
    }
    app.path().home_dir().unwrap_or(typical)
}

#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct PickGameDirResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancelled: Option<bool>,
}

impl PickGameDirResult {
    fn cancelled() -> Self {
        Self {
            cancelled: Some(true),
            ..Self::default()
        }
    }

    fn error(message: impl Into<String>) -> Self {
        Self {
            error: Some(message.into()),
            ..Self::default()
        }
    }

    fn picked(path: &Path) -> Self {
        Self {
            ok: true,
            path: Some(path.to_string_lossy().into_owned()),
            ..Self::default()
        }
    }
}

async fn pick_folder(app: &AppHandle, title: &str) -> Option<PathBuf> {
    let ctx = app.state::<Ctx>();
    let start = ctx
        .store
        .get()
        .gta_path
        .map(PathBuf::from)
        .unwrap_or_else(|| suggested_default(app));
    let mut dialog = app.dialog().file().set_title(title).set_directory(start);
    if let Some(window) = app.get_webview_window("main") {
        dialog = dialog.set_parent(&window);
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    dialog.pick_folder(move |picked| {
        let _ = tx.send(picked);
    });
    rx.await.ok().flatten()?.into_path().ok()
}

/// Makes `dir` the game folder: stores it and (release builds) puts the
/// launcher shortcut in it. A folder picked by hand, one chosen from the
/// detected installs and one adopted automatically all go through here.
pub async fn adopt(app: &AppHandle, dir: &Path) {
    app.state::<Ctx>()
        .store
        .update(|d| d.gta_path = Some(dir.to_string_lossy().into_owned()));
    ensure_game_folder_shortcut(dir).await;
}

/// Adopts `dir` if it is a valid install.
async fn use_dir(app: &AppHandle, dir: &Path) -> PickGameDirResult {
    let missing = find_missing(Some(dir).filter(|d| d.is_absolute()));
    if !missing.is_empty() {
        return PickGameDirResult::error(format!(
            "La carpeta seleccionada no es una instalación válida de GTA: San Andreas. Falta {}.",
            missing.names().join(", ")
        ));
    }
    adopt(app, dir).await;
    PickGameDirResult::picked(dir)
}

pub async fn pick_game_dir(app: &AppHandle) -> PickGameDirResult {
    let Some(selected) = pick_folder(app, "Selecciona la carpeta de GTA: San Andreas").await else {
        return PickGameDirResult::cancelled();
    };
    use_dir(app, &selected).await
}

/// One of the detected installs, chosen by the user.
pub async fn set_game_dir(app: &AppHandle, path: &str) -> PickGameDirResult {
    use_dir(app, Path::new(path)).await
}

pub async fn pick_empty_install_dir(app: &AppHandle) -> PickGameDirResult {
    let title = "Selecciona una carpeta vacía donde instalar GTA: San Andreas";
    let Some(selected) = pick_folder(app, title).await else {
        return PickGameDirResult::cancelled();
    };
    if !is_effectively_empty(&selected) {
        return PickGameDirResult::error(
            "La carpeta debe estar vacía. Crea una nueva o elige otra.",
        );
    }
    PickGameDirResult::picked(&selected)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    /// Creates a folder that passes the install check.
    pub fn fake_install(dir: &Path) {
        for file in REQUIRED_FILES {
            fs::write(dir.join(file), b"").unwrap();
        }
        for sub in REQUIRED_DIRS {
            fs::create_dir_all(dir.join(sub)).unwrap();
        }
    }

    #[test]
    fn detects_missing_items() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(find_missing(None).names().len(), 8);
        assert_eq!(
            find_missing(Some(&dir.path().join("nope"))).names().len(),
            8
        );

        fake_install(dir.path());
        assert!(find_missing(Some(dir.path())).is_empty());

        fs::remove_file(dir.path().join("vorbis.dll")).unwrap();
        fs::remove_dir(dir.path().join("models")).unwrap();
        // A file where a folder should be does not count.
        fs::write(dir.path().join("models"), b"").unwrap();
        assert_eq!(
            find_missing(Some(dir.path())).names(),
            vec!["vorbis.dll", "models"]
        );
    }

    #[test]
    fn game_dir_only_returns_valid_installs() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("store.json"));
        assert_eq!(game_dir(&store), None);

        let gta = dir.path().join("GTA");
        fs::create_dir(&gta).unwrap();
        store.update(|d| d.gta_path = Some(gta.to_string_lossy().into_owned()));
        assert_eq!(game_dir(&store), None);

        fake_install(&gta);
        assert_eq!(game_dir(&store), Some(gta));
    }

    #[test]
    fn pick_result_shape() {
        assert_eq!(
            serde_json::to_value(PickGameDirResult::cancelled()).unwrap(),
            json!({ "ok": false, "cancelled": true })
        );
        assert_eq!(
            serde_json::to_value(PickGameDirResult::picked(Path::new(r"D:\GTA"))).unwrap(),
            json!({ "ok": true, "path": "D:\\GTA" })
        );
        assert_eq!(
            serde_json::to_value(PickGameDirResult::error("x")).unwrap(),
            json!({ "ok": false, "error": "x" })
        );
    }
}
