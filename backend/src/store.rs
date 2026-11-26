//! Small persistent JSON store (`store.json` in the app config dir) with the
//! same keys as the old `LauncherStoreSchema`. Writes are atomic
//! (temp file + rename) and serialised by the mutex. Keys this version no
//! longer knows (`installedCacheSize`, `cacheInstallTempDir` from the cache
//! zip days) are ignored on load and dropped on the next write.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};

use crate::mods::record::{lenient_entries, Installed, ModPref, SavedCatalog};
use crate::net::manifest::LauncherManifest;

pub const FILE_NAME: &str = "store.json";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreData {
    #[serde(default, deserialize_with = "lenient")]
    pub samp_version: Option<u64>,
    #[serde(default, deserialize_with = "lenient")]
    pub cache_version: Option<u64>,
    #[serde(default, deserialize_with = "lenient")]
    pub gta_path: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub gta_install_temp_dir: Option<String>,
    /// Last known window placement, restored (and validated) on next launch.
    #[serde(default, deserialize_with = "lenient")]
    pub window_state: Option<WindowStateData>,
    /// Last good manifest from the API: SA-MP is verified (and installed)
    /// against it while the API can't be reached.
    #[serde(default, deserialize_with = "lenient")]
    pub manifest: Option<LauncherManifest>,
    /// Last accepted mods catalog (text + signature, verified again on
    /// load): mods keep working from it while the API can't be reached.
    #[serde(default, deserialize_with = "lenient")]
    pub mods_catalog: Option<SavedCatalog>,
    /// Highest catalog `serial` ever accepted: an older one is refused.
    #[serde(default, deserialize_with = "lenient")]
    pub mods_serial: Option<u64>,
    /// What the launcher installed, per game folder (`record::folder_key`).
    #[serde(default, deserialize_with = "lenient_map")]
    pub mods_installed: BTreeMap<String, Installed>,
    /// The user's on/off and variant choices, by mod id.
    #[serde(default, deserialize_with = "lenient_map")]
    pub mods_prefs: BTreeMap<String, ModPref>,
}

/// Window placement in *physical* pixels. `width`/`height` are the inner
/// (client) size of the normal — not maximised/fullscreen — window.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowStateData {
    pub width: f64,
    pub height: f64,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub is_maximized: bool,
    pub is_fullscreen: bool,
}

/// A field with an unexpected type falls back to `None` instead of failing the
/// whole file — a bad `windowState` must never cost the user their `gtaPath`.
fn lenient<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).ok())
}

/// A map whose bad entries are dropped one by one.
fn lenient_map<'de, D, V>(deserializer: D) -> Result<BTreeMap<String, V>, D::Error>
where
    D: Deserializer<'de>,
    V: DeserializeOwned,
{
    Ok(lenient_entries(serde_json::Value::deserialize(
        deserializer,
    )?))
}

pub struct Store {
    path: PathBuf,
    data: Mutex<StoreData>,
}

impl Store {
    /// Loads `path`, falling back to defaults when it is missing or corrupt.
    /// A UTF-8 BOM (Notepad, PowerShell 5 `Set-Content -Encoding utf8`) is
    /// tolerated: treating such a file as corrupt would drop `gtaPath`.
    pub fn open(path: PathBuf) -> Self {
        let data = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(strip_bom(&bytes)).unwrap_or_else(|error| {
                log::warn!(
                    "store at {} is corrupt, using defaults: {error}",
                    path.display()
                );
                StoreData::default()
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => StoreData::default(),
            Err(error) => {
                log::warn!("could not read store at {}: {error}", path.display());
                StoreData::default()
            }
        };
        Self {
            path,
            data: Mutex::new(data),
        }
    }

    pub fn get(&self) -> StoreData {
        self.lock().clone()
    }

    /// Applies `change` and persists the result. Persist errors are logged:
    /// the in-memory value stays updated for this session either way.
    pub fn update(&self, change: impl FnOnce(&mut StoreData)) {
        let mut data = self.lock();
        change(&mut data);
        if let Err(error) = write_atomic(&self.path, &data) {
            log::error!("could not save store to {}: {error}", self.path.display());
        }
    }

    fn lock(&self) -> MutexGuard<'_, StoreData> {
        self.data
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn strip_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes)
}

