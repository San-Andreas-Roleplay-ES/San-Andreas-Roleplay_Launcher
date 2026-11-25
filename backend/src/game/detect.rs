//! Finds the GTA: San Andreas installs already on this PC, so most players
//! never have to pick the folder.
//!
//! Everything here is read-only and failure-tolerant: registry keys are opened
//! with `KEY_READ`, files are only read, and a source that is missing, denied
//! or unreadable is simply skipped. No drive is ever scanned: each source
//! looks at a handful of known places.
//!
//! Each source yields raw paths; [`rank`] keeps the ones that pass the same
//! check as a picked folder, drops duplicates (case-insensitive, normalised)
//! and orders them. [`choose`] decides whether one can be adopted without
//! asking.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::os::windows::ffi::OsStringExt;
use std::path::{Component, Path, PathBuf};
use std::time::Instant;

use serde::Serialize;
use tauri::{AppHandle, Manager};
use windows_registry::{Key, CURRENT_USER, LOCAL_MACHINE};

use crate::config::dev_gta_candidates;
use crate::game::launcher::find_game_process;
use crate::game::paths::{self, find_missing, game_dir};
use crate::store::Store;
use crate::Ctx;

/// Where an install was found. Declared best first: the order ranks them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    /// `HKCU\Software\SAMP` → `gta_sa_exe`: the game samp.exe itself starts.
    Samp,
    /// A `gta_sa.exe` running right now.
    Running,
    /// The folder the previous launcher (1.0.x) had stored.
    OldLauncher,
    /// Rockstar's own install keys.
    Rockstar,
    Steam,
    /// An "Apps & features" entry.
    Uninstall,
    /// Windows' record of the programs run on this PC.
    History,
    /// A usual install location.
    Folder,
    /// `DEV_GTA_CANDIDATES` (debug builds only).
    Dev,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    pub path: String,
    /// The best source that found it.
    pub source: Source,
    /// `samp.exe` is in the folder.
    pub has_samp: bool,
    /// The folder the launcher uses now.
    pub current: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Detection {
    /// Adopted without asking: already stored as the game folder.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adopted: Option<Candidate>,
    /// Every valid install found, best first. Empty when the launcher already
    /// had a valid folder: nothing is probed then.
    pub candidates: Vec<Candidate>,
}

impl Detection {
    fn new(candidates: Vec<Candidate>) -> Self {
        Self {
            adopted: choose(&candidates).cloned(),
            candidates,
        }
    }
}

const USER_FILES: &str = "GTA San Andreas User Files";
const STEAM_GAME: &str = r"steamapps\common\Grand Theft Auto San Andreas";
/// Executables whose path in Windows' history points at a game folder.
const GAME_EXES: [&str; 2] = ["gta_sa.exe", "samp.exe"];
/// MuiCache value names are `<exe path>.<property>`.
const MUI_SUFFIXES: [&str; 2] = [".FriendlyAppName", ".ApplicationCompany"];
/// Config files we read are tiny; anything bigger is not what we look for.
const MAX_FILE: u64 = 1024 * 1024;

// ---- pure helpers ----

/// A path as stored in the registry or a config file: trimmed, unquoted
/// (`"C:\x\gta_sa.exe" -arg` → `C:\x\gta_sa.exe`), `/` turned into `\`, `.`
/// and `..` resolved. `None` unless it is absolute (`C:\...` or UNC).
pub fn clean(raw: &str) -> Option<PathBuf> {
    let raw = raw.trim();
    let raw = match raw.strip_prefix('"') {
        Some(quoted) => quoted.split('"').next().unwrap_or_default(),
        None => raw,
    }
    .trim();
    if raw.is_empty() || raw.contains('\0') {
        return None;
    }
    let raw = raw.replace('/', "\\");
    // `\\?\C:\...` (verbatim) names the same folder as `C:\...`.
    let raw = match raw.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => rest,
        _ => &raw,
    };
    let mut path = PathBuf::new();
    for component in Path::new(raw).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                path.pop();
            }
            other => path.push(other.as_os_str()),
        }
    }
    path.is_absolute().then_some(path)
}

/// The folder an `.exe` path is in; any other path as is.
pub fn folder_of(path: PathBuf) -> PathBuf {
    let is_exe = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"));
    match path.parent() {
        Some(parent) if is_exe => parent.to_path_buf(),
        _ => path,
    }
}

