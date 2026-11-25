//! Bakes the build-time settings from the repo-root `.env` into the binary
//! (`env!("VITE_API_URL")`, ...), the same way Vite bakes them into the
//! frontend bundle. Variables already set in the process environment win over
//! the file, matching Vite's `loadEnv` precedence (handy for CI).

use std::collections::HashMap;
use std::path::Path;

const ENV_FILE: &str = "../.env";
const REQUIRED: [&str; 3] = ["VITE_API_URL", "VITE_GAME_CACHE_FOLDER", "LAUNCHER_KEY"];
/// Debug builds only: baked as `SARP_<KEY>` and never into a release binary.
const DEV_ONLY: [&str; 4] = [
    "DEV_GTA_PATH",
    "DEV_API_URL",
    "DEV_CACHE_DIR",
    "DEV_GTA_CANDIDATES",
];

fn main() {
    println!("cargo:rerun-if-changed={ENV_FILE}");
    for key in REQUIRED.iter().chain(DEV_ONLY.iter()) {
        println!("cargo:rerun-if-env-changed={key}");
    }

    let file = read_env_file(Path::new(ENV_FILE));
    let lookup = |key: &str| -> Option<String> {
        std::env::var(key)
            .ok()
            .or_else(|| file.get(key).cloned())
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };

    let mut baked: Vec<(String, String)> = Vec::new();
    for key in REQUIRED {
        let Some(value) = lookup(key) else {
            panic!(
                "\n\n{key} is not set.\nCreate the repo-root .env (copy .env.example) or export \
                 {key} before building.\n\n"
            );
        };
        validate(key, &value);
        baked.push((key.to_string(), value));
    }

    // Dev-only conveniences: never bake a developer's local paths or API into
    // a release binary.
    if std::env::var("PROFILE").as_deref() == Ok("debug") {
        for key in DEV_ONLY {
            if let Some(value) = lookup(key) {
                validate(key, &value);
                baked.push((format!("SARP_{key}"), value));
            }
        }
    }

    tauri_build::build();

    // Printed last: when a build script fails, cargo dumps everything it
    // printed, and that must never include LAUNCHER_KEY.
    for (key, value) in baked {
        println!("cargo:rustc-env={key}={value}");
    }
}

fn read_env_file(path: &Path) -> HashMap<String, String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let line = line.strip_prefix("export ").unwrap_or(line);
            let (key, value) = line.split_once('=')?;
            Some((key.trim().to_string(), unquote(value.trim()).to_string()))
        })
        .collect()
}

fn unquote(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|v| v.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

fn validate(key: &str, value: &str) {
    match key {
        "VITE_API_URL" => {
            // Release builds talk to it, so it must be TLS.
            if !value.starts_with("https://") {
                panic!("\n\nVITE_API_URL must be an https:// URL (got {value:?}).\n\n");
            }
        }
        "DEV_API_URL" => {
            if !value.starts_with("https://") && !value.starts_with("http://") {
                panic!("\n\nDEV_API_URL must be an http(s):// URL (got {value:?}).\n\n");
            }
        }
        "LAUNCHER_KEY" => {
            // Sent as an HTTP header value. Never echo it back.
            if !value.chars().all(|c| c.is_ascii_graphic()) {
                panic!("\n\nLAUNCHER_KEY must be printable ASCII without spaces.\n\n");
            }
        }
        "VITE_GAME_CACHE_FOLDER" => {
            // It names the single folder the cache sync manages (and deletes
            // stale files from), so it must never be able to point anywhere
            // else.
            let bad = value == "."
                || value == ".."
                || value
                    .chars()
                    .any(|c| matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'));
            if bad {
                panic!(
                    "\n\nVITE_GAME_CACHE_FOLDER must be a single folder name (got {value:?}).\n\n"
                );
            }
        }
        _ => {}
    }
}
