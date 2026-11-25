//! One UDP request/response against a SA-MP server.

use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::Duration;

use tokio::net::UdpSocket;

use super::packet::{build, Opcode, HEADER_LEN};

const MAX_RESPONSE_BYTES: usize = 4096;
pub const MAX_PLAYERS_RESPONSE_BYTES: usize = 16_384;

/// Sends one query and returns the response payload (header stripped).
pub async fn query(
    octets: [u8; 4],
    port: u16,
    opcode: Opcode,
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    query_with_limit(octets, port, opcode, timeout, MAX_RESPONSE_BYTES).await
}

pub async fn query_with_limit(
    octets: [u8; 4],
    port: u16,
    opcode: Opcode,
    timeout: Duration,
    max_response_bytes: usize,
) -> Result<Vec<u8>, String> {
    let exchange = async {
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
            .await
            .map_err(|e| e.to_string())?;
        // Connected sockets discard datagrams from hosts other than the server.
        socket
            .connect(SocketAddrV4::new(Ipv4Addr::from(octets), port))
            .await
            .map_err(|e| e.to_string())?;
        socket
            .send(&build(octets, port, opcode))
            .await
            .map_err(|e| e.to_string())?;
        // One extra byte lets us detect a reply over the configured limit.
        let mut buf = vec![0u8; max_response_bytes + 1];
        let n = socket.recv(&mut buf).await.map_err(|e| e.to_string())?;
        buf.truncate(n);
        Ok::<_, String>(buf)
    };
    let msg = tokio::time::timeout(timeout, exchange)
        .await
        .map_err(|_| "timeout".to_string())??;
    if msg.len() > max_response_bytes {
        return Err("response too large".into());
    }
    if msg.len() < HEADER_LEN {
        return Err("response too short".into());
    }
    Ok(msg[HEADER_LEN..].to_vec())
}
