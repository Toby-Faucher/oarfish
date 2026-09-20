//! Combining synthesized slots into a starting bundle.
//!
//! Insertion point is "immediately before the starting bundle's last slot" —
//! not a search for a slot literally named `NUM`, which breaks the moment
//! that slot is renamed. The rule instead trusts the same convention the
//! curated bundle's own header comment already requires: specific patterns
//! above general ones, always. A bundle that follows that convention ends
//! with its single most general pattern, so inserting immediately before it
//! is the mechanical expression of "synthesized slots are more specific than
//! any generic catch-all."

use crate::bundle::slots_to_toml;
use crate::{Bundle, BundleError, SlotDef};

/// Insert `synthesized` immediately before `starting`'s last slot, then
/// validate the result through [`Bundle::parse`] — the same structural
/// checks every bundle on disk goes through, synthesized slots included.
pub fn merge(starting: &Bundle, synthesized: &[SlotDef]) -> Result<Bundle, BundleError> {
    let existing = starting.slots();
    let split = existing.len().saturating_sub(1);
    let mut combined: Vec<&SlotDef> = Vec::with_capacity(existing.len() + synthesized.len());
    combined.extend(existing[..split].iter());
    combined.extend(synthesized.iter());
    combined.extend(existing[split..].iter());

    let text = slots_to_toml(starting.version(), &combined);
    Bundle::parse(&text)
}

#[cfg(test)]
mod tests {
    use crate::curated;

    use super::*;

    fn a_slot(name: &str, pattern: &str) -> SlotDef {
        SlotDef {
            name: name.to_owned(),
            pattern: pattern.to_owned(),
            why: "test slot".to_owned(),
        }
    }

    #[test]
    fn synthesized_slots_land_before_the_last_curated_slot() {
        let merged =
            merge(curated(), &[a_slot("JOBID", r"job-[a-z0-9]{4}")]).expect("a valid slot merges");
        let names: Vec<&str> = merged.slots().iter().map(|s| s.name.as_str()).collect();
        let jobid_pos = names
            .iter()
            .position(|n| *n == "JOBID")
            .expect("JOBID present");
        // NUM is the curated bundle's last slot; JOBID must sit before it.
        assert_eq!(names.last(), Some(&"NUM"));
        assert!(jobid_pos < names.len() - 1);
    }

    #[test]
    fn an_empty_synthesized_list_leaves_the_bundle_unchanged() {
        let merged = merge(curated(), &[]).expect("empty merges");
        assert_eq!(merged.slots(), curated().slots());
    }

    #[test]
    fn a_structurally_invalid_slot_is_rejected() {
        let err = merge(curated(), &[a_slot("JOBID", "job-(a-z0-9)")])
            .expect_err("a capturing group must be rejected");
        assert!(
            matches!(err, BundleError::CapturingGroup { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn a_name_colliding_with_a_curated_slot_is_rejected() {
        let err = merge(curated(), &[a_slot("NUM", r"\d+")])
            .expect_err("a duplicate name must be rejected");
        assert!(
            matches!(err, BundleError::DuplicateName { .. }),
            "got {err:?}"
        );
    }

    proptest::proptest! {
        /// Regardless of how many synthesized slots are proposed, they always
        /// land strictly before the starting bundle's last slot and never
        /// disturb the relative order of the starting bundle's own slots.
        #[test]
        fn synthesized_slots_never_move_past_the_last_slot(
            count in 1usize..5,
        ) {
            let slots: Vec<SlotDef> = (0..count)
                .map(|i| a_slot(&format!("GEN{i}"), &format!("gen{i}-[a-z]+")))
                .collect();
            let merged = merge(curated(), &slots).expect("generated slots are valid");
            let names: Vec<&str> = merged.slots().iter().map(|s| s.name.as_str()).collect();
            proptest::prop_assert_eq!(names.last(), Some(&"NUM"));
            let curated_names: Vec<&str> =
                curated().slots().iter().map(|s| s.name.as_str()).collect();
            let without_last = &curated_names[..curated_names.len() - 1];
            proptest::prop_assert_eq!(&names[..without_last.len()], without_last);
        }
    }
}
