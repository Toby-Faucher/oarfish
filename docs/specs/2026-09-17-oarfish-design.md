# oarfish — design

**Status:** accepted, pre-implementation
**Date:** 2026-09-17

---

## 1. The problem

Homelab monitoring fails in a specific way, and it isn't missed outages. It's the four
hundred notifications a week that trained you to swipe them away, so the one that
mattered got swiped too.

Threshold rules can't fix this. The problem isn't where the threshold sits; it's that
nothing in the stack knows what a log line *means*. `Failed password for invalid user`
appearing once is noise. The same line four hundred times in ten minutes is an
incident. A threshold can catch the second case, but only if somebody already knew to
write a rule for it, which requires knowing the line exists.

Oarfish exists so you don't get woken up. Everything below serves that.

## 2. The core idea

**Work is priced by how often it runs.**

Log volume is enormous. *Distinct template* volume is tiny and converges fast: a busy
lab pushes millions of lines a day and settles at a few thousand templates, with maybe
a dozen genuinely new ones daily.

So cluster lines into templates, judge each template **once**, and cache the judgment
against a stable id forever. The hot path never thinks.

| Rate | Work | Cached against |
|---|---|---|
| Once, at install | Mask bundle synthesis | `bundle_hash` |
| Per new template | Static verdict | `TemplateId` |
| Per close pair | Merge review | `(id_a, id_b)` |
| Per burst, flagged only | Contextual check | not cached |
| **Every line** | mask → cluster → window → alarm engine | pure Rust, no network |

At Jev's published $0.042/M input with free output, steady-state spend should land in
fractions of a cent per day, and a cold start over a back catalogue of ~1,300 templates
in the low single-digit dollars. **These are arithmetic on published pricing and an
estimated template count, not measurements.** Both need checking against a real corpus
before anyone quotes them.

## 3. Scope

**In, for v1**

- Ingest: OTLP, syslog (RFC 5424/3164), systemd journal
- Drain clustering with LLM-synthesized masks
- Static verdict + merge review + contextual check via Jev
- Alarm engine: raise, dedupe, flap-suppress, auto-clear
- The board: alarm list, template view, decision records
- ntfy delivery
- Local corrections ("not an alarm") feeding threshold tuning

**Explicitly out, for v1**

- Metrics and traces. Logs only. OTLP is used as a log transport, nothing more.
- Multi-node / clustering. One box.
- Authentication. Assume a trusted LAN; document it loudly.
- Alert routing rules, escalation chains, on-call schedules.
- Consuming alerts from Alertmanager or Uptime Kuma. Possible later; not now.

## 4. Architecture

```
crates/
  oarfish-core     domain types; depends on nothing else in the workspace
  oarfish-mask     applies the regex mask bundle
  oarfish-drain    Drain port — stable template ids
  oarfish-jev      TypeSafe System One client (transport only, no policy)
  oarfish-store    fjall persistence: verdicts, alarms, decision records
  oarfish-ingest   OTLP + syslog + systemd journal
  oarfish-engine   windows, the two Jev passes, alarm state machine
  oarfish-api      JSON API + SSE stream + static hosting
  oarfish          the daemon and its CLI (wiring only)
web/               the board
```

**Invariants.** `oarfish-core` depends on nothing else in the workspace. Nothing on the
every-line path makes a network call. The raw log line is preserved verbatim, always.

## 5. The pipeline

### 5.1 Intake

Three listeners, one internal shape.

- **OTLP** over gRPC and HTTP — the front door for anything with a collector.
- **Syslog** on 514 — switches, PDUs, UPSes. The hardware that will never run an agent
  and is usually what broke.
- **systemd journal** — read directly on the host, so a single-box install needs no
  collector at all.

Journal reading requires the `systemd` crate, which is FFI to libsystemd. That breaks
static builds and won't compile on macOS or musl, so **it sits behind a `journald`
feature flag**, on by default for Linux packages and off for portable builds.

Ingest is rate-limited with `governor`. A runaway container emitting 100k lines/sec is
a normal homelab failure, not an edge case.

### 5.2 Normalize

Everything becomes one `Event`. Source-specific fields survive in `attrs` rather than
being flattened away. The raw line is kept byte-for-byte.

