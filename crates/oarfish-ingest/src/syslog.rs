//! Syslog over UDP and TCP, both on 514, RFC 5424 and 3164.
//!
//! Two types, one two-phase lifecycle each: `bind` reserves the socket and
//! `run` serves it, so a test can bind `127.0.0.1:0` and read the ephemeral
//! port before anything starts reading. No trait and no registry: there are
//! exactly two syslog transports, they are not pluggable, and a trait would
//! buy indirection nobody calls through.
//!
//! Overload is where the transports differ. TCP applies backpressure — the
//! read loop `send().await`s, the sender's window closes on its own, nothing
//! is lost. UDP has no backpressure to apply, so a full channel means shed
//! and count via [`crate::ShedTracker`]. Framing on TCP is
//! `AnyDelimiterCodec`, per the dependency table's spirit: it must be
//! byte-oriented, because `LinesCodec` decodes to `String` and a single
//! non-UTF-8 byte would kill the whole connection — exactly the malformed
//! input §7 exists to carry.

use std::io;
use std::net::SocketAddr;

use bytes::Bytes;
use governor::{
    RateLimiter, clock::DefaultClock, middleware::NoOpMiddleware, state::InMemoryState,
    state::direct::NotKeyed,
};
use oarfish_core::{Event, Source};
use syslog_loose::{Protocol, Variant, parse_message_with_year_exact};
use time::OffsetDateTime;
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::mpsc;
use tokio_stream::StreamExt as _;
use tokio_util::codec::{AnyDelimiterCodec, AnyDelimiterCodecError, Framed};
use tokio_util::sync::CancellationToken;

use crate::{IngestError, ShedTracker};

/// Largest single syslog line accepted on TCP. Past this `LinesCodec` errors
/// the frame and the connection is dropped at debug: a peer emitting
/// megabyte lines is the firehose, not the signal.
const MAX_LINE_BYTES: usize = 64 * 1024;

/// Largest UDP datagram read in one `recv_from`. 65507 is the max UDP payload
/// over IPv4; anything larger cannot arrive whole.
const MAX_DATAGRAM_BYTES: usize = 65_507;

/// Turn one frame's bytes into an `Event`. Pure, so the snapshot harness can
/// pin it: same bytes in, same event out.
///
/// `peer` is the socket peer. It becomes `host` — the peer *IP*, never
/// `host:port` — when the frame names no host of its own, and it is the only
/// host a malformed frame ever gets. The port is stripped because it is
/// ephemeral: a UDP source port changes per datagram, so keeping it would
/// hand every malformed line a distinct host and fragment per-host grouping,
/// dedup and windows downstream.
///
/// Malformed input is data, not an error. When the bytes are not UTF-8 or
/// `syslog_loose` rejects the frame outright, the event still goes out: raw
/// bytes verbatim, `attrs["parse"] = "failed"`. A device emitting frames our
/// parser rejects is a device worth an alarm; discarding those lines because
/// the firmware disagrees with the RFC would delete exactly the signal this
/// crate exists to carry.
pub fn frame_to_event(raw: &[u8], peer: &SocketAddr, received_at: OffsetDateTime) -> Event {
    let text = std::str::from_utf8(raw).ok();
    let parsed = text.and_then(|line| {
        parse_message_with_year_exact(line, |_| received_at.year(), Variant::Either).ok()
    });
    match parsed {
        Some(message) => {
            let mut attrs = std::collections::BTreeMap::new();
            attrs.insert(
                "protocol".to_owned(),
                match message.protocol {
                    Protocol::RFC5424(_) => "rfc5424".to_owned(),
                    Protocol::RFC3164 => "rfc3164".to_owned(),
                },
            );
            if let Some(facility) = message.facility {
                attrs.insert("facility".to_owned(), facility.as_str().to_owned());
            }
            // The syslog number is a value a vendor's firmware picked. It goes
            // in `attrs`, never into `Severity`: mapping one onto the other
            // would let any device on the network set its own alarm severity.
            if let Some(severity) = message.severity {
                attrs.insert("severity".to_owned(), severity.as_str().to_owned());
            }
            if let Some(app) = message.appname {
                attrs.insert("app".to_owned(), app.to_owned());
            }
            if let Some(procid) = message.procid {
                attrs.insert("procid".to_owned(), procid.to_string());
            }
            if let Some(msgid) = message.msgid {
                attrs.insert("msgid".to_owned(), msgid.to_owned());
            }
            let timestamp = message
                .timestamp
                .and_then(|ts| ts.timestamp_nanos_opt())
                .and_then(|nanos| OffsetDateTime::from_unix_timestamp_nanos(nanos as i128).ok());
            Event {
                raw: Bytes::copy_from_slice(raw),
                received_at,
                timestamp,
                host: message
                    .hostname
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| peer.ip().to_string()),
                source: Source::Syslog,
                attrs,
            }
        }
        None => Event {
            raw: Bytes::copy_from_slice(raw),
            received_at,
            timestamp: None,
            host: peer.ip().to_string(),
            source: Source::Syslog,
            attrs: std::collections::BTreeMap::from([("parse".to_owned(), "failed".to_owned())]),
        },
    }
}

