//! The storage key for cached verdicts: four components, fixed-width first.
//!
//! ```text
//! template_id (32B) ++ questions_hash (16B) ++ bundle_hash (32B) ++ resolved_model_id (variable)
//! ```
//!
//! A verdict is only valid for the question set that produced it, the bundle
//! that masked its template, and the model build that answered — the gate
//! proved a request for `typesafe/jev-1.13` is answered by a dated build — so
//! all three ride in the key rather than being assumed. A reworded question,
//! a moved build, or an edited bundle misses the cache and re-judges instead
//! of serving a stale answer.
//!
//! The bundle component is what turns a bundle edit into a miss rather than
//! a false hit: two different raw lines masking down to the same text under
//! different bundles hold different keys, so the store can never serve one
//! template's verdict for another's.
//!
//! The ordering is load-bearing. With the fixed-width components first, a
//! prefix scan on `template_id` returns every verdict that template has ever
//! received, across question revisions, bundle edits and model builds, which
//! is what makes "why did this wake me three weeks ago" answerable at the
//! template level.

use oarfish_core::{QuestionsHash, TemplateId};
use oarfish_mask::BundleHash;

/// Fixed widths of the leading key components.
pub const TEMPLATE_ID_LEN: usize = 32;
pub const QUESTIONS_HASH_LEN: usize = 16;
pub const BUNDLE_HASH_LEN: usize = 32;

/// The full storage key for one verdict.
pub fn verdict_key(
    template_id: &TemplateId,
    questions_hash: &QuestionsHash,
    bundle_hash: &BundleHash,
    model: &str,
) -> Vec<u8> {
    let mut key =
        Vec::with_capacity(TEMPLATE_ID_LEN + QUESTIONS_HASH_LEN + BUNDLE_HASH_LEN + model.len());
    key.extend_from_slice(template_id.as_bytes());
    key.extend_from_slice(questions_hash.as_bytes());
    key.extend_from_slice(bundle_hash.as_bytes());
    key.extend_from_slice(model.as_bytes());
    key
}

/// The 32-byte prefix covering every verdict for one template, across all
/// question revisions, bundle edits and model builds. The replay scan.
pub fn verdict_template_prefix(template_id: &TemplateId) -> [u8; TEMPLATE_ID_LEN] {
    *template_id.as_bytes()
}

/// The 80-byte prefix covering every verdict for one template under one
/// question set and one bundle, across model builds. The hot-path read.
pub fn verdict_questions_prefix(
    template_id: &TemplateId,
    questions_hash: &QuestionsHash,
    bundle_hash: &BundleHash,
) -> [u8; TEMPLATE_ID_LEN + QUESTIONS_HASH_LEN + BUNDLE_HASH_LEN] {
    let mut prefix = [0u8; TEMPLATE_ID_LEN + QUESTIONS_HASH_LEN + BUNDLE_HASH_LEN];
    prefix[..TEMPLATE_ID_LEN].copy_from_slice(template_id.as_bytes());
    prefix[TEMPLATE_ID_LEN..TEMPLATE_ID_LEN + QUESTIONS_HASH_LEN]
        .copy_from_slice(questions_hash.as_bytes());
    prefix[TEMPLATE_ID_LEN + QUESTIONS_HASH_LEN..].copy_from_slice(bundle_hash.as_bytes());
    prefix
}

/// Split a storage key back into its four components. `None` for a key that
/// is too short or whose model tail is not UTF-8.
pub fn parse_verdict_key(key: &[u8]) -> Option<(TemplateId, QuestionsHash, BundleHash, String)> {
    if key.len() < TEMPLATE_ID_LEN + QUESTIONS_HASH_LEN + BUNDLE_HASH_LEN {
        return None;
    }
    let mut id_bytes = [0u8; TEMPLATE_ID_LEN];
    id_bytes.copy_from_slice(&key[..TEMPLATE_ID_LEN]);
    let mut hash_bytes = [0u8; QUESTIONS_HASH_LEN];
    hash_bytes.copy_from_slice(&key[TEMPLATE_ID_LEN..TEMPLATE_ID_LEN + QUESTIONS_HASH_LEN]);
    let mut bundle_bytes = [0u8; BUNDLE_HASH_LEN];
    bundle_bytes.copy_from_slice(
        &key[TEMPLATE_ID_LEN + QUESTIONS_HASH_LEN
            ..TEMPLATE_ID_LEN + QUESTIONS_HASH_LEN + BUNDLE_HASH_LEN],
    );
    let model =
        std::str::from_utf8(&key[TEMPLATE_ID_LEN + QUESTIONS_HASH_LEN + BUNDLE_HASH_LEN..]).ok()?;
    Some((
        TemplateId::from_bytes(id_bytes),
        QuestionsHash::from_bytes(hash_bytes),
        BundleHash::from_bytes(bundle_bytes),
        model.to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle() -> BundleHash {
        BundleHash::from_bytes([7u8; 32])
    }

    #[test]
    fn keys_round_trip_through_parse() {
        let id = TemplateId::of("task <VAR:NUM> failed");
        let hash = QuestionsHash::of(b"{}");
        let key = verdict_key(&id, &hash, &bundle(), "typesafe/jev-1.13-20260917");
        let (back_id, back_hash, back_bundle, back_model) =
            parse_verdict_key(&key).expect("a key we built parses");
        assert_eq!(back_id, id);
        assert_eq!(back_hash, hash);
        assert_eq!(back_bundle, bundle());
        assert_eq!(back_model, "typesafe/jev-1.13-20260917");
    }

    #[test]
    fn a_changed_question_set_bundle_or_model_each_moves_the_key() {
        let id = TemplateId::of("task <VAR:NUM> failed");
        let base = verdict_key(&id, &QuestionsHash::of(b"a"), &bundle(), "model-a");
        assert_ne!(
            base,
            verdict_key(&id, &QuestionsHash::of(b"b"), &bundle(), "model-a")
        );
        assert_ne!(
            base,
            verdict_key(
                &id,
                &QuestionsHash::of(b"a"),
                &BundleHash::from_bytes([8u8; 32]),
                "model-a"
            )
        );
        assert_ne!(
            base,
            verdict_key(&id, &QuestionsHash::of(b"a"), &bundle(), "model-b")
        );
    }

    /// The load-bearing property: every key for one template sorts under its
    /// 32-byte prefix, so one prefix scan replays the whole history.
    #[test]
    fn every_key_for_a_template_sits_under_its_prefix() {
        let id = TemplateId::of("task <VAR:NUM> failed");
        let other = TemplateId::of("task <VAR:NUM> succeeded");
        let prefix = verdict_template_prefix(&id).to_vec();
        let key = verdict_key(&id, &QuestionsHash::of(b"a"), &bundle(), "model-a");
        let other_key = verdict_key(&other, &QuestionsHash::of(b"a"), &bundle(), "model-a");
        assert!(key.starts_with(&prefix));
        assert!(!other_key.starts_with(&prefix));
    }

    #[test]
    fn short_keys_do_not_parse() {
        assert!(parse_verdict_key(&[]).is_none());
        assert!(parse_verdict_key(&[0u8; 47]).is_none());
        assert!(parse_verdict_key(&[0u8; 79]).is_none());
    }
}
