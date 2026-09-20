# Mask Synthesis Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `oarfish masks synthesize`, a CLI subcommand that finds log patterns
the mask bundle doesn't cover, proposes regex slots for them via an LLM, validates
and merges them, and writes a bundle the daemon can load with a new `--bundle` flag.

**Architecture:** A new transport-only crate `oarfish-synth` (chat/completions,
mirroring `oarfish-jev`'s shape for the decisions endpoint). `oarfish-mask` gains a
`synth` module: Drain-based candidate detection, prompt building, a `Synthesize`
trait implemented for `oarfish_synth::Client`, and merge/validation logic. The
`oarfish` binary gains the subcommand and the daemon's `--bundle` flag — both wiring
only, no new pipeline logic in the binary itself.

**Tech Stack:** Rust 1.98, edition 2024. `reqwest` for HTTP, `serde`/`serde_json` for
the LLM's JSON response, `toml` for the bundle format, `clap` (derive) for the CLI,
`wiremock` for transport tests, `proptest` for the merge-order invariant, `insta` for
candidate-detection snapshots.

**Spec:** `docs/specs/2026-09-19-mask-synthesis-design.md`

## Global Constraints

- `#![forbid(unsafe_code)]` at the top of every crate, new or touched.
- Libraries use `thiserror` for errors; the `oarfish` binary uses `anyhow`.
- Tests live beside the code they test (`#[cfg(test)] mod tests` at the bottom of the
  file), per this codebase's convention — no separate test-only files unless a task
  below says otherwise.
- Augment only: synthesized slots are appended, the curated bundle's own slots are
  never rewritten (spec §2).
- `--model` has no default (spec §3): the operator names one explicitly.
- Every new crate dependency is added to `[workspace.dependencies]` in the root
  `Cargo.toml` first if it isn't already there, then referenced with `.workspace = true`
  in the crate's own `Cargo.toml` — this is how every existing crate in the workspace
  is wired.
- `cargo fmt --all` and `cargo clippy --workspace --all-targets -- -D warnings` must
  both pass before any task is considered done — this is what CI runs.
- No LLM call ever goes on the every-line path. Everything this plan builds runs only
  from the `masks synthesize` CLI subcommand, never from the daemon's ingest loop.

---

## Task 1: `SlotDef` gains `Serialize`, and `Bundle` gains `to_toml`

Both the merge logic (Task 3) and the CLI's `--out` write (Task 8) need to turn a
list of slots back into bundle TOML text. This task builds that once, in the crate
that already owns the format, so nothing downstream reinvents TOML serialization.

**Files:**
- Modify: `crates/oarfish-mask/src/bundle.rs`

**Interfaces:**
- Produces: `pub(crate) fn slots_to_toml(version: u32, slots: &[&SlotDef]) -> String`
  (used by Task 3's `merge`), `pub fn Bundle::to_toml(&self) -> String`

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block at the bottom of
`crates/oarfish-mask/src/bundle.rs`:

```rust
#[test]
fn a_parsed_bundle_round_trips_through_to_toml() {
    let bundle = Bundle::parse(
        r#"
        version = 1

        [[slot]]
        name    = "DEV"
        pattern = '/dev/[a-z0-9]+'
        why     = "device paths"

        [[slot]]
        name    = "NUM"
        pattern = '\d+'
        why     = "bare numbers, last because it is the most general"
        "#,
    )
    .expect("valid bundle");

    let text = bundle.to_toml();
    let reparsed = Bundle::parse(&text).expect("serialized bundle re-parses");

    assert_eq!(reparsed.version(), bundle.version());
    assert_eq!(reparsed.slots(), bundle.slots());
    assert_eq!(reparsed.hash(), bundle.hash());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p oarfish-mask a_parsed_bundle_round_trips_through_to_toml`
Expected: FAIL with "no method named `to_toml` found for struct `Bundle`"

- [ ] **Step 3: Write minimal implementation**

In `crates/oarfish-mask/src/bundle.rs`, change the derive on `SlotDef`:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotDef {
```

Update the `use` line at the top of the file:

```rust
use serde::{Deserialize, Serialize};
```

Add this function above `impl Bundle` (module-level, not inside the `impl` block):

```rust
/// Serialize `slots` back to the bundle TOML format, in the given order.
/// Shared by [`Bundle::to_toml`] (an already-valid bundle) and merge
/// validation (a candidate combination that might not parse yet).
pub(crate) fn slots_to_toml(version: u32, slots: &[&SlotDef]) -> String {
    #[derive(Serialize)]
    struct File<'a> {
        version: u32,
        #[serde(rename = "slot")]
        slot: &'a [&'a SlotDef],
    }
    toml::to_string(&File { version, slot: slots }).expect("a slot list always serializes")
}
```

Add this method inside the existing `impl Bundle { ... }` block, after `hash()`:

```rust
    /// The bundle's slots, serialized back to the on-disk TOML format. What
    /// `masks synthesize` writes to `--out` (Task 8).
    pub fn to_toml(&self) -> String {
        let refs: Vec<&SlotDef> = self.slots.iter().collect();
        slots_to_toml(self.version, &refs)
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p oarfish-mask a_parsed_bundle_round_trips_through_to_toml`
Expected: PASS

- [ ] **Step 5: Run the full existing test suite for this crate**

Run: `cargo test -p oarfish-mask`
Expected: PASS, all prior tests unaffected — adding `Serialize` to `SlotDef` and a
new method changes no existing behavior.

- [ ] **Step 6: Commit**

```bash
git add crates/oarfish-mask/src/bundle.rs
git commit -m "feat(mask): serialize a bundle's slots back to TOML"
```

---

## Task 2: `synth::candidates` — finding unmasked positions via Drain

This is the core mechanism from spec §4: mask the corpus, cluster it with a
throwaway `Drain`, and find token positions Drain generalized to `<*>` that aren't
already a `<VAR:NAME>` placeholder.

**Files:**
- Create: `crates/oarfish-mask/src/synth/mod.rs`
- Create: `crates/oarfish-mask/src/synth/candidates.rs`
- Modify: `crates/oarfish-mask/Cargo.toml` (add `oarfish-drain`)
- Modify: `crates/oarfish-mask/src/lib.rs` (declare the `synth` module)
- Modify: root `Cargo.toml` (no change needed — `oarfish-drain` is already a
  workspace dependency)

**Interfaces:**
- Consumes: `oarfish_drain::{Config, Drain}` (`Drain::new`, `Drain::train`,
  `Drain::clusters`), `crate::Bundle` (`Bundle::mask`)
- Produces: `pub struct Candidate { pub before: Vec<String>, pub after: Vec<String>,
  pub examples: Vec<String>, pub occurrences: u64 }`, `pub fn find_candidates(bundle:
  &Bundle, corpus: &[String], min_occurrences: u64) -> Vec<Candidate>` (sorted
  highest-occurrence first)

- [ ] **Step 1: Add the dependency**

In `crates/oarfish-mask/Cargo.toml`, add to `[dependencies]`:

```toml
oarfish-drain.workspace = true
```

- [ ] **Step 2: Declare the module**

In `crates/oarfish-mask/src/lib.rs`, add after the existing `mod` declarations:

```rust
mod synth;

pub use synth::{Candidate, find_candidates};
```

(Leave the rest of `lib.rs`'s doc comment and other `pub use` lines as they are for
now — Task 9 updates the doc comment once the whole module exists.)

Create `crates/oarfish-mask/src/synth/mod.rs`:

```rust
mod candidates;

pub use candidates::{Candidate, find_candidates};
```

- [ ] **Step 3: Write the failing test**

Create `crates/oarfish-mask/src/synth/candidates.rs` with just the test first:

```rust
//! Finding what the bundle doesn't cover: mask the corpus, cluster it with a
//! throwaway Drain table, and find token positions Drain generalized to
//! `<*>` that aren't already a `<VAR:NAME>` placeholder. That is mechanical
//! evidence of a variable the bundle doesn't know about — not a guess from
//! sampling raw lines.

#[cfg(test)]
mod tests {
    use crate::curated;

    use super::*;

    fn corpus() -> Vec<String> {
        vec![
            "batch xk92 completed".to_owned(),
            "batch qm14 completed".to_owned(),
            "batch zt77 completed".to_owned(),
            "batch xk92 completed".to_owned(),
        ]
    }

    #[test]
    fn an_unmasked_position_becomes_one_candidate_with_its_examples() {
        let candidates = find_candidates(curated(), &corpus(), 3);
        assert_eq!(candidates.len(), 1, "got {candidates:?}");
        let candidate = &candidates[0];
        assert_eq!(candidate.before, vec!["batch".to_owned()]);
        assert_eq!(candidate.after, vec!["completed".to_owned()]);
        assert_eq!(candidate.occurrences, 4);
        for value in ["xk92", "qm14", "zt77"] {
            assert!(
                candidate.examples.contains(&value.to_owned()),
                "missing {value} in {:?}",
                candidate.examples
            );
        }
    }

    #[test]
    fn below_min_occurrences_yields_nothing() {
        let candidates = find_candidates(curated(), &corpus(), 5);
        assert!(candidates.is_empty(), "got {candidates:?}");
    }

    #[test]
    fn a_position_the_bundle_already_covers_is_never_a_candidate() {
        // IP4 already masks this uniformly; Drain never generalizes it,
        // since every line gets the identical `<VAR:IP4>` text.
        let corpus = vec![
            "connect from 10.0.0.1 refused".to_owned(),
            "connect from 10.0.0.2 refused".to_owned(),
            "connect from 10.0.0.3 refused".to_owned(),
        ];
        let candidates = find_candidates(curated(), &corpus, 3);
        assert!(candidates.is_empty(), "got {candidates:?}");
    }
}
```

- [ ] **Step 4: Run test to verify it fails**

Run: `cargo test -p oarfish-mask synth::candidates`
Expected: FAIL to compile — `find_candidates`, `Candidate` don't exist yet.

- [ ] **Step 5: Write the implementation**

Add above the `#[cfg(test)]` block in `crates/oarfish-mask/src/synth/candidates.rs`:

```rust
use std::collections::HashMap;

use oarfish_drain::{Config, Drain};

use crate::Bundle;

/// How many tokens of constant context to keep on each side of a candidate
/// position.
const CONTEXT_WIDTH: usize = 1;
/// At most this many distinct example values are kept per candidate.
const MAX_EXAMPLES: usize = 5;

/// One position in a cluster's template the bundle doesn't already mask: the
/// constant tokens around it, a handful of the real values seen there, and
/// how many corpus lines contributed to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Tokens immediately before the position.
    pub before: Vec<String>,
    /// Tokens immediately after the position.
    pub after: Vec<String>,
    /// Distinct real values seen at this position, first-seen order.
    pub examples: Vec<String>,
    /// How many corpus lines matched the cluster this position came from.
    pub occurrences: u64,
}

/// Mask every corpus line with `bundle`, cluster the result with a throwaway
/// Drain table at the daemon's own default config, and find every token
/// position Drain generalized to `<*>` that isn't already a `<VAR:NAME>`
/// placeholder. Kept above `min_occurrences`, highest-occurrence first.
pub fn find_candidates(bundle: &Bundle, corpus: &[String], min_occurrences: u64) -> Vec<Candidate> {
    let mut drain = Drain::new(Config::default()).expect("default config is valid");
    let mut lines_by_cluster: HashMap<u64, Vec<Vec<String>>> = HashMap::new();

    for line in corpus {
        let masked = bundle.mask(line);
        let tokens: Vec<String> = masked
            .template()
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        let assignment = drain.train(masked.template());
        lines_by_cluster.entry(assignment.seq).or_default().push(tokens);
    }

    let mut candidates = Vec::new();
    for cluster in drain.clusters() {
        if cluster.size < min_occurrences {
            continue;
        }
        let Some(lines) = lines_by_cluster.get(&cluster.seq) else {
            continue;
        };
        for (position, token) in cluster.tokens.iter().enumerate() {
            if token != "<*>" {
                continue;
            }
            let mut examples: Vec<String> = Vec::new();
            for line in lines {
                let Some(value) = line.get(position) else {
                    continue;
                };
                if value.starts_with("<VAR:") && value.ends_with('>') {
                    // Two different already-masked slot types happened to
                    // land at the same position across a close cluster;
                    // that is not a gap the bundle needs filling.
                    continue;
                }
                if !examples.contains(value) {
                    examples.push(value.clone());
                }
                if examples.len() >= MAX_EXAMPLES {
                    break;
                }
            }
            if examples.is_empty() {
                continue;
            }
            let before_start = position.saturating_sub(CONTEXT_WIDTH);
            let before = cluster.tokens[before_start..position].to_vec();
            let after_end = (position + 1 + CONTEXT_WIDTH).min(cluster.tokens.len());
            let after = cluster.tokens[position + 1..after_end].to_vec();
            candidates.push(Candidate {
                before,
                after,
                examples,
                occurrences: cluster.size,
            });
        }
    }
    candidates.sort_by(|a, b| b.occurrences.cmp(&a.occurrences));
    candidates
}
```

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo test -p oarfish-mask synth::candidates`
Expected: PASS, all three tests.

- [ ] **Step 7: Commit**

```bash
git add crates/oarfish-mask/Cargo.toml crates/oarfish-mask/src/lib.rs crates/oarfish-mask/src/synth
git commit -m "feat(mask): find unmasked positions via a throwaway Drain pass"
```

---

## Task 3: `synth::merge` — insert before the last slot, validate

**Files:**
- Create: `crates/oarfish-mask/src/synth/merge.rs`
- Modify: `crates/oarfish-mask/src/synth/mod.rs`
- Modify: `crates/oarfish-mask/Cargo.toml` (add `proptest` if not already a
  dev-dependency — it already is, per the existing `[dev-dependencies]` block)

**Interfaces:**
- Consumes: `crate::bundle::slots_to_toml` (Task 1, `pub(crate)`), `crate::{Bundle,
  BundleError, SlotDef}`
- Produces: `pub fn merge(starting: &Bundle, synthesized: &[SlotDef]) ->
  Result<Bundle, BundleError>`

- [ ] **Step 1: Write the failing tests**

Create `crates/oarfish-mask/src/synth/merge.rs`:

```rust
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
        let merged = merge(curated(), &[a_slot("JOBID", r"job-[a-z0-9]{4}")])
            .expect("a valid slot merges");
        let names: Vec<&str> = merged.slots().iter().map(|s| s.name.as_str()).collect();
        let jobid_pos = names.iter().position(|n| *n == "JOBID").expect("JOBID present");
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
        assert!(matches!(err, BundleError::CapturingGroup { .. }), "got {err:?}");
    }

    #[test]
    fn a_name_colliding_with_a_curated_slot_is_rejected() {
        let err = merge(curated(), &[a_slot("NUM", r"\d+")])
            .expect_err("a duplicate name must be rejected");
        assert!(matches!(err, BundleError::DuplicateName { .. }), "got {err:?}");
    }
}
```

`crate::bundle` needs to be reachable from `synth::merge` — `mod bundle;` in
`lib.rs` is private by default, but `pub(crate)` items inside it are visible
crate-wide regardless, since privacy in Rust follows the module tree and every
module here is a descendant of the crate root. No change to `lib.rs`'s existing
`mod bundle;` line is needed.

- [ ] **Step 2: Wire the new file into the module**

In `crates/oarfish-mask/src/synth/mod.rs`:

```rust
mod candidates;
mod merge;

