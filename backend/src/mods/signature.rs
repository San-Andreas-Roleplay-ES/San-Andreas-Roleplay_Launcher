//! The mods catalog's signature: minisign, exactly as the updater checks the
//! launcher installers (`tauri-plugin-updater`'s `verify_signature`): the
//! key is base64 of a minisign public key file, the signature is the text of
//! the `.sig` that `tauri signer sign` writes (base64 of a minisign signature
//! file). Only prehashed (`ED`) signatures pass, which is what the Tauri
//! signer produces.

use base64::Engine;
use minisign_verify::{PublicKey, Signature};

fn decode_text(b64: &str) -> Result<String, String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|e| format!("base64: {e}"))?;
    String::from_utf8(bytes).map_err(|_| "not UTF-8".to_string())
}

/// A minisign public key the way tauri.conf.json holds it (base64 of the key
/// file). For `DEV_MODS_PUBKEY`, the key file text itself, or its bare key
/// line, also work.
pub fn public_key(raw: &str) -> Result<PublicKey, String> {
    let raw = raw.trim();
    if let Ok(key) =
        decode_text(raw).and_then(|text| PublicKey::decode(&text).map_err(|e| e.to_string()))
    {
        return Ok(key);
    }
    if let Ok(key) = PublicKey::decode(raw) {
        return Ok(key);
    }
    PublicKey::from_base64(raw).map_err(|e| format!("public key: {e}"))
}

/// Checks `signature` over the exact bytes of `data`.
pub fn verify(data: &[u8], signature: &str, key: &PublicKey) -> Result<(), String> {
    let text = decode_text(signature).map_err(|e| format!("signature: {e}"))?;
    let signature = Signature::decode(&text).map_err(|e| format!("signature: {e}"))?;
    // `false`: legacy (non-prehashed) signatures are refused.
    key.verify(data, &signature, false)
        .map_err(|e| format!("signature: {e}"))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const TEST_KEY: &str = include_str!("fixtures/test.pub");
    pub const OTHER_KEY: &str = include_str!("fixtures/other.pub");
    pub const CATALOG_7: &str = include_str!("fixtures/catalog-7.json");
    pub const CATALOG_7_SIG: &str = include_str!("fixtures/catalog-7.json.sig");
    pub const CATALOG_5: &str = include_str!("fixtures/catalog-5.json");
    pub const CATALOG_5_SIG: &str = include_str!("fixtures/catalog-5.json.sig");
    pub const CATALOG_FUTURE: &str = include_str!("fixtures/catalog-future.json");
    pub const CATALOG_FUTURE_SIG: &str = include_str!("fixtures/catalog-future.json.sig");

    fn key() -> PublicKey {
        public_key(TEST_KEY).unwrap()
    }

    #[test]
    fn a_good_signature_verifies() {
        verify(CATALOG_7.as_bytes(), CATALOG_7_SIG, &key()).unwrap();
        verify(CATALOG_5.as_bytes(), CATALOG_5_SIG, &key()).unwrap();
        // Surrounding whitespace in the relayed `.sig` text is fine.
        verify(
            CATALOG_7.as_bytes(),
            &format!("\n{CATALOG_7_SIG}\r\n"),
            &key(),
        )
        .unwrap();
    }

    #[test]
    fn a_tampered_catalog_fails() {
        let tampered = CATALOG_7.replace("\"serial\": 7", "\"serial\": 8");
        assert_ne!(tampered, CATALOG_7);
        assert!(verify(tampered.as_bytes(), CATALOG_7_SIG, &key()).is_err());
        // Not even a whitespace change gets through: the bytes are signed.
        let reformatted = CATALOG_7.trim_end();
        assert!(verify(reformatted.as_bytes(), CATALOG_7_SIG, &key()).is_err());
        // Another signed catalog's signature does not fit this one.
        assert!(verify(CATALOG_7.as_bytes(), CATALOG_5_SIG, &key()).is_err());
    }

    #[test]
    fn a_tampered_signature_fails() {
        let text = decode_text(CATALOG_7_SIG).unwrap();
        let mut lines: Vec<String> = text.lines().map(String::from).collect();
        // Flip one character of the signature line, then of the trusted
        // comment (covered by the global signature).
        let flip = |line: &mut String, at: usize| {
            let mut chars: Vec<char> = line.chars().collect();
            chars[at] = if chars[at] == 'A' { 'B' } else { 'A' };
            *line = chars.into_iter().collect();
        };
        let mut bad_sig = lines.clone();
        flip(&mut bad_sig[1], 20);
        let mut bad_comment = lines.clone();
        bad_comment[2].push_str("\tversion:9.9.9");
        lines.truncate(2);
        let encode = |lines: &[String]| {
            base64::engine::general_purpose::STANDARD.encode(lines.join("\n") + "\n")
        };
        for sig in [encode(&bad_sig), encode(&bad_comment), encode(&lines)] {
            assert!(verify(CATALOG_7.as_bytes(), &sig, &key()).is_err());
        }
        for junk in ["", "not base64 !!", "aGVsbG8=", &CATALOG_7_SIG[..40]] {
            assert!(
                verify(CATALOG_7.as_bytes(), junk, &key()).is_err(),
                "{junk}"
            );
        }
    }

    #[test]
    fn the_wrong_key_fails() {
        let other = public_key(OTHER_KEY).unwrap();
        assert!(verify(CATALOG_7.as_bytes(), CATALOG_7_SIG, &other).is_err());
        // The real updater key never accepts the throwaway test signatures.
        let updater = public_key(env!("SARP_UPDATER_PUBKEY")).unwrap();
        assert!(verify(CATALOG_7.as_bytes(), CATALOG_7_SIG, &updater).is_err());
    }

    #[test]
    fn reads_the_key_formats() {
        // As in tauri.conf.json (and what `tauri signer generate` writes).
        let text = decode_text(TEST_KEY).unwrap();
        assert!(text.starts_with("untrusted comment: minisign public key"));
        // The decoded key file and its bare key line work too.
        let bare = text.lines().nth(1).unwrap();
        for raw in [TEST_KEY, text.as_str(), bare] {
            let key = public_key(raw).unwrap();
            verify(CATALOG_7.as_bytes(), CATALOG_7_SIG, &key).unwrap();
        }
        for bad in ["", "garbage", "dW50cnVzdGVk"] {
            assert!(public_key(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_baked_key_is_the_updaters() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../../tauri.conf.json")).unwrap();
        assert_eq!(
            env!("SARP_UPDATER_PUBKEY"),
            conf["plugins"]["updater"]["pubkey"].as_str().unwrap()
        );
        assert!(public_key(env!("SARP_UPDATER_PUBKEY")).is_ok());
    }
}
