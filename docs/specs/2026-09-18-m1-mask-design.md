# M1 — `oarfish-mask` and the curated default bundle

**Status:** accepted, pre-implementation
**Date:** 2026-09-18
**Parent:** `docs/specs/2026-09-17-oarfish-design.md` — milestone M1 in §11, mechanism in §5.3

---

## 1. What this milestone is for

§11 gives M1 one contract: *a corpus masks deterministically, snapshotted.* That sentence
is doing more work than it looks like it is.

Masking is the highest-leverage decision in the pipeline because everything downstream is
keyed off its output. Template ids derive from masked text. Verdicts are cached against
template ids. So a mask that is merely *good* but not *stable* produces a system that
re-asks questions it has already paid for, and a mask that silently changes produces one
that serves the wrong answer to the wrong line without complaint (§13).

Determinism is therefore not a testing convenience here. It is the property the cost model
rests on, and it is why M1 ships a snapshot harness and three property tests rather than a
handful of unit tests over a regex file.

## 2. Boundaries

**M1 produces** a masked line and the slots that matched it. Nothing else.

**M1 does not produce a `TemplateId`.** Masked text is not yet a template. Drain clusters
masked lines into templates in M2, and `TemplateId::of` is called on a cluster's
representative rather than on every line that passes through. Holding that boundary is also what keeps
the crate's surface this small.

**M1 does not aggregate.** `core::Slot` carries `seen: u64`, a count across a cluster's
history. Building `Slot` values is the clusterer's job; M1 emits per-line matches and
whoever accumulates them owns the counting. A crate on the every-line path should not be
holding state that belongs to a window.

So `oarfish-mask` emits its own `SlotMatch` — a name, a byte span, and the matched text
borrowed from the raw line. That is a different type from `core::Slot` for a good reason:
one is a per-line observation, the other a board-facing aggregate. The consequence is that
nothing in M1 uses `oarfish-core`, and the unused dependency already sitting in the crate's
`Cargo.toml` is removed. It comes back when a consumer needs a shared type, which is how M0
decided every other type's timing too.

**M1 does not touch `Event`.** That type lands with `oarfish-ingest` in M3, shaped by OTLP,
syslog and journald together. Until then the crate's input is a `&str` body, exactly as the
stub's doc comment already says.

## 3. The bundle format

TOML, embedded with `include_str!`, parsed and compiled once behind a `OnceLock`. This adds
a `toml` dependency to `oarfish-mask`. The invariant that matters is that `oarfish-core`
gains nothing, and it does not.

```toml
version = 1

[[slot]]
name    = "IP4"
pattern = '''(?:\d{1,3}\.){3}\d{1,3}'''
why     = "dotted quad, declared before NUM so the octets are not eaten"
```

**Order in the file is precedence.** Order in the file becomes order in the alternation,
and the alternation resolves overlaps by declaration order. That is the single most
important property of this format, and the file opens with a comment block saying so,
because the failure it prevents is invisible: a `NUM` declared too early does not error, it
just quietly eats every IP address in the corpus.

`why` is not decoration. A bundle is a pile of regexes that someone will edit at 3am eight
months from now, and a pattern with no recorded reason is a pattern nobody dares touch.

Validation runs at load and returns a `thiserror` `BundleError`:

- the name matches `[A-Z][A-Z0-9_]*`, so it is both a legal capture-group identifier and a
  legal `<VAR:NAME>` body
- names are unique
- **the pattern introduces no capture group of its own**, checked by compiling it standalone
  and asserting `captures_len() == 1`. Mechanical, and more reliable than parsing regex text
  looking for unescaped parens
- the combined alternation compiles under an explicitly set size limit, so growth from
  source packs and synthesized patterns later fails loudly at a known place rather than at
  an arbitrary one

## 4. Applying the bundle

The whole bundle compiles into **one `Regex` of named groups**:

```
(?<TS>…)|(?<UUID>…)|(?<IP4>…)|…|(?<NUM>…)
```

`captures_iter` walks each line once, yielding both the span and which slot matched, so
replacement needs no second scan and no hand-written overlap resolver.

**This is a deliberate departure from the `RegexSet` row in `CLAUDE.md`, and the reasoning
is recorded here rather than left to be rediscovered.** `RegexSet` answers *which* patterns
match, never *where*: it yields no spans. It can only prefilter, after which each matching
pattern must be re-run to find offsets and the overlapping results merged by hand, because
an IPv4 address also matches the number pattern. A typical log line matches several
patterns, so the shape is one pass plus k more, and the hand-written merge becomes the real
subject of every determinism test.

The alternation gets the intent behind that row — one pass, not N — more literally than
`RegexSet` does, and it moves precedence out of merge code and into the bundle file, where
it can be read. The cost is the capture-group restriction in §3, which validation enforces.

