//! The mask bundle: what it looks like on disk, and what makes one valid.
//!
//! Order in the file is precedence. Everything here exists to keep that true
//! and to reject, at load, anything that would make the combined alternation in
//! `mask` attach the wrong name to the right span.

use std::collections::HashSet;
use std::fmt;

use serde::Deserialize;

/// The slot name the masker reserves for itself. A bundle may not declare it:
/// `mask` injects it as the first alternate so existing placeholders pass
/// through untouched, which is what makes masking idempotent.
pub const RESERVED_SLOT: &str = "MASKED";

/// The only bundle format version this build understands.
const SUPPORTED_VERSION: u32 = 1;

/// One slot definition, exactly as written in the bundle file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SlotDef {
    /// The placeholder name without decoration: `DEV` renders as `<VAR:DEV>`.
    pub name: String,
    /// The regex, as written. Carried verbatim because `core::Slot` displays it.
    pub pattern: String,
    /// Why this pattern exists and why it sits where it sits. Required: a
    /// pattern with no recorded reason is a pattern nobody dares touch.
    pub why: String,
}

/// The parsed, validated bundle, in file order.
#[derive(Debug, Clone)]
pub struct Bundle {
    version: u32,
    slots: Vec<SlotDef>,
    hash: BundleHash,
    pub(crate) alternation: regex::Regex,
}

#[derive(Debug, Deserialize)]
struct BundleFile {
    version: u32,
    #[serde(default, rename = "slot")]
    slots: Vec<SlotDef>,
}

impl Bundle {
    /// Parse and validate a bundle. Every rejectable thing is rejected here,
    /// because the alternative is a silent mis-mask at 3am.
    pub fn parse(source: &str) -> Result<Self, BundleError> {
        let file: BundleFile = toml::from_str(source)?;

        if file.version != SUPPORTED_VERSION {
            return Err(BundleError::UnsupportedVersion(file.version));
        }
        if file.slots.is_empty() {
            return Err(BundleError::Empty);
        }

        let mut seen = HashSet::new();
        for slot in &file.slots {
            if !is_valid_name(&slot.name) {
                return Err(BundleError::BadName {
                    name: slot.name.clone(),
                });
            }
            if slot.name == RESERVED_SLOT {
                return Err(BundleError::ReservedName {
                    name: slot.name.clone(),
                });
            }
            if !seen.insert(slot.name.clone()) {
                return Err(BundleError::DuplicateName {
                    name: slot.name.clone(),
                });
            }

            let compiled =
                regex::Regex::new(&slot.pattern).map_err(|source| BundleError::BadPattern {
                    name: slot.name.clone(),
                    source: Box::new(source),
                })?;

            // `captures_len` counts the implicit whole-match group, so a clean
            // pattern reports exactly 1. Anything higher smuggled in a group.
            if compiled.captures_len() != 1 {
                return Err(BundleError::CapturingGroup {
                    name: slot.name.clone(),
                });
            }
        }

        let mut alternates = vec![format!("(?<{RESERVED_SLOT}><VAR:[A-Z][A-Z0-9_]*>)")];
        alternates.extend(
            file.slots
                .iter()
                .map(|slot| format!("(?<{}>{})", slot.name, slot.pattern)),
        );

        // The size limit is set explicitly so that growth from source packs
        // later fails here, loudly, rather than at some arbitrary later point.
        let alternation = regex::RegexBuilder::new(&alternates.join("|"))
            .size_limit(16 * 1024 * 1024)
            .build()
            .map_err(|source| BundleError::TooLarge {
                source: Box::new(source),
            })?;

        Ok(Self {
            version: file.version,
            slots: file.slots,
            hash: BundleHash(*blake3::hash(source.as_bytes()).as_bytes()),
            alternation,
        })
    }

    pub fn version(&self) -> u32 {
        self.version
    }

    /// The slots, in file order. That order is precedence; do not sort this.
    pub fn slots(&self) -> &[SlotDef] {
        &self.slots
    }

    /// The identity the verdict cache keys against. Change the bundle, and every
    /// template id moves; this is how M4 notices instead of serving stale answers.
    pub fn hash(&self) -> BundleHash {
        self.hash
    }
}

fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_uppercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// A bundle's identity: blake3 over its exact text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BundleHash([u8; 32]);

impl BundleHash {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for BundleHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "b_{}", blake3::Hash::from(self.0).to_hex())
    }
}