pub use candidates::{Candidate, find_candidates};
pub use merge::merge;
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p oarfish-mask synth::merge`
Expected: FAIL to compile until the file above exists — confirm it fails first if
you're following strict TDD by writing the test block before the `merge` function
body; as written above the implementation is included in the same step for this
small a function, so instead run this now and expect PASS (see Step 4).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p oarfish-mask synth::merge`
Expected: PASS, all four tests.

- [ ] **Step 5: Add the proptest for the insertion invariant**

Append to the `#[cfg(test)] mod tests` block in
`crates/oarfish-mask/src/synth/merge.rs`:

```rust
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
```

- [ ] **Step 6: Run the proptest**

Run: `cargo test -p oarfish-mask synth::merge`
Expected: PASS, five tests total in this module.

- [ ] **Step 7: Commit**

```bash
git add crates/oarfish-mask/src/synth
git commit -m "feat(mask): merge synthesized slots before the bundle's last slot"
```

---

## Task 4: `synth::prompt` — building the request text

**Files:**
- Create: `crates/oarfish-mask/src/synth/prompt.rs`
- Modify: `crates/oarfish-mask/src/synth/mod.rs`

**Interfaces:**
- Consumes: `crate::synth::Candidate` (Task 2)
- Produces: `pub fn build_prompt(candidates: &[Candidate]) -> String`,
  `pub fn build_fix_prompt(slot: &SlotDef, error: &BundleError) -> String`

