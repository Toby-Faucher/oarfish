# M0 — `oarfish-core` types, shared with the board

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Define the board-facing domain types in `oarfish-core` and generate the board's
TypeScript from them, so `Alarm` and `Severity` have exactly one definition in the tree.

**Architecture:** `oarfish-core` owns `Severity`, `TemplateId`, `Slot`, `AlarmId` and
`Alarm`. `ts-rs` emits them into `web/src/lib/bindings/oarfish.ts` during `cargo test`;
the board's hand-written copies are deleted and replaced with imports. CI regenerates and
fails on any diff, so the two can never drift apart silently.

**Tech Stack:** Rust 1.98 / edition 2024, `serde`, `ts-rs` 12.0.1, `blake3`, `ulid`,
`time`; `rstest` + `proptest` for tests. Board is Astro 7 + Svelte 5 + TypeScript.

**Spec:** `docs/specs/2026-09-17-oarfish-design.md` — milestone M0 in §11. Types are §6,
severity rules are §8 and `CLAUDE.md`.

## Global Constraints

- `oarfish-core` **depends on nothing else in the workspace**, and gains no new runtime
  dependency in this plan. New *dev*-dependencies are fine (`proptest`).
- `#![forbid(unsafe_code)]` stays at the top of every crate.
- Rust 1.98, edition 2024, pinned in `rust-toolchain.toml`.
- Errors: `thiserror` in libraries, `anyhow` in the binary.
- CI runs `cargo clippy --workspace --all-targets -- -D warnings`. Warnings are failures.
- `cargo fmt --all` before every commit; CI runs `--check`.
- Each crate's `lib.rs` opens with a doc comment saying what it does and what it depends
  on. Keep it accurate when the crate changes.
- Tests live beside the code they test.
- Board: use design tokens from `web/src/styles/global.css`, never a literal colour.
- Board: **never encode severity by colour alone** — bar count, text label and hue.
- Board: JetBrains Mono for templates, ids, timestamps and counts; `tabular-nums`
  wherever digits line up in a column.
- Board copy: active voice, no em-dash overuse.

## Scope

**In:** `Severity`, `TemplateId`, `Slot`, `AlarmId`, `Alarm`; the `ts-rs` export pipeline;
the CI drift gate; rewiring the board onto the generated types.

**Deferred, deliberately.** M0's contract in §11 is *"board and daemon share one
definition of `Alarm`"*, so this plan builds the types that cross that boundary and no
more. The other types named in the `oarfish-core` doc comment land with their first real
consumer, which is where their shape is actually constrained:

| Type | Lands in | Why not now |
|---|---|---|
| `Event` | M3 (`oarfish-ingest`) | Its shape is decided by OTLP, syslog and journald together. Designing it before any of the three exists is guessing. |
| `Verdict`, `Kind`, `Confidence` | M4 (`oarfish-jev` + `oarfish-store`) | Nothing can produce or store one until then, and `Confidence` wants the wiremock tests that prove 0.93 pages and 0.61 does not. |

Task 4 updates the `oarfish-core` doc comment to say so, so the crate's own documentation
does not promise types that are not there.

---

### Task 1: The export pipeline, proved end to end by `Severity`

Severity goes first because it is the smallest real type and it exercises every part of
the pipeline: derive, export, generated file, board import, CI gate.

**Files:**
- Create: `.cargo/config.toml`
- Create: `crates/oarfish-core/src/severity.rs`
- Create: `web/src/lib/bindings/.gitignore` (empty marker so the directory is tracked)
- Modify: `crates/oarfish-core/src/lib.rs`
- Modify: `crates/oarfish-core/Cargo.toml`
- Modify: `.github/workflows/ci.yml`
- Test: `crates/oarfish-core/src/severity.rs` (inline `mod tests`)

**Interfaces:**
- Consumes: nothing.
- Produces: `oarfish_core::Severity` — a C-like enum with variants `Cleared`, `Info`,
  `Minor`, `Major`, `Critical`, deriving `Ord` in that order so `Critical > Major`.
  Serialises lowercase. Exported to TypeScript as
  `export type Severity = "cleared" | "info" | "minor" | "major" | "critical";` in
  `web/src/lib/bindings/oarfish.ts`. Every later task exports into that same file.

