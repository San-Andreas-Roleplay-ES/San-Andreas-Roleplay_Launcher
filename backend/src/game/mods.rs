//! Detects the common GTA San Andreas mod loaders in the game folder.

use std::path::Path;

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModId {
    Cleo,
    Modloader,
    AsiLoader,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DetectedMod {
    pub id: ModId,
    pub label: &'static str,
}

const KNOWN: &[(ModId, &str, &[&str], Option<&str>)] = &[
    (ModId::Cleo, "CLEO 4", &["cleo.asi"], Some("cleo")),
    (
        ModId::Modloader,
        "Modloader",
        &["modloader.asi"],
        Some("modloader"),
    ),
    (
        ModId::AsiLoader,
        "Ultimate ASI Loader",
        &["dinput8.dll", "vorbisFile.dll", "vorbisHooked.dll"],
        None,
    ),
];

pub fn detect(game_dir: Option<&Path>) -> Vec<DetectedMod> {
    let Some(dir) = game_dir else {
        return Vec::new();
    };
    KNOWN
        .iter()
        .filter(|(_, _, files, folder)| {
            files.iter().any(|f| dir.join(f).exists())
                && folder.is_none_or(|name| dir.join(name).is_dir())
        })
        .map(|(id, label, _, _)| DetectedMod { id: *id, label })
        .collect()
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

        fs::write(dir.path().join("cleo.asi"), b"").unwrap();
        assert!(detect(Some(dir.path())).is_empty(), "needs the folder too");
        fs::create_dir(dir.path().join("cleo")).unwrap();
        fs::write(dir.path().join("modloader.asi"), b"").unwrap();
        fs::create_dir(dir.path().join("modloader")).unwrap();
        fs::write(dir.path().join("dinput8.dll"), b"").unwrap();

        assert_eq!(
            serde_json::to_value(detect(Some(dir.path()))).unwrap(),
            json!([
                { "id": "cleo", "label": "CLEO 4" },
                { "id": "modloader", "label": "Modloader" },
                { "id": "asi-loader", "label": "Ultimate ASI Loader" }
            ])
        );
    }
}