/// Syslog over UDP. One socket, one read loop, `try_send` into the channel:
/// a datagram socket has no backpressure to apply, so a full channel sheds
/// and counts.
pub struct SyslogUdp {
    socket: UdpSocket,
    intake: RateLimiter<NotKeyed, InMemoryState, DefaultClock, NoOpMiddleware>,
    shed: ShedTracker,
}

impl SyslogUdp {
    /// Bind the socket. Returns before serving; see [`SyslogUdp::run`].
    pub async fn bind(addr: SocketAddr) -> Result<Self, IngestError> {
        let socket = UdpSocket::bind(addr)
            .await
            .map_err(|source| IngestError::Bind { addr, source })?;
        Ok(Self {
            socket,
            intake: RateLimiter::direct(ShedTracker::udp_quota()),
            shed: ShedTracker::new("syslog-udp"),
        })
    }

    /// The bound address. A test binds port 0 and reads this back.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    /// How many datagrams this listener has shed so far. Shares state with the
    /// running loop, so a test can hold this while traffic flows.
    pub fn shed_tracker(&self) -> ShedTracker {
        self.shed.clone()
    }

    /// Serve datagrams until `cancel` fires. Drops its `Sender` on exit, so
    /// the pipeline's channel closes once every listener is gone.
    pub async fn run(self, tx: mpsc::Sender<Event>, cancel: CancellationToken) {
        let mut buf = vec![0u8; MAX_DATAGRAM_BYTES];
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                res = self.socket.recv_from(&mut buf) => {
                    match res {
                        Ok((len, peer)) => {
                            if self.intake.check().is_err() {
                                self.shed.note_rate_limited();
                                continue;
                            }
                            let event =
                                frame_to_event(&buf[..len], &peer, OffsetDateTime::now_utc());
                            match tx.try_send(event) {
                                Ok(()) => {}
                                // The pipeline is gone: no point holding the socket.
                                Err(mpsc::error::TrySendError::Closed(_)) => break,
                                // No backpressure on a datagram socket: count it.
                                Err(mpsc::error::TrySendError::Full(_)) => {
                                    self.shed.note_dropped();
                                }
                            }
                        }
                        Err(e) => tracing::debug!(error = %e, "syslog-udp recv failed"),
                    }
                }
            }
        }
    }
}

/// Syslog over TCP. Accepts connections until `cancel` fires; each connection
/// is framed by `LinesCodec` and `send().await`s into the channel, stalling
/// the read loop — and the sender's window — instead of shedding.
pub struct SyslogTcp {
    listener: TcpListener,
}

