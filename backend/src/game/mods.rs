//! Detection of common GTA SA mod loaders in the game folder.

use std::path::Path;

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModId {
    Modloader,
    AsiLoader,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DetectedMod {
    pub id: ModId,
    pub label: &'static str,
}

pub fn detect(game_dir: Option<&Path>) -> Vec<DetectedMod> {
    let Some(dir) = game_dir else {
        return Vec::new();
    };
    let mut found = Vec::new();
    if dir.join("modloader.asi").exists() && dir.join("modloader").is_dir() {
        found.push(DetectedMod {
            id: ModId::Modloader,
            label: "Modloader",
        });
    }
    if dir.join("dinput8.dll").exists() {
        found.push(DetectedMod {
            id: ModId::AsiLoader,
            label: "Ultimate ASI Loader",
        });
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    #[test]
    fn detects_loaders() {
        let dir = tempfile::tempdir().unwrap();
        assert!(detect(None).is_empty());
        assert!(detect(Some(dir.path())).is_empty());

        fs::write(dir.path().join("modloader.asi"), b"").unwrap();
        assert!(detect(Some(dir.path())).is_empty(), "needs the folder too");
        fs::create_dir(dir.path().join("modloader")).unwrap();
        fs::write(dir.path().join("dinput8.dll"), b"").unwrap();

        assert_eq!(
            serde_json::to_value(detect(Some(dir.path()))).unwrap(),
            json!([
                { "id": "modloader", "label": "Modloader" },
                { "id": "asi-loader", "label": "Ultimate ASI Loader" }
            ])
        );
    }
}
