//! Parser for the SA-MP `i` (info) response payload (header already stripped).

use super::types::ServerInfo;

const MAX_STRING_LENGTH: usize = 256;
const MAX_PLAYERS: u16 = 10_000;

fn read_string(buf: &[u8], offset: usize) -> Result<(String, usize), String> {
    let len_bytes = buf
        .get(offset..offset + 4)
        .ok_or("buffer overflow reading length")?;
    let len = u32::from_le_bytes(len_bytes.try_into().expect("4 bytes")) as usize;
    if len > MAX_STRING_LENGTH {
        return Err("string too long".into());
    }
    let start = offset + 4;
    let bytes = buf.get(start..start + len).ok_or("string out of bounds")?;
    Ok((String::from_utf8_lossy(bytes).into_owned(), start + len))
}

pub fn parse_info(payload: &[u8]) -> Result<ServerInfo, String> {
    if payload.len() < 5 {
        return Err("response too short".into());
    }
    let has_password = payload[0] == 1;
    let players = u16::from_le_bytes([payload[1], payload[2]]);
    let max_players = u16::from_le_bytes([payload[3], payload[4]]);
    let (hostname, next) = read_string(payload, 5)?;
    let (gamemode, next) = read_string(payload, next)?;
    let (language, _) = read_string(payload, next)?;
    Ok(ServerInfo {
        hostname,
        players: players.min(MAX_PLAYERS),
        max_players: max_players.min(MAX_PLAYERS),
        gamemode,
        language,
        has_password,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn payload(password: u8, players: u16, max: u16, strings: &[&[u8]]) -> Vec<u8> {
        let mut buf = vec![password];
        buf.extend_from_slice(&players.to_le_bytes());
        buf.extend_from_slice(&max.to_le_bytes());
        for s in strings {
            buf.extend_from_slice(&(s.len() as u32).to_le_bytes());
            buf.extend_from_slice(s);
        }
        buf
    }

    #[test]
    fn parses_a_full_response() {
        let info = parse_info(&payload(
            0,
            123,
            500,
            &[b"San Andreas Roleplay", b"SARP v3", b"Espa\xf1ol"],
        ))
        .unwrap();
        assert_eq!(
            info,
            ServerInfo {
                hostname: "San Andreas Roleplay".into(),
                players: 123,
                max_players: 500,
                gamemode: "SARP v3".into(),
                // Invalid UTF-8 is replaced, not fatal.
                language: "Espa\u{FFFD}ol".into(),
                has_password: false,
            }
        );
        assert!(
            parse_info(&payload(1, 0, 0, &[b"", b"", b""]))
                .unwrap()
                .has_password
        );
    }

    #[test]
    fn caps_player_counts() {
        let info = parse_info(&payload(0, 65000, 60000, &[b"", b"", b""])).unwrap();
        assert_eq!((info.players, info.max_players), (10_000, 10_000));
    }

    #[test]
    fn rejects_malformed_payloads() {
        assert_eq!(parse_info(&[0, 1, 0]).unwrap_err(), "response too short");
        let mut truncated = payload(0, 1, 2, &[b"host", b"mode", b"lang"]);
        truncated.truncate(truncated.len() - 2);
        assert_eq!(parse_info(&truncated).unwrap_err(), "string out of bounds");
        let missing_len = payload(0, 1, 2, &[b"host"]);
        assert_eq!(
            parse_info(&missing_len).unwrap_err(),
            "buffer overflow reading length"
        );
        let mut huge = payload(0, 1, 2, &[]);
        huge.extend_from_slice(&1000u32.to_le_bytes());
        assert_eq!(parse_info(&huge).unwrap_err(), "string too long");
    }
}
