//! Filesystem helpers shared by the installers.

use std::collections::hash_map::RandomState;
use std::fs;
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::store::Store;

/// `bytes` random bytes as lowercase hex. Used for temp/probe names, where
/// uniqueness matters but cryptographic strength does not.
pub fn random_hex(bytes: usize) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut out = String::with_capacity(bytes * 2 + 16);
    while out.len() < bytes * 2 {
        let mut hasher = RandomState::new().build_hasher();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        hasher.write_u128(nanos);
        hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
        hasher.write_u32(std::process::id());
        out.push_str(&format!("{:016x}", hasher.finish()));
    }
    out.truncate(bytes * 2);
    out
}

/// Removes the NTFS `Zone.Identifier` stream Windows attaches to downloaded
/// files, so SmartScreen/Defender don't treat the zip or the executables we
/// extract from it as "from the Internet". Best-effort.
pub fn remove_zone_identifier(path: &Path) {
    let mut stream = path.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    let _ = fs::remove_file(PathBuf::from(stream));
}

/// Confirms a directory is writable with a real probe write (an access check
/// alone is unreliable on Windows).
pub fn can_write_to_dir(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let probe = dir.join(format!(".sarp-probe-{}", random_hex(4)));
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(file) => {
            drop(file);
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

// OS-generated metadata files that don't count as "content": without this,
// folders the user perceives as empty get rejected.
const OS_NOISE_FILES: [&str; 3] = ["desktop.ini", "thumbs.db", ".ds_store"];

/// True when the folder exists and only contains OS noise files.
pub fn is_effectively_empty(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    entries.into_iter().all(|entry| match entry {
        Ok(entry) => {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            OS_NOISE_FILES.contains(&name.as_str())
        }
        Err(_) => false,
    })
}

const FALLBACK_FILENAME: &str = "sa-mp-installer.exe";

/// Makes a manifest-provided name safe to use as a single file name: path
/// separators and characters Windows rejects become `_`, whitespace runs
/// collapse to one space. Keeps the original name whenever possible so UAC
/// shows a recognisable installer name.
pub fn sanitize_filename(name: &str) -> String {
    let replaced: String = name
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() && !c.is_whitespace() => '_',
            c => c,
        })
        .collect();
    let collapsed = replaced.split_whitespace().collect::<Vec<_>>().join(" ");
    // "." / ".." (or only dots) would resolve to a directory, never a file.
    if collapsed.is_empty() || collapsed.chars().all(|c| c == '.') {
        FALLBACK_FILENAME.to_string()
    } else {
        collapsed
    }
}

/// Lowercase hex SHA-256 of a file. Blocking: call from `spawn_blocking`.
pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

pub async fn sha256_file_async(path: PathBuf) -> io::Result<String> {
    tauri::async_runtime::spawn_blocking(move || sha256_file(&path))
        .await
        .map_err(io::Error::other)?
}

/// The GTA install's resumable temp folder (persisted in the store so a
/// half-downloaded zip survives a restart): the stored one, or a fresh random
/// one under `%TEMP%`.
pub fn ensure_temp_dir(store: &Store) -> io::Result<PathBuf> {
    if let Some(stored) = store.get().gta_install_temp_dir {
        let stored = PathBuf::from(stored);
        if fs::create_dir_all(&stored).is_ok() {
            return Ok(stored);
        }
    }
    let fresh = std::env::temp_dir().join(format!("sarp-launcher-{}", random_hex(8)));
    fs::create_dir_all(&fresh)?;
    store.update(|d| d.gta_install_temp_dir = Some(fresh.to_string_lossy().into_owned()));
    Ok(fresh)
}

pub fn clear_temp_dir(store: &Store) {
    if let Some(stored) = store.get().gta_install_temp_dir {
        let _ = fs::remove_dir_all(stored);
    }
    store.update(|d| d.gta_install_temp_dir = None);
}

