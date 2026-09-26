//! Ports `GatewayPort.kt`: the well-known gateway port when it is free, otherwise any free loopback
//! port. The chosen value is published in gateway.json, so callers never hard-code it.
//!
//! The well-known port is 41434, the one the Kotlin Nook used, so tools set up for it keep working;
//! a sandbox pins another one (41510).

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream};
use std::time::Duration;

pub const PREFERRED: u16 = 41434;
/// Pins a port for development copies so they never sit on the installed app's port.
pub const PORT_ENV: &str = "NOOK_RS_GATEWAY_PORT";

pub fn choose() -> u16 {
    if let Some(port) = std::env::var(PORT_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u16>().ok())
    {
        if port != 0 {
            return port;
        }
    }
    if is_free(PREFERRED) {
        return PREFERRED;
    }
    TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .unwrap_or(PREFERRED)
}

/// A bind test alone is not enough on Windows, where a socket with SO_REUSEADDR binds happily next
/// to a live listener; so the port also has to refuse a connection.
pub fn is_free(port: u16) -> bool {
    let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port));
    if TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok() {
        return false;
    }
    TcpListener::bind(addr).is_ok()
}
