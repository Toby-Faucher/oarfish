//! Applying a compiled bundle to one line.
//!
//! One pass. `captures_iter` yields each match in order with the span and the
//! group that produced it, so the template is built by copying the gaps between
//! matches and writing a placeholder for each match. There is no second scan and
//! no overlap resolution: the alternation already decided, by declaration order.

use std::ops::Range;

use crate::{Bundle, RESERVED_SLOT};

/// One variable found in one line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotMatch<'a> {
    /// The slot's name, borrowed from the bundle: `DEV` for `<VAR:DEV>`.
    pub name: &'a str,
    /// Byte range within the raw line.
    pub span: Range<usize>,
    /// What actually matched, borrowed from the raw line.
    pub value: &'a str,
}

/// The result of masking one line: the line itself, its template, and what filled it.
///
/// Borrows rather than owns the raw line, which is how invariant 3 is enforced
/// structurally instead of by discipline.
#[derive(Debug, Clone)]
pub struct Masked<'a> {
    raw: &'a str,
    template: String,
    matches: Vec<SlotMatch<'a>>,
}

impl<'a> Masked<'a> {
    /// The bytes that arrived, unchanged.
    pub fn raw(&self) -> &'a str {
        self.raw
    }

    /// The masked text. This is what Drain clusters in M2.
    pub fn template(&self) -> &str {
        &self.template
    }

    /// Every variable found, in the order it appeared.
    pub fn matches(&self) -> &[SlotMatch<'a>] {
        &self.matches
    }
}

