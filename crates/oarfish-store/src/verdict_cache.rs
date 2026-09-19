//! The verdict cache: judged once, served from memory, explained by records.
//!
//! The cache-aside flow that keeps every network call off the every-line
//! path:
//!
//! 1. moka hit — return it.
//! 2. moka miss — read fjall, populate moka, return it.
//! 3. fjall miss — return `None`; the caller enqueues the template with the
//!    judge and the log stream retries it.
//!
//! Reads are synchronous and infallible by design — a storage fault logs and
//! degrades to the same unjudged lane as a miss. A fjall read error is logged
//! and treated as a miss: the every-line path has no useful response to a
//! storage fault, and degrading to "unjudged, so dashboard-only" is the same
//! safe lane a genuine miss takes.
//!
//! This module never judges: it holds no client and spawns nothing. The
//! background judge lives in [`crate::judge`], behind the [`Decide`] seam.

use std::sync::Arc;

use oarfish_core::{QuestionsHash, TemplateId, Verdict};
use oarfish_mask::BundleHash;
use ulid::Ulid;

use crate::keys::{verdict_key, verdict_questions_prefix, verdict_template_prefix};
use crate::record::DecisionRecord;
use crate::shared::Shared;

/// The cache-aside verdict read side, over shared state.
pub struct VerdictCache {
    shared: Arc<Shared>,
}

impl VerdictCache {
    /// Built by the composition root over shared state.
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }

    /// The every-line read: moka, then fjall, then `None`.
    ///
    /// Takes the template text as well as the id because the judge needs
    /// the state, not just the key. Synchronous and infallible by design.
    pub fn lookup(&self, template_id: &TemplateId) -> Option<Verdict> {
        let key = (*template_id, self.shared.bundle_hash);
        if let Some(hit) = self.shared.cache.get(&key) {
            // The key already scopes template and bundle, and the cache
            // lives no longer than the instance whose question set it was
            // judged under — but validity is checked, not reasoned about:
            // a hit for another question set falls through and re-reads.
            if hit.questions_hash == self.shared.questions_hash {
                return Some(hit);
            }
        }
        match read_latest(&self.shared, template_id) {
            Ok(Some(verdict)) => {
                self.shared.cache.insert(key, verdict.clone());
                Some(verdict)
            }
            Ok(None) => None,
            Err(error) => {
                tracing::warn!(
                    template_id = %template_id,
                    %error,
                    "verdict read failed; degrading to unjudged"
                );
                None
            }
        }
    }

    /// The exact-key read, for replay and debugging. A changed question set,
    /// a changed bundle, or a changed resolved model misses here even when
    /// other verdicts for the template exist — that is the key doing its job.
    pub fn by_key(
        &self,
        template_id: &TemplateId,
        questions_hash: &QuestionsHash,
        bundle_hash: &BundleHash,
        model: &str,
    ) -> Option<Verdict> {
        let key = verdict_key(template_id, questions_hash, bundle_hash, model);
        match self.shared.verdicts.get(&key) {
            Ok(Some(bytes)) => match postcard::from_bytes::<Verdict>(bytes.as_ref()) {
                Ok(verdict) => Some(verdict),
                Err(error) => {
                    tracing::warn!(
                        template_id = %template_id,
                        %error,
                        "cached verdict would not decode; treating as absent"
                    );
                    None
                }
            },
            Ok(None) => None,
            Err(error) => {
                tracing::warn!(
                    template_id = %template_id,
                    %error,
                    "verdict read failed; treating as absent"
                );
                None
            }
        }
    }

    /// Every verdict one template has ever received, oldest first. The
    /// answer to "why did this wake me three weeks ago" at the template
    /// level. A replay and debugging helper, not a hot-path read.
    pub fn for_template(&self, template_id: &TemplateId) -> Vec<Verdict> {
        let prefix = verdict_template_prefix(template_id);
        let mut out = Vec::new();
        for guard in self.shared.verdicts.prefix(prefix) {
            let bytes = match guard.value() {
                Ok(bytes) => bytes,
                Err(error) => {
                    tracing::warn!(%error, "verdict scan hit an unreadable entry");
                    continue;
                }
            };
            match postcard::from_bytes::<Verdict>(bytes.as_ref()) {
                Ok(verdict) => {
                    if verdict.template_id == *template_id {
                        out.push(verdict);
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "cached verdict would not decode; skipping");
                }
            }
        }
        out.sort_by_key(|verdict| verdict.judged_at);
        out
    }

    /// Every decision record for one template, oldest first. A prefix scan
    /// over the `records_by_template` secondary index (`template_id ++
    /// record_ulid`), so the cost stays flat for the life of the install.
    /// Pre-index databases fall back to the legacy full scan once.
    pub fn records_for_template(&self, template_id: &TemplateId) -> Vec<DecisionRecord> {
        let mut out = Vec::new();
        let mut indexed = false;
        for guard in self
            .shared
            .records_by_template
            .prefix(template_id.as_bytes())
        {
            indexed = true;
            let key = match guard.key() {
                Ok(key) => key,
                Err(error) => {
                    tracing::warn!(%error, "record index hit an unreadable key");
                    continue;
                }
            };
            let bytes = key.as_ref();
            if bytes.len() != 48 {
                continue;
            }
            let mut ulid_bytes = [0u8; 16];
            ulid_bytes.copy_from_slice(&bytes[32..]);
            let record_bytes = match self.shared.records.get(ulid_bytes) {
                Ok(Some(record_bytes)) => record_bytes,
                Ok(None) => continue,
                Err(error) => {
                    tracing::warn!(%error, "record read hit an unreadable entry");
                    continue;
                }
            };
            match postcard::from_bytes::<DecisionRecord>(record_bytes.as_ref()) {
                Ok(record) => {
                    if record.template_id == *template_id {
                        out.push(record);
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "decision record would not decode; skipping");
                }
            }
        }
        if indexed {
            sort_records(&mut out);
            return out;
        }
        // No index entries: a database written before the secondary index.
        // One legacy full scan keeps those installs readable.
        for guard in self.shared.records.range([0u8; 16]..=[0xFF; 16]) {
            let bytes = match guard.value() {
                Ok(bytes) => bytes,
                Err(error) => {
                    tracing::warn!(%error, "record scan hit an unreadable entry");
                    continue;
                }
            };
            match postcard::from_bytes::<DecisionRecord>(bytes.as_ref()) {
                Ok(record) => {
                    if record.template_id == *template_id {
                        out.push(record);
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "decision record would not decode; skipping");
                }
            }
        }
        sort_records(&mut out);
        out
    }
}