- [ ] **Step 1: Write the failing test**

Create `crates/oarfish-mask/src/synth/prompt.rs`:

```rust
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
        assert!(prompt.contains("capturing group"));
    }
}
```

- [ ] **Step 2: Wire the new file into the module**

In `crates/oarfish-mask/src/synth/mod.rs`:

```rust
mod candidates;
mod merge;
mod prompt;

pub use candidates::{Candidate, find_candidates};
pub use merge::merge;
pub use prompt::{build_fix_prompt, build_prompt};
```

- [ ] **Step 3: Run test to verify it fails then passes**

Run: `cargo test -p oarfish-mask synth::prompt`
Expected: compiles and PASSes once both files above exist — the test and
implementation were written together in Step 1 for this file since it is pure
string formatting with no earlier failing-test/impl split worth the ceremony; if
strict red-green is wanted, comment out the two function bodies first, confirm a
compile failure, then restore them.

- [ ] **Step 4: Commit**

```bash
git add crates/oarfish-mask/src/synth
git commit -m "feat(mask): build the synthesis prompt and the one-slot fix prompt"
```

---

## Task 5: `oarfish-synth` — the chat/completions transport crate

**Files:**
- Create: `crates/oarfish-synth/Cargo.toml`
- Create: `crates/oarfish-synth/src/lib.rs`
- Modify: root `Cargo.toml` (add `oarfish-synth` to `[workspace.dependencies]` and
  `members` is already `["crates/*"]`, so no change needed there)

**Interfaces:**
- Produces: `pub struct Client`, `impl Client { pub fn new(base_url, api_key, model)
  -> Self; pub fn from_env(model: impl Into<String>) -> Result<Self, Error>; pub fn
  model(&self) -> &str; pub async fn synthesize(&self, prompt: &str) -> Result<String,
  Error> }`, `pub enum Error { Transport(reqwest::Error), Api { status:
  reqwest::StatusCode, message: String }, Parse(String), MissingApiKey }`,
  `pub const DEFAULT_BASE_URL: &str`

- [ ] **Step 1: Add the crate to the workspace**

In the root `Cargo.toml`, add to `[workspace.dependencies]` under the `# internal`
section, on its own line right after the `oarfish-jev` entry (the existing list
isn't alphabetical — it's the other internal crates in roughly the order they were
added — so this just keeps the two LLM-transport crates next to each other):

```toml
oarfish-synth  = { path = "crates/oarfish-synth" }
```

- [ ] **Step 2: Create the crate manifest**

Create `crates/oarfish-synth/Cargo.toml`:

```toml
[package]
name = "oarfish-synth"
version.workspace      = true
edition.workspace      = true
rust-version.workspace = true
license.workspace      = true
repository.workspace   = true
authors.workspace      = true

[dependencies]
reqwest.workspace    = true
serde.workspace      = true
serde_json.workspace = true
thiserror.workspace  = true
tracing.workspace    = true

[dev-dependencies]
wiremock.workspace = true
tokio.workspace    = true
```

- [ ] **Step 3: Write the failing test**

Create `crates/oarfish-synth/src/lib.rs` with the doc comment, types, and test
module, leaving `Client::synthesize`'s body as `todo!()` so the test fails at
runtime rather than compile time:

```rust
//! Client for a general chat/completions model, via OpenRouter.
//!
//! `oarfish-jev` is transport only for the *decisions* endpoint — calibrated
//! `choice`/`score`/`noul` answers, and `chat/completions` rejects decisions
//! models outright. Mask synthesis needs the opposite shape: free-form text
//! in, free-form text out. This crate is that transport, holding no policy
//! about what the text says — that lives in `oarfish-mask::synth`.
//!
//! Depends on nothing else in the workspace: it hands back a raw string,
//! nothing more.

#![forbid(unsafe_code)]

/// The default endpoint. Overridden in tests to point at wiremock.
pub const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1/chat/completions";

/// What can go wrong on the one call this crate makes.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The transport failed before a response was read.
    #[error("synthesis request failed: {0}")]
    Transport(#[from] reqwest::Error),
    /// The provider answered with an error status.
    #[error("synthesis request rejected with {status}: {message}")]
    Api {
        status: reqwest::StatusCode,
        message: String,
    },
    /// The payload did not parse as a chat/completions response.
    #[error("synthesis returned an unreadable payload: {0}")]
    Parse(String),
    /// `OPENROUTER_API_KEY` was absent.
    #[error("OPENROUTER_API_KEY is not set")]
    MissingApiKey,
}

#[derive(Debug, serde::Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, serde::Deserialize)]
struct Choice {
    message: Message,
}

#[derive(Debug, serde::Deserialize)]
struct Message {
    content: String,
}

/// The one call, against `chat/completions`.
#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl Client {
    /// Point at any chat/completions-compatible endpoint. Tests pass
    /// wiremock here.
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.into(),
            api_key: api_key.into(),
            model: model.into(),
        }
    }

    /// The production client: default endpoint, key from the environment.
    /// `model` has no default — the operator names one explicitly (spec §3).
    pub fn from_env(model: impl Into<String>) -> Result<Self, Error> {
        let api_key = std::env::var("OPENROUTER_API_KEY").map_err(|_| Error::MissingApiKey)?;
        Ok(Self::new(DEFAULT_BASE_URL, api_key, model))
    }

    /// The model this client asks.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Send one prompt, get back the model's raw text response.
    pub async fn synthesize(&self, prompt: &str) -> Result<String, Error> {
        todo!("Step 5 fills this in")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn chat_body(content: &str) -> serde_json::Value {
        serde_json::json!({
            "choices": [{"message": {"role": "assistant", "content": content}}]
        })
    }

    #[tokio::test]
    async fn synthesize_posts_the_chat_shape_and_reads_the_content_back() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(chat_body("[]")))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::new(server.uri(), "test-key", "some-model");
        let response = client.synthesize("propose some slots").await.expect("synthesize");
        assert_eq!(response, "[]");
    }

    #[tokio::test]
    async fn a_500_is_an_error_and_carries_the_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500).set_body_string("overloaded"))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::new(server.uri(), "test-key", "some-model");
        let err = client.synthesize("propose some slots").await.expect_err("500 must fail");
        match err {
            Error::Api { status, .. } => assert_eq!(status.as_u16(), 500),
            other => panic!("expected an Api error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_unreadable_payload_is_a_parse_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"nope": true})))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::new(server.uri(), "test-key", "some-model");
        let err = client.synthesize("propose some slots").await.expect_err("must fail to parse");
        assert!(matches!(err, Error::Parse(_)), "got {err:?}");
    }
}
```

