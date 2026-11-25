//! Launcher manifest, served live by our API (`/api/public/launcher/manifest`):
//! current SA-MP files and installer, and the GTA rip. The files themselves
//! are downloaded from the CDN URLs it lists. (The server cache has its own
//! per-file list: see `install::cache`.)

use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::net::api::{self, Api};
use crate::store::Store;

const SUPPORTED_SCHEMA_VERSION: u64 = 1;
/// Generous: the API's first build after a restart hashes files in S3 (~6 s).
const FETCH_TIMEOUT: Duration = Duration::from_secs(25);
/// With a saved manifest to fall back on: one attempt, enough for that first
/// build, so a down API never holds the checks (and Play) up for long.
const QUICK_TIMEOUT: Duration = Duration::from_secs(10);
const RETRY_DELAYS: [Duration; 3] = if cfg!(test) {
    [
        Duration::from_millis(5),
        Duration::from_millis(5),
        Duration::from_millis(5),
    ]
} else {
    [
        Duration::from_millis(1000),
        Duration::from_millis(2000),
        Duration::from_millis(4000),
    ]
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestFileEntry {
    pub path: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestInstallerEntry {
    pub filename: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
    pub silent_args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestGtaRipEntry {
    pub filename: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SampSection {
    pub files: Vec<ManifestFileEntry>,
    pub installer: ManifestInstallerEntry,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GtaSection {
    pub rip: ManifestGtaRipEntry,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LauncherManifest {
    pub schema_version: u64,
    pub generated_at: String,
    pub samp: SampSection,
    pub gta: GtaSection,
}

const BAD_SHAPE: &str = "Manifest payload has unexpected shape";

/// The structural checks of the old `isValidManifest`.
fn has_expected_shape(value: &Value) -> bool {
    let obj = |v: &Value| v.is_object();
    obj(value)
        && value["schemaVersion"].is_number()
        && value["generatedAt"].is_string()
        && obj(&value["samp"])
        && value["samp"]["files"].is_array()
        && obj(&value["samp"]["installer"])
        && obj(&value["gta"])
        && obj(&value["gta"]["rip"])
}

/// Validates a downloaded payload and turns it into the typed manifest.
pub fn parse(value: Value) -> Result<LauncherManifest, String> {
    if !has_expected_shape(&value) {
        return Err(BAD_SHAPE.into());
    }
    let version = value["schemaVersion"].as_f64().unwrap_or_default();
    if version > SUPPORTED_SCHEMA_VERSION as f64 {
        return Err(format!(
            "Manifest schemaVersion {} is newer than supported ({SUPPORTED_SCHEMA_VERSION}). Update the launcher.",
            value["schemaVersion"]
        ));
    }
    // Stricter than the old shape check: every field the launcher reads must
    // be present with the right type.
    serde_json::from_value(value).map_err(|error| {
        log::warn!("manifest rejected: {error}");
        BAD_SHAPE.to_string()
    })
}

async fn fetch_once(api: &Api, timeout: Duration) -> Result<LauncherManifest, String> {
    parse(api.get(api::MANIFEST, timeout).await?)
}

/// Keeps `manifest` in the store so a later session can verify SA-MP while
/// the API is down. Written only when it changed.
pub fn save(store: &Store, manifest: &LauncherManifest) {
    if store.get().manifest.as_ref() != Some(manifest) {
        store.update(|d| d.manifest = Some(manifest.clone()));
    }
}

/// In-memory copy of the last good manifest, shared by every command.
#[derive(Default)]
pub struct ManifestCache(Mutex<Option<LauncherManifest>>);

impl ManifestCache {
    pub fn get(&self) -> Option<LauncherManifest> {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn set(&self, manifest: Option<LauncherManifest>) {
        *self.0.lock().unwrap_or_else(|p| p.into_inner()) = manifest;
    }

    /// Fetches with retries (1 s, 2 s, 4 s) and caches the result.
    pub async fn fetch(&self, api: &Api) -> Result<LauncherManifest, String> {
        let mut last_error = String::from("Failed to fetch manifest");
        for attempt in 0..=RETRY_DELAYS.len() {
            match fetch_once(api, FETCH_TIMEOUT).await {
                Ok(manifest) => {
                    self.set(Some(manifest.clone()));
                    return Ok(manifest);
                }
                Err(error) => {
                    log::warn!("manifest fetch attempt {} failed: {error}", attempt + 1);
                    last_error = error;
                    if let Some(delay) = RETRY_DELAYS.get(attempt) {
                        tokio::time::sleep(*delay).await;
                    }
                }
            }
        }
        Err(last_error)
    }

    /// Cached manifest, or a fresh fetch when there is none yet.
    pub async fn ensure(&self, api: &Api) -> Result<LauncherManifest, String> {
        match self.get() {
            Some(manifest) => Ok(manifest),
            None => self.fetch(api).await,
        }
    }

    /// Drops the cached copy and fetches again.
    pub async fn refresh(&self, api: &Api) -> Result<LauncherManifest, String> {
        self.set(None);
        self.fetch(api).await
    }

    /// The manifest to verify and install SA-MP with: the cached one, else a
    /// fresh fetch (saved to `store` for next time), else — API down,
    /// restarting, answering 401 — the last one saved. Errors only when there
    /// has never been one. A fallback is not cached: the next call asks the
    /// API again.
    pub async fn ensure_saved(&self, api: &Api, store: &Store) -> Result<LauncherManifest, String> {
        if let Some(manifest) = self.get() {
            return Ok(manifest);
        }
        let saved = store
            .get()
            .manifest
            .filter(|m| m.schema_version <= SUPPORTED_SCHEMA_VERSION);
        let fetched = match saved {
            Some(_) => fetch_once(api, QUICK_TIMEOUT)
                .await
                .inspect(|manifest| self.set(Some(manifest.clone()))),
            None => self.fetch(api).await,
        };
        match (fetched, saved) {
            (Ok(manifest), _) => {
                save(store, &manifest);
                Ok(manifest)
            }
            (Err(error), Some(saved)) => {
                log::warn!(
                    "manifest unavailable ({error}); using the one saved from {}",
                    saved.generated_at
                );
                Ok(saved)
            }
            (Err(error), None) => Err(error),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::net::test_server::Fault;
    use serde_json::json;

    pub fn sample() -> Value {
        json!({
            "schemaVersion": 1,
            "generatedAt": "2026-11-24T18:10:00.818Z",
            "samp": {
                "files": [{
                    "path": "samp.exe",
                    "url": "https://sarp-public.b-cdn.net/samp/samp.exe",
                    "sha256": "b21351fbcd10ffa0f47a453bc2d193cc820919dba7240cdda7a32c21bb21fb14",
                    "size": 413696
                }],
                "installer": {
                    "filename": "sa-mp-0.3.DL-R1-2-MP-install.exe",
                    "url": "https://sarp-public.b-cdn.net/samp/sa-mp-0.3.DL-R1-2-MP-install.exe",
                    "sha256": "84de7e605f1ea9e19ba631b88e4be7a864be866dd33aac64d404d08f732bde6e",
                    "size": 24396480,
                    "silentArgs": ["/S"]
                }
            },
            "gta": { "rip": {
                "filename": "GTA_RIP.zip",
                "url": "https://sarp-public.b-cdn.net/gta/GTA_RIP.zip",
                "sha256": "2e6d493ef85a383e5ef2083cc4212f5991ae4e1da00320764e2a10bf8c5a621c",
                "size": 959115057
            }}
        })
    }

    #[test]
    fn accepts_and_round_trips_a_valid_manifest() {
        let manifest = parse(sample()).unwrap();
        assert_eq!(manifest.samp.installer.silent_args, vec!["/S"]);
        assert_eq!(manifest.gta.rip.size, 959_115_057);
        // Serialises back to exactly the TS shape.
        assert_eq!(serde_json::to_value(&manifest).unwrap(), sample());
    }

    #[test]
    fn the_old_cache_section_is_ignored() {
        // Older manifests (the CDN JSON) still carry `cache.zip`: not an error,
        // and it does not reach the frontend.
        let mut value = sample();
        value["cache"] = json!({ "zip": { "filename": "x.zip", "url": "https://x", "size": 1 } });
        let manifest = parse(value).unwrap();
        assert_eq!(serde_json::to_value(&manifest).unwrap(), sample());
    }

    #[test]
    fn parses_the_api_envelope() {
        let body = serde_json::to_vec(&json!({
            "message": "OK", "httpCode": 200, "source": "launcher", "payload": sample()
        }))
        .unwrap();
        let manifest = parse(api::payload(&body).unwrap()).unwrap();
        assert_eq!(manifest.schema_version, 1);
    }

    #[test]
    fn rejects_wrong_shapes() {
        let broken = |path: &[&str], replacement: Value| {
            let mut value = sample();
            let mut cursor = &mut value;
            for key in &path[..path.len() - 1] {
                cursor = &mut cursor[*key];
            }
            cursor[path[path.len() - 1]] = replacement;
            parse(value).unwrap_err()
        };
        assert_eq!(broken(&["schemaVersion"], json!("1")), BAD_SHAPE);
        assert_eq!(broken(&["generatedAt"], json!(5)), BAD_SHAPE);
        assert_eq!(broken(&["samp", "files"], json!({})), BAD_SHAPE);
        assert_eq!(broken(&["samp", "installer"], Value::Null), BAD_SHAPE);
        assert_eq!(broken(&["gta", "rip"], json!([])), BAD_SHAPE);
        assert_eq!(
            broken(&["samp", "installer", "size"], json!("big")),
            BAD_SHAPE
        );
        // Passes the old shape check but misses a field we read.
        assert_eq!(broken(&["gta", "rip", "sha256"], Value::Null), BAD_SHAPE);
        assert_eq!(parse(json!([])).unwrap_err(), BAD_SHAPE);
    }

    #[test]
    fn rejects_newer_schema_versions() {
        let mut value = sample();
        value["schemaVersion"] = json!(2);
        assert_eq!(
            parse(value).unwrap_err(),
            "Manifest schemaVersion 2 is newer than supported (1). Update the launcher."
        );
    }

    fn envelope(payload: Value) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "message": "OK", "httpCode": 200, "source": "launcher", "payload": payload
        }))
        .unwrap()
    }

    fn api_with(server: &crate::net::test_server::TestServer) -> Api {
        let _ = rustls::crypto::ring::default_provider().install_default();
        Api::new(crate::net::http::strict_client("test"), server.base.clone())
    }

    #[tokio::test]
    async fn a_fetched_manifest_is_saved_for_later() {
        let server = crate::net::test_server::TestServer::start().await;
        server.put(api::MANIFEST, envelope(sample()));
        let api = api_with(&server);
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("store.json"));

        let cache = ManifestCache::default();
        let manifest = cache.ensure_saved(&api, &store).await.unwrap();
        assert_eq!(manifest, parse(sample()).unwrap());
        assert_eq!(store.get().manifest, Some(manifest.clone()));
        // Cached in memory: no second request.
        cache.ensure_saved(&api, &store).await.unwrap();
        assert_eq!(server.hits(api::MANIFEST), 1);
    }

    #[tokio::test]
    async fn a_down_api_falls_back_to_the_saved_manifest() {
        let server = crate::net::test_server::TestServer::start().await;
        let api = api_with(&server);
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("store.json"));
        let saved = parse(sample()).unwrap();
        store.update(|d| d.manifest = Some(saved.clone()));

        for status in [500, 401, 404] {
            server.forget_requests();
            server.fault(api::MANIFEST, Some(Fault::Status(status)));
            let cache = ManifestCache::default();
            assert_eq!(cache.ensure_saved(&api, &store).await.unwrap(), saved);
            // One quick try, no retries: something to fall back on.
            assert_eq!(server.hits(api::MANIFEST), 1, "HTTP {status}");
            // The fallback is not cached: the API is asked again next time.
            assert_eq!(cache.get(), None);
        }

        // Back up with a newer manifest: used and saved.
        server.clear_faults();
        let mut newer = sample();
        newer["generatedAt"] = json!("2026-12-01T00:00:00.000Z");
        server.put(api::MANIFEST, envelope(newer.clone()));
        let cache = ManifestCache::default();
        let fresh = cache.ensure_saved(&api, &store).await.unwrap();
        assert_eq!(fresh.generated_at, "2026-12-01T00:00:00.000Z");
        assert_eq!(store.get().manifest, Some(fresh));
    }

    #[tokio::test]
    async fn without_a_saved_manifest_a_down_api_is_an_error() {
        let server = crate::net::test_server::TestServer::start().await;
        server.fault(api::MANIFEST, Some(Fault::Status(500)));
        let api = api_with(&server);
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("store.json"));
        // A saved one from a newer schema can't stand in either.
        let mut future = parse(sample()).unwrap();
        future.schema_version = SUPPORTED_SCHEMA_VERSION + 1;
        store.update(|d| d.manifest = Some(future));

        let cache = ManifestCache::default();
        assert!(cache.ensure_saved(&api, &store).await.is_err());
        // Nothing to fall back on: the full retry schedule.
        assert_eq!(server.hits(api::MANIFEST), RETRY_DELAYS.len() + 1);
    }

    /// Contract test against a running API (`DEV_API_URL` in debug builds).
    #[tokio::test]
    #[ignore = "needs the API"]
    async fn fetches_the_real_manifest() {
        let api = crate::net::api::tests::real_api();
        let cache = ManifestCache::default();
        let manifest = cache.fetch(&api).await.unwrap();
        assert_eq!(manifest.schema_version, 1);
        assert!(manifest.samp.files.iter().any(|f| f.path == "samp.exe"));
        assert!(manifest
            .samp
            .files
            .iter()
            .all(|f| f.url.starts_with("https://")));
        assert!(manifest.gta.rip.size > 100_000_000);
        assert_eq!(cache.get(), Some(manifest.clone()));
        assert_eq!(cache.ensure(&api).await.unwrap(), manifest);
    }
}
