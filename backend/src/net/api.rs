//! Our API (`VITE_API_URL`, or `DEV_API_URL` in debug builds): the
//! launcher-only endpoints under `/api/public/launcher/*`. Every request
//! carries `X-Launcher-Key`; nothing else this app sends does.

use std::time::Duration;

use reqwest::header::ACCEPT;
use reqwest::Client;
use serde_json::Value;

use crate::config::LAUNCHER_KEY;
use crate::net::http::{net_error, read_body, SLOW};

pub const MANIFEST: &str = "/api/public/launcher/manifest";
pub const CACHE: &str = "/api/public/launcher/cache";
pub const GLOBAL_METRICS: &str = "/api/public/launcher/global-metrics";
pub const EVENTS: &str = "/api/public/launcher/events";

const KEY_HEADER: &str = "X-Launcher-Key";
/// Metrics and events: small, and the UI only waits for them in the background.
const DATA_TIMEOUT: Duration = Duration::from_secs(15);
/// Longest wait for the next piece of a response body.
const BODY_STALL: Duration = Duration::from_secs(30);

pub const BAD_RESPONSE: &str = "Respuesta inesperada del servidor.";

pub struct Api {
    /// Never follows redirects (see [`crate::net::http::strict_client`]).
    client: Client,
    base: String,
}

impl Api {
    /// `base` without a trailing slash, e.g. `https://api.gta-rol.com`.
    pub fn new(client: Client, base: String) -> Self {
        Self { client, base }
    }

    /// The no-redirect client, without the key: the cache sync downloads
    /// files with it so they can only come from the `baseUrl` it was given.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// GETs `path` and returns the envelope's `payload`. `timeout` bounds the
    /// wait for the response; the body may take as long as it keeps arriving
    /// (the cache list is a few hundred KB, minutes on a bad line).
    pub async fn get(&self, path: &str, timeout: Duration) -> Result<Value, String> {
        let url = format!("{}{path}", self.base);
        let response = tokio::time::timeout(
            timeout,
            self.client
                .get(&url)
                .header(ACCEPT, "application/json")
                .header(KEY_HEADER, LAUNCHER_KEY)
                .send(),
        )
        .await
        .map_err(|_| {
            log::warn!("API {path}: no response after {timeout:?}");
            SLOW.to_string()
        })?
        .map_err(|e| net_error(&e))?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            if status == 401 {
                log::error!("API refused the launcher key on {path}");
            } else {
                log::warn!("API {path}: HTTP {status}");
            }
            return Err(format!(
                "El servidor respondió con un error (HTTP {status})."
            ));
        }
        let body = read_body(response, BODY_STALL)
            .await
            .inspect_err(|error| log::warn!("API {path}: body: {error}"))?;
        payload(&body).inspect_err(|_| log::warn!("API {path}: unexpected body"))
    }

    /// Counters for the header (`onlineAccountCounter`, ...), passed through.
    pub async fn global_metrics(&self) -> Result<Value, String> {
        let value = self.get(GLOBAL_METRICS, DATA_TIMEOUT).await?;
        if value.is_object() {
            Ok(value)
        } else {
            Err(BAD_RESPONSE.into())
        }
    }

    /// Upcoming events as the API sends them; the frontend normalises them.
    pub async fn events(&self) -> Result<Value, String> {
        let value = self.get(EVENTS, DATA_TIMEOUT).await?;
        if value.is_array() {
            Ok(value)
        } else {
            Err(BAD_RESPONSE.into())
        }
    }
}

/// Unwraps `{ message, httpCode, source, payload }`. The API drops falsy keys
/// from the envelope, so a missing `payload` means there was none.
pub fn payload(body: &[u8]) -> Result<Value, String> {
    let mut envelope: Value = serde_json::from_slice(body).map_err(|_| BAD_RESPONSE)?;
    match envelope.get_mut("payload").map(Value::take) {
        Some(payload) if !payload.is_null() => Ok(payload),
        _ => Err(BAD_RESPONSE.into()),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unwraps_the_envelope() {
        let body = br#"{"message":"OK","httpCode":200,"source":"launcher","payload":{"a":1}}"#;
        assert_eq!(payload(body).unwrap(), json!({ "a": 1 }));
        let list = br#"{"message":"OK","httpCode":200,"source":"events","payload":[]}"#;
        assert_eq!(payload(list).unwrap(), json!([]));
    }

    #[test]
    fn rejects_bodies_without_a_payload() {
        for body in [
            &br#"{"message":"unauthorized","httpCode":401}"#[..],
            br#"{"message":"OK","httpCode":200,"payload":null}"#,
            br#"[1,2]"#,
            b"<!DOCTYPE html><pre>Cannot GET</pre>",
            b"",
        ] {
            assert_eq!(payload(body).unwrap_err(), BAD_RESPONSE);
        }
    }

    // ---- Contract tests against a running API (`cargo test -- --ignored`).
    // Debug builds use `DEV_API_URL` (`http://localhost:3333` by default).

    pub fn real_api() -> Api {
        let _ = rustls::crypto::ring::default_provider().install_default();
        Api::new(
            crate::net::http::strict_client("test"),
            crate::config::api_url(),
        )
    }

    #[tokio::test]
    #[ignore = "needs the API"]
    async fn real_metrics_and_events_parse() {
        let api = real_api();
        let metrics = api.global_metrics().await.unwrap();
        assert!(metrics["accountCounter"].is_number(), "{metrics}");
        assert!(metrics["onlineAccountCounter"].is_number(), "{metrics}");

        let events = api.events().await.unwrap();
        for event in events.as_array().unwrap() {
            assert!(event["id"].is_number(), "{event}");
            assert!(event["title"].is_string(), "{event}");
            for key in ["description", "image_url", "event_date"] {
                assert!(
                    event[key].is_string() || event[key].is_null(),
                    "{key} in {event}"
                );
            }
        }
    }

    #[tokio::test]
    #[ignore = "needs the API"]
    async fn real_api_requires_the_key() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let base = crate::config::api_url();
        for path in [MANIFEST, CACHE, GLOBAL_METRICS, EVENTS] {
            let status = crate::net::http::strict_client("test")
                .get(format!("{base}{path}"))
                .send()
                .await
                .unwrap()
                .status();
            assert_eq!(status.as_u16(), 401, "{path}");
        }
    }
}