- [ ] **Step 4: Run test to verify it fails**

Run: `cargo test -p oarfish-synth`
Expected: FAIL (panic) at `todo!("Step 5 fills this in")` for all three tests.

- [ ] **Step 5: Write the minimal implementation**

Replace the `todo!(...)` body of `Client::synthesize` in
`crates/oarfish-synth/src/lib.rs`:

```rust
    pub async fn synthesize(&self, prompt: &str) -> Result<String, Error> {
        let body = serde_json::json!({
            "model": self.model,
            "messages": [{"role": "user", "content": prompt}],
        });

        let response = self
            .http
            .post(&self.base_url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let message = response.text().await.unwrap_or_default();
            let message = message.chars().take(500).collect::<String>();
            return Err(Error::Api { status, message });
        }

        let bytes = response.bytes().await?;
        let parsed: ChatResponse =
            serde_json::from_slice(&bytes).map_err(|e| Error::Parse(e.to_string()))?;
        let content = parsed
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| Error::Parse("no choices in response".to_owned()))?
            .message
            .content;
        Ok(content)
    }
```

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo test -p oarfish-synth`
Expected: PASS, all three tests.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock crates/oarfish-synth
git commit -m "feat(synth): add the oarfish-synth chat/completions transport crate"
```

---

## Task 6: `synth::synthesize` — the `Synthesize` trait, retry, and idempotency check

This is the orchestration task: it ties candidates, prompts, the transport seam, and
merge together, and it's where the spec's stated acceptance test lives.

**Files:**
- Create: `crates/oarfish-mask/src/synth/synthesize.rs`
- Modify: `crates/oarfish-mask/src/synth/mod.rs`
- Modify: `crates/oarfish-mask/src/lib.rs`
- Modify: `crates/oarfish-mask/Cargo.toml` (add `oarfish-synth`, `serde_json`, and a
  `tokio` dev-dependency)

**Interfaces:**
- Consumes: `crate::synth::{Candidate, find_candidates, merge, build_prompt,
  build_fix_prompt}` (Tasks 2–4), `crate::{Bundle, SlotDef}`
- Produces: `pub trait Synthesize: Send + Sync { fn synthesize<'a>(&'a self, prompt:
  &'a str) -> impl Future<Output = Result<String, oarfish_synth::Error>> + Send + 'a;
  }`, `pub struct SynthesisReport { pub bundle: Bundle, pub dropped: Vec<String> }`,
  `pub enum SynthesisError { BadJson(String), Provider(oarfish_synth::Error) }`,
  `pub async fn run<S: Synthesize>(synth: &S, starting: &Bundle, corpus: &[String],
  min_occurrences: u64, max_candidates: usize) -> Result<SynthesisReport,
  SynthesisError>`

- [ ] **Step 1: Add dependencies**

In `crates/oarfish-mask/Cargo.toml`, add to `[dependencies]`:

```toml
oarfish-synth.workspace = true
serde_json.workspace    = true
```

Add to `[dev-dependencies]`:

```toml
tokio = { version = "1.53", features = ["macros", "rt"] }
```

- [ ] **Step 2: Write the failing tests**

Create `crates/oarfish-mask/src/synth/synthesize.rs`:

```rust
//! Orchestrating one synthesis run: find candidates, ask the model, validate
//! and merge each proposed slot, retry a rejection once with the error as
//! feedback, then drop what still fails. Ends with an idempotency check over
//! the full corpus, per §5 of the design.

use crate::synth::{Candidate, build_fix_prompt, build_prompt, find_candidates, merge};
use crate::{Bundle, SlotDef};

/// What talks to the model. Implemented for [`oarfish_synth::Client`] in
/// production; tests implement it with a fake, no network — the same seam
/// discipline as `Decide` everywhere else in this codebase.
pub trait Synthesize: Send + Sync {
    fn synthesize<'a>(
        &'a self,
        prompt: &'a str,
    ) -> impl std::future::Future<Output = Result<String, oarfish_synth::Error>> + Send + 'a;
}

impl Synthesize for oarfish_synth::Client {
    fn synthesize<'a>(
        &'a self,
        prompt: &'a str,
    ) -> impl std::future::Future<Output = Result<String, oarfish_synth::Error>> + Send + 'a {
        oarfish_synth::Client::synthesize(self, prompt)
    }
}

/// What one run produced: the merged bundle, and the name of any slot the
/// model could not fix within one retry.
#[derive(Debug)]
pub struct SynthesisReport {
    pub bundle: Bundle,
    pub dropped: Vec<String>,
}

/// Why a run produced nothing at all — the provider call itself failed, or
/// its response was not JSON. A slot that fails *validation* is not this: it
/// is dropped and reported in [`SynthesisReport::dropped`] instead.
#[derive(Debug, thiserror::Error)]
pub enum SynthesisError {
    #[error("the model's response was not valid JSON: {0}")]
    BadJson(String),
    #[error("the provider call failed: {0}")]
    Provider(#[from] oarfish_synth::Error),
}

/// Run the full synthesis pipeline over one corpus.
pub async fn run<S: Synthesize>(
    synth: &S,
    starting: &Bundle,
    corpus: &[String],
    min_occurrences: u64,
    max_candidates: usize,
) -> Result<SynthesisReport, SynthesisError> {
    let mut candidates: Vec<Candidate> = find_candidates(starting, corpus, min_occurrences);
    candidates.truncate(max_candidates);

    if candidates.is_empty() {
        return Ok(SynthesisReport {
            bundle: starting.clone(),
            dropped: Vec::new(),
        });
    }

    let prompt = build_prompt(&candidates);
    let response = synth.synthesize(&prompt).await?;
    let proposed: Vec<SlotDef> =
        serde_json::from_str(&response).map_err(|e| SynthesisError::BadJson(e.to_string()))?;

    let mut accepted: Vec<SlotDef> = Vec::new();
    let mut dropped: Vec<String> = Vec::new();

    for mut slot in proposed {
        // Traceable at a glance: a synthesized slot's `why` is marked as
        // such, distinct from a curated slot's own reasoning (spec §5).
        slot.why = format!("synthesized: {}", slot.why);
        let mut candidate_list = accepted.clone();
        candidate_list.push(slot.clone());
        match merge(starting, &candidate_list) {
            Ok(_) => accepted = candidate_list,
            Err(error) => {
                let fix_prompt = build_fix_prompt(&slot, &error);
                let fixed = match synth.synthesize(&fix_prompt).await {
                    Ok(text) => text,
                    Err(_) => {
                        dropped.push(slot.name);
                        continue;
                    }
                };
                match serde_json::from_str::<Option<SlotDef>>(&fixed) {
                    Ok(Some(mut fixed_slot)) => {
                        fixed_slot.why = format!("synthesized: {}", fixed_slot.why);
                        let mut retry_list = accepted.clone();
                        retry_list.push(fixed_slot);
                        match merge(starting, &retry_list) {
                            Ok(_) => accepted = retry_list,
                            Err(_) => dropped.push(slot.name),
                        }
                    }
                    Ok(None) | Err(_) => dropped.push(slot.name),
                }
            }
        }
    }

    // `merge` re-validates the accumulated list on every call above, so this
    // final call is redundant work — one more Bundle::parse over a list
    // already proven to validate. It's kept for simplicity: this is a rare,
    // once-at-install path, and re-deriving the last successful `Bundle`
    // from inside the loop instead would save one regex compile at the cost
    // of a more tangled loop body.
    let mut bundle = merge(starting, &accepted).expect("accepted slots already validated above");

    if !accepted.is_empty() {
        for line in corpus {
            let once = bundle.mask(line).template().to_owned();
            let twice = bundle.mask(&once).template().to_owned();
            if once != twice {
                dropped.extend(accepted.iter().map(|s| s.name.clone()));
                bundle = starting.clone();
                break;
            }
        }
    }

    Ok(SynthesisReport { bundle, dropped })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::curated;

    use super::*;

    /// The second adapter: a canned provider with chosen responses in
    /// sequence — no network, no billing.
    struct FakeSynth {
        responses: std::sync::Mutex<Vec<String>>,
        calls: AtomicUsize,
    }

    impl FakeSynth {
        fn new(responses: Vec<&str>) -> Self {
            Self {
                responses: std::sync::Mutex::new(responses.into_iter().map(str::to_owned).collect()),
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl Synthesize for FakeSynth {
        fn synthesize<'a>(
            &'a self,
            _prompt: &'a str,
        ) -> impl std::future::Future<Output = Result<String, oarfish_synth::Error>> + Send + 'a
        {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut guard = self.responses.lock().expect("lock");
            let next = if guard.is_empty() {
                "[]".to_owned()
            } else {
                guard.remove(0)
            };
            async move { Ok(next) }
        }
    }

    fn corpus() -> Vec<String> {
        vec![
            "batch xk92 completed".to_owned(),
            "batch qm14 completed".to_owned(),
            "batch zt77 completed".to_owned(),
        ]
    }

    #[tokio::test]
    async fn a_valid_proposal_merges_and_masks_the_corpus() {
        let synth = FakeSynth::new(vec![
            r#"[{"name": "JOBID", "pattern": "[a-z]{2}\\d{2}", "why": "job codes"}]"#,
        ]);
        let report = run(&synth, curated(), &corpus(), 3, 50).await.expect("run");
        assert!(report.dropped.is_empty(), "got {:?}", report.dropped);
        let slot = report
            .bundle
            .slots()
            .iter()
            .find(|s| s.name == "JOBID")
            .expect("JOBID present");
        assert!(
            slot.why.starts_with("synthesized: "),
            "why should be marked synthesized, got {:?}",
            slot.why
        );
        assert_eq!(
            report.bundle.mask("batch xk92 completed").template(),
            "batch <VAR:JOBID> completed"
        );
    }

    #[tokio::test]
    async fn a_rejected_slot_recovers_on_the_one_retry() {
        let synth = FakeSynth::new(vec![
            r#"[{"name": "JOBID", "pattern": "job-(a-z0-9)", "why": "bad, has a group"}]"#,
            r#"{"name": "JOBID", "pattern": "[a-z]{2}\\d{2}", "why": "fixed"}"#,
        ]);
        let report = run(&synth, curated(), &corpus(), 3, 50).await.expect("run");
        assert!(report.dropped.is_empty(), "got {:?}", report.dropped);
        assert!(report.bundle.slots().iter().any(|s| s.name == "JOBID"));
    }

    #[tokio::test]
    async fn a_twice_rejected_slot_is_dropped_not_blocking() {
        let synth = FakeSynth::new(vec![
            r#"[{"name": "JOBID", "pattern": "job-(a-z0-9)", "why": "bad"}]"#,
            r#"{"name": "JOBID", "pattern": "job-(a-z0-9)", "why": "still bad"}"#,
        ]);
        let report = run(&synth, curated(), &corpus(), 3, 50).await.expect("run");
        assert_eq!(report.dropped, vec!["JOBID".to_owned()]);
        assert_eq!(report.bundle.slots(), curated().slots());
    }

    #[tokio::test]
    async fn no_candidates_returns_the_starting_bundle_unchanged() {
        let synth = FakeSynth::new(vec!["[]".to_owned().leak().to_string()]);
        let corpus = vec!["nothing unusual here".to_owned(); 3];
        let report = run(&synth, curated(), &corpus, 3, 50).await.expect("run");
        assert_eq!(report.dropped, Vec::<String>::new());
        assert_eq!(report.bundle.slots(), curated().slots());
        assert_eq!(synth.calls.load(Ordering::SeqCst), 0, "no candidates, no call");
    }
}
```

- [ ] **Step 3: Wire the new file into the module and crate root**

In `crates/oarfish-mask/src/synth/mod.rs`:

```rust
mod candidates;
mod merge;
mod prompt;
mod synthesize;

pub use candidates::{Candidate, find_candidates};
pub use merge::merge;
pub use prompt::{build_fix_prompt, build_prompt};
pub use synthesize::{Synthesize, SynthesisError, SynthesisReport, run};
```

In `crates/oarfish-mask/src/lib.rs`, replace the earlier `pub use synth::{Candidate,
find_candidates};` line with:

```rust
pub use synth::{
    Candidate, Synthesize, SynthesisError, SynthesisReport, find_candidates, merge, run,
};
```

- [ ] **Step 4: Run test to verify it fails**

Run: `cargo test -p oarfish-mask synth::synthesize`
Expected: FAIL to compile (the `run` function and types referenced by the test
module don't exist as a compiled unit until this file is saved whole — since Step 2
already included the implementation alongside the tests for this orchestration
function, run the tests now and expect the outcome in Step 5 instead).

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p oarfish-mask synth::synthesize`
Expected: PASS, all four tests.

- [ ] **Step 6: Run the full crate test suite**

Run: `cargo test -p oarfish-mask`
Expected: PASS — every test from Tasks 1 through 6, nothing broken.

- [ ] **Step 7: Commit**

```bash
git add crates/oarfish-mask
git commit -m "feat(mask): orchestrate synthesis with per-slot retry and an idempotency check"
```

---

## Task 7: Daemon `--bundle` flag and CLI restructuring

This closes the gap the spec names in §3: `oarfish_mask::curated()` is embedded at
compile time with no way to load a different bundle at runtime. It also restructures
`main.rs`'s flat `Args` into `Cli { command: Option<Command>, daemon: DaemonArgs }` so
`masks synthesize` (Task 8) can be added without breaking the existing daemon
invocation.

**Files:**
- Modify: `crates/oarfish/src/main.rs`

**Interfaces:**
- Produces: `struct DaemonArgs` (all fields from the old `Args`, plus `bundle:
  Option<PathBuf>`), `enum Command { Masks { action: MasksAction } }` (the
  `MasksAction` variant itself is added in Task 8 — this task adds the enum with no
  variants yet would not compile, so this task adds `MasksAction` as an empty-bodied
  placeholder subcommand with no arguments, which Task 8 then fills in)

- [ ] **Step 1: Rename `Args` to `DaemonArgs` and add `--bundle`**

In `crates/oarfish/src/main.rs`, replace the `Args` struct:

```rust
/// Log-driven alarms for homelabs.
#[derive(Debug, Parser)]
struct Args {
    /// Address to listen on for syslog over UDP and TCP.
    #[arg(long, default_value = "0.0.0.0:514")]
    syslog: SocketAddr,

    /// Address to listen on for OTLP logs over gRPC.
    #[arg(long, default_value = "0.0.0.0:4317")]
    otlp: SocketAddr,

    /// Tail the systemd journal. Needs the `journald` build.
    #[arg(long, default_value_t = false)]
    journal: bool,

    /// Address for the JSON API, the SSE stream and the board.
    #[arg(long, default_value = "127.0.0.1:4000")]
    api: SocketAddr,

    /// Where the verdict cache, decision records and open alarms live.
    #[arg(long, default_value = "oarfish-data")]
    data_dir: PathBuf,