- [ ] **Step 1: Add the `proptest` dev-dependency and the export directory**

`crates/oarfish-core/Cargo.toml` — add to `[dev-dependencies]`:

```toml
proptest.workspace = true
```

Create `.cargo/config.toml` at the workspace root. `relative = true` resolves the path
against the directory holding this file, which is the workspace root, so the bindings land
in the board's source tree no matter where `cargo` is invoked from:

```toml
# ts-rs writes the board's TypeScript during `cargo test`. It lands directly in the
# board's source tree: there is one definition of `Alarm` in this repo, and it is the
# Rust one. CI regenerates and fails on a diff, so the two cannot drift.
[env]
TS_RS_EXPORT_DIR = { value = "web/src/lib/bindings", relative = true }
```

Create `web/src/lib/bindings/.gitignore` containing a single line, so the directory is
tracked before anything is generated into it:

```gitignore
# generated by ts-rs; contents are committed
```

- [ ] **Step 2: Write the failing test**

Create `crates/oarfish-core/src/severity.rs`:

```rust
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
```

Add to `crates/oarfish-core/src/lib.rs`, below the existing doc comment and
`#![forbid(unsafe_code)]`:

```rust
mod severity;

pub use severity::Severity;
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p oarfish-core`
Expected: FAIL to compile, `cannot find type 'Severity' in this scope`.

- [ ] **Step 4: Write the implementation**

At the top of `crates/oarfish-core/src/severity.rs`, above the `mod tests` block:

```rust
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
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS,
)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "oarfish.ts")]
pub enum Severity {
    Cleared,
    Info,
    Minor,
    Major,
    Critical,
}
```

- [ ] **Step 5: Run the tests and inspect what was generated**

Run: `cargo test -p oarfish-core`
Expected: PASS, including a generated `export_bindings_severity` test.

Run: `cat web/src/lib/bindings/oarfish.ts`
Expected: a file containing
`export type Severity = "cleared" | "info" | "minor" | "major" | "critical";`

If ts-rs instead wrote `web/src/lib/bindings/Severity.ts`, the `export_to` grouping is not
taking effect: check the attribute is `#[ts(export, export_to = "oarfish.ts")]` on the
type itself and re-run. Every later task depends on all types sharing this one file.

- [ ] **Step 6: Point the board's severity module at the generated type**

Modify `web/src/lib/severity.ts`. Replace the `SEVERITIES` const and the local
`Severity` type definition with a re-export of the generated one, and keep everything
else in the file exactly as it is:

```ts
import type { Severity } from './bindings/oarfish';

export type { Severity };

/**
 * Display order, which is the reverse of the domain's escalation order: the
 * worst thing belongs at the top of a list someone is scanning at 3am.
 * Typed against the generated union, so dropping a variant in Rust breaks the
 * board's build rather than silently rendering nothing.
 */
export const SEVERITIES: readonly Severity[] = [
  'critical',
  'major',
  'minor',
  'info',
  'cleared',
] as const;
```

`Severity.svelte` imports `type Severity` from this module and needs no change.

- [ ] **Step 7: Verify the board still builds**

Run: `cd web && bun run build`
Expected: build succeeds.

- [ ] **Step 8: Add the CI drift gate**

Modify `.github/workflows/ci.yml`. In the `rust` job, replace the
`- run: cargo test --workspace` line with:

```yaml
      - run: cargo test --workspace
      # `cargo test` regenerates the board's bindings. If that produced a diff,
      # someone changed a shared type and did not commit the TypeScript, which is
      # exactly the drift `ts-rs` is here to prevent.
      - name: bindings are up to date
        run: |
          git diff --exit-code -- web/src/lib/bindings \
            || { echo "::error::web/src/lib/bindings is stale. Run 'cargo test --workspace' and commit the result."; exit 1; }
```

