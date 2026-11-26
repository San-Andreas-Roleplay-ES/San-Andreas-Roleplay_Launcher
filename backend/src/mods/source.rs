//! Where the catalog comes from: `GET /api/public/launcher/mods` relays the
//! signed file from the bucket as `{ catalog, signature }`. Nothing reads it
//! before its signature checks out; then its schema and its `serial` (never
//! lower than the highest one accepted) decide whether it replaces the one
//! saved in the store. Whenever the API's can't be used, the saved one (its
//! signature checked again) stands in.

use std::sync::Arc;
use std::time::Duration;

use minisign_verify::PublicKey;
use serde::Deserialize;

use super::catalog::{parse, Catalog, Refused};
use super::record::SavedCatalog;
use super::signature::verify;
use crate::net::api::{self, Api, BAD_RESPONSE};
use crate::store::Store;

/// The catalog is small, and a saved one can stand in: a down API never
/// holds the Mods row (and Play) up for long.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Why the API's catalog was not used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    /// No answer, an error status, or a body that isn't the envelope.
    Unreachable,
    /// The signature does not check out (or is not one).
    BadSignature,
    /// Signed, but an older `serial` than one already accepted.
    Rollback,
    /// Signed, but not a catalog this launcher can read.
    Invalid,
    /// Signed for a newer schema: the launcher needs an update.
    TooNew,
}

/// Where the catalog in use came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The API's, accepted just now.
    Live,
    /// The saved one, because of `problem` (or because the API says none is
    /// published: an unsigned "nothing" never drops a signed catalog).
    Saved,
    /// No mods published, and none saved.
    Empty,
    /// No catalog at all: see `problem`.
    Missing,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Loaded {
    pub catalog: Option<Arc<Catalog>>,
    pub origin: Origin,
    pub problem: Option<Problem>,
}

/// What the API answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Remote {
    /// Nothing published yet (`catalog` and `signature` null).
    None,
    Signed {
        catalog: String,
        signature: String,
    },
}

#[derive(Deserialize)]
struct Payload {
    #[serde(default)]
    catalog: Option<String>,
    #[serde(default)]
    signature: Option<String>,
}

/// The API's answer. Half an answer (one of the two missing) is an error.
pub fn remote(payload: Option<serde_json::Value>) -> Result<Remote, String> {
    let Some(payload) = payload else {
        return Ok(Remote::None);
    };
    // serde would read a struct from an array too.
    if !payload.is_object() {
        return Err(BAD_RESPONSE.into());
    }
    let payload: Payload = serde_json::from_value(payload).map_err(|_| BAD_RESPONSE)?;
    match (payload.catalog, payload.signature) {
        (None, None) => Ok(Remote::None),
        (Some(catalog), Some(signature)) if !catalog.is_empty() && !signature.trim().is_empty() => {
            Ok(Remote::Signed { catalog, signature })
        }
        _ => Err(BAD_RESPONSE.into()),
    }
}

pub async fn fetch(api: &Api) -> Result<Remote, String> {
    remote(api.get_optional(api::MODS, TIMEOUT).await?)
}

/// Signature, then schema and shape. Never parses unsigned text.
fn check(text: &str, signature: &str, key: &PublicKey, https: bool) -> Result<Catalog, Problem> {
    verify(text.as_bytes(), signature, key).map_err(|error| {
        log::error!("mods catalog refused: {error}");
        Problem::BadSignature
    })?;
    parse(text, https).map_err(|refused| match refused {
        Refused::TooNew(schema) => {
            log::warn!("mods catalog has schemaVersion {schema}: the launcher needs an update");
            Problem::TooNew
        }
        Refused::Invalid(error) => {
            log::error!("mods catalog is signed but unreadable: {error}");
            Problem::Invalid
        }
    })
}

/// The saved catalog, if it still checks out. One that doesn't (another
/// key, a hand-edited store) is dropped.
pub fn saved(store: &Store, key: &PublicKey, https: bool) -> Option<Catalog> {
    let saved = store.get().mods_catalog?;
    match check(&saved.catalog, &saved.signature, key, https) {
        Ok(catalog) => Some(catalog),
        Err(problem) => {
            log::warn!("saved mods catalog dropped ({problem:?})");
            store.update(|d| d.mods_catalog = None);
            None
        }
    }
}

