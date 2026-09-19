//! The storage key for cached merge decisions: five components, ordered wide first.
//!
//! ```text
//! id_lo (32B) ++ id_hi (32B) ++ questions_hash (16B) ++ bundle_hash (32B) ++ resolved_model_id (variable)
//! ```
//!
//! The pair is canonically ordered, so `(A, B)` and `(B, A)` are one row and
//! a close pair is judged once rather than twice. The trailing three
//! components are the discipline M4 established: a decision is valid only for
//! the question that produced it, the bundle that masked its templates, and
//! the model build that answered.
//!
//! The bundle component earns its place for M4's reason — `TemplateId::of`
//! hashes masked text, so two different raw lines can carry the same id under
//! different bundles, and without it the store could serve one pair's decision
//! for another's. The stakes here are higher than for a verdict: invariant 4
//! names the danger, and a stale merge served after a bundle edit silently
//! deletes the alarm.
//!
//! With the fixed-width pair first, a prefix scan on `id_lo ++ id_hi` returns
//! every decision that pair has ever received, across question revisions,
//! bundle edits and model builds.

use oarfish_core::{QuestionsHash, TemplateId};
use oarfish_mask::BundleHash;

/// Fixed widths of the leading key components.
pub const ID_LO_LEN: usize = 32;
pub const ID_HI_LEN: usize = 32;
pub const PAIR_PREFIX_LEN: usize = ID_LO_LEN + ID_HI_LEN;

/// One pair in canonical order: the lesser id first, so `(A, B)` and `(B, A)`
/// key identically and the pair is asked once.
pub fn canonical_pair(
    a: TemplateId,
    template_a: &str,
    b: TemplateId,
    template_b: &str,
) -> (TemplateId, String, TemplateId, String) {
    if a <= b {
        (a, template_a.to_owned(), b, template_b.to_owned())
    } else {
        (b, template_b.to_owned(), a, template_a.to_owned())
    }
}

/// The full storage key for one merge decision.
pub fn merge_key(
    id_lo: &TemplateId,
    id_hi: &TemplateId,
    questions_hash: &QuestionsHash,
    bundle_hash: &BundleHash,
    model: &str,
) -> Vec<u8> {
    let mut key = Vec::with_capacity(
        ID_LO_LEN
            + ID_HI_LEN
            + crate::keys::QUESTIONS_HASH_LEN
            + crate::keys::BUNDLE_HASH_LEN
            + model.len(),
    );
    key.extend_from_slice(id_lo.as_bytes());
    key.extend_from_slice(id_hi.as_bytes());
    key.extend_from_slice(questions_hash.as_bytes());
    key.extend_from_slice(bundle_hash.as_bytes());
    key.extend_from_slice(model.as_bytes());
    key
}

/// The 64-byte prefix covering every decision for one pair, across question
/// revisions, bundle edits and model builds. The replay scan and the hot-path
/// read scope.
pub fn merge_pair_prefix(id_lo: &TemplateId, id_hi: &TemplateId) -> [u8; PAIR_PREFIX_LEN] {
    let mut prefix = [0u8; PAIR_PREFIX_LEN];
    prefix[..ID_LO_LEN].copy_from_slice(id_lo.as_bytes());
    prefix[ID_LO_LEN..].copy_from_slice(id_hi.as_bytes());
    prefix
}