- [ ] **Step 9: Format, lint, commit**

The commit comes before the gate is exercised, because exercising it means
reverting a deliberate edit, and `git checkout <path>` only works on a file git
already tracks.

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
git add .cargo/config.toml crates/oarfish-core .github/workflows/ci.yml web/src/lib
git commit -m "feat(core): add Severity, exported to the board via ts-rs"
```

- [ ] **Step 10: Verify the gate catches drift**

A gate nobody has seen fail is a claim, not a property. Break the type on purpose
and confirm the gate notices:

```sh
sed -i 's/    Critical,/    Critical,\n    Fake,/' crates/oarfish-core/src/severity.rs
cargo test -p oarfish-core >/dev/null 2>&1
git diff --exit-code -- web/src/lib/bindings
```
Expected: exit status 1, with `Fake` shown in the diff.

Then revert and confirm it goes clean again:

```sh
git checkout crates/oarfish-core/src/severity.rs
cargo test -p oarfish-core >/dev/null 2>&1
git status --short
```
Expected: no output from `git status --short`. The working tree is clean and the
commit from Step 9 stands unamended.

---

### Task 2: `TemplateId` and `Slot`

**Files:**
- Create: `crates/oarfish-core/src/template.rs`
- Modify: `crates/oarfish-core/src/lib.rs`
- Test: `crates/oarfish-core/src/template.rs` (inline `mod tests`)

**Interfaces:**
- Consumes: nothing from Task 1.
- Produces:
  - `TemplateId` — opaque, `Copy`, wrapping a 32-byte BLAKE3 hash.
    `TemplateId::of(masked: &str) -> TemplateId` is the only constructor.
    `Display` and `Serialize` emit `t_` followed by 64 lowercase hex characters;
    `FromStr` and `Deserialize` parse that back, erroring with `TemplateIdError`.
    TypeScript type is `string`.
  - `TemplateIdError` — `MissingPrefix`, `WrongLength(usize)`, `NotHex`.
  - `Slot { name: String, pattern: String, seen: u64 }`, exported with `seen` as a
    TypeScript `number`.

- [ ] **Step 1: Write the failing tests**

Create `crates/oarfish-core/src/template.rs` with only this block for now:

```rust
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
```

Add to `crates/oarfish-core/src/lib.rs`:

```rust
mod template;

pub use template::{Slot, TemplateId, TemplateIdError};
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p oarfish-core`
Expected: FAIL to compile, `cannot find type 'TemplateId' in this scope`.

- [ ] **Step 3: Write the implementation**

At the top of `crates/oarfish-core/src/template.rs`, above `mod tests`:

```rust
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
/// every id moves, orphaning every verdict. Bundle changes are versioned and
/// trigger a deliberate re-classification rather than a silent cache miss.
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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p oarfish-core`
Expected: PASS, all of them.

- [ ] **Step 5: Check what was generated**

Run: `cat web/src/lib/bindings/oarfish.ts`
Expected: the file now also contains `export type TemplateId = string;` and
`export type Slot = { name: string, pattern: string, seen: number, };`

Confirm `seen` is `number`, not `bigint`. If it is `bigint`, the `#[ts(type = "number")]`
attribute is missing or misplaced.

