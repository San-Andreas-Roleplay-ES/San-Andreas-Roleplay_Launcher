//! Payloads returned to the frontend (`ServerInfo` / `PingResult`).

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub hostname: String,
    pub players: u16,
    pub max_players: u16,
    pub gamemode: String,
    pub language: String,
    pub has_password: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PingResult {
    pub alive: bool,
    pub ms: Option<u64>,
    pub info: Option<ServerInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl PingResult {
    pub fn failed(error: impl Into<String>) -> Self {
        Self {
            alive: false,
            ms: None,
            info: None,
            error: Some(error.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ping_result_shape() {
        assert_eq!(
            serde_json::to_value(PingResult::failed("timeout")).unwrap(),
            json!({ "alive": false, "ms": null, "info": null, "error": "timeout" })
        );
    }

    #[test]
    fn server_info_uses_camel_case_keys() {
        let info = ServerInfo {
            hostname: "h".into(),
            players: 1,
            max_players: 2,
            gamemode: "g".into(),
            language: "l".into(),
            has_password: true,
        };
        assert_eq!(
            serde_json::to_value(info).unwrap(),
            json!({
                "hostname": "h", "players": 1, "maxPlayers": 2,
                "gamemode": "g", "language": "l", "hasPassword": true
            })
        );
    }
}
