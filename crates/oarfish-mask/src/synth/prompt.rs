//! Turning candidates into the text sent to the model, and building the
//! one-slot fix-up prompt used on a validation failure.

use crate::synth::Candidate;
use crate::{BundleError, SlotDef};

/// Build the one prompt sent per synthesis run: every candidate as constant
/// context plus example values, never raw corpus lines — far less noisy for
/// the model, and far cheaper in tokens than sampling whole lines.
pub fn build_prompt(candidates: &[Candidate]) -> String {
    let mut body = String::from(
        "You are extending a log-masking bundle. Each numbered item below is a \
         position in a log template that is NOT yet recognized as a variable. \
         For each position, the surrounding constant tokens and a few real \
         values seen at that position are given.\n\n\
         For each position that represents a genuine variable (an id, a count, \
         a name, anything that changes between log lines of the same event), \
         propose one regex slot. Skip a position if the example values don't \
         actually share a describable pattern.\n\n",
    );
    for (i, candidate) in candidates.iter().enumerate() {
        body.push_str(&format!(
            "{}. context: {} <?> {}\n   examples: {}\n   occurrences: {}\n\n",
            i + 1,
            candidate.before.join(" "),
            candidate.after.join(" "),
            candidate.examples.join(", "),
            candidate.occurrences,
        ));
    }
    body.push_str(
        "Respond with a JSON array, nothing else: \
         [{\"name\": \"UPPER_SNAKE_NAME\", \"pattern\": \"regex\", \"why\": \"one sentence\"}]. \
         name must start with an uppercase letter and contain only A-Z, 0-9 and _. \
         pattern must be a valid Rust regex with no capturing groups \
         (use (?:...) for non-capturing groups). Return [] if none of the \
         positions are worth a slot.",
    );
    body
}

/// The one-slot retry prompt sent after a proposed slot fails validation.
/// Carries only the rejected slot and the error — not the full candidate
/// list — since fixing a regex syntax issue needs neither.
pub fn build_fix_prompt(slot: &SlotDef, error: &BundleError) -> String {
    format!(
        "This regex mask slot was rejected by validation:\n\
         {{\"name\": {:?}, \"pattern\": {:?}, \"why\": {:?}}}\n\n\
         Validation error: {error}\n\n\
         Return one corrected slot as a JSON object with the same three fields \
         (name, pattern, why), or the JSON value null to withdraw it. \
         Respond with only the JSON value, nothing else.",
        slot.name, slot.pattern, slot.why,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_candidate() -> Candidate {
        Candidate {
            before: vec!["batch".to_owned()],
            after: vec!["completed".to_owned()],
            examples: vec!["xk92".to_owned(), "qm14".to_owned()],
            occurrences: 4,
        }
    }

    #[test]
    fn the_prompt_carries_context_and_examples_but_no_raw_lines() {
        let prompt = build_prompt(&[a_candidate()]);
        assert!(prompt.contains("batch <?> completed"));
        assert!(prompt.contains("xk92, qm14"));
        assert!(prompt.contains("occurrences: 4"));
        assert!(prompt.contains("JSON array"));
    }

    #[test]
    fn the_fix_prompt_carries_the_slot_and_the_error() {
        let slot = SlotDef {
            name: "JOBID".to_owned(),
            pattern: "job-(a-z0-9)".to_owned(),
            why: "job ids".to_owned(),
        };
        let error = BundleError::CapturingGroup {
            name: "JOBID".to_owned(),
        };
        let prompt = build_fix_prompt(&slot, &error);
        assert!(prompt.contains("JOBID"));
        assert!(prompt.contains("job-(a-z0-9)"));
        assert!(prompt.contains("capture group"));
    }
}