/// A fresh random folder under `%TEMP%` for one-shot work (installer runs,
/// staging). The caller removes it.
pub fn new_temp_dir() -> io::Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("sarp-launcher-{}", random_hex(8)));
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Our temp folders and scripts. `sarp-cache-<hex>` held the Electron
/// launcher's cache zip download; the current one (`sarp-cache-zip`) is
/// passed to the sweep as kept.
const SWEEP_PREFIXES: [&str; 4] = [
    "sarp-launcher-",
    "sarp-cache-",
    "sarp-copy-",
    "sarp-extract-",
];
const ZOMBIE_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// What the updater plugin leaves after installing an update:
/// `<productName>-<version>-updater-<random>` with the installer inside.
fn is_updater_leftover(name: &str, app_name: &str) -> bool {
    name.strip_prefix(app_name)
        .and_then(|rest| rest.strip_prefix('-'))
        .is_some_and(|rest| rest.contains("-updater-"))
}

/// Removes leftover temp folders/scripts older than a day from earlier runs
/// that didn't get to clean up (file locks, crashes...) and old updater
/// folders. The folders in `keep` (resumable downloads) stay regardless of
/// age. Best-effort.
pub fn sweep_temp(root: &Path, keep: &[&Path], app_name: &str) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    let cutoff = SystemTime::now().checked_sub(ZOMBIE_AGE);
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let ours = SWEEP_PREFIXES.iter().any(|p| name.starts_with(p))
            || is_updater_leftover(&name, app_name);
        if !ours {
            continue;
        }
        let path = entry.path();
        if keep.contains(&path.as_path()) {
            continue;
        }
        // Not followed: a link is removed as itself, never its target.
        let Ok(meta) = entry.metadata() else { continue };
        let old = match (meta.modified(), cutoff) {
            (Ok(modified), Some(cutoff)) => modified < cutoff,
            _ => false,
        };
        if !old {
            continue;
        }
        let _ = if meta.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
    }
}