    /// Directory holding the built board, served around `/api`. Unset until
    /// the board lands: the API and the stream work without it.
    #[arg(long)]
    static_dir: Option<PathBuf>,
}
```

with:

```rust
/// Log-driven alarms for homelabs.
#[derive(Debug, Parser)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    daemon: DaemonArgs,
}

#[derive(Debug, clap::Subcommand)]
enum Command {
    /// Mask bundle tooling.
    Masks {
        #[command(subcommand)]
        action: MasksAction,
    },
}

#[derive(Debug, clap::Subcommand)]
enum MasksAction {
    /// Placeholder — Task 8 adds the real fields and handler.
    Synthesize,
}

/// The daemon's own flags. Flattened into [`Cli`] so `oarfish --syslog ...`
/// keeps working exactly as before subcommands existed.
#[derive(Debug, Parser)]
struct DaemonArgs {
    /// Address to listen on for syslog over UDP and TCP.
    #[arg(long, default_value = "0.0.0.0:514")]
    syslog: SocketAddr,

    /// Address to listen on for OTLP logs over gRPC.
    #[arg(long, default_value = "0.0.0.0:4317")]
    otlp: SocketAddr,

    /// Tail the systemd journal. Needs the `journald` build.
    #[arg(long, default_value_t = false)]
    journal: bool,

    /// Address for the JSON API, the SSE stream and the board.
    #[arg(long, default_value = "127.0.0.1:4000")]
    api: SocketAddr,

    /// Where the verdict cache, decision records and open alarms live.
    #[arg(long, default_value = "oarfish-data")]
    data_dir: PathBuf,

    /// Directory holding the built board, served around `/api`. Unset until
    /// the board lands: the API and the stream work without it.
    #[arg(long)]
    static_dir: Option<PathBuf>,

    /// Mask bundle to load instead of the embedded curated default. Unset
    /// means exactly the M1–M6 behavior: `oarfish_mask::curated()`.
    #[arg(long)]
    bundle: Option<PathBuf>,
}
```

- [ ] **Step 2: Extract the daemon body into its own function**

Cut everything in `main()` from `let args = Args::parse();`'s replacement point
onward. The new `main()` becomes:

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Non-blocking: the daemon's own writes must never stall the path they
    // report on. The guard is held to the end of main so no line is lost.
    let (writer, _guard) = tracing_appender::non_blocking(std::io::stdout());
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(writer)
        .init();

    match cli.command {
        None => run_daemon(cli.daemon).await,
        Some(Command::Masks { action }) => run_masks(action).await,
    }
}

async fn run_masks(action: MasksAction) -> anyhow::Result<()> {
    match action {
        MasksAction::Synthesize => {
            anyhow::bail!("not yet implemented — Task 8 fills this in")
        }
    }
}

async fn run_daemon(args: DaemonArgs) -> anyhow::Result<()> {
    #[cfg(not(feature = "journald"))]
    if args.journal {
        anyhow::bail!(
            "--journal was given but this binary was built without the `journald` feature; \
             rebuild with `--features journald` instead of silently reading nothing"
        );
    }

    // One bounded channel for all three listeners. One consumer: Drain::train
    // takes &mut self, so the pipeline task owns the table alone.
    let (tx, rx) = mpsc::channel(1024);
    let cancel = CancellationToken::new();
    let tracker = TaskTracker::new();

    let udp = SyslogUdp::bind(args.syslog)
        .await
        .with_context(|| format!("cannot bind syslog UDP on {}", args.syslog))?;
    let tcp = SyslogTcp::bind(args.syslog)
        .await
        .with_context(|| format!("cannot bind syslog TCP on {}", args.syslog))?;
    let otlp = Otlp::bind(args.otlp)
        .await
        .with_context(|| format!("cannot bind OTLP gRPC on {}", args.otlp))?;
    // Bind the API before spawning anything: a taken port is a startup
    // error, never a task that fails silently while the daemon claims to
    // listen.
    let api_listener = tokio::net::TcpListener::bind(args.api)
        .await
        .with_context(|| format!("cannot bind API on {}", args.api))?;

    // The bundle: the embedded curated default, or a file the operator
    // pointed at with --bundle — most often one `masks synthesize` wrote.
    // Byte-for-byte the M1-through-M6 behavior when --bundle is absent.
    let bundle: oarfish_mask::Bundle = match &args.bundle {
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("cannot read bundle at {}", path.display()))?;
            let bundle = oarfish_mask::Bundle::parse(&text)
                .with_context(|| format!("bundle at {} is invalid", path.display()))?;
            tracing::info!(path = %path.display(), hash = %bundle.hash(), "loaded bundle");
            bundle
        }
        None => oarfish_mask::curated().clone(),
    };

    // The verdict cache and the open alarms. The question set is the
    // engine's static policy; the bundle hash is the mask identity the
    // verdicts are keyed under, so an edited bundle re-judges instead of
    // false-hitting. The key is optional, and without one the
    // pipeline still runs end to end — templates stay unjudged, the gate
    // holds, and nothing raises until a key arrives.
    let client = match oarfish_jev::Client::from_env() {
        Ok(client) => client,
        Err(error) => {
            tracing::warn!(
                %error,
                "templates will stay unjudged and nothing will raise; set OPENROUTER_API_KEY"
            );
            oarfish_jev::Client::new(
                "http://127.0.0.1:9/unreachable",
                "unset",
                oarfish_jev::DEFAULT_MODEL,
            )
        }
    };
    // One engine task owns the windows, the state machine and the timers.
    // The pipeline forwards it every classified line; the API reads its
    // snapshot and subscribes to its changes. The contextual check judges
    // through the same client as the verdicts: one key, one billing
    // relationship, and the local-model fallback stays a config change.
    let decide_client = client.clone();
    let verdicts = Arc::new(
        oarfish_store::Verdicts::open(
            &args.data_dir,
            client,
            oarfish_engine::static_questions(),
            oarfish_engine::merge_questions(),
            bundle.hash(),
        )
        .with_context(|| format!("cannot open store at {}", args.data_dir.display()))?,
    );

    // One engine task owns the windows, the state machine and the timers.
    // The pipeline forwards it every classified line; the API reads its
    // snapshot and subscribes to its changes.
    let engine = oarfish_engine::Engine::new(
        Arc::clone(&verdicts),
        decide_client,
        oarfish_engine::EngineConfig::default(),
    );
    let api_state = oarfish_api::ApiState::new(
        engine.snapshot_handle(),
        engine.sender(),
        args.static_dir.clone(),
        cancel.child_token(),
    );
    let (engine_tx, engine_rx) = mpsc::channel(1024);

    let pipeline = Pipeline::new(bundle, oarfish_drain::Drain::new(oarfish_drain::Config::default())?)
        .with_merges(verdicts.merges_handle());
    // Consumers first: the pipeline and the engine are running before any
    // socket starts reading, so startup never sheds into a channel with no
    // reader.
    tracker.spawn(async move {
        let report = pipeline.run_forwarding(rx, engine_tx).await;
        tracing::info!(processed = report.processed, "pipeline drained");
    });
    tracker.spawn(engine.run(engine_rx, cancel.child_token()));
    tracker.spawn(oarfish_api::serve_on_listener(
        api_state,
        api_listener,
        cancel.child_token(),
    ));

    // Producers last: binding stayed early so failures abort startup, but
    // the `run` loops start only once the consumer above is listening.
    tracker.spawn(udp.run(tx.clone(), cancel.child_token()));
    tracker.spawn(tcp.run(tx.clone(), cancel.child_token()));
    tracker.spawn(otlp.run(tx.clone(), cancel.child_token()));

    #[cfg(feature = "journald")]
    let journal_thread = if args.journal {
        // The journal handle is !Send: it is opened and read on one dedicated
        // OS thread, never moved. The thread reports its open result back, so
        // a journal failure is still a clear startup error.
        let tx = tx.clone();
        let cancel = cancel.child_token();
        let (open_tx, open_rx) =
            tokio::sync::oneshot::channel::<Result<(), oarfish_ingest::IngestError>>();
        let thread = std::thread::Builder::new()
            .name("oarfish-journal".to_owned())
            .spawn(move || match oarfish_ingest::JournalReader::open() {
                Ok(reader) => {
                    let _ = open_tx.send(Ok(()));
                    reader.run_blocking(tx, cancel);
                }
                Err(e) => {
                    let _ = open_tx.send(Err(e));
                }
            })
            .context("cannot start journal thread")?;
        open_rx
            .await
            .context("journal thread died while opening the journal")?
            .context("cannot open systemd journal")?;
        Some(thread)
    } else {
        None
    };

    // The daemon's copy is dropped here: once the listeners exit on cancel,
    // no Sender remains and the channel closes for the pipeline to drain.
    drop(tx);

    tracing::info!(
        syslog = %args.syslog,
        otlp = %args.otlp,
        journal = args.journal,
        api = %args.api,
        data_dir = %args.data_dir.display(),
        "oarfish listening"
    );

    shutdown_signal().await;
    tracing::info!("shutting down: listeners stop, the pipeline and the engine drain");
    cancel.cancel();
    tracker.close();
    tracker.wait().await;

    // Shut the judge down and flush. If anything still holds the store the
    // explicit close is skipped and the database persists on drop.
    match Arc::try_unwrap(verdicts) {
        Ok(verdicts) => verdicts.close().await.context("cannot close store")?,
        Err(verdicts) => verdicts
            .persist()
            .context("cannot flush store on shutdown")?,
    }
    #[cfg(feature = "journald")]
    if let Some(thread) = journal_thread {
        thread
            .join()
            .map_err(|_| anyhow::anyhow!("journal thread panicked"))?;
    }

    Ok(())
}
```

