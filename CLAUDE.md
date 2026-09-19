# oarfish — working notes for agents

Log-driven alarms for homelabs. Rust daemon + Astro board. Pre-alpha: the scaffold
builds, almost nothing is implemented.

Oarfish exists so you **don't** get woken up. Every decision below serves that.

---

## The one idea

Work is priced by how often it runs. Three rates, kept strictly separate:

| Rate | What | Cached against |
|---|---|---|
| Once, at install | LLM synthesizes the regex mask bundle | `bundle_hash` |
| Per new template | Static verdict (kind, severity, actionable) | `TemplateId` |
| Per close pair | Merge review | `(id_a, id_b)` |
| Per burst, flagged only | Contextual check | not cached |
| Every line | mask → cluster → window → alarm engine | pure Rust |

If you are about to make something run more often than it has to, stop.

---

## Backend invariants

1. **`oarfish-core` depends on nothing else in the workspace.** Every other crate
   depends on it; a cycle here is a cycle everywhere.
2. **Nothing on the every-line path makes a network call.** No exceptions. If a
   feature seems to need one, it belongs on a cached path.
3. **The raw log line is preserved verbatim, always.** At 3am the operator wants the
   bytes that arrived, not our interpretation of them.
4. **Drain runs at a conservative similarity threshold (0.90).** It should over-split
   rather than merge `task succeeded` into `task failed` — a merge like that silently
   deletes the alarm. Over-splits are repaired by merge review, not by lowering the
   threshold.
5. **`oarfish-jev` is transport only.** It holds no policy about which questions to
   ask; that lives in `oarfish-engine`.

---

## Design rules

The board is read at 3am by someone half awake. It is dense, dark, and **measured**:
every mark should be evidence something was counted, not decoration.

Tokens live in `web/src/styles/global.css`. Use the token, never a literal colour.

### Severity

**Never encode severity by colour alone.** It carries three redundant signals —
**bar count, text label, and hue** — so it survives colour blindness, a bad monitor,
and greyscale. `src/lib/severity.ts` owns the mapping; `Severity.svelte` renders it.

Hues come from the **Wong palette** (CVD-safe). Standard red→green ramps are the
single worst pair for colour blindness, which is why ours isn't one.

`info` is deliberately **neutral grey**, not Wong's blue, because blue belongs to the
accent — and an info alarm should recede anyway.

### Colour

- **The accent (`--ui-accent`) is for interaction only.** Never for status. A status
  colour that competes with the accent destroys scanning.
- **Ground is never `#000`.** Pure black causes halation and destroys elevation.
  Base is `#0f1319`; each nested surface steps ~4% lighter (`l0` → `l1` → `l2` → `l3`).
- Depth comes from the surface stack, not from shadows.
- Light theme is a **separate design**, not an inversion.

### Type

- **Barlow** (UI), **Barlow Condensed** (labels, headings, severity), **JetBrains
  Mono** (templates, ids, timestamps, counts). Self-hosted via Fontsource — no
  runtime font dependency.
- Barlow is DIN-derived, which gives a monitoring tool engineering heritage.
- **Never Inter.** Its ubiquity is itself a tell that nothing was chosen.
- `tabular-nums` anywhere digits line up in a column.

### Motion budget — two moments, total

| Event | Motion |
|---|---|
| Alarm arrives | 220ms, ease-out, slide + fade |
| Severity escalates | 160ms, ease-out, one bar fills |
| **Everything else** | 120ms colour change, no movement |

Scattered animation is itself an AI tell, and this board is stared at for hours.
Never animate keyboard-initiated actions or anything triggered hundreds of times a
shift. `prefers-reduced-motion` is honoured globally in `global.css`.

### Rows and tables

- **Row separators, not gridlines.** A full grid of rules fights the data.
- Numbers right-aligned and tabular; magnitude is read off the leading edge.
- **Density is a feature**: `compact` (default) / `comfortable` / `spacious`.
  Compact is default because this is an ops tool and its users came for data.
- **No hover-only actions** — unreachable by keyboard and touch.
- Sort columns show direction; filtered views show that they are filtered.

### Things that mark a UI as AI-generated — don't

- **Thin coloured accent bars down the left edge of a card.** This is the most
  specific tell there is. It is also why severity is a real column.
- Blue→purple gradients. Inter. Three equal cards with identical spacing.
- Decorative motion with no job.
- **Em-dash overuse in UI copy.** Watch this one; it is easy to slip into.

### The signature

`Template.svelte` renders a masked template as a **dimensioned schematic part** —
variable slots boxed with leader ticks, dimension lines beneath reporting what each
slot matched and how often.

This is the one component allowed to be loud. **If a second one starts competing
with it, cut something.**

### Copy

Write from the operator's side of the screen. Active voice. A control says exactly
what happens, and keeps the same word through the flow ("Acknowledge" → "Acknowledged").
Errors explain what broke and how to fix it, without apologising. An empty screen is
an invitation, not a shrug.

---

## The board

`web/` is Astro 7 + Svelte 5 islands + Tailwind 4 + **bun** (no package-lock.json,
only bun.lock). It is not a bolt-on; it is the primary interface.

**The rule that keeps it fast:** a component only gets a `client:` directive if it
genuinely needs to run in the browser. Without one it still renders — as static HTML,
shipping no JavaScript. Default to no directive.

Static: layout, chrome, the template panel.
Islands: `AlarmList` (density state, SSE next), `ConnectionPulse`.

Components:

| File | Job |
|---|---|
| `layouts/Board.astro` | shell + icon rail. Static, zero JS |
| `components/Template.svelte` | **the signature** — dimensioned template |
| `components/Severity.svelte` | bars + label + hue |
| `components/AlarmRow.svelte` | one row, density-aware |
| `components/AlarmList.svelte` | island: density state, arrival motion, empty state |
| `components/DensityToggle.svelte` | compact / comfortable / spacious |
| `lib/severity.ts` | severity + density types and maps |

---

## Commands

```sh
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings   # CI runs with -D warnings
cargo fmt --all
cargo test --workspace
cargo insta review          # after snapshot changes
cargo bench -p oarfish-drain
cargo bench -p oarfish-mask

cd web && bun run dev      # board on :4321, proxies /api to the daemon on :4000
cd web && bun run build
```

---

## Testing approach

- **`insta`** for anything parser-shaped: raw lines -> masked -> templates. When you
  tune a mask or a threshold you should see exactly which templates moved. This is
  also how the Drain port gets validated against drain3 — snapshot both over one
  corpus and diff.
- **`wiremock`** for everything touching `oarfish-jev`. Tests never hit the real API:
  it bills, and the engine's routing needs *chosen* confidence values so you can prove
  0.93 pages and 0.61 does not.
- **`proptest`** for invariants — masking is idempotent, template ids are stable.
- **`criterion`** for the every-line path. "Fast" without a regression guard is a
  claim, not a property.

---

## Conventions

- Rust 1.98, edition 2024, pinned in `rust-toolchain.toml`.
- `#![forbid(unsafe_code)]` at the top of every crate. Keep it there.
- Errors: `thiserror` in libraries, `anyhow` in the binary.
- Each crate's `lib.rs` opens with a doc comment saying what it does, how you use it,
  and what it depends on. Keep it accurate when you change the crate.
- Tests live beside the code they test unless they need fixtures.

---

## Key dependencies — reach for the right one

| Need | Use | Not |
|---|---|---|
| Framing syslog over TCP | `tokio-util` `codec` (`LinesCodec`, `Framed`) | hand-rolled byte scanning |
| Alarm auto-clear / flap windows | `tokio-util` `time::DelayQueue` | a task per open alarm |
| Coordinated shutdown | `tokio-util` `rt` (`CancellationToken`, `TaskTracker`) | ad-hoc channels |
| OTLP ingest | `tonic` + `prost` + `opentelemetry-proto` | vendored `.proto` files |
| Ingest backpressure | `governor` | dropping on the floor |
| Hot verdict lookups | `moka` (in front of `fjall`) | hitting fjall per line |
| Values stored in fjall | `postcard` | JSON in an LSM store |
| Alarm ids | `ulid` (`Ulid::generate()`) | `uuid` v4 — it will not sort |
| Board-facing types | `ts-rs`, exported from `oarfish-core` | hand-written TS interfaces |
| Applying the mask bundle | one compiled `Regex` of named groups | `RegexSet`, which gives no spans |
| Config (file + env + CLI) | `figment` | hand-rolled merging |
| The daemon's own log writes | `tracing-appender` (non-blocking) | blocking on the ingest path |
| Board behaviour (menus, dialogs) | `bits-ui` — headless | a component kit with its own look |
| Alarm list at scale | `@tanstack/svelte-virtual` | a data grid; an alarm list is not a table |
| Templates view | `@tanstack/svelte-table` | hand-rolled sorting |
| Sparklines | `uplot` | Chart.js or Recharts, both overkill here |

`RegexSet` looks like the right tool for the mask bundle and is not: it reports *which*
patterns matched, never *where*, so it can only prefilter and replacement still needs a
second scan per candidate plus a hand-written overlap resolver. One alternation of named
groups walks the line once and yields spans, and it moves precedence out of merge code and
into the bundle file, where it can be read.

A runaway container emitting 100k lines/sec is a normal homelab failure, not an
edge case. Ingest is rate-limited on purpose.

---

## Optional features

- `console` — runtime debugging via tokio-console. Needs the cfg flag too:
  ```sh
  RUSTFLAGS="--cfg tokio_unstable" cargo run -p oarfish --features console
  ```

---

## External APIs

**Jev / TypeSafe System One, via OpenRouter** — base URL
`https://openrouter.ai/api/v1`, model `typesafe/jev-1.13` (pinned), bearer auth via
`OPENROUTER_API_KEY`. One key for every model oarfish calls, and it keeps the
local-model fallback a config change.

Natively the body is `{state, model, questions}`; questions are `choice` / `score` /
`noul` and all evaluate in parallel, so ask everything in one request. Request budget
~32k tokens shared between state and questions. Answers carry calibrated
`probabilities` and `confidence`.

**Never synthesize a confidence value.** It is read from the provider payload or it
does not exist — the routing table keys off it, and a made-up number is worse than
none. OpenRouter fronts Jev with an OpenAI-compatible surface, so whether per-answer
confidence survives that mapping is verified in M4 before the client is written; if it
does not, `oarfish-jev` keeps its shape and points at `https://api.typesafe.ai/v1/systemone`
instead. Docs: https://docs.typesafe.ai and https://openrouter.ai/typesafe

---

## Don't

- Don't add a dependency to `oarfish-core`. The one exception is `bytes`:
  `Event.raw` holds the verbatim log bytes (invariant 3), and a `String` field
  would force a lossy conversion at intake for exactly the devices worth
  reading at 3am. Invariant 1 — core depends on no other workspace crate — is
  untouched.
- Don't put an LLM call on the every-line path.
- Don't lower the Drain threshold to "fix" fragmented templates.
- Don't summarize or rewrite log lines. Oarfish classifies; it does not narrate.
- Don't synthesize a confidence value. Read it from the payload or don't have one.
- Don't use the accent colour for status, or a severity colour for anything else.
- Don't put a coloured accent bar down the left edge of anything.