/// Bytes the current user may still write on the volume holding `dir`
/// (honours quotas). `None` when Windows can't tell.
pub fn free_space(dir: &Path) -> Option<u64> {
    use windows::core::HSTRING;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let mut free = 0u64;
    // SAFETY: a valid NUL-terminated path and an out-pointer to a live u64.
    unsafe { GetDiskFreeSpaceExW(&HSTRING::from(dir), Some(&mut free), None, None) }.ok()?;
    Some(free)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_hex_has_the_requested_length_and_varies() {
        let a = random_hex(8);
        let b = random_hex(8);
        assert_eq!(a.len(), 16);
        assert_eq!(random_hex(4).len(), 8);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn sanitize_filename_cases() {
        assert_eq!(
            sanitize_filename("sa-mp-0.3.DL-R1-2-MP-install.exe"),
            "sa-mp-0.3.DL-R1-2-MP-install.exe"
        );
        assert_eq!(sanitize_filename(r"..\..\evil.exe"), ".._.._evil.exe");
        assert_eq!(sanitize_filename("a/b:c*d?e\"f<g>h|i"), "a_b_c_d_e_f_g_h_i");
        assert_eq!(sanitize_filename("  my   setup\t.exe  "), "my setup .exe");
        assert_eq!(sanitize_filename("bad\u{1}name"), "bad_name");
        assert_eq!(sanitize_filename(""), "sa-mp-installer.exe");
        assert_eq!(sanitize_filename("   "), "sa-mp-installer.exe");
        assert_eq!(sanitize_filename(".."), "sa-mp-installer.exe");
        assert_eq!(sanitize_filename("."), "sa-mp-installer.exe");
    }

    #[test]
    fn effectively_empty_ignores_os_noise() {
        let dir = tempfile::tempdir().unwrap();
        assert!(is_effectively_empty(dir.path()));
        fs::write(dir.path().join("Desktop.ini"), b"").unwrap();
        fs::write(dir.path().join("Thumbs.db"), b"").unwrap();
        assert!(is_effectively_empty(dir.path()));
        fs::write(dir.path().join("gta_sa.exe"), b"").unwrap();
        assert!(!is_effectively_empty(dir.path()));
        assert!(!is_effectively_empty(&dir.path().join("missing")));
    }

    #[test]
    fn write_probe_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        assert!(can_write_to_dir(dir.path()));
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
        assert!(!can_write_to_dir(&dir.path().join("missing")));
    }

    #[test]
    fn sha256_matches_known_vector() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("abc.txt");
        fs::write(&file, b"abc").unwrap();
        assert_eq!(
            sha256_file(&file).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn the_gta_temp_dir_is_persisted_and_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("store.json"));
        let first = ensure_temp_dir(&store).unwrap();
        assert!(first.is_dir());
        assert!(first
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("sarp-launcher-"));
        // Same folder is reused (resume support).
        assert_eq!(ensure_temp_dir(&store).unwrap(), first);
        clear_temp_dir(&store);
        assert!(!first.exists());
        assert_eq!(store.get().gta_install_temp_dir, None);
    }

    #[test]
    fn free_space_reads_the_volume() {
        let dir = tempfile::tempdir().unwrap();
        assert!(free_space(dir.path()).is_some_and(|free| free > 0));
        assert_eq!(free_space(&dir.path().join("missing")), None);
    }

    #[test]
    fn recognises_updater_leftovers() {
        let app = "San Andreas Roleplay - SA-MP Launcher";
        assert!(is_updater_leftover(
            "San Andreas Roleplay - SA-MP Launcher-1.1.0-updater-a1B2c3",
            app
        ));
        assert!(!is_updater_leftover(
            "San Andreas Roleplay - SA-MP Launcher-1.1.0-installer.exe",
            app
        ));
        assert!(!is_updater_leftover("Other App-1.0-updater-x", app));
        assert!(!is_updater_leftover("updater-x", app));
    }

    #[test]
    fn sweep_only_removes_old_launcher_entries() {
        let root = tempfile::tempdir().unwrap();
        let old_folder = root.path().join("sarp-launcher-old");
        let owned = root.path().join("sarp-launcher-owned");
        let package = root.path().join("sarp-cache-zip");
        let recent = root.path().join("sarp-launcher-recent");
        let old_script = root.path().join("sarp-copy-1.ps1");
        let foreign = root.path().join("other-app-old");
        let old_cache_zip = root.path().join("sarp-cache-0123abcd");
        let old_updater = root.path().join("SARP Test-1.1.0-updater-Ab12Cd");
        let new_updater = root.path().join("SARP Test-1.1.1-updater-Ef34Gh");
        let foreign_updater = root.path().join("Other-1.0.0-updater-Ab12Cd");
        for dir in [
            &old_folder,
            &owned,
            &package,
            &recent,
            &foreign,
            &old_cache_zip,
            &old_updater,
            &new_updater,
            &foreign_updater,
        ] {
            fs::create_dir(dir).unwrap();
        }
        fs::write(&old_script, b"").unwrap();
        fs::write(old_updater.join("SARP Test-1.1.0-installer.exe"), b"MZ").unwrap();

        let two_days_ago = SystemTime::now() - Duration::from_secs(48 * 3600);
        for path in [
            &old_folder,
            &owned,
            &package,
            &foreign,
            &old_script,
            &old_cache_zip,
            &old_updater,
            &foreign_updater,
        ] {
            use std::os::windows::fs::OpenOptionsExt;
            // FILE_FLAG_BACKUP_SEMANTICS lets us open directory handles too.
            let handle = fs::OpenOptions::new()
                .write(true)
                .custom_flags(0x0200_0000)
                .open(path)
                .unwrap();
            handle.set_modified(two_days_ago).unwrap();
        }

        sweep_temp(root.path(), &[&owned, &package], "SARP Test");
        assert!(!old_folder.exists());
        assert!(!old_script.exists());
        assert!(!old_cache_zip.exists());
        assert!(!old_updater.exists());
        assert!(owned.exists());
        assert!(package.exists(), "the cache package has its own sweep");
        assert!(recent.exists());
        assert!(foreign.exists());
        assert!(new_updater.exists(), "recent updater folders stay");
        assert!(foreign_updater.exists());
    }
}