/// Decides which catalog to use given what the API answered. Accepting one
/// saves it and raises the stored serial.
pub fn accept(
    answer: Result<Remote, String>,
    store: &Store,
    key: &PublicKey,
    https: bool,
) -> Loaded {
    let fallback = |problem: Problem| match saved(store, key, https) {
        Some(catalog) => Loaded {
            catalog: Some(Arc::new(catalog)),
            origin: Origin::Saved,
            problem: Some(problem),
        },
        None => Loaded {
            catalog: None,
            origin: Origin::Missing,
            problem: Some(problem),
        },
    };
    let (text, signature) = match answer {
        Err(error) => {
            log::warn!("mods catalog unavailable: {error}");
            return fallback(Problem::Unreachable);
        }
        Ok(Remote::None) => {
            return match saved(store, key, https) {
                Some(catalog) => {
                    log::warn!("the API publishes no mods catalog; keeping the saved one");
                    Loaded {
                        catalog: Some(Arc::new(catalog)),
                        origin: Origin::Saved,
                        problem: None,
                    }
                }
                None => Loaded {
                    catalog: None,
                    origin: Origin::Empty,
                    problem: None,
                },
            };
        }
        Ok(Remote::Signed { catalog, signature }) => (catalog, signature),
    };
    let catalog = match check(&text, &signature, key, https) {
        Ok(catalog) => catalog,
        Err(problem) => return fallback(problem),
    };
    let highest = store.get().mods_serial.unwrap_or(0);
    if catalog.serial < highest {
        log::error!(
            "mods catalog refused: serial {} is older than {highest}",
            catalog.serial
        );
        return fallback(Problem::Rollback);
    }
    let fresh = SavedCatalog {
        catalog: text,
        signature,
    };
    let data = store.get();
    if data.mods_catalog.as_ref() != Some(&fresh) || data.mods_serial != Some(catalog.serial) {
        log::info!("mods catalog {} accepted", catalog.serial);
        store.update(|d| {
            d.mods_serial = Some(highest.max(catalog.serial));
            d.mods_catalog = Some(fresh);
        });
    }
    Loaded {
        catalog: Some(Arc::new(catalog)),
        origin: Origin::Live,
        problem: None,
    }
}

