//! Opening links in the user's browser. Only `https:` URLs are accepted;
//! the frontend routes every external link (site, UCP, event URLs coming
//! from the API) through here.

use tauri::{AppHandle, Url};
use tauri_plugin_opener::OpenerExt;

/// The parsed URL when it may be opened externally.
pub fn allowed(url: &str) -> Option<Url> {
    let parsed = Url::parse(url).ok()?;
    (parsed.scheme() == "https" && parsed.host_str().is_some_and(|h| !h.is_empty()))
        .then_some(parsed)
}

/// Opens `url` in the default browser when it is `https:`; anything else is
/// ignored (logged).
pub fn open(app: &AppHandle, url: &str) {
    let Some(parsed) = allowed(url) else {
        log::warn!("blocked external URL: {url}");
        return;
    };
    if let Err(error) = app.opener().open_url(parsed.as_str(), None::<&str>) {
        log::warn!("could not open {parsed}: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_only() {
        for ok in [
            "https://sarp.es",
            "https://ucp.sarp.es/auth/signup",
            "https://www.youtube.com/watch?v=x",
            "https://API.SARP.ES/path",
        ] {
            assert!(allowed(ok).is_some(), "{ok}");
        }
        for bad in [
            "http://sarp.es",
            "file:///C:/Windows/System32/calc.exe",
            "javascript:alert(1)",
            "ms-settings:privacy",
            "steam://run/12210",
            "\\\\server\\share\\evil.exe",
            "C:\\Windows\\System32\\calc.exe",
            "not a url",
            "",
        ] {
            assert!(allowed(bad).is_none(), "{bad}");
        }
    }
}
