//! The three listeners, normalizing everything into `oarfish_core::Event`.
//!
//! - **Syslog** (RFC 5424 / RFC 3164) on 514, for switches, PDUs and UPSes -
//!   the hardware that will never run an agent and is usually what broke.
//! - **OTLP** logs over gRPC on 4317, for anything with a collector in front
//!   of it. Metrics and traces stay out: OTLP is a log transport here.
//! - **systemd journal**, read directly on the host behind the `journald`
//!   feature, so a single-box install needs no collector at all.
//!
//! Each listener is a concrete type with a two-phase lifecycle — bind, then
//! run — and no trait: there are exactly three, they are not pluggable, and a
//! trait would buy indirection nobody calls through. What the three share —
//! which host an event belongs to — lives in [`host`], not restated per
//! transport. Listeners write into one
//! bounded `mpsc<Event>`; one [`Pipeline`] task owns the mask bundle and the
//! Drain table and drains it.
//!
//! The raw line is always preserved verbatim. At 3am you want the bytes that
//! actually arrived, not our interpretation of them. What gets clustered is
//! narrower: a syslog frame with a real timestamp clusters from its app name
//! on (`Event::body_offset`), so neither the clock nor the host lands in the
//! template. Every other line clusters whole.
//!
//! Depends on `oarfish-core` for `Event`, on `oarfish-mask` and
//! `oarfish-drain` for the pipeline, and on nothing else in the workspace.

#![forbid(unsafe_code)]

use std::net::SocketAddr;

pub mod host;
pub mod journal;
pub mod otlp;
pub mod pipeline;
pub mod shed;
pub mod syslog;

pub use host::{peer_ip, resolve};
#[cfg(feature = "journald")]
pub use journal::JournalReader;
pub use journal::{DEFAULT_EXCLUDE_UNIT, record_to_event};
pub use otlp::{Otlp, log_record_to_event, request_to_events};
pub use pipeline::{Pipeline, PipelineReport};
pub use shed::ShedTracker;
pub use syslog::{SyslogTcp, SyslogUdp, frame_to_event};

/// Why ingest could not start. Covers fatal setup only: bind, journal open.
/// A bind failure exits the daemon with a clear message. Per-connection
/// faults — peer reset, truncated frame — log at debug and drop that
/// connection, never the listener.
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    /// A listener socket could not be bound.
    #[error("cannot bind {addr}: {source}")]
    Bind {
        addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },
    /// libsystemd refused to open the journal.
    #[cfg(feature = "journald")]
    #[error("cannot open systemd journal: {reason}")]
    JournalOpen { reason: String },
}