impl SyslogTcp {
    /// Bind the socket. Returns before serving; see [`SyslogTcp::run`].
    pub async fn bind(addr: SocketAddr) -> Result<Self, IngestError> {
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|source| IngestError::Bind { addr, source })?;
        Ok(Self { listener })
    }

    /// The bound address. A test binds port 0 and reads this back.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Accept connections until `cancel` fires. Drops its `Sender` on exit.
    /// Per-connection faults — peer reset, truncated frame — log at debug and
    /// drop that connection, never the listener.
    pub async fn run(self, tx: mpsc::Sender<Event>, cancel: CancellationToken) {
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                res = self.listener.accept() => {
                    match res {
                        Ok((socket, peer)) => {
                            tokio::spawn(Self::serve_conn(
                                socket,
                                peer,
                                tx.clone(),
                                cancel.child_token(),
                            ));
                        }
                        Err(e) => tracing::debug!(error = %e, "syslog-tcp accept failed"),
                    }
                }
            }
        }
    }

    async fn serve_conn(
        socket: TcpStream,
        peer: SocketAddr,
        tx: mpsc::Sender<Event>,
        cancel: CancellationToken,
    ) {
        // Byte-oriented framing, deliberately not LinesCodec: lines arrive as
        // BytesMut, so a Latin-1 byte in one line neither kills the line nor
        // the connection. Only an overlong chunk or a real I/O error tears
        // down the peer.
        let mut framed = Framed::new(
            socket,
            AnyDelimiterCodec::new_with_max_length(vec![b'\n'], vec![b'\n'], MAX_LINE_BYTES),
        );
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                next = framed.next() => {
                    match next {
                        Some(Ok(chunk)) => {
                            let event =
                                frame_to_event(&chunk, &peer, OffsetDateTime::now_utc());
                            // Backpressure, not shedding: stall here.
                            if tx.send(event).await.is_err() {
                                break;
                            }
                        }
                        Some(Err(e)) => {
                            match e {
                                AnyDelimiterCodecError::MaxChunkLengthExceeded => {
                                    tracing::debug!(%peer, "syslog-tcp line over the limit");
                                }
                                AnyDelimiterCodecError::Io(e) => {
                                    tracing::debug!(error = %e, %peer, "syslog-tcp read failed");
                                }
                            }
                            break;
                        }
                        None => break,
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn received_at() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_789_812_000).expect("fixed test time")
    }

    fn peer() -> SocketAddr {
        "10.0.0.9:40000".parse().expect("test peer")
    }

    #[test]
    fn an_rfc5424_frame_becomes_an_event() {
        let raw = b"<34>1 2026-09-19T10:00:00Z web01 nginx 123 req1 - connection accepted";
        let event = frame_to_event(raw, &peer(), received_at());
        insta::assert_json_snapshot!(event, @r###"
        {
          "raw": "<34>1 2026-09-19T10:00:00Z web01 nginx 123 req1 - connection accepted",
          "received_at": "2026-09-19T10:00:00Z",
          "timestamp": "2026-09-19T10:00:00Z",
          "host": "web01",
          "source": "syslog",
          "attrs": {
            "app": "nginx",
            "facility": "auth",
            "msgid": "req1",
            "procid": "123",
            "protocol": "rfc5424",
            "severity": "crit"
          }
        }
        "###);
    }

    #[test]
    fn an_rfc3164_frame_becomes_an_event() {
        let raw = b"<34>Sep 19 10:00:01 web01 sshd[1234]: Failed password for root";
        let event = frame_to_event(raw, &peer(), received_at());
        assert_eq!(event.host, "web01");
        assert_eq!(event.source, Source::Syslog);
        assert_eq!(
            event.raw_lossy(),
            "<34>Sep 19 10:00:01 web01 sshd[1234]: Failed password for root"
        );
        assert_eq!(
            event.attrs.get("protocol").map(String::as_str),
            Some("rfc3164")
        );
        assert_eq!(event.attrs.get("app").map(String::as_str), Some("sshd"));
        assert!(!event.attrs.contains_key("parse"));
        // 3164 carries no year; the parser fills in the intake year.
        let timestamp = event.timestamp.expect("3164 still yields a timestamp");
        assert_eq!(timestamp.year(), received_at().year());
    }

    #[test]
    fn an_unparseable_frame_is_still_an_event_marked_failed() {
        let raw = b"<999>this pri does not exist";
        let event = frame_to_event(raw, &peer(), received_at());
        // The peer IP, never host:port: the source port is ephemeral, and
        // keeping it would hand every malformed line a distinct host.
        assert_eq!(event.host, "10.0.0.9");
        assert_eq!(event.attrs.get("parse").map(String::as_str), Some("failed"));
        assert_eq!(event.raw.as_ref(), raw);
    }

    #[test]
    fn non_utf8_bytes_are_still_an_event_marked_failed() {
        let raw = b"\xff\xfe\x00binary junk";
        let event = frame_to_event(raw, &peer(), received_at());
        assert_eq!(event.host, "10.0.0.9");
        assert_eq!(event.attrs.get("parse").map(String::as_str), Some("failed"));
        assert_eq!(event.raw.as_ref(), raw);
    }

    // Invariant 3: for arbitrary bytes in, `event.raw` equals them exactly.
    proptest::proptest! {
        #[test]
        fn the_raw_frame_survives_byte_for_byte(bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..4096)) {
            let event = frame_to_event(&bytes, &"127.0.0.1:9999".parse().expect("peer"), received_at());
            proptest::prop_assert_eq!(event.raw.as_ref(), bytes.as_slice());
        }
    }

    /// One frame split across two writes is reassembled by the codec.
    #[tokio::test]
    async fn tcp_reassembles_a_frame_split_across_two_writes() {
        use tokio::io::AsyncWriteExt as _;

        let listener = SyslogTcp::bind("127.0.0.1:0".parse().expect("addr"))
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let (tx, mut rx) = mpsc::channel(16);
        let cancel = CancellationToken::new();
        let handle = tokio::spawn(listener.run(tx, cancel.clone()));

        let mut client = tokio::net::TcpStream::connect(addr).await.expect("connect");
        client
            .write_all(b"<34>1 2026-09-19T10:00:00Z web01 nginx")
            .await
            .expect("first half");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        client
            .write_all(b" 123 req1 - split frame\n")
            .await
            .expect("second half");

        let event = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("no timeout")
            .expect("an event");
        assert_eq!(
            event.raw_lossy(),
            "<34>1 2026-09-19T10:00:00Z web01 nginx 123 req1 - split frame"
        );
        assert_eq!(event.host, "web01");
        assert!(!event.attrs.contains_key("parse"));

        cancel.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(5), handle)
            .await
            .expect("no timeout")
            .expect("join");
    }

    /// The §5 policy made executable: with a capacity-1 channel held full,
    /// UDP sheds *and counts* while TCP stalls instead of shedding.
    #[tokio::test]
    async fn overload_udp_sheds_and_counts_while_tcp_stalls() {
        use tokio::io::AsyncWriteExt as _;

        fn syslog_line(n: u8) -> Vec<u8> {
            format!("<34>1 2026-09-19T10:00:0{n}Z web01 nginx 1 req{n} - line {n}\n").into_bytes()
        }

        // UDP sheds.
        let (tx, mut rx) = mpsc::channel(1);
        tx.send(Event::new(
            Bytes::from_static(b"already queued"),
            "test",
            Source::Syslog,
        ))
        .await
        .expect("channel holds one");
        let udp = SyslogUdp::bind("127.0.0.1:0".parse().expect("addr"))
            .await
            .expect("bind");
        let udp_addr = udp.local_addr().expect("local addr");
        let shed = udp.shed_tracker();
        let cancel = CancellationToken::new();
        let udp_handle = tokio::spawn(udp.run(tx.clone(), cancel.clone()));

        let sender = UdpSocket::bind("127.0.0.1:0").await.expect("udp client");
        sender
            .send_to(&syslog_line(1), udp_addr)
            .await
            .expect("send datagram");
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert_eq!(shed.dropped(), 1, "a full channel sheds the datagram");
        // Only the pre-queued event is still there; the datagram is gone.
        assert_eq!(
            rx.recv().await.expect("queued").raw_lossy(),
            "already queued"
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
                .await
                .is_err(),
            "the shed datagram must not arrive late"
        );
        cancel.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(5), udp_handle)
            .await
            .expect("no timeout")
            .expect("join");

        // TCP stalls: nothing is shed, the line arrives once room frees up.
        let (tx, mut rx) = mpsc::channel(1);
        tx.send(Event::new(
            Bytes::from_static(b"already queued"),
            "test",
            Source::Syslog,
        ))
        .await
        .expect("channel holds one");
        let tcp = SyslogTcp::bind("127.0.0.1:0".parse().expect("addr"))
            .await
            .expect("bind");
        let tcp_addr = tcp.local_addr().expect("local addr");
        let cancel = CancellationToken::new();
        let tcp_handle = tokio::spawn(tcp.run(tx.clone(), cancel.clone()));

        let mut client = tokio::net::TcpStream::connect(tcp_addr)
            .await
            .expect("connect");
        client.write_all(&syslog_line(2)).await.expect("send line");
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        // The pre-queued event is still first: the TCP line stalled behind it.
        assert_eq!(
            rx.recv().await.expect("queued").raw_lossy(),
            "already queued"
        );
        let stalled = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("no timeout")
            .expect("the stalled line arrives once the channel frees up");
        assert!(stalled.raw_lossy().contains("line 2"));
        assert_eq!(stalled.host, "web01");
        cancel.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(5), tcp_handle)
            .await
            .expect("no timeout")
            .expect("join");
    }

    /// A single non-UTF-8 byte must cost one line, not the connection. The
    /// codec is byte-oriented, so the Latin-1 line still arrives (marked
    /// failed, raw verbatim) and the valid line behind it arrives too.
    #[tokio::test]
    async fn tcp_survives_a_non_utf8_line() {
        use tokio::io::AsyncWriteExt as _;

        let listener = SyslogTcp::bind("127.0.0.1:0".parse().expect("addr"))
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let (tx, mut rx) = mpsc::channel(16);
        let cancel = CancellationToken::new();
        let handle = tokio::spawn(listener.run(tx, cancel.clone()));

        let mut client = tokio::net::TcpStream::connect(addr).await.expect("connect");
        // A degree sign in Latin-1 where UTF-8 was expected.
        client
            .write_all(b"temp 23\xb0C on sensor 7\n")
            .await
            .expect("latin-1 line");
        client
            .write_all(b"<34>1 2026-09-19T10:00:00Z web01 nginx 1 req1 - after\n")
            .await
            .expect("valid line");

        let bad = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("no timeout")
            .expect("the latin-1 line arrives");
        assert_eq!(bad.raw.as_ref(), b"temp 23\xb0C on sensor 7");
        assert_eq!(bad.attrs.get("parse").map(String::as_str), Some("failed"));
        assert_eq!(bad.host, "127.0.0.1");

        let good = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("no timeout")
            .expect("the connection survives for the next line");
        assert_eq!(good.host, "web01");
        assert!(!good.attrs.contains_key("parse"));

        cancel.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(5), handle)
            .await
            .expect("no timeout")
            .expect("join");
    }
}