fn file_name_is(path: &Path, names: &[&str]) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| names.iter().any(|n| name.eq_ignore_ascii_case(n)))
}

fn strip_suffix_ignore_case<'a>(text: &'a str, suffix: &str) -> Option<&'a str> {
    let cut = text.len().checked_sub(suffix.len())?;
    text.get(cut..)
        .filter(|tail| tail.eq_ignore_ascii_case(suffix))
        .and_then(|_| text.get(..cut))
}

/// The game folder behind a value name of Windows' program history (an exe
/// path; `...\gta_sa.exe.FriendlyAppName` in MuiCache), when that exe is
/// GTA's or SA-MP's.
pub fn history_dir(name: &str) -> Option<PathBuf> {
    let exe = MUI_SUFFIXES
        .iter()
        .find_map(|suffix| strip_suffix_ignore_case(name, suffix))
        .unwrap_or(name);
    let path = clean(exe)?;
    file_name_is(&path, &GAME_EXES).then(|| folder_of(path))
}

/// What an "Apps & features" entry says about a San Andreas install: its
/// `InstallLocation`, or the folder of a `DisplayIcon` that is `gta_sa.exe`.
pub fn uninstall_dirs(name: &str, location: Option<&str>, icon: Option<&str>) -> Vec<PathBuf> {
    if !name.to_lowercase().contains("san andreas") {
        return Vec::new();
    }
    let mut dirs: Vec<PathBuf> = location.and_then(clean).into_iter().collect();
    let icon = icon.map(strip_icon_index).and_then(clean);
    if let Some(exe) = icon.filter(|exe| file_name_is(exe, &["gta_sa.exe"])) {
        dirs.push(folder_of(exe));
    }
    dirs
}

/// `C:\x\gta_sa.exe,0` → `C:\x\gta_sa.exe`.
fn strip_icon_index(raw: &str) -> &str {
    let raw = raw.trim();
    match raw.rsplit_once(',') {
        Some((path, index)) if index.trim().parse::<i32>().is_ok() => path,
        _ => raw,
    }
}

/// `gtaPath` from the 1.0.x launcher's `config.json`.
pub fn old_launcher_path(json: &[u8]) -> Option<PathBuf> {
    let json = json.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(json);
    let value: serde_json::Value = serde_json::from_slice(json).ok()?;
    clean(value.get("gtaPath")?.as_str()?)
}

#[derive(Debug, PartialEq)]
enum Token {
    Text(String),
    Open,
    Close,
}

/// Valve's KeyValues text: quoted (or bare) strings, braces, `//` comments.
fn vdf_tokens(text: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' => tokens.push(Token::Open),
            '}' => tokens.push(Token::Close),
            '"' => {
                let mut value = String::new();
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => match chars.next() {
                            Some('n') => value.push('\n'),
                            Some('t') => value.push('\t'),
                            Some(other) => value.push(other),
                            None => {}
                        },
                        other => value.push(other),
                    }
                }
                tokens.push(Token::Text(value));
            }
            '/' if chars.peek() == Some(&'/') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            c if c.is_whitespace() => {}
            c => {
                let mut value = c.to_string();
                while let Some(&next) = chars.peek() {
                    if next.is_whitespace() || matches!(next, '{' | '}' | '"') {
                        break;
                    }
                    value.push(next);
                    chars.next();
                }
                tokens.push(Token::Text(value));
            }
        }
    }
    tokens
}

/// The library folders in Steam's `libraryfolders.vdf`: each entry's `"path"`
/// or, in the old format, the value of each numbered entry.
pub fn library_folders(vdf: &str) -> Vec<PathBuf> {
    let mut folders = Vec::new();
    let mut depth = 0usize;
    let mut tokens = vdf_tokens(vdf).into_iter().peekable();
    while let Some(token) = tokens.next() {
        match token {
            Token::Open => depth += 1,
            Token::Close => depth = depth.saturating_sub(1),
            Token::Text(key) => match tokens.peek() {
                Some(Token::Open) => {
                    tokens.next();
                    depth += 1;
                }
                Some(Token::Text(_)) => {
                    let Some(Token::Text(value)) = tokens.next() else {
                        continue;
                    };
                    let numbered = !key.is_empty() && key.bytes().all(|b| b.is_ascii_digit());
                    let is_path = (depth == 2 && key.eq_ignore_ascii_case("path"))
                        || (depth == 1 && numbered);
                    if let Some(folder) = is_path.then(|| clean(&value)).flatten() {
                        folders.push(folder);
                    }
                }
                _ => {}
            },
        }
    }
    folders
}