fn sort_records(out: &mut [DecisionRecord]) {
    out.sort_by(|a, b| {
        a.recorded_at_unix
            .cmp(&b.recorded_at_unix)
            .then_with(|| a.id.to_string().cmp(&b.id.to_string()))
    });
}

/// The secondary-index key for one decision record: `template_id (32B) ++
/// record ULID (16B)`. A prefix scan on the template id lists that
/// template's records in chronological order, because ULID bytes sort by
/// time.
pub(crate) fn record_by_template_key(template_id: &TemplateId, id: &Ulid) -> [u8; 48] {
    let mut key = [0u8; 48];
    key[..32].copy_from_slice(template_id.as_bytes());
    key[32..].copy_from_slice(&id.to_bytes());
    key
}

/// The newest verdict for one template under this instance's question set
/// and bundle, if any. Scoped to the 80-byte prefix so question revisions
/// and bundle edits never alias.
pub(crate) fn read_latest(
    shared: &Shared,
    template_id: &TemplateId,
) -> Result<Option<Verdict>, fjall::Error> {
    let prefix = verdict_questions_prefix(template_id, &shared.questions_hash, &shared.bundle_hash);
    let mut best: Option<Verdict> = None;
    for guard in shared.verdicts.prefix(prefix) {
        let bytes = guard.value()?;
        let verdict: Verdict = match postcard::from_bytes(bytes.as_ref()) {
            Ok(verdict) => verdict,
            Err(error) => {
                tracing::warn!(
                    template_id = %template_id,
                    %error,
                    "cached verdict would not decode; treating as absent"
                );
                continue;
            }
        };
        if verdict.template_id != *template_id || verdict.questions_hash != shared.questions_hash {
            continue;
        }
        let newer = match &best {
            Some(current) => verdict.judged_at > current.judged_at,
            None => true,
        };
        if newer {
            best = Some(verdict);
        }
    }
    Ok(best)
}
