//! Wake-on-LAN magic packets.

use std::net::{TcpStream, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

pub fn parse_mac(mac: &str) -> Result<[u8; 6]> {
    let hex: String = mac.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() != 12 {
        bail!("invalid MAC address: {mac}");
    }
    let mut out = [0u8; 6];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)?;
    }
    Ok(out)
}

pub fn magic_packet(mac: [u8; 6]) -> Vec<u8> {
    let mut packet = vec![0xFF; 6];
    for _ in 0..16 {
        packet.extend_from_slice(&mac);
    }
    packet
}

pub fn send(mac: &str, broadcast: &str, port: u16) -> Result<()> {
    let packet = magic_packet(parse_mac(mac)?);
    let socket = UdpSocket::bind("0.0.0.0:0").context("bind UDP socket")?;
    socket.set_broadcast(true)?;
    let target = if broadcast.is_empty() {
        "255.255.255.255"
    } else {
        broadcast
    };
    socket
        .send_to(&packet, (target, if port == 0 { 9 } else { port }))
        .context("send magic packet")?;
    Ok(())
}

/// Block until `host:port` accepts TCP connections or `timeout` elapses.
pub fn wait_for_port(host: &str, port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(addrs) = (host, port).to_socket_addrs() {
            for addr in addrs {
                if TcpStream::connect_timeout(&addr, Duration::from_secs(2)).is_ok() {
                    return true;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_parsing_and_packet() {
        assert_eq!(
            parse_mac("aa:bb:cc:dd:ee:ff").unwrap(),
            [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]
        );
        assert_eq!(parse_mac("AA-BB-CC-DD-EE-01").unwrap()[5], 1);
        assert!(parse_mac("aa:bb").is_err());
        let p = magic_packet([1, 2, 3, 4, 5, 6]);
        assert_eq!(p.len(), 102);
        assert_eq!(&p[..6], &[0xFF; 6]);
        assert_eq!(&p[96..], &[1, 2, 3, 4, 5, 6]);
    }
}