/// Folders in `Documents` worth a look besides `GTA San Andreas`: the ones
/// next to the game's user files whose name starts with "GTA".
fn is_documents_sibling(name: &str) -> bool {
    name.get(..3)
        .is_some_and(|start| start.eq_ignore_ascii_case("gta"))
        && !name.eq_ignore_ascii_case(USER_FILES)
}

fn dedup_key(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

/// Keeps the folders that are valid installs, once each (case-insensitive on
/// the normalised path, with the best source that found it), best first: the
/// one SA-MP uses, then those with SA-MP installed, then by source and the
/// order found. Paths that don't exist or aren't installs are dropped.
pub fn rank(found: Vec<(PathBuf, Source)>, current: Option<&Path>) -> Vec<Candidate> {
    let current = current
        .and_then(|path| clean(&path.to_string_lossy()))
        .map(|path| dedup_key(&path));
    let mut unique: Vec<(PathBuf, Source)> = Vec::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    for (raw, source) in found {
        let Some(path) = clean(&raw.to_string_lossy()) else {
            continue;
        };
        let key = dedup_key(&path);
        match seen.get(&key) {
            Some(&i) => unique[i].1 = unique[i].1.min(source),
            None => {
                seen.insert(key, unique.len());
                unique.push((path, source));
            }
        }
    }
    let mut ranked: Vec<(usize, Candidate)> = unique
        .into_iter()
        .enumerate()
        .filter(|(_, (path, _))| find_missing(Some(path)).is_empty())
        .map(|(order, (path, source))| {
            let candidate = Candidate {
                has_samp: path.join("samp.exe").is_file(),
                current: current.as_deref() == Some(dedup_key(&path).as_str()),
                path: path.to_string_lossy().into_owned(),
                source,
            };
            (order, candidate)
        })
        .collect();
    ranked.sort_by_key(|(order, c)| (c.source != Source::Samp, !c.has_samp, c.source, *order));
    ranked.into_iter().map(|(_, c)| c).collect()
}

/// The install to adopt without asking: the one SA-MP itself starts, else
/// the only one found. `None`: several to choose from, or none at all.
pub fn choose(candidates: &[Candidate]) -> Option<&Candidate> {
    candidates
        .iter()
        .find(|c| c.source == Source::Samp)
        .or(match candidates {
            [only] => Some(only),
            _ => None,
        })
}

/// Detection only runs while the launcher has no valid folder: a stored one
/// that still checks out is never second-guessed.
pub fn needs_detection(store: &Store) -> bool {
    game_dir(store).is_none()
}

// ---- sources (read-only, each one skips what it cannot read) ----

/// Opened with `KEY_READ` only.
fn open(root: &Key, path: &str) -> Option<Key> {
    root.open(path).ok()
}

fn read_string(root: &Key, path: &str, name: &str) -> Option<String> {
    open(root, path)?.get_string(name).ok()
}

fn value_names(root: &Key, path: &str) -> Vec<String> {
    let Some(key) = open(root, path) else {
        return Vec::new();
    };
    let Ok(values) = key.values() else {
        return Vec::new();
    };
    values.map(|(name, _)| name).collect()
}

fn read_small(path: &Path) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .ok()?
        .take(MAX_FILE)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(bytes)
}

fn from_samp() -> Vec<PathBuf> {
    read_string(CURRENT_USER, r"Software\SAMP", "gta_sa_exe")
        .and_then(|exe| clean(&exe))
        .map(folder_of)
        .into_iter()
        .collect()
}

/// Full path of a process' image. Only asks for limited query rights.
fn process_path(pid: u32) -> Option<PathBuf> {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let mut buffer = vec![0u16; 32 * 1024];
    let mut len = buffer.len() as u32;
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let queried = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut len,
        );
        let _ = CloseHandle(process);
        queried.ok()?;
    }
    let path = std::ffi::OsString::from_wide(buffer.get(..len as usize)?);
    Some(PathBuf::from(path))
}

fn from_running() -> Vec<PathBuf> {
    find_game_process()
        .and_then(process_path)
        .map(folder_of)
        .into_iter()
        .collect()
}

