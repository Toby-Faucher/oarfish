//! Perceived severity.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// A subset of ITU-T X.733, because that is the vocabulary a NOC already
/// speaks.
///
/// X.733's `indeterminate` is deliberately dropped: if oarfish cannot decide,
/// that is a confidence signal, not a severity.
///
/// Variants are declared in escalation order so the derived `Ord` is the
/// comparison the alarm engine actually wants. Do not reorder them to match
/// the board's display order, which runs the other way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "oarfish.ts")]
pub enum Severity {
    Cleared,
    Info,
    Minor,
    Major,
    Critical,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Escalation is a comparison, not a lookup table: the alarm engine asks
    /// whether a severity went *up*. Declaration order is that order.
    #[test]
    fn severity_orders_by_escalation() {
        assert!(Severity::Critical > Severity::Major);
        assert!(Severity::Major > Severity::Minor);
        assert!(Severity::Minor > Severity::Info);
        assert!(Severity::Info > Severity::Cleared);
    }

    #[rstest::rstest]
    #[case(Severity::Critical, r#""critical""#)]
    #[case(Severity::Major, r#""major""#)]
    #[case(Severity::Minor, r#""minor""#)]
    #[case(Severity::Info, r#""info""#)]
    #[case(Severity::Cleared, r#""cleared""#)]
    fn severity_serializes_as_a_lowercase_string(#[case] severity: Severity, #[case] json: &str) {
        assert_eq!(serde_json::to_string(&severity).expect("serialize"), json);
        assert_eq!(
            serde_json::from_str::<Severity>(json).expect("deserialize"),
            severity
        );
    }
}
