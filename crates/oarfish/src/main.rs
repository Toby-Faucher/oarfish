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
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    daemon: DaemonArgs,
}

#[derive(Debug, clap::Subcommand)]
enum Command {
    /// Mask bundle tooling.
    Masks {
        #[command(subcommand)]
        action: MasksAction,
    },
}

#[derive(Debug, clap::Subcommand)]
enum MasksAction {
    /// Find corpus patterns the current bundle doesn't cover, propose regex
    /// slots with an LLM, and write a merged bundle.
    Synthesize {
        /// A log file, or a directory of them, to learn from.
        #[arg(long)]
        corpus: PathBuf,
        /// Bundle to start from. Defaults to the embedded curated bundle.
        #[arg(long)]
        bundle: Option<PathBuf>,
        /// A slot needs at least this many corpus occurrences to be proposed.
        #[arg(long, default_value_t = 3)]
        min_occurrences: u64,
        /// At most this many candidates go to the model in one call.
        #[arg(long, default_value_t = 50)]
        max_candidates: usize,
        /// Where to write the merged bundle.
        #[arg(long)]
        out: PathBuf,
        /// The OpenRouter model id to synthesize with. No default: this is a
        /// rare, deliberate command, and guessing a model would assert a
        /// choice with no basis.
        #[arg(long)]
        model: String,
    },
}

/// The daemon's own flags. Flattened into [`Cli`] so `oarfish --syslog ...`
/// keeps working exactly as before subcommands existed.
#[derive(Debug, Parser)]
struct DaemonArgs {
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

    /// Mask bundle to load instead of the embedded curated default. Unset
    /// means exactly the M1–M6 behavior: `oarfish_mask::curated()`.
    #[arg(long)]
    bundle: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

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

    match cli.command {
        None => run_daemon(cli.daemon).await,
        Some(Command::Masks { action }) => run_masks(action).await,
    }
}

async fn run_masks(action: MasksAction) -> anyhow::Result<()> {
    match action {
        MasksAction::Synthesize {
            corpus,
            bundle,
            min_occurrences,
            max_candidates,
            out,
            model,
        } => {
            run_masks_synthesize(corpus, bundle, min_occurrences, max_candidates, out, model).await
        }
    }
}

/// Read every non-empty line from `path`: the file itself, or every regular
/// file directly inside it if it's a directory (non-recursive).
fn read_corpus(path: &PathBuf) -> anyhow::Result<Vec<String>> {
    let mut lines = Vec::new();
    let metadata =
        std::fs::metadata(path).with_context(|| format!("cannot read {}", path.display()))?;
    if metadata.is_file() {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        lines.extend(text.lines().filter(|l| !l.is_empty()).map(str::to_owned));
    } else if metadata.is_dir() {
        for entry in std::fs::read_dir(path)
            .with_context(|| format!("cannot read directory {}", path.display()))?
        {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                let text = std::fs::read_to_string(entry.path())
                    .with_context(|| format!("cannot read {}", entry.path().display()))?;
                lines.extend(text.lines().filter(|l| !l.is_empty()).map(str::to_owned));
            }
        }
    } else {
        anyhow::bail!("{} is neither a file nor a directory", path.display());
    }
    Ok(lines)
}

async fn run_masks_synthesize(
    corpus_path: PathBuf,
    bundle_path: Option<PathBuf>,
    min_occurrences: u64,
    max_candidates: usize,
    out: PathBuf,
    model: String,
) -> anyhow::Result<()> {
    let corpus = read_corpus(&corpus_path)?;
    tracing::info!(lines = corpus.len(), path = %corpus_path.display(), "read corpus");

    let starting: oarfish_mask::Bundle = match &bundle_path {
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("cannot read bundle at {}", path.display()))?;
            oarfish_mask::Bundle::parse(&text)
                .with_context(|| format!("bundle at {} is invalid", path.display()))?
        }
        None => oarfish_mask::curated().clone(),
    };

    let client =
        oarfish_synth::Client::from_env(model).context("cannot build the synthesis client")?;

    let report = oarfish_mask::run(&client, &starting, &corpus, min_occurrences, max_candidates)
        .await
        .map_err(|e| anyhow::anyhow!("synthesis failed: {e}"))?;

    std::fs::write(&out, report.bundle.to_toml())
        .with_context(|| format!("cannot write {}", out.display()))?;

    let added = report.bundle.slots().len() - starting.slots().len();
    tracing::info!(
        out = %out.display(),
        added,
        dropped = report.dropped.len(),
        "wrote bundle"
    );
    if !report.dropped.is_empty() {
        tracing::warn!(slots = ?report.dropped, "dropped after a failed retry");
    }

    Ok(())
}