fn from_old_launcher() -> Vec<PathBuf> {
    let Some(appdata) = std::env::var_os("APPDATA") else {
        return Vec::new();
    };
    let config = PathBuf::from(appdata)
        .join("san-andreas-roleplay-launcher")
        .join("config.json");
    read_small(&config)
        .and_then(|json| old_launcher_path(&json))
        .into_iter()
        .collect()
}

fn from_rockstar() -> Vec<PathBuf> {
    let keys: [(&Key, &str); 3] = [
        (
            LOCAL_MACHINE,
            r"SOFTWARE\WOW6432Node\Rockstar Games\GTA San Andreas\Installation",
        ),
        (
            LOCAL_MACHINE,
            r"SOFTWARE\Rockstar Games\GTA San Andreas\Installation",
        ),
        (
            CURRENT_USER,
            r"Software\Rockstar Games\GTA San Andreas\Installation",
        ),
    ];
    // Seen as an exe path in some keys and as a folder in others.
    let names = ["ExePath", "Installed Path"];
    keys.iter()
        .flat_map(|(root, path)| names.iter().map(move |name| (root, path, name)))
        .filter_map(|(root, path, name)| read_string(root, path, name))
        .filter_map(|raw| clean(&raw))
        .map(folder_of)
        .collect()
}

fn from_uninstall() -> Vec<PathBuf> {
    let roots: [(&Key, &str); 3] = [
        (
            LOCAL_MACHINE,
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
        ),
        (
            LOCAL_MACHINE,
            r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
        ),
        (
            CURRENT_USER,
            r"Software\Microsoft\Windows\CurrentVersion\Uninstall",
        ),
    ];
    let mut dirs = Vec::new();
    for (root, path) in roots {
        let Some(apps) = open(root, path) else {
            continue;
        };
        let Ok(names) = apps.keys() else { continue };
        for name in names {
            let Ok(app) = apps.open(&name) else { continue };
            let Ok(display) = app.get_string("DisplayName") else {
                continue;
            };
            let location = app.get_string("InstallLocation").ok();
            let icon = app.get_string("DisplayIcon").ok();
            dirs.extend(uninstall_dirs(
                &display,
                location.as_deref(),
                icon.as_deref(),
            ));
        }
    }
    dirs
}

fn from_steam() -> Vec<PathBuf> {
    let roots = [
        read_string(CURRENT_USER, r"Software\Valve\Steam", "SteamPath"),
        read_string(
            LOCAL_MACHINE,
            r"SOFTWARE\WOW6432Node\Valve\Steam",
            "InstallPath",
        ),
        read_string(LOCAL_MACHINE, r"SOFTWARE\Valve\Steam", "InstallPath"),
    ];
    let mut libraries = Vec::new();
    for root in roots.iter().flatten().filter_map(|raw| clean(raw)) {
        for file in [
            r"steamapps\libraryfolders.vdf",
            r"config\libraryfolders.vdf",
        ] {
            if let Some(bytes) = read_small(&root.join(file)) {
                libraries.extend(library_folders(&String::from_utf8_lossy(&bytes)));
            }
        }
        libraries.push(root);
    }
    libraries
        .into_iter()
        .map(|library| library.join(STEAM_GAME))
        .collect()
}

fn from_history() -> Vec<PathBuf> {
    const KEYS: [&str; 3] = [
        r"Software\Microsoft\Windows NT\CurrentVersion\AppCompatFlags\Compatibility Assistant\Store",
        r"Software\Microsoft\Windows NT\CurrentVersion\AppCompatFlags\Layers",
        r"Software\Classes\Local Settings\Software\Microsoft\Windows\Shell\MuiCache",
    ];
    KEYS.iter()
        .flat_map(|path| value_names(CURRENT_USER, path))
        .filter_map(|name| history_dir(&name))
        .collect()
}

/// The user's Documents folder as Windows resolves it (it may be redirected,
/// e.g. to OneDrive) — the same folder SA-MP writes its user files to.
fn known_documents() -> Option<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_Documents, SHGetKnownFolderPath, KF_FLAG_DEFAULT};

    unsafe {
        let raw = SHGetKnownFolderPath(&FOLDERID_Documents, KF_FLAG_DEFAULT, None).ok()?;
        let path = raw.to_string();
        CoTaskMemFree(Some(raw.0 as *const _));
        path.ok().map(PathBuf::from)
    }
}

