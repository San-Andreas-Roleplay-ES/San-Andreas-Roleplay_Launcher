//! SA-MP query packet: `SAMP`, IPv4 octets, little-endian port, and opcode.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Opcode {
    Info,
    Players,
    Detailed,
    Rules,
}

impl Opcode {
    fn byte(self) -> u8 {
        match self {
            Opcode::Info => b'i',
            Opcode::Players => b'c',
            Opcode::Detailed => b'd',
            Opcode::Rules => b'r',
        }
    }
}

pub const HEADER_LEN: usize = 11;

/// Parses a dotted-quad IPv4 address, accepting leading zeroes.
pub fn parse_ipv4(ip: &str) -> Option<[u8; 4]> {
    let mut octets = [0u8; 4];
    let mut parts = ip.split('.');
    for octet in &mut octets {
        let part = parts.next()?;
        if part.is_empty() || part.len() > 3 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *octet = part.parse().ok()?;
    }
    parts.next().is_none().then_some(octets)
}

/// Parses an integer port in the range 1–65535.
pub fn parse_port(port: f64) -> Option<u16> {
    (port.fract() == 0.0 && (1.0..=65535.0).contains(&port)).then_some(port as u16)
}

pub fn build(octets: [u8; 4], port: u16, opcode: Opcode) -> [u8; HEADER_LEN] {
    let mut buf = [0u8; HEADER_LEN];
    buf[..4].copy_from_slice(b"SAMP");
    buf[4..8].copy_from_slice(&octets);
    buf[8..10].copy_from_slice(&port.to_le_bytes());
    buf[10] = opcode.byte();
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_the_info_packet() {
        let octets = parse_ipv4("135.148.128.72").unwrap();
        assert_eq!(
            build(octets, 7777, Opcode::Info),
            [b'S', b'A', b'M', b'P', 135, 148, 128, 72, 0x61, 0x1E, b'i']
        );
        assert_eq!(Opcode::Players.byte(), b'c');
        assert_eq!(Opcode::Detailed.byte(), b'd');
        assert_eq!(
            build([1, 2, 3, 4], 0x1234, Opcode::Rules)[8..],
            [0x34, 0x12, b'r']
        );
    }

    #[test]
    fn validates_ipv4() {
        assert_eq!(parse_ipv4("127.0.0.1"), Some([127, 0, 0, 1]));
        assert_eq!(parse_ipv4("010.001.0.255"), Some([10, 1, 0, 255]));
        for bad in [
            "256.0.0.1",
            "1.2.3",
            "1.2.3.4.5",
            "a.b.c.d",
            "1..2.3",
            "1.2.3.4 ",
            "",
            "1234.1.1.1",
            "+1.2.3.4",
        ] {
            assert_eq!(parse_ipv4(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn validates_ports() {
        assert_eq!(parse_port(7777.0), Some(7777));
        assert_eq!(parse_port(1.0), Some(1));
        assert_eq!(parse_port(65535.0), Some(65535));
        for bad in [0.0, -1.0, 65536.0, 7777.5, f64::NAN, f64::INFINITY] {
            assert_eq!(parse_port(bad), None, "{bad}");
        }
    }
}
