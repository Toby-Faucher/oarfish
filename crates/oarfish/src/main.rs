//! `oarfish` - the daemon and its CLI.
//!
//! Wiring only: this binary composes the crates and owns no pipeline logic of
//! its own. It binds the three listeners, the pipeline, the engine and the
//! API server on a `TaskTracker`, and on Ctrl-C cancels the listeners first —
//! they drop their `Sender`s, the channels close, and the pipeline and the
//! engine drain what is still queued before the daemon exits. A clean stop
//! loses nothing that was already accepted.
//!
//! The daemon's own log writes go through a non-blocking appender, so logging
//! never stalls the ingest path it reports on.

#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

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

    /// Address for the JSON API, the SSE stream and the board.
    #[arg(long, default_value = "127.0.0.1:4000")]
    api: SocketAddr,

    /// Where the verdict cache, decision records and open alarms live.
    #[arg(long, default_value = "oarfish-data")]
    data_dir: PathBuf,

    /// Directory holding the built board, served around `/api`. Unset until
    /// the board lands: the API and the stream work without it.
    #[arg(long)]
    static_dir: Option<PathBuf>,
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
    // Bind the API before spawning anything: a taken port is a startup
    // error, never a task that fails silently while the daemon claims to
    // listen.
    let api_listener = tokio::net::TcpListener::bind(args.api)
        .await
        .with_context(|| format!("cannot bind API on {}", args.api))?;

    // The verdict cache and the open alarms. The question set is the
    // engine's static policy; the bundle hash is the mask identity the
    // verdicts are keyed under, so an edited bundle re-judges instead of
    // false-hitting. The key is optional, and without one the
    // pipeline still runs end to end — templates stay unjudged, the gate
    // holds, and nothing raises until a key arrives.
    let client = match oarfish_jev::Client::from_env() {
        Ok(client) => client,
        Err(error) => {
            tracing::warn!(
                %error,
                "templates will stay unjudged and nothing will raise; set OPENROUTER_API_KEY"
            );
            oarfish_jev::Client::new(
                "http://127.0.0.1:9/unreachable",
                "unset",
                oarfish_jev::DEFAULT_MODEL,
            )
        }
    };
    let verdicts = Arc::new(
        oarfish_store::Verdicts::open(
            &args.data_dir,
            client,
            oarfish_engine::static_questions(),
            oarfish_mask::curated().hash(),
        )
        .with_context(|| format!("cannot open store at {}", args.data_dir.display()))?,
    );

    // One engine task owns the windows, the state machine and the timers.
    // The pipeline forwards it every classified line; the API reads its
    // snapshot and subscribes to its changes.
    let engine = oarfish_engine::Engine::new(
        Arc::clone(&verdicts),
        oarfish_engine::EngineConfig::default(),
    );
    let api_state = oarfish_api::ApiState::new(
        engine.snapshot_handle(),
        engine.sender(),
        args.static_dir.clone(),
        cancel.child_token(),
    );
    let (engine_tx, engine_rx) = mpsc::channel(1024);

    let pipeline = Pipeline::new(
        oarfish_mask::curated().clone(),
        oarfish_drain::Drain::new(oarfish_drain::Config::default())?,
    );
    // Consumers first: the pipeline and the engine are running before any
    // socket starts reading, so startup never sheds into a channel with no
    // reader.
    tracker.spawn(async move {
        let report = pipeline.run_forwarding(rx, engine_tx).await;
        tracing::info!(processed = report.processed, "pipeline drained");
    });
    tracker.spawn(engine.run(engine_rx, cancel.child_token()));
    tracker.spawn(oarfish_api::serve_on_listener(
        api_state,
        api_listener,
        cancel.child_token(),
    ));

    // Producers last: binding stayed early so failures abort startup, but
    // the `run` loops start only once the consumer above is listening.
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

    tracing::info!(
        syslog = %args.syslog,
        otlp = %args.otlp,
        journal = args.journal,
        api = %args.api,
        data_dir = %args.data_dir.display(),
        "oarfish listening"
    );

    shutdown_signal().await;
    tracing::info!("shutting down: listeners stop, the pipeline and the engine drain");
    cancel.cancel();
    tracker.close();
    tracker.wait().await;

    // Shut the judge down and flush. If anything still holds the store the
    // explicit close is skipped and the database persists on drop.
    match Arc::try_unwrap(verdicts) {
        Ok(verdicts) => verdicts.close().await.context("cannot close store")?,
        Err(verdicts) => verdicts
            .persist()
            .context("cannot flush store on shutdown")?,
    }
    #[cfg(feature = "journald")]
    if let Some(thread) = journal_thread {
        thread
            .join()
            .map_err(|_| anyhow::anyhow!("journal thread panicked"))?;
    }

    Ok(())
}