/// `Documents\GTA San Andreas`, plus the "GTA..." folders next to the game's
/// user files (one level, nothing recursive).
fn documents_dirs(documents: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![documents.join("GTA San Andreas")];
    if !documents.join(USER_FILES).is_dir() {
        return dirs;
    }
    let Ok(entries) = fs::read_dir(documents) else {
        return dirs;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
        if is_dir && name.to_str().is_some_and(is_documents_sibling) {
            dirs.push(entry.path());
        }
    }
    dirs
}

fn from_folders() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = ["ProgramFiles(x86)", "ProgramFiles"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(|base| PathBuf::from(base).join(r"Rockstar Games\GTA San Andreas"))
        .collect();
    let profile_documents =
        std::env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join("Documents"));
    for documents in [known_documents(), profile_documents].into_iter().flatten() {
        dirs.extend(documents_dirs(&documents));
    }
    dirs
}

/// A source: the raw paths it can see.
type Probe = fn() -> Vec<PathBuf>;

/// Every real source, best first.
pub fn probe() -> Vec<(PathBuf, Source)> {
    let sources: [(Source, Probe); 8] = [
        (Source::Samp, from_samp),
        (Source::Running, from_running),
        (Source::OldLauncher, from_old_launcher),
        (Source::Rockstar, from_rockstar),
        (Source::Steam, from_steam),
        (Source::Uninstall, from_uninstall),
        (Source::History, from_history),
        (Source::Folder, from_folders),
    ];
    sources
        .into_iter()
        .flat_map(|(source, find)| find().into_iter().map(move |path| (path, source)))
        .collect()
}

/// The valid installs on this PC, best first. In debug builds
/// `DEV_GTA_CANDIDATES` replaces every real source. Blocking.
pub fn find(current: Option<&Path>) -> Vec<Candidate> {
    let started = Instant::now();
    let found = match dev_gta_candidates() {
        Some(folders) => folders.into_iter().map(|p| (p, Source::Dev)).collect(),
        None => probe(),
    };
    let raw = found.len();
    let candidates = rank(found, current);
    log::info!(
        "GTA detection: {} installs ({raw} paths seen) in {:?}",
        candidates.len(),
        started.elapsed()
    );
    for c in &candidates {
        log::info!(
            "  {} ({:?}{})",
            c.path,
            c.source,
            if c.has_samp { ", SA-MP" } else { "" }
        );
    }
    candidates
}

// ---- the app side ----

/// `find_game_dirs`: the installs found, marking the one in use.
pub async fn list(app: &AppHandle) -> Vec<Candidate> {
    let current = game_dir(&app.state::<Ctx>().store);
    tauri::async_runtime::spawn_blocking(move || find(current.as_deref()))
        .await
        .unwrap_or_default()
}