- [ ] **Step 6: Format, lint, commit**

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
git add crates/oarfish-core web/src/lib/bindings
git commit -m "feat(core): add TemplateId and Slot"
```

---

### Task 3: `AlarmId` and `Alarm`

**Files:**
- Create: `crates/oarfish-core/src/alarm.rs`
- Modify: `crates/oarfish-core/src/lib.rs`
- Test: `crates/oarfish-core/src/alarm.rs` (inline `mod tests`)

**Interfaces:**
- Consumes: `Severity` from Task 1; `TemplateId` from Task 2.
- Produces:
  - `AlarmId` — newtype over `ulid::Ulid`, `Copy`, `Ord` (chronological),
    `AlarmId::generate()`, serialises as a plain ULID string, TypeScript `string`.
  - `Alarm { id: AlarmId, template_id: TemplateId, template: String, severity: Severity,
    host: String, count: u64, opened_at: OffsetDateTime }`. `count` exports as `number`,
    `opened_at` serialises as an RFC 3339 string and exports as `string`.

- [ ] **Step 1: Write the failing tests**

Create `crates/oarfish-core/src/alarm.rs` with only this block for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn an_alarm() -> Alarm {
        Alarm {
            id: AlarmId::generate(),
            template_id: TemplateId::of("EXT4-fs error (device <VAR:DEV>)"),
            template: "EXT4-fs error (device <VAR:DEV>)".to_owned(),
            severity: Severity::Critical,
            host: "nas01".to_owned(),
            count: 14,
            opened_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    /// Alarm ids are ULIDs so the store gets chronological range scans for
    /// free. A UUIDv4 would scatter them across the keyspace.
    #[test]
    fn alarm_ids_sort_chronologically() {
        let first = AlarmId::generate();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let second = AlarmId::generate();

        assert!(first < second);
        assert!(first.to_string() < second.to_string());
    }

    #[test]
    fn alarm_ids_serialize_as_a_plain_string() {
        let id = AlarmId::generate();
        assert_eq!(
            serde_json::to_string(&id).expect("serialize"),
            format!("\"{id}\"")
        );
    }

    #[test]
    fn timestamps_cross_the_wire_as_rfc_3339() {
        let json = serde_json::to_value(an_alarm()).expect("serialize");
        assert_eq!(json["opened_at"], serde_json::json!("1970-01-01T00:00:00Z"));
    }

    #[test]
    fn an_alarm_round_trips() {
        let alarm = an_alarm();
        let json = serde_json::to_string(&alarm).expect("serialize");
        assert_eq!(
            serde_json::from_str::<Alarm>(&json).expect("deserialize"),
            alarm
        );
    }

    /// Oarfish classifies; it does not narrate. The display line is the masked
    /// template, so there is nowhere for a generated summary to live.
    #[test]
    fn the_display_line_is_the_masked_template() {
        assert_eq!(an_alarm().template, "EXT4-fs error (device <VAR:DEV>)");
    }
}
```

Add to `crates/oarfish-core/src/lib.rs`:

```rust
mod alarm;

pub use alarm::{Alarm, AlarmId};
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p oarfish-core`
Expected: FAIL to compile, `cannot find type 'Alarm' in this scope`.

- [ ] **Step 3: Write the implementation**

At the top of `crates/oarfish-core/src/alarm.rs`, above `mod tests`:

```rust
//! An open alarm: the thing the whole daemon exists to avoid raising.

use std::fmt;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use ts_rs::TS;
use ulid::Ulid;

use crate::{Severity, TemplateId};

/// A ULID, so the store gets chronological range scans for free.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS,
)]
#[serde(transparent)]
#[ts(export, export_to = "oarfish.ts", type = "string")]
pub struct AlarmId(Ulid);

impl AlarmId {
    pub fn generate() -> Self {
        Self(Ulid::generate())
    }
}

impl fmt::Display for AlarmId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// One alarm, in the shape the board reads it.
///
/// There is no `title` and no summary field, by design: oarfish classifies, it
/// does not narrate. The display line is the masked template, which is the same
/// artifact `Template.svelte` draws, so nothing here is generated prose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "oarfish.ts")]
pub struct Alarm {
    pub id: AlarmId,
    pub template_id: TemplateId,
    /// The masked template, verbatim. The alarm's display line.
    pub template: String,
    pub severity: Severity,
    pub host: String,
    /// Occurrences folded into this alarm. Exported as a TypeScript `number`
    /// rather than ts-rs's default `bigint`, because it crosses the wire as a
    /// JSON number.
    #[ts(type = "number")]
    pub count: u64,
    /// `time` has no ts-rs integration, so the TypeScript type is declared by
    /// hand and the wire format is pinned to RFC 3339 rather than `time`'s
    /// default component encoding.
    #[serde(with = "time::serde::rfc3339")]
    #[ts(type = "string")]
    pub opened_at: OffsetDateTime,
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p oarfish-core`
Expected: PASS, all of them.