impl Bundle {
    /// Mask one line. Pure, allocation-light, and on the every-line path: it
    /// makes no network call and never will.
    pub fn mask<'a>(&'a self, raw: &'a str) -> Masked<'a> {
        let mut template = String::with_capacity(raw.len());
        let mut matches = Vec::new();
        let mut cursor = 0;

        for caps in self.alternation.captures_iter(raw) {
            let whole = caps.get(0).expect("group 0 always participates");

            // Group 1 is the reserved alternate. Anything already masked is
            // copied through verbatim, which is what makes this idempotent.
            if caps.name(RESERVED_SLOT).is_some() {
                template.push_str(&raw[cursor..whole.end()]);
                cursor = whole.end();
                continue;
            }

            let name = self
                .slots()
                .iter()
                .map(|slot| slot.name.as_str())
                .find(|name| caps.name(name).is_some())
                .expect("one named group participates in every match");

            template.push_str(&raw[cursor..whole.start()]);
            template.push_str("<VAR:");
            template.push_str(name);
            template.push('>');

            matches.push(SlotMatch {
                name,
                span: whole.range(),
                value: whole.as_str(),
            });
            cursor = whole.end();
        }

        template.push_str(&raw[cursor..]);

        Masked {
            raw,
            template,
            matches,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::Bundle;

    /// IP4 before NUM. Reverse these two and the assertions below invert, which
    /// is the point: precedence lives in this file's order, nowhere else.
    const ORDERED: &str = r#"
version = 1

[[slot]]
name    = "IP4"
pattern = '\b(?:\d{1,3}\.){3}\d{1,3}\b'
why     = "dotted quad, before NUM so the octets are not eaten"

[[slot]]
name    = "NUM"
pattern = '\b\d+\b'
why     = "the catch-all, last on purpose"
"#;

    /// The same two patterns, declared the other way round. Nothing else differs:
    /// the slot *blocks* are swapped, not just their names.
    const REVERSED: &str = r#"
version = 1

[[slot]]
name    = "NUM"
pattern = '\b\d+\b'
why     = "declared first here on purpose, to prove that order is what decides"

[[slot]]
name    = "IP4"
pattern = '\b(?:\d{1,3}\.){3}\d{1,3}\b'
why     = "unreachable for a dotted quad while NUM sits above it"
"#;

    fn ordered() -> Bundle {
        Bundle::parse(ORDERED).expect("parse")
    }

    #[test]
    fn a_line_with_nothing_variable_is_returned_unchanged() {
        let bundle = ordered();
        let masked = bundle.mask("no variables here at all");
        assert_eq!(masked.template(), "no variables here at all");
        assert!(masked.matches().is_empty());
    }

    #[test]
    fn an_empty_line_masks_to_an_empty_line() {
        let bundle = ordered();
        assert_eq!(bundle.mask("").template(), "");
    }

    #[test]
    fn variables_become_placeholders() {
        let bundle = ordered();
        let masked = bundle.mask("conn from 10.0.0.5 port 4242");
        assert_eq!(masked.template(), "conn from <VAR:IP4> port <VAR:NUM>");
    }

    /// Declaration order is precedence. This asserts the `regex` crate's
    /// leftmost-first alternation semantics, which the whole design rests on.
    ///
    /// The second half is the real assertion: the same two patterns in the
    /// other order give a different answer. If that ever stops being true,
    /// every `why` note in the curated bundle is a lie.
    #[test]
    fn the_slot_declared_first_wins_an_overlap() {
        assert_eq!(
            ordered().mask("from 192.168.1.10 port 22").template(),
            "from <VAR:IP4> port <VAR:NUM>"
        );

        let inverted = Bundle::parse(REVERSED).expect("parse");
        assert_eq!(
            inverted.mask("from 192.168.1.10 port 22").template(),
            "from <VAR:NUM>.<VAR:NUM>.<VAR:NUM>.<VAR:NUM> port <VAR:NUM>",
            "with NUM declared first the octets are eaten one at a time"
        );
    }

    /// The reason `MASKED` exists. `IP4` contains a digit, so without a reserved
    /// first alternate the second pass turns `<VAR:IP4>` into `<VAR:IP<VAR:NUM>>`.
    #[test]
    fn masking_is_idempotent() {
        let bundle = ordered();
        for line in [
            "conn from 10.0.0.5 port 4242",
            "no variables here at all",
            "",
            "<VAR:NUM> already masked <VAR:IP4>",
        ] {
            let once = bundle.mask(line).template().to_owned();
            let twice = bundle.mask(&once).template().to_owned();
            assert_eq!(once, twice, "not idempotent for {line:?}");
        }
    }

    #[test]
    fn an_existing_placeholder_passes_through_and_is_not_reported_as_a_match() {
        let bundle = ordered();
        let masked = bundle.mask("already <VAR:IP4> done");
        assert_eq!(masked.template(), "already <VAR:IP4> done");
        assert!(
            masked.matches().is_empty(),
            "the reserved alternate is machinery, not a slot the caller sees"
        );
    }

    #[test]
    fn matches_carry_the_name_the_span_and_the_original_text() {
        let bundle = ordered();
        let line = "conn from 10.0.0.5 port 4242";
        let masked = bundle.mask(line);

        let names: Vec<_> = masked.matches().iter().map(|m| m.name).collect();
        assert_eq!(names, ["IP4", "NUM"]);

        let values: Vec<_> = masked.matches().iter().map(|m| m.value).collect();
        assert_eq!(values, ["10.0.0.5", "4242"]);

        for m in masked.matches() {
            assert_eq!(
                &line[m.span.clone()],
                m.value,
                "span must index the raw line"
            );
        }
    }

    /// Invariant 3 of the parent spec. At 3am the operator wants the bytes that
    /// arrived, not our interpretation of them.
    #[test]
    fn the_raw_line_survives_byte_for_byte() {
        let bundle = ordered();
        let line = "conn from 10.0.0.5 port 4242";
        assert_eq!(bundle.mask(line).raw(), line);
    }

    #[test]
    fn multibyte_input_does_not_panic_and_keeps_its_bytes() {
        let bundle = ordered();
        let line = "café ☕ device 42 naïve";
        let masked = bundle.mask(line);
        assert_eq!(masked.raw(), line);
        assert_eq!(masked.template(), "café ☕ device <VAR:NUM> naïve");
    }

    proptest::proptest! {
        /// Determinism, over anything at all. The cost model rests on the same
        /// line producing the same template every time.
        #[test]
        fn masking_is_deterministic(body in ".*") {
            let bundle = ordered();
            proptest::prop_assert_eq!(
                bundle.mask(&body).template().to_owned(),
                bundle.mask(&body).template().to_owned()
            );
        }

        #[test]
        fn the_raw_line_always_survives(body in ".*") {
            let bundle = ordered();
            proptest::prop_assert_eq!(bundle.mask(&body).raw(), body.as_str());
        }

        #[test]
        fn masking_is_always_idempotent(body in ".*") {
            let bundle = ordered();
            let once = bundle.mask(&body).template().to_owned();
            let twice = bundle.mask(&once).template().to_owned();
            proptest::prop_assert_eq!(once, twice);
        }
    }
}
