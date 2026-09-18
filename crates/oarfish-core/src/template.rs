//! Template identity: what a masked log line is, and what filled its slots.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use ts_rs::TS;

/// The stable identity of one masked template, and the key every cached
/// verdict hangs off.
///
/// Derived from the masked text alone, so it is reproducible from the log
/// corpus and nothing else. That is also the risk: change the mask bundle and
/// every id moves, orphaning every verdict.
///
/// The bundle version has to be recorded alongside each cached verdict and
/// checked on read; nothing enforces that yet. Get that wrong and the failure
/// is not a cache miss, it is a false hit: if a bundle change makes one
/// template mask down to text a different template already produced, the two
/// ids collide and the store serves the wrong verdict without complaint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, TS)]
#[ts(export, export_to = "oarfish.ts", type = "string")]
pub struct TemplateId([u8; 32]);

impl TemplateId {
    /// The only way to make one. Takes the *masked* template, never a raw line.
    pub fn of(masked: &str) -> Self {
        Self(*blake3::hash(masked.as_bytes()).as_bytes())
    }
}

impl fmt::Display for TemplateId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "t_{}", blake3::Hash::from(self.0).to_hex())
    }
}

/// Why a string could not be read as a `TemplateId`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TemplateIdError {
    #[error("template id must start with `t_`")]
    MissingPrefix,
    #[error("template id must carry 64 hex characters, found {0}")]
    WrongLength(usize),
    #[error("template id contains a character that is not hex")]
    NotHex,
}

impl FromStr for TemplateId {
    type Err = TemplateIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let hex = s.strip_prefix("t_").ok_or(TemplateIdError::MissingPrefix)?;
        if hex.len() != 64 {
            return Err(TemplateIdError::WrongLength(hex.len()));
        }
        let hash = blake3::Hash::from_hex(hex).map_err(|_| TemplateIdError::NotHex)?;
        Ok(Self(*hash.as_bytes()))
    }
}

impl Serialize for TemplateId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for TemplateId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

/// One variable slot in a masked template, with how often it has matched.
///
/// This is what `Template.svelte` draws as a dimension line beneath the
/// schematic, so `pattern` is carried verbatim for display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "oarfish.ts")]
pub struct Slot {
    /// The placeholder name, without decoration: `DEV` for `<VAR:DEV>`.
    pub name: String,
    /// The regex that matched, as written in the bundle.
    pub pattern: String,
    /// Occurrences seen. Exported as a TypeScript `number` rather than ts-rs's
    /// default `bigint` for 64-bit integers, because it crosses the wire as a
    /// JSON number. Counts stay far below 2^53.
    #[ts(type = "number")]
    pub seen: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole cost model rests on this: a template judged once stays judged,
    /// and it is found again by an id derived from its text alone.
    #[test]
    fn the_same_template_always_gets_the_same_id() {
        let masked = "EXT4-fs error (device <VAR:DEV>): inode #<VAR:NUM>";
        assert_eq!(TemplateId::of(masked), TemplateId::of(masked));
    }

    #[test]
    fn different_templates_get_different_ids() {
        assert_ne!(
            TemplateId::of("task <VAR:NUM> succeeded"),
            TemplateId::of("task <VAR:NUM> failed")
        );
    }

    #[test]
    fn ids_render_with_a_t_prefix_and_full_hex() {
        let rendered = TemplateId::of("anything").to_string();
        assert!(rendered.starts_with("t_"), "got {rendered}");
        assert_eq!(rendered.len(), 2 + 64);
        assert!(rendered[2..].chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[rstest::rstest]
    #[case("8f3c21a9", TemplateIdError::MissingPrefix)]
    #[case("t_abc", TemplateIdError::WrongLength(3))]
    #[case(
        "t_zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
        TemplateIdError::NotHex
    )]
    fn malformed_ids_are_rejected(#[case] input: &str, #[case] expected: TemplateIdError) {
        assert_eq!(input.parse::<TemplateId>().unwrap_err(), expected);
    }

    #[test]
    fn ids_serialize_as_a_plain_string() {
        let id = TemplateId::of("EXT4-fs error");
        let json = serde_json::to_string(&id).expect("serialize");
        assert_eq!(json, format!("\"{id}\""));
        assert_eq!(
            serde_json::from_str::<TemplateId>(&json).expect("deserialize"),
            id
        );
    }

    proptest::proptest! {
        #[test]
        fn ids_are_stable_across_calls(body in ".*") {
            proptest::prop_assert_eq!(TemplateId::of(&body), TemplateId::of(&body));
        }

        #[test]
        fn ids_round_trip_through_their_string_form(body in ".*") {
            let id = TemplateId::of(&body);
            proptest::prop_assert_eq!(id.to_string().parse::<TemplateId>().unwrap(), id);
        }
    }
}