One caveat gets a named test rather than a comment: the `regex` crate's leftmost-first
alternation semantics are what make declaration order into precedence. That is load-bearing,
so it is asserted, not assumed.

## 5. The generic core

Fourteen slots, declared most-specific-first, since that ordering *is* the precedence
mechanism:

`TS`, `UUID`, `MAC`, `IP6`, `IP4`, `URL`, `EMAIL`, `DEV`, `PATH`, `HEX`, `SIZE`, `DUR`,
`PID`, `NUM`.

Two orderings carry most of the risk and each gets a named test:

- `DEV` before `PATH`, so `/dev/sda1` does not disappear into a generic path
- everything numeric before `NUM`, which is the catch-all and is declared last on purpose

`PID` matches the bracketed form (`sshd[12345]`) rather than bare integers. A bare pid is
indistinguishable from any other number, and pretending otherwise would be the same
category of guess as Drain's own "a token containing a digit is a variable" heuristic,
which is the thing this crate exists to replace.

Exact patterns are fixed during implementation against the corpus, not specified here.
A regex written in a design document and never run is a guess with formatting.

## 6. Bundle identity

`Bundle::hash()` returns a blake3 over the canonical bundle text, rendered `b_<hex>` in the
style of `TemplateId`.

This is the `bundle_hash` that the verdict cache keys against in M4, and it is the guard
against the §13 risk: change the bundle, and every template id moves, orphaning every
cached verdict. The dangerous case is not the cache miss. It is the false hit, where a
bundle change makes one template mask down to text that a different template already
produced, the ids collide, and the store serves the wrong verdict.

Nothing in M1 consumes the hash. It is built now because it costs almost nothing here and
is awkward to retrofit once there is a populated store to migrate.

## 7. The corpus

Two tiers, because the honest corpus and the hermetic corpus are not the same corpus.

**Committed tier.** `crates/oarfish-mask/tests/corpus/*.log`: authored, small, one file per
source shape — kernel, sshd, nginx and the rest, not because M1 ships packs for them (§9
defers those) but because a *generic* pattern set has to be proved against the variety it
claims to cover. This is what `cargo test` and CI run. Offline, deterministic, reviewable in a
diff, and every line is present to exercise something specific. `insta`'s glob support walks
the directory, so adding a file never means editing a test.

**Fetched tier.** `corpus/manifest.toml` pins Loghub URLs with sha256 digests;
`scripts/fetch-corpus.sh` downloads into a gitignored `corpus/`. The test that reads it
skips when the directory is absent, so a fresh clone with no network is still green.

The reason for the split is licensing, and it is worth writing down. Loghub's datasets are
offered "freely available for research or academic work", with a request that any
distribution cite the repository. That is a permission scoped to research, not a
redistribution grant, and oarfish is an MIT-licensed product. So no third-party log data
enters the tree, the citation Loghub asks for lives in the manifest next to the URLs, and
M2 still gets the ground-truth templates it needs for the drain3 comparison.

The reusable artifact here is the harness, not the lines. M2 points the same globbing
machinery at the same two directories.

## 8. Testing

`insta` glob snapshots over both tiers, so tuning a pattern shows exactly which lines moved.

`proptest` for three invariants:

- masking is **idempotent**: masking already-masked text changes nothing
- masking is **deterministic**: the same input yields the same output
- the raw line survives **byte-identical**, which is invariant 3 from the parent spec and is
  enforced structurally by having `Masked` borrow the input rather than own a copy

`rstest` tables for per-slot cases and both precedence orderings. Plain tests for each
`BundleError` variant, including a bundle whose pattern smuggles in a capture group.

A `criterion` bench on `mask`. This is literally the every-line path, and §10 asks for one
there. "Fast" without a regression guard is a claim, not a property.

## 9. Deferred, deliberately

| Deferred | Lands in | Why not now |
|---|---|---|
| The nine source packs (kernel, systemd, sshd, nginx, postgres, docker, ZFS, smartd, Proxmox) | A follow-up to M1 | They are data, not code. The format, the harness and the precedence rules are what need proving; nine packs of regexes prove them nine times over. |
| `oarfish masks synthesize` | After M6 | §11 already places it off the critical path. The curated bundle covers M1 through M6. |
| Loading a bundle from a config path | Delivered in `docs/specs/2026-09-19-mask-synthesis-design.md` | The daemon's `--bundle` flag. |
| Anything touching `Event` | M3 | The crate's input is a `&str` body until `oarfish-ingest` exists to produce something better. |

## 10. Changes to existing documents

- `CLAUDE.md`, dependency table: the "Applying the mask bundle" row changes from
  `regex::RegexSet` to the single compiled alternation, with §4's reasoning attached.
- `CLAUDE.md`, commands: add `cargo bench -p oarfish-mask`.
- `docs/specs/2026-09-17-oarfish-design.md` §5.3: the sentence prescribing `RegexSet` is
  corrected, and the resolution noted in §12 alongside the existing entries.
