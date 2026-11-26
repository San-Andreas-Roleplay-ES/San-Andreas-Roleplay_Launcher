//! Build-time settings baked in by `build.rs` from the repo-root `.env`.

use std::path::PathBuf;

/// Our API in release builds (`VITE_API_URL`). `build.rs` guarantees https.
const API_URL: &str = env!("VITE_API_URL");

/// Debug builds talk to a local API unless `.env` says otherwise.
const DEV_API_URL_DEFAULT: &str = "http://localhost:3333";

/// Sent as `X-Launcher-Key` to our API, and only to it. It ships inside the
/// binary, so it identifies launcher traffic rather than protecting anything;
/// still, it never goes to another host, into the logs or the webview.
pub const LAUNCHER_KEY: &str = env!("LAUNCHER_KEY");

/// Server cache folder under `Documents\GTA San Andreas User Files\SAMP\cache\`
/// (`VITE_GAME_CACHE_FOLDER`). `build.rs` guarantees it is a single folder name.
pub const CACHE_FOLDER: &str = env!("VITE_GAME_CACHE_FOLDER");

/// Debug builds play the role of the old "not packaged" mode: no shortcuts,
/// no updater, the `DEV_*` overrides honoured.
pub const IS_DEV: bool = cfg!(debug_assertions);

/// The updater's minisign public key (`plugins.updater.pubkey` in
/// tauri.conf.json: base64 of the key file), baked by `build.rs`.
const UPDATER_PUBKEY: &str = env!("SARP_UPDATER_PUBKEY");

/// A `DEV_*` setting, debug builds only. A value in the process environment
/// wins over the one baked from `.env`.
fn dev_setting(key: &str, baked: Option<&str>) -> Option<String> {
    if !IS_DEV {
        return None;
    }
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .or_else(|| baked.map(str::to_string))
}

/// `DEV_GTA_PATH`: the folder the GTA picker proposes in debug builds.
pub fn dev_gta_path() -> Option<String> {
    dev_setting("DEV_GTA_PATH", option_env!("SARP_DEV_GTA_PATH"))
}

/// `DEV_CACHE_DIR`: replaces the server cache folder in debug builds, so
/// development and QA never touch the real one.
pub fn dev_cache_dir() -> Option<PathBuf> {
    dev_setting("DEV_CACHE_DIR", option_env!("SARP_DEV_CACHE_DIR")).map(PathBuf::from)
}

/// `DEV_GTA_CANDIDATES`: `;`-separated folders that replace every real GTA
/// detection source in debug builds (a lone `;` means "nothing found").
pub fn dev_gta_candidates() -> Option<Vec<PathBuf>> {
    dev_setting("DEV_GTA_CANDIDATES", option_env!("SARP_DEV_GTA_CANDIDATES"))
        .map(|raw| split_list(&raw))
}

/// The key the mods catalog must be signed with: the updater's, the one the
/// release installers are signed with. Debug builds take `DEV_MODS_PUBKEY`
/// instead when set (the key itself, or the path of a file holding it), so QA
/// can sign test catalogs with a throwaway key.
pub fn mods_pubkey() -> String {
    let Some(dev) = dev_setting("DEV_MODS_PUBKEY", option_env!("SARP_DEV_MODS_PUBKEY")) else {
        return UPDATER_PUBKEY.to_string();
    };
    match std::fs::read_to_string(&dev) {
        Ok(text) => text.trim().to_string(),
        Err(_) => dev,
    }
}

fn split_list(raw: &str) -> Vec<PathBuf> {
    raw.split(';')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Base URL of our API, without a trailing slash: `VITE_API_URL` in release,
/// `DEV_API_URL` in debug builds.
pub fn api_url() -> String {
    let url = if IS_DEV {
        dev_setting("DEV_API_URL", option_env!("SARP_DEV_API_URL"))
            .unwrap_or_else(|| DEV_API_URL_DEFAULT.to_string())
    } else {
        API_URL.to_string()
    };
    url.trim_end_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_candidate_lists() {
        assert_eq!(
            split_list(r" C:\A ; ;D:\Games\GTA San Andreas;"),
            vec![
                PathBuf::from(r"C:\A"),
                PathBuf::from(r"D:\Games\GTA San Andreas")
            ]
        );
        // A lone separator: set, but empty (tests the "nothing found" flow).
        assert!(split_list(";").is_empty());
    }
}