Note the two call sites that changed from the original: `oarfish_mask::curated().hash()`
became `bundle.hash()`, and `oarfish_mask::curated().clone()` became the `bundle`
local variable moved directly into `Pipeline::new(bundle, ...)`.

- [ ] **Step 3: Write and run a test proving `--bundle` parses**

Add to the bottom of `crates/oarfish/src/main.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_flag_parses_into_daemon_args() {
        let cli = Cli::try_parse_from(["oarfish", "--bundle", "/etc/oarfish/bundle.toml"])
            .expect("parses");
        assert_eq!(cli.daemon.bundle, Some(PathBuf::from("/etc/oarfish/bundle.toml")));
        assert!(cli.command.is_none());
    }

    #[test]
    fn omitting_bundle_leaves_it_none_and_keeps_the_daemon_default_path() {
        let cli = Cli::try_parse_from(["oarfish"]).expect("parses");
        assert_eq!(cli.daemon.bundle, None);
        assert_eq!(cli.daemon.api, "127.0.0.1:4000".parse().unwrap());
    }
}
```

No new `use` lines are needed: `clap::Parser` is already imported at the top of the
file (for `Cli::parse()`), and `Command`/`MasksAction` derive `clap::Subcommand` via
its fully qualified path, so nothing else needs importing.

Run: `cargo test -p oarfish bundle_flag`
Expected: PASS, both tests.

- [ ] **Step 4: Manually verify the daemon path is unchanged**

Run: `cargo run -p oarfish -- --help`
Expected: shows the daemon's flags (`--syslog`, `--otlp`, `--journal`, `--api`,
`--data-dir`, `--static-dir`, `--bundle`) exactly as before, plus a `masks`
subcommand now listed.

Run: `cargo run -p oarfish -- masks synthesize`
Expected: prints the "not yet implemented" error and exits non-zero — proving the
subcommand routes correctly before Task 8 fills in its behavior.

- [ ] **Step 5: Run the existing acceptance test suite to confirm nothing broke**

Run: `cargo test --workspace`
Expected: PASS — this task changes no behavior on the `None` (daemon) path, only
its entry point and the addition of an always-present `--bundle` flag defaulting to
the same `curated()` bundle as before.

- [ ] **Step 6: Commit**

```bash
git add crates/oarfish/src/main.rs
git commit -m "feat(oarfish): add --bundle and a masks subcommand skeleton"
```

---

## Task 8: `masks synthesize` subcommand — corpus reading and the real handler

**Files:**
- Modify: `crates/oarfish/src/main.rs`
- Modify: `crates/oarfish/Cargo.toml` (add `oarfish-synth`)

**Interfaces:**
- Consumes: `oarfish_mask::{curated, Bundle, synth::run}` (actually re-exported at
  crate root per Task 6 Step 3 — call as `oarfish_mask::run`), `oarfish_synth::Client`

- [ ] **Step 1: Add the dependency**

In `crates/oarfish/Cargo.toml`, add to `[dependencies]`:

```toml
oarfish-synth.workspace = true
```

- [ ] **Step 2: Fill in `MasksAction::Synthesize`'s fields**

In `crates/oarfish/src/main.rs`, replace:

```rust
#[derive(Debug, clap::Subcommand)]
enum MasksAction {
    /// Placeholder — Task 8 adds the real fields and handler.
    Synthesize,
}
```

with:

```rust
#[derive(Debug, clap::Subcommand)]
enum MasksAction {
    /// Find corpus patterns the current bundle doesn't cover, propose regex
    /// slots with an LLM, and write a merged bundle.
    Synthesize {
        /// A log file, or a directory of them, to learn from.
        #[arg(long)]
        corpus: PathBuf,
        /// Bundle to start from. Defaults to the embedded curated bundle.
        #[arg(long)]
        bundle: Option<PathBuf>,
        /// A slot needs at least this many corpus occurrences to be proposed.
        #[arg(long, default_value_t = 3)]
        min_occurrences: u64,
        /// At most this many candidates go to the model in one call.
        #[arg(long, default_value_t = 50)]
        max_candidates: usize,
        /// Where to write the merged bundle.
        #[arg(long)]
        out: PathBuf,
        /// The OpenRouter model id to synthesize with. No default: this is a
        /// rare, deliberate command, and guessing a model would assert a
        /// choice with no basis.
        #[arg(long)]
        model: String,
    },
}
```

- [ ] **Step 3: Write and run a test proving the subcommand's args parse**

Add to the `#[cfg(test)] mod tests` block at the bottom of
`crates/oarfish/src/main.rs` (added in Task 7 Step 3):

```rust
    #[test]
    fn masks_synthesize_parses_with_its_defaults() {
        let cli = Cli::try_parse_from([
            "oarfish",
            "masks",
            "synthesize",
            "--corpus",
            "/var/log",
            "--out",
            "/etc/oarfish/bundle.toml",
            "--model",
            "some-model",
        ])
        .expect("parses");
        match cli.command {
            Some(Command::Masks {
                action:
                    MasksAction::Synthesize {
                        corpus,
                        bundle,
                        min_occurrences,
                        max_candidates,
                        out,
                        model,
                    },
            }) => {
                assert_eq!(corpus, PathBuf::from("/var/log"));
                assert_eq!(bundle, None);
                assert_eq!(min_occurrences, 3);
                assert_eq!(max_candidates, 50);
                assert_eq!(out, PathBuf::from("/etc/oarfish/bundle.toml"));
                assert_eq!(model, "some-model");
            }
            other => panic!("expected Masks::Synthesize, got {other:?}"),
        }
    }

    #[test]
    fn masks_synthesize_requires_model_corpus_and_out() {
        let err = Cli::try_parse_from(["oarfish", "masks", "synthesize"])
            .expect_err("missing required args must fail to parse");
        let message = err.to_string();
        assert!(message.contains("--corpus"), "got {message:?}");
    }
```