- [ ] **Step 5: Check what was generated**

Run: `cat web/src/lib/bindings/oarfish.ts`
Expected: the file now also contains `export type AlarmId = string;` and an `Alarm` type
whose fields are `id: AlarmId`, `template_id: TemplateId`, `template: string`,
`severity: Severity`, `host: string`, `count: number`, `opened_at: string`.

- [ ] **Step 6: Bring the crate doc comment back in line with reality**

Modify the doc comment at the top of `crates/oarfish-core/src/lib.rs`. Replace the
sentence beginning `Owns \`Event\`` with:

```rust
//! Owns `TemplateId`, `Severity`, `Slot`, `AlarmId` and `Alarm`, and exports
//! them to the board with `ts-rs` so the two never drift. Nothing here does
//! I/O.
//!
//! `Event` arrives with `oarfish-ingest`, and `Verdict` with `oarfish-jev`:
//! both are shaped by their producers, so they are defined alongside them
//! rather than guessed at here.
```

- [ ] **Step 7: Format, lint, commit**

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
git add crates/oarfish-core web/src/lib/bindings
git commit -m "feat(core): add Alarm and AlarmId"
```

---

### Task 4: Rewire the board onto the generated types

This is the task that actually closes M0: after it, no hand-written copy of a shared type
exists anywhere in `web/`.

**Files:**
- Create: `web/src/lib/time.ts`
- Modify: `web/src/components/AlarmRow.svelte`
- Modify: `web/src/components/AlarmList.svelte:4`
- Modify: `web/src/components/Template.svelte:13-17`
- Modify: `web/src/pages/index.astro`

**Interfaces:**
- Consumes: `Alarm`, `Slot`, `Severity` from `web/src/lib/bindings/oarfish`.
- Produces: `clockOf(iso: string): string` in `web/src/lib/time.ts`.

- [ ] **Step 1: Add the timestamp formatter**

Create `web/src/lib/time.ts`:

```ts
/**
 * `opened_at` crosses the wire as RFC 3339. The board shows a bare clock time,
 * because the date is almost always today and the column is scanned, not read.
 *
 * Fixed to UTC on purpose. The list is a server-rendered island, so formatting
 * in the server's zone and then rehydrating in the viewer's would flash a
 * different time on load. Showing local time correctly needs the island to
 * re-format after hydration, which lands with the SSE wiring in M5.
 */
const CLOCK = new Intl.DateTimeFormat('en-GB', {
  hour: '2-digit',
  minute: '2-digit',
  hour12: false,
  timeZone: 'UTC',
});

export function clockOf(iso: string): string {
  return CLOCK.format(new Date(iso));
}
```

- [ ] **Step 2: Rewire `AlarmRow.svelte`**

Modify `web/src/components/AlarmRow.svelte`. Replace the whole `<script>` block with:

```svelte
<script lang="ts">
  import Severity from './Severity.svelte';
  import { ROW_HEIGHT, type Density } from '../lib/severity';
  import type { Alarm } from '../lib/bindings/oarfish';
  import { clockOf } from '../lib/time';

  interface Props {
    alarm: Alarm;
    density?: Density;
    selected?: boolean;
  }

  let { alarm, density = 'compact', selected = false }: Props = $props();
</script>
```

Then, in the markup, make these three replacements:

- `<Severity level={alarm.level} compact={density === 'compact'} />`
  becomes
  `<Severity level={alarm.severity} compact={density === 'compact'} />`

- `<div class="truncate text-[13.5px] font-medium">{alarm.title}</div>`
  becomes

  ```svelte
    <!--
      The display line is the masked template, in mono because it is machine
      text. Flat: the dimensioned drawing is Template.svelte's job, and only
      one component gets to be loud.
    -->
    <div class="truncate font-mono text-[12.5px]">{alarm.template}</div>
  ```

- `{alarm.host} · {alarm.at}` becomes `{alarm.host} · {clockOf(alarm.opened_at)}`

- [ ] **Step 3: Rewire the two components that re-imported those types**

In `web/src/components/AlarmList.svelte`, replace line 4:

```ts
  import AlarmRow, { type Alarm } from './AlarmRow.svelte';