/// Split a storage key back into its five components. `None` for a key that
/// is too short, unordered, or whose model tail is not UTF-8.
pub fn parse_merge_key(
    key: &[u8],
) -> Option<(TemplateId, TemplateId, QuestionsHash, BundleHash, String)> {
    use crate::keys::{BUNDLE_HASH_LEN, QUESTIONS_HASH_LEN, TEMPLATE_ID_LEN};
    let fixed = TEMPLATE_ID_LEN + TEMPLATE_ID_LEN + QUESTIONS_HASH_LEN + BUNDLE_HASH_LEN;
    if key.len() < fixed {
        return None;
    }
    let mut lo_bytes = [0u8; TEMPLATE_ID_LEN];
    lo_bytes.copy_from_slice(&key[..TEMPLATE_ID_LEN]);
    let mut hi_bytes = [0u8; TEMPLATE_ID_LEN];
    hi_bytes.copy_from_slice(&key[TEMPLATE_ID_LEN..2 * TEMPLATE_ID_LEN]);
    let mut hash_bytes = [0u8; QUESTIONS_HASH_LEN];
    hash_bytes.copy_from_slice(&key[2 * TEMPLATE_ID_LEN..2 * TEMPLATE_ID_LEN + QUESTIONS_HASH_LEN]);
    let mut bundle_bytes = [0u8; BUNDLE_HASH_LEN];
    bundle_bytes.copy_from_slice(
        &key[2 * TEMPLATE_ID_LEN + QUESTIONS_HASH_LEN
            ..2 * TEMPLATE_ID_LEN + QUESTIONS_HASH_LEN + BUNDLE_HASH_LEN],
    );
    let model = std::str::from_utf8(&key[fixed..]).ok()?;
    let lo = TemplateId::from_bytes(lo_bytes);
    let hi = TemplateId::from_bytes(hi_bytes);
    if lo > hi {
        return None;
    }
    Some((
        lo,
        hi,
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

    fn pair() -> (TemplateId, TemplateId) {
        let a = TemplateId::of("backup done files ok");
        let b = TemplateId::of("backup done files ok extra");
        if a <= b { (a, b) } else { (b, a) }
    }

    #[test]
    fn canonical_ordering_makes_ab_and_ba_one_row() {
        let a = TemplateId::of("task <VAR:NUM> failed");
        let b = TemplateId::of("task <VAR:NUM> failed twice");
        let (lo, _, hi, _) = canonical_pair(a, "ta", b, "tb");
        let (lo2, _, hi2, _) = canonical_pair(b, "tb", a, "ta");
        assert_eq!((lo, hi), (lo2, hi2));
        assert!(lo <= hi);
    }

    #[test]
    fn keys_round_trip_through_parse() {
        let (lo, hi) = pair();
        let hash = QuestionsHash::of(b"merge");
        let key = merge_key(&lo, &hi, &hash, &bundle(), "typesafe/jev-1.13-20260917");
        let (back_lo, back_hi, back_hash, back_bundle, back_model) =
            parse_merge_key(&key).expect("a key we built parses");
        assert_eq!(back_lo, lo);
        assert_eq!(back_hi, hi);
        assert_eq!(back_hash, hash);
        assert_eq!(back_bundle, bundle());
        assert_eq!(back_model, "typesafe/jev-1.13-20260917");
    }

    #[test]
    fn a_changed_question_bundle_or_model_each_moves_the_key() {
        let (lo, hi) = pair();
        let base = merge_key(&lo, &hi, &QuestionsHash::of(b"a"), &bundle(), "model-a");
        assert_ne!(
            base,
            merge_key(&lo, &hi, &QuestionsHash::of(b"b"), &bundle(), "model-a")
        );
        assert_ne!(
            base,
            merge_key(
                &lo,
                &hi,
                &QuestionsHash::of(b"a"),
                &BundleHash::from_bytes([8u8; 32]),
                "model-a"
            )
        );
        assert_ne!(
            base,
            merge_key(&lo, &hi, &QuestionsHash::of(b"a"), &bundle(), "model-b")
        );
    }

    #[test]
    fn every_key_for_a_pair_sits_under_its_prefix() {
        let (lo, hi) = pair();
        let other = TemplateId::of("something else entirely here");
        let (other_lo, other_hi) = if lo <= other {
            (lo, other)
        } else {
            (other, lo)
        };
        let prefix = merge_pair_prefix(&lo, &hi).to_vec();
        let key = merge_key(&lo, &hi, &QuestionsHash::of(b"a"), &bundle(), "model-a");
        let other_key = merge_key(
            &other_lo,
            &other_hi,
            &QuestionsHash::of(b"a"),
            &bundle(),
            "model-a",
        );
        assert!(key.starts_with(&prefix));
        assert!(!other_key.starts_with(&prefix));
    }

    #[test]
    fn unordered_and_short_keys_do_not_parse() {
        let (lo, hi) = pair();
        let key = merge_key(&hi, &lo, &QuestionsHash::of(b"a"), &bundle(), "m");
        assert!(parse_merge_key(&key).is_none());
        assert!(parse_merge_key(&[]).is_none());
        assert!(parse_merge_key(&[0u8; 95]).is_none());
    }
}