pub async fn load(api: &Api, store: &Store, key: &PublicKey, https: bool) -> Loaded {
    accept(fetch(api).await, store, key, https)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mods::signature::public_key;
    use crate::mods::signature::tests::*;
    use crate::net::test_server::{Fault, TestServer};
    use serde_json::json;

    fn key() -> PublicKey {
        public_key(TEST_KEY).unwrap()
    }

    fn envelope(payload: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "message": "OK", "httpCode": 200, "source": "launcher", "payload": payload
        }))
        .unwrap()
    }

    fn signed(catalog: &str, signature: &str) -> Result<Remote, String> {
        Ok(Remote::Signed {
            catalog: catalog.into(),
            signature: signature.into(),
        })
    }

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("store.json"));
        (dir, store)
    }

    #[test]
    fn reads_the_api_payload() {
        assert_eq!(remote(None), Ok(Remote::None));
        assert_eq!(
            remote(Some(json!({ "catalog": null, "signature": null }))),
            Ok(Remote::None)
        );
        assert_eq!(remote(Some(json!({}))), Ok(Remote::None));
        assert_eq!(
            remote(Some(
                json!({ "catalog": "{}", "signature": "sig", "extra": 1 })
            )),
            signed("{}", "sig")
        );
        for half in [
            json!({ "catalog": "{}", "signature": null }),
            json!({ "catalog": null, "signature": "sig" }),
            json!({ "catalog": "", "signature": "sig" }),
            json!({ "catalog": {}, "signature": "sig" }),
            json!([]),
        ] {
            assert!(remote(Some(half.clone())).is_err(), "{half}");
        }
    }

    #[test]
    fn a_good_catalog_is_accepted_and_saved() {
        let (_dir, store) = store();
        let loaded = accept(signed(CATALOG_7, CATALOG_7_SIG), &store, &key(), true);
        assert_eq!(loaded.origin, Origin::Live);
        assert_eq!(loaded.problem, None);
        assert_eq!(loaded.catalog.unwrap().serial, 7);
        let data = store.get();
        assert_eq!(data.mods_serial, Some(7));
        assert_eq!(data.mods_catalog.unwrap().catalog, CATALOG_7);
    }

    #[test]
    fn tampered_or_foreign_catalogs_are_refused() {
        let (_dir, store) = store();
        let tampered = CATALOG_7.replace("0.3.7", "0.3.8");
        let other = public_key(OTHER_KEY).unwrap();
        for (answer, key) in [
            (signed(&tampered, CATALOG_7_SIG), key()),
            (signed(CATALOG_7, CATALOG_5_SIG), key()),
            (signed(CATALOG_7, "garbage"), key()),
            (signed(CATALOG_7, CATALOG_7_SIG), other),
        ] {
            let loaded = accept(answer, &store, &key, true);
            assert_eq!(loaded.origin, Origin::Missing);
            assert_eq!(loaded.problem, Some(Problem::BadSignature));
            assert_eq!(loaded.catalog, None);
        }
        assert_eq!(store.get().mods_catalog, None, "nothing refused is saved");
        assert_eq!(store.get().mods_serial, None);
    }

    #[test]
    fn an_older_serial_is_a_rollback() {
        let (_dir, store) = store();
        accept(signed(CATALOG_7, CATALOG_7_SIG), &store, &key(), true);
        let loaded = accept(signed(CATALOG_5, CATALOG_5_SIG), &store, &key(), true);
        assert_eq!(loaded.problem, Some(Problem::Rollback));
        assert_eq!(loaded.origin, Origin::Saved);
        assert_eq!(loaded.catalog.unwrap().serial, 7, "the saved one stands in");
        assert_eq!(store.get().mods_serial, Some(7));
        // Even with the saved catalog gone, the serial stays: still refused.
        store.update(|d| d.mods_catalog = None);
        let loaded = accept(signed(CATALOG_5, CATALOG_5_SIG), &store, &key(), true);
        assert_eq!(
            (loaded.origin, loaded.problem),
            (Origin::Missing, Some(Problem::Rollback))
        );
        // The same serial again is fine.
        let again = accept(signed(CATALOG_7, CATALOG_7_SIG), &store, &key(), true);
        assert_eq!(again.origin, Origin::Live);
    }

    #[test]
    fn a_newer_schema_falls_back() {
        let (_dir, store) = store();
        let loaded = accept(
            signed(CATALOG_FUTURE, CATALOG_FUTURE_SIG),
            &store,
            &key(),
            true,
        );
        assert_eq!(
            (loaded.origin, loaded.problem),
            (Origin::Missing, Some(Problem::TooNew))
        );
        accept(signed(CATALOG_7, CATALOG_7_SIG), &store, &key(), true);
        let loaded = accept(
            signed(CATALOG_FUTURE, CATALOG_FUTURE_SIG),
            &store,
            &key(),
            true,
        );
        assert_eq!(
            (loaded.origin, loaded.problem),
            (Origin::Saved, Some(Problem::TooNew))
        );
        assert_eq!(
            store.get().mods_serial,
            Some(7),
            "not accepted, serial unchanged"
        );
    }

    #[test]
    fn nothing_published_never_drops_a_signed_catalog() {
        let (_dir, store) = store();
        let loaded = accept(Ok(Remote::None), &store, &key(), true);
        assert_eq!((loaded.origin, loaded.catalog), (Origin::Empty, None));
        accept(signed(CATALOG_7, CATALOG_7_SIG), &store, &key(), true);
        let loaded = accept(Ok(Remote::None), &store, &key(), true);
        assert_eq!((loaded.origin, loaded.problem), (Origin::Saved, None));
        assert!(loaded.catalog.is_some());
    }

    #[test]
    fn the_saved_catalog_is_checked_again_when_loaded() {
        let (_dir, store) = store();
        accept(signed(CATALOG_7, CATALOG_7_SIG), &store, &key(), true);
        let down = || Err::<Remote, _>("down".to_string());
        let loaded = accept(down(), &store, &key(), true);
        assert_eq!(
            (loaded.origin, loaded.problem),
            (Origin::Saved, Some(Problem::Unreachable))
        );
        // Tampered in the store: dropped, not used.
        store.update(|d| {
            d.mods_catalog.as_mut().unwrap().catalog = CATALOG_7.replace("Radar", "Radar!")
        });
        let loaded = accept(down(), &store, &key(), true);
        assert_eq!((loaded.origin, loaded.catalog), (Origin::Missing, None));
        assert_eq!(store.get().mods_catalog, None);
        // Verified with another key (a new launcher key): dropped too.
        accept(signed(CATALOG_7, CATALOG_7_SIG), &store, &key(), true);
        let other = public_key(OTHER_KEY).unwrap();
        assert_eq!(accept(down(), &store, &other, true).catalog, None);
    }

    #[tokio::test]
    async fn over_http_with_the_launcher_key_and_fallbacks() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let server = TestServer::start().await;
        let api = Api::new(crate::net::http::strict_client("test"), server.base.clone());
        let (_dir, store) = store();

        server.put(
            api::MODS,
            envelope(json!({ "catalog": CATALOG_7, "signature": CATALOG_7_SIG })),
        );
        let loaded = load(&api, &store, &key(), true).await;
        assert_eq!(loaded.origin, Origin::Live);
        let request = &server.requests()[0];
        assert!(request.headers.contains_key("x-launcher-key"));

        for fault in [Fault::Status(500), Fault::Status(401), Fault::Status(404)] {
            server.fault(api::MODS, Some(fault));
            let loaded = load(&api, &store, &key(), true).await;
            assert_eq!(
                (loaded.origin, loaded.problem),
                (Origin::Saved, Some(Problem::Unreachable))
            );
        }
        server.clear_faults();
        server.put(api::MODS, b"<html>not the API</html>".to_vec());
        assert_eq!(
            load(&api, &store, &key(), true).await.problem,
            Some(Problem::Unreachable)
        );
        server.put(
            api::MODS,
            envelope(json!({ "catalog": null, "signature": null })),
        );
        assert_eq!(load(&api, &store, &key(), true).await.origin, Origin::Saved);
        // An envelope without payload (falsy keys dropped) is "nothing".
        server.put(api::MODS, br#"{"message":"OK","httpCode":200}"#.to_vec());
        let (_fresh_dir, fresh) = self::store();
        assert_eq!(load(&api, &fresh, &key(), true).await.origin, Origin::Empty);
    }
}