/// Why a bundle could not be loaded. Every variant names the slot at fault,
/// because a bundle is edited by a human under time pressure.
#[derive(Debug, thiserror::Error)]
pub enum BundleError {
    #[error("the bundle is not valid TOML: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("this build reads bundle version {SUPPORTED_VERSION}, the bundle declares {0}")]
    UnsupportedVersion(u32),
    #[error("the bundle declares no slots")]
    Empty,
    #[error("slot name `{name}` must start with an uppercase letter and hold only A-Z, 0-9 and _")]
    BadName { name: String },
    #[error("slot name `{name}` is reserved by the masker")]
    ReservedName { name: String },
    #[error("slot `{name}` is declared twice")]
    DuplicateName { name: String },
    #[error("slot `{name}` has a pattern that does not compile: {source}")]
    BadPattern {
        name: String,
        source: Box<regex::Error>,
    },
    #[error("slot `{name}` introduces a capture group; write `(?:...)` instead")]
    CapturingGroup { name: String },
    #[error("the bundle's patterns are too large to compile together: {source}")]
    TooLarge { source: Box<regex::Error> },
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
version = 1

[[slot]]
name    = "IP4"
pattern = '(?:\d{1,3}\.){3}\d{1,3}'
why     = "dotted quad"

[[slot]]
name    = "NUM"
pattern = '\d+'
why     = "the catch-all, declared last on purpose"
"#;

    #[test]
    fn a_good_bundle_parses_and_keeps_file_order() {
        let bundle = Bundle::parse(GOOD).expect("parse");
        assert_eq!(bundle.version(), 1);
        let names: Vec<_> = bundle.slots().iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["IP4", "NUM"]);
    }

    /// Order in the file is precedence, so it has to survive parsing unreordered.
    /// If this ever sorts, every IP address in the corpus quietly becomes numbers.
    #[test]
    fn slots_are_not_reordered() {
        let reversed = GOOD.replace("\"IP4\"", "\"ZZZ\"");
        let bundle = Bundle::parse(&reversed).expect("parse");
        assert_eq!(bundle.slots()[0].name, "ZZZ");
    }

    #[test]
    fn an_unknown_version_is_rejected() {
        let wrong = GOOD.replace("version = 1", "version = 2");
        assert!(matches!(
            Bundle::parse(&wrong),
            Err(BundleError::UnsupportedVersion(2))
        ));
    }

    #[test]
    fn an_empty_bundle_is_rejected() {
        assert!(matches!(
            Bundle::parse("version = 1\n"),
            Err(BundleError::Empty)
        ));
    }

    #[rstest::rstest]
    #[case("lowercase")]
    #[case("Has Space")]
    #[case("9LEADING")]
    #[case("")]
    fn a_malformed_name_is_rejected(#[case] name: &str) {
        let src = GOOD.replace("\"IP4\"", &format!("\"{name}\""));
        assert!(
            matches!(Bundle::parse(&src), Err(BundleError::BadName { .. })),
            "expected BadName for {name:?}"
        );
    }

    #[test]
    fn the_reserved_name_is_rejected() {
        let src = GOOD.replace("\"IP4\"", "\"MASKED\"");
        assert!(matches!(
            Bundle::parse(&src),
            Err(BundleError::ReservedName { .. })
        ));
    }

    #[test]
    fn a_duplicate_name_is_rejected() {
        let src = GOOD.replace("\"IP4\"", "\"NUM\"");
        assert!(matches!(
            Bundle::parse(&src),
            Err(BundleError::DuplicateName { .. })
        ));
    }

    #[test]
    fn an_uncompilable_pattern_is_rejected() {
        let src = GOOD.replace(r"'\d+'", "'[unclosed'");
        assert!(matches!(
            Bundle::parse(&src),
            Err(BundleError::BadPattern { .. })
        ));
    }

    /// A capture group smuggled into a pattern shifts every group index in the
    /// combined alternation Task 2 builds, so the wrong slot name gets attached
    /// to the right span. Catch it here, where the error can name the slot.
    #[test]
    fn a_pattern_with_its_own_capture_group_is_rejected() {
        let src = GOOD.replace(r"'\d+'", r"'(\d+)'");
        match Bundle::parse(&src) {
            Err(BundleError::CapturingGroup { name }) => assert_eq!(name, "NUM"),
            other => panic!("expected CapturingGroup, got {other:?}"),
        }
    }

    #[test]
    fn a_non_capturing_group_is_fine() {
        let src = GOOD.replace(r"'\d+'", r"'(?:\d+)'");
        assert!(Bundle::parse(&src).is_ok());
    }

    #[test]
    fn the_hash_is_stable_and_renders_with_a_b_prefix() {
        let hash = Bundle::parse(GOOD).expect("parse").hash();
        assert_eq!(hash, Bundle::parse(GOOD).expect("parse").hash());

        let rendered = hash.to_string();
        assert!(rendered.starts_with("b_"), "got {rendered}");
        assert_eq!(rendered.len(), 2 + 64);
        assert!(rendered[2..].chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// This is the §13 guard. If a bundle change did not move the hash, a
    /// populated verdict cache would keep serving answers keyed to template ids
    /// that no longer exist.
    #[test]
    fn changing_a_pattern_moves_the_hash() {
        let changed = GOOD.replace(r"'\d+'", r"'\d{2,}'");
        assert_ne!(
            Bundle::parse(GOOD).expect("parse").hash(),
            Bundle::parse(&changed).expect("parse").hash()
        );
    }
}