### 5.3 Mask

Variables are replaced with typed placeholders (`<VAR:DEV>`, `<VAR:NUM>`) *before*
clustering, using a bundle that is immutable at runtime.

This is the highest-leverage decision in the pipeline. Drain's own heuristic — "a token
containing a digit is a variable" — is its accuracy bottleneck; DeepParse showed that
moving mask synthesis to a one-time offline LLM pass lifts parsing accuracy sharply and
cuts downstream false alarms by 30%+.

**Bundle sourcing is two-tier:**

1. **A curated default bundle ships with oarfish**, covering the sources a homelab
   actually runs: kernel, systemd, sshd, nginx, postgres, docker, ZFS, smartd, Proxmox.
   This covers most of a typical lab on day one at zero cost and with no LLM call.
2. **Synthesis fills the gaps.** For sources the default bundle doesn't recognise,
   `oarfish masks synthesize` samples representative lines and asks an LLM to produce
   regexes, once, writing the result to the bundle.

The bundle compiles into one `Regex` of named groups, so every pattern is tested in a
single pass rather than by looping N regexes per line. `RegexSet` cannot do this job:
it reports *which* patterns matched, never *where*, so it can only prefilter, and
replacement would still need a second scan per candidate plus a hand-written resolver
for overlaps. Declaration order in the bundle becomes alternation order, which makes
precedence a readable property of the bundle file instead of merge code.

### 5.4 Cluster

Drain assigns a stable `TemplateId`. Ported from Grafana Loki's `pkg/pattern/drain`
rather than drain3, because Loki's version ships the operational pieces we need —
notably a limiter bounding cluster-table growth on hostile input.

**Similarity threshold is 0.90, deliberately high.** High thresholds over-split, which
is annoying. Low thresholds merge `task succeeded` into `task failed`, which silently
deletes the alarm you needed. Over-splits are repaired in 5.6, never by lowering the
threshold.

The port is validated against drain3: both run over one corpus, templates are snapshot
with `insta`, and the diff is reviewed.

### 5.5 Static verdict — cold path, once per template

One Jev call per newly seen template. Every question goes in the same request: they
evaluate in parallel, so a sixth question costs almost nothing in latency and a
rounding error in tokens. Ask speculatively.

| Key | Type | Purpose |
|---|---|---|
| `kind` | choice | hardware_fault / software_error / config / security / routine |
| `severity` | score | 0–3 against described levels |
| `actionable` | noul | can a human do anything about it |
| `transient` | noul | self-resolving |
| `security` | noul | security-relevant |
| `contextual` | noul | does this need re-checking on a burst |

State carries the template, a few example lines, and the host's declared role. The
answer is written to fjall keyed by `TemplateId` and **never asked again**.

### 5.6 Merge review — cold path, per close pair

The conservative threshold leaves near-duplicate templates: one real event split in two
because an optional field changed the token count. No threshold fixes that, because
Drain never compares the two.

Any new template structurally close to an existing one gets exactly one `noul`: *do
these describe the same event type?* Cached by pair. Runs perhaps a dozen times a day.

### 5.7 Window — hot path

A five-minute sliding window per template, in memory. Count, rate, distinct hosts,
first-seen, and delta against the trailing hour. Pure Rust, no network.

### 5.8 Contextual check — cold path, flagged templates only

Only templates the static verdict marked `contextual` reach here. Most never do: a
template judged routine and not actionable is settled forever and costs nothing again.

Window stats and any open alarms go in as state.

| Key | Type | Purpose |
|---|---|---|
| `matters_now` | noul | is this burst worth surfacing at all |
| `correlates_with` | choice | an open alarm id, or `none` |
| `wake_someone` | noul | does this justify a push notification |

`correlates_with` is where incident correlation happens — *is this the same incident as
the alarm already open on this host, or a second one?* `wake_someone` is the value the
alarm engine routes on in 5.9.

