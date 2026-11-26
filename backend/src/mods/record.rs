//! What the launcher keeps about mods in its store: the last accepted
//! catalog, what it installed in each game folder, and the user's choices.

use std::collections::BTreeMap;
use std::path::Path;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// The last catalog that passed every check, exactly as signed: it is
/// verified again whenever it is loaded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedCatalog {
    pub catalog: String,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedFile {
    /// Relative to the game folder, forward slashes.
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
    /// User config (`keep`): never verified or removed.
    #[serde(default)]
    pub keep: bool,
    /// The file it replaced is in the backups folder and goes back on
    /// uninstall.
    #[serde(default)]
    pub backup: bool,
}

/// One mod the launcher installed (or adopted) in a game folder: exactly the
/// files it owns there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModRecord {
    pub variant: String,
    pub version: String,
    pub files: Vec<RecordedFile>,
    /// `replaces` paths this mod renamed out of the way (`x` → `.x`); they
    /// are renamed back when it is removed.
    #[serde(default)]
    pub disabled: Vec<String>,
    /// Found in place with matching hashes rather than written by us.
    #[serde(default)]
    pub adopted: bool,
    /// Folders the launcher created for this mod (they did not exist). Only
    /// these are ever removed, and only once empty: a folder that was
    /// already there (a user's, `modloader/`) never goes. Older records have
    /// none, so nothing is removed for them.
    #[serde(default)]
    pub dirs: Vec<String>,
}

impl ModRecord {
    pub fn owns(&self, path: &str) -> Option<&RecordedFile> {
        self.files
            .iter()
            .find(|f| f.path.eq_ignore_ascii_case(path))
    }
}

/// The user's choice for a mod. Required mods only use `variant`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModPref {
    pub enabled: bool,
    #[serde(default)]
    pub variant: Option<String>,
}

/// A game folder's records, by mod id.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Installed(pub BTreeMap<String, ModRecord>);

impl<'de> Deserialize<'de> for Installed {
    /// Lenient: a mangled record is dropped, never its neighbours.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match Value::deserialize(deserializer)? {
            value @ Value::Object(_) => Ok(Self(lenient_entries(value))),
            _ => Err(serde::de::Error::custom("not an object")),
        }
    }
}

/// The entries of a JSON object that parse as `V`; anything else is empty.
pub fn lenient_entries<V: DeserializeOwned>(value: Value) -> BTreeMap<String, V> {
    match value {
        Value::Object(map) => map
            .into_iter()
            .filter_map(|(key, value)| Some((key, serde_json::from_value(value).ok()?)))
            .collect(),
        _ => BTreeMap::new(),
    }
}

/// The key a game folder's records go under: the path, lower-case, with
/// backslashes and no trailing separator (`C:\Games\GTA` = `c:/games/gta/`).
pub fn folder_key(dir: &Path) -> String {
    dir.to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn folder_keys_ignore_case_and_separators() {
        let key = folder_key(Path::new(r"C:\Games\GTA San Andreas"));
        assert_eq!(key, r"c:\games\gta san andreas");
        assert_eq!(folder_key(Path::new("c:/games/GTA San Andreas/")), key);
        assert_ne!(folder_key(Path::new(r"C:\Games\GTA")), key);
    }

    #[test]
    fn a_bad_record_does_not_cost_the_others() {
        let installed: Installed = serde_json::from_value(json!({
            "radar": { "variant": "hd", "version": "2", "files": [
                { "path": "modloader/x.txd", "bytes": 1, "sha256": "ab" }
            ] },
            "broken": { "variant": 5 },
            "also": "nope"
        }))
        .unwrap();
        assert_eq!(installed.0.keys().collect::<Vec<_>>(), ["radar"]);
        let radar = &installed.0["radar"];
        assert!(!radar.adopted && radar.disabled.is_empty());
        assert!(radar
            .owns("MODLOADER/X.TXD")
            .is_some_and(|f| !f.keep && !f.backup));
        assert!(serde_json::from_value::<Installed>(json!([1, 2])).is_err());
        assert!(serde_json::from_value::<Installed>(json!(5)).is_err());
    }
}
