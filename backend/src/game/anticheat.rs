use std::fs;
use std::path::Path;

use super::launcher::find_process;

const BANNED_PROCESSES: &[&str] = &[
    "cheatengine.exe",
    "cheat engine.exe",
    "s0beit.exe",
    "sampfuncs.exe",
    "artmoney.exe",
    "wireshark.exe",
    "fiddler.exe",
    "processhacker.exe",
];

const BANNED_FILES: &[&str] = &[
    "moonloader.asi",
    "moonloader",
    "sampfuncs.dll",
    "s0beit.dll",
    "cheatengine.dll",
];

fn scan(dir: &Path, depth: u32) -> Option<String> {
    if depth > 3 {
        return None;
    }
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        // `file_type` does not follow links: a junction inside the game folder
        // must not send the walk outside it.
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if BANNED_FILES.iter().any(|b| name.eq_ignore_ascii_case(b)) {
            return Some(name);
        }
        if kind.is_dir() {
            if let Some(found) = scan(&entry.path(), depth + 1) {
                return Some(found);
            }
        }
    }
    None
}

pub fn check(game_dir: &Path) -> Option<String> {
    for name in BANNED_PROCESSES {
        if find_process(name).is_some() {
            return Some(format!(
                "Cierra {} antes de entrar a San Andreas Roleplay.",
                name
            ));
        }
    }
    scan(game_dir, 0).map(|file| {
        format!(
            "El archivo {} no está autorizado en este servidor. Elimínalo de la carpeta de GTA.",
            file
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_banned_files_in_the_game_folder_and_nearby_subfolders() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Moonloader.asi"), b"").unwrap();
        assert_eq!(scan(dir.path(), 0).as_deref(), Some("Moonloader.asi"));

        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b").join("c");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("sampfuncs.dll"), b"").unwrap();
        assert_eq!(scan(dir.path(), 0).as_deref(), Some("sampfuncs.dll"));
    }

    #[test]
    fn stays_within_the_depth_limit() {
        let dir = tempfile::tempdir().unwrap();
        let deep = dir.path().join("a").join("b").join("c").join("d");
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("moonloader.asi"), b"").unwrap();
        assert!(scan(dir.path(), 0).is_none());
    }

    #[test]
    fn clean_folders_are_allowed() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("cleo.asi"), b"").unwrap();
        fs::create_dir(dir.path().join("modloader")).unwrap();
        assert!(scan(dir.path(), 0).is_none());
    }
}