**Where the open-alarm state comes from.** The check never asks the alarm engine
anything. The state machine is the only writer of open-alarm state; before a check
runs, the engine captures an immutable snapshot — id, template id, severity, host,
opened-at, one template excerpt — scoped to the burst's host and bounded so it fits the
token budget. That snapshot is the state sent, and it is persisted verbatim in the
decision record, so the correlation is replayable. `correlates_with` is a `choice` over
the ids in that snapshot plus `none`, so the question cannot name an alarm the model
was never shown.

Data flows one way per evaluation: committed state → check → answer → engine mutation.
The engine never blocks on the check, and the check never reads state the current
evaluation is producing, so there is no cycle. Feedback *across* bursts is the point —
that is the system tracking an incident.

The engine re-validates on apply: a Jev call is not instantaneous, `DelayQueue`
auto-clear runs independently, and an alarm that cleared during the round trip is
treated as `none`. Internally the check depends on a read-only `OpenAlarmView` trait
rather than on the state machine, which is also what lets the routing tests hand it
chosen snapshots.

### 5.9 Alarm engine — hot path

A plain Rust state machine, deliberately not a model. Raise, dedupe against open
alarms, suppress flapping, auto-clear after silence. `DelayQueue` from `tokio-util`
runs every timer; there is no task per alarm.

**Confidence picks the lane, and thresholds scale with stakes:**

| `wake_someone` | Action |
|---|---|
| ≥ 0.90 | Page via ntfy |
| 0.60 – 0.90 | Dashboard only |
| < 0.60 | Record, no surface |

Waking someone needs more certainty than drawing a card on a dashboard. These defaults
are starting points to be tuned against real data, not claims.

### 5.10 Out

SSE to the board; POST to ntfy.

### 5.11 Transport

Jev is reached **through OpenRouter**, not the native TypeSafe endpoint: one key and
one billing relationship for any model oarfish ever calls, and it keeps the local-model
fallback in §13 a config change rather than a second client. Base URL
`https://openrouter.ai/api/v1`, model id `typesafe/jev-1.13`, auth via
`OPENROUTER_API_KEY`.

The cost is that OpenRouter fronts Jev with an OpenAI-compatible surface, while the
native endpoint takes `{state, model, questions}` and returns per-answer
`probabilities` and `confidence` directly. **That confidence is load-bearing** — it is
the entire input to the routing table in 5.9 — and it is never synthesized locally; a
made-up confidence is worse than none. So the M4 spike has one job, and it is a gate,
not a formality: send a two-question request through OpenRouter and confirm calibrated
per-answer confidence survives the mapping. If it does not, `oarfish-jev` keeps its
shape and points at the native endpoint instead.

## 6. Data model

Severity follows a subset of **ITU-T X.733** — `critical`, `major`, `minor`, `info`,
`cleared` — because that is the vocabulary a NOC already speaks. X.733's
`indeterminate` is dropped: if oarfish cannot decide, that is a confidence signal, not
a severity.

Alarm ids are **ULIDs**, so the store gets chronological range scans for free. A UUIDv4
would scatter them across the keyspace.

Domain types live in `oarfish-core` and are exported to the board with `ts-rs`, so
`Alarm` and `Severity` are defined once rather than drifting against hand-written
TypeScript. `ts-rs` has no `time` crate support, so timestamp fields carry
`#[ts(type = "string")]`.

## 7. Storage

`fjall`, an embedded LSM engine. Values are `postcard`, not JSON — this is a hot read
path and JSON in an LSM store is waste. `moka` sits in front as the in-memory layer so
verdict lookups don't touch disk per line.

| Keyspace | Key | Value |
|---|---|---|
| `verdicts` | `TemplateId` | static verdict + model + timestamp |
| `merges` | `(id_a, id_b)` | merge decision |
| `alarms` | `Ulid` | alarm record and state |
| `records` | `Ulid` | decision record: exact state, questions, answers |
| `corrections` | `TemplateId` | local operator corrections |

**Decision records are the trust feature.** Every Jev call is persisted with its exact
input, so *"why did this wake me three weeks ago"* has an answer. They are also what
makes the confidence thresholds tunable rather than superstitious.

## 8. The board

Astro 7 shell, Svelte 5 islands, Tailwind 4, bun. Server-rendered so alarm state is
correct on first paint; SSE keeps it live.

The full design system is documented in `CLAUDE.md`. The decisions that matter here:

