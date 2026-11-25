//! SA-MP server queries over UDP for status and the scoreboard.

mod client;
mod packet;
mod parser;
mod types;

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub use types::{PingResult, ScoreboardSnapshot};

const QUERY_TIMEOUT: Duration = Duration::from_millis(1500);

/// Rebuilds a validated `ip:port` endpoint for launching `samp.exe`.
pub fn endpoint(ip: &str, port: f64) -> Option<String> {
    let [a, b, c, d] = packet::parse_ipv4(ip)?;
    let port = packet::parse_port(port)?;
    Some(format!("{a}.{b}.{c}.{d}:{port}"))
}

/// Pings an IPv4 endpoint with an info query.
pub async fn ping(ip: &str, port: f64) -> PingResult {
    let (Some(octets), Some(port)) = (packet::parse_ipv4(ip), packet::parse_port(port)) else {
        return PingResult::failed("invalid host");
    };
    let start = Instant::now();
    match client::query(octets, port, packet::Opcode::Info, QUERY_TIMEOUT)
        .await
        .and_then(|payload| parser::parse_info(&payload))
    {
        Ok(info) => PingResult {
            alive: true,
            ms: Some(start.elapsed().as_millis() as u64),
            info: Some(info),
            error: None,
        },
        Err(error) => PingResult::failed(error),
    }
}

pub async fn scoreboard(ip: &str, port: f64) -> Result<ScoreboardSnapshot, String> {
    let (Some(octets), Some(port)) = (packet::parse_ipv4(ip), packet::parse_port(port)) else {
        return Err("invalid host".into());
    };
    let (info, detailed) = tokio::join!(
        client::query(octets, port, packet::Opcode::Info, QUERY_TIMEOUT),
        client::query_with_limit(
            octets,
            port,
            packet::Opcode::Detailed,
            QUERY_TIMEOUT,
            client::MAX_PLAYERS_RESPONSE_BYTES,
        ),
    );
    let info = parser::parse_info(&info?)?;
    let players = parser::parse_players(&detailed?)?;
    let queried_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    Ok(ScoreboardSnapshot {
        players,
        total_players: info.players,
        max_players: info.max_players,
        queried_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_the_launch_endpoint() {
        // Normalise the address from the validated octets.
        assert_eq!(
            endpoint("135.148.128.72", 7777.0).as_deref(),
            Some("135.148.128.72:7777")
        );
        assert_eq!(
            endpoint("010.001.0.255", 1.0).as_deref(),
            Some("10.1.0.255:1")
        );
        for (ip, port) in [
            ("135.148.128.72 -c rcon", 7777.0),
            ("135.148.128.72:7777", 7777.0),
            ("evil.example", 7777.0),
            ("\"1.2.3.4\"", 7777.0),
            ("1.2.3.4", 0.0),
            ("", 7777.0),
        ] {
            assert_eq!(endpoint(ip, port), None, "{ip:?} {port}");
        }
    }

    #[tokio::test]
    async fn rejects_invalid_hosts_without_sending() {
        assert_eq!(
            ping("not-an-ip", 7777.0).await,
            PingResult::failed("invalid host")
        );
        assert_eq!(
            ping("127.0.0.1", 0.0).await,
            PingResult::failed("invalid host")
        );
        assert_eq!(
            ping("127.0.0.1", 7777.5).await,
            PingResult::failed("invalid host")
        );
    }

    #[tokio::test]
    async fn round_trip_against_a_local_fake_server() {
        let server = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let port = server.local_addr().unwrap().port();
        tokio::spawn(async move {
            let mut buf = [0u8; 64];
            let (n, from) = server.recv_from(&mut buf).await.unwrap();
            assert_eq!(
                &buf[..n],
                &packet::build([127, 0, 0, 1], port, packet::Opcode::Info)
            );
            let mut reply = buf[..n].to_vec();
            reply.extend(parser::tests::payload(
                0,
                42,
                100,
                &[b"Test Server", b"RP", b"es"],
            ));
            server.send_to(&reply, from).await.unwrap();
        });
        let result = ping("127.0.0.1", f64::from(port)).await;
        assert!(result.alive, "{result:?}");
        assert_eq!(result.info.unwrap().hostname, "Test Server");
        assert!(result.ms.is_some());
        assert_eq!(result.error, None);
    }

    #[tokio::test]
    async fn times_out_when_nobody_answers() {
        // A bound but silent socket verifies the query timeout without networking.
        let silent = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let port = silent.local_addr().unwrap().port();
        let result = ping("127.0.0.1", f64::from(port)).await;
        assert_eq!(result, PingResult::failed("timeout"));
        drop(silent);
    }

    #[tokio::test]
    async fn reads_the_scoreboard_from_a_local_fake_server() {
        let server = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let port = server.local_addr().unwrap().port();
        tokio::spawn(async move {
            let mut buf = [0u8; 64];
            for _ in 0..2 {
                let (n, from) = server.recv_from(&mut buf).await.unwrap();
                let request = buf[..n].to_vec();
                let mut reply = request.clone();
                match request[10] {
                    b'i' => {
                        reply.extend(parser::tests::payload(0, 2, 50, &[b"Test", b"SARP", b"es"]))
                    }
                    b'd' => reply.extend(parser::tests::players_payload(&[
                        (0, b"Maradona", 42, 35),
                        (1, b"Messi", 30, 60),
                    ])),
                    other => panic!("unexpected opcode {other}"),
                }
                server.send_to(&reply, from).await.unwrap();
            }
        });
        let snapshot = scoreboard("127.0.0.1", f64::from(port)).await.unwrap();
        assert_eq!((snapshot.total_players, snapshot.max_players), (2, 50));
        assert_eq!(snapshot.players.len(), 2);
        assert_eq!(snapshot.players[0].name, "Maradona");
        assert_eq!(snapshot.players[1].ping, 60);
        assert!(snapshot.queried_at > 0);
    }

    #[tokio::test]
    async fn rejects_invalid_scoreboard_hosts() {
        assert_eq!(
            scoreboard("not-an-ip", 7777.0).await.unwrap_err(),
            "invalid host"
        );
    }

    #[tokio::test]
    #[ignore = "network"]
    async fn pings_the_real_server() {
        let result = ping("135.148.128.72", 7777.0).await;
        assert!(result.alive, "{result:?}");
        let info = result.info.unwrap();
        assert!(info.max_players > 0);
        println!(
            "{} — {}/{} players, {} ms",
            info.hostname,
            info.players,
            info.max_players,
            result.ms.unwrap()
        );
    }
}
