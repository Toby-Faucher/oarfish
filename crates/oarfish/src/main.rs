//! `oarfish` - the daemon and its CLI.
//!
//! Wiring only: this binary composes the crates and owns no pipeline logic of
//! its own. It binds the three listeners, spawns them and the one pipeline
//! task on a `TaskTracker`, and on Ctrl-C cancels the listeners first — they
//! drop their `Sender`s, the channel closes, and the pipeline drains what is
//! still queued before the daemon exits. A clean stop loses nothing that was
//! already accepted.
//!
//! The daemon's own log writes go through a non-blocking appender, so logging
//! never stalls the ingest path it reports on.

#![forbid(unsafe_code)]

use std::net::SocketAddr;

use anyhow::Context;
use clap::Parser;
use oarfish_ingest::{Otlp, Pipeline, SyslogTcp, SyslogUdp};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// Ctrl-C everywhere, SIGTERM too. This ships as a systemd unit, and
/// `systemctl stop` sends SIGTERM — whose default disposition would kill the
/// process instantly and discard up to a channel-full of queued events.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler installs");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = term.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Log-driven alarms for homelabs.
#[derive(Debug, Parser)]
struct Args {
    /// Address to listen on for syslog over UDP and TCP.
    #[arg(long, default_value = "0.0.0.0:514")]
    syslog: SocketAddr,

    /// Address to listen on for OTLP logs over gRPC.
    #[arg(long, default_value = "0.0.0.0:4317")]
    otlp: SocketAddr,

    /// Tail the systemd journal. Needs the `journald` build.
    #[arg(long, default_value_t = false)]
    journal: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // Non-blocking: the daemon's own writes must never stall the path they
    // report on. The guard is held to the end of main so no line is lost.
    let (writer, _guard) = tracing_appender::non_blocking(std::io::stdout());
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(writer)
        .init();

    #[cfg(not(feature = "journald"))]
    if args.journal {
        anyhow::bail!(
            "--journal was given but this binary was built without the `journald` feature; \
             rebuild with `--features journald` instead of silently reading nothing"
        );
    }

    // One bounded channel for all three listeners. One consumer: Drain::train
    // takes &mut self, so the pipeline task owns the table alone.
    let (tx, rx) = mpsc::channel(1024);
    let cancel = CancellationToken::new();
    let tracker = TaskTracker::new();

    let udp = SyslogUdp::bind(args.syslog)
        .await
        .with_context(|| format!("cannot bind syslog UDP on {}", args.syslog))?;
    let tcp = SyslogTcp::bind(args.syslog)
        .await
        .with_context(|| format!("cannot bind syslog TCP on {}", args.syslog))?;
    let otlp = Otlp::bind(args.otlp)
        .await
        .with_context(|| format!("cannot bind OTLP gRPC on {}", args.otlp))?;

    tracker.spawn(udp.run(tx.clone(), cancel.child_token()));
    tracker.spawn(tcp.run(tx.clone(), cancel.child_token()));
    tracker.spawn(otlp.run(tx.clone(), cancel.child_token()));

    #[cfg(feature = "journald")]
    let journal_thread = if args.journal {
        // The journal handle is !Send: it is opened and read on one dedicated
        // OS thread, never moved. The thread reports its open result back, so
        // a journal failure is still a clear startup error.
        let tx = tx.clone();
        let cancel = cancel.child_token();
        let (open_tx, open_rx) =
            tokio::sync::oneshot::channel::<Result<(), oarfish_ingest::IngestError>>();
        let thread = std::thread::Builder::new()
            .name("oarfish-journal".to_owned())
            .spawn(move || match oarfish_ingest::JournalReader::open() {
                Ok(reader) => {
                    let _ = open_tx.send(Ok(()));
                    reader.run_blocking(tx, cancel);
                }
                Err(e) => {
                    let _ = open_tx.send(Err(e));
                }
            })
            .context("cannot start journal thread")?;
        open_rx
            .await
            .context("journal thread died while opening the journal")?
            .context("cannot open systemd journal")?;
        Some(thread)
    } else {
        None
    };

    // The daemon's copy is dropped here: once the listeners exit on cancel,
    // no Sender remains and the channel closes for the pipeline to drain.
    drop(tx);

    let pipeline = Pipeline::new(
        oarfish_mask::curated().clone(),
        oarfish_drain::Drain::new(oarfish_drain::Config::default())?,
    );
    tracker.spawn(async move {
        let report = pipeline.run(rx).await;
        tracing::info!(processed = report.processed, "pipeline drained");
    });

    tracing::info!(
        syslog = %args.syslog,
        otlp = %args.otlp,
        journal = args.journal,
        "oarfish listening"
    );

    shutdown_signal().await;
    tracing::info!("shutting down: listeners stop, the pipeline drains");
    cancel.cancel();
    tracker.close();
    tracker.wait().await;
    #[cfg(feature = "journald")]
    if let Some(thread) = journal_thread {
        thread
            .join()
            .map_err(|_| anyhow::anyhow!("journal thread panicked"))?;
    }

    Ok(())
}