async fn run_daemon(args: DaemonArgs) -> anyhow::Result<()> {
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

    // The bundle: the embedded curated default, or a file the operator
    // pointed at with --bundle — most often one `masks synthesize` wrote.
    // Byte-for-byte the M1-through-M6 behavior when --bundle is absent.
    let bundle: oarfish_mask::Bundle = match &args.bundle {
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("cannot read bundle at {}", path.display()))?;
            let bundle = oarfish_mask::Bundle::parse(&text)
                .with_context(|| format!("bundle at {} is invalid", path.display()))?;
            tracing::info!(path = %path.display(), hash = %bundle.hash(), "loaded bundle");
            bundle
        }
        None => oarfish_mask::curated().clone(),
    };

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
    // One engine task owns the windows, the state machine and the timers.
    // The pipeline forwards it every classified line; the API reads its
    // snapshot and subscribes to its changes. The contextual check judges
    // through the same client as the verdicts: one key, one billing
    // relationship, and the local-model fallback stays a config change.
    let decide_client = client.clone();
    let verdicts = Arc::new(
        oarfish_store::Verdicts::open(
            &args.data_dir,
            client,
            oarfish_engine::static_questions(),
            oarfish_engine::merge_questions(),
            bundle.hash(),
        )
        .with_context(|| format!("cannot open store at {}", args.data_dir.display()))?,
    );

    // One engine task owns the windows, the state machine and the timers.
    // The pipeline forwards it every classified line; the API reads its
    // snapshot and subscribes to its changes.
    let engine = oarfish_engine::Engine::new(
        Arc::clone(&verdicts),
        decide_client,
        oarfish_engine::EngineConfig::default(),
    );
    let api_state = oarfish_api::ApiState::new(
        engine.snapshot_handle(),
        Arc::clone(&verdicts),
        engine.sender(),
        args.static_dir.clone(),
        cancel.child_token(),
    );
    let (engine_tx, engine_rx) = mpsc::channel(1024);

    let pipeline = Pipeline::new(
        bundle,
        oarfish_drain::Drain::new(oarfish_drain::Config::default())?,
    )
    .with_merges(verdicts.merges_handle());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_flag_parses_into_daemon_args() {
        let cli = Cli::try_parse_from(["oarfish", "--bundle", "/etc/oarfish/bundle.toml"])
            .expect("parses");
        assert_eq!(
            cli.daemon.bundle,
            Some(PathBuf::from("/etc/oarfish/bundle.toml"))
        );
        assert!(cli.command.is_none());
    }

    #[test]
    fn omitting_bundle_leaves_it_none_and_keeps_the_daemon_default_path() {
        let cli = Cli::try_parse_from(["oarfish"]).expect("parses");
        assert_eq!(cli.daemon.bundle, None);
        assert_eq!(cli.daemon.api, "127.0.0.1:4000".parse().unwrap());
    }

    #[test]
    fn masks_synthesize_parses_with_its_defaults() {
        let cli = Cli::try_parse_from([
            "oarfish",
            "masks",
            "synthesize",
            "--corpus",
            "/var/log",
            "--out",
            "/etc/oarfish/bundle.toml",
            "--model",
            "some-model",
        ])
        .expect("parses");
        match cli.command {
            Some(Command::Masks {
                action:
                    MasksAction::Synthesize {
                        corpus,
                        bundle,
                        min_occurrences,
                        max_candidates,
                        out,
                        model,
                    },
            }) => {
                assert_eq!(corpus, PathBuf::from("/var/log"));
                assert_eq!(bundle, None);
                assert_eq!(min_occurrences, 3);
                assert_eq!(max_candidates, 50);
                assert_eq!(out, PathBuf::from("/etc/oarfish/bundle.toml"));
                assert_eq!(model, "some-model");
            }
            other => panic!("expected Masks::Synthesize, got {other:?}"),
        }
    }

    #[test]
    fn masks_synthesize_requires_model_corpus_and_out() {
        let err = Cli::try_parse_from(["oarfish", "masks", "synthesize"])
            .expect_err("missing required args must fail to parse");
        let message = err.to_string();
        assert!(message.contains("--corpus"), "got {message:?}");
    }
}
