//! The three listeners, normalizing everything into `oarfish_core::Event`.
//!
//! - **OTLP** over gRPC and HTTP, for anything with a collector in front of it.
//! - **Syslog** (RFC 5424 / RFC 3164) on 514, for switches, PDUs and UPSes -
//!   the hardware that will never run an agent and is usually what broke.
//! - **systemd journal**, read directly on the host, so a single-box install
//!   needs no collector at all.
//!
//! The raw line is always preserved verbatim. At 3am you want the bytes that
//! actually arrived, not our interpretation of them.

#![forbid(unsafe_code)]
