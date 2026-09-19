//! ingest → mask → drain: the first running pipeline.
//!
//! One task owns the `Bundle` and the `Drain` and drains the channel. There
//! is exactly one consumer because `Drain::train` takes `&mut self` — the
//! type mandates it, so this is not a tuning choice. Every assignment is
//! logged: in M3 the logged `TemplateId` is the far side of the pipeline, and
//! the daemon acceptance is a line in on a socket becoming a `TemplateId` out
//! of one shared Drain table.

use oarfish_core::Event;
use oarfish_drain::{Assignment, Drain};
use oarfish_mask::Bundle;
use tokio::sync::mpsc;

/// What one pipeline run did. Reported when the channel closes and the queued
/// remainder has drained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PipelineReport {
    /// Events masked, clustered and logged.
    pub processed: u64,
}

/// The every-line path, owned by one task. Pure Rust, no network: mask, then
/// cluster, then log the assignment.
pub struct Pipeline {
    bundle: Bundle,
    drain: Drain,
}

impl Pipeline {
    /// Build the pipeline over one shared table. The `Drain` is the table the
    /// acceptance talks about: every line the daemon ingests clusters here.
    pub fn new(bundle: Bundle, drain: Drain) -> Self {
        Self { bundle, drain }
    }

    /// Mask one event and train the table on it. The mask boundary converts
    /// through [`Event::raw_lossy`]: the one place invalid UTF-8 unavoidably
    /// becomes the replacement character.
    pub fn train_one(&mut self, event: &Event) -> Assignment {
        let lossy = event.raw_lossy();
        let masked = self.bundle.mask(&lossy);
        self.drain.train(masked.template())
    }

    /// Drain the channel to close. Returns after `recv()` yields `None` —
    /// which is once every listener has dropped its `Sender` — so a clean
    /// stop loses nothing that was already accepted.
    pub async fn run(mut self, mut rx: mpsc::Receiver<Event>) -> PipelineReport {
        let mut processed = 0;
        while let Some(event) = rx.recv().await {
            let assignment = self.train_one(&event);
            tracing::info!(
                template_id = %assignment.template,
                host = %event.host,
                source = ?event.source,
                "ingested"
            );
            processed += 1;
        }
        PipelineReport { processed }
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use oarfish_core::Source;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::syslog::SyslogUdp;

    fn test_event(n: u8) -> Event {
        Event::new(
            Bytes::from(format!("test line {n} stayed the same here")),
            "test",
            Source::Syslog,
        )
    }

    #[test]
    fn one_line_masks_clusters_and_reports() {
        let mut pipeline = Pipeline::new(
            oarfish_mask::curated().clone(),
            Drain::new(oarfish_drain::Config::default()).expect("default config is valid"),
        );
        let assignment = pipeline.train_one(&test_event(1));
        assert_eq!(assignment.size, 1);
        let again = pipeline.train_one(&test_event(1));
        assert_eq!(again.template, assignment.template);
        assert_eq!(again.size, 2);
    }

    /// The §6 property, made executable: cancel mid-flight, and what was
    /// already queued still drains. The five sends complete before the cancel
    /// fires, so all five must be reported — deterministically, with no timing
    /// dependence and no traffic on any socket.
    #[tokio::test]
    async fn cancel_mid_flight_still_drains_what_was_queued() {
        let (tx, rx) = mpsc::channel(16);
        let pipeline = Pipeline::new(
            oarfish_mask::curated().clone(),
            Drain::new(oarfish_drain::Config::default()).expect("default config is valid"),
        );
        let pipe_handle = tokio::spawn(pipeline.run(rx));
        for n in 0..5 {
            tx.send(test_event(n))
                .await
                .expect("the channel holds sixteen");
        }

        let listener = SyslogUdp::bind("127.0.0.1:0".parse().expect("addr"))
            .await
            .expect("bind");
        let cancel = CancellationToken::new();
        let listen_handle = tokio::spawn(listener.run(tx.clone(), cancel.clone()));
        drop(tx);
        cancel.cancel();

        tokio::time::timeout(std::time::Duration::from_secs(5), listen_handle)
            .await
            .expect("no timeout")
            .expect("join");
        let report = tokio::time::timeout(std::time::Duration::from_secs(5), pipe_handle)
            .await
            .expect("no timeout")
            .expect("join");
        assert_eq!(report.processed, 5);
    }
}
