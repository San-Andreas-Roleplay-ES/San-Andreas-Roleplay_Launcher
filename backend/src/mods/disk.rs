//! Reading the game folder for mods: every path stays inside it, and a
//! symlink or junction on the way never counts (nothing is read or written
//! through one). File hashes are cached by size and modification time, so
//! repeated checks don't hash the same files over and over.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use crate::install::extract::strict_relative_path;
use crate::install::fs::sha256_file;

/// `dir/rel` (`rel` game-relative, forward slashes, an optional trailing `/`)
/// if no part of it below `dir` is a symlink or junction. Parts that don't
/// exist yet are fine. `dir` itself may be a link: the user picked it.
pub fn confined(dir: &Path, rel: &str) -> Option<PathBuf> {
    let parts = strict_relative_path(rel, true)?;
    let mut path = dir.to_path_buf();
    for part in parts {
        path.push(part);
        match fs::symlink_metadata(&path) {
            // `is_symlink` covers junctions too (name-surrogate reparse
            // points), not OneDrive's cloud placeholders.
            Ok(meta) if meta.file_type().is_symlink() => return None,
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let mut full = dir.to_path_buf();
    full.extend(strict_relative_path(rel, true)?);
    Some(full)
}

/// `rel` exists in `dir`: a folder when it ends with `/`, anything else
/// otherwise. Links never count.
pub fn exists(dir: &Path, rel: &str) -> bool {
    let Some(path) = confined(dir, rel) else {
        return false;
    };
    match fs::symlink_metadata(path) {
        Ok(meta) if rel.ends_with('/') => meta.is_dir(),
        Ok(meta) => !meta.file_type().is_symlink(),
        Err(_) => false,
    }
}

/// SHA-256 of files, remembered while their size and modification time
/// don't change.
#[derive(Default)]
pub struct Hasher(Mutex<HashMap<PathBuf, Hashed>>);

/// Size, modification time and SHA-256 of a file when it was hashed.
type Hashed = (u64, Option<SystemTime>, String);

impl Hasher {
    /// Lower-case hex, `None` when it can't be read. Blocking.
    pub fn sha256(&self, path: &Path) -> Option<String> {
        let meta = fs::symlink_metadata(path).ok()?;
        if !meta.file_type().is_file() {
            return None;
        }
        let stamp = (meta.len(), meta.modified().ok());
        let cache = self.0.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((len, modified, sha)) = cache.get(path) {
            if (*len, *modified) == stamp {
                return Some(sha.clone());
            }
        }
        drop(cache);
        let sha = sha256_file(path).ok()?;
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(path.to_path_buf(), (stamp.0, stamp.1, sha.clone()));
        Some(sha)
    }

    /// `rel` in `dir` is a plain file of `bytes` bytes with this hash.
    pub fn matches(&self, dir: &Path, rel: &str, bytes: u64, sha256: &str) -> bool {
        let Some(path) = confined(dir, rel) else {
            return false;
        };
        let Ok(meta) = fs::symlink_metadata(&path) else {
            return false;
        };
        meta.file_type().is_file()
            && meta.len() == bytes
            && self
                .sha256(&path)
                .is_some_and(|sha| sha.eq_ignore_ascii_case(sha256))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn paths_stay_inside_and_links_never_count() {
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("GTA");
        let outside = root.path().join("outside");
        fs::create_dir_all(game.join("modloader")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("x.asi"), b"x").unwrap();
        fs::write(game.join("modloader").join("a.txd"), b"a").unwrap();

        assert_eq!(
            confined(&game, "modloader/a.txd"),
            Some(game.join("modloader").join("a.txd"))
        );
        assert_eq!(
            confined(&game, "new/folder/b.txd"),
            Some(game.join("new").join("folder").join("b.txd"))
        );
        for bad in ["../outside/x.asi", "C:/Windows", "a\\b", "", "nul"] {
            assert_eq!(confined(&game, bad), None, "{bad}");
        }
        assert!(exists(&game, "modloader/"));
        assert!(exists(&game, "modloader"));
        assert!(exists(&game, "modloader/a.txd"));
        assert!(!exists(&game, "modloader/a.txd/"), "a file is not a folder");
        assert!(!exists(&game, "missing.asi"));

        // A junction inside the game folder pointing outside it.
        let junction = game.join("evil");
        let made = Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&junction)
            .arg(&outside)
            .output()
            .unwrap();
        assert!(made.status.success(), "{made:?}");
        assert!(outside.join("x.asi").exists());
        assert_eq!(confined(&game, "evil/x.asi"), None);
        assert_eq!(confined(&game, "evil"), None);
        assert!(!exists(&game, "evil/"));
        assert!(!exists(&game, "evil/x.asi"));
        let hasher = Hasher::default();
        let sha = sha256_file(&outside.join("x.asi")).unwrap();
        assert!(!hasher.matches(&game, "evil/x.asi", 1, &sha));
        fs::remove_dir(&junction).unwrap();
        assert!(outside.join("x.asi").exists());
    }

    #[test]
    fn hashes_are_cached_until_the_file_changes() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.asi");
        fs::write(&file, b"abc").unwrap();
        let hasher = Hasher::default();
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(hasher.sha256(&file).as_deref(), Some(abc));
        assert!(hasher.matches(dir.path(), "a.asi", 3, abc));
        assert!(hasher.matches(dir.path(), "a.asi", 3, &abc.to_uppercase()));
        assert!(!hasher.matches(dir.path(), "a.asi", 4, abc));
        fs::write(&file, b"abd").unwrap();
        let later = SystemTime::now() + std::time::Duration::from_secs(5);
        fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(later)
            .unwrap();
        assert!(!hasher.matches(dir.path(), "a.asi", 3, abc));
        assert_eq!(hasher.sha256(&dir.path().join("missing")), None);
        assert_eq!(hasher.sha256(dir.path()), None, "folders have no hash");
    }
}
