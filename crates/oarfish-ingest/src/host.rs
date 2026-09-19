//! Host resolution: which host an event belongs to, in one place.
//!
//! Every transport answers the same question with the same shape — a named
//! source the payload carries, else a fallback the transport supplies — so
//! the precedence lives here instead of restated per transport:
//!
//! - syslog: the frame's hostname, else the peer IP.
//! - OTLP: `host.name`, else the peer IP.
//! - journal: `_HOSTNAME`, else `localhost` — the journal is local.
//!
//! Per-host grouping, dedupe and windows downstream all key on this value,
//! so getting it wrong fragments them. The two rules below are the whole
//! policy: the peer IP never carries a port, and an empty name counts as
//! absent.

use std::net::SocketAddr;

/// The socket peer's IP as text. Never `host:port`: the source port is
/// ephemeral — a UDP source port changes per datagram — so keeping it would
/// hand every malformed line a distinct host and fragment per-host grouping,
/// dedupe and windows downstream.
pub fn peer_ip(peer: &SocketAddr) -> String {
    peer.ip().to_string()
}

/// Pick the host for one event: the named source wins when present and
/// non-empty, else the fallback. An empty name counts as absent — a
/// `host.name` of `""` names no host.
pub fn resolve(name: Option<impl AsRef<str>>, fallback: &str) -> String {
    match name {
        Some(name) if !name.as_ref().is_empty() => name.as_ref().to_owned(),
        _ => fallback.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_peer_ip_never_carries_the_port() {
        let v4: SocketAddr = "10.0.0.9:40000".parse().expect("peer");
        assert_eq!(peer_ip(&v4), "10.0.0.9");
        let v6: SocketAddr = "[::1]:4317".parse().expect("peer");
        assert_eq!(peer_ip(&v6), "::1");
    }

    #[test]
    fn a_named_host_wins_over_the_fallback() {
        assert_eq!(resolve(Some("web01"), "10.0.0.9"), "web01");
        assert_eq!(resolve(Some("web01".to_owned()), "10.0.0.9"), "web01");
    }

    #[test]
    fn a_missing_or_empty_name_falls_back() {
        assert_eq!(resolve(None::<&str>, "10.0.0.9"), "10.0.0.9");
        assert_eq!(resolve(Some(""), "10.0.0.9"), "10.0.0.9");
        assert_eq!(resolve(Some(String::new()), "localhost"), "localhost");
    }
}
