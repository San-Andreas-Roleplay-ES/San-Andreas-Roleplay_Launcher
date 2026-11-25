//! Parsers for SA-MP info and player-list response payloads.

use super::types::{ScoreboardEntry, ServerInfo};

const MAX_STRING_LENGTH: usize = 256;
const MAX_PLAYER_NAME_LENGTH: usize = 64;
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

pub fn parse_players(payload: &[u8]) -> Result<Vec<ScoreboardEntry>, String> {
    if payload.len() < 2 {
        return Err("response too short".into());
    }
    let count = u16::from_le_bytes([payload[0], payload[1]]) as usize;
    if count > MAX_PLAYERS as usize {
        return Err("too many players".into());
    }
    let mut players = Vec::with_capacity(count);
    let mut offset = 2;
    for _ in 0..count {
        let id = *payload
            .get(offset)
            .ok_or("buffer overflow reading player")?;
        let name_len = *payload
            .get(offset + 1)
            .ok_or("buffer overflow reading name length")? as usize;
        if name_len > MAX_PLAYER_NAME_LENGTH {
            return Err("name too long".into());
        }
        let name_start = offset + 2;
        let name_end = name_start + name_len;
        let name_bytes = payload
            .get(name_start..name_end)
            .ok_or("name out of bounds")?;
        let score_bytes = payload
            .get(name_end..name_end + 4)
            .ok_or("buffer overflow reading score")?;
        let ping_bytes = payload
            .get(name_end + 4..name_end + 8)
            .ok_or("buffer overflow reading ping")?;
        players.push(ScoreboardEntry {
            id,
            name: String::from_utf8_lossy(name_bytes).into_owned(),
            score: u32::from_le_bytes(score_bytes.try_into().expect("4 bytes")),
            ping: u32::from_le_bytes(ping_bytes.try_into().expect("4 bytes")),
        });
        offset = name_end + 8;
    }
    Ok(players)
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

    pub fn players_payload(players: &[(u8, &[u8], u32, u32)]) -> Vec<u8> {
        let mut buf = (players.len() as u16).to_le_bytes().to_vec();
        for (id, name, score, ping) in players {
            buf.push(*id);
            buf.push(name.len() as u8);
            buf.extend_from_slice(name);
            buf.extend_from_slice(&score.to_le_bytes());
            buf.extend_from_slice(&ping.to_le_bytes());
        }
        buf
    }

    #[test]
    fn parses_a_full_response() {
        // Invalid UTF-8 is replaced rather than making the whole response fail.
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

    #[test]
    fn parses_the_detailed_player_list() {
        let raw = players_payload(&[(0, b"Maradona", 42, 35), (7, b"Jugador\xf1", 12, 180)]);
        assert_eq!(
            parse_players(&raw).unwrap(),
            vec![
                ScoreboardEntry {
                    id: 0,
                    name: "Maradona".into(),
                    score: 42,
                    ping: 35,
                },
                ScoreboardEntry {
                    id: 7,
                    name: "Jugador\u{FFFD}".into(),
                    score: 12,
                    ping: 180,
                },
            ]
        );
        assert_eq!(parse_players(&[0, 0]).unwrap(), vec![]);
    }

    #[test]
    fn rejects_malformed_player_lists() {
        assert_eq!(parse_players(&[1]).unwrap_err(), "response too short");
        let mut truncated = players_payload(&[(1, b"Pepe", 3, 20)]);
        truncated.truncate(truncated.len() - 1);
        assert_eq!(
            parse_players(&truncated).unwrap_err(),
            "buffer overflow reading ping"
        );
        let mut long_name = (1u16).to_le_bytes().to_vec();
        long_name.extend_from_slice(&[1, 255]);
        long_name.extend(std::iter::repeat_n(b'a', 255));
        assert_eq!(parse_players(&long_name).unwrap_err(), "name too long");
    }
}