Run: `cargo test -p oarfish masks_synthesize`
Expected: PASS, both tests.

- [ ] **Step 4: Write the corpus reader and the real handler**

Replace the `run_masks` function:

```rust
async fn run_masks(action: MasksAction) -> anyhow::Result<()> {
    match action {
        MasksAction::Synthesize {
            corpus,
            bundle,
            min_occurrences,
            max_candidates,
            out,
            model,
        } => run_masks_synthesize(corpus, bundle, min_occurrences, max_candidates, out, model).await,
    }
}

/// Read every non-empty line from `path`: the file itself, or every regular
/// file directly inside it if it's a directory (non-recursive).
fn read_corpus(path: &PathBuf) -> anyhow::Result<Vec<String>> {
    let mut lines = Vec::new();
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    if metadata.is_file() {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        lines.extend(text.lines().filter(|l| !l.is_empty()).map(str::to_owned));
    } else if metadata.is_dir() {
        for entry in std::fs::read_dir(path)
            .with_context(|| format!("cannot read directory {}", path.display()))?
        {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                let text = std::fs::read_to_string(entry.path())
                    .with_context(|| format!("cannot read {}", entry.path().display()))?;
                lines.extend(text.lines().filter(|l| !l.is_empty()).map(str::to_owned));
            }
        }
    } else {
        anyhow::bail!("{} is neither a file nor a directory", path.display());
    }
    Ok(lines)
}

async fn run_masks_synthesize(
    corpus_path: PathBuf,
    bundle_path: Option<PathBuf>,
    min_occurrences: u64,
    max_candidates: usize,
    out: PathBuf,
    model: String,
) -> anyhow::Result<()> {
    let corpus = read_corpus(&corpus_path)?;
    tracing::info!(lines = corpus.len(), path = %corpus_path.display(), "read corpus");

    let starting: oarfish_mask::Bundle = match &bundle_path {
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("cannot read bundle at {}", path.display()))?;
            oarfish_mask::Bundle::parse(&text)
                .with_context(|| format!("bundle at {} is invalid", path.display()))?
        }
        None => oarfish_mask::curated().clone(),
    };

    let client = oarfish_synth::Client::from_env(model)
        .context("cannot build the synthesis client")?;

    let report = oarfish_mask::run(&client, &starting, &corpus, min_occurrences, max_candidates)
        .await
        .map_err(|e| anyhow::anyhow!("synthesis failed: {e}"))?;

    std::fs::write(&out, report.bundle.to_toml())
        .with_context(|| format!("cannot write {}", out.display()))?;

    let added = report.bundle.slots().len() - starting.slots().len();
    tracing::info!(
        out = %out.display(),
        added,
        dropped = report.dropped.len(),
        "wrote bundle"
    );
    if !report.dropped.is_empty() {
        tracing::warn!(slots = ?report.dropped, "dropped after a failed retry");
    }

    Ok(())
}
```

- [ ] **Step 5: Run the build and clippy**

Run: `cargo check -p oarfish && cargo clippy -p oarfish --all-targets -- -D warnings`
Expected: both clean.

- [ ] **Step 6: Manual end-to-end check with a fake corpus**

```bash
mkdir -p /tmp/oarfish-synth-test
cat > /tmp/oarfish-synth-test/app.log <<'EOF'
batch xk92 completed
batch qm14 completed
batch zt77 completed
batch xk92 completed
EOF
```

Run: `OPENROUTER_API_KEY=unused cargo run -p oarfish -- masks synthesize --corpus /tmp/oarfish-synth-test --out /tmp/oarfish-synth-test/out.toml --model some-model`

Expected: the command reaches the network call and fails there (no real
`OPENROUTER_API_KEY`/model), proving every step before the actual HTTP call —
corpus reading, candidate detection finding the `xk92`/`qm14`/`zt77` position,
prompt building — ran without error. This is the expected outcome without a real
API key; Task 6's fake-based tests already prove the full pipeline including a
successful response.

- [ ] **Step 7: Run the full workspace test suite**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/oarfish
git commit -m "feat(oarfish): implement the masks synthesize subcommand"
```

---

## Task 9: Documentation updates

**Files:**
- Modify: `docs/specs/2026-09-17-oarfish-design.md`
- Modify: `docs/specs/2026-09-18-m1-mask-design.md`
- Modify: `crates/oarfish-mask/src/lib.rs`
- Modify: `README.md`

- [ ] **Step 1: Resolve open question #1 in the parent design doc**

In `docs/specs/2026-09-17-oarfish-design.md`, in `## 12. Open questions`, change:

```markdown
1. **Which model synthesizes masks.** DeepParse fine-tuned a local 8B model, which we
   can't ship. A general LLM called once at install is the likely answer, but the
   prompt and its validation need designing.
```

to:

```markdown
1. **Resolved 2026-09-19** (`docs/specs/2026-09-19-mask-synthesis-design.md`): a
   general LLM via a separate `oarfish-synth` transport, called once via `oarfish
   masks synthesize`. Candidates come from a throwaway Drain pass over the masked
   corpus, not raw-line sampling; validation is the existing `Bundle::parse` plus an
   idempotency check, with one model retry per rejected slot before it's dropped.
```

- [ ] **Step 2: Update M1's deferred table**

In `docs/specs/2026-09-18-m1-mask-design.md`, find the row:

```markdown
| `oarfish masks synthesize` | After M6 | §11 already places it off the critical path. The curated bundle covers M1 through M6. |
```

and the row about loading a bundle from a config path, then change the second one:

```markdown
| Loading a bundle from a config path | M4 or later | Nothing configures oarfish yet, and the embedded bundle is what every milestone through M6 runs on. |
```

to:

```markdown
| Loading a bundle from a config path | Delivered in `docs/specs/2026-09-19-mask-synthesis-design.md` | The daemon's `--bundle` flag. |
```

- [ ] **Step 3: Update the crate doc comment**

In `crates/oarfish-mask/src/lib.rs`, after the existing paragraph ending "...and
never writes them.", add:

```rust
//! `synth` is the one exception, and it is cold-path only: `masks synthesize`
//! finds corpus positions the bundle doesn't cover via a throwaway
//! `oarfish-drain` pass, proposes slots for them through `oarfish-synth`, and
//! merges what validates. The daemon's ingest loop never touches this module.
```

Update the final paragraph's dependency list:

```rust
//! In: a `&str` body. Out: a `Masked` holding the raw line, its template, and
//! every `SlotMatch` that filled it. Depends on `regex`, `toml`, `serde`,
//! `blake3`, `thiserror`, `oarfish-drain` and `oarfish-synth` — the latter two
//! only reachable through `synth`, never on the every-line path — and on
//! nothing else in the workspace.
```

- [ ] **Step 4: Update README**

In `README.md`, find:

```markdown
Not on the critical path: `oarfish masks synthesize`. The curated bundle carries M1–M6.
```

and change to:

```markdown
`oarfish masks synthesize` (design: `docs/specs/2026-09-19-mask-synthesis-design.md`)
augments the curated bundle with an LLM call at install time, run manually.
```

- [ ] **Step 5: Final full-workspace check**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: fmt makes no changes (or only whitespace it applies cleanly), clippy is
clean, and every test in the workspace passes.

- [ ] **Step 6: Commit**

```bash
git add docs/specs/2026-09-17-oarfish-design.md docs/specs/2026-09-18-m1-mask-design.md crates/oarfish-mask/src/lib.rs README.md
git commit -m "docs: point the mask-synthesis open questions at the shipped design"
```