fn write_atomic(path: &Path, data: &StoreData) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec_pretty(data).map_err(io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(&json)?;
        file.sync_all()?;
    }
    // MoveFileEx(MOVEFILE_REPLACE_EXISTING) under the hood: readers never see
    // a half-written file.
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join(FILE_NAME);

        let store = Store::open(path.clone());
        assert_eq!(store.get(), StoreData::default());

        store.update(|d| {
            d.gta_path = Some(r"C:\Games\GTA San Andreas".into());
            d.gta_install_temp_dir = Some(r"C:\Temp\sarp-launcher-1".into());
            d.window_state = Some(WindowStateData {
                width: 1100.0,
                height: 980.0,
                x: Some(-1910.0),
                y: Some(20.0),
                is_maximized: true,
                is_fullscreen: false,
            });
        });
        assert!(!path.with_extension("json.tmp").exists());

        let reopened = Store::open(path);
        assert_eq!(reopened.get(), store.get());
    }

    #[test]
    fn uses_the_old_camel_case_keys_with_nulls() {
        let value = serde_json::to_value(StoreData::default()).unwrap();
        assert_eq!(
            value,
            json!({
                "sampVersion": null,
                "cacheVersion": null,
                "gtaPath": null,
                "gtaInstallTempDir": null,
                "windowState": null,
                "manifest": null,
                "modsCatalog": null,
                "modsSerial": null,
                "modsInstalled": {},
                "modsPrefs": {}
            })
        );
    }

    #[test]
    fn mod_data_survives_bad_entries() {
        let data: StoreData = serde_json::from_value(json!({
            "gtaPath": "D:\\GTA",
            "modsSerial": 7,
            "modsCatalog": { "catalog": "{}", "signature": "x" },
            "modsInstalled": {
                "d:\\gta": { "radar": { "variant": "lq", "version": "1", "files": [] } },
                "e:\\bad": 5
            },
            "modsPrefs": { "radar": { "enabled": false, "variant": "hd" }, "x": "on" }
        }))
        .unwrap();
        assert_eq!(data.mods_serial, Some(7));
        assert_eq!(data.mods_catalog.unwrap().signature, "x");
        assert_eq!(data.mods_installed.len(), 1);
        assert!(data.mods_installed["d:\\gta"].0.contains_key("radar"));
        assert_eq!(data.mods_prefs.len(), 1);
        assert!(!data.mods_prefs["radar"].enabled);
        // Wrong types for the whole maps: empty, the rest intact.
        let data: StoreData = serde_json::from_value(json!({
            "gtaPath": "D:\\GTA", "modsInstalled": [], "modsPrefs": null, "modsSerial": "7"
        }))
        .unwrap();
        assert!(data.mods_installed.is_empty() && data.mods_prefs.is_empty());
        assert_eq!(data.mods_serial, None);
        assert_eq!(data.gta_path.as_deref(), Some("D:\\GTA"));
    }

    #[test]
    fn keeps_the_last_manifest_and_survives_a_bad_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        let manifest = crate::net::manifest::parse(crate::net::manifest::tests::sample()).unwrap();
        let store = Store::open(path.clone());
        store.update(|d| d.manifest = Some(manifest.clone()));
        assert_eq!(Store::open(path.clone()).get().manifest, Some(manifest));

        // A mangled manifest is dropped, never the folder.
        fs::write(
            &path,
            br#"{ "gtaPath": "D:\\GTA", "manifest": { "schemaVersion": "x" } }"#,
        )
        .unwrap();
        let data = Store::open(path).get();
        assert_eq!(data.manifest, None);
        assert_eq!(data.gta_path.as_deref(), Some("D:\\GTA"));
    }

    #[test]
    fn bad_fields_do_not_poison_the_rest() {
        let data: StoreData = serde_json::from_value(json!({
            "gtaPath": "D:\\GTA",
            "windowState": { "width": "wide" },
            "sampVersion": -5,
            "unknownKey": true
        }))
        .unwrap();
        assert_eq!(data.gta_path.as_deref(), Some("D:\\GTA"));
        assert_eq!(data.window_state, None);
        assert_eq!(data.samp_version, None);
    }

    #[test]
    fn old_files_with_removed_keys_load_and_are_cleaned_on_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        // A store written by the cache-zip versions of the launcher.
        fs::write(
            &path,
            br#"{
              "sampVersion": null,
              "cacheVersion": null,
              "gtaPath": "D:\\GTA",
              "gtaInstallTempDir": null,
              "cacheInstallTempDir": "C:\\Users\\x\\AppData\\Local\\Temp\\sarp-cache-0011",
              "installedCacheSize": 850396987,
              "windowState": { "width": 1100, "height": 980, "x": 10, "y": 20,
                               "isMaximized": false, "isFullscreen": false }
            }"#,
        )
        .unwrap();
        let store = Store::open(path.clone());
        let data = store.get();
        assert_eq!(data.gta_path.as_deref(), Some("D:\\GTA"));
        assert_eq!(data.window_state.map(|w| w.width), Some(1100.0));

        store.update(|d| d.samp_version = Some(1));
        let written: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert!(written.get("installedCacheSize").is_none(), "{written}");
        assert!(written.get("cacheInstallTempDir").is_none(), "{written}");
        assert_eq!(written["gtaPath"], "D:\\GTA");
    }

    #[test]
    fn tolerates_a_utf8_bom() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        let mut bytes = b"\xEF\xBB\xBF".to_vec();
        bytes.extend_from_slice(br#"{ "gtaPath": "D:\\GTA", "sampVersion": 42 }"#);
        fs::write(&path, bytes).unwrap();
        let data = Store::open(path).get();
        assert_eq!(data.gta_path.as_deref(), Some("D:\\GTA"));
        assert_eq!(data.samp_version, Some(42));
    }

    #[test]
    fn corrupt_file_falls_back_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        fs::write(&path, b"{ not json").unwrap();
        let store = Store::open(path.clone());
        assert_eq!(store.get(), StoreData::default());
        store.update(|d| d.gta_path = Some("X".into()));
        assert_eq!(Store::open(path).get().gta_path.as_deref(), Some("X"));
    }
}