```

with:

```ts
  import AlarmRow from './AlarmRow.svelte';
  import type { Alarm } from '../lib/bindings/oarfish';
```

In `web/src/components/Template.svelte`, delete the local `Slot` interface:

```ts
  export interface Slot {
    name: string;
    pattern: string;
    seen: number;
  }
```

and replace it with an import, placed with the other imports at the top of the script:

```ts
  import type { Slot } from '../lib/bindings/oarfish';
```

- [ ] **Step 4: Update the placeholder data to the real shape**

Modify `web/src/pages/index.astro`. Replace the `import type { Alarm }` line and the
`alarms` array with:

```ts
import type { Alarm } from '../lib/bindings/oarfish';

// Placeholder data until oarfish-api exists. The shape is generated from
// oarfish-core, so this breaks loudly if the daemon's Alarm ever changes.
const alarms: Alarm[] = [
  {
    id: '01K5ZG7Q2M0000000000000001',
    template_id: 't_' + '8f3c21a9'.repeat(8),
    template: 'EXT4-fs error (device <VAR:DEV>): inode #<VAR:NUM>: comm <VAR:PROC>',
    severity: 'critical',
    host: 'nas01',
    count: 14,
    opened_at: '2026-09-18T03:14:07Z',
  },
  {
    id: '01K5ZG7Q2M0000000000000002',
    template_id: 't_' + '4b19c7e2'.repeat(8),
    template: 'smbd[<VAR:NUM>]: session teardown for <VAR:USER> from <VAR:IP>',
    severity: 'major',
    host: 'nas01',
    count: 61,
    opened_at: '2026-09-18T03:14:41Z',
  },
  {
    id: '01K5ZG7Q2M0000000000000003',
    template_id: 't_' + 'c70da155'.repeat(8),
    template: 'certificate for <VAR:HOST> expires in <VAR:NUM> days',
    severity: 'minor',
    host: 'proxy01',
    count: 1,
    opened_at: '2026-09-18T01:02:19Z',
  },
  {
    id: '01K5ZG7Q2M0000000000000004',
    template_id: 't_' + 'a2e64b03'.repeat(8),
    template: 'new template seen on <VAR:HOST>',
    severity: 'info',
    host: 'pve01',
    count: 3,
    opened_at: '2026-09-17T22:40:52Z',
  },
];
```

Leave the `template` and `slots` consts below it unchanged: `slots` already matches the
generated `Slot`.

- [ ] **Step 5: Verify no hand-written copies survive**

Run:
```sh
grep -rn "interface Alarm\|interface Slot\|SEVERITIES = \[" web/src
```
Expected: no matches outside `web/src/lib/severity.ts`, which legitimately keeps
`SEVERITIES` as display order typed against the generated union.

- [ ] **Step 6: Verify the board builds and type-checks**

Run: `cd web && bun run build`
Expected: build succeeds with no TypeScript errors.

- [ ] **Step 7: Look at it**

Run: `cd web && bun run dev`, then open `http://localhost:4321`.

Check by eye, against the rules in `CLAUDE.md`:
- Four alarm rows, severity column showing bars *and* label *and* hue.
- The display line is the masked template, in mono, not competing with the
  dimensioned `Template.svelte` panel beside it.
- Counts right-aligned and tabular.
- Times render as `03:14`, not `Invalid Date`.
- Density toggle still switches compact / comfortable / spacious.
- No coloured bar down the left edge of anything.

Stop the dev server.

- [ ] **Step 8: Full verification and commit**

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
git diff --exit-code -- web/src/lib/bindings && echo "bindings clean"
cd web && bun run build && cd ..
git add web crates
git commit -m "feat(web): read Alarm, Slot and Severity from the generated bindings"
```

All five commands must pass before the commit. M0's contract is that the board and the
daemon share one definition of `Alarm`: the passing `git diff --exit-code` is the evidence.