- **Severity is never colour alone** — bar count, label and hue together. Hues come
  from the Wong palette; a standard red-to-green ramp is the worst possible pair for
  colour blindness.
- **The accent is for interaction only.** A status colour competing with the accent
  destroys scanning.
- **Two things animate**: an alarm arriving, a severity escalating.
- **`Template.svelte` is the signature** — a masked template drawn as a dimensioned
  schematic part. It is the only component allowed to be loud.

## 9. Configuration

`figment` merges, in order: defaults → `/etc/oarfish/config.toml` → environment →
CLI flags.

```toml
[ingest]
otlp   = "0.0.0.0:4317"
syslog = "0.0.0.0:514"
journal = true

[decide]
model     = "typesafe/jev-1.13"   # pinned, not jev-latest
threshold = 0.90            # Drain similarity

[notify]
ntfy = "https://ntfy.sh/…"
```

`OPENROUTER_API_KEY` comes from the environment only, never a config file.

The model is **pinned**, not `jev-latest`. An alerting system's behaviour should not
change because a vendor shipped a new revision overnight.

## 10. Testing

- **`insta`** for everything parser-shaped. Tuning a mask or threshold should show
  exactly which templates moved.
- **`wiremock`** for `oarfish-jev`. Tests never hit the real API: it bills, and the
  engine's routing needs *chosen* confidence values to prove 0.93 pages and 0.61 does not.
- **`proptest`** for invariants: masking is idempotent, template ids are stable.
- **`criterion`** on the every-line path.

## 11. Implementation order

This is more than one implementation plan's worth of work. It decomposes into six
milestones, each independently testable and each leaving the tree green.

| # | Milestone | Done when |
|---|---|---|
| M0 | `oarfish-core` types, exported via `ts-rs` | board and daemon share one definition of `Alarm` |
| M1 | `oarfish-mask` + curated default bundle | a corpus masks deterministically, snapshotted |
| M2 | `oarfish-drain` port | templates match drain3 over the same corpus |
| M3 | `oarfish-ingest`: syslog → journald → OTLP | lines from all three arrive as `Event` |
| M4 | `oarfish-jev` + `oarfish-store` | a template is judged once and cached, proven against wiremock |
| M5 | `oarfish-engine` + `oarfish-api` | an alarm raises, dedupes, clears, and reaches SSE |
| M6 | The board on live data, ntfy delivery | a real log line reaches a real phone |

Order is deliberate: masking before clustering because template ids depend on it, and
the decision layer after storage because a verdict with nowhere to live is untestable.
Mask synthesis (`oarfish masks synthesize`) is not on the critical path — the curated
bundle covers M1 through M6, and synthesis lands after.

## 12. Open questions

1. **Which model synthesizes masks.** DeepParse fine-tuned a local 8B model, which we
   can't ship. A general LLM called once at install is the likely answer, but the
   prompt and its validation need designing.
2. **Light theme.** Specified in tokens, not yet drawn or reviewed. Needs its own pass.

*Resolved 2026-09-18: transport is OpenRouter (§5.11); the contextual check reads a
snapshot, not the engine (§5.8); the mask bundle is one compiled alternation, not a
`RegexSet` (§5.3, and `docs/specs/2026-09-18-m1-mask-design.md` §4).*

## 13. Risks

**The verdict cache is only as good as the template ids.** If the mask bundle changes,
template ids move, and every cached verdict is orphaned. Bundle changes must be
versioned and trigger a deliberate, visible re-classification rather than silent
cache misses.

**Jev is a young, single-vendor dependency in beta.** The decision layer is isolated
behind `oarfish-jev` for exactly this reason: the pipeline runs without it, it just
stops knowing what anything means. A local-model backend of the same shape is a
realistic fallback, and going through OpenRouter (§5.11) makes swapping to one a config
change. The exposure OpenRouter adds is the confidence passthrough: if calibrated
confidence does not survive the OpenAI-compatible mapping, 5.9 has no input to route
on. M4 verifies that before the client is written.

**Confidence thresholds are guesses until there is data.** The numbers in 5.9 are
starting points. Decision records and local corrections exist so they can be tuned
against a real lab instead of defended as principles.