/// `detect_game_dir`, at startup: without a valid folder, looks for installs
/// and adopts the one SA-MP uses (or the only one found) exactly like a
/// folder picked by hand. With one, does nothing.
pub async fn detect(app: &AppHandle) -> Detection {
    if !needs_detection(&app.state::<Ctx>().store) {
        return Detection::default();
    }
    let candidates = tauri::async_runtime::spawn_blocking(|| find(None))
        .await
        .unwrap_or_default();
    let mut detection = Detection::new(candidates);
    if let Some(adopted) = detection.adopted.as_mut() {
        log::info!("adopting the detected GTA folder {}", adopted.path);
        paths::adopt(app, Path::new(&adopted.path)).await;
        adopted.current = true;
        let path = adopted.path.clone();
        for c in &mut detection.candidates {
            c.current = c.path == path;
        }
    }
    detection
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::paths::tests::fake_install;
    use serde_json::json;

    fn p(raw: &str) -> PathBuf {
        PathBuf::from(raw)
    }

    fn candidate(path: &str, source: Source, has_samp: bool) -> Candidate {
        Candidate {
            path: path.into(),
            source,
            has_samp,
            current: false,
        }
    }

    #[test]
    fn cleans_paths_from_the_registry() {
        assert_eq!(
            clean(r#"  "C:\Games\GTA San Andreas\gta_sa.exe"  "#),
            Some(p(r"C:\Games\GTA San Andreas\gta_sa.exe"))
        );
        // A quoted exe followed by arguments.
        assert_eq!(
            clean(r#""D:\GTA SA\gta_sa.exe" -window"#),
            Some(p(r"D:\GTA SA\gta_sa.exe"))
        );
        // Steam writes forward slashes, sometimes lower-case.
        assert_eq!(
            clean("c:/program files (x86)/steam"),
            Some(p(r"c:\program files (x86)\steam"))
        );
        // Trailing and doubled separators, `.` and `..`.
        assert_eq!(
            clean(r"C:\Games\\GTA\.\..\GTA San Andreas\"),
            Some(p(r"C:\Games\GTA San Andreas"))
        );
        assert_eq!(clean(r"C:\..\.."), Some(p(r"C:\")));
        assert_eq!(clean(r"\\?\C:\GTA"), Some(p(r"C:\GTA")));
        assert_eq!(clean(r"\\nas\games\GTA"), Some(p(r"\\nas\games\GTA")));
        for bad in [
            "",
            "   ",
            "\"\"",
            "gta_sa.exe",
            r"GTA\gta_sa.exe",
            r"C:GTA",
            r"\GTA",
            "C:\\GTA\0",
        ] {
            assert_eq!(clean(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn exe_paths_become_their_folder() {
        assert_eq!(folder_of(p(r"C:\GTA\gta_sa.exe")), p(r"C:\GTA"));
        assert_eq!(folder_of(p(r"C:\GTA\GTA_SA.EXE")), p(r"C:\GTA"));
        assert_eq!(folder_of(p(r"C:\GTA")), p(r"C:\GTA"));
        assert_eq!(
            folder_of(p(r"C:\GTA San Andreas")),
            p(r"C:\GTA San Andreas")
        );
    }

    #[test]
    fn reads_windows_program_history_names() {
        assert_eq!(
            history_dir(r"C:\Games\GTA San Andreas\gta_sa.exe"),
            Some(p(r"C:\Games\GTA San Andreas"))
        );
        assert_eq!(
            history_dir(r"D:\GTA\SAMP.EXE"),
            Some(p(r"D:\GTA")),
            "case-insensitive"
        );
        // MuiCache: `<exe>.FriendlyAppName` / `<exe>.ApplicationCompany`.
        assert_eq!(
            history_dir(r"C:\GTA SA\gta_sa.exe.FriendlyAppName"),
            Some(p(r"C:\GTA SA"))
        );
        assert_eq!(
            history_dir(r"C:\GTA SA\samp.exe.ApplicationCompany"),
            Some(p(r"C:\GTA SA"))
        );
        for other in [
            r"C:\Windows\notepad.exe",
            r"C:\GTA\gta_sa.exe.bak",
            r"C:\GTA\my_gta_sa.exe",
            r"C:\GTA\gta-sa.exe",
            r"C:\GTA\notepad.exe.FriendlyAppName",
            "gta_sa.exe",
            "SIGN.MEDIA=12AB gta_sa.exe",
            "@%SystemRoot%\\system32\\shell32.dll,-1",
            "",
        ] {
            assert_eq!(history_dir(other), None, "{other:?}");
        }
    }

    #[test]
    fn reads_uninstall_entries() {
        assert_eq!(
            uninstall_dirs(
                "Grand Theft Auto: San Andreas",
                Some(r"C:\Program Files (x86)\Rockstar Games\GTA San Andreas\"),
                None
            ),
            vec![p(r"C:\Program Files (x86)\Rockstar Games\GTA San Andreas")]
        );
        // No location, but the icon is the game's exe.
        assert_eq!(
            uninstall_dirs(
                "GTA SAN ANDREAS",
                Some(""),
                Some(r#""D:\GTA\gta_sa.exe",0"#)
            ),
            vec![p(r"D:\GTA")]
        );
        assert_eq!(
            uninstall_dirs("San Andreas", None, Some(r"D:\GTA\gta_sa.exe, -1")),
            vec![p(r"D:\GTA")]
        );
        // An icon that is not the game's exe says nothing.
        assert!(
            uninstall_dirs("San Andreas Multiplayer", None, Some(r"C:\x\uninstall.exe")).is_empty()
        );
        // Other programs are ignored, whatever they point at.
        assert!(uninstall_dirs("Grand Theft Auto V", Some(r"C:\GTAV"), None).is_empty());
    }

    #[test]
    fn reads_the_old_launcher_store() {
        let store = br#"{ "sampVersion": null, "gtaPath": "C:\\Program Files (x86)\\Rockstar Games\\GTA San Andreas (2)", "installedCacheSize": 802471674 }"#;
        assert_eq!(
            old_launcher_path(store),
            Some(p(
                r"C:\Program Files (x86)\Rockstar Games\GTA San Andreas (2)"
            ))
        );
        let mut bom = b"\xEF\xBB\xBF".to_vec();
        bom.extend_from_slice(br#"{"gtaPath":"D:\\GTA"}"#);
        assert_eq!(old_launcher_path(&bom), Some(p(r"D:\GTA")));
        for bad in [
            &br#"{"gtaPath":null}"#[..],
            br#"{"gtaPath":42}"#,
            br#"{"gtaPath":"relative\\GTA"}"#,
            br#"{}"#,
            br#"[]"#,
            b"{ not json",
            b"",
        ] {
            assert_eq!(
                old_launcher_path(bad),
                None,
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn reads_steam_library_folders() {
        let current = r#"
"libraryfolders"
{
	"0"
	{
		"path"		"C:\\Program Files (x86)\\Steam"
		"label"		""
		"apps"
		{
			"228980"		"493832728"
			"12120"		"4732187"
		}
	}
	"1"
	{
		"path"		"D:\\SteamLibrary"
		"label"		"Juegos"
	}
}
"#;
        assert_eq!(
            library_folders(current),
            vec![p(r"C:\Program Files (x86)\Steam"), p(r"D:\SteamLibrary")]
        );
        // Old format: numbered entries straight to the path.
        let old = r#"
"LibraryFolders"
{
	// a comment
	"TimeNextStatsReport"		"1612345678"
	"ContentStatsID"		"-123"
	"1"		"E:\\Games\\Steam"
}
"#;
        assert_eq!(library_folders(old), vec![p(r"E:\Games\Steam")]);
        for junk in ["", "{{{", "\"path\" \"C:\\\\x\"", "}}} \"1\" \"C:\\\\x\""] {
            assert!(library_folders(junk).is_empty(), "{junk:?}");
        }
    }

    #[test]
    fn documents_siblings() {
        assert!(is_documents_sibling("GTA San Andreas (2)"));
        assert!(is_documents_sibling("gta sa"));
        assert!(!is_documents_sibling("GTA San Andreas User Files"));
        assert!(!is_documents_sibling("Rockstar Games"));
        assert!(!is_documents_sibling("GT"));

        let docs = tempfile::tempdir().unwrap();
        let root = docs.path();
        // Without the user files folder only the usual name is tried.
        assert_eq!(documents_dirs(root), vec![root.join("GTA San Andreas")]);
        fs::create_dir_all(root.join(USER_FILES)).unwrap();
        fs::create_dir_all(root.join("GTA SA Mods")).unwrap();
        fs::create_dir_all(root.join("Other")).unwrap();
        fs::write(root.join("GTA notes.txt"), b"x").unwrap();
        let dirs = documents_dirs(root);
        assert_eq!(dirs[0], root.join("GTA San Andreas"));
        assert!(dirs.contains(&root.join("GTA SA Mods")));
        assert_eq!(dirs.len(), 2, "{dirs:?}");
    }

    #[test]
    fn ranks_only_valid_installs_once_each() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let (a, b, c) = (root.join("A"), root.join("B"), root.join("C"));
        for dir in [&a, &b, &c] {
            fs::create_dir_all(dir).unwrap();
            fake_install(dir);
        }
        fs::write(b.join("samp.exe"), b"").unwrap();
        let broken = root.join("Broken");
        fs::create_dir_all(&broken).unwrap();
        fs::write(broken.join("gta_sa.exe"), b"").unwrap();
        let a_upper = PathBuf::from(a.to_string_lossy().to_uppercase());
        let a_slashes = PathBuf::from(format!("{}/", a.to_string_lossy().replace('\\', "/")));

        let found = vec![
            (root.join("gone"), Source::Running),
            (c.clone(), Source::History),
            (a.clone(), Source::Folder),
            (broken.clone(), Source::Rockstar),
            (a_upper, Source::OldLauncher), // same as A: keeps the better source
            (b.join("samp.exe"), Source::Uninstall), // an exe path is not a folder
            (b.clone(), Source::Steam),
            (a_slashes, Source::Folder),
            (p("relative"), Source::Dev),
        ];
        let ranked = rank(found, Some(&c));
        let summary: Vec<(String, Source, bool, bool)> = ranked
            .iter()
            .map(|x| (x.path.clone(), x.source, x.has_samp, x.current))
            .collect();
        assert_eq!(
            summary,
            vec![
                // SA-MP installed first.
                (b.to_string_lossy().into(), Source::Steam, true, false),
                // Then by source.
                (
                    a.to_string_lossy().into(),
                    Source::OldLauncher,
                    false,
                    false
                ),
                (c.to_string_lossy().into(), Source::History, false, true),
            ]
        );
    }

    #[test]
    fn the_samp_install_ranks_first_even_without_samp_exe() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (tmp.path().join("A"), tmp.path().join("B"));
        for dir in [&a, &b] {
            fs::create_dir_all(dir).unwrap();
            fake_install(dir);
        }
        fs::write(b.join("samp.exe"), b"").unwrap();
        let ranked = rank(
            vec![
                (b.clone(), Source::Rockstar),
                (folder_of(a.join("gta_sa.exe")), Source::Samp),
            ],
            None,
        );
        assert_eq!(ranked[0].path, a.to_string_lossy());
        assert_eq!(ranked[0].source, Source::Samp);
        assert!(!ranked[0].has_samp);
        assert!(rank(Vec::new(), None).is_empty());
    }

    #[test]
    fn chooses_what_to_adopt() {
        let samp = candidate(r"C:\A", Source::Samp, true);
        let steam = candidate(r"C:\B", Source::Steam, true);
        let folder = candidate(r"C:\C", Source::Folder, false);
        // The install SA-MP starts wins over any number of others.
        let several = [samp.clone(), steam.clone(), folder.clone()];
        assert_eq!(choose(&several), Some(&samp));
        // A single install is adopted, whatever found it.
        assert_eq!(choose(std::slice::from_ref(&folder)), Some(&folder));
        // Several, none from SA-MP: ask.
        assert_eq!(choose(&[steam.clone(), folder.clone()]), None);
        // Nothing found.
        assert_eq!(choose(&[]), None);

        assert_eq!(
            Detection::new(vec![steam.clone(), folder.clone()]).adopted,
            None
        );
        assert_eq!(Detection::new(vec![folder.clone()]).adopted, Some(folder));
    }

    #[test]
    fn only_detects_without_a_valid_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().join("store.json"));
        assert!(needs_detection(&store), "nothing stored");
        let gta = tmp.path().join("GTA");
        store.update(|d| d.gta_path = Some(gta.to_string_lossy().into_owned()));
        assert!(needs_detection(&store), "stored, but not there");
        fs::create_dir_all(&gta).unwrap();
        fake_install(&gta);
        assert!(!needs_detection(&store), "stored and valid: left alone");
        fs::remove_file(gta.join("gta_sa.exe")).unwrap();
        assert!(needs_detection(&store), "broken since it was picked");
    }

    #[test]
    fn payload_shapes() {
        let detection = Detection::new(vec![Candidate {
            current: true,
            ..candidate(r"D:\GTA", Source::OldLauncher, true)
        }]);
        assert_eq!(
            serde_json::to_value(&detection).unwrap(),
            json!({
                "adopted": { "path": "D:\\GTA", "source": "old-launcher", "hasSamp": true, "current": true },
                "candidates": [{ "path": "D:\\GTA", "source": "old-launcher", "hasSamp": true, "current": true }]
            })
        );
        assert_eq!(
            serde_json::to_value(Detection::default()).unwrap(),
            json!({ "candidates": [] })
        );
        let sources = [
            Source::Samp,
            Source::Running,
            Source::OldLauncher,
            Source::Rockstar,
            Source::Steam,
            Source::Uninstall,
            Source::History,
            Source::Folder,
            Source::Dev,
        ];
        assert_eq!(
            serde_json::to_value(sources).unwrap(),
            json!([
                "samp",
                "running",
                "old-launcher",
                "rockstar",
                "steam",
                "uninstall",
                "history",
                "folder",
                "dev"
            ])
        );
    }

    /// Read-only look at this PC's real sources (never adopts anything):
    /// `cargo test real_detection -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_detection() {
        let found = probe();
        for (path, source) in &found {
            println!("seen   {source:?}: {}", path.display());
        }
        for c in rank(found, None) {
            println!(
                "valid  {:?}: {} (samp.exe: {})",
                c.source, c.path, c.has_samp
            );
        }
    }
}
